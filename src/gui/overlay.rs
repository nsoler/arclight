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
use arclight::color;
use arclight::image::Image;
use std::sync::Arc;
use arclight::{anim, shape, track};
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
/// Length of a Light 2D's cone edges and aim, in screen points.
const BEAM: f32 = 90.0;
/// Fewest vertices a point list keeps when deleting.
const MIN_POINTS: usize = 3;
/// Stride of a spline's points: x, y, in-handle and out-handle offsets.
const SPLINE: usize = 6;
/// Samples per spline segment when hit-testing and drawing it.
const SAMPLES: usize = 48;
/// A Tracker's track position params, in track order.
const TRACK_PARAMS: [&str; track::TRACKS] = ["track1", "track2", "track3", "track4"];

const HANDLE: Color32 = Color32::from_rgb(240, 200, 80);
const HOT: Color32 = Color32::WHITE;
/// Tracked frames whose match was weak.
const WEAK: Color32 = Color32::from_rgb(240, 120, 50);
const X_AXIS: Color32 = Color32::from_rgb(235, 90, 80);
const Y_AXIS: Color32 = Color32::from_rgb(110, 210, 100);

/// Image ↔ screen mapping: the image's screen rect and points per pixel,
/// and where the shown image puts the overlay's points (`warp`: e.g. a
/// Tracker's input points in its match-moved output).
#[derive(Clone, Copy)]
pub struct Xf {
    pub img_rect: Rect,
    pub zoom: f32,
    pub warp: Option<track::Affine>,
}

impl Xf {
    pub fn new(img_rect: Rect, zoom: f32) -> Xf {
        Xf { img_rect, zoom, warp: None }
    }

    pub(crate) fn screen(&self, p: Vec2) -> Pos2 {
        let p = match &self.warp {
            Some(m) => Vec2::from(track::apply(m, p.into())),
            None => p,
        };
        self.img_rect.min + p * self.zoom
    }

    fn image(&self, s: Pos2) -> Vec2 {
        let p = (s - self.img_rect.min) / self.zoom;
        match &self.warp {
            Some(m) => Vec2::from(track::apply(&track::invert(m), p.into())),
            None => p,
        }
    }

    /// This mapping for the selected node's overlay: a Tracker shown
    /// through its own match-move or stabilize draws its tracks where its
    /// output shows them.
    fn for_node(&self, doc: &Doc, id: &str, frame: i64) -> Xf {
        let n = doc.node(id).filter(|n| n.op == "tracker" && doc.viewed().as_deref() == Some(id));
        Xf { warp: n.and_then(|n| track::output_map(&n.params, frame)), ..*self }
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
    /// Tracker track `i`: its pattern box moves the track.
    Track(usize),
    /// A corner of track `i`'s pattern (or search) box: sizes that box for
    /// every track.
    TrackBox { i: usize, search: bool },
    /// Track `i`'s boxes alone, away from its point (Ctrl-drag): its offset.
    TrackOffset(usize),
    /// A selection rectangle over a point list's vertices, dragged from
    /// empty space.
    Marquee,
    /// The transform box around two or more selected vertices.
    SelBox(BoxHandle),
    /// Corner `i` of a corner pin's To quad, or of its From quad.
    Corner { i: usize, from: bool },
    /// Picking a color from the node's input (Ctrl+click / Ctrl+drag).
    Pick,
    /// Edge `i` of a point list, pressed at fraction `t`: dragging moves its
    /// two vertices; a click without moving inserts a vertex there.
    EdgeMove { i: usize, t: f32 },
    /// Inside a point list's shape: dragging moves the whole shape.
    Body,
    /// A Light 2D's aim (its direction), one of its cone's edges, the
    /// inner edge of its softness, and its radius (a point light's) or
    /// gradient (a directional light's).
    LightAim,
    LightCone,
    LightSoft,
    LightReach,
}

/// A handle of the selected vertices' transform box.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BoxHandle {
    /// Inside the box: move the vertices.
    Move,
    /// The stem above the top edge: turn them about the box center.
    Rotate,
    /// A corner (`x` and `y` both ±1) or an edge (one of them 0): scale
    /// about the box center.
    Scale { x: i8, y: i8 },
}

/// Gap between the selected vertices and their transform box, in screen
/// points, so the box's handles don't sit on the vertices.
const BOX_PAD: f32 = 12.0;

/// The transform box around selected vertices `sel` of `pts`, in image
/// pixels (padded), or `None` with fewer than two selected.
fn sel_box(xf: &Xf, pts: &[Vec2], sel: &[usize]) -> Option<Rect> {
    let mut chosen = sel.iter().filter_map(|&i| pts.get(i));
    let first = *chosen.next()?;
    chosen.next()?;
    let r = sel.iter().filter_map(|&i| pts.get(i)).fold(Rect::from_min_max(first.to_pos2(), first.to_pos2()), |r, p| r.union(Rect::from_min_max(p.to_pos2(), p.to_pos2())));
    Some(r.expand(BOX_PAD / xf.zoom))
}

/// The transform box's scale handles (screen position, handle) and the tip
/// of its rotation stem.
fn sel_box_handles(xf: &Xf, r: Rect) -> (Vec<(Pos2, BoxHandle)>, Pos2) {
    let c = r.center();
    let at = |x: i8, y: i8| xf.screen(Vec2::new(c.x + x as f32 * r.width() / 2.0, c.y + y as f32 * r.height() / 2.0));
    let sides = [(-1, -1), (1, -1), (1, 1), (-1, 1), (0, -1), (1, 0), (0, 1), (-1, 0)];
    let handles = sides.iter().map(|&(x, y)| (at(x, y), BoxHandle::Scale { x, y })).collect();
    (handles, at(0, -1) - Vec2::new(0.0, STEM))
}

/// The selected node's overlay, resolved at the current frame.
#[derive(Clone)]
enum Geo {
    Point { param: &'static str, value: Vec2 },
    Transform {
        names: [&'static str; 3],
        /// The pivot and translate, in image pixels.
        center: Vec2,
        /// The center param is unset: the pivot is the layer's center.
        auto: bool,
        translate: Vec2,
        rotate: f32,
        scale: Vec2,
        /// The input's data (the layer being moved), before the transform:
        /// its data window when known, else the whole frame.
        layer: Rect,
    },
    Insets { names: [&'static str; 4], values: [f32; 4], size: Vec2 },
    Box { names: [&'static str; 3], center: Vec2, size: Vec2, rotate: f32, ellipse: bool },
    /// Vertices of a point list (`stride` numbers each, x and y first). A
    /// spline (stride 6) also has each vertex's in and out handle offsets
    /// in `tan`; other point lists leave it empty.
    Points { param: &'static str, stride: usize, pts: Vec<Vec2>, tan: Vec<[Vec2; 2]> },
    /// Tracker boxes: each track's position (`None` when it is off) and its
    /// `[pattern, search]` box sizes and its boxes' offset from its point,
    /// in image pixels.
    Tracker { names: [&'static str; 2], pos: Vec<Option<Vec2>>, sizes: Vec<[f32; 2]>, offsets: Vec<Vec2> },
    /// Corner pin quads, in image pixels.
    CornerPin { names: [[&'static str; 4]; 2], to: [Vec2; 4], from: [Vec2; 4] },
    /// A color param to pick from the input.
    Pick { param: &'static str },
    /// A Light 2D (not an area light, which is a [`Geo::Box`]): its type
    /// (spot, point or directional), position, direction, cone and
    /// softness (degrees), and radius (point) or gradient (directional).
    Light { kind: LightType, pos: Vec2, dir: f32, cone: f32, soft: f32, reach: f32 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum LightType {
    Spot,
    Point,
    Directional,
}

/// Where a Light 2D's handles are on screen: the aim, the cone's two edges,
/// the softness's two inner edges, and the radius or gradient (`None`
/// where the type has none).
struct LightHandles {
    aim: Option<Pos2>,
    cone: Vec<Pos2>,
    soft: Vec<Pos2>,
    reach: Option<Pos2>,
}

fn light_handles(xf: &Xf, kind: LightType, pos: Vec2, dir: f32, cone: f32, soft: f32, reach: f32) -> LightHandles {
    let c = xf.screen(pos);
    let along = |deg: f32, len: f32| c + rot(Vec2::X, deg) * len;
    match kind {
        LightType::Spot => {
            let inner = (cone / 2.0 - soft / 2.0).max(0.0);
            LightHandles {
                aim: Some(along(dir, BEAM * 1.35)),
                cone: vec![along(dir - cone / 2.0, BEAM), along(dir + cone / 2.0, BEAM)],
                soft: vec![along(dir - inner, BEAM * 0.7), along(dir + inner, BEAM * 0.7)],
                reach: None,
            }
        }
        LightType::Point => LightHandles { aim: None, cone: Vec::new(), soft: Vec::new(), reach: Some(along(0.0, reach * xf.zoom)) },
        LightType::Directional => LightHandles { aim: Some(along(dir, BEAM * 1.35)), cone: Vec::new(), soft: Vec::new(), reach: Some(along(dir, reach * xf.zoom)) },
    }
}

impl Geo {
    /// What rotation handles turn about.
    fn pivot(&self) -> Vec2 {
        match *self {
            Geo::Transform { center, translate, .. } => center + translate,
            Geo::Box { center, .. } => center,
            Geo::Light { pos, .. } => pos,
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
    /// The mapping when the drag started: a Tracker's shown output moves as
    /// its tracks are dragged, so the pointer is read through this one.
    xf: Xf,
    /// Pointer now, in image pixels.
    now: Vec2,
    /// What rotation turns about.
    pivot: Vec2,
}

impl Drag {
    /// Moved more than a few screen pixels since the press: a drag, not a
    /// click.
    fn moved(&self) -> bool {
        (self.now - self.start).length() * self.xf.zoom > 3.0
    }
}

#[derive(Default)]
pub struct Overlays {
    drag: Option<Drag>,
    /// Selected vertices of the selected node's point list, and that node.
    points: Vec<usize>,
    points_node: Option<String>,
    /// The selection a box selection adds to (empty unless Shift was held).
    marquee_base: Vec<usize>,
    /// A node and its input's image, when known: a Transform's layer is
    /// its data window, a picker samples it.
    input: Option<(String, Arc<Image>)>,
    /// A solved Camera Tracker's points, to select and place things on.
    picker: crate::camera_tracker::PointPicker,
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
    if op == "particle_emitter" {
        return emitter_geo(doc, id, frame);
    }
    Some(match ops::info(op)?.overlay {
        // Drawn by its own pass (see `camera_points`).
        Overlay::None | Overlay::CameraTracker => return None,
        Overlay::Point(param) => Geo::Point { param, value: vec2(&at(param)) },
        Overlay::Pick(param) => Geo::Pick { param },
        Overlay::Transform { translate, rotate, scale, center } => Geo::Transform {
            names: [translate, rotate, scale],
            auto: at(center).is_null(),
            center: if at(center).is_null() { size / 2.0 } else { vec2(&at(center)) },
            translate: vec2(&at(translate)),
            rotate: num(&at(rotate)),
            scale: vec2(&at(scale)),
            layer: Rect::from_min_size(Pos2::ZERO, size),
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
        Overlay::Tracker { pattern, search } => Geo::Tracker {
            names: [pattern, search],
            pos: track::positions(&doc.node(id)?.params, frame).into_iter().map(|p| p.map(|[x, y]| Vec2::new(x, y))).collect(),
            sizes: track::box_sizes(&doc.node(id)?.params),
            offsets: track::offsets(&doc.node(id)?.params).into_iter().map(Vec2::from).collect(),
        },
        Overlay::CornerPin { to, from } => Geo::CornerPin {
            names: [to, from],
            to: to.map(|n| vec2(&at(n))),
            from: from.map(|n| vec2(&at(n))),
        },
        // An area light's box is a Box's; the others have their own handles.
        Overlay::Light2d => match at("type").as_str() {
            Some("area") => Geo::Box { names: ["position", "size", "rotation"], center: vec2(&at("position")), size: vec2(&at("size")), rotate: num(&at("rotation")), ellipse: at("shape").as_str() == Some("ellipse") },
            kind => {
                let kind = match kind {
                    Some("point") => LightType::Point,
                    Some("directional") => LightType::Directional,
                    _ => LightType::Spot,
                };
                let reach = num(&at(if kind == LightType::Point { "radius" } else { "gradient" }));
                Geo::Light { kind, pos: vec2(&at("position")), dir: num(&at("direction")), cone: num(&at("cone")), soft: num(&at("softness")), reach }
            }
        },
    })
}

/// A Particle Emitter's handles, when it feeds a Particles 2D that's being
/// viewed (its particles are then frame pixels): a rectangle's, disk's or
/// line's box (Translate's x, y, Dimensions and Rotate's z), else a point
/// to move it by.
fn emitter_geo(doc: &Doc, id: &str, frame: i64) -> Option<Geo> {
    let viewed = doc.viewed()?;
    let flat = doc.graph.nodes.iter().any(|n| n.op == "particles_2d" && doc.is_upstream(id, &n.id) && doc.is_upstream(&n.id, &viewed));
    if !flat {
        return None;
    }
    let at = |name| doc.param_at(id, name, frame);
    let center = vec2(&at("translate"));
    let d = vec2(&at("dimensions"));
    let turn = at("rotate").get(2).map_or(0.0, num);
    let names = ["translate", "dimensions", "rotate"];
    Some(match at("shape").as_str() {
        Some("rectangle") => Geo::Box { names, center, size: d, rotate: turn, ellipse: false },
        Some("disk") => Geo::Box { names, center, size: Vec2::splat(d.x), rotate: turn, ellipse: true },
        Some("line") => Geo::Box { names, center, size: Vec2::new(d.x, 1.0), rotate: turn, ellipse: false },
        _ => Geo::Point { param: "translate", value: center },
    })
}

/// Write `v` to `node`'s param `name` at `frame` (see [`Doc::edit_param`]):
/// into the first components of a longer list (x and y of an emitter's 3D
/// Translate), or a number into a list's last (an emitter's turn about z).
fn put(doc: &mut Doc, node: &str, name: &str, frame: i64, v: Value) {
    let old = doc.param_at(node, name, frame);
    let v = match (old.as_array(), &v) {
        (Some(o), Value::Array(n)) if o.len() > n.len() => {
            let mut o = o.clone();
            o[..n.len()].clone_from_slice(n);
            Value::Array(o)
        }
        (Some(o), Value::Number(_)) if !o.is_empty() => {
            let mut o = o.clone();
            *o.last_mut().unwrap() = v.clone();
            Value::Array(o)
        }
        _ => v,
    };
    doc.edit_param(node, name, frame, v);
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

/// Screen positions of the transformed layer's frame: its corners
/// (clockwise from top left), then the midpoints of its top, right, bottom
/// and left edges. The layer is taken to be the viewed image's size.
fn layer_frame(xf: &Xf, center: Vec2, layer: Rect, translate: Vec2, rotate: f32, scale: Vec2) -> [Pos2; 8] {
    // A point of the layer (x, y from −1 to 1 across it), transformed.
    let at = |x: f32, y: f32| {
        let q = layer.center().to_vec2() + Vec2::new(x, y) * layer.size() / 2.0;
        xf.screen(center + translate + rot((q - center) * scale, rotate))
    };
    [at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0), at(0.0, -1.0), at(1.0, 0.0), at(0.0, 1.0), at(-1.0, 0.0)]
}

/// Which axes each [`layer_frame`] handle scales.
const LAYER_PARTS: [Part; 8] = [
    Part::Size { x: true, y: true },
    Part::Size { x: true, y: true },
    Part::Size { x: true, y: true },
    Part::Size { x: true, y: true },
    Part::Size { x: false, y: true },
    Part::Size { x: true, y: false },
    Part::Size { x: false, y: true },
    Part::Size { x: true, y: false },
];

/// The screen corners of a square box of `size` image pixels centered on
/// `c`, clockwise from top left.
fn square(xf: &Xf, c: Vec2, size: f32) -> [Pos2; 4] {
    let h = size / 2.0;
    [(-h, -h), (h, -h), (h, h), (-h, h)].map(|(x, y)| xf.screen(c + Vec2::new(x, y)))
}

/// Whether `p` lies inside the convex quad `q` (either winding).
/// Inside a closed outline (even-odd), any shape.
fn in_polygon(p: Pos2, poly: &[Pos2]) -> bool {
    let mut inside = false;
    for (j, &a) in poly.iter().enumerate() {
        let b = poly[(j + 1) % poly.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            inside = !inside;
        }
    }
    inside
}

fn in_quad(p: Pos2, q: &[Pos2]) -> bool {
    let side = |a: Pos2, b: Pos2| (b - a).x * (p - a).y - (b - a).y * (p - a).x;
    let s: Vec<f32> = (0..4).map(|i| side(q[i], q[(i + 1) % 4])).collect();
    s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)
}

/// The crop box in screen space.
fn inset_rect(xf: &Xf, v: [f32; 4], size: Vec2) -> Rect {
    Rect::from_min_max(xf.screen(Vec2::new(v[0], v[1])), xf.screen(Vec2::new(size.x - v[2], size.y - v[3])))
}

/// The handle at screen position `p`. `sel` are the selected vertices of a
/// point list, whose tangent handles are live.
fn hit(g: &Geo, xf: &Xf, p: Pos2, sel: &[usize]) -> Option<Part> {
    match *g {
        Geo::Light { kind, pos, dir, cone, soft, reach } => {
            let h = light_handles(xf, kind, pos, dir, cone, soft, reach);
            let near = |q: &Pos2| q.distance(p) < GRAB;
            if h.aim.as_ref().is_some_and(near) {
                Some(Part::LightAim)
            } else if h.cone.iter().any(near) {
                Some(Part::LightCone)
            } else if h.soft.iter().any(near) {
                Some(Part::LightSoft)
            } else if h.reach.as_ref().is_some_and(near) {
                Some(Part::LightReach)
            } else {
                (xf.screen(pos).distance(p) < GRAB * 1.5).then_some(Part::Move)
            }
        }
        Geo::Point { value, .. } => (xf.screen(value).distance(p) < GRAB * 1.5).then_some(Part::Point),
        // No handles: Ctrl+click picks (see `Overlays::press`).
        Geo::Pick { .. } => None,
        Geo::CornerPin { to, from, .. } => {
            // To corners first: they're what's usually adjusted.
            let near = |q: &[Vec2; 4]| (0..4).find(|&i| xf.screen(q[i]).distance(p) < GRAB * 1.5);
            near(&to).map(|i| Part::Corner { i, from: false }).or_else(|| near(&from).map(|i| Part::Corner { i, from: true }))
        }
        Geo::Transform { center, translate, rotate, scale, layer, .. } => {
            let (c, x, y) = gizmo(xf, center, translate, rotate);
            let frame = layer_frame(xf, center, layer, translate, rotate, scale);
            if x.distance(p) < GRAB {
                Some(Part::ScaleX)
            } else if y.distance(p) < GRAB {
                Some(Part::ScaleY)
            } else if c.distance(p) < GRAB * 1.5 {
                Some(Part::Move)
            } else if let Some(i) = (0..8).find(|&i| frame[i].distance(p) < GRAB) {
                // Before the ring, which can run over a small layer's corners.
                Some(LAYER_PARTS[i])
            } else if (c.distance(p) - RING).abs() < GRAB {
                Some(Part::Rotate)
            } else if in_quad(p, &frame[..4]) {
                // Anywhere on the layer moves it.
                Some(Part::Move)
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
        Geo::Box { center, size, rotate, ellipse, .. } => {
            let (handles, spin) = box_handles(xf, center, size, rotate);
            if spin.distance(p) < GRAB {
                return Some(Part::Rotate);
            }
            if let Some(&(_, part)) = handles.iter().find(|(h, _)| h.distance(p) < GRAB) {
                return Some(part);
            }
            // The center, or anywhere inside the shape, moves it.
            let inside = in_polygon(p, &box_outline(xf, center, size, rotate, ellipse));
            (inside || xf.screen(center).distance(p) < GRAB * 1.5).then_some(Part::Move)
        }
        Geo::Tracker { ref pos, ref sizes, ref offsets, .. } => {
            // Later tracks draw on top, so they take the pointer first.
            for (i, point) in pos.iter().enumerate().rev() {
                let Some(point) = *point else { continue };
                let [pattern, search] = sizes[i];
                let c = point + offsets[i];
                // The nearer corner, as the boxes' corners can nearly meet.
                let corner = [(pattern, false), (search, true)]
                    .into_iter()
                    .map(|(size, is_search)| (square(xf, c, size).iter().map(|q| q.distance(p)).fold(f32::MAX, f32::min), is_search))
                    .filter(|&(d, _)| d < GRAB)
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((_, is_search)) = corner {
                    return Some(Part::TrackBox { i, search: is_search });
                }
                // The box may be turned when shown through the tracker's output.
                if in_quad(p, &square(xf, c, pattern)) || xf.screen(point).distance(p) < GRAB {
                    return Some(Part::Track(i));
                }
            }
            None
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
            let sbox = sel_box(xf, pts, sel);
            if let Some(r) = sbox {
                let (handles, stem) = sel_box_handles(xf, r);
                if stem.distance(p) < GRAB {
                    return Some(Part::SelBox(BoxHandle::Rotate));
                }
                if let Some(&(_, h)) = handles.iter().find(|(q, _)| q.distance(p) < GRAB) {
                    return Some(Part::SelBox(h));
                }
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
                // Anywhere else inside the transform box moves the selection…
                .or_else(|| sbox.filter(|r| r.contains(xf.image(p).to_pos2())).map(|_| Part::SelBox(BoxHandle::Move)))
                // …and inside the shape, the whole shape.
                .or_else(|| (s.len() >= 3 && in_polygon(p, &line)).then_some(Part::Body))
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
        self.drag.is_some() || self.picker.dragging()
    }

    /// Tell the overlays a node's input image (or that none is known).
    pub fn set_input(&mut self, input: Option<(String, Arc<Image>)>) {
        self.input = input;
    }

    /// The selected node's input, averaged (straight color) over the
    /// pixels the image-space box from `a` to `b` touches. None when it
    /// isn't known or is transparent there.
    fn sample_input(&self, id: &str, a: Vec2, b: Vec2) -> Option<[f32; 4]> {
        let img = self.input.as_ref().filter(|(n, _)| n == id).map(|(_, img)| img)?;
        let (lo, hi) = (a.min(b), a.max(b));
        let (x0, y0) = (lo.x.floor() as i32, lo.y.floor() as i32);
        let (x1, y1) = ((hi.x.floor() as i32).max(x0), (hi.y.floor() as i32).max(y0));
        let mut sum = [0.0f64; 4];
        for y in y0..=y1 {
            for x in x0..=x1 {
                for (s, v) in sum.iter_mut().zip(img.at(x, y)) {
                    *s += v as f64;
                }
            }
        }
        (sum[3] > 0.0).then(|| {
            let [r, g, b, _] = color::unpremultiply(sum.map(|v| v as f32));
            [r, g, b, 1.0]
        })
    }

    /// Set the picked color from the box `drag` spans.
    fn pick(&self, drag: &Drag, doc: &mut Doc, frame: i64) {
        let Geo::Pick { param } = drag.geo else { return };
        if let Some(c) = self.sample_input(&drag.node, drag.start, drag.now) {
            let round = |v: f32| (v as f64 * 1e4).round() / 1e4;
            doc.edit_param(&drag.node, param, frame, json!(c.map(round)));
        }
    }

    /// The node's overlay at `frame`; a Transform's layer is its input's
    /// data window when known.
    fn node_geo(&self, doc: &Doc, id: &str, frame: i64, size: Vec2) -> Option<Geo> {
        let mut g = geo(doc, id, frame, size)?;
        if let (Geo::Transform { layer, center, auto, .. }, Some((node, img))) = (&mut g, &self.input) {
            let w = img.window();
            if node == id && !w.is_empty() {
                *layer = Rect::from_min_max(Pos2::new(w.x0 as f32, w.y0 as f32), Pos2::new(w.x1 as f32, w.y1 as f32));
                if *auto {
                    // Where the op pivots (`ops::transform_pivot`).
                    *center = layer.center().to_vec2();
                }
            }
        }
        Some(g)
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
        if self.picker.interact(ui, resp, xf, doc, frame) {
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
            let xf = self.drag.as_ref().map_or(*xf, |d| d.xf);
            self.drag_to(doc, frame, xf.image(ptr), mods);
        }
        if released || resp.drag_stopped() {
            self.release(doc);
        }
    }

    /// The pointer came up: end the drag. An edge clicked without moving
    /// gets a new vertex where it was clicked, in every key.
    fn release(&mut self, doc: &mut Doc) {
        let Some(drag) = self.drag.take() else { return };
        let (Part::EdgeMove { i, t }, Geo::Points { param, stride, pts, .. }) = (drag.part, &drag.geo) else { return };
        if drag.moved() {
            return;
        }
        let (n, stride, spline) = (pts.len(), *stride, *stride == SPLINE);
        doc.set_param(&drag.node, param, anim::edit_all(&doc.param(&drag.node, param), |v| {
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
    }

    /// The pointer went down at screen position `o`: grab the handle there,
    /// or, on a point list, select / insert vertices.
    fn press(&mut self, doc: &mut Doc, frame: i64, size: Vec2, xf: &Xf, o: Pos2, mods: Mods) {
        self.drag = None;
        let Some(id) = doc.selected.clone() else { return };
        let Some(g) = self.node_geo(doc, &id, frame, size) else { return };
        let xf = &xf.for_node(doc, &id, frame);
        self.points.retain(|&i| matches!(&g, Geo::Points { pts, .. } if i < pts.len()));
        let mut part = hit(&g, xf, o, &self.points);
        if let (Some(Part::Track(i)), true) = (part, mods.command) {
            part = Some(Part::TrackOffset(i));
        }
        if let (Geo::Pick { .. }, true) = (&g, mods.command) {
            part = Some(Part::Pick);
        }
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
                    // Grab the edge's two vertices (Shift: as well as the
                    // selection); a click that doesn't move inserts a vertex
                    // instead (see `release`).
                    let ends = [i, (i + 1) % pts.len()];
                    if !mods.shift {
                        self.points.clear();
                    }
                    self.points.extend(ends.iter().filter(|e| !self.points.contains(e)).collect::<Vec<_>>());
                    part = Some(Part::EdgeMove { i, t });
                }
                Some(Part::Body) if mods.shift => {
                    // Shift-drag inside still box-selects, adding.
                    self.marquee_base = self.points.clone();
                    part = Some(Part::Marquee);
                }
                Some(Part::Body) => self.points = (0..pts.len()).collect(),
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
                None => {
                    // Empty space: box-select (Shift adds to the selection).
                    if !mods.shift {
                        self.points.clear();
                    }
                    self.marquee_base = self.points.clone();
                    part = Some(Part::Marquee);
                }
                _ => {}
            }
        }
        let Some(part) = part else { return };
        // Inserting changed the geometry: start from the new one.
        let Some(g) = self.node_geo(doc, &id, frame, size) else { return };
        let start = xf.image(o);
        let pivot = match (&g, part) {
            (Geo::Points { pts, .. }, Part::SelBox(_)) => sel_box(xf, pts, &self.points).map_or(Vec2::ZERO, |r| r.center().to_vec2()),
            _ => g.pivot(),
        };
        let d = start - pivot;
        let drag = Drag { node: id, part, start, geo: g, angle: d.y.atan2(d.x), turned: 0.0, xf: *xf, now: start, pivot };
        if part == Part::Pick {
            // A click picks the pixel under it.
            self.pick(&drag, doc, frame);
        }
        self.drag = Some(drag);
    }

    fn drag_to(&mut self, doc: &mut Doc, frame: i64, p: Vec2, mods: Mods) {
        let Some(drag) = &mut self.drag else { return };
        drag.now = p;
        if drag.part == Part::Pick {
            let drag = self.drag.take().unwrap();
            self.pick(&drag, doc, frame);
            self.drag = Some(drag);
            return;
        }
        if matches!(drag.part, Part::EdgeMove { .. }) && !drag.moved() {
            return;
        }
        if drag.part != Part::Marquee {
            return apply(drag, &self.points, p, mods, doc, frame);
        }
        let Geo::Points { pts, .. } = &drag.geo else { return };
        let r = Rect::from_two_pos(drag.start.to_pos2(), p.to_pos2());
        self.points = self.marquee_base.clone();
        self.points.extend((0..pts.len()).filter(|&i| r.contains(pts[i].to_pos2()) && !self.marquee_base.contains(&i)));
    }

    /// Select every vertex of the selected node's point list. False when it
    /// has none.
    pub fn select_all_points(&mut self, doc: &Doc) -> bool {
        self.sync_selection(doc);
        let Some(Geo::Points { pts, .. }) = doc.selected.as_deref().and_then(|id| geo(doc, id, 0, Vec2::ZERO)) else { return false };
        self.points = (0..pts.len()).collect();
        true
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
        self.picker.draw(painter, xf, doc, frame);
        let Some(id) = doc.selected.as_deref() else { return };
        let Some(g) = self.node_geo(doc, id, frame, size) else { return };
        let xf = &self.drag.as_ref().map_or_else(|| xf.for_node(doc, id, frame), |d| d.xf);
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
        if let Some(d) = self.drag.as_ref().filter(|d| d.part == Part::Marquee) {
            let r = Rect::from_two_pos(xf.screen(d.start), xf.screen(d.now));
            painter.rect(r, 0.0, Color32::from_white_alpha(18), Stroke::new(1.0, Color32::from_white_alpha(170)), egui::StrokeKind::Middle);
        }

        match g {
            Geo::Pick { .. } => {
                // The box being averaged; a crosshair while Ctrl is held.
                if let Some(d) = self.drag.as_ref().filter(|d| d.part == Part::Pick) {
                    let r = Rect::from_two_pos(xf.screen(d.start.floor()), xf.screen(d.now.floor() + Vec2::splat(1.0)));
                    painter.rect_stroke(r, 0.0, Stroke::new(1.0, HOT), egui::StrokeKind::Middle);
                }
                if hover.is_some() && ui.input(|i| i.modifiers.command) {
                    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                }
            }
            Geo::Light { kind, pos, dir, cone, soft, reach } => {
                let c = xf.screen(pos);
                let h = light_handles(xf, kind, pos, dir, cone, soft, reach);
                let line = |a: Pos2, b: Pos2, col: Color32, w: f32| {
                    painter.line_segment([a, b], Stroke::new(w + 1.5, Color32::from_black_alpha(140)));
                    painter.line_segment([a, b], Stroke::new(w, col));
                };
                let faint = Color32::from_white_alpha(110);
                let along = |deg: f32, len: f32| c + rot(Vec2::X, deg) * len;
                match kind {
                    LightType::Spot => {
                        // The cone's edges and an arc across them; fainter, where
                        // the softness starts and ends.
                        for e in &h.cone {
                            line(c, *e, color(Part::LightCone, HANDLE), 1.5);
                        }
                        let half = cone / 2.0;
                        for a in [half - soft / 2.0, half + soft / 2.0].into_iter().filter(|a| *a > 0.0 && soft > 0.0) {
                            line(c, along(dir - a, BEAM * 0.85), faint, 1.0);
                            line(c, along(dir + a, BEAM * 0.85), faint, 1.0);
                        }
                        let arc: Vec<Pos2> = (0..=24).map(|i| along(dir - half + cone * i as f32 / 24.0, BEAM)).collect();
                        painter.add(Shape::line(arc, Stroke::new(1.0, faint)));
                    }
                    LightType::Point => {
                        let r = reach * xf.zoom;
                        painter.circle_stroke(c, r, Stroke::new(3.0, Color32::from_black_alpha(140)));
                        painter.circle_stroke(c, r, Stroke::new(1.5, color(Part::LightReach, HANDLE)));
                    }
                    LightType::Directional => {
                        // The line where it starts to fade, and where it's gone.
                        let across = rot(Vec2::X, dir + 90.0) * (size.length() * xf.zoom);
                        line(c - across, c + across, HANDLE, 1.5);
                        if let Some(g) = h.reach {
                            line(g - across, g + across, color(Part::LightReach, faint), 1.0);
                        }
                    }
                }
                if let Some(a) = h.aim {
                    line(c, a, color(Part::LightAim, HANDLE), 1.5);
                    painter.circle(a, 5.0, color(Part::LightAim, HANDLE), outline);
                }
                for q in &h.cone {
                    painter.rect(Rect::from_center_size(*q, Vec2::splat(8.0)), 1.0, color(Part::LightCone, HANDLE), outline, egui::StrokeKind::Middle);
                }
                for q in &h.soft {
                    painter.circle(*q, 4.0, color(Part::LightSoft, faint), outline);
                }
                if let Some(q) = h.reach {
                    painter.rect(Rect::from_center_size(q, Vec2::splat(8.0)), 1.0, color(Part::LightReach, HANDLE), outline, egui::StrokeKind::Middle);
                }
                painter.circle(c, 6.0, color(Part::Move, HANDLE), outline);
                crosshair(painter, c, 11.0, Stroke::new(1.0, color(Part::Move, HANDLE)));
            }
            Geo::Point { value, .. } => {
                let c = xf.screen(value);
                painter.circle(c, 5.0, color(Part::Point, HANDLE), outline);
                crosshair(painter, c, 10.0, Stroke::new(1.0, color(Part::Point, HANDLE)));
            }
            Geo::Transform { center, translate, rotate, scale, layer, .. } => {
                // The layer's frame, and its scale handles.
                let layer = layer_frame(xf, center, layer, translate, rotate, scale);
                let hot_move = hot == Some(Part::Move);
                outline_path(painter, layer[..4].to_vec(), if hot_move { HOT } else { Color32::from_white_alpha(170) });
                if let Some(Some(i)) = hover.filter(|_| self.drag.is_none()).map(|p| (0..8).find(|&i| layer[i].distance(p) < GRAB)) {
                    // Only the handle under the pointer lights up, not every one sharing its part.
                    painter.rect(Rect::from_center_size(layer[i], Vec2::splat(9.0)), 1.0, HOT, outline, egui::StrokeKind::Middle);
                }
                for &h in &layer {
                    painter.rect(Rect::from_center_size(h, Vec2::splat(7.0)), 1.0, HANDLE, outline, egui::StrokeKind::Middle);
                }
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
            Geo::CornerPin { to, from, .. } => {
                let quad = |q: [Vec2; 4]| q.map(|v| xf.screen(v));
                let mut ring = quad(from).to_vec();
                ring.push(ring[0]);
                painter.add(Shape::dashed_line(&ring, Stroke::new(1.0, Color32::from_white_alpha(150)), 5.0, 4.0));
                outline_path(painter, quad(to).to_vec(), HANDLE);
                for (i, (t, f)) in quad(to).into_iter().zip(quad(from)).enumerate() {
                    let fill = color(Part::Corner { i, from: true }, Color32::from_white_alpha(150));
                    painter.rect(Rect::from_center_size(f, Vec2::splat(6.0)), 1.0, fill, outline, egui::StrokeKind::Middle);
                    let col = color(Part::Corner { i, from: false }, HANDLE);
                    painter.circle(t, 5.0, col, outline);
                    painter.text(t + Vec2::new(7.0, -7.0), egui::Align2::LEFT_BOTTOM, format!("{}", i + 1), egui::FontId::proportional(12.0), col);
                }
            }
            Geo::Tracker { pos, sizes, offsets, .. } => {
                let node = doc.node(id);
                for (i, point) in pos.iter().enumerate() {
                    let Some(point) = *point else { continue };
                    let [pattern, search] = sizes[i];
                    let c = point + offsets[i];
                    // The tracked path: the position at every keyed frame.
                    let p = node.map(|n| ops::params_with_defaults(&n.op, &n.params)).unwrap_or_default();
                    let (raw, scores) = (&p[TRACK_PARAMS[i]], &p[format!("track{}_score", i + 1)]);
                    let weak = track::weak_below(p["min_score"].as_f64().unwrap_or(0.7) as f32) as f64;
                    let frames = anim::key_frames(raw);
                    let path: Vec<Pos2> = frames.iter().map(|&f| xf.screen(vec2(&json!(anim::sample(raw, f as f64))))).collect();
                    if path.len() >= 2 {
                        painter.add(Shape::line(path.clone(), Stroke::new(1.0, Color32::from_rgba_unmultiplied(240, 200, 80, 120))));
                        for (&q, &f) in path.iter().zip(&frames) {
                            // Frames that barely matched stand out.
                            if anim::has_key_at(scores, f) && anim::sample(scores, f as f64)[0] < weak {
                                painter.circle_filled(q, 3.0, WEAK);
                            } else {
                                painter.circle_filled(q, 1.5, Color32::from_rgba_unmultiplied(240, 200, 80, 160));
                            }
                        }
                    }
                    let hot_track = hot == Some(Part::Track(i)) || hot == Some(Part::TrackOffset(i));
                    let col = if hot_track { HOT } else { HANDLE };
                    let sbox = square(xf, c, search);
                    let mut ring = sbox.to_vec();
                    ring.push(sbox[0]);
                    painter.add(Shape::dashed_line(&ring, Stroke::new(1.0, Color32::from_white_alpha(150)), 5.0, 4.0));
                    let pbox = square(xf, c, pattern);
                    outline_path(painter, pbox.to_vec(), col);
                    for (corners, is_search) in [(pbox, false), (sbox, true)] {
                        let fill = if hot == Some(Part::TrackBox { i, search: is_search }) { HOT } else { HANDLE };
                        for q in corners {
                            painter.rect(Rect::from_center_size(q, Vec2::splat(6.0)), 1.0, fill, outline, egui::StrokeKind::Middle);
                        }
                    }
                    if offsets[i] != Vec2::ZERO {
                        // The point the track reports, tied to the feature it follows.
                        painter.add(Shape::dashed_line(&[xf.screen(c), xf.screen(point)], Stroke::new(1.0, col), 4.0, 3.0));
                        crosshair(painter, xf.screen(c), 3.0, Stroke::new(1.0, Color32::from_white_alpha(150)));
                    }
                    crosshair(painter, xf.screen(point), 5.0, Stroke::new(1.0, col));
                    let label = pbox[0] + Vec2::new(0.0, -3.0);
                    painter.text(label, egui::Align2::LEFT_BOTTOM, format!("{}", i + 1), egui::FontId::proportional(12.0), col);
                }
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
                // The transform box, while no box selection is being drawn.
                if let (Some(r), false) = (sel_box(xf, &pts, self.selection(doc)), self.drag.as_ref().is_some_and(|d| d.part == Part::Marquee)) {
                    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|q| xf.screen(q.to_vec2()));
                    let mut ring = corners.to_vec();
                    ring.push(corners[0]);
                    painter.add(Shape::dashed_line(&ring, Stroke::new(1.0, color(Part::SelBox(BoxHandle::Move), Color32::from_white_alpha(170))), 5.0, 4.0));
                    let (handles, stem) = sel_box_handles(xf, r);
                    let top = xf.screen(Vec2::new(r.center().x, r.top()));
                    painter.line_segment([top, stem], Stroke::new(1.0, Color32::from_white_alpha(170)));
                    painter.circle(stem, 4.5, color(Part::SelBox(BoxHandle::Rotate), HANDLE), outline);
                    for (q, h) in handles {
                        painter.rect(Rect::from_center_size(q, Vec2::splat(6.0)), 1.0, color(Part::SelBox(h), HANDLE), outline, egui::StrokeKind::Middle);
                    }
                    crosshair(painter, xf.screen(r.center().to_vec2()), 5.0, Stroke::new(1.0, Color32::from_white_alpha(170)));
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
    let d = p - drag.pivot;
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
        Geo::Pick { .. } => {}
        Geo::Point { param, value } => {
            let v = value + delta;
            put(doc, &id, param, frame, json!([v.x, v.y]));
        }
        Geo::CornerPin { names, to, from } => {
            let Part::Corner { i, from: is_from } = drag.part else { return };
            let v = if is_from { from[i] } else { to[i] } + if shift { constrain(delta) } else { delta };
            doc.edit_param(&id, names[is_from as usize][i], frame, json!([v.x, v.y]));
        }
        Geo::Transform { names, center, translate, rotate, scale, .. } => {
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
                Part::Size { x, y } => {
                    // Frame handles, in the layer's own (rotated) axes.
                    let from = rot(drag.start - pivot, -rotate);
                    let to = rot(p - pivot, -rotate);
                    let ratio = |a: f32, b: f32| if a.abs() < 1e-3 { 1.0 } else { b / a };
                    let s = if x && y && !shift {
                        // Corners scale uniformly; Shift frees the aspect.
                        if from.length() < 1e-3 {
                            return;
                        }
                        scale * (to.length() / from.length())
                    } else {
                        Vec2::new(
                            if x { scale.x * ratio(from.x, to.x) } else { scale.x },
                            if y { scale.y * ratio(from.y, to.y) } else { scale.y },
                        )
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
        Geo::Light { kind, pos, dir, cone, .. } => {
            let to = p - pos;
            // The pointer's angle from the aim, either way (degrees).
            let off = || {
                let a = to.y.atan2(to.x).to_degrees() - dir;
                ((a + 180.0).rem_euclid(360.0) - 180.0).abs()
            };
            match drag.part {
                Part::Move => {
                    let c = pos + if shift { constrain(delta) } else { delta };
                    doc.edit_param(&id, "position", frame, json!([c.x, c.y]));
                }
                Part::LightAim => {
                    let mut deg = dir + turn(drag, p);
                    if shift {
                        deg = (deg / 15.0).round() * 15.0;
                    }
                    doc.edit_param(&id, "direction", frame, json!(deg));
                }
                Part::LightCone => doc.edit_param(&id, "cone", frame, json!((2.0 * off()).min(360.0))),
                Part::LightSoft => doc.edit_param(&id, "softness", frame, json!((2.0 * (cone / 2.0 - off())).clamp(0.0, cone.max(0.0)))),
                Part::LightReach if kind == LightType::Point => doc.edit_param(&id, "radius", frame, json!(to.length())),
                Part::LightReach => doc.edit_param(&id, "gradient", frame, json!(to.dot(rot(Vec2::X, dir)).max(0.0))),
                _ => {}
            }
        }
        Geo::Box { names, center, size, rotate, .. } => match drag.part {
            Part::Move => {
                let c = center + if shift { constrain(delta) } else { delta };
                put(doc, &id, names[0], frame, json!([c.x, c.y]));
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
                put(doc, &id, names[1], frame, json!([s.x.max(1.0), s.y.max(1.0)]));
            }
            Part::Rotate => {
                let mut deg = rotate + turn(drag, p);
                if shift {
                    deg = (deg / 15.0).round() * 15.0;
                }
                put(doc, &id, names[2], frame, json!(deg));
            }
            _ => {}
        },
        Geo::Tracker { names, pos, sizes, offsets } => match drag.part {
            Part::TrackOffset(i) => {
                let o = offsets[i] + if shift { constrain(delta) } else { delta };
                doc.set_param(&id, &format!("track{}_offset", i + 1), json!([o.x, o.y]));
            }
            Part::Track(i) => {
                let Some(c) = pos.get(i).copied().flatten() else { return };
                let q = c + if shift { constrain(delta) } else { delta };
                doc.edit_param(&id, TRACK_PARAMS[i], frame, json!([q.x, q.y]));
            }
            Part::TrackBox { i, search: is_search } => {
                let Some(c) = pos.get(i).copied().flatten().map(|p| p + offsets[i]) else { return };
                let [pattern, search] = sizes[i];
                // Symmetric about the track: the corner follows the pointer.
                let d = (p - c).abs();
                let size = (2.0 * d.x.max(d.y)).round();
                // The search box stays bigger than the pattern.
                let size = if is_search { size.clamp(pattern + 2.0, 511.0) } else { size.clamp(5.0, (search - 2.0).min(255.0)) };
                let which = if is_search { "search" } else { "pattern" };
                if shift {
                    // Every track: the shared size, dropping each one's own.
                    doc.edit_param(&id, names[is_search as usize], frame, json!(size as i64));
                    for i in 1..=track::TRACKS {
                        doc.remove_param(&id, &format!("track{i}_{which}"));
                    }
                } else {
                    doc.set_param(&id, &format!("track{}_{which}", i + 1), json!(size as i64));
                }
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
            // Where each selected vertex goes, and what its tangent offsets
            // (a vector, so never moved) become.
            let to: Box<dyn Fn(Vec2) -> Vec2> = match drag.part {
                Part::SelBox(BoxHandle::Rotate) => {
                    let deg = turn(drag, p);
                    let deg = if shift { (deg / 15.0).round() * 15.0 } else { deg };
                    let c = drag.pivot;
                    Box::new(move |q| c + rot(q - c, deg))
                }
                Part::SelBox(BoxHandle::Scale { x, y }) => {
                    let c = drag.pivot;
                    let (from, now) = (drag.start - c, p - c);
                    let ratio = |a: f32, b: f32| if a.abs() > 1e-3 { b / a } else { 1.0 };
                    let f = if x != 0 && y != 0 && !shift {
                        // Corners scale uniformly (Shift: each axis freely).
                        Vec2::splat(ratio(from.length(), now.length()))
                    } else {
                        Vec2::new(if x != 0 { ratio(from.x, now.x) } else { 1.0 }, if y != 0 { ratio(from.y, now.y) } else { 1.0 })
                    };
                    Box::new(move |q| c + (q - c) * f)
                }
                _ => {
                    let d = if shift { constrain(delta) } else { delta };
                    Box::new(move |q| q + d)
                }
            };
            let linear = |t: Vec2| to(t) - to(Vec2::ZERO);
            for &i in selected.iter().filter(|&&i| i < pts.len()) {
                let q = to(pts[i]);
                if let Some(c) = v.get_mut(i * stride..i * stride + 2) {
                    c.copy_from_slice(&[q.x as f64, q.y as f64]);
                }
                if let (Some(c), Some(&[hi, ho])) = (v.get_mut(i * stride + 2..i * stride + 6).filter(|_| stride == SPLINE), tan.get(i)) {
                    let (hi, ho) = (linear(hi), linear(ho));
                    c.copy_from_slice(&[hi.x, hi.y, ho.x, ho.y].map(|x| x as f64));
                }
            }
            doc.edit_param(&id, param, frame, json!(v));
        }
    }
}

fn cursor(part: Part) -> CursorIcon {
    match part {
        Part::Point | Part::Move | Part::Body | Part::EdgeMove { .. } | Part::Vertex(_) | Part::Tangent { .. } | Part::Pull(_) | Part::Track(_) | Part::TrackOffset(_) | Part::Corner { .. } => CursorIcon::Move,
        Part::Marquee | Part::Pick => CursorIcon::Crosshair,
        Part::LightAim | Part::LightCone | Part::LightSoft => CursorIcon::Alias,
        Part::LightReach => CursorIcon::ResizeHorizontal,
        Part::SelBox(BoxHandle::Move) => CursorIcon::Move,
        Part::SelBox(BoxHandle::Rotate) => CursorIcon::Alias,
        Part::SelBox(BoxHandle::Scale { x, y }) => match (x, y) {
            (0, _) => CursorIcon::ResizeVertical,
            (_, 0) => CursorIcon::ResizeHorizontal,
            _ if x == y => CursorIcon::ResizeNwSe,
            _ => CursorIcon::ResizeNeSw,
        },
        Part::TrackBox { .. } => CursorIcon::ResizeNwSe,
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
    use arclight::image::IRect;

    const SIZE: Vec2 = Vec2::new(200.0, 100.0);
    /// 1:1 zoom with the image at the screen origin, so screen = image.
    const XF: Xf = Xf { img_rect: Rect { min: Pos2::ZERO, max: Pos2::new(200.0, 100.0) }, zoom: 1.0, warp: None };

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
        ov.release(d);
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
    fn an_emitter_has_handles_when_its_particles_2d_is_viewed() {
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| arclight::graph::NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None };
        let g = Graph {
            nodes: vec![
                node("n", "particle_emitter", &["", ""], json!({ "shape": "rectangle", "translate": [100, 50, 7], "dimensions": [40, 20, 3], "rotate": [0, 0, 0] })),
                node("flat", "particles_2d", &["n", ""], json!({})),
            ],
            ..Default::default()
        };
        let mut d = Doc::new(g, None);
        d.selected = Some("n".into());
        // Not viewed in 2D: no handles.
        assert!(gesture(&mut Overlays::default(), &mut d, Pos2::new(120.0, 60.0), Pos2::new(130.0, 70.0), 1, Mods::default()).is_none());
        d.view_node("flat");
        // A rectangle's box: sizing keeps Dimensions' z, moving Translate's,
        // turning writes Rotate's z.
        assert_eq!(drag(&mut d, Pos2::new(120.0, 60.0), Pos2::new(130.0, 70.0), 1), Part::Size { x: true, y: true });
        assert_eq!(d.param("n", "dimensions"), json!([60.0, 40.0, 3]));
        assert_eq!(drag(&mut d, Pos2::new(100.0, 50.0), Pos2::new(90.0, 45.0), 1), Part::Move);
        assert_eq!(d.param("n", "translate"), json!([90.0, 45.0, 7]));
        assert_eq!(drag(&mut d, Pos2::new(90.0, 45.0 - 20.0 - STEM), Pos2::new(140.0, 45.0), 1), Part::Rotate);
        let r = d.param("n", "rotate");
        assert!(r[0] == 0 && r[1] == 0 && (num(&r[2]) - 90.0).abs() < 1e-3, "{r}");
        // A point emitter moves by its point.
        d.set_param("n", "shape", json!("point"));
        assert_eq!(drag(&mut d, Pos2::new(90.0, 45.0), Pos2::new(95.0, 40.0), 1), Part::Point);
        assert_eq!(d.param("n", "translate"), json!([95.0, 40.0, 7]));
    }

    #[test]
    fn transform_gizmo_moves_rotates_and_scales() {
        let mut d = doc("transform", json!({}));
        let c = Pos2::new(100.0, 50.0);
        assert_eq!(drag(&mut d, c, c + Vec2::new(10.0, 5.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "translate")), Vec2::new(10.0, 5.0));

        let c = c + Vec2::new(10.0, 5.0);
        let ring = |deg: f32| c + rot(Vec2::new(RING, 0.0), deg);
        assert_eq!(drag(&mut d, ring(-45.0), ring(45.0), 1), Part::Rotate);
        assert!((num(&d.param("n", "rotate")) - 90.0).abs() < 1e-3);

        // At 90°, the x axis points down the screen.
        let x_handle = c + Vec2::new(0.0, AXIS);
        assert_eq!(drag(&mut d, x_handle, c + Vec2::new(0.0, AXIS * 2.0), 1), Part::ScaleX);
        let s = vec2(&d.param("n", "scale"));
        assert!((s.x - 2.0).abs() < 1e-3 && (s.y - 1.0).abs() < 1e-3, "{s:?}");
    }

    #[test]
    fn dragging_inside_the_layer_moves_it_and_its_frame_scales_it() {
        let mut d = doc("transform", json!({ "scale": [0.5, 0.5] }));
        // The layer covers x 50–150, y 25–75; grab it between the center and the ring.
        assert_eq!(drag(&mut d, Pos2::new(75.0, 50.0), Pos2::new(95.0, 60.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "translate")), Vec2::new(20.0, 10.0));
        // Outside the layer nothing is grabbed (the viewer pans).
        let missed = gesture(&mut Overlays::default(), &mut d, Pos2::new(10.0, 10.0), Pos2::new(20.0, 20.0), 1, Mods::default());
        assert_eq!(missed, None);

        // Corners scale uniformly about the pivot (now at 120, 60).
        let corner = Pos2::new(170.0, 85.0);
        assert_eq!(drag(&mut d, corner, Pos2::new(220.0, 110.0), 1), Part::Size { x: true, y: true });
        let s = vec2(&d.param("n", "scale"));
        assert!((s.x - 1.0).abs() < 1e-3 && (s.y - 1.0).abs() < 1e-3, "{s:?}");
        // An edge handle scales one axis.
        let mut d = doc("transform", json!({ "scale": [0.5, 0.5], "rotate": 90.0 }));
        // Rotated 90° clockwise, the layer's right edge faces down: its
        // midpoint is 25 px (half width × 0.5) below the center.
        let right = Pos2::new(100.0, 100.0);
        assert_eq!(drag(&mut d, right, Pos2::new(100.0, 150.0), 1), Part::Size { x: true, y: false });
        let s = vec2(&d.param("n", "scale"));
        assert!((s.x - 1.0).abs() < 1e-3 && (s.y - 0.5).abs() < 1e-3, "{s:?}");
    }

    #[test]
    fn tracker_boxes_move_tracks_and_size_the_boxes() {
        let mut d = doc("tracker", json!({ "track1": [50, 50], "track2_on": true, "track2": [150, 50], "pattern": 20, "search": 40 }));
        // Anywhere in the pattern box moves its track; the other stays.
        assert_eq!(drag(&mut d, Pos2::new(155.0, 45.0), Pos2::new(165.0, 55.0), 1), Part::Track(1));
        assert_eq!(vec2(&d.param("n", "track2")), Vec2::new(160.0, 60.0));
        assert_eq!(vec2(&d.param("n", "track1")), Vec2::new(50.0, 50.0));
        // A pattern corner sizes that track's pattern.
        assert_eq!(drag(&mut d, Pos2::new(60.0, 60.0), Pos2::new(65.0, 62.0), 1), Part::TrackBox { i: 0, search: false });
        assert_eq!(track::box_sizes(&d.node("n").unwrap().params)[..2], [[30.0, 40.0], [20.0, 40.0]]);
        // The pattern can't outgrow the search box.
        drag(&mut d, Pos2::new(65.0, 65.0), Pos2::new(90.0, 90.0), 1);
        assert_eq!(d.param("n", "track1_pattern"), json!(38));
        assert_eq!(drag(&mut d, Pos2::new(70.0, 30.0), Pos2::new(80.0, 20.0), 1), Part::TrackBox { i: 0, search: true });
        assert_eq!(d.param("n", "track1_search"), json!(60));
        // With Shift, a corner sizes every track and drops their own sizes.
        let shift = Mods { shift: true, ..Mods::default() };
        let part = gesture(&mut Overlays::default(), &mut d, Pos2::new(170.0, 70.0), Pos2::new(172.0, 72.0), 1, shift);
        assert_eq!(part, Some(Part::TrackBox { i: 1, search: false }));
        assert_eq!(track::box_sizes(&d.node("n").unwrap().params)[..2], [[24.0, 60.0], [24.0, 40.0]]);
        // Ctrl-dragging a box moves it away from its point: the offset. Its
        // corners and inside then follow the box.
        let ctrl = Mods { command: true, ..Mods::default() };
        let part = gesture(&mut Overlays::default(), &mut d, Pos2::new(50.0, 50.0), Pos2::new(60.0, 45.0), 1, ctrl);
        assert_eq!(part, Some(Part::TrackOffset(0)));
        assert_eq!((d.param("n", "track1_offset"), vec2(&d.param("n", "track1"))), (json!([10.0, -5.0]), Vec2::new(50.0, 50.0)));
        assert_eq!(drag(&mut d, Pos2::new(62.0, 47.0), Pos2::new(64.0, 47.0), 1), Part::Track(0));
        assert_eq!(vec2(&d.param("n", "track1")), Vec2::new(52.0, 50.0));
        // Tracks that are off have no handles.
        let mut d = doc("tracker", json!({ "track1_on": false, "track1": [50, 50] }));
        let missed = gesture(&mut Overlays::default(), &mut d, Pos2::new(50.0, 50.0), Pos2::new(60.0, 50.0), 1, Mods::default());
        assert_eq!(missed, None);
    }

    #[test]
    fn corner_pin_corners_drag_to_and_from() {
        let mut d = doc("corner_pin", json!({ "to3": [150, 80], "from3": [140, 80] }));
        // To corners win where both are in reach.
        assert_eq!(drag(&mut d, Pos2::new(148.0, 80.0), Pos2::new(160.0, 90.0), 1), Part::Corner { i: 2, from: false });
        assert_eq!(vec2(&d.param("n", "to3")), Vec2::new(162.0, 90.0));
        assert_eq!(drag(&mut d, Pos2::new(140.0, 80.0), Pos2::new(130.0, 70.0), 1), Part::Corner { i: 2, from: true });
        assert_eq!(vec2(&d.param("n", "from3")), Vec2::new(130.0, 70.0));
    }

    #[test]
    fn tracker_boxes_follow_its_shown_output() {
        // One track moved 20 px right since the reference frame, shown
        // through the tracker's match-move: the box shows 20 px further on.
        let mut d = doc("tracker", json!({
            "track1": { "keys": [[1, [50, 50]], [2, [70, 50]]], "interp": "linear" },
            "output": "match-move", "pattern": 20, "search": 40,
        }));
        d.graph.nodes.push(arclight::graph::NodeSpec { id: "v".into(), op: "viewer".into(), inputs: vec!["n".into()], params: json!({}), pos: None });
        let xf = XF.for_node(&d, "n", 2);
        assert_eq!(xf.screen(Vec2::new(70.0, 50.0)), Pos2::new(90.0, 50.0));
        // Dragging it there moves the track by the pointer's motion in input pixels.
        let mut ov = Overlays::default();
        ov.press(&mut d, 2, SIZE, &XF, Pos2::new(92.0, 52.0), Mods::default());
        assert_eq!(ov.drag.as_ref().map(|dr| dr.part), Some(Part::Track(0)));
        let frozen = ov.drag.as_ref().unwrap().xf;
        ov.drag_to(&mut d, 2, frozen.image(Pos2::new(97.0, 52.0)), Mods::default());
        assert_eq!(vec2(&d.param_at("n", "track1", 2)), Vec2::new(75.0, 50.0));
        // Viewing something else (or the plain input) leaves the boxes in place.
        d.graph.nodes[1].inputs = vec![String::new()];
        assert!(XF.for_node(&d, "n", 2).warp.is_none());
    }

    #[test]
    fn the_transform_frame_outlines_its_input_s_data() {
        // The frame is 200×100; the input's data only (50, 25)–(100, 50),
        // which the pivot follows to (75, 37.5). Its top-left corner is
        // then a scale handle about there…
        let mut d = doc("transform", json!({}));
        let mut ov = Overlays::default();
        ov.set_input(Some(("n".into(), layer(IRect::new(50, 25, 100, 50)))));
        let part = gesture(&mut ov, &mut d, Pos2::new(50.0, 25.0), Pos2::new(25.0, 12.5), 1, Mods::default());
        assert_eq!(part, Some(Part::Size { x: true, y: true }));
        assert_eq!(vec2(&d.param("n", "scale")), Vec2::new(2.0, 2.0));
        // …while without it the layer is the whole frame, so a spot like
        // (170, 85) (clear of the rotation ring) is inside it: a move.
        let mut d = doc("transform", json!({}));
        assert_eq!(drag(&mut d, Pos2::new(170.0, 85.0), Pos2::new(175.0, 85.0), 1), Part::Move);
        let mut ov = Overlays::default();
        ov.set_input(Some(("n".into(), layer(IRect::new(50, 25, 100, 50)))));
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(170.0, 85.0), Pos2::new(175.0, 85.0), 1, Mods::default()), None, "outside the small layer");
    }

    /// A 200×100 input whose data is only `window`, filled with mid grey.
    fn layer(window: IRect) -> Arc<Image> {
        let mut img = Image::blank(arclight::image::Size { width: 200, height: 100 }, window);
        img.pixels.iter_mut().for_each(|p| *p = [0.5, 0.5, 0.5, 1.0]);
        Arc::new(img)
    }

    #[test]
    fn ctrl_click_or_drag_picks_a_keyer_s_screen_color_from_its_input() {
        let mut d = doc("keyer", json!({}));
        let mut ov = Overlays::default();
        // Green on the left half, blue on the right, half-transparent blue
        // in the bottom-right corner (picked as straight color).
        let mut img = Image::filled(200, 100, [0.1, 0.6, 0.1, 1.0]);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            if i % 200 >= 100 {
                *p = if i / 200 >= 90 { [0.05, 0.1, 0.4, 0.5] } else { [0.1, 0.2, 0.8, 1.0] };
            }
        }
        ov.set_input(Some(("n".into(), Arc::new(img))));
        let ctrl = Mods { command: true, ..Mods::default() };
        let screen = |d: &Doc| d.param("n", "screen");
        // Without Ctrl nothing is grabbed.
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(20.5, 20.5), Pos2::new(20.5, 20.5), 1, Mods::default()), None);
        assert!(screen(&d).is_null());
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(20.5, 20.5), Pos2::new(20.5, 20.5), 1, ctrl), Some(Part::Pick));
        assert_eq!(screen(&d), json!([0.1, 0.6, 0.1, 1.0]));
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(150.5, 95.5), Pos2::new(150.5, 95.5), 1, ctrl), Some(Part::Pick));
        assert_eq!(screen(&d), json!([0.1, 0.2, 0.8, 1.0]));
        // A box over the seam averages both sides.
        gesture(&mut ov, &mut d, Pos2::new(98.5, 10.5), Pos2::new(101.5, 11.5), 1, ctrl);
        let c: Vec<f64> = screen(&d).as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert!((c[1] - 0.4).abs() < 1e-3 && (c[2] - 0.45).abs() < 1e-3, "{c:?}");
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
    fn light_2d_handles_aim_open_soften_and_reach() {
        // A spot at (100, 50) aiming right, 60° across, 10° soft.
        let mut d = doc("light_2d", json!({ "type": "spot", "position": [100, 50], "direction": 0, "cone": 60, "softness": 10 }));
        let c = Pos2::new(100.0, 50.0);
        let at = |deg: f32, len: f32| c + rot(Vec2::X, deg) * len;
        // A cone edge to 45° off the aim: 90° across.
        assert_eq!(drag(&mut d, at(-30.0, BEAM), at(45.0, 50.0), 1), Part::LightCone);
        assert!((num(&d.param("n", "cone")) - 90.0).abs() < 1e-3, "{}", d.param("n", "cone"));
        // The softness's inner edge (45 − 5 = 40° out) to 30°: 30° soft.
        assert_eq!(drag(&mut d, at(40.0, BEAM * 0.7), at(30.0, 60.0), 1), Part::LightSoft);
        assert!((num(&d.param("n", "softness")) - 30.0).abs() < 1e-3);
        // The aim, turned to point down (90°).
        assert_eq!(drag(&mut d, at(0.0, BEAM * 1.35), at(90.0, 80.0), 1), Part::LightAim);
        assert!((num(&d.param("n", "direction")) - 90.0).abs() < 1e-3);
        // The middle moves it.
        assert_eq!(drag(&mut d, c, Pos2::new(80.0, 40.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "position")), Vec2::new(80.0, 40.0));
        // A point light's radius, and a directional light's gradient
        // (measured along its direction).
        let mut d = doc("light_2d", json!({ "type": "point", "position": [100, 50], "radius": 40 }));
        assert_eq!(drag(&mut d, Pos2::new(140.0, 50.0), Pos2::new(100.0, 120.0), 1), Part::LightReach);
        assert!((num(&d.param("n", "radius")) - 70.0).abs() < 1e-3);
        let mut d = doc("light_2d", json!({ "type": "directional", "position": [100, 50], "direction": 0, "gradient": 30 }));
        assert_eq!(drag(&mut d, Pos2::new(130.0, 50.0), Pos2::new(160.0, 80.0), 1), Part::LightReach);
        assert!((num(&d.param("n", "gradient")) - 60.0).abs() < 1e-3);
        // An area light: a box's handles (a corner sizes it about its middle).
        let mut d = doc("light_2d", json!({ "type": "area", "position": [100, 50], "size": [40, 20] }));
        assert_eq!(drag(&mut d, Pos2::new(120.0, 60.0), Pos2::new(130.0, 70.0), 1), Part::Size { x: true, y: true });
        assert_eq!(vec2(&d.param("n", "size")), Vec2::new(60.0, 40.0));
        // Animated: a key at the frame.
        let mut d = doc("light_2d", json!({ "type": "point", "position": { "keys": [[1, [10, 10]], [10, [100, 50]]], "interp": "linear" }, "radius": 40 }));
        drag(&mut d, Pos2::new(100.0, 50.0), Pos2::new(90.0, 50.0), 10);
        assert!(anim::has_key_at(&d.param("n", "position"), 10) && vec2(&anim::evaluate(&d.param("n", "position"), 10.0)) == Vec2::new(90.0, 50.0));
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
        // Click the top edge's midpoint (no drag).
        let at = Pos2::new(100.0, 25.0);
        let part = gesture(&mut ov, &mut d, at, at, 1, Mods::default());
        assert_eq!(part, Some(Part::EdgeMove { i: 0, t: 0.5 }));
        assert_eq!(points(&d, 1), [50.0, 25.0, 100.0, 25.0, 150.0, 25.0, 150.0, 75.0, 50.0, 75.0]);
        // The other key gets the vertex on its own edge, at the same fraction.
        assert_eq!(points(&d, 10)[2..4], [110.0, 35.0]);
        assert_eq!(ov.points, vec![1]);
    }

    #[test]
    fn dragging_an_edge_moves_its_two_vertices() {
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let mut ov = Overlays::default();
        // The right edge, pulled 20 px right: both its ends follow, nothing
        // is inserted.
        let part = gesture(&mut ov, &mut d, Pos2::new(150.0, 50.0), Pos2::new(170.0, 50.0), 1, Mods::default());
        assert_eq!(part, Some(Part::EdgeMove { i: 1, t: 0.5 }));
        assert_eq!(points(&d, 1), [50.0, 25.0, 170.0, 25.0, 170.0, 75.0, 50.0, 75.0]);
        assert_eq!(ov.points, vec![1, 2]);
        // A shaky click (a pixel or two) still inserts rather than moving.
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        gesture(&mut ov, &mut d, Pos2::new(100.0, 75.0), Pos2::new(101.0, 76.0), 1, Mods::default());
        assert_eq!(points(&d, 1).len(), 10);
    }

    #[test]
    fn dragging_inside_a_shape_moves_all_of_it() {
        for op in ["polygon", "roto"] {
            let pts = if op == "roto" { spline_square() } else { SQUARE.to_vec() };
            let mut d = doc(op, json!({ "points": pts }));
            let mut ov = Overlays::default();
            let part = gesture(&mut ov, &mut d, Pos2::new(80.0, 60.0), Pos2::new(90.0, 40.0), 1, Mods::default());
            assert_eq!(part, Some(Part::Body), "{op}");
            let v = points(&d, 1);
            let stride = if op == "roto" { SPLINE } else { 2 };
            let corners: Vec<f32> = v.chunks(stride).flat_map(|c| [c[0], c[1]]).collect();
            assert_eq!(corners, [60.0, 5.0, 160.0, 5.0, 160.0, 55.0, 60.0, 55.0], "{op}");
            assert_eq!(ov.points, vec![0, 1, 2, 3], "{op}: all selected, for the transform box");
        }
        // Shift-drag inside still box-selects instead.
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let mut ov = Overlays::default();
        let shift = Mods { shift: true, ..Default::default() };
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(40.0, 15.0), Pos2::new(110.0, 85.0), 1, Mods::default()), Some(Part::Marquee));
        assert_eq!(ov.points, vec![0, 3]);
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(140.0, 60.0), Pos2::new(160.0, 80.0), 1, shift), Some(Part::Marquee));
        assert_eq!(ov.points, vec![0, 3, 2]);
        assert_eq!(points(&d, 1), SQUARE, "nothing moved");
        // Rectangles move from anywhere inside, not just the center.
        let mut d = doc("rectangle", json!({ "center": [100, 50], "size": [100, 50] }));
        assert_eq!(drag(&mut d, Pos2::new(70.0, 35.0), Pos2::new(80.0, 45.0), 1), Part::Move);
        assert_eq!(vec2(&d.param("n", "center")), Vec2::new(110.0, 60.0));
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
        assert_eq!(gesture(&mut ov, &mut d, Pos2::new(5.0, 95.0), Pos2::new(5.0, 95.0), 1, Mods::default()), Some(Part::Marquee));
        assert!(ov.points.is_empty());
    }

    #[test]
    fn dragging_on_empty_space_box_selects_vertices() {
        // The square's vertices: (50, 25), (150, 25), (150, 75), (50, 75).
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let mut ov = Overlays::default();
        let part = gesture(&mut ov, &mut d, Pos2::new(30.0, 10.0), Pos2::new(170.0, 40.0), 1, Mods::default());
        assert_eq!((part, ov.points.clone()), (Some(Part::Marquee), vec![0, 1]));
        // Nothing moved.
        assert_eq!(points(&d, 1), [50.0, 25.0, 150.0, 25.0, 150.0, 75.0, 50.0, 75.0]);
        // Shift adds; a plain box replaces.
        let shift = Mods { shift: true, ..Default::default() };
        gesture(&mut ov, &mut d, Pos2::new(40.0, 90.0), Pos2::new(60.0, 60.0), 1, shift);
        assert_eq!(ov.points, vec![0, 1, 3]);
        gesture(&mut ov, &mut d, Pos2::new(140.0, 90.0), Pos2::new(160.0, 60.0), 1, Mods::default());
        assert_eq!(ov.points, vec![2]);
        // Select all.
        assert!(ov.select_all_points(&d));
        assert_eq!(ov.points, vec![0, 1, 2, 3]);
        assert!(!Overlays::default().select_all_points(&doc("transform", json!({}))));
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
    #[test]
    fn the_transform_box_moves_scales_and_turns_the_selection() {
        // All four selected: the box is the square (50, 25)–(150, 75)
        // padded by BOX_PAD, around the center (100, 50).
        let all = |d: &Doc| {
            let mut ov = Overlays::default();
            assert!(ov.select_all_points(d));
            ov
        };
        let close = |a: Vec<f32>, b: [f32; 8]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-3);
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let part = gesture(&mut all(&d), &mut d, Pos2::new(100.0, 50.0), Pos2::new(110.0, 55.0), 1, Mods::default());
        assert_eq!(part, Some(Part::SelBox(BoxHandle::Move)));
        assert_eq!(points(&d, 1), [60.0, 30.0, 160.0, 30.0, 160.0, 80.0, 60.0, 80.0]);
        // A corner doubles it about the center; an edge stretches one axis.
        let corner = Pos2::new(150.0 + BOX_PAD, 75.0 + BOX_PAD);
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let part = gesture(&mut all(&d), &mut d, corner, Pos2::new(100.0, 50.0) + (corner - Pos2::new(100.0, 50.0)) * 2.0, 1, Mods::default());
        assert_eq!(part, Some(Part::SelBox(BoxHandle::Scale { x: 1, y: 1 })));
        assert!(close(points(&d, 1), [0.0, 0.0, 200.0, 0.0, 200.0, 100.0, 0.0, 100.0]), "{:?}", points(&d, 1));
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let edge = Pos2::new(150.0 + BOX_PAD, 50.0);
        gesture(&mut all(&d), &mut d, edge, Pos2::new(100.0 + 2.0 * (50.0 + BOX_PAD), 70.0), 1, Mods::default());
        assert!(close(points(&d, 1), [0.0, 25.0, 200.0, 25.0, 200.0, 75.0, 0.0, 75.0]), "{:?}", points(&d, 1));
        // The stem above the box turns it 90° clockwise about the center.
        let mut d = doc("polygon", json!({ "points": SQUARE }));
        let stem = Pos2::new(100.0, 25.0 - BOX_PAD - STEM);
        let part = gesture(&mut all(&d), &mut d, stem, Pos2::new(200.0, 50.0), 1, Mods::default());
        assert_eq!(part, Some(Part::SelBox(BoxHandle::Rotate)));
        assert!(close(points(&d, 1), [125.0, 0.0, 125.0, 100.0, 75.0, 100.0, 75.0, 0.0]), "{:?}", points(&d, 1));
        // On a spline the tangents turn too, without moving.
        let mut d = doc("roto", json!({ "points": spline_square() }));
        gesture(&mut all(&d), &mut d, stem, Pos2::new(200.0, 50.0), 1, Mods::default());
        let v = points(&d, 1);
        assert!(v[2..6].iter().zip([0.0, -20.0, 0.0, 20.0]).all(|(a, b)| (a - b).abs() < 1e-3), "{:?}", &v[..6]);
        // One vertex selected: no box.
        let mut ov = Overlays::default();
        select(&mut ov, &mut d, Pos2::new(125.0, 0.0));
        assert!(!matches!(hit(&geo(&d, "n", 1, SIZE).unwrap(), &XF, Pos2::new(100.0, 50.0), &ov.points), Some(Part::SelBox(_))));
    }

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
        assert_eq!(gesture(&mut ov, &mut d, mid, mid, 1, Mods::default()), Some(Part::EdgeMove { i: 1, t: 0.5 }));
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
