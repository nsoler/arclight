//! The node graph: a JSON-described DAG, topologically sorted and evaluated.
//!
//! [`Evaluator`] caches each node's last result under a key hashed from the
//! node's op, params, inputs' keys and (for time-varying ops) the frame. A
//! change to one node therefore only re-renders that node and its downstream.

use crate::image::{IRect, Image, Size};
use crate::ops::{self, Category, EvalContext};
use crate::particle_nodes;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

/// One node in the graph.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NodeSpec {
    pub id: String,
    pub op: String,
    /// Upstream node ids, one per input port. An empty string is an
    /// unconnected port.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub params: Value,
    /// Position in the node editor (ignored by the engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 2]>,
    /// Turned off (bypassed): it passes its main input on unchanged (see
    /// [`ops::bypass_port`]), or outputs nothing; an output writes nothing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A node's size in the node editor.
pub const NODE_W: f32 = 150.0;
pub const NODE_H: f32 = 46.0;

impl NodeSpec {
    /// Its box in the node editor (min and max corners); one never placed
    /// counts at 0,0.
    pub fn bounds(&self) -> ([f32; 2], [f32; 2]) {
        let p = self.pos.unwrap_or([0.0, 0.0]);
        (p, [p[0] + NODE_W, p[1] + NODE_H])
    }
}

/// Timeline settings for interactive playback.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Project {
    pub first_frame: i64,
    pub last_frame: i64,
    pub fps: f64,
    /// The frame size for sources with no input to take one from (a
    /// Constant, a shape without a background) whose own size is 0.
    #[serde(default = "default_format")]
    pub format: Size,
    /// The project's folder, where its media are read from and renders
    /// written to by default (None: the folder its file is in, if saved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<std::path::PathBuf>,
}

impl Project {
    /// Its timing, for formulas' `time` and `fps`.
    pub fn clock(&self) -> crate::expr::Clock {
        crate::expr::Clock { fps: self.fps, first_frame: self.first_frame as f64 }
    }

    /// Its folder: the one it was given, else that of its file (`file`,
    /// where it's saved, if it is).
    pub fn folder_for(&self, file: Option<&Path>) -> Option<std::path::PathBuf> {
        self.folder.clone().or_else(|| std::path::absolute(file?).ok()?.parent().map(Path::to_path_buf))
    }

    /// A file path someone gave (in a param) as the graph keeps it: one
    /// given relative to the project's folder (see
    /// [`folder_for`](Self::folder_for), the project saved at `file`) made
    /// whole; others as they are.
    pub fn resolve_path(&self, file: Option<&Path>, path: &str) -> String {
        match self.folder_for(file) {
            Some(folder) if !path.is_empty() && Path::new(path).is_relative() => clean(&folder.join(path)).to_string_lossy().into_owned(),
            _ => path.to_string(),
        }
    }

    /// [`resolve_path`](Self::resolve_path)'s way back, for showing a path:
    /// one in the project's folder relative to it; others as they are.
    pub fn relative_path(&self, file: Option<&Path>, path: &str) -> String {
        let Some(folder) = self.folder_for(file) else { return path.to_string() };
        match Path::new(path).strip_prefix(&folder) {
            Ok(rel) if Path::new(path).is_absolute() && !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
            _ => path.to_string(),
        }
    }

    /// [`resolve_path`](Self::resolve_path) for `op`'s file params among
    /// `params` (plain text ones).
    pub fn resolve_paths(&self, file: Option<&Path>, op: &str, params: &mut serde_json::Map<String, Value>) {
        let Some(info) = ops::info(op) else { return };
        for spec in info.params.iter().filter(|s| matches!(s.kind, ops::ParamKind::Path { .. })) {
            if let Some(Value::String(p)) = params.get_mut(spec.name) {
                *p = self.resolve_path(file, p);
            }
        }
    }
}

fn default_format() -> Size {
    ops::DEFAULT_FORMAT
}

impl Default for Project {
    fn default() -> Self {
        Project { first_frame: 1, last_frame: 100, fps: 24.0, format: ops::DEFAULT_FORMAT, folder: None }
    }
}

/// `path` made absolute against the working directory, `.` and `..`
/// taken out (without looking at the disk).
fn absolute(path: &Path) -> std::path::PathBuf {
    clean(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// `path` with `.` and `..` taken out, as far as it can be.
fn clean(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Call `f` on every file path in `nodes`' params (their ops' Path
/// params given as plain text; empty ones left out).
fn for_each_path(nodes: &mut [NodeSpec], mut f: impl FnMut(&mut String)) {
    for n in nodes {
        let Some(info) = ops::info(&n.op) else { continue };
        for spec in info.params.iter().filter(|s| matches!(s.kind, ops::ParamKind::Path { .. })) {
            if let Some(Value::String(p)) = n.params.get_mut(spec.name) {
                if !p.is_empty() {
                    f(p);
                }
            }
        }
    }
}

/// A group box drawn behind nodes in the node editor (ignored by the
/// engine). Nodes belong to it by position only: those fully inside it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Backdrop {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Top-left corner and size, in node editor coordinates.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// sRGB, 0..1.
    #[serde(default = "default_backdrop_color")]
    pub color: [f32; 3],
    #[serde(default = "default_backdrop_font")]
    pub font_size: f32,
}

fn default_backdrop_color() -> [f32; 3] {
    Backdrop::COLORS[0]
}

fn default_backdrop_font() -> f32 {
    Backdrop::FONT_SIZE
}

impl Backdrop {
    /// The palette offered for backdrops (muted, so nodes stay readable
    /// on them): slate, teal, green, olive, amber, rust, plum, blue.
    pub const COLORS: [[f32; 3]; 8] = [
        [0.36, 0.40, 0.46],
        [0.25, 0.48, 0.47],
        [0.30, 0.48, 0.30],
        [0.47, 0.47, 0.25],
        [0.58, 0.44, 0.20],
        [0.56, 0.30, 0.24],
        [0.45, 0.30, 0.48],
        [0.26, 0.36, 0.58],
    ];
    /// The palette's names, in order.
    pub const COLOR_NAMES: [&str; 8] = ["slate", "teal", "green", "olive", "amber", "rust", "plum", "blue"];
    pub const FONT_SIZE: f32 = 14.0;
    /// The title sizes offered.
    pub const FONT_RANGE: std::ops::RangeInclusive<f32> = 8.0..=72.0;
    /// The smallest size a box is resized to.
    pub const MIN_SIZE: [f32; 2] = [120.0, 60.0];
    /// Room around the nodes it's made to fit, and above them for the title.
    pub const PAD: f32 = 20.0;
    pub const TITLE_PAD: f32 = 30.0;

    /// A backdrop around `rects` (min and max corners), with padding and
    /// room for its title; None for no rects.
    pub fn around(id: &str, title: &str, rects: impl IntoIterator<Item = ([f32; 2], [f32; 2])>) -> Option<Backdrop> {
        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
        for (a, b) in rects {
            for k in 0..2 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        if !lo[0].is_finite() {
            return None;
        }
        let pos = [lo[0] - Self::PAD, lo[1] - Self::PAD - Self::TITLE_PAD];
        let size = [hi[0] - lo[0] + 2.0 * Self::PAD, hi[1] - lo[1] + 2.0 * Self::PAD + Self::TITLE_PAD];
        Some(Backdrop { id: id.into(), title: title.into(), pos, size, color: Self::COLORS[0], font_size: Self::FONT_SIZE })
    }

    /// Whether the box from `a` to `b` lies fully inside it.
    pub fn contains(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        (0..2).all(|k| a[k] >= self.pos[k] && b[k] <= self.pos[k] + self.size[k])
    }
}

/// A whole graph document.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub project: Project,
    /// Group boxes in the node editor, back to front.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backdrops: Vec<Backdrop>,
    /// How the GUI's views were left (the 3D viewport's camera), saved with
    /// the project but not part of it (see [`ViewState`]).
    #[serde(default, skip_serializing_if = "ViewState::is_empty")]
    pub view: ViewState,
}

impl FromStr for Graph {
    type Err = anyhow::Error;

    /// A graph from its JSON.
    fn from_str(text: &str) -> Result<Graph> {
        serde_json::from_str(text).context("parsing graph JSON")
    }
}

/// View settings saved with a project that aren't part of its content:
/// they always compare equal, so changing them is no edit (no undo step,
/// no unsaved-changes mark) and undo leaves them alone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewState(pub serde_json::Map<String, Value>);

impl PartialEq for ViewState {
    fn eq(&self, _: &ViewState) -> bool {
        true
    }
}

impl ViewState {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An evaluation failure, attributed to a node where possible.
#[derive(Clone, Debug)]
pub struct NodeError {
    pub node: Option<String>,
    pub message: String,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node {
            Some(n) => write!(f, "{n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for NodeError {}

fn node_err(node: &str, message: impl Into<String>) -> NodeError {
    NodeError { node: Some(node.to_string()), message: message.into() }
}

impl Graph {
    /// The graph saved at `path`, its paths as [`save`](Self::save) wrote
    /// them made whole again: a folder given relative to the file, and
    /// files given relative to the folder.
    pub fn from_file(path: &Path) -> Result<Graph> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading graph {}", path.display()))?;
        let mut g = Self::from_str(&text)?;
        g.resolve_paths(path);
        Ok(g)
    }

    /// Save it at `path` (see [`to_json_at`](Self::to_json_at)).
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json_at(Some(path))).with_context(|| format!("writing {}", path.display()))
    }

    /// Its JSON as saved at `file` (None: not in a file, as an autosave of
    /// an unsaved project). With a project folder, the files under it
    /// (read, written) are given relative to it, and the folder relative
    /// to the file when the file is in it, so the whole folder can move:
    /// [`from_file`](Self::from_file) makes them whole again. Files
    /// elsewhere stay absolute; without a folder nothing changes.
    pub fn to_json_at(&self, file: Option<&Path>) -> String {
        let Some(folder) = self.project.folder.as_deref().map(absolute) else { return self.to_json() };
        let mut g = self.clone();
        for_each_path(&mut g.nodes, |p| {
            let abs = absolute(Path::new(p.as_str()));
            let rel = abs.strip_prefix(&folder).ok().filter(|r| !r.as_os_str().is_empty());
            *p = rel.unwrap_or(&abs).to_string_lossy().into_owned();
        });
        let file_dir = file.map(absolute).and_then(|f| f.parent().map(Path::to_path_buf));
        g.project.folder = Some(match file_dir.as_deref().and_then(|d| d.strip_prefix(&folder).ok()) {
            // The file is inside it: up from the file's folder to it.
            Some(down) => down.components().fold(std::path::PathBuf::from("."), |up, _| if up == Path::new(".") { "..".into() } else { up.join("..") }),
            None => folder,
        });
        g.to_json()
    }

    /// Make the paths [`to_json_at`](Self::to_json_at) shortened, for a
    /// graph read from `file`, whole again.
    fn resolve_paths(&mut self, file: &Path) {
        let Some(folder) = self.project.folder.clone() else { return };
        let file_dir = absolute(file).parent().map(Path::to_path_buf).unwrap_or_default();
        let folder = clean(&file_dir.join(folder));
        for_each_path(&mut self.nodes, |p| {
            if Path::new(p.as_str()).is_relative() {
                *p = clean(&folder.join(p.as_str())).to_string_lossy().into_owned();
            }
        });
        self.project.folder = Some(folder);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("graph serializes")
    }

    pub fn backdrop(&self, id: &str) -> Option<&Backdrop> {
        self.backdrops.iter().find(|b| b.id == id)
    }

    /// A backdrop id not yet used: "backdrop1", "backdrop2", …
    pub fn new_backdrop_id(&self) -> String {
        (1..).map(|i| format!("backdrop{i}")).find(|id| self.backdrop(id).is_none()).expect("ids run out")
    }

    /// Place nodes in the node editor: those with no position (`all`: every
    /// node) go in a column per depth from the sources, left to right, one
    /// row per node down each column.
    pub fn layout(&mut self, all: bool) {
        if all {
            for n in &mut self.nodes {
                n.pos = None;
            }
        }
        if self.nodes.iter().all(|n| n.pos.is_some()) {
            return;
        }
        let mut depth: HashMap<String, usize> = HashMap::new();
        // Longest-path depth; bounded passes so a malformed cyclic file can't hang.
        for _ in 0..self.nodes.len() {
            let mut changed = false;
            for n in &self.nodes {
                let d = n.inputs.iter().filter_map(|i| depth.get(i)).map(|d| d + 1).max().unwrap_or(0);
                if depth.get(&n.id) != Some(&d) {
                    depth.insert(n.id.clone(), d);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut rows: HashMap<usize, usize> = HashMap::new();
        for n in &mut self.nodes {
            if n.pos.is_none() {
                let col = depth.get(&n.id).copied().unwrap_or(0);
                let row = rows.entry(col).or_insert(0);
                n.pos = Some([col as f32 * (NODE_W + 50.0), *row as f32 * (NODE_H + 40.0)]);
                *row += 1;
            }
        }
    }

    pub fn node(&self, id: &str) -> Option<&NodeSpec> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Evaluate the whole graph at frame 1 with writes enabled, returning
    /// every node's output image keyed by id.
    pub fn evaluate(&self) -> Result<HashMap<String, Arc<Image>>> {
        let ctx = EvalContext { frame: 1, write_outputs: true };
        Ok(Evaluator::new().evaluate_all(self, &ctx)?)
    }

    fn index(&self) -> Result<HashMap<&str, &NodeSpec>, NodeError> {
        let mut index = HashMap::with_capacity(self.nodes.len());
        for n in &self.nodes {
            if index.insert(n.id.as_str(), n).is_some() {
                return Err(node_err(&n.id, "duplicate node id"));
            }
        }
        Ok(index)
    }

    /// Depth-first topological order of `roots` and everything upstream of
    /// them, with cycle and dangling-edge detection.
    fn order(
        index: &HashMap<&str, &NodeSpec>,
        roots: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, NodeError> {
        // 0 = unvisited, 1 = on stack, 2 = done
        let mut state: HashMap<&str, u8> = index.keys().map(|k| (*k, 0u8)).collect();
        let mut order: Vec<String> = Vec::new();
        // Iterative DFS to avoid stack overflow on deep graphs.
        let mut stack: Vec<(&str, usize)> = Vec::new();

        for root in roots {
            let Some((&root, _)) = index.get_key_value(root.as_str()) else {
                return Err(node_err(&root, "no such node"));
            };
            if state[root] != 0 {
                continue;
            }
            stack.push((root, 0));
            while let Some((id, dep_i)) = stack.pop() {
                let node = index[id];
                if dep_i == 0 {
                    state.insert(id, 1);
                }
                if dep_i < node.inputs.len() {
                    stack.push((id, dep_i + 1));
                    let dep = node.inputs[dep_i].as_str();
                    if dep.is_empty() {
                        continue;
                    }
                    let dep_state = *state
                        .get(dep)
                        .ok_or_else(|| node_err(id, format!("references unknown input '{dep}'")))?;
                    match dep_state {
                        0 => stack.push((dep, 0)),
                        1 => return Err(node_err(dep, "graph has a cycle involving this node")),
                        _ => {}
                    }
                } else {
                    state.insert(id, 2);
                    order.push(id.to_string());
                }
            }
        }
        Ok(order)
    }
}

/// A node's cached result: on the CPU, or left on the GPU for the next
/// GPU op (read back when something on the CPU needs it, then kept).
#[derive(Clone)]
enum Res {
    Cpu(Arc<Image>),
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    Gpu(Arc<crate::gpu::Resident>),
}

#[cfg(test)]
impl Res {
    fn cpu_now(self) -> Arc<Image> {
        match self {
            Res::Cpu(img) => img,
            Res::Gpu(r) => r.cpu().unwrap(),
        }
    }
}

/// `res` on the CPU. A failed read-back (a GPU error) sets `failed`, so
/// the evaluation is done again on the CPU (see [`Evaluator::evaluate`]).
fn read(res: &Res, id: &str, failed: &mut bool) -> Result<Arc<Image>, NodeError> {
    match res {
        Res::Cpu(img) => Ok(img.clone()),
        Res::Gpu(r) => r.cpu().map_err(|e| {
            *failed = true;
            node_err(id, format!("reading its result back from the GPU: {e:#}"))
        }),
    }
}

/// Evaluates graphs, keeping each node's latest result for reuse.
#[derive(Default)]
pub struct Evaluator {
    cache: HashMap<String, (u64, Res)>,
    /// When each result left on the GPU was last made or used (a count),
    /// for evicting the least recently used past the budget.
    gpu_used: HashMap<String, u64>,
    gpu_tick: u64,
    /// A GPU result couldn't be read back during this evaluation.
    gpu_failed: bool,
    /// Results at other frames, by offset from the one evaluated, for
    /// nodes that need them (a Scanline Render's motion pass and motion
    /// blur: see [`ops::other_frames`]).
    others: std::collections::BTreeMap<i64, Evaluator>,
    /// Particle nodes' simulation caches and shapes.
    particles: particle_nodes::ParticleState,
    /// Particle nodes' inputs at other frames (a Particle Render's
    /// texture sequence, an emitter's geometry), by node.
    elsewhen: HashMap<String, Elsewhen>,
}

/// A particle node's input at other frames (see
/// [`particle_nodes::input_frames`]): rendered in an evaluator of its own;
/// a texture sequence's frames kept by their cache keys (so a frame
/// renders once, until it changes; an emitter keeps shapes made from them
/// instead).
#[derive(Default)]
struct Elsewhen {
    eval: Evaluator,
    images: HashMap<u64, Arc<Image>>,
}

/// An input needed at other frames, the frames, and their cache keys.
type FramesKeys = (String, Vec<i64>, Vec<u64>);

/// The input a particle node with (resolved) `params` at `frame` needs at
/// other frames (see [`particle_nodes::input_frames`]), the frames, and
/// their cache keys. None when it needs none.
fn input_frames(graph: &Graph, node: &NodeSpec, params: &Value, frame: i64) -> Result<Option<FramesKeys>, NodeError> {
    let wanted = particle_nodes::input_frames(&node.op, params, frame).map_err(|e| node_err(&node.id, format!("{e:#}")))?;
    let Some((port, frames)) = wanted else { return Ok(None) };
    let Some(dep) = node.inputs.get(port).filter(|d| !d.is_empty()) else { return Ok(None) };
    let frames: Vec<i64> = frames.collect();
    let keys = Evaluator::keys(graph, dep, &frames).map_err(|e| node_err(&node.id, format!("its input '{dep}': {e}")))?;
    Ok(Some((dep.clone(), frames, keys)))
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new evaluator that goes on from the particle simulations this one
    /// holds, where they still stand for the graph it's given, but never
    /// changes them (see [`particle_nodes::ParticleState::borrowing`]): a
    /// render alongside the viewer's, without re-simulating or holding it.
    pub fn borrowing_particles(&self) -> Evaluator {
        Evaluator { particles: self.particles.borrowing(), ..Self::default() }
    }

    /// Render `target` (and whatever it depends on) at `ctx.frame`.
    pub fn evaluate(
        &mut self,
        graph: &Graph,
        target: &str,
        ctx: &EvalContext,
    ) -> Result<Arc<Image>, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, [target.to_string()])?;
        let mut out = self.evaluate_order(&index, &order, &[target.to_string()], ctx, &graph.project)?;
        Ok(out.remove(target).expect("the target is read back"))
    }

    /// Render every node.
    pub fn evaluate_all(
        &mut self,
        graph: &Graph,
        ctx: &EvalContext,
    ) -> Result<HashMap<String, Arc<Image>>, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, graph.nodes.iter().map(|n| n.id.clone()))?;
        self.evaluate_order(&index, &order, &order, ctx, &graph.project)
    }

    /// [`evaluate`](Self::evaluate), and the result still on the GPU when
    /// it was made there (for a viewer drawing it without a conversion on
    /// the CPU; see `gpu::display`).
    pub fn evaluate_resident(&mut self, graph: &Graph, target: &str, ctx: &EvalContext) -> Result<(Arc<Image>, Option<Arc<crate::gpu::Resident>>), NodeError> {
        let img = self.evaluate(graph, target, ctx)?;
        let resident = match self.cache.get(target).map(|(_, r)| r) {
            Some(Res::Gpu(r)) => Some(r.clone()),
            _ => None,
        };
        Ok((img, resident))
    }

    /// Render every node, reading none back that nothing on the CPU
    /// needed (results may stay on the GPU): each node's id, frame and
    /// data window, in evaluation order. For renders that only want the
    /// writes done (`arclight render`).
    pub fn run_all(&mut self, graph: &Graph, ctx: &EvalContext) -> Result<Vec<(String, Size, IRect)>, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, graph.nodes.iter().map(|n| n.id.clone()))?;
        self.evaluate_order(&index, &order, &[], ctx, &graph.project)?;
        Ok(order
            .into_iter()
            .map(|id| {
                let (format, window) = match &self.cache[&id].1 {
                    Res::Cpu(img) => (img.format, img.window()),
                    Res::Gpu(r) => r.shape(),
                };
                (id, format, window)
            })
            .collect())
    }

    /// Run `order` and read back the results of `wanted`. Results never
    /// fail because of the GPU: when one can't be read back, the GPU's
    /// results are dropped and it all runs again on the CPU.
    fn evaluate_order(&mut self, index: &HashMap<&str, &NodeSpec>, order: &[String], wanted: &[String], ctx: &EvalContext, project: &Project) -> Result<HashMap<String, Arc<Image>>, NodeError> {
        self.cache.retain(|id, _| index.contains_key(id.as_str()));
        self.gpu_used.retain(|id, _| index.contains_key(id.as_str()));
        self.particles.retain(|id| index.contains_key(id));
        self.elsewhen.retain(|id, _| index.contains_key(id.as_str()));
        let attempt = |ev: &mut Evaluator| -> Result<HashMap<String, Arc<Image>>, NodeError> {
            ev.run(index, order, ctx, project)?;
            wanted.iter().map(|id| Ok((id.clone(), read(&ev.cache[id.as_str()].1, id, &mut ev.gpu_failed)?))).collect()
        };
        match attempt(self) {
            Err(_) if std::mem::take(&mut self.gpu_failed) => {
                self.cache.retain(|_, (_, r)| matches!(r, Res::Cpu(_)));
                crate::gpu::cpu_only(|| attempt(self))
            }
            other => other,
        }
    }

    /// The cache key `target` would have at `ctx.frame`, without rendering.
    /// Equal keys mean identical images, so callers can cache outputs by it.
    pub fn key(graph: &Graph, target: &str, ctx: &EvalContext) -> Result<u64, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, [target.to_string()])?;
        Self::key_in(graph, &index, &order, target, ctx)
    }

    /// [`key`](Self::key) at each of `frames` (not writing outputs).
    pub fn keys(graph: &Graph, target: &str, frames: &[i64]) -> Result<Vec<u64>, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, [target.to_string()])?;
        frames.iter().map(|&frame| Self::key_in(graph, &index, &order, target, &EvalContext { frame, write_outputs: false })).collect()
    }

    /// [`key`](Self::key) with `graph`'s index and `target`'s order made.
    fn key_in(graph: &Graph, index: &HashMap<&str, &NodeSpec>, order: &[String], target: &str, ctx: &EvalContext) -> Result<u64, NodeError> {
        let mut keys = HashMap::new();
        for id in order {
            let node = index[id.as_str()];
            if node.disabled {
                keys.insert(id.as_str(), bypass_key(node, &keys, &graph.project));
                continue;
            }
            let mut params = linked_params(index, node, ctx.frame, &graph.project)?;
            ops::apply_format(&node.op, &mut params, graph.project.format);
            let mut k = node_key(node, &params, &keys, ctx);
            if let Some((_, _, more)) = input_frames(graph, node, &params, ctx.frame)? {
                k = mix(k, &more);
            }
            for off in ops::other_frames(&node.op, &params) {
                let later = EvalContext { frame: ctx.frame + off, write_outputs: false };
                // (A frame that fails is left out, as in `run`.)
                let then: Option<Vec<u64>> = other_inputs(node).iter().flatten().map(|d| Evaluator::key(graph, d, &later).ok()).collect();
                k = mix(k, &[off as u64]);
                if let Some(then) = then {
                    k = mix(k, &then);
                }
            }
            if let Some((ports, at)) = ops::project3d::held_inputs(&node.op, &params) {
                let then = EvalContext { frame: at, write_outputs: false };
                for d in held_deps(node, ports) {
                    k = mix(k, &[Evaluator::key(graph, &d.1, &then)?]);
                }
            }
            keys.insert(id.as_str(), k);
        }
        Ok(keys[target])
    }

    fn run(
        &mut self,
        index: &HashMap<&str, &NodeSpec>,
        order: &[String],
        ctx: &EvalContext,
        project: &Project,
    ) -> Result<(), NodeError> {
        let graph_format = project.format;
        let mut keys: HashMap<&str, u64> = HashMap::with_capacity(order.len());
        // What an unconnected input sees: nothing, in the project's format.
        let empty = Image::blank(graph_format, IRect::default());
        // The graph as it is, for particle nodes to read their params at
        // other frames (made when the first one needs it).
        let mut snapshot: Option<Arc<Vec<NodeSpec>>> = None;
        for id in order {
            let node = index[id.as_str()];
            // Turned off: its input passed on (the same image), or nothing.
            if node.disabled {
                let key = bypass_key(node, &keys, project);
                keys.insert(id.as_str(), key);
                if !self.cache.get(id).is_some_and(|(k, _)| *k == key) {
                    let out = match bypass_input(node) {
                        Some(dep) => self.cache[dep].1.clone(),
                        None => Res::Cpu(Arc::new(nothing(&node.op, graph_format))),
                    };
                    self.cache.insert(id.clone(), (key, out));
                }
                continue;
            }
            // Keyframes and links are resolved first and the resolved values
            // hashed, so a curve that holds between frames still hits the
            // cache, and a changed link source re-renders.
            let mut params = linked_params(index, node, ctx.frame, project)?;
            ops::apply_format(&node.op, &mut params, graph_format);
            let mut key = node_key(node, &params, &keys, ctx);
            // A node that needs its inputs at other frames: render those (and
            // what they need) then too, and key on them. A frame they fail
            // at (say before a plate's first) is left out: the render does
            // without it.
            let mut others: Vec<(i64, Vec<Option<Arc<Image>>>)> = Vec::new();
            for off in ops::other_frames(&node.op, &params) {
                let deps = other_inputs(node);
                let roots: Vec<String> = deps.iter().flatten().cloned().collect();
                let later = self.others.entry(off).or_default();
                let ctx_then = EvalContext { frame: ctx.frame + off, write_outputs: false };
                if later.run(index, &Graph::order(index, roots)?, &ctx_then, project).is_err() {
                    key = mix(key, &[off as u64]);
                    continue;
                }
                let found: Vec<_> = deps.iter().map(|d| d.as_ref().map(|d| later.cache[d].clone())).collect();
                key = mix(key, &[off as u64]);
                key = mix(key, &found.iter().flatten().map(|f| f.0).collect::<Vec<_>>());
                let images = found.into_iter().zip(&deps).map(|(f, d)| f.map(|f| read(&f.1, d.as_deref().unwrap_or(id), &mut self.gpu_failed)).transpose());
                others.push((off, images.collect::<Result<_, _>>()?));
            }
            // An input needed at other frames (a texture sequence, an
            // emitter's geometry): keyed on its keys then too.
            let elsewhen = if !matches!(particle_nodes::input_frames(&node.op, &params, ctx.frame), Ok(None)) {
                let snap = snapshot.get_or_insert_with(|| Arc::new(index.values().map(|n| (*n).clone()).collect())).clone();
                let graph = Graph { nodes: (*snap).clone(), project: project.clone(), ..Graph::default() };
                input_frames(&graph, node, &params, ctx.frame)?.map(|s| (graph, s))
            } else {
                None
            };
            if let Some((_, (_, _, more))) = &elsewhen {
                key = mix(key, more);
            }
            // Inputs held at another frame (Project 3D's Frame hold):
            // rendered then, in an evaluator of their own, keyed on their
            // keys then.
            let mut held_at: Vec<(usize, Arc<Image>)> = Vec::new();
            if let Some((ports, at)) = ops::project3d::held_inputs(&node.op, &params) {
                let deps = held_deps(node, ports);
                let then = &mut self.elsewhen.entry(id.clone()).or_default().eval;
                let ctx_then = EvalContext { frame: at, write_outputs: false };
                then.run(index, &Graph::order(index, deps.iter().map(|d| d.1.clone()))?, &ctx_then, project).map_err(|e| node_err(id, format!("at its hold frame {at}: {e}")))?;
                for (port, dep) in deps {
                    let (k, res) = then.cache[&dep].clone();
                    key = mix(key, &[k]);
                    held_at.push((port, read(&res, &dep, &mut self.gpu_failed)?));
                }
            }
            keys.insert(id.as_str(), key);
            if self.cache.get(id).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            crate::crash::evaluating(&node.op, id);
            // A Viewer or Media Out passes its input on as it is: the same
            // result, not a copy (and still on the GPU when it's there).
            if matches!(node.op.as_str(), "viewer" | "media_out") {
                if let Some(dep) = node.inputs.first().filter(|d| !d.is_empty()) {
                    let res = self.cache[dep.as_str()].1.clone();
                    self.cache.insert(id.clone(), (key, res));
                    continue;
                }
            }
            // On the GPU when it has this op and it pays (see `gpu::ops`).
            if particle_nodes::info(&node.op).is_none() && others.is_empty() && elsewhen.is_none() && held_at.is_empty() {
                if let Some(out) = self.try_gpu(node, &params, &empty) {
                    self.cache.insert(id.clone(), (key, out));
                    self.touch(id);
                    self.enforce_gpu_budget();
                    continue;
                }
            }
            let mut kept = elsewhen.as_ref().map(|_| self.elsewhen.entry(id.clone()).or_default());
            // A texture sequence's frames, those not kept rendered now.
            let sequence: Option<Vec<Arc<Image>>> = match (&elsewhen, kept.as_deref_mut()) {
                (Some((graph, (dep, frames, more))), Some(kept)) if node.op == "particle_render" => {
                    kept.images.retain(|k, _| more.contains(k));
                    for (&g, k) in frames.iter().zip(more) {
                        if !kept.images.contains_key(k) {
                            let img = kept.eval.evaluate(graph, dep, &EvalContext { frame: g, write_outputs: false });
                            let img = img.map_err(|e| node_err(id, format!("its texture sequence at frame {g}: {e}")))?;
                            kept.images.insert(*k, img);
                        }
                    }
                    Some(more.iter().map(|k| kept.images[k].clone()).collect())
                }
                _ => None,
            };

            let particle = particle_nodes::info(&node.op);
            let info = ops::info(&node.op);
            let ports = info.map(|i| i.inputs).unwrap_or(&[]);
            // Outputs (viewer, writes) refuse to show or write nothing;
            // other ops take an unconnected input as empty, so one node left
            // unplugged doesn't blank everything below it.
            let strict = info.is_some_and(|i| i.category == Category::Output);
            let n_ports = ports.len().max(node.inputs.len());
            // The inputs on the CPU (read back from the GPU if they're there).
            let mut held: HashMap<&str, Arc<Image>> = HashMap::new();
            for dep in node.inputs.iter().filter(|d| !d.is_empty()) {
                if !held.contains_key(dep.as_str()) {
                    held.insert(dep, read(&self.cache[dep].1, dep, &mut self.gpu_failed)?);
                }
            }
            let mut inputs: Vec<Option<&Image>> = Vec::with_capacity(n_ports);
            for i in 0..n_ports {
                let dep = node.inputs.get(i).map_or("", String::as_str);
                if dep.is_empty() {
                    match ports.get(i).filter(|p| !p.optional) {
                        Some(port) if strict => {
                            return Err(node_err(id, format!("input '{}' is not connected", port.label)));
                        }
                        Some(_) => inputs.push(Some(&empty)),
                        None => inputs.push(None),
                    }
                } else {
                    let img = held[dep].as_ref();
                    // A scene only into a scene port, an image only into an
                    // image port (a hand-edited or old file may mix them).
                    if let Some(port) = ops::port(&node.op, i).filter(|p| !p.kind.takes(img)) {
                        let got = ops::article(ops::Kind::of(img));
                        return Err(node_err(id, format!("input '{}' takes {}, but '{dep}' outputs {got}", port.label, ops::article(port.kind))));
                    }
                    inputs.push(Some(img));
                }
            }
            for (port, img) in &held_at {
                inputs[*port] = Some(img);
            }
            let out = match &others {
                _ if particle.is_some() => {
                    let snap = snapshot.get_or_insert_with(|| Arc::new(index.values().map(|n| (*n).clone()).collect())).clone();
                    let input_keys: Vec<u64> = node.inputs.iter().map(|d| keys.get(d.as_str()).copied().unwrap_or(0)).collect();
                    // An emitter's geometry then, rendered as its shapes need it.
                    let mut render_then = |g: i64| -> Result<Arc<Image>> {
                        let (Some((graph, (dep, _, _))), Some(kept)) = (&elsewhen, kept.as_deref_mut()) else { anyhow::bail!("not available") };
                        kept.eval.evaluate(graph, dep, &EvalContext { frame: g, write_outputs: false }).map_err(|e| anyhow::anyhow!("{e}"))
                    };
                    let history = elsewhen
                        .as_ref()
                        .filter(|_| node.op != "particle_render")
                        .map(|(_, (_, frames, more))| particle_nodes::History { frames, keys: more, render: &mut render_then });
                    particle_node(node, snap, project, &inputs, &input_keys, sequence.as_deref(), history, ctx.frame, &mut self.particles)
                }
                others if !others.is_empty() => {
                    let others: Vec<(i64, Vec<Option<&Image>>)> = others.iter().map(|(k, v)| (*k, v.iter().map(|i| i.as_deref()).collect())).collect();
                    ops::scanline_render_frames(&params, &inputs, &others)
                }
                _ => ops::eval(&node.op, &params, &inputs, ctx),
            };
            let mut out = out.map_err(|e| node_err(id, format!("{e:#}")))?;
            // What a node made in 3D carries its name (for the viewport).
            if let Some(scene) = out.scene.as_mut() {
                Arc::make_mut(scene).tag(id);
            }
            self.cache.insert(id.clone(), (key, Res::Cpu(Arc::new(out))));
        }
        Ok(())
    }

    /// Mark result `id` as just used (for the GPU budget).
    fn touch(&mut self, id: &str) {
        self.gpu_tick += 1;
        self.gpu_used.insert(id.to_string(), self.gpu_tick);
    }

    /// Past the GPU budget ([`crate::gpu::cache_budget`]), the least
    /// recently used results on the GPU go back to the CPU.
    fn enforce_gpu_budget(&mut self) {
        self.evict_gpu_past(crate::gpu::cache_budget());
    }

    /// Results on the GPU back to the CPU, least recently used first,
    /// until they hold at most `budget` bytes there.
    fn evict_gpu_past(&mut self, budget: u64) {
        loop {
            let mut seen = std::collections::HashSet::new();
            let mut total = 0;
            for (_, r) in self.cache.values() {
                if let Res::Gpu(r) = r {
                    if seen.insert(Arc::as_ptr(r)) {
                        total += r.bytes();
                    }
                }
            }
            if total <= budget {
                return;
            }
            let lru = self.cache.iter().filter(|(_, (_, r))| matches!(r, Res::Gpu(_))).min_by_key(|(id, _)| self.gpu_used.get(*id).copied().unwrap_or(0)).map(|(id, _)| id.clone());
            let Some(id) = lru else { return };
            let res = &mut self.cache.get_mut(&id).expect("just found").1;
            match res {
                Res::Gpu(r) => match r.cpu() {
                    Ok(img) => *res = Res::Cpu(img),
                    // (Rendered again when next needed.)
                    Err(_) => {
                        self.cache.remove(&id);
                    }
                },
                Res::Cpu(_) => {}
            }
        }
    }

    /// `node` on the GPU, if it has a GPU path for these params and it
    /// pays (see [`crate::gpu::ops::preference`]): its inputs uploaded
    /// when they're on the CPU (and kept there, for the next GPU op).
    /// None: run it on the CPU (a GPU error is recorded).
    #[cfg(feature = "gpu")]
    fn try_gpu(&mut self, node: &NodeSpec, params: &Value, empty: &Image) -> Option<Res> {
        use crate::gpu::{self, ops::Prefer, GpuImage, Resident};
        if !gpu::OPS.contains(&node.op.as_str()) || !gpu::ops::supports(&node.op, params) {
            return None;
        }
        let gpu = gpu::get()?;
        let on_gpu = node.inputs.iter().any(|d| matches!(self.cache.get(d), Some((_, Res::Gpu(_)))));
        match gpu::ops::preference(&node.op, params) {
            Prefer::Never => return None,
            Prefer::WithResidentInput if !on_gpu => return None,
            _ => {}
        }
        let failed = |e: anyhow::Error| gpu::report_error(format!("{}: {e:#}", node.id), false);
        let ports = ops::info(&node.op).map(|i| i.inputs).unwrap_or(&[]);
        let mut inputs: Vec<Option<Arc<Resident>>> = Vec::new();
        for i in 0..ports.len().max(node.inputs.len()) {
            let dep = node.inputs.get(i).map_or("", String::as_str);
            if dep.is_empty() {
                // An unconnected required port sees the empty image.
                if ports.get(i).is_some_and(|p| !p.optional) {
                    let up = GpuImage::upload(&gpu, empty).map_err(failed).ok()?;
                    inputs.push(Some(Arc::new(Resident::uploaded(gpu.clone(), up, Arc::new(empty.clone())))));
                } else {
                    inputs.push(None);
                }
                continue;
            }
            let r = match self.cache.get(dep)?.1.clone() {
                Res::Gpu(r) => r,
                // A scene stays on the CPU, which refuses it in an image port.
                Res::Cpu(img) if img.scene.is_some() => return None,
                Res::Cpu(img) => {
                    let up = GpuImage::upload(&gpu, &img).map_err(failed).ok()?;
                    let r = Arc::new(Resident::uploaded(gpu.clone(), up, img));
                    self.cache.get_mut(dep)?.1 = Res::Gpu(r.clone());
                    r
                }
            };
            if r.image.scene.is_some() {
                return None;
            }
            self.touch(dep);
            inputs.push(Some(r));
        }
        let refs: Vec<Option<&GpuImage>> = inputs.iter().map(|r| r.as_ref().map(|r| &r.image)).collect();
        // (A Keyer reads its plate back to estimate the screen color.)
        let cpu = |i: usize| inputs.get(i).and_then(Option::as_ref).ok_or_else(|| anyhow::anyhow!("input {i} is not connected"))?.cpu();
        match gpu::ops::eval(&gpu, &node.op, params, &refs, &cpu) {
            Ok(out) => out.map(|out| Res::Gpu(Arc::new(Resident::new(gpu, out)))),
            Err(e) => {
                failed(e);
                None
            }
        }
    }

    #[cfg(not(feature = "gpu"))]
    fn try_gpu(&mut self, _node: &NodeSpec, _params: &Value, _empty: &Image) -> Option<Res> {
        None
    }
}

/// Evaluate particle node `node` (see [`particle_nodes`]): its params at
/// any frame read from `graph` (a snapshot), its particles input's stream
/// as its upstream, a stream out as a scene.
#[allow(clippy::too_many_arguments)]
fn particle_node<'a>(
    node: &'a NodeSpec,
    graph: Arc<Vec<NodeSpec>>,
    project: &Project,
    inputs: &'a [Option<&'a Image>],
    keys: &'a [u64],
    sequence: Option<&'a [Arc<Image>]>,
    history: Option<particle_nodes::History<'a>>,
    frame: i64,
    state: &mut particle_nodes::ParticleState,
) -> Result<Image> {
    let (id, at) = (node.id.clone(), project.clone());
    let own: particle_nodes::Own = Arc::new(move |g| {
        let index: HashMap<&str, &NodeSpec> = graph.iter().map(|n| (n.id.as_str(), n)).collect();
        index.get(id.as_str()).and_then(|n| linked_params(&index, n, g, &at).ok()).unwrap_or(Value::Null)
    });
    let upstream = inputs.first().copied().flatten().and_then(|i| i.scene.as_ref()).and_then(|s| s.particles.first());
    let cx = particle_nodes::Context { node: &node.id, frame, fps: project.fps, format: project.format, own, inputs, keys, upstream, sequence, history };
    Ok(match particle_nodes::eval(&node.op, cx, state)? {
        particle_nodes::Output::Image(img) => img,
        particle_nodes::Output::Stream(stream) => {
            let mut out = Image::blank(project.format, IRect::default());
            out.scene = Some(Arc::new(crate::scene::Scene { particles: vec![stream], ..Default::default() }));
            out
        }
    })
}

/// `node`'s params at `frame`: keyframes evaluated and live links
/// (`crate::link`) followed through the graph's other nodes. A link to a
/// node that's gone is an error naming it.
fn linked_params(index: &HashMap<&str, &NodeSpec>, node: &NodeSpec, frame: i64, project: &Project) -> Result<Value, NodeError> {
    let lookup = |id: &str| index.get(id).copied();
    let clock = project.clock();
    if let Some((param, gone)) = crate::link::missing(&lookup, &node.params) {
        return Err(node_err(&node.id, format!("'{param}' is linked to '{gone}', which doesn't exist")));
    }
    let links = node.params.as_object().into_iter().flatten().filter(|(_, v)| crate::link::is_link(v));
    for (param, v) in links {
        if let Some(e) = crate::link::error(&lookup, v, frame as f64, clock) {
            return Err(node_err(&node.id, format!("'{param}': {e}")));
        }
    }
    let follow = |v: &Value, t: f64| crate::link::resolve(&lookup, v, t, clock);
    Ok(ops::resolve_params_linked(&node.op, &node.params, frame, Some(&follow)))
}

/// `params` are the node's params already resolved at `ctx.frame`.
/// The inputs a node needing other frames wants then: a Scanline Render's
/// all but its background (None there and where unconnected).
fn other_inputs(node: &NodeSpec) -> Vec<Option<String>> {
    node.inputs.iter().enumerate().map(|(i, d)| (i != 2 && !d.is_empty()).then(|| d.clone())).collect()
}

/// The inputs connected at `ports` of `node`: (port, node id).
fn held_deps(node: &NodeSpec, ports: &[usize]) -> Vec<(usize, String)> {
    ports.iter().filter_map(|&p| node.inputs.get(p).filter(|d| !d.is_empty()).map(|d| (p, d.clone()))).collect()
}

/// `key` combined with `more` keys.
fn mix(key: u64, more: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    more.hash(&mut h);
    h.finish()
}

/// What disabled `node` passes on: the node at its bypass port, if one's
/// connected there.
fn bypass_input(node: &NodeSpec) -> Option<&str> {
    ops::bypass_port(&node.op).and_then(|p| node.inputs.get(p)).map(String::as_str).filter(|d| !d.is_empty())
}

/// What a disabled node with nothing to pass on outputs: a transparent
/// image of the project's format, or an empty scene.
fn nothing(op: &str, format: Size) -> Image {
    let mut img = Image::blank(format, IRect::default());
    if ops::output_kind(op) == ops::Kind::Scene {
        img.scene = Some(Arc::new(crate::scene::Scene::default()));
    }
    img
}

/// A disabled node's key: its input's, or (nothing to pass) the empty
/// output's format and kind; marked so it differs from the input's own.
fn bypass_key(node: &NodeSpec, keys: &HashMap<&str, u64>, project: &Project) -> u64 {
    const BYPASS: u64 = 0xb1a5_5ed0_0ff0_0ff0;
    match bypass_input(node) {
        Some(dep) => mix(BYPASS, &[keys.get(dep).copied().unwrap_or(0)]),
        None => mix(BYPASS, &[project.format.width as u64, project.format.height as u64, (ops::output_kind(&node.op) == ops::Kind::Scene) as u64]),
    }
}

fn node_key(node: &NodeSpec, params: &Value, keys: &HashMap<&str, u64>, ctx: &EvalContext) -> u64 {
    let mut h = DefaultHasher::new();
    node.op.hash(&mut h);
    params.to_string().hash(&mut h);
    // A particle node's past frames count too (a key moved earlier).
    let particle = particle_nodes::is_particle(&node.op);
    if particle {
        node.params.to_string().hash(&mut h);
    }
    for dep in &node.inputs {
        keys.get(dep.as_str()).copied().unwrap_or(0).hash(&mut h);
    }
    // CPU and GPU results are kept apart (see `gpu::backend_tag`).
    crate::gpu::backend_tag(&node.op).hash(&mut h);
    if particle || ops::is_time_varying(&node.op, params) {
        ctx.frame.hash(&mut h);
    }
    if node.op == "write" {
        ctx.write_outputs.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    /// A project folder with its plates, geometry and renders, saved, moved
    /// whole and opened again: what's in it is found where it went (read
    /// and written there); what's elsewhere stays put; a project without a
    /// folder saves and loads as it always did.
    #[test]
    fn a_project_folder_moves_whole() {
        use serde_json::json;
        let tmp = std::env::temp_dir().join(format!("arclight_folder_move_{}", std::process::id()));
        let (before, after) = (tmp.join("show_v1"), tmp.join("moved/show_v2"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(before.join("plates")).unwrap();
        std::fs::create_dir_all(before.join("renders")).unwrap();
        std::fs::create_dir_all(before.join("scripts")).unwrap();
        Image::filled(4, 2, [0.25, 0.5, 0.75, 1.0]).write(&before.join("plates/bg.0001.png")).unwrap();
        let outside = tmp.join("library/logo.png");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        Image::filled(2, 2, [1.0; 4]).write(&outside).unwrap();
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let at = |dir: &Path, rel: &str| dir.join(rel).to_string_lossy().into_owned();
        let mut g = Graph::default();
        g.project.folder = Some(before.clone());
        g.nodes = vec![
            node("plate", "read", &[], json!({ "path": at(&before, "plates/bg.####.png") })),
            node("logo", "read", &[], json!({ "path": outside.to_string_lossy() })),
            node("geo", "read_geo", &[], json!({ "file": at(&before, "geo/../geo/set.obj") })),
            node("out", "write", &["plate"], json!({ "path": at(&before, "renders/comp.####.png") })),
        ];
        // Saved in the folder's scripts/: relative there and to the folder.
        let file = before.join("scripts/comp.json");
        g.save(&file).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["project"]["folder"], "..");
        let paths: Vec<&Value> = saved["nodes"].as_array().unwrap().iter().map(|n| n["params"].get("path").unwrap_or(&n["params"]["file"])).collect();
        assert_eq!(paths, [&json!("plates/bg.####.png"), &json!(outside.to_string_lossy()), &json!("geo/set.obj"), &json!("renders/comp.####.png")]);
        // Moved whole: found in its new place.
        std::fs::create_dir_all(after.parent().unwrap()).unwrap();
        std::fs::rename(&before, &after).unwrap();
        let back = Graph::from_file(&after.join("scripts/comp.json")).unwrap();
        assert_eq!(back.project.folder.as_deref(), Some(after.as_path()));
        assert_eq!(back.nodes[0].params["path"], at(&after, "plates/bg.####.png"));
        assert_eq!(back.nodes[1].params["path"], outside.to_string_lossy().as_ref(), "outside: as it was");
        assert_eq!(back.nodes[2].params["file"], at(&after, "geo/set.obj"));
        // Rendered there: the plate read from the moved folder, written into it.
        let ctx = EvalContext { frame: 1, write_outputs: true };
        Evaluator::new().evaluate(&back, "out", &ctx).unwrap();
        let out = Image::read(&after.join("renders/comp.0001.png")).unwrap();
        assert!((out.at(1, 1)[2] - 0.75).abs() < 0.01, "{:?}", out.at(1, 1));
        assert!(!before.exists());
        // Saved again where it is: the same file.
        let again = after.join("scripts/again.json");
        back.save(&again).unwrap();
        assert_eq!(std::fs::read_to_string(&again).unwrap(), std::fs::read_to_string(after.join("scripts/comp.json")).unwrap());
        // Saved outside its folder: the folder absolute, the files still
        // relative to it.
        let elsewhere = tmp.join("elsewhere.json");
        back.save(&elsewhere).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&elsewhere).unwrap()).unwrap();
        assert_eq!(saved["project"]["folder"], after.to_string_lossy().as_ref());
        assert_eq!(Graph::from_file(&elsewhere).unwrap(), back);
        // No folder: as before, byte for byte.
        let mut plain = back.clone();
        plain.project.folder = None;
        plain.save(&elsewhere).unwrap();
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), plain.to_json());
        assert_eq!(Graph::from_file(&elsewhere).unwrap(), plain);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A path given relative means relative to the project's folder.
    #[test]
    fn relative_paths_given_are_in_the_project_folder() {
        let mut p = Project::default();
        assert_eq!(p.resolve_path(None, "plates/a.exr"), "plates/a.exr", "no folder: as given");
        assert_eq!(p.resolve_path(Some(Path::new("/shows/a/comp.json")), "plates/a.exr"), "/shows/a/plates/a.exr");
        p.folder = Some("/shows/b".into());
        assert_eq!(p.resolve_path(Some(Path::new("/x/comp.json")), "../b/plates/./a.####.exr"), "/shows/b/plates/a.####.exr");
        assert_eq!(p.resolve_path(None, "/abs/a.exr"), "/abs/a.exr");
        assert_eq!(p.resolve_path(None, ""), "");
        // Shown the other way.
        assert_eq!(p.relative_path(None, "/shows/b/plates/a.####.exr"), "plates/a.####.exr");
        assert_eq!(p.relative_path(None, "/shows/bb/a.exr"), "/shows/bb/a.exr", "a folder named alike isn't it");
        assert_eq!(p.relative_path(None, "/elsewhere/a.exr"), "/elsewhere/a.exr");
        for typed in ["plates/a.exr", "/elsewhere/a.exr", "a"] {
            assert_eq!(p.relative_path(None, &p.resolve_path(None, typed)), typed, "what's typed shows as typed");
        }
        let mut params = serde_json::json!({ "path": "out/r.####.png", "fit": "none" }).as_object().unwrap().clone();
        p.resolve_paths(None, "write", &mut params);
        assert_eq!(params["path"], "/shows/b/out/r.####.png");
    }

    /// A project's folder: saved when it has one, missing from older
    /// files (which load without), else the folder of its file.
    #[test]
    fn a_projects_folder_is_its_own_or_its_files() {
        let old: Graph = serde_json::from_str(r#"{"nodes": [], "project": {"first_frame": 1, "last_frame": 10, "fps": 24}}"#).unwrap();
        assert_eq!(old.project.folder, None);
        assert!(!old.to_json().contains("folder"), "none: nothing saved");
        assert_eq!(old.project.folder_for(Some(Path::new("/shows/a/comp.json"))), Some("/shows/a".into()));
        assert_eq!(old.project.folder_for(None), None);
        assert_eq!(old.project.folder_for(Some(Path::new("comp.json"))), Some(std::env::current_dir().unwrap()), "a file named relative: where it is");
        let mut g = old.clone();
        g.project.folder = Some("/shows/b".into());
        let back: Graph = serde_json::from_str(&g.to_json()).unwrap();
        assert_eq!(back.project.folder, Some("/shows/b".into()));
        assert_eq!(back.project.folder_for(Some(Path::new("/elsewhere/comp.json"))), Some("/shows/b".into()), "its own first");
    }

    #[test]
    fn view_state_saves_but_is_never_a_difference() {
        let mut g = Graph::default();
        assert!(!g.to_json().contains("\"view\""), "nothing saved when empty");
        let before = g.clone();
        g.view.0.insert("viewport3d".into(), serde_json::json!({ "yaw": 30 }));
        assert_eq!(g, before, "not a change");
        let back: Graph = serde_json::from_str(&g.to_json()).unwrap();
        assert_eq!(back.view.0["viewport3d"]["yaw"], 30);
    }
    use super::*;

    #[test]
    fn backdrops_fit_their_nodes_and_round_trip() {
        // Two nodes, 100×30 each, at (0, 0) and (200, 100).
        let b = Backdrop::around("backdrop1", "Keying", [([0.0, 0.0], [100.0, 30.0]), ([200.0, 100.0], [300.0, 130.0])]).unwrap();
        assert_eq!(b.pos, [-20.0, -50.0]);
        assert_eq!(b.size, [340.0, 200.0]);
        assert!(b.contains([0.0, 0.0], [100.0, 30.0]) && b.contains([200.0, 100.0], [300.0, 130.0]));
        assert!(!b.contains([250.0, 100.0], [350.0, 130.0]), "only partly inside");
        assert!(Backdrop::around("b", "", []).is_none());

        let mut g = Graph::from_str(r#"{"nodes": [{"id": "a", "op": "blur"}]}"#).unwrap();
        assert!(g.backdrops.is_empty(), "old files load without backdrops");
        assert!(!g.to_json().contains("backdrops"), "and save without them");
        assert_eq!(g.new_backdrop_id(), "backdrop1");
        g.backdrops.push(b);
        assert_eq!(g.new_backdrop_id(), "backdrop2");
        let back = Graph::from_str(&g.to_json()).unwrap();
        assert_eq!(back, g);
        // Title, color and font size are optional in the file.
        let g = Graph::from_str(r#"{"nodes": [], "backdrops": [{"id": "x", "pos": [0, 0], "size": [10, 10]}]}"#).unwrap();
        assert_eq!((g.backdrops[0].color, g.backdrops[0].font_size), (Backdrop::COLORS[0], Backdrop::FONT_SIZE));
    }

    #[test]
    fn particle_nodes_flow_through_the_graph_with_keyed_params() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let mut g = Graph::default();
        g.project.format = Size { width: 64, height: 48 };
        g.nodes = vec![
            node("cam", "camera", &[], json!({})),
            // One at frame 1, then (keyed on at frame 4) one a frame.
            node("emit", "particle_emitter", &["", ""], json!({ "burst": 1, "rate": { "keys": [[1, 0], [4, 1]], "interp": "step" }, "translate": [0, 0, -5], "size": 0.5, "speed": 0, "color": [1, 0, 0, 1] })),
            node("look", "particle_render", &["emit", "", ""], json!({ "look": "sprites" })),
            node("fall", "particle_gravity", &["look"], json!({ "acceleration": [0, -50, 0] })),
            node("render", "scanline_render", &["fall", "cam", ""], json!({ "samples": 1 })),
        ];
        let mut ev = Evaluator::new();
        let at = |ev: &mut Evaluator, g: &Graph, id: &str, frame: i64| ev.evaluate(g, id, &EvalContext { frame, write_outputs: false }).unwrap();
        let stream = |img: &Image| img.scene.as_ref().unwrap().particles[0].clone();
        // Frame 1: the burst, in the middle of the render.
        let img = at(&mut ev, &g, "render", 1);
        assert_eq!(img.at(32, 24), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(img.at(2, 2)[3], 0.0);
        assert!(img.plane("depth").is_some_and(|d| (img.plane_value(d, 32, 24) - 5.0).abs() < 1e-4));
        // Frame 6: three more since frame 4, and the first has fallen.
        let fall = at(&mut ev, &g, "fall", 6);
        let ps = stream(&fall).particles();
        assert_eq!(ps.len(), 4);
        let t = 5.0 / 24.0;
        assert!((ps[0].position.y + 25.0 * t * t).abs() < 1e-9);
        assert!(matches!(stream(&fall).look, particle_nodes::Look::Sprites(_)), "the look carries down the chain");
        // The emitter's own stream simulates nothing until asked.
        assert_eq!(stream(&at(&mut ev, &g, "emit", 6)).steps(), 0);
        // A key edited at frame 4 (not the frame shown) changes frame 6.
        g.nodes[1].params["rate"] = json!({ "keys": [[1, 0], [4, 3]], "interp": "step" });
        assert_eq!(stream(&at(&mut ev, &g, "fall", 6)).particles().len(), 10);
        // Into a Scanline Render's extra input instead: rendered with its scene.
        g.nodes[4].inputs = vec!["".into(), "cam".into(), "".into(), "look".into(), "".into()];
        assert_eq!(at(&mut ev, &g, "render", 1).at(32, 24), [1.0, 0.0, 0.0, 1.0]);
        g.nodes.push(node("plate", "constant", &[], json!({})));
        g.nodes[4].inputs[4] = "plate".into();
        let err = ev.evaluate(&g, "render", &EvalContext { frame: 1, write_outputs: false }).err().unwrap();
        assert!(err.to_string().contains("input 'scene' takes a scene, but 'plate' outputs an image"), "{err}");
        // A particle node with nothing on its particles input: an error naming it.
        g.nodes[3].inputs = vec![String::new()];
        let err = ev.evaluate(&g, "fall", &EvalContext { frame: 6, write_outputs: false }).err().unwrap();
        assert!(err.to_string().contains("fall") && err.to_string().contains("particles"), "{err}");
    }

    /// Sprites textured by a sequence: each shows the texture input at the
    /// frame its age picks (here played from frame 3 on, holding the
    /// last), whatever frame is rendered; each frame of it rendered once;
    /// a change at one of its frames (not the one shown) re-renders.
    #[test]
    fn sprites_play_a_texture_sequence_by_age() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let mut g = Graph::default();
        g.project.format = Size { width: 64, height: 48 };
        // Red is the frame number.
        let red = |keys: Value| json!({ "width": 2, "height": 2, "color": { "keys": keys, "interp": "linear" } });
        g.nodes = vec![
            node("tex", "constant", &[], red(json!([[1, [1, 0, 0, 1]], [10, [10, 0, 0, 1]]]))),
            // One born at frame 1, 10 px across at (20, 30).
            node("emit", "particle_emitter", &["", ""], json!({ "burst": 1, "rate": 0, "translate": [20, 30, 0], "size": 10, "speed": 0, "life": 1000 })),
            node("look", "particle_render", &["emit", "tex", ""], json!({ "look": "sprites", "outline": "square", "texture_frames": "sequence", "sequence_first": 3, "sequence_last": 6, "pick": "play", "cycle_rate": 1 })),
            node("draw", "particles_2d", &["look", ""], json!({})),
        ];
        let mut ev = Evaluator::new();
        let shown = |ev: &mut Evaluator, g: &Graph, frame: i64| ev.evaluate(g, "draw", &EvalContext { frame, write_outputs: false }).unwrap().at(20, 30)[0];
        let played: Vec<f32> = (1..=7).map(|f| shown(&mut ev, &g, f)).collect();
        assert_eq!(played, [3.0, 4.0, 5.0, 6.0, 6.0, 6.0, 6.0]);
        // The four frames rendered once and kept, not again for each frame.
        let kept = |ev: &Evaluator| ev.elsewhen["look"].images.values().cloned().collect::<Vec<_>>();
        let before = kept(&ev);
        assert_eq!(before.len(), 4);
        shown(&mut ev, &g, 2);
        assert!(kept(&ev).iter().all(|a| before.iter().any(|b| Arc::ptr_eq(a, b))));
        // Frame 6 of the texture changed (not 1–5): frame 4 shows it.
        let ctx4 = EvalContext { frame: 4, write_outputs: false };
        let key = Evaluator::key(&g, "draw", &ctx4).unwrap();
        g.nodes[0].params = red(json!([[1, [1, 0, 0, 1]], [5, [5, 0, 0, 1]], [6, [60, 0, 0, 1]], [10, [10, 0, 0, 1]]]));
        assert_ne!(Evaluator::key(&g, "draw", &ctx4).unwrap(), key, "its key changes with it");
        assert_eq!(shown(&mut ev, &g, 4), 60.0);
        assert_eq!(shown(&mut ev, &g, 2), 4.0);
        // Looping, and the frame rendered (the default) as before.
        g.nodes[2].params["pick"] = json!("loop");
        assert_eq!(shown(&mut ev, &g, 5), 3.0);
        g.nodes[2].params["texture_frames"] = json!("current frame");
        assert_eq!((shown(&mut ev, &g, 2), shown(&mut ev, &g, 5)), (2.0, 5.0));
        // A sequence ending before it starts: an error naming the node.
        g.nodes[2].params["texture_frames"] = json!("sequence");
        g.nodes[2].params["sequence_last"] = json!(2);
        let err = ev.evaluate(&g, "draw", &ctx4).err().unwrap().to_string();
        assert!(err.contains("look") && err.contains("before it starts"), "{err}");
    }

    /// An emitter on animated geometry: each frame's births come from the
    /// geometry at that frame, and going on a frame steps on from the one
    /// before (as for a plain emitter), giving exactly what simulating
    /// from scratch gives. An edit to the geometry re-simulates from the
    /// first frame it changes.
    #[test]
    fn emitters_on_animated_geometry_step_on_frame_by_frame() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let mut g = Graph::default();
        // A card 1 across sliding from x −4 (frame 1) to 4 (frame 17), 0.5 a frame.
        let slide = |keys: Value| json!({ "translate": { "keys": keys, "interp": "linear" }, "size": [1, 1] });
        g.nodes = vec![
            node("plate", "constant", &[], json!({ "width": 4, "height": 4 })),
            node("geo", "card", &["plate"], slide(json!([[1, [-4, 0, -5]], [17, [4, 0, -5]]]))),
            node("emit", "particle_emitter", &["geo", ""], json!({ "shape": "geometry surface", "rate": 5, "speed": 0, "life": 1000, "seed": 3 })),
            node("fall", "particle_gravity", &["emit"], json!({ "acceleration": [0, -1, 0] })),
        ];
        let ctx = |frame: i64| EvalContext { frame, write_outputs: false };
        let stream = |ev: &mut Evaluator, g: &Graph, frame: i64| ev.evaluate(g, "fall", &ctx(frame)).unwrap().scene.as_ref().unwrap().particles[0].clone();
        let mut ev = Evaluator::new();
        // (Simulated as shown: each frame's particles asked for.)
        for f in 1..=20 {
            stream(&mut ev, &g, f).particles();
        }
        let stepped = stream(&mut ev, &g, 20);
        let fresh = stream(&mut Evaluator::new(), &g, 20);
        let all = |s: &particle_nodes::ParticleStream| format!("{:?}", s.particles());
        assert_eq!(all(&stepped), all(&fresh), "the same as from scratch");
        assert_eq!(stepped.steps(), fresh.steps(), "each frame simulated once, not again every frame");
        // Born along the card's way, not all where it is now.
        let xs: Vec<f64> = stepped.particles().iter().map(|p| p.position.x).collect();
        assert_eq!(xs.len(), 100);
        assert!(xs.iter().any(|&x| x < -3.0) && xs.iter().any(|&x| x > 3.0), "{xs:?}");
        // The card moved on after frame 17 (it held there): frames 18–20
        // again, and only frames that use them change their key.
        let (k10, k20) = (Evaluator::key(&g, "fall", &ctx(10)).unwrap(), Evaluator::key(&g, "fall", &ctx(20)).unwrap());
        g.nodes[1].params = slide(json!([[1, [-4, 0, -5]], [17, [4, 0, -5]], [20, [4, 3, -5]]]));
        assert_eq!(Evaluator::key(&g, "fall", &ctx(10)).unwrap(), k10);
        assert_ne!(Evaluator::key(&g, "fall", &ctx(20)).unwrap(), k20);
        let before = stepped.steps();
        let edited = stream(&mut ev, &g, 20);
        let now = all(&edited);
        assert_eq!(edited.steps() - before, 3, "frames 18 to 20 again");
        assert_eq!(now, all(&stream(&mut Evaluator::new(), &g, 20)));
        assert_ne!(now, all(&fresh));
    }

    /// An evaluator borrowing another's particle simulations takes the
    /// frames that still stand for its graph and simulates only the rest,
    /// in caches of its own: the other's are never changed.
    #[test]
    fn a_borrowed_simulation_is_gone_on_from_never_changed() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let slide = |keys: Value| json!({ "translate": { "keys": keys, "interp": "linear" }, "size": [1, 1] });
        let graph = |slide_keys: Value, fall: f64| {
            let nodes = vec![
                node("plate", "constant", &[], json!({ "width": 4, "height": 4 })),
                node("geo", "card", &["plate"], slide(slide_keys)),
                node("emit", "particle_emitter", &["geo", ""], json!({ "shape": "geometry surface", "rate": 5, "speed": 0, "life": 1000, "seed": 3 })),
                node("fall", "particle_gravity", &["emit"], json!({ "acceleration": [0, -fall, 0] })),
            ];
            Graph { nodes, ..Graph::default() }
        };
        let g = graph(json!([[1, [-4, 0, -5]], [17, [4, 0, -5]]]), 1.0);
        let ctx = |frame: i64| EvalContext { frame, write_outputs: false };
        let stream = |ev: &mut Evaluator, g: &Graph, frame: i64| ev.evaluate(g, "fall", &ctx(frame)).unwrap().scene.as_ref().unwrap().particles[0].clone();
        let all = |s: &particle_nodes::ParticleStream| format!("{:?}", s.particles());
        // The viewer's simulation, shown up to frame 20.
        let mut viewer = Evaluator::new();
        for f in 1..=20 {
            stream(&mut viewer, &g, f).particles();
        }
        let shown = all(&stream(&mut viewer, &g, 20));
        let steps = stream(&mut viewer, &g, 20).steps();
        let untouched = |viewer: &mut Evaluator| {
            let s = stream(viewer, &g, 20);
            assert_eq!((all(&s), s.steps()), (shown.clone(), steps), "the viewer's simulation as it was, nothing to simulate again");
        };
        // The same graph: nothing simulated again, then on from there.
        let mut same = viewer.borrowing_particles();
        let s = stream(&mut same, &g, 20);
        assert_eq!((all(&s), s.steps()), (shown.clone(), 0));
        let s = stream(&mut same, &g, 22);
        let on = all(&s);
        assert_eq!((on, s.steps()), (all(&stream(&mut Evaluator::new(), &g, 22)), 2));
        untouched(&mut viewer);
        // The card moving on after frame 17 here: frames 18 to 20 only.
        let later = graph(json!([[1, [-4, 0, -5]], [17, [4, 0, -5]], [20, [4, 3, -5]]]), 1.0);
        let s = stream(&mut viewer.borrowing_particles(), &later, 20);
        let got = all(&s);
        assert_eq!((got, s.steps()), (all(&stream(&mut Evaluator::new(), &later, 20)), 3));
        untouched(&mut viewer);
        // Stronger gravity, so no frame stands: all of it, as from scratch.
        let other = graph(json!([[1, [-4, 0, -5]], [17, [4, 0, -5]]]), 5.0);
        let s = stream(&mut viewer.borrowing_particles(), &other, 20);
        let fresh = stream(&mut Evaluator::new(), &other, 20);
        let got = all(&s);
        assert_eq!((got, s.steps()), (all(&fresh), fresh.steps()));
        assert_ne!(all(&s), shown);
        untouched(&mut viewer);
    }

    /// A Scanline Render whose params leave out what it doesn't change
    /// (samples, width…) still blurs: the frames round its shutter count
    /// its params' defaults.
    #[test]
    fn a_sparse_scanline_render_still_blurs() {
        let g: Graph = serde_json::from_value(serde_json::json!({ "project": { "first_frame": 1, "last_frame": 5, "fps": 24, "format": { "width": 64, "height": 40 } }, "nodes": [
            { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 40, "size": 4 } },
            { "id": "card", "op": "card", "inputs": ["plate"], "params": { "size": [4, 2.5], "translate": { "keys": [[1, [-1, 0, -5]], [5, [3, 0, -5]]], "interp": "linear" } } },
            { "id": "cam", "op": "camera" },
            { "id": "set", "op": "scene", "inputs": ["card", "cam"] },
            { "id": "sharp", "op": "scanline_render", "inputs": ["set", "", ""] },
            { "id": "blurred", "op": "scanline_render", "inputs": ["set", "", ""], "params": { "motion_blur": 4, "shutter": 1.0 } },
        ] })).unwrap();
        let ctx = EvalContext { frame: 3, write_outputs: false };
        let mut ev = Evaluator::new();
        let (a, b) = (ev.evaluate(&g, "sharp", &ctx).unwrap(), ev.evaluate(&g, "blurred", &ctx).unwrap());
        let diff = a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
        assert!(diff > 0.05, "{diff}");
    }

    /// A node turned off passes on its main input (the same image) or, with
    /// none to pass, nothing: per op family, as ops::bypass_port says.
    #[test]
    fn a_disabled_node_passes_its_input_on_or_outputs_nothing() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let splats = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let mut g = Graph::default();
        g.project.format = Size { width: 16, height: 8 };
        g.nodes = vec![
            node("red", "constant", &[], json!({"width": 4, "height": 4, "color": [1, 0, 0, 1]})),
            node("blue", "constant", &[], json!({"width": 4, "height": 4, "color": [0, 0, 1, 1]})),
            node("grade", "grade", &["red"], json!({"multiply": [2, 2, 2]})),
            node("merge", "merge", &["red", "blue"], json!({})),
            node("cam", "camera", &[], json!({})),
            node("card", "card", &["red"], json!({})),
            node("scene", "scene", &["card", "cam"], json!({})),
            node("render", "scanline_render", &["scene", "cam", "blue"], json!({})),
            node("emit", "particle_emitter", &[], json!({"burst": 5, "rate": 0})),
            node("gravity", "particle_gravity", &["emit"], json!({})),
            node("cloud", "read_splat", &[], json!({"file": splats})),
            node("crop", "splat_crop", &["cloud"], json!({})),
            node("light", "light_point", &["card"], json!({})),
            // A broken link: an error on, no matter off.
            node("broken", "grade", &["red"], json!({"multiply": {"link": {"node": "gone", "param": "multiply"}}})),
        ];
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let mut on = Evaluator::new();
        assert!(on.evaluate(&g, "broken", &ctx).is_err());
        let before: HashMap<String, u64> = g.nodes.iter().filter(|n| n.id != "broken").map(|n| (n.id.clone(), Evaluator::key(&g, &n.id, &ctx).unwrap())).collect();
        for n in &mut g.nodes {
            n.disabled = !matches!(n.id.as_str(), "red" | "blue" | "cloud" | "emit" | "card");
        }
        let mut off = Evaluator::new();
        let out = off.evaluate_all(&g, &ctx).unwrap();
        let same = |a: &str, b: &str| Arc::ptr_eq(&out[a], &out[b]);
        assert!(same("grade", "red"), "an image op: its input");
        assert!(same("merge", "blue"), "a merge: its B");
        assert!(same("render", "blue"), "a Scanline Render: its background");
        assert!(same("gravity", "emit"), "a particle modifier: its stream");
        assert!(same("crop", "cloud"), "a splat op: its scene");
        assert!(same("broken", "red"), "off, its broken link doesn't matter");
        assert!(same("scene", "card"), "a Scene: its first input");
        // Nothing to pass: a transparent frame of the project's format, or
        // an empty scene.
        for id in ["cam", "light"] {
            let s = out[id].scene.as_ref().unwrap_or_else(|| panic!("{id}: a scene"));
            assert!(s.cameras.is_empty() && s.cards.is_empty() && s.lights.is_empty(), "{id}: empty");
        }
        let mut g2 = g.clone();
        g2.nodes.iter_mut().find(|n| n.id == "red").unwrap().disabled = true;
        let red = Evaluator::new().evaluate(&g2, "red", &ctx).unwrap();
        assert_eq!((red.format, red.pixels.len(), red.scene.is_none()), (Size { width: 16, height: 8 }, 0, true));
        // Keys change with the switch (so caches and the 3D view follow), and
        // match what a render keys on.
        for (id, k) in &before {
            if g.node(id).unwrap().disabled {
                assert_ne!(Evaluator::key(&g, id, &ctx).unwrap(), *k, "{id}");
            }
        }
        assert_eq!(off.cache["merge"].0, Evaluator::key(&g, "merge", &ctx).unwrap());
        // Saved: only when off; old files load on.
        let text = g.to_json();
        assert!(text.contains("\"disabled\": true"));
        assert_eq!(Graph::from_str(&text).unwrap().nodes, g.nodes);
        assert!(!Graph::from_str(r#"{"nodes": [{"id": "a", "op": "blur"}]}"#).unwrap().to_json().contains("disabled"));
    }

    /// A Write turned off writes nothing, even when outputs are written.
    #[test]
    fn a_disabled_write_writes_nothing() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("arclight_disabled_write_{}", std::process::id()));
        let file = dir.join("out.png");
        let g = |disabled| Graph {
            nodes: vec![
                NodeSpec { id: "c".into(), op: "constant".into(), inputs: vec![], params: json!({"width": 2, "height": 2}), pos: None, disabled: false },
                NodeSpec { id: "w".into(), op: "write".into(), inputs: vec!["c".into()], params: json!({"path": file.to_string_lossy()}), pos: None, disabled },
            ],
            ..Graph::default()
        };
        let ctx = EvalContext { frame: 1, write_outputs: true };
        Evaluator::new().evaluate(&g(true), "w", &ctx).unwrap();
        assert!(!file.exists());
        Evaluator::new().evaluate(&g(false), "w", &ctx).unwrap();
        assert!(file.exists(), "on, it writes");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A chain through GPU ops, a CPU-only op and back renders as on the
    /// CPU alone; the GPU's results stay there between GPU ops, go back
    /// to the CPU past the budget, and are keyed apart from the CPU's.
    #[test]
    fn gpu_chains_render_as_on_the_cpu() {
        use serde_json::json;
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None, disabled: false };
        let mut nodes = vec![
            node("checker", "checkerboard", &[], json!({ "width": 301, "height": 203, "size": 7 })),
            node("soft", "blur", &["checker"], json!({ "radius": 6, "passes": 2 })),
            node("look", "grade", &["soft"], json!({ "gamma": [1.4, 1.0, 0.8] })),
            node("turn", "transform", &["look"], json!({ "rotate": 10, "scale": [0.8, 0.8] })),
            node("glow", "blur", &["turn"], json!({ "radius": 3 })),
            node("comp", "merge", &["glow", "look"], json!({ "operation": "screen" })),
            node("dim", "exposure", &["comp", "checker"], json!({ "stops": -0.5, "mix": 0.6, "mask_channel": "luminance" })),
            node("off", "saturation", &["dim"], json!({ "saturation": 0 })),
            node("alone", "rectangle", &[], json!({ "width": 64, "height": 32 })),
        ];
        nodes[7].disabled = true;
        let g = Graph { nodes, ..Graph::default() };
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let cpu = crate::gpu::cpu_only(|| Evaluator::new().evaluate(&g, "off", &ctx).unwrap());
        let mut ev = Evaluator::new();
        let got = ev.evaluate(&g, "off", &ctx).unwrap();
        let row = crate::gpu::check::compare("chain", "", &cpu, &got, 1e-4, 1e-4, 0.0, 0.0, 0.0);
        assert!(row.pass, "{row:?}");
        if crate::gpu::get().is_none() {
            return eprintln!("skipped the GPU half: no GPU adapter");
        }
        let on_gpu = |ev: &Evaluator, id: &str| matches!(ev.cache[id].1, Res::Gpu(_));
        assert!(on_gpu(&ev, "soft") && on_gpu(&ev, "look") && on_gpu(&ev, "glow") && on_gpu(&ev, "comp") && on_gpu(&ev, "dim"));
        // The checkerboard ran on the CPU; its result was uploaded for the
        // blur and kept there.
        assert!(on_gpu(&ev, "turn") && on_gpu(&ev, "checker"));
        // Keys: GPU ops (and what's below them) apart, others the same.
        let key = |id: &str| Evaluator::key(&g, id, &ctx).unwrap();
        let cpu_key = |id: &str| crate::gpu::cpu_only(|| Evaluator::key(&g, id, &ctx).unwrap());
        assert!(key("soft") != cpu_key("soft") && key("turn") != cpu_key("turn"));
        assert_eq!(key("alone"), cpu_key("alone"));
        assert_eq!(ev.cache["off"].0, key("off"), "run and key agree");
        // Past the budget: back on the CPU, the same pixels, no re-render.
        ev.evict_gpu_past(0);
        assert!(ev.cache.values().all(|(_, r)| matches!(r, Res::Cpu(_))));
        let again = ev.evaluate(&g, "off", &ctx).unwrap();
        assert!(Arc::ptr_eq(&again, &ev.cache["off"].1.clone().cpu_now()) && again.pixels == got.pixels);
    }
}
