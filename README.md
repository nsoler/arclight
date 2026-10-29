# Arclight

An original **node-graph image compositor** written in Rust. Arclight reads a
JSON description of an image-processing DAG, evaluates it, and writes the
result. It ships as a command-line renderer (`arclight`) and an interactive desktop
app (`arclight-gui`) with a node editor, viewer, player and properties panels.

This is original work — its design and code are not derived from any existing
compositing product.

## Design

- **Pixels**: 32-bit float RGBA.
- **Color space**: linear light. sRGB encode/decode happens only at the 8-bit
  (PNG) boundary.
- **Alpha**: **premultiplied (associated)** throughout the pipeline — the
  mathematically correct space for compositing and filtering. Blurring and
  merging in this space is why soft edges fade cleanly with no dark fringing.
- **Graph**: a JSON list of nodes (`id`, `op`, `inputs`, `params`), topologically
  sorted (with cycle detection) and evaluated once per node, results cached.
- **Parallelism**: per-pixel work is parallelized across cores with Rayon.

## Build

```sh
cargo build --release      # -> target/release/arclight, target/release/arclight-gui
cargo test                 # unit + graph tests
cargo build --release --no-default-features   # CLI only, no GUI dependencies
cargo build --release -p arclight-plugin-example  # example native plugin
python/build.sh                                   # Python `arclt` module (needs Python ≥ 3.10 headers)
```

Video input/output needs `ffmpeg` and `ffprobe` on `PATH` (e.g. `brew install ffmpeg`).
Image sequences and stills need nothing extra.

## GUI

```sh
cargo run --release --bin arclight-gui                 # starter project
cargo run --release --bin arclight-gui -- comp.json    # open a graph
```

| Area | What it does |
|------|--------------|
| **Viewer** (center) | Shows the input of the first `viewer` node (or the selected node). Double-clicking another node (or **V**) shows that node instead, without rewiring the Viewer (an eye marks it, and the status line says so) until the Viewer node is clicked or double-clicked. A node with an input left unplugged treats it as empty, so the comp below it keeps showing; only output nodes (Viewer, Write, Media Out) report an unconnected input. Drag (or middle-drag) to pan, scroll/pinch to zoom, double-click or **F** to fit. RGB/R/G/B/A channel isolation, exposure in stops, float pixel readout under the cursor. When an image stores pixels for only part of the frame, or past it (its *data window*: a small shape, a layer moved off-frame), the window is outlined dashed, its position and size show next to the frame size, and pixels past the frame show dimmed. The selected node's handles draw over the image: **Transform** outlines the transformed layer and shows a gizmo at its center (the input layer's middle unless Center is set; the panel's **Auto** puts it back) (drag anywhere on the layer or its center to move, ⇧ for one axis; the frame's corners scale uniformly, ⇧ for free, and its edges scale one axis; the ring to rotate, ⇧ snaps to 15°; the red/green handles to scale x/y, ⇧ for uniform), **Crop** shows its box (drag edges or corners), **Rectangle**/**Ellipse** show a box (drag the center to move, corners/edges to size about the center, ⇧ keeps the aspect; the stem above the top edge rotates), **Polygon** shows its vertices (drag to move, ⇧-click to select several and drag them together, drag on empty space to box-select (⇧ adds; middle-drag still pans), ⌘/Ctrl+A selects all; with two or more selected, a dashed transform box surrounds them: drag inside to move, corners to scale about its center (⇧ free), edges to stretch one axis, the stem above to rotate (⇧ snaps to 15°), and Roto tangents turn and scale along, click an edge to insert a vertex, ⌘/Ctrl-click to add one after the selected vertex, **⌫** deletes selected vertices), **Roto** works the same on its curve and also shows the selected vertices' tangent handles (drag one to reshape the curve smoothly, ⌥/Alt-drag to break the tangent; Alt-drag a vertex to pull out new tangents; ⌘/Ctrl-click-drag adds a vertex and pulls out its tangents; clicking the curve splits it without changing its shape). **Tracker** shows each enabled track's pattern box (solid), search box (dashed) and tracked path; drag inside a pattern box to place or correct a track (keys the current frame), drag a box corner to size that track's box (⇧ sizes it for every track, back to the panel's shared Pattern/Search size). ⌘/Ctrl-drag a box to move it off its point (an offset: follow a textured feature, report a point beside it, e.g. one that gets covered); **Clear offsets** puts them back. Frames where a track matched weakly show as orange dots on its path and orange bands on the timeline; the panel names the weakest match (click it to go there), and **Clear ◀ / ▶** removes the enabled tracks' keys before or after the current frame. While the viewer shows the Tracker's own match-moved or stabilized output, the boxes are drawn (and dragged) where that output shows them. **Keyer**: ⌘/Ctrl+click picks the screen color from the Keyer's input (whatever the viewer shows), ⌘/Ctrl+drag averages a box. **Corner Pin** shows its To quad (solid, numbered corners) and From quad (dashed); drag either's corners. Handle drags key animated params at the current frame; inserting or deleting vertices applies to every key. |
| **Player** (under the viewer) | Go to start, step back, play backward, pause, play forward, step forward, go to end. Scrubbable timeline (green strip = frames already cached), in/out, FPS, loop, and the project **Format** (frame size for sources whose own size is 0). Plays in real time and shows the achieved fps. |
| **Curve editor** (bottom, second tab) | The selected node's keyframed params as value-over-time curves, one per channel (x/y, r/g/b/a), with a toggle per channel. Click / Shift-click / box-drag to select keys, drag to move them in time and value, double-click a curve to add a key, **⌫** to delete. Smooth/Linear/Step buttons set interpolation. Selected keys on smooth curves show tangent handles: drag one to set the slope (in and out move together), **⌥**-drag to break the key so each side moves on its own; hollow handles are automatic, filled are user-set. Tangents Auto/Flat/Break-Unify buttons act on the selected keys. Scroll to pan, ⌘-scroll or pinch to zoom (⌥ value only, ⇧ time only), middle-drag to pan, **F** to frame. The ruler scrubs the playhead. |
| **Node graph** (bottom) | Right-click, or press **⇧E** for a searchable node browser (type to filter, ↑/↓ and Enter to place, Esc to close), to add nodes. With a node selected, the new node is wired after it; opened over a wire, the menu or browser inserts the new node into that wire. Drag an unconnected node onto a wire (it lights up) to splice it in; shake a node while dragging it to pull it out of its wires (its input then feeds what it fed). The minimap in the lower right shows the whole graph and the view: click or drag in it to move there, scroll over it to zoom into that spot, double-click to fit. Inputs are yellow squares (hollow when optional, e.g. `mask`), outputs green circles. Drag from an output to an input (or back) to connect; drag a connected input away to reroute or disconnect. Drag from an output onto a wire (it lights up) to join the two with a new Merge: the dragged node goes into A, over the wire's source in B (e.g. drop a Keyer onto the background's wire to put the keyed foreground over it). Double-click a node or press **V** to view it (click the Viewer node to go back to its output). **⌫** deletes (the chain is reconnected). Drop image/video files onto the window to create Media In nodes. |
| **Properties** (right) | One panel per opened node, newest first, generated from each op's parameter specs. Media In: browse, auto-detected frame range, clip info. Media Out: browse, render the frame range with progress and cancel. |

Shortcuts: **Space** play/pause · **J/K/L** back/stop/forward · **←/→** step ·
**⌥←/⌥→** previous/next keyframe · **Home/End** · **⌘Z/⇧⌘Z** undo/redo ·
**⌘O/⌘S/⇧⌘S/⌘N** open/save/save as/new · **⇧E** node browser.

**Keyframes.** Every numeric parameter has a ◇ next to its name: grey = static,
orange outline = animated, filled = key on this frame. Click it to set or remove
a key at the current frame. Once a parameter is animated, editing it sets a key
at the current frame automatically. Right-click the ◇ for interpolation
(smooth / linear / step), previous/next key, and *Delete animation*. The
selected node's keys are marked on the timeline, and animated nodes show a
small diamond in the graph. Use the Curve Editor tab to retime keys and shape
curves visually. Frame ranges and rates are not animatable.

**Undo/redo** covers every graph edit: nodes, connections, parameters, keys,
moves, renames and the timeline range. A whole gesture (a slider drag, a node
move, typing in a field) is one step; history holds 200 steps.

Rendering happens on a background thread. Each node's result is cached under a
hash of its op, params, inputs and (for time-varying nodes) the frame, so an
edit only re-renders that node and what is downstream of it. Finished viewer
frames are kept in a 1.5 GB LRU, so scrubbing back over frames is instant.

## Use

```sh
# Render a graph: `write` nodes write one frame (--frame, default 1);
# `media_out` nodes write their whole frame range
arclight render examples/basic.json -v

# Generate and render a built-in demo (writes arclight_demo.{json,png})
arclight demo ./out
```

## Graph format

```json
{
  "nodes": [
    { "id": "bg",  "op": "checkerboard", "params": { "width": 640, "height": 480 } },
    { "id": "fg",  "op": "read",         "params": { "path": "plate.exr" } },
    { "id": "cc",  "op": "grade", "inputs": ["fg"], "params": { "multiply": [1.1,1.0,0.9] } },
    { "id": "out", "op": "merge", "inputs": ["cc","bg"], "params": { "operation": "over" } },
    { "id": "w",   "op": "write", "inputs": ["out"], "params": { "path": "comp.png" } }
  ]
}
```

Any numeric param can be keyframed by replacing its value with a curve —
`{ "keys": [[frame, value], ...], "interp": "smooth" | "linear" | "step" }` —
where each value has the param's normal shape. Curves hold before the first
and after the last key; `smooth` is a cubic Hermite with automatic tangents
(slope across the neighbouring keys, flat at the ends). A key may carry a third
element with user tangents — `[frame, value, { "in": s, "out": s, "broken": true }]`
— slopes in value units per frame, shaped like the value, `null` = automatic.

A node's `inputs` are the ids of upstream nodes, one per input port (`""` =
unconnected). `write` passes its input through unchanged, so it can sit
anywhere in the graph. The GUI also stores a `pos` per node and a top-level
`project` (`first_frame`, `last_frame`, `fps`, `format` {`width`, `height`},
default 1920×1080); the engine ignores `pos`. Sources with no input
(`constant`, `checkerboard`, shapes without a background) whose `width` /
`height` is 0 (the default for new nodes) take the project format. Images
carry a frame (format) and a data window, the part where pixels are stored,
which can be smaller than the frame (a shape stores only its bounds) or reach
past it (a layer moved off-frame keeps its pixels, up to one frame size
beyond each edge).

## Operators

Ops marked "+ mask" have an optional `mask` input and a `mix` param: the
result is blended back toward the main input (B for merge/keymix) by
`mix × mask alpha`. With no mask, `mix` applies everywhere.

| op | inputs | key params | notes |
|----|--------|-----------|-------|
| `media_in` | 0 | `path`, `first_frame`, `last_frame` | still, sequence (`shot.####.exr` / `shot.%04d.exr`) or video (via ffmpeg); holds outside the range |
| `read` | 0 | `path` | `.exr` = linear float; others decoded from sRGB 8-bit |
| `constant` | 0 | `width`, `height`, `color` [r,g,b,a] | solid fill (color is straight/unassociated) |
| `checkerboard` | 0 | `width`, `height`, `size`, `color_a`, `color_b` | test pattern |
| `grade` | 1 + mask | `multiply` [3], `offset` [3], `gamma` [3] | lift/gain/gamma on unassociated color |
| `exposure` | 1 + mask | `stops` | ×2 per stop |
| `saturation` | 1 + mask | `saturation` | about Rec. 709 luminance; 0 = grayscale |
| `invert` | 1 + mask | `channels` (`rgb`/`alpha`/`rgba`) | on unpremultiplied values |
| `clamp` | 1 + mask | `min`, `max`, `channels` | alpha always kept in 0–1 |
| `premult` / `unpremult` | 1 | — | color × alpha / color ÷ alpha |
| `channel_select` | A, B (optional) | `red`, `green`, `blue`, `alpha` | each from `A red` … `B alpha`, `zero`, `one` |
| `merge` | A, B + mask | `operation` | `over`, `under`, `add`, `multiply`, `screen`, `min`, `max`; outside the mask = B |
| `keymix` | A, B + mask | — | A where mask alpha is 1, B where 0 |
| `keyer` | input, clean plate (opt) + mask | `screen` (color, or null = estimated from the image), `balance`, `clip_black`, `clip_white`, `despill`, `output` (`keyed`/`matte`/`despill only`) | green/blue screen color-difference key with despill; a clean plate gives a per-pixel screen color |
| `dissolve` | A, B | `amount` | cross-fade A → B |
| `switch` | 5 (optional) | `which` | passes input `which` through; keyframe it to cut |
| `transform` | 1 + mask | `translate` [2], `scale` [2], `rotate` (deg), `center` [2] or null | affine, bilinear resample, about `center`; null (the default) pivots on the middle of the input's data window, so a Transform after a shape turns it in place; `motion_blur`, `shutter`, `shutter_offset` as for shapes (the animated warp, averaged over the shutter) |
| `corner_pin` | 1 + mask | `to1`–`to4` [2], `from1`–`from4` [2] | perspective warp taking the From quad onto the To quad; `motion_blur`, `shutter`, `shutter_offset` as for shapes (the animated warp, averaged over the shutter) |
| `crop` | 1 | `left`, `top`, `right`, `bottom`, `mode` | `clear outside` or `shrink canvas` |
| `resize` | 1 | `width`, `height`, `fit` (`stretch`/`fit`/`fill`) | box-filtered down, bilinear up |
| `blur` | 1 + mask | `radius`, `passes` | separable box blur; 3 passes ≈ gaussian |
| `rectangle` / `ellipse` | background (optional) | `center` [2], `size` [2], `rotate`, + shape params | rotate is clockwise on screen |
| `polygon` | background (optional) | `points` [x0,y0, x1,y1, …], `fill_rule` (`non-zero`/`even-odd`), + shape params | the whole point list animates as one value |
| `roto` | background (optional) | `points` [x, y, in dx, in dy, out dx, out dy, …], `fill_rule`, + shape params | one closed bezier; handles are offsets from their vertex, so moving a vertex carries them; the whole list animates as one value |
| `tracker` | 1 | `track1`–`track4` [2] + `track<i>_on`, `pattern`, `search` (box sizes, px), `min_score`, `update` (`first frame`/`every frame`), `motion` (`translate`/`translate, rotate`/`translate, rotate, scale`: what the matcher follows besides position), `output` (`input`/`match-move`/`stabilize`), `solve` (`translate`/`translate, rotate`/`translate, rotate, scale`), `reference_frame` | the panel's Track buttons key each track's position per frame (and, when Track motion follows rotation, its turn and growth in `track<i>_warp` = [degrees, scale], so one track can solve rotation and scale) (NCC on luminance, sub-pixel); the output applies or removes the motion relative to the reference frame; **To Transform: Match-move / Stabilize** adds a Transform keyed with that motion on every tracked frame, to move another layer with the shot (or steady it); with all four tracks on, **To Corner Pin** adds a Corner Pin that follows them in perspective; `motion_blur`, `shutter`, `shutter_offset` blur the match-move/stabilize output over the shutter, as for Transform |
| `write` | 1 | `path` | `.exr` = float; others = 8-bit sRGB |
| `media_out` | 1 | `path`, `first_frame`, `last_frame`, `fps` | renders a range to a sequence, `.mp4` (H.264) or `.mov` (ProRes 422 HQ) |
| `viewer` | 1 | — | pass-through; marks what the GUI viewer shows |

Shape params, shared by `rectangle`, `ellipse`, `polygon` and `roto`: `width`,
`height` (the frame when there's no background, 0 = the project format;
with one, its frame is used), `color` (straight), `opacity`, `feather` (soft-edge width in pixels,
centered on the outline), `invert`, `operation` (`over` draws the shape over
the background; `subtract` cuts it out of the background), `motion_blur`
(samples per frame, 0 = off), `shutter` (frames the shutter is open,
default 0.5) and `shutter_offset` (`centered`/`start`/`end`): with motion
blur on, an animated shape is drawn at each sample time across the shutter
and the coverage averaged. Edges are
anti-aliased from the signed distance to the outline, so the alpha is a
clean matte for any `mask` input.

## Scripting and automation

Every front end edits graphs through one engine API (`session.rs`), so
validation, undo/redo and error messages are the same everywhere. A running
GUI also listens on a local socket (`bridge.rs`), so scripts and AI agents can
**attach** to it and edit the open project live. Their edits land in the GUI's
undo history. Renders they request run in the background. The menu bar
shows their progress and a Cancel button, and the window stays usable.

### MCP server (AI agents)

```sh
arclight mcp [graph.json]                 # JSON-RPC 2.0 over stdio, one message per line
arclight mcp --http 127.0.0.1:7878        # JSON-RPC over HTTP POST (/mcp)
arclight mcp --attach [PID]               # drive the newest (or given) running GUI
```

Register it with an MCP client, e.g. `claude mcp add arclight -- /path/to/arclight mcp`.
Tools: `list_ops`, `describe_op`, `get_graph`, `describe_node`, `create_node`,
`delete_node`, `rename_node`, `set_params`, `set_key`, `export_tracker` (a Tracker's motion as a keyed Transform or Corner Pin), `connect`, `disconnect`,
`set_project`, `render_frame` (stats, optional file and PNG preview the model can
see), `render_output`, `probe_media`, `new_graph`, `load_graph`, `save_graph`,
`undo`, `redo`, `list_plugins`, `list_instances`, `attach_instance`,
`detach_instance`. A few tools only work on an attached GUI: `select_node`
(selects a node, opens its properties and scrolls the node graph to it),
`view_node` (shows a node in the viewer, optionally at a frame) and
`get_view` (what the user is looking at: selection, viewed node, frame,
playing). Invalid calls come back as tool errors (`isError: true`) with
a message naming the bad param or node. Protocol errors use the standard
JSON-RPC codes.

### Python (`arclt`)

```sh
python/build.sh
PYTHONPATH=python python3
```

```python
import arclt

bg   = arclt.create_node("checkerboard", width=960, height=540)
sun  = arclt.create_node("ellipse", width=960, height=540, center=[480, 270],
                         size=[260, 260], color=[1, 0.45, 0.1, 1], feather=30)
comp = arclt.create_node("merge", sun, bg)          # inputs by port: A, B
comp["operation"] = "screen"                         # params validated on set
sun.set_key("center", 1, [200, 270]).set_key("center", 48, [760, 270])
arclt.render(comp, frame=24, path="/tmp/comp.exr")  # -> size + mean/min/max color
img = arclt.to_numpy(comp, 24)                      # (h, w, 4) float32, needs numpy
arclt.save("/tmp/comp.json")

arclt.attach()        # from now on, edit the project open in the GUI
```

Also: `node`, `nodes`, `delete`, `connect`, `disconnect`, `load`, `new`,
`pixels`, `ops`, `op_info`, `project`, `undo`, `redo`, `metadata` (media file
info), `graph`, `plugins`, `instances`, `detach`, and, when attached,
`select`, `view(node, frame=None)`, `view_state`. `Node` has `info`,
`params`, `set`, `input(s)`, `set_input`, `outputs`, `rename`, `render`,
`select`, `view`, `to_transform` / `to_corner_pin` (for a Tracker).
Errors raise `arclt.ArcltError`. Tests:
`PYTHONPATH=python python3 -m unittest discover -s python/tests`.

### Native plugins

Plugins are shared libraries with a small C ABI
([`include/arclight_plugin.h`](include/arclight_plugin.h)). A plugin declares its ops
as JSON (ports, typed params, category, whether it varies per frame) and gets
float RGBA buffers to process. Plugin ops show up in the node menus with
generated property panels, can be keyframed, are cached, and can be scripted.
`plugins/example` (Rust) adds **Vignette** and **Film Grain**.

Files must be named `libarclight_plugin_*.so` or `*.arclight.so`. Arclight
searches `$ARCLIGHT_PLUGIN_PATH`, the `plugin_paths` setting,
`~/.local/share/arclight/plugins`, `/usr/local/lib/arclight/plugins` and
`/usr/lib/arclight/plugins`. Run `arclight plugins` to see the search path and
what loaded, and `arclight ops [OP]` to see the op catalog.

### Settings and files (XDG)

| What | Where |
|------|-------|
| Settings | `$XDG_CONFIG_HOME/arclight/settings.json`: `plugin_paths`, `frame_cache_mb` (GUI frame cache, default 1536), `mcp_http_addr` |
| User plugins | `$XDG_DATA_HOME/arclight/plugins/` |
| Running-GUI registry and sockets | `$XDG_RUNTIME_DIR/arclight/` |

## I/O formats

- **EXR**: full 32-bit float, assumed linear + premultiplied.
- **PNG / JPEG / TIFF / …** (via the `image` crate): decoded from / encoded to
  8-bit sRGB; straight alpha on disk, premultiplied internally.

## Layout

```
src/
  lib.rs           engine library
  main.rs          CLI (clap) + demo + tests
  anim.rs          keyframe curves: evaluation, interpolation, key editing
  graph.rs         graph model, topo sort, cached frame-aware Evaluator
  ops.rs           operator registry (inputs, params, defaults) + implementations
  media.rs         image sequences, ffmpeg video decode/encode
  render.rs        frame-range rendering for media_out
  track.rs         2D tracking (NCC matching, tracking runs) and similarity solves
  shape.rs         polygon coverage (anti-aliasing, feather, fill rules), shape outlines, bezier flattening/splitting
  image.rs         Image raster type, bilinear sampling, file I/O
  color.rs         Rgba pixel, sRGB and premultiply conversions
  session.rs       headless editing API: validated edits, undo/redo, rendering
  mcp.rs           MCP server (JSON-RPC 2.0 over stdio / HTTP) and its tools
  bridge.rs        running-GUI registry + Unix-socket bridge for live scripting
  plugin.rs        native plugin host (C ABI, dynamic op registration)
  config.rs        XDG paths and settings.json
  gui/
    main.rs        app shell: layout, menus, shortcuts, file drop
    doc.rs         document model, graph edits, undo/redo history
    node_editor.rs node graph canvas
    curve_editor.rs keyframe curve editor
    viewer.rs      image viewer
    overlay.rs     viewer handles for the selected node (gizmos, boxes, vertices)
    transport.rs   player controls + timeline
    properties.rs  per-node property panels, render jobs
    worker.rs      background evaluation thread + frame cache
include/arclight_plugin.h  plugin C ABI
plugins/example/  example native plugin (Vignette, Film Grain)
python/           `arclt` Python module: Rust core (src/lib.rs) + Python API (arclt/)
examples/
  basic.json        a small sample graph
  media/testsrc.mp4 5 s test clip used by the GUI starter project
```

## Roadmap ideas

- Bounding-box / region-of-interest propagation so ops only touch covered pixels
- Tiled, cached evaluation for large plates
- More ops: color-space nodes, keyer, defocus, time/sequence I/O
- Multi-select of nodes; a normalized view and key copy/paste in the curve editor
- GPU display transform / OCIO in the viewer

## License

MIT OR Apache-2.0.
