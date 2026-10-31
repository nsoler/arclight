//! Geometry nodes' work (docs/geo-plan.md, P2 on): each takes a scene and
//! gives one back, acting on its meshes, point sets, curves and instances
//! and passing the rest through. `ops.rs` registers them.
//!
//! Random choices are stable (D5): each comes from a hash of the seed and
//! where it's made (a triangle and a candidate's number on it, a cell),
//! never from a running generator, so an edit (more density, a painted
//! mask) keeps most points where they were, with their ids.

use crate::geo::{self, stable_hash, stable_unit, Attr, Attrs, PointSet};
use crate::expr::{Elem, Val};
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

/// Where a Delete deletes: inside or outside a region (scene space taken
/// into the unit cube or ball), or where a formula is true.
enum Doomed {
    Region { to_region: Mat4, sphere: bool, inside: bool },
    Formula { expr: crate::expr::Expr, frame: f64, refs: serde_json::Map<String, Value> },
}

impl Doomed {
    /// Whether the element at `p` (`elem` its @ names) goes.
    fn judge(&self, p: &Vec3, elem: Elem) -> Result<bool, String> {
        match self {
            Doomed::Region { to_region, sphere, inside } => {
                let q = geom::transform_point(to_region, p);
                let within = if *sphere { q.norm_squared() <= 1.0 } else { q.x.abs() <= 1.0 && q.y.abs() <= 1.0 && q.z.abs() <= 1.0 };
                Ok(within == *inside)
            }
            Doomed::Formula { expr, frame, refs } => match expr.eval_elem(crate::expr::Clock::default(), *frame, elem, refs)? {
                Val::Num(x) => Ok(x != 0.0),
                Val::List(v) => Err(format!("the expression gives a list of {} (a condition is one number)", v.len())),
            },
        }
    }

    /// Which of `count` elements go: element i at `at(i)`, with
    /// attributes `attrs`, and `extra` @ names of its own (by name and i).
    fn which(&self, count: usize, at: &(dyn Fn(usize) -> Vec3 + Sync), attrs: &Attrs, extra: &(dyn Fn(&str, usize) -> Option<Val> + Sync)) -> Result<Vec<bool>> {
        (0..count)
            .into_par_iter()
            .map(|i| {
                let p = at(i);
                let elem = |name: &str| match name {
                    "P" => Some(Val::List(vec![p.x, p.y, p.z])),
                    "index" => Some(Val::Num(i as f64)),
                    "count" => Some(Val::Num(count as f64)),
                    _ => attr_value(attrs, name, i).or_else(|| extra(name, i)),
                };
                self.judge(&p, &elem)
            })
            .collect::<Result<Vec<bool>, String>>()
            .map_err(|e| anyhow::anyhow!("Delete: {e}"))
    }
}

/// The other nodes' params a geometry node's formulas read, as the graph
/// worked them out for this cook (see `ops::resolve_params_linked`).
fn refs_of(params: &Value) -> serde_json::Map<String, Value> {
    params["_refs"].as_object().cloned().unwrap_or_default()
}

/// Attribute `name`'s value at element `i`, as a formula reads it.
fn attr_value(attrs: &Attrs, name: &str, i: usize) -> Option<Val> {
    Some(match attrs.get(name)? {
        Attr::Float(v) => Val::Num(v[i] as f64),
        Attr::Int(v) => Val::Num(v[i] as f64),
        Attr::Vec3(v) => Val::List(v[i].iter().map(|&x| x as f64).collect()),
        Attr::Vec4(v) => Val::List(v[i].iter().map(|&x| x as f64).collect()),
    })
}

/// The indices of `gone`'s false entries: what stays.
fn staying(gone: &[bool]) -> Vec<u32> {
    gone.iter().enumerate().filter(|(_, g)| !**g).map(|(i, _)| i as u32).collect()
}

/// A Delete: `scene` without the points, faces or instances (of its
/// group) inside or outside its region, or where its formula is true;
/// the region kept for the 3D view to show.
pub fn delete(params: &Value, mut scene: Scene, frame: f64) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let doomed = match params["mode"].as_str() {
        Some("expression") => {
            let src = params["expression"].as_str().unwrap_or("").trim();
            if src.is_empty() {
                return Ok(scene);
            }
            Doomed::Formula { expr: crate::expr::parse(src).map_err(|e| anyhow::anyhow!("Delete: {e}"))?, frame, refs: refs_of(params) }
        }
        mode => {
            let size = crate::scene::numbers::<3>(&params["size"], 1.0).map(|v| (v.abs() / 2.0).max(1e-12));
            let region = geom::trs(crate::scene::numbers(&params["center"], 0.0), crate::scene::numbers(&params["rotate"], 0.0), size);
            let sphere = params["shape"].as_str() == Some("sphere");
            scene.regions.push(crate::scene::Region { node: String::new(), world: region, sphere });
            Doomed::Region { to_region: region.try_inverse().unwrap_or_else(Mat4::identity), sphere, inside: mode != Some("outside") }
        }
    };
    let none = |_: &str, _: usize| None;
    match params["elements"].as_str() {
        Some("faces") => {
            for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
                let s = Surf::new(m.mesh.clone(), &m.world);
                let center = |t: usize| s.corners(t).iter().sum::<Vec3>() / 3.0;
                let normal = |name: &str, t: usize| (name == geo::N).then(|| {
                    let [a, b, c] = s.corners(t);
                    let n = (b - a).cross(&(c - a)).try_normalize(1e-300).unwrap_or_else(Vec3::y);
                    Val::List(vec![n.x, n.y, n.z])
                });
                let gone = doomed.which(s.mesh.triangles.len(), &center, &s.mesh.face_attrs, &normal)?;
                if gone.contains(&true) {
                    m.mesh = Arc::new(without_faces(&s.mesh, &staying(&gone)));
                }
            }
            scene.meshes.retain(|m| !take(&m.node) || !m.mesh.triangles.is_empty());
        }
        Some("instances") => {
            for g in scene.instances.iter_mut().filter(|g| take(&g.node)) {
                let gone = doomed.which(g.len(), &|i| g.center(i), &g.attrs, &none)?;
                let keep = staying(&gone);
                g.worlds = keep.iter().map(|&i| g.worlds[i as usize]).collect();
                g.which = keep.iter().map(|&i| g.which[i as usize]).collect();
                g.attrs = g.attrs.gathered(&keep);
            }
        }
        _ => {
            for g in scene.point_sets.iter_mut().filter(|g| take(&g.node)) {
                let gone = doomed.which(g.len(), &|i| g.placed(i), &g.attrs, &none)?;
                let keep = staying(&gone);
                g.positions = keep.iter().map(|&i| g.positions[i as usize]).collect();
                g.attrs = g.attrs.gathered(&keep);
            }
            for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
                let gone = doomed.which(c.positions.len(), &|i| geom::transform_point(&c.world, &c.positions[i]), &c.attrs, &none)?;
                // Each curve's points that stay; one left with fewer than
                // two goes.
                let mut out = geo::Curves { world: c.world, node: c.node.clone(), ..geo::Curves::default() };
                let mut kept = Vec::new();
                for k in 0..c.len() {
                    let pts: Vec<u32> = (c.starts[k]..c.starts[k + 1]).filter(|&i| !gone[i as usize]).collect();
                    if pts.len() >= 2 {
                        out.push(&pts.iter().map(|&i| c.positions[i as usize]).collect::<Vec<_>>(), c.closed[k]);
                        kept.extend(pts);
                    }
                }
                out.attrs = c.attrs.gathered(&kept);
                *c = out;
            }
        }
    }
    Ok(scene)
}

/// `mesh` with only the triangles `keep`, and only the vertices they use
/// (their attributes with them).
fn without_faces(mesh: &Mesh, keep: &[u32]) -> Mesh {
    let mut index = vec![u32::MAX; mesh.positions.len()];
    let mut used = Vec::new();
    let triangles = keep
        .iter()
        .map(|&t| {
            mesh.triangles[t as usize].map(|v| {
                if index[v as usize] == u32::MAX {
                    index[v as usize] = used.len() as u32;
                    used.push(v);
                }
                index[v as usize]
            })
        })
        .collect();
    let pick = |n: usize| n == mesh.positions.len();
    Mesh {
        positions: used.iter().map(|&v| mesh.positions[v as usize]).collect(),
        normals: if pick(mesh.normals.len()) { used.iter().map(|&v| mesh.normals[v as usize]).collect() } else { Vec::new() },
        uvs: if pick(mesh.uvs.len()) { used.iter().map(|&v| mesh.uvs[v as usize]).collect() } else { Vec::new() },
        triangles,
        point_attrs: mesh.point_attrs.gathered(&used),
        face_attrs: mesh.face_attrs.gathered(keep),
    }
}

/// The kind of value an Attribute Set row makes.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Float,
    Vec3,
    Vec4,
    Int,
}

/// One row of an Attribute Set: the attribute it sets, its kind, and the
/// formula worked out per element.
struct Row {
    name: String,
    kind: Kind,
    expr: crate::expr::Expr,
}

/// An Attribute Set's rows (those with a name), in order.
fn rows(params: &Value) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for k in 1..=4 {
        let name = params[format!("attr{k}_name")].as_str().unwrap_or("").trim().to_string();
        if name.is_empty() {
            continue;
        }
        let kind = match params[format!("attr{k}_kind")].as_str() {
            Some("vec3") => Kind::Vec3,
            Some("vec4") => Kind::Vec4,
            Some("int") => Kind::Int,
            _ => Kind::Float,
        };
        if (name == "P" || name == geo::N) && kind != Kind::Vec3 {
            bail!("Attribute Set, row {k}: {name} is a vec3");
        }
        let src = params[format!("attr{k}_expr")].as_str().unwrap_or("").trim();
        let expr = crate::expr::parse(src).map_err(|e| anyhow::anyhow!("Attribute Set, row {k} ({name}): {e}"))?;
        out.push(Row { name, kind, expr });
    }
    Ok(out)
}

/// What a formula sees of a collection of elements: how many, where each
/// is (in the scene), its attributes, and @ names of its own (by name and
/// element).
struct Elements<'a> {
    count: usize,
    at: &'a (dyn Fn(usize) -> Vec3 + Sync),
    attrs: &'a Attrs,
    extra: &'a (dyn Fn(&str, usize) -> Option<Val> + Sync),
}

impl Row {
    /// Its value for each of `els`, as an attribute of its kind.
    fn values(&self, els: &Elements, refs: &serde_json::Map<String, Value>, frame: f64) -> Result<Attr> {
        let count = els.count;
        let vals: Vec<Val> = (0..count)
            .into_par_iter()
            .map(|i| {
                let p = (els.at)(i);
                let elem = |name: &str| match name {
                    "P" => Some(Val::List(vec![p.x, p.y, p.z])),
                    "index" => Some(Val::Num(i as f64)),
                    "count" => Some(Val::Num(count as f64)),
                    _ => attr_value(els.attrs, name, i).or_else(|| (els.extra)(name, i)),
                };
                self.expr.eval_elem(crate::expr::Clock::default(), frame, &elem, refs)
            })
            .collect::<Result<_, String>>()
            .map_err(|e| anyhow::anyhow!("Attribute Set ({}): {e}", self.name))?;
        let wrong = |v: &Val| anyhow::anyhow!("Attribute Set ({}): the formula gives {}", self.name, match v { Val::Num(_) => "a number".to_string(), Val::List(l) => format!("a list of {}", l.len()) });
        let num = |v: &Val| match v {
            Val::Num(x) => Ok(*x),
            _ => Err(wrong(v)),
        };
        fn many<const N: usize>(v: &Val, wrong: &dyn Fn(&Val) -> anyhow::Error) -> Result<[f32; N]> {
            match v {
                Val::Num(x) => Ok([*x as f32; N]),
                Val::List(l) if l.len() == N => Ok(std::array::from_fn(|i| l[i] as f32)),
                _ => Err(wrong(v)),
            }
        }
        Ok(match self.kind {
            Kind::Float => Attr::Float(vals.iter().map(|v| num(v).map(|x| x as f32)).collect::<Result<_>>()?),
            Kind::Int => Attr::Int(vals.iter().map(|v| num(v).map(|x| x.round() as i32)).collect::<Result<_>>()?),
            Kind::Vec3 => Attr::Vec3(vals.iter().map(|v| many::<3>(v, &wrong)).collect::<Result<_>>()?),
            Kind::Vec4 => Attr::Vec4(vals.iter().map(|v| many::<4>(v, &wrong)).collect::<Result<_>>()?),
        })
    }
}

fn vec3s(a: &Attr) -> Vec<Vec3> {
    match a {
        Attr::Vec3(v) => v.iter().map(|p| Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect(),
        _ => Vec::new(),
    }
}

/// An Attribute Set: each row's formula worked out for every element of
/// its kind (in its group), then set as that attribute, row after row
/// (a later row sees an earlier one's). @P moves the points (an
/// instance's placement), a mesh's @N sets its normals.
pub fn attribute_set(params: &Value, mut scene: Scene, frame: f64) -> Result<Scene> {
    let rows = rows(params)?;
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let refs = refs_of(params);
    let none = |_: &str, _: usize| None;
    let unplace = |world: &Mat4, ps: &[Vec3]| {
        let inv = world.try_inverse().unwrap_or_else(Mat4::identity);
        ps.iter().map(|p| geom::transform_point(&inv, p)).collect::<Vec<Vec3>>()
    };
    match params["elements"].as_str() {
        Some("mesh points") => {
            for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
                for row in &rows {
                    let s = Surf::new(m.mesh.clone(), &m.world);
                    let normal = |name: &str, i: usize| (name == geo::N).then(|| s.normals.get(i).map(|n| Val::List(vec![n.x, n.y, n.z]))).flatten();
                    let els = Elements { count: s.placed.len(), at: &|i| s.placed[i], attrs: &s.mesh.point_attrs, extra: &normal };
                    let a = row.values(&els, &refs, frame)?;
                    let n = s.placed.len();
                    let mesh = Arc::make_mut(&mut m.mesh);
                    match row.name.as_str() {
                        "P" => mesh.positions = unplace(&m.world, &vec3s(&a)),
                        "N" => {
                            // Back into the mesh's space: the placement's
                            // linear part, transposed.
                            let lt = m.world.fixed_view::<3, 3>(0, 0).transpose();
                            mesh.normals = vec3s(&a).iter().map(|n| (lt * n).try_normalize(1e-300).unwrap_or(*n)).collect();
                        }
                        name => mesh.point_attrs.set(name, a, n)?,
                    }
                }
            }
        }
        Some("faces") => {
            for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
                for row in &rows {
                    if row.name == "P" {
                        bail!("Attribute Set: faces can't be moved (@P): set mesh points instead");
                    }
                    let s = Surf::new(m.mesh.clone(), &m.world);
                    let center = |t: usize| s.corners(t).iter().sum::<Vec3>() / 3.0;
                    let normal = |name: &str, t: usize| (name == geo::N).then(|| {
                        let [a, b, c] = s.corners(t);
                        let n = (b - a).cross(&(c - a)).try_normalize(1e-300).unwrap_or_else(Vec3::y);
                        Val::List(vec![n.x, n.y, n.z])
                    });
                    let els = Elements { count: s.mesh.triangles.len(), at: &center, attrs: &s.mesh.face_attrs, extra: &normal };
                    let a = row.values(&els, &refs, frame)?;
                    let n = s.mesh.triangles.len();
                    Arc::make_mut(&mut m.mesh).face_attrs.set(&row.name, a, n)?;
                }
            }
        }
        Some("instances") => {
            for g in scene.instances.iter_mut().filter(|g| take(&g.node)) {
                for row in &rows {
                    let origins: Vec<Vec3> = g.worlds.iter().map(|w| Vec3::new(w[(0, 3)], w[(1, 3)], w[(2, 3)])).collect();
                    let els = Elements { count: g.len(), at: &|i| origins[i], attrs: &g.attrs, extra: &none };
                    let a = row.values(&els, &refs, frame)?;
                    if row.name == "P" {
                        for (w, p) in g.worlds.iter_mut().zip(vec3s(&a)) {
                            w.fixed_view_mut::<3, 1>(0, 3).copy_from(&p);
                        }
                    } else {
                        let n = g.len();
                        g.attrs.set(&row.name, a, n)?;
                    }
                }
            }
        }
        _ => {
            for g in scene.point_sets.iter_mut().filter(|g| take(&g.node)) {
                for row in &rows {
                    let els = Elements { count: g.len(), at: &|i| g.placed(i), attrs: &g.attrs, extra: &none };
                    let a = row.values(&els, &refs, frame)?;
                    match row.name.as_str() {
                        "P" => g.positions = unplace(&g.world, &vec3s(&a)),
                        name => {
                            let n = g.len();
                            g.attrs.set(name, a, n)?;
                        }
                    }
                }
            }
            for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
                for row in &rows {
                    let els = Elements { count: c.positions.len(), at: &|i| geom::transform_point(&c.world, &c.positions[i]), attrs: &c.attrs, extra: &none };
                    let a = row.values(&els, &refs, frame)?;
                    match row.name.as_str() {
                        "P" => c.positions = unplace(&c.world, &vec3s(&a)),
                        name => {
                            let n = c.positions.len();
                            c.attrs.set(name, a, n)?;
                        }
                    }
                }
            }
        }
    }
    Ok(scene)
}

/// The turn putting +Z along `z` with +Y toward `up` (as near as it can:
/// up made square to z); None when z is no direction.
fn aim(z: Vec3, up: Vec3) -> Option<nalgebra::Matrix3<f64>> {
    let z = z.try_normalize(1e-12)?;
    let x = up.cross(&z).try_normalize(1e-12).or_else(|| Vec3::x().cross(&z).try_normalize(1e-12)).or_else(|| Vec3::y().cross(&z).try_normalize(1e-12))?;
    let y = z.cross(&x);
    Some(nalgebra::Matrix3::from_columns(&[x, y, z]))
}

fn vec3_at(v: &[[f32; 3]], i: usize) -> Vec3 {
    Vec3::new(v[i][0] as f64, v[i][1] as f64, v[i][2] as f64)
}

/// An Instance on Points: a copy of one of `protos`' meshes, cards and
/// splat captures at each point of the scene's point sets (in Group),
/// picked, turned, sized and tinted by the point's attributes (see the
/// node's help in ops.rs); the points' attributes carried along.
pub fn instance_on_points(params: &Value, mut scene: Scene, protos: Option<&Scene>, camera: Option<&Scene>) -> Result<Scene> {
    let protos = protos.ok_or_else(|| anyhow::anyhow!("Instance on Points: connect what to copy (a scene with meshes, cards or splats) to its prototypes input"))?;
    let prototypes: Vec<geo::Prototype> = protos
        .meshes
        .iter()
        .cloned()
        .map(geo::Prototype::Model)
        .chain(protos.cards.iter().cloned().map(geo::Prototype::Card))
        .chain(protos.splats.iter().cloned().map(geo::Prototype::Splats))
        .collect();
    if prototypes.is_empty() {
        bail!("Instance on Points: its prototypes scene has no meshes, cards or splats to copy");
    }
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let seed = params["seed"].as_i64().unwrap_or(0) as u64;
    let pick = params["pick"].as_str().unwrap_or("random");
    let orient = params["orient"].as_str().unwrap_or("auto");
    let up_param = Vec3::from(crate::scene::numbers::<3>(&params["up"], 0.0)).try_normalize(1e-12).unwrap_or_else(Vec3::y);
    let scale = crate::scene::numbers::<3>(&params["scale"], 1.0);
    let eye = camera.and_then(|s| s.cameras.first()).map(|c| geom::transform_point(&c.world, &Vec3::zeros()));
    if orient == "face camera" && eye.is_none() {
        bail!("Instance on Points: Orient is 'face camera', but nothing with a camera is connected to its camera input");
    }
    let n = prototypes.len();
    let mut made = Vec::new();
    for g in scene.point_sets.iter().filter(|g| take(&g.node)) {
        let a = &g.attrs;
        let linear = g.world.fixed_view::<3, 3>(0, 0).into_owned();
        let normal = linear.try_inverse().map(|i| i.transpose()).unwrap_or(linear);
        let turn = nalgebra::Rotation3::from_matrix_eps(&linear, 1e-12, 100, nalgebra::Rotation3::identity());
        let (variant, ids) = (a.get(geo::VARIANT), a.int(geo::ID));
        let (normals, ups, orients) = (a.vec3(geo::N), a.vec3(geo::UP), a.vec4(geo::ORIENT));
        let (pscale, scales) = (a.float(geo::PSCALE), a.vec3(geo::SCALE));
        let placed: Vec<(u32, Mat4)> = (0..g.len())
            .into_par_iter()
            .map(|i| {
                let which = match pick {
                    "variant" => match variant {
                        Some(Attr::Int(v)) => v[i].rem_euclid(n as i32) as usize,
                        Some(Attr::Float(v)) => (v[i].floor() as i64).rem_euclid(n as i64) as usize,
                        _ => 0,
                    },
                    "in order" => i % n,
                    _ => {
                        let id = ids.map_or(i as u64, |v| v[i] as u32 as u64);
                        ((stable_unit(&[seed, 9, id]) * n as f64) as usize).min(n - 1)
                    }
                };
                let p = g.placed(i);
                let up = ups.map_or(up_param, |u| (linear * vec3_at(u, i)).try_normalize(1e-12).unwrap_or(up_param));
                let r = match orient {
                    "none" => None,
                    "face camera" => eye.and_then(|e| aim(e - p, Vec3::y())),
                    _ => match (orients, normals) {
                        (Some(q), _) => {
                            let q = nalgebra::UnitQuaternion::new_normalize(nalgebra::Quaternion::new(q[i][3] as f64, q[i][0] as f64, q[i][1] as f64, q[i][2] as f64));
                            Some(turn.matrix() * q.to_rotation_matrix().matrix())
                        }
                        (None, Some(nv)) => aim(normal * vec3_at(nv, i), up),
                        (None, None) => None,
                    },
                }
                .unwrap_or_else(nalgebra::Matrix3::identity);
                let k = pscale.map_or(1.0, |v| v[i] as f64);
                let s = scales.map_or(Vec3::new(1.0, 1.0, 1.0), |v| vec3_at(v, i));
                let size = nalgebra::Matrix3::from_diagonal(&Vec3::new(k * s.x * scale[0], k * s.y * scale[1], k * s.z * scale[2]));
                let mut w = Mat4::identity();
                w.fixed_view_mut::<3, 3>(0, 0).copy_from(&(r * size));
                w.fixed_view_mut::<3, 1>(0, 3).copy_from(&p);
                (which as u32, w)
            })
            .collect();
        made.push(geo::Instances { prototypes: prototypes.clone(), which: placed.iter().map(|p| p.0).collect(), worlds: placed.into_iter().map(|p| p.1).collect(), attrs: a.clone(), node: String::new(), ..geo::Instances::default() });
    }
    if !params["keep_points"].as_bool().unwrap_or(false) {
        scene.point_sets.retain(|g| !take(&g.node));
    }
    scene.instances.extend(made);
    Ok(scene)
}

/// Instance `i` of `g`'s tint: its Cd (linear RGBA), else none.
pub fn tint(g: &geo::Instances, i: usize) -> Option<[f32; 4]> {
    g.attrs.vec4(geo::CD).map(|c| c[i])
}

/// A Realize Instances: each instance (of Group's) as a mesh, card or
/// splat capture of its own (meshes and splats still shared), placed and
/// tinted as drawn.
pub fn realize_instances(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let (gone, kept): (Vec<geo::Instances>, Vec<geo::Instances>) = std::mem::take(&mut scene.instances).into_iter().partition(|g| take(&g.node));
    scene.instances = kept;
    for g in gone {
        for i in 0..g.len() {
            let w = g.worlds[i];
            match &g.prototypes[g.which[i] as usize] {
                geo::Prototype::Model(m) => {
                    let mut m = crate::scene::Model { world: w * m.world, node: g.node.clone(), ..m.clone() };
                    if let Some(c) = tint(&g, i) {
                        m.material.color = std::array::from_fn(|k| m.material.color[k] * c[k]);
                        m.material.opacity *= c[3];
                    }
                    scene.meshes.push(m);
                }
                geo::Prototype::Card(c) => {
                    let mut c = crate::scene::Card { world: w * c.world, node: g.node.clone(), ..c.clone() };
                    if let Some(t) = tint(&g, i) {
                        c.opacity *= t[3];
                        if let Some(m) = &mut c.material {
                            m.color = std::array::from_fn(|k| m.color[k] * t[k]);
                        }
                    }
                    scene.cards.push(c);
                }
                geo::Prototype::Splats(s) => scene.splats.push(Arc::new(crate::splat::Splats { world: w * s.world, node: g.node.clone(), ..(**s).clone() })),
            }
        }
    }
    Ok(scene)
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

    /// Delete takes points inside or outside a region, or where a formula
    /// is true; faces with their unused vertices; instances; short curves.
    #[test]
    fn delete_by_region_and_by_formula() {
        use crate::geo::{Curves, Instances};
        let pts = run(json!({ "density": 10.0, "seed": 3 }), ground(10.0));
        let n = pts.len();
        let mut scene = Scene { point_sets: vec![PointSet { node: "scatter".into(), ..pts }], ..Scene::default() };
        let del = |p: Value, s: &Scene| delete(&p, s.clone(), 1.0).unwrap();
        // A 4-wide box round the middle: those points go, and its region shows.
        let out = del(json!({ "center": [0, 0, 0], "size": [4, 4, 4] }), &scene);
        let ps = &out.point_sets[0];
        assert!(ps.positions.iter().all(|p| p.x.abs() > 2.0 || p.z.abs() > 2.0) && ps.len() < n && ps.validate().is_ok());
        assert_eq!(out.regions.len(), 1);
        let outside = del(json!({ "mode": "outside", "center": [0, 0, 0], "size": [4, 4, 4] }), &scene);
        assert_eq!(outside.point_sets[0].len() + ps.len(), n, "inside and outside split them");
        // A formula: those right of the middle, then a random third by id.
        let right = del(json!({ "mode": "expression", "expression": "@P.x > 0" }), &scene);
        assert!(right.point_sets[0].positions.iter().all(|p| p.x <= 0.0) && right.regions.is_empty());
        let third = del(json!({ "mode": "expression", "expression": "random(@id) < 0.3" }), &scene);
        let k = third.point_sets[0].len() as f64 / n as f64;
        assert!((0.6..0.8).contains(&k), "{k}");
        let again = del(json!({ "mode": "expression", "expression": "random(@id) < 0.3" }), &scene);
        assert_eq!(again.point_sets[0].attrs, third.point_sets[0].attrs, "the same every time");
        assert!(delete(&json!({ "mode": "expression", "expression": "@P" }), scene.clone(), 1.0).is_err(), "a list isn't a condition");
        assert!(delete(&json!({ "mode": "expression", "expression": "@nope > 1" }), scene.clone(), 1.0).is_err());
        assert_eq!(del(json!({ "group": "other", "mode": "expression", "expression": "1" }), &scene).point_sets[0].len(), n, "not its group");
        // Faces: the plane's left half; the vertices only they used go, and
        // their attributes in step.
        scene = ground(10.0);
        let mesh = Arc::make_mut(&mut scene.meshes[0].mesh);
        let (np, nf) = (mesh.positions.len(), mesh.triangles.len());
        mesh.point_attrs.set("w", Attr::Float((0..np).map(|i| i as f32).collect()), np).unwrap();
        mesh.face_attrs.set("f", Attr::Int((0..nf as i32).collect()), nf).unwrap();
        let out = del(json!({ "elements": "faces", "mode": "expression", "expression": "@P.x < 0 && @N.y > 0.5" }), &scene);
        let m = &out.meshes[0].mesh;
        assert_eq!(m.triangles.len(), nf / 2);
        assert!(m.positions.len() < np && m.point_attrs.check(m.positions.len()).is_ok() && m.face_attrs.check(nf / 2).is_ok());
        assert!(m.positions.iter().all(|p| p.x >= 0.0), "only the right half's vertices");
        assert!(del(json!({ "elements": "faces", "mode": "expression", "expression": "1" }), &scene).meshes.is_empty(), "no faces left: no mesh");
        // Instances by their middles; curves losing points.
        let at = |x: f64| geom::trs([x, 0.0, 0.0], [0.0; 3], [1.0; 3]);
        let inst = Instances { worlds: vec![at(-1.0), at(1.0)], which: vec![0, 0], node: "i".into(), ..Instances::default() };
        let mut c = Curves { node: "c".into(), ..Curves::default() };
        c.push(&[Vec3::new(-2.0, 0.0, 0.0), Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)], false);
        c.push(&[Vec3::new(1.0, 1.0, 0.0), Vec3::new(2.0, 1.0, 0.0)], true);
        scene = Scene { instances: vec![inst], curves: vec![c], ..Scene::default() };
        let out = del(json!({ "elements": "instances", "mode": "expression", "expression": "@P.x > 0" }), &scene);
        assert_eq!(out.instances[0].worlds, vec![at(-1.0)]);
        let out = del(json!({ "mode": "expression", "expression": "@P.x > 0" }), &scene);
        let c = &out.curves[0];
        assert!(c.validate().is_ok());
        assert_eq!((c.len(), c.positions.len()), (1, 2), "the first keeps two points; the second, none left, goes");
    }

    /// Attribute Set: rows in order on each kind of element; @P moves
    /// things, a mesh's @N sets its normals; kinds checked; other nodes'
    /// params read from what the graph resolved.
    #[test]
    fn attribute_set_works_out_rows_per_element() {
        use crate::geo::Instances;
        let set = |p: Value, s: Scene| attribute_set(&p, s, 1.0);
        let pts = PointSet { positions: vec![Vec3::zeros(), Vec3::x(), Vec3::new(2.0, 0.0, 0.0)], world: geom::trs([0.0, 1.0, 0.0], [0.0; 3], [1.0; 3]), node: "s".into(), ..PointSet::default() };
        let scene = Scene { point_sets: vec![pts], ..Scene::default() };
        // Row 1 a float from x; row 2 reads it; row 3 a color; row 4 moves
        // them up by row 1 (in the scene: the set's own placement undone).
        let out = set(json!({
            "attr1_name": "w", "attr1_kind": "float", "attr1_expr": "@P.x * 2",
            "attr2_name": "k", "attr2_kind": "int", "attr2_expr": "@w + @index + 0.4",
            "attr3_name": "Cd", "attr3_kind": "vec4", "attr3_expr": "0.5",
            "attr4_name": "P", "attr4_kind": "vec3", "attr4_expr": "@P + [0, @w, 0]",
        }), scene.clone()).unwrap();
        let g = &out.point_sets[0];
        assert_eq!(g.attrs.float("w"), Some(&[0.0, 2.0, 4.0][..]));
        assert_eq!(g.attrs.int("k"), Some(&[0, 3, 6][..]));
        assert_eq!(g.attrs.vec4(geo::CD).unwrap()[2], [0.5; 4]);
        assert_eq!(g.placed(1), Vec3::new(1.0, 3.0, 0.0));
        assert!(g.validate().is_ok());
        // Kinds: a list for a float, P not a vec3; a formula's own errors.
        assert!(set(json!({ "attr1_name": "w", "attr1_expr": "@P" }), scene.clone()).err().unwrap().to_string().contains("list of 3"));
        assert!(set(json!({ "attr1_name": "P", "attr1_kind": "float", "attr1_expr": "1" }), scene.clone()).is_err());
        assert!(set(json!({ "attr1_name": "w", "attr1_expr": "@nope" }), scene.clone()).is_err());
        // Other nodes' params from _refs.
        let out = set(json!({ "attr1_name": "d", "attr1_expr": "dist(@P, loc.translate)", "_refs": { "loc.translate": [0, 1, 0] } }), scene.clone()).unwrap();
        assert_eq!(out.point_sets[0].attrs.float("d"), Some(&[0.0, 1.0, 2.0][..]));
        // Mesh points: moved and given normals; faces: an attribute.
        let mesh = Scene { meshes: vec![crate::scene::Model { node: "m".into(), ..crate::scene::Model::new(Arc::new(crate::mesh::plane([2.0, 2.0], [1, 1])), geom::trs([5.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), crate::mesh::Material::default()) }], ..Scene::default() };
        let out = set(json!({ "elements": "mesh points", "attr1_name": "P", "attr1_kind": "vec3", "attr1_expr": "@P * 2", "attr2_name": "N", "attr2_kind": "vec3", "attr2_expr": "[1, 0, 0]" }), mesh.clone()).unwrap();
        let m = &out.meshes[0].mesh;
        assert_eq!(geom::transform_point(&out.meshes[0].world, &m.positions[0]), Vec3::new(8.0, -2.0, 0.0), "(4, -1, 0) doubled in the scene");
        assert!(m.normals.iter().all(|n| (n - Vec3::x()).norm() < 1e-9));
        let out = set(json!({ "elements": "faces", "attr1_name": "up", "attr1_kind": "float", "attr1_expr": "@N.z" }), mesh.clone()).unwrap();
        assert_eq!(out.meshes[0].mesh.face_attrs.float("up"), Some(&[1.0, 1.0][..]));
        assert!(set(json!({ "elements": "faces", "attr1_name": "P", "attr1_kind": "vec3", "attr1_expr": "@P" }), mesh).is_err());
        // Instances: @P is the placement's origin; setting it moves them.
        let at = |x: f64| geom::trs([x, 0.0, 0.0], [0.0, 90.0, 0.0], [1.0; 3]);
        let inst = Scene { instances: vec![Instances { worlds: vec![at(1.0), at(2.0)], which: vec![0, 0], node: "i".into(), ..Instances::default() }], ..Scene::default() };
        let out = set(json!({ "elements": "instances", "attr1_name": "P", "attr1_kind": "vec3", "attr1_expr": "@P * [1, 0, 0] + [0, @index, 0]" }), inst).unwrap();
        assert_eq!(out.instances[0].worlds[1], geom::trs([2.0, 1.0, 0.0], [0.0, 90.0, 0.0], [1.0; 3]), "moved, still turned");
    }

    /// Instance on Points: a prototype a point, picked, turned (+Z along
    /// N, +Y toward up), sized and carrying the point's attributes; Realize
    /// makes them real (meshes shared, tinted).
    #[test]
    fn instances_on_points_and_realized() {
        use crate::scene::Model;
        let cube = |c: f32| Model { node: format!("proto{c}"), material: crate::mesh::Material { color: [c; 3], ..crate::mesh::Material::default() }, ..Model::new(Arc::new(crate::mesh::cube([1.0; 3], [1; 3])), Mat4::identity(), crate::mesh::Material::default()) };
        let protos = Scene { meshes: vec![cube(1.0), cube(0.5)], cards: vec![Card { node: "leaf".into(), ..Card::new(Arc::new(Image::new(2, 2)), Mat4::identity(), [1.0, 1.0]) }], ..Scene::default() };
        let mut pts = PointSet { positions: vec![Vec3::zeros(), Vec3::new(5.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 5.0)], node: "pts".into(), ..PointSet::default() };
        let a = &mut pts.attrs;
        a.set(geo::N, Attr::Vec3(vec![[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]]), 3).unwrap();
        a.set(geo::PSCALE, Attr::Float(vec![2.0, 1.0, 1.0]), 3).unwrap();
        a.set(geo::VARIANT, Attr::Int(vec![0, 1, 5]), 3).unwrap();
        a.set(geo::CD, Attr::Vec4(vec![[1.0, 0.0, 0.0, 1.0]; 3]), 3).unwrap();
        a.set(geo::ID, Attr::Int(vec![10, 11, 12]), 3).unwrap();
        let scene = Scene { point_sets: vec![pts], ..Scene::default() };
        let run = |p: Value| instance_on_points(&p, scene.clone(), Some(&protos), None);
        let out = run(json!({ "pick": "variant" })).unwrap();
        assert!(out.point_sets.is_empty(), "the points are used up");
        let g = &out.instances[0];
        assert!(g.validate().is_ok());
        assert_eq!(g.which, vec![0, 1, 2], "variant 5 of 3 is the third");
        assert_eq!(g.attrs.int(geo::ID), Some(&[10, 11, 12][..]), "attributes carried");
        // The first: 2 big, its +Z along +X, up still +Y.
        let w = g.worlds[0];
        let dir = |v: Vec3| (w.fixed_view::<3, 3>(0, 0) * v).normalize();
        assert!((dir(Vec3::z()) - Vec3::x()).norm() < 1e-9 && (dir(Vec3::y()) - Vec3::y()).norm() < 1e-9);
        assert!(((w.fixed_view::<3, 3>(0, 0) * Vec3::z()).norm() - 2.0).abs() < 1e-9);
        assert_eq!(geom::transform_point(&g.worlds[1], &Vec3::zeros()), Vec3::new(5.0, 0.0, 0.0));
        // N straight up: +Y can't point up too, so it's turned square.
        assert!((g.worlds[2].fixed_view::<3, 3>(0, 0) * Vec3::z() - Vec3::y()).norm() < 1e-9);
        // Picks: in order; random by id, the same each time.
        assert_eq!(run(json!({ "pick": "in order" })).unwrap().instances[0].which, vec![0, 1, 2]);
        let r = run(json!({ "pick": "random", "seed": 4 })).unwrap().instances[0].which.clone();
        assert_eq!(run(json!({ "pick": "random", "seed": 4 })).unwrap().instances[0].which, r);
        assert_eq!(run(json!({ "keep_points": true, "orient": "none" })).unwrap().point_sets.len(), 1);
        // Facing a camera at +Z: +Z points at it.
        let cam = Scene { cameras: vec![crate::geom::Camera { world: geom::trs([0.0, 0.0, 20.0], [0.0; 3], [1.0; 3]), ..crate::geom::Camera::default() }], ..Scene::default() };
        let faced = instance_on_points(&json!({ "orient": "face camera" }), scene.clone(), Some(&protos), Some(&cam)).unwrap();
        let w = faced.instances[0].worlds[1];
        let to = (Vec3::new(0.0, 0.0, 20.0) - Vec3::new(5.0, 0.0, 0.0)).normalize();
        assert!(((w.fixed_view::<3, 3>(0, 0) * Vec3::z()).normalize() - to).norm() < 1e-9);
        assert!(instance_on_points(&json!({ "orient": "face camera" }), scene.clone(), Some(&protos), None).is_err());
        assert!(instance_on_points(&json!({}), scene.clone(), None, None).is_err());
        assert!(instance_on_points(&json!({}), scene.clone(), Some(&Scene::default()), None).is_err());
        // Realized: two meshes sharing the prototypes', red, and a card.
        let placed = out.instances[0].worlds[1];
        let real = realize_instances(&json!({}), out).unwrap();
        assert!(real.instances.is_empty() && real.meshes.len() == 2 && real.cards.len() == 1);
        assert!(Arc::ptr_eq(&real.meshes[0].mesh, &protos.meshes[0].mesh));
        assert_eq!(real.meshes[1].material.color, [0.5, 0.0, 0.0]);
        assert_eq!(real.meshes[1].world, placed * protos.meshes[1].world);
        assert_eq!(real.meshes[1].node, "", "the Instances' node (tagged by the graph)");
    }
}
