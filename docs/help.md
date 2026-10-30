# Arclight Help

Arclight is a node-graph compositor: build a graph of nodes in the
**Node Graph** at the bottom, see the result in the **Viewer**, and edit the
selected node in **Properties** on the right. On macOS, ⌘ stands for Ctrl.

## Node graph

| Action | How |
|---|---|
| Add a node | Right-click (the node browser; type to search) |
| Connect | Drag from an output (circle) to an input (square), or the other way |
| Select a wire | Click it (it lights up); **⌫** disconnects it |
| Wire menu | Right-click a wire: **Insert 3D Camera Setup**, **Insert Node…**, **Delete connection** |
| Disconnect / reroute | Drag a connected input away |
| View a node | Double-click it, or **V** |
| Delete | **⌫** (Backspace) or **Delete** |
| Frame all | **F** |
| Pan | Middle-drag or **⌥**-drag (Alt) |
| Zoom | Scroll or pinch |

**Fit** (top-right corner of the Node Graph) or **F** with the pointer over it frames every node and backdrop.

Over the Node Graph: **Shift+Tab** opens the node browser at the pointer (like right-click); **Shift+R**, **Shift+T**, **Shift+M** add a Media In, Transform or Merge there, into the wire under the pointer (or the selected wire), else after the selected node.

A node's inputs are yellow squares (hollow when optional); a **mask**
input is violet and labelled **mask**.

### 3D camera setup

To put a picture into 3D, use **Edit ▸ Add 3D Camera Setup** (or choose
**Camera** in the node browser; hold **⇧** for the Camera alone). It adds a
**Camera**, a **Scene** and a **Scanline Render**, wired up, in a **3D
Setup** backdrop, just before the **Viewer**, so the render shows straight
away: the picture the Viewer showed becomes the render's background (with
no Viewer, one is added; a Viewer with nothing connected takes the
selected node, or the end of the chain, as the background). To put it
somewhere else, right-click a wire and choose **Insert 3D Camera Setup**:
the picture on that wire becomes the background and the render goes on to
where the wire went. Then add Cards, meshes and lights to the Scene's free
inputs (or the render's last, empty input). The setup's Camera starts back
along +Z, far enough that a new Card (1 wide, at the origin) just fills
its frame. The 3D view shows everything the render draws, extra inputs
included.

To see the scene as a Camera sees it, click **Look through in 3D view** in
its Properties (spot and area lights have it too): the viewer switches to
3D through that lens; click again to come back out. The **0** key does the
same for the selected camera or light.

The first setup in a project switches the viewer to its **3D** view,
framed on the new scene, and says how to switch back (**Tab**). The
viewer's **3D** button shows a dot whenever the viewed node has a 3D
scene, and is dimmed when it hasn't.

### Merge

**Merge** puts **A** (the foreground) onto **B** (the background), in B's
frame, by its **Operation** (over, under, add, multiply, screen, min, max,
zmerge).

- **⇄ A / B** beside Operation swaps them, for any operation: B over A, in
  A's frame (the button lights while swapped).
- The **mask** input (violet) limits where A is combined: outside it you
  see B.
- **Mix** fades the whole result toward B.

### Masks

Merge, Keymix, Grade, Exposure, Saturation, Invert, Clamp, Blur, Keyer,
Transform, Corner Pin and Lens Distort (the nodes with a **mask** input)
apply only where the mask lets them; elsewhere their input shows through. Connect any image to it, then
choose in Properties:

- **Mask channel**: what masks: its **alpha** (the default), **red**,
  **green**, **blue** or **luminance**.
- **Invert mask**: the opposite (everywhere but the mask). With nothing
  connected the node applies everywhere and Invert does nothing.
- **Mix**: how much of the result shows, on top of the mask.

## Selecting nodes

- Click to select; **⇧** or **⌘** click to add or remove.
- Drag a box on empty space (**⇧** adds).
- **⌘A** selects all, **Esc** none.
- **⌘G** groups the selection in a backdrop, **⇧⌘G** ungroups.

## Viewer

- A project can have several **Viewer** nodes; the viewer shows the one
  you last wired into, double-clicked (or pressed **V** on) or clicked,
  marked with an eye in the graph (the others read "Viewer (not shown)").
- Drag (or middle-drag) to pan; scroll or pinch to zoom.
- Double-click or **F** to fit.
- **Tab** (with the pointer over the viewer) switches between **2D** (the
  image) and **3D** (the scene); so do the **2D / 3D** buttons (**3D ●**
  when the viewed node has a scene).
- The selected node's handles draw over the image: drag them to edit (a
  Transform's gizmo, shape boxes and vertices, tracker boxes, corner pins,
  a Light 2D's position, aim, cone and softness).
  **⌘**-click (or **⌘**-drag) in a Keyer picks the screen color.
- **View ▸ Show frame size**: labels the viewed frame's bottom-left
  corner 0,0 and its top-right corner with its width,height (the image's
  format); on by default.
- View preferences (Show frame size; the 3D view's Solid / Lit, wireframe
  and shadows) are remembered between runs in
  `~/.local/state/arclight/view.json`; a project that saved its own 3D
  view keeps it.

## Player

| Key | Does |
|---|---|
| **Space** | Play / pause |
| **J** / **K** / **L** | Play backward / stop / play forward |
| **←** / **→** | Step a frame |
| **⌥←** / **⌥→** | Previous / next key |
| **Home** / **End** | First / last frame |

## Project settings

Each project can have a **folder**: **File ▸ New** asks for the new
project's name and folder (in the Projects folder from Preferences unless
you choose another; or none), and **Folder** in Project Settings changes
it (Browse…). File dialogs for media (Read, Media In, Read Geo, Import)
and outputs (Write, Media Out) start there; without one, in the folder
the project file is in.
Files inside the project's folder are saved relative to it (and the
folder relative to the project file, when the file is in it), so you can
move or copy the whole folder and open it elsewhere. A path you type
relative, in a File field or through MCP / Python, means relative to the
project's folder; File fields show paths in the folder relative to it
(hover for the full path).

**File ▸ Project Settings…** (**⇧⌘P**) sets the project's frame size, frame
rate and frame range. Pick a format (HD, UHD, 2K or 4K DCI, 720p, PAL,
NTSC, square, vertical…) or type the width and height (⇄ swaps them), and
a frame rate (23.976 to 60, or your own). **Match media** takes the size,
rate and frame range from the selected Media In or Read (its size as in
the file). **Apply** or **OK** changes the project (one undo step);
**Cancel** leaves it as it was. The transport bar's In, Out, FPS and Format
fields change the same settings directly.

**Read** and **Media In** both read images and video (`.mp4`, `.mov`, …,
in upper or lower case): Read plays a video from frame 1, Media In over the
frame range it sets.

New **Read** and **Media In** nodes fit what they read to the project's
size (**Fit to project**), and follow it when it changes:

- **fit**: all of the picture, with transparent bars where its shape
  differs (the default for new nodes);
- **fill**: covering the frame, cropped where it overflows;
- **stretch**: exactly the frame, ignoring the picture's shape;
- **width** / **height**: matching that side, keeping the shape;
- **none**: its own size (as nodes in projects saved before this read).

## Keyframes

- Click a parameter's **◇** to set or remove a key at the current frame.
- Editing an animated parameter keys it automatically.
- Right-click **◇** for the interpolation and to jump between keys.

## Curve editor

- Click, **⇧**-click or box-drag to select keys; drag to move them.
- Double-click a curve to add a key; **⌫** deletes.
- Scroll to pan; **⌘**-scroll or pinch to zoom (**⌥**: value only, **⇧**:
  time only); middle-drag pans; **F** frames all.

## 3D view

Switch the viewer to **3D** (or press **Tab** over it) to see the viewed
node's scene: a Camera, Card, Cube, Sphere, Cylinder, Plane, Read Geo,
light, Scene, Camera Tracker or Scanline Render. The controls follow Blender's
defaults. Arclight's world is Y up, so the top view looks down −Y and the
front view down −Z.

### Navigating

| Action | Mouse | Trackpad |
|---|---|---|
| Orbit | Middle-drag | Two-finger scroll |
| Pan | **⇧** + middle-drag | **⇧** + two-finger scroll |
| Zoom (toward the pointer) | Scroll, or **⌘** + middle-drag | Pinch, or **⌘** + two-finger scroll |

With **Emulate 3-button mouse** on, **⌥** + left-drag orbits, **⇧⌥** +
left-drag pans and **⌘⌥** + left-drag zooms.

The **navigation gizmo** in the top-right corner does the same with the
mouse: click an axis of the ball to look along it, drag the ball to orbit,
and drag the hand (pan) or magnifier (zoom) buttons; the camera button looks
through the scene camera and the grid button switches perspective /
orthographic.

### Views

| Key | View |
|---|---|
| **1** / **⌘1** | Front / back |
| **3** / **⌘3** | Right / left |
| **7** / **⌘7** | Top / bottom |
| **5** | Perspective / orthographic |
| **0** | Through the selected camera, spot light or area light, else the scene camera (its frame outlined, outside darkened); again to come back out. Also **View ▸ Look through selected** |
| **2** / **4** / **6** / **8** | Orbit 15° down / left / right / up |
| **.** or **F** | Frame the selection |
| **Home** | Frame everything |
| **`** (backtick) | A pie of views: point at one and click, or let go of the key |
| **Z** | Meshes solid (lit from the view) or as wireframes |
| **⇧Z** | Solid (lit from the view) or Lit (by the scene's lights) |

The number row works as well as the numpad. Axis views switch to
orthographic until you orbit away. Orbiting leaves a camera view for the
free view from where the camera was. Looking through a spot or area light
shows where it shines (a spot's cone just fills the frame's height): move
or rotate it with G / R or the gizmo to aim it.

- **Solid / Lit** (⇧Z, the 3D toolbar, View ▸ Lit shading): Solid lights meshes from the view, for working fast; Lit lights them by the scene's lights and materials as Scanline Render does (from the view if there are no lights). Lit is the default when the scene has lights. **Shadows** (toolbar, View ▸ Lit shadows) adds the lights' shadows in Lit, once the view stops moving.

### Selecting in 3D

- Left-click a card, mesh, light, camera, locator or tracker point;
  **⇧**-click adds or removes. A selected mesh shows its edges.
- Left-drag on empty space box-selects (**⇧** adds).
- **A** selects everything; **⌥A** or **Esc** deselects (**Esc** first
  cancels a box).
- Selecting an object selects its node in the graph, and the other way round.
- Right-click a Camera Tracker's selected points: **Place Card here** or
  **Place locator**.

### Moving, rotating, scaling

| Key | Does |
|---|---|
| **G** / **R** / **S** | Move / rotate / scale the selection: it follows the mouse |
| **X** / **Y** / **Z** | Constrain to that axis (press twice for the object's own axis) |
| **⇧X** / **⇧Y** / **⇧Z** | Every axis but that one |
| A number, then **Enter** | That exact amount (`G X 2 Enter`, `R Z 90 Enter`, `S 0.5 Enter`; **-** negates) |
| **⌘** (held) | Snap: 1 unit, 15°, 0.1 scale |
| Click or **Enter** | Confirm |
| Right-click or **Esc** | Cancel |
| **⌥G** / **⌥R** / **⌥S** | Reset translate / rotate / scale |

The viewer's toolbar picks a gizmo for the selected object: **Move** (axis
arrows, plane squares, a center circle), **Rotate** (axis rings, an outer
view ring, a trackball inside), **Scale** (axis cubes, the center for all
axes), **All**, or **Select** for none; **Global / Local** sets the axes. Gizmo
drags and G / R / S key animated parameters at the current frame and are
one undo step each. A parameter linked to another node can't be moved
until it's unlinked.

A selected **Spot Light** shows four square handles on the edge of its
cone: drag one out or in to widen or narrow the **Cone angle**. A selected
**Area Light** shows a handle in the middle of each edge: drag one to
resize it (about its middle; the opposite edge moves too). These are one
undo step too, and key animated parameters at the current frame.

With several objects selected, the gizmo sits at their median point (the
middle of their origins), and moving, rotating or scaling works on all of
them at once: rotate and scale turn and size each one about its own center
and carry it round the median point, as Blender's Median Point pivot does.
Objects whose position or rotation is linked to another node are left out
(a linked position still turns in place). **Alt + G / R / S** resets each
selected object.

### 3D objects

- **Cube**, **Sphere**, **Cylinder** and **Plane** (3D category) make
  meshes with their own size and segments; connect an image to their
  **texture** input to wrap it on, times **Color**.
- **Read Splat** reads a Gaussian splat capture (`.ply`, `.splat` or
  Niantic's compressed `.spz`; `.ply` as Postshot,
  Polycam, Luma, Nerfstudio, gsplat and the INRIA 3DGS code export it:
  binary or ASCII, color detail up to spherical-harmonics degree 3), with
  **Up axis** (as is, Z-up, or Y-down for COLMAP captures, which
  otherwise look upside down), Translate / Rotate / Scale, **Opacity** and
  a **Color detail** cap (lower is faster). A sample is in
  `examples/geo/splats.ply`. Splats render in Scanline Render with the
  rest of the scene: in front of, behind or through cards and meshes by
  depth; holdouts cut them out, and **Holdout** on a Read Splat makes it
  cut out what's behind it. They light themselves (no shadows from
  Arclight's lights). In the passes: depth and P where a capture becomes
  half opaque, N from the strongest splat (its flat side), motion, an id
  and a matte per Read Splat, and emission; no uv. An animated Read Splat
  motion-blurs. In the 3D
  view they show as dots while you orbit and render in full a moment
  after you stop (a selected capture shows its box; the view uses only
  the first color bands, a little less view-dependent shine, while
  renders use all of them). Re-rendering the same view of a capture
  (another pass, or an edit elsewhere in the scene) reuses its sorted
  splats, so it's quick; click near a splat (its center) to select its Read Splat,
  and **F** frames splats too.
- **Splat Crop** keeps a capture's splats inside a box or sphere
  (Center, Size, Rotate; **Invert** keeps those outside), to cut it down
  or clear floaters; everything else in the scene passes through. Its
  box or sphere shows in the 3D view (purple; orange when selected):
  click its middle to select it, then G / R / S or the gizmo place, turn
  and size it.
- **Splat Color** grades captures (**Exposure**, **Saturation**, **Gain**,
  exact from every viewing angle) and their **Opacity**; **View
  independent** drops the color's change with the angle (quicker, and
  steadier where reflections flicker).
- **Read Geo** reads an OBJ file (with its MTL colors and textures) or a
  glTF file (.gltf, .glb).
- Each has Translate / Rotate / Scale, like a Card; wire them into a Scene
  and a Scanline Render draws them with the cards.
- **Shading** sets how they take light: **unlit** (their color as it is),
  **lambert** (dull; the default) or **blinn** (with a highlight:
  **Specular** is how strong, **Roughness** how broad). **Emission** is
  light of their own. Read Geo uses the file's materials unless **Override
  material** is on.
- **Opacity** on a Read Geo fades all its meshes (each material's own
  opacity times it, shadows too), with or without Override material.
- **Opacity** on a Card fades the whole card (e.g. clouds at lower opacity
  at different depths); see-through cards and meshes (a mesh's opacity is
  its **Color**'s alpha) blend with what's behind them, nearest first,
  whatever order they're wired in. The 3D view shows it too.
- **Shading** on a Card is **unlit** by default (its image as it is);
  **lambert** or **blinn** lights it by the scene's lights, from either
  side like a real card, with Specular, Roughness and Emission as on the
  mesh nodes, and it takes shadows unless **Receives shadows** is off.

### Lights

- **Ambient Light** (everywhere, evenly), **Directional Light** (parallel,
  like the sun, down its −Z as Rotate turns it), **Point Light** (from a
  point), **Spot Light** (a cone) and **Area Light** (a glowing rectangle
  or disk) each have **Color**, **Intensity** and **Exposure** (in stops:
  +1 doubles it), and **Enabled** (off, the light stays in the 3D view but
  lights nothing).
- Point, spot and area lights fade with distance by **Decay** (0 not at
  all, 1 linearly, 2 as real light does).
- **Spot Light:** **Cone angle** across, its edge soft over **Penumbra**
  degrees; **Falloff exponent** dims it from the middle toward the edge
  (0: even across).
- **Radius** (point and spot) gives the light a soft size: highlights get
  broader and softer.
- **Area Light:** **Shape** (rectangle or disk) **Size** across, shining
  down its −Z (both ways if **Two-sided**). Its Intensity is shared over a
  **Quality** × **Quality** grid of points on it: more is smoother, and
  slower.
- **Look at:** wire a Locator, Card, Camera (any scene) into a light's
  **look at** input and the light aims at it (its first camera, card,
  mesh, locator or light), whatever its Rotate; move the target and the
  light follows. The target isn't added to the light's scene.
- **Shadows:** turn on a light's **Cast shadows** and objects between it
  and others shadow them. **Shadow softness** blurs the shadow's edge (0:
  sharp); a point or spot light's **Radius** softens it more the further
  the shadow falls from what casts it, and an area light's shadows are
  soft by themselves (each of its Quality × Quality points casts its own).
  Each object has **Casts shadows** and **Receives shadows** (a Card only
  casts). A shadow is as strong as what casts it is opaque: a 30% opacity
  card casts a 30% shadow, where its image is solid, and none where it is
  clear; turn one off to keep, say, a
  background plate from shadowing the scene. Shadows are off until you
  turn them on, so older projects render as before.
- Scripts and AI agents (MCP, Python) make lights with **create_node**
  (`light_spot`, `light_area`, …) and list them, with where they are and
  which way they point, with **scene_objects**.
- Wire lights into a Scene like objects. Without any lights, everything
  renders unlit (as cards always do); with lights, Lambert and Blinn objects
  are lit and unlit ones and cards keep their color.

## Render passes

Scanline Render can add render passes to its image: depth, position
(**P**), normal (**N**), texture coordinates (**uv**), **motion**, object
**id** and a matte per object (**matte.<node>**), and the light in parts:
**diffuse**, **specular**, **emission** (with unlit surfaces' color) and
**ambient**, which add up to the picture, **lights** (a
**light.<node>** for each light's share) and **shadow** (the light that
shadows took away: add it to the picture to take the shadows out, or
grade it to make them lighter or darker). The viewer's plane buttons show
any pass (one button a layer; **P**, **N**, **uv** and **motion** as
color).

- Choose them with **Passes** in Scanline Render's properties: **depth**,
  **P**, **N**, **uv**, **motion**, **id**, **mattes** (a **matte.<node>**
  for each object, adding up to the picture's alpha) and the lighting
  passes. Depth, P, N, uv, motion and id come from the nearest surface in
  each pixel that is at least half opaque there (where only fainter ones
  are, the nearest of those), so they aren't blurred across edges and a
  faint card in front doesn't take them from what shows through it.
- **motion** (`motion.u`, `motion.v`) is how far, in pixels (right and
  down), what each pixel shows moves by the next frame, with the camera's
  move and the objects' own animation (particles by their velocity): for
  motion blur in 2D.
- **Motion blur** (Scanline Render, 0 or 1: off, the default) renders
  that many times across the **Shutter** (in frames: 0.5 is a 180°
  shutter), **centered** on the frame, or starting or ending at it
  (**Shutter offset**), and averages them. Each pixel's times are shifted a little within the shutter, in a fixed dither pattern, so a very fast move at a low count shows fine grain rather than separate copies (and the same frame always renders the same). **Jitter shutter** (on) does that spreading; turn it off for every pixel at the same times (the look before 0.9.55: sharp copies rather than grain). Cameras, cards, meshes (their
  vertices too, if they deform) and lights move between where they are at
  the frames round each time; particles move along their velocities,
  curving with their acceleration (gravity, wind, …), so fast ones
  streak. The picture, mattes and light passes blur; depth, P,
  N, uv, motion and id stay the frame's own. 8 is usually smooth (16 for
  fast moves); each one costs about a render. Viewing the render in the
  3D view shows the blur too once the view settles (up to 8 samples;
  sharp while you move it). Particle dots streak over the same shutter.
  Looking through a moving camera or light, its own motion blurs the
  view too.
- **Holdout** on a Cube, Sphere, Cylinder, Plane or Read Geo makes it draw
  nothing but cut out what is behind it (alpha 0 there); its matte still
  shows where it is.
- **Shuffle** works on passes with any node: set **In** to a pass to see it
  as color (then grade, blur or key it), take single channels with **Red /
  Green / Blue / Alpha from** (any channel, a plane such as `N.y`, or `0` /
  `1`), set **Out** to write the color back into a pass instead, and list
  passes in **Remove** (`*` for all) to drop them once used. The ▾ beside
  each field offers the usual names.
- **Merge** with **zmerge** combines two renders by their depth: the nearer
  is in front at each pixel, its soft edges blended over what's behind.
- Writing **EXR** keeps every pass as a layer (`P.x`, `N.x`, `depth.Z`,
  `matte.<node>.A`, …) that other compositing and 3D apps read; other
  formats write the picture only.

## Particles

Particles: Particle Emitter → forces (Gravity, Wind, Turbulence, Attractor,
Vortex, Bounce, Kill, Speed Limit) → Particle Render (points, sprites,
objects) → a Scanline Render input, or Particles 2D for no camera.
Particles are simulated only where they're drawn and cached per frame;
editing a param re-simulates from the first frame it changes.

In the **3D view**, Particle Emitters show their shape and direction,
Attractors a star (and their reach), Vortices a ring round their axis,
and Bounce planes a square and normal: click to select, then **G / R /
S** or the gizmos to place them (Attractor, Vortex and Bounce only
move). In **2D**, while viewing a Particles 2D, select its Emitter to
drag its position, size and angle in the viewer.

- **Scanline Render** takes any number of scenes or particle streams:
  connect to its last (empty) input and a new one appears. With a Scanline
  Render selected, a Particle Emitter added from the node browser goes
  straight into its spare input. A **Scene** node gathers streams too.
- **Particle Emitter:** where they're born: a **Shape** (point, line,
  rectangle, disk, sphere, box, placed by Translate / Rotate / Scale; **On
  the surface** for a sphere's or box's skin), the surface or vertices of
  the **geometry** input (a card, a primitive, a file), or where the
  **mask** input's alpha is (for Particles 2D); an animated geometry or
  mask emits from where it is at each frame (a moving object leaves a
  trail), stepping on frame by frame, and an edit to it re-simulates
  only from the first frame it changes. **Rate** a frame from
  **Start** to **End frame**, plus a **Burst** at the start. They leave
  along **Direction** at **Speed** (units a second) within a **Spread**
  cone, with variations; **Life** (frames), **Size** and **Color** change
  to their end-of-life values; **Rotation** and **Spin** turn sprites and
  objects. The same **Seed** always gives the same particles;
  **Substeps** split frames for fast motion; **Pre-roll** starts early.
- **Forces and constraints**, each a node in the chain with **Strength**
  (0: off) and **Start / End frame**: **Gravity** (an acceleration),
  **Wind** (drag toward a wind velocity), **Turbulence** (swirling curl
  noise), **Attractor** (pulls to a point; negative pushes), **Vortex**
  (swirls round an axis), **Bounce** (a floor plane, with bounce and
  friction), **Kill** (by age, or inside / outside a sphere or box) and
  **Speed Limit**.
- **Particle Render:** how they look. **Points**; **sprites** (camera-facing
  squares or disks, tinted by their color, textured by the **texture**
  input, faded from **Opacity** to its end value, **over** or
  **additive** for sparks and fire); **Texture frames** takes the texture
  at the frame rendered, or as a **sequence** (its frames Sequence first
  to Sequence last, each rendered once and kept until it changes), each
  sprite showing the image **Texture images** picks: **by age** (the
  sequence once over its life), **cycle** (round and round at Rate images
  a frame, each particle starting on its own), **play** (from the first
  at Rate images a frame as it ages, holding the last) or **loop** (the
  same, round and round); with **Random start**, play and loop start
  each particle on an image of its own; or **objects** (a copy of the **objects** input's meshes at each
  particle, one at random when there are several, scaled by size and
  facing along their velocity, spinning, or as modelled). Rendered by
  Scanline Render with everything else, behind nearer surfaces; its
  **Motion blur** streaks them along their paths (curving under gravity
  and other forces), and its **motion** pass gives their motion vectors.
- **Particles 2D:** no camera: x and y are frame pixels (y down) and size
  is pixels, drawn over the **background** as Particle Render says. Pair it
  with an emitter's **mask** shape to emit where an image is.
- In the **3D view**, particles show as colored dots.
- Scripts and AI agents: create the nodes with **create_node**
  (`particle_emitter`, `particle_gravity`, …, `particle_render`,
  `particles_2d`) and read particles with **particles** (Python:
  `node.particles(frame)`): ids, positions, velocities, ages, sizes,
  colors.

## Light 2D

**Light 2D** lights a flat image: a lamp on a plate, a spot on a wall, the
sun coming from one side.

- **Type:** **spot** (from **Position** toward **Direction**, within **Cone
  angle**, its edge soft over **Softness** degrees, fading to half at
  **Reach** pixels; 0 doesn't fade), **area** (a rectangle or ellipse
  **Size** across, turned by **Rotation**, its edge soft over **Feather**
  pixels), **point** (full at Position, nothing past **Radius**, falling
  faster with a higher **Falloff**) or **directional** (across the frame
  toward Direction, fading over **Gradient** pixels past Position; 0 lights
  everything). Directions are degrees: 0 points right, 90 down. The panel
  shows only the type's own settings.
- **Color**, **Intensity** and **Exposure** (stops) set how strong;
  **Blend** puts it on by **add**, **screen** (adding, but easing off near
  white) or **multiply** (the image lit by it alone: dark where it doesn't
  reach).
- **Normals** input: a render's **N** pass (or a normal map, RGB 0–1 for
  −1–1) makes it real relighting: the image is shaded as a surface, the
  light **Height** pixels above it (a directional light at **Elevation**
  degrees), with a **Specular** highlight (**Roughness** sharp to broad).
  A **depth** input (a Z pass × **Depth scale** pixels) moves the surface
  back from the light. Without normals it's a shaped glow.
- **mask**, **Mix**, **Mask channel** and **Invert mask** limit it, as on
  other nodes. Stack several Light 2D nodes for several lights.
- **In the viewer** (node selected): drag the middle to move it, the round
  knob to aim it (**⇧** snaps to 15°), a square on a cone edge to open or
  close the cone, a dot inside it to set the softness, a point light's
  square to set its radius and a directional light's to set its gradient;
  an area light has a box's handles (corners and edges size it, the stem
  turns it). Animated settings get a key at the current frame.

## Camera Tracker

- **Track & Solve** in its panel follows features through the frame range
  and solves the camera; **Export** adds a keyed Camera, the point cloud, a
  Scene and a Scanline Render over the plate.
- With it selected, select solved points in the viewer (click, **⇧**-click,
  drag a box) to place a Card or Locator on them, or to set the origin,
  ground plane and scale.
- **Solve**: **Free move** for a camera that travels, **Tripod** for one that
  only turns about its center (a pan or tilt), or **Auto** (the default),
  which solves as Tripod when no frames show parallax and the camera only
  turns, and says so. Free move refuses a shot that only turns and
  suggests Tripod.
- A tripod solve keeps the camera in one place (the exported Camera is
  keyed on rotation only) and puts the points on a sphere **Tripod
  distance** away, as they have no depth. Cards still go on them; setting
  the scale only changes the sphere's size.

## Preferences

**Edit ▸ Preferences** (**Ctrl/⌘+,**) holds your settings, kept in
`~/.config/arclight/settings.json` (still fine to edit by hand; keys it
doesn't know are kept). **Apply**, **OK**, **Cancel**, **Reset to
defaults**.

- **General:** plugin folders, the played-frame cache (MB) and the MCP
  `--http` address (these take effect at the next start).
- **Interface:** scale, theme (follow the system, dark or light),
  snapping nodes to the graph's grid, curved or straight wires.
- **Files and safety:** **Autosave** every N minutes (0: off) writes
  unsaved changes to `<project file>.autosave` beside the project (an
  unsaved project's to `~/.local/state/arclight/autosave/`), never over
  the project, and removes it when you save; **File ▸ Open Recent** shows
  the last N projects; **Undo levels**; the **Projects folder** (where
  File ▸ New makes projects' folders and where Open starts).
- **New project:** the format, frame rate and frames File ▸ New starts
  with (or use **Save as defaults** in Project Settings).
- **3D view:** **Emulate 3-button mouse** (Alt + left-drag navigates);
  two-finger scroll orbits (⇧ pans) or pans (⇧ orbits); zoom toward the
  pointer or the middle of the view; **Invert zoom** (wheel, ⌃ + scroll,
  ⌃ + middle-drag); **Shading** for projects without their own 3D view:
  Automatic (as last chosen, else Lit with lights), Solid or Lit.

## Undo

**⌘Z** undoes, **⇧⌘Z** redoes.

## Help

**F1** opens this window; **Esc** closes it. Help ▸ About shows the version
and build.
