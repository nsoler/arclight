//! The Camera Tracker: follow features through a shot, solve the camera
//! and a cloud of points from them ([`crate::features`], [`crate::sfm`]),
//! and keep the result on the node (its `solution` param) in the scene's
//! own terms: per frame, a Camera node's translate and rotate; points in
//! scene units, Y up (see [`crate::geom`]).
//!
//! The solve's scale and placement are arbitrary (its first camera at the
//! origin); [`Solution::set_origin`], [`Solution::set_ground`] and
//! [`Solution::set_scale`] move it to something meaningful, and
//! [`card_on_points`] / [`Solution::centroid`] place things on the points.

use crate::features::{FeatureTracker, Settings as TrackSettings};
use crate::geom::{self, Camera, Vec3};
use crate::image::{Image, Size};
use crate::render::Progress;
use crate::sfm;
use crate::solve::{Intrinsics, Pose};
use anyhow::{bail, Context, Result};
use nalgebra::{Matrix3, Matrix4, Rotation3};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;

/// How to track and solve, from a Camera Tracker's params.
#[derive(Clone, Debug)]
pub struct Settings {
    pub first: i64,
    pub last: i64,
    pub features: usize,
    pub min_distance: f32,
    /// Focal length (mm): known, or the starting guess with `solve_focal`.
    pub focal: f64,
    pub film_back: [f64; 2],
    pub solve_focal: bool,
    /// Pixels a track may miss by and still count.
    pub max_error: f64,
}

impl Settings {
    /// From a node's params (defaults filled in) and a frame range.
    pub fn from_params(p: &Value, first: i64, last: i64) -> Settings {
        let num = |k: &str, d: f64| p[k].as_f64().unwrap_or(d);
        Settings {
            first,
            last,
            features: num("features", 300.0).max(20.0) as usize,
            min_distance: num("min_distance", 12.0) as f32,
            focal: num("focal", 35.0),
            film_back: [p["film_back"][0].as_f64().unwrap_or(36.0), p["film_back"][1].as_f64().unwrap_or(24.0)],
            solve_focal: p["solve_focal"].as_bool().unwrap_or(false),
            max_error: num("max_error", 1.0),
        }
    }
}

/// A solved shot, as kept in a Camera Tracker's `solution` param.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Solution {
    /// The tracked frame size.
    pub format: [usize; 2],
    /// Focal length (mm, solved or as given) and film back (mm).
    pub focal: f64,
    pub film_back: [f64; 2],
    /// Solved frames, and on each a camera's translate and rotate
    /// (degrees, X then Y then Z), as a Camera node takes them.
    pub frames: Vec<i64>,
    pub cameras: Vec<[f64; 6]>,
    /// Solved points, scene units.
    pub points: Vec<[f64; 3]>,
    /// Root-mean-square reprojection error, pixels: overall and per frame.
    pub rms: f64,
    pub frame_rms: Vec<f64>,
    /// Each point's reprojection error (pixels), in the same order as
    /// `points` (empty in solves saved before it was kept).
    #[serde(default)]
    pub point_rms: Vec<f64>,
    /// Frames placed by perspective-n-point (after a jump, in a gap or past
    /// a cut) rather than from a neighbouring frame's camera.
    #[serde(default)]
    pub pnp_frames: Vec<i64>,
}

/// Computer-vision frame (y down, z forward) ↔ the scene's (y up, z back).
fn flip() -> Matrix3<f64> {
    Matrix3::from_diagonal(&Vec3::new(1.0, -1.0, -1.0))
}

impl Solution {
    fn from_sfm(sol: &sfm::Solution, film_back: [f64; 2], format: Size) -> Solution {
        let f = flip();
        let mut f4 = Matrix4::identity();
        f4.fixed_view_mut::<3, 3>(0, 0).copy_from(&f);
        let (frames, cameras): (Vec<i64>, Vec<[f64; 6]>) = sol
            .poses
            .iter()
            .map(|(&frame, pose)| {
                let w = f4 * pose.world();
                let r = Rotation3::from_matrix_unchecked(w.fixed_view::<3, 3>(0, 0).into_owned());
                let [rx, ry, rz] = geom::angles(&r);
                (frame, [w[(0, 3)], w[(1, 3)], w[(2, 3)], rx, ry, rz])
            })
            .unzip();
        let frame_rms = frames.iter().map(|f| sol.frame_rms.get(f).copied().unwrap_or(0.0)).collect();
        Solution {
            format: [format.width, format.height],
            focal: sol.intr.f / format.width as f64 * film_back[0],
            film_back,
            frames,
            cameras,
            points: sol.points.iter().flatten().map(|x| (f * x).into()).collect(),
            rms: sol.rms,
            frame_rms,
            point_rms: sol.points.iter().zip(&sol.point_rms).filter(|(p, _)| p.is_some()).map(|(_, e)| e.unwrap_or(0.0)).collect(),
            pnp_frames: sol.pnp.iter().copied().collect(),
        }
    }

    /// A Camera Tracker's solution, if it has one.
    pub fn from_params(p: &Value) -> Option<Solution> {
        serde_json::from_value(p.get("solution")?.clone()).ok()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("solution serializes")
    }

    /// The camera on `frame`, if it was solved.
    pub fn camera(&self, frame: i64) -> Option<Camera> {
        let i = self.frames.iter().position(|f| *f == frame)?;
        let c = self.cameras[i];
        Some(Camera { focal: self.focal, film_back: self.film_back, world: geom::trs([c[0], c[1], c[2]], [c[3], c[4], c[5]], [1.0; 3]), ..Camera::default() })
    }

    pub fn points(&self) -> Vec<Vec3> {
        self.points.iter().map(|p| Vec3::from(*p)).collect()
    }

    /// The middle of points `ids`.
    pub fn centroid(&self, ids: &[usize]) -> Option<Vec3> {
        let pts: Vec<Vec3> = ids.iter().filter_map(|&i| self.points.get(i).map(|p| Vec3::from(*p))).collect();
        (!pts.is_empty()).then(|| pts.iter().sum::<Vec3>() / pts.len() as f64)
    }

    /// Move, turn and scale the whole solve: every point x becomes
    /// `s r x + t`, and the cameras go along.
    pub fn transform(&mut self, s: f64, r: &Rotation3<f64>, t: &Vec3) {
        for p in &mut self.points {
            *p = (s * (r * Vec3::from(*p)) + t).into();
        }
        for c in &mut self.cameras {
            let center = s * (r * Vec3::new(c[0], c[1], c[2])) + t;
            let turn = geom::angles(&(r * geom::rotation([c[3], c[4], c[5]])));
            *c = [center.x, center.y, center.z, turn[0], turn[1], turn[2]];
        }
    }

    /// Put point `i` at the origin.
    pub fn set_origin(&mut self, i: usize) -> Result<()> {
        let p = self.centroid(&[i]).context("no such point")?;
        self.transform(1.0, &Rotation3::identity(), &-p);
        Ok(())
    }

    /// Turn the solve so the plane through points `ids` (three or more) is
    /// the ground: level, at height 0, with the cameras above it.
    pub fn set_ground(&mut self, ids: &[usize]) -> Result<()> {
        let pts: Vec<Vec3> = ids.iter().filter_map(|&i| self.points.get(i).map(|p| Vec3::from(*p))).collect();
        let (center, mut normal) = fit_plane(&pts).context("pick three or more points that aren't in a line")?;
        let cams: Vec<Vec3> = self.cameras.iter().map(|c| Vec3::new(c[0], c[1], c[2])).collect();
        let above = cams.iter().sum::<Vec3>() / cams.len().max(1) as f64 - center;
        if normal.dot(&above) < 0.0 {
            normal = -normal;
        }
        let r = Rotation3::rotation_between(&normal, &Vec3::y()).unwrap_or_else(|| Rotation3::from_axis_angle(&Vec3::x_axis(), std::f64::consts::PI));
        // Turned about the plane's center, then lowered onto y = 0.
        let c = r * center;
        self.transform(1.0, &r, &Vec3::new(0.0, -c.y, 0.0));
        Ok(())
    }

    /// Scale the solve (about the origin) so points `a` and `b` are
    /// `distance` apart.
    pub fn set_scale(&mut self, a: usize, b: usize, distance: f64) -> Result<()> {
        let (pa, pb) = (self.centroid(&[a]).context("no such point")?, self.centroid(&[b]).context("no such point")?);
        let d = (pa - pb).norm();
        if d < 1e-12 || distance <= 0.0 {
            bail!("pick two different points and a distance above 0");
        }
        self.transform(distance / d, &Rotation3::identity(), &Vec3::zeros());
        Ok(())
    }
}

/// The plane best fitting `pts` (three or more, not all in a line): a point
/// on it and its unit normal.
pub fn fit_plane(pts: &[Vec3]) -> Option<(Vec3, Vec3)> {
    if pts.len() < 3 {
        return None;
    }
    let c = pts.iter().sum::<Vec3>() / pts.len() as f64;
    let cov = pts.iter().fold(Matrix3::zeros(), |m, p| m + (p - c) * (p - c).transpose());
    let e = nalgebra::SymmetricEigen::new(cov);
    let mut order = [0, 1, 2];
    order.sort_by(|&a, &b| e.eigenvalues[a].total_cmp(&e.eigenvalues[b]));
    // In a line: the middle spread is no bigger than the smallest.
    if e.eigenvalues[order[1]] <= 1e-9 * e.eigenvalues[order[2]].max(1e-30) {
        return None;
    }
    Some((c, e.eigenvectors.column(order[0]).normalize()))
}

/// A Card's translate, rotate and size to lie on points `ids` of `sol`:
/// at their middle, on the plane through them (three or more) facing the
/// camera of `frame`, else square to that camera; as big as the points'
/// spread (and a little more), or for one or two points a size that looks
/// reasonable from the camera.
pub fn card_on_points(sol: &Solution, ids: &[usize], frame: i64) -> Result<Map<String, Value>> {
    let pts: Vec<Vec3> = ids.iter().filter_map(|&i| sol.points.get(i).map(|p| Vec3::from(*p))).collect();
    if pts.is_empty() {
        bail!("pick some solved points");
    }
    let cam = sol.camera(frame).or_else(|| sol.frames.first().and_then(|&f| sol.camera(f))).context("nothing solved")?;
    let eye = geom::transform_point(&cam.world, &Vec3::zeros());
    let center = pts.iter().sum::<Vec3>() / pts.len() as f64;
    let toward = (eye - center).normalize();
    let mut normal = fit_plane(&pts).map_or(toward, |(_, n)| n);
    if normal.dot(&toward) < 0.0 {
        normal = -normal;
    }
    // The card's x along the camera's right, flattened onto the plane.
    let right = cam.world.fixed_view::<3, 1>(0, 0).into_owned();
    let mut x = right - normal * right.dot(&normal);
    if x.norm() < 1e-9 {
        x = normal.cross(&Vec3::y());
    }
    let x = x.normalize();
    let y = normal.cross(&x);
    let r = Rotation3::from_matrix_unchecked(Matrix3::from_columns(&[x, y, normal]));
    let spread = |axis: &Vec3| pts.iter().map(|p| (p - center).dot(axis)).fold((0.0f64, 0.0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let ((x0, x1), (y0, y1)) = (spread(&x), spread(&y));
    let floor = 0.05 * (eye - center).norm();
    let size = [((x1 - x0) * 1.2).max(floor), ((y1 - y0) * 1.2).max(floor)];
    let mut out = Map::new();
    out.insert("translate".into(), json!([center.x, center.y, center.z]));
    out.insert("rotate".into(), json!(geom::angles(&r)));
    out.insert("size".into(), json!(size));
    Ok(out)
}

/// Track the features of frames `s.first..=s.last` (from `frames`) and solve
/// the camera. `progress` counts tracked frames, then solved ones (out of
/// twice the frame count) and is checked for a cancel.
pub fn run(frames: &mut dyn FnMut(i64) -> Result<Image>, s: &Settings, progress: &Progress) -> Result<Solution> {
    if s.last <= s.first {
        bail!("the frame range needs at least two frames");
    }
    let n = s.last - s.first + 1;
    progress.total.store(2 * n, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut tracker = FeatureTracker::new(TrackSettings { max_features: s.features, min_distance: s.min_distance, ..TrackSettings::default() });
    let mut format = None;
    for f in s.first..=s.last {
        if progress.cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let img = frames(f).with_context(|| format!("frame {f}"))?;
        format.get_or_insert(img.format);
        tracker.push(f, &img);
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    let format = format.unwrap();
    let intr = Intrinsics::new(s.focal, s.film_back[0], format);
    let settings = sfm::Settings { max_error: s.max_error, solve_focal: s.solve_focal, ..sfm::Settings::default() };
    let sol = sfm::solve(&tracker.tracks, s.first, s.last, intr, &settings, &mut |frac| {
        progress.done.store(n + (frac * n as f32) as i64, Ordering::Relaxed);
        !progress.cancel.load(Ordering::Relaxed)
    })?;
    progress.done.store(2 * n, Ordering::Relaxed);
    Ok(Solution::from_sfm(&sol, s.film_back, format))
}

/// A Camera node's params, keyed on every solved frame (linear), from
/// `sol`.
pub fn camera_params(sol: &Solution) -> Map<String, Value> {
    let keys = |k: usize| -> Value {
        let keys: Vec<Value> = sol.frames.iter().zip(&sol.cameras).map(|(f, c)| json!([f, [c[k], c[k + 1], c[k + 2]]])).collect();
        json!({ "keys": keys, "interp": "linear" })
    };
    let mut out = Map::new();
    out.insert("focal".into(), json!(sol.focal));
    out.insert("film_back".into(), json!(sol.film_back));
    out.insert("translate".into(), keys(0));
    out.insert("rotate".into(), keys(3));
    out
}

/// The world point a solved pose (computer-vision frame) has as its
/// center, in scene terms: for tests comparing against a solve.
pub fn scene_center(p: &Pose) -> Vec3 {
    flip() * p.center()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tilted() -> Solution {
        // Points on a tilted plane below two cameras.
        let r = geom::rotation([20.0, 0.0, 10.0]);
        let points: Vec<[f64; 3]> = (0..25).map(|i| (r * Vec3::new((i % 5) as f64 - 2.0, 0.0, (i / 5) as f64 - 2.0) + Vec3::new(1.0, -3.0, -8.0)).into()).collect();
        Solution {
            format: [960, 540],
            focal: 30.0,
            film_back: [36.0, 24.0],
            frames: vec![1, 2],
            cameras: vec![[0.0, 0.0, 0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0, 5.0, 0.0]],
            points,
            rms: 0.3,
            frame_rms: vec![0.3, 0.3],
            point_rms: Vec::new(),
            pnp_frames: Vec::new(),
        }
    }

    #[test]
    fn origin_ground_and_scale_reorient_the_solve() {
        let mut s = tilted();
        // A point seen by the camera before lands in the same pixel after.
        let probe = |s: &Solution, i: usize| s.camera(2).unwrap().project(&Vec3::from(s.points[i]), Size { width: 960, height: 540 }).unwrap().0;
        let before = probe(&s, 7);
        let ids: Vec<usize> = (0..25).collect();
        s.set_ground(&ids).unwrap();
        assert!(s.points.iter().all(|p| p[1].abs() < 1e-9), "level at 0");
        assert!(s.cameras.iter().all(|c| c[1] > 0.0), "cameras above");
        s.set_origin(12).unwrap();
        assert!(Vec3::from(s.points[12]).norm() < 1e-9);
        s.set_scale(0, 4, 10.0).unwrap();
        assert!(((Vec3::from(s.points[0]) - Vec3::from(s.points[4])).norm() - 10.0).abs() < 1e-9);
        let after = probe(&s, 7);
        assert!((before[0] - after[0]).abs() < 1e-6 && (before[1] - after[1]).abs() < 1e-6, "{before:?} {after:?}");
        assert!(fit_plane(&[Vec3::zeros(), Vec3::x(), Vec3::x() * 2.0]).is_none(), "a line has no plane");
    }

    #[test]
    fn cards_lie_on_their_points() {
        let s = tilted();
        let ids = [6, 7, 8, 11, 12, 13];
        let p = card_on_points(&s, &ids, 1).unwrap();
        let t = Vec3::from(crate::scene::numbers::<3>(&p["translate"], 0.0));
        let r = geom::rotation(crate::scene::numbers::<3>(&p["rotate"], 0.0));
        let normal = r * Vec3::z();
        // In the middle of the points, on their plane, facing the camera.
        assert!((t - s.centroid(&ids).unwrap()).norm() < 1e-9);
        for &i in &ids {
            assert!((Vec3::from(s.points[i]) - t).dot(&normal).abs() < 1e-9);
        }
        assert!(normal.dot(&(Vec3::zeros() - t)) > 0.0);
        // Big enough to hold every point, and not much more (they spread
        // over 2 × 1 on the plane).
        let size = crate::scene::numbers::<2>(&p["size"], 0.0);
        for &i in &ids {
            let d = r.inverse() * (Vec3::from(s.points[i]) - t);
            assert!(d.x.abs() <= size[0] / 2.0 && d.y.abs() <= size[1] / 2.0, "{d} outside {size:?}");
        }
        assert!(size[0] * size[1] < 2.0 * 1.2 * 1.2 * 1.6, "{size:?}");
        // One point: square to the camera.
        let p = card_on_points(&s, &[3], 1).unwrap();
        let n = geom::rotation(crate::scene::numbers::<3>(&p["rotate"], 0.0)) * Vec3::z();
        let toward = (Vec3::zeros() - Vec3::from(s.points[3])).normalize();
        assert!((n - toward).norm() < 1e-9);
    }

    /// The whole node workflow on a rendered shot: track and solve a plate
    /// (textured cards seen by a moving camera, rendered through the graph),
    /// export the camera, and place a card on solved points on the floor:
    /// it stays on the same spot of the floor in every frame, and renders
    /// there over the plate.
    #[test]
    fn a_card_placed_on_solved_points_stays_locked_to_the_plate() {
        use crate::session::Session;
        let dir = std::env::temp_dir().join(format!("arclight_camtrack_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tex: Vec<String> = (0..4)
            .map(|i| {
                let path = dir.join(format!("tex{i}.exr"));
                crate::features::tests::blob_texture(2500, 768, 768, 31 + i).write(&path).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let keys = |a: [f64; 3], b: [f64; 3]| json!({ "keys": [[1, a], [24, b]], "interp": "linear" });
        let graph: crate::graph::Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 24, "fps": 24, "format": { "width": 480, "height": 270 } },
            "nodes": [
                { "id": "t0", "op": "read", "params": { "path": tex[0] } },
                { "id": "t1", "op": "read", "params": { "path": tex[1] } },
                { "id": "t2", "op": "read", "params": { "path": tex[2] } },
                { "id": "t3", "op": "read", "params": { "path": tex[3] } },
                { "id": "wall", "op": "card", "inputs": ["t0"], "params": { "translate": [0, 3, -16], "size": [36, 24] } },
                { "id": "floor", "op": "card", "inputs": ["t1"], "params": { "translate": [0, -1.5, -10], "rotate": [-90, 0, 0], "size": [30, 24] } },
                { "id": "panel1", "op": "card", "inputs": ["t2"], "params": { "translate": [-2.5, 1.5, -7], "rotate": [0, 25, 0], "size": [3, 3] } },
                { "id": "panel2", "op": "card", "inputs": ["t3"], "params": { "translate": [3, 1.5, -9], "rotate": [0, -20, 0], "size": [3, 3] } },
                { "id": "truecam", "op": "camera", "params": { "focal": 24, "translate": keys([0.0, 0.0, 0.0], [1.8, 0.4, -0.8]), "rotate": keys([0.0, 0.0, 0.0], [-2.0, 6.0, 0.0]) } },
                { "id": "set", "op": "scene", "inputs": ["wall", "floor", "panel1", "panel2"] },
                { "id": "plate", "op": "scanline_render", "inputs": ["set", "truecam", ""], "params": { "samples": 2 } },
                { "id": "ct", "op": "camera_tracker", "inputs": ["plate"], "params": { "focal": 24 } },
            ] })).unwrap();
        let mut s = Session::new(graph);
        let summary = s.track_camera("ct", &Progress::default()).unwrap();
        assert_eq!(summary["frames_solved"], 24, "{summary}");
        assert!(summary["rms_error_px"].as_f64().unwrap() < 0.3, "{summary}");
        let sol = s.camera_solution("ct").unwrap();
        assert_eq!(sol.point_rms.len(), sol.points.len(), "an error per point");
        let fmt = Size { width: 480, height: 270 };
        // Solved points below row 175 of frame 1: all on the floor (it meets
        // the wall at about row 165).
        let cam1 = sol.camera(1).unwrap();
        let floor: Vec<usize> = (0..sol.points.len())
            .filter(|&i| cam1.project(&Vec3::from(sol.points[i]), fmt).is_some_and(|(q, _)| q[1] > 175.0 && q[0] > 60.0 && q[0] < 420.0))
            .collect();
        assert!(floor.len() >= 6, "{} floor points", floor.len());
        let red = s.create_node("constant", Some("red"), &serde_json::from_value(json!({ "color": [1, 0, 0, 1], "width": 64, "height": 64 })).unwrap(), &[]).unwrap();
        let card = s.place_on_points("ct", &floor, "card", 1, Some(&red), Some("marker")).unwrap();
        let p = s.node(&card).unwrap().params.clone();
        let center = Vec3::from(crate::scene::numbers::<3>(&p["translate"], 0.0));
        // Where the card's center is in each frame through the solve, and
        // where the floor spot under it at frame 1 really goes.
        let truth = |f: i64| {
            let t = (f - 1) as f64 / 23.0;
            let lerp = |a: f64, b: f64| a + (b - a) * t;
            Camera { focal: 24.0, world: geom::trs([lerp(0.0, 1.8), lerp(0.0, 0.4), lerp(0.0, -0.8)], [lerp(0.0, -2.0), lerp(0.0, 6.0), 0.0], [1.0; 3]), ..Camera::default() }
        };
        let p1 = cam1.project(&center, fmt).unwrap().0;
        let (c1, eye) = (truth(1), geom::transform_point(&truth(1).world, &Vec3::zeros()));
        let ray = c1.unproject(p1, 1.0, fmt) - eye;
        let spot = eye + ray * ((-1.5 - eye.y) / ray.y);
        let mut worst = 0.0f64;
        for f in 1..=24 {
            let a = sol.camera(f).unwrap().project(&center, fmt).unwrap().0;
            let b = truth(f).project(&spot, fmt).unwrap().0;
            worst = worst.max((a[0] - b[0]).hypot(a[1] - b[1]));
        }
        assert!(worst < 1.0, "the card drifts {worst} px off its floor spot");
        // Exported and rendered over the plate (the solved points and the
        // marker, shrunk to a small square), the marker shows there.
        let away = (geom::transform_point(&cam1.world, &Vec3::zeros()) - center).norm();
        let mut small = serde_json::Map::new();
        small.insert("size".into(), json!([0.05 * away, 0.05 * away]));
        s.set_params(&card, &small).unwrap();
        let names = s.export_camera("ct").unwrap();
        let (scene, render) = (names["scene"].as_str().unwrap().to_string(), names["render"].as_str().unwrap().to_string());
        s.connect(&card, &scene, 2).unwrap();
        for f in [1, 24] {
            let img = crate::graph::Evaluator::new().evaluate(&s.graph, &render, &crate::ops::EvalContext { frame: f, write_outputs: false }).unwrap();
            let (mut sum, mut n) = ([0.0, 0.0], 0.0);
            for y in 0..270 {
                for x in 0..480 {
                    let px = img.at(x, y);
                    if px[0] > 0.9 && px[1] < 0.05 && px[2] < 0.05 {
                        sum = [sum[0] + x as f64 + 0.5, sum[1] + y as f64 + 0.5];
                        n += 1.0;
                    }
                }
            }
            assert!(n > 20.0, "frame {f}: marker not seen");
            let want = truth(f).project(&spot, fmt).unwrap().0;
            let got = [sum[0] / n, sum[1] / n];
            assert!((got[0] - want[0]).hypot(got[1] - want[1]) < 2.0, "frame {f}: marker at {got:?}, floor spot at {want:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
