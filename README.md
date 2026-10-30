# Arclight

An original **node-graph image compositor** written in Rust. Arclight reads a
JSON description of an image-processing DAG, evaluates it, and writes the
result. It ships as a command-line renderer (`arclight`) and an interactive desktop
app (`arclight-gui`) with a node editor, viewer, player and properties panels.

This is original work — its design and code are not derived from any existing
compositing product.

**Download the latest release:**
[github.com/nsoler/arclight/releases/latest](https://github.com/nsoler/arclight/releases/latest)
(Linux x86_64 tarball, macOS universal DMG for Apple Silicon and Intel,
Python wheels). All releases: [Releases](https://github.com/nsoler/arclight/releases).

## Design

- **Pixels**: 32-bit float RGBA.
- **Color space**: linear light. sRGB encode/decode happens only at the 8-bit
  (PNG) boundary.
- **Alpha**: **premultiplied (associated)** throughout the pipeline — the
  mathematically correct space for compositing and filtering. Blurring and
  merging in this space is why soft edges fade cleanly with no dark fringing.
- **Graph**: a JSON list of nodes (`id`, `op`, `inputs`, `params`), topologically
  sorted (with cycle detection) and evaluated once per node, results cached.
- **Parallelism**: per-pixel work is parallelized across cores with Rayon.

## Build

Version **0.9.48** (alpha); see `CHANGELOG.md`.

```sh
cargo build --release      # -> target/release/arclight, target/release/arclight-gui
cargo test                 # unit + graph tests
cargo build --release --no-default-features   # CLI only, no GUI dependencies
cargo build --release -p arclight-plugin-example  # example native plugin
python/build.sh                                   # Python `arclt` module (needs Python ≥ 3.10 headers)
```

Video input/output needs `ffmpeg` and `ffprobe` on `PATH` (e.g. `brew install ffmpeg`).
Image sequences and stills need nothing extra.

### App launcher (Linux)

```sh
./install.sh            # add Arclight to the app launcher (builds first if needed)
./install.sh --build    # rebuild the release binary, then install
./uninstall.sh          # take it out again
```

`install.sh` puts `arclight.desktop` in `~/.local/share/applications` and the
icons in `~/.local/share/icons/hicolor`: the logo without its lettering
(`assets/arclight-icon.svg`) as the scalable icon and at 16–64 px, where
launchers draw it and the word would be unreadable, and the full logo
(`assets/arclight.svg`) at 128 and 256 px, so it shows in Omarchy's launcher (Super +
Space) and any freedesktop desktop's menu. The entry runs this checkout's
`target/release/arclight-gui`, so later release builds need no reinstall.
No root needed; nothing outside `~/.local/share` is touched.

### Linux release tarball

```sh
packaging/linux/build-tarball.sh   # -> dist/arclight-<version>-linux-x86_64.tar.gz
```

The tarball holds `bin/arclight-gui`, `bin/arclight`, the example clip and
icons. Unpack it and run `./install.sh` to put the binaries in
`~/.local/bin` and Arclight in the app launcher (`PREFIX=… ./install.sh`
for elsewhere; `./uninstall.sh` removes it), or run `bin/arclight-gui` in
place. The binaries link only glibc, so they run on the distribution they
were built on (Arch / Omarchy for the published releases) and newer ones.
Releases are on GitHub (`gh release list`), with the macOS DMG (universal:
Apple Silicon and Intel) and the Python wheels beside the tarball.

### macOS (Apple Silicon and Intel)

```sh
brew install librsvg ffmpeg       # icon drawing for the build; video I/O at run time
packaging/macos/build-dmg.sh      # -> dist/Arclight.app, dist/Arclight-<version>-universal.dmg
ARCHS=arm64 packaging/macos/build-dmg.sh   # one architecture only (-arm64.dmg; or x86_64)
```

The app is **universal**: its binaries hold an Apple Silicon (arm64) and
an Intel (x86_64) build, joined with `lipo`, so the one DMG runs natively
on any Mac with macOS 11 or later. The Intel half is cross-compiled on
the Apple Silicon build machine.

Run on a Mac: Apple's SDK, `iconutil` and `hdiutil` only run there. Without
one, the **macOS build** GitHub Actions workflow (`.github/workflows/macos.yml`)
does the same on a GitHub-hosted M-series Mac, started by hand from the
Actions tab or by pushing a `v*` tag; the DMG is attached to the run and,
for a tag, to that tag's GitHub release. The
app is signed ad hoc, not with a Developer ID, so a downloaded copy opens
the first time with right-click → **Open** (or `xattr -dr
com.apple.quarantine /Applications/Arclight.app`). ffmpeg isn't bundled:
Arclight finds Homebrew's (`/opt/homebrew/bin`) or MacPorts' even when
opened from the Finder (on Intel Macs Homebrew lives in `/usr/local/bin`,
which is searched too). The CLI is inside the bundle at
`Arclight.app/Contents/MacOS/arclight`. Plugins on macOS are
`libarclight_plugin_*.dylib`.

## GUI

```sh
cargo run --release --bin arclight-gui                 # starter project
cargo run --release --bin arclight-gui -- comp.json    # open a graph
```

| Area | What it does |
|------|--------------|
| **Viewer** (center) | Shows the input of the first `viewer` node (or the selected node). Double-clicking another node (or **V**) shows that node instead, without rewiring the Viewer (an eye marks it, and the status line says so) until the Viewer node is clicked or double-clicked. A node with an input left unplugged treats it as empty, so the comp below it keeps showing; only output nodes (Viewer, Write, Media Out) report an unconnected input. Drag (or middle-drag) to pan, scroll/pinch to zoom, double-click or **F** to fit. RGB/R/G/B/A channel isolation (images with extra planes, e.g. an EXR with a Z channel, read as `depth`, get a button per plane beside them, showing it as gray from its lowest to its highest value, 0 staying black; the readout adds the plane's value), exposure in stops, float pixel readout under the cursor, and a color picker at the right end of the viewer's toolbar: a swatch of the hovered pixel as displayed with its R, G, B and A values (linear, premultiplied); it keeps reading that pixel after the pointer leaves, through playback, and its tooltip gives the position, 8-bit values and hex. When an image stores pixels for only part of the frame, or past it (its *data window*: a small shape, a layer moved off-frame), the window is outlined dashed, its position and size show next to the frame size, and pixels past the frame show dimmed. The selected node's handles draw over the image: **Transform** outlines the transformed layer and shows a gizmo at its center (the input layer's middle unless Center is set; the panel's **Auto** puts it back) (drag anywhere on the layer or its center to move, ⇧ for one axis; the frame's corners scale uniformly, ⇧ for free, and its edges scale one axis; the ring to rotate, ⇧ snaps to 15°; the red/green handles to scale x/y, ⇧ for uniform), **Crop** shows its box (drag edges or corners), **Rectangle**/**Ellipse** show a box (drag inside to move, corners/edges to size about the center, ⇧ keeps the aspect; the stem above the top edge rotates), **Polygon** shows its vertices (drag inside the shape to move all of it, which also selects every vertex; drag a vertex to move it, or an edge to move its two ends; ⇧-click to select several and drag them together, drag on empty space to box-select (⇧ adds, also from inside the shape; middle-drag still pans), ⌘/Ctrl+A selects all; with two or more selected, a dashed transform box surrounds them: drag inside to move, corners to scale about its center (⇧ free), edges to stretch one axis, the stem above to rotate (⇧ snaps to 15°), and Roto tangents turn and scale along, click an edge without dragging to insert a vertex, ⌘/Ctrl-click to add one after the selected vertex, **⌫** deletes selected vertices), **Roto** works the same on its curve and also shows the selected vertices' tangent handles (drag one to reshape the curve smoothly, ⌥/Alt-drag to break the tangent; Alt-drag a vertex to pull out new tangents; ⌘/Ctrl-click-drag adds a vertex and pulls out its tangents; clicking the curve splits it without changing its shape, dragging it moves the two vertices either side). **Tracker** shows each enabled track's pattern box (solid), search box (dashed) and tracked path; drag inside a pattern box to place or correct a track (keys the current frame), drag a box corner to size that track's box (⇧ sizes it for every track, back to the panel's shared Pattern/Search size). ⌘/Ctrl-drag a box to move it off its point (an offset: follow a textured feature, report a point beside it, e.g. one that gets covered); **Clear offsets** puts them back. The panel's **Per-track boxes** table shows and edits each track's own pattern and search sizes (shared ones marked ·) and offset; ↺ returns a track to the shared sizes. Frames where a track matched weakly show as orange dots on its path and orange bands on the timeline; the panel names the weakest match (click it to go there), and **Clear ◀ / ▶** removes the enabled tracks' keys before or after the current frame. While the viewer shows the Tracker's own match-moved or stabilized output, the boxes are drawn (and dragged) where that output shows them. **Keyer**: ⌘/Ctrl+click picks the screen color from the Keyer's input (whatever the viewer shows), ⌘/Ctrl+drag averages a box. **Corner Pin** shows its To quad (solid, numbered corners) and From quad (dashed); drag either's corners. **Light 2D** shows its position (drag to move), aim knob (⇧ snaps to 15°), cone edges and softness dots (a spot), radius (a point) or gradient line (a directional light), or an area light's box. Handle drags key animated params at the current frame; inserting or deleting vertices applies to every key. |
| **Player** (under the viewer) | Go to start, step back, play backward, pause, play forward, step forward, go to end. Scrubbable timeline (green strip = frames already cached), in/out, FPS, loop, and the project **Format** (frame size for sources whose own size is 0). Plays in real time and shows the achieved fps. |
| **Curve editor** (bottom, second tab) | The selected node's keyframed params as value-over-time curves, one per channel (x/y, r/g/b/a), with a toggle per channel. Click / Shift-click / box-drag to select keys, drag to move them in time and value, double-click a curve to add a key, **⌫** to delete. Smooth/Linear/Step buttons set interpolation. Selected keys on smooth curves show tangent handles: drag one to set the slope (in and out move together), **⌥**-drag to break the key so each side moves on its own; hollow handles are automatic, filled are user-set. Tangents Auto/Flat/Break-Unify buttons act on the selected keys. Scroll to pan, ⌘-scroll or pinch to zoom (⌥ value only, ⇧ time only), middle-drag to pan, **F** to frame. The ruler scrubs the playhead. |
| **Node graph** (bottom) | Right-click the canvas, or press **⇧E**, or **⇧Tab** with the pointer over the graph, to open the searchable node browser at the pointer (type to filter, ↑/↓ and Enter or click to place, Esc or a click elsewhere to close) and add nodes. With a node selected, the new node is wired after it; with a Scanline Render selected, a new scene or particle node (a Particle Emitter, a Card, …) goes into its spare input. **Wires:** click one to select it (it lights up; **⌫** disconnects it); right-click one for **Insert 3D Camera Setup**, **Insert Node…** (the browser, inserting into that wire) or **Delete connection**. **3D Camera Setup** (the wire menu, **Edit ▸ Add 3D Camera Setup**, or Camera in the browser; ⇧ for the Camera alone) adds a Camera, a Scene and a Scanline Render, wired up, in a "3D Setup" backdrop just before the Viewer (one is added if there's none), so the render shows straight away, the Viewer's picture becoming its background; from the wire menu (or the browser aimed at a wire) it goes into that wire instead, the wire's picture becoming the background. The first in a project switches the viewer to 3D, framed on the scene, with a hint that Tab switches back; the viewer's 3D button shows a dot whenever the viewed node has a scene. Drag an unconnected node onto a wire (it lights up) to splice it in; shake a node while dragging it to pull it out of its wires (its input then feeds what it fed). The minimap in the lower right shows the whole graph and the view: click or drag in it to move there, scroll over it to zoom into that spot, double-click to fit. Inputs are yellow squares (hollow when optional), outputs green circles; a masked node's `mask` input is violet and labelled (hover it for what it does). **Fit** (top-right corner of the Node Graph) or **F** with the pointer over it frames every node and backdrop. Drag from an output to an input (or back) to connect; drag a connected input away to reroute or disconnect. Drag from an output onto a wire (it lights up) to join the two with a new Merge: the dragged node goes into A, over the wire's source in B (e.g. drop a Keyer onto the background's wire to put the keyed foreground over it). Double-click a node or press **V** to view it (click the Viewer node to go back to its output). **⌫** deletes the selected nodes (each chain is reconnected). Drop image/video files onto the window to create Media In nodes. **Selecting:** click a node; **⇧**- or **⌘**-click adds or removes one; drag on empty space for a selection box (it takes every node it touches; **⇧** adds); **⌘A** (pointer over the graph) selects all, **Esc** none. Drag any selected node to move them all (one undo step). The Properties panel shows the last node picked. **Panning:** middle-drag or **⌥**-drag on empty space (a plain drag now selects), scroll to pan, pinch or ⌘-scroll to zoom. **Backdrops:** **⌘G** (or right-click a node → Group) puts a box behind the selected nodes, fitted to them; drag its title bar to move it together with the nodes (and backdrops) lying fully inside it when the drag starts; drag a corner to resize; double-click the title to rename it; the swatch at the right of the title picks its color. Select one by its title to edit Title, Color and Title size in Properties. **⇧⌘G**, **⌫**, its right-click menu or **Delete box** removes the box and leaves the nodes. |
| **Properties** (right) | One panel per opened node, newest first, generated from each op's parameter specs. Media In: browse, auto-detected frame range, clip info. Media Out: browse, render the frame range with progress and cancel. |

Shortcuts: **Space** play/pause · **J/K/L** back/stop/forward · **←/→** step ·
**⌥←/⌥→** previous/next keyframe · **Home/End** · **⌘Z/⇧⌘Z** undo/redo ·
**⌘O/⌘S/⇧⌘S/⌘N** open/save/save as/new · **⇧E** node browser · **⇧Tab** (over the graph) node browser · **⇧R/⇧T/⇧M** (over the graph) add Media In/Transform/Merge ·
**⌘A** select all (over the graph; over the viewer, every point of the
selected shape) · **Esc** select none · **⌘G / ⇧⌘G** group / ungroup.

### 3D view (Blender-style)

The viewer's **2D | 3D** toggle (or **Tab** with the pointer over the
viewer) shows the 3D scene of the node being viewed (a Camera, Card,
Scene, Locator, Point Cloud, Camera Tracker, or a Scanline Render's scene
input); a node without one says so. The viewport camera is saved with the
project.

- **Navigate:** middle-drag orbits, **⇧** + middle-drag pans, scroll or
  **Ctrl** + middle-drag zooms toward the pointer. Trackpad: two-finger
  scroll orbits, **⇧** pans, pinch or **Ctrl** zooms. Without a middle
  button, turn on *Emulate 3-button mouse* (Alt + left-drag).
- **Views:** numpad **1 / 3 / 7** front, right, top (**Ctrl**: the
  opposite sides), **5** perspective / orthographic, **0** (or **View ▸
  Look through selected**) through the selected camera, spot light or area
  light, else the scene camera (its film frame, outside darkened), **2 / 4 / 6 / 8**
  orbit 15°, **.** or **F** frame the selection, **Home** frame all;
  **`** (backtick) opens a pie of views around the pointer. The
  navigation gizmo (top right) does the same with the mouse: click an
  axis, drag the ball to orbit, or use the pan, zoom, camera and ortho
  buttons.
- **Draws** the ground grid and axes, cards textured with their images
  (at lower resolution while the view moves), outlines, camera frusta and
  animated cameras' paths, locators, and point clouds (a Camera Tracker's
  colored by error: green under 0.25 px, yellow about 1 px, red from
  2 px).
- **Select:** left-click a card, camera, locator or point (**⇧** adds or
  removes), left-drag on empty space to box-select, **A** all, **Alt+A**
  or **Esc** none. The node graph and Properties follow, and the other
  way round. Right-click a Camera Tracker's selected points to Place Card
  here or Place locator.

**Transforming in 3D** (Blender-style). With an object selected and the
pointer over the 3D view: **G** moves, **R** rotates, **S** scales it,
following the pointer (Move: in the view plane at the object's depth;
Rotate: by how far the pointer turns about the object's center; Scale: by
how much farther from its center the pointer is). Then **X / Y / Z**
constrains to that world axis (press it again for the object's own axis,
a third time to free it), **⇧ X / Y / Z** excludes that axis, typing a
number sets the amount exactly (`G X 2 Enter`, `R Z 90 Enter`,
`S 0.5 Enter`; **-** negates, Backspace edits), **Ctrl** snaps (1 unit,
15°, 0.1); **Enter** or a click confirms, **Esc** or a right-click
cancels and puts everything back. **Alt + G / R / S** resets translate /
rotate / scale. The viewer toolbar's **Select / Move / Rotate / Scale /
All** pick the gizmo on the selected object: drag an arrow to move along
an axis, a square to move in that plane, the center circle to move in the
view plane; a colored ring to rotate about that axis, the white outer
ring about the view, inside it like a trackball; a cube to scale along
that axis, the center to scale evenly. **Global / Local** sets the
gizmo's axes. Every move, rotate or scale sets a key when the param is
animated (at the current frame) and is one undo step; a linked param
(e.g. a Card linked to tracker points) shows the gizmo gray and says what
it's linked to: Unlink it in Properties to move it by hand. A selected spot
light shows handles on its cone's edge (drag to change the cone angle) and
an area light on its edges' middles (drag to resize it about its middle);
these key and undo the same way.

**Help → Arclight Help…** (or **F1**) opens a scrollable help window with
these controls and more (the text is `docs/help.md`, built into the app;
edit it to change the help); **Help → About Arclight Compositor…** shows
the version and when the binary was built (UTC; `build.rs` stamps it, or
uses `SOURCE_DATE_EPOCH` when set).

**Keyframes.** Every numeric parameter has a ◇ next to its name: grey = static,
orange outline = animated, filled = key on this frame. Click it to set or remove
a key at the current frame. Once a parameter is animated, editing it sets a key
at the current frame automatically. Right-click the ◇ for interpolation
(smooth / linear / step), previous/next key, and *Delete animation*. The
selected node's keys are marked on the timeline, and animated nodes show a
small diamond in the graph. Use the Curve Editor tab to retime keys and shape
curves visually. Frame ranges and rates are not animatable.

**Undo/redo** covers every graph edit: nodes, connections, parameters, keys,
moves, renames and the timeline range. A whole gesture (a slider drag, a node
move, typing in a field) is one step; history holds 200 steps.

Rendering happens on a background thread. Each node's result is cached under a
hash of its op, params, inputs and (for time-varying nodes) the frame, so an
edit only re-renders that node and what is downstream of it. Finished viewer
frames are kept in a 1.5 GB LRU, so scrubbing back over frames is instant.

## Use

```sh
# Render a graph: `write` nodes write one frame (--frame, default 1);
# `media_out` nodes write their whole frame range
arclight render examples/basic.json -v

# Generate and render a built-in demo (writes arclight_demo.{json,png})
arclight demo ./out
```

## Graph format

```json
{
  "nodes": [
    { "id": "bg",  "op": "checkerboard", "params": { "width": 640, "height": 480 } },
    { "id": "fg",  "op": "read",         "params": { "path": "plate.exr" } },
    { "id": "cc",  "op": "grade", "inputs": ["fg"], "params": { "multiply": [1.1,1.0,0.9] } },
    { "id": "out", "op": "merge", "inputs": ["cc","bg"], "params": { "operation": "over" } },
    { "id": "w",   "op": "write", "inputs": ["out"], "params": { "path": "comp.png" } }
  ]
}
```

Any numeric param can be keyframed by replacing its value with a curve —
`{ "keys": [[frame, value], ...], "interp": "smooth" | "linear" | "step" }` —
where each value has the param's normal shape. Curves hold before the first
and after the last key; `smooth` is a cubic Hermite with automatic tangents
(slope across the neighbouring keys, flat at the ends). A key may carry a third
element with user tangents — `[frame, value, { "in": s, "out": s, "broken": true }]`
— slopes in value units per frame, shaped like the value, `null` = automatic.

A node's `inputs` are the ids of upstream nodes, one per input port (`""` =
unconnected). `write` passes its input through unchanged, so it can sit
anywhere in the graph. The GUI also stores a `pos` per node and a top-level
`project` (`first_frame`, `last_frame`, `fps`, `format` {`width`, `height`},
default 1920×1080), and `backdrops` when there are any: `{ "id", "title",
"pos": [x, y], "size": [w, h], "color": [r, g, b] (sRGB 0–1), "font_size" }`
(files without them load as before); the engine ignores `pos` and
`backdrops`. Sources with no input
(`constant`, `checkerboard`, shapes without a background) whose `width` /
`height` is 0 (the default for new nodes) take the project format. Images
carry a frame (format) and a data window, the part where pixels are stored,
which can be smaller than the frame (a shape stores only its bounds) or reach
past it (a layer moved off-frame keeps its pixels, up to one frame size
beyond each edge).

## Operators

Ops marked "+ mask" have an optional `mask` input (a violet port, labelled
`mask`, on the node) and `mix`, `mask_channel` and `mask_invert` params: the
result is blended back toward the main input (B for merge/keymix) by
`mix × mask`, where the mask is its `mask_channel` (`alpha` by default, or
`red` / `green` / `blue` / `luminance`), flipped by `mask_invert`. Outside
the mask image it counts as empty (inverted: full). With no mask, `mix`
applies everywhere and `mask_invert` does nothing.

| op | inputs | key params | notes |
|----|--------|-----------|-------|
| `media_in` | 0 | `path`, `first_frame`, `last_frame`, `fit` (`none` / `fit` / `fill` / `stretch` / `width` / `height`) | still, sequence (`shot.####.exr` / `shot.%04d.exr`) or video (via ffmpeg); holds outside the range. `fit` scales it to the project format (follows it when it changes): fit = all of it with transparent bars, fill = covering it and cropped, stretch = exactly, ignoring the aspect, width / height = matching that and keeping the aspect; new nodes default to `fit`, projects saved without it read as `none` |
| `read` | 0 | `path`, `fit` (as `media_in`) | an image (`.exr` = linear float; others decoded from sRGB 8-bit) or a video (its frames from frame 1, holding the first and last outside); scaled to the project format as `fit` says |
| `constant` | 0 | `width`, `height`, `color` [r,g,b,a] | solid fill (color is straight/unassociated) |
| `checkerboard` | 0 | `width`, `height`, `size`, `color_a`, `color_b` | test pattern |
| `grade` | 1 + mask | `multiply` [3], `offset` [3], `gamma` [3] | lift/gain/gamma on unassociated color |
| `exposure` | 1 + mask | `stops` | ×2 per stop |
| `light_2d` | 1–3 (`normals`, `depth`) + mask | `type` (spot / area / point / directional), `position`, `direction`, `cone`, `softness`, `reach` (spot), `shape`, `size`, `rotation`, `feather` (area), `radius`, `falloff` (point), `gradient` (directional), `color`, `intensity`, `exposure`, `blend` (add / screen / multiply), `height`, `elevation`, `specular`, `roughness`, `depth_scale` | lights the image in 2D: a shaped glow, or with a normals input (an N pass or normal map) real relighting; viewer handles for position, aim, cone, softness, radius / gradient and an area's box |
| `saturation` | 1 + mask | `saturation` | about Rec. 709 luminance; 0 = grayscale |
| `invert` | 1 + mask | `channels` (`rgb`/`alpha`/`rgba`) | on unpremultiplied values |
| `clamp` | 1 + mask | `min`, `max`, `channels` | alpha always kept in 0–1 |
| `premult` / `unpremult` | 1 | — | color × alpha / color ÷ alpha |
| `channel_select` | A, B (optional) | `red`, `green`, `blue`, `alpha` | each from `A red` … `B alpha`, `zero`, `one` |
| `shuffle` | 1 | `in`, `red`, `green`, `blue`, `alpha`, `out`, `remove` | moves render passes: `in` brings a pass (`P`, `N`, `depth`, `matte.<node>`, `diffuse`, …) into RGBA; each channel then from `R`/`G`/`B`/`A`, any plane (`N.y`) or `0`/`1`; `out` writes the result into a named pass instead (the color kept); `remove` drops passes (comma- or space-separated, `*` for all) |
| `merge` | A, B + mask | `operation`, `swap` | `swap` (**Swap A / B**, the ⇄ toggle beside Operation) trades A and B for every operation: B over A, in A's frame, masked toward A; `over`, `under`, `add`, `multiply`, `screen`, `min`, `max`, `zmerge` (by the inputs' `depth` passes: the nearer over the farther at each pixel, soft edges blended, A in front on a tie, other passes carried); outside the mask = B |
| `keymix` | A, B + mask | — | A where mask alpha is 1, B where 0 |
| `keyer` | input, clean plate (opt) + mask | `screen` (color, or null = estimated from the image), `balance`, `clip_black`, `clip_white`, `despill`, `shrink_grow` (px, negative shrinks), `softness` (px the matte's edge blurs over), `output` (`keyed`/`matte`/`despill only`) | green/blue screen color-difference key with despill; a clean plate gives a per-pixel screen color |
| `dissolve` | A, B | `amount` | cross-fade A → B |
| `switch` | 5 (optional) | `which` | passes input `which` through; keyframe it to cut |
| `transform` | 1 + mask | `translate` [2], `scale` [2], `rotate` (deg), `center` [2] or null | affine, bilinear resample (averaged over each pixel's footprint where it shrinks, as Corner Pin and the Tracker output also do), about `center`; null (the default) pivots on the middle of the input's data window, so a Transform after a shape turns it in place; `motion_blur`, `shutter`, `shutter_offset` as for shapes (the animated warp, averaged over the shutter) |
| `corner_pin` | 1 + mask | `to1`–`to4` [2], `from1`–`from4` [2] | perspective warp taking the From quad onto the To quad; `motion_blur`, `shutter`, `shutter_offset` as for shapes (the animated warp, averaged over the shutter); the panel's **From = image corners** / **To = image corners** put a quad on the input's frame corners |
| `lens_distort` | 1 + mask | `mode` (`undistort`/`distort`), `k1`, `k2`, `center` [2] or null (the frame's center), `mix` | removes or adds a lens's radial distortion (Brown model, r in half frame widths); negative K1 is barrel, positive pincushion; undistort straightens a plate's lines, distort with the same values bends CG to match; keeps overscan, carries extra planes |
| `crop` | 1 | `left`, `top`, `right`, `bottom`, `mode` | `clear outside` or `shrink canvas` |
| `resize` | 1 | `width`, `height`, `fit` (`stretch`/`fit`/`fill`) | box-filtered down, bilinear up |
| `blur` | 1 + mask | `radius`, `passes` | separable box blur; 3 passes ≈ gaussian |
| `rectangle` / `ellipse` | background (optional) | `center` [2], `size` [2], `rotate`, + shape params | rotate is clockwise on screen |
| `polygon` | background (optional) | `points` [x0,y0, x1,y1, …], `fill_rule` (`non-zero`/`even-odd`), + shape params | the whole point list animates as one value |
| `roto` | background (optional) | `points` [x, y, in dx, in dy, out dx, out dy, …], `fill_rule`, + shape params | one closed bezier; handles are offsets from their vertex, so moving a vertex carries them; the whole list animates as one value |
| `tracker` | 1 | `track1`–`track4` [2] + `track<i>_on`, `pattern`, `search` (box sizes, px), `min_score`, `update` (`first frame`/`every frame`), `motion` (`translate`/`translate, rotate`/`translate, rotate, scale`: what the matcher follows besides position), `output` (`input`/`match-move`/`stabilize`), `solve` (`translate`/`translate, rotate`/`translate, rotate, scale`), `reference_frame` | the panel's Track buttons key each track's position per frame (and, when Track motion follows rotation, its turn and growth in `track<i>_warp` = [degrees, scale], so one track can solve rotation and scale) (NCC on luminance, sub-pixel); the output applies or removes the motion relative to the reference frame; **To Transform: Match-move / Stabilize** adds a Transform keyed with that motion on every tracked frame, to move another layer with the shot (or steady it); with all four tracks on, **To Corner Pin** adds a Corner Pin that follows them in perspective; `motion_blur`, `shutter`, `shutter_offset` blur the match-move/stabilize output over the shutter, as for Transform |
| `write` | 1 | `path` | `.exr` = float, with every render pass as a layer (`depth.Z`, `P.x`, `N.x`, `uv.u`, `motion.u`, `matte.<node>.A`, `diffuse.R`, …), read back by name; others = 8-bit sRGB, the color only |
| `media_out` | 1 | `path`, `first_frame`, `last_frame`, `fps`, `dpx` (DPX encoding), `codec` (`auto`/`H.264 8-bit`/`HEVC 10-bit`/`ProRes 422 HQ 10-bit`/`ProRes 4444 + alpha`) | renders a range to a sequence or a video; `auto` = ProRes 422 HQ for `.mov`, H.264 otherwise; ProRes needs `.mov`/`.mkv`; the 10-bit codecs are fed 16-bit frames, ProRes 4444 keeps alpha |
| `viewer` | 1 | — | pass-through; marks what the GUI viewer shows |
| `camera` | 0 | `focal` (mm), `film_back` [2] (mm), `translate`/`rotate`/`scale` [3], `near`, `far` | 3D: outputs a scene holding a camera (right-handed, Y up, looking down −Z; rotate in degrees, X then Y then Z) |
| `card` | 1 | `translate`/`rotate`/`scale` [3], `size` [2] or null, `opacity` (0–1, animatable), `shading` (unlit (default) / lambert / blinn), `specular`, `roughness`, `emission`, `casts_shadows`, `receives_shadows` | 3D: puts its input image on a flat card (1 wide by default, keeping the image's aspect), faded by Opacity (its shadow too, as strong as the card is opaque); unlit by default (the image as it is), or with Shading lambert / blinn lit by the scene's lights from either side like a real card, taking shadows unless Receives shadows is off; outputs a scene |
| `cube` / `sphere` / `cylinder` / `plane` | 0–1 (`texture`) | `translate`/`rotate`/`scale` [3], size (or `radius` / `height`), `segments`, `caps` (cylinder), `color`, `shading` (unlit / lambert / blinn), `specular`, `roughness`, `emission`, `holdout`, `casts_shadows`, `receives_shadows` | 3D: a primitive mesh, colored or textured by its optional input, lit by the scene's lights (none: unlit); outputs a scene |
| `read_geo` | 0 | `file`, `translate`/`rotate`/`scale` [3], `override_material` with `shading`, `specular`, `roughness`, `emission`; `holdout`, `casts_shadows`, `receives_shadows` | 3D: imports a mesh: `.obj` (with its `.mtl` materials) or glTF `.gltf` / `.glb` (node transforms applied, PBR materials mapped); outputs a scene |
| `light_ambient` / `light_directional` / `light_point` / `light_spot` / `light_area` | 0–1 (`look at`, a scene; not ambient) | `enabled`, `color`, `intensity`, `exposure` (stops), `shadows` (cast shadows, off by default) and `shadow_softness` (texels; not ambient); `translate` (not ambient), `rotate` (directional, spot, area), `decay` (point, spot, area), `radius` soft size (point, spot), `cone`, `penumbra`, `falloff` (spot), `shape` rectangle/disk, `size`, `two_sided`, `quality` (area: a quality × quality sample grid) | 3D: a light shining down its −Z (point: every way; ambient: from everywhere; area: from a rectangle or disk), aimed at what's in `look at` if wired; outputs a scene |
| `scene` | 4 scenes | — | 3D: gathers up to four cameras, cards, meshes, lights or scenes into one scene |
| `scanline_render` | scene, camera (scene, optional), background (optional), then any number of scenes or particle streams (connect to the last, empty input and a new one appears) | `width`, `height` (0 = project format), `samples` (anti-aliasing, n×n per pixel, default 3), `passes` (any of `depth`, `P`, `N`, `uv`, `motion`, `id`, `mattes`, `diffuse`, `specular`, `emission`, `ambient`, `lights`, `shadow`; default `["depth"]`), `motion_blur` (renders across the shutter, 0/1 = off, the default), `shutter` (frames, default 0.5), `shutter_offset` (centered / start / end) | 3D: draws the scene's cards and meshes through a camera (the camera input's, else the scene's first) over the background; perspective-correct textures, surfaces composited front to back, holdouts cutting out what's behind them, tracker points as dots. Passes become planes: `depth` (distance in front of the camera, 0 where empty), `P.x/y/z` (scene position), `N.x/y/z` (normal), `uv.u/v`, `motion.u/v` (pixels, right and down, that what each pixel shows moves by the next frame: the scene and camera are also evaluated a frame later), `id` (object number, 0 empty), `matte.<node>` (each object's coverage), `diffuse.R/G/B`, `specular.*`, `emission.*`, `ambient.*` (adding up to the beauty), `light.<node>.R/G/B` (each light's share), `shadow.R/G/B`; depth/P/N/uv/motion/id from the nearest surface in each pixel, mattes adding up to alpha. Particle streams on the extra inputs render with everything else (sprites hidden behind nearer surfaces, or object instances), with motion vectors from their velocities. Motion blur averages renders across the shutter: cameras, objects (deforming vertices too) and lights in between their places at the frames round each time (those frames are evaluated too), particles moved along their velocities; the picture, mattes and light passes blur, depth/P/N/uv/motion/id stay the frame's own |
| `particle_emitter` | 0–2 (`geometry` scene, `mask` image) | `shape` (point, line, rectangle, disk, sphere, box, geometry surface, geometry vertices, mask), `dimensions`, `surface`, `translate`/`rotate`/`scale`, `rate` (per frame), `burst`, `start`, `end`, `direction`, `speed`, `speed_var`, `spread`, `along_normal`, `life`, `life_var`, `size`, `size_var`, `size_end`, `color`, `color_end`, `rotation`, `rotation_var`, `spin`, `spin_var`, `seed`, `substeps`, `preroll` | particles: gives birth to them (repeatable per seed); outputs a particle stream |
| `particle_gravity` / `particle_wind` / `particle_turbulence` / `particle_attractor` / `particle_vortex` / `particle_bounce` / `particle_kill` / `particle_speed_limit` | 1 (`particles`) | `acceleration` (gravity); `wind`, `drag`; `amount`, `scale`, `speed`, `seed` (turbulence: curl noise); `position`, `amount`, `radius` (attractor); `center`, `axis`, `amount`, `radius` (vortex); `point`, `normal`, `bounce`, `friction` (bounce: a plane); `age`, `region`, `center`, `radius`, `size` (kill); `max` (speed limit); all: `strength`, `start`, `end` | particles: a force or constraint added to the stream, in chain order |
| `particle_render` | 1–3 (`particles`, `texture` image, `objects` scene) | `look` (points, sprites, objects), `outline` (round, square), `blend` (over, additive), `scale`, `opacity`, `opacity_end`, `pick` (by age, cycle), `cycle_rate`, `orient` (none, velocity, spin) | particles: how they look where rendered: dots, camera-facing sprites (textured, faded over life) or copies of the objects input's meshes; a Scanline Render's motion blur streaks them along their velocities |
| `particles_2d` | 1–2 (`particles`, `background`) | — | draws particles straight onto the image, no camera: x, y in frame pixels (y down), size in pixels, as Particle Render says |
| `camera_tracker` | 1 | features, solve settings, solve mode (auto / free move / tripod), tripod distance, focal (known or solved) | 3D: solves a moving or a tripod (only turning) camera from the plate (**Track & Solve**, with progress and Stop; the result saves with the node), then **Export** adds a keyed Camera, a Point Cloud, a Scene and a Scanline Render over the plate; with it selected, select solved points in the viewer (click, ⇧-click, drag a box) to **Place Card here** (on the plane through 3+ points, else facing the camera), **Place locator**, or **Set origin**, **Ground plane** and **Set scale** (two points a given distance apart) |
| `point_cloud` | 0 | points (x, y, z) | 3D: points drawn as dots |
| `locator` | 0 | `translate` [3] | 3D: a point to wire into a Scene or link params to |

In the 3D view, emitters (shape and direction), attractors, vortices and bounce planes are drawn, picked and moved with G / R / S and the gizmos (forces move only); in 2D, an emitter feeding a viewed Particles 2D shows box or point handles in the viewer.

3D nodes (Camera, Card, Scene) build a 3D scene: their blue wires carry
scenes, not images, and only connect to scene inputs (the node editor,
the session/MCP and the graph all refuse a mismatch). Viewing a 3D node directly
lists what its scene holds; Scanline Render draws it.

Adding a **Camera** in the node editor brings what it needs, wired: Camera →
Scene → Scanline Render, the Camera and Scene stacked above and to the left,
one undo step, the Camera selected. The Scanline Render goes where a single
node would: into the highlighted wire (its source becomes the background),
or after the selected node (which feeds the background; what it fed now
comes from the render; a selected Card or other scene joins the Scene
instead), or on its own. Hold **Shift** when choosing Camera to add the
Camera alone. (Scripts' `create_node` always adds just the node.)

Shape params, shared by `rectangle`, `ellipse`, `polygon` and `roto`: `width`,
`height` (the frame when there's no background, 0 = the project format;
with one, its frame is used), `color` (straight), `opacity`, `feather` (soft-edge width in pixels,
centered on the outline), `invert`, `operation` (`over` draws the shape over
the background; `subtract` cuts it out of the background), `motion_blur`
(samples per frame, 0 = off), `shutter` (frames the shutter is open,
default 0.5) and `shutter_offset` (`centered`/`start`/`end`): with motion
blur on, an animated shape is drawn at each sample time across the shutter
and the coverage averaged. Edges are
anti-aliased from the signed distance to the outline, so the alpha is a
clean matte for any `mask` input.

### Live links

A param can hold a **link** instead of a value; it's worked out from
another node at every frame (and between frames, for motion blur), so it
follows that node live:

```json
{"link": {"node": "tracker1", "param": "track1"}}
{"link": {"node": "tracker1", "param": "track1", "at": "reference"}}
{"link": {"node": "tracker1", "motion": "translate", "stabilize": false, "pivot": [960.5, 540.5]}}
```

The first reads another node's param (keyframes evaluated, links
followed); the second reads it at that node's reference frame; the third
is a Tracker's solved motion as a Transform's `translate`, `rotate` or
`scale` about `pivot`. Any link can add `"multiply"` and `"add"` (a number
for every component, or one per component) for `linked × multiply + add`:
`"multiply": 0.5` on a rotate link turns half as far, `"add": [10, 0]` on
a translate link follows 10 px to the right. The panel's × and + fields
under a linked param set them. The Tracker panel's **To Transform** / **To Corner
Pin** make linked nodes (untick **Linked** for a keyed copy). Linked
params show their value and source in the panel, with **Unlink** to keep
the value as a plain one; viewer handles don't move them. Renaming the
source updates its links; deleting it makes the linked node report it.

**Formulas.** A link can hold a formula instead, reading any number of
nodes' params at the frame being rendered:

```json
{"link": {"expr": "tracker1.track1 + [wiggle(0.1, 5, 1), wiggle(0.1, 5, 2)]"}}
{"link": {"expr": "45 * sin(frame / 8)"}}
{"link": {"expr": "mix(a.color, b.color, smoothstep(1, 24, frame))"}}
```

- `node.param` is that param now (keyframes evaluated, links followed);
  `.x .y .z .w` or `.r .g .b .a` (or `[i]`) pick a component. Quote a
  node name that isn't a plain identifier: `"plate-2".center`.
- `frame`; `time`, seconds from the project's first frame
  (`(frame - first_frame) / fps`, so 0 on the first frame); `fps`, the
  project's rate; `pi`, `tau`, `e`; lists `[a, b]`; `+ - * / % ^`; comparisons
  and `&& || !` (1 or 0). Lists work per component, and a number goes
  with every component (`a.center * 0.5`). One number for a list
  param (a position, size or color) fills every component.
- `sin cos tan asin acos atan atan2` (radians: `sin(rad(30))`; rotation
  params are degrees), `sqrt abs floor ceil round fract sign pow exp log
  rad deg min max clamp mix smoothstep length dist`, `if(c, a, b)`.
- `noise(x[, seed])`: smooth noise in -1..1; `random(seed)`: a number in
  0..1 that stays the same; `wiggle(freq, amp[, seed])` = `amp ×
  noise(frame × freq, seed)`, `freq` wobbles per frame (per second:
  `amp * noise(time * freq, seed)`). All repeatable: the same frame
  always gives the same value.
- `at(x, f)`: `x` at frame `f`, e.g. `at(t.track1, frame - 3)` lags a
  track by three frames; `at(x, frame - fps)` is `x` a second earlier.

`multiply` / `add` apply after the formula. In the panel, right-click a
param's name for **Use a formula** (it starts as the current value); the
formula field applies on Enter (it suggests names as you type: ↑/↓ and
Tab, or click; the ? beside it lists every function), and a formula that doesn't parse, reads a
missing node or param, mixes lists of different sizes or reads itself
(directly or through other nodes) shows the reason in red; rendering it
is an error naming the param. Renaming a node rewrites the formulas that
name it. MCP `set_params` and Python take the same JSON.

## Scripting and automation

Every front end edits graphs through one engine API (`session.rs`), so
validation, undo/redo and error messages are the same everywhere. A running
GUI also listens on a local socket (`bridge.rs`), so scripts and AI agents can
**attach** to it and edit the open project live. Their edits land in the GUI's
undo history. Renders they request run in the background. The menu bar
shows their progress and a Cancel button, and the window stays usable.

### MCP server (AI agents)

```sh
arclight mcp [graph.json]                 # JSON-RPC 2.0 over stdio, one message per line
arclight mcp --http 127.0.0.1:7878        # JSON-RPC over HTTP POST (/mcp)
arclight mcp --attach [PID]               # drive the newest (or given) running GUI
```

Register it with an MCP client, e.g. `claude mcp add arclight -- /path/to/arclight mcp`.
Tools: `list_ops`, `describe_op`, `get_graph`, `describe_node`, `create_node`,
`delete_node`, `rename_node`, `set_params`, `set_key`, `export_tracker` (a Tracker's motion as a keyed Transform or Corner Pin), `track_camera`, `camera_points`, `orient_camera`, `export_camera`, `place_on_points` (the Camera Tracker: solve, read the points, set origin/ground/scale, export, place cards and locators), `scene_objects` (what a 3D node's scene holds: cameras, cards, meshes, and lights with their type, world position and direction, color, intensity and settings), `particles` (a particle node's particles at a frame: ids, positions, velocities, ages, lives, sizes, colors, in pages of up to 100000), `connect`, `disconnect`,
`set_project`, `render_frame` (stats, optional file and PNG preview the model can
see), `render_output`, `probe_media`, `new_graph`, `load_graph`, `save_graph`,
`undo`, `redo`, `list_plugins`, `list_instances`, `attach_instance`,
`detach_instance`. A few tools only work on an attached GUI: `select_node`
(selects a node, opens its properties and scrolls the node graph to it),
`view_node` (shows a node in the viewer, optionally at a frame) and
`get_view` (what the user is looking at: selection, viewed node, frame,
playing). Invalid calls come back as tool errors (`isError: true`) with
a message naming the bad param or node. Protocol errors use the standard
JSON-RPC codes.

### Python (`arclt`)

Install: `pip install arclt-<version>-cp310-abi3-<platform>.whl` from a
release (or `pip install ./python` to build it; needs Rust and maturin).
The wheel is self-contained; video needs ffmpeg on `PATH`. For working on
Arclight itself, `python/build.sh` builds the module in place:

```sh
python/build.sh
PYTHONPATH=python python3
```

```python
import arclt

bg   = arclt.create_node("checkerboard", width=960, height=540)
sun  = arclt.create_node("ellipse", width=960, height=540, center=[480, 270],
                         size=[260, 260], color=[1, 0.45, 0.1, 1], feather=30)
comp = arclt.create_node("merge", sun, bg)          # inputs by port: A, B
comp["operation"] = "screen"                         # params validated on set
sun.set_key("center", 1, [200, 270]).set_key("center", 48, [760, 270])
arclt.render(comp, frame=24, path="/tmp/comp.exr")  # -> size + mean/min/max color
img = arclt.to_numpy(comp, 24)                      # (h, w, 4) float32, needs numpy
arclt.save("/tmp/comp.json")

arclt.attach()        # from now on, edit the project open in the GUI
```

Also: `node`, `nodes`, `delete`, `connect`, `disconnect`, `load`, `new`,
`pixels`, `ops`, `op_info`, `project`, `undo`, `redo`, `metadata` (media file
info), `graph`, `plugins`, `instances`, `detach`, and, when attached,
`select`, `view(node, frame=None)`, `view_state`. `Node` has `info`,
`params`, `set`, `input(s)`, `set_input`, `outputs`, `rename`, `render`,
`select`, `view`, `to_transform` / `to_corner_pin` (for a Tracker),
`track_camera`, `camera_points`, `orient_camera`, `export_camera`,
`place_on_points` (for a Camera Tracker), `scene_objects(frame=None)` (for
any 3D node: its cameras, cards, meshes and lights, as the MCP tool lists
them), `particles(frame=None, offset=0, limit=None)` (a particle node's
particles as columns; every page unless `limit`). Lights are nodes like any other: `create_node("light_spot",
target, cone=30)` (a Locator, Card or Camera in its `look at` input aims
it), `light_area`, `light_point`, `light_directional`, `light_ambient`;
`light_2d` lights an image.
Errors raise `arclt.ArcltError`. Tests:
`PYTHONPATH=python python3 -m unittest discover -s python/tests`.

### Native plugins

Plugins are shared libraries with a small C ABI
([`include/arclight_plugin.h`](include/arclight_plugin.h)). A plugin declares its ops
as JSON (ports, typed params, category, whether it varies per frame) and gets
float RGBA buffers to process. Plugin ops show up in the node menus with
generated property panels, can be keyframed, are cached, and can be scripted.
`plugins/example` (Rust) adds **Vignette** and **Film Grain**.

Files must be named `libarclight_plugin_*.so` or `*.arclight.so`. Arclight
searches `$ARCLIGHT_PLUGIN_PATH`, the `plugin_paths` setting,
`~/.local/share/arclight/plugins`, `/usr/local/lib/arclight/plugins` and
`/usr/lib/arclight/plugins`. Run `arclight plugins` to see the search path and
what loaded, and `arclight ops [OP]` to see the op catalog.

### Settings and files (XDG)

| What | Where |
|------|-------|
| Settings | `$XDG_CONFIG_HOME/arclight/settings.json`: `plugin_paths`, `frame_cache_mb` (GUI frame cache, default 1536), `mcp_http_addr` |
| User plugins | `$XDG_DATA_HOME/arclight/plugins/` |
| Running-GUI registry and sockets | `$XDG_RUNTIME_DIR/arclight/` |

## I/O formats

- **EXR**: full 32-bit float, assumed linear + premultiplied. The display window is the frame and the data window where the pixels are (smaller, offset or past the frame); writing stores only the data window.
- **DPX** (`.dpx`): read at 8, 10, 12 or 16 bits, or 32/64-bit float (RGB, RGBA, ABGR or
  luma; 10/12-bit filled or packed; either byte order; any orientation).
  Log (printing density) DPX is converted to linear with the Cineon curve
  (black 95, white 685, gamma 0.6), linear DPX is taken as is, and
  video-coded DPX is decoded like the 8/16-bit formats; float DPX is
  linear unless marked as video. Written per the **DPX encoding** setting
  on Media Out and Write: `10-bit log` (default; keeps values above 1.0),
  `10-bit video` or `16-bit video` (sRGB-coded, full range, marked
  Rec. 709; values above 1.0 clip), or `32-bit float linear` (exact). As
  RGB over black, or with **DPX alpha** on, RGBA with straight alpha.
- **TIFF** (`.tif`/`.tiff`): read at 8, 16 or 32-bit (any common
  compression); written as 16-bit sRGB with straight alpha (tagged as
  alpha), LZW-compressed.
- **Targa** (`.tga`), **WebP** (`.webp`), **PNG**, **JPEG**, **BMP**: read
  from sRGB at the file's own depth (8- or 16-bit); written as 8-bit sRGB
  with straight alpha (WebP lossless, Targa RLE). JPEG has no alpha, so it
  is written over black. Float files such as Radiance `.hdr` are read as
  linear.
- **Sequences**: any of the above as numbered files. Pick (or drop) one
  file of `shot.0001.exr` (or `.dpx`, `.tif`, …), `shot.0002.exr`, … and Media In finds the whole
  sequence (`shot.####.exr`) and its frame range; a single numbered file is
  a one-frame sequence. A frame missing from the middle holds the nearest
  earlier frame. Media Out and Write take `####` / `%04d` patterns.
- **Video** (via ffmpeg): `.mp4 .mov .m4v .mkv .avi .webm .mxf .mpg .mpeg`
  in, in either case (a camera's `CLIP.MP4`; the file browser lists both),
  through Media In or Read; the first video stream is read, other streams
  (audio, a camera's data tracks) are ignored. Playback that falls behind
  and skips a few frames reads on to the next one wanted rather than seeking
  (a seek in a long-GOP 4K clip costs about half a second). Out as H.264 (8-bit), HEVC (10-bit), ProRes 422 HQ (10-bit) or
  ProRes 4444 with alpha (Media Out's Codec), converted with the BT.709
  matrix and tagged BT.709 primaries/matrix, sRGB transfer, video range. Sources deeper than 8 bits
  (10-bit HEVC, 10/12-bit ProRes, DNxHR HQX, …) are decoded at 16 bits, so
  their full precision reaches the comp; 8-bit sources stay on the lighter
  8-bit path. The Media In panel shows the clip's bit depth.
- **Resolution**: no fixed limit on reads. Every image is held as 32-bit
  float RGBA (16 bytes a pixel: about 130 MB for a 4K UHD frame, 530 MB
  for 8K UHD), so memory sets the practical ceiling; the frame cache
  (`frame_cache_mb`, default 1536) holds fewer large frames. Stills decode
  up to 4 GB each. Sizes typed into nodes (Constant, shapes, Resize,
  Crop, project format) go up to 16384. The viewer shows images larger
  than the GPU's texture limit (8192 here) at reduced resolution; the
  pixel readout still reads full-resolution values.

## Layout

```
src/
  lib.rs           engine library
  main.rs          CLI (clap) + demo + tests
  anim.rs          keyframe curves: evaluation, interpolation, key editing
  graph.rs         graph model, topo sort, cached frame-aware Evaluator
  ops.rs           operator registry (inputs, params, defaults) + implementations
  media.rs         image sequences, ffmpeg video decode/encode
  render.rs        frame-range rendering for media_out
  track.rs         2D tracking (NCC matching, tracking runs) and similarity solves
  shape.rs         polygon coverage (anti-aliasing, feather, fill rules), shape outlines, bezier flattening/splitting
  image.rs         Image raster type, bilinear sampling, file I/O
  color.rs         Rgba pixel, sRGB and premultiply conversions
  session.rs       headless editing API: validated edits, undo/redo, rendering
  mcp.rs           MCP server (JSON-RPC 2.0 over stdio / HTTP) and its tools
  bridge.rs        running-GUI registry + Unix-socket bridge for live scripting
  plugin.rs        native plugin host (C ABI, dynamic op registration)
  config.rs        XDG paths and settings.json
  gui/
    main.rs        app shell: layout, menus, shortcuts, file drop
    doc.rs         document model, graph edits, undo/redo history
    node_editor.rs node graph canvas
    curve_editor.rs keyframe curve editor
    viewer.rs      image viewer
    overlay.rs     viewer handles for the selected node (gizmos, boxes, vertices)
    transport.rs   player controls + timeline
    properties.rs  per-node property panels, render jobs
    worker.rs      background evaluation thread + frame cache
include/arclight_plugin.h  plugin C ABI
plugins/example/  example native plugin (Vignette, Film Grain)
python/           `arclt` Python module: Rust core (src/lib.rs) + Python API (arclt/)
examples/
  basic.json        a small sample graph
  media/testsrc.mp4 5 s test clip used by the GUI starter project
```

## Roadmap ideas

- Bounding-box / region-of-interest propagation so ops only touch covered pixels
- Tiled, cached evaluation for large plates
- More ops: color-space nodes, keyer, defocus, time/sequence I/O
- Multi-select of nodes; a normalized view and key copy/paste in the curve editor
- GPU display transform / OCIO in the viewer

## License

MIT OR Apache-2.0.
