//! The Project Settings window (File ▸ Project Settings…, ⇧⌘P): the
//! project's frame size, frame rate and frame range, picked from presets or
//! typed, or matched to a selected Media In or Read. It edits a copy: Apply
//! (or OK) puts it in the project as one undo step, Cancel drops it. The
//! transport bar's fields edit the same values directly.

use crate::doc::Doc;
use anyhow::{bail, Context, Result};
use arclight::graph::Project;
use arclight::image::{Image, Size};
use arclight::media;
use eframe::egui;
use serde_json::Value;
use std::path::Path;

/// Frame sizes offered, with their names.
pub const FORMATS: &[(&str, usize, usize)] = &[
    ("HD 1080", 1920, 1080),
    ("UHD 4K", 3840, 2160),
    ("2K DCI", 2048, 1080),
    ("4K DCI", 4096, 2160),
    ("HD 720", 1280, 720),
    ("2K Full Aperture", 2048, 1556),
    ("PAL", 720, 576),
    ("NTSC", 720, 486),
    ("Square 1080", 1080, 1080),
    ("Vertical 1080", 1080, 1920),
];

/// Frame rates offered (the NTSC ones exactly 1000/1001 of the round rate).
pub const RATES: &[(&str, f64)] = &[
    ("23.976", 24000.0 / 1001.0),
    ("24", 24.0),
    ("25", 25.0),
    ("29.97", 30000.0 / 1001.0),
    ("30", 30.0),
    ("48", 48.0),
    ("50", 50.0),
    ("59.94", 60000.0 / 1001.0),
    ("60", 60.0),
];

/// The preset named for frame size `s`, if it is one.
pub fn format_name(s: Size) -> Option<&'static str> {
    FORMATS.iter().find(|f| (f.1, f.2) == (s.width, s.height)).map(|f| f.0)
}

/// The preset for frame rate `fps` (within a thousandth, so a typed
/// 23.976 counts as 24000/1001), if it is one.
pub fn rate_name(fps: f64) -> Option<&'static str> {
    RATES.iter().find(|r| (r.1 - fps).abs() < 1e-3).map(|r| r.0)
}

/// What a media node's source is: its frame size as stored in the file
/// (before any fitting to the project), and for a clip its frame rate (a
/// video's) and the frames it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub size: Size,
    pub fps: Option<f64>,
    pub range: Option<(i64, i64)>,
}

/// The source of a Media In (`media_in`) or Read (`read`) node with `params`,
/// read from its file.
pub fn probe(op: &str, params: &Value) -> Result<Source> {
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    if path.is_empty() {
        bail!("it has no file yet");
    }
    let frame = |k: &str| params.get(k).and_then(Value::as_i64);
    match op {
        "media_in" => {
            let (first, last) = (frame("first_frame").unwrap_or(1), frame("last_frame").unwrap_or(1));
            let range = Some((first, last.max(first)));
            if media::is_video(Path::new(path)) {
                let v = media::probe_video(Path::new(path))?;
                return Ok(Source { size: Size { width: v.width, height: v.height }, fps: Some(v.fps), range });
            }
            let file = if media::is_sequence_pattern(path) { media::existing_frame_path(path, first, first, last) } else { path.into() };
            Ok(Source { size: image_size(&file)?, fps: None, range })
        }
        // A Read of a video plays the whole clip from frame 1.
        "read" if media::is_video(Path::new(path)) => {
            let v = media::probe_video(Path::new(path))?;
            Ok(Source { size: Size { width: v.width, height: v.height }, fps: Some(v.fps), range: Some((1, v.frames as i64)) })
        }
        "read" => Ok(Source { size: image_size(Path::new(path))?, fps: None, range: None }),
        _ => bail!("Match media takes a Media In or Read node"),
    }
}

/// An image file's frame size (an EXR's display window).
fn image_size(path: &Path) -> Result<Size> {
    Ok(Image::read(path).with_context(|| format!("can't read {}", path.display()))?.format)
}

/// `project` taking `src`'s size, and its frame rate and range where it has
/// them.
pub fn matched(project: &Project, src: &Source) -> Project {
    let mut p = project.clone();
    p.format = src.size;
    if let Some(fps) = src.fps.filter(|f| *f > 0.0) {
        p.fps = fps;
    }
    if let Some((a, b)) = src.range {
        (p.first_frame, p.last_frame) = (a, b.max(a));
    }
    p
}

/// The window's state: open or not, and the settings being edited.
#[derive(Default)]
pub struct ProjectSettings {
    /// The copy being edited, while the window is open.
    draft: Option<Project>,
    /// What Match media did, or why it couldn't.
    message: Option<(bool, String)>,
    /// Settings to save as the defaults for new projects (see
    /// [`take_defaults`](Self::take_defaults)).
    defaults: Option<Project>,
}

impl ProjectSettings {
    /// Open it on the project's current settings.
    pub fn open(&mut self, doc: &Doc) {
        self.draft = Some(doc.graph.project.clone());
        self.message = None;
    }

    /// Close, dropping any edits.
    pub fn cancel(&mut self) {
        self.draft = None;
        self.message = None;
    }

    /// Put the edits in the project (one undo step, if anything changed).
    pub fn apply(&mut self, doc: &mut Doc) {
        if let Some(d) = &self.draft {
            let mut d = d.clone();
            d.last_frame = d.last_frame.max(d.first_frame);
            if d != doc.graph.project {
                doc.graph.project = d;
                doc.changed();
            }
        }
    }

    /// The settings "Save as defaults" asked to keep for new projects
    /// (Preferences ▸ New project), once.
    pub fn take_defaults(&mut self) -> Option<Project> {
        self.defaults.take()
    }

    /// Ask for the settings being edited to be the defaults for new
    /// projects (the frame range tidied as Apply does).
    fn save_defaults(&mut self) {
        if let Some(d) = &self.draft {
            let mut d = d.clone();
            d.last_frame = d.last_frame.max(d.first_frame);
            self.defaults = Some(d);
            self.message = Some((true, "New projects will start with these settings.".into()));
        }
    }

    /// Take the size, rate and range from node `id` (a Media In or Read).
    pub fn match_media(&mut self, doc: &Doc, id: &str) {
        let Some(draft) = &mut self.draft else { return };
        let Some(node) = doc.node(id) else { return };
        self.message = Some(match probe(&node.op, &node.params) {
            Ok(src) => {
                *draft = matched(draft, &src);
                let mut what = format!("{} × {}", src.size.width, src.size.height);
                if let Some(f) = src.fps {
                    what += &format!(", {} fps", rate_name(f).map_or_else(|| format!("{f:.3}"), str::to_string));
                }
                if let Some((a, b)) = src.range {
                    what += &format!(", frames {a}–{b}");
                }
                (true, format!("Took {what} from {id}."))
            }
            Err(e) => (false, format!("Can't match {id}: {e:#}")),
        });
    }

    /// The window, when open.
    pub fn ui(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let Some(mut d) = self.draft.clone() else { return };
        let mut keep_open = true;
        let (mut apply, mut close) = (false, false);
        let media = doc.selected.clone().filter(|id| doc.node(id).is_some_and(|n| matches!(n.op.as_str(), "media_in" | "read")));
        let mut match_from = None;
        let mut defaults = false;
        let window = egui::Window::new("Project Settings").open(&mut keep_open).collapsible(false).resizable(false);
        // In the middle of the window the first time it opens.
        let window = window.pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center());
        window.show(ctx, |ui| {
            egui::Grid::new("project_settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Format");
                ui.vertical(|ui| {
                    let name = format_name(d.format).unwrap_or("Custom");
                    egui::ComboBox::from_id_salt("format_preset").selected_text(name).width(220.0).show_ui(ui, |ui| {
                        for (n, w, h) in FORMATS {
                            if ui.selectable_label(*n == name, format!("{n}  ·  {w} × {h}")).clicked() {
                                d.format = Size { width: *w, height: *h };
                            }
                        }
                        let _ = ui.selectable_label(name == "Custom", "Custom (type the size)");
                    });
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.format.width).range(1..=16384).suffix(" w"));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut d.format.height).range(1..=16384).suffix(" h"));
                        if ui.button("⇄").on_hover_text("Swap width and height").clicked() {
                            d.format = Size { width: d.format.height, height: d.format.width };
                        }
                    });
                });
                ui.end_row();
                ui.label("Frame rate");
                ui.horizontal(|ui| {
                    let name = rate_name(d.fps).unwrap_or("Custom");
                    egui::ComboBox::from_id_salt("rate_preset").selected_text(name).width(90.0).show_ui(ui, |ui| {
                        for (n, f) in RATES {
                            if ui.selectable_label(*n == name, *n).clicked() {
                                d.fps = *f;
                            }
                        }
                        let _ = ui.selectable_label(name == "Custom", "Custom");
                    });
                    ui.add(egui::DragValue::new(&mut d.fps).range(0.1..=240.0).speed(0.1).max_decimals(3).suffix(" fps"));
                });
                ui.end_row();
                ui.label("Frames");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.first_frame).prefix("first "));
                    ui.add(egui::DragValue::new(&mut d.last_frame).prefix("last "));
                    d.last_frame = d.last_frame.max(d.first_frame);
                    let n = d.last_frame - d.first_frame + 1;
                    ui.weak(format!("{n} frame{} · {:.2} s", if n == 1 { "" } else { "s" }, n as f64 / d.fps.max(0.1)));
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let b = ui.add_enabled(media.is_some(), egui::Button::new("Match media"));
                let b = b.on_hover_text("Take the frame size (as in the file), frame rate and frame range from the selected Media In or Read").on_disabled_hover_text("Select a Media In or Read node first");
                if b.clicked() {
                    match_from = media.clone();
                }
                if let Some(id) = &media {
                    ui.weak(format!("from {id}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    defaults = ui.button("Save as defaults").on_hover_text("Start new projects with these settings (Preferences ▸ New project)").clicked();
                });
            });
            if let Some((ok, msg)) = &self.message {
                let color = if *ok { egui::Color32::from_rgb(110, 200, 120) } else { egui::Color32::from_rgb(235, 100, 90) };
                ui.colored_label(color, msg);
            }
            ui.separator();
            ui.horizontal(|ui| {
                close = ui.button("Cancel").clicked();
                apply = ui.button("Apply").clicked();
                if ui.button("OK").clicked() {
                    apply = true;
                    close = true;
                }
            });
        });
        self.draft = Some(d);
        if let Some(id) = match_from {
            self.match_media(doc, &id);
        }
        if defaults {
            self.save_defaults();
        }
        if apply {
            self.apply(doc);
        }
        if close || !keep_open {
            self.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;
    use serde_json::json;

    #[test]
    fn presets_name_sizes_and_rates() {
        assert_eq!(format_name(Size { width: 1920, height: 1080 }), Some("HD 1080"));
        assert_eq!(format_name(Size { width: 1080, height: 1920 }), Some("Vertical 1080"));
        assert_eq!(format_name(Size { width: 1000, height: 500 }), None);
        assert!(FORMATS.iter().all(|f| format_name(Size { width: f.1, height: f.2 }) == Some(f.0)), "no two presets share a size");
        assert_eq!((rate_name(24000.0 / 1001.0), rate_name(23.976), rate_name(25.0), rate_name(23.0)), (Some("23.976"), Some("23.976"), Some("25"), None));
        assert!((RATES.iter().find(|r| r.0 == "59.94").unwrap().1 - 59.94).abs() < 1e-2);
    }

    #[test]
    fn matching_takes_what_the_source_has() {
        let project = Project { fps: 25.0, ..Default::default() };
        let clip = Source { size: Size { width: 3840, height: 2160 }, fps: Some(24000.0 / 1001.0), range: Some((1001, 1048)) };
        let m = matched(&project, &clip);
        assert_eq!((m.first_frame, m.last_frame, m.fps, m.format), (1001, 1048, 24000.0 / 1001.0, clip.size));
        // A still: just its size.
        let still = Source { size: Size { width: 640, height: 480 }, fps: None, range: None };
        assert_eq!(matched(&project, &still), Project { format: still.size, ..project.clone() });
    }

    #[test]
    fn match_media_reads_the_source_size_from_its_file() {
        let dir = std::env::temp_dir().join(format!("arclight_project_settings_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A sequence of 64 × 48 frames 5–7, and a 32 × 20 still.
        for f in 5..=7 {
            Image::new(64, 48).write(&dir.join(format!("plate.{f:04}.exr"))).unwrap();
        }
        let still = dir.join("logo.png");
        Image::new(32, 20).write(&still).unwrap();
        let pattern = dir.join("plate.####.exr").to_string_lossy().into_owned();
        let seq = probe("media_in", &json!({ "path": pattern, "first_frame": 5, "last_frame": 7 })).unwrap();
        assert_eq!(seq, Source { size: Size { width: 64, height: 48 }, fps: None, range: Some((5, 7)) });
        let read = probe("read", &json!({ "path": still.to_string_lossy() })).unwrap();
        assert_eq!((read.size, read.range), (Size { width: 32, height: 20 }, None));
        assert!(probe("read", &json!({ "path": "" })).unwrap_err().to_string().contains("no file"));
        // A video through a Read: its size, rate and frames, like Media In's.
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/media/testsrc.mp4");
        let v = media::probe_video(&clip).unwrap();
        let read = probe("read", &json!({ "path": clip.to_string_lossy() })).unwrap();
        assert_eq!(read, Source { size: Size { width: v.width, height: v.height }, fps: Some(v.fps), range: Some((1, v.frames as i64)) });
        assert!(probe("grade", &json!({ "path": "x" })).is_err());

        // Through the window: Match media changes the copy only; Cancel
        // drops it; Apply is one undo step.
        let mut doc = Doc::new(Graph::default(), None);
        let id = doc.add_node("media_in", [0.0, 0.0]);
        doc.set_param(&id, "path", json!(pattern));
        doc.set_param(&id, "first_frame", json!(5));
        doc.set_param(&id, "last_frame", json!(7));
        doc.commit(false);
        let before = doc.graph.project.clone();
        let mut w = ProjectSettings::default();
        assert!(w.draft.is_none());
        w.open(&doc);
        w.match_media(&doc, &id);
        assert!(w.message.as_ref().is_some_and(|(ok, m)| *ok && m.contains("64 × 48") && m.contains("frames 5–7")), "{:?}", w.message);
        assert_eq!(doc.graph.project, before, "nothing changes until Apply");
        w.cancel();
        assert!(w.draft.is_none());
        assert_eq!(doc.graph.project, before);
        w.open(&doc);
        w.match_media(&doc, &id);
        w.apply(&mut doc);
        doc.commit(false);
        let after = doc.graph.project.clone();
        assert_eq!((after.format, after.first_frame, after.last_frame), (Size { width: 64, height: 48 }, 5, 7));
        assert!(w.draft.is_some(), "Apply keeps it open");
        doc.undo();
        assert_eq!(doc.graph.project, before, "one undo step");
        // Applying nothing new adds no step.
        let rev = doc.revision;
        w.open(&doc);
        w.apply(&mut doc);
        assert_eq!(doc.revision, rev);
        // A broken source says why.
        doc.set_param(&id, "path", json!(dir.join("missing.####.exr").to_string_lossy()));
        w.match_media(&doc, &id);
        assert!(w.message.as_ref().is_some_and(|(ok, m)| !ok && m.contains("Can't match")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_as_defaults_keeps_the_settings_being_edited_once() {
        let mut doc = Doc::new(Graph::default(), None);
        let mut w = ProjectSettings::default();
        w.save_defaults();
        assert_eq!(w.take_defaults(), None, "not open");
        w.open(&doc);
        if let Some(d) = &mut w.draft {
            (d.fps, d.first_frame, d.last_frame) = (25.0, 10, 5);
        }
        w.save_defaults();
        let kept = w.take_defaults().unwrap();
        assert_eq!((kept.fps, kept.first_frame, kept.last_frame), (25.0, 10, 10), "tidied as Apply does");
        assert_eq!(w.take_defaults(), None, "once");
        assert_eq!(doc.graph.project, Project::default(), "the project itself is unchanged");
        w.apply(&mut doc);
        assert_eq!(doc.graph.project.fps, 25.0);
    }
}
