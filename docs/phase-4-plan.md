# Phase 4 plan: 3D and the camera tracker

Status: **agreed** (2026-10-29). The user chose the proposed option for
D1–D4; D5 (viewport) and D6 (order) stand as proposed until milestone 9.
Nothing built yet. The work goes one milestone at a time, as in
`phase-0.6-plan.md`.

## Goal

Two features, and the engine work they need:

1. **A 3D scene you can render:** a Camera (focal length, film back,
   position, rotation, all animatable), Cards (an image on a plane in
   3D), Points, a Scene node that gathers them, and a Scanline Render op
   that draws the scene through a camera into an image with depth.
2. **A 3D camera tracker:** from a plate, work out the camera's path and
   a cloud of 3D points, so CG cards can be locked onto the shot. Output:
   an animated Camera node and a point cloud for the scene.

The tracker is the hard part; the scene system is what makes its result
usable, so it comes first.

## What exists to build on

- `Image`: RGBA float, linear premultiplied, with a frame (`format`) and
  a data window (Phase 0.6). No channels other than RGBA.
- Every node outputs one `Image`; ports have no types.
- `track.rs`: NCC template matching with sub-pixel refinement, affine /
  perspective pattern warps, 2×2 and 3×3 solves (similarity, affine,
  homography), its own small `Pt` / `Mat` types. It follows boxes the
  user places; it doesn't find features by itself.
- `link.rs`: live links (a param read from another node every frame),
  which a Card or Scanline Render can use to follow a solved camera.
- `anim.rs`: keyframes; a baked camera is keys on its params.

## The model

### Extra planes on `Image` (the "arbitrary channels" prep)

```rust
pub struct Image {
    pub format: Size,
    pub x0: i32, pub y0: i32, pub width: usize, pub height: usize,
    pub pixels: Vec<Rgba>,          // unchanged: color + alpha
    /// Named extra planes over the same data window, one f32 per pixel:
    /// "depth", later "motion.x", "motion.y", "id", ... Empty for most
    /// images.
    pub planes: Vec<Plane>,
}
pub struct Plane { pub name: String, pub data: Vec<f32> }
```

RGBA stays a fixed `[f32; 4]` (the fast path every op uses today); other
channels ride along beside it (**D1**). Rules:

- Ops that move pixels (Transform, Corner Pin, Crop, Resize) move planes
  the same way (nearest sample for `depth` and `id`: blending depths is
  meaningless).
- Color ops (Grade, Saturation, ...) pass planes through untouched.
- Merge: `over` keeps B's planes outside A and A's where A is opaque
  (alpha > 0.5); a depth-aware merge (`zmerge`, nearest wins) is a new
  op mode.
- Ops that don't know about planes drop them (safe: the plane is just
  missing downstream). The viewer can show a plane as gray (min/max
  normalized), like it shows one channel today.
- EXR reads and writes extra channels (`Z` ↔ `depth`, others by name).
  PNG / video / DPX ignore them. Plugins (ABI v1) see RGBA only.
- Cache: planes count toward the frame's memory.

### Scene data on the graph

A node outputs `Output::Image(Image)` or `Output::Scene(Arc<Scene>)`
(**D2**). Ports get a kind (`image` / `scene` / `camera`); the node editor
refuses a wire between different kinds and colors them apart.

```rust
pub struct Scene {
    pub cameras: Vec<Camera>,       // usually one
    pub cards: Vec<Card>,
    pub points: Vec<Point3>,        // a point cloud (tracker output), drawn as dots
}
pub struct Camera {
    pub focal: f64,                 // mm
    pub film_back: [f64; 2],        // mm (default 36 × 24)
    pub world: Mat4,                // camera-to-world: translate, rotate (XYZ order), scale
    pub near: f64, pub far: f64,
    pub distortion: [f64; 2],       // k1, k2 (Brown radial), applied by Lens Distort, not by the renderer
}
pub struct Card {
    pub image: Image,               // the texture (the Card node's input)
    pub world: Mat4,
    pub size: [f64; 2],             // world units; default keeps the image aspect, width 1
}
```

Coordinates: right-handed, Y up, camera looks down −Z (the common
convention in 3D tools and in OpenGL). Units are arbitrary ("scene
units"); the camera tracker's scale is arbitrary too and gets set from
a known distance.

New nodes (category **3D**):

| Node | Inputs | Output | Params |
|---|---|---|---|
| Camera | – | camera | focal, film back, translate, rotate, scale, near, far, k1, k2 |
| Card | image | scene | translate, rotate, scale, size, ... |
| Scene | scene × N (optional) | scene | – |
| Scanline Render | scene, camera, background (optional image) | image (+ depth) | format, samples (anti-aliasing), motion blur (later) |
| Lens Distort | image | image | k1, k2, center, mode (distort / undistort); 2D, but uses the camera model |
| Camera Tracker | image | scene (camera + points) | features, min distance, solve settings, focal (known or solved) |

The viewer gets a 3D view toggle later (**M6**); before that, the way to
look at a scene is through Scanline Render.

### The renderer

A scanline / triangle rasterizer on the CPU (rayon over rows, like the
other ops): each card is two triangles; vertices go through
world → camera → projection → frame pixels; perspective-correct UVs
sample the card's image bilinearly (mipmaps later); a depth buffer keeps
the nearest; edges are anti-aliased by supersampling (`samples`).
Output: premultiplied RGBA over the optional background, plus a `depth`
plane (camera-space distance, 0 where empty). Points draw as small dots.

### The camera tracker

Built from standard, published methods (originality rule):

1. **Features:** Shi–Tomasi corners (minimum eigenvalue of the structure
   tensor) on a luma pyramid, spread out by a minimum distance, topped
   up as tracks are lost.
2. **Tracking:** pyramidal Lucas–Kanade (KLT) frame to frame, with a
   forward–backward check to drop drifting tracks. `track.rs`'s NCC
   matcher stays for the 2D Tracker; KLT is the better fit for hundreds
   of small features.
3. **Initialization:** pick two keyframes far enough apart (enough
   parallax: the homography fits worse than the fundamental matrix);
   8-point fundamental matrix in RANSAC (Hartley normalization) →
   essential matrix with the focal → the four pose candidates, keep the
   one with points in front of both cameras → triangulate (linear DLT).
4. **Growing:** for each further frame, camera pose from known 3D points
   (PnP: DLT + refinement, in RANSAC), triangulate new points, then
   **bundle adjustment**: Levenberg–Marquardt over all poses and points
   minimizing reprojection error, with the Schur complement so the
   point blocks solve cheaply; a robust (Huber) loss for outliers.
5. **Focal length:** known (from the user) by default; optionally solved
   as one extra parameter in the bundle adjustment (needs real camera
   motion to be well-defined). Lens distortion k1 optionally solved too.
6. **Output:** the camera per frame (baked into an animated Camera node,
   or a live link), the point cloud, per-frame and per-point error, and
   "set origin / ground plane / scale" tools: pick three points for the
   ground, two for a known distance.

Performance target: 1080p, ~200 features, 200 frames in under a minute
on this machine (KLT dominates; bundle adjustment on keyframes only,
then poses of in-between frames by PnP and a final pass).

## Decisions (D1–D4 as proposed, chosen 2026-10-29)

- **D1 Channels.** *Proposed:* keep RGBA as `[f32; 4]` and add named
  extra planes beside it (above). Small change, every op keeps its fast
  path, planes cost nothing when absent. *Alternative:* fully general
  channel lists (`Vec<Plane>` for everything, RGBA included): cleaner on
  paper, but touches every op (`pixels` appears ~170 times) and the
  plugin ABI, and slows the common case.
- **D2 Scene data.** *Proposed:* typed node outputs (`Image` or `Scene`)
  and typed ports, so 3D nodes are wired like the rest of the graph.
  *Alternative:* no new data type; Scanline Render names its camera and
  cards in params (like links). Less engine work, but a scene isn't
  visible as wires and can't be gathered or reused.
- **D3 Maths library.** *Proposed:* add `nalgebra` (pure Rust, MIT /
  Apache, widely used) for 3×3 / 4×4 / SVD / small dense solves; write
  Levenberg–Marquardt and the Schur step ourselves on top (≈ 300 lines,
  and we control the sparse structure). *Alternative:* no new
  dependency: extend `track.rs`'s own `Pt` / `Mat` with a hand-written
  SVD (Jacobi) and solver. More code to get right; no gain.
- **D4 Tracker scope for a first version.** *Proposed:* free camera
  moving through a static scene, known focal length or solved focal, no
  distortion solve, one shot. Later: nodal (tripod) solve, distortion,
  masks to ignore moving objects (a mask input), user-placed 2D tracks
  as extra constraints. *Alternative:* aim for all of it in v1.
- **D5 3D viewport.** *Proposed:* later (after the tracker), as a viewer
  mode drawing the scene, the camera path and the point cloud through a
  free orbit camera with the same CPU renderer. *Alternative:* a GPU
  viewport (wgpu, which eframe already ships) early: faster, but a second
  renderer to keep in step.
- **D6 Order of work** as below, one milestone each, tests green and
  clippy clean at every step.

## Milestones

1. **Extra planes on `Image` (D1).** `planes`, accessors, `plane(name)`;
   Transform / Corner Pin / Crop / Resize / Merge carry them; EXR `Z`
   and named channels in and out; viewer shows a chosen plane; MCP /
   Python stats list planes. Tests: EXR round trip with depth; a
   transformed depth plane moves with its color; color ops keep it.
2. **Camera maths and Lens Distort.** `src/geom.rs`: camera model
   (world matrix from translate / rotate / scale, projection, film back
   ↔ focal ↔ field of view), Brown radial distortion and its inverse.
   Lens Distort op (2D, useful on its own). Tests: project / unproject
   round trips; distort ∘ undistort ≈ identity; a known grid.
3. **Scene data and the 3D nodes (D2).** `Output` enum, port kinds,
   node editor wire kinds; Camera, Card, Scene nodes; scenes in the
   cache. Tests: a scene's contents; refused wires; save / load.
4. **Scanline Render.** Rasterizer, textures, depth plane, background,
   anti-aliasing. Tests: a card facing the camera at the distance that
   makes it fill the frame renders like the image; depth equals that
   distance; perspective (a tilted card's near edge is wider); two
   overlapping cards sort by depth.
5. **Features and KLT.** Shi–Tomasi on a pyramid, pyramidal LK, forward–
   backward check. Tests on synthetic renders (Scanline Render of
   textured cards through a known moving camera: ground truth for free).
6. **Two-view initialization and triangulation.** Fundamental /
   essential matrix in RANSAC, pose recovery, DLT triangulation. Tests:
   known synthetic camera pair recovered up to scale.
7. **Incremental solve and bundle adjustment.** PnP, LM with Schur,
   Huber loss, optional focal. Tests: synthetic shot solved to sub-pixel
   reprojection error and camera path matching ground truth (after a
   similarity alignment); outliers rejected.
8. **Camera Tracker node.** Params, progress and Cancel in the GUI (like
   the 2D tracker), output scene, bake to a Camera node or link,
   origin / ground / scale tools, error display; MCP and Python
   (`track_camera`, `export_camera`). Test: the synthetic shot through
   the node, then a card placed on a solved point renders locked to the
   plate.
9. **3D viewport (D5)** and the remaining follow-ups (nodal solve,
   distortion solve, mask input, motion blur in Scanline Render).
