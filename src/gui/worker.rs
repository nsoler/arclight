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
                    // Only cards and solid meshes are rendered; the viewport
                    // draws the rest.
                    let render = match (&scene, &req.render) {
                        (Ok(Some(s)), Some((cam, format))) if !s.cards.is_empty() || (req.solid && !s.meshes.is_empty()) => {
                            let meshes = if req.solid { s.meshes.clone() } else { Vec::new() };
                            let shown = arclight::scene::Scene { cards: s.cards.clone(), meshes, ..Default::default() };
                            Some(Arc::new(arclight::render3d::render_preview(&shown, cam, *format)))
                        }
                        _ => None,
                    };
                    let resp = SceneResponse { node: req.node, scene, render, rect: req.rect, millis: t0.elapsed().as_secs_f32() * 1000.0 };
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

/// The scene node `id` shows at `frame`: its own output for 3D nodes, a
/// Scanline Render's scene and camera inputs together, a Camera Tracker's
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
            let input = |i: usize| node.inputs.get(i).cloned().unwrap_or_default();
            let mut all = Scene::default();
            for src in [input(0), input(1)] {
                if let Some(s) = out_of(&src)? {
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
