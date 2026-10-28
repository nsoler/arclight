//! Viewer overlays: handles for editing the selected node's params directly
//! on the image.
//!
//! Each op declares an [`Overlay`] kind naming the params it edits; this
//! module draws the handles, hit-tests them, and writes through
//! [`Doc::edit_param`], so animated params get a key at the current frame.
//! The viewer calls [`Overlays::interact`] before its own pan handling (a
//! grabbed handle suppresses panning) and [`Overlays::draw`] after the image.

use crate::doc::Doc;
use arclight::ops::{self, Overlay};
use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Response, Shape, Stroke, Vec2};
use serde_json::{json, Value};

/// Hit radius around handles, in screen points.
const GRAB: f32 = 7.0;
/// Length of the transform gizmo's scale axes, in screen points. Fixed, so the
/// handles stay grabbable at any scale; a drag scales by the ratio of
/// pointer distances along the axis.
const AXIS: f32 = 70.0;
/// Radius of the transform gizmo's rotation ring, in screen points.
const RING: f32 = 50.0;

const HANDLE: Color32 = Color32::from_rgb(240, 200, 80);
const HOT: Color32 = Color32::WHITE;
const X_AXIS: Color32 = Color32::from_rgb(235, 90, 80);
const Y_AXIS: Color32 = Color32::from_rgb(110, 210, 100);

/// Image ↔ screen mapping: the image's screen rect and points per pixel.
#[derive(Clone, Copy)]
pub struct Xf {
    pub img_rect: Rect,
    pub zoom: f32,
}

impl Xf {
    fn screen(&self, p: Vec2) -> Pos2 {
        self.img_rect.min + p * self.zoom
    }

    fn image(&self, s: Pos2) -> Vec2 {
        (s - self.img_rect.min) / self.zoom
    }
}

/// What part of an overlay a drag holds.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Point,
    Move,
    Rotate,
    ScaleX,
    ScaleY,
    /// Crop edges: left, top, right, bottom.
    Edges([bool; 4]),
}

/// The selected node's overlay, resolved at the current frame.
enum Geo {
    Point { param: &'static str, value: Vec2 },
    Transform {
        names: [&'static str; 3],
        /// Image center (the pivot) and translate, in image pixels.
        center: Vec2,
        translate: Vec2,
        rotate: f32,
        scale: Vec2,
    },
    Insets { names: [&'static str; 4], values: [f32; 4], size: Vec2 },
}

struct Drag {
    node: String,
    part: Part,
    /// Pointer at the start of the drag, in image pixels.
    start: Vec2,
    /// The overlay's values when the drag started.
    geo: Geo,
    /// Last pointer angle about the pivot (radians), for wrap-free rotation.
    angle: f32,
    /// Rotation accumulated over the drag, in degrees.
    turned: f32,
}

#[derive(Default)]
pub struct Overlays {
    drag: Option<Drag>,
}

fn vec2(v: &Value) -> Vec2 {
    let f = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    Vec2::new(f(0), f(1))
}

fn num(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

/// Rotate `v` by `deg` the way the Transform op does (y down).
fn rot(v: Vec2, deg: f32) -> Vec2 {
    let (s, c) = deg.to_radians().sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

fn geo(doc: &Doc, id: &str, frame: i64, size: Vec2) -> Option<Geo> {
    let op = &doc.node(id)?.op;
    let at = |name| doc.param_at(id, name, frame);
    Some(match ops::info(op)?.overlay {
        Overlay::None => return None,
        Overlay::Point(param) => Geo::Point { param, value: vec2(&at(param)) },
        Overlay::Transform { translate, rotate, scale } => Geo::Transform {
            names: [translate, rotate, scale],
            center: size / 2.0,
            translate: vec2(&at(translate)),
            rotate: num(&at(rotate)),
            scale: vec2(&at(scale)),
        },
        Overlay::Insets { left, top, right, bottom } => Geo::Insets {
            names: [left, top, right, bottom],
            values: [left, top, right, bottom].map(|n| num(&at(n))),
            size,
        },
    })
}

/// Screen positions of the transform gizmo: pivot, x-axis handle, y-axis handle.
fn gizmo(xf: &Xf, center: Vec2, translate: Vec2, rotate: f32) -> (Pos2, Pos2, Pos2) {
    let c = xf.screen(center + translate);
    let x = c + rot(Vec2::new(AXIS, 0.0), rotate);
    // The y handle points up on screen, the usual way to read "height".
    let y = c + rot(Vec2::new(0.0, -AXIS), rotate);
    (c, x, y)
}

/// The crop box in screen space.
fn inset_rect(xf: &Xf, v: [f32; 4], size: Vec2) -> Rect {
    Rect::from_min_max(xf.screen(Vec2::new(v[0], v[1])), xf.screen(Vec2::new(size.x - v[2], size.y - v[3])))
}

fn hit(g: &Geo, xf: &Xf, p: Pos2) -> Option<Part> {
    match *g {
        Geo::Point { value, .. } => (xf.screen(value).distance(p) < GRAB * 1.5).then_some(Part::Point),
        Geo::Transform { center, translate, rotate, .. } => {
            let (c, x, y) = gizmo(xf, center, translate, rotate);
            if x.distance(p) < GRAB {
                Some(Part::ScaleX)
            } else if y.distance(p) < GRAB {
                Some(Part::ScaleY)
            } else if c.distance(p) < GRAB * 1.5 {
                Some(Part::Move)
            } else if (c.distance(p) - RING).abs() < GRAB {
                Some(Part::Rotate)
            } else {
                None
            }
        }
        Geo::Insets { values, size, .. } => {
            let r = inset_rect(xf, values, size);
            let near_x = |x: f32| (p.x - x).abs() < GRAB;
            let near_y = |y: f32| (p.y - y).abs() < GRAB;
            let in_x = p.x > r.left() - GRAB && p.x < r.right() + GRAB;
            let in_y = p.y > r.top() - GRAB && p.y < r.bottom() + GRAB;
            // When the box is thin, a pointer near both edges takes the closer one.
            let left = in_y && near_x(r.left()) && (p.x - r.left()).abs() <= (p.x - r.right()).abs();
            let right = in_y && near_x(r.right()) && !left;
            let top = in_x && near_y(r.top()) && (p.y - r.top()).abs() <= (p.y - r.bottom()).abs();
            let bottom = in_x && near_y(r.bottom()) && !top;
            let e = [left, top, right, bottom];
            e.contains(&true).then_some(Part::Edges(e))
        }
    }
}

impl Overlays {
    /// A handle is being dragged (the viewer shouldn't pan).
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Start, continue or finish a handle drag. `resp` is the viewer canvas.
    pub fn interact(&mut self, ui: &egui::Ui, resp: &Response, xf: &Xf, size: Vec2, doc: &mut Doc, frame: i64) {
        let Some(id) = doc.selected.clone() else {
            self.drag = None;
            return;
        };
        if resp.drag_started() {
            // Hit-test where the press began, not where the drag threshold was crossed.
            let origin = ui.input(|i| i.pointer.press_origin());
            if let (Some(o), Some(g)) = (origin, geo(doc, &id, frame, size)) {
                if let Some(part) = hit(&g, xf, o) {
                    let angle = match g {
                        Geo::Transform { center, translate, .. } => {
                            let d = xf.image(o) - (center + translate);
                            d.y.atan2(d.x)
                        }
                        _ => 0.0,
                    };
                    self.drag = Some(Drag { node: id.clone(), part, start: xf.image(o), geo: g, angle, turned: 0.0 });
                }
            }
        }
        if resp.drag_stopped() || self.drag.as_ref().is_some_and(|d| d.node != id) {
            self.drag = None;
            return;
        }
        if let (Some(drag), Some(ptr)) = (&mut self.drag, resp.interact_pointer_pos()) {
            if resp.dragged() {
                let shift = ui.input(|i| i.modifiers.shift);
                apply(drag, xf.image(ptr), shift, doc, frame);
            }
        }
    }

    pub fn draw(&self, ui: &egui::Ui, painter: &egui::Painter, xf: &Xf, size: Vec2, doc: &Doc, frame: i64) {
        let Some(id) = doc.selected.as_deref() else { return };
        let Some(g) = geo(doc, id, frame, size) else { return };
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| painter.clip_rect().contains(*p));
        let hot = match &self.drag {
            Some(d) => Some(d.part),
            None => hover.and_then(|p| hit(&g, xf, p)),
        };
        if let Some(part) = hot {
            ui.ctx().set_cursor_icon(cursor(part));
        }
        let color = |p: Part, base: Color32| if hot == Some(p) { HOT } else { base };
        let outline = Stroke::new(1.0, Color32::from_black_alpha(200));

        match g {
            Geo::Point { value, .. } => {
                let c = xf.screen(value);
                painter.circle(c, 5.0, color(Part::Point, HANDLE), outline);
                crosshair(painter, c, 10.0, Stroke::new(1.0, color(Part::Point, HANDLE)));
            }
            Geo::Transform { center, translate, rotate, .. } => {
                let (c, x, y) = gizmo(xf, center, translate, rotate);
                // A dark under-stroke keeps lines readable over bright images.
                let shadowed = |a: Pos2, b: Pos2, col: Color32| {
                    painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
                    painter.line_segment([a, b], Stroke::new(1.5, col));
                };
                let ring = color(Part::Rotate, HANDLE);
                painter.circle_stroke(c, RING, Stroke::new(3.0, Color32::from_black_alpha(140)));
                painter.circle_stroke(c, RING, Stroke::new(1.5, ring));
                // A tick on the ring shows the current rotation.
                let tick = rot(Vec2::new(RING, 0.0), rotate);
                painter.line_segment([c + tick * 0.85, c + tick * 1.15], Stroke::new(2.5, ring));
                shadowed(c, x, color(Part::ScaleX, X_AXIS));
                shadowed(c, y, color(Part::ScaleY, Y_AXIS));
                for (p, part, col) in [(x, Part::ScaleX, X_AXIS), (y, Part::ScaleY, Y_AXIS)] {
                    painter.rect(Rect::from_center_size(p, Vec2::splat(9.0)), 1.0, color(part, col), outline, egui::StrokeKind::Middle);
                }
                painter.circle(c, 6.0, color(Part::Move, HANDLE), outline);
            }
            Geo::Insets { values, size, .. } => {
                let r = inset_rect(xf, values, size);
                let img = xf.img_rect;
                // Dim what the crop removes.
                let dim = Color32::from_black_alpha(110);
                for band in [
                    Rect::from_min_max(img.min, Pos2::new(img.max.x, r.top())),
                    Rect::from_min_max(Pos2::new(img.min.x, r.bottom()), img.max),
                    Rect::from_min_max(Pos2::new(img.min.x, r.top()), Pos2::new(r.left(), r.bottom())),
                    Rect::from_min_max(Pos2::new(r.right(), r.top()), Pos2::new(img.max.x, r.bottom())),
                ] {
                    if band.is_positive() {
                        painter.rect_filled(band, 0.0, dim);
                    }
                }
                let edge_hot = |i: usize| matches!(hot, Some(Part::Edges(e)) if e[i]);
                let edge = |a: Pos2, b: Pos2, i: usize| {
                    let col = if edge_hot(i) { HOT } else { HANDLE };
                    painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
                    painter.line_segment([a, b], Stroke::new(1.5, col));
                };
                edge(r.left_top(), r.left_bottom(), 0);
                edge(r.left_top(), r.right_top(), 1);
                edge(r.right_top(), r.right_bottom(), 2);
                edge(r.left_bottom(), r.right_bottom(), 3);
                for p in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom(), r.center_top(), r.center_bottom(), r.left_center(), r.right_center()] {
                    painter.rect(Rect::from_center_size(p, Vec2::splat(7.0)), 1.0, HANDLE, outline, egui::StrokeKind::Middle);
                }
            }
        }
    }
}

/// Write the params for `drag` with the pointer at image position `p`.
fn apply(drag: &mut Drag, p: Vec2, shift: bool, doc: &mut Doc, frame: i64) {
    let id = drag.node.clone();
    let delta = p - drag.start;
    match drag.geo {
        Geo::Point { param, value } => {
            let v = value + delta;
            doc.edit_param(&id, param, frame, json!([v.x, v.y]));
        }
        Geo::Transform { names, center, translate, rotate, scale } => {
            let pivot = center + translate;
            match drag.part {
                Part::Move => {
                    // Shift constrains the move to the dominant axis.
                    let d = if shift { if delta.x.abs() > delta.y.abs() { Vec2::new(delta.x, 0.0) } else { Vec2::new(0.0, delta.y) } } else { delta };
                    let t = translate + d;
                    doc.edit_param(&id, names[0], frame, json!([t.x, t.y]));
                }
                Part::Rotate => {
                    let d = p - pivot;
                    let a = d.y.atan2(d.x);
                    // Accumulate wrapped steps so crossing ±180° doesn't jump.
                    let mut step = a - drag.angle;
                    step -= std::f32::consts::TAU * (step / std::f32::consts::TAU).round();
                    drag.angle = a;
                    drag.turned += step.to_degrees();
                    let mut deg = rotate + drag.turned;
                    if shift {
                        // Shift snaps to 15° steps.
                        deg = (deg / 15.0).round() * 15.0;
                    }
                    doc.edit_param(&id, names[1], frame, json!(deg));
                }
                Part::ScaleX | Part::ScaleY => {
                    let axis = if drag.part == Part::ScaleX { rot(Vec2::X, rotate) } else { rot(-Vec2::Y, rotate) };
                    let from = (drag.start - pivot).dot(axis);
                    let to = (p - pivot).dot(axis);
                    if from.abs() < 1e-3 {
                        return;
                    }
                    let k = to / from;
                    let s = if shift {
                        scale * k
                    } else if drag.part == Part::ScaleX {
                        Vec2::new(scale.x * k, scale.y)
                    } else {
                        Vec2::new(scale.x, scale.y * k)
                    };
                    doc.edit_param(&id, names[2], frame, json!([s.x, s.y]));
                }
                _ => {}
            }
        }
        Geo::Insets { names, values, size } => {
            let Part::Edges(edges) = drag.part else { return };
            // Absolute: each held edge follows the pointer, never past the other side.
            let target = [p.x, p.y, size.x - p.x, size.y - p.y];
            let limit = [size.x - values[2], size.y - values[3], size.x - values[0], size.y - values[1]];
            for i in 0..4 {
                if edges[i] {
                    let v = target[i].round().clamp(0.0, limit[i].max(0.0));
                    doc.edit_param(&id, names[i], frame, json!(v as i64));
                }
            }
        }
    }
}

fn cursor(part: Part) -> CursorIcon {
    match part {
        Part::Point | Part::Move => CursorIcon::Move,
        Part::Rotate => CursorIcon::Alias,
        Part::ScaleX | Part::ScaleY => CursorIcon::ResizeNeSw,
        Part::Edges([l, t, r, b]) => match (l || r, t || b) {
            (true, true) => {
                if (l && t) || (r && b) {
                    CursorIcon::ResizeNwSe
                } else {
                    CursorIcon::ResizeNeSw
                }
            }
            (true, false) => CursorIcon::ResizeHorizontal,
            _ => CursorIcon::ResizeVertical,
        },
    }
}

fn crosshair(painter: &egui::Painter, c: Pos2, r: f32, stroke: Stroke) {
    painter.add(Shape::line_segment([c - Vec2::X * r, c + Vec2::X * r], stroke));
    painter.add(Shape::line_segment([c - Vec2::Y * r, c + Vec2::Y * r], stroke));
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;

    const SIZE: Vec2 = Vec2::new(200.0, 100.0);
    /// 1:1 zoom with the image at the screen origin, so screen = image.
    const XF: Xf = Xf { img_rect: Rect { min: Pos2::ZERO, max: Pos2::new(200.0, 100.0) }, zoom: 1.0 };

    fn doc(op: &str, params: Value) -> Doc {
        let g = Graph { nodes: vec![arclight::graph::NodeSpec {
            id: "n".into(), op: op.into(), inputs: vec![], params, pos: None,
        }], ..Default::default() };
        let mut d = Doc::new(g, None);
        d.selected = Some("n".into());
        d
    }

    /// Press at `from`, drag to `to`, as `interact` would.
    fn drag(d: &mut Doc, from: Pos2, to: Pos2, frame: i64) -> Part {
        let g = geo(d, "n", frame, SIZE).unwrap();
        let part = hit(&g, &XF, from).expect("press should hit a handle");
        let pivot = match g { Geo::Transform { center, translate, .. } => center + translate, _ => Vec2::ZERO };
        let a = (XF.image(from) - pivot).y.atan2((XF.image(from) - pivot).x);
        let mut dr = Drag { node: "n".into(), part, start: XF.image(from), geo: g, angle: a, turned: 0.0 };
        // Two steps, as a real drag arrives over several frames.
        apply(&mut dr, XF.image(from.lerp(to, 0.5)), false, d, frame);
        apply(&mut dr, XF.image(to), false, d, frame);
        part
    }

    #[test]
    fn transform_gizmo_moves_rotates_and_scales() {
        let mut d = doc("transform", json!({}));
        let c = Pos2::new(100.0, 50.0);
        assert_eq!(drag(&mut d, c, c + Vec2::new(10.0, 5.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "translate")), Vec2::new(10.0, 5.0));

        let c = c + Vec2::new(10.0, 5.0);
        assert_eq!(drag(&mut d, c + Vec2::new(0.0, -RING), c + Vec2::new(RING, 0.0), 1), Part::Rotate);
        assert!((num(&d.param("n", "rotate")) - 90.0).abs() < 1e-3);

        // At 90°, the x axis points down the screen.
        let x_handle = c + Vec2::new(0.0, AXIS);
        assert_eq!(drag(&mut d, x_handle, c + Vec2::new(0.0, AXIS * 2.0), 1), Part::ScaleX);
        let s = vec2(&d.param("n", "scale"));
        assert!((s.x - 2.0).abs() < 1e-3 && (s.y - 1.0).abs() < 1e-3, "{s:?}");
    }

    #[test]
    fn rotation_crosses_180_without_jumping() {
        let mut d = doc("transform", json!({ "rotate": 170.0 }));
        let c = Pos2::new(100.0, 50.0);
        let at = |deg: f32| c + rot(Vec2::new(RING, 0.0), deg);
        drag(&mut d, at(170.0), at(-170.0), 1);
        assert!((num(&d.param("n", "rotate")) - 190.0).abs() < 1e-2);
    }

    #[test]
    fn crop_edges_follow_the_pointer_and_stop_at_the_other_side() {
        let mut d = doc("crop", json!({ "right": 50 }));
        let part = drag(&mut d, Pos2::new(0.0, 50.0), Pos2::new(30.4, 60.0), 1);
        assert_eq!(part, Part::Edges([true, false, false, false]));
        assert_eq!(d.param("n", "left"), json!(30));
        // The left edge can't pass the right edge (at 200 - 50).
        drag(&mut d, Pos2::new(30.0, 50.0), Pos2::new(190.0, 50.0), 1);
        assert_eq!(d.param("n", "left"), json!(150));
        // A corner drags two edges.
        let mut d = doc("crop", json!({}));
        let part = drag(&mut d, Pos2::new(200.0, 100.0), Pos2::new(190.0, 90.0), 1);
        assert_eq!(part, Part::Edges([false, false, true, true]));
        assert_eq!((d.param("n", "right"), d.param("n", "bottom")), (json!(10), json!(10)));
    }

    #[test]
    fn dragging_an_animated_param_keys_the_current_frame() {
        let mut d = doc("transform", json!({ "translate": { "keys": [[1, [0, 0]], [10, [0, 0]]] } }));
        let c = Pos2::new(100.0, 50.0);
        drag(&mut d, c, c + Vec2::new(20.0, 0.0), 5);
        assert_eq!(vec2(&d.param_at("n", "translate", 5)), Vec2::new(20.0, 0.0));
        assert_eq!(vec2(&d.param_at("n", "translate", 1)), Vec2::ZERO);
    }
}
