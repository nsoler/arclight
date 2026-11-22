# Arclight — TODO

What's still open, as of 0.16.0 (2026-11-22). Done items are removed
(`CHANGELOG.md` has what shipped; the long list before this cleanup is in
git: `git show 106707a:TODO.md`). Add items as they come up and delete
them when they ship. `ROADMAP.md` is the public summary.

## To check by hand (user)

On Linux and on the Macs (the DMG), from the release builds.

**Phase 15 (0.15.8)**
- [X] Straight wires: ⌘-click a wire twice, drag the bends into a
      right-angle route; switch to curved: it flows through the same
      points; move both nodes → the route follows, one → bends stay;
      drop a free Blur onto the bent wire → each side keeps its bends;
      ⌘C / ⌘V the pair → the copy has them; save, reopen, undo / redo a
      bend drag; an old project saves byte-identical

**Phase 15 (0.15.7)**
- [X] Open a project with media outside its folder (the Pond comp),
      save: the menu-bar warning; File ▸ Collect Media…: sizes and free
      space, Collect with progress, the nodes point into media/, it
      saves, Undo reverts; copy the folder to another machine or path
      and reopen

**Phase 15 (0.15.6)**
- [X] Pen-draw a Roto that a Transform feeds into a Viewer: the points
      land under the pointer; through a 3D render, pen and "." refuse
      (note shown, clicks pan); a Roto following a Tracker, away from
      its reference frame: select 2+ points, ".", click → the crosshair
      sits where clicked

**Phase 15 (0.15.5)**
- [X] Help ▸ Examples ▸ Geo Info: picking (the 0.14.36 check below,
      with the steps on its backdrops): Tab to 3D, Points on, click a
      sphere dot (orange; one row under Points with Selected only),
      ⇧-click cube corners (Mesh points rows), box over the sphere
      (front only), Esc clears, Points https://www.google.com/search?q=xfree.ocm&oq=xfree.ocm&gs_lcrp=EgZjaHJvbWUyBggAEEUYOdIBCDI3NDhqMGo3qAIAsAIB&sourceid=chrome&source=chrome.ob&ie=UTF-8&sei=IpECa4iII8ug0PEP8oqnoQkoff picks objects; then
      points_heavy + D and box again: still responsive

**Phase 15 (0.15.4)**
- [X] Pond-Martian-Comp, viewer1 on the render: select transform2 → its
      gizmo is dimmed, the note says card2 takes it into a 3D scene,
      press V; a press pans; V → the gizmo sits on the martian. A
      Transform between a Rectangle and a Viewer: the Rectangle's
      handles sit on the moved shape and drag normally. Mac and Linux

**Phase 15 (0.15.3)**
- [X] `cargo test --release` on NVIDIA/Linux ends without a SIGSEGV
      after the results, also right after a shader change (or with
      `__GL_SHADER_DISK_CACHE=0`); a test failure still exits 101;
      `arclight gpu --check` still lists a slow "read one pixel" as FAIL
      while the unit self-test passes

**Phase 15 (0.15.2)**
- [X] A Read Geo of an Alembic export with curves, a particle cache, two
      cameras and a multi-material mesh: the 3D view shows the curves
      and points; Scanline Render's Camera picker lists both cameras
      (read_geo/<name>) and renders through each; Geo Info shows
      faceset.<name> on Faces; Split by face set gives a mesh per
      material; an older project with an Alembic Read Geo renders the
      same; a Maya / Houdini export if one is at hand

**Phase 15 (0.15.1)**
- [X] Open Pond-Martian-Comp (on a Mac, as its paths are) in 2D, wait
      for the first frame, Tab: the cards come textured within a second
      or two, nothing selected; the same with GPU acceleration off; the
      LizardSplat example likewise

**Phase 14 (0.14.37)**
- [X] A Shuffle with 3 inputs routing 2:A → A and 3:depth → depth, and a
      Matte Combine of 4 mattes (one Soft, one inverted), with the GPU
      on and off (Preferences): identical; playback stays on the GPU;
      Macs: the same, plus `gpu --check`

**Phase 14 (0.14.36)**
- [X] Geo Info picking (example: Help ▸ Examples ▸ Geo Info: picking, 0.15.5): Scatter points (or a Cube), 3D toolbar Points
      on: click a point → orange dot, Geo Info (Sheet) Selected only →
      just that row in Points (or Mesh points); ⇧-click two more; box
      over part of a sphere → front vertices only; Esc clears the picks,
      Esc again deselects; Points off → clicks select objects again;
      cloud picks, curve ⇧-picks and G / R / S unchanged; ~200k points:
      a click picks without a hitch

**Phase 14 (0.14.35)**
- [X] Merge an Ellipse (A) over a plate (B) with each of subtract /
      difference / stencil / mask, and Swap A / B on subtract → A − B; a
      Grade with a 3D render's matte.<node> pass as mask (picked from
      Mask channel's list; typing works too), Invert mask; a Camera
      Tracker masked by a pass; a 0.14.34 project with masked nodes and
      merges looks the same

**Phase 14 (0.14.34)**
- [X] On a Mac (Apple M4, Metal): `examples/Pond-Martian-LizardSplat-Combo.json`
      plays at least as fast with the GPU as with it off (was 1017 vs
      321 ms/frame), and `arclight gpu --check` passes 85/85 (new
      splats-over-a-card row)

**Phase 14 (0.14.33)**
- [X] Matte Combine: an Ellipse into input 1, a Rectangle into 2 set to
      subtract, another Ellipse into 3 (union) → the ellipse with a
      hole, plus the circle; Invert on input 2; key input 2's operation
      at 1 (subtract) and 10 (union) → switches at 10, no blend at 5, the
      curve editor shows Step; Soft on with feathered edges; channel =
      a matte.<node> pass from a 3D render; selecting it in a saved
      project doesn't mark it modified; `arclt`:
      create_node("matte_combine", a, b, operations=["union", "subtract"])

      NOTE: Gizmos are Offset to the left and not centered.

**Phase 14 (0.14.32)**
- [X] On a Mac (Apple M4, Metal): `arclight gpu --check` passes 81/81+
      (the 13 depth-plane failures of 2026-11-13 gone); the Pond
      example's still Media In no longer redraws the splat every frame

**Phase 14 (0.14.31)**
- [X] Shuffle routing: wire a plate into a Shuffle, an Ellipse into its
      port 2 (a spare port 3 appears); Properties ▸ Routes ▸ + Add route
      → 2:A → A: the plate takes the ellipse's alpha; 3:depth from a
      Scanline Render ihttps://www.google.com/search?q=xfree.ocm&oq=xfree.ocm&gs_lcrp=EgZjaHJvbWUyBggAEEUYOdIBCDI3NDhqMGo3qAIAsAIB&sourceid=chrome&source=chrome.ob&ie=UTF-8&sei=IpECa4iII8ug0PEP8oqnoQknto depth; a 0.14.30 project's Shuffles look the
      same; `arclt`: create_node("shuffle", a, b, routes=[["2:A","A"]])

**Phase 14 (0.14.30)**
- [X] Roto pen: add a Roto from the menu → pen on; click 4 points, drag
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
- [X] Curve to Mesh (tube, ribbon, profile, pscale taper); Points on Curve
      with Instance on Points
- [ ] Particles to Points driving Instance on Points; an emitter on a
      Scatter's point set
- [ ] Project 3D: the plate comes back exactly from the shot camera;
      occlusion; Crop / Edge; Frame hold with a moving camera; a patch
      stacked Over; GPU on and off match. Help ▸ Examples ▸ Projection
      (multi-projection): Blend softens the seam, Fade start / end,
      Frames fills what one frame misses
- [X] Points from Geo / Grid / Line Points; Normals by angle; Subdivide
      (Loop on a cube); Extrude (individual with Inset, region from
      Attribute Set, walls by `side`)
- [X] Write Geo: render a scene to .obj / .gltf / .glb (textured card,
      instances), open in Blender; Read Geo reads the glTF back
- [X] Area-light shadows at Quality 16: ~7 s a 4K frame with the GPU on

**GPU and the Macs**
- [X] Transform / Corner Pin / Lens Distort / Keyer match with the GPU on
      and off; quitting never leaves a crash report behind
- [X] Scanline Render with shadows and motion blur: about a second at 4K
      with the GPU on, matching the CPU
- [X] A 2D chain with big blurs feels quicker with the GPU on

**Older**
- [X] Several Viewer nodes (the active one shows, eye icon); 3D view
      motion-blur preview (particle streaks, a moving camera); Scanline
      Render Jitter shutter on / off

## Known gaps and follow-ups

- [ ] Wire bends: dragging an output onto a bent wire (merge into wire)
      drops its bends instead of splitting them; bends not in the
      minimap; Shift+R/T/M into a bent wire has no test of its own
- [ ] 2D handles: mapping through a Card + Camera (homography) to drag
      on the rendered card; a first pen click for a new Roto fed by an
      auto-pivot Transform can be off until the worker reports that
      Transform's input shape (one render later)
- [ ] MCP / Python / CLI "collect media" (`arclight::collect` has it all)
- [ ] Project 3D targets are node names fixed at the node: fine today
      (nothing renames or merges meshes under a new node); if such an op
      is added, carry projections on Card / Model as Instances do
- [ ] Particles to Points ids pack emitter (8 bits) + birth count (24
      bits): past ~16.7 M births ids wrap (only Instance on Points'
      random pick cares)
- [ ] Splats: SOG / compressed PLY if asked
- [ ] Alembic: Hermite / power curve bases (read as linear), camera
      film offset / squeeze / overscan, indexed arbGeomParams
- [ ] A new logo: `assets/arclight.png` and the full logo still say
      ARCLIGHT
- [ ] Windows: the zip ships (since 0.13.34) but is untested on a real
      PC; MCP / GUI bridge without Unix sockets (named pipe or
      localhost), `%APPDATA%` folders, `.dll` plugins; unsigned
      (SmartScreen warning)
- [ ] RLE / 1-bit DPX (needs a sample file first)

## Now: phase 16, polish (0.16.x, `wip-ns-polish`)

The user's call (2026-11-22): wire bends snapping to exact right angles
(and the merge-into-wire split), Collect Media from MCP, dragging
handles on a rendered card through Card + Camera. Phase 15 (gap fixes)
ran 0.15.0–0.15.8 on `wip-ns-gapfixes`.

## Later: production readiness (proposed)

Planned in `docs/production-plan.md`, waiting for the user's answers (Q1–Q5
there); after phase 16 (polish). P1 color management (OCIO /
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
