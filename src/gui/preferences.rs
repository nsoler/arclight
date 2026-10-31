//! Edit ▸ Preferences (⌘,): the user's settings ([`Settings`], kept in
//! settings.json), a section at a time. Changes are a draft until Apply or
//! OK hands them to the app, which saves and applies them; Cancel drops
//! them, Reset to defaults puts every section back to how the app works
//! without settings.

use crate::dialogs;
use crate::gpu_check::StatusProbe;
use arclight::config::{FilePrefs, GpuPrefs, InterfacePrefs, NewProjectPrefs, Settings, View3dPrefs};
use arclight::gpu;
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
    Performance,
}

const SECTIONS: [(Section, &str); 6] = [
    (Section::General, "General"),
    (Section::Interface, "Interface"),
    (Section::NewProject, "New project"),
    (Section::Files, "Files and safety"),
    (Section::View3d, "3D view"),
    (Section::Performance, "Performance"),
];

/// A folder a section asks to choose with the system's dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    /// One more plugin folder.
    Plugin,
    /// The Projects folder.
    Projects,
}

/// The window, and the settings being changed in it.
#[derive(Default)]
pub struct Preferences {
    pub open: bool,
    draft: Settings,
    section: Section,
    /// A folder dialog open for a section, if any.
    browse: Option<dialogs::Pending<Folder>>,
    /// What GPU acceleration is doing now (as applied, not the draft).
    gpu_status: StatusProbe,
    /// Look at the GPU's status again on the next frame (opened, or
    /// settings just applied).
    look_again: bool,
}

impl Preferences {
    /// Show the window, changing a copy of `now`.
    pub fn show(&mut self, now: &Settings) {
        self.open = true;
        self.draft = now.clone();
        self.look_again = true;
    }

    /// The window, if open. Some(settings) when Apply or OK asks for them
    /// to take effect.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Settings> {
        if !self.open {
            return None;
        }
        self.poll_browse(ctx);
        if std::mem::take(&mut self.look_again) {
            self.gpu_status.refresh(ctx);
        }
        let gpu_line = self.gpu_status.line(ctx);
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
                        if ui.button("Reset to defaults").on_hover_text("Every section back to how ArclightVFX works without settings (Apply or OK to keep it)").clicked() {
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
                            Section::Performance => performance(ui, &mut s.gpu, &gpu_line),
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
        // The app applies them after this returns: look at the GPU then.
        self.look_again |= apply;
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
        Folder::Projects => s.files.project_dir = Some(dir),
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
    ui.label("Plugin folders").on_hover_text("Searched for native plugins (after $ARCLIGHT_PLUGIN_PATH, before the standard places). Takes effect next time ArclightVFX starts.");
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
            .on_hover_text("Memory for played frames, so playing and scrubbing back is instant. Takes effect next time ArclightVFX starts.");
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
    use crate::project_settings::{format_name, rate_name, FORMATS, RATES};
    heading(ui, "New project");
    ui.label("File ▸ New starts a project with these (Project Settings ▸ Save as defaults sets them too).");
    ui.add_space(6.0);
    egui::Grid::new("new_project_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Format");
        ui.horizontal(|ui| {
            let size = arclight::image::Size { width: p.width, height: p.height };
            let name = format_name(size).unwrap_or("Custom");
            egui::ComboBox::from_id_salt("new_format").selected_text(name).width(160.0).show_ui(ui, |ui| {
                for (n, w, h) in FORMATS {
                    if ui.selectable_label(*n == name, format!("{n}  ·  {w} × {h}")).clicked() {
                        (p.width, p.height) = (*w, *h);
                    }
                }
            });
            ui.add(egui::DragValue::new(&mut p.width).range(1..=16384).suffix(" w"));
            ui.label("×");
            ui.add(egui::DragValue::new(&mut p.height).range(1..=16384).suffix(" h"));
        });
        ui.end_row();
        ui.label("Frame rate");
        ui.horizontal(|ui| {
            let name = rate_name(p.fps).unwrap_or("Custom");
            egui::ComboBox::from_id_salt("new_rate").selected_text(name).width(90.0).show_ui(ui, |ui| {
                for (n, f) in RATES {
                    if ui.selectable_label(*n == name, *n).clicked() {
                        p.fps = *f;
                    }
                }
            });
            ui.add(egui::DragValue::new(&mut p.fps).range(0.1..=240.0).speed(0.1).max_decimals(3).suffix(" fps"));
        });
        ui.end_row();
        ui.label("Frames");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut p.first_frame).prefix("first "));
            ui.add(egui::DragValue::new(&mut p.last_frame).prefix("last "));
            p.last_frame = p.last_frame.max(p.first_frame);
        });
        ui.end_row();
    });
}

/// Autosave, recent files, undo and the Projects folder.
fn files(ui: &mut egui::Ui, p: &mut FilePrefs, wanted: &mut Option<Folder>) {
    heading(ui, "Files and safety");
    egui::Grid::new("files_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Autosave every");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut p.autosave_minutes).range(0..=120).suffix(" min"))
                .on_hover_text("Save a copy of unsaved changes this often: beside the project as <name>.autosave (the project itself is never overwritten), or in ArclightVFX's state folder for a project not saved yet");
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
    egui::Grid::new("folders_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Projects folder").on_hover_text("Where File ▸ New puts new projects' folders, and where Open starts. A project's own folder (File ▸ Project Settings) is where its media dialogs and renders start.");
        folder_row(ui, &mut p.project_dir, Folder::Projects, wanted);
        ui.end_row();
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
    egui::Grid::new("view3d_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Mouse");
        ui.checkbox(&mut p.emulate_three_button, "Emulate 3-button mouse").on_hover_text("Alt + left-drag orbits, pans (⇧) and zooms (⌃), as the middle button does");
        ui.end_row();
        ui.label("Orbit");
        ui.checkbox(&mut p.orbit_selection, "Around the selection").on_hover_text("Orbiting turns about the selected objects, so they stay in place on screen; else about the middle of the view");
        ui.end_row();
        ui.label("Two-finger scroll");
        ui.horizontal(|ui| {
            ui.radio_value(&mut p.trackpad, "orbit".to_string(), "Orbits").on_hover_text("⇧ pans");
            ui.radio_value(&mut p.trackpad, "pan".to_string(), "Pans").on_hover_text("⇧ orbits");
        });
        ui.end_row();
        ui.label("Zoom");
        ui.vertical(|ui| {
            ui.checkbox(&mut p.zoom_to_pointer, "Toward the pointer").on_hover_text("Else toward the middle of the view");
            ui.checkbox(&mut p.invert_zoom, "Invert").on_hover_text("The wheel, ⌃ + scroll and ⌃ + middle-drag zoom the other way (pinching doesn't change)");
        });
        ui.end_row();
        ui.label("Shading");
        egui::ComboBox::from_id_salt("default_shading").selected_text(shading_label(&p.default_shading)).show_ui(ui, |ui| {
            for t in ["auto", "solid", "lit"] {
                ui.selectable_value(&mut p.default_shading, t.to_string(), shading_label(t));
            }
        })
        .response
        .on_hover_text("How projects without their own 3D view start. Automatic: as you last chose, else Lit when the scene has lights");
        ui.end_row();
    });
}

/// GPU acceleration: the switch and what's in use now (`status`). Greyed
/// out when `ARCLIGHT_GPU=0` decides instead.
fn performance(ui: &mut egui::Ui, p: &mut GpuPrefs, status: &str) {
    heading(ui, "Performance");
    let forced = gpu::forced_off();
    ui.add_enabled(!forced, egui::Checkbox::new(&mut p.enabled, "GPU acceleration"))
        .on_hover_text("Render on the graphics card where ArclightVFX can (everything else, and any GPU error, falls back to the CPU). Takes effect when applied, no restart.")
        .on_disabled_hover_text("ARCLIGHT_GPU=0 is set in the environment, so everything renders on the CPU");
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.weak(if forced { "Using the CPU: ARCLIGHT_GPU=0 is set in the environment" } else { status });
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("GPU frame cache");
        ui.add(egui::DragValue::new(&mut p.frame_cache_mb).range(0..=1_048_576).speed(16).suffix(" MB"))
            .on_hover_text("Graphics memory for played frames rendered on the GPU, so playing them back draws straight from there (0: none; frames past it play from the CPU's frame cache). Apart from the GPU's own cache. Takes effect next time ArclightVFX starts.");
    });
    ui.add_space(8.0);
    ui.weak("Help ▸ GPU self-test compares the GPU with the CPU on this computer.");
}

fn shading_label(shading: &str) -> &'static str {
    match shading {
        "solid" => "Solid",
        "lit" => "Lit",
        _ => "Automatic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_folders_land_in_their_settings() {
        let mut s = Settings::default();
        set_folder(&mut s, Folder::Plugin, "/plug".into());
        set_folder(&mut s, Folder::Plugin, "/plug".into());
        set_folder(&mut s, Folder::Projects, "/shows".into());
        assert_eq!(s.plugin_paths, [PathBuf::from("/plug")], "once");
        assert_eq!(s.files.project_dir, Some("/shows".into()));
    }
}
