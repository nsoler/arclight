//! The 3D viewport: the viewed node's scene from a free camera that belongs
//! to the viewer, with Blender-style navigation (the conventions only; no
//! Blender code), picking and selection shared with the node graph, and the
//! cards textured and meshes shaded by the CPU renderer on a worker thread
//! (meshes lit from the view, or drawn as wireframes).
//!
//! Navigation: middle-drag orbits (turntable: yaw about world Y, pitch
//! short of the poles), ⇧ + middle-drag pans, scroll or ⌃ + middle-drag
//! zooms toward the pointer; on a trackpad two-finger scroll orbits, ⇧ pans,
//! pinch or ⌃ zooms; with "emulate 3-button mouse", Alt + left-drag stands
//! in for the middle button. Keys (over the viewport, when no gizmo gesture
//! owns them): 1 / 3 / 7 front / right / top (⌃: the opposite side), 5
//! perspective / orthographic, 0 through the scene camera, 2 / 4 / 6 / 8
//! orbit 15°, . or F frame the selection, Home frame everything, A select
//! all, Alt+A or Esc deselect, Z solid / wireframe meshes, ` a pie of
//! views. (egui doesn't tell the numpad from the
//! number row, so both work.) A navigation gizmo in the top-right corner
//! does the same with the mouse.
//!
//! Arclight's world is Y up: "top" looks down −Y, "front" down −Z.

use crate::doc::Doc;
use crate::worker::{SceneRequest, SceneWorker};
use arclight::geom::{self, Mat4, Vec3};
use arclight::image::{Image, Size};
use arclight::scene::Scene;
use eframe::egui::{self, Align2, Color32, FontId, PointerButton, Pos2, Rect, Response, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use nalgebra::Rotation3;
use std::sync::Arc;

/// The free camera's lens: 35 mm on a 36 mm film back (about 54° across).
const FOCAL: f64 = 35.0;
const FILM: f64 = 36.0;
/// Degrees of orbit per pixel dragged.
const ORBIT_SPEED: f64 = 0.35;
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
}

impl Default for ViewCam {
    fn default() -> ViewCam {
        ViewCam { center: [0.0; 3], yaw: 30.0, pitch: 25.0, distance: 12.0, ortho: false, auto_ortho: false, wireframe: false }
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
        let f = cam.focal / cam.film_back[0].max(1e-9) * frame.width() as f64;
        Projector { world: cam.world, view: cam.view(), f, c: frame.center(), ortho: false, near: cam.near, rect, frame: Some(frame) }
    }

    /// `p` in camera space (looking down −Z).
    fn camera_space(&self, p: &Vec3) -> Vec3 {
        geom::transform_point(&self.view, p)
    }

    fn screen(&self, q: &Vec3) -> Pos2 {
        let s = if self.ortho { self.f } else { self.f / -q.z };
        Pos2::new((self.c.x as f64 + q.x * s) as f32, (self.c.y as f64 - q.y * s) as f32)
    }

    /// Where `p` lands on screen and how far in front of the camera it is;
    /// None behind (or, in perspective, too near).
    pub fn project(&self, p: Vec3) -> Option<(Pos2, f64)> {
        let q = self.camera_space(&p);
        let depth = -q.z;
        (self.ortho || depth >= self.near).then(|| (self.screen(&q), depth))
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
        if !self.ortho {
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

/// The scene's extent: a center and radius holding its cameras, cards and
/// points (None when it's empty).
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
    for (i, p) in scene.points.iter().enumerate() {
        if keep(scene.point_node(i)) {
            pts.push(*p);
        }
    }
    if pts.is_empty() {
        return None;
    }
    let (lo, hi) = pts.iter().fold((pts[0], pts[0]), |(lo, hi), p| (lo.inf(p), hi.sup(p)));
    let c = (lo + hi) / 2.0;
    Some((c, pts.iter().map(|p| (p - c).norm()).fold(0.0, f64::max).max(0.5)))
}

/// A camera's frustum corners at a depth showing its focal length, and its
/// apex: (apex, [four corners]).
fn frustum(cam: &geom::Camera, size: f64, aspect: f64) -> (Vec3, [Vec3; 4]) {
    let depth = size * cam.focal / cam.film_back[0].max(1e-9);
    let (w, h) = (size / 2.0, size / 2.0 / aspect.max(1e-3));
    let corner = |x: f64, y: f64| geom::transform_point(&cam.world, &Vec3::new(x, y, -depth));
    (geom::transform_point(&cam.world, &Vec3::zeros()), [corner(-w, h), corner(w, h), corner(w, -h), corner(-w, -h)])
}

/// What a click picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// A card, mesh, camera or locator: its node.
    Object(String),
    /// Point `.1` of the point cloud (or Camera Tracker) node `.0`.
    Point(String, usize),
}

/// How near (screen points) a click must be to a point, locator or camera.
const PICK_RADIUS: f32 = 7.0;

/// What's under screen point `s` in `scene` seen through `p`: the nearest
/// point, locator or camera within reach, else the nearest card or mesh
/// the ray through `s` hits. `is_locator` tells a locator's point from a
/// cloud's.
pub fn pick(scene: &Scene, p: &Projector, s: Pos2, is_locator: &dyn Fn(&str) -> bool) -> Option<Hit> {
    let mut best: Option<(f32, f64, Hit)> = None;
    let mut consider = |at: Vec3, hit: Hit| {
        if let Some((q, depth)) = p.project(at) {
            let d = q.distance(s);
            if d <= PICK_RADIUS && best.as_ref().is_none_or(|b| (d, depth) < (b.0, b.1)) {
                best = Some((d, depth, hit));
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
    if let Some((_, _, hit)) = best {
        return Some(hit);
    }
    // Cards (the nearest the ray hits inside its rectangle) and meshes.
    let (o, d) = p.ray(s);
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
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, n)| Hit::Object(n))
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
/// inside (cards, meshes, cameras, locators) and points.
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
    look_through: Option<usize>,
    pixels: [usize; 2],
}

/// The camera and frame size that render the cards for projector `p` at
/// `pixels`, matching what it draws: the scene camera through it; the free
/// camera; for orthographic, a long lens from far away (as good as flat).
fn render_camera(view: &ViewCam, p: &Projector, through: Option<&geom::Camera>, pixels: [usize; 2]) -> (geom::Camera, Size) {
    let format = Size { width: pixels[0].max(1), height: pixels[1].max(1) };
    if let Some(cam) = through {
        return (cam.clone(), format);
    }
    let film = [FILM, FILM * format.height as f64 / format.width as f64];
    if view.ortho {
        let far = 1000.0;
        let mut v = *view;
        v.distance *= far;
        let world = v.world();
        let cam = geom::Camera { focal: FOCAL * far, film_back: film, world, near: v.distance * 1e-3, far: v.distance * 10.0 };
        return (cam, format);
    }
    (geom::Camera { focal: FOCAL, film_back: film, world: view.world(), near: p.near, far: view.distance * 1e4 }, format)
}

/// The 3D viewport (see the module notes).
pub struct Viewport3d {
    pub view: ViewCam,
    /// Through scene camera `i` instead of the free camera.
    pub look_through: Option<usize>,
    /// Alt + left-drag navigates, as Blender's "emulate 3-button mouse".
    pub emulate_three_button: bool,
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
    /// The node whose scene is shown, if any.
    node: Option<String>,
    last: Option<Projector>,
    /// Camera nodes' paths.
    paths: Vec<CameraPath>,
    /// The textured cards, and where on screen they go.
    texture: Option<TextureHandle>,
    texture_rect: Option<Rect>,
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
}

impl Default for Viewport3d {
    fn default() -> Viewport3d {
        Viewport3d {
            view: ViewCam::default(),
            look_through: None,
            emulate_three_button: false,
            worker: None,
            scene: None,
            asked: None,
            graph: None,
            moved_at: 0.0,
            render_ms: 0.0,
            node: None,
            last: None,
            paths: Vec::new(),
            texture: None,
            texture_rect: None,
            error: None,
            points: Vec::new(),
            marquee: None,
            message: None,
            restored: None,
            frame_first_scene: false,
            pie: None,
            edges: Default::default(),
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
const SELECTED: Color32 = Color32::from_rgb(255, 160, 40);

// `projector`, `has_scene` and `object_world` are the seam for the
// transform gizmos (gizmo3d.rs) and the viewer.
#[allow(dead_code)]
impl Viewport3d {
    /// The view as of the last frame drawn (for the transform gizmos).
    pub fn projector(&self) -> Option<Projector> {
        self.last
    }

    /// Whether the viewed node shows a scene.
    pub fn has_scene(&self) -> bool {
        self.scene.is_some()
    }

    /// The world matrix of node `node`'s object in the viewed scene (a
    /// card, mesh, camera or locator; no parenting yet, so it's the node's own
    /// translate / rotate / scale).
    pub fn object_world(&self, node: &str) -> Option<Mat4> {
        let s = self.scene.as_ref()?;
        if let Some(c) = s.cards.iter().find(|c| c.node == node) {
            return Some(c.world);
        }
        if let Some(m) = s.meshes.iter().find(|m| m.node == node) {
            return Some(m.world);
        }
        if let Some(i) = (0..s.cameras.len()).find(|&i| s.camera_node(i) == node) {
            return Some(s.cameras[i].world);
        }
        let i = (0..s.points.len()).find(|&i| s.point_node(i) == node)?;
        Some(Mat4::new_translation(&s.points[i]))
    }

    /// The scene camera to look through (`look_through`, if it's still there).
    fn through_camera(&self) -> Option<&geom::Camera> {
        self.scene.as_ref()?.cameras.get(self.look_through?)
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
            self.look_through = None;
            self.texture_rect = None;
            self.node = node.clone();
        }
        let Some(node) = node else {
            self.asked = None;
            return;
        };
        let now = ctx.input(|i| i.time);
        let view_changed = self.asked.as_ref().is_some_and(|a| a.view != self.view || a.look_through != self.look_through);
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
        let ask = Ask { node: node.clone(), frame, revision: doc.revision, view: self.view, look_through: self.look_through, pixels };
        if self.asked.as_ref() == Some(&ask) {
            return;
        }
        if self.graph.as_ref().is_none_or(|(r, _)| *r != doc.revision) {
            self.graph = Some((doc.revision, Arc::new(doc.graph.clone())));
        }
        let graph = self.graph.as_ref().unwrap().1.clone();
        let render = self.scene.is_some().then(|| render_camera(&self.view, p, self.through_camera(), pixels));
        if let Some(w) = &self.worker {
            w.request(SceneRequest { graph, node, frame, render, solid: !self.view.wireframe, rect: Some(target) });
        }
        self.asked = Some(ask);
    }

    /// Draw the viewport into `rect` (whose response is `resp`) and handle
    /// its input. `blocked`: a transform gizmo or G / R / S gesture owns the
    /// left button and the keys; navigation still works.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &Response, doc: &mut Doc, frame: i64, blocked: bool) {
        self.restore(doc);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(38));
        self.navigate(ui, rect, resp, blocked);
        if !blocked && resp.hovered() {
            self.keys(ui, doc);
        }
        let p = match self.through_camera() {
            Some(cam) => Projector::through(cam, doc.graph.project.format, rect),
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
        if self.pie.is_some() {
            self.pie_ui(ui, &painter, blocked);
        } else if !blocked {
            self.select(ui, resp, doc, &p);
        } else {
            self.marquee = None;
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
        let emulated = self.emulate_three_button && !blocked && resp.dragged_by(PointerButton::Primary) && mods.alt;
        if middle || emulated {
            let d = resp.drag_delta();
            if mods.shift {
                self.leave_camera();
                self.view.pan(d, &p);
            } else if mods.command || mods.ctrl {
                let at = resp.interact_pointer_pos().unwrap_or(rect.center());
                self.view.zoom_at((d.y as f64 * 0.01).exp(), at, &p);
            } else {
                self.leave_camera();
                self.view.orbit(d.x as f64, d.y as f64);
            }
        }
        if !resp.hovered() {
            return;
        }
        let at = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
        for e in events {
            if let egui::Event::MouseWheel { unit, delta, source, modifiers, .. } = e {
                // A wheel zooms; trackpad fingers (and their momentum)
                // orbit, ⇧ pan, ⌃ zoom. When the platform doesn't say,
                // line steps mean a wheel.
                let wheel = match source {
                    egui::MouseWheelSource::Wheel => true,
                    egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum => false,
                    egui::MouseWheelSource::Unknown => unit != egui::MouseWheelUnit::Point,
                };
                if wheel {
                    let steps = if unit == egui::MouseWheelUnit::Point { delta.y as f64 / 50.0 } else { delta.y as f64 };
                    self.view.zoom_at(0.85f64.powf(steps), at, &p);
                } else if modifiers.command || modifiers.ctrl {
                    self.view.zoom_at((-delta.y as f64 * 0.01).exp(), at, &p);
                } else if modifiers.shift {
                    self.leave_camera();
                    self.view.pan(-delta, &p);
                } else {
                    self.leave_camera();
                    self.view.orbit(-delta.x as f64, -delta.y as f64);
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

    /// Take the view saved with a project when one is opened (or framing
    /// its first scene, when none was saved).
    fn restore(&mut self, doc: &Doc) {
        if self.restored.as_ref() == Some(&doc.path) {
            return;
        }
        self.restored = Some(doc.path.clone());
        match doc.graph.view.0.get("viewport3d").and_then(|v| serde_json::from_value::<ViewCam>(v.clone()).ok()) {
            Some(v) => {
                self.view = v;
                self.frame_first_scene = false;
            }
            None => {
                self.view = ViewCam::default();
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
        let Some(scene) = self.scene.clone() else { return };
        let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
        if self.emulate_three_button && alt {
            return;
        }
        if resp.clicked_by(PointerButton::Primary) {
            if let Some(s) = resp.interact_pointer_pos() {
                let hit = pick(&scene, p, s, &Self::locator(doc));
                self.apply_hit(doc, hit, shift);
            }
        }
        if resp.drag_started_by(PointerButton::Primary) {
            let start = ui.input(|i| i.pointer.press_origin());
            if let Some(o) = start.filter(|o| pick(&scene, p, *o, &Self::locator(doc)).is_none()) {
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
        let mut objects: Vec<String> = s.cards.iter().map(|c| c.node.clone()).chain(s.meshes.iter().map(|m| m.node.clone())).chain((0..s.cameras.len()).map(|i| s.camera_node(i).to_string())).collect();
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

    /// Leave a look-through for the free camera, where that camera was.
    fn leave_camera(&mut self) {
        let Some(cam) = self.through_camera().cloned() else { return };
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
        let (pressed, ctrl, alt) = ui.input(|i| {
            let keys = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Period, Key::Home, Key::F, Key::A, Key::Z, Key::Escape, Key::Backtick];
            (keys.into_iter().filter(|k| i.key_pressed(*k)).collect::<Vec<_>>(), i.modifiers.command || i.modifiers.ctrl, i.modifiers.alt)
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
                Key::Num0 => self.toggle_camera(),
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

    fn axis(&mut self, a: Axis) {
        self.look_through = None;
        self.view.view_axis(a);
    }

    /// Orbit by whole degrees (the 2 / 4 / 6 / 8 keys).
    fn step(&mut self, yaw: f64, pitch: f64) {
        self.leave_camera();
        self.view.orbit(-yaw / ORBIT_SPEED, -pitch / ORBIT_SPEED);
    }

    /// Look through the scene's first camera, or back out.
    pub fn toggle_camera(&mut self) {
        if self.look_through.is_some() {
            self.leave_camera();
        } else if self.scene.as_ref().is_some_and(|s| !s.cameras.is_empty()) {
            self.look_through = Some(0);
        }
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

    /// The grid, axes, cards, cameras, paths, points and locators.
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
            if self.scene.as_ref().is_some_and(|s| !s.cards.is_empty() || (!self.view.wireframe && !s.meshes.is_empty())) {
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
        let size = bounds(&scene, None).map_or(1.0, |(_, r)| (r * 0.08).max(0.15));
        let aspect = doc.graph.project.format.width as f64 / doc.graph.project.format.height.max(1) as f64;
        for (i, cam) in scene.cameras.iter().enumerate() {
            if self.look_through == Some(i) {
                continue;
            }
            let node = scene.camera_node(i);
            let color = if selected(node) { SELECTED } else { CAMERA };
            let stroke = Stroke::new(if selected(node) { 2.0 } else { 1.2 }, color);
            let (apex, k) = frustum(cam, size, aspect);
            for j in 0..4 {
                line(apex, k[j], stroke);
                line(k[j], k[(j + 1) % 4], stroke);
            }
            // A notch on the frame's top edge shows which way is up.
            let up = (k[0] + k[1]) / 2.0 + (k[0] - k[3]) * 0.2;
            line(k[0], up, stroke);
            line(up, k[1], stroke);
            self.draw_path(painter, p, doc, node, frame);
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
            let name = self.look_through.map(|i| scene.camera_node(i).to_string()).unwrap_or_default();
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
            self.view.orbit(d.x as f64, d.y as f64);
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
                "camera" => "Look through the scene camera (0)",
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

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 450.0))
    }

    fn close(a: Pos2, b: Pos2) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn projection_and_rays_agree() {
        for ortho in [false, true] {
            let v = ViewCam { center: [1.0, 0.5, -2.0], yaw: 40.0, pitch: 20.0, distance: 9.0, ortho, auto_ortho: false, wireframe: false };
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
    fn picking_scene() -> (Scene, Projector, Doc) {
        use arclight::graph::{Graph, NodeSpec};
        let tex = Arc::new(Image::new(4, 4));
        let card = |node: &str, z: f64, w: f64| arclight::scene::Card { image: tex.clone(), world: geom::trs([0.0, 0.0, z], [0.0; 3], [1.0; 3]), size: [w, w], node: node.into() };
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
        });
        let doc = Doc::new(Graph { nodes: nodes.collect(), ..Default::default() }, None);
        let p = ViewCam { center: [0.0, 0.0, -8.0], yaw: 0.0, pitch: 0.0, distance: 8.0, ortho: false, auto_ortho: false, wireframe: false }.projector(rect());
        (s, p, doc)
    }

    #[test]
    fn clicks_pick_points_objects_and_the_nearer_card() {
        let (s, p, doc) = picking_scene();
        let loc = |n: &str| doc.node(n).is_some_and(|n| n.op == "locator");
        let at = |x: Vec3| p.project(x).unwrap().0;
        // A click at (or near) a projected point picks it; a locator's
        // point picks the locator; a camera's apex picks the camera.
        assert_eq!(pick(&s, &p, at(s.points[1]) + Vec2::new(3.0, -2.0), &loc), Some(Hit::Point("cloud".into(), 1)));
        assert_eq!(pick(&s, &p, at(s.points[3]), &loc), Some(Hit::Object("loc".into())));
        assert_eq!(pick(&s, &p, at(Vec3::new(-2.5, -1.0, -7.0)), &loc), Some(Hit::Object("cam".into())));
        // The middle of the view is on both cards: the nearer wins.
        assert_eq!(pick(&s, &p, rect().center(), &loc), Some(Hit::Object("near".into())));
        // Past the near card's edge, on the bigger far one: the far one.
        assert_eq!(pick(&s, &p, at(Vec3::new(2.6, -2.6, -10.0)), &loc), Some(Hit::Object("far".into())));
        assert_eq!(pick(&s, &p, Pos2::new(5.0, 5.0), &loc), None);
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
        assert_eq!(pick(&s, &p, rect().center(), &loc), Some(Hit::Object("ball".into())));
        assert_eq!(pick(&s, &p, at(Vec3::new(0.6, 0.0, -4.0)), &loc), Some(Hit::Object("near".into())));
        assert_eq!(pick(&s, &p, at(Vec3::new(3.5, 2.5, -9.0)), &loc), Some(Hit::Object("crate".into())));
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
}
