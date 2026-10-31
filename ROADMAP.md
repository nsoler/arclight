# ArclightVFX Compositor: roadmap

Where the project is, what comes next, and where help is welcome. It
changes as work lands; `CHANGELOG.md` has what shipped, version by
version. Versions are `0.<phase>.<increment>`: the middle number is the
development phase.

## Where it is (beta)

A node-based compositor with a 3D scene, written in Rust, for Linux and
macOS, scriptable from Python and over MCP. Done so far, by phase:

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
  mesh operations (Normals, Subdivide), particles to and from points, Fit
  Plane, Project 3D (camera projection); Displace, Extrude and Write Geo
  (OBJ / glTF export) are landing now. Plan: `docs/geo-plan.md`.

## Now

Phase 13 is wrapping up, then a pause for hands-on testing of the new
geometry nodes, curves and Project 3D on Linux and macOS. Bug reports
from real shots are the most useful thing right now (Help ▸ Report a
problem in the app gathers what's needed).

## Next

- **Detachable panels for dual monitors:** the Node Graph (and the
  Viewer, Curve Editor, Properties) in their own windows, docked back
  when closed, remembered between sessions.
- **Phase 13 follow-ups:** Project 3D under motion blur on the GPU,
  baking a projection to a uv texture, per-point selection in Geo Info,
  world-space values in the sheet.
- **Node groups:** reusable subgraphs with their own parameters.
- **Smaller fixes** listed in `TODO.md`.

## Later

- A depth-aware **Defocus** node, on the GPU.
- **OCIO** color management and a GPU display transform in the viewer.
- **Tiled / region-of-interest** processing for very large plates.
- **Python plugins** (custom nodes in Python; native plugins exist).
- **Alembic** curves, points and cameras; **USD**.
- **Windows** builds; distro packages (deb, rpm, AUR); signed and
  notarized macOS builds.

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
