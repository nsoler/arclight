//! Triangle meshes for 3D objects: primitives (cube, sphere, cylinder,
//! plane) and materials.
//!
//! A mesh is in its object's own space (the scene places it); Y is up, as
//! in [`crate::geom`]. Attributes are per vertex: a corner with its own
//! normal or texture coordinate (a hard edge, a UV seam) is its own vertex.
//! Triangles wind counter-clockwise seen from outside, so `(b − a) × (c − a)`
//! points out, the way the normals do. Texture coordinates run left to
//! right and bottom to top (v = 0 at the bottom of the image), as OBJ and
//! GL have them.

use crate::geom::{self, Mat4, Vec3};
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::PathBuf;

/// A triangle mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    /// Unit normals, one per position; empty for flat shading.
    pub normals: Vec<Vec3>,
    /// Texture coordinates, one per position, or empty.
    pub uvs: Vec<[f64; 2]>,
    pub triangles: Vec<[u32; 3]>,
}

/// How a material responds to light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shading {
    /// Its color as it is, lights or not (as cards render).
    #[default]
    Unlit,
    /// Diffuse only.
    Lambert,
    /// Diffuse plus a Blinn–Phong highlight.
    Blinn,
}

/// What a surface looks like.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    pub shading: Shading,
    /// Diffuse color, linear.
    pub color: [f32; 3],
    pub opacity: f32,
    /// An image file for the color (an MTL's `map_Kd`), resolved.
    pub texture: Option<PathBuf>,
    /// Highlight color and strength, linear.
    pub specular: [f32; 3],
    /// 0 (a sharp highlight) to 1 (a broad one).
    pub roughness: f32,
    /// Light it gives off, linear.
    pub emission: [f32; 3],
}

impl Default for Material {
    fn default() -> Material {
        Material {
            name: String::new(),
            shading: Shading::Unlit,
            color: [1.0; 3],
            opacity: 1.0,
            texture: None,
            specular: [0.0; 3],
            roughness: 0.5,
            emission: [0.0; 3],
        }
    }
}

impl Mesh {
    /// The smallest and largest corner of the box around it (None when it
    /// has no vertices).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let first = *self.positions.first()?;
        Some(self.positions.iter().fold((first, first), |(lo, hi), p| (lo.inf(p), hi.sup(p))))
    }

    /// Moved by `m`: positions as points, normals by the inverse transpose
    /// (so they stay at right angles to the surface under a non-uniform
    /// scale), and the winding reversed if `m` mirrors.
    pub fn transform(&mut self, m: &Mat4) {
        for p in &mut self.positions {
            *p = geom::transform_point(m, p);
        }
        let linear = m.fixed_view::<3, 3>(0, 0).into_owned();
        let normal = linear.try_inverse().map(|i| i.transpose()).unwrap_or(linear);
        for n in &mut self.normals {
            *n = (normal * *n).try_normalize(1e-300).unwrap_or(*n);
        }
        if linear.determinant() < 0.0 {
            for t in &mut self.triangles {
                t.swap(1, 2);
            }
        }
    }

    /// All of `meshes` as one. Normals and texture coordinates are kept
    /// when every mesh has them.
    pub fn merge(meshes: &[Mesh]) -> Mesh {
        let has = |f: &dyn Fn(&Mesh) -> bool| meshes.iter().all(|m| f(m) || m.positions.is_empty());
        let (normals, uvs) = (has(&|m| m.normals.len() == m.positions.len()), has(&|m| m.uvs.len() == m.positions.len()));
        let mut out = Mesh::default();
        for m in meshes {
            let base = out.positions.len() as u32;
            out.positions.extend_from_slice(&m.positions);
            if normals {
                out.normals.extend_from_slice(&m.normals);
            }
            if uvs {
                out.uvs.extend_from_slice(&m.uvs);
            }
            out.triangles.extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
        }
        out
    }

    /// The (unnormalized) normal of triangle `t`, as long as twice its area.
    pub fn face_normal(&self, t: usize) -> Vec3 {
        let [a, b, c] = self.triangles[t].map(|i| self.positions[i as usize]);
        (b - a).cross(&(c - a))
    }

    /// Smooth normals: at each position, the mean of the faces around it
    /// weighted by their angle there, across seams too (vertices at the
    /// same place share one normal).
    pub fn smooth_normals(&mut self) {
        let key = |p: &Vec3| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let mut sums: HashMap<[u64; 3], Vec3> = HashMap::new();
        for t in 0..self.triangles.len() {
            let Some(n) = self.face_normal(t).try_normalize(1e-300) else { continue };
            let tri = self.triangles[t].map(|i| i as usize);
            for k in 0..3 {
                let p = self.positions[tri[k]];
                let angle = (self.positions[tri[(k + 1) % 3]] - p).angle(&(self.positions[tri[(k + 2) % 3]] - p));
                *sums.entry(key(&p)).or_insert_with(Vec3::zeros) += n * angle;
            }
        }
        self.normals = self.positions.iter().map(|p| sums.get(&key(p)).and_then(|n| n.try_normalize(1e-300)).unwrap_or_else(Vec3::y)).collect();
    }

    /// Faceted: every triangle gets its own three vertices, with its face
    /// normal.
    pub fn flat(&self) -> Mesh {
        let mut out = Mesh::default();
        let uvs = self.uvs.len() == self.positions.len();
        for (t, tri) in self.triangles.iter().enumerate() {
            let n = self.face_normal(t).try_normalize(1e-300).unwrap_or_else(Vec3::y);
            let base = out.positions.len() as u32;
            for &i in tri {
                out.positions.push(self.positions[i as usize]);
                out.normals.push(n);
                if uvs {
                    out.uvs.push(self.uvs[i as usize]);
                }
            }
            out.triangles.push([base, base + 1, base + 2]);
        }
        out
    }

    /// A vertex; returns its index.
    fn vertex(&mut self, p: Vec3, n: Vec3, uv: [f64; 2]) -> u32 {
        self.positions.push(p);
        self.normals.push(n);
        self.uvs.push(uv);
        self.positions.len() as u32 - 1
    }

    /// A flat grid of `nu` × `nv` cells from `origin`, spanning `du` (u = 0
    /// to 1) and `dv` (v = 0 to 1), facing `du × dv`.
    fn grid(&mut self, origin: Vec3, du: Vec3, dv: Vec3, nu: usize, nv: usize) {
        let n = du.cross(&dv).normalize();
        let (nu, nv) = (nu.max(1), nv.max(1));
        let base = self.positions.len() as u32;
        for j in 0..=nv {
            for i in 0..=nu {
                let (u, v) = (i as f64 / nu as f64, j as f64 / nv as f64);
                self.vertex(origin + du * u + dv * v, n, [u, v]);
            }
        }
        self.cells(base, nu, nv, false);
    }

    /// The two triangles of each cell of a grid of vertices from `base`,
    /// `nu + 1` to a row, `nv + 1` rows. With `poles`, the first and last
    /// rows are each one point (a vertex per cell): their cells get one
    /// triangle, to that cell's own pole vertex.
    fn cells(&mut self, base: u32, nu: usize, nv: usize, poles: bool) {
        let at = |i: usize, j: usize| base + (j * (nu + 1) + i) as u32;
        for j in 0..nv {
            for i in 0..nu {
                let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
                if poles && j == 0 {
                    self.triangles.push([a, c, d]);
                } else if poles && j + 1 == nv {
                    self.triangles.push([a, b, d]);
                } else {
                    self.triangles.push([a, b, c]);
                    self.triangles.push([a, c, d]);
                }
            }
        }
    }
}

/// The point at turn `u` (0 to 1) around the Y axis, radius 1: −Z at u = 0
/// (the seam), +Z at u = 0.5, turning so that u grows to the right seen
/// from outside.
fn around(u: f64) -> Vec3 {
    let a = TAU * u;
    Vec3::new(-a.sin(), 0.0, -a.cos())
}

/// A box `size` across (x, y, z), centered on the origin, each face split
/// into `segments` along its axes. Each face has its own vertices (hard
/// edges) and texture coordinates 0 to 1, upright seen from outside (the
/// top and bottom with −Z and +Z up, respectively).
pub fn cube(size: [f64; 3], segments: [usize; 3]) -> Mesh {
    let [hx, hy, hz] = size.map(|v| v / 2.0);
    let [sx, sy, sz] = size;
    let [nx, ny, nz] = segments;
    let (x, y, z) = (Vec3::x(), Vec3::y(), Vec3::z());
    let mut m = Mesh::default();
    m.grid(Vec3::new(-hx, -hy, hz), x * sx, y * sy, nx, ny); // +Z
    m.grid(Vec3::new(hx, -hy, -hz), -x * sx, y * sy, nx, ny); // −Z
    m.grid(Vec3::new(hx, -hy, hz), -z * sz, y * sy, nz, ny); // +X
    m.grid(Vec3::new(-hx, -hy, -hz), z * sz, y * sy, nz, ny); // −X
    m.grid(Vec3::new(-hx, hy, hz), x * sx, -z * sz, nx, nz); // +Y
    m.grid(Vec3::new(-hx, -hy, -hz), x * sx, z * sz, nx, nz); // −Y
    m
}

/// A sphere of `radius` centered on the origin, `segments` = [around (3+),
/// up (2+)]. u runs once around (the seam, at −Z, has a vertex on each
/// side, so no triangle spans it), v from the bottom pole to the top; each
/// pole has a vertex per segment.
pub fn sphere(radius: f64, segments: [usize; 2]) -> Mesh {
    let (s, r) = (segments[0].max(3), segments[1].max(2));
    let mut m = Mesh::default();
    for j in 0..=r {
        let v = j as f64 / r as f64;
        let phi = std::f64::consts::PI * v;
        for i in 0..=s {
            // At a pole every vertex is the same point: give each the
            // middle of its segment.
            let u = if j == 0 || j == r { (i as f64 + 0.5).min(s as f64) / s as f64 } else { i as f64 / s as f64 };
            let n = around(u) * phi.sin() + Vec3::new(0.0, -phi.cos(), 0.0);
            m.vertex(n * radius, n, [u, v]);
        }
    }
    m.cells(0, s, r, true);
    m
}

/// A cylinder of `radius` and `height` along Y, centered on the origin,
/// `segments` = [around (3+), up (1+)], with flat end caps if `caps`. The
/// side's u runs once around (seam at −Z, as on [`sphere`]), v bottom to
/// top; the caps map the texture flat, seen from outside.
pub fn cylinder(radius: f64, height: f64, segments: [usize; 2], caps: bool) -> Mesh {
    let (s, r) = (segments[0].max(3), segments[1].max(1));
    let h = height / 2.0;
    let mut m = Mesh::default();
    for j in 0..=r {
        let v = j as f64 / r as f64;
        for i in 0..=s {
            let u = i as f64 / s as f64;
            let n = around(u);
            m.vertex(n * radius + Vec3::new(0.0, -h + height * v, 0.0), n, [u, v]);
        }
    }
    m.cells(0, s, r, false);
    if caps {
        for top in [true, false] {
            let (y, n) = if top { (h, Vec3::y()) } else { (-h, -Vec3::y()) };
            // Seen from outside: from above −Z is up, from below +Z is.
            let flip = if top { -1.0 } else { 1.0 };
            let center = m.vertex(Vec3::new(0.0, y, 0.0), n, [0.5, 0.5]);
            for i in 0..=s {
                let p = around(i as f64 / s as f64);
                m.vertex(p * radius + Vec3::new(0.0, y, 0.0), n, [0.5 + p.x / 2.0, 0.5 + flip * p.z / 2.0]);
            }
            for i in 0..s as u32 {
                let (a, b) = (center + 1 + i, center + 2 + i);
                m.triangles.push(if top { [center, a, b] } else { [center, b, a] });
            }
        }
    }
    m
}

/// A flat rectangle `size` = [width, height] in the XY plane, centered on
/// the origin and facing +Z (as a Card), split into `segments` = [across,
/// up] cells. A 1 × 1 plane is a card's two triangles.
pub fn plane(size: [f64; 2], segments: [usize; 2]) -> Mesh {
    let mut m = Mesh::default();
    m.grid(Vec3::new(-size[0] / 2.0, -size[1] / 2.0, 0.0), Vec3::x() * size[0], Vec3::y() * size[1], segments[0], segments[1]);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed volume (positive when the triangles face out of a closed
    /// mesh).
    fn volume(m: &Mesh) -> f64 {
        m.triangles.iter().map(|t| {
            let [a, b, c] = t.map(|i| m.positions[i as usize]);
            a.dot(&b.cross(&c)) / 6.0
        }).sum()
    }

    /// Every attribute per vertex, normals of unit length, indices in
    /// range, UVs within 0..1, and each triangle facing the way its
    /// vertices' normals do; with `closed`, facing away from the center.
    fn check(m: &Mesh, closed: bool) {
        let n = m.positions.len();
        assert!(m.normals.len() == n && m.uvs.len() == n);
        assert!(m.normals.iter().all(|v| (v.norm() - 1.0).abs() < 1e-12));
        assert!(m.uvs.iter().all(|uv| uv.iter().all(|c| (0.0..=1.0).contains(c))));
        for (t, tri) in m.triangles.iter().enumerate() {
            assert!(tri.iter().all(|&i| (i as usize) < n));
            let f = m.face_normal(t);
            assert!(f.norm() > 1e-12, "triangle {t} is degenerate");
            assert!(tri.iter().all(|&i| f.dot(&m.normals[i as usize]) > 0.0), "triangle {t} faces in");
            if closed {
                let mid = tri.iter().map(|&i| m.positions[i as usize]).sum::<Vec3>() / 3.0;
                assert!(f.dot(&mid) > 0.0, "triangle {t} faces the center");
            }
        }
    }

    #[test]
    fn a_cube_has_its_size_counts_and_outward_faces() {
        let m = cube([2.0, 4.0, 6.0], [1, 1, 1]);
        assert_eq!((m.positions.len(), m.triangles.len()), (24, 12));
        check(&m, true);
        assert_eq!(m.bounds(), Some((Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0))));
        assert!((volume(&m) - 48.0).abs() < 1e-9);
        let m = cube([1.0; 3], [2, 3, 4]);
        // Faces: two of 2 × 3, two of 4 × 3, two of 2 × 4 cells.
        let cells = 2 * (2 * 3 + 4 * 3 + 2 * 4);
        let verts = 2 * (3 * 4 + 5 * 4 + 3 * 5);
        assert_eq!((m.positions.len(), m.triangles.len()), (verts, 2 * cells));
        check(&m, true);
        assert!((volume(&m) - 1.0).abs() < 1e-9);
        // The front face's texture is upright: u to the right, v up.
        let front: Vec<usize> = (0..m.positions.len()).filter(|&i| m.normals[i] == Vec3::z()).collect();
        assert!(front.iter().all(|&i| (m.uvs[i][0] - (m.positions[i].x + 0.5)).abs() < 1e-12 && (m.uvs[i][1] - (m.positions[i].y + 0.5)).abs() < 1e-12));
    }

    #[test]
    fn a_sphere_is_round_outward_and_seamless() {
        let (s, r) = (16, 8);
        let m = sphere(2.0, [s, r]);
        assert_eq!((m.positions.len(), m.triangles.len()), ((s + 1) * (r + 1), 2 * s * (r - 1)));
        check(&m, true);
        assert!(m.positions.iter().zip(&m.normals).all(|(p, n)| (p - n * 2.0).norm() < 1e-12), "normals along the radius");
        let (lo, hi) = m.bounds().unwrap();
        assert!((lo + Vec3::repeat(2.0)).norm() < 0.1 && (hi - Vec3::repeat(2.0)).norm() < 0.1 && (hi.y - 2.0).abs() < 1e-12);
        let true_volume = 4.0 / 3.0 * std::f64::consts::PI * 8.0;
        assert!(volume(&m) > 0.9 * true_volume && volume(&m) < true_volume);
        // No triangle spans the seam: its u values stay within a segment
        // (and a half, at the poles).
        for t in &m.triangles {
            let us = t.map(|i| m.uvs[i as usize][0]);
            let span = us.iter().copied().fold(f64::MIN, f64::max) - us.iter().copied().fold(f64::MAX, f64::min);
            assert!(span <= 1.0 / s as f64 + 1e-12, "a triangle wraps around: {us:?}");
        }
        // Across the seam, the two sides' vertices meet; u = 0.5 faces +Z
        // and grows to the right (+X) seen from there.
        let at = |i: usize, j: usize| j * (s + 1) + i;
        for j in 1..r {
            assert!((m.positions[at(0, j)] - m.positions[at(s, j)]).norm() < 1e-12);
            assert!(m.positions[at(0, j)].z < 0.0 && m.positions[at(0, j)].x.abs() < 1e-12);
        }
        let eq = at(s / 2, r / 2);
        assert!((m.positions[eq] - Vec3::new(0.0, 0.0, 2.0)).norm() < 1e-12);
        assert!(m.positions[eq + 1].x > 0.0);
    }

    #[test]
    fn a_cylinder_has_a_side_and_caps() {
        let (s, r) = (12, 3);
        let m = cylinder(1.0, 4.0, [s, r], true);
        assert_eq!(m.positions.len(), (s + 1) * (r + 1) + 2 * (s + 2));
        assert_eq!(m.triangles.len(), 2 * s * r + 2 * s);
        check(&m, true);
        assert_eq!(m.bounds().map(|b| (b.0.y, b.1.y)), Some((-2.0, 2.0)));
        let area = 0.5 * s as f64 * (TAU / s as f64).sin();
        assert!((volume(&m) - area * 4.0).abs() < 1e-9);
        // The side's normals are level; the caps' straight up and down.
        let side = (s + 1) * (r + 1);
        assert!(m.normals[..side].iter().all(|n| n.y == 0.0));
        assert!(m.normals[side..].iter().all(|n| n.y.abs() == 1.0));
        // Without caps it's open: only the side.
        let open = cylinder(1.0, 4.0, [s, r], false);
        assert_eq!((open.positions.len(), open.triangles.len()), (side, 2 * s * r));
        check(&open, true);
    }

    #[test]
    fn a_plane_faces_z_like_a_card() {
        let m = plane([4.0, 2.0], [1, 1]);
        assert_eq!((m.positions.len(), m.triangles.len()), (4, 2));
        check(&m, false);
        assert!(m.normals.iter().all(|n| *n == Vec3::z()));
        assert!((0..2).all(|t| m.face_normal(t).normalize() == Vec3::z()));
        assert_eq!(m.bounds(), Some((Vec3::new(-2.0, -1.0, 0.0), Vec3::new(2.0, 1.0, 0.0))));
        // Corners: bottom left is uv (0, 0), top right (1, 1).
        assert_eq!((m.positions[0], m.uvs[0]), (Vec3::new(-2.0, -1.0, 0.0), [0.0, 0.0]));
        assert_eq!((m.positions[3], m.uvs[3]), (Vec3::new(2.0, 1.0, 0.0), [1.0, 1.0]));
        let m = plane([1.0, 1.0], [4, 2]);
        assert_eq!((m.positions.len(), m.triangles.len()), (15, 16));
        check(&m, false);
    }

    #[test]
    fn transforms_keep_normals_and_faces_outward() {
        // A squashed, mirrored, turned and moved sphere: still closed and
        // facing out, normals at right angles to the surface.
        let mut m = sphere(1.0, [12, 6]);
        let world = geom::trs([1.0, 2.0, 3.0], [20.0, 30.0, 40.0], [-2.0, 0.5, 1.0]);
        m.transform(&world);
        assert!(volume(&m) > 0.0, "still facing out after a mirror");
        let center = Vec3::new(1.0, 2.0, 3.0);
        let mut moved = m.clone();
        moved.transform(&geom::trs([-1.0, -2.0, -3.0], [0.0; 3], [1.0; 3]));
        for (t, tri) in moved.triangles.iter().enumerate() {
            let f = moved.face_normal(t);
            assert!(tri.iter().all(|&i| f.dot(&moved.normals[i as usize]) > 0.0));
        }
        assert!(m.positions.iter().all(|p| (p - center).norm() <= 2.0 + 1e-9));
        // The normals are across the surface: on a fine sphere, close to
        // the faces' own.
        let mut fine = sphere(1.0, [128, 64]);
        fine.transform(&world);
        let worst = (0..fine.triangles.len())
            .flat_map(|t| fine.triangles[t].map(|i| fine.face_normal(t).normalize().dot(&fine.normals[i as usize])))
            .fold(1.0, f64::min);
        assert!(worst > 0.99, "a normal {}° off its face", worst.acos().to_degrees());
    }

    #[test]
    fn meshes_merge_and_get_normals() {
        let (a, b) = (cube([1.0; 3], [1, 1, 1]), plane([1.0, 1.0], [1, 1]));
        let m = Mesh::merge(&[a.clone(), b.clone()]);
        assert_eq!((m.positions.len(), m.triangles.len()), (28, 14));
        assert_eq!(m.triangles[12], b.triangles[0].map(|i| i + 24));
        assert_eq!((m.normals.len(), m.uvs.len()), (28, 28));
        // A mesh without texture coordinates drops them from the merge.
        let bare = Mesh { uvs: Vec::new(), ..b };
        assert!(Mesh::merge(&[a.clone(), bare]).uvs.is_empty());
        assert!(Mesh::default().bounds().is_none());
        // Smooth normals on a cube point out of its corners; flat ones
        // along its faces.
        let mut s = a.clone();
        s.smooth_normals();
        assert!(s.positions.iter().zip(&s.normals).all(|(p, n)| (p.normalize() - n).norm() < 1e-12));
        let f = s.flat();
        assert_eq!((f.positions.len(), f.triangles.len()), (36, 12));
        assert!(f.normals.iter().all(|n| n.iter().filter(|c| c.abs() == 1.0).count() == 1));
        check(&f, true);
    }
}
