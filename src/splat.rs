//! Gaussian splats (3D Gaussian Splatting captures): what a Read Splat
//! node puts in a scene ([`crate::scene::Scene::splats`]) and the renderer
//! draws. Each splat is a 3D Gaussian: a center, a size along each of its
//! own axes, a turn, an opacity, and a color that can change with the
//! direction it's seen from (spherical harmonics, degree 0 to 3).
//!
//! Kept as arrays of f32 (captures run to millions of splats), each array
//! shared ([`Arc`]): placing a capture (its [`world`](Splats::world)) or
//! tagging it copies none of them.

use crate::geom::{self, Mat4, Vec3};
use std::sync::Arc;

/// Gaussian splats read from a file, placed by `world`.
#[derive(Clone, Debug, Default)]
pub struct Splats {
    /// Centers, in the capture's own space.
    pub positions: Arc<Vec<[f32; 3]>>,
    /// Sizes (1σ) along each splat's own x, y and z: linear (a file's log
    /// scales, exp'd).
    pub scales: Arc<Vec<[f32; 3]>>,
    /// Turns, unit quaternions as [w, x, y, z] (a file's order).
    pub rotations: Arc<Vec<[f32; 4]>>,
    /// 0 to 1 (a file's logits, sigmoid'd).
    pub opacity: Arc<Vec<f32>>,
    /// Color as spherical harmonics, [`sh_stride`](Splats::sh_stride) per
    /// splat: coefficient by coefficient, each as R, G, B (so the first
    /// three are the DC term). See [`Splats::color`].
    pub sh: Arc<Vec<f32>>,
    /// 0 to 3.
    pub sh_degree: u8,
    /// Where the capture's space sits in the scene.
    pub world: Mat4,
    /// The node it came from ("" until tagged).
    pub node: String,
}

/// The SH basis' constants (as in the reference 3DGS rasterizer).
const SH_C0: f32 = 0.282_094_8;
const SH_C1: f32 = 0.488_602_5;
const SH_C2: [f32; 5] = [1.092_548_4, -1.092_548_4, 0.315_391_57, -1.092_548_4, 0.546_274_2];
const SH_C3: [f32; 7] = [-0.590_043_6, 2.890_611_4, -0.457_045_8, 0.373_176_33, -0.457_045_8, 1.445_305_7, -0.590_043_6];

/// SH coefficients per color channel at `degree`: (degree + 1)².
pub fn sh_coeffs(degree: u8) -> usize {
    (degree as usize + 1).pow(2)
}

impl Splats {
    /// How many splats.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Floats of [`sh`](Splats::sh) per splat: 3 × (degree + 1)².
    pub fn sh_stride(&self) -> usize {
        3 * sh_coeffs(self.sh_degree)
    }

    /// Splat `i`'s SH coefficients.
    pub fn sh_of(&self, i: usize) -> &[f32] {
        let n = self.sh_stride();
        &self.sh[i * n..(i + 1) * n]
    }

    /// Roughly the memory its arrays hold.
    pub fn bytes(&self) -> usize {
        self.len() * (12 + 12 + 16 + 4) + self.sh.len() * 4 + 256
    }

    /// A box around the splats' centers in scene space (the capture's own
    /// box, placed), or None when there are none.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let first = self.positions.first()?;
        let (mut lo, mut hi) = (*first, *first);
        for p in self.positions.iter() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let corner = |i: usize| Vec3::new([lo[0], hi[0]][i & 1] as f64, [lo[1], hi[1]][(i >> 1) & 1] as f64, [lo[2], hi[2]][(i >> 2) & 1] as f64);
        let placed: Vec<Vec3> = (0..8).map(|i| geom::transform_point(&self.world, &corner(i))).collect();
        let fold = |f: fn(f64, f64) -> f64, init: f64| Vec3::new(placed.iter().map(|p| p.x).fold(init, f), placed.iter().map(|p| p.y).fold(init, f), placed.iter().map(|p| p.z).fold(init, f));
        Some((fold(f64::min, f64::INFINITY), fold(f64::max, f64::NEG_INFINITY)))
    }

    /// Splat `i`'s color (linear RGB, as the capture stored it) seen along
    /// `dir`: the unit direction from the viewer to the splat, in the
    /// capture's own space. The SH sum plus 0.5, at least 0 (as the 3DGS
    /// reference does); degree 0 doesn't depend on `dir`.
    pub fn color(&self, i: usize, dir: [f32; 3]) -> [f32; 3] {
        sh_color(self.sh_of(i), self.sh_degree, dir)
    }
}

/// The color the SH coefficients `sh` (degree `degree`, laid out as in
/// [`Splats::sh`]) give along unit direction `dir`.
pub fn sh_color(sh: &[f32], degree: u8, dir: [f32; 3]) -> [f32; 3] {
    let c = |k: usize, ch: usize| sh[k * 3 + ch];
    let mut out = [0.0f32; 3];
    let [x, y, z] = dir;
    for (ch, o) in out.iter_mut().enumerate() {
        let mut v = SH_C0 * c(0, ch);
        if degree >= 1 {
            v += SH_C1 * (-y * c(1, ch) + z * c(2, ch) - x * c(3, ch));
        }
        if degree >= 2 {
            let (xx, yy, zz, xy, yz, xz) = (x * x, y * y, z * z, x * y, y * z, x * z);
            v += SH_C2[0] * xy * c(4, ch) + SH_C2[1] * yz * c(5, ch) + SH_C2[2] * (2.0 * zz - xx - yy) * c(6, ch) + SH_C2[3] * xz * c(7, ch) + SH_C2[4] * (xx - yy) * c(8, ch);
            if degree >= 3 {
                v += SH_C3[0] * y * (3.0 * xx - yy) * c(9, ch)
                    + SH_C3[1] * xy * z * c(10, ch)
                    + SH_C3[2] * y * (4.0 * zz - xx - yy) * c(11, ch)
                    + SH_C3[3] * z * (2.0 * zz - 3.0 * xx - 3.0 * yy) * c(12, ch)
                    + SH_C3[4] * x * (4.0 * zz - xx - yy) * c(13, ch)
                    + SH_C3[5] * z * (xx - yy) * c(14, ch)
                    + SH_C3[6] * x * (xx - 3.0 * yy) * c(15, ch);
            }
        }
        *o = (v + 0.5).max(0.0);
    }
    out
}

/// The DC coefficient that gives color `c` (0..1) at degree 0: what
/// [`sh_color`] undoes.
pub fn dc_of(c: f32) -> f32 {
    (c - 0.5) / SH_C0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three splats, degree 1, the middle one tinted by direction.
    pub(crate) fn three() -> Splats {
        let mut sh = vec![0.0f32; 3 * 3 * 4];
        for (i, c) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].iter().enumerate() {
            for ch in 0..3 {
                sh[i * 12 + ch] = dc_of(c[ch]);
            }
        }
        // Splat 1: redder seen along +x (its coefficient 3 is −x).
        sh[12 + 3 * 3] = -0.5;
        Splats {
            positions: Arc::new(vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-1.0, -2.0, -3.0]]),
            scales: Arc::new(vec![[0.1; 3]; 3]),
            rotations: Arc::new(vec![[1.0, 0.0, 0.0, 0.0]; 3]),
            opacity: Arc::new(vec![1.0, 0.5, 0.25]),
            sh: Arc::new(sh),
            sh_degree: 1,
            world: Mat4::identity(),
            node: String::new(),
        }
    }

    #[test]
    fn colors_bounds_and_layout() {
        let s = three();
        assert_eq!((s.len(), s.sh_stride(), s.sh_of(1).len()), (3, 12, 12));
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert!(close(s.color(0, [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]) && close(s.color(2, [1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]));
        // Degree 1 changes with the direction; degree 0 doesn't.
        let (along, against) = (s.color(1, [1.0, 0.0, 0.0]), s.color(1, [-1.0, 0.0, 0.0]));
        assert!(along[0] > 0.2 && against[0] == 0.0, "{along:?} {against:?}");
        assert_eq!(sh_color(s.sh_of(1), 0, [1.0, 0.0, 0.0]), sh_color(s.sh_of(1), 0, [-1.0, 0.0, 0.0]));
        assert_eq!(s.bounds(), Some((Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0))));
        let placed = Splats { world: geom::trs([10.0, 0.0, 0.0], [0.0; 3], [2.0; 3]), ..s.clone() };
        assert_eq!(placed.bounds(), Some((Vec3::new(8.0, -4.0, -6.0), Vec3::new(12.0, 4.0, 6.0))));
        assert!(Arc::ptr_eq(&placed.positions, &s.positions), "placing shares the data");
        assert_eq!(Splats::default().bounds(), None);
    }

    /// In a scene: gathered, tagged with their node, moved between frames
    /// (for motion blur) and counted, the data shared all the while.
    #[test]
    fn splats_travel_in_scenes() {
        use crate::scene::Scene;
        let s = Arc::new(three());
        let at = |x: f64| Scene { splats: vec![Arc::new(Splats { world: geom::trs([x, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..(*s).clone() })], ..Scene::default() };
        let mut a = at(0.0);
        a.tag("capture");
        let mut gathered = Scene::default();
        gathered.extend(&a);
        gathered.tag("scene1");
        assert_eq!(gathered.splats[0].node, "capture", "kept its own node");
        assert!(Arc::ptr_eq(&gathered.splats[0].positions, &s.positions));
        assert!(gathered.describe().contains("1 splat capture (3 splats)"));
        let mut b = at(4.0);
        b.tag("capture");
        let half = a.at_time(&[(0, &a), (1, &b)], 0.5);
        assert_eq!(geom::transform_point(&half.splats[0].world, &Vec3::zeros()), Vec3::new(2.0, 0.0, 0.0));
        assert!(Arc::ptr_eq(&half.splats[0].sh, &s.sh));
        let img = crate::image::Image { scene: Some(Arc::new(a)), ..crate::image::Image::blank(crate::image::Size { width: 1, height: 1 }, crate::image::IRect::default()) };
        assert!(img.bytes() >= s.bytes());
    }
}
