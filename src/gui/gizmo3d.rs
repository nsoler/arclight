//! Moving, rotating and scaling 3D objects in the viewport, Blender-style:
//! the maths behind G / R / S and the move / rotate / scale gizmos.
//!
//! A [`Gesture`] starts from an object's translate / rotate / scale params
//! and its place in the scene, and works out new params from the pointer:
//! - Move: in the view plane at the object's depth (free), along an axis
//!   (the point of the axis line nearest the pointer's ray), or in a plane
//!   (the ray's hit on it).
//! - Rotate: by the angle the pointer turns about the object's center on
//!   screen, about the view axis (free) or an axis; or as a trackball.
//! - Scale: by how much farther from the center on screen the pointer is
//!   than where it started; all axes, one, or two (an axis excluded).
//!
//! X / Y / Z constrain to that axis (again: the object's own axis, a third
//! time: free), Shift + X / Y / Z exclude it; typed numbers set the amount
//! exactly (`-` negates); snapping rounds to 1 unit, 15° or 0.1. Axes are
//! the world's (global) or the object's (local). Pure maths, so it's
//! tested without a window; the viewport supplies a [`View`].
//! [`Transforming`] runs one on a node through `Doc`, and puts the param
//! back exactly as it was on cancel. With several objects selected it runs
//! one gesture about their median point (the mean of their origins, the
//! pivot): moves add the same offset to each, rotate and scale turn and
//! scale each about its own origin and carry its position round the pivot.

use crate::doc::Doc;
use arclight::geom::{self, Mat4, Vec3};
use eframe::egui::{self, Color32, Key, Pos2, Stroke, Vec2};
use nalgebra::{Rotation3, Unit};
use serde_json::{json, Value};

/// How the viewport maps the scene to the screen and back.
pub trait View {
    /// Where scene point `p` is on screen, and its depth; None behind the
    /// camera.
    fn project(&self, p: Vec3) -> Option<(Pos2, f64)>;
    /// The ray (origin, unit direction) through screen point `s`.
    fn ray(&self, s: Pos2) -> (Vec3, Vec3);
    /// Screen points per scene unit at `depth`.
    fn pixels_per_unit(&self, depth: f64) -> f64;
    /// Which way the view looks (unit, scene space).
    fn view_dir(&self) -> Vec3;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Move,
    Rotate,
    Scale,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Move => "Move",
            Mode::Rotate => "Rotate",
            Mode::Scale => "Scale",
        }
    }
}

/// World axes, or the object's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Space {
    #[default]
    Global,
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    Free,
    /// Along (move, scale) or about (rotate) axis 0, 1 or 2.
    Axis(usize, Space),
    /// Every axis but this one (Shift + X / Y / Z, a gizmo's plane square).
    Plane(usize, Space),
    /// Rotate freely, following the pointer like a ball (the rotate gizmo's
    /// inside).
    Trackball,
}

/// An object's transform params.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub translate: [f64; 3],
    /// Degrees, about X, then Y, then Z.
    pub rotate: [f64; 3],
    pub scale: [f64; 3],
}

impl Default for Trs {
    fn default() -> Self {
        Trs { translate: [0.0; 3], rotate: [0.0; 3], scale: [1.0; 3] }
    }
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        geom::trs(self.translate, self.rotate, self.scale)
    }

    /// Alt + G / R / S: that part back to its default.
    pub fn reset(mut self, mode: Mode) -> Trs {
        let d = Trs::default();
        match mode {
            Mode::Move => self.translate = d.translate,
            Mode::Rotate => self.rotate = d.rotate,
            Mode::Scale => self.scale = d.scale,
        }
        self
    }
}

/// A key pressed during a gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKey {
    /// X / Y / Z (0, 1, 2); with Shift, excluding it.
    Axis(usize, bool),
    /// 0–9 or '.'.
    Digit(char),
    Minus,
    Backspace,
}

/// The viewport's projection (the free view, orthographic, or through a
/// scene camera).
impl View for crate::viewport3d::Projector {
    fn project(&self, p: Vec3) -> Option<(Pos2, f64)> {
        crate::viewport3d::Projector::project(self, p)
    }
    fn ray(&self, s: Pos2) -> (Vec3, Vec3) {
        crate::viewport3d::Projector::ray(self, s)
    }
    fn pixels_per_unit(&self, depth: f64) -> f64 {
        crate::viewport3d::Projector::pixels_per_unit(self, depth)
    }
    fn view_dir(&self) -> Vec3 {
        crate::viewport3d::Projector::view_dir(self)
    }
}

/// Snapping steps: 1 unit, 15°, 0.1 scale.
pub const SNAP_MOVE: f64 = 1.0;
pub const SNAP_ANGLE: f64 = 15.0;
pub const SNAP_SCALE: f64 = 0.1;
/// Trackball degrees per screen point.
const TRACKBALL_RATE: f64 = 0.5;
/// Turning by the pointer's angle about the center: closer in than this
/// (points), the turn is damped as though the pointer were this far out,
/// so a pointer passing near the center doesn't swing the object round.
const TURN_RADIUS: f64 = 60.0;

/// A move, rotate or scale in progress.
#[derive(Clone, Debug)]
pub struct Gesture {
    pub mode: Mode,
    /// The params when it started (Esc / right-click puts them back).
    pub start: Trs,
    /// The object's parent: scene from the space its params are in.
    parent: Mat4,
    /// Where the pointer was when it started.
    from: Pos2,
    pub constraint: Constraint,
    typed: String,
    negate: bool,
    /// Rotate: the angle turned so far (degrees, counter-clockwise on
    /// screen) and the pointer's last angle about the center.
    turned: f64,
    last_angle: Option<f64>,
    /// A ring drag: the ring's direction on screen where it was grabbed
    /// (unit, the way a positive turn moves it) and its radius in points.
    /// The turn goes by how far the pointer moves along it, so a ring seen
    /// edge-on turns smoothly too.
    along: Option<(Vec2, f32)>,
    /// The pivot, when it isn't the object's origin (several objects).
    center: Option<Vec3>,
}

/// What the pointer asks for, in the scene: the same for every object of a
/// multi-object gesture.
#[derive(Clone, Copy, Debug)]
pub enum Change {
    Move(Vec3),
    /// A turn; `snap`: round the resulting angles (the trackball's Ctrl).
    Turn(Rotation3<f64>, bool),
    /// A factor along the axes `on` of `axes` (scene directions).
    Scale { factor: f64, on: [bool; 3], axes: [Vec3; 3] },
}

impl Gesture {
    /// Start `mode` on an object whose params are `start` and whose place
    /// in the scene is `world`, with the pointer at `from`.
    pub fn new(mode: Mode, start: Trs, world: &Mat4, from: Pos2) -> Gesture {
        let own = start.matrix();
        let parent = own.try_inverse().map_or_else(Mat4::identity, |inv| world * inv);
        Gesture { mode, start, parent, from, constraint: Constraint::Free, typed: String::new(), negate: false, turned: 0.0, last_angle: None, along: None, center: None }
    }

    /// The same about pivot `c` (scene) instead of the object's origin: where
    /// the move's axis and plane go through, rotate and scale center.
    pub fn about(mut self, c: Vec3) -> Gesture {
        self.center = Some(c);
        self
    }

    /// A ring drag: turn by the pointer's travel along the ring's screen
    /// direction `along` (see [`Layout::ring_tangent`]).
    pub fn along(mut self, along: Option<(Vec2, f32)>) -> Gesture {
        self.along = along;
        self
    }

    /// A gizmo drag: like [`Gesture::new`], constrained from the start.
    pub fn with_constraint(mut self, c: Constraint) -> Gesture {
        self.constraint = c;
        self
    }

    /// The pivot in the scene: the object's origin, or the one given.
    pub fn origin(&self) -> Vec3 {
        self.center.unwrap_or_else(|| geom::transform_point(&self.parent, &Vec3::from(self.start.translate)))
    }

    fn parent_rotation(&self) -> Rotation3<f64> {
        rotation_of(&self.parent)
    }

    /// Axis `k` in the scene: the world's, or the object's own (as it was
    /// at the start).
    pub fn axis(&self, k: usize, space: Space) -> Vec3 {
        match space {
            Space::Global => Vec3::ith(k, 1.0),
            Space::Local => self.parent_rotation() * geom::rotation(self.start.rotate) * Vec3::ith(k, 1.0),
        }
    }

    /// Handle a key: X / Y / Z cycle global → local → free, Shift + X / Y /
    /// Z the same for excluding the axis (rotate has nothing to exclude, so
    /// it constrains to the axis); digits, '.', '-', Backspace type an
    /// exact amount.
    pub fn key(&mut self, k: GestureKey) {
        match k {
            GestureKey::Axis(a, shift) => {
                let plane = shift && self.mode != Mode::Rotate;
                let make = |s| if plane { Constraint::Plane(a, s) } else { Constraint::Axis(a, s) };
                self.constraint = match self.constraint {
                    c if c == make(Space::Global) => make(Space::Local),
                    c if c == make(Space::Local) => Constraint::Free,
                    _ => make(Space::Global),
                };
            }
            GestureKey::Digit(c) => {
                if c.is_ascii_digit() || (c == '.' && !self.typed.contains('.')) {
                    self.typed.push(c);
                }
            }
            GestureKey::Minus => self.negate = !self.negate,
            GestureKey::Backspace => {
                if self.typed.pop().is_none() {
                    self.negate = false;
                }
            }
        }
    }

    /// The typed amount, if any (negated with '-').
    pub fn typed(&self) -> Option<f64> {
        let v: f64 = self.typed.parse().ok()?;
        Some(if self.negate { -v } else { v })
    }

    /// The params for the pointer at `mouse` (`snap`: Ctrl held).
    pub fn apply(&mut self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        match self.mode {
            Mode::Move => self.moved(view, mouse, snap),
            Mode::Rotate => self.rotated(view, mouse, snap),
            Mode::Scale => self.scaled(view, mouse, snap),
        }
    }

    /// What the gesture does, for the viewport's header: "Move X (local)
    /// 2.000", "Rotate Z 90°", "Scale 0.500".
    pub fn describe(&self, now: &Trs) -> String {
        const AXES: [&str; 3] = ["X", "Y", "Z"];
        let space = |s: Space| if s == Space::Local { " (local)" } else { "" };
        let what = match self.constraint {
            Constraint::Free => String::new(),
            Constraint::Axis(k, s) => format!(" {}{}", AXES[k], space(s)),
            Constraint::Plane(k, s) => format!(" not {}{}", AXES[k], space(s)),
            Constraint::Trackball => " trackball".into(),
        };
        let amount = match (self.typed(), self.mode) {
            (Some(v), Mode::Rotate) => format!("{v}°  (typed)"),
            (Some(v), _) => format!("{v}  (typed)"),
            (None, Mode::Move) => {
                let d: Vec<String> = (0..3).map(|i| format!("{:.3}", now.translate[i] - self.start.translate[i])).collect();
                format!("D: {}", d.join("  "))
            }
            (None, Mode::Rotate) => {
                let d: Vec<String> = (0..3).map(|i| format!("{:.1}°", now.rotate[i] - self.start.rotate[i])).collect();
                d.join("  ")
            }
            (None, Mode::Scale) => {
                let d: Vec<String> = (0..3).map(|i| format!("{:.3}", now.scale[i] / nonzero(self.start.scale[i]))).collect();
                d.join("  ")
            }
        };
        format!("{}{what}  {amount}", self.mode.name())
    }

    fn moved(&self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        moved_by(&self.start, &self.parent, self.move_delta(view, mouse, snap))
    }

    /// The move in the scene.
    fn move_delta(&self, view: &dyn View, mouse: Pos2, snap: bool) -> Vec3 {
        let o = self.origin();
        match (self.typed(), self.constraint) {
            (Some(v), Constraint::Axis(k, s)) => self.axis(k, s) * v,
            // Excluding an axis: the first one left.
            (Some(v), Constraint::Plane(k, s)) => self.axis((k + 1) % 3, s) * v,
            (Some(v), _) => Vec3::x() * v,
            (None, Constraint::Axis(k, s)) => {
                let a = self.axis(k, s);
                let along = |p: Pos2| {
                    let (r0, d) = view.ray(p);
                    line_ray(&o, &a, &r0, &d)
                };
                match (along(self.from), along(mouse)) {
                    (Some(t0), Some(t1)) => a * snapped(t1 - t0, SNAP_MOVE, snap),
                    _ => Vec3::zeros(),
                }
            }
            (None, c) => {
                let n = match c {
                    Constraint::Plane(k, s) => self.axis(k, s),
                    _ => view.view_dir(),
                };
                let hit = |p: Pos2| {
                    let (r0, d) = view.ray(p);
                    ray_plane(&r0, &d, &o, &n)
                };
                match (hit(self.from), hit(mouse)) {
                    (Some(a), Some(b)) => (b - a).map(|x| snapped(x, SNAP_MOVE, snap)),
                    _ => Vec3::zeros(),
                }
            }
        }
    }

    fn rotated(&mut self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        match self.turn(view, mouse, snap) {
            Turn::Exact(k, angle) => {
                let mut t = self.start;
                t.rotate[k] += angle;
                t
            }
            Turn::By(q, snap) => self.rotated_by(q, snap),
        }
    }

    /// The turn the pointer asks for.
    fn turn(&mut self, view: &dyn View, mouse: Pos2, snap: bool) -> Turn {
        let toward_viewer = -view.view_dir();
        if self.constraint == Constraint::Trackball && self.typed().is_none() {
            // Left / right turns about the view's up, up / down about its right.
            let d = mouse - self.from;
            let right = view.view_dir().cross(&Vec3::y()).try_normalize(1e-9).unwrap_or_else(Vec3::x);
            let up = right.cross(&view.view_dir());
            let q = Rotation3::from_axis_angle(&Unit::new_normalize(up), (d.x as f64 * TRACKBALL_RATE).to_radians())
                * Rotation3::from_axis_angle(&Unit::new_normalize(right), (d.y as f64 * TRACKBALL_RATE).to_radians());
            return Turn::By(q, snap);
        }
        let (axis, space) = match self.constraint {
            Constraint::Axis(k, s) | Constraint::Plane(k, s) => (self.axis(k, s), Some((k, s))),
            _ => (toward_viewer, None),
        };
        let angle = match (self.typed(), self.along) {
            (Some(v), _) => v,
            (None, Some((dir, r))) if space.is_some() => {
                let travel = (mouse - self.from).dot(dir) as f64;
                snapped((travel / r.max(1.0) as f64).to_degrees(), SNAP_ANGLE, snap)
            }
            (None, _) => {
                // The pointer's turn about the center, counter-clockwise on
                // screen; about an axis pointing away from the viewer that's
                // a clockwise turn.
                let c = view.project(self.origin()).map_or(self.from, |(c, _)| c);
                if let Some(a) = screen_angle(c, mouse) {
                    let last = *self.last_angle.get_or_insert(screen_angle(c, self.from).unwrap_or(a));
                    let damp = ((mouse - c).length() as f64 / TURN_RADIUS).min(1.0);
                    self.turned += wrap_degrees(a - last) * damp;
                    self.last_angle = Some(a);
                }
                let sign = if axis.dot(&toward_viewer) < 0.0 { -1.0 } else { 1.0 };
                snapped(self.turned * sign, SNAP_ANGLE, snap)
            }
        };
        // Exact, and continuous past ±180°: turning about the object's own X
        // (the first rotation applied), or the world's Z under a parent that
        // doesn't rotate (the last), adds to that angle alone.
        let exact = match space {
            Some((0, Space::Local)) => Some(0),
            Some((2, Space::Global)) if self.parent_rotation().angle() < 1e-9 => Some(2),
            _ => None,
        };
        match exact {
            Some(k) if self.center.is_none() => Turn::Exact(k, angle),
            _ => Turn::By(Rotation3::from_axis_angle(&Unit::new_normalize(axis), angle.to_radians()), false),
        }
    }

    /// The start rotation turned by `q` (scene space), as params nearest
    /// the start's.
    fn rotated_by(&self, q: Rotation3<f64>, snap: bool) -> Trs {
        turned_by(&self.start, &self.parent, q, snap)
    }

    fn scaled(&self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        scaled_by(&self.start, self.factor(view, mouse, snap), self.scale_axes())
    }

    /// How much the pointer scales by.
    fn factor(&self, view: &dyn View, mouse: Pos2, snap: bool) -> f64 {
        match self.typed() {
            Some(v) => v,
            None => {
                let c = view.project(self.origin()).map_or(self.from, |(c, _)| c);
                let r0 = (self.from - c).length() as f64;
                if r0 < 1.0 { 1.0 } else { snapped((mouse - c).length() as f64 / r0, SNAP_SCALE, snap) }
            }
        }
    }

    /// Which axes a scale acts on.
    fn scale_axes(&self) -> [bool; 3] {
        std::array::from_fn(|i| match self.constraint {
            Constraint::Axis(k, _) => i == k,
            Constraint::Plane(k, _) => i != k,
            _ => true,
        })
    }

    /// What the pointer at `mouse` asks for, in the scene (for several
    /// objects at once).
    pub fn change(&mut self, view: &dyn View, mouse: Pos2, snap: bool) -> Change {
        match self.mode {
            Mode::Move => Change::Move(self.move_delta(view, mouse, snap)),
            Mode::Rotate => match self.turn(view, mouse, snap) {
                Turn::By(q, s) => Change::Turn(q, s),
                Turn::Exact(..) => unreachable!("exact turns are single-object"),
            },
            Mode::Scale => {
                let space = match self.constraint {
                    Constraint::Axis(_, s) | Constraint::Plane(_, s) => s,
                    _ => Space::Global,
                };
                Change::Scale { factor: self.factor(view, mouse, snap), on: self.scale_axes(), axes: std::array::from_fn(|k| self.axis(k, space)) }
            }
        }
    }
}

/// A turn: by a scene rotation (`.1`: round the angles), or exactly by an
/// angle on one param (a single object's own X, or world Z).
enum Turn {
    By(Rotation3<f64>, bool),
    Exact(usize, f64),
}

/// The rotation in `m` (its scale taken out).
fn rotation_of(m: &Mat4) -> Rotation3<f64> {
    let m = m.fixed_view::<3, 3>(0, 0).into_owned();
    let cols: Vec<Vec3> = (0..3).map(|i| m.column(i).into_owned().try_normalize(1e-12).unwrap_or_else(|| Vec3::ith(i, 1.0))).collect();
    Rotation3::from_matrix(&nalgebra::Matrix3::from_columns(&cols))
}

/// `start` (params under `parent`) moved by `delta` in the scene.
fn moved_by(start: &Trs, parent: &Mat4, delta: Vec3) -> Trs {
    let local = parent.fixed_view::<3, 3>(0, 0).try_inverse().map_or(delta, |inv| inv * delta);
    let mut t = *start;
    for i in 0..3 {
        t.translate[i] += local[i];
    }
    t
}

/// `start`'s rotation (params under `parent`) turned by scene rotation `q`,
/// as angles nearest the start's (`snap`: rounded to 15°).
fn turned_by(start: &Trs, parent: &Mat4, q: Rotation3<f64>, snap: bool) -> Trs {
    let p = rotation_of(parent);
    let r = p.inverse() * q * p * geom::rotation(start.rotate);
    let mut t = *start;
    t.rotate = nearest_angles(geom::angles(&r), start.rotate);
    if snap {
        t.rotate = t.rotate.map(|a| snapped(a, SNAP_ANGLE, true));
    }
    t
}

/// `start` scaled by `factor` along its own axes `on`.
fn scaled_by(start: &Trs, factor: f64, on: [bool; 3]) -> Trs {
    let mut t = *start;
    for i in (0..3).filter(|&i| on[i]) {
        t.scale[i] *= factor;
    }
    t
}

/// The param a mode edits.
pub fn param_of(mode: Mode) -> &'static str {
    match mode {
        Mode::Move => "translate",
        Mode::Rotate => "rotate",
        Mode::Scale => "scale",
    }
}

/// The param `mode` changes on a node of `op`: its translate, rotate or
/// scale, a particle force's point (an Attractor's position, a Vortex's
/// center, a Bounce plane's point), which only moves, or a Splat Crop's
/// center and size.
pub fn param_for(op: &str, mode: Mode) -> &'static str {
    match (op, mode) {
        ("particle_attractor", Mode::Move) => "position",
        ("particle_vortex", Mode::Move) => "center",
        ("particle_bounce", Mode::Move) => "point",
        ("splat_crop", Mode::Move) => "center",
        ("splat_crop", Mode::Scale) => "size",
        _ => param_of(mode),
    }
}

/// The param `mode` changes on `node` (see [`param_for`]).
pub fn param(doc: &Doc, node: &str, mode: Mode) -> &'static str {
    param_for(doc.node(node).map_or("", |n| n.op.as_str()), mode)
}

/// A node's transform at `frame` (defaults for what it lacks).
pub fn trs_at(doc: &Doc, node: &str, frame: i64) -> Trs {
    let get = |mode: Mode, default: [f64; 3]| -> [f64; 3] {
        let v = doc.param_at(node, param(doc, node, mode), frame);
        std::array::from_fn(|i| v.get(i).and_then(Value::as_f64).unwrap_or(default[i]))
    };
    let d = Trs::default();
    Trs { translate: get(Mode::Move, d.translate), rotate: get(Mode::Rotate, d.rotate), scale: get(Mode::Scale, d.scale) }
}

/// Why `mode` can't change `node`'s transform, if it can't: it has no such
/// param (a Locator only moves), or the param is linked.
pub fn refusal(doc: &Doc, node: &str, mode: Mode) -> Option<String> {
    let name = param(doc, node, mode);
    let n = doc.node(node)?;
    let has = arclight::ops::info(&n.op).is_some_and(|i| i.params.iter().any(|p| p.name == name && matches!(p.kind, arclight::ops::ParamKind::Vec3)));
    if !has {
        return Some(format!("{node} has no {name} to {}", mode.name().to_lowercase()));
    }
    let raw = doc.param(node, name);
    arclight::link::is_link(&raw).then(|| format!("{node}'s {name} is linked ({}): Unlink it in Properties to change it here", arclight::link::describe(&raw)))
}

/// Alt + G / R / S: put `node`'s translate, rotate or scale back to its
/// default (a key at `frame` when animated). Err: why not.
pub fn reset(doc: &mut Doc, node: &str, mode: Mode, frame: i64) -> Result<(), String> {
    if let Some(why) = refusal(doc, node, mode) {
        return Err(why);
    }
    let d = Trs::default().reset(mode);
    let v = match mode {
        Mode::Move => d.translate,
        Mode::Rotate => d.rotate,
        Mode::Scale => d.scale,
    };
    doc.edit_param(node, param(doc, node, mode), frame, json!(v));
    Ok(())
}

/// This frame's input to a G / R / S in progress.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub mouse: Option<Pos2>,
    /// Gesture keys pressed this frame, in order.
    pub keys: Vec<GestureKey>,
    /// Enter or a left click.
    pub confirm: bool,
    /// Esc or a right click.
    pub cancel: bool,
    /// Ctrl held.
    pub snap: bool,
}

impl Input {
    /// Read (and take, so nothing else acts on them) the keys a gesture
    /// uses from egui's input.
    /// `by_drag`: a gizmo drag, which releasing the button confirms (else a
    /// click does).
    pub fn take(i: &mut eframe::egui::InputState, by_drag: bool) -> Input {
        use eframe::egui::Modifiers;
        let mut keys = Vec::new();
        for (k, axis) in [(Key::X, 0), (Key::Y, 1), (Key::Z, 2)] {
            if i.consume_key(Modifiers::SHIFT, k) {
                keys.push(GestureKey::Axis(axis, true));
            } else if i.consume_key(Modifiers::NONE, k) {
                keys.push(GestureKey::Axis(axis, false));
            }
        }
        const DIGITS: [(Key, char); 11] = [
            (Key::Num0, '0'), (Key::Num1, '1'), (Key::Num2, '2'), (Key::Num3, '3'), (Key::Num4, '4'),
            (Key::Num5, '5'), (Key::Num6, '6'), (Key::Num7, '7'), (Key::Num8, '8'), (Key::Num9, '9'), (Key::Period, '.'),
        ];
        for (k, c) in DIGITS {
            if i.consume_key(Modifiers::NONE, k) {
                keys.push(GestureKey::Digit(c));
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Minus) {
            keys.push(GestureKey::Minus);
        }
        if i.consume_key(Modifiers::NONE, Key::Backspace) {
            keys.push(GestureKey::Backspace);
        }
        let click = if by_drag { i.pointer.primary_released() } else { i.pointer.primary_pressed() };
        let confirm = i.consume_key(Modifiers::NONE, Key::Enter) || click;
        let cancel = i.consume_key(Modifiers::NONE, Key::Escape) || i.pointer.secondary_pressed();
        Input { mouse: i.pointer.latest_pos(), keys, confirm, cancel, snap: i.modifiers.ctrl || i.modifiers.command }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Going,
    Done,
    Cancelled,
}

/// G / R / S on a node: the gesture, writing the node's param as it goes
/// (a key at the frame when it's animated), and putting it back exactly as
/// it was (keys and all, or unset) on cancel.
pub struct Transforming {
    pub node: String,
    pub gesture: Gesture,
    /// The param as stored in the node when it started (None: unset).
    stored: Option<Value>,
    frame: i64,
    /// The params last written, for the header.
    pub now: Trs,
    /// Started by dragging a gizmo handle: releasing the button confirms.
    pub by_drag: bool,
    /// Several objects at once: each of them, the gesture running about
    /// their pivot (empty for one object).
    group: Vec<Member>,
}

/// One of several objects transformed together.
struct Member {
    node: String,
    start: Trs,
    /// Scene from its params' space.
    parent: Mat4,
    /// Whether its position follows the pivot as it turns or scales (its
    /// translate isn't linked).
    carried: bool,
    /// The params it changes, as stored when it started (None: unset).
    stored: Vec<(&'static str, Option<Value>)>,
    now: Trs,
}

impl Member {
    fn new(doc: &Doc, node: &str, world: &Mat4, mode: Mode, frame: i64) -> Member {
        let start = trs_at(doc, node, frame);
        let parent = start.matrix().try_inverse().map_or_else(Mat4::identity, |inv| world * inv);
        let carried = mode != Mode::Move && refusal(doc, node, Mode::Move).is_none();
        let mut names = vec![param(doc, node, mode)];
        if carried {
            names.push(param(doc, node, Mode::Move));
        }
        let stored = names.into_iter().map(|n| (n, doc.node(node).and_then(|x| x.params.get(n).cloned()))).collect();
        Member { node: node.into(), start, parent, carried, stored, now: start }
    }

    /// Its params after `c` about `pivot`.
    fn place(&self, c: &Change, pivot: Vec3) -> Trs {
        let at = geom::transform_point(&self.parent, &Vec3::from(self.start.translate));
        let (mut t, to) = match *c {
            Change::Move(d) => (moved_by(&self.start, &self.parent, d), None),
            Change::Turn(q, snap) => (turned_by(&self.start, &self.parent, q, snap), Some(pivot + q * (at - pivot))),
            Change::Scale { factor, on, axes } => {
                let rel = at - pivot;
                let stretch: Vec3 = (0..3).filter(|&k| on[k]).map(|k| axes[k] * (axes[k].dot(&rel) * (factor - 1.0))).sum();
                (scaled_by(&self.start, factor, on), Some(pivot + rel + stretch))
            }
        };
        if let (Some(p), true, Some(inv)) = (to, self.carried, self.parent.try_inverse()) {
            t.translate = geom::transform_point(&inv, &p).into();
        }
        t
    }
}

/// The median point of objects placed by `worlds`: the mean of their
/// origins (the pivot of a multi-object gesture).
pub fn median<'a>(worlds: impl IntoIterator<Item = &'a Mat4>) -> Option<Vec3> {
    let pts: Vec<Vec3> = worlds.into_iter().map(|w| geom::transform_point(w, &Vec3::zeros())).collect();
    (!pts.is_empty()).then(|| pts.iter().sum::<Vec3>() / pts.len() as f64)
}

/// Put `node`'s param `name` back as `stored` (None: unset).
fn put_back(doc: &mut Doc, node: &str, name: &str, stored: &Option<Value>) {
    if let Some(m) = doc.node_mut(node).and_then(|n| n.params.as_object_mut()) {
        match stored {
            Some(v) => {
                m.insert(name.into(), v.clone());
            }
            None => {
                m.remove(name);
            }
        }
        doc.changed();
    }
}

impl Transforming {
    /// Start `mode` on `node`, whose place in the scene is `world`, with the
    /// pointer at `mouse`. Err: why it can't (see [`refusal`]).
    pub fn start(doc: &Doc, node: &str, mode: Mode, world: &Mat4, mouse: Pos2, frame: i64) -> Result<Transforming, String> {
        if let Some(why) = refusal(doc, node, mode) {
            return Err(why);
        }
        let start = trs_at(doc, node, frame);
        let stored = doc.node(node).and_then(|n| n.params.get(param(doc, node, mode)).cloned());
        Ok(Transforming { node: node.into(), gesture: Gesture::new(mode, start, world, mouse), stored, frame, now: start, by_drag: false, group: Vec::new() })
    }

    /// Start `mode` on the selected `objects` (node, place in the scene),
    /// the primary last: one alone as [`Transforming::start`], several about
    /// their median point. Objects that can't take `mode` (see [`refusal`])
    /// are left out; Err when none can.
    pub fn start_many(doc: &Doc, objects: &[(String, Mat4)], mode: Mode, mouse: Pos2, frame: i64) -> Result<Transforming, String> {
        let can: Vec<&(String, Mat4)> = objects.iter().filter(|(n, _)| refusal(doc, n, mode).is_none()).collect();
        let Some((lead, world)) = can.last().copied().or(objects.last()) else { return Err(format!("Select an object to {} it", mode.name().to_lowercase())) };
        let mut t = Transforming::start(doc, lead, mode, world, mouse, frame)?;
        if can.len() > 1 {
            let pivot = median(can.iter().map(|(_, w)| w)).unwrap_or_default();
            t.gesture = t.gesture.about(pivot);
            t.group = can.iter().map(|(n, w)| Member::new(doc, n, w, mode, frame)).collect();
        }
        Ok(t)
    }

    /// A drag on gizmo handle `h` (axes in `space`) on the selected
    /// `objects` (see [`Transforming::start_many`]): releasing confirms.
    pub fn drag_many(doc: &Doc, objects: &[(String, Mat4)], h: Handle, space: Space, mouse: Pos2, frame: i64) -> Result<Transforming, String> {
        let (mode, c) = h.gesture(space);
        let t = Transforming::start_many(doc, objects, mode, mouse, frame)?;
        Ok(Transforming { gesture: t.gesture.with_constraint(c), by_drag: true, ..t })
    }


    /// One frame: handle `input`, write the param. Done or Cancelled ends it.
    pub fn step(&mut self, input: &Input, view: &dyn View, doc: &mut Doc) -> Outcome {
        for k in &input.keys {
            self.gesture.key(*k);
        }
        if input.cancel {
            self.restore(doc);
            return Outcome::Cancelled;
        }
        // (Without a pointer, e.g. it left the window, a typed amount still
        // applies.)
        let m = input.mouse.unwrap_or(self.gesture.from);
        if self.group.is_empty() {
            let t = self.gesture.apply(view, m, input.snap);
            if t != self.now {
                self.write(doc, &t);
                self.now = t;
            }
        } else {
            let c = self.gesture.change(view, m, input.snap);
            let pivot = self.gesture.origin();
            let mode = self.gesture.mode;
            for member in &mut self.group {
                let t = member.place(&c, pivot);
                if t == member.now {
                    continue;
                }
                let own = match mode {
                    Mode::Move => t.translate,
                    Mode::Rotate => t.rotate,
                    Mode::Scale => t.scale,
                };
                let (own_name, at_name) = (param(doc, &member.node, mode), param(doc, &member.node, Mode::Move));
                doc.edit_param(&member.node, own_name, self.frame, json!(own));
                if member.carried && t.translate != member.now.translate {
                    doc.edit_param(&member.node, at_name, self.frame, json!(t.translate));
                }
                member.now = t;
            }
            if let Some(lead) = self.group.iter().find(|m| m.node == self.node) {
                self.now = lead.now;
            }
        }
        if input.confirm { Outcome::Done } else { Outcome::Going }
    }

    fn write(&self, doc: &mut Doc, t: &Trs) {
        let v = match self.gesture.mode {
            Mode::Move => t.translate,
            Mode::Rotate => t.rotate,
            Mode::Scale => t.scale,
        };
        doc.edit_param(&self.node, param(doc, &self.node, self.gesture.mode), self.frame, json!(v));
    }

    /// Put the params back as stored when it started.
    fn restore(&self, doc: &mut Doc) {
        if self.group.is_empty() {
            put_back(doc, &self.node, param(doc, &self.node, self.gesture.mode), &self.stored);
        }
        for m in &self.group {
            for (name, stored) in &m.stored {
                put_back(doc, &m.node, name, stored);
            }
        }
    }
}

// ---- Gizmos ----

/// Which gizmo the viewport shows on the selected object (the viewer
/// toolbar's tools, as Blender's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    /// None: clicks only select.
    Select,
    Move,
    Rotate,
    Scale,
    /// Move, rotate and scale together (what a selected object shows
    /// until another tool is picked).
    #[default]
    All,
}

impl Tool {
    pub const ALL: [(Tool, &'static str, &'static str); 5] = [
        (Tool::Select, "Select", "No gizmo: clicks select (G / R / S still work)"),
        (Tool::Move, "Move", "Arrows move along an axis, squares in a plane, the center circle in the view plane"),
        (Tool::Rotate, "Rotate", "Rings turn about an axis, the outer ring about the view, inside like a trackball"),
        (Tool::Scale, "Scale", "Cubes scale along an axis, the center circle all of them"),
        (Tool::All, "All", "Move, rotate and scale gizmos together"),
    ];
    fn moves(self) -> bool {
        matches!(self, Tool::Move | Tool::All)
    }
    fn rotates(self) -> bool {
        matches!(self, Tool::Rotate | Tool::All)
    }
    fn scales(self) -> bool {
        matches!(self, Tool::Scale | Tool::All)
    }
}

/// A part of a gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Arrow(usize),
    /// The square for the plane without this axis.
    Square(usize),
    MoveCenter,
    Ring(usize),
    ViewRing,
    Trackball,
    Cube(usize),
    ScaleCenter,
}

impl Handle {
    /// The gesture a drag on it makes (scale is always along the object's
    /// own axes).
    pub fn gesture(self, space: Space) -> (Mode, Constraint) {
        match self {
            Handle::Arrow(k) => (Mode::Move, Constraint::Axis(k, space)),
            Handle::Square(k) => (Mode::Move, Constraint::Plane(k, space)),
            Handle::MoveCenter => (Mode::Move, Constraint::Free),
            Handle::Ring(k) => (Mode::Rotate, Constraint::Axis(k, space)),
            Handle::ViewRing => (Mode::Rotate, Constraint::Free),
            Handle::Trackball => (Mode::Rotate, Constraint::Trackball),
            Handle::Cube(k) => (Mode::Scale, Constraint::Axis(k, Space::Local)),
            Handle::ScaleCenter => (Mode::Scale, Constraint::Free),
        }
    }
}

/// How big a gizmo is on screen: its arrows' length, in points.
pub const GIZMO_SIZE: f32 = 90.0;
/// How near (points) the pointer must be to take a handle.
const GRAB: f32 = 7.0;
const AXIS_COLORS: [Color32; 3] = [Color32::from_rgb(235, 75, 85), Color32::from_rgb(120, 200, 60), Color32::from_rgb(70, 135, 245)];

/// A gizmo's parts on screen.
#[derive(Clone, Debug)]
pub struct Layout {
    pub tool: Tool,
    pub center: Pos2,
    /// Arrow k: from near the center to its tip; None when it points at
    /// (or away from) the viewer and is too short to grab.
    pub arrows: [Option<(Pos2, Pos2)>; 3],
    /// The plane squares' corners (by the axis they leave out).
    pub squares: [Option<[Pos2; 4]>; 3],
    /// Ring k as a polyline, and which samples are behind the center.
    pub rings: [Vec<(Pos2, bool)>; 3],
    /// The view ring's and trackball's radius.
    pub ring_radius: f32,
    pub cubes: [Option<Pos2>; 3],
}

impl Layout {
    /// The gizmo for `tool` on an object at scene point `origin` with axes
    /// `axes` (unit, scene space). None when it's behind the camera or the
    /// tool shows none.
    pub fn new(view: &dyn View, tool: Tool, origin: Vec3, axes: [Vec3; 3]) -> Option<Layout> {
        if tool == Tool::Select {
            return None;
        }
        let (center, depth) = view.project(origin)?;
        let len = GIZMO_SIZE as f64 / view.pixels_per_unit(depth).max(1e-9);
        let at = |v: Vec3| view.project(v).map(|p| p.0);
        let arrows = std::array::from_fn(|k| {
            let tip = at(origin + axes[k] * len)?;
            let base = at(origin + axes[k] * len * 0.2)?;
            ((tip - center).length() > 12.0 && tool.moves()).then_some((base, tip))
        });
        let squares = std::array::from_fn(|k| {
            let (u, v) = (axes[(k + 1) % 3], axes[(k + 2) % 3]);
            let (a, b) = (0.28 * len, 0.46 * len);
            let quad = [at(origin + u * a + v * a)?, at(origin + u * b + v * a)?, at(origin + u * b + v * b)?, at(origin + u * a + v * b)?];
            // Seen edge-on it can't be grabbed.
            (tool.moves() && quad_area(&quad) > 30.0).then_some(quad)
        });
        let ring_r = if tool == Tool::All { 1.15 * len } else { len };
        let toward = -view.view_dir();
        let rings = std::array::from_fn(|k| {
            if !tool.rotates() {
                return Vec::new();
            }
            let (u, v) = (axes[(k + 1) % 3], axes[(k + 2) % 3]);
            (0..=64)
                .filter_map(|i| {
                    let t = i as f64 / 64.0 * std::f64::consts::TAU;
                    let off = (u * t.cos() + v * t.sin()) * ring_r;
                    at(origin + off).map(|p| (p, off.dot(&toward) < -1e-6 * ring_r))
                })
                .collect()
        });
        let cube_at = if tool == Tool::All { 0.62 } else { 1.0 };
        let cubes = std::array::from_fn(|k| {
            let p = at(origin + axes[k] * len * cube_at)?;
            (tool.scales() && (p - center).length() > 10.0).then_some(p)
        });
        let ring_radius = (GIZMO_SIZE * if tool == Tool::All { 1.15 } else { 1.0 }) * 1.12;
        Some(Layout { tool, center, arrows, squares, rings, ring_radius, cubes })
    }

    /// The handle under screen point `p`, the small ones first.
    pub fn hit(&self, p: Pos2) -> Option<Handle> {
        for k in 0..3 {
            if self.cubes[k].is_some_and(|c| (c - p).length() < GRAB + 2.0) {
                return Some(Handle::Cube(k));
            }
        }
        if self.tool.moves() && (p - self.center).length() < GRAB + 3.0 {
            return Some(Handle::MoveCenter);
        }
        if self.tool == Tool::Scale && (p - self.center).length() < GRAB + 5.0 {
            return Some(Handle::ScaleCenter);
        }
        for k in 0..3 {
            if self.squares[k].is_some_and(|q| in_quad(&q, p)) {
                return Some(Handle::Square(k));
            }
        }
        for k in 0..3 {
            if self.arrows[k].is_some_and(|(a, b)| segment_distance(a, b, p) < GRAB) {
                return Some(Handle::Arrow(k));
            }
        }
        if self.tool.rotates() {
            // Rings cross on screen: the nearest one.
            let dist = |k: usize| self.rings[k].windows(2).map(|w| segment_distance(w[0].0, w[1].0, p)).fold(f32::INFINITY, f32::min);
            if let Some((k, d)) = (0..3).map(|k| (k, dist(k))).min_by(|a, b| a.1.total_cmp(&b.1)) {
                if d < GRAB {
                    return Some(Handle::Ring(k));
                }
            }
            let r = (p - self.center).length();
            if (r - self.ring_radius).abs() < GRAB {
                return Some(Handle::ViewRing);
            }
            if self.tool == Tool::Rotate && r < self.ring_radius - GRAB {
                return Some(Handle::Trackball);
            }
        }
        None
    }

    /// Ring `k`'s direction on screen nearest point `p` (unit, the way a
    /// positive turn about its axis moves it) and the ring's radius on
    /// screen, for [`Gesture::along`]. None where the ring runs toward
    /// the viewer there (no direction to drag along).
    pub fn ring_tangent(&self, k: usize, p: Pos2) -> Option<(Vec2, f32)> {
        let ring = &self.rings[k];
        // The nearest stretch; where the near and far halves overlap (seen
        // edge-on), the near one.
        let dist = |i: usize| segment_distance(ring[i].0, ring[i + 1].0, p) + if ring[i].1 && ring[i + 1].1 { 1.5 } else { 0.0 };
        let i = (0..ring.len().saturating_sub(1)).min_by(|&a, &b| dist(a).total_cmp(&dist(b)))?;
        // A few samples either side, so a foreshortened stretch still has
        // a direction.
        let d = ring[(i + 3).min(ring.len() - 1)].0 - ring[i.saturating_sub(2)].0;
        let radius = if self.tool == Tool::All { 1.15 * GIZMO_SIZE } else { GIZMO_SIZE };
        (d.length() > 2.0).then(|| (d.normalized(), radius))
    }

    /// Draw it; `hot` lit (hovered or dragged), all of it gray when
    /// `enabled` is false (the param is linked).
    pub fn draw(&self, painter: &egui::Painter, hot: Option<Handle>, enabled: bool) {
        let color = |k: usize, h: Handle| -> Color32 {
            if !enabled {
                Color32::from_gray(110)
            } else if hot == Some(h) {
                Color32::from_rgb(255, 225, 120)
            } else if k < 3 {
                AXIS_COLORS[k]
            } else {
                Color32::from_gray(225)
            }
        };
        if self.tool.rotates() {
            let r = self.ring_radius;
            painter.circle_stroke(self.center, r, Stroke::new(if hot == Some(Handle::ViewRing) { 2.5 } else { 1.5 }, color(3, Handle::ViewRing)));
            if self.tool == Tool::Rotate && hot == Some(Handle::Trackball) {
                painter.circle_filled(self.center, r - GRAB, Color32::from_white_alpha(14));
            }
            for k in 0..3 {
                let c = color(k, Handle::Ring(k));
                let w = if hot == Some(Handle::Ring(k)) { 3.0 } else { 2.0 };
                for seg in self.rings[k].windows(2) {
                    // The far half fainter, as Blender draws it.
                    let c = if seg[0].1 && seg[1].1 { c.gamma_multiply(0.3) } else { c };
                    painter.line_segment([seg[0].0, seg[1].0], Stroke::new(w, c));
                }
            }
        }
        for k in 0..3 {
            if let Some(q) = self.squares[k] {
                let c = color(k, Handle::Square(k));
                painter.add(egui::Shape::convex_polygon(q.to_vec(), c.gamma_multiply(0.45), Stroke::new(1.0, c)));
            }
            if let Some((a, b)) = self.arrows[k] {
                let c = color(k, Handle::Arrow(k));
                painter.line_segment([a, b], Stroke::new(if hot == Some(Handle::Arrow(k)) { 3.5 } else { 2.5 }, c));
                // The head: a small triangle along the arrow.
                let d = (b - a).normalized();
                let n = Vec2::new(-d.y, d.x);
                painter.add(egui::Shape::convex_polygon(vec![b + d * 10.0, b + n * 5.0, b - n * 5.0], c, Stroke::NONE));
            }
            if let Some(p) = self.cubes[k] {
                let c = color(k, Handle::Cube(k));
                painter.rect_filled(egui::Rect::from_center_size(p, Vec2::splat(9.0)), 1.0, c);
                if self.tool == Tool::Scale {
                    painter.line_segment([self.center, p], Stroke::new(2.0, c));
                }
            }
        }
        if self.tool.moves() || self.tool == Tool::Scale {
            let h = if self.tool.moves() { Handle::MoveCenter } else { Handle::ScaleCenter };
            painter.circle_stroke(self.center, GRAB + 1.0, Stroke::new(if hot == Some(h) { 2.5 } else { 1.5 }, color(3, h)));
        }
    }
}

/// An object's axes in `space`, from its place in the scene.
pub fn axes_of(world: &Mat4, space: Space) -> [Vec3; 3] {
    std::array::from_fn(|k| match space {
        Space::Global => Vec3::ith(k, 1.0),
        Space::Local => world.fixed_view::<3, 3>(0, 0).column(k).into_owned().try_normalize(1e-12).unwrap_or_else(|| Vec3::ith(k, 1.0)),
    })
}

/// The distance from `p` to the segment `a`–`b`.
fn segment_distance(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

fn quad_area(q: &[Pos2; 4]) -> f32 {
    let cross = |a: Pos2, b: Pos2| a.x * b.y - a.y * b.x;
    (0..4).map(|i| cross(q[i], q[(i + 1) % 4])).sum::<f32>().abs() / 2.0
}

/// Whether `p` is inside convex quad `q` (either winding).
fn in_quad(q: &[Pos2; 4], p: Pos2) -> bool {
    let side = |a: Pos2, b: Pos2| (b - a).x * (p - a).y - (b - a).y * (p - a).x;
    let s: Vec<f32> = (0..4).map(|i| side(q[i], q[(i + 1) % 4])).collect();
    s.iter().all(|&x| x >= 0.0) || s.iter().all(|&x| x <= 0.0)
}

fn nonzero(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { x }
}

/// `x` rounded to a multiple of `step` when `on`.
pub fn snapped(x: f64, step: f64, on: bool) -> f64 {
    if on { (x / step).round() * step } else { x }
}

/// `a` in -180..180.
fn wrap_degrees(a: f64) -> f64 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// The counter-clockwise angle (degrees) of `p` about `c` on screen (y
/// down); None at the center.
fn screen_angle(c: Pos2, p: Pos2) -> Option<f64> {
    let d = p - c;
    (d.length() > 1e-3).then(|| (-(d.y as f64)).atan2(d.x as f64).to_degrees())
}

/// Euler angles for the same rotation as `a`, nearest `near`: each angle
/// moved by whole turns, and the other triple for that rotation
/// (x + 180, 180 − y, z + 180) when it's nearer.
pub fn nearest_angles(a: [f64; 3], near: [f64; 3]) -> [f64; 3] {
    let fit = |v: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| v[i] + ((near[i] - v[i]) / 360.0).round() * 360.0) };
    let dist = |v: &[f64; 3]| (0..3).map(|i| (v[i] - near[i]).abs()).sum::<f64>();
    let one = fit(a);
    let two = fit([a[0] + 180.0, 180.0 - a[1], a[2] + 180.0]);
    if dist(&two) < dist(&one) - 1e-9 { two } else { one }
}

/// Where along line `o + t·a` (unit `a`) it comes nearest the ray `r0 +
/// u·d` (unit `d`); None when they're parallel.
pub fn line_ray(o: &Vec3, a: &Vec3, r0: &Vec3, d: &Vec3) -> Option<f64> {
    let b = a.dot(d);
    let w = o - r0;
    let den = 1.0 - b * b;
    (den.abs() > 1e-9).then(|| (b * d.dot(&w) - a.dot(&w)) / den)
}

/// Where the ray `r0 + u·d` meets the plane through `p0` with normal `n`;
/// None when it runs along the plane or the plane is behind it.
pub fn ray_plane(r0: &Vec3, d: &Vec3, p0: &Vec3, n: &Vec3) -> Option<Vec3> {
    let den = d.dot(n);
    if den.abs() < 1e-9 {
        return None;
    }
    let u = (p0 - r0).dot(n) / den;
    (u >= 0.0).then(|| r0 + d * u)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use arclight::geom::Camera;
    use arclight::image::Size;

    /// A pinhole view filling a 800×600 screen.
    pub struct Pinhole(pub Camera);

    const SCREEN: Size = Size { width: 800, height: 600 };

    impl View for Pinhole {
        fn project(&self, p: Vec3) -> Option<(Pos2, f64)> {
            self.0.project(&p, SCREEN).map(|(s, d)| (Pos2::new(s[0] as f32, s[1] as f32), d))
        }
        fn ray(&self, s: Pos2) -> (Vec3, Vec3) {
            let o = geom::transform_point(&self.0.world, &Vec3::zeros());
            let p = self.0.unproject([s.x as f64, s.y as f64], 1.0, SCREEN);
            (o, (p - o).normalize())
        }
        fn pixels_per_unit(&self, depth: f64) -> f64 {
            self.0.focal_pixels(SCREEN) / depth
        }
        fn view_dir(&self) -> Vec3 {
            (self.0.world.fixed_view::<3, 3>(0, 0) * -Vec3::z()).normalize()
        }
    }

    /// Looking down −Z from (0, 0, 10).
    pub fn front() -> Pinhole {
        Pinhole(Camera { world: geom::trs([0.0, 0.0, 10.0], [0.0; 3], [1.0; 3]), ..Camera::default() })
    }

    /// From above and in front: (0, 6, 8), tilted down.
    fn high() -> Pinhole {
        Pinhole(Camera { world: geom::trs([0.0, 6.0, 8.0], [-36.87, 0.0, 0.0], [1.0; 3]), ..Camera::default() })
    }

    fn at(view: &Pinhole, p: [f64; 3]) -> Pos2 {
        view.project(Vec3::from(p)).unwrap().0
    }

    fn near(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < tol)
    }

    fn start(t: [f64; 3]) -> Trs {
        Trs { translate: t, ..Trs::default() }
    }

    #[test]
    fn moving_along_an_axis_follows_the_pointer_at_the_objects_depth() {
        for view in [front(), high()] {
            // An object off center and away from the camera.
            let s = start([1.0, 0.5, -3.0]);
            let mut g = Gesture::new(Mode::Move, s, &s.matrix(), at(&view, s.translate)).with_constraint(Constraint::Axis(0, Space::Global));
            let t = g.apply(&view, at(&view, [3.5, 0.5, -3.0]), false);
            assert!(near(t.translate, [3.5, 0.5, -3.0], 1e-3), "{:?}", t.translate);
            // Off the axis on screen: only its nearest point counts.
            let t = g.apply(&view, at(&view, [3.5, 0.5, -3.0]) + eframe::egui::vec2(0.0, 40.0), false);
            assert!((t.translate[1] - 0.5).abs() < 1e-9 && (t.translate[2] + 3.0).abs() < 1e-9);
            // Snapping: whole units.
            let t = g.apply(&view, at(&view, [2.4, 0.5, -3.0]), true);
            assert!(near(t.translate, [2.0, 0.5, -3.0], 1e-9), "{:?}", t.translate);
        }
    }

    #[test]
    fn free_and_plane_moves_stay_in_their_plane() {
        // Free: in the view plane through the object.
        let view = front();
        let s = start([0.0, 0.0, 0.0]);
        let mut g = Gesture::new(Mode::Move, s, &s.matrix(), at(&view, [0.0; 3]));
        let t = g.apply(&view, at(&view, [1.0, 1.0, 0.0]), false);
        assert!(near(t.translate, [1.0, 1.0, 0.0], 1e-4), "{:?}", t.translate);
        // Shift + Y: on the ground (XZ), seen from above.
        let view = high();
        let mut g = Gesture::new(Mode::Move, s, &s.matrix(), at(&view, [0.0; 3]));
        g.key(GestureKey::Axis(1, true));
        assert_eq!(g.constraint, Constraint::Plane(1, Space::Global));
        let t = g.apply(&view, at(&view, [1.0, 0.0, 2.0]), false);
        assert!(near(t.translate, [1.0, 0.0, 2.0], 1e-4), "{:?}", t.translate);
    }

    #[test]
    fn local_axes_follow_the_objects_rotation_and_parent() {
        let view = front();
        // Turned 90° about Z: its own X is the world's Y.
        let s = Trs { rotate: [0.0, 0.0, 90.0], ..Trs::default() };
        let mut g = Gesture::new(Mode::Move, s, &s.matrix(), at(&view, [0.0; 3]));
        g.key(GestureKey::Axis(0, false));
        g.key(GestureKey::Axis(0, false));
        assert_eq!(g.constraint, Constraint::Axis(0, Space::Local));
        g.key(GestureKey::Digit('2'));
        let t = g.apply(&view, Pos2::ZERO, false);
        assert!(near(t.translate, [0.0, 2.0, 0.0], 1e-9), "{:?}", t.translate);
        // A third X: free again.
        g.key(GestureKey::Axis(0, false));
        assert_eq!(g.constraint, Constraint::Free);
        // Under a parent scaled 2×, a world move of 2 is 1 in params.
        let parent = Mat4::new_scaling(2.0);
        let s = start([0.0; 3]);
        let mut g = Gesture::new(Mode::Move, s, &(parent * s.matrix()), at(&view, [0.0; 3])).with_constraint(Constraint::Axis(0, Space::Global));
        let t = g.apply(&view, at(&view, [2.0, 0.0, 0.0]), false);
        assert!(near(t.translate, [1.0, 0.0, 0.0], 1e-4), "{:?}", t.translate);
    }

    #[test]
    fn rings_turn_by_the_pointers_angle_about_the_center() {
        let view = front();
        let s = Trs::default();
        let c = at(&view, [0.0; 3]);
        let right = c + eframe::egui::vec2(100.0, 0.0);
        let up = c + eframe::egui::vec2(0.0, -100.0);
        let left = c + eframe::egui::vec2(-100.0, 0.0);
        // Z points at the viewer: a quarter turn counter-clockwise is +90°.
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), right).with_constraint(Constraint::Axis(2, Space::Global));
        let t = g.apply(&view, up, false);
        assert!(near(t.rotate, [0.0, 0.0, 90.0], 1e-6), "{:?}", t.rotate);
        // On past the half turn: it keeps counting.
        g.apply(&view, left, false);
        let t = g.apply(&view, c + eframe::egui::vec2(0.0, 100.0), false);
        assert!(near(t.rotate, [0.0, 0.0, 270.0], 1e-6), "{:?}", t.rotate);
        // Free R turns about the view axis (here also Z); snapping: 15°.
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), right);
        let a = 37f32.to_radians();
        let t = g.apply(&view, c + eframe::egui::vec2(100.0 * a.cos(), -100.0 * a.sin()), true);
        assert!(near(t.rotate, [0.0, 0.0, 30.0], 1e-6), "{:?}", t.rotate);
        // About Y (seen edge-on from the front it points up): typed.
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), right);
        for k in [GestureKey::Axis(1, false), GestureKey::Digit('4'), GestureKey::Digit('5'), GestureKey::Minus] {
            g.key(k);
        }
        let t = g.apply(&view, up, false);
        assert!(near(t.rotate, [0.0, -45.0, 0.0], 1e-6), "{:?}", t.rotate);
        // Local X adds to rotate.x alone, past 180.
        let s = Trs { rotate: [170.0, 20.0, 30.0], ..Trs::default() };
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), right);
        g.key(GestureKey::Axis(0, false));
        g.key(GestureKey::Axis(0, false));
        g.key(GestureKey::Digit('2'));
        g.key(GestureKey::Digit('0'));
        assert!(near(g.apply(&view, up, false).rotate, [190.0, 20.0, 30.0], 1e-9));
    }

    #[test]
    fn a_world_axis_turn_of_a_turned_object_keeps_its_angles_near() {
        let view = front();
        let s = Trs { rotate: [10.0, 20.0, 30.0], ..Trs::default() };
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), Pos2::ZERO).with_constraint(Constraint::Axis(0, Space::Global));
        g.key(GestureKey::Digit('5'));
        let t = g.apply(&view, Pos2::ZERO, false);
        // Same rotation as turning the start 5° about world X…
        let want = Rotation3::from_axis_angle(&Vec3::x_axis(), 5f64.to_radians()) * geom::rotation(s.rotate);
        assert!((geom::rotation(t.rotate).matrix() - want.matrix()).norm() < 1e-9);
        // …with angles close to where it started.
        assert!(near(t.rotate, s.rotate, 10.0), "{:?}", t.rotate);
        assert_eq!(nearest_angles([-170.0, 0.0, 0.0], [175.0, 0.0, 0.0]), [190.0, 0.0, 0.0]);
    }

    #[test]
    fn a_ring_seen_edge_on_turns_by_the_drag_along_it() {
        // From the front the Y ring is a flat line through the center: the
        // pointer's angle about the center would flip 0° / 180° along it.
        let view = front();
        let s = Trs::default();
        let axes = axes_of(&Mat4::identity(), Space::Global);
        let lay = Layout::new(&view, Tool::Rotate, Vec3::zeros(), axes).unwrap();
        let c = at(&view, [0.0; 3]);
        let grab = c + eframe::egui::vec2(40.0, 0.0);
        let along = lay.ring_tangent(1, grab).unwrap();
        assert!(along.0.x > 0.99, "the near half, which a +Y turn moves right: {along:?}");
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), grab).with_constraint(Constraint::Axis(1, Space::Global)).along(Some(along));
        let mut last = 0.0;
        // Steadily on past the center: the turn grows steadily.
        for step in 1..=10 {
            let t = g.apply(&view, grab + eframe::egui::vec2(-10.0 * step as f32, 0.0), false);
            assert!(t.rotate[0].abs() < 1e-6 && t.rotate[2].abs() < 1e-6, "{:?}", t.rotate);
            let want = -(10.0 * step as f64 / GIZMO_SIZE as f64).to_degrees();
            assert!((t.rotate[1] - want).abs() < 1e-6 && t.rotate[1] < last, "{:?} vs {want}", t.rotate);
            last = t.rotate[1];
        }
        // Snapping still rounds to 15°.
        let t = g.apply(&view, grab + eframe::egui::vec2(30.0, 0.0), true);
        assert!(near(t.rotate, [0.0, 15.0, 0.0], 1e-6), "{:?}", t.rotate);
    }

    #[test]
    fn turning_close_to_the_center_is_damped() {
        // A quarter turn 10 points from the center turns a sixth of it:
        // no wild swings from a pointer near the object.
        let view = front();
        let s = Trs::default();
        let c = at(&view, [0.0; 3]);
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), c + eframe::egui::vec2(10.0, 0.0));
        let t = g.apply(&view, c + eframe::egui::vec2(0.0, -10.0), false);
        assert!(near(t.rotate, [0.0, 0.0, 15.0], 1e-6), "{:?}", t.rotate);
    }

    #[test]
    fn the_trackball_turns_about_the_views_axes() {
        let view = front();
        let s = Trs::default();
        let mut g = Gesture::new(Mode::Rotate, s, &s.matrix(), Pos2::new(400.0, 300.0)).with_constraint(Constraint::Trackball);
        // 60 points right: 30° about the view's up (world Y here).
        let t = g.apply(&view, Pos2::new(460.0, 300.0), false);
        assert!(near(t.rotate, [0.0, 30.0, 0.0], 1e-6), "{:?}", t.rotate);
    }

    #[test]
    fn scaling_goes_by_distance_from_the_center() {
        let view = front();
        let s = Trs { scale: [1.0, 2.0, 1.0], ..Trs::default() };
        let c = at(&view, [0.0; 3]);
        let from = c + eframe::egui::vec2(50.0, 0.0);
        let mut g = Gesture::new(Mode::Scale, s, &s.matrix(), from);
        let t = g.apply(&view, c + eframe::egui::vec2(0.0, 75.0), false);
        assert!(near(t.scale, [1.5, 3.0, 1.5], 1e-6), "{:?}", t.scale);
        assert!(near(g.apply(&view, c + eframe::egui::vec2(63.0, 0.0), true).scale, [1.3, 2.6, 1.3], 1e-9), "snaps the factor to 0.1");
        // X only, then everything but X; typed.
        g.key(GestureKey::Axis(0, false));
        g.key(GestureKey::Digit('2'));
        assert_eq!(g.apply(&view, from, false).scale, [2.0, 2.0, 1.0]);
        g.key(GestureKey::Axis(0, true));
        assert_eq!(g.apply(&view, from, false).scale, [1.0, 4.0, 2.0]);
        // A scale keeps the object where it is.
        assert_eq!(g.apply(&view, from, false).translate, s.translate);
    }

    #[test]
    fn typing_negating_and_cancelling() {
        let view = front();
        let s = start([1.0, 2.0, 3.0]);
        let mut g = Gesture::new(Mode::Move, s, &s.matrix(), Pos2::ZERO);
        for k in [GestureKey::Axis(2, false), GestureKey::Digit('1'), GestureKey::Digit('.'), GestureKey::Digit('.'), GestureKey::Digit('5'), GestureKey::Minus] {
            g.key(k);
        }
        assert_eq!(g.typed(), Some(-1.5), "a second '.' is ignored");
        let t = g.apply(&view, Pos2::new(123.0, 45.0), false);
        assert_eq!(t.translate, [1.0, 2.0, 1.5], "typed wins over the pointer");
        assert_eq!(g.describe(&t), "Move Z  -1.5  (typed)");
        g.key(GestureKey::Backspace);
        g.key(GestureKey::Backspace);
        g.key(GestureKey::Backspace);
        assert_eq!(g.typed(), None);
        assert_eq!(g.start, s, "the start stays as it was, for cancelling");
        // Alt + G / R / S.
        let t = Trs { translate: [1.0; 3], rotate: [2.0; 3], scale: [3.0; 3] };
        assert_eq!(t.reset(Mode::Rotate), Trs { rotate: [0.0; 3], ..t });
        assert_eq!(t.reset(Mode::Scale).scale, [1.0; 3]);
    }

    impl Transforming {
        /// Type `digits` along X and step once.
        fn key_typed(&mut self, doc: &mut Doc, view: &Pinhole, digits: &str) {
            let mut keys = vec![GestureKey::Axis(0, false)];
            keys.extend(digits.chars().map(GestureKey::Digit));
            self.step(&Input { keys, ..input(Pos2::ZERO) }, view, doc);
        }
    }

    fn input(mouse: Pos2) -> Input {
        Input { mouse: Some(mouse), ..Input::default() }
    }

    #[test]
    fn grab_writes_the_node_keys_when_animated_and_cancel_puts_it_back() {
        use arclight::graph::Graph;
        let view = front();
        let mut doc = Doc::new(Graph::default(), None);
        let card = doc.add_node("card", [0.0, 0.0]);
        // Unset, as in a file that leaves it out.
        doc.node_mut(&card).unwrap().params.as_object_mut().unwrap().remove("rotate");
        doc.commit(false);
        let placed = doc.node(&card).unwrap().params["translate"].clone();
        let c = at(&view, [0.0; 3]);
        // G, then the pointer to x = 2: the card follows; Enter confirms.
        let mut g = Transforming::start(&doc, &card, Mode::Move, &Mat4::identity(), c, 1).unwrap();
        assert_eq!(g.step(&input(at(&view, [1.0, 0.0, 0.0])), &view, &mut doc), Outcome::Going);
        let keys = Input { keys: vec![GestureKey::Axis(0, false)], ..input(at(&view, [2.0, 0.0, 0.0])) };
        assert_eq!(g.step(&keys, &view, &mut doc), Outcome::Going);
        assert!(near(trs_at(&doc, &card, 1).translate, [2.0, 0.0, 0.0], 1e-4));
        assert_eq!(g.step(&Input { confirm: true, ..input(at(&view, [2.0, 0.0, 0.0])) }, &view, &mut doc), Outcome::Done);
        doc.commit(false);
        // One undo step for the whole gesture.
        doc.undo();
        assert_eq!(doc.node(&card).unwrap().params["translate"], placed);
        doc.redo();

        // Cancel: exactly as before, even an unset param, and no undo step.
        let mut g = Transforming::start(&doc, &card, Mode::Rotate, &Mat4::identity(), c + eframe::egui::vec2(100.0, 0.0), 1).unwrap();
        g.step(&input(c + eframe::egui::vec2(0.0, -100.0)), &view, &mut doc);
        assert!(doc.node(&card).unwrap().params.get("rotate").is_some());
        assert_eq!(g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc), Outcome::Cancelled);
        assert_eq!(doc.node(&card).unwrap().params.get("rotate"), None);
        let before = doc.graph.clone();
        doc.commit(false);
        doc.undo();
        assert_ne!(doc.graph, before, "the undo went to the move, not a cancelled rotate");

        // Animated: a key at the frame; cancel takes the key out again.
        doc.redo();
        doc.set_param(&card, "translate", json!({ "keys": [[1, [0, 0, 0]], [10, [5, 0, 0]]], "interp": "linear" }));
        let stored = doc.node(&card).unwrap().params["translate"].clone();
        let mut g = Transforming::start(&doc, &card, Mode::Move, &Mat4::identity(), at(&view, [2.5, 0.0, 0.0]), 5).unwrap();
        g.key_typed(&mut doc, &view, "1");
        assert!(arclight::anim::has_key_at(&doc.node(&card).unwrap().params["translate"], 5));
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        assert_eq!(doc.node(&card).unwrap().params["translate"], stored);
    }

    #[test]
    fn several_objects_move_turn_and_scale_about_their_median() {
        use arclight::graph::Graph;
        let view = front();
        let mut doc = Doc::new(Graph::default(), None);
        let (a, b) = (doc.add_node("card", [0.0, 0.0]), doc.add_node("card", [0.0, 100.0]));
        doc.set_param(&a, "translate", json!([-2, 0, 1]));
        doc.set_param(&b, "translate", json!([2, 0, -1]));
        // Unset on a, as in a file that leaves it out.
        doc.node_mut(&a).unwrap().params.as_object_mut().unwrap().remove("rotate");
        // Something else selected too, with no transform: left out.
        let grade = doc.add_node("grade", [200.0, 0.0]);
        doc.commit(false);
        let placed = |doc: &Doc| [a.as_str(), b.as_str()].map(|n| trs_at(doc, n, 1));
        let start = placed(&doc);
        let objects: Vec<(String, Mat4)> = [&a, &grade, &b].iter().map(|n| (n.to_string(), trs_at(&doc, n, 1).matrix())).collect();
        assert_eq!(median([start[0].matrix(), start[1].matrix()].iter()), Some(Vec3::zeros()));
        assert_eq!(median([].iter()), None);
        let typed = |g: &mut Transforming, doc: &mut Doc, axis: usize, v: &str| {
            let mut keys = vec![GestureKey::Axis(axis, false)];
            keys.extend(v.chars().map(GestureKey::Digit));
            g.step(&Input { keys, ..input(Pos2::ZERO) }, &view, doc);
        };
        // G X 1: both move by one; Enter; one undo step for both.
        let mut g = Transforming::start_many(&doc, &objects, Mode::Move, Pos2::ZERO, 1).unwrap();
        typed(&mut g, &mut doc, 0, "1");
        assert_eq!(g.step(&Input { confirm: true, ..input(Pos2::ZERO) }, &view, &mut doc), Outcome::Done);
        let moved = placed(&doc);
        assert!(near(moved[0].translate, [-1.0, 0.0, 1.0], 1e-9) && near(moved[1].translate, [3.0, 0.0, -1.0], 1e-9));
        doc.commit(false);
        doc.undo();
        assert_eq!(placed(&doc), start);
        // R Z 90 about the median (0, 0, 0): each turns, and goes round it.
        let mut g = Transforming::start_many(&doc, &objects, Mode::Rotate, Pos2::ZERO, 1).unwrap();
        typed(&mut g, &mut doc, 2, "90");
        let turned = placed(&doc);
        assert!(near(turned[0].translate, [0.0, -2.0, 1.0], 1e-9) && near(turned[1].translate, [0.0, 2.0, -1.0], 1e-9), "{turned:?}");
        assert!(turned.iter().all(|t| near(t.rotate, [0.0, 0.0, 90.0], 1e-9)));
        // Cancel: both exactly as before, a's rotate unset again.
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        assert_eq!(placed(&doc), start);
        assert_eq!(doc.node(&a).unwrap().params.get("rotate"), None);
        // S 2: each twice the size, twice as far from the median.
        let mut g = Transforming::start_many(&doc, &objects, Mode::Scale, Pos2::ZERO, 1).unwrap();
        g.step(&Input { keys: vec![GestureKey::Digit('2')], ..input(Pos2::ZERO) }, &view, &mut doc);
        let scaled = placed(&doc);
        assert!(near(scaled[0].translate, [-4.0, 0.0, 2.0], 1e-9) && near(scaled[1].scale, [2.0; 3], 1e-9), "{scaled:?}");
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        // Scaling along X only stretches positions along X.
        let mut g = Transforming::start_many(&doc, &objects, Mode::Scale, Pos2::ZERO, 1).unwrap();
        typed(&mut g, &mut doc, 0, "3");
        let stretched = placed(&doc);
        assert!(near(stretched[1].translate, [6.0, 0.0, -1.0], 1e-9) && near(stretched[1].scale, [3.0, 1.0, 1.0], 1e-9), "{stretched:?}");
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        assert_eq!(placed(&doc), start);
        // b's translate linked: a move leaves it out; a turn turns it in
        // place (its position can't follow).
        let loc = doc.add_node("locator", [0.0, 200.0]);
        doc.set_param(&b, "translate", arclight::link::param(&loc, "translate"));
        let linked = doc.node(&b).unwrap().params["translate"].clone();
        let mut g = Transforming::start_many(&doc, &objects, Mode::Move, Pos2::ZERO, 1).unwrap();
        typed(&mut g, &mut doc, 0, "1");
        assert_eq!(doc.node(&b).unwrap().params["translate"], linked);
        assert!(near(trs_at(&doc, &a, 1).translate, [-1.0, 0.0, 1.0], 1e-9));
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        let mut g = Transforming::start_many(&doc, &objects, Mode::Rotate, Pos2::ZERO, 1).unwrap();
        typed(&mut g, &mut doc, 2, "90");
        assert_eq!(doc.node(&b).unwrap().params["translate"], linked);
        assert!(near(trs_at(&doc, &b, 1).rotate, [0.0, 0.0, 90.0], 1e-9));
        g.step(&Input { cancel: true, ..Input::default() }, &view, &mut doc);
        // Nothing that can move: refused with the reason.
        assert!(Transforming::start_many(&doc, &[(grade.clone(), Mat4::identity())], Mode::Move, Pos2::ZERO, 1).err().unwrap().contains("no translate"));
    }

    #[test]
    fn linked_or_missing_params_refuse_and_alt_resets() {
        use arclight::graph::Graph;
        let mut doc = Doc::new(Graph::default(), None);
        let card = doc.add_node("card", [0.0, 0.0]);
        let loc = doc.add_node("locator", [0.0, 100.0]);
        doc.set_param(&card, "translate", arclight::link::param(&loc, "translate"));
        let why = Transforming::start(&doc, &card, Mode::Move, &Mat4::identity(), Pos2::ZERO, 1).err().unwrap();
        assert!(why.contains("linked") && why.contains(&loc) && why.contains("Unlink"), "{why}");
        assert!(refusal(&doc, &loc, Mode::Rotate).unwrap().contains("no rotate"));
        assert!(refusal(&doc, &loc, Mode::Move).is_none());
        // Alt + R / S: back to the defaults.
        doc.set_param(&card, "rotate", json!([10, 20, 30]));
        doc.set_param(&card, "scale", json!([2, 2, 2]));
        reset(&mut doc, &card, Mode::Rotate, 1).unwrap();
        reset(&mut doc, &card, Mode::Scale, 1).unwrap();
        assert_eq!(trs_at(&doc, &card, 1).rotate, [0.0; 3]);
        assert_eq!(trs_at(&doc, &card, 1).scale, [1.0; 3]);
        assert!(reset(&mut doc, &card, Mode::Move, 1).is_err(), "linked");
    }

    /// From above and to the side, so no axis points at the camera.
    fn oblique() -> Pinhole {
        let world = geom::trs([6.0, 5.0, 8.0], [-27.0, 37.0, 0.0], [1.0; 3]);
        Pinhole(Camera { world, ..Camera::default() })
    }

    #[test]
    fn gizmo_handles_are_where_they_are_drawn() {
        let view = oblique();
        let axes = axes_of(&Mat4::identity(), Space::Global);
        let lay = Layout::new(&view, Tool::Move, Vec3::zeros(), axes).unwrap();
        let (base, tip) = lay.arrows[0].unwrap();
        assert!(((tip - lay.center).length() - GIZMO_SIZE).abs() < 25.0, "about GIZMO_SIZE long: {}", (tip - lay.center).length());
        assert_eq!(lay.hit(base.lerp(tip, 0.6)), Some(Handle::Arrow(0)));
        assert_eq!(lay.hit(lay.center + Vec2::new(2.0, 1.0)), Some(Handle::MoveCenter));
        let q = lay.squares[1].unwrap();
        let mid = Pos2::new(q.iter().map(|p| p.x).sum::<f32>() / 4.0, q.iter().map(|p| p.y).sum::<f32>() / 4.0);
        assert_eq!(lay.hit(mid), Some(Handle::Square(1)));
        assert_eq!(lay.hit(lay.center + Vec2::new(300.0, 0.0)), None);
        assert!(lay.rings.iter().all(Vec::is_empty) && lay.cubes.iter().all(Option::is_none), "move only");

        // Rotate: on a ring, the outer ring, inside.
        let lay = Layout::new(&view, Tool::Rotate, Vec3::zeros(), axes).unwrap();
        let on_z = lay.rings[2][10].0;
        assert_eq!(lay.hit(on_z), Some(Handle::Ring(2)));
        assert_eq!(lay.hit(lay.center + Vec2::new(lay.ring_radius, 0.0)), Some(Handle::ViewRing));
        assert_eq!(lay.hit(lay.center + Vec2::new(3.0, 3.0)), Some(Handle::Trackball));
        // Scale: cubes at the tips, the center for all axes.
        let lay = Layout::new(&view, Tool::Scale, Vec3::zeros(), axes).unwrap();
        assert_eq!(lay.hit(lay.cubes[2].unwrap()), Some(Handle::Cube(2)));
        assert_eq!(lay.hit(lay.center), Some(Handle::ScaleCenter));
        assert!(Layout::new(&view, Tool::Select, Vec3::zeros(), axes).is_none());
    }

    #[test]
    fn an_arrow_pointing_at_the_viewer_isnt_offered() {
        // From the front, Z points straight at the camera.
        let lay = Layout::new(&front(), Tool::Move, Vec3::zeros(), axes_of(&Mat4::identity(), Space::Global)).unwrap();
        assert!(lay.arrows[2].is_none() && lay.arrows[0].is_some());
        assert!(lay.squares[2].is_some() && lay.squares[0].is_none(), "the XY square faces the camera; YZ is edge-on");
        // Local axes of an object turned 90° about Z: its X arrow points up.
        let world = geom::trs([0.0; 3], [0.0, 0.0, 90.0], [1.0; 3]);
        let lay = Layout::new(&front(), Tool::Move, Vec3::zeros(), axes_of(&world, Space::Local)).unwrap();
        let (_, tip) = lay.arrows[0].unwrap();
        assert!(tip.y < lay.center.y - 50.0 && (tip.x - lay.center.x).abs() < 1.0);
    }

    #[test]
    fn dragging_a_handle_edits_only_what_it_should() {
        use arclight::graph::Graph;
        let view = oblique();
        let mut doc = Doc::new(Graph::default(), None);
        let card = doc.add_node("card", [0.0, 0.0]);
        let axes = axes_of(&Mat4::identity(), Space::Global);
        let lay = Layout::new(&view, Tool::All, Vec3::zeros(), axes).unwrap();
        // The X arrow, grabbed at its tip (x = len) and dragged to where
        // x = 3 + len is: 3 along X. Released.
        let (_, tip) = lay.arrows[0].unwrap();
        let len = GIZMO_SIZE as f64 / view.pixels_per_unit(view.project(Vec3::zeros()).unwrap().1);
        let mut t = Transforming::drag_many(&doc, &[(card.clone(), Mat4::identity())], Handle::Arrow(0), Space::Global, tip, 1).unwrap();
        let to = at(&view, [3.0 + len, 0.0, 0.0]);
        assert_eq!(t.step(&input(to), &view, &mut doc), Outcome::Going);
        assert_eq!(t.step(&Input { confirm: true, ..input(to) }, &view, &mut doc), Outcome::Done);
        let moved = trs_at(&doc, &card, 1);
        assert!((moved.translate[0] - 3.0).abs() < 1e-6 && moved.translate[1] == 0.0 && moved.translate[2] == 0.0, "{moved:?}");
        // The Y cube, dragged to twice its distance from the center: scale.y × 2.
        let c = view.project(Vec3::new(3.0, 0.0, 0.0)).unwrap().0;
        let lay = Layout::new(&view, Tool::Scale, Vec3::new(3.0, 0.0, 0.0), axes).unwrap();
        let cube = lay.cubes[1].unwrap();
        let mut t = Transforming::drag_many(&doc, &[(card.clone(), moved.matrix())], Handle::Cube(1), Space::Global, cube, 1).unwrap();
        t.step(&input(c + (cube - c) * 2.0), &view, &mut doc);
        assert!(near(trs_at(&doc, &card, 1).scale, [1.0, 2.0, 1.0], 1e-4), "{:?}", trs_at(&doc, &card, 1).scale);
        // The Z ring: a rotation about Z only.
        let lay = Layout::new(&view, Tool::Rotate, Vec3::new(3.0, 0.0, 0.0), axes).unwrap();
        let mut t = Transforming::drag_many(&doc, &[(card.clone(), moved.matrix())], Handle::Ring(2), Space::Global, lay.rings[2][0].0, 1).unwrap();
        t.step(&input(lay.rings[2][8].0), &view, &mut doc);
        let r = trs_at(&doc, &card, 1).rotate;
        assert!(r[0] == 0.0 && r[1] == 0.0 && r[2].abs() > 1.0, "{r:?}");
        // A gizmo can't take a linked param (refused like G).
        doc.set_param(&card, "translate", arclight::link::expr("[1, 2, 3]"));
        assert!(Transforming::drag_many(&doc, &[(card.clone(), Mat4::identity())], Handle::Arrow(0), Space::Global, tip, 1).is_err());
    }
}
