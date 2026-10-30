//! Gaussian splats in a render (see `docs/splat-plan.md`).
//!
//! Each splat is a 3D Gaussian: a center, a covariance from its scales and
//! rotation, an opacity and a color that changes with the direction it's
//! seen from (spherical harmonics: [`Splats::color`]). Per view, [`prepare`] puts every splat
//! in camera space, drops those behind the near plane or whose ellipse
//! (out to 3σ) misses the frame, projects the covariance to a 2D ellipse
//! on the frame through the projection's Jacobian at the center (the EWA
//! approximation, widened by a third of a pixel so none is thinner than a
//! pixel), and works out its color for the direction from the camera (in
//! the capture's own space). The covariance goes through the capture's
//! whole placement (Σ' = M Σ Mᵀ), so a mirrored or stretched one works. The
//! projected splats are binned into 16 × 16 pixel tiles and sorted by
//! depth within each tile. [`Prepared::fragments`] then gives a pixel's
//! splats front to back, each with its alpha there (opacity times the
//! Gaussian at the pixel's middle), until what's left showing through
//! falls below [`T_MIN`]; the renderer blends them in depth order with the
//! surfaces' samples (`draw_passes`), so splats and see-through surfaces
//! mix, opaque ones and holdouts cut them.

use crate::color::Rgba;
use crate::geom::{self, Camera, Mat4, Vec3};
use crate::image::Size;
use crate::splat::Splats;
use rayon::prelude::*;
use std::sync::Arc;

/// Pixels a tile is across.
const TILE: usize = 16;
/// The blend stops once less than this shows through.
pub(super) const T_MIN: f32 = 1e-4;
/// Alphas below this add nothing.
const ALPHA_MIN: f32 = 1.0 / 255.0;
/// No splat is quite opaque (so those behind still count a little).
const ALPHA_MAX: f32 = 0.99;
/// Added to the projected covariance's diagonal (pixels²): every splat at
/// least about a pixel across.
const DILATE: f64 = 0.3;

/// A splat as seen: where its center lands (frame pixels), its ellipse as
/// the inverse 2D covariance (xx, xy, yy), its depth, color and opacity.
#[derive(Clone, Copy, Debug)]
struct Projected {
    center: [f32; 2],
    conic: [f32; 3],
    depth: f32,
    color: [f32; 3],
    opacity: f32,
    /// Which set it's from, and which splat of it.
    set: u32,
    src: u32,
}

/// One splat's share at a pixel: its depth, premultiplied color, and which
/// of the view's splats it is (for [`Prepared::set_of`], normals, motion).
#[derive(Clone, Copy, Debug)]
pub(super) struct Fragment {
    pub depth: f64,
    pub color: Rgba,
    pub splat: u32,
}

/// A view's splats, projected, binned into tiles and sorted by depth
/// (kept for re-renders of the same view: see [`prepare`]).
struct View {
    splats: Vec<Projected>,
    tiles_x: usize,
    /// Per tile: its range in `order`.
    ranges: Vec<(u32, u32)>,
    /// Splat indices, by tile then nearest first.
    order: Vec<u32>,
    /// Per set: its node, and whether it's a holdout.
    nodes: Vec<String>,
    holdout: Vec<bool>,
}

impl View {
    fn bytes(&self) -> usize {
        self.splats.len() * std::mem::size_of::<Projected>() + self.order.len() * 4 + self.ranges.len() * 8
    }
}

/// A view's splats ready to blend, and per splat, when asked for, its
/// normal (see [`add_normals`](Prepared::add_normals)) and how far its
/// center moves on the frame by the next frame.
pub(super) struct Prepared {
    view: Arc<View>,
    normals: Vec<[f32; 3]>,
    motion: Vec<[f32; 2]>,
}

/// A prepared view as kept: what it's of.
struct Kept {
    sets: Vec<Arc<Splats>>,
    cam: Camera,
    format: Size,
    degree: u8,
    view: Arc<View>,
}

/// The views prepared last (newest last), so a re-render that changes
/// nothing about the splats or the camera (a pass, the anti-aliasing,
/// something else in the scene) doesn't project and sort them again.
static KEPT: std::sync::Mutex<Vec<Kept>> = std::sync::Mutex::new(Vec::new());
/// How many views, and how much memory, [`KEPT`] holds at most (more
/// views in tests, which run side by side).
const KEPT_VIEWS: usize = if cfg!(test) { 64 } else { 4 };
const KEPT_BYTES: usize = 1 << 30;

/// Whether two splat sets are the same capture placed the same way (the
/// same arrays, not equal copies).
fn same(a: &Splats, b: &Splats) -> bool {
    Arc::ptr_eq(&a.positions, &b.positions)
        && Arc::ptr_eq(&a.scales, &b.scales)
        && Arc::ptr_eq(&a.rotations, &b.rotations)
        && Arc::ptr_eq(&a.opacity, &b.opacity)
        && Arc::ptr_eq(&a.sh, &b.sh)
        && a.sh_degree == b.sh_degree
        && a.world == b.world
        && a.holdout == b.holdout
        && a.node == b.node
}

/// Rotation matrix of quaternion `q` (w, x, y, z; normalized here).
fn rotation(q: [f32; 4]) -> nalgebra::Matrix3<f64> {
    let [w, x, y, z] = q.map(f64::from);
    let n = (w * w + x * x + y * y + z * z).sqrt();
    let (w, x, y, z) = if n > 0.0 { (w / n, x / n, y / n, z / n) } else { (1.0, 0.0, 0.0, 0.0) };
    nalgebra::Matrix3::new(
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - w * z),
        2.0 * (x * z + w * y),
        2.0 * (x * y + w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - w * x),
        2.0 * (x * z - w * y),
        2.0 * (y * z + w * x),
        1.0 - 2.0 * (x * x + y * y),
    )
}

/// The linear (3 × 3) part of `m`.
fn linear(m: &Mat4) -> nalgebra::Matrix3<f64> {
    m.fixed_view::<3, 3>(0, 0).into_owned()
}

/// Splat `i` of `s` seen through `cam` in a frame of `format` (`to_cam`:
/// its space to the camera's; `eye_own`: the camera in its space, or for
/// an orthographic camera the way it looks), or None if it doesn't show.
#[allow(clippy::too_many_arguments)]
fn project(s: &Splats, i: usize, cam: &Camera, format: Size, to_cam: &Mat4, eye_own: &Vec3, set: usize, degree: u8) -> Option<Projected> {
    let opacity = s.opacity[i];
    if opacity < ALPHA_MIN {
        return None;
    }
    let pos = Vec3::from(s.positions[i].map(f64::from));
    let p = geom::transform_point(to_cam, &pos);
    let depth = -p.z;
    if depth < cam.near.max(1e-9) || depth > cam.far {
        return None;
    }
    let f = cam.focal_pixels(format);
    let (center, j) = if cam.is_ortho() {
        // Flat: the same scale everywhere, nothing for depth.
        let k = cam.pixels_per_unit(depth, format);
        ([format.width as f64 / 2.0 + k * p.x, format.height as f64 / 2.0 - k * p.y], nalgebra::Matrix2x3::new(k, 0.0, 0.0, 0.0, -k, 0.0))
    } else {
        let center = [format.width as f64 / 2.0 + f * p.x / depth, format.height as f64 / 2.0 - f * p.y / depth];
        (center, nalgebra::Matrix2x3::new(f / depth, 0.0, f * p.x / (depth * depth), 0.0, -f / depth, -f * p.y / (depth * depth)))
    };
    // Its covariance in camera space, then on the frame.
    let r = rotation(s.rotations[i]);
    let sc = s.scales[i].map(f64::from);
    let m = linear(to_cam) * r * nalgebra::Matrix3::from_diagonal(&nalgebra::Vector3::new(sc[0], sc[1], sc[2]));
    let cov = m * m.transpose();
    let c2 = j * cov * j.transpose();
    let (a, b, c) = (c2[(0, 0)] + DILATE, c2[(0, 1)], c2[(1, 1)] + DILATE);
    let det = a * c - b * b;
    if det <= 0.0 {
        return None;
    }
    let mid = (a + c) / 2.0;
    let reach = 3.0 * (mid + (mid * mid - det).max(0.0).sqrt()).sqrt();
    if center[0] + reach < 0.0 || center[1] + reach < 0.0 || center[0] - reach > format.width as f64 || center[1] - reach > format.height as f64 {
        return None;
    }
    let look = if cam.is_ortho() { *eye_own } else { pos - eye_own };
    let dir = look.try_normalize(1e-300).map_or([0.0, 0.0, 1.0], |d| [d.x as f32, d.y as f32, d.z as f32]);
    let color = crate::splat::sh_color(s.sh_of(i), degree.min(s.sh_degree), dir);
    Some(Projected { center: center.map(|v| v as f32), conic: [(c / det) as f32, (-b / det) as f32, (a / det) as f32], depth: depth as f32, color, opacity: opacity.min(ALPHA_MAX), set: set as u32, src: i as u32 })
}

/// How far out (squared, in σ: the Gaussian's exponent ×2) splat `s`
/// counts: 3σ, or nearer where it fades below [`ALPHA_MIN`] first.
fn reach2(s: &Projected) -> f32 {
    (2.0 * (s.opacity / ALPHA_MIN).ln()).clamp(0.0, 9.0)
}

/// The tiles (x0, x1, y0, y1, inclusive) the box round splat `s`'s
/// ellipse covers, out to [`reach2`].
fn tile_box(s: &Projected, tiles_x: usize, tiles_y: usize) -> [u16; 4] {
    let det = (s.conic[0] * s.conic[2] - s.conic[1] * s.conic[1]).recip();
    let r = reach2(s).sqrt();
    let (rx, ry) = (r * (s.conic[2] * det).sqrt(), r * (s.conic[0] * det).sqrt());
    let t = |v: f32, n: usize| ((v / TILE as f32).floor().max(0.0) as usize).min(n - 1) as u16;
    [t(s.center[0] - rx, tiles_x), t(s.center[0] + rx, tiles_x), t(s.center[1] - ry, tiles_y), t(s.center[1] + ry, tiles_y)]
}

/// Whether splat `s`'s ellipse (out to [`reach2`]) reaches tile (`tx`,
/// `ty`): the least of its quadratic form over the tile, found on the
/// tile's edges when the center is outside, is within reach.
fn reaches(s: &Projected, tx: usize, ty: usize) -> bool {
    let (x0, y0) = ((tx * TILE) as f32 - s.center[0], (ty * TILE) as f32 - s.center[1]);
    let (x1, y1) = (x0 + TILE as f32, y0 + TILE as f32);
    if x0 <= 0.0 && 0.0 <= x1 && y0 <= 0.0 && 0.0 <= y1 {
        return true;
    }
    let [a, b, c] = s.conic;
    let q = |x: f32, y: f32| a * x * x + 2.0 * b * x * y + c * y * y;
    // Along an edge at x (or y) fixed, the least is where the derivative
    // is 0, kept on the edge.
    let along_y = |x: f32| q(x, (-b * x / c).clamp(y0, y1));
    let along_x = |y: f32| q((-b * y / a).clamp(x0, x1), y);
    along_y(x0).min(along_y(x1)).min(along_x(y0)).min(along_x(y1)) <= reach2(s)
}

/// `sets` seen through `cam` in a frame of `format`, ready to blend; None
/// when none shows. The same view again (see [`KEPT`]) isn't worked out
/// again.
pub(super) fn prepare(sets: &[Arc<Splats>], cam: &Camera, format: Size) -> Option<Prepared> {
    prepare_upto(sets, cam, format, 3)
}

/// [`prepare`], colors from at most SH degree `degree` (0: the same from
/// every side; 1 is close to the full color, and quicker: the 3D view).
pub(super) fn prepare_upto(sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Option<Prepared> {
    if sets.iter().all(|s| s.is_empty()) {
        return None;
    }
    let found = {
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        let hit = kept.iter().position(|k| k.cam == *cam && k.format == format && k.degree == degree && k.sets.len() == sets.len() && k.sets.iter().zip(sets).all(|(a, b)| same(a, b)));
        hit.map(|i| {
            let k = kept.remove(i);
            let view = k.view.clone();
            kept.push(k);
            view
        })
    };
    let view = match found {
        Some(v) => v,
        None => {
            let v = Arc::new(project_all(sets, cam, format, degree)?);
            let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
            kept.push(Kept { sets: sets.to_vec(), cam: cam.clone(), format, degree, view: v.clone() });
            while kept.len() > KEPT_VIEWS || (kept.len() > 1 && kept.iter().map(|k| k.view.bytes()).sum::<usize>() > KEPT_BYTES) {
                kept.remove(0);
            }
            v
        }
    };
    (!view.splats.is_empty()).then(|| Prepared { view, normals: Vec::new(), motion: Vec::new() })
}

/// [`prepare_upto`]'s work: project, cull, bin and sort.
fn project_all(sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Option<View> {
    let view = cam.view();
    let eye = geom::transform_point(&cam.world, &Vec3::zeros());
    let splats: Vec<Projected> = sets
        .iter()
        .enumerate()
        .flat_map(|(k, s)| {
            let to_cam = view * s.world;
            let own = s.world.try_inverse().unwrap_or_else(Mat4::identity);
            let eye_own = if cam.is_ortho() { linear(&own) * -cam.to_eye(&Vec3::zeros()) } else { geom::transform_point(&own, &eye) };
            let n = s.positions.len().min(s.scales.len()).min(s.rotations.len()).min(s.opacity.len());
            let n = n.min(s.sh.len() / s.sh_stride().max(1));
            (0..n).into_par_iter().filter_map(|i| project(s, i, cam, format, &to_cam, &eye_own, k, degree)).collect::<Vec<_>>()
        })
        .collect();
    if splats.is_empty() {
        return Some(View { splats, tiles_x: 0, ranges: Vec::new(), order: Vec::new(), nodes: Vec::new(), holdout: Vec::new() });
    }
    let tiles_x = format.width.div_ceil(TILE);
    let tiles_y = format.height.div_ceil(TILE);
    // Each splat's tiles (the box round its ellipse, out to where it fades
    // below ALPHA_MIN or 3σ), nearest first; then, a row of tiles at a
    // time, each tile's splats in that order where the ellipse reaches it.
    let boxes: Vec<[u16; 4]> = splats.par_iter().map(|s| tile_box(s, tiles_x, tiles_y)).collect();
    let mut by_depth: Vec<u32> = (0..splats.len() as u32).collect();
    by_depth.par_sort_unstable_by_key(|&i| splats[i as usize].depth.to_bits());
    // Each row of tiles' splats, nearest first: from runs of them in depth
    // order, put together in that order.
    let runs: Vec<Vec<Vec<u32>>> = by_depth
        .par_chunks(by_depth.len().div_ceil(rayon::current_num_threads() * 4).max(1))
        .map(|run| {
            let mut rows = vec![Vec::new(); tiles_y];
            for &i in run {
                let b = boxes[i as usize];
                for row in &mut rows[b[2] as usize..=b[3] as usize] {
                    row.push(i);
                }
            }
            rows
        })
        .collect();
    let rows: Vec<Vec<Vec<u32>>> = (0..tiles_y)
        .into_par_iter()
        .map(|ty| {
            let mut tiles = vec![Vec::new(); tiles_x];
            for &i in runs.iter().flat_map(|r| &r[ty]) {
                let b = boxes[i as usize];
                for (tx, tile) in tiles.iter_mut().enumerate().take(b[1] as usize + 1).skip(b[0] as usize) {
                    if reaches(&splats[i as usize], tx, ty) {
                        tile.push(i);
                    }
                }
            }
            tiles
        })
        .collect();
    let mut ranges = Vec::with_capacity(tiles_x * tiles_y);
    let mut at = 0u32;
    for tile in rows.iter().flatten() {
        ranges.push((at, at + tile.len() as u32));
        at += tile.len() as u32;
    }
    let order: Vec<u32> = rows.into_par_iter().flat_map_iter(|row| row.into_iter().flatten()).collect();
    let nodes = sets.iter().map(|s| s.node.clone()).collect();
    let holdout = sets.iter().map(|s| s.holdout).collect();
    Some(View { splats, tiles_x, ranges, order, nodes, holdout })
}

impl Prepared {
    /// Pixel (`x`, `y`)'s splats into `out` (cleared first), nearest
    /// first, until less than [`T_MIN`] shows through them.
    pub(super) fn fragments(&self, x: usize, y: usize, out: &mut Vec<Fragment>) {
        out.clear();
        let Some(&(a, b)) = self.view.ranges.get((y / TILE) * self.view.tiles_x + x / TILE) else { return };
        let q = [x as f32 + 0.5, y as f32 + 0.5];
        let mut through = 1.0f32;
        for &i in &self.view.order[a as usize..b as usize] {
            let s = &self.view.splats[i as usize];
            let (dx, dy) = (q[0] - s.center[0], q[1] - s.center[1]);
            let power = -0.5 * (s.conic[0] * dx * dx + s.conic[2] * dy * dy) - s.conic[1] * dx * dy;
            // (Past 3σ it counts for nothing, whichever tile it's in.)
            if !(-4.5..=0.0).contains(&power) {
                continue;
            }
            let alpha = s.opacity * power.exp();
            if alpha < ALPHA_MIN {
                continue;
            }
            out.push(Fragment { depth: s.depth as f64, color: [s.color[0] * alpha, s.color[1] * alpha, s.color[2] * alpha, alpha], splat: i });
            through *= 1.0 - alpha;
            if through < T_MIN {
                break;
            }
        }
    }

    /// The sets' nodes, in order.
    pub(super) fn nodes(&self) -> &[String] {
        &self.view.nodes
    }

    /// Which set splat `i` (as in a [`Fragment`]) is from.
    pub(super) fn set_of(&self, i: u32) -> usize {
        self.view.splats[i as usize].set as usize
    }

    /// Whether set `k` is a holdout.
    pub(super) fn holdout(&self, k: usize) -> bool {
        self.view.holdout[k]
    }

    /// Each splat's normal (for the N pass): its shortest axis in the scene
    /// (through the placement's inverse transpose, as normals go), turned
    /// toward the camera at `cam`.
    pub(super) fn add_normals(&mut self, sets: &[Arc<Splats>], cam: &Camera) {
        let normal_of: Vec<nalgebra::Matrix3<f64>> = sets.iter().map(|s| linear(&s.world).try_inverse().unwrap_or_else(nalgebra::Matrix3::identity).transpose()).collect();
        self.normals = self
            .view
            .splats
            .par_iter()
            .map(|p| {
                let (s, i) = (&sets[p.set as usize], p.src as usize);
                let sc = s.scales[i];
                let k = (0..3).min_by(|&a, &b| sc[a].total_cmp(&sc[b])).unwrap_or(2);
                let n = normal_of[p.set as usize] * rotation(s.rotations[i]).column(k);
                let at = geom::transform_point(&s.world, &Vec3::from(s.positions[i].map(f64::from)));
                let n = n.try_normalize(1e-300).unwrap_or_else(Vec3::z);
                let n = if n.dot(&cam.to_eye(&at)) < 0.0 { -n } else { n };
                [n.x as f32, n.y as f32, n.z as f32]
            })
            .collect();
    }

    /// Each splat's motion on the frame (for the motion pass): where its
    /// center lands a frame later (its set placed as in `next`'s sets, if
    /// the same set is there in the same place in the list, else as now;
    /// seen through `next`'s camera) less where it is now; 0 if it goes
    /// behind the camera.
    pub(super) fn add_motion(&mut self, sets: &[Arc<Splats>], next: Option<(&[Arc<Splats>], &Camera)>, cam: &Camera, format: Size) {
        let (later, next_cam) = next.unwrap_or((sets, cam));
        let worlds: Vec<Mat4> = sets.iter().enumerate().map(|(k, s)| later.get(k).filter(|l| l.node == s.node && l.len() == s.len()).map_or(s.world, |l| l.world)).collect();
        let view = next_cam.view();
        self.motion = self
            .view
            .splats
            .par_iter()
            .map(|p| {
                let s = &sets[p.set as usize];
                let at = geom::transform_point(&(view * worlds[p.set as usize]), &Vec3::from(s.positions[p.src as usize].map(f64::from)));
                next_cam.project_camera(&at, format).map_or([0.0; 2], |(q, _)| [q[0] as f32 - p.center[0], q[1] as f32 - p.center[1]])
            })
            .collect();
    }

    /// Splat `i`'s normal and motion, if worked out.
    pub(super) fn normal(&self, i: u32) -> Option<[f32; 3]> {
        self.normals.get(i as usize).copied()
    }

    pub(super) fn motion(&self, i: u32) -> Option<[f32; 2]> {
        self.motion.get(i as usize).copied()
    }

    /// How many splats show (after culling).
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.view.splats.len()
    }
}

#[cfg(test)]
mod tests {
    use super::super::render;
    use super::*;
    use crate::image::Image;
    use crate::scene::{Card, Scene};
    use crate::splat::dc_of;

    const FMT: Size = Size { width: 128, height: 64 };

    /// A 90° camera at the origin, looking down −Z.
    fn camera() -> Camera {
        Camera { focal: 18.0, ..Camera::default() }
    }

    /// One splat per (center, 1σ size, opacity, color), round, degree 0.
    fn splats(each: &[([f32; 3], f32, f32, [f32; 3])]) -> Arc<Splats> {
        Arc::new(Splats {
            positions: Arc::new(each.iter().map(|s| s.0).collect()),
            scales: Arc::new(each.iter().map(|s| [s.1; 3]).collect()),
            rotations: Arc::new(vec![[1.0, 0.0, 0.0, 0.0]; each.len()]),
            opacity: Arc::new(each.iter().map(|s| s.2).collect()),
            sh: Arc::new(each.iter().flat_map(|s| s.3.map(dc_of)).collect()),
            sh_degree: 0,
            world: Mat4::identity(),
            holdout: false,
            node: "splat1".into(),
        })
    }

    fn scene(s: Arc<Splats>) -> Scene {
        Scene { splats: vec![s], ..Scene::default() }
    }

    #[test]
    fn a_round_splat_is_as_wide_as_its_projection() {
        // 1σ = 0.5 at 10 away: f·σ/d pixels, widened by the third of a pixel.
        let out = render(&scene(splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [1.0; 3])])), &camera(), FMT, 1, None);
        let f = camera().focal_pixels(FMT);
        let sigma = ((f * 0.5 / 10.0).powi(2) + DILATE).sqrt();
        // Along the middle row, out from the center (64, 32): alpha falls
        // to e^-½ of the peak at 1σ.
        let alpha = |x: usize| out.at(x as i32, 32)[3] as f64;
        // (Pixel (64, 32)'s middle is half a pixel off the center each way.)
        let peak = 0.99 * (-0.5 * 0.5 / sigma.powi(2)).exp();
        assert!((alpha(64) - peak).abs() < 1e-3, "{} {peak}", alpha(64));
        let at_sigma = (64..128).map(|x| (x, alpha(x))).find(|(_, a)| *a < 0.99 * (-0.5f64).exp()).unwrap();
        let measured = at_sigma.0 as f64 + 0.5 - 64.0;
        assert!((measured - sigma).abs() < 1.0, "1σ {measured} px, expected {sigma:.2}");
        // Its color, and depth where it's half opaque.
        assert!((out.at(64, 32)[0] - out.at(64, 32)[3]).abs() < 1e-4, "white, premultiplied");
        assert!((out.plane_at("depth", 64, 32) - 10.0).abs() < 1e-4);
        assert_eq!(out.plane_at("depth", 64 + 3 * sigma as i32, 32), 0.0, "too faint for depth");
    }

    #[test]
    fn splats_blend_front_to_back_whatever_their_order() {
        let (red, blue) = (([0.0, 0.0, -5.0], 1.0, 0.8, [1.0, 0.0, 0.0]), ([0.0, 0.0, -8.0], 1.6, 0.8, [0.0, 0.0, 1.0]));
        let a = render(&scene(splats(&[red, blue])), &camera(), FMT, 1, None);
        let b = render(&scene(splats(&[blue, red])), &camera(), FMT, 1, None);
        assert_eq!(a.pixels, b.pixels);
        // At the middle: red over blue (each nearly its opacity there).
        let p = a.at(64, 32);
        let (ar, ab) = (p[0], p[2]);
        assert!(ar > 0.75 && (ab - 0.8 * (1.0 - ar)).abs() < 0.02, "{p:?}");
        assert!((p[3] - (ar + ab)).abs() < 1e-4);
    }

    #[test]
    fn splats_out_of_view_are_dropped() {
        let cam = camera();
        let fmt = FMT;
        let count = |c: [f32; 3], size: f32| prepare(&[splats(&[(c, size, 0.9, [1.0; 3])])], &cam, fmt).map_or(0, |p| p.len());
        assert_eq!(count([0.0, 0.0, -10.0], 0.5), 1);
        assert_eq!(count([0.0, 0.0, 10.0], 0.5), 0, "behind the camera");
        assert_eq!(count([0.0, 0.0, -0.05], 0.5), 0, "nearer than near");
        // The frame is 20 across at 10 away: its edge at x = 10, 5 above.
        assert_eq!(count([30.0, 0.0, -10.0], 0.5), 0, "off to the side");
        assert_eq!(count([11.0, 0.0, -10.0], 0.5), 1, "its 3σ reaches in");
        assert_eq!(count([0.0, 7.0, -10.0], 0.5), 0, "above, 3σ short of it");
        // Faint ones too.
        assert!(prepare(&[splats(&[([0.0, 0.0, -10.0], 0.5, 0.001, [1.0; 3])])], &cam, fmt).is_none());
    }

    #[test]
    fn degree_one_color_turns_with_the_view() {
        // A gray splat greener seen looking along −X, redder along +X.
        let mut sh = vec![dc_of(0.5); 3];
        sh.extend([0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        sh.extend([-0.4, 0.4, 0.0]);
        let s = Arc::new(Splats { sh: Arc::new(sh), sh_degree: 1, ..(*splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [0.5; 3])])).clone() });
        let seen_from = |eye: [f64; 3], yaw: f64| {
            let cam = Camera { world: geom::trs(eye, [0.0, yaw, 0.0], [1.0; 3]), ..camera() };
            render(&scene(s.clone()), &cam, FMT, 1, None).at(64, 32)
        };
        // From +X looking toward −X (yaw 90°), and from −X (yaw −90°).
        let (from_right, from_left) = (seen_from([10.0, 0.0, -10.0], 90.0), seen_from([-10.0, 0.0, -10.0], -90.0));
        let expect = |dir: [f32; 3]| s.color(0, dir);
        let unpremul = |p: Rgba| [p[0] / p[3], p[1] / p[3], p[2] / p[3]];
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|c| (a[c] - b[c]).abs() < 1e-3);
        assert!(close(unpremul(from_right), expect([-1.0, 0.0, 0.0])), "{from_right:?}");
        assert!(close(unpremul(from_left), expect([1.0, 0.0, 0.0])), "{from_left:?}");
        assert!(from_right[1] > from_right[0] && from_left[0] > from_left[1], "{from_right:?} {from_left:?}");
        // Degree 0 doesn't change.
        let gray = splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [0.5; 3])]);
        let a = render(&scene(gray.clone()), &Camera { world: geom::trs([10.0, 0.0, -10.0], [0.0, 90.0, 0.0], [1.0; 3]), ..camera() }, FMT, 1, None);
        assert!(close(unpremul(a.at(64, 32)), [0.5; 3]));
    }

    #[test]
    fn splats_mix_with_cards_by_depth() {
        let red = Image::filled(8, 8, [1.0, 0.0, 0.0, 1.0]);
        let card = |z: f64, opacity: f32| Card { opacity, ..Card::new(Arc::new(red.clone()), geom::trs([0.0, 0.0, z], [0.0; 3], [1.0; 3]), [40.0, 20.0]) };
        let blue = |z: f32| splats(&[([0.0, 0.0, z], 1.0, 0.99, [0.0, 0.0, 1.0])]);
        let shot = |cards: Vec<Card>, z: f32, samples: usize| render(&Scene { cards, splats: vec![blue(z)], ..Scene::default() }, &camera(), FMT, samples, None);
        // In front of an opaque card: blue over red; behind: red alone.
        let front = shot(vec![card(-10.0, 1.0)], -5.0, 1);
        let p = front.at(64, 32);
        assert!(p[2] > 0.9 && p[0] < 0.1 && (p[3] - 1.0).abs() < 1e-5, "{p:?}");
        assert!((front.plane_at("depth", 64, 32) - 5.0).abs() < 1e-4);
        let behind = shot(vec![card(-10.0, 1.0)], -15.0, 1);
        assert_eq!(behind.at(64, 32), [1.0, 0.0, 0.0, 1.0]);
        assert!((behind.plane_at("depth", 64, 32) - 10.0).abs() < 1e-4);
        // Through a half see-through card: half of the splat behind it.
        let through = shot(vec![card(-10.0, 0.5)], -15.0, 1).at(64, 32);
        let alone = render(&scene(blue(-15.0)), &camera(), FMT, 1, None).at(64, 32);
        assert!((through[0] - 0.5).abs() < 1e-4 && (through[2] - 0.5 * alone[2]).abs() < 1e-4, "{through:?} {alone:?}");
        // Anti-aliased, the same inside.
        let aa = shot(vec![card(-10.0, 1.0)], -5.0, 3).at(64, 32);
        assert!((0..4).all(|c| (aa[c] - p[c]).abs() < 1e-4), "{aa:?} {p:?}");
    }

    #[test]
    fn the_3d_view_preview_draws_splats_still_or_blurred() {
        use super::super::{render_preview, render_preview_blurred, Blur, Preview};
        let sc = scene(splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [0.0, 1.0, 0.0])]));
        let sharp = render_preview(&sc, &camera(), FMT, Preview::Solid);
        assert!(sharp.at(64, 32)[1] > 0.9 && sharp.at(0, 0)[3] == 0.0, "{:?}", sharp.at(64, 32));
        // Blurred with nothing moving: the same picture.
        let blur = Blur { times: Blur::times(-0.25, 0.5, 4), frames: vec![(0, &sc, camera())], jitter: true };
        let still = render_preview_blurred(&sc, &camera(), FMT, Preview::Solid, Some(&blur));
        let worst = still.pixels.iter().zip(&sharp.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(worst < 1e-5, "{worst}");
        // A camera moving across it smears it.
        let moved = Camera { world: geom::trs([1.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..camera() };
        let blur = Blur { times: Blur::times(0.0, 1.0, 4), frames: vec![(0, &sc, camera()), (1, &sc, moved)], jitter: false };
        let smear = render_preview_blurred(&sc, &camera(), FMT, Preview::Solid, Some(&blur));
        assert!(smear.at(64, 32)[1] < sharp.at(64, 32)[1] - 0.1, "{:?} {:?}", smear.at(64, 32), sharp.at(64, 32));
    }

    /// Splats over a card, with every pass: mattes add up to alpha, the
    /// light passes to the picture (splats are emission), and depth, P, N,
    /// id and motion come from the splats where they're half opaque.
    #[test]
    fn splat_passes_add_up_over_a_card() {
        use super::super::{render_motion, Passes};
        let red = Image::filled(8, 8, [1.0, 0.0, 0.0, 1.0]);
        let card = Card { node: "card".into(), ..Card::new(Arc::new(red), geom::trs([0.0, 0.0, -10.0], [0.0; 3], [1.0; 3]), [40.0, 20.0]) };
        // A flat splat (thin along z: its normal is ±z) 6 away, and a
        // faint one beside it.
        let mut s = (*splats(&[([0.0, 0.0, -6.0], 1.0, 0.9, [0.0, 0.0, 1.0]), ([2.4, 0.0, -6.0], 0.6, 0.3, [0.0, 1.0, 0.0])])).clone();
        s.scales = Arc::new(vec![[1.0, 1.0, 0.01], [0.6, 0.6, 0.01]]);
        s.node = "cap".into();
        let sc = Scene { cards: vec![card], splats: vec![Arc::new(s.clone())], ..Scene::default() };
        // A frame later the capture has moved 1 to the right.
        let moved = Splats { world: geom::trs([1.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..s };
        let next = Scene { splats: vec![Arc::new(moved)], ..sc.clone() };
        let names = ["depth", "P", "N", "motion", "id", "mattes", "diffuse", "specular", "emission", "ambient"];
        let out = render_motion(&sc, &camera(), FMT, 2, None, Passes::from_names(names), Some((&next, &camera())));
        let plane = |n: &str, x: i32, y: i32| out.plane_at(n, x, y);
        let mut worst = (0.0f32, 0.0f32);
        for y in 0..FMT.height as i32 {
            for x in 0..FMT.width as i32 {
                let p = out.at(x, y);
                let mattes = plane("matte.cap", x, y) + plane("matte.card", x, y);
                worst.0 = worst.0.max((mattes - p[3]).abs());
                for (c, ch) in ["R", "G", "B"].iter().enumerate() {
                    let parts: f32 = ["diffuse", "specular", "emission", "ambient"].iter().map(|n| plane(&format!("{n}.{ch}"), x, y)).sum();
                    worst.1 = worst.1.max((parts - p[c]).abs());
                }
            }
        }
        assert!(worst.0 < 1e-4 && worst.1 < 1e-4, "mattes off by {}, light passes by {}", worst.0, worst.1);
        // At the middle: the splat's share, over the card's.
        let (m, c) = (plane("matte.cap", 64, 32), plane("matte.card", 64, 32));
        assert!(m > 0.85 && (c - (1.0 - m)).abs() < 1e-4, "{m} {c}");
        assert!((plane("depth", 64, 32) - 6.0).abs() < 1e-4);
        let p = camera().unproject([64.5, 32.5], 6.0, FMT);
        assert!((plane("P.x", 64, 32) as f64 - p.x).abs() < 1e-4 && (plane("P.z", 64, 32) + 6.0).abs() < 1e-4);
        assert!((plane("N.z", 64, 32) - 1.0).abs() < 1e-4, "facing the camera");
        let f = camera().focal_pixels(FMT) as f32;
        assert!((plane("motion.u", 64, 32) - f / 6.0).abs() < 1e-3 && plane("motion.v", 64, 32).abs() < 1e-3, "{}", plane("motion.u", 64, 32));
        // id: the objects are the surfaces' nodes, then the splat sets'.
        assert_eq!((plane("id", 64, 32), plane("id", 4, 4)), (2.0, 1.0));
        // Where the faint splat is, the card still gives depth and id.
        let fx = 64 + (2.4 * f as f64 / 6.0) as i32;
        assert!(plane("matte.cap", fx, 32) > 0.05 && (plane("depth", fx, 32) - 10.0).abs() < 1e-4 && plane("id", fx, 32) == 1.0);
    }

    #[test]
    fn holdout_splats_cut_what_is_behind() {
        let red = Image::filled(8, 8, [1.0, 0.0, 0.0, 1.0]);
        let card = Card::new(Arc::new(red), geom::trs([0.0, 0.0, -10.0], [0.0; 3], [1.0; 3]), [40.0, 20.0]);
        let s = Splats { holdout: true, ..(*splats(&[([0.0, 0.0, -6.0], 1.0, 0.99, [0.0, 0.0, 1.0])])).clone() };
        let out = render(&Scene { cards: vec![card], splats: vec![Arc::new(s)], ..Scene::default() }, &camera(), FMT, 1, None);
        let p = out.at(64, 32);
        assert!(p[3] < 0.05 && p[2] == 0.0, "the card cut, nothing blue: {p:?}");
        assert_eq!(out.at(4, 4), [1.0, 0.0, 0.0, 1.0], "the card elsewhere");
    }

    /// A Read Splat moving across the shutter smears, as other objects do.
    #[test]
    fn moving_splats_blur() {
        use super::super::{render_blurred, Blur, Passes};
        let s = splats(&[([0.0, 0.0, -10.0], 0.3, 0.99, [1.0; 3])]);
        let now = scene(s.clone());
        let later = scene(Arc::new(Splats { world: geom::trs([2.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..(*s).clone() }));
        let blur = Blur { times: Blur::times(0.0, 1.0, 16), frames: vec![(0, &now, camera()), (1, &later, camera())], jitter: true };
        let sharp = render(&now, &camera(), FMT, 1, None);
        let soft = render_blurred(&now, &camera(), FMT, 1, None, Passes::DEPTH, None, Some(&blur));
        let row = |img: &Image| (0..128).filter(|&x| img.at(x, 32)[3] > 0.05).count();
        assert!(row(&soft) > row(&sharp) + 6, "{} vs {}", row(&soft), row(&sharp));
        let total = |img: &Image| img.pixels.iter().map(|p| p[3]).sum::<f32>();
        assert!((total(&soft) - total(&sharp)).abs() < 0.05 * total(&sharp), "{} vs {}", total(&soft), total(&sharp));
    }

    /// The same view again is the same work kept, so renders that change
    /// something else (here the passes) give the same splats; a moved
    /// capture or camera is worked out anew.
    #[test]
    fn views_are_kept_for_re_renders() {
        use super::super::{render_passes, Passes};
        let s = splats(&[([0.0, 0.0, -10.0], 0.5, 0.9, [1.0, 0.5, 0.0]), ([1.0, 0.0, -12.0], 0.8, 0.6, [0.0, 0.5, 1.0])]);
        let cam = camera();
        let a = prepare(std::slice::from_ref(&s), &cam, FMT).unwrap();
        let b = prepare(std::slice::from_ref(&s), &cam, FMT).unwrap();
        assert!(Arc::ptr_eq(&a.view, &b.view), "kept");
        let moved = Arc::new(Splats { world: geom::trs([0.1, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..(*s).clone() });
        assert!(!Arc::ptr_eq(&a.view, &prepare(&[moved], &cam, FMT).unwrap().view), "moved: anew");
        let turned = Camera { world: geom::trs([0.0; 3], [0.0, 1.0, 0.0], [1.0; 3]), ..cam.clone() };
        assert!(!Arc::ptr_eq(&a.view, &prepare(std::slice::from_ref(&s), &turned, FMT).unwrap().view), "another camera: anew");
        assert!(!Arc::ptr_eq(&a.view, &prepare_upto(std::slice::from_ref(&s), &cam, FMT, 1).unwrap().view), "another SH degree: anew");
        // An equal copy (not the same arrays) isn't taken for the same.
        let copy = Arc::new(Splats { positions: Arc::new((*s.positions).clone()), ..(*s).clone() });
        assert!(!Arc::ptr_eq(&a.view, &prepare(std::slice::from_ref(&copy), &cam, FMT).unwrap().view));
        // Kept or not, the picture is the same.
        let sc = |s: Arc<Splats>| scene(s);
        let first = render_passes(&sc(copy.clone()), &cam, FMT, 2, None, Passes::DEPTH);
        let again = render_passes(&sc(copy), &cam, FMT, 2, None, Passes::from_names(["depth", "id"]));
        assert_eq!(first.pixels, again.pixels);
        assert_eq!(first.plane("depth").unwrap().data, again.plane("depth").unwrap().data);
    }

    #[test]
    fn the_3d_view_takes_color_up_to_degree_one() {
        use super::super::{render_preview, Preview};
        // Degree 2 color seen head on: the preview leaves it out.
        let mut sh = vec![dc_of(0.5); 3];
        sh.extend(vec![0.0; 3 * 3]);
        sh.extend(vec![0.0; 3 * 2]);
        sh.extend([0.4, 0.4, 0.4]);
        sh.extend(vec![0.0; 3 * 2]);
        let s = Arc::new(Splats { sh: Arc::new(sh), sh_degree: 2, ..(*splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [0.5; 3])])).clone() });
        let full = render(&scene(s.clone()), &camera(), FMT, 1, None).at(64, 32);
        let quick = render_preview(&scene(s.clone()), &camera(), FMT, Preview::Solid).at(64, 32);
        let gray = render(&scene(splats(&[([0.0, 0.0, -10.0], 0.5, 0.99, [0.5; 3])])), &camera(), FMT, 1, None).at(64, 32);
        assert!((full[0] - gray[0]).abs() > 0.05, "degree 2 shows in the render: {full:?} {gray:?}");
        assert!((0..4).all(|c| (quick[c] - gray[c]).abs() < 1e-5), "not in the preview: {quick:?} {gray:?}");
    }

    #[test]
    fn holdouts_cut_splats() {
        let mut cube = crate::scene::primitive_model("cube", &serde_json::json!({ "size": [4, 4, 4] }), None).unwrap();
        cube.world = geom::trs([0.0, 0.0, -8.0], [0.0; 3], [1.0; 3]);
        cube.holdout = true;
        let s = splats(&[([0.0, 0.0, -15.0], 1.0, 0.99, [0.0, 0.0, 1.0])]);
        let out = render(&Scene { meshes: vec![cube], splats: vec![s.clone()], ..Scene::default() }, &camera(), FMT, 1, None);
        assert_eq!(out.at(64, 32), [0.0; 4], "held out");
        let alone = render(&scene(s), &camera(), FMT, 1, None);
        assert!(alone.at(64, 32)[3] > 0.9);
    }

    /// Through an orthographic camera a round splat is as wide at any
    /// distance: its size times the pixels per unit.
    #[test]
    fn an_orthographic_splat_keeps_its_size() {
        let cam = Camera { ortho: 16.0, ..camera() };
        let k = cam.pixels_per_unit(1.0, FMT);
        let sigma = ((k * 0.5).powi(2) + DILATE).sqrt();
        for z in [-4.0, -400.0] {
            let out = render(&scene(splats(&[([0.0, 0.0, z], 0.5, 0.99, [1.0; 3])])), &cam, FMT, 1, None);
            let at_sigma = (64..128).find(|&x| (out.at(x, 32)[3] as f64) < 0.99 * (-0.5f64).exp()).unwrap();
            let measured = at_sigma as f64 + 0.5 - 64.0;
            assert!((measured - sigma).abs() < 1.0, "at {z}: 1σ {measured} px, expected {sigma:.2}");
            assert!((out.plane_at("depth", 64, 32) as f64 + z as f64).abs() < 1e-3);
        }
    }
}
