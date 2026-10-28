//! The node graph: a JSON-described DAG, topologically sorted and evaluated.
//!
//! [`Evaluator`] caches each node's last result under a key hashed from the
//! node's op, params, inputs' keys and (for time-varying ops) the frame. A
//! change to one node therefore only re-renders that node and its downstream.

use crate::image::Image;
use crate::ops::{self, EvalContext};
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
}

impl Default for Project {
    fn default() -> Self {
        Project { first_frame: 1, last_frame: 100, fps: 24.0 }
    }
}

/// A whole graph document.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub project: Project,
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
        let order = Graph::order(&index, [target.to_string()])?;
        self.run(&index, &order, ctx)?;
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
        let order = Graph::order(&index, graph.nodes.iter().map(|n| n.id.clone()))?;
        self.run(&index, &order, ctx)?;
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
            let params = ops::resolve_params(&node.op, &node.params, ctx.frame);
            let k = node_key(node, &params, &keys, ctx);
            keys.insert(id.as_str(), k);
        }
        Ok(keys[target])
    }

    fn run(
        &mut self,
        index: &HashMap<&str, &NodeSpec>,
        order: &[String],
        ctx: &EvalContext,
    ) -> Result<(), NodeError> {
        let mut keys: HashMap<&str, u64> = HashMap::with_capacity(order.len());
        for id in order {
            let node = index[id.as_str()];
            // Keyframes are resolved first and the resolved values hashed, so a
            // curve that holds between frames still hits the cache.
            let params = ops::resolve_params(&node.op, &node.params, ctx.frame);
            let key = node_key(node, &params, &keys, ctx);
            keys.insert(id.as_str(), key);
            if self.cache.get(id).is_some_and(|(k, _)| *k == key) {
                continue;
            }

            let ports = ops::info(&node.op).map(|i| i.inputs).unwrap_or(&[]);
            let mut inputs: Vec<&Image> = Vec::with_capacity(node.inputs.len());
            for (i, dep) in node.inputs.iter().enumerate() {
                if dep.is_empty() {
                    let port = ports.get(i).copied().unwrap_or("input");
                    return Err(node_err(id, format!("input '{port}' is not connected")));
                }
                inputs.push(self.cache[dep.as_str()].1.as_ref());
            }
            let out = ops::eval(&node.op, &params, &inputs, ctx)
                .map_err(|e| node_err(id, format!("{e:#}")))?;
            self.cache.insert(id.clone(), (key, Arc::new(out)));
        }
        Ok(())
    }
}

/// `params` are the node's params already resolved at `ctx.frame`.
fn node_key(node: &NodeSpec, params: &Value, keys: &HashMap<&str, u64>, ctx: &EvalContext) -> u64 {
    let mut h = DefaultHasher::new();
    node.op.hash(&mut h);
    params.to_string().hash(&mut h);
    for dep in &node.inputs {
        keys.get(dep.as_str()).copied().unwrap_or(0).hash(&mut h);
    }
    if ops::is_time_varying(&node.op, params) {
        ctx.frame.hash(&mut h);
    }
    if node.op == "write" {
        ctx.write_outputs.hash(&mut h);
    }
    h.finish()
}
