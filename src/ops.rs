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
use crate::image::{IRect, Image, Size};
use crate::media;
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
    /// Move/rotate/scale gizmo about the image center, plus `translate`.
    Transform { translate: &'static str, rotate: &'static str, scale: &'static str },
    /// A box inset from the image edges by four Int params.
    Insets { left: &'static str, top: &'static str, right: &'static str, bottom: &'static str },
    /// A box (or the ellipse inside it) with center, size and rotation params.
    Box { center: &'static str, size: &'static str, rotate: &'static str, ellipse: bool },
    /// Editable vertices of a `Points` param.
    Points(&'static str),
    /// Tracker boxes: each enabled `track<i>` position with the shared
    /// pattern and search box sizes.
    Tracker { pattern: &'static str, search: &'static str },
    /// A quad of four Vec2 corners (`to`), and the four it is mapped from.
    CornerPin { to: [&'static str; 4], from: [&'static str; 4] },
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

const CHANNEL_SOURCES: &[&str] = &[
    "A red", "A green", "A blue", "A alpha", "B red", "B green", "B blue", "B alpha", "zero", "one",
];
const SWITCH_PORTS: usize = 5;

const SIZE: ParamKind = ParamKind::Int { min: 1, max: 16384 };
/// A source's frame size: 0 takes the project's format.
const FRAME_SIZE: ParamKind = ParamKind::Int { min: 0, max: 16384 };

/// The project format when none is set, and the frame of sources whose size
/// is 0 when evaluated outside a graph.
pub const DEFAULT_FORMAT: Size = Size { width: 1920, height: 1080 };

/// Ops whose `width` / `height` set their frame when they have no input to
/// take one from.
const SIZED_SOURCES: [&str; 6] = ["constant", "checkerboard", "rectangle", "ellipse", "polygon", "roto"];

/// Give a source whose `width` / `height` is 0 (the default) the project's
/// `format`. The graph evaluator calls this after resolving keyframes, so
/// the cache key follows the format too.
pub fn apply_format(op: &str, params: &mut Value, format: Size) {
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
/// `format`: centered, scaled to the shorter side. Call on a fresh node's
/// defaults, before any params given for it.
pub fn fit_new_shape(op: &str, params: &mut Value, format: Size) {
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
               solve them from the tracks' positions.",
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
            MIX,
        ],
        overlay: Overlay::CornerPin { to: ["to1", "to2", "to3", "to4"], from: ["from1", "from2", "from3", "from4"] },
        mask_base: Some(0),
        help: "Warps the image in perspective so the four From points land on the four To \
               points. In the viewer, drag the To corners (solid) or the From corners (dashed). \
               A Tracker with four tracks exports one (To Corner Pin) to pin a layer onto a \
               moving plane, or to stabilize it.",
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
    OPS.iter().find(|o| o.name == op).or_else(|| crate::plugin::info(op))
}

/// Every op: the built-ins followed by plugin ops.
pub fn all() -> Vec<&'static OpInfo> {
    OPS.iter().chain(crate::plugin::infos()).collect()
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
    let Value::Object(map) = params else { return params.clone() };
    if !map.values().any(anim::is_animated) {
        let mut out = params.clone();
        if op == "tracker" {
            add_reference_tracks(params, &mut out);
        }
        return out;
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
    let mut resolved = Value::Object(resolved);
    if op == "tracker" {
        add_reference_tracks(params, &mut resolved);
    }
    resolved
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
        "read" | "write" => media::is_sequence_pattern(path),
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
fn mask_mix(base: &Image, result: Image, mask: Option<&Image>, mix: f32) -> Image {
    let mix = mix.clamp(0.0, 1.0);
    if mask.is_none() && mix >= 1.0 {
        return result;
    }
    // Where the mask holds back the result, the base shows: both windows.
    let window = result.window().union(&base.window());
    generate(result.format, window, |x, y| {
        let t = mix * mask.map_or(1.0, |m| m.at(x, y)[3].clamp(0.0, 1.0));
        let (b, r) = (base.at(x, y), result.at(x, y));
        std::array::from_fn(|c| b[c] + (r[c] - b[c]) * t)
    })
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

    // B's frame, holding both inputs' data.
    let op = p.operation.as_str();
    let blank = blend(op, color::TRANSPARENT, color::TRANSPARENT)?;
    let window = pointwise_window(bg.format, &[fg.window(), bg.window()], blank);
    Ok(generate(bg.format, window, |x, y| blend(op, fg.at(x, y), bg.at(x, y)).unwrap_or(blank)))
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
    // About the frame's center (pixel centers on integers).
    let cx = src.format.width as f32 / 2.0;
    let cy = src.format.height as f32 / 2.0;
    let theta = -p.rotate.to_radians(); // inverse rotation
    let (s, c) = theta.sin_cos();
    let sx = if p.scale[0] == 0.0 { 1e-6 } else { p.scale[0] };
    let sy = if p.scale[1] == 0.0 { 1e-6 } else { p.scale[1] };
    // Forward: scale, rotate, then translate.
    let window = moved_window(src.window(), src.format, |[x, y]| {
        let (dx, dy) = ((x - cx) * sx, (y - cy) * sy);
        Some([dx * c + dy * s + cx + p.translate[0], -dx * s + dy * c + cy + p.translate[1]])
    });
    Ok(generate(src.format, window, |ox, oy| {
        // Undo translate, then rotate+scale about center (inverse).
        let dx = ox as f32 - p.translate[0] - cx;
        let dy = oy as f32 - p.translate[1] - cy;
        let rx = (dx * c - dy * s) / sx + cx;
        let ry = (dx * s + dy * c) / sy + cy;
        src.sample_bilinear(rx, ry)
    }))
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
    Ok(generate(src.format, window, |x, y| match track::project(&h, [x as f32 + 0.5, y as f32 + 0.5]) {
        Some(q) => src.sample_bilinear(q[0] - 0.5, q[1] - 0.5),
        None => color::TRANSPARENT,
    }))
}

/// Resample `src`: each output pixel center `o` reads `src` at `m(o)`.
fn warp(src: &Image, m: &track::Affine) -> Image {
    let forward = track::invert(m);
    let window = moved_window(src.window(), src.format, |[x, y]| {
        let [u, v] = track::apply(&forward, [x + 0.5, y + 0.5]);
        Some([u - 0.5, v - 0.5])
    });
    generate(src.format, window, |x, y| {
        let q = track::apply(m, [x as f32 + 0.5, y as f32 + 0.5]);
        // `sample_bilinear` puts pixel centers on integers.
        src.sample_bilinear(q[0] - 0.5, q[1] - 0.5)
    })
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
    Ok(generate(b.format, window, |x, y| {
        let (pa, pb) = (a.at(x, y), b.at(x, y));
        std::array::from_fn(|c| pa[c] + (pb[c] - pa[c]) * t)
    }))
}

/// A in B's frame; [`eval`]'s mask/mix step then blends it over B by the
/// mask.
fn keymix(inputs: &[Option<&Image>]) -> Result<Image> {
    let a = input(inputs, 0, "keymix")?;
    let b = input(inputs, 1, "keymix")?;
    Ok(generate(b.format, a.window(), |x, y| a.at(x, y)))
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
        "clear outside" => Ok(generate(f, window, |x, y| src.at(x, y))),
        "shrink canvas" => {
            // The box becomes the frame, its corner the new origin.
            let format = Size { width: keep.width().max(1), height: keep.height().max(1) };
            let (dx, dy) = (keep.x0, keep.y0);
            let moved = IRect::new(window.x0 - dx, window.y0 - dy, window.x1 - dx, window.y1 - dy);
            Ok(generate(format, moved, |x, y| src.at(x + dx, y + dy)))
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
    // Center the scaled image in the output frame.
    let ox = (w as f32 - src.format.width as f32 * sx) / 2.0;
    let oy = (h as f32 - src.format.height as f32 * sy) / 2.0;
    // Source pixels per output pixel, and how many taps cover that span.
    let (fx, fy) = (1.0 / sx, 1.0 / sy);
    let (nx, ny) = (fx.ceil().max(1.0) as usize, fy.ceil().max(1.0) as usize);
    let format = Size { width: w, height: h };
    let window = moved_window(src.window(), format, |[x, y]| Some([(x + 0.5) * sx + ox - 0.5, (y + 0.5) * sy + oy - 0.5]));
    Ok(generate(format, window, |x, y| {
        // Source position of this output pixel's center.
        let cx = (x as f32 + 0.5 - ox) * fx - 0.5;
        let cy = (y as f32 + 0.5 - oy) * fy - 0.5;
        if nx == 1 && ny == 1 {
            return src.sample_bilinear(cx, cy);
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
        acc.map(|v| v * norm)
    }))
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
    let format = bg.map_or(source_size(style.width, style.height), |b| b.format);
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
    // Coverage only where the shape can reach: its outline's bounds plus
    // the feather (centered on the edge), within the frame. Inverted, it
    // covers the rest of the frame.
    let frame = IRect::frame(format);
    let reach = if style.invert {
        frame
    } else {
        let pad = (style.feather.max(1.0) / 2.0).ceil() as i32 + 1;
        let (lo, hi) = outline.iter().filter(|p| p.iter().all(|v| v.is_finite())).fold(
            ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]),
            |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]),
        );
        let bounds = if lo[0] <= hi[0] {
            IRect::new(lo[0].floor() as i32, lo[1].floor() as i32, hi[0].ceil() as i32, hi[1].ceil() as i32).expand(pad)
        } else {
            IRect::default()
        };
        bounds.intersect(&frame)
    };
    let shifted: Vec<Pt> = outline.iter().map(|p| [p[0] - reach.x0 as f32, p[1] - reach.y0 as f32]).collect();
    let cov = shape::coverage(&[shifted], reach.width(), reach.height(), style.feather, rule);
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
        let gone = eval("transform", &json!({ "scale": [500, 500] }), &[Some(&red), None], &CTX).unwrap();
        assert!(gone.window().is_empty());
        // The tracker's match-move and a corner pin move windows the same way.
        let pin = json!({ "from1": [0, 0], "from2": [100, 0], "from3": [100, 50], "from4": [0, 50],
                          "to1": [20, 5], "to2": [120, 5], "to3": [120, 55], "to4": [20, 55] });
        let pinned = eval("corner_pin", &pin, &[Some(&red), None], &CTX).unwrap();
        assert_eq!(pinned.at(31, 16), [1.0, 0.0, 0.0, 1.0]);
        assert!(pinned.window().width() < 10, "{:?}", pinned.window());
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
