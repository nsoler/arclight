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
use crate::image::Image;
use crate::media;
use crate::shape::{self, FillRule, Pt};
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
    Merge,
    Shape,
    Transform,
    Filter,
    Output,
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
    Bool,
    /// A flat array of point data, `stride` numbers per point (x, y first).
    /// Animates as a whole: one key holds every point.
    Points { stride: usize },
}

impl ParamKind {
    const fn is_numeric(self) -> bool {
        matches!(self, ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Vec2 | ParamKind::Vec3 | ParamKind::Color | ParamKind::Points { .. })
    }
}

pub struct ParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: fn() -> Value,
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
    /// Move/rotate/scale gizmo about the image center, plus `translate`.
    Transform { translate: &'static str, rotate: &'static str, scale: &'static str },
    /// A box inset from the image edges by four Int params.
    Insets { left: &'static str, top: &'static str, right: &'static str, bottom: &'static str },
    /// A box (or the ellipse inside it) with center, size and rotation params.
    Box { center: &'static str, size: &'static str, rotate: &'static str, ellipse: bool },
    /// Editable vertices of a `Points` param.
    Points(&'static str),
}

/// An input port.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub label: &'static str,
    /// An optional port may be left unconnected; the op then sees `None`.
    pub optional: bool,
}

const fn req(label: &'static str) -> Port {
    Port { label, optional: false }
}

const fn opt(label: &'static str) -> Port {
    Port { label, optional: true }
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
    /// params with [`MIX`], and [`eval`] blends the result back toward this
    /// input by `mix × mask alpha`. Ops don't handle masking themselves.
    pub mask_base: Option<usize>,
    /// Handles the viewer shows while this op's node is selected.
    pub overlay: Overlay,
    pub help: &'static str,
}

const MERGE_OPS: &[&str] = &["over", "under", "add", "multiply", "screen", "min", "max"];

macro_rules! p {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: || json!($default), animatable: $kind.is_numeric() }
    };
}

/// A param that can't be keyframed.
macro_rules! fixed {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: || json!($default), animatable: false }
    };
}

/// The generic mask port of ops with [`OpInfo::mask_base`].
const MASK: Port = opt("mask");
/// The generic mix param of ops with [`OpInfo::mask_base`].
const MIX: ParamSpec = p!("mix", "Mix", ParamKind::Float { min: 0.0, max: 1.0 }, 1.0);

const CHANNEL_SOURCES: &[&str] = &[
    "A red", "A green", "A blue", "A alpha", "B red", "B green", "B blue", "B alpha", "zero", "one",
];
const SWITCH_PORTS: usize = 5;

const SIZE: ParamKind = ParamKind::Int { min: 1, max: 16384 };
const FEATHER: ParamKind = ParamKind::Float { min: 0.0, max: 200.0 };
const OPACITY: ParamKind = ParamKind::Float { min: 0.0, max: 1.0 };
const SHAPE_OPS: &[&str] = &["over", "subtract"];
const FILL_RULES: &[&str] = &["non-zero", "even-odd"];
/// Help text for shape ops: their own sentence plus what they share.
macro_rules! shape_help {
    ($s:literal) => {
        concat!(
            $s,
            " Without a background the canvas is Width × Height; with one, the shape \
             is drawn over it (or, with Subtract, cut out of its alpha). Feather is the \
             width of the soft edge, centered on the outline. The alpha feeds mask inputs directly."
        )
    };
}
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
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads a still, an image sequence (shot.####.exr) or a video file. \
               Frames outside the range hold the first/last frame.",
    },
    OpInfo {
        name: "constant",
        label: "Constant",
        category: Category::Source,
        inputs: &[],
        params: &[
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
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
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
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
        label: "Read (still)",
        category: Category::Source,
        inputs: &[],
        params: &[p!("path", "File", ParamKind::Path { save: false }, "")],
        overlay: Overlay::None,
        mask_base: None,
        help: "Reads a single image file. Prefer Media In for sequences and video.",
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
            MIX,
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
        params: &[p!("operation", "Operation", ParamKind::Choice(MERGE_OPS), "over"), MIX],
        overlay: Overlay::None,
        mask_base: Some(1),
        help: "Combines A (foreground) with B (background). The mask limits \
               where A is combined; outside it the output is B.",
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
            MIX,
        ],
        overlay: Overlay::Transform { translate: "translate", rotate: "rotate", scale: "scale" },
        mask_base: Some(0),
        help: "Translate, scale and rotate about the image center.",
    },
    OpInfo {
        name: "blur",
        label: "Blur",
        category: Category::Filter,
        inputs: &[IN, MASK],
        params: &[
            p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 200.0 }, 8.0),
            p!("passes", "Passes", ParamKind::Int { min: 1, max: 6 }, 3),
            MIX,
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
        params: &[p!("stops", "Stops", ParamKind::Float { min: -10.0, max: 10.0 }, 0.0), MIX],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Brightens or darkens color by photographic stops (×2 per stop).",
    },
    OpInfo {
        name: "saturation",
        label: "Saturation",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[p!("saturation", "Saturation", ParamKind::Float { min: 0.0, max: 4.0 }, 1.0), MIX],
        overlay: Overlay::None,
        mask_base: Some(0),
        help: "Scales color away from (or toward) its Rec. 709 luminance. 0 is grayscale.",
    },
    OpInfo {
        name: "invert",
        label: "Invert",
        category: Category::Color,
        inputs: &[IN, MASK],
        params: &[p!("channels", "Channels", ParamKind::Choice(&["rgb", "alpha", "rgba"]), "rgb"), MIX],
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
            MIX,
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
        params: &[MIX],
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
        name: "rectangle",
        label: "Rectangle",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
            p!("center", "Center", ParamKind::Vec2, [256.0, 256.0]),
            p!("size", "Size", ParamKind::Vec2, [256.0, 256.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
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
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
            p!("center", "Center", ParamKind::Vec2, [256.0, 256.0]),
            p!("size", "Size", ParamKind::Vec2, [256.0, 256.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
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
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
            p!("points", "Points", ParamKind::Points { stride: 2 }, [156.0, 356.0, 256.0, 156.0, 356.0, 356.0]),
            fixed!("fill_rule", "Fill rule", ParamKind::Choice(FILL_RULES), "non-zero"),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("opacity", "Opacity", OPACITY, 1.0),
            p!("feather", "Feather", FEATHER, 0.0),
            fixed!("invert", "Invert", ParamKind::Bool, false),
            fixed!("operation", "Operation", ParamKind::Choice(SHAPE_OPS), "over"),
        ],
        overlay: Overlay::Points("points"),
        mask_base: None,
        help: shape_help!("A filled polygon. In the viewer, drag points, click an edge to add one, \
               Ctrl+click to add after the selected point, Shift+click to select several, ⌫ to delete."),
    },
    OpInfo {
        name: "roto",
        label: "Roto",
        category: Category::Shape,
        inputs: &[opt("background")],
        params: &[
            p!("width", "Width", SIZE, 512),
            p!("height", "Height", SIZE, 512),
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
        ],
        overlay: Overlay::Points("points"),
        mask_base: None,
        help: shape_help!("A closed bezier shape for rotoscoping; chain Roto nodes through the \
               background for several shapes or holes. In the viewer, drag points, click the \
               curve to add one, Ctrl+click to add after the selected point (drag to pull out \
               its tangents), Shift+click to select several, ⌫ to delete. Selected points show \
               their tangent handles: dragging one keeps the curve smooth, Alt breaks it; \
               Alt+drag a point to pull new tangents out of it."),
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
        ],
        overlay: Overlay::None,
        mask_base: None,
        help: "Renders a frame range to an image sequence (shot.####.exr) or a \
               video (.mp4 = H.264, .mov = ProRes 422 HQ).",
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
        params: &[p!("path", "File", ParamKind::Path { save: true }, "")],
        overlay: Overlay::None,
        mask_base: None,
        help: "Writes its input when the graph is rendered from the command line.",
    },
];

pub fn info(op: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|o| o.name == op)
}

/// `params` with every missing key filled in from the op's declared defaults.
pub fn params_with_defaults(op: &str, params: &Value) -> Value {
    let mut obj = match params {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    if let Some(info) = info(op) {
        for spec in info.params {
            obj.entry(spec.name).or_insert_with(spec.default);
        }
    }
    Value::Object(obj)
}

/// `params` with every keyframed value replaced by its value at `frame`.
/// Integer params are rounded so they still deserialize as integers.
pub fn resolve_params(op: &str, params: &Value, frame: i64) -> Value {
    let Value::Object(map) = params else { return params.clone() };
    if !map.values().any(anim::is_animated) {
        return params.clone();
    }
    let specs = info(op).map(|i| i.params).unwrap_or(&[]);
    let resolved = map
        .iter()
        .map(|(k, v)| {
            let mut v = anim::evaluate(v, frame as f64);
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

/// True when the op's output can differ from frame to frame on its own,
/// independent of its inputs.
pub fn is_time_varying(op: &str, params: &Value) -> bool {
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    match op {
        "media_in" => true,
        "read" | "write" => media::is_sequence_pattern(path),
        _ => false,
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

/// A copy of `src` with `f` applied to every pixel.
fn mapped(src: &Image, f: impl Fn(Rgba) -> Rgba + Sync + Send) -> Image {
    let mut out = src.clone();
    map_pixels(&mut out, f);
    out
}

/// Pixel (x, y) of `img`, transparent outside it.
#[inline]
fn pixel_or_clear(img: &Image, x: usize, y: usize) -> Rgba {
    if x < img.width && y < img.height {
        img.get(x, y)
    } else {
        color::TRANSPARENT
    }
}

/// Evaluate one operator. `inputs` are the already-rendered upstream images,
/// one per port; an unconnected optional port is `None`.
pub fn eval(op: &str, params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Result<Image> {
    let out = eval_op(op, params, inputs, ctx)?;
    match info(op).and_then(|i| i.mask_base.map(|b| (b, i.inputs.len() - 1))) {
        Some((base, mask)) => {
            let mix = params.get("mix").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            Ok(mask_mix(input(inputs, base, op)?, out, inputs.get(mask).copied().flatten(), mix))
        }
        None => Ok(out),
    }
}

/// `result` blended back toward `base` by `mix × mask alpha`. Outside the
/// mask's extent the mask counts as empty; with no mask it counts as full.
fn mask_mix(base: &Image, mut result: Image, mask: Option<&Image>, mix: f32) -> Image {
    let mix = mix.clamp(0.0, 1.0);
    if mask.is_none() && mix >= 1.0 {
        return result;
    }
    let w = result.width;
    result.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let (x, y) = (i % w, i / w);
        let m = match mask {
            Some(m) if x < m.width && y < m.height => m.get(x, y)[3].clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 1.0,
        };
        let t = mix * m;
        let b = if x < base.width && y < base.height { base.get(x, y) } else { color::TRANSPARENT };
        for c in 0..4 {
            px[c] = b[c] + (px[c] - b[c]) * t;
        }
    });
    result
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
        "saturation" => saturation(params, inputs),
        "invert" => invert(params, inputs),
        "clamp" => clamp(params, inputs),
        "premult" => Ok(mapped(input(inputs, 0, op)?, color::premultiply)),
        "unpremult" => Ok(mapped(input(inputs, 0, op)?, color::unpremultiply)),
        "channel_select" => channel_select(params, inputs),
        "switch" => switch(params, inputs),
        "dissolve" => dissolve(params, inputs),
        "keymix" => keymix(inputs),
        "crop" => crop(params, inputs),
        "resize" => resize(params, inputs),
        "rectangle" | "ellipse" | "polygon" | "roto" => shape_op(op, params, inputs),
        "write" => write(params, inputs, ctx),
        // Rendering to disk is driven by `render`; in the graph these pass through.
        "media_out" | "viewer" => Ok(input(inputs, 0, op)?.clone()),
        other => bail!("unknown op '{other}'"),
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
}

fn media_in(params: &Value, ctx: &EvalContext) -> Result<Image> {
    let p: MediaIn = parse(params, "media_in")?;
    if p.path.is_empty() {
        bail!("no file selected");
    }
    let last = p.last_frame.max(p.first_frame);
    let frame = ctx.frame.clamp(p.first_frame, last);
    let path = Path::new(&p.path);
    if media::is_video(path) {
        let img = media::read_video_frame(path, (frame - p.first_frame) as usize)?;
        Ok(Arc::unwrap_or_clone(img))
    } else {
        Image::read(&media::frame_path(&p.path, frame))
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
    Ok(Image::filled(
        p.width,
        p.height,
        color::premultiply(p.color),
    ))
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
    let mut img = Image::new(p.width, p.height);
    let w = p.width;
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
}

fn read(params: &Value, ctx: &EvalContext) -> Result<Image> {
    let p: Read = parse(params, "read")?;
    if p.path.is_empty() {
        bail!("no file selected");
    }
    Image::read(&media::frame_path(&p.path, ctx.frame))
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
    let mut out = src.clone();
    map_pixels(&mut out, |px| {
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
    });
    Ok(out)
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

    // Output canvas is the union bounding box (both share origin here).
    let w = fg.width.max(bg.width);
    let h = fg.height.max(bg.height);
    let sample = |img: &Image, x: usize, y: usize| -> Rgba {
        if x < img.width && y < img.height {
            img.get(x, y)
        } else {
            color::TRANSPARENT
        }
    };

    let op = p.operation.as_str();
    let mut out = Image::new(w, h);
    out.pixels
        .par_iter_mut()
        .enumerate()
        .try_for_each(|(i, px)| -> Result<()> {
            let x = i % w;
            let y = i / w;
            let f = sample(fg, x, y);
            let b = sample(bg, x, y);
            *px = blend(op, f, b)?;
            Ok(())
        })?;
    Ok(out)
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
    /// Rotation in degrees about the image center; positive turns clockwise
    /// on screen (image y points down).
    rotate: f32,
}

/// Affine transform with bilinear resampling. Uses inverse mapping: for each
/// output pixel we find the source location and sample it.
fn transform(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: Transform = parse(params, "transform")?;
    let src = input(inputs, 0, "transform")?;
    let (w, h) = (src.width, src.height);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let theta = -p.rotate.to_radians(); // inverse rotation
    let (s, c) = theta.sin_cos();
    let sx = if p.scale[0] == 0.0 { 1e-6 } else { p.scale[0] };
    let sy = if p.scale[1] == 0.0 { 1e-6 } else { p.scale[1] };

    let mut out = Image::new(w, h);
    out.pixels
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, px)| {
            let ox = (i % w) as f32;
            let oy = (i / w) as f32;
            // Undo translate, then rotate+scale about center (inverse).
            let dx = ox - p.translate[0] - cx;
            let dy = oy - p.translate[1] - cy;
            let rx = (dx * c - dy * s) / sx + cx;
            let ry = (dx * s + dy * c) / sy + cy;
            *px = src.sample_bilinear(rx, ry);
        });
    Ok(out)
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
    if r == 0 {
        return Ok(src.clone());
    }
    let mut img = src.clone();
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
    let w = a.width;
    let mut out = Image::new(a.width, a.height);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let (x, y) = (i % w, i / w);
        let pa = a.pixels[i];
        let pb = b.map_or(color::TRANSPARENT, |b| pixel_or_clear(b, x, y));
        for (c, src) in srcs.iter().enumerate() {
            px[c] = match *src {
                Source::Channel(0, ch) => pa[ch],
                Source::Channel(_, ch) => pb[ch],
                Source::Value(v) => v,
            };
        }
    });
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
    let (w, h) = (a.width.max(b.width), a.height.max(b.height));
    let mut out = Image::new(w, h);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let (x, y) = (i % w, i / w);
        let (pa, pb) = (pixel_or_clear(a, x, y), pixel_or_clear(b, x, y));
        for c in 0..4 {
            px[c] = pa[c] + (pb[c] - pa[c]) * t;
        }
    });
    Ok(out)
}

/// A on a canvas covering both inputs; [`eval`]'s mask/mix step then blends
/// it over B by the mask.
fn keymix(inputs: &[Option<&Image>]) -> Result<Image> {
    let a = input(inputs, 0, "keymix")?;
    let b = input(inputs, 1, "keymix")?;
    let (w, h) = (a.width.max(b.width), a.height.max(b.height));
    let mut out = Image::new(w, h);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| *px = pixel_or_clear(a, i % w, i / w));
    Ok(out)
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
    let x0 = p.left.min(src.width);
    let y0 = p.top.min(src.height);
    let x1 = src.width.saturating_sub(p.right).max(x0);
    let y1 = src.height.saturating_sub(p.bottom).max(y0);
    match p.mode.as_str() {
        "clear outside" => {
            let w = src.width;
            let mut out = src.clone();
            out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
                let (x, y) = (i % w, i / w);
                if x < x0 || x >= x1 || y < y0 || y >= y1 {
                    *px = color::TRANSPARENT;
                }
            });
            Ok(out)
        }
        "shrink canvas" => {
            let w = (x1 - x0).max(1);
            let h = (y1 - y0).max(1);
            let mut out = Image::new(w, h);
            out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
                *px = pixel_or_clear(src, x0 + i % w, y0 + i / w);
            });
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
    let (sx, sy) = (w as f32 / src.width as f32, h as f32 / src.height as f32);
    let (sx, sy) = match p.fit.as_str() {
        "stretch" => (sx, sy),
        "fit" => (sx.min(sy), sx.min(sy)),
        "fill" => (sx.max(sy), sx.max(sy)),
        other => bail!("unknown fit '{other}'"),
    };
    // Center the scaled image in the output frame.
    let ox = (w as f32 - src.width as f32 * sx) / 2.0;
    let oy = (h as f32 - src.height as f32 * sy) / 2.0;
    // Source pixels per output pixel, and how many taps cover that span.
    let (fx, fy) = (1.0 / sx, 1.0 / sy);
    let (nx, ny) = (fx.ceil().max(1.0) as usize, fy.ceil().max(1.0) as usize);
    let mut out = Image::new(w, h);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        // Source position of this output pixel's center.
        let cx = ((i % w) as f32 + 0.5 - ox) * fx - 0.5;
        let cy = ((i / w) as f32 + 0.5 - oy) * fy - 0.5;
        if nx == 1 && ny == 1 {
            *px = src.sample_bilinear(cx, cy);
            return;
        }
        // Average an nx × ny grid of bilinear taps spread over the footprint.
        let mut acc = [0.0f32; 4];
        for j in 0..ny {
            for k in 0..nx {
                let tx = cx + fx * ((k as f32 + 0.5) / nx as f32 - 0.5);
                let ty = cy + fy * ((j as f32 + 0.5) / ny as f32 - 0.5);
                let s = src.sample_bilinear(tx, ty);
                for c in 0..4 {
                    acc[c] += s[c];
                }
            }
        }
        let norm = 1.0 / (nx * ny) as f32;
        *px = acc.map(|v| v * norm);
    });
    Ok(out)
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

fn shape_op(op: &str, params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let style: ShapeStyle = parse(params, op)?;
    let bg = inputs.first().copied().flatten();
    let (w, h) = bg.map_or((style.width, style.height), |b| (b.width, b.height));
    let (outline, rule) = match op {
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
    };
    let cov = shape::coverage(&[outline], w, h, style.feather, rule);
    shade(&cov, w, h, &style, bg)
}

/// Turn shape coverage into an image: the shape's color over `bg`, or with
/// the Subtract operation, `bg` held out by the shape.
fn shade(cov: &[f32], w: usize, h: usize, style: &ShapeStyle, bg: Option<&Image>) -> Result<Image> {
    let subtract = match style.operation.as_str() {
        "over" => false,
        "subtract" => true,
        other => bail!("unknown shape operation '{other}'"),
    };
    let color = color::premultiply(style.color);
    let opacity = style.opacity.clamp(0.0, 1.0);
    let mut img = Image::new(w, h);
    img.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let c = if style.invert { 1.0 - cov[i] } else { cov[i] } * opacity;
        let b = bg.map_or(color::TRANSPARENT, |b| b.pixels[i]);
        for k in 0..4 {
            px[k] = if subtract { b[k] * (1.0 - c) } else { color[k] * c + b[k] * (1.0 - color[3] * c) };
        }
    });
    Ok(img)
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WriteParams {
    path: PathBuf,
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
        src.write(&media::frame_path(&p.path.to_string_lossy(), ctx.frame))?;
    }
    Ok(src.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!((out.width, out.height), (20, 10));
        assert_eq!(out.get(10, 5), [0.5, 0.0, 0.0, 0.5], "premultiplied color");
        assert_eq!(out.get(0, 0), color::TRANSPARENT);

        let bg = solid(8, 8, [0.0, 0.0, 1.0, 1.0]);
        let params = json!({ "center": [4, 4], "size": [4, 4], "color": [1, 0, 0, 0.5] });
        let out = eval("ellipse", &params, &[Some(&bg)], &CTX).unwrap();
        assert_eq!((out.width, out.height), (8, 8), "background sets the canvas");
        assert_eq!(out.get(0, 0), [0.0, 0.0, 1.0, 1.0]);
        let c = out.get(4, 4);
        assert!(approx(c[0], 0.5) && approx(c[2], 0.5) && approx(c[3], 1.0), "{c:?}");

        let cut = eval("rectangle", &json!({ "center": [4, 4], "size": [2, 2], "operation": "subtract" }), &[Some(&bg)], &CTX).unwrap();
        assert_eq!(cut.get(4, 4), color::TRANSPARENT);
        assert_eq!(cut.get(0, 0), [0.0, 0.0, 1.0, 1.0]);

        let inv = eval("rectangle", &json!({ "width": 8, "height": 8, "center": [4, 4], "size": [2, 2], "invert": true }), &[None], &CTX).unwrap();
        assert_eq!((inv.get(4, 4)[3], inv.get(0, 0)[3]), (0.0, 1.0));
    }

    #[test]
    fn polygon_points_animate_as_one_value() {
        let params = json!({
            "width": 10, "height": 10,
            "points": { "keys": [[1, [0, 0, 4, 0, 4, 4, 0, 4]], [11, [6, 6, 10, 6, 10, 10, 6, 10]]], "interp": "linear" },
        });
        let first = eval("polygon", &resolve_params("polygon", &params, 1), &[None], &CTX).unwrap();
        let last = eval("polygon", &resolve_params("polygon", &params, 11), &[None], &CTX).unwrap();
        assert_eq!((first.get(2, 2)[3], first.get(8, 8)[3]), (1.0, 0.0));
        assert_eq!((last.get(2, 2)[3], last.get(8, 8)[3]), (0.0, 1.0));
    }

    #[test]
    fn roto_draws_a_smooth_closed_curve() {
        let out = eval("roto", &json!({}), &[None], &CTX).unwrap();
        assert_eq!((out.width, out.height), (512, 512));
        assert_eq!(out.get(256, 256)[3], 1.0);
        // The default circle has radius 120: a square's corner at 45° would be inside, the circle's isn't.
        assert_eq!(out.get(256 + 100, 256 + 100)[3], 0.0);
        assert_eq!(out.get(256 + 80, 256 + 80)[3], 1.0);
        let area: f32 = out.pixels.iter().map(|p| p[3]).sum();
        let exact = std::f32::consts::PI * 120.0 * 120.0;
        assert!((area - exact).abs() / exact < 0.003, "{area} vs {exact}");
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
    fn resize_downscale_averages_every_pixel() {
        let check = checkerboard(&json!({ "width": 64, "height": 64, "size": 1,
            "color_a": [0, 0, 0, 1], "color_b": [1, 1, 1, 1] })).unwrap();
        let p = json!({ "width": 32, "height": 32, "fit": "stretch" });
        let out = eval("resize", &p, &[Some(&check)], &CTX).unwrap();
        assert_eq!((out.width, out.height), (32, 32));
        assert!(out.pixels.iter().all(|px| approx(px[0], 0.5)));
    }

    #[test]
    fn crop_shrinks_or_clears() {
        let src = solid(10, 8, [1.0; 4]);
        let p = json!({ "left": 2, "top": 1, "right": 3, "bottom": 0, "mode": "shrink canvas" });
        let out = eval("crop", &p, &[Some(&src)], &CTX).unwrap();
        assert_eq!((out.width, out.height), (5, 7));
        let p = json!({ "left": 2, "mode": "clear outside" });
        let out = eval("crop", &p, &[Some(&src)], &CTX).unwrap();
        assert_eq!(out.get(1, 0)[3], 0.0);
        assert_eq!(out.get(2, 0)[3], 1.0);
    }
}
