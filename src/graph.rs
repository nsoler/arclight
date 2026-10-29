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
}

impl Project {
    /// Its timing, for formulas' `time` and `fps`.
    pub fn clock(&self) -> crate::expr::Clock {
        crate::expr::Clock { fps: self.fps, first_frame: self.first_frame as f64 }
    }
}

fn default_format() -> Size {
    ops::DEFAULT_FORMAT
}

impl Default for Project {
    fn default() -> Self {
        Project { first_frame: 1, last_frame: 100, fps: 24.0, format: ops::DEFAULT_FORMAT }
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
    pub const FONT_SIZE: f32 = 14.0;
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
    pub fn from_file(path: &Path) -> Result<Graph> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading graph {}", path.display()))?;
        Self::from_str(&text)
    }

    pub fn from_str(text: &str) -> Result<Graph> {
        serde_json::from_str(text).context("parsing graph JSON")
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

/// Evaluates graphs, keeping each node's latest result for reuse.
#[derive(Default)]
pub struct Evaluator {
    cache: HashMap<String, (u64, Arc<Image>)>,
    /// Results a frame later, for nodes that need them (a Scanline
    /// Render's motion pass: see [`ops::needs_next_frame`]).
    next: Option<Box<Evaluator>>,
    /// Particle nodes' simulation caches and shapes.
    particles: particle_nodes::ParticleState,
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render `target` (and whatever it depends on) at `ctx.frame`.
    pub fn evaluate(
        &mut self,
        graph: &Graph,
        target: &str,
        ctx: &EvalContext,
    ) -> Result<Arc<Image>, NodeError> {
        let index = graph.index()?;
        self.cache.retain(|id, _| index.contains_key(id.as_str()));
        self.particles.retain(|id| index.contains_key(id));
        let order = Graph::order(&index, [target.to_string()])?;
        self.run(&index, &order, ctx, &graph.project)?;
        Ok(self.cache[target].1.clone())
    }

    /// Render every node.
    pub fn evaluate_all(
        &mut self,
        graph: &Graph,
        ctx: &EvalContext,
    ) -> Result<HashMap<String, Arc<Image>>, NodeError> {
        let index = graph.index()?;
        self.cache.retain(|id, _| index.contains_key(id.as_str()));
        self.particles.retain(|id| index.contains_key(id));
        let order = Graph::order(&index, graph.nodes.iter().map(|n| n.id.clone()))?;
        self.run(&index, &order, ctx, &graph.project)?;
        Ok(order
            .into_iter()
            .map(|id| {
                let img = self.cache[&id].1.clone();
                (id, img)
            })
            .collect())
    }

    /// The cache key `target` would have at `ctx.frame`, without rendering.
    /// Equal keys mean identical images, so callers can cache outputs by it.
    pub fn key(graph: &Graph, target: &str, ctx: &EvalContext) -> Result<u64, NodeError> {
        let index = graph.index()?;
        let order = Graph::order(&index, [target.to_string()])?;
        let mut keys = HashMap::new();
        for id in &order {
            let node = index[id.as_str()];
            let mut params = linked_params(&index, node, ctx.frame, &graph.project)?;
            ops::apply_format(&node.op, &mut params, graph.project.format);
            let mut k = node_key(node, &params, &keys, ctx);
            if ops::needs_next_frame(&node.op, &params) {
                let later = EvalContext { frame: ctx.frame + 1, write_outputs: false };
                let next: Vec<u64> = next_inputs(node).iter().flatten().map(|d| Evaluator::key(graph, d, &later)).collect::<Result<_, _>>()?;
                k = mix(k, &next);
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
            // Keyframes and links are resolved first and the resolved values
            // hashed, so a curve that holds between frames still hits the
            // cache, and a changed link source re-renders.
            let mut params = linked_params(index, node, ctx.frame, project)?;
            ops::apply_format(&node.op, &mut params, graph_format);
            let mut key = node_key(node, &params, &keys, ctx);
            // A node that needs its inputs a frame later: render those (and
            // what they need) at the next frame too, and key on them.
            let next = if ops::needs_next_frame(&node.op, &params) {
                let deps = next_inputs(node);
                let roots: Vec<String> = deps.iter().flatten().cloned().collect();
                let later = self.next.get_or_insert_with(Default::default);
                let ctx_next = EvalContext { frame: ctx.frame + 1, write_outputs: false };
                later.run(index, &Graph::order(index, roots)?, &ctx_next, project)?;
                let found = deps.map(|d| d.map(|d| later.cache[&d].clone()));
                key = mix(key, &found.iter().flatten().map(|f| f.0).collect::<Vec<_>>());
                Some(found.map(|f| f.map(|f| f.1)))
            } else {
                None
            };
            keys.insert(id.as_str(), key);
            if self.cache.get(id).is_some_and(|(k, _)| *k == key) {
                continue;
            }

            let particle = particle_nodes::info(&node.op);
            let info = ops::info(&node.op);
            let ports = info.map(|i| i.inputs).unwrap_or(&[]);
            // Outputs (viewer, writes) refuse to show or write nothing;
            // other ops take an unconnected input as empty, so one node left
            // unplugged doesn't blank everything below it.
            let strict = info.is_some_and(|i| i.category == Category::Output);
            let n_ports = ports.len().max(node.inputs.len());
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
                    let img = self.cache[dep].1.as_ref();
                    // A scene only into a scene port, an image only into an
                    // image port (a hand-edited or old file may mix them).
                    if let Some(port) = ops::port(&node.op, i).filter(|p| !p.kind.takes(img)) {
                        let got = ops::article(ops::Kind::of(img));
                        return Err(node_err(id, format!("input '{}' takes {}, but '{dep}' outputs {got}", port.label, ops::article(port.kind))));
                    }
                    inputs.push(Some(img));
                }
            }
            let out = match &next {
                _ if particle.is_some() => {
                    let snap = snapshot.get_or_insert_with(|| Arc::new(index.values().map(|n| (*n).clone()).collect())).clone();
                    let input_keys: Vec<u64> = node.inputs.iter().map(|d| keys.get(d.as_str()).copied().unwrap_or(0)).collect();
                    particle_node(node, snap, project, &inputs, &input_keys, ctx.frame, &mut self.particles)
                }
                Some(later) => ops::scanline_render_next(&params, &inputs, [later[0].as_deref(), later[1].as_deref()]),
                None => ops::eval(&node.op, &params, &inputs, ctx),
            };
            let mut out = out.map_err(|e| node_err(id, format!("{e:#}")))?;
            // What a node made in 3D carries its name (for the viewport).
            if let Some(scene) = out.scene.as_mut() {
                Arc::make_mut(scene).tag(id);
            }
            self.cache.insert(id.clone(), (key, Arc::new(out)));
        }
        Ok(())
    }
}

/// Evaluate particle node `node` (see [`particle_nodes`]): its params at
/// any frame read from `graph` (a snapshot), its particles input's stream
/// as its upstream, a stream out as a scene.
fn particle_node(
    node: &NodeSpec,
    graph: Arc<Vec<NodeSpec>>,
    project: &Project,
    inputs: &[Option<&Image>],
    keys: &[u64],
    frame: i64,
    state: &mut particle_nodes::ParticleState,
) -> Result<Image> {
    let (id, at) = (node.id.clone(), project.clone());
    let own: particle_nodes::Own = Arc::new(move |g| {
        let index: HashMap<&str, &NodeSpec> = graph.iter().map(|n| (n.id.as_str(), n)).collect();
        index.get(id.as_str()).and_then(|n| linked_params(&index, n, g, &at).ok()).unwrap_or(Value::Null)
    });
    let upstream = inputs.first().copied().flatten().and_then(|i| i.scene.as_ref()).and_then(|s| s.particles.first());
    let cx = particle_nodes::Context { node: &node.id, frame, fps: project.fps, format: project.format, own, inputs, keys, upstream };
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
/// The inputs a node needing the next frame wants then (a Scanline
/// Render's scene and camera; None where unconnected).
fn next_inputs(node: &NodeSpec) -> [Option<String>; 2] {
    std::array::from_fn(|i| node.inputs.get(i).filter(|d| !d.is_empty()).cloned())
}

/// `key` combined with `more` keys.
fn mix(key: u64, more: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    more.hash(&mut h);
    h.finish()
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
        let node = |id: &str, op: &str, inputs: &[&str], params: Value| NodeSpec { id: id.into(), op: op.into(), inputs: inputs.iter().map(|s| s.to_string()).collect(), params, pos: None };
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
}
