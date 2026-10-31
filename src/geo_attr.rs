//! Attribute nodes (docs/geo-plan.md, P3) besides Attribute Set: each
//! writes one attribute on the elements of a scene's geometry (point sets'
//! and curves' points, mesh points, or instances) and passes the rest
//! through. `ops.rs` registers them.
//!
//! - Attribute Noise: 3-D noise (the formulas' `noise3`), set, added or
//!   multiplied in, optionally only in a region with a soft edge.

use crate::geo::{self, Attr, Attrs};
use crate::geom::{self, Mat4, Vec3};
use crate::scene::{self, Region, Scene};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::sync::Arc;

/// The elements a node writes to, by its Elements param.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Elements {
    /// Point sets' and curves' points.
    Points,
    MeshPoints,
    Instances,
}

impl Elements {
    fn from(params: &Value) -> Elements {
        match params["elements"].as_str() {
            Some("mesh points") => Elements::MeshPoints,
            Some("instances") => Elements::Instances,
            _ => Elements::Points,
        }
    }
}

/// One object's elements: their places in the scene and its attributes
/// to change.
struct Target<'a> {
    placed: Vec<Vec3>,
    attrs: &'a mut Attrs,
}

/// Call `f` with each object of `scene` (of `group`'s nodes, a name or a
/// pattern, or all) that has `elements`.
fn each_target(scene: &mut Scene, elements: Elements, group: &str, mut f: impl FnMut(Target) -> Result<()>) -> Result<()> {
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    match elements {
        Elements::Points => {
            for g in scene.point_sets.iter_mut().filter(|g| take(&g.node)) {
                let placed = (0..g.len()).map(|i| g.placed(i)).collect();
                f(Target { placed, attrs: &mut g.attrs })?;
            }
            for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
                let placed = c.positions.iter().map(|p| geom::transform_point(&c.world, p)).collect();
                f(Target { placed, attrs: &mut c.attrs })?;
            }
        }
        Elements::MeshPoints => {
            for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
                let placed = m.mesh.positions.iter().map(|p| geom::transform_point(&m.world, p)).collect();
                f(Target { placed, attrs: &mut Arc::make_mut(&mut m.mesh).point_attrs })?;
            }
        }
        Elements::Instances => {
            for g in scene.instances.iter_mut().filter(|g| take(&g.node)) {
                let placed = g.worlds.iter().map(|w| Vec3::new(w[(0, 3)], w[(1, 3)], w[(2, 3)])).collect();
                f(Target { placed, attrs: &mut g.attrs })?;
            }
        }
    }
    Ok(())
}

/// How many components an attribute kind has.
fn width(kind: &str) -> usize {
    match kind {
        "vec3" => 3,
        "vec4" => 4,
        _ => 1,
    }
}

/// `values` (`n` rows of `w` components) as an attribute of `kind`.
fn attr_of(kind: &str, w: usize, values: &[f64]) -> Attr {
    let row = |i: usize| &values[i * w..(i + 1) * w];
    let n = values.len() / w.max(1);
    match kind {
        "vec3" => Attr::Vec3((0..n).map(|i| std::array::from_fn(|k| row(i)[k] as f32)).collect()),
        "vec4" => Attr::Vec4((0..n).map(|i| std::array::from_fn(|k| row(i)[k] as f32)).collect()),
        "int" => Attr::Int(values.iter().map(|v| v.round() as i32).collect()),
        _ => Attr::Float(values.iter().map(|v| *v as f32).collect()),
    }
}

/// Element `i`'s value of `attr` as `w` components (a scalar fills them
/// all; missing components are `fill`).
fn components(attr: &Attr, i: usize, w: usize, fill: f64) -> Vec<f64> {
    let all: Vec<f64> = match attr {
        Attr::Float(v) => vec![v[i] as f64; w],
        Attr::Int(v) => vec![v[i] as f64; w],
        Attr::Vec3(v) => v[i].iter().map(|x| *x as f64).collect(),
        Attr::Vec4(v) => v[i].iter().map(|x| *x as f64).collect(),
    };
    (0..w).map(|k| all.get(k).copied().unwrap_or(fill)).collect()
}

/// A box or ball a node acts in, with a soft edge.
pub struct Falloff {
    /// Scene space into the region's, where it's the unit cube or ball.
    to_region: Mat4,
    sphere: bool,
    /// The share of the region (from its edge in) over which the weight
    /// rises from 0 to 1.
    softness: f64,
}

impl Falloff {
    /// The region `params` describe (Falloff none, sphere or box; its
    /// region_center, region_size (whole, not half) and region_rotate;
    /// softness 0–1), and it as a Region for the 3D view to show. None for
    /// none.
    fn from(params: &Value) -> Option<(Falloff, Region)> {
        let sphere = match params["falloff"].as_str() {
            Some("sphere") => true,
            Some("box") => false,
            _ => return None,
        };
        let half = scene::numbers::<3>(&params["region_size"], 2.0).map(|v| (v.abs() / 2.0).max(1e-12));
        let world = geom::trs(scene::numbers(&params["region_center"], 0.0), scene::numbers(&params["region_rotate"], 0.0), half);
        let softness = params["softness"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        Some((Falloff { to_region: world.try_inverse().unwrap_or_else(Mat4::identity), sphere, softness }, Region { node: String::new(), world, sphere }))
    }

    /// The weight at `p`: 1 well inside, 0 outside, smooth across the
    /// soft edge.
    fn weight(&self, p: &Vec3) -> f64 {
        let q = geom::transform_point(&self.to_region, p);
        // 0 at the middle, 1 at the edge.
        let d = if self.sphere { q.norm() } else { q.abs().max() };
        if d >= 1.0 {
            return 0.0;
        }
        if self.softness <= 0.0 {
            return 1.0;
        }
        smooth(((1.0 - d) / self.softness).clamp(0.0, 1.0))
    }
}

/// Smoothstep from 0 to 1.
fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Fractal noise at `p`: `octaves` of noise3, each twice the frequency
/// and half the weight of the last, in -1..1.
fn fbm(p: Vec3, octaves: usize, seed: f64) -> f64 {
    let (mut sum, mut weight, mut total, mut f) = (0.0, 1.0, 0.0, 1.0);
    for _ in 0..octaves.max(1) {
        sum += weight * crate::expr::noise3([p.x * f, p.y * f, p.z * f], seed);
        total += weight;
        weight *= 0.5;
        f *= 2.0;
    }
    sum / total
}

/// An Attribute Noise: on each element, Center + Amplitude × noise (noise
/// in -1..1 at its place × Frequency + Offset; per component for vectors,
/// each its own), set, added to or multiplied into the attribute; in a
/// region only, if Falloff is on.
pub fn attribute_noise(params: &Value, mut scene: Scene) -> Result<Scene> {
    let name = params["name"].as_str().unwrap_or("pscale").trim();
    if name.is_empty() || name == "P" {
        bail!("Attribute Noise: give it an attribute to write (not P: move points with Attribute Set or Transform Geo)");
    }
    let kind = params["kind"].as_str().unwrap_or("float");
    let w = width(kind);
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let (freq, amp, center) = (num("frequency", 1.0), num("amplitude", 0.5), num("center", 1.0));
    let offset = Vec3::from(scene::numbers::<3>(&params["offset"], 0.0));
    let octaves = params["octaves"].as_u64().unwrap_or(1).clamp(1, 8) as usize;
    let seed = params["seed"].as_f64().unwrap_or(0.0);
    let mode = params["mode"].as_str().unwrap_or("set");
    let falloff = Falloff::from(params);
    let group = params["group"].as_str().unwrap_or("").trim();
    each_target(&mut scene, Elements::from(params), group, |t| {
        let n = t.placed.len();
        let old = t.attrs.get(name);
        if let Some(a) = old.filter(|_| mode != "set") {
            if width(a.kind()) != w {
                bail!("Attribute Noise: {name} is a {} here, not a {kind}", a.kind());
            }
        }
        // What's there now (0, or 1 to multiply, where it isn't).
        let fill = if mode == "multiply" { 1.0 } else { 0.0 };
        let values: Vec<f64> = (0..n)
            .into_par_iter()
            .flat_map_iter(|i| {
                let p = t.placed[i] * freq + offset;
                let before = old.map_or_else(|| vec![fill; w], |a| components(a, i, w, fill));
                let k = falloff.as_ref().map_or(1.0, |(f, _)| f.weight(&t.placed[i]));
                (0..w).map(move |c| {
                    let v = center + amp * fbm(p, octaves, seed + 101.0 * c as f64);
                    let new = match mode {
                        "add" => before[c] + v,
                        "multiply" => before[c] * v,
                        _ => v,
                    };
                    before[c] + (new - before[c]) * k
                })
            })
            .collect();
        t.attrs.set(name, attr_of(kind, w, &values), n)
    })?;
    if let Some((_, region)) = falloff {
        scene.regions.push(region);
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::PointSet;
    use serde_json::json;

    fn grid() -> PointSet {
        PointSet { positions: (0..100).map(|i| Vec3::new((i % 10) as f64 * 0.37, (i / 10) as f64 * 0.29, 0.5)).collect(), node: "pts".into(), ..PointSet::default() }
    }

    fn floats(s: &Scene, name: &str) -> Vec<f32> {
        s.point_sets[0].attrs.float(name).expect(name).to_vec()
    }

    #[test]
    fn noise_is_center_plus_amplitude_times_the_formulas_noise3() {
        let scene = Scene { point_sets: vec![grid()], ..Scene::default() };
        let out = attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 4}), scene.clone()).unwrap();
        let v = floats(&out, "pscale");
        for (i, p) in grid().positions.iter().enumerate() {
            let q = p * 2.0 + Vec3::new(0.1, 0.0, 0.0);
            let want = 1.0 + 0.5 * crate::expr::noise3([q.x, q.y, q.z], 4.0);
            assert!((v[i] as f64 - want).abs() < 1e-6, "{i}: {} vs {want}", v[i]);
        }
        assert!(v.iter().all(|x| (0.5..=1.5).contains(x)) && v.iter().any(|x| *x > 1.05) && v.iter().any(|x| *x < 0.95), "{v:?}");
        // The same again; another seed differs.
        assert_eq!(floats(&attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 4}), scene.clone()).unwrap(), "pscale"), v);
        assert_ne!(floats(&attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 5}), scene).unwrap(), "pscale"), v);
    }

    /// Octaves add finer detail and stay in range.
    #[test]
    fn octaves_add_detail_within_the_range() {
        let scene = Scene { point_sets: vec![grid()], ..Scene::default() };
        let one = floats(&attribute_noise(&json!({"name": "k", "center": 0.0, "amplitude": 1.0, "frequency": 3.0}), scene.clone()).unwrap(), "k");
        let four = floats(&attribute_noise(&json!({"name": "k", "center": 0.0, "amplitude": 1.0, "frequency": 3.0, "octaves": 4}), scene).unwrap(), "k");
        assert!(one.iter().zip(&four).any(|(a, b)| (a - b).abs() > 0.01), "different");
        assert!(four.iter().all(|v| v.abs() <= 1.0));
        // Its first octave weighs most: still near the one-octave noise.
        let diff: f32 = one.iter().zip(&four).map(|(a, b)| (a - b).abs()).sum::<f32>() / 100.0;
        assert!(diff < 0.5, "{diff}");
    }

    #[test]
    fn modes_add_and_multiply_and_vectors_vary_per_component() {
        let mut g = grid();
        g.attrs.set("w", Attr::Float(vec![2.0; 100]), 100).unwrap();
        let scene = Scene { point_sets: vec![g], ..Scene::default() };
        let set = floats(&attribute_noise(&json!({"name": "w"}), scene.clone()).unwrap(), "w");
        let add = floats(&attribute_noise(&json!({"name": "w", "mode": "add"}), scene.clone()).unwrap(), "w");
        let mul = floats(&attribute_noise(&json!({"name": "w", "mode": "multiply"}), scene.clone()).unwrap(), "w");
        for i in 0..100 {
            assert!((add[i] - (2.0 + set[i])).abs() < 1e-6 && (mul[i] - 2.0 * set[i]).abs() < 1e-6);
        }
        let out = attribute_noise(&json!({"name": "Cd", "kind": "vec4", "center": 0.5, "amplitude": 0.4}), scene.clone()).unwrap();
        let cd = out.point_sets[0].attrs.vec4("Cd").unwrap();
        assert!(cd.iter().any(|c| (c[0] - c[1]).abs() > 1e-3), "components differ");
        let wrong = attribute_noise(&json!({"name": "w", "kind": "vec3", "mode": "add"}), scene.clone()).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(wrong.contains("is a float"), "{wrong}");
        assert!(attribute_noise(&json!({"name": "P"}), scene).is_err());
    }

    #[test]
    fn a_falloff_region_limits_it_with_a_soft_edge_and_shows() {
        let mut g = PointSet { positions: (0..41).map(|i| Vec3::new(i as f64 * 0.1 - 2.0, 0.0, 0.0)).collect(), ..PointSet::default() };
        g.attrs.set("w", Attr::Float(vec![1.0; 41]), 41).unwrap();
        let scene = Scene { point_sets: vec![g], ..Scene::default() };
        // A 2-wide ball at 0 (radius 1), soft over its outer half.
        let p = json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": "sphere", "region_size": [2.0, 2.0, 2.0], "softness": 0.5});
        let out = attribute_noise(&p, scene.clone()).unwrap();
        let w = floats(&out, "w");
        let at = |x: f64| w[((x + 2.0) / 0.1).round() as usize];
        assert_eq!((at(0.0), at(0.4), at(1.5), at(-1.5)), (5.0, 5.0, 1.0, 1.0), "full inside, untouched outside");
        assert!(at(0.7) > 1.0 && at(0.7) < 5.0 && at(0.7) == at(-0.7), "between on the soft edge: {} {}", at(0.7), at(-0.7));
        assert_eq!(out.regions.len(), 1);
        assert!(out.regions[0].sphere);
        // A box instead: its corners count.
        let boxed = attribute_noise(&json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": "box", "region_size": [3.0, 1.0, 1.0]}), scene).unwrap();
        let w = floats(&boxed, "w");
        assert_eq!((w[((1.4 + 2.0) / 0.1f64).round() as usize], w[((1.6 + 2.0) / 0.1f64).round() as usize]), (5.0, 1.0));
        // Toward a corner: inside the box, outside the ball the same size.
        let mut corner = PointSet { positions: vec![Vec3::new(1.2, 0.4, 0.4)], ..PointSet::default() };
        corner.attrs.set("w", Attr::Float(vec![1.0]), 1).unwrap();
        let at = |shape: &str| {
            let p = json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": shape, "region_size": [3.0, 1.0, 1.0], "softness": 0.0});
            floats(&attribute_noise(&p, Scene { point_sets: vec![corner.clone()], ..Scene::default() }).unwrap(), "w")[0]
        };
        assert_eq!((at("box"), at("sphere")), (5.0, 1.0));
    }

    /// As a node in a graph, with its defaults (pscale around 1).
    #[test]
    fn the_node_in_a_graph() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "ground", "op": "plane", "params": {"size": [4, 4]}},
            {"id": "pts", "op": "scatter", "inputs": ["ground", "", ""], "params": {"density": 20}},
            {"id": "sizes", "op": "attribute_noise", "inputs": ["pts"], "params": {"frequency": 2}},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let out = crate::graph::Evaluator::new().evaluate(&graph, "sizes", &ctx).unwrap();
        let s = &out.scene.as_ref().unwrap().point_sets[0];
        let k = s.attrs.float("pscale").expect("pscale");
        assert!(k.len() > 100 && k.iter().all(|v| (0.5..=1.5).contains(v)), "{} points", k.len());
    }

    #[test]
    fn mesh_points_instances_and_groups() {
        let m = crate::mesh::plane([2.0, 2.0], [1, 1]);
        let model = scene::Model { node: "floor".into(), ..scene::Model::new(Arc::new(m), Mat4::identity(), Default::default()) };
        let inst = geo::Instances { worlds: vec![geom::trs([1.0, 2.0, 3.0], [0.0; 3], [1.0; 3]); 2], which: vec![0, 0], node: "copies".into(), ..geo::Instances::default() };
        let scene = Scene { meshes: vec![model], instances: vec![inst], point_sets: vec![grid()], ..Scene::default() };
        let out = attribute_noise(&json!({"name": "k", "elements": "mesh points"}), scene.clone()).unwrap();
        assert_eq!(out.meshes[0].mesh.point_attrs.float("k").map(<[f32]>::len), Some(4));
        assert!(out.point_sets[0].attrs.get("k").is_none(), "only the elements asked for");
        let out = attribute_noise(&json!({"name": "k", "elements": "instances"}), scene.clone()).unwrap();
        let k = out.instances[0].attrs.float("k").unwrap();
        assert_eq!(k[0], k[1], "at the same place");
        let out = attribute_noise(&json!({"name": "k", "group": "other*"}), scene).unwrap();
        assert!(out.point_sets[0].attrs.get("k").is_none(), "not in Group");
    }
}
