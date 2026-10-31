//! Geometry nodes' work (docs/geo-plan.md, P2 on): each takes a scene and
//! gives one back, acting on its meshes, point sets, curves and instances
//! and passing the rest through. `ops.rs` registers them.
//!
//! Random choices are stable (D5): each comes from a hash of the seed and
//! where it's made (a triangle and a candidate's number on it, a cell),
//! never from a running generator, so an edit (more density, a painted
//! mask) keeps most points where they were, with their ids.

use crate::geo::{self, stable_hash, stable_unit, Attr, Attrs, PointSet};
use crate::geom::{self, Mat4, Vec3};
use crate::image::Image;
use crate::mesh::Mesh;
use crate::scene::Scene;
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The most points a Scatter makes.
pub const MAX_POINTS: usize = 5_000_000;

/// Where Scatter puts points.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// On the meshes' and cards' surfaces, evenly by area.
    Surface,
    /// Inside the closed meshes.
    Volume,
    /// At the scene's tracker points.
    Tracker,
}

/// What else a point's chance of staying is multiplied by: a mask image
/// (sampled at the point's uv, or where a camera sees it) and its channel.
struct Mask<'a> {
    image: &'a Image,
    camera: Option<&'a geom::Camera>,
    luminance: bool,
}

impl Mask<'_> {
    /// The mask at frame pixel position `x`, `y` (0 off its data window).
    fn at(&self, x: f64, y: f64) -> f64 {
        let img = self.image;
        let (ix, iy) = (x.floor() as i64 - img.x0 as i64, y.floor() as i64 - img.y0 as i64);
        if ix < 0 || iy < 0 || ix >= img.width as i64 || iy >= img.height as i64 {
            return 0.0;
        }
        let p = img.pixels[iy as usize * img.width + ix as usize];
        let v = if self.luminance { 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2] } else { p[3] };
        v as f64
    }

    /// The mask for a point at `p` (scene space) with texture coordinates
    /// `uv` (if its surface has them): through the camera when there is
    /// one, else at the uv (0 without).
    fn factor(&self, p: &Vec3, uv: Option<[f64; 2]>) -> f64 {
        let f = self.image.format;
        match self.camera {
            Some(cam) => cam.project(p, f).map_or(0.0, |(q, _)| self.at(q[0], q[1])),
            None => uv.map_or(0.0, |[u, v]| self.at(u * f.width as f64, (1.0 - v) * f.height as f64)),
        }
    }
}

/// A surface to scatter on: a mesh (a card as two triangles) and where it
/// is, with its corners placed in the scene.
struct Surf {
    mesh: Arc<Mesh>,
    placed: Vec<Vec3>,
    normals: Vec<Vec3>,
}

impl Surf {
    fn new(mesh: Arc<Mesh>, world: &Mat4) -> Surf {
        let placed = mesh.positions.iter().map(|p| geom::transform_point(world, p)).collect();
        let linear = world.fixed_view::<3, 3>(0, 0).into_owned();
        let normal = linear.try_inverse().map(|i| i.transpose()).unwrap_or(linear);
        let normals = mesh.normals.iter().map(|n| (normal * n).try_normalize(1e-300).unwrap_or(*n)).collect();
        Surf { mesh, placed, normals }
    }

    fn corners(&self, t: usize) -> [Vec3; 3] {
        self.mesh.triangles[t].map(|i| self.placed[i as usize])
    }

    fn area(&self, t: usize) -> f64 {
        let [a, b, c] = self.corners(t);
        (b - a).cross(&(c - a)).norm() / 2.0
    }

    /// Where barycentrics `w` on triangle `t` are, its normal there, and
    /// its texture coordinates (if it has them).
    fn at(&self, t: usize, w: [f64; 3]) -> (Vec3, Vec3, Option<[f64; 2]>) {
        let tri = self.mesh.triangles[t].map(|i| i as usize);
        let [a, b, c] = self.corners(t);
        let p = a * w[0] + b * w[1] + c * w[2];
        let n = if self.normals.len() == self.placed.len() { self.normals[tri[0]] * w[0] + self.normals[tri[1]] * w[1] + self.normals[tri[2]] * w[2] } else { (b - a).cross(&(c - a)) };
        let uvs = &self.mesh.uvs;
        let uv = (uvs.len() == self.placed.len()).then(|| [0, 1].map(|k| uvs[tri[0]][k] * w[0] + uvs[tri[1]][k] * w[1] + uvs[tri[2]][k] * w[2]));
        (p, n.try_normalize(1e-300).unwrap_or_else(Vec3::y), uv)
    }
}

/// A point Scatter may make: on surface `surf`'s triangle `tri` at
/// barycentrics `w` (a tracker point's or a volume's: `tri` its number, no
/// surface), where it is and its normal, and its stable numbers: `id`
/// and `keep` (0..1, against its chance of staying).
#[derive(Clone)]
struct Candidate {
    surf: usize,
    tri: usize,
    w: [f64; 3],
    p: Vec3,
    n: Option<Vec3>,
    uv: Option<[f64; 2]>,
    id: u64,
    keep: f64,
}

/// How many of a place's candidates to make where `expected` are wanted
/// on average: the whole part, and one more as likely as the rest (by the
/// hash `u`).
fn how_many(expected: f64, u: f64) -> usize {
    expected.floor() as usize + usize::from(u < expected.fract())
}

/// A Scatter: the input scene with one more point set (see the node's
/// help in ops.rs).
pub fn scatter(params: &Value, mut scene: Scene, image: Option<&Image>, camera: Option<&Scene>) -> Result<Scene> {
    let source = match params["source"].as_str() {
        Some("volume") => Source::Volume,
        Some("tracker points") => Source::Tracker,
        _ => Source::Surface,
    };
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let density = params["density"].as_f64().unwrap_or(10.0).max(0.0);
    let seed = params["seed"].as_i64().unwrap_or(0) as u64;
    let attr = params["density_attr"].as_str().unwrap_or("").trim().to_string();
    let min_distance = params["min_distance"].as_f64().unwrap_or(0.0).max(0.0);
    let by_camera = params["image_via"].as_str() == Some("camera");
    let cam = camera.and_then(|s| s.cameras.first());
    if image.is_some() && by_camera && cam.is_none() {
        bail!("Scatter: the density image is seen through a camera, but nothing with one is connected to its camera input");
    }
    let mask = image.map(|image| Mask { image, camera: if by_camera { cam } else { None }, luminance: params["channel"].as_str() == Some("luminance") });
    // The surfaces (meshes, then cards as two triangles each).
    let mut surfs: Vec<Surf> = scene.meshes.iter().filter(|m| take(&m.node)).map(|m| Surf::new(m.mesh.clone(), &m.world)).collect();
    if source == Source::Surface {
        for c in scene.cards.iter().filter(|c| take(&c.node)) {
            let [w, h] = c.size.map(|v| v / 2.0);
            let mesh = Mesh {
                positions: vec![Vec3::new(-w, h, 0.0), Vec3::new(w, h, 0.0), Vec3::new(w, -h, 0.0), Vec3::new(-w, -h, 0.0)],
                normals: vec![Vec3::z(); 4],
                uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                triangles: vec![[0, 3, 2], [0, 2, 1]],
                ..Mesh::default()
            };
            surfs.push(Surf::new(Arc::new(mesh), &c.world));
        }
    }
    let candidates = match source {
        Source::Surface => on_surfaces(&surfs, density, seed)?,
        Source::Volume => in_volumes(&surfs, density, seed)?,
        Source::Tracker => (0..scene.points.len())
            .filter(|&i| take(scene.point_node(i)))
            .filter(|&i| stable_unit(&[seed, 3, i as u64]) < density)
            .map(|i| Candidate { surf: 0, tri: i, w: [1.0, 0.0, 0.0], p: scene.points[i], n: None, uv: None, id: i as u64, keep: stable_unit(&[seed, 4, i as u64]) })
            .collect(),
    };
    // Each stays as likely as its density attribute and mask say.
    let kept: Vec<Candidate> = candidates
        .into_par_iter()
        .filter(|c| {
            let mut chance = 1.0;
            if !attr.is_empty() {
                chance *= match surfs.get(c.surf).filter(|_| source == Source::Surface) {
                    Some(s) => match s.mesh.point_attrs.float(&attr) {
                        Some(v) => {
                            let tri = s.mesh.triangles[c.tri].map(|i| v[i as usize] as f64);
                            tri[0] * c.w[0] + tri[1] * c.w[1] + tri[2] * c.w[2]
                        }
                        None => 0.0,
                    },
                    None => 0.0,
                };
            }
            if let Some(m) = &mask {
                chance *= m.factor(&c.p, c.uv);
            }
            c.keep < chance.clamp(0.0, 1.0)
        })
        .collect();
    let kept = if min_distance > 0.0 { spaced(kept, min_distance) } else { kept };
    let mut set = PointSet { positions: kept.iter().map(|c| c.p).collect(), ..PointSet::default() };
    let n = kept.len();
    // Attributes: each surface's for its points, then together.
    if source == Source::Surface {
        let mut parts: Vec<(Attrs, usize)> = Vec::new();
        let mut start = 0;
        while start < n {
            let s = kept[start].surf;
            let end = start + kept[start..].iter().take_while(|c| c.surf == s).count();
            parts.push((interpolated(&surfs[s].mesh.point_attrs, &surfs[s].mesh, &kept[start..end]), end - start));
            start = end;
        }
        set.attrs = Attrs::concat(&parts.iter().map(|(a, n)| (a, *n)).collect::<Vec<_>>());
        if kept.iter().any(|c| c.uv.is_some()) {
            let uv = kept.iter().map(|c| c.uv.map_or([0.0; 3], |[u, v]| [u as f32, v as f32, 0.0])).collect();
            set.attrs.set("uv", Attr::Vec3(uv), n)?;
        }
    }
    if kept.iter().all(|c| c.n.is_some()) && n > 0 {
        set.attrs.set(geo::N, Attr::Vec3(kept.iter().map(|c| c.n.map_or([0.0; 3], |v| [v.x as f32, v.y as f32, v.z as f32])).collect()), n)?;
    }
    set.attrs.set(geo::ID, Attr::Int(unique_ids(kept.iter().map(|c| c.id))), n)?;
    scene.point_sets.push(set);
    Ok(scene)
}

/// Candidates on `surfs`' triangles, `density` a unit of area: each
/// triangle its own share, each candidate at a place hashed from the
/// seed, its surface, triangle and number (so more density adds
/// candidates and keeps the rest).
fn on_surfaces(surfs: &[Surf], density: f64, seed: u64) -> Result<Vec<Candidate>> {
    let counts: Vec<Vec<usize>> = surfs.iter().enumerate().map(|(s, surf)| (0..surf.mesh.triangles.len()).into_par_iter().map(|t| how_many(surf.area(t) * density, stable_unit(&[seed, 0, s as u64, t as u64]))).collect()).collect();
    let total: usize = counts.iter().flatten().sum();
    if total > MAX_POINTS {
        bail!("Scatter would make {total} points (more than {MAX_POINTS}): lower the density");
    }
    let mut out = Vec::with_capacity(total);
    for (s, surf) in surfs.iter().enumerate() {
        let per: Vec<Vec<Candidate>> = counts[s]
            .par_iter()
            .enumerate()
            .map(|(t, &k)| {
                (0..k)
                    .map(|k| {
                        let h = |j: u64| stable_unit(&[seed, 1, s as u64, t as u64, k as u64, j]);
                        // Even over the triangle: the unit square folded in half.
                        let (mut u, mut v) = (h(0), h(1));
                        if u + v > 1.0 {
                            (u, v) = (1.0 - u, 1.0 - v);
                        }
                        let w = [1.0 - u - v, u, v];
                        let (p, n, uv) = surf.at(t, w);
                        Candidate { surf: s, tri: t, w, p, n: Some(n), uv, id: stable_hash(&[seed, 2, s as u64, t as u64, k as u64]), keep: h(2) }
                    })
                    .collect()
            })
            .collect();
        out.extend(per.into_iter().flatten());
    }
    Ok(out)
}

/// Candidates inside `surfs` (closed meshes), `density` a unit of
/// volume: in the cells of a grid in the scene (cells a power of two
/// across, from the meshes' size, so a small edit keeps the grid), each
/// cell its own share at hashed places, kept where inside a mesh.
fn in_volumes(surfs: &[Surf], density: f64, seed: u64) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for (s, surf) in surfs.iter().enumerate() {
        let Some((lo, hi)) = bounds(&surf.placed) else { continue };
        let size = (hi - lo).norm().max(1e-9);
        let cell = 2f64.powf((size / 32.0).log2().round());
        let lo_cell = lo.map(|v| (v / cell).floor() as i64);
        let hi_cell = hi.map(|v| (v / cell).floor() as i64);
        let cells: Vec<[i64; 3]> = (lo_cell.x..=hi_cell.x).flat_map(|x| (lo_cell.y..=hi_cell.y).flat_map(move |y| (lo_cell.z..=hi_cell.z).map(move |z| [x, y, z]))).collect();
        let key = |c: &[i64; 3]| c.map(|v| v as u64);
        let counts: Vec<usize> = cells.par_iter().map(|c| how_many(cell.powi(3) * density, stable_unit(&[seed, 5, s as u64, key(c)[0], key(c)[1], key(c)[2]]))).collect();
        let total: usize = counts.iter().sum::<usize>() + out.len();
        if total > MAX_POINTS {
            bail!("Scatter would try {total} points (more than {MAX_POINTS}): lower the density");
        }
        let inside = Inside::new(surf);
        let per: Vec<Vec<Candidate>> = cells
            .par_iter()
            .zip(&counts)
            .map(|(c, &k)| {
                let [cx, cy, cz] = key(c);
                (0..k)
                    .filter_map(|k| {
                        let h = |j: u64| stable_unit(&[seed, 6, s as u64, cx, cy, cz, k as u64, j]);
                        let p = Vec3::new(c[0] as f64 + h(0), c[1] as f64 + h(1), c[2] as f64 + h(2)) * cell;
                        inside.contains(&p).then(|| Candidate { surf: s, tri: 0, w: [1.0, 0.0, 0.0], p, n: None, uv: None, id: stable_hash(&[seed, 7, s as u64, cx, cy, cz, k as u64]), keep: h(3) })
                    })
                    .collect()
            })
            .collect();
        out.extend(per.into_iter().flatten());
    }
    Ok(out)
}

fn bounds(points: &[Vec3]) -> Option<(Vec3, Vec3)> {
    let first = *points.first()?;
    Some(points.iter().fold((first, first), |(lo, hi), p| (lo.inf(p), hi.sup(p))))
}

/// Whether points are inside a closed mesh: a ray along +X crosses its
/// triangles an odd number of times. Triangles binned by where they lie
/// across Y and Z, so each ray tries only those it could cross.
struct Inside<'a> {
    surf: &'a Surf,
    lo: [f64; 2],
    step: [f64; 2],
    bins: Vec<Vec<u32>>,
}

const INSIDE_BINS: usize = 64;

impl<'a> Inside<'a> {
    fn new(surf: &'a Surf) -> Inside<'a> {
        let (lo, hi) = bounds(&surf.placed).unwrap_or((Vec3::zeros(), Vec3::zeros()));
        let step = [((hi.y - lo.y) / INSIDE_BINS as f64).max(1e-12), ((hi.z - lo.z) / INSIDE_BINS as f64).max(1e-12)];
        let mut bins = vec![Vec::new(); INSIDE_BINS * INSIDE_BINS];
        let bin = |v: f64, lo: f64, step: f64| (((v - lo) / step).floor().max(0.0) as usize).min(INSIDE_BINS - 1);
        for t in 0..surf.mesh.triangles.len() {
            let c = surf.corners(t);
            let (y0, y1) = (c.iter().map(|p| p.y).fold(f64::MAX, f64::min), c.iter().map(|p| p.y).fold(f64::MIN, f64::max));
            let (z0, z1) = (c.iter().map(|p| p.z).fold(f64::MAX, f64::min), c.iter().map(|p| p.z).fold(f64::MIN, f64::max));
            for by in bin(y0, lo.y, step[0])..=bin(y1, lo.y, step[0]) {
                for bz in bin(z0, lo.z, step[1])..=bin(z1, lo.z, step[1]) {
                    bins[by * INSIDE_BINS + bz].push(t as u32);
                }
            }
        }
        Inside { surf, lo: [lo.y, lo.z], step, bins }
    }

    fn contains(&self, p: &Vec3) -> bool {
        let by = (p.y - self.lo[0]) / self.step[0];
        let bz = (p.z - self.lo[1]) / self.step[1];
        if by < 0.0 || bz < 0.0 || by >= INSIDE_BINS as f64 || bz >= INSIDE_BINS as f64 {
            return false;
        }
        let tris = &self.bins[by as usize * INSIDE_BINS + bz as usize];
        let crossings = tris.iter().filter(|&&t| crosses(&self.surf.corners(t as usize), p)).count();
        crossings % 2 == 1
    }
}

/// Whether the ray from `p` along +X crosses triangle `c` (its YZ shadow
/// holding p's, by a half-open rule so a shared edge counts once).
fn crosses(c: &[Vec3; 3], p: &Vec3) -> bool {
    let mut inside = false;
    for i in 0..3 {
        let (a, b) = (c[i], c[(i + 1) % 3]);
        if (a.y > p.y) != (b.y > p.y) {
            let z = a.z + (p.y - a.y) / (b.y - a.y) * (b.z - a.z);
            if p.z < z {
                inside = !inside;
            }
        }
    }
    if !inside {
        return false;
    }
    // Where the triangle's plane is at p's Y and Z: ahead of p?
    let n = (c[1] - c[0]).cross(&(c[2] - c[0]));
    if n.x.abs() < 1e-300 {
        return false;
    }
    let x = c[0].x - (n.y * (p.y - c[0].y) + n.z * (p.z - c[0].z)) / n.x;
    x > p.x
}

/// `points` with each closer than `r` to one kept before it left out,
/// trying them in a stable hashed order (their `keep` numbers); the rest
/// in their own order.
fn spaced(points: Vec<Candidate>, r: f64) -> Vec<Candidate> {
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&a, &b| points[a].keep.total_cmp(&points[b].keep).then(a.cmp(&b)));
    let cell = |p: &Vec3| p.map(|v| (v / r).floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    let mut keep = vec![false; points.len()];
    for i in order {
        let c = cell(&points[i].p);
        let near = (-1..=1).any(|dx| {
            (-1..=1).any(|dy| {
                (-1..=1).any(|dz| grid.get(&[c.x + dx, c.y + dy, c.z + dz]).is_some_and(|v| v.iter().any(|&j| (points[j].p - points[i].p).norm() < r)))
            })
        });
        if !near {
            grid.entry([c.x, c.y, c.z]).or_default().push(i);
            keep[i] = true;
        }
    }
    points.into_iter().zip(keep).filter(|(_, k)| *k).map(|(c, _)| c).collect()
}

/// `ids` as distinct numbers: each its own low 31 bits, or, where an
/// earlier one has them, hashed again until free.
fn unique_ids(ids: impl Iterator<Item = u64>) -> Vec<i32> {
    let mut seen = HashSet::new();
    ids.map(|mut h| loop {
        let id = (h & 0x7fff_ffff) as i32;
        if seen.insert(id) {
            break id;
        }
        h = geo::mix(h);
    })
    .collect()
}

/// `attrs` (a mesh's per point) at `points` on its triangles: numbers and
/// vectors blended by the barycentrics, whole numbers the nearest
/// corner's.
fn interpolated(attrs: &Attrs, mesh: &Mesh, points: &[Candidate]) -> Attrs {
    let mut out = Attrs::default();
    for (name, a) in attrs.iter() {
        let corners = |c: &Candidate| mesh.triangles[c.tri].map(|i| i as usize);
        let blend = |c: &Candidate, f: &dyn Fn(usize) -> f64| {
            let k = corners(c);
            (f(k[0]) * c.w[0] + f(k[1]) * c.w[1] + f(k[2]) * c.w[2]) as f32
        };
        let value = match a {
            Attr::Float(v) => Attr::Float(points.iter().map(|c| blend(c, &|i| v[i] as f64)).collect()),
            Attr::Vec3(v) => Attr::Vec3(points.iter().map(|c| std::array::from_fn(|j| blend(c, &|i| v[i][j] as f64))).collect()),
            Attr::Vec4(v) => Attr::Vec4(points.iter().map(|c| std::array::from_fn(|j| blend(c, &|i| v[i][j] as f64))).collect()),
            Attr::Int(v) => Attr::Int(
                points
                    .iter()
                    .map(|c| {
                        let k = corners(c);
                        let best = (0..3).max_by(|&a, &b| c.w[a].total_cmp(&c.w[b])).unwrap_or(0);
                        v[k[best]]
                    })
                    .collect(),
            ),
        };
        let _ = out.set(name, value, points.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Card, Model};
    use serde_json::json;

    fn ground(size: f64) -> Scene {
        // Lying flat, facing up.
        let flat = geom::trs([0.0; 3], [-90.0, 0.0, 0.0], [1.0; 3]);
        let model = Model { node: "ground".into(), ..Model::new(Arc::new(crate::mesh::plane([size, size], [4, 4])), flat, crate::mesh::Material::default()) };
        Scene { meshes: vec![model], ..Scene::default() }
    }

    fn run(p: Value, scene: Scene) -> PointSet {
        scatter(&p, scene, None, None).unwrap().point_sets.pop().unwrap()
    }

    #[test]
    fn scatter_spreads_by_area_and_keeps_ids_as_density_grows() {
        // A 10 × 10 plane at 10 a unit area: about 1000.
        let a = run(json!({ "density": 10.0, "seed": 1 }), ground(10.0));
        assert!((900..1100).contains(&a.len()), "{}", a.len());
        assert!(a.validate().is_ok());
        let ids = a.attrs.int(geo::ID).unwrap();
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len(), "distinct ids");
        // On the plane, facing up.
        assert!(a.positions.iter().all(|p| p.y.abs() < 1e-9 && p.x.abs() <= 5.0 && p.z.abs() <= 5.0));
        assert!(a.attrs.vec3(geo::N).unwrap().iter().all(|n| (n[1] - 1.0).abs() < 1e-6));
        assert!(a.attrs.vec3("uv").is_some());
        // A tenth more: the points there were stay, with their ids.
        let b = run(json!({ "density": 11.0, "seed": 1 }), ground(10.0));
        let old: HashMap<i32, Vec3> = ids.iter().copied().zip(a.positions.iter().copied()).collect();
        let kept = b.attrs.int(geo::ID).unwrap().iter().zip(&b.positions).filter(|(id, p)| old.get(id).is_some_and(|q| (*q - **p).norm() < 1e-12)).count();
        assert!(kept as f64 >= 0.9 * a.len() as f64, "{kept} of {} kept", a.len());
        // Another seed: other points.
        let c = run(json!({ "density": 10.0, "seed": 2 }), ground(10.0));
        assert!(c.positions.iter().zip(&a.positions).filter(|(p, q)| (*p - *q).norm() < 1e-9).count() < 10);
        // Too many: an error, not a hang.
        assert!(scatter(&json!({ "density": 1e6 }), ground(10.0), None, None).is_err());
    }

    #[test]
    fn scatter_thins_by_a_mask_image_and_an_attribute() {
        use crate::image::{IRect, Size};
        // A mask white on its left half, black on its right.
        let size = Size { width: 64, height: 64 };
        let mut img = Image::blank(size, IRect::new(0, 0, 64, 64));
        for y in 0..64 {
            for x in 0..32 {
                img.pixels[y * 64 + x] = [1.0; 4];
            }
        }
        // Through the uvs: the plane's u runs along x.
        let pts = scatter(&json!({ "density": 10.0 }), ground(10.0), Some(&img), None).unwrap().point_sets.pop().unwrap();
        assert!(pts.len() > 300 && pts.positions.iter().all(|p| p.x <= 0.0 + 1e-9), "{}: only on the left", pts.len());
        // Through a camera looking down: its left half of the frame.
        let cam = crate::geom::Camera { world: geom::trs([0.0, 20.0, 0.0], [-90.0, 0.0, 0.0], [1.0; 3]), ..crate::geom::Camera::default() };
        let viewer = Scene { cameras: vec![cam], ..Scene::default() };
        let pts = scatter(&json!({ "density": 10.0, "image_via": "camera" }), ground(10.0), Some(&img), Some(&viewer)).unwrap().point_sets.pop().unwrap();
        assert!(pts.len() > 100 && pts.positions.iter().all(|p| p.x <= 1e-9), "{}: the camera's left", pts.len());
        assert!(scatter(&json!({ "image_via": "camera" }), ground(10.0), Some(&img), None).is_err(), "no camera to see through");
        // A density attribute: 1 at x < 0 corners, 0 elsewhere.
        let mut s = ground(10.0);
        let mesh = Arc::make_mut(&mut s.meshes[0].mesh);
        let w: Vec<f32> = mesh.positions.iter().map(|p| if p.x < 0.0 { 1.0 } else { 0.0 }).collect();
        let n = w.len();
        mesh.point_attrs.set("dens", Attr::Float(w), n).unwrap();
        let pts = run(json!({ "density": 10.0, "density_attr": "dens" }), s.clone());
        assert!(pts.positions.iter().all(|p| p.x < 2.5 + 1e-9) && pts.attrs.float("dens").is_some(), "the attribute thins and comes along");
        assert!(run(json!({ "density": 10.0, "density_attr": "missing" }), s).is_empty());
    }

    #[test]
    fn scatter_spaces_points_fills_volumes_and_takes_tracker_points() {
        let pts = run(json!({ "density": 50.0, "min_distance": 0.5 }), ground(10.0));
        assert!(pts.len() > 50);
        for (i, p) in pts.positions.iter().enumerate() {
            assert!(pts.positions[i + 1..].iter().all(|q| (p - q).norm() >= 0.5), "too close");
        }
        // Inside a 2-unit cube at 10 a unit volume: about 80.
        let cube = Model { node: "box".into(), ..Model::new(Arc::new(crate::mesh::cube([2.0; 3], [1; 3])), Mat4::identity(), crate::mesh::Material::default()) };
        let vol = run(json!({ "density": 10.0, "source": "volume" }), Scene { meshes: vec![cube], ..Scene::default() });
        assert!((50..110).contains(&vol.len()), "{}", vol.len());
        assert!(vol.positions.iter().all(|p| p.abs().max() <= 1.0));
        // Tracker points: a fraction of them, ids their numbers.
        let cloud = Scene { points: (0..1000).map(|i| Vec3::new(i as f64, 0.0, 0.0)).collect(), point_nodes: vec!["solve".into(); 1000], ..Scene::default() };
        let half = run(json!({ "density": 0.5, "source": "tracker points" }), cloud.clone());
        assert!((400..600).contains(&half.len()), "{}", half.len());
        assert!(half.attrs.int(geo::ID).unwrap().iter().zip(&half.positions).all(|(id, p)| *id as f64 == p.x));
        assert_eq!(run(json!({ "density": 1.0, "source": "tracker points", "group": "other" }), cloud).len(), 0);
        // A card's surface too, its image's uvs.
        let card = Card { node: "c".into(), ..Card::new(Arc::new(Image::new(4, 4)), Mat4::identity(), [2.0, 2.0]) };
        let on = run(json!({ "density": 25.0 }), Scene { cards: vec![card], ..Scene::default() });
        assert!((70..130).contains(&on.len()) && on.positions.iter().all(|p| p.z.abs() < 1e-9));
    }
}
