//! Model Context Protocol server: JSON-RPC 2.0 over stdio (one message per
//! line) or HTTP (`POST`), exposing graph editing and rendering as tools so
//! AI agents can build and render composites.
//!
//! Tools run against a local headless [`Session`], or — after
//! `attach_instance` or with `arclight mcp --attach` — against a running GUI
//! through the [`crate::bridge`], where the user watches the edits happen.

use crate::bridge::{self, Instance};
use crate::media;
use crate::ops;
use crate::plugin;
use crate::render::Progress;
use crate::session::{self, Session};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Arclight is a node-graph compositor (linear float RGBA, premultiplied alpha). \
Build a graph with create_node/connect/set_params, inspect ops with list_ops/describe_op, and check \
results with render_frame (preview=true returns a PNG). Merge composites input A over B. Params that \
are numeric can be keyframed with set_key. Colors are straight linear RGBA.";

/// One tool's name, description and JSON Schema for its arguments.
struct Tool {
    name: &'static str,
    description: &'static str,
    schema: fn() -> Value,
}

macro_rules! tool {
    ($name:literal, $desc:literal, $schema:tt) => {
        Tool { name: $name, description: $desc, schema: || json!($schema) }
    };
}


static TOOLS: &[Tool] = &[
    tool!("list_ops", "List every node type (op) with its label and category, including plugin ops.",
        {"type": "object", "properties": {"category": {"type": "string", "description": "Only this category (source, color, channel, keyer, merge, shape, transform, filter, output, scene)."}}}),
    tool!("describe_op", "Inputs, params (kind, range, default) and help for one op.",
        {"type": "object", "properties": {"op": {"type": "string"}}, "required": ["op"]}),
    tool!("get_graph", "The whole graph as JSON, plus file path, unsaved state and undo availability.",
        {"type": "object", "properties": {}}),
    tool!("describe_node", "A node's op, connected inputs, downstream nodes and params (defaults filled in).",
        {"type": "object", "properties": {"node": {"type": "string"}}, "required": ["node"]}),
    tool!("create_node", "Add a node. Returns its name. 'inputs' lists upstream node names by port order (A, B, mask for merge).",
        {"type": "object", "properties": {
            "op": {"type": "string"},
            "name": {"type": "string", "description": "Node name; defaults to op + number."},
            "params": {"type": "object"},
            "inputs": {"type": "array", "items": {"type": "string"}}
        }, "required": ["op"]}),
    tool!("delete_node", "Remove a node; nodes it fed are reconnected to its first input.",
        {"type": "object", "properties": {"node": {"type": "string"}}, "required": ["node"]}),
    tool!("rename_node", "Rename a node, updating connections.",
        {"type": "object", "properties": {"node": {"type": "string"}, "new_name": {"type": "string"}}, "required": ["node", "new_name"]}),
    tool!("set_params", "Set one or more params of a node (validated; one undo step). A value can be keyframes {\"keys\": ...}, a link {\"link\": {\"node\", \"param\"}} or a formula {\"link\": {\"expr\": \"sin(frame / 8) * 10 + tracker1.track1.x\"}} (see README, Live links).",
        {"type": "object", "properties": {"node": {"type": "string"}, "params": {"type": "object"}}, "required": ["node", "params"]}),
    tool!("set_key", "Set a keyframe on a numeric param at a frame, animating it.",
        {"type": "object", "properties": {"node": {"type": "string"}, "param": {"type": "string"}, "frame": {"type": "integer"}, "value": {}}, "required": ["node", "param", "frame", "value"]}),
    tool!("export_tracker", "Add a node that carries a Tracker's motion: a Transform following the solved motion (op 'transform', default), or a Corner Pin following all four tracks in perspective (op 'corner_pin'). Mode 'match-move' (default) moves a layer along with the tracks, 'stabilize' removes the motion. 'linked' (default true) makes it follow the Tracker live, so re-tracking updates it; false keys a copy on each tracked frame instead. 'input' connects a layer to it. Returns the new node's name.",
        {"type": "object", "properties": {
            "node": {"type": "string", "description": "The Tracker node."},
            "op": {"type": "string", "enum": ["transform", "corner_pin"]},
            "mode": {"type": "string", "enum": ["match-move", "stabilize"]},
            "linked": {"type": "boolean", "description": "Follow the Tracker live (default true), or key a copy (false)."},
            "name": {"type": "string"},
            "input": {"type": "string"}
        }, "required": ["node"]}),
    tool!("track_camera", "Camera Tracker: find and follow features over the project's frame range in its input, solve the camera's path and 3D points, and keep the solve on the node. Returns frames solved, points, the reprojection error (px) and the focal length. Takes a while (seconds to minutes).",
        {"type": "object", "properties": {"node": {"type": "string", "description": "The Camera Tracker node."}}, "required": ["node"]}),
    tool!("camera_points", "A Camera Tracker's solved points: index, position in 3D, and where each lands in the frame at 'frame' (default: the first solved frame), to pick points for place_on_points or orient_camera.",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}}, "required": ["node"]}),
    tool!("orient_camera", "Move a Camera Tracker's solve (arbitrary until then): 'ground' (3+ point indices) becomes level at height 0 with the camera above, 'origin' (a point index) goes to 0,0,0, and 'scale' ({points: [a, b], distance}) sets the distance between two points. Applied in that order; any may be left out.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "ground": {"type": "array", "items": {"type": "integer"}},
            "origin": {"type": "integer"},
            "scale": {"type": "object", "properties": {"points": {"type": "array", "items": {"type": "integer"}}, "distance": {"type": "number"}}}
        }, "required": ["node"]}),
    tool!("export_camera", "From a Camera Tracker's solve, add a Camera keyed on every solved frame, a Point Cloud, a Scene of the two and a Scanline Render of it over the plate. Returns their names.",
        {"type": "object", "properties": {"node": {"type": "string"}}, "required": ["node"]}),
    tool!("place_on_points", "Place a node on a Camera Tracker's solved points (indices from camera_points): 'card' (default) lies on the plane through 3+ points (else faces the camera), at their middle, sized to them, showing 'input' if given; 'locator' is a point at their middle to wire into a Scene or link params to. Returns the new node's name.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "points": {"type": "array", "items": {"type": "integer"}},
            "what": {"type": "string", "enum": ["card", "locator"]},
            "frame": {"type": "integer", "description": "The frame whose camera a card faces (default: the first solved)."},
            "input": {"type": "string", "description": "An image for the card."},
            "name": {"type": "string"}
        }, "required": ["node", "points"]}),
    tool!("connect", "Connect node 'from' to input 'port' (index or label like \"B\"/\"mask\", default 0) of node 'to'.",
        {"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}, "port": {"type": ["integer", "string"]}}, "required": ["from", "to"]}),
    tool!("disconnect", "Disconnect input 'port' (index or label) of a node.",
        {"type": "object", "properties": {"node": {"type": "string"}, "port": {"type": ["integer", "string"]}}, "required": ["node", "port"]}),
    tool!("set_project", "Set the timeline (first/last frame, fps) and the format: the frame size (width, height) of sources whose own size is 0, the default.",
        {"type": "object", "properties": {"first_frame": {"type": "integer"}, "last_frame": {"type": "integer"}, "fps": {"type": "number"},
            "width": {"type": "integer"}, "height": {"type": "integer"}}}),
    tool!("render_frame", "Render a node at a frame. Returns its frame size, data window ([x0, y0, x1, y1]: where pixels are stored) and mean/min/max color over the frame; 'path' also writes the image (.exr keeps float); 'preview' returns a PNG.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "frame": {"type": "integer"},
            "path": {"type": "string"},
            "preview": {"type": "boolean"},
            "preview_size": {"type": "integer", "description": "Longest side of the preview in pixels (default 512)."}
        }, "required": ["node"]}),
    tool!("render_output", "Render the whole frame range of a media_out node to its file(s).",
        {"type": "object", "properties": {"node": {"type": "string"}}, "required": ["node"]}),
    tool!("probe_media", "Metadata of an image, image sequence (####) or video file: size, frame range, fps.",
        {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    tool!("new_graph", "Start an empty graph (undoable).", {"type": "object", "properties": {}}),
    tool!("load_graph", "Open a graph JSON file (undoable).",
        {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    tool!("save_graph", "Save the graph, to 'path' or to where it was loaded from.",
        {"type": "object", "properties": {"path": {"type": "string"}}}),
    tool!("undo", "Undo the last edit.", {"type": "object", "properties": {}}),
    tool!("redo", "Redo the last undone edit.", {"type": "object", "properties": {}}),
    tool!("list_plugins", "Plugin libraries found at startup and the ops each added (or why it failed).",
        {"type": "object", "properties": {}}),
    tool!("select_node", "In an attached GUI: select a node (its properties open, the node graph scrolls to it). null clears the selection.",
        {"type": "object", "properties": {"node": {"type": ["string", "null"]}}, "required": ["node"]}),
    tool!("view_node", "In an attached GUI: show a node in the viewer and select it. With a Viewer node, the node is shown in place of the Viewer's input until the Viewer itself is viewed (view_node on the Viewer goes back); with none, a Viewer is created and wired to it. 'frame' also moves the playhead.",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}}, "required": ["node"]}),
    tool!("get_view", "In an attached GUI: what the user is looking at: selected node, viewed node, current frame, playing.",
        {"type": "object", "properties": {}}),
    tool!("list_instances", "Running Arclight GUI windows that tools can be sent to.", {"type": "object", "properties": {}}),
    tool!("attach_instance", "Send all further tools to a running GUI (by pid, default the newest).",
        {"type": "object", "properties": {"pid": {"type": "integer"}}}),
    tool!("detach_instance", "Go back to the local headless graph.", {"type": "object", "properties": {}}),
];

/// The `tools/list` result.
pub fn tool_list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
        .collect();
    json!({ "tools": tools })
}

// ---------------------------------------------------------------------------
// Tool implementations (shared with the GUI bridge and Python)
// ---------------------------------------------------------------------------

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name).and_then(Value::as_str).ok_or_else(|| anyhow!("missing string argument '{name}'"))
}

fn opt_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn obj_arg(args: &Value, name: &str) -> Result<Map<String, Value>> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(_) => Err(anyhow!("'{name}' must be an object")),
    }
}

/// Tools about a window's view state (selection, viewer, playhead). Only a
/// GUI answers them; the headless session refuses them.
pub fn is_gui_tool(tool: &str) -> bool {
    matches!(tool, "select_node" | "view_node" | "get_view")
}

/// Tools that can run for a long time: front ends with an interactive
/// thread (the GUI) run these on a worker, on a snapshot of the graph.
pub fn is_render_tool(tool: &str) -> bool {
    matches!(tool, "render_frame" | "render_output")
}

/// Run one graph tool against `s`. Returns the tool's JSON result. A
/// requested preview comes back as `preview_png_base64`.
pub fn call_tool(s: &mut Session, tool: &str, args: &Value) -> Result<Value> {
    call_tool_with_progress(s, tool, args, &Progress::default())
}

/// [`call_tool`], reporting `render_output`'s progress through `progress`
/// (which can also cancel it).
pub fn call_tool_with_progress(s: &mut Session, tool: &str, args: &Value, progress: &Progress) -> Result<Value> {
    let ok = || json!({"ok": true});
    Ok(match tool {
        "list_ops" => {
            let cat = opt_str(args, "category");
            let list: Vec<Value> = ops::all()
                .into_iter()
                .map(session::op_schema)
                .filter(|o| cat.is_none_or(|c| o["category"] == c))
                .map(|o| json!({"name": o["name"], "label": o["label"], "category": o["category"]}))
                .collect();
            json!(list)
        }
        "describe_op" => session::op_schema(session::op_info(str_arg(args, "op")?)?),
        "get_graph" => json!({
            "graph": s.graph,
            "path": s.path,
            "dirty": s.dirty,
            "can_undo": s.can_undo(),
            "can_redo": s.can_redo(),
        }),
        "describe_node" => s.describe_node(str_arg(args, "node")?)?,
        "create_node" => {
            let inputs: Vec<String> = match args.get("inputs") {
                None | Some(Value::Null) => Vec::new(),
                Some(v) => serde_json::from_value(v.clone()).context("'inputs' must be a list of node names")?,
            };
            let id = s.create_node(str_arg(args, "op")?, opt_str(args, "name"), &obj_arg(args, "params")?, &inputs)?;
            json!({"node": id})
        }
        "delete_node" => {
            s.delete_node(str_arg(args, "node")?)?;
            ok()
        }
        "rename_node" => {
            s.rename(str_arg(args, "node")?, str_arg(args, "new_name")?)?;
            ok()
        }
        "set_params" => {
            s.set_params(str_arg(args, "node")?, &obj_arg(args, "params")?)?;
            ok()
        }
        "set_key" => {
            let frame = args.get("frame").and_then(Value::as_i64).ok_or_else(|| anyhow!("missing integer 'frame'"))?;
            let value = args.get("value").ok_or_else(|| anyhow!("missing 'value'"))?;
            s.set_key(str_arg(args, "node")?, str_arg(args, "param")?, frame, value)?;
            ok()
        }
        "export_tracker" => {
            let stabilize = match opt_str(args, "mode").unwrap_or("match-move") {
                "match-move" => false,
                "stabilize" => true,
                other => bail!("unknown mode '{other}' (match-move or stabilize)"),
            };
            let op = opt_str(args, "op").unwrap_or("transform");
            let linked = args.get("linked").and_then(Value::as_bool).unwrap_or(true);
            let id = s.export_tracker(str_arg(args, "node")?, op, stabilize, linked, opt_str(args, "name"), opt_str(args, "input"))?;
            json!({"node": id})
        }
        "connect" => {
            let to = str_arg(args, "to")?;
            let port = s.port_index(to, args.get("port").unwrap_or(&json!(0)))?;
            s.connect(str_arg(args, "from")?, to, port)?;
            ok()
        }
        "disconnect" => {
            let node = str_arg(args, "node")?;
            let port = s.port_index(node, args.get("port").ok_or_else(|| anyhow!("missing 'port'"))?)?;
            s.disconnect(node, port)?;
            ok()
        }
        "set_project" => {
            let int = |k| args.get(k).and_then(Value::as_i64);
            let format = match (int("width"), int("height")) {
                (None, None) => None,
                (w, h) => {
                    let f = s.graph.project.format;
                    let size = |v: Option<i64>, d: usize| v.map_or(d, |v| v.max(0) as usize);
                    Some(crate::image::Size { width: size(w, f.width), height: size(h, f.height) })
                }
            };
            s.set_project(int("first_frame"), int("last_frame"), args.get("fps").and_then(Value::as_f64), format)?;
            json!(s.graph.project)
        }
        "render_frame" => {
            let node = str_arg(args, "node")?;
            let frame = args.get("frame").and_then(Value::as_i64).unwrap_or(s.graph.project.first_frame);
            let img = match opt_str(args, "path") {
                Some(p) => s.render_to_file(node, frame, Path::new(p))?,
                None => s.render(node, frame)?,
            };
            let mut out = session::image_stats(&img);
            out["frame"] = json!(frame);
            if let Some(p) = opt_str(args, "path") {
                out["path"] = json!(p);
            }
            if args.get("preview").and_then(Value::as_bool).unwrap_or(false) {
                let size = args.get("preview_size").and_then(Value::as_u64).unwrap_or(512).clamp(16, 4096) as u32;
                out["preview_png_base64"] = json!(base64(&preview_png(&img, size)?));
            }
            out
        }
        "track_camera" => s.track_camera(str_arg(args, "node")?, progress)?,
        "camera_points" => {
            let sol = s.camera_solution(str_arg(args, "node")?)?;
            let frame = args.get("frame").and_then(Value::as_i64).or_else(|| sol.frames.first().copied()).unwrap_or(1);
            let cam = sol.camera(frame);
            let format = crate::image::Size { width: sol.format[0], height: sol.format[1] };
            let pts: Vec<Value> = sol
                .points
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let px = cam.as_ref().and_then(|c| c.project(&crate::geom::Vec3::from(*p), format)).map(|(q, _)| [q[0], q[1]]);
                    json!({"index": i, "position": p, "frame_position": px})
                })
                .collect();
            json!({"frame": frame, "points": pts})
        }
        "orient_camera" => {
            let ids = |k: &str| -> Option<Vec<usize>> { args.get(k)?.as_array().map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()) };
            let origin = args.get("origin").and_then(Value::as_u64).map(|v| v as usize);
            let scale = match args.get("scale") {
                Some(sc) => {
                    let p: Vec<usize> = sc["points"].as_array().map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()).unwrap_or_default();
                    let d = sc["distance"].as_f64().ok_or_else(|| anyhow!("scale needs a distance"))?;
                    if p.len() != 2 {
                        bail!("scale needs two points");
                    }
                    Some((p[0], p[1], d))
                }
                None => None,
            };
            s.orient_camera(str_arg(args, "node")?, origin, ids("ground").as_deref(), scale)?;
            ok()
        }
        "export_camera" => s.export_camera(str_arg(args, "node")?)?,
        "place_on_points" => {
            let node = str_arg(args, "node")?;
            let ids: Vec<usize> = args.get("points").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()).unwrap_or_default();
            let frame = match args.get("frame").and_then(Value::as_i64) {
                Some(f) => f,
                None => s.camera_solution(node)?.frames.first().copied().unwrap_or(1),
            };
            let id = s.place_on_points(node, &ids, opt_str(args, "what").unwrap_or("card"), frame, opt_str(args, "input"), opt_str(args, "name"))?;
            json!({"node": id})
        }
        "render_output" => {
            let node = str_arg(args, "node")?;
            let frames = s.render_output(node, progress)?;
            json!({"frames": frames, "path": s.node(node).and_then(|n| n.params.get("path")).cloned()})
        }
        "probe_media" => probe_media(Path::new(str_arg(args, "path")?))?,
        "new_graph" => {
            s.clear();
            ok()
        }
        "load_graph" => {
            s.load(Path::new(str_arg(args, "path")?))?;
            json!({"nodes": s.graph.nodes.len()})
        }
        "save_graph" => json!({"path": s.save(opt_str(args, "path").map(Path::new))?}),
        "undo" => json!({"undone": s.undo()}),
        "redo" => json!({"redone": s.redo()}),
        "list_plugins" => json!(plugin::reports()
            .into_iter()
            .map(|r| match r.result {
                Ok(ops) => json!({"path": r.path, "ops": ops}),
                Err(e) => json!({"path": r.path, "error": e}),
            })
            .collect::<Vec<_>>()),
        gui if is_gui_tool(gui) => {
            return Err(anyhow!("'{gui}' needs a running Arclight window: call attach_instance first"))
        }
        other => return Err(anyhow!("unknown tool '{other}'")),
    })
}

/// Metadata for a media file: stills report their size, sequences their
/// range too, videos their fps and frame count.
pub fn probe_media(path: &Path) -> Result<Value> {
    let s = path.to_string_lossy();
    if media::is_video(path) {
        let v = media::probe_video(path)?;
        return Ok(json!({"kind": "video", "width": v.width, "height": v.height, "bits": v.bits, "fps": v.fps, "frames": v.frames}));
    }
    let (pattern, range) = if media::is_sequence_pattern(&s) {
        (s.to_string(), media::detect_sequence_range(&s))
    } else {
        match media::sequence_from_file(path) {
            Some((p, a, b)) if a != b => (p, Some((a, b))),
            _ => (s.to_string(), None),
        }
    };
    let first = match range {
        Some((a, _)) => media::frame_path(&pattern, a),
        None => path.to_path_buf(),
    };
    let img = crate::image::Image::read(&first)?;
    let mut out = json!({"kind": if range.is_some() { "sequence" } else { "still" }, "width": img.width, "height": img.height});
    if let Some((a, b)) = range {
        out["pattern"] = json!(pattern);
        out["first_frame"] = json!(a);
        out["last_frame"] = json!(b);
    }
    Ok(out)
}

fn preview_png(img: &crate::image::Image, size: u32) -> Result<Vec<u8>> {
    let rgba = img.flat().to_rgba8();
    let rgba = if rgba.width().max(rgba.height()) > size {
        let scale = size as f64 / rgba.width().max(rgba.height()) as f64;
        let (w, h) = ((rgba.width() as f64 * scale).round().max(1.0) as u32, (rgba.height() as f64 * scale).round().max(1.0) as u32);
        image::imageops::thumbnail(&rgba, w, h)
    } else {
        rgba
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut buf, image::ImageFormat::Png)?;
    Ok(buf.into_inner())
}

pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Protocol state: the local session, or the GUI instance tools go to.
pub struct Server {
    pub session: Session,
    pub attached: Option<Instance>,
}

impl Server {
    pub fn new(session: Session) -> Server {
        Server { session, attached: None }
    }

    /// Run a tool, routing to the attached GUI if there is one.
    pub fn call(&mut self, tool: &str, args: &Value) -> Result<Value> {
        match tool {
            "list_instances" => Ok(json!(bridge::list_instances())),
            "attach_instance" => {
                let pid = args.get("pid").and_then(Value::as_u64).map(|p| p as u32);
                let inst = bridge::find_instance(pid)?;
                let out = json!({"attached": inst});
                self.attached = Some(inst);
                Ok(out)
            }
            "detach_instance" => Ok(json!({"detached": self.attached.take()})),
            _ => match &self.attached {
                Some(inst) => bridge::call(inst, tool, args),
                None => call_tool(&mut self.session, tool, args),
            },
        }
    }

    /// Handle one JSON-RPC message (or batch). Returns `None` when nothing
    /// should be sent back (notifications).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            if batch.is_empty() {
                return Some(error(Value::Null, INVALID_REQUEST, "empty batch"));
            }
            let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
            return (!replies.is_empty()).then_some(Value::Array(replies));
        }
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response from the client (we send no requests) or garbage.
            return msg.get("id").filter(|_| msg.get("result").is_none() && msg.get("error").is_none()).map(|id| {
                error(id.clone(), INVALID_REQUEST, "missing method")
            });
        };
        let id = msg.get("id").cloned()?; // notifications get no reply
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        Some(match method {
            "initialize" => {
                let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
                result(id, json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "arclight", "title": "Arclight Compositor", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, tool_list()),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error(id, INVALID_PARAMS, "missing tool name"));
                };
                if !TOOLS.iter().any(|t| t.name == name) {
                    return Some(error(id, INVALID_PARAMS, &format!("unknown tool '{name}'")));
                }
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                result(id, tool_result(self.call(name, &args)))
            }
            _ => error(id, METHOD_NOT_FOUND, &format!("method '{method}' not found")),
        })
    }
}

/// An MCP `CallToolResult`: errors are reported in-band so the model sees them.
fn tool_result(r: Result<Value>) -> Value {
    match r {
        Ok(mut v) => {
            let mut content = Vec::new();
            if let Some(png) = v.as_object_mut().and_then(|m| m.remove("preview_png_base64")) {
                content.push(json!({"type": "image", "data": png, "mimeType": "image/png"}));
            }
            content.insert(0, json!({"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}));
            let mut out = json!({"content": content, "isError": false});
            if v.is_object() {
                out["structuredContent"] = v;
            }
            out
        }
        Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn parse_message(text: &str) -> Result<Value, Value> {
    serde_json::from_str(text).map_err(|e| error(Value::Null, PARSE_ERROR, &format!("parse error: {e}")))
}

/// Serve MCP over stdin/stdout, one JSON message per line, until EOF.
pub fn serve_stdio(server: &mut Server) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match parse_message(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(e),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Serve MCP over HTTP: each `POST` carries one JSON-RPC message and gets the
/// reply as `application/json` (202 for notifications).
pub fn serve_http(server: Server, addr: &str) -> Result<()> {
    let listener = TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    eprintln!("arclight: MCP server on http://{}/mcp", listener.local_addr()?);
    let server = Arc::new(Mutex::new(server));
    for stream in listener.incoming().flatten() {
        let server = server.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_http_client(stream, &server) {
                eprintln!("arclight: http: {e:#}");
            }
        });
    }
    Ok(())
}

fn serve_http_client(stream: TcpStream, server: &Mutex<Server>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut content_length = 0usize;
        let mut keep_alive = !request_line.ends_with("HTTP/1.0\r\n");
        loop {
            let mut h = String::new();
            reader.read_line(&mut h)?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => content_length = v.trim().parse().context("bad Content-Length")?,
                    "connection" => keep_alive = !v.trim().eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }
        let mut body = vec![0u8; content_length.min(64 << 20)];
        reader.read_exact(&mut body)?;
        let method = request_line.split_whitespace().next().unwrap_or("");
        let (status, payload) = if method != "POST" {
            ("405 Method Not Allowed", String::new())
        } else {
            let reply = match parse_message(&String::from_utf8_lossy(&body)) {
                Ok(msg) => server.lock().unwrap().handle(&msg),
                Err(e) => Some(e),
            };
            match reply {
                Some(r) => ("200 OK", r.to_string()),
                None => ("202 Accepted", String::new()),
            }
        };
        let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", payload.len());
        if !payload.is_empty() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if method != "POST" {
            head.push_str("Allow: POST\r\n");
        }
        head.push_str(if keep_alive { "\r\n" } else { "Connection: close\r\n\r\n" });
        out.write_all(head.as_bytes())?;
        out.write_all(payload.as_bytes())?;
        out.flush()?;
        if !keep_alive {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(server: &mut Server, id: i64, method: &str, params: Value) -> Value {
        server.handle(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap()
    }

    fn call(server: &mut Server, name: &str, args: Value) -> Value {
        rpc(server, 1, "tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    #[test]
    fn handshake_and_tool_listing() {
        let mut s = Server::new(Session::default());
        let init = rpc(&mut s, 1, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}}));
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(s.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let tools = rpc(&mut s, 2, "tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"create_node") && names.contains(&"render_frame"));
        assert_eq!(rpc(&mut s, 3, "nope", json!({}))["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn tracker_exports_a_keyed_transform_or_corner_pin() {
        let mut s = Server::new(Session::default());
        call(&mut s, "create_node", json!({"op": "constant", "name": "plate", "params": {"width": 64, "height": 32}}));
        call(&mut s, "create_node", json!({"op": "tracker", "name": "trk", "inputs": ["plate"], "params": {
            "track1": {"keys": [[1, [10.5, 10.5]], [4, [13.5, 9.5]]], "interp": "linear"}}}));
        let r = call(&mut s, "export_tracker", json!({"node": "trk", "input": "plate", "linked": false}));
        assert_eq!(r["isError"], false, "{r}");
        let xf = call(&mut s, "describe_node", json!({"node": r["structuredContent"]["node"]}))["structuredContent"].clone();
        assert_eq!(xf["inputs"][0]["source"], "plate");
        assert_eq!(xf["params"]["translate"]["keys"][1], json!([4, [3.0, -1.0]]));
        let r = call(&mut s, "export_tracker", json!({"node": "trk", "mode": "stabilize"}));
        assert_eq!(r["isError"], false, "{r}");
        let r = call(&mut s, "export_tracker", json!({"node": "plate"}));
        assert_eq!(r["isError"], true);
        // A corner pin needs all four tracks.
        let r = call(&mut s, "export_tracker", json!({"node": "trk", "op": "corner_pin"}));
        assert_eq!(r["isError"], true);
        call(&mut s, "set_params", json!({"node": "trk", "params": {"track2_on": true, "track3_on": true, "track4_on": true}}));
        let r = call(&mut s, "export_tracker", json!({"node": "trk", "op": "corner_pin", "mode": "stabilize", "linked": false}));
        let pin = call(&mut s, "describe_node", json!({"node": r["structuredContent"]["node"]}))["structuredContent"].clone();
        assert_eq!((pin["op"].as_str(), &pin["params"]["to1"]), (Some("corner_pin"), &json!([10.5, 10.5])));
        assert_eq!(pin["params"]["from1"]["keys"][1], json!([4, [13.5, 9.5]]));
    }

    /// The camera-tracker tools on a solve put in place by hand: listing
    /// points, orienting, placing a card and a locator, exporting.
    #[test]
    fn camera_tracker_tools() {
        let mut s = Server::new(Session::default());
        call(&mut s, "create_node", json!({"op": "constant", "name": "plate", "params": {"width": 96, "height": 54}}));
        call(&mut s, "create_node", json!({"op": "camera_tracker", "name": "ct", "inputs": ["plate"]}));
        let r = call(&mut s, "camera_points", json!({"node": "ct"}));
        assert!(r["isError"] == true && r["content"][0]["text"].as_str().unwrap().contains("track it first"), "{r}");
        // A 4 × 4 grid of points on the ground 5 below, two cameras above.
        let points: Vec<[f64; 3]> = (0..16).map(|i| [(i % 4) as f64 - 1.5, -5.0, -10.0 + (i / 4) as f64]).collect();
        let sol = crate::camtrack::Solution {
            format: [96, 54], focal: 35.0, film_back: [36.0, 24.0], frames: vec![1, 2],
            cameras: vec![[0.0, 0.0, 0.0, -20.0, 0.0, 0.0], [0.5, 0.0, 0.0, -20.0, 0.0, 0.0]],
            points, rms: 0.2, frame_rms: vec![0.2, 0.2],
        };
        s.session.graph.nodes[1].params["solution"] = sol.to_value();
        let r = call(&mut s, "camera_points", json!({"node": "ct", "frame": 2}))["structuredContent"].clone();
        assert_eq!((r["frame"].as_i64(), r["points"].as_array().unwrap().len()), (Some(2), 16));
        assert!(r["points"][5]["frame_position"].is_array());
        let r = call(&mut s, "orient_camera", json!({"node": "ct", "ground": [0, 3, 12, 15], "origin": 0, "scale": {"points": [0, 3], "distance": 6}}));
        assert_eq!(r["isError"], false, "{r}");
        let sol = s.session.camera_solution("ct").unwrap();
        assert!(sol.points.iter().all(|p| p[1].abs() < 1e-9) && sol.points[0].iter().all(|v| v.abs() < 1e-9));
        assert!(((sol.points[3][0] - sol.points[0][0]).hypot(sol.points[3][2] - sol.points[0][2]) - 6.0).abs() < 1e-9);
        let card = call(&mut s, "place_on_points", json!({"node": "ct", "points": [5, 6, 9, 10], "input": "plate"}))["structuredContent"]["node"].clone();
        let c = call(&mut s, "describe_node", json!({"node": card}))["structuredContent"].clone();
        assert_eq!((c["op"].as_str(), c["inputs"][0]["source"].as_str()), (Some("card"), Some("plate")));
        assert!(c["params"]["translate"][1].as_f64().unwrap().abs() < 1e-9, "on the ground");
        let loc = call(&mut s, "place_on_points", json!({"node": "ct", "points": [0], "what": "locator"}))["structuredContent"]["node"].clone();
        let l = call(&mut s, "describe_node", json!({"node": loc}))["structuredContent"].clone();
        assert_eq!(l["params"]["translate"], json!([0.0, 0.0, 0.0]));
        let names = call(&mut s, "export_camera", json!({"node": "ct"}))["structuredContent"].clone();
        let cam = call(&mut s, "describe_node", json!({"node": names["camera"]}))["structuredContent"].clone();
        assert_eq!(cam["params"]["translate"]["keys"].as_array().unwrap().len(), 2);
        let r = call(&mut s, "render_frame", json!({"node": names["render"], "frame": 1}));
        assert_eq!(r["isError"], false, "{r}");
        // Tracking needs a moving camera: a still plate can't be solved.
        let r = call(&mut s, "track_camera", json!({"node": "ct"}));
        assert_eq!(r["isError"], true, "{r}");
    }

    #[test]
    fn linked_exports_follow_the_tracker_live() {
        let mut s = Server::new(Session::default());
        // A small layer (its data window shows where it's moved) and a
        // track sliding right 3 px a frame.
        call(&mut s, "create_node", json!({"op": "rectangle", "name": "plate", "params": {"width": 64, "height": 32, "center": [20, 16], "size": [8, 8]}}));
        call(&mut s, "create_node", json!({"op": "tracker", "name": "trk", "inputs": ["plate"], "params": {
            "track1": {"keys": [[1, [10.5, 10.5]], [4, [19.5, 10.5]]], "interp": "linear"}}}));
        let linked = call(&mut s, "export_tracker", json!({"node": "trk", "input": "plate"}))["structuredContent"]["node"].clone();
        let copy = call(&mut s, "export_tracker", json!({"node": "trk", "input": "plate", "linked": false}))["structuredContent"]["node"].clone();
        let linked = linked.as_str().unwrap().to_string();
        let copy = copy.as_str().unwrap().to_string();
        let params = call(&mut s, "describe_node", json!({"node": linked}))["structuredContent"]["params"].clone();
        assert_eq!(params["translate"]["link"]["node"], "trk", "{params}");
        let window = |s: &mut Server, node: &str, frame: i64| {
            let r = call(s, "render_frame", json!({"node": node, "frame": frame}));
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"]["window"].clone()
        };
        // Same picture as a copy on the tracked frames, and in between.
        for f in [1, 3, 4] {
            assert_eq!(window(&mut s, &linked, f), window(&mut s, &copy, f), "frame {f}");
        }
        assert_eq!(window(&mut s, &linked, 4)[0].as_i64().unwrap() - window(&mut s, &linked, 1)[0].as_i64().unwrap(), 9);
        // Re-track frame 4 further right: the linked one follows, the copy doesn't.
        call(&mut s, "set_key", json!({"node": "trk", "param": "track1", "frame": 4, "value": [25.5, 10.5]}));
        assert_eq!(window(&mut s, &linked, 4)[0].as_i64().unwrap() - window(&mut s, &copy, 4)[0].as_i64().unwrap(), 6);
        // Renaming the Tracker keeps the link; deleting it is an error naming it.
        assert_eq!(call(&mut s, "rename_node", json!({"node": "trk", "new_name": "trk2"}))["isError"], false);
        let params = call(&mut s, "describe_node", json!({"node": linked}))["structuredContent"]["params"].clone();
        assert_eq!(params["translate"]["link"]["node"], "trk2");
        window(&mut s, &linked, 4);
        call(&mut s, "delete_node", json!({"node": "trk2"}));
        let r = call(&mut s, "render_frame", json!({"node": linked, "frame": 4}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("linked to 'trk2'"), "{r}");
    }

    #[test]
    fn scripts_set_links_with_offsets() {
        let mut s = Server::new(Session::default());
        call(&mut s, "create_node", json!({"op": "rectangle", "name": "a", "params": {"width": 64, "height": 32, "center": [20, 16], "size": [4, 4]}}));
        call(&mut s, "create_node", json!({"op": "rectangle", "name": "b", "params": {"width": 64, "height": 32, "size": [4, 4]}}));
        let link = json!({"link": {"node": "a", "param": "center", "add": [10, -4]}});
        assert_eq!(call(&mut s, "set_params", json!({"node": "b", "params": {"center": link}}))["isError"], false);
        let window = |s: &mut Server, n: &str| call(s, "render_frame", json!({"node": n}))["structuredContent"]["window"].clone();
        let (wa, wb) = (window(&mut s, "a"), window(&mut s, "b"));
        assert_eq!((wb[0].as_i64().unwrap() - wa[0].as_i64().unwrap(), wb[1].as_i64().unwrap() - wa[1].as_i64().unwrap()), (10, -4));
        // Moving the source moves the follower.
        call(&mut s, "set_params", json!({"node": "a", "params": {"center": [30, 16]}}));
        assert_eq!(window(&mut s, "b")[0].as_i64().unwrap() - window(&mut s, "a")[0].as_i64().unwrap(), 10);
        // A link with no node is refused.
        let bad = call(&mut s, "set_params", json!({"node": "b", "params": {"center": {"link": {"param": "center"}}}}));
        assert_eq!(bad["isError"], true);
    }

    #[test]
    fn sources_follow_the_project_format() {
        let mut s = Server::new(Session::default());
        let stats = |s: &mut Server, node: &str| call(s, "render_frame", json!({"node": node}))["structuredContent"].clone();
        call(&mut s, "create_node", json!({"op": "constant", "name": "c"}));
        assert_eq!((stats(&mut s, "c")["width"].as_u64(), stats(&mut s, "c")["height"].as_u64()), (Some(1920), Some(1080)));
        assert_eq!(call(&mut s, "set_project", json!({"width": 640, "height": 360}))["isError"], false);
        assert_eq!(stats(&mut s, "c")["width"], 640, "re-rendered at the new format");
        // A new shape is centered in the format and stores only its bounds.
        call(&mut s, "create_node", json!({"op": "rectangle", "name": "r"}));
        let r = call(&mut s, "describe_node", json!({"node": "r"}))["structuredContent"]["params"].clone();
        assert_eq!(r["center"], json!([320.0, 180.0]));
        let st = stats(&mut s, "r");
        // 180 px wide (256 scaled to the 360 px short side) from x = 230,
        // plus 2 px of edge padding.
        assert_eq!((st["width"].as_u64(), st["window"][0].as_i64()), (Some(640), Some(228)));
        // An explicit size wins.
        call(&mut s, "set_params", json!({"node": "c", "params": {"width": 32, "height": 16}}));
        assert_eq!(stats(&mut s, "c")["width"], 32);
        assert_eq!(call(&mut s, "set_project", json!({"width": 0}))["isError"], true);
    }

    #[test]
    fn agent_builds_and_renders_a_comp() {
        let mut s = Server::new(Session::default());
        let r = call(&mut s, "create_node", json!({"op": "constant", "name": "bg", "params": {"width": 64, "height": 32, "color": [0, 0, 1, 1]}}));
        assert_eq!(r["isError"], false, "{r}");
        call(&mut s, "create_node", json!({"op": "ellipse", "name": "dot", "params": {"width": 64, "height": 32, "center": [32, 16], "size": [20, 20]}}));
        call(&mut s, "create_node", json!({"op": "merge", "name": "comp"}));
        assert_eq!(call(&mut s, "connect", json!({"from": "dot", "to": "comp", "port": "A"}))["isError"], false);
        assert_eq!(call(&mut s, "connect", json!({"from": "bg", "to": "comp", "port": "B"}))["isError"], false);

        let r = call(&mut s, "render_frame", json!({"node": "comp", "preview": true, "preview_size": 32}));
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["width"], 64);
        assert_eq!(r["content"][1]["type"], "image");

        let gui = call(&mut s, "view_node", json!({"node": "comp"}));
        assert_eq!(gui["isError"], true);
        assert!(gui["content"][0]["text"].as_str().unwrap().contains("attach_instance"));

        let bad = call(&mut s, "set_params", json!({"node": "comp", "params": {"operation": "explode"}}));
        assert_eq!(bad["isError"], true);
        assert!(bad["content"][0]["text"].as_str().unwrap().contains("not a choice"));
    }

    #[test]
    fn batches_and_parse_errors() {
        let mut s = Server::new(Session::default());
        let reply = s
            .handle(&json!([
                {"jsonrpc": "2.0", "id": 1, "method": "ping"},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "ping"}
            ]))
            .unwrap();
        assert_eq!(reply.as_array().unwrap().len(), 2);
        assert_eq!(parse_message("{nope").unwrap_err()["error"]["code"], PARSE_ERROR);
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
