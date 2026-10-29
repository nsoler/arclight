# Changelog

Arclight is alpha software: versions are `0.0.x`, and every feature or
change that ships bumps the last number (0.0.15, 0.0.16, …). The version is
set in `Cargo.toml` (also `python/Cargo.toml` and
`plugins/example/Cargo.toml`, kept in step) and shows in the GUI's Help
menu, `arclight --version`, the MCP server info and Python's
`arclt.__version__`.

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
