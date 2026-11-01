# ArclightVFX Compositor: roadmap

Where the project is, what comes next, and where help is welcome. It
changes as work lands; `CHANGELOG.md` has what shipped, version by
version. Versions are `0.<phase>.<increment>`: the middle number is the
development phase.

## Where it is (beta)

A node-based compositor with a 3D scene, written in Rust, for Linux,
macOS and Windows (x64, early), scriptable from Python and over MCP.
Done so far, by phase:

- **2D compositing:** reading and writing images, video and sequences
  (EXR, DPX, TIFF, PNG, …), color, keying, warps, tracking, roto and
  paint, a curve editor, render to disk.
- **3D (phase 9):** cameras, cards, meshes (OBJ, glTF, Alembic), lights
  and shadows, Scanline Render with render passes and motion blur, a 3D
  viewport with gizmos, a camera tracker with a solve, particles.
- **Gaussian splats (phase 10):** reading captures and rendering them
  with everything else.
- **Engine and platform (phase 11):** crash reports kept on the user's
  machine, plugins in helper processes.
- **GPU acceleration (phase 12):** wgpu (Vulkan, Metal) for most 2D
  nodes, Scanline Render, shadows, particles, splats and the viewer.
- **Procedural geometry (phase 13, GeoNodes-like):** Scatter, attributes
  from formulas, noise, images and falloffs, Instance on Points (drawn
  instanced on the GPU), curves edited in the 3D view, Curve to Mesh,
  mesh operations (Normals, Subdivide, Displace, Extrude), particles to
  and from points, Fit Plane, Project 3D (camera projection, several
  cameras blended), Write Geo (OBJ / glTF export). Plan:
  `docs/geo-plan.md`.
- **Workspace (phase 14):** panels in windows of their own for a second
  monitor; File ▸ Import project, copy / paste of nodes, templates; Bake
  Projection (to uv textures; a Cube atlas layout); a Camera on its own
  and Scanline Render picking its camera by name; examples for each in
  Help ▸ Examples. A Windows x64 build since 0.13.34.

## Now

Phase 14 is wrapping up, with hands-on testing of the new panels and
examples on Linux and macOS. Bug reports from real shots are the most
useful thing right now (Help ▸ Report a problem in the app gathers
what's needed).

## Next: phase 15, production readiness

What a film pipeline needs before a facility can adopt it, planned in
`docs/production-plan.md` (proposed, awaiting approval):

- **Color management:** OpenColorIO configs (ACES by default), an input
  space on every Read, an output space on every Write, a display / view
  transform in the viewer on the GPU, and OCIO nodes (color space,
  display, LUT file, CDL). Old projects keep today's look.
- **Node groups and gizmos:** nodes wrapped up as one node with its own
  exposed controls, opened like a sub-graph, and saved as gizmos shared
  across projects and shows.
- **Big plates:** rendering only the region asked for, a disk cache,
  and a proxy mode (½, ¼), so 4K–8K EXR comps stay interactive.
- **The farm:** `arclight render` over frame ranges and chunks, JSON
  progress, documented exit codes, path mapping, bit-identical CPU
  renders on any machine, and a Deadline submitter.
- **EXR for a pipeline:** multi-part and multi-layer files, a Shuffle
  node, every compression, metadata and timecode carried through.
- **Alongside:** STMap, Defocus / ZDefocus, VectorBlur and an optical
  flow retime, Denoise, an IBK-style keyer and Edge Extend.

## Later

- Deep compositing (deep EXR) and stereo / multi-view.
- A planar tracker; editorial conform (OTIO / EDL, timecode).
- Review tools: a flipbook with the display transform, A/B wipe,
  annotations.
- **Python plugins** (custom nodes in Python; native plugins exist).
- **Alembic** curves, points and cameras; **USD**.
- Pipeline hooks (on-open / on-save callbacks, per-show configuration).
- Signed and notarized macOS builds, signed Windows builds tested on
  real machines, distro packages (deb, rpm, AUR).

## Not planned (on purpose)

- OpenFX hosting (a large project of its own).
- Importing other applications' scripts or project files.

## How to help

- **Try it on real work** and report what breaks or confuses: an issue
  with the steps, the project file if you can share it, and the crash
  report if there was one.
- **Pick an item** from "Next", "Later" or `TODO.md` and open an issue
  first, to agree on the approach before code.
- **Build and test** (see `README.md`, "Build"):

  ```sh
  cargo build --release
  cargo test --release
  cargo clippy --release --all-targets
  ```

  Changes come with tests, and clippy stays clean with and without the
  `gpu` feature (`--no-default-features --features gui`). GPU changes
  also pass `arclight gpu --check`, which compares every GPU path against
  the CPU on your machine.
- **Read first:** `README.md` (features, file format, scripting),
  `docs/help.md` (the in-app help) and the plans in `docs/`.
