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

use arclight::geom::{self, Mat4, Vec3};
use eframe::egui::Pos2;
use nalgebra::{Rotation3, Unit};

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

/// Snapping steps: 1 unit, 15°, 0.1 scale.
pub const SNAP_MOVE: f64 = 1.0;
pub const SNAP_ANGLE: f64 = 15.0;
pub const SNAP_SCALE: f64 = 0.1;
/// Trackball degrees per screen point.
const TRACKBALL_RATE: f64 = 0.5;

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
}

impl Gesture {
    /// Start `mode` on an object whose params are `start` and whose place
    /// in the scene is `world`, with the pointer at `from`.
    pub fn new(mode: Mode, start: Trs, world: &Mat4, from: Pos2) -> Gesture {
        let own = start.matrix();
        let parent = own.try_inverse().map_or_else(Mat4::identity, |inv| world * inv);
        Gesture { mode, start, parent, from, constraint: Constraint::Free, typed: String::new(), negate: false, turned: 0.0, last_angle: None }
    }

    /// A gizmo drag: like [`Gesture::new`], constrained from the start.
    pub fn with_constraint(mut self, c: Constraint) -> Gesture {
        self.constraint = c;
        self
    }

    /// The object's origin in the scene.
    pub fn origin(&self) -> Vec3 {
        geom::transform_point(&self.parent, &Vec3::from(self.start.translate))
    }

    /// The parent's rotation (its scale taken out).
    fn parent_rotation(&self) -> Rotation3<f64> {
        let m = self.parent.fixed_view::<3, 3>(0, 0).into_owned();
        let cols: Vec<Vec3> = (0..3).map(|i| m.column(i).into_owned().try_normalize(1e-12).unwrap_or_else(|| Vec3::ith(i, 1.0))).collect();
        Rotation3::from_matrix(&nalgebra::Matrix3::from_columns(&cols))
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

    /// Esc / right-click: the params as they were.
    pub fn cancel(&self) -> Trs {
        self.start
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
        let o = self.origin();
        let delta = match (self.typed(), self.constraint) {
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
        };
        // Into the params' space.
        let local = self.parent.fixed_view::<3, 3>(0, 0).try_inverse().map_or(delta, |inv| inv * delta);
        let mut t = self.start;
        for i in 0..3 {
            t.translate[i] += local[i];
        }
        t
    }

    fn rotated(&mut self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        let toward_viewer = -view.view_dir();
        if self.constraint == Constraint::Trackball && self.typed().is_none() {
            // Left / right turns about the view's up, up / down about its right.
            let d = mouse - self.from;
            let right = view.view_dir().cross(&Vec3::y()).try_normalize(1e-9).unwrap_or_else(Vec3::x);
            let up = right.cross(&view.view_dir());
            let q = Rotation3::from_axis_angle(&Unit::new_normalize(up), (d.x as f64 * TRACKBALL_RATE).to_radians())
                * Rotation3::from_axis_angle(&Unit::new_normalize(right), (d.y as f64 * TRACKBALL_RATE).to_radians());
            return self.rotated_by(q, snap);
        }
        let (axis, space) = match self.constraint {
            Constraint::Axis(k, s) | Constraint::Plane(k, s) => (self.axis(k, s), Some((k, s))),
            _ => (toward_viewer, None),
        };
        let angle = match self.typed() {
            Some(v) => v,
            None => {
                // The pointer's turn about the center, counter-clockwise on
                // screen; about an axis pointing away from the viewer that's
                // a clockwise turn.
                let c = view.project(self.origin()).map_or(self.from, |(c, _)| c);
                if let Some(a) = screen_angle(c, mouse) {
                    let last = *self.last_angle.get_or_insert(screen_angle(c, self.from).unwrap_or(a));
                    self.turned += wrap_degrees(a - last);
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
        if let Some(k) = exact {
            let mut t = self.start;
            t.rotate[k] += angle;
            return t;
        }
        let q = Rotation3::from_axis_angle(&Unit::new_normalize(axis), angle.to_radians());
        self.rotated_by(q, false)
    }

    /// The start rotation turned by `q` (scene space), as params nearest
    /// the start's.
    fn rotated_by(&self, q: Rotation3<f64>, snap: bool) -> Trs {
        let p = self.parent_rotation();
        let r = p.inverse() * q * p * geom::rotation(self.start.rotate);
        let mut t = self.start;
        t.rotate = nearest_angles(geom::angles(&r), self.start.rotate);
        if snap {
            t.rotate = t.rotate.map(|a| snapped(a, SNAP_ANGLE, true));
        }
        t
    }

    fn scaled(&self, view: &dyn View, mouse: Pos2, snap: bool) -> Trs {
        let factor = match self.typed() {
            Some(v) => v,
            None => {
                let c = view.project(self.origin()).map_or(self.from, |(c, _)| c);
                let r0 = (self.from - c).length() as f64;
                if r0 < 1.0 { 1.0 } else { snapped((mouse - c).length() as f64 / r0, SNAP_SCALE, snap) }
            }
        };
        // Scale is always along the object's own axes.
        let on = |i: usize| match self.constraint {
            Constraint::Axis(k, _) => i == k,
            Constraint::Plane(k, _) => i != k,
            _ => true,
        };
        let mut t = self.start;
        for i in (0..3).filter(|&i| on(i)) {
            t.scale[i] *= factor;
        }
        t
    }
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
        assert_eq!(g.cancel(), s, "cancel gives back the start");
        // Alt + G / R / S.
        let t = Trs { translate: [1.0; 3], rotate: [2.0; 3], scale: [3.0; 3] };
        assert_eq!(t.reset(Mode::Rotate), Trs { rotate: [0.0; 3], ..t });
        assert_eq!(t.reset(Mode::Scale).scale, [1.0; 3]);
    }
}
