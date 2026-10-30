//! Compositing operators.
//!
//! Every operator consumes zero or more input images and produces one. All math
//! is in linear, premultiplied float. Operators are original implementations of
//! standard compositing primitives.
//!
//! [`OPS`] describes every operator — inputs, parameters, defaults — so the
//! engine and the GUI's property panels share one source of truth.

use crate::anim;
use crate::color::{self, Rgba};
use crate::geom;
use crate::render3d;
use crate::scene::{self, Scene};
use crate::image::{IRect, Image, Size};
use crate::media;
use crate::particle_nodes;
use crate::shape::{self, FillRule, Pt};
use crate::track::{self, Solve};
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Per-evaluation settings shared by every operator.
#[derive(Clone, Copy, Debug)]
pub struct EvalContext {
    /// The frame being rendered.
    pub frame: i64,
    /// Whether `write` nodes touch the disk. Off for interactive viewing.
    pub write_outputs: bool,
}

// ---------------------------------------------------------------------------
// Operator registry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Source,
    Color,
    Channel,
    Keyer,
    Merge,
    Shape,
    Transform,
    Filter,
    Output,
    /// 3D: cameras, cards and scenes.
    Scene,
}

/// What travels along a wire: an image, or a 3D scene (an image whose
/// [`scene`](Image::scene) is set, with no pixels). Particle streams
/// travel in scenes; a port of kind `Particles` takes only a particle
/// node's stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Scene,
    Particles,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Scene => "scene",
            Kind::Particles => "particles",
        }
    }

    /// The kind of a rendered output.
    pub fn of(img: &Image) -> Kind {
        if img.scene.is_some() { Kind::Scene } else { Kind::Image }
    }

    /// Whether a port of this kind takes `img`.
    pub fn takes(self, img: &Image) -> bool {
        match self {
            Kind::Particles => img.scene.as_ref().is_some_and(|s| !s.particles.is_empty()),
            k => k == Kind::of(img),
        }
    }
}

/// The kind of output `op` makes (a particle node's stream is a scene).
pub fn output_kind(op: &str) -> Kind {
    match op {
        "particles_2d" => Kind::Image,
        _ if particle_nodes::is_particle(op) => Kind::Scene,
        "camera" | "card" | "scene" | "point_cloud" | "locator" | "cube" | "sphere" | "cylinder" | "plane" | "read_geo" | "read_splat" | "splat_crop" | "splat_color" | "light_ambient" | "light_directional" | "light_point" | "light_spot" | "light_area" => Kind::Scene,
        _ => Kind::Image,
    }
}

/// Why the output of an `from_op` node can't go into port `port` of a
/// `to_op` node, if it can't: a scene into an image port or the other way,
/// or anything but a particle node's stream into a particles port.
pub fn wire_error(from_op: &str, to_op: &str, port: usize) -> Option<String> {
    let p = self::port(to_op, port)?;
    let k = output_kind(from_op);
    let stream = particle_nodes::is_particle(from_op) && k == Kind::Scene;
    let fits = match p.kind {
        Kind::Particles => stream,
        want => want == k,
    };
    let made = if stream { Kind::Particles } else { k };
    (!fits).then(|| format!("input '{}' takes {}, not {}", p.label, article(p.kind), article(made)))
}

/// "an image", "a scene", "particles".
pub fn article(k: Kind) -> &'static str {
    match k {
        Kind::Image => "an image",
        Kind::Scene => "a scene",
        Kind::Particles => "particles",
    }
}

/// How many fixed inputs `op` has before any number more of the same
/// kind, if it grows that way: a Scanline Render's scene, camera and
/// background, then more scenes or particle streams, rendered together.
/// The node editor keeps one spare empty input after the connected ones.
pub fn growable(op: &str) -> Option<usize> {
    (op == "scanline_render").then_some(3)
}

/// `op`'s input `i`: a fixed one, or a growable op's extra.
pub fn port(op: &str, i: usize) -> Option<Port> {
    let fixed = info(op)?.inputs;
    match fixed.get(i) {
        Some(p) => Some(*p),
        None => growable(op).filter(|&n| i >= n).map(|_| scene_in("scene")),
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Vec2,
    Vec3,
    /// Straight (unassociated) linear RGBA.
    Color,
    Choice(&'static [&'static str]),
    /// A file path. `save` picks a save dialog rather than an open dialog.
    Path { save: bool },
    /// Free text (a Shuffle's plane names).
    Text,
    /// Any of these names, as a JSON array of the chosen ones (checkboxes).
    Flags(&'static [&'static str]),
    Bool,
    /// A flat array of point data, `stride` numbers per point (x, y first).
    /// Animates as a whole: one key holds every point.
    Points { stride: usize },
}

impl ParamKind {
    pub const fn is_numeric(self) -> bool {
        matches!(self, ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Vec2 | ParamKind::Vec3 | ParamKind::Color | ParamKind::Points { .. })
    }
}

/// A param's default value: built in, or read from a plugin's descriptor.
#[derive(Clone, Copy)]
pub enum DefaultValue {
    Fn(fn() -> Value),
    Static(&'static Value),
}

pub struct ParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: DefaultValue,
    /// Can be keyframed. True for numeric params, except ones that define
    /// the timeline itself (frame ranges, rates).
    pub animatable: bool,
}

/// Viewer handles for editing an op's params directly on the image. Each
/// variant names the params it edits; positions are in image pixels with
/// y down, matching the ops' own coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Overlay {
    None,
    /// A draggable point bound to a Vec2 param.
    Point(&'static str),
    /// Move/rotate/scale gizmo about `center` (null: the input layer's
    /// center), plus `translate`.
    Transform { translate: &'static str, rotate: &'static str, scale: &'static str, center: &'static str },
    /// A Color param picked from the node's input in the viewer: Ctrl+click
    /// a pixel, or Ctrl+drag to average a box.
    Pick(&'static str),
    /// A box inset from the image edges by four Int params.
    Insets { left: &'static str, top: &'static str, right: &'static str, bottom: &'static str },
    /// A box (or the ellipse inside it) with center, size and rotation params.
    Box { center: &'static str, size: &'static str, rotate: &'static str, ellipse: bool },
    /// Editable vertices of a `Points` param.
    Points(&'static str),
    /// Tracker boxes: each enabled `track<i>` position with the shared
    /// pattern and search box sizes.
    Tracker { pattern: &'static str, search: &'static str },
    /// A Camera Tracker's solved points, drawn through the frame's solved
    /// camera, to select and place things on.
    CameraTracker,
    /// A quad of four Vec2 corners (`to`), and the four it is mapped from.
    CornerPin { to: [&'static str; 4], from: [&'static str; 4] },
    /// A Light 2D's handles, by its type: position, aim, cone and
    /// softness, radius, gradient, or an area's box.
    Light2d,
}

/// An input port.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub label: &'static str,
    /// An optional port may be left unconnected; the op then sees `None`.
    pub optional: bool,
    /// What it takes.
    pub kind: Kind,
}

const fn req(label: &'static str) -> Port {
    Port { label, optional: false, kind: Kind::Image }
}

const fn opt(label: &'static str) -> Port {
    Port { label, optional: true, kind: Kind::Image }
}

/// An optional port taking a scene.
const fn scene_in(label: &'static str) -> Port {
    Port { label, optional: true, kind: Kind::Scene }
}

const IN: Port = req("input");

pub struct OpInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub category: Category,
    /// Input ports, in order.
    pub inputs: &'static [Port],
    pub params: &'static [ParamSpec],
    /// When set, the op ends its ports with an optional [`MASK`] and its
    /// params with [`MIX`], [`MASK_CHANNEL`] and [`MASK_INVERT`], and
    /// [`eval`] blends the result back toward this input by `mix × mask`
    /// (the mask's chosen channel). Ops don't handle masking themselves.
    pub mask_base: Option<usize>,
    /// Handles the viewer shows while this op's node is selected.
    pub overlay: Overlay,
    pub help: &'static str,
}

const MERGE_OPS: &[&str] = &["over", "under", "add", "multiply", "screen", "min", "max", "zmerge"];

macro_rules! p {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: DefaultValue::Fn(|| json!($default)), animatable: $kind.is_numeric() }
    };
}

/// A param that can't be keyframed.
macro_rules! fixed {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: DefaultValue::Fn(|| json!($default)), animatable: false }
    };
}

/// The generic mask port of ops with [`OpInfo::mask_base`].
const MASK: Port = opt("mask");
/// The generic mix param of ops with [`OpInfo::mask_base`].
const MIX: ParamSpec = p!("mix", "Mix", ParamKind::Float { min: 0.0, max: 1.0 }, 1.0);
/// Which of the mask's channels masks (see [`mask_value`]).
pub const MASK_CHANNELS: &[&str] = &["alpha", "red", "green", "blue", "luminance"];
const MASK_CHANNEL: ParamSpec = fixed!("mask_channel", "Mask channel", ParamKind::Choice(MASK_CHANNELS), "alpha");
const MASK_INVERT: ParamSpec = fixed!("mask_invert", "Invert mask", ParamKind::Bool, false);
/// A 3D object's placement (as Card's) and a primitive's color.
/// Read Splat's Up axis: the capture's up as it is, Z-up, or Y down (Z
/// forward, as COLMAP / OpenCV; the default).
const SPLAT_UPS: &[&str] = &["y", "z", "-y"];
/// The scene a splat editing node changes (everything else passed on).
const SPLATS_IN: Port = Port { label: "scene", optional: false, kind: Kind::Scene };

const PLACE_T: ParamSpec = p!("translate", "Translate", ParamKind::Vec3, [0.0, 0.0, 0.0]);
const PLACE_R: ParamSpec = p!("rotate", "Rotate", ParamKind::Vec3, [0.0, 0.0, 0.0]);
const PLACE_S: ParamSpec = p!("scale", "Scale", ParamKind::Vec3, [1.0, 1.0, 1.0]);
const SURFACE_COLOR: ParamSpec = p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]);
/// A light's color, strength and falloff.
const LIGHT_COLOR: ParamSpec = p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]);
const LIGHT_INTENSITY: ParamSpec = p!("intensity", "Intensity", ParamKind::Float { min: 0.0, max: 100.0 }, 1.0);
const LIGHT_DECAY: ParamSpec = p!("decay", "Decay", ParamKind::Float { min: 0.0, max: 3.0 }, 0.0);
const LIGHT_EXPOSURE: ParamSpec = p!("exposure", "Exposure (stops)", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0);
const LIGHT_ENABLED: ParamSpec = fixed!("enabled", "Enabled", ParamKind::Bool, true);
const LIGHT_SHADOWS: ParamSpec = fixed!("shadows", "Cast shadows", ParamKind::Bool, false);
const LIGHT_SHADOW_SOFTNESS: ParamSpec = p!("shadow_softness", "Shadow softness", ParamKind::Float { min: 0.0, max: 20.0 }, 1.0);
/// Whether a 3D object blocks shadow-casting lights, and takes their shadows.
const CASTS_SHADOWS: ParamSpec = fixed!("casts_shadows", "Casts shadows", ParamKind::Bool, true);
const RECEIVES_SHADOWS: ParamSpec = fixed!("receives_shadows", "Receives shadows", ParamKind::Bool, true);
const LIGHT_RADIUS: ParamSpec = p!("radius", "Radius (soft size)", ParamKind::Float { min: 0.0, max: 100.0 }, 0.0);
/// A light's optional target: it aims at whatever this scene holds first.
const LOOK_AT: Port = scene_in("look at");
/// How a 3D object takes light (see [`crate::shade`]).
/// A Camera's projections.
pub const PROJECTIONS: &[&str] = &["perspective", "orthographic"];
const SHADING: ParamSpec = fixed!("shading", "Shading", ParamKind::Choice(&["unlit", "lambert", "blinn"]), "lambert");
const SPECULAR: ParamSpec = p!("specular", "Specular", ParamKind::Float { min: 0.0, max: 1.0 }, 0.0);
const ROUGHNESS: ParamSpec = p!("roughness", "Roughness", ParamKind::Float { min: 0.0, max: 1.0 }, 0.5);
const EMISSION: ParamSpec = p!("emission", "Emission", ParamKind::Color, [0.0, 0.0, 0.0, 1.0]);
/// How a Card takes light: unlit (its image as it is) unless chosen.
const CARD_SHADING: ParamSpec = fixed!("shading", "Shading", ParamKind::Choice(&["unlit", "lambert", "blinn"]), "unlit");
/// A 3D object that renders nothing but hides what's behind it.
const HOLDOUT: ParamSpec = fixed!("holdout", "Holdout", ParamKind::Bool, false);

const CHANNEL_SOURCES: &[&str] = &[
    "A red", "A green", "A blue", "A alpha", "B red", "B green", "B blue", "B alpha", "zero", "one",
];
const SWITCH_PORTS: usize = 5;
const KEYER_OUTPUTS: &[&str] = &["keyed", "matte", "despill only"];

const SIZE: ParamKind = ParamKind::Int { min: 1, max: 16384 };
/// A source's frame size: 0 takes the project's format.
const FRAME_SIZE: ParamKind = ParamKind::Int { min: 0, max: 16384 };

/// The project format when none is set, and the frame of sources whose size
/// is 0 when evaluated outside a graph.
pub const DEFAULT_FORMAT: Size = Size { width: 1920, height: 1080 };

/// Ops whose `width` / `height` set their frame when they have no input to
/// take one from.
/// How Read and Media In scale what they read to the project's format.
const PROJECT_FITS: &[&str] = &["none", "fit", "fill", "stretch", "width", "height"];
/// Its param. The default is none, so a project saved before it reads as it
/// did; new nodes get fit (see [`fit_new_shape`]).
const PROJECT_FIT: ParamSpec = fixed!("fit", "Fit to project", ParamKind::Choice(PROJECT_FITS), "none");

const SIZED_SOURCES: [&str; 7] = ["constant", "checkerboard", "rectangle", "ellipse", "polygon", "roto", "scanline_render"];

/// Give a source whose `width` / `height` is 0 (the default) the project's
/// `format`. The graph evaluator calls this after resolving keyframes, so
/// the cache key follows the format too.
pub fn apply_format(op: &str, params: &mut Value, format: Size) {
    // A Read or Media In fitting to the project: it reads the format, and
    // its cache key changes with it.
    if matches!(op, "read" | "media_in") && params.get("fit").and_then(Value::as_str).is_some_and(|f| f != "none") {
        params["project_format"] = json!([format.width, format.height]);
        return;
    }
    if !SIZED_SOURCES.contains(&op) {
        return;
    }
    for (name, v) in [("width", format.width), ("height", format.height)] {
        if params.get(name).and_then(Value::as_f64).is_none_or(|x| x < 1.0) {
            params[name] = json!(v);
        }
    }
}

/// Fit a new shape's default geometry (drawn for a 512×512 frame) to
/// `format`: centered, scaled to the shorter side; and a new Read or Media
/// In fits what it reads to the project. Call on a fresh node's defaults,
/// before any params given for it.
pub fn fit_new_shape(op: &str, params: &mut Value, format: Size) {
    if matches!(op, "read" | "media_in") {
        params["fit"] = json!("fit");
        return;
    }
    let k = format.width.min(format.height) as f64 / 512.0;
    let (cx, cy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);
    let place = |x: &Value, y: &Value| [cx + (num(x) - 256.0) * k, cy + (num(y) - 256.0) * k];
    match op {
        "rectangle" | "ellipse" => {
            let (c, s) = (params["center"].clone(), params["size"].clone());
            params["center"] = json!(place(&c[0], &c[1]));
            params["size"] = json!([num(&s[0]) * k, num(&s[1]) * k]);
        }
        "polygon" | "roto" => {
            let stride = if op == "roto" { 6 } else { 2 };
            let v: Vec<Value> = params["points"].as_array().cloned().unwrap_or_default();
            let fitted: Vec<f64> = v
                .chunks(stride)
                .flat_map(|c| {
                    let [x, y] = place(&c[0], &c[1]);
                    // Tangent offsets only scale.
                    std::iter::once(x).chain(std::iter::once(y)).chain(c[2..].iter().map(|t| num(t) * k)).collect::<Vec<_>>()
                })
                .collect();
            params["points"] = json!(fitted);
        }
        _ => {}
    }
}

/// A source's frame from its own size, 0 meaning [`DEFAULT_FORMAT`] (for
/// evaluation outside a graph, which fills in the project's).
fn source_size(width: usize, height: usize) -> Size {
    let pick = |own: usize, default: usize| if own == 0 { default } else { own };
    Size { width: pick(width, DEFAULT_FORMAT.width), height: pick(height, DEFAULT_FORMAT.height) }
}
const FEATHER: ParamKind = ParamKind::Float { min: 0.0, max: 200.0 };
const OPACITY: ParamKind = ParamKind::Float { min: 0.0, max: 1.0 };
const SHAPE_OPS: &[&str] = &["over", "subtract"];
/// Motion blur samples per frame; 0 or 1 is off.
const MOTION_SAMPLES: ParamKind = ParamKind::Int { min: 0, max: 64 };
/// How long the shutter is open, in frames.
const SHUTTER: ParamKind = ParamKind::Float { min: 0.0, max: 2.0 };
const SHUTTER_OFFSETS: &[&str] = &["centered", "start", "end"];
/// A Camera Tracker's solve: see [`crate::camtrack::Mode`].
pub const SOLVE_MODES: &[&str] = &["auto", "free move", "tripod"];
/// Ops with motion blur params: shapes average their coverage, the warps
/// their output images (see [`eval`]).
const MOTION_BLUR_OPS: [&str; 7] = ["rectangle", "ellipse", "polygon", "roto", "transform", "corner_pin", "tracker"];
const FILL_RULES: &[&str] = &["non-zero", "even-odd"];
/// Help text for shape ops: their own sentence plus what they share.
macro_rules! shape_help {
    ($s:literal) => {
        concat!(
            $s,
            " Without a background the canvas is Width × Height; with one, the shape \
             is drawn over it (or, with Subtract, cut out of its alpha). Feather is the \
             width of the soft edge, centered on the outline. Motion blur (samples per \
             frame, 0 for off) averages the animated shape over the shutter: Shutter frames \
             long, centered on the frame or starting or ending there. The alpha feeds mask \
             inputs directly."
        )
    };
}
const SOLVES: &[&str] = &["translate", "translate, rotate", "translate, rotate, scale"];
const FRAME: ParamKind = ParamKind::Int { min: -100_000, max: 1_000_000 };

pub static OPS: &[OpInfo] = &[
    OpInfo {
        name: "media_in",
        label: "Media In",
        category: Category::Source,
        inputs: &[],
        params: &[
            p!("path", "File", ParamKind::Path { save: false }, ""),
            fixed!("first_frame", "First frame", FRAME, 1),
            fixed!("last_frame", "Last frame", FRAME, 1),
            PROJECT_FIT,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads a still, an image sequence (shot.####.exr) or a video file. \
               Frames outside the range hold the first/last frame. Fit to project scales it to the project's format: \
               fit (all of it, with transparent bars), fill (covering it, cropped), stretch (exactly, ignoring the aspect), \
               width or height (matching that, keeping the aspect), or none (its own size).",
    },
    OpInfo {
        name: "constant",
        label: "Constant",
        category: Category::Source,
        inputs: &[],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A solid color fill.",
    },
    OpInfo {
        name: "checkerboard",
        label: "Checkerboard",
        category: Category::Source,
        inputs: &[],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            p!("size", "Square size", ParamKind::Int { min: 1, max: 4096 }, 32),
            p!("color_a", "Color A", ParamKind::Color, [0.18, 0.18, 0.18, 1.0]),
            p!("color_b", "Color B", ParamKind::Color, [0.5, 0.5, 0.5, 1.0]),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A test pattern.",
    },
    OpInfo {
        name: "read",
        label: "Read",
        category: Category::Source,
        inputs: &[],
        params: &[p!("path", "File", ParamKind::Path { save: false }, ""), PROJECT_FIT],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads an image file, or a video file (its first frame at frame 1, holding the first and last frames \
               outside it). Prefer Media In for sequences and to set a video's range. Fit to project scales it to the \
               project's format (as Media In's).",
    },
    OpInfo {
        name: "grade",
        label: "Grade",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[
            p!("multiply", "Multiply", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            p!("offset", "Offset", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("gamma", "Gamma", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Per-channel gain, offset and gamma on unpremultiplied color.",
    },
    OpInfo {
        name: "merge",
        label: "Merge",
        category: Category::Merge,
        inputs: &[req("A"), req("B"), MASK],
        params: &[
            p!("operation", "Operation", ParamKind::Choice(MERGE_OPS), "over"),
            fixed!("swap", "Swap A / B", ParamKind::Bool, false),
            MIX,
            MASK_CHANNEL,
            MASK_INVERT,
        ],
        overlay: Overlay::None,
        mask_base: Some(1),
        help: "Combines A (foreground) with B (background), in B's frame. Swap A / B trades them for any \
               operation (B over A, and the frame is A's). The optional mask input limits where A is combined: \
               outside it the output is B; Mask channel picks which of its channels masks, Invert mask flips it, \
               and Mix fades the whole result. zmerge combines by the inputs' depth passes: at each pixel the \
               nearer goes over the farther (A in front where they tie), soft edges blended, the other passes carried.",
    },
    OpInfo {
        name: "transform",
        label: "Transform",
        category: Category::Transform,
        inputs: &[IN, MASK],
        params: &[
            p!("translate", "Translate", ParamKind::Vec2, [0.0, 0.0]),
            p!("scale", "Scale", ParamKind::Vec2, [1.0, 1.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            // Null: the center of the input's data window, so the pivot
            // follows a shape or moved layer.
            p!("center", "Center", ParamKind::Vec2, Value::Null),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::Transform { translate: "translate", rotate: "rotate", scale: "scale", center: "center" },
        mask_base: Some(0),
        help: "Translate, scale and rotate about the center: by default the middle of the input layer (its data window). \
               Motion blur (samples per frame, 0 for off) averages the animated move over the shutter, \
               Shutter frames long, centered on the frame or starting or ending there.",
    },
    OpInfo {
        name: "blur",
        label: "Blur",
        category: Category::Filter,
        inputs: &[IN, MASK],
        params: &[
            p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 200.0 }, 8.0),
            p!("passes", "Passes", ParamKind::Int { min: 1, max: 6 }, 3),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Repeated box blur; 3 passes approximate a gaussian.",
    },
    OpInfo {
        name: "exposure",
        label: "Exposure",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[p!("stops", "Stops", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0), MIX, MASK_CHANNEL, MASK_INVERT],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Brightens or darkens color by photographic stops (×2 per stop).",
    },
    OpInfo {
        name: "light_2d",
        label: "Light 2D",
        category: Category::Color,
        inputs: &[IN, opt("normals"), opt("depth"), MASK],
        params: &[
            fixed!("type", "Type", ParamKind::Choice(&["spot", "area", "point", "directional"]), "spot"),
            p!("position", "Position", ParamKind::Vec2, [960.0, 540.0]),
            p!("direction", "Direction", ParamKind::Float { min: -360.0, max: 360.0 }, 90.0),
            p!("cone", "Cone angle", ParamKind::Float { min: 0.0, max: 360.0 }, 60.0),
            p!("softness", "Softness", ParamKind::Float { min: 0.0, max: 180.0 }, 10.0),
            p!("reach", "Reach", ParamKind::Float { min: 0.0, max: 100000.0 }, 0.0),
            fixed!("shape", "Area shape", ParamKind::Choice(&["rectangle", "ellipse"]), "rectangle"),
            p!("size", "Size", ParamKind::Vec2, [400.0, 300.0]),
            p!("rotation", "Rotation", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            p!("feather", "Feather", ParamKind::Float { min: 0.0, max: 10000.0 }, 50.0),
            p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 100000.0 }, 400.0),
            p!("falloff", "Falloff", ParamKind::Float { min: 0.0, max: 20.0 }, 1.0),
            p!("gradient", "Gradient", ParamKind::Float { min: 0.0, max: 100000.0 }, 300.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("intensity", "Intensity", ParamKind::Float { min: 0.0, max: 100.0 }, 1.0),
            p!("exposure", "Exposure (stops)", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0),
            fixed!("blend", "Blend", ParamKind::Choice(&["add", "screen", "multiply"]), "add"),
            p!("height", "Height", ParamKind::Float { min: 0.0, max: 100000.0 }, 100.0),
            p!("elevation", "Elevation", ParamKind::Float { min: 0.0, max: 90.0 }, 45.0),
            p!("specular", "Specular", ParamKind::Float { min: 0.0, max: 1.0 }, 0.0),
            p!("roughness", "Roughness", ParamKind::Float { min: 0.0, max: 1.0 }, 0.5),
            p!("depth_scale", "Depth scale", ParamKind::Float { min: 0.0, max: 10000.0 }, 1.0),
            MIX,
            MASK_CHANNEL,
            MASK_INVERT,
        ],
        overlay: Overlay::Light2d,
        mask_base: Some(0),
        help: "Lights the image in 2D: a Spot from Position toward Direction (degrees, 0 right, clockwise) within Cone angle, \
               its edge soft over Softness degrees, fading to half at Reach pixels (0: no fade); an Area rectangle or ellipse \
               Size across, turned by Rotation, its edge soft over Feather pixels; a Point full at Position and gone at Radius \
               ((1 − d / Radius)^Falloff); a Directional light across the frame toward Direction, fading over Gradient pixels \
               past Position (0: even). Color × Intensity × 2^Exposure goes on by Blend (add, screen, multiply). With normals \
               (a render's N pass, or a normal map) it shades the image as a surface, the light Height pixels above it (a \
               directional one at Elevation degrees), with a Specular highlight sharp to broad by Roughness; depth (a Z pass \
               × Depth scale pixels) moves the surface back. Drag its handles in the viewer.",
    },
    OpInfo {
        name: "saturation",
        label: "Saturation",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[p!("saturation", "Saturation", ParamKind::Float { min: 0.0, max: 4.0 }, 1.0), MIX, MASK_CHANNEL, MASK_INVERT],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Scales color away from (or toward) its Rec. 709 luminance. 0 is grayscale.",
    },
    OpInfo {
        name: "invert",
        label: "Invert",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[p!("channels", "Channels", ParamKind::Choice(&["rgb", "alpha", "rgba"]), "rgb"), MIX, MASK_CHANNEL, MASK_INVERT],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "1 − value on unpremultiplied color and/or alpha. Inverting alpha \
               keeps the color, so an inverted matte stays a usable mask.",
    },
    OpInfo {
        name: "clamp",
        label: "Clamp",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[
            p!("min", "Min", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0),
            p!("max", "Max", ParamKind::Float { min: -10.0, max: 10.0 }, 1.0),
            p!("channels", "Channels", ParamKind::Choice(&["rgb", "rgba"]), "rgba"),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Limits channel values to [min, max]. Alpha is always kept within 0–1.",
    },
    OpInfo {
        name: "premult",
        label: "Premultiply",
        category: Category::Channel,
        inputs: &[IN],
        params: &[],
        overlay: Overlay::None,
        mask_base: None,
        help: "Multiplies color by alpha. Use after Unpremultiply, or on images \
               whose color is straight (unassociated).",
    },
    OpInfo {
        name: "unpremult",
        label: "Unpremultiply",
        category: Category::Channel,
        inputs: &[IN],
        params: &[],
        overlay: Overlay::None,
        mask_base: None,
        help: "Divides color by alpha. Nodes after it see straight color, so \
               follow them with Premultiply. (Grade already does this internally.)",
    },
    OpInfo {
        name: "keyer",
        label: "Keyer",
        category: Category::Keyer,
        inputs: &[IN, opt("clean plate"), MASK],
        params: &[
            // Null: estimated from the image (its dominant green or blue).
            p!("screen", "Screen color", ParamKind::Color, Value::Null),
            p!("balance", "Balance", ParamKind::Float { min: 0.0, max: 1.0 }, 0.5),
            p!("clip_black", "Clip black", ParamKind::Float { min: 0.0, max: 1.0 }, 0.0),
            p!("clip_white", "Clip white", ParamKind::Float { min: 0.0, max: 1.0 }, 1.0),
            p!("despill", "Despill", ParamKind::Float { min: 0.0, max: 1.0 }, 1.0),
            p!("shrink_grow", "Shrink / grow", ParamKind::Float { min: -50.0, max: 50.0 }, 0.0),
            p!("softness", "Softness", ParamKind::Float { min: 0.0, max: 50.0 }, 0.0),
            fixed!("output", "Output", ParamKind::Choice(KEYER_OUTPUTS), "keyed"),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::Pick("screen"),
        mask_base: Some(0),
        help: "Pulls a key from a green or blue screen by color difference: how far \
               each pixel's screen channel rises above the other two, against the \
               screen color's. Screen color: picked from the image unless set; a \
               clean plate (the empty screen) evens out uneven lighting; Ctrl+click \
               or Ctrl+drag in the viewer picks it from the input. Balance \
               weighs the larger of the other two channels (1) against the smaller \
               (0). Clip black/white crush the matte's ends; despill pulls the \
               screen channel down to the others. Shrink / grow moves the matte's edge in \
               (negative) or out by that many pixels; Softness blurs it, reaching about that far. Output: the keyed image \
               (premultiplied), the matte, or the despilled plate.",
    },
    OpInfo {
        name: "channel_select",
        label: "Channel Select",
        category: Category::Channel,
        inputs: &[req("A"), opt("B")],
        params: &[
            fixed!("red", "Red from", ParamKind::Choice(CHANNEL_SOURCES), "A red"),
            fixed!("green", "Green from", ParamKind::Choice(CHANNEL_SOURCES), "A green"),
            fixed!("blue", "Blue from", ParamKind::Choice(CHANNEL_SOURCES), "A blue"),
            fixed!("alpha", "Alpha from", ParamKind::Choice(CHANNEL_SOURCES), "A alpha"),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Builds each output channel from any channel of A or B, or a \
               constant. Copies values as stored (premultiplied). Alpha from B \
               copies a matte across; follow with Premultiply if A's color was straight.",
    },
    OpInfo {
        name: "shuffle",
        label: "Shuffle",
        category: Category::Channel,
        inputs: &[IN],
        params: &[
            fixed!("in", "In", ParamKind::Text, "rgba"),
            fixed!("red", "Red from", ParamKind::Text, "R"),
            fixed!("green", "Green from", ParamKind::Text, "G"),
            fixed!("blue", "Blue from", ParamKind::Text, "B"),
            fixed!("alpha", "Alpha from", ParamKind::Text, "A"),
            fixed!("out", "Out", ParamKind::Text, "rgba"),
            fixed!("remove", "Remove", ParamKind::Text, ""),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Moves channels and render passes. In brings a pass (P, N, depth, matte.<node>, diffuse, …) \
               into the color, to grade, blur or key it with any node; Red, Green, Blue and Alpha from take \
               each channel from R, G, B, A, any plane (N.y, matte.Cube1) or 0 / 1. Out writes the result \
               into a named pass instead, leaving the color as it was; Remove drops passes once used \
               (names separated by commas or spaces, * for all). Values are copied as stored.",
    },
    OpInfo {
        name: "switch",
        label: "Switch",
        category: Category::Merge,
        inputs: &[opt("0"), opt("1"), opt("2"), opt("3"), opt("4")],
        params: &[p!("which", "Which", ParamKind::Int { min: 0, max: SWITCH_PORTS as i64 - 1 }, 0)],
        overlay: Overlay::None,
        mask_base: None,
        help: "Passes one of its inputs through. Keyframe Which to cut between \
               inputs over time.",
    },
    OpInfo {
        name: "dissolve",
        label: "Dissolve",
        category: Category::Merge,
        inputs: &[req("A"), req("B")],
        params: &[p!("amount", "Amount", ParamKind::Float { min: 0.0, max: 1.0 }, 0.5)],
        overlay: Overlay::None,
        mask_base: None,
        help: "Cross-fades from A (0) to B (1).",
    },
    OpInfo {
        name: "keymix",
        label: "Keymix",
        category: Category::Merge,
        inputs: &[req("A"), req("B"), MASK],
        params: &[MIX, MASK_CHANNEL, MASK_INVERT],
        overlay: Overlay::None,
        mask_base: Some(1),
        help: "A where the mask's alpha is 1, B where it is 0. Unlike Merge \
               over, A's own alpha is ignored. Without a mask the output is A.",
    },
    OpInfo {
        name: "crop",
        label: "Crop",
        category: Category::Transform,
        inputs: &[IN],
        params: &[
            p!("left", "Left", ParamKind::Int { min: 0, max: 16384 }, 0),
            p!("top", "Top", ParamKind::Int { min: 0, max: 16384 }, 0),
            p!("right", "Right", ParamKind::Int { min: 0, max: 16384 }, 0),
            p!("bottom", "Bottom", ParamKind::Int { min: 0, max: 16384 }, 0),
            fixed!("mode", "Mode", ParamKind::Choice(&["clear outside", "shrink canvas"]), "clear outside"),
        ],
        overlay: Overlay::Insets { left: "left", top: "top", right: "right", bottom: "bottom" },
        mask_base: None,
        help: "Removes pixels from each edge: either makes them transparent \
               (keeping the frame size) or shrinks the image.",
    },
    OpInfo {
        name: "resize",
        label: "Resize",
        category: Category::Transform,
        inputs: &[IN],
        params: &[
            p!("width", "Width", SIZE, 1920),
            p!("height", "Height", SIZE, 1080),
            fixed!("fit", "Fit", ParamKind::Choice(&["stretch", "fit", "fill"]), "fit"),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Resamples to a new frame size. Fit keeps the aspect ratio inside \
               the frame (with transparent bars); fill covers it and crops.",
    },
    OpInfo {
        name: "camera",
        label: "Camera",
        category: Category::Scene,
        inputs: &[],
        params: &[
            p!("focal", "Focal length (mm)", ParamKind::Float { min: 1.0, max: 1000.0 }, 35.0),
            p!("film_back", "Film back (mm)", ParamKind::Vec2, [36.0, 24.0]),
            fixed!("projection", "Projection", ParamKind::Choice(PROJECTIONS), "perspective"),
            p!("ortho_width", "Ortho width", ParamKind::Float { min: 0.001, max: 100000.0 }, 10.0),
            p!("translate", "Translate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("rotate", "Rotate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("scale", "Scale", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            p!("near", "Near", ParamKind::Float { min: 0.001, max: 1000.0 }, 0.1),
            p!("far", "Far", ParamKind::Float { min: 1.0, max: 1000000.0 }, 10000.0),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A pinhole camera for a Scanline Render: it looks down its −Z axis (Y up), placed by Translate, \
               then Rotate (degrees about X, then Y, then Z). The film back's width spans the frame's width. \
               Projection orthographic makes it see straight down its axis instead, Ortho width scene units \
               across the frame at any distance (no perspective: a plan or elevation view). \
               Outputs a scene holding the camera.",
    },
    OpInfo {
        name: "card",
        label: "Card",
        category: Category::Scene,
        inputs: &[req("image")],
        params: &[
            p!("translate", "Translate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("rotate", "Rotate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("scale", "Scale", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            // Null: 1 wide, as high as the image's aspect makes it.
            p!("size", "Size", ParamKind::Vec2, Value::Null),
            p!("opacity", "Opacity", OPACITY, 1.0),
            CARD_SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Puts its image on a flat card in 3D, centered on its origin and facing +Z, Size wide and high \
               (by default 1 wide, keeping the image's aspect), then scaled, rotated and moved. Opacity \
               fades the whole card (its shadow too), e.g. clouds at different depths. \
               Shading is unlit by default (the image shows as it is); Lambert or Blinn lights it by \
               the scene's lights (from either side, like a real card), with Specular, Roughness and \
               Emission as on the mesh nodes, and shadows fall on it if Receives shadows is on. \
               Outputs a scene holding the card.",
    },
    OpInfo {
        name: "cube",
        label: "Cube",
        category: Category::Scene,
        inputs: &[opt("texture")],
        params: &[
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("size", "Size", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            fixed!("segments", "Segments", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            SURFACE_COLOR,
            SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            HOLDOUT,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A box in 3D, Size across, centered on its origin, each face divided into Segments, then scaled, \
               rotated and moved. Each face shows the whole texture (if connected) times Color. Outputs a scene holding it.",
    },
    OpInfo {
        name: "sphere",
        label: "Sphere",
        category: Category::Scene,
        inputs: &[opt("texture")],
        params: &[
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("radius", "Radius", ParamKind::Float { min: 0.001, max: 10000.0 }, 0.5),
            fixed!("segments", "Segments (around, up)", ParamKind::Vec2, [32.0, 16.0]),
            SURFACE_COLOR,
            SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            HOLDOUT,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A sphere in 3D, centered on its origin, made of Segments around and up, then scaled, rotated and moved. \
               The texture (if connected) wraps around it once, its middle facing +Z; times Color. Outputs a scene holding it.",
    },
    OpInfo {
        name: "cylinder",
        label: "Cylinder",
        category: Category::Scene,
        inputs: &[opt("texture")],
        params: &[
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("radius", "Radius", ParamKind::Float { min: 0.001, max: 10000.0 }, 0.5),
            p!("height", "Height", ParamKind::Float { min: 0.001, max: 10000.0 }, 1.0),
            fixed!("segments", "Segments (around, up)", ParamKind::Vec2, [32.0, 1.0]),
            fixed!("caps", "Caps", ParamKind::Bool, true),
            SURFACE_COLOR,
            SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            HOLDOUT,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A cylinder in 3D standing on its Y axis, centered on its origin, with flat ends if Caps, then scaled, \
               rotated and moved. The texture (if connected) wraps around its side once; times Color. Outputs a scene holding it.",
    },
    OpInfo {
        name: "plane",
        label: "Plane",
        category: Category::Scene,
        inputs: &[opt("texture")],
        params: &[
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("size", "Size", ParamKind::Vec2, [1.0, 1.0]),
            fixed!("segments", "Segments", ParamKind::Vec2, [1.0, 1.0]),
            SURFACE_COLOR,
            SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            HOLDOUT,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "A flat rectangle in 3D facing +Z, Size wide and high, divided into Segments across and up, then scaled, \
               rotated and moved: like a Card, but sized by its own params and repeating its texture (times Color). \
               Outputs a scene holding it.",
    },
    OpInfo {
        name: "read_geo",
        label: "Read Geo",
        category: Category::Scene,
        inputs: &[],
        params: &[
            fixed!("file", "File", ParamKind::Path { save: false }, ""),
            fixed!("frame_rate", "Frame rate (Alembic)", ParamKind::Float { min: 1.0, max: 240.0 }, 24.0),
            p!("frame_offset", "Frame offset (Alembic)", ParamKind::Float { min: -100000.0, max: 100000.0 }, 0.0),
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("opacity", "Opacity", OPACITY, 1.0),
            fixed!("override_material", "Override material", ParamKind::Bool, false),
            SHADING,
            SPECULAR,
            ROUGHNESS,
            EMISSION,
            HOLDOUT,
            CASTS_SHADOWS,
            RECEIVES_SHADOWS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads meshes from a Wavefront OBJ file (with the colors and textures of its MTL materials), a glTF 2.0 \
               file (.gltf or .glb) or an Alembic cache (.abc, Ogawa: its polygon and subdivision meshes, moving and \
               deforming, under their transforms; white, as it has no materials), then scales, rotates and moves them all. \
               An Alembic cache is read at the frame's time: (frame + Frame offset) ÷ Frame rate seconds, the rate \
               starting as the project's. Opacity fades them all (each material's own \
               opacity times it, their shadows too), with or without Override material. Outputs a scene holding one mesh \
               per object and material in the file.",
    },
    OpInfo {
        name: "read_splat",
        label: "Read Splat",
        category: Category::Scene,
        inputs: &[],
        params: &[
            fixed!("file", "File", ParamKind::Path { save: false }, ""),
            fixed!("up", "Up axis", ParamKind::Choice(SPLAT_UPS), "-y"),
            PLACE_T,
            PLACE_R,
            PLACE_S,
            p!("opacity", "Opacity", OPACITY, 1.0),
            fixed!("sh_degree", "Color detail (SH degree)", ParamKind::Int { min: 0, max: 3 }, 3),
            HOLDOUT,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads a Gaussian splat capture: a 3D Gaussian Splatting .ply file, as Postshot, Polycam, Luma, Nerfstudio, \
               gsplat and the INRIA code export it, a .splat file (the antimatter15 web format: one color per splat) or a \
               .spz file (Niantic's compressed format, versions 2 and 3; read Y down like the others). Up axis says which way is up in the \
               capture: -y (Y down, Z forward, the default: captures from COLMAP / OpenCV, as most .ply and .splat files and every .spz \
               are), y (already Y up, as Arclight), or z (Z-up tools). Translate, Rotate and Scale then place it. Opacity fades every splat. Color detail \
               keeps the color's change with the viewing angle up to that spherical-harmonics degree (0: one color per \
               splat, the quickest; 3: all of it). Holdout draws nothing but hides what's behind the splats, as a holdout mesh \
               does. Outputs a scene holding the capture.",
    },
    OpInfo {
        name: "splat_crop",
        label: "Splat Crop",
        category: Category::Scene,
        inputs: &[SPLATS_IN],
        params: &[
            fixed!("shape", "Shape", ParamKind::Choice(&["box", "sphere"]), "box"),
            p!("center", "Center", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("size", "Size", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            p!("rotate", "Rotate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            fixed!("invert", "Invert", ParamKind::Bool, false),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Keeps the Gaussian splats whose centers are inside a box or a sphere (in the scene: Center, Size across \
               each way, a sphere stretched by it, turned by Rotate), and drops the rest: to cut a capture down to what's \
               wanted or clear away floaters at its edges. Invert keeps those outside instead. Every capture in the scene \
               is cropped; everything else passes through.",
    },
    OpInfo {
        name: "splat_color",
        label: "Splat Color",
        category: Category::Scene,
        inputs: &[SPLATS_IN],
        params: &[
            p!("exposure", "Exposure (stops)", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0),
            p!("saturation", "Saturation", ParamKind::Float { min: 0.0, max: 4.0 }, 1.0),
            p!("gain", "Gain", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", ParamKind::Float { min: 0.0, max: 4.0 }, 1.0),
            fixed!("view_independent", "View independent", ParamKind::Bool, false),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Grades Gaussian splat captures' colors: Exposure (stops), Saturation (0: gray, by Rec. 709 luminance) and \
               Gain (a tint, red, green and blue), exactly at every viewing angle; and multiplies their opacity by \
               Opacity (at most fully opaque). View independent drops the change of color with the viewing angle (to \
               SH degree 0: quicker to render, and steadier where a capture's reflections flicker). Every capture in the \
               scene is graded; everything else passes through.",
    },
    OpInfo {
        name: "light_ambient",
        label: "Ambient Light",
        category: Category::Scene,
        inputs: &[],
        params: &[LIGHT_ENABLED, LIGHT_COLOR, LIGHT_INTENSITY, LIGHT_EXPOSURE],
        overlay: Overlay::None,
        mask_base: None,
        help: "Light from everywhere, evenly: lights the dark sides of Lambert and Blinn objects as much as the \
               lit ones (Color × Intensity × 2^Exposure of their color). Outputs a scene holding it; with no lights in a \
               scene, everything renders unlit (a light that isn't Enabled still counts, but adds nothing).",
    },
    OpInfo {
        name: "light_directional",
        label: "Directional Light",
        category: Category::Scene,
        inputs: &[LOOK_AT],
        params: &[LIGHT_ENABLED, PLACE_T, PLACE_R, LIGHT_COLOR, LIGHT_INTENSITY, LIGHT_EXPOSURE, LIGHT_SHADOWS, LIGHT_SHADOW_SOFTNESS],
        overlay: Overlay::None,
        mask_base: None,
        help: "Parallel light, as from the sun, shining down its −Z axis as Rotate turns it (Translate only places it \
               in the 3D view); the same strength everywhere. With a scene in Look at (a Locator, Card, Camera…) it \
               aims at that instead. Exposure brightens it in stops. Outputs a scene holding it.",
    },
    OpInfo {
        name: "light_point",
        label: "Point Light",
        category: Category::Scene,
        inputs: &[LOOK_AT],
        params: &[LIGHT_ENABLED, PLACE_T, LIGHT_COLOR, LIGHT_INTENSITY, LIGHT_EXPOSURE, LIGHT_DECAY, LIGHT_RADIUS, LIGHT_SHADOWS, LIGHT_SHADOW_SOFTNESS],
        overlay: Overlay::None,
        mask_base: None,
        help: "Light from a point, every way. Decay is how it fades with distance d: Intensity / d^Decay \
               (0: not at all, 1: linearly, 2: as real light does). Radius gives it a soft size: it shines from a ball \
               that big, for broader highlights. Outputs a scene holding it.",
    },
    OpInfo {
        name: "light_spot",
        label: "Spot Light",
        category: Category::Scene,
        inputs: &[LOOK_AT],
        params: &[
            LIGHT_ENABLED,
            PLACE_T,
            PLACE_R,
            LIGHT_COLOR,
            LIGHT_INTENSITY,
            LIGHT_EXPOSURE,
            LIGHT_DECAY,
            p!("cone", "Cone angle", ParamKind::Float { min: 0.0, max: 179.0 }, 40.0),
            p!("penumbra", "Penumbra", ParamKind::Float { min: 0.0, max: 90.0 }, 5.0),
            p!("falloff", "Falloff exponent", ParamKind::Float { min: 0.0, max: 100.0 }, 0.0),
            LIGHT_RADIUS,
            LIGHT_SHADOWS,
            LIGHT_SHADOW_SOFTNESS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Light from a point down its −Z axis, within a cone Cone angle degrees across, its edge softened over \
               Penumbra degrees and dimmed toward it by the cosine from the axis to the power Falloff exponent; fading \
               with distance by Decay and softened by Radius (as a Point Light). With a scene in Look at it aims at \
               that instead of turning by Rotate. Outputs a scene holding it.",
    },
    OpInfo {
        name: "light_area",
        label: "Area Light",
        category: Category::Scene,
        inputs: &[LOOK_AT],
        params: &[
            LIGHT_ENABLED,
            PLACE_T,
            PLACE_R,
            LIGHT_COLOR,
            LIGHT_INTENSITY,
            LIGHT_EXPOSURE,
            LIGHT_DECAY,
            fixed!("shape", "Shape", ParamKind::Choice(&["rectangle", "disk"]), "rectangle"),
            p!("size", "Size", ParamKind::Vec2, [1.0, 1.0]),
            fixed!("two_sided", "Two-sided", ParamKind::Bool, false),
            fixed!("quality", "Quality", ParamKind::Int { min: 1, max: 16 }, 4),
            LIGHT_SHADOWS,
            LIGHT_SHADOW_SOFTNESS,
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Light from a rectangle (or a disk) Size across, shining down its −Z axis (both ways if Two-sided), for soft \
               light and broad highlights. Intensity is the whole light's, shared over a Quality × Quality grid of points \
               on it (more is smoother and slower); each point fades with distance by Decay and lights more squarely in \
               front. With a scene in Look at it aims at that. Outputs a scene holding it.",
    },
    OpInfo {
        name: "scene",
        label: "Scene",
        category: Category::Scene,
        inputs: &[scene_in("1"), scene_in("2"), scene_in("3"), scene_in("4")],
        params: &[],
        overlay: Overlay::None,
        mask_base: None,
        help: "Gathers cameras, cards, meshes, lights and other scenes into one scene.",
    },
    OpInfo {
        name: "camera_tracker",
        label: "Camera Tracker",
        category: Category::Scene,
        inputs: &[IN, MASK],
        params: &[
            fixed!("features", "Features", ParamKind::Int { min: 20, max: 2000 }, 300),
            fixed!("min_distance", "Feature spacing (px)", ParamKind::Float { min: 2.0, max: 100.0 }, 12.0),
            fixed!("focal", "Focal length (mm)", ParamKind::Float { min: 1.0, max: 1000.0 }, 35.0),
            fixed!("film_back", "Film back (mm)", ParamKind::Vec2, [36.0, 24.0]),
            fixed!("solve_focal", "Solve focal length", ParamKind::Bool, false),
            fixed!("solve_distortion", "Solve lens distortion", ParamKind::Bool, false),
            fixed!("max_error", "Max error (px)", ParamKind::Float { min: 0.2, max: 5.0 }, 1.0),
            fixed!("solve_mode", "Solve", ParamKind::Choice(SOLVE_MODES), "auto"),
            fixed!("distance", "Tripod distance", ParamKind::Float { min: 0.1, max: 10000.0 }, 10.0),
            MASK_CHANNEL,
            MASK_INVERT,
        ],
        overlay: Overlay::CameraTracker,
        mask_base: None,
        help: "Works out the camera's path through the shot, and points in 3D, from features it finds and follows \
               over the project's frame range (Track & Solve in its panel). It passes its input through; the viewer \
               shows the solved points, to select and place a Card or a Locator on, or to set the origin, ground and \
               scale. Export makes a keyed Camera, the point cloud, a Scene and a Scanline Render over the plate. \
               The focal length is used as given, or as a starting guess with Solve focal length (which needs the camera to move, \
               or to turn enough on a tripod). Solve lens distortion also solves the lens's radial K1 and K2 (as Lens Distort's, \
               about the frame's center; not for a Tripod solve), and Export then undistorts the plate with them. Solve: Free move for a camera that travels, Tripod for one that only turns about \
               its center (its points go on a sphere Tripod distance away, as they have no depth), or Auto (picks Tripod when \
               the camera only turns). A mask (its Mask channel, frame by frame, so an animated Roto follows along) keeps the \
               tracker off what it covers, such as people or cars moving through the shot: no features are found there, and \
               features going into it are dropped.",
    },
    OpInfo {
        name: "point_cloud",
        label: "Point Cloud",
        category: Category::Scene,
        inputs: &[],
        params: &[fixed!("points", "Points", ParamKind::Points { stride: 3 }, Vec::<f64>::new())],
        overlay: Overlay::None,
        mask_base: None,
        help: "Points in 3D (x, y, z), drawn as dots by a Scanline Render: a Camera Tracker's solved points.",
    },
    OpInfo {
        name: "locator",
        label: "Locator",
        category: Category::Scene,
        inputs: &[],
        params: &[p!("translate", "Translate", ParamKind::Vec3, [0.0, 0.0, 0.0])],
        overlay: Overlay::None,
        mask_base: None,
        help: "A point in 3D, drawn as a dot: wire it into a Scene, or link other nodes' params to its Translate.",
    },
    OpInfo {
        name: "scanline_render",
        label: "Scanline Render",
        category: Category::Scene,
        inputs: &[scene_in("scene"), scene_in("camera"), opt("background")],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            fixed!("samples", "Anti-aliasing", ParamKind::Int { min: 1, max: 8 }, 3),
            fixed!("passes", "Passes", ParamKind::Flags(&render3d::Passes::NAMES), ["depth"]),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
            fixed!("jitter", "Jitter shutter", ParamKind::Bool, true),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Draws a scene's cards and meshes (and points, as dots) through a camera: the one on the camera input, \
               else the scene's first. Over the background, in its frame, else Width × Height (0: the project's). \
               Anti-aliasing takes that many samples across each pixel and down (3: nine). \
               Passes adds planes next to the picture: depth (each pixel's distance in front of the camera, 0 where \
               nothing is), P (scene position), N (normal), uv (texture coordinates), motion (pixels each pixel moves by \
               the next frame), id (an object number per \
               pixel), mattes (matte.<node>: each object's coverage), the light in parts adding up to the picture \
               (diffuse, specular, emission, ambient), lights (light.<node>: each light's share) and shadow (the light \
               shadows took away: the picture plus it has no shadows). The scene's lights light Lambert and Blinn objects; \
               with none, everything renders unlit. Lights with Cast shadows on are blocked by objects that cast shadows, \
               on objects that receive them. Holdout objects draw \
               nothing but cut out what is behind them. Motion blur renders that many times across the shutter \
               (Shutter frames long, centered on the frame, starting or ending at it; 0 or 1: off) and averages them: \
               cameras, objects and lights move between their places at the frames round each time, particles along \
               their velocities. Jitter shutter spreads each pixel's times a little within the shutter, so a fast move \
               smears into fine grain rather than a few sharp copies; off, every pixel renders at the same times (as \
               before 0.9.55). The picture, mattes and light passes blur; depth, P, N, uv, motion and id are the frame's own.",
    },
    OpInfo {
        name: "lens_distort",
        label: "Lens Distort",
        category: Category::Transform,
        inputs: &[IN, MASK],
        params: &[
            fixed!("mode", "Mode", ParamKind::Choice(&["undistort", "distort"]), "undistort"),
            p!("k1", "K1", ParamKind::Float { min: -1.0, max: 1.0 }, 0.0),
            p!("k2", "K2", ParamKind::Float { min: -1.0, max: 1.0 }, 0.0),
            // Null: the frame's center.
            p!("center", "Center", ParamKind::Vec2, Value::Null),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Removes or adds a lens's radial distortion (Brown model: a point r from the center, \
               in half frame widths, moves to r × (1 + K1 r² + K2 r⁴)). Negative K1 is barrel \
               distortion (wide lenses), positive pincushion. Undistort straightens a plate's lines \
               (for tracking or adding CG); Distort, with the same values, bends CG to match the plate.",
    },
    OpInfo {
        name: "rectangle",
        label: "Rectangle",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            p!("center", "Center", ParamKind::Vec2, [256.0, 256.0]),
            p!("size", "Size", ParamKind::Vec2, [256.0, 256.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
        ],
        overlay: Overlay::Box { center: "center", size: "size", rotate: "rotate", ellipse: false },
        mask_base: None,
        help: shape_help!("A filled, optionally rotated rectangle."),
    },
    OpInfo {
        name: "ellipse",
        label: "Ellipse",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            p!("center", "Center", ParamKind::Vec2, [256.0, 256.0]),
            p!("size", "Size", ParamKind::Vec2, [256.0, 256.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
        ],
        overlay: Overlay::Box { center: "center", size: "size", rotate: "rotate", ellipse: true },
        mask_base: None,
        help: shape_help!("A filled, optionally rotated ellipse."),
    },
    OpInfo {
        name: "polygon",
        label: "Polygon",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            p!("points", "Points", ParamKind::Points { stride: 2 }, [156.0, 356.0, 256.0, 156.0, 356.0, 356.0]),
            fixed!("fill_rule", "Fill rule", ParamKind::Choice(FILL_RULES), "non-zero"),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
        ],
        overlay: Overlay::Points("points"),
        mask_base: None,
        help: shape_help!("A filled polygon. In the viewer, drag inside it to move it all, drag points \
               or edges to reshape it, click an edge to add a point, Ctrl+click to add after the \
               selected point, Shift+click to select several, ⌫ to delete."),
    },
    OpInfo {
        name: "roto",
        label: "Roto",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", FRAME_SIZE, 0),
            p!("height", "Height", FRAME_SIZE, 0),
            // A circle: per vertex x, y, in-handle and out-handle offsets.
            p!("points", "Points", ParamKind::Points { stride: 6 }, [
                256.0, 136.0, -66.3, 0.0, 66.3, 0.0,
                376.0, 256.0, 0.0, -66.3, 0.0, 66.3,
                256.0, 376.0, 66.3, 0.0, -66.3, 0.0,
                136.0, 256.0, 0.0, 66.3, 0.0, -66.3,
            ]),
            fixed!("fill_rule", "Fill rule", ParamKind::Choice(FILL_RULES), "non-zero"),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
        ],
        overlay: Overlay::Points("points"),
        mask_base: None,
        help: shape_help!("A closed bezier shape for rotoscoping; chain Roto nodes through the \
               background for several shapes or holes. In the viewer, drag inside it to move it \
               all, drag points or the curve between them to reshape it, click the curve to add a point, Ctrl+click to add after the selected point (drag to pull out \
               its tangents), Shift+click to select several, ⌫ to delete. Selected points show \
               their tangent handles: dragging one keeps the curve smooth, Alt breaks it; \
               Alt+drag a point to pull new tangents out of it."),
    },
    OpInfo {
        name: "tracker",
        label: "Tracker",
        category: Category::Transform,
        inputs: &[IN],
        params: &[
            fixed!("track1_on", "Track 1", ParamKind::Bool, true),
            p!("track1", "Position 1", ParamKind::Vec2, [256.0, 256.0]),
            fixed!("track2_on", "Track 2", ParamKind::Bool, false),
            p!("track2", "Position 2", ParamKind::Vec2, [356.0, 256.0]),
            fixed!("track3_on", "Track 3", ParamKind::Bool, false),
            p!("track3", "Position 3", ParamKind::Vec2, [256.0, 356.0]),
            fixed!("track4_on", "Track 4", ParamKind::Bool, false),
            p!("track4", "Position 4", ParamKind::Vec2, [356.0, 356.0]),
            fixed!("pattern", "Pattern size", ParamKind::Int { min: 5, max: 255 }, 31),
            fixed!("search", "Search size", ParamKind::Int { min: 7, max: 511 }, 91),
            fixed!("min_score", "Min. correlation", ParamKind::Float { min: 0.0, max: 1.0 }, 0.7),
            fixed!("update", "Pattern from", ParamKind::Choice(&["first frame", "every frame"]), "first frame"),
            fixed!("motion", "Track motion", ParamKind::Choice(SOLVES), "translate, rotate"),
            fixed!("output", "Output", ParamKind::Choice(&["input", "match-move", "stabilize"]), "input"),
            fixed!("solve", "Solve", ParamKind::Choice(SOLVES), "translate, rotate, scale"),
            fixed!("reference_frame", "Reference frame", FRAME, 1),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
        ],
        overlay: Overlay::Tracker { pattern: "pattern", search: "search" },
        mask_base: None,
        help: "Tracks up to four points through the shot. Place each track's box on a \
               distinct feature in the viewer, then Track forward or backward: each tracked \
               frame keys the track's position (correct bad frames by dragging the box). \
               Track motion sets what the features may do besides move: turn, or turn and \
               change size (needed for features that rotate, or zoom, through the shot). \
               A track stops when its best match falls below Min. correlation. Output: the \
               input unchanged, or the solved motion relative to the reference frame applied \
               to it (match-move) or removed from it (stabilize). One track solves translation, \
               and also rotation and scale when Track motion followed them; two or more \
               solve them from the tracks' positions. Motion blur (samples per frame, 0 for \
               off) averages the match-move or stabilize over the shutter.",
    },
    OpInfo {
        name: "corner_pin",
        label: "Corner Pin",
        category: Category::Transform,
        inputs: &[IN, MASK],
        params: &[
            p!("to1", "To 1", ParamKind::Vec2, [0.0, 0.0]),
            p!("to2", "To 2", ParamKind::Vec2, [512.0, 0.0]),
            p!("to3", "To 3", ParamKind::Vec2, [512.0, 512.0]),
            p!("to4", "To 4", ParamKind::Vec2, [0.0, 512.0]),
            p!("from1", "From 1", ParamKind::Vec2, [0.0, 0.0]),
            p!("from2", "From 2", ParamKind::Vec2, [512.0, 0.0]),
            p!("from3", "From 3", ParamKind::Vec2, [512.0, 512.0]),
            p!("from4", "From 4", ParamKind::Vec2, [0.0, 512.0]),
            fixed!("motion_blur", "Motion blur", MOTION_SAMPLES, 0),
            fixed!("shutter", "Shutter", SHUTTER, 0.5),
            fixed!("shutter_offset", "Shutter offset", ParamKind::Choice(SHUTTER_OFFSETS), "centered"),
            MIX, MASK_CHANNEL, MASK_INVERT,
        ],
        overlay: Overlay::CornerPin { to: ["to1", "to2", "to3", "to4"], from: ["from1", "from2", "from3", "from4"] },
        mask_base: Some(0),
        help: "Warps the image in perspective so the four From points land on the four To \
               points. In the viewer, drag the To corners (solid) or the From corners (dashed). \
               A Tracker with four tracks exports one (To Corner Pin) to pin a layer onto a \
               moving plane, or to stabilize it. \
               Motion blur (samples per frame, 0 for off) averages the animated warp over the shutter.",
    },
    OpInfo {
        name: "media_out",
        label: "Media Out",
        category: Category::Output,
        inputs: &[IN],
        params: &[
            p!("path", "File", ParamKind::Path { save: true }, ""),
            fixed!("first_frame", "First frame", FRAME, 1),
            fixed!("last_frame", "Last frame", FRAME, 100),
            fixed!("fps", "FPS (video)", ParamKind::Float { min: 1.0, max: 240.0 }, 24.0),
            fixed!("codec", "Codec (video)", ParamKind::Choice(crate::media::VIDEO_CODECS), "auto"),
            fixed!("dpx", "DPX encoding", ParamKind::Choice(crate::dpx::ENCODINGS), "10-bit log"),
            fixed!("dpx_alpha", "DPX alpha", ParamKind::Bool, false),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Renders a frame range to an image sequence (shot.####.exr) or a \
               video. Codec: auto (.mov = ProRes 422 HQ, others H.264), H.264 8-bit, \
               HEVC 10-bit, ProRes 422 HQ 10-bit, or ProRes 4444 with alpha (ProRes \
               needs .mov or .mkv). The 10-bit codecs get the full 10 bits. DPX \
               encoding (for .dpx sequences): 10-bit log (keeps values above 1.0), \
               10/16-bit video (sRGB-coded, full range, marked Rec. 709) or 32-bit \
               float linear (exact); DPX alpha stores alpha too (straight).",
    },
    OpInfo {
        name: "viewer",
        label: "Viewer",
        category: Category::Output,
        inputs: &[IN],
        params: &[],
        overlay: Overlay::None,
        mask_base: None,
        help: "Shows its input in the viewer panel.",
    },
    OpInfo {
        name: "write",
        label: "Write (still)",
        category: Category::Output,
        inputs: &[IN],
        params: &[
            p!("path", "File", ParamKind::Path { save: true }, ""),
            fixed!("dpx", "DPX encoding", ParamKind::Choice(crate::dpx::ENCODINGS), "10-bit log"),
            fixed!("dpx_alpha", "DPX alpha", ParamKind::Bool, false),
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Writes its input when the graph is rendered from the command line. \
               DPX encoding (for .dpx files): 10-bit log, 10/16-bit video or 32-bit float \
               linear; DPX alpha stores alpha too (straight).",
    },
];

impl ParamSpec {
    pub fn default_value(&self) -> Value {
        match self.default {
            DefaultValue::Fn(f) => f(),
            DefaultValue::Static(v) => v.clone(),
        }
    }
}

/// A built-in op or one registered by a loaded plugin.
pub fn info(op: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|o| o.name == op).or_else(|| particle_nodes::info(op)).or_else(|| crate::plugin::info(op))
}

/// Every op: the built-ins, the particle nodes, then plugin ops.
pub fn all() -> Vec<&'static OpInfo> {
    OPS.iter().chain(particle_nodes::OPS).chain(crate::plugin::infos()).collect()
}

/// `params` with every missing key filled in from the op's declared defaults.
pub fn params_with_defaults(op: &str, params: &Value) -> Value {
    let mut obj = match params {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    if let Some(info) = info(op) {
        for spec in info.params {
            obj.entry(spec.name).or_insert_with(|| spec.default_value());
        }
    }
    Value::Object(obj)
}

/// `params` with every keyframed value replaced by its value at `frame`.
/// Integer params are rounded so they still deserialize as integers.
pub fn resolve_params(op: &str, params: &Value, frame: i64) -> Value {
    resolve_params_linked(op, params, frame, None)
}

/// Resolves [`crate::link`]s too, with `links` (the graph's resolver: a
/// value and a time to the value there), at the frame and at each motion
/// blur sample.
pub type Links<'a> = Option<&'a dyn Fn(&Value, f64) -> Value>;

/// [`resolve_params`], following live links through `links`.
pub fn resolve_params_linked(op: &str, params: &Value, frame: i64, links: Links) -> Value {
    let Value::Object(map) = params else { return params.clone() };
    if !map.values().any(|v| anim::is_animated(v) || crate::link::is_link(v)) {
        let mut out = params.clone();
        if op == "tracker" {
            add_reference_tracks(params, &mut out);
        }
        return out;
    }
    let mut resolved = resolve_at(op, map, frame as f64, links);
    if op == "tracker" {
        add_reference_tracks(params, &mut resolved);
    }
    if MOTION_BLUR_OPS.contains(&op) {
        add_motion_samples(op, params, frame, &mut resolved, links);
    }
    resolved
}

/// `map`'s values at time `t` (a frame, or between frames).
fn resolve_at(op: &str, map: &Map<String, Value>, t: f64, links: Links) -> Value {
    let specs = info(op).map(|i| i.params).unwrap_or(&[]);
    let resolved = map
        .iter()
        .map(|(k, v)| {
            let mut v = match links {
                Some(follow) if crate::link::is_link(v) => follow(v, t),
                _ => anim::evaluate(v, t),
            };
            // A link giving one number for a list param: every component.
            if let (Some(x), true) = (v.as_f64(), crate::link::is_link(&map[k])) {
                if let Some(Value::Array(d)) = specs.iter().find(|s| s.name == k).map(ParamSpec::default_value) {
                    v = json!(vec![x; d.len()]);
                }
            }
            if specs.iter().any(|s| s.name == k && matches!(s.kind, ParamKind::Int { .. })) {
                if let Some(x) = v.as_f64() {
                    v = json!(x.round() as i64);
                }
            }
            (k.clone(), v)
        })
        .collect();
    Value::Object(resolved)
}

/// For an animated shape or warp with motion blur on, its params at each
/// sample time across the shutter, as `_motion` (also in the cache key).
fn add_motion_samples(op: &str, params: &Value, frame: i64, out: &mut Value, links: Links) {
    let Value::Object(map) = params else { return };
    let n = out.get("motion_blur").and_then(Value::as_i64).unwrap_or(0);
    let shutter = out.get("shutter").and_then(Value::as_f64).unwrap_or(0.5);
    // A Tracker passing its input through has nothing to blur.
    let still = op == "tracker" && out.get("output").and_then(Value::as_str).is_none_or(|o| o == "input");
    if n < 2 || shutter <= 0.0 || still {
        return;
    }
    let start = match out.get("shutter_offset").and_then(Value::as_str) {
        Some("start") => 0.0,
        Some("end") => -shutter,
        _ => -shutter / 2.0,
    };
    let samples: Vec<Value> = (0..n)
        .map(|i| {
            let mut p = resolve_at(op, map, frame as f64 + start + shutter * i as f64 / (n - 1) as f64, links);
            if op == "tracker" {
                add_reference_tracks(params, &mut p);
            }
            p
        })
        .collect();
    out["_motion"] = Value::Array(samples);
}

/// Give a Tracker's resolved params its tracks' positions at the reference
/// frame too (`ref1` …), which its output transform is relative to.
fn add_reference_tracks(params: &Value, resolved: &mut Value) {
    let reference = params.get("reference_frame").and_then(Value::as_i64).unwrap_or(1);
    for (i, p) in track::positions(params, reference).into_iter().enumerate() {
        resolved[format!("ref{}", i + 1)] = json!(p);
    }
    for (i, w) in track::warps(params, reference).into_iter().enumerate() {
        if let Some(w) = w {
            resolved[format!("refwarp{}", i + 1)] = json!(w.to_param());
        }
    }
}

/// True when the op's output can differ from frame to frame on its own,
/// independent of its inputs.
pub fn is_time_varying(op: &str, params: &Value) -> bool {
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    match op {
        "media_in" => true,
        "read" => media::is_sequence_pattern(path) || media::is_video(Path::new(path)),
        "write" => media::is_sequence_pattern(path),
        _ => crate::plugin::is_time_varying(op),
    }
}

/// Deserialize an op's `params` into a typed struct, after filling defaults.
pub fn parse<T: for<'de> Deserialize<'de>>(params: &Value, op: &str) -> Result<T> {
    serde_json::from_value(params_with_defaults(op, params))
        .with_context(|| format!("invalid params for op '{op}'"))
}

/// Apply a per-pixel closure across an image in parallel.
fn map_pixels(img: &mut Image, f: impl Fn(Rgba) -> Rgba + Sync + Send) {
    img.pixels.par_iter_mut().for_each(|p| *p = f(*p));
}

/// The mean of same-format images (premultiplied, so edges blend right),
/// over the union of their windows.
fn average(images: &[Image]) -> Image {
    let window = images.iter().fold(IRect::default(), |u, img| u.union(&img.window()));
    let w = 1.0 / images.len().max(1) as f32;
    generate(images[0].format, window, |x, y| {
        let mut sum = [0.0; 4];
        for img in images {
            for (s, v) in sum.iter_mut().zip(img.at(x, y)) {
                *s += v * w;
            }
        }
        sum
    })
}

/// A copy of `src` with `f` applied to every pixel (see [`pointwise_window`]
/// for where).
fn mapped(src: &Image, f: impl Fn(Rgba) -> Rgba + Sync + Send) -> Image {
    let window = pointwise_window(src.format, &[src.window()], f(color::TRANSPARENT));
    if window != src.window() {
        return generate(src.format, window, |x, y| f(src.at(x, y)));
    }
    let mut out = src.clone();
    map_pixels(&mut out, f);
    out
}

/// The data window of an op whose output pixel depends only on its inputs'
/// pixels at the same place: their windows' union, grown to the whole frame
/// when the op turns transparent inputs (`blank`, its output there) into
/// something else.
fn pointwise_window(format: Size, windows: &[IRect], blank: Rgba) -> IRect {
    let w = windows.iter().fold(IRect::default(), |u, w| u.union(w));
    if blank == color::TRANSPARENT { w } else { w.union(&IRect::frame(format)) }
}

/// How far past the frame a moved image keeps its pixels: one frame size on
/// each side, so a later move can bring them back without a huge scale
/// allocating gigabytes.
fn overscan(format: Size) -> IRect {
    let (w, h) = (format.width as i32, format.height as i32);
    IRect::new(-w, -h, 2 * w, 2 * h)
}

/// Where `window` lands under `f` (a point map in frame pixels, pixel
/// centers on integers), limited to the overscan; the whole overscan when
/// a corner can't be mapped. Bilinear sampling reads pixels `x0..x1` for
/// positions in (x0 − 1, x1), so that span's image is what's covered.
fn moved_window(window: IRect, format: Size, f: impl Fn([f32; 2]) -> Option<[f32; 2]>) -> IRect {
    if window.is_empty() {
        return window;
    }
    let (x0, y0, x1, y1) = (window.x0 as f32 - 1.0, window.y0 as f32 - 1.0, window.x1 as f32, window.y1 as f32);
    let corners: Option<Vec<[f32; 2]>> = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].into_iter().map(&f).collect();
    let Some(c) = corners.filter(|c| c.iter().flatten().all(|v| v.is_finite())) else { return overscan(format) };
    // Rounded out by one on the low side: float error near a whole number.
    let lo = |k: usize| c.iter().map(|p| p[k]).fold(f32::INFINITY, f32::min).floor().max(-1e8) as i32;
    let hi = |k: usize| c.iter().map(|p| p[k]).fold(f32::NEG_INFINITY, f32::max).ceil().min(1e8) as i32;
    IRect::new(lo(0), lo(1), hi(0), hi(1)).intersect(&overscan(format))
}

/// An image of frame `format` holding `window`, each pixel `f(x, y)` at its
/// frame position.
fn generate(format: Size, window: IRect, f: impl Fn(i32, i32) -> Rgba + Sync + Send) -> Image {
    let mut out = Image::blank(format, window);
    let w = window.width().max(1);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        *px = f(window.x0 + (i % w) as i32, window.y0 + (i / w) as i32);
    });
    out
}

/// Evaluate one operator. `inputs` are the already-rendered upstream images,
/// one per port; an unconnected optional port is `None`.
pub fn eval(op: &str, params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Result<Image> {
    // A Merge with Swap A / B on: its A and B trade places for everything
    // below, the mask's base and the frame included.
    let swapped: Vec<Option<&Image>>;
    let inputs = if op == "merge" && params.get("swap").and_then(Value::as_bool) == Some(true) && inputs.len() >= 2 {
        swapped = [inputs[1], inputs[0]].into_iter().chain(inputs[2..].iter().copied()).collect();
        &swapped[..]
    } else {
        inputs
    };
    let out = match params.get("_motion").and_then(Value::as_array) {
        // A warp with motion blur: its output at each sample, averaged.
        Some(samples) if matches!(op, "transform" | "corner_pin" | "tracker") => {
            let outs = samples.iter().map(|p| eval_op(op, p, inputs, ctx)).collect::<Result<Vec<_>>>()?;
            // Planes (depth) can't be averaged: the middle sample's.
            let mut out = average(&outs);
            out.keep_planes(&outs[outs.len() / 2]);
            out
        }
        _ => eval_op(op, params, inputs, ctx)?,
    };
    let mut out = out;
    if let Some(src) = planes_source(op).and_then(|n| inputs.get(n).copied().flatten()) {
        if out.planes.is_empty() && out.format == src.format {
            out.keep_planes(src);
        }
    }
    match info(op).and_then(|i| i.mask_base.map(|b| (b, i.inputs.len() - 1))) {
        Some((base, mask)) => {
            let mix = params.get("mix").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let channel = params.get("mask_channel").and_then(Value::as_str).unwrap_or("alpha");
            let invert = params.get("mask_invert").and_then(Value::as_bool).unwrap_or(false);
            let mask = inputs.get(mask).copied().flatten().map(|m| Mask { image: m, channel, invert });
            Ok(mask_mix(input(inputs, base, op)?, out, mask, mix))
        }
        None => Ok(out),
    }
}

/// The input whose extra planes (depth, ...) an op keeps where they are: ops
/// that change color but don't move pixels. Ops that move pixels carry
/// planes themselves; others (plugins, sources) drop them.
fn planes_source(op: &str) -> Option<usize> {
    match op {
        "grade" | "exposure" | "saturation" | "invert" | "clamp" | "premult" | "unpremult" | "channel_select" | "keyer" | "blur" => Some(0),
        "rectangle" | "ellipse" | "polygon" | "roto" => Some(0),
        "keymix" => Some(1),
        _ => None,
    }
}

/// A mask image and how to read it.
struct Mask<'a> {
    image: &'a Image,
    /// One of [`MASK_CHANNELS`].
    channel: &'a str,
    invert: bool,
}

/// How much `mask` lets through at (x, y), 0–1: its channel's value
/// (luminance with Rec. 709 weights), flipped when inverted. Outside the
/// mask's extent it counts as empty (so inverted, full).
fn mask_value(mask: &Mask, x: i32, y: i32) -> f32 {
    let px = mask.image.at(x, y);
    let v = match mask.channel {
        "red" => px[0],
        "green" => px[1],
        "blue" => px[2],
        "luminance" => px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2],
        _ => px[3],
    }
    .clamp(0.0, 1.0);
    if mask.invert { 1.0 - v } else { v }
}

/// How much `mask` covers at (x, y), 0–1: its `channel` (one of
/// [`MASK_CHANNELS`]), flipped when `invert`, as a masked op reads it.
pub fn mask_coverage(mask: &Image, channel: &str, invert: bool, x: i32, y: i32) -> f32 {
    mask_value(&Mask { image: mask, channel, invert }, x, y)
}

/// `result` blended back toward `base` by `mix × mask` ([`mask_value`]).
/// With no mask connected it counts as full (Invert mask then does nothing).
fn mask_mix(base: &Image, result: Image, mask: Option<Mask>, mix: f32) -> Image {
    let mix = mix.clamp(0.0, 1.0);
    if mask.is_none() && mix >= 1.0 {
        return result;
    }
    // Where the mask holds back the result, the base shows: both windows.
    let window = result.window().union(&base.window());
    let mut out = generate(result.format, window, |x, y| {
        let t = mix * mask.as_ref().map_or(1.0, |m| mask_value(m, x, y));
        let (b, r) = (base.at(x, y), result.at(x, y));
        std::array::from_fn(|c| b[c] + (r[c] - b[c]) * t)
    });
    out.keep_planes(if result.planes.is_empty() { base } else { &result });
    out
}

fn eval_op(op: &str, params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Result<Image> {
    match op {
        "media_in" => media_in(params, ctx),
        "constant" => constant(params),
        "checkerboard" => checkerboard(params),
        "read" => read(params, ctx),
        "grade" => grade(params, inputs),
        "merge" => merge(params, inputs),
        "transform" => transform(params, inputs),
        "blur" => blur(params, inputs),
        "exposure" => exposure(params, inputs),
        "light_2d" => light_2d(params, inputs),
        "saturation" => saturation(params, inputs),
        "invert" => invert(params, inputs),
        "clamp" => clamp(params, inputs),
        "premult" => Ok(mapped(input(inputs, 0, op)?, color::premultiply)),
        "unpremult" => Ok(mapped(input(inputs, 0, op)?, color::unpremultiply)),
        "channel_select" => channel_select(params, inputs),
        "shuffle" => shuffle(params, inputs),
        "switch" => switch(params, inputs),
        "dissolve" => dissolve(params, inputs),
        "keymix" => keymix(inputs),
        "keyer" => keyer(params, inputs),
        "crop" => crop(params, inputs),
        "resize" => resize(params, inputs),
        "lens_distort" => lens_distort(params, inputs),
        "camera" => Ok(scene_output(DEFAULT_FORMAT, Scene { cameras: vec![scene::camera(params)], ..Scene::default() })),
        "card" => {
            let img = input(inputs, 0, "card")?;
            Ok(scene_output(img.format, Scene { cards: vec![scene::card(params, img)], ..Scene::default() }))
        }
        "scene" => gather(inputs),
        "cube" | "sphere" | "cylinder" | "plane" => {
            let texture = inputs.first().copied().flatten();
            let model = scene::primitive_model(op, params, texture).context("unknown primitive")?;
            Ok(scene_output(texture.map_or(DEFAULT_FORMAT, |t| t.format), Scene { meshes: vec![model], ..Scene::default() }))
        }
        "read_geo" => read_geo(params, ctx.frame),
        "read_splat" => read_splat(params),
        "splat_crop" => splat_crop(params, inputs),
        "splat_color" => splat_color(params, inputs),
        "light_ambient" | "light_directional" | "light_point" | "light_spot" | "light_area" => {
            let target = inputs.first().copied().flatten().and_then(|t| t.scene.as_deref());
            let light = scene::light(op, params, target).context("unknown light")?;
            Ok(scene_output(DEFAULT_FORMAT, Scene { lights: vec![light], ..Scene::default() }))
        }
        "scanline_render" => scanline_render(params, inputs),
        _ if particle_nodes::is_particle(op) => particle_node(op, params, inputs, ctx),
        // The solve lives in its params; in the graph the plate passes through.
        "camera_tracker" => Ok(input(inputs, 0, "camera_tracker")?.clone()),
        "point_cloud" => {
            let flat: Vec<f64> = params["points"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(Value::as_f64).collect());
            let points = flat.as_chunks::<3>().0.iter().map(|c| geom::Vec3::from(*c)).collect();
            Ok(scene_output(DEFAULT_FORMAT, Scene { points, ..Scene::default() }))
        }
        "locator" => Ok(scene_output(DEFAULT_FORMAT, Scene { points: vec![geom::Vec3::from(scene::numbers::<3>(&params["translate"], 0.0))], ..Scene::default() })),
        "tracker" => tracker(params, inputs),
        "corner_pin" => corner_pin(params, inputs),
        "rectangle" | "ellipse" | "polygon" | "roto" => shape_op(op, params, inputs),
        "write" => write(params, inputs, ctx),
        // Rendering to disk is driven by `render`; in the graph these pass through.
        "media_out" | "viewer" => Ok(input(inputs, 0, op)?.clone()),
        other => match crate::plugin::eval(other, params, inputs, ctx) {
            Some(result) => result,
            None => bail!("unknown op '{other}'"),
        },
    }
}

/// Required input `n`.
fn input<'a>(inputs: &[Option<&'a Image>], n: usize, op: &str) -> Result<&'a Image> {
    inputs.get(n).copied().flatten().ok_or_else(|| {
        let port = info(op).and_then(|i| i.inputs.get(n)).map_or("input", |p| p.label);
        anyhow!("input '{port}' is not connected")
    })
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MediaIn {
    path: String,
    first_frame: i64,
    last_frame: i64,
    fit: String,
    /// Set by [`apply_format`] when fitting.
    #[serde(default)]
    project_format: Option<[usize; 2]>,
}

fn media_in(params: &Value, ctx: &EvalContext) -> Result<Image> {
    let p: MediaIn = parse(params, "media_in")?;
    fit_to_project(read_media(&p, ctx)?, &p.fit, p.project_format)
}

/// What a Media In reads at `ctx`'s frame, at its own size (shared with
/// the decoder for a video).
fn read_media(p: &MediaIn, ctx: &EvalContext) -> Result<Arc<Image>> {
    if p.path.is_empty() {
        bail!("no file selected");
    }
    let last = p.last_frame.max(p.first_frame);
    let frame = ctx.frame.clamp(p.first_frame, last);
    let path = Path::new(&p.path);
    if media::is_video(path) {
        media::read_video_frame(path, (frame - p.first_frame) as usize)
    } else {
        Ok(Arc::new(Image::read(&media::existing_frame_path(&p.path, frame, p.first_frame, last))?))
    }
}

#[derive(Deserialize)]
struct Constant {
    width: usize,
    height: usize,
    /// Straight (unassociated) RGBA; stored premultiplied.
    color: [f32; 4],
}

fn constant(params: &Value) -> Result<Image> {
    let p: Constant = parse(params, "constant")?;
    let f = source_size(p.width, p.height);
    Ok(Image::filled(f.width, f.height, color::premultiply(p.color)))
}

#[derive(Deserialize)]
struct Checkerboard {
    width: usize,
    height: usize,
    size: usize,
    color_a: [f32; 4],
    color_b: [f32; 4],
}

fn checkerboard(params: &Value) -> Result<Image> {
    let p: Checkerboard = parse(params, "checkerboard")?;
    let size = p.size.max(1);
    let a = color::premultiply(p.color_a);
    let b = color::premultiply(p.color_b);
    let f = source_size(p.width, p.height);
    let mut img = Image::new(f.width, f.height);
    let w = f.width;
    img.pixels
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, px)| {
            let x = i % w;
            let y = i / w;
            let cell = (x / size + y / size) % 2;
            *px = if cell == 0 { a } else { b };
        });
    Ok(img)
}

#[derive(Deserialize)]
struct Read {
    path: String,
    fit: String,
    #[serde(default)]
    project_format: Option<[usize; 2]>,
}

fn read(params: &Value, ctx: &EvalContext) -> Result<Image> {
    let p: Read = parse(params, "read")?;
    if p.path.is_empty() {
        bail!("no file selected");
    }
    let path = Path::new(&p.path);
    let img = if media::is_video(path) {
        // The clip's own range from frame 1, as a new Media In reads it.
        media::read_video_frame(path, (ctx.frame - 1).max(0) as usize)?
    } else {
        Arc::new(Image::read(&media::frame_path(&p.path, ctx.frame))?)
    };
    fit_to_project(img, &p.fit, p.project_format)
}

/// `img` scaled to the project's `format` (None outside a graph: the
/// default format) as `fit` says (see [`PROJECT_FITS`]), centered; as it
/// is for none, or when it already has that size. Fitting reads `img` in
/// place (a 4K video frame isn't copied first).
fn fit_to_project(img: Arc<Image>, fit: &str, format: Option<[usize; 2]>) -> Result<Image> {
    if fit == "none" {
        return Ok(Arc::unwrap_or_clone(img));
    }
    let [w, h] = format.unwrap_or([DEFAULT_FORMAT.width, DEFAULT_FORMAT.height]).map(|v| v.max(1));
    let (sx, sy) = (w as f32 / img.format.width.max(1) as f32, h as f32 / img.format.height.max(1) as f32);
    let (sx, sy) = match fit {
        "fit" => (sx.min(sy), sx.min(sy)),
        "fill" => (sx.max(sy), sx.max(sy)),
        "stretch" => (sx, sy),
        "width" => (sx, sx),
        "height" => (sy, sy),
        other => bail!("unknown fit '{other}' (one of: {})", PROJECT_FITS.join(", ")),
    };
    if img.format == (Size { width: w, height: h }) && sx == 1.0 && sy == 1.0 {
        return Ok(Arc::unwrap_or_clone(img));
    }
    // Cut to the frame: what fill crops is gone, and so is the filter's
    // spill past the edges.
    let fitted = resample(&img, Size { width: w, height: h }, sx, sy, true);
    let window = fitted.window().intersect(&fitted.frame());
    if window == fitted.window() {
        return Ok(fitted);
    }
    let mut out = generate(fitted.format, window, |x, y| fitted.at(x, y));
    out.carry_planes(&fitted, |x, y| Some([x as f32, y as f32]));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Grade {
    /// Per-channel multiply (gain).
    multiply: [f32; 3],
    /// Per-channel additive offset, applied after multiply.
    offset: [f32; 3],
    /// Per-channel gamma (output = value^(1/gamma)).
    gamma: [f32; 3],
}

/// Generic lift/gain/gamma color correction. Operates on unassociated color so
/// the correction is independent of coverage, then re-premultiplies.
fn grade(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Grade = parse(params, "grade")?;
    let src = input(inputs, 0, "grade")?;
    Ok(mapped(src, |px| {
        let s = color::unpremultiply(px);
        let mut rgb = [0.0f32; 3];
        for c in 0..3 {
            let mut v = s[c] * p.multiply[c] + p.offset[c];
            if p.gamma[c] != 1.0 {
                v = v.max(0.0).powf(1.0 / p.gamma[c].max(1e-6));
            }
            rgb[c] = v;
        }
        color::premultiply([rgb[0], rgb[1], rgb[2], s[3]])
    }))
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Merge {
    operation: String,
}

/// Composite input[0] (foreground / A) over input[1] (background / B).
/// Formulas are the standard premultiplied Porter-Duff / blend operators.
fn merge(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Merge = parse(params, "merge")?;
    let fg = input(inputs, 0, "merge")?;
    let bg = input(inputs, 1, "merge")?;
    if p.operation == "zmerge" {
        // By depth, passes and all, in B's frame.
        let mut out = Image::zmerge(fg, bg);
        out.format = bg.format;
        return Ok(out);
    }

    // B's frame, holding both inputs' data.
    let op = p.operation.as_str();
    let blank = blend(op, color::TRANSPARENT, color::TRANSPARENT)?;
    let window = pointwise_window(bg.format, &[fg.window(), bg.window()], blank);
    let mut out = generate(bg.format, window, |x, y| blend(op, fg.at(x, y), bg.at(x, y)).unwrap_or(blank));
    merge_planes(&mut out, fg, bg, op == "under");
    Ok(out)
}

/// A merge's extra planes (depth, ...): the front input's where it is more
/// than half opaque, the back's elsewhere (A is in front, B with `under`).
/// A plane only one input has counts as 0 in the other.
fn merge_planes(out: &mut Image, fg: &Image, bg: &Image, under: bool) {
    let (front, back) = if under { (bg, fg) } else { (fg, bg) };
    let mut names: Vec<&str> = back.planes.iter().map(|p| p.name.as_str()).collect();
    names.extend(front.planes.iter().map(|p| p.name.as_str()).filter(|n| back.plane(n).is_none()));
    let (w, x0, y0) = (out.width.max(1), out.x0, out.y0);
    out.planes = names
        .into_iter()
        .map(|name| {
            let mut data = vec![0.0; out.width * out.height];
            data.par_iter_mut().enumerate().for_each(|(i, v)| {
                let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
                let src = if front.at(x, y)[3] > 0.5 { front } else { back };
                *v = src.plane_at(name, x, y);
            });
            crate::image::Plane { name: name.to_string(), data }
        })
        .collect();
}

#[inline]
fn blend(op: &str, f: Rgba, b: Rgba) -> Result<Rgba> {
    let fa = f[3];
    let ba = b[3];
    let mk = |rgb: [f32; 3], a: f32| [rgb[0], rgb[1], rgb[2], a];
    let out = match op {
        // A over B
        "over" => {
            let ia = 1.0 - fa;
            mk(
                [
                    f[0] + b[0] * ia,
                    f[1] + b[1] * ia,
                    f[2] + b[2] * ia,
                ],
                fa + ba * ia,
            )
        }
        // B over A
        "under" => {
            let ia = 1.0 - ba;
            mk(
                [
                    b[0] + f[0] * ia,
                    b[1] + f[1] * ia,
                    b[2] + f[2] * ia,
                ],
                ba + fa * ia,
            )
        }
        "add" => mk([f[0] + b[0], f[1] + b[1], f[2] + b[2]], (fa + ba).min(1.0)),
        "multiply" => mk([f[0] * b[0], f[1] * b[1], f[2] * b[2]], fa * ba),
        "screen" => mk(
            [
                f[0] + b[0] - f[0] * b[0],
                f[1] + b[1] - f[1] * b[1],
                f[2] + b[2] - f[2] * b[2],
            ],
            fa + ba - fa * ba,
        ),
        "max" => mk(
            [f[0].max(b[0]), f[1].max(b[1]), f[2].max(b[2])],
            fa.max(ba),
        ),
        "min" => mk(
            [f[0].min(b[0]), f[1].min(b[1]), f[2].min(b[2])],
            fa.min(ba),
        ),
        other => bail!("unknown merge operation '{other}'"),
    };
    Ok(out)
}

// ---------------------------------------------------------------------------
// Transform
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Transform {
    translate: [f32; 2],
    scale: [f32; 2],
    /// Rotation in degrees about `center`; positive turns clockwise on
    /// screen (image y points down).
    rotate: f32,
    /// The pivot; None for the middle of the input's data window.
    #[serde(default)]
    center: Option<[f32; 2]>,
}

/// Where a Transform with no `center` pivots: the middle of `src`'s data
/// window, or of its frame when it holds no data (pixel i spanning i..i+1,
/// as shapes and the viewer's handles count).
pub fn transform_pivot(src: &Image) -> [f32; 2] {
    let w = src.window();
    let r = if w.is_empty() { src.frame() } else { w };
    [(r.x0 + r.x1) as f32 / 2.0, (r.y0 + r.y1) as f32 / 2.0]
}

/// Affine transform with bilinear resampling. Uses inverse mapping: for each
/// output pixel we find the source location and sample it.
fn transform(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Transform = parse(params, "transform")?;
    let src = input(inputs, 0, "transform")?;
    // About the pivot, given like a shape's center (pixel i spans i..i+1);
    // here pixel centers are on integers.
    let [cx, cy] = p.center.unwrap_or_else(|| transform_pivot(src)).map(|v| v - 0.5);
    let theta = -p.rotate.to_radians(); // inverse rotation
    let (s, c) = theta.sin_cos();
    let sx = if p.scale[0] == 0.0 { 1e-6 } else { p.scale[0] };
    let sy = if p.scale[1] == 0.0 { 1e-6 } else { p.scale[1] };
    // Forward: scale, rotate, then translate.
    let window = moved_window(src.window(), src.format, |[x, y]| {
        let (dx, dy) = ((x - cx) * sx, (y - cy) * sy);
        Some([dx * c + dy * s + cx + p.translate[0], -dx * s + dy * c + cy + p.translate[1]])
    });
    // Undo translate, then rotate+scale about center (inverse).
    let inverse = |[ox, oy]: [f32; 2]| {
        let dx = ox - p.translate[0] - cx;
        let dy = oy - p.translate[1] - cy;
        Some([(dx * c - dy * s) / sx + cx, (dx * s + dy * c) / sy + cy])
    };
    let mut out = generate(src.format, window, |ox, oy| sample_warped(src, ox, oy, inverse));
    out.carry_planes(src, |x, y| inverse([x as f32, y as f32]));
    Ok(out)
}

/// The most taps a side [`sample_warped`] averages: shrinking by more than
/// this much aliases a little again, but costs no more.
const MAX_TAPS: usize = 16;

/// Output pixel (`ox`, `oy`) of a warp whose `inverse` maps output
/// positions to source positions (both with pixel centers on integers;
/// None: nothing there). Bilinear where the warp keeps or enlarges the
/// image; where it shrinks it, the mean of a grid of bilinear taps spread
/// over the source area the pixel covers (its footprint, from the warp's
/// local derivatives), so detail averages out instead of aliasing (as
/// Resize does). Works for any warp: turned, uneven, or in perspective.
fn sample_warped(src: &Image, ox: i32, oy: i32, inverse: impl Fn([f32; 2]) -> Option<[f32; 2]>) -> Rgba {
    let (x, y) = (ox as f32, oy as f32);
    let Some(a) = inverse([x, y]) else { return color::TRANSPARENT };
    // Source distance per output pixel along the output's x and y.
    let (du, dv) = match (inverse([x + 1.0, y]), inverse([x, y + 1.0])) {
        (Some(ax), Some(ay)) => ([ax[0] - a[0], ax[1] - a[1]], [ay[0] - a[0], ay[1] - a[1]]),
        _ => return src.sample_bilinear(a[0], a[1]),
    };
    // (A hair of slack: a turn at scale 1 measures 1.0000001, not 1.)
    let taps = |d: [f32; 2]| ((d[0].hypot(d[1]) - 1e-3).ceil().max(1.0) as usize).min(MAX_TAPS);
    let (nu, nv) = (taps(du), taps(dv));
    if nu == 1 && nv == 1 {
        return src.sample_bilinear(a[0], a[1]);
    }
    let mut acc = [0.0f32; 4];
    for j in 0..nv {
        let t = (j as f32 + 0.5) / nv as f32 - 0.5;
        for k in 0..nu {
            let s = (k as f32 + 0.5) / nu as f32 - 0.5;
            let px = src.sample_bilinear(a[0] + du[0] * s + dv[0] * t, a[1] + du[1] * s + dv[1] * t);
            for c in 0..4 {
                acc[c] += px[c];
            }
        }
    }
    let n = (nu * nv) as f32;
    acc.map(|v| v / n)
}

// ---------------------------------------------------------------------------
// Tracker
// ---------------------------------------------------------------------------

/// Apply the motion solved from the tracks, relative to the reference
/// frame: `params` carry each track's position now (`track<i>`) and at the
/// reference frame (`ref<i>`, added by [`resolve_params`]).
fn tracker(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let output = params.get("output").and_then(Value::as_str).unwrap_or("input");
    if output == "input" {
        return Ok(input(inputs, 0, "tracker")?.clone());
    }
    let src = input(inputs, 0, "tracker")?;
    let p = params_with_defaults("tracker", params);
    let now = track::positions(&p, 0);
    let refs: Vec<_> = (1..=now.len())
        .map(|i| {
            let r = p[format!("ref{i}")].as_array().filter(|a| a.len() >= 2)?;
            Some([r[0].as_f64().unwrap_or(0.0) as f32, r[1].as_f64().unwrap_or(0.0) as f32])
        })
        .collect();
    let ref_warps: Vec<_> = (1..=now.len())
        .map(|i| {
            let w = p[format!("refwarp{i}")].as_array().filter(|a| a.len() >= 2)?;
            Some(track::Warp { angle: (w[0].as_f64()? as f32).to_radians(), scale: w[1].as_f64()? as f32 })
        })
        .collect();
    let kind = Solve::from_choice(p["solve"].as_str(), Solve::Scale);
    // Reference → now. A match-move moves the image along with the
    // tracks, so each output pixel samples where it came from.
    let m = track::solve_tracks(&refs, &now, &ref_warps, &track::warps(&p, 0), kind).context("no tracks enabled")?;
    let sample_at = match output {
        "match-move" => track::invert(&m),
        "stabilize" => m,
        other => bail!("unknown tracker output '{other}'"),
    };
    Ok(warp(src, &sample_at))
}

// ---------------------------------------------------------------------------
// Corner pin
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CornerPin {
    to1: Pt,
    to2: Pt,
    to3: Pt,
    to4: Pt,
    from1: Pt,
    from2: Pt,
    from3: Pt,
    from4: Pt,
}

/// Perspective warp taking the `from` quad onto the `to` quad. Output pixels
/// that map from behind the source's horizon stay transparent.
fn corner_pin(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: CornerPin = parse(params, "corner_pin")?;
    let src = input(inputs, 0, "corner_pin")?;
    let to = [p.to1, p.to2, p.to3, p.to4];
    let from = [p.from1, p.from2, p.from3, p.from4];
    // Each output pixel reads where it came from: to → from.
    let h = track::homography(&to, &from).context("corner_pin: three of the corners are in a line")?;
    let forward = track::homography(&from, &to).context("corner_pin: three of the corners are in a line")?;
    // In track coordinates pixel centers sit at +0.5.
    let window = moved_window(src.window(), src.format, |[x, y]| track::project(&forward, [x + 0.5, y + 0.5]).map(|[u, v]| [u - 0.5, v - 0.5]));
    let inverse = |[x, y]: [f32; 2]| track::project(&h, [x + 0.5, y + 0.5]).map(|[u, v]| [u - 0.5, v - 0.5]);
    let mut out = generate(src.format, window, |x, y| sample_warped(src, x, y, inverse));
    out.carry_planes(src, |x, y| inverse([x as f32, y as f32]));
    Ok(out)
}

/// Resample `src`: each output pixel center `o` reads `src` at `m(o)`.
fn warp(src: &Image, m: &track::Affine) -> Image {
    let forward = track::invert(m);
    let window = moved_window(src.window(), src.format, |[x, y]| {
        let [u, v] = track::apply(&forward, [x + 0.5, y + 0.5]);
        Some([u - 0.5, v - 0.5])
    });
    // Track coordinates put pixel centers at +0.5; `sample_bilinear` on integers.
    let inverse = |[x, y]: [f32; 2]| {
        let [u, v] = track::apply(m, [x + 0.5, y + 0.5]);
        Some([u - 0.5, v - 0.5])
    };
    let mut out = generate(src.format, window, |x, y| sample_warped(src, x, y, inverse));
    out.carry_planes(src, |x, y| inverse([x as f32, y as f32]));
    out
}

// ---------------------------------------------------------------------------
// Blur (separable box blur, repeated to approximate a gaussian)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Blur {
    radius: f32,
    passes: usize,
}

fn blur(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Blur = parse(params, "blur")?;
    let src = input(inputs, 0, "blur")?;
    let r = p.radius.max(0.0).round() as usize;
    if r == 0 || src.window().is_empty() {
        return Ok(src.clone());
    }
    // The blur spreads into transparency by its full reach, except where
    // the data meets the frame's edge: there the edge pixels repeat, as
    // for a full-frame plate.
    let reach = (r * p.passes.max(1)) as i32;
    let (w, f) = (src.window(), src.frame());
    let region = IRect::new(
        w.x0 - if w.x0 > f.x0 { reach } else { 0 },
        w.y0 - if w.y0 > f.y0 { reach } else { 0 },
        w.x1 + if w.x1 < f.x1 { reach } else { 0 },
        w.y1 + if w.y1 < f.y1 { reach } else { 0 },
    );
    let mut img = generate(src.format, region, |x, y| src.at(x, y));
    for _ in 0..p.passes.max(1) {
        box_pass_h(&mut img, r);
        box_pass_v(&mut img, r);
    }
    Ok(img)
}

fn box_pass_h(img: &mut Image, r: usize) {
    let w = img.width;
    let src = img.pixels.clone();
    let norm = 1.0 / (2 * r + 1) as f32;
    img.pixels
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            let base = y * w;
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut acc = [0.0f32; 4];
                for k in 0..=(2 * r) {
                    let sx = (x as isize + k as isize - r as isize).clamp(0, w as isize - 1) as usize;
                    let s = src[base + sx];
                    for ch in 0..4 {
                        acc[ch] += s[ch];
                    }
                }
                for ch in 0..4 {
                    out_px[ch] = acc[ch] * norm;
                }
            }
        });
}

fn box_pass_v(img: &mut Image, r: usize) {
    let w = img.width;
    let h = img.height;
    let src = img.pixels.clone();
    let norm = 1.0 / (2 * r + 1) as f32;
    img.pixels
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut acc = [0.0f32; 4];
                for k in 0..=(2 * r) {
                    let sy = (y as isize + k as isize - r as isize).clamp(0, h as isize - 1) as usize;
                    let s = src[sy * w + x];
                    for ch in 0..4 {
                        acc[ch] += s[ch];
                    }
                }
                for ch in 0..4 {
                    out_px[ch] = acc[ch] * norm;
                }
            }
        });
}

#[derive(Deserialize)]
struct Exposure {
    stops: f32,
}

fn exposure(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Exposure = parse(params, "exposure")?;
    let k = 2f32.powf(p.stops);
    Ok(mapped(input(inputs, 0, "exposure")?, |px| [px[0] * k, px[1] * k, px[2] * k, px[3]]))
}

/// Light 2D: the light its params describe (see [`crate::light2d`]).
pub fn light_2d_light(p: &Value) -> crate::light2d::Light2d {
    use crate::light2d::{Blend, Light2d, Shape};
    let num = |n: &str, d: f64| p[n].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let pos = crate::scene::numbers::<2>(&p["position"], 0.0);
    let shape = match p["type"].as_str() {
        Some("area") => Shape::Area { center: pos, size: crate::scene::numbers(&p["size"], 400.0), rotation: num("rotation", 0.0), ellipse: p["shape"].as_str() == Some("ellipse"), softness: num("feather", 50.0).max(0.0) },
        Some("point") => Shape::Point { position: pos, radius: num("radius", 400.0).max(0.0), falloff: num("falloff", 1.0).max(0.0) },
        Some("directional") => Shape::Directional { angle: num("direction", 90.0), position: pos, falloff: num("gradient", 300.0).max(0.0) },
        _ => Shape::Spot { position: pos, direction: num("direction", 90.0), cone: num("cone", 60.0).clamp(0.0, 360.0), softness: num("softness", 10.0).max(0.0), reach: num("reach", 0.0).max(0.0) },
    };
    let c = crate::scene::numbers::<3>(&p["color"], 1.0).map(|v| v as f32);
    let blend = match p["blend"].as_str() {
        Some("screen") => Blend::Screen,
        Some("multiply") => Blend::Multiply,
        _ => Blend::Add,
    };
    Light2d {
        color: c,
        intensity: num("intensity", 1.0) as f32,
        exposure: num("exposure", 0.0) as f32,
        blend,
        height: num("height", 100.0),
        elevation: num("elevation", 45.0),
        specular: num("specular", 0.0) as f32,
        roughness: num("roughness", 0.5) as f32,
        ..Light2d::new(shape)
    }
}

/// Light 2D on its input, with its normals (a render's `N` pass if it
/// has one, else a normal map) and depth inputs if connected.
fn light_2d(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    use crate::light2d::{Depth, Normals};
    let img = input(inputs, 0, "light_2d")?;
    let normals = inputs.get(1).copied().flatten().map(|n| if n.plane("N.x").is_some() { Normals::Pass(n) } else { Normals::Map(n) });
    let depth = inputs.get(2).copied().flatten().map(|image| Depth { image, scale: params["depth_scale"].as_f64().unwrap_or(1.0) });
    Ok(crate::light2d::light2d(img, &[light_2d_light(params)], normals, depth, None))
}

/// Rec. 709 luminance weights.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

#[derive(Deserialize)]
struct Saturation {
    saturation: f32,
}

/// Premultiplied color can be scaled about its luminance directly: both are
/// linear in color, so the result equals doing it on straight color.
fn saturation(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Saturation = parse(params, "saturation")?;
    let s = p.saturation;
    Ok(mapped(input(inputs, 0, "saturation")?, |px| {
        let y = px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2];
        [y + (px[0] - y) * s, y + (px[1] - y) * s, y + (px[2] - y) * s, px[3]]
    }))
}

#[derive(Deserialize)]
struct Channels {
    channels: String,
}

fn invert(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Channels = parse(params, "invert")?;
    let (rgb, alpha) = match p.channels.as_str() {
        "rgb" => (true, false),
        "alpha" => (false, true),
        "rgba" => (true, true),
        other => bail!("unknown channels '{other}'"),
    };
    Ok(mapped(input(inputs, 0, "invert")?, |px| {
        // Transparent pixels unpremultiply to black.
        let mut s = color::unpremultiply(px);
        if rgb {
            for c in s.iter_mut().take(3) {
                *c = 1.0 - *c;
            }
        }
        s[3] = if alpha { 1.0 - px[3].clamp(0.0, 1.0) } else { px[3] };
        color::premultiply(s)
    }))
}

#[derive(Deserialize)]
struct Clamp {
    min: f32,
    max: f32,
    channels: String,
}

fn clamp(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Clamp = parse(params, "clamp")?;
    let (lo, hi) = (p.min.min(p.max), p.max.max(p.min));
    let alpha = p.channels == "rgba";
    Ok(mapped(input(inputs, 0, "clamp")?, |px| {
        let a = if alpha { px[3].clamp(lo.max(0.0), hi.min(1.0)) } else { px[3] };
        [px[0].clamp(lo, hi), px[1].clamp(lo, hi), px[2].clamp(lo, hi), a]
    }))
}

// ---------------------------------------------------------------------------
// Channel
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChannelSelect {
    red: String,
    green: String,
    blue: String,
    alpha: String,
}

/// Where an output channel comes from: (input index, channel), or a constant.
enum Source {
    Channel(usize, usize),
    Value(f32),
}

fn channel_source(name: &str) -> Result<Source> {
    if let Some(i) = CHANNEL_SOURCES.iter().position(|s| *s == name) {
        return Ok(match i {
            0..=7 => Source::Channel(i / 4, i % 4),
            8 => Source::Value(0.0),
            _ => Source::Value(1.0),
        });
    }
    bail!("unknown channel source '{name}'")
}

fn channel_select(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: ChannelSelect = parse(params, "channel_select")?;
    let a = input(inputs, 0, "channel_select")?;
    let srcs = [&p.red, &p.green, &p.blue, &p.alpha].map(|n| channel_source(n));
    let srcs = srcs.into_iter().collect::<Result<Vec<_>>>()?;
    let b = inputs.get(1).copied().flatten();
    if b.is_none() && srcs.iter().any(|s| matches!(s, Source::Channel(1, _))) {
        bail!("input 'B' is not connected");
    }
    let pick = |pa: Rgba, pb: Rgba| -> Rgba {
        std::array::from_fn(|c| match srcs[c] {
            Source::Channel(0, ch) => pa[ch],
            Source::Channel(_, ch) => pb[ch],
            Source::Value(v) => v,
        })
    };
    let windows: Vec<IRect> = [Some(a), b].into_iter().flatten().map(Image::window).collect();
    let window = pointwise_window(a.format, &windows, pick(color::TRANSPARENT, color::TRANSPARENT));
    Ok(generate(a.format, window, |x, y| pick(a.at(x, y), b.map_or(color::TRANSPARENT, |b| b.at(x, y)))))
}

#[derive(Deserialize)]
struct Shuffle {
    #[serde(rename = "in")]
    from: String,
    red: String,
    green: String,
    blue: String,
    alpha: String,
    out: String,
    remove: String,
}

/// Channels and passes moved about: `in` (a layer, or `rgba`) into the
/// color, each channel then from any channel or plane, the result into
/// `out` (a layer, keeping the color, or `rgba`; an existing layer keeps
/// its channel count, a new one gets `in`'s), and `remove`'s layers
/// dropped.
fn shuffle(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Shuffle = parse(params, "shuffle")?;
    let a = input(inputs, 0, "shuffle")?;
    let whole = |s: &str| s.trim().is_empty() || s.trim().eq_ignore_ascii_case("rgba");
    let (img, count) = if whole(&p.from) { (a.clone(), 4) } else { (a.layer_to_rgba(p.from.trim())?, a.layer(p.from.trim()).len()) };
    fn pick<'a>(s: &'a str, default: &'static str) -> &'a str {
        if s.trim().is_empty() { default } else { s.trim() }
    }
    let rgba = [pick(&p.red, "R"), pick(&p.green, "G"), pick(&p.blue, "B"), pick(&p.alpha, "A")];
    let mut out = img.shuffle(rgba)?;
    if !whole(&p.out) {
        // Into an existing pass, as many channels as it has; else as many
        // as came in.
        let existing = a.layer(p.out.trim()).len();
        out.rgba_to_layer(p.out.trim(), if existing > 0 { existing } else { count })?;
        out.pixels.clone_from(&a.pixels);
    }
    let drop: Vec<&str> = p.remove.split([',', ' ']).filter(|s| !s.is_empty()).collect();
    out.remove_layers(&drop);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Switch, dissolve, keymix
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Switch {
    which: i64,
}

fn switch(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Switch = parse(params, "switch")?;
    let n = p.which.clamp(0, SWITCH_PORTS as i64 - 1) as usize;
    Ok(input(inputs, n, "switch")?.clone())
}

#[derive(Deserialize)]
struct Dissolve {
    amount: f32,
}

fn dissolve(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Dissolve = parse(params, "dissolve")?;
    let a = input(inputs, 0, "dissolve")?;
    let b = input(inputs, 1, "dissolve")?;
    let t = p.amount.clamp(0.0, 1.0);
    let window = a.window().union(&b.window());
    let mut out = generate(b.format, window, |x, y| {
        let (pa, pb) = (a.at(x, y), b.at(x, y));
        std::array::from_fn(|c| pa[c] + (pb[c] - pa[c]) * t)
    });
    // Planes can't be blended: the nearer input's.
    out.keep_planes(if t < 0.5 { a } else { b });
    Ok(out)
}

/// A in B's frame; [`eval`]'s mask/mix step then blends it over B by the
/// mask.
fn keymix(inputs: &[Option<&Image>]) -> Result<Image> {
    let a = input(inputs, 0, "keymix")?;
    let b = input(inputs, 1, "keymix")?;
    Ok(generate(b.format, a.window(), |x, y| a.at(x, y)))
}

// ---------------------------------------------------------------------------
// Keyer
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Keyer {
    /// None: estimate it from the image.
    #[serde(default)]
    screen: Option<[f32; 4]>,
    balance: f32,
    clip_black: f32,
    clip_white: f32,
    despill: f32,
    shrink_grow: f32,
    softness: f32,
    output: String,
}

/// A screen color's dominant channel and the other two.
fn screen_channels(s: Rgba) -> (usize, usize, usize) {
    if s[2] > s[1] && s[2] > s[0] {
        (2, 0, 1)
    } else if s[0] > s[1] && s[0] > s[2] {
        (0, 1, 2)
    } else {
        (1, 0, 2)
    }
}

/// The other two channels of `c`, weighted: `balance` 1 takes the larger,
/// 0 the smaller.
fn others(c: Rgba, q: usize, r: usize, balance: f32) -> f32 {
    let (hi, lo) = (c[q].max(c[r]), c[q].min(c[r]));
    balance * hi + (1.0 - balance) * lo
}

/// The screen color of a green or blue screen plate: the per-channel
/// median of the pixels where green (or blue, if more pixels lean that way)
/// clearly leads the other two, so edges and spill don't pull it. Green
/// when nothing does.
pub fn estimate_screen(img: &Image) -> Rgba {
    let leading = |p: usize, q: usize, r: usize| -> Vec<[f32; 3]> {
        img.pixels
            .par_iter()
            .filter(|px| px[3] > 0.0)
            .map(|&px| color::unpremultiply(px))
            .filter(|c| c[p] > 0.02 && c[p] > 1.25 * c[q].max(c[r]))
            .map(|c| [c[0], c[1], c[2]])
            .collect()
    };
    let (green, blue) = (leading(1, 0, 2), leading(2, 0, 1));
    let mut pick = if blue.len() > green.len() { blue } else { green };
    if pick.is_empty() {
        return [0.1, 0.5, 0.1, 1.0];
    }
    let mid = pick.len() / 2;
    let mut out = [0.0, 0.0, 0.0, 1.0];
    for (i, o) in out.iter_mut().take(3).enumerate() {
        pick.select_nth_unstable_by(mid, |a, b| a[i].total_cmp(&b[i]));
        *o = pick[mid][i];
    }
    out
}

/// Color-difference keying (see the op's help).
fn keyer(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Keyer = parse(params, "keyer")?;
    let src = input(inputs, 0, "keyer")?;
    let plate = inputs.get(1).copied().flatten();
    let screen = p.screen.unwrap_or_else(|| estimate_screen(src));
    let output = KEYER_OUTPUTS.iter().position(|o| *o == p.output).ok_or_else(|| anyhow!("unknown output '{}'", p.output))?;
    let span = (p.clip_white - p.clip_black).max(1e-4);
    // At (x, y): the matte (the keyed image's alpha) and the despilled
    // straight color.
    let key = |x: i32, y: i32| -> (f32, Rgba) {
        let px = src.at(x, y);
        let c = color::unpremultiply(px);
        // The screen here: the clean plate's, where it shows one.
        let s = plate.map(|pl| color::unpremultiply(pl.at(x, y))).filter(|s| s[3] > 0.0).unwrap_or(screen);
        let (pc, q, r) = screen_channels(s);
        let ds = s[pc] - others(s, q, r, p.balance);
        let dc = c[pc] - others(c, q, r, p.balance);
        let raw = if ds > 1e-6 { (1.0 - dc / ds).clamp(0.0, 1.0) } else { 1.0 };
        let alpha = ((raw - p.clip_black) / span).clamp(0.0, 1.0);
        let mut fg = c;
        fg[pc] -= p.despill * (c[pc] - others(c, q, r, p.balance)).max(0.0);
        (alpha * px[3], fg)
    };
    let shade = |m: f32, fg: Rgba| -> Rgba {
        match output {
            0 => [fg[0] * m, fg[1] * m, fg[2] * m, m],
            1 => [m, m, m, 1.0],
            _ => color::premultiply(fg),
        }
    };
    let mut windows = vec![src.window()];
    windows.extend(plate.map(Image::window));
    let data = windows.iter().fold(IRect::default(), |u, w| u.union(w));
    let grow = p.shrink_grow.clamp(-50.0, 50.0);
    // Softness is how far the soft edge reaches: three box passes of a
    // third of it each (at least one pixel).
    let soft = if p.softness > 0.0 { (p.softness.min(50.0) / 3.0).round().max(1.0) as usize } else { 0 };
    if output == 2 || (grow == 0.0 && soft == 0) {
        // Pixel by pixel.
        let window = pointwise_window(src.format, &windows, shade(0.0, color::TRANSPARENT));
        return Ok(generate(src.format, window, |x, y| {
            let (m, fg) = key(x, y);
            shade(m, fg)
        }));
    }
    // The matte over the data, grown into the space around it (up to the
    // frame), then shrunk or grown and softened.
    let pad = grow.max(0.0).ceil() as i32 + 3 * soft as i32;
    let region = data.expand(pad).intersect(&data.union(&IRect::frame(src.format)));
    let mut matte = generate(src.format, region, |x, y| [key(x, y).0; 4]);
    morph(&mut matte, grow);
    if soft > 0 {
        // Three box passes: close to a gaussian.
        for _ in 0..3 {
            box_pass_h(&mut matte, soft);
            box_pass_v(&mut matte, soft);
        }
    }
    let window = if output == 1 { region.union(&IRect::frame(src.format)) } else { region };
    Ok(generate(src.format, window, |x, y| shade(matte.at(x, y)[3], key(x, y).1)))
}

/// Grow (`by` > 0) or shrink a matte (in every channel) by `by` pixels:
/// one-pixel steps alternating 4- and 8-neighbor max (or min), which keeps
/// round edges round (an octagon), the fraction blended. Edges repeat.
fn morph(img: &mut Image, by: f32) {
    let steps = by.abs().ceil() as usize;
    let frac = by.abs() - by.abs().floor();
    let (w, h) = (img.width, img.height);
    if steps == 0 || w == 0 || h == 0 {
        return;
    }
    let pick = |a: f32, b: f32| if by > 0.0 { a.max(b) } else { a.min(b) };
    let mut before = Vec::new();
    for step in 0..steps {
        if step + 1 == steps && frac > 0.0 {
            before = img.pixels.clone();
        }
        let src = img.pixels.clone();
        let square = step % 2 == 1;
        img.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                let mut v = src[y * w + x][3];
                for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)].into_iter().take(if square { 8 } else { 4 }) {
                    let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    v = pick(v, src[sy * w + sx][3]);
                }
                *out = [v; 4];
            }
        });
    }
    if !before.is_empty() {
        img.pixels.iter_mut().zip(&before).for_each(|(p, b)| *p = [b[3] + (p[3] - b[3]) * frac; 4]);
    }
}

// ---------------------------------------------------------------------------
// Crop and resize
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Crop {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
    mode: String,
}

fn crop(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Crop = parse(params, "crop")?;
    let src = input(inputs, 0, "crop")?;
    let f = src.format;
    let x0 = p.left.min(f.width);
    let y0 = p.top.min(f.height);
    let x1 = f.width.saturating_sub(p.right).max(x0);
    let y1 = f.height.saturating_sub(p.bottom).max(y0);
    let keep = IRect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32);
    let window = src.window().intersect(&keep);
    match p.mode.as_str() {
        "clear outside" => {
            let mut out = generate(f, window, |x, y| src.at(x, y));
            out.keep_planes(src);
            Ok(out)
        }
        "shrink canvas" => {
            // The box becomes the frame, its corner the new origin.
            let format = Size { width: keep.width().max(1), height: keep.height().max(1) };
            let (dx, dy) = (keep.x0, keep.y0);
            let moved = IRect::new(window.x0 - dx, window.y0 - dy, window.x1 - dx, window.y1 - dy);
            let mut out = generate(format, moved, |x, y| src.at(x + dx, y + dy));
            out.carry_planes(src, |x, y| Some([(x + dx) as f32, (y + dy) as f32]));
            Ok(out)
        }
        other => bail!("unknown crop mode '{other}'"),
    }
}

#[derive(Deserialize)]
struct Resize {
    width: usize,
    height: usize,
    fit: String,
}

/// Resample with a box filter sized to the scale when shrinking (so no
/// pixels are skipped) and bilinear interpolation when enlarging.
fn resize(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Resize = parse(params, "resize")?;
    let src = input(inputs, 0, "resize")?;
    let (w, h) = (p.width.max(1), p.height.max(1));
    let (sx, sy) = (w as f32 / src.format.width.max(1) as f32, h as f32 / src.format.height.max(1) as f32);
    let (sx, sy) = match p.fit.as_str() {
        "stretch" => (sx, sy),
        "fit" => (sx.min(sy), sx.min(sy)),
        "fill" => (sx.max(sy), sx.max(sy)),
        other => bail!("unknown fit '{other}'"),
    };
    Ok(resample(src, Size { width: w, height: h }, sx, sy, false))
}

/// `src` scaled by `sx` × `sy` into a frame of `format`, centered: a box
/// filter shrinking (taps spread over each output pixel's footprint),
/// bilinear growing. Its planes come along. With `hard_edges`, taps past
/// the source's data stay on its edge pixels and only how much of each
/// output pixel the scaled data covers softens its outline (a plate
/// filling the frame keeps a solid border); else they fade to transparent
/// (Resize).
fn resample(src: &Image, format: Size, sx: f32, sy: f32, hard_edges: bool) -> Image {
    let (w, h) = (format.width, format.height);
    // Center the scaled image in the output frame.
    let ox = (w as f32 - src.format.width as f32 * sx) / 2.0;
    let oy = (h as f32 - src.format.height as f32 * sy) / 2.0;
    // Source pixels per output pixel, and how many taps cover that span.
    let (fx, fy) = (1.0 / sx, 1.0 / sy);
    let (nx, ny) = (fx.ceil().max(1.0) as usize, fy.ceil().max(1.0) as usize);
    let sw = src.window();
    // Where the scaled data lands (output pixels).
    let (px0, px1) = (ox + sw.x0 as f32 * sx, ox + sw.x1 as f32 * sx);
    let (py0, py1) = (oy + sw.y0 as f32 * sy, oy + sw.y1 as f32 * sy);
    let window = if hard_edges {
        IRect::new(px0.floor() as i32, py0.floor() as i32, px1.ceil() as i32, py1.ceil() as i32)
    } else {
        moved_window(sw, format, |[x, y]| Some([(x + 0.5) * sx + ox - 0.5, (y + 0.5) * sy + oy - 0.5]))
    };
    let sample = |x: f32, y: f32| {
        if hard_edges && !sw.is_empty() {
            src.sample_bilinear(x.clamp(sw.x0 as f32, (sw.x1 - 1) as f32), y.clamp(sw.y0 as f32, (sw.y1 - 1) as f32))
        } else {
            src.sample_bilinear(x, y)
        }
    };
    // How much of output pixel (x, y) the scaled data covers.
    let cover = |x: i32, y: i32| -> f32 {
        let span = |a: f32, lo: f32, hi: f32| (hi.min(a + 1.0) - lo.max(a)).clamp(0.0, 1.0);
        span(x as f32, px0, px1) * span(y as f32, py0, py1)
    };
    let mut out = generate(format, window, |x, y| {
        // Source position of this output pixel's center.
        let cx = (x as f32 + 0.5 - ox) * fx - 0.5;
        let cy = (y as f32 + 0.5 - oy) * fy - 0.5;
        let k = if hard_edges { cover(x, y) } else { 1.0 };
        if nx == 1 && ny == 1 {
            return sample(cx, cy).map(|v| v * k);
        }
        // Average an nx × ny grid of bilinear taps spread over the footprint.
        let mut acc = [0.0f32; 4];
        for j in 0..ny {
            for k in 0..nx {
                let tx = cx + fx * ((k as f32 + 0.5) / nx as f32 - 0.5);
                let ty = cy + fy * ((j as f32 + 0.5) / ny as f32 - 0.5);
                let s = sample(tx, ty);
                for c in 0..4 {
                    acc[c] += s[c];
                }
            }
        }
        let norm = k / (nx * ny) as f32;
        acc.map(|v| v * norm)
    });
    out.carry_planes(src, |x, y| Some([(x as f32 + 0.5 - ox) * fx - 0.5, (y as f32 + 0.5 - oy) * fy - 0.5]));
    out
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// The output of a node making `scene`: no pixels, frame `format`.
fn scene_output(format: Size, scene: Scene) -> Image {
    let mut out = Image::blank(format, IRect::default());
    out.scene = Some(std::sync::Arc::new(scene));
    out
}

#[derive(Deserialize)]
struct ScanlineRender {
    width: usize,
    height: usize,
    samples: usize,
    /// None (a project from before passes): depth only.
    #[serde(default)]
    passes: Option<Vec<String>>,
    /// Renders across the shutter (0 or 1: no motion blur; missing: 0).
    #[serde(default)]
    motion_blur: usize,
    #[serde(default = "half")]
    shutter: f64,
    #[serde(default)]
    shutter_offset: Option<String>,
    /// Each pixel's shutter times jittered (missing: on).
    #[serde(default = "on")]
    jitter: bool,
}

fn half() -> f64 {
    0.5
}

fn on() -> bool {
    true
}

impl ScanlineRender {
    /// With motion blur, the times to render at (frames from the frame).
    fn blur_times(&self) -> Option<Vec<f64>> {
        if self.motion_blur < 2 || self.shutter <= 0.0 {
            return None;
        }
        let start = match self.shutter_offset.as_deref() {
            Some("start") => 0.0,
            Some("end") => -self.shutter,
            _ => -self.shutter / 2.0,
        };
        Some(render3d::Blur::times(start, self.shutter, self.motion_blur))
    }
}

/// A particle node outside a graph: its params the same at every frame,
/// simulated afresh (the graph's evaluator keeps its simulation and reads
/// animated params at every frame).
fn particle_node(op: &str, params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Result<Image> {
    let fixed = params.clone();
    let upstream = inputs.first().copied().flatten().and_then(|i| i.scene.as_ref()).and_then(|s| s.particles.first());
    let keys = vec![0; inputs.len()];
    let cx = particle_nodes::Context { node: op, frame: ctx.frame, fps: 24.0, format: DEFAULT_FORMAT, own: std::sync::Arc::new(move |_| fixed.clone()), inputs, keys: &keys, upstream, sequence: None, history: None };
    Ok(match particle_nodes::eval(op, cx, &mut particle_nodes::ParticleState::default())? {
        particle_nodes::Output::Image(img) => img,
        particle_nodes::Output::Stream(stream) => scene_output(DEFAULT_FORMAT, Scene { particles: vec![stream], ..Scene::default() }),
    })
}

fn scanline_render(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    scanline_render_frames(params, inputs, &[])
}

/// A Scanline Render's motion blur times (frames from the frame), from
/// its params at the frame; None without blur.
pub fn blur_times(params: &Value) -> Option<Vec<f64>> {
    serde_json::from_value::<ScanlineRender>(params.clone()).ok()?.blur_times()
}

/// Whether a Scanline Render with `params` jitters its pixels' shutter
/// times (see [`render3d::Blur::jitter`]): on unless Jitter shutter is off.
pub fn blur_jitter(params: &Value) -> bool {
    params.get("jitter").and_then(Value::as_bool).unwrap_or(true)
}

/// The frames besides its own (offsets from it) whose inputs a node of `op`
/// with (resolved) `params` needs too (see [`scanline_render_frames`]): a
/// Scanline Render's next with the motion pass, and with motion blur
/// those round its shutter. Ascending. Params left out count as their
/// defaults.
pub fn other_frames(op: &str, params: &Value) -> Vec<i64> {
    if op != "scanline_render" {
        return Vec::new();
    }
    let params = &params_with_defaults(op, params);
    let mut out = Vec::new();
    if params["passes"].as_array().is_some_and(|a| a.iter().any(|v| v == "motion")) {
        out.push(1);
    }
    let times = serde_json::from_value::<ScanlineRender>(params.clone()).ok().and_then(|p| p.blur_times());
    if let Some(t) = times {
        let (lo, hi) = t.iter().fold((0.0f64, 0.0f64), |(lo, hi), &t| (lo.min(t), hi.max(t)));
        out.extend((lo.floor() as i64..=hi.ceil() as i64).filter(|k| *k != 0));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A Scanline Render's scene (its scene input's, with the scenes or
/// particles on its extra inputs joined) and camera (the camera input's,
/// else the scene's first), from `inputs`.
fn render_scene(inputs: &[Option<&Image>]) -> (Option<std::sync::Arc<Scene>>, Option<crate::geom::Camera>) {
    let port = |i: usize| inputs.get(i).copied().flatten();
    let camera = port(1).and_then(|img| img.scene.as_ref()?.cameras.first().cloned()).or_else(|| port(0)?.scene.as_ref()?.cameras.first().cloned());
    let extra: Vec<&Scene> = inputs.iter().skip(3).flatten().filter_map(|i| i.scene.as_deref()).collect();
    let scene = match port(0).and_then(|img| img.scene.clone()) {
        Some(s) if extra.is_empty() => Some(s),
        first => {
            let mut all = first.as_deref().cloned().unwrap_or_default();
            extra.into_iter().for_each(|s| all.extend(s));
            Some(std::sync::Arc::new(all))
        }
    };
    (scene, camera)
}

/// A Scanline Render, given also its inputs at `others` frames (offset,
/// inputs then; see [`other_frames`]): the next for the motion pass, those
/// round the shutter for motion blur (unconnected or missing: as now).
pub fn scanline_render_frames(params: &Value, inputs: &[Option<&Image>], others: &[(i64, Vec<Option<&Image>>)]) -> Result<Image> {
    let p: ScanlineRender = parse(params, "scanline_render")?;
    let (scene, camera) = render_scene(inputs);
    let camera = camera.context("no camera: connect a Camera to the camera input, or put one in the scene")?;
    let bg = inputs.get(2).copied().flatten();
    let format = bg.map_or(source_size(p.width, p.height), |b| b.format);
    let empty = Scene::default();
    let scene = scene.as_deref().unwrap_or(&empty);
    let passes = p.passes.as_ref().map_or(render3d::Passes::DEPTH, |n| render3d::Passes::from_names(n.iter().map(String::as_str)));
    let then: Vec<(i64, Option<std::sync::Arc<Scene>>, crate::geom::Camera)> = others
        .iter()
        .map(|(k, ins)| {
            let (s, c) = render_scene(ins);
            (*k, s, c.unwrap_or_else(|| camera.clone()))
        })
        .collect();
    let next = then.iter().find(|t| t.0 == 1).map(|t| (t.1.as_deref().unwrap_or(&empty), &t.2));
    let later = next.filter(|_| passes.motion).or(passes.motion.then_some((&empty, &camera)));
    let blur = p.blur_times().map(|times| render3d::Blur {
        times,
        jitter: p.jitter,
        frames: std::iter::once((0, scene, camera.clone())).chain(then.iter().map(|t| (t.0, t.1.as_deref().unwrap_or(&empty), t.2.clone()))).collect(),
    });
    Ok(render3d::render_blurred(scene, &camera, format, p.samples, bg, passes, later, blur.as_ref()))
}

/// A Read Geo's meshes: the file's parts (OBJ, or glTF for .gltf / .glb;
/// read once per path and change time, with their textures), placed by its
/// transform.
fn read_geo(params: &Value, frame: i64) -> Result<Image> {
    static CACHE: std::sync::Mutex<Option<(std::path::PathBuf, Option<std::time::SystemTime>, GeoParts)>> = std::sync::Mutex::new(None);
    let file = params["file"].as_str().unwrap_or("");
    if file.is_empty() {
        bail!("no file: choose an OBJ file");
    }
    let path = std::path::PathBuf::from(file);
    let modified = std::fs::metadata(&path).with_context(|| format!("can't read {file}"))?.modified().ok();
    let alembic = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("abc"));
    let cached = CACHE.lock().unwrap().as_ref().filter(|c| c.0 == path && c.1 == modified).map(|c| c.2.clone());
    let parts = match cached {
        _ if alembic => alembic_parts(path, modified, params, frame)?,
        Some(p) => p,
        None => {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let parts = match ext.as_str() {
                "gltf" | "glb" => crate::gltf::read_gltf(&path)?,
                _ => crate::mesh::read_obj(&path)?,
            };
            let loaded: GeoParts = std::sync::Arc::new(
                parts
                    .into_iter()
                    .map(|part| {
                        // A texture that won't load leaves the color alone.
                        let tex = part.material.texture.as_deref().and_then(|t| Image::read(t).ok()).map(std::sync::Arc::new);
                        (part, tex)
                    })
                    .collect(),
            );
            *CACHE.lock().unwrap() = Some((path, modified, loaded.clone()));
            loaded
        }
    };
    let world = scene::placement(params);
    let meshes = parts
        .iter()
        .map(|(part, tex)| {
            let mut material = if params["override_material"].as_bool().unwrap_or(false) { scene::lighting(params, &part.material) } else { part.material.clone() };
            material.opacity *= params["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
            let m = scene::Model::new(std::sync::Arc::new(part.mesh.clone()), world, material).flags(params);
            scene::Model { texture: tex.clone(), name: part.name.clone(), ..m }
        })
        .collect();
    Ok(scene_output(DEFAULT_FORMAT, Scene { meshes, ..Scene::default() }))
}

/// A Read Geo's meshes, each with its texture.
type GeoParts = std::sync::Arc<Vec<(crate::mesh::Part, Option<std::sync::Arc<Image>>)>>;

/// A Read Geo's Alembic meshes at `frame`'s time ((frame + Frame offset) ÷
/// Frame rate): the archive read once per file and change, and its meshes
/// at the last few times asked kept.
fn alembic_parts(path: std::path::PathBuf, modified: Option<std::time::SystemTime>, params: &Value, frame: i64) -> Result<GeoParts> {
    use crate::alembic::Archive;
    use std::sync::{Arc, Mutex};
    type Key = (std::path::PathBuf, Option<std::time::SystemTime>);
    static ARCHIVE: Mutex<Option<(Key, Arc<Archive>)>> = Mutex::new(None);
    static TIMES: Mutex<Vec<(Key, u64, GeoParts)>> = Mutex::new(Vec::new());
    const KEPT: usize = 8;
    let fps = params["frame_rate"].as_f64().unwrap_or(24.0).max(1e-3);
    let t = (frame as f64 + params["frame_offset"].as_f64().unwrap_or(0.0)) / fps;
    let key = (path, modified);
    if let Some(p) = TIMES.lock().unwrap().iter().find(|c| c.0 == key && c.1 == t.to_bits()).map(|c| c.2.clone()) {
        return Ok(p);
    }
    let archive = {
        let mut a = ARCHIVE.lock().unwrap();
        match a.as_ref().filter(|a| a.0 == key) {
            Some(a) => a.1.clone(),
            None => {
                let opened = Arc::new(Archive::open(&key.0)?);
                *a = Some((key.clone(), opened.clone()));
                opened
            }
        }
    };
    let parts: GeoParts = Arc::new(archive.parts(t)?.into_iter().map(|p| (p, None)).collect());
    let mut times = TIMES.lock().unwrap();
    times.retain(|c| c.0 == key);
    if times.len() >= KEPT {
        times.remove(0);
    }
    times.push((key, t.to_bits(), parts.clone()));
    Ok(parts)
}

/// The scene on a splat editing node's input (an empty one when nothing
/// is connected).
fn input_scene(inputs: &[Option<&Image>]) -> Scene {
    inputs.first().copied().flatten().and_then(|i| i.scene.as_deref()).cloned().unwrap_or_default()
}

/// A Splat Crop: each capture's splats with their centers inside its box
/// or sphere (outside, inverted); the arrays shared where it keeps them all.
fn splat_crop(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let mut out = input_scene(inputs);
    let size = scene::numbers::<3>(&params["size"], 1.0).map(|v| (v.abs() / 2.0).max(1e-12));
    let region = geom::trs(scene::numbers(&params["center"], 0.0), scene::numbers(&params["rotate"], 0.0), size);
    // Scene space into the region's, where it's the unit cube or ball.
    let to_region = region.try_inverse().unwrap_or_else(geom::Mat4::identity);
    let sphere = params["shape"].as_str() == Some("sphere");
    let invert = params["invert"].as_bool().unwrap_or(false);
    // Shown in the 3D view (tagged with this node by the graph).
    out.regions.push(scene::Region { node: String::new(), world: region, sphere });
    out.splats = out
        .splats
        .iter()
        .map(|g| {
            let kept = g.select(|i| {
                let q = geom::transform_point(&to_region, &g.placed(i));
                let inside = if sphere { q.norm_squared() <= 1.0 } else { q.x.abs() <= 1.0 && q.y.abs() <= 1.0 && q.z.abs() <= 1.0 };
                inside != invert
            });
            std::sync::Arc::new(kept)
        })
        .collect();
    Ok(scene_output(DEFAULT_FORMAT, out))
}

/// The linear color transform a Splat Color's params describe: gain and
/// exposure after saturation (by Rec. 709 luminance), row-major.
fn splat_grade(params: &Value) -> [[f32; 3]; 3] {
    let k = 2f64.powf(params["exposure"].as_f64().unwrap_or(0.0)) as f32;
    let s = params["saturation"].as_f64().unwrap_or(1.0) as f32;
    let gain = scene::numbers::<3>(&params["gain"], 1.0).map(|g| g as f32 * k);
    let luma = [0.2126f32, 0.7152, 0.0722];
    std::array::from_fn(|r| std::array::from_fn(|c| gain[r] * ((1.0 - s) * luma[c] + if r == c { s } else { 0.0 })))
}

/// A Splat Color: every capture's colors through [`splat_grade`] (at SH
/// degree 0 first, when view independent) and opacities faded or raised;
/// the arrays shared where nothing changes.
fn splat_color(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let mut out = input_scene(inputs);
    let m = splat_grade(params);
    let fade = params["opacity"].as_f64().unwrap_or(1.0).max(0.0) as f32;
    let flat = params["view_independent"].as_bool().unwrap_or(false);
    out.splats = out
        .splats
        .iter()
        .map(|g| {
            let mut g = if flat { g.view_independent() } else { (**g).clone() };
            g.sh = g.graded_sh(m);
            if fade != 1.0 {
                g.opacity = std::sync::Arc::new(g.opacity.iter().map(|a| (a * fade).min(1.0)).collect());
            }
            std::sync::Arc::new(g)
        })
        .collect();
    Ok(scene_output(DEFAULT_FORMAT, out))
}

/// A Read Splat's capture: read (once per file, change and SH degree; see
/// [`crate::splat::Splats::read`]), turned so its up is +Y, placed, and
/// faded by Opacity (a copy of the opacities only when that isn't 1).
fn read_splat(params: &Value) -> Result<Image> {
    use crate::splat::Splats;
    let file = params["file"].as_str().unwrap_or("");
    if file.is_empty() {
        bail!("no file: choose a Gaussian splat .ply file");
    }
    let degree = params["sh_degree"].as_i64().unwrap_or(3).clamp(0, 3) as u8;
    let read = Splats::read(std::path::Path::new(file), degree)?;
    // Its up to +Y: Z-up turns −90° about X; Y-down (Z forward) 180°.
    let up = match params["up"].as_str().unwrap_or("-y") {
        "z" => geom::Mat4::new(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        "-y" => geom::Mat4::new(1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        _ => geom::Mat4::identity(),
    };
    let holdout = params["holdout"].as_bool().unwrap_or(false);
    let mut splats = Splats { world: scene::placement(params) * up, holdout, ..(*read).clone() };
    let fade = params["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    if fade != 1.0 {
        splats.opacity = std::sync::Arc::new(read.opacity.iter().map(|a| a * fade).collect());
    }
    Ok(scene_output(DEFAULT_FORMAT, Scene { splats: vec![std::sync::Arc::new(splats)], ..Scene::default() }))
}

/// Everything in the connected scenes, in port order; the first one's
/// frame.
fn gather(inputs: &[Option<&Image>]) -> Result<Image> {
    let mut all = Scene::default();
    let mut format = None;
    for img in inputs.iter().flatten() {
        if let Some(s) = &img.scene {
            all.extend(s);
            format.get_or_insert(img.format);
        }
    }
    Ok(scene_output(format.unwrap_or(DEFAULT_FORMAT), all))
}

// ---------------------------------------------------------------------------
// Lens distort
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LensDistort {
    mode: String,
    k1: f64,
    k2: f64,
    #[serde(default)]
    center: Option<[f64; 2]>,
}

fn lens_distort(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: LensDistort = parse(params, "lens_distort")?;
    let src = input(inputs, 0, "lens_distort")?;
    let f = src.format;
    let center = p.center.unwrap_or([f.width as f64 / 2.0, f.height as f64 / 2.0]);
    let lens = geom::Distortion::new(p.k1, p.k2, center, f);
    let undistort = match p.mode.as_str() {
        "undistort" => true,
        "distort" => false,
        other => bail!("unknown lens distort mode '{other}'"),
    };
    // Each output pixel reads where it came from: undistorting reads the
    // plate where the lens put it. `geom` puts pixel centers at +0.5,
    // sampling on integers.
    let map = |to_plate: bool, [x, y]: [f32; 2]| {
        let q = [x as f64 + 0.5, y as f64 + 0.5];
        let r = if to_plate { Some(lens.distort(q)) } else { lens.undistort(q) };
        r.map(|[u, v]| [(u - 0.5) as f32, (v - 0.5) as f32])
    };
    let inverse = |o: [f32; 2]| map(undistort, o);
    let window = bent_window(src.window(), f, |q| map(!undistort, q));
    let mut out = generate(f, window, |x, y| sample_warped(src, x, y, inverse));
    out.carry_planes(src, |x, y| inverse([x as f32, y as f32]));
    Ok(out)
}

/// [`moved_window`] for a map that bends straight lines (a lens), whose
/// extremes needn't be at the corners: points along every edge, rounded out
/// by a pixel.
fn bent_window(window: IRect, format: Size, f: impl Fn([f32; 2]) -> Option<[f32; 2]>) -> IRect {
    if window.is_empty() {
        return window;
    }
    const STEPS: usize = 32;
    let (x0, y0, x1, y1) = (window.x0 as f32 - 1.0, window.y0 as f32 - 1.0, window.x1 as f32, window.y1 as f32);
    let mut bounds = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        for q in [[x, y0], [x, y1], [x0, y], [x1, y]] {
            match f(q).filter(|p| p.iter().all(|v| v.is_finite())) {
                Some([u, v]) => bounds = [bounds[0].min(u), bounds[1].min(v), bounds[2].max(u), bounds[3].max(v)],
                None => return overscan(format),
            }
        }
    }
    let clamp = |v: f32| v.clamp(-1e8, 1e8) as i32;
    IRect::new(clamp(bounds[0].floor()) - 1, clamp(bounds[1].floor()) - 1, clamp(bounds[2].ceil()) + 1, clamp(bounds[3].ceil()) + 1).intersect(&overscan(format))
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// Params every shape op shares.
#[derive(Deserialize)]
struct ShapeStyle {
    width: usize,
    height: usize,
    /// Straight (unassociated) RGBA.
    color: [f32; 4],
    opacity: f32,
    feather: f32,
    invert: bool,
    operation: String,
}

#[derive(Deserialize)]
struct BoxShape {
    center: Pt,
    size: Pt,
    rotate: f32,
}

#[derive(Deserialize)]
struct Polygon {
    points: Vec<f32>,
    fill_rule: String,
}

/// A shape op's outline (and fill rule) from its resolved params.
fn shape_outline(op: &str, params: &Value) -> Result<(Vec<Pt>, FillRule)> {
    Ok(match op {
        "rectangle" | "ellipse" => {
            let b: BoxShape = parse(params, op)?;
            let f = if op == "rectangle" { shape::rectangle } else { shape::ellipse };
            (f(b.center, b.size, b.rotate), FillRule::NonZero)
        }
        _ => {
            let p: Polygon = parse(params, op)?;
            let rule = if p.fill_rule == "even-odd" { FillRule::EvenOdd } else { FillRule::NonZero };
            let outline = if op == "roto" { shape::spline(&p.points) } else { p.points.as_chunks::<2>().0.to_vec() };
            (outline, rule)
        }
    })
}

/// Pixels an outline can touch with a soft edge `feather` wide (centered
/// on it).
fn outline_bounds(outline: &[Pt], feather: f32) -> IRect {
    let pad = (feather.max(1.0) / 2.0).ceil() as i32 + 1;
    let (lo, hi) = outline.iter().filter(|p| p.iter().all(|v| v.is_finite())).fold(
        ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]),
        |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]),
    );
    if lo[0] <= hi[0] {
        IRect::new(lo[0].floor() as i32, lo[1].floor() as i32, hi[0].ceil() as i32, hi[1].ceil() as i32).expand(pad)
    } else {
        IRect::default()
    }
}

fn shape_op(op: &str, params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let style: ShapeStyle = parse(params, op)?;
    let bg = inputs.first().copied().flatten();
    let format = bg.map_or(source_size(style.width, style.height), |b| b.format);
    // One outline (with its feather), or with motion blur one per sample
    // across the shutter, averaged.
    let samples: Vec<(Vec<Pt>, FillRule, f32)> = match params.get("_motion").and_then(Value::as_array) {
        Some(motion) => motion
            .iter()
            .map(|p| {
                let feather = p.get("feather").and_then(Value::as_f64).map_or(style.feather, |f| f as f32);
                shape_outline(op, p).map(|(o, r)| (o, r, feather))
            })
            .collect::<Result<_>>()?,
        None => {
            let (o, r) = shape_outline(op, params)?;
            vec![(o, r, style.feather)]
        }
    };
    // Coverage only where the shape can reach: its outlines' bounds plus
    // the feather, within the frame. Inverted, it covers the rest of the
    // frame.
    let frame = IRect::frame(format);
    let reach = if style.invert {
        frame
    } else {
        samples.iter().fold(IRect::default(), |u, (o, _, f)| u.union(&outline_bounds(o, *f))).intersect(&frame)
    };
    let mut cov = vec![0.0f32; reach.width() * reach.height()];
    let weight = 1.0 / samples.len() as f32;
    for (outline, rule, feather) in &samples {
        let shifted: Vec<Pt> = outline.iter().map(|p| [p[0] - reach.x0 as f32, p[1] - reach.y0 as f32]).collect();
        let c = shape::coverage(&[shifted], reach.width(), reach.height(), *feather, *rule);
        cov.iter_mut().zip(c).for_each(|(a, b)| *a += b * weight);
    }
    let at = |x: i32, y: i32| {
        if reach.contains(x, y) { cov[(y - reach.y0) as usize * reach.width() + (x - reach.x0) as usize] } else { 0.0 }
    };
    shade(at, reach, format, &style, bg)
}

/// Turn shape coverage (`cov(x, y)`, zero outside `reach`) into an image:
/// the shape's color over `bg`, or with the Subtract operation, `bg` held
/// out by the shape.
fn shade(cov: impl Fn(i32, i32) -> f32 + Sync + Send, reach: IRect, format: Size, style: &ShapeStyle, bg: Option<&Image>) -> Result<Image> {
    let subtract = match style.operation.as_str() {
        "over" => false,
        "subtract" => true,
        other => bail!("unknown shape operation '{other}'"),
    };
    let color = color::premultiply(style.color);
    let opacity = style.opacity.clamp(0.0, 1.0);
    let bg_window = bg.map_or(IRect::default(), Image::window);
    // Subtracting only removes from the background.
    let window = if subtract { bg_window } else { bg_window.union(&reach) };
    Ok(generate(format, window, |x, y| {
        let c = if style.invert { 1.0 - cov(x, y) } else { cov(x, y) } * opacity;
        let b = bg.map_or(color::TRANSPARENT, |b| b.at(x, y));
        std::array::from_fn(|k| if subtract { b[k] * (1.0 - c) } else { color[k] * c + b[k] * (1.0 - color[3] * c) })
    }))
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WriteParams {
    path: PathBuf,
    dpx: String,
    dpx_alpha: bool,
}

/// Write input[0] to disk (when the context allows) and pass it through
/// unchanged so writes can sit mid-graph.
fn write(params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Result<Image> {
    let p: WriteParams = parse(params, "write")?;
    let src = input(inputs, 0, "write")?;
    if ctx.write_outputs {
        if p.path.as_os_str().is_empty() {
            bail!("no output file set");
        }
        src.write_as(&media::frame_path(&p.path.to_string_lossy(), ctx.frame), crate::dpx::Options { encoding: crate::dpx::Encoding::from_choice(&p.dpx), alpha: p.dpx_alpha })?;
    }
    Ok(src.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read Splat: the sample capture as a scene, turned up as asked,
    /// placed, faded, its color detail capped; errors that say what's
    /// wrong.
    #[test]
    fn read_splat_places_fades_and_caps_a_capture() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        assert_eq!(output_kind("read_splat"), Kind::Scene);
        let scene = |p: Value| eval("read_splat", &p, &[], &CTX).unwrap().scene.clone().unwrap();
        let s = scene(json!({ "file": file }));
        let g = &s.splats[0];
        assert_eq!((s.splats.len(), g.len(), g.sh_degree), (1, 320, 1));
        let at = |p: Value, v: [f64; 3]| crate::geom::transform_point(&scene(p).splats[0].world, &crate::geom::Vec3::from(v));
        // Up: by default COLMAP's −Y (Z forward) comes out as +Y, as do
        // Z-up's +Z and, as it is, y's; placed after.
        assert!((at(json!({ "file": file }), [0.0, -1.0, 0.0]) - crate::geom::Vec3::y()).norm() < 1e-12 && (at(json!({ "file": file }), [0.0, 0.0, 1.0]) + crate::geom::Vec3::z()).norm() < 1e-12);
        assert_eq!(scene(json!({ "file": file, "up": "y" })).splats[0].world, crate::geom::Mat4::identity());
        assert!((at(json!({ "file": file, "up": "z" }), [0.0, 0.0, 1.0]) - crate::geom::Vec3::y()).norm() < 1e-12);
        assert!((at(json!({ "file": file, "up": "-y" }), [0.0, -1.0, 0.0]) - crate::geom::Vec3::y()).norm() < 1e-12);
        assert!((at(json!({ "file": file, "up": "-y", "translate": [5, 0, 0] }), [0.0, 0.0, 1.0]) - crate::geom::Vec3::new(5.0, 0.0, -1.0)).norm() < 1e-12, "Z forward: toward the default camera");
        // Faded: a copy of the opacities; unfaded: the read ones, shared.
        let faded = scene(json!({ "file": file, "opacity": 0.5 }));
        assert!((faded.splats[0].opacity[0] - g.opacity[0] * 0.5).abs() < 1e-6);
        assert!(std::sync::Arc::ptr_eq(&g.positions, &faded.splats[0].positions) && std::sync::Arc::ptr_eq(&g.opacity, &scene(json!({ "file": file })).splats[0].opacity));
        assert_eq!(scene(json!({ "file": file, "sh_degree": 0 })).splats[0].sh_degree, 0);
        assert!(!g.holdout && scene(json!({ "file": file, "holdout": true })).splats[0].holdout);
        // The sample, upright by default: its ball above the ground.
        let (lo, hi) = (0..g.len()).map(|i| crate::geom::transform_point(&g.world, &crate::geom::Vec3::from(g.positions[i].map(f64::from)))).fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        assert!(lo.abs() < 1e-6 && (hi - 2.0).abs() < 0.01, "{lo} {hi}");
        // The same as .spz (natively Y up), by default, lands where the
        // .ply does.
        let spz = std::env::temp_dir().join(format!("arclight_read_splat_{}.spz", std::process::id()));
        crate::splat::write_spz(&spz, g, 3).unwrap();
        let other = scene(json!({ "file": spz.to_str().unwrap() }));
        let o = &other.splats[0];
        let world = |s: &crate::splat::Splats, i: usize| crate::geom::transform_point(&s.world, &crate::geom::Vec3::from(s.positions[i].map(f64::from)));
        assert!((0..g.len()).all(|i| (world(o, i) - world(g, i)).norm() < 1e-3), "the .spz and the .ply agree");
        assert!((0..g.len()).all(|i| (0..3).all(|k| (o.color(i, [0.0, 0.6, 0.8])[k] - g.color(i, [0.0, 0.6, 0.8])[k]).abs() < 0.01)), "colors too");
        std::fs::remove_file(&spz).ok();
        assert!(eval("read_splat", &json!({}), &[], &CTX).err().unwrap().to_string().contains("no file"));
        assert!(eval("read_splat", &json!({ "file": "/no/such.ply" }), &[], &CTX).is_err());
    }

    /// Splat Crop on the sample (a ball of 256 splats, radius 1 at y 1,
    /// over 64 on the ground): a sphere round the ball keeps just it, a
    /// thin box (turned or not) just the ground; inverted, the rest; what
    /// keeps everything shares it; the rest of the scene passes through.
    #[test]
    fn splat_crop_keeps_whats_inside() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let mut input = eval("read_splat", &json!({ "file": file }), &[], &CTX).unwrap().scene.clone().unwrap();
        std::sync::Arc::make_mut(&mut input).extend(eval("cube", &json!({}), &[None], &CTX).unwrap().scene.as_ref().unwrap());
        let plate = scene_output(DEFAULT_FORMAT, (*input).clone());
        let crop = |p: Value| eval("splat_crop", &p, &[Some(&plate)], &CTX).unwrap().scene.clone().unwrap();
        let count = |p: Value| crop(p).splats[0].len();
        assert_eq!(output_kind("splat_crop"), Kind::Scene);
        let ball = json!({ "shape": "sphere", "center": [0, 1, 0], "size": [2.02, 2.02, 2.02] });
        assert_eq!(count(ball.clone()), 256);
        let mut outside = ball.clone();
        outside["invert"] = json!(true);
        assert_eq!(count(outside), 64);
        assert_eq!(count(json!({ "size": [10, 0.005, 10] })), 64, "a slab at the ground");
        assert_eq!(count(json!({ "size": [10, 10, 0.005], "rotate": [90, 0, 0] })), 64, "the same, turned flat");
        // Kept: what was in the ball, in order, colors and all.
        let kept = crop(ball);
        let g = &input.splats[0];
        assert!(kept.splats[0].positions.iter().zip(g.positions.iter()).all(|(a, b)| a == b) && kept.splats[0].sh_of(255) == g.sh_of(255));
        assert_eq!((kept.meshes.len(), kept.splats[0].node.as_str()), (1, g.node.as_str()), "the rest passes through");
        let all = crop(json!({ "size": [100, 100, 100] }));
        assert!(std::sync::Arc::ptr_eq(&all.splats[0].positions, &g.positions) && std::sync::Arc::ptr_eq(&all.splats[0].sh, &g.sh), "kept whole: shared");
        assert!(eval("splat_crop", &json!({}), &[None], &CTX).unwrap().scene.as_ref().unwrap().splats.is_empty(), "nothing in: nothing out");
        // Its region, for the 3D view: the unit cube or ball onto the box.
        let r = crop(json!({ "shape": "sphere", "center": [1, 2, 3], "size": [2, 4, 6], "rotate": [0, 90, 0] })).regions.clone();
        assert_eq!((r.len(), r[0].sphere, r[0].node.as_str()), (1, true, ""));
        let on = |v: [f64; 3]| crate::geom::transform_point(&r[0].world, &crate::geom::Vec3::from(v));
        assert!((on([0.0; 3]) - crate::geom::Vec3::new(1.0, 2.0, 3.0)).norm() < 1e-9 && (on([0.0, 1.0, 0.0]) - crate::geom::Vec3::new(1.0, 4.0, 3.0)).norm() < 1e-9);
        assert!(((on([1.0, 0.0, 0.0]) - on([0.0; 3])).norm() - 1.0).abs() < 1e-9 && ((on([0.0, 0.0, 1.0]) - on([0.0; 3])).norm() - 3.0).abs() < 1e-9, "half the size each way, turned");
    }

    /// Splat Color on the sample (degree 1: its color changes with the
    /// view): graded exactly from every side, opacity faded and capped,
    /// flattened to degree 0 when asked, shared where nothing changes.
    #[test]
    fn splat_color_grades_from_every_side() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/splats.ply");
        let mut input = eval("read_splat", &json!({ "file": file }), &[], &CTX).unwrap().scene.clone().unwrap();
        std::sync::Arc::make_mut(&mut input).extend(eval("cube", &json!({}), &[None], &CTX).unwrap().scene.as_ref().unwrap());
        let plate = scene_output(DEFAULT_FORMAT, (*input).clone());
        let graded = |p: Value| eval("splat_color", &p, &[Some(&plate)], &CTX).unwrap().scene.clone().unwrap();
        let g = &input.splats[0];
        assert_eq!(output_kind("splat_color"), Kind::Scene);
        let same = graded(json!({}));
        assert!(std::sync::Arc::ptr_eq(&same.splats[0].sh, &g.sh) && std::sync::Arc::ptr_eq(&same.splats[0].opacity, &g.opacity) && same.meshes.len() == 1);
        let dirs = [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.6, 0.0, 0.8]];
        let check = |p: Value, want: &dyn Fn([f32; 3]) -> [f32; 3]| {
            let s = graded(p);
            let mut compared = 0;
            for i in [0, 100, 255, 300] {
                for d in dirs {
                    let (was, now) = (g.color(i, d), s.splats[0].color(i, d));
                    let want = want(was);
                    // (Colors clamp at 0: compare where they don't.)
                    if was.iter().all(|&c| c > 1e-3) && want.iter().all(|&c| c > 1e-3) {
                        assert!(now.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-4), "splat {i} from {d:?}: {now:?} vs {want:?}");
                        compared += 1;
                    }
                }
            }
            assert!(compared >= 8, "only {compared} compared");
        };
        assert!(g.color(0, dirs[0]) != g.color(0, dirs[1]), "the sample's color changes with the view");
        check(json!({ "exposure": 1 }), &|c| c.map(|v| v * 2.0));
        check(json!({ "saturation": 0 }), &|c| [0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]; 3]);
        check(json!({ "gain": [1, 0.5, 0.25, 1], "exposure": -1 }), &|c| [c[0] * 0.5, c[1] * 0.25, c[2] * 0.125]);
        // Opacity: faded, or raised to at most opaque.
        let (half, more) = (graded(json!({ "opacity": 0.5 })), graded(json!({ "opacity": 4 })));
        assert!((half.splats[0].opacity[0] - g.opacity[0] / 2.0).abs() < 1e-6 && more.splats[0].opacity[0] == 1.0);
        // View independent: degree 0, the DC color from every side.
        let flat = graded(json!({ "view_independent": true }));
        let f = &flat.splats[0];
        assert_eq!((f.sh_degree, f.sh.len()), (0, 3 * g.len()));
        assert_eq!(f.color(5, dirs[0]), f.color(5, dirs[1]));
        assert_eq!(f.color(5, dirs[0]), crate::splat::sh_color(g.sh_of(5), 0, dirs[0]));
        assert!(std::sync::Arc::ptr_eq(&f.positions, &g.positions));
    }

    /// Read Geo on the Alembic sample (examples/geo/anim.abc: a cube under
    /// a rig moving from x 0 to 3 over frames 1–10 at 24 fps): read at the
    /// frame's time, shifted by Frame offset and scaled by Frame rate, which
    /// starts as the project's; placed like any Read Geo.
    #[test]
    fn read_geo_reads_alembic_at_the_frames_time() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/geo/anim.abc");
        let cube_x = |p: Value, frame: i64| {
            let ctx = EvalContext { frame, write_outputs: false };
            let s = eval("read_geo", &p, &[], &ctx).unwrap().scene.clone().unwrap();
            assert_eq!(s.meshes.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["rig/box/Cube", "sheet/Grid"]);
            let m = &s.meshes[0];
            let c = m.mesh.positions.iter().sum::<crate::geom::Vec3>() / m.mesh.positions.len() as f64;
            crate::geom::transform_point(&m.world, &c).x
        };
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(cube_x(json!({ "file": file }), 1), 0.0) && close(cube_x(json!({ "file": file }), 10), 3.0));
        assert!(close(cube_x(json!({ "file": file, "frame_offset": -9 }), 10), 0.0), "offset back to frame 1");
        assert!(close(cube_x(json!({ "file": file, "frame_rate": 48 }), 20), 3.0), "at 48 fps, frame 20 is frame 10's time");
        assert!(close(cube_x(json!({ "file": file, "translate": [1, 0, 0] }), 10), 4.0), "placed");
        // A new Read Geo takes the project's frame rate.
        let mut session = crate::session::Session::new(serde_json::from_value(json!({ "project": { "first_frame": 1, "last_frame": 10, "fps": 30, "format": { "width": 64, "height": 32 } }, "nodes": [] })).unwrap());
        let id = session.create_node("read_geo", None, &Map::new(), &[]).unwrap();
        assert_eq!(session.node(&id).unwrap().params["frame_rate"], json!(30.0));
        assert!(format!("{:#}", eval("read_geo", &json!({ "file": "/no/such.abc" }), &[], &CTX).err().unwrap()).contains("can't read"));
    }

    #[test]
    fn read_geo_opacity_fades_the_files_materials() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("arclight_geo_opacity_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.mtl"), "newmtl half\nKd 1 0 0\nd 0.5\n").unwrap();
        std::fs::write(dir.join("t.obj"), "mtllib t.mtl\nusemtl half\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        let file = dir.join("t.obj").to_string_lossy().into_owned();
        let opacity = |p: Value| {
            let img = read_geo(&p, 1).unwrap();
            img.scene.as_ref().unwrap().meshes.iter().map(|m| m.material.opacity).collect::<Vec<_>>()
        };
        // Missing (older projects): the file's own; else times it, with or
        // without Override material.
        assert_eq!(opacity(json!({ "file": file })), [0.5]);
        assert_eq!(opacity(json!({ "file": file, "opacity": 0.5 })), [0.25]);
        assert_eq!(opacity(json!({ "file": file, "opacity": 0.5, "override_material": true, "shading": "unlit" })), [0.25]);
        assert_eq!(opacity(json!({ "file": file, "opacity": 3.0 })), [0.5], "clamped to 1");
        assert!(info("read_geo").unwrap().params.iter().any(|p| p.name == "opacity" && p.default_value() == json!(1.0) && p.animatable));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A 960 × 540 still (white, with a red column at its left edge) in a
    /// 1920 × 1080 project, through a Read and a Media In with each fit.
    #[test]
    fn reads_fit_their_media_to_the_project() {
        use crate::graph::{Evaluator, Graph};
        let dir = std::env::temp_dir().join(format!("arclight_fit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("still.exr");
        let mut still = Image::filled(960, 540, [1.0; 4]);
        for y in 0..540 {
            still.pixels[y * 960] = [1.0, 0.0, 0.0, 1.0];
        }
        still.write(&file).unwrap();
        let path = file.to_string_lossy().to_string();
        let graph = |w: usize, h: usize, fit: Option<&str>| -> Graph {
            let mut read = json!({ "id": "r", "op": "read", "params": { "path": path } });
            let mut media = json!({ "id": "m", "op": "media_in", "params": { "path": path, "first_frame": 1, "last_frame": 1 } });
            if let Some(f) = fit {
                read["params"]["fit"] = json!(f);
                media["params"]["fit"] = json!(f);
            }
            serde_json::from_value(json!({ "project": { "first_frame": 1, "last_frame": 1, "fps": 24, "format": { "width": w, "height": h } }, "nodes": [read, media] })).unwrap()
        };
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let eval = |g: &Graph, n: &str| Evaluator::new().evaluate(g, n, &ctx).unwrap();
        // Saved without the param (an older project): as read.
        for n in ["r", "m"] {
            let img = eval(&graph(1920, 1080, None), n);
            assert_eq!((img.format, img.window()), (Size { width: 960, height: 540 }, IRect::new(0, 0, 960, 540)));
        }
        let img = eval(&graph(1920, 1080, Some("none")), "r");
        assert_eq!(img.format, Size { width: 960, height: 540 });
        // Same aspect: fit, fill, stretch, width and height all double it
        // exactly into the frame.
        for fit in ["fit", "fill", "stretch", "width", "height"] {
            for n in ["r", "m"] {
                let img = eval(&graph(1920, 1080, Some(fit)), n);
                assert_eq!((img.format, img.window()), (Size { width: 1920, height: 1080 }, IRect::new(0, 0, 1920, 1080)), "{fit} {n}");
                // Doubled (bilinear): the red column's first pixel solid red
                // (the edge stays solid, as do the corners), the next a
                // quarter white, then white.
                assert_eq!((img.at(0, 500), img.at(1, 500), img.at(5, 500), img.at(1919, 1079), img.at(1919, 0)), ([1.0, 0.0, 0.0, 1.0], [1.0, 0.25, 0.25, 1.0], [1.0; 4], [1.0; 4], [1.0; 4]), "{fit}");
            }
        }
        // Into a square 1080 × 1080 project: fit has bars above and below,
        // fill crops the sides, stretch squeezes, width matches the width
        // (bars), height the height (cropped).
        let sq = |fit: &str| eval(&graph(1080, 1080, Some(fit)), "r");
        let fit = sq("fit");
        let a = |img: &Image, y: i32| img.at(540, y)[3];
        assert_eq!(fit.format, Size { width: 1080, height: 1080 });
        assert_eq!((a(&fit, 230), a(&fit, 240), a(&fit, 839), a(&fit, 850)), (0.0, 1.0, 1.0, 0.0), "1080 × 607.5 (rows 236–844), transparent bars above and below");
        assert_eq!(fit.at(0, 540), [1.0, 0.0, 0.0, 1.0], "the left edge kept");
        let fill = sq("fill");
        assert_eq!((fill.window(), fill.at(0, 540)), (IRect::new(0, 0, 1080, 1080), [1.0; 4]), "1920 wide, cut to the frame: its left edge cropped off");
        let stretch = sq("stretch");
        assert_eq!((stretch.window(), stretch.at(0, 540), stretch.at(3, 540)), (IRect::new(0, 0, 1080, 1080), [1.0, 0.0, 0.0, 1.0], [1.0; 4]), "squeezed, edge to edge");
        assert_eq!((sq("width").window(), sq("width").pixels == fit.pixels), (fit.window(), true));
        assert_eq!((sq("height").window(), sq("height").pixels == fill.pixels), (fill.window(), true));
        // Changing the project's format re-fits (it's in the cache key).
        let (a, b) = (graph(1920, 1080, Some("fit")), graph(1280, 720, Some("fit")));
        assert_ne!(Evaluator::key(&a, "r", &ctx).unwrap(), Evaluator::key(&b, "r", &ctx).unwrap());
        assert_eq!(Evaluator::key(&graph(1920, 1080, None), "r", &ctx).unwrap(), Evaluator::key(&graph(1280, 720, None), "r", &ctx).unwrap(), "none doesn't care");
        let mut ev = Evaluator::new();
        assert_eq!(ev.evaluate(&a, "m", &ctx).unwrap().format, Size { width: 1920, height: 1080 });
        assert_eq!(ev.evaluate(&b, "m", &ctx).unwrap().format, Size { width: 1280, height: 720 });
        // New nodes fit; an old one loaded keeps none.
        let mut s = crate::session::Session::new(graph(1920, 1080, None));
        let id = s.create_node("read", None, json!({ "path": path }).as_object().unwrap(), &[]).unwrap();
        assert_eq!((s.graph.node(&id).unwrap().params["fit"].as_str(), s.graph.node("r").unwrap().params.get("fit")), (Some("fit"), None));
        assert_eq!(s.render(&id, 1).unwrap().format, Size { width: 1920, height: 1080 });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A video with an upper-case extension (as cameras name them) reads
    /// through Read and Media In alike, frame by frame, holding outside.
    #[test]
    fn read_and_media_in_read_videos_in_either_case() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/media/testsrc.mp4");
        let dir = std::env::temp_dir().join(format!("arclight_mp4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("CLIP.MP4");
        std::fs::copy(&src, &clip).unwrap();
        let path = clip.to_string_lossy().to_string();
        assert!(is_time_varying("read", &json!({ "path": path })));
        let at = |op: &str, params: Value, frame: i64| eval_op(op, &params, &[], &EvalContext { frame, write_outputs: false }).unwrap();
        let read = |frame| at("read", json!({ "path": path }), frame);
        let media = |frame| at("media_in", json!({ "path": path, "first_frame": 1, "last_frame": 120 }), frame);
        let (r1, m1) = (read(1), media(1));
        assert_eq!((r1.format, r1.pixels == m1.pixels), (media::probe_video(&clip).map(|v| Size { width: v.width, height: v.height }).unwrap(), true));
        let (r30, m30) = (read(30), media(30));
        assert!(r30.pixels == m30.pixels && r30.pixels != r1.pixels, "it moves, in step with Media In");
        assert!(read(0).pixels == r1.pixels && read(500).pixels == media(500).pixels, "holds outside the clip");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The user's 4K camera clip, when it's there (it isn't in the repo).
    #[test]
    fn reads_a_4k_camera_clip_if_present() {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/media/musicmen.MP4");
        if !clip.exists() {
            return;
        }
        let path = clip.to_string_lossy().to_string();
        for op in ["read", "media_in"] {
            let img = eval_op(op, &json!({ "path": path, "first_frame": 1, "last_frame": 132 }), &[], &CTX).unwrap();
            assert_eq!(img.format, Size { width: 3840, height: 2160 }, "{op}");
            assert!(img.pixels.iter().any(|p| p[0] > 0.01), "{op}: a picture, not black");
        }
    }

    const CTX: EvalContext = EvalContext { frame: 1, write_outputs: false };

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn solid(w: usize, h: usize, c: Rgba) -> Image {
        Image::filled(w, h, c)
    }

    #[test]
    fn masked_ops_declare_mask_port_and_mix() {
        for op in OPS.iter().filter(|o| o.mask_base.is_some()) {
            let last = op.inputs.last().unwrap();
            assert!(last.label == "mask" && last.optional, "{}: last port must be the mask", op.name);
            assert!(op.mask_base.unwrap() < op.inputs.len() - 1, "{}", op.name);
            assert!(op.params.iter().any(|p| p.name == "mix"), "{}: needs mix", op.name);
        }
    }

    #[test]
    fn mask_and_mix_limit_an_effect() {
        let src = solid(4, 1, [1.0, 1.0, 1.0, 1.0]);
        // Mask: alpha 1 on the left half, 0 on the right.
        let mut mask = Image::new(4, 1);
        mask.set(0, 0, [1.0; 4]);
        mask.set(1, 0, [1.0; 4]);
        let params = json!({ "multiply": [0, 0, 0], "mix": 0.5 });
        let out = eval("grade", &params, &[Some(&src), Some(&mask)], &CTX).unwrap();
        assert!(approx(out.get(0, 0)[0], 0.5), "masked + half mix");
        assert!(approx(out.get(3, 0)[0], 1.0), "outside mask untouched");
        // No mask: mix applies everywhere.
        let out = eval("grade", &params, &[Some(&src), None], &CTX).unwrap();
        assert!(approx(out.get(3, 0)[0], 0.5));
    }

    #[test]
    fn merge_mask_falls_back_to_b() {
        let a = solid(2, 1, [1.0, 0.0, 0.0, 1.0]);
        let b = solid(2, 1, [0.0, 0.0, 1.0, 1.0]);
        let mut mask = Image::new(2, 1);
        mask.set(0, 0, [1.0; 4]);
        let out = eval("merge", &json!({}), &[Some(&a), Some(&b), Some(&mask)], &CTX).unwrap();
        assert_eq!(out.get(0, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(out.get(1, 0), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn shapes_draw_on_their_own_canvas_or_over_a_background() {
        let params = json!({ "width": 20, "height": 10, "center": [10, 5], "size": [4, 4], "color": [1, 0, 0, 0.5] });
        let out = eval("rectangle", &params, &[None], &CTX).unwrap();
        assert_eq!((out.format.width, out.format.height), (20, 10));
        assert_eq!(out.at(10, 5), [0.5, 0.0, 0.0, 0.5], "premultiplied color");
        assert_eq!(out.at(0, 0), color::TRANSPARENT);
        assert_eq!(out.window(), IRect::new(6, 1, 14, 9), "only the shape's bounds are stored");

        let bg = solid(8, 8, [0.0, 0.0, 1.0, 1.0]);
        let params = json!({ "center": [4, 4], "size": [4, 4], "color": [1, 0, 0, 0.5] });
        let out = eval("ellipse", &params, &[Some(&bg)], &CTX).unwrap();
        assert_eq!((out.format.width, out.format.height), (8, 8), "background sets the canvas");
        assert_eq!(out.at(0, 0), [0.0, 0.0, 1.0, 1.0]);
        let c = out.at(4, 4);
        assert!(approx(c[0], 0.5) && approx(c[2], 0.5) && approx(c[3], 1.0), "{c:?}");

        let cut = eval("rectangle", &json!({ "center": [4, 4], "size": [2, 2], "operation": "subtract" }), &[Some(&bg)], &CTX).unwrap();
        assert_eq!(cut.at(4, 4), color::TRANSPARENT);
        assert_eq!(cut.at(0, 0), [0.0, 0.0, 1.0, 1.0]);

        let inv = eval("rectangle", &json!({ "width": 8, "height": 8, "center": [4, 4], "size": [2, 2], "invert": true }), &[None], &CTX).unwrap();
        assert_eq!((inv.at(4, 4)[3], inv.at(0, 0)[3]), (0.0, 1.0));
    }

    /// A `w × h` patch of `px` at (x, y) in a `fw × fh` frame.
    fn patch(fw: usize, fh: usize, x: i32, y: i32, w: usize, h: usize, px: Rgba) -> Image {
        let mut img = Image::blank(Size { width: fw, height: fh }, IRect::new(x, y, x + w as i32, y + h as i32));
        img.pixels.iter_mut().for_each(|p| *p = px);
        img
    }

    #[test]
    fn shape_motion_blur_smears_over_the_shutter() {
        // A 10×10 square moving 20 px a frame to the right.
        let params = |mb: i64, offset: &str| json!({
            "width": 100, "height": 40, "size": [10, 10], "motion_blur": mb, "shutter": 1.0, "shutter_offset": offset,
            "center": { "keys": [[0, [20, 20]], [10, [220, 20]]], "interp": "linear" },
        });
        let render = |p: &Value| {
            let r = resolve_params("rectangle", p, 2);
            eval("rectangle", &r, &[None], &CTX).unwrap()
        };
        let alpha = |img: &Image, x: i32| img.at(x, 20)[3];
        let total = |img: &Image| img.flat().pixels.iter().map(|p| p[3]).sum::<f32>();
        // Off: a sharp square at x 55–65 (frame 2).
        let sharp = render(&params(0, "centered"));
        assert!(approx(alpha(&sharp, 60), 1.0) && approx(alpha(&sharp, 50), 0.0) && approx(alpha(&sharp, 70), 0.0));
        // Centered: spread over 45–75. It moves twice its width while the
        // shutter is open, so the middle is covered half the time; the ends
        // fainter; the total coverage kept.
        let blurred = render(&params(32, "centered"));
        assert!((alpha(&blurred, 60) - 0.5).abs() < 0.05, "{}", alpha(&blurred, 60));
        assert!(alpha(&blurred, 47) > 0.0 && alpha(&blurred, 47) < 0.3, "{}", alpha(&blurred, 47));
        assert!(alpha(&blurred, 72) > 0.0 && alpha(&blurred, 72) < 0.3);
        assert!((total(&blurred) - total(&sharp)).abs() < 2.0, "{} {}", total(&blurred), total(&sharp));
        // Starting at the frame: the smear trails ahead, to 85.
        let ahead = render(&params(32, "start"));
        assert!(alpha(&ahead, 50) == 0.0 && alpha(&ahead, 80) > 0.0);
        // Nothing animated: no samples, no blur.
        let still = json!({ "width": 100, "height": 40, "size": [10, 10], "center": [60, 20], "motion_blur": 32 });
        assert!(resolve_params("rectangle", &still, 2).get("_motion").is_none());
    }

    #[test]
    fn transform_and_corner_pin_motion_blur_average_the_move() {
        // A 10×10 square at x 15–25, moved 20 px a frame (40 px at frame 2).
        let sq = patch(100, 40, 15, 15, 10, 10, [1.0; 4]);
        let ctx = EvalContext { frame: 2, write_outputs: false };
        let moving = |x: i64| json!({ "keys": [[0, [0, 0]], [10, [x, 0]]], "interp": "linear" });
        let pin = |mb: i64| {
            let corner = |x: f64, y: f64| json!({ "keys": [[0, [x, y]], [10, [x + 200.0, y]]], "interp": "linear" });
            json!({ "from1": [0, 0], "from2": [100, 0], "from3": [100, 40], "from4": [0, 40],
                    "to1": corner(0.0, 0.0), "to2": corner(100.0, 0.0), "to3": corner(100.0, 40.0), "to4": corner(0.0, 40.0),
                    "motion_blur": mb, "shutter": 1.0 })
        };
        let run = |op: &str, p: Value| eval(op, &resolve_params(op, &p, 2), &[Some(&sq), None], &ctx).unwrap();
        let total = |img: &Image| img.flat().pixels.iter().map(|p| p[3]).sum::<f32>();
        for (op, sharp, blurred) in [
            ("transform", run("transform", json!({ "translate": moving(200) })), run("transform", json!({ "translate": moving(200), "motion_blur": 32, "shutter": 1.0 }))),
            ("corner_pin", run("corner_pin", pin(0)), run("corner_pin", pin(32))),
        ] {
            // Sharp: the square at x 55–65. Blurred over a frame (20 px):
            // spread over 45–75, its middle covered half the time, the
            // total coverage the same.
            assert!(approx(sharp.at(60, 20)[3], 1.0) && approx(sharp.at(50, 20)[3], 0.0), "{op}");
            assert!((blurred.at(60, 20)[3] - 0.5).abs() < 0.06, "{op}: {}", blurred.at(60, 20)[3]);
            assert!(blurred.at(47, 20)[3] > 0.0 && blurred.at(72, 20)[3] > 0.0, "{op}");
            assert!((total(&blurred) - total(&sharp)).abs() < 2.0, "{op}: {} {}", total(&blurred), total(&sharp));
            assert!(blurred.window().x0 <= 45 && blurred.window().x1 >= 75, "{op}: {:?}", blurred.window());
        }
    }

    #[test]
    fn keyer_matte_shrinks_grows_and_softens() {
        // A 10×10 subject at x 15–25 on a 40×20 green screen.
        let mut plate = solid(40, 20, [0.1, 0.6, 0.1, 1.0]);
        for y in 5..15 {
            for x in 15..25 {
                plate.pixels[y * 40 + x] = [0.5, 0.4, 0.3, 1.0];
            }
        }
        let matte = |extra: Value| {
            let mut p = json!({ "output": "matte", "screen": [0.1, 0.6, 0.1, 1.0] });
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let img = eval("keyer", &p, &[Some(&plate), None, None], &CTX).unwrap();
            (0..40).map(|x| img.at(x, 10)[0]).collect::<Vec<f32>>()
        };
        let row = matte(json!({}));
        assert_eq!((row[14], row[15], row[24], row[25]), (0.0, 1.0, 1.0, 0.0));
        let grown = matte(json!({ "shrink_grow": 2 }));
        assert_eq!((grown[12], grown[13], grown[26], grown[27]), (0.0, 1.0, 1.0, 0.0));
        let shrunk = matte(json!({ "shrink_grow": -2 }));
        assert_eq!((shrunk[16], shrunk[17], shrunk[22], shrunk[23]), (0.0, 1.0, 1.0, 0.0));
        let half = matte(json!({ "shrink_grow": 0.5 }));
        assert!(approx(half[14], 0.5) && approx(half[13], 0.0) && approx(half[15], 1.0));
        // Softness: a ramp across the edge, the middle still solid.
        let soft = matte(json!({ "softness": 2 }));
        assert!(soft[15] > 0.3 && soft[15] < 0.8 && soft[13] < soft[15] && approx(soft[20], 1.0), "{soft:?}");
        assert!((soft.iter().sum::<f32>() - 10.0).abs() < 0.1, "softness keeps the matte's area");
        // Keyed output follows the matte; the frame's edges aren't eaten.
        let edge = eval("keyer", &json!({ "screen": [0.1, 0.6, 0.1, 1.0], "shrink_grow": -3, "softness": 1 }), &[Some(&solid(10, 10, [0.5, 0.4, 0.3, 1.0])), None, None], &CTX).unwrap();
        assert!(approx(edge.at(0, 0)[3], 1.0), "{:?}", edge.at(0, 0));
    }

    #[test]
    fn keyer_pulls_a_matte_and_despills() {
        // A green screen with a neutral subject in a 4×4 square, and a
        // half-and-half edge pixel at (0, 0).
        let screen = [0.1, 0.6, 0.1, 1.0];
        let subject = [0.5, 0.4, 0.3, 1.0];
        let mut plate = solid(8, 8, screen);
        for y in 2..6 {
            for x in 2..6 {
                plate.pixels[y * 8 + x] = subject;
            }
        }
        plate.pixels[0] = std::array::from_fn(|i| (screen[i] + subject[i]) / 2.0);
        let close = |a: Rgba, b: Rgba| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4);
        // The screen is found on its own, with no subject pixels in it.
        assert!(close(estimate_screen(&plate), screen), "{:?}", estimate_screen(&plate));
        let key = |params: Value| eval("keyer", &params, &[Some(&plate), None, None], &CTX).unwrap();
        let keyed = key(json!({}));
        assert!(close(keyed.at(3, 3), subject), "subject kept: {:?}", keyed.at(3, 3));
        assert!(close(keyed.at(7, 7), [0.0; 4]), "screen gone: {:?}", keyed.at(7, 7));
        // The edge: half transparent, and despill takes the screen's green
        // out (down to the mean of red and blue), premultiplied.
        let edge = keyed.at(0, 0);
        assert!((edge[3] - 0.5).abs() < 1e-4, "{edge:?}");
        assert!(close(edge, [0.15, 0.125, 0.1, 0.5]), "{edge:?}");
        // Matte output, and clipping.
        assert!(close(key(json!({ "output": "matte" })).at(0, 0), [0.5, 0.5, 0.5, 1.0]));
        assert!(close(key(json!({ "output": "matte", "clip_white": 0.5 })).at(0, 0), [1.0, 1.0, 1.0, 1.0]));
        assert!(close(key(json!({ "output": "matte", "clip_black": 0.5 })).at(0, 0), [0.0, 0.0, 0.0, 1.0]));
        // A clean plate of a darker screen keys that instead.
        let dark = solid(8, 8, [0.05, 0.3, 0.05, 1.0]);
        let mut dim = plate.clone();
        dim.pixels[63] = [0.05, 0.3, 0.05, 1.0];
        let alpha = |img: &Image| img.at(7, 7)[3];
        let without = eval("keyer", &json!({}), &[Some(&dim), None, None], &CTX).unwrap();
        let with = eval("keyer", &json!({}), &[Some(&dim), Some(&dark), None], &CTX).unwrap();
        assert!(alpha(&without) > 0.4 && alpha(&with) < 1e-4, "{} {}", alpha(&without), alpha(&with));
    }

    #[test]
    fn small_layers_keep_small_windows() {
        let red = patch(100, 50, 10, 10, 4, 4, [1.0, 0.0, 0.0, 1.0]);
        let blue = patch(80, 40, 60, 20, 5, 5, [0.0, 0.0, 1.0, 1.0]);
        // Merge: B's frame, holding both.
        let m = eval("merge", &json!({}), &[Some(&red), Some(&blue), None], &CTX).unwrap();
        assert_eq!((m.format, m.window()), (blue.format, IRect::new(10, 10, 65, 25)));
        assert_eq!((m.at(11, 11), m.at(61, 21), m.at(30, 15)), ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], color::TRANSPARENT));
        // Color ops keep the window unless transparent black stops being so.
        let rgb = eval("invert", &json!({ "channels": "rgb" }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!(rgb.window(), red.window());
        let alpha = eval("invert", &json!({ "channels": "alpha" }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!((alpha.window(), alpha.at(0, 0)[3], alpha.at(11, 11)[3]), (red.frame(), 1.0, 0.0));
        let lifted = eval("clamp", &json!({ "min": 0.2 }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!(lifted.window(), red.frame());
        // A constant-1 channel fills the frame.
        let cs = eval("channel_select", &json!({ "alpha": "one" }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!((cs.window(), cs.at(90, 40)[3]), (red.frame(), 1.0));
        // Mask/mix: the result where the mask is, the base elsewhere.
        let mask = patch(100, 50, 12, 10, 2, 4, [1.0; 4]);
        let graded = eval("exposure", &json!({ "stops": 1 }), &[Some(&red), Some(&mask)], &CTX).unwrap();
        assert_eq!((graded.at(10, 11)[0], graded.at(12, 11)[0]), (1.0, 2.0));
        // Shrinking the canvas moves the window with the new origin.
        let c = eval("crop", &json!({ "left": 8, "top": 5, "mode": "shrink canvas" }), &[Some(&red)], &CTX).unwrap();
        assert_eq!((c.format, c.window()), (Size { width: 92, height: 45 }, IRect::new(2, 5, 6, 9)));
    }

    /// Extra planes (a 3D render's depth) move with the pixels they
    /// belong to, stay put through color ops, and take the front input's
    /// value in a merge.
    #[test]
    fn planes_follow_their_pixels() {
        // A 4×4 square at (10, 10) with depth 5, in a 100×50 frame.
        let with_depth = |img: Image, d: f32| {
            let mut img = img;
            let n = img.pixels.len();
            *img.plane_mut("depth") = img.pixels.iter().map(|p| if p[3] > 0.0 { d } else { 0.0 }).take(n).collect();
            img
        };
        let sq = with_depth(patch(100, 50, 10, 10, 4, 4, [1.0; 4]), 5.0);
        let run = |op: &str, p: Value, inputs: &[Option<&Image>]| eval(op, &p, inputs, &CTX).unwrap();
        // Moved 20 px right: the depth goes along, nearest, not blended.
        let moved = run("transform", json!({ "translate": [20.5, 0] }), &[Some(&sq), None]);
        assert_eq!((moved.plane_at("depth", 31, 11), moved.plane_at("depth", 11, 11)), (5.0, 0.0));
        assert!(moved.plane("depth").unwrap().data.iter().all(|&d| d == 0.0 || d == 5.0));
        let pin = json!({ "from1": [0, 0], "from2": [100, 0], "from3": [100, 50], "from4": [0, 50],
                          "to1": [20, 5], "to2": [120, 5], "to3": [120, 55], "to4": [20, 55] });
        assert_eq!(run("corner_pin", pin, &[Some(&sq), None]).plane_at("depth", 31, 16), 5.0);
        // Color ops (masked too) keep it; a merge over a far plate keeps
        // the square's depth where it covers and the plate's elsewhere.
        assert_eq!(run("grade", json!({ "multiply": [0.5, 0.5, 0.5] }), &[Some(&sq), None]).plane_at("depth", 11, 11), 5.0);
        let mut mask = Image::new(100, 50);
        mask.set(11, 11, [1.0; 4]);
        assert_eq!(run("exposure", json!({ "stops": 1, "mix": 0.5 }), &[Some(&sq), Some(&mask)]).plane_at("depth", 12, 12), 5.0);
        let plate = with_depth(solid(100, 50, [0.2, 0.2, 0.2, 1.0]), 100.0);
        let over = run("merge", json!({}), &[Some(&sq), Some(&plate), None]);
        assert_eq!((over.plane_at("depth", 11, 11), over.plane_at("depth", 50, 30)), (5.0, 100.0));
        let under = run("merge", json!({ "operation": "under" }), &[Some(&sq), Some(&plate), None]);
        assert_eq!(under.plane_at("depth", 11, 11), 100.0, "under: B in front");
        // Crop to a new canvas and resize move it like the pixels.
        let cropped = run("crop", json!({ "left": 10, "top": 10, "mode": "shrink canvas" }), &[Some(&sq), None]);
        assert_eq!((cropped.plane_at("depth", 1, 1), cropped.plane_at("depth", 5, 5)), (5.0, 0.0));
        let half = run("resize", json!({ "width": 50, "height": 25, "fit": "stretch" }), &[Some(&sq), None]);
        assert_eq!((half.plane_at("depth", 6, 6), half.plane_at("depth", 2, 2)), (5.0, 0.0));
        // Motion blur: the depth of the middle of the shutter.
        let p = json!({ "translate": { "keys": [[0, [0, 0]], [10, [200, 0]]], "interp": "linear" }, "motion_blur": 8, "shutter": 1.0 });
        let blurred = eval("transform", &resolve_params("transform", &p, 1), &[Some(&sq), None], &CTX).unwrap();
        assert_eq!((blurred.plane_at("depth", 31, 11), blurred.plane_at("depth", 11, 11)), (5.0, 0.0));
        // A source drops nothing it never had; an op that can't know keeps none.
        assert!(run("constant", json!({}), &[]).planes.is_empty());
    }

    #[test]
    fn lens_distort_moves_points_radially_and_undoes_itself() {
        let ramp = generate(Size { width: 200, height: 100 }, IRect::new(0, 0, 200, 100), |x, y| [x as f32 / 200.0, y as f32 / 100.0, 0.5, 1.0]);
        let run = |img: &Image, p: Value| eval("lens_distort", &p, &[Some(img), None], &CTX).unwrap();
        // No distortion: the same pixels.
        let same = run(&ramp, json!({ "k1": 0, "k2": 0 }));
        assert_eq!(same.flat().pixels, ramp.pixels);
        // A dot 90.5 px right of the center (r = 0.905 half-widths), with
        // K1 = 0.1, lands at 100 + 90.5 × (1 + 0.1 × 0.905²) ≈ 197.9.
        let dot = patch(200, 100, 190, 50, 1, 1, [1.0; 4]);
        let bent = run(&dot, json!({ "mode": "distort", "k1": 0.1 }));
        let brightest = (150..220).max_by(|&a, &b| bent.at(a, 50)[3].total_cmp(&bent.at(b, 50)[3])).unwrap();
        assert!((197..=198).contains(&brightest), "{brightest}");
        // Undistort after distort (and the other way) gets the image back,
        // to within resampling, where it stays in the frame.
        for k1 in [0.08, -0.08] {
            let back = run(&run(&ramp, json!({ "mode": "distort", "k1": k1, "k2": 0.01 })), json!({ "mode": "undistort", "k1": k1, "k2": 0.01 }));
            for (x, y) in [(100, 50), (30, 20), (170, 80), (10, 50)] {
                let (a, b) = (back.at(x, y), ramp.at(x, y));
                assert!((0..4).all(|c| (a[c] - b[c]).abs() < 0.01), "{k1} at {x},{y}: {a:?} vs {b:?}");
            }
        }
        // Undistorting barrel distortion pushes the corners out: kept as
        // overscan. Planes go along.
        let mut plate = ramp.clone();
        *plate.plane_mut("depth") = vec![3.0; 200 * 100];
        let flat = run(&plate, json!({ "k1": -0.1 }));
        assert!(flat.window().x0 < 0 && flat.window().x1 > 200, "{:?}", flat.window());
        assert_eq!(flat.plane_at("depth", 100, 50), 3.0);
    }

    #[test]
    fn moved_layers_keep_pixels_past_the_frame() {
        let red = patch(100, 50, 10, 10, 4, 4, [1.0, 0.0, 0.0, 1.0]);
        // Out of the frame to the right, and back: nothing lost.
        let out = eval("transform", &json!({ "translate": [150, 0] }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!((out.at(161, 11), out.flat().pixels.iter().map(|p| p[3]).sum::<f32>()), ([1.0, 0.0, 0.0, 1.0], 0.0));
        let back = eval("transform", &json!({ "translate": [-150, 0] }), &[Some(&out), None], &CTX).unwrap();
        assert_eq!(back.at(11, 11), [1.0, 0.0, 0.0, 1.0]);
        assert!(back.window().width() < 10, "{:?}", back.window());
        // A huge scale stops at the overscan: one frame past each edge (and
        // an off-center layer scaled clean past it keeps nothing).
        let mid = patch(100, 50, 48, 23, 4, 4, [1.0; 4]);
        let big = eval("transform", &json!({ "scale": [500, 500] }), &[Some(&mid), None], &CTX).unwrap();
        assert_eq!(big.window(), IRect::new(-100, -50, 200, 100));
        let gone = eval("transform", &json!({ "scale": [500, 500], "center": [50, 25] }), &[Some(&red), None], &CTX).unwrap();
        assert!(gone.window().is_empty());
        // The tracker's match-move and a corner pin move windows the same way.
        let pin = json!({ "from1": [0, 0], "from2": [100, 0], "from3": [100, 50], "from4": [0, 50],
                          "to1": [20, 5], "to2": [120, 5], "to3": [120, 55], "to4": [20, 55] });
        let pinned = eval("corner_pin", &pin, &[Some(&red), None], &CTX).unwrap();
        assert_eq!(pinned.at(31, 16), [1.0, 0.0, 0.0, 1.0]);
        assert!(pinned.window().width() < 10, "{:?}", pinned.window());
    }

    #[test]
    fn transform_pivots_on_its_layer_unless_given_a_center() {
        // A 4×4 layer at (10, 10): auto pivots on its middle, (12, 12).
        let red = patch(100, 50, 10, 10, 4, 4, [1.0, 0.0, 0.0, 1.0]);
        let turned = eval("transform", &json!({ "rotate": 90, "scale": [2, 2] }), &[Some(&red), None], &CTX).unwrap();
        let flat = turned.flat();
        let (mut sum, mut at) = (0.0, [0.0f32; 2]);
        for (i, p) in flat.pixels.iter().enumerate() {
            sum += p[3];
            at[0] += p[3] * (i % 100) as f32;
            at[1] += p[3] * (i / 100) as f32;
        }
        let centroid = [at[0] / sum, at[1] / sum];
        // Four times the coverage, centered where it was (pixel centers are
        // on integers here, so the middle of 10..14 is 11.5).
        assert!((sum - 64.0).abs() < 1.0 && centroid.iter().all(|c| (c - 11.5).abs() < 0.05), "{sum} {centroid:?}");
        assert_eq!(turned.at(12, 12), [1.0, 0.0, 0.0, 1.0]);
        // An explicit center: the frame's middle.
        let framed = eval("transform", &json!({ "scale": [2, 2], "center": [50, 25] }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!(framed.at(12, 12)[3], 0.0);
        assert_eq!(framed.at(-24, 1), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn blur_spreads_a_layer_but_keeps_a_plate_s_edges() {
        let red = patch(100, 50, 40, 20, 4, 4, [1.0, 0.0, 0.0, 1.0]);
        let soft = eval("blur", &json!({ "radius": 2, "passes": 3 }), &[Some(&red), None], &CTX).unwrap();
        assert_eq!(soft.window(), IRect::new(34, 14, 50, 30));
        let total = |img: &Image| img.flat().pixels.iter().map(|p| p[3]).sum::<f32>();
        assert!((total(&soft) - 16.0).abs() < 1e-3, "blur keeps the total: {}", total(&soft));
        // A full-frame plate: same window, edges not darkened.
        let plate = solid(20, 10, [0.5, 0.5, 0.5, 1.0]);
        let soft = eval("blur", &json!({ "radius": 2 }), &[Some(&plate), None], &CTX).unwrap();
        assert_eq!((soft.window(), soft.at(0, 0)), (plate.window(), [0.5, 0.5, 0.5, 1.0]));
        // Resize scales the window along with the frame.
        let half = eval("resize", &json!({ "width": 50, "height": 25 }), &[Some(&red)], &CTX).unwrap();
        assert_eq!(half.format, Size { width: 50, height: 25 });
        assert!(half.window().width() <= 6 && half.at(21, 11)[3] > 0.9, "{:?}", half.window());
    }

    #[test]
    fn a_shape_merged_over_a_background_matches_one_drawn_on_it() {
        let bg = eval("checkerboard", &json!({ "width": 64, "height": 48, "size": 8 }), &[], &CTX).unwrap();
        let p = json!({ "width": 64, "height": 48, "center": [30, 20], "size": [20, 12], "rotate": 20, "feather": 3, "color": [1, 0.5, 0, 0.8] });
        let alone = eval("rectangle", &p, &[None], &CTX).unwrap();
        assert!(alone.window().width() < 40, "{:?}", alone.window());
        let merged = eval("merge", &json!({}), &[Some(&alone), Some(&bg), None], &CTX).unwrap();
        let drawn = eval("rectangle", &p, &[Some(&bg)], &CTX).unwrap();
        let (a, b) = (merged.flat(), drawn.flat());
        let diff = a.pixels.iter().zip(&b.pixels).flat_map(|(x, y)| (0..4).map(move |c| (x[c] - y[c]).abs())).fold(0.0, f32::max);
        assert!(diff < 1e-5, "off by {diff}");
    }

    #[test]
    fn polygon_points_animate_as_one_value() {
        let params = json!({
            "width": 10, "height": 10,
            "points": { "keys": [[1, [0, 0, 4, 0, 4, 4, 0, 4]], [11, [6, 6, 10, 6, 10, 10, 6, 10]]], "interp": "linear" },
        });
        let first = eval("polygon", &resolve_params("polygon", &params, 1), &[None], &CTX).unwrap();
        let last = eval("polygon", &resolve_params("polygon", &params, 11), &[None], &CTX).unwrap();
        assert_eq!((first.at(2, 2)[3], first.at(8, 8)[3]), (1.0, 0.0));
        assert_eq!((last.at(2, 2)[3], last.at(8, 8)[3]), (0.0, 1.0));
    }

    #[test]
    fn roto_draws_a_smooth_closed_curve() {
        let out = eval("roto", &json!({}), &[None], &CTX).unwrap();
        // Size 0: the default format, outside a graph.
        assert_eq!(out.format, DEFAULT_FORMAT);
        assert_eq!(out.at(256, 256)[3], 1.0);
        // The default circle has radius 120: a square's corner at 45° would be inside, the circle's isn't.
        assert_eq!(out.at(256 + 100, 256 + 100)[3], 0.0);
        assert_eq!(out.at(256 + 80, 256 + 80)[3], 1.0);
        let area: f32 = out.pixels.iter().map(|p| p[3]).sum();
        let exact = std::f32::consts::PI * 120.0 * 120.0;
        assert!((area - exact).abs() / exact < 0.003, "{area} vs {exact}");
    }

    #[test]
    fn tracker_match_moves_and_stabilizes_relative_to_the_reference_frame() {
        let mut src = Image::new(20, 20);
        src.set(5, 5, [1.0, 1.0, 1.0, 1.0]);
        // One track moving 3 px right and 2 down from frame 1 to frame 5.
        let params = json!({
            "track1": { "keys": [[1, [5.5, 5.5]], [5, [8.5, 7.5]]], "interp": "linear" },
            "output": "match-move", "reference_frame": 1,
        });
        let at = |f: i64, params: &Value| eval("tracker", &resolve_params("tracker", params, f), &[Some(&src)], &CTX).unwrap();
        assert_eq!(at(1, &params).at(5, 5)[3], 1.0, "identity at the reference frame");
        let moved = at(5, &params);
        assert_eq!((moved.at(8, 7)[3], moved.at(5, 5)[3]), (1.0, 0.0));
        let mut params = params;
        params["output"] = json!("stabilize");
        let steady = at(5, &params);
        assert_eq!((steady.at(2, 3)[3]), 1.0, "moved back by the track's motion");
        params["output"] = json!("input");
        assert_eq!(at(5, &params).at(5, 5)[3], 1.0);
    }

    #[test]
    fn tracker_motion_blur_matches_a_blurred_exported_transform() {
        // A 4×4 square, and one track moving 10 px a frame to the right.
        let src = patch(80, 20, 8, 8, 4, 4, [1.0; 4]);
        let ctx = EvalContext { frame: 3, write_outputs: false };
        let mut params = json!({
            "track1": { "keys": [[1, [10.0, 10.0]], [5, [50.0, 10.0]]], "interp": "linear" },
            "output": "match-move", "reference_frame": 1, "motion_blur": 16, "shutter": 1.0,
        });
        let run = |op: &str, p: &Value| eval(op, &resolve_params(op, p, 3), &[Some(&src), None], &ctx).unwrap();
        let blurred = run("tracker", &params);
        let total = |img: &Image| img.flat().pixels.iter().map(|p| p[3]).sum::<f32>();
        // Moved 20 px by frame 3, smeared 10 px wide: its middle half covered.
        assert!((blurred.at(30, 10)[3] - 0.4).abs() < 0.06, "{}", blurred.at(30, 10)[3]);
        assert!((total(&blurred) - 16.0).abs() < 0.5, "{}", total(&blurred));
        let mut xf = Value::Object(track::transform_params(&params, [80, 20], false).unwrap());
        xf["motion_blur"] = json!(16);
        xf["shutter"] = json!(1.0);
        let exported = run("transform", &xf);
        let diff = blurred.flat().pixels.iter().zip(&exported.flat().pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(diff < 2e-3, "off by {diff}");
        // Passing its input through, there's nothing to sample.
        params["output"] = json!("input");
        assert!(resolve_params("tracker", &params, 3).get("_motion").is_none());
    }

    #[test]
    fn exported_transform_matches_the_tracker_output() {
        let mut src = Image::new(40, 30);
        for (i, px) in src.pixels.iter_mut().enumerate() {
            let (x, y) = ((i % 40) as f32, (i / 40) as f32);
            *px = [x / 40.0, y / 30.0, ((x * 0.7).sin() * (y * 0.5).cos()).abs(), 1.0];
        }
        // Two tracks that move, turn and grow between frames 1 and 5.
        let mut params = json!({
            "track1": { "keys": [[1, [10.5, 10.5]], [5, [14.5, 12.5]]], "interp": "linear" },
            "track2_on": true,
            "track2": { "keys": [[1, [30.5, 10.5]], [5, [33.0, 18.0]]], "interp": "linear" },
            "reference_frame": 1,
        });
        for (output, stabilize) in [("match-move", false), ("stabilize", true)] {
            params["output"] = json!(output);
            let xf = Value::Object(track::transform_params(&params, [40, 30], stabilize).unwrap());
            assert_eq!(anim::key_frames(&xf["rotate"]), [1, 5]);
            for f in [1, 5] {
                let want = eval("tracker", &resolve_params("tracker", &params, f), &[Some(&src)], &CTX).unwrap();
                let got = eval("transform", &resolve_params("transform", &xf, f), &[Some(&src)], &CTX).unwrap();
                let diff = want.pixels.iter().zip(&got.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
                assert!(diff < 2e-3, "{output} at frame {f}: off by {diff}");
            }
        }
        // A translate-only solve leaves rotate and scale alone.
        params["solve"] = json!("translate");
        let xf = track::transform_params(&params, [40, 30], false).unwrap();
        assert!(xf.contains_key("translate") && !xf.contains_key("rotate") && !xf.contains_key("scale"));
    }

    #[test]
    fn one_tracker_track_turns_the_output_by_its_warp() {
        let mut src = Image::new(40, 30);
        src.set(25, 15, [1.0, 1.0, 1.0, 1.0]);
        // The track stays put at (15.5, 15.5) but the feature turns 90°.
        let params = json!({
            "track1": [15.5, 15.5],
            "track1_warp": { "keys": [[1, [0.0, 1.0]], [5, [90.0, 1.0]]], "interp": "linear" },
            "output": "match-move", "reference_frame": 1, "solve": "translate, rotate",
        });
        let out = eval("tracker", &resolve_params("tracker", &params, 5), &[Some(&src)], &CTX).unwrap();
        // (25.5, 15.5) turned 90° clockwise about the track lands at (15.5, 25.5).
        assert!(out.at(15, 25)[3] > 0.99, "{:?}", out.at(15, 25));
        // The export agrees.
        let xf = Value::Object(track::transform_params(&params, [40, 30], false).unwrap());
        let got = eval("transform", &resolve_params("transform", &xf, 5), &[Some(&src)], &CTX).unwrap();
        let diff = out.flat().pixels.iter().zip(&got.flat().pixels).map(|(a, b)| (a[3] - b[3]).abs()).fold(0.0, f32::max);
        assert!(diff < 1e-3, "off by {diff}");
    }

    #[test]
    fn corner_pin_from_four_tracks_matches_the_tracker_output() {
        let mut src = Image::new(40, 30);
        for (i, px) in src.pixels.iter_mut().enumerate() {
            let (x, y) = ((i % 40) as f32, (i / 40) as f32);
            *px = [x / 40.0, y / 30.0, ((x * 0.7).sin() * (y * 0.5).cos()).abs(), 1.0];
        }
        // Four tracks moved by one similarity (turn, grow, shift) by frame 5,
        // so the tracker's own solve and the corner pin should agree.
        let m = track::solve(&[[0.0, 0.0], [1.0, 0.0]], &[[2.0, 1.0], [2.9, 0.4]], Solve::Scale);
        let starts = [[8.5, 8.5], [30.5, 7.5], [31.5, 22.5], [9.5, 21.5]];
        let mut params = json!({ "reference_frame": 1 });
        for (i, s0) in starts.iter().enumerate() {
            params[format!("track{}_on", i + 1)] = json!(true);
            params[format!("track{}", i + 1)] = json!({ "keys": [[1, s0], [5, track::apply(&m, *s0)]], "interp": "linear" });
        }
        for (output, stabilize) in [("match-move", false), ("stabilize", true)] {
            params["output"] = json!(output);
            let pin = Value::Object(track::corner_pin_params(&params, stabilize).unwrap());
            let want = eval("tracker", &resolve_params("tracker", &params, 5), &[Some(&src)], &CTX).unwrap();
            let got = eval("corner_pin", &resolve_params("corner_pin", &pin, 5), &[Some(&src)], &CTX).unwrap();
            let diff = want.flat().pixels.iter().zip(&got.flat().pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
            assert!(diff < 2e-3, "{output}: off by {diff}");
            // At the reference frame it's the identity.
            let still = eval("corner_pin", &resolve_params("corner_pin", &pin, 1), &[Some(&src)], &CTX).unwrap();
            assert!(still.flat().pixels.iter().zip(&src.flat().pixels).all(|(a, b)| (0..4).all(|c| (a[c] - b[c]).abs() < 1e-4)));
        }
        params["track4_on"] = json!(false);
        assert!(track::corner_pin_params(&params, false).is_err());
    }

    #[test]
    fn exported_rotation_does_not_jump_a_full_turn() {
        // One pair of tracks turning from 170° to 190° about (50, 50).
        let at = |deg: f64| {
            let (s, c) = deg.to_radians().sin_cos();
            [[50.0 - 20.0 * c, 50.0 - 20.0 * s], [50.0 + 20.0 * c, 50.0 + 20.0 * s]]
        };
        let key = |i: usize| json!({ "keys": [[1, at(0.0)[i]], [2, at(170.0)[i]], [3, at(190.0)[i]]], "interp": "linear" });
        let params = json!({ "track1": key(0), "track2_on": true, "track2": key(1), "solve": "translate, rotate" });
        let xf = track::transform_params(&params, [100, 100], false).unwrap();
        let angles: Vec<f64> = anim::keys(&xf["rotate"]).iter().map(|k| k.value[0]).collect();
        assert!((angles[1] - 170.0).abs() < 0.01 && (angles[2] - 190.0).abs() < 0.01, "{angles:?}");
    }

    #[test]
    fn premult_round_trip() {
        let src = solid(1, 1, [0.2, 0.3, 0.4, 0.5]);
        let straight = eval("unpremult", &json!({}), &[Some(&src)], &CTX).unwrap();
        assert!(approx(straight.get(0, 0)[0], 0.4));
        let back = eval("premult", &json!({}), &[Some(&straight)], &CTX).unwrap();
        assert!(approx(back.get(0, 0)[0], 0.2) && approx(back.get(0, 0)[3], 0.5));
    }

    #[test]
    fn shuffle_moves_passes_and_channels() {
        let mut img = solid(2, 2, [0.1, 0.2, 0.3, 1.0]);
        for (c, v) in [("P.x", 1.0), ("P.y", 2.0), ("P.z", 3.0), ("N.y", -1.0), ("depth", 7.0)] {
            *img.plane_mut(c) = vec![v; 4];
        }
        let run = |p: Value| eval("shuffle", &p, &[Some(&img)], &CTX);
        // Defaults change nothing.
        let same = run(json!({})).unwrap();
        assert_eq!((&same.pixels, &same.planes), (&img.pixels, &img.planes));
        // A pass into the color, any plane into a channel.
        let p = run(json!({ "in": "P" })).unwrap();
        assert_eq!(p.get(1, 1), [1.0, 2.0, 3.0, 1.0]);
        assert_eq!(run(json!({ "green": "N.y", "alpha": "1" })).unwrap().get(0, 0), [0.1, -1.0, 0.3, 1.0]);
        assert_eq!(run(json!({ "in": "depth" })).unwrap().get(0, 0), [7.0, 7.0, 7.0, 1.0]);
        // And back: graded in between, the pass changes, the color doesn't.
        let mut graded = p.clone();
        graded.pixels.iter_mut().for_each(|px| px[0] *= 2.0);
        let back = eval("shuffle", &json!({ "out": "P" }), &[Some(&graded)], &CTX).unwrap();
        assert_eq!((back.plane_at("P.x", 0, 0), back.plane_at("P.y", 0, 0)), (2.0, 2.0));
        let round = eval("shuffle", &json!({ "out": "P" }), &[Some(&p)], &CTX).unwrap();
        assert_eq!(round.planes, img.planes, "a pass to RGBA and back is unchanged");
        // The color into a new pass, keeping the color.
        let glow = run(json!({ "out": "glow" })).unwrap();
        assert_eq!((glow.pixels.clone(), glow.plane_at("glow.B", 0, 0)), (img.pixels.clone(), 0.3));
        // Removing passes.
        let some = run(json!({ "remove": "P, depth" })).unwrap();
        assert_eq!(some.planes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["N.y"]);
        assert!(run(json!({ "remove": "*" })).unwrap().planes.is_empty());
        // Missing layers and channels are named.
        assert!(run(json!({ "in": "motion" })).err().unwrap().to_string().contains("motion"));
        assert!(run(json!({ "blue": "N.q" })).err().unwrap().to_string().contains("N.q"));
    }

    #[test]
    fn merge_by_depth() {
        // A: red 5 away on the left half; B: blue 3 away on the right half
        // and red's middle.
        let mut a = solid(4, 1, [1.0, 0.0, 0.0, 1.0]);
        *a.plane_mut("depth") = vec![5.0; 4];
        let mut b = solid(4, 1, [0.0, 0.0, 1.0, 1.0]);
        b.pixels[0] = [0.0; 4];
        *b.plane_mut("depth") = vec![0.0, 3.0, 3.0, 3.0];
        let m = eval("merge", &json!({ "operation": "zmerge" }), &[Some(&a), Some(&b), None], &CTX).unwrap();
        assert_eq!((m.get(0, 0), m.get(2, 0), m.plane_at("depth", 2, 0)), ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], 3.0));
        // With B behind, A shows; mix still blends toward B.
        *b.plane_mut("depth") = vec![0.0, 9.0, 9.0, 9.0];
        let m = eval("merge", &json!({ "operation": "zmerge" }), &[Some(&a), Some(&b), None], &CTX).unwrap();
        assert_eq!(m.get(2, 0), [1.0, 0.0, 0.0, 1.0]);
        let half = eval("merge", &json!({ "operation": "zmerge", "mix": 0.5 }), &[Some(&a), Some(&b), None], &CTX).unwrap();
        assert_eq!(half.get(2, 0), [0.5, 0.0, 0.5, 1.0]);
    }

    /// Swap A / B is the same Merge with its inputs exchanged, for every
    /// operation, the mask's base and the frame included.
    #[test]
    fn merge_swap_exchanges_a_and_b() {
        // Different sizes, colors, alphas and depths, so any mix-up shows.
        let mut a = solid(6, 2, [0.6, 0.2, 0.1, 0.8]);
        a.pixels[1] = [0.0; 4];
        *a.plane_mut("depth") = (0..12).map(|i| 2.0 + i as f32).collect();
        let mut b = solid(4, 2, [0.1, 0.3, 0.5, 0.5]);
        *b.plane_mut("depth") = (0..8).map(|i| 9.0 - i as f32).collect();
        let mut mask = solid(6, 2, [0.0, 0.0, 0.0, 0.5]);
        mask.pixels[0] = [0.0, 0.0, 0.0, 1.0];
        for op in ["over", "under", "add", "multiply", "screen", "min", "max", "zmerge"] {
            for m in [None, Some(&mask)] {
                let swapped = eval("merge", &json!({ "operation": op, "swap": true, "mix": 0.75 }), &[Some(&a), Some(&b), m], &CTX).unwrap();
                let exchanged = eval("merge", &json!({ "operation": op, "mix": 0.75 }), &[Some(&b), Some(&a), m], &CTX).unwrap();
                assert!(swapped.format == exchanged.format && swapped.pixels == exchanged.pixels && swapped.planes == exchanged.planes, "{op}, mask {}", m.is_some());
                assert_eq!(swapped.format, a.format, "{op}: in A's frame");
            }
            let plain = eval("merge", &json!({ "operation": op }), &[Some(&a), Some(&b), None], &CTX).unwrap();
            let off = eval("merge", &json!({ "operation": op, "swap": false }), &[Some(&a), Some(&b), None], &CTX).unwrap();
            assert!(plain.pixels == off.pixels && plain.format == b.format, "{op}: off is as before");
        }
        // B over A, spelled out: B's half alpha over A.
        let o = eval("merge", &json!({ "operation": "over", "swap": true }), &[Some(&a), Some(&b), None], &CTX).unwrap();
        let want: [f32; 4] = std::array::from_fn(|c| b.get(0, 0)[c] + a.get(0, 0)[c] * 0.5);
        assert!(o.get(0, 0).iter().zip(want).all(|(x, y)| approx(*x, y)), "{:?} vs {want:?}", o.get(0, 0));
    }

    /// Mask channel and Invert mask choose what masks, for any masked op.
    #[test]
    fn mask_channel_and_invert() {
        let img = solid(5, 1, [0.2, 0.2, 0.2, 1.0]);
        // One mask pixel per channel: red 1 / green 0.5 / blue 0.25 /
        // alpha 0.8, then a gray 0.5 for luminance; past its 5 pixels the
        // mask counts as empty.
        let mut mask = solid(5, 1, [0.0; 4]);
        mask.pixels = vec![[1.0, 0.0, 0.0, 0.0], [0.0, 0.5, 0.0, 0.0], [0.0, 0.0, 0.25, 0.0], [0.0, 0.0, 0.0, 0.8], [0.5, 0.5, 0.5, 0.0]];
        // Exposure +1 doubles; masked, the pixel goes from 0.2 toward 0.4 by
        // the mask's value.
        let amount = |channel: &str, invert: bool| -> Vec<f32> {
            let p = json!({ "stops": 1.0, "mask_channel": channel, "mask_invert": invert });
            let out = eval("exposure", &p, &[Some(&img), Some(&mask)], &CTX).unwrap();
            (0..5).map(|x| (out.get(x, 0)[0] - 0.2) / 0.2).collect()
        };
        let close = |got: Vec<f32>, want: [f32; 5]| got.iter().zip(want).all(|(g, w)| approx(*g, w)) || panic!("{got:?} vs {want:?}");
        close(amount("alpha", false), [0.0, 0.0, 0.0, 0.8, 0.0]);
        close(amount("red", false), [1.0, 0.0, 0.0, 0.0, 0.5]);
        close(amount("green", false), [0.0, 0.5, 0.0, 0.0, 0.5]);
        close(amount("blue", false), [0.0, 0.0, 0.25, 0.0, 0.5]);
        close(amount("luminance", false), [0.2126, 0.7152 * 0.5, 0.0722 * 0.25, 0.0, 0.5]);
        close(amount("alpha", true), [1.0, 1.0, 1.0, 0.2, 1.0]);
        close(amount("red", true), [0.0, 1.0, 1.0, 1.0, 0.5]);
        // Without a mask connected, invert does nothing: the effect is whole.
        let whole = eval("exposure", &json!({ "stops": 1.0, "mask_invert": true }), &[Some(&img), None], &CTX).unwrap();
        assert!(approx(whole.get(0, 0)[0], 0.4));
        // Inverted, the mask's outside (empty) lets everything through.
        let small = solid(2, 1, [0.0, 0.0, 0.0, 1.0]);
        let out = eval("exposure", &json!({ "stops": 1.0, "mask_invert": true }), &[Some(&img), Some(&small)], &CTX).unwrap();
        assert!(approx(out.get(0, 0)[0], 0.2) && approx(out.get(4, 0)[0], 0.4));
        // Every masked op offers both params; Merge also swaps.
        for info in OPS.iter().filter(|o| o.mask_base.is_some()) {
            let names: Vec<&str> = info.params.iter().map(|p| p.name).collect();
            assert!(names.contains(&"mask_channel") && names.contains(&"mask_invert"), "{}", info.name);
            assert_eq!(info.inputs.last().map(|p| (p.label, p.optional)), Some(("mask", true)), "{}", info.name);
        }
        assert!(info("merge").unwrap().params.iter().any(|p| p.name == "swap"));
    }

    /// A comp saved before Swap and the mask choices loads and renders as
    /// it did, and saves without them.
    #[test]
    fn light_2d_lights_the_image_by_its_type() {
        let img = solid(100, 80, [0.25, 0.25, 0.25, 1.0]);
        let lit = |p: Value, inputs: &[Option<&Image>]| eval("light_2d", &p, inputs, &CTX).unwrap();
        // A spot at a pixel lights it most; half at its cone's (soft) edge.
        let spot = json!({ "type": "spot", "position": [50.5, 40.5], "direction": 0, "cone": 60, "softness": 10 });
        let out = lit(spot.clone(), &[Some(&img), None, None, None]);
        let added = |x: usize, y: usize| out.get(x, y)[0] - 0.25;
        assert!(approx(added(70, 40), 1.0) && approx(added(20, 40), 0.0), "{} {}", added(70, 40), added(20, 40));
        let light = light_2d_light(&spot);
        let a = 30f64.to_radians();
        assert!((light.reach(50.5 + 30.0 * a.cos(), 40.5 + 30.0 * a.sin()) - 0.5).abs() < 1e-9);
        // Each type's params make its shape; color, intensity and exposure
        // its strength; the blend how it goes on.
        let p = json!({ "type": "point", "position": [10, 10], "radius": 20, "falloff": 2, "color": [1, 0.5, 0, 1], "intensity": 2, "exposure": 1, "blend": "multiply" });
        let l = light_2d_light(&p);
        assert!(matches!(l.shape, crate::light2d::Shape::Point { radius: 20.0, falloff: 2.0, .. }) && l.blend == crate::light2d::Blend::Multiply);
        assert!(approx(lit(p, &[Some(&img), None, None, None]).get(10, 10)[1], 0.25 * 0.5 * 4.0 * (1.0 - 0.5f32.sqrt() / 20.0).powi(2)));
        assert!(matches!(light_2d_light(&json!({ "type": "area", "shape": "ellipse" })).shape, crate::light2d::Shape::Area { ellipse: true, .. }));
        assert!(matches!(light_2d_light(&json!({ "type": "directional", "gradient": 50 })).shape, crate::light2d::Shape::Directional { falloff: 50.0, .. }));
        // Normals: a render's N pass and a normal map light alike (flat,
        // facing out: a directional light at 30° elevation gives sin 30°).
        let mut pass = solid(100, 80, [0.0; 4]);
        for (axis, v) in [("N.x", 0.0), ("N.y", 0.0), ("N.z", 1.0)] {
            *pass.plane_mut(axis) = vec![v; 100 * 80];
        }
        let map = solid(100, 80, [0.5, 0.5, 1.0, 1.0]);
        let sun = json!({ "type": "directional", "gradient": 0, "elevation": 30 });
        let (a, b) = (lit(sun.clone(), &[Some(&img), Some(&pass), None, None]), lit(sun, &[Some(&img), Some(&map), None, None]));
        assert!(approx(a.get(5, 5)[0] - 0.25, 0.5) && approx(b.get(5, 5)[0], a.get(5, 5)[0]), "{} {}", a.get(5, 5)[0], b.get(5, 5)[0]);
        // The generic mask: none of it where the mask is empty.
        let mut mask = solid(100, 80, [0.0; 4]);
        mask.pixels[40 * 100 + 70] = [0.0, 0.0, 0.0, 1.0];
        let out = lit(spot, &[Some(&img), None, None, Some(&mask)]);
        assert!(approx(out.get(70, 40)[0], 1.25) && approx(out.get(71, 40)[0], 0.25));
    }

    #[test]
    fn old_merges_and_masks_load_unchanged() {
        use crate::graph::{Evaluator, Graph};
        let old = json!({ "project": { "first_frame": 1, "last_frame": 1, "fps": 24, "format": { "width": 8, "height": 4 } },
            "nodes": [
                { "id": "a", "op": "constant", "params": { "color": [1.0, 0.0, 0.0, 0.5] } },
                { "id": "b", "op": "checkerboard", "params": { "size": 2 } },
                { "id": "m", "op": "rectangle", "params": { "center": [2.0, 2.0], "size": [4.0, 4.0] } },
                { "id": "merge", "op": "merge", "inputs": ["a", "b", "m"], "params": { "operation": "over", "mix": 0.8 } },
                { "id": "grade", "op": "grade", "inputs": ["merge", "m"], "params": { "multiply": [2.0, 2.0, 2.0] } } ] });
        let g: Graph = serde_json::from_value(old.clone()).unwrap();
        let saved = serde_json::to_value(&g).unwrap();
        for (n, before) in saved["nodes"].as_array().unwrap().iter().zip(old["nodes"].as_array().unwrap()) {
            assert_eq!(n["params"], before["params"], "saved as it was");
        }
        let mut explicit = old.clone();
        explicit["nodes"][3]["params"]["swap"] = json!(false);
        for k in [3, 4] {
            explicit["nodes"][k]["params"]["mask_channel"] = json!("alpha");
            explicit["nodes"][k]["params"]["mask_invert"] = json!(false);
        }
        let e: Graph = serde_json::from_value(explicit).unwrap();
        let render = |g: &Graph| Evaluator::new().evaluate(g, "grade", &CTX).unwrap();
        let (old, new) = (render(&g), render(&e));
        assert!(old.pixels == new.pixels, "as with the defaults spelled out");
        // Masked by the rectangle's alpha: graded inside it, not outside.
        let b = Evaluator::new().evaluate(&g, "b", &CTX).unwrap();
        assert_eq!(old.get(7, 3), b.get(7, 3), "outside the mask: B, ungraded");
        assert_ne!(old.get(2, 2), b.get(2, 2));
    }

    #[test]
    fn channel_select_copies_alpha_from_b() {
        let a = solid(2, 2, [0.1, 0.2, 0.3, 1.0]);
        let b = solid(2, 2, [0.0, 0.0, 0.0, 0.25]);
        let p = json!({ "alpha": "B alpha", "blue": "one" });
        let out = eval("channel_select", &p, &[Some(&a), Some(&b)], &CTX).unwrap();
        assert_eq!(out.get(1, 1), [0.1, 0.2, 1.0, 0.25]);
        let err = eval("channel_select", &p, &[Some(&a), None], &CTX).err().unwrap();
        assert!(err.to_string().contains("'B'"), "{err}");
    }

    #[test]
    fn switch_picks_input_and_names_missing_port() {
        let a = solid(1, 1, [1.0; 4]);
        let b = solid(1, 1, [0.5; 4]);
        let ins = [Some(&a), None, Some(&b), None, None];
        let out = eval("switch", &json!({ "which": 2 }), &ins, &CTX).unwrap();
        assert_eq!(out.get(0, 0), [0.5; 4]);
        let err = eval("switch", &json!({ "which": 1 }), &ins, &CTX).err().unwrap();
        assert!(err.to_string().contains("'1'"), "{err}");
    }

    #[test]
    fn keymix_ignores_a_alpha() {
        let a = solid(1, 1, [0.5, 0.0, 0.0, 0.5]);
        let b = solid(1, 1, [0.0, 0.0, 1.0, 1.0]);
        let mask = solid(1, 1, [1.0; 4]);
        let out = eval("keymix", &json!({}), &[Some(&a), Some(&b), Some(&mask)], &CTX).unwrap();
        assert_eq!(out.get(0, 0), [0.5, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn invert_alpha_keeps_color() {
        let src = solid(1, 1, color::premultiply([0.8, 0.6, 0.4, 0.25]));
        let out = eval("invert", &json!({ "channels": "alpha" }), &[Some(&src)], &CTX).unwrap();
        let s = color::unpremultiply(out.get(0, 0));
        assert!(approx(s[0], 0.8) && approx(s[3], 0.75));
    }

    #[test]
    fn shrinking_warps_average_instead_of_aliasing() {
        // A one-pixel checkerboard, the worst case: shrunk to a quarter, it
        // should come out an even grey, not a pattern of 0s and 1s.
        let check = checkerboard(&json!({ "width": 128, "height": 128, "size": 1,
            "color_a": [0, 0, 0, 1], "color_b": [1, 1, 1, 1] })).unwrap();
        // Over the middle of the shrunk image, away from its edges: the
        // spread of values (an aliased result has 0s and 1s).
        let spread = |img: &Image| {
            let vals: Vec<f32> = (56..72).flat_map(|y| (56..72).map(move |x| (x, y))).map(|(x, y)| img.at(x, y)[0]).collect();
            let mean = vals.iter().sum::<f32>() / vals.len() as f32;
            (mean, vals.iter().map(|v| (v - mean).abs()).fold(0.0, f32::max))
        };
        let shrunk = eval("transform", &json!({ "scale": [0.25, 0.25], "rotate": 30 }), &[Some(&check), None], &CTX).unwrap();
        let (mean, off) = spread(&shrunk);
        assert!((mean - 0.5).abs() < 0.03 && off < 0.08, "transform: mean {mean}, off by {off}");
        // Corner Pin squeezing the whole frame into the middle quarter.
        let pin = json!({ "from1": [0, 0], "from2": [128, 0], "from3": [128, 128], "from4": [0, 128],
                          "to1": [48, 48], "to2": [80, 48], "to3": [80, 80], "to4": [48, 80] });
        let pinned = eval("corner_pin", &pin, &[Some(&check), None], &CTX).unwrap();
        let (mean, off) = spread(&pinned);
        assert!((mean - 0.5).abs() < 0.03 && off < 0.08, "corner pin: mean {mean}, off by {off}");
        // The tracker's warp (an affine) the same.
        let m: track::Affine = [4.0, 0.0, -192.0, 0.0, 4.0, -192.0];
        let (mean, off) = spread(&warp(&check, &m));
        assert!((mean - 0.5).abs() < 0.03 && off < 0.08, "tracker warp: mean {mean}, off by {off}");
        // Enlarging is untouched: still plain bilinear.
        let big = eval("transform", &json!({ "scale": [2, 2] }), &[Some(&check), None], &CTX).unwrap();
        let p = big.at(64, 64);
        let q = check.sample_bilinear(63.75, 63.75);
        assert_eq!(p, q);
    }

    #[test]
    fn resize_downscale_averages_every_pixel() {
        let check = checkerboard(&json!({ "width": 64, "height": 64, "size": 1,
            "color_a": [0, 0, 0, 1], "color_b": [1, 1, 1, 1] })).unwrap();
        let p = json!({ "width": 32, "height": 32, "fit": "stretch" });
        let out = eval("resize", &p, &[Some(&check)], &CTX).unwrap();
        assert_eq!((out.format.width, out.format.height), (32, 32));
        assert!(out.flat().pixels.iter().all(|px| approx(px[0], 0.5)));
    }

    #[test]
    fn crop_shrinks_or_clears() {
        let src = solid(10, 8, [1.0; 4]);
        let p = json!({ "left": 2, "top": 1, "right": 3, "bottom": 0, "mode": "shrink canvas" });
        let out = eval("crop", &p, &[Some(&src)], &CTX).unwrap();
        assert_eq!((out.format.width, out.format.height), (5, 7));
        let p = json!({ "left": 2, "mode": "clear outside" });
        let out = eval("crop", &p, &[Some(&src)], &CTX).unwrap();
        assert_eq!(out.at(1, 0)[3], 0.0);
        assert_eq!(out.at(2, 0)[3], 1.0);
        assert_eq!(out.window(), IRect::new(2, 0, 10, 8));
    }
}
