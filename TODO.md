# Arclight — TODO

A running list of what's still wanted, as of 0.9.68 (2026-10-30). The
3D plan (`docs/phase-4-plan.md`, M1–M13) is done; this is what's left
around it. `HANDOFF.md` has the details and history; add items here as
they come up and strike them (or delete them) when they ship.

## To check by hand (user)

- [ ] 0.10.2–0.10.13: Splat Crop (drag its box in 3D), Splat Color, .splat /
      .spz files; Read Splat a real capture (.ply from Postshot /
      Polycam / Luma…; try Up axis -y if it's upside down), render it
      with a card in front / behind, the 3D view's dots then full render,
      F, click to select
- [ ] 0.9.65–0.9.68: Edit ▸ Preferences (scale, theme, snap, straight
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

- [ ] Bridged calls other than `particles` that simulate inside one
      evaluation (`scene_objects`, a Scanline Render / Particles 2D at a
      late frame) hold the shared evaluator for the whole run and the
      viewer waits; renders through the bridge still re-simulate

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
      PLY if asked; Read Splat's Up axis default (y today; most COLMAP
      captures need -y: ask the user)

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
