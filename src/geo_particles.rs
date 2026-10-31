//! Particles and point sets (docs/geo-plan.md, P6): Particles to Points
//! makes a particle stream's particles at the frame into a point set, so
//! they can drive geometry nodes (Instance on Points: debris on
//! particles). The other way, Points to Particles, is the Particle
//! Emitter's "point set" shape (see `particles::PointSource`). `ops.rs`
//! registers the node.

use crate::geo::{self, Attr, Attrs, PointSet};
use crate::particles::{Particle, Shape, NO_SOURCE};
use crate::scene::Scene;
use anyhow::Result;
use serde_json::Value;

/// A Particles to Points: each particle stream (of Group's node) as a
/// point set of its particles now, carrying what they are (see the node's
/// help in ops.rs); the streams taken out unless Keep particles is on.
pub fn particles_to_points(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let keep = params["keep_particles"].as_bool().unwrap_or(false);
    for stream in scene.particles.iter().filter(|s| take(&s.node)) {
        let particles = stream.particles();
        // The point sets the stream's emitters emit from (by emitter), for
        // the attributes their particles carry.
        let system = (stream.system)(stream.frame);
        let sources: Vec<Option<&crate::particles::PointSource>> = system.emitters.iter().map(|e| if let Shape::Points(s) = &e.shape { Some(&**s) } else { None }).collect();
        scene.point_sets.push(points_of(&particles, &sources));
    }
    if !keep {
        scene.particles.retain(|s| !take(&s.node));
    }
    Ok(scene)
}

/// `particles` as a point set: P; id, v, age, life, pscale, Cd (straight),
/// orient, spin; and, those born at a point of `sources[emitter]`, that
/// point's other attributes (and its id).
fn points_of(particles: &[Particle], sources: &[Option<&crate::particles::PointSource>]) -> PointSet {
    let n = particles.len();
    let f3 = |v: &crate::geom::Vec3| [v.x as f32, v.y as f32, v.z as f32];
    let source = |p: &Particle| {
        let e = (p.id >> 32) as usize;
        sources.get(e).copied().flatten().filter(|s| p.source != NO_SOURCE && (p.source as usize) < s.len())
    };
    // Each particle's id: its point's where it was born at one with an id,
    // else its own (the low bits: its birth count, its emitter above).
    let ids: Vec<i32> = particles
        .iter()
        .map(|p| match source(p).and_then(|s| s.attrs.int(geo::ID)) {
            Some(ids) => ids[p.source as usize],
            None => ((p.id >> 32) << 24 | (p.id & 0xff_ffff)) as i32,
        })
        .collect();
    let mut a = Attrs::default();
    let _ = a.set(geo::ID, Attr::Int(ids), n);
    let _ = a.set("v", Attr::Vec3(particles.iter().map(|p| f3(&p.velocity)).collect()), n);
    let _ = a.set("age", Attr::Float(particles.iter().map(|p| p.age as f32).collect()), n);
    let _ = a.set("life", Attr::Float(particles.iter().map(|p| p.life as f32).collect()), n);
    let _ = a.set(geo::PSCALE, Attr::Float(particles.iter().map(|p| p.size as f32).collect()), n);
    let _ = a.set(geo::CD, Attr::Vec4(particles.iter().map(|p| {
        let c = p.color;
        if c[3] > 0.0 { [c[0] / c[3], c[1] / c[3], c[2] / c[3], c[3]] } else { [0.0; 4] }
    }).collect()), n);
    let _ = a.set(geo::ORIENT, Attr::Vec4(particles.iter().map(|p| {
        let axis = nalgebra::Unit::try_new(p.axis, 1e-12).unwrap_or_else(nalgebra::Vector3::z_axis);
        let q = nalgebra::UnitQuaternion::from_axis_angle(&axis, p.rotation.to_radians());
        [q.i as f32, q.j as f32, q.k as f32, q.w as f32]
    }).collect()), n);
    let _ = a.set("spin", Attr::Float(particles.iter().map(|p| p.spin as f32).collect()), n);
    // The points' other attributes: each particle's point's (zeros for
    // those born elsewhere).
    let mut names: Vec<(String, Attr)> = Vec::new();
    for s in sources.iter().flatten() {
        for (name, attr) in s.attrs.iter() {
            if a.get(name).is_none() && !names.iter().any(|(n, _)| n == name) {
                names.push((name.to_string(), attr.zeros(0)));
            }
        }
    }
    for (name, kind) in names {
        let at = |p: &Particle| source(p).and_then(|s| s.attrs.get(&name)).map(|x| (x, p.source as usize));
        let v = match kind {
            Attr::Float(_) => Attr::Float(particles.iter().map(|p| if let Some((Attr::Float(v), i)) = at(p) { v[i] } else { 0.0 }).collect()),
            Attr::Vec3(_) => Attr::Vec3(particles.iter().map(|p| if let Some((Attr::Vec3(v), i)) = at(p) { v[i] } else { [0.0; 3] }).collect()),
            Attr::Vec4(_) => Attr::Vec4(particles.iter().map(|p| if let Some((Attr::Vec4(v), i)) = at(p) { v[i] } else { [0.0; 4] }).collect()),
            Attr::Int(_) => Attr::Int(particles.iter().map(|p| if let Some((Attr::Int(v), i)) = at(p) { v[i] } else { 0 }).collect()),
        };
        let _ = a.set(&name, v, n);
    }
    PointSet { positions: particles.iter().map(|p| p.position).collect(), attrs: a, ..PointSet::default() }
}

#[cfg(test)]
mod tests {
    use crate::graph::{Evaluator, Graph};
    use crate::ops::EvalContext;
    use serde_json::json;

    fn graph(extra: serde_json::Value) -> Graph {
        let mut nodes = json!([
            { "id": "ground", "op": "plane", "params": { "size": [4, 4], "rotate": [-90, 0, 0] } },
            { "id": "pts", "op": "scatter", "inputs": ["ground"], "params": { "density": 3 } },
            { "id": "marked", "op": "attribute_set", "inputs": ["pts"], "params": { "attr1_name": "w", "attr1_kind": "float", "attr1_expr": "@P.x", "attr2_name": "Cd", "attr2_kind": "vec4", "attr2_expr": "[1, 0, 0, 1]" } },
            { "id": "emit", "op": "particle_emitter", "inputs": ["marked"], "params": { "shape": "point set", "rate": 0, "start": 1, "speed": 0, "life": 1000 } },
            { "id": "fall", "op": "particle_gravity", "inputs": ["emit"], "params": { "acceleration": [0, -10, 0] } },
            { "id": "out", "op": "particles_to_points", "inputs": ["fall"] },
        ]);
        nodes.as_array_mut().unwrap().extend(extra.as_array().unwrap().iter().cloned());
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    fn scene(g: &Graph, node: &str, frame: i64) -> std::sync::Arc<crate::scene::Scene> {
        let img = Evaluator::new().evaluate(g, node, &EvalContext { frame, write_outputs: false }).unwrap();
        img.scene.clone().unwrap()
    }

    /// A scatter's points emit one particle each (Points to Particles),
    /// which fall; back as points they carry the scatter's ids, its
    /// attributes and the particles' own.
    #[test]
    fn points_become_particles_and_back() {
        let g = graph(json!([]));
        let src = scene(&g, "marked", 1).point_sets[0].clone();
        let out = scene(&g, "out", 11);
        assert!(out.particles.is_empty(), "the stream taken out");
        let ps = &out.point_sets[0];
        assert!(ps.validate().is_ok());
        assert_eq!(ps.len(), src.len(), "one each");
        let mut a = ps.attrs.int(crate::geo::ID).unwrap().to_vec();
        let mut b = src.attrs.int(crate::geo::ID).unwrap().to_vec();
        a.sort();
        b.sort();
        assert_eq!(a, b, "the scatter's ids");
        // Each carries its point's w (its x then) and red; it has fallen.
        let w = ps.attrs.float("w").unwrap();
        for (i, p) in ps.positions.iter().enumerate() {
            assert!((w[i] as f64 - p.x).abs() < 1e-4, "w {} at x {}", w[i], p.x);
            assert!(p.y < -0.4, "fallen: {}", p.y);
        }
        assert!(ps.attrs.vec4(crate::geo::CD).unwrap().iter().all(|c| (c[0] - 1.0).abs() < 1e-6 && c[1] == 0.0));
        assert!(ps.attrs.vec3("v").unwrap().iter().all(|v| v[1] < -1.0), "falling");
        assert!(ps.attrs.float("age").unwrap().iter().all(|a| *a > 9.0));
        // Keep particles: the stream stays too.
        let g = graph(json!([{ "id": "kept", "op": "particles_to_points", "inputs": ["fall"], "params": { "keep_particles": true } }]));
        assert_eq!(scene(&g, "kept", 3).particles.len(), 1);
        // Rate instead: particles only at the scatter's points.
        let g = graph(json!([
            { "id": "rain", "op": "particle_emitter", "inputs": ["marked"], "params": { "shape": "point set", "each_point_once": false, "rate": 20, "start": 1, "speed": 0, "life": 1000 } },
            { "id": "drops", "op": "particles_to_points", "inputs": ["rain"] },
        ]));
        let drops = &scene(&g, "drops", 4).point_sets[0];
        assert_eq!(drops.len(), 80);
        assert!(drops.positions.iter().all(|p| src.positions.iter().any(|q| (src.world.transform_point(&(*q).into()).coords - p).norm() < 1e-9)), "each at a scatter point");
    }

    /// An ordinary emitter's particles as points: their own ids and a
    /// unit orient; and they drive Instance on Points.
    #[test]
    fn particles_drive_instances() {
        let g = graph(json!([
            { "id": "spray", "op": "particle_emitter", "params": { "rate": 5, "speed": 2, "spread": 30, "spin": 90 } },
            { "id": "dots", "op": "particles_to_points", "inputs": ["spray"] },
            { "id": "rock", "op": "cube", "params": { "size": [0.1, 0.1, 0.1] } },
            { "id": "copies", "op": "instance_on_points", "inputs": ["dots", "rock"] },
        ]));
        let dots = scene(&g, "dots", 6);
        let ps = &dots.point_sets[0];
        assert_eq!(ps.len(), 30);
        let ids = ps.attrs.int(crate::geo::ID).unwrap();
        assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), 30, "distinct ids");
        assert!(ps.attrs.vec4(crate::geo::ORIENT).unwrap().iter().all(|q| ((q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]) - 1.0).abs() < 1e-5));
        let copies = scene(&g, "copies", 6);
        assert_eq!(copies.instances[0].len(), 30);
    }
}
