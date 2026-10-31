//! Crash reports, kept on this machine (nothing is ever sent): a panic
//! hook and, on Unix, a handler for native crashes (a segfault in a
//! decoder or a plugin) write a report with what Arclight was doing to
//! [`dir`]; a crash of the main thread also leaves a marker for the next
//! start to ask about it (the GUI's "Arclight closed unexpectedly"), and
//! saves the open project to its autosave first if it can ([`set_rescue`]).
//! A panic on another thread is caught where it happens (it becomes an
//! error) and its report marked recovered.
//!
//! Reports name no files: every path in them reads `<path>` when shown or
//! copied ([`scrub`]), unless the user chooses to include them.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Where a prefilled GitHub issue is opened (the user reviews and submits
/// it).
pub const ISSUES_URL: &str = "https://github.com/nsoler/arclight/issues/new";
/// Reports kept (the oldest go first).
const KEEP: usize = 20;
/// Actions remembered for a report.
const NOTES: usize = 30;

/// Where reports are written: ~/Library/Logs/Arclight on macOS, else
/// `crashes` in the state dir (~/.local/state/arclight/crashes).
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ARCLIGHT_CRASH_DIR") {
        return PathBuf::from(d);
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Logs/Arclight");
        }
    }
    crate::config::state_dir().join("crashes")
}

/// What a report is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The main thread panicked: ArclightVFX closed.
    Crash,
    /// Another thread panicked, and it was caught (it became an error).
    Recovered,
    /// A plugin host (a child process) crashed; its parent reports it.
    Host,
}

impl Kind {
    fn suffix(self) -> &'static str {
        match self {
            Kind::Crash => "",
            Kind::Recovered => "-recovered",
            Kind::Host => "-host",
        }
    }
}

/// A report's contents, before it's written.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub thread: String,
    pub message: String,
    /// Where in Arclight's source it panicked (file:line).
    pub location: String,
    pub backtrace: String,
    /// The last actions, oldest first.
    pub notes: Vec<String>,
    /// The node being evaluated, if one was.
    pub evaluating: String,
    /// What happened to the open project (saved to its autosave, or why
    /// not).
    pub rescue: Option<String>,
}

/// The report as text: the header (version, system), then what happened.
pub fn render(header: &str, kind: Kind, r: &Report) -> String {
    let title = match kind {
        Kind::Crash => "ArclightVFX closed unexpectedly",
        Kind::Recovered => "An internal error was caught (ArclightVFX kept running)",
        Kind::Host => "A plugin host closed unexpectedly",
    };
    let mut out = format!("{title}\n\n{header}\n");
    out += &format!("Thread: {}\nPanic: {}\nAt: {}\n", r.thread, r.message, r.location);
    if !r.evaluating.is_empty() {
        out += &format!("Evaluating: {}\n", r.evaluating);
    }
    if let Some(rescue) = &r.rescue {
        out += &format!("Project: {rescue}\n");
    }
    out += "\nLast actions:\n";
    for n in &r.notes {
        out += &format!("  {n}\n");
    }
    out += &format!("\nBacktrace:\n{}\n", r.backtrace.trim_end());
    out
}

/// `text` with every file path replaced by `<path>` (the home folder by
/// `~` first), so a report can be shared: a word with a `/` or `\` in it
/// (or starting with `~`), except Arclight's and Rust's own source files
/// (`src/…`, `/rustc/…`) in a backtrace.
pub fn scrub(text: &str) -> String {
    let home = std::env::var("HOME").ok().filter(|h| h.len() > 1);
    let text = match &home {
        Some(h) => text.replace(h.as_str(), "~"),
        None => text.to_string(),
    };
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str(&scrub_word(word));
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || c == '"' || c == '\'' || c == '`' {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn scrub_word(word: &str) -> String {
    // Punctuation round it stays (a path in "(… at /a/b.rs:3)").
    let start = word.find(|c: char| !"([{<,;".contains(c)).unwrap_or(word.len());
    let end = word.rfind(|c: char| !")]}>,;:.".contains(c)).map_or(start, |i| i + 1).max(start);
    let (pre, core, post) = (&word[..start], &word[start..end], &word[end..]);
    let ours = core.starts_with("src/") || core.starts_with("./src/") || core.starts_with("/rustc/") || core.starts_with("plugins/") || core.starts_with("python/");
    let path = core.contains('/') || core.contains('\\') || core.starts_with('~');
    if path && !ours && !core.contains("://") {
        // Keep the line number of a source location: <path>:123.
        let line = core.rsplit_once(':').filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).map(|(_, n)| format!(":{n}"));
        format!("{pre}<path>{}{post}", line.unwrap_or_default())
    } else {
        word.to_string()
    }
}

// -- what Arclight was doing ---------------------------------------------

/// A fixed buffer of bytes a signal handler can read without locking or
/// allocating (written outside it; a crash mid-write reads garbled).
struct SigBuf<const N: usize> {
    len: AtomicUsize,
    bytes: [AtomicU8; N],
}

impl<const N: usize> SigBuf<N> {
    const fn new() -> Self {
        SigBuf { len: AtomicUsize::new(0), bytes: [const { AtomicU8::new(0) }; N] }
    }

    fn set(&self, s: &[u8]) {
        let n = s.len().min(N);
        self.len.store(0, Ordering::Release);
        for (b, &v) in self.bytes.iter().zip(&s[..n]) {
            b.store(v, Ordering::Relaxed);
        }
        self.len.store(n, Ordering::Release);
    }

    /// Copy into `out`; how many bytes.
    fn read(&self, out: &mut [u8]) -> usize {
        let n = self.len.load(Ordering::Acquire).min(out.len());
        for (o, b) in out.iter_mut().zip(&self.bytes[..n]) {
            *o = b.load(Ordering::Relaxed);
        }
        n
    }

    fn string(&self) -> String {
        let mut buf = vec![0u8; N];
        let n = self.read(&mut buf);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

static NOTE_LIST: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static NOTES_SIG: SigBuf<4096> = SigBuf::new();
static EVALUATING: SigBuf<160> = SigBuf::new();
static HEADER_SIG: SigBuf<1024> = SigBuf::new();

/// Remember an action for a report (the last [`NOTES`] are kept): opening
/// or saving a project, starting a render, a script's call…. Paths in it
/// are scrubbed when a report is shown.
pub fn note(action: impl Into<String>) {
    let mut list = NOTE_LIST.lock().unwrap_or_else(|e| e.into_inner());
    list.push_back(action.into());
    while list.len() > NOTES {
        list.pop_front();
    }
    let joined: String = list.iter().map(|n| format!("  {n}\n")).collect();
    NOTES_SIG.set(joined.as_bytes());
}

fn notes() -> Vec<String> {
    NOTE_LIST.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
}

/// The node being evaluated now (called for each, so cheap: no lock, no
/// allocation).
pub fn evaluating(op: &str, node: &str) {
    let mut buf = [0u8; 160];
    let mut n = 0;
    for part in [node.as_bytes(), b" (", op.as_bytes(), b")"] {
        let k = part.len().min(buf.len() - n);
        buf[n..n + k].copy_from_slice(&part[..k]);
        n += k;
    }
    EVALUATING.set(&buf[..n]);
}

/// The node evaluated last (see [`evaluating`]), "" before any.
pub fn evaluating_now() -> String {
    EVALUATING.string()
}

/// A caught panic's message (its payload as text).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into())
}

/// What a caught panic tells the user: what went wrong and where, and
/// that a report was saved.
pub fn caught(payload: &(dyn std::any::Any + Send)) -> String {
    let at = evaluating_now();
    let at = if at.is_empty() { String::new() } else { format!(" in {at}") };
    format!("internal error{at}: {} (a crash report was saved; Help ▸ Report a problem)", panic_message(payload))
}

// -- the system ------------------------------------------------------------

static APP: OnceLock<&'static str> = OnceLock::new();
static GPU: Mutex<String> = Mutex::new(String::new());
static SYSTEM: OnceLock<String> = OnceLock::new();

/// The graphics adapter, for reports (the GUI sets it once it knows, and
/// the engine when it creates its GPU device).
pub fn set_gpu(gpu: impl Into<String>) {
    *GPU.lock().unwrap_or_else(|e| e.into_inner()) = gpu.into();
    refresh_header();
}

/// Rebuild the header a fatal signal writes (after the GPU switch or
/// device changed: see [`crate::gpu::crash_line`]).
pub fn refresh_header() {
    HEADER_SIG.set(header().as_bytes());
}

fn first_line(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string()).filter(|s| !s.is_empty())
}

/// OS (name and version), architecture and CPU.
fn system() -> &'static str {
    SYSTEM.get_or_init(|| {
        let os = if cfg!(target_os = "macos") {
            first_line("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
        } else {
            std::fs::read_to_string("/etc/os-release").ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        }
        .unwrap_or_else(|| std::env::consts::OS.to_string());
        let cpu = if cfg!(target_os = "macos") {
            first_line("sysctl", &["-n", "machdep.cpu.brand_string"])
        } else {
            std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| t.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim() == "model name").map(|(_, v)| v.trim().to_string())))
        }
        .unwrap_or_else(|| "unknown CPU".into());
        let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
        format!("OS: {os} ({})\nCPU: {cpu}, {cores} threads", std::env::consts::ARCH)
    })
}

/// Version, system, GPU and whether GPU acceleration is on.
pub fn header() -> String {
    let app = APP.get().copied().unwrap_or("arclight");
    let gpu = GPU.lock().map(|g| g.clone()).unwrap_or_default();
    format!(
        "Version: {} {} ({}), {app}\n{}\nGPU: {}\nGPU acceleration: {}",
        crate::PRODUCT,
        env!("CARGO_PKG_VERSION"),
        crate::STAGE,
        system(),
        if gpu.is_empty() { "not known" } else { gpu.as_str() },
        crate::gpu::crash_line()
    )
}

// -- files -----------------------------------------------------------------

/// "20261030-142501" (UTC) for a time.
fn stamp(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Write report `text` to [`dir`] (the oldest beyond [`KEEP`] removed).
pub fn write_report(kind: Kind, text: &str) -> std::io::Result<PathBuf> {
    write_report_in(&dir(), kind, text)
}

fn write_report_in(dir: &Path, kind: Kind, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let app = APP.get().copied().unwrap_or("arclight");
    let mut path = dir.join(format!("{app}-{}-{}{}.txt", stamp(std::time::SystemTime::now()), std::process::id(), kind.suffix()));
    // (Two in the same second: keep both.)
    let mut k = 1;
    while path.exists() {
        path = dir.join(format!("{app}-{}-{}-{k}{}.txt", stamp(std::time::SystemTime::now()), std::process::id(), kind.suffix()));
        k += 1;
    }
    std::fs::write(&path, text)?;
    let mut all = reports_in(dir);
    while all.len() > KEEP {
        std::fs::remove_file(all.remove(0)).ok();
    }
    Ok(path)
}

/// The reports in [`dir`], oldest first.
pub fn reports() -> Vec<PathBuf> {
    reports_in(&dir())
}

fn reports_in(dir: &Path) -> Vec<PathBuf> {
    let mut all: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    all.sort();
    all.into_iter().map(|(_, p)| p).collect()
}

fn pending_marker(dir: &Path) -> PathBuf {
    dir.join("pending")
}

/// The report of the last crash, if the next start hasn't asked about it
/// yet (see [`clear_pending`]).
pub fn pending() -> Option<PathBuf> {
    pending_in(&dir())
}

fn pending_in(dir: &Path) -> Option<PathBuf> {
    let name = std::fs::read_to_string(pending_marker(dir)).ok()?;
    Some(dir.join(name.trim())).filter(|p| p.exists())
}

/// Asked about: don't ask again at the next start.
pub fn clear_pending() {
    std::fs::remove_file(pending_marker(&dir())).ok();
}

fn set_pending(dir: &Path, report: &Path) {
    if let Some(name) = report.file_name() {
        std::fs::write(pending_marker(dir), name.to_string_lossy().as_bytes()).ok();
    }
}

// -- the hook ----------------------------------------------------------------

type Rescue = Box<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;
static RESCUE: OnceLock<Rescue> = OnceLock::new();

/// What saves the open project on a crash (the GUI: to its autosave),
/// returning where, or why it couldn't. It must not block: it runs in the
/// panic hook, maybe while the project's lock is held (`try_lock` it).
pub fn set_rescue(rescue: impl Fn() -> Result<PathBuf, String> + Send + Sync + 'static) {
    let _ = RESCUE.set(Box::new(rescue));
}

static HOST: AtomicBool = AtomicBool::new(false);
/// A panic's report was written: a native crash after it (a panic across
/// a C callback aborts) leaves that one, the better.
static PANIC_REPORTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install the crash handlers for app `app` ("arclight-gui", "arclight"):
/// the panic hook (after the default one's message) and, on Unix, the
/// native crash handler. Once; later calls do nothing.
pub fn install(app: &'static str) {
    install_as(app, false);
}

/// [`install`] for a plugin host (a child process): reports are marked
/// host, leave no marker for a next start and rescue nothing; their path
/// goes to stderr for the parent to quote.
pub fn install_host(app: &'static str) {
    install_as(app, true);
}

fn install_as(app: &'static str, host: bool) {
    if APP.set(app).is_err() {
        return;
    }
    HOST.store(host, Ordering::Relaxed);
    HEADER_SIG.set(header().as_bytes());
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        if IN_HOOK.with(|f| f.replace(true)) {
            return;
        }
        on_panic(info);
        IN_HOOK.with(|f| f.set(false));
    }));
    #[cfg(unix)]
    native::install(&dir(), host);
}

fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("unnamed").to_string();
    let host = HOST.load(Ordering::Relaxed);
    let kind = if host {
        Kind::Host
    } else if name == "main" {
        Kind::Crash
    } else {
        Kind::Recovered
    };
    let message = panic_message(info.payload());
    let report = Report {
        thread: name,
        message,
        location: info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default(),
        backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        notes: notes(),
        evaluating: EVALUATING.string(),
        rescue: (kind == Kind::Crash).then(rescue).flatten(),
    };
    let dir = dir();
    match write_report_in(&dir, kind, &render(&header(), kind, &report)) {
        Ok(path) => {
            if kind != Kind::Recovered {
                PANIC_REPORTED.store(true, Ordering::Relaxed);
            }
            if kind == Kind::Crash {
                set_pending(&dir, &path);
            }
            eprintln!("arclight: crash report written to {}", path.display());
        }
        Err(e) => eprintln!("arclight: couldn't write a crash report: {e}"),
    }
}

/// Save the open project, if something to save it is set; what happened.
fn rescue() -> Option<String> {
    let f = RESCUE.get()?;
    Some(match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(path)) => format!("saved to its autosave, {}", path.display()),
        Ok(Err(why)) => format!("not saved: {why}"),
        Err(_) => "not saved: saving it failed too".into(),
    })
}

/// Crash on purpose, for trying the handlers out: `ARCLIGHT_CRASH_TEST`
/// set to "panic" panics here, "segv" dereferences null, "abort" aborts.
/// Anything else (or unset) does nothing; "worker" is the GUI's (a panic
/// in the next 2D evaluation, see [`crash_test`]).
pub fn crash_test_now() {
    match crash_test().as_deref() {
        Some("panic") => panic!("ARCLIGHT_CRASH_TEST=panic: a deliberate panic"),
        Some("segv") => {
            note("ARCLIGHT_CRASH_TEST=segv");
            // SAFETY: none: crashing is the point.
            unsafe { std::ptr::null_mut::<u8>().write_volatile(1) }
        }
        Some("abort") => std::process::abort(),
        _ => {}
    }
}

/// `ARCLIGHT_CRASH_TEST`'s value, if set.
pub fn crash_test() -> Option<String> {
    std::env::var("ARCLIGHT_CRASH_TEST").ok().filter(|v| !v.is_empty())
}

// -- native crashes ------------------------------------------------------------

#[cfg(unix)]
mod native {
    //! A handler for SIGSEGV, SIGBUS, SIGILL, SIGFPE and SIGABRT that only
    //! makes async-signal-safe calls: everything it writes was prepared
    //! beforehand (the file names, the header) or is read from fixed
    //! buffers; then the signal's previous handler is put back and the
    //! signal raised again, so the process ends as it would have. It never
    //! holds the process up: the backtrace (which can fault again, or loop,
    //! unwinding from a wild address) comes last, an alarm ends the process
    //! with the signal if the handler takes over [`LIMIT`] seconds, and a
    //! second crash meanwhile (another thread's, or in the handler) goes
    //! straight to ending it.
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    const SIGNALS: [(libc::c_int, &[u8]); 5] = [
        (libc::SIGSEGV, b"SIGSEGV (bad memory access)"),
        (libc::SIGBUS, b"SIGBUS (bad memory access)"),
        (libc::SIGILL, b"SIGILL (illegal instruction)"),
        (libc::SIGFPE, b"SIGFPE (arithmetic error)"),
        (libc::SIGABRT, b"SIGABRT (aborted)"),
    ];

    /// The report's and the marker's paths, and the report's name, NUL-ended.
    static REPORT_PATH: SigBuf<1024> = SigBuf::new();
    static MARKER_PATH: SigBuf<1024> = SigBuf::new();
    static REPORT_NAME: SigBuf<256> = SigBuf::new();
    static NO_MARKER: AtomicBool = AtomicBool::new(false);
    /// A crash is being handled; the signal it was.
    static HANDLING: AtomicBool = AtomicBool::new(false);
    static SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    /// The most seconds the handler may take.
    const LIMIT: libc::c_uint = 3;
    /// For a test: the handler hangs after the report, as a backtrace
    /// stuck unwinding would.
    #[cfg(test)]
    pub(super) static STALL: AtomicBool = AtomicBool::new(false);
    static mut PREVIOUS: [Option<libc::sigaction>; 5] = [None; 5];
    /// The stack the handler runs on (the thread's may be what overflowed).
    static mut ALT_STACK: [u8; 64 * 1024] = [0; 64 * 1024];
    /// The handler's buffers: static, not on its stack, which on other
    /// threads is Rust's small signal stack (8 KiB): one crash is handled
    /// at a time ([`HANDLING`]).
    static mut PATH: [u8; 1024] = [0; 1024];
    static mut BUF: [u8; 4096] = [0; 4096];
    #[cfg(any(target_env = "gnu", target_os = "macos"))]
    static mut FRAMES: [*mut libc::c_void; 64] = [std::ptr::null_mut(); 64];

    pub fn install(dir: &Path, host: bool) {
        std::fs::create_dir_all(dir).ok();
        let app = APP.get().copied().unwrap_or("arclight");
        let name = format!("{app}-{}-{}-native{}.txt", stamp(std::time::SystemTime::now()), std::process::id(), if host { "-host" } else { "" });
        let nul = |p: &Path| {
            let mut b = p.as_os_str().as_bytes().to_vec();
            b.push(0);
            b
        };
        REPORT_PATH.set(&nul(&dir.join(&name)));
        MARKER_PATH.set(&nul(&pending_marker(dir)));
        REPORT_NAME.set(name.as_bytes());
        NO_MARKER.store(host, Ordering::Relaxed);
        // SAFETY: plain libc calls with valid, 'static buffers; installing
        // handlers happens once, before other threads use them.
        unsafe {
            // (backtrace() loads its unwinder the first time: not in the
            // handler.)
            #[cfg(any(target_env = "gnu", target_os = "macos"))]
            {
                let mut frames = [std::ptr::null_mut::<libc::c_void>(); 4];
                libc::backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int);
            }
            let stack = libc::stack_t { ss_sp: std::ptr::addr_of_mut!(ALT_STACK) as *mut libc::c_void, ss_flags: 0, ss_size: 64 * 1024 };
            libc::sigaltstack(&stack, std::ptr::null_mut());
            for (k, (sig, _)) in SIGNALS.iter().enumerate() {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = handler as *const () as libc::sighandler_t;
                act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut act.sa_mask);
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(*sig, &act, &mut old) == 0 {
                    (*std::ptr::addr_of_mut!(PREVIOUS))[k] = Some(old);
                }
            }
        }
    }

    /// End the process with signal `sig` as its default action does (a
    /// core dump for these): no handler of ours, the signal unblocked.
    unsafe fn end_with(sig: libc::c_int) -> ! {
        libc::signal(sig, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, sig);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(sig);
        libc::_exit(128 + sig)
    }

    /// The alarm the handler set went off: it got stuck (a backtrace
    /// looping, say). End with the crash's signal.
    extern "C" fn too_long(_: libc::c_int) {
        // SAFETY: async-signal-safe calls only.
        unsafe {
            put(2, b"arclight: the crash handler took too long; ending\n");
            let sig = SIGNAL.load(Ordering::Relaxed);
            end_with(if sig > 0 { sig } else { libc::SIGSEGV })
        }
    }

    unsafe fn put(fd: libc::c_int, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let n = libc::write(fd, bytes[done..].as_ptr() as *const libc::c_void, bytes.len() - done);
            if n <= 0 {
                return;
            }
            done += n as usize;
        }
    }

    extern "C" fn handler(sig: libc::c_int, _info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        // SAFETY: only async-signal-safe calls (open, write, close,
        // backtrace_symbols_fd, sigaction, raise) on stack or 'static
        // buffers.
        unsafe {
            // A crash while one is handled (another thread's, or this one
            // faulting again): end now.
            if HANDLING.swap(true, Ordering::SeqCst) {
                end_with(sig);
            }
            SIGNAL.store(sig, Ordering::Relaxed);
            // (SIGALRM is taken only now, the process ending anyway.)
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = too_long as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(libc::SIGALRM, &act, std::ptr::null_mut());
            libc::alarm(LIMIT);
            let k = SIGNALS.iter().position(|(s, _)| *s == sig);
            let path = &mut *std::ptr::addr_of_mut!(PATH);
            let n = if PANIC_REPORTED.load(Ordering::Relaxed) { 0 } else { REPORT_PATH.read(path) };
            if n > 0 {
                let fd = libc::open(path.as_ptr() as *const libc::c_char, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o644);
                if fd >= 0 {
                    let buf = &mut *std::ptr::addr_of_mut!(BUF);
                    put(fd, b"ArclightVFX closed unexpectedly (a native crash)\n\n");
                    let n = HEADER_SIG.read(buf);
                    put(fd, &buf[..n]);
                    put(fd, b"\nSignal: ");
                    put(fd, k.map_or(b"unknown" as &[u8], |k| SIGNALS[k].1));
                    put(fd, b"\nEvaluating: ");
                    let n = EVALUATING.read(buf);
                    put(fd, &buf[..n]);
                    put(fd, b"\n\nLast actions:\n");
                    let n = NOTES_SIG.read(buf);
                    put(fd, &buf[..n]);
                    // The report is complete enough now: marked pending
                    // before the backtrace, which may not come back.
                    if !NO_MARKER.load(Ordering::Relaxed) {
                        let n = MARKER_PATH.read(path);
                        if n > 0 {
                            let m = libc::open(path.as_ptr() as *const libc::c_char, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o644);
                            if m >= 0 {
                                let n = REPORT_NAME.read(buf);
                                put(m, &buf[..n]);
                                libc::close(m);
                            }
                        }
                    }
                    put(2, b"arclight: native crash; a report was written to the crash reports folder\n");
                    #[cfg(test)]
                    while STALL.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                    put(fd, b"\nBacktrace (addresses in the binary: addr2line -e <binary> -f <offset> names them):\n");
                    #[cfg(any(target_env = "gnu", target_os = "macos"))]
                    {
                        let frames = &mut *std::ptr::addr_of_mut!(FRAMES);
                        let n = libc::backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int);
                        libc::backtrace_symbols_fd(frames.as_ptr(), n, fd);
                    }
                    libc::close(fd);
                }
            }
            // As it would have ended: the handler before ours (Rust's
            // stack overflow message, or the default: a core dump).
            match k.and_then(|k| (*std::ptr::addr_of!(PREVIOUS))[k]) {
                Some(old) => {
                    libc::sigaction(sig, &old, std::ptr::null_mut());
                }
                None => {
                    libc::signal(sig, libc::SIG_DFL);
                }
            }
            libc::raise(sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_name_no_files() {
        let home = std::env::var("HOME").unwrap_or_default();
        let text = format!("failed to read {home}/shots/plate.0001.exr: missing\n  at src/ops.rs:120\n  at /rustc/abc/library/core/src/x.rs:5\n  at {home}/.cargo/registry/src/foo-1.0/lib.rs:44\n(C:\\Users\\me\\a.mov) see https://example.com/x \"/tmp/a b\"");
        let s = scrub(&text);
        assert!(!s.contains(&home) || home.is_empty(), "{s}");
        assert!(s.contains("failed to read <path>: missing"), "{s}");
        assert!(s.contains("at src/ops.rs:120") && s.contains("at /rustc/abc/library/core/src/x.rs:5"), "ours kept: {s}");
        assert!(s.contains("at <path>:44"), "the line kept: {s}");
        assert!(s.contains("(<path>)") && s.contains("https://example.com/x"), "{s}");
        assert!(s.contains("\"<path> b\""), "{s}");
    }

    #[test]
    fn a_report_says_what_happened_and_where() {
        let header = header();
        assert!(header.contains(env!("CARGO_PKG_VERSION")) && header.contains("OS: ") && header.contains("CPU: ") && header.contains("GPU: ") && header.contains("GPU acceleration: "), "{header}");
        let r = Report {
            thread: "main".into(),
            message: "index out of bounds".into(),
            location: "src/ops.rs:12".into(),
            backtrace: "   0: arclight::ops::blur\n".into(),
            notes: vec!["open".into(), "render".into()],
            evaluating: "blur1 (blur)".into(),
            rescue: Some("saved to its autosave".into()),
        };
        let text = render(&header, Kind::Crash, &r);
        for want in ["ArclightVFX closed unexpectedly", "Thread: main", "Panic: index out of bounds", "At: src/ops.rs:12", "Evaluating: blur1 (blur)", "Project: saved", "  open\n  render\n", "arclight::ops::blur"] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(render(&header, Kind::Recovered, &r).starts_with("An internal error was caught"));
    }

    #[test]
    fn only_the_last_actions_are_kept_and_the_node_being_evaluated() {
        for i in 0..40 {
            note(format!("action {i}"));
        }
        let kept = notes();
        assert_eq!(kept.len(), NOTES);
        assert_eq!((kept[0].as_str(), kept[NOTES - 1].as_str()), ("action 10", "action 39"));
        assert!(NOTES_SIG.string().ends_with("  action 39\n"));
        evaluating("blur", "blur1");
        assert_eq!(EVALUATING.string(), "blur1 (blur)");
        evaluating("op", &"n".repeat(500));
        assert_eq!(EVALUATING.string().len(), 160, "cut, not overflowing");
    }

    #[test]
    fn reports_are_kept_to_the_last_few_and_the_crash_is_pending() {
        let dir = std::env::temp_dir().join(format!("arclight_crash_files_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let mut last = PathBuf::new();
        for i in 0..KEEP + 5 {
            last = write_report_in(&dir, if i % 2 == 0 { Kind::Crash } else { Kind::Recovered }, &format!("report {i}")).unwrap();
        }
        let all = reports_in(&dir);
        assert_eq!(all.len(), KEEP);
        assert_eq!(std::fs::read_to_string(all.last().unwrap()).unwrap(), format!("report {}", KEEP + 4));
        assert!(last.to_string_lossy().ends_with(".txt") && reports_in(&dir).contains(&last));
        assert!(all.iter().any(|p| p.to_string_lossy().ends_with("-recovered.txt")));
        assert_eq!(pending_in(&dir), None);
        set_pending(&dir, &last);
        assert_eq!(pending_in(&dir), Some(last.clone()));
        std::fs::remove_file(pending_marker(&dir)).unwrap();
        assert_eq!(pending_in(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stamps_are_utc_dates() {
        let t = |secs: u64| stamp(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
        assert_eq!(t(0), "19700101-000000");
        assert_eq!(t(951782400), "20000229-000000");
        assert_eq!(t(1793370301), "20261030-142501");
    }

    /// A child process (this test binary again) installs the handlers and
    /// crashes as asked; it leaves a report, and a pending marker for a
    /// crash of its main thread.
    #[test]
    fn crashes_leave_a_report() {
        const CHILD: &str = "ARCLIGHT_CRASH_CHILD";
        if let Ok(how) = std::env::var(CHILD) {
            install("crash-test");
            note("about to crash");
            evaluating("blur", "blur1");
            set_rescue(|| Err("nothing open".into()));
            match how.as_str() {
                // As if on the main thread (the test runner's is "main"
                // only in the child: run on a thread named so).
                "panic" => {
                    let _ = std::thread::Builder::new().name("main".into()).spawn(|| panic!("on purpose")).unwrap().join();
                    std::process::exit(0);
                }
                "worker" => {
                    let _ = std::thread::Builder::new().name("eval".into()).spawn(|| panic!("caught")).unwrap().join();
                    std::process::exit(0);
                }
                // A thread jumping to a wild address, as a driver's thread
                // did at exit: the backtrace starts from garbage.
                #[cfg(unix)]
                "wild" => {
                    let _ = std::thread::spawn(|| {
                        // SAFETY: none: crashing is the point.
                        let f: extern "C" fn() = unsafe { std::mem::transmute(0x170usize) };
                        f();
                    })
                    .join();
                }
                // On a thread of its own (Rust's small signal stack).
                #[cfg(unix)]
                "thread" => {
                    // SAFETY: none: crashing is the point.
                    let _ = std::thread::spawn(|| unsafe { std::ptr::null_mut::<u8>().write_volatile(1) }).join();
                }
                #[cfg(unix)]
                "stuck" => {
                    native::STALL.store(true, Ordering::Relaxed);
                    crash_test_now();
                }
                _ => crash_test_now(),
            }
            return;
        }
        let exe = std::env::current_exe().unwrap();
        for how in ["panic", "worker", "segv", "thread", "wild", "stuck"] {
            if matches!(how, "segv" | "thread" | "wild" | "stuck") && !cfg!(unix) {
                continue;
            }
            let dir = std::env::temp_dir().join(format!("arclight_crash_child_{}_{how}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            let mut child = std::process::Command::new(&exe)
                .args(["--exact", "crash::tests::crashes_leave_a_report", "--nocapture", "--test-threads=1"])
                .env(CHILD, how)
                .env("ARCLIGHT_CRASH_TEST", if matches!(how, "segv" | "stuck") { "segv" } else { "" })
                .env("ARCLIGHT_CRASH_DIR", &dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            // The handler never holds the process up: ended by the signal
            // within its time limit, whatever the backtrace does.
            let t0 = std::time::Instant::now();
            while child.try_wait().unwrap().is_none() {
                if t0.elapsed().as_secs() > 20 {
                    child.kill().ok();
                    panic!("{how}: the crashed process didn't end");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let out = child.wait_with_output().unwrap();
            #[cfg(unix)]
            if matches!(how, "segv" | "thread" | "wild" | "stuck") {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(out.status.signal(), Some(libc::SIGSEGV), "{how}: {:?}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
            }
            if how == "stuck" {
                assert!(String::from_utf8_lossy(&out.stderr).contains("crash handler took too long"));
            }
            let all = reports_in(&dir);
            assert_eq!(all.len(), 1, "{how}: {all:?}\n{}", String::from_utf8_lossy(&out.stderr));
            let text = std::fs::read_to_string(&all[0]).unwrap();
            let name = all[0].file_name().unwrap().to_string_lossy().into_owned();
            assert!(text.contains("Version: ArclightVFX Compositor ") && text.contains("(beta), crash-test") && text.contains("about to crash") && text.contains("blur1 (blur)"), "{how}: {text}");
            match how {
                "panic" => {
                    assert!(text.contains("Panic: on purpose") && text.contains("Project: not saved: nothing open") && text.contains("Backtrace:"), "{text}");
                    assert_eq!(pending_in(&dir).as_deref(), Some(all[0].as_path()));
                }
                "worker" => {
                    assert!(name.ends_with("-recovered.txt") && text.contains("Thread: eval") && !text.contains("Project:"), "{name}: {text}");
                    assert_eq!(pending_in(&dir), None, "no question at the next start");
                }
                _ => {
                    assert!(!out.status.success());
                    assert!(name.ends_with("-native.txt") && text.contains("Signal: SIGSEGV"), "{name}: {text}");
                    if how == "segv" && cfg!(any(target_env = "gnu", target_os = "macos")) {
                        assert!(text.lines().skip_while(|l| !l.starts_with("Backtrace")).count() > 3, "frames: {text}");
                    }
                    assert_eq!(pending_in(&dir).as_deref(), Some(all[0].as_path()));
                }
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
