# Arclight — TODO

A running list of what's still wanted, as of 0.13.34 (2026-10-31). The
3D plan (`docs/phase-4-plan.md`, M1–M13) is done; this is what's left
around it. `HANDOFF.md` has the details and history; add items here as
they come up and strike them (or delete them) when they ship.

## To check by hand (user)

Phase 13 (v0.13.34, the pause after 2026-10-31): try these on Linux and
on the Macs (the DMG), from the release builds.

- [X] Tab switches 2D / 3D with the pointer over the node graph, the
      viewer or Properties; not while typing in a field (0.13.14)
- [X] Help ▸ Examples ▸ Geometry nodes opens geo-scatter as an untitled
      project and renders (rocks, bush cards, sun shadows); Save asks
      where (0.13.21)
- [X] Help ▸ Examples ▸ Particles opens particledirectionalforce as an
      untitled project; playing it, the teal stream drifts right and the
      orange one left (0.13.36)
- [X] A wire dropped anywhere on a node connects to its first free input
      (the node lights up, the wire's end snaps); never into a mask; the
      other way from an input (0.13.2, 0.13.6)
- [ ] Scatter on a Plane with a painted / keyed mask: raise the density
      or paint the mask and existing points don't jump; Min distance;
      tracker points as the source (0.13.9)
- [ ] Attribute Set / Attribute Noise / Falloff (a Locator moving points'
      weight) / Attribute from Image (through a camera) /
      Attribute Transfer, checked in the viewer's **Sheet** (Geo Info)
- [ ] Instance on Points: rocks and cards facing the camera, pick by
      variant / random, pscale and Cd; shadows and motion blur; big
      counts (100k cards) stay quick with the GPU on (0.13.13–0.13.23)
- [ ] Delete Geo's box / sphere moved with G and sized with S in the 3D
      view; an expression like random(@id) < 0.3 (0.13.10)
- [X] Fit Plane from the camera tracker's point picker (Fit plane, Use
      picked points); as a shadow catcher (0.13.12)
- [ ] Curves in the 3D view: pick points (Shift), drag, G / R / S, Bezier
      handles (Alt breaks), Ctrl/Cmd-click adds or inserts, Delete; a
      Curve from a solved camera's path (0.13.25, 0.13.26)
- [ ] Curve to Mesh (tube, ribbon, a profile curve, pscale taper) and
      Points on Curve with Instance on Points (posts upright, facing
      along) (0.13.29)
- [ ] Particles to Points driving Instance on Points; a Particle Emitter
      with the point set shape on a Scatter (0.13.27)
- [ ] Project 3D: a plate projected from the shot camera comes back
      exactly; occlusion; Crop / Edge; Frame hold with a moving camera; a
      patch stacked Over; GPU on and off look the same (0.13.30, 0.13.32)
- [ ] Points from Geo / Grid / Line Points, Normals (by angle on a
      cylinder), Subdivide (Loop on a cube) (0.13.28–0.13.33)
- [ ] Area-light shadows at Quality 16 render in ~7 s at 4K with the GPU
      on (0.13.3, 0.13.4)
- [ ] On the Macs: `arclight gpu --check` (69 rows now, including
      instancing, instanced shadows and Project 3D) and
      `cargo test --release` on Metal
- [ ] Extrude a cube's faces: individual with Inset, region on faces
      picked by Attribute Set (`extrude`); walls colored by `side`
      (0.13.40)
- [ ] Write Geo: `arclight render` a scene to .obj, .gltf and .glb
      (with a textured card and instances) and open them in Blender;
      Read Geo reads the glTF back with its attributes (0.13.41)
- [ ] Help ▸ Examples ▸ Projection (multi-projection.json, 0.13.39):
      Blend with others softens the seam on the wall and block; Fade
      start / end fade a grazing projection out; Frames (three frames of
      one moving camera) fills what one frame misses; GPU on and off look
      the same (0.13.38)

- [X] 0.12.17 (beta release v0.12.17): at the default window size the
      viewer's toolbar wraps onto a second row (Fit and 1:1 visible) and
      the 3D view's too; a long node error (a plugin crash, a missing
      file) wraps in the viewer and the 3D view and can be selected and
      copied (0.12.15)
- [X] 0.12.16 (layered shadows): a card below full opacity or a cutout
      texture casts a soft-edged shadow that looks the same with the GPU
      on and off, and renders much faster with it on; on the Macs too
- [X] 0.12.0 (GPU G1): Preferences ▸ Performance ▸ GPU acceleration off /
      on and the About line follows; `ARCLIGHT_GPU=0` greys it; Help ▸
      GPU self-test. On the **M4 Pro** and the **Intel Mac**: `arclight
      gpu` names the GPU (Metal; "unified memory" on the M4 Pro), `arclight
      gpu --check` passes, and `cargo test --release` there (the GPU tests
      run on Metal)
- [X] 0.12.10 (GPU G6): `examples/hornedlizard.json` in the 3D view
      orbits smoothly with the GPU on; its render looks the same with
      `ARCLIGHT_GPU=0`; on the Macs too
- [X] 0.12.9 (GPU G7): view a 4K GPU chain (blurs, grade): it looks the
      same as with `ARCLIGHT_GPU=0`, and scrubbing is smooth; pixel
      readout values unchanged. On the M4 Pro and the Intel Mac too
      (eframe on Metal sharing the engine's device)
- [ ] 0.12.4 (GPU G3): Transform / Corner Pin / Lens Distort / Keyer
      look the same with the GPU on and off; on the Macs `arclight gpu
      --check --quick` first (seconds), then the full check; quitting the
      app and the CLI never leaves a crash report behind
- [ ] 0.12.3 (GPU G5 part 1): a Scanline Render with shadows and motion
      blur renders in about a second at 4K with the GPU on (it took
      30 s) and looks the same as with it off; on the Macs too
- [X] 0.12.2 (GPU G4): a Scanline Render without shadows renders much
      faster with the GPU on, and looks the same as with it off (toggle
      in Preferences); the 3D view orbits more smoothly (Lit and Solid);
      on the Macs too
- [ ] 0.12.1 (GPU G2): on the Macs, `arclight gpu --check` includes the
      Grade / Merge / Blur rows (Metal's fast math against the
      tolerances); a 2D chain with big blurs feels quicker with the GPU on

- [X] 0.11.4: the 3D view with a mouse (user found it hard and erratic in
      0.11.3; since 0.13.1 the Move gizmo by default, G / R / S switch it:
      the user found it good, 2026-10-31): select a Camera / Card / light and the All gizmo shows
      (arrows, rings, cubes); drag each ring, including one seen nearly
      edge-on, and it turns smoothly the way the pointer goes; R with the
      pointer near the object doesn't swing it; middle-drag orbits around
      the selection (it stays put); scroll only zooms. Also the frame rate
      while orbiting a heavy scene (not measured)

- [x] 0.11.1–0.11.2 (before the beta): run `ARCLIGHT_CRASH_TEST=panic
      arclight-gui` (also `segv`, `abort`, and `worker`, which should only
      show an error): a report is saved, unsaved changes land in the
      autosave, and the next start shows "Arclight closed unexpectedly"
      (View / Copy / Report on GitHub / Don't ask again); Help ▸ Report a
      problem. Plugins: the example's Film Grain / Vignette render as
      before, on Linux and in the Mac DMG (helper processes there too)
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
- [X] 0.9.62–0.9.64: Particle Render texture sequences (play / loop /
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

- [x] **The beta step** (0.11.3): rename to **ArclightVFX Compositor** (user, 2026-10-30;
      short name **ArclightVFX**) and switch to beta.
      - Renamed: window title, About box (with "Beta"), Help window title,
        the crash dialog's text, the Linux launcher entry (`Name=`, both
        install.sh files), the macOS app (`ArclightVFX.app`,
        CFBundleName / DisplayName, the DMG volume name), asset names
        (`ArclightVFX-<ver>-universal.dmg`,
        `arclightvfx-<ver>-linux-x86_64.tar.gz`), README / help /
        CHANGELOG headings, MCP serverInfo's title, release titles
        ("ArclightVFX 0.11.n (beta)", GitHub releases marked pre-release).
      - Kept, so nothing breaks for existing users and scripts: the
        binaries `arclight` / `arclight-gui`, the Python package `arclt`,
        the settings / state folders (`~/.config/arclight`, …), the MCP
        server id `arclight`, the macOS bundle id
        `com.github.nsoler.arclight`, the crate names, the GitHub repo
        (renaming it later is possible: GitHub redirects).
      - Mac users delete the old `Arclight.app` once (the release notes
        say so).

- [x] Crash reporting (0.11.1)
- [x] Plugin process isolation (0.11.2)
- [ ] A new logo: `assets/arclight.png` and the full logo still draw the
      word ARCLIGHT (the About box, icons 128 px and up)
- [x] Long node errors (a plugin crash with its report path) are clipped
      on one line in the viewer: wrap them (0.12.15)

## Later: 0.12.0

- [x] GPU acceleration (`wgpu`: Vulkan, Metal): phase 12, 0.12.x on
      `wip-ns-gpu`, plan `docs/gpu-plan.md`; G1–G7 done in 0.12.0–0.12.16
      (pointwise ops, Blur, warps, Keyer, Shuffle, zmerge, Scanline
      Render with shadows and motion blur, see-through shadows, particle
      sprites, splats, zero-copy viewer and 3D view)
- [x] GPU frame cache for playback (Dev1, 0.12.18): cached frames keep
      their color GPU-resident and play through the zero-copy display;
      separate budget `gpu.frame_cache_mb` (1024)
- [x] Area-light shadows stay slow on the GPU too (16 samples: 20–34 s
      a 4K frame): 0.13.3 / 0.13.4, 22.9 → 7.0 s
- [x] The transport bar's Out field is cut off at the default window
      size (Dev1, 0.12.19: labels wrap with their fields)
- [ ] Later: skip the target read-back during GPU playback, read back
      lazily for the readout / picker (~5–10 ms a 1080p frame)
- [ ] Macs: run `arclight gpu --check` and `cargo test --release` on the
      M4 Pro and the Intel Mac (Metal) before the next release
- [ ] Later (after GPU G3): a GPU-first, depth-aware **Defocus** node
      (none exists today)
- [ ] Not planned for now: particle simulation on the GPU
- [ ] Tile-based / region-of-interest processing for very large images
- [ ] Python plugins (custom nodes in Python)
- [ ] Distro packages (deb / rpm / AUR) and signed, notarized macOS
      builds (no right-click → Open; needs the Apple Developer Program)
- [ ] USD / Hydra
- [ ] RLE / 1-bit DPX (needs a sample file first)
- [ ] Windows release (on hold, user 2026-10-30): the lib and GUI already
      cross-compile here (`cargo zigbuild --target x86_64-pc-windows-gnu`);
      left: MCP + GUI bridge without Unix sockets (named pipe / localhost),
      %APPDATA% folders, .dll plugins (in-process), a zip asset; needs a
      real Windows PC for GUI testing; unsigned → SmartScreen warning

## After the phase 13 pause (user, 2026-10-31)

Phase 13 ends with a pause for the user's hand testing; these come
after it. `ROADMAP.md` is the public summary of this list.

- [X] **Detachable panels for dual monitors** (0.14.2, 0.14.4; the OS places windows; checked on the Mac by the user, 2026-10-31) (first UI item after the
      pause; Dev1): the Node Graph in its own OS window (egui immediate
      viewport: `editor.ui(ui, &mut doc, …)` drawn in it), docked back
      when closed, state and size in settings.json (Wayland: the
      compositor places it; X11 / macOS: position and monitor too);
      route graph shortcuts per window (⇧Tab, ⇧E, F, Delete, ⌘A, D, V,
      Tab, undo / redo / save), popups and file drops in that window.
      Built generic, so the Viewer (full screen on a second monitor),
      Curve Editor and Properties can detach too. About two reports.
- [X] **On the Mac first** (0.14.4): detach the Node Graph: it opens on
      screen, no flicker, takes clicks; Dock, closing it and View ▸ Dock
      all panels each put it back; its greyed tab in the main window
      docks it too
- [ ] Node Graph in its own window (0.14.2): View ▸ Node Graph in its
      own window (and ⧉): a second window shows the graph, the bottom
      panel keeps the Curve Editor; move it to the other monitor and
      resize. There: right-click / ⇧Tab / ⇧E browser, wire and node menus
      open there; F, ⌘A, ⌫, D, V, ⌘Z / ⇧⌘Z, ⌘S work; a node drag is one
      undo step. ⇧E in the main window opens the browser in the graph
      window and raises it. Drop an image on it: a Media In. Close it
      (or Dock): back in the panel. Quit with it out and restart: out
      again, same size (the OS places it). Edit ▸
      Preferences ▸ OK while it's out: it stays out
- [ ] Viewer / Curve Editor / Properties in their own windows (0.14.4):
      View ▸ Viewer in its own window: picture, toolbar and player there,
      graph and curves fill the main window; play, scrub, Tab, 3D orbit
      and G / R / S there. Full screen (button and F11) on the second
      monitor and back; restart: it reopens full screen. A GPU render
      (GPU on) and the 3D lit preview show in it. Curve Editor out: F
      frames, ⌫ deletes keys, a key click seeks. Properties out: edit,
      Look through, formula popups open there. All four out, then dock
      each back
- [ ] Import project (0.14.5): File ▸ Import project… on
      examples/geo-scatter.json into the starter project: nodes to the
      right, selected, framed; the menu bar names renames; ⌘Z removes
      them all, ⇧⌘Z brings them back; the render shows the rocks. Import
      a project from another folder with relative plates, save this one
      elsewhere, reopen: plates load. Drag a project .json onto the
      Node Graph (also in its own window): imported; onto the viewer:
      opened. Import the same project twice: the second copy renamed,
      its wires and links inside it. Rename a Locator used in an
      Attribute Set formula / a Curve's Path of: they follow
- [ ] Copy / paste (0.14.6): select nodes, ⌘C, ⌘V with the pointer over
      the graph: pasted there, renamed, selected; ⌘Z removes them. ⌘X
      then ⌘V moves them. ⌘D duplicates. A backdrop selected copies its
      nodes. Paste into another project (File ▸ New) and into a second
      running ArclightVFX: Reads still load. In the graph's own window
      too. ⌘C / ⌘V inside a text field stay the field's. Paste text
      from elsewhere: nothing happens
- [ ] Templates (0.14.7): select a few nodes, Insert ▸ Save selection as
      template… (a name; the same name offers Replace); Insert ▸
      Templates puts it in; ⇧Tab and type its name: it's listed after
      the nodes, Enter inserts it there; File ▸ New from template starts
      an untitled project; the examples are listed as built-in; Manage
      templates: rename, delete, Show the folder
- [X] **Import project, copy / paste, templates** (0.14.5–0.14.7) (phase 14, after the
      panels; user, 2026-10-31): File ▸ Import project… merges a saved
      .json into the current graph (nodes renamed on a clash with wires,
      links and expressions following; placed beside the work and
      selected; the project's format / fps / range kept; media paths
      rebased to this project's folder). ⌘C / ⌘X / ⌘V / ⌘D through the
      clipboard as the same JSON (between windows and instances).
      Templates: Save selection as template… into the user data folder;
      Insert ▸ Templates and the node browser; File ▸ New from
      template; Help ▸ Examples as built-in templates.
- [X] Project 3D under motion blur renders on the GPU (0.14.1); each
      projector's depth map is drawn once, not per time sample (0.13.38)
- [X] Project 3D occlusion on the GPU (0.14.3): an instanced copy straddling the
      projector's near plane isn't in its depth map (lights' maps split
      those out; projectors don't). Rare (Dev2)
- [ ] Project 3D targets are node names fixed at the node: a mesh renamed
      or merged under another node downstream would lose its projection.
      Checked 2026-10-31 (Dev2): none does today (`Scene::tag` only fills
      empty names; Subdivide, Displace, Normals, merge, Realize keep the
      node). Fix only if an op that renames or merges under a new node
      is added: carry projections on Card / Model as Instances do
      (`project_3d` pushes onto them; `render3d::attach` reads them).
- [ ] Particles to Points ids pack emitter (8 bits) and birth count (24
      bits): past ~16.7 M births ids wrap (only Instance on Points'
      random pick by id cares) (Dev2)
- [ ] Project 3D: bake a projection to a uv texture (P8's "later")
- [ ] Geo Info: per-point selection in the sheet's filter; world-space
      values
- [ ] Scanline Render's Width / Height are ignored when a background is
      connected (the plate's size wins): say so in help, or let them win
      (Dev1, 2026-10-31, from the geo-scatter example)
- [ ] Ellipse's Size is a radius but reads like a diameter: label or
      tooltip it (Dev1, 2026-10-31)

## Before going public

- [ ] Licence (MIT / Apache or GPL), a LICENSE and a NOTICES file
- [ ] Audit the repo for anything private (paths, emails in notes,
      `examples/mad-martian.png`, where `musicmen.MP4` came from)
- [ ] A public-facing README, issue templates, CONTRIBUTING (a
      `ROADMAP.md` with "How to help" exists since 0.13.34)
- [ ] Actions is free for public repos: the macOS DMG and wheels should
      build there again

## Housekeeping

- [x] Clippy clean across the workspace (0.10.29)

## Not planned (on purpose)

- OpenFX hosting (a large project of its own)
- Copying another product's licensing scheme
- Nuke script import (originality rule)

