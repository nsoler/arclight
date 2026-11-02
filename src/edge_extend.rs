//! Edge Extend: pushes the colors just inside a matte's edge outward into
//! the transparent area around it, so a keyed foreground's edges don't
//! pick up dark or screen-colored fringes, and (with the matte inverted
//! and Fill everything on) turns a green / blue screen plate with the
//! subject matted out into a clean screen plate for a keyer.
//!
//! The fill is pull-push (Gortler et al., "The Lumigraph", 1996): a
//! pyramid of premultiplied color and coverage, each level half the size
//! of the one below (2×2 box sums, coverage capped at 1), then filled back
//! down, each level's gaps taking the coarser level's bilinear upsample.
//! Pixels the matte fully covers keep their color exactly.
//!
//! The GPU path (`gpu/edge_extend.rs`, `gpu/edge_extend.wgsl`) runs the
//! same passes in the same order; [`plan`] is shared.

use crate::color::{self, Rgba};
use crate::image::{IRect, Image, Size};
use crate::ops::{self, LUMA};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

/// The `alpha` choices: an opaque plate, or the extended (straight) color
/// with the input's alpha.
pub(crate) const ALPHA_MODES: &[&str] = &["solid", "original (unpremultiplied)"];

/// The largest Distance, pixels.
pub(crate) const MAX_DISTANCE: f32 = 1000.0;

#[derive(Deserialize)]
pub(crate) struct EdgeExtend {
    /// One of `ops::MASK_CHANNELS`: which channel of the matte (or of the
    /// image when no matte is connected) says what's covered.
    pub(crate) matte_channel: String,
    /// Flip the matte: extend from what it leaves out (a screen plate
    /// with the subject matted out).
    pub(crate) invert: bool,
    /// Fill the whole frame, however far it is from the matte.
    pub(crate) fill_all: bool,
    /// How far past the matte's edge to extend, pixels (when not filling
    /// everything).
    pub(crate) distance: f32,
    /// One of [`ALPHA_MODES`].
    pub(crate) alpha: String,
}

/// How an Edge Extend runs (shared with its GPU path).
pub(crate) struct Plan {
    /// An index in `ops::MASK_CHANNELS`.
    pub(crate) channel: usize,
    pub(crate) invert: bool,
    pub(crate) fill_all: bool,
    /// The reach past the matte, whole pixels (unused with `fill_all`).
    pub(crate) radius: usize,
    /// An index in [`ALPHA_MODES`].
    pub(crate) alpha: usize,
    /// Where the fill is worked out and output (the output's data window):
    /// the input's data plus the frame (Fill everything), or the data
    /// grown by the reach into the space around it, up to the frame.
    pub(crate) region: IRect,
    /// The pyramid's level sizes, finest (the region's) first, down to 1×1.
    pub(crate) levels: Vec<(usize, usize)>,
}

/// The plan for an Edge Extend of an image with frame `format` and data
/// window `data`.
pub(crate) fn plan(p: &EdgeExtend, format: Size, data: IRect) -> Result<Plan> {
    let channel = ops::MASK_CHANNELS.iter().position(|c| *c == p.matte_channel).ok_or_else(|| anyhow!("unknown matte channel '{}'", p.matte_channel))?;
    let alpha = ALPHA_MODES.iter().position(|a| *a == p.alpha).ok_or_else(|| anyhow!("unknown alpha '{}'", p.alpha))?;
    let radius = p.distance.clamp(0.0, MAX_DISTANCE).ceil() as usize;
    let around = data.union(&IRect::frame(format));
    let region = if data.is_empty() {
        data
    } else if p.fill_all {
        around
    } else {
        data.expand(radius as i32).intersect(&around)
    };
    let mut levels = vec![(region.width(), region.height())];
    while let Some(&(w, h)) = levels.last().filter(|(w, h)| *w > 1 || *h > 1) {
        levels.push((w.div_ceil(2), h.div_ceil(2)));
    }
    Ok(Plan { channel, invert: p.invert, fill_all: p.fill_all, radius, alpha, region, levels })
}

/// How much the matte covers at a pixel, 0–1: `px`'s `channel` (an index
/// in `ops::MASK_CHANNELS`), flipped when inverted.
pub(crate) fn coverage(px: Rgba, channel: usize, invert: bool) -> f32 {
    let v = match channel {
        1 => px[0],
        2 => px[1],
        3 => px[2],
        4 => px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2],
        _ => px[3],
    }
    .clamp(0.0, 1.0);
    if invert { 1.0 - v } else { v }
}

/// The pyramid's finest level at frame position (x, y): the straight
/// color times the coverage, and the coverage. Nothing where the image
/// is transparent (its color there is unknown).
pub(crate) fn seed(src: &Image, matte: Option<&Image>, plan: &Plan, x: i32, y: i32) -> Rgba {
    let px = src.at(x, y);
    if px[3] <= 0.0 {
        return color::TRANSPARENT;
    }
    let w = coverage(matte.map_or(px, |m| m.at(x, y)), plan.channel, plan.invert);
    let c = color::unpremultiply(px);
    [c[0] * w, c[1] * w, c[2] * w, w]
}

/// A level from the one below it (`fine`, `fw` × `fh`): each pixel the
/// sum of the 2×2 below, its coverage capped at 1 (the color scaled with
/// it). Pixels past the edge count as empty.
pub(crate) fn pull(fine: &[Rgba], fw: usize, fh: usize) -> Vec<Rgba> {
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
    (0..cw * ch)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (2 * (i % cw), 2 * (i / cw));
            let at = |x: usize, y: usize| if x < fw && y < fh { fine[y * fw + x] } else { color::TRANSPARENT };
            let (a, b, c, d) = (at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1));
            let s: [f32; 4] = std::array::from_fn(|k| a[k] + b[k] + c[k] + d[k]);
            pulled(s)
        })
        .collect()
}

/// A 2×2 sum with its coverage capped at 1.
fn pulled(s: Rgba) -> Rgba {
    if s[3] <= 0.0 {
        return color::TRANSPARENT;
    }
    let w = s[3].min(1.0);
    let k = w / s[3];
    [s[0] * k, s[1] * k, s[2] * k, w]
}

/// A level filled from the filled level above it (`coarse`, `cw` × `ch`):
/// each pixel its own value plus what its coverage leaves, of the coarse
/// level's bilinear upsample there.
pub(crate) fn push(fine: &[Rgba], fw: usize, coarse: &[Rgba], cw: usize, ch: usize) -> Vec<Rgba> {
    fine.par_iter()
        .enumerate()
        .map(|(i, f)| {
            let u = upsample(coarse, cw, ch, i % fw, i / fw);
            let left = 1.0 - f[3];
            std::array::from_fn(|k| f[k] + left * u[k])
        })
        .collect()
}

/// `coarse` (half size, `cw` × `ch`) bilinearly at fine pixel (x, y)'s
/// center, its edges repeating.
fn upsample(coarse: &[Rgba], cw: usize, ch: usize, x: usize, y: usize) -> Rgba {
    let (fx, fy) = (x as f32 * 0.5 - 0.25, y as f32 * 0.5 - 0.25);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let clampi = |v: f32, n: usize| (v.max(0.0) as usize).min(n - 1);
    let (xa, xb) = (clampi(x0, cw), clampi(x0 + 1.0, cw));
    let (ya, yb) = (clampi(y0, ch), clampi(y0 + 1.0, ch));
    let at = |x: usize, y: usize| coarse[y * cw + x];
    let (a, b, c, d) = (at(xa, ya), at(xb, ya), at(xa, yb), at(xb, yb));
    std::array::from_fn(|k| {
        let top = a[k] * (1.0 - tx) + b[k] * tx;
        let bottom = c[k] * (1.0 - tx) + d[k] * tx;
        top * (1.0 - ty) + bottom * ty
    })
}

/// Which pixels of a `w` × `h` level lie within `r` pixels (a square
/// reach: separable, horizontal then vertical) of one with coverage: 1 or
/// 0 in each.
pub(crate) fn reach(level0: &[Rgba], w: usize, h: usize, r: usize) -> Vec<f32> {
    let covered: Vec<f32> = level0.par_iter().map(|p| if p[3] > 0.0 { 1.0 } else { 0.0 }).collect();
    let mut across = vec![0.0; w * h];
    across.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
        let line = &covered[y * w..(y + 1) * w];
        spread(line, row, r);
    });
    // Vertically: column by column, through a transposed copy.
    let mut t = vec![0.0; w * h];
    t.par_chunks_mut(h.max(1)).enumerate().for_each(|(x, col)| {
        let line: Vec<f32> = (0..h).map(|y| across[y * w + x]).collect();
        spread(&line, col, r);
    });
    (0..w * h).map(|i| t[(i % w) * h + i / w]).collect()
}

/// `out[i]` = 1 when any of `line[i − r ..= i + r]` is set (running count).
fn spread(line: &[f32], out: &mut [f32], r: usize) {
    let n = line.len();
    let mut prefix = vec![0u32; n + 1];
    for (i, v) in line.iter().enumerate() {
        prefix[i + 1] = prefix[i] + u32::from(*v > 0.0);
    }
    for (i, o) in out.iter_mut().enumerate() {
        let (lo, hi) = (i.saturating_sub(r), (i + r + 1).min(n));
        *o = if prefix[hi] > prefix[lo] { 1.0 } else { 0.0 };
    }
}

/// An output pixel from the filled finest level's value `filled`, whether
/// it's within reach, and the input there (`px`, premultiplied).
pub(crate) fn shade(alpha: usize, filled: Rgba, reached: bool, px: Rgba) -> Rgba {
    let c = if filled[3] > 0.0 { [filled[0] / filled[3], filled[1] / filled[3], filled[2] / filled[3]] } else { [0.0; 3] };
    match (alpha, reached) {
        (0, true) => [c[0], c[1], c[2], 1.0],
        (0, false) => color::TRANSPARENT,
        (_, true) => [c[0], c[1], c[2], px[3]],
        (_, false) => {
            let s = color::unpremultiply(px);
            [s[0], s[1], s[2], px[3]]
        }
    }
}

/// Edge Extend (see the op's help): input 0 the image, input 1 an
/// optional matte.
pub(crate) fn edge_extend(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: EdgeExtend = ops::parse(params, "edge_extend")?;
    let src = inputs.first().copied().flatten().ok_or_else(|| anyhow!("input 'input' is not connected"))?;
    let matte = inputs.get(1).copied().flatten();
    let plan = plan(&p, src.format, src.window())?;
    let r = plan.region;
    let mut out = Image::blank(src.format, r);
    if r.is_empty() {
        return Ok(out);
    }
    let (w, h) = (r.width(), r.height());
    let mut pyramid = vec![(0..w * h).into_par_iter().map(|i| seed(src, matte, &plan, r.x0 + (i % w) as i32, r.y0 + (i / w) as i32)).collect::<Vec<_>>()];
    for k in 1..plan.levels.len() {
        let (fw, fh) = plan.levels[k - 1];
        let next = pull(&pyramid[k - 1], fw, fh);
        pyramid.push(next);
    }
    let reached = (!plan.fill_all).then(|| reach(&pyramid[0], w, h, plan.radius));
    let mut filled = pyramid.pop().unwrap_or_default();
    for k in (0..pyramid.len()).rev() {
        let (cw, ch) = plan.levels[k + 1];
        filled = push(&pyramid[k], plan.levels[k].0, &filled, cw, ch);
    }
    out.pixels.par_iter_mut().enumerate().for_each(|(i, o)| {
        let inside = reached.as_ref().is_none_or(|m| m[i] > 0.0);
        *o = shade(plan.alpha, filled[i], inside, src.at(r.x0 + (i % w) as i32, r.y0 + (i / w) as i32));
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::image::{IRect, Image, Size};
    use crate::ops::{eval, EvalContext};
    use serde_json::{json, Value};

    const CTX: EvalContext = EvalContext { frame: 1, write_outputs: false };

    fn run(params: Value, src: &Image, matte: Option<&Image>) -> Image {
        eval("edge_extend", &params, &[Some(src), matte], &CTX).unwrap()
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    /// A 40×30 frame holding data at (9..21, 9..21): an orange square at
    /// (10..20, 10..20), its one-pixel edge half covered.
    fn keyed() -> Image {
        let mut img = Image::blank(Size { width: 40, height: 30 }, IRect::new(9, 9, 21, 21));
        for y in 9..21 {
            for x in 9..21 {
                let edge = !(10..20).contains(&x) || !(10..20).contains(&y);
                let px = if edge { [0.9 * 0.5, 0.4 * 0.5, 0.1 * 0.5, 0.5] } else { [0.9, 0.4, 0.1, 1.0] };
                img.set(x - 9, y - 9, px);
            }
        }
        img
    }

    #[test]
    fn fill_everything_reaches_the_corners_with_the_edge_color() {
        let out = run(json!({ "fill_all": true }), &keyed(), None);
        assert_eq!(out.window(), IRect::new(0, 0, 40, 30));
        for (x, y) in [(0, 0), (39, 29), (39, 0), (0, 29), (25, 15)] {
            assert!(close(out.at(x, y), [0.9, 0.4, 0.1, 1.0]), "({x}, {y}): {:?}", out.at(x, y));
        }
        // Inside the matte: unchanged (straight, solid alpha).
        assert!(close(out.at(15, 15), [0.9, 0.4, 0.1, 1.0]));
    }

    #[test]
    fn the_fill_mixes_nearby_colors_without_a_dark_fringe() {
        // Left half red, right half blue, a transparent band between.
        let mut img = Image::new(32, 8);
        for y in 0..8 {
            for x in 0..12 {
                img.set(x, y, [1.0, 0.0, 0.0, 1.0]);
            }
            for x in 20..32 {
                img.set(x, y, [0.0, 0.0, 1.0, 1.0]);
            }
        }
        let out = run(json!({ "fill_all": true }), &img, None);
        for x in 12..20 {
            let px = out.at(x, 4);
            // Every filled pixel is a mix of red and blue: no darkening.
            assert!((px[0] + px[2] - 1.0).abs() < 1e-3, "x {x}: {px:?}");
        }
        assert!(out.at(12, 4)[0] > out.at(19, 4)[0], "nearer red leans red");
    }

    #[test]
    fn distance_stops_the_fill() {
        let out = run(json!({ "fill_all": false, "distance": 4 }), &keyed(), None);
        // The data (9..21) grown by 4 px.
        assert_eq!(out.window(), IRect::new(5, 5, 25, 25));
        assert!(close(out.at(24, 15), [0.9, 0.4, 0.1, 1.0]), "{:?}", out.at(24, 15));
        assert_eq!(out.at(26, 15), [0.0; 4]);
    }

    #[test]
    fn original_alpha_keeps_the_input_alpha_with_straight_color() {
        let src = keyed();
        let out = run(json!({ "fill_all": true, "alpha": "original (unpremultiplied)" }), &src, None);
        for (x, y) in [(0, 0), (9, 15), (15, 15)] {
            assert_eq!(out.at(x, y)[3], src.at(x, y)[3]);
        }
        assert!(close(out.at(9, 15), [0.9, 0.4, 0.1, 0.5]), "the half-covered fringe, straight: {:?}", out.at(9, 15));
        assert!(close(out.at(0, 0), [0.9, 0.4, 0.1, 0.0]));
    }

    #[test]
    fn inverted_matte_on_a_screen_plate_makes_a_clean_plate() {
        // A green plate, a gray subject in the middle, the subject's matte.
        let (w, h) = (48, 32);
        let green = [0.1, 0.7, 0.15, 1.0];
        let mut plate = Image::filled(w, h, green);
        let mut matte = Image::new(w, h);
        for y in 10..22 {
            for x in 18..30 {
                plate.set(x, y, [0.5, 0.5, 0.5, 1.0]);
                matte.set(x, y, [1.0; 4]);
            }
        }
        let clean = run(json!({ "fill_all": true, "invert": true }), &plate, Some(&matte));
        assert_eq!(clean.window(), IRect::new(0, 0, w as i32, h as i32));
        for px in &clean.pixels {
            assert!(close(*px, green), "{px:?}");
        }
        // The luminance of a matte works too.
        let clean = run(json!({ "fill_all": true, "invert": true, "matte_channel": "luminance" }), &plate, Some(&matte));
        assert!(close(clean.at(24, 16), green));
    }

    #[test]
    fn data_window_overscan_and_empty_input() {
        let f = Size { width: 20, height: 10 };
        let mut img = Image::blank(f, IRect::new(-4, 2, 6, 6));
        img.pixels.fill([0.2, 0.3, 0.4, 1.0]);
        let out = run(json!({ "fill_all": true }), &img, None);
        assert_eq!(out.window(), IRect::new(-4, 0, 20, 10));
        assert!(close(out.at(19, 9), [0.2, 0.3, 0.4, 1.0]));
        let empty = Image::blank(f, IRect::default());
        assert!(run(json!({}), &empty, None).window().is_empty());
    }
}
