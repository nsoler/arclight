//! File ▸ Collect Media into Project Folder… and the warning that leads to
//! it: a project with no folder, or reading files outside it, saves their
//! paths absolute, so it won't open on another machine. After a save that
//! leaves such files, the menu bar says so (once for the same files, until
//! dismissed); the window lists every file the project reads and writes,
//! what collecting does with each and how much it copies against the free
//! space there, and, on Collect, copies them into `<folder>/media/` on a
//! thread of its own (with progress and Cancel), points the nodes at the
//! copies and gives the project its folder (one undo step), then saves.
//! Originals are never moved, deleted or written over; outputs aren't
//! copied, only pointed into `<folder>/out/` when asked. The work is
//! `arclight::collect`'s.

use crate::dialogs;
use crate::doc::Doc;
use arclight::collect::{self, Plan, Progress, Report, Role, State, Survey};
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

/// The warning, the window and the copying under way.
#[derive(Default)]
pub struct CollectMedia {
    window: Option<Window>,
    /// The menu bar's warning, until dismissed or seen to.
    warning: Option<String>,
    /// What's been warned about this session (see `Survey::warning_key`).
    warned: HashSet<String>,
}

/// The window's state.
struct Window {
    survey: Survey,
    /// Where to collect into (None: not chosen yet).
    folder: Option<PathBuf>,
    /// Point outputs into `<folder>/out/` too.
    outputs: bool,
    /// The plan for `folder` and `outputs`, and the free space there.
    plan: Option<(PathBuf, bool, Plan, Option<u64>)>,
    browse: Option<dialogs::Pending<()>>,
    job: Option<Job>,
    /// How the last Collect went: done (true) or not, and what to say.
    outcome: Option<(bool, String)>,
}

/// A copy under way.
struct Job {
    plan: Plan,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Progress,
}

enum Msg {
    Progress(Progress),
    Done(anyhow::Result<Report>),
}

impl CollectMedia {
    /// Open the window on `doc`'s files.
    pub fn open(&mut self, doc: &Doc) {
        if self.window.as_ref().is_some_and(|w| w.job.is_some()) {
            return;
        }
        let survey = collect::survey(&doc.graph, doc.path.as_deref());
        // The project's folder, else its file's.
        let folder = survey.folder.clone().or_else(|| doc.graph.project.folder_for(doc.path.as_deref()));
        self.window = Some(Window { survey, folder, outputs: false, plan: None, browse: None, job: None, outcome: None });
    }

    /// Another project is open: no window, no warning (copying under way
    /// carries on; its result is dropped).
    pub fn close(&mut self) {
        if let Some(job) = self.window.as_ref().and_then(|w| w.job.as_ref()) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.window = None;
        self.warning = None;
    }

    /// `doc` was saved: warn (in the menu bar) about files that won't move
    /// with it, unless the same ones were warned about already.
    pub fn saved(&mut self, doc: &Doc) {
        let survey = collect::survey(&doc.graph, doc.path.as_deref());
        match survey.summary() {
            Some(s) => {
                if self.warned.insert(survey.warning_key()) {
                    self.warning = Some(s);
                }
            }
            None => self.warning = None,
        }
    }

    /// The menu bar's warning, if there is one: a button opening the
    /// window, and × to dismiss it.
    pub fn bar_ui(&mut self, ui: &mut egui::Ui, doc: &Doc) {
        let Some(s) = self.warning.clone() else { return };
        ui.separator();
        let text = egui::RichText::new(format!("⚠ {s}")).color(egui::Color32::from_rgb(235, 180, 80));
        let tip = "This project won't find these files on another machine. Click to collect them into the project folder (File ▸ Collect Media into Project Folder…)";
        if ui.add(egui::Button::new(text).frame(false)).on_hover_text(tip).clicked() {
            self.open(doc);
        }
        if ui.small_button("×").on_hover_text("Dismiss (shown again if other files are outside)").clicked() {
            self.warning = None;
        }
    }

    /// The window, when open. True when it collected and the project
    /// should be saved now.
    pub fn ui(&mut self, ctx: &egui::Context, doc: &mut Doc) -> bool {
        let Some(w) = self.window.as_mut() else { return false };
        let mut save = false;
        // A folder chosen with Choose….
        match w.browse.as_ref().and_then(|b| b.poll()) {
            Some(answer) => {
                w.browse = None;
                if let Some(dir) = answer {
                    w.folder = Some(dir);
                }
            }
            None if w.browse.is_some() => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            None => {}
        }
        // Copying: progress, and the end of it.
        if let Some(job) = w.job.as_mut() {
            let mut done = None;
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    Msg::Progress(p) => job.progress = p,
                    Msg::Done(r) => done = Some(r),
                }
            }
            match done {
                Some(Ok(report)) => {
                    let plan = w.job.take().unwrap().plan;
                    collect::relink(&mut doc.graph, &plan);
                    doc.changed();
                    w.outcome = Some((true, outcome_text(&report, &plan)));
                    w.survey = collect::survey(&doc.graph, doc.path.as_deref());
                    w.plan = None;
                    self.warning = None;
                    save = true;
                }
                Some(Err(e)) => {
                    w.job = None;
                    w.outcome = Some((false, format!("Nothing relinked: {e:#}")));
                    w.plan = None;
                }
                None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        // The plan for the folder chosen.
        if let Some(folder) = w.folder.clone() {
            if w.plan.as_ref().is_none_or(|(f, o, ..)| *f != folder || *o != w.outputs) {
                let plan = collect::plan(&w.survey, &folder, w.outputs);
                let free = collect::free_space(&folder);
                w.plan = Some((folder, w.outputs, plan, free));
            }
        } else {
            w.plan = None;
        }

        let (mut collect_now, mut close, mut browse, mut cancel) = (false, false, false, false);
        let mut keep_open = true;
        let busy = w.job.is_some();
        let window = egui::Window::new("Collect Media into Project Folder").open(&mut keep_open).collapsible(false).resizable(true).default_width(760.0);
        let window = window.pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center());
        window.show(ctx, |ui| {
            ui.label("Copies the files this project reads into its folder's media/ and points the nodes at the copies, so the folder opens anywhere. Originals stay where they are.");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Project folder");
                match &w.folder {
                    Some(f) => ui.monospace(f.display().to_string()),
                    None => ui.weak("none yet: choose one"),
                };
                browse = ui.add_enabled(!busy && !dialogs::busy(), egui::Button::new("Choose…")).clicked();
            });
            if w.survey.folder.is_none() && w.folder.is_some() {
                ui.weak("The project has no folder: it gets this one.");
            }
            ui.add_enabled(!busy, egui::Checkbox::new(&mut w.outputs, "Also point outputs (Write, Media Out, Write Geo) into the folder's out/"))
                .on_hover_text("Renders and exports then go into the project folder; nothing is copied for them");
            ui.add_space(4.0);
            let plan = w.plan.as_ref().map(|(_, _, p, _)| p);
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("collect_media_files").num_columns(5).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                    for h in ["Node", "File", "Files", "Size", "Collecting"] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for r in &w.survey.refs {
                        ui.label(&r.node);
                        let shown = r.path.display().to_string();
                        ui.add(egui::Label::new(egui::RichText::new(&shown).monospace()).truncate()).on_hover_text(&shown);
                        ui.label(match r.role {
                            Role::Output => "output".to_string(),
                            Role::Input if r.is_sequence() => format!("{} frames", r.files.len()),
                            Role::Input => r.files.len().to_string(),
                        });
                        ui.label(if r.role == Role::Input && r.state != State::Missing { collect::size(r.bytes) } else { String::new() });
                        let (what, tip) = fate(r, plan, w.folder.as_deref());
                        let l = ui.label(what);
                        if let Some(t) = tip {
                            l.on_hover_text(t);
                        }
                        ui.end_row();
                    }
                });
                if w.survey.refs.is_empty() {
                    ui.weak("This project reads and writes no files.");
                }
            });
            ui.separator();
            let mut fits = true;
            if let Some((folder, _, plan, free)) = &w.plan {
                let n = plan.copies.len();
                let mut line = format!("To copy: {} in {n} file{}", collect::size(plan.bytes()), if n == 1 { "" } else { "s" });
                if !plan.reused.is_empty() {
                    line += &format!(" ({} already there)", plan.reused.len());
                }
                line += &match free {
                    Some(f) => format!(" · free there: {}", collect::size(*f)),
                    None => " · free space unknown".to_string(),
                };
                ui.label(line);
                if free.is_some_and(|f| plan.bytes() > f) {
                    fits = false;
                    let msg = format!("Not enough space at {}: free some, or choose another folder.", folder.display());
                    ui.colored_label(egui::Color32::from_rgb(235, 100, 90), msg);
                }
            }
            if let Some(job) = &w.job {
                let p = job.progress;
                let frac = if p.bytes > 0 { p.bytes_done as f32 / p.bytes as f32 } else if p.files > 0 { p.files_done as f32 / p.files as f32 } else { 0.0 };
                let text = format!("{} / {} files · {} of {}", p.files_done, p.files, collect::size(p.bytes_done), collect::size(p.bytes));
                ui.add(egui::ProgressBar::new(frac).text(text));
            }
            if let Some((ok, msg)) = &w.outcome {
                let color = if *ok { egui::Color32::from_rgb(110, 200, 120) } else { egui::Color32::from_rgb(235, 100, 90) };
                ui.colored_label(color, msg);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if busy {
                    cancel = ui.button("Cancel copying").clicked();
                } else {
                    close = ui.button("Close").clicked();
                    let ready = plan.is_some_and(|p| !p.is_empty()) && fits;
                    let b = ui.add_enabled(ready, egui::Button::new("Collect"));
                    let b = b.on_hover_text("Copy, point the nodes at the copies, then save the project").on_disabled_hover_text(match plan {
                        None => "Choose a project folder first",
                        Some(p) if p.is_empty() => "Nothing to collect",
                        _ => "Not enough space there",
                    });
                    collect_now = b.clicked();
                }
            });
        });
        if browse && w.browse.is_none() {
            let start = w.folder.clone().or_else(|| doc.graph.project.folder_for(doc.path.as_deref()));
            let dialog = dialogs::new();
            let dialog = match start.filter(|s| s.is_dir()) {
                Some(dir) => dialog.set_directory(dir),
                None => dialog,
            };
            w.browse = dialogs::pick_folder((), dialog);
        }
        if cancel {
            if let Some(job) = &w.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
        if collect_now {
            if let Some((_, _, plan, _)) = &w.plan {
                w.outcome = None;
                w.job = Some(start(plan.clone()));
                ctx.request_repaint();
            }
        }
        if (close || !keep_open) && w.job.is_none() {
            self.window = None;
        }
        save
    }
}

/// Copy `plan`'s files on a thread of its own.
fn start(plan: Plan) -> Job {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (plan.clone(), cancel.clone());
    std::thread::spawn(move || {
        let progress = tx.clone();
        let r = collect::copy(&p, |pr| drop(progress.send(Msg::Progress(pr))), &c);
        let _ = tx.send(Msg::Done(r));
    });
    Job { progress: Progress { files: plan.copies.len(), bytes: plan.bytes(), ..Progress::default() }, plan, rx, cancel }
}

/// What collecting does with `r` (under `plan`, into `folder`), and more
/// on hover.
fn fate(r: &collect::Ref, plan: Option<&Plan>, folder: Option<&Path>) -> (String, Option<String>) {
    let shown = |p: &str| match folder.and_then(|f| Path::new(p).strip_prefix(f).ok()) {
        Some(rel) => rel.display().to_string(),
        None => p.to_string(),
    };
    let relink = plan.and_then(|p| p.relinks.iter().find(|l| l.node == r.node && l.param == r.param));
    let skipped = plan.and_then(|p| p.skipped.iter().find(|(s, _)| s.node == r.node && s.param == r.param)).map(|(_, why)| why.clone());
    let inside = folder.is_some_and(|f| r.path.strip_prefix(f).is_ok_and(|rel| !rel.as_os_str().is_empty()));
    match (r.role, relink) {
        (_, None) if inside => ("in the folder already".into(), None),
        (Role::Output, Some(l)) => (format!("points to {}", shown(&l.path)), None),
        (Role::Output, None) => ("left as is (output)".into(), Some("Tick “Also point outputs” to point it into out/".into())),
        (Role::Input, Some(l)) => (format!("→ {}", shown(&l.path)), skipped),
        (Role::Input, None) if r.state == State::Missing => ("missing: left as is".into(), Some("The file isn't on disk, so there's nothing to copy".into())),
        (Role::Input, None) => (if plan.is_some() { "left as is".into() } else { "copied once a folder is chosen".into() }, skipped),
    }
}

/// What a finished Collect says.
fn outcome_text(report: &Report, plan: &Plan) -> String {
    let mut s = format!("Copied {} file{} ({})", report.copied, if report.copied == 1 { "" } else { "s" }, collect::size(report.bytes));
    if report.reused > 0 {
        s += &format!(", {} already there", report.reused);
    }
    s += &format!(", relinked {} param{}", plan.relinks.len(), if plan.relinks.len() == 1 { "" } else { "s" });
    let missing = plan.skipped.iter().filter(|(r, _)| r.files.is_empty()).count();
    if missing > 0 {
        s += &format!("; {missing} missing file{} left as is", if missing == 1 { "" } else { "s" });
    }
    s + ". Saving the project."
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;
    use serde_json::json;

    fn frame(ctx: &egui::Context, c: &mut CollectMedia, doc: &mut Doc) -> bool {
        let mut save = false;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), ..Default::default() };
        let _ = ctx.run_ui(input, |ui| save |= c.ui(ui.ctx(), doc));
        save
    }

    /// Saving a project with files outside its folder warns once; the
    /// window collects them on a thread, relinks as one undo step, asks
    /// for a save, and the warning goes.
    #[test]
    fn the_window_collects_relinks_and_asks_to_save() {
        let root = std::env::temp_dir().join(format!("arclight_collect_gui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("show")).unwrap();
        std::fs::write(root.join("src/a.png"), b"a").unwrap();
        let g = Graph { nodes: vec![serde_json::from_value(json!({ "id": "a", "op": "read", "inputs": [], "params": { "path": root.join("src/a.png").to_str().unwrap() } })).unwrap()], ..Graph::default() };
        let mut doc = Doc::new(g, Some(root.join("show/comp.json")));
        let mut c = CollectMedia::default();
        c.saved(&doc);
        assert_eq!(c.warning.as_deref(), Some("No project folder: 1 file saved with absolute paths"));
        c.warning = None;
        c.saved(&doc);
        assert_eq!(c.warning, None, "the same files aren't warned about twice");

        c.open(&doc);
        let ctx = egui::Context::default();
        assert!(!frame(&ctx, &mut c, &mut doc));
        let w = c.window.as_mut().unwrap();
        assert_eq!(w.folder.as_deref(), Some(root.join("show").as_path()), "the project file's folder");
        let plan = w.plan.as_ref().unwrap().2.clone();
        assert_eq!(plan.copies.len(), 1);
        w.job = Some(start(plan));
        let mut saved = false;
        for _ in 0..500 {
            saved |= frame(&ctx, &mut c, &mut doc);
            if saved {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(saved, "asks to save once copied");
        assert_eq!(doc.graph.project.folder.as_deref(), Some(root.join("show").as_path()));
        assert_eq!(doc.graph.nodes[0].params["path"], root.join("show/media/a.png").to_str().unwrap());
        assert!(root.join("src/a.png").is_file(), "the original stays");
        doc.commit(false);
        doc.undo();
        assert_eq!(doc.graph.nodes[0].params["path"], root.join("src/a.png").to_str().unwrap(), "one undo step back");
        assert_eq!(doc.graph.project.folder, None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
