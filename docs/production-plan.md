# Production plan: what a film pipeline needs

Status: **proposed** (2026-11-01, the Supervisor, at the user's request),
waiting for the user's answers to the questions at the end. When
approved, phase 15 is **0.15.x** on a new branch **`wip-ns-prod`**,
`main` following it, as in earlier phases. The work goes one milestone
at a time per owner; Dev1 and Dev2 run in parallel and never edit the
same file at once (see "Owners and files").

## Goal

Make ArclightVFX something a facility could put in a feature-film
pipeline: plates in the show's color space, tools shared across shots as
single nodes, 4K–8K EXR comps that stay interactive, and renders that go
to a farm and come back the same every time. Those four are the
blockers; without any one of them a studio can't adopt it, whatever
else it does.

**Out of scope here:** deep compositing, stereo / multi-view, a planar
tracker, editorial conform (OTIO / EDL), review tools (see "After phase
15"). OpenFX hosting and importing other applications' scripts stay
"Not planned".

## Where it is (checked in the code, 2026-11-01)

- **Color:** a fixed linear working space. 8- and 16-bit files are
  converted sRGB ↔ linear on read and write (`image.rs`); EXR is taken
  as linear; the viewer shows sRGB. No OpenColorIO, no ACES, no per-file
  input transform, no display / view choice.
- **Reuse:** templates (0.14.7) paste a set of nodes in, Import project
  and copy / paste merge graphs (`graph_merge.rs`). Nothing wraps nodes
  up as one node with its own controls.
- **Scale:** every node renders its whole frame (the data window, not a
  region of interest); results are cached in memory only; there is no
  proxy mode. The GPU (phase 12) makes most 2D nodes and Scanline Render
  fast, but a large graph of 8K EXRs still holds every frame in RAM.
- **Batch:** `arclight render graph.json --frame N` writes Write nodes
  at one frame; Media Out nodes write their whole range. `--cpu` gives
  exact, reproducible results. No frame-range or chunk options for Write,
  no machine-readable progress, exit codes not specified, no farm
  submitter.
- **EXR:** read and write with the `exr` crate; extra planes (depth, P,
  N, motion, …) travel with the image and show in the viewer. Multi-part
  files, a shuffle node over named layers, every compression type, and
  metadata / timecode carried through to the output aren't all there.

## Milestones

Four tracks, one per blocker, in the order they matter. Each milestone
lands with tests, docs (`docs/help.md`, README), a CHANGELOG entry and a
hand-test item in `TODO.md`.

### P1, color management (OpenColorIO)

Nothing else matters if the colors are wrong, so this is first.

- **P1a, the config.** Load an OCIO config: `$OCIO`, else a project
  setting, else a built-in one (ACES 2.x CG config, which ships with
  OCIO). Project Settings gains Color: the config, the working
  (scene-linear) space, the default display / view. Old projects open
  as today: a built-in "legacy" mode that reproduces the current sRGB ↔
  linear conversions bit for bit until the user picks a config.
- **P1b, read and write.** Read / Media In get an **Input space**
  (guessed from the file: EXR → scene-linear, 8/16-bit → sRGB, DPX →
  the config's log space if it has one; file rules from the config when
  present). Write / Media Out get an **Output space**. Converting happens
  at the file boundary only; inside the graph everything is in the
  working space.
- **P1c, the viewer.** A display / view menu in the viewer's toolbar
  (e.g. ACES 1.0 SDR-video on sRGB), applied on the GPU as a 3D LUT or
  shader from OCIO's GPU path, the CPU fallback matching it. The color
  picker shows working-space values (as now) and, on hover, the
  displayed ones.
- **P1d, nodes.** **OCIO Color Space** (from / to), **OCIO Display**
  (bake a view for delivery), **OCIO File Transform** (a .cube / .clf /
  .csp LUT), **OCIO CDL** (slope / offset / power / saturation, and
  reading .cdl / .ccc). All on the CPU and the GPU.
- **How:** the `opencolorio` C++ library through a small Rust binding
  (none we'd trust exists; write `src/ocio/` with a C shim, built with
  the `cc` crate, vendored like the other native deps), or, if the user
  prefers no C++ dependency, a Rust implementation of the subset the
  ACES config needs (matrices, log curves, the ACES output transforms,
  LUT files). See Q1.
- **Tests:** legacy mode bit-identical to 0.14.x on every example; an
  ACEScg plate through ACES → sRGB view matches OCIO's own `ociochecklut`
  output within 1e-4; LUT files round-trip; GPU vs CPU within the usual
  tolerance (`gpu --check` rows).

### P2, node groups (gizmos)

- **P2a, Group node.** Select nodes, Edit ▸ Group (⌘⇧G is taken by
  backdrops: use ⌥G): they become one **Group** node with inputs and
  outputs where wires crossed the boundary. Double-click (or Enter)
  opens it in the Node Graph as its own level, with a breadcrumb back
  out; Ungroup puts the nodes back. Saved inside the project, nested
  groups allowed. The evaluator flattens groups when it builds its plan,
  so caching and the GPU work unchanged.
- **P2b, exposed controls.** In a group, right-click any parameter ▸
  Expose: it appears on the Group's own Properties panel (renamable,
  with its range), linked to the inner node. Keyframes and expressions
  work on the exposed control.
- **P2c, gizmos.** Save a group as a **gizmo**: a file in the user's
  (or a studio's, `$ARCLIGHT_GIZMO_PATH`) gizmo folders, listed in the
  node browser like any node. A project stores a gizmo instance as a
  reference plus its exposed values (and a copy of the inside, so it
  still opens where the gizmo file isn't); "Update from gizmo" and
  "Make editable" (copy in) for when the file changes. Templates stay
  as they are (paste a set of nodes); gizmos are the reusable kind.
- **Tests:** group / ungroup round trip renders identically; exposed
  controls drive the inner node, animate, survive save / load and
  `graph_merge` renames; a gizmo used in two projects updates from its
  file; Python and MCP see groups (`create_group`, `expose`).

### P3, big plates (regions, a disk cache, proxies)

- **P3a, region of interest.** Nodes render only the part of their
  output that's asked for: the viewer asks for what's on screen, a
  Write for its frame. Each op says which input region it needs for an
  output region (a blur grows it by its radius, a transform maps it, a
  merge passes it on). Ops that don't say yet render whole frames, as
  now. Tiles on the CPU in parallel; the GPU paths render the region.
- **P3b, a disk cache.** Results that took long to make (over a
  threshold) are written to a disk cache (`$XDG_CACHE_HOME/arclight`,
  size-capped in Preferences, EXR with fast compression), keyed by the
  same node key the memory cache uses, so scrubbing back after the
  memory cache dropped a frame reads it instead of rendering again. The
  timeline marks frames cached on disk.
- **P3c, proxy mode.** A proxy switch (full, ½, ¼) in the viewer's
  toolbar: Reads load at that scale (or read a proxy file beside the
  plate when present: `plate.####.exr` → `proxy/plate.####.exr`, a
  project setting), and position / size params scale with it so the
  comp looks the same, only smaller. Writes and the farm always render
  full size.
- **Tests:** a region render equals the same crop of a full render, for
  every op that declares regions; a disk-cached frame equals the
  rendered one; a proxy render scaled up is within filtering of the full
  one; a benchmark (an 8K EXR comp, 50 nodes) with numbers before and
  after in the report.

### P4, the farm

- **P4a, the command line.** `arclight render graph.json --frames
  1001-1100 [--step N] [--chunk 1010-1019] [--write node,…]`: Write
  nodes over a range (sequence paths with ####), Media Out as now.
  Progress as one JSON line a frame on stdout with `--json` (frame, node,
  time, file), errors on stderr, documented exit codes (0 done, 1 a node
  failed, 2 bad arguments, 3 a file couldn't be read / written). Paths
  remapped with `--path-map old=new` (Windows / Linux / macOS farms).
- **P4b, reproducible.** The same project, frame and `--cpu` give
  bit-identical files on any machine (the CPU renderer is already
  deterministic; check threading and float sums, fix where they aren't).
  GPU renders match within the `gpu --check` tolerances, and the farm
  can force `--cpu`.
- **P4c, a submitter.** A Deadline plugin (Python, in `packaging/farm/`)
  that submits a project from the GUI (File ▸ Submit to farm…) or the
  command line, chunked; OpenCue if the user wants it too (Q4).
- **Tests:** range and chunk renders write the right files; exit codes;
  the JSON progress; a path map; two `--cpu` renders hash the same.

### P5, EXR for a pipeline

- **P5a, multi-part and layers.** Read multi-part and multi-layer EXRs
  (every part, every layer as named planes), write them (a Write's
  Channels: rgba, all, or chosen layers, as one part or a part each).
- **P5b, Shuffle.** A **Shuffle** node moving channels between layers
  (rgba ← a render pass, depth into alpha, …), with presets for the
  common AOV rebuilds.
- **P5c, compression and metadata.** Every compression the format has
  (zip, zips, piz, dwaa / dwab with a level, b44, rle, none), half or
  float per layer; metadata (timecode, camera, lens, frame rate, the
  OCIO space) read, carried through the graph and written out, with a
  **Metadata** node to view and set it.
- **Tests:** round trips through each compression; a multi-part file
  from another tool (Blender, Houdini samples in `tests/data/`) reads
  with every layer; timecode survives Read → Write.

## Alongside: comp tools

Smaller, self-contained, and what an artist misses first after the
blockers. They can fill gaps between the tracks above (one owner each,
in files the tracks don't touch).

- **STMap** (warp an image by a uv map; lens distortion handed between
  departments), and Lens Distort writing an STMap.
- **Defocus / ZDefocus** (bokeh by a depth plane, on the GPU).
- **Vector tools:** VectorBlur (motion blur from a motion pass) and a
  retime with optical flow (Kronos-like).
- **Denoise** (temporal, for grain-heavy plates before keying), and
  grain matching (the Film Grain plugin, measured from a plate).
- **Keying:** an IBK-style colour-difference keyer and Edge Extend.

## Owners and files

Proposed split, to keep the two developers out of each other's files:

- **Dev2:** P1 (color: `src/ocio/`, `image.rs` / `media.rs` file
  boundaries, the viewer's display transform, the GPU shaders) and P3
  (regions and caches: `graph.rs` evaluator, `ops.rs` region
  declarations, `gpu/`), since both reach into the renderer and the GPU.
- **Dev1:** P2 (groups and gizmos: `graph.rs` structure, `gui/`
  node editor and Properties, `session.rs` / Python / MCP), P4 (the
  farm: `main.rs`, `packaging/farm/`) and P5 (EXR: the reader / writer,
  Shuffle, Metadata).
- `graph.rs` is shared between P2 and P3: the Supervisor sequences
  those milestones so they don't overlap.

## Order

1. P1a–P1c (color through the viewer) ‖ P2a (Group node)
2. P1d (color nodes) ‖ P2b–P2c (exposed controls, gizmos)
3. P3a (regions) ‖ P4a–P4b (command line, reproducible)
4. P3b–P3c (disk cache, proxies) ‖ P5 (EXR)
5. P4c (submitter), then the comp tools

A release after each step, as in phase 14.

## After phase 15

Deep compositing (deep EXR), stereo / multi-view, a planar tracker,
editorial conform (OTIO / EDL, timecode), review (a flipbook with the
display transform, A/B wipe, annotations), pipeline hooks (on-open /
on-save callbacks, per-show configuration), signed and notarized builds
and testing on real Windows machines.

## Questions for the user

- **Q1.** OCIO: link the real OpenColorIO library (C++, exact, every
  config works; a native dependency to build on Linux, macOS and
  Windows) or a Rust implementation of what the ACES config needs (no
  C++, but configs using other transforms won't load)? The Supervisor
  recommends the real library.
- **Q2.** Which ACES config by default: the CG config (ACEScg working
  space, small) or the Studio config (every camera's input transform,
  larger)?
- **Q3.** Group shortcut: ⌥G for Group (⌘G stays the backdrop), or swap
  them so ⌘G groups nodes as in other compositors and the backdrop
  moves to ⌥G?
- **Q4.** Farm: Deadline only, or OpenCue as well? Any other manager
  your studio uses?
- **Q5.** Start phase 15 now, or finish anything in phase 14 first
  (the open TODO items: Geo Info per-point selection, Scanline Render's
  Width / Height with a background)?
