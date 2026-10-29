# Phase 4 plan: 3D and the camera tracker

Status: **agreed** (2026-10-29). The user chose the proposed option for
D1–D4; D5 (viewport) and D6 (order) stand as proposed until milestone 9.
Milestones 1–8 done. The work goes one milestone at a time, as in
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
  renderer to keep in step. (2026-10-29: the full M9 spec, with
  navigation, selection and gizmos, is under "Milestone 9 in detail";
  it keeps the CPU renderer for textures and draws overlays and gizmos
  as vectors.)
- **D6 Order of work** as below, one milestone each, tests green and
  clippy clean at every step.

## Milestones

1. **Done (2026-10-29).** **Extra planes on `Image` (D1).** (As built:
   warps, Crop, Resize, Merge and motion blur carry planes; color ops,
   Keyer, Blur, shapes over a background and Keymix keep their input's;
   Dissolve takes the nearer input's; plugins and sources drop them.
   `Image::plane_stats` for MCP / Python stats; zmerge waits for M4.) `planes`, accessors, `plane(name)`;
   Transform / Corner Pin / Crop / Resize / Merge carry them; EXR `Z`
   and named channels in and out; viewer shows a chosen plane; MCP /
   Python stats list planes. Tests: EXR round trip with depth; a
   transformed depth plane moves with its color; color ops keep it.
2. **Done (2026-10-29).** **Camera maths and Lens Distort.** (As built:
   the film back's width spans the frame's width, pixels square;
   distortion r is in half frame widths; Lens Distort has undistort /
   distort modes, K1, K2 and a center, a mask and mix.) `src/geom.rs`: camera model
   (world matrix from translate / rotate / scale, projection, film back
   ↔ focal ↔ field of view), Brown radial distortion and its inverse.
   Lens Distort op (2D, useful on its own). Tests: project / unproject
   round trips; distort ∘ undistort ≈ identity; a known grid.
3. **Done (2026-10-29).** **Scene data and the 3D nodes (D2).** (As
   built: no `Output` enum. A scene rides on the graph's existing value
   as `Image::scene` (an image with no pixels), so the cache, worker,
   viewer, stats and plugin calls needed no change; the wires are typed
   all the same: `Port::kind` (image / scene), `ops::output_kind`,
   `ops::wire_error`, checked by the graph at evaluation, by
   `Session::connect` and by the node editor, which draws scene wires and
   ports blue. A Camera outputs a scene holding the camera, so there are
   two kinds, not three. Camera k1/k2 wait for the tracker. `Image::bytes`
   counts card textures for the frame cache.) `Output` enum, port kinds,
   node editor wire kinds; Camera, Card, Scene nodes; scenes in the
   cache. Tests: a scene's contents; refused wires; save / load.
4. **Done (2026-10-29).** **Scanline Render.** (As built:
   `src/render3d.rs`; per-pixel sample grid over the triangles in each
   row (no z-buffer memory), cards composited front to back so their
   alpha works, texture lookups clamped to the card image's frame (sharp
   card edges, softened by the samples), no mipmaps yet; ports scene /
   camera (else the scene's first camera) / background; 1080p with three
   cards at 3×3 samples renders in ~0.1 s. zmerge still to do.) Rasterizer, textures, depth plane, background,
   anti-aliasing. Tests: a card facing the camera at the distance that
   makes it fill the frame renders like the image; depth equals that
   distance; perspective (a tilted card's near edge is wider); two
   overlapping cards sort by depth.
5. **Done (2026-10-29).** **Features and KLT.** (As built:
   `src/features.rs`: Gray, Pyramid, detect (Shi–Tomasi), follow /
   follow_checked (pyramidal LK, forward–backward), affine_refine, and
   FeatureTracker (top-up below 90 %). The drift first seen here was not
   a projection mismatch (a sideways camera move tracks to 1.0002 of the
   true motion): a shift-only window can't follow content that stretches
   (toward the edges of a pan, all over in a push-in), so each step
   landed slightly off and the error added up. Each step is now refined
   against the feature's first frame with an affine window warp:
   0.015 px median, 0.12 px at the 95th percentile over the test shot,
   down from 0.22 / 0.94.) Shi–Tomasi on a pyramid, pyramidal LK, forward–
   backward check. Tests on synthetic renders (Scanline Render of
   textured cards through a known moving camera: ground truth for free).
6. **Done (2026-10-29).** **Two-view initialization and triangulation.**
   (As built: `src/solve.rs`, computer-vision camera frame inside
   (`Pose`, `Intrinsics`, `Pose::world` / `from_world` to `geom`);
   eight-point essential matrix on normalized rays in adaptive RANSAC
   (Sampson distance), refit on inliers, decomposition chosen by points
   in front, DLT triangulation, `initial_pair` scanning forward for
   parallax.) Fundamental /
   essential matrix in RANSAC, pose recovery, DLT triangulation. Tests:
   known synthetic camera pair recovered up to scale.
7. **Done (2026-10-29).** **Incremental solve and bundle adjustment.**
   (As built: `src/bundle.rs` (Levenberg–Marquardt, Huber loss, Schur
   complement onto the cameras, left-multiplied rotation updates with
   renormalization, optional focal, cameras-only mode) and `src/sfm.rs`
   (start pair, frames added nearest-first from a constant-velocity
   guess refined cameras-only with outlier tracks skipped, triangulation
   past a parallax threshold, periodic and final full adjustment,
   outlier points dropped, per-frame RMS, cancel via the progress
   callback; `similarity` (Umeyama)). No PnP: each frame starts from its
   neighbour, which suits video and avoids PnP's trouble with flat
   scenes. Tested on a synthetic 40-frame shot with noise and outlier
   tracks, with the focal solved from 10 % off, and end to end on a
   rendered, tracked shot.) PnP, LM with Schur,
   Huber loss, optional focal. Tests: synthetic shot solved to sub-pixel
   reprojection error and camera path matching ground truth (after a
   similarity alignment); outliers rejected.
8. **Done (2026-10-29).** **Camera Tracker node.** (As built:
   `src/camtrack.rs` (Settings, run, Solution kept in the node's
   `solution` param in scene terms with per-frame Camera translate /
   rotate, set_origin / set_ground / set_scale, fit_plane,
   card_on_points, camera_params); ops `camera_tracker` (passes its plate
   through), `point_cloud`, `locator`; Session / MCP `track_camera`,
   `camera_points`, `orient_camera`, `export_camera`, `place_on_points`
   and the same in Python; GUI `src/gui/camera_tracker.rs` (panel with
   Track & Solve on a thread, progress, Stop, summary, Export, Clear
   solve; viewer point picker over the Camera Tracker with click,
   ⇧-click and box selection and a floating panel: Place Card here,
   Place locator, Set origin, Ground plane, Set scale). Export bakes keys
   (a live link to the solve is a follow-up), and picking works over the
   Camera Tracker, not yet over a Scanline Render of its scene. Locked-card
   test: placed on solved floor points, a card stays within 1 px of its
   floor spot over 24 frames and renders there over the plate.)
    Params, progress and Cancel in the GUI (like
   the 2D tracker), output scene, bake to a Camera node or link,
   origin / ground / scale tools, error display; MCP and Python
   (`track_camera`, `export_camera`). Test: the synthetic shot through
   the node, then a card placed on a solved point renders locked to the
   plate.
   **Point placement (added 2026-10-29, user request).** In the viewer,
   over the Camera Tracker's output or a Scanline Render of its scene,
   solved points can be selected (click, ⇧-click to add, drag a box) and
   used to place things at their depth: **Place Card here** (a Card at
   the selection's centroid, lying on the plane fitted to three or more
   points, else facing the camera; sized to the selection's spread) and
   **Place locator** (a point the user can wire to other nodes or link
   params to). The same through MCP / Python (`place_on_points`). Test: a
   card placed on solved points renders locked to the plate.
9. **3D viewport (D5)**, specified below, and the remaining follow-ups
   (distortion solve, mask input, motion blur in Scanline Render; the
   nodal solve is now part of M11).
10. **3D objects and render passes (AOVs)**, specified below (added
    2026-10-29, user request): geometry beyond cards (primitives and
    imported meshes), simple lights and shading, and Scanline Render
    writing render passes as image planes.
11. **Camera solve: PnP and the tripod solve**, specified below (added
    2026-10-29, user request): M7 left these out. PnP places a frame's
    camera from 3D points it sees, so solves survive jumps, gaps and
    restarts; the tripod (nodal) solve handles shots where the camera
    only turns, which the solver refuses today ("the camera may only be
    turning, not moving"). Suggested before M10, since it makes the
    tracker work on more real shots.
12. **Lights: area lights, lights in the 3D view, and 2D lights**,
    specified below (added 2026-10-29, user request): builds on M10's
    lights (Ambient, Directional, Point, Spot) with Area lights, lights as
    objects the 3D view can select and move with G / R / S and gizmos, and
    a 2D Light node for lighting flat images.
13. **Particles**, specified below (added 2026-10-29, user request):
    emitters, particle simulation, and particles drawn as sprites, cards,
    primitives or meshes from a file, in 3D scenes and in 2D. After M12,
    so particles can be lit.

## Milestone 9 in detail: the 3D viewport

Specified 2026-10-29 at the user's request. **Done (0.9.1–0.9.6)** on
branch `wip-ns-3dwork`, with Blender-style controls. Leftovers: a true
orthographic texture path (ortho views use a 1000×-away long lens), the
pivot at a multi-selection's median, transforming several objects at once.

**What it is.** A mode of the existing Viewer panel (a **2D / 3D**
toggle in the viewer toolbar, and **Tab** to switch, only while the
pointer is over the Viewer panel, so Tab keeps its usual job elsewhere:
the node graph, the Properties fields, text entry; user request,
2026-10-29) that shows the
scene of the node being viewed, if it outputs or renders one (Camera,
Card, Scene, Camera Tracker, Scanline Render: its scene input), from a
free *viewport camera* that belongs to the viewer, not to the graph.
Switching back to 2D shows the image as now. The viewport camera is
saved per project (like the viewer's zoom), not as a node.

**Navigation: Blender-style** (user request, 2026-10-29, when M9
started on branch `wip-ns-3dwork`). The controls follow Blender's
defaults so they feel familiar; only the conventions are borrowed (no
Blender code: it's GPL). Arclight's world is Y-up, so "top" looks down
−Y where Blender's looks down −Z.
- **Orbit:** middle-drag (turntable: yaw about world Y, pitch clamped
  short of the poles). **Pan:** ⇧ + middle-drag. **Zoom:** scroll, or
  ⌃/Ctrl + middle-drag; toward the point under the pointer.
- **Trackpads** (the user's Macs): two-finger scroll orbits, ⇧ +
  two-finger scroll pans, pinch or ⌃/Ctrl + two-finger scroll zooms, as
  Blender does on macOS.
- **Emulate 3-button mouse** (a viewer setting, off by default): Alt +
  left-drag orbits, ⇧ + Alt + left-drag pans, ⌃/Ctrl + Alt + left-drag
  zooms (Blender's option of the same name; also how Alt-drag worked in
  the earlier draft of this spec).
- **Numpad views:** 1 front, 3 right, 7 top; ⌃/Ctrl + 1 / 3 / 7 the
  opposite sides; 5 toggles perspective / orthographic; 0 looks through
  the scene camera (the first, or the one chosen in the menu); 2 / 4 / 6
  / 8 orbit in 15° steps; numpad . frames the selection; **Home** frames
  everything. Without a numpad: the same views from the view menu and
  the navigation gizmo, and **`** (backtick) opens a pie of views as in
  Blender.
- **Navigation gizmo** in the top-right corner, as in Blender: an axis
  ball (X red, Y green, Z blue; click an axis to view along it, drag the
  ball to orbit), and buttons under it: hand (drag to pan), magnifier
  (drag to zoom), camera (look through the scene camera), grid
  (perspective / orthographic).
- **Look through a camera:** numpad 0 or the camera button / menu
  ("Viewport", "camera1", …). Through a camera the view is exactly that
  camera's (what Scanline Render would draw, with its film-back frame
  outlined, the outside darkened); orbiting leaves it for the free
  viewport camera.
- **F** also frames the selection (Arclight's existing key, kept beside
  numpad .).

**What it draws.**
- Cards textured with their image (the CPU renderer, D5), plus their
  outlines; a ground grid on world XZ with the axes colored (X red, Y
  green, Z blue).
- Cameras as wireframe frusta (film-back aspect, focal length shown by
  the frustum's depth), the animated camera's **path** as a polyline
  with frame ticks, and the current frame's camera highlighted.
- The tracker's **point cloud** as dots, colored by reprojection error
  (green good → red poor) or by the color sampled from the plate.
- Locators as small axes.
- The current frame follows the timeline, so scrubbing moves animated
  cameras and cards.

**Selection** (Blender-style):
- Left-click selects a card, camera, locator or point; ⇧-click adds or
  removes; left-drag on empty space box-selects (points and objects whose
  centers fall inside); **A** selects all, **Alt+A** deselects all,
  **Esc** cancels a gesture or deselects.
- Picking is done in screen space: points and object centers projected
  through the viewport camera, cards by ray-quad intersection with the
  nearest hit winning, so it stays exact and cheap.
- Selecting an object in the viewport selects its node in the node graph
  and shows its params in the Properties panel, and the other way round
  (one selection model, as the 2D viewer's handles already share).
- Selected points feed M8's **Place Card here / Place locator**, also
  offered in the viewport's right-click menu.

**Transforming: Blender-style keys plus gizmos.**
- **G** grab (move), **R** rotate, **S** scale the selection as a modal
  gesture, as in Blender: the selection follows the mouse; then **X /
  Y / Z** constrain to that world axis (press twice for the local
  axis), **⇧ + X / Y / Z** excludes an axis (moves in the other two);
  typing a number sets the amount exactly (e.g. `G X 2 Enter`,
  `R Z 90 Enter`, `S 0.5 Enter`; `-` negates); ⌃/Ctrl snaps (1 unit,
  15°, 0.1 scale); **left-click or Enter** confirms, **right-click or
  Esc** cancels and restores. **Alt+G / Alt+R / Alt+S** reset
  translate / rotate / scale.
- **Gizmos** on the selected object (drawn over the render as vectors,
  so they stay crisp and fast), switched from the viewer toolbar (Move,
  Rotate, Scale, or all three, like Blender's toolbar tools):
  - Move: three axis arrows, three plane squares, and a center circle
    (drag in the view plane).
  - Rotate: three axis rings plus an outer view-axis ring and a
    trackball area inside.
  - Scale: axis handles with cubes, the center for uniform scale.
  - ⌃/Ctrl snaps while dragging, as with G / R / S.
- **Local / Global** orientation toggle in the toolbar; the pivot is
  the object's origin (later: median point of a multi-selection, as
  Blender's pivot menu).
- Gizmo drags and G / R / S edit the node's translate / rotate / scale
  params, set keys when the param is animated (as 2D handles do), and
  are one undo step per gesture. Linked params (e.g. a Card linked to
  tracker points) show the gizmo grayed with a tooltip naming the link,
  and G / R / S say why they can't move it; Unlink makes them editable.
- Candidate crate: `transform-gizmo-egui` (MIT/Apache, egui gizmos for
  move/rotate/scale); check that it supports egui 0.37 and draws in our
  viewport before using it; otherwise write the gizmos ourselves.

**How it's built.**
- A new `src/gui/viewport3d.rs`: the viewport camera (orbit center,
  yaw, pitch, distance, ortho flag), navigation, picking and gizmo
  hit-testing; drawing the grid, frusta, paths, points, locators and
  gizmos with egui's painter, projected through `geom::Camera`.
- Cards textured by `render3d::render` of the scene through the viewport
  camera, on the worker thread like other frames, at reduced resolution
  while the view moves (half, then quarter if a frame takes > 30 ms) and
  at full resolution once it stops. **D5 stays as proposed (CPU
  renderer)**; if orbiting large scenes is too slow, a GPU path (wgpu,
  which eframe ships) for the textured cards is the fallback, with the
  vector overlays unchanged.
- Gizmo drags go through `Doc` / `Session` edits, so undo, keys, MCP and
  the bridge see them like any other param change.
- MCP / Python: `viewport_set` (camera, view, look-through) and
  `select_in_viewport` are not needed for scripting (scripts set params
  directly); the viewport is GUI only.

**Tests.**
- Picking: a click at a projected point selects that point; ray-card
  hits pick the nearer of two overlapping cards; box selection.
- Gizmo maths: a drag along the X arrow changes only translate.x by the
  dragged distance at the object's depth; rotate rings and snapping;
  scale about the origin; local vs world axes.
- Navigation: orbit keeps the orbit center fixed on screen; dolly toward
  the pointer keeps the point under it fixed; F frames a selection.
- Look-through: the viewport through camera1 renders exactly like
  Scanline Render through camera1 (pixel-identical at full resolution).
- Round trip: a gizmo drag then undo restores the params; on an animated
  param it sets a key at the current frame.
- Screenshot checks of the GUI (orbit, gizmos, point cloud, look
  through) with `ARCLIGHT_SCREENSHOT`.

**Order inside M9:** (a) viewport camera, navigation, grid, frusta and
points, look-through; (b) picking and shared selection; (c) textured
cards via the CPU renderer with resolution fallback; (d) move / rotate /
scale gizmos with keys and undo; (e) camera path, error coloring, view
cube, ortho views.

## Milestone 10 in detail: 3D objects and render passes (AOVs)

Specified 2026-10-29 at the user's request: when 3D objects come into a
scene, Scanline Render can write **render passes**, also called AOVs
(arbitrary output variables), so each part of the render can be graded
and composited on its own. Built after M9 (the viewport shows objects
and passes too), in a later session.

**3D objects.**
- **Primitives** (new nodes in the 3D category): Cube, Sphere,
  Cylinder, Plane (subdivided), each with translate / rotate / scale like
  Card, a size and segment counts, and an optional texture input.
- **Read Geo:** imports meshes. OBJ first (with its MTL colors and
  texture paths, UVs and normals); glTF 2.0 next (open, carries
  hierarchy, materials and animation); Alembic later for animated caches.
  Its own translate / rotate / scale on top of the file's.
- **Scene data:** `Scene` gains `meshes: Vec<Mesh>` (positions, normals,
  UVs, triangle indices, a world matrix, a material, a name and an
  object ID); a Card becomes a two-triangle mesh internally, so one
  render path draws both.
- **Materials:** Unlit (the default, as cards render today), Lambert and
  Phong/Blinn (color or texture, specular amount and roughness),
  emission. **Lights** (new nodes, scenes like the rest): Ambient,
  Directional, Point, Spot (color, intensity, falloff); without lights
  the render stays unlit, so existing comps don't change.
- **Per-object flags:** visible to the camera, holdout (renders nothing
  but cuts out what is behind it, with alpha 0), casts / receives
  shadows (shadows are a later follow-up), and included in mattes.

**Render passes (AOVs).** Scanline Render gets a **Passes** list
(checkboxes in the panel, a list param for scripts); each chosen pass
becomes a named plane on the output image (D1's planes), next to RGBA,
which is always the beauty:
- `depth`: distance in front of the camera (exists today).
- `P` (world position, xyz), `N` (world normal, xyz), `uv` (texture
  coordinates): for relighting and texture work in 2D.
- `motion` (screen-space vectors to the next frame, from the camera's
  and objects' animation): for vector motion blur in 2D.
- `id` (an object ID per pixel) and **mattes**: one `matte.<node>`
  plane per object (or per chosen object), antialiased coverage, so any
  object can be isolated without re-rendering.
- Lighting breakdown, once lights exist: `diffuse`, `specular`,
  `emission`, `ambient` (each an RGB plane), adding up to the beauty;
  per-light passes (`light.<node>`) as an option.
- Each pass is filtered the way it should be: coverage-weighted for
  color passes and mattes, nearest sample for `depth`, `P`, `N`, `id`
  (averaging positions across an edge would make values that don't
  exist).

**Using passes in the comp.**
- A **Shuffle** node (Channel category): copies any plane (or one
  component of it) into R, G, B or A, or back into a named plane, so a
  pass can be graded, blurred or keyed with every existing node; plus a
  **Remove planes** option to drop passes once they're used.
- The Viewer's plane buttons (M1) already show any pass; vector passes
  (`P`, `N`, `motion`) show as color.
- **Write / Media Out:** EXR writes every plane with standard channel
  names (`P.x`, `N.x`, `depth.Z`, `motion.u`, `matte.card1.A`, …), so
  passes open as layers in other compositing and 3D apps; other formats
  write the beauty only (as now).
- A **Merge** option to combine by depth (zmerge, already a follow-up of
  M4) uses the `depth` passes of both inputs.
- The 3D viewport (M9) can show any pass of the scene it draws.

**Built on what exists.** Image planes (M1) carry the passes through
warps, crops and merges already. The renderer's per-sample hits (M4)
gain the hit's triangle, barycentrics and material, from which every pass
is computed in the same loop, so passes cost little when asked for and
nothing when not. Mesh rendering needs a depth-sorted hit list per
sample (as cards have now) plus per-row binning of triangles, or a BVH,
for meshes with many triangles.

**Tests.**
- Primitives: a unit cube seen face-on renders a square of the right
  size; normals point outward; UVs map a texture without seams on a
  sphere's sides.
- OBJ import: a known file's vertex count, bounds, UVs and normals; MTL
  colors.
- Passes: `P` at a pixel equals the unprojected `depth` there; `N` of a
  plane equals the plane's normal; `motion` matches the camera's move
  between frames; `matte.<node>` is 1 inside the object, 0 outside,
  fractional on edges, and the mattes of non-overlapping objects add up
  to the beauty's alpha; `diffuse + specular + emission + ambient`
  equals the beauty.
- A holdout cuts out what's behind it; mattes still work through it.
- Shuffle: a pass copied to RGBA and back is unchanged; EXR round trip
  of a multi-pass render keeps every plane and its names.
- Performance: 1080p, a 100k-triangle mesh, beauty + 4 passes at 3×3
  samples under a second on this machine.

**Order inside M10:** (a) meshes in `Scene`, primitives, cards as
meshes; (b) `P`, `N`, `uv`, `id`, mattes and Shuffle; (c) OBJ import;
(d) materials, lights and the lighting passes; (e) `motion`, zmerge,
glTF.

## Milestone 11 in detail: PnP and the tripod solve

Specified 2026-10-29 at the user's request, after reviewing M1–M8: M7's
plan listed PnP, which was left out (each frame starts from its
neighbour's camera), and D4 put the nodal (tripod) solve "later".

**Where the solver stands.** `sfm::solve` starts from the frame pair
with the most parallax, then adds frames nearest-first, each starting
from its neighbour's pose (a constant-velocity guess) refined with
cameras-only bundle adjustment. That works on smooth video but has
three gaps: (1) a jump (a cut, a dropped stretch of frames, a whip pan)
leaves the guess too far off to converge; (2) frames between solved
ones are listed as unsolved instead of being placed from the points
they see; (3) a camera that only turns has no parallax, so
`initial_pair` finds no start pair and the solve is refused.

**PnP (perspective-n-point).**
- `solve::pnp(points_3d, pixels, intrinsics) -> Option<(Pose, inliers)>`:
  a minimal solver (P3P, Kneip or Lambda Twist, up to four poses per
  sample, checked with a fourth point) inside RANSAC (adaptive iteration
  count, the same `max_error` threshold as the solve), then refined on
  the inliers with Levenberg–Marquardt (a cameras-only `bundle::Problem`
  with the points fixed). EPnP as the many-point fallback (and for the
  final fit), so flat scenes (all points on a floor or wall), where P3P
  samples get unstable, still solve.
- Use in `sfm::solve`: (a) each new frame tries the neighbour guess
  first and falls back to PnP when the guess's reprojection error is
  over `max_error` or fewer than half its tracks agree; (b) after the
  main pass, frames left unsolved (gaps) are placed by PnP from the
  points they see, then adjusted; (c) a solve can restart after a cut
  from any frame that sees enough known points.
- The solver reports which frames came from PnP (for the panel's
  per-frame list).

**Tripod (nodal) solve.**
- **Detection:** when no pair reaches `start_parallax` but tracks are
  long and consistent, fit a homography between frames (RANSAC, then
  refined). If homographies explain the tracks (median error under
  `max_error`) and they decompose as `K R K⁻¹` (a pure rotation), the
  shot is nodal. The Camera Tracker's **Solve** setting becomes
  **Auto / Free move / Tripod**: Auto (the default) picks Tripod when it
  detects one, and says so; Free move keeps today's behaviour, and
  refuses a nodal shot with a message suggesting Tripod.
- **Model:** the camera has a fixed center (at the origin) and a
  rotation per frame; each track is a direction (a unit vector), not a
  point. Initialize rotations by chaining the frame-to-frame homographies
  (`R = K⁻¹ H K`, orthonormalized), then refine everything in a
  rotation-only bundle adjustment (`bundle::Problem` with the camera
  centers fixed at 0 and the points constrained to the unit sphere; or
  a separate small solver). The focal length is solvable here too
  (homographies constrain it well when the camera turns enough).
- **Output:** a Camera keyed on rotation only (translate constant), and
  the point cloud placed on a sphere at a user-chosen **Distance**
  (default 10 units; points at infinity have no real depth), so cards
  can still be put "on" points and the scene stays a normal 3D scene.
  Set origin / ground / scale tools work, except that scale only changes
  the sphere's radius.
- Lens distortion: not in this milestone (still a follow-up), but the
  homography fit must tolerate mild distortion (a looser threshold for
  the nodal test).

**MCP / Python.** `track_camera` gains `mode` (`auto` / `free` /
`tripod`) and `distance`; `camera_solution` reports `mode` and, per
frame, whether it came from PnP. Same as `Node.track_camera(...)`
arguments.

**Tests.**
- PnP: noiseless points give the exact pose (P3P and EPnP); 0.5 px
  noise + 30 % outliers within 0.1° and 1 % of the distance; a flat
  scene (all points on a plane) still solves; too few points return
  None.
- Solver with PnP: a synthetic shot with a 10-frame gap and a 30° jump
  solves every frame, with the jump's frames marked as PnP; a restart
  after a cut.
- Tripod: a synthetic pan-and-tilt shot (camera turning about its
  center, 40 frames) is detected as nodal under Auto, rotations within
  0.1° of the truth, focal solved from 10 % off within 1 %; Free move
  refuses it with the message; a shot with real parallax is *not*
  detected as nodal.
- End to end: a rendered pan with Scanline Render, tracked and solved in
  Tripod mode; a card placed on the solved points stays locked to the
  plate.

**Order inside M11:** (a) `pnp` + tests; (b) PnP in the solver
(fallback, gap filling, restart) + panel marks; (c) homography fit and
nodal detection; (d) rotation-only adjustment, sphere points, output;
(e) the Camera Tracker's Auto / Free move / Tripod setting, MCP /
Python, help text (`docs/help.md`).

## Milestone 12 in detail: lights

Specified 2026-10-29 at the user's request ("spots or area or just a
light that shines in a preferred direction", in 2D and in 3D, "be able
to move lights ... in x, y, z"). M10 brings the first lights (Ambient,
Directional, Point, Spot) and shading; M12 completes them.

**3D lights.**
- **Area light** (new): a rectangle or disk with width / height, color,
  intensity, and a two-sided flag; shaded by sampling the area (a fixed
  grid of samples per pixel, e.g. 4×4, with a Quality param), giving
  soft falloff and soft highlights. Spot gains **cone angle** and
  **penumbra** (soft edge) and a falloff exponent; Point and Spot gain
  **decay** (none / linear / quadratic) and an optional radius (soft
  size). Every light has **color, intensity, exposure** (stops), and an
  **enabled** flag; all params animatable and linkable like the rest.
- **Placement:** a light is a scene object with a `geom::Transform`
  (translate / rotate / scale), like the camera and cards, so it can be
  parented and linked. **Look at:** an optional target (a locator, a
  card, the camera) the light aims at, as for cameras.
- **In the 3D view (M9's tools):** lights draw as icons (Point: a small
  star; Spot: a cone showing angle and penumbra; Directional: parallel
  arrows; Area: its outline and normal; Ambient: not drawn). They are
  selectable (click, box, Shift), and move, rotate and scale with
  **G / R / S** (with X/Y/Z axis locks, typed values, Ctrl snap) and the
  translate / rotate / scale **gizmos**, one undo step per drag. Extra
  handles: drag the spot's cone edge to change the angle; drag an area
  light's edges to resize it. **Numpad 0 / View → Look through
  selected** looks through a spot or area light (for aiming).
- **Shading:** Lambert and Blinn–Phong from M10, summed over lights;
  Area by sampling. Shadows: shadow maps for Spot, Directional and Point
  (cube), soft by percentage-closer filtering; Area shadows by its
  samples. The per-object casts / receives flags from M10 apply.
  (Shadows may land as the last step if time is short.)
- **Passes:** M10's `diffuse`, `specular` and per-light `light.<node>`
  passes include Area lights; plus a `shadow` pass.

**2D lights (new node, Light 2D).** Lights a flat image in the viewer's
2D space, for adding a lamp, a spot on a wall, or a sun direction to a
plate.
- Types: **Spot** (position, direction, cone angle, softness),
  **Area** (a rectangle or ellipse with softness), **Point**
  (position, radius, falloff), **Directional** (an angle across the
  frame, with a gradient falloff).
- Inputs: the image; optional **normals** (an N pass from M10, or a
  normal map) and **depth** (a Z pass) for real relighting: with normals
  it shades with Lambert / Blinn–Phong as in 3D (the light given a
  height above the image); without them it is a shaped glow / gradient.
- Blend: add (screen) or multiply, with color, intensity, exposure, and
  a mask input. Several Light 2D nodes stack (or one node with a list
  of lights).
- **Viewer handles:** drag the light's position, its aim (the spot's
  direction), cone width and softness, and an area's corners, as for
  shapes and transforms today; all animatable.

**MCP / Python.** Create lights with `create_node` (types `light_area`,
`light_2d`, etc.), set params as usual; `scene_objects` lists lights
with type and world position. Python the same through `Node`.

**Tests.**
- Area light: a Lambert card lit by an area light matches a dense-sample
  reference within 1 %; a spot's cone edge falls where its angle says;
  decay laws give the right ratios at 1 and 2 units.
- Shadows: a card over a floor casts a shadow where projection says.
- Light 2D: a spot at a pixel lights that pixel brightest and falls to
  half at its softness edge; with a flat-normals input it matches the
  3D Lambert result.
- 3D view: selecting a light and pressing G X 2 Enter moves it 2 units
  in x (undo restores it); a cone-edge drag changes the angle.

**Order inside M12:** (a) light params as scene objects, Area light,
spot cone / penumbra, decay; (b) lights in the 3D view (icons, picking,
G / R / S, gizmos, handles); (c) Light 2D node and viewer handles;
(d) shadows; (e) passes, MCP / Python, help text.

## Milestone 13 in detail: particles

Specified 2026-10-29 at the user's request: "a particle system, emitter,
particle nodes that can be objects from a file or just a" sprite/point.

**Nodes** (scenes like the rest; particle streams flow between them):
- **Particle Emitter:** where particles come from: a point, a line, a
  rectangle / disk / sphere / box, the surface or vertices of an input
  geometry (a card, a primitive, an imported mesh), or, in 2D, an
  image's alpha (emit where the mask is). Params: rate (per frame) or a
  one-shot burst, start frame / end frame, initial velocity (direction,
  speed, spread cone, randomness), lifetime (with variation), start
  size, color (and color over life), rotation and spin, and a **seed**
  (results are repeatable per seed).
- **Particle forces / modifiers:** Gravity (a direction), Wind / Drag,
  Turbulence (curl noise: strength, scale, speed), Attractor (a point or
  locator pulling or pushing), Vortex, Bounce (off a plane or card,
  with friction), Kill (by age, by a region), and Speed limit. Each is a
  node in the chain, with a mask/strength and start / end frames.
- **Particle Render / instancing:** how particles look:
  - **Sprites / points:** camera-facing quads (or round points), with a
    texture input (an image or a sequence, cycling or by age), size and
    opacity over life, additive or over blending.
  - **Objects:** each particle instances geometry: a card, a primitive
    (sphere, cube, ...), or a **mesh loaded from a file** (OBJ, then
    glTF, as in M10), oriented by velocity or randomly spun, scaled by
    size. Several objects can be chosen at random per particle.
  - The output is a scene, rendered by Scanline Render (lit by M12's
    lights, with motion blur from velocity once motion blur exists, and
    passes); in 2D, a **Particles 2D** path renders sprites straight
    into the image (no camera needed).

**Simulation.** Particles step frame by frame (with substeps for fast
motion), from the start frame; results are cached per frame, so
scrubbing back is instant and scrubbing forward simulates only what is
missing. Changing a param clears the cache from its first frame. The
same seed and params always give the same particles (tests rely on it).
A **pre-roll** param starts the sim before the comp's first frame.

**In the 3D view.** Emitters draw as their shape; particles draw as
points (colored) for speed, with an option to draw instanced objects.
Emitters, attractors and bounce planes move with G / R / S and gizmos
like other objects. In 2D, the viewer shows the emitter's shape with
handles.

**MCP / Python.** Particle nodes through `create_node` / params; a
`particles(node, frame)` query returns positions, velocities, ages and
ids (for scripting and tests).

**Tests.**
- Determinism: the same seed gives identical particles; a different
  seed differs.
- Emission: rate × frames particles exist (minus deaths by lifetime);
  a burst emits once.
- Physics: under gravity alone, a particle's height follows
  `y0 + v0 t − ½ g t²` within 1e-6 (substeps); bounce reflects velocity
  with the given friction; kill by age removes particles at the right
  frame.
- Cache: scrubbing back returns the cached frame; changing gravity at
  frame 10 re-simulates from there.
- Rendering: sprites appear where the camera projects their positions;
  an instanced OBJ mesh renders at each particle with its size and
  rotation.

**Order inside M13:** (a) particle data and the simulation core (emit,
age, forces, cache, seed); (b) emitter shapes (incl. geometry surfaces
and 2D masks) and modifier nodes; (c) sprite rendering in Scanline
Render and Particles 2D; (d) object instancing (primitives, meshes from
files); (e) 3D-view drawing and handles, MCP / Python, help text.
