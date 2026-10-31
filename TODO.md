# Arclight — TODO

A running list of what's still wanted, as of 0.11.0 (2026-10-30). The
3D plan (`docs/phase-4-plan.md`, M1–M13) is done; this is what's left
around it. `HANDOFF.md` has the details and history; add items here as
they come up and strike them (or delete them) when they ship.

## To check by hand (user)

- [X] 0.10.34–0.10.35: sound. Open a clip with sound in a Media In and
      view it; L plays it in step (lips / claps against the picture);
      K / Space / J / arrows / scrubbing are silent; a short loop restarts
      it at the in point; a heavy node makes it drop out, then return in
      step; the speaker button off / on, kept after a restart; with no
      audio device playback still works and the status says sound is off.
      Media Out to .mov / .mp4 keeps the sound in sync (Audio auto); the
      DMG plays sound on the Mac
- [X] 0.10.20–0.10.22: a Camera set to Orthographic (Ortho width) through
      Scanline Render, and the 3D view's 5 / axis views; a far-off
      textured card no longer shimmers; Read Geo an .abc from Blender /
      Maya / Houdini (animated, deforming, motion blur)
- [X] 0.10.15–0.10.19: Camera Tracker on real footage: a Roto mask over
      something moving through the shot; Export Linked (re-solve or Set
      origin and the Camera follows; Bake / Unlink in Properties); pick
      points over the exported Scanline Render; Solve lens distortion on
      a wide-lens shot with camera movement (K1/K2 in the panel, the
      Lens Distort under the render)
- [X] 0.10.2–0.10.14: Splat Crop (drag its box in 3D), Splat Color, .splat /
      .spz files; Read Splat a real capture (.ply from Postshot /
      Polycam / Luma…; Up axis now defaults to -y (0.10.14)), render it
      with a card in front / behind, the 3D view's dots then full render,
      F, click to select
- [X] 0.9.65–0.9.68: Edit ▸ Preferences (scale, theme, snap, straight
      wires, autosave, Open Recent, undo levels, New project defaults,
      3D view mouse / trackpad / shading); File ▸ New's project folder;
      media dialogs start there; move a saved project folder (or copy it
      to the Mac) and it opens with its plates
- [ ] 0.9.62–0.9.64: Particle Render texture sequences (play / loop /
      random start); an emitter on a moving card leaves a trail and plays
      fast; MCP / Python calls to a running GUI share its caches
      and stay responsive
- [ ] 0.9.58–0.9.61: several Viewer nodes (the active one shows, eye
      icon); 3D view motion blur preview: particle dots streak, looking
      through a moving camera blurs by its motion; Scanline Render
      Jitter shutter on / off
- [X] 0.9.52–0.9.57: Read Geo Opacity; view preferences remembered
      between runs (Show frame size, Solid / Lit, wireframe, shadows);
      motion blur grain instead of copies at low counts; particle streaks
      curving under gravity; motion blur in the 3D view once it settles
- [X] 0.9.48: Scanline Render motion blur (incl. particles), the 3D
      view's Solid / Lit (⇧Z) (the camera setup before the Viewer:
      user tested it, which led to 0.9.49–0.9.51)
- [X] 0.9.47: the viewer's frame size labels, the square About logo
      (Card Opacity checked by the user with 0.9.51)
- [X] 0.9.34–0.9.46: node graph Tab / ⇧R / ⇧T / ⇧M; Area Light, falloff,
      radius, exposure, Look at; light handles and Look through (0);
      shadows and the shadow pass; Light 2D; particles
- [X] 0.9.33: Merge Swap A / B, Mask channel / Invert, the centered
      playback strip, the node graph Fit button
- [X] 0.9.32: `examples/media/musicmen.MP4` reads and plays through Read
      and Media In
- [X] 0.9.31: the file picker no longer triggers "Not Responding" while
      navigating folders
- [X] 0.9.30: Fit to project, Project Settings
- [X] The macOS DMG on Apple Silicon (M4 Pro), and on an Intel Mac
- [X] The 3D view with a real mouse and trackpad (orbit / pan / zoom,
      pie, gizmos, G / R / S typing)

## Particles

- [x] Bridged particle renders no longer block the viewer and go on
      from its simulation (0.10.30)

## Gaussian splats (plan: `docs/splat-plan.md`, S1–S5 done)

Phase 10: 0.10.x on branch `wip-ns-splats` (started 0.10.0).

- [x] S1 (0.10.1–0.10.3): Read Splat (`.ply`) and a CPU splat renderer in Scanline
      Render, composited with cards and meshes by depth
- [x] S2 (0.10.4–0.10.6): the 3D view (dots while moving, full once settled), picking,
      framing; `scene_objects` / a paged `splats` query in MCP and Python
- [x] S3 (0.10.7–0.10.8): render passes, holdouts and motion blur for splats
- [x] S4 (0.10.7, 0.10.9–0.10.11): Splat Crop (with its 3D view handle), Splat Color; placement via Read Splat's own transform
- [x] S5 (0.10.12–0.10.13): `.splat` / `.spz` readers, 2–2.5× faster
      rendering, a per-view cache
- [ ] Splat follow-ups: a GPU path (with the wgpu item); SOG / compressed
      PLY if asked (Up axis now defaults to -y: 0.10.14)

## 3D and rendering

- [x] A true orthographic camera and render path (0.10.20)
- [x] Mipmaps for minified textures (0.10.21)
- [x] Alembic import (.abc, Ogawa): transforms and deforming meshes (0.10.22)
- [ ] Alembic follow-ups: curves, points (into particles?), cameras, face
      sets as materials

## Camera tracker

- [x] A mask input to ignore moving objects (0.10.15)
- [x] A live link from a Camera to the solve (0.10.16)
- [x] Point picking over a Scanline Render of the solved scene (0.10.17)
- [x] Lens-distortion solve, K1/K2; Export undistorts the plate (0.10.19)

## Interface

- [x] Backdrop tools in MCP / Python (0.10.24)
- [x] `set_node_position` / `layout_nodes` in MCP / Python (0.10.28)
- [x] Shift+Tab over the viewer no longer toggles 2D / 3D (0.10.23)

## Media

- [X] Audio: Media Out writes video only, and sources' audio isn't read
      (reading, syncing and mixing it is a project of its own)

## Phase 11: engine and platform (0.11.x, branch `wip-ns-engine`)

Ends in a **beta** (About box, release titles, GitHub pre-releases) for
a public release once tested.

- [ ] Crash reporting
- [ ] Plugin process isolation (a crashing native plugin kills the app)

## Later: 0.12.0

- [ ] GPU acceleration (`wgpu` compute; CPU / rayon only today)
- [ ] Tile-based / region-of-interest processing for very large images
- [ ] Python plugins (custom nodes in Python)
- [ ] Distro packages (deb / rpm / AUR) and signed, notarized macOS
      builds (no right-click → Open; needs the Apple Developer Program)
- [ ] USD / Hydra
- [ ] RLE / 1-bit DPX (needs a sample file first)

## Before going public

- [ ] Licence (MIT / Apache or GPL), a LICENSE and a NOTICES file
- [ ] Audit the repo for anything private (paths, emails in notes,
      `examples/mad-martian.png`, where `musicmen.MP4` came from)
- [ ] A public-facing README, issue templates, CONTRIBUTING
- [ ] Actions is free for public repos: the macOS DMG and wheels should
      build there again

## Housekeeping

- [x] Clippy clean across the workspace (0.10.29)

## Not planned (on purpose)

- OpenFX hosting (a large project of its own)
- Copying another product's licensing scheme
- Nuke script import (originality rule)
