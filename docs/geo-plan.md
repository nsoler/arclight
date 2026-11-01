# Geo plan: procedural geometry nodes (GeoNodes-like)

Status: **phase 13, planned; P1 not started.** Proposed 2026-10-31 at the
user's request; the user had it committed as the phase 13 plan the same
day. Decisions D1–D6 stand as proposed until the user says otherwise.

**Versioning (user, 2026-10-31):** phase 12 (GPU) ended at 0.12.19
(v0.12.19); **phase 13** is **0.13.x** on branch **`wip-ns-geo`** (from
`wip-ns-gpu`), starting at 0.13.0 with this plan.

## Goal

Build and change 3D geometry from rules inside the node graph, the way
Blender's Geometry Nodes or Houdini's SOPs do, sized for a compositor's
3D work, not for general modelling:

- **Set dressing and extensions:** scatter rocks, debris, foliage cards
  or splat props over a ground plane from the camera tracker, with
  density from a painted or keyed mask.
- **Matte and helper geometry:** ground planes, walls, and shadow
  catchers fitted to tracked points, displaced by an image.
- **Motion graphics:** arrays and grids of cards or meshes, with
  effectors (falloff by distance, noise, time) on scale, rotation and
  color.
- **Crowds of cards:** many instances of a few textured cards (people,
  trees, smoke elements) facing the camera, picked at random.

**Out of scope:** booleans, remeshing, UV unwrapping, sculpting, rigging,
volumes (VDB), and a general simulation solver. A file made elsewhere
(Read Geo) still covers hand-made models.

## What exists to build on

- `scene.rs`: `Scene { cameras, cards, meshes: Vec<Model>, lights,
  points, particles, splats, regions }` flows along scene wires
  (`ops::Kind::Scene`). `Model` already shares its mesh (`Arc<Mesh>`)
  and places it with `world`, so N copies of one mesh are cheap to
  describe; the renderers just don't know they are copies.
- `mesh.rs`: `Mesh { positions, normals, uvs, triangles }` (no other
  attributes), primitives (`cube`, `sphere`, `cylinder`, `plane`),
  `merge`, `transform`, `smooth_normals`, `flat`, and the OBJ/MTL reader;
  `gltf.rs` and `alembic.rs` readers.
- `particles.rs`: emission shapes that are already scatter code:
  `Shape::MeshSurface` (evenly by area), `MeshVertices`, `Mask` (by an
  image's alpha), `Sphere` / `Box` (surface or volume).
- `expr.rs`: the formula language for live links (math, vectors,
  `noise`, `random`, `if`, `mix`, `smoothstep`, `node.param`). This
  becomes the per-element field language (D2).
- `ops.rs` scene modifiers: `splat_crop` / `splat_color` take a scene
  and return a changed one, as geometry nodes will; `regions` give the
  3D view a box or ball to show and move (falloff volumes).
- `render3d.rs` (CPU) and `render3d/gpu` (the compute rasterizer, 0.12),
  shadows, motion blur (`Scene::between`), passes (P, N, id, mattes).
- `sfm.rs` / `solve.rs` / the camera tracker's point cloud
  (`Scene::points`): the ground truth to scatter on and fit planes to.

## The model

```rust
/// Named per-element data on geometry (Houdini's attributes, Blender's
/// named attributes).
pub enum Attr {
    Float(Vec<f32>),
    Vec3(Vec<[f32; 3]>),
    Color(Vec<[f32; 4]>),
    Int(Vec<i32>),
}

pub struct Attrs(BTreeMap<String, Attr>);

/// Mesh gains attributes per point and per face (corner attributes
/// later, if uv seams need them). `positions` / `normals` / `uvs` stay
/// as they are, so readers and renderers don't change.
pub struct Mesh { /* … */ pub point_attrs: Attrs, pub face_attrs: Attrs }

/// Points with attributes, no faces: what Scatter makes and Instance on
/// Points consumes. Reserved names: P, N, up, pscale, orient (quat),
/// scale (vec3), Cd (color), Alpha, id, variant.
pub struct PointSet { pub positions: Vec<Vec3>, pub attrs: Attrs, pub world: Mat4, pub node: String }

/// Copies of one prototype (a mesh, a card or a splat capture) at many
/// placements, kept as copies: the renderers draw them instanced.
pub struct Instances {
    pub prototypes: Vec<Prototype>,      // Model | Card | Arc<Splats>
    pub which: Vec<u32>,                 // prototype per instance
    pub worlds: Vec<Mat4>,
    pub colors: Vec<[f32; 4]>,           // tint (from Cd), or empty
    pub node: String,
}

pub struct Scene { /* … */ pub point_sets: Vec<PointSet>, pub instances: Vec<Instances>, pub curves: Vec<Curves> }
```

`Scene::extend`, `tag`, `at_time` / `between` (motion blur: instance
worlds interpolate by `id`) and the 3D view's picking learn the new lists.

## Decisions (proposed)

- **D1, scene wires, not a new port kind.** Geometry nodes take and give
  a `Scene`; they act on its meshes, point sets, curves and instances and
  pass the rest through. So Scanline Render, the 3D view, shadows,
  motion blur and the GPU path draw their output with no new wiring, and
  a geometry chain drops into any existing scene. An optional **Group**
  param (a node name or `name*` glob) narrows what a node acts on.
- **D2, fields are expressions, not node wires.** Blender wires fields as
  graphs of little nodes, while Houdini writes them as VEX in a
  Wrangle. Arclight's graph is a compositor's and gets busy fast, so one
  **Attribute Set** node evaluates an `expr.rs` formula per element, with
  new per-element names: `@P`, `@N`, `@uv`, `@Cd`, `@pscale`, `@id`,
  `@index`, `@count`, any attribute as `@name`, plus vector `noise(@P *
  4)`, `random(@id)` and `dist(@P, locator1.translate)`. Any numeric
  param on a geometry node may also be a per-element expression
  (`scale: "0.5 + random(@id) * 0.5"`). The parser and evaluator stay one
  piece of code, compiled once per cook and run in parallel (rayon).
- **D3, eager cooking, cached by the graph.** Unlike particles (lazy
  streams simulated on demand), geometry nodes cook at the frame asked
  for, and the graph's node cache keeps the result. A node with no
  animated params or inputs cooks once for every frame (the cache key
  leaves out the frame), so a static scatter of 100k points costs nothing
  when you scrub.
- **D4, instances stay instances.** Instance on Points makes
  `Instances`, not merged meshes: 50k trees are 50k matrices, not 50k
  copies of a mesh. The CPU renderer bins each instance's triangles
  through its matrix, and the GPU rasterizer draws them in one instanced
  dispatch. **Realize Instances** makes real meshes when a later node
  needs to edit them (displace, delete faces).
- **D5, stable ids.** Scatter gives each point an `id` (seeded, and
  stable when density or the mask changes slightly: points are made in
  cells by a hashed seed, so most survive an edit). Random picks, motion
  blur and the id/matte passes key on it, so instances don't jump when
  you tweak.
- **D6, no node groups or loops at first.** Reusable subgraphs (Blender's
  node groups, Houdini's HDAs) and repeat or simulation zones are a
  later phase. Time-driven effects come from expressions (`frame`,
  `wiggle`) and from the particle system (P6).

## Nodes, by milestone

**P1, the model and the plumbing.** `Attrs` on `Mesh`, `PointSet`,
`Instances`, `Curves` in `Scene` (with `extend`, `tag`, `between`,
picking). Nodes: **Transform Geo** (moves what it's given, Group param),
**Geo Info** (the viewer shows counts and an attribute **spreadsheet**
panel, as Blender's spreadsheet editor does: per point / face / instance,
filtered by the selection). MCP / Python `geometry` query (paged columns,
as `particles`). The 3D view draws point sets as dots (sized by
`pscale`, colored by `Cd`).

**P2, points.** **Scatter** (on a mesh's surface by area, in a volume,
on the camera tracker's points; density by a number, an attribute, or an
image projected through a camera or on the uvs; min distance (Poisson
disk) option; seed); **Points from Geo** (vertices, face centers);
**Grid Points** / **Line Points**; **Delete** (points or faces where an
expression is true, or inside / outside a region from the 3D view).
Reuses `particles::Shape`'s area-weighted surface sampling.

**P3, attributes (fields).** **Attribute Set** (D2's per-element
formula; several attributes in one node); **Attribute Noise** (3D noise
into an attribute, with a falloff region); **Attribute from Image**
(sample an image at each point through a camera (projection) or at its
uv: color, density, height); **Attribute Transfer** (nearest neighbor
from another input, with a radius and falloff); **Falloff** (distance
to a locator, region or curve into a 0–1 weight, the MoGraph
"effector"). Tests compare against hand-computed values per element.

**P4, instancing.** **Instance on Points** (prototypes from the second
input: its meshes, cards and splat captures; pick by `variant`, by
`random(@id)` or in order; orient by `N` + `up` or `orient`, or **face
the camera** for cards; scale by `pscale` / `scale`; tint by `Cd`);
**Realize Instances**. CPU renderer: instanced binning (one mesh, many
matrices). GPU rasterizer: instanced dispatch, prototypes resident once.
Shadows and motion blur per instance. The id pass and mattes per
instance. Bench: 100k instanced cards and 10k instanced meshes at 4K,
CPU and GPU.

**P5, mesh and curve ops.** **Subdivide** (linear and Loop),
**Displace** (along N by an attribute, noise or image), **Extrude**
(faces, by an attribute), **Smooth Normals** / **Flat**; curves:
**Curve** (a polyline or Bézier drawn in the 3D view, or from a
tracker's path), **Resample**, **Curve to Mesh** (sweep a profile: roads,
cables, pipes), **Points on Curve** (fences, lampposts), **Fit Plane**
(a ground plane fitted to selected tracker points: the most common VFX
use, and a one-click win). **Done** (0.13.x; Extrude last), with
Normals' by-angle mode for Smooth Normals / Flat.

**P6, time and particles.** **Points to Particles** (emit from a point
set, its attributes carried over) and **Particles to Points** (a frame
of a particle stream as a point set, so particles can drive instances:
debris meshes on particles). **Write Geo** (OBJ and glTF; Alembic per
frame later) to hand the result to other tools. **Done** (Write Geo
last; Alembic still later).

**P7, the GPU** (as phase 12 did for 2D): Attribute Set and Scatter as
compute shaders when the counts are large (> 100k), the expression
compiled to WGSL. Profiled before committing to it: CPU and rayon may be
enough at compositor scale.

**P8, Project 3D** (added 2026-10-31 at the user's request): camera
projection as a texture, the classic set-extension and patch setup
(Nuke's Project3D). What exists samples an image per point or vertex
(Attribute from Image, Displace via camera, Scatter's mask); this
projects it per rendered pixel, so a sharp plate lands on a 4-vertex card.
- **Project 3D** node: inputs scene (what receives it), image (the plate
  or a painted patch), projector (a scene with a camera). Output: the
  scene with a projection attached to its meshes and cards (Group), each
  a projector camera (its matrices and lens) and the image.
- **Frame hold:** the projector camera taken at a fixed frame (the patch
  painted on frame 1040 stays put while the shot camera moves), or live.
- **Mode:** replace the surface's color or texture, over it (the plate's
  alpha), or multiply; **Crop** to the projector's frame with a soft
  **Edge** feather; outside the frame the surface keeps its own look.
- **Occlusion:** only what the projector sees first receives it (no
  smearing through to hidden surfaces), from a depth map rendered from
  the projector: the shadow-map code (`render3d/shadow.rs`) already makes
  exactly this. **Backfaces** (facing away from the projector) off by
  default.
- Several projections on one surface stack in order (a cleanup patch over
  the plate). Instances take their prototype's projection unless the
  node targets the instances themselves.
- Renderers: the CPU shader and the GPU rasterizer project each shaded
  sample's P through the projector (a texture lookup with the projector's
  matrix and lens), with mip filtering by the projected footprint; the 3D
  view's lit preview shows it. Motion blur: the projection follows the
  surface (projected in the projector's frame, which is held or live).
- **Bake** (later): write a projection into a uv texture (an image
  output), to hand projected geometry to other tools or freeze it.
- Tests: a card through the shot camera with the plate projected from the
  same camera renders the plate exactly (to the pixel); a second card
  hidden behind the first receives nothing with Occlusion on; frame hold
  stays put as the shot camera moves; CPU vs GPU within D8.

**P8b, multi-camera projection** (added 2026-10-31 at the user's
request; extends P8, 0.13.30 / 0.13.32). Several Project 3D nodes stack
in order, so where two projectors overlap the later wins with a hard
seam. Other compositors blend those seams by facing angle, and project
one tracked camera from several frames. Split: the node side
(`project3d.rs`, `ops.rs`, `graph.rs`; Dev1), the renderers
(`render3d/project.rs`, `render3d/gpu/*`; Dev2).
- **Facing fade:** `Projection::fade_start` / `fade_end` (degrees between
  the surface's normal and the way to the projector; the side facing it
  with Backfaces on): weight 1 up to start, smoothstep to 0 at end, a hard
  edge at end when start isn't below it. 90 / 90 (the default) is no fade:
  old projects render as before. `Projection::facing(cos)` is the one
  curve the CPU and GPU share.
- **Blend with others** (`Projection::blend`): a run of consecutive
  blending projections on a surface is one layer. Each one's weight is
  w_i = Mix × crop / Edge × facing × occlusion visibility; the layer's
  plate is Σ w_i · plate_i / Σ w_i, put on min(Σ w_i, 1) of the way by the
  first one's mode. Off: today's stacking.
- **Frames** (a text param, `1001, 1040, 1080`; empty is off; it overrides
  Frame hold): one projection a frame listed, from the projector's camera
  and with its image then, all blending. `held_inputs` gives the frames;
  the graph renders the held inputs in an evaluator a frame and hands
  them to the node after its own inputs, an (image, projector) pair a
  frame. The node's key counts the held inputs' keys at their frames,
  not the live projector's and image's, so moving the live camera
  doesn't re-evaluate a held projection.
- Example: `examples/multi-projection.json` (a block and a wall under two
  projectors from left and right, blended, faded by facing).
- Tests: params read and default to the old look; Frames gives one
  projection a frame at the camera and image then; the facing curve; the
  key steady under a moving live projector; blended seams on the CPU and
  GPU (Dev2).

## Risks and open questions

- **Expression speed:** `expr.rs` is a tree-walking interpreter. For 1M
  points × several attributes, compile to a small bytecode (or closures)
  first, and to WGSL only in P7.
- **Graph UX:** many geometry nodes per scene could crowd the node
  graph. The Group param and Attribute Set's many-attributes-per-node
  keep chains short; node groups (D6) may be needed sooner than planned.
- **The 3D view's speed with 100k+ instances:** the preview needs the
  instanced GPU path (P4) before scatters get big; until then, a
  display-percentage option on Scatter (as Houdini's display flag
  thinning).
- **Projects and scripts:** new ops go into the project file, MCP
  (`ops` lists them) and Python like the rest; old projects are
  unaffected (D1 adds no port kind).
- **Which first?** Fit Plane + Scatter + Instance on Points (cards
  facing the camera) is the smallest slice that's useful in a shot. It
  could ship before the rest of P2–P5 as a vertical slice: P1, then
  Scatter (P2), Attribute Set (P3), Instance on Points (P4, CPU only),
  Fit Plane (P5).
