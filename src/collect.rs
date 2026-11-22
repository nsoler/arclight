//! Collecting a project's media into its folder (docs: TODO.md, "Projects
//! without a folder save media paths absolute"). A project saves the files
//! it reads and writes relative to its folder only when they're in it
//! ([`Graph::to_json_at`]); [`survey`] finds the ones that aren't (or a
//! project with no folder at all), [`plan`] works out copies of them into
//! `<folder>/media/` and where each node then points, [`copy`] makes them
//! (never moving, deleting or overwriting a file) and [`relink`] points the
//! nodes at the copies. Outputs (Write, Media Out, Write Geo) are never
//! copied; [`plan`] can point them into `<folder>/out/`.
//!
//! What's copied for a reference: the file; a video whole; a sequence
//! (`####`, `%04d`) as every frame of it on disk; an OBJ with its material
//! libraries and their textures, a `.gltf` with its buffers and images,
//! when those sit in the model's folder or under it (they keep their
//! layout beside the copy; others are listed as not collected).

use crate::graph::Graph;
use crate::media;
use crate::ops::{self, ParamKind};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The subfolder of the project's folder media are collected into.
pub const MEDIA: &str = "media";
/// The subfolder outputs are pointed into, when asked.
pub const OUT: &str = "out";

/// Whether a file param is read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Input,
    Output,
}

/// Where a referenced file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// In the project's folder: saved relative to it already.
    Inside,
    /// Elsewhere (or the project has no folder): saved absolute.
    Outside,
    /// Not on disk (an input), so nothing to copy.
    Missing,
}

/// One file param of one node.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub node: String,
    pub op: String,
    pub param: String,
    pub role: Role,
    /// The path as the node reads or writes it, made whole (a sequence's
    /// pattern as it is).
    pub path: PathBuf,
    /// The files on disk it stands for (an input): itself, a sequence's
    /// frames, a model and the companions it needs.
    pub files: Vec<PathBuf>,
    /// Their size, in bytes.
    pub bytes: u64,
    pub state: State,
    /// Companions it needs from outside the model's folder: not collected.
    pub strays: Vec<PathBuf>,
}

impl Ref {
    /// A sequence's pattern (frames, not one file).
    pub fn is_sequence(&self) -> bool {
        media::is_sequence_pattern(&self.path.to_string_lossy())
    }
}

/// A project's file references, and its folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Survey {
    /// `project.folder`, whole (None: it has none, and saves every path
    /// absolute).
    pub folder: Option<PathBuf>,
    pub refs: Vec<Ref>,
}

impl Survey {
    /// The references that won't move with the project: every one when
    /// it has no folder, else those outside it (inputs missing from disk
    /// too, when they're outside).
    pub fn warnings(&self) -> Vec<&Ref> {
        self.refs.iter().filter(|r| self.folder.is_none() || r.state != State::Inside).collect()
    }

    /// One line on [`warnings`](Self::warnings), if there are any: what
    /// the menu bar and Project Settings say.
    pub fn summary(&self) -> Option<String> {
        let n = self.warnings().len();
        if n == 0 {
            return None;
        }
        let files = if n == 1 { "1 file".to_string() } else { format!("{n} files") };
        Some(match self.folder {
            None => format!("No project folder: {files} saved with absolute paths"),
            Some(_) => format!("{files} outside the project folder (saved with absolute paths)"),
        })
    }

    /// A key for what [`warnings`](Self::warnings) holds: the same set of
    /// paths warned about once is not warned about again.
    pub fn warning_key(&self) -> String {
        let mut paths: Vec<String> = self.warnings().iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        paths.sort();
        paths.dedup();
        format!("{:?}|{}", self.folder, paths.join("\n"))
    }
}

/// `graph`'s file references (the project saved at `file`, if it is).
pub fn survey(graph: &Graph, file: Option<&Path>) -> Survey {
    let file_dir = file.map(whole).and_then(|f| f.parent().map(Path::to_path_buf));
    let folder = graph.project.folder.as_deref().map(|f| match &file_dir {
        Some(d) if f.is_relative() => clean(&d.join(f)),
        _ => whole(f),
    });
    let mut refs = Vec::new();
    for n in &graph.nodes {
        let Some(info) = ops::info(&n.op) else { continue };
        for spec in info.params {
            let ParamKind::Path { save } = spec.kind else { continue };
            let Some(Value::String(p)) = n.params.get(spec.name) else { continue };
            if p.is_empty() {
                continue;
            }
            let path = whole(Path::new(&graph.project.resolve_path(file, p)));
            let role = if save { Role::Output } else { Role::Input };
            let (files, strays) = if save { (Vec::new(), Vec::new()) } else { files_of(&path) };
            let bytes = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
            let inside = folder.as_deref().is_some_and(|d| path.strip_prefix(d).is_ok_and(|r| !r.as_os_str().is_empty()));
            let state = if inside {
                State::Inside
            } else if role == Role::Input && files.is_empty() {
                State::Missing
            } else {
                State::Outside
            };
            refs.push(Ref { node: n.id.clone(), op: n.op.clone(), param: spec.name.to_string(), role, path, files, bytes, state, strays });
        }
    }
    Survey { folder, refs }
}

/// The files on disk an input at `path` stands for, and the companions it
/// needs from outside its folder.
fn files_of(path: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let text = path.to_string_lossy();
    if media::is_sequence_pattern(&text) {
        let frames = match media::detect_sequence_range(&text) {
            Some((a, b)) => (a..=b).map(|f| media::frame_path(&text, f)).filter(|p| p.is_file()).collect(),
            None => Vec::new(),
        };
        return (frames, Vec::new());
    }
    if !path.is_file() {
        return (Vec::new(), Vec::new());
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    let mut files = vec![path.to_path_buf()];
    let mut strays = Vec::new();
    for c in companions(path) {
        let c = clean(&c);
        if !c.is_file() || files.contains(&c) || strays.contains(&c) {
            continue;
        }
        if c.strip_prefix(dir).is_ok() {
            files.push(c);
        } else {
            strays.push(c);
        }
    }
    (files, strays)
}

/// The files a model reads beside it: an OBJ's material libraries and
/// their textures (as `mesh::read_obj` finds them), a `.gltf`'s buffers and
/// images (not `data:` ones).
fn companions(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut out = Vec::new();
    match ext.as_str() {
        "obj" => {
            let Ok(text) = std::fs::read_to_string(path) else { return out };
            for line in text.lines() {
                let words: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
                let Some((&"mtllib", args)) = words.split_first() else { continue };
                let joined = dir.join(args.join(" "));
                let libs: Vec<PathBuf> = if joined.is_file() { vec![joined] } else { args.iter().map(|a| dir.join(a)).collect() };
                for lib in libs {
                    if let Ok(src) = std::fs::read_to_string(&lib) {
                        let lib_dir = lib.parent().unwrap_or(dir).to_path_buf();
                        out.extend(crate::mesh::parse_mtl(&src, &lib_dir).into_iter().filter_map(|m| m.texture));
                    }
                    out.push(lib);
                }
            }
        }
        "gltf" => {
            let Ok(doc) = std::fs::read_to_string(path).map(|t| serde_json::from_str::<Value>(&t).unwrap_or(Value::Null)) else { return out };
            for list in ["buffers", "images"] {
                for item in doc[list].as_array().into_iter().flatten() {
                    if let Some(uri) = item["uri"].as_str().filter(|u| !u.starts_with("data:") && !u.contains("://")) {
                        out.push(dir.join(percent_decoded(uri)));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// A URI's `%XX` escapes decoded (as glTF's relative URIs are written).
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One file to copy.
#[derive(Clone, Debug, PartialEq)]
pub struct CopyFile {
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
}

/// A node's file param pointed at a new path (whole).
#[derive(Clone, Debug, PartialEq)]
pub struct Relink {
    pub node: String,
    pub param: String,
    pub path: String,
}

/// What collecting does: the folder it collects into, the copies to make,
/// the copies already there (identical files, so reused), and the params
/// pointed at them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub folder: PathBuf,
    pub copies: Vec<CopyFile>,
    /// Files already at their destination, the same bytes: not copied.
    pub reused: Vec<PathBuf>,
    pub relinks: Vec<Relink>,
    /// Inputs left as they are, and why (missing, companions outside).
    pub skipped: Vec<(Ref, String)>,
}

impl Plan {
    /// Bytes to copy.
    pub fn bytes(&self) -> u64 {
        self.copies.iter().map(|c| c.bytes).sum()
    }

    /// Whether it does anything.
    pub fn is_empty(&self) -> bool {
        self.copies.is_empty() && self.relinks.is_empty()
    }
}

/// Collecting `survey`'s references into `folder` (whole): each input
/// outside it, once however many nodes read it, into `folder/media/` (a
/// sequence or a model with companions in a folder of its own there),
/// under a name not yet taken unless the same bytes are there already;
/// with `outputs`, each output outside it pointed into `folder/out/`.
pub fn plan(survey: &Survey, folder: &Path, outputs: bool) -> Plan {
    let folder = whole(folder);
    let mut plan = Plan { folder: folder.clone(), ..Plan::default() };
    let inside = |p: &Path| p.strip_prefix(&folder).is_ok_and(|r| !r.as_os_str().is_empty());
    // Destinations given so far (paths, and whole folders for units).
    let mut taken: BTreeSet<PathBuf> = BTreeSet::new();
    // Source path → new path, for a source read by several nodes.
    let mut done: Vec<(PathBuf, String)> = Vec::new();
    for r in &survey.refs {
        if inside(&r.path) {
            continue;
        }
        if r.role == Role::Output {
            if outputs {
                let name = r.path.file_name().map(PathBuf::from).unwrap_or_default();
                let path = folder.join(OUT).join(name);
                plan.relinks.push(Relink { node: r.node.clone(), param: r.param.clone(), path: path.to_string_lossy().into_owned() });
            }
            continue;
        }
        if r.files.is_empty() {
            plan.skipped.push((r.clone(), "missing: no file to copy".into()));
            continue;
        }
        if !r.strays.is_empty() {
            let list: Vec<String> = r.strays.iter().map(|s| s.display().to_string()).collect();
            plan.skipped.push((r.clone(), format!("copied, but these files it uses are outside its folder and stay where they are: {}", list.join(", "))));
        }
        if let Some((_, to)) = done.iter().find(|(from, _)| *from == r.path) {
            plan.relinks.push(Relink { node: r.node.clone(), param: r.param.clone(), path: to.clone() });
            continue;
        }
        let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let src_dir = r.path.parent().unwrap_or(Path::new("/")).to_path_buf();
        // A folder of its own for frames or a model's companions.
        let own_folder = r.is_sequence() || r.files.len() > 1;
        let stem = unit_stem(&name);
        let media = folder.join(MEDIA);
        let mut n = 1;
        let (dest_root, pairs) = loop {
            let suffix = if n == 1 { String::new() } else { format!("_{n}") };
            let (root, pairs): (PathBuf, Vec<(PathBuf, PathBuf)>) = if own_folder {
                let root = media.join(format!("{stem}{suffix}"));
                let pairs = r.files.iter().map(|f| (f.clone(), root.join(f.strip_prefix(&src_dir).unwrap_or(Path::new(f.file_name().unwrap_or_default()))))).collect();
                (root, pairs)
            } else {
                let file = media.join(with_suffix(&name, &suffix));
                (file.clone(), vec![(r.files[0].clone(), file)])
            };
            let clash_taken = taken.iter().any(|t| t.starts_with(&root) || root.starts_with(t));
            let fits = !clash_taken && pairs.iter().all(|(from, to)| !to.exists() || same_bytes(from, to));
            if fits || n > 9999 {
                break (root, pairs);
            }
            n += 1;
        };
        taken.insert(dest_root.clone());
        for (from, to) in pairs {
            if to.exists() {
                plan.reused.push(to);
            } else {
                let bytes = std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
                plan.copies.push(CopyFile { from, to, bytes });
            }
        }
        let to = if own_folder { dest_root.join(&name) } else { dest_root };
        let to = to.to_string_lossy().into_owned();
        done.push((r.path.clone(), to.clone()));
        plan.relinks.push(Relink { node: r.node.clone(), param: r.param.clone(), path: to });
    }
    plan
}

/// A name for a sequence's or model's folder: the file name without its
/// frame number and extension (`plate.####.exr` → `plate`).
fn unit_stem(name: &str) -> String {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = match stem.rfind('%') {
        Some(i) if media::is_sequence_pattern(&stem[i..]) => stem[..i].to_string(),
        _ => stem.trim_end_matches('#').to_string(),
    };
    let stem = stem.trim_end_matches(['.', '_', '-', ' ']);
    if stem.is_empty() { "sequence".to_string() } else { stem.to_string() }
}

/// `name` with `suffix` before its extension.
fn with_suffix(name: &str, suffix: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}{suffix}{}", &name[..i], &name[i..]),
        _ => format!("{name}{suffix}"),
    }
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if ma.len() != mb.len() || !mb.is_file() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let (Ok(na), Ok(nb)) = (read_full(&mut fa, &mut ba), read_full(&mut fb, &mut bb)) else { return false };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// How far [`copy`] has got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub files_done: usize,
    pub files: usize,
    pub bytes_done: u64,
    pub bytes: u64,
}

/// What [`copy`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub copied: usize,
    pub bytes: u64,
    pub reused: usize,
}

/// Free space for a new file at `path` (or the nearest folder above it
/// that exists), in bytes; None if it can't be told.
pub fn free_space(path: &Path) -> Option<u64> {
    let mut dir = whole(path);
    while !dir.exists() {
        dir = dir.parent()?.to_path_buf();
    }
    free_space_at(&dir)
}

#[cfg(unix)]
fn free_space_at(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a statvfs to fill.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

#[cfg(windows)]
fn free_space_at(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut avail = 0u64;
    // SAFETY: a NUL-terminated wide string; the others may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(avail)
}

#[cfg(not(any(unix, windows)))]
fn free_space_at(_: &Path) -> Option<u64> {
    None
}

/// Why `plan` won't fit at its folder, if it won't: its bytes against the
/// free space there.
pub fn wont_fit(plan: &Plan) -> Option<String> {
    let free = free_space(&plan.folder)?;
    (plan.bytes() > free).then(|| format!("Not enough space: {} to copy, {} free at {}", size(plan.bytes()), size(free), plan.folder.display()))
}

/// Make `plan`'s copies, reporting each file done to `progress`; stops
/// (with an error) when `cancel` is set, leaving the files copied so far
/// (the one under way is removed). Each file goes to `<name>.part` first,
/// then takes its name, never over a file that's there; nothing is moved
/// or deleted but its own `.part` files.
pub fn copy(plan: &Plan, mut progress: impl FnMut(Progress), cancel: &AtomicBool) -> Result<Report> {
    if let Some(why) = wont_fit(plan) {
        bail!(why);
    }
    let mut p = Progress { files: plan.copies.len(), bytes: plan.bytes(), ..Progress::default() };
    progress(p);
    let mut report = Report { reused: plan.reused.len(), ..Report::default() };
    for c in &plan.copies {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled: {} of {} files copied", p.files_done, p.files);
        }
        if c.to.exists() {
            bail!("{} is there already: not overwritten", c.to.display());
        }
        let dir = c.to.parent().context("a destination with no folder")?;
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        let mut part = c.to.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let copied = copy_file(&c.from, &part, cancel).and_then(|n| {
            if c.to.exists() {
                bail!("{} is there already: not overwritten", c.to.display());
            }
            std::fs::rename(&part, &c.to).with_context(|| format!("naming {}", c.to.display()))?;
            Ok(n)
        });
        match copied {
            Ok(n) => {
                report.copied += 1;
                report.bytes += n;
                p.files_done += 1;
                p.bytes_done += n;
                progress(p);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        }
    }
    Ok(report)
}

/// Copy `from` to `to` (a new file), in chunks so `cancel` is seen within
/// a big file; its bytes.
fn copy_file(from: &Path, to: &Path, cancel: &AtomicBool) -> Result<u64> {
    use std::io::Write;
    let mut src = std::fs::File::open(from).with_context(|| format!("reading {}", from.display()))?;
    let mut dst = std::fs::OpenOptions::new().write(true).create_new(true).open(to).with_context(|| format!("writing {}", to.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let k = src.read(&mut buf).with_context(|| format!("reading {}", from.display()))?;
        if k == 0 {
            break;
        }
        dst.write_all(&buf[..k]).with_context(|| format!("writing {}", to.display()))?;
        n += k as u64;
    }
    dst.sync_all().ok();
    if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
        let _ = dst.set_modified(t);
    }
    Ok(n)
}

/// Point `graph`'s nodes as `plan` says and give the project its folder.
pub fn relink(graph: &mut Graph, plan: &Plan) {
    for r in &plan.relinks {
        if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == r.node) {
            if let Some(params) = n.params.as_object_mut() {
                params.insert(r.param.clone(), Value::String(r.path.clone()));
            } else {
                n.params = serde_json::json!({ r.param.clone(): r.path.clone() });
            }
        }
    }
    graph.project.folder = Some(plan.folder.clone());
}

/// A size for people: `289.4 MB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// `path` made absolute against the working folder, `.` and `..` out.
fn whole(path: &Path) -> PathBuf {
    clean(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// `.` and `..` taken out (without looking at the disk).
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeSpec;
    use serde_json::json;

    /// A fresh folder of its own under the temp folder.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arclight_collect_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn node(id: &str, op: &str, params: Value) -> NodeSpec {
        serde_json::from_value(json!({ "id": id, "op": op, "inputs": [], "params": params })).unwrap()
    }

    fn param<'a>(g: &'a Graph, id: &str, name: &str) -> &'a str {
        g.nodes.iter().find(|n| n.id == id).unwrap().params[name].as_str().unwrap()
    }

    /// The bytes and modified times of every file under `dir`.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), std::fs::read(&p).unwrap(), std::fs::metadata(&p).unwrap().modified().unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// Stills, videos, sequences (every frame on disk), an OBJ with its
    /// material and texture, a glTF with its buffer: surveyed, planned
    /// into media/, copied, relinked; the originals untouched; saved, the
    /// paths are relative and the folder moves with them.
    #[test]
    fn media_outside_the_folder_are_collected_into_it_and_the_project_moves() {
        let root = scratch("all");
        let src = root.join("elsewhere");
        std::fs::create_dir_all(&src).unwrap();
        image::RgbImage::from_pixel(4, 2, image::Rgb([200, 100, 50])).save(src.join("still.png")).unwrap();
        write(&src.join("clip.mp4"), &[7u8; 3000]);
        for f in [1, 2, 3, 5] {
            write(&src.join(format!("plates/plate.{f:04}.exr")), format!("frame {f}").as_bytes());
        }
        write(&src.join("plates/other.txt"), b"not a frame");
        write(&src.join("model/box.obj"), b"mtllib box.mtl\nv 0 0 0\n");
        write(&src.join("model/box.mtl"), b"newmtl a\nmap_Kd tex/wood.png\n");
        write(&src.join("model/tex/wood.png"), b"wood");
        write(&src.join("scene/thing.gltf"), br#"{"buffers": [{"uri": "thing%20data.bin"}], "images": [{"uri": "data:image/png;base64,AA"}]}"#);
        write(&src.join("scene/thing data.bin"), b"bin");
        let before = snapshot(&src);
        let proj = root.join("show");
        std::fs::create_dir_all(&proj).unwrap();
        let mut g = Graph { nodes: vec![
            node("still", "read", json!({ "path": src.join("still.png").to_str().unwrap() })),
            node("still_again", "read", json!({ "path": src.join("still.png").to_str().unwrap() })),
            node("clip", "media_in", json!({ "path": src.join("clip.mp4").to_str().unwrap() })),
            node("plate", "read", json!({ "path": src.join("plates/plate.####.exr").to_str().unwrap() })),
            node("geo", "read_geo", json!({ "file": src.join("model/box.obj").to_str().unwrap() })),
            node("gltf", "read_geo", json!({ "file": src.join("scene/thing.gltf").to_str().unwrap() })),
            node("gone", "read", json!({ "path": src.join("nope.png").to_str().unwrap() })),
            node("out", "write", json!({ "path": "/renders/comp.####.png" })),
        ], ..Graph::default() };
        let file = proj.join("comp.json");
        // No folder: every reference warned about.
        let s = survey(&g, Some(&file));
        assert_eq!(s.folder, None);
        assert_eq!(s.warnings().len(), 8);
        assert!(s.summary().unwrap().starts_with("No project folder: 8 files"));
        let plate = s.refs.iter().find(|r| r.node == "plate").unwrap();
        assert_eq!(plate.files.len(), 4, "every frame on disk, gap and all");
        assert_eq!(s.refs.iter().find(|r| r.node == "geo").unwrap().files.len(), 3, "the OBJ, its library and texture");
        assert_eq!(s.refs.iter().find(|r| r.node == "gltf").unwrap().files.len(), 2, "the glTF and its buffer");
        assert_eq!(s.refs.iter().find(|r| r.node == "gone").unwrap().state, State::Missing);
        assert_eq!(s.refs.iter().find(|r| r.node == "clip").unwrap().bytes, 3000);

        let p = plan(&s, &proj, false);
        assert_eq!(p.copies.len(), 1 + 1 + 4 + 3 + 2, "the still once though read twice");
        assert_eq!(p.skipped.len(), 1, "the missing one");
        assert!(p.relinks.iter().all(|r| r.node != "out"), "outputs only when asked");
        let mut seen = Vec::new();
        let report = copy(&p, |pr| seen.push(pr), &AtomicBool::new(false)).unwrap();
        assert_eq!(report.copied, 11);
        assert_eq!(seen.last().unwrap().files_done, 11);
        assert_eq!(seen.last().unwrap().bytes_done, p.bytes());
        assert_eq!(snapshot(&src), before, "originals untouched");
        relink(&mut g, &p);

        let media = proj.join(MEDIA);
        assert_eq!(param(&g, "still", "path"), media.join("still.png").to_str().unwrap());
        assert_eq!(param(&g, "still_again", "path"), param(&g, "still", "path"));
        assert_eq!(param(&g, "clip", "path"), media.join("clip.mp4").to_str().unwrap());
        assert_eq!(param(&g, "plate", "path"), media.join("plate/plate.####.exr").to_str().unwrap());
        assert_eq!(param(&g, "geo", "file"), media.join("box/box.obj").to_str().unwrap());
        assert!(media.join("box/tex/wood.png").is_file() && media.join("thing/thing data.bin").is_file(), "companions keep their layout");
        assert!(media.join("plate/plate.0005.exr").is_file() && !media.join("plate/other.txt").exists());
        assert_eq!(param(&g, "out", "path"), "/renders/comp.####.png");
        assert_eq!(param(&g, "gone", "path"), src.join("nope.png").to_str().unwrap());

        // Saved: relative to the folder, which is the file's; nothing left
        // to warn about but the missing file and the output.
        g.save(&file).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["project"]["folder"], ".");
        assert!(saved["nodes"].as_array().unwrap().iter().any(|n| n["params"]["path"] == "media/plate/plate.####.exr"));
        let s = survey(&Graph::from_file(&file).unwrap(), Some(&file));
        let left: Vec<&str> = s.warnings().iter().map(|r| r.node.as_str()).collect();
        assert_eq!(left, ["gone", "out"]);
        // Moved whole, it finds its media.
        let moved = root.join("moved");
        std::fs::rename(&proj, &moved).unwrap();
        let g = Graph::from_file(&moved.join("comp.json")).unwrap();
        assert!(Path::new(param(&g, "plate", "path")).starts_with(&moved));
        assert!(media::frame_path(param(&g, "plate", "path"), 5).is_file());
        assert!(Path::new(param(&g, "geo", "file")).is_file());
        // And reads it: the copy, not the original (gone now).
        std::fs::remove_file(src.join("still.png")).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let img = crate::graph::Evaluator::new().evaluate(&g, "still", &ctx).expect("the copy renders");
        assert_eq!((img.width, img.height), (4, 2));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A name taken by other bytes gets a new one; the same bytes are
    /// reused, not copied; outputs are pointed into out/ when asked; files
    /// in the folder are left alone; nothing's overwritten.
    #[test]
    fn names_taken_get_new_ones_identical_files_are_reused() {
        let root = scratch("names");
        let proj = root.join("show");
        write(&root.join("a/plate.png"), b"one");
        write(&root.join("b/plate.png"), b"two!");
        write(&root.join("c/same.png"), b"same");
        write(&proj.join("media/same.png"), b"same");
        write(&proj.join("media/plate.png"), b"other bytes");
        write(&proj.join("in/here.png"), b"here");
        let mut g = Graph { nodes: vec![
            node("a", "read", json!({ "path": root.join("a/plate.png").to_str().unwrap() })),
            node("b", "read", json!({ "path": root.join("b/plate.png").to_str().unwrap() })),
            node("c", "read", json!({ "path": root.join("c/same.png").to_str().unwrap() })),
            node("here", "read", json!({ "path": proj.join("in/here.png").to_str().unwrap() })),
            node("w", "write", json!({ "path": "/elsewhere/r.####.exr" })),
            node("geo_out", "write_geo", json!({ "file": proj.join("geo/out.obj").to_str().unwrap() })),
        ], ..Graph::default() };
        g.project.folder = Some(proj.clone());
        let s = survey(&g, None);
        let warned: Vec<&str> = s.warnings().iter().map(|r| r.node.as_str()).collect();
        assert_eq!(warned, ["a", "b", "c", "w"], "the folder's own files and outputs aren't");
        let p = plan(&s, &proj, true);
        let to: Vec<&Path> = p.copies.iter().map(|c| c.to.as_path()).collect();
        assert_eq!(to, [proj.join("media/plate_2.png"), proj.join("media/plate_3.png")]);
        assert_eq!(p.reused, [proj.join("media/same.png")]);
        copy(&p, |_| {}, &AtomicBool::new(false)).unwrap();
        relink(&mut g, &p);
        assert_eq!(std::fs::read(proj.join("media/plate.png")).unwrap(), b"other bytes", "not overwritten");
        assert_eq!(param(&g, "b", "path"), proj.join("media/plate_3.png").to_str().unwrap());
        assert_eq!(param(&g, "c", "path"), proj.join("media/same.png").to_str().unwrap());
        assert_eq!(param(&g, "w", "path"), proj.join("out/r.####.exr").to_str().unwrap());
        assert_eq!(param(&g, "here", "path"), proj.join("in/here.png").to_str().unwrap());
        // Collected again: nothing left to do.
        let again = plan(&survey(&g, None), &proj, true);
        assert!(again.is_empty(), "{again:?}");
        // A copy whose name was taken meanwhile isn't written over.
        write(&root.join("d/fresh.png"), b"fresh");
        let mut late = plan(&survey(&Graph { nodes: vec![node("x", "read", json!({ "path": root.join("d/fresh.png").to_str().unwrap() }))], ..g.clone() }, None), &proj, false);
        late.copies[0].to = proj.join("media/plate.png");
        assert!(copy(&late, |_| {}, &AtomicBool::new(false)).is_err());
        assert_eq!(std::fs::read(proj.join("media/plate.png")).unwrap(), b"other bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Cancelled, it stops, leaving no .part file; companions outside a
    /// model's folder are reported, not copied; a plan bigger than the
    /// disk is refused before anything's copied.
    #[test]
    fn cancel_strays_and_space() {
        let root = scratch("cancel");
        write(&root.join("src/a.png"), b"a");
        write(&root.join("src/b.png"), b"b");
        write(&root.join("src/m/box.obj"), b"mtllib ../shared.mtl\n");
        write(&root.join("src/shared.mtl"), b"newmtl a\n");
        let g = Graph { nodes: vec![
            node("a", "read", json!({ "path": root.join("src/a.png").to_str().unwrap() })),
            node("b", "read", json!({ "path": root.join("src/b.png").to_str().unwrap() })),
            node("m", "read_geo", json!({ "file": root.join("src/m/box.obj").to_str().unwrap() })),
        ], ..Graph::default() };
        let proj = root.join("show");
        let s = survey(&g, None);
        assert_eq!(s.refs[2].strays, [root.join("src/shared.mtl")]);
        let p = plan(&s, &proj, false);
        assert!(p.skipped.iter().any(|(r, why)| r.node == "m" && why.contains("shared.mtl")));
        let cancel = AtomicBool::new(false);
        let err = copy(&p, |pr| if pr.files_done == 1 { cancel.store(true, Ordering::Relaxed) }, &cancel).unwrap_err();
        assert!(err.to_string().contains("cancelled: 1 of 3"), "{err}");
        let parts = snapshot(&proj).into_iter().filter(|(f, ..)| f.to_string_lossy().ends_with(".part")).count();
        assert_eq!(parts, 0);
        let mut huge = p.clone();
        huge.copies[0].bytes = u64::MAX / 2;
        if free_space(&proj).is_some() {
            assert!(wont_fit(&huge).unwrap().starts_with("Not enough space"));
            assert!(copy(&huge, |_| {}, &AtomicBool::new(false)).is_err());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Names and sizes as shown.
    #[test]
    fn stems_and_sizes() {
        assert_eq!(unit_stem("plate.####.exr"), "plate");
        assert_eq!(unit_stem("shot_%04d.dpx"), "shot");
        assert_eq!(unit_stem("####.png"), "sequence");
        assert_eq!(unit_stem("box.obj"), "box");
        assert_eq!(with_suffix("a.b.png", "_2"), "a.b_2.png");
        assert_eq!(with_suffix("noext", "_2"), "noext_2");
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(289_400_000), "289.4 MB");
        assert_eq!(percent_decoded("a%20b%zz"), "a b%zz");
    }
}
