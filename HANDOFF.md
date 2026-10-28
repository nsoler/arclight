# Arclight — handoff notes

Last updated 2026-10-27 (session 3, after the shape generators). Read this, then `README.md`
(user-facing features, file format, shortcuts).

## Where things stand

- Repo: `github.com/nsoler/arclight` (private). Local:
  `~/Projects/arclight-compound/arclight/`.
- **Active branch: `wip-ns-effects`** (tracks `origin/wip-ns-effects`). All
  session-2 and session-3 work is there; `main` is still at `3dc833a` (session 1). Merge
  to `main` only when the user asks.
- Done: Phase 0.1–0.5 (Shift+E browser, optional inputs, generic mask/mix,
  Switch ports, viewer overlays), all of Phase 1, and the Phase 2 shape
  generators (Rectangle, Ellipse, Polygon). Not done: Roto (rest of Phase 2),
  Phase 0.6 (bounding boxes / format), Phases 3–4.
- ~8,100 lines of Rust, 50 tests passing, clippy clean apart from two old
  style lints (`Graph::from_str` name, `chunks_exact_mut(4)` in media.rs).
- **Next:** Roto (Phase 2, below).
- Development moved from macOS to Linux (Omarchy/Hyprland) in session 3:
  use `sed -i` (not `sed -i ''`); ffmpeg is in `/usr/bin`. Hyprland tiles the
  window narrower than its 1600 px default, so screenshots come out ~1030 px
  wide, and at that width the viewer toolbar text overlaps (layout issue, not
  yet fixed).

## Progress log

- **Session 1:** engine, GUI, undo/redo, keyframes (`04a2f3a`), curve editor
  (`9f121e9`), tangent handles (`d4f7e45`), these notes (`3dc833a`), on `main`.
- **Session 2, part 1** (`8892866`, branch `wip-ns-effects`): Phase 0.1–0.4
  and Phase 1. Engine: `Port { label, optional }` replaces `&str` port
  labels; ops get `&[Option<&Image>]`; `OpInfo::mask_base` + the shared
  `MASK` port / `MIX` param give any op a generic mask/mix applied in
  `ops::eval` (ops never handle masks themselves). New `Category::Channel`.
  New ops: exposure, saturation, invert, clamp, premult, unpremult,
  channel_select, switch (5 optional ports), dissolve, keymix, crop, resize.
  GUI: Shift+E node browser (`NodeEditor::open_browser` / `browse()`),
  hollow optional ports, port labels drawn outside the node.
- **Session 2, part 2** (next commit on `wip-ns-effects`): Phase 0.5 viewer
  overlays. `ops::Overlay` kind per op (`Point`, `Transform`, `Insets`) names
  the params its handles edit; `gui/overlay.rs` draws, hit-tests and writes
  via `Doc::edit_param` (auto-key); the viewer calls `Overlays::interact`
  before panning and `Overlays::draw` after the image. Transform gizmo
  (fixed-length handles, ratio-based scale, wrap-free rotation) and Crop box
  wired; `Point` is ready for shapes. Fixed the Transform doc comment:
  positive `rotate` is **clockwise** on screen (y down). Behaviour unchanged;
  flipping it would alter existing projects, so ask the user first.

- **Session 3, part 1** (commit "Add Rectangle, Ellipse and Polygon shape nodes" on `wip-ns-effects`): shape generators.
  `src/shape.rs` rasterizes any polygon list to coverage from the *signed
  distance to the outline* (smoothstep ramp, 1 px = anti-aliasing, wider =
  feather centered on the edge) with scanline crossings for inside/outside
  (non-zero / even-odd). Ellipses are flattened to ~4 px segments. Ops
  `rectangle`, `ellipse`, `polygon` (new `Category::Shape`) share
  `ShapeStyle` + `shade()` in ops.rs: optional `background` input sets the
  canvas, `operation` over/subtract (chain shapes to build holes), `invert`,
  `opacity`, `feather`. New `ParamKind::Bool` (checkbox, not animatable) and
  `ParamKind::Points { stride }` (flat array, animates as one value; the
  properties panel shows a count, the curve editor skips it). New overlays
  `Overlay::Box` (center/size/rotate, ellipse flag) and `Overlay::Points`.
  The overlay now grabs on *press* (not drag start) so clicks can select or
  insert vertices; `Overlays::press/drag_to` are what the tests drive.
  Vertex insert/delete goes through `anim::edit_all`, which applies a
  structural edit to every key (resetting their tangents to auto), so all
  keys keep the same point count. ⌫ deletes selected vertices before it
  would delete the node (`Viewer::delete_points`).

```sh
cargo run --release --bin arclight-gui        # GUI with the animated starter project
cargo test                                    # engine + GUI unit tests
cargo run --release -- render graph.json      # CLI render (media_out ranges, write frames)
```

Needs `ffmpeg`/`ffprobe` on PATH for video (Homebrew install is present).

**Originality rule.** The parent folder `arclight-compound/` holds a Nuke
disassembly. It is not to be used as a reference or copied from, and it is
not in the repo. Build every new node from public, standard algorithms
(Porter-Duff, NCC template matching, scanline rasterization, etc.). Prefer
descriptive node names (e.g. "Channel Select", "Shape") over another product's
node names and panel layouts. The goal is comparable *capability*, not a clone.

## Architecture in one page

Engine (`src/`, library crate `arclight`):

| File | Role |
|------|------|
| `ops.rs` | **Op registry** `OPS: &[OpInfo]` (name, label, `Category`, input `Port`s (label, optional), `ParamSpec`s with defaults, `mask_base`, viewer `Overlay` kind, help) + every op's implementation. `eval(op, params, inputs, ctx)` dispatches (inputs are `Option<&Image>` per port) and then applies the generic mask/mix. `resolve_params` evaluates keyframes before an op runs; `is_time_varying` marks ops whose output depends on the frame by itself. |
| `graph.rs` | `Graph { nodes: Vec<NodeSpec>, project }`. `Evaluator` caches each node's result under a hash of (op, *resolved* params, input keys, frame if time-varying). An edit re-renders only that node and what's downstream. `Evaluator::key` predicts a cache key without rendering (the GUI's frame cache uses it). Errors are `NodeError { node, message }` (the GUI outlines the failing node in red). |
| `anim.rs` | Keyframe curves stored inline in params: `{"keys": [[f, v], [f, v, {in,out,broken}]], "interp": ...}`. Cubic Hermite with auto/user tangents. Key-level API (`keys`, `replace_keys`, `set_tangent`, ...). Any numeric array param can be animated, which matters for shapes (below). |
| `image.rs`, `color.rs` | `Image { width, height, pixels: Vec<[f32;4]> }`: **RGBA only, linear, premultiplied, no bounding box / offset**. sRGB conversion only at 8-bit file I/O. |
| `media.rs` | Sequence patterns (`####`, `%04d`), range detection, ffmpeg decode (persistent per-file decoder, frame-accurate seek) and encode. |
| `render.rs` | Frame-range rendering of `media_out` with a `Progress` (done/total/cancel). Template for any other long background job, e.g. tracking. |

GUI (`src/gui/`, binary `arclight-gui`, eframe/egui 0.37):

| File | Role |
|------|------|
| `main.rs` | App shell: panel layout, menus, keyboard shortcuts (`shortcuts()`), file drop, starter graph, `ARCLIGHT_SCREENSHOT` hook. |
| `doc.rs` | `Doc`: the graph plus every edit (`add_node`, `connect`, `set_param`, `edit_param` with auto-key, ...). **Undo** is whole-graph snapshots, committed once per finished gesture (`commit(gesture_in_progress)`). Mutate the graph only through `Doc` methods that call `changed()` (re-render) or `touched()` (no re-render). |
| `node_editor.rs` | Canvas: nodes, ports (hollow = optional), bezier wires, pan/zoom, right-click add menu (`add_menu`), Shift+E browser (`open_browser`, `browse`); both place nodes via `place`. |
| `properties.rs` | Property panels generated from `ParamSpec`s; keyframe diamonds; media extras; render jobs. |
| `viewer.rs` | Shows the image; pan/zoom; channels; exposure; pixel readout. Hosts the overlays. |
| `overlay.rs` | Handles for the selected node's `Overlay` kind: `Xf` (image↔screen), `geo` (resolve params at the frame), `hit`, `apply` (drag → `edit_param`), `draw`. Add new handle kinds here. Positions use the viewed image's size (no bbox yet). |
| `transport.rs`, `curve_editor.rs`, `worker.rs` | Player + timeline; keyframe curves with tangent handles; background eval thread + 1.5 GB frame LRU. |

### Adding a node today (checklist)

1. `ops.rs`: add an `OpInfo` to `OPS` (ports via `IN` / `req()` / `opt()`,
   params via `p!` / `fixed!`). For mask support, end the ports with `MASK`,
   the params with `MIX`, and set `mask_base: Some(<input to mix back to>)`.
   For viewer handles, set `overlay` to an `Overlay` kind naming its params
   (`Overlay::None` otherwise).
2. `ops.rs`: add a `#[derive(Deserialize)]` params struct and a function; add a
   match arm in `eval`. Defaults come from the spec, so the struct needs none.
3. If it depends on the frame by itself, add it to `is_time_varying`.
4. Add an engine test (`src/main.rs` tests, or a module test).
5. Nothing else is needed in the GUI: the add menu, properties panel,
   keyframing and curve editor pick it up from the registry automatically.

## Next goal: a full set of compositing nodes

Wanted: 2D/3D motion trackers, mask nodes, rotoscoping, polygon shapes with
alpha, premult/unpremult, colour channel selectors, switches. Plus **Shift+E**
to open a node list for placing nodes in the node editor.

Several of these need engine or GUI infrastructure first. Suggested order:

### Phase 0: infrastructure — 0.1–0.5 done, 0.6 open

1. **Shift+E node browser.** A popup at the mouse position in the node editor:
   a search field (focused immediately), filtered list of `OPS` grouped by
   `Category`, arrow keys + Enter to place, Esc to close. It reuses the
   placement logic from `NodeEditor::add_menu` (auto-connects to the selected
   node). Wire it in `main.rs::shortcuts()` with
   `KeyboardShortcut::new(Modifiers::SHIFT, Key::E)`. `E` is currently unused.
   Note that the `pressed()` helper there ignores Shift, so check Shift+E before
   any future plain-`E` binding. Keep the right-click menu too.
2. **Optional inputs.** Today an unconnected port is an error
   (`Evaluator::run`). Add an `optional` marker to input ports (e.g. a
   `&[Port { label, optional }]` instead of `&[&str]`) and pass `Option<&Image>`
   to ops. This is needed for mask inputs and for Switch.
3. **Mask input + mix, generic.** Give filter/colour ops an optional `mask`
   port and a `mix` param, applied in one helper after the op:
   `out = lerp(input, result, mix * mask.alpha)`. One implementation covers
   Grade, Blur, Transform and the rest.
4. **Variable input count** for Switch/Dissolve/multi-merge: either a fixed
   generous port count with optional ports, or ports grown by the editor when
   the last one is connected. The first is simpler and good enough.
5. **Viewer overlays.** An interaction layer in `viewer.rs` that ops can opt
   into. It needs image↔screen transforms (already implicit in `img_rect` and
   `zoom`), handle drawing, and hit-testing. It writes to params through `Doc`
   (with auto-key at the current frame). Roto, shapes, tracker boxes and
   transform gizmos all depend on it. A per-op "overlay kind" in `OpInfo` keeps
   the viewer generic.
6. **Bounding boxes / format** (optional but valuable): images are always full
   frame from origin 0,0. Shapes and trackers are fine without it. It matters
   later for performance and for Crop/Reformat semantics.

### Phase 1: simple nodes — done

- **Premult / Unpremult.** The pipeline is premultiplied internally, so
  Unpremult outputs straight colour (mark it in help: following ops then see
  straight colour) and Premult multiplies colour by alpha. Grade already
  unpremults internally.
- **Channel Select** (colour channel selector / shuffle): per output channel
  R/G/B/A choose a source of R, G, B, A, 0 or 1 from input A or B.
- **Copy channel** (alpha from B into A, etc.).
- **Switch** (int `which`, animatable, so editorial switching can be keyed) and
  **Dissolve** (float mix between two inputs).
- Cheap extras that round out a toolkit: Invert, Clamp, Saturation,
  Exposure, Crop, Reformat/Resize, Keymix (A/B through mask), Constant matte.

### Phase 2: shapes and roto (needs phase 0.5 overlays)

- **Shape generators** (Rectangle, Ellipse, Polygon) producing RGBA with alpha:
  analytic coverage for rect/ellipse; polygon scanline rasterization with
  anti-aliasing (supersample or exact area coverage), even-odd/non-zero fill,
  plus **feather** (blur of the coverage, or distance-based falloff), opacity,
  colour, invert.
- **Roto**: a list of closed bezier splines. Store each shape's points as one
  flat numeric array param (`[x0,y0, in/out tangent handles..., x1,y1, ...]`);
  the existing keyframe system then animates whole shapes for free, with one
  key per frame holding every point. The curve editor will show one channel per
  number. Add a "points" `ParamKind` so the properties panel shows a compact
  summary instead of hundreds of fields, and so channel lists collapse.
  Editing happens in the viewer overlay: add/move points, drag tangents,
  select multiple, move a whole shape. Rasterize by flattening beziers to
  polygons, then the polygon rasterizer above. Per-shape feather, opacity,
  blend mode (add/subtract), motion blur later.
- Outputs alpha (and RGB = colour × alpha), so a Roto feeds mask inputs
  directly.
- **Shapes are done** (see session 3 above). Box-select of vertices isn't.
- **Roto plan:** one closed bezier per Roto node (chain Roto nodes through
  `background` with over/subtract for several shapes and holes). Store it as
  `ParamKind::Points { stride: 6 }`: x, y, then in- and out-handle offsets
  relative to the vertex, so moving a vertex carries its handles. Flatten
  each cubic segment adaptively and reuse `shape::coverage` + `shade()`.
  In `gui/overlay.rs`, `Geo::Points` already handles stride > 2 for
  select/insert/delete/move; Roto needs handle parts (draw + drag the
  selected vertices' tangents, ⌥ to break symmetry) and, on edge insert,
  a de Casteljau split instead of a straight lerp.

### Phase 3: 2D tracking (needs overlays + a background job like `render.rs`)

- **Tracker node**: one or more tracks, each a pattern box + search box placed
  in the viewer. Track forward/backward over the frame range on a background
  thread with progress/cancel (copy the `render.rs` + `properties.rs`
  render-job pattern). Per frame, use normalized cross-correlation of the
  pattern within the search area on luminance, with sub-pixel refinement
  (parabola fit on the NCC peak). Write results as **keyframes** on each
  track's `position` param, so the curve editor can clean up bad frames.
- Outputs: match-move and stabilize transforms (translate from 1 track;
  translate+rotate+scale from 2+ tracks, least-squares similarity). Either
  apply them in the Tracker node itself or export to a Transform node by
  copying/linking keys. Linking needs expressions, a separate feature.
- Corner-pin from 4 tracks is a natural follow-on (needs a perspective warp op).

### Phase 4: 3D (large; plan it separately)

- 3D needs a scene system first: Camera (focal length, film back, transform,
  animatable), Card / geometry nodes, a Scene node, and a **scanline renderer**
  op that projects textured cards through a camera into an image. The image
  model may need depth (a Z channel), which argues for generalizing `Image`
  beyond RGBA first.
- **3D camera tracking** is the hardest item: feature detection + KLT tracking
  across frames, outlier rejection (RANSAC), essential-matrix initialization,
  incremental bundle adjustment for camera path + point cloud, lens distortion.
  Consider existing Rust crates (e.g. nalgebra/argmin for the optimization)
  or treat it as a research spike before committing to it.

### Engine changes that would make the above easier

- **Arbitrary channels** in `Image` (named planes such as rgba, mask,
  depth, motion) instead of fixed `[f32; 4]`. This is a big refactor. Do it
  before 3D, not before phases 1–3, which work with RGBA + alpha masks.
- **Expressions / param linking** (e.g. a Transform following a Tracker).
- **ROI / tiled evaluation** once images get large or graphs get deep.

## Known issues and gaps

- Interactive behaviour has mostly been verified through unit tests and
  screenshots, not by hand: wire dragging, file dialogs, the Render button,
  curve-key dragging, tangent dragging, box select. Worth a manual pass.
- First frame after launch takes ~1.5 s (ffprobe + ffmpeg start); later
  frames are ~5 ms for the starter graph.
- Closing the window with unsaved changes doesn't warn (New/Open do).
- Viewer display conversion is on the CPU (rayon); fine to ~4K.
- Video decode is sequential-friendly: playing backward re-seeks every frame
  (slow for long-GOP files; the frame cache hides it on second pass).

## Working tips

- **Screenshots without screen-capture permission:**
  `ARCLIGHT_SCREENSHOT=/path/out.png ARCLIGHT_SCREENSHOT_DELAY=3 ./target/release/arclight-gui`
  saves the window and quits. For a specific UI state, add temporary
  env-var hooks (selection, frame, tab) marked `// TEMPHOOK`, then remove them
  with `sed -i '' '/TEMPHOOK/d'` and confirm `grep -rn TEMPHOOK src` is empty.
- egui 0.37 differs from older docs: `Panel::left/right/top/bottom`,
  `App::ui(&mut self, ui, frame)`, `Id::unique`, `ui.close()`,
  `MenuBar::new().ui(...)`. When unsure, read the source under
  `~/.cargo/registry/src/*/egui-0.37.0/src/`.
- Keyboard shortcuts must skip when a text field has focus
  (`ctx.text_edit_focused()`), and `consume_shortcut` ignores an extra Shift:
  check Shift variants before plain ones.
