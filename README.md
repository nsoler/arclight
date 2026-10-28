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
| **Viewer** (center) | Shows the input of the first `viewer` node (or the selected node). Drag to pan, scroll/pinch to zoom, double-click or **F** to fit. RGB/R/G/B/A channel isolation, exposure in stops, float pixel readout under the cursor. |
| **Player** (under the viewer) | Go to start, step back, play backward, pause, play forward, step forward, go to end. Scrubbable timeline (green strip = frames already cached), in/out, FPS, loop. Plays in real time and shows the achieved fps. |
| **Curve editor** (bottom, second tab) | The selected node's keyframed params as value-over-time curves, one per channel (x/y, r/g/b/a), with a toggle per channel. Click / Shift-click / box-drag to select keys, drag to move them in time and value, double-click a curve to add a key, **⌫** to delete. Smooth/Linear/Step buttons set interpolation. Selected keys on smooth curves show tangent handles: drag one to set the slope (in and out move together), **⌥**-drag to break the key so each side moves on its own; hollow handles are automatic, filled are user-set. Tangents Auto/Flat/Break-Unify buttons act on the selected keys. Scroll to pan, ⌘-scroll or pinch to zoom (⌥ value only, ⇧ time only), middle-drag to pan, **F** to frame. The ruler scrubs the playhead. |
| **Node graph** (bottom) | Right-click, or press **⇧E** for a searchable node browser (type to filter, ↑/↓ and Enter to place, Esc to close), to add nodes. With a node selected, the new node is wired after it. Hollow ports are optional (e.g. `mask`). Drag from an output to an input (or back) to connect; drag a connected input away to reroute or disconnect. Double-click a node or press **V** to view it. **⌫** deletes (the chain is reconnected). Drop image/video files onto the window to create Media In nodes. |
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
`project` (`first_frame`, `last_frame`, `fps`); the engine ignores `pos`.

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
| `dissolve` | A, B | `amount` | cross-fade A → B |
| `switch` | 5 (optional) | `which` | passes input `which` through; keyframe it to cut |
| `transform` | 1 + mask | `translate` [2], `scale` [2], `rotate` (deg) | affine, bilinear resample, about center |
| `crop` | 1 | `left`, `top`, `right`, `bottom`, `mode` | `clear outside` or `shrink canvas` |
| `resize` | 1 | `width`, `height`, `fit` (`stretch`/`fit`/`fill`) | box-filtered down, bilinear up |
| `blur` | 1 + mask | `radius`, `passes` | separable box blur; 3 passes ≈ gaussian |
| `write` | 1 | `path` | `.exr` = float; others = 8-bit sRGB |
| `media_out` | 1 | `path`, `first_frame`, `last_frame`, `fps` | renders a range to a sequence, `.mp4` (H.264) or `.mov` (ProRes 422 HQ) |
| `viewer` | 1 | — | pass-through; marks what the GUI viewer shows |

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
  image.rs         Image raster type, bilinear sampling, file I/O
  color.rs         Rgba pixel, sRGB and premultiply conversions
  gui/
    main.rs        app shell: layout, menus, shortcuts, file drop
    doc.rs         document model, graph edits, undo/redo history
    node_editor.rs node graph canvas
    curve_editor.rs keyframe curve editor
    viewer.rs      image viewer
    transport.rs   player controls + timeline
    properties.rs  per-node property panels, render jobs
    worker.rs      background evaluation thread + frame cache
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
