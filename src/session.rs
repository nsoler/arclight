//! A headless editing session: a graph plus the edits scripting and
//! automation make to it, with validation and undo/redo.
//!
//! The MCP server, the Python `arclt` module and the GUI's app bridge all
//! drive graphs through [`Session`], so a script behaves the same whichever
//! front end runs it.

use crate::anim;
use crate::graph::{Evaluator, Graph, NodeSpec};
use crate::image::{Image, Size};
use crate::ops::{self, EvalContext, OpInfo, ParamKind};
use crate::render::{self, Progress};
use crate::track;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const UNDO_LIMIT: usize = 200;

pub struct Session {
    pub graph: Graph,
    /// Where the graph was loaded from or last saved to.
    pub path: Option<PathBuf>,
    /// Edited since the last load or save.
    pub dirty: bool,
    eval: Evaluator,
    undo: Vec<Graph>,
    redo: Vec<Graph>,
}

impl Default for Session {
    fn default() -> Self {
        Session::new(Graph::default())
    }
}

impl Session {
    pub fn new(graph: Graph) -> Session {
        let mut graph = graph;
        normalize(&mut graph);
        Session { graph, path: None, dirty: false, eval: Evaluator::new(), undo: Vec::new(), redo: Vec::new() }
    }

    pub fn open(path: &Path) -> Result<Session> {
        let mut s = Session::new(Graph::from_file(path)?);
        s.path = Some(path.to_path_buf());
        Ok(s)
    }

    /// Replace the graph with one from `path`. Undoable.
    pub fn load(&mut self, path: &Path) -> Result<()> {
        let mut g = Graph::from_file(path)?;
        normalize(&mut g);
        self.checkpoint();
        self.graph = g;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    /// Start an empty graph. Undoable.
    pub fn clear(&mut self) {
        self.checkpoint();
        self.graph = Graph::default();
        self.path = None;
    }

    /// Write the graph to `path`, or to where it came from.
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => self.path.clone().ok_or_else(|| anyhow!("no file name: pass a path"))?,
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, self.graph.to_json()).with_context(|| format!("writing {}", path.display()))?;
        self.path = Some(path.clone());
        self.dirty = false;
        Ok(path)
    }

    // -- undo ---------------------------------------------------------------

    /// Record the current graph as an undo step before an edit.
    fn checkpoint(&mut self) {
        self.undo.push(self.graph.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.graph, prev));
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.graph, next));
        self.dirty = true;
        true
    }

    // -- queries ------------------------------------------------------------

    pub fn node(&self, id: &str) -> Option<&NodeSpec> {
        self.graph.node(id)
    }

    fn node_index(&self, id: &str) -> Result<usize> {
        self.graph.nodes.iter().position(|n| n.id == id).ok_or_else(|| anyhow!("no node named '{id}'"))
    }

    /// `base1`, `base2`, … : the first id not in use.
    pub fn unique_id(&self, base: &str) -> String {
        (1..).map(|i| format!("{base}{i}")).find(|id| self.node(id).is_none()).unwrap()
    }

    /// A node's op, ports (with what feeds them), params with defaults filled
    /// in, and the nodes it feeds.
    pub fn describe_node(&self, id: &str) -> Result<Value> {
        let n = &self.graph.nodes[self.node_index(id)?];
        let info = op_info(&n.op)?;
        let inputs: Vec<Value> = info
            .inputs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let src = n.inputs.get(i).filter(|s| !s.is_empty());
                json!({"port": i, "label": p.label, "optional": p.optional, "source": src})
            })
            .collect();
        let outputs: Vec<Value> = self
            .graph
            .nodes
            .iter()
            .flat_map(|d| {
                d.inputs.iter().enumerate().filter(|(_, s)| *s == id).map(|(port, _)| json!({"node": d.id, "port": port}))
            })
            .collect();
        Ok(json!({
            "id": n.id,
            "op": n.op,
            "label": info.label,
            "inputs": inputs,
            "outputs": outputs,
            "params": ops::params_with_defaults(&n.op, &n.params),
        }))
    }

    // -- edits --------------------------------------------------------------

    /// Add a node. `id` defaults to the op name plus a number; `params` are
    /// validated and merged over the op's defaults; `inputs` connect ports in
    /// order (empty strings leave a port unconnected).
    pub fn create_node(&mut self, op: &str, id: Option<&str>, params: &Map<String, Value>, inputs: &[String]) -> Result<String> {
        let info = op_info(op)?;
        let id = match id {
            Some(id) => {
                check_id(id)?;
                if self.node(id).is_some() {
                    bail!("'{id}' is already used");
                }
                id.to_string()
            }
            None => self.unique_id(op),
        };
        let mut values = ops::params_with_defaults(op, &Value::Null);
        ops::fit_new_shape(op, &mut values, self.graph.project.format);
        if op == "media_out" {
            values["first_frame"] = self.graph.project.first_frame.into();
            values["last_frame"] = self.graph.project.last_frame.into();
            values["fps"] = self.graph.project.fps.into();
        }
        for (name, v) in params {
            values[name.as_str()] = validate_param(info, name, v)?;
        }
        if inputs.len() > info.inputs.len() {
            bail!("'{op}' has {} input(s), got {}", info.inputs.len(), inputs.len());
        }
        for src in inputs.iter().filter(|s| !s.is_empty()) {
            if self.node(src).is_none() {
                bail!("no node named '{src}'");
            }
        }
        let mut node_inputs = inputs.to_vec();
        node_inputs.resize(info.inputs.len(), String::new());
        self.checkpoint();
        self.graph.nodes.push(NodeSpec { id: id.clone(), op: op.to_string(), inputs: node_inputs, params: values, pos: None });
        Ok(id)
    }

    /// Add a node of type `op` (`transform` or `corner_pin`) carrying
    /// Tracker `id`'s motion (the match-move, or with `stabilize` its
    /// inverse), linked to it live or (not `linked`) keyed with a copy, fed
    /// by `input` if given.
    pub fn export_tracker(&mut self, id: &str, op: &str, stabilize: bool, linked: bool, name: Option<&str>, input: Option<&str>) -> Result<String> {
        let params = track::export(&self.graph, id, op, stabilize, linked)?;
        self.create_node(op, name, &params, &[input.unwrap_or_default().to_string()])
    }

    /// Remove a node. Ports it fed are reconnected to its first input, so
    /// deleting from the middle of a chain keeps the chain.
    pub fn delete_node(&mut self, id: &str) -> Result<()> {
        let i = self.node_index(id)?;
        self.checkpoint();
        let removed = self.graph.nodes.remove(i);
        let heal = removed.inputs.first().cloned().unwrap_or_default();
        for n in &mut self.graph.nodes {
            for inp in &mut n.inputs {
                if inp == id {
                    *inp = heal.clone();
                }
            }
        }
        Ok(())
    }

    pub fn rename(&mut self, old: &str, new: &str) -> Result<()> {
        self.node_index(old)?;
        check_id(new)?;
        if new == old {
            return Ok(());
        }
        if self.node(new).is_some() {
            bail!("'{new}' is already used");
        }
        self.checkpoint();
        for n in &mut self.graph.nodes {
            if n.id == old {
                n.id = new.to_string();
            }
            for inp in &mut n.inputs {
                if inp == old {
                    *inp = new.to_string();
                }
            }
            crate::link::rename(&mut n.params, old, new);
        }
        Ok(())
    }

    /// Set several params at once (one undo step). Each is validated against
    /// the op's spec; keyframed values (`{"keys": ...}`) are accepted for
    /// animatable params.
    pub fn set_params(&mut self, id: &str, params: &Map<String, Value>) -> Result<()> {
        let i = self.node_index(id)?;
        let info = op_info(&self.graph.nodes[i].op)?;
        let checked = params
            .iter()
            .map(|(k, v)| Ok((k.clone(), validate_param(info, k, v)?)))
            .collect::<Result<Vec<_>>>()?;
        self.checkpoint();
        let node = &mut self.graph.nodes[i];
        if !node.params.is_object() {
            node.params = Value::Object(Map::new());
        }
        for (k, v) in checked {
            node.params[k.as_str()] = v;
        }
        Ok(())
    }

    /// Key param `name` at `frame` (animating it if it was static).
    pub fn set_key(&mut self, id: &str, name: &str, frame: i64, value: &Value) -> Result<()> {
        let i = self.node_index(id)?;
        let info = op_info(&self.graph.nodes[i].op)?;
        let spec = param_spec(info, name)?;
        if !spec.animatable {
            bail!("'{name}' can't be animated");
        }
        let value = validate_param(info, name, value)?;
        self.checkpoint();
        let node = &mut self.graph.nodes[i];
        let current = ops::params_with_defaults(&node.op, &node.params)[name].clone();
        node.params[name] = anim::set_key(&current, frame, &value);
        Ok(())
    }

    /// Connect `from`'s output to input `port` of `to`. Refuses cycles.
    pub fn connect(&mut self, from: &str, to: &str, port: usize) -> Result<()> {
        self.node_index(from)?;
        let i = self.node_index(to)?;
        let n_ports = self.graph.nodes[i].inputs.len();
        if port >= n_ports {
            bail!("'{to}' has no input {port} (it has {n_ports})");
        }
        if self.is_upstream(to, from) {
            bail!("connecting '{from}' to '{to}' would create a cycle");
        }
        self.checkpoint();
        self.graph.nodes[i].inputs[port] = from.to_string();
        Ok(())
    }

    pub fn disconnect(&mut self, to: &str, port: usize) -> Result<()> {
        let i = self.node_index(to)?;
        if port >= self.graph.nodes[i].inputs.len() {
            bail!("'{to}' has no input {port}");
        }
        self.checkpoint();
        self.graph.nodes[i].inputs[port].clear();
        Ok(())
    }

    /// Port index for a label like "B" or "mask" on node `id`, or a number.
    pub fn port_index(&self, id: &str, port: &Value) -> Result<usize> {
        if let Some(n) = port.as_u64() {
            return Ok(n as usize);
        }
        let label = port.as_str().ok_or_else(|| anyhow!("port must be a number or a label"))?;
        let n = &self.graph.nodes[self.node_index(id)?];
        let info = op_info(&n.op)?;
        info.inputs
            .iter()
            .position(|p| p.label.eq_ignore_ascii_case(label))
            .ok_or_else(|| anyhow!("'{id}' has no input '{label}'"))
    }

    pub fn set_project(&mut self, first: Option<i64>, last: Option<i64>, fps: Option<f64>, format: Option<Size>) -> Result<()> {
        let mut p = self.graph.project.clone();
        p.first_frame = first.unwrap_or(p.first_frame);
        p.last_frame = last.unwrap_or(p.last_frame);
        p.fps = fps.unwrap_or(p.fps);
        p.format = format.unwrap_or(p.format);
        if !(1..=16384).contains(&p.format.width) || !(1..=16384).contains(&p.format.height) {
            bail!("the format must be 1 to 16384 pixels each way");
        }
        if p.last_frame < p.first_frame {
            bail!("last frame {} is before first frame {}", p.last_frame, p.first_frame);
        }
        if !p.fps.is_finite() || p.fps <= 0.0 {
            bail!("fps must be positive");
        }
        self.checkpoint();
        self.graph.project = p;
        Ok(())
    }

    fn is_upstream(&self, ancestor: &str, node: &str) -> bool {
        let mut stack = vec![node.to_string()];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if id == ancestor {
                return true;
            }
            if seen.insert(id.clone()) {
                if let Some(n) = self.node(&id) {
                    stack.extend(n.inputs.iter().filter(|s| !s.is_empty()).cloned());
                }
            }
        }
        false
    }

    // -- rendering ----------------------------------------------------------

    /// Render node `id` at `frame`. Results are cached between calls.
    pub fn render(&mut self, id: &str, frame: i64) -> Result<Arc<Image>> {
        let ctx = EvalContext { frame, write_outputs: false };
        Ok(self.eval.evaluate(&self.graph, id, &ctx)?)
    }

    /// Render node `id` at `frame` and write it to `path` (EXR keeps float;
    /// other formats are 8-bit sRGB).
    pub fn render_to_file(&mut self, id: &str, frame: i64, path: &Path) -> Result<Arc<Image>> {
        let img = self.render(id, frame)?;
        img.write(path)?;
        Ok(img)
    }

    /// Render the frame range of `media_out` node `id`. Returns frames written.
    pub fn render_output(&self, id: &str, progress: &Progress) -> Result<i64> {
        render::render_media_out(&self.graph, id, progress)
    }
}

/// Pad every node's inputs to its op's port count.
fn normalize(graph: &mut Graph) {
    for n in &mut graph.nodes {
        if let Some(info) = ops::info(&n.op) {
            if n.inputs.len() < info.inputs.len() {
                n.inputs.resize(info.inputs.len(), String::new());
            }
        }
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        bail!("invalid node name '{id}': use letters, digits, '_', '-' or '.'");
    }
    Ok(())
}

pub fn op_info(op: &str) -> Result<&'static OpInfo> {
    ops::info(op).ok_or_else(|| anyhow!("unknown op '{op}'"))
}

fn param_spec<'a>(info: &'a OpInfo, name: &str) -> Result<&'a ops::ParamSpec> {
    info.params.iter().find(|s| s.name == name).ok_or_else(|| {
        let names: Vec<&str> = info.params.iter().map(|s| s.name).collect();
        anyhow!("'{}' has no param '{name}' (params: {})", info.name, names.join(", "))
    })
}

/// Check `value` against param `name` of `info`, returning it normalized
/// (ints rounded, a 3-component color given alpha 1).
pub fn validate_param(info: &OpInfo, name: &str, value: &Value) -> Result<Value> {
    let spec = param_spec(info, name)?;
    // A live link to another node's value (see `crate::link`).
    if crate::link::is_link(value) {
        if crate::link::source(value).is_none() {
            bail!("'{name}': a link needs a \"node\"");
        }
        return Ok(value.clone());
    }
    // Null puts back an automatic default (a Transform's center).
    if value.is_null() && spec.default_value().is_null() {
        return Ok(Value::Null);
    }
    if anim::is_animated(value) {
        if !spec.animatable {
            bail!("'{name}' can't be animated");
        }
        return Ok(value.clone());
    }
    let numbers = |n: usize| -> Result<Vec<f64>> {
        let arr = value.as_array().filter(|a| a.len() == n).ok_or_else(|| anyhow!("'{name}' needs {n} numbers"))?;
        arr.iter().map(|v| v.as_f64().ok_or_else(|| anyhow!("'{name}' needs {n} numbers"))).collect()
    };
    Ok(match spec.kind {
        ParamKind::Float { .. } => json!(value.as_f64().ok_or_else(|| anyhow!("'{name}' needs a number"))?),
        ParamKind::Int { min, max } => {
            let v = value.as_f64().ok_or_else(|| anyhow!("'{name}' needs a whole number"))?.round() as i64;
            if v < min || v > max {
                bail!("'{name}' must be between {min} and {max}");
            }
            json!(v)
        }
        ParamKind::Vec2 => json!(numbers(2)?),
        ParamKind::Vec3 => json!(numbers(3)?),
        ParamKind::Color => match value.as_array().map(Vec::len) {
            Some(3) => {
                let mut c = numbers(3)?;
                c.push(1.0);
                json!(c)
            }
            _ => json!(numbers(4)?),
        },
        ParamKind::Choice(choices) => {
            let s = value.as_str().ok_or_else(|| anyhow!("'{name}' needs one of: {}", choices.join(", ")))?;
            if !choices.contains(&s) {
                bail!("'{s}' is not a choice for '{name}' (one of: {})", choices.join(", "));
            }
            value.clone()
        }
        ParamKind::Path { .. } => json!(value.as_str().ok_or_else(|| anyhow!("'{name}' needs a file path"))?),
        ParamKind::Bool => json!(value.as_bool().ok_or_else(|| anyhow!("'{name}' needs true or false"))?),
        ParamKind::Points { stride } => {
            let arr = value.as_array().ok_or_else(|| anyhow!("'{name}' needs an array of numbers"))?;
            if arr.len() % stride != 0 || arr.iter().any(|v| !v.is_number()) {
                bail!("'{name}' needs numbers in groups of {stride}");
            }
            value.clone()
        }
    })
}

/// A JSON description of an op: ports, params (kind, range, default) and help.
pub fn op_schema(info: &OpInfo) -> Value {
    let params: Vec<Value> = info
        .params
        .iter()
        .map(|p| {
            let mut v = json!({"name": p.name, "label": p.label, "default": p.default_value(), "animatable": p.animatable});
            let (kind, extra) = match p.kind {
                ParamKind::Float { min, max } => ("float", json!({"min": min, "max": max})),
                ParamKind::Int { min, max } => ("int", json!({"min": min, "max": max})),
                ParamKind::Vec2 => ("vec2", json!({})),
                ParamKind::Vec3 => ("vec3", json!({})),
                ParamKind::Color => ("color", json!({"note": "straight linear RGBA"})),
                ParamKind::Choice(c) => ("choice", json!({"choices": c})),
                ParamKind::Path { save } => ("path", json!({"save": save})),
                ParamKind::Bool => ("bool", json!({})),
                ParamKind::Points { stride } => ("points", json!({"stride": stride})),
            };
            v["kind"] = json!(kind);
            if let Value::Object(m) = extra {
                v.as_object_mut().unwrap().extend(m);
            }
            v
        })
        .collect();
    let inputs: Vec<Value> = info.inputs.iter().map(|p| json!({"label": p.label, "optional": p.optional})).collect();
    let mut v = json!({
        "name": info.name,
        "label": info.label,
        "category": format!("{:?}", info.category).to_lowercase(),
        "inputs": inputs,
        "params": params,
        "help": info.help,
    });
    if let Some(lib) = crate::plugin::library(info.name) {
        v["plugin"] = json!(lib);
    }
    v
}

/// Size and average/min/max color of an image, for reporting a render
/// without shipping its pixels.
pub fn image_stats(img: &Image) -> Value {
    // The frame's size and stats; `window` is where data is stored.
    let orig = img;
    let img = img.flat();
    let n = img.pixels.len().max(1) as f64;
    let mut sum = [0f64; 4];
    let mut lo = [f32::INFINITY; 4];
    let mut hi = [f32::NEG_INFINITY; 4];
    for p in &img.pixels {
        for c in 0..4 {
            sum[c] += p[c] as f64;
            lo[c] = lo[c].min(p[c]);
            hi[c] = hi[c].max(p[c]);
        }
    }
    let round = |x: f64| (x * 1e5).round() / 1e5;
    json!({
        "width": img.width,
        "height": img.height,
        "window": [orig.x0, orig.y0, orig.x0 + orig.width as i32, orig.y0 + orig.height as i32],
        "mean_rgba": sum.map(|s| round(s / n)),
        "min_rgba": if img.pixels.is_empty() { [0.0; 4] } else { lo.map(|x| round(x as f64)) },
        "max_rgba": if img.pixels.is_empty() { [0.0; 4] } else { hi.map(|x| round(x as f64)) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn build_render_and_undo() {
        let mut s = Session::default();
        let bg = s.create_node("constant", Some("bg"), &map(json!({"width": 8, "height": 4, "color": [0.0, 0.0, 1.0]})), &[]).unwrap();
        let fg = s.create_node("constant", None, &map(json!({"width": 8, "height": 4, "color": [1.0, 0.0, 0.0, 1.0]})), &[]).unwrap();
        assert_eq!(fg, "constant1");
        let m = s.create_node("merge", None, &Map::new(), &[fg.clone(), bg.clone()]).unwrap();
        let img = s.render(&m, 1).unwrap();
        assert_eq!((img.width, img.height), (8, 4));
        assert!((img.get(0, 0)[0] - 1.0).abs() < 1e-6);

        s.set_params(&fg, &map(json!({"color": [0.0, 1.0, 0.0, 1.0]}))).unwrap();
        assert!((s.render(&m, 1).unwrap().get(0, 0)[1] - 1.0).abs() < 1e-6);
        assert!(s.undo());
        assert!((s.render(&m, 1).unwrap().get(0, 0)[0] - 1.0).abs() < 1e-6);
        assert!(s.redo());
        assert!((s.render(&m, 1).unwrap().get(0, 0)[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn invalid_edits_are_refused_without_an_undo_step() {
        let mut s = Session::default();
        let c = s.create_node("constant", None, &Map::new(), &[]).unwrap();
        let undo_depth = s.undo.len();
        assert!(s.set_params(&c, &map(json!({"nope": 1}))).is_err());
        assert!(s.set_params(&c, &map(json!({"color": "red"}))).is_err());
        assert!(s.create_node("no_such_op", None, &Map::new(), &[]).is_err());
        let b = s.create_node("blur", None, &Map::new(), std::slice::from_ref(&c)).unwrap();
        assert!(s.connect(&b, &c, 0).is_err(), "constant has no inputs");
        let b2 = s.create_node("blur", None, &Map::new(), std::slice::from_ref(&b)).unwrap();
        assert!(s.connect(&b2, &b, 0).unwrap_err().to_string().contains("cycle"));
        assert_eq!(s.undo.len(), undo_depth + 2);
    }

    #[test]
    fn keys_animate_a_param() {
        let mut s = Session::default();
        let c = s.create_node("constant", None, &map(json!({"width": 2, "height": 2})), &[]).unwrap();
        s.set_key(&c, "color", 1, &json!([0.0, 0.0, 0.0, 1.0])).unwrap();
        s.set_key(&c, "color", 11, &json!([1.0, 1.0, 1.0, 1.0])).unwrap();
        let mid = s.render(&c, 6).unwrap().get(0, 0)[0];
        assert!(mid > 0.1 && mid < 0.9, "{mid}");
    }

    #[test]
    fn port_labels_resolve() {
        let mut s = Session::default();
        let c = s.create_node("constant", None, &Map::new(), &[]).unwrap();
        let m = s.create_node("merge", None, &Map::new(), &[]).unwrap();
        assert_eq!(s.port_index(&m, &json!("B")).unwrap(), 1);
        s.connect(&c, &m, s.port_index(&m, &json!("b")).unwrap()).unwrap();
        assert_eq!(s.node(&m).unwrap().inputs[1], c);
    }
}
