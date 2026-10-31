# Arclight — handoff notes

Last updated 2026-10-31, session 12: **phase 13 (procedural geometry,
GeoNodes-like, `docs/geo-plan.md`) on branch `wip-ns-geo`, 0.13.x, from
0.13.0 (the plan)**; phase 12 (GPU acceleration, `docs/gpu-plan.md`)
ended at 0.12.19 (beta v0.12.19, with the macOS DMG) on `wip-ns-gpu`,
now frozen; phase 11 (engine and platform: crash reporting, plugin
process isolation, beta v0.11.3) ended at 0.11.5 on `wip-ns-engine`,
now frozen; phase 10 (Gaussian splats
and follow-ups) ended at 0.10.37 (v0.10.37) on `wip-ns-splats`, now
frozen; phase 9 ended at 0.9.68 (v0.9.68) on `wip-ns-3dwork`, also frozen (herdr multi-agent sessions:
one Supervisor (formerly "master") + workers; see "Several agent sessions" below). Read this,
then `README.md` (user-facing features, file format, shortcuts,
scripting, the "3D view" section), `docs/phase-4-plan.md` (3D plan: M1–M13
done), `docs/help.md` (the in-app help) and
`docs/spec-coverage.md` (planning docs vs. what exists), and `docs/splat-plan.md` (Gaussian splats, S1–S5: done).

## Start here (next session)

- **0.13.0 (2026-10-31): phase 13 begins** on `wip-ns-geo` with
  `docs/geo-plan.md` (GeoNodes-like geometry: D1 scene wires, D2
  per-element expressions, D4 instancing; P1–P7; suggested first slice
  P1 + Scatter + Attribute Set + Instance on Points + Fit Plane). The
  user's two UI asks of the same day go first: drop a dragged wire on a
  node's body to connect it (Dev1) and the 3D gizmo starting as Move,
  switched by G / R / S (Dev2).
- **0.13.3, area-light step 1** (Dev2, user-approved): `raster.wgsl`
  `map_lit_grid` reads each footprint texel once where PCF taps fall a
  texel apart (integer softness ≤ 4, no penumbra radius), same tap order
  and weights (bit-identical 4K renders); map table words read once per
  lookup (`MapAt`). 4K AA3 Quality 16: softness 1 22.9 → 15.8 s, 2 51.2 →
  31.4 s, layered bench 35.1 → 22.8 s. Test
  `many_sample_area_lights_shadow_as_on_the_cpu`.
- **0.13.7, P1b: Transform Geo + drawing** (Dev2): `transform_geo`
  (ops.rs; world' = T(t + pivot) R S T(-pivot) · world, nothing
  re-baked; Group = node name or `*` glob; `cameras_lights` off by
  default). 3D view: point sets as one dot mesh (pscale at depth, 2–8 px,
  Cd or gray), curves as green lines, instances as a bounds box per node;
  Scanline Render ignores point sets and curves. P1 left: Dev1's
  spreadsheet + `geometry` query (P1c), then the v0.13 release.
- **0.13.5, P1a: the geometry model** (Dev2): `src/geo.rs`: `Attr`
  {Float, Vec3, Vec4, Int}, `Attrs` (length-checked, concat zero-fills,
  kind clash dropped, `transform` turns N / up / orient), `PointSet`,
  `Curves` (starts + closed), `Instances` (prototypes Model / Card /
  Splats, `which`, `worlds`, attrs; tint = Cd attr), `geo::matched` (by
  Int id, else index). `Mesh.point_attrs` / `face_attrs`. `Scene`
  `point_sets` / `instances` / `curves` through extend, tag, describe,
  at_time. The 3D view frames, picks and box-selects them (drawn in P1b).
  **Release rule (user, 2026-10-31): release v0.13.x when P1 is done**
  (P1a + P1b Transform Geo / drawing + Dev1's spreadsheet and `geometry`
  query).
- **0.13.4, area-light step 2** (Dev2): a `map_blocks` pass after each
  shadow map stores per 4×4 texels the nearest caster and the farthest
  opaque one (map table word 27, 2 words a block, counted in the maps'
  memory room); a lookup reads the blocks first and skips texel reads
  when its footprint is wholly lit (1) or shadowed (0). 4×4, not the
  proposed 8×8: 74–82% of lookups caught vs 65–70%. Bit-identical. 4K AA3
  Quality 16: softness 1 15.8 → 7.0 s, 2 31.4 → 12.0 s, layered 22.8 →
  10.6 s (from 22.9 / 51.2 / 35.1 before 0.13.3).
- **0.13.2, drop a wire on a node's body** (Dev1): `node_editor.rs`
  `body_input` (first free input whose kind fits, in port order, else the
  first that fits; none on a cycle via `doc.is_upstream`) and
  `body_output` (the reverse drag; not a Viewer); ports within reach win;
  the drop-on-a-wire Merge doesn't fire under a node. 0.13.6 (user's
  call): mask ports are left out of the body drop (`is_mask_port`); a
  mask takes a wire only on its own port.
- **0.13.1, gizmo starts as Move** (Dev2): `gizmo3d::Tool` defaults to
  `Move` (was `All` since 0.11.4); a plain G / R / S that starts its
  transform sets the tool (from Select or All too) and it stays after
  Esc; Alt+G/R/S and gizmo drags leave it. Test
  `viewer::tests::g_r_s_pick_the_gizmo_they_start`. Not tried by hand yet.
- **In flight (2026-10-31, after 0.12.19):** Dev1 (wD:p9): free, awaiting
  the next task. Dev2: profiling many-sample area-light shadows (20–34 s
  a 4K frame at 16 samples, even on the GPU), proposal before code. Macs:
  `arclight gpu --check` / `cargo test --release` on Metal not yet run
  (TODO). Released as the beta **v0.12.19** (Latest): Linux tarball,
  macOS universal DMG (built and uploaded by the user), `arclt` wheel +
  sdist; v0.12.17 stays Linux only (no DMG).
- **0.12.19, transport labels wrap with their fields** (Dev1):
  `src/gui/transport.rs`'s `together(ui, name, add)` keeps Frame, In,
  Out, FPS and Format with their fields; a group nested in a wrapping row
  never wraps itself (it runs past the edge), so the helper remembers each
  group's width from the last frame (egui temp data) and calls `end_row`
  when it won't fit. Test `a_label_wraps_with_its_field`.
- **0.12.18, GPU frame cache** (Dev1): `FrameCache` entries in
  `src/gui/worker.rs` keep, beside the CPU image, the color of frames
  rendered on the GPU (`gpu::Resident::color`: the render's own rgba
  buffer shared, no planes, no copy); a hit returns it as `Response.gpu`
  and draws through `gpu::display`. Its own LRU under
  `gpu.frame_cache_mb` (`gpu::frame_cache_budget`, default 1024, read
  once): past it frames lose only their GPU color; CPU eviction takes it
  along; GPU off drops all on the next request. `GpuPrefs` gains
  `cache_mb` / `frame_cache_mb` (saving Preferences used to drop a
  hand-set `gpu.cache_mb`). Measured: 1080p and 4K hold 24 fps (were
  ~20 / ~10), 4K@60 ~54 (was ~10); 1024 MB holds ~30 frames at 1080p,
  ~7 at 4K. Note: the cached color shares the evaluator's buffer for the
  frame just rendered, so GPU use is up to `cache_mb + frame_cache_mb`.
  Later: skip the target read-back during playback (lazy, for the
  readout).
- **0.12.17, toolbars wrap** (Dev1): `Viewer::toolbar` and `toolbar_3d`
  use `horizontal_wrapped`; an overflowing row also widened everything
  under it (status row, canvas). Test `the_toolbars_wrap_in_a_narrow_view`.
  README's Scanline Render paragraph updated for see-through shadows.
  Released as the beta **v0.12.17** (the first phase-12 release; Linux
  tarball + wheel + sdist, notes "macOS DMG to follow"; the DMG is built
  by the user while Actions is billing-blocked).
- **Session 12, 0.11.4 (Supervisor, wD:p1, alone):** the user found the
  3D view hard to use in 0.11.3 (no rotate / scale gizmos; rotating
  "swung wildly back and forth and up and down"; mouse: Razer DeathAdder
  V3 on Hyprland, no trackpad). Changes: `gizmo3d::Tool` defaults to
  `All`; ring drags go by travel along the ring's screen direction
  (`Layout::ring_tangent`, `Gesture::along`, set in
  `viewer::gizmo_input`) instead of the angle about the center, which
  flipped on edge-on rings; free / axis R damps the angle within
  `TURN_RADIUS` (60 pt) of the center; `ViewCam::orbit_about` +
  `Viewport3d::orbit` orbit about the selection's median
  (`View3dPrefs.orbit_selection`, default on); Linux fine scroll steps
  within `WHEEL_SPELL` (1 s) of a line step count as the wheel
  (`Navigation::scroll`'s `wheel_recent`). Not tried by hand yet (TODO).
- **0.11.5 (Dev1):** the 20 GUI tests that panicked with epaint's
  "Dropped TexturesDelta with 1 unapplied deltas" (egui 0.37) now end
  `ctx.run_ui(..)` with `.drop_without_applying_deltas()` instead of
  `let _ =`; all GUI tests pass. **Tip:** do the same in new egui tests.
- **0.12.16, layered shadows** (Dev2): see-through shadow casters render
  on the GPU (up to 4 layers a texel, more → CPU pixels; layer count
  shrinks 4→2→1 under a 384 / 160 MB budget, then the frame goes to the
  CPU). README's note on see-through casters still says CPU (Dev1 to
  update). Many-sample area lights stay slow (20–34 s at 4K): follow-up.
- **0.12.15, error wrapping** (Dev1): long node errors wrap and are
  selectable in the viewer and the 3D view; the status line truncates
  to fit with the full text on hover. Known: the viewer toolbar's Fit
  and 1:1 buttons are cut off at the default window size (Dev1, next).
- **0.12.13, 3D view zero-copy** (Dev1 + Dev2):
  `render3d::render_preview_gpu` → `Option<gpu::Resident>` (a fresh
  `GpuImage::blank` per call; None for motion blur, see-through shadow
  casters, overflowed pixels, GPU off); `SceneResponse.gpu_render`, and
  `SceneResponse::image()` reads back on demand for anything needing
  pixels; `gpu::display::Channel::Premultiplied` is the 3D view's mode.
  One unexplained failure of
  `render3d::gpu::tests::motion_blur_adds_up_on_the_gpu_as_on_the_cpu`
  (line 255, a plane over tolerance) in a full lib run under heavy load
  (363 s instead of ~33); not reproduced in 14 + 4 runs, margins ~1000×.
  If it recurs, read its message (Dev2 is adding `gpu::last_error()` and
  the path taken to it).
- **0.12.10, GPU G6** (Dev2): `src/render3d/gpu/splat_view.rs` +
  `splat_view.wgsl` prepare the view on the GPU and keep it there (only
  the pair count comes back); `raster.wgsl` blends it, a tile's
  workgroup loading 256 splats at a time, merged per sample with
  surfaces. `splats::Prepared` holds the GPU view; the CPU's view is only
  built if the CPU renders, before its parallel loop (a lazy OnceLock
  inside rayon rows deadlocked). Per-frame tables share one words buffer
  (portable 8-buffer limit). Follow-ups: keep the splats-alone blend
  between sample rows (most of the 4K cost), splats in the GPU
  motion-blur sum. **The plan's G1–G7 are all done.**
- **0.12.9, GPU G7** (Dev1): `gpu::for_window` hands the engine's
  device to eframe (`WgpuSetup::Existing`; None with ARCLIGHT_GPU=0, no /
  software adapter, GL, or without the gpu feature: eframe makes its
  own). `gpu::display` (+ display.wgsl) writes an **Rgba8Unorm** texture
  (egui's shader takes the sRGB-encoded bytes as they are; an sRGB view
  decodes twice), registered as a native egui texture, only when the
  result lives on the window's own device. `Evaluator::evaluate_resident`,
  worker `Response.gpu`; the read-back stays (readout, pickers). Viewer
  and Media Out pass their input on as an Arc. Follow-ups: 3D view
  zero-copy (Dev2's renderer output on the GPU), a GPU frame cache for
  playback, single-pixel readout from the GPU (no read-back at all).
- **0.12.7, GPU G5 done** (Dev2): `render_blurred`'s GPU sum takes
  sprites: each (cell, time) renders into a scratch frame, `blur.wgsl`
  resolves it to picture + depth, that time's sprites go over it on the
  GPU (`sprites::Job`), then it's added; one read-back. Additive sprites
  over a background go time by time. For 500k sprites ~75 ms a time is
  CPU work (sort ~22, binning ~12…): moving sort + binning to the GPU is
  the next lever if dense particle blur matters.
- **0.12.6, GPU G5 part 2** (Dev2): `src/render3d/gpu/sprites.rs`:
  sprites sorted and binned on the CPU, one GPU thread a pixel lays its
  tile's sprites as the CPU does; used only when the sprites' boxes
  cover more than 2× the frame (tiny sprites are faster on the CPU).
  Next: sprites in the GPU motion-blur sum (before G6). Note: the
  Supervisor's verification worktrees are `../.verify/sup-*`.
- **0.12.4, GPU G3 + exit + crash handler** (Dev1): `ops::WarpMap` /
  `keyer_plan` / `resample_layout` are the single source for CPU and GPU
  maths; `gpu::ops::eval` takes a `CpuInput` closure for ops needing a
  CPU view; warp tolerance `check::WARP_SHARE`; `Evaluator::run_all`
  reads back only what CPU ops need. **Exits go through
  `gpu::exit_process`** (CLI, GUI: wait for idle, flush, `_exit`; the
  device is never dropped); Python's atexit hook only waits (never
  `_exit`). `tests/cli_exit.rs` guards "everything is written before
  _exit". crash.rs native handler: static buffers (Rust threads' signal
  stacks are 8 KiB), `alarm(3)`, re-entry guard, report before the
  backtrace; `crash::tests::crashes_leave_a_report` covers panic /
  worker / segv / thread / wild jump / stuck. Open: `arclight mcp --http`
  SIGINT / SIGTERM handler: done in 0.12.5 (the handler only sets an
  atomic; `serve_http` returns, so main ends through `exit_process`).
- **0.12.3, GPU G5 part 1** (Dev2): shadows and motion-blur
  accumulation on the GPU. `shadow::plan` is the single source of map
  placement for both renderers; `gpu::blurred` is tried first in
  `render_blurred` (falls back to the per-time loop). Still CPU: shadow
  casters that let light through; blur frames with sprites, points,
  splats or a background with planes. 4K MB8 + shadows 0.87 s (plan
  goal < 0.3 s; next lever: share shadow maps between nearby times).
  Particles (G5 part 2) next.
- **0.12.2, GPU G4** (Dev2): `src/render3d/gpu/` is a compute
  rasterizer copying the CPU's per-sample algorithm (D6 in the plan, as
  rewritten), dispatched at the top of `draw_passes`; `Slots`,
  `next_positions`, `draw_points`, `add_planes` are shared with the CPU,
  and `draw_cpu(.., only)` renders a pixel mask. Deep see-through stacks:
  16 layers, then a rerun keeping 64, then those pixels on the CPU. Lib
  unit tests keep the GPU renderer off unless a test sets
  `render3d::gpu::tests::GPU_ON` (the old render3d tests check exact CPU
  values); the GPU is held to D8 in its own tests. Warm render_preview
  1080p Lit 21 ms vs CPU 61 ms on a quiet machine. Under heavy CPU load
  (builds) the GPU path suffers more: half its time is still CPU work
  (setup, read-back, finishing), which G7 removes. Seen once in 17
  bench runs: SIGSEGV at process exit inside NVIDIA's shader-compiler
  thread (the device is a static, never torn down): Dev1 to add a
  `gpu::shutdown()` before exit.
- **0.12.1, GPU G2** (Dev1): Evaluator cache entries `Res::{Cpu, Gpu}`
  (graph.rs; read back once on first CPU need), `gpu::ops` (dispatcher;
  the uber-shader's op codes must match `pointwise.wgsl`'s consts),
  policy `gpu::ops::preference` (Blur always on the GPU, pointwise only
  when an input is already there, Constant / Checkerboard on the CPU
  until G7), `gpu::backend_tag(op)` in `node_key`. **`GpuImage` clones
  share buffers: kernels must never write into an input's buffer.**
  Shuffle and zmerge stay on the CPU. CPU Blur is a running sum.
  Follow-ups: `arclight render` reads back every node (evaluate_all):
  read back only outputs; a staging-buffer pool (4K round trip ~55 ms).
- **0.12.0, GPU G1** (Dev1 core, Dev2 Preferences): `src/gpu/` (mod,
  image, check, image.wgsl): lazy headless device, `gpu::{status, get,
  set_enabled, enabled, cpu_only, test_device, report_error, crash_line}`,
  `GpuImage` upload / read_back (staging, or shared buffers on integrated
  GPUs), `arclight gpu [--check] [--json]`, `--cpu`, MCP / Python
  gpu_info / gpu_check; `render3d::gpu_checks()` is the hook Dev2's 3D
  rows go in. `enabled()` reads settings.json `"gpu"` raw, so every front
  end follows the preference. GPU tests start with `let Some(gpu) =
  gpu::test_device() else { eprintln!("skipped: no GPU adapter");
  return; };`. The naga test translates every `src/**/*.wgsl` to MSL.
  The crash dialog's GPU suggestion keys on the "GPU acceleration: on
  (<adapter>)" line or a backtrace through wgpu / `arclight::gpu` /
  `render3d::gpu`, not the "GPU:" line (egui's adapter, in every GUI
  report). `cpu_only` is thread-local: carry it if G2+ spreads nodes over
  threads. G2's first step: Evaluator cache keys mix in whether the GPU
  is in use. 4K round trip ~60 ms (staging buffer allocated per readback;
  a pool is a G2 follow-up). Next: Dev1 G2, Dev2 G4.
- **Phase 12 (user, 2026-10-30): GPU acceleration (wgpu), 0.12.x on
  branch `wip-ns-gpu`** (from `wip-ns-engine` at 0.11.5, now frozen;
  `main` follows it). Plan: `docs/gpu-plan.md` (Dev2). The user wants a
  Preferences entry to turn GPU acceleration on / off.

- **`TODO.md`** (repo root, added 2026-10-29 at the user's request) is
  the running list of what's still wanted: hand checks, follow-ups,
  larger items. Keep it current: add items as they come up, tick or
  remove them when they ship.

- **Session 12, latest, 0.10.23–0.10.27 (released v0.10.25 and
  v0.10.26; v0.10.26 has its DMG, v0.10.25 has none and the user skipped
  it):** Interface group: 0.10.23 (Dev2) `consume_plain` in
  `gui/main.rs` (egui's `consume_key(Modifiers::NONE, ..)` matches
  logically and ignores an extra Shift / Alt), so Shift+Tab over the
  viewer no longer toggles 2D / 3D; 0.10.24 (Dev1) backdrops from scripts
  (`Session::backdrops` / `create_backdrop` / `update_backdrop` /
  `delete_backdrop`, one undo step each; MCP `list_backdrops` …; Python
  `arclt.backdrops()`, `create_backdrop`, `Backdrop`; `NODE_W` / `NODE_H`
  moved to `graph.rs`, `Backdrop::COLOR_NAMES`); 0.10.25 (Dev2, from its
  key audit) `player_keys` with exact modifier matching (Alt only for
  arrows), Home / F over a hovered 3D view stay there (no transport seek),
  plain-Tab-only formula completion (Shift+Tab moves focus back), viewer
  G / R / S plain only. 0.10.26 a real splat sample,
  `examples/geo/hornedlizard.spz` (18 MB, 786 k splats, SH 3, unchanged
  from nianticlabs/spz under MIT, notice in
  `examples/geo/hornedlizard.LICENSE.txt`) and `examples/hornedlizard.json`
  (Read Splat → Splat Crop box 6 → Camera → Scanline Render → Viewer; the
  capture has a background shell of big splats ~46 units out, so a
  camera inside it sees a flat colour until cropped). Reads upright with
  the -y default: real-file confirmation of 0.10.14's `.spz` turn. Graph
  files resolve relative media paths only when `project.folder` is set
  (`Graph::from_file` → `resolve_paths`), and a `project` block needs
  `format`, `fps`, `first_frame`, `last_frame`. 0.10.27 root `Cargo.toml`
  `exclude = ["examples/"]`: the arclt sdist bundles the crate with cargo's
  file list (maturin's own `exclude` didn't drop it), now 851 KB instead
  of 20 MB; checked by building arclt from the sdist (14 tests pass).
  v0.10.26's sdist is still the 20 MB one. **Next (proposed to the user,
  not yet approved):** a small round (Dev1 `set_node_position` + the
  clippy warnings; Dev2 the bridge calls that hold the shared evaluator
  while simulating), then GPU acceleration (wgpu) as phase 11 with a
  written plan first. Both workers idle.
- **Session 12, phase 10 continued, 0.10.14–0.10.22 (released v0.10.14
  and v0.10.22; Linux assets built locally, the user uploads the macOS
  DMG while Actions is billing-blocked):** 0.10.14 (Dev1) Read Splat Up
  axis defaults to -y; `.spz` read/written turned 180° about X (positions,
  quats, SH parity signs) so every format is Y down on disk, upright by
  default; the sample `.ply` regenerated Y down. Camera Tracker group:
  0.10.15 (Dev1) mask input (`features::Blocked`, `camtrack::input_frames`);
  0.10.16 (Dev2) live link to the solve (`link.rs` kind `solve`,
  `camtrack::camera_links` / `solved_value` / `baked_value`, Session
  `export_camera_linked`, MCP `linked` default true, Properties Bake);
  0.10.17 (Dev2) point picking over a Scanline Render
  (`camera_tracker::tracker_for`); 0.10.18 (Dev2) `ops::other_frames`
  fills Scanline Render defaults; 0.10.19 (Dev1) lens-distortion solve
  (`solve::Intrinsics` k1/k2 = Lens Distort's Brown model about the frame
  center, bundle Jacobian through the lens, `Solution.k1/k2`, Export adds
  a Lens Distort undistort). 3D and rendering group: 0.10.20 (Dev2)
  `geom::Camera.ortho`, Camera Projection / Ortho width, ortho
  render3d / splats / sprites / 3D view / sun shadow maps; 0.10.21 (Dev2)
  mipmaps (`render3d/mip.rs`, analytic per-sample footprint, trilinear
  only when minified, else bit-identical); 0.10.22 (Dev1) Alembic
  (`src/ogawa.rs`, `src/alembic.rs`; Read Geo `.abc`, Frame rate seeded
  from the project, Frame offset; no materials, HDF5 refused). Follow-ups
  are in TODO (Alembic curves / points / cameras; splat GPU path).
  The master was renamed **Supervisor** (herdr `supervisor`, wD:p1) by
  the user on 2026-10-30.
- **Session 12, phase 10 (splats), 0.10.0–0.10.13 on `wip-ns-splats`:**
  0.10.1 `splat::Splats` (Arc'd SoA arrays) + `Scene.splats`; 0.10.2 the
  `.ply` reader + `read_splat` + `examples/geo/splats.ply`; 0.10.3
  `render3d/splats.rs` rasterizer (EWA, SH 0–3, tiles, merged per AA
  sample with surfaces in `draw_passes`) + viewport dots; 0.10.4 pick /
  frame / box; 0.10.5 `scene_objects` + paged `splats` (MCP, Python);
  0.10.6 settled 3D view render; 0.10.7 Splat Crop + `Splats.holdout`;
  0.10.8 passes / holdout / motion blur; 0.10.9 Splat Color
  (`graded_sh`); 0.10.10 `Scene.regions`; 0.10.11 crop handle
  (`GadgetKind::Crop`); 0.10.12 `.splat` / `.spz` (dep `flate2`);
  0.10.13 tile binning without a global sort + `View` LRU cache, preview
  SH ≤ 1. 1 M splats 1080p ≈ 0.17 s. (Up default became -y in 0.10.14.)
  Master's ship script lives in the session scratchpad: snapshot reported
  paths, test at HEAD in a worktree, commit those blobs, bump in place,
  build the launcher from the committed tree.
- **Session 12, fourth round:** 0.9.62 (Dev1) sprite texture sequences;
  0.9.63 (Dev2) the bridge shares the GUI's evaluator
  (`Session.eval: Arc<Mutex<Evaluator>>`, `gui/bridge_calls.rs`); 0.9.64
  (Dev1) emitters on animated geometry step on (`particle_nodes::
  input_frames`, `History`); 0.9.65 (Dev1) Preferences window
  (`gui/preferences.rs`, `config::{InterfacePrefs, NewProjectPrefs,
  FilePrefs, View3dPrefs, RecentFiles}`, autosave, Open Recent); 0.9.66
  (Dev2) New project defaults, undo levels, 3D view `Navigation`; 0.9.67
  (Dev1) `Project.folder`, New project modal, dialogs start there; 0.9.68
  (Dev1) relative paths at the file boundary (`Graph::save` /
  `to_json_at`, `Project::resolve_path`). Splat plan written
  (`docs/splat-plan.md`); the user wants it to start phase 10 (0.10.0) on
  a new branch `wip-ns-splats`. **Released as v0.9.68** (Latest): Linux
  tarball, Linux wheel + sdist built locally (Actions still billing-blocked);
  the macOS DMG and universal2 wheel await the user's Mac or Actions.
- **Session 12, third round:** 0.9.58 (Dev1) the active Viewer
  (`Doc.active_viewer`, `Doc::viewer()`: last wired / viewed / clicked,
  else the first) fixes the user's "black background" Merge report (a
  second Viewer never showed); side notes: a Merge works in B's frame, an
  identity Transform grows the data window by 1 px. 0.9.59 (Dev2) particle
  dots streak in the settled blurred 3D preview (`SceneResponse.blur_times`,
  `streak`); 0.9.60 (Dev1) Scanline Render `jitter` (Jitter shutter,
  default on; off = the 0.9.54 render exactly; `Blur.jitter`,
  `ops::blur_jitter`); 0.9.61 (Dev2) looked-through camera motion in the
  blurred preview (`SceneRequest.through`, `frame_camera`). Motion blur
  follow-ups all done. Not released yet (user: "not yet"); Actions was
  billing-blocked at v0.9.57 (user built the DMG on a Mac). The user
  turned down narrowing Blur's Radius to 0–100. `examples/mad-martian.png`
  is the user's test PNG, untracked (ask before adding).
- **Session 12, second round (user approved the master's picks):**
  0.9.52 viewer.rs scene_of takes extra inputs (Dev2); 0.9.53 Read Geo
  Opacity (Dev1); 0.9.54 view prefs in `$XDG_STATE_HOME/arclight/view.json`
  (`config::ViewPrefs`, `Viewport3d.new_view`) (Dev1); 0.9.55 per-pixel
  Bayer-interleaved shutter jitter in render3d (blurred renders differ in
  grain from earlier versions) (Dev2); 0.9.56 `Particle.accel`,
  `Particle::moved` (Dev2); 0.9.57 `render_preview_blurred`,
  `ops::blur_times`, viewport blur when settled (≤ 8 samples) (Dev2).
  **Released as v0.9.57**, but GitHub Actions was blocked by an account
  billing problem ("recent account payments have failed"), so the user
  built the universal DMG (and an arm64 one) on a Mac and uploaded them
  by hand; checked: both binaries universal (x86_64 + arm64), 0.9.57.
  The master then built the Linux `arclt` wheel (maturin --zig,
  manylinux2014) and the sdist locally from a clean worktree of the tag,
  tested the wheel in a fresh venv with the example plugin (12 OK) and
  attached both. Still missing: the macOS universal2 wheel (needs Actions
  or a Mac). Master workflow: a ship script snapshots the
  reported paths, tests them alone at HEAD with the bump, commits those
  blobs only; workers keep README / help / CHANGELOG / TODO to the master.
  The ship script builds in `../.verify/target`, so it does NOT update
  `target/release`, which the Omarchy launcher runs: run
  `cargo build --release` in the repo after commits (it sat at 0.9.48
  until 0.9.68).
- **Session 12 (2026-10-29, master wD:p1; workers **Dev1** = herdr `dev1`
  (wD:p3, formerly mathwiz / MathWiz) and **Dev2** = herdr `dev2` (wD:p4,
  formerly worker3d / 3DWorker), renamed at the user's word):** from the user's hand test of the 0.9.48 camera setup:
  0.9.49 node browser on ⇧Tab (MathWiz); 0.9.50 Card **Shading**
  (unlit default, lambert / blinn, two-sided, Receives shadows;
  `Card.material: Option<Material>`, `Surface.two_sided`) (MathWiz);
  0.9.51 (3DWorker) the setup's Camera starts at z = focal / film back
  width (it sat at the origin, in a new Card's plane, so the render was
  black), `worker::scene_of` includes a render's extra inputs (lights
  there had no icon / handles), Look through toggle in Camera / Spot /
  Area Light Properties. **Released as v0.9.51.** Ask the user how these
  test; their old test project still has its camera at z = 0. The user hand-tested 0.9.49–0.9.51 (Card Shading, Opacity, Blinn) and it works. v0.9.51 checked: both workflows passed, all five assets attached, marked Latest. A Card
  can't go into the render's background (image) port: Scene or the
  render's last input.
- **State (before session 12):** version **0.9.48** on branch **`wip-ns-3dwork`**, merged to
  `main`; **M12 (lights) and M13 (particles) are DONE**. The local build
  (`target/release`) is 0.9.48, so the Omarchy launcher runs 0.9.48
  (the Python `.so` is 0.9.46). Both workers idle; nothing uncommitted.
- **Latest release: v0.9.48** (marked **Latest**;
  github.com/nsoler/arclight/releases/latest): Linux x86_64 tarball,
  macOS universal DMG, `arclt` wheels + sdist (both workflows passed,
  all five assets checked). Earlier: v0.9.47, v0.9.46, v0.9.33, v0.9.32,
  v0.9.31, v0.9.30, v0.9.27, v0.9.11. The stray v0.9.7 draft release was deleted
  at the user's request (the published v0.9.7 release and tag remain).
- **Answered (session 12):** the user hand-tested everything through 0.9.51, the macOS DMG (Apple Silicon and Intel) and the 3D view with mouse / trackpad, and ticked it all in TODO.md. **Was:** ask the user how 0.9.34–0.9.48 test (0.9.48: motion blur, Lit / Solid view, and the camera setup now placed before the Viewer — the user hand-tested the 0.9.30 setup and asked for that, so ask if it now feels right; 0.9.47: Card Opacity, frame size labels, square About logo); node graph Tab /
  ⇧R / ⇧T / ⇧M; Area Light, falloff, radius, exposure, Look at; light
  handles and Look through selected (0); shadows and the shadow pass;
  Light 2D; particles (Emitter → forces → Particle Render → Scanline
  Render's growable inputs, Particles 2D, dots and gizmos in the 3D
  view). Then the older open questions below.
- **Known limits (updated session 12):** sprite sequences, incremental
  animated emitters and the shared bridge evaluator shipped in
  0.9.62–0.9.64; see TODO for the bridge's remaining lock limit. Shadows cost time (soft spot + 6×6 area ≈ 3.5× unshadowed);
  motion blur costs about one render per sample (8 samples ≈ 8×).
- **Next (no plan yet; ask the user):** motion blur follow-ups
  (per-pixel time jitter against banding at low samples, acceleration in
  particle streaks, blur in the 3D view preview); particle follow-ups (a
  sprite sequence input, reusing the GUI's evaluator in the MCP bridge); an Opacity on Read Geo to fade file
  materials without Override material (3DWorker's suggestion); saving
  view preferences (e.g. Show frame size) between runs (eframe
  persistence is off); the M9 leftover (true ortho texture path); newer clippy flags old code
  (graph.rs `from_str` naming, plugins/example `chunks_exact`), harmless
  but could be tidied; other items under "Next steps".
- **0.9.47 (after M12 / M13, user requests):** Card **Opacity**
  (3DWorker; `Card.opacity`, surface color [opacity;4]; data passes take
  the nearest hit with alpha × opacity ≥ 0.5, else the nearest; shadows
  let through 1 − alpha × opacity, faint casters kept per texel as
  layers in shadow.rs — semi-transparent meshes and soft-alpha cards now
  cast partial shadows; no separate Opacity on mesh nodes, Color alpha
  is theirs); viewer frame size labels + View ▸ Show frame size
  (session-only) and a square, undistorted About logo (`square_uv`;
  egui's Image kept the whole texture's aspect and squeezed it)
  (MathWiz). Released as v0.9.47.
- **0.9.48 (user requests after 0.9.47):** Scanline Render motion blur
  incl. particles (3DWorker: params `motion_blur` (Int samples, 0/1 off),
  `shutter`, `shutter_offset`, like other blurring nodes; stratified
  temporal supersampling; `Evaluator.others: BTreeMap<offset,
  Evaluator>`, `ops::other_frames`, `scanline_render_frames`,
  `Scene::at_time`, `scene::between`; `ParticleStream.shift`; particle
  motion vectors). Follow-ups: per-pixel time jitter, acceleration in
  particle streaks, blur in the viewport preview. 3D view Solid / Lit
  (MathWiz: `render3d::Preview {Solid, Lit {shadows}}`,
  `ViewCam.shading / shadows`, ⇧Z, toolbar, View menu). Camera setup
  before the Viewer (MathWiz: `Doc::add_camera_setup` priority: targeted
  wire > first Viewer's input > new Viewer; `last_image()`). The user
  hand-tested the 0.9.30 camera setup and asked for this placement.
- **M12 (lights), done 0.9.35–0.9.43:** Area Light, falloff, radius,
  exposure / enabled, look at (0.9.35, 3DWorker); 3D view handles + Look
  through selected (0.9.37); Light 2D engine (0.9.38, MathWiz) and node
  (0.9.42); shadows + shadow pass (0.9.40); `scene_objects` for MCP /
  Python (0.9.43). Per-version notes further down.
- **M13 (particles), done 0.9.36–0.9.46 (MathWiz; scripting / docs by
  3DWorker):** engine `particles.rs` (0.9.36), render engine
  `particles_render.rs` (0.9.39), nodes `particle_nodes.rs` (0.9.41),
  hooks (0.9.44: lazy `ParticleStream` in `Scene.particles`,
  `ops::Kind::Particles`, `ops::port` / `growable`, Scanline Render's
  growable inputs, `src/render3d/particles.rs`, viewport dots),
  `particles` MCP / Python (0.9.45), emitter / force icons and 2D
  handles (0.9.46).
- **Release v0.9.46 checked:** both GitHub workflows passed, all five
  assets attached, marked Latest.
- **Answered in session 12 (all work), was unanswered from earlier:** whether `musicmen.MP4` plays from
  the GUI (0.9.32), the file-dialog fix when navigating folders (0.9.31),
  0.9.30 features (Fit to project, Project Settings, 3D setup), 0.9.33
  (Merge swap, mask channel, centered transport, Fit), and the Mac DMG.
- **Constructors:** use `scene::Model::new(mesh, world, material)`
  (+ `.flags(params)`) and `scene::Card::new(image, world, size)`, not
  struct literals.
- **Release tips learned in session 10:** create releases with
  `--latest` (user wants /releases/latest to work); GitHub's macOS arm64
  runners sometimes aren't acquired ("capacity constraints", the job
  fails after 15 min) — just `gh run rerun <id>`; verify in
  `../.verify/` (on /home), not `/tmp` (tmpfs fills up).
- **Done (session 10): M10, 3D objects and render passes**
  ("Milestone 10 in detail" in `docs/phase-4-plan.md`), 0.9.12–0.9.27,
  released as v0.9.27. Per-version notes below.
  - **Committed (engine side mostly by MathWiz):**
    - 0.9.12 `src/mesh.rs`: `Mesh { name, positions: Vec<Vec3>, normals,
      uvs: Vec<[f64;2]>, triangles: Vec<[u32;3]> }` (f64, per-vertex,
      CCW from outside, UV v up, object space; world / ID / flags live on
      the Scene entry), `Material { name, shading: Unlit|Lambert|Blinn,
      color, opacity, texture: Option<PathBuf>, specular: [f32;3],
      roughness, emission }` (default Unlit white), primitives cube /
      sphere / cylinder / plane (XY facing +Z, like Card), helpers.
    - 0.9.13 (3DWorker) render3d triangle core: `Surface`, `card_surface`,
      `surfaces(scene)`, row binning, per-sample depth sort; cards
      bit-identical to before; 80k tris 1080p 3×3 ≈ 46 ms.
    - 0.9.14 OBJ + MTL: `mesh::read_obj` / `parse_obj` → `Vec<Part {name,
      mesh, material}>`, `parse_mtl`; sample `examples/geo/crate.obj`.
    - 0.9.15 image.rs: EXR planes as `layer.channel` (`depth.Z`, `P.x`,
      `matte.<node>.A`; bare `Z` still reads as depth); Shuffle engine
      (`layers`, `channel`, `shuffle`, `copy_to_plane`, `layer_to_rgba`,
      `rgba_to_layer`, `remove_layers`).
    - 0.9.16 `src/shade.rs`: lights Ambient / Directional / Point / Spot
      {cone, penumbra} / Area {size} (`Light {kind, color, intensity,
      decay, world, node}`, aims down −Z); `shade(hit, material, albedo,
      lights) → Shaded {diffuse, specular, emission, ambient}` (sums to
      the beauty), per-light `light()`; no lights or Unlit = color as is.
    - 0.9.17 `src/gltf.rs`: `gltf::read_gltf` / `parse_gltf` → `Vec<Part>`
      (world space, PBR mapped; embedded textures saved to
      `<tmp>/arclight_gltf/<hash>.png`); dep `gltf` 1.4; Rust ≥ 1.88.
    - 0.9.18 `Image::zmerge(a, b)` (depth 0 / none = empty / background;
      coverage passes blended over, data passes from the nearer).
    - 0.9.19 (3DWorker) (a2): `Scene.meshes: Vec<Model {mesh: Arc<Mesh>,
      world, material, texture, holdout, name, node}>`; nodes cube /
      sphere / cylinder / plane / read_geo (cached by path + mtime);
      `render3d::render_preview` (headlit, viewport); viewport solid /
      wireframe (Z), ray-triangle picking. `holdout` param read, not yet
      rendered. `Image::bytes` doesn't count mesh memory yet.
    - 0.9.20 (MathWiz): `shuffle` node (Text params in / red / green /
      blue / alpha / out / remove), Merge `zmerge`, `ParamKind::Text`,
      stats `layers`, `Image::bytes` counts meshes, README rows, help
      "Render passes". The ▾ list doesn't show the input's actual layers
      yet (needs main.rs probe for shuffle).
    - 0.9.21 (3DWorker) (b1): `render3d::Passes {depth, position, normal,
      uv, id, mattes}`, `render_passes(..)`; holdout rendered; viewer
      buttons per layer. Card output still bit-identical to pre-M10.
    - 0.9.22 (3DWorker) (b2): `ParamKind::Flags`, scanline_render
      `passes` (default ['depth']), `holdout` on mesh nodes, Read Geo
      file filter, Shuffle ▾ from real input layers. (a) and (b) done.
    - 0.9.23 (3DWorker) (d1): `Scene.lights: Vec<shade::Light>`,
      `scene::light(op, params)`, nodes light_ambient / light_directional
      / light_point / light_spot (decay default 0, cone 40, penumbra 5).
    - 0.9.24 (MathWiz): light icons in viewport3d, picking / box /
      A / framing / object_world include lights.
    - 0.9.25 (3DWorker) (d2): material params on mesh nodes (shading
      default lambert, specular, roughness, emission; read_geo
      `override_material`), `render3d` shades each hit via `shade::shade`
      (unlit / no lights bit-identical), passes diffuse / specular /
      emission / ambient / lights (`light.<node>.R/G/B`); help "Lights".
      Viewport solid view still headlit. **(d) done.**
    - 0.9.26 (MathWiz): M9 leftovers: `gizmo3d::median`,
      `Gesture::about(pivot)`, `Transforming::start_many / drag_many`;
      viewer G/R/S and gizmo act on `doc.selection()`. Only the true
      ortho texture path (render3d) remains of M9's leftovers.
    - 0.9.27 (3DWorker) (e): `motion` pass (`render_motion`, next-frame
      scene + camera via `Evaluator.next`, keys mixed in;
      `ops::needs_next_frame`, `scanline_render_next`). **M10 done.**
- **Session 10, after M10 (user requests, release when all land):**
  (1) Fit to project on Read / Media In — 0.9.29 (3DWorker, `ops::fit_to_project`, `fit` param default 'none' in spec, 'fit' for new nodes via `fit_new_shape`, `project_format` injected by `apply_format` so it's in the cache key; shared `resample`); (2) Project Settings
  window — 0.9.28 (MathWiz, `src/gui/project_settings.rs`, File menu,
  ⇧⌘P); (3) 3D camera setup on wires (selectable wires, right-click
  "Insert 3D Camera Setup", backdrop, first-time switch to 3D view and a
  glowing 2D|3D toggle) (3DWorker, after (1)). Help lines for Project
  Settings are with 3DWorker (holds help.md).
  - **Left over from M10:** the viewport's solid view is headlit (not
    lit by scene lights); Area light has engine support (`shade.rs`) but
    no node (M12); shadows (M12); true ortho texture path (M9).
- **0.9.31 (MathWiz): ANR fix.** All rfd file dialogs run on a thread
  via `src/gui/dialogs.rs` (`start` → `Pending<T>`, polled each frame,
  one at a time, parented to the window); unsaved-changes prompts are an
  `egui::Modal`. No rfd calls outside dialogs.rs. Navigating folders in
  the portal picker by hand still to be confirmed by the user.
- **0.9.32 (3DWorker): video in Read, upper-case extensions.** Read
  decodes video via media (frame 1 = clip's first); `dialogs::filter`
  adds upper-case extensions; ffmpeg `-map 0:v:0 -an -sn -dn`, a reader
  thread one frame ahead, read-on for jumps ≤ 24 frames. Test clip
  `examples/media/musicmen.MP4` (user's, 71 MB, 4K) is **gitignored**,
  not committed; a test reads it only when present. Limits: backward
  scrubbing seeks every frame (~2 fps on long-GOP 4K); full-4K playback
  ~6–7 fps.
- **0.9.33 (both): Merge swap + mask channel / invert (3DWorker),
  centered transport + node graph Fit button (MathWiz).** merge `swap`
  (inputs 0/1 exchanged in `ops::eval`); `mask_channel` / `mask_invert`
  on all 12 mask_base ops; violet mask ports (`MASK_PORT_COLOR`,
  `is_mask_port`); transport row centered via measured width
  (`transport::indent`); `node_editor::fit_view` + corner `fit_button`
  (F; Home is Go to start); ZOOM_MIN 0.25.
- **Session 11 (started, same day): M12 (3DWorker) and M13 engine
  (MathWiz); the user wants the release only after both.** 0.9.34
  (MathWiz): node graph hotkeys (`NodeEditor::hotkeys`: Tab browser,
  ⇧R Media In, ⇧T Transform, ⇧M Merge, only over the graph).
  0.9.35 (3DWorker) M12 (a): `light_area` node (`LightKind::Area {size,
  disk, two_sided, samples}`), Spot `falloff`, `Light.radius / enabled`,
  exposure folded into intensity; `look at` input (target's first
  camera, card, mesh bounds centre, point, then light; up = Y). Old
  light params missing = old behaviour.
  0.9.36 (MathWiz) M13 engine: `src/particles.rs` (`Particle`, `Shape`
  incl. `mesh_surface(Arc<Mesh>)` / `mask(&Image)` (Arcs compare by
  pointer: keep one per source), `Emitter`, `Modifier {effect, strength,
  start, end}`, `System`, trait `Params { at(f) -> System }` for
  animated params, `Cache::frame(&params, f)` re-sims from the first
  changed frame). Units: frames for start / end / life, seconds for
  velocity. No nodes yet.
  0.9.37 (3DWorker) M12 (b): light handles (`light_handles` /
  `drag_light`), `look_through: Option<Through {Camera(i) | Light(node)}>`,
  `light_camera()`, View ▸ Look through selected.
  0.9.38 (MathWiz) M12 (c) engine: `light2d::light2d(image, &[Light2d],
  normals, depth, mask)`; `Light2d::reach(x, y)` for handles; frame
  pixels y down, angles clockwise. Node + viewer handles: 3DWorker.
  0.9.39 (MathWiz) M13 render engine: `particles_render::render_sprites`
  (camera, depth) / `render_sprites_2d`, `instances(particles, meshes,
  orient, scale) -> Vec<Instance {mesh, world, color, id}>`.
  0.9.40 (3DWorker) M12 (d): shadows (`src/render3d/shadow.rs`; maps:
  spot 1024, cube 512/face, area 256/face, directional 2048), light
  `shadows` / `shadow_softness`, object casts / receives, `shadow` pass;
  `shade::light_shadowed` / `shade_shadowed`. **Use `scene::Model::new`
  (+ `.flags(params)`) and `scene::Card::new`** instead of struct
  literals. Cost: soft spot + 6×6 area shadowed 1.8 s vs 0.5 s (960×540).
  0.9.41 (MathWiz) M13 (A): `src/particle_nodes.rs` (`OPS`, `ParticleStream`,
  `ParticleState`, `eval(op, Context {own: params at any frame, ...})`,
  `world_mesh`); ports typed `STREAM = Kind::Scene` until the ops hook.
  0.9.42 (3DWorker) M12 (c): `light_2d` op (`Overlay::Light2d`, area uses
  `Geo::Box` [position, size, rotation]), overlay handles, properties
  shows the type's params. Note: `Normals::Pass` treats missing N.x / N.y
  planes as non-zero.
  0.9.43 (3DWorker) M12 (e): `Session::scene_objects`, MCP `scene_objects`,
  Python `Node.scene_objects(frame)`. **M12 done.**
  0.9.44 (MathWiz) M13 (B): hooks: `ops::Kind::Particles`, `ops::port`,
  `ops::growable` (scanline_render: 3 fixed then 'scene' extras),
  `Evaluator.particles` + `particle_node()` (params at any frame via a
  graph snapshot; particle ops frame-keyed + raw-param hash),
  `Scene.particles`, `src/render3d/particles.rs`, doc.rs keeps one spare
  input, auto-wire into a selected Scanline Render, viewport dots (20k
  cap). Limits: animated geometry emitters re-sim from scratch; no
  motion vectors for particles; sprite texture is one image.
  0.9.45 (3DWorker) M13 (C): `Session::particles` (paged, MAX_PARTICLES),
  MCP `particles`, Python `Node.particles`; README / help "Particles".
  Note: the GUI bridge builds a fresh Session per call, so particles via
  an attached GUI re-simulate each call (correct, slower).
  0.9.46 (MathWiz) M13 (e): `ParticleStream.parts`, viewport3d `Gadget`s
  (emitter shapes, attractor, vortex, bounce) picked / moved;
  `gizmo3d::param_for(op, mode)` (forces move only); overlay emitter
  handles when a Particles 2D is viewed. **M13 done.**
  - Next version: **0.9.49**. Both workers (mathwiz, worker3d) were
    idle at the end of session 10; nothing uncommitted. (CHANGELOG has one entry per version.)
- Mac: also ask whether anyone tried an Intel Mac. Mac-only bugs come
  first.
- **Next work, in order:** (1) the user's feedback on 0.9.27–0.9.31 (objects, lights, passes, multi-object G / R / S, fit, Project Settings, 3D setup, dialogs); (2) Mac feedback
  and hand checks of the 3D view (below) and the Camera Tracker's tripod
  / PnP on real footage; (3) M9 leftover: a true orthographic texture
  path in render3d (ortho views now use a 1000×-away long lens; the
  median pivot and multi-object transforms were done in 0.9.26); also
  from M10: the viewport's solid view is headlit, not lit by the scene's
  lights; (4) **M12, lights** (Area lights, lights movable in the 3D
  view with G / R / S and gizmos, a Light 2D node) and (5) **M13,
  particles** (emitters, forces, sprites or objects from files; emitters
  plug into Scanline Render as extra, growable inputs), both specified
  in the plan 2026-10-29; (6) other M8 follow-ups and older items under
  "Next steps". Keep `docs/help.md` in step with new features.
- **Session 10 so far:** M11 done (0.9.8 MathWiz nodal engine, 0.9.9
  3DWorker `solve::pnp`, 0.9.10 PnP in the solve + Frames list, 0.9.11
  Solve Auto / Free move / Tripod + MCP/Python), released v0.9.11; M12
  lights and M13 particles specified in the plan.
- **Verification worktrees:** `/tmp` (tmpfs, 16 GB) filled up once with
  build targets; verify in `../.verify/` (on /home) with its own
  `CARGO_TARGET_DIR=../.verify/target`. `packaging/linux/build-tarball.sh`
  ignores `CARGO_TARGET_DIR` (installs from `./target`), so run it without
  it inside the release worktree.
- **Not yet tried by hand** (session 9): all of the 3D view with a real
  mouse and trackpad (orbit / pan / zoom feel, numpad and pie, the
  navigation gizmo's buttons, box select, G / R / S typing, gizmo drags,
  Esc / right-click cancel), the Help window's scrolling and resizing,
  the Camera rig placement and backdrops (rename on the box, palette,
  corner resize).
- **Multi-agent setup:** the master commits; workers report (rules
  below). Last workers: **mathwiz** (wD:p3) and **3DWorker** (wD:p4,
  herdr name `worker3d`). Pane IDs change between sessions: confirm with
  the user who is master. **Master's commit procedure:** snapshot the
  reported files first, test the snapshot on HEAD in a clean worktree,
  stage exactly those blobs (`git hash-object -w` + `git update-index
  --add --cacheinfo`), commit, then build and test the committed HEAD
  alone before tagging (see "Working tips").

- **Phase 9 (started 2026-10-29): M9, the 3D viewport, with
  Blender-style controls,** on branch `wip-ns-3dwork`, from 0.9.0. Spec:
  `docs/phase-4-plan.md`, "Milestone 9 in detail". Workers: wD:p4
  (viewport camera, navigation, drawing, picking, textured cards) and
  mathwiz/wD:p3 (2D/3D toggle and Tab, G/R/S and gizmos). wD:p4 is
  called **3DWorker** by the user (herdr name `worker3d`).
  - **0.9.1 (mathwiz step 1):** 2D | 3D toggle at the left of the viewer
    toolbar; Tab switches only over the viewer and never while a text
    field has focus (handled after main.rs's text-field guard; egui moves
    focus on Tab at frame start, so the handler calls that off: tested).
    `viewer::scene_of(doc, node)` finds the scene (the node's own, else
    its connected scene input, e.g. Scanline Render's; a Viewer node
    stands for what feeds it). No scene: a message; with a scene, a
    placeholder in `Viewer::ui_3d` until `Viewport3d::ui` replaces it.
  - **0.9.2 (mathwiz step 2):** `src/gui/gizmo3d.rs`, maths only (under
    `#[allow(dead_code)]` in main.rs until step 3): a `View` trait
    (project, ray, pixels_per_unit, view_dir; 3DWorker's `Projector`
    will implement it) and `Gesture` (Move / Rotate / Scale from an
    object's Trs and world matrix). Axis move = the axis line's nearest
    point to the pointer ray; free = view plane; plane / Shift-exclude =
    ray-plane hit; rotate = accumulated turn about the screen center
    (past 180°), trackball 0.5°/pt; world turns map back to the Euler
    triple nearest the start; scale = distance ratio. X/Y/Z cycle global
    → local → free; typed values win over the pointer; Ctrl snaps 1 /
    15° / 0.1; `cancel()`, `Trs::reset`, `describe()` for the header.
    Parented params handled. 8 tests.
  - **0.9.3 (3DWorker steps a + c):** `src/gui/viewport3d.rs`: `ViewCam`
    (center, yaw, pitch, distance, ortho, auto_ortho; orbit / pan /
    zoom_at / view_axis / frame, pure and tested), `Projector`
    (perspective, ortho, `Projector::through(scene camera, format,
    rect)` for look-through; project / ray / segment / pixels_per_unit /
    view_dir), `Viewport3d::ui(ui, rect, resp, doc, frame, blocked)` plus
    `projector()` / `has_scene()` / `object_world(node)` for the gizmos.
    Scene objects carry their node ids (`Card.node`,
    `Scene.camera_nodes` / `point_nodes`, `Scene::tag`, run by the graph
    after eval). worker.rs: `SceneWorker` renders only the cards through
    the view (ortho as a 1000×-away long lens), at half / quarter
    resolution while moving. Not yet: picking / selection (step b),
    viewport-camera persistence, point error coloring.
  - **0.9.4 (3DWorker step b):** picking and shared selection.
    `Hit {Object(node), Point(node, index)}`; `pick(scene, projector,
    pos, is_locator)` (points / locators / camera apexes within 7 px,
    nearest then nearest in depth; else ray-quad against cards, nearest
    hit); `box_pick` → (object nodes, points); `Viewport3d.points`
    (selected (node, index)); `apply_hit` through `Doc::select_only` /
    `toggle_selected` / `clear_selection`; A = `select_everything`;
    `context_menu` (Place Card / locator via camera_tracker.rs, now
    `pub(crate)`; Frame selected / all). Selection is skipped while a
    gizmo or G/R/S is running.
  - **0.9.5 (mathwiz step 3 + wiring):** the 3D view is live.
    `Viewer.viewport3d`; `Viewer::ui_3d` runs G/R/S input first (so a
    gesture takes its keys before the viewport's), then
    `viewport3d.ui(.., blocked = is_transforming())`, then the gesture
    header and notices. `gizmo3d::Transforming` runs a `Gesture` on the
    selected node through `Doc::edit_param` (a key at the frame when
    animated); Esc / right-click restore the param exactly as stored (no
    key, no undo step); main.rs counts a running G/R/S as a gesture and
    skips its own shortcuts meanwhile. `impl View for Projector`; the
    world matrix comes from `viewport3d.object_world(node)`. Refusals
    (no selection, missing param, linked param) show 4 s at the
    viewport's bottom-left. `Graph.view` saves the viewport camera.
    0.9.5 also holds mathwiz's step 4 (it was in the tree when the
    snapshot was taken): toolbar Select / Move / Rotate / Scale / All and
    Global / Local; `gizmo3d::Layout` (pure: arrows left out when they
    point at the viewer, plane squares left out edge-on, rings with the
    far half faint, `GIZMO_SIZE` 90 pt at any depth); hit-testing small
    handles first, then arrows, rings, view ring, trackball; pressing a
    handle starts a `Transforming` with that constraint (same code as
    G/R/S); linked params gray with the reason on hover.
  - **0.9.6 (3DWorker):** view pie (`PIE`, 8 slots clockwise from the
    right, `pie_slot` with a 20 px dead zone, `pie_ui` / `pie_pick`) and
    point error colors (`error_color`); `sfm::Solution.point_rms`,
    `camtrack::Solution.point_rms` (`#[serde(default)]`),
    `Scene.point_errors` (negative = unknown). Also mathwiz's small
    `with_constraint` cleanup and the README "3D view" section.
  - **0.9.11 (MathWiz): M11 (e), Camera Tracker modes.** `ops.rs`
    camera_tracker `solve_mode` (SOLVE_MODES auto / free move / tripod)
    and `distance`; `camtrack::Mode`, `Solution.tripod` (serde default);
    Auto falls back to `nodal::solve` on `NoParallax` when detected nodal;
    tripod export has constant translate, set_scale resizes the sphere.
    `session::track_camera(id, mode, distance, ..)`; MCP/Python `mode`,
    `distance`; reports `mode`, `pnp_frames`. Test: a rendered pan.
  - **0.9.10 (3DWorker): M11 (b), PnP in the solve.** `sfm::Solution.pnp`
    (BTreeSet of frames), `sfm::NoParallax` typed error; growth from any
    solved neighbour, PnP fallback when the refined guess fails, PnP
    placement of stuck frames (gaps, restart after a cut).
    `camtrack::Solution.pnp_frames` (serde default); panel Frames list.
    Note: on clean synthetic jumps the refined neighbour guess already
    recovers, so PnP fires mainly on bad starts / heavy mistracks.
  - **0.9.9 (3DWorker): M11 (a), `solve::pnp`.** `pnp(points, pixels, k,
    max_error)` = P3P (Grunert quartic) in RANSAC + EPnP candidate/refit +
    cameras-only LM; also pub `p3p`, `epnp`, `rigid` (Kabsch). None under
    4 points/inliers.
  - **0.9.8 (MathWiz): M11 (c)+(d), tripod solve engine.** `src/nodal.rs`:
    `nodal::detect` (homographies over up to 8 frame pairs, nodal when
    median transfer error <= 2*max_error and `K⁻¹HK` within 0.02 of a
    rotation) and `nodal::solve` (chained rotations, rotation-only LM with
    Schur, optional focal, points at `distance`). Returns `sfm::Solution`
    (built in one place). Not wired into the tracker yet.
  - **0.9.7 (3DWorker): Help window.** `src/gui/help.rs`: `HELP =
    include_str!("../../docs/help.md")`, `Help {open, cache}` with
    `shortcuts(ctx)` (F1 opens, Esc closes and consumes the key; called
    first in `App::shortcuts`) and `ui(ctx)` (`egui::Window` +
    `ScrollArea` + egui_commonmark `CommonMarkViewer`). New dependency
    `egui_commonmark` 0.26 (MIT OR Apache-2.0, egui 0.37; optional, in
    the `gui` feature, default features off + `pulldown_cmark`);
    dev-dependency `pulldown-cmark` 0.13 for the test. **Edit
    `docs/help.md` to change the help**; tests check its sections, lists
    and tables parse, and that F1 opens / Esc closes the window. Keep it
    in step with new features.
  - **Process note:** twice a worker kept editing after reporting, and a
    commit took newer files than were tested (0.9.4 didn't build alone;
    fixed by `f61ec6e`). The master now snapshots reported files first
    and commits exactly the snapshot (see "Working tips"); workers don't
    touch reported files until the master says "committed".
- **Next work, in order:** (1) anything from the Mac test; (2) the
  hand checks listed below for session 8's GUI work; (3) **M9, the 3D
  viewport** (fully specified in `docs/phase-4-plan.md`, "Milestone 9 in
  detail": 2D/3D toggle, Tab only while the pointer is over the Viewer,
  orbit / pan / dolly, look-through, ortho views, picking with shared
  selection, move / rotate / scale gizmos, point cloud and camera path);
  (4) **M10, 3D objects and render passes (AOVs)** ("Milestone 10 in
  detail": primitives, OBJ / glTF import, materials and lights, passes as
  image planes, Shuffle node, multi-layer EXR); (5) M8 follow-ups and
  the older items under "Next steps".
- **Multi-agent setup:** if the user runs several sessions again, the
  master/worker rules below apply. Confirm with the user which session
  is master (herdr pane IDs and names change between sessions; last
  time the master was `wD:p1`, workers `wD:p3` (renamed `mathwiz`) and
  `wD:p4`).

## Where things stand

- Repo: `github.com/nsoler/arclight` (private). Local:
  `~/Projects/arclight-compound/arclight/`. The planning docs (C++/Qt
  "Linux port" plans that session 4 implemented in Rust) are in
  `~/Projects/arclight-compound/Archlight-Development-Documentationm/`.
- **Branches:** since 2026-10-30 (phase 12) work happens on
  **`wip-ns-gpu`** (phases 9–11 used `wip-ns-3dwork`, `wip-ns-splats`,
  `wip-ns-engine`, all frozen; the text below names the phase-9 one).
  Before that, from 2026-10-29 (phase 9), on
  **`wip-ns-3dwork`** (made from `main` at v0.8.4, tracks
  `origin/wip-ns-3dwork`); `wip-ns-effects` is the finished phase-8
  branch and stays at v0.8.4 (`aa04861`), not updated any more. **Standing instruction from the user:** when a
  milestone is finished (tests and clippy pass, README/HANDOFF updated),
  commit it, push `wip-ns-3dwork`, and fast-forward `main` to it and push
  `main`, without asking first. Then carry on with the next step. Keep
  `main`, `wip-ns-3dwork` and both remote branches at the same commit.
- **Several agent sessions (herdr), one master (2026-10-29, user
  instruction; the master is called the **Supervisor** since 2026-10-30,
  herdr name `supervisor`):** the user runs several Claude sessions in herdr on this
  one checkout. One of them is the **master** (herdr agent name `supervisor`, formerly `master`;
  see `herdr agent list`). Only the master commits, pushes, fast-forwards
  `main`, bumps the version, writes CHANGELOG version headers, tags
  releases and keeps this file's status current. **Every other session
  is a worker:** don't commit, push, tag, bump the version, or
  `git add`/stash/reset/checkout anything. Before starting, tell the
  master what you'll work on and which files you expect to touch (two
  workers mustn't edit the same file at once). When a milestone is done
  (`cargo test --release` and `cargo clippy --release --all-targets`
  clean), report it: `herdr agent prompt supervisor "<report>"` with a
  one-line title, the exact paths changed or added, a CHANGELOG bullet and
  any README/HANDOFF notes. The master checks and commits only those
  paths, and replies when it's in. (A `git add -A` by one session once
  swept another's unfinished files into its commit; this prevents that.)
- **Versioning (standing instruction, 2026-10-28; renumbered
  2026-10-29):** Arclight is alpha, `0.8.x` (the user renumbered 0.0.36
  to **0.8.1**: their earlier numbering was a mistake; releases before
  that are v0.0.x). Scheme: `0.<phase>.<increment>`: the middle number
  is the phase the user says the project is in (8 now); the last counts
  shipped changes within it. Every feature or change that ships bumps
  the last number; when the user declares a new phase, the middle number
  goes up and the last restarts (the user set **0.9.0** for phase 9, the
  3D viewport, so its changes go 0.9.1, 0.9.2, … 0.9.nn).
  The bump goes in `Cargo.toml`, `python/Cargo.toml` and
  `plugins/example/Cargo.toml` together, with an entry in `CHANGELOG.md`,
  in the same commit as the change. Current: **0.9.48**.
- ~27,400 lines of Rust + Python. Tests: 136 library + 10 CLI + 77 GUI
  (`cargo test --release`) and 10 Python (see "Working tips"), all
  passing. Some tests need ffmpeg (with libx265) and skip without it.
- **Release build (end of session 9):** `cargo build --release
  --workspace` and `python/build.sh` were run at 0.9.7, so
  `target/release/arclight-gui` (GUI), `target/release/arclight` (CLI /
  MCP server), `target/release/libarclight_plugin_example.so` and
  `python/arclt/_arclt.abi3.so` are current. Run the GUI with
  `./target/release/arclight-gui [project.json]`, or from the Omarchy
  launcher (Super+Space, "Arclight"): the repo's `install.sh` entry runs
  this checkout's release binary, so rebuilding is enough (a release
  tarball's `install.sh` writes the same launcher entry and replaces it).
  Clippy is clean apart from one old style lint (`Graph::from_str` name).
- **To check by hand** (session 7 work exercised only by tests,
  screenshots and headless renders): Keyer Ctrl+click/Ctrl+drag screen
  pick, shrink/grow, softness on real footage; right-click node browser;
  dragging an output onto a wire (Merge, dropped node in A); view
  override (double-click a node, click the Viewer to go back); the
  close-with-unsaved-changes dialog's Save / Don't Save buttons; Corner
  Pin From/To = image corners buttons; motion blur on real animation;
  10-bit footage and DPX/TIFF/EXR sequences from real productions.
  **Session 8 GUI work not yet tried by hand:** Camera Tracker point
  selection with the mouse (click / ⇧-click / box) and its Place Card,
  Place locator, Set origin, Ground plane and Set scale buttons; Track &
  Solve and Export on real footage; dragging wires between 3D nodes and
  the blue scene wires; the viewer's plane buttons (open an EXR with a Z
  channel); Lens Distort on a real plate; formula completions popup;
  everything on macOS (first run of v0.0.36 on the user's M4 Pro).
- **App icon:** the user's own design (teal, "ARCLIGHT") replaced the
  placeholder. `assets/arclight.svg` is framed as a square around the
  logo (viewBox `129.875 146.75 305 305`; the artwork itself unchanged)
  with "ARCLIGHT" converted to outlines in Source Sans 3 Regular (not
  installed here; outlined so it renders the same everywhere).
  `assets/arclight.png` is rendered from it at 256 px on transparency
  (`rsvg-convert -w 256 -h 256`) and built into the GUI (`app_icon()`);
  re-render it after editing the SVG, then rebuild and `./install.sh`.
  The lettering is #4e9290 (the logo's mid teal; was #033c3a, invisible on
  dark themes): contrast ≥ 3.6:1 on white and on dark backgrounds. At
  48 px the word is too small to read, so `assets/arclight-icon.svg` is
  the same logo without the lettering, same frame. `install.sh` installs
  it as the scalable icon (Omarchy's launcher always takes the SVG, at
  ~20–32 px: `AppLibrary.qml` indexes SVGs before PNGs) and at 16–64 px;
  128/256 px PNGs use the full logo. The GUI's window icon is still the
  full logo (`assets/arclight.png`).
- **macOS (session 7, part 22, user request):** `cargo check --target
  aarch64-apple-darwin` is clean (target installed here). Portability
  fixes: `media::tool` finds ffmpeg/ffprobe beside the executable, on
  PATH, or in /opt/homebrew/bin, /usr/local/bin, /opt/local/bin (Finder
  apps get a bare PATH); plugins use `DLL_SUFFIX` (.dylib on macOS); the
  starter project looks for its clip in the bundle's Resources and renders
  to ~/Movies/Arclight when the source checkout isn't there.
  `packaging/macos/build-dmg.sh` (macOS only) builds arm64 with
  MACOSX_DEPLOYMENT_TARGET 11.0, assembles Arclight.app (Info.plist id
  `com.github.nsoler.arclight`, .icns via iconutil from both SVGs, the
  example clip, the CLI), signs ad hoc, and makes a UDZO DMG with an
  Applications link. `.github/workflows/macos.yml` runs it on macos-14
  (workflow_dispatch or `v*` tags) and uploads the DMG. Run 40825211063
  (third try; fixes: no `…` right after a `$var` under macOS bash 3.2,
  no `$([[ … ]] && …)` in assignments under `set -e`) passed tests on the
  runner and produced `Arclight-0.1.0-arm64.dmg` (15.5 MB; checked: arm64
  Mach-O GUI + CLI, .icns, Info.plist, signature, example clip). The GUI
  hasn't been opened on a real Mac yet: that's the next check.
- **About dialog (0.0.21, worker wD:p3):** Help → About Arclight
  Compositor… (`App::about_dialog`, `egui::Modal`; logo texture made on
  first open from `app_icon()`; OK, Esc or a backdrop click close it).
  `build.rs` (pure Rust) sets `ARCLIGHT_BUILD_TIME` ("YYYY-MM-DD HH:MM
  UTC", Hinnant's `civil_from_days`; `SOURCE_DATE_EPOCH` wins). It reruns
  only on build.rs, `SOURCE_DATE_EPOCH`, `.git/HEAD` and the current branch
  ref (only paths that exist: in a `git worktree` `.git` is a file, and a
  missing path would rerun every build), so the stamp is the time of the
  first build after a commit. `rustc-env` is package-wide, so the lib sees
  it too.
  Layout (v0.0.35, mathwiz/wD:p3): each info row is one centered label
  (weak "Created by:" + value in one `LayoutJob`); the modal frame has a
  24 px inner margin on every side; the logo is drawn trimmed to its
  opaque pixels (UV rect computed once; `App.logo` is `(TextureHandle,
  Rect)`), since the PNG's transparent border made the top gap larger.
- **The product is ArclightVFX Compositor since 0.11.3 (beta)** (user,
  2026-10-30: another product is called Arclight Compositor). Display
  names, the Mac app (`ArclightVFX.app`), the DMG
  (`ArclightVFX-<ver>-universal.dmg`) and the tarball
  (`arclightvfx-<ver>-linux-x86_64.tar.gz`) use it; release titles are
  "ArclightVFX <ver> (beta)", still marked Latest. Commands, crates,
  `arclt`, settings folders, env vars, the MCP id, the bundle id and the
  repo keep `arclight`. The logo art still draws the word ARCLIGHT (a new
  logo is a design job).
- **While Actions is billing-blocked (since v0.9.57; still so at
  v0.10.26):** the Supervisor tags `v<ver>` from `main`, builds the Linux
  tarball in a clean worktree (`env -u CARGO_TARGET_DIR
  packaging/linux/build-tarball.sh`), the Linux wheel and sdist (a venv
  with `maturin` + `ziglang`: `maturin build --release --zig
  --compatibility manylinux2014 --target x86_64-unknown-linux-gnu -m
  python/Cargo.toml`, `maturin sdist -m python/Cargo.toml`), tests the
  wheel in a fresh venv run from `python/tests` (from `python/` the source
  package shadows the installed one) with `ARCLIGHT_PLUGIN_PATH` at the
  example plugin's build, and `gh release create … --latest` with notes
  ending "macOS DMG to follow". The user builds the DMG on a Mac
  (`packaging/macos/build-dmg.sh` at the tag) and uploads it; then the
  notes' Assets line is updated. Check the asset really arrived (`gh api
  repos/nsoler/arclight/releases/tags/v<ver>`) before editing the notes:
  one upload silently didn't. No macOS wheel until Actions works.
- **Release rule (user, 2026-10-29):** every release carries the Linux
  x86_64 tarball, the **macOS universal DMG** (Apple Silicon *and*
  Intel; `Arclight-<ver>-universal.dmg`, since v0.8.4; before that only
  `-arm64.dmg`), and the `arclt` wheels (Linux manylinux2014 x86_64,
  macOS universal2) plus sdist. Tag from `main` after the milestone is
  merged. The Intel half is cross-compiled on the macos-14 runner and
  checked with `lipo -archs`; it runs there only if the runner has
  Rosetta, and hasn't been run on a real Intel Mac yet.
- **Releases (session 7, user request):** GitHub releases tagged
  `v<version>`. **Since v0.9.27 (user request, session 10) each new
  release is marked Latest, not pre-release** (`gh release create …
  --latest`; the workflows' fallback create uses `--latest` too), so
  github.com/nsoler/arclight/releases/latest and the repo's Releases
  sidebar point at it; README links there. Older ones stay pre-release. First: `v0.0.19`. To make one at the
  current commit: tag it (`git tag -a v0.0.N -m "Arclight 0.0.N (alpha)"`)
  and push the tag; the macOS workflow builds the DMG and attaches it to
  the tag's release (`gh release upload`, creating the release if needed).
  Build the Linux tarball (`packaging/linux/build-tarball.sh`, from a
  clean `git worktree` so uncommitted work stays out) and
  `gh release create v0.0.N … dist/arclight-0.0.N-linux-x86_64.tar.gz`
  (or `gh release upload` if the workflow got there first). The tarball
  is built on Omarchy (links glibc only); its `install.sh` installs to
  `~/.local/bin` and writes the same `arclight.desktop` as the repo's
  `install.sh`, so on this machine the two replace each other. The GUI
  finds the starter clip at `../share/arclight/examples/media` beside its
  binary (and `../Resources/…` in the macOS bundle).
  **Python wheels (v0.0.30, worker wD:p3):** `.github/workflows/wheels.yml`
  (workflow_dispatch and `v*` tags) builds a manylinux2014 x86_64 wheel
  (maturin-action), a macos-14 arm64 wheel (Python tests only, no cargo
  test: macOS minutes) and an sdist, tests the Linux wheel on Python 3.10
  and 3.13 in fresh venvs with the example plugin, and for tags attaches
  `arclt-*.whl` and `arclt-*.tar.gz` to the release. Both workflows
  tolerate creating the same release at once (`create || true`, then
  re-check). Local build: a venv with maturin + ziglang, then from
  `python/`: `maturin build --release --zig --compatibility manylinux2014
  --target x86_64-unknown-linux-gnu --out <dir>`. `python/build.sh` is
  still the quick in-place build for the tests.
- **Session 7, part 23** (user request): viewer toolbar color picker
  (`Viewer::color_picker`, rightmost in `status`): swatch + R/G/B/A
  (`{:>6.3}`, monospace, tinted labels); `Viewer::picked` is set while
  hovering and re-read from each frame at the same pixel afterwards. The
  old bottom-left hover readout (x, y, RGBA to 4 places) is kept.
- **Session 7, part 24** (user report: new shapes land outside the
  picture; polygons move a point at a time): (1) `Doc::view_format` (the
  viewed image's frame, set by the App from each response) is what
  `add_node` fits new shapes to, else the project format: the starter
  clip is 960×540 in a 1920×1080 project, so shapes landed at its corner.
  (2) Overlay: `Part::Body` (inside a Polygon/Roto outline, `in_polygon`
  even-odd on `points_outline`) selects every vertex and drags them all;
  Shift there still box-selects. `Part::EdgeMove { i, t }` (press on an
  edge) selects its two ends and drags them, but only once the pointer
  moves > 3 screen px (`Drag::moved`); a click inserts a vertex on
  release (`Overlays::release`, which the tests' `gesture` now calls).
  Rectangle/Ellipse move from anywhere inside (`box_outline`).
- **Session 7, part 25** (user request): Tracker panel "Per-track boxes"
  (collapsed by default; `Properties::track_boxes`): a grid per track of
  pattern/search DragValues (shared size shown with "·" until the track
  has its own; ranges keep search ≥ pattern + 2, as the viewer does),
  offset x/y, and ↺ (removes the track's `_pattern`/`_search`/`_offset`).
  Off tracks are greyed.
- **Session 7, part 26** (user request: nodes look huge on opening): the
  node editor's fit zooms to at most `FIT_MAX_ZOOM` = 0.7 (was 1.25), so
  the starter graph's nodes are ~105×32 px instead of ~190×58; node
  titles, subtitles and port labels have minimum font sizes (10 / 8.5 /
  8 px) so they stay readable when zoomed out.
- **Session 7, part 27** (user request), **v0.0.15**: smoother
  downscaling. `ops::sample_warped(src, x, y, inverse)` (Transform,
  `warp` for the Tracker output, Corner Pin): the inverse map's finite
  differences give each output pixel's source footprint; where it's over
  one pixel a side (minus 1e-3 slack, so a 90° turn stays bilinear), the
  mean of a grid of bilinear taps over it, up to `MAX_TAPS` = 16 a side;
  else plain bilinear (unchanged output). A 1-px checkerboard shrunk 4×
  went from values 0.03–0.97 to 0.5 ± 0.08. Also set the version to
  0.0.15 (was 0.1.0), shown in the Help menu; `CHANGELOG.md` added.
- **Session 8, part 1** (user request: format gaps), **v0.0.16**: EXR data
  windows. `read_exr` sets `format` from the display window and `x0/y0`
  from the layer position minus the display origin; `write_exr` writes a
  `Layer` of the window's size at `layer_position`, display window = the
  frame (empty window: one transparent pixel at 0,0). Checked with
  oiiotool-made offset / past-the-frame data windows and `exrheader` on
  ours.
- **Session 8, part 2**, **v0.0.17**: DPX alpha and float. `dpx::Options
  { encoding, alpha }` (`Image::write_as`); `dpx_alpha` Bool on
  media_out/write. Writer builds each row's component stream (straight
  color + linear alpha with alpha; premultiplied color without) and packs
  it: 10-bit 3 per word (method A, lines word-aligned), 16-bit halves
  (lines padded), 32-bit float (`Encoding::Float32`, transfer 2,
  colorimetric 6). Reader: 32/64-bit float via `unpack_float`, values
  linear unless transfer is a video code (4–10). Interop: integer RGBA
  read correctly by ImageMagick, OIIO, ffmpeg; float matches OIIO's
  layout and OIIO reads it exactly, but ImageMagick and ffmpeg can't read
  float DPX at all (not even their own / OIIO's).
- **Session 8, part 3**, **v0.0.18**: video color tags. `VideoWriter`'s
  filter chain: pad → `scale=out_color_matrix=bt709:out_range=tv` →
  `format=<VideoCodec::pix_fmt>` → `setparams=color_primaries=bt709:
  color_trc=iec61966-2-1:colorspace=bt709:range=tv` (ffmpeg 7 takes tags
  from frames; the `-color_*` output options alone left trc/primaries
  unknown). Before, swscale's BT.601 default made BT.709 decoders show
  200,100,50 as ~209,105,45. Arclight's own decode follows the tags (reads
  back 200,100,50). `video_codecs_keep_their_depth` checks the tags.
  Not done: RLE and 1-bit DPX (rare; ffmpeg, ImageMagick and OIIO can't
  make them, so there's nothing to test against).
- **Session 8, part 4** (user request), **v0.0.19**: live links.
  `src/link.rs`: a param value `{"link": {"node", "param"[, "at":
  "reference"]}}` or `{"link": {"node", "motion": translate|rotate|scale,
  "stabilize", "pivot"}}`; `link::resolve(lookup, v, t)` (chains ≤ 8,
  loops → Null), `missing`, `rename`, `describe`. Engine:
  `ops::resolve_params_linked(op, params, frame, links)` follows links in
  `resolve_at` (so motion blur samples follow them too); `graph.rs`'s
  `linked_params` does it for `run` and `key` (cache keys see resolved
  values) and errors on a missing source. Tracker: `positions_at` /
  `warps_at` / `solved_at` (fractional frames) and `motion_at` (shared
  with the baked export); `track::export(.., linked)` makes motion links
  (Transform, pivot = image center like the copy) or track-param links
  (Corner Pin: To = track, From = track at the reference frame).
  `Session::export_tracker`, MCP `export_tracker` (`linked`, default
  true), Python `to_transform` / `to_corner_pin(linked=True)`;
  `validate_param` accepts links; renames update them (Session and Doc).
  GUI: `Doc::param_at` resolves links, `edit_param` ignores linked params;
  panel `linked_widget` (value, "← source", Unlink); Tracker panel
  "Linked" checkbox (`Properties::copy_exports`). Tests: link chains /
  reference / loop / rename; linked Corner Pin = copy pixel for pixel;
  linked Transform motion blur = blurred copy; MCP live follow of a
  re-track, rename, delete error; Doc.
- **Session 8, part 5** (user request), **v0.0.20**: maths on links.
  Link objects take optional `multiply` / `add` (number or per-component
  list); `link::adjust` applies `v × m + a` after resolving (numbers and
  number lists; anything else passes through); `with_adjust` sets or
  (Null) removes them; `describe` appends "× m + a". Panel
  `linked_widget` has a second row of × and + DragValues per component
  (×1 / +0 are stored as absent). Tests: `links_multiply_and_add` (incl.
  half of a solved 90° rotation), MCP `scripts_set_links_with_offsets`. No Developer ID signing / notarization (needs a paid Apple
  account); ffmpeg not bundled (GPL builds; Homebrew instead).
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
  Session 7 (all user requests; see "Progress log", parts 1–21): port
  styling and drag-output-onto-wire → Merge; close-with-unsaved warning;
  narrow viewer toolbar; Transform auto pivot (`center`); Keyer node
  (color difference, despill, clean plate, viewer color pick, shrink/grow,
  softness); viewer override instead of rewiring, unplugged inputs read
  as empty; motion blur on shapes, Transform, Corner Pin and Tracker
  output; Corner Pin From/To = image corners; right-click opens the node
  browser; app launcher entry (`install.sh` / `uninstall.sh`, icon,
  app id `arclight`); viewer crash past the GPU texture limit fixed; big
  and 16-bit stills; EXR/TIFF/Targa/WebP sequences checked, missing frames
  hold, TIFF alpha tagged; DPX read/write (log, 10/16-bit video); 10-bit
  video decode and encode (HEVC 10-bit, ProRes 422 HQ, ProRes 4444).
  CorridorKey (AI keyer) was researched and deferred by the user (notes
  in part 5).
- **Not done:** small UX debts, session-4 follow-ups, Phase 4 (3D). See
  "Next steps".
- Development is on Linux (Omarchy/Hyprland): use `sed -i` (not
  `sed -i ''`); ffmpeg/ffprobe are in `/usr/bin`. Hyprland tiles the window
  narrower than its 1600 px default, so screenshots come out ~920–1030 px
  wide (or the full monitor when it's the only window).

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
report back after a few milestones. Make a release (see "Releases") when
the user asks, usually after a visible feature lands.

1. **Mac feedback** on v0.0.36 (ask the user), then the session 8 hand
   checks above.
2. **Phase 4 M9: 3D viewport — done in 0.9.1–0.9.6** (leftovers: true
   ortho textures, multi-object transforms; see "Start here"). As specified in `docs/phase-4-plan.md`
   (order inside M9: (a) viewport camera, navigation, grid, frusta,
   points, look-through; (b) picking and shared selection; (c) textured
   cards via the CPU renderer with a resolution fallback; (d) gizmos with
   keys and undo; (e) camera path, error coloring, view cube, ortho).
3. **Phase 4 M10: 3D objects and render passes (AOVs)**, as specified
   there (order: meshes + primitives; P / N / uv / id / mattes + Shuffle;
   OBJ import; materials, lights, lighting passes; motion, zmerge, glTF).
4. **M11: PnP and the tripod solve — done in 0.9.8–0.9.11** (covers the
   old follow-ups "solving frames between solved ones" and "nodal
   solve"). **Other M8 follow-ups:** a live link from a Camera to the
   solve (Export bakes keys today); point picking over a Scanline Render
   of the solved scene; a lens-distortion solve; a mask input to ignore
   moving objects; motion blur in Scanline Render; mipmaps for minified
   cards.
5. **Older follow-ups** (small to medium):
   - Python plugins (custom ops written in Python): embed Python in the
     host via the `python/` crate, or run them out of process.
   - Plugin process isolation: a crashing native plugin kills the app.
   - Formats: RLE / 1-bit DPX (rare; no test files: ask the user for a
     sample before writing a reader).
   - Not planned: OpenFX hosting (large; its own project), licensing (the
     docs wanted another product's scheme copied; declined), `import_nuke`
     (originality rule).

## Node graph selection and backdrops (v0.8.3, mathwiz/wD:p3)

- `Doc` keeps `selected` (the primary node: Properties, viewer handles,
  MCP `select_node`) plus a private selection list and `selection_for`;
  code that only sets `selected` selects just that node, so viewer,
  overlay and MCP work unchanged. `selected_backdrop` for the panel.
- `graph.rs`: `Backdrop {id ("backdrop1"…), title, pos, size, color
  (sRGB 0..1), font_size}`, `Graph.backdrops` (serde default, skipped
  when empty: old files load, unchanged files save the same).
- Gestures: click / Shift or Cmd-click toggle / box select on empty
  space (Shift adds) / Cmd+A (over the graph; elsewhere it still selects
  a shape's points in the viewer) / Esc (after main.rs's text-field
  guard, and not while the node browser is open). Dragging moves the
  whole selection (one undo step). **Pan moved:** left-drag on empty
  space used to pan; now middle-drag or Alt-drag pans.
- Backdrops: Cmd+G / right-click Group; title-bar drag moves the nodes
  (and backdrops) *fully inside* when the drag starts; corner resize;
  double-click renames in place; swatch palette; Shift+Cmd+G or Delete
  removes the box and keeps the nodes. Fit (F) and the minimap include
  backdrops. Shake-to-extract and drop-into-wire act only when a single
  node is dragged.
- Not done: MCP/session backdrop tools. Not checked by hand: the inline
  rename, the palette popup and the corner cursors (tested headlessly).

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
  its From corners set (session 7 added "From/To = image corners").
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
- **Session 6, part 10** (user bug report): wires couldn't be connected
  to a Merge's B input (or reliably to any closely spaced input) at
  normal zoom; it showed up after inserting a Merge into a wire. Cause:
  a dropped wire took the *first* input within 13.75 points, and a
  Merge's ports are 11.5 apart at 1:1 (so B's drop landed on A), and the
  ports' 16.5-point grab squares overlapped. Now the nearest port wins
  and grab areas are capped at half the gap (`NodeEditor::port_reach`).
  `node_editor.rs` tests gained a `Harness` that drives the editor
  headlessly with egui pointer events (`Context::run_ui`), at a chosen
  zoom: use it for any node-graph gesture.
- **Session 7, part 1** (user request): ports restyled (`port_dot` +
  `PortKind`: inputs yellow squares, outputs light green circles; optional
  inputs hollow). Dragging an output onto a wire (`NodeEditor::input_at`
  wins first, then `wire_at`, lit while dragging) calls
  `Doc::merge_into_wire`: a new Merge halfway between the wire's ends, the
  dragged node into A (over), the wire's source into B (swapped at the
  user's request in part 8), the Merge into the old
  port; refused on a cycle. Dragging a node *body* onto a wire still
  splices it (`insert_on_wire`).
- **Session 7, part 2**: closing the window with unsaved changes asks
  Save / Don't Save / Cancel (`App::handle_close`: `CancelClose`, rfd
  dialog, then `Close` with `closing` set so the next request passes).
  Checked on Hyprland by dirtying a window over the bridge and closing it
  with `hyprctl dispatch "hl.dsp.window.close({ window = 'pid:<pid>' })"`.
- **Session 7, part 3**: the viewer toolbar's status text (Viewing …,
  size, frame, ms) moves to a second row when it doesn't fit beside the
  controls (`Viewer::status`, width remembered in `status_w`). Checked by
  screenshot with a temporarily widened properties panel.
- **Session 7, part 4** (user report: a Transform after a shape had its
  gizmo far from the shape): Transform has a `center` param, default
  **null** = `ops::transform_pivot` (middle of the input's data window;
  the frame when it has none). Centers count like shapes do (pixel i spans
  i..i+1); the op subtracts 0.5. A null default is new: `validate_param`
  accepts null for such params, the panel shows the auto value (from
  `Properties::layer`, the App's `probe`) marked "auto", with an **Auto**
  button once set, and keying an auto value fixes it first. The overlay's
  `Geo::Transform::auto` takes the probed layer's center. Tracker export
  writes an explicit `center` (the frame's). Behaviour change: old
  projects with a rotated/scaled Transform over a *small-window* input
  (only possible since Phase 0.6) now pivot on that layer.
- **Session 7, part 5** (user request): `keyer` op, new `Category::Keyer`
  (plugin category "keyer"). Color difference: alpha = 1 − (c[p] −
  others(c)) / (s[p] − others(s)), p the screen's dominant channel,
  `others` weighs the larger/smaller of the other two by `balance`; clip
  black/white; despill limits c[p] to `others(c)`. `screen` defaults to
  null → `estimate_screen` (per-channel median of pixels where green, or
  blue if more, leads by 25%). Optional clean plate gives the screen per
  pixel. Panel: null Color params show "auto" + Set, and an Auto button
  once set.
- **Session 7, part 6**: viewer color picker. `Overlay::Pick(param)`
  (Keyer: `screen`) → `Geo::Pick`, `Part::Pick`: Ctrl+click samples the
  node's *input* at the pixel, Ctrl+drag averages the box (straight color,
  alpha 1, rounded to 1e-4), via `edit_param` (auto-keys). The worker's
  probe now returns the input `Arc<Image>` (small cache of 8) instead of
  its window, for Transform and Pick overlays alike;
  `Overlays::set_input`, `Display::input`.
- **Session 7, part 7** (user bug: adding a Keyer, which joined the
  Viewer's wire with a Merge, blanked the viewer): (1) `Evaluator::run`
  feeds an unconnected *required* input an empty image in the project
  format; only `Category::Output` ops (viewer, write, media_out) still
  error. (2) Viewing is an override, not a rewire: `Doc::view_override`
  (view state, not saved/undone; follows renames, lapses when the node is
  deleted), `Doc::overridden`, `view_target`/`viewed` prefer it.
  `view_node` on a Viewer clears it, on another node sets it (or, with no
  Viewer yet, `route_to_viewer` makes and wires one, which file drops
  still use). Clicking the Viewer node clears it too. Status line: "(click
  viewer1 to go back)"; the node graph draws an eye on the overriding
  node. MCP `view_node` follows the same rule.
- **Session 7, part 8**: `merge_into_wire` now puts the dropped node in A
  (over) and the wire's source in B, so a keyed foreground lands over its
  background.
- **Session 7, part 9** (user request): shape motion blur. Shapes have
  `motion_blur` (samples, 0/1 off), `shutter` (frames, 0.5) and
  `shutter_offset` (centered/start/end), all fixed (not animatable).
  `resolve_params` → `add_motion_samples`: when any param is animated, the
  shape's params at each sample time (`resolve_at`, f64 frames) go into a
  hidden `_motion` array, so the cache key sees them. `shape_op` builds an
  outline per sample (`shape_outline`), reaches over their union
  (`outline_bounds`), and averages coverage. Viewer handles show the
  shape at the frame itself.
- **Session 7, part 10**: Transform and Corner Pin motion blur, same three
  params (`MOTION_BLUR_OPS`). `ops::eval` sees `_motion` on a warp and
  runs `eval_op` once per sample, then `average`s the outputs
  (premultiplied, over the union of windows), before mask/mix.
- **Session 7, part 11**: Tracker output motion blur, the same way. Each
  `_motion` sample also gets `ref<i>`/`refwarp<i>` (`add_reference_tracks`);
  no samples while Output is "input". A test checks it matches a blurred
  exported Transform.
- **Session 7, part 12**: Corner Pin panel buttons "From = image corners"
  and "To = image corners" (`corner_pin_extras`, `corner_pin_corners`:
  (0,0), (w,0), (w,h), (0,h), clockwise from the top left), via
  `edit_param` (keys if animated). The input's frame comes from the probe,
  now also requested for `Overlay::CornerPin`; `Properties::layer` carries
  the input's window and format.
- **Session 7, part 13** (user request): right-click on the node graph
  opens the Shift+E node browser at the pointer (the old category context
  menu, `add_menu` / `menu_at`, is gone); over a wire the browser shows
  "Insert between …" and its node goes into the wire.
- **Session 7, part 14** (user request): Keyer `shrink_grow` (−50…50 px)
  and `softness` (0…50). With either set (and Output not "despill
  only"), `keyer` builds the matte (alpha × input alpha) over the data
  grown by the reach (within the frame), runs `morph` (1-px steps
  alternating 4/8-neighbor max or min = octagonal, fraction blended,
  edges repeat), then 3 `box_pass_h/v` of round(softness/3) px, and
  shades color from the filtered matte. Otherwise the old pixel path.
  The matte output is now alpha × input alpha (0 outside the data).
- **Session 7, part 15** (user request): app launcher entry. `install.sh`
  / `uninstall.sh` at the repo root (per-user: `~/.local/share/
  applications/arclight.desktop`, icons in `~/.local/share/icons/hicolor`
  scalable + 48–256 PNGs via rsvg-convert). The entry's Exec is this
  checkout's release binary. Placeholder icon `assets/arclight.svg` (+
  `arclight.png`, built into the GUI as the window icon via
  `app_icon()`). The window's app id is now `arclight` (it was the
  title-derived "Arclight"): Hyprland window rules need the new class.
  Installed on this machine; checked by launching with `gtk-launch`.
- **Session 7, part 16** (from a user question about read resolutions):
  (1) crash fix: images wider or taller than the GPU's max texture side
  (8192 on this machine) panicked in wgpu `create_texture`; the viewer now
  averages `step`×`step` blocks into each texel (`to_color_image`'s
  `step`, from `max_texture_side`). (2) `Image::read_ldr` raises the image
  crate's 512 MB decode cap to 4 GB (a 9000² 16-bit RGBA PNG was
  refused). (3) 16-bit stills keep 16-bit precision (`to_rgba16`), float
  ones (`.hdr`) are read as linear; 8-bit as before. Video stays 8-bit
  (ffmpeg `-pix_fmt rgba`): a 16-bit/float decode path would be the next
  step for 10-bit footage.
- **Session 7, part 17** (user request: EXR/TIFF/Targa sequences, WebP):
  reading already worked for all (image crate default formats + `####`
  detection); verified with generated 5-frame sequences of EXR, 8/16-bit
  LZW/ZIP TIFF, RLE Targa, lossy/lossless WebP. Added: a missing frame in
  a sequence holds the nearest earlier one (`media::existing_frame_path`);
  TIFF is written by the `tiff` crate directly as 16-bit RGB +
  UnassociatedAlpha ExtraSamples, LZW (the image crate's RGBA TIFF had no
  ExtraSamples tag, so ImageMagick and others didn't see the alpha); JPEG
  writes RGB over black (RGBA failed); the file dialogs list
  `READ_EXTS` / `WRITE_EXTS` (adds tga, webp, hdr, bmp, mxf, mpg).
- **Session 7, part 18** (user request): DPX, own reader/writer in
  `src/dpx.rs` (SMPTE 268M, public spec): first element; 8/10/12/16-bit;
  packing 0 (bit stream), 1 (method A) and 2 (method B); BE/LE;
  descriptors 6/50/51/52/4; orientations. Transfer 1/3 → Cineon log
  (`log_to_linear`, 95/685/0.6), 2 → linear, else sRGB decode. Writes
  10-bit log RGB BE method A, header 2048. Checked against ffmpeg
  (8/10/12/16-bit, RGB/RGBA, little-endian, transfer 4) and ImageMagick
  (10-bit, and log via `-colorspace Log`): all read back to the source
  color; ffmpeg and ImageMagick read our DPX back to it too. Not handled:
  RLE-encoded DPX, 1-bit, 32/64-bit float, multi-element files (first
  element only).
- **Session 7, part 19** (user request): 10-bit video. `VideoInfo::bits`
  from ffprobe's `pix_fmt` (`pix_fmt_bits`: trailing digits, nv* = 8,
  48/64 = 16, f32 = 32) and `bits_per_raw_sample`. Deeper than 8 →
  `-pix_fmt rgba64le` and `rgba16_to_image` (65536-entry LUT); else the
  8-bit path as before. Checked: an FFV1 gbrp10 ramp keeps 1024 levels
  (8-bit gave 256); HEVC Main10, ProRes 422 HQ 10-bit and 4444 12-bit
  probe and decode; 4K ProRes 10-bit ~72 ms/frame vs 60 ms for 8-bit
  H.264. `probe_media` reports `bits`.
- **Session 7, part 20** (user request): 10-bit video encode. Media Out
  `codec` (`media::VIDEO_CODECS`; `VideoCodec::choose` resolves "auto"
  by container and refuses ProRes outside .mov/.mkv before rendering).
  HEVC 10-bit (libx265 yuv420p10le, hvc1 tag), ProRes 422 HQ, ProRes
  4444 (yuva444p10le, alpha_bits 16, straight color + alpha) are fed
  `rgba64le`; H.264 stays `rgba`. `render::tests::
  video_codecs_keep_their_depth` round-trips a 1024-step ramp through
  each. Also fixed the old `chunks_exact_mut(4)` clippy lint (now
  `as_chunks_mut`); only `Graph::from_str` remains.
- **Session 7, part 21** (user request): video-coded DPX output.
  `dpx::Encoding` (Log10 default, Video10, Video16; `dpx::ENCODINGS`
  choices) as a `dpx` param on media_out and write; `Image::write_as`
  (`write` = default log). Video: sRGB-coded full range, transfer and
  colorimetric 6 (Rec. 709), ref codes 0/max; 10-bit filled method A,
  16-bit packing 0 with lines padded to words. Checked: ImageMagick and
  ffmpeg read both back to the source color.
- **CorridorKey** (AI keyer, github.com/nikopueringer/CorridorKey; the
  OFX port gitcapoom/corridorkey_ofx is Windows/Resolve-only): the user
  deferred it. If revisited: run upstream's PyTorch engine (Linux OK; the
  RTX 5070 12 GB fits) in an out-of-process Python worker, feed it the
  Keyer's matte as the alpha hint. Licence is CC BY-NC-SA-like with a
  no-resale / commercial-software-needs-agreement clause: keep it out of
  the repo, user-installed, and ask the user whether Arclight is
  commercial first.

## Architecture in one page

Engine (`src/`, library crate `arclight`):

| File | Role |
|------|------|
| `ops.rs` | **Op registry** `OPS: &[OpInfo]` (name, label, `Category`, input `Port`s, `ParamSpec`s with defaults, `mask_base`, viewer `Overlay` kind, help) + every op's implementation. `eval(op, params, inputs, ctx)` dispatches and applies the generic mask/mix. `resolve_params` evaluates keyframes before an op runs (and, for `tracker`, adds the reference-frame positions); `is_time_varying` marks ops whose output depends on the frame by itself. `warp` resamples by an affine. |
| `graph.rs` | `Graph { nodes: Vec<NodeSpec>, project }`. `Evaluator` caches each node's result under a hash of (op, *resolved* params, input keys, frame if time-varying). `Evaluator::key` predicts a cache key without rendering. Errors are `NodeError { node, message }`. |
| `anim.rs` | Keyframe curves stored inline in params: `{"keys": [[f, v], [f, v, {in,out,broken}]], "interp": ...}`. Cubic Hermite with auto/user tangents. `set_key`, `edit_all` (structural edit of every key), `sample`, `key_frames`, ... Any numeric array param can be animated. |
| `image.rs`, `color.rs` | `Image { width, height, pixels: Vec<[f32;4]>, planes, scene, … }`: RGBA linear premultiplied, with a data window (bounding box / offset, Phase 0.6), extra named f32 `planes` (depth, …; `plane`, `carry_planes`, `plane_stats`) and an optional `scene: Option<Arc<Scene>>` (a scene image has no pixels). `Image::bytes()` sizes the frame cache. `sample_bilinear` puts pixel centers on integers (shapes, overlays and tracking use i + 0.5). |
| `shape.rs` | Coverage of polygons (anti-aliasing, feather, fill rules), rectangle/ellipse outlines, bezier splines (`spline_segments`, `spline`, `cubic_at`, `split_cubic`). |
| `track.rs` | 2D tracking: `Luma`, `Pattern`, `Warp`, `find`, `track` (a run), `input_frames`, `positions`, and the similarity `solve` / `Affine` helpers. |
| `media.rs` | Sequence patterns (`####`, `%04d`), range detection, ffmpeg decode (persistent per-file decoder, frame-accurate seek) and encode. |
| `render.rs` | Frame-range rendering of `media_out`; `Progress` (done/total/cancel) is shared with tracking. |
| `session.rs` | `Session`: headless graph + validated edits (`create_node`, `set_params`, `set_key`, `connect`, `port_index` for labels like "B"), whole-graph undo/redo, render/save/load. `validate_param` normalizes values against `ParamSpec`s; `op_schema` describes an op as JSON; `image_stats` gives size + mean/min/max. |
| `mcp.rs` | MCP server. `TOOLS` table (name, description, JSON Schema) and `call_tool(&mut Session, tool, args)`, the one tool dispatcher shared by MCP, Python and the bridge. `Server` routes to the local session or an attached GUI (`list/attach/detach_instance` are handled there). `serve_stdio` (line-delimited) and `serve_http` (hand-rolled HTTP/1.1, POST only). A `preview_png_base64` field in a result becomes an MCP image block. |
| `bridge.rs` | Running-GUI registry (`$XDG_RUNTIME_DIR/arclight/instances/<pid>.json`, stale entries pruned by a connect test) and Unix-socket server/client. Protocol: one JSON line `{"tool", "arguments"}` → `{"ok"}` / `{"error"}`. The GUI polls `Server::poll()` each frame; a request wakes it with `request_repaint`. |
| `plugin.rs` | Native plugin host for the C ABI in `include/arclight_plugin.h`. `init()` (once; called by CLI, GUI, Python) scans `config::plugin_dirs()` for `libarclight_plugin_*.so` / `*.arclight.so`, parses each library's JSON op descriptors into leaked `&'static OpInfo`s, and registers them. `ops::info`/`ops::all`/`eval_op`/`is_time_varying` fall back to it. |
| `config.rs` | XDG dirs (`config_dir`, `data_dir`, `cache_dir`, `runtime_dir`), `plugin_dirs()`, `Settings` (`settings.json`: `plugin_paths`, `frame_cache_mb`, `mcp_http_addr`). |
| `expr.rs`, `link.rs` | Live links and formulas: `{"link": {"node", "param"}}` or `{"link": {"expr": "…"}}` with multiply/add; `expr` is the lexer / Pratt parser / evaluator (`Clock` gives `time`, `fps`); `link::evaluate` resolves with a depth limit (loops are errors) and renames rewrite refs. |
| `geom.rs` | 3D maths (nalgebra): TRS placement (X-Y-Z degrees), pinhole `Camera` (project / unproject in frame pixels; right-handed, Y up, looking down −Z), Brown `Distortion` and its inverse. |
| `scene.rs`, `render3d.rs` | `Scene {cameras, cards, points}` carried on `Image.scene` along typed (blue) wires; `render3d::render` is Scanline Render (perspective-correct, depth-sorted, anti-aliased, depth plane). |
| `features.rs`, `solve.rs`, `bundle.rs`, `sfm.rs`, `camtrack.rs` | Camera tracker: Shi–Tomasi + pyramidal KLT with affine refinement; two-view essential matrix / triangulation; Levenberg–Marquardt bundle adjustment (Schur, Huber); incremental solve; the Camera Tracker node's run / `Solution` / orient / export / place-on-points. |
| `dpx.rs` | DPX read/write (8/10/12/16-bit, float, alpha). |

GUI (`src/gui/`, binary `arclight-gui`, eframe/egui 0.37):

| File | Role |
|------|------|
| `main.rs` | App shell: panel layout, menus, keyboard shortcuts (`shortcuts()`), file drop, starter graph, `ARCLIGHT_SCREENSHOT` hook. Commits undo once per gesture; an active tracking run counts as a gesture. |
| `doc.rs` | `Doc`: the graph plus every edit (`add_node`, `connect`, `set_param`, `edit_param` with auto-key, ...). **Undo** is whole-graph snapshots, committed once per finished gesture. Mutate the graph only through `Doc` methods that call `changed()` or `touched()`. |
| `node_editor.rs` | Canvas: nodes, ports (yellow squares in, green circles out; hollow = optional), wires, pan/zoom, the node browser (right-click or Shift+E), which places nodes via `place`. |
| `properties.rs` | Property panels generated from `ParamSpec`s; keyframe diamonds; media extras; render jobs (`RenderJob`); tracking (`tracker_extras`, `TrackJob`, `collect_tracks`). |
| `viewer.rs` | Shows the image; pan/zoom; channels; exposure; pixel readout. Hosts the overlays. |
| `overlay.rs` | Handles for the selected node's `Overlay` kind: `Xf` (image↔screen), `geo` (resolve at the frame), `hit`, `press`, `apply` (drag → `edit_param`), `draw`. Add new handle kinds here. Positions use the viewed image's size (no bbox yet). |
| `transport.rs`, `curve_editor.rs`, `worker.rs` | Player + timeline; keyframe curves with tangent handles; background eval thread + frame LRU (`frame_cache_mb`, default 1536). |
| `camera_tracker.rs` | Camera Tracker panel: `CamJob` (Track & Solve on a thread, progress, Stop), Export, and the `PointPicker` used by `overlay.rs` for point selection and placement. |
| `script_jobs.rs` | Background renders requested over the bridge. |

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

Plan: `docs/phase-4-plan.md` (2026-10-29, worker wD:p4). D1–D4 agreed
as proposed; D5–D6 still open. **M1 done (v0.0.23): extra image planes.**
`Image.planes: Vec<Plane>` (name + f32 per window pixel), `plane`,
`plane_mut`, `plane_at`/`plane_value` (frame coords, 0 outside),
`carry_planes(src, source_fn)` (nearest), `keep_planes`, `plane_stats`;
`flat()` keeps them. ops.rs: `planes_source(op)` lists ops that keep
their input's planes (applied in `eval`), `merge_planes`; warps, crop and
resize call `carry_planes`; `mask_mix` keeps the result's (else the
base's); motion blur keeps the middle sample's. EXR: `read_exr_planes`
(reads the flat layer only when the header has non-RGBA channels),
`write_exr_planes` (AnyChannels when planes exist). Viewer:
`Channel::Plane`, `Viewer::plane`, `plane_image`. `image_stats` (MCP)
lists planes. Not checked by hand: the viewer's plane buttons (no 3D
renderer yet; an EXR with Z shows them). Tests:
`image::exr_keeps_extra_planes`, `ops::planes_follow_their_pixels`.
**M2 done (v0.0.24): camera maths + Lens Distort.** `src/geom.rs`:
right-handed, Y up, camera looks down -Z; `rotation(r)`/`angles(R)` (X
then Y then Z, nalgebra `from_euler_angles`), `trs`, `transform_point`,
`fov`/`focal_for_fov`, `Camera {focal, film_back, world, near, far}` with
`view`/`focal_pixels`/`project` (-> pixel, depth)/`project_camera`/
`unproject` (frame pixels, pixel i spans i..i+1, film width fits frame
width), `Distortion {k1, k2, center, scale = w/2}` `distort`/`undistort`
(Newton, None past the fold). ops.rs: `lens_distort` op, `bent_window`
(samples the edges, since a lens map's extremes aren't at the corners).
Tests: geom rotations/projection/distortion round trips,
`ops::lens_distort_moves_points_radially_and_undoes_itself`.
**M3 done (v0.0.27): scene data, Camera / Card / Scene, typed wires.**
Deviation from the plan text (same result for users): no `Output` enum.
A scene rides on `Image` as `Image.scene: Option<Arc<Scene>>` with no
pixels, so it uses the existing cache and worker; `Image::bytes()`
(pixels + planes + card textures/points + struct size) sizes the frame
cache, so a scene-only image never counts as 0. `flat()` keeps the scene;
MCP/Python stats flatten a scene image to a transparent frame and add
`"scene": "1 camera, 1 card"`. Wires are typed by `Port.kind`
(`Kind::Image`/`Scene`), `ops::output_kind`, `ops::wire_error`; checked in
graph.rs `run()` ("input X takes an image, but Y outputs a scene"),
`Session::connect` and the node editor (refuses the drop; scene wires
and ports drawn blue). Plugin ports are image ports, so the ABI is
unchanged. `src/scene.rs`: `Scene {cameras, cards, points}`,
`Card {image: Arc<Image>, world, size}` + `corners()`, `extend`,
`describe`, params → placement/camera/card. ops.rs: `Category::Scene`
(shown "3D"), `scene_in`, ops camera/card/scene (`scene_output`,
`gather`); Camera's (empty) frame is `DEFAULT_FORMAT`. Not checked by
hand: dragging wires between 3D nodes, blue wire drawing. Tests:
`scene::cards_and_cameras_from_params`,
`scene::scenes_travel_along_typed_wires`.
**M4 done (v0.0.29): Scanline Render.** `render3d::render(scene, cam,
format, samples, background)`: `triangles()` clips each card quad at the
near plane (`clip_near`, Sutherland–Hodgman) and fans it into `Tri`s with
screen bounds; rows in parallel, an n×n sample grid per pixel,
barycentric tests per card (one hit per card), perspective-correct uv via
1/depth, bilinear lookup clamped to the card image's frame; hits sorted
by depth and composited front to back; depth = nearest hit with alpha >
0. `over_background` keeps the background's window and depth where
nothing renders. ops.rs: `scanline_render` (in `SIZED_SOURCES`); the
viewer names a scene's contents. Not done: mipmaps (minified cards alias
unless samples go up), motion blur, zmerge. Checked: a 1080p scene
(checkerboard floor and wall, a turned disc, tilted camera) renders
correctly from the CLI in 0.12 s at 3×3 samples. Tests:
`render3d::{a_card_filling_the_frame_renders_like_its_image,
perspective_depth_order_transparency_and_edges,
the_scanline_render_node_follows_the_camera,
renders_over_a_background_and_draws_points}`.
**M5 done (v0.0.31–0.0.32): features + KLT, library only** (nothing
calls it yet; no node or GUI). `src/features.rs`: `Gray` (luma, bilinear
sample), `Pyramid` (binomial blur + halve, down to ~32 px),
`detect(gray, max, min_distance, quality, border, avoid)` (Shi–Tomasi
min eigenvalue over 5×5, 3×3 local maxima, greedy spacing via a grid),
`lk_level` / `follow` / `follow_checked` (pyramidal LK, 15×15 window,
forward–backward error ≤ 0.5 px), `FeatureTrack {start, points}`,
`FeatureTracker::push(frame, img)`. Positions in frame pixels (pixel i
spans i..i+1). **Drift fixed (v0.0.32):** not a projection mismatch
(a sideways move tracks at 1.0002 of the true displacement); a
shift-only LK window can't follow content that stretches (pan edges,
push-ins), so each step lands slightly off and chaining adds it up. Each
step is now refined against the feature's first frame with an affine
window warp: `affine_refine(anchor, img, p, warp, q, r)` (19×19 window,
forward-additive Gauss–Newton on 6 params, nalgebra `Matrix6` Cholesky;
rejected if it doesn't settle, the warp's determinant leaves 0.5..2, or
it lands > 2 px from the followed position). `FeatureTracker` keeps
per-track warps and an anchor `Gray` per start frame (dropped when no
live track uses it). Test shot: 0.015 px median / 0.12 px p95 (was
0.22 / 0.94); `rendered_tracks_drift_under_a_tenth_of_a_pixel` is on
(median < 0.05, p95 < 0.2).
Speed: ~6.5 ms/frame at 1080p with ~200 live features (20 rendered
frames, push-in + pan + roll), so 200 frames in ~1.3 s.
**M6 done (v0.0.33): two-view init + triangulation.** `src/solve.rs`
works in the CV camera frame (x right, y down, z forward;
x_cam = R X + t). `Pose {r, t}` with `center`/`apply`/`from_center`/
`world()`/`from_world()` converting to geom's y-up, −Z-looking camera
(tested: same pixels either way); `Intrinsics {f, c}` (`new(focal,
film_width, format)`, `ray`, `project`); `Rng` (deterministic LCG);
`essential` (8-point via the 9×9 normal-equation eigenvector, singular
values forced to 1,1,0), `sampson`, `decompose` (4 poses),
`triangulate` (DLT, any number of views), `parallax`, `two_view`
(adaptive RANSAC, refit, cheirality + reprojection filter, median
parallax), `shared`, `initial_pair` (scans forward until median
parallax ≥ enough). Tests: pose conversion, exact noiseless
triangulation/decomposition, 0.3 px noise + 20 % outliers (rotation
< 0.1°, direction < 2°, points < 1 % after scaling), initial pair waits
for parallax and refuses a pure pan.
**M7 done (v0.0.34): incremental solve + bundle adjustment.**
`src/bundle.rs`: `Obs {cam, point, px}`, `Problem {poses, points, obs,
intr, fixed, points_fixed, solve_focal, huber}` with `residual`/`cost`/
`adjust(iterations)` → `Report {start_rms, end_rms, iterations}`;
analytic Jacobians; rotations updated R ← exp(w) R and renormalized
(without that, the constant-velocity guess compounded rounding into a 4°
error by frame 39). `src/sfm.rs`: `Settings {max_error 1 px,
min_parallax 1°, start_parallax 4°, min_shared 30, solve_focal}`,
`solve(tracks, first, last, intr, settings, progress: FnMut(f32) ->
bool)` → `Solution {intr, poses: BTreeMap<frame, Pose>, points:
Vec<Option<V3>> per track, rms, frame_rms}`; `similarity()` (Umeyama).
Scale and placement are arbitrary (start frame at the origin, 1 unit to
the second). No PnP: a frame starts from a neighbour's pose. Tests:
bundle (disturbed solve pulled back; focal from 15 % off with 5 %
outliers; cameras only), sfm (synthetic 40 frames, focal from 10 % off,
pure turn refused, rendered + tracked + solved).
**M8 done (v0.0.36): Camera Tracker node + point placement.**
`src/camtrack.rs`: `Settings::from_params`, `run(frames, settings,
&Progress)` (progress = tracked frames then solved, 2n in total;
cancellable), `Solution {format, focal mm, film_back, frames, cameras
[tx,ty,tz,rx,ry,rz] scene terms, points, rms, frame_rms}` (serde, kept
in the node's "solution" param, not a ParamSpec), `camera(frame)`,
`centroid`, `transform`/`set_origin`/`set_ground`/`set_scale`,
`fit_plane`, `card_on_points`, `camera_params` (linear keys). CV to
scene: flip diag(1, −1, −1) on world coords. ops.rs: `camera_tracker`,
`point_cloud`, `locator`, `Overlay::CameraTracker`. session.rs:
`track_camera`, `camera_solution`, `orient_camera`, `export_camera`,
`place_on_points`, `solution_summary`; 5 MCP tools; Python `Node`
methods. GUI: `src/gui/camera_tracker.rs` (`CamJob` on a thread, joined
when finished, writes the solution via `doc.set_param`; `export()`
mirrors `Session::export_camera` with Doc calls); `PointPicker` in
`Overlays` takes over interaction when a solved camera_tracker is
selected (press/drag_to pure, tested). Checked in the real GUI: a
48-frame rendered shot solved via MCP (48/48 frames, 378 points,
0.046 px rms, 1.2 s); the panel shows the result and the viewer draws
the solved points over the plate. **Not checked by hand:** selecting
points with the mouse in the live GUI (logic unit-tested), Place Card /
locator / origin / ground / scale from the GUI. Follow-ups: a live link
from a Camera to the solve (export bakes keys), picking over a Scanline
Render of the scene, solving frames between solved ones when tracks
allow (unsolved frames are listed), motion blur in Scanline Render.
Camera rig (v0.8.2, wD:p4): `Doc::add_camera_rig(pos, wire, up)` →
(camera, scene, render) in doc.rs; `NodeEditor::place(.., bare)` makes a
rig for Camera unless Shift is held (bare). Over an image wire the render
goes into the wire; after a selected image node it takes that node's
outputs; a selected scene node joins the Scene's 2nd input; else
free-standing. MCP `create_node` still adds just the node.
Tests: camtrack (orient, cards on points, locked card end to end),
`mcp::camera_tracker_tools`, the GUI picker, Python wrapper errors.
M8 now also includes **point placement** (select solved points → Place
Card here / Place locator; MCP/Python `place_on_points`), user request.
**M9, the 3D viewport, is specified** in `docs/phase-4-plan.md`
("Milestone 9 in detail": orbit/pan/dolly, look-through, ortho views,
picking and shared selection, move/rotate/scale gizmos, point cloud and
camera path); it's for a later session.
**M10, 3D objects and render passes (AOVs), is specified** there too
("Milestone 10 in detail": primitives, OBJ / glTF import, materials and
lights; Scanline Render writes passes as image planes: depth, P, N, uv,
motion, id, per-object mattes, lighting breakdown; a Shuffle node; EXR
multi-layer out); later session, after M9.

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

- **Arbitrary channels**: done as extra named planes beside RGBA
  (v0.0.23, Phase 4 M1; see above).
- **Expressions**: done. Live links with multiply/add (v0.0.19–0.0.20)
  and formula links (v0.0.22, worker wD:p3): `src/expr.rs` is the lexer,
  Pratt-style parser and evaluator (`Val` = number or list; `Lookup`
  returns `Result`). `link::evaluate` is Result-based (`resolve` =
  `unwrap_or` Null, `link::error` for the panel and graph); every ref goes
  through it with `MAX_DEPTH` 8, so loops are errors, not overflows.
  `link::sources` / `formula` / `expr` added; `graph::linked_params` fails
  the node on any link that fails, naming the param. The frame cache keys
  on resolved values, so formula or source edits re-render (tested).
  `noise` is splitmix-hashed 1-D gradient noise, pure in frame and seed.
  `time`/`fps` (v0.0.25): `expr::Clock { fps, first_frame }` (default
  24 / 1; fps <= 0 or non-finite counts as 24); `time = (frame -
  first_frame) / fps` (fractional at motion blur samples; `at(x, f)`
  moves time too). `Expr::eval`, `link::resolve` and `link::error` take a
  Clock; `Project::clock()` in graph.rs; `Evaluator::run` takes
  `&Project` instead of the format.
  Formula completions (v0.0.26): `properties::completions()` is pure
  (text, char cursor, Doc) -> (word start, list); `FORMULA_FUNCS` /
  `FORMULA_VARS` feed the list and the ? help, and a test parses every
  listed name so the list can't drift from `src/expr.rs`.
  One number for a list param (v0.0.28): `ops::resolve_at` (and
  `Doc::param_at` for the panel) repeats a link's or formula's single
  number to the length of the param's list default; plain stored numbers,
  number params and Null defaults (Transform's automatic center) are left
  alone.
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
- First frame after launch takes ~1.5 s (ffprobe + ffmpeg start); later
  frames are ~5 ms for the starter graph.
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
- Camera Tracker v1: free camera through a static scene only; a pure
  rotation (tripod) shot is refused; no lens-distortion solve; no mask
  for moving objects; Export bakes keys (no live link to the solve); the
  solve's scale and placement are arbitrary until Set origin / Ground /
  Scale are used.
- Scanline Render: cards only (no meshes yet), no mipmaps (minified
  cards alias unless samples are raised), no motion blur, no zmerge.

## Working tips

- **Master in a multi-agent session:** test a worker's milestone on its
  own when other workers have uncommitted files in the tree: `git
  worktree add --detach <scratch> HEAD`, copy in only the reported paths,
  and build with a separate `CARGO_TARGET_DIR`. Commit only reported
  paths (never `git add -A`); if a shared file holds another worker's
  hunk, stage a filtered copy with `git hash-object -w` + `git
  update-index --add --cacheinfo` (`--add` is needed for new files). Check reported files for leftover debug
  tests before committing (one slipped into 0.0.32). **Snapshot first:**
  copy the reported files to a scratch folder before testing, test that
  copy on HEAD, stage exactly those blobs, and build the committed HEAD
  again before tagging (workers keep editing after they report).
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
