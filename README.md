# ArclightVFX Compositor

**Beta.** An original **node-graph image compositor** written in Rust.
ArclightVFX (formerly Arclight) reads a
JSON description of an image-processing DAG, evaluates it, and writes the
result. It ships as a command-line renderer (`arclight`) and an interactive desktop
app (`arclight-gui`; the commands, the `arclt` Python module and the
settings folders keep the name `arclight`) with a node editor, viewer, player and properties panels.

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

Version **0.11.3** (beta); see `CHANGELOG.md`.

```sh
cargo build --release      # -> target/release/arclight, target/release/arclight-gui
cargo test                 # unit + graph tests
cargo build --release --no-default-features   # CLI only, no GUI dependencies
cargo build --release -p arclight-plugin-example  # example native plugin
python/build.sh                                   # Python `arclt` module (needs Python ≥ 3.10 headers)
```

Video input/output needs `ffmpeg` and `ffprobe` on `PATH` (e.g. `brew install ffmpeg`).
On Linux the GUI's sound needs ALSA's development files to build (`alsa-lib`
on Arch, `libasound2-dev` on Debian / Ubuntu); at run time it uses
`libasound.so.2`, present on any desktop with PipeWire or PulseAudio.
Image sequences and stills need nothing extra.

### App launcher (Linux)

```sh
./install.sh            # add ArclightVFX to the app launcher (builds first if needed)
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
packaging/linux/build-tarball.sh   # -> dist/arclightvfx-<version>-linux-x86_64.tar.gz
```

The tarball holds `bin/arclight-gui`, `bin/arclight`, the example clip and
icons. Unpack it and run `./install.sh` to put the binaries in
`~/.local/bin` and ArclightVFX in the app launcher (`PREFIX=… ./install.sh`
for elsewhere; `./uninstall.sh` removes it), or run `bin/arclight-gui` in
place. The binaries link only glibc (and, for the GUI, `libasound.so.2`), so they run on the distribution they
were built on (Arch / Omarchy for the published releases) and newer ones.
Releases are on GitHub (`gh release list`), with the macOS DMG (universal:
Apple Silicon and Intel) and the Python wheels beside the tarball.

### macOS (Apple Silicon and Intel)

```sh
brew install librsvg ffmpeg       # icon drawing for the build; video I/O at run time
packaging/macos/build-dmg.sh      # -> dist/ArclightVFX.app, dist/ArclightVFX-<version>-universal.dmg
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
com.apple.quarantine /Applications/ArclightVFX.app`). ffmpeg isn't bundled:
ArclightVFX finds Homebrew's (`/opt/homebrew/bin`) or MacPorts' even when
opened from the Finder (on Intel Macs Homebrew lives in `/usr/local/bin`,
which is searched too). The CLI is inside the bundle at
`ArclightVFX.app/Contents/MacOS/arclight`. Upgrading from a release named
Arclight, delete the old `Arclight.app` from Applications once. Plugins on macOS are
`libarclight_plugin_*.dylib`.

## GUI

```sh
cargo run --release --bin arclight-gui                 # starter project
cargo run --release --bin arclight-gui -- comp.json    # open a graph
```

| Area | What it does |
|------|--------------|
| **Viewer** (center) | Shows the input of the active `viewer` node: a project can have several, and the viewer shows the one last wired into, double-clicked (or **V**) or clicked, marked with an eye in the graph (the others read "Viewer (not shown)"); with one, it's that one. Double-clicking another node (or **V**) shows that node instead, without rewiring the Viewer (an eye marks it, and the status line says so) until the Viewer node is clicked or double-clicked. A node with an input left unplugged treats it as empty, so the comp below it keeps showing; only output nodes (Viewer, Write, Media Out) report an unconnected input. Drag (or middle-drag) to pan, scroll/pinch to zoom, double-click or **F** to fit. RGB/R/G/B/A channel isolation (images with extra planes, e.g. an EXR with a Z channel, read as `depth`, get a button per plane beside them, showing it as gray from its lowest to its highest value, 0 staying black; the readout adds the plane's value), exposure in stops, float pixel readout under the cursor, and a color picker at the right end of the viewer's toolbar: a swatch of the hovered pixel as displayed with its R, G, B and A values (linear, premultiplied); it keeps reading that pixel after the pointer leaves, through playback, and its tooltip gives the position, 8-bit values and hex. When an image stores pixels for only part of the frame, or past it (its *data window*: a small shape, a layer moved off-frame), the window is outlined dashed, its position and size show next to the frame size, and pixels past the frame show dimmed. The selected node's handles draw over the image: **Transform** outlines the transformed layer and shows a gizmo at its center (the input layer's middle unless Center is set; the panel's **Auto** puts it back) (drag anywhere on the layer or its center to move, ⇧ for one axis; the frame's corners scale uniformly, ⇧ for free, and its edges scale one axis; the ring to rotate, ⇧ snaps to 15°; the red/green handles to scale x/y, ⇧ for uniform), **Crop** shows its box (drag edges or corners), **Rectangle**/**Ellipse** show a box (drag inside to move, corners/edges to size about the center, ⇧ keeps the aspect; the stem above the top edge rotates), **Polygon** shows its vertices (drag inside the shape to move all of it, which also selects every vertex; drag a vertex to move it, or an edge to move its two ends; ⇧-click to select several and drag them together, drag on empty space to box-select (⇧ adds, also from inside the shape; middle-drag still pans), ⌘/Ctrl+A selects all; with two or more selected, a dashed transform box surrounds them: drag inside to move, corners to scale about its center (⇧ free), edges to stretch one axis, the stem above to rotate (⇧ snaps to 15°), and Roto tangents turn and scale along, click an edge without dragging to insert a vertex, ⌘/Ctrl-click to add one after the selected vertex, **⌫** deletes selected vertices), **Roto** works the same on its curve and also shows the selected vertices' tangent handles (drag one to reshape the curve smoothly, ⌥/Alt-drag to break the tangent; Alt-drag a vertex to pull out new tangents; ⌘/Ctrl-click-drag adds a vertex and pulls out its tangents; clicking the curve splits it without changing its shape, dragging it moves the two vertices either side). **Tracker** shows each enabled track's pattern box (solid), search box (dashed) and tracked path; drag inside a pattern box to place or correct a track (keys the current frame), drag a box corner to size that track's box (⇧ sizes it for every track, back to the panel's shared Pattern/Search size). ⌘/Ctrl-drag a box to move it off its point (an offset: follow a textured feature, report a point beside it, e.g. one that gets covered); **Clear offsets** puts them back. The panel's **Per-track boxes** table shows and edits each track's own pattern and search sizes (shared ones marked ·) and offset; ↺ returns a track to the shared sizes. Frames where a track matched weakly show as orange dots on its path and orange bands on the timeline; the panel names the weakest match (click it to go there), and **Clear ◀ / ▶** removes the enabled tracks' keys before or after the current frame. While the viewer shows the Tracker's own match-moved or stabilized output, the boxes are drawn (and dragged) where that output shows them. **Keyer**: ⌘/Ctrl+click picks the screen color from the Keyer's input (whatever the viewer shows), ⌘/Ctrl+drag averages a box. **Corner Pin** shows its To quad (solid, numbered corners) and From quad (dashed); drag either's corners. **Light 2D** shows its position (drag to move), aim knob (⇧ snaps to 15°), cone edges and softness dots (a spot), radius (a point) or gradient line (a directional light), or an area light's box. Handle drags key animated params at the current frame; inserting or deleting vertices applies to every key. **Sheet** (beside 2D / 3D) opens **Geo Info** under the view: the viewed node's 3D scene as a table, as Blender's spreadsheet: tabs for point sets' points, mesh points, faces, instances and curve points (with their counts), a row per element and a column per value (P, N, uv, rotation / scale for instances, then each attribute, colors with a swatch); pick one object, or Selected only for the selected nodes'. Values are in each object's own space. A million rows scroll as smoothly as ten. |
| **Player** (under the viewer) | Go to start, step back, play backward, pause, play forward, step forward, go to end. Scrubbable timeline (green strip = frames already cached), in/out, FPS, loop, and the project **Format** (frame size for sources whose own size is 0). Plays in real time and shows the achieved fps. |
| **Curve editor** (bottom, second tab) | The selected node's keyframed params as value-over-time curves, one per channel (x/y, r/g/b/a), with a toggle per channel. Click / Shift-click / box-drag to select keys, drag to move them in time and value, double-click a curve to add a key, **⌫** to delete. Smooth/Linear/Step buttons set interpolation. Selected keys on smooth curves show tangent handles: drag one to set the slope (in and out move together), **⌥**-drag to break the key so each side moves on its own; hollow handles are automatic, filled are user-set. Tangents Auto/Flat/Break-Unify buttons act on the selected keys. Scroll to pan, ⌘-scroll or pinch to zoom (⌥ value only, ⇧ time only), middle-drag to pan, **F** to frame. The ruler scrubs the playhead. |
| **Node graph** (bottom) | Right-click the canvas, or press **⇧E**, or **⇧Tab** with the pointer over the graph, to open the searchable node browser at the pointer (type to filter, ↑/↓ and Enter or click to place, Esc or a click elsewhere to close) and add nodes. With a node selected, the new node is wired after it; with a Scanline Render selected, a new scene or particle node (a Particle Emitter, a Card, …) goes into its spare input. **Wires:** click one to select it (it lights up; **⌫** disconnects it); right-click one for **Insert 3D Camera Setup**, **Insert Node…** (the browser, inserting into that wire) or **Delete connection**. **3D Camera Setup** (the wire menu, **Edit ▸ Add 3D Camera Setup**, or Camera in the browser; ⇧ for the Camera alone) adds a Camera, a Scene and a Scanline Render, wired up, in a "3D Setup" backdrop just before the Viewer (one is added if there's none), its Camera backed off along +Z so a new Card fills the frame, so the render shows straight away, the Viewer's picture becoming its background; from the wire menu (or the browser aimed at a wire) it goes into that wire instead, the wire's picture becoming the background. The first in a project switches the viewer to 3D, framed on the scene, with a hint that Tab switches back; the viewer's 3D button shows a dot whenever the viewed node has a scene. Drag an unconnected node onto a wire (it lights up) to splice it in; shake a node while dragging it to pull it out of its wires (its input then feeds what it fed). The minimap in the lower right shows the whole graph and the view: click or drag in it to move there, scroll over it to zoom into that spot, double-click to fit. Inputs are yellow squares (hollow when optional), outputs green circles; a masked node's `mask` input is violet and labelled (hover it for what it does). **Fit** (top-right corner of the Node Graph) or **F** with the pointer over it frames every node and backdrop. Drag from an output to an input (or back) to connect, or drop the wire anywhere on a node (it lights up and the wire's end shows the input it will take): from an output it goes into the node's first free input that takes it (else its first, replacing that wire; never the mask, which takes a wire only on its own port), from an input it takes the node's output; drag a connected input away to reroute or disconnect. Drag from an output onto a wire (it lights up) to join the two with a new Merge: the dragged node goes into A, over the wire's source in B (e.g. drop a Keyer onto the background's wire to put the keyed foreground over it). **The switch** at a node's top-right (or **D** for the selected nodes, or Enabled in Properties) turns it off: bypassed, passing its input on (a Merge its background, a Scanline Render its background), or outputting nothing if it has none; it's dimmed and struck through. Double-click a node or press **V** to view it (click the Viewer node to go back to its output). **⌫** deletes the selected nodes (each chain is reconnected). Drop image/video files onto the window to create Media In nodes. **Selecting:** click a node; **⇧**- or **⌘**-click adds or removes one; drag on empty space for a selection box (it takes every node it touches; **⇧** adds); **⌘A** (pointer over the graph) selects all, **Esc** none. Drag any selected node to move them all (one undo step). The Properties panel shows the last node picked. **Panning:** middle-drag or **⌥**-drag on empty space (a plain drag now selects), scroll to pan, pinch or ⌘-scroll to zoom. **Backdrops:** **⌘G** (or right-click a node → Group) puts a box behind the selected nodes, fitted to them; drag its title bar to move it together with the nodes (and backdrops) lying fully inside it when the drag starts; drag a corner to resize; double-click the title to rename it; the swatch at the right of the title picks its color. Select one by its title to edit Title, Color and Title size in Properties. **⇧⌘G**, **⌫**, its right-click menu or **Delete box** removes the box and leaves the nodes. |
| **Properties** (right) | One panel per opened node, newest first, generated from each op's parameter specs. Media In: browse, auto-detected frame range, clip info. Media Out: browse, render the frame range with progress and cancel. |

Shortcuts: **Space** play/pause · **J/K/L** back/stop/forward · **←/→** step ·
**⌥←/⌥→** previous/next keyframe · **Home/End** · **⌘Z/⇧⌘Z** undo/redo ·
**⌘O/⌘S/⇧⌘S/⌘N** open/save/save as/new · **⇧E** node browser · **⇧Tab** (over the graph) node browser · **⇧R/⇧T/⇧M** (over the graph) add Media In/Transform/Merge · **D** (over the graph) turn the selected nodes off / on ·
**⌘A** select all (over the graph; over the viewer, every point of the
selected shape) · **Esc** select none · **⌘G / ⇧⌘G** group / ungroup.

### 3D view (Blender-style)

The viewer's **2D | 3D** toggle (or **Tab**, anywhere in the window but
a text field) shows the 3D scene of the node being viewed (a Camera, Card,
Scene, Locator, Point Cloud, Camera Tracker, or a Scanline Render's scene
input); a node without one says so. The viewport camera is saved with the
project.

- **Navigate:** middle-drag orbits, **⇧** + middle-drag pans, scroll or
  **Ctrl** + middle-drag zooms toward the pointer. Trackpad: two-finger
  scroll orbits, **⇧** pans, pinch or **Ctrl** zooms. Without a middle
  button, turn on *Emulate 3-button mouse* (Alt + left-drag). With
  something selected, orbiting turns around it, so it stays in place on
  screen (*Orbit around the selection* in Preferences, on by default).
  Fine scroll steps from an unknown source (Linux) count as the wheel for
  a second after it steps, so a smooth-scrolling wheel zooms, not orbits.
- **Views:** numpad **1 / 3 / 7** front, right, top (**Ctrl**: the
  opposite sides), **5** perspective / orthographic, **0** (or **View ▸
  Look through selected**, or **Look through in 3D view** in a Camera's,
  spot or area light's Properties) through the selected camera, spot light or area
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
Rotate: by how far the pointer turns about the object's center, damped
within 60 points of it so a pointer near the object doesn't swing it; Scale: by
how much farther from its center the pointer is). Then **X / Y / Z**
constrains to that world axis (press it again for the object's own axis,
a third time to free it), **⇧ X / Y / Z** excludes that axis, typing a
number sets the amount exactly (`G X 2 Enter`, `R Z 90 Enter`,
`S 0.5 Enter`; **-** negates, Backspace edits), **Ctrl** snaps (1 unit,
15°, 0.1); **Enter** or a click confirms, **Esc** or a right-click
cancels and puts everything back. **Alt + G / R / S** resets translate /
rotate / scale. Curves: pick points (Shift for more), drag or G/R/S
them, drag Bezier handles (Alt breaks), Ctrl/Cmd-click adds or inserts,
Delete removes. The viewer toolbar's **Select / Move / Rotate / Scale /
All** pick the gizmo on the selected object (**Move**, the default;
**G / R / S** also pick Move / Rotate / Scale as they start, so the gizmo
stays up for tweaks; **All** shows arrows, rings and cubes together): drag an arrow to move along
an axis, a square to move in that plane, the center circle to move in the
view plane; a colored ring to rotate about that axis (by how far the
pointer moves along the ring, so it turns smoothly even seen edge-on), the white outer
ring about the view, inside it like a trackball; a cube to scale along
that axis, the center to scale evenly. **Global / Local** sets the
gizmo's axes. Every move, rotate or scale sets a key when the param is
animated (at the current frame) and is one undo step; a linked param
(e.g. a Card linked to tracker points) shows the gizmo gray and says what
it's linked to: Unlink it in Properties to move it by hand. A selected spot
light shows handles on its cone's edge (drag to change the cone angle) and
an area light on its edges' middles (drag to resize it about its middle);
these key and undo the same way.

**Help → ArclightVFX Help…** (or **F1**) opens a scrollable help window with
these controls and more (the text is `docs/help.md`, built into the app;
edit it to change the help); **Help → About ArclightVFX Compositor…** shows
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

## GPU acceleration

ArclightVFX renders on the GPU where it can (wgpu: Vulkan on Linux, Metal
on macOS) and on the CPU everywhere else. The CPU renderer stays the
reference: GPU results are tested against it within stated tolerances
(`docs/gpu-plan.md`). The device is created the first time a render needs
it, never at start-up. Ops move to the GPU milestone by milestone (phase
12); until an op has a GPU path, it renders on the CPU as before.

On the GPU now: **Blur**, **Keyer**, **Lens Distort** and motion-blurred
**Transform / Corner Pin / Tracker** (always); and **Grade, Exposure,
Saturation, Invert, Clamp, Premult, Unpremult, Channel Select, Switch,
Dissolve, Keymix, Merge** (zmerge too), **Shuffle**, **Resize** and
unblurred **Transform, Corner Pin, Tracker** when an input is already on
the GPU: alone those run on the CPU about as fast as moving the image
would take. Masks and mix included; a Keyer with no Screen set estimates
it on the CPU. Results stay on the GPU from one GPU op to the next and
come back only when something on the CPU needs them (`arclight render`
reads back only what its writes need); past `gpu.cache_mb` (settings.json,
default 2048) of GPU memory the least recently used come back to the CPU.
Constant and Checkerboard, shapes, Light 2D, Read's fit
and everything else still run on the CPU. The CPU Blur costs the same
whatever its radius (a running sum).

In the GUI the window draws with the same GPU device, so the viewer shows
a result made on the GPU straight from there: its 8-bit display (exposure,
channel, overscan) is made by the GPU, not on the UI thread (4K: ~20 ms
less per new frame; scrubbing keeps up with the display). Results are still
read back on the viewer's worker thread, so the pixel readout and pickers
read the exact values. Played frames made on the GPU keep their color
there too (`gpu.frame_cache_mb`, default 1024, apart from `gpu.cache_mb`;
past it the least recently played go), so cached playback draws straight
from the GPU: 24 fps held at 1080p and 4K where the CPU's way managed
~20 and ~10. The planes view (depth and the like), frames past that
budget, and GPU acceleration off take the CPU's way, as before.

**Scanline Render** and the 3D view's shaded preview run on the GPU too (a
compute rasterizer that follows the CPU renderer sample for sample):
shadows, motion blur, Gaussian splats (prepared and blended there, so the
3D view orbits a capture interactively) and big particle sprites
included. Shadows of see-through casters (a card less than opaque, a
texture with transparency) render there too, up to 4 see-through layers a
shadow-map texel (fewer on GPUs with less memory): pixels behind more,
and frames that still won't fit the memory budget, are rendered on the
CPU. Small particle sprites are drawn on the CPU on purpose (quicker
there). Results match the CPU renderer within the tolerances in
`docs/gpu-plan.md` (D8).

```sh
arclight gpu                 # in use or not, and why; the adapter, backend, limits, transfer path
arclight gpu --json          # the same as JSON
arclight gpu --check         # self-test: every GPU path against the CPU, errors and timings
arclight gpu --check --json  # for pasting into an issue; exit status 1 if a case fails
arclight gpu --check --quick # the 2D cases at one small size: seconds
arclight render graph.json --cpu   # this render on the CPU only (exact, reproducible)
```

- **Turning it off:** Edit ▸ Preferences ▸ Performance ▸ GPU acceleration
  (`"gpu": {"enabled": false}` in settings.json; the CLI, MCP and Python
  follow it too), or `ARCLIGHT_GPU=0` in the environment, which wins over
  the setting. `--cpu` (CLI), `"cpu": true` (MCP `render_frame` /
  `render_output`) and `cpu=True` (Python `render`) do it for one render.
- **Linux:** needs a Vulkan driver (NVIDIA's, or Mesa's `vulkan-radeon` /
  `vulkan-intel`). Without one there is no adapter and everything renders
  on the CPU; `arclight gpu` says so. A software Vulkan adapter (lavapipe)
  isn't used for rendering.
- **macOS:** Metal, on Apple Silicon and Intel Macs (macOS 11 and later).
  On integrated GPUs (Apple Silicon) images move through shared memory
  ("unified memory" in `arclight gpu`); on discrete ones by copies.
- **Errors:** a GPU error on a node makes that node render on the CPU; a
  lost device turns the GPU off for the rest of the session. Results never
  fail because of the GPU. Crash reports name the adapter and whether GPU
  acceleration was on.
- `WGPU_BACKEND` and `WGPU_POWER_PREF` pick another backend or adapter.

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
| `media_out` | 1 | `path`, `first_frame`, `last_frame`, `fps`, `dpx` (DPX encoding), `audio` (`auto`/`none`/a node), `codec` (`auto`/`H.264 8-bit`/`HEVC 10-bit`/`ProRes 422 HQ 10-bit`/`ProRes 4444 + alpha`/`DNxHR HQX 10-bit`/`DNxHR 444 10-bit`/`VP9 10-bit`/`VP9 + alpha`/`AV1 10-bit`) | renders a range to a sequence or a video (.mp4/.mov/.mkv/.webm/.mxf/.m4v/.avi); `auto` = ProRes 422 HQ for `.mov`, VP9 for `.webm`, DNxHR HQX for `.mxf`, H.264 otherwise; each codec only in containers that hold it (ProRes .mov/.mkv; DNxHR .mov/.mxf/.mkv; VP9 .webm/.mkv/.mp4, with alpha .webm/.mkv; AV1 .mp4/.mkv/.webm; HEVC .mp4/.mov/.mkv), else an error; the 10-bit codecs are fed 16-bit frames; ProRes 4444 and VP9 + alpha keep alpha; DNxHR needs at least 256×120; the panel's Format menu sets the file type (renaming the file) and Codec offers only codecs the container holds; a video gets the sound of the clip upstream (`auto`) or of a named node, for exactly the rendered frames and in sync (AAC in .mp4/.m4v, FLAC in .mkv, 24-bit PCM in .mov/.mxf/.avi, Opus in .webm) |
| `viewer` | 1 | — | pass-through; marks what the GUI viewer shows |
| `camera` | 0 | `focal` (mm), `film_back` [2] (mm), `projection` (perspective / orthographic), `ortho_width` (scene units across the frame), `translate`/`rotate`/`scale` [3], `near`, `far` | 3D: outputs a scene holding a camera (right-handed, Y up, looking down −Z; rotate in degrees, X then Y then Z) |
| `card` | 1 | `translate`/`rotate`/`scale` [3], `size` [2] or null, `opacity` (0–1, animatable), `shading` (unlit (default) / lambert / blinn), `specular`, `roughness`, `emission`, `casts_shadows`, `receives_shadows` | 3D: puts its input image on a flat card (1 wide by default, keeping the image's aspect), faded by Opacity (its shadow too, as strong as the card is opaque); unlit by default (the image as it is), or with Shading lambert / blinn lit by the scene's lights from either side like a real card, taking shadows unless Receives shadows is off; outputs a scene |
| `cube` / `sphere` / `cylinder` / `plane` | 0–1 (`texture`) | `translate`/`rotate`/`scale` [3], size (or `radius` / `height`), `segments`, `caps` (cylinder), `color`, `shading` (unlit / lambert / blinn), `specular`, `roughness`, `emission`, `holdout`, `casts_shadows`, `receives_shadows` | 3D: a primitive mesh, colored or textured by its optional input, lit by the scene's lights (none: unlit); outputs a scene |
| `read_geo` | 0 | `file`, `frame_rate`, `frame_offset` (Alembic: the time read is (frame + offset) / rate), `translate`/`rotate`/`scale` [3], `opacity` (0–1, animatable: fades every material, shadows too), `override_material` with `shading`, `specular`, `roughness`, `emission`; `holdout`, `casts_shadows`, `receives_shadows` | 3D: imports a mesh: `.obj` (with its `.mtl` materials), glTF `.gltf` / `.glb` (node transforms applied, PBR materials mapped) or an Alembic `.abc` cache (Ogawa; animated transforms and deforming polygon/subdivision meshes, read at each frame); outputs a scene |
| `read_splat` | 0 | `file`, `up` (-y default, y, z), `translate`/`rotate`/`scale` [3], `opacity`, `sh_degree` (0–3), `holdout` | 3D: reads a Gaussian splat capture (`.ply`: INRIA 3DGS layout, binary or ASCII, spherical harmonics up to degree 3; `.splat`: the antimatter15 web format; `.spz`: Niantic's compressed format, versions 2 and 3) into the scene; samples are `examples/geo/splats.ply` (synthetic) and `examples/geo/hornedlizard.spz` (a real capture from Niantic's SPZ repository, MIT; `examples/hornedlizard.json` sets it up). Scanline Render draws splats mixed by depth with cards and meshes (holdouts cut them, and a Holdout Read Splat cuts what's behind it; self-lit, so emission only; depth / P where half opaque, N, motion, id and a matte per Read Splat; motion blur); the 3D view shows their centers as dots while moving and renders them in full once it settles |
| `splat_crop` | 1 (scene) | `shape` (box, sphere), `center`, `size`, `rotate`, `invert` | 3D: keeps the scene's splats whose centers are inside the region (outside with Invert), to cut a capture down or clear floaters; everything else passes through; its region shows in the 3D view, moved / turned / sized with G / R / S or the gizmo |
| `splat_color` | 1 (scene) | `exposure` (stops), `saturation`, `gain` (color), `opacity`, `view_independent` | 3D: grades the scene's splats (exact from every viewing angle) and their opacity; View independent drops the color's change with the angle |
| `transform_geo` | 1 (scene) | `translate`, `rotate`, `scale`, `pivot`, `group`, `cameras_lights` | 3D: moves, turns and scales what's in the scene about Pivot (scene space): meshes, cards, point sets, curves, instances, splat captures and regions, each placed anew (nothing rebuilt); Group narrows it to one node's (its name, or a pattern with `*`: `rock*`); cameras and lights stay unless Cameras and lights is on |
| `scatter` | 1 (scene), 2 density image (optional), 3 camera (optional) | `source` (surface, volume, tracker points), `group`, `density`, `density_attr`, `image_via` (uv, camera), `channel` (alpha, luminance), `min_distance`, `seed` | 3D: adds a point set scattered over the scene's meshes and cards (evenly by area, Density a unit of area), inside its closed meshes (a unit of volume) or at its tracker points (Density the share kept); a mesh attribute and a mask image (at the uvs or through a camera) thin it; points keep their place and id when the density or mask changes; attributes id, N, uv and the surface's own |
| `delete_geo` | 1 (scene) | `elements` (points, faces, instances), `group`, `mode` (inside, outside, expression), `shape` (box, sphere), `center`, `size`, `rotate`, `expression` | 3D: deletes points (of point sets and curves), faces (by their middles; unused vertices go too) or instances inside or outside a box or sphere (shown and moved in the 3D view), or where Expression is true: a formula per element reading @P, @N, @id, @index, @count and any attribute as @name |
| `points_from_geo` | 1 (scene) | `group`, `from` (vertices / face centers), `merge_shared`, `keep_meshes` | 3D: the meshes as point sets: a point at each vertex (one where several meet) or face center, with N, uv, id and their attributes; the meshes go unless Keep meshes |
| `grid_points` | 0–1 (scene) | `size` [2], `count` [2], `plane` (xz / xy / yz), `center`, `rotate`, `jitter`, `seed` | 3D: a grid of points (edges included) with ids by cell, N and uv, jittered within cells; added to the scene given |
| `line_points` | 0–1 (scene) | `start`, `end`, `mode` (count / spacing), `count`, `spacing` | 3D: points along a line with id, uv and N / tangent along it (copies' +Z along the line); added to the scene given |
| `fit_plane` | 1–2 (`scene`, `texture` image optional) | `points` (point numbers, "0 3 5"; empty = all), `align` (world / spread), `size_mode` (fit / fixed), `margin`, `size`, `offset`, `segments`, `keep_input`, and Plane's `color`, `shading`, `specular`, `roughness`, `emission`, `holdout`, `casts_shadows`, `receives_shadows` | 3D: a plane fitted (least squares) to the scene's tracker points (those numbered, or all; else its point sets' points, else its meshes' vertices), facing its camera (else up), sized to them plus Margin; a ground plane, wall, shadow catcher or holdout; outputs the plane alone (Keep input: the scene too) |
| `attribute_set` | 1 (scene) | `elements` (points, mesh points, faces, instances), `group`, `attr1_name`/`attr1_kind`/`attr1_expr` ... `attr4_*` | 3D: sets attributes per element from formulas (@P, @N, @id, @index, @count, @name; other nodes' params; random, noise3), rows in order; P moves the elements, a mesh's N sets its normals |
| `attribute_noise` | 1 (scene) | `elements` (points / mesh points / instances), `group`, `name` (default pscale), `kind` (float / vec3 / vec4), `mode` (set / add / multiply), `center`, `amplitude`, `frequency`, `offset` [3] (animate to drift), `octaves`, `seed`, `falloff` (none / sphere / box) with `region_center`, `region_size`, `region_rotate`, `softness` | 3D: writes Center + Amplitude x noise (the formulas' noise3) into an attribute of each element, set, added or multiplied in, per component for vectors; limited to a region with a soft edge if Falloff is on (shown in the 3D view) |
| `falloff` | 1–2 (scene, `from` scene optional) | `elements`, `group`, `name` (default weight), `inner`, `outer`, `shape` (smooth / linear / ease in / ease out), `invert`, `combine` (set / multiply / max / min), `region` (sphere / box), `region_center`, `region_size`, `region_rotate` | 3D: a 0–1 weight attribute by distance to the from input's curves, else its points (a Locator, tracker points, point sets), else the node's own region (shown in the 3D view): 1 within Inner, 0 past Outer; MoGraph's effector, read in formulas as @weight |
| `attribute_from_image` | 2–3 (scene, `image`, `camera` scene optional) | `elements`, `group`, `name` (default Cd), `via` (uv / camera), `take` (rgba / rgb / luminance / alpha), `outside` (keep / fill), `fill` | 3D: samples the image at each element, blending pixels, into an attribute: where the camera input's camera sees it (project a plate or painted map onto points), or at its uv (a mesh's, or a Scatter's); off the image, behind the camera or without uvs: kept or Fill |
| `attribute_transfer` | 2 (scene, `source` scene) | `elements`, `group`, `source_elements`, `source_group`, `attributes` (names, or *), `mode` (nearest / average), `radius` (0: no limit), `falloff` (none / linear / smooth), `missing` (keep / zero) | 3D: copies attributes from the source's nearest points, mesh points or instances (or the mean of those within Radius, by nearness; ids from the nearest), faded across the radius; elements with nothing within it keep theirs or get 0 |
| `instance_on_points` | 1 (scene with points), 2 prototypes (scene), 3 camera (optional) | `group`, `pick` (random, variant, in order), `seed`, `orient` (auto, face camera, none), `up`, `scale`, `keep_points` | 3D: a copy of one of the prototypes' meshes, cards or splat captures at each point (kept as instances), picked, turned (orient; N and up; or toward the camera), sized (pscale x scale x Scale) and tinted (Cd; an unlit card only fades by Cd's alpha) by the point's attributes, which come along |
| `realize_instances` | 1 (scene) | `group` | 3D: makes each instance an ordinary mesh, card or splat capture (meshes still shared), placed and tinted as drawn |
| `curve` | none | `source` (drawn, node path), `kind` (polyline, smooth, bezier), `closed`, `detail`, `points`, `path_of`, `start`, `end`, `step`, `translate`, `rotate`, `scale` | 3D: a curve through its control points (edited in the 3D view) or along a node's path over frames (a Camera Tracker's camera, else its translate); points get tangent, u, cp, N (along it) and up (a rotation-minimizing frame) |
| `resample` | 1 (scene) | `group`, `by` (count, length), `count`, `length` | 3D: evenly spaced points along each curve; attributes blended, the frame worked out again |
| `curve_to_mesh` | 1 (scene), 2 profile (optional scene) | `group`, `profile` (circle, ribbon, curve), `radius`, `sides`, `width`, `scale`, `caps`, `keep_curves`, color and shading as primitives | 3D: a profile swept along each curve, square to it and upright by its frame, sized by pscale; caps, uv along (u) and around (v); the curves' attributes on the mesh's points |
| `points_on_curve` | 1 (scene) | `group`, `by` (count, length, points), `count`, `length`, `keep_curves` | 3D: a point set along curves with tangent, N, up, u, curve and the curves' attributes |
| `particles_to_points` | 1 (scene with particles) | `group`, `keep_particles` | 3D: each particle stream's particles at the frame as a point set: id, v, age, life, pscale, Cd, orient, spin, and the attributes of the points they were born at |
| `light_ambient` / `light_directional` / `light_point` / `light_spot` / `light_area` | 0–1 (`look at`, a scene; not ambient) | `enabled`, `color`, `intensity`, `exposure` (stops), `shadows` (cast shadows, off by default) and `shadow_softness` (texels; not ambient); `translate` (not ambient), `rotate` (directional, spot, area), `decay` (point, spot, area), `radius` soft size (point, spot), `cone`, `penumbra`, `falloff` (spot), `shape` rectangle/disk, `size`, `two_sided`, `quality` (area: a quality × quality sample grid) | 3D: a light shining down its −Z (point: every way; ambient: from everywhere; area: from a rectangle or disk), aimed at what's in `look at` if wired; outputs a scene |
| `scene` | 4 scenes | — | 3D: gathers up to four cameras, cards, meshes, lights or scenes into one scene |
| `scanline_render` | scene, camera (scene, optional), background (optional), then any number of scenes or particle streams (connect to the last, empty input and a new one appears) | `width`, `height` (0 = project format), `samples` (anti-aliasing, n×n per pixel, default 3), `passes` (any of `depth`, `P`, `N`, `uv`, `motion`, `id`, `mattes`, `diffuse`, `specular`, `emission`, `ambient`, `lights`, `shadow`; default `["depth"]`), `motion_blur` (renders across the shutter, 0/1 = off, the default), `shutter` (frames, default 0.5), `shutter_offset` (centered / start / end), `jitter` (Jitter shutter, default true) | 3D: draws the scene's cards and meshes through a camera (the camera input's, else the scene's first) over the background; perspective-correct textures, surfaces composited front to back, holdouts cutting out what's behind them, tracker points as dots. Passes become planes: `depth` (distance in front of the camera, 0 where empty), `P.x/y/z` (scene position), `N.x/y/z` (normal), `uv.u/v`, `motion.u/v` (pixels, right and down, that what each pixel shows moves by the next frame: the scene and camera are also evaluated a frame later), `id` (object number, 0 empty), `matte.<node>` (each object's coverage), `diffuse.R/G/B`, `specular.*`, `emission.*`, `ambient.*` (adding up to the beauty), `light.<node>.R/G/B` (each light's share), `shadow.R/G/B`; depth/P/N/uv/motion/id from the nearest surface in each pixel, mattes adding up to alpha. Particle streams on the extra inputs render with everything else (sprites hidden behind nearer surfaces, or object instances), with motion vectors from their velocities. Motion blur averages renders across the shutter (each pixel's times jittered in a fixed dither pattern, so few samples give grain, not copies; `jitter` false (default true) gives every pixel the same times, as before 0.9.55): cameras, objects (deforming vertices too) and lights in between their places at the frames round each time (those frames are evaluated too), particles moved along their velocities and accelerations; the picture, mattes and light passes blur, depth/P/N/uv/motion/id stay the frame's own; the 3D view previews the blur once it settles, particle dots streaking over the same shutter, and a looked-through moving camera or light blurring it by its own motion |
| `particle_emitter` | 0–2 (`geometry` scene, `mask` image) | `shape` (point, line, rectangle, disk, sphere, box, geometry surface, geometry vertices, mask, point set: the geometry input's points, one particle each with Each point once, else at random, their v, pscale, Cd, life, orient used), `dimensions`, `surface`, `translate`/`rotate`/`scale`, `rate` (per frame), `burst`, `start`, `end`, `direction`, `speed`, `speed_var`, `spread`, `along_normal`, `life`, `life_var`, `size`, `size_var`, `size_end`, `color`, `color_end`, `rotation`, `rotation_var`, `spin`, `spin_var`, `seed`, `substeps`, `preroll` | particles: gives birth to them (repeatable per seed); outputs a particle stream |
| `particle_gravity` / `particle_wind` / `particle_turbulence` / `particle_attractor` / `particle_vortex` / `particle_bounce` / `particle_kill` / `particle_speed_limit` | 1 (`particles`) | `acceleration` (gravity); `wind`, `drag`; `amount`, `scale`, `speed`, `seed` (turbulence: curl noise); `position`, `amount`, `radius` (attractor); `center`, `axis`, `amount`, `radius` (vortex); `point`, `normal`, `bounce`, `friction` (bounce: a plane); `age`, `region`, `center`, `radius`, `size` (kill); `max` (speed limit); all: `strength`, `start`, `end` | particles: a force or constraint added to the stream, in chain order |
| `particle_render` | 1–3 (`particles`, `texture` image, `objects` scene) | `look` (points, sprites, objects), `outline` (round, square), `blend` (over, additive), `scale`, `opacity`, `opacity_end`, `texture_frames` (current frame, sequence), `sequence_first`, `sequence_last`, `pick` (by age, cycle, play, loop), `cycle_rate`, `random_start`, `orient` (none, velocity, spin) | particles: how they look where rendered: dots, camera-facing sprites (textured, faded over life) or copies of the objects input's meshes; a Scanline Render's motion blur streaks them along their velocities |
| `particles_2d` | 1–2 (`particles`, `background`) | — | draws particles straight onto the image, no camera: x, y in frame pixels (y down), size in pixels, as Particle Render says |
| `camera_tracker` | 1 (+ mask) | features, solve settings, solve mode (auto / free move / tripod), tripod distance, focal (known or solved), lens distortion K1/K2 (solved on request; Export then adds a Lens Distort undistorting the plate), mask channel / invert (no features where the mask covers) | 3D: solves a moving or a tripod (only turning) camera from the plate (**Track & Solve**, with progress and Stop; the result saves with the node), then **Export** adds a Camera (linked to the solve live, or keyed with Linked off), a Point Cloud, a Scene and a Scanline Render over the plate; with it selected, select solved points in the viewer (click, ⇧-click, drag a box) to **Place Card here** (on the plane through 3+ points, else facing the camera), **Place locator**, **Fit plane** (3+ points: a Fit Plane through them, on the tracker's point cloud: wire it into the Scene as a ground plane, Receives shadows to catch the CG's shadows, Holdout to cut CG behind the ground; select other points and **Use picked points** in its Properties to refit), or **Set origin**, **Ground plane** and **Set scale** (two points a given distance apart) |
| `point_cloud` | 0 | points (x, y, z) | 3D: points drawn as dots |
| `locator` | 0 | `translate` [3] | 3D: a point to wire into a Scene or link params to |

`examples/geo-scatter.json` puts the geometry nodes together: open it (**Help ▸ Examples** in the app, which ships it) and select a node to see its part.

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

In a geometry node's expression (Delete Geo), the formula runs once per
point, face or instance and reads it with @ names: `@P` (its place in the
scene; `@P.y` one part), `@N`, `@id`, `@index`, `@count`, and any
attribute as `@name`; `random(@id)` gives each element its own lasting
number. They can read other nodes' params as at the frame being made
(`dist(@P, locator1.translate)`), not through `at()`; `noise3(@P * 4)`
gives a smooth 3D noise in -1 to 1.

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
Other tools share the window's caches and particle simulations (a later frame
steps on rather than re-simulating) and run in order in the background, each
seeing the last one's edits; a call that would overwrite an edit made in the
window meanwhile is refused. Renders still use their own copy.

### MCP server (AI agents)

```sh
arclight mcp [graph.json]                 # JSON-RPC 2.0 over stdio, one message per line
arclight mcp --http 127.0.0.1:7878        # JSON-RPC over HTTP POST (/mcp)
arclight mcp --attach [PID]               # drive the newest (or given) running GUI
```

Register it with an MCP client, e.g. `claude mcp add arclight -- /path/to/arclight mcp`.
Tools: `list_ops`, `describe_op`, `get_graph`, `describe_node`, `create_node`,
`set_disabled` (turn nodes off, bypassed, or on; `describe_node` gives `disabled`), `set_node_position` / `layout_nodes` (place nodes in the node editor, one or many at once, or lay out the unplaced ones, or `all`, by depth left to right; `create_node` also takes a `pos`), `list_backdrops` / `create_backdrop` / `update_backdrop` / `delete_backdrop` (the node editor's group boxes: create one around nodes, or at `pos` with `size`; set `title`, `color` (palette name, index or [r, g, b]), `font_size` (8–72), `pos`, `size`, or refit it `around` nodes; deleting a box keeps its nodes; `list_backdrops` also gives the nodes fully inside each box), `delete_node`, `rename_node`, `set_params`, `set_key`, `export_tracker` (a Tracker's motion as a keyed Transform or Corner Pin), `track_camera`, `camera_points`, `orient_camera`, `export_camera`, `place_on_points` (the Camera Tracker: solve, read the points, set origin/ground/scale, export (`export_camera` links the Camera to the solve live by default; `"linked": false` keys a copy), place cards, locators and planes: `what` "plane" adds a Fit Plane), `scene_objects` (what a 3D node's scene holds: cameras, cards, meshes, splat captures, and lights with their type, world position and direction, color, intensity and settings), `particles` (a particle node's particles at a frame: ids, positions, velocities, ages, lives, sizes, colors, in pages of up to 100000), `splats` (a scene's Gaussian splats in pages: positions in the scene, scales, rotations (w,x,y,z), opacities, colors (linear RGB), and which capture (`sets`: node, count, SH degree, world)), `geometry` (a 3D node's points, mesh points, faces, instances or curve points as paged columns: P, N, uv, attributes; `domains` counts each), `connect`, `disconnect`,
`set_project` (incl. `folder`: relative paths are in it), `render_frame` (stats, optional file and PNG preview the model can
see), `render_output`, `probe_media`, `new_graph`, `load_graph`, `save_graph`,
`undo`, `redo`, `list_plugins`, `list_instances`, `attach_instance`,
`detach_instance`, `gpu_info` (GPU acceleration in use or why not, the
adapter and limits), `gpu_check` (the GPU self-test; `"quick": true` for
seconds). `render_frame` and
`render_output` take `"cpu": true` to render on the CPU only. A few tools only work on an attached GUI: `select_node`
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
ArclightVFX itself, `python/build.sh` builds the module in place:

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
info), `graph`, `plugins`, `instances`, `detach`, `set_positions({node: [x, y]})`, `layout(all=False)`, `backdrops()`, `backdrop(id)`,
`gpu_info()`, `gpu_check(quick=False)` (see GPU acceleration; `render(..., cpu=True)`
renders on the CPU only),
`create_backdrop(*nodes, title=, color=, font_size=, pos=, size=)` (a
`Backdrop` with `title` / `color` / `font_size` / `pos` / `size`, `set`, `fit(*nodes)`,
`nodes()`, `delete()`), and, when attached,
`select`, `view(node, frame=None)`, `view_state`. `Node` has `disabled` (read / write: turned off, bypassed), `pos` (read / write; `create_node(..., pos=[x, y])` too), `info`,
`params`, `set`, `input(s)`, `set_input`, `outputs`, `rename`, `render`,
`select`, `view`, `to_transform` / `to_corner_pin` (for a Tracker),
`track_camera`, `camera_points`, `orient_camera`, `export_camera(linked=True)`,
`place_on_points` (for a Camera Tracker), `scene_objects(frame=None)` (for
any 3D node: its cameras, cards, meshes, splat captures and lights, as the
MCP tool lists them), `particles(frame=None, offset=0, limit=None)` (a particle node's
particles as columns; every page unless `limit`), `splats(frame=None,
offset=0, limit=None)` (a scene's Gaussian splats as columns, likewise), `geometry(frame=None, domain=None, offset=0, limit=None, object=None)` (as the MCP tool; also `arclt.geometry(node, ...)`). Lights are nodes like any other: `create_node("light_spot",
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

Files must be named `libarclight_plugin_*.so` or `*.arclight.so`. ArclightVFX
searches `$ARCLIGHT_PLUGIN_PATH`, the `plugin_paths` setting,
`~/.local/share/arclight/plugins`, `/usr/local/lib/arclight/plugins` and
`/usr/lib/arclight/plugins`. Run `arclight plugins` to see the search path and
what loaded, and `arclight ops [OP]` to see the op catalog.

On Linux and macOS each plugin library runs in helper processes of
ArclightVFX's own (the same program, or the Python interpreter for `arclt`):
a plugin that crashes or hangs gives its node an error, with the crash
report's path, and is restarted at its next render. Images travel in
shared memory (about 30 ms for a 4K frame). `ARCLIGHT_PLUGINS_IN_PROCESS=1`
loads plugins into ArclightVFX itself (trusted plugins, a little faster);
`ARCLIGHT_PLUGIN_TIMEOUT=<seconds>` (default 60) is how long a render may
take. A plugin's ops are remembered, so a project still opens with them
when its plugin can't start.

### Settings and files (XDG)

| What | Where |
|------|-------|
| Settings | `$XDG_CONFIG_HOME/arclight/settings.json`, edited in **Edit ▸ Preferences** (Ctrl/⌘+,) or by hand (unknown keys are kept): `plugin_paths`, `frame_cache_mb` (GUI frame cache, default 1536), `mcp_http_addr`, `interface` (`ui_scale`, `theme` system / dark / light, `snap_to_grid`, `wire_style` curved / straight), `new_project` (`width`, `height`, `fps`, `first_frame`, `last_frame`: what File ▸ New starts with; Project Settings ▸ Save as defaults), `files` (`autosave_minutes` (0 off; writes `<project>.autosave` beside the project, never over it), `recent_count`, `undo_levels`, `project_dir`: the Projects folder, where File ▸ New makes projects' folders and Open starts), `view3d` (`emulate_three_button`, `trackpad` orbit / pan, `zoom_to_pointer`, `invert_zoom`, `default_shading` auto / solid / lit), `gpu` (`enabled`: GPU acceleration, default on; `ARCLIGHT_GPU=0` overrides; `cache_mb`: GPU memory for results kept there, default 2048; `frame_cache_mb`: GPU memory for played frames, default 1024, takes effect at the next start) |
| Crash reports | `$XDG_STATE_HOME/arclight/crashes/` (macOS `~/Library/Logs/Arclight/`), kept on this machine (see Help ▸ Report a problem) |
| Plugin ops cache | `$XDG_STATE_HOME/arclight/plugin-ops.json` |
| Recent projects | `$XDG_STATE_HOME/arclight/recent.json` (File ▸ Open Recent) |
| View preferences | `$XDG_STATE_HOME/arclight/view.json` (`~/.local/state/arclight/view.json`), written by the GUI: View ▸ Show frame size and the 3D view's Solid / Lit, wireframe and Lit shadows, remembered between runs (a project that saved its own 3D view keeps it); not part of projects or settings.json |
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
  (a seek in a long-GOP 4K clip costs about half a second). Out as H.264 (8-bit), HEVC (10-bit), ProRes 422 HQ (10-bit),
  ProRes 4444 with alpha, DNxHR HQX / 444 (10-bit), VP9 (10-bit, or 8-bit
  with alpha for the web) or AV1 (10-bit) (Media Out's Codec), converted with the BT.709
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
  geo-scatter.json  geometry nodes end to end: Scatter with a mask, Attribute Set, Instance on Points (rocks, cards facing the camera), sun shadows
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
