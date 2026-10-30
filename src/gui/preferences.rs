//! Edit ▸ Preferences (⌘,): the user's settings ([`Settings`], kept in
//! settings.json), a section at a time. Changes are a draft until Apply or
//! OK hands them to the app, which saves and applies them; Cancel drops
//! them, Reset to defaults puts every section back to how the app works
//! without settings.

use crate::dialogs;
use arclight::config::{FilePrefs, InterfacePrefs, NewProjectPrefs, Settings, View3dPrefs};
use eframe::egui;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Section {
    #[default]
    General,
    Interface,
    NewProject,
    Files,
    View3d,
}

const SECTIONS: [(Section, &str); 5] = [
    (Section::General, "General"),
    (Section::Interface, "Interface"),
    (Section::NewProject, "New project"),
    (Section::Files, "Files and safety"),
    (Section::View3d, "3D view"),
];

/// A folder a section asks to choose with the system's dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    /// One more plugin folder.
    Plugin,
    Project,
    Read,
    Write,
}

/// The window, and the settings being changed in it.
#[derive(Default)]
pub struct Preferences {
    pub open: bool,
    draft: Settings,
    section: Section,
    /// A folder dialog open for a section, if any.
    browse: Option<dialogs::Pending<Folder>>,
}

impl Preferences {
    /// Show the window, changing a copy of `now`.
    pub fn show(&mut self, now: &Settings) {
        self.open = true;
        self.draft = now.clone();
    }

    /// The window, if open. Some(settings) when Apply or OK asks for them
    /// to take effect.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Settings> {
        if !self.open {
            return None;
        }
        self.poll_browse(ctx);
        let (mut apply, mut close) = (false, false);
        let mut open = true;
        let mut wanted: Option<Folder> = None;
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                egui::Panel::bottom("preferences_buttons").show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Reset to defaults").on_hover_text("Every section back to how Arclight works without settings (Apply or OK to keep it)").clicked() {
                            self.draft = Settings { other: std::mem::take(&mut self.draft.other), ..Settings::default() };
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("OK").clicked() {
                                (apply, close) = (true, true);
                            }
                            if ui.button("Apply").clicked() {
                                apply = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                });
                egui::Panel::left("preferences_sections").resizable(false).default_size(150.0).show(ui, |ui| {
                    for (section, label) in SECTIONS {
                        if ui.selectable_label(self.section == section, label).clicked() {
                            self.section = section;
                        }
                    }
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let s = &mut self.draft;
                        match self.section {
                            Section::General => general(ui, s, &mut wanted),
                            Section::Interface => interface(ui, &mut s.interface),
                            Section::NewProject => new_project(ui, &mut s.new_project),
                            Section::Files => files(ui, &mut s.files, &mut wanted),
                            Section::View3d => view3d(ui, &mut s.view3d),
                        }
                    });
                });
            });
        if let Some(which) = wanted.filter(|_| self.browse.is_none()) {
            self.browse = dialogs::pick_folder(which, dialogs::new());
        }
        if !open || close {
            self.open = false;
        }
        apply.then(|| self.draft.clone())
    }

    /// A folder dialog's answer, once it has closed, into the draft.
    fn poll_browse(&mut self, ctx: &egui::Context) {
        let Some(answer) = self.browse.as_ref().and_then(|p| p.poll()) else {
            if self.browse.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let which = self.browse.take().map(|p| p.what);
        if let (Some(which), Some(dir)) = (which, answer) {
            set_folder(&mut self.draft, which, dir);
        }
    }
}

/// Put folder `dir`, chosen for `which`, into settings `s`.
fn set_folder(s: &mut Settings, which: Folder, dir: PathBuf) {
    match which {
        Folder::Plugin if !s.plugin_paths.contains(&dir) => s.plugin_paths.push(dir),
        Folder::Plugin => {}
        Folder::Project => s.files.project_dir = Some(dir),
        Folder::Read => s.files.read_dir = Some(dir),
        Folder::Write => s.files.write_dir = Some(dir),
    }
}

/// A heading and some space under it.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.heading(text);
    ui.add_space(6.0);
}

/// Plugin folders, the frame cache and the MCP address.
fn general(ui: &mut egui::Ui, s: &mut Settings, wanted: &mut Option<Folder>) {
    heading(ui, "General");
    ui.label("Plugin folders").on_hover_text("Searched for native plugins (after $ARCLIGHT_PLUGIN_PATH, before the standard places). Takes effect next time Arclight starts.");
    let mut gone = None;
    for (i, dir) in s.plugin_paths.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let mut text = dir.to_string_lossy().into_owned();
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(380.0)).changed() {
                *dir = PathBuf::from(text);
            }
            if ui.small_button("✕").on_hover_text("Remove this folder").clicked() {
                gone = Some(i);
            }
        });
    }
    if let Some(i) = gone {
        s.plugin_paths.remove(i);
    }
    if ui.add_enabled(!dialogs::busy(), egui::Button::new("Add folder…")).clicked() {
        *wanted = Some(Folder::Plugin);
    }
    ui.add_space(10.0);
    egui::Grid::new("general_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Frame cache");
        ui.add(egui::DragValue::new(&mut s.frame_cache_mb).range(64..=1_048_576).speed(16).suffix(" MB"))
            .on_hover_text("Memory for played frames, so playing and scrubbing back is instant. Takes effect next time Arclight starts.");
        ui.end_row();
        ui.label("MCP address");
        ui.add(egui::TextEdit::singleline(&mut s.mcp_http_addr).desired_width(200.0))
            .on_hover_text("Where `arclight mcp --http` listens when it isn't given one (host:port)");
        ui.end_row();
    });
}

/// Scale, theme and the node graph's grid and wires.
fn interface(ui: &mut egui::Ui, p: &mut InterfacePrefs) {
    heading(ui, "Interface");
    egui::Grid::new("interface_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Interface scale");
        let mut percent = (p.ui_scale * 100.0).round();
        if ui.add(egui::DragValue::new(&mut percent).range(50.0..=300.0).speed(1.0).suffix(" %")).on_hover_text("The size of everything, on top of the system's scale").changed() {
            p.ui_scale = percent / 100.0;
        }
        ui.end_row();
        ui.label("Theme");
        egui::ComboBox::from_id_salt("theme").selected_text(theme_label(&p.theme)).show_ui(ui, |ui| {
            for t in ["system", "dark", "light"] {
                ui.selectable_value(&mut p.theme, t.to_string(), theme_label(t));
            }
        });
        ui.end_row();
        ui.label("Node graph");
        ui.checkbox(&mut p.snap_to_grid, "Snap nodes to the grid").on_hover_text("Nodes you move land on the node graph's grid");
        ui.end_row();
        ui.label("Wires");
        ui.horizontal(|ui| {
            ui.radio_value(&mut p.wire_style, "curved".to_string(), "Curved");
            ui.radio_value(&mut p.wire_style, "straight".to_string(), "Straight");
        });
        ui.end_row();
    });
}

fn theme_label(theme: &str) -> &'static str {
    match theme {
        "dark" => "Dark",
        "light" => "Light",
        _ => "Follow the system",
    }
}

/// What File ▸ New starts with.
fn new_project(ui: &mut egui::Ui, p: &mut NewProjectPrefs) {
    heading(ui, "New project");
    let _ = p;
    ui.weak("Coming soon.");
}

/// Autosave, recent files, undo and where file dialogs start.
fn files(ui: &mut egui::Ui, p: &mut FilePrefs, wanted: &mut Option<Folder>) {
    heading(ui, "Files and safety");
    egui::Grid::new("files_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Autosave every");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut p.autosave_minutes).range(0..=120).suffix(" min"))
                .on_hover_text("Save a copy of unsaved changes this often: beside the project as <name>.autosave (the project itself is never overwritten), or in Arclight's state folder for a project not saved yet");
            ui.weak(if p.autosave_minutes == 0 { "(off)" } else { "" });
        });
        ui.end_row();
        ui.label("Recent projects");
        ui.add(egui::DragValue::new(&mut p.recent_count).range(0..=50)).on_hover_text("How many File ▸ Open Recent lists");
        ui.end_row();
        ui.label("Undo levels");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut p.undo_levels).range(0..=10_000)).on_hover_text("Steps of undo kept");
            ui.weak(if p.undo_levels == 0 { "(no limit)" } else { "" });
        });
        ui.end_row();
    });
    ui.add_space(10.0);
    ui.label("Where file dialogs start (empty: wherever the system likes)");
    ui.add_space(4.0);
    egui::Grid::new("folders_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        for (label, dir, which) in [("Projects", &mut p.project_dir, Folder::Project), ("Reading media", &mut p.read_dir, Folder::Read), ("Writing renders", &mut p.write_dir, Folder::Write)] {
            ui.label(label);
            folder_row(ui, dir, which, wanted);
            ui.end_row();
        }
    });
}

/// A folder setting: its path (editable), Choose… and Clear.
fn folder_row(ui: &mut egui::Ui, dir: &mut Option<PathBuf>, which: Folder, wanted: &mut Option<Folder>) {
    ui.horizontal(|ui| {
        let mut text = dir.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut text).desired_width(300.0)).changed() {
            *dir = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        }
        if ui.add_enabled(!dialogs::busy(), egui::Button::new("Choose…")).clicked() {
            *wanted = Some(which);
        }
        if ui.add_enabled(dir.is_some(), egui::Button::new("Clear")).clicked() {
            *dir = None;
        }
    });
}

/// How the 3D view takes the mouse and trackpad.
fn view3d(ui: &mut egui::Ui, p: &mut View3dPrefs) {
    heading(ui, "3D view");
    let _ = p;
    ui.weak("Coming soon.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_folders_land_in_their_settings() {
        let mut s = Settings::default();
        set_folder(&mut s, Folder::Plugin, "/plug".into());
        set_folder(&mut s, Folder::Plugin, "/plug".into());
        set_folder(&mut s, Folder::Project, "/proj".into());
        set_folder(&mut s, Folder::Read, "/plates".into());
        set_folder(&mut s, Folder::Write, "/renders".into());
        assert_eq!(s.plugin_paths, [PathBuf::from("/plug")], "once");
        assert_eq!((s.files.project_dir, s.files.read_dir, s.files.write_dir), (Some("/proj".into()), Some("/plates".into()), Some("/renders".into())));
    }
}
