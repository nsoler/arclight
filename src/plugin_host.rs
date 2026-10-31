//! Plugins out of process: each plugin library runs in helper processes
//! (hosts) of its own, so one that crashes, hangs or corrupts memory gives a
//! node error ("plugin X crashed: killed by SIGSEGV") and is started again
//! on its next use, instead of taking Arclight down.
//!
//! A host is the front end itself started with the library's path (see
//! [`crate::plugin::set_host_command`]); it loads the library as Arclight
//! did in-process (the plugin ABI is the same) and serves renders:
//!
//! - Messages are a little-endian u32 length and that much JSON, requests
//!   on the host's stdin, replies on its stdout. Its first reply describes
//!   the library's ops (or says why it couldn't load it).
//! - Images travel in POSIX shared memory, not the pipe: one segment per
//!   host, holding the inputs and room for the output, grown when a frame
//!   needs more. The plugin writes its output straight into it; one larger
//!   than the room goes in a segment the host makes for it.
//! - A host gets [`crate::plugin::timeout`] for each reply; past it, it's
//!   killed. One that dies (or is killed) is restarted at the next render.
//!
//! A library has up to [`HOSTS`] hosts, so renders from several threads
//! (the viewer, scripts, a Media Out) still run side by side. Its ops are
//! kept on disk once described (by path, size and modification time), so a
//! library that hasn't changed starts no host until it renders, and one
//! whose host can't start still lists its ops.

use crate::image::Image;
use crate::plugin::{self, ArclightImage, ArclightPlugin};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{c_void, CString, OsString};
use std::io::{BufRead, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Set in a host's environment.
pub const HOST_ENV: &str = "ARCLIGHT_PLUGIN_HOST";
/// The most hosts one library runs at once.
pub const HOSTS: usize = 4;
/// Bytes per pixel: four f32s.
const PIXEL: usize = 16;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// A render: `op` with `params` (JSON, defaults filled in) at `frame`; the
/// inputs (None: unconnected) at their offsets in shared memory segment
/// `shm` (`size` bytes); `room` bytes from `out_at` for the output.
#[derive(Serialize, Deserialize)]
struct Request {
    op: String,
    params: String,
    frame: i64,
    shm: String,
    size: usize,
    inputs: Vec<Option<Shape>>,
    out_at: usize,
    room: usize,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Shape {
    width: u32,
    height: u32,
    at: usize,
}

#[derive(Serialize, Deserialize)]
enum Reply {
    /// The library loaded: its ops JSON.
    Hello(String),
    /// Loading or rendering failed (the plugin's message).
    Failed(String),
    /// Rendered: the output's size, in the room asked for or (too big for
    /// it) in a segment of its own, named.
    Done { width: u32, height: u32, own: Option<String> },
}

fn send(w: &mut impl Write, msg: &impl Serialize) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).expect("messages serialize");
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// The next message, None at the end of the stream.
fn receive(r: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    r.read_exact(&mut body).ok()?;
    Some(body)
}

// ---------------------------------------------------------------------------
// Shared memory
// ---------------------------------------------------------------------------

/// A mapped POSIX shared memory segment.
struct Shm {
    name: String,
    ptr: *mut u8,
    len: usize,
}

// The mapping is plain memory, used by one thread at a time.
unsafe impl Send for Shm {}

static SEGMENTS: AtomicU64 = AtomicU64::new(0);

impl Shm {
    /// A new segment of `len` bytes (zeroed), named for this process.
    fn create(len: usize) -> Result<Shm> {
        // Short: macOS allows 31 bytes.
        let name = format!("/arcl{}-{}", std::process::id(), SEGMENTS.fetch_add(1, Ordering::Relaxed));
        let c = CString::new(name.clone())?;
        let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600 as libc::c_uint) };
        if fd < 0 {
            bail!("can't make shared memory: {}", std::io::Error::last_os_error());
        }
        let sized = unsafe { libc::ftruncate(fd, len.max(1) as libc::off_t) } == 0;
        let err = std::io::Error::last_os_error();
        let shm = sized.then(|| Self::map(name.clone(), fd, len));
        unsafe { libc::close(fd) };
        match shm {
            Some(Ok(shm)) => Ok(shm),
            other => {
                unsafe { libc::shm_unlink(c.as_ptr()) };
                match other {
                    Some(Err(e)) => Err(e),
                    _ => bail!("can't size shared memory to {len} bytes: {err}"),
                }
            }
        }
    }

    /// Segment `name` (made elsewhere), `len` bytes of it.
    fn open(name: &str, len: usize) -> Result<Shm> {
        let c = CString::new(name)?;
        let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_RDWR, 0 as libc::c_uint) };
        if fd < 0 {
            bail!("can't open shared memory {name}: {}", std::io::Error::last_os_error());
        }
        let shm = Self::map(name.to_string(), fd, len);
        unsafe { libc::close(fd) };
        shm
    }

    fn map(name: String, fd: libc::c_int, len: usize) -> Result<Shm> {
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len.max(1), libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) };
        if ptr == libc::MAP_FAILED {
            bail!("can't map shared memory: {}", std::io::Error::last_os_error());
        }
        Ok(Shm { name, ptr: ptr as *mut u8, len })
    }

    /// Remove its name (the memory stays while mapped).
    fn unlink(&self) {
        if let Ok(c) = CString::new(self.name.as_str()) {
            unsafe { libc::shm_unlink(c.as_ptr()) };
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut c_void, self.len.max(1)) };
    }
}

/// `dst` = `src`, in parallel (a 4K frame is 130 MB).
fn copy(dst: &mut [u8], src: &[u8]) {
    use rayon::prelude::*;
    const CHUNK: usize = 4 << 20;
    dst.par_chunks_mut(CHUNK).zip(src.par_chunks(CHUNK)).for_each(|(d, s)| d.copy_from_slice(s));
}

/// An image's pixels as bytes.
fn pixel_bytes(img: &Image) -> &[u8] {
    unsafe { std::slice::from_raw_parts(img.pixels.as_ptr() as *const u8, img.pixels.len() * PIXEL) }
}

// ---------------------------------------------------------------------------
// The parent's side
// ---------------------------------------------------------------------------

/// Why a render through a host failed.
enum Failure {
    /// The plugin said so; the host is fine.
    Plugin(anyhow::Error),
    /// The host died, hung or broke the protocol: start another.
    Host(anyhow::Error),
}

/// A running host.
struct Host {
    library: String,
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Option<Vec<u8>>>,
    /// The end of what it wrote to stderr (a crash report's path, a panic).
    stderr: Arc<Mutex<String>>,
    shm: Option<Shm>,
    /// The segment's name still exists (until the host has opened it).
    linked: bool,
}

impl Host {
    /// Start one for `library`: its ops JSON.
    fn start(command: &[OsString], library: &Path) -> Result<(Host, String)> {
        let name = library.file_name().map_or_else(|| library.display().to_string(), |n| n.to_string_lossy().into_owned());
        let (program, args) = command.split_first().ok_or_else(|| anyhow!("no plugin host command"))?;
        let mut child = Command::new(program)
            .args(args)
            .arg(library)
            // Marked, so it loads no plugins of its own (see plugin::init).
            .env(HOST_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("can't start a plugin host ({})", Path::new(program).display()))?;
        let (stdin, mut stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let (tx, replies) = mpsc::channel();
        std::thread::Builder::new().name(format!("plugin host {name}")).spawn(move || loop {
            let msg = receive(&mut stdout);
            let end = msg.is_none();
            if tx.send(msg).is_err() || end {
                break;
            }
        })?;
        // Passed on to Arclight's stderr, the end kept for errors.
        let tail = Arc::new(Mutex::new(String::new()));
        let keep = tail.clone();
        std::thread::Builder::new().name(format!("plugin host {name} stderr")).spawn(move || {
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("{line}");
                let mut t = keep.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 2000;
                    let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
                    t.drain(..cut);
                }
            }
        })?;
        let mut host = Host { library: name, child, stdin, replies, stderr: tail, shm: None, linked: false };
        let ops = match host.reply() {
            Ok(Reply::Hello(ops)) => ops,
            Ok(Reply::Failed(e)) => bail!("{e}"),
            Ok(_) => bail!("plugin host {} answered out of turn", host.library),
            Err(Failure::Host(e) | Failure::Plugin(e)) => return Err(e),
        };
        Ok((host, ops))
    }

    /// The next reply, within the timeout.
    fn reply(&mut self) -> Result<Reply, Failure> {
        let timeout = plugin::timeout();
        match self.replies.recv_timeout(timeout) {
            Ok(Some(body)) => serde_json::from_slice(&body).map_err(|e| Failure::Host(anyhow!("plugin host {} sent garbage: {e}", self.library))),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(Failure::Host(anyhow!("plugin {} timed out after {}, and was stopped", self.library, secs(timeout))))
            }
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                let status = self.child.wait().ok();
                // Its last words (a crash report's path) come in just after.
                std::thread::sleep(Duration::from_millis(50));
                Err(Failure::Host(anyhow!("plugin {} crashed: {}{}", self.library, status.map_or("it stopped".into(), |s| how(&s)), self.last_words())))
            }
        }
    }

    /// Where its crash report went (its name holds the host's pid, see
    /// crate::crash), else its last line on stderr.
    fn last_words(&self) -> String {
        let pid = format!("-{}-", self.child.id());
        let report = std::fs::read_dir(crate::crash::dir()).into_iter().flatten().flatten().map(|e| e.path()).find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("plugin-host-") && n.contains(&pid))
        });
        if let Some(path) = report {
            return format!(" (report: {})", path.display());
        }
        let t = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        let last = t.lines().rev().find(|l| !l.trim().is_empty());
        last.map_or(String::new(), |l| format!(" ({})", l.trim().trim_start_matches("arclight: ")))
    }

    fn render(&mut self, op: &str, params: &str, inputs: &[Option<&Image>], frame: i64) -> Result<Image, Failure> {
        // ABI v1 has no data windows: plugins see whole frames.
        let frames: Vec<Option<Cow<Image>>> = inputs.iter().map(|i| i.map(Image::flat)).collect();
        let mut shapes = Vec::with_capacity(frames.len());
        let mut at = 0;
        for f in &frames {
            shapes.push(f.as_ref().map(|img| {
                let s = Shape { width: img.width as u32, height: img.height as u32, at };
                at += img.pixels.len() * PIXEL;
                s
            }));
        }
        // Room for an output as big as the biggest input (most are).
        let room = frames.iter().flatten().map(|f| f.pixels.len() * PIXEL).max().unwrap_or(0);
        let (out_at, size) = (at, at + room);
        if self.shm.as_ref().is_none_or(|s| s.len < size) {
            let shm = Shm::create(size.max(1 << 20)).map_err(Failure::Host)?;
            (self.shm, self.linked) = (Some(shm), true);
        }
        let shm = self.shm.as_mut().unwrap();
        let bytes = shm.bytes();
        for (f, s) in frames.iter().zip(&shapes) {
            if let (Some(img), Some(s)) = (f, s) {
                let src = pixel_bytes(img);
                copy(&mut bytes[s.at..s.at + src.len()], src);
            }
        }
        let req = Request { op: op.into(), params: params.into(), frame, shm: shm.name.clone(), size: shm.len, inputs: shapes, out_at, room };
        let sent = send(&mut self.stdin, &req);
        let reply = match sent {
            Ok(()) => self.reply(),
            Err(_) => self.reply().and(Err(Failure::Host(anyhow!("plugin host {} stopped reading", self.library)))),
        };
        // It has the segment open now (or never will): its name can go.
        if std::mem::take(&mut self.linked) {
            self.shm.as_ref().unwrap().unlink();
        }
        match reply? {
            Reply::Done { width, height, own } => {
                let (w, h) = (width as usize, height as usize);
                let n = w * h * PIXEL;
                let mut big = None;
                let src = match own {
                    None if n <= room => &self.shm.as_mut().unwrap().bytes()[out_at..out_at + n],
                    None => return Err(Failure::Host(anyhow!("plugin host {} wrote past its room", self.library))),
                    Some(name) => {
                        let shm = big.insert(Shm::open(&name, n).map_err(Failure::Host)?);
                        shm.unlink();
                        &shm.bytes()[..n]
                    }
                };
                // Filled straight from shared memory (not zeroed first).
                let mut pixels: Vec<[f32; 4]> = Vec::with_capacity(w * h);
                let dst = unsafe { std::slice::from_raw_parts_mut(pixels.as_mut_ptr() as *mut u8, n) };
                copy(dst, src);
                // SAFETY: every byte was just written.
                unsafe { pixels.set_len(w * h) };
                let mut img = Image::new(0, 0);
                (img.format, img.width, img.height, img.pixels) = (crate::image::Size { width: w, height: h }, w, h, pixels);
                Ok(img)
            }
            Reply::Failed(e) => Err(Failure::Plugin(anyhow!("{e}"))),
            Reply::Hello(_) => Err(Failure::Host(anyhow!("plugin host {} answered out of turn", self.library))),
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How a host ended.
fn how(status: &std::process::ExitStatus) -> String {
    if let Some(sig) = status.signal() {
        let name = match sig {
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGILL => "SIGILL",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGTERM => "SIGTERM",
            _ => return format!("killed by signal {sig}"),
        };
        return format!("killed by {name}");
    }
    match status.code() {
        Some(code) => format!("it exited with status {code}"),
        None => "it stopped".into(),
    }
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s.fract() == 0.0 { format!("{s:.0} s") } else { format!("{s:.1} s") }
}

/// A library's hosts.
pub struct Pool {
    library: &'static Path,
    command: Vec<OsString>,
    slots: Vec<Mutex<Option<Host>>>,
    turn: AtomicU64,
}

impl Pool {
    pub fn new(library: &'static Path, command: Vec<OsString>) -> Pool {
        Pool { library, command, slots: (0..HOSTS).map(|_| Mutex::new(None)).collect(), turn: AtomicU64::new(0) }
    }

    /// Its ops JSON: from the last time if the library hasn't changed, else
    /// from a host started for it (kept for rendering). With a host that
    /// can't start, the ops from the last time and why (else the error).
    pub fn describe(&self) -> Result<(String, Option<String>)> {
        let stamp = stamp(self.library);
        let cached = cached_ops(self.library);
        if let (Some(stamp), Some((was, ops))) = (&stamp, &cached) {
            if stamp == was {
                return Ok((ops.clone(), None));
            }
        }
        match Host::start(&self.command, self.library) {
            Ok((host, ops)) => {
                *self.slots[0].lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
                if let Some(stamp) = stamp {
                    remember_ops(self.library, stamp, &ops);
                }
                Ok((ops, None))
            }
            Err(e) => match cached {
                Some((_, ops)) => Ok((ops, Some(format!("{e:#}")))),
                None => Err(e),
            },
        }
    }

    /// Render `op` in one of its hosts (a free one, else waiting a turn),
    /// started as needed.
    pub fn render(&self, op: &str, params: &str, inputs: &[Option<&Image>], frame: i64) -> Result<Image> {
        let mut slot = self.slots.iter().find_map(|s| s.try_lock().ok());
        if slot.is_none() {
            let k = self.turn.fetch_add(1, Ordering::Relaxed) as usize % self.slots.len();
            slot = Some(self.slots[k].lock().unwrap_or_else(|e| e.into_inner()));
        }
        let mut slot = slot.unwrap();
        if slot.is_none() {
            let (host, _) = Host::start(&self.command, self.library).map_err(|e| anyhow!("plugin {} can't start: {e:#}", self.name()))?;
            *slot = Some(host);
        }
        match slot.as_mut().unwrap().render(op, params, inputs, frame) {
            Ok(img) => Ok(img),
            Err(Failure::Plugin(e)) => Err(e),
            Err(Failure::Host(e)) => {
                // Started again at the next render.
                *slot = None;
                Err(e)
            }
        }
    }

    fn name(&self) -> String {
        self.library.file_name().map_or_else(|| self.library.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

// ---------------------------------------------------------------------------
// Ops remembered between runs
// ---------------------------------------------------------------------------

/// What identifies a library's build: its size and modification time.
fn stamp(library: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(library).ok()?;
    Some((m.len(), m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos()))
}

#[derive(Serialize, Deserialize, Default)]
struct Remembered {
    libraries: HashMap<PathBuf, (u64, u128, String)>,
}

fn remembered_path() -> PathBuf {
    crate::config::state_dir().join("plugin-ops.json")
}

static REMEMBERED: Mutex<()> = Mutex::new(());

fn cached_ops(library: &Path) -> Option<((u64, u128), String)> {
    let _lock = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    let r: Remembered = serde_json::from_str(&std::fs::read_to_string(remembered_path()).ok()?).ok()?;
    let (len, time, ops) = r.libraries.get(&std::path::absolute(library).ok()?)?.clone();
    Some(((len, time), ops))
}

fn remember_ops(library: &Path, (len, time): (u64, u128), ops: &str) {
    let _lock = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    let path = remembered_path();
    let mut r: Remembered = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let Ok(library) = std::path::absolute(library) else { return };
    r.libraries.insert(library, (len, time, ops.to_string()));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written whole then moved over, so a reader never sees half.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, serde_json::to_string(&r).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ---------------------------------------------------------------------------
// The host's side
// ---------------------------------------------------------------------------

/// Where a render's output goes.
struct Out {
    /// The room in the request's segment.
    room: *mut u8,
    room_len: usize,
    /// One too big for it: a segment of its own.
    own: Option<Shm>,
    size: Option<(u32, u32)>,
    failed: Option<String>,
}

unsafe extern "C" fn host_alloc(host: *mut c_void, width: u32, height: u32) -> *mut f32 {
    let out = unsafe { &mut *(host as *mut Out) };
    let n = width as usize * height as usize * PIXEL;
    out.size = Some((width, height));
    if n <= out.room_len {
        // Zero-filled, as the ABI promises.
        use rayon::prelude::*;
        unsafe { std::slice::from_raw_parts_mut(out.room, n) }.par_chunks_mut(4 << 20).for_each(|c| c.fill(0));
        return out.room as *mut f32;
    }
    match Shm::create(n) {
        Ok(shm) => out.own.insert(shm).ptr as *mut f32,
        Err(e) => {
            out.failed = Some(format!("{e:#}"));
            std::ptr::null_mut()
        }
    }
}

/// The host: load `library`, say what's in it, then render as asked until
/// stdin closes. Returns the exit code.
pub fn serve(library: &Path) -> i32 {
    // The replies get stdout to themselves: what the plugin prints goes to
    // stderr instead.
    let replies_fd = unsafe { libc::dup(1) };
    if replies_fd < 0 || unsafe { libc::dup2(2, 1) } < 0 {
        eprintln!("arclight plugin host: can't set up its output");
        return 2;
    }
    let mut replies: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(replies_fd) };
    let mut requests = std::io::stdin().lock();
    let entry: &ArclightPlugin = match plugin::open_library(library) {
        Ok((entry, ops)) => {
            if send(&mut replies, &Reply::Hello(ops.to_string())).is_err() {
                return 1;
            }
            entry
        }
        Err(e) => {
            let _ = send(&mut replies, &Reply::Failed(format!("{e:#}")));
            return 1;
        }
    };
    let mut shm: Option<Shm> = None;
    while let Some(body) = receive(&mut requests) {
        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(req) => render(entry, &req, &mut shm),
            Err(e) => Reply::Failed(format!("bad request: {e}")),
        };
        if send(&mut replies, &reply).is_err() {
            break;
        }
    }
    0
}

/// One render in the host.
fn render(entry: &ArclightPlugin, req: &Request, shm: &mut Option<Shm>) -> Reply {
    if shm.as_ref().is_none_or(|s| s.name != req.shm) {
        *shm = None;
        match Shm::open(&req.shm, req.size) {
            Ok(s) => *shm = Some(s),
            Err(e) => return Reply::Failed(format!("{e:#}")),
        }
    }
    let seg = shm.as_mut().unwrap();
    let fits = |at: usize, n: usize| at.checked_add(n).is_some_and(|end| end <= seg.len);
    let mut images = Vec::with_capacity(req.inputs.len());
    for s in &req.inputs {
        images.push(match s {
            Some(s) if fits(s.at, s.width as usize * s.height as usize * PIXEL) => {
                ArclightImage { width: s.width, height: s.height, pixels: unsafe { seg.ptr.add(s.at) } as *const f32 }
            }
            Some(_) => return Reply::Failed("an input lies outside shared memory".into()),
            None => ArclightImage { width: 0, height: 0, pixels: std::ptr::null() },
        });
    }
    if !fits(req.out_at, req.room) {
        return Reply::Failed("the output's room lies outside shared memory".into());
    }
    let mut out = Out { room: unsafe { seg.ptr.add(req.out_at) }, room_len: req.room, own: None, size: None, failed: None };
    // SAFETY: the inputs and the room are inside the mapped segment, and
    // `out` outlives the call.
    let called = unsafe { plugin::call(entry, &req.op, &req.params, &images, req.frame, &mut out as *mut Out as *mut c_void, host_alloc) };
    if let Some(e) = out.failed {
        return Reply::Failed(e);
    }
    match (called, out.size) {
        (Err(e), _) => Reply::Failed(format!("{e:#}")),
        (Ok(()), None) => Reply::Failed("plugin returned no image".into()),
        (Ok(()), Some((width, height))) => Reply::Done { width, height, own: out.own.map(|s| s.name.clone()) },
    }
}
