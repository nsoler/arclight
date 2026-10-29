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
}

/// An image on a flat rectangle in 3D: `size` wide and high in its own
/// space, centered on its origin, facing +Z, then placed by `world`.
#[derive(Clone)]
pub struct Card {
    pub image: Arc<Image>,
    pub world: Mat4,
    pub size: [f64; 2],
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
    /// Its name within the node (a file's object), and the node it came
    /// from ("" until tagged).
    pub name: String,
    pub node: String,
}

impl Model {
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
/// and as high as the image's frame aspect makes it.
pub fn card(p: &Value, image: &Image) -> Card {
    let size = match p["size"].as_array() {
        Some(_) => numbers::<2>(&p["size"], 1.0),
        None => [1.0, image.format.height as f64 / image.format.width.max(1) as f64],
    };
    Card { image: Arc::new(image.clone()), world: placement(p), size, node: String::new() }
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
    Some(Model {
        mesh: Arc::new(primitive(op, p)?),
        world: placement(p),
        material: material(p),
        texture: texture.map(|t| Arc::new(t.clone())),
        holdout: p["holdout"].as_bool().unwrap_or(false),
        name: String::new(),
        node: String::new(),
    })
}

/// A light node's light (`op` one of light_ambient, light_directional,
/// light_point, light_spot), from its resolved params: placed by its
/// translate and rotate, shining down its −Z.
pub fn light(op: &str, p: &Value) -> Option<Light> {
    let num = |n: &str, d: f64| p[n].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let kind = match op {
        "light_ambient" => LightKind::Ambient,
        "light_directional" => LightKind::Directional,
        "light_point" => LightKind::Point,
        "light_spot" => LightKind::Spot { cone: num("cone", 40.0).clamp(0.0, 179.0), penumbra: num("penumbra", 5.0).max(0.0) },
        _ => return None,
    };
    let c = numbers::<3>(&p["color"], 1.0).map(|v| v as f32);
    let world = geom::trs(numbers(&p["translate"], 0.0), numbers(&p["rotate"], 0.0), [1.0; 3]);
    Some(Light { kind, color: c, intensity: num("intensity", 1.0) as f32, decay: num("decay", 0.0).max(0.0), world, node: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(spot.kind, LightKind::Spot { cone: 30.0, penumbra: 4.0 });
        // Defaults: white, intensity 1, no falloff.
        let d = light("light_point", &json!({})).unwrap();
        assert_eq!((d.color, d.intensity, d.decay), ([1.0; 3], 1.0, 0.0));
        assert!(light("cube", &json!({})).is_none());
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
