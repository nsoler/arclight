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
}
