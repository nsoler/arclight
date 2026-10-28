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
use arclight::{anim, shape};
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
/// Distance of a box's rotation handle beyond its top edge, in screen points.
const STEM: f32 = 24.0;
/// Fewest vertices a point list keeps when deleting.
const MIN_POINTS: usize = 3;
/// Stride of a spline's points: x, y, in-handle and out-handle offsets.
const SPLINE: usize = 6;
/// Samples per spline segment when hit-testing and drawing it.
const SAMPLES: usize = 48;

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
    /// Box size along its own x and/or y axis.
    Size { x: bool, y: bool },
    /// A vertex of a point list; a drag moves every selected vertex.
    Vertex(usize),
    /// The edge from vertex `i` to the next, at fraction `t` along it (the
    /// curve parameter, on a spline).
    Edge(usize, f32),
    /// A spline vertex's in (`out: false`) or out tangent handle.
    Tangent { i: usize, out: bool },
    /// New symmetric tangents pulled out of spline vertex `i`.
    Pull(usize),
}

/// The selected node's overlay, resolved at the current frame.
#[derive(Clone)]
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
    Box { names: [&'static str; 3], center: Vec2, size: Vec2, rotate: f32, ellipse: bool },
    /// Vertices of a point list (`stride` numbers each, x and y first). A
    /// spline (stride 6) also has each vertex's in and out handle offsets
    /// in `tan`; other point lists leave it empty.
    Points { param: &'static str, stride: usize, pts: Vec<Vec2>, tan: Vec<[Vec2; 2]> },
}

impl Geo {
    /// What rotation handles turn about.
    fn pivot(&self) -> Vec2 {
        match *self {
            Geo::Transform { center, translate, .. } => center + translate,
            Geo::Box { center, .. } => center,
            _ => Vec2::ZERO,
        }
    }
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
    /// Selected vertices of the selected node's point list, and that node.
    points: Vec<usize>,
    points_node: Option<String>,
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
        Overlay::Box { center, size, rotate, ellipse } => Geo::Box {
            names: [center, size, rotate],
            center: vec2(&at(center)),
            size: vec2(&at(size)),
            rotate: num(&at(rotate)),
            ellipse,
        },
        Overlay::Points(param) => {
            let stride = points_stride(op, param);
            let flat: Vec<f32> = at(param).as_array().map(|a| a.iter().map(num).collect()).unwrap_or_default();
            let pts = flat.chunks_exact(stride).map(|c| Vec2::new(c[0], c[1])).collect();
            let tan = if stride == SPLINE {
                flat.chunks_exact(stride).map(|c| [Vec2::new(c[2], c[3]), Vec2::new(c[4], c[5])]).collect()
            } else {
                Vec::new()
            };
            Geo::Points { param, stride, pts, tan }
        }
    })
}

fn points_stride(op: &str, param: &str) -> usize {
    let spec = ops::info(op).and_then(|i| i.params.iter().find(|s| s.name == param));
    match spec.map(|s| s.kind) {
        Some(ops::ParamKind::Points { stride }) => stride.max(2),
        _ => 2,
    }
}

/// Screen positions of a box's handles: corners and edge midpoints (with the
/// axes each one sizes), then the rotation handle.
fn box_handles(xf: &Xf, center: Vec2, size: Vec2, rotate: f32) -> (Vec<(Pos2, Part)>, Pos2) {
    let h = size / 2.0;
    let at = |x: f32, y: f32| xf.screen(center + rot(Vec2::new(x * h.x, y * h.y), rotate));
    let mut handles = Vec::new();
    for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        handles.push((at(x, y), Part::Size { x: true, y: true }));
    }
    for (x, y) in [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)] {
        handles.push((at(x, y), Part::Size { x: x != 0.0, y: y != 0.0 }));
    }
    // Above the top edge, so it stays put however the size changes sign.
    let up = rot(Vec2::new(0.0, -1.0), rotate) * if size.y < 0.0 { -1.0 } else { 1.0 };
    (handles, at(0.0, -1.0) + up * STEM)
}

/// The outline of a box or ellipse, in screen space.
fn box_outline(xf: &Xf, center: Vec2, size: Vec2, rotate: f32, ellipse: bool) -> Vec<Pos2> {
    let c = [center.x, center.y];
    let s = [size.x, size.y];
    let pts = if ellipse {
        let h = size.abs() / 2.0;
        (0..72)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 72.0;
                let r = shape::rotate([h.x * a.cos(), h.y * a.sin()], rotate);
                [c[0] + r[0], c[1] + r[1]]
            })
            .collect()
    } else {
        shape::rectangle(c, s, rotate)
    };
    pts.into_iter().map(|p| xf.screen(Vec2::new(p[0], p[1]))).collect()
}

/// The nearest point on segment `a`–`b` to `p`, as a fraction along it.
fn along(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let d = b - a;
    let len2 = d.length_sq();
    if len2 > 0.0 { ((p - a).dot(d) / len2).clamp(0.0, 1.0) } else { 0.0 }
}

/// The bezier segments through `pts` with handle offsets `tan`, in image pixels.
fn segments(pts: &[Vec2], tan: &[[Vec2; 2]]) -> Vec<shape::Cubic> {
    let flat: Vec<f32> = pts.iter().zip(tan).flat_map(|(p, [i, o])| [p.x, p.y, i.x, i.y, o.x, o.y]).collect();
    shape::spline_segments(&flat)
}

/// The outline of a point list in screen space: its polygon, or its curve.
fn points_outline(xf: &Xf, pts: &[Vec2], tan: &[[Vec2; 2]]) -> Vec<Pos2> {
    if tan.is_empty() {
        return pts.iter().map(|&v| xf.screen(v)).collect();
    }
    segments(pts, tan)
        .iter()
        .flat_map(|c| (0..SAMPLES).map(move |j| shape::cubic_at(c, j as f32 / SAMPLES as f32)))
        .map(|p| xf.screen(Vec2::new(p[0], p[1])))
        .collect()
}

/// Screen positions of a spline vertex's in and out handles.
fn tangent_tips(xf: &Xf, p: Vec2, t: [Vec2; 2]) -> [Pos2; 2] {
    t.map(|o| xf.screen(p + o))
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

/// The handle at screen position `p`. `sel` are the selected vertices of a
/// point list, whose tangent handles are live.
fn hit(g: &Geo, xf: &Xf, p: Pos2, sel: &[usize]) -> Option<Part> {
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
        Geo::Box { center, size, rotate, .. } => {
            let (handles, spin) = box_handles(xf, center, size, rotate);
            if spin.distance(p) < GRAB {
                return Some(Part::Rotate);
            }
            if let Some(&(_, part)) = handles.iter().find(|(h, _)| h.distance(p) < GRAB) {
                return Some(part);
            }
            (xf.screen(center).distance(p) < GRAB * 1.5).then_some(Part::Move)
        }
        Geo::Points { ref pts, ref tan, .. } => {
            let s: Vec<Pos2> = pts.iter().map(|&v| xf.screen(v)).collect();
            // Tangent tips first, so one lying over another vertex stays
            // grabbable; a collapsed handle leaves the vertex to its own hit.
            for &i in sel.iter().filter(|&&i| i < tan.len()) {
                for (k, tip) in tangent_tips(xf, pts[i], tan[i]).into_iter().enumerate() {
                    if tip.distance(s[i]) > GRAB * 0.5 && tip.distance(p) < GRAB {
                        return Some(Part::Tangent { i, out: k == 1 });
                    }
                }
            }
            let nearest = (0..s.len()).min_by(|&a, &b| s[a].distance(p).total_cmp(&s[b].distance(p)));
            if let Some(i) = nearest.filter(|&i| s[i].distance(p) < GRAB) {
                return Some(Part::Vertex(i));
            }
            if s.len() < 2 {
                return None;
            }
            // Each edge as chords: a straight line, or the sampled curve.
            let line = points_outline(xf, pts, tan);
            let per = if tan.is_empty() { 1 } else { SAMPLES };
            (0..line.len())
                .map(|j| {
                    let (a, b) = (line[j], line[(j + 1) % line.len()]);
                    let t = along(p, a, b);
                    (j / per, ((j % per) as f32 + t) / per as f32, a.lerp(b, t).distance(p))
                })
                .filter(|&(.., d)| d < GRAB * 0.8)
                .min_by(|a, b| a.2.total_cmp(&b.2))
                .map(|(i, t, _)| Part::Edge(i, t))
        }
    }
}

/// Modifier keys held when a handle is pressed.
#[derive(Clone, Copy, Default)]
pub struct Mods {
    /// Add to / toggle the vertex selection.
    pub shift: bool,
    /// Ctrl (⌘ on macOS): add a vertex at the pointer.
    pub command: bool,
    /// Alt (⌥): on a spline, pull tangents out of a vertex, or move one
    /// tangent handle without its opposite.
    pub alt: bool,
}

impl Overlays {
    /// A handle is being dragged (the viewer shouldn't pan).
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Forget the vertex selection when another node is selected.
    fn sync_selection(&mut self, doc: &Doc) {
        if self.points_node != doc.selected {
            self.points.clear();
            self.points_node = doc.selected.clone();
        }
    }

    /// Start, continue or finish a handle drag. `resp` is the viewer canvas.
    pub fn interact(&mut self, ui: &egui::Ui, resp: &Response, xf: &Xf, size: Vec2, doc: &mut Doc, frame: i64) {
        self.sync_selection(doc);
        if doc.selected.is_none() {
            self.drag = None;
            return;
        }
        let (pressed, released, mods) = ui.input(|i| {
            let m = i.modifiers;
            (i.pointer.primary_pressed(), i.pointer.primary_released(), Mods { shift: m.shift, command: m.command, alt: m.alt })
        });
        if pressed {
            if let Some(o) = ui.input(|i| i.pointer.press_origin()).filter(|o| resp.rect.contains(*o)) {
                self.press(doc, frame, size, xf, o, mods);
            }
        }
        if let (Some(ptr), true) = (resp.interact_pointer_pos(), resp.dragged()) {
            self.drag_to(doc, frame, xf.image(ptr), mods);
        }
        if released || resp.drag_stopped() {
            self.drag = None;
        }
    }

    /// The pointer went down at screen position `o`: grab the handle there,
    /// or, on a point list, select / insert vertices.
    fn press(&mut self, doc: &mut Doc, frame: i64, size: Vec2, xf: &Xf, o: Pos2, mods: Mods) {
        self.drag = None;
        let Some(id) = doc.selected.clone() else { return };
        let Some(g) = geo(doc, &id, frame, size) else { return };
        self.points.retain(|&i| matches!(&g, Geo::Points { pts, .. } if i < pts.len()));
        let mut part = hit(&g, xf, o, &self.points);
        if let Geo::Points { param, stride, ref pts, .. } = g {
            let spline = stride == SPLINE;
            match part {
                Some(Part::Vertex(i)) if spline && mods.alt => {
                    self.points = vec![i];
                    part = Some(Part::Pull(i));
                }
                Some(Part::Vertex(i)) if mods.shift => {
                    // Shift toggles; a vertex shift-clicked off isn't dragged.
                    if let Some(k) = self.points.iter().position(|&j| j == i) {
                        self.points.remove(k);
                        return;
                    }
                    self.points.push(i);
                }
                Some(Part::Vertex(i)) => {
                    if !self.points.contains(&i) {
                        self.points = vec![i];
                    }
                }
                Some(Part::Edge(i, t)) => {
                    // Each key gets the vertex at the same fraction of its own edge.
                    let n = pts.len();
                    doc.set_param(&id, param, anim::edit_all(&doc.param(&id, param), |v| {
                        if spline {
                            return split_spline(v, i, t);
                        }
                        let at = |k: usize, c: usize| v.get((k % n) * stride + c).copied().unwrap_or(0.0);
                        let mut vertex = vec![0.0; stride];
                        for (c, x) in vertex.iter_mut().enumerate().take(2) {
                            *x = at(i, c) + (at(i + 1, c) - at(i, c)) * t as f64;
                        }
                        insert_vertex(v, stride, i + 1, &vertex)
                    }));
                    self.points = vec![i + 1];
                    part = Some(Part::Vertex(i + 1));
                }
                None if mods.command => {
                    // Add after the last selected vertex, or at the end.
                    let i = self.points.last().map_or(pts.len(), |&i| i + 1);
                    let p = xf.image(o);
                    let mut vertex = vec![0.0; stride];
                    vertex[0] = p.x as f64;
                    vertex[1] = p.y as f64;
                    doc.set_param(&id, param, anim::edit_all(&doc.param(&id, param), |v| insert_vertex(v, stride, i, &vertex)));
                    self.points = vec![i];
                    // On a spline, dragging on pulls out the new vertex's tangents.
                    part = Some(if spline { Part::Pull(i) } else { Part::Vertex(i) });
                }
                None if !mods.shift => self.points.clear(),
                _ => {}
            }
        }
        let Some(part) = part else { return };
        // Inserting changed the geometry: start from the new one.
        let Some(g) = geo(doc, &id, frame, size) else { return };
        let start = xf.image(o);
        let d = start - g.pivot();
        self.drag = Some(Drag { node: id, part, start, geo: g, angle: d.y.atan2(d.x), turned: 0.0 });
    }

    fn drag_to(&mut self, doc: &mut Doc, frame: i64, p: Vec2, mods: Mods) {
        if let Some(drag) = &mut self.drag {
            apply(drag, &self.points, p, mods, doc, frame);
        }
    }

    /// Delete the selected vertices of the selected node's point list.
    /// Returns false (doing nothing) when no vertices are selected, so the
    /// caller can delete the node instead.
    pub fn delete_points(&mut self, doc: &mut Doc) -> bool {
        self.sync_selection(doc);
        let Some(id) = doc.selected.clone() else { return false };
        let Some(Overlay::Points(param)) = doc.node(&id).and_then(|n| ops::info(&n.op)).map(|i| i.overlay) else {
            return false;
        };
        if self.points.is_empty() {
            return false;
        }
        let stride = points_stride(&doc.node(&id).unwrap().op, param);
        let gone = std::mem::take(&mut self.points);
        let raw = doc.param(&id, param);
        let n = anim::sample(&raw, 0.0).len() / stride;
        if n.saturating_sub(gone.len()) >= MIN_POINTS {
            doc.set_param(&id, param, anim::edit_all(&raw, |v| {
                v.chunks(stride).enumerate().filter(|(i, _)| !gone.contains(i)).flat_map(|(_, c)| c.to_vec()).collect()
            }));
        }
        true
    }

    /// The vertex selection, if it belongs to the selected node.
    fn selection(&self, doc: &Doc) -> &[usize] {
        if self.points_node == doc.selected { &self.points } else { &[] }
    }

    pub fn draw(&self, ui: &egui::Ui, painter: &egui::Painter, xf: &Xf, size: Vec2, doc: &Doc, frame: i64) {
        let Some(id) = doc.selected.as_deref() else { return };
        let Some(g) = geo(doc, id, frame, size) else { return };
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| painter.clip_rect().contains(*p));
        let hot = match &self.drag {
            Some(d) => Some(d.part),
            None => hover.and_then(|p| hit(&g, xf, p, self.selection(doc))),
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
            Geo::Box { center, size, rotate, ellipse, .. } => {
                let (handles, spin) = box_handles(xf, center, size, rotate);
                let top = handles[4].0;
                outline_path(painter, box_outline(xf, center, size, rotate, ellipse), HANDLE);
                if ellipse {
                    // The bounding box the handles sit on.
                    let frame = box_outline(xf, center, size, rotate, false);
                    painter.add(Shape::closed_line(frame, Stroke::new(1.0, Color32::from_white_alpha(60))));
                }
                let ring = color(Part::Rotate, HANDLE);
                painter.line_segment([top, spin], Stroke::new(1.5, ring));
                painter.circle(spin, 4.5, ring, outline);
                for (p, part) in handles {
                    painter.rect(Rect::from_center_size(p, Vec2::splat(8.0)), 1.0, color(part, HANDLE), outline, egui::StrokeKind::Middle);
                }
                let c = xf.screen(center);
                painter.circle(c, 5.0, color(Part::Move, HANDLE), outline);
            }
            Geo::Points { pts, tan, .. } => {
                let s: Vec<Pos2> = pts.iter().map(|&v| xf.screen(v)).collect();
                outline_path(painter, points_outline(xf, &pts, &tan), HANDLE);
                // Where a click would insert a vertex.
                if let (Some(Part::Edge(i, t)), None) = (hot, &self.drag) {
                    let p = if tan.is_empty() {
                        s[i].lerp(s[(i + 1) % s.len()], t)
                    } else {
                        let q = shape::cubic_at(&segments(&pts, &tan)[i], t);
                        xf.screen(Vec2::new(q[0], q[1]))
                    };
                    painter.circle(p, 3.5, HOT, outline);
                }
                // The selected vertices' tangent handles.
                for &i in self.selection(doc).iter().filter(|&&i| i < tan.len()) {
                    for (k, tip) in tangent_tips(xf, pts[i], tan[i]).into_iter().enumerate() {
                        let part = Part::Tangent { i, out: k == 1 };
                        let col = if hot == Some(part) || hot == Some(Part::Pull(i)) { HOT } else { HANDLE };
                        painter.line_segment([s[i], tip], Stroke::new(3.0, Color32::from_black_alpha(140)));
                        painter.line_segment([s[i], tip], Stroke::new(1.0, col));
                        painter.circle(tip, 3.5, col, outline);
                    }
                }
                for (i, &p) in s.iter().enumerate() {
                    let selected = self.points_node == doc.selected && self.points.contains(&i);
                    let fill = if selected { HOT } else if hot == Some(Part::Vertex(i)) { HANDLE } else { Color32::from_black_alpha(160) };
                    painter.rect(Rect::from_center_size(p, Vec2::splat(7.0)), 1.0, fill, Stroke::new(1.5, HANDLE), egui::StrokeKind::Middle);
                }
            }
        }
    }
}

/// `v` with `vertex` inserted as vertex `i`.
fn insert_vertex(v: &[f64], stride: usize, i: usize, vertex: &[f64]) -> Vec<f64> {
    let at = (i * stride).min(v.len());
    let mut out = v[..at].to_vec();
    out.extend_from_slice(vertex);
    out.extend_from_slice(&v[at..]);
    out
}

/// Spline points `v` with a vertex inserted at `t` along segment `i`,
/// splitting the curve there (de Casteljau) so its shape doesn't change.
fn split_spline(v: &[f64], i: usize, t: f32) -> Vec<f64> {
    let n = v.len() / SPLINE;
    let flat: Vec<f32> = v.iter().map(|&x| x as f32).collect();
    let Some(seg) = shape::spline_segments(&flat).get(i).copied() else { return v.to_vec() };
    let (a, b) = shape::split_cubic(&seg, t);
    let m = a[3];
    let mut out = v.to_vec();
    let (j, k) = (i * SPLINE, ((i + 1) % n) * SPLINE);
    // Vertex i's out handle and the next vertex's in handle shorten to fit.
    out[j + 4] = (a[1][0] - a[0][0]) as f64;
    out[j + 5] = (a[1][1] - a[0][1]) as f64;
    out[k + 2] = (b[2][0] - b[3][0]) as f64;
    out[k + 3] = (b[2][1] - b[3][1]) as f64;
    let vertex = [m[0], m[1], a[2][0] - m[0], a[2][1] - m[1], b[1][0] - m[0], b[1][1] - m[1]].map(|x| x as f64);
    insert_vertex(&out, SPLINE, i + 1, &vertex)
}

/// Rotation accumulated over `drag` with the pointer at `p`, in degrees.
/// Wrapped steps are summed so crossing ±180° doesn't jump.
fn turn(drag: &mut Drag, p: Vec2) -> f32 {
    let d = p - drag.geo.pivot();
    let a = d.y.atan2(d.x);
    let mut step = a - drag.angle;
    step -= std::f32::consts::TAU * (step / std::f32::consts::TAU).round();
    drag.angle = a;
    drag.turned += step.to_degrees();
    drag.turned
}

/// `delta` limited to its dominant axis.
fn constrain(delta: Vec2) -> Vec2 {
    if delta.x.abs() > delta.y.abs() { Vec2::new(delta.x, 0.0) } else { Vec2::new(0.0, delta.y) }
}

/// Write the params for `drag` with the pointer at image position `p`.
/// `selected` are the selected vertices, for point lists.
fn apply(drag: &mut Drag, selected: &[usize], p: Vec2, mods: Mods, doc: &mut Doc, frame: i64) {
    let shift = mods.shift;
    let id = drag.node.clone();
    let delta = p - drag.start;
    match drag.geo.clone() {
        Geo::Point { param, value } => {
            let v = value + delta;
            doc.edit_param(&id, param, frame, json!([v.x, v.y]));
        }
        Geo::Transform { names, center, translate, rotate, scale } => {
            let pivot = center + translate;
            match drag.part {
                Part::Move => {
                    // Shift constrains the move to the dominant axis.
                    let t = translate + if shift { constrain(delta) } else { delta };
                    doc.edit_param(&id, names[0], frame, json!([t.x, t.y]));
                }
                Part::Rotate => {
                    let mut deg = rotate + turn(drag, p);
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
        Geo::Box { names, center, size, rotate, .. } => match drag.part {
            Part::Move => {
                let c = center + if shift { constrain(delta) } else { delta };
                doc.edit_param(&id, names[0], frame, json!([c.x, c.y]));
            }
            Part::Size { x, y } => {
                // Symmetric about the center: the handle follows the pointer.
                let local = rot(p - center, -rotate).abs() * 2.0;
                let mut s = Vec2::new(if x { local.x } else { size.x }, if y { local.y } else { size.y });
                if shift && x && y {
                    // Shift keeps the aspect ratio.
                    let k = (s.x / size.x.abs().max(1e-3)).max(s.y / size.y.abs().max(1e-3));
                    s = size.abs() * k;
                }
                doc.edit_param(&id, names[1], frame, json!([s.x.max(1.0), s.y.max(1.0)]));
            }
            Part::Rotate => {
                let mut deg = rotate + turn(drag, p);
                if shift {
                    deg = (deg / 15.0).round() * 15.0;
                }
                doc.edit_param(&id, names[2], frame, json!(deg));
            }
            _ => {}
        },
        Geo::Points { param, stride, pts, tan } => {
            let mut v: Vec<f64> = anim::sample(&doc.param(&id, param), frame as f64);
            let handles = match drag.part {
                Part::Tangent { i, out } if i < tan.len() => {
                    let (this, other) = if out { (1, 0) } else { (0, 1) };
                    let new = tan[i][this] + delta;
                    let mut h = tan[i];
                    h[this] = new;
                    if !mods.alt {
                        // Keep the curve smooth: the opposite handle turns to
                        // stay opposite, keeping its length (or matching, if it had none).
                        let len = tan[i][other].length();
                        h[other] = if len > 1e-3 { -new.normalized() * len } else { -new };
                    }
                    Some((i, h))
                }
                Part::Pull(i) if i < pts.len() => {
                    let out = p - pts[i];
                    Some((i, [-out, out]))
                }
                _ => None,
            };
            if let Some((i, [hi, ho])) = handles {
                if let Some(c) = v.get_mut(i * stride + 2..i * stride + 6) {
                    c.copy_from_slice(&[hi.x, hi.y, ho.x, ho.y].map(|x| x as f64));
                }
                doc.edit_param(&id, param, frame, json!(v));
                return;
            }
            let d = if shift { constrain(delta) } else { delta };
            for &i in selected.iter().filter(|&&i| i < pts.len()) {
                let q = pts[i] + d;
                if let Some(c) = v.get_mut(i * stride..i * stride + 2) {
                    c.copy_from_slice(&[q.x as f64, q.y as f64]);
                }
            }
            doc.edit_param(&id, param, frame, json!(v));
        }
    }
}

fn cursor(part: Part) -> CursorIcon {
    match part {
        Part::Point | Part::Move | Part::Vertex(_) | Part::Tangent { .. } | Part::Pull(_) => CursorIcon::Move,
        Part::Edge(..) => CursorIcon::Crosshair,
        Part::Size { x: true, y: true } => CursorIcon::ResizeNwSe,
        Part::Size { x: true, .. } => CursorIcon::ResizeHorizontal,
        Part::Size { .. } => CursorIcon::ResizeVertical,
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

/// A closed path drawn over a dark under-stroke, readable on any image.
fn outline_path(painter: &egui::Painter, pts: Vec<Pos2>, col: Color32) {
    painter.add(Shape::closed_line(pts.clone(), Stroke::new(3.0, Color32::from_black_alpha(140))));
    painter.add(Shape::closed_line(pts, Stroke::new(1.5, col)));
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

    /// Press at `from`, drag to `to` and release, as `interact` would.
    /// Returns the part grabbed, if any.
    fn gesture(ov: &mut Overlays, d: &mut Doc, from: Pos2, to: Pos2, frame: i64, mods: Mods) -> Option<Part> {
        ov.sync_selection(d);
        ov.press(d, frame, SIZE, &XF, from, mods);
        let part = ov.drag.as_ref().map(|dr| dr.part);
        // Two steps, as a real drag arrives over several frames.
        ov.drag_to(d, frame, XF.image(from.lerp(to, 0.5)), mods);
        ov.drag_to(d, frame, XF.image(to), mods);
        ov.drag = None;
        part
    }

    fn drag(d: &mut Doc, from: Pos2, to: Pos2, frame: i64) -> Part {
        gesture(&mut Overlays::default(), d, from, to, frame, Mods::default()).expect("press should hit a handle")
    }

    fn points(d: &Doc, frame: i64) -> Vec<f32> {
        d.param_at("n", "points", frame).as_array().unwrap().iter().map(num).collect()
    }

    const SQUARE: [f32; 8] = [50.0, 25.0, 150.0, 25.0, 150.0, 75.0, 50.0, 75.0];

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

    #[test]
    fn box_handles_move_size_and_rotate() {
        let mut d = doc("rectangle", json!({ "center": [100, 50], "size": [40, 20] }));
        assert_eq!(drag(&mut d, Pos2::new(120.0, 60.0), Pos2::new(130.0, 70.0), 1), Part::Size { x: true, y: true });
        assert_eq!(vec2(&d.param("n", "size")), Vec2::new(60.0, 40.0));
        // An edge handle sizes one axis only.
        assert_eq!(drag(&mut d, Pos2::new(130.0, 50.0), Pos2::new(110.0, 0.0), 1), Part::Size { x: true, y: false });
        assert_eq!(vec2(&d.param("n", "size")), Vec2::new(20.0, 40.0));
        // The rotation handle sits above the top edge (y = 30).
        let spin = Pos2::new(100.0, 30.0 - STEM);
        assert_eq!(drag(&mut d, spin, Pos2::new(150.0, 50.0), 1), Part::Rotate);
        assert!((num(&d.param("n", "rotate")) - 90.0).abs() < 1e-3);
        assert_eq!(drag(&mut d, Pos2::new(100.0, 50.0), Pos2::new(90.0, 45.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "center")), Vec2::new(90.0, 45.0));
    }

    #[test]
    fn clicking_an_edge_inserts_a_vertex_in_every_key() {
        let moved: Vec<f32> = SQUARE.iter().map(|v| v + 10.0).collect();
        let mut d = doc("polygon", json!({ "points": { "keys": [[1, SQUARE], [10, moved]] } }));
        let mut ov = Overlays::default();
        // Press on the top edge's midpoint and pull it up.
        let part = gesture(&mut ov, &mut d, Pos2::new(100.0, 25.0), Pos2::new(100.0, 5.0), 1, Mods::default());
        assert_eq!(part, Some(Part::Vertex(1)));
        assert_eq!(points(&d, 1), [50.0, 25.0, 100.0, 5.0, 150.0, 25.0, 150.0, 75.0, 50.0, 75.0]);
        // The other key gets the vertex on its own edge, at the same fraction.
        assert_eq!(points(&d, 10)[2..4], [110.0, 35.0]);
        assert_eq!(ov.points, vec![1]);
    }

    #[test]
    fn shift_selects_several_vertices_that_move_together() {
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let mut ov = Overlays::default();
        gesture(&mut ov, &mut d, Pos2::new(50.0, 25.0), Pos2::new(50.0, 25.0), 1, Mods::default());
        let shift = Mods { shift: true, ..Default::default() };
        gesture(&mut ov, &mut d, Pos2::new(150.0, 25.0), Pos2::new(150.0, 25.0), 1, shift);
        assert_eq!(ov.points, vec![0, 1]);
        // Dragging either selected vertex moves both.
        gesture(&mut ov, &mut d, Pos2::new(150.0, 25.0), Pos2::new(150.0, 15.0), 1, Mods::default());
        assert_eq!(points(&d, 1), [50.0, 15.0, 150.0, 15.0, 150.0, 75.0, 50.0, 75.0]);
        // Shift-clicking a selected vertex deselects it; clicking empty space clears.
        gesture(&mut ov, &mut d, Pos2::new(50.0, 15.0), Pos2::new(50.0, 15.0), 1, shift);
        assert_eq!(ov.points, vec![1]);
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(5.0, 95.0), Pos2::new(5.0, 95.0), 1, Mods::default()), None);
        assert!(ov.points.is_empty());
    }

    #[test]
    fn ctrl_click_adds_and_delete_removes_vertices() {
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let mut ov = Overlays::default();
        let ctrl = Mods { command: true, ..Default::default() };
        // Nothing selected: the new vertex goes at the end.
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(20.0, 50.0), Pos2::new(20.0, 50.0), 1, ctrl), Some(Part::Vertex(4)));
        assert_eq!(points(&d, 1)[8..], [20.0, 50.0]);
        assert!(ov.delete_points(&mut d));
        assert_eq!(points(&d, 1), SQUARE);
        // Never below three vertices; with nothing selected, delete passes.
        ov.points = vec![0, 1];
        assert!(ov.delete_points(&mut d));
        assert_eq!(points(&d, 1), SQUARE);
        assert!(!ov.delete_points(&mut d));
    }

    /// SQUARE as a spline: corners with horizontal handles 20 px long, so
    /// the top and bottom edges stay straight and the sides bulge out.
    fn spline_square() -> Vec<f32> {
        SQUARE.as_chunks::<2>().0.iter().enumerate().flat_map(|(i, &[x, y])| {
            // Clockwise: the top edge runs right, the bottom edge runs left.
            let h = if i < 2 { 20.0 } else { -20.0 };
            [x, y, -h, 0.0, h, 0.0]
        }).collect()
    }

    fn select(ov: &mut Overlays, d: &mut Doc, at: Pos2) {
        gesture(ov, d, at, at, 1, Mods::default());
    }

    #[test]
    fn inserting_on_a_spline_keeps_its_shape() {
        let mut d = doc("roto", json!({ "points": spline_square() }));
        let before = shape::spline(&points(&d, 1));
        let mut ov = Overlays::default();
        // The right side bulges: its midpoint is at t = 0.5 on segment 1,
        // 3/4 · 20 = 15 px right of the straight line.
        let mid = Pos2::new(165.0, 50.0);
        assert_eq!(gesture(&mut ov, &mut d, mid, mid, 1, Mods::default()), Some(Part::Vertex(2)));
        let v = points(&d, 1);
        assert_eq!(v.len(), 5 * SPLINE);
        assert!((v[12] - 165.0).abs() < 0.5 && (v[13] - 50.0).abs() < 0.5, "{:?}", &v[12..18]);
        // Every point of the old outline is still on the new one.
        let after: Vec<[f32; 2]> = shape::spline_segments(&v)
            .iter()
            .flat_map(|c| (0..2000).map(move |j| shape::cubic_at(c, j as f32 / 2000.0)))
            .collect();
        for p in before.iter().step_by(7) {
            let d = after.iter().map(|q| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()).fold(f32::MAX, f32::min);
            assert!(d < 0.05, "{p:?} is {d} px off");
        }
    }

    #[test]
    fn tangent_drags_stay_smooth_unless_alt_is_held() {
        let mut d = doc("roto", json!({ "points": spline_square() }));
        let mut ov = Overlays::default();
        select(&mut ov, &mut d, Pos2::new(150.0, 25.0));
        // Vertex 1's out handle tip is 20 px right of it; pull it straight down.
        let tip = Pos2::new(170.0, 25.0);
        let part = gesture(&mut ov, &mut d, tip, Pos2::new(150.0, 45.0), 1, Mods::default());
        assert_eq!(part, Some(Part::Tangent { i: 1, out: true }));
        assert_eq!(points(&d, 1)[6..12], [150.0, 25.0, 0.0, -20.0, 0.0, 20.0]);
        // With Alt, only the dragged handle moves.
        let alt = Mods { alt: true, ..Default::default() };
        gesture(&mut ov, &mut d, Pos2::new(150.0, 45.0), Pos2::new(160.0, 45.0), 1, alt);
        assert_eq!(points(&d, 1)[6..12], [150.0, 25.0, 0.0, -20.0, 10.0, 20.0]);
    }

    #[test]
    fn new_spline_vertices_pull_out_tangents() {
        let mut d = doc("roto", json!({ "points": spline_square() }));
        let mut ov = Overlays::default();
        let ctrl = Mods { command: true, ..Default::default() };
        let part = gesture(&mut ov, &mut d, Pos2::new(20.0, 50.0), Pos2::new(20.0, 40.0), 1, ctrl);
        assert_eq!(part, Some(Part::Pull(4)));
        assert_eq!(points(&d, 1)[24..], [20.0, 50.0, 0.0, 10.0, 0.0, -10.0]);
        // Alt-drag on an existing vertex replaces its tangents.
        let alt = Mods { alt: true, ..Default::default() };
        gesture(&mut ov, &mut d, Pos2::new(50.0, 75.0), Pos2::new(40.0, 75.0), 1, alt);
        assert_eq!(points(&d, 1)[18..24], [50.0, 75.0, 10.0, 0.0, -10.0, 0.0]);
        // Moving a vertex carries its handles.
        gesture(&mut ov, &mut d, Pos2::new(50.0, 75.0), Pos2::new(55.0, 80.0), 1, Mods::default());
        assert_eq!(points(&d, 1)[18..24], [55.0, 80.0, 10.0, 0.0, -10.0, 0.0]);
    }
}
