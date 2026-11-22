# Arclight — handoff notes

Last updated 2026-11-21.

## Status

- **0.15.1 (2026-11-21): 3D view's first shaded render** (Dev2).
  `viewport3d::sync`'s first request after switching to 3D goes out
  before the scene is known, so it carries no render; the scene's
  arrival left the request key (`Ask`) unchanged, so the textured cards
  never came until a selection (ghosts) or orbit (view) changed it.
  `Ask.render` now records whether a render was asked for. Anything new
  that decides what's asked belongs in `Ask`, or the request is skipped.
  Test `the_first_shaded_render_comes_without_a_selection_or_view_change`
  drives sync at a settled egui time (a still clock keeps the view at
  reduced resolution, which re-asks by accident and hid the bug).

- **Phase 15: gap fixes, 0.15.x on `wip-ns-gapfixes`** (from 0.14.37
  `c9268c7`, the user's call 2026-11-21); `main` follows it. `wip-ns-ui`
  is frozen at 0.14.37. First: the 3D view's textured cards (0.15.1), then Dev1's test-exit fix
  (`gpu::exit_tests_cleanly`, glibc `on_exit` → `exit_process` in unit
  tests only; `CheckRow.slow` / `CheckReport::correct` for the
  self-test's timing bar), then the TODO gaps.

- **0.14.37 (2026-11-21): multi-input Shuffle routes + Matte Combine
  on the GPU** (Dev2). `gpu/multi.rs` + `multi.wgsl`: each pass reads
  one source (one input's rgba or one plane) and writes the output
  being built (≤ 4 storage buffers: no binding-count issue on Metal /
  WebGPU-baseline adapters). Routes write into fresh buffers; Matte
  Combine folds into an f32 accumulator (also the `plane` output);
  `matte_combine::Plan` (settings + window rule) is shared by CPU and
  GPU; Remove uses the CPU's `remove_layers` on a probe of the names.
  32 inputs at 4K: ~0.35 ms a pass, linear. Last version on
  `wip-ns-ui`.

- **0.14.36 (2026-11-21): Geo Info per-point picking** (Dev2). Element
  picks live in `viewport3d.elements` (UI state: domain, node, index
  into scene.point_sets / meshes, element), validated against the
  sheet's scene by `element_pick::valid`; the 3D toolbar's Points toggle
  (`points_mode`) routes clicks / box / Esc to `gui/element_pick.rs`.
  `geo_table::View.picked_elements` narrows Points / MeshPoints per
  object through `Table.subsets` (row → element index in `locate`);
  MCP's `Table::new` / `counts` untouched (tested). CPU screen-space
  hit-testing only on click / box release; the click skips mesh
  vertices behind the nearest `ray_mesh` surface; a box keeps
  camera-facing vertices but doesn't test occlusion (known limit; a GPU
  id pass if ever needed). Seen once: `gpu::check::tests::the_self_test_passes`
  failed in a full run while other GPU work ran; passed on rerun.

- **0.14.35 (2026-11-21): Merge modes + any pass as a mask** (Dev2).
  New modes in `ops::blend` plus GPU codes 7–10 (`gpu/ops.rs`
  merge_mode order = the `pointwise.wgsl` switch; min is `case 6u`,
  default still min); stencil / mask (`ops::holds_out`) carry B's passes
  only. Masks: `mask_plane` (fixed text, "" = none) on the 15 mask ops;
  `mask_source.rs` reads channel or plane (missing plane = error); the
  GPU moves the plane into alpha with the shuffle kernel, then
  mask_mix(alpha). Edge Extend's matte_channel unchanged on purpose.
  GUI: `gui/mask_picker.rs`; the worker's `probe_mask` runs only for the
  selected node with a connected mask. CLI byte-identity: 29 old merge /
  mask outputs, 0 differ (CPU and GPU).

- **0.14.34 (2026-11-21): GPU splats over surfaces** (Dev1). The Mac
  slowness was general: with AA, each sample re-walked its tile's splat
  list wherever it met a surface (9 walks a pixel at samples 3).
  `raster.wgsl` now walks the splats in front of the row's nearest
  surface once per pixel as a workgroup batch (`front_batched`, bound =
  nearest cover depth over the row's samples) and each sample continues
  from that front cursor: same arithmetic and order, bit-identical.
  Fallbacks to the per-sample path: MATTES on, instances in the bin (no
  bound; cursor unbatched), a jitter cell, or a sample whose surface is
  nearer than the front; splats behind a see-through surface still walk
  per sample. Pond 1280×720 samples 3: 185 → 103 ms (CPU 144). Further:
  keep the front between rows (~50 B a pixel) or one dispatch for all
  rows. Test `splats_in_front_of_surfaces_match_at_every_sample`; check
  row "3D render, splats over a card, AA 3×3". Mac timing pending (TODO).

- **0.14.33 (2026-11-21): Matte Combine** (Dev2; the user's request,
  plan approved with the user's answers: min/max + Soft switch, white
  matte default, keyable stepped Invert / operation). `matte_combine.rs`;
  params `channels` (strings), `inverts` / `operations` (number lists,
  codes 0/1 and 0–3; MCP / Python names → codes in
  `session::matte_combine_list`), `soft`, `output`, `plane`.
  `ops::STEPPED` makes `resolve_at` evaluate those two lists with
  `anim::evaluate_step` whatever their curve's interp; the panel
  (`gui/matte_combine_panel.rs`) resets their curves to Step only when
  they aren't (a hand-edited smooth-keyed file is marked modified the
  first time its panel shows). Growable image ports are shared with
  Shuffle (`NUMBERED_PORTS`; `session::input_cap` for any image-growable
  op). Window: whole frame when the fold of the inputs' outsides
  (inverted → 1) is above 0. CPU only.

- **0.14.32 (2026-11-21): static one-frame Media In + GPU depth race**
  (Dev1). `ops::is_time_varying("media_in")`: time-varying only when
  last_frame > first_frame and the path is a video / sequence (Read
  already was). `raster.wgsl` / `blur.wgsl`: the hits buffer is
  `array<u32>` with `hit_at` / `set_hit`; depth is one word at
  `16*pixels + px`. Before, each depth went into one lane of a
  `vec4<u32>` shared by 4 pixels, which naga's MSL leaves as a dynamic
  lane store into a device uint4 (can compile to a 16-byte RMW → race:
  "depth only, 46–75% off" on the M4). Linux was never affected; not yet
  confirmed on a Mac. Static-layer caching inside a render: skipped on
  purpose (not exact with AA / see-through surfaces; prepared splats are
  already kept). The splat slowness is general, not Metal: being fixed
  in `raster.wgsl` (Dev1, next). The intermittent SIGSEGV at test exit
  is an NVIDIA driver race, pre-existing (TODO).

- **0.14.31 (2026-11-21): Shuffle routing** (Dev2; the user's channel
  combine request, decided as "grow Shuffle"). `routes` (Shuffle only,
  default []) = [["<input>:<channel>", "<dest>"]], inputs numbered from
  1; they run after in / channels / out and before remove, each reading
  the unshuffled inputs (`shuffle_route.rs`). Output window = input 1's
  ∪ the windows of the inputs read. `ops::growable` is a match: shuffle
  (1 fixed, image ports "2".."32" via SHUFFLE_PORTS), scanline_render
  unchanged; the node editor's "spare input" add only goes to scene
  ports. Scripts: `session::input_cap` (Shuffle 32, others fixed);
  describe_node lists the extra ports. GUI: the worker's `probe_more`
  probes a selected Shuffle's ports 2+ only (`gui/shuffle_routes.rs`).
  GPU: routes reading input 2+ run on the CPU; within input 1 they stay
  on the GPU.

- **0.14.30 (2026-11-21): Roto pen + feather falloff** (Dev2). Roto
  `closed` (default true; false = open path: `shape_op` returns the
  background, invert ignored; the overlay drops the closing chord, Body
  and feather handles). Pen state is UI-only (`gui/pen.rs`): on while an
  open Roto is selected unless dismissed; a new-shape request goes
  through `NodeEditor::add_at_view`, so it chains like a menu add; only
  `NodeEditor::place` starts a Roto empty (scripts / bridge / Python keep
  the circle). `shape_falloff.rs` acts on the ramp t inside
  `coverage_with` on both coverage paths; defaults and ≤1 px edges use
  the identical smoothstep (tests `rotos_from_before_pen_mode_render_the_same`,
  `defaults_are_the_plain_smoothstep_bit_for_bit`). In flight: Dev1 on
  the TODO splat / depth items (one-frame Media In static, the Metal
  depth race in raster/blur.wgsl, the GPU splat/surface merge, ~2.4×
  slower than the CPU on Linux too). User's decision: the channel
  combine node = Shuffle grows inputs.

- **0.14.26–0.14.27 (2026-11-13, in v0.14.29).** 0.14.26: gizmo-only 3D
  transforms. `Viewer::transform_keys` no longer starts object
  transforms; every `Transforming` is a gizmo drag (`by_drag` removed,
  `start_many` private). `Viewer::follow_selection` resets the tool to
  Move when the sorted selection changes, unless Select is on.
  `PointsTransforming` (a Curve's picked points) keeps keyboard G / R / S.
  0.14.27: node-graph thumbnails. `gui/thumbnails.rs` (`Thumbnails::get`
  never blocks; one decoder thread, catch_unwind per job; keyed on op,
  path, first frame and file mtime; failures cached until the key
  changes); `gui/graph_thumbs.rs` places and draws them; `NodeEditor`
  owns the cache and only asks for on-screen read / media_in nodes at
  zoom ≥ 0.4; the toggle is `ViewPrefs.node_thumbnails` (view.json).
  Known: a video's thumbnail goes through `media::read_video_frame`'s
  shared per-path decoder, so a first thumbnail of a playing video
  restarts its stream once (accepted; media.rs untouched).

- **0.14.28 (2026-11-13, in v0.14.29): Geo Info World + Cloud, Scanline
  note.** `geo_table::View {world, picked}` and `Domain::Cloud` are the
  sheet's only (`Domain::SHEET` / `sheet_counts` / `Table::with_view`);
  MCP geometry still uses `Domain::ALL` / `counts` / `Table::new`,
  byte-identical (tested). World transforms per visible cell (point:
  world; normal: inverse transpose). Cloud picks come from
  `viewport3d.points` (scene.points indices), validated against the
  sheet's scene.
- **0.14.29 (2026-11-13, in v0.14.29): GPU playback without the
  per-frame read-back.** `Evaluator::evaluate_lazy` + `gui/view_frame.rs`
  (`ViewFrame`: format, window, layers and scene kept as made; `image()`
  reads back once, memoised; `pixel()` reads single GPU pixels) replace
  the viewer's read-back. `FrameCache` keeps GPU-only frames within the
  GPU budget and reads them back past it or on GPU off. A failed
  read-back triggers one `cpu_only` re-request (`App::cpu_retry`). The
  worker waits on the device after a lazy frame and pre-reads the
  picker's pixel (`Request.pick`). Bench: `cargo test --release --bin
  arclight-gui playback_bench -- --ignored --nocapture`. Not yet tried:
  the live GUI by hand, a forced read-back failure, Macs.
- **Version 0.14.29, released as v0.14.29** (2026-11-13: Linux tarball,
  Windows zip, `arclt` wheel + sdist, macOS universal DMG; marked
  Latest). It
  carries 0.14.26–0.14.29 (below). v0.14.25 (2026-11-02) was the plasma
  beam example; v0.14.21 has no DMG on purpose.
- **Phase 14 (workspace and UI)** on branch `wip-ns-ui` (from 0.13.41,
  `5d35db7`); `main` is fast-forwarded to it. Earlier phase branches are
  frozen (`wip-ns-geo`, `-gpu`, `-engine`, `-splats`, `-3dwork`, `-effects`).
- **Paused 2026-11-13 after v0.14.29** for the user's hand testing (TODO
  "To check by hand": 0.14.28, 0.14.29 incl. the Macs). Dev1 / Dev2 idle,
  nothing in flight. Next candidates: phase 15 (Q1–Q5 in
  `docs/production-plan.md`), the channel combine / routing node and the Roto
  pen (TODO, user's requests), the 3D pivot, Geo Info per-point picking.
- **Mac findings (2026-11-13, Apple M4, v0.14.29 DMG; TODO "Known gaps",
  first three items).** The DMG already runs native arm64 on Metal
  (`lipo`: x86_64 + arm64; `arclight gpu`: `Apple M4 (Metal,
  IntegratedGpu)`, unified memory), so an Apple-Silicon-only build
  wouldn't be faster. The user's project
  `examples/Pond-Martian-LizardSplat-Combo.json` (splat + two cards over
  a plate, Scanline Render 1280×720) plays at ~1 fps with the GPU and
  ~3 fps without: 1017 vs 321 ms/frame warm (Evaluator kept across
  frames, `evaluate_lazy` + `gpu.wait()`); the CPU idles in
  `__psynch_cvwait`, so a GPU pass is slow, likely splats mixed with
  surfaces (`sort_tiles` past `LOCAL`, or the merge in `raster.wgsl`).
  The plate's size doesn't matter (4K or 720p: same). A one-frame Media
  In (the still PNG) makes the render time-varying, so the static splat
  redraws every frame. `gpu --check` fails 13 / 81 there, all 3D, on the
  depth plane (the 2026-11-02 Mac run passed). Workaround for the user:
  GPU off. Next: the same project and `gpu --check` on Linux / the RTX
  5070, to tell Metal from general. The example is committed with
  relative paths; its plate `examples/media/pond-720.mp4` (36 MB) is a
  720p transcode of the user's 4K original. `main` is now at
  `wip-ns-tinyshiny` (TODO + the example on top of `wip-ns-ui`).
- **Next: the rest of the user's hand testing** (TODO.md "To check by
  hand"; Camera Tracker, "." pivot, Roto following a Tracker, detached
  windows, the Cameras example and `gpu --check` on the Macs passed on
  2026-11-02; left: the Cube UV atlas, the phase 13 list, the GPU / Mac
  checks and the older ones). Then the user picks what's next (TODO.md
  "Known gaps and follow-ups", phase 15, ROADMAP).
- **Production readiness** is proposed in `docs/production-plan.md`
  (OCIO, groups / gizmos, regions / disk cache / proxies, farm, EXR),
  awaiting the user's Q1–Q5; now after phase 15 (gap fixes).
- **On hold** (ROADMAP.md "Later"): Paint, and a 3D pivot ("." frames the
  selection in 3D; a 3D pivot is a "maybe").

## Read first

1. `CLAUDE.md`: team rules, build / test commands, rules.
2. `TODO.md`: what's wanted, hand-check lists, "Not planned".
3. `ROADMAP.md` (public; keep it in step with TODO.md when phases end).
4. `README.md` (features, file format, shortcuts, scripting, "3D view").
5. `docs/help.md` (the in-app help, `include_str!`'d, tests parse it; keep
   it in step with features).
6. `docs/*-plan.md` for the phase at hand; `docs/spec-coverage.md`.
7. `CHANGELOG.md`: per-version detail (with git history), not repeated here.

Done phases: 0.6 data windows / project format (`docs/phase-0.6-plan.md`);
4 / 9 3D, M1–M13 (`docs/phase-4-plan.md`); 10 splats
(`docs/splat-plan.md`); 11 engine / platform (CHANGELOG 0.11.x); 12 GPU
(`docs/gpu-plan.md`); 13 procedural geometry (`docs/geo-plan.md`).

## Team workflow (herdr)

- One **Supervisor** (owns git, versions, releases, HANDOFF / TODO) and
  two workers, **Dev1** and **Dev2**. The full rules, locks and report
  format are in `CLAUDE.md`.
- Not in CLAUDE.md:
  - Verify from a snapshot of exactly the reported paths
    (`../.verify/sup-<ver>`, `CARGO_TARGET_DIR=../.verify/target-sup`),
    never in `/tmp` (tmpfs filled up once). Look for leftover debug tests.
    Workers have kept editing after reporting: build the committed HEAD
    again before tagging.
  - Filtered staging of a shared file: `git hash-object -w` + `git
    update-index --add --cacheinfo` (`--add` for new files).
  - Verification builds don't update `target/release`, which the Omarchy
    launcher runs: `cargo build --release` in the repo after commits.
  - Standing instruction: after a finished milestone, commit, push the
    phase branch, fast-forward and push `main` without asking; keep both
    branches and remotes at one commit.
- Repo `github.com/nsoler/arclight` (private). The planning docs session
  4 implemented: `../Archlight-Development-Documentationm/`.

## Versioning

- `0.<phase>.<increment>`: the middle number is the phase the user
  declares; every shipped change bumps the last; a new phase restarts it
  on a new `wip-ns-*` branch. Each Dev milestone is its own version.
- A bump changes `Cargo.toml`, `python/Cargo.toml` and
  `plugins/example/Cargo.toml` together, with a `CHANGELOG.md` entry, in
  the same commit. (Releases before the renumbering are v0.0.x.)

## Releases

Only when the user says so. Actions is billing-blocked (since v0.9.57), so
everything but the DMG is built here.

1. Fast-forward `main`; `git tag -a v<ver> -m "ArclightVFX <ver> (beta)" main`;
   push it.
2. In a **clean worktree at the tag**:
   - Linux: `env -u CARGO_TARGET_DIR packaging/linux/build-tarball.sh`
     (it installs from `./target`) → `dist/arclightvfx-<ver>-linux-x86_64.tar.gz`.
   - Windows: `ZIG=$(ls ../.verify/venv-rel/lib/python3*/site-packages/ziglang/zig)
     env -u CARGO_TARGET_DIR packaging/windows/build-zip.sh` (zig from the release venv's
     `ziglang`) → `dist/arclightvfx-<ver>-windows-x86_64.zip`.
   - With the release venv `../.verify/venv-rel` (maturin + ziglang):
     `maturin build --release --zig --compatibility manylinux2014 --target
     x86_64-unknown-linux-gnu -m python/Cargo.toml` and
     `maturin sdist -m python/Cargo.toml`.
3. Test the wheel in a fresh venv **run from `python/tests`** (from
   `python/` the source package shadows the installed one), with
   `ARCLIGHT_PLUGIN_PATH` at the example plugin's build.
4. `gh release create v<ver> --title "ArclightVFX <ver> (beta)"
   --notes-file ../.verify/rel-<ver>-notes.md --latest <assets>`; notes
   (New / Fixed, "Since v<prev>: see CHANGELOG", an Assets line) say the
   DMG is to follow.
5. Check every asset arrived: `gh api repos/nsoler/arclight/releases/tags/v<ver>`
   (one upload once silently didn't).
6. The user builds the DMG on a Mac (`packaging/macos/build-dmg.sh` at
   the tag) and uploads it; check it, then update the notes' Assets line.

- **Release rule (user):** every release carries the Linux x86_64
  tarball, the Windows x64 zip (since v0.13.34), the macOS universal DMG
  (Apple Silicon + Intel), the `arclt` wheels (Linux manylinux2014; macOS
  universal2 only once Actions works) and the sdist. Every new release is
  marked **Latest** (`--latest`), not pre-release.
- Windows: tested under wine only. No loopback-TCP bridge on purpose (any
  local web page could drive the GUI). macOS: no Developer ID signing /
  notarization; ffmpeg not bundled (`media::tool` searches beside the
  exe, PATH, /opt/homebrew/bin, /usr/local/bin, /opt/local/bin).

## Naming and branding

- Product: **ArclightVFX Compositor** (short **ArclightVFX**) since
  0.11.3. Used in display names, `ArclightVFX.app`,
  `ArclightVFX-<ver>-universal.dmg`, `arclightvfx-<ver>-…` tarball / zip.
  Release titles: "ArclightVFX <ver> (beta)".
- Kept as `arclight` on purpose: binaries `arclight` / `arclight-gui`,
  crates, `arclt`, settings folders, env vars, MCP id, bundle id
  `com.github.nsoler.arclight`, the repo.
- Icons: `assets/arclight.svg`, `assets/arclight-icon.svg` (no lettering,
  ≤ 64 px); `assets/arclight.png` = `rsvg-convert -w 256 -h 256` of the
  SVG, built in (`app_icon()`): re-render, rebuild, `./install.sh`.

## Rules

- **Originality rule.** The parent folder `arclight-compound/` holds a
  Nuke disassembly. Never read it, use it as a reference or copy from it;
  it's not in the repo. Build every node from public, standard algorithms
  (Porter-Duff, NCC matching, scanline rasterization, …) and use
  descriptive node names rather than another product's node names or
  panel layouts. The goal is comparable capability, not a clone.
- **CorridorKey** (`../CorridorKey`, CC BY-NC-SA 4.0 + no-resale terms):
  the user chose classic keying instead ("option D": Color Difference Key
  + Edge Extend). It stays out of the repo; if ever revisited, only as a
  separate, user-installed component Arclight calls, never vendored.

## Architecture in one page

Engine (`src/`, library crate `arclight`):

| File | Role |
|------|------|
| `ops.rs` | Op registry `OPS: &[OpInfo]` (ports, `ParamSpec`s, `mask_base`, `Overlay`, help) and most ops; `eval` applies the generic mask / mix; `resolve_params` / `resolve_at` resolve keys and links; `is_time_varying`; `apply_format`. `geo_ops.rs`, `geo_curves.rs` are `#[path]` modules of it. |
| `graph.rs` | `Graph { nodes, project, backdrops }`; `Evaluator` caches by `node_key` (op, resolved params, input keys, frame if time-varying, GPU backend tag), entries `Res::{Cpu, Gpu}`. |
| `anim.rs` | Keys inline in params (`{"keys": [[f, v], …], "interp"}`), cubic Hermite; `set_key`, `edit_all`, `sample`, `remove_keys`. |
| `image.rs`, `color.rs` | `Image`: linear premultiplied RGBA with a data window, named f32 `planes` (`layer.channel`), optional `scene`; `flat()` at every boundary; pixel centers on integers. |
| `link.rs`, `expr.rs` | Live links and formulas (Pratt parser, `Clock`, `@attr` names, `noise3`); depth limit 8, loops are errors. |
| `shape.rs`, `shape_feather.rs`, `shape_follow.rs` | Shape coverage (signed distance + smoothstep), splines; per-point feather; shapes following a Tracker. |
| `track.rs` | 2D tracker (below). |
| `features.rs`, `solve.rs`, `bundle.rs`, `sfm.rs`, `nodal.rs`, `camtrack.rs`, `camtrack_tracks.rs` | Camera tracker: KLT + affine refine, two-view init, PnP, LM bundle adjustment, incremental / tripod solves, `Solution`, stored tracks. |
| `geom.rs`, `scene.rs`, `mesh.rs`, `shade.rs`, `render3d/` | 3D maths, `Scene`, meshes, lights, Scanline Render (CPU and `render3d/gpu/`), shadows, projections, bake, splats, sprites, mipmaps. |
| `geo*.rs`, `project3d.rs`, `particles*.rs`, `splat.rs` | Procedural geometry, Project 3D, particles, Gaussian splats. |
| `gpu/` | wgpu device, `GpuImage`, GPU ops, `check.rs`, display. |
| `key_color.rs`, `edge_extend.rs` | Color Difference Key, Edge Extend (CPU + GPU). |
| `media.rs`, `dpx.rs`, `gltf.rs`, `alembic.rs`, `ogawa.rs`, `geo_write.rs`, `render.rs` | I/O and frame-range rendering. |
| `session.rs`, `mcp.rs`, `bridge.rs` | Headless editing API (MCP, Python and the bridge all use it); `TOOLS` + `call_tool`, the one dispatcher; the running-GUI registry and Unix socket. |
| `plugin.rs`, `config.rs`, `crash.rs` | C ABI v1 plugins (`include/arclight_plugin.h`, run out of process); XDG dirs and `Settings`; crash reports. |
| `graph_merge.rs`, `templates.rs` | Project import / merge, `rename_refs`, copy / paste excerpts, templates. |

GUI (`src/gui/`, eframe / egui 0.37): `main.rs` (shell, menus,
`shortcuts()`, `EXAMPLES`), `panels.rs`, `doc.rs` (`Doc`: every edit and
undo), `node_editor.rs`, `properties.rs`, `viewer.rs`, `overlay.rs` (2D
handles), `viewport3d.rs` + `gizmo3d.rs`, `worker.rs` (eval thread, frame
cache), `transport.rs`, `curve_editor.rs`, `camera_tracker.rs`,
`camera_points.rs`, `solve_quality.rs`, `pivot.rs`, `clipboard.rs`,
`templates.rs`, `spreadsheet.rs`, `dialogs.rs`, `preferences.rs`,
`help.rs`, `script_jobs.rs`, `bridge_calls.rs`.

Bridge: `App::bridge_call` runs a tool on a `Session` over a copy of
`doc.graph`, swaps it back as one undo step, places new nodes
(`Doc::place_new_nodes`). View tools (`mcp::is_gui_tool`) go to
`App::view_call` (headless: "attach_instance first"). Render tools run on
threads (`gui/script_jobs.rs`) on a snapshot, with progress and Cancel.

Workspace: `.`, `python/` (crate `arclt`, pyo3 abi3-py310; public API in
`python/arclt/__init__.py`; `python/build.sh`), `plugins/example/`
(`vignette`, `film_grain`). Plain `cargo build` builds only the root crate.

### Adding a tool (MCP + Python + bridge at once)

1. Add the edit / query to `Session` if it changes the graph (`checkpoint()`
   after validating, before mutating, so a refused edit leaves no undo step).
2. A `tool!` entry in `TOOLS` and a match arm in `call_tool`.
3. A Python wrapper in `python/arclt/__init__.py` (`_call("tool", ...)`).
4. GUI-only state (selection, viewer, dialogs): special-case it in
   `App::bridge_call`.
5. Tests: `mcp.rs` (`call` helper), `python/tests/test_arclt.py`.

### Adding a node today (checklist)

1. `ops.rs`: an `OpInfo` in `OPS` (ports via `IN` / `req()` / `opt()`,
   params via `p!` / `fixed!`). Mask support: end the ports with `MASK`,
   the params with `MIX`, set `mask_base: Some(<input>)`. Viewer handles:
   set `overlay`; a new kind needs a `Geo` variant plus `hit`, `apply`,
   `draw` arms in `gui/overlay.rs`.
2. A `#[derive(Deserialize)]` params struct (defaults come from the spec)
   and a function with a match arm in `eval_op`; the core in its own file.
3. Frame-dependent by itself → `is_time_varying`. Depends on other frames
   or nodes → put those values into the params in `resolve_params` so the
   cache key sees them.
4. GPU path: share the maths with the CPU (a `plan()`), add a
   `gpu/check.rs` row.
5. Tests: engine, overlay (`gesture` / `drag`), old projects unchanged.
   The GUI picks up the rest from the registry; add help.md / README lines.

### Tracker internals

- **Params** (`tracker`): four tracks (`track<i>_on`, `track<i>` keyed per
  frame), shared `pattern` / `search`, `min_score`, `update`, `motion`
  (translate / + rotate (default) / + scale), `output` (input / match-move
  / stabilize), `solve`, `reference_frame`.
- **Hidden per-track params** (outside the spec, no default, not in the
  panel or curve editor): `track<i>_warp` ([degrees, scale]),
  `_score`, `_pattern` / `_search`, `_offset`. Read via `track::warps` /
  `weakest` / `weak_frames` / `box_sizes` / `offsets`; `resolve_params`
  adds `ref<i>` / `refwarp<i>`.
- **Solve / output:** `track::solved(params, frame)` (reference → frame),
  `output_map`, `solve_tracks` (one track with warps → its own turn /
  scale; else least-squares similarity); `positions_at` / `warps_at` /
  `solved_at` / `motion_at` take fractional frames. `track::export` →
  `transform_params` / `corner_pin_params`, or live links.
- **Matching** (`track::find`): target sampled through the track's
  `Warp`; integer NCC (summed-area tables) → angle ±6° / scale ±4% →
  ±2 px re-search → 4 rounds of parabola `refine`. Windows flatter than
  `FLAT` score 0.
- **Runs** (`track::track`): either direction, constant-velocity
  prediction; lost below `min_score` or off-image; `input_frames` renders
  the input with its own `Evaluator`.
- **GUI:** `TrackJob` → `Found` queue drained by `collect_tracks` each UI
  frame (checks "finished" before draining). Overlay `Geo::Tracker`,
  `Part::Track` / `TrackBox` / `TrackOffset`; boxes drawn through the
  shown output (`Xf.warp`, frozen at press in `Drag::xf`).

## Subsystem notes

**Engine / graph**
- Anything an op reads besides its own params must reach `node_key`
  through the resolved params: `_motion`, `_refs` (formula refs),
  `_path`, `_follow`, `project_format`, `ref<i>`; `write` / `write_geo`
  mix in `ctx.write_outputs`; held ports are skipped (`held_inputs`).
- `node_key` hashes the params string, including the Camera Tracker's
  `tracks` (+0.3 ms per evaluation).
- Unconnected required inputs read as an empty project-format image
  (only Output ops error).
- Link kinds (`link.rs`): `{node, param[, at]}`, motion, `expr`, `solve`
  (Camera ← Camera Tracker), `follow` (shape ← Tracker). A single number
  repeats to a list param's length (`resolve_at`).
- `Doc` revisions come from a process-wide counter (`doc.rs`
  `next_revision`); per-Doc counters made a loaded project show the
  previous one's 3D view.
- `graph_merge::rename_refs` is the one place for node-name references
  (inputs, links, formulas, `path_of`, Media Out `audio`, Group, Scanline
  Render `camera`, shape `follow`): extend it for new ones.
- Relative media paths resolve only when `project.folder` is set; a
  `project` block needs `format`, `fps`, `first_frame`, `last_frame`.
- Positive `rotate` is clockwise on screen (y down); don't flip it.
- Exits go through `gpu::exit_process` (wait, flush, `_exit`; the wgpu
  device is never dropped); Python's atexit only waits.
  `tests/cli_exit.rs` guards it.

**GPU** (`src/gpu/`, `src/render3d/gpu/`)
- CPU and GPU must agree: GPU paths share their maths with the CPU
  (`ops::WarpMap`, `keyer_plan`, `resample_layout`, `shadow::plan`, the
  `plan()` of `edge_extend` / `key_color`) and get a `gpu/check.rs` row,
  run by `arclight gpu --check`.
- `GpuImage` clones share buffers: never write into an input's buffer.
  `gpu::ops` op codes must match `pointwise.wgsl`'s consts.
- GPU tests start `let Some(gpu) = gpu::test_device() else { … return; };`.
  render3d lib tests keep the GPU off unless `render3d::gpu::tests::GPU_ON`.
  The naga test translates every `src/**/*.wgsl` to MSL; at most 8
  storage buffers (per-frame tables share one words buffer).
- `enabled()` reads settings.json `"gpu"` raw; `ARCLIGHT_GPU=0` disables.
  Display writes Rgba8Unorm (an sRGB view decodes twice). GPU memory is
  up to `cache_mb + frame_cache_mb`.

**3D / render**
- Use `scene::Model::new(..)` (+ `.flags(params)`) and `scene::Card::new(..)`,
  not struct literals.
- `geom.rs`: right-handed, Y up, cameras look down −Z, Euler X then Y then
  Z. `solve.rs` uses the CV frame (y down, z forward); CV → scene flips
  diag(1, −1, −1). Splat files are Y down on disk; Read Splat Up = −y.
- Never build a lazy `OnceLock` inside rayon rows (it deadlocked).
- GPU instancing (P4b) is needed past ~1M instanced triangles; shadow-map
  lights cost maps × copies.
- Projections: `held_inputs` (an evaluator per held frame,
  `Elsewhen.held`); projector maps from `project::at_frame`,
  `project::SameMaps` across blur samples. Project 3D errors on an empty
  image.
- A primitive Cube needs `uv_layout` atlas to bake to one texture.
  Transform output is limited to one frame of overscan.
- Scanline Render ignores Width / Height with a background wired, and
  ignores point sets and curves; its `camera` param "" = the old path.

**Geometry**
- Stable ids per `docs/geo-plan.md` D5; `geo::matched` pairs by Int `id`,
  else index. `geo_mesh::weld()` keeps seams closed.
- `node.param` refs in per-element expressions go through `_refs`
  (`Expr::eval_elem` is Sync; graph lookups aren't thread-safe).

**Keying**
- Keyer and Color Difference Key (`key_color.rs`) are separate nodes so
  saved projects don't move. CDK's matte 1 − d(c)/d(s) is ill-conditioned
  where a screen pixel barely leads (gpu/check uses a picked screen).
- Edge Extend: pull-push; alpha-0 input pixels never seed; square reach.

**Roto / shapes**
- `ops::shape_sample` builds every outline (and each motion sample's).
  All-zero `point_feather` uses `shape::coverage` itself (bit-identical,
  test `old_projects_render_the_same`).
- Parallel list params (`point_feather`, stride 1) leave `points` (stride
  6 Roto / 2 Polygon) alone. Vertex insert / delete: `anim::edit_all`.
- `_follow` is injected per motion sample by `ops::resolve_at`; following
  shapes bypass resolve_linked's no-animation fast path.
- `closed` / feather falloff: see 0.14.30 in Status; pen logic and its
  tests (`drawing_with_events`, real egui events) in `gui/pen.rs`.
- Test gotcha: overlay tests' `gesture()` uses the unwarped `Xf`; for
  warped nodes drive `press` / `drag_to` with the drag's frozen xf.

**Camera tracker**
- Tracks live on the node as `tracks` ("ft1:" + base64 LE; ~0.7 MB for
  300 × 200, copied by each undo snapshot); deletions in
  `excluded_tracks`; `Solution.point_tracks` ties points to tracks (absent
  in old projects, so Delete is off there). Track & Solve replaces
  `tracks` and clears exclusions.
- A re-solve is fitted onto the previous one (similarity over ≥ 4 shared
  points) so origin / ground / scale survive; solve-made Point Clouds
  carry `from_tracker` / `point_tracks`; Fit Planes remap by track.
- `solve_quality.rs` grades by `Solution::point_rms`;
  `viewport3d::error_color` delegates to it.

**GUI / viewer**
- ⌫ over the viewer never deletes nodes (points / vertices first).
- Shortcuts skip when a text field has focus; egui's `consume_key(NONE,
  ..)` ignores extra Shift / Alt: use `consume_plain`, check Shift
  variants first. Plain Tab toggles 2D / 3D anywhere.
- "." pivot mode (`gui/pivot.rs`) runs before the overlays and takes Esc
  first; shape-box pivots are UI-only (`Overlays::sel_pivot`).
- Detached panes: `Panels.opened` hands egui the same ViewportBuilder
  every frame (a changing one made macOS windows flicker).
- All rfd dialogs go through `gui/dialogs.rs`.
- New egui tests end `ctx.run_ui(..)` with `.drop_without_applying_deltas()`;
  node-graph gestures use the `node_editor.rs` test `Harness`.
- Examples: add to `EXAMPLES` (`gui/main.rs`) and both packaging scripts
  together; `tests/examples.rs` checks them.

**Python / MCP / plugins**
- Rebuild the `.so` (`python/build.sh`) after engine changes. MCP HTTP
  has no auth: bind to localhost. Plugins are never unloaded.

**Packaging**
- Root `Cargo.toml` `exclude = ["examples/"]` keeps the sdist small.
- Untracked user files: `examples/media/musicmen.MP4` (gitignored),
  `examples/mad-martian.png` (ask before adding).

## Possible follow-ups

- Edge Extend: a Euclidean (round) reach if diagonal overreach shows.
- Shape follow: Bake (key the motion into the shape); the follow in the
  Tracker's Export menu.
- Per-point feather: a smoothstep blend in t if kinks show at vertices.
- `node_key`: skip the Camera Tracker's `tracks` string when hashing.
- GPU playback: skip the target read-back (lazy for the readout); a
  staging-buffer pool.
- Geo Info per-point selection and world-space values; say that Scanline
  Render's Width / Height yield to a background; notes: Project 3D
  node-name targets, Particles to Points id packing.

## Known issues and gaps

- Much interaction is verified by tests and screenshots, not by hand
  (TODO.md lists it). Nothing tested on real Windows; `arclight gpu
  --check` on the Apple M4 fails 13 / 81 (3D depth, 2026-11-13; see
  Status). Camera Tracker deletion / re-solve alignment not
  tried on real footage.
- Overlay handles misalign when the node's input format differs from the
  viewed image (view the node itself).
- Video: backward scrubbing re-seeks every frame; first frame after
  launch ~1.5 s.
- A bridged render uses the graph as of the request; a script edit
  during a drag commits the partial drag as its own undo step.
- `motion_blur_adds_up_on_the_gpu_as_on_the_cpu` once failed under heavy
  load; if it recurs, read `gpu::last_error()`.

## Working tips

- Omarchy / Hyprland: GNU `sed -i`; ffmpeg / ffprobe in `/usr/bin`;
  screenshots come out ~920–1030 px wide (tiled). Super+Space "Arclight"
  runs this checkout's `target/release/arclight-gui`.
- Screenshots: `ARCLIGHT_SCREENSHOT=/path/out.png
  ARCLIGHT_SCREENSHOT_DELAY=3 ./target/release/arclight-gui [graph.json]`.
  Temporary hooks marked `// TEMPHOOK`; remove with `sed -i '/TEMPHOOK/d'
  src/*.rs src/gui/*.rs`, then `grep -rn TEMPHOOK src`. No PIL: `magick`.
- Test scenes: hand-written graph JSON (nodes without `pos` are laid out);
  keyed shapes give exact ground truth for tracking.
- Tracker tests in release (`cargo test --release --lib track`).
- egui 0.37 differs from older docs; read
  `~/.cargo/registry/src/*/egui-0.37.0/src/` when unsure.
- Python tests: `python/build.sh && cargo build --release -p
  arclight-plugin-example && ARCLIGHT_PLUGIN_PATH=target/release
  PYTHONPATH=python python3 -m unittest discover -s python/tests -v`.
- MCP by hand: pipe JSON-RPC lines into `./target/release/arclight mcp`;
  HTTP: `arclight mcp --http 127.0.0.1:7899` + `curl -X POST
  localhost:7899/mcp -d '...'`.
- Bridge end to end: GUI with the screenshot hook and ~6 s delay, a
  Python script with `arclt.attach()`, then look at the shot. Close a
  window: `hyprctl dispatch "hl.dsp.window.close({ window = 'pid:<pid>' })"`.

Older detail: `git show 106707a:HANDOFF.md` (the full log before this condensing) and CHANGELOG.md.
