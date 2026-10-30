# Splat plan: Gaussian splats in the 3D scene

Status: **proposed** (2026-10-30, at the user's request). Not started;
it follows the TODO "Particles" round. Decisions D1–D5 below stand as
proposed until the user says otherwise. The work goes one milestone at a
time, as in `phase-4-plan.md`.

**Versioning (user, 2026-10-30):** starting S1 begins **phase 10**: the
version goes to **0.10.0** and the work moves to a new branch
**`wip-ns-splats`** (from the current work branch, pushed to GitHub);
changes then count 0.10.1, 0.10.2, ….

## Goal

Read existing 3D Gaussian splat captures and render them in Arclight's
3D scene, composited with cards, meshes, particles and plates, through
the same cameras, with the same render passes and motion blur. The use:
a captured environment or prop (from Postshot, Polycam, Luma, Nerfstudio,
gsplat, …) as a set extension or background, with CG and live action
over and inside it.

**Out of scope:** training splats from footage or the camera tracker's
solve. That needs a GPU training loop (CUDA in every tool that does it
well); users train elsewhere and bring the result in.

## What exists to build on

- `scene.rs`: `Scene { cameras, cards, meshes, lights, points,
  particles, … }` flows along scene wires; `Scene::extend`, `tag` (the
  node each object came from), `at_time` / `between` (motion blur).
  Splats become one more list.
- `render3d.rs`: a CPU (rayon) renderer. Surfaces (cards, meshes) are
  binned by row and depth-sorted per sample; see-through ones blend front
  to back. `Passes` (depth, P, N, uv, motion, id, mattes, light passes,
  shadow); `render_blurred` (motion blur, jittered shutter); holdouts.
  `render3d/particles.rs` already composites a non-surface primitive
  (sprites) with surfaces by depth: the closest model for splats.
- `render3d::render_preview` / `render_preview_blurred` and the 3D view's
  worker (`gui/worker.rs`): a lower resolution while the view moves, full
  once it settles; viewport dots for points and particles.
- Read Geo (`ops.rs` `read_geo`): file nodes cached by path and mtime,
  placed by a transform. `mesh.rs` / `gltf.rs` readers.
- MCP / Python `scene_objects`, `particles` (paged columns): the pattern
  for a `splats` query.

## The model

```rust
/// Gaussian splats read from a file, placed by `world`.
pub struct Splats {
    pub positions: Vec<[f32; 3]>,   // object space
    pub scales: Vec<[f32; 3]>,      // linear (the file's log scale, exp'd)
    pub rotations: Vec<[f32; 4]>,   // unit quaternions
    pub opacity: Vec<f32>,          // 0..1 (the file's logit, sigmoid'd)
    pub sh: Vec<f32>,               // color: SH degree 0..3, 3 × (deg+1)² per splat
    pub sh_degree: u8,
    pub world: Mat4,
    pub node: String,
}
// Scene gains `pub splats: Vec<Arc<Splats>>` (shared, never copied per frame).
```

Kept as struct-of-arrays in f32 (millions of splats: 1–6 M is common,
~250 bytes each at degree 3). The world transform stays separate so an
animated Read Splat doesn't rewrite the data.

**Rendering (the standard 3DGS rasterizer, on the CPU):** per view,
transform centers to camera space, cull outside the frustum (with a
3σ margin), project each 3D covariance to a 2D ellipse (the EWA /
Jacobian approximation), evaluate the color from the SH for the view
direction, bin splats into 16×16 pixel tiles, sort each tile's splats by
depth, and blend front to back until transmittance falls below a
threshold. Rayon over tiles. Output linear premultiplied RGBA, like
everything else.

## Decisions (proposed)

- **D1, formats.** The INRIA 3DGS `.ply` first (what every trainer
  exports). Then `.splat` (antimatter15) and `.spz` (Niantic,
  compressed). Later, if asked: SOG / PlayCanvas compressed `.ply`.
- **D2, compositing with surfaces.** Splats blend in depth order with
  see-through surfaces and are cut by opaque ones per pixel: the
  renderer gathers, per pixel, the surface fragments and the splats in
  depth order and blends them all front to back (what particle sprites
  do now). Holdout meshes cut splats out. Splats don't cast or receive
  shadows from Arclight's lights in M1–M3 (their lighting is baked in
  the capture); a Light Influence option may come later.
- **D3, passes.** depth and P from the depth where accumulated alpha
  first passes 0.5 (the usual "median depth"); N from the dominant
  splat's shortest axis; mattes and id per Read Splat node; motion from
  the transform and camera; uv none. Light passes: splats go to
  `emission` (they are self-lit).
- **D4, the 3D view.** While moving: splat centers as dots in their
  color (decimated past ~300 k), like a point cloud. Settled: the real
  splat render at the view's resolution (as the blurred preview does
  now). A per-node "Preview" choice (dots / full) for heavy captures.
- **D5, performance target.** A 1 M-splat capture at 1920×1080 renders
  in under ~2 s on the user's machine (CPU, rayon), with sorting and
  binning cached per frame and per camera. A GPU path is the TODO's
  "GPU acceleration (wgpu)" item, not part of this plan.

## Milestones

Each one ships as its own versions (0.x.y bumps), tested alone,
committed and pushed by the master.

### S1: Read Splat and the renderer

- `src/splat.rs`: `Splats`, the `.ply` reader (binary little-endian and
  ASCII headers; degree 0–3; missing SH rest = degree 0), bounds,
  `Splats::read` cached by path and mtime like Read Geo.
- `read_splat` node (Scene category): File, Translate / Rotate / Scale,
  Opacity (multiplies), SH degree cap (0–3, for speed), Holdout off.
  Outputs a scene with one `Splats`.
- `render3d/splats.rs`: project, tile, sort, blend; composited with
  surfaces as in D2. Scanline Render draws them; Anti-aliasing applies as
  for everything else (or once at the splat level if that's faster:
  measure).
- Tests: a few hand-made splats render where and as large as expected
  (an isotropic splat's 1σ radius in pixels matches the projection), the
  front-to-back blend, culling, SH degree 0 vs 1 color change with view
  direction, and a small `.ply` round trip in `examples/geo/`.

### S2: The 3D view, picking and scripting

- Viewport dots (decimated) while moving, the full render once settled
  (D4); bounds for framing (F) and the selection box; pick by the nearest
  center under the pointer.
- MCP / Python: `scene_objects` lists splat sets (node, count, bounds,
  SH degree); a paged `splats` query like `particles`.
- README, help, CHANGELOG.

### S3: Passes, holdouts, motion blur

- The passes in D3, mattes / id per node, holdouts cutting splats,
  `Scene::at_time` / `between` for animated Read Splat transforms so
  motion blur and the motion pass work; Jitter shutter applies.

### S4: Editing nodes

- **Splat Crop**: keep or remove splats inside a box or sphere
  (floaters, cleaning up a capture's edges), with a viewport handle.
- **Splat Color**: grade the SH DC term (exposure, saturation, tint)
  and opacity, with an optional view-independent (degree 0) switch.
- **Splat Transform** for placing a capture in a shot's space (or reuse
  the Read Splat's own transform plus a Locator / Look-at).

### S5: More formats and speed

- `.splat` and `.spz` readers.
- Faster rendering: per-frame caches of the projected and sorted splats
  per camera, a lower-SH preview, and measured hot spots. If the GPU
  item lands first, a wgpu splat rasterizer slots in behind the same
  `render3d/splats.rs` interface.

## Risks and open questions

- **Speed** is the main risk: CPU splat rendering is fine for final
  frames, heavy for interaction; D4's dots-while-moving keeps the view
  usable.
- **Mixing with surfaces** per pixel (D2) is more work than drawing
  splats over or under the render; it's what makes CG inside a capture
  look right.
- **Coordinate conventions:** captures are usually Y-down or Z-up
  (COLMAP / OpenCV); Read Splat needs an "Up axis" / "Flip Y" choice and
  a sensible default.
- **Memory:** a 6 M-splat degree-3 capture is ~1.5 GB as f32; the SH
  degree cap reads less, and S5 may store the SH rest as f16.
- **Lighting:** splats are self-lit; relighting them is research, not
  planned.
