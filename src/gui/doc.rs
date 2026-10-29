//! The open document: the graph plus editor state, and every graph edit.
//!
//! Each edit bumps `revision`, which is what tells the viewer to re-render.
//!
//! Undo works on whole-graph snapshots (graphs are small). Edits made during
//! one gesture — a slider drag, a node move, typing in a field — are merged
//! into a single undo step by committing only once the gesture has ended.

use arclight::anim::{self, Interp};
use arclight::graph::{Graph, NodeSpec};
use arclight::ops;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub const NODE_W: f32 = 150.0;
pub const NODE_H: f32 = 46.0;
const UNDO_LIMIT: usize = 200;

struct History {
    undo: Vec<Graph>,
    redo: Vec<Graph>,
    /// The graph as of the last undo step.
    committed: Graph,
    committed_edits: u64,
}

pub struct Doc {
    pub graph: Graph,
    pub selected: Option<String>,
    pub revision: u64,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Counts every edit, including ones (like node moves) that don't need a
    /// re-render. Compared against the history to find uncommitted edits.
    edits: u64,
    history: History,
}

impl Doc {
    pub fn new(graph: Graph, path: Option<PathBuf>) -> Doc {
        let history = History { undo: Vec::new(), redo: Vec::new(), committed: Graph::default(), committed_edits: 0 };
        let mut doc = Doc { graph, selected: None, revision: 1, path, dirty: false, edits: 0, history };
        doc.normalize();
        doc.auto_layout();
        doc.history.committed = doc.graph.clone();
        doc
    }

    /// An edit that affects rendering.
    pub fn changed(&mut self) {
        self.revision += 1;
        self.dirty = true;
        self.edits += 1;
    }

    /// An edit that doesn't affect rendering (e.g. moving a node).
    pub fn touched(&mut self) {
        self.dirty = true;
        self.edits += 1;
    }

    /// Record an undo step for any edits made since the last one, unless a
    /// gesture is still in progress. Call once per UI frame.
    pub fn commit(&mut self, gesture_in_progress: bool) {
        if self.edits == self.history.committed_edits || gesture_in_progress {
            return;
        }
        self.history.committed_edits = self.edits;
        if self.graph != self.history.committed {
            let prev = std::mem::replace(&mut self.history.committed, self.graph.clone());
            self.history.undo.push(prev);
            if self.history.undo.len() > UNDO_LIMIT {
                self.history.undo.remove(0);
            }
            self.history.redo.clear();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty() || self.graph != self.history.committed
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub fn undo(&mut self) {
        self.commit(false);
        if let Some(prev) = self.history.undo.pop() {
            let cur = std::mem::replace(&mut self.graph, prev);
            self.history.redo.push(cur);
            self.restored();
        }
    }

    pub fn redo(&mut self) {
        self.commit(false);
        if let Some(next) = self.history.redo.pop() {
            let cur = std::mem::replace(&mut self.graph, next);
            self.history.undo.push(cur);
            self.restored();
        }
    }

    fn restored(&mut self) {
        self.history.committed = self.graph.clone();
        self.changed();
        self.history.committed_edits = self.edits;
        if self.selected.as_ref().is_some_and(|id| self.node(id).is_none()) {
            self.selected = None;
        }
    }

    pub fn node(&self, id: &str) -> Option<&NodeSpec> {
        self.graph.node(id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut NodeSpec> {
        self.graph.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Give every node exactly one input slot per port of its op, so the
    /// editor can address ports by index.
    fn normalize(&mut self) {
        for n in &mut self.graph.nodes {
            if let Some(info) = ops::info(&n.op) {
                if n.inputs.len() < info.inputs.len() {
                    n.inputs.resize(info.inputs.len(), String::new());
                }
            }
        }
    }

    pub fn unique_id(&self, base: &str) -> String {
        (1..)
            .map(|i| format!("{base}{i}"))
            .find(|id| self.node(id).is_none())
            .unwrap()
    }

    /// Add a node with its op's default params at graph position `pos`.
    pub fn add_node(&mut self, op: &str, pos: [f32; 2]) -> String {
        let info = ops::info(op).expect("known op");
        let id = self.unique_id(op);
        let mut params = ops::params_with_defaults(op, &Value::Null);
        ops::fit_new_shape(op, &mut params, self.graph.project.format);
        if op == "media_out" {
            params["first_frame"] = self.graph.project.first_frame.into();
            params["last_frame"] = self.graph.project.last_frame.into();
            params["fps"] = self.graph.project.fps.into();
        }
        self.graph.nodes.push(NodeSpec {
            id: id.clone(),
            op: op.to_string(),
            inputs: vec![String::new(); info.inputs.len()],
            params,
            pos: Some(pos),
        });
        self.changed();
        id
    }

    /// Remove a node. Downstream ports it fed are reconnected to its first
    /// input, so deleting a node from the middle of a chain keeps the chain.
    pub fn delete_node(&mut self, id: &str) {
        let Some(i) = self.graph.nodes.iter().position(|n| n.id == id) else { return };
        let removed = self.graph.nodes.remove(i);
        let heal = removed.inputs.first().cloned().unwrap_or_default();
        for n in &mut self.graph.nodes {
            for inp in &mut n.inputs {
                if inp == id {
                    *inp = heal.clone();
                }
            }
        }
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        self.changed();
    }

    /// Is `ancestor` upstream of (or equal to) `node`?
    fn is_upstream(&self, ancestor: &str, node: &str) -> bool {
        let mut stack = vec![node.to_string()];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if id == ancestor {
                return true;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(n) = self.node(&id) {
                stack.extend(n.inputs.iter().filter(|s| !s.is_empty()).cloned());
            }
        }
        false
    }

    /// Connect `from`'s output to input `port` of `to`. Refuses connections
    /// that would create a cycle.
    pub fn connect(&mut self, from: &str, to: &str, port: usize) -> bool {
        if self.is_upstream(to, from) {
            return false;
        }
        let Some(n) = self.node_mut(to) else { return false };
        if port >= n.inputs.len() {
            return false;
        }
        n.inputs[port] = from.to_string();
        self.changed();
        true
    }

    /// Splice `node` into the wire from `src` to input `port` of `dst`:
    /// `src` feeds the node's first input and the node feeds that port.
    /// Refuses (returning false, changing nothing) when the node has no
    /// input, is an end of the wire, or would close a cycle.
    pub fn insert_on_wire(&mut self, node: &str, src: &str, dst: &str, port: usize) -> bool {
        let ok = node != src
            && node != dst
            && self.node(node).is_some_and(|n| !n.inputs.is_empty() && n.op != "viewer")
            && self.node(dst).is_some_and(|n| n.inputs.get(port).is_some_and(|s| s == src))
            && !self.is_upstream(node, src)
            && !self.is_upstream(dst, node);
        if !ok {
            return false;
        }
        self.node_mut(node).unwrap().inputs[0] = src.to_string();
        self.node_mut(dst).unwrap().inputs[port] = node.to_string();
        self.changed();
        true
    }

    /// Bring `node` into the wire from `src` to input `port` of `dst` with a
    /// new Merge, halfway along the wire: `src` feeds its A, `node` its B,
    /// and the Merge feeds that port. The Merge's id, or None (changing
    /// nothing) when `node` has no output, is an end of the wire, or would
    /// close a cycle.
    pub fn merge_into_wire(&mut self, node: &str, src: &str, dst: &str, port: usize) -> Option<String> {
        let ok = node != src
            && node != dst
            && self.node(node).is_some_and(|n| n.op != "viewer")
            && self.node(dst).is_some_and(|n| n.inputs.get(port).is_some_and(|s| s == src))
            && !self.is_upstream(dst, node);
        if !ok {
            return None;
        }
        let at = |id: &str| self.node(id).and_then(|n| n.pos).unwrap_or([0.0, 0.0]);
        let (a, b) = (at(src), at(dst));
        let merge = self.add_node("merge", [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
        let m = self.node_mut(&merge).unwrap();
        m.inputs[0] = src.to_string();
        m.inputs[1] = node.to_string();
        self.node_mut(dst).unwrap().inputs[port] = merge.clone();
        self.changed();
        Some(merge)
    }

    /// Take `node` out of the graph's wires, keeping it: everything it fed
    /// is fed by its first input instead (as when deleting it), and its own
    /// inputs are cleared. False when it had no connections.
    pub fn extract(&mut self, id: &str) -> bool {
        let Some(heal) = self.node(id).map(|n| n.inputs.first().cloned().unwrap_or_default()) else { return false };
        let mut changed = false;
        for n in &mut self.graph.nodes {
            for inp in n.inputs.iter_mut().filter(|i| *i == id) {
                *inp = heal.clone();
                changed = true;
            }
        }
        for inp in self.node_mut(id).unwrap().inputs.iter_mut().filter(|i| !i.is_empty()) {
            inp.clear();
            changed = true;
        }
        if changed {
            self.changed();
        }
        changed
    }

    pub fn disconnect(&mut self, to: &str, port: usize) {
        if let Some(n) = self.node_mut(to) {
            if let Some(slot) = n.inputs.get_mut(port) {
                slot.clear();
                self.changed();
            }
        }
    }

    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new.is_empty() {
            return Err("name can't be empty".into());
        }
        if new == old {
            return Ok(());
        }
        if self.node(new).is_some() {
            return Err(format!("'{new}' is already used"));
        }
        for n in &mut self.graph.nodes {
            if n.id == old {
                n.id = new.to_string();
            }
            for inp in &mut n.inputs {
                if inp == old {
                    *inp = new.to_string();
                }
            }
        }
        if self.selected.as_deref() == Some(old) {
            self.selected = Some(new.to_string());
        }
        self.changed();
        Ok(())
    }

    /// What the viewer shows: the first Viewer node, else the selection.
    pub fn view_target(&self) -> Option<String> {
        self.graph
            .nodes
            .iter()
            .find(|n| n.op == "viewer")
            .map(|n| n.id.clone())
            .or_else(|| self.selected.clone())
    }

    /// The node whose image the viewer shows: the Viewer node's input, or
    /// the selected node when there is no Viewer.
    pub fn viewed(&self) -> Option<String> {
        match self.graph.nodes.iter().find(|n| n.op == "viewer") {
            Some(v) => v.inputs.first().filter(|i| !i.is_empty()).cloned(),
            None => self.selected.clone(),
        }
    }

    /// Route `id` into the Viewer, creating a Viewer node if there is none.
    pub fn view_node(&mut self, id: &str) {
        if self.node(id).is_none_or(|n| n.op == "viewer") {
            return;
        }
        let viewer = match self.graph.nodes.iter().find(|n| n.op == "viewer") {
            Some(v) => v.id.clone(),
            None => {
                let p = self.node(id).and_then(|n| n.pos).unwrap_or([0.0, 0.0]);
                self.add_node("viewer", [p[0] + NODE_W + 60.0, p[1] + 80.0])
            }
        };
        self.connect(id, &viewer, 0);
    }

    /// The stored value (with defaults): a plain value, or a keyframe curve.
    pub fn param(&self, id: &str, name: &str) -> Value {
        self.node(id)
            .map(|n| ops::params_with_defaults(&n.op, &n.params)[name].clone())
            .unwrap_or(Value::Null)
    }

    /// A param's value at `frame`, with keyframes evaluated.
    pub fn param_at(&self, id: &str, name: &str, frame: i64) -> Value {
        anim::evaluate(&self.param(id, name), frame as f64)
    }

    /// Edit a param from the UI: animated params get a key at `frame`
    /// (auto-key); static ones are simply replaced.
    pub fn edit_param(&mut self, id: &str, name: &str, frame: i64, value: Value) {
        let raw = self.param(id, name);
        if anim::is_animated(&raw) {
            self.set_param(id, name, anim::set_key(&raw, frame, &value));
        } else {
            self.set_param(id, name, value);
        }
    }

    /// Add a key at `frame` holding the param's current value there.
    pub fn add_key(&mut self, id: &str, name: &str, frame: i64) {
        let raw = self.param(id, name);
        let here = anim::evaluate(&raw, frame as f64);
        self.set_param(id, name, anim::set_key(&raw, frame, &here));
    }

    pub fn remove_key(&mut self, id: &str, name: &str, frame: i64) {
        let raw = self.param(id, name);
        self.set_param(id, name, anim::remove_key(&raw, frame));
    }

    /// Drop all keys, keeping the value at `frame` as the static value.
    pub fn clear_animation(&mut self, id: &str, name: &str, frame: i64) {
        let here = self.param_at(id, name, frame);
        self.set_param(id, name, here);
    }

    pub fn set_interp(&mut self, id: &str, name: &str, interp: Interp) {
        let raw = self.param(id, name);
        self.set_param(id, name, anim::set_interp(&raw, interp));
    }

    /// All keyframe frames on a node, across its params, sorted and unique.
    pub fn node_key_frames(&self, id: &str) -> Vec<i64> {
        let Some(n) = self.node(id) else { return Vec::new() };
        let mut frames: Vec<i64> = n
            .params
            .as_object()
            .map(|m| m.values().flat_map(anim::key_frames).collect())
            .unwrap_or_default();
        frames.sort_unstable();
        frames.dedup();
        frames
    }

    pub fn is_node_animated(&self, id: &str) -> bool {
        self.node(id)
            .and_then(|n| n.params.as_object())
            .is_some_and(|m| m.values().any(anim::is_animated))
    }

    /// Drop a param from a node (back to its default, or gone for one
    /// outside the op's spec).
    pub fn remove_param(&mut self, id: &str, name: &str) {
        if self.node_mut(id).and_then(|n| n.params.as_object_mut()).and_then(|m| m.remove(name)).is_some() {
            self.changed();
        }
    }

    pub fn set_param(&mut self, id: &str, name: &str, value: Value) {
        if let Some(n) = self.node_mut(id) {
            if !n.params.is_object() {
                n.params = Value::Object(Default::default());
            }
            n.params[name] = value;
            self.changed();
        }
    }

    /// Place nodes added without a position (e.g. by a script) into a graph
    /// that's already laid out: right of their rightmost input, or below
    /// everything for sources, then moved down until clear of other nodes.
    pub fn place_new_nodes(&mut self) {
        if self.graph.nodes.iter().all(|n| n.pos.is_none()) {
            return self.auto_layout();
        }
        for i in 0..self.graph.nodes.len() {
            if self.graph.nodes[i].pos.is_some() {
                continue;
            }
            let placed = || self.graph.nodes.iter().filter_map(|n| n.pos);
            let feeds = self.graph.nodes[i].inputs.iter().filter_map(|id| self.node(id).and_then(|n| n.pos));
            let mut p = match feeds.max_by(|a, b| a[0].total_cmp(&b[0])) {
                Some(q) => [q[0] + NODE_W + 50.0, q[1]],
                None => [
                    placed().map(|q| q[0]).fold(f32::INFINITY, f32::min),
                    placed().map(|q| q[1]).fold(f32::NEG_INFINITY, f32::max) + NODE_H + 40.0,
                ],
            };
            while placed().any(|q| (q[0] - p[0]).abs() < NODE_W + 20.0 && (q[1] - p[1]).abs() < NODE_H + 20.0) {
                p[1] += NODE_H + 40.0;
            }
            self.graph.nodes[i].pos = Some(p);
        }
    }

    /// Place nodes that have no position (e.g. from a hand-written JSON
    /// graph): one column per depth from the sources, left to right.
    pub fn auto_layout(&mut self) {
        if self.graph.nodes.iter().all(|n| n.pos.is_some()) {
            return;
        }
        let mut depth: HashMap<String, usize> = HashMap::new();
        // Longest-path depth; bounded passes so a malformed cyclic file can't hang.
        for _ in 0..self.graph.nodes.len() {
            let mut changed = false;
            for n in &self.graph.nodes {
                let d = n
                    .inputs
                    .iter()
                    .filter_map(|i| depth.get(i))
                    .map(|d| d + 1)
                    .max()
                    .unwrap_or(0);
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
        for n in &mut self.graph.nodes {
            if n.pos.is_none() {
                let col = depth.get(&n.id).copied().unwrap_or(0);
                let row = rows.entry(col).or_insert(0);
                n.pos = Some([col as f32 * (NODE_W + 50.0), *row as f32 * (NODE_H + 40.0)]);
                *row += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scripted_nodes_are_placed_clear_of_existing_ones() {
        let mut d = Doc::new(Graph::default(), None);
        let a = d.add_node("constant", [0.0, 0.0]);
        d.add_node("constant", [NODE_W + 50.0, 0.0]);
        let new = |id: &str, op: &str, inputs: Vec<String>| NodeSpec { id: id.into(), op: op.into(), inputs, params: json!({}), pos: None };
        d.graph.nodes.push(new("src", "constant", vec![]));
        d.graph.nodes.push(new("b", "blur", vec![a.clone()]));
        d.place_new_nodes();
        let pos = |id: &str| d.node(id).unwrap().pos.unwrap();
        assert!(pos("src")[1] > 0.0, "a new source goes below the graph");
        assert_eq!(pos("b")[0], NODE_W + 50.0, "right of its input");
        assert!(pos("b")[1] > 0.0, "moved down off the occupied spot");
    }

    #[test]
    fn nodes_splice_into_wires_and_extract_from_them() {
        let mut d = Doc::new(Graph::default(), None);
        let src = d.add_node("constant", [0.0, 0.0]);
        let merge = d.add_node("merge", [0.0, 0.0]);
        let blur = d.add_node("blur", [0.0, 0.0]);
        d.connect(&src, &merge, 1);
        assert!(d.insert_on_wire(&blur, &src, &merge, 1));
        let inputs = |d: &Doc, id: &str| d.node(id).unwrap().inputs.clone();
        assert_eq!((inputs(&d, &blur)[0].as_str(), inputs(&d, &merge)[1].as_str()), (src.as_str(), blur.as_str()));
        // Not onto a wire that isn't there, nor into its own wire.
        assert!(!d.insert_on_wire(&blur, &src, &merge, 0));
        assert!(!d.insert_on_wire(&blur, &blur, &merge, 1));
        // A source has no input to take the wire.
        let other = d.add_node("constant", [0.0, 0.0]);
        assert!(!d.insert_on_wire(&other, &blur, &merge, 1));
        // Extracting heals the wire and frees the node.
        assert!(d.extract(&blur));
        assert_eq!(inputs(&d, &merge)[1], src);
        assert!(inputs(&d, &blur).iter().all(String::is_empty));
        assert!(!d.extract(&blur));
    }

    fn doc() -> Doc {
        let mut d = Doc::new(Graph::default(), None);
        d.add_node("blur", [0.0, 0.0]);
        d.commit(false);
        d
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut d = doc();
        // Many edits while the pointer is held...
        for r in 1..=20 {
            d.set_param("blur1", "radius", json!(r));
            d.commit(true);
        }
        // ...commit as one step when it's released.
        d.commit(false);
        assert_eq!(d.param("blur1", "radius"), json!(20));
        d.undo();
        assert_eq!(d.param("blur1", "radius"), json!(8.0));
        d.redo();
        assert_eq!(d.param("blur1", "radius"), json!(20));
    }

    #[test]
    fn undo_removes_added_node_and_new_edit_clears_redo() {
        let mut d = doc();
        d.undo();
        assert!(d.graph.nodes.is_empty());
        assert!(d.can_redo());
        d.add_node("grade", [0.0, 0.0]);
        d.commit(false);
        assert!(!d.can_redo());
    }

    #[test]
    fn editing_animated_param_auto_keys() {
        let mut d = doc();
        d.add_key("blur1", "radius", 1);
        d.edit_param("blur1", "radius", 50, json!(30.0));
        assert_eq!(d.node_key_frames("blur1"), vec![1, 50]);
        assert_eq!(d.param_at("blur1", "radius", 50), json!(30.0));
        assert_eq!(d.param_at("blur1", "radius", 1), json!(8.0));
        d.clear_animation("blur1", "radius", 50);
        assert_eq!(d.param("blur1", "radius"), json!(30.0));
    }
}
