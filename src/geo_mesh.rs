//! Mesh and point nodes (docs/geo-plan.md, P2 and P5): points from a
//! mesh's vertices or faces, grids and lines of points, and (to come)
//! the mesh operations. `ops.rs` registers them.
//!
//! Meshes keep their vertices split at seams (a cube's faces share no
//! vertices, uv seams split too); what needs to know which vertices are
//! the same point welds them by position (see [`weld`]).

use crate::geo::{self, Attr, Attrs, PointSet};
use crate::geom::{self, Vec3};
use crate::mesh::Mesh;
use crate::scene::{self, Scene};
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashMap;

/// The most points a grid or line makes.
pub const MAX_POINTS: usize = 5_000_000;

/// Vertices at the same place, as one: each vertex's group, and each
/// group's first vertex.
pub(crate) fn weld(positions: &[Vec3]) -> (Vec<u32>, Vec<u32>) {
    let mut groups: HashMap<[u64; 3], u32> = HashMap::new();
    let mut firsts = Vec::new();
    let of = positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            *groups.entry([p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]).or_insert_with(|| {
                firsts.push(i as u32);
                firsts.len() as u32 - 1
            })
        })
        .collect();
    (of, firsts)
}

/// `attr` per face, the mean of its three corners' values (an int: the
/// first corner's).
fn per_face(attr: &Attr, triangles: &[[u32; 3]]) -> Attr {
    let mean = |v: &dyn Fn(usize) -> f32, t: &[u32; 3]| (v(t[0] as usize) + v(t[1] as usize) + v(t[2] as usize)) / 3.0;
    match attr {
        Attr::Float(v) => Attr::Float(triangles.iter().map(|t| mean(&|i| v[i], t)).collect()),
        Attr::Int(v) => Attr::Int(triangles.iter().map(|t| v[t[0] as usize]).collect()),
        Attr::Vec3(v) => Attr::Vec3(triangles.iter().map(|t| std::array::from_fn(|k| mean(&|i| v[i][k], t))).collect()),
        Attr::Vec4(v) => Attr::Vec4(triangles.iter().map(|t| std::array::from_fn(|k| mean(&|i| v[i][k], t))).collect()),
    }
}

fn vec3s(v: impl Iterator<Item = Vec3>) -> Attr {
    Attr::Vec3(v.map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect())
}

fn ids(n: usize) -> Attr {
    Attr::Int((0..n as i32).collect())
}

/// A point set in the making: positions, attributes, normals (or none)
/// and texture coordinates (if any).
type Points = (Vec<Vec3>, Attrs, Vec<Vec3>, Option<Vec<[f64; 2]>>);

/// A mesh's points as a point set's positions and attributes: its
/// vertices (welded by position if `merge`: one point a place, its normal
/// the mean of theirs), or its faces' centers.
fn mesh_points(m: &Mesh, faces: bool, merge: bool) -> Result<(Vec<Vec3>, Attrs)> {
    let has_n = m.normals.len() == m.positions.len();
    let has_uv = m.uvs.len() == m.positions.len();
    let (positions, mut attrs, n, uv): Points = if faces {
        let mut attrs = Attrs::default();
        let count = m.triangles.len();
        for (name, a) in m.point_attrs.iter() {
            attrs.set(name, per_face(a, &m.triangles), count)?;
        }
        // The face's own attributes win over its corners'.
        for (name, a) in m.face_attrs.iter() {
            attrs.set(name, a.clone(), count)?;
        }
        let centers = m.triangles.iter().map(|t| t.iter().map(|&i| m.positions[i as usize]).sum::<Vec3>() / 3.0).collect();
        let normals = (0..count).map(|t| m.face_normal(t).try_normalize(1e-300).unwrap_or_else(Vec3::y)).collect();
        let uv = has_uv.then(|| m.triangles.iter().map(|t| {
            let s = t.iter().fold([0.0; 2], |a, &i| [a[0] + m.uvs[i as usize][0], a[1] + m.uvs[i as usize][1]]);
            [s[0] / 3.0, s[1] / 3.0]
        }).collect());
        (centers, attrs, normals, uv)
    } else if merge {
        let (of, firsts) = weld(&m.positions);
        let mut sums = vec![Vec3::zeros(); firsts.len()];
        for (i, g) in of.iter().enumerate() {
            if has_n {
                sums[*g as usize] += m.normals[i];
            }
        }
        let normals = if has_n { sums.iter().map(|s| s.try_normalize(1e-300).unwrap_or_else(Vec3::y)).collect() } else { Vec::new() };
        let uv = has_uv.then(|| firsts.iter().map(|&i| m.uvs[i as usize]).collect());
        (firsts.iter().map(|&i| m.positions[i as usize]).collect(), m.point_attrs.gathered(&firsts), normals, uv)
    } else {
        (m.positions.clone(), m.point_attrs.clone(), if has_n { m.normals.clone() } else { Vec::new() }, has_uv.then(|| m.uvs.clone()))
    };
    let count = positions.len();
    if !n.is_empty() {
        attrs.set(geo::N, vec3s(n.into_iter()), count)?;
    }
    if let Some(uv) = uv {
        attrs.set("uv", vec3s(uv.into_iter().map(|[u, v]| Vec3::new(u, v, 0.0))), count)?;
    }
    if attrs.get(geo::ID).is_none() {
        attrs.set(geo::ID, ids(count), count)?;
    }
    Ok((positions, attrs))
}

/// A Points from Geo: the meshes (of Group's nodes, or all) as point sets,
/// from their vertices or their faces' centers, with their attributes;
/// the meshes go unless Keep meshes is on.
pub fn points_from_geo(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let faces = params["from"].as_str() == Some("face centers");
    let merge = params["merge_shared"].as_bool().unwrap_or(true);
    let mut made = Vec::new();
    for m in scene.meshes.iter().filter(|m| take(&m.node)) {
        let (positions, attrs) = mesh_points(&m.mesh, faces, merge)?;
        made.push(PointSet { positions, attrs, world: m.world, node: m.node.clone() });
    }
    if !params["keep_meshes"].as_bool().unwrap_or(false) {
        scene.meshes.retain(|m| !take(&m.node));
    }
    scene.point_sets.extend(made);
    Ok(scene)
}

/// Element `k` of a [count] or [x, y, z] param, at least 1.
fn count(v: &Value, k: usize, default: usize) -> usize {
    v.get(k).and_then(Value::as_f64).map_or(default, |c| c.round().max(1.0) as usize)
}

/// A Grid Points: Count points across a Size rectangle (edges included)
/// in Plane, jittered within their cells by Jitter (stably: by Seed and
/// the cell), placed by Center and Rotate; each with an id from its cell
/// (so ids stay put when the grid grows), N (the plane's normal) and uv.
/// Added to the scene it's given.
pub fn grid_points(params: &Value, mut scene: Scene) -> Result<Scene> {
    let size = scene::numbers::<2>(&params["size"], 4.0);
    let (nx, nz) = (count(&params["count"], 0, 10), count(&params["count"], 1, 10));
    if nx > 10_000 || nz > 10_000 || nx * nz > MAX_POINTS {
        bail!("Grid Points: {nx} × {nz} points is too many (at most 10,000 a side, {MAX_POINTS} in all)");
    }
    let jitter = params["jitter"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let seed = params["seed"].as_i64().unwrap_or(0) as u64;
    let step = |n: usize, s: f64| if n > 1 { s / (n - 1) as f64 } else { 0.0 };
    let (sx, sz) = (step(nx, size[0]), step(nz, size[1]));
    // (a, b) across the plane, and its normal.
    let (to_3d, normal): (fn(f64, f64) -> Vec3, Vec3) = match params["plane"].as_str() {
        Some("xy") => (|a, b| Vec3::new(a, b, 0.0), Vec3::z()),
        Some("yz") => (|a, b| Vec3::new(0.0, a, b), Vec3::x()),
        _ => (|a, b| Vec3::new(a, 0.0, b), Vec3::y()),
    };
    let mut positions = Vec::with_capacity(nx * nz);
    let (mut id, mut uv) = (Vec::with_capacity(nx * nz), Vec::with_capacity(nx * nz));
    for j in 0..nz {
        for i in 0..nx {
            let shake = |axis: u64| (geo::stable_unit(&[seed, i as u64, j as u64, axis]) - 0.5) * jitter;
            let a = if nx > 1 { i as f64 * sx - size[0] / 2.0 } else { 0.0 } + shake(0) * sx;
            let b = if nz > 1 { j as f64 * sz - size[1] / 2.0 } else { 0.0 } + shake(1) * sz;
            positions.push(to_3d(a, b));
            id.push((j * 10_000 + i) as i32);
            let f = |k: usize, n: usize| if n > 1 { k as f64 / (n - 1) as f64 } else { 0.5 };
            uv.push(Vec3::new(f(i, nx), f(j, nz), 0.0));
        }
    }
    let n = positions.len();
    let mut attrs = Attrs::default();
    attrs.set(geo::ID, Attr::Int(id), n)?;
    attrs.set(geo::N, vec3s(std::iter::repeat_n(normal, n)), n)?;
    attrs.set("uv", vec3s(uv.into_iter()), n)?;
    let world = geom::trs(scene::numbers(&params["center"], 0.0), scene::numbers(&params["rotate"], 0.0), [1.0; 3]);
    scene.point_sets.push(PointSet { positions, attrs, world, node: String::new() });
    Ok(scene)
}

/// A Line Points: points from Start to End, Count of them (both ends
/// included) or every Spacing from Start; each with an id, uv (u along
/// the line) and N and tangent along it (so Instance on Points turns
/// copies' +Z along the line). Added to the scene it's given.
pub fn line_points(params: &Value, mut scene: Scene) -> Result<Scene> {
    let a = Vec3::from(scene::numbers::<3>(&params["start"], 0.0));
    let b = params.get("end").map_or(Vec3::x(), |e| Vec3::from(scene::numbers::<3>(e, 0.0)));
    let length = (b - a).norm();
    let dir = (b - a).try_normalize(1e-12).unwrap_or_else(Vec3::x);
    let ts: Vec<f64> = if params["mode"].as_str() == Some("spacing") {
        let s = params["spacing"].as_f64().unwrap_or(0.5);
        if s <= 0.0 {
            bail!("Line Points: Spacing must be above 0");
        }
        let n = (length / s + 1e-9).floor() as usize + 1;
        if n > MAX_POINTS {
            bail!("Line Points: {n} points is too many (at most {MAX_POINTS})");
        }
        (0..n).map(|k| if length > 0.0 { k as f64 * s / length } else { 0.0 }).collect()
    } else {
        let n = params["count"].as_u64().unwrap_or(10).max(1) as usize;
        if n > MAX_POINTS {
            bail!("Line Points: {n} points is too many (at most {MAX_POINTS})");
        }
        (0..n).map(|k| if n > 1 { k as f64 / (n - 1) as f64 } else { 0.0 }).collect()
    };
    let n = ts.len();
    let positions: Vec<Vec3> = ts.iter().map(|t| a + (b - a) * *t).collect();
    let mut attrs = Attrs::default();
    attrs.set(geo::ID, ids(n), n)?;
    attrs.set("uv", vec3s(ts.iter().map(|t| Vec3::new(*t, 0.0, 0.0))), n)?;
    attrs.set("tangent", vec3s(std::iter::repeat_n(dir, n)), n)?;
    attrs.set(geo::N, vec3s(std::iter::repeat_n(dir, n)), n)?;
    scene.point_sets.push(PointSet { positions, attrs, node: String::new(), ..PointSet::default() });
    Ok(scene)
}

/// `m` with normals that are smooth across edges whose faces meet at less
/// than `angle` degrees and sharp across the rest (vertices split there):
/// each corner's normal is the angle-weighted mean of the faces around
/// its point (welded by position) within `angle` of its own face.
pub fn normals_by_angle(m: &Mesh, angle: f64) -> Mesh {
    let (of, firsts) = weld(&m.positions);
    let face_n: Vec<Vec3> = (0..m.triangles.len()).map(|t| m.face_normal(t).try_normalize(1e-300).unwrap_or_else(Vec3::y)).collect();
    // The faces at each point, with the angle of their corner there.
    let mut around: Vec<Vec<(usize, f64)>> = vec![Vec::new(); firsts.len()];
    for (t, tri) in m.triangles.iter().enumerate() {
        for k in 0..3 {
            let (p, a, b) = (m.positions[tri[k] as usize], m.positions[tri[(k + 1) % 3] as usize], m.positions[tri[(k + 2) % 3] as usize]);
            around[of[tri[k] as usize] as usize].push((t, (a - p).angle(&(b - p))));
        }
    }
    let cos = angle.to_radians().cos();
    let mut out = Mesh { face_attrs: m.face_attrs.clone(), ..Mesh::default() };
    let mut made: HashMap<(u32, [u64; 3]), u32> = HashMap::new();
    let mut sources = Vec::new();
    let has_uv = m.uvs.len() == m.positions.len();
    for (t, tri) in m.triangles.iter().enumerate() {
        let corners = tri.map(|v| {
            let n = around[of[v as usize] as usize]
                .iter()
                .filter(|(f, _)| face_n[*f].dot(&face_n[t]) >= cos - 1e-12)
                .fold(Vec3::zeros(), |s, (f, w)| s + face_n[*f] * *w)
                .try_normalize(1e-300)
                .unwrap_or(face_n[t]);
            *made.entry((v, [n.x.to_bits(), n.y.to_bits(), n.z.to_bits()])).or_insert_with(|| {
                out.positions.push(m.positions[v as usize]);
                out.normals.push(n);
                if has_uv {
                    out.uvs.push(m.uvs[v as usize]);
                }
                sources.push(v);
                out.positions.len() as u32 - 1
            })
        });
        out.triangles.push(corners);
    }
    out.point_attrs = m.point_attrs.gathered(&sources);
    out
}

/// A Normals: the meshes' (Group's) normals made smooth (across seams),
/// flat (faceted) or by angle (smooth where faces meet at less than
/// Angle, sharp across the rest).
pub fn normals(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let mode = params["mode"].as_str().unwrap_or("smooth");
    let angle = params["angle"].as_f64().unwrap_or(30.0).clamp(0.0, 180.0);
    for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
        let mesh = match mode {
            "flat" => m.mesh.flat(),
            "by angle" => normals_by_angle(&m.mesh, angle),
            _ => {
                let mut s = (*m.mesh).clone();
                s.smooth_normals();
                s
            }
        };
        m.mesh = std::sync::Arc::new(mesh);
    }
    Ok(scene)
}

/// The most triangles Subdivide makes.
pub const MAX_TRIANGLES: usize = 5_000_000;

/// `attr` for the vertices `pairs` (each the mean of two; an int, the
/// first's), after its own values.
fn with_midpoints(attr: &Attr, pairs: &[(u32, u32)]) -> Attr {
    let mid = |a: f32, b: f32| (a + b) / 2.0;
    match attr {
        Attr::Float(v) => Attr::Float(v.iter().copied().chain(pairs.iter().map(|&(a, b)| mid(v[a as usize], v[b as usize]))).collect()),
        Attr::Int(v) => Attr::Int(v.iter().copied().chain(pairs.iter().map(|&(a, _)| v[a as usize])).collect()),
        Attr::Vec3(v) => Attr::Vec3(v.iter().copied().chain(pairs.iter().map(|&(a, b)| std::array::from_fn(|k| mid(v[a as usize][k], v[b as usize][k])))).collect()),
        Attr::Vec4(v) => Attr::Vec4(v.iter().copied().chain(pairs.iter().map(|&(a, b)| std::array::from_fn(|k| mid(v[a as usize][k], v[b as usize][k])))).collect()),
    }
}

/// `m` subdivided once: each triangle into four at its edges' midpoints,
/// and with `smooth` moved by Loop's rules (on the vertices welded by
/// position, so seams stay closed; boundaries kept as curves). Vertices
/// keep their layout (seams stay seams); new ones take the mean of their
/// edge's ends' uvs and attributes; child faces their parent's.
pub fn subdivide_once(m: &Mesh, smooth: bool) -> Mesh {
    let (of, firsts) = weld(&m.positions);
    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
    // Welded edges: the points opposite them, a face each.
    let mut edges: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for tri in &m.triangles {
        let g = tri.map(|v| of[v as usize]);
        for k in 0..3 {
            edges.entry(key(g[k], g[(k + 1) % 3])).or_default().push(g[(k + 2) % 3]);
        }
    }
    let at = |g: u32| m.positions[firsts[g as usize] as usize];
    // Each welded point's neighbors, and those along a boundary.
    let mut near: Vec<Vec<u32>> = vec![Vec::new(); firsts.len()];
    let mut rim: Vec<Vec<u32>> = vec![Vec::new(); firsts.len()];
    for (&(a, b), opposite) in &edges {
        near[a as usize].push(b);
        near[b as usize].push(a);
        if opposite.len() == 1 {
            rim[a as usize].push(b);
            rim[b as usize].push(a);
        }
    }
    let moved: Vec<Vec3> = (0..firsts.len() as u32)
        .map(|g| {
            let v = at(g);
            if !smooth {
                return v;
            }
            let (n, r) = (&near[g as usize], &rim[g as usize]);
            if r.len() >= 2 {
                v * 0.75 + (at(r[0]) + at(r[1])) * 0.125
            } else if r.is_empty() && !n.is_empty() {
                let k = n.len() as f64;
                let beta = if n.len() == 3 { 3.0 / 16.0 } else { 3.0 / (8.0 * k) };
                v * (1.0 - k * beta) + n.iter().map(|&j| at(j)).sum::<Vec3>() * beta
            } else {
                v
            }
        })
        .collect();
    let edge_point = |a: u32, b: u32| -> Vec3 {
        let opposite = &edges[&key(a, b)];
        if smooth && opposite.len() == 2 {
            (at(a) + at(b)) * 0.375 + (at(opposite[0]) + at(opposite[1])) * 0.125
        } else {
            (at(a) + at(b)) * 0.5
        }
    };
    let has_uv = m.uvs.len() == m.positions.len();
    let has_n = m.normals.len() == m.positions.len();
    let mut out = Mesh { positions: of.iter().map(|g| moved[*g as usize]).collect(), uvs: if has_uv { m.uvs.clone() } else { Vec::new() }, ..Mesh::default() };
    if has_n && !smooth {
        out.normals = m.normals.clone();
    }
    // A new vertex per edge of the original layout.
    let mut mids: HashMap<(u32, u32), u32> = HashMap::new();
    let mut pairs = Vec::new();
    let mut mid = |a: u32, b: u32, out: &mut Mesh| -> u32 {
        *mids.entry(key(a, b)).or_insert_with(|| {
            out.positions.push(edge_point(of[a as usize], of[b as usize]));
            if has_uv {
                let (p, q) = (m.uvs[a as usize], m.uvs[b as usize]);
                out.uvs.push([(p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0]);
            }
            if has_n && !smooth {
                out.normals.push((m.normals[a as usize] + m.normals[b as usize]).try_normalize(1e-300).unwrap_or(m.normals[a as usize]));
            }
            pairs.push((a, b));
            out.positions.len() as u32 - 1
        })
    };
    let mut parents = Vec::with_capacity(m.triangles.len() * 4);
    for (t, &[a, b, c]) in m.triangles.iter().enumerate() {
        let (ab, bc, ca) = (mid(a, b, &mut out), mid(b, c, &mut out), mid(c, a, &mut out));
        out.triangles.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        parents.extend([t as u32; 4]);
    }
    for (name, a) in m.point_attrs.iter() {
        let n = out.positions.len();
        let _ = out.point_attrs.set(name, with_midpoints(a, &pairs), n);
    }
    out.face_attrs = m.face_attrs.gathered(&parents);
    if smooth && has_n {
        out.smooth_normals();
    }
    out
}

/// A Subdivide: the meshes (Group's) subdivided Iterations times, linear
/// (the shape kept, finer) or Loop (smoothed toward a limit surface).
pub fn subdivide(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let smooth = params["mode"].as_str() != Some("linear");
    let times = params["iterations"].as_u64().unwrap_or(1).clamp(1, 4) as u32;
    for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
        let made = m.mesh.triangles.len().saturating_mul(4usize.pow(times));
        if made > MAX_TRIANGLES {
            bail!("Subdivide: {} would have {made} triangles (at most {MAX_TRIANGLES}); fewer iterations", m.node);
        }
        let mut mesh = (*m.mesh).clone();
        for _ in 0..times {
            mesh = subdivide_once(&mesh, smooth);
        }
        m.mesh = std::sync::Arc::new(mesh);
    }
    Ok(scene)
}

/// A Displace: the meshes' (Group's) points moved along their normals by
/// Amount × a height: a float point attribute (Source attribute), 3-D
/// noise at their place in the scene (noise, as Attribute Noise's), or an
/// image's luminance less Midlevel (image: at their uv, or where the
/// camera input's camera sees them). Heights and normals are taken per
/// point welded by position (the mean of its vertices'), so seams stay
/// closed. Normals are made smooth again after, unless Recompute normals
/// is off.
pub fn displace(params: &Value, mut scene: Scene, image: Option<&crate::image::Image>, camera: Option<&Scene>) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let amount = num("amount", 0.1);
    let source = params["source"].as_str().unwrap_or("noise");
    let attr = params["attribute"].as_str().unwrap_or("height").trim();
    let (freq, octaves, seed) = (num("frequency", 1.0), params["octaves"].as_u64().unwrap_or(1).clamp(1, 8) as usize, num("seed", 0.0));
    let offset = Vec3::from(scene::numbers::<3>(&params["offset"], 0.0));
    let midlevel = num("midlevel", 0.5);
    let via_camera = params["via"].as_str() == Some("camera");
    let cam = camera.and_then(|s| s.cameras.first()).cloned();
    if source == "image" {
        if image.is_none() {
            bail!("Displace: Source is image, but nothing is connected to its image input");
        }
        if via_camera && cam.is_none() {
            bail!("Displace: Via is camera, but nothing with a camera is connected to its camera input");
        }
    }
    let recompute = params["recompute_normals"].as_bool().unwrap_or(true);
    for model in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
        let m = &*model.mesh;
        if source == "attribute" && m.point_attrs.float(attr).is_none() {
            bail!("Displace: {} has no float point attribute '{attr}' (set one with Attribute Set or Attribute Noise)", model.node);
        }
        let (of, firsts) = weld(&m.positions);
        let normals = if m.normals.len() == m.positions.len() {
            m.normals.clone()
        } else {
            let mut s = m.clone();
            s.smooth_normals();
            s.normals
        };
        // Per welded point: the mean normal and height of its vertices.
        let mut sum_n = vec![Vec3::zeros(); firsts.len()];
        let mut sum_h = vec![0.0; firsts.len()];
        let mut count = vec![0.0; firsts.len()];
        for (i, g) in of.iter().enumerate() {
            let g = *g as usize;
            sum_n[g] += normals[i];
            count[g] += 1.0;
            let placed = geom::transform_point(&model.world, &m.positions[i]);
            sum_h[g] += match source {
                "attribute" => m.point_attrs.float(attr).map_or(0.0, |v| v[i] as f64),
                "image" => {
                    let img = image.expect("checked");
                    let f = img.format;
                    let q = match &cam {
                        Some(c) if via_camera => c.project(&placed, f).map(|(q, _)| q),
                        _ => m.uvs.get(i).map(|[u, v]| [u * f.width as f64, (1.0 - v) * f.height as f64]),
                    };
                    q.map_or(midlevel, |[x, y]| {
                        let p = crate::geo_attr::sample(img, x.clamp(0.5, f.width as f64 - 0.5), y.clamp(0.5, f.height as f64 - 0.5));
                        0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
                    }) - midlevel
                }
                _ => crate::geo_attr::fbm(placed * freq + offset, octaves, seed),
            };
        }
        let mut out = m.clone();
        for (i, g) in of.iter().enumerate() {
            let g = *g as usize;
            let n = sum_n[g].try_normalize(1e-300).unwrap_or_else(Vec3::y);
            out.positions[i] = m.positions[firsts[g] as usize] + n * (amount * sum_h[g] / count[g]);
        }
        if recompute {
            out.smooth_normals();
        }
        model.mesh = std::sync::Arc::new(out);
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Model;
    use serde_json::json;
    use std::sync::Arc;

    fn cube_scene() -> Scene {
        let mut m = crate::mesh::cube([2.0, 2.0, 2.0], [1, 1, 1]);
        let n = m.positions.len();
        m.point_attrs.set("w", Attr::Float((0..n).map(|i| i as f32).collect()), n).unwrap();
        let t = m.triangles.len();
        m.face_attrs.set("side", Attr::Int((0..t as i32).map(|k| k / 2).collect()), t).unwrap();
        let model = Model { node: "box".into(), ..Model::new(Arc::new(m), geom::trs([0.0, 5.0, 0.0], [0.0; 3], [1.0; 3]), Default::default()) };
        Scene { meshes: vec![model], ..Scene::default() }
    }

    #[test]
    fn a_cube_gives_its_eight_corners_or_its_vertices_or_its_faces() {
        let out = points_from_geo(&json!({}), cube_scene()).unwrap();
        assert!(out.meshes.is_empty(), "the mesh goes");
        let p = &out.point_sets[0];
        assert_eq!((p.len(), p.node.as_str()), (8, "box"));
        assert!((0..8).all(|i| p.placed(i).y == 4.0 || p.placed(i).y == 6.0), "placed by the mesh's world");
        // A corner's normal: the mean of its three faces', diagonal.
        let n = p.attrs.vec3("N").unwrap()[0];
        assert!((n[0].abs() - 0.57735).abs() < 1e-4 && (n[1].abs() - 0.57735).abs() < 1e-4, "{n:?}");
        assert_eq!(p.attrs.int("id").unwrap().len(), 8);
        let all = points_from_geo(&json!({"merge_shared": false, "keep_meshes": true}), cube_scene()).unwrap();
        assert_eq!((all.point_sets[0].len(), all.meshes.len()), (24, 1));
        assert_eq!(all.point_sets[0].attrs.float("w").unwrap()[5], 5.0, "the vertices' own attributes");
        let faces = points_from_geo(&json!({"from": "face centers"}), cube_scene()).unwrap();
        let f = &faces.point_sets[0];
        assert_eq!(f.len(), 12);
        // Each face center on the cube's surface, its N the face's normal.
        for i in 0..12 {
            let (c, n) = (f.positions[i], Vec3::from(f.attrs.vec3("N").unwrap()[i].map(f64::from)));
            assert!((c.dot(&n) - 1.0).abs() < 1e-6, "{i}: {c:?} {n:?}");
        }
        assert_eq!(f.attrs.int("side").unwrap()[3], 1, "face attributes");
        let w = f.attrs.float("w").unwrap();
        let t = cube_scene().meshes[0].mesh.triangles[4];
        assert!((w[4] - (t[0] + t[1] + t[2]) as f32 / 3.0).abs() < 1e-6, "point attributes averaged");
    }

    #[test]
    fn grids_of_points_with_stable_ids_and_jitter_in_their_cells() {
        let out = grid_points(&json!({"size": [4, 2], "count": [5, 3]}), Scene::default()).unwrap();
        let p = &out.point_sets[0];
        assert_eq!(p.len(), 15);
        assert_eq!((p.positions[0], p.positions[14]), (Vec3::new(-2.0, 0.0, -1.0), Vec3::new(2.0, 0.0, 1.0)), "edges included, on XZ");
        assert_eq!(p.attrs.vec3("N").unwrap()[0], [0.0, 1.0, 0.0]);
        assert_eq!(p.attrs.vec3("uv").unwrap()[14], [1.0, 1.0, 0.0]);
        // A wider grid keeps the ids of the cells it shares.
        let wide = grid_points(&json!({"size": [4, 2], "count": [7, 3]}), Scene::default()).unwrap();
        let (a, b) = (p.attrs.int("id").unwrap(), wide.point_sets[0].attrs.int("id").unwrap());
        assert_eq!((a[6], b[8]), (10_001, 10_001), "cell (1, 1)");
        // Jitter: within half a cell each way, the same for a seed.
        let j = grid_points(&json!({"size": [4, 2], "count": [5, 3], "jitter": 1.0, "seed": 3}), Scene::default()).unwrap();
        let moved: Vec<f64> = (0..15).map(|i| (j.point_sets[0].positions[i] - p.positions[i]).abs().max()).collect();
        assert!(moved.iter().all(|d| *d <= 0.5 + 1e-9) && moved.iter().any(|d| *d > 0.05), "{moved:?}");
        assert_eq!(grid_points(&json!({"size": [4, 2], "count": [5, 3], "jitter": 1.0, "seed": 3}), Scene::default()).unwrap().point_sets[0].positions, j.point_sets[0].positions);
        // Other planes, placed; and too many.
        let xy = grid_points(&json!({"count": [2, 2], "plane": "xy", "center": [0, 1, 0]}), Scene::default()).unwrap();
        assert_eq!((xy.point_sets[0].placed(3), xy.point_sets[0].attrs.vec3("N").unwrap()[0]), (Vec3::new(2.0, 3.0, 0.0), [0.0, 0.0, 1.0]));
        assert!(grid_points(&json!({"count": [20000, 2]}), Scene::default()).is_err());
    }

    /// As nodes: a grid's points instanced, and a cube's corners.
    #[test]
    fn the_nodes_in_a_graph() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "grid", "op": "grid_points", "params": {"count": [3, 4]}},
            {"id": "line", "op": "line_points", "inputs": ["grid"], "params": {"count": 5}},
            {"id": "box", "op": "cube"},
            {"id": "corners", "op": "points_from_geo", "inputs": ["box"]},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let mut ev = crate::graph::Evaluator::new();
        let both = ev.evaluate(&graph, "line", &ctx).unwrap();
        let sets = &both.scene.as_ref().unwrap().point_sets;
        assert_eq!((sets.len(), sets[0].len(), sets[1].len()), (2, 12, 5));
        assert_eq!((sets[0].node.as_str(), sets[1].node.as_str()), ("grid", "line"), "each tagged with its node");
        let corners = ev.evaluate(&graph, "corners", &ctx).unwrap();
        assert_eq!(corners.scene.as_ref().unwrap().point_sets[0].len(), 8);
    }

    #[test]
    fn lines_of_points_by_count_or_spacing() {
        let out = line_points(&json!({"start": [0, 0, 0], "end": [0, 0, 4], "count": 5}), Scene::default()).unwrap();
        let p = &out.point_sets[0];
        assert_eq!(p.positions, (0..5).map(|k| Vec3::new(0.0, 0.0, k as f64)).collect::<Vec<_>>());
        assert_eq!((p.attrs.vec3("tangent").unwrap()[2], p.attrs.vec3("N").unwrap()[0]), ([0.0, 0.0, 1.0], [0.0, 0.0, 1.0]));
        assert_eq!(p.attrs.vec3("uv").unwrap()[4], [1.0, 0.0, 0.0]);
        let spaced = line_points(&json!({"end": [3.5, 0, 0], "mode": "spacing", "spacing": 1.0}), Scene::default()).unwrap();
        let xs: Vec<f64> = spaced.point_sets[0].positions.iter().map(|p| p.x).collect();
        assert_eq!(xs, [0.0, 1.0, 2.0, 3.0], "short of the end");
        assert!(line_points(&json!({"mode": "spacing", "spacing": 0}), Scene::default()).is_err());
        // Added to the scene given.
        let both = line_points(&json!({}), out).unwrap();
        assert_eq!(both.point_sets.len(), 2);
    }

    fn mesh_of(op: &str, p: Value) -> Mesh {
        let model = crate::scene::primitive_model(op, &p, None).unwrap();
        (*model.mesh).clone()
    }

    fn one(m: Mesh) -> Scene {
        Scene { meshes: vec![Model::new(Arc::new(m), crate::geom::Mat4::identity(), Default::default())], ..Scene::default() }
    }

    #[test]
    fn normals_smooth_flat_or_by_angle() {
        let cube = mesh_of("cube", json!({}));
        // By 30°: the cube's faces meet at 90°, so every corner has its face's normal.
        let sharp = normals_by_angle(&cube, 30.0);
        assert_eq!(sharp.positions.len(), 24);
        for (t, tri) in sharp.triangles.iter().enumerate() {
            let f = sharp.face_normal(t).normalize();
            assert!(tri.iter().all(|&v| (sharp.normals[v as usize] - f).norm() < 1e-9), "{t}");
        }
        // By 100°: all smooth, each corner's normal the diagonal.
        let soft = normals_by_angle(&cube, 100.0);
        assert!(soft.normals.iter().all(|n| (n.x.abs() - 1.0 / 3f64.sqrt()).abs() < 1e-9), "{:?}", &soft.normals[..3]);
        // A cylinder by 30°: the side smooth (its normals level), the caps flat and split from it.
        let cyl = normals_by_angle(&mesh_of("cylinder", json!({"segments": [16, 1]})), 30.0);
        let level = cyl.normals.iter().filter(|n| n.y.abs() < 1e-9).count();
        let caps = cyl.normals.iter().filter(|n| (n.y.abs() - 1.0).abs() < 1e-9).count();
        assert!(level > 0 && caps > 0 && level + caps == cyl.normals.len(), "{level} {caps} of {}", cyl.normals.len());
        // A tent: two faces at 90° sharing an edge's vertices; split by
        // 30°, shared by 100°.
        let tent = Mesh {
            positions: vec![Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 1.0, 1.0), Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)],
            triangles: vec![[0, 2, 1], [0, 1, 3]],
            ..Mesh::default()
        };
        assert_eq!((normals_by_angle(&tent, 30.0).positions.len(), normals_by_angle(&tent, 100.0).positions.len()), (6, 4));
        // The node: smooth, flat (3 vertices a face), by angle; attributes carried.
        let mut c = cube.clone();
        let n = c.positions.len();
        c.point_attrs.set("w", Attr::Float((0..n).map(|i| i as f32).collect()), n).unwrap();
        let flat = normals(&json!({"mode": "flat"}), one(c.clone())).unwrap();
        assert_eq!(flat.meshes[0].mesh.positions.len(), 36);
        let smooth = normals(&json!({}), one(c.clone())).unwrap();
        assert!(smooth.meshes[0].mesh.normals.iter().all(|n| (n.y.abs() - 1.0 / 3f64.sqrt()).abs() < 1e-9));
        let by = normals(&json!({"mode": "by angle", "angle": 30}), one(c)).unwrap();
        let m = &by.meshes[0].mesh;
        let w = m.point_attrs.float("w").unwrap();
        assert!((0..m.positions.len()).all(|i| cube.positions[w[i] as usize] == m.positions[i]), "each vertex's own attributes");
    }

    /// Evaluates `nodes` and returns `id`'s first mesh.
    fn graph_mesh(nodes: Value, id: &str) -> Mesh {
        let graph: crate::graph::Graph = serde_json::from_value(json!({ "nodes": nodes })).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let out = crate::graph::Evaluator::new().evaluate(&graph, id, &ctx).unwrap_or_else(|e| panic!("{e}"));
        (*out.scene.as_ref().unwrap().meshes[0].mesh).clone()
    }

    #[test]
    fn normals_in_a_graph() {
        let m = graph_mesh(json!([{"id": "c", "op": "cylinder"}, {"id": "n", "op": "normals", "inputs": ["c"], "params": {"mode": "flat"}}]), "n");
        assert_eq!(m.positions.len(), m.triangles.len() * 3);
    }

    #[test]
    fn linear_subdivision_keeps_the_shape_and_interpolates() {
        let mut plane = mesh_of("plane", json!({"size": [2, 2]}));
        let n = plane.positions.len();
        plane.point_attrs.set("w", Attr::Float((0..n).map(|i| i as f32).collect()), n).unwrap();
        plane.face_attrs.set("f", Attr::Int(vec![4, 7]), 2).unwrap();
        let out = subdivide(&json!({"mode": "linear", "iterations": 2}), one(plane.clone())).unwrap();
        let m = &out.meshes[0].mesh;
        assert_eq!(m.triangles.len(), 2 * 16);
        assert!(m.positions.iter().all(|p| p.z == 0.0 && p.x.abs() <= 1.0 + 1e-12), "flat and within");
        assert!(m.normals.len() == m.positions.len() && m.normals.iter().all(|n| (n.z - 1.0).abs() < 1e-12), "normals kept");
        let once = subdivide_once(&plane, false);
        let w = once.point_attrs.float("w").unwrap();
        // The first new vertex: the mean of the first edge's ends.
        let t = plane.triangles[0];
        assert_eq!(w[n], (t[0] + t[1]) as f32 / 2.0);
        assert_eq!(once.face_attrs.int("f").unwrap(), [4, 4, 4, 4, 7, 7, 7, 7]);
        assert!(subdivide(&json!({"iterations": 4}), one(mesh_of("sphere", json!({"segments": [512, 256]})))).is_err(), "too many");
    }

    #[test]
    fn loop_subdivision_by_hand_and_closed_seams() {
        // A regular tetrahedron about the origin: every vertex goes to a
        // quarter of itself (1 - 3·3/16 of it, 3/16 of the others' sum,
        // which is minus it), every edge point to a quarter of its ends.
        let v = [Vec3::new(1.0, 1.0, 1.0), Vec3::new(1.0, -1.0, -1.0), Vec3::new(-1.0, 1.0, -1.0), Vec3::new(-1.0, -1.0, 1.0)];
        let tet = Mesh { positions: v.to_vec(), triangles: vec![[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]], ..Mesh::default() };
        let s = subdivide_once(&tet, true);
        for (i, p) in v.iter().enumerate() {
            assert!((s.positions[i] - p / 4.0).norm() < 1e-12, "{i}");
        }
        let (a, b) = (s.triangles[0][0], s.triangles[0][1]);
        assert!((s.positions[b as usize] - (v[0] + v[1]) / 4.0).norm() < 1e-12, "{a} {b}");
        // A cube (faces with their own vertices): the same welded points as
        // a linear split (no cracks), drawn in toward a sphere.
        let cube = mesh_of("cube", json!({"size": [2, 2, 2]}));
        let (smooth, lin) = (subdivide_once(&cube, true), subdivide_once(&cube, false));
        assert_eq!(weld(&smooth.positions).1.len(), weld(&lin.positions).1.len());
        assert_eq!(weld(&smooth.positions).1.len(), 8 + 18);
        assert!(smooth.positions.iter().all(|p| p.norm() < 3f64.sqrt() - 0.1), "rounded");
        let r: Vec<f64> = smooth.positions.iter().map(|p| p.norm()).collect();
        let corner = r.iter().cloned().fold(0.0, f64::max);
        assert!(r.iter().filter(|x| (**x - corner).abs() < 1e-9).count() >= 8, "symmetric corners");
        // A flat plane stays flat; its border shrinks along itself only.
        let p = subdivide_once(&mesh_of("plane", json!({"size": [2, 2], "segments": [2, 2]})), true);
        assert!(p.positions.iter().all(|q| q.z.abs() < 1e-12));
        // Its corner (1, 1) moves in along the border: ¾ of it and ⅛ of each
        // border neighbor, (1, 0) and (0, 1).
        let far = p.positions.iter().map(|q| q.x + q.y).fold(f64::MIN, f64::max);
        assert!((far - 1.75).abs() < 1e-12, "{far}");
    }

    #[test]
    fn subdivide_in_a_graph() {
        let m = graph_mesh(json!([{"id": "c", "op": "cube"}, {"id": "s", "op": "subdivide", "inputs": ["c"], "params": {"iterations": 2}}]), "s");
        assert_eq!(m.triangles.len(), 12 * 16);
    }

    #[test]
    fn displace_by_an_attribute_noise_or_an_image() {
        // A 2 × 2 plane in 4 × 4 squares, facing +z, its height x + 1.
        let mut plane = mesh_of("plane", json!({"size": [2, 2], "segments": [4, 4]}));
        let n = plane.positions.len();
        let h: Vec<f32> = plane.positions.iter().map(|p| p.x as f32 + 1.0).collect();
        plane.point_attrs.set("height", Attr::Float(h), n).unwrap();
        let out = displace(&json!({"source": "attribute", "amount": 0.5}), one(plane.clone()), None, None).unwrap();
        let m = &out.meshes[0].mesh;
        assert!(m.positions.iter().zip(&plane.positions).all(|(q, p)| (q.z - 0.5 * (p.x + 1.0)).abs() < 1e-9 && q.x == p.x), "along +z by it");
        assert!(m.normals.iter().all(|n| n.z < 0.99), "normals made again: tilted");
        assert!(displace(&json!({"source": "attribute", "attribute": "nope"}), one(plane.clone()), None, None).is_err());
        // Noise: the formulas' noise at each point.
        let noisy = displace(&json!({"amount": 0.3, "frequency": 2.0, "seed": 1}), one(plane.clone()), None, None).unwrap();
        for (q, p) in noisy.meshes[0].mesh.positions.iter().zip(&plane.positions) {
            let want = 0.3 * crate::expr::noise3([p.x * 2.0, p.y * 2.0, 0.0], 1.0);
            assert!((q.z - want).abs() < 1e-9, "{q:?}");
        }
        // An image by uv: white is up (1 - midlevel), black down.
        let mut img = crate::image::Image::blank(crate::image::Size { width: 2, height: 1 }, crate::image::IRect::new(0, 0, 2, 1));
        img.pixels = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]];
        let shown = displace(&json!({"source": "image", "amount": 1.0, "recompute_normals": false}), one(plane.clone()), Some(&img), None).unwrap();
        let m = &shown.meshes[0].mesh;
        let at = |x: f64| m.positions.iter().zip(&plane.positions).find(|(_, p)| (p.x - x).abs() < 1e-9).unwrap().0.z;
        assert!((at(-1.0) + 0.5).abs() < 1e-9 && (at(1.0) - 0.5).abs() < 1e-9 && at(0.0).abs() < 1e-9, "{} {} {}", at(-1.0), at(0.0), at(1.0));
        assert!(displace(&json!({"source": "image"}), one(plane), None, None).is_err(), "no image");
    }

    #[test]
    fn displaced_seams_stay_closed() {
        // A cube's faces have their own vertices: displaced by noise along
        // the (welded) mean normal, its corners stay one point each.
        let cube = mesh_of("cube", json!({"size": [2, 2, 2], "segments": [2, 2, 2]}));
        let before = weld(&cube.positions).1.len();
        let out = displace(&json!({"amount": 0.4, "frequency": 1.3}), one(cube), None, None).unwrap();
        assert_eq!(weld(&out.meshes[0].mesh.positions).1.len(), before);
    }

    #[test]
    fn displace_in_a_graph() {
        let m = graph_mesh(json!([{"id": "p", "op": "plane", "params": {"segments": [8, 8]}}, {"id": "d", "op": "displace", "inputs": ["p", "", ""], "params": {"amount": 0.5}}]), "d");
        assert!(m.positions.iter().any(|p| p.z.abs() > 1e-3), "moved off the plane");
    }
}
