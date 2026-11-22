//! Matte Combine: several mattes into one.
//!
//! Input 1 is the base matte; each further connected input (2, 3, …, see
//! [`crate::ops::growable`]) is combined into the running result, in port
//! order, by its own operation. Per input, in port order:
//!
//! - `channels`: alpha (default), red, green, blue, luminance (the mask
//!   channels' weights) or a plane's name (`matte.hero`); values are read
//!   clamped to 0–1, as masks are;
//! - `inverts`: 1 inverts that input's matte (animatable, stepping);
//! - `operations`: 0 union, 1 subtract, 2 intersect, 3 difference
//!   (animatable, stepping; entry 0 is unused).
//!
//! Missing entries are the defaults (alpha, not inverted, union). With
//! result `r` and an input's matte `m`, the operations are min / max ones
//! (union max(r, m), subtract max(r − m, 0), intersect min(r, m),
//! difference |r − m|), exact for hard mattes; Soft uses the multiplicative
//! forms instead (r + m − rm, r(1 − m), rm, r + m − 2rm), which blend
//! overlapping soft edges more smoothly.
//!
//! The result comes out as a white matte (`output` "matte": R, G, B and A
//! all the result) or in input 1's alpha, its color and passes kept
//! ("input 1 + alpha", values copied as stored); `plane`, when set, also
//! gets it, as a pass.

use crate::image::{IRect, Image, Plane, Size};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use serde_json::Value;

/// The operations, by code (their index).
pub const OPERATIONS: [&str; 4] = ["union", "subtract", "intersect", "difference"];
/// The channel choices besides plane names.
pub const CHANNELS: [&str; 5] = ["alpha", "red", "green", "blue", "luminance"];
/// The `output` choices.
pub const OUTPUTS: &[&str] = &["matte", "input 1 + alpha"];
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Union,
    Subtract,
    Intersect,
    Difference,
}

impl Op {
    /// The operation a stored code stands for (rounded, clamped).
    pub fn from_code(v: f64) -> Op {
        match if v.is_finite() { v.round().clamp(0.0, 3.0) as u8 } else { 0 } {
            1 => Op::Subtract,
            2 => Op::Intersect,
            3 => Op::Difference,
            _ => Op::Union,
        }
    }
}

/// An operation given as a name or a code, as its code (MCP / Python
/// accept either); None when it's neither.
pub fn operation_code(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => OPERATIONS.iter().position(|o| o.eq_ignore_ascii_case(s.trim())).map(|i| i as f64),
        Value::Number(n) => n.as_f64().filter(|c| (0.0..=3.0).contains(c) && c.fract() == 0.0),
        _ => None,
    }
}

/// An invert given as a bool or 0 / 1, as its code.
pub fn invert_code(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::Number(n) => n.as_f64().filter(|c| *c == 0.0 || *c == 1.0),
        _ => None,
    }
}

/// `r` and `m` combined by `op` (min / max, or with `soft` the
/// multiplicative forms).
#[inline]
pub fn combine(r: f32, m: f32, op: Op, soft: bool) -> f32 {
    match (op, soft) {
        (Op::Union, false) => r.max(m),
        (Op::Subtract, false) => (r - m).max(0.0),
        (Op::Intersect, false) => r.min(m),
        (Op::Difference, false) => (r - m).abs(),
        (Op::Union, true) => r + m - r * m,
        (Op::Subtract, true) => r * (1.0 - m),
        (Op::Intersect, true) => r * m,
        (Op::Difference, true) => r + m - 2.0 * r * m,
    }
}

/// One input's part: where its matte comes from, inverted or not, how it
/// combines.
struct Part<'a> {
    img: &'a Image,
    plane: Option<&'a Plane>,
    channel: usize,
    invert: bool,
    op: Op,
}

impl Part<'_> {
    /// The matte at (x, y), clamped and inverted as set.
    #[inline]
    fn at(&self, x: i32, y: i32) -> f32 {
        let v = match self.plane {
            Some(p) => self.img.plane_value(p, x, y),
            None => {
                let px = self.img.at(x, y);
                if self.channel == 4 { px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2] } else { px[self.channel] }
            }
        }
        .clamp(0.0, 1.0);
        if self.invert { 1.0 - v } else { v }
    }

    /// The matte where the input has no data.
    fn outside(&self) -> f32 {
        if self.invert { 1.0 } else { 0.0 }
    }
}

fn list(params: &Value, name: &str) -> Vec<Value> {
    params.get(name).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The combined matte of `inputs` (`params` resolved at the frame).
pub fn eval(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let base = inputs.first().copied().flatten().ok_or_else(|| anyhow!("input 'matte' is not connected"))?;
    let (channels, inverts, operations) = (list(params, "channels"), list(params, "inverts"), list(params, "operations"));
    let soft = params.get("soft").and_then(Value::as_bool).unwrap_or(false);
    let keep_input = match params.get("output").and_then(Value::as_str).unwrap_or("matte") {
        "matte" => false,
        "input 1 + alpha" => true,
        other => bail!("unknown output '{other}'"),
    };
    let plane = params.get("plane").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let parts: Vec<Part> = inputs
        .iter()
        .enumerate()
        .filter_map(|(k, img)| img.map(|img| (k, img)))
        .map(|(k, img)| {
            let channel = channels.get(k).and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()).unwrap_or("alpha");
            let (channel, plane) = match CHANNELS.iter().position(|c| *c == channel) {
                Some(i) => ([3, 0, 1, 2, 4][i], None),
                None => (0, Some(img.plane(channel).ok_or_else(|| anyhow!("input {} has no channel '{channel}'", k + 1))?)),
            };
            let invert = inverts.get(k).and_then(Value::as_f64).is_some_and(|v| v >= 0.5);
            let op = Op::from_code(operations.get(k).and_then(Value::as_f64).unwrap_or(0.0));
            Ok(Part { img, plane, channel, invert, op })
        })
        .collect::<Result<_>>()?;
    let run = |m: &dyn Fn(&Part) -> f32| parts.iter().skip(1).fold(m(&parts[0]), |r, p| combine(r, m(p), p.op, soft));
    // Where no input has data the result is what their outsides combine
    // to: anything but 0 there covers the whole frame.
    let format: Size = base.format;
    let windows = parts.iter().fold(IRect::default(), |u, p| u.union(&p.img.window()));
    let outside = run(&|p: &Part| p.outside());
    let mut window = if outside != 0.0 { windows.union(&IRect::frame(format)) } else { windows };
    if keep_input {
        window = window.union(&base.window());
    }
    let mut out = Image::blank(format, window);
    let w = out.width.max(1);
    let (x0, y0) = (out.x0, out.y0);
    let values: Vec<f32> = (0..out.width * out.height)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
            run(&|p: &Part| p.at(x, y))
        })
        .collect();
    out.pixels.par_iter_mut().enumerate().zip(values.par_iter()).for_each(|((i, px), &r)| {
        *px = if keep_input {
            let mut c = base.at(x0 + (i % w) as i32, y0 + (i / w) as i32);
            c[3] = r;
            c
        } else {
            [r; 4]
        };
    });
    if keep_input {
        out.keep_planes(base);
    }
    if !plane.is_empty() {
        *out.plane_mut(&plane) = values;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const F: Size = Size { width: 4, height: 1 };

    /// A 4 × 1 image whose alpha is `a` (color 0.5 grey, premultiplied by it).
    fn matte(a: [f32; 4]) -> Image {
        let mut i = Image::blank(F, IRect::new(0, 0, 4, 1));
        for (p, v) in i.pixels.iter_mut().zip(a) {
            *p = [0.5 * v, 0.25 * v, 0.0, v];
        }
        i
    }

    fn alphas(img: &Image) -> Vec<f32> {
        (0..4).map(|x| img.at(x, 0)[3]).collect()
    }

    #[test]
    fn each_operation_follows_its_formula() {
        let (r, m) = (0.75f32, 0.5f32);
        let want = [(0.75, 0.875), (0.25, 0.375), (0.5, 0.375), (0.25, 0.5)];
        for (code, (hard, soft)) in want.iter().enumerate() {
            let op = Op::from_code(code as f64);
            assert_eq!(combine(r, m, op, false), *hard, "{op:?}");
            assert_eq!(combine(r, m, op, true), *soft, "{op:?} soft");
        }
        // Hard mattes come out the same either way.
        for op in [Op::Union, Op::Subtract, Op::Intersect, Op::Difference] {
            for (a, b) in [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)] {
                assert_eq!(combine(a, b, op, false), combine(a, b, op, true), "{op:?} {a} {b}");
            }
        }
    }

    #[test]
    fn inputs_combine_in_port_order() {
        let a = matte([1.0, 1.0, 1.0, 0.0]);
        let b = matte([0.0, 1.0, 1.0, 1.0]);
        let c = matte([0.0, 0.0, 1.0, 0.0]);
        // (A ∪ B) − C.
        let p = json!({"operations": [0, 0, 1]});
        let out = eval(&p, &[Some(&a), Some(&b), Some(&c)]).unwrap();
        assert_eq!(alphas(&out), [1.0, 1.0, 0.0, 1.0]);
        assert_eq!(out.at(1, 0), [1.0; 4], "a white matte");
        // An unconnected input in between is skipped.
        let out = eval(&json!({"operations": [0, 2, 1]}), &[Some(&a), None, Some(&c)]).unwrap();
        assert_eq!(alphas(&out), [1.0, 1.0, 0.0, 0.0]);
        // Missing entries: alpha, not inverted, union.
        assert_eq!(alphas(&eval(&json!({}), &[Some(&a), Some(&b)]).unwrap()), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn channels_planes_and_inverts() {
        let mut a = matte([1.0, 0.5, 0.0, 0.0]);
        a.plane_mut("matte.hero").copy_from_slice(&[0.0, 0.0, 1.0, 1.0]);
        // Red is 0.5 × alpha; luminance mixes red and green.
        let red = eval(&json!({"channels": ["red"]}), &[Some(&a)]).unwrap();
        assert_eq!(alphas(&red), [0.5, 0.25, 0.0, 0.0]);
        let luma = eval(&json!({"channels": ["luminance"]}), &[Some(&a)]).unwrap();
        assert!((luma.at(0, 0)[3] - (0.5 * 0.2126 + 0.25 * 0.7152)).abs() < 1e-6);
        let hero = eval(&json!({"channels": ["matte.hero"], "inverts": [1]}), &[Some(&a)]).unwrap();
        assert_eq!(alphas(&hero), [1.0, 1.0, 0.0, 0.0]);
        let e = eval(&json!({"channels": ["", "matte.nope"]}), &[Some(&a), Some(&a)]).err().unwrap().to_string();
        assert!(e.contains("input 2 has no channel 'matte.nope'"), "{e}");
        assert!(eval(&json!({}), &[None]).is_err());
    }

    #[test]
    fn the_window_covers_the_frame_when_outsides_are_on() {
        let small = {
            let mut i = Image::blank(Size { width: 8, height: 8 }, IRect::new(2, 2, 4, 4));
            i.pixels.fill([1.0; 4]);
            i
        };
        let plain = eval(&json!({}), &[Some(&small)]).unwrap();
        assert_eq!(plain.window(), IRect::new(2, 2, 4, 4));
        let inverted = eval(&json!({"inverts": [1]}), &[Some(&small)]).unwrap();
        assert_eq!(inverted.window(), IRect::new(0, 0, 8, 8));
        assert_eq!((inverted.at(0, 0)[3], inverted.at(2, 2)[3]), (1.0, 0.0));
        // Intersecting with an inverted matte clears the outside again.
        let both = eval(&json!({"inverts": [0, 1], "operations": [0, 2]}), &[Some(&small), Some(&small)]).unwrap();
        assert_eq!(both.window(), IRect::new(2, 2, 4, 4));
    }

    #[test]
    fn output_into_input_1s_alpha_and_a_plane() {
        let mut a = matte([1.0, 1.0, 0.0, 0.0]);
        a.plane_mut("depth").fill(3.0);
        let b = matte([0.0, 1.0, 1.0, 0.0]);
        let p = json!({"operations": [0, 2], "output": "input 1 + alpha", "plane": "matte.combined"});
        let out = eval(&p, &[Some(&a), Some(&b)]).unwrap();
        assert_eq!(out.at(1, 0), [0.5, 0.25, 0.0, 1.0]);
        assert_eq!(out.at(0, 0), [0.5, 0.25, 0.0, 0.0], "color kept, alpha replaced");
        assert_eq!((out.plane_at("depth", 0, 0), out.plane_at("matte.combined", 1, 0), out.plane_at("matte.combined", 0, 0)), (3.0, 1.0, 0.0));
        assert!(eval(&json!({"output": "nope"}), &[Some(&a)]).is_err());
    }

    #[test]
    fn codes_from_names_and_numbers() {
        assert_eq!((operation_code(&json!("Subtract")), operation_code(&json!(3)), operation_code(&json!("xor")), operation_code(&json!(1.5))), (Some(1.0), Some(3.0), None, None));
        assert_eq!((invert_code(&json!(true)), invert_code(&json!(0)), invert_code(&json!(2))), (Some(1.0), Some(0.0), None));
        // Stored codes round (a hand-edited 0.9 is subtract's 1).
        assert_eq!((Op::from_code(0.9), Op::from_code(7.0), Op::from_code(f64::NAN)), (Op::Subtract, Op::Difference, Op::Union));
    }
}
