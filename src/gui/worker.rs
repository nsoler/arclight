//! Background evaluation thread, so rendering never blocks the UI.
//!
//! Requests are coalesced: if several arrive while a frame is rendering, only
//! the newest is evaluated. Finished frames are kept in a byte-budgeted LRU
//! keyed by the graph's cache key, so scrubbing back over a frame is instant.

use arclight::graph::{Evaluator, Graph, NodeError};
use arclight::image::Image;
use std::collections::HashMap;
use arclight::ops::EvalContext;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Memory budget for finished viewer frames (`frame_cache_mb` in settings).
static FRAME_CACHE_BYTES: std::sync::LazyLock<usize> =
    std::sync::LazyLock::new(|| arclight::config::Settings::load().frame_cache_mb.max(64) * 1024 * 1024);

pub struct Request {
    pub graph: Arc<Graph>,
    pub target: String,
    pub frame: i64,
    /// A node and its input to report alongside (the selected Transform's
    /// layer, a Keyer's plate to pick colors from), for its overlay.
    pub probe: Option<(String, String)>,
}

pub struct Response {
    pub target: String,
    pub frame: i64,
    pub result: Result<Arc<Image>, NodeError>,
    /// The result as it is on the GPU, when it was made there just now
    /// (not from the frame cache): the viewer draws it from there.
    pub gpu: Option<Arc<arclight::gpu::Resident>>,
    pub millis: f32,
    /// Frames of the project range already in the frame cache for this target.
    pub cached_frames: Vec<i64>,
    /// The probed node and its input's image, when asked and known.
    pub probe: Option<(String, Arc<Image>)>,
}

pub struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Response>,
}

impl Worker {
    /// A worker evaluating with `eval` (shared with the app bridge's calls,
    /// see [`crate::bridge_calls`]: each holds it for one evaluation at a
    /// time).
    pub fn spawn(repaint: eframe::egui::Context, eval: Arc<Mutex<Evaluator>>) -> Worker {
        let (req_tx, req_rx) = channel::<Request>();
        let (resp_tx, resp_rx) = channel::<Response>();
        std::thread::Builder::new()
            .name("arclight-eval".into())
            .spawn(move || run(req_rx, resp_tx, repaint, eval))
            .expect("spawn eval thread");
        Worker { tx: req_tx, rx: resp_rx }
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// The newest finished response, if any arrived since the last call.
    pub fn poll(&self) -> Option<Response> {
        self.rx.try_iter().last()
    }
}

struct FrameCache {
    entries: VecDeque<(u64, Arc<Image>)>,
    bytes: usize,
}

impl FrameCache {
    fn get(&mut self, key: u64) -> Option<Arc<Image>> {
        let i = self.entries.iter().position(|(k, _)| *k == key)?;
        let entry = self.entries.remove(i)?;
        let img = entry.1.clone();
        self.entries.push_back(entry);
        Some(img)
    }

    fn contains(&self, key: u64) -> bool {
        self.entries.iter().any(|(k, _)| *k == key)
    }

    fn insert(&mut self, key: u64, img: Arc<Image>) {
        self.bytes += size_of_image(&img);
        self.entries.push_back((key, img));
        while self.bytes > *FRAME_CACHE_BYTES && self.entries.len() > 1 {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= size_of_image(&old);
            }
        }
    }
}

fn size_of_image(img: &Image) -> usize {
    img.bytes()
}

/// Whether to panic in this evaluation (of `target`) on purpose:
/// `ARCLIGHT_CRASH_TEST` "worker" makes the first one panic (and tests,
/// one of a node they name).
fn crash_test_worker(target: &str) -> bool {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    #[cfg(test)]
    if tests::PANIC_ON.lock().unwrap().take_if(|n| n == target).is_some() {
        return true;
    }
    let _ = target;
    arclight::crash::crash_test().as_deref() == Some("worker") && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
}

fn run(rx: Receiver<Request>, tx: Sender<Response>, repaint: eframe::egui::Context, eval: Arc<Mutex<Evaluator>>) {
    let evaluate = |graph: &Graph, target: &str, ctx: &EvalContext| eval.lock().unwrap_or_else(|e| e.into_inner()).evaluate(graph, target, ctx);
    let evaluate_resident = |graph: &Graph, target: &str, ctx: &EvalContext| eval.lock().unwrap_or_else(|e| e.into_inner()).evaluate_resident(graph, target, ctx);
    let mut frames = FrameCache { entries: VecDeque::new(), bytes: 0 };
    // Probed inputs by cache key, so frames served from the frame cache
    // needn't render the input again. Kept small: they're whole images.
    let mut probed: HashMap<u64, Arc<Image>> = HashMap::new();

    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        // A panic in an evaluation is that frame's error: the evaluator
        // and caches start afresh (they may be half-updated), and a report
        // is saved (see crash.rs).
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if crash_test_worker(&req.target) {
                panic!("ARCLIGHT_CRASH_TEST=worker: a deliberate panic in an evaluation");
            }
            let start = Instant::now();
            let ctx = EvalContext { frame: req.frame, write_outputs: false };
            let mut gpu = None;
            let result = match Evaluator::key(&req.graph, &req.target, &ctx) {
                Err(e) => Err(e),
                Ok(key) => match frames.get(key) {
                    Some(img) => Ok(img),
                    None => match evaluate_resident(&req.graph, &req.target, &ctx) {
                        Ok((img, resident)) => {
                            frames.insert(key, img.clone());
                            gpu = resident;
                            Ok(img)
                        }
                        Err(e) => Err(e),
                    },
                },
            };
            let millis = start.elapsed().as_secs_f32() * 1000.0;
            let probe = req.probe.take().and_then(|(node, input)| {
                let key = Evaluator::key(&req.graph, &input, &ctx).ok()?;
                if !probed.contains_key(&key) {
                    let img = evaluate(&req.graph, &input, &ctx).ok()?;
                    if probed.len() >= 8 {
                        probed.clear();
                    }
                    probed.insert(key, img);
                }
                Some((node, probed[&key].clone()))
            });

            let p = &req.graph.project;
            let cached_frames = (p.first_frame..=p.last_frame.max(p.first_frame))
                .filter(|&f| {
                    let c = EvalContext { frame: f, write_outputs: false };
                    Evaluator::key(&req.graph, &req.target, &c).is_ok_and(|k| frames.contains(k))
                })
                .collect();
            (result, gpu, millis, probe, cached_frames)
        }));
        let (result, gpu, millis, probe, cached_frames) = match done {
            Ok(done) => done,
            Err(payload) => {
                *eval.lock().unwrap_or_else(|e| e.into_inner()) = Evaluator::new();
                frames = FrameCache { entries: VecDeque::new(), bytes: 0 };
                probed.clear();
                (Err(NodeError { node: None, message: arclight::crash::caught(&*payload) }), None, 0.0, None, Vec::new())
            }
        };

        let resp = Response {
            target: req.target,
            frame: req.frame,
            result,
            gpu,
            millis,
            cached_frames,
            probe,
        };
        if tx.send(resp).is_err() {
            break;
        }
        repaint.request_repaint();
    }
}

/// What the 3D viewport asks for: the scene a node shows at a frame, and
/// optionally its cards (and meshes) rendered through a camera.
pub struct SceneRequest {
    pub graph: Arc<Graph>,
    pub node: String,
    pub frame: i64,
    /// Render the cards through this camera into a frame this size.
    pub render: Option<(arclight::geom::Camera, arclight::image::Size)>,
    /// Render the meshes too, solid (else the viewport draws their edges).
    pub solid: bool,
    /// How solid meshes are shaded: from the camera, or by the scene's
    /// lights.
    pub preview: arclight::render3d::Preview,
    /// With a Scanline Render's motion blur (the view has settled): see
    /// [`blurred`].
    pub blur: bool,
    /// Render the splats too (the view has settled; while it moves the
    /// viewport draws their centers instead).
    pub splats: bool,
    /// What the render camera looks through (and the project's frame size,
    /// for a light): with blur, it's found again in the scene at each frame
    /// round the shutter, so its own motion blurs the view too.
    pub through: Option<(crate::viewport3d::Through, arclight::image::Size)>,
    /// Where the render goes on screen (echoed back).
    pub rect: Option<eframe::egui::Rect>,
    /// Selected nodes to show even if they aren't in the scene (see
    /// [`ghosts_of`]).
    pub ghosts: Vec<String>,
}

pub struct SceneResponse {
    pub node: String,
    /// The node's scene: None when it shows none (an image node).
    pub scene: Result<Option<Arc<arclight::scene::Scene>>, String>,
    pub render: Option<Arc<Image>>,
    /// Whether `render` has the splats in it.
    pub splats: bool,
    pub rect: Option<eframe::egui::Rect>,
    pub millis: f32,
    /// The shutter times (frames on from the frame) a settled view of a
    /// render with motion blur shows: the cards blurred over them, and the
    /// particles streaked. Empty otherwise.
    pub blur_times: Vec<f64>,
    /// The asked-for ghosts not in the scene, and why any couldn't be made.
    pub ghosts: Option<Arc<arclight::scene::Scene>>,
    pub ghost_errors: Vec<(String, String)>,
}

/// The 3D viewport's own background thread (so orbiting doesn't queue
/// behind 2D frames). Requests coalesce: only the newest is served.
pub struct SceneWorker {
    tx: Sender<SceneRequest>,
    rx: Receiver<SceneResponse>,
}

impl SceneWorker {
    pub fn spawn(repaint: eframe::egui::Context) -> SceneWorker {
        let (req_tx, req_rx) = channel::<SceneRequest>();
        let (resp_tx, resp_rx) = channel::<SceneResponse>();
        std::thread::Builder::new()
            .name("arclight-viewport".into())
            .spawn(move || {
                let mut eval = Evaluator::new();
                while let Ok(mut req) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let t0 = Instant::now();
                    // A panic is this request's error, with a fresh
                    // evaluator after it (see the 2D worker's).
                    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        // (A 2D node after a render shows the render's scene.)
                        let viewed = scene_source(&req.graph, &req.node).unwrap_or_else(|| req.node.clone());
                        let scene = scene_of(&mut eval, &req.graph, &viewed, req.frame);
                        let blur_times = if req.blur { view_blur_times(&req.graph, &viewed, req.frame) } else { Vec::new() };
                        // Only cards, solid meshes and (settled) splats are
                        // rendered; the viewport draws the rest.
                        let splats = req.splats && matches!(&scene, Ok(Some(s)) if !s.splats.is_empty());
                        let render = match (&scene, &req.render) {
                            (Ok(Some(s)), Some((cam, format))) if !s.cards.is_empty() || (req.solid && !s.meshes.is_empty()) || splats => {
                                let lit = matches!(req.preview, arclight::render3d::Preview::Lit { .. });
                                let shown = |s: &arclight::scene::Scene| {
                                    let meshes = if req.solid { s.meshes.clone() } else { Vec::new() };
                                    let lights = if lit { s.lights.clone() } else { Vec::new() };
                                    let splats = if splats { s.splats.clone() } else { Vec::new() };
                                    arclight::scene::Scene { cards: s.cards.clone(), meshes, lights, splats, ..Default::default() }
                                };
                                let now = shown(s);
                                let (times, then) = if blur_times.is_empty() { Default::default() } else { blurred(&mut eval, &req.graph, &viewed, req.frame) };
                                let then: Vec<(i64, arclight::scene::Scene, arclight::geom::Camera)> = then.iter().map(|(k, s)| (*k, shown(s), frame_camera(cam, s, req.through.as_ref()))).collect();
                                // Jittered as the render is (Jitter shutter).
                                let jitter = req.graph.node(&viewed).is_none_or(|n| arclight::ops::blur_jitter(&n.params));
                                let blur = arclight::render3d::Blur { times, frames: std::iter::once((0, &now, cam.clone())).chain(then.iter().map(|(k, s, c)| (*k, s, c.clone()))).collect(), jitter };
                                Some(Arc::new(arclight::render3d::render_preview_blurred(&now, cam, *format, req.preview, Some(&blur))))
                            }
                            _ => None,
                        };
                        let splats = splats && render.is_some();
                        let in_scene = scene.as_ref().ok().cloned().flatten();
                        let (ghosts, ghost_errors) = ghosts_of(&mut eval, &req.graph, in_scene.as_deref(), &req.ghosts, req.frame);
                        (scene, render, splats, blur_times, ghosts, ghost_errors)
                    }));
                    let (scene, render, splats, blur_times, ghosts, ghost_errors) = done.unwrap_or_else(|payload| {
                        eval = Evaluator::new();
                        (Err(arclight::crash::caught(&*payload)), None, false, Vec::new(), None, Vec::new())
                    });
                    let resp = SceneResponse { node: req.node, scene, render, splats, rect: req.rect, millis: t0.elapsed().as_secs_f32() * 1000.0, blur_times, ghosts, ghost_errors };
                    if resp_tx.send(resp).is_err() {
                        break;
                    }
                    repaint.request_repaint();
                }
            })
            .expect("spawn viewport thread");
        SceneWorker { tx: req_tx, rx: resp_rx }
    }

    pub fn request(&self, req: SceneRequest) {
        let _ = self.tx.send(req);
    }

    pub fn poll(&self) -> Option<SceneResponse> {
        self.rx.try_iter().last()
    }
}

/// The most shutter times the 3D view blurs over (few: it's a preview,
/// and the times are jittered).
const VIEW_BLUR_TIMES: usize = 8;

/// For the 3D view of Scanline Render `id` with motion blur at `frame`:
/// its shutter times (at most [`VIEW_BLUR_TIMES`], spread over the same
/// shutter) and its scenes at the frames round them (by offset; frames
/// whose scene fails are left out). Nothing without blur.
fn blurred(eval: &mut Evaluator, graph: &Graph, id: &str, frame: i64) -> (Vec<f64>, Vec<(i64, Arc<arclight::scene::Scene>)>) {
    let times = view_blur_times(graph, id, frame);
    if times.is_empty() {
        return Default::default();
    }
    let (lo, hi) = times.iter().fold((0.0f64, 0.0f64), |(lo, hi), &t| (lo.min(t), hi.max(t)));
    let then = (lo.floor() as i64..=hi.ceil() as i64).filter(|k| *k != 0).filter_map(|k| Some((k, scene_of(eval, graph, id, frame + k).ok()??))).collect();
    (times, then)
}

/// The render camera `cam` at another frame's `scene`: what it looks
/// through, found there, else (the view's own camera) unchanged.
fn frame_camera(cam: &arclight::geom::Camera, scene: &arclight::scene::Scene, through: Option<&(crate::viewport3d::Through, arclight::image::Size)>) -> arclight::geom::Camera {
    through.and_then(|(t, format)| t.camera_in(scene, *format)).unwrap_or_else(|| cam.clone())
}

/// The shutter times of [`blurred`] alone (no scenes evaluated).
fn view_blur_times(graph: &Graph, id: &str, frame: i64) -> Vec<f64> {
    let Some(node) = graph.node(id).filter(|n| n.op == "scanline_render") else { return Vec::new() };
    let params = arclight::ops::params_with_defaults(&node.op, &arclight::ops::resolve_params(&node.op, &node.params, frame));
    let Some(times) = arclight::ops::blur_times(&params).filter(|t| t.len() > 1) else { return Vec::new() };
    if times.len() <= VIEW_BLUR_TIMES {
        return times;
    }
    let share = times[1] - times[0];
    arclight::render3d::Blur::times(times[0] - share / 2.0, share * times.len() as f64, VIEW_BLUR_TIMES)
}

/// The node whose 3D scene viewing `id` shows: the nearest up its inputs
/// (itself first, breadth-first, a node's main input before the others: a
/// Merge's B, its background, before its A) that makes a scene (a Camera,
/// Card, Scene…), renders one (a Scanline Render) or solves one (a Camera
/// Tracker). So viewing a Grade or a Merge after a render shows the
/// render's scene.
pub fn scene_source(graph: &Graph, id: &str) -> Option<String> {
    use arclight::ops;
    let shows = |op: &str| ops::output_kind(op) == ops::Kind::Scene || matches!(op, "scanline_render" | "camera_tracker");
    let mut queue = std::collections::VecDeque::from([id.to_string()]);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = queue.pop_front() {
        let Some(n) = graph.node(&id).filter(|_| seen.insert(id.clone())) else { continue };
        if shows(&n.op) {
            return Some(id);
        }
        let mut order: Vec<usize> = (0..n.inputs.len()).collect();
        if order.len() > 1 && ops::port(&n.op, 1).is_some_and(|p| p.label == "B") {
            order.swap(0, 1);
        }
        queue.extend(order.into_iter().map(|i| n.inputs[i].clone()).filter(|i| !i.is_empty()));
    }
    None
}

/// The ops a selected node of may be shown as a ghost: one object each.
const GHOST_OPS: &[&str] = &[
    "card", "camera", "locator", "cube", "sphere", "cylinder", "plane", "read_geo",
    "light_ambient", "light_directional", "light_point", "light_spot", "light_area",
];

/// Whether `scene` holds node `node`'s card, mesh, camera, light or
/// locator.
pub fn holds(scene: &arclight::scene::Scene, node: &str) -> bool {
    scene.cards.iter().any(|c| c.node == node)
        || scene.meshes.iter().any(|m| m.node == node)
        || scene.lights.iter().any(|l| l.node == node)
        || (0..scene.cameras.len()).any(|i| scene.camera_node(i) == node)
        || (0..scene.points.len()).any(|i| scene.point_node(i) == node)
}

/// Of `nodes` (the selection), the cards, lights, cameras, locators and
/// meshes `scene` doesn't hold, each as its own output at `frame`, in one
/// scene (None if there are none): the 3D view shows them dimmed, so a
/// node picked in the graph can still be seen and moved. And for each that
/// can't be made, why.
pub fn ghosts_of(eval: &mut Evaluator, graph: &Graph, scene: Option<&arclight::scene::Scene>, nodes: &[String], frame: i64) -> (Option<Arc<arclight::scene::Scene>>, Vec<(String, String)>) {
    let ctx = EvalContext { frame, write_outputs: false };
    let (mut ghosts, mut errors) = (arclight::scene::Scene::default(), Vec::new());
    let mut any = false;
    for id in nodes {
        let Some(n) = graph.node(id).filter(|n| GHOST_OPS.contains(&n.op.as_str())) else { continue };
        if scene.is_some_and(|s| holds(s, id)) {
            continue;
        }
        match eval.evaluate(graph, &n.id, &ctx) {
            Ok(img) => {
                if let Some(s) = &img.scene {
                    ghosts.extend(s);
                    any = true;
                }
            }
            Err(e) => errors.push((id.clone(), e.to_string())),
        }
    }
    (any.then(|| Arc::new(ghosts)), errors)
}

/// The scene node `id` shows at `frame`: its own output for 3D nodes, a
/// Scanline Render's scene, camera and extra scene inputs together, a Camera Tracker's
/// solved camera and points; None for nodes that show no scene.
pub fn scene_of(eval: &mut Evaluator, graph: &Graph, id: &str, frame: i64) -> Result<Option<Arc<arclight::scene::Scene>>, String> {
    use arclight::scene::Scene;
    let Some(node) = graph.node(id) else { return Ok(None) };
    let ctx = EvalContext { frame, write_outputs: false };
    let mut out_of = |src: &str| -> Result<Option<Arc<Scene>>, String> {
        if src.is_empty() {
            return Ok(None);
        }
        Ok(eval.evaluate(graph, src, &ctx).map_err(|e| e.to_string())?.scene.clone())
    };
    if arclight::ops::output_kind(&node.op) == arclight::ops::Kind::Scene {
        return out_of(id);
    }
    match node.op.as_str() {
        "scanline_render" => {
            // All but the background: the scene, the camera, and the extra
            // scenes it renders too (lights, cards, …; see ops::render_scene).
            let mut all = Scene::default();
            for src in node.inputs.iter().enumerate().filter(|(i, _)| *i != 2).map(|(_, s)| s) {
                if let Some(s) = out_of(src)? {
                    all.extend(&s);
                }
            }
            Ok(Some(Arc::new(all)))
        }
        "camera_tracker" => {
            let Some(sol) = arclight::camtrack::Solution::from_params(&node.params) else { return Ok(Some(Arc::new(Scene::default()))) };
            let mut s = Scene { points: sol.points(), ..Scene::default() };
            s.point_errors = sol.point_rms.iter().map(|e| *e as f32).collect();
            s.cameras.extend(sol.camera(frame));
            s.tag(id);
            Ok(Some(Arc::new(s)))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Doc;

    /// A node whose next evaluation panics (see `crash_test_worker`).
    pub static PANIC_ON: Mutex<Option<String>> = Mutex::new(None);

    /// A panic in an evaluation is that frame's error (naming the node
    /// being evaluated), and the next request is served as usual.
    #[test]
    fn a_panicking_evaluation_is_an_error_and_the_worker_goes_on() {
        let graph = Arc::new(serde_json::from_value::<Graph>(serde_json::json!({ "nodes": [{ "id": "panicky_c", "op": "constant", "params": { "width": 8, "height": 8 } }] })).unwrap());
        let eval = Arc::new(Mutex::new(Evaluator::new()));
        let worker = Worker::spawn(eframe::egui::Context::default(), eval);
        let ask = |w: &Worker| {
            w.request(Request { graph: graph.clone(), target: "panicky_c".into(), frame: 1, probe: None });
            let t0 = Instant::now();
            loop {
                if let Some(r) = w.poll() {
                    break r;
                }
                assert!(t0.elapsed().as_secs() < 30, "no answer");
                std::thread::yield_now();
            }
        };
        *PANIC_ON.lock().unwrap() = Some("panicky_c".into());
        let err = ask(&worker).result.err().expect("an error").to_string();
        assert!(err.starts_with("internal error") && err.contains("a crash report was saved"), "{err}");
        assert!(ask(&worker).result.is_ok(), "served again");
    }

    /// A 3D setup with lights on the render's extra inputs, a render on a
    /// Merge's B with a roto on its A, and 3D nodes outside it.
    fn staged() -> Graph {
        serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "plate", "op": "constant", "params": { "width": 64, "height": 64 } },
            { "id": "card", "op": "card" },
            { "id": "spot", "op": "light_spot" },
            { "id": "scene", "op": "scene", "inputs": ["card", "spot"] },
            { "id": "cam", "op": "camera" },
            { "id": "sun", "op": "light_directional" },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam", "plate", "sun"] },
            { "id": "roto", "op": "roto" },
            { "id": "comp", "op": "merge", "inputs": ["roto", "render"] },
            { "id": "graded", "op": "grade", "inputs": ["comp"] },
            { "id": "other", "op": "scanline_render", "inputs": ["scene", "cam"] },
            { "id": "both", "op": "merge", "inputs": ["other", "render"] },
            { "id": "loose", "op": "card" },
            { "id": "cam2", "op": "camera" },
            { "id": "unrendered", "op": "scene", "inputs": ["cam2"] },
            { "id": "bulb", "op": "light_point" },
        ] }))
        .unwrap()
    }

    /// Viewing a 2D node after a render shows the render's scene: the
    /// nearest up its inputs, breadth-first, a Merge's B before its A.
    #[test]
    fn a_2d_node_after_a_render_shows_its_scene() {
        let g = staged();
        let src = |id: &str| scene_source(&g, id);
        assert_eq!((src("graded"), src("comp"), src("render"), src("card")), (Some("render".into()), Some("render".into()), Some("render".into()), Some("card".into())));
        assert_eq!(src("both").as_deref(), Some("render"), "B, the background, first");
        assert_eq!((src("plate"), src("roto"), src("nope")), (None, None, None));
        let mut eval = Evaluator::new();
        let viewed = src("graded").unwrap();
        let s = scene_of(&mut eval, &g, &viewed, 1).unwrap().unwrap();
        assert!(["card", "spot", "cam", "sun"].iter().all(|n| holds(&s, n)), "the render's scene, its camera and extra inputs");
    }

    /// Selected cards, cameras and lights the viewed scene doesn't hold come
    /// back as ghosts; those it holds, and nodes that aren't objects, don't.
    #[test]
    fn selected_objects_outside_the_scene_come_back_as_ghosts() {
        let g = staged();
        let mut eval = Evaluator::new();
        let s = scene_of(&mut eval, &g, "render", 1).unwrap().unwrap();
        let ask: Vec<String> = ["card", "cam", "sun", "loose", "cam2", "bulb", "graded", "unrendered"].map(String::from).to_vec();
        let (ghosts, errors) = ghosts_of(&mut eval, &g, Some(&s), &ask, 1);
        let ghosts = ghosts.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(["loose", "cam2", "bulb"].iter().all(|n| holds(&ghosts, n)));
        assert!(["card", "cam", "sun"].iter().all(|n| !holds(&ghosts, n)), "in the scene already");
        assert_eq!((ghosts.cards.len(), ghosts.cameras.len(), ghosts.lights.len()), (1, 1, 1), "not the Scene node's contents");
        // Viewing just the Scene: the render's camera is a ghost.
        let only = scene_of(&mut eval, &g, "scene", 1).unwrap().unwrap();
        assert!(holds(&ghosts_of(&mut eval, &g, Some(&only), &["cam".to_string()], 1).0.unwrap(), "cam"));
        assert!(ghosts_of(&mut eval, &g, Some(&s), &["card".to_string()], 1).0.is_none());
    }

    /// The 3D view of a blurred render: its shutter times (at most 8, over
    /// the same shutter) and scenes round them; the card's preview smears.
    #[test]
    fn the_3d_view_blurs_as_the_render_does() {
        use arclight::render3d::{render_preview, render_preview_blurred, Blur, Preview};
        let graph = |mb: i64| -> Graph {
            serde_json::from_value(serde_json::json!({
                "project": { "first_frame": 1, "last_frame": 10, "fps": 24, "format": { "width": 64, "height": 32 } },
                "nodes": [
                    { "id": "plate", "op": "constant", "params": { "width": 16, "height": 16 } },
                    { "id": "cam", "op": "camera", "params": { "focal": 18 } },
                    { "id": "card1", "op": "card", "inputs": ["plate"], "params": { "translate": { "keys": [[1, [-7.0, 0, -5]], [10, [7.0625, 0, -5]]], "interp": "linear" }, "size": [3, 3] } },
                    { "id": "r", "op": "scanline_render", "inputs": ["card1", "cam", ""], "params": { "motion_blur": mb, "shutter": 0.5 } },
                ] })).unwrap()
        };
        let mut eval = Evaluator::new();
        let none = blurred(&mut eval, &graph(0), "r", 5);
        assert!(none.0.is_empty() && none.1.is_empty(), "no blur");
        assert_eq!(blurred(&mut eval, &graph(1), "card1", 5).0, Vec::<f64>::new(), "not a render");
        let (times, then) = blurred(&mut eval, &graph(16), "r", 5);
        assert_eq!(times, Blur::times(-0.25, 0.5, VIEW_BLUR_TIMES), "8 times over the render's shutter");
        assert_eq!(view_blur_times(&graph(16), "r", 5), times, "the times alone (for the particles)");
        assert!(view_blur_times(&graph(0), "r", 5).is_empty() && view_blur_times(&graph(16), "card1", 5).is_empty());
        assert_eq!(then.iter().map(|t| t.0).collect::<Vec<_>>(), [-1, 1]);
        // Its preview: soft edges 5 px wide, as the render's.
        let g = graph(4);
        let now = scene_of(&mut eval, &g, "r", 5).unwrap().unwrap();
        let (times, then) = blurred(&mut eval, &g, "r", 5);
        assert_eq!(times.len(), 4);
        let cam = now.cameras[0].clone();
        let size = arclight::image::Size { width: 64, height: 32 };
        let blur = Blur { times, frames: std::iter::once((0, &*now, cam.clone())).chain(then.iter().map(|(k, s)| (*k, &**s, cam.clone()))).collect(), jitter: true };
        let (sharp, soft) = (render_preview(&now, &cam, size, Preview::Solid), render_preview_blurred(&now, &cam, size, Preview::Solid, Some(&blur)));
        let partly = |img: &Image| (0..64).filter(|&x| (0.02..0.98).contains(&img.at(x, 16)[3])).count();
        assert!(partly(&sharp) <= 2 && partly(&soft) >= 6, "{} {}", partly(&sharp), partly(&soft));
        assert_eq!(render_preview_blurred(&now, &cam, size, Preview::Solid, None).pixels, sharp.pixels, "unblurred as before");
    }

    /// Looking through a moving camera at a still card, the view blurs as
    /// the render does; the view's own camera doesn't move.
    #[test]
    fn a_looked_through_camera_s_motion_blurs_the_3d_view() {
        use crate::viewport3d::Through;
        use arclight::render3d::{render_preview, render_preview_blurred, Blur, Preview};
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "plate", "op": "constant", "params": { "width": 16, "height": 16 } },
            { "id": "cam", "op": "camera", "params": { "focal": 18, "translate": { "keys": [[1, [7.0, 0, 0]], [10, [-7.0625, 0, 0]]], "interp": "linear" } } },
            { "id": "card1", "op": "card", "inputs": ["plate"], "params": { "translate": [0, 0, -5], "size": [3, 3] } },
            { "id": "r", "op": "scanline_render", "inputs": ["card1", "cam", ""], "params": { "motion_blur": 4, "shutter": 0.5 } },
        ] })).unwrap();
        let mut eval = Evaluator::new();
        let now = scene_of(&mut eval, &graph, "r", 5).unwrap().unwrap();
        let (times, then) = blurred(&mut eval, &graph, "r", 5);
        let (cam, size) = (now.cameras[0].clone(), arclight::image::Size { width: 64, height: 32 });
        let through = (Through::Camera(0), size);
        let preview = |through: Option<&(Through, arclight::image::Size)>| {
            let frames = std::iter::once((0, &*now, cam.clone())).chain(then.iter().map(|(k, s)| (*k, &**s, frame_camera(&cam, s, through)))).collect();
            render_preview_blurred(&now, &cam, size, Preview::Solid, Some(&Blur { times: times.clone(), frames, jitter: true }))
        };
        let partly = |img: &Image| (0..64).filter(|&x| (0.02..0.98).contains(&img.at(x, 16)[3])).count();
        let (sharp, own, moving) = (render_preview(&now, &cam, size, Preview::Solid), preview(None), preview(Some(&through)));
        assert!(partly(&sharp) <= 2 && partly(&own) <= 2, "the view's camera stays: {} {}", partly(&sharp), partly(&own));
        assert!(partly(&moving) >= 6, "the camera's motion blurs: {}", partly(&moving));
        assert_ne!(frame_camera(&cam, &then[0].1, Some(&through)).world, cam.world, "found at the frame before");
        assert_eq!(frame_camera(&cam, &then[0].1, Some(&(Through::Camera(3), size))).world, cam.world, "or the view's when it's gone");
    }

    /// Splats render only once the view has settled (asked for), and the
    /// answer says so; unsettled, a splats-only scene renders nothing (the
    /// viewport draws their centers).
    #[test]
    fn settled_views_render_the_splats() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "cap", "op": "read_splat", "params": { "file": file, "translate": [0, 0, -6] } },
        ] }))
        .unwrap();
        let worker = SceneWorker::spawn(eframe::egui::Context::default());
        let size = arclight::image::Size { width: 64, height: 32 };
        let cam = arclight::geom::Camera { focal: 18.0, ..Default::default() };
        let ask = |splats: bool| {
            worker.request(SceneRequest { graph: Arc::new(graph.clone()), node: "cap".into(), frame: 1, render: Some((cam.clone(), size)), solid: true, preview: arclight::render3d::Preview::Solid, blur: splats, splats, through: None, rect: None, ghosts: Vec::new() });
            let t0 = Instant::now();
            loop {
                if let Some(r) = worker.poll() {
                    break r;
                }
                assert!(t0.elapsed().as_secs() < 30, "the worker answers");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let moving = ask(false);
        assert!(moving.render.is_none() && !moving.splats);
        let settled = ask(true);
        assert!(settled.splats, "says it has them");
        let img = settled.render.expect("a render");
        assert!(img.pixels.iter().any(|p| p[3] > 0.5), "the capture shows");
    }

    #[test]
    fn a_new_camera_setup_sees_a_card_and_the_renders_extra_inputs() {
        // The hand test: a 3D setup, a Card of a picture in its Scene, an
        // Area Light on the render's growable input.
        let mut d = Doc::new(Graph::default(), None);
        let plate = d.add_node("constant", [0.0, 0.0]);
        d.set_param(&plate, "color", serde_json::json!([1.0, 0.5, 0.25, 1.0]));
        let card = d.add_node("card", [0.0, 100.0]);
        d.connect(&plate, &card, 0);
        let (_, scene, render) = d.add_camera_rig([300.0, 300.0], None, Some(&card));
        assert_eq!(d.graph.node(&scene).unwrap().inputs[1], card);
        let light = d.add_node("light_area", [500.0, 100.0]);
        d.commit(false);
        assert!(d.connect(&light, &render, 3));
        let mut eval = Evaluator::new();
        let s = scene_of(&mut eval, &d.graph, &render, 1).unwrap().unwrap();
        assert_eq!((s.cameras.len(), s.cards.len(), s.lights.len()), (1, 1, 1), "the extra input's light is in the 3D view too");
        // The render isn't black: the card fills the camera's frame.
        let img = eval.evaluate(&d.graph, &render, &EvalContext { frame: 1, write_outputs: false }).unwrap();
        let (w, h) = (img.width, img.pixels.len() / img.width);
        for (x, y) in [(w / 2, h / 2), (w / 20, h / 2), (w - 1 - w / 20, h / 2)] {
            let p = img.get(x, y);
            assert!((p[0] - 1.0).abs() < 0.02 && (p[1] - 0.5).abs() < 0.02 && (p[3] - 1.0).abs() < 0.02, "the card at ({x}, {y}): {p:?}");
        }
    }
}
