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
}

/// One splat's share at a pixel: its depth and premultiplied color.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fragment {
    pub depth: f64,
    pub color: Rgba,
}

/// A view's splats, projected, binned into tiles and sorted by depth.
pub(super) struct Prepared {
    splats: Vec<Projected>,
    tiles_x: usize,
    /// Per tile: its range in `order`.
    ranges: Vec<(u32, u32)>,
    /// Splat indices, by tile then nearest first.
    order: Vec<u32>,
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
/// its space to the camera's; `eye_own`: the camera in its space), or None
/// if it doesn't show.
fn project(s: &Splats, i: usize, cam: &Camera, format: Size, to_cam: &Mat4, eye_own: &Vec3) -> Option<Projected> {
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
    let center = [format.width as f64 / 2.0 + f * p.x / depth, format.height as f64 / 2.0 - f * p.y / depth];
    // Its covariance in camera space, then on the frame.
    let r = rotation(s.rotations[i]);
    let sc = s.scales[i].map(f64::from);
    let m = linear(to_cam) * r * nalgebra::Matrix3::from_diagonal(&nalgebra::Vector3::new(sc[0], sc[1], sc[2]));
    let cov = m * m.transpose();
    let j = nalgebra::Matrix2x3::new(f / depth, 0.0, f * p.x / (depth * depth), 0.0, -f / depth, -f * p.y / (depth * depth));
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
    let dir = (pos - eye_own).try_normalize(1e-300).map_or([0.0, 0.0, 1.0], |d| [d.x as f32, d.y as f32, d.z as f32]);
    let color = s.color(i, dir);
    Some(Projected { center: center.map(|v| v as f32), conic: [(c / det) as f32, (-b / det) as f32, (a / det) as f32], depth: depth as f32, color, opacity: opacity.min(ALPHA_MAX) })
}

/// `sets` seen through `cam` in a frame of `format`, ready to blend; None
/// when none shows.
pub(super) fn prepare(sets: &[Arc<Splats>], cam: &Camera, format: Size) -> Option<Prepared> {
    let view = cam.view();
    let eye = geom::transform_point(&cam.world, &Vec3::zeros());
    let splats: Vec<Projected> = sets
        .iter()
        .flat_map(|s| {
            let to_cam = view * s.world;
            let own = s.world.try_inverse().unwrap_or_else(Mat4::identity);
            let eye_own = geom::transform_point(&own, &eye);
            let n = s.positions.len().min(s.scales.len()).min(s.rotations.len()).min(s.opacity.len());
            let n = n.min(s.sh.len() / s.sh_stride().max(1));
            (0..n).into_par_iter().filter_map(|i| project(s, i, cam, format, &to_cam, &eye_own)).collect::<Vec<_>>()
        })
        .collect();
    if splats.is_empty() {
        return None;
    }
    let tiles_x = format.width.div_ceil(TILE);
    let tiles_y = format.height.div_ceil(TILE);
    // (tile, depth) keys: depths are positive, so their bits sort as they do.
    let mut keys: Vec<(u64, u32)> = splats
        .par_iter()
        .enumerate()
        .flat_map_iter(|(i, s)| {
            let mid = (s.conic[0] * s.conic[2] - s.conic[1] * s.conic[1]).recip();
            let (a, c) = (s.conic[2] * mid, s.conic[0] * mid);
            let (rx, ry) = (3.0 * a.sqrt(), 3.0 * c.sqrt());
            let tx = |v: f32| ((v / TILE as f32).floor().max(0.0) as usize).min(tiles_x - 1);
            let ty = |v: f32| ((v / TILE as f32).floor().max(0.0) as usize).min(tiles_y - 1);
            let (x0, x1, y0, y1) = (tx(s.center[0] - rx), tx(s.center[0] + rx), ty(s.center[1] - ry), ty(s.center[1] + ry));
            let d = s.depth.to_bits() as u64;
            (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (((y * tiles_x + x) as u64) << 32 | d, i as u32)))
        })
        .collect();
    keys.par_sort_unstable_by_key(|k| k.0);
    let mut ranges = vec![(0u32, 0u32); tiles_x * tiles_y];
    let mut start = 0;
    while start < keys.len() {
        let tile = (keys[start].0 >> 32) as usize;
        let end = start + keys[start..].partition_point(|k| (k.0 >> 32) as usize == tile);
        ranges[tile] = (start as u32, end as u32);
        start = end;
    }
    let order = keys.into_iter().map(|k| k.1).collect();
    Some(Prepared { splats, tiles_x, ranges, order })
}

impl Prepared {
    /// Pixel (`x`, `y`)'s splats into `out` (cleared first), nearest
    /// first, until less than [`T_MIN`] shows through them.
    pub(super) fn fragments(&self, x: usize, y: usize, out: &mut Vec<Fragment>) {
        out.clear();
        let Some(&(a, b)) = self.ranges.get((y / TILE) * self.tiles_x + x / TILE) else { return };
        let q = [x as f32 + 0.5, y as f32 + 0.5];
        let mut through = 1.0f32;
        for &i in &self.order[a as usize..b as usize] {
            let s = &self.splats[i as usize];
            let (dx, dy) = (q[0] - s.center[0], q[1] - s.center[1]);
            let power = -0.5 * (s.conic[0] * dx * dx + s.conic[2] * dy * dy) - s.conic[1] * dx * dy;
            if power > 0.0 {
                continue;
            }
            let alpha = s.opacity * power.exp();
            if alpha < ALPHA_MIN {
                continue;
            }
            out.push(Fragment { depth: s.depth as f64, color: [s.color[0] * alpha, s.color[1] * alpha, s.color[2] * alpha, alpha] });
            through *= 1.0 - alpha;
            if through < T_MIN {
                break;
            }
        }
    }

    /// How many splats show (after culling).
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.splats.len()
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
            node: "splat1".into(),
            holdout: false,
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
}
