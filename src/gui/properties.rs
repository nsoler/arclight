//! Properties panels: one per opened node, stacked newest-first. Widgets are
//! generated from each op's [`ParamSpec`]s, plus extras for media nodes.

use crate::dialogs;
use crate::doc::{Doc, NODE_H, NODE_W};
use crate::node_editor::category_color;
use arclight::anim::{self, Interp};
use arclight::media;
use arclight::ops::{self, ParamKind, ParamSpec};
use arclight::render::{self, Progress};
use arclight::track;
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
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

/// Positions found so far by a tracking run, per frame.
/// What a tracking run found on one frame: positions, match scores, and
/// warps when the run follows rotation.
struct Tracked {
    frame: i64,
    pos: Vec<Option<track::Pt>>,
    scores: Vec<f32>,
    warps: Option<Vec<track::Warp>>,
}

type Found = Arc<Mutex<Vec<Tracked>>>;

struct TrackJob {
    progress: Arc<Progress>,
    handle: Option<JoinHandle<anyhow::Result<track::Outcome>>>,
    found: Found,
    /// Outcome once finished: (succeeded, message).
    result: Option<(bool, String)>,
}

/// What the file browser offers to read: image formats (one file or a
/// numbered sequence) and video.
const READ_EXTS: &[&str] = &[
    "exr", "dpx", "tif", "tiff", "tga", "png", "jpg", "jpeg", "webp", "hdr", "bmp",
    "mp4", "mov", "m4v", "mkv", "avi", "webm", "mxf", "mpg", "mpeg",
];
/// What Read Geo's file browser offers.
const GEO_EXTS: &[&str] = &["obj", "gltf", "glb"];
/// …and to write.
const WRITE_EXTS: &[&str] = &["exr", "dpx", "tif", "tiff", "tga", "png", "jpg", "webp", "mp4", "mov"];

/// A layer's name and its planes' names.
type Layer = (String, Vec<String>);

/// Names offered for text param `name` of `op`: a Shuffle's layers (In,
/// Out, Remove) or channels (Red … Alpha from). With its input's `layers`
/// known (as [`Image::layers`](arclight::image::Image::layers) lists them),
/// those (Out also offers the usual pass names, to write a new one); else
/// the names render passes use.
fn text_suggestions(op: &str, name: &str, layers: Option<&[Layer]>) -> Vec<String> {
    const LAYERS: &[&str] = &["rgba", "P", "N", "uv", "motion", "depth", "id", "diffuse", "specular", "emission", "ambient"];
    const REMOVE: &[&str] = &["*", "P", "N", "uv", "motion", "depth", "id", "diffuse", "specular", "emission", "ambient"];
    const CHANNELS: &[&str] = &["R", "G", "B", "A", "0", "1", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "uv.u", "uv.v", "motion.u", "motion.v", "depth", "id"];
    if op != "shuffle" {
        return Vec::new();
    }
    let strings = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let Some(layers) = layers else {
        return strings(match name {
            "in" | "out" => LAYERS,
            "remove" => REMOVE,
            _ => CHANNELS,
        });
    };
    let mut out: Vec<String> = match name {
        "in" | "out" => std::iter::once("rgba".to_string()).chain(layers.iter().map(|l| l.0.clone())).collect(),
        "remove" => std::iter::once("*".to_string()).chain(layers.iter().map(|l| l.0.clone())).collect(),
        _ => strings(&CHANNELS[..6]).into_iter().chain(layers.iter().flat_map(|l| l.1.iter().cloned())).collect(),
    };
    if name == "out" {
        for l in &LAYERS[1..] {
            if !out.iter().any(|o| o == l) {
                out.push(l.to_string());
            }
        }
    }
    out
}

#[derive(Default)]
pub struct Properties {
    open: Vec<String>,
    last_selected: Option<String>,
    name_edit: HashMap<String, String>,
    name_error: Option<(String, String)>,
    jobs: HashMap<String, RenderJob>,
    tracks: HashMap<String, TrackJob>,
    /// Camera Tracker solves, running or done, by node.
    camera_jobs: HashMap<String, crate::camera_tracker::CamJob>,
    media_info: HashMap<String, String>,
    /// The current frame (keyframes are read and written here).
    frame: i64,
    /// A frame the user asked to jump to (e.g. "next key").
    seek: Option<i64>,
    /// The selected node (a Transform, Corner Pin, …) and its input's data
    /// window and frame: they place a Transform's automatic center and a
    /// Corner Pin's image corners.
    pub layer: Option<(String, arclight::image::IRect, arclight::image::Size)>,
    /// The selected node (a Shuffle) and its input's layers, for the names
    /// its fields offer.
    pub input_layers: Option<(String, Vec<Layer>)>,
    /// The Tracker panel's exports copy the keys instead of linking.
    copy_exports: bool,
    /// Formulas being typed, by node and param (applied on Enter or when
    /// the field loses focus, if they parse).
    formula_edit: HashMap<(String, String), String>,
    /// A Browse… file dialog that's open, and the param it's for.
    browse: Option<dialogs::Pending<Browse>>,
    /// Text params being typed (a Shuffle's plane names), by node and param:
    /// applied on Enter or when the field loses focus, so a half-typed name
    /// isn't evaluated.
    text_edit: HashMap<(String, String), String>,
    /// The highlighted completion under the formula being typed.
    formula_pick: usize,
    /// Where the completion list was drawn last frame: a click there
    /// mustn't count as leaving the field.
    formula_popup: Option<egui::Rect>,
}

impl Properties {
    pub fn is_rendering(&self) -> bool {
        self.jobs.values().any(|j| j.handle.is_some())
    }

    /// A tracking run is writing keys (the edits form one undo step).
    pub fn is_tracking(&self) -> bool {
        self.tracks.values().any(|j| j.handle.is_some())
    }

    /// Forget cached per-node UI state; call after undo/redo replaces the graph.
    pub fn invalidate(&mut self) {
        self.name_edit.clear();
        self.formula_edit.clear();
        self.text_edit.clear();
        self.media_info.clear();
        self.name_error = None;
    }

    /// Draw the panels at `frame`. Returns a frame to seek to, if requested.
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, frame: i64) -> Option<i64> {
        self.frame = frame;
        self.seek = None;
        self.poll_browse(ui.ctx(), doc);
        self.collect_tracks(doc);
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
        let backdrop = doc.selected_backdrop.clone().filter(|b| doc.graph.backdrop(b).is_some());

        if self.open.is_empty() && backdrop.is_none() {
            ui.weak("Select a node to edit its properties.");
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if let Some(b) = &backdrop {
                self.backdrop_panel(ui, doc, b);
                ui.add_space(6.0);
            }
            for id in self.open.clone() {
                self.panel(ui, doc, &id);
                ui.add_space(6.0);
            }
        });
    }

    /// A selected backdrop: its title, color and title size, and Delete
    /// box (its nodes stay).
    fn backdrop_panel(&mut self, ui: &mut egui::Ui, doc: &mut Doc, bid: &str) {
        let Some(mut b) = doc.graph.backdrop(bid).cloned() else { return };
        let before = b.clone();
        let frame = egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.5, Color32::from_rgb(250, 190, 70)));
        let mut delete = false;
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, backdrop_color32(b.color));
                ui.strong(if b.title.is_empty() { bid } else { b.title.as_str() });
                ui.weak("Backdrop");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    delete = ui.small_button("Delete box").on_hover_text("Remove the box (Ungroup); the nodes in it stay").clicked();
                });
            });
            ui.label(RichText::new("A group box behind nodes. Drag its title bar to move it with the nodes fully inside it; drag a corner to resize.").small().weak());
            ui.separator();
            egui::Grid::new(("backdrop", bid)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut b.title).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Color");
                backdrop_palette(ui, &mut b.color);
                ui.end_row();
                ui.label("Title size");
                ui.add(egui::DragValue::new(&mut b.font_size).range(8.0..=72.0).speed(0.2).suffix(" px"));
                ui.end_row();
            });
        });
        if delete {
            doc.delete_backdrop(bid);
        } else if b != before {
            doc.set_backdrop(b);
        }
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
                    let linked = arclight::link::is_link(&doc.param(id, spec.name));
                    ui.horizontal(|ui| {
                        if spec.animatable && !linked {
                            self.key_button(ui, doc, id, spec.name);
                        } else {
                            ui.add_space(KEY_SIZE + ui.spacing().item_spacing.x);
                        }
                        let animated = anim::is_animated(&doc.param(id, spec.name));
                        let label = if animated { ui.colored_label(KEY_COLOR, spec.label) } else { ui.label(spec.label) };
                        if !linked {
                            label.context_menu(|ui| self.formula_menu(ui, doc, id, spec.name));
                        }
                    });
                    if linked {
                        self.linked_widget(ui, doc, id, spec);
                    } else {
                        self.param_widget(ui, doc, id, &op, spec);
                    }
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
                "tracker" => self.tracker_extras(ui, doc, id),
                "camera_tracker" => crate::camera_tracker::panel(ui, doc, id, &mut self.camera_jobs),
                "corner_pin" => self.corner_pin_extras(ui, doc, id),
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
                        if let Some(job) = self.tracks.remove(id) {
                            self.tracks.insert(new.clone(), job);
                        }
                        self.last_selected = doc.selected.clone();
                        self.name_error = None;
                    }
                    Err(e) => self.name_error = Some((id.to_string(), e)),
                }
            }
        });
    }

    /// "Use a formula": turns a numeric param into a formula link
    /// (`arclight::expr`) that starts as its value at this frame.
    fn formula_menu(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str, name: &str) {
        let now = doc.param_at(id, name, self.frame);
        let start = match &now {
            Value::Number(n) => n.as_f64().map(|x| format!("{x}")),
            Value::Array(a) => a.iter().map(|v| v.as_f64().map(|x| format!("{x}"))).collect::<Option<Vec<_>>>().map(|v| format!("[{}]", v.join(", "))),
            _ => None,
        };
        let tip = "Work this value out from a formula every frame: other nodes' params (node.param), frame, sin, noise, wiggle…";
        if ui.add_enabled(start.is_some(), egui::Button::new("Use a formula")).on_hover_text(tip).clicked() {
            doc.set_param(id, name, arclight::link::expr(start.as_deref().unwrap_or("0")));
            ui.close();
        }
    }

    /// A param linked to another node: its value now (read-only), where it
    /// comes from, and Unlink, which keeps the value as a plain one; below,
    /// × and + per component (`linked × multiply + add`), e.g. half a
    /// rotation or a track followed 10 px to the side. A formula link has
    /// its formula to edit above that.
    fn linked_widget(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str, spec: &ParamSpec) {
        let raw = doc.param(id, spec.name);
        let problem = doc.param_error(id, spec.name, self.frame);
        let now = doc.param_at(id, spec.name, self.frame);
        let shown = match &now {
            Value::Array(a) => a.iter().map(|v| format!("{:.3}", v.as_f64().unwrap_or(0.0))).collect::<Vec<_>>().join("  "),
            Value::Number(n) => format!("{:.3}", n.as_f64().unwrap_or(0.0)),
            other => other.to_string(),
        };
        // Components, and the link's multiply / add spread over them.
        let n = now.as_array().map_or(1, Vec::len).max(1);
        let spread = |x: &Value, default: f64| -> Vec<f64> {
            (0..n)
                .map(|i| match x {
                    Value::Array(a) => a.get(i).and_then(Value::as_f64).unwrap_or(default),
                    other => other.as_f64().unwrap_or(default),
                })
                .collect()
        };
        let (mut mul, mut add) = (spread(&raw["link"]["multiply"], 1.0), spread(&raw["link"]["add"], 0.0));
        let mut changed = false;
        let formula = arclight::link::formula(&raw).map(str::to_string);
        ui.vertical(|ui| {
            if let Some(f) = &formula {
                let key = (id.to_string(), spec.name.to_string());
                let mut buf = self.formula_edit.remove(&key).unwrap_or_else(|| f.clone());
                let field = ui.make_persistent_id(("formula", id, spec.name));
                let left = ui.horizontal(|ui| {
                    ui.weak("?").on_hover_ui(formula_help);
                    self.formula_field(ui, doc, field, &mut buf)
                });
                let parsed = arclight::expr::parse(&buf);
                if let Err(e) = &parsed {
                    ui.colored_label(Color32::from_rgb(235, 100, 90), e);
                }
                if left.inner {
                    if parsed.is_ok() && &buf != f {
                        let mut l = raw.clone();
                        l["link"]["expr"] = json!(buf);
                        doc.set_param(id, spec.name, l);
                    }
                } else {
                    self.formula_edit.insert(key, buf);
                }
            }
            if let Some(e) = &problem {
                ui.colored_label(Color32::from_rgb(235, 100, 90), e);
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(shown).monospace());
                let (text, tip) = match formula {
                    Some(_) => ("= formula".to_string(), "Worked out from the formula above at every frame (sin, cos, noise, wiggle, at, node.param…; see README)"),
                    None => (format!("← {}", arclight::link::describe(&raw)), "Linked: follows that node live, so it updates when it changes (re-tracking, say)"),
                };
                ui.label(RichText::new(text).color(Color32::from_rgb(120, 200, 240))).on_hover_text(tip);
                if ui.small_button("Unlink").on_hover_text("Keep this frame's value as a plain one and stop following").clicked() {
                    doc.set_param(id, spec.name, now.clone());
                }
            });
            ui.horizontal(|ui| {
                ui.weak("×").on_hover_text("Multiply the linked value (0.5: half of it; -1: mirrored)");
                for m in &mut mul {
                    changed |= ui.add(egui::DragValue::new(m).speed(0.01).max_decimals(3)).changed();
                }
                ui.weak("+").on_hover_text("Then add this (an offset)");
                for a in &mut add {
                    changed |= ui.add(egui::DragValue::new(a).speed(0.5).max_decimals(3)).changed();
                }
            });
        });
        if changed && arclight::link::is_link(&doc.param(id, spec.name)) {
            // One number for a single component; ×1 and +0 mean none.
            let pack = |v: &[f64], identity: f64| {
                if v.iter().all(|&x| x == identity) {
                    Value::Null
                } else if n == 1 {
                    json!(v[0])
                } else {
                    json!(v)
                }
            };
            doc.set_param(id, spec.name, arclight::link::with_adjust(&raw, pack(&mul, 1.0), pack(&add, 0.0)));
        }
    }

    /// The formula text field, with completions for the word at the
    /// cursor (functions, `frame`/`time`/…, node names, then a node's
    /// params and components) in a list under it: ↑/↓ pick, Tab or a click
    /// takes one. True when the user is done with the field (Enter or
    /// focus elsewhere).
    fn formula_field(&mut self, ui: &mut egui::Ui, doc: &Doc, field: egui::Id, buf: &mut String) -> bool {
        let ctx = ui.ctx().clone();
        let focused = ctx.memory(|m| m.has_focus(field));
        let cursor = egui::text_edit::TextEditState::load(&ctx, field).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0);
        let mut take = None;
        if let (true, Some(c)) = (focused, cursor) {
            let n = completions(buf, c, doc).1.len();
            if n > 0 {
                ui.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        self.formula_pick = (self.formula_pick + 1) % n;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        self.formula_pick = (self.formula_pick + n - 1) % n;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                        take = Some(self.formula_pick.min(n - 1));
                    }
                });
            }
        }
        let out = egui::TextEdit::singleline(buf)
            .id(field)
            .code_editor()
            .desired_width(f32::INFINITY)
            .hint_text("e.g. t.track1 + [wiggle(0.1, 5), 0]")
            .show(ui);
        if out.response.changed() {
            self.formula_pick = 0;
        }
        // A press on the list takes focus from the field: give it back.
        let on_list = self.formula_popup.zip(ctx.input(|i| i.pointer.interact_pos())).is_some_and(|(r, p)| r.contains(p));
        let mut done = out.response.lost_focus();
        if done && on_list {
            done = false;
            ctx.memory_mut(|m| m.request_focus(field));
        }
        let cursor = out.cursor_range.map(|r| r.primary.index.0).or(cursor);
        let (start, list) = match cursor {
            Some(c) if out.response.has_focus() || on_list => completions(buf, c, doc),
            _ => (0, Vec::new()),
        };
        self.formula_popup = None;
        if !list.is_empty() && !done {
            let pick = self.formula_pick.min(list.len() - 1);
            let area = egui::Area::new(field.with("completions"))
                .order(egui::Order::Foreground)
                .fixed_pos(out.response.rect.left_bottom())
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        for (i, c) in list.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.selectable_label(i == pick, RichText::new(&c.label).monospace()).clicked() {
                                    take = Some(i);
                                }
                                ui.weak(&c.detail);
                            });
                        }
                    });
                });
            self.formula_popup = Some(area.response.rect);
        }
        if let (Some(i), Some(c)) = (take, cursor) {
            if let Some(s) = list.get(i) {
                let at = |k: usize| buf.char_indices().nth(k).map_or(buf.len(), |(b, _)| b);
                let (from, to) = (at(start), at(c));
                buf.replace_range(from..to, &s.insert);
                let end = start + s.insert.chars().count();
                let mut state = out.state;
                state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(end))));
                state.store(&ctx, field);
                ctx.memory_mut(|m| m.request_focus(field));
                self.formula_pick = 0;
            }
        }
        done
    }

    /// What an unset param with an automatic default stands for, when
    /// known: a Transform's center is its input layer's.
    fn auto_value(&self, id: &str, spec: &ParamSpec) -> Option<Value> {
        let (_, w, _) = self.layer.as_ref().filter(|(node, w, _)| node == id && !w.is_empty())?;
        (spec.name == "center").then(|| json!([(w.x0 + w.x1) as f32 / 2.0, (w.y0 + w.y1) as f32 / 2.0]))
    }

    fn param_widget(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str, op: &str, spec: &ParamSpec) {
        let mut value = doc.param_at(id, spec.name, self.frame);
        // Unset with an automatic default: show what it stands for.
        let auto = value.is_null() && spec.default_value().is_null();
        if auto {
            value = self.auto_value(id, spec).unwrap_or(Value::Null);
        }
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
                let mut reset = false;
                ui.horizontal(|ui| {
                    if auto && value.is_null() {
                        ui.weak("auto");
                        return;
                    }
                    for (i, x) in v.iter_mut().enumerate() {
                        let prefix = ["x ", "y ", "z "][i];
                        let prefix = if n == 3 { ["r ", "g ", "b "][i] } else { prefix };
                        changed |= ui.add(egui::DragValue::new(x).speed(speed).prefix(prefix).max_decimals(4)).changed();
                    }
                    if auto {
                        ui.weak("auto").on_hover_text("The input layer's center; edit to set your own");
                    } else if spec.default_value().is_null() {
                        reset = ui.small_button("Auto").on_hover_text("Back to the input layer's center").clicked();
                    }
                });
                if reset {
                    doc.set_param(id, spec.name, Value::Null);
                }
                changed.then(|| json!(v))
            }
            ParamKind::Color if auto && value.is_null() => {
                // Worked out by the op (a Keyer's screen color): nothing to
                // show until set.
                let mut set = false;
                ui.horizontal(|ui| {
                    ui.weak("auto").on_hover_text("Estimated from the image. Ctrl+click (or Ctrl+drag) in the viewer to pick it");
                    set = ui.small_button("Set").on_hover_text("Choose the color yourself").clicked();
                });
                set.then(|| json!([0.1, 0.5, 0.1, 1.0]))
            }
            ParamKind::Color => {
                let mut c = [f(&value, 0), f(&value, 1), f(&value, 2), f(&value, 3)];
                let mut changed = false;
                let mut reset = false;
                ui.horizontal(|ui| {
                    changed |= ui.color_edit_button_rgba_unmultiplied(&mut c).changed();
                    for (i, x) in c.iter_mut().enumerate() {
                        changed |= ui
                            .add(egui::DragValue::new(x).speed(0.005).prefix(["r ", "g ", "b ", "a "][i]).max_decimals(3))
                            .changed();
                    }
                    if spec.default_value().is_null() {
                        reset = ui.small_button("Auto").on_hover_text("Estimate it from the image again").clicked();
                    }
                });
                if reset {
                    doc.set_param(id, spec.name, Value::Null);
                }
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
            ParamKind::Flags(names) => {
                let on = |n: &str| value.as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(n)));
                let mut chosen: Vec<&str> = names.iter().copied().filter(|n| on(n)).collect();
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    for n in names.iter() {
                        let mut b = chosen.contains(n);
                        if ui.checkbox(&mut b, *n).changed() {
                            changed = true;
                            if b { chosen.push(n) } else { chosen.retain(|c| c != n) }
                        }
                    }
                });
                // In the list's own order.
                changed.then(|| json!(names.iter().filter(|n| chosen.contains(n)).collect::<Vec<_>>()))
            }
            ParamKind::Text => {
                let key = (id.to_string(), spec.name.to_string());
                let current = value.as_str().unwrap_or("").to_string();
                let mut buf = self.text_edit.remove(&key).unwrap_or_else(|| current.clone());
                let mut out = None;
                let layers = self.input_layers.as_ref().filter(|(n, _)| n == id).map(|(_, l)| l.as_slice());
                let offered = text_suggestions(op, spec.name, layers);
                ui.horizontal(|ui| {
                    let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(150.0));
                    if resp.has_focus() {
                        self.text_edit.insert(key.clone(), buf.clone());
                    } else if resp.lost_focus() && buf != current {
                        out = Some(json!(buf.trim()));
                    }
                    egui::ComboBox::from_id_salt(("text", id, spec.name)).selected_text("").width(20.0).show_ui(ui, |ui| {
                        for o in &offered {
                            if ui.selectable_label(*o == current, o).clicked() {
                                out = Some(json!(o));
                            }
                        }
                    });
                });
                out
            }
            ParamKind::Path { save } => {
                let mut s = value.as_str().unwrap_or("").to_string();
                let mut out = None;
                ui.horizontal(|ui| {
                    let browse = ui.add_enabled(!dialogs::busy(), egui::Button::new("Browse…")).on_disabled_hover_text("A file dialog is open");
                    if browse.clicked() {
                        let what = Browse { node: id.to_string(), param: spec.name, op: op.to_string(), animatable: spec.animatable, frame: self.frame };
                        self.browse = if save {
                            dialogs::save(what, dialogs::filter(dialogs::new().set_file_name("render.####.png"), "Image sequence / video", WRITE_EXTS))
                        } else if op == "read_geo" {
                            dialogs::pick(what, dialogs::filter(dialogs::new(), "3D geometry (OBJ, glTF)", GEO_EXTS))
                        } else {
                            dialogs::pick(what, dialogs::filter(dialogs::new(), "Images & video", READ_EXTS))
                        };
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

    /// A Browse… dialog that has closed: set its param to the file chosen
    /// (a Media In takes the file's range too), as if chosen at once.
    fn poll_browse(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let Some(answer) = self.browse.as_ref().and_then(|p| p.poll()) else {
            if self.browse.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let Some(b) = self.browse.take().map(|p| p.what) else { return };
        if let Some(path) = answer {
            apply_browse(doc, &mut self.media_info, &b, &path);
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
            // Keying an automatic value fixes it first.
            if raw.is_null() {
                let auto = doc
                    .node(id)
                    .and_then(|n| ops::info(&n.op))
                    .and_then(|i| i.params.iter().find(|s| s.name == name))
                    .and_then(|s| self.auto_value(id, s));
                match auto {
                    Some(v) => doc.set_param(id, name, v),
                    None => return,
                }
            }
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

    /// Set a Corner Pin's From (or To) quad to its input's frame corners.
    fn corner_pin_extras(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        let size = self.layer.as_ref().filter(|(node, ..)| node == id).map(|(.., f)| *f);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (quad, label) in [("from", "From"), ("to", "To")] {
                let button = ui
                    .add_enabled(size.is_some(), egui::Button::new(format!("{label} = image corners")))
                    .on_hover_text(format!("Put the {label} corners on the input's frame corners, clockwise from the top left"))
                    .on_disabled_hover_text("Connect an input first");
                if let (true, Some(f)) = (button.clicked(), size) {
                    for (i, c) in corner_pin_corners(f).into_iter().enumerate() {
                        doc.edit_param(id, &format!("{quad}{}", i + 1), self.frame, json!(c));
                    }
                }
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

impl Properties {
    /// Key the positions tracking runs have found since the last UI frame,
    /// follow them with the current frame, and collect finished runs. Runs
    /// for every tracker, open panel or not.
    fn collect_tracks(&mut self, doc: &mut Doc) {
        for (id, job) in &mut self.tracks {
            // Checked before draining, so a finished run's last frame isn't missed.
            let finished = job.handle.as_ref().is_some_and(|h| h.is_finished());
            let found = std::mem::take(&mut *job.found.lock().unwrap());
            for t in &found {
                key_tracks(doc, id, t.frame, &t.pos, t.warps.as_deref(), Some(&t.scores));
            }
            if let Some(t) = found.last() {
                self.seek = Some(t.frame);
            }
            if finished {
                job.result = Some(match job.handle.take().unwrap().join() {
                    Ok(Ok(out)) => {
                        let mut msg = format!("Tracked {} frame(s).", out.frames);
                        for (i, f) in out.lost {
                            msg += &format!(" Track {} lost at frame {f}.", i + 1);
                        }
                        (true, msg)
                    }
                    Ok(Err(e)) => (false, format!("{e:#}")),
                    Err(_) => (false, "tracking thread panicked".into()),
                });
            }
        }
    }

    fn tracker_extras(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        ui.add_space(4.0);
        if let Some(job) = self.tracks.get(id).filter(|j| j.handle.is_some()) {
            let p = &job.progress;
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(p.fraction())
                        .text(format!("{} / {}", p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed)))
                        .desired_width(ui.available_width() - 70.0),
                );
                if ui.button("Stop").clicked() {
                    p.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
            return;
        }
        let (first, last) = (doc.graph.project.first_frame, doc.graph.project.last_frame);
        let frame = self.frame;
        let mut run = None;
        ui.horizontal(|ui| {
            let enabled = doc.node(id).is_some_and(|n| n.inputs.first().is_some_and(|i| !i.is_empty()));
            ui.add_enabled_ui(enabled, |ui| {
                if ui.button("◀◀ Track").on_hover_text(format!("Track backward to frame {first}")).clicked() {
                    run = Some(first);
                }
                if ui.button("◀ 1").on_hover_text("Track one frame backward").clicked() {
                    run = Some((frame - 1).max(first));
                }
                if ui.button("1 ▶").on_hover_text("Track one frame forward").clicked() {
                    run = Some((frame + 1).min(last));
                }
                if ui.button("Track ▶▶").on_hover_text(format!("Track forward to frame {last}")).clicked() {
                    run = Some(last);
                }
            })
            .response
            .on_disabled_hover_text("Connect an input to track");
        });
        ui.horizontal(|ui| {
            if ui.button("Clear ◀").on_hover_text(format!("Remove the enabled tracks' keys before frame {frame}")).clicked() {
                clear_track_keys(doc, id, frame, false);
            }
            if ui.button("Clear ▶").on_hover_text(format!("Remove the enabled tracks' keys after frame {frame}")).clicked() {
                clear_track_keys(doc, id, frame, true);
            }
            let offset = doc.node(id).is_some_and(|n| track::offsets(&n.params).iter().any(|o| *o != [0.0, 0.0]));
            if offset && ui.button("Clear offsets").on_hover_text("Put every track's boxes back on its point (Ctrl-drag a box to offset it)").clicked() {
                for i in 1..=track::TRACKS {
                    doc.remove_param(id, &format!("track{i}_offset"));
                }
            }
        });
        self.track_boxes(ui, doc, id);
        if ui.button("Reference = this frame").on_hover_text("Solve the output transform relative to the current frame").clicked() {
            doc.set_param(id, "reference_frame", json!(frame));
        }
        let mut export = None;
        let mut linked = !self.copy_exports;
        if ui
            .checkbox(&mut linked, "Linked")
            .on_hover_text("Exported nodes follow this Tracker live, so re-tracking updates them. Off: they get a copy of the keys")
            .changed()
        {
            self.copy_exports = !linked;
        }
        let four = doc.node(id).is_some_and(|n| track::positions(&n.params, frame).iter().all(Option::is_some));
        for (op, label, enabled) in [("transform", "Transform", true), ("corner_pin", "Corner Pin", four)] {
            ui.horizontal(|ui| {
                ui.label(format!("To {label}:"));
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.button("Match-move").on_hover_text(format!("Add a {label} that moves a layer along with the tracks")).clicked() {
                        export = Some((op, false));
                    }
                    if ui.button("Stabilize").on_hover_text(format!("Add a {label} that removes the tracked motion")).clicked() {
                        export = Some((op, true));
                    }
                })
                .response
                .on_disabled_hover_text("Turn on all four tracks");
            });
        }
        if let Some(to) = run.filter(|&to| to != frame) {
            self.start_tracking(doc, id, frame, to);
        }
        if let Some((op, stabilize)) = export {
            let result = match export_tracker(doc, id, op, stabilize, !self.copy_exports) {
                Ok(xf) => (true, format!("Added {xf}{}", if self.copy_exports { "" } else { ", linked" })),
                Err(e) => (false, format!("{e:#}")),
            };
            let job = TrackJob { progress: Default::default(), handle: None, found: Default::default(), result: Some(result) };
            self.tracks.insert(id.to_string(), job);
        }
        if let Some((ok, msg)) = self.tracks.get(id).and_then(|j| j.result.as_ref()) {
            let color = if *ok { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(235, 100, 90) };
            ui.colored_label(color, msg);
        }
        if let Some(node) = doc.node(id) {
            let min = ops::params_with_defaults("tracker", &node.params)["min_score"].as_f64().unwrap_or(0.7) as f32;
            if let Some((i, f, score)) = track::weakest(&node.params) {
                let text = format!("Weakest match: track {} at frame {f} ({score:.2})", i + 1);
                let weak = score < track::weak_below(min);
                let label = if weak { RichText::new(text).color(Color32::from_rgb(235, 150, 60)) } else { RichText::new(text).weak() };
                if ui.add(egui::Label::new(label).sense(egui::Sense::click())).on_hover_text("Click to go to that frame").clicked() {
                    self.seek = Some(f);
                }
            }
        }
    }

    /// Each track's own pattern and search box sizes and its offset, as a
    /// table (the viewer edits the same by dragging box corners and
    /// Ctrl-dragging boxes). A size with no override shows the shared one,
    /// greyed; editing it gives the track its own. ↺ puts the track back on
    /// the shared sizes with no offset.
    fn track_boxes(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        let Some(node) = doc.node(id) else { return };
        let params = node.params.clone();
        let p = ops::params_with_defaults("tracker", &params);
        let sizes = track::box_sizes(&params);
        let offsets = track::offsets(&params);
        egui::CollapsingHeader::new("Per-track boxes").id_salt(("track_boxes", id)).default_open(false).show(ui, |ui| {
            egui::Grid::new(("track_boxes_grid", id)).num_columns(6).spacing([8.0, 4.0]).show(ui, |ui| {
                for h in ["", "Pattern", "Search", "Offset x", "Offset y", ""] {
                    ui.weak(h);
                }
                ui.end_row();
                for i in 0..track::TRACKS {
                    let n = i + 1;
                    let on = p[format!("track{n}_on")].as_bool() == Some(true);
                    let own = |which: &str| params.get(format!("track{n}_{which}")).is_some_and(|v| !v.is_null());
                    let has_own = own("pattern") || own("search") || offsets[i] != [0.0, 0.0];
                    ui.add_enabled_ui(on, |ui| ui.label(format!("Track {n}")));
                    let [mut pattern, mut search] = sizes[i].map(|s| s as i64);
                    for (which, value, range) in [
                        ("pattern", &mut pattern, 5..=(sizes[i][1] as i64 - 2).min(255)),
                        ("search", &mut search, (sizes[i][0] as i64 + 2)..=511),
                    ] {
                        let field = egui::DragValue::new(value).range(range).speed(0.5);
                        let field = if own(which) { field } else { field.custom_formatter(|v, _| format!("{v:.0} ·")) };
                        let tip = if own(which) { "This track's own size" } else { "The shared size (·); edit to give this track its own" };
                        if ui.add_enabled(on, field).on_hover_text(tip).changed() {
                            doc.set_param(id, &format!("track{n}_{which}"), json!(*value));
                        }
                    }
                    let mut o = offsets[i];
                    for c in 0..2 {
                        if ui.add_enabled(on, egui::DragValue::new(&mut o[c]).speed(0.5).max_decimals(2)).on_hover_text("The boxes' offset from the track's point, in pixels").changed() {
                            doc.set_param(id, &format!("track{n}_offset"), json!(o));
                        }
                    }
                    if ui.add_enabled(on && has_own, egui::Button::new("↺").small()).on_hover_text("Back to the shared sizes, no offset").clicked() {
                        for which in ["pattern", "search", "offset"] {
                            doc.remove_param(id, &format!("track{n}_{which}"));
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }

    fn start_tracking(&mut self, doc: &mut Doc, id: &str, from: i64, to: i64) {
        let Some(node) = doc.node(id) else { return };
        let start = track::positions(&node.params, from);
        let settings = track::Settings::from_params(&ops::params_with_defaults("tracker", &node.params));
        // Runs measure turn and growth from the start frame: carry on from
        // what's keyed there.
        let follow = settings.motion != track::Solve::Translate;
        let base: Vec<_> = track::warps(&node.params, from).into_iter().map(Option::unwrap_or_default).collect();
        // Anchor each track's curve at the start frame.
        key_tracks(doc, id, from, &start, follow.then_some(&base[..]), None);
        let frames = match track::input_frames(&doc.graph, id) {
            Ok(f) => f,
            Err(e) => {
                let job = TrackJob { progress: Default::default(), handle: None, found: Default::default(), result: Some((false, format!("{e:#}"))) };
                self.tracks.insert(id.to_string(), job);
                return;
            }
        };
        let progress = Arc::new(Progress::default());
        let found = Found::default();
        let (pg, out) = (progress.clone(), found.clone());
        let handle = std::thread::spawn(move || {
            track::track(frames, &start, from, to, &settings, &pg, |frame, step| {
                let warps = follow.then(|| base.iter().zip(step.warps).map(|(b, w)| b.then(*w)).collect());
                out.lock().unwrap().push(Tracked { frame, pos: step.pos.to_vec(), scores: step.scores.to_vec(), warps })
            })
        });
        self.tracks.insert(id.to_string(), TrackJob { progress, handle: Some(handle), found, result: None });
    }
}

/// Add a node of type `op` below tracker `id`, keyed with its motion, and
/// select it. Returns its name.
fn export_tracker(doc: &mut Doc, id: &str, op: &str, stabilize: bool, linked: bool) -> anyhow::Result<String> {
    let params = track::export(&doc.graph, id, op, stabilize, linked)?;
    let at = doc.node(id).and_then(|n| n.pos).unwrap_or_default();
    let mut pos = [at[0], at[1] + NODE_H + 40.0];
    while doc.graph.nodes.iter().filter_map(|n| n.pos).any(|q| (q[0] - pos[0]).abs() < NODE_W + 20.0 && (q[1] - pos[1]).abs() < NODE_H + 20.0) {
        pos[1] += NODE_H + 40.0;
    }
    let xf = doc.add_node(op, pos);
    for (name, v) in params {
        doc.set_param(&xf, &name, v);
    }
    doc.selected = Some(xf.clone());
    Ok(xf)
}

/// Key each found track position of tracker `id` at `frame`, and its warp
/// (`track<i>_warp`) and match score (`track<i>_score`) when given.
fn key_tracks(doc: &mut Doc, id: &str, frame: i64, pos: &[Option<track::Pt>], warps: Option<&[track::Warp]>, scores: Option<&[f32]>) {
    let mut key = |name: String, v: Value| {
        let raw = doc.param(id, &name);
        doc.set_param(id, &name, anim::set_key(&raw, frame, &v));
    };
    for (i, p) in pos.iter().enumerate() {
        if let Some(p) = p {
            key(format!("track{}", i + 1), json!(p));
            if let Some(w) = warps.and_then(|w| w.get(i)) {
                key(format!("track{}_warp", i + 1), json!(w.to_param()));
            }
            if let Some(s) = scores.and_then(|s| s.get(i)) {
                key(format!("track{}_score", i + 1), json!((*s as f64 * 1e4).round() / 1e4));
            }
        }
    }
}

/// Remove the keys of tracker `id`'s enabled tracks (positions, warps and
/// scores) before `frame`, or after it with `after`.
fn clear_track_keys(doc: &mut Doc, id: &str, frame: i64, after: bool) {
    let Some(node) = doc.node(id) else { return };
    let on: Vec<bool> = track::positions(&node.params, frame).iter().map(Option::is_some).collect();
    for i in (1..=on.len()).filter(|i| on[i - 1]) {
        for name in [format!("track{i}"), format!("track{i}_warp"), format!("track{i}_score")] {
            let raw = doc.param(id, &name);
            if anim::is_animated(&raw) {
                let drop = |f: i64| if after { f > frame } else { f < frame };
                doc.set_param(id, &name, anim::remove_keys(&raw, drop, frame as f64));
            }
        }
    }
}

/// Point a Media In node at `file`: videos and sequences get their frame
/// range filled in. The first clip brought into a project also sets the
/// project's frame range and rate.
/// What a Browse… dialog is choosing a file for.
#[derive(Clone, Debug, PartialEq)]
pub struct Browse {
    pub node: String,
    pub param: &'static str,
    pub op: String,
    /// The param animates: the file goes in as a key at `frame`.
    pub animatable: bool,
    pub frame: i64,
}

/// Put file `path`, chosen for `b`, in its param: a Media In's file brings
/// its frame range along ([`apply_media`]); others just the path.
fn apply_browse(doc: &mut Doc, media_info: &mut HashMap<String, String>, b: &Browse, path: &Path) {
    if doc.node(&b.node).is_none() {
        return;
    }
    if b.op == "media_in" && b.param == "path" {
        media_info.remove(&b.node);
        apply_media(doc, &b.node, path);
    } else if b.animatable {
        doc.edit_param(&b.node, b.param, b.frame, json!(path.to_string_lossy()));
    } else {
        doc.set_param(&b.node, b.param, json!(path.to_string_lossy()));
    }
}

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
            Ok(v) => format!("Video · {}x{} · {}-bit · {:.3} fps · {} frames", v.width, v.height, v.bits, v.fps, v.frames),
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

/// A frame's corners, clockwise from the top left (Corner Pin's 1 to 4).
fn corner_pin_corners(f: arclight::image::Size) -> [[f32; 2]; 4] {
    let (w, h) = (f.width as f32, f.height as f32);
    [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]]
}

/// A backdrop color for painting.
pub fn backdrop_color32(c: [f32; 3]) -> Color32 {
    let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(u(c[0]), u(c[1]), u(c[2]))
}

/// The backdrop palette's swatches, then a picker for any other color.
/// True when `color` changed.
pub fn backdrop_palette(ui: &mut egui::Ui, color: &mut [f32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for c in arclight::graph::Backdrop::COLORS {
            let (r, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
            let on = *color == c;
            ui.painter().rect_filled(r, 3.0, backdrop_color32(c));
            if on || resp.hovered() {
                ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(if on { 2.0 } else { 1.0 }, Color32::WHITE), egui::StrokeKind::Inside);
            }
            if resp.clicked() && !on {
                *color = c;
                changed = true;
            }
        }
        changed |= ui.color_edit_button_rgb(color).on_hover_text("Any color").changed();
    });
    changed
}

/// Formula functions for completion and the help: name, call, what it does.
const FORMULA_FUNCS: &[(&str, &str, &str)] = &[
    ("sin", "sin(x)", "sine (radians)"),
    ("cos", "cos(x)", "cosine (radians)"),
    ("tan", "tan(x)", "tangent (radians)"),
    ("asin", "asin(x)", "arc sine"),
    ("acos", "acos(x)", "arc cosine"),
    ("atan", "atan(x)", "arc tangent"),
    ("atan2", "atan2(y, x)", "angle of (x, y)"),
    ("sqrt", "sqrt(x)", "square root"),
    ("abs", "abs(x)", "absolute value"),
    ("floor", "floor(x)", "round down"),
    ("ceil", "ceil(x)", "round up"),
    ("round", "round(x)", "nearest whole number"),
    ("fract", "fract(x)", "x - floor(x)"),
    ("sign", "sign(x)", "-1, 0 or 1"),
    ("min", "min(a, b, …)", "smallest (one list: its smallest)"),
    ("max", "max(a, b, …)", "largest (one list: its largest)"),
    ("clamp", "clamp(x, lo, hi)", "x kept between lo and hi"),
    ("mix", "mix(a, b, k)", "a to b by k (0..1)"),
    ("smoothstep", "smoothstep(a, b, x)", "0 to 1 as x goes a to b, eased"),
    ("pow", "pow(x, y)", "x to the power y (also x ^ y)"),
    ("exp", "exp(x)", "e to the power x"),
    ("log", "log(x)", "natural logarithm"),
    ("rad", "rad(deg)", "degrees to radians"),
    ("deg", "deg(rad)", "radians to degrees"),
    ("length", "length(v)", "length of a list as a vector"),
    ("dist", "dist(a, b)", "distance between two points"),
    ("if", "if(c, a, b)", "a where c isn't 0, else b"),
    ("noise", "noise(x, seed)", "smooth noise, -1..1"),
    ("random", "random(seed)", "a fixed number in 0..1"),
    ("wiggle", "wiggle(freq, amp, seed)", "amp × noise(frame × freq)"),
    ("at", "at(x, f)", "x worked out at frame f"),
];

/// Formula variables: name, what it is.
const FORMULA_VARS: &[(&str, &str)] = &[
    ("frame", "the frame"),
    ("time", "seconds from the first frame"),
    ("fps", "the project's frame rate"),
    ("pi", "3.14159…"),
    ("tau", "2π"),
    ("e", "2.71828…"),
];

/// A completion: what it matches, what it puts in, how it's listed.
#[derive(Debug, PartialEq)]
struct Completion {
    key: String,
    insert: String,
    label: String,
    detail: String,
}

/// Completions for the word before char `cursor` in `text`, and the char
/// where that word starts: after `node.`, its params; after
/// `node.param.`, components; else variables, functions and node names.
/// Nothing inside a number or before anything is typed.
fn completions(text: &str, cursor: usize, doc: &Doc) -> (usize, Vec<Completion>) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let word = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let start = cursor - chars[..cursor].iter().rev().take_while(|c| word(c)).count();
    let typed: String = chars[start..cursor].iter().collect();
    let item = |key: &str, insert: String, label: String, detail: &str| Completion { key: key.into(), insert, label, detail: detail.into() };
    let mut out = Vec::new();
    if typed.starts_with(|c: char| c.is_ascii_digit()) {
        return (start, out);
    }
    if start > 0 && chars[start - 1] == '.' {
        // The name before the dot: plain, or "quoted".
        let dot = start - 1;
        let (name, before) = if dot > 0 && chars[dot - 1] == '"' {
            match chars[..dot - 1].iter().rposition(|c| *c == '"') {
                Some(q) => (chars[q + 1..dot - 1].iter().collect::<String>(), q),
                None => return (start, out),
            }
        } else {
            let k = dot - chars[..dot].iter().rev().take_while(|c| word(c)).count();
            (chars[k..dot].iter().collect(), k)
        };
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return (start, out);
        }
        if before > 0 && chars[before - 1] == '.' {
            for (c, what) in [("x", "1st"), ("y", "2nd"), ("z", "3rd"), ("w", "4th"), ("r", "red"), ("g", "green"), ("b", "blue"), ("a", "alpha")] {
                out.push(item(c, c.into(), c.into(), what));
            }
        } else if let Some(info) = doc.node(&name).and_then(|n| ops::info(&n.op)) {
            for p in info.params {
                out.push(item(p.name, p.name.into(), p.name.into(), p.label));
            }
        }
    } else {
        if typed.is_empty() {
            return (start, out);
        }
        for (name, what) in FORMULA_VARS {
            out.push(item(name, name.to_string(), name.to_string(), what));
        }
        for (name, call, what) in FORMULA_FUNCS {
            out.push(item(name, format!("{name}("), call.to_string(), what));
        }
        for n in &doc.graph.nodes {
            let plain = n.id.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && n.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            let insert = if plain { format!("{}.", n.id) } else { format!("\"{}\".", n.id) };
            let detail = ops::info(&n.op).map_or(n.op.as_str(), |i| i.label);
            out.push(item(&n.id, insert.clone(), insert, detail));
        }
    }
    let typed_lower = typed.to_lowercase();
    out.retain(|c| c.key.to_lowercase().starts_with(&typed_lower) && c.insert != typed);
    out.truncate(12);
    (start, out)
}

/// The `?` beside a formula: what can go in one.
fn formula_help(ui: &mut egui::Ui) {
    ui.label(RichText::new("Formula").strong());
    ui.label("node.param reads another node's param (.x .y .z .w / .r .g .b .a or [i] for one component). Lists work per component: [a, b] * 2. Operators: + - * / % ^ < <= > >= == != && || !");
    ui.add_space(4.0);
    egui::Grid::new("formula-help").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
        for (name, what) in FORMULA_VARS {
            ui.monospace(*name);
            ui.weak(*what);
            ui.end_row();
        }
        for (_, call, what) in FORMULA_FUNCS {
            ui.monospace(*call);
            ui.weak(*what);
            ui.end_row();
        }
    });
    ui.add_space(4.0);
    ui.weak("Type to get suggestions: ↑/↓ to pick, Tab to take one. Enter applies the formula.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::{Graph, NodeSpec};

    #[test]
    fn a_browse_answer_arriving_later_sets_the_param() {
        let _turn = crate::dialogs::tests::TURN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("arclight_browse_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 3..=6 {
            arclight::image::Image::new(4, 4).write(&dir.join(format!("shot.{f:04}.exr"))).unwrap();
        }
        let mut doc = Doc::new(Graph::default(), None);
        let read = doc.add_node("read", [0.0, 0.0]);
        let clip = doc.add_node("media_in", [0.0, 100.0]);
        doc.commit(false);
        let ctx = egui::Context::default();
        let mut props = Properties::default();
        let browse = |node: &str, op: &str| Browse { node: node.into(), param: "path", op: op.into(), animatable: false, frame: 1 };
        // A dialog held open: nothing happens until it answers.
        let (go, answer) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
        props.browse = dialogs::start(browse(&read, "read"), move || answer.recv().unwrap());
        props.poll_browse(&ctx, &mut doc);
        assert!(props.browse.is_some() && doc.param(&read, "path") == json!(""));
        go.send(Some(dir.join("plate.exr"))).unwrap();
        let t0 = std::time::Instant::now();
        while props.browse.is_some() && t0.elapsed().as_secs() < 5 {
            props.poll_browse(&ctx, &mut doc);
        }
        assert_eq!(doc.param(&read, "path"), json!(dir.join("plate.exr").to_string_lossy()));
        // One undo step, as a pick at once would be.
        doc.commit(false);
        doc.undo();
        assert_eq!(doc.param(&read, "path"), json!(""));
        // A Media In takes the sequence's range with the file.
        let frame = dir.join("shot.0004.exr");
        props.browse = dialogs::start(browse(&clip, "media_in"), move || Some(frame));
        while props.browse.is_some() {
            props.poll_browse(&ctx, &mut doc);
        }
        assert_eq!((doc.param(&clip, "first_frame"), doc.param(&clip, "last_frame")), (json!(3), json!(6)));
        assert!(doc.param(&clip, "path").as_str().unwrap().ends_with("shot.####.exr"));
        // Cancelled: nothing changes; the node gone meanwhile: ignored.
        let before = doc.graph.clone();
        props.browse = dialogs::start(browse(&read, "read"), || None);
        while props.browse.is_some() {
            props.poll_browse(&ctx, &mut doc);
        }
        assert_eq!(doc.graph, before);
        props.browse = dialogs::start(browse("gone", "read"), || Some("x.exr".into()));
        while props.browse.is_some() {
            props.poll_browse(&ctx, &mut doc);
        }
        assert_eq!(doc.graph, before);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shuffle_offers_its_inputs_layers() {
        let layers = vec![("depth".to_string(), vec!["depth".to_string()]), ("N".to_string(), vec!["N.x".to_string(), "N.y".to_string(), "N.z".to_string()])];
        assert_eq!(text_suggestions("shuffle", "in", Some(&layers)), ["rgba", "depth", "N"]);
        assert_eq!(text_suggestions("shuffle", "remove", Some(&layers)), ["*", "depth", "N"]);
        assert_eq!(text_suggestions("shuffle", "red", Some(&layers)), ["R", "G", "B", "A", "0", "1", "depth", "N.x", "N.y", "N.z"]);
        // Out also offers new passes to write.
        let out = text_suggestions("shuffle", "out", Some(&layers));
        assert!(out.starts_with(&["rgba".to_string(), "depth".to_string(), "N".to_string()]) && out.contains(&"diffuse".to_string()));
        // Input not known yet: the usual names.
        assert!(text_suggestions("shuffle", "in", None).contains(&"P".to_string()));
        assert!(text_suggestions("grade", "in", Some(&layers)).is_empty());
    }

    #[test]
    fn formula_completions_follow_what_is_typed() {
        let mut d = Doc::new(Graph::default(), None);
        let t = d.add_node("tracker", [0.0, 0.0]);
        let r = d.add_node("rectangle", [200.0, 0.0]);
        d.rename(&r, "plate-2").unwrap();
        let keys = |text: &str| {
            let (start, list) = completions(text, text.chars().count(), &d);
            (start, list.into_iter().map(|c| c.insert).collect::<Vec<_>>())
        };
        // Variables, functions and nodes by prefix (any case).
        assert_eq!(keys("1 + si"), (4, vec!["sin(".to_string(), "sign(".to_string()]));
        assert_eq!(keys("Ti").1, ["time"]);
        assert!(keys("tr").1.contains(&format!("{t}.")));
        assert_eq!(keys("pla").1, ["\"plate-2\"."]);
        // A node's params, then components.
        assert!(keys(&format!("{t}.track1")).1.contains(&"track1_on".to_string()));
        assert_eq!(keys(&format!("{t}.ref")).1, ["reference_frame"]);
        assert_eq!(keys(&format!("{t}.")).1.len(), 12, "the first dozen before typing");
        assert_eq!(keys("\"plate-2\".cen").1, ["center"]);
        assert_eq!(keys(&format!("{t}.track1.")).1, ["x", "y", "z", "w", "r", "g", "b", "a"]);
        // Nothing yet, in a number, a finished word or an unknown node.
        assert!(keys("").1.is_empty() && keys("1 + ").1.is_empty() && keys("2.5e").1.is_empty());
        assert!(keys("frame").1.is_empty() && keys("nope.").1.is_empty());
        // In the middle of the text, up to the cursor.
        let (start, list) = completions("cos(fr) + 1", 6, &d);
        assert_eq!((start, list[0].insert.as_str()), (4, "frame"));
    }

    /// Every function the help and completions offer is one formulas know.
    #[test]
    fn formula_help_lists_real_functions() {
        for (name, call, _) in FORMULA_FUNCS {
            let e = arclight::expr::parse(&format!("{name}(1)")).err().unwrap_or_default();
            assert!(!e.contains("unknown function"), "{name}: {e}");
            assert!(call.starts_with(&format!("{name}(")));
        }
        for (name, _) in FORMULA_VARS {
            assert!(arclight::expr::parse(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn a_corner_pin_from_and_to_the_image_corners_leaves_the_image_alone() {
        use arclight::image::{Image, Size};
        use arclight::ops::{self, EvalContext};
        let mut src = Image::new(30, 20);
        for (i, p) in src.pixels.iter_mut().enumerate() {
            *p = [(i % 30) as f32 / 30.0, (i / 30) as f32 / 20.0, 0.5, 1.0];
        }
        let corners = corner_pin_corners(Size { width: 30, height: 20 });
        assert_eq!(corners, [[0.0, 0.0], [30.0, 0.0], [30.0, 20.0], [0.0, 20.0]]);
        let mut params = json!({});
        for (i, c) in corners.iter().enumerate() {
            params[format!("from{}", i + 1)] = json!(c);
            params[format!("to{}", i + 1)] = json!(c);
        }
        let out = ops::eval("corner_pin", &params, &[Some(&src), None], &EvalContext { frame: 1, write_outputs: false }).unwrap();
        let diff = out.flat().pixels.iter().zip(&src.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(diff < 1e-4, "off by {diff}");
    }

    #[test]
    fn clearing_keys_after_a_frame_spares_tracks_that_are_off() {
        let curve = json!({ "keys": [[1, [0, 0]], [2, [1, 0]], [3, [2, 0]]], "interp": "linear" });
        let params = json!({ "track1": curve, "track1_score": { "keys": [[2, 0.9], [3, 0.8]] }, "track2": curve });
        let g = Graph { nodes: vec![NodeSpec { id: "t".into(), op: "tracker".into(), inputs: vec![String::new()], params, pos: None }], ..Default::default() };
        let mut doc = Doc::new(g, None);
        clear_track_keys(&mut doc, "t", 2, true);
        assert_eq!(anim::key_frames(&doc.param("t", "track1")), [1, 2]);
        assert_eq!(anim::key_frames(&doc.param("t", "track1_score")), [2]);
        assert_eq!(anim::key_frames(&doc.param("t", "track2")), [1, 2, 3]);
        clear_track_keys(&mut doc, "t", 2, false);
        assert_eq!(anim::key_frames(&doc.param("t", "track1")), [2]);
    }
}
