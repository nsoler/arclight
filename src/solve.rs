//! Camera solving from tracked features: the geometry that turns 2D tracks
//! into a camera path and 3D points.
//!
//! Internally this works in the usual computer-vision camera frame: x
//! right, y down, z forward, a pose (R, t) mapping scene points into it
//! (`x_cam = R X + t`), and image points as normalized rays
//! ((u − cx) / f, (v − cy) / f, 1). [`Pose::camera`] turns a pose into a
//! [`geom::Camera`] (y up, looking down −Z).
//!
//! Two views ([`two_view`]): the essential matrix from eight point pairs
//! (Longuet-Higgins' eight-point method, on normalized rays) in RANSAC,
//! with its four decompositions into a rotation and a direction of
//! travel told apart by which puts the points in front of both cameras;
//! points by linear (DLT) triangulation. The scale is arbitrary: the
//! second camera is one unit from the first.

use crate::features::FeatureTrack;
use crate::geom::{self, Mat4};
use crate::image::Size;
use nalgebra::{Matrix3, Matrix4, Rotation3, SymmetricEigen, Vector3, SMatrix};

pub type V3 = Vector3<f64>;

/// A camera's pose: scene → camera (computer-vision frame, see the module
/// notes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub r: Rotation3<f64>,
    pub t: V3,
}

impl Pose {
    pub fn identity() -> Pose {
        Pose { r: Rotation3::identity(), t: V3::zeros() }
    }

    /// Where the camera is, in the scene.
    pub fn center(&self) -> V3 {
        -(self.r.inverse() * self.t)
    }

    /// `x` in this camera's frame.
    pub fn apply(&self, x: &V3) -> V3 {
        self.r * x + self.t
    }

    /// The pose of a camera at `center` turned by `r` (scene → camera).
    pub fn from_center(r: Rotation3<f64>, center: V3) -> Pose {
        Pose { r, t: -(r * center) }
    }

    /// As a [`geom::Camera`]'s world matrix (camera → scene, y up,
    /// looking down −Z).
    pub fn world(&self) -> Mat4 {
        let flip = Matrix3::from_diagonal(&V3::new(1.0, -1.0, -1.0));
        let m = self.r.inverse().matrix() * flip;
        let c = self.center();
        let mut w = Matrix4::identity();
        w.fixed_view_mut::<3, 3>(0, 0).copy_from(&m);
        w.fixed_view_mut::<3, 1>(0, 3).copy_from(&c);
        w
    }

    /// The pose of a [`geom::Camera`] with world matrix `w` (rigid: no
    /// scale).
    pub fn from_world(w: &Mat4) -> Pose {
        let flip = Matrix3::from_diagonal(&V3::new(1.0, -1.0, -1.0));
        let m: Matrix3<f64> = w.fixed_view::<3, 3>(0, 0).into_owned() * flip;
        let r = Rotation3::from_matrix_unchecked(m.transpose());
        Pose::from_center(r, w.fixed_view::<3, 1>(0, 3).into_owned())
    }
}

/// Focal length and principal point, in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intrinsics {
    pub f: f64,
    pub c: [f64; 2],
}

impl Intrinsics {
    /// A camera with focal length `focal` mm on a film back `film_width` mm
    /// wide, centered in a frame of `format`.
    pub fn new(focal: f64, film_width: f64, format: Size) -> Intrinsics {
        let cam = geom::Camera { focal, film_back: [film_width, 1.0], ..geom::Camera::default() };
        Intrinsics { f: cam.focal_pixels(format), c: [format.width as f64 / 2.0, format.height as f64 / 2.0] }
    }

    /// The normalized ray through frame position `p`.
    pub fn ray(&self, p: [f64; 2]) -> V3 {
        V3::new((p[0] - self.c[0]) / self.f, (p[1] - self.c[1]) / self.f, 1.0)
    }

    /// Where camera-frame point `x` lands (None behind the camera).
    pub fn project(&self, x: &V3) -> Option<[f64; 2]> {
        (x.z > 1e-9).then(|| [self.c[0] + self.f * x.x / x.z, self.c[1] + self.f * x.y / x.z])
    }
}

/// Repeatable pseudo-random numbers, for RANSAC.
pub struct Rng(pub u64);

impl Rng {
    pub fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }

    /// `k` distinct indices below `n`.
    pub fn sample(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut out = Vec::with_capacity(k);
        while out.len() < k.min(n) {
            let i = self.below(n);
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }
}

/// The eigenvector of symmetric `m` with the smallest eigenvalue.
fn smallest_eigenvector(m: nalgebra::DMatrix<f64>) -> nalgebra::DVector<f64> {
    let n = m.nrows();
    let e = SymmetricEigen::new(m);
    let i = (0..n).min_by(|&a, &b| e.eigenvalues[a].total_cmp(&e.eigenvalues[b])).unwrap();
    e.eigenvectors.column(i).into_owned()
}

/// The essential matrix from eight or more pairs of normalized rays (`a`
/// in the first view, `b` in the second: `bᵀ E a = 0`), with its singular
/// values set to (1, 1, 0).
pub fn essential(a: &[V3], b: &[V3]) -> Option<Matrix3<f64>> {
    if a.len() < 8 || a.len() != b.len() {
        return None;
    }
    let mut ata = SMatrix::<f64, 9, 9>::zeros();
    for (p, q) in a.iter().zip(b) {
        let row = SMatrix::<f64, 9, 1>::from_column_slice(&[
            q.x * p.x, q.x * p.y, q.x, q.y * p.x, q.y * p.y, q.y, p.x, p.y, 1.0,
        ]);
        ata += row * row.transpose();
    }
    let e = smallest_eigenvector(nalgebra::DMatrix::from_column_slice(9, 9, ata.as_slice()));
    let e = Matrix3::new(e[0], e[1], e[2], e[3], e[4], e[5], e[6], e[7], e[8]);
    let svd = e.svd(true, true);
    let (u, vt) = (svd.u?, svd.v_t?);
    Some(u * Matrix3::from_diagonal(&V3::new(1.0, 1.0, 0.0)) * vt)
}

/// The Sampson distance (squared, normalized units) of pair (`a`, `b`) from
/// essential matrix `e`: a first-order estimate of how far the points are
/// from agreeing with it.
pub fn sampson(e: &Matrix3<f64>, a: &V3, b: &V3) -> f64 {
    let ea = e * a;
    let etb = e.transpose() * b;
    let num = b.dot(&ea).powi(2);
    let den = ea.x * ea.x + ea.y * ea.y + etb.x * etb.x + etb.y * etb.y;
    if den < 1e-30 { f64::INFINITY } else { num / den }
}

/// The four (R, t) an essential matrix allows, |t| = 1.
pub fn decompose(e: &Matrix3<f64>) -> Vec<Pose> {
    let svd = e.svd(true, true);
    let (Some(mut u), Some(mut vt)) = (svd.u, svd.v_t) else { return Vec::new() };
    if u.determinant() < 0.0 {
        u = -u;
    }
    if vt.determinant() < 0.0 {
        vt = -vt;
    }
    let w = Matrix3::new(0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    let t: V3 = u.column(2).into_owned();
    let mut out = Vec::new();
    for r in [u * w * vt, u * w.transpose() * vt] {
        let r = Rotation3::from_matrix_unchecked(r);
        out.push(Pose { r, t });
        out.push(Pose { r, t: -t });
    }
    out
}

/// The point seen along normalized rays `rays[i]` from cameras `poses[i]`,
/// by linear triangulation (the null vector of the stacked projection
/// constraints). None when it's at infinity.
pub fn triangulate(poses: &[Pose], rays: &[V3]) -> Option<V3> {
    let mut ata = Matrix4::<f64>::zeros();
    for (pose, ray) in poses.iter().zip(rays) {
        let m = pose.r.matrix();
        let p = |i: usize| nalgebra::RowVector4::new(m[(i, 0)], m[(i, 1)], m[(i, 2)], pose.t[i]);
        let (x, y) = (ray.x / ray.z, ray.y / ray.z);
        for row in [p(2) * x - p(0), p(2) * y - p(1)] {
            ata += row.transpose() * row;
        }
    }
    let h = smallest_eigenvector(nalgebra::DMatrix::from_column_slice(4, 4, ata.as_slice()));
    (h[3].abs() > 1e-12).then(|| V3::new(h[0] / h[3], h[1] / h[3], h[2] / h[3]))
}

/// The angle (radians) between the rays from two camera centers to `x`.
pub fn parallax(c1: &V3, c2: &V3, x: &V3) -> f64 {
    (x - c1).angle(&(x - c2))
}

/// Two views solved: the second camera's pose relative to the first (the
/// first at the origin, unturned; one unit between them), and the
/// triangulated points of the inlier pairs, by pair index.
#[derive(Clone, Debug)]
pub struct TwoView {
    pub pose: Pose,
    pub points: Vec<(usize, V3)>,
    /// The median parallax of the points, radians.
    pub parallax: f64,
}

/// Solve two views from pairs of frame positions (`a[i]` in the first, `b[i]`
/// in the second): RANSAC on the essential matrix with inliers within
/// `max_error` pixels (Sampson), then the decomposition with the most
/// inliers in front of both cameras, then triangulation, keeping points in
/// front of both with reprojection errors within `max_error` · 2.
pub fn two_view(k: &Intrinsics, a: &[[f64; 2]], b: &[[f64; 2]], max_error: f64, rng: &mut Rng) -> Option<TwoView> {
    let n = a.len().min(b.len());
    if n < 8 {
        return None;
    }
    let ra: Vec<V3> = a[..n].iter().map(|p| k.ray(*p)).collect();
    let rb: Vec<V3> = b[..n].iter().map(|p| k.ray(*p)).collect();
    let thresh = (max_error / k.f).powi(2);
    let inliers_of = |e: &Matrix3<f64>| -> Vec<usize> { (0..n).filter(|&i| sampson(e, &ra[i], &rb[i]) < thresh).collect() };
    let mut best: Vec<usize> = Vec::new();
    let mut iterations = 2000usize;
    let mut i = 0;
    while i < iterations {
        i += 1;
        let s = rng.sample(n, 8);
        let Some(e) = essential(&s.iter().map(|&j| ra[j]).collect::<Vec<_>>(), &s.iter().map(|&j| rb[j]).collect::<Vec<_>>()) else { continue };
        let inl = inliers_of(&e);
        if inl.len() > best.len() {
            best = inl;
            // Enough tries to draw an all-inlier sample with 99.9 % odds.
            let w = best.len() as f64 / n as f64;
            let need = ((1.0 - 0.999f64).ln() / (1.0 - w.powi(8)).max(1e-12).ln()).ceil();
            iterations = iterations.min((need as usize).max(50));
        }
    }
    if best.len() < 8 {
        return None;
    }
    // Refit on all inliers (twice: the refit may gather a few more).
    let mut e = essential(&best.iter().map(|&j| ra[j]).collect::<Vec<_>>(), &best.iter().map(|&j| rb[j]).collect::<Vec<_>>())?;
    let refit = inliers_of(&e);
    if refit.len() >= best.len() {
        best = refit;
        e = essential(&best.iter().map(|&j| ra[j]).collect::<Vec<_>>(), &best.iter().map(|&j| rb[j]).collect::<Vec<_>>())?;
    }
    let first = Pose::identity();
    let solve = |pose: &Pose| -> Vec<(usize, V3)> {
        best.iter()
            .filter_map(|&j| {
                let x = triangulate(&[first, *pose], &[ra[j], rb[j]])?;
                let (x1, x2) = (first.apply(&x), pose.apply(&x));
                if x1.z <= 0.0 || x2.z <= 0.0 {
                    return None;
                }
                let err = |p: [f64; 2], q: [f64; 2]| (p[0] - q[0]).hypot(p[1] - q[1]);
                let ok = err(k.project(&x1)?, a[j]) < 2.0 * max_error && err(k.project(&x2)?, b[j]) < 2.0 * max_error;
                ok.then_some((j, x))
            })
            .collect()
    };
    let (pose, points) = decompose(&e).into_iter().map(|p| (p, solve(&p))).max_by_key(|(_, pts)| pts.len())?;
    if points.len() < 8 {
        return None;
    }
    let (c1, c2) = (first.center(), pose.center());
    let mut angles: Vec<f64> = points.iter().map(|(_, x)| parallax(&c1, &c2, x)).collect();
    angles.sort_by(f64::total_cmp);
    Some(TwoView { pose, parallax: angles[angles.len() / 2], points })
}

/// The pairs of positions tracks share on frames `f1` and `f2`, with the
/// index of each pair's track.
pub fn shared(tracks: &[FeatureTrack], f1: i64, f2: i64) -> (Vec<usize>, Vec<[f64; 2]>, Vec<[f64; 2]>) {
    let mut out = (Vec::new(), Vec::new(), Vec::new());
    for (i, t) in tracks.iter().enumerate() {
        if let (Some(p), Some(q)) = (t.at(f1), t.at(f2)) {
            out.0.push(i);
            out.1.push([p[0] as f64, p[1] as f64]);
            out.2.push([q[0] as f64, q[1] as f64]);
        }
    }
    out
}

/// The frame pair to start a solve from: `first` and the frame after it
/// that sees the most parallax (median triangulation angle) while sharing
/// at least `min_shared` tracks, stopping at the first that reaches
/// `enough` radians. With it, the two-view solution (its point indices are
/// track indices).
pub fn initial_pair(tracks: &[FeatureTrack], k: &Intrinsics, first: i64, last: i64, min_shared: usize, enough: f64, max_error: f64) -> Option<(i64, TwoView)> {
    let mut best: Option<(i64, TwoView)> = None;
    let mut rng = Rng(0x5eed);
    for f2 in first + 1..=last {
        let (ids, a, b) = shared(tracks, first, f2);
        if ids.len() < min_shared {
            break;
        }
        let Some(mut tv) = two_view(k, &a, &b, max_error, &mut rng) else { continue };
        if tv.points.len() < min_shared / 2 {
            continue;
        }
        tv.points.iter_mut().for_each(|(j, _)| *j = ids[*j]);
        let better = best.as_ref().is_none_or(|(_, b)| tv.parallax > b.parallax);
        let done = tv.parallax >= enough;
        if better {
            best = Some((f2, tv));
        }
        if done {
            break;
        }
    }
    best
}

/// The rigid motion taking points `a` closest to `b` (`b ≈ R a + t`, least
/// squares; Kabsch's method), as a pose when `b` are camera-frame points.
pub fn rigid(a: &[V3], b: &[V3]) -> Option<Pose> {
    let n = a.len().min(b.len());
    if n < 3 {
        return None;
    }
    let (ma, mb) = (a[..n].iter().sum::<V3>() / n as f64, b[..n].iter().sum::<V3>() / n as f64);
    let mut cov = Matrix3::<f64>::zeros();
    for (p, q) in a.iter().zip(b) {
        cov += (q - mb) * (p - ma).transpose();
    }
    let svd = cov.svd(true, true);
    let (u, vt) = (svd.u?, svd.v_t?);
    let mut d = Matrix3::identity();
    if (u * vt).determinant() < 0.0 {
        d[(2, 2)] = -1.0;
    }
    let r = Rotation3::from_matrix_unchecked(u * d * vt);
    Some(Pose { r, t: mb - r * ma })
}

/// Polynomial product (coefficients from the constant term up).
fn poly_mul(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, x) in a.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            out[i + j] += x * y;
        }
    }
    out
}

fn poly_eval(p: &[f64], x: f64) -> f64 {
    p.iter().rev().fold(0.0, |acc, c| acc * x + c)
}

/// The real roots of polynomial `p` (constant term first): the eigenvalues
/// of its companion matrix that are (nearly) real, polished by Newton's
/// method.
fn real_roots(p: &[f64]) -> Vec<f64> {
    let scale = p.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    let mut deg = p.len() - 1;
    while deg > 0 && p[deg].abs() <= 1e-12 * scale {
        deg -= 1;
    }
    if deg == 0 {
        return Vec::new();
    }
    let mut c = nalgebra::DMatrix::<f64>::zeros(deg, deg);
    for i in 0..deg {
        c[(0, i)] = -p[deg - 1 - i] / p[deg];
        if i + 1 < deg {
            c[(i + 1, i)] = 1.0;
        }
    }
    let dp: Vec<f64> = (1..=deg).map(|i| p[i] * i as f64).collect();
    c.complex_eigenvalues()
        .iter()
        .filter(|z| z.im.abs() <= 1e-3 * (1.0 + z.re.abs()))
        .map(|z| {
            let mut x = z.re;
            for _ in 0..4 {
                let d = poly_eval(&dp, x);
                if d.abs() > 1e-300 {
                    x -= poly_eval(&p[..=deg], x) / d;
                }
            }
            x
        })
        .collect()
}

/// The poses (up to four) that see scene points `x` along unit rays
/// `rays` (the minimal three-point problem, Grunert's solution): with
/// s₂ = u s₁ and s₃ = v s₁ for the distances along the rays, the law of
/// cosines on the three triangles at the camera leaves a quartic in v.
pub fn p3p(x: &[V3; 3], rays: &[V3; 3]) -> Vec<Pose> {
    let a2 = (x[1] - x[2]).norm_squared();
    let b2 = (x[0] - x[2]).norm_squared();
    let c2 = (x[0] - x[1]).norm_squared();
    if a2 < 1e-24 || b2 < 1e-24 || c2 < 1e-24 {
        return Vec::new();
    }
    let (ca, cb, cg) = (rays[1].dot(&rays[2]), rays[0].dot(&rays[2]), rays[0].dot(&rays[1]));
    // u = N(v) / D(v) from the first two triangles; the third gives
    // D² + N² − 2 cos γ N D − (c²/b²) Q D² = 0, Q = 1 + v² − 2 v cos β.
    let k = (a2 - c2) / b2;
    let n = [1.0 + k, -2.0 * k * cb, k - 1.0];
    let d = [2.0 * cg, -2.0 * ca];
    let q = [1.0, -2.0 * cb, 1.0];
    let dd = poly_mul(&d, &d);
    let nn = poly_mul(&n, &n);
    let nd = poly_mul(&n, &d);
    let qdd = poly_mul(&q, &dd);
    let mut quartic = [0.0; 5];
    for (i, c) in quartic.iter_mut().enumerate() {
        let at = |p: &[f64]| p.get(i).copied().unwrap_or(0.0);
        *c = at(&dd) + at(&nn) - 2.0 * cg * at(&nd) - c2 / b2 * at(&qdd);
    }
    let mut out = Vec::new();
    for v in real_roots(&quartic) {
        let (qv, dv) = (poly_eval(&q, v), poly_eval(&d, v));
        if qv <= 1e-12 || dv.abs() < 1e-12 {
            continue;
        }
        let s1 = (b2 / qv).sqrt();
        let u = poly_eval(&n, v) / dv;
        let s = [s1, u * s1, v * s1];
        if s.iter().any(|&s| s <= 0.0) {
            continue;
        }
        let cam = [rays[0] * s[0], rays[1] * s[1], rays[2] * s[2]];
        out.extend(rigid(x, &cam));
    }
    out
}

/// The pose that sees scene points `x` along normalized rays `rays` (four
/// or more, any number), by EPnP (Lepetit, Moreno-Noguer and Fua): each
/// point is a weighted sum of four control points (three for a flat
/// scene), whose camera-frame positions are a combination of the null
/// vectors of the projection constraints, the weights found from the
/// control points' known distances (for one to three null vectors, the
/// one that reprojects best).
pub fn epnp(x: &[V3], rays: &[V3]) -> Option<Pose> {
    let n = x.len().min(rays.len());
    if n < 4 {
        return None;
    }
    let c0 = x[..n].iter().sum::<V3>() / n as f64;
    let mut cov = Matrix3::<f64>::zeros();
    for p in &x[..n] {
        cov += (p - c0) * (p - c0).transpose();
    }
    let e = SymmetricEigen::new(cov);
    let mut axes: Vec<usize> = (0..3).collect();
    axes.sort_by(|&a, &b| e.eigenvalues[b].total_cmp(&e.eigenvalues[a]));
    if e.eigenvalues[axes[1]] <= 1e-12 * e.eigenvalues[axes[0]].max(1e-300) {
        return None; // on a line
    }
    // A flat scene: the third axis carries next to nothing.
    let flat = e.eigenvalues[axes[2]] < 1e-3 * e.eigenvalues[axes[0]];
    axes.truncate(if flat { 2 } else { 3 });
    let dirs: Vec<V3> = axes.iter().map(|&a| e.eigenvectors.column(a) * (e.eigenvalues[a] / n as f64).sqrt()).collect();
    let mut ctrl = vec![c0];
    ctrl.extend(dirs.iter().map(|d| c0 + d));
    let nc = ctrl.len();
    // The points' weights on the control points (the axes are orthogonal).
    let alphas: Vec<Vec<f64>> = x[..n]
        .iter()
        .map(|p| {
            let mut w: Vec<f64> = dirs.iter().map(|d| (p - c0).dot(d) / d.norm_squared()).collect();
            w.insert(0, 1.0 - w.iter().sum::<f64>());
            w
        })
        .collect();
    let mut mtm = nalgebra::DMatrix::<f64>::zeros(3 * nc, 3 * nc);
    for (al, r) in alphas.iter().zip(rays) {
        let (u, v) = (r.x / r.z, r.y / r.z);
        for row in [[1.0, 0.0, -u], [0.0, 1.0, -v]] {
            let mut m = nalgebra::DVector::<f64>::zeros(3 * nc);
            for (j, a) in al.iter().enumerate() {
                for k in 0..3 {
                    m[3 * j + k] = a * row[k];
                }
            }
            mtm += &m * m.transpose();
        }
    }
    let eig = SymmetricEigen::new(mtm);
    let mut order: Vec<usize> = (0..3 * nc).collect();
    order.sort_by(|&a, &b| eig.eigenvalues[a].total_cmp(&eig.eigenvalues[b]));
    let null: Vec<Vec<V3>> = order
        .iter()
        .take(3)
        .map(|&i| (0..nc).map(|j| V3::new(eig.eigenvectors[(3 * j, i)], eig.eigenvectors[(3 * j + 1, i)], eig.eigenvectors[(3 * j + 2, i)])).collect())
        .collect();
    let pairs: Vec<(usize, usize)> = (0..nc).flat_map(|i| (i + 1..nc).map(move |j| (i, j))).collect();
    let dist: Vec<f64> = pairs.iter().map(|&(i, j)| (ctrl[i] - ctrl[j]).norm_squared()).collect();
    let reproj = |pose: &Pose| -> f64 {
        x[..n].iter().zip(rays).map(|(p, r)| {
            let c = pose.apply(p);
            if c.z <= 1e-12 { 1e3 } else { (c.x / c.z - r.x / r.z).hypot(c.y / c.z - r.y / r.z) }
        }).sum()
    };
    let mut best: Option<(f64, Pose)> = None;
    for nn in 1..=(if flat { 2 } else { 3 }).min(pairs.len()) {
        // Differences of the null vectors between control points.
        let diff = |a: usize, (i, j): (usize, usize)| null[a][i] - null[a][j];
        // Linearized: unknowns βₐ β_b (a ≤ b), least squares.
        let prods: Vec<(usize, usize)> = (0..nn).flat_map(|a| (a..nn).map(move |b| (a, b))).collect();
        let mut l = nalgebra::DMatrix::<f64>::zeros(pairs.len(), prods.len());
        for (r, &pr) in pairs.iter().enumerate() {
            for (c, &(a, b)) in prods.iter().enumerate() {
                l[(r, c)] = if a == b { 1.0 } else { 2.0 } * diff(a, pr).dot(&diff(b, pr));
            }
        }
        let rho = nalgebra::DVector::from_vec(dist.clone());
        let Ok(bb) = l.svd(true, true).solve(&rho, 1e-12) else { continue };
        let b0 = bb[0].abs().sqrt();
        if b0 < 1e-12 {
            continue;
        }
        let mut beta: Vec<f64> = (0..nn).map(|a| if a == 0 { b0 } else { bb[a] / b0 }).collect();
        // Gauss–Newton on the distances themselves.
        for _ in 0..10 {
            let mut jtj = nalgebra::DMatrix::<f64>::zeros(nn, nn);
            let mut jtr = nalgebra::DVector::<f64>::zeros(nn);
            for (&pr, &d2) in pairs.iter().zip(&dist) {
                let v: V3 = (0..nn).map(|a| diff(a, pr) * beta[a]).sum();
                let res = v.norm_squared() - d2;
                let jac: Vec<f64> = (0..nn).map(|a| 2.0 * v.dot(&diff(a, pr))).collect();
                for a in 0..nn {
                    jtr[a] += jac[a] * res;
                    for b in 0..nn {
                        jtj[(a, b)] += jac[a] * jac[b];
                    }
                }
            }
            let Some(step) = jtj.cholesky().map(|c| c.solve(&jtr)) else { break };
            for a in 0..nn {
                beta[a] -= step[a];
            }
        }
        let cc: Vec<V3> = (0..nc).map(|j| (0..nn).map(|a| null[a][j] * beta[a]).sum()).collect();
        let mut cam: Vec<V3> = alphas.iter().map(|al| al.iter().zip(&cc).map(|(a, c)| c * *a).sum()).collect();
        if cam.iter().map(|c| c.z).sum::<f64>() < 0.0 {
            cam.iter_mut().for_each(|c| *c = -*c);
        }
        let Some(pose) = rigid(&x[..n], &cam) else { continue };
        let err = reproj(&pose);
        if best.as_ref().is_none_or(|b| err < b.0) {
            best = Some((err, pose));
        }
    }
    best.map(|b| b.1)
}

/// Pose `start` refined against the scene points `x` it sees at `px` (only
/// those listed in `use_`), by a cameras-only bundle adjustment.
fn refine_pose(x: &[V3], px: &[[f64; 2]], k: &Intrinsics, use_: &[usize], start: Pose, max_error: f64) -> Pose {
    let mut p = crate::bundle::Problem {
        poses: vec![start],
        points: use_.iter().map(|&i| x[i]).collect(),
        obs: use_.iter().enumerate().map(|(j, &i)| crate::bundle::Obs { cam: 0, point: j, px: px[i] }).collect(),
        intr: *k,
        fixed: vec![false],
        points_fixed: true,
        solve_focal: false,
        huber: max_error,
    };
    p.adjust(30);
    p.poses[0]
}

/// The camera that sees scene points `x` at frame positions `px` (pose
/// from perspective-n-point), and which points agree with it (within
/// `max_error` pixels): P3P samples, each checked against a fourth point,
/// in RANSAC with an adaptive number of tries; EPnP on all points as a
/// further candidate (it copes where P3P samples get unstable, as on a
/// flat scene); then EPnP on the agreeing points, and a Levenberg–Marquardt
/// refinement on them. None with fewer than four points, or fewer than
/// four agreeing.
pub fn pnp(x: &[V3], px: &[[f64; 2]], k: &Intrinsics, max_error: f64) -> Option<(Pose, Vec<usize>)> {
    let n = x.len().min(px.len());
    if n < 4 {
        return None;
    }
    let rays: Vec<V3> = px[..n].iter().map(|p| k.ray(*p)).collect();
    let units: Vec<V3> = rays.iter().map(|r| r.normalize()).collect();
    let err = |pose: &Pose, i: usize| k.project(&pose.apply(&x[i])).map_or(f64::INFINITY, |q| (q[0] - px[i][0]).hypot(q[1] - px[i][1]));
    let inliers_of = |pose: &Pose| -> Vec<usize> { (0..n).filter(|&i| err(pose, i) < max_error).collect() };
    let mut best: Option<(Pose, Vec<usize>)> = None;
    let consider = |pose: Pose, best: &mut Option<(Pose, Vec<usize>)>| -> bool {
        let inl = inliers_of(&pose);
        let better = best.as_ref().is_none_or(|b| inl.len() > b.1.len());
        if better {
            *best = Some((pose, inl));
        }
        better
    };
    let mut rng = Rng(0x9e37_79b9);
    let (mut tries, mut i) = (1000usize, 0);
    while i < tries {
        i += 1;
        let s = rng.sample(n, 4);
        for pose in p3p(&[x[s[0]], x[s[1]], x[s[2]]], &[units[s[0]], units[s[1]], units[s[2]]]) {
            if err(&pose, s[3]) < max_error && consider(pose, &mut best) {
                // Enough tries to draw an all-inlier sample with 99.9 % odds.
                let w = best.as_ref().unwrap().1.len() as f64 / n as f64;
                let need = ((1.0 - 0.999f64).ln() / (1.0 - w.powi(3)).max(1e-12).ln()).ceil();
                tries = tries.min((need as usize).max(30));
            }
        }
    }
    if let Some(pose) = epnp(&x[..n], &rays) {
        consider(pose, &mut best);
    }
    let (mut pose, mut inl) = best?;
    if inl.len() >= 6 {
        let (xs, rs): (Vec<V3>, Vec<V3>) = inl.iter().map(|&i| (x[i], rays[i])).unzip();
        if let Some(p) = epnp(&xs, &rs) {
            let more = inliers_of(&p);
            if more.len() >= inl.len() {
                (pose, inl) = (p, more);
            }
        }
    }
    // Refine, gather the points that now agree, and refine on those.
    for _ in 0..2 {
        if inl.len() < 4 {
            return None;
        }
        pose = refine_pose(x, px, k, &inl, pose, max_error);
        inl = inliers_of(&pose);
    }
    (inl.len() >= 4).then_some((pose, inl))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const FMT: Size = Size { width: 960, height: 540 };

    /// Points scattered in a box 4–12 units in front of the origin camera
    /// (computer-vision frame, z forward).
    pub(crate) fn cloud(n: usize, seed: u64) -> Vec<V3> {
        let mut r = Rng(seed);
        let mut u = || r.below(1_000_000) as f64 / 1e6;
        (0..n).map(|_| V3::new(u() * 8.0 - 4.0, u() * 5.0 - 2.5, 4.0 + u() * 8.0)).collect()
    }

    /// Gaussian-ish noise (sum of uniforms), standard deviation `s`.
    pub(crate) fn noise(r: &mut Rng, s: f64) -> f64 {
        (0..6).map(|_| r.below(1_000_000) as f64 / 1e6 - 0.5).sum::<f64>() * s * (2.0f64).sqrt()
    }

    /// Degrees between two rotations (clamped: nalgebra's `angle` is NaN
    /// for a product a rounding error past the identity).
    fn rot_angle(a: &Rotation3<f64>, b: &Rotation3<f64>) -> f64 {
        (((a.inverse() * b).matrix().trace() - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
    }

    #[test]
    fn poses_convert_to_cameras_and_back() {
        let pose = Pose::from_center(Rotation3::from_euler_angles(0.1, -0.3, 0.2), V3::new(1.0, 2.0, -3.0));
        let cam = geom::Camera { focal: 35.0, world: pose.world(), ..geom::Camera::default() };
        let k = Intrinsics::new(35.0, 36.0, FMT);
        // The same scene point lands in the same place either way.
        for x in [V3::new(0.5, 1.0, 9.0), V3::new(-2.0, 3.0, 4.0)] {
            let (a, b) = (k.project(&pose.apply(&x)), cam.project(&x, FMT).map(|p| p.0));
            assert_eq!(a.is_some(), b.is_some());
            if let (Some(a), Some(b)) = (a, b) {
                assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} {b:?}");
            }
        }
        let back = Pose::from_world(&pose.world());
        assert!(rot_angle(&back.r, &pose.r) < 1e-9 && (back.t - pose.t).norm() < 1e-9);
    }

    #[test]
    fn triangulation_and_decomposition_are_exact_without_noise() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let p2 = Pose::from_center(Rotation3::from_euler_angles(0.02, 0.15, -0.03), V3::new(1.5, 0.2, 0.3));
        let pts = cloud(40, 1);
        let a: Vec<[f64; 2]> = pts.iter().map(|x| k.project(x).unwrap()).collect();
        let b: Vec<[f64; 2]> = pts.iter().map(|x| k.project(&p2.apply(x)).unwrap()).collect();
        let x = triangulate(&[Pose::identity(), p2], &[k.ray(a[0]), k.ray(b[0])]).unwrap();
        assert!((x - pts[0]).norm() < 1e-9, "{x} vs {}", pts[0]);
        let e = essential(&a.iter().map(|p| k.ray(*p)).collect::<Vec<_>>(), &b.iter().map(|p| k.ray(*p)).collect::<Vec<_>>()).unwrap();
        let truth = decompose(&e).into_iter().find(|p| rot_angle(&p.r, &p2.r) < 1e-6 && (p.t - p2.t.normalize()).norm() < 1e-6);
        assert!(truth.is_some(), "the true pose is one of the four");
    }

    #[test]
    fn two_views_recover_motion_and_structure_despite_noise_and_outliers() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let p2 = Pose::from_center(Rotation3::from_euler_angles(-0.03, 0.12, 0.05), V3::new(1.2, -0.3, 0.6));
        let pts = cloud(200, 2);
        let mut r = Rng(9);
        let mut a: Vec<[f64; 2]> = pts.iter().map(|x| k.project(x).unwrap()).collect();
        let mut b: Vec<[f64; 2]> = pts.iter().map(|x| k.project(&p2.apply(x)).unwrap()).collect();
        for p in a.iter_mut().chain(b.iter_mut()) {
            p[0] += noise(&mut r, 0.3);
            p[1] += noise(&mut r, 0.3);
        }
        // One in five pairs is a mismatch.
        for i in (0..200).step_by(5) {
            b[i] = [r.below(960) as f64, r.below(540) as f64];
        }
        let tv = two_view(&k, &a, &b, 1.0, &mut Rng(1)).unwrap();
        assert!(rot_angle(&tv.pose.r, &p2.r) < 0.1, "rotation off by {}°", rot_angle(&tv.pose.r, &p2.r));
        let dir = tv.pose.t.angle(&p2.t.normalize()).to_degrees();
        assert!(dir < 2.0, "direction off by {dir}°");
        assert!(tv.points.iter().all(|(i, _)| i % 5 != 0) && tv.points.len() >= 140, "{} points", tv.points.len());
        // Up to scale (the true baseline), the points are where they were.
        let scale = p2.t.norm();
        let mut errs: Vec<f64> = tv.points.iter().map(|(i, x)| (x * scale - pts[*i]).norm() / pts[*i].norm()).collect();
        errs.sort_by(f64::total_cmp);
        assert!(errs[errs.len() / 2] < 0.01, "median relative error {}", errs[errs.len() / 2]);
        assert!(tv.parallax > 0.05);
    }

    /// Tracks of `pts` seen by `poses` (one per frame from 0), with noise:
    /// what the feature tracker would give, without images.
    pub(crate) fn synthetic_tracks(k: &Intrinsics, poses: &[Pose], pts: &[V3], s: f64) -> Vec<FeatureTrack> {
        let mut r = Rng(77);
        pts.iter()
            .map(|x| {
                let points = poses
                    .iter()
                    .map_while(|p| {
                        let q = k.project(&p.apply(x)).filter(|q| q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= FMT.width as f64 && q[1] <= FMT.height as f64)?;
                        Some([(q[0] + noise(&mut r, s)) as f32, (q[1] + noise(&mut r, s)) as f32])
                    })
                    .collect();
                FeatureTrack { start: 0, points }
            })
            .filter(|t| !t.points.is_empty())
            .collect()
    }

    #[test]
    fn the_initial_pair_waits_for_parallax() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let pts = cloud(300, 4);
        // Sliding sideways 0.1 a frame, turning a little to keep looking in.
        let slide: Vec<Pose> = (0..30).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.0, -0.004 * f as f64, 0.0), V3::new(0.1 * f as f64, 0.0, 0.0))).collect();
        let tracks = synthetic_tracks(&k, &slide, &pts, 0.2);
        let (f2, tv) = initial_pair(&tracks, &k, 0, 29, 50, 3f64.to_radians(), 1.0).unwrap();
        assert!(tv.parallax >= 3f64.to_radians() && f2 > 3 && f2 < 29, "frame {f2}, parallax {}°", tv.parallax.to_degrees());
        let dir = tv.pose.t.angle(&slide[f2 as usize].t.normalize()).to_degrees();
        assert!(dir < 3.0, "direction off by {dir}°");
        // Turning on the spot gives no parallax to start from.
        let pan: Vec<Pose> = (0..30).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.0, 0.01 * f as f64, 0.0), V3::zeros())).collect();
        let tracks = synthetic_tracks(&k, &pan, &pts, 0.2);
        let best = initial_pair(&tracks, &k, 0, 29, 50, 3f64.to_radians(), 1.0);
        assert!(best.is_none_or(|(_, tv)| tv.parallax < 0.5f64.to_radians()));
    }

    /// Points `x` seen by `pose`, projected.
    fn seen(k: &Intrinsics, pose: &Pose, x: &[V3]) -> Vec<[f64; 2]> {
        x.iter().map(|p| k.project(&pose.apply(p)).unwrap()).collect()
    }

    #[test]
    fn pnp_is_exact_without_noise() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let truth = Pose::from_center(Rotation3::from_euler_angles(0.1, -0.2, 0.05), V3::new(0.7, -0.3, 0.4));
        // The cloud as the camera at the origin sees it, moved into the scene.
        let pts: Vec<V3> = cloud(60, 31).iter().map(|c| truth.r.inverse() * (c - truth.t)).collect();
        let px = seen(&k, &truth, &pts);
        let units: Vec<V3> = px.iter().map(|p| k.ray(*p).normalize()).collect();
        // P3P: the true pose is among the up to four.
        let cands = p3p(&[pts[0], pts[1], pts[2]], &[units[0], units[1], units[2]]);
        assert!(cands.iter().any(|p| rot_angle(&p.r, &truth.r) < 1e-6 && (p.t - truth.t).norm() < 1e-6), "{} candidates", cands.len());
        // EPnP on them all.
        let rays: Vec<V3> = px.iter().map(|p| k.ray(*p)).collect();
        let e = epnp(&pts, &rays).unwrap();
        assert!(rot_angle(&e.r, &truth.r) < 1e-6 && (e.t - truth.t).norm() < 1e-6, "{e:?}");
        let (pose, inliers) = pnp(&pts, &px, &k, 1.0).unwrap();
        assert!(rot_angle(&pose.r, &truth.r) < 1e-6 && (pose.t - truth.t).norm() < 1e-6);
        assert_eq!(inliers.len(), 60);
    }

    #[test]
    fn pnp_shrugs_off_noise_and_outliers() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let truth = Pose::from_center(Rotation3::from_euler_angles(-0.05, 0.3, 0.02), V3::new(-1.0, 0.2, 0.5));
        let pts: Vec<V3> = cloud(200, 32).iter().map(|c| truth.r.inverse() * (c - truth.t)).collect();
        let mut r = Rng(4);
        let mut px = seen(&k, &truth, &pts);
        for p in px.iter_mut() {
            p[0] += noise(&mut r, 0.5);
            p[1] += noise(&mut r, 0.5);
        }
        // Three in ten are mismatches.
        for i in (0..200).filter(|i| i % 10 < 3) {
            px[i] = [r.below(960) as f64, r.below(540) as f64];
        }
        let (pose, inliers) = pnp(&pts, &px, &k, 1.0).unwrap();
        let dist = pts.iter().map(|p| (p - truth.center()).norm()).sum::<f64>() / pts.len() as f64;
        assert!(rot_angle(&pose.r, &truth.r) < 0.1, "rotation off by {}°", rot_angle(&pose.r, &truth.r));
        assert!((pose.center() - truth.center()).norm() < 0.01 * dist, "center off by {}", (pose.center() - truth.center()).norm());
        assert!(inliers.iter().all(|i| i % 10 >= 3) && inliers.len() > 110, "{} inliers", inliers.len());
    }

    #[test]
    fn pnp_solves_a_flat_scene() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        // A floor 2 below, seen from above at an angle.
        let mut r = Rng(8);
        let mut u = || r.below(1_000_000) as f64 / 1e6;
        let pts: Vec<V3> = (0..80).map(|_| V3::new(u() * 8.0 - 4.0, 2.0, 5.0 + u() * 8.0)).collect();
        let truth = Pose::from_center(Rotation3::from_euler_angles(0.25, 0.1, 0.0), V3::new(0.3, -0.5, 0.0));
        let mut r = Rng(9);
        let mut px = seen(&k, &truth, &pts);
        let rays: Vec<V3> = px.iter().map(|p| k.ray(*p)).collect();
        let e = epnp(&pts, &rays).unwrap();
        assert!(rot_angle(&e.r, &truth.r) < 1e-6 && (e.t - truth.t).norm() < 1e-6, "flat EPnP {e:?}");
        for p in px.iter_mut() {
            p[0] += noise(&mut r, 0.3);
            p[1] += noise(&mut r, 0.3);
        }
        let (pose, inliers) = pnp(&pts, &px, &k, 1.0).unwrap();
        assert!(rot_angle(&pose.r, &truth.r) < 0.1 && (pose.center() - truth.center()).norm() < 0.05, "{pose:?}");
        assert!(inliers.len() > 70);
    }

    #[test]
    fn pnp_needs_four_points() {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let pts = cloud(3, 5);
        let px = seen(&k, &Pose::identity(), &pts);
        assert!(pnp(&pts, &px, &k, 1.0).is_none());
        assert!(pnp(&[], &[], &k, 1.0).is_none());
    }
}
