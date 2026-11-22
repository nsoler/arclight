//! Shuffle's routing table: any channel of any of its inputs into any
//! output channel or pass.
//!
//! A Shuffle grows inputs (`in`, then `2`, `3`, …, see
//! [`crate::ops::growable`]). Its usual params (In, Red … Alpha from, Out,
//! Remove) still work on input 1 as they always have; `routes` then copies
//! more channels in, in order, each from the original input images:
//!
//! - a route is `[source, dest]`;
//! - `source` is `"<input>:<channel>"`, the input numbered from 1 as the
//!   ports are, the channel `R`, `G`, `B`, `A`, a plane's name (`depth`,
//!   `N.y`, `matte.Cube1`) or `0` / `1` (a constant: the input needn't be
//!   connected);
//! - `dest` is `R`, `G`, `B` or `A` (the output's color) or a plane's name,
//!   replaced when the output has it, else added.
//!
//! The output keeps input 1's frame; its data window takes in the windows
//! of the inputs the routes read (outside an input's window its channels
//! read 0, as everywhere). With no routes nothing here runs, so Shuffles
//! from before routing render exactly as they did. Remove runs after the
//! routes, so a pass can be routed and then dropped.

use crate::image::{IRect, Image, Plane};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use serde_json::Value;

const COLOR: [&str; 4] = ["R", "G", "B", "A"];

/// One parsed route.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// The source input, from 0 (`"1:…"` is input 0).
    pub input: usize,
    /// The source channel: R, G, B, A, 0, 1 or a plane name.
    pub channel: String,
    /// The destination: R, G, B, A or a plane name.
    pub dest: String,
}

impl Route {
    fn constant(&self) -> Option<f32> {
        match self.channel.as_str() {
            "0" => Some(0.0),
            "1" => Some(1.0),
            _ => None,
        }
    }
}

/// `"<input>:<channel>"` → (input from 0, channel).
pub fn parse_source(s: &str) -> Result<(usize, String)> {
    let (n, ch) = s.trim().split_once(':').ok_or_else(|| anyhow!("route source '{s}' should be <input>:<channel>, e.g. 2:A"))?;
    let n: usize = n.trim().parse().map_err(|_| anyhow!("route source '{s}': '{n}' isn't an input number"))?;
    let ch = ch.trim();
    if n == 0 {
        bail!("route source '{s}': inputs are numbered from 1");
    }
    if ch.is_empty() {
        bail!("route source '{s}' names no channel");
    }
    Ok((n - 1, ch.to_string()))
}

/// The `routes` param: a list of `[source, dest]` (null or missing: none).
pub fn parse(v: &Value) -> Result<Vec<Route>> {
    let Some(list) = v.as_array() else {
        return if v.is_null() { Ok(Vec::new()) } else { Err(anyhow!("routes should be a list of [source, dest]")) };
    };
    list.iter()
        .map(|r| {
            let pair = r.as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_str()?, a[1].as_str()?)));
            let (src, dest) = pair.ok_or_else(|| anyhow!("route {r} should be [\"<input>:<channel>\", \"<dest>\"]"))?;
            let (input, channel) = parse_source(src)?;
            let dest = dest.trim();
            if dest.is_empty() {
                bail!("route from '{src}' has no destination");
            }
            Ok(Route { input, channel, dest: dest.to_string() })
        })
        .collect()
}

/// The inputs (from 0) whose pixels the routes read (constants read none).
pub fn inputs_read(routes: &[Route]) -> Vec<usize> {
    let mut out: Vec<usize> = routes.iter().filter(|r| r.constant().is_none()).map(|r| r.input).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `img` has channel `name` (R, G, B, A or a plane).
fn has_channel(img: &Image, name: &str) -> bool {
    COLOR.contains(&name) || img.plane(name).is_some()
}

/// `img` with its pixels and planes stored over `window` instead (0 /
/// transparent where it had nothing).
fn rewindow(img: &Image, window: IRect) -> Image {
    let mut out = Image::blank(img.format, window);
    let w = out.width.max(1);
    let at = |i: usize| (window.x0 + (i % w) as i32, window.y0 + (i / w) as i32);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, p)| {
        let (x, y) = at(i);
        *p = img.at(x, y);
    });
    out.planes = img
        .planes
        .iter()
        .map(|pl| {
            let mut data = vec![0.0; out.width * out.height];
            data.par_iter_mut().enumerate().for_each(|(i, v)| {
                let (x, y) = at(i);
                *v = img.plane_value(pl, x, y);
            });
            Plane { name: pl.name.clone(), data }
        })
        .collect();
    out
}

/// `base` (the Shuffle's result from input 1) with `routes` applied, each
/// reading `inputs` (the Shuffle's own, unshuffled). Errors name the route
/// and what's missing.
pub fn apply(base: Image, inputs: &[Option<&Image>], routes: &[Route]) -> Result<Image> {
    if routes.is_empty() {
        return Ok(base);
    }
    let src = |r: &Route| -> Result<&Image> {
        let img = inputs.get(r.input).copied().flatten().ok_or_else(|| anyhow!("route {}:{}: input {} is not connected", r.input + 1, r.channel, r.input + 1))?;
        if !has_channel(img, &r.channel) {
            bail!("route {}:{}: input {} has no channel '{}'", r.input + 1, r.channel, r.input + 1, r.channel);
        }
        Ok(img)
    };
    // Check every route before doing anything.
    for r in routes.iter().filter(|r| r.constant().is_none()) {
        src(r)?;
    }
    let window = inputs_read(routes).into_iter().filter_map(|k| inputs.get(k).copied().flatten()).fold(base.window(), |u, img| u.union(&img.window()));
    let mut out = if window == base.window() { base } else { rewindow(&base, window) };
    let w = out.width.max(1);
    let (x0, y0) = (out.x0, out.y0);
    for r in routes {
        // This route's values over the output's window.
        let values: Vec<f32> = match r.constant() {
            Some(c) => vec![c; out.width * out.height],
            None => {
                let img = src(r)?;
                let plane = img.plane(&r.channel);
                let c = COLOR.iter().position(|k| *k == r.channel);
                (0..out.width * out.height)
                    .into_par_iter()
                    .map(|i| {
                        let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
                        match (c, plane) {
                            (Some(c), _) => img.at(x, y)[c],
                            (None, Some(p)) => img.plane_value(p, x, y),
                            (None, None) => 0.0,
                        }
                    })
                    .collect()
            }
        };
        match COLOR.iter().position(|k| *k == r.dest) {
            Some(c) => out.pixels.par_iter_mut().zip(values.par_iter()).for_each(|(p, v)| p[c] = *v),
            None => *out.plane_mut(&r.dest) = values,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Size;
    use serde_json::json;

    fn img(window: IRect, rgba: [f32; 4], planes: &[(&str, f32)]) -> Image {
        let mut i = Image::blank(Size { width: 8, height: 6 }, window);
        i.pixels.fill(rgba);
        for (name, v) in planes {
            i.plane_mut(name).fill(*v);
        }
        i
    }

    #[test]
    fn sources_and_lists_parse() {
        assert_eq!(parse_source("2:N.y").unwrap(), (1, "N.y".into()));
        assert_eq!(parse_source(" 1 : matte.Cube1 ").unwrap(), (0, "matte.Cube1".into()));
        for bad in ["A", "0:A", "x:A", "2:"] {
            assert!(parse_source(bad).is_err(), "{bad}");
        }
        assert!(parse(&json!(null)).unwrap().is_empty() && parse(&json!([])).unwrap().is_empty());
        let r = parse(&json!([["2:A", "A"], ["3:depth", "depth"]])).unwrap();
        assert_eq!(r[1], Route { input: 2, channel: "depth".into(), dest: "depth".into() });
        for bad in [json!("2:A"), json!([["2:A"]]), json!([["2:A", ""]]), json!([[1, "A"]])] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn no_routes_leaves_the_image_alone() {
        let a = img(IRect::new(0, 0, 8, 6), [0.1, 0.2, 0.3, 0.4], &[("depth", 5.0)]);
        let out = apply(a.clone(), &[Some(&a)], &[]).unwrap();
        assert!(out.pixels == a.pixels && out.planes == a.planes && out.window() == a.window());
    }

    #[test]
    fn channels_come_from_any_input_into_color_or_passes() {
        let full = IRect::new(0, 0, 8, 6);
        let a = img(full, [0.1, 0.2, 0.3, 0.4], &[]);
        let b = img(full, [0.5, 0.6, 0.7, 0.8], &[("N.x", 0.25)]);
        let c = img(full, [0.0; 4], &[("depth", 9.0)]);
        let routes = parse(&json!([["2:A", "A"], ["3:depth", "depth"], ["2:N.x", "specular.R"], ["1:R", "G"], ["4:1", "B"]])).unwrap();
        let out = apply(a.clone(), &[Some(&a), Some(&b), Some(&c)], &routes).unwrap();
        assert_eq!(out.at(3, 3), [0.1, 0.1, 1.0, 0.8]);
        assert_eq!((out.plane_at("depth", 3, 3), out.plane_at("specular.R", 3, 3)), (9.0, 0.25));
        // Routes read the inputs as they came in: 1:R is input 1's red even
        // after an earlier route wrote the output's.
        let swap = parse(&json!([["1:G", "R"], ["1:R", "G"]])).unwrap();
        assert_eq!(apply(a.clone(), &[Some(&a)], &swap).unwrap().at(0, 0), [0.2, 0.1, 0.3, 0.4]);
    }

    #[test]
    fn the_window_takes_in_the_inputs_read() {
        let a = img(IRect::new(0, 0, 4, 3), [1.0; 4], &[("depth", 2.0)]);
        let b = img(IRect::new(2, 2, 8, 6), [0.5; 4], &[]);
        let out = apply(a.clone(), &[Some(&a), Some(&b)], &parse(&json!([["2:A", "A"]])).unwrap()).unwrap();
        assert_eq!(out.window(), IRect::new(0, 0, 8, 6));
        // Input 1's pixels and planes where it had them, nothing elsewhere;
        // B's alpha where B is, 0 where it isn't.
        assert_eq!(out.at(0, 0), [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(out.at(3, 2), [1.0, 1.0, 1.0, 0.5]);
        assert_eq!(out.at(7, 5), [0.0, 0.0, 0.0, 0.5]);
        assert_eq!((out.plane_at("depth", 1, 1), out.plane_at("depth", 7, 5)), (2.0, 0.0));
        // A constant reads no input: the window stays.
        let k = apply(a.clone(), &[Some(&a)], &parse(&json!([["5:1", "A"]])).unwrap()).unwrap();
        assert_eq!(k.window(), a.window());
    }

    #[test]
    fn errors_say_what_is_missing() {
        let a = img(IRect::new(0, 0, 8, 6), [0.0; 4], &[]);
        let e = apply(a.clone(), &[Some(&a), None], &parse(&json!([["2:A", "A"]])).unwrap()).err().unwrap().to_string();
        assert!(e.contains("input 2 is not connected"), "{e}");
        let e = apply(a.clone(), &[Some(&a)], &parse(&json!([["1:N.w", "A"]])).unwrap()).err().unwrap().to_string();
        assert!(e.contains("no channel 'N.w'"), "{e}");
    }
}
