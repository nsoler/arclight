# Phase 0.6 plan: formats and data windows

Status: agreed (session 6). The user chose the proposed option for each
decision **D1–D4** (2026-10-28); work milestone by milestone below.

## Problem

`Image` is `width × height` pixels from origin (0, 0). That one size serves
as both the *frame* (what the shot is: 1920×1080) and the *data* (where
pixels exist). Consequences:

- A small layer (a 200×100 logo) is either a 200×100 image pinned to the
  top-left corner of the frame, or a full-frame image that's mostly empty.
  Moving it with a Transform can't push pixels outside 0..width.
- Shapes, Constant and Checkerboard each carry their own `width`/`height`
  (default 512), so a comp mixes frame sizes by accident.
- Every op processes the whole frame even when the content is a small
  shape: a 4K roto of a 50 px dot fills 33 M floats per op.
- Overlays use the *viewed* image's size for the node's input (Transform
  pivot and layer frame, Tracker boxes, the Transform export's pivot).
- Crop's "shrink canvas" and Resize have to invent a new top-left origin.

## Model

```rust
pub struct Image {
    /// The frame: what a Viewer shows and a Write saves, from (0, 0).
    pub format: Size,              // { width, height }
    /// Where pixels are stored, in frame pixels (y down). May lie partly or
    /// wholly outside the frame (overscan), or be empty.
    pub window: IRect,             // { x0, y0, x1, y1 }, half-open
    /// window.width() * window.height() pixels, row-major from (x0, y0).
    pub pixels: Vec<Rgba>,
}
```

- Everything outside `window` is transparent black. `get(x, y)` and
  `sample_bilinear` take frame coordinates and return transparent outside.
- Coordinates stay as today: frame pixels, y down, pixel `i` covers
  `[i, i+1)`. Shapes, tracks and corner pins keep their meaning.
- `Image::full(format)` is today's behavior (window = frame);
  `flatten()` returns a window = frame copy (cropping overscan) for code
  that wants a plain buffer.
- As built (milestone 1): `width`/`height` stayed, meaning the *data
  window's* size (what `pixels` holds), with `x0`/`y0` for its corner and
  `format` for the frame, so existing code kept working while windows
  equal the frame. `get`/`set` index the window; `at(x, y)` and
  `sample_bilinear` take frame coordinates. Milestones 2–3 review each op's
  use of `width`/`height`/`get` (frame vs window) as they make it
  window-aware.

## Per-op rules

| Ops | Output window | Output format |
|---|---|---|
| Constant, Checkerboard, Media In, Read | frame | own size |
| Rectangle, Ellipse, Polygon, Roto | shape bounds + feather (∪ background window) | background's, else own size |
| Grade, Exposure, Saturation, Invert, Clamp, Premult, Unpremult, Channel Select | input window, grown to the frame when the op turns transparent black into something else (checked generically by applying it to one transparent pixel; e.g. Invert on alpha) | input's |
| Merge, Dissolve, Keymix | union of inputs (Merge `in`/`mask`-type ops could intersect; union is always correct) | B's |
| Switch, Viewer, Media Out | input's | input's |
| Transform, Tracker output, Corner Pin | input window's corners mapped, rounded out (see **D1**) | input's |
| Blur | input window grown by the radius | input's |
| Crop "clear outside" | input window ∩ box | input's |
| Crop "shrink canvas" | window ∩ box, shifted by (−left, −top) | box size |
| Resize | window scaled and offset like the content | new size |
| generic mask/mix | ∪ of base window and result window | base's |

Pivots stay at the **frame** center (Transform rotates/scales about
format/2, as today), so existing projects and the Tracker → Transform
export don't change.

## Boundaries

- **Viewer**: draws the frame as today, plus the data window as a dashed
  rectangle when it differs; pixels outside the frame (overscan) are drawn
  too, dimmed. Pixel readout uses frame coordinates.
- **Overlays**: take the viewed image's *format* instead of its size (same
  as today for full-frame images). Transform's layer frame should outline
  the *input's* data window, which the GUI doesn't have: the worker would
  report the selected node's input window alongside the rendered frame.
- **Write / Media Out / render_frame `path`**: save the frame (flatten).
- **Python `pixels()` / `to_numpy`**: the frame (flatten); `render_frame`
  stats gain `format` and `window`.
- **Plugins**: see **D3**.
- **Tracker**: matches on the flattened frame (as today).
- **Cache**: smaller windows mean less memory per cached frame; no other
  change.

## Decisions (all as proposed)

- **D1 Overscan.** Keep pixels a Transform pushes outside the frame, so a
  later Transform can bring them back (proposed), capped to the frame
  padded by one frame size on each side so a 100× scale can't allocate
  gigabytes. Alternative: clip every window to the frame (simpler, but a
  move-out-and-back-in loses pixels).
- **D2 Project format.** Add `project.format` (default 1920×1080). Sources
  without an input (shapes, Constant, Checkerboard) use it unless their
  own width/height is set; new nodes stop getting 512. Old project files
  keep their explicit sizes. Alternative: keep per-node sizes only.
- **D3 Plugin ABI.** Keep ABI v1 by flattening to the frame before calling
  a plugin and treating its output as full-frame (proposed; no plugin
  changes, loses the speedup for plugin ops). Alternative: ABI v2 with
  windows, v1 still accepted.
- **D4 Order of work** as below, one milestone each, tests green at every
  step.

## Milestones

1. **Done (session 6).** **Data model, no behavior change.** New `Image` (format, window),
   accessors, `full`, `flatten`. Every op still outputs window = frame.
   Boundaries (viewer, I/O, Python, plugins, tracker) go through `flatten`.
   All existing tests pass unchanged; add model tests.
2. **Done (session 6).** **Windows that shrink.** (Dissolve takes B's
   frame too. Nodes still waiting for milestone 3 flatten their input.) Shapes (tight bounds), Crop, mask/mix and
   pixel ops honor windows; Merge takes the union. Tests: a 4K shape's
   window is its bounds; merge of two small layers; alpha Invert grows
   to the frame.
3. **Done (session 6).** **Windows that move.** (Blur grows only on
   sides inside the frame; where data meets the frame edge it keeps
   clamping, so plates blur as before.) Transform, Tracker output, Corner Pin, Blur,
   Resize, Crop "shrink canvas"; overscan per **D1**. Tests: move a layer
   out and back; blur grows the window; resize scales it.
4. **Done (session 6).** **Viewer and overlays.** Data-window outline and overscan in the
   viewer; overlays use the format; the worker reports the selected
   node's input window for Transform's layer frame. Screenshot checks.
5. **Done (session 6).** **Project format (D2)** and stats in MCP/Python (`format`, `window`).
6. **Measure**: time a 4K comp of small shapes before/after; note results
   in HANDOFF.
