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
    Merge,
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
}

impl ParamKind {
    const fn is_numeric(self) -> bool {
        matches!(self, ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Vec2 | ParamKind::Vec3 | ParamKind::Color)
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

pub struct OpInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub category: Category,
    /// Input port labels, in order.
    pub inputs: &'static [&'static str],
    pub params: &'static [ParamSpec],
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

const SIZE: ParamKind = ParamKind::Int { min: 1, max: 16384 };
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
        help: "A test pattern.",
    },
    OpInfo {
        name: "read",
        label: "Read (still)",
        category: Category::Source,
        inputs: &[],
        params: &[p!("path", "File", ParamKind::Path { save: false }, "")],
        help: "Reads a single image file. Prefer Media In for sequences and video.",
    },
    OpInfo {
        name: "grade",
        label: "Grade",
        category: Category::Color,
        inputs: &["input"],
        params: &[
            p!("multiply", "Multiply", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            p!("offset", "Offset", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("gamma", "Gamma", ParamKind::Vec3, [1.0, 1.0, 1.0]),
        ],
        help: "Per-channel gain, offset and gamma on unpremultiplied color.",
    },
    OpInfo {
        name: "merge",
        label: "Merge",
        category: Category::Merge,
        inputs: &["A", "B"],
        params: &[p!("operation", "Operation", ParamKind::Choice(MERGE_OPS), "over")],
        help: "Combines A (foreground) with B (background).",
    },
    OpInfo {
        name: "transform",
        label: "Transform",
        category: Category::Transform,
        inputs: &["input"],
        params: &[
            p!("translate", "Translate", ParamKind::Vec2, [0.0, 0.0]),
            p!("scale", "Scale", ParamKind::Vec2, [1.0, 1.0]),
            p!("rotate", "Rotate", ParamKind::Float { min: -360.0, max: 360.0 }, 0.0),
        ],
        help: "Translate, scale and rotate about the image center.",
    },
    OpInfo {
        name: "blur",
        label: "Blur",
        category: Category::Filter,
        inputs: &["input"],
        params: &[
            p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 200.0 }, 8.0),
            p!("passes", "Passes", ParamKind::Int { min: 1, max: 6 }, 3),
        ],
        help: "Repeated box blur; 3 passes approximate a gaussian.",
    },
    OpInfo {
        name: "media_out",
        label: "Media Out",
        category: Category::Output,
        inputs: &["input"],
        params: &[
            p!("path", "File", ParamKind::Path { save: true }, ""),
            fixed!("first_frame", "First frame", FRAME, 1),
            fixed!("last_frame", "Last frame", FRAME, 100),
            fixed!("fps", "FPS (video)", ParamKind::Float { min: 1.0, max: 240.0 }, 24.0),
        ],
        help: "Renders a frame range to an image sequence (shot.####.exr) or a \
               video (.mp4 = H.264, .mov = ProRes 422 HQ).",
    },
    OpInfo {
        name: "viewer",
        label: "Viewer",
        category: Category::Output,
        inputs: &["input"],
        params: &[],
        help: "Shows its input in the viewer panel.",
    },
    OpInfo {
        name: "write",
        label: "Write (still)",
        category: Category::Output,
        inputs: &["input"],
        params: &[p!("path", "File", ParamKind::Path { save: true }, "")],
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

/// Evaluate one operator. `inputs` are the already-rendered upstream images.
pub fn eval(op: &str, params: &Value, inputs: &[&Image], ctx: &EvalContext) -> Result<Image> {
    match op {
        "media_in" => media_in(params, ctx),
        "constant" => constant(params),
        "checkerboard" => checkerboard(params),
        "read" => read(params, ctx),
        "grade" => grade(params, inputs),
        "merge" => merge(params, inputs),
        "transform" => transform(params, inputs),
        "blur" => blur(params, inputs),
        "write" => write(params, inputs, ctx),
        // Rendering to disk is driven by `render`; in the graph these pass through.
        "media_out" | "viewer" => Ok(input(inputs, 0, op)?.clone()),
        other => bail!("unknown op '{other}'"),
    }
}

fn input<'a>(inputs: &'a [&'a Image], n: usize, op: &str) -> Result<&'a Image> {
    inputs
        .get(n)
        .copied()
        .ok_or_else(|| anyhow!("op '{op}' requires at least {} input(s)", n + 1))
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
fn grade(params: &Value, inputs: &[&Image]) -> Result<Image> {
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
fn merge(params: &Value, inputs: &[&Image]) -> Result<Image> {
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
    /// Rotation in degrees, counter-clockwise, about the image center.
    rotate: f32,
}

/// Affine transform with bilinear resampling. Uses inverse mapping: for each
/// output pixel we find the source location and sample it.
fn transform(params: &Value, inputs: &[&Image]) -> Result<Image> {
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

fn blur(params: &Value, inputs: &[&Image]) -> Result<Image> {
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

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WriteParams {
    path: PathBuf,
}

/// Write input[0] to disk (when the context allows) and pass it through
/// unchanged so writes can sit mid-graph.
fn write(params: &Value, inputs: &[&Image], ctx: &EvalContext) -> Result<Image> {
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
