//! The curve editor: the selected node's keyframed params plotted as value
//! over time, one curve per channel.
//!
//! Click a key to select it (Shift adds), drag on empty space to box-select,
//! drag keys to move them in time and value. Moving a key in time moves it on
//! every channel of that param, since a key holds the whole value. Double-click
//! a curve to add a key; ⌫ deletes selected keys. Scroll to pan, pinch or
//! ⌘-scroll to zoom (⌥ zooms value only, ⇧ time only), F to frame all.
//!
//! Selected keys on smooth curves show tangent handles. Dragging a handle sets
//! that channel's slope (both sides, unless the key is broken); ⌥-dragging
//! breaks the key first so the sides move independently. Hollow handles are
//! automatic tangents, filled ones are user-set.

use crate::doc::Doc;
use crate::properties::KEY_COLOR;
use arclight::anim::{self, Interp, Key, Side};
use arclight::ops::{self, ParamKind};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

const KEY_R: f32 = 4.5;
const HIT_R: f32 = 7.0;
const RULER_H: f32 = 20.0;
const LIST_W: f32 = 170.0;
/// On-screen length of a tangent handle.
const HANDLE_LEN: f32 = 48.0;
const HANDLE_COLOR: Color32 = Color32::from_gray(185);

/// A tangent handle of a selected key, in screen space.
struct Handle {
    key: KeyRef,
    side: Side,
    key_pos: Pos2,
    pos: Pos2,
    /// A user slope (vs. automatic).
    user: bool,
}

/// A selected key: (param name, frame, channel).
type KeyRef = (String, i64, usize);

/// One plotted curve: a channel of an animated param.
struct Channel {
    param: String,
    index: usize,
    label: String,
    color: Color32,
}

enum Drag {
    /// Moving the selection. `origin` holds each touched param's value at
    /// drag start; every frame the move is re-applied from it, so the result
    /// depends only on the total offset.
    Move { origin: HashMap<String, Value>, start: Pos2 },
    Box { start: Pos2 },
    /// Dragging a tangent handle; re-applied from `origin` each frame.
    Tangent { key: KeyRef, side: Side, origin: Value },
}

#[derive(Clone, Copy)]
enum TangentAction {
    Auto,
    Flat,
    Broken(bool),
}

pub struct CurveEditor {
    /// Frame at the left edge, value at the bottom edge.
    t0: f64,
    v0: f64,
    /// Points per frame and per value unit.
    sx: f64,
    sy: f64,
    node: Option<String>,
    hidden: BTreeSet<(String, usize)>,
    selection: BTreeSet<KeyRef>,
    drag: Option<Drag>,
    /// Current (frames, value) offset while moving keys.
    drag_offset: (i64, f64),
    fit_pending: bool,
    pub rect: Rect,
}

impl Default for CurveEditor {
    fn default() -> Self {
        CurveEditor {
            t0: 0.0,
            v0: -1.0,
            sx: 4.0,
            sy: 50.0,
            node: None,
            hidden: BTreeSet::new(),
            selection: BTreeSet::new(),
            drag: None,
            drag_offset: (0, 0.0),
            fit_pending: true,
            rect: Rect::NOTHING,
        }
    }
}

fn channel_names(kind: ParamKind) -> &'static [&'static str] {
    match kind {
        ParamKind::Vec2 => &["x", "y"],
        ParamKind::Vec3 => &["r", "g", "b"],
        ParamKind::Color => &["r", "g", "b", "a"],
        _ => &[""],
    }
}

fn channel_color(name: &str, param_index: usize) -> Color32 {
    match name {
        "r" | "x" => Color32::from_rgb(235, 90, 85),
        "g" | "y" => Color32::from_rgb(110, 200, 100),
        "b" => Color32::from_rgb(90, 150, 240),
        "a" => Color32::from_gray(200),
        // Scalars: a distinct hue per param.
        _ => [KEY_COLOR, Color32::from_rgb(90, 200, 220), Color32::from_rgb(210, 130, 230), Color32::from_rgb(230, 230, 110)]
            [param_index % 4],
    }
}

/// A "nice" grid step (1, 2 or 5 × 10^k) at least `min` units apart.
fn nice_step(min: f64) -> f64 {
    let p = 10f64.powf(min.max(1e-12).log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * p).find(|s| *s >= min).unwrap_or(10.0 * p)
}

fn fmt_value(v: f64, step: f64) -> String {
    let decimals = (-step.log10().floor()).max(0.0) as usize;
    format!("{v:.decimals$}")
}

impl CurveEditor {
    pub fn fit(&mut self) {
        self.fit_pending = true;
    }

    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    fn x(&self, f: f64) -> f32 {
        self.rect.left() + ((f - self.t0) * self.sx) as f32
    }

    fn y(&self, v: f64) -> f32 {
        self.rect.bottom() - ((v - self.v0) * self.sy) as f32
    }

    fn frame_at(&self, x: f32) -> f64 {
        self.t0 + (x - self.rect.left()) as f64 / self.sx
    }

    fn value_at(&self, y: f32) -> f64 {
        self.v0 + (self.rect.bottom() - y) as f64 / self.sy
    }

    /// Animated params of the current node, one entry per channel.
    fn channels(&self, doc: &Doc) -> Vec<Channel> {
        let Some(node) = self.node.as_deref().and_then(|id| doc.node(id)) else { return Vec::new() };
        let Some(info) = ops::info(&node.op) else { return Vec::new() };
        let mut out = Vec::new();
        for (pi, spec) in info.params.iter().filter(|s| s.animatable).enumerate() {
            // Point lists have a channel per coordinate; they're keyed in the viewer.
            if matches!(spec.kind, ParamKind::Points { .. }) || !anim::is_animated(&doc.param(&node.id, spec.name)) {
                continue;
            }
            for (ci, name) in channel_names(spec.kind).iter().enumerate() {
                let label = if name.is_empty() { spec.label.to_string() } else { format!("{} {name}", spec.label) };
                out.push(Channel { param: spec.name.to_string(), index: ci, label, color: channel_color(name, pi) });
            }
        }
        out
    }

    /// Draw the editor. Returns a frame to seek to (ruler scrubbing).
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, frame: i64) -> Option<i64> {
        if doc.selected != self.node {
            self.node = doc.selected.clone();
            self.selection.clear();
            self.drag = None;
            self.fit_pending = true;
        }
        let channels = self.channels(doc);
        // Drop selections that no longer exist (undo, deleted keys...).
        let keys_now: BTreeSet<KeyRef> = self.all_keys(doc, &channels).into_iter().collect();
        if self.drag.is_none() {
            self.selection.retain(|k| keys_now.contains(k));
        }

        let mut seek = None;
        self.toolbar(ui, doc, &channels);
        ui.horizontal_top(|ui| {
            self.channel_list(ui, &channels);
            ui.vertical(|ui| {
                seek = self.ruler(ui, doc, frame);
                self.canvas(ui, doc, &channels, frame);
            });
        });
        seek
    }

    fn all_keys(&self, doc: &Doc, channels: &[Channel]) -> Vec<KeyRef> {
        let Some(id) = &self.node else { return Vec::new() };
        channels
            .iter()
            .flat_map(|c| {
                anim::key_frames(&doc.param(id, &c.param)).into_iter().map(move |f| (c.param.clone(), f, c.index))
            })
            .collect()
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, doc: &mut Doc, channels: &[Channel]) {
        ui.horizontal(|ui| {
            if ui.button("Frame all").on_hover_text("F").clicked() {
                self.fit();
            }
            ui.separator();
            let params = self.target_params(channels);
            ui.add_enabled_ui(!params.is_empty(), |ui| {
                for (interp, label) in [(Interp::Smooth, "Smooth"), (Interp::Linear, "Linear"), (Interp::Step, "Step")] {
                    let tip = "Interpolation for the selected keys' curves (all shown curves if none selected)";
                    if ui.button(label).on_hover_text(tip).clicked() {
                        self.set_interp(doc, &params, interp);
                    }
                }
            });
            ui.separator();
            ui.add_enabled_ui(self.has_selection(), |ui| {
                ui.label("Tangents");
                if ui.button("Auto").on_hover_text("Automatic slopes for the selected keys").clicked() {
                    self.tangent_action(doc, TangentAction::Auto);
                }
                if ui.button("Flat").on_hover_text("Zero slope: the curve levels out at the selected keys").clicked() {
                    self.tangent_action(doc, TangentAction::Flat);
                }
                let broken = self.selection_broken(doc);
                let (label, tip) = if broken {
                    ("Unify", "Make in and out tangents move together again")
                } else {
                    ("Break", "Let in and out tangents move independently (or ⌥-drag a handle)")
                };
                if ui.button(label).on_hover_text(tip).clicked() {
                    self.tangent_action(doc, TangentAction::Broken(!broken));
                }
            });
            ui.separator();
            if ui.add_enabled(self.has_selection(), egui::Button::new("Delete keys")).on_hover_text("⌫").clicked() {
                self.delete_selected(doc);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("Double-click a curve to add a key · drag keys to move · ⌘-scroll to zoom");
            });
        });
    }

    /// Params the toolbar/menu acts on: those with selected keys, else all shown.
    fn target_params(&self, channels: &[Channel]) -> Vec<String> {
        let mut params: Vec<String> = if self.selection.is_empty() {
            channels.iter().map(|c| c.param.clone()).collect()
        } else {
            self.selection.iter().map(|k| k.0.clone()).collect()
        };
        params.dedup();
        params.sort();
        params.dedup();
        params
    }

    fn set_interp(&self, doc: &mut Doc, params: &[String], interp: Interp) {
        if let Some(id) = &self.node {
            for p in params {
                doc.set_interp(id, p, interp);
            }
        }
    }

    pub fn delete_selected(&mut self, doc: &mut Doc) {
        let Some(id) = self.node.clone() else { return };
        let frames: BTreeSet<(String, i64)> = self.selection.iter().map(|(p, f, _)| (p.clone(), *f)).collect();
        for (p, f) in frames {
            doc.remove_key(&id, &p, f);
        }
        self.selection.clear();
    }

    fn channel_list(&mut self, ui: &mut egui::Ui, channels: &[Channel]) {
        ui.vertical(|ui| {
            ui.set_width(LIST_W);
            if channels.is_empty() {
                ui.weak("No animated parameters.\nSet keys with ◇ in Properties.");
                return;
            }
            egui::ScrollArea::vertical().id_salt("curve_channels").show(ui, |ui| {
                for c in channels {
                    let key = (c.param.clone(), c.index);
                    let mut shown = !self.hidden.contains(&key);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, c.color);
                        let resp = ui.checkbox(&mut shown, c.label.as_str());
                        if resp.changed() {
                            // Re-frame so the remaining curves fill the view.
                            self.fit_pending = true;
                            if shown {
                                self.hidden.remove(&key);
                            } else {
                                self.hidden.insert(key.clone());
                                self.selection.retain(|k| (k.0.as_str(), k.2) != (c.param.as_str(), c.index));
                            }
                        }
                    });
                }
            });
        });
    }

    fn ruler(&mut self, ui: &mut egui::Ui, doc: &Doc, frame: i64) -> Option<i64> {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), RULER_H), Sense::click_and_drag());
        // Share the canvas's horizontal mapping (canvas rect is from last frame;
        // its left/width match the ruler's).
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(36));
        let step = nice_step(50.0 / self.sx).max(1.0).round();
        let mut f = (self.frame_at(rect.left()) / step).floor() * step;
        while self.x(f) < rect.right() {
            let x = self.x(f);
            painter.line_segment([Pos2::new(x, rect.bottom() - 5.0), Pos2::new(x, rect.bottom())], (1.0, Color32::from_gray(100)));
            painter.text(Pos2::new(x + 2.0, rect.top() + 2.0), Align2::LEFT_TOP, f as i64, FontId::proportional(10.0), Color32::from_gray(150));
            f += step;
        }
        let p = &doc.graph.project;
        for edge in [p.first_frame, p.last_frame] {
            let x = self.x(edge as f64);
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], (1.0, Color32::from_gray(80)));
        }
        let x = self.x(frame as f64);
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, Color32::from_rgb(240, 160, 40)));
        let pos = resp.interact_pointer_pos()?;
        (resp.clicked() || resp.dragged()).then(|| self.frame_at(pos.x).round() as i64)
    }

    fn canvas(&mut self, ui: &mut egui::Ui, doc: &mut Doc, channels: &[Channel], frame: i64) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(26));
        let Some(id) = self.node.clone() else {
            painter.text(rect.center(), Align2::CENTER_CENTER, "Select a node to see its curves", FontId::proportional(14.0), Color32::GRAY);
            return;
        };
        let visible: Vec<&Channel> = channels.iter().filter(|c| !self.hidden.contains(&(c.param.clone(), c.index))).collect();
        if self.fit_pending {
            self.fit_to(doc, &id, &visible);
        }
        self.navigate(ui, rect);
        self.draw_grid(&painter, doc);

        // Curves, sampled per column through the engine's own interpolation.
        for c in &visible {
            let raw = doc.param(&id, &c.param);
            let pts: Vec<Pos2> = (0..=(rect.width() as usize / 2))
                .map(|i| {
                    let x = rect.left() + i as f32 * 2.0;
                    let v = anim::sample(&raw, self.frame_at(x));
                    Pos2::new(x, self.y(v.get(c.index).copied().unwrap_or(0.0)))
                })
                .collect();
            painter.add(Shape::line(pts, Stroke::new(1.6, c.color)));
        }

        // Keys.
        let press = ui.input(|i| i.pointer.press_origin());
        let hover = resp.hover_pos();
        let mut key_points: Vec<(KeyRef, Pos2, Color32)> = Vec::new();
        for c in &visible {
            for k in anim::keys(&doc.param(&id, &c.param)) {
                let p = Pos2::new(self.x(k.frame as f64), self.y(k.value.get(c.index).copied().unwrap_or(0.0)));
                key_points.push(((c.param.clone(), k.frame, c.index), p, c.color));
            }
        }
        let near = |at: Option<Pos2>| -> Option<KeyRef> {
            let at = at?;
            key_points
                .iter()
                .filter(|(_, p, _)| p.distance(at) <= HIT_R)
                .min_by(|a, b| a.1.distance(at).total_cmp(&b.1.distance(at)))
                .map(|(k, ..)| k.clone())
        };
        let hovered_key = near(hover);

        // Tangent handles of selected keys on smooth curves (hidden while moving keys).
        let handles: Vec<Handle> = if matches!(self.drag, Some(Drag::Move { .. })) {
            Vec::new()
        } else {
            self.handles(doc, &id, &visible)
        };
        let near_handle = |at: Option<Pos2>| -> Option<usize> {
            let at = at?;
            handles
                .iter()
                .enumerate()
                .filter(|(_, h)| h.pos.distance(at) <= HIT_R)
                .min_by(|a, b| a.1.pos.distance(at).total_cmp(&b.1.pos.distance(at)))
                .map(|(i, _)| i)
        };
        let hovered_handle = near_handle(hover);

        // --- Interaction ---
        let shift = ui.input(|i| i.modifiers.shift);
        let alt = ui.input(|i| i.modifiers.alt);
        if resp.drag_started() {
            if let Some(h) = near_handle(press).map(|i| &handles[i]) {
                let (param, frame, _) = &h.key;
                let mut origin = doc.param(&id, param);
                if alt {
                    origin = anim::set_broken(&origin, *frame, true);
                }
                self.drag = Some(Drag::Tangent { key: h.key.clone(), side: h.side, origin });
            } else {
                self.start_key_drag(doc, &id, near(press), press, shift);
            }
        }
        if resp.clicked() && near_handle(resp.interact_pointer_pos()).is_none() {
            match near(resp.interact_pointer_pos()) {

                Some(k) if shift => {
                    if !self.selection.remove(&k) {
                        self.selection.insert(k);
                    }
                }
                Some(k) => {
                    self.selection.clear();
                    self.selection.insert(k);
                }
                None if !shift => self.selection.clear(),
                None => {}
            }
        }
        if resp.secondary_clicked() {
            if let Some(k) = near(resp.interact_pointer_pos()) {
                if !self.selection.contains(&k) {
                    self.selection.clear();
                    self.selection.insert(k);
                }
            }
        }
        if resp.double_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.add_key_near(doc, &id, &visible, pos);
            }
        }

        let pointer = ui.input(|i| i.pointer.interact_pos());
        match (&self.drag, pointer) {
            (Some(Drag::Move { origin, start }), Some(ptr)) if resp.dragged() => {
                let df = ((ptr.x - start.x) as f64 / self.sx).round() as i64;
                let dv = -(ptr.y - start.y) as f64 / self.sy;
                if (df, dv) != self.drag_offset {
                    self.drag_offset = (df, dv);
                    let origin = origin.clone();
                    self.apply_move(doc, &id, &origin, df, dv);
                }
            }
            (Some(Drag::Tangent { key: (param, f, c), side, origin }), Some(ptr)) if resp.dragged() => {
                let key_v = anim::keys(origin).into_iter().find(|k| k.frame == *f).and_then(|k| k.value.get(*c).copied());
                if let Some(key_v) = key_v {
                    // Slope from the key to the pointer, kept on the handle's side.
                    let min_df = 1.0 / self.sx;
                    let df = self.frame_at(ptr.x) - *f as f64;
                    let df = if *side == Side::Out { df.max(min_df) } else { df.min(-min_df) };
                    let slope = (self.value_at(ptr.y) - key_v) / df;
                    let v = anim::set_tangent(origin, *f, *c, *side, slope);
                    doc.set_param(&id, param, v);
                }
            }
            (Some(Drag::Box { start }), Some(ptr)) if resp.dragged() => {
                let r = Rect::from_two_pos(*start, ptr);
                painter.rect(r, 0.0, Color32::from_rgba_unmultiplied(240, 175, 50, 20), Stroke::new(1.0, KEY_COLOR), egui::StrokeKind::Inside);
            }
            _ => {}
        }
        if resp.drag_stopped() {
            match self.drag.take() {
                Some(Drag::Move { .. }) => {
                    let df = self.drag_offset.0;
                    self.selection = std::mem::take(&mut self.selection).into_iter().map(|(p, f, c)| (p, f + df, c)).collect();
                }
                Some(Drag::Box { start }) => {
                    if let Some(end) = pointer {
                        let r = Rect::from_two_pos(start, end);
                        if !shift {
                            self.selection.clear();
                        }
                        self.selection.extend(key_points.iter().filter(|(_, p, _)| r.contains(*p)).map(|(k, ..)| k.clone()));
                    }
                }
                Some(Drag::Tangent { .. }) | None => {}
            }
            self.drag_offset = (0, 0.0);
        }

        // Handles, beneath the keys. Recomputed so they follow a drag in progress.
        let dragging_tangent = match &self.drag {
            Some(Drag::Tangent { key, side, .. }) => Some((key.clone(), *side)),
            _ => None,
        };
        let live = if matches!(self.drag, Some(Drag::Move { .. })) { Vec::new() } else { self.handles(doc, &id, &visible) };
        for (i, h) in live.iter().enumerate() {
            let hot = dragging_tangent.as_ref().is_some_and(|(k, s)| *k == h.key && *s == h.side)
                || (dragging_tangent.is_none() && hovered_handle == Some(i));
            let color = if hot { KEY_COLOR } else { HANDLE_COLOR };
            painter.line_segment([h.key_pos, h.pos], Stroke::new(1.0, color));
            let r = if hot { 4.5 } else { 3.5 };
            if h.user {
                painter.circle_filled(h.pos, r, color);
            } else {
                painter.circle_stroke(h.pos, r, Stroke::new(1.5, color));
            }
        }

        // Draw keys (after the move so they show their new positions).
        let moving = matches!(self.drag, Some(Drag::Move { .. }));
        let df = if moving { self.drag_offset.0 } else { 0 };
        for c in &visible {
            for k in anim::keys(&doc.param(&id, &c.param)) {
                let p = Pos2::new(self.x(k.frame as f64), self.y(k.value.get(c.index).copied().unwrap_or(0.0)));
                let kref = (c.param.clone(), k.frame - df, c.index);
                let selected = self.selection.contains(&kref);
                let hot = hovered_key.as_ref().is_some_and(|h| *h == (c.param.clone(), k.frame, c.index));
                let r = if hot { KEY_R + 1.5 } else { KEY_R };
                let quad = vec![p + Vec2::new(0.0, -r), p + Vec2::new(r, 0.0), p + Vec2::new(0.0, r), p + Vec2::new(-r, 0.0)];
                let (fill, stroke) = if selected {
                    (Color32::WHITE, Stroke::new(1.5, KEY_COLOR))
                } else {
                    (c.color, Stroke::new(1.0, Color32::from_black_alpha(200)))
                };
                painter.add(Shape::convex_polygon(quad, fill, stroke));
            }
        }

        // Playhead.
        let x = self.x(frame as f64);
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, Color32::from_rgba_unmultiplied(240, 160, 40, 160)));

        // Readout for the handle being dragged, or the key being moved or hovered.
        if let (Some(((p, f, c), side)), Some(ptr)) = (&dragging_tangent, pointer) {
            if let Some((tin, tout)) = anim::slopes(&doc.param(&id, p), *f) {
                let slope = if *side == Side::In { tin[*c] } else { tout[*c] };
                let label = channels.iter().find(|ch| &ch.param == p && ch.index == *c).map_or(p.clone(), |ch| ch.label.clone());
                let which = if *side == Side::In { "in" } else { "out" };
                let text = format!("{label}   frame {f}   {which} slope {slope:.4}/frame");
                let galley = painter.layout_no_wrap(text, FontId::monospace(11.0), Color32::from_gray(230));
                let r = Align2::LEFT_BOTTOM.anchor_size(ptr + Vec2::new(12.0, -10.0), galley.size());
                painter.rect_filled(r.expand(3.0), 3.0, Color32::from_black_alpha(190));
                painter.galley(r.min, galley, Color32::WHITE);
            }
        }
        let readout = if dragging_tangent.is_some() {
            None
        } else if moving {
            self.selection.iter().next().cloned().map(|(p, f, c)| (p, f + df, c))
        } else {
            hovered_key.clone()
        };
        if let Some((p, f, c)) = readout {
            let v = anim::keys(&doc.param(&id, &p)).into_iter().find(|k| k.frame == f).and_then(|k| k.value.get(c).copied());
            if let (Some(v), Some(ptr)) = (v, pointer.or(hover)) {
                let label = channels.iter().find(|ch| ch.param == p && ch.index == c).map_or(p.clone(), |ch| ch.label.clone());
                let text = format!("{label}   frame {f}   {v:.4}");
                let galley = painter.layout_no_wrap(text, FontId::monospace(11.0), Color32::from_gray(230));
                let r = Align2::LEFT_BOTTOM.anchor_size(ptr + Vec2::new(12.0, -10.0), galley.size());
                painter.rect_filled(r.expand(3.0), 3.0, Color32::from_black_alpha(190));
                painter.galley(r.min, galley, Color32::WHITE);
            }
        }

        if visible.is_empty() {
            painter.text(rect.center(), Align2::CENTER_CENTER, format!("{id} has no visible animated parameters"), FontId::proportional(14.0), Color32::GRAY);
        }

        resp.context_menu(|ui| {
            let params = self.target_params(channels);
            ui.add_enabled_ui(!params.is_empty(), |ui| {
                ui.label("Interpolation");
                for (interp, label) in [(Interp::Smooth, "Smooth"), (Interp::Linear, "Linear"), (Interp::Step, "Step")] {
                    if ui.button(label).clicked() {
                        self.set_interp(doc, &params, interp);
                        ui.close();
                    }
                }
            });
            ui.separator();
            ui.add_enabled_ui(self.has_selection(), |ui| {
                ui.label("Tangents");
                if ui.button("Auto").clicked() {
                    self.tangent_action(doc, TangentAction::Auto);
                    ui.close();
                }
                if ui.button("Flat").clicked() {
                    self.tangent_action(doc, TangentAction::Flat);
                    ui.close();
                }
                let broken = self.selection_broken(doc);
                if ui.button(if broken { "Unify" } else { "Break" }).clicked() {
                    self.tangent_action(doc, TangentAction::Broken(!broken));
                    ui.close();
                }
            });
            ui.separator();
            if ui.add_enabled(self.has_selection(), egui::Button::new("Delete keys")).clicked() {
                self.delete_selected(doc);
                ui.close();
            }
            if ui.button("Frame all").clicked() {
                self.fit();
                ui.close();
            }
        });
        if self.drag.is_some() {
            ui.ctx().request_repaint();
        }
    }

    fn start_key_drag(&mut self, doc: &Doc, id: &str, hit: Option<KeyRef>, press: Option<Pos2>, shift: bool) {
        let start = press.unwrap_or_default();
        let Some(k) = hit else {
            self.drag = Some(Drag::Box { start });
            return;
        };
        if !self.selection.contains(&k) {
            if !shift {
                self.selection.clear();
            }
            self.selection.insert(k);
        }
        let origin = self.selection.iter().map(|(p, ..)| (p.clone(), doc.param(id, p))).collect();
        self.drag = Some(Drag::Move { origin, start });
        self.drag_offset = (0, 0.0);
    }

    /// In/out handles for every selected key whose curve is smooth.
    fn handles(&self, doc: &Doc, id: &str, visible: &[&Channel]) -> Vec<Handle> {
        let mut out = Vec::new();
        for (param, frame, c) in &self.selection {
            if !visible.iter().any(|ch| &ch.param == param && ch.index == *c) {
                continue;
            }
            let raw = doc.param(id, param);
            if anim::interp(&raw) != Some(Interp::Smooth) {
                continue;
            }
            let (Some((tin, tout)), Some(key)) =
                (anim::slopes(&raw, *frame), anim::keys(&raw).into_iter().find(|k| k.frame == *frame))
            else {
                continue;
            };
            let Some(&v) = key.value.get(*c) else { continue };
            let key_pos = Pos2::new(self.x(*frame as f64), self.y(v));
            for (side, slope, user) in [
                (Side::In, tin[*c], key.tan_in.get(*c).copied().flatten().is_some()),
                (Side::Out, tout[*c], key.tan_out.get(*c).copied().flatten().is_some()),
            ] {
                // Direction of (1 frame, slope) on screen, at a fixed length.
                let dir = Vec2::new(self.sx as f32, -(slope * self.sy) as f32).normalized();
                let dir = if side == Side::In { -dir } else { dir };
                out.push(Handle { key: (param.clone(), *frame, *c), side, key_pos, pos: key_pos + dir * HANDLE_LEN, user });
            }
        }
        out
    }

    /// True if every selected key is broken.
    fn selection_broken(&self, doc: &Doc) -> bool {
        let Some(id) = &self.node else { return false };
        !self.selection.is_empty() && self.selection.iter().all(|(p, f, _)| anim::is_broken(&doc.param(id, p), *f))
    }

    /// Apply a tangent change to the selected keys. Tangents only shape smooth
    /// curves, so affected curves are switched to smooth.
    fn tangent_action(&self, doc: &mut Doc, action: TangentAction) {
        let Some(id) = self.node.clone() else { return };
        for (p, f, c) in &self.selection {
            let raw = doc.param(&id, p);
            let v = match action {
                TangentAction::Auto => anim::set_auto_tangent(&raw, *f, *c),
                TangentAction::Flat => {
                    let v = anim::set_tangent(&raw, *f, *c, Side::In, 0.0);
                    anim::set_tangent(&v, *f, *c, Side::Out, 0.0)
                }
                TangentAction::Broken(b) => anim::set_broken(&raw, *f, b),
            };
            doc.set_param(&id, p, anim::set_interp(&v, Interp::Smooth));
        }
    }

    /// Re-apply a move of the selected keys by (`df` frames, `dv` value) to
    /// each param's value as it was when the drag started.
    fn apply_move(&self, doc: &mut Doc, id: &str, origin: &HashMap<String, Value>, df: i64, dv: f64) {
        for (param, raw) in origin {
            // Channels selected per original frame of this param.
            let mut moving: HashMap<i64, Vec<usize>> = HashMap::new();
            for (p, f, c) in &self.selection {
                if p == param {
                    moving.entry(*f).or_default().push(*c);
                }
            }
            let mut stay = Vec::new();
            let mut moved = Vec::new();
            for mut k in anim::keys(raw) {
                match moving.get(&k.frame) {
                    Some(chs) => {
                        for &c in chs {
                            if let Some(x) = k.value.get_mut(c) {
                                *x += dv;
                            }
                        }
                        k.frame += df;
                        moved.push(k);
                    }
                    None => stay.push(k),
                }
            }
            // Moved keys land on (and replace) any unselected key at their new frame.
            let mut all: Vec<Key> = stay;
            all.extend(moved);
            doc.set_param(id, param, anim::replace_keys(raw, all));
        }
    }

    /// Double-click: key the param whose curve passes closest to the click,
    /// at the clicked frame, with the curve's current value there.
    fn add_key_near(&mut self, doc: &mut Doc, id: &str, visible: &[&Channel], pos: Pos2) {
        let f = self.frame_at(pos.x).round() as i64;
        let best = visible.iter().min_by(|a, b| {
            let d = |c: &Channel| {
                let v = anim::sample(&doc.param(id, &c.param), f as f64);
                (self.y(v.get(c.index).copied().unwrap_or(0.0)) - pos.y).abs()
            };
            d(a).total_cmp(&d(b))
        });
        if let Some(c) = best {
            doc.add_key(id, &c.param, f);
            self.selection.clear();
            self.selection.insert((c.param.clone(), f, c.index));
        }
    }

    fn navigate(&mut self, ui: &egui::Ui, rect: Rect) {
        let Some(hover) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)) else { return };
        let (scroll, zoom, mods) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
        let middle = ui.input(|i| i.pointer.middle_down());
        if middle {
            let d = ui.input(|i| i.pointer.delta());
            self.t0 -= d.x as f64 / self.sx;
            self.v0 += d.y as f64 / self.sy;
        }
        if zoom != 1.0 {
            let (f, v) = (self.frame_at(hover.x), self.value_at(hover.y));
            let z = zoom as f64;
            if !mods.alt {
                self.sx = (self.sx * z).clamp(0.05, 400.0);
            }
            if !mods.shift {
                self.sy = (self.sy * z).clamp(1e-4, 1e6);
            }
            // Keep the point under the cursor fixed.
            self.t0 = f - (hover.x - rect.left()) as f64 / self.sx;
            self.v0 = v - (rect.bottom() - hover.y) as f64 / self.sy;
        } else if scroll != Vec2::ZERO {
            self.t0 -= scroll.x as f64 / self.sx;
            self.v0 += scroll.y as f64 / self.sy;
        }
    }

    fn fit_to(&mut self, doc: &Doc, id: &str, visible: &[&Channel]) {
        self.fit_pending = false;
        if !self.rect.is_positive() {
            return;
        }
        let p = &doc.graph.project;
        let (mut f0, mut f1) = (p.first_frame as f64, p.last_frame as f64);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in visible {
            let raw = doc.param(id, &c.param);
            let frames = anim::key_frames(&raw);
            if let (Some(a), Some(b)) = (frames.first(), frames.last()) {
                f0 = f0.min(*a as f64);
                f1 = f1.max(*b as f64);
            }
        }
        // Sample, so smooth-curve overshoot between keys stays in view.
        for c in visible {
            let raw = doc.param(id, &c.param);
            for i in 0..=200 {
                let v = anim::sample(&raw, f0 + (f1 - f0) * i as f64 / 200.0);
                if let Some(x) = v.get(c.index) {
                    lo = lo.min(*x);
                    hi = hi.max(*x);
                }
            }
        }
        if !lo.is_finite() {
            (lo, hi) = (0.0, 1.0);
        }
        if hi - lo < 1e-6 {
            let pad = lo.abs().max(1.0) * 0.5;
            (lo, hi) = (lo - pad, hi + pad);
        }
        let (fpad, vpad) = ((f1 - f0).max(1.0) * 0.04, (hi - lo) * 0.12);
        self.sx = self.rect.width() as f64 / ((f1 - f0) + 2.0 * fpad).max(1.0);
        self.sy = self.rect.height() as f64 / ((hi - lo) + 2.0 * vpad);
        self.t0 = f0 - fpad;
        self.v0 = lo - vpad;
    }

    fn draw_grid(&self, painter: &egui::Painter, doc: &Doc) {
        let r = self.rect;
        // Project range.
        let p = &doc.graph.project;
        let shade = Rect::from_x_y_ranges(self.x(p.first_frame as f64)..=self.x(p.last_frame as f64), r.y_range());
        painter.rect_filled(shade.intersect(r), 0.0, Color32::from_gray(31));

        let fstep = nice_step(50.0 / self.sx).max(1.0).round();
        let mut f = (self.frame_at(r.left()) / fstep).floor() * fstep;
        while self.x(f) < r.right() {
            let x = self.x(f);
            painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], (1.0, Color32::from_gray(40)));
            f += fstep;
        }
        let vstep = nice_step(28.0 / self.sy);
        let mut v = (self.value_at(r.bottom()) / vstep).floor() * vstep;
        while self.y(v) > r.top() {
            let y = self.y(v);
            let color = if v.abs() < vstep * 0.5 { Color32::from_gray(70) } else { Color32::from_gray(40) };
            painter.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], (1.0, color));
            painter.text(Pos2::new(r.left() + 4.0, y - 1.0), Align2::LEFT_BOTTOM, fmt_value(v, vstep), FontId::proportional(10.0), Color32::from_gray(120));
            v += vstep;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;
    use serde_json::json;

    #[test]
    fn moving_keys_shifts_time_and_only_selected_channels() {
        let mut doc = Doc::new(Graph::default(), None);
        let id = doc.add_node("transform", [0.0, 0.0]);
        let curve = json!({ "keys": [[1, [0.0, 0.0]], [10, [5.0, 5.0]], [20, [9.0, 9.0]]], "interp": "linear" });
        doc.set_param(&id, "translate", curve.clone());

        let mut ed = CurveEditor { node: Some(id.clone()), ..Default::default() };
        // Select only the x channel of the key at frame 1.
        ed.selection.insert(("translate".into(), 1, 0));
        let origin = HashMap::from([("translate".to_string(), curve)]);

        ed.apply_move(&mut doc, &id, &origin, 4, 2.0);
        let ks = anim::keys(&doc.param(&id, "translate"));
        assert_eq!(ks.iter().map(|k| k.frame).collect::<Vec<_>>(), vec![5, 10, 20]);
        assert_eq!(ks[0].value, vec![2.0, 0.0]); // x moved, y kept

        // Re-applied from the origin, not cumulatively; landing on frame 10 replaces it.
        ed.apply_move(&mut doc, &id, &origin, 9, 0.0);
        let ks = anim::keys(&doc.param(&id, "translate"));
        assert_eq!(ks.iter().map(|k| k.frame).collect::<Vec<_>>(), vec![10, 20]);
        assert_eq!(ks[0].value, vec![0.0, 0.0]);
    }

    #[test]
    fn tangent_actions_and_handles() {
        let mut doc = Doc::new(Graph::default(), None);
        let id = doc.add_node("blur", [0.0, 0.0]);
        doc.set_param(&id, "radius", json!({ "keys": [[0, 0.0], [10, 10.0], [20, 0.0]], "interp": "linear" }));
        let mut ed = CurveEditor { node: Some(id.clone()), ..Default::default() };
        ed.rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0));
        ed.selection.insert(("radius".into(), 0, 0));
        let ch = Channel { param: "radius".into(), index: 0, label: String::new(), color: Color32::WHITE };

        // Linear curves have no handles; a tangent action switches to smooth.
        assert!(ed.handles(&doc, &id, &[&ch]).is_empty());
        ed.tangent_action(&mut doc, TangentAction::Flat);
        assert_eq!(anim::interp(&doc.param(&id, "radius")), Some(Interp::Smooth));
        let hs = ed.handles(&doc, &id, &[&ch]);
        assert_eq!(hs.len(), 2);
        // Flat: handles are horizontal, out to the right, in to the left, user-set.
        let out = hs.iter().find(|h| h.side == Side::Out).unwrap();
        let inn = hs.iter().find(|h| h.side == Side::In).unwrap();
        assert!((out.pos.y - out.key_pos.y).abs() < 1e-3 && out.pos.x > out.key_pos.x && out.user);
        assert!(inn.pos.x < inn.key_pos.x);

        // A rising slope points the out handle up (smaller screen y).
        let v = anim::set_tangent(&doc.param(&id, "radius"), 0, 0, Side::Out, 2.0);
        doc.set_param(&id, "radius", v);
        let hs = ed.handles(&doc, &id, &[&ch]);
        let out = hs.iter().find(|h| h.side == Side::Out).unwrap();
        assert!(out.pos.y < out.key_pos.y);

        ed.tangent_action(&mut doc, TangentAction::Broken(true));
        assert!(ed.selection_broken(&doc));
        ed.tangent_action(&mut doc, TangentAction::Auto);
        let hs = ed.handles(&doc, &id, &[&ch]);
        assert!(hs.iter().all(|h| !h.user));
    }
}
