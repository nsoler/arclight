//! The incremental camera solve: from feature tracks to a camera for every
//! frame and a cloud of 3D points.
//!
//! 1. Start from two frames with enough parallax ([`solve::initial_pair`]).
//! 2. Add frames one at a time, nearest to the solved ones first: each
//!    starts at its neighbour's pose (carried on at the same velocity past
//!    the ends) and is refined against the points it sees (a cameras-only
//!    bundle adjustment); tracks that miss by too much there don't count.
//! 3. Triangulate tracks seen from far enough apart.
//! 4. Every so often, and at the end, adjust everything together
//!    ([`bundle`]), optionally with the focal length, then drop points
//!    that still miss.
//!
//! The result's scale and placement are arbitrary: the starting frame's
//! camera sits at the origin, one unit from the second.

use crate::bundle::{Obs, Problem};
use crate::features::FeatureTrack;
use crate::solve::{self, Intrinsics, Pose, V3};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Settings {
    /// How far (pixels) a track may miss and still count.
    pub max_error: f64,
    /// The least parallax (radians) to triangulate a point from.
    pub min_parallax: f64,
    /// Parallax (radians) at which the starting pair search stops looking
    /// further.
    pub start_parallax: f64,
    /// Tracks two frames must share to start from them.
    pub min_shared: usize,
    pub solve_focal: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { max_error: 1.0, min_parallax: 1f64.to_radians(), start_parallax: 4f64.to_radians(), min_shared: 30, solve_focal: false }
    }
}

/// A solved shot.
#[derive(Clone, Debug)]
pub struct Solution {
    pub intr: Intrinsics,
    /// Each solved frame's camera.
    pub poses: BTreeMap<i64, Pose>,
    /// Each track's point, when it was triangulated and kept.
    pub points: Vec<Option<V3>>,
    /// Root-mean-square reprojection error, pixels: overall and per frame.
    pub rms: f64,
    pub frame_rms: BTreeMap<i64, f64>,
}

/// The working state while solving.
struct State<'a> {
    tracks: &'a [FeatureTrack],
    intr: Intrinsics,
    poses: BTreeMap<i64, Pose>,
    points: Vec<Option<V3>>,
    /// Tracks thrown out as outliers: not triangulated again.
    rejected: Vec<bool>,
    s: &'a Settings,
}

impl State<'_> {
    fn pos(&self, track: usize, frame: i64) -> Option<[f64; 2]> {
        self.tracks[track].at(frame).map(|p| [p[0] as f64, p[1] as f64])
    }

    /// A bundle problem over the solved frames (in order) and points, with
    /// `fixed` frames held.
    fn problem(&self, frames: &[i64], fixed: &dyn Fn(i64) -> bool, points_fixed: bool) -> (Problem, Vec<usize>) {
        let cam: BTreeMap<i64, usize> = frames.iter().enumerate().map(|(i, f)| (*f, i)).collect();
        let ids: Vec<usize> = (0..self.points.len()).filter(|&t| self.points[t].is_some()).collect();
        let mut obs = Vec::new();
        for (j, &t) in ids.iter().enumerate() {
            for (&f, &c) in &cam {
                if let Some(px) = self.pos(t, f) {
                    obs.push(Obs { cam: c, point: j, px });
                }
            }
        }
        let p = Problem {
            poses: frames.iter().map(|f| self.poses[f]).collect(),
            points: ids.iter().map(|&t| self.points[t].unwrap()).collect(),
            obs,
            intr: self.intr,
            fixed: frames.iter().map(|&f| fixed(f)).collect(),
            points_fixed,
            solve_focal: false,
            huber: self.s.max_error,
        };
        (p, ids)
    }

    /// Adjust every solved frame and point (the first frame held), then drop
    /// points that still miss by more than three times the allowed error.
    fn adjust_all(&mut self, solve_focal: bool, iterations: usize) {
        let frames: Vec<i64> = self.poses.keys().copied().collect();
        let first = frames[0];
        let (mut p, ids) = self.problem(&frames, &|f| f == first, false);
        p.solve_focal = solve_focal;
        p.adjust(iterations);
        self.intr = p.intr;
        for (i, f) in frames.iter().enumerate() {
            self.poses.insert(*f, p.poses[i]);
        }
        let mut worst = vec![0.0f64; ids.len()];
        for o in &p.obs {
            let e = p.residual(o).map_or(f64::INFINITY, |r| r[0].hypot(r[1]));
            worst[o.point] = worst[o.point].max(e);
        }
        for (j, &t) in ids.iter().enumerate() {
            if worst[j] > 3.0 * self.s.max_error {
                self.points[t] = None;
                self.rejected[t] = true;
            } else {
                self.points[t] = Some(p.points[j]);
            }
        }
    }

    /// Solve frame `f`'s camera from the points it sees, starting at `guess`.
    fn add_frame(&mut self, f: i64, guess: Pose) -> bool {
        let seen = |st: &State, t: usize| st.points[t].is_some() && st.pos(t, f).is_some();
        if (0..self.points.len()).filter(|&t| seen(self, t)).count() < 6 {
            return false;
        }
        self.poses.insert(f, guess);
        // Twice: the second time without the tracks the first found off.
        let mut skip: Vec<bool> = vec![false; self.points.len()];
        for round in 0..2 {
            let (mut p, ids) = self.problem(&[f], &|_| false, true);
            p.obs.retain(|o| !skip[ids[o.point]]);
            p.huber = self.s.max_error;
            p.adjust(30);
            let pose = p.poses[0];
            self.poses.insert(f, pose);
            let mut good = 0;
            for o in &p.obs {
                let e = p.residual(o).map_or(f64::INFINITY, |r| r[0].hypot(r[1]));
                if e > 3.0 * self.s.max_error {
                    skip[ids[o.point]] = true;
                } else {
                    good += 1;
                }
            }
            if good < 6 {
                self.poses.remove(&f);
                return false;
            }
            if round == 1 {
                return true;
            }
        }
        true
    }

    /// Triangulate tracks not yet placed that two solved frames see from
    /// far enough apart, keeping those in front of every camera that sees
    /// them and within twice the allowed error.
    fn triangulate_new(&mut self) {
        for t in 0..self.tracks.len() {
            if self.points[t].is_some() || self.rejected[t] {
                continue;
            }
            let seen: Vec<(Pose, [f64; 2])> = self.poses.iter().filter_map(|(f, p)| Some((*p, self.pos(t, *f)?))).collect();
            if seen.len() < 2 {
                continue;
            }
            let (a, b) = (seen.first().unwrap(), seen.last().unwrap());
            let rays: Vec<V3> = seen.iter().map(|(_, px)| self.intr.ray(*px)).collect();
            let poses: Vec<Pose> = seen.iter().map(|(p, _)| *p).collect();
            let Some(x) = solve::triangulate(&poses, &rays) else { continue };
            if solve::parallax(&a.0.center(), &b.0.center(), &x) < self.s.min_parallax {
                continue;
            }
            let ok = seen.iter().all(|(p, px)| {
                self.intr.project(&p.apply(&x)).is_some_and(|q| (q[0] - px[0]).hypot(q[1] - px[1]) < 2.0 * self.s.max_error)
            });
            if ok {
                self.points[t] = Some(x);
            }
        }
    }
}

/// Solve frames `first..=last` from `tracks`, with focal length and center
/// `intr` (a starting guess when `s.solve_focal`). `progress` gets the
/// fraction of frames solved and returns false to cancel.
pub fn solve(tracks: &[FeatureTrack], first: i64, last: i64, intr: Intrinsics, s: &Settings, progress: &mut dyn FnMut(f32) -> bool) -> Result<Solution> {
    // A starting pair: from the first frame, or failing that a little later.
    let step = ((last - first) / 10).max(1);
    let mut start = None;
    let mut a = first;
    while a < last && start.is_none() {
        start = solve::initial_pair(tracks, &intr, a, last, s.min_shared, s.start_parallax, s.max_error)
            .filter(|(_, tv)| tv.parallax >= s.min_parallax)
            .map(|(b, tv)| (a, b, tv));
        a += step;
    }
    let Some((a, b, tv)) = start else {
        bail!("no two frames see enough parallax to start from: the camera may only be turning, not moving")
    };
    let mut st = State { tracks, intr, poses: BTreeMap::new(), points: vec![None; tracks.len()], rejected: vec![false; tracks.len()], s };
    st.poses.insert(a, Pose::identity());
    st.poses.insert(b, tv.pose);
    for (t, x) in tv.points {
        st.points[t] = Some(x);
    }
    st.adjust_all(false, 30);
    let total = (last - first + 1) as f32;
    let mut failed: Vec<i64> = Vec::new();
    let mut since_adjust = 0;
    loop {
        if !progress(st.poses.len() as f32 / total) {
            bail!("cancelled");
        }
        let (lo, hi) = (*st.poses.keys().next().unwrap(), *st.poses.keys().next_back().unwrap());
        // Unsolved frames next to or between solved ones, the one seeing
        // the most points first.
        let candidates = (lo.max(first + 1) - 1..=(hi + 1).min(last)).filter(|f| !st.poses.contains_key(f) && !failed.contains(f));
        let count = |f: i64| (0..tracks.len()).filter(|&t| st.points[t].is_some() && st.pos(t, f).is_some()).count();
        let Some(f) = candidates.max_by_key(|&f| (count(f), -(f - a).abs())) else { break };
        // Start at the nearest solved frame's pose, carried on at the same
        // velocity past the ends.
        let near = |d: i64| st.poses.get(&(f - d)).or_else(|| st.poses.get(&(f + d))).copied();
        let guess = match (st.poses.get(&(f - 1)), st.poses.get(&(f - 2)), st.poses.get(&(f + 1)), st.poses.get(&(f + 2))) {
            (Some(p1), Some(p2), None, _) | (None, _, Some(p1), Some(p2)) => {
                // The step from p2 to p1 (x₁ = dr x₂ + dt), taken again.
                let dr = p1.r * p2.r.inverse();
                let dt = p1.t - dr * p2.t;
                let mut r = dr * p1.r;
                r.renormalize();
                Pose { r, t: dr * p1.t + dt }
            }
            _ => (1..=last - first).find_map(near).unwrap_or_else(Pose::identity),
        };
        if st.add_frame(f, guess) {
            st.triangulate_new();
            since_adjust += 1;
            if since_adjust * 5 >= st.poses.len().max(10) {
                st.adjust_all(false, 20);
                since_adjust = 0;
            }
        } else {
            failed.push(f);
        }
    }
    // Last, everything together (with the focal length if asked), twice:
    // points dropped the first time no longer pull the second.
    st.adjust_all(s.solve_focal, 50);
    st.triangulate_new();
    st.adjust_all(s.solve_focal, 50);
    progress(st.poses.len() as f32 / total);
    let frames: Vec<i64> = st.poses.keys().copied().collect();
    let (p, _) = st.problem(&frames, &|_| true, true);
    let mut sums: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    let mut all = (0.0, 0usize);
    for o in &p.obs {
        let e = p.residual(o).map_or(1000.0, |r| r[0].hypot(r[1]));
        let s = sums.entry(frames[o.cam]).or_default();
        s.0 += e * e;
        s.1 += 1;
        all.0 += e * e;
        all.1 += 1;
    }
    let frame_rms = sums.into_iter().map(|(f, (s, n))| (f, (s / n.max(1) as f64).sqrt())).collect();
    Ok(Solution { intr: st.intr, poses: st.poses, points: st.points, rms: (all.0 / all.1.max(1) as f64).sqrt(), frame_rms })
}

/// The similarity (scale `s`, rotation `r`, translation `t`) taking points
/// `from` closest to `to` in the least-squares sense (Umeyama's method):
/// `to ≈ s r from + t`.
pub fn similarity(from: &[V3], to: &[V3]) -> (f64, nalgebra::Rotation3<f64>, V3) {
    let n = from.len().max(1) as f64;
    let (ma, mb) = (from.iter().sum::<V3>() / n, to.iter().sum::<V3>() / n);
    let mut cov = nalgebra::Matrix3::<f64>::zeros();
    let mut var = 0.0;
    for (a, b) in from.iter().zip(to) {
        cov += (b - mb) * (a - ma).transpose();
        var += (a - ma).norm_squared();
    }
    cov /= n;
    var /= n;
    let svd = cov.svd(true, true);
    let (u, vt) = (svd.u.unwrap(), svd.v_t.unwrap());
    let mut d = nalgebra::Matrix3::identity();
    if (u * vt).determinant() < 0.0 {
        d[(2, 2)] = -1.0;
    }
    let r = u * d * vt;
    let s = (svd.singular_values.component_mul(&nalgebra::Vector3::new(d[(0, 0)], d[(1, 1)], d[(2, 2)]))).sum() / var.max(1e-30);
    let t = mb - s * r * ma;
    (s, nalgebra::Rotation3::from_matrix_unchecked(r), t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::tests::{noise, FMT};
    use crate::solve::Rng;
    use nalgebra::Rotation3;

    /// A 40-frame shot: the camera trucks right 0.15 a frame, rising and
    /// turning a little, past points spread wide and deep. Each point's
    /// track runs while it's in view, with noise; one track in ten jumps
    /// off once.
    fn shot() -> (Intrinsics, Vec<Pose>, Vec<V3>, Vec<FeatureTrack>, Vec<usize>) {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let poses: Vec<Pose> = (0..40)
            .map(|f| {
                let t = f as f64;
                Pose::from_center(Rotation3::from_euler_angles(0.002 * t, -0.006 * t, 0.001 * t), V3::new(0.15 * t, -0.01 * t, 0.02 * t))
            })
            .collect();
        let mut r = Rng(13);
        let mut u = || r.below(1_000_000) as f64 / 1e6;
        let pts: Vec<V3> = (0..900).map(|_| V3::new(u() * 20.0 - 6.0, u() * 6.0 - 3.0, 4.0 + u() * 12.0)).collect();
        let mut r = Rng(17);
        let (mut tracks, mut which) = (Vec::new(), Vec::new());
        for (j, x) in pts.iter().enumerate() {
            let seen: Vec<Option<[f32; 2]>> = poses
                .iter()
                .map(|p| {
                    let q = k.project(&p.apply(x))?;
                    (q[0] > 5.0 && q[1] > 5.0 && q[0] < FMT.width as f64 - 5.0 && q[1] < FMT.height as f64 - 5.0).then_some([q[0] as f32, q[1] as f32])
                })
                .collect();
            let Some(start) = seen.iter().position(Option::is_some) else { continue };
            let mut points: Vec<[f32; 2]> = seen[start..].iter().map_while(|q| *q).collect();
            if points.len() < 2 {
                continue;
            }
            for p in points.iter_mut() {
                p[0] += noise(&mut r, 0.3) as f32;
                p[1] += noise(&mut r, 0.3) as f32;
            }
            if j % 10 == 3 {
                let n = points.len();
                points[n / 2][0] += 25.0;
            }
            tracks.push(FeatureTrack { start: start as i64, points });
            which.push(j);
        }
        (k, poses, pts, tracks, which)
    }

    /// Solved camera centers and rotations against the truth, after the
    /// similarity that best lines the solved points up with the true ones
    /// (the camera path alone, nearly straight, leaves the turn about it
    /// open): the worst center error relative to the path's length, and
    /// the worst rotation error (degrees).
    fn compare(sol: &Solution, truth: &[Pose], pts: &[V3], which: &[usize]) -> (f64, f64) {
        let frames: Vec<i64> = sol.poses.keys().copied().collect();
        let pairs: Vec<(V3, V3)> = sol.points.iter().zip(which).filter_map(|(p, &j)| Some(((*p)?, pts[j]))).collect();
        let (s, r, t) = similarity(&pairs.iter().map(|p| p.0).collect::<Vec<_>>(), &pairs.iter().map(|p| p.1).collect::<Vec<_>>());
        let from: Vec<V3> = frames.iter().map(|f| sol.poses[f].center()).collect();
        let to: Vec<V3> = frames.iter().map(|f| truth[*f as usize].center()).collect();
        let mut pe: Vec<f64> = pairs.iter().map(|(a, b)| (s * (r * a) + t - b).norm()).collect(); pe.sort_by(f64::total_cmp);
        let length = (to[to.len() - 1] - to[0]).norm();
        let center = from.iter().zip(&to).map(|(a, b)| (s * (r * a) + t - b).norm()).fold(0.0, f64::max) / length;
        // Truth scene = s r (solved scene) + t, so a solved camera's
        // rotation should be the true one times r.
        let rot = frames
            .iter()
            .map(|f| (sol.poses[f].r.inverse() * truth[*f as usize].r * r).angle().to_degrees())
            .fold(0.0, f64::max);
        (center, rot)
    }

    #[test]
    fn a_shot_solves_to_its_true_camera_path() {
        let (k, truth, pts, tracks, which) = shot();
        let mut calls = 0;
        let sol = solve(&tracks, 0, 39, k, &Settings::default(), &mut |_| {
            calls += 1;
            true
        })
        .unwrap();
        assert_eq!(sol.poses.len(), 40, "every frame solved");
        assert!(sol.rms < 0.5, "rms {}", sol.rms);
        let (center, rot) = compare(&sol, &truth, &pts, &which);
        assert!(center < 0.005 && rot < 0.1, "center {center} of the path, rotation {rot}°");
        // The tracks that jumped were thrown out, most of the rest kept.
        let kept = sol.points.iter().filter(|p| p.is_some()).count();
        assert!(kept * 10 > tracks.len() * 7, "{kept} of {} points", tracks.len());
        assert!(calls > 30);
        // Cancelling stops it.
        assert!(solve(&tracks, 0, 39, k, &Settings::default(), &mut |f| f < 0.3).unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn the_focal_length_can_be_solved_too() {
        let (k, truth, pts, tracks, which) = shot();
        let guess = Intrinsics { f: k.f * 1.1, ..k };
        let sol = solve(&tracks, 0, 39, guess, &Settings { solve_focal: true, ..Settings::default() }, &mut |_| true).unwrap();
        assert!((sol.intr.f / k.f - 1.0).abs() < 0.01, "focal {} vs {}", sol.intr.f, k.f);
        let (center, rot) = compare(&sol, &truth, &pts, &which);
        assert!(center < 0.01 && rot < 0.2, "center {center}, rotation {rot}°");
    }

    #[test]
    fn a_camera_that_only_turns_is_refused() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let poses: Vec<Pose> = (0..20).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.0, 0.01 * f as f64, 0.0), V3::zeros())).collect();
        let tracks = crate::solve::tests::synthetic_tracks(&k, &poses, &crate::solve::tests::cloud(300, 4), 0.2);
        let e = solve(&tracks, 0, 19, k, &Settings::default(), &mut |_| true).unwrap_err();
        assert!(e.to_string().contains("only be turning"), "{e}");
    }

    /// The whole pipeline on pictures: a camera moving through a small
    /// set of textured cards (a floor, a back wall, two panels at other
    /// depths), rendered with Scanline Render, tracked, and solved.
    #[test]
    fn a_rendered_shot_solves() {
        use crate::features::{tests::blob_texture, FeatureTracker, Settings as Track};
        use crate::geom;
        use crate::image::Size;
        use crate::scene::{Card, Scene};
        use std::sync::Arc;
        let fmt = Size { width: 480, height: 270 };
        let card = |seed: u64, t: [f64; 3], r: [f64; 3], size: [f64; 2]| Card {
            image: Arc::new(blob_texture(2500, 768, 768, seed)),
            world: geom::trs(t, r, [1.0; 3]),
            size,
        };
        let scene = Scene {
            cards: vec![
                card(1, [0.0, 0.0, -14.0], [0.0; 3], [30.0, 30.0]),
                card(2, [0.0, -2.0, -6.0], [-90.0, 0.0, 0.0], [30.0, 20.0]),
                card(3, [-2.0, 0.5, -6.0], [0.0, 25.0, 0.0], [3.0, 4.0]),
                card(4, [2.5, 0.0, -8.0], [0.0, -20.0, 0.0], [3.0, 4.0]),
            ],
            ..Scene::default()
        };
        let cam = |f: f64| geom::Camera { focal: 24.0, world: geom::trs([0.08 * f, 0.02 * f, -0.03 * f], [0.0, 0.3 * f, 0.0], [1.0; 3]), ..geom::Camera::default() };
        let mut tracker = FeatureTracker::new(Track { max_features: 300, min_distance: 10.0, ..Track::default() });
        for f in 0..24 {
            tracker.push(f, &crate::render3d::render(&scene, &cam(f as f64), fmt, 2, None));
        }
        let k = Intrinsics::new(24.0, 36.0, fmt);
        let sol = solve(&tracker.tracks, 0, 23, k, &Settings::default(), &mut |_| true).unwrap();
        assert_eq!(sol.poses.len(), 24);
        assert!(sol.rms < 0.3, "rms {}", sol.rms);
        // Against the true cameras: centers after the best similarity;
        // rotations relative to the first frame's (which needs no
        // alignment, and a nearly straight path can't fix the turn about it).
        let truth: Vec<Pose> = (0..24).map(|f| Pose::from_world(&cam(f as f64).world)).collect();
        let from: Vec<V3> = (0..24).map(|f| sol.poses[&f].center()).collect();
        let to: Vec<V3> = truth.iter().map(Pose::center).collect();
        let (s, r, t) = similarity(&from, &to);
        let length = (to[23] - to[0]).norm();
        let worst = from.iter().zip(&to).map(|(a, b)| (s * (r * a) + t - b).norm()).fold(0.0, f64::max) / length;
        let rel = |p: &Pose, q: &Pose| p.r * q.r.inverse();
        let rot = (0..24)
            .map(|f| (rel(&sol.poses[&(f as i64)], &sol.poses[&0]).inverse() * rel(&truth[f as usize], &truth[0])).angle().to_degrees())
            .fold(0.0, f64::max);
        assert!(worst < 0.01 && rot < 0.1, "center {worst} of the path, rotation {rot}°");
    }
}
