//! The 3D viewport: the viewed node's scene from a free camera that belongs
//! to the viewer, with Blender-style navigation (the conventions only; no
//! Blender code), picking and selection shared with the node graph, and the
//! cards textured and meshes shaded by the CPU renderer on a worker thread
//! (meshes Solid, lit from the view; Lit, by the scene's lights and
//! materials, optionally with shadows; or drawn as wireframes). Lights show as
//! icons (a star for a point light, a cone for a spot, parallel arrows for
//! a directional light, the rectangle for an area light, a ring for
//! ambient light) and are picked, selected, framed and moved like cameras.
//! A selected spot light shows handles on its cone's edge (drag one to
//! change the cone angle) and a selected area light on its edges (drag
//! one to resize it about its middle).
//!
//! Navigation: middle-drag orbits (turntable: yaw about world Y, pitch
//! short of the poles), ⇧ + middle-drag pans, scroll or ⌃ + middle-drag
//! zooms toward the pointer; on a trackpad two-finger scroll orbits, ⇧ pans,
//! pinch or ⌃ zooms; with "emulate 3-button mouse", Alt + left-drag stands
//! in for the middle button. Preferences ▸ 3D view can make two-finger
//! scroll pan (⇧ orbits), zoom toward the middle, and turn the zoom around
//! (see [`Navigation`]). Keys (over the viewport, when no gizmo gesture
//! owns them): 1 / 3 / 7 front / right / top (⌃: the opposite side), 5
//! perspective / orthographic, 0 through the selected camera, spot or area
//! light (else the scene's first camera), or back out, 2 / 4 / 6 / 8
//! orbit 15°, . or F frame the selection, Home frame everything, A select
//! all, Alt+A or Esc deselect, Z solid / wireframe meshes, ⇧Z Solid / Lit
//! shading, ` a pie of
//! views. (egui doesn't tell the numpad from the
//! number row, so both work.) A navigation gizmo in the top-right corner
//! does the same with the mouse.
//!
//! Arclight's world is Y up: "top" looks down −Y, "front" down −Z.

use crate::doc::Doc;
use crate::worker::{SceneRequest, SceneWorker};
use arclight::geom::{self, Mat4, Vec3};
use arclight::particle_nodes::PartOf;
use arclight::particles::{Effect, Particle, Shape};
use arclight::image::{Image, Size};
use arclight::render3d::Preview;
use arclight::scene::Scene;
use arclight::shade::{Light, LightKind};
use eframe::egui::{self, Align2, Color32, FontId, PointerButton, Pos2, Rect, Response, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use nalgebra::Rotation3;
use std::sync::Arc;

/// The free camera's lens: 35 mm on a 36 mm film back (about 54° across).
const FOCAL: f64 = 35.0;
const FILM: f64 = 36.0;
/// Degrees of orbit per pixel dragged.
const ORBIT_SPEED: f64 = 0.35;
/// How long (seconds) after a wheel's line step fine scroll steps are
/// still taken as the wheel (see [`Navigation::scroll`]).
const WHEEL_SPELL: f64 = 1.0;
/// Pitch is kept this far short of straight up or down while orbiting.
const PITCH_LIMIT: f64 = 89.5;

/// The viewport's own camera: it circles `center` at `distance`, turned
/// `yaw` degrees about world Y and tilted `pitch` degrees (positive: from
/// above).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ViewCam {
    pub center: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub ortho: bool,
    /// Set by an axis view, which switches to orthographic; orbiting away
    /// goes back to perspective (Blender's "auto perspective").
    #[serde(default)]
    pub auto_ortho: bool,
    /// Meshes as wireframes, not solid.
    #[serde(default)]
    pub wireframe: bool,
    /// How solid meshes are shaded, once chosen (else Lit when the scene
    /// has lights, Solid when not: see [`Viewport3d::shading`]).
    #[serde(default)]
    pub shading: Option<Shading>,
    /// Lit shading shows the lights' shadows.
    #[serde(default)]
    pub shadows: bool,
}

/// How the viewport shades solid meshes (Blender's): Solid lit from the
/// view, quick to work in; Lit by the scene's lights and materials, as a
/// render is (from the view where the scene has no lights).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Shading {
    Solid,
    Lit,
}

impl Default for ViewCam {
    fn default() -> ViewCam {
        ViewCam { center: [0.0; 3], yaw: 30.0, pitch: 25.0, distance: 12.0, ortho: false, auto_ortho: false, wireframe: false, shading: None, shadows: false }
    }
}

/// Which way an axis view looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    Front,
    Back,
    Right,
    Left,
    Top,
    Bottom,
}

impl ViewCam {
    fn center(&self) -> Vec3 {
        Vec3::from(self.center)
    }

    fn rotation(&self) -> Rotation3<f64> {
        geom::rotation([-self.pitch, self.yaw, 0.0])
    }

    /// Where the camera is.
    pub fn eye(&self) -> Vec3 {
        self.center() + self.rotation() * Vec3::new(0.0, 0.0, self.distance)
    }

    /// Camera → world (looking down its −Z, Y up).
    pub fn world(&self) -> Mat4 {
        let mut w = self.rotation().to_homogeneous();
        let e = self.eye();
        w[(0, 3)] = e.x;
        w[(1, 3)] = e.y;
        w[(2, 3)] = e.z;
        w
    }

    /// How it projects into `rect`.
    pub fn projector(&self, rect: Rect) -> Projector {
        let world = self.world();
        let per_unit = rect.width() as f64 * FOCAL / FILM;
        // Orthographic matches the perspective's scale at the orbit center.
        let f = if self.ortho { per_unit / self.distance.max(1e-9) } else { per_unit };
        Projector { world, view: world.try_inverse().unwrap_or_else(Mat4::identity), f, c: rect.center(), ortho: self.ortho, near: (self.distance * 1e-3).max(1e-4), rect, frame: None }
    }

    /// Turn about the orbit center by a drag of (`dx`, `dy`) pixels.
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        self.yaw -= dx * ORBIT_SPEED;
        self.pitch = (self.pitch + dy * ORBIT_SPEED).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        self.yaw = self.yaw.rem_euclid(360.0);
        if self.auto_ortho {
            self.ortho = false;
            self.auto_ortho = false;
        }
    }

    /// Orbit as [`ViewCam::orbit`], but about scene point `pivot` (the
    /// selection): the camera swings round it rigidly, so what's selected
    /// stays where it is on screen instead of sweeping across it.
    pub fn orbit_about(&mut self, pivot: Vec3, dx: f64, dy: f64) {
        let before = self.rotation();
        self.orbit(dx, dy);
        let turn = self.rotation() * before.inverse();
        self.center = (pivot + turn * (self.center() - pivot)).into();
    }

    /// Slide the view by a drag of `d` pixels: what's at the orbit center
    /// follows the pointer.
    pub fn pan(&mut self, d: Vec2, p: &Projector) {
        let units = 1.0 / p.pixels_per_unit(self.distance);
        let r = self.rotation();
        let (right, up) = (r * Vec3::x(), r * Vec3::y());
        let c = self.center() - right * (d.x as f64 * units) + up * (d.y as f64 * units);
        self.center = c.into();
    }

    /// Zoom by `factor` (below 1: closer) toward screen point `s`: the point
    /// under it, at the orbit center's depth, stays under it.
    pub fn zoom_at(&mut self, factor: f64, s: Pos2, p: &Projector) {
        let factor = factor.clamp(0.01, 100.0);
        let (o, d) = p.ray(s);
        let forward = self.rotation() * -Vec3::z();
        let along = if p.ortho { self.distance } else { self.distance / d.dot(&forward).max(1e-9) };
        let w = o + d * along;
        let c = w + (self.center() - w) * factor;
        self.center = c.into();
        self.distance = (self.distance * factor).clamp(1e-4, 1e7);
    }

    /// Look along an axis, orthographic (until orbited away from).
    pub fn view_axis(&mut self, a: Axis) {
        (self.yaw, self.pitch) = match a {
            Axis::Front => (0.0, 0.0),
            Axis::Back => (180.0, 0.0),
            Axis::Right => (90.0, 0.0),
            Axis::Left => (270.0, 0.0),
            Axis::Top => (0.0, 90.0),
            Axis::Bottom => (0.0, -90.0),
        };
        self.ortho = true;
        self.auto_ortho = true;
    }

    /// Fit a sphere (`center`, `radius`) in a view `aspect` wide per unit
    /// high (the narrower of its two angles of view decides).
    pub fn frame(&mut self, center: Vec3, radius: f64, aspect: f64) {
        let across = FILM / 2.0 / FOCAL;
        let half = across.min(across / aspect.max(1e-3)).atan();
        self.center = center.into();
        self.distance = (radius.max(1e-3) / half.sin() * 1.1).max(0.01);
    }
}

/// How the current view maps the world to the screen and back (perspective,
/// orthographic, or through a scene camera), for drawing, picking and
/// gizmos. Screen positions are egui points.
#[derive(Clone, Copy, Debug)]
pub struct Projector {
    world: Mat4,
    view: Mat4,
    /// Screen points per unit: at depth 1 (perspective) or anywhere (ortho).
    f: f64,
    c: Pos2,
    pub ortho: bool,
    near: f64,
    pub rect: Rect,
    /// Through a scene camera: its film-back frame on screen.
    pub frame: Option<Rect>,
}

impl Projector {
    /// Through scene camera `cam` rendering frames of `format`: exactly
    /// what Scanline Render draws, its frame fitted in `rect`.
    pub fn through(cam: &geom::Camera, format: Size, rect: Rect) -> Projector {
        let aspect = format.width.max(1) as f32 / format.height.max(1) as f32;
        let room = rect.shrink(rect.width().min(rect.height()) * 0.04);
        let (w, h) = if room.width() / room.height() > aspect { (room.height() * aspect, room.height()) } else { (room.width(), room.width() / aspect) };
        let frame = Rect::from_center_size(room.center(), Vec2::new(w, h));
        let f = if cam.is_ortho() { frame.width() as f64 / cam.ortho } else { cam.focal / cam.film_back[0].max(1e-9) * frame.width() as f64 };
        Projector { world: cam.world, view: cam.view(), f, c: frame.center(), ortho: cam.is_ortho(), near: cam.near, rect, frame: Some(frame) }
    }

    /// `p` in camera space (looking down −Z).
    fn camera_space(&self, p: &Vec3) -> Vec3 {
        geom::transform_point(&self.view, p)
    }

    fn screen(&self, q: &Vec3) -> Pos2 {
        let s = if self.ortho { self.f } else { self.f / -q.z };
        Pos2::new((self.c.x as f64 + q.x * s) as f32, (self.c.y as f64 - q.y * s) as f32)
    }

    /// Whether what's nearer than `near` is cut: in perspective, and
    /// through a scene camera (as it renders); the free orthographic view
    /// sees everything.
    fn clips(&self) -> bool {
        !self.ortho || self.frame.is_some()
    }

    /// Where `p` lands on screen and how far in front of the camera it is;
    /// None behind (or too near: see [`clips`](Self::clips)).
    pub fn project(&self, p: Vec3) -> Option<(Pos2, f64)> {
        let q = self.camera_space(&p);
        let depth = -q.z;
        (!self.clips() || depth >= self.near).then(|| (self.screen(&q), depth))
    }

    /// The ray through screen point `s`: origin and unit direction.
    pub fn ray(&self, s: Pos2) -> (Vec3, Vec3) {
        let (x, y) = ((s.x - self.c.x) as f64 / self.f, -(s.y - self.c.y) as f64 / self.f);
        let r = self.world.fixed_view::<3, 3>(0, 0).into_owned();
        let eye = Vec3::new(self.world[(0, 3)], self.world[(1, 3)], self.world[(2, 3)]);
        if self.ortho {
            (eye + r * Vec3::new(x, y, 0.0), (r * -Vec3::z()).normalize())
        } else {
            (eye, (r * Vec3::new(x, y, -1.0)).normalize())
        }
    }

    /// Screen points per world unit at `depth`.
    pub fn pixels_per_unit(&self, depth: f64) -> f64 {
        if self.ortho { self.f } else { self.f / depth.max(1e-9) }
    }

    /// The way the camera looks.
    #[allow(dead_code)] // For the transform gizmos (gizmo3d.rs).
    pub fn view_dir(&self) -> Vec3 {
        (self.world.fixed_view::<3, 3>(0, 0) * -Vec3::z()).normalize()
    }

    /// The segment from `a` to `b` on screen, cut where it passes behind
    /// the near plane.
    pub fn segment(&self, a: Vec3, b: Vec3) -> Option<(Pos2, Pos2)> {
        let (mut qa, mut qb) = (self.camera_space(&a), self.camera_space(&b));
        if self.clips() {
            let z = -self.near;
            match (qa.z <= z, qb.z <= z) {
                (false, false) => return None,
                (true, false) => qb = qa + (qb - qa) * ((z - qa.z) / (qb.z - qa.z)),
                (false, true) => qa = qa + (qb - qa) * ((z - qa.z) / (qb.z - qa.z)),
                _ => {}
            }
        }
        Some((self.screen(&qa), self.screen(&qb)))
    }
}

/// The scene's extent: a center and radius holding its cameras, cards,
/// meshes, lights, points and splats (None when it's empty).
fn bounds(scene: &Scene, only: Option<&dyn Fn(&str) -> bool>) -> Option<(Vec3, f64)> {
    let keep = |n: &str| only.is_none_or(|f| f(n));
    let mut pts: Vec<Vec3> = Vec::new();
    for (i, c) in scene.cameras.iter().enumerate() {
        if keep(scene.camera_node(i)) {
            pts.push(geom::transform_point(&c.world, &Vec3::zeros()));
        }
    }
    for c in scene.cards.iter().filter(|c| keep(&c.node)) {
        pts.extend(c.corners());
    }
    for (lo, hi) in scene.meshes.iter().filter(|m| keep(&m.node)).filter_map(|m| m.bounds()) {
        pts.extend([lo, hi]);
    }
    for l in scene.lights.iter().filter(|l| keep(&l.node)) {
        pts.push(l.position());
    }
    for (i, p) in scene.points.iter().enumerate() {
        if keep(scene.point_node(i)) {
            pts.push(*p);
        }
    }
    for s in scene.particles.iter().filter(|s| keep(&s.node)) {
        pts.extend(s.particles().iter().map(|p| p.position));
    }
    for g in gadgets(scene).iter().filter(|g| keep(&g.node)) {
        pts.push(geom::transform_point(&g.world, &Vec3::zeros()));
    }
    for (lo, hi) in scene.splats.iter().filter(|s| keep(&s.node)).filter_map(|s| s.bounds()) {
        pts.extend([lo, hi]);
    }
    if pts.is_empty() {
        return None;
    }
    let (lo, hi) = pts.iter().fold((pts[0], pts[0]), |(lo, hi), p| (lo.inf(p), hi.sup(p)));
    let c = (lo + hi) / 2.0;
    Some((c, pts.iter().map(|p| (p - c).norm()).fold(0.0, f64::max).max(0.5)))
}

/// A camera's frustum, `size` across: its corners at the camera (all its
/// apex for a pinhole; a frame `size` across, orthographic) and at a depth
/// showing its focal length (`size` on for orthographic).
fn frustum(cam: &geom::Camera, size: f64, aspect: f64) -> ([Vec3; 4], [Vec3; 4]) {
    let depth = if cam.is_ortho() { size } else { size * cam.focal / cam.film_back[0].max(1e-9) };
    let (w, h) = (size / 2.0, size / 2.0 / aspect.max(1e-3));
    let corners = |z: f64, k: f64| [(-w, h), (w, h), (w, -h), (-w, -h)].map(|(x, y)| geom::transform_point(&cam.world, &Vec3::new(x * k, y * k, z)));
    (corners(0.0, if cam.is_ortho() { 1.0 } else { 0.0 }), corners(-depth, 1.0))
}

/// A camera's icon as line segments: its frustum `size` across (a box for
/// an orthographic one) and a notch on the frame's top edge for up.
fn camera_lines(cam: &geom::Camera, size: f64, aspect: f64) -> Vec<(Vec3, Vec3)> {
    let (back, k) = frustum(cam, size, aspect);
    let mut lines = Vec::new();
    for j in 0..4 {
        lines.push((back[j], k[j]));
        lines.push((k[j], k[(j + 1) % 4]));
        if cam.is_ortho() {
            lines.push((back[j], back[(j + 1) % 4]));
        }
    }
    let up = (k[0] + k[1]) / 2.0 + (k[0] - k[3]) * 0.2;
    lines.push((k[0], up));
    lines.push((up, k[1]));
    lines
}

/// Each named card, camera, light and particle gadget in `scene` with the
/// lines it's drawn as (`size`: the icons' size; `aspect`: the cameras'
/// frames'), which a click near picks it by.
fn object_lines(scene: &Scene, size: f64, aspect: f64) -> Vec<(String, Vec<(Vec3, Vec3)>)> {
    let mut out = Vec::new();
    for c in scene.cards.iter().filter(|c| !c.node.is_empty()) {
        let k = c.corners();
        out.push((c.node.clone(), (0..4).map(|i| (k[i], k[(i + 1) % 4])).collect()));
    }
    for (i, cam) in scene.cameras.iter().enumerate() {
        let node = scene.camera_node(i);
        if !node.is_empty() {
            out.push((node.to_string(), camera_lines(cam, size, aspect)));
        }
    }
    for l in scene.lights.iter().filter(|l| !l.node.is_empty()) {
        out.push((l.node.clone(), light_lines(l, size)));
    }
    for g in gadgets(scene) {
        let lines = gadget_lines(&g, size);
        out.push((g.node, lines));
    }
    out
}

/// The edges of the box round a model, in the scene.
fn box_lines(m: &arclight::scene::Model) -> Vec<(Vec3, Vec3)> {
    let Some((lo, hi)) = m.mesh.bounds() else { return Vec::new() };
    let c = |i: usize| geom::transform_point(&m.world, &Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][(i >> 2) & 1]));
    BOX_EDGES.iter().map(|&(a, b)| (c(a), c(b))).collect()
}

/// How far screen point `s` is from segment `a`–`b`, and how far along it
/// (0 to 1) the nearest point is.
fn segment_distance(s: Pos2, a: Pos2, b: Pos2) -> (f32, f32) {
    let ab = b - a;
    let t = if ab.length_sq() < 1e-6 { 0.0 } else { ((s - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) };
    (s.distance(a + ab * t), t)
}

/// A light's icon as line segments in the scene, `size` across (none for
/// ambient light, drawn as a ring instead):
/// - point: a star of eight rays round it (and a ring its radius across
///   if it has one);
/// - spot: a cone `2 size` long down its −Z, a ring at the cone's edge, a
///   second inside where the penumbra starts, and four sides;
/// - directional: three parallel arrows down its −Z;
/// - area: its rectangle or disk and an arrow out of its middle the way it
///   shines (both ways if two-sided).
fn light_lines(l: &Light, size: f64) -> Vec<(Vec3, Vec3)> {
    let at = l.position();
    let dir = l.direction();
    // Two directions across the light's axis (its own X and Y).
    let (u, v) = (l.axis(0), l.axis(1));
    let ring = |center: Vec3, r: f64| -> Vec<(Vec3, Vec3)> {
        let pt = |i: usize| {
            let a = std::f64::consts::TAU * i as f64 / 24.0;
            center + (u * a.cos() + v * a.sin()) * r
        };
        (0..24).map(|i| (pt(i), pt(i + 1))).collect()
    };
    let arrow = |from: Vec3, to: Vec3| -> Vec<(Vec3, Vec3)> {
        let back = (from - to) * 0.25;
        vec![(from, to), (to, to + back + u * back.norm() * 0.5), (to, to + back - u * back.norm() * 0.5)]
    };
    match l.kind {
        LightKind::Ambient => Vec::new(),
        LightKind::Point => {
            let r = size * 0.5;
            let d = std::f64::consts::FRAC_1_SQRT_2;
            let mut out: Vec<(Vec3, Vec3)> = [u, v, dir, (u + v) * d, (u - v) * d, (u + dir) * d, (v + dir) * d, (u - dir) * d].iter().map(|a| (at - a * r, at + a * r)).collect();
            if l.radius > 0.0 {
                out.extend(ring(at, l.radius));
            }
            out
        }
        LightKind::Spot { cone, penumbra, .. } => {
            let len = size * 2.0;
            let end = at + dir * len;
            let half = (cone / 2.0).clamp(0.0, 89.0).to_radians();
            let inner = (cone / 2.0 - penumbra.max(0.0)).clamp(0.0, 89.0).to_radians();
            let (r, ri) = (len * half.tan(), len * inner.tan());
            let mut out = ring(end, r);
            if penumbra > 0.0 && ri > 0.0 {
                out.extend(ring(end, ri));
            }
            out.extend([u, v, -u, -v].iter().map(|a| (at, end + a * r)));
            out
        }
        LightKind::Directional => {
            let len = size * 1.5;
            let mut out = Vec::new();
            for off in [Vec3::zeros(), u * size * 0.4, -u * size * 0.4] {
                out.extend(arrow(at + off, at + off + dir * len));
            }
            out
        }
        LightKind::Area { size: wh, disk, two_sided, .. } => {
            let c = |x: f64, y: f64| geom::transform_point(&l.world, &Vec3::new(x * wh[0] / 2.0, y * wh[1] / 2.0, 0.0));
            let outline: Vec<Vec3> = if disk {
                (0..24).map(|i| std::f64::consts::TAU * i as f64 / 24.0).map(|a| c(a.cos(), a.sin())).collect()
            } else {
                vec![c(-1.0, 1.0), c(1.0, 1.0), c(1.0, -1.0), c(-1.0, -1.0)]
            };
            let mut out: Vec<(Vec3, Vec3)> = (0..outline.len()).map(|i| (outline[i], outline[(i + 1) % outline.len()])).collect();
            out.extend(arrow(at, at + dir * size));
            if two_sided {
                out.extend(arrow(at, at - dir * size));
            }
            out
        }
    }
}

/// A particle system's emitter or modifier shown in the scene, picked and
/// moved like other objects through its node.
#[derive(Clone, Debug)]
pub struct Gadget {
    pub node: String,
    /// Its place: an emitter's transform, a modifier's point.
    pub world: Mat4,
    pub kind: GadgetKind,
}

#[derive(Clone, Debug)]
pub enum GadgetKind {
    /// Its shape, and which way particles leave (in the scene).
    Emitter(Shape, Vec3),
    Attractor { radius: f64 },
    Vortex { axis: Vec3, radius: f64 },
    Bounce { normal: Vec3 },
    /// A Splat Crop's region: the box or ball (radius ½) of side 1 its
    /// world places (Center, Rotate, Size).
    Crop { sphere: bool },
}

/// The emitters, attractors, vortices and bounce planes of `scene`'s
/// particle streams, as their systems are at the streams' frames, and its
/// Splat Crops' regions (each node once).
pub fn gadgets(scene: &Scene) -> Vec<Gadget> {
    let mut out: Vec<Gadget> = Vec::new();
    for stream in &scene.particles {
        let system = (stream.system)(stream.frame);
        for part in &stream.parts {
            if out.iter().any(|g| g.node == part.node) {
                continue;
            }
            let (world, kind) = match part.what {
                PartOf::Emitter(i) => {
                    let Some(e) = system.emitters.get(i) else { continue };
                    let dir = (e.transform.fixed_view::<3, 3>(0, 0) * e.direction).try_normalize(1e-12).unwrap_or_default();
                    (e.transform, GadgetKind::Emitter(e.shape.clone(), dir))
                }
                PartOf::Modifier(i) => match system.modifiers.get(i).map(|m| &m.effect) {
                    Some(Effect::Attractor { point, radius, .. }) => (Mat4::new_translation(point), GadgetKind::Attractor { radius: *radius }),
                    Some(Effect::Vortex { center, axis, radius, .. }) => (Mat4::new_translation(center), GadgetKind::Vortex { axis: axis.try_normalize(1e-12).unwrap_or_else(Vec3::y), radius: *radius }),
                    Some(Effect::Bounce { point, normal, .. }) => (Mat4::new_translation(point), GadgetKind::Bounce { normal: normal.try_normalize(1e-12).unwrap_or_else(Vec3::y) }),
                    _ => continue,
                },
            };
            out.push(Gadget { node: part.node.clone(), world, kind });
        }
    }
    for r in &scene.regions {
        if !out.iter().any(|g| g.node == r.node) {
            // (A region's world takes the cube of side 2 onto it; the
            // gadget's, Center / Rotate / Size as the gizmo sets them.)
            out.push(Gadget { node: r.node.clone(), world: r.world * Mat4::new_scaling(2.0), kind: GadgetKind::Crop { sphere: r.sphere } });
        }
    }
    out
}

/// Two unit directions across unit `n`.
fn across(n: Vec3) -> (Vec3, Vec3) {
    let u = n.cross(&if n.x.abs() < 0.9 { Vec3::x() } else { Vec3::y() }).normalize();
    (u, n.cross(&u))
}

/// A ring of radius `r` about `center` in the plane of `u` and `v`.
fn ring_lines(center: Vec3, u: Vec3, v: Vec3, r: f64) -> Vec<(Vec3, Vec3)> {
    let pt = |i: usize| {
        let a = std::f64::consts::TAU * i as f64 / 32.0;
        center + (u * a.cos() + v * a.sin()) * r
    };
    (0..32).map(|i| (pt(i), pt(i + 1))).collect()
}

/// An arrow from `from` to `to`.
fn arrow_lines(from: Vec3, to: Vec3) -> Vec<(Vec3, Vec3)> {
    let Some(d) = (to - from).try_normalize(1e-12) else { return Vec::new() };
    let (u, _) = across(d);
    let back = (from - to) * 0.25;
    let side = u * back.norm() * 0.5;
    vec![(from, to), (to, to + back + side), (to, to + back - side)]
}

/// A gadget's icon as line segments in the scene (`size` across for the
/// parts without a size of their own):
/// - an emitter: its shape (a point's small cross, a line, a rectangle, a
///   disk, a sphere's three rings, a box; a cross for geometry and masks)
///   and an arrow the way particles leave;
/// - an attractor: a six-rayed star, and a ring at its reach;
/// - a vortex: a ring around its axis and an arrow along it;
/// - a bounce plane: a square in the plane and an arrow out of it.
pub fn gadget_lines(g: &Gadget, size: f64) -> Vec<(Vec3, Vec3)> {
    let at = geom::transform_point(&g.world, &Vec3::zeros());
    let local = |x: f64, y: f64, z: f64| geom::transform_point(&g.world, &Vec3::new(x, y, z));
    let cross = |r: f64| -> Vec<(Vec3, Vec3)> { [Vec3::x(), Vec3::y(), Vec3::z()].iter().map(|a| (at - a * r, at + a * r)).collect() };
    match &g.kind {
        GadgetKind::Emitter(shape, dir) => {
            let mut out = match shape {
                Shape::Line { length } => vec![(local(-length / 2.0, 0.0, 0.0), local(length / 2.0, 0.0, 0.0))],
                Shape::Rect { width, height } => {
                    let c = [local(-width / 2.0, height / 2.0, 0.0), local(width / 2.0, height / 2.0, 0.0), local(width / 2.0, -height / 2.0, 0.0), local(-width / 2.0, -height / 2.0, 0.0)];
                    (0..4).map(|i| (c[i], c[(i + 1) % 4])).collect()
                }
                Shape::Disk { radius } => ring_local(&g.world, [0, 1], *radius),
                Shape::Sphere { radius, .. } => [[0, 1], [1, 2], [0, 2]].into_iter().flat_map(|a| ring_local(&g.world, a, *radius)).collect(),
                Shape::Box { size: b, .. } => {
                    let corner = |i: usize| local(if i & 1 == 0 { -b.x } else { b.x } / 2.0, if i & 2 == 0 { -b.y } else { b.y } / 2.0, if i & 4 == 0 { -b.z } else { b.z } / 2.0);
                    (0..8).flat_map(|i| [1, 2, 4].into_iter().filter(move |bit| i & bit == 0).map(move |bit| (i, i | bit))).map(|(i, j)| (corner(i), corner(j))).collect()
                }
                _ => cross(size * 0.3),
            };
            if *dir != Vec3::zeros() {
                out.extend(arrow_lines(at, at + dir * size));
            }
            out
        }
        GadgetKind::Attractor { radius } => {
            let r = size * 0.4;
            let d = std::f64::consts::FRAC_1_SQRT_2;
            let mut out: Vec<(Vec3, Vec3)> = [Vec3::x(), Vec3::y(), Vec3::z(), Vec3::new(d, d, 0.0), Vec3::new(d, -d, 0.0), Vec3::new(0.0, d, d)].iter().map(|a| (at - a * r, at + a * r)).collect();
            if *radius > 0.0 {
                out.extend(ring_lines(at, Vec3::x(), Vec3::z(), *radius));
            }
            out
        }
        GadgetKind::Vortex { axis, radius } => {
            let (u, v) = across(*axis);
            let mut out = ring_lines(at, u, v, if *radius > 0.0 { *radius } else { size * 0.6 });
            out.extend(arrow_lines(at - axis * size * 0.5, at + axis * size));
            out
        }
        GadgetKind::Bounce { normal } => {
            let (u, v) = across(*normal);
            let c = [at + (u + v) * size, at + (u - v) * size, at - (u + v) * size, at - (u - v) * size];
            let mut out: Vec<(Vec3, Vec3)> = (0..4).map(|i| (c[i], c[(i + 1) % 4])).collect();
            out.extend(arrow_lines(at, at + normal * size));
            out
        }
        GadgetKind::Crop { sphere: true } => [[0, 1], [1, 2], [0, 2]].into_iter().flat_map(|a| ring_local(&g.world, a, 0.5)).collect(),
        GadgetKind::Crop { sphere: false } => {
            let corner = |i: usize| local([-0.5, 0.5][i & 1], [-0.5, 0.5][(i >> 1) & 1], [-0.5, 0.5][(i >> 2) & 1]);
            BOX_EDGES.iter().map(|&(a, b)| (corner(a), corner(b))).collect()
        }
    }
}

/// A ring of radius `r` in the plane of local axes `axes` of `world`.
fn ring_local(world: &Mat4, axes: [usize; 2], r: f64) -> Vec<(Vec3, Vec3)> {
    let pt = |i: usize| {
        let a = std::f64::consts::TAU * i as f64 / 32.0;
        let mut p = Vec3::zeros();
        p[axes[0]] = a.cos() * r;
        p[axes[1]] = a.sin() * r;
        geom::transform_point(world, &p)
    };
    (0..32).map(|i| (pt(i), pt(i + 1))).collect()
}

/// What a click picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// A card, mesh, camera, light, locator or splat set: its node.
    Object(String),
    /// Point `.1` of the point cloud (or Camera Tracker) node `.0`.
    Point(String, usize),
}

/// How near (screen points) a click must be to a point, locator or camera.
const PICK_RADIUS: f32 = 7.0;
/// How near a click must be to a thin thing's lines: a card's edge (seen
/// edge-on it's only a line), a camera's frustum, a light's icon.
const LINE_PICK: f32 = 8.0;
/// How near a click must be to a selected light's handle (a 7-point
/// square) to drag it.
const HANDLE_PICK: f32 = 10.0;
/// How near the last click a click must be to pick the next thing under
/// it.
const CYCLE_REACH: f32 = 4.0;

/// The project's frame aspect (the cameras' frames are drawn so).
fn project_aspect(doc: &Doc) -> f64 {
    doc.graph.project.format.width as f64 / doc.graph.project.format.height.max(1) as f64
}

/// What's under screen point `s` in `scene` seen through `p` (`aspect`:
/// the cameras' frames', as drawn), the first of [`pick_all`].
pub fn pick(scene: &Scene, p: &Projector, aspect: f64, s: Pos2, is_locator: &dyn Fn(&str) -> bool) -> Option<Hit> {
    pick_all(scene, p, aspect, s, is_locator).into_iter().next()
}

/// Everything under screen point `s` in `scene` seen through `p`, what a
/// click picks first: the points, locators, cameras, lights and splat
/// centers within reach, nearest first; then, nearest the eye first, the
/// cards and meshes the ray through `s` hits and the cards, cameras,
/// lights and gadgets whose drawn lines pass within reach. Repeated clicks
/// go down the list. `is_locator` tells a locator's point from a cloud's.
pub fn pick_all(scene: &Scene, p: &Projector, aspect: f64, s: Pos2, is_locator: &dyn Fn(&str) -> bool) -> Vec<Hit> {
    let mut near: Vec<(f32, f64, Hit)> = Vec::new();
    let mut consider = |at: Vec3, hit: Hit| {
        if let Some((q, depth)) = p.project(at) {
            let d = q.distance(s);
            if d <= PICK_RADIUS {
                near.push((d, depth, hit));
            }
        }
    };
    for (i, pt) in scene.points.iter().enumerate() {
        let node = scene.point_node(i);
        consider(*pt, if is_locator(node) { Hit::Object(node.to_string()) } else { Hit::Point(node.to_string(), i) });
    }
    for (i, cam) in scene.cameras.iter().enumerate() {
        consider(geom::transform_point(&cam.world, &Vec3::zeros()), Hit::Object(scene.camera_node(i).to_string()));
    }
    for l in scene.lights.iter().filter(|l| !l.node.is_empty()) {
        consider(l.position(), Hit::Object(l.node.clone()));
    }
    for g in gadgets(scene) {
        consider(geom::transform_point(&g.world, &Vec3::zeros()), Hit::Object(g.node));
    }
    // Splats: the nearest center (only the set's node is picked).
    for sp in scene.splats.iter().filter(|sp| !sp.node.is_empty()) {
        if let Some((d, depth)) = nearest_splat(sp, p, s) {
            if d <= PICK_RADIUS {
                near.push((d, depth, Hit::Object(sp.node.clone())));
            }
        }
    }
    near.sort_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal));
    // Thin things by their lines, as far along the ray as where they pass
    // nearest it (as cards and meshes are below).
    let (o, d) = p.ray(s);
    let mut under: Vec<(f64, Hit)> = Vec::new();
    for (node, lines) in object_lines(scene, icon_size(scene), aspect) {
        let nearest = lines
            .iter()
            .filter_map(|&(a, b)| {
                let (sa, sb) = p.segment(a, b)?;
                let (off, t) = segment_distance(s, sa, sb);
                // (Clipped to the view, the screen segment may be only part
                // of the line, and t on screen isn't t in the scene: near
                // enough for which is in front.)
                (off <= LINE_PICK).then(|| (a + (b - a) * t as f64 - o).dot(&d))
            })
            .min_by(f64::total_cmp);
        if let Some(depth) = nearest {
            under.push((depth, Hit::Object(node)));
        }
    }
    // Cards (where the ray hits inside its rectangle) and meshes.
    let meshes = scene.meshes.iter().filter_map(|m| Some((ray_mesh(m, &o, &d)?, m.node.clone())));
    scene
        .cards
        .iter()
        .filter_map(|c| {
            let inv = c.world.try_inverse()?;
            let (lo, ld) = (geom::transform_point(&inv, &o), inv.fixed_view::<3, 3>(0, 0) * d);
            if ld.z.abs() < 1e-12 {
                return None;
            }
            let t = -lo.z / ld.z;
            let hit = lo + ld * t;
            let inside = hit.x.abs() <= c.size[0] / 2.0 && hit.y.abs() <= c.size[1] / 2.0;
            // Distance along the world ray, for "nearest".
            let world_t = (geom::transform_point(&c.world, &hit) - o).dot(&d);
            (t > 0.0 && inside && world_t > 0.0).then_some((world_t, c.node.clone()))
        })
        .chain(meshes)
        .for_each(|(t, n)| under.push((t, Hit::Object(n))));
    under.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut all: Vec<Hit> = Vec::new();
    for hit in near.into_iter().map(|n| n.2).chain(under.into_iter().map(|u| u.1)) {
        if !all.contains(&hit) {
            all.push(hit);
        }
    }
    all
}

/// The nearest of splat set `s`'s centers to screen point `at` seen
/// through `p`: how far from it on screen, and its depth.
fn nearest_splat(s: &arclight::splat::Splats, p: &Projector, at: Pos2) -> Option<(f32, f64)> {
    s.positions
        .iter()
        .filter_map(|c| {
            let (q, depth) = p.project(geom::transform_point(&s.world, &Vec3::from(c.map(f64::from))))?;
            Some((q.distance(at), depth))
        })
        .min_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal))
}

/// How far along ray (`o`, unit `d`) it first meets model `m`'s triangles
/// (Möller–Trumbore, in the scene), if it does.
fn ray_mesh(m: &arclight::scene::Model, o: &Vec3, d: &Vec3) -> Option<f64> {
    let (lo, hi) = m.bounds()?;
    // Past the box around it: no need to try each triangle.
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for k in 0..3 {
        if d[k].abs() < 1e-12 {
            if o[k] < lo[k] || o[k] > hi[k] {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo[k] - o[k]) / d[k], (hi[k] - o[k]) / d[k]);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    if t0 > t1 {
        return None;
    }
    let world: Vec<Vec3> = m.mesh.positions.iter().map(|p| geom::transform_point(&m.world, p)).collect();
    m.mesh
        .triangles
        .iter()
        .filter_map(|t| {
            let idx = t.map(|i| i as usize);
            if idx.iter().any(|&i| i >= world.len()) {
                return None;
            }
            let [a, b, c] = idx.map(|i| world[i]);
            let (e1, e2) = (b - a, c - a);
            let h = d.cross(&e2);
            let det = e1.dot(&h);
            if det.abs() < 1e-12 {
                return None;
            }
            let sv = o - a;
            let u = sv.dot(&h) / det;
            let q = sv.cross(&e1);
            let v = d.dot(&q) / det;
            let t = e2.dot(&q) / det;
            (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0).then_some(t)
        })
        .min_by(f64::total_cmp)
}

/// Mesh `m`'s edges, each once (by position, so a seam's or a hard edge's
/// split vertices don't draw it twice).
fn mesh_edges(m: &arclight::mesh::Mesh) -> Vec<[u32; 2]> {
    let key = |i: u32| m.positions.get(i as usize).map(|p| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in &m.triangles {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (Some(ka), Some(kb)) = (key(a), key(b)) else { continue };
            if seen.insert(if ka <= kb { (ka, kb) } else { (kb, ka) }) {
                out.push([a, b]);
            }
        }
    }
    out
}

/// What a box from `a` to `b` (screen) holds: objects whose centers fall
/// inside (cards, meshes, cameras, lights, locators; a splat set's middle)
/// and points.
pub fn box_pick(scene: &Scene, p: &Projector, a: Pos2, b: Pos2, is_locator: &dyn Fn(&str) -> bool) -> (Vec<String>, Vec<(String, usize)>) {
    let r = Rect::from_two_pos(a, b);
    let inside = |x: Vec3| p.project(x).is_some_and(|(q, _)| r.contains(q));
    let mut objects: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        if !n.is_empty() && !objects.iter().any(|o| o == n) {
            objects.push(n.to_string());
        }
    };
    for c in scene.cards.iter().filter(|c| inside(geom::transform_point(&c.world, &Vec3::zeros()))) {
        add(&c.node);
    }
    for m in scene.meshes.iter().filter(|m| inside(geom::transform_point(&m.world, &Vec3::zeros()))) {
        add(&m.node);
    }
    for (i, cam) in scene.cameras.iter().enumerate() {
        if inside(geom::transform_point(&cam.world, &Vec3::zeros())) {
            add(scene.camera_node(i));
        }
    }
    for l in scene.lights.iter().filter(|l| inside(l.position())) {
        add(&l.node);
    }
    for g in gadgets(scene).iter().filter(|g| inside(geom::transform_point(&g.world, &Vec3::zeros()))) {
        add(&g.node);
    }
    for s in scene.splats.iter().filter(|s| s.bounds().is_some_and(|(lo, hi)| inside((lo + hi) / 2.0))) {
        add(&s.node);
    }
    let mut points = Vec::new();
    for (i, pt) in scene.points.iter().enumerate() {
        let node = scene.point_node(i);
        if inside(*pt) {
            if is_locator(node) {
                add(node);
            } else {
                points.push((node.to_string(), i));
            }
        }
    }
    (objects, points)
}

/// The backtick pie's slots, clockwise from the right (as Blender's view
/// pie: left / right across, top / bottom up and down, the rest between).
const PIE: [&str; 8] = ["Right", "Back", "Bottom", "Camera", "Left", "Front", "Top", "Persp / Ortho"];

/// Which pie slot screen point `at` points to from the pie's `center`
/// (None within its dead zone).
pub fn pie_slot(center: Pos2, at: Pos2) -> Option<usize> {
    let d = at - center;
    if d.length() < 20.0 {
        return None;
    }
    // Angle clockwise from the right (screen y is down), in eighths.
    let a = d.y.atan2(d.x).to_degrees().rem_euclid(360.0);
    Some(((a + 22.5) / 45.0) as usize % 8)
}

/// A camera node's path at a graph revision: positions by frame.
type CameraPath = (String, u64, Vec<(i64, Vec3)>);

/// What the viewport last asked its worker for: the node, frame and graph
/// revision, and the view and size the cards were rendered for.
#[derive(Clone, PartialEq)]
struct Ask {
    node: String,
    frame: i64,
    revision: u64,
    view: ViewCam,
    look_through: Option<Through>,
    /// The camera looked through as it was (it moves with its node).
    through: Option<geom::Camera>,
    pixels: [usize; 2],
    preview: Preview,
    /// The selection (shown as ghosts where it isn't in the scene).
    ghosts: Vec<String>,
}

/// What the view looks through instead of the free camera.
#[derive(Clone, Debug, PartialEq)]
pub enum Through {
    /// Scene camera `i`.
    Camera(usize),
    /// The spot or area light of this node (see [`light_camera`]).
    Light(String),
}

impl Through {
    /// Its camera in `scene` (a light's for frames of `format`), if it's
    /// there.
    pub fn camera_in(&self, scene: &Scene, format: Size) -> Option<geom::Camera> {
        match self {
            Through::Camera(i) => scene.cameras.get(*i).cloned(),
            Through::Light(n) => light_camera(scene.lights.iter().find(|l| &l.node == n)?, format),
        }
    }
}

/// A camera at a spot or area light, looking the way it shines (for aiming
/// it), for frames of `format`: a spot's cone just fits the frame's height;
/// an area light sees 90° high. None for other lights.
pub fn light_camera(l: &Light, format: Size) -> Option<geom::Camera> {
    let high = match l.kind {
        LightKind::Spot { cone, .. } => cone.clamp(1.0, 170.0),
        LightKind::Area { .. } => 90.0,
        _ => return None,
    };
    let aspect = format.width.max(1) as f64 / format.height.max(1) as f64;
    let across = (high / 2.0).to_radians().tan() * aspect;
    Some(geom::Camera { focal: FILM / (2.0 * across), film_back: [FILM, FILM / aspect], world: l.world, near: 0.01, far: 1e5, ortho: 0.0 })
}

/// A light handle: a dot on a spot's cone edge (at its X, Y, −X, −Y), or
/// the middle of an area light's edge (+X, −X, +Y, −Y).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightHandle {
    Cone(usize),
    Edge(usize),
}

/// The handles of light `l` drawn at icon size `size` (see
/// [`light_lines`]), where they are in the scene.
pub fn light_handles(l: &Light, size: f64) -> Vec<(LightHandle, Vec3)> {
    let (at, dir, u, v) = (l.position(), l.direction(), l.axis(0), l.axis(1));
    match l.kind {
        LightKind::Spot { cone, .. } => {
            let len = size * 2.0;
            let r = len * (cone / 2.0).clamp(0.0, 89.0).to_radians().tan();
            [u, v, -u, -v].iter().enumerate().map(|(k, a)| (LightHandle::Cone(k), at + dir * len + a * r)).collect()
        }
        LightKind::Area { size: wh, .. } => {
            [u * wh[0] / 2.0, -u * wh[0] / 2.0, v * wh[1] / 2.0, -v * wh[1] / 2.0].iter().enumerate().map(|(k, a)| (LightHandle::Edge(k), at + a)).collect()
        }
        _ => Vec::new(),
    }
}

/// Where along the line `from + along × a` the ray `o + d t` passes
/// nearest: `a` (None when they're parallel).
fn nearest_along(from: Vec3, along: Vec3, o: Vec3, d: Vec3) -> Option<f64> {
    let w = from - o;
    let (b, dd, aa) = (along.dot(&d), d.dot(&d), along.dot(&along));
    let den = aa * dd - b * b;
    (den.abs() > 1e-12 * aa * dd).then(|| (b * d.dot(&w) - dd * along.dot(&w)) / den)
}

/// Dragging handle `h` of light `l` (icon size `size`) to where the ray
/// `o + d t` points: the param to set and its value — a spot's cone angle
/// (its edge through the pointer) or an area light's size (that edge at
/// the pointer, the opposite one mirroring it).
pub fn drag_light(l: &Light, h: LightHandle, size: f64, o: Vec3, d: Vec3) -> Option<(&'static str, serde_json::Value)> {
    let (at, dir) = (l.position(), l.direction());
    match (h, l.kind) {
        (LightHandle::Cone(k), LightKind::Spot { .. }) => {
            let len = size * 2.0;
            let out = [l.axis(0), l.axis(1), -l.axis(0), -l.axis(1)][k % 4];
            let r = nearest_along(at + dir * len, out, o, d)?.max(0.0);
            let cone = (2.0 * (r / len).atan().to_degrees()).clamp(0.0, 179.0);
            Some(("cone", serde_json::json!(cone)))
        }
        (LightHandle::Edge(k), LightKind::Area { size: wh, .. }) => {
            let axis = l.axis(k / 2);
            let half = nearest_along(at, axis, o, d)?.abs().max(0.0005);
            let mut wh = wh;
            wh[k / 2] = 2.0 * half;
            Some(("size", serde_json::json!(wh)))
        }
        _ => None,
    }
}

/// The size lights, cameras and locators are drawn at in `scene`.
fn icon_size(scene: &Scene) -> f64 {
    bounds(scene, None).map_or(1.0, |(_, r)| (r * 0.08).max(0.15))
}

/// Splat set `s`'s dots in the 3D view: each center (in the scene) and its
/// color seen from any side (the SH's first term), every so many past
/// [`MAX_SPLAT_DOTS`].
fn splat_dots(s: &arclight::splat::Splats) -> impl Iterator<Item = (Vec3, Color32)> + '_ {
    let every = s.len().div_ceil(MAX_SPLAT_DOTS).max(1);
    (0..s.len()).step_by(every).filter(move |&i| s.sh.len() >= (i + 1) * s.sh_stride()).map(move |i| {
        let at = geom::transform_point(&s.world, &Vec3::from(s.positions[i].map(f64::from)));
        let [r, g, b] = arclight::splat::sh_color(&s.sh_of(i)[..3], 0, [0.0, 0.0, 1.0]).map(|c| c.clamp(0.0, 1.0));
        (at, egui::Rgba::from_rgb(r, g, b).into())
    })
}

/// Where particle `pt` is at each of `times` (frames on from its frame, at
/// `fps`), as a render's motion blur moves it (see [`Particle::moved`]).
/// Empty without times.
fn streak(pt: &Particle, times: &[f64], fps: f64) -> Vec<Vec3> {
    times.iter().map(|t| pt.moved(t / fps).0).collect()
}

/// How the mouse and trackpad navigate the view (Preferences ▸ 3D view;
/// the defaults are as it always has).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Navigation {
    /// Alt + left-drag navigates, as Blender's "emulate 3-button mouse".
    pub emulate_three_button: bool,
    /// Two-finger scroll pans (⇧ orbits) rather than orbits (⇧ pans).
    pub trackpad_pans: bool,
    /// Zoom toward the pointer, else toward the middle of the view.
    pub zoom_to_pointer: bool,
    /// The wheel, ⌃ + scroll and ⌃ + middle-drag zoom the other way.
    pub invert_zoom: bool,
    /// Orbit about the selected objects (else about the view's middle).
    pub orbit_selection: bool,
}

impl Default for Navigation {
    fn default() -> Self {
        Navigation { emulate_three_button: false, trackpad_pans: false, zoom_to_pointer: true, invert_zoom: false, orbit_selection: true }
    }
}

/// What a scroll does to the view.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scroll {
    /// Zoom by this factor (below 1: closer).
    Zoom(f64),
    Pan(Vec2),
    Orbit(f64, f64),
}

impl Navigation {
    /// What a scroll of `delta` does: a wheel zooms; trackpad fingers (and
    /// their momentum) orbit, ⇧ pan, ⌃ zoom (orbit and pan swapped by
    /// `trackpad_pans`). When the platform doesn't say (Linux), line steps
    /// mean a wheel, and so do fine steps soon after one (`wheel_recent`):
    /// smooth-scrolling wheels send part-notches as fine steps, which
    /// would otherwise orbit.
    fn scroll(&self, unit: egui::MouseWheelUnit, delta: Vec2, source: egui::MouseWheelSource, mods: egui::Modifiers, wheel_recent: bool) -> Scroll {
        let wheel = match source {
            egui::MouseWheelSource::Wheel => true,
            egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum => false,
            egui::MouseWheelSource::Unknown => unit != egui::MouseWheelUnit::Point || wheel_recent,
        };
        let turn = if self.invert_zoom { -1.0 } else { 1.0 };
        if wheel {
            let steps = if unit == egui::MouseWheelUnit::Point { delta.y as f64 / 50.0 } else { delta.y as f64 };
            Scroll::Zoom(0.85f64.powf(steps * turn))
        } else if mods.command || mods.ctrl {
            Scroll::Zoom((-delta.y as f64 * 0.01 * turn).exp())
        } else if mods.shift != self.trackpad_pans {
            Scroll::Pan(-delta)
        } else {
            Scroll::Orbit(-delta.x as f64, -delta.y as f64)
        }
    }

    /// The factor ⌃ + middle-drag by `dy` zooms by.
    fn drag_zoom(&self, dy: f32) -> f64 {
        (dy as f64 * 0.01 * if self.invert_zoom { -1.0 } else { 1.0 }).exp()
    }

    /// Where a zoom goes toward: the pointer at `at`, or the middle of `rect`.
    fn zoom_toward(&self, at: Pos2, rect: Rect) -> Pos2 {
        if self.zoom_to_pointer { at } else { rect.center() }
    }
}

/// The camera and frame size that render the cards for projector `p` at
/// `pixels`, matching what it draws: the scene camera through it; the free
/// camera; for orthographic, an orthographic camera as wide as the view
/// shows at its orbit center, pulled back along its axis so it cuts off
/// nothing the view draws.
fn render_camera(view: &ViewCam, p: &Projector, through: Option<geom::Camera>, pixels: [usize; 2]) -> (geom::Camera, Size) {
    let format = Size { width: pixels[0].max(1), height: pixels[1].max(1) };
    if let Some(cam) = through {
        return (cam, format);
    }
    let film = [FILM, FILM * format.height as f64 / format.width as f64];
    if view.ortho {
        let back = view.distance * 1000.0;
        let mut v = *view;
        v.distance += back;
        let cam = geom::Camera { film_back: film, world: v.world(), near: back * 1e-3, far: back * 10.0, ortho: p.rect.width() as f64 / p.f, ..geom::Camera::default() };
        return (cam, format);
    }
    (geom::Camera { focal: FOCAL, film_back: film, world: view.world(), near: p.near, far: view.distance * 1e4, ortho: 0.0 }, format)
}

/// The 3D viewport (see the module notes).
pub struct Viewport3d {
    pub view: ViewCam,
    /// Through a scene camera or a light instead of the free camera.
    pub look_through: Option<Through>,
    /// A camera or light node to look through once the scene has it (see
    /// [`look_through_node`](Self::look_through_node)).
    through_node: Option<String>,
    /// The project's frame size (for looking through a light).
    format: Size,
    /// A light handle being dragged (its node), and the one under the
    /// pointer.
    light_drag: Option<(String, LightHandle)>,
    light_hot: Option<LightHandle>,
    /// How the mouse and trackpad navigate (Preferences ▸ 3D view).
    pub nav: Navigation,
    /// When a wheel last scrolled in line steps (seconds; see
    /// [`Navigation::scroll`]).
    wheel_at: f64,
    /// What orbiting turns about this frame: the selection, when
    /// `nav.orbit_selection` and it's in the view.
    pivot: Option<Vec3>,
    /// The view a project without a saved one starts with (the saved view
    /// preferences), and the Solid / Lit for one that never chose.
    pub new_view: ViewCam,
    /// Solid or Lit for projects without their own view, over the last
    /// used one in `new_view` (Preferences ▸ 3D view); None: as `new_view`.
    pub default_shading: Option<Shading>,
    worker: Option<SceneWorker>,
    scene: Option<Arc<Scene>>,
    /// What was last asked of the worker (see [`Ask`]).
    asked: Option<Ask>,
    /// The graph as of a revision, shared with the worker.
    graph: Option<(u64, Arc<arclight::graph::Graph>)>,
    /// When the view last changed (seconds), and how long the last render
    /// took (ms): renders are smaller while it moves.
    moved_at: f64,
    render_ms: f32,
    /// The shutter times the particles streak over (see
    /// [`SceneResponse::blur_times`](crate::worker::SceneResponse)).
    blur_times: Vec<f64>,
    /// The node whose scene is shown, if any.
    node: Option<String>,
    last: Option<Projector>,
    /// Camera nodes' paths.
    paths: Vec<CameraPath>,
    /// The textured cards, and where on screen they go.
    texture: Option<TextureHandle>,
    texture_rect: Option<Rect>,
    /// The texture has the splats in it (the view has settled): their
    /// centers aren't drawn over it.
    splats_shown: bool,
    error: Option<String>,
    /// Selected points (of point clouds and Camera Trackers): node, index.
    pub points: Vec<(String, usize)>,
    /// A box selection being dragged: screen start and now, and whether it
    /// adds to the selection.
    marquee: Option<(Pos2, Pos2, bool)>,
    message: Option<String>,
    /// The project the view was last restored from (its path), and
    /// whether it had a saved view (if not, the first scene is framed).
    restored: Option<Option<std::path::PathBuf>>,
    frame_first_scene: bool,
    /// The backtick pie, open at this screen point.
    pie: Option<Pos2>,
    /// Each shown mesh's edges (by the mesh's address), for wireframes.
    edges: std::collections::HashMap<usize, Arc<Vec<[u32; 2]>>>,
    /// Selected cards, lights, cameras… that aren't in the viewed scene,
    /// each as its own output: drawn dashed, and given the gizmo.
    ghosts: Option<Arc<Scene>>,
    /// Selected nodes whose ghost couldn't be made, and why.
    ghost_errors: Vec<(String, String)>,
    /// The last click: where, everything under it (nearest first) and
    /// which of them it picked; a click there again picks the next.
    cycle: Option<(Pos2, Vec<Hit>, usize)>,
    /// The pointer and what a click there would pick (drawn highlighted).
    hover: Option<(Pos2, Option<Hit>)>,
}

impl Default for Viewport3d {
    fn default() -> Viewport3d {
        Viewport3d {
            view: ViewCam::default(),
            look_through: None,
            through_node: None,
            format: Size { width: 1920, height: 1080 },
            light_drag: None,
            light_hot: None,
            nav: Navigation::default(),
            wheel_at: f64::NEG_INFINITY,
            pivot: None,
            new_view: ViewCam::default(),
            default_shading: None,
            worker: None,
            scene: None,
            asked: None,
            graph: None,
            moved_at: 0.0,
            render_ms: 0.0,
            blur_times: Vec::new(),
            node: None,
            last: None,
            paths: Vec::new(),
            texture: None,
            texture_rect: None,
            splats_shown: false,
            error: None,
            points: Vec::new(),
            marquee: None,
            message: None,
            restored: None,
            frame_first_scene: false,
            pie: None,
            edges: Default::default(),
            ghosts: None,
            ghost_errors: Vec::new(),
            cycle: None,
            hover: None,
        }
    }
}

const GRID: Color32 = Color32::from_gray(70);
const GRID_MAJOR: Color32 = Color32::from_gray(95);
const X_COLOR: Color32 = Color32::from_rgb(220, 70, 80);
const Y_COLOR: Color32 = Color32::from_rgb(120, 200, 60);
const Z_COLOR: Color32 = Color32::from_rgb(70, 130, 235);
const CAMERA: Color32 = Color32::from_rgb(200, 200, 205);
const CARD: Color32 = Color32::from_rgb(230, 170, 60);
const MESH: Color32 = Color32::from_rgb(150, 170, 190);
/// Meshes with more edges than this are outlined by their box.
const MAX_EDGES: usize = 40_000;
const POINT: Color32 = Color32::from_rgb(90, 175, 225);
/// The most particles drawn as dots per stream (past it, every so many).
const MAX_PARTICLE_DOTS: usize = 20_000;
/// A box's twelve edges, by corner (bit 0: x high, 1: y high, 2: z high).
const BOX_EDGES: [(usize, usize); 12] = [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];
/// The most splats drawn as dots per set (past it, every so many).
const MAX_SPLAT_DOTS: usize = 300_000;
const SELECTED: Color32 = Color32::from_rgb(255, 160, 40);
/// What a click would pick (see `hover`).
const HOVER: Color32 = Color32::from_rgba_premultiplied(230, 240, 255, 230);
const LIGHT: Color32 = Color32::from_rgb(250, 225, 120);
/// Particle emitters and forces.
const GADGET: Color32 = Color32::from_rgb(150, 220, 130);
/// Splat Crop regions.
const CROP: Color32 = Color32::from_rgb(200, 150, 235);

// `projector`, `has_scene` and `object_world` are the seam for the
// transform gizmos (gizmo3d.rs) and the viewer.
#[allow(dead_code)]
impl Viewport3d {
    /// The view as of the last frame drawn (for the transform gizmos).
    pub fn projector(&self) -> Option<Projector> {
        self.last
    }

    /// Frame the next scene the viewer shows (when it arrives from the
    /// worker), as for a project opened without a saved view.
    pub fn frame_next_scene(&mut self) {
        self.frame_first_scene = true;
        self.scene = None;
        self.asked = None;
    }

    /// Whether the viewed node shows a scene.
    pub fn has_scene(&self) -> bool {
        self.scene.is_some()
    }

    /// How solid meshes are shaded: as chosen, else Lit when the viewed
    /// scene has lights and Solid when not.
    pub fn shading(&self) -> Shading {
        let lights = self.scene.as_ref().is_some_and(|s| !s.lights.is_empty());
        self.view.shading.unwrap_or(if lights { Shading::Lit } else { Shading::Solid })
    }

    /// Choose Solid or Lit shading (⇧Z, the toolbar, View menu).
    pub fn set_shading(&mut self, shading: Shading) {
        self.view.shading = Some(shading);
    }

    /// Switch between Solid and Lit shading.
    pub fn toggle_shading(&mut self) {
        self.set_shading(match self.shading() {
            Shading::Solid => Shading::Lit,
            Shading::Lit => Shading::Solid,
        });
    }

    /// What the renderer is asked for: Lit's shadows only once the view
    /// settles (`settled`), so navigating stays quick.
    fn preview(&self, settled: bool) -> Preview {
        match self.shading() {
            Shading::Solid => Preview::Solid,
            Shading::Lit => Preview::Lit { shadows: self.view.shadows && settled },
        }
    }

    /// The world matrix of node `node`'s object in the viewed scene (a
    /// card, mesh, camera, light or locator; no parenting yet, so it's the node's own
    /// translate / rotate / scale).
    pub fn object_world(&self, node: &str) -> Option<Mat4> {
        self.scene.as_deref().and_then(|s| Self::world_in(s, node)).or_else(|| Self::world_in(self.ghosts.as_deref()?, node))
    }

    /// Selected nodes shown as ghosts (not in the viewed scene), and those
    /// whose ghost couldn't be made, with why.
    pub fn ghosts(&self) -> (Vec<String>, &[(String, String)]) {
        let held: Vec<String> = self.ghosts.as_deref().map_or_else(Vec::new, |g| {
            let mut v: Vec<String> = g.cards.iter().map(|c| c.node.clone()).chain(g.meshes.iter().map(|m| m.node.clone())).chain(g.lights.iter().map(|l| l.node.clone())).collect();
            v.extend((0..g.cameras.len()).map(|i| g.camera_node(i).to_string()).chain((0..g.points.len()).map(|i| g.point_node(i).to_string())));
            v.sort();
            v.dedup();
            v
        });
        (held, &self.ghost_errors)
    }

    /// Show `scene` and `ghosts` as drawn last through `p` (for tests
    /// elsewhere, without a worker).
    #[cfg(test)]
    pub fn show_for_test(&mut self, scene: Option<Arc<Scene>>, ghosts: Option<Arc<Scene>>, p: Projector) {
        (self.scene, self.ghosts, self.last) = (scene, ghosts, Some(p));
    }

    #[cfg(test)]
    pub fn set_ghost_errors_for_test(&mut self, errors: Vec<(String, String)>) {
        self.ghost_errors = errors;
    }

    /// The scene and the ghosts together (for picking).
    fn pickable(&self) -> Option<Arc<Scene>> {
        let scene = self.scene.clone()?;
        Some(match self.ghosts.as_deref() {
            Some(g) => {
                let mut all = (*scene).clone();
                all.extend(g);
                Arc::new(all)
            }
            None => scene,
        })
    }

    /// The world matrix of node `node`'s object in scene `s`: a card,
    /// mesh, camera, light, particle gadget or locator.
    fn world_in(s: &Scene, node: &str) -> Option<Mat4> {
        if let Some(c) = s.cards.iter().find(|c| c.node == node) {
            return Some(c.world);
        }
        if let Some(m) = s.meshes.iter().find(|m| m.node == node) {
            return Some(m.world);
        }
        if let Some(i) = (0..s.cameras.len()).find(|&i| s.camera_node(i) == node) {
            return Some(s.cameras[i].world);
        }
        if let Some(l) = s.lights.iter().find(|l| l.node == node) {
            return Some(l.world);
        }
        if let Some(g) = gadgets(s).into_iter().find(|g| g.node == node) {
            return Some(g.world);
        }
        let i = (0..s.points.len()).find(|&i| s.point_node(i) == node)?;
        Some(Mat4::new_translation(&s.points[i]))
    }

    /// The camera to look through (`look_through`, if it's still there).
    fn through_camera(&self) -> Option<geom::Camera> {
        self.look_through.as_ref()?.camera_in(self.scene.as_ref()?, self.format)
    }

    /// What's looked through, by name ("" for an unnamed camera).
    fn through_name(&self) -> String {
        match (&self.look_through, &self.scene) {
            (Some(Through::Camera(i)), Some(s)) => s.camera_node(*i).to_string(),
            (Some(Through::Light(n)), _) => n.clone(),
            _ => String::new(),
        }
    }

    /// Take what the worker sent back, and ask it again when the viewed
    /// node, the frame, the graph or the view changed: for the node's scene
    /// and its cards rendered through the view (at half or a quarter of the
    /// resolution while the view moves, full once it has been still for a
    /// moment).
    fn sync(&mut self, ctx: &egui::Context, doc: &Doc, frame: i64, p: &Projector) {
        let worker = self.worker.get_or_insert_with(|| SceneWorker::spawn(ctx.clone()));
        let responses: Vec<_> = std::iter::from_fn(|| worker.poll()).collect();
        for r in responses {
            if Some(&r.node) != self.node.as_ref() {
                continue;
            }
            match r.scene {
                Ok(s) => {
                    self.scene = s;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
            self.render_ms = r.millis;
            self.ghosts = r.ghosts;
            self.ghost_errors = r.ghost_errors;
            self.splats_shown = r.splats;
            self.blur_times = r.blur_times;
            if let Some(img) = r.render {
                let size = [img.format.width, img.format.height];
                let ci = egui::ColorImage::from_rgba_premultiplied(size, &premultiplied_bytes(&img));
                match &mut self.texture {
                    Some(t) => t.set(ci, TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("viewport3d", ci, TextureOptions::LINEAR)),
                }
                self.texture_rect = r.rect;
            }
        }
        let node = doc.viewed();
        if node != self.node {
            self.scene = None;
            self.ghosts = None;
            self.ghost_errors.clear();
            self.look_through = None;
            self.texture_rect = None;
            self.blur_times.clear();
            self.node = node.clone();
        }
        let Some(node) = node else {
            self.asked = None;
            return;
        };
        let now = ctx.input(|i| i.time);
        let through = self.through_camera();
        let view_changed = self.asked.as_ref().is_some_and(|a| a.view != self.view || a.look_through != self.look_through || a.through != through);
        if view_changed {
            self.moved_at = now;
        }
        let scale = if now - self.moved_at < 0.25 {
            ctx.request_repaint_after(std::time::Duration::from_millis(260));
            if self.render_ms > 30.0 { 0.25 } else { 0.5 }
        } else {
            1.0
        };
        let target = p.frame.unwrap_or(p.rect);
        let ppp = ctx.pixels_per_point() * scale;
        let pixels = [(target.width() * ppp).round() as usize, (target.height() * ppp).round() as usize];
        // Asked again when the scene's lights change what Lit is.
        let preview = self.preview(scale == 1.0);
        let ghosts = doc.selection();
        let ask = Ask { node: node.clone(), frame, revision: doc.revision, view: self.view, look_through: self.look_through.clone(), through: through.clone(), pixels, preview, ghosts: ghosts.clone() };
        if self.asked.as_ref() == Some(&ask) {
            return;
        }
        if self.graph.as_ref().is_none_or(|(r, _)| *r != doc.revision) {
            self.graph = Some((doc.revision, Arc::new(doc.graph.clone())));
        }
        let graph = self.graph.as_ref().unwrap().1.clone();
        // A looked-through camera moves within the blur's shutter too.
        let looking = through.is_some().then(|| self.look_through.clone().map(|t| (t, self.format))).flatten();
        let render = self.scene.is_some().then(|| render_camera(&self.view, p, through, pixels));
        if let Some(w) = &self.worker {
            // A render's motion blur and the splats only once the view has
            // settled.
            w.request(SceneRequest { graph, node, frame, render, solid: !self.view.wireframe, preview, blur: scale == 1.0, splats: scale == 1.0, through: looking, rect: Some(target), ghosts });
        }
        self.asked = Some(ask);
    }

    /// Draw the viewport into `rect` (whose response is `resp`) and handle
    /// its input. `blocked`: a transform gizmo or G / R / S gesture owns the
    /// left button and the keys; navigation still works.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &Response, doc: &mut Doc, frame: i64, blocked: bool) {
        self.restore(doc);
        self.format = doc.graph.project.format;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(38));
        let placed: Vec<Mat4> = doc.selection().iter().filter_map(|n| self.object_world(n)).collect();
        self.pivot = self.nav.orbit_selection.then(|| crate::gizmo3d::median(&placed)).flatten();
        self.navigate(ui, rect, resp, blocked);
        if !blocked && resp.hovered() {
            self.keys(ui, doc);
        }
        self.resolve_through_node();
        let p = match self.through_camera() {
            Some(cam) => Projector::through(&cam, doc.graph.project.format, rect),
            None => {
                self.look_through = None;
                self.view.projector(rect)
            }
        };
        self.last = Some(p);
        self.sync(ui.ctx(), doc, frame, &p);
        if self.frame_first_scene && self.scene.is_some() {
            self.frame_first_scene = false;
            self.frame_all();
        }
        self.save(doc);
        let on_handle = !blocked && self.pie.is_none() && self.light_handle_input(ui, resp, doc, frame, &p);
        if self.pie.is_some() {
            self.pie_ui(ui, &painter, blocked);
        } else if !blocked && !on_handle {
            self.select(ui, resp, doc, &p);
        } else {
            self.marquee = None;
            self.hover = None;
        }
        self.context_menu(resp, doc, frame);
        self.draw(ui, &painter, &p, doc, frame);
        if let Some((a, b, _)) = self.marquee {
            painter.rect(Rect::from_two_pos(a, b), 0.0, Color32::from_white_alpha(18), Stroke::new(1.0, Color32::from_white_alpha(170)), egui::StrokeKind::Middle);
        }
        self.nav_gizmo(ui, rect);
        if let Some(m) = &self.message {
            painter.text(rect.left_top() + Vec2::new(10.0, 10.0), Align2::LEFT_TOP, m, FontId::proportional(13.0), Color32::from_rgb(110, 200, 120));
        }
        if let Some(e) = &self.error {
            painter.text(rect.left_bottom() + Vec2::new(10.0, -10.0), Align2::LEFT_BOTTOM, e, FontId::proportional(13.0), Color32::from_rgb(255, 110, 100));
        } else if self.scene.is_none() {
            painter.text(rect.center(), Align2::CENTER_CENTER, "No 3D scene here: view a Camera, Card, mesh, Scene, Camera Tracker or Scanline Render", FontId::proportional(14.0), Color32::GRAY);
        }
    }

    /// Middle-drag, scroll, trackpad and (emulated) Alt + left-drag.
    fn navigate(&mut self, ui: &egui::Ui, rect: Rect, resp: &Response, blocked: bool) {
        let p = self.view.projector(rect);
        let (mods, events) = ui.input(|i| (i.modifiers, i.raw.events.clone()));
        let middle = resp.dragged_by(PointerButton::Middle);
        let emulated = self.nav.emulate_three_button && !blocked && resp.dragged_by(PointerButton::Primary) && mods.alt;
        if middle || emulated {
            let d = resp.drag_delta();
            if mods.shift {
                self.leave_camera();
                self.view.pan(d, &p);
            } else if mods.command || mods.ctrl {
                let at = self.nav.zoom_toward(resp.interact_pointer_pos().unwrap_or(rect.center()), rect);
                self.view.zoom_at(self.nav.drag_zoom(d.y), at, &p);
            } else {
                self.leave_camera();
                self.orbit(d.x as f64, d.y as f64);
            }
        }
        if !resp.hovered() {
            return;
        }
        let at = self.nav.zoom_toward(ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center()), rect);
        let now = ui.input(|i| i.time);
        for e in events {
            if let egui::Event::MouseWheel { unit, delta, source, modifiers, .. } = e {
                if unit != egui::MouseWheelUnit::Point && source == egui::MouseWheelSource::Unknown {
                    self.wheel_at = now;
                }
                match self.nav.scroll(unit, delta, source, modifiers, now - self.wheel_at < WHEEL_SPELL) {
                    Scroll::Zoom(f) => self.view.zoom_at(f, at, &p),
                    Scroll::Pan(d) => {
                        self.leave_camera();
                        self.view.pan(d, &p);
                    }
                    Scroll::Orbit(x, y) => {
                        self.leave_camera();
                        self.orbit(x, y);
                    }
                }
            }
        }
        let pinch = ui.input(|i| i.zoom_delta());
        if pinch != 1.0 {
            self.view.zoom_at(1.0 / pinch as f64, at, &p);
        }
    }

    /// The open pie: draw its slots around where it opened, the one the
    /// pointer points to lit; a click or letting go of ` takes it, Esc
    /// (or a gizmo taking over) closes it.
    fn pie_ui(&mut self, ui: &egui::Ui, painter: &egui::Painter, blocked: bool) {
        let Some(center) = self.pie else { return };
        let hover = ui.input(|i| i.pointer.hover_pos()).unwrap_or(center);
        let lit = pie_slot(center, hover);
        painter.circle_stroke(center, 20.0, Stroke::new(1.0, Color32::from_white_alpha(90)));
        for (k, label) in PIE.iter().enumerate() {
            let a = (k as f32 * 45.0).to_radians();
            let at = center + Vec2::new(a.cos(), a.sin()) * 95.0;
            let galley = painter.layout_no_wrap(label.to_string(), FontId::proportional(13.0), Color32::from_gray(230));
            let r = Rect::from_center_size(at, galley.size() + Vec2::new(16.0, 8.0));
            let fill = if lit == Some(k) { Color32::from_rgb(70, 110, 160) } else { Color32::from_gray(50) };
            painter.rect(r, 4.0, fill, Stroke::new(1.0, Color32::from_gray(90)), egui::StrokeKind::Outside);
            painter.galley(r.center() - galley.size() / 2.0, galley, Color32::from_gray(230));
        }
        let (click, released, esc) = ui.input(|i| (i.pointer.primary_clicked(), i.key_released(egui::Key::Backtick), i.key_pressed(egui::Key::Escape)));
        if esc || blocked {
            self.pie = None;
        } else if click || (released && lit.is_some()) {
            self.pie = None;
            if let Some(k) = lit {
                self.pie_pick(k);
            }
        }
    }

    /// Do what pie slot `k` says.
    fn pie_pick(&mut self, k: usize) {
        match PIE[k] {
            "Right" => self.axis(Axis::Right),
            "Left" => self.axis(Axis::Left),
            "Top" => self.axis(Axis::Top),
            "Bottom" => self.axis(Axis::Bottom),
            "Front" => self.axis(Axis::Front),
            "Back" => self.axis(Axis::Back),
            "Camera" => {
                if self.look_through.is_none() {
                    self.toggle_camera();
                }
            }
            _ => {
                self.view.ortho = !self.view.ortho;
                self.view.auto_ortho = false;
            }
        }
    }

    /// The view a project without its own starts with: `new_view`, with
    /// the preferred Solid / Lit over its last used one.
    pub fn fresh_view(&self) -> ViewCam {
        ViewCam { shading: self.default_shading.or(self.new_view.shading), ..self.new_view }
    }

    /// Take the view saved with a project when one is opened (or framing
    /// its first scene, when none was saved).
    fn restore(&mut self, doc: &Doc) {
        if self.restored.as_ref() == Some(&doc.path) {
            return;
        }
        self.restored = Some(doc.path.clone());
        match doc.graph.view.0.get("viewport3d").and_then(|v| serde_json::from_value::<ViewCam>(v.clone()).ok()) {
            Some(v) => {
                self.view = ViewCam { shading: v.shading.or(self.fresh_view().shading), ..v };
                self.frame_first_scene = false;
            }
            None => {
                self.view = self.fresh_view();
                self.frame_first_scene = true;
            }
        }
        self.look_through = None;
    }

    /// Keep the view in the project's saved view state (no edit: it saves
    /// with the project but isn't undone or marked as a change).
    fn save(&self, doc: &mut Doc) {
        let v = serde_json::to_value(self.view).expect("view serializes");
        if doc.graph.view.0.get("viewport3d") != Some(&v) {
            doc.graph.view.0.insert("viewport3d".into(), v);
        }
    }

    /// Whether node `n` is a Locator (its point is an object, not a point).
    fn locator(doc: &Doc) -> impl Fn(&str) -> bool + '_ {
        move |n: &str| doc.node(n).is_some_and(|n| n.op == "locator")
    }

    /// Left-click picks (⇧ adds or removes), a left-drag from empty space
    /// box-selects (⇧ adds); clicking empty space deselects.
    fn select(&mut self, ui: &egui::Ui, resp: &Response, doc: &mut Doc, p: &Projector) {
        let Some(scene) = self.pickable() else { return };
        let aspect = project_aspect(doc);
        let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
        if self.nav.emulate_three_button && alt {
            return;
        }
        if resp.clicked_by(PointerButton::Primary) {
            if let Some(s) = resp.interact_pointer_pos() {
                let hit = self.click(&scene, p, aspect, s, shift, doc);
                self.apply_hit(doc, hit, shift);
            }
        }
        // What a click would pick, while the pointer moves with no button
        // down.
        let idle = !ui.input(|i| i.pointer.any_down()) && self.marquee.is_none();
        match resp.hover_pos().filter(|_| idle) {
            Some(at) if self.hover.as_ref().is_none_or(|(was, _)| *was != at) => {
                let hits = pick_all(&scene, p, aspect, at, &Self::locator(doc));
                let next = self.cycle_next(at, &hits);
                self.hover = Some((at, hits.get(next).cloned()));
            }
            Some(_) => {}
            None => self.hover = None,
        }
        if resp.drag_started_by(PointerButton::Primary) {
            let start = ui.input(|i| i.pointer.press_origin());
            if let Some(o) = start.filter(|o| pick(&scene, p, aspect, *o, &Self::locator(doc)).is_none()) {
                self.marquee = Some((o, o, shift));
            }
        }
        if let (Some(m), Some(now)) = (&mut self.marquee, resp.interact_pointer_pos()) {
            m.1 = now;
        }
        if resp.drag_stopped_by(PointerButton::Primary) {
            if let Some((a, b, add)) = self.marquee.take() {
                let (objects, points) = box_pick(&scene, p, a, b, &Self::locator(doc));
                if !add {
                    self.points.clear();
                }
                self.points.extend(points.into_iter().filter(|pt| !self.points.contains(pt)).collect::<Vec<_>>());
                doc.select_nodes(&objects, add);
            }
        }
    }

    /// What a click at `at` picks: the first of what's under it, or, clicked
    /// again where it was last (and the same things under it), the next
    /// one (⇧ always takes the first).
    fn click(&mut self, scene: &Scene, p: &Projector, aspect: f64, at: Pos2, shift: bool, doc: &Doc) -> Option<Hit> {
        let hits = pick_all(scene, p, aspect, at, &Self::locator(doc));
        let k = if shift { 0 } else { self.cycle_next(at, &hits) };
        let hit = hits.get(k).cloned();
        self.cycle = Some((at, hits, k));
        self.hover = None;
        hit
    }

    /// Which of `hits` (under `at`) a click there picks: the one after the
    /// last click's, if it was here on the same things, else the first.
    fn cycle_next(&self, at: Pos2, hits: &[Hit]) -> usize {
        match &self.cycle {
            Some((was, before, k)) if was.distance(at) <= CYCLE_REACH && before.as_slice() == hits && !hits.is_empty() => (k + 1) % hits.len(),
            _ => 0,
        }
    }

    /// Select what a click hit (`add`: toggle it instead), or with nothing
    /// hit and not adding, nothing.
    fn apply_hit(&mut self, doc: &mut Doc, hit: Option<Hit>, add: bool) {
        match (hit, add) {
            (Some(Hit::Object(n)), false) => {
                self.points.clear();
                doc.select_only(&n);
            }
            (Some(Hit::Object(n)), true) => doc.toggle_selected(&n),
            (Some(Hit::Point(n, i)), false) => {
                self.points = vec![(n, i)];
            }
            (Some(Hit::Point(n, i)), true) => match self.points.iter().position(|p| *p == (n.clone(), i)) {
                Some(k) => {
                    self.points.remove(k);
                }
                None => self.points.push((n, i)),
            },
            (None, false) => {
                self.points.clear();
                doc.clear_selection();
            }
            (None, true) => {}
        }
    }

    /// Everything in the scene (A).
    fn select_everything(&mut self, doc: &mut Doc) {
        let Some(s) = self.scene.clone() else { return };
        let is_locator = Self::locator(doc);
        let mut objects: Vec<String> = s
            .cards
            .iter()
            .map(|c| c.node.clone())
            .chain(s.meshes.iter().map(|m| m.node.clone()))
            .chain((0..s.cameras.len()).map(|i| s.camera_node(i).to_string()))
            .chain(s.lights.iter().map(|l| l.node.clone()))
            .chain(gadgets(&s).into_iter().map(|g| g.node))
            .collect();
        self.points.clear();
        for i in 0..s.points.len() {
            let n = s.point_node(i);
            if is_locator(n) {
                objects.push(n.to_string());
            } else {
                self.points.push((n.to_string(), i));
            }
        }
        drop(is_locator);
        objects.retain(|n| !n.is_empty());
        objects.dedup();
        doc.select_nodes(&objects, false);
    }

    /// The right-click menu: place a Card or a Locator on the selected
    /// points of a Camera Tracker (M8's placement), and framing.
    fn context_menu(&mut self, resp: &Response, doc: &mut Doc, frame: i64) {
        let tracker = self.points.first().map(|p| p.0.clone()).filter(|n| self.points.iter().all(|p| &p.0 == n));
        let sol = tracker.as_ref().and_then(|n| doc.node(n)).filter(|n| n.op == "camera_tracker").and_then(|n| arclight::camtrack::Solution::from_params(&n.params));
        let ids: Vec<usize> = self.points.iter().map(|p| p.1).collect();
        let mut placed: Option<String> = None;
        resp.context_menu(|ui| {
            let enabled = sol.is_some();
            let why = "Select points of a solved Camera Tracker";
            if ui.add_enabled(enabled, egui::Button::new("Place Card here")).on_disabled_hover_text(why).clicked() {
                if let (Some(t), Some(sol)) = (&tracker, &sol) {
                    placed = Some(match crate::camera_tracker::place_card(doc, t, sol, &ids, frame) {
                        Ok(card) => format!("Added {card}: connect an image to it and add it to a Scene."),
                        Err(e) => format!("{e:#}"),
                    });
                }
                ui.close();
            }
            if ui.add_enabled(enabled, egui::Button::new("Place locator")).on_disabled_hover_text(why).clicked() {
                if let (Some(t), Some(sol)) = (&tracker, &sol) {
                    placed = crate::camera_tracker::place_locator(doc, t, sol, &ids).map(|l| format!("Added {l}."));
                }
                ui.close();
            }
            ui.separator();
            if ui.button("Frame selected (F)").clicked() {
                let sel = doc.selection();
                self.frame_selected(Some(&sel));
                ui.close();
            }
            if ui.button("Frame all (Home)").clicked() {
                self.frame_all();
                ui.close();
            }
        });
        if placed.is_some() {
            self.message = placed;
        }
    }

    /// Hovering and dragging the handles of the selected spot or area light
    /// (the primary selection): true while one is under the pointer or
    /// being dragged, so the click doesn't also select. A drag sets the
    /// param (a key at `frame` if it's animated), one undo step.
    fn light_handle_input(&mut self, ui: &egui::Ui, resp: &Response, doc: &mut Doc, frame: i64, p: &Projector) -> bool {
        self.light_hot = None;
        let Some(scene) = self.scene.clone() else {
            self.light_drag = None;
            return false;
        };
        let size = icon_size(&scene);
        if let Some((node, h)) = self.light_drag.clone() {
            if !ui.input(|i| i.pointer.primary_down()) {
                self.light_drag = None;
                return true;
            }
            self.light_hot = Some(h);
            if let (Some(l), Some(at)) = (scene.lights.iter().find(|l| l.node == node), ui.input(|i| i.pointer.hover_pos())) {
                let (o, d) = p.ray(at);
                if let Some((name, v)) = drag_light(l, h, size, o, d) {
                    doc.edit_param(&node, name, frame, v);
                }
            }
            return true;
        }
        let Some(node) = doc.selection().last().cloned() else { return false };
        let Some(l) = scene.lights.iter().find(|l| l.node == node) else { return false };
        let Some(at) = resp.hover_pos() else { return false };
        let hot = light_handles(l, size)
            .into_iter()
            .filter_map(|(h, w)| p.project(w).map(|(s, _)| (h, s.distance(at))))
            .filter(|(_, d)| *d <= HANDLE_PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(h, _)| h);
        let Some(h) = hot else { return false };
        self.light_hot = Some(h);
        if ui.input(|i| i.pointer.primary_pressed()) {
            let name = if matches!(h, LightHandle::Cone(_)) { "cone" } else { "size" };
            let raw = doc.param(&node, name);
            if arclight::link::is_link(&raw) {
                self.message = Some(format!("{node}'s {name} is linked ({}): Unlink it in Properties to change it here", arclight::link::describe(&raw)));
            } else {
                self.light_drag = Some((node, h));
            }
        }
        true
    }

    /// Leave a look-through for the free camera, where that camera was.
    fn leave_camera(&mut self) {
        let Some(cam) = self.through_camera() else { return };
        let eye = geom::transform_point(&cam.world, &Vec3::zeros());
        let fwd = (cam.world.fixed_view::<3, 3>(0, 0) * -Vec3::z()).normalize();
        let d = self.view.distance;
        self.view.center = (eye + fwd * d).into();
        self.view.yaw = (-fwd.x).atan2(-fwd.z).to_degrees();
        self.view.pitch = (-fwd.y).asin().to_degrees().clamp(-PITCH_LIMIT, PITCH_LIMIT);
        self.view.ortho = false;
        self.look_through = None;
    }

    /// The view keys (see the module notes).
    fn keys(&mut self, ui: &egui::Ui, doc: &mut Doc) {
        use egui::Key;
        let (pressed, ctrl, alt, shift) = ui.input(|i| {
            let keys = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Period, Key::Home, Key::F, Key::A, Key::Z, Key::Escape, Key::Backtick];
            (keys.into_iter().filter(|k| i.key_pressed(*k)).collect::<Vec<_>>(), i.modifiers.command || i.modifiers.ctrl, i.modifiers.alt, i.modifiers.shift)
        });
        for k in pressed {
            match k {
                Key::Num1 => self.axis(if ctrl { Axis::Back } else { Axis::Front }),
                Key::Num3 => self.axis(if ctrl { Axis::Left } else { Axis::Right }),
                Key::Num7 => self.axis(if ctrl { Axis::Bottom } else { Axis::Top }),
                Key::Num5 => {
                    self.view.ortho = !self.view.ortho;
                    self.view.auto_ortho = false;
                }
                Key::Num0 => {
                    let sel = doc.selection();
                    self.look_through_selected(&sel);
                }
                Key::Num4 => self.step(15.0, 0.0),
                Key::Num6 => self.step(-15.0, 0.0),
                Key::Num8 => self.step(0.0, 15.0),
                Key::Num2 => self.step(0.0, -15.0),
                Key::Home => self.frame_all(),
                Key::Period | Key::F => {
                    let sel = doc.selection();
                    self.frame_selected(Some(&sel));
                }
                Key::A if alt => {
                    self.points.clear();
                    doc.clear_selection();
                }
                Key::A => self.select_everything(doc),
                Key::Z if shift => self.toggle_shading(),
                Key::Z => self.view.wireframe = !self.view.wireframe,
                Key::Backtick if self.pie.is_none() => self.pie = ui.input(|i| i.pointer.hover_pos()),
                // Esc cancels a box first, else deselects.
                Key::Escape if self.marquee.take().is_none() => {
                    self.points.clear();
                    doc.clear_selection();
                }
                _ => {}
            }
        }
    }

    /// Orbit by a drag of (`dx`, `dy`) pixels: about the selection (see
    /// `pivot`), else the view's middle.
    fn orbit(&mut self, dx: f64, dy: f64) {
        match self.pivot {
            Some(p) => self.view.orbit_about(p, dx, dy),
            None => self.view.orbit(dx, dy),
        }
    }

    fn axis(&mut self, a: Axis) {
        self.look_through = None;
        self.view.view_axis(a);
    }

    /// Orbit by whole degrees (the 2 / 4 / 6 / 8 keys).
    fn step(&mut self, yaw: f64, pitch: f64) {
        self.leave_camera();
        self.orbit(-yaw / ORBIT_SPEED, -pitch / ORBIT_SPEED);
    }

    /// Look through the scene's first camera, or back out.
    pub fn toggle_camera(&mut self) {
        if self.look_through.is_some() {
            self.leave_camera();
        } else if self.scene.as_ref().is_some_and(|s| !s.cameras.is_empty()) {
            self.look_through = Some(Through::Camera(0));
        }
    }

    /// Look through the primary selected object of `selection` (the last)
    /// if it's a camera, spot or area light, else the scene's first camera;
    /// or back out if already looking through something. Says why not
    /// when there's nothing to look through.
    pub fn look_through_selected(&mut self, selection: &[String]) {
        if self.look_through.is_some() {
            self.leave_camera();
            return;
        }
        let Some(s) = self.scene.as_ref() else { return };
        let pick = selection.last().and_then(|n| self.through_of(n));
        match pick.or_else(|| (!s.cameras.is_empty()).then_some(Through::Camera(0))) {
            Some(t) => self.look_through = Some(t),
            None => self.message = Some("Nothing to look through: select a camera, spot light or area light".into()),
        }
    }

    /// How to look through node `n` of the scene: a camera, spot or area
    /// light (None for anything else, or not in the scene).
    fn through_of(&self, n: &str) -> Option<Through> {
        let s = self.scene.as_ref()?;
        if let Some(i) = (0..s.cameras.len()).find(|&i| s.camera_node(i) == n) {
            return Some(Through::Camera(i));
        }
        let l = s.lights.iter().find(|l| l.node == n)?;
        light_camera(l, self.format).map(|_| Through::Light(n.to_string()))
    }

    /// Look through camera, spot or area light node `n` (as soon as the
    /// viewed scene is there; it says so if `n` isn't in it).
    pub fn look_through_node(&mut self, n: &str) {
        self.through_node = Some(n.to_string());
    }

    /// Look through the asked-for node once there's a scene.
    fn resolve_through_node(&mut self) {
        if let Some(n) = self.through_node.take_if(|_| self.scene.is_some()) {
            match self.through_of(&n) {
                Some(t) => self.look_through = Some(t),
                None => self.message = Some(format!("Can't look through '{n}': it isn't in the viewed scene")),
            }
        }
    }

    /// The camera or light node being looked through, if any (or about
    /// to be).
    pub fn looking_through(&self) -> Option<String> {
        self.through_node.clone().or_else(|| Some(self.through_name()).filter(|n| self.look_through.is_some() && !n.is_empty()))
    }

    /// Back out of looking through, where the camera or light was.
    pub fn stop_looking_through(&mut self) {
        self.through_node = None;
        self.leave_camera();
    }

    /// The aspect of the last view drawn.
    fn aspect(&self) -> f64 {
        self.last.map_or(16.0 / 9.0, |p| p.rect.width() as f64 / p.rect.height().max(1.0) as f64)
    }

    /// Frame everything in the scene.
    pub fn frame_all(&mut self) {
        if let Some((c, r)) = self.scene.as_ref().and_then(|s| bounds(s, None)) {
            self.look_through = None;
            self.view.frame(c, r, self.aspect());
        }
    }

    /// Frame the selected objects (`selected`: the nodes; None: the doc's
    /// selection, given by the caller), or everything when none are in the
    /// scene.
    pub fn frame_selected(&mut self, selected: Option<&[String]>) {
        let Some(s) = self.scene.clone() else { return };
        let sel: Vec<String> = selected.map(<[String]>::to_vec).unwrap_or_default();
        // Selected objects, and selected points (alone, as a little scene).
        let mut picked = Scene { points: self.points.iter().filter_map(|(_, i)| s.points.get(*i).copied()).collect(), ..Scene::default() };
        let only = |n: &str| sel.iter().any(|s| s == n);
        picked.cards = s.cards.iter().filter(|c| only(&c.node)).cloned().collect();
        picked.meshes = s.meshes.iter().filter(|m| only(&m.node)).cloned().collect();
        picked.lights = s.lights.iter().filter(|l| only(&l.node)).cloned().collect();
        for (i, c) in s.cameras.iter().enumerate() {
            if only(s.camera_node(i)) {
                picked.cameras.push(c.clone());
            }
        }
        for (i, pt) in s.points.iter().enumerate() {
            if only(s.point_node(i)) {
                picked.points.push(*pt);
            }
        }
        match bounds(&picked, None) {
            Some((c, r)) => {
                self.look_through = None;
                self.view.frame(c, r, self.aspect());
            }
            None => self.frame_all(),
        }
    }

    /// The grid, axes, cards, meshes, cameras, paths, lights, points and
    /// locators.
    fn draw(&mut self, ui: &egui::Ui, painter: &egui::Painter, p: &Projector, doc: &Doc, frame: i64) {
        let line = |a: Vec3, b: Vec3, stroke: Stroke| {
            if let Some((sa, sb)) = p.segment(a, b) {
                painter.line_segment([sa, sb], stroke);
            }
        };
        // The ground grid on XZ, its spacing a power of ten that suits the
        // view, centered under the orbit center.
        let reach = self.view.distance.max(1.0) * 2.0;
        let step = 10f64.powf((reach / 20.0).log10().floor()).max(1e-3);
        let n = (reach / step).ceil() as i64;
        let (cx, cz) = ((self.view.center[0] / step).round() as i64, (self.view.center[2] / step).round() as i64);
        for i in -n..=n {
            for (k, axis) in [(cx + i, 0), (cz + i, 2)] {
                let v = k as f64 * step;
                let (a, b) = if axis == 0 {
                    (Vec3::new(v, 0.0, (cz - n) as f64 * step), Vec3::new(v, 0.0, (cz + n) as f64 * step))
                } else {
                    (Vec3::new((cx - n) as f64 * step, 0.0, v), Vec3::new((cx + n) as f64 * step, 0.0, v))
                };
                let color = if k == 0 { if axis == 0 { Z_COLOR } else { X_COLOR } } else if k % 10 == 0 { GRID_MAJOR } else { GRID };
                line(a, b, Stroke::new(if k == 0 { 1.5 } else { 1.0 }, color));
            }
        }
        line(Vec3::zeros(), Vec3::new(0.0, step * 2.0, 0.0), Stroke::new(1.5, Y_COLOR));
        // The textured cards, under the outlines (the newest render, which
        // may lag the view by a frame while it moves).
        if let (Some(tex), Some(r)) = (&self.texture, self.texture_rect) {
            if self.scene.as_ref().is_some_and(|s| !s.cards.is_empty() || (!self.view.wireframe && !s.meshes.is_empty()) || (self.splats_shown && !s.splats.is_empty())) {
                painter.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
        }
        let Some(scene) = self.scene.clone() else { return };
        let selected = |n: &str| !n.is_empty() && doc.is_selected(n);
        for c in &scene.cards {
            let k = c.corners();
            let stroke = Stroke::new(if selected(&c.node) { 2.0 } else { 1.0 }, if selected(&c.node) { SELECTED } else { CARD });
            for i in 0..4 {
                line(k[i], k[(i + 1) % 4], stroke);
            }
        }
        // Meshes: every edge as a wireframe, else the selected ones' edges
        // over the solid render (a box round one too big to outline).
        self.edges.retain(|k, _| scene.meshes.iter().any(|m| Arc::as_ptr(&m.mesh) as usize == *k));
        for m in &scene.meshes {
            let on = selected(&m.node);
            if !self.view.wireframe && !on {
                continue;
            }
            let stroke = Stroke::new(if on { 1.5 } else { 1.0 }, if on { SELECTED } else { MESH });
            let edges = self.edges.entry(Arc::as_ptr(&m.mesh) as usize).or_insert_with(|| Arc::new(mesh_edges(&m.mesh))).clone();
            if edges.len() > MAX_EDGES {
                if let Some((lo, hi)) = m.mesh.bounds() {
                    let c = |i: usize| geom::transform_point(&m.world, &Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }));
                    for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                        line(c(a), c(b), stroke);
                    }
                }
                continue;
            }
            let world: Vec<Vec3> = m.mesh.positions.iter().map(|q| geom::transform_point(&m.world, q)).collect();
            for [a, b] in edges.iter() {
                line(world[*a as usize], world[*b as usize], stroke);
            }
        }
        let size = icon_size(&scene);
        let aspect = doc.graph.project.format.width as f64 / doc.graph.project.format.height.max(1) as f64;
        for (i, cam) in scene.cameras.iter().enumerate() {
            if self.look_through == Some(Through::Camera(i)) {
                continue;
            }
            let node = scene.camera_node(i);
            let color = if selected(node) { SELECTED } else { CAMERA };
            let stroke = Stroke::new(if selected(node) { 2.0 } else { 1.2 }, color);
            for (a, b) in camera_lines(cam, size, aspect) {
                line(a, b, stroke);
            }
            self.draw_path(painter, p, doc, node, frame);
        }
        let primary = doc.selection().last().cloned();
        for l in &scene.lights {
            if self.look_through == Some(Through::Light(l.node.clone())) {
                continue;
            }
            let on = selected(&l.node);
            let stroke = Stroke::new(if on { 2.0 } else { 1.2 }, if on { SELECTED } else { LIGHT });
            for (a, b) in light_lines(l, size) {
                line(a, b, stroke);
            }
            if let Some((s, _)) = p.project(l.position()) {
                // A dot at the light itself (a ring for ambient light),
                // what a click picks.
                if l.kind == LightKind::Ambient {
                    painter.circle_stroke(s, 5.0, stroke);
                    painter.circle_stroke(s, 2.0, stroke);
                } else {
                    painter.circle_filled(s, 2.5, stroke.color);
                }
            }
            // The primary selection's handles: squares, the one under the
            // pointer (or dragged) white.
            if primary.as_deref() == Some(l.node.as_str()) {
                for (h, w) in light_handles(l, size) {
                    if let Some((s, _)) = p.project(w) {
                        let hot = self.light_hot == Some(h);
                        let r = Rect::from_center_size(s, Vec2::splat(if hot { 9.0 } else { 7.0 }));
                        painter.rect(r, 1.0, if hot { Color32::WHITE } else { SELECTED }, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
                    }
                }
            }
        }
        for (i, pt) in scene.points.iter().enumerate() {
            let node = scene.point_node(i);
            let Some((s, _)) = p.project(*pt) else { continue };
            if !p.rect.contains(s) {
                continue;
            }
            let locator = doc.node(node).is_some_and(|n| n.op == "locator");
            if locator {
                let arm = size * 0.5;
                let color = |c: Color32| if selected(node) { SELECTED } else { c };
                line(*pt - Vec3::x() * arm, *pt + Vec3::x() * arm, Stroke::new(1.5, color(X_COLOR)));
                line(*pt - Vec3::y() * arm, *pt + Vec3::y() * arm, Stroke::new(1.5, color(Y_COLOR)));
                line(*pt - Vec3::z() * arm, *pt + Vec3::z() * arm, Stroke::new(1.5, color(Z_COLOR)));
            } else {
                let on = self.points.iter().any(|(n, k)| n == node && *k == i);
                let color = scene.point_errors.get(i).copied().filter(|e| *e >= 0.0).map_or(POINT, error_color);
                painter.circle_filled(s, if on { 3.0 } else { 1.8 }, if on { SELECTED } else { color });
            }
        }
        // Particle emitters, attractors, vortices and bounce planes; Splat
        // Crops' regions.
        for g in gadgets(&scene) {
            let on = selected(&g.node);
            let color = if matches!(g.kind, GadgetKind::Crop { .. }) { CROP } else { GADGET };
            let stroke = Stroke::new(if on { 2.0 } else { 1.2 }, if on { SELECTED } else { color });
            for (a, b) in gadget_lines(&g, size) {
                line(a, b, stroke);
            }
            if let Some((s, _)) = p.project(geom::transform_point(&g.world, &Vec3::zeros())) {
                painter.circle_filled(s, 2.5, stroke.color);
            }
        }
        // Particles: a dot each in its color (every so many past a cap),
        // ringed when their node is selected; streaked over the shutter
        // when the render's motion blur is shown.
        for stream in &scene.particles {
            let all = stream.particles();
            let every = all.len().div_ceil(MAX_PARTICLE_DOTS).max(1);
            let on = selected(&stream.node);
            let fps = stream.fps();
            for pt in all.iter().step_by(every) {
                let Some((s, _)) = p.project(pt.position) else { continue };
                let streak: Vec<Pos2> = streak(pt, &self.blur_times, fps).into_iter().filter_map(|q| Some(p.project(q)?.0)).collect();
                if !p.rect.contains(s) && !streak.iter().any(|q| p.rect.contains(*q)) {
                    continue;
                }
                let a = pt.color[3].max(1e-6);
                let [r, g, b] = [0, 1, 2].map(|c| (pt.color[c] / a).clamp(0.0, 1.0));
                let color: Color32 = egui::Rgba::from_rgb(r, g, b).into();
                let long = streak.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>() >= 1.0;
                if long {
                    if on {
                        painter.add(egui::Shape::line(streak.clone(), Stroke::new(5.2, SELECTED)));
                    }
                    painter.add(egui::Shape::line(streak, Stroke::new(3.2, color)));
                    continue;
                }
                if on {
                    painter.circle_filled(s, 2.6, SELECTED);
                }
                painter.circle_filled(s, 1.6, color);
            }
        }
        // Splats, while the render doesn't have them (the view moving): a
        // dot at each center in its color (every so many past a cap), as
        // one mesh (there can be hundreds of thousands).
        let mut dots = egui::Mesh::default();
        for set in scene.splats.iter().filter(|_| !self.splats_shown) {
            let on = selected(&set.node);
            for (at, color) in splat_dots(set) {
                let Some((s, _)) = p.project(at) else { continue };
                if p.rect.contains(s) {
                    dots.add_colored_rect(Rect::from_center_size(s, Vec2::splat(if on { 2.6 } else { 1.8 })), if on { SELECTED } else { color });
                }
            }
        }
        if !dots.is_empty() {
            painter.add(egui::Shape::mesh(dots));
        }
        // A selected splat set: the box round its centers.
        for (lo, hi) in scene.splats.iter().filter(|set| selected(&set.node)).filter_map(|set| set.bounds()) {
            let corner = |i: usize| Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][(i >> 2) & 1]);
            for (a, b) in BOX_EDGES {
                line(corner(a), corner(b), Stroke::new(1.5, SELECTED));
            }
        }
        // Selected objects not in the viewed scene, dashed (see `ghosts`).
        if let Some(g) = self.ghosts.clone() {
            let dashed = |a: Vec3, b: Vec3| {
                if let Some((sa, sb)) = p.segment(a, b) {
                    painter.extend(egui::Shape::dashed_line(&[sa, sb], Stroke::new(1.5, SELECTED), 6.0, 4.0));
                }
            };
            for (_, lines) in object_lines(&g, size, aspect) {
                lines.into_iter().for_each(|(a, b)| dashed(a, b));
            }
            for m in &g.meshes {
                box_lines(m).into_iter().for_each(|(a, b)| dashed(a, b));
            }
            for pt in &g.points {
                let arm = size * 0.5;
                for axis in [Vec3::x(), Vec3::y(), Vec3::z()] {
                    dashed(*pt - axis * arm, *pt + axis * arm);
                }
            }
        }
        // What a click would pick, brighter.
        if let Some((_, Some(hit))) = &self.hover {
            let hover = Stroke::new(2.5, HOVER);
            let all = self.pickable().unwrap_or(scene.clone());
            match hit {
                Hit::Object(n) => {
                    for (_, lines) in object_lines(&all, size, aspect).into_iter().filter(|(o, _)| o == n) {
                        lines.into_iter().for_each(|(a, b)| line(a, b, hover));
                    }
                    for m in all.meshes.iter().filter(|m| &m.node == n) {
                        box_lines(m).into_iter().for_each(|(a, b)| line(a, b, hover));
                    }
                    let dots = (0..all.points.len()).filter(|&i| all.point_node(i) == n).map(|i| all.points[i]);
                    let lights = all.lights.iter().filter(|l| &l.node == n).map(|l| l.position());
                    for at in dots.chain(lights) {
                        if let Some((q, _)) = p.project(at) {
                            painter.circle_stroke(q, 6.0, hover);
                        }
                    }
                    for (lo, hi) in all.splats.iter().filter(|g| &g.node == n).filter_map(|g| g.bounds()) {
                        let corner = |i: usize| Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][(i >> 2) & 1]);
                        BOX_EDGES.iter().for_each(|&(a, b)| line(corner(a), corner(b), hover));
                    }
                }
                Hit::Point(_, i) => {
                    if let Some((q, _)) = all.points.get(*i).and_then(|pt| p.project(*pt)) {
                        painter.circle_stroke(q, 5.0, hover);
                    }
                }
            }
        }
        // Through a camera: its frame, the outside darkened.
        if let Some(f) = p.frame {
            let dim = Color32::from_black_alpha(150);
            let r = p.rect;
            for band in [
                Rect::from_min_max(r.min, Pos2::new(r.max.x, f.min.y)),
                Rect::from_min_max(Pos2::new(r.min.x, f.max.y), r.max),
                Rect::from_min_max(Pos2::new(r.min.x, f.min.y), Pos2::new(f.min.x, f.max.y)),
                Rect::from_min_max(Pos2::new(f.max.x, f.min.y), Pos2::new(r.max.x, f.max.y)),
            ] {
                painter.rect_filled(band, 0.0, dim);
            }
            painter.rect_stroke(f, 0.0, Stroke::new(1.0, Color32::from_gray(200)), egui::StrokeKind::Outside);
            let name = self.through_name();
            painter.text(f.left_top() + Vec2::new(4.0, -4.0), Align2::LEFT_BOTTOM, format!("Through {name}"), FontId::proportional(12.0), Color32::from_gray(220));
        }
        let _ = ui;
    }

    /// An animated camera's path over the project's frames, with a tick
    /// per frame and the current one marked.
    fn draw_path(&mut self, painter: &egui::Painter, p: &Projector, doc: &Doc, node: &str, frame: i64) {
        if node.is_empty() || !doc.is_node_animated(node) || doc.node(node).is_none_or(|n| n.op != "camera") {
            return;
        }
        let rev = doc.revision;
        if !self.paths.iter().any(|(n, r, _)| n == node && *r == rev) {
            let (first, last) = (doc.graph.project.first_frame, doc.graph.project.last_frame);
            let pts = (first..=last)
                .map(|f| {
                    let params = serde_json::json!({ "translate": doc.param_at(node, "translate", f), "rotate": doc.param_at(node, "rotate", f) });
                    (f, Vec3::from(arclight::scene::numbers::<3>(&params["translate"], 0.0)))
                })
                .collect();
            self.paths.retain(|(n, _, _)| n != node);
            self.paths.push((node.to_string(), rev, pts));
        }
        let Some((_, _, pts)) = self.paths.iter().find(|(n, _, _)| n == node) else { return };
        let stroke = Stroke::new(1.0, Color32::from_rgb(170, 130, 220));
        for w in pts.windows(2) {
            if let Some((a, b)) = p.segment(w[0].1, w[1].1) {
                painter.line_segment([a, b], stroke);
            }
        }
        for (f, at) in pts {
            if let Some((s, _)) = p.project(*at) {
                if *f == frame {
                    painter.circle_stroke(s, 4.0, Stroke::new(1.5, Color32::WHITE));
                } else {
                    painter.circle_filled(s, 1.5, stroke.color);
                }
            }
        }
    }

    /// The navigation gizmo in the top-right corner: an axis ball (click an
    /// axis to look along it, drag to orbit) and pan, zoom, camera and
    /// perspective buttons under it.
    fn nav_gizmo(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let r = 38.0;
        let center = Pos2::new(rect.max.x - r - 14.0, rect.min.y + r + 14.0);
        let ball = ui.interact(Rect::from_center_size(center, Vec2::splat(2.0 * r)), ui.scope_id().with("nav_ball"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        if ball.hovered() || ball.dragged() {
            painter.circle_filled(center, r, Color32::from_white_alpha(18));
        }
        // Each axis's direction on screen (camera space x right, y up).
        let rot = match self.through_camera() {
            Some(cam) => cam.world.fixed_view::<3, 3>(0, 0).into_owned(),
            None => self.view.rotation().matrix().into_owned(),
        };
        let inv = rot.transpose();
        let mut bubbles: Vec<(f64, Pos2, Color32, &str, Axis, bool)> = Vec::new();
        for (axis, color, label, pos, neg) in [
            (Vec3::x(), X_COLOR, "X", Axis::Right, Axis::Left),
            (Vec3::y(), Y_COLOR, "Y", Axis::Top, Axis::Bottom),
            (Vec3::z(), Z_COLOR, "Z", Axis::Front, Axis::Back),
        ] {
            let q = inv * axis;
            let s = Vec2::new(q.x as f32, -q.y as f32) * (r - 9.0);
            bubbles.push((q.z, center + s, color, label, pos, true));
            bubbles.push((-q.z, center - s, color, label, neg, false));
        }
        // Far ones first, so near ones draw on top.
        bubbles.sort_by(|a, b| a.0.total_cmp(&b.0));
        let hover = ui.input(|i| i.pointer.hover_pos());
        let mut clicked = None;
        for (_, at, color, label, axis, positive) in &bubbles {
            if *positive {
                painter.line_segment([center, *at], Stroke::new(2.0, *color));
            }
            let hot = hover.is_some_and(|h| h.distance(*at) < 8.0);
            if *positive {
                painter.circle_filled(*at, 8.0, if hot { Color32::WHITE } else { *color });
                painter.text(*at, Align2::CENTER_CENTER, label, FontId::proportional(10.0), Color32::BLACK);
            } else {
                painter.circle(*at, 7.0, color.gamma_multiply(0.35), Stroke::new(1.0, *color));
            }
            if ball.clicked() && ball.interact_pointer_pos().is_some_and(|p| p.distance(*at) < 8.0) {
                clicked = Some(*axis);
            }
        }
        if let Some(a) = clicked {
            self.axis(a);
        } else if ball.dragged() {
            let d = ball.drag_delta();
            self.leave_camera();
            self.orbit(d.x as f64, d.y as f64);
        }
        // The buttons: pan, zoom (drag them), camera, perspective.
        let p = self.view.projector(rect);
        for (k, what) in ["pan", "zoom", "camera", "ortho"].into_iter().enumerate() {
            let at = Pos2::new(center.x, center.y + r + 20.0 + k as f32 * 28.0);
            let b = ui.interact(Rect::from_center_size(at, Vec2::splat(24.0)), ui.scope_id().with(("nav_btn", what)), Sense::click_and_drag());
            let on = match what {
                "camera" => self.look_through.is_some(),
                "ortho" => self.view.ortho,
                _ => b.dragged(),
            };
            painter.circle_filled(at, 12.0, if on { Color32::from_rgb(70, 110, 160) } else if b.hovered() { Color32::from_gray(85) } else { Color32::from_gray(60) });
            icon(&painter, at, what);
            let tip = match what {
                "pan" => "Drag to pan (⇧ + middle-drag)",
                "zoom" => "Drag to zoom (scroll, or ⌃ + middle-drag)",
                "camera" => "Look through the scene camera (0: the selected camera, spot or area light)",
                _ => "Perspective / orthographic (5)",
            };
            let b = b.on_hover_text(tip);
            match what {
                "pan" if b.dragged() => {
                    self.leave_camera();
                    self.view.pan(b.drag_delta(), &p);
                }
                "zoom" if b.dragged() => {
                    let c = rect.center();
                    self.view.zoom_at((b.drag_delta().y as f64 * 0.01).exp(), c, &p);
                }
                "camera" if b.clicked() => self.toggle_camera(),
                "ortho" if b.clicked() => {
                    self.view.ortho = !self.view.ortho;
                    self.view.auto_ortho = false;
                }
                _ => {}
            }
        }
    }
}

/// A solved point's color by its reprojection error: green under a
/// quarter pixel, through yellow at 1, to red from 2 pixels.
pub fn error_color(e: f32) -> Color32 {
    let t = ((e - 0.25) / 1.75).clamp(0.0, 1.0);
    let (g, y, r) = ([80.0, 220.0, 90.0], [240.0, 210.0, 60.0], [235.0, 70.0, 60.0]);
    let mix = |a: [f32; 3], b: [f32; 3], u: f32| Color32::from_rgb((a[0] + (b[0] - a[0]) * u) as u8, (a[1] + (b[1] - a[1]) * u) as u8, (a[2] + (b[2] - a[2]) * u) as u8);
    if t < 0.43 { mix(g, y, t / 0.43) } else { mix(y, r, (t - 0.43) / 0.57) }
}

/// `img`'s frame as premultiplied 8-bit sRGB bytes (egui blends
/// premultiplied, so card edges stay clean over the background).
fn premultiplied_bytes(img: &Image) -> Vec<u8> {
    let flat = img.flat();
    let enc = |v: f32| (arclight::color::linear_to_srgb(v.max(0.0)) * 255.0).round() as u8;
    flat.pixels
        .iter()
        .flat_map(|p| {
            // sRGB-encode the straight color, then premultiply in 8 bits.
            let s = arclight::color::unpremultiply(*p);
            let a = p[3].clamp(0.0, 1.0);
            let pm = |c: f32| (enc(c) as f32 * a).round() as u8;
            [pm(s[0]), pm(s[1]), pm(s[2]), (a * 255.0).round() as u8]
        })
        .collect()
}

/// The navigation gizmo's button icons, drawn as lines.
fn icon(painter: &egui::Painter, at: Pos2, what: &str) {
    let s = Stroke::new(1.5, Color32::from_gray(225));
    match what {
        "pan" => {
            for d in [Vec2::X, Vec2::Y, -Vec2::X, -Vec2::Y] {
                let tip = at + d * 7.0;
                painter.line_segment([at - d * 7.0, tip], s);
                let side = Vec2::new(-d.y, d.x) * 2.5;
                painter.line_segment([tip, tip - d * 2.5 + side], s);
                painter.line_segment([tip, tip - d * 2.5 - side], s);
            }
        }
        "zoom" => {
            painter.circle_stroke(at - Vec2::splat(2.0), 4.5, s);
            painter.line_segment([at + Vec2::splat(1.5), at + Vec2::splat(6.5)], s);
        }
        "camera" => {
            painter.rect_stroke(Rect::from_center_size(at - Vec2::new(2.0, 0.0), Vec2::new(10.0, 8.0)), 1.0, s, egui::StrokeKind::Middle);
            painter.line_segment([at + Vec2::new(3.0, 0.0), at + Vec2::new(7.0, -3.5)], s);
            painter.line_segment([at + Vec2::new(3.0, 0.0), at + Vec2::new(7.0, 3.5)], s);
        }
        _ => {
            for k in [-4.0, 0.0, 4.0] {
                painter.line_segment([at + Vec2::new(-6.0, k), at + Vec2::new(6.0, k)], s);
                painter.line_segment([at + Vec2::new(k, -6.0), at + Vec2::new(k, 6.0)], s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The cameras' frame aspect the tests pick with (the default project's).
    const ASPECT: f64 = 16.0 / 9.0;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 450.0))
    }

    #[test]
    fn splats_show_as_dots_in_their_color_decimated_past_a_cap() {
        use arclight::splat::{dc_of, Splats};
        let n = 700_000;
        let s = Splats {
            positions: Arc::new((0..n).map(|i| [i as f32, 0.0, 0.0]).collect()),
            scales: Arc::new(vec![[0.1; 3]; n]),
            rotations: Arc::new(vec![[1.0, 0.0, 0.0, 0.0]; n]),
            opacity: Arc::new(vec![1.0; n]),
            sh: Arc::new([1.0, 0.5, 0.0].map(dc_of).repeat(n)),
            sh_degree: 0,
            world: geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]),
            holdout: false,
            node: "splat1".into(),
        };
        let dots: Vec<(Vec3, Color32)> = splat_dots(&s).collect();
        assert_eq!(dots.len(), 700_000usize.div_ceil(3), "every third");
        assert_eq!(dots[1].0, Vec3::new(3.0, 0.0, -5.0), "placed in the scene");
        assert_eq!(dots[0].1, Color32::from(egui::Rgba::from_rgb(1.0, 0.5, 0.0)));
    }

    #[test]
    fn navigation_preferences_change_what_scrolls_do() {
        use egui::{Modifiers, MouseWheelSource as Src, MouseWheelUnit as Unit};
        let d = Vec2::new(3.0, 10.0);
        let shift = Modifiers { shift: true, ..Modifiers::NONE };
        let ctrl = Modifiers { ctrl: true, ..Modifiers::NONE };
        let n = Navigation::default();
        // As always: a wheel zooms, fingers orbit, ⇧ pan, ⌃ zoom.
        assert_eq!(n.scroll(Unit::Line, d, Src::Wheel, Modifiers::NONE, false), Scroll::Zoom(0.85f64.powf(10.0)));
        assert_eq!(n.scroll(Unit::Line, d, Src::Unknown, Modifiers::NONE, false), Scroll::Zoom(0.85f64.powf(10.0)), "line steps: a wheel");
        // Fine steps from an unknown source orbit, unless a wheel just
        // stepped (a smooth-scrolling wheel's part-notches, on Linux).
        assert_eq!(n.scroll(Unit::Point, d, Src::Unknown, Modifiers::NONE, false), Scroll::Orbit(-3.0, -10.0));
        assert_eq!(n.scroll(Unit::Point, d, Src::Unknown, Modifiers::NONE, true), Scroll::Zoom(0.85f64.powf(10.0 / 50.0)));
        assert_eq!(n.scroll(Unit::Point, d, Src::Trackpad, Modifiers::NONE, true), Scroll::Orbit(-3.0, -10.0), "a trackpad said so");
        assert_eq!(n.scroll(Unit::Point, d, Src::Trackpad, Modifiers::NONE, false), Scroll::Orbit(-3.0, -10.0));
        assert_eq!(n.scroll(Unit::Point, d, Src::Momentum, shift, false), Scroll::Pan(-d));
        assert_eq!(n.scroll(Unit::Point, d, Src::Trackpad, ctrl, false), Scroll::Zoom((-0.1f64).exp()));
        assert_eq!((n.drag_zoom(10.0), n.zoom_toward(Pos2::new(5.0, 6.0), rect())), (0.1f64.exp(), Pos2::new(5.0, 6.0)));
        // Fingers pan (⇧ orbits); zooms turned around, toward the middle.
        let p = Navigation { trackpad_pans: true, invert_zoom: true, zoom_to_pointer: false, ..n };
        assert_eq!(p.scroll(Unit::Point, d, Src::Trackpad, Modifiers::NONE, false), Scroll::Pan(-d));
        assert_eq!(p.scroll(Unit::Point, d, Src::Trackpad, shift, false), Scroll::Orbit(-3.0, -10.0));
        assert_eq!(p.scroll(Unit::Line, d, Src::Wheel, Modifiers::NONE, false), Scroll::Zoom(0.85f64.powf(-10.0)));
        assert_eq!(p.scroll(Unit::Point, d, Src::Trackpad, ctrl, false), Scroll::Zoom(0.1f64.exp()));
        assert_eq!((p.drag_zoom(10.0), p.zoom_toward(Pos2::new(5.0, 6.0), rect())), ((-0.1f64).exp(), rect().center()));
    }

    #[test]
    fn the_preferred_shading_starts_projects_without_their_own() {
        let mut v = Viewport3d::default();
        assert_eq!(v.fresh_view(), ViewCam::default(), "auto, as always");
        v.new_view.shading = Some(Shading::Lit);
        assert_eq!(v.fresh_view().shading, Some(Shading::Lit), "auto: the last used");
        v.default_shading = Some(Shading::Solid);
        assert_eq!(v.fresh_view().shading, Some(Shading::Solid), "the preference over the last used");
        // A project's own view keeps its own shading.
        let mut doc = Doc::new(arclight::graph::Graph::default(), None);
        v.restore(&doc);
        assert_eq!(v.view.shading, Some(Shading::Solid));
        let own = ViewCam { shading: Some(Shading::Lit), ..ViewCam::default() };
        doc.graph.view.0.insert("viewport3d".into(), serde_json::to_value(own).unwrap());
        doc.path = Some("own.arc".into());
        v.restore(&doc);
        assert_eq!(v.view.shading, Some(Shading::Lit));
    }

    fn close(a: Pos2, b: Pos2) -> bool {
        a.distance(b) < 1e-3
    }

    /// A gray lambert cube seen from front right, and a sun from the right
    /// (lighting its +X face, grazing its +Z one); each face's middle on
    /// screen.
    fn sunlit_cube() -> (Scene, geom::Camera, Size, [(usize, usize); 2]) {
        let cube = arclight::scene::primitive_model("cube", &json!({ "color": [0.8, 0.8, 0.8, 1], "shading": "lambert" }), None).unwrap();
        let sun = arclight::scene::light("light_directional", &json!({ "rotate": [0, 90, 0] }), None).unwrap();
        let cam = geom::Camera { focal: 35.0, world: geom::trs([4.0, 0.0, 4.0], [0.0, 45.0, 0.0], [1.0; 3]), ..geom::Camera::default() };
        let format = Size { width: 96, height: 64 };
        let at = |p: Vec3| {
            let (q, _) = cam.project(&p, format).unwrap();
            (q[0].floor() as usize, q[1].floor() as usize)
        };
        let faces = [at(Vec3::new(0.5, 0.0, 0.0)), at(Vec3::new(0.0, 0.0, 0.5))];
        (Scene { meshes: vec![cube], lights: vec![sun], ..Scene::default() }, cam, format, faces)
    }

    #[test]
    fn lit_shading_lights_meshes_by_the_scenes_lights() {
        use arclight::render3d::render_preview;
        let (scene, cam, format, [side, front]) = sunlit_cube();
        let red = |img: &Image, (x, y): (usize, usize)| img.at(x as i32, y as i32)[0];
        // Lit: the side the sun faces bright, the one it grazes dark.
        let lit = render_preview(&scene, &cam, format, Preview::Lit { shadows: false });
        assert!(red(&lit, side) > 0.6 && red(&lit, front) < 0.05, "{} {}", red(&lit, side), red(&lit, front));
        assert!([side, front].iter().all(|&(x, y)| lit.at(x as i32, y as i32)[3] == 1.0), "both on the cube");
        // Solid: lit from the view, both faces turned as much from it alike.
        let solid = render_preview(&scene, &cam, format, Preview::Solid);
        assert!((red(&solid, side) - red(&solid, front)).abs() < 0.02 && red(&solid, front) > 0.4, "{} {}", red(&solid, side), red(&solid, front));
        // No lights: Lit is Solid, so nothing goes black.
        let dark = Scene { lights: Vec::new(), ..scene };
        assert_eq!(render_preview(&dark, &cam, format, Preview::Lit { shadows: true }).pixels, solid.pixels);
    }

    #[test]
    fn shading_is_lit_with_lights_until_chosen_and_shift_z_toggles_it() {
        let (scene, ..) = sunlit_cube();
        let mut v = Viewport3d::default();
        assert_eq!(v.shading(), Shading::Solid, "no scene");
        v.scene = Some(Arc::new(Scene { lights: Vec::new(), ..scene.clone() }));
        assert_eq!(v.shading(), Shading::Solid, "no lights");
        v.scene = Some(Arc::new(scene));
        assert_eq!(v.shading(), Shading::Lit, "lights");
        assert_eq!(v.preview(true), Preview::Lit { shadows: false }, "shadows off by default");
        v.view.shadows = true;
        assert_eq!((v.preview(true), v.preview(false)), (Preview::Lit { shadows: true }, Preview::Lit { shadows: false }), "not while moving");
        v.toggle_shading();
        assert_eq!((v.shading(), v.preview(true)), (Shading::Solid, Preview::Solid));
        v.toggle_shading();
        assert_eq!(v.shading(), Shading::Lit);
        // The choice is kept with the view (in the project).
        let back: ViewCam = serde_json::from_value(serde_json::to_value(v.view).unwrap()).unwrap();
        assert_eq!(back.shading, Some(Shading::Lit));
        let old: ViewCam = serde_json::from_value(json!({ "center": [0, 0, 0], "yaw": 0, "pitch": 0, "distance": 5, "ortho": false })).unwrap();
        assert_eq!((old.shading, old.shadows), (None, false), "older projects: auto");
    }

    #[test]
    fn projection_and_rays_agree() {
        for ortho in [false, true] {
            let v = ViewCam { center: [1.0, 0.5, -2.0], yaw: 40.0, pitch: 20.0, distance: 9.0, ortho, auto_ortho: false, wireframe: false, shading: None, shadows: false };
            let p = v.projector(rect());
            // The orbit center is the middle of the view.
            let (c, depth) = p.project(Vec3::from(v.center)).unwrap();
            assert!(close(c, rect().center()) && (depth - 9.0).abs() < 1e-9);
            // A point projected, then its ray, passes back through it.
            let x = Vec3::new(2.0, 1.0, -1.0);
            let (s, _) = p.project(x).unwrap();
            let (o, d) = p.ray(s);
            let off = (x - o) - d * (x - o).dot(&d);
            // (Screen positions are f32.)
            assert!(off.norm() < 1e-5, "ortho {ortho}: {off}");
        }
        // The cards render through a camera landing points where the view
        // draws them (orthographic: a true one, even behind the orbit eye),
        // at twice the screen's pixels.
        for ortho in [false, true] {
            let v = ViewCam { center: [1.0, 0.5, -2.0], yaw: 40.0, pitch: 20.0, distance: 9.0, ortho, ..ViewCam::default() };
            let p = v.projector(rect());
            let pixels = [(rect().width() * 2.0) as usize, (rect().height() * 2.0) as usize];
            let (cam, format) = render_camera(&v, &p, None, pixels);
            assert_eq!(cam.is_ortho(), ortho);
            for x in [Vec3::new(2.0, 1.0, -1.0), v.eye() + (v.eye() - Vec3::from(v.center)) * if ortho { 0.5 } else { -0.5 }] {
                let (s, _) = p.project(x).unwrap();
                let (q, _) = cam.project(&x, format).unwrap();
                let back = rect().min + Vec2::new(q[0] as f32, q[1] as f32) / 2.0;
                assert!(close(s, back), "ortho {ortho}: {s:?} vs {back:?}");
            }
        }
        // Through an orthographic scene camera: its frame, flat; nothing
        // behind it.
        let cam = geom::Camera { ortho: 6.0, world: geom::trs([0.0, 1.0, 4.0], [-10.0, 20.0, 0.0], [1.0; 3]), ..geom::Camera::default() };
        let fmt = Size { width: 160, height: 90 };
        let p = Projector::through(&cam, fmt, rect());
        let frame = p.frame.unwrap();
        let x = Vec3::new(1.0, 0.0, -3.0);
        let (q, _) = cam.project(&x, fmt).unwrap();
        let want = frame.min + Vec2::new(q[0] as f32, q[1] as f32) * (frame.width() / 160.0);
        assert!(p.ortho && close(p.project(x).unwrap().0, want));
        assert!(p.project(geom::transform_point(&cam.world, &Vec3::new(0.0, 0.0, 1.0))).is_none());
        // A segment crossing behind the camera is cut at the near plane.
        let v = ViewCam::default();
        let p = v.projector(rect());
        let eye = v.eye();
        let behind = eye + (eye - Vec3::from(v.center));
        assert!(p.segment(Vec3::from(v.center), behind).is_some());
        assert!(p.segment(behind, behind * 2.0).is_none());
    }

    #[test]
    fn navigation_keeps_what_it_should_in_place() {
        let mut v = ViewCam::default();
        // Orbit: the orbit center stays in the middle.
        v.orbit(120.0, -60.0);
        let p = v.projector(rect());
        assert!(close(p.project(Vec3::from(v.center)).unwrap().0, rect().center()));
        assert!(v.pitch.abs() <= PITCH_LIMIT);
        v.orbit(0.0, 1e5);
        assert_eq!(v.pitch, PITCH_LIMIT, "short of the pole");
        // Zoom toward the pointer: the point under it stays under it.
        for ortho in [false, true] {
            let mut v = ViewCam { ortho, ..ViewCam::default() };
            let p = v.projector(rect());
            let s = Pos2::new(600.0, 120.0);
            let (o, d) = p.ray(s);
            let fwd = p.view_dir();
            let w = o + d * if ortho { v.distance } else { v.distance / d.dot(&fwd) };
            v.zoom_at(0.5, s, &p);
            let after = v.projector(rect()).project(w).unwrap().0;
            assert!(close(after, s), "ortho {ortho}: {after:?} vs {s:?}");
            assert!((v.distance - 6.0).abs() < 1e-9);
        }
        // Orbiting about a pivot swings the camera rigidly round it: the
        // pivot stays where it was on screen.
        let mut v = ViewCam { center: [0.0; 3], distance: 10.0, ..ViewCam::default() };
        let pivot = Vec3::new(3.0, 1.0, -2.0);
        let (before, _) = v.projector(rect()).project(pivot).unwrap();
        let eye_to_pivot = (v.eye() - pivot).norm();
        v.orbit_about(pivot, 140.0, -45.0);
        let (after, _) = v.projector(rect()).project(pivot).unwrap();
        assert!(close(before, after), "{before:?} {after:?}");
        assert!(((v.eye() - pivot).norm() - eye_to_pivot).abs() < 1e-9);
        assert!(((Vec3::from(v.center) - v.eye()).norm() - 10.0).abs() < 1e-9, "same distance to the orbit center");
        // Pan: the orbit center follows the pointer.
        let mut v = ViewCam::default();
        let c = Vec3::from(v.center);
        let p = v.projector(rect());
        v.pan(Vec2::new(30.0, -20.0), &p);
        let moved = v.projector(rect()).project(c).unwrap().0;
        assert!(close(moved, rect().center() + Vec2::new(30.0, -20.0)), "{moved:?}");
    }

    #[test]
    fn axis_views_and_framing() {
        let mut v = ViewCam::default();
        v.view_axis(Axis::Front);
        assert!(v.ortho && v.eye().z > v.center[2], "front looks down −Z");
        v.view_axis(Axis::Top);
        let p = v.projector(rect());
        assert!((p.view_dir() + Vec3::y()).norm() < 1e-9, "top looks down −Y");
        v.view_axis(Axis::Right);
        assert!(v.eye().x > v.center[0]);
        v.orbit(10.0, 0.0);
        assert!(!v.ortho, "orbiting away goes back to perspective");
        // Framing: every corner of a box in view.
        let s = Scene { points: vec![Vec3::new(10.0, 0.0, 0.0), Vec3::new(14.0, 3.0, -4.0), Vec3::new(12.0, -2.0, 2.0)], ..Scene::default() };
        let (c, r) = bounds(&s, None).unwrap();
        let mut v = ViewCam::default();
        v.frame(c, r, 800.0 / 450.0);
        let p = v.projector(rect());
        for pt in &s.points {
            assert!(rect().contains(p.project(*pt).unwrap().0), "{pt}");
        }
    }

    #[test]
    fn looking_through_a_camera_matches_its_render() {
        // A point lands in the frame where Scanline Render would put it.
        let cam = geom::Camera { focal: 24.0, world: geom::trs([1.0, 2.0, 8.0], [-5.0, 10.0, 0.0], [1.0; 3]), ..geom::Camera::default() };
        let format = Size { width: 1920, height: 1080 };
        let p = Projector::through(&cam, format, rect());
        let f = p.frame.unwrap();
        assert!((f.width() / f.height() - 16.0 / 9.0).abs() < 1e-3);
        for x in [Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 3.0, -1.0)] {
            let (px, _) = cam.project(&x, format).unwrap();
            let want = f.min + Vec2::new((px[0] / 1920.0) as f32 * f.width(), (px[1] / 1080.0) as f32 * f.height());
            let got = p.project(x).unwrap().0;
            assert!(got.distance(want) < 1e-3, "{got:?} vs {want:?}");
        }
    }

    /// A camera at the origin looking down −Z, two overlapping cards, a
    /// cloud of three points, a locator and a scene camera.
    /// A card seen edge-on, a camera and a point light, side by side, and
    /// two cards one behind the other in the middle; the view from the
    /// origin down −Z.
    fn thin_scene() -> (Scene, Projector, Doc) {
        let tex = Arc::new(Image::new(4, 4));
        let card = |node: &str, at: [f64; 3], turn: f64| arclight::scene::Card { node: node.into(), ..arclight::scene::Card::new(tex.clone(), geom::trs(at, [0.0, turn, 0.0], [1.0; 3]), [2.0, 2.0]) };
        let s = Scene {
            cards: vec![card("edge", [3.0, 0.0, -8.0], 90.0), card("front", [0.0, 0.0, -6.0], 0.0), card("back", [0.0, 0.0, -10.0], 0.0)],
            cameras: vec![geom::Camera { world: geom::trs([-3.5, 1.5, -8.0], [0.0; 3], [1.0; 3]), ..geom::Camera::default() }],
            camera_nodes: vec!["cam".into()],
            lights: vec![Light { node: "bulb".into(), ..Light::new(LightKind::Point, geom::trs([0.0, -2.5, -8.0], [0.0; 3], [1.0; 3])) }],
            ..Scene::default()
        };
        let graph: arclight::graph::Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "edge", "op": "card" }, { "id": "front", "op": "card" }, { "id": "back", "op": "card" },
            { "id": "cam", "op": "camera" }, { "id": "bulb", "op": "light_point" },
        ] }))
        .unwrap();
        let p = ViewCam { center: [0.0, 0.0, -8.0], yaw: 0.0, pitch: 0.0, distance: 8.0, ortho: false, auto_ortho: false, wireframe: false, shading: None, shadows: false }.projector(rect());
        (s, p, Doc::new(graph, None))
    }

    /// Thin things are picked by the lines they're drawn as, within a few
    /// points and not past them: a card edge-on, a camera's frustum, a
    /// light's rays.
    #[test]
    fn thin_things_are_picked_by_their_lines() {
        let (s, p, doc) = thin_scene();
        let loc = Viewport3d::locator(&doc);
        let size = icon_size(&s);
        let near = |lines: Vec<(Vec3, Vec3)>, k: usize, t: f32, off: Vec2| {
            let (a, b) = p.segment(lines[k].0, lines[k].1).unwrap();
            a + (b - a) * t + off
        };
        // The edge-on card: its near upright edge, a few points to its right.
        let k = s.cards[0].corners();
        let edge = object_lines(&s, size, ASPECT).into_iter().find(|(n, _)| n == "edge").unwrap().1;
        let upright = (0..4).find(|&i| (edge[i].0.z - edge[i].1.z).abs() < 1e-9 && edge[i].0.z > -8.0).unwrap();
        assert!(k.iter().all(|c| (c.x - 3.0).abs() < 1e-9), "edge-on: {k:?}");
        let by = near(edge.clone(), upright, 0.5, Vec2::new(6.0, 0.0));
        assert_eq!(pick(&s, &p, ASPECT, by, &loc), Some(Hit::Object("edge".into())));
        assert_eq!(pick(&s, &p, ASPECT, by + Vec2::new(8.0, 0.0), &loc), None, "past reach");
        // A frustum's far top edge, and a light's ray tip, a few points off.
        let cam = camera_lines(&s.cameras[0], size, ASPECT);
        assert_eq!(pick(&s, &p, ASPECT, near(cam, 1, 0.5, Vec2::new(0.0, -6.0)), &loc), Some(Hit::Object("cam".into())));
        let rays = light_lines(&s.lights[0], size);
        assert_eq!(pick(&s, &p, ASPECT, near(rays, 0, 1.0, Vec2::new(4.0, 4.0)), &loc), Some(Hit::Object("bulb".into())));
    }

    /// Clicking again where the last click was picks the next thing under
    /// it, round and round; elsewhere, the first again. What the pointer
    /// hovers is highlighted as what the click there would pick.
    #[test]
    fn clicks_in_one_spot_cycle_through_what_s_under_it() {
        let (s, p, mut doc) = thin_scene();
        let mut v = Viewport3d::default();
        let mid = rect().center() + Vec2::new(0.0, 10.0);
        let under = pick_all(&s, &p, ASPECT, mid, &Viewport3d::locator(&doc));
        assert_eq!(under, [Hit::Object("front".into()), Hit::Object("back".into())], "nearest first");
        let mut picked = Vec::new();
        for at in [mid, mid + Vec2::new(2.0, 1.0), mid, mid + Vec2::new(30.0, 0.0), mid + Vec2::new(30.0, 0.0)] {
            // What hovering there shows is what the click picks.
            let shown = pick_all(&s, &p, ASPECT, at, &Viewport3d::locator(&doc)).get(v.cycle_next(at, &pick_all(&s, &p, ASPECT, at, &Viewport3d::locator(&doc)))).cloned();
            let hit = v.click(&s, &p, ASPECT, at, false, &doc);
            assert_eq!(shown, hit, "hover at {at:?}");
            v.apply_hit(&mut doc, hit.clone(), false);
            picked.push(hit.map(|h| match h {
                Hit::Object(n) => n,
                Hit::Point(n, _) => n,
            }));
        }
        let names: Vec<&str> = picked.iter().map(|n| n.as_deref().unwrap_or("-")).collect();
        assert_eq!(names, ["front", "back", "front", "front", "back"]);
        assert_eq!(doc.selection(), ["back"]);
        // ⇧ takes the first, adding it.
        assert_eq!(v.click(&s, &p, ASPECT, mid + Vec2::new(30.0, 0.0), true, &doc), Some(Hit::Object("front".into())));
    }

    fn picking_scene() -> (Scene, Projector, Doc) {
        use arclight::graph::{Graph, NodeSpec};
        let tex = Arc::new(Image::new(4, 4));
        let card = |node: &str, z: f64, w: f64| arclight::scene::Card { node: node.into(), ..arclight::scene::Card::new(tex.clone(), geom::trs([0.0, 0.0, z], [0.0; 3], [1.0; 3]), [w, w]) };
        let mut s = Scene {
            cards: vec![card("far", -10.0, 6.0), card("near", -6.0, 2.0)],
            points: vec![Vec3::new(2.0, 1.0, -8.0), Vec3::new(-2.0, 1.0, -8.0), Vec3::new(0.0, -1.5, -8.0), Vec3::new(3.0, -1.0, -7.0)],
            cameras: vec![geom::Camera { world: geom::trs([-2.5, -1.0, -7.0], [0.0; 3], [1.0; 3]), ..geom::Camera::default() }],
            ..Scene::default()
        };
        s.point_nodes = vec!["cloud".into(), "cloud".into(), "cloud".into(), "loc".into()];
        s.camera_nodes = vec!["cam".into()];
        let nodes = ["far", "near", "cloud", "loc", "cam"].iter().map(|id| NodeSpec {
            id: id.to_string(),
            op: match *id { "loc" => "locator", "cam" => "camera", "cloud" => "point_cloud", _ => "card" }.into(),
            inputs: vec![],
            params: serde_json::json!({}),
            pos: None,
            disabled: false,
        });
        let doc = Doc::new(Graph { nodes: nodes.collect(), ..Default::default() }, None);
        let p = ViewCam { center: [0.0, 0.0, -8.0], yaw: 0.0, pitch: 0.0, distance: 8.0, ortho: false, auto_ortho: false, wireframe: false, shading: None, shadows: false }.projector(rect());
        (s, p, doc)
    }

    #[test]
    fn splats_are_picked_by_their_nearest_center_framed_and_boxed() {
        use arclight::splat::{dc_of, Splats};
        let (mut s, p, _) = picking_scene();
        let loc = |_: &str| false;
        let at = |x: Vec3| p.project(x).unwrap().0;
        // A capture of three splats around (−3, 2, −9), placed 1 to the right.
        let centers = vec![[-4.0f32, 2.0, -9.0], [-3.0, 2.0, -9.0], [-2.0, 2.0, -9.0]];
        s.splats.push(Arc::new(Splats {
            positions: Arc::new(centers),
            scales: Arc::new(vec![[0.1; 3]; 3]),
            rotations: Arc::new(vec![[1.0, 0.0, 0.0, 0.0]; 3]),
            opacity: Arc::new(vec![1.0; 3]),
            sh: Arc::new([0.5f32; 3].map(dc_of).repeat(3)),
            sh_degree: 0,
            world: geom::trs([1.0, 0.0, 0.0], [0.0; 3], [1.0; 3]),
            holdout: false,
            node: "capture".into(),
        }));
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(-1.0, 2.0, -9.0)) + Vec2::new(3.0, 2.0), &loc), Some(Hit::Object("capture".into())), "near a center");
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(-0.5, 2.0, -9.0)), &loc), Some(Hit::Object("far".into())), "between centers, out of reach: the card behind");
        assert_eq!(pick(&s, &p, ASPECT, rect().center(), &loc), Some(Hit::Object("near".into())), "the cards as before");
        // Framed by its centers' box; boxed by its middle.
        let only = |n: &str| n == "capture";
        let (c, _) = bounds(&s, Some(&only)).unwrap();
        assert!((c - Vec3::new(-2.0, 2.0, -9.0)).norm() < 1e-6, "{c:?}");
        let mid = at(Vec3::new(-2.0, 2.0, -9.0));
        let (objects, _) = box_pick(&s, &p, mid - Vec2::splat(4.0), mid + Vec2::splat(4.0), &loc);
        assert_eq!(objects, ["capture"]);
    }

    #[test]
    fn clicks_pick_points_objects_and_the_nearer_card() {
        let (s, p, doc) = picking_scene();
        let loc = |n: &str| doc.node(n).is_some_and(|n| n.op == "locator");
        let at = |x: Vec3| p.project(x).unwrap().0;
        // A click at (or near) a projected point picks it; a locator's
        // point picks the locator; a camera's apex picks the camera.
        assert_eq!(pick(&s, &p, ASPECT, at(s.points[1]) + Vec2::new(3.0, -2.0), &loc), Some(Hit::Point("cloud".into(), 1)));
        assert_eq!(pick(&s, &p, ASPECT, at(s.points[3]), &loc), Some(Hit::Object("loc".into())));
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(-2.5, -1.0, -7.0)), &loc), Some(Hit::Object("cam".into())));
        // The middle of the view is on both cards: the nearer wins.
        assert_eq!(pick(&s, &p, ASPECT, rect().center(), &loc), Some(Hit::Object("near".into())));
        // Past the near card's edge, on the bigger far one: the far one.
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(2.6, -2.6, -10.0)), &loc), Some(Hit::Object("far".into())));
        assert_eq!(pick(&s, &p, ASPECT, Pos2::new(5.0, 5.0), &loc), None);
        // A box around the left half: the left point, the camera.
        let (objects, points) = box_pick(&s, &p, Pos2::new(0.0, 0.0), Pos2::new(rect().center().x - 50.0, rect().max.y), &loc);
        assert_eq!((objects, points), (vec!["cam".to_string()], vec![("cloud".to_string(), 1)]));
    }

    #[test]
    fn meshes_are_picked_framed_and_outlined() {
        let (mut s, p, _) = picking_scene();
        let loc = |_: &str| false;
        let at = |x: Vec3| p.project(x).unwrap().0;
        // A ball in front of the near card, and a crate off to the right.
        let mut ball = arclight::scene::primitive_model("sphere", &serde_json::json!({ "radius": 0.5, "segments": [16, 8], "translate": [0, 0, -4] }), None).unwrap();
        ball.node = "ball".into();
        let mut crate_ = arclight::scene::primitive_model("cube", &serde_json::json!({ "translate": [3.5, 2.5, -9], "rotate": [0, 30, 0] }), None).unwrap();
        crate_.node = "crate".into();
        s.meshes = vec![ball, crate_];
        // Through the ball's middle: the ball, nearer than both cards; just
        // past its edge, the near card; on the crate's middle, the crate.
        assert_eq!(pick(&s, &p, ASPECT, rect().center(), &loc), Some(Hit::Object("ball".into())));
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(0.6, 0.0, -4.0)), &loc), Some(Hit::Object("near".into())));
        assert_eq!(pick(&s, &p, ASPECT, at(Vec3::new(3.5, 2.5, -9.0)), &loc), Some(Hit::Object("crate".into())));
        // A box around the right half takes the crate (by its center).
        let (objects, _) = box_pick(&s, &p, Pos2::new(rect().center().x + 40.0, 0.0), rect().max, &loc);
        assert!(objects.contains(&"crate".to_string()) && !objects.contains(&"ball".to_string()), "{objects:?}");
        // Framing takes in its box; its world is its placement.
        let (c, r) = bounds(&Scene { meshes: vec![s.meshes[1].clone()], ..Scene::default() }, None).unwrap();
        assert!((c - Vec3::new(3.5, 2.5, -9.0)).norm() < 1e-9 && r > 0.8 && r < 1.2, "{c} {r}");
        let mut v = Viewport3d { scene: Some(Arc::new(s.clone())), ..Viewport3d::default() };
        assert_eq!(v.object_world("crate"), Some(s.meshes[1].world));
        v.frame_selected(Some(&["crate".to_string()]));
        assert!((Vec3::from(v.view.center) - Vec3::new(3.5, 2.5, -9.0)).norm() < 1e-6);
        // A cube's edges: 12 sides and a diagonal across each face, once
        // each though every face has its own corners.
        assert_eq!(mesh_edges(&arclight::mesh::cube([1.0; 3], [1; 3])).len(), 18);
    }

    #[test]
    fn lights_are_picked_framed_and_drawn() {
        let (mut s, p, _) = picking_scene();
        let loc = |_: &str| false;
        let at = |x: Vec3| p.project(x).unwrap().0;
        let light = |node: &str, kind: LightKind, t: [f64; 3], r: [f64; 3]| Light { node: node.into(), ..Light::new(kind, geom::trs(t, r, [1.0; 3])) };
        s.lights = vec![
            light("bulb", LightKind::Point, [1.0, 2.0, -6.0], [0.0; 3]),
            light("spot", LightKind::Spot { cone: 60.0, penumbra: 10.0, falloff: 0.0 }, [-1.5, 2.0, -7.0], [-90.0, 0.0, 0.0]),
            light("sun", LightKind::Directional, [3.0, 3.0, -9.0], [-45.0, 0.0, 0.0]),
            light("panel", LightKind::rect([2.0, 1.0]), [-3.0, -1.5, -8.0], [0.0, 0.0, 0.0]),
            light("fill", LightKind::Ambient, [0.0, 3.5, -8.0], [0.0; 3]),
        ];
        // A click on (or near) each light picks it, over the cards behind.
        for l in &s.lights {
            assert_eq!(pick(&s, &p, ASPECT, at(l.position()) + Vec2::new(2.0, 2.0), &loc), Some(Hit::Object(l.node.clone())), "{}", l.node);
        }
        // A box round the left half takes the spot and the panel.
        let (objects, _) = box_pick(&s, &p, Pos2::new(0.0, 0.0), Pos2::new(rect().center().x - 30.0, rect().max.y), &loc);
        assert!(objects.contains(&"spot".to_string()) && objects.contains(&"panel".to_string()) && !objects.contains(&"bulb".to_string()), "{objects:?}");
        // Framing one centers on it; its world is its placement (what the
        // gizmo and G / R / S move).
        let mut v = Viewport3d { scene: Some(Arc::new(s.clone())), ..Viewport3d::default() };
        assert_eq!(v.object_world("spot"), Some(s.lights[1].world));
        v.frame_selected(Some(&["sun".to_string()]));
        assert!((Vec3::from(v.view.center) - Vec3::new(3.0, 3.0, -9.0)).norm() < 1e-6);
        let (c, r) = bounds(&Scene { lights: s.lights.clone(), ..Scene::default() }, None).unwrap();
        assert!(r > 3.0 && (c.y - 1.0).abs() < 1e-9, "{c} {r}");
        // Icons. A point light: a star of rays through it, each `size` long.
        let star = light_lines(&s.lights[0], 1.0);
        assert_eq!(star.len(), 8);
        assert!(star.iter().all(|(a, b)| ((a + b) / 2.0 - s.lights[0].position()).norm() < 1e-9 && ((b - a).norm() - 1.0).abs() < 1e-9));
        // A spot: a cone 2 long down (its −Z), the outer ring at its 30°
        // edge, the inner where the 10° penumbra starts (20°).
        let cone = light_lines(&s.lights[1], 1.0);
        let apex = s.lights[1].position();
        let ring_radius = |lines: &[(Vec3, Vec3)]| lines.iter().map(|(a, _)| (a - (apex - Vec3::y() * 2.0)).norm()).collect::<Vec<_>>();
        let radii = ring_radius(&cone[..48]);
        assert!(radii[..24].iter().all(|r| (r - 2.0 * 30f64.to_radians().tan()).abs() < 1e-9), "{radii:?}");
        assert!(radii[24..].iter().all(|r| (r - 2.0 * 20f64.to_radians().tan()).abs() < 1e-9));
        assert!(cone[48..].iter().all(|(a, b)| *a == apex && (b - apex).normalize().dot(&-Vec3::y()) > 0.8));
        // A hard-edged spot has no inner ring.
        assert_eq!(light_lines(&Light { kind: LightKind::Spot { cone: 60.0, penumbra: 0.0, falloff: 0.0 }, ..s.lights[1].clone() }, 1.0).len(), 24 + 4);
        // Directional: three arrows, their shafts parallel to the light.
        let sun = light_lines(&s.lights[2], 1.0);
        let dir = s.lights[2].direction();
        assert!(sun.iter().step_by(3).all(|(a, b)| (b - a).normalize().dot(&dir) > 1.0 - 1e-9) && sun.len() == 9);
        // Area: its rectangle, 2 × 1, and an arrow down its −Z.
        let panel = light_lines(&s.lights[3], 1.0);
        let corners: Vec<Vec3> = panel[..4].iter().map(|(a, _)| *a).collect();
        assert!(((corners[0] - corners[1]).norm() - 2.0).abs() < 1e-9 && ((corners[1] - corners[2]).norm() - 1.0).abs() < 1e-9);
        assert!((panel[4].1 - panel[4].0).normalize().dot(&-Vec3::z()) > 1.0 - 1e-9);
        // Ambient light has no icon lines (a ring is drawn instead).
        assert!(light_lines(&s.lights[4], 1.0).is_empty());
    }

    /// A doc with a spot light "spot" and an area light "panel" (params as
    /// given), and the scene they make, tagged.
    fn light_doc(spot: serde_json::Value, panel: serde_json::Value) -> (Doc, Scene) {
        use arclight::graph::{Graph, NodeSpec};
        let node = |id: &str, op: &str, params: serde_json::Value| NodeSpec { id: id.into(), op: op.into(), inputs: vec![], params, pos: None, disabled: false };
        let doc = Doc::new(Graph { nodes: vec![node("spot", "light_spot", spot.clone()), node("panel", "light_area", panel.clone())], ..Default::default() }, None);
        let light = |n: &str, op: &str, p: &serde_json::Value| Light { node: n.into(), ..arclight::scene::light(op, p, None).unwrap() };
        let scene = Scene { lights: vec![light("spot", "light_spot", &spot), light("panel", "light_area", &panel)], ..Scene::default() };
        (doc, scene)
    }

    #[test]
    fn dragging_a_cone_edge_sets_the_angle_and_an_area_edge_the_size() {
        // A spot at the origin shining down, 40° across; an icon size of
        // 1 puts its cone's end 2 below.
        let (_, s) = light_doc(json!({ "rotate": [-90, 0, 0], "cone": 40 }), json!({ "translate": [0, 0, -5], "size": [2, 1] }));
        let (spot, panel) = (&s.lights[0], &s.lights[1]);
        let hs = light_handles(spot, 1.0);
        assert_eq!(hs.len(), 4);
        for (_, at) in &hs {
            let r = Vec3::new(at.x, 0.0, at.z).norm();
            assert!((at.y + 2.0).abs() < 1e-9 && (r - 2.0 * 20f64.to_radians().tan()).abs() < 1e-9, "on the cone's edge ring: {at}");
        }
        // A ray from the front through the point on the +X handle's line
        // at the radius of a 60° cone (and from above, down the axis).
        let want = Vec3::new(2.0 * 30f64.to_radians().tan(), -2.0, 0.0);
        let x_handle = hs.iter().find(|(_, at)| at.x > 0.1).unwrap().0;
        for o in [want + Vec3::new(0.0, 0.0, 10.0), want + Vec3::new(0.3, 10.0, 0.2)] {
            let (name, v) = drag_light(spot, x_handle, 1.0, o, (want - o).normalize()).unwrap();
            assert_eq!(name, "cone");
            assert!((v.as_f64().unwrap() - 60.0).abs() < 0.5, "{v}");
        }
        // Past the apex side: a closed cone, not a negative one.
        let (_, v) = drag_light(spot, x_handle, 1.0, Vec3::new(-3.0, -2.0, 10.0), -Vec3::z()).unwrap();
        assert_eq!(v.as_f64(), Some(0.0));
        // The area light's handles sit on its edges' middles; dragging the
        // right edge to x = 1.5 makes it 3 wide (about its middle), the
        // height kept; the bottom edge to y = −1 makes it 2 high.
        let hp = light_handles(panel, 1.0);
        let pts: Vec<Vec3> = hp.iter().map(|h| h.1).collect();
        assert_eq!(pts, [Vec3::new(1.0, 0.0, -5.0), Vec3::new(-1.0, 0.0, -5.0), Vec3::new(0.0, 0.5, -5.0), Vec3::new(0.0, -0.5, -5.0)]);
        let o = Vec3::new(0.0, 0.0, 10.0);
        let toward = |p: Vec3| (p - o).normalize();
        let (_, v) = drag_light(panel, hp[0].0, 1.0, o, toward(Vec3::new(1.5, 0.0, -5.0))).unwrap();
        assert!((v[0].as_f64().unwrap() - 3.0).abs() < 1e-9 && v[1].as_f64() == Some(1.0), "{v}");
        let (name, v) = drag_light(panel, hp[3].0, 1.0, o, toward(Vec3::new(0.0, -1.0, -5.0))).unwrap();
        assert!(name == "size" && (v[0].as_f64().unwrap() - 2.0).abs() < 1e-9 && (v[1].as_f64().unwrap() - 2.0).abs() < 1e-9, "{v}");
        assert!(light_handles(&Light::new(LightKind::Point, Mat4::identity()), 1.0).is_empty());
    }

    /// The handles in the view: pressing one and dragging edits the param
    /// (not the selection), one undo step; elsewhere a click still selects.
    #[test]
    fn handle_drags_in_the_view_are_one_undo_step() {
        let (mut doc, s) = light_doc(json!({ "translate": [0, 0, -8], "rotate": [0, 0, 0], "cone": 40 }), json!({ "translate": [5, 0, -8] }));
        doc.select_nodes(&["spot".to_string()], false);
        doc.commit(false);
        let p = ViewCam { center: [0.0, 0.0, -8.0], yaw: 0.0, pitch: 60.0, distance: 8.0, ortho: false, auto_ortho: false, wireframe: false, shading: None, shadows: false }.projector(rect());
        let size = icon_size(&s);
        let mut v = Viewport3d { scene: Some(Arc::new(s.clone())), ..Viewport3d::default() };
        let (h, at) = light_handles(&s.lights[0], size).into_iter().find(|(_, w)| p.project(*w).is_some()).unwrap();
        let start = p.project(at).unwrap().0;
        let end = start + (start - p.project(s.lights[0].position() + s.lights[0].direction() * size * 2.0).unwrap().0) * 0.5;
        let ctx = egui::Context::default();
        let pointer = |pos: Pos2, pressed: Option<bool>| {
            let mut e = vec![egui::Event::PointerMoved(pos)];
            if let Some(pressed) = pressed {
                e.push(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            }
            e
        };
        let mut handled = Vec::new();
        for events in [pointer(start, None), pointer(start, Some(true)), pointer((start + end.to_vec2()) / 2.0, None), pointer(end, None), pointer(end, Some(false)), vec![]] {
            let raw = egui::RawInput { screen_rect: Some(rect()), events, ..Default::default() };
            ctx.run_ui(raw, |ui| {
                let resp = ui.interact(rect(), ui.scope_id().with("vp"), Sense::click_and_drag());
                handled.push(v.light_handle_input(ui, &resp, &mut doc, 1, &p));
                doc.commit(ui.input(|i| i.pointer.any_down()));
            }).drop_without_applying_deltas();
        }
        // (egui knows the hover from the second frame.)
        assert_eq!(&handled[1..5], [true; 4], "hovered, pressed, dragged, released: {handled:?}");
        assert!(matches!(h, LightHandle::Cone(_)) && v.light_drag.is_none());
        let cone = doc.param("spot", "cone").as_f64().unwrap();
        assert!(cone > 50.0, "pulled out: {cone}");
        assert_eq!(doc.selection(), ["spot"], "the selection stays");
        doc.undo();
        assert_eq!(doc.param("spot", "cone").as_f64(), Some(40.0), "one undo step back to where it was");
        // Not on a handle: nothing taken.
        let raw = egui::RawInput { screen_rect: Some(rect()), events: pointer(rect().left_top() + Vec2::splat(5.0), None), ..Default::default() };
        ctx.run_ui(raw, |ui| {
            let resp = ui.interact(rect(), ui.scope_id().with("vp"), Sense::click_and_drag());
            assert!(!v.light_handle_input(ui, &resp, &mut doc, 1, &p));
        }).drop_without_applying_deltas();
    }

    #[test]
    fn looking_through_a_spot_or_area_light() {
        let (_, mut s) = light_doc(json!({ "translate": [0, 3, 0], "rotate": [-90, 0, 0], "cone": 50 }), json!({ "translate": [2, 0, 0], "rotate": [0, 90, 0] }));
        s.lights.push(Light { node: "bulb".into(), ..Light::new(LightKind::Point, Mat4::identity()) });
        s.cameras.push(geom::Camera::default());
        s.camera_nodes.push("cam".into());
        let format = Size { width: 1920, height: 1080 };
        // Through the spot, its cone's edge meets the frame's top and bottom.
        let cam = light_camera(&s.lights[0], format).unwrap();
        let edge = Vec3::new(0.0, 0.0, -3.0 * 25f64.to_radians().tan());
        let (px, _) = cam.project(&edge, format).unwrap();
        assert!(px[1].abs() < 0.5 || (px[1] - 1080.0).abs() < 0.5, "{px:?}");
        assert!((px[0] - 960.0).abs() < 1e-6);
        let a = light_camera(&s.lights[1], format).unwrap();
        assert!((geom::transform_point(&a.world, &Vec3::zeros()) - Vec3::new(2.0, 0.0, 0.0)).norm() < 1e-12 && (a.fov() - 2.0 * (45f64.to_radians().tan() * 16.0 / 9.0).atan().to_degrees()).abs() < 1e-9);
        assert!(light_camera(&s.lights[2], format).is_none());
        // 0: the selected spot or area light, else the camera; again leaves.
        let mut v = Viewport3d { scene: Some(Arc::new(s)), ..Viewport3d::default() };
        v.look_through_selected(&["spot".to_string()]);
        assert_eq!(v.look_through, Some(Through::Light("spot".into())));
        assert_eq!(v.through_camera(), Some(cam));
        assert_eq!(v.through_name(), "spot");
        v.look_through_selected(&["spot".to_string()]);
        assert_eq!(v.look_through, None, "out again, where the light was");
        assert!((Vec3::from(v.view.center) - Vec3::new(0.0, 3.0 - v.view.distance, 0.0)).norm() < 1e-6, "{:?}", v.view.center);
        v.look_through_selected(&["cam".to_string(), "panel".to_string()]);
        assert_eq!(v.look_through, Some(Through::Light("panel".into())), "the primary (last) selected");
        v.look_through = None;
        v.look_through_selected(&["bulb".to_string()]);
        assert_eq!(v.look_through, Some(Through::Camera(0)), "a point light has no view: the camera");
        v.look_through = None;
        v.scene = Some(Arc::new(Scene { lights: vec![Light { node: "bulb".into(), ..Light::new(LightKind::Point, Mat4::identity()) }], ..Scene::default() }));
        v.look_through_selected(&["bulb".to_string()]);
        assert!(v.look_through.is_none() && v.message.as_deref().is_some_and(|m| m.contains("Nothing to look through")));
    }

    /// Properties' Look through button: by node, once the scene is there.
    #[test]
    fn looking_through_a_node_waits_for_its_scene() {
        let (_, mut s) = light_doc(json!({ "translate": [0, 3, 0], "rotate": [-90, 0, 0] }), json!({ "translate": [2, 0, 0] }));
        s.cameras.push(geom::Camera::default());
        s.camera_nodes.push("cam".into());
        let mut v = Viewport3d::default();
        v.look_through_node("panel");
        v.resolve_through_node();
        assert_eq!((v.look_through.clone(), v.looking_through()), (None, Some("panel".to_string())), "no scene yet: still asked for");
        v.scene = Some(Arc::new(s));
        v.resolve_through_node();
        assert_eq!((v.look_through.clone(), v.looking_through()), (Some(Through::Light("panel".into())), Some("panel".to_string())));
        v.look_through_node("cam");
        v.resolve_through_node();
        assert_eq!(v.looking_through().as_deref(), Some("cam"));
        v.stop_looking_through();
        assert_eq!((v.look_through.clone(), v.looking_through()), (None, None));
        v.look_through_node("elsewhere");
        v.resolve_through_node();
        assert!(v.looking_through().is_none() && v.message.as_deref().is_some_and(|m| m.contains("isn't in the viewed scene")));
    }

    /// G X 2 Enter on a selected light moves it 2 along X; undo puts it
    /// back.
    #[test]
    fn g_x_2_enter_moves_a_light() {
        use crate::gizmo3d::{tests::front, GestureKey, Input, Mode, Outcome, Transforming};
        let (mut doc, s) = light_doc(json!({ "translate": [1, 2, 3] }), json!({}));
        doc.commit(false);
        let view = front();
        let world = s.lights[0].world;
        let mut g = Transforming::start(&doc, "spot", Mode::Move, &world, Pos2::new(400.0, 300.0), 1).unwrap();
        let keys = vec![GestureKey::Axis(0, false), GestureKey::Digit('2')];
        assert_eq!(g.step(&Input { mouse: Some(Pos2::new(410.0, 300.0)), keys, ..Input::default() }, &view, &mut doc), Outcome::Going);
        assert_eq!(g.step(&Input { confirm: true, ..Input::default() }, &view, &mut doc), Outcome::Done);
        assert_eq!(doc.param("spot", "translate"), json!([3.0, 2.0, 3.0]));
        doc.commit(false);
        doc.undo();
        assert_eq!(doc.param("spot", "translate"), json!([1, 2, 3]));
    }

    #[test]
    fn selection_is_shared_with_the_node_graph() {
        let (s, _, mut doc) = picking_scene();
        let mut v = Viewport3d { scene: Some(Arc::new(s)), ..Viewport3d::default() };
        v.apply_hit(&mut doc, Some(Hit::Object("near".into())), false);
        assert_eq!(doc.selection(), vec!["near".to_string()]);
        v.apply_hit(&mut doc, Some(Hit::Object("cam".into())), true);
        assert!(doc.is_selected("near") && doc.is_selected("cam"));
        v.apply_hit(&mut doc, Some(Hit::Object("near".into())), true);
        assert_eq!(doc.selection(), vec!["cam".to_string()], "⇧-click takes it out");
        v.apply_hit(&mut doc, Some(Hit::Point("cloud".into(), 2)), true);
        v.apply_hit(&mut doc, Some(Hit::Point("cloud".into(), 0)), true);
        assert_eq!(v.points, vec![("cloud".to_string(), 2), ("cloud".to_string(), 0)]);
        // A: everything (objects as nodes, points by index); empty click: nothing.
        v.select_everything(&mut doc);
        let mut sel = doc.selection();
        sel.sort();
        assert_eq!(sel, vec!["cam", "far", "loc", "near"]);
        assert_eq!(v.points.len(), 3);
        v.apply_hit(&mut doc, None, false);
        assert!(doc.selection().is_empty() && v.points.is_empty());
    }

    #[test]
    fn the_view_saves_with_the_project_without_being_an_edit() {
        use arclight::graph::Graph;
        let mut doc = Doc::new(Graph::default(), None);
        let mut v = Viewport3d::default();
        v.restore(&doc);
        assert!(v.frame_first_scene, "nothing saved: frame the first scene");
        v.view.orbit(50.0, 10.0);
        let before = doc.graph.clone();
        v.save(&mut doc);
        assert_eq!(doc.graph, before, "not an edit");
        assert!(!doc.can_undo());
        // Saved and loaded back, a new viewport takes it.
        let loaded: Graph = serde_json::from_str(&doc.graph.to_json()).unwrap();
        let mut w = Viewport3d::default();
        w.restore(&Doc::new(loaded, Some("p.json".into())));
        assert_eq!((w.view, w.frame_first_scene), (v.view, false));
    }

    #[test]
    fn the_pie_points_by_direction() {
        let c = Pos2::new(100.0, 100.0);
        let at = |dx: f32, dy: f32| pie_slot(c, c + Vec2::new(dx, dy)).map(|k| PIE[k]);
        assert_eq!(at(5.0, 5.0), None, "dead zone");
        assert_eq!((at(60.0, 0.0), at(-60.0, 0.0)), (Some("Right"), Some("Left")));
        assert_eq!((at(0.0, -60.0), at(0.0, 60.0)), (Some("Top"), Some("Bottom")));
        assert_eq!((at(-40.0, -40.0), at(40.0, 40.0)), (Some("Front"), Some("Back")));
        let mut v = Viewport3d::default();
        v.pie_pick(PIE.iter().position(|p| *p == "Top").unwrap());
        assert!(v.view.ortho && v.view.pitch == 90.0);
    }

    #[test]
    fn points_are_colored_by_their_error() {
        assert_eq!(error_color(0.1), error_color(0.25), "good is green");
        let (good, poor) = (error_color(0.2), error_color(3.0));
        assert!(good.g() > good.r() && poor.r() > poor.g());
        let mid = error_color(1.0);
        assert!(mid.r() > 200 && mid.g() > 150, "about yellow at a pixel: {mid:?}");
    }

    /// A blurred view's particle streaks pass where the render's motion
    /// blur shows each particle at each shutter time (curving as it falls).
    #[test]
    fn particle_streaks_follow_the_render_s_shutter() {
        use arclight::graph::{Evaluator, Graph, NodeSpec};
        let node = |id: &str, op: &str, input: &str, params: serde_json::Value| NodeSpec { id: id.into(), op: op.into(), inputs: vec![input.into()], params, pos: None, disabled: false };
        let graph = Graph {
            nodes: vec![
                node("emit", "particle_emitter", "", json!({ "speed": 4, "translate": [0, 0, -8] })),
                node("fall", "particle_gravity", "emit", json!({ "strength": 20 })),
            ],
            ..Default::default()
        };
        let img = Evaluator::new().evaluate(&graph, "fall", &arclight::ops::EvalContext { frame: 6, write_outputs: false }).unwrap();
        let stream = &img.scene.as_deref().unwrap().particles[0];
        let (now, fps) = (stream.particles(), stream.fps());
        let pt = now.iter().find(|p| p.accel.norm() > 0.0 && p.velocity.norm() > 0.0).expect("a moving, falling particle");
        let times = [-0.25, 0.0, 0.25];
        let line = streak(pt, &times, fps);
        for (t, q) in times.iter().zip(&line) {
            let then = stream.shifted(*t).particles().into_iter().find(|p| p.id == pt.id).unwrap();
            assert!((then.position - q).norm() < 1e-9, "at {t}: {q:?} vs {:?}", then.position);
        }
        assert_eq!(line[1], pt.position);
        let bend = (line[0] + line[2]) / 2.0 - line[1];
        assert!(bend.norm() > 1e-6, "curves with the acceleration");
        assert!(streak(pt, &[], fps).is_empty(), "a dot when not blurred");
    }

    #[test]
    fn splat_crop_regions_are_drawn_picked_and_resized() {
        use crate::gizmo3d::{tests::front, GestureKey, Input, Mode, Outcome, Transforming};
        use arclight::graph::{Evaluator, Graph, NodeSpec};
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let node = |id: &str, op: &str, input: &str, params: serde_json::Value| NodeSpec { id: id.into(), op: op.into(), inputs: vec![input.into()], params, pos: None, disabled: false };
        let graph = Graph {
            nodes: vec![
                node("cap", "read_splat", "", json!({ "file": file, "translate": [0, 0, -12] })),
                node("crop", "splat_crop", "cap", json!({ "center": [3, 0, -8], "size": [2, 4, 6], "rotate": [0, 30, 0] })),
            ],
            ..Default::default()
        };
        let mut doc = Doc::new(graph, None);
        let scene = |doc: &Doc| {
            let img = Evaluator::new().evaluate(&doc.graph, "crop", &arclight::ops::EvalContext { frame: 1, write_outputs: false }).unwrap();
            img.scene.as_deref().unwrap().clone()
        };
        let s = scene(&doc);
        let gs = gadgets(&s);
        assert_eq!(gs.len(), 1);
        let g = &gs[0];
        assert!(matches!(g.kind, GadgetKind::Crop { sphere: false }) && g.node == "crop");
        // Its world is Center / Rotate / Size, as the gizmo sets them.
        assert!((g.world - geom::trs([3.0, 0.0, -8.0], [0.0, 30.0, 0.0], [2.0, 4.0, 6.0])).norm() < 1e-9);
        let lines = gadget_lines(g, 1.0);
        assert_eq!(lines.len(), 12, "a box's edges");
        let top = lines.iter().map(|(a, b)| a.y.max(b.y)).fold(f64::MIN, f64::max);
        assert!((top - 2.0).abs() < 1e-9, "4 high: {top}");
        let mut ball = s.clone();
        ball.regions[0].sphere = true;
        assert_eq!(gadget_lines(&gadgets(&ball)[0], 1.0).len(), 96, "three rings");
        // Picked by its middle; framed; the gizmo finds it.
        let (_, p, _) = picking_scene();
        let loc = |_: &str| false;
        let mid = p.project(Vec3::new(3.0, 0.0, -8.0)).unwrap().0;
        assert_eq!(pick(&s, &p, ASPECT, mid + Vec2::new(2.0, 1.0), &loc), Some(Hit::Object("crop".into())));
        assert!(bounds(&s, Some(&|n: &str| n == "crop")).is_some_and(|(c, _)| (c - Vec3::new(3.0, 0.0, -8.0)).norm() < 1e-9));
        let v = Viewport3d { scene: Some(Arc::new(s.clone())), ..Viewport3d::default() };
        assert_eq!(v.object_world("crop"), Some(g.world));
        // S X 2 Enter: twice as wide (its Size); G Y 1 Enter: up 1 (its Center).
        doc.commit(false);
        let view = front();
        let mut t = Transforming::start(&doc, "crop", Mode::Scale, &g.world, Pos2::new(400.0, 300.0), 1).unwrap();
        t.step(&Input { mouse: Some(Pos2::new(400.0, 300.0)), keys: vec![GestureKey::Axis(0, false), GestureKey::Digit('2')], ..Input::default() }, &view, &mut doc);
        assert_eq!(t.step(&Input { confirm: true, ..Input::default() }, &view, &mut doc), Outcome::Done);
        let size: Vec<f64> = serde_json::from_value(doc.param("crop", "size")).unwrap();
        assert!((size[0] - 4.0).abs() < 1e-9 && (size[1] - 4.0).abs() < 1e-9 && (size[2] - 6.0).abs() < 1e-9, "{size:?}");
        let mut m = Transforming::start(&doc, "crop", Mode::Move, &gadgets(&scene(&doc))[0].world, Pos2::new(400.0, 300.0), 1).unwrap();
        m.step(&Input { mouse: Some(Pos2::new(400.0, 300.0)), keys: vec![GestureKey::Axis(1, false), GestureKey::Digit('1')], ..Input::default() }, &view, &mut doc);
        m.step(&Input { confirm: true, ..Input::default() }, &view, &mut doc);
        let center: Vec<f64> = serde_json::from_value(doc.param("crop", "center")).unwrap();
        assert!((center[1] - 1.0).abs() < 1e-9 && (center[0] - 3.0).abs() < 1e-9, "{center:?}");
        assert_eq!(gadgets(&scene(&doc))[0].world.column(3).xyz(), Vec3::new(3.0, 1.0, -8.0), "the box follows");
    }

    #[test]
    fn particle_emitters_and_forces_are_drawn_picked_and_moved() {
        use crate::gizmo3d::{tests::front, GestureKey, Input, Mode, Outcome, Transforming};
        use arclight::graph::{Evaluator, Graph, NodeSpec};
        let node = |id: &str, op: &str, input: &str, params: serde_json::Value| NodeSpec { id: id.into(), op: op.into(), inputs: vec![input.into()], params, pos: None, disabled: false };
        let graph = Graph {
            nodes: vec![
                node("emit", "particle_emitter", "", json!({ "shape": "box", "dimensions": [1, 1, 1], "translate": [0, 0, -8] })),
                node("pull", "particle_attractor", "emit", json!({ "position": [2, 1, -8], "radius": 1 })),
                node("swirl", "particle_vortex", "pull", json!({ "center": [-2, 1, -8] })),
                node("floor", "particle_bounce", "swirl", json!({ "point": [0, -2, -8] })),
                node("fall", "particle_gravity", "floor", json!({})),
            ],
            ..Default::default()
        };
        let mut doc = Doc::new(graph, None);
        let scene = |doc: &Doc| {
            let img = Evaluator::new().evaluate(&doc.graph, "fall", &arclight::ops::EvalContext { frame: 1, write_outputs: false }).unwrap();
            img.scene.as_deref().unwrap().clone()
        };
        let s = scene(&doc);
        let gs = gadgets(&s);
        let at = |g: &Gadget| geom::transform_point(&g.world, &Vec3::zeros());
        assert_eq!(gs.iter().map(|g| g.node.as_str()).collect::<Vec<_>>(), ["emit", "pull", "swirl", "floor"], "gravity has nothing to show");
        assert_eq!(gs.iter().map(at).collect::<Vec<_>>(), [Vec3::new(0.0, 0.0, -8.0), Vec3::new(2.0, 1.0, -8.0), Vec3::new(-2.0, 1.0, -8.0), Vec3::new(0.0, -2.0, -8.0)]);
        assert_eq!(gadget_lines(&gs[0], 1.0).len(), 12 + 3, "a box's edges and the direction arrow");
        assert_eq!(gadget_lines(&gs[3], 1.0).len(), 4 + 3, "a square and the normal");
        // Picked by a click on (or near) each, framed with the rest.
        let (_, p, _) = picking_scene();
        let loc = |_: &str| false;
        for g in &gs {
            assert_eq!(pick(&s, &p, ASPECT, p.project(at(g)).unwrap().0 + Vec2::new(2.0, 1.0), &loc), Some(Hit::Object(g.node.clone())), "{}", g.node);
        }
        let (objects, _) = box_pick(&s, &p, Pos2::new(0.0, 0.0), Pos2::new(rect().center().x - 30.0, rect().max.y), &loc);
        assert_eq!(objects, ["swirl"]);
        assert!(bounds(&s, Some(&|n: &str| n == "floor")).is_some_and(|(c, _)| (c - at(&gs[3])).norm() < 1e-9));
        let v = Viewport3d { scene: Some(Arc::new(s.clone())), ..Viewport3d::default() };
        assert_eq!(v.object_world("pull"), Some(gs[1].world));
        // G X 2 Enter moves the attractor's position; it doesn't turn.
        doc.commit(false);
        let view = front();
        let mut g = Transforming::start(&doc, "pull", Mode::Move, &gs[1].world, Pos2::new(400.0, 300.0), 1).unwrap();
        let keys = vec![GestureKey::Axis(0, false), GestureKey::Digit('2')];
        g.step(&Input { mouse: Some(Pos2::new(410.0, 300.0)), keys, ..Input::default() }, &view, &mut doc);
        assert_eq!(g.step(&Input { confirm: true, ..Input::default() }, &view, &mut doc), Outcome::Done);
        assert_eq!(doc.param("pull", "position"), json!([4.0, 1.0, -8.0]));
        assert!(Transforming::start(&doc, "pull", Mode::Rotate, &gs[1].world, Pos2::ZERO, 1).err().unwrap().contains("no rotate"));
        // The emitter turns through its rotate, and the icon follows.
        let mut r = Transforming::start(&doc, "emit", Mode::Rotate, &gs[0].world, Pos2::new(400.0, 300.0), 1).unwrap();
        let keys = vec![GestureKey::Axis(1, false), GestureKey::Digit('9'), GestureKey::Digit('0')];
        r.step(&Input { mouse: Some(Pos2::new(400.0, 300.0)), keys, ..Input::default() }, &view, &mut doc);
        r.step(&Input { confirm: true, ..Input::default() }, &view, &mut doc);
        let turned = gadgets(&scene(&doc));
        assert!((turned[0].world.fixed_view::<3, 3>(0, 0) * Vec3::x() - Vec3::new(0.0, 0.0, -1.0)).norm() < 1e-9, "turned 90° about Y");
        assert_eq!(at(&turned[1]), Vec3::new(4.0, 1.0, -8.0));
    }
}
