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
use crate::session::{self, BackdropEdit, Session};
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

const INSTRUCTIONS: &str = "ArclightVFX Compositor (beta) is a node-graph compositor (linear float RGBA, premultiplied alpha). \
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
            "inputs": {"type": "array", "items": {"type": "string"}},
            "pos": {"type": "array", "items": {"type": "number"}, "description": "[x, y] in the node editor (top-left corner); default: a GUI places it beside what feeds it."}
        }, "required": ["op"]}),
    tool!("set_node_position", "Move nodes in the node editor (one undo step): 'node' to 'pos' [x, y] (top-left corner; nodes are 150 x 46), or many at once with 'positions' {node: [x, y], ...}.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "pos": {"type": "array", "items": {"type": "number"}},
            "positions": {"type": "object", "additionalProperties": {"type": "array", "items": {"type": "number"}}}
        }}),
    tool!("set_disabled", "Turn nodes off (bypassed: 'disabled' true) or on again, as one undo step: 'node', or several in 'nodes'. A node turned off passes its main input on unchanged (an image op its input, a merge its B, a Scanline Render its background, a particle modifier its stream), or outputs nothing (a source: a transparent frame or an empty scene); a Write or Media Out turned off writes nothing. describe_node says whether a node is.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "nodes": {"type": "array", "items": {"type": "string"}},
            "disabled": {"type": "boolean"}
        }, "required": ["disabled"]}),
    tool!("layout_nodes", "Lay out the nodes with no position, or with 'all' every node, as the node editor does a graph without positions: a column per depth from the sources, left to right (one undo step).",
        {"type": "object", "properties": {"all": {"type": "boolean"}}}),
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
    tool!("track_camera", "Camera Tracker: find and follow features over the project's frame range in its input, solve the camera's path and 3D points, and keep the solve on the node. Returns frames solved, points, the reprojection error (px), the focal length, the mode solved in ('free move' or 'tripod') and the frames placed by PnP. Takes a while (seconds to minutes).",
        {"type": "object", "properties": {
            "node": {"type": "string", "description": "The Camera Tracker node."},
            "mode": {"type": "string", "enum": ["auto", "free move", "free", "tripod"], "description": "'free move' (or 'free') for a camera that travels; 'tripod' for one that only turns about its center; 'auto' picks tripod when the camera only turns. Default: the node's Solve setting (auto). Kept on the node."},
            "distance": {"type": "number", "description": "How far from the camera a tripod solve puts its points (they have no depth). Default: the node's Tripod distance (10). Kept on the node."}
        }, "required": ["node"]}),
    tool!("scene_objects", "What a 3D node's scene holds at 'frame' (default 1): cameras, cards and meshes (node, world position; cards their size, shading and shadow flags; meshes their bounds), lights (node, type ambient/directional/point/spot/area, world position and direction, color, intensity, enabled, shadows, and the type's own settings: cone/penumbra/falloff, size/shape/two_sided/quality, radius), splat captures (node, count, bounds, sh_degree, world position; see the splats tool for the splats themselves), and the number of points. Lights are made with create_node (light_ambient, light_directional, light_point, light_spot, light_area; light_2d lights an image in 2D).",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}}, "required": ["node"]}),
    tool!("particles", "The particles a particle node passes on at 'frame' (default 1), simulated as needed: an emitter's, a modifier's or a Particle Render's stream, or the streams a Scene or Scanline Render gathers. Columns: ids, positions, velocities (units per second), ages and lives (frames), sizes, colors (premultiplied linear RGBA) and stream (index into 'streams', the nodes they came from). Pages: 'limit' (default 1000, at most 100000) from 'offset' (default 0); 'total' counts all, 'next_offset' starts the next page. Particle nodes: particle_emitter, particle_gravity/wind/turbulence/attractor/vortex/bounce/kill/speed_limit, particle_render, particles_2d.",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 0}}, "required": ["node"]}),
    tool!("splats", "The Gaussian splats a 3D node's scene holds at 'frame' (default 1): a Read Splat's capture, or those a Scene or Scanline Render gathers. Columns: positions (in the scene), scales (1 sigma, linear) and rotations ([w, x, y, z]) in the capture's own space, opacities (0-1), colors (the view-independent color, linear RGB) and set (index into 'sets': node, count, sh_degree and world, the capture's placement as a row-major 4x4). Pages: 'limit' (default 1000, at most 100000) from 'offset' (default 0); 'total' counts all, 'next_offset' starts the next page.",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["node"]}),
    tool!("camera_points", "A Camera Tracker's solved points: index, position in 3D, and where each lands in the frame at 'frame' (default: the first solved frame), to pick points for place_on_points or orient_camera; also the solve's mode ('free move' or 'tripod') and the frames placed by PnP.",
        {"type": "object", "properties": {"node": {"type": "string"}, "frame": {"type": "integer"}}, "required": ["node"]}),
    tool!("orient_camera", "Move a Camera Tracker's solve (arbitrary until then): 'ground' (3+ point indices) becomes level at height 0 with the camera above, 'origin' (a point index) goes to 0,0,0, and 'scale' ({points: [a, b], distance}) sets the distance between two points. Applied in that order; any may be left out.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "ground": {"type": "array", "items": {"type": "integer"}},
            "origin": {"type": "integer"},
            "scale": {"type": "object", "properties": {"points": {"type": "array", "items": {"type": "integer"}}, "distance": {"type": "number"}}}
        }, "required": ["node"]}),
    tool!("export_camera", "From a Camera Tracker's solve, add a Camera, a Point Cloud, a Scene of the two and a Scanline Render of it over the plate (when the lens distortion was solved, over a Lens Distort undistorting the plate, returned as 'lens'; to match the original plate instead, bend the render with a Lens Distort set to distort, same K1/K2). 'linked' (default true) makes the Camera follow the solve live, so re-solving or reorienting it updates the Camera; false keys a copy on every solved frame instead. Returns their names.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "linked": {"type": "boolean", "description": "Follow the solve live (default true), or key a copy (false)."}
        }, "required": ["node"]}),
    tool!("place_on_points", "Place a node on a Camera Tracker's solved points (indices from camera_points): 'card' (default) lies on the plane through 3+ points (else faces the camera), at their middle, sized to them, showing 'input' if given; 'locator' is a point at their middle to wire into a Scene or link params to. Returns the new node's name.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "points": {"type": "array", "items": {"type": "integer"}},
            "what": {"type": "string", "enum": ["card", "locator"]},
            "frame": {"type": "integer", "description": "The frame whose camera a card faces (default: the first solved)."},
            "input": {"type": "string", "description": "An image for the card."},
            "name": {"type": "string"}
        }, "required": ["node", "points"]}),
    tool!("list_backdrops", "'backdrops': the node editor's backdrops (group boxes behind nodes), back to front: id, title, pos and size (node editor units, top-left corner), color (sRGB 0-1, and color_name when it's a palette color), font_size (the title's) and nodes (those fully inside it, which move with it).",
        {"type": "object", "properties": {}}),
    tool!("create_backdrop", "Add a backdrop (a group box behind nodes in the node editor), in front of the others: fitted 'around' nodes with a margin as the editor's Group does, or at 'pos' with 'size'. 'color' is a palette name (slate, teal, green, olive, amber, rust, plum, blue), an index 0-7 into that order or [r, g, b] 0-1. Returns its id.",
        {"type": "object", "properties": {
            "title": {"type": "string", "description": "Default \"Group\"."},
            "around": {"type": "array", "items": {"type": "string"}, "description": "Nodes to fit it around."},
            "pos": {"type": "array", "items": {"type": "number"}, "description": "Top-left corner [x, y]."},
            "size": {"type": "array", "items": {"type": "number"}, "description": "[width, height], at least 120 x 60."},
            "color": {"type": ["string", "integer", "array"]},
            "font_size": {"type": "number", "description": "The title's size, 8-72 (default 14)."},
            "id": {"type": "string", "description": "Default backdrop1, backdrop2, ..."}
        }}),
    tool!("update_backdrop", "Change a backdrop (one undo step): any of title, color, font_size, pos, size, or 'around' to refit it to nodes. Moving it doesn't move the nodes in it.",
        {"type": "object", "properties": {
            "backdrop": {"type": "string"},
            "title": {"type": "string"},
            "around": {"type": "array", "items": {"type": "string"}},
            "pos": {"type": "array", "items": {"type": "number"}},
            "size": {"type": "array", "items": {"type": "number"}},
            "color": {"type": ["string", "integer", "array"]},
            "font_size": {"type": "number"}
        }, "required": ["backdrop"]}),
    tool!("delete_backdrop", "Remove a backdrop; the nodes in it stay.",
        {"type": "object", "properties": {"backdrop": {"type": "string"}}, "required": ["backdrop"]}),
    tool!("connect", "Connect node 'from' to input 'port' (index or label like \"B\"/\"mask\", default 0) of node 'to'.",
        {"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}, "port": {"type": ["integer", "string"]}}, "required": ["from", "to"]}),
    tool!("disconnect", "Disconnect input 'port' (index or label) of a node.",
        {"type": "object", "properties": {"node": {"type": "string"}, "port": {"type": ["integer", "string"]}}, "required": ["node", "port"]}),
    tool!("set_project", "Set the timeline (first/last frame, fps), the format: the frame size (width, height) of sources whose own size is 0, the default, and the project's folder (\"\" for none: then the folder its file is in). File paths given relative (in params) are in the folder; saved, the files in it are written relative to it so the whole folder can move.",
        {"type": "object", "properties": {"first_frame": {"type": "integer"}, "last_frame": {"type": "integer"}, "fps": {"type": "number"},
            "width": {"type": "integer"}, "height": {"type": "integer"}, "folder": {"type": "string"}}}),
    tool!("render_frame", "Render a node at a frame. Returns its frame size, data window ([x0, y0, x1, y1]: where pixels are stored) and mean/min/max color over the frame; 'path' also writes the image (.exr keeps float); 'preview' returns a PNG.",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "frame": {"type": "integer"},
            "path": {"type": "string"},
            "preview": {"type": "boolean"},
            "preview_size": {"type": "integer", "description": "Longest side of the preview in pixels (default 512)."},
            "cpu": {"type": "boolean", "description": "Render on the CPU only, for exact reproducible results (default: the GPU where it can)."}
        }, "required": ["node"]}),
    tool!("render_output", "Render the whole frame range of a media_out node to its file(s).",
        {"type": "object", "properties": {
            "node": {"type": "string"},
            "cpu": {"type": "boolean", "description": "Render on the CPU only (default: the GPU where it can)."}
        }, "required": ["node"]}),
    tool!("gpu_info", "Whether GPU acceleration is in use: status line, adapter, backend, limits and transfer path, or why rendering is on the CPU.",
        {"type": "object", "properties": {}}),
    tool!("gpu_check", "Run the GPU self-test: every GPU path against the CPU on generated images, with errors, tolerances and timings. Takes seconds.",
        {"type": "object", "properties": {}}),
    tool!("probe_media", "Metadata of an image, image sequence (####) or video file: size, frame range, fps, and a video's sound (sample rate, channels, duration; null without).",
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
    tool!("list_instances", "Running ArclightVFX windows that tools can be sent to.", {"type": "object", "properties": {}}),
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

/// A node editor position, [x, y].
fn pos_value(v: &Value) -> Result<[f32; 2]> {
    serde_json::from_value(v.clone()).map_err(|_| anyhow!("a position must be [x, y]"))
}

/// Tools about a window's view state (selection, viewer, playhead). Only a
/// GUI answers them; the headless session refuses them.
pub fn is_gui_tool(tool: &str) -> bool {
    matches!(tool, "select_node" | "view_node" | "get_view")
}

/// Tools that can run for a long time: front ends with an interactive
/// thread (the GUI) run these on a worker, on a snapshot of the graph.
pub fn is_render_tool(tool: &str) -> bool {
    matches!(tool, "render_frame" | "render_output" | "gpu_check")
}

/// Run one graph tool against `s`. Returns the tool's JSON result. A
/// requested preview comes back as `preview_png_base64`.
pub fn call_tool(s: &mut Session, tool: &str, args: &Value) -> Result<Value> {
    call_tool_with_progress(s, tool, args, &Progress::default())
}

/// [`call_tool`], reporting `render_output`'s progress through `progress`
/// (which can also cancel it).
pub fn call_tool_with_progress(s: &mut Session, tool: &str, args: &Value, progress: &Progress) -> Result<Value> {
    crate::crash::note(format!("tool {tool}"));
    // "cpu": true renders on the CPU only (see `gpu::cpu_only`).
    if args.get("cpu").and_then(Value::as_bool) == Some(true) {
        return crate::gpu::cpu_only(|| run_tool(s, tool, args, progress));
    }
    run_tool(s, tool, args, progress)
}

fn run_tool(s: &mut Session, tool: &str, args: &Value, progress: &Progress) -> Result<Value> {
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
            let pos = match args.get("pos") {
                None | Some(Value::Null) => None,
                Some(v) => Some(pos_value(v)?),
            };
            let id = s.create_node_at(str_arg(args, "op")?, opt_str(args, "name"), &obj_arg(args, "params")?, &inputs, pos)?;
            json!({"node": id})
        }
        "set_node_position" => {
            let mut moves = Vec::new();
            if let Some(node) = opt_str(args, "node") {
                moves.push((node.to_string(), pos_value(args.get("pos").ok_or_else(|| anyhow!("missing 'pos' for '{node}'"))?)?));
            }
            for (node, p) in obj_arg(args, "positions")? {
                moves.push((node, pos_value(&p)?));
            }
            if moves.is_empty() {
                bail!("give 'node' and 'pos', or 'positions'");
            }
            s.set_node_positions(&moves)?;
            ok()
        }
        "set_disabled" => {
            let mut ids: Vec<String> = match args.get("nodes") {
                None | Some(Value::Null) => Vec::new(),
                Some(v) => serde_json::from_value(v.clone()).context("'nodes' must be a list of node names")?,
            };
            ids.extend(opt_str(args, "node").map(str::to_string));
            if ids.is_empty() {
                bail!("give 'node' or 'nodes'");
            }
            let disabled = args.get("disabled").and_then(Value::as_bool).ok_or_else(|| anyhow!("missing boolean argument 'disabled'"))?;
            s.set_disabled(&ids, disabled)?;
            ok()
        }
        "layout_nodes" => {
            s.layout_nodes(args.get("all").and_then(Value::as_bool).unwrap_or(false));
            json!({"positions": s.graph.nodes.iter().map(|n| (n.id.clone(), json!(n.pos))).collect::<Map<String, Value>>()})
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
        "list_backdrops" => json!({"backdrops": s.backdrops()}),
        "create_backdrop" => json!({"backdrop": s.create_backdrop(opt_str(args, "id"), &BackdropEdit::from_args(args)?)?}),
        "update_backdrop" => {
            s.update_backdrop(str_arg(args, "backdrop")?, &BackdropEdit::from_args(args)?)?;
            ok()
        }
        "delete_backdrop" => {
            s.delete_backdrop(str_arg(args, "backdrop")?)?;
            ok()
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
            if let Some(folder) = opt_str(args, "folder") {
                s.set_folder(Some(Path::new(folder)))?;
            }
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
        "track_camera" => s.track_camera(str_arg(args, "node")?, opt_str(args, "mode"), args.get("distance").and_then(Value::as_f64), progress)?,
        "scene_objects" => s.scene_objects(str_arg(args, "node")?, args.get("frame").and_then(Value::as_i64).unwrap_or(1))?,
        "particles" => {
            let n = |k: &str, d: u64| args.get(k).and_then(Value::as_u64).unwrap_or(d) as usize;
            s.particles(str_arg(args, "node")?, args.get("frame").and_then(Value::as_i64).unwrap_or(1), n("offset", 0), n("limit", 1000))?
        }
        "splats" => {
            let n = |k: &str, d: u64| args.get(k).and_then(Value::as_u64).unwrap_or(d) as usize;
            s.splats(str_arg(args, "node")?, args.get("frame").and_then(Value::as_i64).unwrap_or(1), n("offset", 0), n("limit", 1000))?
        }
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
            json!({"frame": frame, "points": pts, "mode": if sol.tripod { "tripod" } else { "free move" }, "pnp_frames": sol.pnp_frames})
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
        "export_camera" => s.export_camera_linked(str_arg(args, "node")?, args.get("linked").and_then(Value::as_bool).unwrap_or(true))?,
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
        "gpu_info" => crate::gpu::info(),
        "gpu_check" => {
            let report = crate::gpu::check::run();
            let mut out = serde_json::to_value(&report)?;
            out["passed"] = json!(report.passed());
            out["summary"] = json!(report.summary());
            out
        }
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
            return Err(anyhow!("'{gui}' needs a running ArclightVFX window: call attach_instance first"))
        }
        other => return Err(anyhow!("unknown tool '{other}'")),
    })
}

/// Metadata for a media file: stills report their size, sequences their
/// range too, videos their fps, frame count and sound (or null).
pub fn probe_media(path: &Path) -> Result<Value> {
    let s = path.to_string_lossy();
    if media::is_video(path) {
        let v = media::probe_video(path)?;
        let audio = crate::audio::probe(path).map(|a| json!({"sample_rate": a.sample_rate, "channels": a.channels, "duration": a.duration_secs, "start": a.start_secs}));
        return Ok(json!({"kind": "video", "width": v.width, "height": v.height, "bits": v.bits, "fps": v.fps, "frames": v.frames, "audio": audio}));
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
                    "serverInfo": {"name": "arclight", "title": "ArclightVFX Compositor", "version": env!("CARGO_PKG_VERSION")},
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

    /// Through MCP as in the GUI: with a project folder, paths given
    /// relative are in it; saved, they're relative to it (and the folder to
    /// the file in it); the whole folder moved and loaded, renders read and
    /// write there.
    #[test]
    fn a_project_folder_moves_whole_through_mcp() {
        let tmp = std::env::temp_dir().join(format!("arclight_mcp_folder_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (show, moved) = (tmp.join("show"), tmp.join("moved/show"));
        std::fs::create_dir_all(show.join("plates")).unwrap();
        crate::image::Image::filled(4, 2, [0.25, 0.5, 0.75, 1.0]).write(&show.join("plates/bg.0001.png")).unwrap();
        let mut s = Server::new(Session::default());
        let ok = |r: Value| assert_eq!(r["isError"], false, "{r}");
        ok(call(&mut s, "set_project", json!({"folder": show, "first_frame": 1, "last_frame": 1})));
        ok(call(&mut s, "create_node", json!({"op": "read", "name": "plate", "params": {"path": "plates/bg.####.png"}})));
        ok(call(&mut s, "create_node", json!({"op": "media_out", "name": "out", "inputs": ["plate"], "params": {"path": "renders/comp.####.png", "first_frame": 1, "last_frame": 1}})));
        let graph = call(&mut s, "get_graph", json!({}))["structuredContent"]["graph"].clone();
        assert_eq!(graph["nodes"][0]["params"]["path"], show.join("plates/bg.####.png").to_string_lossy().as_ref(), "kept whole");
        ok(call(&mut s, "save_graph", json!({"path": show.join("comp.json")})));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(show.join("comp.json")).unwrap()).unwrap();
        assert_eq!((&saved["project"]["folder"], &saved["nodes"][0]["params"]["path"], &saved["nodes"][1]["params"]["path"]), (&json!("."), &json!("plates/bg.####.png"), &json!("renders/comp.####.png")));
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&show, &moved).unwrap();
        let mut s = Server::new(Session::default());
        ok(call(&mut s, "load_graph", json!({"path": moved.join("comp.json")})));
        ok(call(&mut s, "render_output", json!({"node": "out"})));
        ok(call(&mut s, "render_frame", json!({"node": "plate", "frame": 1, "path": "renders/still.png"})));
        assert!(moved.join("renders/comp.0001.png").exists() && moved.join("renders/still.png").exists());
        assert!(!show.exists());
        // No folder: cleared, the file's own folder counts for paths given.
        ok(call(&mut s, "set_project", json!({"folder": ""})));
        ok(call(&mut s, "set_params", json!({"node": "plate", "params": {"path": "plates/bg.####.png"}})));
        let graph = call(&mut s, "get_graph", json!({}))["structuredContent"]["graph"].clone();
        assert_eq!(graph["project"].get("folder"), None);
        assert_eq!(graph["nodes"][0]["params"]["path"], moved.join("plates/bg.####.png").to_string_lossy().as_ref());
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Backdrops through MCP: made around nodes, listed, changed and
    /// deleted, each one undo step; bad arguments say what's wrong.
    #[test]
    fn backdrops_through_mcp() {
        let mut s = Server::new(Session::default());
        let ok = |r: Value| {
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"].clone()
        };
        ok(call(&mut s, "create_node", json!({"op": "constant", "name": "plate"})));
        ok(call(&mut s, "create_node", json!({"op": "blur", "name": "soft", "inputs": ["plate"]})));
        let made = ok(call(&mut s, "create_backdrop", json!({"around": ["plate", "soft"], "title": "Plate", "color": "amber", "font_size": 24})));
        assert_eq!(made["backdrop"], "backdrop1");
        ok(call(&mut s, "create_backdrop", json!({"id": "notes", "pos": [500, 0], "size": [300, 200]})));
        let list = ok(call(&mut s, "list_backdrops", json!({})))["backdrops"].clone();
        assert_eq!(list.as_array().unwrap().len(), 2);
        assert_eq!((&list[0]["nodes"], &list[0]["color_name"], &list[0]["font_size"]), (&json!(["plate", "soft"]), &json!("amber"), &json!(24.0)));
        assert_eq!((&list[1]["title"], &list[1]["nodes"]), (&json!("Group"), &json!([])));
        ok(call(&mut s, "update_backdrop", json!({"backdrop": "notes", "title": "Notes", "color": [0.2, 0.2, 0.2]})));
        let graph = ok(call(&mut s, "get_graph", json!({})))["graph"].clone();
        assert_eq!(graph["backdrops"][1]["title"], "Notes", "saved with the graph");
        ok(call(&mut s, "delete_backdrop", json!({"backdrop": "backdrop1"})));
        let list = ok(call(&mut s, "list_backdrops", json!({})))["backdrops"].clone();
        assert_eq!(list.as_array().unwrap().len(), 1);
        ok(call(&mut s, "undo", json!({})));
        ok(call(&mut s, "undo", json!({})));
        let list = ok(call(&mut s, "list_backdrops", json!({})))["backdrops"].clone();
        assert_eq!((list[0]["id"].as_str(), list[1]["title"].as_str()), (Some("backdrop1"), Some("Group")));
        for (tool, args, says) in [
            ("create_backdrop", json!({"title": "x"}), "'around'"),
            ("create_backdrop", json!({"around": ["nope"]}), "nope"),
            ("create_backdrop", json!({"around": ["plate"], "color": "mauve"}), "mauve"),
            ("update_backdrop", json!({"backdrop": "gone"}), "gone"),
            ("update_backdrop", json!({"backdrop": "notes", "font_size": 4}), "font_size"),
            ("delete_backdrop", json!({}), "backdrop"),
        ] {
            let r = call(&mut s, tool, args);
            assert!(r["isError"] == true && r["content"][0]["text"].as_str().unwrap().contains(says), "{tool}: {r}");
        }
    }

    /// Node positions through MCP: given when made, moved one or many at
    /// once, laid out.
    #[test]
    fn node_positions_through_mcp() {
        let mut s = Server::new(Session::default());
        let ok = |r: Value| {
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"].clone()
        };
        ok(call(&mut s, "create_node", json!({"op": "constant", "name": "plate", "pos": [0, 0]})));
        ok(call(&mut s, "create_node", json!({"op": "blur", "name": "soft", "inputs": ["plate"]})));
        ok(call(&mut s, "create_node", json!({"op": "grade", "name": "look", "inputs": ["soft"]})));
        let laid = ok(call(&mut s, "layout_nodes", json!({})))["positions"].clone();
        assert_eq!(laid["plate"], json!([0.0, 0.0]));
        assert!(laid["look"][0].as_f64() > laid["soft"][0].as_f64());
        ok(call(&mut s, "set_node_position", json!({"node": "plate", "pos": [-200, 50]})));
        ok(call(&mut s, "set_node_position", json!({"positions": {"soft": [100, 50], "look": [400, 50]}})));
        let n = ok(call(&mut s, "describe_node", json!({"node": "look"})));
        assert_eq!(n["pos"], json!([400.0, 50.0]));
        ok(call(&mut s, "create_backdrop", json!({"around": ["soft", "look"]})));
        assert_eq!(ok(call(&mut s, "list_backdrops", json!({})))["backdrops"][0]["nodes"], json!(["soft", "look"]));
        for (args, says) in [
            (json!({}), "'positions'"),
            (json!({"node": "plate"}), "'pos'"),
            (json!({"node": "plate", "pos": [1]}), "[x, y]"),
            (json!({"positions": {"nope": [0, 0]}}), "nope"),
        ] {
            let r = call(&mut s, "set_node_position", args);
            assert!(r["isError"] == true && r["content"][0]["text"].as_str().unwrap().contains(says), "{r}");
        }
    }

    /// Turning nodes off through MCP: one or several, seen in describe_node.
    #[test]
    fn nodes_turn_off_through_mcp() {
        let mut s = Server::new(Session::default());
        let ok = |r: Value| {
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"].clone()
        };
        ok(call(&mut s, "create_node", json!({"op": "constant", "name": "plate"})));
        ok(call(&mut s, "create_node", json!({"op": "grade", "name": "look", "inputs": ["plate"]})));
        ok(call(&mut s, "create_node", json!({"op": "blur", "name": "soft", "inputs": ["look"]})));
        ok(call(&mut s, "set_disabled", json!({"node": "look", "disabled": true})));
        assert_eq!(ok(call(&mut s, "describe_node", json!({"node": "look"})))["disabled"], true);
        ok(call(&mut s, "set_disabled", json!({"nodes": ["look", "soft"], "disabled": false})));
        assert_eq!(ok(call(&mut s, "describe_node", json!({"node": "look"})))["disabled"], false);
        for (args, says) in [(json!({"disabled": true}), "'node'"), (json!({"node": "look"}), "'disabled'"), (json!({"node": "nope", "disabled": true}), "nope")] {
            let r = call(&mut s, "set_disabled", args);
            assert!(r["isError"] == true && r["content"][0]["text"].as_str().unwrap().contains(says), "{r}");
        }
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
            points, rms: 0.2, frame_rms: vec![0.2, 0.2], point_rms: Vec::new(), pnp_frames: Vec::new(), tripod: false, k1: 0.0, k2: 0.0,
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
        let names = call(&mut s, "export_camera", json!({"node": "ct", "linked": false}))["structuredContent"].clone();
        let cam = call(&mut s, "describe_node", json!({"node": names["camera"]}))["structuredContent"].clone();
        assert_eq!(cam["params"]["translate"]["keys"].as_array().unwrap().len(), 2);
        let r = call(&mut s, "render_frame", json!({"node": names["render"], "frame": 1}));
        assert_eq!(r["isError"], false, "{r}");
        // Tracking needs a moving camera: a still plate can't be solved.
        let r = call(&mut s, "track_camera", json!({"node": "ct"}));
        assert_eq!(r["isError"], true, "{r}");
        // A mode or distance that isn't one is refused before tracking; a
        // failed solve leaves the node's settings as they were.
        let r = call(&mut s, "track_camera", json!({"node": "ct", "mode": "sideways"}));
        assert!(r["content"][0]["text"].as_str().unwrap().contains("isn't one of"), "{r}");
        let r = call(&mut s, "track_camera", json!({"node": "ct", "mode": "tripod", "distance": 0}));
        assert!(r["content"][0]["text"].as_str().unwrap().contains("above 0"), "{r}");
        let r = call(&mut s, "track_camera", json!({"node": "ct", "mode": "free", "distance": 5}));
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!((&s.session.graph.nodes[1].params["solve_mode"], &s.session.graph.nodes[1].params["distance"]), (&json!("auto"), &json!(10.0)));
        // A tripod solve: scaling keeps the camera where it is, and the
        // exported Camera only turns.
        let mut sol = s.session.camera_solution("ct").unwrap();
        sol.tripod = true;
        sol.cameras = vec![[1.0, 2.0, 3.0, -20.0, 0.0, 0.0], [1.0, 2.0, 3.0, -20.0, 5.0, 0.0]];
        s.session.graph.nodes[1].params["solution"] = sol.to_value();
        assert_eq!(call(&mut s, "camera_points", json!({"node": "ct"}))["structuredContent"]["mode"], "tripod");
        call(&mut s, "orient_camera", json!({"node": "ct", "scale": {"points": [0, 3], "distance": 12}}));
        let sol = s.session.camera_solution("ct").unwrap();
        let near = |v: &[f64]| (v[0] - 1.0).abs() + (v[1] - 2.0).abs() + (v[2] - 3.0).abs() < 1e-9;
        assert!(sol.cameras.iter().all(|c| near(&c[..3])), "{:?}", sol.cameras);
        let names = call(&mut s, "export_camera", json!({"node": "ct", "linked": false}))["structuredContent"].clone();
        let cam = call(&mut s, "describe_node", json!({"node": names["camera"]}))["structuredContent"].clone();
        let t: Vec<f64> = cam["params"]["translate"].as_array().unwrap().iter().filter_map(Value::as_f64).collect();
        assert!(near(&t) && cam["params"]["rotate"]["keys"].as_array().unwrap().len() == 2, "{cam}");
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

    /// Lights made with create_node, listed by scene_objects with their type
    /// and world position; Light 2D lights an image.
    #[test]
    fn lights_are_created_and_listed() {
        let mut s = Server::new(Session::default());
        let ops = call(&mut s, "list_ops", json!({"category": "scene"}))["content"][0]["text"].to_string();
        for op in ["light_ambient", "light_directional", "light_point", "light_spot", "light_area"] {
            assert!(ops.contains(op), "{op} listed");
        }
        call(&mut s, "create_node", json!({"op": "locator", "name": "target", "params": {"translate": [3, 0, 0]}}));
        call(&mut s, "create_node", json!({"op": "light_spot", "name": "key", "inputs": ["target"], "params": {"translate": [0, 4, 0], "cone": 30, "shadows": true}}));
        call(&mut s, "create_node", json!({"op": "light_area", "name": "soft", "params": {"translate": [1, 2, 3], "size": [2, 1], "shape": "disk", "exposure": 1}}));
        call(&mut s, "create_node", json!({"op": "light_ambient", "name": "fill", "params": {"intensity": 0.2}}));
        call(&mut s, "create_node", json!({"op": "cube", "name": "box"}));
        call(&mut s, "create_node", json!({"op": "scene", "name": "all", "inputs": ["key", "soft", "fill", "box"]}));
        let r = call(&mut s, "scene_objects", json!({"node": "all"}));
        assert_eq!(r["isError"], false, "{r}");
        let o = &r["structuredContent"];
        let lights = o["lights"].as_array().unwrap();
        let types: Vec<&str> = lights.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["spot", "area", "ambient"]);
        let key = &lights[0];
        assert_eq!((key["node"].as_str(), key["position"].clone(), key["cone"].clone(), key["shadows"].clone()), (Some("key"), json!([0.0, 4.0, 0.0]), json!(30.0), json!(true)));
        let d: Vec<f64> = key["direction"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert!((d[0] - 0.6).abs() < 1e-9 && (d[1] + 0.8).abs() < 1e-9, "aims at the locator: {d:?}");
        assert_eq!((lights[1]["position"].clone(), lights[1]["shape"].as_str(), lights[1]["intensity"].clone()), (json!([1.0, 2.0, 3.0]), Some("disk"), json!(2.0)));
        assert_eq!(o["meshes"][0]["node"], "box");
        assert_eq!(call(&mut s, "scene_objects", json!({"node": "fill"}))["structuredContent"]["lights"][0]["type"], "ambient");
        // An image node has no scene.
        call(&mut s, "create_node", json!({"op": "constant", "name": "plate", "params": {"width": 32, "height": 16, "color": [0.1, 0.1, 0.1, 1]}}));
        assert_eq!(call(&mut s, "scene_objects", json!({"node": "plate"}))["isError"], true);
        // Light 2D: a point light at a pixel brightens it.
        call(&mut s, "create_node", json!({"op": "light_2d", "name": "lamp", "inputs": ["plate"], "params": {"type": "point", "position": [8, 8], "radius": 6}}));
        let r = call(&mut s, "render_frame", json!({"node": "lamp"}));
        assert_eq!(r["isError"], false, "{r}");
        let img = s.session.render("lamp", 1).unwrap();
        assert!(img.get(8, 8)[0] > 0.9 && (img.get(20, 8)[0] - 0.1).abs() < 1e-6);
    }

    /// The particles query: a burst under gravity, in pages, the same for
    /// the same seed.
    #[test]
    fn particles_are_listed_in_pages() {
        let mut s = Server::new(Session::default());
        call(&mut s, "create_node", json!({"op": "particle_emitter", "name": "emit", "params": {"rate": 0, "burst": 25, "start": 1, "speed": 0, "life": 1000, "seed": 3}}));
        call(&mut s, "create_node", json!({"op": "particle_gravity", "name": "fall", "inputs": ["emit"]}));
        let at = |s: &mut Server, args: Value| {
            let r = call(s, "particles", args);
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"].clone()
        };
        let all = at(&mut s, json!({"node": "fall", "frame": 25}));
        assert_eq!((all["total"].as_u64(), all["count"].as_u64(), all["next_offset"].clone()), (Some(25), Some(25), Value::Null));
        assert_eq!(all["streams"], json!(["fall"]));
        // Falling: below where they were born, moving down.
        let born = at(&mut s, json!({"node": "emit", "frame": 25}));
        let y = |o: &Value, k: &str, i: usize| o[k][i][1].as_f64().unwrap();
        assert!(y(&all, "positions", 0) < y(&born, "positions", 0) - 1.0 && y(&all, "velocities", 0) < -5.0, "{} {}", y(&all, "positions", 0), y(&all, "velocities", 0));
        assert_eq!(all["ids"].as_array().unwrap().len(), 25);
        assert!(all["ages"][0].as_f64().unwrap() > 0.0 && all["sizes"][0].as_f64().unwrap() > 0.0 && all["colors"][0].as_array().unwrap().len() == 4);
        // Pages of 10: 10, 10, 5, then the ids line up with the whole list.
        let mut ids = Vec::new();
        let mut offset = Some(0u64);
        while let Some(o) = offset {
            let page = at(&mut s, json!({"node": "fall", "frame": 25, "offset": o, "limit": 10}));
            ids.extend(page["ids"].as_array().unwrap().clone());
            offset = page["next_offset"].as_u64();
        }
        assert_eq!(json!(ids), all["ids"]);
        // The same again (cached or not), and an image node has none.
        assert_eq!(at(&mut s, json!({"node": "fall", "frame": 25}))["positions"], all["positions"]);
        call(&mut s, "create_node", json!({"op": "constant", "name": "plate"}));
        assert_eq!(call(&mut s, "particles", json!({"node": "plate"}))["isError"], true);
    }

    /// The splats query and scene_objects: the sample capture placed, in
    /// pages that add up, gathered with another by a Scene.
    #[test]
    fn splats_are_listed_in_pages() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let mut s = Server::new(Session::default());
        let ok = |r: Value| {
            assert_eq!(r["isError"], false, "{r}");
            r["structuredContent"].clone()
        };
        ok(call(&mut s, "create_node", json!({"op": "read_splat", "name": "cap", "params": {"file": file, "translate": [10, 0, 0], "sh_degree": 0}})));
        ok(call(&mut s, "create_node", json!({"op": "read_splat", "name": "cap2", "params": {"file": file}})));
        ok(call(&mut s, "create_node", json!({"op": "scene", "name": "all", "inputs": ["cap", "cap2"]})));
        let objs = ok(call(&mut s, "scene_objects", json!({"node": "all"})));
        assert_eq!(objs["splats"].as_array().unwrap().len(), 2);
        let one = &objs["splats"][0];
        assert_eq!((one["node"].as_str(), one["count"].as_u64(), one["sh_degree"].as_u64(), one["position"].clone()), (Some("cap"), Some(320), Some(0), json!([10.0, 0.0, 0.0])));
        let b = |k: usize, c: usize| one["bounds"][k][c].as_f64().unwrap();
        assert!(b(0, 0) > 8.0 && b(1, 0) < 12.0 && b(1, 0) > 11.5 && (b(1, 1) - 2.0).abs() < 0.01 && b(0, 1).abs() < 1e-6, "placed: {one}");
        let all = ok(call(&mut s, "splats", json!({"node": "all", "limit": 100000})));
        assert_eq!((all["total"].as_u64(), all["count"].as_u64(), all["next_offset"].clone()), (Some(640), Some(640), Value::Null));
        assert_eq!((all["sets"][0]["node"].as_str(), all["sets"][1]["sh_degree"].as_u64(), all["sets"][0]["world"][3].as_f64()), (Some("cap"), Some(1), Some(10.0)));
        // Placed: the first set 10 to the right of the second, the same splat.
        let x = |i: usize| all["positions"][i][0].as_f64().unwrap();
        assert!((x(0) - x(320) - 10.0).abs() < 1e-5);
        assert_eq!((all["set"][0].as_u64(), all["set"][320].as_u64()), (Some(0), Some(1)));
        assert_eq!(all["scales"][0], all["scales"][320], "scales in the capture's own space");
        assert!(all["opacities"][0].as_f64().unwrap() > 0.8 && all["colors"][0].as_array().unwrap().len() == 3 && all["rotations"][0].as_array().unwrap().len() == 4);
        // Pages of 250 across the two sets add up to the whole.
        let mut positions = Vec::new();
        let mut offset = Some(0u64);
        while let Some(o) = offset {
            let page = ok(call(&mut s, "splats", json!({"node": "all", "offset": o, "limit": 250})));
            positions.extend(page["positions"].as_array().unwrap().clone());
            offset = page["next_offset"].as_u64();
        }
        assert_eq!(json!(positions), all["positions"]);
        assert_eq!(ok(call(&mut s, "splats", json!({"node": "cap2", "offset": 1000})))["count"], 0);
        // A scene without splats, or an image: an error.
        ok(call(&mut s, "create_node", json!({"op": "cube", "name": "box"})));
        assert_eq!(call(&mut s, "splats", json!({"node": "box"}))["isError"], true);
        ok(call(&mut s, "create_node", json!({"op": "constant", "name": "plate"})));
        assert_eq!(call(&mut s, "splats", json!({"node": "plate"}))["isError"], true);
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
