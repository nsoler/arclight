# Changelog

Arclight is alpha software: versions are `0.0.x`, and every feature or
change that ships bumps the last number (0.0.15, 0.0.16, …). The version is
set in `Cargo.toml` (also `python/Cargo.toml` and
`plugins/example/Cargo.toml`, kept in step) and shows in the GUI's Help
menu, `arclight --version`, the MCP server info and Python's
`arclt.__version__`.

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
