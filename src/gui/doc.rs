//! The open document: the graph plus editor state, and every graph edit.
//!
//! Each edit bumps `revision`, which is what tells the viewer to re-render.
//!
//! Undo works on whole-graph snapshots (graphs are small). Edits made during
//! one gesture — a slider drag, a node move, typing in a field — are merged
//! into a single undo step by committing only once the gesture has ended.

use arclight::anim::{self, Interp};
use arclight::graph::{Backdrop, Graph, NodeSpec};
pub use arclight::graph::{NODE_H, NODE_W};
use arclight::ops;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

/// Undo steps kept unless the preferences say otherwise.
pub const UNDO_LEVELS: usize = 200;

struct History {
    undo: Vec<Graph>,
    redo: Vec<Graph>,
    /// The graph as of the last undo step.
    committed: Graph,
    committed_edits: u64,
}

pub struct Doc {
    pub graph: Graph,
    /// The primary selected node: the one the Properties panel, the viewer's
    /// handles and scripts' `select_node` work on.
    pub selected: Option<String>,
    /// Every selected node, the primary among them (see [`Doc::selection`]:
    /// code that only sets `selected` selects just that node).
    selection: Vec<String>,
    /// The primary `selection` was made with: if `selected` has been set to
    /// something else since, the selection is just that.
    selection_for: Option<String>,
    /// A selected backdrop (selecting one deselects the nodes).
    pub selected_backdrop: Option<String>,
    /// A selected wire: `src`'s output into input `port` of `dst`
    /// (selecting one deselects the nodes and backdrop).
    pub selected_wire: Option<(String, String, usize)>,
    /// A 3D setup was just added for the first time in this project: the
    /// app shows its render (this node) in the viewer's 3D view, once.
    pub show_3d: Option<String>,
    /// Whether that has happened (also kept in the project's view state;
    /// this survives undoing the setup).
    intro_3d: bool,
    /// A node shown in place of the Viewer node's input (double-click, V),
    /// until the Viewer is viewed again. View state: not saved or undone.
    pub view_override: Option<String>,
    /// The Viewer node shown when a project has several: the one last
    /// viewed, clicked or wired into (see [`viewer`](Self::viewer)). View
    /// state: not saved or undone.
    active_viewer: Option<String>,
    /// The frame of the image in the viewer, if any: new shapes are placed
    /// and sized to fit it (the project format otherwise), so they land in
    /// the picture being looked at even when it isn't project-sized.
    pub view_format: Option<arclight::image::Size>,
    pub revision: u64,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Counts every edit, including ones (like node moves) that don't need a
    /// re-render. Compared against the history to find uncommitted edits.
    edits: u64,
    history: History,
    /// The most undo steps kept (0: no limit; Preferences ▸ Files).
    pub undo_levels: usize,
}

/// A revision no document has had yet: unique across the process, so a
/// newly loaded project never shares one with the project it replaced (the
/// viewers' caches are keyed by it).
fn next_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Doc {
    pub fn new(graph: Graph, path: Option<PathBuf>) -> Doc {
        let history = History { undo: Vec::new(), redo: Vec::new(), committed: Graph::default(), committed_edits: 0 };
        let mut doc = Doc { graph, selected: None, selection: Vec::new(), selection_for: None, selected_backdrop: None, selected_wire: None, show_3d: None, intro_3d: false, view_override: None, active_viewer: None, view_format: None, revision: next_revision(), path, dirty: false, edits: 0, history, undo_levels: UNDO_LEVELS };
        doc.normalize();
        doc.auto_layout();
        doc.history.committed = doc.graph.clone();
        doc
    }

    /// A new, empty project with `project`'s format, rate and frames
    /// (Preferences ▸ New project).
    pub fn blank(project: arclight::graph::Project) -> Doc {
        Doc::new(Graph { project, ..Graph::default() }, None)
    }

    /// An edit that affects rendering.
    pub fn changed(&mut self) {
        self.normalize();
        self.revision = next_revision();
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
            if self.undo_levels > 0 && self.history.undo.len() > self.undo_levels {
                let extra = self.history.undo.len() - self.undo_levels;
                self.history.undo.drain(..extra);
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
        let sel = self.selection();
        self.set_selection(sel.into_iter().filter(|id| self.graph.node(id).is_some()).collect());
        if self.selected_backdrop.as_ref().is_some_and(|b| self.graph.backdrop(b).is_none()) {
            self.selected_backdrop = None;
        }
        if self.selected_wire.as_ref().is_some_and(|w| !self.wire_exists(w)) {
            self.selected_wire = None;
        }
    }

    // ---- Selection ----

    /// The selected nodes, the primary ([`Doc::selected`]) last if it was
    /// picked last. Only `selected` set (by the viewer, a script…): just it.
    pub fn selection(&self) -> Vec<String> {
        match &self.selected {
            None => Vec::new(),
            Some(p) if self.selection_for.as_ref() != Some(p) => vec![p.clone()],
            Some(_) => self.selection.iter().filter(|id| self.graph.node(id).is_some()).cloned().collect(),
        }
    }

    pub fn is_selected(&self, id: &str) -> bool {
        match &self.selected {
            None => false,
            Some(p) if self.selection_for.as_ref() != Some(p) => p == id,
            Some(_) => self.selection.iter().any(|s| s == id),
        }
    }

    /// Select `ids` (the last is the primary), and no backdrop.
    fn set_selection(&mut self, ids: Vec<String>) {
        self.selected = ids.last().cloned();
        self.selection_for = self.selected.clone();
        self.selection = ids;
        if self.selected.is_some() {
            self.selected_backdrop = None;
            self.selected_wire = None;
        }
    }

    /// Select the wire from `src` into input `port` of `dst` (and nothing
    /// else).
    pub fn select_wire(&mut self, src: &str, dst: &str, port: usize) {
        self.set_selection(Vec::new());
        self.selected_backdrop = None;
        self.selected_wire = Some((src.to_string(), dst.to_string(), port));
    }

    /// Whether `src` still feeds input `port` of `dst`.
    pub fn wire_exists(&self, (src, dst, port): &(String, String, usize)) -> bool {
        self.node(dst).is_some_and(|n| n.inputs.get(*port) == Some(src))
    }

    /// Just `id`.
    pub fn select_only(&mut self, id: &str) {
        self.set_selection(vec![id.to_string()]);
    }

    /// Add `id` to the selection (it becomes the primary), or take it out.
    pub fn toggle_selected(&mut self, id: &str) {
        let mut sel = self.selection();
        match sel.iter().position(|s| s == id) {
            Some(i) => {
                sel.remove(i);
            }
            None => sel.push(id.to_string()),
        }
        self.set_selection(sel);
    }

    /// Select `ids` (a box select), added to the selection with `add`.
    pub fn select_nodes(&mut self, ids: &[String], add: bool) {
        let mut sel = if add { self.selection() } else { Vec::new() };
        for id in ids {
            if !sel.contains(id) {
                sel.push(id.clone());
            }
        }
        self.set_selection(sel);
    }

    /// Merge `source`'s nodes and backdrops (all, or `how.only`) into the
    /// graph (see [`arclight::graph_merge::merge`]): clashing ids renamed,
    /// the project's settings kept, the new nodes selected. One undo step
    /// (all in this frame's edit).
    pub fn import(&mut self, source: &Graph, how: &arclight::graph_merge::Merge) -> arclight::graph_merge::Merged {
        let added = arclight::graph_merge::merge(&mut self.graph, source, how);
        self.set_selection(added.nodes.clone());
        self.changed();
        added
    }

    pub fn select_all(&mut self) {
        let ids: Vec<String> = self.graph.nodes.iter().map(|n| n.id.clone()).collect();
        // Keep the primary the primary.
        let mut ids: Vec<String> = ids.into_iter().filter(|id| Some(id) != self.selected.as_ref()).collect();
        ids.extend(self.selected.clone());
        self.set_selection(ids);
    }

    pub fn clear_selection(&mut self) {
        self.set_selection(Vec::new());
        self.selected_backdrop = None;
        self.selected_wire = None;
    }

    /// Select backdrop `id` (and no nodes).
    pub fn select_backdrop(&mut self, id: &str) {
        self.set_selection(Vec::new());
        self.selected_backdrop = Some(id.to_string());
        self.selected_wire = None;
    }

    /// Move nodes `ids` by `d` (graph units).
    pub fn move_nodes(&mut self, ids: &[String], d: [f32; 2]) {
        for id in ids {
            if let Some(n) = self.node_mut(id) {
                let p = n.pos.get_or_insert([0.0, 0.0]);
                p[0] += d[0];
                p[1] += d[1];
            }
        }
        self.touched();
    }

    /// Delete the selected nodes, or else the selected backdrop. False when
    /// nothing was selected.
    /// Turn nodes off (bypassed) or on again.
    pub fn set_disabled(&mut self, ids: &[String], disabled: bool) {
        let mut any = false;
        for id in ids {
            if let Some(n) = self.node_mut(id).filter(|n| n.disabled != disabled) {
                n.disabled = disabled;
                any = true;
            }
        }
        if any {
            self.changed();
        }
    }

    /// Turn the selected nodes off, or on if all are off already (D). False
    /// with none selected.
    pub fn toggle_disabled_selection(&mut self) -> bool {
        let sel = self.selection();
        if sel.is_empty() {
            return false;
        }
        let all_off = sel.iter().all(|id| self.node(id).is_some_and(|n| n.disabled));
        self.set_disabled(&sel, !all_off);
        true
    }

    pub fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        if !sel.is_empty() {
            for id in &sel {
                self.delete_node(id);
            }
            return true;
        }
        if let Some(w) = self.selected_wire.take() {
            if self.wire_exists(&w) {
                self.disconnect(&w.1, w.2);
            }
            return true;
        }
        match self.selected_backdrop.clone() {
            Some(b) => {
                self.delete_backdrop(&b);
                true
            }
            None => false,
        }
    }

    // ---- Backdrops ----

    /// A node's box in graph units: its corner and the opposite one.
    pub fn node_box(n: &NodeSpec) -> ([f32; 2], [f32; 2]) {
        n.bounds()
    }

    /// Put a backdrop behind the selected nodes, fitted to them, and select
    /// it. None when no nodes are selected.
    pub fn group_selection(&mut self) -> Option<String> {
        let sel = self.selection();
        let id = self.graph.new_backdrop_id();
        let boxes = sel.iter().filter_map(|s| self.graph.node(s)).map(Self::node_box);
        let b = Backdrop::around(&id, "Group", boxes)?;
        // In front of the backdrops it may sit inside, behind every node.
        self.graph.backdrops.push(b);
        self.select_backdrop(&id);
        self.touched();
        Some(id)
    }

    /// Remove backdrop `id` (Ungroup); its nodes stay.
    pub fn delete_backdrop(&mut self, id: &str) {
        let before = self.graph.backdrops.len();
        self.graph.backdrops.retain(|b| b.id != id);
        if self.selected_backdrop.as_deref() == Some(id) {
            self.selected_backdrop = None;
        }
        if self.graph.backdrops.len() != before {
            self.touched();
        }
    }

    /// Replace the backdrop with `b`'s id by `b`.
    pub fn set_backdrop(&mut self, b: Backdrop) {
        if let Some(old) = self.graph.backdrops.iter_mut().find(|o| o.id == b.id) {
            if *old != b {
                *old = b;
                self.touched();
            }
        }
    }

    /// The nodes fully inside backdrop `id`: the ones that move with it.
    pub fn backdrop_nodes(&self, id: &str) -> Vec<String> {
        let Some(b) = self.graph.backdrop(id) else { return Vec::new() };
        self.graph.nodes.iter().filter(|n| {
            let (a, c) = Self::node_box(n);
            b.contains(a, c)
        }).map(|n| n.id.clone()).collect()
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
            // A growable node (a Scanline Render) ends in one empty input
            // to connect more to.
            if let Some(fixed) = ops::growable(&n.op) {
                while n.inputs.len() > fixed && n.inputs.last().is_some_and(String::is_empty) {
                    n.inputs.pop();
                }
                n.inputs.push(String::new());
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
        ops::fit_new_shape(op, &mut params, self.view_format.unwrap_or(self.graph.project.format));
        if op == "media_out" {
            params["first_frame"] = self.graph.project.first_frame.into();
            params["last_frame"] = self.graph.project.last_frame.into();
            params["fps"] = self.graph.project.fps.into();
        }
        // An Alembic cache's times in the project's frames.
        if op == "read_geo" {
            params["frame_rate"] = self.graph.project.fps.into();
        }
        self.graph.nodes.push(NodeSpec {
            id: id.clone(),
            op: op.to_string(),
            inputs: vec![String::new(); info.inputs.len()],
            params,
            pos: Some(pos),
            disabled: false,
        });
        self.changed();
        id
    }

    /// Add a camera rig with its Scanline Render at `pos`: a Camera into a
    /// Scene (input 1) into the render's scene input, the Camera and Scene
    /// stacked above and to the left, the Camera backed off along +Z so a
    /// new Card fills its frame. Over `wire` (source, destination,
    /// port) carrying an image, the render goes into it: the source feeds
    /// its background and it feeds the destination. Else, after node `up`:
    /// an image `up` feeds the background and whatever `up` fed comes from
    /// the render instead; a scene `up` (a Card, say) goes into the Scene's
    /// second input. Returns (camera, scene, render).
    pub fn add_camera_rig(&mut self, pos: [f32; 2], wire: Option<(&str, &str, usize)>, up: Option<&str>) -> (String, String, String) {
        let (dx, dy) = (NODE_W + 40.0, NODE_H + 40.0);
        let render = self.add_node("scanline_render", pos);
        let scene = self.add_node("scene", [pos[0] - dx, pos[1] - dy]);
        let camera = self.add_node("camera", [pos[0] - dx, pos[1] - 2.0 * dy]);
        // Back on +Z till a new Card (1 wide, at the origin) just fills the
        // frame's width: the film back's width at the focal length.
        let z = self.param(&camera, "focal").as_f64().unwrap_or(35.0) / self.param(&camera, "film_back")[0].as_f64().unwrap_or(36.0);
        self.set_param(&camera, "translate", serde_json::json!([0.0, 0.0, z]));
        self.connect(&camera, &scene, 0);
        self.connect(&scene, &render, 0);
        let op = |d: &Doc, id: &str| d.node(id).map(|n| n.op.clone()).unwrap_or_default();
        let image_wire = wire.filter(|(src, dst, port)| {
            ops::output_kind(&op(self, src)) == ops::Kind::Image && ops::wire_error("scanline_render", &op(self, dst), *port).is_none()
        });
        if let Some((src, dst, port)) = image_wire {
            if self.node(dst).is_some_and(|n| n.inputs.get(port).is_some_and(|s| s == src)) {
                self.connect(src, &render, 2);
                self.node_mut(dst).unwrap().inputs[port] = render.clone();
                self.changed();
            }
        } else if let Some(up) = up.filter(|u| self.node(u).is_some_and(|n| n.op != "viewer")) {
            if ops::output_kind(&op(self, up)) == ops::Kind::Scene {
                self.connect(up, &scene, 1);
            } else {
                for n in &mut self.graph.nodes {
                    if n.id != render {
                        n.inputs.iter_mut().filter(|i| *i == up).for_each(|i| *i = render.clone());
                    }
                }
                self.connect(up, &render, 2);
                self.changed();
            }
        }
        (camera, scene, render)
    }

    /// Add a 3D camera setup (a Camera into a Scene into a Scanline Render,
    /// see [`add_camera_rig`](Self::add_camera_rig)) so its render shows
    /// straight away: into the selected (or aimed at) wire, its picture
    /// becoming the render's background and the render feeding where the
    /// wire went; else just before the Viewer (the first one), into its
    /// input, with what it showed as the background (a scene goes into the
    /// Scene instead); an unconnected Viewer, or none (one is made), gets
    /// the render with the selected node, else the last image node feeding
    /// nothing, as its background. The three stand in a column (beside the
    /// wire, or where the Viewer was, which moves to just below the render)
    /// moved right until clear of other nodes, inside a "3D Setup"
    /// backdrop; the Camera is selected. With no wire and nothing to see
    /// (an empty project) it stands on its own at `free_at`, with a Viewer.
    /// The first setup in a project asks the app to show it in 3D
    /// ([`show_3d`](Self::show_3d)). Returns (camera, scene, render).
    pub fn add_camera_setup(&mut self, free_at: [f32; 2]) -> (String, String, String) {
        let wire = self.selected_wire.take().filter(|w| self.wire_exists(w));
        let pos = |d: &Doc, id: &str| d.node(id).and_then(|n| n.pos).unwrap_or([0.0, 0.0]);
        let (dx, dy) = (NODE_W + 60.0, NODE_H + 40.0);
        // Without a wire: the Viewer, and the picture (or scene) it gets.
        let viewer = wire.is_none().then(|| self.viewer()).flatten();
        let feed = if wire.is_some() {
            None
        } else {
            viewer
                .as_ref()
                .and_then(|v| self.node(v).and_then(|n| n.inputs.first()).filter(|i| !i.is_empty()).cloned())
                .or_else(|| self.selected.clone().filter(|u| self.node(u).is_some_and(|n| n.op != "viewer")))
                .or_else(|| self.last_image())
        };
        // Beside what it attaches to: right of the wire's middle, level
        // with it; where the Viewer is; below and right of the picture.
        let anchor = match (&wire, &viewer, &feed) {
            (Some((src, dst, _)), _, _) => {
                let (a, b) = (pos(self, src), pos(self, dst));
                [a[0].max(b[0]) + dx, (a[1] + b[1]) / 2.0]
            }
            (None, Some(v), _) => pos(self, v),
            (None, None, Some(f)) => {
                let p = pos(self, f);
                [p[0] + dx, p[1] + dy]
            }
            _ => free_at,
        };
        let (camera, scene, render) = self.add_camera_rig(anchor, wire.as_ref().map(|(s, d, p)| (s.as_str(), d.as_str(), *p)), None);
        let viewer = match (&wire, viewer) {
            (Some(_), _) => None,
            (None, Some(v)) => Some(v),
            (None, None) => Some(self.add_node("viewer", [anchor[0], anchor[1] + dy])),
        };
        if let Some(v) = &viewer {
            if let Some(f) = &feed {
                if ops::output_kind(&self.node(f).map(|n| n.op.clone()).unwrap_or_default()) == ops::Kind::Scene {
                    self.connect(f, &scene, 1);
                } else {
                    self.connect(f, &render, 2);
                }
            }
            self.connect(&render, v, 0);
        }
        let ours = [camera.clone(), scene.clone(), render.clone()];
        // A column, render at the anchor's height: camera, scene, render.
        let column = |x: f32| [[x, anchor[1] - 2.0 * dy], [x, anchor[1] - dy], [x, anchor[1]]];
        let boxes = |x: f32| column(x).map(|p| (p, [p[0] + NODE_W, p[1] + NODE_H]));
        let clear = |d: &Doc, x: f32| {
            let Some(b) = Backdrop::around("", "", boxes(x)) else { return true };
            !d.graph.nodes.iter().filter(|n| !ours.contains(&n.id) && Some(&n.id) != viewer.as_ref()).any(|n| {
                let (a, c) = Self::node_box(n);
                a[0] < b.pos[0] + b.size[0] && c[0] > b.pos[0] && a[1] < b.pos[1] + b.size[1] && c[1] > b.pos[1]
            })
        };
        let mut x = anchor[0];
        for _ in 0..40 {
            if clear(self, x) {
                break;
            }
            x += dx / 2.0;
        }
        for (id, p) in ours.iter().zip(column(x)) {
            if let Some(n) = self.node_mut(id) {
                n.pos = Some(p);
            }
        }
        // The Viewer last, just below the render.
        if let Some(n) = viewer.as_ref().and_then(|v| self.node_mut(v)) {
            n.pos = Some([x, anchor[1] + dy]);
        }
        let id = self.graph.new_backdrop_id();
        if let Some(mut b) = Backdrop::around(&id, "3D Setup", boxes(x)) {
            b.color = Backdrop::COLORS[7];
            self.graph.backdrops.push(b);
        }
        self.touched();
        self.select_only(&camera);
        // The first in this project: show it in 3D (once; remembered with
        // the project's view settings, so it's no edit).
        if !self.intro_3d && !self.graph.view.0.contains_key("intro_3d") {
            self.graph.view.0.insert("intro_3d".into(), serde_json::Value::Bool(true));
            self.intro_3d = true;
            self.show_3d = Some(render.clone());
        }
        (camera, scene, render)
    }

    /// The last image node (in the graph's order) that feeds nothing but
    /// outputs (Viewers, Writes, Media Outs): what ends the chain.
    fn last_image(&self) -> Option<String> {
        let output = |op: &str| matches!(op, "viewer" | "write" | "media_out");
        let fed: HashSet<&str> = self.graph.nodes.iter().filter(|n| !output(&n.op)).flat_map(|n| n.inputs.iter().map(String::as_str)).collect();
        self.graph
            .nodes
            .iter()
            .rev()
            .find(|n| !output(&n.op) && !fed.contains(n.id.as_str()) && ops::output_kind(&n.op) == ops::Kind::Image)
            .map(|n| n.id.clone())
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
        if self.is_selected(id) {
            let rest: Vec<String> = self.selection().into_iter().filter(|s| s != id).collect();
            self.set_selection(rest);
        }
        self.changed();
    }

    /// Is `ancestor` upstream of (or equal to) `node`?
    pub fn is_upstream(&self, ancestor: &str, node: &str) -> bool {
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
        // Wiring into a Viewer is asking to see it.
        if n.op == "viewer" {
            self.active_viewer = Some(to.to_string());
        }
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
    /// new Merge, halfway along the wire: `node` feeds its A (over), `src`
    /// its B (under), and the Merge feeds that port. The Merge's id, or None (changing
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
        m.inputs[0] = node.to_string();
        m.inputs[1] = src.to_string();
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
            arclight::graph_merge::rename_refs(&n.op, &mut n.params, old, new);
        }
        let lists = [&mut self.selected, &mut self.view_override, &mut self.active_viewer, &mut self.selection_for].into_iter().flatten();
        for id in lists.chain(self.selection.iter_mut()) {
            if id == old {
                *id = new.to_string();
            }
        }
        self.changed();
        Ok(())
    }

    /// The node viewed in place of the Viewer's input, if it still exists.
    pub fn overridden(&self) -> Option<&str> {
        self.view_override.as_deref().filter(|id| self.node(id).is_some())
    }

    /// The Viewer node the viewer shows: the one last viewed, clicked or
    /// wired into, else the first.
    pub fn viewer(&self) -> Option<String> {
        self.active_viewer
            .as_deref()
            .filter(|id| self.node(id).is_some_and(|n| n.op == "viewer"))
            .map(str::to_string)
            .or_else(|| self.graph.nodes.iter().find(|n| n.op == "viewer").map(|n| n.id.clone()))
    }

    /// What the viewer shows: a node viewed by double-click (or V), else
    /// the Viewer node, else the selection.
    pub fn view_target(&self) -> Option<String> {
        self.overridden().map(str::to_string).or_else(|| self.viewer()).or_else(|| self.selected.clone())
    }

    /// The node whose image the viewer shows: the overriding node, the
    /// Viewer node's input, or the selected node when there is no Viewer.
    pub fn viewed(&self) -> Option<String> {
        if let Some(id) = self.overridden() {
            return Some(id.to_string());
        }
        match self.viewer() {
            Some(v) => self.node(&v)?.inputs.first().filter(|i| !i.is_empty()).cloned(),
            None => self.selected.clone(),
        }
    }

    /// Show `id` in the viewer. A Viewer node goes back to showing its own
    /// input; any other node is shown in its place, leaving the graph
    /// alone. With no Viewer yet, one is made and `id` wired into it.
    pub fn view_node(&mut self, id: &str) {
        let Some(n) = self.node(id) else { return };
        if n.op == "viewer" {
            self.view_override = None;
            self.active_viewer = Some(id.to_string());
            return;
        }
        if self.viewer().is_some() {
            self.view_override = Some(id.to_string());
            return;
        }
        self.route_to_viewer(id);
    }

    /// Wire `id` into the Viewer node, creating one if there is none, and
    /// show the Viewer.
    pub fn route_to_viewer(&mut self, id: &str) {
        if self.node(id).is_none_or(|n| n.op == "viewer") {
            return;
        }
        self.view_override = None;
        let viewer = match self.viewer() {
            Some(v) => v,
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

    /// A param's value at `frame`, with keyframes evaluated and a live
    /// link followed.
    /// A link giving one number for a list param stands for it in every
    /// component, as when rendering.
    pub fn param_at(&self, id: &str, name: &str, frame: i64) -> Value {
        let lookup = |n: &str| self.node(n);
        let raw = self.param(id, name);
        let v = arclight::link::resolve(&lookup, &raw, frame as f64, self.graph.project.clock());
        let default = || self.node(id).and_then(|n| ops::info(&n.op))?.params.iter().find(|s| s.name == name).map(|s| s.default_value());
        match (v.as_f64(), arclight::link::is_link(&raw)) {
            (Some(x), true) => match default() {
                Some(Value::Array(d)) => Value::from(vec![x; d.len()]),
                _ => v,
            },
            _ => v,
        }
    }

    /// Why a linked param can't be worked out at `frame` (a formula that
    /// doesn't parse, reads a missing node, loops…), if it can't.
    pub fn param_error(&self, id: &str, name: &str, frame: i64) -> Option<String> {
        let lookup = |n: &str| self.node(n);
        arclight::link::error(&lookup, &self.param(id, name), frame as f64, self.graph.project.clock())
    }

    /// Edit a param from the UI: animated params get a key at `frame`
    /// (auto-key); static ones are simply replaced.
    pub fn edit_param(&mut self, id: &str, name: &str, frame: i64, value: Value) {
        let raw = self.param(id, name);
        // A linked value follows its source; Unlink (in the panel) first.
        if arclight::link::is_link(&raw) {
            return;
        }
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
        self.graph.layout(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    /// A loaded project never shares a revision with the one it replaced,
    /// or the viewers (keyed by it) would keep showing the old project.
    #[test]
    fn revisions_unique_across_documents() {
        let a = Doc::new(Graph::default(), None);
        let mut b = Doc::new(Graph::default(), None);
        assert_ne!(a.revision, b.revision);
        let before = b.revision;
        b.changed();
        assert!(b.revision > before);
        assert_ne!(Doc::new(Graph::default(), None).revision, b.revision);
    }

    /// Importing a project: its nodes come in renamed where they clash,
    /// selected, the project's settings kept; one undo takes it all back.
    #[test]
    fn an_import_is_one_undo_step() {
        let mut d = Doc::new(Graph::default(), None);
        d.graph.project.fps = 25.0;
        let a = d.add_node("constant", [0.0, 0.0]);
        d.commit(false);
        let before = d.graph.clone();
        let source = Graph::from_str(r#"{"project": {"first_frame": 1, "last_frame": 9, "fps": 30.0}, "nodes": [
            {"id": "constant1", "op": "constant", "pos": [0, 0]},
            {"id": "blur1", "op": "blur", "inputs": ["constant1"], "pos": [200, 0]}]}"#).unwrap();
        let added = d.import(&source, &arclight::graph_merge::Merge { only: None, place: arclight::graph_merge::Place::Beside });
        d.commit(false);
        assert_eq!(added.nodes, ["constant2", "blur1"]);
        assert_eq!(d.node("blur1").unwrap().inputs[0], "constant2");
        assert_eq!(d.selection(), ["constant2", "blur1"], "the imported nodes selected");
        assert_eq!((d.graph.project.fps, d.graph.nodes.len()), (25.0, 3));
        assert!(d.node(&a).is_some() && d.dirty);
        d.undo();
        assert_eq!(d.graph, before, "one undo: as before the import");
    }

    #[test]
    fn a_selection_holds_several_nodes_beside_the_primary() {
        let mut d = Doc::new(Graph::default(), None);
        let a = d.add_node("constant", [0.0, 0.0]);
        let b = d.add_node("blur", [200.0, 0.0]);
        let c = d.add_node("blur", [400.0, 0.0]);
        d.select_nodes(&[a.clone(), b.clone()], false);
        assert_eq!((d.selection(), d.selected.clone()), (vec![a.clone(), b.clone()], Some(b.clone())));
        d.toggle_selected(&b);
        assert_eq!((d.selection(), d.selected.clone()), (vec![a.clone()], Some(a.clone())), "the primary moves to what's left");
        // Code that only sets `selected` (the viewer, scripts) selects just it.
        d.select_all();
        assert_eq!(d.selection().len(), 3);
        d.selected = Some(c.clone());
        assert_eq!(d.selection(), std::slice::from_ref(&c));
        assert!(!d.is_selected(&a));
        d.selected = None;
        assert!(d.selection().is_empty());
        // Renames follow; deleting the selection deletes them all.
        d.select_nodes(&[a.clone(), b.clone()], false);
        d.rename(&a, "plate").unwrap();
        assert_eq!(d.selection(), ["plate".to_string(), b.clone()]);
        d.commit(false);
        assert!(d.delete_selection());
        assert_eq!(d.graph.nodes.len(), 1);
        assert!(d.selection().is_empty());
        d.commit(false);
        d.undo();
        assert_eq!(d.graph.nodes.len(), 3);
        // A backdrop: selected on its own, saved with the graph.
        d.select_nodes(&[b.clone(), c.clone()], false);
        let bid = d.group_selection().unwrap();
        assert!(d.selection().is_empty() && d.selected_backdrop.as_ref() == Some(&bid));
        let again = Doc::new(Graph::from_str(&d.graph.to_json()).unwrap(), None);
        assert_eq!(again.graph.backdrops, d.graph.backdrops);
        assert_eq!(again.backdrop_nodes(&bid), [b.clone(), c.clone()]);
        assert!(d.delete_selection(), "deletes the box");
        assert!(d.graph.backdrops.is_empty() && d.graph.nodes.len() == 3);
    }

    fn inputs(d: &Doc, id: &str) -> Vec<String> {
        d.node(id).unwrap().inputs.clone()
    }

    /// A Camera rig goes into a wire, after a selected node, or on its own,
    /// in one undo step.
    #[test]
    fn camera_rigs_wire_themselves_in() {
        let mut d = Doc::new(Graph::default(), None);
        let plate = d.add_node("checkerboard", [0.0, 0.0]);
        let grade = d.add_node("grade", [0.0, 100.0]);
        let viewer = d.add_node("viewer", [0.0, 200.0]);
        d.connect(&plate, &grade, 0);
        d.connect(&grade, &viewer, 0);
        d.commit(false);
        let before = d.graph.clone();
        // Into the plate → grade wire.
        let (cam, scene, render) = d.add_camera_rig([0.0, 50.0], Some((&plate, &grade, 0)), None);
        d.commit(false);
        assert_eq!((inputs(&d, &scene)[0].as_str(), inputs(&d, &render)[0].as_str()), (cam.as_str(), scene.as_str()));
        assert_eq!((inputs(&d, &render)[2].as_str(), inputs(&d, &grade)[0].as_str()), (plate.as_str(), render.as_str()));
        let (pc, pr) = (d.node(&cam).unwrap().pos.unwrap(), d.node(&render).unwrap().pos.unwrap());
        assert!(pc[0] < pr[0] && pc[1] < pr[1], "the camera above and to the left");
        let t = d.param(&cam, "translate");
        assert_eq!((t[0].as_f64(), t[1].as_f64()), (Some(0.0), Some(0.0)));
        assert!((t[2].as_f64().unwrap() - 35.0 / 36.0).abs() < 1e-12, "back on +Z till a new Card fills the frame: {t}");
        d.undo();
        assert_eq!(d.graph, before, "one undo step");
        // After the selected grade: it feeds the background, the viewer
        // now watches the render.
        let (_, _, render) = d.add_camera_rig([0.0, 150.0], None, Some(&grade));
        assert_eq!((inputs(&d, &render)[2].as_str(), inputs(&d, &viewer)[0].as_str()), (grade.as_str(), render.as_str()));
        assert_eq!(inputs(&d, &grade)[0], plate, "upstream untouched");
        d.commit(false);
        d.undo();
        assert_eq!(d.graph, before);
        // A selected Card joins the Scene; alone, the rig is just itself.
        let card = d.add_node("card", [300.0, 0.0]);
        let (_, scene, render) = d.add_camera_rig([300.0, 200.0], None, Some(&card));
        assert_eq!((inputs(&d, &scene)[1].as_str(), inputs(&d, &render)[2].as_str()), (card.as_str(), ""));
        let n = d.graph.nodes.len();
        let (_, _, render) = d.add_camera_rig([600.0, 200.0], None, None);
        assert_eq!((d.graph.nodes.len(), inputs(&d, &render)[2].as_str()), (n + 3, ""));
        // A scene wire isn't one to put a render in: the rig stands apart.
        let (_, scene2, _) = d.add_camera_rig([900.0, 200.0], Some((&card, &scene, 1)), None);
        assert_eq!(inputs(&d, &scene)[1], card);
        assert_eq!(inputs(&d, &scene2)[1], "");
    }

    #[test]
    fn a_3d_camera_setup_goes_into_a_selected_wire_in_a_backdrop() {
        let mut d = Doc::new(Graph::default(), None);
        let plate = d.add_node("checkerboard", [0.0, 0.0]);
        let grade = d.add_node("grade", [0.0, 100.0]);
        let viewer = d.add_node("viewer", [0.0, 200.0]);
        d.connect(&plate, &grade, 0);
        d.connect(&grade, &viewer, 0);
        d.commit(false);
        let before = d.graph.clone();
        // A selected wire: picked, it deselects the nodes; the setup goes
        // into it.
        d.select_only(&grade);
        d.select_wire(&plate, &grade, 0);
        assert!(d.selection().is_empty() && d.selected_wire.is_some());
        let (cam, scene, render) = d.add_camera_setup([500.0, 500.0]);
        d.commit(false);
        assert_eq!((inputs(&d, &render)[2].as_str(), inputs(&d, &grade)[0].as_str(), inputs(&d, &render)[0].as_str()), (plate.as_str(), render.as_str(), scene.as_str()));
        assert_eq!((d.selected.as_deref(), d.selected_wire.as_ref()), (Some(cam.as_str()), None), "the Camera selected");
        // A column beside the wire, in a "3D Setup" backdrop holding just
        // the three.
        let pos = |d: &Doc, id: &str| d.node(id).unwrap().pos.unwrap();
        assert!(pos(&d, &cam)[0] == pos(&d, &render)[0] && pos(&d, &cam)[1] < pos(&d, &scene)[1] && pos(&d, &scene)[1] < pos(&d, &render)[1]);
        let b = d.graph.backdrops.iter().find(|b| b.title == "3D Setup").unwrap().clone();
        let inside = |id: &str| {
            let (a, c) = Doc::node_box(d.node(id).unwrap());
            b.contains(a, c)
        };
        assert!(inside(&cam) && inside(&scene) && inside(&render));
        assert!(![&plate, &grade, &viewer].iter().any(|n| {
            let (a, c) = Doc::node_box(d.node(n).unwrap());
            a[0] < b.pos[0] + b.size[0] && c[0] > b.pos[0] && a[1] < b.pos[1] + b.size[1] && c[1] > b.pos[1]
        }), "clear of the other nodes");
        // The first in the project asks to be shown in 3D.
        assert_eq!(d.show_3d.take(), Some(render.clone()));
        d.undo();
        assert_eq!(d.graph, before, "one undo step");
        // A selected wire is deleted by Delete, like a node; selecting a
        // node drops it.
        d.select_wire(&plate, &grade, 0);
        assert!(d.delete_selection());
        assert_eq!((inputs(&d, &grade)[0].as_str(), d.selected_wire.as_ref()), ("", None));
        d.select_wire(&grade, &render, 2);
        d.select_only(&plate);
        assert!(d.selected_wire.is_none());
    }

    /// Without a wire, a camera setup goes just before the Viewer (moved
    /// to just below the render), or makes one: each one undo step.
    #[test]
    fn a_3d_camera_setup_goes_before_the_viewer_or_brings_one() {
        let pos = |d: &Doc, id: &str| d.node(id).unwrap().pos.unwrap();
        let viewers = |d: &Doc| d.graph.nodes.iter().filter(|n| n.op == "viewer").map(|n| n.id.clone()).collect::<Vec<_>>();
        // The Viewer last, under the render, outside the backdrop.
        let last = |d: &Doc, render: &str, viewer: &str| {
            assert_eq!(inputs(d, viewer)[0], render);
            assert!(pos(d, viewer)[0] == pos(d, render)[0] && pos(d, viewer)[1] > pos(d, render)[1], "{:?} {:?}", pos(d, viewer), pos(d, render));
            let b = d.graph.backdrops.iter().find(|b| b.title == "3D Setup").unwrap();
            let (a, c) = Doc::node_box(d.node(viewer).unwrap());
            assert!(!(a[0] < b.pos[0] + b.size[0] && c[0] > b.pos[0] && a[1] < b.pos[1] + b.size[1] && c[1] > b.pos[1]), "outside the backdrop");
        };
        let chain = |viewer: bool| {
            let mut d = Doc::new(Graph::default(), None);
            let plate = d.add_node("checkerboard", [0.0, 0.0]);
            let grade = d.add_node("grade", [0.0, 100.0]);
            d.connect(&plate, &grade, 0);
            let v = viewer.then(|| d.add_node("viewer", [0.0, 200.0]));
            if let Some(v) = &v {
                d.connect(&grade, v, 0);
            }
            d.commit(false);
            (d, plate, grade, v)
        };
        let undone = |mut d: Doc, before: Graph| {
            d.commit(false);
            d.undo();
            assert_eq!(d.graph, before, "one undo step");
        };
        // Into the Viewer's wire, whatever is selected: what it showed is the
        // background.
        let (mut d, plate, grade, v) = chain(true);
        let (v, before) = (v.unwrap(), d.graph.clone());
        d.select_only(&plate);
        let (cam, scene, render) = d.add_camera_setup([900.0, 900.0]);
        assert_eq!((inputs(&d, &render)[0].as_str(), inputs(&d, &render)[2].as_str()), (scene.as_str(), grade.as_str()));
        assert_eq!((viewers(&d), d.selected.clone()), (vec![v.clone()], Some(cam)));
        last(&d, &render, &v);
        assert_eq!(d.show_3d, Some(render.clone()), "the first setup shows in 3D");
        undone(d, before);
        // A Viewer showing a scene: that goes into the Scene.
        let (mut d, ..) = chain(false);
        let card = d.add_node("card", [300.0, 0.0]);
        let v = d.add_node("viewer", [300.0, 100.0]);
        d.connect(&card, &v, 0);
        let (_, scene, render) = d.add_camera_setup([900.0, 900.0]);
        assert_eq!((inputs(&d, &scene)[1].as_str(), inputs(&d, &render)[2].as_str()), (card.as_str(), ""));
        last(&d, &render, &v);
        // An unconnected Viewer: the selected node, else the chain's end,
        // is the background.
        for (select, bg) in [(true, 0), (false, 1)] {
            let (mut d, plate, grade, _) = chain(false);
            let v = d.add_node("viewer", [400.0, 400.0]);
            d.commit(false);
            let before = d.graph.clone();
            if select {
                d.select_only(&plate);
            }
            let (_, _, render) = d.add_camera_setup([900.0, 900.0]);
            assert_eq!(&inputs(&d, &render)[2], [&plate, &grade][bg]);
            last(&d, &render, &v);
            undone(d, before);
        }
        // No Viewer: one is made, the chain's end (not a Write after it)
        // the background.
        let (mut d, _, grade, _) = chain(false);
        let write = d.add_node("write", [0.0, 200.0]);
        d.connect(&grade, &write, 0);
        d.commit(false);
        let before = d.graph.clone();
        let (_, _, render) = d.add_camera_setup([900.0, 900.0]);
        assert_eq!((inputs(&d, &render)[2].as_str(), inputs(&d, &write)[0].as_str()), (grade.as_str(), grade.as_str()));
        let made = viewers(&d);
        assert_eq!(made.len(), 1);
        last(&d, &render, &made[0]);
        undone(d, before);
        // An empty project: on its own at the given spot, with a Viewer.
        let mut d = Doc::new(Graph::default(), None);
        let (cam, _, render) = d.add_camera_setup([800.0, 300.0]);
        assert_eq!((inputs(&d, &render)[2].as_str(), pos(&d, &render)), ("", [800.0, 300.0]));
        assert_eq!(pos(&d, &cam)[1], 300.0 - 2.0 * (NODE_H + 40.0));
        last(&d, &render, &viewers(&d)[0]);
    }

    #[test]
    fn linked_params_show_live_values_and_survive_renames() {
        let mut d = Doc::new(Graph::default(), None);
        let a = d.add_node("rectangle", [0.0, 0.0]);
        let b = d.add_node("rectangle", [200.0, 0.0]);
        d.set_param(&a, "center", json!({ "keys": [[1, [0, 0]], [11, [100, 50]]], "interp": "linear" }));
        d.set_param(&b, "center", arclight::link::param(&a, "center"));
        assert_eq!(d.param_at(&b, "center", 6), json!([50.0, 25.0]));
        // A drag (or typing) doesn't overwrite a link.
        d.edit_param(&b, "center", 6, json!([1, 2]));
        assert!(arclight::link::is_link(&d.param(&b, "center")));
        // Renaming the source keeps the link pointing at it.
        d.rename(&a, "src").unwrap();
        assert_eq!(arclight::link::source(&d.param(&b, "center")), Some("src"));
        assert_eq!(d.param_at(&b, "center", 11), json!([100.0, 50.0]));
        // Unlink (what the panel does): the value stays, plain.
        let now = d.param_at(&b, "center", 11);
        d.set_param(&b, "center", now);
        assert_eq!(d.param(&b, "center"), json!([100.0, 50.0]));
        // A formula: live, renamed with its source, and its errors told.
        d.set_param(&b, "center", arclight::link::expr("src.center + [wiggle(0.2, 0), 1]"));
        assert_eq!(d.param_at(&b, "center", 6), json!([50.0, 26.0]));
        d.rename("src", "a2").unwrap();
        assert_eq!(arclight::link::formula(&d.param(&b, "center")), Some("a2.center + [wiggle(0.2, 0), 1]"));
        assert_eq!(d.param_error(&b, "center", 6), None);
        d.set_param(&b, "center", arclight::link::expr("7"));
        assert_eq!(d.param_at(&b, "center", 6), json!([7.0, 7.0]), "one number fills both");
        d.set_param(&b, "center", arclight::link::expr("a2.center + [1, 2, 3]"));
        assert_eq!(d.param_error(&b, "center", 6), Some("lists of 2 and 3 components".into()));
    }

    #[test]
    fn new_shapes_land_in_the_viewed_picture() {
        // A 960×540 picture in a 1920×1080 project: shapes centered on it.
        let mut d = Doc::new(Graph::default(), None);
        d.view_format = Some(arclight::image::Size { width: 960, height: 540 });
        let r = d.add_node("rectangle", [0.0, 0.0]);
        assert_eq!(d.param(&r, "center"), json!([480.0, 270.0]));
        let p = d.add_node("polygon", [0.0, 0.0]);
        let pts: Vec<f64> = d.param(&p, "points").as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert!(pts.chunks(2).all(|c| c[0] > 0.0 && c[0] < 960.0 && c[1] > 0.0 && c[1] < 540.0), "{pts:?}");
        // Nothing viewed: the project format.
        d.view_format = None;
        let r = d.add_node("rectangle", [0.0, 0.0]);
        assert_eq!(d.param(&r, "center"), json!([960.0, 540.0]));
    }

    #[test]
    fn viewing_a_node_overrides_the_viewer_until_the_viewer_is_viewed() {
        let mut d = Doc::new(Graph::default(), None);
        let plate = d.add_node("constant", [0.0, 0.0]);
        // No Viewer yet: one is made and wired up.
        d.view_node(&plate);
        let viewer = d.graph.nodes.iter().find(|n| n.op == "viewer").unwrap().id.clone();
        assert_eq!((d.view_target(), d.viewed()), (Some(viewer.clone()), Some(plate.clone())));
        // Another node shows in its place, without rewiring the Viewer…
        let grade = d.add_node("grade", [200.0, 0.0]);
        let rev = d.revision;
        d.view_node(&grade);
        assert_eq!((d.view_target(), d.viewed()), (Some(grade.clone()), Some(grade.clone())));
        assert_eq!((d.node(&viewer).unwrap().inputs[0].as_str(), d.revision), (plate.as_str(), rev));
        // …follows a rename, ends when the node goes…
        d.rename(&grade, "look").unwrap();
        assert_eq!(d.view_target().as_deref(), Some("look"));
        d.delete_node("look");
        assert_eq!(d.view_target(), Some(viewer.clone()));
        // …or when the Viewer itself is viewed.
        d.view_node(&plate);
        assert_eq!(d.view_target(), Some(plate.clone()));
        d.view_node(&viewer);
        assert_eq!((d.view_target(), d.viewed()), (Some(viewer), Some(plate)));
    }

    /// A project that already has a Viewer, and a new comp wired into a
    /// second one: the viewer shows the Viewer last wired into, viewed or
    /// clicked, not just the first in the file (which left the new comp
    /// unseen, the old picture on screen whatever was done to it).
    #[test]
    fn the_viewer_shows_the_viewer_node_last_wired_viewed_or_clicked() {
        let mut d = Doc::new(Graph::default(), None);
        let old = d.add_node("constant", [0.0, 0.0]);
        d.view_node(&old);
        let first = d.viewer().unwrap();
        assert_eq!(d.viewed(), Some(old.clone()));
        let (plate, logo) = (d.add_node("read", [0.0, 200.0]), d.add_node("read", [0.0, 300.0]));
        let merge = d.add_node("merge", [200.0, 250.0]);
        d.connect(&plate, &merge, 0);
        d.connect(&logo, &merge, 1);
        // Loaded this way, the first Viewer is shown, as before.
        let second = d.add_node("viewer", [400.0, 250.0]);
        assert_eq!(d.view_target(), Some(first.clone()));
        // Wiring the comp into the new Viewer shows it…
        d.connect(&merge, &second, 0);
        assert_eq!((d.view_target(), d.viewed()), (Some(second.clone()), Some(merge.clone())));
        // …as does viewing it again after viewing the old one; the rest
        // goes into it too (route_to_viewer, a node viewed in its place).
        d.view_node(&first);
        assert_eq!(d.viewed(), Some(old.clone()));
        d.view_node(&second);
        assert_eq!(d.viewed(), Some(merge.clone()));
        d.route_to_viewer(&logo);
        assert_eq!((d.node(&second).unwrap().inputs[0].as_str(), d.node(&first).unwrap().inputs[0].as_str()), (logo.as_str(), old.as_str()));
        d.view_node(&plate);
        assert_eq!(d.view_target(), Some(plate.clone()));
        d.view_node(&second);
        // It follows a rename, and when it goes the first one shows again.
        d.rename(&second, "comp_view").unwrap();
        assert_eq!(d.view_target().as_deref(), Some("comp_view"));
        d.delete_node("comp_view");
        assert_eq!((d.view_target(), d.viewed()), (Some(first), Some(old)));
    }

    #[test]
    fn scripted_nodes_are_placed_clear_of_existing_ones() {
        let mut d = Doc::new(Graph::default(), None);
        let a = d.add_node("constant", [0.0, 0.0]);
        d.add_node("constant", [NODE_W + 50.0, 0.0]);
        let new = |id: &str, op: &str, inputs: Vec<String>| NodeSpec { id: id.into(), op: op.into(), inputs, params: json!({}), pos: None, disabled: false };
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
    fn undo_keeps_as_many_steps_as_the_preferences_say() {
        let steps = |levels: usize| {
            let mut d = Doc::new(Graph::default(), None);
            d.undo_levels = levels;
            for i in 0..250 {
                d.add_node("grade", [i as f32 * 200.0, 0.0]);
                d.commit(false);
            }
            let mut n = 0;
            while d.history.undo.pop().is_some() {
                n += 1;
            }
            n
        };
        assert_eq!((steps(UNDO_LEVELS), steps(3), steps(0)), (200, 3, 250), "200 as before, 3, or all of them");
    }

    #[test]
    fn a_blank_project_takes_the_new_project_defaults() {
        let mut project = arclight::graph::Project::default();
        (project.first_frame, project.last_frame, project.fps, project.format) = (1001, 1100, 25.0, arclight::image::Size { width: 1280, height: 720 });
        let d = Doc::blank(project.clone());
        assert_eq!((d.graph.project.clone(), d.graph.nodes.len(), d.path.clone(), d.dirty, d.can_undo()), (project, 0, None, false, false));
        assert_eq!(Doc::blank(Default::default()).graph, Doc::new(Graph::default(), None).graph, "the defaults as today");
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
