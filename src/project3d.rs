//! Project 3D (docs/geo-plan.md, P8): an image projected from a camera
//! onto the scene's meshes and cards, per rendered sample, as a slide
//! projector would (set extensions, patches over a plate). The node adds
//! a [`Projection`] to the scene, naming the nodes whose surfaces take it;
//! instances carry theirs themselves (see
//! [`Instances::projections`](crate::geo::Instances::projections)). The
//! renderer (`render3d`) projects each shaded sample's point through
//! the projector's camera into the image, before lighting. `ops.rs`
//! registers the node; the graph hands it its projector and image at the
//! Hold frame when Frame hold is on, or at each of Frames (see
//! [`held_inputs`]), each frame's a projection of its own, blended.

use crate::geo;
use crate::geom::Camera;
use crate::image::Image;
use crate::scene::Scene;
use anyhow::{bail, Result};
use serde_json::Value;
use std::sync::Arc;

/// How a projection's image goes onto a surface's color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The image's color instead (over black where it's clear).
    Replace,
    /// The image over the surface's color, by its alpha.
    Over,
    /// The surface's color times the image's (white where it's clear).
    Multiply,
}

/// An image projected from a camera onto the cards and meshes of
/// `targets`' nodes (or onto the instances holding it).
#[derive(Clone)]
pub struct Projection {
    pub targets: Vec<String>,
    pub camera: Camera,
    /// What it projects: its frame spans the camera's.
    pub image: Arc<Image>,
    pub mode: Mode,
    pub mix: f32,
    /// Nothing outside the image's frame (else its edge pixels go on),
    /// faded in over `edge` pixels inside it.
    pub crop: bool,
    pub edge: f64,
    /// Only on what the camera sees first.
    pub occlusion: bool,
    /// On surfaces facing away from the camera too.
    pub backfaces: bool,
    /// Faded out by the angle (degrees) between a surface's normal and
    /// the direction to the camera: whole up to `fade_start`, gone at
    /// `fade_end` (see [`facing`](Self::facing)). 90 / 90: no fade.
    pub fade_start: f32,
    pub fade_end: f32,
    /// Blended with the projections next to it that blend too: on a
    /// surface, a run of them is one layer, its plate their plates
    /// weighed by their weights w (Mix × crop × facing × visibility),
    /// sum(w · plate) / sum(w), put on min(sum(w), 1) of the way by the
    /// first one's mode. Off: put on after the ones before, over them.
    pub blend: bool,
}

impl Projection {
    /// Whether it lands on the surfaces of node `node`.
    pub fn targets(&self, node: &str) -> bool {
        self.targets.iter().any(|t| t == node)
    }

    /// `color` (premultiplied) with `plate` (the image there, premultiplied)
    /// put on by the mode, `w` of the way (0: `color` as it is).
    pub fn apply(&self, color: [f32; 4], plate: [f32; 4], w: f32) -> [f32; 4] {
        let a = color[3];
        let put: [f32; 4] = match self.mode {
            Mode::Replace => [plate[0] * a, plate[1] * a, plate[2] * a, a],
            Mode::Over => std::array::from_fn(|c| if c == 3 { a } else { plate[c] * a + color[c] * (1.0 - plate[3]) }),
            Mode::Multiply => std::array::from_fn(|c| if c == 3 { a } else { color[c] * (plate[c] + 1.0 - plate[3]) }),
        };
        std::array::from_fn(|c| color[c] + (put[c] - color[c]) * w)
    }

    /// How much of it lands on a surface by its facing: `cos_angle` is the
    /// cosine of the angle between the surface's normal (the side towards
    /// the camera, with Backfaces on) and the direction to the camera. 1 up
    /// to Fade start, smoothly to 0 at Fade end (a hard edge there when
    /// start isn't below end), so at 90 / 90 everything facing it takes it.
    pub fn facing(&self, cos_angle: f32) -> f32 {
        let angle = cos_angle.clamp(-1.0, 1.0).acos().to_degrees();
        let (start, end) = (self.fade_start.min(self.fade_end), self.fade_end);
        if angle <= start {
            1.0
        } else if angle >= end {
            0.0
        } else {
            let t = (end - angle) / (end - start);
            t * t * (3.0 - 2.0 * t)
        }
    }

    /// How much of it reaches frame position `px` of its image: 0 outside
    /// the frame when cropped, rising smoothly over Edge pixels inside.
    pub fn crop_at(&self, px: [f64; 2]) -> f32 {
        if !self.crop {
            return 1.0;
        }
        let (w, h) = (self.image.format.width as f64, self.image.format.height as f64);
        let d = px[0].min(w - px[0]).min(px[1]).min(h - px[1]);
        if d <= 0.0 {
            return 0.0;
        }
        if self.edge <= 0.0 {
            return 1.0;
        }
        let t = (d / self.edge).min(1.0);
        (t * t * (3.0 - 2.0 * t)) as f32
    }
}

/// The frames listed in Frames (as "1001, 1040, 1080": commas or
/// spaces between), in order, each once; none when it's empty.
pub fn frames(params: &Value) -> Result<Vec<i64>> {
    let mut out: Vec<i64> = Vec::new();
    for t in params["frames"].as_str().unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()) {
        let Ok(f) = t.parse::<i64>() else {
            bail!("Project 3D: Frames takes frame numbers with commas between (as 1001, 1040, 1080), not '{t}'");
        };
        if !out.contains(&f) {
            out.push(f);
        }
    }
    Ok(out)
}

/// The inputs a node takes at other frames, and the frames: Project 3D's
/// projector and image at each of Frames, or at Hold frame when Frame
/// hold is on. The graph hands it the first frame's at their ports and
/// each further frame's after its inputs, a pair (image, projector) a
/// frame (see [`project_3d`]).
pub fn held_inputs(op: &str, params: &Value) -> Option<(&'static [usize], Vec<i64>)> {
    if op != "project_3d" {
        return None;
    }
    // (Frames that don't read are the node's error to give.)
    let listed = frames(params).unwrap_or_default();
    if !listed.is_empty() {
        return Some((&[1, 2][..], listed));
    }
    params["hold"].as_bool().unwrap_or(false).then(|| (&[1, 2][..], vec![params["hold_frame"].as_i64().unwrap_or(1)]))
}

/// A Project 3D: `scene` with `image` projected from `projector`'s first
/// camera onto the meshes, cards and instances of Group's nodes (see the
/// node's help in ops.rs).
pub fn project_3d(params: &Value, scene: Scene, image: Option<&Image>, projector: Option<&Scene>) -> Result<Scene> {
    project_shots(params, scene, &[(image, projector)])
}

/// A Project 3D taking its image and projector at several frames
/// (Frames): `shots` are (image, projector) a frame, each projected,
/// blending with the others. One without Frames.
pub fn project_shots(params: &Value, mut scene: Scene, shots: &[(Option<&Image>, Option<&Scene>)]) -> Result<Scene> {
    let listed = frames(params)?;
    if !listed.is_empty() && listed.len() != shots.len() {
        bail!("Project 3D: Frames lists {} frames, but it was given the projector and image at {}", listed.len(), shots.len());
    }
    let mut taken = Vec::with_capacity(shots.len());
    for (i, (image, projector)) in shots.iter().enumerate() {
        let at = || listed.get(i).map_or(String::new(), |f| format!(" at frame {f}"));
        let Some(camera) = projector.and_then(|s| s.cameras.first()) else {
            bail!("Project 3D: connect a scene with a camera (the projector) to its projector input{}", at());
        };
        // (An unconnected input arrives as an empty image: nothing to
        // project, not a clear one that would blacken what it lands on.)
        let Some(image) = image.filter(|i| i.scene.is_none() && !i.window().is_empty()) else {
            bail!("Project 3D: connect the image to project to its image input{}", at());
        };
        taken.push((camera.clone(), Arc::new(image.clone())));
    }
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    // Cards and meshes by their nodes; instances (by theirs, or by one of
    // their prototypes') hold it themselves.
    let mut targets: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        if !targets.iter().any(|t| t == n) {
            targets.push(n.to_string());
        }
    };
    scene.cards.iter().map(|c| &c.node).chain(scene.meshes.iter().map(|m| &m.node)).filter(|n| take(n)).for_each(|n| add(n));
    let mode = match params["mode"].as_str().unwrap_or("replace") {
        "over" => Mode::Over,
        "multiply" => Mode::Multiply,
        _ => Mode::Replace,
    };
    let on = |n: &str, d: bool| params[n].as_bool().unwrap_or(d);
    let angle = |n: &str| params[n].as_f64().filter(|v| v.is_finite()).unwrap_or(90.0).clamp(0.0, 180.0) as f32;
    let first = Projection {
        targets,
        camera: taken[0].0.clone(),
        image: taken[0].1.clone(),
        mode,
        mix: params["mix"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
        crop: on("crop", true),
        edge: params["edge"].as_f64().unwrap_or(0.0).max(0.0),
        occlusion: on("occlusion", true),
        backfaces: on("backfaces", false),
        fade_start: angle("fade_start"),
        fade_end: angle("fade_end"),
        blend: on("blend", false) || !listed.is_empty(),
    };
    // One a frame, from the camera then with the image then.
    let projections: Vec<Projection> = taken.into_iter().map(|(camera, image)| Projection { camera, image, ..first.clone() }).collect();
    for g in scene.instances.iter_mut() {
        let proto = |p: &geo::Prototype| match p {
            geo::Prototype::Model(m) => take(&m.node),
            geo::Prototype::Card(c) => take(&c.node),
            geo::Prototype::Splats(_) => false,
        };
        if take(&g.node) || g.prototypes.iter().any(proto) {
            g.projections.extend(projections.iter().map(|p| Projection { targets: Vec::new(), ..p.clone() }));
        }
    }
    scene.projections.extend(projections);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use crate::geom::Camera;
    use crate::graph::{Evaluator, Graph};
    use crate::image::{IRect, Image, Size};
    use crate::ops::EvalContext;
    use crate::render3d;
    use crate::scene::{Card, Scene};
    use serde_json::json;
    use std::sync::Arc;

    const FMT: Size = Size { width: 48, height: 32 };

    /// A plate with a different color at every pixel, opaque.
    fn plate() -> Image {
        let mut img = Image::blank(FMT, IRect::frame(FMT));
        for y in 0..FMT.height {
            for x in 0..FMT.width {
                img.pixels[y * FMT.width + x] = [x as f32 / 47.0, y as f32 / 31.0, ((x * 7 + y * 3) % 11) as f32 / 10.0, 1.0];
            }
        }
        img
    }

    /// A camera at `z` looking down -Z.
    fn camera_at(x: f64, z: f64) -> Camera {
        Camera { world: crate::geom::trs([x, 0.0, z], [0.0, 0.0, 0.0], [1.0; 3]), ..Camera::default() }
    }

    /// A gray card filling (more than) `cam`'s view at depth `d`, named `node`.
    fn card(node: &str, z: f64, size: f64) -> Card {
        let mut gray = Image::blank(Size { width: 4, height: 4 }, IRect::frame(Size { width: 4, height: 4 }));
        gray.pixels.fill([0.5, 0.5, 0.5, 1.0]);
        let mut c = Card::new(Arc::new(gray), crate::geom::trs([0.0, 0.0, z], [0.0; 3], [1.0; 3]), [size, size]);
        c.node = node.to_string();
        c
    }

    fn projected(scene: Scene, cam: &Camera, params: serde_json::Value) -> Scene {
        let projector = Scene { cameras: vec![cam.clone()], ..Scene::default() };
        super::project_3d(&params, scene, Some(&plate()), Some(&projector)).unwrap()
    }

    fn render(scene: &Scene, cam: &Camera) -> Image {
        render3d::render(scene, cam, FMT, 1, None)
    }

    fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    /// Through the projector's own camera, at the image's format, a card
    /// shows the image to the pixel; a second card hidden behind it gets
    /// nothing with Occlusion on, the image without.
    #[test]
    fn the_projector_sees_its_image_back() {
        let cam = camera_at(0.0, 0.0);
        let near = card("wall", -10.0, 30.0);
        let scene = projected(Scene { cards: vec![near], ..Scene::default() }, &cam, json!({}));
        let out = render(&scene, &cam);
        let p = plate();
        for (i, (a, b)) in out.pixels.iter().zip(&p.pixels).enumerate() {
            assert!(close(*a, *b, 1e-4), "pixel {i}: {a:?} vs {b:?}");
        }
        // Behind a smaller card in front: seen from the side, the back
        // card has gray where the front one hides it from the projector.
        let side = camera_at(3.0, 0.0);
        let front = card("front", -5.0, 2.0);
        let back = card("back", -10.0, 30.0);
        let shadowed = |params: serde_json::Value| {
            let scene = projected(Scene { cards: vec![front.clone(), back.clone()], ..Scene::default() }, &cam, params);
            render(&scene, &side)
        };
        // The point of the back card behind the front card's middle, as
        // the side camera sees it (and isn't covered by the front card
        // from there).
        let (q, _) = side.project(&crate::geom::Vec3::new(0.0, 0.0, -10.0), FMT).unwrap();
        let at = |img: &Image| img.pixels[q[1] as usize * FMT.width + q[0] as usize];
        assert!(close(at(&shadowed(json!({}))), [0.5, 0.5, 0.5, 1.0], 1e-4), "hidden from the projector: its own gray");
        let through = at(&shadowed(json!({ "occlusion": false })));
        assert!(!close(through, [0.5, 0.5, 0.5, 1.0], 1e-2), "without occlusion the image goes through: {through:?}");
        // Group: only the back card.
        let scene = projected(Scene { cards: vec![card("front", -5.0, 2.0)], ..Scene::default() }, &cam, json!({ "group": "back" }));
        assert!(close(render(&scene, &cam).pixels[16 * 48 + 24], [0.5, 0.5, 0.5, 1.0], 1e-4));
    }

    /// Crop leaves what's outside the image's frame as it was, with a
    /// soft edge; Backfaces off leaves a card facing away untouched; the
    /// modes; and projections stack in order.
    #[test]
    fn crop_backfaces_modes_and_stacking() {
        let cam = camera_at(0.0, 0.0);
        // Seen from farther back, the projection covers the middle.
        let view = camera_at(0.0, 10.0);
        let scene = projected(Scene { cards: vec![card("wall", -10.0, 60.0)], ..Scene::default() }, &cam, json!({}));
        let out = render(&scene, &view);
        assert!(close(out.pixels[24], [0.5, 0.5, 0.5, 1.0], 1e-4), "outside: its own gray");
        assert!(!close(out.pixels[16 * 48 + 24], [0.5, 0.5, 0.5, 1.0], 1e-2), "inside: the image");
        let uncropped = render(&projected(Scene { cards: vec![card("wall", -10.0, 60.0)], ..Scene::default() }, &cam, json!({ "crop": false })), &view);
        assert!(!close(uncropped.pixels[24], [0.5, 0.5, 0.5, 1.0], 1e-2), "not cropped: the image's edge goes on");
        // Edge: half way in from the frame's edge.
        let p = super::Projection { crop: true, edge: 4.0, ..scene.projections[0].clone() };
        assert_eq!(p.crop_at([-1.0, 10.0]), 0.0);
        assert!((p.crop_at([2.0, 10.0]) - 0.5).abs() < 1e-6);
        assert_eq!(p.crop_at([24.0, 16.0]), 1.0);
        // A card facing away from the projector.
        let mut away = card("wall", -10.0, 30.0);
        away.world = crate::geom::trs([0.0, 0.0, -10.0], [0.0, 180.0, 0.0], [1.0; 3]);
        let gray = [0.5, 0.5, 0.5, 1.0];
        let scene = projected(Scene { cards: vec![away.clone()], ..Scene::default() }, &cam, json!({}));
        assert!(close(render(&scene, &cam).pixels[16 * 48 + 24], gray, 1e-4), "backface: untouched");
        let scene = projected(Scene { cards: vec![away], ..Scene::default() }, &cam, json!({ "backfaces": true }));
        assert!(!close(render(&scene, &cam).pixels[16 * 48 + 24], gray, 1e-2), "backfaces on");
        // Modes, at one pixel.
        let pl = plate().pixels[16 * 48 + 24];
        let mode = |m: &str, mix: f64| render(&projected(Scene { cards: vec![card("wall", -10.0, 30.0)], ..Scene::default() }, &cam, json!({ "mode": m, "mix": mix })), &cam).pixels[16 * 48 + 24];
        assert!(close(mode("multiply", 1.0), [0.5 * pl[0], 0.5 * pl[1], 0.5 * pl[2], 1.0], 1e-4));
        assert!(close(mode("over", 1.0), pl, 1e-4), "an opaque image over: the image");
        assert!(close(mode("replace", 0.5), std::array::from_fn(|c| 0.5 * gray[c] + 0.5 * pl[c]), 1e-4), "Mix");
        // Stacked: a red patch (clear but for a square) over the plate.
        let base = projected(Scene { cards: vec![card("wall", -10.0, 30.0)], ..Scene::default() }, &cam, json!({}));
        let mut patch = Image::blank(FMT, IRect::frame(FMT));
        for y in 10..20 {
            for x in 20..30 {
                patch.pixels[y * 48 + x] = [1.0, 0.0, 0.0, 1.0];
            }
        }
        let projector = Scene { cameras: vec![cam.clone()], ..Scene::default() };
        let both = super::project_3d(&json!({ "mode": "over" }), base, Some(&patch), Some(&projector)).unwrap();
        let out = render(&both, &cam);
        assert!(close(out.pixels[15 * 48 + 25], [1.0, 0.0, 0.0, 1.0], 1e-4), "the patch on top");
        assert!(close(out.pixels[5 * 48 + 5], plate().pixels[5 * 48 + 5], 1e-4), "the plate round it");
    }

    /// Instances take a projection aimed at them (by their node or their
    /// prototypes'), and one their prototypes had before Instance on
    /// Points; each copy shows the part of the image falling on it.
    #[test]
    fn instances_take_projections() {
        let cam = camera_at(0.0, 0.0);
        let proto = card("tile", 0.0, 4.0);
        let pts = crate::geo::PointSet { positions: vec![crate::geom::Vec3::new(-3.0, 0.0, -10.0), crate::geom::Vec3::new(3.0, 0.0, -10.0)], node: "pts".into(), ..Default::default() };
        let protos = Scene { cards: vec![proto], ..Scene::default() };
        let copied = |protos: &Scene| {
            let mut s = crate::ops::geo_ops::instance_on_points(&json!({}), Scene { point_sets: vec![pts.clone()], ..Scene::default() }, Some(protos), None).unwrap();
            s.tag("copies");
            s
        };
        let p = plate();
        let check = |scene: &Scene| {
            let out = render(scene, &cam);
            // Each copy's middle shows the image there; between them, nothing.
            for x in [crate::geom::Vec3::new(-3.0, 0.0, -10.0), crate::geom::Vec3::new(3.0, 0.0, -10.0)] {
                let (q, _) = cam.project(&x, FMT).unwrap();
                let i = q[1] as usize * 48 + q[0] as usize;
                assert!(close(out.pixels[i], p.pixels[i], 1e-4), "{:?} vs {:?}", out.pixels[i], p.pixels[i]);
            }
            assert_eq!(out.pixels[16 * 48 + 24][3], 0.0);
        };
        // Aimed at the instances, by their node and by their prototype's.
        check(&projected(copied(&protos), &cam, json!({ "group": "copies" })));
        check(&projected(copied(&protos), &cam, json!({ "group": "tile" })));
        // On the prototypes before they're copied.
        let scene = copied(&projected(protos.clone(), &cam, json!({})));
        assert_eq!(scene.instances[0].projections.len(), 1);
        check(&scene);
        // Realized, they keep it.
        check(&crate::ops::geo_ops::realize_instances(&json!({}), scene).unwrap());
    }

    fn graph(extra: serde_json::Value) -> Graph {
        let mut nodes = json!([
            { "id": "plate", "op": "constant", "params": { "color": [0.2, 0.4, 0.8, 1.0], "width": 48, "height": 32 } },
            { "id": "shot", "op": "camera", "params": { "translate": [0, 0, 0] } },
            { "id": "proj", "op": "camera", "params": { "translate": [0, 0, 0] } },
            { "id": "wall", "op": "card", "inputs": ["plate"], "params": { "translate": [0, 0, -10], "size": [30, 30] } },
            { "id": "pr", "op": "project_3d", "inputs": ["wall", "plate", "proj"] },
        ]);
        nodes.as_array_mut().unwrap().extend(extra.as_array().unwrap().iter().cloned());
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    fn scene_at(g: &Graph, node: &str, frame: i64) -> Arc<Scene> {
        Evaluator::new().evaluate(g, node, &EvalContext { frame, write_outputs: false }).unwrap().scene.clone().unwrap()
    }

    /// Frame hold: the projector taken at Hold frame, however it moves
    /// after; live, it moves with its keys.
    #[test]
    fn frame_hold_keeps_the_projector_still() {
        let mut g = graph(json!([]));
        let keys = json!({ "translate": { "keys": [[1, [0, 0, 0]], [11, [5, 0, 0]]], "interp": "linear" } });
        let proj = g.nodes.iter_mut().find(|n| n.id == "proj").unwrap();
        proj.params["translate"] = keys["translate"].clone();
        let x = |s: &Scene| s.projections[0].camera.world[(0, 3)];
        assert!((x(&scene_at(&g, "pr", 6)) - 2.5).abs() < 1e-6, "live");
        let pr = g.nodes.iter_mut().find(|n| n.id == "pr").unwrap();
        pr.params = json!({ "hold": true, "hold_frame": 3 });
        for f in [1, 6, 11] {
            assert!((x(&scene_at(&g, "pr", f)) - 1.0).abs() < 1e-6, "held at frame 3, at {f}");
        }
        // A target and the error without a camera.
        assert_eq!(scene_at(&g, "pr", 1).projections[0].targets, vec!["wall".to_string()]);
        let g = graph(json!([{ "id": "bad", "op": "project_3d", "inputs": ["wall", "plate", ""] }]));
        assert!(Evaluator::new().evaluate(&g, "bad", &EvalContext { frame: 1, write_outputs: false }).is_err());
        // Nor without its image (an empty one, as an unconnected input
        // arrives): an error, not the cards blackened.
        let g = graph(json!([{ "id": "blank", "op": "project_3d", "inputs": ["wall", "", "proj"] }]));
        let err = Evaluator::new().evaluate(&g, "blank", &EvalContext { frame: 1, write_outputs: false }).err().expect("an error");
        assert!(format!("{err}").contains("connect the image"), "{err}");
    }

    /// Frames: one projection a frame listed, from the projector's camera
    /// and with its image then, all blending; it overrides Frame hold.
    /// The node's key follows the held frames, not the live projector.
    #[test]
    fn frames_project_from_each_frame_listed() {
        // The plate projected brightens over the shot (the wall's own stays).
        let fading = json!({ "keys": [[1, [0, 0, 0, 1]], [11, [1, 1, 1, 1]]], "interp": "linear" });
        let mut g = graph(json!([{ "id": "shotplate", "op": "constant", "params": { "color": fading, "width": 48, "height": 32 } }]));
        let proj = g.nodes.iter_mut().find(|n| n.id == "proj").unwrap();
        proj.params["translate"] = json!({ "keys": [[1, [0, 0, 0]], [11, [5, 0, 0]]], "interp": "linear" });
        let pr = g.nodes.iter_mut().find(|n| n.id == "pr").unwrap();
        pr.inputs[1] = "shotplate".into();
        pr.params = json!({ "frames": "11, 1 6,1", "hold": true, "hold_frame": 3 });
        for f in [1, 4, 9] {
            let s = scene_at(&g, "pr", f);
            assert_eq!(s.projections.len(), 3, "one a frame, each once");
            for (p, (x, gray)) in s.projections.iter().zip([(5.0, 1.0), (0.0, 0.0), (2.5, 0.5)]) {
                assert!((p.camera.world[(0, 3)] - x).abs() < 1e-6, "the camera then");
                assert!((p.image.pixels[0][0] - gray).abs() < 1e-6, "the image then");
                assert!(p.blend && p.targets == ["wall"]);
            }
        }
        // The live projector moves, the plate changes: the held key doesn't.
        let keys = Evaluator::keys(&g, "pr", &[1, 4, 9]).unwrap();
        assert!(keys.iter().all(|k| *k == keys[0]), "{keys:?}");
        let live = graph(json!([]));
        let mut moving = live.clone();
        moving.nodes.iter_mut().find(|n| n.id == "proj").unwrap().params["translate"] = json!({ "keys": [[1, [0, 0, 0]], [11, [5, 0, 0]]], "interp": "linear" });
        let keys = Evaluator::keys(&moving, "pr", &[1, 4]).unwrap();
        assert_ne!(keys[0], keys[1], "live, it follows the projector");
        // A held frame's key changes when the projector does then.
        let mut other = g.clone();
        other.nodes.iter_mut().find(|n| n.id == "proj").unwrap().params["translate"] = json!({ "keys": [[1, [0, 0, 0]], [11, [6, 0, 0]]], "interp": "linear" });
        assert_ne!(Evaluator::key(&g, "pr", &EvalContext { frame: 1, write_outputs: false }).unwrap(), Evaluator::key(&other, "pr", &EvalContext { frame: 1, write_outputs: false }).unwrap());
        // Frames that don't read are an error.
        g.nodes.iter_mut().find(|n| n.id == "pr").unwrap().params = json!({ "frames": "1001-1010" });
        let err = Evaluator::new().evaluate(&g, "pr", &EvalContext { frame: 1, write_outputs: false }).err().expect("an error");
        assert!(format!("{err}").contains("not '1001-1010'"), "{err}");
        assert_eq!(super::frames(&json!({ "frames": " 1001,1040  1080 , " })).unwrap(), vec![1001, 1040, 1080]);
        assert!(super::frames(&json!({})).unwrap().is_empty());
    }

    /// The new params: off by default (old projects load unchanged: no
    /// fade, no blending, the same render); read when set; the facing
    /// curve the renderers share.
    #[test]
    fn fade_and_blend_params() {
        let cam = camera_at(0.0, 0.0);
        let old = projected(Scene { cards: vec![card("wall", -10.0, 30.0)], ..Scene::default() }, &cam, json!({ "mode": "over", "mix": 0.5 }));
        let p = &old.projections[0];
        assert_eq!((p.fade_start, p.fade_end, p.blend), (90.0, 90.0, false));
        for c in [1.0, 0.5, 0.0, -0.3] {
            assert_eq!(p.facing(c), if c >= 0.0 { 1.0 } else { 0.0 }, "90 / 90: whole on all that faces it");
        }
        let set = projected(Scene { cards: vec![card("wall", -10.0, 30.0)], ..Scene::default() }, &cam, json!({ "fade_start": 30, "fade_end": 60, "blend": true }));
        let p = &set.projections[0];
        assert_eq!((p.fade_start, p.fade_end, p.blend), (30.0, 60.0, true));
        let at = |deg: f32| p.facing(deg.to_radians().cos());
        assert_eq!(at(0.0), 1.0);
        assert!((at(29.0) - 1.0).abs() < 1e-6);
        assert!((at(45.0) - 0.5).abs() < 1e-4, "half way: {}", at(45.0));
        assert!(at(40.0) > at(50.0) && at(50.0) > 0.0);
        assert_eq!(at(61.0), 0.0);
        // Start past end: a hard edge at end.
        let hard = super::Projection { fade_start: 70.0, fade_end: 50.0, ..p.clone() };
        assert_eq!((hard.facing(49f32.to_radians().cos()), hard.facing(51f32.to_radians().cos())), (1.0, 0.0));
    }

    /// With motion blur the projection rides on a moving card: projected
    /// at the frame's own place, the same image under every time.
    #[test]
    fn the_projection_rides_with_a_moving_card() {
        let cam = camera_at(0.0, 0.0);
        let mut a = card("wall", -10.0, 30.0);
        let still = projected(Scene { cards: vec![a.clone()], ..Scene::default() }, &cam, json!({}));
        a.world = crate::geom::trs([0.0, 0.0, -10.0], [0.0; 3], [1.0; 3]);
        let mut b = a.clone();
        b.world = crate::geom::trs([0.0, 0.0, -10.5], [0.0; 3], [1.0; 3]);
        let later = Scene { cards: vec![b], ..still.clone() };
        let at = still.at_time(&[(0, &still), (1, &later)], 0.5);
        assert!(at.rest.is_some());
        // The card moved straight away from the camera: the image on it
        // shrinks with it (rides), not stays the size it projects at.
        let moved = render(&at, &cam);
        let fixed = render(&Scene { rest: None, ..at.clone() }, &cam);
        let differs = moved.pixels.iter().zip(&fixed.pixels).any(|(p, q)| !close(*p, *q, 1e-3));
        assert!(differs, "riding differs from projecting at the moved place");
        assert!(fixed.pixels.iter().zip(&plate().pixels).all(|(p, q)| close(*p, *q, 1e-4)), "at the moved place, it projects the plate as is");
    }
}
