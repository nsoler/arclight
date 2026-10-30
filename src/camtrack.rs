//! The Camera Tracker: follow features through a shot, solve the camera
//! and a cloud of points from them ([`crate::features`], [`crate::sfm`]),
//! and keep the result on the node (its `solution` param) in the scene's
//! own terms: per frame, a Camera node's translate and rotate; points in
//! scene units, Y up (see [`crate::geom`]).
//!
//! The solve is [`sfm`]'s for a camera that moves, or [`nodal`]'s for one
//! that only turns (a tripod shot: a camera fixed at one place, its points
//! on a sphere around it, as they have no depth); [`Mode`] picks which.
//!
//! The solve's scale and placement are arbitrary (its first camera at the
//! origin); [`Solution::set_origin`], [`Solution::set_ground`] and
//! [`Solution::set_scale`] move it to something meaningful, and
//! [`card_on_points`] / [`Solution::centroid`] place things on the points.

use crate::features::{Blocked, FeatureTracker, Settings as TrackSettings};
use crate::graph::{Evaluator, Graph};
use crate::ops::EvalContext;
use crate::geom::{self, Camera, Vec3};
use crate::image::{Image, Size};
use crate::render::Progress;
use crate::nodal;
use crate::sfm;
use crate::solve::{Intrinsics, Pose};
use anyhow::{bail, Context, Result};
use nalgebra::{Matrix3, Matrix4, Rotation3};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Which solve to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Free move, or Tripod when no two frames have parallax to start
    /// from and the camera is found to only turn.
    Auto,
    Free,
    Tripod,
}

impl Mode {
    /// From a `solve_mode` param or a tool argument: "auto", "free move"
    /// (or "free") or "tripod".
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "auto" => Some(Mode::Auto),
            "free move" | "free" => Some(Mode::Free),
            "tripod" => Some(Mode::Tripod),
            _ => None,
        }
    }

    /// Its `solve_mode` param value.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Free => "free move",
            Mode::Tripod => "tripod",
        }
    }
}

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
    /// Solve the lens's radial distortion too (not on a tripod).
    pub solve_distortion: bool,
    /// Pixels a track may miss by and still count.
    pub max_error: f64,
    pub mode: Mode,
    /// How far from a tripod solve's camera its points go.
    pub distance: f64,
    /// How to read a connected mask (where it covers, nothing is
    /// tracked): one of [`crate::ops::MASK_CHANNELS`], and whether it's
    /// flipped.
    pub mask_channel: String,
    pub mask_invert: bool,
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
            solve_distortion: p["solve_distortion"].as_bool().unwrap_or(false),
            max_error: num("max_error", 1.0),
            mode: p["solve_mode"].as_str().and_then(Mode::parse).unwrap_or(Mode::Auto),
            distance: num("distance", 10.0).max(1e-3),
            mask_channel: p["mask_channel"].as_str().unwrap_or("alpha").to_string(),
            mask_invert: p["mask_invert"].as_bool().unwrap_or(false),
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
    /// A tripod solve: the camera only turns, staying where it is, and the
    /// points lie on a sphere around it.
    #[serde(default)]
    pub tripod: bool,
    /// The lens's radial distortion (solved, else 0), as Lens Distort's K1
    /// and K2 about the frame's center.
    #[serde(default)]
    pub k1: f64,
    #[serde(default)]
    pub k2: f64,
}

/// Computer-vision frame (y down, z forward) ↔ the scene's (y up, z back).
fn flip() -> Matrix3<f64> {
    Matrix3::from_diagonal(&Vec3::new(1.0, -1.0, -1.0))
}

impl Solution {
    fn from_sfm(sol: &sfm::Solution, film_back: [f64; 2], format: Size, tripod: bool) -> Solution {
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
            tripod,
            k1: sol.intr.k1,
            k2: sol.intr.k2,
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

    /// Scale the solve so points `a` and `b` are `distance` apart: about
    /// the origin, or for a tripod solve about its camera (which stays put:
    /// only the sphere of points grows or shrinks).
    pub fn set_scale(&mut self, a: usize, b: usize, distance: f64) -> Result<()> {
        let (pa, pb) = (self.centroid(&[a]).context("no such point")?, self.centroid(&[b]).context("no such point")?);
        let d = (pa - pb).norm();
        if d < 1e-12 || distance <= 0.0 {
            bail!("pick two different points and a distance above 0");
        }
        let s = distance / d;
        let about = match (self.tripod, self.cameras.first()) {
            (true, Some(c)) => Vec3::new(c[0], c[1], c[2]),
            _ => Vec3::zeros(),
        };
        self.transform(s, &Rotation3::identity(), &(about * (1.0 - s)));
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

/// A frame to track, and its mask's frame when one is connected.
pub type Frame = (Image, Option<Arc<Image>>);

/// Camera Tracker `node`'s input frames ([`crate::track::input_frames`]),
/// each with its mask input's, evaluated on the same frame.
pub fn input_frames(graph: &Graph, node: &str) -> Result<impl FnMut(i64) -> Result<Frame> + use<>> {
    let mut plate = crate::track::input_frames(graph, node)?;
    let mask = graph.node(node).and_then(|n| n.inputs.get(1)).filter(|m| !m.is_empty()).cloned();
    let graph = graph.clone();
    let mut eval = Evaluator::new();
    Ok(move |frame| {
        let img = plate(frame)?;
        let ctx = EvalContext { frame, write_outputs: false };
        let mask = mask.as_ref().map(|m| eval.evaluate(&graph, m, &ctx).with_context(|| format!("the mask {m}"))).transpose()?;
        Ok((img, mask))
    })
}

/// The pixels of a `format` frame that `mask` covers (more than half).
fn blocked(mask: &Image, format: Size, s: &Settings) -> Blocked {
    Blocked::new(format.width, format.height, |x, y| crate::ops::mask_coverage(mask, &s.mask_channel, s.mask_invert, x as i32, y as i32) > 0.5)
}

/// Track the features of frames `s.first..=s.last` (from `frames`; none
/// where their masks cover) and solve the camera. `progress` counts
/// tracked frames, then solved ones (out of twice the frame count) and is
/// checked for a cancel.
pub fn run(frames: &mut dyn FnMut(i64) -> Result<Frame>, s: &Settings, progress: &Progress) -> Result<Solution> {
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
        let (img, mask) = frames(f).with_context(|| format!("frame {f}"))?;
        format.get_or_insert(img.format);
        tracker.push_masked(f, &img, mask.map(|m| blocked(&m, img.format, s)).as_ref());
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    let format = format.unwrap();
    let intr = Intrinsics::new(s.focal, s.film_back[0], format);
    let settings = sfm::Settings { max_error: s.max_error, solve_focal: s.solve_focal, solve_distortion: s.solve_distortion, ..sfm::Settings::default() };
    let mut solving = |frac: f32| {
        progress.done.store(n + (frac * n as f32) as i64, Ordering::Relaxed);
        !progress.cancel.load(Ordering::Relaxed)
    };
    let (sol, tripod) = solve(&tracker.tracks, s, intr, &settings, &mut solving)?;
    progress.done.store(2 * n, Ordering::Relaxed);
    Ok(Solution::from_sfm(&sol, s.film_back, format, tripod))
}

/// Solve `tracks` as `s.mode` says, and whether that was a tripod solve.
fn solve(tracks: &[crate::features::FeatureTrack], s: &Settings, intr: Intrinsics, settings: &sfm::Settings, progress: &mut dyn FnMut(f32) -> bool) -> Result<(sfm::Solution, bool)> {
    if s.mode == Mode::Tripod {
        return Ok((nodal::solve(tracks, s.first, s.last, intr, settings, s.distance, progress)?, true));
    }
    match sfm::solve(tracks, s.first, s.last, intr, settings, progress) {
        Err(e) if e.downcast_ref::<sfm::NoParallax>().is_some() && nodal::detect(tracks, s.first, s.last, &intr, settings).nodal => {
            if s.mode == Mode::Auto {
                Ok((nodal::solve(tracks, s.first, s.last, intr, settings, s.distance, progress)?, true))
            } else {
                bail!("the camera only turns about its center, so a free-move solve has no parallax to start from: set Solve to Tripod")
            }
        }
        r => Ok((r?, false)),
    }
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
    // A tripod solve's camera doesn't move.
    let still = sol.tripod.then(|| sol.cameras.first()).flatten();
    out.insert("translate".into(), still.map_or_else(|| keys(0), |c| json!([c[0], c[1], c[2]])));
    out.insert("rotate".into(), keys(3));
    out
}

/// The Camera params a live link follows on a solve.
pub const SOLVED: [&str; 4] = ["translate", "rotate", "focal", "film_back"];

/// A Camera node's params following Camera Tracker `node`'s solve live
/// ([`crate::link::solve`]): re-solving moves it too.
pub fn camera_links(node: &str) -> Map<String, Value> {
    SOLVED.iter().map(|what| (what.to_string(), crate::link::solve(node, what))).collect()
}

/// Camera Tracker params `p`'s solve as a Camera's `what` (one of
/// [`SOLVED`]) at frame `t`: between solved frames, linear as the keys of
/// [`camera_params`] are (so motion blur sees the same), held before the
/// first and after the last.
pub fn solved_value(p: &Value, what: &str, t: f64) -> Result<Value> {
    let s = p.get("solution").filter(|s| s.is_object()).context("it has no solve yet: track it first")?;
    let k = match what {
        "focal" | "film_back" => return Ok(s[what].clone()),
        "translate" => 0,
        "rotate" => 3,
        other => bail!("a solve has no '{other}' (only {})", SOLVED.join(", ")),
    };
    let frames: Vec<i64> = serde_json::from_value(s["frames"].clone()).context("a broken solve")?;
    let cameras: Vec<[f64; 6]> = serde_json::from_value(s["cameras"].clone()).context("a broken solve")?;
    let n = frames.len().min(cameras.len());
    if n == 0 {
        bail!("its solve has no frames");
    }
    let at = |i: usize| [cameras[i][k], cameras[i][k + 1], cameras[i][k + 2]];
    // A tripod solve's camera doesn't move.
    let still = k == 0 && s["tripod"].as_bool() == Some(true);
    let v = if still || n == 1 || t <= frames[0] as f64 {
        at(0)
    } else if t >= frames[n - 1] as f64 {
        at(n - 1)
    } else {
        let i = frames[..n].partition_point(|&f| f as f64 <= t) - 1;
        let u = (t - frames[i] as f64) / (frames[i + 1] - frames[i]) as f64;
        let (a, b) = (at(i), at(i + 1));
        std::array::from_fn(|d| a[d] + (b[d] - a[d]) * u)
    };
    Ok(json!(v))
}

/// Camera param `what` keyed from Camera Tracker params `p`'s solve, as
/// [`camera_params`] gives it: what a linked one bakes to.
pub fn baked_value(p: &Value, what: &str) -> Option<Value> {
    camera_params(&Solution::from_params(p)?).remove(what)
}

/// A Lens Distort node's params undistorting the plate as `sol`'s lens
/// bends it (Distort with the same values bends CG to match), or None for
/// a perfect lens.
pub fn lens_params(sol: &Solution) -> Option<Map<String, Value>> {
    (sol.k1 != 0.0 || sol.k2 != 0.0).then(|| {
        let mut out = Map::new();
        out.insert("mode".into(), json!("undistort"));
        out.insert("k1".into(), json!(sol.k1));
        out.insert("k2".into(), json!(sol.k2));
        out
    })
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
            tripod: false,
            k1: 0.0,
            k2: 0.0,
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
        let summary = s.track_camera("ct", None, None, &Progress::default()).unwrap();
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

    /// A rendered shot with a card moving across it in front of the set:
    /// masked out (by a render of the card alone, frame by frame), no
    /// feature is ever on it and the camera solves as well as without it.
    #[test]
    fn a_masked_moving_object_is_left_out_of_the_solve() {
        use crate::session::Session;
        let dir = std::env::temp_dir().join(format!("arclight_camtrack_mask_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tex: Vec<String> = (0..4)
            .map(|i| {
                let path = dir.join(format!("tex{i}.exr"));
                crate::features::tests::blob_texture(2500, 768, 768, 51 + i).write(&path).unwrap();
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
                { "id": "panel", "op": "card", "inputs": ["t2"], "params": { "translate": [-2.5, 1.5, -7], "rotate": [0, 25, 0], "size": [3, 3] } },
                // Crossing the frame, turning as it goes.
                { "id": "mover", "op": "card", "inputs": ["t3"], "params": { "translate": keys([-4.0, 0.5, -6.0], [4.5, 1.0, -5.0]), "rotate": keys([0.0, 0.0, 0.0], [10.0, 40.0, 20.0]), "size": [3.5, 3.5] } },
                { "id": "truecam", "op": "camera", "params": { "focal": 24, "translate": keys([0.0, 0.0, 0.0], [1.8, 0.4, -0.8]), "rotate": keys([0.0, 0.0, 0.0], [-2.0, 6.0, 0.0]) } },
                { "id": "still", "op": "scene", "inputs": ["wall", "floor", "panel"] },
                { "id": "busy", "op": "scene", "inputs": ["wall", "floor", "panel", "mover"] },
                { "id": "only", "op": "scene", "inputs": ["mover"] },
                { "id": "clean", "op": "scanline_render", "inputs": ["still", "truecam", ""], "params": { "samples": 2 } },
                { "id": "plate", "op": "scanline_render", "inputs": ["busy", "truecam", ""], "params": { "samples": 2 } },
                { "id": "matte", "op": "scanline_render", "inputs": ["only", "truecam", ""], "params": { "samples": 2 } },
                { "id": "ct_clean", "op": "camera_tracker", "inputs": ["clean"], "params": { "focal": 24 } },
                { "id": "ct", "op": "camera_tracker", "inputs": ["plate", "matte"], "params": { "focal": 24 } },
            ] })).unwrap();
        // The truth: the camera's centers, to compare a solve's with (up
        // to its arbitrary scale and placement).
        let truth: Vec<Vec3> = (1..=24).map(|f| Vec3::new(1.8, 0.4, -0.8) * ((f - 1) as f64 / 23.0)).collect();
        let off = |sol: &Solution| {
            let got: Vec<Vec3> = (1..=24).map(|f| geom::transform_point(&sol.camera(f).unwrap().world, &Vec3::zeros())).collect();
            let (k, r, t) = sfm::similarity(&got, &truth);
            got.iter().zip(&truth).map(|(a, b)| (k * (r * a) + t - b).norm()).fold(0.0, f64::max)
        };
        let mut s = Session::new(graph);
        for id in ["ct_clean", "ct"] {
            let summary = s.track_camera(id, None, None, &Progress::default()).unwrap();
            assert_eq!(summary["frames_solved"], 24, "{id}: {summary}");
            assert!(summary["rms_error_px"].as_f64().unwrap() < 0.3, "{id}: {summary}");
        }
        let (clean, masked) = (off(&s.camera_solution("ct_clean").unwrap()), off(&s.camera_solution("ct").unwrap()));
        assert!(masked < 0.01 && masked < clean * 2.0 + 0.002, "masked: {masked} off; without the card: {clean}");
        // Tracked again by hand: the card is in the shot, and no live
        // feature ever touches its matte.
        let settings = Settings::from_params(&crate::ops::params_with_defaults("camera_tracker", &json!({})), 1, 24);
        let mut frames = input_frames(&s.graph, "ct").unwrap();
        let mut tracker = FeatureTracker::new(TrackSettings { max_features: settings.features, min_distance: settings.min_distance, ..TrackSettings::default() });
        let mut covered = 0;
        for f in 1..=24 {
            let (img, mask) = frames(f).unwrap();
            let b = blocked(mask.as_ref().unwrap(), img.format, &settings);
            covered += (0..img.format.height).filter(|&y| (0..img.format.width).any(|x| b.near([x as f32, y as f32], 0))).count();
            tracker.push_masked(f, &img, Some(&b));
            assert!(tracker.live().all(|t| !b.near(*t.points.last().unwrap(), 0)), "frame {f}: a feature on the card");
        }
        assert!(covered > 24 * 40, "the card crosses the shot: {covered} rows");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A rendered shot bent by a Lens Distort (Distort, pincushion): with
    /// Solve lens distortion its K1 and K2 come back as that node's, and
    /// Export undistorts the plate with them; without it, a perfect lens
    /// and no Lens Distort. (The camera moves and turns well past cards at
    /// many depths: over a nearly flat set, a bend of the lens and a bend of
    /// the points look much alike.)
    #[test]
    fn a_lens_distort_plate_solves_back_to_its_values() {
        use crate::session::Session;
        let dir = std::env::temp_dir().join(format!("arclight_camtrack_lens_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tex: Vec<String> = (0..3)
            .map(|i| {
                let path = dir.join(format!("tex{i}.exr"));
                crate::features::tests::blob_texture(2500, 768, 768, 61 + i).write(&path).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let keys = |a: [f64; 3], b: [f64; 3]| json!({ "keys": [[1, a], [24, b]], "interp": "linear" });
        let (k1, k2) = (0.06, 0.015);
        let graph: crate::graph::Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 24, "fps": 24, "format": { "width": 480, "height": 270 } },
            "nodes": [
                { "id": "t0", "op": "read", "params": { "path": tex[0] } },
                { "id": "t1", "op": "read", "params": { "path": tex[1] } },
                { "id": "t2", "op": "read", "params": { "path": tex[2] } },
                { "id": "wall", "op": "card", "inputs": ["t0"], "params": { "translate": [0, 3, -16], "size": [36, 24] } },
                { "id": "floor", "op": "card", "inputs": ["t1"], "params": { "translate": [0, -1.5, -10], "rotate": [-90, 0, 0], "size": [30, 24] } },
                { "id": "panel", "op": "card", "inputs": ["t2"], "params": { "translate": [-2.5, 1.5, -7], "rotate": [0, 25, 0], "size": [3, 3] } },
                { "id": "panel2", "op": "card", "inputs": ["t1"], "params": { "translate": [2.5, 0.5, -5], "rotate": [0, -30, 0], "size": [2.5, 2.5] } },
                { "id": "panel3", "op": "card", "inputs": ["t0"], "params": { "translate": [5.0, 2.0, -11], "rotate": [0, -40, 0], "size": [4, 4] } },
                { "id": "truecam", "op": "camera", "params": { "focal": 24, "translate": keys([0.0, 0.0, 0.0], [3.5, 0.8, -2.5]), "rotate": keys([0.0, 0.0, 0.0], [-5.0, 16.0, 3.0]) } },
                { "id": "set", "op": "scene", "inputs": ["wall", "floor", "panel", "panel2", "panel3"] },
                { "id": "render", "op": "scanline_render", "inputs": ["set", "truecam", ""], "params": { "samples": 2 } },
                { "id": "bent", "op": "lens_distort", "inputs": ["render"], "params": { "mode": "distort", "k1": k1, "k2": k2 } },
                { "id": "ct", "op": "camera_tracker", "inputs": ["bent"], "params": { "focal": 24, "solve_distortion": true } },
                { "id": "ct_off", "op": "camera_tracker", "inputs": ["bent"], "params": { "focal": 24 } },
            ] })).unwrap();
        let mut s = Session::new(graph);
        let summary = s.track_camera("ct", None, None, &Progress::default()).unwrap();
        assert_eq!(summary["frames_solved"], 24, "{summary}");
        assert!(summary["rms_error_px"].as_f64().unwrap() < 0.3, "{summary}");
        let sol = s.camera_solution("ct").unwrap();
        assert!((sol.k1 - k1).abs() < 0.004 && (sol.k2 - k2).abs() < 0.006, "K1 {} K2 {}", sol.k1, sol.k2);
        assert_eq!((summary["k1"].as_f64(), summary["k2"].as_f64()), (Some((sol.k1 * 1e4).round() / 1e4), Some((sol.k2 * 1e4).round() / 1e4)));
        let names = s.export_camera("ct").unwrap();
        let lens = names["lens"].as_str().unwrap().to_string();
        let n = s.node(&lens).unwrap();
        assert_eq!((n.op.as_str(), n.inputs[0].as_str(), n.params["mode"].as_str(), n.params["k1"].as_f64(), n.params["k2"].as_f64()), ("lens_distort", "ct", Some("undistort"), Some(sol.k1), Some(sol.k2)));
        assert_eq!(s.node(names["render"].as_str().unwrap()).unwrap().inputs[2], lens, "the render goes over the undistorted plate");
        // Off: a perfect lens, more error, and nothing to undistort.
        let off = s.track_camera("ct_off", None, None, &Progress::default()).unwrap();
        let off_sol = s.camera_solution("ct_off").unwrap();
        assert!(off_sol.k1 == 0.0 && off_sol.k2 == 0.0 && off["rms_error_px"].as_f64().unwrap() > summary["rms_error_px"].as_f64().unwrap(), "{off} vs {summary}");
        let names = s.export_camera("ct_off").unwrap();
        assert!(names.get("lens").is_none() && s.node(names["render"].as_str().unwrap()).unwrap().inputs[2] == "ct_off");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A camera turning on the spot: Auto solves it as a tripod shot, Free
    /// move refuses it and says to use Tripod.
    #[test]
    fn auto_picks_tripod_for_a_camera_that_only_turns() {
        use crate::solve::tests::{cloud, synthetic_tracks, FMT};
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let pan: Vec<Pose> = (0..20).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.002 * f as f64, 0.01 * f as f64, 0.0), Vec3::zeros())).collect();
        let tracks = synthetic_tracks(&k, &pan, &cloud(300, 4), 0.2);
        let mut s = Settings::from_params(&json!({}), 0, 19);
        s.distance = 7.0;
        let solve_as = |s: &Settings| solve(&tracks, s, k, &sfm::Settings::default(), &mut |_| true);
        let (sol, tripod) = solve_as(&s).unwrap();
        assert!(tripod && sol.poses.len() == 20 && sol.poses.values().all(|p| p.t.norm() < 1e-12));
        assert!(sol.points.iter().flatten().all(|x| (x.norm() - 7.0).abs() < 1e-9));
        let e = solve_as(&Settings { mode: Mode::Free, ..s.clone() }).unwrap_err();
        assert!(e.to_string().contains("set Solve to Tripod"), "{e}");
        assert!(solve_as(&Settings { mode: Mode::Tripod, ..s }).unwrap().1);
        assert_eq!(Mode::parse("free"), Some(Mode::Free));
        assert!([Mode::Auto, Mode::Free, Mode::Tripod].iter().all(|m| Mode::parse(m.name()) == Some(*m) && crate::ops::SOLVE_MODES.contains(&m.name())));
    }

    /// The whole tripod pipeline: a rendered pan (the camera turning on the
    /// spot), tracked and solved under Auto; a card placed on floor points
    /// stays on its spot of the plate in every frame, and the exported
    /// Camera only turns.
    #[test]
    fn a_rendered_pan_solves_as_a_tripod_shot() {
        use crate::session::Session;
        let dir = std::env::temp_dir().join(format!("arclight_camtrack_pan_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tex: Vec<String> = (0..3)
            .map(|i| {
                let path = dir.join(format!("tex{i}.exr"));
                crate::features::tests::blob_texture(2500, 768, 768, 41 + i).write(&path).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let graph: crate::graph::Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 24, "fps": 24, "format": { "width": 480, "height": 270 } },
            "nodes": [
                { "id": "t0", "op": "read", "params": { "path": tex[0] } },
                { "id": "t1", "op": "read", "params": { "path": tex[1] } },
                { "id": "t2", "op": "read", "params": { "path": tex[2] } },
                { "id": "wall", "op": "card", "inputs": ["t0"], "params": { "translate": [0, 3, -16], "size": [48, 24] } },
                { "id": "floor", "op": "card", "inputs": ["t1"], "params": { "translate": [0, -1.5, -10], "rotate": [-90, 0, 0], "size": [40, 24] } },
                { "id": "panel", "op": "card", "inputs": ["t2"], "params": { "translate": [-2.5, 1.0, -6], "rotate": [0, 25, 0], "size": [3, 3] } },
                { "id": "truecam", "op": "camera", "params": { "focal": 24, "rotate": { "keys": [[1, [0.0, 0.0, 0.0]], [24, [-4.0, 12.0, 0.0]]], "interp": "linear" } } },
                { "id": "set", "op": "scene", "inputs": ["wall", "floor", "panel"] },
                { "id": "plate", "op": "scanline_render", "inputs": ["set", "truecam", ""], "params": { "samples": 2 } },
                { "id": "ct", "op": "camera_tracker", "inputs": ["plate"], "params": { "focal": 24 } },
            ] })).unwrap();
        let mut s = Session::new(graph);
        let summary = s.track_camera("ct", None, Some(20.0), &Progress::default()).unwrap();
        assert_eq!((summary["frames_solved"].as_i64(), summary["mode"].as_str()), (Some(24), Some("tripod")), "{summary}");
        assert!(summary["rms_error_px"].as_f64().unwrap() < 0.3, "{summary}");
        assert_eq!(s.node("ct").unwrap().params["distance"], json!(20.0));
        let sol = s.camera_solution("ct").unwrap();
        assert!(sol.tripod && sol.points.iter().all(|p| (Vec3::from(*p).norm() - 20.0).abs() < 1e-6));
        let fmt = Size { width: 480, height: 270 };
        let cam1 = sol.camera(1).unwrap();
        let floor: Vec<usize> = (0..sol.points.len())
            .filter(|&i| cam1.project(&Vec3::from(sol.points[i]), fmt).is_some_and(|(q, _)| q[1] > 190.0 && q[0] > 100.0 && q[0] < 380.0))
            .collect();
        assert!(floor.len() >= 6, "{} floor points", floor.len());
        let card = s.place_on_points("ct", &floor, "card", 1, None, Some("marker")).unwrap();
        let center = Vec3::from(crate::scene::numbers::<3>(&s.node(&card).unwrap().params["translate"], 0.0));
        // The true camera only turns, so the floor spot under the card at
        // frame 1 is anywhere along that frame's ray through it.
        let truth = |f: i64| {
            let t = (f - 1) as f64 / 23.0;
            Camera { focal: 24.0, world: geom::trs([0.0; 3], [-4.0 * t, 12.0 * t, 0.0], [1.0; 3]), ..Camera::default() }
        };
        let p1 = cam1.project(&center, fmt).unwrap().0;
        let spot = truth(1).unproject(p1, 10.0, fmt);
        let mut worst = 0.0f64;
        for f in 1..=24 {
            let a = sol.camera(f).unwrap().project(&center, fmt).unwrap().0;
            let b = truth(f).project(&spot, fmt).unwrap().0;
            worst = worst.max((a[0] - b[0]).hypot(a[1] - b[1]));
        }
        assert!(worst < 1.0, "the card drifts {worst} px off its spot");
        let names = s.export_camera("ct").unwrap();
        let cam = &s.node(names["camera"].as_str().unwrap()).unwrap().params;
        assert!(cam["translate"].is_array() && cam["rotate"]["keys"].as_array().is_some_and(|k| k.len() == 24), "{cam:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
