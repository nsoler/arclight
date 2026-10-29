# Arclight Help

Arclight is a node-graph compositor: build a graph of nodes in the
**Node Graph** at the bottom, see the result in the **Viewer**, and edit the
selected node in **Properties** on the right. On macOS, ⌘ stands for Ctrl.

## Node graph

| Action | How |
|---|---|
| Add a node | Right-click (the node browser; type to search), or over a wire to insert it there |
| Connect | Drag from an output (circle) to an input (square), or the other way |
| Disconnect / reroute | Drag a connected input away |
| View a node | Double-click it, or **V** |
| Delete | **⌫** (Backspace) or **Delete** |
| Frame all | **F** |
| Pan | Middle-drag or **⌥**-drag (Alt) |
| Zoom | Scroll or pinch |

Adding a **Camera** brings a Scene and a Scanline Render with it, wired in;
hold **⇧** (Shift) when choosing it for the Camera alone.

## Selecting nodes

- Click to select; **⇧** or **⌘** click to add or remove.
- Drag a box on empty space (**⇧** adds).
- **⌘A** selects all, **Esc** none.
- **⌘G** groups the selection in a backdrop, **⇧⌘G** ungroups.

## Viewer

- Drag (or middle-drag) to pan; scroll or pinch to zoom.
- Double-click or **F** to fit.
- **Tab** (with the pointer over the viewer) switches between **2D** (the
  image) and **3D** (the scene); so do the **2D / 3D** buttons.
- The selected node's handles draw over the image: drag them to edit (a
  Transform's gizmo, shape boxes and vertices, tracker boxes, corner pins).
  **⌘**-click (or **⌘**-drag) in a Keyer picks the screen color.

## Player

| Key | Does |
|---|---|
| **Space** | Play / pause |
| **J** / **K** / **L** | Play backward / stop / play forward |
| **←** / **→** | Step a frame |
| **⌥←** / **⌥→** | Previous / next key |
| **Home** / **End** | First / last frame |

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
| **0** | Through the scene camera (its frame outlined, outside darkened) |
| **2** / **4** / **6** / **8** | Orbit 15° down / left / right / up |
| **.** or **F** | Frame the selection |
| **Home** | Frame everything |
| **`** (backtick) | A pie of views: point at one and click, or let go of the key |
| **Z** | Meshes solid (lit from the view) or as wireframes |

The number row works as well as the numpad. Axis views switch to
orthographic until you orbit away. Orbiting leaves a camera view for the
free view from where the camera was.

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

### 3D objects

- **Cube**, **Sphere**, **Cylinder** and **Plane** (3D category) make
  meshes with their own size and segments; connect an image to their
  **texture** input to wrap it on, times **Color**.
- **Read Geo** reads an OBJ file (with its MTL colors and textures) or a
  glTF file (.gltf, .glb).
- Each has Translate / Rotate / Scale, like a Card; wire them into a Scene
  and a Scanline Render draws them with the cards.
- **Shading** sets how they take light: **unlit** (their color as it is),
  **lambert** (dull; the default) or **blinn** (with a highlight:
  **Specular** is how strong, **Roughness** how broad). **Emission** is
  light of their own. Read Geo uses the file's materials unless **Override
  material** is on.

### Lights

- **Ambient Light** (everywhere, evenly), **Directional Light** (parallel,
  like the sun, down its −Z as Rotate turns it), **Point Light** (from a
  point) and **Spot Light** (a cone, **Cone angle** across, soft over
  **Penumbra** degrees) each have **Color** and **Intensity**; point and
  spot lights fade with distance by **Decay** (0 not at all, 2 as real
  light does).
- Wire them into a Scene like objects. Without any lights, everything
  renders unlit (as cards always do); with lights, Lambert and Blinn objects
  are lit and unlit ones and cards keep their color.

## Render passes

Scanline Render can add render passes to its image: depth, position
(**P**), normal (**N**), texture coordinates (**uv**), object **id** and a
matte per object (**matte.<node>**), and the light in parts: **diffuse**,
**specular**, **emission** (with unlit surfaces' color) and **ambient**,
which add up to the picture, and **lights** (a **light.<node>** for each
light's share); motion is still to come.
The viewer's plane buttons show any pass (one button a layer; **P**, **N**
and **uv** as color).

- Choose them with **Passes** in Scanline Render's properties: **depth**,
  **P**, **N**, **uv**, **id**, **mattes** (a **matte.<node>** for each
  object, adding up to the picture's alpha) and the lighting passes. Depth, P, N, uv and id come
  from the nearest surface in each pixel, so they aren't blurred across
  edges.
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

## Undo

**⌘Z** undoes, **⇧⌘Z** redoes.

## Help

**F1** opens this window; **Esc** closes it. Help ▸ About shows the version
and build.
