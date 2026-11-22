//! Pen mode in the 2D viewer: draw a Roto point by point.
//!
//! On while the selected node is an open Roto (`closed` off, as a Roto
//! added from the node menu starts), or turned on with P / the toolbar's
//! Pen button to start a new one. Each click adds a bezier point at the
//! end of the path; dragging on pulls out its tangents (symmetric; Alt
//! moves only the out tangent, breaking them). Clicking the first point
//! (with 3 or more), or Enter, closes the shape and leaves pen mode; Esc
//! or ⌫ removes the last point, and Esc with none left leaves pen mode
//! (the empty Roto stays; undo removes it). A press on another existing
//! point goes to the usual handles, so points can be moved while drawing.
//!
//! With no open Roto selected, the first click asks the app for a new Roto
//! ([`PenMode::take_new_shape`]: added as the node menu adds one, so it
//! takes the selected node as its background) and becomes its first point
//! once it appears.

use crate::doc::Doc;
use crate::overlay::Xf;
use arclight::anim;
use eframe::egui::{self, Color32, FontId, Key, Pos2, Response, Shape, Stroke, Vec2};
use serde_json::{json, Value};

const COLOR: Color32 = Color32::from_rgb(120, 220, 255);
pub const HINT: &str = "Pen: click to add a point, drag for tangents (Alt breaks) · click the first point or Enter to close · Esc / ⌫ removes the last";
/// Screen distance that counts as on a point.
const GRAB: f32 = 7.0;
/// Fewest points a closed shape needs.
const MIN_CLOSE: usize = 3;
/// Screen distance a press must move before it pulls out tangents.
const PULL_AFTER: f32 = 2.0;

#[derive(Default)]
pub struct PenMode {
    on: bool,
    /// The open Roto pen mode was left on (Esc, P): it isn't taken up again
    /// until something else is selected.
    dismissed: Option<String>,
    /// A first click (screen), waiting for its new Roto: placed in that
    /// Roto's own space once it exists (it may be seen through what's
    /// downstream of it).
    pending: Option<Pos2>,
    /// The app should add a Roto (see [`PenMode::take_new_shape`]).
    want_shape: bool,
    /// Pulling the tangents of (node, vertex) out from where it was pressed
    /// (screen).
    pull: Option<(String, usize, Pos2)>,
    /// This press belongs to the usual handles (an existing point).
    passthrough: bool,
    /// The toolbar's Pen button was clicked (handled with the doc in `ui`).
    button: bool,
}

/// The selected node, when it's a Roto still being drawn.
pub fn open_roto(doc: &Doc) -> Option<String> {
    let id = doc.selected.clone()?;
    let n = doc.node(&id)?;
    (n.op == "roto" && doc.param(&id, "closed") == Value::Bool(false)).then_some(id)
}

/// A Roto's flat points (stride 6) at `frame`.
fn flat(doc: &Doc, id: &str, frame: i64) -> Vec<f64> {
    doc.param_at(id, "points", frame).as_array().map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()).unwrap_or_default()
}

/// `v` (stride 6) with a vertex at `p`, no tangents, added at the end.
pub fn push_vertex(v: &[f64], p: [f64; 2]) -> Vec<f64> {
    let mut out = v[..v.len() - v.len() % 6].to_vec();
    out.extend_from_slice(&[p[0], p[1], 0.0, 0.0, 0.0, 0.0]);
    out
}

/// `v` (stride 6) without its last vertex.
pub fn pop_vertex(v: &[f64]) -> Vec<f64> {
    let n = v.len() / 6;
    v[..n.saturating_sub(1) * 6].to_vec()
}

/// `v` (stride 6) with vertex `i`'s out tangent set to `out`, and its in
/// tangent mirrored unless `broken`.
pub fn set_tangents(v: &[f64], i: usize, out: [f64; 2], broken: bool) -> Vec<f64> {
    let mut v = v.to_vec();
    if let Some(c) = v.get_mut(i * 6..i * 6 + 6) {
        c[4] = out[0];
        c[5] = out[1];
        if !broken {
            c[2] = -out[0];
            c[3] = -out[1];
        }
    }
    v
}

/// Edit a Roto's points in every key (as the viewer's vertex edits do),
/// keeping a per-point feather list as long as the points.
fn edit(doc: &mut Doc, id: &str, f: impl Fn(&[f64]) -> Vec<f64>) {
    let new = anim::edit_all(&doc.param(id, "points"), &f);
    doc.set_param(id, "points", new);
    let n = anim::sample(&doc.param(id, "points"), 0.0).len() / 6;
    let pf = doc.param(id, "point_feather");
    if anim::is_animated(&pf) || pf.as_array().is_some_and(|a| a.len() > n) {
        doc.set_param(id, "point_feather", anim::edit_all(&pf, |v| v[..v.len().min(n)].to_vec()));
    }
}

impl PenMode {
    /// Whether pen mode is on.
    pub fn active(&self) -> bool {
        self.on
    }

    /// The app should add a new Roto now (once).
    pub fn take_new_shape(&mut self) -> bool {
        std::mem::take(&mut self.want_shape)
    }

    /// The toolbar's Pen button: toggles at the next [`PenMode::ui`].
    pub fn press_button(&mut self) {
        self.button = true;
    }

    /// Turn pen mode on or off (P, the toolbar's Pen button).
    pub fn toggle(&mut self, doc: &Doc) {
        self.on = !self.on;
        if !self.on {
            self.leave(doc);
        } else {
            self.dismissed = None;
        }
    }

    fn leave(&mut self, doc: &Doc) {
        self.on = false;
        self.dismissed = open_roto(doc);
        self.pending = None;
        self.want_shape = false;
        self.pull = None;
        self.passthrough = false;
    }

    /// Close the Roto being drawn, if it has enough points; pen mode ends.
    fn close(&mut self, doc: &mut Doc, id: &str, frame: i64) -> bool {
        if flat(doc, id, frame).len() / 6 < MIN_CLOSE {
            return false;
        }
        doc.set_param(id, "closed", json!(true));
        self.leave(doc);
        self.dismissed = None;
        true
    }

    /// ⌫ while drawing: remove the last point. False when pen mode isn't
    /// drawing anything (so ⌫ does what it otherwise does).
    pub fn delete_last(&mut self, doc: &mut Doc) -> bool {
        let Some(id) = open_roto(doc).filter(|_| self.on) else { return false };
        self.pull = None;
        edit(doc, &id, pop_vertex);
        true
    }

    /// The keys, and while on, the pointer. True while on and the pointer
    /// is the pen's (the overlay's handles and panning stand aside).
    pub fn ui(&mut self, ui: &egui::Ui, resp: &Response, xf: &Xf, doc: &mut Doc, frame: i64) -> bool {
        let open = open_roto(doc);
        // Took up again by any open Roto newly selected.
        if self.dismissed.is_some() && self.dismissed != open {
            self.dismissed = None;
        }
        if open.is_some() && self.dismissed.is_none() {
            self.on = true;
        }
        if !ui.ctx().text_edit_focused() {
            let hovered = resp.hovered();
            let on = self.on;
            let (p, enter, esc) = ui.input_mut(|i| {
                let p = hovered && crate::consume_plain(i, Key::P);
                let enter = on && crate::consume_plain(i, Key::Enter);
                let esc = on && crate::consume_plain(i, Key::Escape);
                (p, enter, esc)
            });
            if p || std::mem::take(&mut self.button) {
                self.toggle(doc);
            }
            if let (true, Some(id)) = (enter, &open) {
                self.close(doc, id, frame);
            }
            if esc {
                match &open {
                    Some(id) if !flat(doc, id, frame).is_empty() => {
                        self.pull = None;
                        edit(doc, id, pop_vertex);
                    }
                    _ => self.leave(doc),
                }
            }
        }
        if !self.on {
            return false;
        }
        let open = open_roto(doc);
        let (pressed, down, alt, origin, latest) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.alt, i.pointer.press_origin(), i.pointer.latest_pos()));
        // A new Roto appeared for the waiting first click (dropped if it's
        // seen through what its handles can't follow).
        if let (Some(o), Some(id)) = (self.pending, &open) {
            self.pending = None;
            if !xf.blocked {
                let at = xf.for_node(doc, id, frame).image(o);
                let n = flat(doc, id, frame).len() / 6;
                edit(doc, id, |v| push_vertex(v, [at.x as f64, at.y as f64]));
                if down {
                    self.pull = Some((id.clone(), n, origin.unwrap_or_default()));
                }
            }
        }
        // Seen through what the handles can't follow (a 3D render, a
        // warp): pen mode stays on but takes no clicks (they pan), and the
        // viewer says why.
        if xf.blocked {
            self.pull = None;
            self.passthrough = false;
            return false;
        }
        if pressed {
            if let Some(o) = origin.filter(|o| resp.rect.contains(*o)) {
                self.press(doc, frame, xf, o, open.as_deref());
            }
        }
        if self.passthrough {
            if !down {
                self.passthrough = false;
            }
            return false;
        }
        if let (Some((id, i, from)), Some(p)) = (self.pull.clone(), latest) {
            if down && p.distance(from) > PULL_AFTER {
                let xf = xf.for_node(doc, &id, frame);
                let v = flat(doc, &id, frame);
                if let Some(c) = v.get(i * 6..i * 6 + 2) {
                    let q = xf.image(p);
                    let out = [q.x as f64 - c[0], q.y as f64 - c[1]];
                    edit(doc, &id, |v| set_tangents(v, i, out, alt));
                }
            }
        }
        if !down {
            self.pull = None;
        }
        ui.ctx().request_repaint();
        true
    }

    fn press(&mut self, doc: &mut Doc, frame: i64, xf: &Xf, o: Pos2, open: Option<&str>) {
        let Some(id) = open else {
            // No Roto to add to: one is made, the click its first point.
            self.pending = Some(o);
            self.want_shape = true;
            return;
        };
        let xf = xf.for_node(doc, id, frame);
        let v = flat(doc, id, frame);
        let pts: Vec<Pos2> = v.as_chunks::<6>().0.iter().map(|c| xf.screen(Vec2::new(c[0] as f32, c[1] as f32))).collect();
        if pts.len() >= MIN_CLOSE && pts[0].distance(o) < GRAB {
            self.close(doc, id, frame);
            return;
        }
        if pts.iter().any(|p| p.distance(o) < GRAB) {
            self.passthrough = true;
            return;
        }
        let at = xf.image(o);
        edit(doc, id, |v| push_vertex(v, [at.x as f64, at.y as f64]));
        self.pull = Some((id.to_string(), pts.len(), o));
    }

    /// While on: the hint, a line from the last point to the pointer, the
    /// first point ringed when a click would close the shape, and the last
    /// point's tangents.
    pub fn draw(&self, ui: &egui::Ui, painter: &egui::Painter, xf: &Xf, doc: &Doc, frame: i64) {
        // (Blocked: the viewer's note says why nothing can be drawn.)
        if !self.on || xf.blocked {
            return;
        }
        let rect = painter.clip_rect();
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
        if let Some(id) = open_roto(doc) {
            let xf = xf.for_node(doc, &id, frame);
            let v = flat(doc, &id, frame);
            let at = |c: &[f64], dx: f64, dy: f64| xf.screen(Vec2::new((c[0] + dx) as f32, (c[1] + dy) as f32));
            let pts: Vec<Pos2> = v.as_chunks::<6>().0.iter().map(|c| at(c, 0.0, 0.0)).collect();
            let shadow = Stroke::new(3.0, Color32::from_black_alpha(140));
            for (k, p) in pts.iter().enumerate() {
                painter.circle(*p, if k == 0 { 4.5 } else { 3.5 }, COLOR, shadow);
            }
            if let (Some(&first), Some(h)) = (pts.first(), hover) {
                if pts.len() >= MIN_CLOSE && first.distance(h) < GRAB {
                    painter.circle_stroke(first, 9.0, Stroke::new(2.0, Color32::WHITE));
                } else if self.pull.is_none() && !self.passthrough {
                    let last = *pts.last().unwrap();
                    painter.add(Shape::dashed_line(&[last, h], Stroke::new(1.0, COLOR), 5.0, 4.0));
                }
            }
            if let Some(c) = v.as_chunks::<6>().0.last() {
                let p = at(c, 0.0, 0.0);
                for tip in [at(c, c[2], c[3]), at(c, c[4], c[5])] {
                    if tip.distance(p) > 0.5 {
                        painter.line_segment([p, tip], Stroke::new(1.0, COLOR));
                        painter.circle(tip, 3.0, COLOR, shadow);
                    }
                }
            }
        }
        let galley = painter.layout_no_wrap(HINT.to_string(), FontId::proportional(13.0), COLOR);
        let at = Pos2::new(rect.center().x - galley.size().x / 2.0, rect.top() + 8.0);
        painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(5.0), 4.0, Color32::from_black_alpha(190));
        painter.galley(at, galley, COLOR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertices_push_and_pop_at_the_end() {
        let v = push_vertex(&[], [10.0, 20.0]);
        assert_eq!(v, vec![10.0, 20.0, 0.0, 0.0, 0.0, 0.0]);
        let v = push_vertex(&v, [30.0, 40.0]);
        assert_eq!(v.len(), 12);
        assert_eq!(pop_vertex(&v), vec![10.0, 20.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(pop_vertex(&pop_vertex(&v)).is_empty());
        assert!(pop_vertex(&[]).is_empty());
    }

    #[test]
    fn tangents_mirror_unless_broken() {
        let v = push_vertex(&[], [10.0, 20.0]);
        assert_eq!(set_tangents(&v, 0, [5.0, -2.0], false), vec![10.0, 20.0, -5.0, 2.0, 5.0, -2.0]);
        let broken = set_tangents(&set_tangents(&v, 0, [5.0, -2.0], false), 0, [1.0, 1.0], true);
        assert_eq!(broken, vec![10.0, 20.0, -5.0, 2.0, 1.0, 1.0]);
        assert_eq!(set_tangents(&v, 3, [1.0, 1.0], false), v, "a missing vertex is left alone");
    }

    fn doc_with(points: Value, closed: bool) -> Doc {
        let mut d = Doc::blank(Default::default());
        let id = d.add_node("roto", [0.0, 0.0]);
        d.set_param(&id, "points", points);
        d.set_param(&id, "closed", json!(closed));
        d.selected = Some(id);
        d
    }

    #[test]
    fn only_an_open_roto_is_drawn_into() {
        assert!(open_roto(&doc_with(json!([]), false)).is_some());
        assert!(open_roto(&doc_with(json!([]), true)).is_none());
    }

    #[test]
    fn closing_needs_three_points_and_leaves_pen_mode() {
        let mut d = doc_with(json!([0, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0]), false);
        let id = d.selected.clone().unwrap();
        let mut pen = PenMode { on: true, ..Default::default() };
        assert!(!pen.close(&mut d, &id, 1));
        edit(&mut d, &id, |v| push_vertex(v, [5.0, 9.0]));
        assert!(pen.close(&mut d, &id, 1));
        assert_eq!(d.param(&id, "closed"), json!(true));
        assert!(!pen.active());
    }

    #[test]
    fn backspace_removes_the_last_point_and_keeps_point_feather_in_step() {
        let mut d = doc_with(json!([0, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 5, 9, 0, 0, 0, 0]), false);
        let id = d.selected.clone().unwrap();
        d.set_param(&id, "point_feather", json!([1, 2, 3]));
        let mut pen = PenMode::default();
        assert!(!pen.delete_last(&mut d), "off: ⌫ does what it otherwise does");
        pen.on = true;
        assert!(pen.delete_last(&mut d));
        assert_eq!(d.param(&id, "points").as_array().unwrap().len(), 12);
        assert_eq!(d.param(&id, "point_feather"), json!([1.0, 2.0]));
    }

    /// Real pointer and key events through `PenMode::ui`: clicks place
    /// points, a drag pulls tangents, Esc takes the last back, a click on
    /// the first point closes it and leaves pen mode.
    #[test]
    fn drawing_with_events() {
        use egui::{Event, Modifiers, PointerButton, Rect};
        let mut d = doc_with(json!([]), false);
        let id = d.selected.clone().unwrap();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(512.0, 512.0));
        // 1:1, the image at the screen origin: screen = image.
        let xf = Xf::new(rect, 1.0);
        let ctx = egui::Context::default();
        let mut pen = PenMode::default();
        let frame = |pen: &mut PenMode, d: &mut Doc, events: Vec<Event>| {
            let raw = egui::RawInput { screen_rect: Some(rect), events, ..Default::default() };
            ctx.run_ui(raw, |ui| {
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                pen.ui(ui, &resp, &xf, d, 1);
            })
            .drop_without_applying_deltas();
        };
        let button = |at: Pos2, pressed: bool| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let click = |at: Pos2| vec![vec![Event::PointerMoved(at), button(at, true)], vec![button(at, false)]];
        let n = |d: &Doc| d.param(&id, "points").as_array().unwrap().len() / 6;
        frame(&mut pen, &mut d, vec![]);
        assert!(pen.active(), "an open Roto selected: pen mode is on");
        for at in [Pos2::new(100.0, 100.0), Pos2::new(300.0, 100.0)] {
            for e in click(at) {
                frame(&mut pen, &mut d, e);
            }
        }
        assert_eq!(n(&d), 2);
        // Press, drag 40 px right, release: the third point's tangents.
        let (a, b) = (Pos2::new(300.0, 300.0), Pos2::new(340.0, 300.0));
        frame(&mut pen, &mut d, vec![Event::PointerMoved(a), button(a, true)]);
        frame(&mut pen, &mut d, vec![Event::PointerMoved(b)]);
        frame(&mut pen, &mut d, vec![button(b, false)]);
        let v = anim::sample(&d.param(&id, "points"), 1.0);
        assert_eq!(&v[12..18], &[300.0, 300.0, -40.0, 0.0, 40.0, 0.0]);
        // Esc takes it back; place it again (no drag).
        frame(&mut pen, &mut d, vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        assert_eq!(n(&d), 2);
        for e in click(a) {
            frame(&mut pen, &mut d, e);
        }
        assert_eq!(n(&d), 3);
        assert_eq!(d.param(&id, "closed"), json!(false));
        // A click on the first point closes it.
        for e in click(Pos2::new(102.0, 101.0)) {
            frame(&mut pen, &mut d, e);
        }
        assert_eq!((n(&d), d.param(&id, "closed")), (3, json!(true)));
        assert!(!pen.active());
    }

    /// Seen downstream past a Transform (scale 2, then +30, +10): points
    /// land in the Roto's own space, the first click of a new Roto too,
    /// and a tangent pull is measured there. Seen through what the handles
    /// can't follow: pen mode stays on, and clicks add nothing.
    #[test]
    fn drawing_through_a_downstream_map_or_not_at_all() {
        use egui::{Event, Modifiers, PointerButton, Rect};
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(512.0, 512.0));
        let down = Xf { down: Some(crate::card_map::Proj::affine(&[2.0, 0.0, 30.0, 0.0, 2.0, 10.0])), ..Xf::new(rect, 1.0) };
        let ctx = egui::Context::default();
        let frame = |pen: &mut PenMode, d: &mut Doc, xf: &Xf, events: Vec<Event>| {
            let raw = egui::RawInput { screen_rect: Some(rect), events, ..Default::default() };
            let mut took = false;
            ctx.run_ui(raw, |ui| {
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                took = pen.ui(ui, &resp, xf, d, 1);
            })
            .drop_without_applying_deltas();
            took
        };
        let button = |at: Pos2, pressed: bool| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let click = |at: Pos2| vec![vec![Event::PointerMoved(at), button(at, true)], vec![button(at, false)]];
        let pts = |d: &Doc, id: &str| anim::sample(&d.param(id, "points"), 1.0);
        // No Roto yet: the first click waits for the new one (as the app
        // adds it: selected, open), then lands in its space.
        let mut d = Doc::blank(Default::default());
        let mut pen = PenMode { on: true, ..Default::default() };
        let mut events = click(Pos2::new(130.0, 110.0)).into_iter();
        frame(&mut pen, &mut d, &down, events.next().unwrap());
        assert!(pen.take_new_shape());
        let id = d.add_node("roto", [0.0, 0.0]);
        d.set_param(&id, "points", json!([]));
        d.set_param(&id, "closed", json!(false));
        d.selected = Some(id.clone());
        frame(&mut pen, &mut d, &down, events.next().unwrap());
        assert_eq!(pts(&d, &id), vec![50.0, 50.0, 0.0, 0.0, 0.0, 0.0]);
        // Then a click and a 40 px pull: (70, 45) with 20 px tangents.
        let (a, b) = (Pos2::new(170.0, 100.0), Pos2::new(210.0, 100.0));
        frame(&mut pen, &mut d, &down, vec![Event::PointerMoved(a), button(a, true)]);
        frame(&mut pen, &mut d, &down, vec![Event::PointerMoved(b)]);
        frame(&mut pen, &mut d, &down, vec![button(b, false)]);
        assert_eq!(&pts(&d, &id)[6..12], &[70.0, 45.0, -20.0, 0.0, 20.0, 0.0]);
        // Blocked: on, but a click adds nothing and is left to the view.
        let blocked = Xf { blocked: true, ..Xf::new(rect, 1.0) };
        let at = Pos2::new(300.0, 300.0);
        frame(&mut pen, &mut d, &blocked, vec![Event::PointerMoved(at)]);
        assert!(!frame(&mut pen, &mut d, &blocked, vec![button(at, true)]), "the press isn't the pen's");
        frame(&mut pen, &mut d, &blocked, vec![button(at, false)]);
        assert_eq!(pts(&d, &id).len(), 12);
        assert!(pen.active(), "still on");
        // Viewed so it can follow again: clicks add points.
        for e in click(Pos2::new(330.0, 210.0)) {
            frame(&mut pen, &mut d, &down, e);
        }
        assert_eq!(&pts(&d, &id)[12..14], &[150.0, 100.0]);
    }

    #[test]
    fn edits_reach_every_key() {
        let keyed = json!({"keys": [[1, [0, 0, 0, 0, 0, 0]], [5, [4, 4, 0, 0, 0, 0]]], "interp": "linear"});
        let mut d = doc_with(keyed, false);
        let id = d.selected.clone().unwrap();
        edit(&mut d, &id, |v| push_vertex(v, [9.0, 9.0]));
        for f in [1.0, 5.0] {
            assert_eq!(anim::sample(&d.param(&id, "points"), f).len(), 12);
        }
    }
}
