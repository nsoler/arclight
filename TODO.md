# Arclight — TODO

What's still open, as of 0.14.33 (2026-11-21). Done items are removed
(`CHANGELOG.md` has what shipped; the long list before this cleanup is in
git: `git show 106707a:TODO.md`). Add items as they come up and delete
them when they ship. `ROADMAP.md` is the public summary.

## To check by hand (user)

On Linux and on the Macs (the DMG), from the release builds.

**Phase 14 (0.14.33)**
- [ ] Matte Combine: an Ellipse into input 1, a Rectangle into 2 set to
      subtract, another Ellipse into 3 (union) → the ellipse with a
      hole, plus the circle; Invert on input 2; key input 2's operation
      at 1 (subtract) and 10 (union) → switches at 10, no blend at 5, the
      curve editor shows Step; Soft on with feathered edges; channel =
      a matte.<node> pass from a 3D render; selecting it in a saved
      project doesn't mark it modified; `arclt`:
      create_node("matte_combine", a, b, operations=["union", "subtract"])

**Phase 14 (0.14.32)**
- [ ] On a Mac (Apple M4, Metal): `arclight gpu --check` passes 81/81+
      (the 13 depth-plane failures of 2026-11-13 gone); the Pond
      example's still Media In no longer redraws the splat every frame

**Phase 14 (0.14.31)**
- [ ] Shuffle routing: wire a plate into a Shuffle, an Ellipse into its
      port 2 (a spare port 3 appears); Properties ▸ Routes ▸ + Add route
      → 2:A → A: the plate takes the ellipse's alpha; 3:depth from a
      Scanline Render into depth; a 0.14.30 project's Shuffles look the
      same; `arclt`: create_node("shuffle", a, b, routes=[["2:A","A"]])

**Phase 14 (0.14.30)**
- [ ] Roto pen: add a Roto from the menu → pen on; click 4 points, drag
      one for tangents, Alt-drag breaks; Esc / ⌫ removes the last; click
      the first point → closes, renders, and Feather / falloff / strength
      / cut-off enable; P with a Read selected → the new Roto takes it as
      background; undo steps one point at a time; a 0.14.29 project with
      Rotos looks the same

**Phase 14 (0.14.27)**
- [X] Node graph: a Read of a still, a Read of a #### sequence and a
      Media In of a video each show their first frame above the node,
      aspect right, dimmed when the node is off; click selects,
      double-click views; rewrite the still on disk: it updates within a
      second; a missing file: no thumbnail, no CPU spin; zoomed far out:
      hidden; View ▸ Node thumbnails off stays off after a restart; play
      a video while its thumbnail first loads: at most one hitch

**Phase 14 (0.14.29)**
- [X] Viewer, GPU on: play a heavy GPU comp (blur / grade on a 4K
      plate): fps up; the picker follows the pointer, also while
      playing; switch to a plane layer (depth): it shows; turn the GPU
      off in Preferences mid-play: cached frames stay and scrub
      instantly; old projects look the same; Macs: the same, plus
      `gpu --check`

**Phase 14 (0.14.28)**
- [X] Geo Info: World on a moved / scaled mesh changes P and N
      (instances don't); a Camera Tracker's Cloud tab numbers match the
      tracker's; pick cloud points in 3D + Selected only: just those
      rows; a Scanline Render with a background shows the size note in
      Properties

**Phase 14 (0.14.4–0.14.13)**
- [X] Cube UV layout atlas: each face its own cell; Bake Projection on a
      projected atlas cube renders like the live projection

**Phase 13 (0.13.x)**
- [X] Scatter with a painted / keyed mask: raising density or painting
      the mask doesn't make existing points jump; Min distance; tracker
      points as the source
- [X] Attribute Set / Noise / Falloff / from Image / Transfer, checked in
      the viewer's Sheet (Geo Info)
- [X] Instance on Points: rocks and cards facing the camera, variant /
      random pick, pscale and Cd, shadows and motion blur; 100k cards stay
      quick with the GPU on
- [X] Delete Geo's box / sphere moved with G, sized with S; an expression
      like `random(@id) < 0.3`
- [X] Curves in the 3D view: pick (⇧), drag, G / R / S, Bezier handles
      (Alt breaks), Ctrl/⌘-click adds or inserts, Delete; a Curve from a
      solved camera's path
- [ ] Curve to Mesh (tube, ribbon, profile, pscale taper); Points on Curve
      with Instance on Points
- [ ] Particles to Points driving Instance on Points; an emitter on a
      Scatter's point set
- [ ] Project 3D: the plate comes back exactly from the shot camera;
      occlusion; Crop / Edge; Frame hold with a moving camera; a patch
      stacked Over; GPU on and off match. Help ▸ Examples ▸ Projection
      (multi-projection): Blend softens the seam, Fade start / end,
      Frames fills what one frame misses
- [ ] Points from Geo / Grid / Line Points; Normals by angle; Subdivide
      (Loop on a cube); Extrude (individual with Inset, region from
      Attribute Set, walls by `side`)
- [ ] Write Geo: render a scene to .obj / .gltf / .glb (textured card,
      instances), open in Blender; Read Geo reads the glTF back
- [ ] Area-light shadows at Quality 16: ~7 s a 4K frame with the GPU on

**GPU and the Macs**
- [ ] Transform / Corner Pin / Lens Distort / Keyer match with the GPU on
      and off; quitting never leaves a crash report behind
- [ ] Scanline Render with shadows and motion blur: about a second at 4K
      with the GPU on, matching the CPU
- [ ] A 2D chain with big blurs feels quicker with the GPU on

**Older**
- [X] Several Viewer nodes (the active one shows, eye icon); 3D view
      motion-blur preview (particle streaks, a moving camera); Scanline
      Render Jitter shutter on / off

## Known gaps and follow-ups

- [ ] **Apple Silicon: 3D render with splats + cards is ~3× slower on the
      GPU than the CPU** (user, 2026-11-13; M4, Metal, v0.14.29 DMG).
      Project `examples/Pond-Martian-LizardSplat-Combo.json` (Read Splat
      `geo/hornedlizard.spz` → Splat Crop, two Cards with Media Ins:
      `media/pond-720.mp4` and the still `mad-martian.png`, a Camera,
      Scanline Render 1280×720; the user's original plate was 3840×2160,
      timed below). **Not Metal-specific** (2026-11-21, RTX 5070: GPU
      188–354 vs CPU 137–147 ms/frame at samples 3, scaling with the AA
      samples): the splat/surface merge in `raster.wgsl` restarts the
      tile's splat walk for every sample; Dev1 fixing (front cursor +
      batched walk).
      Warm playback, Evaluator kept across frames: full project 1017
      ms/frame on the GPU vs 321 ms with `ARCLIGHT_GPU=0`; splat + still
      card 451 vs 164; two cards, no splat 146 vs 113; the plate alone 22
      ms. A 720p plate changes nothing (992 vs 313). The CPU sits ~80% in
      `__psynch_cvwait` (waiting on the GPU), so it's a GPU pass, not
      host code; `gpu --check` has 3D renders 4–22× faster on the GPU, so
      it's specific to splats mixed with surfaces. Profile with Xcode's
      Metal capture; suspects: `sort_tiles` in `splat_view.wgsl` when a
      tile holds more than `LOCAL` (2048) pairs (the in-place path), and
      how `raster.wgsl` merges splats with surfaces. Done when the GPU is
      at least as quick as the CPU on this project and `gpu --check`
      passes. The build is native arm64 + Metal already (universal DMG,
      `arclight gpu` says `Apple M4 (Metal)`): an Apple-Silicon-only build
      wouldn't help
- [ ] **NVIDIA/Linux: a process can SIGSEGV at exit** inside the
      driver (libnvidia-gpucomp, a background shader compile racing the
      ICD teardown), after its work is done; mostly with shaders cold in
      the NVIDIA cache (first runs after a WGSL change). Pre-existing
      (0.14.30: 1 / 8 lib-test runs with `__GL_SHADER_DISK_CACHE=0`).
      Candidate: an atexit hook in `gpu/mod.rs` waiting on the device
      before the ICD tears down (Dev1, measured with a fast repro)
- [ ] Shuffle / Matte Combine: a multi-source GPU kernel (routes
      reading input 2+ and Matte Combine run on the CPU today)
- [ ] Geo Info: picking individual point-set / mesh points in the 3D
      view (to narrow Points / Mesh points like Cloud picks) isn't
      supported; only Camera Tracker cloud points are pickable
- [ ] Project 3D targets are node names fixed at the node: fine today
      (nothing renames or merges meshes under a new node); if such an op
      is added, carry projections on Card / Model as Instances do
- [ ] Particles to Points ids pack emitter (8 bits) + birth count (24
      bits): past ~16.7 M births ids wrap (only Instance on Points'
      random pick cares)
- [ ] Splats: SOG / compressed PLY if asked
- [ ] Alembic: curves, points, cameras, face sets as materials
- [ ] A new logo: `assets/arclight.png` and the full logo still say
      ARCLIGHT
- [ ] Windows: the zip ships (since 0.13.34) but is untested on a real
      PC; MCP / GUI bridge without Unix sockets (named pipe or
      localhost), `%APPDATA%` folders, `.dll` plugins; unsigned
      (SmartScreen warning)
- [ ] RLE / 1-bit DPX (needs a sample file first)

## Next: phase 15, production readiness (proposed)

Planned in `docs/production-plan.md`, waiting for the user's answers (Q1–Q5
there); the user said stay on 0.14.x for now. P1 color management (OCIO /
ACES), P2 node groups and gizmos, P3 regions / disk cache / proxies, P4 the
farm, P5 EXR; alongside STMap, Defocus / ZDefocus, vector tools, Denoise.
On hold in `ROADMAP.md` "Later": Paint, a 3D pivot.

## Before going public

- [ ] Licence (MIT / Apache or GPL), a LICENSE and a NOTICES file
- [ ] Audit the repo for anything private (paths, emails in notes,
      `examples/mad-martian.png`, where `musicmen.MP4` came from)
- [ ] A public-facing README, issue templates, CONTRIBUTING
- [ ] Actions is free for public repos: build the macOS DMG and wheels
      there again
- [ ] Distro packages (deb / rpm / AUR); signed, notarized macOS builds

## Not planned (on purpose)

- OpenFX hosting (a large project of its own)
- Copying another product's licensing scheme
- Nuke script import (originality rule)
