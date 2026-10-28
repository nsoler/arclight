# Arclight — handoff notes

Last updated 2026-10-27 (end of session 3). Read this, then `README.md`
(user-facing features, file format, shortcuts).

## Where things stand

- Repo: `github.com/nsoler/arclight` (private). Local:
  `~/Projects/arclight-compund/arclight/` (the parent folder name really is
  spelled `compund`).
- **Branches:** work happens on `wip-ns-effects` (tracks
  `origin/wip-ns-effects`). At the end of session 3 everything was pushed
  and `main` fast-forwarded to match it, so `main`, `wip-ns-effects` and
  both remotes are at the same commit. Push and fast-forward `main` only
  when the user asks (they have asked after each milestone so far).
- ~9,600 lines of Rust, 69 tests passing (`cargo test`), clippy clean apart
  from two old style lints (`Graph::from_str` name, `chunks_exact_mut(4)` in
  media.rs).
- **Done:** Phase 0.1–0.5 (Shift+E browser, optional inputs, generic
  mask/mix, Switch ports, viewer overlays), Phase 1 (simple nodes), Phase 2
  (Rectangle, Ellipse, Polygon, Roto), Transform layer dragging in the viewer,
  and Phase 3's 2D Tracker (NCC with rotation/scale, match-move, stabilize).
- **Not done:** Phase 0.6 (bounding boxes / format), tracker follow-ups,
  Phase 2 extras, Phase 4 (3D). See "Next steps".
- Development is on Linux (Omarchy/Hyprland): use `sed -i` (not
  `sed -i ''`); ffmpeg/ffprobe are in `/usr/bin`. Hyprland tiles the window
  narrower than its 1600 px default, so screenshots come out ~920–1030 px
  wide, and at that width the viewer toolbar text overlaps (layout issue,
  not yet fixed).

```sh
cargo run --release --bin arclight-gui        # GUI with the animated starter project
cargo run --release --bin arclight-gui g.json # GUI with a project file
cargo test                                    # engine + GUI unit tests
cargo run --release -- render graph.json      # CLI render (media_out ranges, write frames)
```

**Originality rule.** The parent folder `arclight-compund/` holds a Nuke
disassembly. It is not to be used as a reference or copied from, and it is
not in the repo. Build every new node from public, standard algorithms
(Porter-Duff, NCC template matching, scanline rasterization, etc.). Prefer
descriptive node names (e.g. "Channel Select", "Shape") over another product's
node names and panel layouts. The goal is comparable *capability*, not a clone.

## Next steps (suggested order)

The user's overall goal is a full compositing toolkit: 2D/3D trackers, masks,
roto, shapes, channel tools, switches. Ask which of these to take next; in
rough order of value:

1. **Tracker follow-ups** (small to medium, builds on fresh code):
   - **Export to a Transform node**: copy the solved per-frame transform
     into keys on a new Transform's `translate`/`rotate`/`scale`, so the
     motion can drive other layers (no expressions needed). Note Transform
     pivots on the image center; convert the affine accordingly.
   - **Use a single track's rotation/scale.** `track()` already estimates a
     per-track `Warp { angle, scale }` but discards it; keying it (e.g.
     hidden `track<i>_warp` params) would let one track solve rotate/scale.
   - **Corner pin** from 4 tracks: needs a perspective (homography) warp op.
   - Per-track box sizes; per-frame match score (show weak frames on the
     timeline or curve); "clear keys after/before this frame"; an offset
     point (track one spot, place the result at another).
   - The overlay draws tracks in input coordinates, also when the output is
     match-moved/stabilized, so the boxes look offset over that output.
     Either draw through the solved transform or show input while tracking.
2. **Phase 2 extras for Roto/Polygon:** box-select of vertices, select all
   (Ctrl+A in the viewer), move/scale/rotate a whole shape (a transform box
   around the selection), motion blur later.
3. **Phase 0.6 bounding boxes / format.** Images are always full frame from
   origin 0,0. Overlays assume a node's input is the viewed image's size
   (Transform's pivot and layer frame, Tracker boxes), which is wrong for a
   smaller layer placed over a bigger background. Also matters for
   performance and Crop/Reformat semantics.
4. **Small UX debts:** viewer toolbar overlap at narrow widths; no warning
   when closing the window with unsaved changes.
5. **Phase 4: 3D** — plan separately (see below).

## Progress log

- **Session 1:** engine, GUI, undo/redo, keyframes (`04a2f3a`), curve editor
  (`9f121e9`), tangent handles (`d4f7e45`), these notes (`3dc833a`).
- **Session 2, part 1** (`8892866`): Phase 0.1–0.4 and Phase 1. Engine:
  `Port { label, optional }`; ops get `&[Option<&Image>]`;
  `OpInfo::mask_base` + the shared `MASK` port / `MIX` param give any op a
  generic mask/mix applied in `ops::eval` (ops never handle masks
  themselves). New `Category::Channel`. Ops: exposure, saturation, invert,
  clamp, premult, unpremult, channel_select, switch (5 optional ports),
  dissolve, keymix, crop, resize. GUI: Shift+E node browser
  (`NodeEditor::open_browser` / `browse()`), hollow optional ports.
- **Session 2, part 2** (`df5b86e`): Phase 0.5 viewer overlays.
  `ops::Overlay` kind per op names the params its handles edit;
  `gui/overlay.rs` draws, hit-tests and writes via `Doc::edit_param`
  (auto-key); the viewer calls `Overlays::interact` before panning and
  `Overlays::draw` after the image. Transform gizmo and Crop box. Positive
  `rotate` is **clockwise** on screen (y down); flipping it would alter
  existing projects, so ask the user first.
- **Session 3, part 1** (`bc87236`): shape generators. `src/shape.rs`
  rasterizes polygons to coverage from the *signed distance to the outline*
  (smoothstep ramp: 1 px = anti-aliasing, wider = feather centered on the
  edge) with scanline crossings for inside/outside (non-zero / even-odd).
  Ops `rectangle`, `ellipse`, `polygon` share `ShapeStyle` + `shade()`:
  optional `background` input sets the canvas, `operation` over/subtract
  (chain shapes for holes), `invert`, `opacity`, `feather`. New
  `ParamKind::Bool` and `ParamKind::Points { stride }` (flat array, animates
  as one value). Overlays `Box` and `Points`; the overlay grabs on *press*
  so clicks can select/insert vertices (`Overlays::press/drag_to` are what
  tests drive). Vertex insert/delete goes through `anim::edit_all` (applied
  to every key). ⌫ deletes selected vertices before the node.
- **Session 3, part 2** (`f8430e5`): `roto`, one closed bezier as
  `Points { stride: 6 }` (x, y, in dx/dy, out dx/dy; handles relative to the
  vertex). `shape::spline` flattens adaptively (≤ 0.05 px) into the same
  `coverage` + `shade()`; `shape::split_cubic` is de Casteljau. Overlay:
  `Geo::Points.tan`, `Part::Tangent { i, out }` (smooth by default, Alt
  breaks), `Part::Pull(i)` (Alt-drag a vertex or Ctrl-click-drag a new one).
  Tangents are live only for selected vertices; an edge click splits the
  curve in every key without changing its shape.
- **Session 3, part 3** (`264df6f`): the Transform overlay draws the
  transformed layer's frame (`layer_frame`); drag inside it to move
  (`in_quad`), corners scale uniformly (⇧ free), edge midpoints one axis.
  Hit order: axis handles, center, frame handles, ring, inside. Left-drag
  over a selected Transform's layer moves instead of panning (middle-drag
  pans).
- **Session 3, part 4** (`9b0a0ad`): 2D Tracker, see "Tracker internals".
- **Session 3, part 5** (`58386cd`): rotation/scale-aware matching. The
  translation-only version drifted ~6 px over 48 frames on a corner turning
  25° ("every frame") or lost it after ~8° ("first frame"); now within
  ~0.1 px on that shot.

## Architecture in one page

Engine (`src/`, library crate `arclight`):

| File | Role |
|------|------|
| `ops.rs` | **Op registry** `OPS: &[OpInfo]` (name, label, `Category`, input `Port`s, `ParamSpec`s with defaults, `mask_base`, viewer `Overlay` kind, help) + every op's implementation. `eval(op, params, inputs, ctx)` dispatches and applies the generic mask/mix. `resolve_params` evaluates keyframes before an op runs (and, for `tracker`, adds the reference-frame positions); `is_time_varying` marks ops whose output depends on the frame by itself. `warp` resamples by an affine. |
| `graph.rs` | `Graph { nodes: Vec<NodeSpec>, project }`. `Evaluator` caches each node's result under a hash of (op, *resolved* params, input keys, frame if time-varying). `Evaluator::key` predicts a cache key without rendering. Errors are `NodeError { node, message }`. |
| `anim.rs` | Keyframe curves stored inline in params: `{"keys": [[f, v], [f, v, {in,out,broken}]], "interp": ...}`. Cubic Hermite with auto/user tangents. `set_key`, `edit_all` (structural edit of every key), `sample`, `key_frames`, ... Any numeric array param can be animated. |
| `image.rs`, `color.rs` | `Image { width, height, pixels: Vec<[f32;4]> }`: **RGBA only, linear, premultiplied, no bounding box / offset**. `sample_bilinear` puts pixel centers on integers (shapes, overlays and tracking use i + 0.5). |
| `shape.rs` | Coverage of polygons (anti-aliasing, feather, fill rules), rectangle/ellipse outlines, bezier splines (`spline_segments`, `spline`, `cubic_at`, `split_cubic`). |
| `track.rs` | 2D tracking: `Luma`, `Pattern`, `Warp`, `find`, `track` (a run), `input_frames`, `positions`, and the similarity `solve` / `Affine` helpers. |
| `media.rs` | Sequence patterns (`####`, `%04d`), range detection, ffmpeg decode (persistent per-file decoder, frame-accurate seek) and encode. |
| `render.rs` | Frame-range rendering of `media_out`; `Progress` (done/total/cancel) is shared with tracking. |

GUI (`src/gui/`, binary `arclight-gui`, eframe/egui 0.37):

| File | Role |
|------|------|
| `main.rs` | App shell: panel layout, menus, keyboard shortcuts (`shortcuts()`), file drop, starter graph, `ARCLIGHT_SCREENSHOT` hook. Commits undo once per gesture; an active tracking run counts as a gesture. |
| `doc.rs` | `Doc`: the graph plus every edit (`add_node`, `connect`, `set_param`, `edit_param` with auto-key, ...). **Undo** is whole-graph snapshots, committed once per finished gesture. Mutate the graph only through `Doc` methods that call `changed()` or `touched()`. |
| `node_editor.rs` | Canvas: nodes, ports (hollow = optional), wires, pan/zoom, right-click add menu, Shift+E browser; both place nodes via `place`. |
| `properties.rs` | Property panels generated from `ParamSpec`s; keyframe diamonds; media extras; render jobs (`RenderJob`); tracking (`tracker_extras`, `TrackJob`, `collect_tracks`). |
| `viewer.rs` | Shows the image; pan/zoom; channels; exposure; pixel readout. Hosts the overlays. |
| `overlay.rs` | Handles for the selected node's `Overlay` kind: `Xf` (image↔screen), `geo` (resolve at the frame), `hit`, `press`, `apply` (drag → `edit_param`), `draw`. Add new handle kinds here. Positions use the viewed image's size (no bbox yet). |
| `transport.rs`, `curve_editor.rs`, `worker.rs` | Player + timeline; keyframe curves with tangent handles; background eval thread + 1.5 GB frame LRU. |

### Adding a node today (checklist)

1. `ops.rs`: add an `OpInfo` to `OPS` (ports via `IN` / `req()` / `opt()`,
   params via `p!` / `fixed!`). For mask support, end the ports with `MASK`,
   the params with `MIX`, and set `mask_base: Some(<input to mix back to>)`.
   For viewer handles, set `overlay` to an `Overlay` kind naming its params
   (`Overlay::None` otherwise); a new kind needs a `Geo` variant plus `hit`,
   `apply` and `draw` arms in `gui/overlay.rs`.
2. `ops.rs`: add a `#[derive(Deserialize)]` params struct and a function; add a
   match arm in `eval_op`. Defaults come from the spec, so the struct needs none.
3. If it depends on the frame by itself, add it to `is_time_varying`. If its
   output depends on params at *other* frames (like the Tracker's reference
   frame), add those values in `resolve_params` so the cache key sees them.
4. Add an engine test (`src/main.rs` tests, or a module test), and overlay
   tests in `gui/overlay.rs` (`gesture` / `drag` helpers) for new handles.
5. Nothing else is needed in the GUI: the add menu, properties panel,
   keyframing and curve editor pick it up from the registry automatically.

### Tracker internals

- **Params** (`tracker` op): four fixed tracks (`track<i>_on` Bool +
  `track<i>` Vec2, keyed per tracked frame), shared `pattern` / `search` box
  sizes (px), `min_score`, `update` (`first frame` / `every frame`),
  `motion` (`translate` / `translate, rotate` (default) /
  `translate, rotate, scale`), `output` (`input` / `match-move` /
  `stabilize`), `solve` (what the output transform contains), and
  `reference_frame` (where the output is identity).
- **Matching** (`track::find`): the *target* window is sampled through the
  track's current `Warp` (`Luma::window(c, r, &Mat)`), so the first-frame
  pattern keeps matching a turning or growing feature. Steps: integer NCC
  search with the predicted warp (summed-area tables, rayon over offsets),
  coarse angle scan ±6° (and scale ±4%), a ±2 px re-search, then 4 rounds
  of per-parameter parabola `refine` with halving steps (a step may move a
  full step, else the coarse residual can't close). Windows flatter than
  `FLAT` score 0: rounding noise in flat areas otherwise scored > 1 and won.
  "Every frame" re-takes the pattern through the current warp, so it keeps
  the first orientation.
- **Runs** (`track::track`): either direction, constant-velocity prediction
  of position, angle and scale; a track is lost below `min_score` or
  off-image. `input_frames` renders the tracker's input with its own
  `Evaluator` on a cloned graph.
- **GUI:** `tracker_extras` starts a `TrackJob` thread; it pushes per-frame
  positions to a `Found` queue that `collect_tracks` drains every UI frame
  (keying positions, seeking the playhead to follow). It runs for any
  tracker, open panel or not, and checks "finished" before draining so the
  last frame isn't lost. Overlay: `Geo::Tracker`, `Part::Track` (drag inside
  the pattern box), `Part::TrackBox` (nearest box corner), path drawn from
  the key frames.
- **Output:** `track::solve` is a least-squares similarity (centroids +
  dot/cross sums) from reference to current positions; match-move samples
  through its inverse, stabilize through it.
- **Accuracy checks:** `track.rs` tests use synthetic gaussian-blob images
  moved by a known affine (`blobs_moved`) and compare against the truth:
  sub-pixel shift < 0.02 px; 40° rotation < 0.1 px; rotation + 49% growth
  < 0.15 px. `src/main.rs` tracks a shape through a real graph (< 0.05 px).

## Later phases

### Phase 0.6: bounding boxes / format

Give `Image` a data window (offset + size) and a format, so layers can be
smaller than the frame. Then fix overlays to use each node's input size
rather than the viewed image's (Transform pivot/frame, Tracker boxes).

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
  or treat it as a research spike before committing to it. `track.rs`'s NCC
  matcher can seed it but isn't a feature detector.

### Engine changes that would make the above easier

- **Arbitrary channels** in `Image` (named planes such as rgba, mask,
  depth, motion) instead of fixed `[f32; 4]`. A big refactor; do it before
  3D.
- **Expressions / param linking** (e.g. a Transform following a Tracker
  live, instead of copied keys).
- **ROI / tiled evaluation** once images get large or graphs get deep.

## Known issues and gaps

- Interactive behaviour has mostly been verified through unit tests and
  screenshots, not by hand: wire dragging, file dialogs, the Render button,
  curve-key and tangent dragging, box select, roto tangent editing, the
  Transform layer frame, tracker box dragging. Worth a manual pass.
- Overlays assume the node's input is the viewed image's size (no bbox).
- Tracker boxes are drawn in input coordinates even over a
  match-moved/stabilized output.
- Viewer toolbar text overlaps when the window is narrow.
- First frame after launch takes ~1.5 s (ffprobe + ffmpeg start); later
  frames are ~5 ms for the starter graph.
- Closing the window with unsaved changes doesn't warn (New/Open do).
- Viewer display conversion is on the CPU (rayon); fine to ~4K.
- Video decode is sequential-friendly: playing backward re-seeks every frame
  (slow for long-GOP files; the frame cache hides it on second pass).

## Working tips

- **Screenshots without screen-capture permission:**
  `ARCLIGHT_SCREENSHOT=/path/out.png ARCLIGHT_SCREENSHOT_DELAY=3 ./target/release/arclight-gui [graph.json]`
  saves the window and quits. For a specific UI state, add temporary
  env-var hooks (selection, a button press such as starting a track run,
  a debug `eprintln!` of param values) marked `// TEMPHOOK`, then remove
  them with `sed -i '/TEMPHOOK/d' src/*.rs src/gui/*.rs` and confirm
  `grep -rn TEMPHOOK src` is empty.
- **Test scenes:** hand-written graph JSON (nodes without `pos` are laid
  out automatically) is the quickest way to set up a case, e.g. a
  `constant` → animated `rectangle` (keyed `center`/`rotate`) → `tracker` →
  `viewer`. Keyed shapes give exact ground truth to compare tracks against.
- **Timing-sensitive tests:** run the tracker tests in release
  (`cargo test --release --lib track`) when iterating; debug is fine for CI.
- egui 0.37 differs from older docs: `Panel::left/right/top/bottom`,
  `App::ui(&mut self, ui, frame)`, `Id::unique`, `ui.close()`,
  `MenuBar::new().ui(...)`. When unsure, read the source under
  `~/.cargo/registry/src/*/egui-0.37.0/src/`.
- Keyboard shortcuts must skip when a text field has focus
  (`ctx.text_edit_focused()`), and `consume_shortcut` ignores an extra Shift:
  check Shift variants before plain ones.
