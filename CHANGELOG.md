# Changelog

ArclightVFX Compositor (Arclight until 0.11.2) is **beta** software since
0.11.3 (alpha before): versions are `0.<phase>.<increment>`. The
middle number is the development phase the project is in (13 now:
procedural geometry, on branch `wip-ns-geo`, see `docs/geo-plan.md`;
phase 12, GPU acceleration, ran 0.12.0–0.12.19 on `wip-ns-gpu`; phase 11,
engine and platform (crash reporting, plugin process isolation), ran
0.11.0–0.11.5 on `wip-ns-engine`, ending in the first beta; phase 10,
Gaussian splats and follow-ups, ran 0.10.0–0.10.37 on `wip-ns-splats`;
phase 9, the 3D viewport, ran 0.9.0–0.9.68 on `wip-ns-3dwork`), and every feature or change that
ships bumps the last number (0.9.1, 0.9.2, … 0.9.nn). Phase 9 started
at 0.9.0 (the user's choice); phase 8 ran 0.8.1–0.8.4. Renumbered from `0.0.36` to `0.8.1` on 2026-10-29 at the
user's request (nothing else changed then). The version is
set in `Cargo.toml` (also `python/Cargo.toml` and
`plugins/example/Cargo.toml`, kept in step) and shows in the GUI's Help
menu, `arclight --version`, the MCP server info and Python's
`arclt.__version__`.

## 0.13.8 — 2026-10-31

- Geometry: **Geo Info**, the viewer's **Sheet** toggle: the viewed
  node's 3D scene as a spreadsheet under the view, as Blender's: point
  sets' points, mesh points, faces, instances and curve points as tabs
  with their counts, a row per element and a column per value (P, N, uv,
  instances' rotation and scale, every attribute; colors with a swatch),
  one object or the selected ones; a million rows scroll smoothly. The
  same table for scripts: the MCP tool `geometry` and Python's
  `node.geometry()` / `arclt.geometry()`, in pages of columns.

## 0.13.7 — 2026-10-31

- New **Transform Geo** node: moves, turns and scales what's in a scene
  (meshes, cards, point sets, curves, instances, splat captures) about a
  pivot, all of it or one node's (Group, with `*` patterns); cameras and
  lights stay unless asked.
- The 3D view draws point sets as dots (sized by pscale, colored by Cd),
  curves as lines and instances as boxes.

## 0.13.6 — 2026-10-31

- Node graph: a wire dropped on a node's body no longer goes into its mask
  input: on a node whose main input is wired it replaces that input
  instead. A mask takes a wire only when it's dropped on the mask's own
  port.

## 0.13.5 — 2026-10-31

- Geometry (phase 13, P1): scenes can now carry point sets, instances and
  curves with named attributes (Float, Vec3, Vec4, Int), and meshes gain
  point and face attributes. Nothing makes them yet; this is the base for
  the geometry nodes (`docs/geo-plan.md`). The 3D view frames and picks
  them.

## 0.13.4 — 2026-10-31

- GPU: soft shadows render faster again: lookups skip the parts of a
  shadow map that are wholly lit or wholly in shadow, read from a small
  summary of each map (4K, a Quality 16 area light: 15.8 s to 7.0 s at
  softness 1, 31.4 s to 12.0 s at softness 2; 22.9 s and 51.2 s before
  0.13.3), with the same pixels as before.

## 0.13.3 — 2026-10-31

- GPU: soft shadows render faster, most of all from area lights at high
  Quality: each shadow-map texel under the softening filter is read once
  instead of up to four times (4K, a Quality 16 area light: 22.9 s to
  15.8 s at softness 1, 51.2 s to 31.4 s at softness 2), with the same
  pixels as before.

## 0.13.2 — 2026-10-31

- Node graph: drop a wire anywhere on a node (body or title) to connect
  it, without aiming for the port: from an output it goes into the node's
  first free input that takes it (else its first, replacing that wire),
  from an input it takes the node's output. While hovering, the node
  lights up and the wire's end snaps to the port it will take; nothing
  connects if no input fits or it would make a cycle. Dropping on a port
  works as before and wins.

## 0.13.1 — 2026-10-31

- A selected object in the 3D view now shows the Move gizmo instead of
  all three; G, R and S switch to the Move, Rotate or Scale gizmo as they
  start, so the rotate rings stay up for tweaks after R (All is still on
  the toolbar).

## 0.13.0 — 2026-10-31

- Phase 13 begins: procedural geometry nodes (GeoNodes-like: scatter,
  attributes, instancing, mesh and curve ops), planned in
  `docs/geo-plan.md`. No change to the app yet.

## 0.12.19 — 2026-10-31

- The transport bar keeps each label with its field (Frame, In, Out, FPS,
  Format) when it wraps in a narrow window, instead of leaving a field on
  the next line or past the edge.

## 0.12.18 — 2026-10-31

- GPU: played frames rendered on the GPU keep their color there, so cached
  playback draws straight from the GPU instead of converting and uploading
  every frame: 24 fps held at 1080p and 4K where it managed ~20 and ~10
  (4K at 60 fps: ~54 instead of ~10). Its own budget, `gpu.frame_cache_mb`
  (default 1024, Preferences ▸ Performance ▸ GPU frame cache), apart from
  `gpu.cache_mb`; past it the least recently played frames play from the
  CPU as before, and turning GPU acceleration off lets them all go.
- Hand-set GPU budgets in settings.json are no longer lost when
  Preferences are saved.

## 0.12.17 — 2026-10-31

- The viewer's toolbar wraps onto a second row when the view is narrow,
  instead of cutting off its last controls (Fit, 1:1, the zoom) and
  pushing the image past the view's edge; the 3D view's toolbar too.
- README: shadows of see-through casters render on the GPU too.

## 0.12.16 — 2026-10-31

- GPU: **shadows cast by see-through casters** (cards below full opacity,
  cutout textures with soft edges) now render on the GPU instead of
  sending the whole frame to the CPU: each shadow map keeps up to 4
  see-through layers a texel, and pixels needing more are rendered on the
  CPU. On smaller GPUs the layers shrink to fit a memory budget (384 MB
  discrete, 160 MB integrated), and a frame that still won't fit renders
  on the CPU. A 4K scene with sun, spot and a 16-sample area light: 34 s
  instead of 343 s.

## 0.12.15 — 2026-10-31

- Long node errors (a plugin crash with its report path, a missing file's
  full path) wrap in the viewer and the 3D view instead of running off
  the edge, and can be selected and copied. The status line shows as much
  as fits, all of it on hover.

## 0.12.14 — 2026-10-31

- GPU: **motion blur over Gaussian splats adds up on the GPU too**, each
  time's view prepared there: a moving capture with 8 times at 4K in
  0.56 s instead of 4.2 s (1080p: 0.46 s instead of 2.1 s).
- The GPU motion-blur tests say, when they fail, whether the picture came
  from the GPU sum or another way, and the last GPU error.

## 0.12.13 — 2026-10-31

- GPU: **the 3D view draws its shaded picture straight from the GPU**:
  the render stays there and is made into its 8-bit form there, no
  read-back while orbiting. At 1080p a settled render reaches the screen
  every display frame (16.7 ms) instead of every 65–70 ms, the render
  takes ~8 ms instead of ~31 for the lizard sample, and the interface
  thread no longer spends ~30 ms on each. Motion-blurred previews,
  see-through shadow casters and GPU acceleration off use the old path.

## 0.12.12 — 2026-10-31

- GPU: anti-aliased renders of Gaussian splats blend the splats once a
  pixel instead of once per row of samples (4K: 143 to 128 ms for the
  lizard sample).

## 0.12.11 — 2026-10-31

- GPU: **Shuffle and Merge's zmerge run on the GPU too**, so a 2D chain
  no longer drops to the CPU for them; a plain Shuffle costs nothing
  (its buffers are shared). Shuffle follows the CPU's own layer and
  naming rules exactly (it is worked out on a 1×1 probe by the CPU
  Shuffle itself).

## 0.12.10 — 2026-10-31

- GPU: **Gaussian splats render on the GPU**: the view is prepared
  (projected, colored, sorted per tile) and blended there, mixed with
  surfaces as on the CPU, every pass included. The 3D view orbits the
  lizard sample at 10.7 ms a frame at 720p instead of 60 (17 ms instead
  of 82 at 1080p). This completes the GPU plan's G1–G7.

## 0.12.9 — 2026-10-31

- GPU: **the viewer shows results made on the GPU straight from the
  GPU**: the window shares the GPU device and the 8-bit display is made
  there, not on the interface thread (4K: about 20 ms less per new frame
  on the interface thread, 25–50 ms less from request to screen;
  scrubbing a GPU chain keeps up with the display, 60 fps instead of
  30–40). The pixel readout and pickers read exact values as before;
  with GPU acceleration off everything is as it was.
- Viewer and Media Out nodes pass their input on without copying it (a
  4K frame copy less per frame).
- GPU errors are printed to the terminal as well as kept for crash
  reports.

## 0.12.8 — 2026-10-31

- Plugin helper processes end without the exit-time teardown, as the
  command line and the GUI do: a crash as they ended, which left core
  dumps after every session with a crashing plugin, is gone. (The
  Python plugin host, a process of its own, does the same; the user's
  own interpreter is never affected.)

## 0.12.7 — 2026-10-31

- GPU: **motion blur over particle sprites adds up on the GPU too**: at
  4K, 8 times, 50 000 sprites in 0.33 s instead of 6.7 s, 500 000 in
  2.7 s instead of 10.4 s. Additive sprites over a background still
  render time by time. This completes G5 (shadows, motion blur,
  particles).

## 0.12.6 — 2026-10-31

- GPU: **particle sprites render on the GPU** where they cover much of
  the frame (big or many: smoke, dust): 20 000 big textured sprites at
  4K in 0.3 s instead of a minute. Small ones stay on the CPU, which is
  quicker for them. Motion-blurred sprites still render time by time
  (next).
- The CPU's sprite merge and background copy run in parallel.

## 0.12.5 — 2026-10-31

- `arclight mcp --http` stops cleanly on Ctrl-C or SIGTERM: it finishes
  the request in progress (new ones get 503), then shuts the GPU down and
  exits 0; a second Ctrl-C stops at once.

## 0.12.4 — 2026-10-31

- GPU: **Transform, Corner Pin and Tracker (with motion blur), Lens
  Distort, Resize and the Keyer run on the GPU**; motion-blurred warps,
  Lens Distort's distort mode and the Keyer always, the others when their
  input is already on the GPU.
- `arclight render` reads back from the GPU only what its writes need: a
  4K blur / grade / merge chain renders about twice as fast as on the
  CPU. GPU read-backs reuse their staging buffers.
- The command line and the GUI end without the C libraries' exit-time
  teardown, after everything is written: a rare crash (or hang) in
  NVIDIA's Vulkan driver as the program ended is gone. Python waits for
  the GPU at exit.
- **Crash reports:** the crash handler can no longer hang (it gives up
  after 3 s, ending with the crash's signal), and crashes on worker
  threads (rendering, the GUI's evaluation) are now reported: their
  small signal stacks used to overflow, losing the report.
- `arclight gpu --check --quick` (MCP `"quick": true`, Python
  `gpu_check(quick=True)`): the 2D checks at one small size, in seconds.
  The full check also covers warps, Lens Distort, Resize and the Keyer;
  warps pass by the plan's statistical rule (every value within 1e-4, at
  most 0.1 % off, as f32 positions differ by a hair).

## 0.12.3 — 2026-10-31

- GPU: **shadows render on the GPU**: every light kind's shadow maps are
  drawn and filtered there as on the CPU (soft edges, a light's radius,
  the shadow and per-light passes): at 4K about 17× faster than the CPU
  here. Shadows cast by something see-through still render on the CPU.
- GPU: **motion blur in Scanline Render adds its times up on the GPU**,
  read back once: at 4K, 8 times, about 21× faster than the CPU (12.5 s
  → 0.59 s), 35× with shadows (30 s → 0.87 s).

## 0.12.2 — 2026-10-31

- GPU: **Scanline Render and the 3D view render on the GPU**: a compute
  rasterizer that follows the CPU renderer sample for sample (every light
  and pass, anti-aliasing, holdouts, see-through stacks, mipmaps made on
  the GPU). 4K anti-aliased is about 12× faster than the CPU here, the 3D
  view about 2–3×. Scenes with shadow-casting lights or splats still
  render on the CPU (G5, G6). Pixels with very deep see-through stacks
  (over 64 layers) are finished on the CPU, the rest stays on the GPU.
- The CPU renderer's pass assembly runs in parallel (a little faster).
- The GPU self-test gains four 3D rows.

## 0.12.1 — 2026-10-30

- GPU: **Blur runs on the GPU**, and Grade, Exposure, Saturation, Invert,
  Clamp, Premult, Unpremult, Channel Select, Switch, Dissolve, Keymix and
  Merge (all but zmerge) with their masks and mix, when their input is
  already on the GPU. Results stay on the GPU between GPU nodes and are
  read back only when needed.
- GPU memory for results kept there: settings.json `"gpu": {"cache_mb":
  2048}`; past it the least recently used go back to the CPU.
- Turning GPU acceleration on or off re-renders only the nodes that can
  run on the GPU, and what's below them.
- The CPU Blur no longer slows down with its radius (a running sum): a
  4K radius 100 blur went from about 2 s to 0.23 s.
- `arclight gpu --check` also checks and times Grade (with a mask),
  Merge and Blur against the CPU.

## 0.12.0 — 2026-10-30

Phase 12 starts: **GPU acceleration** (wgpu: Vulkan on Linux, Metal on
macOS), branch `wip-ns-gpu`, plan `docs/gpu-plan.md`. This first step
lays the groundwork: no node runs on the GPU yet, but the switch,
status and self-test work end to end.

- GPU acceleration core: a `gpu` cargo feature, on by default in the
  GUI, CLI and Python wheels. The device starts the first time a render
  needs it.
- **Edit ▸ Preferences ▸ Performance ▸ GPU acceleration**
  (settings.json `"gpu": {"enabled"}`, on by default): applies without a
  restart, shows the GPU in use or why the CPU is, greyed when
  `ARCLIGHT_GPU=0`; the setting applies to the CLI, MCP and Python too.
  The About box shows the same.
- `arclight gpu` shows whether the GPU is in use (adapter, backend,
  limits, transfer path) or why not; `arclight gpu --check [--json]`
  runs the GPU self-test (GPU vs CPU, errors and timings; exit status 1
  on failure), also **Help ▸ GPU self-test**, MCP `gpu_info` /
  `gpu_check`, Python `arclt.gpu_info()` / `arclt.gpu_check()`.
- `--cpu` on `arclight render` and `arclight demo`, `"cpu": true` on MCP
  `render_frame` / `render_output`, `cpu=True` on Python `render`: that
  render runs on the CPU only, for exact results. `ARCLIGHT_GPU=0` forces
  the CPU everywhere.
- Crash reports say whether GPU acceleration was on, and with which
  adapter; after such a crash the crash dialog offers to turn it off.

## 0.11.5 — 2026-10-30

- Tests: the GUI tests no longer panic under egui 0.37 with "Dropped
  TexturesDelta with 1 unapplied deltas"; each test run clears its
  texture deltas (`FullOutput::drop_without_applying_deltas`). The app
  itself was never affected.

## 0.11.4 — 2026-10-30

- **3D view: gizmos and smoother rotation.** A selected object now
  shows the **All** gizmo by default (move arrows, rotate rings and scale
  cubes together) rather than only the move arrows; the toolbar's Move /
  Rotate / Scale / Select still pick one.
- Dragging a rotate ring turns by how far the pointer moves along the
  ring. It used to go by the pointer's angle about the object's center,
  which flipped back and forth when the ring was seen edge-on or the
  pointer crossed the center.
- **R** (rotate by the mouse) is damped within 60 points of the object's
  center, so small moves close to the object no longer swing it wildly.
- Orbiting turns around the selected objects, which stay in place on
  screen instead of sweeping across it (Preferences ▸ 3D view ▸ Orbit
  around the selection; on by default). With nothing selected it turns
  around the middle of the view, as before.
- Linux: a smooth-scrolling mouse wheel's part-notch steps zoom instead
  of being taken for a trackpad and orbiting.

## 0.11.3 — 2026-10-30

- **Beta: Arclight is now ArclightVFX Compositor** (ArclightVFX for
  short). Renamed: the window, About box (with a Beta badge), Help, crash
  dialog and reports, MCP server title, the Linux launcher and tarball
  (`arclightvfx-<ver>-linux-x86_64.tar.gz`), and the Mac app and DMG
  (`ArclightVFX.app`, `ArclightVFX-<ver>-universal.dmg`). Unchanged: the
  commands (`arclight`, `arclight-gui`), the `arclt` Python module,
  settings and project files, and environment variables.
- Upgrading: Mac users delete the old `Arclight.app` from Applications
  once. Linux users re-run `install.sh` (it replaces the old launcher
  entry).

## 0.11.2 — 2026-10-30

- **Plugins run in helper processes** (Linux, macOS): a native plugin
  that crashes, aborts or hangs no longer takes Arclight (or Python)
  down. Its node shows "plugin X crashed: killed by SIGSEGV (report: …)"
  or "timed out after 60 s", and the next render starts it again.
- Plugins work unchanged (same ABI), with images passed in shared memory.
  That costs about 30 ms on a 4K frame; `ARCLIGHT_PLUGINS_IN_PROCESS=1`
  loads trusted plugins in-process instead. `ARCLIGHT_PLUGIN_TIMEOUT` sets
  the time limit (seconds).
- A plugin's ops are remembered, so a project still opens with them when
  its plugin can't start.

## 0.11.1 — 2026-10-30

- **Crash reports**: if Arclight crashes (a panic, or a native crash in a
  decoder or plugin), a report of what it was doing (version, system,
  the backtrace, the last actions, the node being evaluated) is saved on
  this computer, in `~/.local/state/arclight/crashes` (macOS:
  `~/Library/Logs/Arclight`). Nothing is sent. File paths are hidden
  unless you choose to include them.
- After a crash, the open project's unsaved changes are saved to its
  autosave, and the next start asks: view or copy the report, or open a
  prefilled GitHub issue to review and submit (Don't ask again turns this
  off). **Help ▸ Report a problem** does the same any time.
- An internal error while rendering a frame, the 3D view, sound or a
  script's call no longer stops that part of Arclight: it shows as that
  frame's error, and a report is saved.

## 0.11.0 — 2026-10-30

- **Phase 11 begins: engine and platform**, on branch `wip-ns-engine`:
  crash reporting and plugin process isolation, then a beta for a public
  release. Same features as 0.10.37.

## 0.10.37 — 2026-10-30

- A card, light or camera **selected in the node graph gets its gizmo**
  in the 3D view wherever it is: the 3D view shows a render's scene also
  when viewing a node after it (a Merge, a Grade…), and a selected object
  that isn't in the viewed scene is drawn dashed and can still be moved
  (the viewer says so). When it can't have a gizmo (behind the view,
  being looked through, an error), the viewer says why.
- **Easier picking in the 3D view**: cards seen edge-on, camera frustums
  and light icons can be clicked near their lines (8 px); clicking again
  in the same spot picks the next thing under the pointer; what a click
  would pick is highlighted as you hover.

## 0.10.36 — 2026-10-30

- **Turn nodes off** to troubleshoot: a switch on each node in the graph
  (or **D** for the selected nodes, or Enabled in Properties) bypasses a
  node, as one undo step. A node that's off passes on its input (a Merge
  its B background, a Scanline Render its background, a particle
  modifier its stream, a Scene its first input); a source that's off
  (Read, Constant, Camera, lights, emitters) outputs nothing: a
  transparent frame or an empty scene. A Write or Media Out that's off
  writes nothing (rendering a Media Out that's off says so; the CLI skips
  it). Nodes that are off are drawn dimmed and struck through, and are
  saved with the project; older projects load unchanged. Sound follows
  what's passed on. Scripts: MCP `set_disabled` and `describe_node`'s
  `disabled`, Python `Node.disabled`.

## 0.10.35 — 2026-10-30

- **Sound in playback**: playing forward plays the viewed node's audio
  (a Media In's or Read's clip, through the nodes after it), in step
  with the picture. It's silent while paused, stepping, scrubbing or
  playing backward; a loop restarts it at the loop point; when frames
  can't render in time the sound drops out rather than stutter or drift,
  and comes back in step. The **speaker** button on the transport turns
  it on or off (remembered; on by default). Without an output device,
  sound is off and the status line says so. (Linux: the GUI now uses
  `libasound.so.2` at run time and ALSA's development files to build.)

## 0.10.34 — 2026-10-30

- Clips with sound: the Read and Media In panels (and MCP / Python
  metadata) show the audio's sample rate and channels.
- **Media Out keeps the sound** of the clip upstream in videos (new
  **Audio** setting: auto, none, or a node), for exactly the rendered
  frames and in sync: silent before the clip starts, sped up or slowed if
  Media Out's frame rate differs from the clip's. AAC in `.mp4` / `.m4v`,
  FLAC in `.mkv` (AAC plays 21 ms early there), 24-bit PCM in `.mov` /
  `.mxf` / `.avi`, Opus in `.webm`.
- New core audio API (`arclight::audio`: `probe`, `decode`,
  `source_at`) for reading clips' sound.

## 0.10.33 — 2026-10-30

- Media Out's properties have a **Format** menu (image sequences EXR,
  DPX, TIFF, PNG, JPEG, WebP, TGA; video MP4, MOV, MKV, WebM, MXF, M4V,
  AVI) that renames the file. Switching between a sequence and a video
  adds or drops the frame number (`shot.mov` ↔ `shot.####.exr`), and an
  empty file gets a name in the project folder. The Codec menu offers
  only codecs the chosen container holds, and goes back to auto when a
  new format can't hold the current one. Codec and FPS show only for
  video, the DPX settings only for DPX.

## 0.10.32 — 2026-10-30

- **Media Out writes DNxHR, VP9 and AV1**: DNxHR HQX and DNxHR 444
  (10-bit, `.mov` / `.mxf` / `.mkv`), VP9 10-bit and VP9 with alpha (the
  web's transparent video, `.webm` / `.mkv`), and AV1 10-bit (SVT-AV1, or
  libaom if that's what ffmpeg has). `auto` picks VP9 for `.webm` and
  DNxHR HQX for `.mxf`.
- A codec its container can't hold, a container Media Out can't write,
  or an encoder missing from the installed ffmpeg is refused before
  rendering, with a message saying what works. A failed encode reports
  ffmpeg's reason and leaves no broken file (a `.webm` used to fail with
  "Broken pipe" and leave a 262-byte one).
- The Media Out save dialog lists `.mkv`, `.webm`, `.mxf`, `.m4v` and
  `.avi`.

## 0.10.31 — 2026-10-30

- The Media Out **Render** button goes on from the particles the viewer
  has already simulated (where they still match the graph) instead of
  simulating from the first frame, without changing the viewer's
  simulation.

## 0.10.30 — 2026-10-30

- Bridged calls that render particles at a late frame (a Scanline Render
  or Particles 2D through `render_frame`, or scene-reading calls)
  simulate a short run at a time, so the viewer keeps updating
  meanwhile. Renders over the bridge (`render_frame`, `render_output`)
  and a session's `render_output` go on from the particles already
  simulated, where they still match the rendered graph, instead of
  simulating from the first frame, and never change the viewer's
  simulation.

## 0.10.29 — 2026-10-30

- Internal: `cargo clippy` is clean across the workspace (`Graph`
  implements `FromStr`; the example plugin walks pixels with
  `as_chunks`).

## 0.10.28 — 2026-10-30

- **Scripts place nodes in the node editor**: the MCP tool
  `set_node_position` moves one node or many at once, as one undo step;
  `layout_nodes` lays out the unplaced nodes (or all) by depth, left to
  right, as the editor does for a graph without positions; `create_node`
  takes a `pos`. In Python: `Node.pos`, `arclt.set_positions()`,
  `arclt.layout()` and `create_node(..., pos=)`. Placed this way,
  backdrops made around nodes fit headless too. Moves show in an
  attached GUI and undo there.
- Fixed: the error for a bad node name in `create_node` called it a
  backdrop name, and the reverse (0.10.24).

## 0.10.27 — 2026-10-30

- The `arclt` source package (sdist) leaves out the repo's `examples/`
  (sample media and splat captures): 851 KB instead of 20 MB. Nothing
  compiled reads them; the wheels and the Linux tarball are unchanged.

## 0.10.26 — 2026-10-30

- A real Gaussian splat capture to try: `examples/geo/hornedlizard.spz`
  (a horned lizard statue, 786 k splats, SH degree 3), from Niantic's
  SPZ repository under its MIT license (notice in
  `examples/geo/hornedlizard.LICENSE.txt`). `examples/hornedlizard.json`
  opens it cropped (Splat Crop drops the capture's far-off background
  shell), with a Camera and a Scanline Render.

## 0.10.25 — 2026-10-30

- Player and edit keys (Space, J / K / L, arrows, Home / End, Delete, V,
  F) no longer fire with Shift or Alt held (Alt+arrows still jumps
  between keys), and the viewer's G / R / S gestures need no modifier.
- Home and F over the 3D view only frame the view: Home no longer also
  jumps to the first frame.
- In a formula field, Shift+Tab moves focus back instead of taking the
  completion; Tab still takes it.

## 0.10.24 — 2026-10-30

- **Backdrops from scripts**: MCP tools `list_backdrops`,
  `create_backdrop`, `update_backdrop` and `delete_backdrop`, and in
  Python `arclt.backdrops()`, `arclt.backdrop(id)`,
  `arclt.create_backdrop(...)` and `Backdrop` objects (title / color /
  font_size / pos / size, `set()`, `fit()`, `nodes()`, `delete()`). A box
  is made around nodes with a margin as Group does, or at a position and
  size; colors are a palette name (slate, teal, green, olive, amber,
  rust, plum, blue), an index 0–7, or [r, g, b]. Each edit is one undo
  step, also in a GUI attached over the bridge.

## 0.10.23 — 2026-10-30

- Shift+Tab over the viewer no longer also switches it between 2D and
  3D; only a plain Tab does (Shift+Tab is the node browser's, over the
  node graph).

## 0.10.22 — 2026-10-30

- **Read Geo reads Alembic caches** (`.abc`, Ogawa): polygon and
  subdivision meshes under their transforms, moving and deforming, with
  their normals and UVs, at each frame's time ((frame + Frame offset) /
  Frame rate; the rate starts as the project's). Deforming meshes
  motion-blur. Alembic has no materials, so meshes come in white: use
  Override material. Curves, points and cameras aren't read yet. A
  sample is `examples/geo/anim.abc`.

## 0.10.21 — 2026-10-30

- **Mipmaps**: a texture seen smaller than its own pixels (a card or a
  model far away or at a grazing angle) now takes the texture's mean
  over what each sample covers, from a pyramid of halved copies made on
  first need and kept, instead of shimmering and moiré-ing. Textures
  seen 1:1 or closer render exactly as before. Scanline Render and the
  3D view's preview both use it.

## 0.10.20 — 2026-10-30

- **Orthographic cameras**: a Camera's new **Projection** (perspective /
  orthographic) and **Ortho width** (scene units across the frame,
  animatable) make a Scanline Render see straight down the camera's
  axis, with no perspective, for plan and elevation views. Cards, meshes,
  splats and particles all render orthographically, with exact textures
  and depth / P passes. The 3D view's orthographic mode (5, axis views)
  now renders through a true orthographic camera instead of a very long
  lens, and looks through orthographic Cameras (drawn as a box).
  Directional lights' shadow maps are orthographic too.

## 0.10.19 — 2026-10-30

- Camera Tracker: **Solve lens distortion** solves the lens's radial
  distortion (K1, K2) with the camera, as Lens Distort's values about the
  frame's center (not for Tripod solves). The panel shows them, and
  Export (GUI, MCP, Python) adds a Lens Distort undistorting the plate
  under the render (Distort with the same values bends CG to match the
  original plate). Old solves load as a perfect lens.

## 0.10.18 — 2026-10-30

- Fixed: a Scanline Render whose saved params leave out some settings (a
  hand-written or script-made graph) rendered without motion blur or the
  motion pass's next frame.

## 0.10.17 — 2026-10-30

- The Camera Tracker's **point picking also works over a Scanline Render**
  of the solved scene (or anything downstream of it): with the render,
  the viewed node or its camera selected, the solved points show over
  the render. Click, ⇧-click or drag a box to place a Card or Locator, or
  to set the origin, ground and scale. A render counts when its camera is
  linked to the solve, or when the tracker is its background (a baked
  Export).

## 0.10.16 — 2026-10-30

- Camera Tracker **Export can link the Camera to the solve live** (the
  Linked checkbox, on by default): its translate, rotate, focal and film
  back follow the solve, so re-solving or setting the origin, ground or
  scale moves it; between frames (motion blur) it moves as the keyed copy
  would. In Properties, **Bake** keys it on every solved frame; Unlink
  keeps that frame's value. MCP `export_camera` now links by default
  (`"linked": false` keys a copy as before); Python
  `export_camera(linked=True)`.

## 0.10.15 — 2026-10-30

- Camera Tracker has a **mask** input: features aren't found where the
  mask covers (its Mask channel, Invert mask), and features that move
  into it are dropped on that frame. Mask out people or cars crossing the
  shot so they don't pull on the solve; an animated Roto follows them
  frame by frame.

## 0.10.14 — 2026-10-30

- Read Splat's Up axis defaults to -y (Y down, Z forward), the way most
  captures come (COLMAP / INRIA `.ply`, and `.splat` converted from
  them), so they read upright. `.spz` files (natively Y up) are now read
  turned into the same Y down, and written back the same way, so every
  format reads upright by default. Projects saved before keep their Up
  axis; a graph file written by hand with no `up` key now reads Y down.
  The sample `examples/geo/splats.ply` is now Y down too.

## 0.10.13 — 2026-10-30

- **Splats render 2–2.5× faster** (Dev2, splat plan S5): 1 M at
  1920×1080 0.36 → 0.17 s, 3 M 0.97 → 0.38 s, by binning each splat only
  into the tiles its ellipse reaches, sorted without a global sort. A
  re-render of the same view (another pass, anti-aliasing, an edit
  elsewhere) reuses the projected splats (3 M: 0.97 → 0.09 s). The 3D
  view takes splat color from SH degree 0–1. Read Splat's Browse lists
  `.ply`, `.splat` and `.spz`. Faint splat edges may differ by up to
  about 1% alpha from 0.10.3–0.10.12.

## 0.10.12 — 2026-10-30

- **Read Splat reads `.splat` and `.spz`** (Dev1, splat plan S5): the
  antimatter15 web format and Niantic's compressed format (versions 2
  and 3) as well as `.ply`. (`.spz` needs the `flate2` crate, already in
  the build through the image libraries.)

## 0.10.11 — 2026-10-30

- **Splat Crop's region in the 3D view** (Dev2, splat plan S4): its box
  or sphere shows (purple; orange when selected) for any viewed node
  downstream; click its middle to select it, and move, turn or resize it
  with G / R / S or the gizmo (its Center, Rotate and Size).

## 0.10.10 — 2026-10-30

- **Scene regions** (Dev1, internal, for the Splat Crop handle): Splat
  Crop now passes its box or sphere along in the scene (`Scene.regions`,
  never rendered), so the 3D view can show and drag it downstream.

## 0.10.9 — 2026-10-30

- **Splat Color** (Dev1, splat plan S4): grades Gaussian splat captures
  (Exposure, Saturation, Gain, exactly from every viewing angle) and
  their opacity; View independent drops the color's change with the
  angle (quicker, and steadier where reflections flicker).

## 0.10.8 — 2026-10-30

- **Splats in Scanline Render's passes** (Dev2, splat plan S3): depth
  and P where they become half opaque, N from the strongest splat's flat
  side, motion from the capture's and camera's moves, an id and a matte
  per Read Splat (mattes still add up to alpha), and the emission light
  pass (the light passes still add up to the picture). Holdout Read
  Splats cut out what's behind them; an animated Read Splat motion-blurs.

## 0.10.7 — 2026-10-30

- **Splat Crop** (Dev1, splat plan S4): keeps a Gaussian splat capture's
  splats inside a box or sphere (Center, Size, Rotate; Invert keeps
  those outside), to cut a capture down or clear floaters; everything
  else in the scene passes through.
- **Read Splat: Holdout** (Dev1): marks a capture as a holdout (it will
  draw nothing but hide what's behind it, as a holdout mesh does; the
  renderer honours it with the render-pass work, S3, next).
- Fixed: two threads reading the same splat file at once could each
  cache their own copy.

## 0.10.6 — 2026-10-30

- **The 3D view renders splats in full once it settles** (Dev2): with
  cards and meshes by depth, and in the motion-blurred preview; their
  centers show as dots while the view moves, and a selected capture
  shows its box.

## 0.10.5 — 2026-10-30

- **Splats in scripting** (Dev1): `scene_objects` lists splat captures
  (node, count, bounds, SH degree, position), and a new `splats` query
  (MCP `splats`, Python `Node.splats()`) returns a scene's splats in
  pages: positions in the scene, scales, rotations, opacities, colors and
  which capture each came from.

## 0.10.4 — 2026-10-30

- **Splats in the 3D view: framing, box selection, picking** (Dev2): F
  frames splat captures by their extent, the selection box takes them,
  and clicking near any of a capture's splat centers selects its Read
  Splat. Read Splat's Browse lists `.ply` files.

## 0.10.3 — 2026-10-30

- **Scanline Render draws Gaussian splats** (Dev2, splat plan S1): a CPU
  3DGS rasterizer (EWA projection, view-dependent color up to degree 3,
  16×16 tiles sorted by depth, front-to-back blend) mixed per pixel with
  cards and meshes in depth order: see-through surfaces blend with
  splats, opaque ones and holdouts cut them; depth is where the splats
  get half opaque. 1 M splats at 1920×1080 render in about 0.4 s. The 3D
  view shows splat centers as dots in their color (up to 300 k per set).

## 0.10.2 — 2026-10-30

- **Read Splat** (Dev1, splat plan S1): reads a Gaussian splat capture
  (`.ply`, as Postshot, Polycam, Luma, Nerfstudio, gsplat and the INRIA
  3DGS code export it: binary or ASCII, color detail up to
  spherical-harmonics degree 3) into the 3D scene, with Up axis (as is,
  Z-up, or Y-down for COLMAP captures), Translate / Rotate / Scale,
  Opacity and a Color detail cap. 1 M splats read in about 0.1 s. A
  sample capture is in `examples/geo/splats.ply`. The renderer follows.

## 0.10.1 — 2026-10-30

- **Splat data model** (Dev1, internal, splat plan S1): `splat::Splats`
  (positions, scales, rotations, opacity, spherical-harmonic color up to
  degree 3, shared without copying) and `Scene.splats`, carried through
  scenes and motion blur. Nothing user-visible yet: the reader, Read
  Splat node and renderer follow.

## 0.10.0 — 2026-10-30

- **Phase 10 begins: Gaussian splats** (`docs/splat-plan.md`), on branch
  `wip-ns-splats`. Same features as 0.9.68.

## 0.9.68 — 2026-10-29

- **Relative media paths: a project folder can move** (Dev1): projects
  with a folder save the files in it (plates, geometry, renders) relative
  to it, and the folder relative to the project file when it's inside,
  so the whole folder can be moved or copied to another machine and
  opened as it was. Files elsewhere stay absolute; projects without a
  folder save exactly as before. A path typed relative (in a node's File
  field, or over MCP / Python, including render_frame's path) is in the
  project's folder; File fields show paths in the folder relative to it.
  MCP set_project takes "folder".

## 0.9.67 — 2026-10-29

- **Project folder** (Dev1): File ▸ New asks for the new project's name
  and folder (made in Preferences ▸ Files and safety ▸ Projects folder
  unless you choose another; or none). Media and render file dialogs
  start in the project's folder; a saved project without one uses the
  folder its file is in. File ▸ Project Settings shows and changes it
  (Browse…). Preferences' separate media / render folders are gone; the
  Projects folder is where new projects go and where Open starts. Older
  projects load and save unchanged.

## 0.9.66 — 2026-10-29

- **Preferences: New project, Undo levels and 3D view** (Dev2):
  Preferences ▸ New project sets the format, frame rate and frame range
  File ▸ New starts with (Project Settings ▸ Save as defaults sets them
  from the open project); Files ▸ Undo levels now limits undo; 3D view:
  emulate a 3-button mouse, two-finger scroll orbits or pans, zoom toward
  the pointer or the middle, invert zoom, and the shading projects
  without their own view start with.

## 0.9.65 — 2026-10-29

- **Preferences** (Dev1): **Edit ▸ Preferences** (Ctrl/⌘+,), saved in
  settings.json: General (plugin folders, frame cache, MCP address),
  Interface (scale; theme: follow the system, dark or light; snap nodes
  to the grid; curved or straight wires) and Files and safety (autosave
  every N minutes to `<project>.autosave` beside the project, never over
  it; how many recent projects; undo levels; the folders project, media
  and render dialogs start in). **File ▸ Open Recent** lists recent
  projects. Apply, OK, Cancel and Reset to defaults; without settings
  Arclight works as before. Also fixed: at a non-1 interface scale the
  node graph could open fitted off-screen.

## 0.9.64 — 2026-10-29

- **Particle Emitter on animated geometry steps on frame by frame**
  (Dev1): on animated geometry (or an animated mask), particles are born
  where the geometry is at each frame, so a moving object leaves a trail
  (before, every particle jumped onto the geometry's current place), and
  playing or scrubbing forward steps the simulation on a frame instead of
  re-simulating from the start. Editing the geometry re-simulates only
  from the first frame it changes.

## 0.9.63 — 2026-10-29

- **Calls through an attached GUI share its evaluator** (Dev2): MCP /
  Python calls to a running window now run off its UI thread and share
  the viewer's evaluator, so particles and other reads reuse what the
  window has already rendered and simulated (a later frame steps on
  rather than re-simulating). Calls run in order, each seeing the last
  one's edits; one that would overwrite an edit made in the window
  meanwhile is refused. Renders still use their own copy.

## 0.9.62 — 2026-10-29

- **Particle Render sprites can play an image sequence** (Dev1): set
  Texture frames to sequence and the texture input is taken at frames
  Sequence first to Sequence last; each sprite shows the image its age
  picks: by age (once over its life), cycle, play (from the first at
  Rate images a frame, holding the last) or loop, optionally from a
  random start per particle. Each frame of the sequence is rendered once
  and kept until it changes (up to 1000 frames). Current frame (the
  default) works as before.

## 0.9.61 — 2026-10-29

- **The 3D view's motion blur preview includes a looked-through camera's
  own motion** (Dev2): looking through a moving camera or light, the
  settled preview blurs by its motion across the shutter, as Scanline
  Render does.

## 0.9.60 — 2026-10-29

- **Scanline Render: Jitter shutter** (Dev1): on by default. Off, every
  pixel renders at the same shutter times, as before 0.9.55: a fast move
  shows as sharp copies rather than grain, exactly the render 0.9.48–0.9.54
  gave. The 3D view's blurred preview follows it.

## 0.9.59 — 2026-10-29

- **Particle dots streak in the 3D view's motion blur preview** (Dev2):
  viewing a Scanline Render with Motion blur, once the view settles its
  particle dots streak across the shutter, curving with their
  acceleration as in the render.

## 0.9.58 — 2026-10-29

- **Fixed: a comp wired into a second Viewer showed black or a stale
  picture** (Dev1): in a project with more than one Viewer node the
  viewer always showed the first one, so a new Viewer (even
  double-clicked) never showed and edits to its Merge seemed to do
  nothing. The viewer now shows the Viewer last wired into,
  double-clicked or clicked; the graph marks it with an eye and the
  others read "Viewer (not shown)". One-Viewer projects are unchanged.

## 0.9.57 — 2026-10-29

- **The 3D view shows a Scanline Render's motion blur** (Dev2): once the
  view stops moving, its cards and meshes (Lit or Solid) smear across
  the render's shutter (up to 8 samples, jittered); while you orbit it
  stays sharp. Particles (drawn as dots) and a looked-through camera's
  own motion aren't blurred.

## 0.9.56 — 2026-10-29

- **Motion-blurred particles curve** (Dev2): under gravity and other
  forces their streaks follow the path instead of the tangent (each
  particle keeps its acceleration), and their motion vectors include it.
  Constant-velocity particles are unchanged.

## 0.9.55 — 2026-10-29

- **Motion blur jitters each pixel's times** (Dev2): Scanline Render
  shifts each pixel's times within the shutter in a fixed dither
  pattern (so renders repeat exactly), so low Motion blur counts on very
  fast moves give fine grain instead of a few sharp copies, at no extra
  cost. Blur off is unchanged; blurred renders get the same average with
  a different grain than 0.9.48–0.9.54.

## 0.9.54 — 2026-10-29

- **View preferences are remembered between runs** (Dev1): View ▸ Show
  frame size, and the 3D view's Solid / Lit, wireframe and Lit shadows
  (for projects that haven't saved their own), in
  `~/.local/state/arclight/view.json` (`$XDG_STATE_HOME`), apart from
  projects and settings.json.

## 0.9.53 — 2026-10-29

- **Read Geo Opacity** (Dev1): fades the whole file (each material's
  own opacity times it, its shadows too), with or without Override
  material; animatable. Default 1, so existing projects are unchanged.

## 0.9.52 — 2026-10-29

- **Fixed: the 3D view of a Scanline Render with scenes only on its
  extra inputs** (Dev2): one with nothing on its scene and camera inputs
  but, say, a light on an extra input said it had no 3D scene; it now
  shows.

## 0.9.51 — 2026-10-29

- **Fixed: a new 3D Camera Setup rendered black** with a default Card
  (3DWorker): its Camera sat at the origin, in the card's own plane. The
  setup's Camera now starts back along +Z, so a new Card just fills its
  frame. (A project made before this keeps its camera at 0: move it
  back, Translate z ≈ 1, or insert the setup again.)
- **The 3D view shows a Scanline Render's extra inputs** (3DWorker):
  lights, cards and particles on its growable inputs now get their
  icons, handles and look-through.
- **Look through in 3D view** (3DWorker): a button in a Camera's (and a
  Spot or Area Light's) Properties switches the viewer to 3D looking
  through it; click again to come back out.

## 0.9.50 — 2026-10-29

- **Cards can take light** (MathWiz): the Card node's new **Shading**
  (unlit by default, so existing projects look the same; lambert or
  blinn) lights it by the scene's lights from either side, with
  Specular, Roughness and Emission as on the mesh nodes, and shadows
  fall on it unless **Receives shadows** is off. Light passes, shadows,
  the 3D view's Lit preview and `scene_objects` include it.

## 0.9.49 — 2026-10-29

- **Node browser on Shift+Tab** (MathWiz): over the node graph,
  **Shift+Tab** (not Tab) opens the node browser at the pointer, so Tab
  only switches the viewer between 2D and 3D; plain Tab over the graph
  does nothing.

## 0.9.48 — 2026-10-29

- **Scanline Render motion blur** (3DWorker): **Motion blur** (samples
  across the shutter; 0 = off, the default), **Shutter** and **Shutter
  offset** average renders with cameras, objects (deforming vertices
  too) and lights in between their places at the surrounding frames,
  and particles moved along their velocities, so fast ones streak;
  depth / P / N / uv / motion / id stay the frame's own. Particles now
  have motion vectors in the motion pass (sprites, points and objects).
  8 samples cost about 8× a plain render.
- **3D view Solid / Lit** (MathWiz): Lit lights meshes by the scene's
  lights and materials as Scanline Render does (optionally with
  shadows, drawn once the view settles); Solid lights them from the
  view. Lit is the default when the scene has lights. **⇧Z**, the 3D
  toolbar or View ▸ Lit shading.
- **3D Camera Setup goes just before the Viewer** (MathWiz), so its
  render shows straight away with the Viewer's picture as background;
  with no Viewer it brings one. From a wire's menu it still goes into
  that wire.

## 0.9.47 — 2026-10-29

- Card has an **Opacity** (0–1, animatable): a faded card blends with
  what's behind it, nearest first, and in the 3D view. Shadows are as
  strong as their caster is opaque (opacity × alpha, also for
  see-through meshes and soft-alpha textures), and depth / P / N / uv /
  motion / id passes come from the nearest surface at least half opaque
  (3DWorker).
- The 2D viewer labels the frame's corners with its size, Nuke-style
  (0,0 under the bottom-left corner, W,H over the top-right, from the
  image's format); **View ▸ Show frame size** turns it off. The About
  logo shows square and undistorted (it was being squeezed
  horizontally) (MathWiz).

## 0.9.46 — 2026-10-29

- In the 3D view, particle emitters (their shape and direction),
  attractors, vortices and bounce planes are drawn, picked and moved
  with G / R / S and the gizmos (forces move only); in the 2D viewer, an
  emitter feeding a viewed Particles 2D shows its box or point handles.
  Completes **M13, particles** (0.9.36–0.9.46).

## 0.9.45 — 2026-10-29

- MCP / Python: new **particles** tool (`Node.particles(frame, offset,
  limit)`) returns the particles a particle node (or a Scene / Scanline
  Render) passes on at a frame: ids, positions, velocities, ages, lives,
  sizes, colors and source stream, paged (limit default 1000, at most
  100000). README and help describe every particle node, Scanline
  Render's growable inputs and the auto-wire into a selected Scanline
  Render (M13).

## 0.9.44 — 2026-10-29

- **Particle nodes in the graph:** Particle Emitter, Gravity, Wind,
  Turbulence, Attractor, Vortex, Bounce, Kill, Speed Limit, Particle
  Render and Particles 2D. Streams plug into Scanline Render's extra
  inputs (a new empty one appears as you connect; a new Emitter goes
  straight into a selected Scanline Render), render as sprites hidden
  behind nearer geometry or as object instances, and show as colored
  dots in the 3D view; animated params are followed at every frame
  (M13).

## 0.9.43 — 2026-10-29

- MCP / Python: new **scene_objects** tool (`Node.scene_objects(frame)`)
  lists what a 3D node's scene holds: cameras, cards and meshes with
  their node and world position, and lights with type, position,
  direction, color, intensity, decay, enabled, shadows and their type's
  settings. `create_node` makes every light and Light 2D. Completes
  **M12, lights** (0.9.35–0.9.43).

## 0.9.42 — 2026-10-29

- New **Light 2D** node (Color category): spot, area, point or
  directional light on a flat image, with color, intensity, exposure and
  add / screen / multiply. An optional **normals** input (an N pass or a
  normal map) relights for real, with height, elevation, specular and
  roughness, plus an optional **depth** input; the mask / mix options
  apply. Viewer handles move, aim (⇧ snaps to 15°), open the cone,
  soften, and size areas, all animatable. Properties shows only the
  chosen type's settings (M12 step c).

## 0.9.41 — 2026-10-29

- Particle node definitions and lazy particle streams
  (`src/particle_nodes.rs`): Emitter, Gravity, Wind, Turbulence,
  Attractor, Vortex, Bounce, Kill, Speed Limit, Particle Render and
  Particles 2D, simulated only where drawn; not in the node menu yet
  (M13).

## 0.9.40 — 2026-10-29

- **Shadows:** lights gain **Cast shadows** (off by default, so old
  projects render unchanged) and **Shadow softness**. Spot lights use a
  perspective shadow map, directional lights a map fitted to the scene,
  point lights a cube map; an area light casts from each of its sample
  points, so its shadows are naturally soft. A point or spot light's
  Radius adds a penumbra that grows with distance from the blocker.
  Objects gain **Casts shadows** / **Receives shadows**. Scanline Render
  gains a **shadow** pass (beauty + shadow = no shadows) (M12 step d).

## 0.9.39 — 2026-10-29

- Particle rendering engine (`src/particles_render.rs`): round or square
  sprites, textured by an image or a sequence by age, opacity over
  life, over or additive, through a camera with depth (hidden behind
  nearer geometry) or straight onto frame pixels in 2D; per-particle
  object instances (oriented by velocity or spin) for the 3D renderer.
  No nodes yet (M13 steps c–d, engine).

## 0.9.38 — 2026-10-29

- Light 2D engine (`src/light2d.rs`): spot, area (rect / ellipse),
  point and directional lights on a flat image, as a shaped glow or,
  with a normal pass / normal map (and depth), shaded like 3D lights
  (Lambert, Blinn highlight); add, screen or multiply, with color,
  intensity, exposure and a mask. No node yet (M12 step c, engine).

## 0.9.37 — 2026-10-29

- 3D view: a selected Spot Light shows four handles on its cone's edge
  (drag to change the Cone angle) and a selected Area Light a handle on
  each edge (drag to resize it about its middle); they key animated
  params, are one undo step, and refuse linked params with a message.
  **0** (and **View ▸ Look through selected**) looks through the
  selected camera, spot light or area light, else the scene's camera;
  G / R / the gizmo then aim the light from its own view (M12 step b).

## 0.9.36 — 2026-10-29

- Particles engine (`src/particles.rs`): emitters (point, line, rect,
  disk, sphere, box, mesh surface / vertices, image alpha) with rate /
  burst, velocity cone, lifetime, size / color over life and seeds;
  gravity, wind / drag, curl-noise turbulence, attractors, vortices,
  bounce, kill and speed limit; substeps, pre-roll and a per-frame cache
  that re-simulates only from a change; animated params sampled per
  frame. No nodes yet (M13 steps a–b, engine side).

## 0.9.35 — 2026-10-29

- New **Area Light** node: a rectangle or disk, Size across, optionally
  Two-sided, sampled over a Quality × Quality grid (default 4×4) for
  soft light and broad highlights. Spot lights gain a **Falloff**
  exponent; point and spot lights gain **Radius** (a soft size). Every
  light gains **Exposure** (stops) and **Enabled**. Directional, point,
  spot and area lights get an optional **look at** input: wire a
  Locator, Card or Camera in and the light aims at it. The 3D view draws
  disk area lights, two-sided arrows and a point light's radius ring
  (M12 step a).

## 0.9.34 — 2026-10-29

- Node graph keys with the pointer over the graph: **Tab** opens the node
  browser there (into the wire under the pointer), **Shift+R / Shift+T /
  Shift+M** add a Media In / Transform / Merge at the pointer, wired like
  a browser placement.

## 0.9.33 — 2026-10-29

- Merge has a **Swap A / B** toggle (⇄ beside Operation) that trades its
  inputs for every operation: B over A, in A's frame (3DWorker).
- Every node with a mask input (Merge, Grade, Blur, Transform, Keyer, …)
  gains **Mask channel** (alpha, red, green, blue or luminance) and
  **Invert mask**; mask inputs are drawn violet and labelled in the node
  graph. Old comps are unchanged (3DWorker).
- The playback controls sit centered under the timeline (wrapping from
  the left when the panel is too narrow), and the Node Graph has a
  **Fit** button in its top-right corner that frames every node and
  backdrop, like F (MathWiz).

## 0.9.32 — 2026-10-29

- **Read plays video files** as well as stills (it's now labelled
  "Read"), and both Read and Media In read upper-case extensions such
  as a camera's `CLIP.MP4`; the file browsers list them (the portal's
  filters are case-sensitive, so each filter now adds the upper-case
  form). Match media works on a video Read.
- Video decoding reads only the first video stream (camera files with
  audio and data streams work), decodes a frame ahead, and reads on
  through frames that playback skips instead of seeking, so a 4K clip
  plays at about 15 fps fitted to HD (22–24 once cached).

## 0.9.31 — 2026-10-29

- Fixed: the app stopped responding while a file dialog was open
  (Hyprland and other compositors flagged it as Not Responding). File
  dialogs now run beside the app (`src/gui/dialogs.rs`), which keeps
  drawing ("Choosing a file…" in the status bar), and the
  unsaved-changes questions are shown inside the window.

## 0.9.30 — 2026-10-29

- Wires can be selected (click; Delete disconnects) and right-clicked
  for **Insert 3D Camera Setup**, **Insert Node…** or **Delete
  connection**; **Edit ▸ Add 3D Camera Setup** adds a wired Camera,
  Scene and Scanline Render in a "3D Setup" backdrop (into the selected
  wire, after the selected node, or on its own). The first one in a
  project switches the viewer to 3D with a hint ("Tab switches 2D /
  3D"), and the toolbar's **3D ●** shows whenever the viewed node has a
  scene.
- The node browser (right-click) is bigger (340 wide, up to 560 tall)
  and shows a line of help for each node.

## 0.9.29 — 2026-10-29

- Read and Media In **fit what they read to the project's format**
  (**Fit to project**: fit, fill, stretch, width, height or none),
  following it when it changes; new nodes fit by default, older projects
  keep their media at its own size. Edges stay solid when a plate fills
  the frame. Help: a Project settings section.
- Note: tracks and camera solves are in the fitted image's pixels; a
  fit with a different aspect (bars or crop) needs Solve focal on, or
  the project format matched to the media (Project Settings ▸ Match
  media).

## 0.9.28 — 2026-10-29

- **File ▸ Project Settings…** (⇧⌘P / Ctrl+Shift+P): set the project's
  frame size, frame rate and frame range from presets (HD, UHD, 2K / 4K
  DCI, 720p, 2K Full Aperture, PAL, NTSC, square, vertical; 23.976 to
  60 fps) or by hand, or **Match media** to the selected Media In or
  Read; Apply is one undo step.

## 0.9.27 — 2026-10-29

- Motion vectors: Scanline Render's **motion** pass (`motion.u` /
  `motion.v`, pixels to the next frame) from the camera's move and the
  objects' animation (deforming meshes too), evaluating the scene a
  frame later; shown as color in the viewer. Completes **M10, 3D
  objects and render passes** (0.9.12–0.9.27).
- Help: the 3D view section describes transforming several objects.

## 0.9.26 — 2026-10-29

- 3D view: with several objects selected, G / R / S and the gizmos
  move, rotate and scale them together about their median point (the
  gizmo sits there), as Blender's Median Point pivot does; objects whose
  params are linked are left out (or turn in place), and the whole
  gesture is one undo step (M9 leftovers).

## 0.9.25 — 2026-10-29

- Lit rendering: Lambert and Blinn shading (with specular, roughness
  and emission) on Cube, Sphere, Cylinder, Plane and Read Geo (which
  keeps its file's materials unless **Override material** is on), lit by
  the scene's lights; new passes `diffuse`, `specular`, `emission`,
  `ambient` (adding up to the picture) and `light.<node>` per light.
  Scenes without lights render exactly as before (M10 step d).

## 0.9.24 — 2026-10-29

- 3D view: lights show as icons (a star for point lights, a cone with
  its penumbra for spots, parallel arrows for directional lights, a ring
  for ambient light) and can be picked, box-selected, framed and moved
  with the gizmos and G / R / S like other objects (M10 step d).

## 0.9.23 — 2026-10-29

- **Ambient, Directional, Point** and **Spot Light** nodes (color,
  intensity, decay, cone and penumbra) go into scenes (`Scene.lights`);
  rendering with them comes next (M10 step d).

## 0.9.22 — 2026-10-29

- Scanline Render's **Passes** checkboxes choose depth, P, N, uv, id and
  mattes (older projects render depth only, as before); **Holdout** on
  Cube, Sphere, Cylinder, Plane and Read Geo; Read Geo's file browser
  shows OBJ and glTF files; Shuffle's ▾ lists offer the layers its input
  actually has. Completes M10 steps (a) and (b).

## 0.9.21 — 2026-10-29

- Render passes, engine side: Scanline Render can write `P`, `N`, `uv`,
  object `id` and a `matte.<node>` per object (nearest sample for
  depth / P / N / uv / id, coverage for mattes, which add up to alpha),
  and holdout objects cut out what is behind them. Over a background,
  the background's other planes are kept. The viewer shows one button
  per pass layer, vectors as color. Not yet a Scanline Render setting
  (M10 step b).

## 0.9.20 — 2026-10-29

- **Shuffle** node: brings a render pass into the color (to grade, blur
  or key it), takes channels from any pass, writes the color back into a
  pass, and removes passes. Pass names are typed (a new text param kind,
  applied on Enter) or picked from a ▾ list.
- Merge: **zmerge** combines two renders by depth, the nearer in front.
- Image stats (MCP / Python) list the passes as layers.
- Memory use counts 3D meshes (M10 steps b, e).

## 0.9.19 — 2026-10-29

- 3D objects: **Cube, Sphere, Cylinder** and **Plane** nodes (with a
  texture input and a color) and **Read Geo** (OBJ with MTL, glTF
  .gltf / .glb), rendered by Scanline Render with the cards. The 3D
  viewport shows meshes solid (lit from the camera) or as wireframes
  (**Z**), and picks, frames and moves them like cards (M10 step a).

## 0.9.18 — 2026-10-29

- Depth merge, engine side (`Image::zmerge`): combines two images pixel
  by pixel by their depth passes, the nearer over the farther with soft
  edges blended, and carries the other passes along (mattes and color
  passes blended, position / normal / id taken from the nearer). The
  Merge option comes next (M10 step e).

## 0.9.17 — 2026-10-29

- glTF 2.0 import, engine side (`gltf::read_gltf`): .gltf and .glb, node
  transforms applied, triangles / strips / fans with normals and UVs,
  PBR materials mapped to ours (base color and texture, roughness,
  metallic as the highlight color, emission with strength, opacity for
  blended materials, unlit), external and embedded textures. Sample:
  `examples/geo/crate.gltf`. Not yet a node. New dependency: `gltf` 1.4
  (MIT / Apache); needs Rust 1.88+ (M10 step e).

## 0.9.16 — 2026-10-29

- Shading, engine side (`src/shade.rs`): ambient, directional, point,
  spot (cone and soft penumbra) and area lights with distance falloff;
  Lambert and Blinn–Phong shading of a point, split into diffuse,
  specular, emission and ambient parts that add up to the beauty (the
  lighting passes), per light too. Without lights a surface shows its
  color as it is. Not yet used by Scanline Render (M10 step d).

## 0.9.15 — 2026-10-29

- Render passes in EXR: every plane is written under the standard
  `layer.channel` name (`depth.Z`, `P.x`, `N.x`, `motion.u`,
  `matte.<node>.A`, …) and read back by name (a bare `Z` still reads as
  depth). The engine side of Shuffle: copy any channel or pass into R,
  G, B, A or a named plane, and remove planes (M10 step b).

## 0.9.14 — 2026-10-29

- Wavefront OBJ import, engine side (`mesh::read_obj`): objects and
  groups per material, polygons, negative indices, normals computed when
  missing, and MTL materials (diffuse, specular, emission, opacity,
  shininess, texture path). Sample: `examples/geo/crate.obj`. Not yet a
  node (Read Geo comes next) (M10 step c).

## 0.9.13 — 2026-10-29

- Scanline Render draws everything as triangles (a Card is two), binned
  by pixel row and swept with an active list, ready for meshes; cards
  render exactly as before (bit-identical). An 80,000-triangle plane at
  1920×1080, 3×3 samples renders in about 46 ms (M10 step a).

## 0.9.12 — 2026-10-29

- 3D meshes, engine side (`src/mesh.rs`): triangle meshes with normals
  and texture coordinates, materials (unlit, Lambert, Blinn; color,
  opacity, texture, specular, roughness, emission), and Cube, Sphere,
  Cylinder and Plane generators with sizes and segment counts. Not yet
  in the node graph or the renderer (M10 step a).

## 0.9.11 — 2026-10-29

- Camera Tracker: **Solve** setting Auto / Free move / Tripod, and
  **Tripod distance**. Auto solves a camera that only turns (a pan or
  tilt on a tripod) as a tripod shot: the camera keyed on rotation only,
  its points on a sphere at the distance (setting the scale only resizes
  the sphere). Free move refuses such a shot and suggests Tripod. MCP
  `track_camera` / Python `Node.track_camera` take `mode` and
  `distance`; the result and `camera_points` report the mode and PnP
  frames. Completes M11 (PnP and the tripod solve).

## 0.9.10 — 2026-10-29

- Camera Tracker: frames the neighbouring camera can't explain (after a
  jump, a gap or a cut) are placed from the points they see (PnP), and
  the solve restarts past unusable frames instead of stopping; the panel
  has a collapsible **Frames** list with each frame's error, marking PnP
  and unsolved frames. A refused tripod shot returns a typed `NoParallax`
  error (M11 step b).

## 0.9.9 — 2026-10-29

- Camera solving: perspective-n-point (`solve::pnp`): a camera from the
  3D points it sees, by P3P (Grunert) in adaptive RANSAC, EPnP (flat
  scenes too) and a least-squares refinement on the inliers; the
  groundwork for bridging jumps and gaps in the camera solve (M11 step a).

## 0.9.8 — 2026-10-29

- Tripod (nodal) solve, engine side (`src/nodal.rs`): detects a camera
  that only turns (homographies between frames that are pure rotations,
  `K⁻¹ H K`) and solves it: a rotation per frame about a fixed center,
  the focal length optionally, and points on a sphere at a chosen
  distance. Not yet wired into the Camera Tracker (M11 step e).

## 0.9.7 — 2026-10-29

- Help → Arclight Help… (or F1) opens a help window: scrollable,
  resizable, Esc or its close button closes it. It covers the node graph,
  selecting, the viewer, player, keyframes, curve editor, the 3D view
  (navigation with mouse and trackpad, the views and pie, selection,
  G / R / S and the gizmos) and the Camera Tracker. The text is
  `docs/help.md`, built into the app (rendered with egui_commonmark, MIT
  / Apache); the Help menu no longer lists shortcuts inline.

## 0.9.6 — 2026-10-29

- 3D viewport: ` (backtick) opens a pie of views around the pointer
  (Left, Right, Top, Bottom, Front, Back, Camera, Persp / Ortho), as in
  Blender: point and click, or let go of the key; Esc cancels. A Camera
  Tracker's point cloud is colored by each point's reprojection error
  (green under 0.25 px, yellow about 1 px, red from 2 px); solves keep a
  per-point error (re-track older solves to get it).
- README: a "3D view" section with the navigation, selection and
  transform controls.

## 0.9.5 — 2026-10-29

- The 3D view is live: the viewer's 3D mode (2D / 3D toggle, or Tab over
  the viewer) shows the viewed node's scene in the 3D viewport, and says
  so when it has none.
- In the 3D view, G / R / S move, rotate and scale the selected object
  Blender-style: X / Y / Z to constrain (twice: local), Shift to exclude
  an axis, type a value (- negates), Ctrl snaps, Enter or click to
  confirm, Esc or right-click to cancel; Alt + G / R / S reset. Animated
  params get a key; one undo step per gesture; linked params say why they
  can't move.
- The viewport camera is saved with the project.
- 3D view gizmos, switched in the viewer toolbar (Select, Move, Rotate,
  Scale, All; Global or Local axes): arrows, plane squares and a center
  to move, axis rings, a view ring and a trackball to rotate, cubes and a
  center to scale; drags key animated params, are one undo step each, and
  snap with Ctrl; linked params show the gizmo gray and say why.

## 0.9.4 — 2026-10-29

- 3D viewport selection, Blender-style: left-click picks a card, camera,
  locator or tracker point (Shift-click adds or removes), left-drag on
  empty space box-selects (object centers and points; Shift adds), A
  selects everything, Alt+A or Esc deselects (Esc first cancels a box).
  Objects select their nodes, so the node graph and Properties follow
  (and the viewport highlights what the graph selects); F / . frame the
  selection. Right-click a Camera Tracker's selected points to Place
  Card here or Place locator, as in the 2D viewer.

## 0.9.3 — 2026-10-29

- 3D viewport (in progress): the viewer's 3D mode will show the viewed
  scene from a free camera with Blender-style navigation (middle-drag
  orbit, Shift pan, scroll / Ctrl+middle zoom toward the pointer;
  trackpad two-finger orbit, Shift pan, pinch / Ctrl zoom; optional
  emulate-3-button Alt+left-drag), views on 1/3/7 (Ctrl: opposite), 5
  perspective/ortho, 0 look through the scene camera (its film frame,
  outside darkened), 2/4/6/8 orbit 15°, . / F frame, Home frame all, and
  a navigation gizmo (axis ball; pan, zoom, camera, ortho buttons). It
  draws the ground grid and axes, cards textured by the CPU renderer (on
  a worker, at half or quarter resolution while the view moves), card
  outlines, camera frusta and animated cameras' paths, point clouds and
  locators. Scene objects now carry their node's name. (Shown in the
  viewer once it's wired into the 2D / 3D toggle.)

## 0.9.2 — 2026-10-29

- Groundwork for moving, rotating and scaling in the 3D view (no UI
  yet): the gizmo and G / R / S maths: axis, plane and free moves at the
  object's depth, ring and trackball rotation, scaling, X / Y / Z
  constraints (global, local, Shift to exclude), typed values, snapping,
  cancel and reset.

## 0.9.1 — 2026-10-29

- Viewer: a 2D / 3D toggle in the toolbar (Tab over the viewer switches,
  never while a text field has focus); in 3D a node without a scene says
  so. The 3D view itself is a placeholder until the viewport lands.

## 0.9.0 — 2026-10-29

- Phase 9 begins: work on the 3D viewport (M9 of `docs/phase-4-plan.md`)
  moves to branch `wip-ns-3dwork`, with Blender-style controls (the
  spec's navigation, selection and G / R / S transform keys now follow
  Blender's defaults). No code change yet.

## 0.8.4 — 2026-10-29

- Intel Macs: the macOS app is now universal (Apple Silicon and Intel in
  one app, joined with lipo; macOS 11+), released as
  `Arclight-<version>-universal.dmg`, and the macOS Python wheel is
  universal2. `ARCHS=arm64` (or `x86_64`) builds one architecture only.

## 0.8.3 — 2026-10-29

- Node graph multi-selection and backdrops: Shift/Cmd-click, box select
  (Shift adds), Cmd+A and Esc; drag to move the selection, Delete deletes
  it. Cmd+G (or right-click a node, Group) puts a titled, colored box
  behind the selected nodes: drag its title to move it with the nodes
  fully inside it, a corner to resize, double-click to rename, the swatch
  or the Properties panel for color and title size; saved in the project
  (`backdrops`), undoable; Shift+Cmd+G or Delete removes the box and
  keeps the nodes. Panning the graph is now middle-drag or Alt-drag (a
  plain drag on empty space selects).

## 0.8.2 — 2026-10-29

- Adding a Camera in the node editor now brings a Scene and a Scanline
  Render, wired (Camera → Scene → Scanline Render) and placed like a
  single node: into the highlighted wire (its source becomes the
  background), or after the selected node (which feeds the background,
  and what it fed now comes from the render), or on its own; one undo
  step, the Camera selected. Hold Shift to add the Camera alone.

## 0.8.1 — 2026-10-29

- Version numbering moves to 0.8.x: this is 0.0.36 renumbered, with no
  other change. Later versions go 0.8.2, 0.8.3, …

## 0.0.36 — 2026-10-29

- Camera Tracker node (3D): Track & Solve finds and follows features over
  the project's frame range and solves the camera's path and a cloud of
  3D points (focal length given, or solved), with progress and Stop; the
  result (error per frame, points, focal) stays on the node and saves
  with the project. Export adds a Camera keyed on every frame, a Point
  Cloud, a Scene and a Scanline Render over the plate. Phase 4
  milestone 8.
- Point placement: with a solved Camera Tracker selected, the viewer
  shows its points; select them (click, Shift-click, drag a box) to Place
  Card here (on the plane through 3+ points, else facing the camera,
  sized to them) or Place locator, or to Set origin, Ground plane (level
  at 0) and Set scale (two points a given distance apart).
- New Point Cloud and Locator nodes (3D). MCP tools `track_camera`,
  `camera_points`, `orient_camera`, `export_camera`, `place_on_points`,
  and the same as Python `Node` methods.

## 0.0.35 — 2026-10-29

- Help → About is laid out evenly: all text centered, equal margins left
  and right, and equal space above the logo and below OK.

## 0.0.34 — 2026-10-29

- Camera solving (library, no node yet): from feature tracks, a camera
  for every frame and a cloud of 3D points: start from the frame pair
  with the most parallax, add frames one at a time, triangulate new
  points, and refine everything with bundle adjustment
  (Levenberg–Marquardt with a robust loss and the Schur complement),
  optionally solving the focal length; outlier tracks are dropped. Shots
  where the camera only turns are refused with a clear message. End to
  end on a rendered test shot (tracked with the feature tracker), the
  camera path comes out within 1 % and 0.1° of the truth. Phase 4
  milestone 7.

## 0.0.33 — 2026-10-29

- Camera-tracker groundwork: from tracked features in two frames, the
  camera's motion between them and the 3D points (eight-point essential
  matrix in RANSAC, the right one of its four decompositions, linear
  triangulation), and a search for the frame pair with enough parallax
  to start a solve from. Library only, no node yet. Phase 4 milestone 6.

## 0.0.32 — 2026-10-29

- Camera-tracker features no longer drift: each tracked feature is
  refined against the frame it was found on with an affine window warp,
  so content that stretches as the camera pans or pushes in (which a
  shift-only window follows slightly off, adding up frame after frame)
  stays locked: 0.015 px median error over a rendered test shot, down
  from 0.22 px.

## 0.0.31 — 2026-10-29

- Groundwork for the 3D camera tracker (no node yet): `src/features.rs`
  finds corners (Shi–Tomasi) and follows them from frame to frame
  (pyramidal Lucas–Kanade with a forward–backward check), topping up
  features as they're lost. Tested on exact shifts (sub-0.02 px median,
  shifts up to ~22 px) and on a camera move rendered with Scanline Render.
  Phase 4 milestone 5, part 1.

## 0.0.30 — 2026-10-29

- Python wheels: the `arclt` module is packaged with maturin as one abi3
  wheel per platform (CPython 3.10+) for Linux x86_64 (manylinux2014) and
  macOS arm64, plus an sdist; the Python wheels workflow builds and tests
  them and attaches them to release tags.

## 0.0.29 — 2026-10-29

- Scanline Render node (3D): draws a scene's cards through a camera (the
  camera input's, else the scene's first) into an image, over an optional
  background (else Width × Height, 0 = the project format).
  Perspective-correct textures, cards sorted by depth and composited front
  to back (transparent parts of a card show what's behind), near-plane
  clipping, anti-aliasing (samples across each pixel, default 3 = nine),
  tracker points as dots, and a depth plane (distance in front of the
  camera, 0 where empty; the background's depth shows through). Viewing a
  Camera/Card/Scene node now says what the scene holds instead of showing
  an empty frame. Phase 4 milestone 4.

## 0.0.28 — 2026-10-29

- A link or formula that gives one number for a list param (a position,
  size or color) now fills every component, e.g. a color formula of 0.5
  is mid grey.

## 0.0.27 — 2026-10-29

- 3D scene nodes (new 3D category): Camera (focal length, film back,
  translate/rotate/scale, near/far), Card (puts its image on a flat card
  in 3D, 1 wide by default keeping the image's aspect) and Scene (gathers
  up to four cameras/cards/scenes). Scene wires are typed: the node editor
  draws them blue and refuses a scene into an image input or an image
  into a scene input; the session/MCP and the graph refuse them too, and
  node descriptions list each input's kind. MCP `list_ops` also filters
  by `keyer` and `scene`. Nothing draws a scene yet (Scanline Render is
  next). Phase 4 milestone 3.

## 0.0.26 — 2026-10-29

- The formula field suggests functions, variables, node names, params and
  components as you type (Up/Down, Tab or click), and its ? lists
  everything a formula can use.

## 0.0.25 — 2026-10-29

- Formulas can use `time` (seconds from the project's first frame,
  `(frame - first_frame) / fps`) and `fps`, e.g. `5 * noise(time * 2)`
  wobbles twice a second, `at(x, frame - fps)` reads x a second earlier.

## 0.0.24 — 2026-10-29

- Lens Distort node (Transform): removes or adds radial lens distortion
  (Brown model, K1/K2 about a center, r in half frame widths); undistort
  straightens a plate's lines, distort bends CG to match; keeps overscan,
  carries planes, has mask/mix. Under it, `src/geom.rs`: 3D placement
  (translate/rotate/scale, X-Y-Z degrees), a pinhole camera (focal, film
  back, project/unproject to frame pixels), lens distortion and its
  inverse. Adds the nalgebra dependency. Phase 4 milestone 2.

## 0.0.23 — 2026-10-29

- Images can carry extra named planes beside RGBA (a 3D render's depth,
  to come): EXR reads and writes them (`Z` as `depth`); Transform, Corner
  Pin, Tracker output, Crop, Resize and motion blur move them with the
  pixels (nearest, not blended); color ops, Keyer, Blur and shapes keep
  them; Merge takes the front input's where it is over half opaque; the
  Viewer lists an image's planes beside R G B A and shows one as gray
  (the readout adds its value). First step of the Phase 4 plan
  (`docs/phase-4-plan.md`).

## 0.0.22 — 2026-10-29

- Formula links: `{"link": {"expr": "..."}}` works a param out every
  frame from other nodes' params (`node.param`, `.x`/`.y` or `[i]`),
  `frame`, arithmetic and comparisons per component,
  sin/cos/…/clamp/mix/smoothstep, `if()`, seeded noise/random/wiggle and
  `at(x, frame)`. Right-click a param name for Use a formula; errors show
  in red and fail the render naming the param; renames rewrite formulas.

## 0.0.21 — 2026-10-29

- Help → About Arclight Compositor… opens a dialog with the logo, the
  product name, Created by: Nelson Soler, the version and the build
  date/time (UTC), and an OK button.

## 0.0.20 — 2026-10-29

- Maths on links: a linked value can be multiplied and offset
  (`linked × multiply + add`, per component or one number for all): half
  a rotation, a track followed 10 px to the side, mirrored motion. The
  panel shows × and + fields under every linked param; scripts set
  `"multiply"` / `"add"` in the link.

## 0.0.19 — 2026-10-29

- Live links between nodes: a param can follow another node instead of
  holding a value, worked out at every frame (and between frames for
  motion blur). The Tracker's To Transform / To Corner Pin now make
  linked nodes by default (a "Linked" checkbox; off makes a keyed copy as
  before), so re-tracking moves everything that follows the Tracker. In
  the panel, linked params show their value, their source and an Unlink
  button. Links survive renaming their source; a deleted source is an
  error naming it. Scripts: `export_tracker` / `to_transform` /
  `to_corner_pin` take `linked` (default true), and `set_params` accepts
  `{"link": {...}}` values (see README).
- Releases: `packaging/linux/build-tarball.sh` packages a Linux build
  (`arclight-<ver>-linux-x86_64.tar.gz`: GUI, CLI, example clip, icons and
  a per-user `install.sh` / `uninstall.sh` under `~/.local`), and the macOS
  workflow attaches its DMG to the GitHub release of a `v*` tag. The GUI
  finds the starter clip in a release's `share/arclight`.

## 0.0.18 — 2026-10-29

- Video color: exported video now converts RGB to YUV with the BT.709
  matrix (ffmpeg's default was BT.601, so apps decoding HD as BT.709
  showed colors shifted: 200,100,50 came out about 209,105,45) and is
  tagged BT.709 primaries and matrix, sRGB transfer, video range, so
  players and editors show it as Arclight does. All four codecs.

## 0.0.17 — 2026-10-29

- DPX with alpha: a DPX alpha setting on Media Out and Write stores RGBA
  (alpha straight, as DPX readers expect) in any DPX encoding. Read by
  ImageMagick, OpenImageIO and ffmpeg.
- Float DPX: reading 32- and 64-bit float DPX (linear unless marked as
  video), and a new 32-bit float linear encoding that stores values
  exactly, highlights included. Matches OpenImageIO's float DPX (which
  reads it exactly); ImageMagick and ffmpeg don't read float DPX at all.

## 0.0.16 — 2026-10-29

- EXR data windows: reading places an EXR's pixels where its data window
  says, in a frame the size of its display window (they used to land at
  0,0 in a frame the size of the data). Writing stores only the image's
  data window, inside a display window of the frame, so a small layer
  makes a small file. Checked against oiiotool and exrheader.

## 0.0.15 — 2026-10-28

First numbered version (before this the crates said 0.1.0). Includes all
work up to and through session 7, most recently:

- Smoother downscaling: Transform, Tracker match-move/stabilize and Corner
  Pin average over each output pixel's source footprint when they shrink
  the image (up to 16×16 taps), instead of aliasing; enlarging is unchanged.
- Tracker panel: per-track pattern/search box sizes and offsets.
- Node graph opens at a comfortable size (fit zoom capped at 70%).
- New shapes land in the viewed picture; drag inside a Polygon/Roto to move
  it all, drag an edge to move its two points.
- Viewer toolbar color picker (R, G, B, A and a swatch).
- macOS (Apple Silicon) build and DMG packaging, and a GitHub Actions
  workflow for it.
- DPX read/write (log, 10/16-bit video); 10-bit video decode and encode
  (HEVC 10-bit, ProRes 422 HQ, ProRes 4444 with alpha); EXR/TIFF/Targa/WebP
  sequences, held missing frames, 16-bit TIFF output with tagged alpha.
- Keyer node (screen pick, shrink/grow, softness), motion blur on shapes,
  Transform, Corner Pin and the Tracker, Transform auto pivot, view
  override, app launcher entry and icon.

See `HANDOFF.md` ("Progress log") for the full history.
