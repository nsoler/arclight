//! Properties panels: one per opened node, stacked newest-first. Widgets are
//! generated from each op's [`ParamSpec`]s, plus extras for media nodes.

use crate::doc::Doc;
use crate::node_editor::category_color;
use arclight::anim::{self, Interp};
use arclight::media;
use arclight::ops::{self, ParamKind, ParamSpec};
use arclight::render::{self, Progress};
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

const MAX_OPEN: usize = 6;
const KEY_SIZE: f32 = 13.0;
pub const KEY_COLOR: Color32 = Color32::from_rgb(240, 175, 50);

struct RenderJob {
    progress: Arc<Progress>,
    handle: Option<JoinHandle<anyhow::Result<i64>>>,
    /// Outcome once finished: (succeeded, message).
    result: Option<(bool, String)>,
}

#[derive(Default)]
pub struct Properties {
    open: Vec<String>,
    last_selected: Option<String>,
    name_edit: HashMap<String, String>,
    name_error: Option<(String, String)>,
    jobs: HashMap<String, RenderJob>,
    media_info: HashMap<String, String>,
    /// The current frame (keyframes are read and written here).
    frame: i64,
    /// A frame the user asked to jump to (e.g. "next key").
    seek: Option<i64>,
}

impl Properties {
    pub fn is_rendering(&self) -> bool {
        self.jobs.values().any(|j| j.handle.is_some())
    }

    /// Forget cached per-node UI state; call after undo/redo replaces the graph.
    pub fn invalidate(&mut self) {
        self.name_edit.clear();
        self.media_info.clear();
        self.name_error = None;
    }

    /// Draw the panels at `frame`. Returns a frame to seek to, if requested.
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, frame: i64) -> Option<i64> {
        self.frame = frame;
        self.seek = None;
        self.show(ui, doc);
        self.seek.take()
    }

    fn show(&mut self, ui: &mut egui::Ui, doc: &mut Doc) {
        if doc.selected != self.last_selected {
            if let Some(sel) = &doc.selected {
                self.open.retain(|id| id != sel);
                self.open.insert(0, sel.clone());
                self.open.truncate(MAX_OPEN);
            }
            self.last_selected = doc.selected.clone();
        }
        self.open.retain(|id| doc.node(id).is_some());

        if self.open.is_empty() {
            ui.weak("Select a node to edit its properties.");
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for id in self.open.clone() {
                self.panel(ui, doc, &id);
                ui.add_space(6.0);
            }
        });
    }

    fn panel(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        let Some(node) = doc.node(id) else { return };
        let op = node.op.clone();
        let Some(info) = ops::info(&op) else {
            ui.colored_label(Color32::RED, format!("{id}: unknown op '{op}'"));
            return;
        };
        let selected = doc.selected.as_deref() == Some(id);
        let frame = egui::Frame::group(ui.style()).stroke(if selected {
            egui::Stroke::new(1.5, Color32::from_rgb(250, 190, 70))
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        });
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, category_color(info.category));
                ui.strong(id);
                ui.weak(info.label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        self.open.retain(|o| o != id);
                    }
                    if op != "viewer" && ui.small_button("View").on_hover_text("Show in the viewer (V)").clicked() {
                        doc.view_node(id);
                    }
                });
            });
            ui.label(RichText::new(info.help).small().weak());
            ui.separator();

            let mut rename: Option<String> = None;
            egui::Grid::new(("props", id)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Name");
                let buf = self.name_edit.entry(id.to_string()).or_insert_with(|| id.to_string());
                let resp = ui.text_edit_singleline(buf);
                if resp.lost_focus() && buf.as_str() != id {
                    rename = Some(buf.clone());
                }
                ui.end_row();

                for spec in info.params {
                    ui.horizontal(|ui| {
                        if spec.animatable {
                            self.key_button(ui, doc, id, spec.name);
                        } else {
                            ui.add_space(KEY_SIZE + ui.spacing().item_spacing.x);
                        }
                        let animated = anim::is_animated(&doc.param(id, spec.name));
                        if animated {
                            ui.colored_label(KEY_COLOR, spec.label);
                        } else {
                            ui.label(spec.label);
                        }
                    });
                    self.param_widget(ui, doc, id, &op, spec);
                    ui.end_row();
                }
            });
            if let Some((err_id, msg)) = &self.name_error {
                if err_id == id {
                    ui.colored_label(Color32::from_rgb(235, 100, 90), msg);
                }
            }

            match op.as_str() {
                "media_in" => self.media_in_extras(ui, doc, id),
                "media_out" => self.media_out_extras(ui, doc, id),
                _ => {}
            }

            if let Some(new) = rename {
                self.name_edit.remove(id);
                match doc.rename(id, &new) {
                    Ok(()) => {
                        let new = new.trim().to_string();
                        for o in &mut self.open {
                            if o == id {
                                *o = new.clone();
                            }
                        }
                        if let Some(job) = self.jobs.remove(id) {
                            self.jobs.insert(new.clone(), job);
                        }
                        self.last_selected = doc.selected.clone();
                        self.name_error = None;
                    }
                    Err(e) => self.name_error = Some((id.to_string(), e)),
                }
            }
        });
    }

    fn param_widget(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str, op: &str, spec: &ParamSpec) {
        let value = doc.param_at(id, spec.name, self.frame);
        let f = |v: &Value, i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let new: Option<Value> = match spec.kind {
            ParamKind::Float { min, max } => {
                let mut v = value.as_f64().unwrap_or(0.0) as f32;
                let resp = ui.add(egui::Slider::new(&mut v, min..=max).clamping(egui::SliderClamping::Never));
                resp.changed().then(|| json!(v))
            }
            ParamKind::Int { min, max } => {
                let mut v = value.as_i64().unwrap_or(0);
                ui.add(egui::DragValue::new(&mut v).range(min..=max)).changed().then(|| json!(v))
            }
            ParamKind::Vec2 | ParamKind::Vec3 => {
                let n = if matches!(spec.kind, ParamKind::Vec2) { 2 } else { 3 };
                let speed = if spec.name == "translate" { 1.0 } else { 0.005 };
                let mut v: Vec<f32> = (0..n).map(|i| f(&value, i)).collect();
                let mut changed = false;
                ui.horizontal(|ui| {
                    for (i, x) in v.iter_mut().enumerate() {
                        let prefix = ["x ", "y ", "z "][i];
                        let prefix = if n == 3 { ["r ", "g ", "b "][i] } else { prefix };
                        changed |= ui.add(egui::DragValue::new(x).speed(speed).prefix(prefix).max_decimals(4)).changed();
                    }
                });
                changed.then(|| json!(v))
            }
            ParamKind::Color => {
                let mut c = [f(&value, 0), f(&value, 1), f(&value, 2), f(&value, 3)];
                let mut changed = false;
                ui.horizontal(|ui| {
                    changed |= ui.color_edit_button_rgba_unmultiplied(&mut c).changed();
                    for (i, x) in c.iter_mut().enumerate() {
                        changed |= ui
                            .add(egui::DragValue::new(x).speed(0.005).prefix(["r ", "g ", "b ", "a "][i]).max_decimals(3))
                            .changed();
                    }
                });
                changed.then(|| json!(c))
            }
            ParamKind::Bool => {
                let mut b = value.as_bool().unwrap_or(false);
                ui.checkbox(&mut b, "").changed().then(|| json!(b))
            }
            ParamKind::Points { stride } => {
                // Points are edited in the viewer; show how many there are.
                let n = value.as_array().map_or(0, |a| a.len() / stride.max(1));
                ui.weak(format!("{n} points · edit in the viewer"));
                None
            }
            ParamKind::Choice(options) => {
                let current = value.as_str().unwrap_or("").to_string();
                let mut chosen = current.clone();
                egui::ComboBox::from_id_salt(("choice", id, spec.name))
                    .selected_text(&chosen)
                    .show_ui(ui, |ui| {
                        for o in options {
                            ui.selectable_value(&mut chosen, o.to_string(), *o);
                        }
                    });
                (chosen != current).then(|| json!(chosen))
            }
            ParamKind::Path { save } => {
                let mut s = value.as_str().unwrap_or("").to_string();
                let mut out = None;
                ui.horizontal(|ui| {
                    if ui.button("Browse…").clicked() {
                        if save {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_file_name("render.####.png")
                                .add_filter("Image sequence / video", &["png", "exr", "jpg", "tif", "mp4", "mov"])
                                .save_file()
                            {
                                out = Some(json!(p.to_string_lossy()));
                            }
                        } else if let Some(p) = rfd::FileDialog::new()
                            .add_filter(
                                "Images & video",
                                &["exr", "png", "jpg", "jpeg", "tif", "tiff", "mp4", "mov", "m4v", "mkv", "avi", "webm"],
                            )
                            .pick_file()
                        {
                            if op == "media_in" {
                                self.media_info.remove(id);
                                apply_media(doc, id, &p);
                            } else {
                                out = Some(json!(p.to_string_lossy()));
                            }
                        }
                    }
                    let resp = ui.add(egui::TextEdit::singleline(&mut s).desired_width(f32::INFINITY));
                    if resp.changed() {
                        self.media_info.remove(id);
                        out = Some(json!(s));
                    }
                });
                out
            }
        };
        if let Some(v) = new {
            if spec.animatable {
                doc.edit_param(id, spec.name, self.frame, v);
            } else {
                doc.set_param(id, spec.name, v);
            }
        }
    }

    /// The keyframe diamond: hollow grey = static, hollow orange = animated,
    /// filled orange = key on this frame. Click to set/remove a key;
    /// right-click for interpolation and key navigation.
    fn key_button(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str, name: &str) {
        let raw = doc.param(id, name);
        let animated = anim::is_animated(&raw);
        let on_key = anim::has_key_at(&raw, self.frame);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(KEY_SIZE, KEY_SIZE), egui::Sense::click());
        let c = rect.center();
        let r = KEY_SIZE * 0.42 * if resp.hovered() { 1.15 } else { 1.0 };
        let pts = vec![c + egui::vec2(0.0, -r), c + egui::vec2(r, 0.0), c + egui::vec2(0.0, r), c + egui::vec2(-r, 0.0)];
        let (fill, stroke) = match (animated, on_key) {
            (_, true) => (KEY_COLOR, KEY_COLOR),
            (true, false) => (Color32::TRANSPARENT, KEY_COLOR),
            _ => (Color32::TRANSPARENT, Color32::from_gray(120)),
        };
        ui.painter().add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::new(1.5, stroke)));

        let tip = if on_key {
            "Remove key at this frame (right-click for more)"
        } else {
            "Set key at this frame (right-click for more)"
        };
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            if on_key {
                doc.remove_key(id, name, self.frame);
            } else {
                doc.add_key(id, name, self.frame);
            }
        }
        resp.context_menu(|ui| {
            let keys = anim::key_frames(&raw);
            let prev = keys.iter().rev().find(|&&f| f < self.frame).copied();
            let next = keys.iter().find(|&&f| f > self.frame).copied();
            if ui.add_enabled(prev.is_some(), egui::Button::new("Previous key")).clicked() {
                self.seek = prev;
                ui.close();
            }
            if ui.add_enabled(next.is_some(), egui::Button::new("Next key")).clicked() {
                self.seek = next;
                ui.close();
            }
            ui.separator();
            let current = anim::interp(&raw);
            ui.add_enabled_ui(animated, |ui| {
                ui.label("Interpolation");
                for (interp, label) in [(Interp::Smooth, "Smooth"), (Interp::Linear, "Linear"), (Interp::Step, "Step")] {
                    if ui.radio(current == Some(interp), label).clicked() {
                        doc.set_interp(id, name, interp);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Delete animation").on_hover_text("Keep the current value, drop all keys").clicked() {
                    doc.clear_animation(id, name, self.frame);
                    ui.close();
                }
            });
        });
    }

    fn media_in_extras(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        let path = doc.param(id, "path").as_str().unwrap_or("").to_string();
        if path.is_empty() {
            return;
        }
        let info = self
            .media_info
            .entry(id.to_string())
            .or_insert_with(|| describe_media(&path))
            .clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.weak(info);
        });
        ui.horizontal(|ui| {
            if ui.button("Detect range").on_hover_text("Re-scan the file or sequence for its frame range").clicked() {
                self.media_info.remove(id);
                apply_media(doc, id, Path::new(&media::frame_path(&path, doc.param(id, "first_frame").as_i64().unwrap_or(1))));
            }
            if ui.button("Set project range").on_hover_text("Make the timeline match this clip").clicked() {
                let first = doc.param(id, "first_frame").as_i64().unwrap_or(1);
                let last = doc.param(id, "last_frame").as_i64().unwrap_or(first);
                doc.graph.project.first_frame = first;
                doc.graph.project.last_frame = last.max(first);
                if media::is_video(Path::new(&path)) {
                    if let Ok(v) = media::probe_video(Path::new(&path)) {
                        doc.graph.project.fps = v.fps;
                    }
                }
                doc.changed();
            }
        });
    }

    fn media_out_extras(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Use project range").clicked() {
                let p = doc.graph.project.clone();
                doc.set_param(id, "first_frame", json!(p.first_frame));
                doc.set_param(id, "last_frame", json!(p.last_frame));
                doc.set_param(id, "fps", json!(p.fps));
            }
        });

        // Collect a finished render.
        if let Some(job) = self.jobs.get_mut(id) {
            if job.handle.as_ref().is_some_and(|h| h.is_finished()) {
                let outcome = job.handle.take().unwrap().join();
                job.result = Some(match outcome {
                    Ok(Ok(n)) => (true, format!("Rendered {n} frame(s).")),
                    Ok(Err(e)) => (false, format!("{e:#}")),
                    Err(_) => (false, "render thread panicked".into()),
                });
            }
        }

        let running = self.jobs.get(id).is_some_and(|j| j.handle.is_some());
        if running {
            let job = &self.jobs[id];
            let p = &job.progress;
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(p.fraction())
                        .text(format!("{} / {}", p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed)))
                        .desired_width(ui.available_width() - 70.0),
                );
                if ui.button("Cancel").clicked() {
                    p.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            let ready = !doc.param(id, "path").as_str().unwrap_or("").is_empty()
                && doc.node(id).is_some_and(|n| n.inputs.first().is_some_and(|i| !i.is_empty()));
            let btn = ui.add_enabled(ready, egui::Button::new(RichText::new("Render").strong()));
            let btn = btn.on_disabled_hover_text("Connect an input and set an output file first");
            if btn.clicked() {
                let progress = Arc::new(Progress::default());
                let graph = doc.graph.clone();
                let (pg, node) = (progress.clone(), id.to_string());
                let handle = std::thread::spawn(move || render::render_media_out(&graph, &node, &pg));
                self.jobs.insert(id.to_string(), RenderJob { progress, handle: Some(handle), result: None });
            }
            if let Some((ok, msg)) = self.jobs.get(id).and_then(|j| j.result.as_ref()) {
                let color = if *ok { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(235, 100, 90) };
                ui.colored_label(color, msg);
            }
        }
    }
}

/// Point a Media In node at `file`: videos and sequences get their frame
/// range filled in. The first clip brought into a project also sets the
/// project's frame range and rate.
pub fn apply_media(doc: &mut Doc, id: &str, file: &Path) {
    let first_clip = !doc
        .graph
        .nodes
        .iter()
        .any(|n| n.op == "media_in" && n.id != id && n.params.get("path").and_then(Value::as_str).is_some_and(|p| !p.is_empty()));

    let mut fps = None;
    let (path, first, last) = if media::is_video(file) {
        match media::probe_video(file) {
            Ok(v) => {
                fps = Some(v.fps);
                (file.to_string_lossy().into_owned(), 1, v.frames as i64)
            }
            Err(_) => (file.to_string_lossy().into_owned(), 1, 1),
        }
    } else if let Some((pattern, a, b)) = media::sequence_from_file(file) {
        (pattern, a, b)
    } else {
        (file.to_string_lossy().into_owned(), 1, 1)
    };
    doc.set_param(id, "path", json!(path));
    doc.set_param(id, "first_frame", json!(first));
    doc.set_param(id, "last_frame", json!(last));
    if first_clip && last > first {
        doc.graph.project.first_frame = first;
        doc.graph.project.last_frame = last;
        if let Some(fps) = fps {
            doc.graph.project.fps = fps;
        }
        doc.changed();
    }
}

fn describe_media(path: &str) -> String {
    let p = Path::new(path);
    if media::is_video(p) {
        match media::probe_video(p) {
            Ok(v) => format!("Video · {}x{} · {:.3} fps · {} frames", v.width, v.height, v.fps, v.frames),
            Err(e) => format!("{e:#}"),
        }
    } else if media::is_sequence_pattern(path) {
        match media::detect_sequence_range(path) {
            Some((a, b)) => format!("Image sequence · frames {a}–{b} on disk"),
            None => "Image sequence · no matching files found".into(),
        }
    } else if p.exists() {
        "Still image".into()
    } else {
        "File not found".into()
    }
}
