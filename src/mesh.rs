//! Triangle meshes for 3D objects: primitives (cube, sphere, cylinder,
//! plane), materials, and Wavefront OBJ / MTL files.
//!
//! A mesh is in its object's own space (the scene places it); Y is up, as
//! in [`crate::geom`]. Attributes are per vertex: a corner with its own
//! normal or texture coordinate (a hard edge, a UV seam) is its own vertex.
//! Triangles wind counter-clockwise seen from outside, so `(b − a) × (c − a)`
//! points out, the way the normals do. Texture coordinates run left to
//! right and bottom to top (v = 0 at the bottom of the image), as OBJ and
//! GL have them.

use crate::geo::Attrs;
use crate::geom::{self, Mat4, Vec3};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

/// A triangle mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    /// Unit normals, one per position; empty for flat shading.
    pub normals: Vec<Vec3>,
    /// Texture coordinates, one per position, or empty.
    pub uvs: Vec<[f64; 2]>,
    pub triangles: Vec<[u32; 3]>,
    /// Named attributes per position and per triangle (see
    /// [`crate::geo`]): positions and normals stay in their own lists.
    pub point_attrs: Attrs,
    pub face_attrs: Attrs,
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
    /// scale), and the winding reversed if `m` mirrors; the attributes
    /// that are directions too (see [`Attrs::transform`]).
    pub fn transform(&mut self, m: &Mat4) {
        self.point_attrs.transform(m);
        self.face_attrs.transform(m);
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
    /// when every mesh has them; attributes are joined (see
    /// [`Attrs::concat`]: zeros where a mesh hasn't one).
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
        out.point_attrs = Attrs::concat(&meshes.iter().map(|m| (&m.point_attrs, m.positions.len())).collect::<Vec<_>>());
        out.face_attrs = Attrs::concat(&meshes.iter().map(|m| (&m.face_attrs, m.triangles.len())).collect::<Vec<_>>());
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
    /// normal (and its points' attributes); the faces keep theirs.
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
        let corners: Vec<u32> = self.triangles.iter().flatten().copied().collect();
        out.point_attrs = self.point_attrs.gathered(&corners);
        out.face_attrs = self.face_attrs.clone();
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
        self.grid_in(origin, du, dv, nu, nv, ([0.0, 0.0], [1.0, 1.0]));
    }

    /// [`Mesh::grid`] with its texture coordinates in `cell` (its corner
    /// and size in uv) instead of the whole square.
    fn grid_in(&mut self, origin: Vec3, du: Vec3, dv: Vec3, nu: usize, nv: usize, cell: ([f64; 2], [f64; 2])) {
        let n = du.cross(&dv).normalize();
        let (nu, nv) = (nu.max(1), nv.max(1));
        let base = self.positions.len() as u32;
        let ([u0, v0], [w, h]) = cell;
        for j in 0..=nv {
            for i in 0..=nu {
                let (u, v) = (i as f64 / nu as f64, j as f64 / nv as f64);
                self.vertex(origin + du * u + dv * v, n, [u0 + u * w, v0 + v * h]);
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

/// How a [`cube`]'s faces share the texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CubeUvs {
    /// Each face uses the whole uv square.
    PerFace,
    /// Each face in its own cell of a 3 × 2 grid, read as on the texture
    /// (v up): +X, −X, +Y along the top row, −Y, +Z, −Z along the bottom,
    /// [`ATLAS_GUTTER`] apart, so one texture can paint (or a bake fill)
    /// every face.
    Atlas,
}

/// The empty uv space between two cells of a [`CubeUvs::Atlas`] (half of
/// it round the outside), so a bake's padding and mip filtering don't run
/// from one face into the next.
pub const ATLAS_GUTTER: f64 = 0.02;

/// A box `size` across (x, y, z), centered on the origin, each face split
/// into `segments` along its axes. Each face has its own vertices (hard
/// edges) and texture coordinates 0 to 1, upright seen from outside (the
/// top and bottom with −Z and +Z up, respectively).
pub fn cube(size: [f64; 3], segments: [usize; 3]) -> Mesh {
    cube_uvs(size, segments, CubeUvs::PerFace)
}

/// [`cube`] with its faces laid out in uv by `layout`; each is upright
/// seen from outside either way.
pub fn cube_uvs(size: [f64; 3], segments: [usize; 3], layout: CubeUvs) -> Mesh {
    let [hx, hy, hz] = size.map(|v| v / 2.0);
    let [sx, sy, sz] = size;
    let [nx, ny, nz] = segments;
    let (x, y, z) = (Vec3::x(), Vec3::y(), Vec3::z());
    // The face's cell, by its place in +X −X +Y −Y +Z −Z.
    let cell = |k: usize| match layout {
        CubeUvs::PerFace => ([0.0, 0.0], [1.0, 1.0]),
        CubeUvs::Atlas => {
            let (col, row) = ((k % 3) as f64, if k < 3 { 1.0 } else { 0.0 });
            let g = ATLAS_GUTTER / 2.0;
            ([col / 3.0 + g, row / 2.0 + g], [1.0 / 3.0 - 2.0 * g, 0.5 - 2.0 * g])
        }
    };
    let mut m = Mesh::default();
    m.grid_in(Vec3::new(-hx, -hy, hz), x * sx, y * sy, nx, ny, cell(4)); // +Z
    m.grid_in(Vec3::new(hx, -hy, -hz), -x * sx, y * sy, nx, ny, cell(5)); // −Z
    m.grid_in(Vec3::new(hx, -hy, hz), -z * sz, y * sy, nz, ny, cell(0)); // +X
    m.grid_in(Vec3::new(-hx, -hy, -hz), z * sz, y * sy, nz, ny, cell(1)); // −X
    m.grid_in(Vec3::new(-hx, hy, hz), x * sx, -z * sz, nx, nz, cell(2)); // +Y
    m.grid_in(Vec3::new(-hx, -hy, -hz), x * sx, z * sz, nx, nz, cell(3)); // −Y
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

/// One object or group of a geometry file, in one material.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub mesh: Mesh,
    pub material: Material,
}

/// The parts of the Wavefront OBJ file at `path` (see [`parse_obj`]).
pub fn read_obj(path: &Path) -> Result<Vec<Part>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
    parse_obj(&text, path.parent().unwrap_or(Path::new("."))).with_context(|| format!("in {}", path.display()))
}

/// A face corner: position, texture coordinate and normal indices (from 0).
type Corner = (usize, Option<usize>, Option<usize>);

/// A part being read.
struct Building {
    group: String,
    material: String,
    mesh: Mesh,
    corners: HashMap<Corner, u32>,
    /// Each vertex's position index in the file, and whether it has a
    /// normal and texture coordinate there.
    from: Vec<Corner>,
}

/// The index an OBJ index refers to, out of `count` so far: from 1, or
/// from the end when negative.
fn obj_index(s: &str, count: usize, what: &str) -> Result<usize> {
    let i: i64 = s.parse().map_err(|_| anyhow!("'{s}' isn't an index"))?;
    let k = if i > 0 { i - 1 } else { count as i64 + i };
    if i == 0 || k < 0 || k >= count as i64 {
        bail!("{what} {i} doesn't exist ({count} so far)");
    }
    Ok(k as usize)
}

/// Numbers `n` of `words` (missing ones `default`).
fn numbers<const N: usize>(words: &[&str], default: f64) -> Result<[f64; N]> {
    let mut out = [default; N];
    for (o, w) in out.iter_mut().zip(words) {
        *o = w.parse().map_err(|_| anyhow!("'{w}' isn't a number"))?;
    }
    Ok(out)
}

/// Wavefront OBJ text: positions (`v`), texture coordinates (`vt`), normals
/// (`vn`) and faces (`f`, polygons split into fans; indices from 1, or
/// negative from the end), into one [`Part`] per object or group (`o`,
/// `g`) and material (`usemtl`, from the `mtllib` files in `folder`; see
/// [`parse_mtl`]). A part's name is its group's (or object's), with
/// `.<material>` when the group has more than one. Corners without normals
/// get smooth ones from the faces around their position; without texture
/// coordinates, (0, 0) (none at all when the part has none). Lines, points,
/// smoothing groups and the rest are skipped.
pub fn parse_obj(text: &str, folder: &Path) -> Result<Vec<Part>> {
    let (mut v, mut vt, mut vn): (Vec<Vec3>, Vec<[f64; 2]>, Vec<Vec3>) = Default::default();
    let mut materials: HashMap<String, Material> = HashMap::new();
    let mut parts: Vec<Building> = Vec::new();
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    let (mut group, mut material) = ("default".to_string(), String::new());
    for (n, line) in text.lines().enumerate() {
        let words: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        let Some((&key, args)) = words.split_first() else { continue };
        let fail = |e: anyhow::Error| e.context(format!("line {}: {}", n + 1, line.trim()));
        match key {
            "v" => v.push(Vec3::from(numbers::<3>(args, 0.0).map_err(fail)?)),
            "vt" => vt.push(numbers::<2>(args, 0.0).map_err(fail)?),
            "vn" => vn.push(Vec3::from(numbers::<3>(args, 0.0).map_err(fail)?).try_normalize(1e-300).unwrap_or_else(Vec3::y)),
            "o" | "g" => group = if args.is_empty() { "default".into() } else { args.join(" ") },
            "usemtl" => material = args.join(" "),
            "mtllib" => {
                // One name with spaces, or several; a missing library
                // leaves its materials plain.
                let joined = folder.join(args.join(" "));
                let files: Vec<PathBuf> = if joined.is_file() { vec![joined] } else { args.iter().map(|a| folder.join(a)).collect() };
                for path in files {
                    if let Ok(src) = std::fs::read_to_string(&path) {
                        for m in parse_mtl(&src, path.parent().unwrap_or(folder)) {
                            materials.insert(m.name.clone(), m);
                        }
                    }
                }
            }
            "f" => {
                let corners: Vec<Corner> = args
                    .iter()
                    .map(|c| -> Result<Corner> {
                        let mut it = c.split('/');
                        let p = obj_index(it.next().unwrap_or(""), v.len(), "position")?;
                        let t = it.next().filter(|s| !s.is_empty()).map(|s| obj_index(s, vt.len(), "texture coordinate")).transpose()?;
                        let nn = it.next().filter(|s| !s.is_empty()).map(|s| obj_index(s, vn.len(), "normal")).transpose()?;
                        Ok((p, t, nn))
                    })
                    .collect::<Result<_>>()
                    .map_err(fail)?;
                if corners.len() < 3 {
                    return Err(fail(anyhow!("a face needs three corners")));
                }
                let k = *at.entry((group.clone(), material.clone())).or_insert_with(|| {
                    parts.push(Building { group: group.clone(), material: material.clone(), mesh: Mesh::default(), corners: HashMap::new(), from: Vec::new() });
                    parts.len() - 1
                });
                let b = &mut parts[k];
                let ids: Vec<u32> = corners
                    .iter()
                    .map(|&c| {
                        *b.corners.entry(c).or_insert_with(|| {
                            b.mesh.positions.push(v[c.0]);
                            b.mesh.normals.push(c.2.map_or_else(Vec3::zeros, |i| vn[i]));
                            b.mesh.uvs.push(c.1.map_or([0.0, 0.0], |i| vt[i]));
                            b.from.push(c);
                            b.mesh.positions.len() as u32 - 1
                        })
                    })
                    .collect();
                for i in 1..ids.len() - 1 {
                    b.mesh.triangles.push([ids[0], ids[i], ids[i + 1]]);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for b in &parts {
        let mut mesh = b.mesh.clone();
        if b.from.iter().all(|c| c.1.is_none()) {
            mesh.uvs.clear();
        }
        // Smooth normals for corners without one, from every face of the
        // part at their position (by its index in the file).
        if b.from.iter().any(|c| c.2.is_none()) {
            let mut sums: HashMap<usize, Vec3> = HashMap::new();
            for t in 0..mesh.triangles.len() {
                let Some(n) = mesh.face_normal(t).try_normalize(1e-300) else { continue };
                let tri = mesh.triangles[t].map(|i| i as usize);
                for k in 0..3 {
                    let p = mesh.positions[tri[k]];
                    let angle = (mesh.positions[tri[(k + 1) % 3]] - p).angle(&(mesh.positions[tri[(k + 2) % 3]] - p));
                    *sums.entry(b.from[tri[k]].0).or_insert_with(Vec3::zeros) += n * angle;
                }
            }
            for (i, c) in b.from.iter().enumerate() {
                if c.2.is_none() {
                    mesh.normals[i] = sums.get(&c.0).and_then(|n| n.try_normalize(1e-300)).unwrap_or_else(Vec3::y);
                }
            }
        }
        let several = parts.iter().filter(|o| o.group == b.group).count() > 1;
        let name = if several && !b.material.is_empty() { format!("{}.{}", b.group, b.material) } else { b.group.clone() };
        let material = materials
            .get(&b.material)
            .cloned()
            .unwrap_or_else(|| Material { name: b.material.clone(), shading: Shading::Lambert, ..Material::default() });
        out.push(Part { name, mesh, material });
    }
    Ok(out)
}

/// The materials of Wavefront MTL text: `newmtl` names, colors `Kd`
/// (diffuse), `Ks` (specular) and `Ke` (emission), opacity `d` (or `Tr`,
/// its complement), shininess `Ns` (as roughness √(2 / (Ns + 2))), `illum`
/// (0 unlit, 1 Lambert, 2 and up Blinn; without it, Blinn when `Ks` isn't
/// black) and the `map_Kd` image, resolved against `folder`.
pub fn parse_mtl(text: &str, folder: &Path) -> Vec<Material> {
    let mut out: Vec<(Material, Option<i64>)> = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        let Some((&key, args)) = words.split_first() else { continue };
        if key == "newmtl" {
            out.push((Material { name: args.join(" "), shading: Shading::Lambert, ..Material::default() }, None));
            continue;
        }
        let Some((m, illum)) = out.last_mut() else { continue };
        let rgb = || numbers::<3>(args, f64::NAN).ok().map(|c| {
            // One number is a gray.
            let c = if c[1].is_nan() { [c[0]; 3] } else { c };
            c.map(|x| x as f32)
        });
        let one = || args.first().and_then(|s| s.parse::<f64>().ok());
        match key {
            "Kd" => m.color = rgb().unwrap_or(m.color),
            "Ks" => m.specular = rgb().unwrap_or(m.specular),
            "Ke" => m.emission = rgb().unwrap_or(m.emission),
            "d" => m.opacity = one().map_or(m.opacity, |d| d as f32),
            "Tr" => m.opacity = one().map_or(m.opacity, |t| 1.0 - t as f32),
            "Ns" => m.roughness = one().map_or(m.roughness, |ns| (2.0 / (ns.max(0.0) + 2.0)).sqrt() as f32),
            "illum" => *illum = one().map(|i| i as i64),
            "map_Kd" => {
                // Options (-s 1 1 1, -clamp on, ...) come before the file
                // name, which may hold spaces.
                let mut rest = args;
                while rest.len() > 1 && rest[0].starts_with('-') {
                    let more = &rest[1..];
                    let n = more.iter().take_while(|s| s.parse::<f64>().is_ok() || matches!(**s, "on" | "off")).count();
                    rest = &more[n.max(1).min(more.len() - 1)..];
                }
                if !rest.is_empty() {
                    m.texture = Some(folder.join(rest.join(" ").replace('\\', "/")));
                }
            }
            _ => {}
        }
    }
    out.into_iter()
        .map(|(mut m, illum)| {
            m.shading = match illum {
                Some(0) => Shading::Unlit,
                Some(1) => Shading::Lambert,
                Some(_) => Shading::Blinn,
                None if m.specular.iter().any(|c| *c > 0.0) => Shading::Blinn,
                None => Shading::Lambert,
            };
            m
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attributes follow the mesh: merged (zeros where a mesh has none),
    /// per corner when faceted, turned with it where they're directions.
    #[test]
    fn attributes_follow_merge_flat_and_transform() {
        use crate::geo::{Attr, N};
        let mut a = plane([1.0, 1.0], [1, 1]);
        let (np, nf) = (a.positions.len(), a.triangles.len());
        a.point_attrs.set("w", Attr::Float((0..np).map(|i| i as f32).collect()), np).unwrap();
        a.point_attrs.set(N, Attr::Vec3(vec![[0.0, 0.0, 1.0]; np]), np).unwrap();
        a.face_attrs.set("id", Attr::Int((0..nf as i32).collect()), nf).unwrap();
        let b = cube([1.0; 3], [1; 3]);
        let m = Mesh::merge(&[a.clone(), b.clone()]);
        assert!(m.point_attrs.check(m.positions.len()).is_ok() && m.face_attrs.check(m.triangles.len()).is_ok());
        assert_eq!(m.point_attrs.float("w").unwrap()[np], 0.0, "the cube's: zeros");
        assert_eq!(m.face_attrs.int("id").unwrap()[..nf], (0..nf as i32).collect::<Vec<_>>()[..]);
        let f = a.flat();
        let corners: Vec<f32> = a.triangles.iter().flatten().map(|&i| i as f32).collect();
        assert_eq!(f.point_attrs.float("w"), Some(&corners[..]));
        assert_eq!(f.face_attrs, a.face_attrs);
        let mut t = a.clone();
        t.transform(&geom::trs([0.0; 3], [90.0, 0.0, 0.0], [1.0; 3]));
        let n = t.point_attrs.vec3(N).unwrap()[0];
        assert!((n[1] + 1.0).abs() < 1e-6, "{n:?}: +z turned 90° about x is -y");
        assert_eq!(t.point_attrs.float("w"), a.point_attrs.float("w"));
    }

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

    /// Per face: each face's uvs are its grid's (i / nu, j / nv), exactly
    /// as before the atlas; atlas: the same faces, each in its own cell
    /// minus the gutter, the cells apart and in the unit square, upright as
    /// per face (the same uvs, moved and scaled).
    #[test]
    fn a_cubes_uvs_per_face_and_in_an_atlas() {
        let segments = [2, 3, 4];
        let per_face = cube_uvs([1.0, 2.0, 3.0], segments, CubeUvs::PerFace);
        assert_eq!(per_face.uvs, cube([1.0, 2.0, 3.0], segments).uvs);
        let [nx, ny, nz] = segments;
        let faces = [(nx, ny), (nx, ny), (nz, ny), (nz, ny), (nx, nz), (nx, nz)];
        let mut old = Vec::new();
        for (nu, nv) in faces {
            for j in 0..=nv {
                for i in 0..=nu {
                    old.push([i as f64 / nu as f64, j as f64 / nv as f64]);
                }
            }
        }
        assert_eq!(per_face.uvs, old, "bit for bit");
        let atlas = cube_uvs([1.0, 2.0, 3.0], segments, CubeUvs::Atlas);
        assert_eq!((atlas.positions.clone(), atlas.triangles.clone()), (per_face.positions.clone(), per_face.triangles.clone()));
        // The faces in mesh order (+Z −Z +X −X +Y −Y), and their cells.
        let cell_of = [4, 5, 0, 1, 2, 3];
        let g = ATLAS_GUTTER / 2.0;
        let mut start = 0;
        let mut boxes = Vec::new();
        for (f, (nu, nv)) in faces.into_iter().enumerate() {
            let n = (nu + 1) * (nv + 1);
            let k = cell_of[f];
            let (lo, hi) = ([(k % 3) as f64 / 3.0 + g, if k < 3 { 0.5 } else { 0.0 } + g], [(k % 3 + 1) as f64 / 3.0 - g, if k < 3 { 1.0 } else { 0.5 } - g]);
            for i in start..start + n {
                let (a, p) = (atlas.uvs[i], per_face.uvs[i]);
                let want = [lo[0] + p[0] * (hi[0] - lo[0]), lo[1] + p[1] * (hi[1] - lo[1])];
                assert!((a[0] - want[0]).abs() < 1e-12 && (a[1] - want[1]).abs() < 1e-12, "face {f} vertex {i}: {a:?} vs {want:?}");
            }
            let us = atlas.uvs[start..start + n].iter();
            let (umin, umax) = us.clone().fold((f64::MAX, f64::MIN), |(a, b), uv| (a.min(uv[0]), b.max(uv[0])));
            let (vmin, vmax) = us.fold((f64::MAX, f64::MIN), |(a, b), uv| (a.min(uv[1]), b.max(uv[1])));
            assert!((umin - lo[0]).abs() < 1e-12 && (umax - hi[0]).abs() < 1e-12 && (vmin - lo[1]).abs() < 1e-12 && (vmax - hi[1]).abs() < 1e-12, "face {f} fills its cell");
            assert!(umin > 0.0 && vmin > 0.0 && umax < 1.0 && vmax < 1.0);
            boxes.push((umin, umax, vmin, vmax));
            start += n;
        }
        for a in 0..6 {
            for b in a + 1..6 {
                let (p, q) = (boxes[a], boxes[b]);
                let gap = (q.0 - p.1).max(p.0 - q.1).max(q.2 - p.3).max(p.2 - q.3);
                assert!(gap >= ATLAS_GUTTER - 1e-12, "faces {a} and {b} {ATLAS_GUTTER} apart: {gap}");
            }
        }
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


    /// A box in two groups: the sides as quads with texture coordinates but
    /// no normals (some by negative index), the top and bottom with
    /// normals, the top a pentagon fan; materials from an MTL file.
    const BOX: &str = "# test box
mtllib box.mtl
o box
v -1 0 -1
v 1 0 -1
v 1 2 -1
v -1 2 -1
v -1 0 1
v 1 0 1
v 1 2 1
v -1 2 1
v 0 2 -1   # mid top back edge
vt 0 0
vt 1 0
vt 1 1
vt 0 1
vn 0 1 0
vn 0 -1 0
usemtl red
f 5/1 6/2 7/3 8/4
f -8/1 -9/2 -6/3 -7/4
usemtl blue
f 6/1 2/2 3/3 7/4
f 1/1 5/2 8/3 4/4
g lid
usemtl wood
f 8//1 7//1 3//1 9//1 4//1
f 1//2 2//2 6//2 5//2
";

    const BOX_MTL: &str = "newmtl red
Kd 0.8 0.1 0.1
illum 1
newmtl blue
Kd 0.1 0.1 0.9
Ks 0.5 0.5 0.5
Ns 198
d 0.5
newmtl wood
Kd 1 1 1
Ke 0.2
map_Kd -s 1 1 1 -clamp on textures\\wood grain.png
";

    #[test]
    fn an_obj_file_reads_into_parts_with_materials() {
        let dir = std::env::temp_dir().join(format!("arclight_obj_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("box.obj"), BOX).unwrap();
        std::fs::write(dir.join("box.mtl"), BOX_MTL).unwrap();
        let parts = read_obj(&dir.join("box.obj")).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["box.red", "box.blue", "lid"]);
        let (red, blue, lid) = (&parts[0], &parts[1], &parts[2]);
        // Two quads each: eight corners, four triangles; the lid a
        // pentagon (three triangles) and a quad (two).
        assert_eq!((red.mesh.positions.len(), red.mesh.triangles.len()), (8, 4));
        assert_eq!((blue.mesh.positions.len(), blue.mesh.triangles.len()), (8, 4));
        assert_eq!((lid.mesh.positions.len(), lid.mesh.triangles.len()), (9, 5));
        assert_eq!(Mesh::merge(&parts.iter().map(|p| p.mesh.clone()).collect::<Vec<_>>()).bounds(), Some((Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 2.0, 1.0))));
        // Texture coordinates where given, none where not.
        assert_eq!(red.mesh.uvs[..4], [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        assert!(lid.mesh.uvs.is_empty() && lid.mesh.normals.len() == 9);
        // Normals: the file's on the lid; made, facing out, on the sides.
        assert!(lid.mesh.normals[..5].iter().all(|n| *n == Vec3::y()) && lid.mesh.normals[5..].iter().all(|n| *n == -Vec3::y()));
        assert_eq!(red.mesh.normals[0], Vec3::z());
        assert_eq!(red.mesh.normals[4], -Vec3::z());
        assert!(blue.mesh.normals[..4].iter().all(|n| *n == Vec3::x()));
        for p in &parts {
            for t in 0..p.mesh.triangles.len() {
                let f = p.mesh.face_normal(t);
                assert!(p.mesh.triangles[t].iter().all(|&i| f.dot(&p.mesh.normals[i as usize]) > 0.0), "{} triangle {t}", p.name);
            }
        }
        // Materials.
        assert_eq!((red.material.color, red.material.shading), ([0.8, 0.1, 0.1], Shading::Lambert));
        let b = &blue.material;
        assert_eq!((b.shading, b.specular, b.opacity), (Shading::Blinn, [0.5; 3], 0.5));
        assert!((b.roughness - 0.1).abs() < 1e-6);
        let w = &lid.material;
        assert_eq!((w.emission, w.texture.as_deref()), ([0.2; 3], Some(dir.join("textures/wood grain.png").as_path())));
    }

    #[test]
    fn bad_obj_files_say_where() {
        let e = parse_obj("v 0 0 0\nv 1 0 0\nf 1 2 3\n", Path::new(".")).unwrap_err();
        assert!(format!("{e:#}").contains("line 3") && format!("{e:#}").contains("position 3"), "{e:#}");
        let e = parse_obj("v 0 0 x\n", Path::new(".")).unwrap_err();
        assert!(format!("{e:#}").contains("line 1"), "{e:#}");
        assert!(parse_obj("v 0 0 0\nf 1 1\n", Path::new(".")).is_err());
        // A missing material library leaves plain materials; no faces, no
        // parts.
        let parts = parse_obj("mtllib nowhere.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl x\nf 1 2 3\n", Path::new("/nonexistent")).unwrap();
        assert_eq!((parts.len(), parts[0].name.as_str(), parts[0].material.name.as_str()), (1, "default", "x"));
        assert_eq!(parts[0].mesh.normals[0], Vec3::z());
        assert!(parse_obj("v 0 0 0\n", Path::new(".")).unwrap().is_empty());
    }

    #[test]
    fn the_example_obj_reads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geo/crate.obj");
        let parts = read_obj(&path).unwrap();
        assert_eq!(parts.len(), 1);
        let (m, mat) = (&parts[0].mesh, &parts[0].material);
        assert_eq!((parts[0].name.as_str(), m.positions.len(), m.triangles.len(), m.uvs.len()), ("crate", 24, 12, 24));
        assert_eq!(m.bounds(), Some((Vec3::repeat(-0.5), Vec3::repeat(0.5))));
        for t in 0..m.triangles.len() {
            let f = m.face_normal(t).normalize();
            assert!(f.dot(&f.map(|c| c.signum() * c.abs().round())) > 0.999, "faces along an axis");
            assert!(m.triangles[t].iter().all(|&i| (m.normals[i as usize] - f).norm() < 1e-12), "triangle {t}");
            let mid = m.triangles[t].iter().map(|&i| m.positions[i as usize]).sum::<Vec3>() / 3.0;
            assert!(f.dot(&mid) > 0.0, "triangle {t} faces in");
        }
        assert_eq!((mat.name.as_str(), mat.color, mat.shading), ("crate", [0.6, 0.4, 0.2], Shading::Blinn));
    }
}
