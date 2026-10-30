//! A headless editing session: a graph plus the edits scripting and
//! automation make to it, with validation and undo/redo.
//!
//! The MCP server, the Python `arclt` module and the GUI's app bridge all
//! drive graphs through [`Session`], so a script behaves the same whichever
//! front end runs it.

use crate::anim;
use crate::camtrack;
use crate::graph::{Evaluator, Graph, NodeSpec};
use crate::image::{Image, Size};
use crate::ops::{self, EvalContext, OpInfo, ParamKind};
use crate::render::{self, Progress};
use crate::track;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const UNDO_LIMIT: usize = 200;
/// The most particles one [`Session::particles`] call returns (page
/// through more with its offset).
pub const MAX_PARTICLES: usize = 100_000;
/// The most splats one [`Session::splats`] page holds.
pub const MAX_SPLATS: usize = 100_000;
/// About how long [`Session::particles`] simulates at a time.
const PARTICLE_RUN: std::time::Duration = std::time::Duration::from_millis(30);

pub struct Session {
    pub graph: Graph,
    /// Where the graph was loaded from or last saved to.
    pub path: Option<PathBuf>,
    /// Edited since the last load or save.
    pub dirty: bool,
    /// Its own, or one shared with others (see [`Session::sharing`]).
    eval: Arc<Mutex<Evaluator>>,
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
        Session::sharing(graph, Arc::default())
    }

    /// A session over `graph` that evaluates with `eval`, sharing its
    /// caches (and particle simulations) with whatever else uses it: the
    /// GUI's viewer, for calls over its bridge.
    pub fn sharing(graph: Graph, eval: Arc<Mutex<Evaluator>>) -> Session {
        let mut graph = graph;
        normalize(&mut graph);
        Session { graph, path: None, dirty: false, eval, undo: Vec::new(), redo: Vec::new() }
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
        // Files in the project's folder relative to it (see Graph::save).
        self.graph.save(&path)?;
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
                json!({"port": i, "label": p.label, "optional": p.optional, "kind": p.kind.name(), "source": src})
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
        // A file given relative: in the project's folder.
        if let Some(values) = values.as_object_mut() {
            self.graph.project.resolve_paths(self.path.as_deref(), op, values);
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

    /// Track and solve Camera Tracker `id` over the project's frame range
    /// (frames of its input), keeping the solution on the node (one undo
    /// step). `mode` ("auto", "free move" or "free", "tripod") and
    /// `distance`, when given, are set on the node too. Returns a summary.
    pub fn track_camera(&mut self, id: &str, mode: Option<&str>, distance: Option<f64>, progress: &Progress) -> Result<Value> {
        let n = &self.graph.nodes[self.node_index(id)?];
        if n.op != "camera_tracker" {
            bail!("'{id}' isn't a Camera Tracker");
        }
        let mut set = Vec::new();
        if let Some(m) = mode {
            let m = camtrack::Mode::parse(m).ok_or_else(|| anyhow!("mode '{m}' isn't one of auto, free move (or free) and tripod"))?;
            set.push(("solve_mode", json!(m.name())));
        }
        if let Some(d) = distance {
            if d.is_nan() || d <= 0.0 {
                bail!("the distance needs to be above 0");
            }
            set.push(("distance", json!(d)));
        }
        let mut p = ops::params_with_defaults("camera_tracker", &n.params);
        for (k, v) in &set {
            p[*k] = v.clone();
        }
        let settings = camtrack::Settings::from_params(&p, self.graph.project.first_frame, self.graph.project.last_frame);
        let mut frames = camtrack::input_frames(&self.graph, id)?;
        let sol = camtrack::run(&mut frames, &settings, progress)?;
        let summary = solution_summary(&sol, settings.first, settings.last);
        self.checkpoint();
        let i = self.node_index(id)?;
        for (k, v) in set {
            self.graph.nodes[i].params[k] = v;
        }
        self.graph.nodes[i].params["solution"] = sol.to_value();
        Ok(summary)
    }

    /// Camera Tracker `id`'s solution.
    pub fn camera_solution(&self, id: &str) -> Result<camtrack::Solution> {
        let n = &self.graph.nodes[self.node_index(id)?];
        camtrack::Solution::from_params(&n.params).ok_or_else(|| anyhow!("'{id}' has no solve yet: track it first"))
    }

    /// Move Camera Tracker `id`'s solve: point `origin` to the origin, the
    /// plane through points `ground` level at height 0, and points
    /// `scale.0` and `scale.1` `scale.2` apart (in that order; any may be
    /// left out). One undo step.
    pub fn orient_camera(&mut self, id: &str, origin: Option<usize>, ground: Option<&[usize]>, scale: Option<(usize, usize, f64)>) -> Result<()> {
        let mut sol = self.camera_solution(id)?;
        if let Some(g) = ground {
            sol.set_ground(g)?;
        }
        if let Some(o) = origin {
            sol.set_origin(o)?;
        }
        if let Some((a, b, d)) = scale {
            sol.set_scale(a, b, d)?;
        }
        self.checkpoint();
        let i = self.node_index(id)?;
        self.graph.nodes[i].params["solution"] = sol.to_value();
        Ok(())
    }

    /// From Camera Tracker `id`'s solve: a Camera keyed on every solved
    /// frame, a Point Cloud of the points, a Scene of the two and a Scanline
    /// Render of it over the tracker's plate. Their names.
    pub fn export_camera(&mut self, id: &str) -> Result<Value> {
        self.export_camera_linked(id, false)
    }

    /// [`Session::export_camera`], the Camera (`linked`) following the
    /// solve live instead, so re-solving or reorienting moves it too.
    pub fn export_camera_linked(&mut self, id: &str, linked: bool) -> Result<Value> {
        let sol = self.camera_solution(id)?;
        let params = if linked { camtrack::camera_links(id) } else { camtrack::camera_params(&sol) };
        let camera = self.create_node("camera", None, &params, &[])?;
        let mut cloud = Map::new();
        cloud.insert("points".into(), json!(sol.points.iter().flatten().collect::<Vec<_>>()));
        let points = self.create_node("point_cloud", None, &cloud, &[])?;
        let scene = self.create_node("scene", None, &Map::new(), &[camera.clone(), points.clone()])?;
        let render = self.create_node("scanline_render", None, &Map::new(), &[scene.clone(), String::new(), id.to_string()])?;
        Ok(json!({ "camera": camera, "points": points, "scene": scene, "render": render }))
    }

    /// Place a node on points `ids` of Camera Tracker `id`'s solve: `what`
    /// is "card" (on their plane, facing the camera of `frame`, sized to
    /// them, showing `input` if given) or "locator" (at their middle).
    /// Returns its name.
    pub fn place_on_points(&mut self, id: &str, ids: &[usize], what: &str, frame: i64, input: Option<&str>, name: Option<&str>) -> Result<String> {
        let sol = self.camera_solution(id)?;
        match what {
            "card" => {
                let params = camtrack::card_on_points(&sol, ids, frame)?;
                self.create_node("card", name, &params, &[input.unwrap_or_default().to_string()])
            }
            "locator" => {
                let c = sol.centroid(ids).ok_or_else(|| anyhow!("pick some solved points"))?;
                let mut params = Map::new();
                params.insert("translate".into(), json!([c.x, c.y, c.z]));
                self.create_node("locator", name, &params, &[])
            }
            other => bail!("unknown placement '{other}' (card or locator)"),
        }
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
        let mut checked = params
            .iter()
            .map(|(k, v)| Ok((k.clone(), validate_param(info, k, v)?)))
            .collect::<Result<Map<String, Value>>>()?;
        // A file given relative: in the project's folder.
        self.graph.project.resolve_paths(self.path.as_deref(), &self.graph.nodes[i].op.clone(), &mut checked);
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
        let from_op = &self.graph.nodes[self.node_index(from)?].op;
        if let Some(e) = ops::wire_error(from_op, &self.graph.nodes[i].op, port) {
            bail!("can't connect '{from}' to '{to}': {e}");
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

    /// Give the project a folder (None: none, so the folder its file is
    /// in counts). One given relative is relative to where the project's
    /// file is (else the working directory). Undoable.
    pub fn set_folder(&mut self, folder: Option<&Path>) -> Result<()> {
        let base = self.path.as_deref().and_then(|f| std::path::absolute(f).ok()?.parent().map(Path::to_path_buf));
        let folder = match folder.filter(|f| !f.as_os_str().is_empty()) {
            Some(f) if f.is_relative() => Some(std::path::absolute(base.map_or_else(|| f.to_path_buf(), |b| b.join(f)))?),
            f => f.map(Path::to_path_buf),
        };
        self.checkpoint();
        self.graph.project.folder = folder;
        Ok(())
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
        let mut eval = self.eval.lock().unwrap_or_else(|e| e.into_inner());
        Ok(eval.evaluate(&self.graph, id, &ctx)?)
    }

    /// What node `id`'s 3D scene holds at `frame`: its cameras, cards,
    /// meshes and lights (each with the node it came from and where it is in
    /// the scene; lights also their type, direction, color and settings),
    /// splat captures (node, count, bounds, SH degree, where), and how many
    /// points. Err if the node outputs an image, not a scene.
    pub fn scene_objects(&mut self, id: &str, frame: i64) -> Result<Value> {
        use crate::shade::LightKind;
        let img = self.render(id, frame)?;
        let scene = img.scene.as_ref().ok_or_else(|| anyhow!("'{id}' outputs an image, not a 3D scene"))?;
        let origin = |m: &crate::geom::Mat4| -> [f64; 3] { crate::geom::transform_point(m, &crate::geom::Vec3::zeros()).into() };
        let card_shading = |c: &crate::scene::Card| match c.material.as_ref().map(|m| m.shading) {
            Some(crate::mesh::Shading::Lambert) => "lambert",
            Some(crate::mesh::Shading::Blinn) => "blinn",
            _ => "unlit",
        };
        let facing = |m: &crate::geom::Mat4| -> [f64; 3] { (m.fixed_view::<3, 3>(0, 0) * -crate::geom::Vec3::z()).try_normalize(1e-12).unwrap_or_else(|| -crate::geom::Vec3::z()).into() };
        let cameras: Vec<Value> = scene.cameras.iter().enumerate().map(|(i, c)| json!({"node": scene.camera_node(i), "position": origin(&c.world), "direction": facing(&c.world), "focal": c.focal})).collect();
        let cards: Vec<Value> = scene.cards.iter().map(|c| json!({"node": c.node, "position": origin(&c.world), "size": c.size, "shading": card_shading(c), "casts_shadows": c.casts_shadows, "receives_shadows": c.receives_shadows})).collect();
        let meshes: Vec<Value> = scene
            .meshes
            .iter()
            .map(|m| {
                let bounds = m.bounds().map(|(lo, hi)| [<[f64; 3]>::from(lo), hi.into()]);
                json!({"node": m.node, "name": m.name, "position": origin(&m.world), "bounds": bounds, "holdout": m.holdout, "casts_shadows": m.casts_shadows, "receives_shadows": m.receives_shadows})
            })
            .collect();
        let lights: Vec<Value> = scene
            .lights
            .iter()
            .map(|l| {
                let mut v = json!({
                    "node": l.node, "position": <[f64; 3]>::from(l.position()), "direction": <[f64; 3]>::from(l.direction()),
                    "color": l.color, "intensity": l.intensity, "decay": l.decay, "enabled": l.enabled, "shadows": l.shadows,
                });
                v["type"] = json!(match l.kind {
                    LightKind::Ambient => "ambient",
                    LightKind::Directional => "directional",
                    LightKind::Point => "point",
                    LightKind::Spot { cone, penumbra, falloff } => {
                        v["cone"] = json!(cone);
                        v["penumbra"] = json!(penumbra);
                        v["falloff"] = json!(falloff);
                        "spot"
                    }
                    LightKind::Area { size, disk, two_sided, samples } => {
                        v["size"] = json!(size);
                        v["shape"] = json!(if disk { "disk" } else { "rectangle" });
                        v["two_sided"] = json!(two_sided);
                        v["quality"] = json!(samples);
                        "area"
                    }
                });
                if l.radius > 0.0 {
                    v["radius"] = json!(l.radius);
                }
                v
            })
            .collect();
        let splats: Vec<Value> = scene
            .splats
            .iter()
            .map(|g| {
                let bounds = g.bounds().map(|(lo, hi)| [<[f64; 3]>::from(lo), hi.into()]);
                json!({"node": g.node, "count": g.len(), "bounds": bounds, "sh_degree": g.sh_degree, "position": origin(&g.world)})
            })
            .collect();
        Ok(json!({"node": id, "frame": frame, "cameras": cameras, "cards": cards, "meshes": meshes, "lights": lights, "splats": splats, "points": scene.points.len()}))
    }

    /// The particles node `id` passes on at `frame` (an emitter's,
    /// a modifier's, a Particle Render's, or a Scene's or Scanline Render's
    /// gathered streams), simulated as needed: from the `offset`th, at most
    /// `limit` of them (capped at [`MAX_PARTICLES`]), as columns: ids,
    /// positions, velocities (units per second), ages and lives (frames),
    /// sizes, colors (premultiplied linear RGBA), and which stream each
    /// came from. `total` counts them all; `next_offset` is where the next
    /// page starts, if there is one. Err if the node passes on no particles.
    pub fn particles(&mut self, id: &str, frame: i64, offset: usize, limit: usize) -> Result<Value> {
        let img = self.render(id, frame)?;
        let streams = img.scene.as_ref().map(|s| s.particles.clone()).unwrap_or_default();
        if streams.is_empty() {
            bail!("'{id}' passes on no particles (connect it after a Particle Emitter)");
        }
        // Simulated up to `frame` a short run at a time (as many frames as
        // take about PARTICLE_RUN), so a viewer sharing the simulation (the
        // GUI's, over its bridge) waits for one run at most, not the whole way.
        for st in &streams {
            let (mut g, mut run) = (self.graph.project.first_frame, 1);
            while g + 1 < frame {
                g = (g + run).min(frame - 1);
                let t = std::time::Instant::now();
                st.particles_at(g);
                let took = t.elapsed();
                run = if took > PARTICLE_RUN { (run / 2).max(1) } else if took < PARTICLE_RUN / 4 { run * 2 } else { run };
                // (Long enough for a waiting viewer to take its turn.)
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
        }
        let all: Vec<(usize, crate::particles::Particle)> = streams.iter().enumerate().flat_map(|(k, st)| st.particles().into_iter().map(move |p| (k, p))).collect();
        let total = all.len();
        let page = &all[offset.min(total)..(offset.saturating_add(limit.min(MAX_PARTICLES))).min(total)];
        let v3 = |v: &crate::geom::Vec3| [v.x, v.y, v.z];
        let end = offset.min(total) + page.len();
        Ok(json!({
            "node": id,
            "frame": frame,
            "total": total,
            "offset": offset.min(total),
            "count": page.len(),
            "next_offset": (end < total).then_some(end),
            "streams": streams.iter().map(|st| st.node.clone()).collect::<Vec<_>>(),
            "ids": page.iter().map(|(_, p)| p.id).collect::<Vec<_>>(),
            "positions": page.iter().map(|(_, p)| v3(&p.position)).collect::<Vec<_>>(),
            "velocities": page.iter().map(|(_, p)| v3(&p.velocity)).collect::<Vec<_>>(),
            "ages": page.iter().map(|(_, p)| p.age).collect::<Vec<_>>(),
            "lives": page.iter().map(|(_, p)| p.life).collect::<Vec<_>>(),
            "sizes": page.iter().map(|(_, p)| p.size).collect::<Vec<_>>(),
            "colors": page.iter().map(|(_, p)| p.color).collect::<Vec<_>>(),
            "stream": page.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        }))
    }

    /// The Gaussian splats node `id`'s scene holds at `frame` (a Read
    /// Splat's, or the captures a Scene or Scanline Render gathers): from
    /// the `offset`th, at most `limit` (capped at [`MAX_SPLATS`]), as
    /// columns: positions (in the scene, placed), scales (1σ, linear) and
    /// rotations ([w, x, y, z]) in the capture's own space, opacities
    /// (0–1), colors (the view-independent part, linear RGB) and which set
    /// each came from (an index into `sets`: node, count, SH degree and
    /// placement as a 4×4 row-major matrix). `total` counts them all;
    /// `next_offset` is where the next page starts, if there is one. Err if
    /// the scene holds none.
    pub fn splats(&mut self, id: &str, frame: i64, offset: usize, limit: usize) -> Result<Value> {
        let img = self.render(id, frame)?;
        let sets = img.scene.as_ref().map(|s| s.splats.clone()).unwrap_or_default();
        if sets.iter().all(|g| g.is_empty()) {
            bail!("'{id}' holds no splats (read a capture with Read Splat)");
        }
        let total: usize = sets.iter().map(|g| g.len()).sum();
        let start = offset.min(total);
        let end = start.saturating_add(limit.min(MAX_SPLATS)).min(total);
        // (set, index in it) for each splat of the page.
        let mut page = Vec::with_capacity(end - start);
        let mut first = 0;
        for (k, g) in sets.iter().enumerate() {
            let (lo, hi) = (start.max(first), end.min(first + g.len()));
            page.extend((lo..hi.max(lo)).map(|i| (k, i - first)));
            first += g.len();
        }
        let placed = |k: usize, i: usize| -> [f64; 3] {
            let p = sets[k].positions[i];
            crate::geom::transform_point(&sets[k].world, &crate::geom::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).into()
        };
        let matrix = |m: &crate::geom::Mat4| -> Vec<f64> { (0..4).flat_map(|r| (0..4).map(move |c| m[(r, c)])).collect() };
        Ok(json!({
            "node": id,
            "frame": frame,
            "total": total,
            "offset": start,
            "count": page.len(),
            "next_offset": (end < total).then_some(end),
            "sets": sets.iter().map(|g| json!({"node": g.node, "count": g.len(), "sh_degree": g.sh_degree, "world": matrix(&g.world)})).collect::<Vec<_>>(),
            "positions": page.iter().map(|&(k, i)| placed(k, i)).collect::<Vec<_>>(),
            "scales": page.iter().map(|&(k, i)| sets[k].scales[i]).collect::<Vec<_>>(),
            "rotations": page.iter().map(|&(k, i)| sets[k].rotations[i]).collect::<Vec<_>>(),
            "opacities": page.iter().map(|&(k, i)| sets[k].opacity[i]).collect::<Vec<_>>(),
            "colors": page.iter().map(|&(k, i)| crate::splat::sh_color(sets[k].sh_of(i), 0, [0.0, 0.0, 1.0])).collect::<Vec<_>>(),
            "set": page.iter().map(|&(k, _)| k).collect::<Vec<_>>(),
        }))
    }

    /// Render node `id` at `frame` and write it to `path` (EXR keeps float;
    /// other formats are 8-bit sRGB).
    /// Render node `id` at `frame` into the file at `path` (given relative:
    /// in the project's folder).
    pub fn render_to_file(&mut self, id: &str, frame: i64, path: &Path) -> Result<Arc<Image>> {
        let path = PathBuf::from(self.graph.project.resolve_path(self.path.as_deref(), &path.to_string_lossy()));
        let img = self.render(id, frame)?;
        img.write(&path)?;
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
        if let Some(f) = value["link"].get("expr") {
            let f = f.as_str().ok_or_else(|| anyhow!("'{name}': a formula must be text"))?;
            crate::expr::parse(f).map_err(|e| anyhow!("'{name}': formula: {e}"))?;
        } else if crate::link::source(value).is_none() {
            bail!("'{name}': a link needs a \"node\" (or an \"expr\")");
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
        ParamKind::Text => json!(value.as_str().ok_or_else(|| anyhow!("'{name}' needs text"))?),
        ParamKind::Flags(names) => {
            let arr = value.as_array().ok_or_else(|| anyhow!("'{name}' needs a list of: {}", names.join(", ")))?;
            let mut chosen = Vec::new();
            for v in arr {
                let s = v.as_str().filter(|s| names.contains(s)).ok_or_else(|| anyhow!("'{v}' is not one of '{name}' (any of: {})", names.join(", ")))?;
                if !chosen.contains(&s) {
                    chosen.push(s);
                }
            }
            json!(chosen)
        }
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
                ParamKind::Text => ("text", json!({})),
                ParamKind::Flags(names) => ("flags", json!({"choices": names, "note": "a list of any of the choices"})),
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
    let inputs: Vec<Value> = info.inputs.iter().map(|p| json!({"label": p.label, "optional": p.optional, "kind": p.kind.name()})).collect();
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

/// What a camera solve came to, for reporting.
pub fn solution_summary(sol: &camtrack::Solution, first: i64, last: i64) -> Value {
    let unsolved: Vec<i64> = (first..=last).filter(|f| !sol.frames.contains(f)).collect();
    let round = |x: f64| (x * 1e4).round() / 1e4;
    json!({
        "frames_solved": sol.frames.len(),
        "unsolved_frames": unsolved,
        "points": sol.points.len(),
        "rms_error_px": round(sol.rms),
        "worst_frame_rms_px": round(sol.frame_rms.iter().copied().fold(0.0, f64::max)),
        "focal_mm": round(sol.focal),
        "mode": if sol.tripod { "tripod" } else { "free move" },
        "pnp_frames": sol.pnp_frames,
    })
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
        "planes": img.plane_stats(),
        // The passes, grouped: [{name: "P", planes: ["P.x", "P.y", "P.z"]}, ...].
        "layers": orig.layers().into_iter().map(|(name, planes)| json!({"name": name, "planes": planes})).collect::<Vec<_>>(),
        "scene": orig.scene.as_ref().map(|s| s.describe()),
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
    fn text_params_and_layers_in_stats() {
        let mut s = Session::default();
        let c = s.create_node("constant", None, &Map::new(), &[]).unwrap();
        let sh = s.create_node("shuffle", None, &Map::new(), std::slice::from_ref(&c)).unwrap();
        assert!(s.set_params(&sh, &map(json!({"in": "N", "green": "N.y"}))).is_ok());
        assert!(s.set_params(&sh, &map(json!({"in": 3}))).unwrap_err().to_string().contains("needs text"));
        let schema = op_schema(ops::info("shuffle").unwrap());
        assert_eq!(schema["params"][0]["kind"], "text");
        let mut img = Image::new(2, 1);
        for p in ["N.y", "depth", "N.x"] {
            *img.plane_mut(p) = vec![0.0; 2];
        }
        assert_eq!(image_stats(&img)["layers"], json!([{"name": "N", "planes": ["N.x", "N.y"]}, {"name": "depth", "planes": ["depth"]}]));
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
    fn formulas_are_checked_when_set_and_follow_renames() {
        let mut s = Session::default();
        let a = s.create_node("constant", None, &map(json!({"width": 4, "height": 4, "color": [0.25, 0.0, 0.0, 1.0]})), &[]).unwrap();
        let b = s.create_node("constant", None, &map(json!({"width": 4, "height": 4})), &[]).unwrap();
        let bad = s.set_params(&b, &map(json!({"color": crate::link::expr("sin(")}))).unwrap_err().to_string();
        assert!(bad.contains("'color': formula:"), "{bad}");
        assert!(s.set_params(&b, &map(json!({"color": {"link": {"expr": 3}}}))).is_err());
        let f = format!("{a}.color * [2, 1, 1, 1] + [0.1 * frame, 0, 0, 0]");
        s.set_params(&b, &map(json!({"color": crate::link::expr(&f)}))).unwrap();
        assert!((s.render(&b, 2).unwrap().get(0, 0)[0] - 0.7).abs() < 1e-6);
        s.rename(&a, "plate-2").unwrap();
        assert_eq!(crate::link::formula(&s.node(&b).unwrap().params["color"]), Some("\"plate-2\".color * [2, 1, 1, 1] + [0.1 * frame, 0, 0, 0]"));
        assert!((s.render(&b, 1).unwrap().get(0, 0)[0] - 0.6).abs() < 1e-6);
        // Deleting the source: the render names the param and the node.
        s.delete_node("plate-2").unwrap();
        let e = s.render(&b, 1).err().unwrap().to_string();
        assert!(e.contains("'color'") && e.contains("plate-2"), "{e}");
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
