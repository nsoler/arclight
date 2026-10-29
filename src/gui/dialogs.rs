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
//! system shows them over it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

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

/// Choose a file to open with dialog `d`.
pub fn pick<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    start(what, move || d.pick_file())
}

/// Choose where to save with dialog `d`.
pub fn save<T>(what: T, d: rfd::FileDialog) -> Option<Pending<T>> {
    start(what, move || d.save_file())
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
