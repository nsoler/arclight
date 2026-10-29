//! Bundle adjustment: refine camera poses, 3D points and (optionally) the
//! focal length together so that the points project as close as possible
//! to where they were tracked.
//!
//! Levenberg–Marquardt on the reprojection errors, with a Huber loss so a
//! few bad tracks pull less than they would squared. Each step solves the
//! normal equations by the Schur complement: the points' 3×3 blocks are
//! eliminated first, leaving a dense system in the camera parameters only
//! (six per free camera, plus one for the focal length), which stays small
//! even with thousands of points. Rotations are updated by a small turn
//! applied on the left, R ← exp(ω) R. The first camera is usually held
//! fixed (`fixed`) to pin the solution down; the overall scale is left to
//! the damping.

use crate::solve::{Intrinsics, Pose, V3};
use nalgebra::{DMatrix, DVector, Matrix3, Rotation3, Vector3};

/// One tracked position: camera `cam` saw point `point` at `px`.
#[derive(Clone, Copy, Debug)]
pub struct Obs {
    pub cam: usize,
    pub point: usize,
    pub px: [f64; 2],
}

/// What to adjust and what it was seen as.
pub struct Problem {
    pub poses: Vec<Pose>,
    pub points: Vec<V3>,
    pub obs: Vec<Obs>,
    pub intr: Intrinsics,
    /// Cameras left as they are.
    pub fixed: Vec<bool>,
    /// Points left as they are (adjust cameras only).
    pub points_fixed: bool,
    pub solve_focal: bool,
    /// Errors past this many pixels count linearly, not squared.
    pub huber: f64,
}

/// How an adjustment went.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    /// Root-mean-square reprojection error before and after, pixels.
    pub start_rms: f64,
    pub end_rms: f64,
    pub iterations: usize,
}

/// The skew-symmetric matrix of `v` (v × x = [v]× x).
fn skew(v: &V3) -> Matrix3<f64> {
    Matrix3::new(0.0, -v.z, v.y, v.z, 0.0, -v.x, -v.y, v.x, 0.0)
}

/// Huber's loss of a residual of length `r` (squared below `d`).
fn huber(r: f64, d: f64) -> f64 {
    if r <= d { r * r } else { 2.0 * d * r - d * d }
}

impl Problem {
    /// Observation `o`'s reprojection residual (projected − tracked), or
    /// None behind the camera.
    pub fn residual(&self, o: &Obs) -> Option<[f64; 2]> {
        let p = self.intr.project(&self.poses[o.cam].apply(&self.points[o.point]))?;
        Some([p[0] - o.px[0], p[1] - o.px[1]])
    }

    /// The robust cost and the root-mean-square error (pixels; an
    /// observation behind its camera counts as 1000 px).
    pub fn cost(&self) -> (f64, f64) {
        let (mut cost, mut sq) = (0.0, 0.0);
        for o in &self.obs {
            let r = self.residual(o).map_or(1000.0, |r| r[0].hypot(r[1]));
            cost += huber(r, self.huber);
            sq += r * r;
        }
        (cost, (sq / self.obs.len().max(1) as f64).sqrt())
    }

    /// Up to `iterations` Levenberg–Marquardt steps.
    pub fn adjust(&mut self, iterations: usize) -> Report {
        // Free cameras' parameter offsets; the focal length goes last.
        let mut offset = vec![usize::MAX; self.poses.len()];
        let mut nc = 0;
        for (i, o) in offset.iter_mut().enumerate() {
            if !self.fixed.get(i).copied().unwrap_or(false) {
                *o = nc;
                nc += 6;
            }
        }
        let focal_at = self.solve_focal.then(|| {
            nc += 1;
            nc - 1
        });
        let np = if self.points_fixed { 0 } else { self.points.len() };
        let mut by_point: Vec<Vec<usize>> = vec![Vec::new(); np];
        for (k, o) in self.obs.iter().enumerate() {
            if np > 0 {
                by_point[o.point].push(k);
            }
        }
        let (start_cost, start_rms) = self.cost();
        let mut cost = start_cost;
        let mut lambda = 1e-3;
        let mut done = 0;
        for _ in 0..iterations {
            done += 1;
            // Linearize: per observation, its camera-side Jacobian columns
            // (global index, d residual) and its point Jacobian.
            let mut u = DMatrix::<f64>::zeros(nc, nc);
            let mut gc = DVector::<f64>::zeros(nc);
            let mut v = vec![Matrix3::<f64>::zeros(); np];
            let mut gp = vec![V3::zeros(); np];
            // Per observation, its rows of W (camera parameter × point).
            let mut wcols: Vec<Vec<(usize, Vector3<f64>)>> = vec![Vec::new(); self.obs.len()];
            for (k, o) in self.obs.iter().enumerate() {
                let pose = &self.poses[o.cam];
                let rx = pose.r * self.points[o.point];
                let xc = rx + pose.t;
                if xc.z <= 1e-9 {
                    continue;
                }
                let f = self.intr.f;
                let (iz, x, y) = (1.0 / xc.z, xc.x, xc.y);
                let r = [self.intr.c[0] + f * x * iz - o.px[0], self.intr.c[1] + f * y * iz - o.px[1]];
                let len = r[0].hypot(r[1]);
                let w = if len <= self.huber { 1.0 } else { self.huber / len };
                // d residual / d camera-frame point.
                let du = Vector3::new(f * iz, 0.0, -f * x * iz * iz);
                let dv = Vector3::new(0.0, f * iz, -f * y * iz * iz);
                let mut cols: Vec<(usize, [f64; 2])> = Vec::with_capacity(7);
                if offset[o.cam] != usize::MAX {
                    let base = offset[o.cam];
                    // d xc / d ω = −[R X]×, d xc / d t = I.
                    let dw = -skew(&rx);
                    for j in 0..3 {
                        let col = dw.column(j);
                        cols.push((base + j, [du.dot(&col), dv.dot(&col)]));
                    }
                    for j in 0..3 {
                        cols.push((base + 3 + j, [du[j], dv[j]]));
                    }
                }
                if let Some(fi) = focal_at {
                    cols.push((fi, [x * iz, y * iz]));
                }
                for &(a, ja) in &cols {
                    gc[a] += w * (ja[0] * r[0] + ja[1] * r[1]);
                    for &(b, jb) in &cols {
                        u[(a, b)] += w * (ja[0] * jb[0] + ja[1] * jb[1]);
                    }
                }
                if np > 0 {
                    let m = pose.r.matrix();
                    let (ju, jv) = (m.transpose() * du, m.transpose() * dv);
                    v[o.point] += w * (ju * ju.transpose() + jv * jv.transpose());
                    gp[o.point] += w * (ju * r[0] + jv * r[1]);
                    wcols[k] = cols.iter().map(|&(a, ja)| (a, ju * (w * ja[0]) + jv * (w * ja[1]))).collect();
                }
            }
            // Try damped steps until one lowers the cost.
            let mut improved = false;
            for _ in 0..10 {
                let mut s = u.clone();
                for i in 0..nc {
                    s[(i, i)] += lambda * u[(i, i)].max(1e-9);
                }
                let mut rhs = -gc.clone();
                let vinv: Vec<Matrix3<f64>> = v
                    .iter()
                    .map(|vj| {
                        let mut d = *vj;
                        for i in 0..3 {
                            d[(i, i)] += lambda * vj[(i, i)].max(1e-9);
                        }
                        d.try_inverse().unwrap_or_else(Matrix3::zeros)
                    })
                    .collect();
                // Eliminate each point: S −= W V⁻¹ Wᵀ, rhs −= W V⁻¹ (−g_p).
                for (j, ks) in by_point.iter().enumerate() {
                    let rows: Vec<(usize, Vector3<f64>)> = ks
                        .iter()
                        .flat_map(|&k| wcols[k].iter().copied())
                        .fold(Vec::new(), |mut acc: Vec<(usize, Vector3<f64>)>, (a, wcol)| {
                            match acc.iter_mut().find(|e| e.0 == a) {
                                Some(e) => e.1 += wcol,
                                None => acc.push((a, wcol)),
                            }
                            acc
                        });
                    let vi = vinv[j];
                    let vg = vi * (-gp[j]);
                    for &(a, wa) in &rows {
                        let wv = vi.transpose() * wa;
                        rhs[a] -= wa.dot(&vg);
                        for &(b, wb) in &rows {
                            s[(a, b)] -= wv.dot(&wb);
                        }
                    }
                }
                let dc = if nc == 0 { DVector::zeros(0) } else {
                    match s.clone().cholesky() {
                        Some(ch) => ch.solve(&rhs),
                        None => {
                            lambda *= 10.0;
                            continue;
                        }
                    }
                };
                // Back-substitute the points: δp = V⁻¹ (−g_p − Wᵀ δc).
                let mut dp = vec![V3::zeros(); np];
                for (j, ks) in by_point.iter().enumerate() {
                    let mut b = -gp[j];
                    for &k in ks {
                        for &(a, wa) in &wcols[k] {
                            b -= wa * dc[a];
                        }
                    }
                    dp[j] = vinv[j] * b;
                }
                let saved = (self.poses.clone(), self.points.clone(), self.intr);
                for (i, &o) in offset.iter().enumerate() {
                    if o != usize::MAX {
                        let w = Vector3::new(dc[o], dc[o + 1], dc[o + 2]);
                        let p = &mut self.poses[i];
                        p.r = Rotation3::new(w) * p.r;
                        // Products of rotations slowly lose orthonormality
                        // to rounding; left alone it compounds.
                        p.r.renormalize();
                        p.t += Vector3::new(dc[o + 3], dc[o + 4], dc[o + 5]);
                    }
                }
                if let Some(fi) = focal_at {
                    self.intr.f += dc[fi];
                }
                for (x, d) in self.points.iter_mut().zip(&dp) {
                    *x += d;
                }
                let (new_cost, _) = self.cost();
                if new_cost < cost && self.intr.f > 0.0 {
                    let gain = (cost - new_cost) / cost.max(1e-30);
                    cost = new_cost;
                    lambda = (lambda / 3.0).max(1e-9);
                    improved = gain > 1e-9;
                    break;
                }
                (self.poses, self.points, self.intr) = saved;
                lambda *= 4.0;
            }
            if !improved {
                break;
            }
        }
        Report { start_rms, end_rms: self.cost().1, iterations: done }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::tests::{cloud, noise, FMT};
    use crate::solve::Rng;

    /// A camera sliding and turning past a point cloud; noisy observations.
    fn scene(n_cams: usize, noise_px: f64) -> (Vec<Pose>, Vec<V3>, Vec<Obs>, Intrinsics) {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let poses: Vec<Pose> = (0..n_cams)
            .map(|i| {
                let t = i as f64;
                Pose::from_center(Rotation3::from_euler_angles(0.01 * t, -0.03 * t, 0.005 * t), V3::new(0.3 * t, 0.05 * t, 0.1 * t))
            })
            .collect();
        let pts = cloud(150, 21);
        let mut r = Rng(5);
        let mut obs = Vec::new();
        for (c, p) in poses.iter().enumerate() {
            for (j, x) in pts.iter().enumerate() {
                if let Some(q) = k.project(&p.apply(x)) {
                    obs.push(Obs { cam: c, point: j, px: [q[0] + noise(&mut r, noise_px), q[1] + noise(&mut r, noise_px)] });
                }
            }
        }
        (poses, pts, obs, k)
    }

    #[test]
    fn adjustment_pulls_a_disturbed_solve_back() {
        let (truth, pts, obs, k) = scene(8, 0.3);
        // Disturb every camera but the first two (which fix the frame and
        // the scale) and every point.
        let mut r = Rng(3);
        let poses: Vec<Pose> = truth
            .iter()
            .enumerate()
            .map(|(i, p)| if i < 2 { *p } else { Pose { r: Rotation3::new(V3::new(noise(&mut r, 0.01), noise(&mut r, 0.01), noise(&mut r, 0.01))) * p.r, t: p.t + V3::new(noise(&mut r, 0.05), noise(&mut r, 0.05), noise(&mut r, 0.05)) } })
            .collect();
        let points: Vec<V3> = pts.iter().map(|x| x + V3::new(noise(&mut r, 0.1), noise(&mut r, 0.1), noise(&mut r, 0.1))).collect();
        let fixed = (0..8).map(|i| i < 2).collect();
        let mut p = Problem { poses, points, obs, intr: k, fixed, points_fixed: false, solve_focal: false, huber: 1.0 };
        let rep = p.adjust(50);
        assert!(rep.start_rms > 3.0 && rep.end_rms < 0.45, "{rep:?}");
        for (a, b) in p.poses.iter().zip(&truth) {
            assert!((a.center() - b.center()).norm() < 0.02 && (a.r.inverse() * b.r).angle() < 2e-3, "{} vs {}", a.center(), b.center());
        }
    }

    #[test]
    fn adjustment_finds_the_focal_length_and_shrugs_off_outliers() {
        let (truth, pts, mut obs, k) = scene(10, 0.3);
        // One observation in twenty is wildly wrong.
        for (i, o) in obs.iter_mut().enumerate() {
            if i % 20 == 7 {
                o.px[0] += 40.0;
            }
        }
        let fixed = (0..10).map(|i| i < 2).collect();
        let wrong_f = Intrinsics { f: k.f * 1.15, ..k };
        let mut p = Problem { poses: truth.clone(), points: pts.clone(), obs, intr: wrong_f, fixed, points_fixed: false, solve_focal: true, huber: 1.0 };
        p.adjust(100);
        assert!((p.intr.f / k.f - 1.0).abs() < 0.005, "focal {} vs {}", p.intr.f, k.f);
        for (a, b) in p.poses.iter().zip(&truth) {
            assert!((a.center() - b.center()).norm() < 0.03, "{} vs {}", a.center(), b.center());
        }
        // Cameras only, points held: a lone disturbed camera comes back.
        let (truth, pts, obs, k) = scene(4, 0.0);
        let mut poses = truth.clone();
        poses[3].t += V3::new(0.2, -0.1, 0.05);
        let fixed = vec![true, true, true, false];
        let mut p = Problem { poses, points: pts, obs, intr: k, fixed, points_fixed: true, solve_focal: false, huber: 1.0 };
        p.adjust(30);
        assert!((p.poses[3].t - truth[3].t).norm() < 1e-6);
    }
}
