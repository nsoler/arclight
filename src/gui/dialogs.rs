//! File dialogs that don't stop the app.
//!
//! A system file dialog (the XDG desktop portal's picker on Linux) answers
//! only when it closes; shown from the UI thread it would stop the event
//! loop all that time, and the compositor would call the window "not
//! responding" (Hyprland does after a few seconds). So each one runs on a
//! thread of its own ([`start`]) and its answer is picked up by polling
//! ([`Pending::poll`]) once a frame, then applied as if it came at once.
//! One at a time: [`busy`] says one is open. Dialogs are built by [`new`],
//! which makes them belong to the app's window (see [`set_parent`]), so the
//! system shows them over it. Add file-type filters with [`filter`], which
//! matches extensions in either case. They start in the folders
//! [`set_folders`] gives: [`pick`] and [`save`] (media read, renders
//! written) in the project's folder; [`pick_project`] in the Projects
//! folder (Preferences), [`save_project`] in the project's.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Mutex;

/// Where dialogs start (None: wherever the system likes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folders {
    /// The open project's folder (see `Project::folder_for`).
    pub project: Option<PathBuf>,
    /// Where projects go (Preferences ▸ Files and safety).
    pub projects: Option<PathBuf>,
}

static FOLDERS: Mutex<Folders> = Mutex::new(Folders { project: None, projects: None });

/// Start dialogs in `folders` from now on.
pub fn set_folders(folders: Folders) {
    *FOLDERS.lock().unwrap_or_else(|e| e.into_inner()) = folders;
}

/// The first of the folders `order` picks from `folders` that's set and
/// exists.
fn first_folder(folders: &Folders, order: fn(&Folders) -> [&Option<PathBuf>; 2]) -> Option<PathBuf> {
    order(folders).into_iter().flatten().find(|p| p.is_dir()).cloned()
}

/// Media read and renders written: the project's folder.
const FOR_MEDIA: fn(&Folders) -> [&Option<PathBuf>; 2] = |f| [&f.project, &None];
/// Projects opened: the Projects folder, else the open project's.
const FOR_OPEN: fn(&Folders) -> [&Option<PathBuf>; 2] = |f| [&f.projects, &f.project];
/// A project saved: its folder, else the Projects folder.
const FOR_SAVE: fn(&Folders) -> [&Option<PathBuf>; 2] = |f| [&f.project, &f.projects];

/// `d` starting in the first of the folders `order` picks that's set and
/// exists.
fn in_folder(d: rfd::FileDialog, order: fn(&Folders) -> [&Option<PathBuf>; 2]) -> rfd::FileDialog {
    let dir = first_folder(&FOLDERS.lock().unwrap_or_else(|e| e.into_inner()), order);
    match dir {
        Some(dir) => d.set_directory(dir),
        None => d,
    }
}

/// A dialog is open.
static OPEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// A blank dialog belonging to the app's window (set once a frame).
    static TEMPLATE: RefCell<rfd::FileDialog> = RefCell::new(rfd::FileDialog::new());
}

/// Make dialogs built from now on belong to `parent`, the app's window.
/// Call once a frame (the window's handle can change).
pub fn set_parent(parent: &eframe::Frame) {
    TEMPLATE.with(|t| *t.borrow_mut() = rfd::FileDialog::new().set_parent(parent));
}

/// A new file dialog over the app's window, to configure and [`start`].
pub fn new() -> rfd::FileDialog {
    TEMPLATE.with(|t| t.borrow().clone())
}

/// Dialog `d` with a filter `name` for files ending in `exts` (lower case),
/// in lower or upper case: the portal's filters are case-sensitive globs, so
/// `*.mp4` alone hides a camera's `CLIP.MP4`.
pub fn filter(d: rfd::FileDialog, name: &str, exts: &[&str]) -> rfd::FileDialog {
    d.add_filter(name, &both_cases(exts))
}

/// `exts` and their upper-case versions.
fn both_cases(exts: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = exts.iter().map(|e| e.to_ascii_lowercase()).collect();
    for e in exts {
        let up = e.to_ascii_uppercase();
        if !all.contains(&up) {
            all.push(up);
        }
    }
    all
}

/// Whether a dialog is open (another can't be started until it closes).
pub fn busy() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// A dialog showing on its own thread; `what` says what its answer is for.
pub struct Pending<T> {
    pub what: T,
    answer: Receiver<Option<PathBuf>>,
}

impl<T> Pending<T> {
    /// The answer, once the dialog has closed: the file chosen, or None if
    /// it was cancelled. None while it's still open.
    pub fn poll(&self) -> Option<Option<PathBuf>> {
        match self.answer.try_recv() {
            Ok(path) => Some(path),
            Err(TryRecvError::Empty) => None,
            // The thread ended without answering (it panicked): cancelled.
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

/// Clears the open flag when the dialog's thread ends, however it ends.
struct Closed;

impl Drop for Closed {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Release);
    }
}

/// Run `show` (a dialog's `pick_file` or `save_file`) on a thread of its
/// own, its answer for `what`. None, and nothing shown, while another is
/// open.
pub fn start<T>(what: T, show: impl FnOnce() -> Option<PathBuf> + Send + 'static) -> Option<Pending<T>> {
    if OPEN.swap(true, Ordering::AcqRel) {
        return None;
    }
    let (tx, answer) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("file dialog".into()).spawn(move || {
        let closed = Closed;
        let path = show();
        // Free for the next before the answer arrives, so whoever polls it
        // can start another straight away.
        drop(closed);
        let _ = tx.send(path);
    });
    if spawned.is_err() {
        OPEN.store(false, Ordering::Release);
        return None;
    }
    Some(Pending { what, answer })
}

/// Choose a file to read with dialog `d` (starting in the project's
/// folder).
pub fn pick<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    let d = in_folder(d, FOR_MEDIA);
    start(what, move || d.pick_file())
}

/// Choose where to write with dialog `d` (starting in the project's
/// folder).
pub fn save<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    let d = in_folder(d, FOR_MEDIA);
    start(what, move || d.save_file())
}

/// Choose a project to open with dialog `d` (in the Projects folder, else
/// the open project's).
pub fn pick_project<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    let d = in_folder(d, FOR_OPEN);
    start(what, move || d.pick_file())
}

/// Choose where to save a project with dialog `d` (in its folder, else the
/// Projects folder).
pub fn save_project<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    let d = in_folder(d, FOR_SAVE);
    start(what, move || d.save_file())
}

/// Choose a folder with dialog `d`.
pub fn pick_folder<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    start(what, move || d.pick_folder())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Tests that open dialogs take turns (there's one flag for the app).
    pub(crate) static TURN: Mutex<()> = Mutex::new(());

    /// Poll until it answers (a few seconds at most).
    pub(crate) fn wait<T>(p: &Pending<T>) -> Option<PathBuf> {
        let t0 = Instant::now();
        loop {
            if let Some(a) = p.poll() {
                return a;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn dialogs_start_in_the_project_folder_or_the_projects_folder() {
        let tmp = std::env::temp_dir().join(format!("arclight_dialog_folders_{}", std::process::id()));
        let (project, projects) = (tmp.join("show"), tmp.join("shows"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        let both = Folders { project: Some(project.clone()), projects: Some(projects.clone()) };
        assert_eq!([FOR_MEDIA, FOR_OPEN, FOR_SAVE].map(|o| first_folder(&both, o)), [Some(project.clone()), Some(projects.clone()), Some(project.clone())]);
        let no_project = Folders { project: None, ..both.clone() };
        assert_eq!([FOR_MEDIA, FOR_OPEN, FOR_SAVE].map(|o| first_folder(&no_project, o)), [None, Some(projects.clone()), Some(projects.clone())]);
        // One that's gone is passed over.
        let gone = Folders { projects: Some(tmp.join("gone")), ..both };
        assert_eq!(first_folder(&gone, FOR_OPEN), Some(project));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn filters_match_either_case() {
        assert_eq!(both_cases(&["mp4", "exr"]), ["mp4", "exr", "MP4", "EXR"]);
        assert_eq!(both_cases(&["JPG"]), ["jpg", "JPG"]);
    }

    #[test]
    fn one_dialog_at_a_time_answered_by_polling() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        // A fake dialog, held open until told to answer.
        let (go, wait_go) = mpsc::channel::<Option<PathBuf>>();
        let p = start("open", move || wait_go.recv().unwrap()).unwrap();
        assert!(busy());
        assert_eq!(p.poll(), None, "still open: no answer, and polling doesn't wait");
        assert!(start("another", || None).is_none(), "one at a time");
        go.send(Some(PathBuf::from("/clips/a.mov"))).unwrap();
        assert_eq!((p.what, wait(&p)), ("open", Some(PathBuf::from("/clips/a.mov"))));
        assert!(!busy());
        // Cancelled: None. A dialog that panics counts as cancelled and
        // frees the way too.
        assert_eq!(wait(&start((), || None).unwrap()), None);
        let p = start((), || panic!("the dialog broke")).unwrap();
        assert_eq!(wait(&p), None);
        assert!(!busy());
    }
}
