# Arclight — handoff notes

Last updated 2026-10-28 (end of session 5). Read this, then `README.md`
(user-facing features, file format, shortcuts, scripting) and
`docs/spec-coverage.md` (what the planning docs ask for vs. what exists).

## Where things stand

- Repo: `github.com/nsoler/arclight` (private). Local:
  `~/Projects/arclight-compound/arclight/`. The planning docs (C++/Qt
  "Linux port" plans that session 4 implemented in Rust) are in
  `~/Projects/arclight-compound/Archlight-Development-Documentationm/`.
- **Branches:** work happens on `wip-ns-effects` (tracks
  `origin/wip-ns-effects`). **Standing instruction from the user:** when a
  milestone is finished (tests and clippy pass, README/HANDOFF updated),
  commit it, push `wip-ns-effects`, and fast-forward `main` to it and push
  `main`, without asking first. Then carry on with the next step. Keep
  `main`, `wip-ns-effects` and both remote branches at the same commit.
- ~13,300 lines of Rust + Python. Tests: 71 library + 10 CLI + 31 GUI
  (`cargo test --release`) and 9 Python (see "Working tips"), all passing.
  Clippy is clean apart from two old style lints (`Graph::from_str` name,
  `chunks_exact_mut(4)` in media.rs).
- **Done:** Phase 0.1–0.5 (Shift+E browser, optional inputs, generic
  mask/mix, Switch ports, viewer overlays), Phase 1 (simple nodes), Phase 2
  (Rectangle, Ellipse, Polygon, Roto), Transform layer dragging in the viewer,
  and Phase 3's 2D Tracker. Session 4: MCP server, Python `arclt` module,
  live GUI scripting bridge, native plugin host, XDG config (from the
  planning docs). Session 5 finished the tracker follow-ups: export to a
  keyed Transform or a new Corner Pin node, one-track rotation/scale, boxes
  drawn through the tracker's output, match scores (weak frames on the
  path, timeline and panel), clear keys before/after, per-track box sizes,
  offset points. See "Progress log".
  Session 6: Roto/Polygon box select, select all and transform box; node
  graph insert-into-wire, shake-to-disconnect and minimap; Phase 0.6
  (formats, data windows, overscan, project format; plan and measurements
  in `docs/phase-0.6-plan.md`).
- **Not done:** small UX debts, session-4 follow-ups, Phase 4 (3D). See
  "Next steps".
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
cargo run --release -- mcp [--http ADDR] [--attach [PID]]   # MCP server
cargo run --release -- plugins                # plugin search path + what loaded
cargo build --release -p arclight-plugin-example && python/build.sh  # plugin + Python module
```

**Originality rule.** The parent folder `arclight-compound/` holds a Nuke
disassembly. It is not to be used as a reference or copied from, and it is
not in the repo. Build every new node from public, standard algorithms
(Porter-Duff, NCC template matching, scanline rasterization, etc.). Prefer
descriptive node names (e.g. "Channel Select", "Shape") over another product's
node names and panel layouts. The goal is comparable *capability*, not a clone.

## Next steps (suggested order)

The user's overall goal is a full compositing toolkit: 2D/3D trackers, masks,
roto, shapes, channel tools, switches, now also scriptable and extensible
as the planning docs describe. The user has been saying "continue with next
steps": take them in this order, one milestone at a time (commit, push and
fast-forward `main` after each, per the standing instruction above), and
report back after a few milestones.

1. **Small UX debts:** viewer toolbar overlap at narrow widths; no warning
   when closing the window with unsaved changes; Corner Pin's default
   From/To is a 512 square (add "From = image corners"); per-track tracker
   box sizes and offsets are only editable in the viewer; no filtering when
   Transform / Tracker output / Corner Pin shrink the image (aliasing);
   shape motion blur.
2. **Session-4 follow-ups** (small to medium):
   - Python plugins (custom ops written in Python): embed Python in the
     host via the `python/` crate, or run them out of process.
   - Plugin process isolation: a crashing native plugin kills the app.
   - Package the Python module as a wheel (`maturin build` from `python/`,
     `pyproject.toml` is ready). maturin isn't installed on this machine.
   - Not planned: OpenFX hosting (large; its own project), licensing (the
     docs wanted another product's scheme copied; declined), `import_nuke`
     (originality rule).
3. **Phase 4: 3D** — plan separately (see below). A 3D camera tracker
   would build on the 2D tracker.

## Session 4: automation, plugins, Python

Built from the planning docs in `../Archlight-Development-Documentationm/`
(coverage map: `docs/spec-coverage.md`).

- **`session.rs`**: headless editing API (validated `create_node`/`set_params`/
  `set_key`/`connect`/…, undo/redo, render, save/load). MCP, Python and the
  GUI bridge all go through it.
- **`mcp.rs`**: MCP server (`arclight mcp`, stdio or `--http`), 27 tools.
  `call_tool(&mut Session, tool, args)` is the shared tool dispatcher.
  `render_frame` with `preview` returns a PNG image content block.
- **`bridge.rs`**: each GUI listens on `$XDG_RUNTIME_DIR/arclight/gui-<pid>.sock`
  and registers in `instances/<pid>.json`. `App::bridge_call` runs a tool on a
  `Session` over a copy of `doc.graph`, then swaps the graph back in as one
  undo step and places new nodes with `Doc::place_new_nodes`. `undo`/`redo`
  map to the Doc's own history. `load_graph`/`new_graph` refuse while the
  window has unsaved changes. Render tools (`mcp::is_render_tool`) run on
  worker threads (`gui/script_jobs.rs`), on a snapshot of the graph, with
  progress and Cancel in the menu bar.
- **`plugin.rs`** + `include/arclight_plugin.h`: C ABI v1. Ops are
  registered at runtime (`ops::info` falls back to the plugin registry;
  `ops::all()` replaces direct `OPS` iteration in the GUI). Plugin param
  defaults use `DefaultValue::Static`. Libraries are leaked (never unloaded).
  Only files named `libarclight_plugin_*.so` / `*.arclight.so` are loaded.
- **`config.rs`**: XDG dirs; `settings.json` (`plugin_paths`,
  `frame_cache_mb`, `mcp_http_addr`).
- **`python/`**: workspace crate `arclt` (pyo3 0.30, abi3-py310) exposing
  `_arclt.call(tool, json)` and `_arclt.pixels`. The public API is plain
  Python in `python/arclt/__init__.py`. Build with `python/build.sh`.
- The root `Cargo.toml` is now also a workspace (`.`, `plugins/example`,
  `python`); a plain `cargo build`/`cargo test` still builds only the root crate.

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
- **Session 4**: implemented the planning docs' automation,
  plugin and platform layers in Rust: `session.rs`, `mcp.rs`, `bridge.rs`,
  `plugin.rs`, `config.rs`, `python/`, `plugins/example/`,
  `include/arclight_plugin.h`, `docs/spec-coverage.md`. Also changed:
  `ops.rs` (`DefaultValue`, `ops::all()`, plugin fallbacks), `image.rs`
  (`to_rgba8`), `main.rs` (CLI `mcp`/`plugins`/`ops`), `gui/main.rs`
  (bridge), `gui/doc.rs` (`place_new_nodes`), `gui/node_editor.rs`
  (`ops::all()`), `gui/worker.rs` (cache size from settings).
- **Session 5, part 1** (`655e5f6`): Tracker → Transform export.
  `track::transform_params` turns the solve at each tracked frame into
  linear keys on `translate` / `rotate` (unwrapped across ±180°) / `scale`,
  pivoting where Transform does (`size/2 + 0.5` in track coordinates, since
  Transform puts pixel centers on integers). The Transform is sized by
  rendering the tracker's input at the reference frame (assumes the layer
  it moves is the same size: no bbox yet). Keys are a snapshot: re-export
  after re-tracking. `track::solve_tracks` / `Solve::from_choice` are shared
  with the `tracker` op. A test checks the Transform renders the tracker's
  own match-move/stabilize output pixel for pixel.
- **Session 5, part 2** (`d2c26fb`): one track can solve rotation/scale.
  Runs whose Track motion follows rotation key `track<i>_warp` =
  `[degrees, scale]` next to each position (a param *outside* the op's
  spec, so the panel and curve editor don't list it and it has no default;
  `track::warps` reads it, `None` when absent). The GUI continues from the
  warp keyed at the run's start frame (`Warp::then`), since `track()`
  reports warps relative to its start. `resolve_params` adds `refwarp<i>`
  beside `ref<i>`. `solve_tracks` uses the warp only when exactly one track
  is usable and both warps are known; two or more tracks still solve from
  positions. Dragging a box keys only its position (the warp curve
  interpolates).
- **Session 5, part 3** (`86a33c0`): Corner Pin. Op `corner_pin` (`to1..4`, `from1..4`,
  mask/mix) warps by `track::homography(to, from)` (8×8 elimination; sign
  fixed so w > 0 near the source points; `None` for three collinear) and
  `track::project` (`None` past the horizon → transparent). Overlay kind
  `CornerPin` / `Geo::CornerPin` / `Part::Corner { i, from }`. Tracker
  export: `track::corner_pin_params` copies each track's curve into `to<i>`
  with `from<i>` = the reference position (swapped to stabilize), so it
  needs no image size; `track::export(graph, node, op, stabilize)` picks
  Transform or Corner Pin, shared by `Session::export_tracker` (MCP tool
  `export_tracker` with `op`; it was briefly `tracker_to_transform`),
  Python `Node.to_transform` / `to_corner_pin`, and the Tracker panel's
  "To Transform" / "To Corner Pin" rows. Known gaps: no filtering when a
  warp shrinks the image (aliasing, same as Transform); the default
  From/To is a 512 square, so a hand-made Corner Pin on another size needs
  its From corners set (a "From = image corners" button would help).
- **Session 5, part 4** (`d08e081`): Tracker boxes follow its shown output. `Xf` has
  an optional `warp` (overlay points → shown image), set by
  `Xf::for_node` when the selected Tracker is what the viewer shows
  (`Doc::viewed`) and its output is match-move/stabilize
  (`track::output_map`, built on `track::solved`). Drags read the pointer
  through the mapping frozen at press (`Drag::xf`), since moving a track
  changes the solve. Boxes show turned/scaled through it; the hit test
  uses `in_quad`.
- **Session 5, part 5** (`2025115`): match scores and clearing keys. `track()` now
  reports a `Step { pos, warps, scores }` per frame; the GUI keys
  `track<i>_score` (hidden, like `_warp`). `track::weakest` /
  `weak_below(min_score)` (halfway to 1) drive the panel's "Weakest match"
  line (click → seek) and orange dots on the viewer path. "Clear ◀ / ▶"
  removes enabled tracks' position/warp/score keys before/after the
  current frame (`anim::remove_keys`, which holds the value when no key
  is left).
- **Session 5, part 6** (`638d642`): per-track box sizes. Hidden `track<i>_pattern` /
  `track<i>_search` override the shared sizes (`track::box_sizes`;
  `Settings::sizes`, `radii(i)`). Dragging a box corner sizes that track;
  Shift+drag sets the shared size and drops every override
  (`Doc::remove_param`). The panel's Pattern/Search size fields only reach
  tracks without their own size (no per-track fields in the panel).
- **Session 5, part 7** (`9657b6a`): weak frames on the timeline. `track::weak_frames`
  (built on the private `scores`, shared with `weakest`) feeds a new `weak`
  argument of `Transport::ui`, drawn as an orange band behind the key
  diamonds while a Tracker is selected.
- **Session 5, part 8** (`23f6c48`): offset points. Hidden `track<i>_offset` (static
  Vec2, `track::offsets`; `Settings::offsets`) puts a track's boxes away
  from its point. `track()` follows the feature at point + offset and
  reports point = feature − W·offset, W being the run's warp so far (the
  offset turns and scales with the feature). Overlay: Ctrl-drag inside a
  box → `Part::TrackOffset(i)`; boxes and corner sizing use point +
  offset; a dashed line ties the boxes to the point. Panel: "Clear
  offsets" while any is set.
- **Session 6, part 1**: box select and select all for Polygon/Roto. A
  press on empty space (no handle, no Ctrl) starts `Part::Marquee`;
  `drag_to` reselects the vertices inside the rectangle from
  `Drag::start` to `Drag::now`, on top of `Overlays::marquee_base` (the
  selection when Shift was held, else empty). Left-drag no longer pans
  with a shape selected; middle-drag does. ⌘A → `select_all_points`
  (`gui/main.rs` shortcuts, skipped while a text field has focus).
- **Session 6, part 2**: transform box for selected vertices (2+).
  `sel_box` (selected vertices' bounds padded by `BOX_PAD` screen points,
  so handles don't sit on vertices) and `sel_box_handles`;
  `Part::SelBox(BoxHandle::{Move, Rotate, Scale { x, y }})`. Hit order:
  tangents, vertices, box handles, curve edges (insert), inside the box
  (move). `apply` maps each selected vertex through one function (move /
  scale about the box center / rotate about it via `Drag::pivot`, the new
  field `turn` now uses) and its tangent offsets through the same map's
  linear part. The box re-fits the vertices as they change.
- **Session 6, part 3**: Phase 0.6 milestone 1 (see the plan doc).
  `image.rs`: `Size`, `IRect`, `Image { format, x0, y0, width, height,
  pixels }`, `blank`, `window`, `frame`, `at`, `flat`. Boundaries go
  through `flat()`: file writes, video encode, `image_stats`, previews,
  Python `pixels`, plugin inputs (ABI v1), tracker `Luma`, the viewer.
  Every op still outputs window = frame.
- **Session 6, part 4** (user request, between Phase 0.6 milestones): node
  graph insert / shake / minimap. `Doc::insert_on_wire(node, src, dst,
  port)` and `Doc::extract(node)` (heals like delete, keeps the node).
  `node_editor.rs`: `wire_at` (nearest wire within `WIRE_GRAB`, via
  `bezier_distance` on the same curve `bezier` draws); `target_wire` (set
  on right-click / Shift+E over a wire, used by `place`) and `drop_wire`
  (while dragging a node that `is_free`, spliced in on release; lit while
  pending); `Shake::shaken` (3 direction reversals within 0.7 s →
  `extract`, once per drag); `minimap` (lower right, hidden when the
  editor is small; graph bounds frozen while dragging in it, scroll zooms
  about the hovered spot). Unit tests cover the graph edits, shake
  detection and wire distance; the drag gestures themselves haven't been
  tried with a mouse.
- **Session 6, part 5** (`916ae1e`): Phase 0.6 milestone 2, windows that
  shrink. `ops.rs` helpers: `generate(format, window, f(x, y))` and
  `pointwise_window` (inputs' union, grown to the frame when transparent
  inputs give a non-transparent result; `mapped` applies it). Shapes store
  only their outline bounds + feather (∩ frame; whole frame inverted;
  coverage computed on that window); Merge / Dissolve / Keymix / Channel
  Select use B's (or A's) frame and the union; Crop intersects ("shrink
  canvas" shifts); mask/mix works in frame coordinates. Transform, Tracker
  output, Corner Pin, Blur and Resize `flat()` their input until milestone
  3. A 12-node test graph renders identically (MAE 0) to the previous
  commit.
- **Session 6, part 6**: Phase 0.6 milestone 3, windows that move.
  `moved_window(window, format, f)` maps the span bilinear sampling
  reads, (x0 − 1, x1), through the forward map and limits it to
  `overscan(format)` (one frame past each edge; the whole cap when a
  corner can't be mapped, e.g. past a corner pin's horizon). Transform,
  `warp` (Tracker output), Corner Pin and Resize use it; Blur grows by
  radius × passes on sides inside the frame and clamps where data meets
  the frame edge (old behavior for plates). No op flattens any more. The
  12-node comparison graph still matches pre-0.6 output exactly.
- **Session 6, part 7**: Phase 0.6 milestone 4, viewer and overlays.
  Viewer: texture over `frame ∪ window` (`Viewer::region`), overscan
  dimmed in `to_color_image`, dashed data-window outline, "data x,y w×h"
  in the status line, readout anywhere in the region. Transform's layer
  frame: `Request::probe` (selected node with a Transform overlay, and its
  input) → worker evaluates the input (cached by key in a window map) →
  `Response::probe` → `Display::input_window` →
  `Overlays::set_input_window` → `node_geo` sets `Geo::Transform::layer`;
  `layer_frame` maps that rect.
- **Session 6, part 8**: Phase 0.6 milestone 5, project format.
  `Project::format` (`Size`, serde default 1920×1080). Constant,
  Checkerboard and shapes have `width`/`height` kind `FRAME_SIZE` (min 0,
  default 0); `ops::apply_format` fills 0 with the project format after
  `resolve_params` in `Evaluator::run` and `Evaluator::key` (so the cache
  follows it); direct `ops::eval` falls back to `DEFAULT_FORMAT`
  (`source_size`). `ops::fit_new_shape` centers/scales a new shape's
  512-square defaults into the format (`Doc::add_node`,
  `Session::create_node`). `set_project` takes width/height (MCP, Python
  `project(width=, height=)`); transport bar has a Format field;
  `image_stats` adds `window`. Old files keep their stored sizes.
- **Session 6, part 9**: Phase 0.6 milestone 6, measured (table in
  `docs/phase-0.6-plan.md`): a 4K comp of ten small shapes renders 33×
  faster with a tenth of the memory; over a 4K plate 7.5× faster. Output
  identical. **Phase 0.6 is done.** Follow-ups: ROI (render only what a
  downstream crop/viewer needs) and tiling are now possible on top of
  windows; Merge could intersect for `in`/`mask`-type operations.

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
| `session.rs` | `Session`: headless graph + validated edits (`create_node`, `set_params`, `set_key`, `connect`, `port_index` for labels like "B"), whole-graph undo/redo, render/save/load. `validate_param` normalizes values against `ParamSpec`s; `op_schema` describes an op as JSON; `image_stats` gives size + mean/min/max. |
| `mcp.rs` | MCP server. `TOOLS` table (name, description, JSON Schema) and `call_tool(&mut Session, tool, args)`, the one tool dispatcher shared by MCP, Python and the bridge. `Server` routes to the local session or an attached GUI (`list/attach/detach_instance` are handled there). `serve_stdio` (line-delimited) and `serve_http` (hand-rolled HTTP/1.1, POST only). A `preview_png_base64` field in a result becomes an MCP image block. |
| `bridge.rs` | Running-GUI registry (`$XDG_RUNTIME_DIR/arclight/instances/<pid>.json`, stale entries pruned by a connect test) and Unix-socket server/client. Protocol: one JSON line `{"tool", "arguments"}` → `{"ok"}` / `{"error"}`. The GUI polls `Server::poll()` each frame; a request wakes it with `request_repaint`. |
| `plugin.rs` | Native plugin host for the C ABI in `include/arclight_plugin.h`. `init()` (once; called by CLI, GUI, Python) scans `config::plugin_dirs()` for `libarclight_plugin_*.so` / `*.arclight.so`, parses each library's JSON op descriptors into leaked `&'static OpInfo`s, and registers them. `ops::info`/`ops::all`/`eval_op`/`is_time_varying` fall back to it. |
| `config.rs` | XDG dirs (`config_dir`, `data_dir`, `cache_dir`, `runtime_dir`), `plugin_dirs()`, `Settings` (`settings.json`: `plugin_paths`, `frame_cache_mb`, `mcp_http_addr`). |

GUI (`src/gui/`, binary `arclight-gui`, eframe/egui 0.37):

| File | Role |
|------|------|
| `main.rs` | App shell: panel layout, menus, keyboard shortcuts (`shortcuts()`), file drop, starter graph, `ARCLIGHT_SCREENSHOT` hook. Commits undo once per gesture; an active tracking run counts as a gesture. |
| `doc.rs` | `Doc`: the graph plus every edit (`add_node`, `connect`, `set_param`, `edit_param` with auto-key, ...). **Undo** is whole-graph snapshots, committed once per finished gesture. Mutate the graph only through `Doc` methods that call `changed()` or `touched()`. |
| `node_editor.rs` | Canvas: nodes, ports (hollow = optional), wires, pan/zoom, right-click add menu, Shift+E browser; both place nodes via `place`. |
| `properties.rs` | Property panels generated from `ParamSpec`s; keyframe diamonds; media extras; render jobs (`RenderJob`); tracking (`tracker_extras`, `TrackJob`, `collect_tracks`). |
| `viewer.rs` | Shows the image; pan/zoom; channels; exposure; pixel readout. Hosts the overlays. |
| `overlay.rs` | Handles for the selected node's `Overlay` kind: `Xf` (image↔screen), `geo` (resolve at the frame), `hit`, `press`, `apply` (drag → `edit_param`), `draw`. Add new handle kinds here. Positions use the viewed image's size (no bbox yet). |
| `transport.rs`, `curve_editor.rs`, `worker.rs` | Player + timeline; keyframe curves with tangent handles; background eval thread + frame LRU (`frame_cache_mb`, default 1536). |

The GUI starts a `bridge::Server` in `App::new`. `App::bridge_call` runs a
tool on a `Session` over a *copy* of `doc.graph`, then, if the graph
changed, commits pending edits, swaps the graph in, places new nodes
(`Doc::place_new_nodes`: right of their input, moved down until clear) and
commits again, so each script call is one undo step. `undo`/`redo` use the
Doc's history; `load_graph`/`new_graph` refuse while the window is dirty.
View tools (`mcp::is_gui_tool`: `select_node`, `view_node`, `get_view`)
go to `App::view_call` instead: they set `doc.selected`, route through
`Doc::view_node` (an undoable graph edit, like the V key), seek the
transport, and scroll the node graph with `NodeEditor::reveal`. Every one
returns `App::view_state()`. Headless, `call_tool` refuses them with
"call attach_instance first".
Render tools skip all that: `handle_bridge` hands them to
`ScriptJobs::spawn` (`gui/script_jobs.rs`), which renders a snapshot of the
graph on a thread via `mcp::call_tool_with_progress`, replies to the client
and wakes the UI. The menu bar lists running jobs with progress and Cancel.
A client's next call waits for the reply (the bridge is request/response
per connection), but other clients and the user keep working meanwhile.

Other crates in the workspace (root `Cargo.toml` is package + workspace; a
plain `cargo build` builds only the root crate):

| Path | Role |
|------|------|
| `python/` | Crate `arclt` → `_arclt` extension (pyo3 0.30, abi3-py310): `call(tool, args_json)` (runs `mcp::Server::call` on a global server, GIL released) and `pixels(node, frame)`. The public API is plain Python in `python/arclt/__init__.py` (`Node` handle class plus module functions, all built on tool calls). `build.sh` sets `PYO3_BUILD_EXTENSION_MODULE=1` and copies the `.so` into the package. |
| `plugins/example/` | Example plugin (cdylib, no arclight dependency): `vignette`, `film_grain` (time-varying). |
| `include/arclight_plugin.h` | Plugin ABI v1: `arclight_plugin_entry()` → `{abi_version, ops_json, render}`; the host passes an `alloc` callback for the output. `render` must be thread-safe. |

### Adding a tool (MCP + Python + bridge at once)

1. Add the edit/query to `Session` if it changes the graph (call
   `checkpoint()` before mutating, *after* validating, so a refused edit
   leaves no undo step).
2. Add a `tool!` entry to `TOOLS` and a match arm in `call_tool`.
3. Add a Python wrapper in `python/arclt/__init__.py` (`_call("tool", ...)`).
4. If the tool touches GUI-only state (selection, viewer, file dialogs),
   special-case it in `App::bridge_call`.
5. Tests: `mcp.rs` tests (`call` helper), `python/tests/test_arclt.py`.

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
- **Hidden per-track params** (outside the op's spec, so not in the panel
  or curve editor, and with no default): `track<i>_warp` ([degrees,
  scale], keyed by runs that follow rotation), `track<i>_score` (match
  correlation per tracked frame), `track<i>_pattern` / `track<i>_search`
  (own box sizes), `track<i>_offset` (static; boxes away from the point).
  Read them with `track::warps` / `weakest` / `weak_frames` / `box_sizes`
  / `offsets`; `resolve_params` passes them through, and adds `ref<i>` /
  `refwarp<i>` (values at the reference frame).
- **Solving / output:** `track::solved(params, frame)` (reference → frame),
  `output_map` (what the output shows: solved, inverse, or none),
  `solve_tracks` (one track with warps → its own turn/scale; else the
  least-squares similarity). Exports: `track::export(graph, node, op,
  stabilize)` → `transform_params` or `corner_pin_params`.
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

### Phase 0.6: bounding boxes / format — done in session 6

See `docs/phase-0.6-plan.md`. Possible follow-ups: ROI evaluation (render
only the part a downstream crop or the viewer needs) and tiling, now that
windows exist.

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
- Overlays use the *viewed* image's format: handles of a node whose own
  input has another format (e.g. viewing downstream of a Resize or a
  shrinking Crop) don't line up. View the node itself to edit it.
- Session 5's tracker features were checked with unit tests and
  screenshots of synthetic scenes, not on real footage or by hand: weak
  frames have only been seen with hand-keyed scores; Ctrl-drag offsets,
  per-track box sizing and drags over a match-moved output haven't been
  tried with a mouse.
- Hidden tracker params (sizes, offsets) can't be seen or typed in the
  panel; only the viewer shows them.
- Corner Pin and every other warp sample bilinearly with no filtering, so
  strong shrinking aliases.
- Viewer toolbar text overlaps when the window is narrow.
- First frame after launch takes ~1.5 s (ffprobe + ffmpeg start); later
  frames are ~5 ms for the starter graph.
- Closing the window with unsaved changes doesn't warn (New/Open do).
- Viewer display conversion is on the CPU (rayon); fine to ~4K.
- Video decode is sequential-friendly: playing backward re-seeks every frame
  (slow for long-GOP files; the frame cache hides it on second pass).
- A bridged render renders the graph as it was when the request arrived;
  edits made during it don't affect that render.
- Bridged edits in the GUI were checked with Python + screenshot, not by
  hand mid-gesture. A script edit during a user drag commits the partial
  drag as its own undo step.
- The GUI's screenshot hook exits with `process::exit`, so it leaves a stale
  registry file. `list_instances` prunes it on the next call.
- Plugins are never unloaded; there's no hot reload, so restart to pick up a
  rebuilt plugin.
- MCP HTTP has no auth and no SSE stream. Bind it to localhost only.
- No Python plugin support, no OpenFX, no GPU path (CPU/rayon only).

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
- **Python tests:** `python/build.sh && cargo build --release -p
  arclight-plugin-example && ARCLIGHT_PLUGIN_PATH=target/release
  PYTHONPATH=python python3 -m unittest discover -s python/tests -v`
  (the plugin test is skipped without the plugin path). Rebuild the `.so`
  with `build.sh` after any engine change, or Python runs stale code.
- **MCP by hand:** pipe JSON lines into `arclight mcp`, e.g.
  `printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_ops","arguments":{}}}' | ./target/release/arclight mcp`;
  HTTP: `arclight mcp --http 127.0.0.1:7899` + `curl -X POST localhost:7899/mcp -d '...'`.
- **Bridge end to end:** start the GUI with the screenshot hook and a
  ~6 s delay, run a Python script that calls `arclt.attach()` and edits,
  then look at the screenshot (python3 has no PIL here; crop with
  `magick in.png -gravity south -crop 80%x24%+0+0 out.png`).
- Keyboard shortcuts must skip when a text field has focus
  (`ctx.text_edit_focused()`), and `consume_shortcut` ignores an extra Shift:
  check Shift variants before plain ones.
