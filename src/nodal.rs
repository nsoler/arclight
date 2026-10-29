//! The tripod (nodal) solve: a camera that only turns about its center.
//!
//! A camera that doesn't move gives no parallax, so [`crate::sfm::solve`]
//! can't start; but then every pair of frames is related by a homography
//! `H = K R K⁻¹` (`R` the turn between them, `K` the intrinsics), whatever
//! the scene's depth.
//!
//! - **Detection** ([`detect`]): over pairs of frames as far apart as their
//!   shared tracks allow, fit a homography (normalized DLT in RANSAC, then
//!   refined by Levenberg–Marquardt). The shot is nodal when the
//!   homographies explain the tracks (median error within a looser
//!   `max_error`, for mild lens distortion) and are rotations
//!   (`K⁻¹ H K` orthonormal, for the best focal length near the given one).
//! - **Solve** ([`solve`]): the camera sits at the origin with a rotation
//!   per frame, chained from frame-to-frame homographies
//!   (`R = K⁻¹ H K`, orthonormalized); each track is a direction (a unit
//!   vector). A rotation-only adjustment then refines the rotations, the
//!   directions and optionally the focal length, and the points come out
//!   on a sphere at a chosen distance (points at infinity have no depth).

use crate::bundle::Obs;
use crate::features::FeatureTrack;
use crate::sfm::{Settings, Solution};
use crate::solve::{self, Intrinsics, Pose, Rng, V3};
use anyhow::{bail, Result};
use nalgebra::{DMatrix, DVector, Matrix2, Matrix3, Rotation3, SMatrix, SVector, SymmetricEigen, Vector2, Vector3};
use std::collections::BTreeMap;

/// How much looser than `max_error` the homography tests are, for mild
/// lens distortion.
const LOOSE: f64 = 2.0;
/// The most a pair's `K⁻¹ H K` (scaled to determinant 1) may depart from a
/// rotation, as ‖MᵀM − I‖, and still count as one.
const MAX_DEPARTURE: f64 = 0.02;
/// Frame pairs tried for detection.
const PAIRS: usize = 8;

/// `h` with unit norm and a positive determinant.
fn unit(h: Matrix3<f64>) -> Matrix3<f64> {
    let h = h / h.norm();
    if h.determinant() < 0.0 { -h } else { h }
}

/// Where homography `h` takes `p` (None at infinity).
fn apply(h: &Matrix3<f64>, p: [f64; 2]) -> Option<[f64; 2]> {
    let q = h * V3::new(p[0], p[1], 1.0);
    (q.z.abs() > 1e-12).then(|| [q.x / q.z, q.y / q.z])
}

/// How far (pixels) `h` takes `a` from `b`.
pub fn transfer_error(h: &Matrix3<f64>, a: [f64; 2], b: [f64; 2]) -> f64 {
    apply(h, a).map_or(f64::INFINITY, |q| (q[0] - b[0]).hypot(q[1] - b[1]))
}

/// Hartley's normalization: the similarity moving `p`'s centroid to the
/// origin and its mean distance from it to √2.
fn normalizer(p: &[[f64; 2]]) -> Matrix3<f64> {
    let n = p.len().max(1) as f64;
    let (mx, my) = (p.iter().map(|q| q[0]).sum::<f64>() / n, p.iter().map(|q| q[1]).sum::<f64>() / n);
    let d = p.iter().map(|q| (q[0] - mx).hypot(q[1] - my)).sum::<f64>() / n;
    let s = if d > 1e-12 { 2f64.sqrt() / d } else { 1.0 };
    Matrix3::new(s, 0.0, -s * mx, 0.0, s, -s * my, 0.0, 0.0, 1.0)
}

/// The homography taking `a` to `b` from four or more pairs (the direct
/// linear transform, on normalized coordinates).
pub fn dlt(a: &[[f64; 2]], b: &[[f64; 2]]) -> Option<Matrix3<f64>> {
    if a.len() < 4 || a.len() != b.len() {
        return None;
    }
    let (ta, tb) = (normalizer(a), normalizer(b));
    let mut m = SMatrix::<f64, 9, 9>::zeros();
    for (p, q) in a.iter().zip(b) {
        let x = ta * V3::new(p[0], p[1], 1.0);
        let y = tb * V3::new(q[0], q[1], 1.0);
        let r1 = SVector::<f64, 9>::from([-x.x, -x.y, -1.0, 0.0, 0.0, 0.0, y.x * x.x, y.x * x.y, y.x]);
        let r2 = SVector::<f64, 9>::from([0.0, 0.0, 0.0, -x.x, -x.y, -1.0, y.y * x.x, y.y * x.y, y.y]);
        m += r1 * r1.transpose() + r2 * r2.transpose();
    }
    let e = SymmetricEigen::new(m);
    let i = (0..9).min_by(|&i, &j| e.eigenvalues[i].total_cmp(&e.eigenvalues[j]))?;
    let h = e.eigenvectors.column(i);
    let hn = Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8]);
    let h = tb.try_inverse()? * hn * ta;
    h.iter().all(|v| v.is_finite()).then(|| unit(h))
}

/// `h` refined to minimize the transfer error from `a` to `b`
/// (Levenberg–Marquardt on its nine entries, in normalized coordinates).
fn refine(h: &Matrix3<f64>, a: &[[f64; 2]], b: &[[f64; 2]]) -> Matrix3<f64> {
    let (ta, tb) = (normalizer(a), normalizer(b));
    let (Some(tai), Some(tbi)) = (ta.try_inverse(), tb.try_inverse()) else { return *h };
    let xs: Vec<V3> = a.iter().map(|p| ta * V3::new(p[0], p[1], 1.0)).collect();
    let ys: Vec<V3> = b.iter().map(|q| tb * V3::new(q[0], q[1], 1.0)).collect();
    let cost = |h: &Matrix3<f64>| -> f64 {
        xs.iter()
            .zip(&ys)
            .map(|(x, y)| {
                let p = h * x;
                if p.z.abs() < 1e-12 { 1e6 } else { (p.x / p.z - y.x).powi(2) + (p.y / p.z - y.y).powi(2) }
            })
            .sum()
    };
    let mut hn = unit(tb * h * tai);
    let mut c = cost(&hn);
    let mut lambda = 1e-3;
    for _ in 0..20 {
        let mut jtj = SMatrix::<f64, 9, 9>::zeros();
        let mut g = SVector::<f64, 9>::zeros();
        for (x, y) in xs.iter().zip(&ys) {
            let p = hn * x;
            if p.z.abs() < 1e-12 {
                continue;
            }
            let iz = 1.0 / p.z;
            let (u, v) = (p.x * iz, p.y * iz);
            let (mut ju, mut jv) = (SVector::<f64, 9>::zeros(), SVector::<f64, 9>::zeros());
            for j in 0..3 {
                ju[j] = x[j] * iz;
                jv[3 + j] = x[j] * iz;
                ju[6 + j] = -u * x[j] * iz;
                jv[6 + j] = -v * x[j] * iz;
            }
            jtj += ju * ju.transpose() + jv * jv.transpose();
            g += ju * (u - y.x) + jv * (v - y.y);
        }
        let mut improved = false;
        for _ in 0..10 {
            let mut m = jtj;
            for i in 0..9 {
                m[(i, i)] += lambda * jtj[(i, i)].max(1e-12);
            }
            let Some(d) = m.cholesky().map(|ch| ch.solve(&-g)) else {
                lambda *= 10.0;
                continue;
            };
            let next = unit(hn + Matrix3::new(d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8]));
            let nc = cost(&next);
            if nc < c {
                improved = (c - nc) > 1e-12 * c;
                hn = next;
                c = nc;
                lambda = (lambda / 10.0).max(1e-12);
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    unit(tbi * hn * ta)
}

/// The homography taking `a` to `b`, robust to mismatches: DLT on samples
/// of four in RANSAC (pairs within `threshold` pixels agreeing), refit on
/// the agreeing pairs and refined. Also which pairs agree.
pub fn homography(a: &[[f64; 2]], b: &[[f64; 2]], threshold: f64, rng: &mut Rng) -> Option<(Matrix3<f64>, Vec<bool>)> {
    let n = a.len();
    if n < 4 || n != b.len() {
        return None;
    }
    let agree = |h: &Matrix3<f64>| -> Vec<bool> { a.iter().zip(b).map(|(p, q)| transfer_error(h, *p, *q) < threshold).collect() };
    let pick = |mask: &[bool]| -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
        (0..n).filter(|&i| mask[i]).map(|i| (a[i], b[i])).unzip()
    };
    let mut best: Option<(Matrix3<f64>, Vec<bool>, usize)> = None;
    let (mut needed, mut tried) = (2000usize, 0);
    while tried < needed {
        tried += 1;
        let s = rng.sample(n, 4);
        let Some(h) = dlt(&s.iter().map(|&i| a[i]).collect::<Vec<_>>(), &s.iter().map(|&i| b[i]).collect::<Vec<_>>()) else { continue };
        let mask = agree(&h);
        let count = mask.iter().filter(|&&m| m).count();
        if best.as_ref().is_none_or(|b| count > b.2) {
            // Enough samples to draw an all-agreeing one with 99.9 % odds.
            let w = count as f64 / n as f64;
            needed = needed.min((0.001f64.ln() / (1.0 - w.powi(4)).max(1e-12).ln()).ceil().max(1.0) as usize);
            best = Some((h, mask, count));
        }
    }
    let (mut h, mut mask, _) = best?;
    for _ in 0..2 {
        let (pa, pb) = pick(&mask);
        let Some(next) = dlt(&pa, &pb) else { break };
        h = next;
        mask = agree(&h);
    }
    let (pa, pb) = pick(&mask);
    if pa.len() < 4 {
        return None;
    }
    h = refine(&h, &pa, &pb);
    mask = agree(&h);
    Some((h, mask))
}

/// The camera matrix of `k` (normalized ray → pixel).
fn kmat(k: &Intrinsics) -> Matrix3<f64> {
    Matrix3::new(k.f, 0.0, k.c[0], 0.0, k.f, k.c[1], 0.0, 0.0, 1.0)
}

/// The rotation nearest `K⁻¹ h K`, and how far that (scaled to determinant
/// 1) is from being one: ‖MᵀM − I‖.
pub fn rotation_of(h: &Matrix3<f64>, k: &Intrinsics) -> (Rotation3<f64>, f64) {
    let kk = kmat(k);
    let Some(ki) = kk.try_inverse() else { return (Rotation3::identity(), f64::INFINITY) };
    let mut m = ki * h * kk;
    let d = m.determinant();
    if d.abs() < 1e-15 || !d.is_finite() {
        return (Rotation3::identity(), f64::INFINITY);
    }
    m /= d.cbrt();
    let departure = (m.transpose() * m - Matrix3::identity()).norm();
    let svd = m.svd(true, true);
    let (u, vt) = (svd.u.unwrap(), svd.v_t.unwrap());
    let mut fix = Matrix3::identity();
    if (u * vt).determinant() < 0.0 {
        fix[(2, 2)] = -1.0;
    }
    (Rotation3::from_matrix_unchecked(u * fix * vt), departure)
}

/// The focal length (between half and twice `k`'s) that makes `K⁻¹ h K`
/// most nearly a rotation, and its departure from one.
fn fit_focal(h: &Matrix3<f64>, k: &Intrinsics) -> (f64, f64) {
    let dep = |x: f64| rotation_of(h, &Intrinsics { f: k.f * x.exp(), ..*k }).1;
    let (lo, hi, n) = (0.5f64.ln(), 2f64.ln(), 40);
    let step = (hi - lo) / n as f64;
    let x0 = (0..=n).map(|i| lo + step * i as f64).min_by(|a, b| dep(*a).total_cmp(&dep(*b))).unwrap();
    // Golden-section search around the best on the grid.
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let (mut a, mut b) = ((x0 - step).max(lo), (x0 + step).min(hi));
    for _ in 0..40 {
        let (c, d) = (b - g * (b - a), a + g * (b - a));
        if dep(c) < dep(d) { b = d } else { a = c }
    }
    let x = (a + b) / 2.0;
    (k.f * x.exp(), dep(x))
}

/// How many tracks frames `a` and `b` share.
fn shared_count(tracks: &[FeatureTrack], a: i64, b: i64) -> usize {
    tracks.iter().filter(|t| t.at(a).is_some() && t.at(b).is_some()).count()
}

/// What [`detect`] found.
#[derive(Clone, Copy, Debug)]
pub struct Detection {
    /// Whether the camera only turns.
    pub nodal: bool,
    /// The frame pairs tried.
    pub pairs: usize,
    /// The worst pair's median transfer error, pixels.
    pub error: f64,
    /// The worst pair's departure from a rotation (see [`rotation_of`]).
    pub departure: f64,
    /// The focal length (pixels) the homographies suggest, when the camera
    /// turned enough between some pair to tell.
    pub focal: Option<f64>,
}

/// Whether the camera over frames `first..=last` only turns, judged from
/// up to eight frame pairs spread over the shot, each as far apart as
/// `s.min_shared` shared tracks allow.
pub fn detect(tracks: &[FeatureTrack], first: i64, last: i64, k: &Intrinsics, s: &Settings) -> Detection {
    let mut pairs: Vec<(i64, i64)> = Vec::new();
    let span = (last - first).max(1);
    for i in 0..PAIRS as i64 {
        let a = first + span * i / PAIRS as i64;
        let b = (a + 1..=last).take_while(|&b| shared_count(tracks, a, b) >= s.min_shared.max(8)).last();
        if let Some(b) = b.filter(|_| !pairs.iter().any(|p| p.0 == a)) {
            pairs.push((a, b));
        }
    }
    let mut out = Detection { nodal: false, pairs: pairs.len(), error: 0.0, departure: 0.0, focal: None };
    let mut focals = Vec::new();
    let mut rng = Rng(5);
    for &(a, b) in &pairs {
        let (_, pa, pb) = solve::shared(tracks, a, b);
        let Some((h, _)) = homography(&pa, &pb, LOOSE * s.max_error, &mut rng) else {
            out.error = f64::INFINITY;
            continue;
        };
        let mut errs: Vec<f64> = pa.iter().zip(&pb).map(|(p, q)| transfer_error(&h, *p, *q)).collect();
        errs.sort_by(f64::total_cmp);
        out.error = out.error.max(errs[errs.len() / 2]);
        let (f, departure) = fit_focal(&h, k);
        out.departure = out.departure.max(departure);
        // A small turn says little about the focal length.
        if rotation_of(&h, &Intrinsics { f, ..*k }).0.angle() > 5f64.to_radians() {
            focals.push(f);
        }
    }
    focals.sort_by(f64::total_cmp);
    out.focal = focals.get(focals.len() / 2).copied();
    out.nodal = !pairs.is_empty() && out.error <= LOOSE * s.max_error && out.departure <= MAX_DEPARTURE;
    out
}

/// The skew-symmetric matrix of `v` (v × x = [v]× x).
fn skew(v: &V3) -> Matrix3<f64> {
    Matrix3::new(0.0, -v.z, v.y, v.z, 0.0, -v.x, -v.y, v.x, 0.0)
}

/// Huber's loss of a residual of length `r` (squared below `d`).
fn huber(r: f64, d: f64) -> f64 {
    if r <= d { r * r } else { 2.0 * d * r - d * d }
}

/// Two unit vectors at right angles to unit `d` and each other.
fn tangent(d: &V3) -> (V3, V3) {
    let a = if d.x.abs() < 0.9 { V3::x() } else { V3::y() };
    let e1 = a.cross(d).normalize();
    (e1, d.cross(&e1))
}

/// The rotation-only adjustment: camera rotations (the first held) and
/// track directions, optionally with the focal length. Levenberg–Marquardt
/// with a Huber loss and the Schur complement, as in [`crate::bundle`],
/// but with three parameters per camera (no translation) and two per
/// direction (a step in its tangent plane, then back onto the sphere).
struct Turn {
    rots: Vec<Rotation3<f64>>,
    dirs: Vec<V3>,
    obs: Vec<Obs>,
    intr: Intrinsics,
    solve_focal: bool,
    huber: f64,
}

impl Turn {
    fn residual(&self, o: &Obs) -> Option<[f64; 2]> {
        let p = self.intr.project(&(self.rots[o.cam] * self.dirs[o.point]))?;
        Some([p[0] - o.px[0], p[1] - o.px[1]])
    }

    fn cost(&self) -> f64 {
        self.obs.iter().map(|o| huber(self.residual(o).map_or(1000.0, |r| r[0].hypot(r[1])), self.huber)).sum()
    }

    fn adjust(&mut self, iterations: usize) {
        let ncam = self.rots.len();
        let mut nc = 3 * ncam.saturating_sub(1);
        let focal_at = self.solve_focal.then(|| {
            nc += 1;
            nc - 1
        });
        let np = self.dirs.len();
        let mut by_point: Vec<Vec<usize>> = vec![Vec::new(); np];
        for (k, o) in self.obs.iter().enumerate() {
            by_point[o.point].push(k);
        }
        let mut cost = self.cost();
        let mut lambda = 1e-3;
        for _ in 0..iterations {
            let basis: Vec<(V3, V3)> = self.dirs.iter().map(tangent).collect();
            let mut u = DMatrix::<f64>::zeros(nc, nc);
            let mut gc = DVector::<f64>::zeros(nc);
            let mut v = vec![Matrix2::<f64>::zeros(); np];
            let mut gp = vec![Vector2::<f64>::zeros(); np];
            let mut wcols: Vec<Vec<(usize, Vector2<f64>)>> = vec![Vec::new(); self.obs.len()];
            for (k, o) in self.obs.iter().enumerate() {
                let r = self.rots[o.cam];
                let xc = r * self.dirs[o.point];
                if xc.z <= 1e-9 {
                    continue;
                }
                let f = self.intr.f;
                let (iz, x, y) = (1.0 / xc.z, xc.x, xc.y);
                let res = [self.intr.c[0] + f * x * iz - o.px[0], self.intr.c[1] + f * y * iz - o.px[1]];
                let len = res[0].hypot(res[1]);
                let w = if len <= self.huber { 1.0 } else { self.huber / len };
                let du = Vector3::new(f * iz, 0.0, -f * x * iz * iz);
                let dv = Vector3::new(0.0, f * iz, -f * y * iz * iz);
                let mut cols: Vec<(usize, [f64; 2])> = Vec::with_capacity(4);
                if o.cam > 0 {
                    let base = 3 * (o.cam - 1);
                    let dw = -skew(&xc);
                    for j in 0..3 {
                        let col = dw.column(j);
                        cols.push((base + j, [du.dot(&col), dv.dot(&col)]));
                    }
                }
                if let Some(fi) = focal_at {
                    cols.push((fi, [x * iz, y * iz]));
                }
                for &(a, ja) in &cols {
                    gc[a] += w * (ja[0] * res[0] + ja[1] * res[1]);
                    for &(b, jb) in &cols {
                        u[(a, b)] += w * (ja[0] * jb[0] + ja[1] * jb[1]);
                    }
                }
                let (e1, e2) = basis[o.point];
                let (a1, a2) = (r * e1, r * e2);
                let ju = Vector2::new(du.dot(&a1), du.dot(&a2));
                let jv = Vector2::new(dv.dot(&a1), dv.dot(&a2));
                v[o.point] += w * (ju * ju.transpose() + jv * jv.transpose());
                gp[o.point] += w * (ju * res[0] + jv * res[1]);
                wcols[k] = cols.iter().map(|&(a, ja)| (a, ju * (w * ja[0]) + jv * (w * ja[1]))).collect();
            }
            let mut improved = false;
            for _ in 0..10 {
                let mut s = u.clone();
                for i in 0..nc {
                    s[(i, i)] += lambda * u[(i, i)].max(1e-9);
                }
                let mut rhs = -gc.clone();
                let vinv: Vec<Matrix2<f64>> = v
                    .iter()
                    .map(|vj| {
                        let mut d = *vj;
                        for i in 0..2 {
                            d[(i, i)] += lambda * vj[(i, i)].max(1e-9);
                        }
                        d.try_inverse().unwrap_or_else(Matrix2::zeros)
                    })
                    .collect();
                // Eliminate each direction: S −= W V⁻¹ Wᵀ, rhs −= W V⁻¹ (−g_p).
                for (j, ks) in by_point.iter().enumerate() {
                    let rows: Vec<(usize, Vector2<f64>)> = ks.iter().flat_map(|&k| wcols[k].iter().copied()).fold(Vec::new(), |mut acc: Vec<(usize, Vector2<f64>)>, (a, wcol)| {
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
                let dc = if nc == 0 {
                    DVector::zeros(0)
                } else {
                    match s.cholesky() {
                        Some(ch) => ch.solve(&rhs),
                        None => {
                            lambda *= 10.0;
                            continue;
                        }
                    }
                };
                let saved = (self.rots.clone(), self.dirs.clone(), self.intr);
                for (j, ks) in by_point.iter().enumerate() {
                    let mut b = -gp[j];
                    for &k in ks {
                        for &(a, wa) in &wcols[k] {
                            b -= wa * dc[a];
                        }
                    }
                    let dp = vinv[j] * b;
                    let (e1, e2) = basis[j];
                    self.dirs[j] = (self.dirs[j] + e1 * dp[0] + e2 * dp[1]).normalize();
                }
                for i in 1..ncam {
                    let o = 3 * (i - 1);
                    let r = &mut self.rots[i];
                    *r = Rotation3::new(Vector3::new(dc[o], dc[o + 1], dc[o + 2])) * *r;
                    r.renormalize();
                }
                if let Some(fi) = focal_at {
                    self.intr.f += dc[fi];
                }
                let new_cost = self.cost();
                if new_cost < cost {
                    improved = (cost - new_cost) > 1e-10 * cost;
                    cost = new_cost;
                    lambda = (lambda / 10.0).max(1e-12);
                    break;
                }
                (self.rots, self.dirs, self.intr) = saved;
                lambda *= 10.0;
            }
            if !improved {
                break;
            }
        }
    }

    /// Stop counting directions whose worst observation still misses by
    /// more than `limit` pixels; returns which were dropped.
    fn drop_worst(&mut self, limit: f64) -> Vec<bool> {
        let mut worst = vec![0.0f64; self.dirs.len()];
        for o in &self.obs {
            let e = self.residual(o).map_or(f64::INFINITY, |r| r[0].hypot(r[1]));
            worst[o.point] = worst[o.point].max(e);
        }
        let dropped: Vec<bool> = worst.iter().map(|&w| w > limit).collect();
        self.obs.retain(|o| !dropped[o.point]);
        dropped
    }
}

/// Solve frames `first..=last` of a shot whose camera only turns, from
/// `tracks`, with focal length and center `intr` (a starting guess when
/// `s.solve_focal`). The camera stays at the origin; each kept track's
/// point is put `distance` from it. `progress` gets the fraction done and
/// returns false to cancel.
pub fn solve(tracks: &[FeatureTrack], first: i64, last: i64, intr: Intrinsics, s: &Settings, distance: f64, progress: &mut dyn FnMut(f32) -> bool) -> Result<Solution> {
    let total = (last - first + 1).max(1) as f32;
    let threshold = LOOSE * s.max_error;
    // Each frame's rotation from the nearest earlier solved frame's, through
    // the homography between them: x_f = R_f d, so H ≈ K R_f R_gᵀ K⁻¹.
    let mut rots: BTreeMap<i64, Rotation3<f64>> = BTreeMap::new();
    rots.insert(first, Rotation3::identity());
    let mut rng = Rng(11);
    for f in first + 1..=last {
        if !progress(0.5 * (f - first) as f32 / total) {
            bail!("cancelled");
        }
        for g in ((f - 10).max(first)..f).rev() {
            let Some(rg) = rots.get(&g).copied() else { continue };
            let (_, pa, pb) = solve::shared(tracks, g, f);
            if pa.len() < 8 {
                continue;
            }
            let Some((h, mask)) = homography(&pa, &pb, threshold, &mut rng) else { continue };
            if mask.iter().filter(|&&m| m).count() < 8 {
                continue;
            }
            let mut r = rotation_of(&h, &intr).0 * rg;
            r.renormalize();
            rots.insert(f, r);
            break;
        }
    }
    if rots.len() < 2 {
        bail!("too few frames share enough tracks to follow the camera's turn");
    }
    let frames: Vec<i64> = rots.keys().copied().collect();
    let cam: BTreeMap<i64, usize> = frames.iter().enumerate().map(|(i, f)| (*f, i)).collect();
    // Each track seen on two or more solved frames gets a direction: the
    // mean of its rays, turned back into the scene.
    let mut ids: Vec<usize> = Vec::new();
    let mut dirs: Vec<V3> = Vec::new();
    let mut obs: Vec<Obs> = Vec::new();
    for (t, track) in tracks.iter().enumerate() {
        let seen: Vec<(usize, [f64; 2])> = frames.iter().filter_map(|f| Some((cam[f], track.at(*f).map(|p| [p[0] as f64, p[1] as f64])?))).collect();
        if seen.len() < 2 {
            continue;
        }
        let sum: V3 = seen.iter().map(|(c, px)| rots[&frames[*c]].inverse() * intr.ray(*px).normalize()).sum();
        let j = dirs.len();
        dirs.push(sum.normalize());
        ids.push(t);
        obs.extend(seen.iter().map(|&(c, px)| Obs { cam: c, point: j, px }));
    }
    let mut turn = Turn { rots: frames.iter().map(|f| rots[f]).collect(), dirs, obs, intr, solve_focal: false, huber: s.max_error };
    // Rotations and directions first, then with the focal length if asked,
    // dropping tracks that still miss after each.
    turn.adjust(30);
    let mut dropped = turn.drop_worst(3.0 * s.max_error);
    if !progress(0.75) {
        bail!("cancelled");
    }
    turn.solve_focal = s.solve_focal;
    turn.adjust(100);
    for (d, more) in dropped.iter_mut().zip(turn.drop_worst(3.0 * s.max_error)) {
        *d |= more;
    }
    turn.adjust(50);
    progress(1.0);
    // Errors, as sfm reports them.
    let mut sums: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    let mut per_point = vec![(0.0, 0usize); turn.dirs.len()];
    let mut all = (0.0, 0usize);
    for o in &turn.obs {
        let e = turn.residual(o).map_or(1000.0, |r| r[0].hypot(r[1]));
        let sum = sums.entry(frames[o.cam]).or_default();
        sum.0 += e * e;
        sum.1 += 1;
        per_point[o.point].0 += e * e;
        per_point[o.point].1 += 1;
        all.0 += e * e;
        all.1 += 1;
    }
    let rms = |(s, n): (f64, usize)| (s / n.max(1) as f64).sqrt();
    let mut points = vec![None; tracks.len()];
    let mut point_rms = vec![None; tracks.len()];
    for (j, &t) in ids.iter().enumerate() {
        if !dropped[j] {
            points[t] = Some(turn.dirs[j] * distance);
            point_rms[t] = Some(rms(per_point[j]));
        }
    }
    Ok(Solution {
        intr: turn.intr,
        poses: frames.iter().zip(&turn.rots).map(|(f, r)| (*f, Pose { r: *r, t: V3::zeros() })).collect(),
        points,
        rms: rms(all),
        frame_rms: sums.into_iter().map(|(f, sn)| (f, rms(sn))).collect(),
        point_rms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::tests::{noise, FMT};

    /// Points in directions spread ±45° across and ±25° up and down from
    /// straight ahead, `near` to `far` away.
    fn points(n: usize, near: f64, far: f64, seed: u64) -> Vec<V3> {
        let mut r = Rng(seed);
        let mut u = || r.below(1_000_000) as f64 / 1e6;
        (0..n)
            .map(|_| {
                let (az, el) = ((u() * 90.0 - 45.0).to_radians(), (u() * 50.0 - 25.0).to_radians());
                V3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos()) * (near + u() * (far - near))
            })
            .collect()
    }

    /// Each point's track while it's in view, with noise; one in ten jumps
    /// off once.
    fn tracks(k: &Intrinsics, poses: &[Pose], pts: &[V3], s: f64) -> Vec<FeatureTrack> {
        let mut r = Rng(17);
        let mut out = Vec::new();
        for (j, x) in pts.iter().enumerate() {
            let seen: Vec<Option<[f32; 2]>> = poses
                .iter()
                .map(|p| {
                    let q = k.project(&p.apply(x))?;
                    (q[0] > 5.0 && q[1] > 5.0 && q[0] < FMT.width as f64 - 5.0 && q[1] < FMT.height as f64 - 5.0).then_some([q[0] as f32, q[1] as f32])
                })
                .collect();
            let Some(start) = seen.iter().position(Option::is_some) else { continue };
            let mut pts: Vec<[f32; 2]> = seen[start..].iter().map_while(|q| *q).collect();
            if pts.len() < 2 {
                continue;
            }
            for p in pts.iter_mut() {
                p[0] += noise(&mut r, s) as f32;
                p[1] += noise(&mut r, s) as f32;
            }
            if j % 10 == 3 {
                let n = pts.len();
                pts[n / 2][0] += 25.0;
            }
            out.push(FeatureTrack { start: start as i64, points: pts });
        }
        out
    }

    /// 40 frames panning 23° and tilting 7° about the camera's center.
    fn pan() -> (Intrinsics, Vec<Pose>, Vec<FeatureTrack>) {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let poses: Vec<Pose> = (0..40)
            .map(|f| {
                let t = f as f64;
                Pose::from_center(Rotation3::from_euler_angles(0.003 * t, 0.01 * t, 0.001 * t), V3::zeros())
            })
            .collect();
        let tracks = tracks(&k, &poses, &points(1500, 3.0, 60.0, 3), 0.3);
        (k, poses, tracks)
    }

    /// The worst rotation error (degrees) against the truth, relative to the
    /// first frame (which the solve holds at no turn).
    fn rotation_error(sol: &Solution, truth: &[Pose]) -> f64 {
        sol.poses
            .iter()
            .map(|(f, p)| {
                let t = truth[*f as usize].r * truth[0].r.inverse();
                (p.r.inverse() * t).angle().to_degrees()
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn a_homography_is_found_despite_noise_and_mismatches() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let r = Rotation3::from_euler_angles(0.05, 0.2, -0.03);
        let h = kmat(&k) * r.matrix() * kmat(&k).try_inverse().unwrap();
        let mut rng = Rng(3);
        let a: Vec<[f64; 2]> = (0..200).map(|_| [rng.below(960) as f64, rng.below(540) as f64]).collect();
        let mut b: Vec<[f64; 2]> = a.iter().map(|p| apply(&h, *p).unwrap()).collect();
        assert!(dlt(&a[..4], &b[..4]).is_some_and(|g| (g - unit(h)).norm() < 1e-6), "four exact pairs give it exactly");
        for p in b.iter_mut() {
            p[0] += noise(&mut rng, 0.3);
            p[1] += noise(&mut rng, 0.3);
        }
        for i in (0..200).step_by(4) {
            b[i] = [rng.below(960) as f64, rng.below(540) as f64];
        }
        let (g, mask) = homography(&a, &b, 2.0, &mut Rng(1)).unwrap();
        // Every good pair agrees (the noise is well within 2 px); a
        // mismatch only by landing near where it should.
        assert!((0..200).filter(|i| i % 4 != 0).all(|i| mask[i]));
        assert!((0..200).step_by(4).all(|i| !mask[i] || transfer_error(&h, a[i], b[i]) < 3.0));
        let (rg, departure) = rotation_of(&g, &k);
        assert!((rg.inverse() * r).angle().to_degrees() < 0.02, "off by {}°", (rg.inverse() * r).angle().to_degrees());
        assert!(departure < 0.002, "departure {departure}");
        // Too few pairs give nothing.
        assert!(homography(&a[..3], &b[..3], 1.0, &mut Rng(1)).is_none());
    }

    #[test]
    fn a_pan_and_tilt_is_detected_and_solved() {
        let (k, truth, tracks) = pan();
        let s = Settings::default();
        let d = detect(&tracks, 0, 39, &k, &s);
        assert!(d.nodal && d.pairs >= 4, "{d:?}");
        assert!(d.focal.is_some_and(|f| (f / k.f - 1.0).abs() < 0.02), "{d:?} vs {}", k.f);
        let mut calls = 0;
        let sol = solve(&tracks, 0, 39, k, &s, 10.0, &mut |_| {
            calls += 1;
            true
        })
        .unwrap();
        assert_eq!(sol.poses.len(), 40, "every frame solved");
        assert!(sol.poses.values().all(|p| p.center().norm() < 1e-12), "the camera stays put");
        assert!(sol.rms < 0.5, "rms {}", sol.rms);
        let rot = rotation_error(&sol, &truth);
        assert!(rot < 0.1, "rotation off by {rot}°");
        // The tracks that jumped were dropped, most of the rest kept, on
        // the sphere at the distance asked.
        let kept: Vec<V3> = sol.points.iter().flatten().copied().collect();
        assert!(kept.len() * 10 > tracks.len() * 7, "{} of {} points", kept.len(), tracks.len());
        assert!(kept.iter().all(|x| (x.norm() - 10.0).abs() < 1e-9));
        assert!(sol.points.iter().zip(&sol.point_rms).all(|(p, e)| p.is_some() == e.is_some()));
        assert!(sol.point_rms.iter().flatten().all(|e| *e < 1.5));
        assert!(calls >= 40);
        assert!(solve(&tracks, 0, 39, k, &s, 10.0, &mut |f| f < 0.3).unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn the_focal_length_is_solved_from_ten_percent_off() {
        let (k, truth, tracks) = pan();
        let guess = Intrinsics { f: k.f * 1.1, ..k };
        let s = Settings { solve_focal: true, ..Settings::default() };
        assert!(detect(&tracks, 0, 39, &guess, &s).nodal, "detected with the wrong focal length too");
        let sol = solve(&tracks, 0, 39, guess, &s, 10.0, &mut |_| true).unwrap();
        assert!((sol.intr.f / k.f - 1.0).abs() < 0.01, "focal {} vs {}", sol.intr.f, k.f);
        let rot = rotation_error(&sol, &truth);
        assert!(rot < 0.1, "rotation off by {rot}°");
    }

    #[test]
    fn a_shot_with_parallax_is_not_nodal() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        // Trucking right 0.15 a frame past points 4 to 16 away.
        let poses: Vec<Pose> = (0..40)
            .map(|f| {
                let t = f as f64;
                Pose::from_center(Rotation3::from_euler_angles(0.002 * t, -0.006 * t, 0.001 * t), V3::new(0.15 * t, -0.01 * t, 0.02 * t))
            })
            .collect();
        let tracks = tracks(&k, &poses, &points(1500, 4.0, 16.0, 5), 0.3);
        let d = detect(&tracks, 0, 39, &k, &Settings::default());
        assert!(!d.nodal && d.pairs >= 4 && d.error > 5.0, "{d:?}");
    }

    #[test]
    fn a_moving_camera_over_a_flat_scene_is_not_nodal() {
        // A homography explains a plane from anywhere, but it isn't a turn.
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let poses: Vec<Pose> = (0..40).map(|f| Pose::from_center(Rotation3::identity(), V3::new(0.05 * f as f64, 0.0, 0.0))).collect();
        let mut r = Rng(8);
        let mut u = || r.below(1_000_000) as f64 / 1e6;
        let wall: Vec<V3> = (0..1500).map(|_| V3::new(u() * 24.0 - 10.0, u() * 12.0 - 6.0, 10.0)).collect();
        let tracks = tracks(&k, &poses, &wall, 0.3);
        let d = detect(&tracks, 0, 39, &k, &Settings::default());
        assert!(!d.nodal && d.error < 1.0 && d.departure > MAX_DEPARTURE, "{d:?}");
    }
}
