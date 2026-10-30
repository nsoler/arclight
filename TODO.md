# Arclight — TODO

A running list of what's still wanted, as of 0.9.61 (2026-10-30). The
3D plan (`docs/phase-4-plan.md`, M1–M13) is done; this is what's left
around it. `HANDOFF.md` has the details and history; add items here as
they come up and strike them (or delete them) when they ship.

## To check by hand (user)

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

- [ ] An image-sequence input for sprite textures
- [ ] Animated geometry emitters without re-simulating from scratch
- [ ] Reuse the GUI's evaluator in the MCP bridge, so `particles` via an
      attached GUI doesn't re-simulate each call

## Gaussian splats (plan: `docs/splat-plan.md`, proposed)

- [ ] S1: Read Splat (`.ply`) and a CPU splat renderer in Scanline
      Render, composited with cards and meshes by depth
- [ ] S2: the 3D view (dots while moving, full once settled), picking,
      framing; `scene_objects` / a paged `splats` query in MCP and Python
- [ ] S3: render passes, holdouts and motion blur for splats
- [ ] S4: Splat Crop, Splat Color (and transform) nodes
- [ ] S5: `.splat` / `.spz` readers, faster rendering (a GPU path if the
      wgpu item lands)

## 3D and rendering

- [ ] A true orthographic texture path in render3d (ortho views use a
      very long lens today)
- [ ] Mipmaps for minified cards
- [ ] Alembic import for animated caches

## Camera tracker

- [ ] A live link from a Camera to the solve (Export bakes keys today)
- [ ] Lens-distortion solve
- [ ] A mask input to ignore moving objects
- [ ] Point picking over a Scanline Render of the solved scene

## Interface

- [ ] Backdrop tools in MCP / Python
- [ ] Shift+Tab over the viewer also toggles 2D / 3D (main.rs's plain
      Tab match ignores Shift); harmless, could be tightened

## Engine and platform (larger)

- [ ] GPU acceleration (`wgpu` compute; CPU / rayon only today)
- [ ] Tile-based / region-of-interest processing for very large images
- [ ] Python plugins (custom nodes in Python)
- [ ] Plugin process isolation (a crashing native plugin kills the app)
- [ ] Distro packages (deb / rpm / AUR) and signed, notarized macOS
      builds (no right-click → Open)
- [ ] USD / Hydra
- [ ] Crash reporting
- [ ] RLE / 1-bit DPX (needs a sample file first)

## Housekeeping

- [ ] Tidy the clippy warnings newer toolchains raise on old code
      (`graph.rs` `from_str` naming, `plugins/example` `chunks_exact`)

## Not planned (on purpose)

- OpenFX hosting (a large project of its own)
- Copying another product's licensing scheme
- Nuke script import (originality rule)
