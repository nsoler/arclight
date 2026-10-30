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
use std::sync::Arc;
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
    pub fn spawn(repaint: eframe::egui::Context) -> Worker {
        let (req_tx, req_rx) = channel::<Request>();
        let (resp_tx, resp_rx) = channel::<Response>();
        std::thread::Builder::new()
            .name("arclight-eval".into())
            .spawn(move || run(req_rx, resp_tx, repaint))
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

fn run(rx: Receiver<Request>, tx: Sender<Response>, repaint: eframe::egui::Context) {
    let mut eval = Evaluator::new();
    let mut frames = FrameCache { entries: VecDeque::new(), bytes: 0 };
    // Probed inputs by cache key, so frames served from the frame cache
    // needn't render the input again. Kept small: they're whole images.
    let mut probed: HashMap<u64, Arc<Image>> = HashMap::new();

    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let start = Instant::now();
        let ctx = EvalContext { frame: req.frame, write_outputs: false };
        let result = match Evaluator::key(&req.graph, &req.target, &ctx) {
            Err(e) => Err(e),
            Ok(key) => match frames.get(key) {
                Some(img) => Ok(img),
                None => {
                    let r = eval.evaluate(&req.graph, &req.target, &ctx);
                    if let Ok(img) = &r {
                        frames.insert(key, img.clone());
                    }
                    r
                }
            },
        };
        let millis = start.elapsed().as_secs_f32() * 1000.0;
        let probe = req.probe.and_then(|(node, input)| {
            let key = Evaluator::key(&req.graph, &input, &ctx).ok()?;
            if !probed.contains_key(&key) {
                let img = eval.evaluate(&req.graph, &input, &ctx).ok()?;
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

        let resp = Response {
            target: req.target,
            frame: req.frame,
            result,
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
    /// What the render camera looks through (and the project's frame size,
    /// for a light): with blur, it's found again in the scene at each frame
    /// round the shutter, so its own motion blurs the view too.
    pub through: Option<(crate::viewport3d::Through, arclight::image::Size)>,
    /// Where the render goes on screen (echoed back).
    pub rect: Option<eframe::egui::Rect>,
}

pub struct SceneResponse {
    pub node: String,
    /// The node's scene: None when it shows none (an image node).
    pub scene: Result<Option<Arc<arclight::scene::Scene>>, String>,
    pub render: Option<Arc<Image>>,
    pub rect: Option<eframe::egui::Rect>,
    pub millis: f32,
    /// The shutter times (frames on from the frame) a settled view of a
    /// render with motion blur shows: the cards blurred over them, and the
    /// particles streaked. Empty otherwise.
    pub blur_times: Vec<f64>,
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
                    let scene = scene_of(&mut eval, &req.graph, &req.node, req.frame);
                    let blur_times = if req.blur { view_blur_times(&req.graph, &req.node, req.frame) } else { Vec::new() };
                    // Only cards and solid meshes are rendered; the viewport
                    // draws the rest.
                    let render = match (&scene, &req.render) {
                        (Ok(Some(s)), Some((cam, format))) if !s.cards.is_empty() || (req.solid && !s.meshes.is_empty()) => {
                            let lit = matches!(req.preview, arclight::render3d::Preview::Lit { .. });
                            let shown = |s: &arclight::scene::Scene| {
                                let meshes = if req.solid { s.meshes.clone() } else { Vec::new() };
                                let lights = if lit { s.lights.clone() } else { Vec::new() };
                                arclight::scene::Scene { cards: s.cards.clone(), meshes, lights, ..Default::default() }
                            };
                            let now = shown(s);
                            let (times, then) = if blur_times.is_empty() { Default::default() } else { blurred(&mut eval, &req.graph, &req.node, req.frame) };
                            let then: Vec<(i64, arclight::scene::Scene, arclight::geom::Camera)> = then.iter().map(|(k, s)| (*k, shown(s), frame_camera(cam, s, req.through.as_ref()))).collect();
                            // Jittered as the render is (Jitter shutter).
                            let jitter = req.graph.node(&req.node).is_none_or(|n| arclight::ops::blur_jitter(&n.params));
                            let blur = arclight::render3d::Blur { times, frames: std::iter::once((0, &now, cam.clone())).chain(then.iter().map(|(k, s, c)| (*k, s, c.clone()))).collect(), jitter };
                            Some(Arc::new(arclight::render3d::render_preview_blurred(&now, cam, *format, req.preview, Some(&blur))))
                        }
                        _ => None,
                    };
                    let resp = SceneResponse { node: req.node, scene, render, rect: req.rect, millis: t0.elapsed().as_secs_f32() * 1000.0, blur_times };
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
