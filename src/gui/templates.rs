//! Templates in the app (see `arclight::templates`): Insert ▸ Save
//! selection as template… (asks a name), Insert ▸ Manage templates…
//! (rename, delete, show the folder), the menus listing them (Insert ▸
//! Templates, File ▸ New from template), and putting one into the
//! project, as a paste does. The examples shipped with the app are listed
//! as built-in templates.

use crate::doc::Doc;
use arclight::templates::{self, Template};
use eframe::egui;

/// The user's templates, then the built-in ones.
pub fn all(builtin: &[Template]) -> Vec<Template> {
    let mut out = templates::list();
    out.extend(builtin.iter().cloned());
    out
}

/// Template `t` merged into `doc` with its top-left at `at` (graph units),
/// its nodes selected, one undo step: how many nodes it brought.
pub fn insert(doc: &mut Doc, t: &Template, at: [f32; 2]) -> anyhow::Result<usize> {
    let g = t.load()?;
    let before = doc.graph.nodes.len();
    crate::clipboard::put(doc, &g, Some(at));
    Ok(doc.graph.nodes.len() - before)
}

/// A menu of `list` (the user's, then the built-in ones under a line):
/// the one clicked.
pub fn menu(ui: &mut egui::Ui, list: &[Template]) -> Option<Template> {
    let mut chosen = None;
    if list.iter().all(|t| t.builtin) {
        ui.weak("No templates of your own yet: select nodes, then Insert ▸ Save selection as template…");
    }
    let mut had_own = false;
    for t in list {
        if t.builtin && had_own {
            ui.separator();
            had_own = false;
        }
        had_own |= !t.builtin;
        let tip = if t.builtin { format!("Comes with Arclight (Help ▸ Examples): {}", t.path.display()) } else { t.path.display().to_string() };
        if ui.button(&t.name).on_hover_text(tip).clicked() {
            chosen = Some(t.clone());
            ui.close();
        }
    }
    chosen
}

/// Show `dir` in the system's file browser (made first if need be).
pub fn reveal(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(dir).spawn().map(|_| ())
}

/// The Save selection as template… and Manage templates… windows.
#[derive(Default)]
pub struct Dialogs {
    save: Option<Save>,
    manage: Option<Manage>,
}

struct Save {
    name: String,
    /// Why the last try didn't save.
    error: Option<String>,
    /// The name is taken: the next Save replaces it.
    taken: bool,
    just_opened: bool,
}

#[derive(Default)]
struct Manage {
    /// The template being renamed (its file) and the name typed so far.
    renaming: Option<(std::path::PathBuf, String)>,
    /// Delete clicked once for this one: click again to delete.
    deleting: Option<std::path::PathBuf>,
    error: Option<String>,
}

impl Dialogs {
    pub fn open_save(&mut self) {
        self.save = Some(Save { name: String::new(), error: None, taken: false, just_opened: true });
    }

    pub fn open_manage(&mut self) {
        self.manage = Some(Manage::default());
    }

    /// Draw whichever is open; `doc`'s selection is what Save saves. A
    /// note for the menu bar when something was saved.
    pub fn ui(&mut self, ctx: &egui::Context, doc: &Doc) -> Option<String> {
        let note = self.save_ui(ctx, doc);
        self.manage_ui(ctx);
        note
    }

    fn save_ui(&mut self, ctx: &egui::Context, doc: &Doc) -> Option<String> {
        let s = self.save.as_mut()?;
        let (mut open, mut done, mut note) = (true, false, None);
        let nodes = doc.selection();
        let backdrops: Vec<String> = doc.selected_backdrop.iter().cloned().collect();
        egui::Window::new("Save selection as template").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            if nodes.is_empty() && backdrops.is_empty() {
                ui.label("Select the nodes (or a backdrop) to keep as a template first.");
                return;
            }
            let what = if nodes.is_empty() { "the selected backdrop and its nodes".to_string() } else { format!("{} selected node{}", nodes.len(), if nodes.len() == 1 { "" } else { "s" }) };
            ui.label(format!("Keep {what} as a template, to insert from Insert ▸ Templates or the node browser (⇧Tab), or start a project from."));
            let edit = ui.add(egui::TextEdit::singleline(&mut s.name).hint_text("Name").desired_width(280.0));
            if s.just_opened {
                edit.request_focus();
                s.just_opened = false;
            }
            if edit.changed() {
                s.taken = false;
                s.error = None;
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if let Some(e) = &s.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            ui.horizontal(|ui| {
                let label = if s.taken { "Replace" } else { "Save" };
                if ui.add_enabled(!s.name.trim().is_empty(), egui::Button::new(label)).clicked() || (enter && !s.name.trim().is_empty()) {
                    let g = templates::excerpt(&doc.graph, doc.path.as_deref(), &nodes, &backdrops);
                    match templates::save_in(&arclight::config::templates_dir(), &s.name, &g, s.taken) {
                        Ok(t) => {
                            note = Some(format!("Saved template {}", t.name));
                            done = true;
                        }
                        Err(e) => {
                            s.taken = templates::path_in(&arclight::config::templates_dir(), &s.name).is_ok_and(|p| p.exists());
                            s.error = Some(if s.taken { format!("{e:#}: Replace to overwrite it") } else { format!("{e:#}") });
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
        if !open || done || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.save = None;
        }
        note
    }

    fn manage_ui(&mut self, ctx: &egui::Context) {
        let Some(m) = self.manage.as_mut() else { return };
        let mut open = true;
        let dir = arclight::config::templates_dir();
        egui::Window::new("Templates").collapsible(false).open(&mut open).show(ctx, |ui| {
            let list = templates::list_in(&dir);
            if list.is_empty() {
                ui.weak("No templates of your own yet: select nodes, then Insert ▸ Save selection as template…");
            }
            egui::Grid::new("templates_grid").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                for t in &list {
                    match &mut m.renaming {
                        Some((path, name)) if *path == t.path => {
                            let edit = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("Rename").clicked() || enter {
                                match templates::rename(t, name) {
                                    Ok(_) => m.renaming = None,
                                    Err(e) => m.error = Some(format!("{e:#}")),
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                m.renaming = None;
                            }
                        }
                        _ => {
                            ui.label(&t.name).on_hover_text(t.path.display().to_string());
                            if ui.button("Rename…").clicked() {
                                m.renaming = Some((t.path.clone(), t.name.clone()));
                                m.deleting = None;
                                m.error = None;
                            }
                            let sure = m.deleting.as_ref() == Some(&t.path);
                            if ui.button(if sure { "Delete: sure?" } else { "Delete" }).clicked() {
                                if sure {
                                    if let Err(e) = templates::delete(t) {
                                        m.error = Some(format!("{e:#}"));
                                    }
                                    m.deleting = None;
                                } else {
                                    m.deleting = Some(t.path.clone());
                                }
                            }
                        }
                    }
                    ui.end_row();
                }
            });
            if let Some(e) = &m.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            ui.separator();
            if ui.button("Show the folder").on_hover_text(dir.display().to_string()).clicked() {
                if let Err(e) = reveal(&dir) {
                    m.error = Some(format!("Couldn't show {}: {e}", dir.display()));
                }
            }
        });
        if !open {
            self.manage = None;
        }
    }
}
