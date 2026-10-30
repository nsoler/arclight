//! 3D scenes: what the Camera, Card and Scene nodes output and a Scanline
//! Render draws. A scene travels through the graph attached to an
//! [`Image`] (its [`scene`](Image::scene)), whose pixels are empty, so the
//! cache, the worker and the viewer handle it like any other node's output;
//! the graph checks ports' kinds so a scene only goes where one is wanted
//! (see [`crate::ops::Kind`]).

use crate::geom::{self, Camera, Mat4, Vec3};
use crate::image::Image;
use crate::mesh::{Material, Mesh};
use crate::shade::{Light, LightKind};
use serde_json::Value;
use std::sync::Arc;

/// Cameras, textured cards, meshes, lights and points, placed in one space
/// (see [`crate::geom`] for its conventions).
#[derive(Clone, Default)]
pub struct Scene {
    pub cameras: Vec<Camera>,
    pub cards: Vec<Card>,
    pub meshes: Vec<Model>,
    /// Lights (each knows its node); with none, everything renders unlit.
    pub lights: Vec<Light>,
    /// A point cloud (a camera tracker's), drawn as dots.
    pub points: Vec<Vec3>,
    /// The node each camera and point came from, in the same order (filled
    /// in by the graph after each node renders, see [`Scene::tag`]): the
    /// 3D viewport selects objects by node.
    pub camera_nodes: Vec<String>,
    pub point_nodes: Vec<String>,
    /// Each point's reprojection error in pixels, when a solve gave one
    /// (else negative), for coloring the cloud.
    pub point_errors: Vec<f32>,
    /// Particle streams (simulated when drawn; each knows its node).
    pub particles: Vec<crate::particle_nodes::ParticleStream>,
}

/// An image on a flat rectangle in 3D: `size` wide and high in its own
/// space, centered on its origin, facing +Z, then placed by `world`.
#[derive(Clone)]
pub struct Card {
    pub image: Arc<Image>,
    pub world: Mat4,
    pub size: [f64; 2],
    /// How opaque it is (0 to 1): its image, all channels, times this.
    pub opacity: f32,
    /// Whether it blocks lights that cast shadows (as much as it is
    /// opaque).
    pub casts_shadows: bool,
    /// The Card node it came from ("" until tagged).
    pub node: String,
}

/// A mesh placed in the scene: a primitive's or a file's.
#[derive(Clone)]
pub struct Model {
    pub mesh: Arc<Mesh>,
    pub world: Mat4,
    pub material: Material,
    /// The image the material's color is multiplied by (a primitive's
    /// texture input, or the file's texture), if any.
    pub texture: Option<Arc<Image>>,
    /// Renders nothing, but cuts out what is behind it.
    pub holdout: bool,
    /// Whether it blocks lights that cast shadows, and whether shadows
    /// fall on it.
    pub casts_shadows: bool,
    pub receives_shadows: bool,
    /// Its name within the node (a file's object), and the node it came
    /// from ("" until tagged).
    pub name: String,
    pub node: String,
}

impl Model {
    /// `mesh` placed by `world` with `material`: untextured, not a
    /// holdout, casting and receiving shadows, unnamed.
    pub fn new(mesh: Arc<Mesh>, world: Mat4, material: Material) -> Model {
        Model { mesh, world, material, texture: None, holdout: false, casts_shadows: true, receives_shadows: true, name: String::new(), node: String::new() }
    }

    /// Its holdout and shadow flags from a node's params (Holdout, Casts
    /// shadows, Receives shadows; missing: off, on, on).
    pub fn flags(self, p: &Value) -> Model {
        let on = |n: &str, d: bool| p[n].as_bool().unwrap_or(d);
        Model { holdout: on("holdout", false), casts_shadows: on("casts_shadows", true), receives_shadows: on("receives_shadows", true), ..self }
    }

    /// A box around it in scene space (its mesh's box, placed), or None for
    /// an empty mesh.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let (lo, hi) = self.mesh.bounds()?;
        let corners = (0..8).map(|i| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }));
        let placed: Vec<Vec3> = corners.map(|c| geom::transform_point(&self.world, &c)).collect();
        let fold = |f: fn(f64, f64) -> f64, init: f64| Vec3::new(placed.iter().map(|p| p.x).fold(init, f), placed.iter().map(|p| p.y).fold(init, f), placed.iter().map(|p| p.z).fold(init, f));
        Some((fold(f64::min, f64::INFINITY), fold(f64::max, f64::NEG_INFINITY)))
    }
}

impl Card {
    /// `image` on a card `size` across placed by `world`, opaque, casting
    /// shadows.
    pub fn new(image: Arc<Image>, world: Mat4, size: [f64; 2]) -> Card {
        Card { image, world, size, opacity: 1.0, casts_shadows: true, node: String::new() }
    }

    /// The four corners in scene space: top left, top right, bottom right,
    /// bottom left (as the image's own corners).
    pub fn corners(&self) -> [Vec3; 4] {
        let [w, h] = self.size.map(|v| v / 2.0);
        [[-w, h], [w, h], [w, -h], [-w, -h]].map(|[x, y]| geom::transform_point(&self.world, &Vec3::new(x, y, 0.0)))
    }
}

impl Scene {
    /// Everything in `self`, then everything in `other`.
    pub fn extend(&mut self, other: &Scene) {
        self.fill_nodes();
        self.cameras.extend(other.cameras.iter().cloned());
        self.cards.extend(other.cards.iter().cloned());
        self.meshes.extend(other.meshes.iter().cloned());
        self.lights.extend(other.lights.iter().cloned());
        self.points.extend(other.points.iter().copied());
        self.particles.extend(other.particles.iter().cloned());
        let mut other = other.clone();
        other.fill_nodes();
        self.camera_nodes.extend(other.camera_nodes);
        self.point_nodes.extend(other.point_nodes);
        self.point_errors.extend(other.point_errors);
    }

    /// Pad the node lists to the cameras and points ("" for unknown).
    fn fill_nodes(&mut self) {
        self.camera_nodes.resize(self.cameras.len(), String::new());
        self.point_nodes.resize(self.points.len(), String::new());
        self.point_errors.resize(self.points.len(), -1.0);
    }

    /// Mark everything not yet marked as coming from node `id` (what a
    /// node made itself; what it gathered from upstream is marked already).
    pub fn tag(&mut self, id: &str) {
        self.fill_nodes();
        for n in self.camera_nodes.iter_mut().chain(self.point_nodes.iter_mut()).chain(self.cards.iter_mut().map(|c| &mut c.node)).chain(self.meshes.iter_mut().map(|m| &mut m.node)).chain(self.lights.iter_mut().map(|l| &mut l.node)) {
            if n.is_empty() {
                *n = id.to_string();
            }
        }
    }

    /// The node camera `i` came from ("" when unknown).
    pub fn camera_node(&self, i: usize) -> &str {
        self.camera_nodes.get(i).map_or("", String::as_str)
    }

    /// The node point `i` came from ("" when unknown).
    pub fn point_node(&self, i: usize) -> &str {
        self.point_nodes.get(i).map_or("", String::as_str)
    }

    /// "1 camera, 2 cards", for the viewer and stats.
    pub fn describe(&self) -> String {
        let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        let mut parts = vec![n(self.cameras.len(), "camera", "cameras"), n(self.cards.len(), "card", "cards")];
        if !self.meshes.is_empty() {
            parts.push(n(self.meshes.len(), "mesh", "meshes"));
        }
        if !self.lights.is_empty() {
            parts.push(n(self.lights.len(), "light", "lights"));
        }
        if !self.points.is_empty() {
            parts.push(n(self.points.len(), "point", "points"));
        }
        if !self.particles.is_empty() {
            parts.push(n(self.particles.len(), "particle stream", "particle streams"));
        }
        parts.join(", ")
    }
}

/// Read a param as `N` numbers (missing ones: `default`).
pub fn numbers<const N: usize>(v: &Value, default: f64) -> [f64; N] {
    std::array::from_fn(|i| v.get(i).and_then(Value::as_f64).unwrap_or(default))
}

/// The placement of a node with `translate`, `rotate` and `scale` (Vec3)
/// params.
pub fn placement(p: &Value) -> Mat4 {
    geom::trs(numbers(&p["translate"], 0.0), numbers(&p["rotate"], 0.0), numbers(&p["scale"], 1.0))
}

/// A Camera node's camera, from its resolved params.
pub fn camera(p: &Value) -> Camera {
    let film = numbers::<2>(&p["film_back"], 0.0);
    let d = Camera::default();
    Camera {
        focal: p["focal"].as_f64().filter(|f| *f > 0.0).unwrap_or(d.focal),
        film_back: [if film[0] > 0.0 { film[0] } else { d.film_back[0] }, if film[1] > 0.0 { film[1] } else { d.film_back[1] }],
        world: placement(p),
        near: p["near"].as_f64().filter(|v| *v > 0.0).unwrap_or(d.near),
        far: p["far"].as_f64().filter(|v| *v > 0.0).unwrap_or(d.far),
    }
}

/// A Card node's card showing `image`. With no `size` (null), it is 1 wide
/// and as high as the image's frame aspect makes it; Opacity (missing: 1)
/// scales the image.
pub fn card(p: &Value, image: &Image) -> Card {
    let size = match p["size"].as_array() {
        Some(_) => numbers::<2>(&p["size"], 1.0),
        None => [1.0, image.format.height as f64 / image.format.width.max(1) as f64],
    };
    let opacity = p["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    Card { opacity, casts_shadows: p["casts_shadows"].as_bool().unwrap_or(true), ..Card::new(Arc::new(image.clone()), placement(p), size) }
}

/// The surface look a primitive's params give: Color (straight RGBA), and
/// how it takes light (see [`lighting`]).
pub fn material(p: &Value) -> Material {
    let c = numbers::<4>(&p["color"], 1.0).map(|v| v as f32);
    lighting(p, &Material { color: [c[0], c[1], c[2]], opacity: c[3], ..Material::default() })
}

/// `base` with the lighting params set: Shading (unlit, lambert, blinn;
/// missing: Lambert, which shows its color as it is with no lights),
/// Specular (a gray highlight strength), Roughness and Emission (RGB).
pub fn lighting(p: &Value, base: &Material) -> Material {
    use crate::mesh::Shading;
    let shading = match p["shading"].as_str() {
        Some("unlit") => Shading::Unlit,
        Some("blinn") => Shading::Blinn,
        _ => Shading::Lambert,
    };
    let specular = p["specular"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    let e = numbers::<3>(&p["emission"], 0.0).map(|v| v.max(0.0) as f32);
    Material { shading, specular: [specular; 3], roughness: p["roughness"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0) as f32, emission: e, ..base.clone() }
}

/// A primitive node's mesh (`op` one of cube, sphere, cylinder, plane),
/// from its resolved params, in its own space.
pub fn primitive(op: &str, p: &Value) -> Option<Mesh> {
    use crate::mesh;
    // Segment counts (missing ones: the node's defaults).
    let segs = |d: [f64; 3]| -> [usize; 3] { std::array::from_fn(|i| p["segments"].get(i).and_then(Value::as_f64).unwrap_or(d[i]).round().clamp(1.0, 512.0) as usize) };
    let num = |n: &str, d: f64| p[n].as_f64().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(d);
    Some(match op {
        "cube" => mesh::cube(numbers::<3>(&p["size"], 1.0), segs([1.0; 3])),
        "sphere" => {
            let s = segs([32.0, 16.0, 1.0]);
            mesh::sphere(num("radius", 0.5), [s[0].max(3), s[1].max(2)])
        }
        "cylinder" => {
            let s = segs([32.0, 1.0, 1.0]);
            mesh::cylinder(num("radius", 0.5), num("height", 1.0), [s[0].max(3), s[1]], p["caps"].as_bool().unwrap_or(true))
        }
        "plane" => {
            let s = segs([1.0; 3]);
            mesh::plane(numbers::<2>(&p["size"], 1.0), [s[0], s[1]])
        }
        _ => return None,
    })
}

/// A primitive node's scene object: its mesh placed by its transform, with
/// its color, textured by `texture` when connected.
pub fn primitive_model(op: &str, p: &Value, texture: Option<&Image>) -> Option<Model> {
    let m = Model::new(Arc::new(primitive(op, p)?), placement(p), material(p)).flags(p);
    Some(Model { texture: texture.map(|t| Arc::new(t.clone())), ..m })
}

/// A light node's light (`op` one of light_ambient, light_directional,
/// light_point, light_spot, light_area), from its resolved params: placed
/// by its translate and rotate, shining down its −Z, or if `target` (its
/// Look at input) holds something, turned to aim at that (see [`aim`]).
/// Exposure (stops) scales the intensity; Enabled off keeps the light in
/// the scene, drawn but dark.
pub fn light(op: &str, p: &Value, target: Option<&Scene>) -> Option<Light> {
    let num = |n: &str, d: f64| p[n].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let kind = match op {
        "light_ambient" => LightKind::Ambient,
        "light_directional" => LightKind::Directional,
        "light_point" => LightKind::Point,
        "light_spot" => LightKind::Spot { cone: num("cone", 40.0).clamp(0.0, 179.0), penumbra: num("penumbra", 5.0).max(0.0), falloff: num("falloff", 0.0).max(0.0) },
        "light_area" => LightKind::Area {
            size: numbers::<2>(&p["size"], 1.0).map(|v| v.abs()),
            disk: p["shape"].as_str() == Some("disk"),
            two_sided: p["two_sided"].as_bool().unwrap_or(false),
            samples: num("quality", 4.0).round().clamp(1.0, 16.0) as usize,
        },
        _ => return None,
    };
    let c = numbers::<3>(&p["color"], 1.0).map(|v| v as f32);
    let at = numbers(&p["translate"], 0.0);
    let mut world = geom::trs(at, numbers(&p["rotate"], 0.0), [1.0; 3]);
    if let Some(to) = target.and_then(Scene::aim_point) {
        world = aim(Vec3::from(at), to).unwrap_or(world);
    }
    Some(Light {
        kind,
        color: c,
        intensity: (num("intensity", 1.0) * num("exposure", 0.0).exp2()) as f32,
        decay: num("decay", 0.0).max(0.0),
        radius: num("radius", 0.0).max(0.0),
        enabled: p["enabled"].as_bool().unwrap_or(true),
        shadows: p["shadows"].as_bool().unwrap_or(false),
        shadow_softness: num("shadow_softness", 1.0).clamp(0.0, 20.0),
        world,
        node: String::new(),
    })
}

/// Placed at `from` with its −Z pointing at `to` and its Y as near up as
/// that allows (None if they're the same point).
pub fn aim(from: Vec3, to: Vec3) -> Option<Mat4> {
    let dir = (to - from).try_normalize(1e-12)?;
    let up = if dir.y.abs() > 0.999 { -Vec3::z() * dir.y.signum() } else { Vec3::y() };
    // face_towards turns +Z toward its target, so −Z goes the other way.
    let r = nalgebra::Rotation3::face_towards(&-dir, &up);
    Some(Mat4::new_translation(&from) * r.to_homogeneous())
}

impl Scene {
    /// Where something aiming at this scene aims: its first camera's
    /// position, else the middle of its first card, mesh, point (a
    /// locator) or light.
    pub fn aim_point(&self) -> Option<Vec3> {
        let origin = |m: &Mat4| geom::transform_point(m, &Vec3::zeros());
        self.cameras
            .first()
            .map(|c| origin(&c.world))
            .or_else(|| self.cards.first().map(|c| origin(&c.world)))
            .or_else(|| self.meshes.first().map(|m| m.bounds().map_or_else(|| origin(&m.world), |(lo, hi)| (lo + hi) / 2.0)))
            .or_else(|| self.points.first().copied())
            .or_else(|| self.lights.first().map(Light::position))
    }

    /// The scene `t` frames after this one (for motion blur), from
    /// `frames`: the scenes at whole frames from this one (0 is this one).
    /// Between the two round `t`, each object's placement (and a deforming
    /// mesh's vertices, a camera's focal length, the points) goes from one
    /// to the other, where both list the same objects from the same nodes;
    /// otherwise it stays as here. Particles move on along their
    /// velocities. Everything else is as here.
    pub fn at_time(&self, frames: &[(i64, &Scene)], t: f64) -> Scene {
        let mut out = self.clone();
        out.particles = self.particles.iter().map(|s| s.shifted(t)).collect();
        let k = t.floor();
        let w = t - k;
        let find = |k: i64| frames.iter().find(|f| f.0 == k).map(|f| f.1);
        let (Some(a), Some(b)) = (find(k as i64), find(k as i64 + 1).or_else(|| (w == 0.0).then(|| find(k as i64)).flatten())) else { return out };
        let mut a = a.clone();
        let mut b = b.clone();
        a.fill_nodes();
        b.fill_nodes();
        out.fill_nodes();
        if a.camera_nodes == out.camera_nodes && b.camera_nodes == out.camera_nodes {
            for (c, (x, y)) in out.cameras.iter_mut().zip(a.cameras.iter().zip(&b.cameras)) {
                c.world = between(&x.world, &y.world, w);
                c.focal = x.focal + (y.focal - x.focal) * w;
            }
        }
        if same_nodes(&out.cards, &a.cards, &b.cards, |c| &c.node) {
            for (c, (x, y)) in out.cards.iter_mut().zip(a.cards.iter().zip(&b.cards)) {
                c.world = between(&x.world, &y.world, w);
            }
        }
        if same_nodes(&out.meshes, &a.meshes, &b.meshes, |m| &m.node) {
            for (m, (x, y)) in out.meshes.iter_mut().zip(a.meshes.iter().zip(&b.meshes)) {
                m.world = between(&x.world, &y.world, w);
                let (p, q) = (&x.mesh.positions, &y.mesh.positions);
                if !Arc::ptr_eq(&x.mesh, &y.mesh) && p.len() == q.len() && p.len() == m.mesh.positions.len() && p != q {
                    let lerp = |u: &[Vec3], v: &[Vec3]| u.iter().zip(v).map(|(u, v)| u + (v - u) * w).collect::<Vec<_>>();
                    let normals = if x.mesh.normals.len() == y.mesh.normals.len() && x.mesh.normals.len() == m.mesh.normals.len() {
                        lerp(&x.mesh.normals, &y.mesh.normals).into_iter().map(|n| n.try_normalize(1e-300).unwrap_or(n)).collect()
                    } else {
                        m.mesh.normals.clone()
                    };
                    m.mesh = Arc::new(Mesh { positions: lerp(p, q), normals, ..(*m.mesh).clone() });
                }
            }
        }
        if same_nodes(&out.lights, &a.lights, &b.lights, |l| &l.node) {
            for (l, (x, y)) in out.lights.iter_mut().zip(a.lights.iter().zip(&b.lights)) {
                l.world = between(&x.world, &y.world, w);
            }
        }
        if a.point_nodes == out.point_nodes && b.point_nodes == out.point_nodes {
            for (p, (x, y)) in out.points.iter_mut().zip(a.points.iter().zip(&b.points)) {
                *p = x + (y - x) * w;
            }
        }
        out
    }
}

/// Whether `x`, `a` and `b` list as many things, from the same nodes in
/// the same order.
fn same_nodes<T>(x: &[T], a: &[T], b: &[T], node: impl Fn(&T) -> &String) -> bool {
    let same = |u: &[T], v: &[T]| u.len() == v.len() && u.iter().zip(v).all(|(u, v)| node(u) == node(v));
    same(x, a) && same(x, b)
}

/// The placement `w` of the way from `a` to `b` (0: `a`, 1: `b`): the
/// move blended, the turn slerped, the scale (and shear) blended; with a
/// mirror in either, element by element.
pub fn between(a: &Mat4, b: &Mat4, w: f64) -> Mat4 {
    use nalgebra::{Matrix3, Rotation3, UnitQuaternion};
    if w == 0.0 || a == b {
        return *a;
    }
    if w == 1.0 {
        return *b;
    }
    let mut out = a + (b - a) * w;
    let linear = |m: &Mat4| -> Matrix3<f64> { m.fixed_view::<3, 3>(0, 0).into_owned() };
    let (la, lb) = (linear(a), linear(b));
    if la.determinant() > 0.0 && lb.determinant() > 0.0 {
        // Each as a turn times a stretch (polar decomposition).
        let split = |l: &Matrix3<f64>| {
            let r = Rotation3::from_matrix_eps(l, 1e-12, 100, Rotation3::identity());
            (UnitQuaternion::from_rotation_matrix(&r), r.matrix().transpose() * l)
        };
        let ((qa, sa), (qb, sb)) = (split(&la), split(&lb));
        let q = qa.try_slerp(&qb, w, 1e-9).unwrap_or(if w < 0.5 { qa } else { qb });
        let l = q.to_rotation_matrix().matrix() * (sa + (sb - sa) * w);
        out.fixed_view_mut::<3, 3>(0, 0).copy_from(&l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placements_and_scenes_between_frames() {
        // Halfway from still to 90° about Y, 2 right and twice as big: 45°,
        // 1 right, 1.5 times.
        let (a, b) = (geom::trs([0.0; 3], [0.0; 3], [1.0; 3]), geom::trs([2.0, 0.0, 0.0], [0.0, 90.0, 0.0], [2.0; 3]));
        let want = geom::trs([1.0, 0.0, 0.0], [0.0, 45.0, 0.0], [1.5; 3]);
        assert!((between(&a, &b, 0.5) - want).abs().max() < 1e-9, "{}", between(&a, &b, 0.5));
        assert_eq!((between(&a, &b, 0.0), between(&a, &b, 1.0)), (a, b));
        // A scene a quarter frame on and back: its card and camera between
        // the frames' (the one before for negative times); a card that
        // only one frame has stays put.
        let img = Arc::new(Image::filled(2, 2, [1.0; 4]));
        let at = |x: f64| Scene {
            cards: vec![Card { node: "c".into(), ..Card::new(img.clone(), geom::trs([x, 0.0, 0.0], [0.0; 3], [1.0; 3]), [1.0, 1.0]) }],
            cameras: vec![Camera { world: geom::trs([0.0, x, 0.0], [0.0; 3], [1.0; 3]), ..Camera::default() }],
            ..Scene::default()
        };
        let (before, now, after) = (at(-4.0), at(0.0), at(8.0));
        let frames = [(-1, &before), (0, &now), (1, &after)];
        let x = |s: &Scene| (s.cards[0].world[(0, 3)], s.cameras[0].world[(1, 3)]);
        assert_eq!(x(&now.at_time(&frames, 0.25)), (2.0, 2.0));
        assert_eq!(x(&now.at_time(&frames, -0.25)), (-1.0, -1.0));
        assert_eq!(x(&now.at_time(&frames, 0.0)), (0.0, 0.0));
        let mut more = after.clone();
        more.cards.push(more.cards[0].clone());
        assert_eq!(x(&now.at_time(&[(0, &now), (1, &more)], 0.5)).0, 0.0);
    }

    #[test]
    fn cards_and_cameras_from_params() {
        let img = Image::new(200, 100);
        // Default size: 1 wide, the image's aspect high.
        let c = card(&json!({ "translate": [0, 0, -5], "rotate": [0, 0, 0], "scale": [1, 1, 1], "size": null }), &img);
        assert_eq!(c.size, [1.0, 0.5]);
        let tl = c.corners()[0];
        assert!((tl - Vec3::new(-0.5, 0.25, -5.0)).norm() < 1e-12, "{tl}");
        // Turned 90° about Y, scaled twice: its right edge points down −Z.
        let c = card(&json!({ "translate": [0, 0, 0], "rotate": [0, 90, 0], "scale": [2, 2, 2], "size": [1, 1] }), &img);
        let tr = c.corners()[1];
        assert!((tr - Vec3::new(0.0, 1.0, -1.0)).norm() < 1e-12, "{tr}");
        let cam = camera(&json!({ "focal": 50, "film_back": [36, 24], "translate": [0, 1, 5], "near": 0.5 }));
        assert_eq!((cam.focal, cam.near, cam.world[(1, 3)]), (50.0, 0.5, 1.0));
        let mut s = Scene { cameras: vec![cam], ..Scene::default() };
        s.extend(&Scene { cards: vec![c.clone(), c], ..Scene::default() });
        assert_eq!(s.describe(), "1 camera, 2 cards");
    }

    /// Light nodes make scenes of one light each, placed and shaped by
    /// their params; a Scene gathers them with everything else.
    #[test]
    fn light_nodes_make_lights() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        let g: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "fill", "op": "light_ambient", "params": { "color": [0.2, 0.3, 0.4, 1], "intensity": 0.5 } },
            { "id": "sun", "op": "light_directional", "params": { "rotate": [-90, 0, 0] } },
            { "id": "bulb", "op": "light_point", "params": { "translate": [1, 2, 3], "decay": 2 } },
            { "id": "spot", "op": "light_spot", "params": { "translate": [0, 5, 0], "rotate": [-90, 0, 0], "cone": 30, "penumbra": 4 } },
            { "id": "box", "op": "cube", "params": {} },
            { "id": "lit", "op": "scene", "inputs": ["fill", "sun", "bulb", "spot"] },
            { "id": "all", "op": "scene", "inputs": ["lit", "box", "", ""] },
        ] })).unwrap();
        let mut ev = Evaluator::new();
        let out = ev.evaluate(&g, "all", &EvalContext { frame: 1, write_outputs: false }).unwrap();
        let s = out.scene.as_ref().unwrap();
        assert_eq!(s.describe(), "0 cameras, 0 cards, 1 mesh, 4 lights");
        let nodes: Vec<&str> = s.lights.iter().map(|l| l.node.as_str()).collect();
        assert_eq!(nodes, ["fill", "sun", "bulb", "spot"], "each light knows its node");
        let (fill, sun, bulb, spot) = (&s.lights[0], &s.lights[1], &s.lights[2], &s.lights[3]);
        assert_eq!((fill.kind, fill.color, fill.intensity), (LightKind::Ambient, [0.2, 0.3, 0.4], 0.5));
        assert!(sun.kind == LightKind::Directional && (sun.direction() + Vec3::y()).norm() < 1e-12, "turned to shine down");
        assert_eq!((bulb.kind, bulb.position(), bulb.decay), (LightKind::Point, Vec3::new(1.0, 2.0, 3.0), 2.0));
        assert_eq!(spot.kind, LightKind::Spot { cone: 30.0, penumbra: 4.0, falloff: 0.0 });
        // Defaults: white, intensity 1, no falloff.
        let d = light("light_point", &json!({}), None).unwrap();
        assert_eq!((d.color, d.intensity, d.decay, d.radius, d.enabled), ([1.0; 3], 1.0, 0.0, 0.0, true));
        assert!(light("cube", &json!({}), None).is_none());
    }

    /// The Area Light's shape params, and exposure, enabled and radius on
    /// every light.
    #[test]
    fn area_lights_and_the_shared_light_params() {
        let a = light("light_area", &json!({ "size": [2, 0.5], "shape": "disk", "two_sided": true, "quality": 6 }), None).unwrap();
        assert_eq!(a.kind, LightKind::Area { size: [2.0, 0.5], disk: true, two_sided: true, samples: 6 });
        let a = light("light_area", &json!({}), None).unwrap();
        assert_eq!(a.kind, LightKind::rect([1.0, 1.0]), "a 1 × 1 one-sided rectangle, 4 × 4 samples");
        let s = light("light_spot", &json!({ "intensity": 3, "exposure": 2, "enabled": false, "radius": 0.25, "falloff": 1.5 }), None).unwrap();
        assert_eq!((s.intensity, s.enabled, s.radius), (12.0, false, 0.25), "two stops up: four times");
        assert!(matches!(s.kind, LightKind::Spot { falloff: 1.5, .. }));
        assert_eq!(light("light_point", &json!({ "exposure": -1 }), None).unwrap().intensity, 0.5);
    }

    /// A light with something in its Look at input turns to aim at it,
    /// whatever its Rotate; without, Rotate turns it.
    #[test]
    fn lights_look_at_their_target() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        let g: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "loc", "op": "locator", "params": { "translate": [3, 0, 0] } },
            { "id": "cam", "op": "camera", "params": { "translate": [0, 0, 10] } },
            { "id": "spot", "op": "light_spot", "params": { "translate": [0, 4, 0], "rotate": [0, 45, 0] }, "inputs": ["loc"] },
            { "id": "panel", "op": "light_area", "params": { "translate": [0, 0, 0] }, "inputs": ["cam"] },
            { "id": "free", "op": "light_spot", "params": { "rotate": [-90, 0, 0] } },
            { "id": "all", "op": "scene", "inputs": ["spot", "panel", "free", ""] },
        ] })).unwrap();
        let mut ev = Evaluator::new();
        let out = ev.evaluate(&g, "all", &EvalContext { frame: 1, write_outputs: false }).unwrap();
        let s = out.scene.as_ref().unwrap();
        assert_eq!(s.describe(), "0 cameras, 0 cards, 3 lights", "the targets aren't in the lights' scenes");
        let dir = |i: usize| s.lights[i].direction();
        assert!((dir(0) - Vec3::new(3.0, -4.0, 0.0) / 5.0).norm() < 1e-9, "{}", dir(0));
        assert!(s.lights[0].axis(1).y > 0.0, "kept upright");
        assert!((dir(1) - Vec3::z()).norm() < 1e-9 && (s.lights[1].position()).norm() < 1e-12);
        assert!((dir(2) + Vec3::y()).norm() < 1e-9);
        // Straight down still works (up can't be Y there).
        let m = aim(Vec3::new(0.0, 5.0, 0.0), Vec3::zeros()).unwrap();
        assert!((Light::new(LightKind::Point, m).direction() + Vec3::y()).norm() < 1e-9);
        assert!(aim(Vec3::zeros(), Vec3::zeros()).is_none());
    }

    /// Camera and Card nodes make scenes, a Scene node gathers them, and
    /// the graph and the session keep scenes out of image ports and images
    /// out of scene ports.
    #[test]
    fn scenes_travel_along_typed_wires() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        let g: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "plate", "op": "constant", "params": { "width": 64, "height": 32 } },
            { "id": "cam", "op": "camera", "params": { "focal": 50, "translate": [0, 0, 5] } },
            { "id": "card1", "op": "card", "inputs": ["plate"], "params": { "translate": [1, 0, 0] } },
            { "id": "all", "op": "scene", "inputs": ["cam", "card1", "", ""] },
            { "id": "bad", "op": "grade", "inputs": ["all", ""] },
            { "id": "bad2", "op": "scene", "inputs": ["plate", "", "", ""] },
        ] })).unwrap();
        let mut ev = Evaluator::new();
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let out = ev.evaluate(&g, "all", &ctx).unwrap();
        let s = out.scene.as_ref().unwrap();
        assert_eq!((s.describe(), s.cameras[0].focal), ("1 camera, 1 card".to_string(), 50.0));
        assert_eq!(s.cards[0].size, [1.0, 0.5], "the plate's aspect");
        assert_eq!((s.camera_node(0), s.cards[0].node.as_str()), ("cam", "card1"), "each object knows its node");
        assert_eq!(s.cards[0].image.format, crate::image::Size { width: 64, height: 32 });
        assert!(out.window().is_empty() && out.bytes() > 64 * 32 * 16, "no pixels, but the card's texture counts");
        let e = ev.evaluate(&g, "bad", &ctx).err().unwrap();
        assert_eq!(e.node.as_deref(), Some("bad"));
        assert!(e.message.contains("takes an image, but 'all' outputs a scene"), "{}", e.message);
        let e = ev.evaluate(&g, "bad2", &ctx).err().unwrap();
        assert!(e.message.contains("takes a scene, but 'plate' outputs an image"), "{}", e.message);
        // The session refuses such wires up front, and says what ports take.
        let mut s = crate::session::Session::new(g);
        assert!(s.connect("plate", "all", 2).unwrap_err().to_string().contains("takes a scene, not an image"));
        assert!(s.connect("cam", "card1", 0).is_err());
        s.connect("card1", "all", 2).unwrap();
        assert_eq!(s.describe_node("all").unwrap()["inputs"][0]["kind"], "scene");
        // Stats name what's in a scene.
        let stats = crate::session::image_stats(&out);
        assert_eq!(stats["scene"], "1 camera, 1 card");
    }
}
