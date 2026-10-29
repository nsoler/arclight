//! 2D point tracking: find a pattern from one frame in the next by
//! normalized cross-correlation (NCC) on luminance, refined to sub-pixel
//! accuracy with a parabola fit through the correlation peak.
//!
//! Positions are in image pixels with y down, pixel `i` covering `[i, i+1)`,
//! matching the shape ops and the viewer overlays.

use crate::graph::{Evaluator, Graph};
use crate::image::Image;
use crate::ops::{self, EvalContext};
use crate::render::Progress;
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;

pub type Pt = [f32; 2];

/// Number of tracks a Tracker node holds.
pub const TRACKS: usize = 4;

/// Luminance of an image, for matching.
pub struct Luma {
    pub width: usize,
    pub height: usize,
    px: Vec<f32>,
}

impl Luma {
    /// Rec. 709 luminance of the (premultiplied) colour.
    pub fn new(img: &Image) -> Luma {
        let img = img.flat();
        let px = img.pixels.par_iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
        Luma { width: img.width, height: img.height, px }
    }

    fn get(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.px[y * self.width + x]
    }

    /// Bilinear sample at image position `(x, y)`, clamped to the edge.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        // Pixel centers sit at i + 0.5.
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = self.get(x0, y0) * (1.0 - fx) + self.get(x0 + 1, y0) * fx;
        let bot = self.get(x0, y0 + 1) * (1.0 - fx) + self.get(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bot * fy
    }

    /// The `(2r+1)²` samples of a square window centered on `c` and
    /// warped by `m` (sample `k` of the grid reads `c + m·k`), row-major.
    pub fn window(&self, c: Pt, r: usize, m: &Mat) -> Vec<f32> {
        let n = 2 * r + 1;
        (0..n * n)
            .map(|i| {
                let k = [(i % n) as f32 - r as f32, (i / n) as f32 - r as f32];
                let q = mul(m, k);
                self.at(c[0] + q[0], c[1] + q[1])
            })
            .collect()
    }

    pub fn contains(&self, p: Pt) -> bool {
        p[0] >= 0.0 && p[1] >= 0.0 && p[0] < self.width as f32 && p[1] < self.height as f32
    }
}

/// A 2×2 matrix `[a, b, c, d]` (rows `a b`, `c d`).
pub type Mat = [f32; 4];

pub const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0];

fn mul(m: &Mat, k: Pt) -> Pt {
    [m[0] * k[0] + m[1] * k[1], m[2] * k[0] + m[3] * k[1]]
}

fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1]]
}

/// How a track's feature has turned and grown since its pattern was taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    /// Radians; positive is clockwise on screen (y down).
    pub angle: f32,
    pub scale: f32,
}

impl Default for Warp {
    fn default() -> Warp {
        Warp { angle: 0.0, scale: 1.0 }
    }
}

impl Warp {
    /// This warp followed by `next`.
    pub fn then(&self, next: Warp) -> Warp {
        Warp { angle: self.angle + next.angle, scale: self.scale * next.scale }
    }

    /// As stored in a Tracker's `track<i>_warp` param: `[degrees, scale]`.
    pub fn to_param(&self) -> [f64; 2] {
        [(self.angle as f64).to_degrees(), self.scale as f64]
    }

    pub fn matrix(&self) -> Mat {
        let (s, c) = self.angle.sin_cos();
        [self.scale * c, -self.scale * s, self.scale * s, self.scale * c]
    }
}

/// Below this standard deviation a window is flat: rounding noise, with
/// nothing to match.
const FLAT: f32 = 1e-3;

/// A pattern ready for matching: zero mean, unit length.
pub struct Pattern {
    r: usize,
    v: Vec<f32>,
}

impl Pattern {
    /// The `(2r+1)²` pattern centered on `c`, sampled through `m` (so a
    /// pattern re-taken from a turned feature keeps the first orientation),
    /// or `None` when it is flat (nothing to lock onto).
    pub fn new(img: &Luma, c: Pt, r: usize, m: &Mat) -> Option<Pattern> {
        let mut v = img.window(c, r, m);
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        v.iter_mut().for_each(|x| *x -= mean);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm < FLAT * (v.len() as f32).sqrt() {
            return None;
        }
        v.iter_mut().for_each(|x| *x /= norm);
        Some(Pattern { r, v })
    }
}

/// Search for `pat` within `s` pixels of `guess` in `img`, with the
/// feature turned and scaled by about `warp`. `motion` says what may
/// change: with rotation (and scale), `warp` is re-estimated too, so the
/// pattern from the first frame keeps matching a turning feature. Returns
/// the best match position (sub-pixel) and its correlation, −1 to 1.
pub fn find(pat: &Pattern, img: &Luma, guess: Pt, s: usize, warp: &mut Warp, motion: Solve) -> (Pt, f32) {
    let (rotate, scale) = (motion != Solve::Translate, motion == Solve::Scale);
    let (mut p, _) = search(pat, img, guess, s, &warp.matrix());
    if rotate {
        // Coarse steps first, then a short search where the turned
        // pattern now fits best.
        let score = |w: &Warp| ncc(pat, img, p, &w.matrix());
        let best = |cands: Vec<Warp>| cands.into_iter().map(|w| (score(&w), w)).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, w)| w);
        let step = 3f32.to_radians();
        *warp = best((-2..=2).map(|k| Warp { angle: warp.angle + k as f32 * step, ..*warp }).collect()).unwrap_or(*warp);
        if scale {
            *warp = best((-1..=1).map(|k| Warp { scale: warp.scale * (1.0 + 0.04 * k as f32), ..*warp }).collect()).unwrap_or(*warp);
        }
        (p, _) = search(pat, img, p, 2, &warp.matrix());
    }
    // A parabola through whole-pixel (or whole-step) neighbours misplaces
    // an asymmetric peak; fitting again through ever closer samples
    // shrinks that error.
    for k in [1.0, 0.5, 0.25, 0.125] {
        let m = warp.matrix();
        p[0] = refine(p[0], 0.5 * k, |x| ncc(pat, img, [x, p[1]], &m));
        p[1] = refine(p[1], 0.5 * k, |y| ncc(pat, img, [p[0], y], &m));
        if rotate {
            let w = *warp;
            warp.angle = refine(w.angle, k * 1f32.to_radians(), |a| ncc(pat, img, p, &Warp { angle: a, ..w }.matrix()));
        }
        if scale {
            let w = *warp;
            warp.scale = refine(w.scale, k * 0.02, |s| ncc(pat, img, p, &Warp { scale: s, ..w }.matrix())).clamp(0.25, 4.0);
        }
    }
    (p, ncc(pat, img, p, &warp.matrix()))
}

/// One parabola step toward the peak of `f` near `x`, sampling `h` either
/// side and moving at most `h`; `x` is kept when the step doesn't improve
/// on it.
fn refine(x: f32, h: f32, f: impl Fn(f32) -> f32) -> f32 {
    let (l, c, r) = (f(x - h), f(x), f(x + h));
    let d = l - 2.0 * c + r;
    if d >= 0.0 {
        return x;
    }
    let q = x + (0.5 * (l - r) / d).clamp(-1.0, 1.0) * h;
    if f(q) >= c { q } else { x }
}

/// Vertex offset (in sample steps, within ±½) of the parabola through
/// three equally spaced samples; 0 when they don't form a peak.
fn parabola(l: f32, c: f32, r: f32) -> f32 {
    let d = l - 2.0 * c + r;
    if d < 0.0 { (0.5 * (l - r) / d).clamp(-0.5, 0.5) } else { 0.0 }
}

/// Correlation of `pat` with the window centered on `c`, warped by `m`.
fn ncc(pat: &Pattern, img: &Luma, c: Pt, m: &Mat) -> f32 {
    let w = img.window(c, pat.r, m);
    let mean = w.iter().sum::<f32>() / w.len() as f32;
    let (mut dot, mut var) = (0.0, 0.0);
    for (a, b) in w.iter().zip(&pat.v) {
        dot += (a - mean) * b;
        var += (a - mean) * (a - mean);
    }
    if var < FLAT * FLAT * w.len() as f32 { 0.0 } else { (dot / var.sqrt()).clamp(-1.0, 1.0) }
}

/// One correlation search on the integer grid around `guess` (in the
/// pattern's frame, warped by `m`), with a parabola fit through the peak.
fn search(pat: &Pattern, img: &Luma, guess: Pt, s: usize, m: &Mat) -> (Pt, f32) {
    let r = pat.r;
    let pn = 2 * r + 1;
    let gn = 2 * (r + s) + 1;
    // The search area, sampled once on the guess's sub-pixel grid.
    let g = img.window(guess, r + s, m);
    // Summed-area tables of the values and their squares, for each
    // window's mean and variance in O(1).
    let mut sum = vec![0.0f64; (gn + 1) * (gn + 1)];
    let mut sq = vec![0.0f64; (gn + 1) * (gn + 1)];
    for y in 0..gn {
        for x in 0..gn {
            let v = g[y * gn + x] as f64;
            let i = (y + 1) * (gn + 1) + x + 1;
            sum[i] = v + sum[i - 1] + sum[i - gn - 1] - sum[i - gn - 2];
            sq[i] = v * v + sq[i - 1] + sq[i - gn - 1] - sq[i - gn - 2];
        }
    }
    let area = |t: &[f64], x: usize, y: usize| {
        let (x1, y1) = (x + pn, y + pn);
        t[y1 * (gn + 1) + x1] - t[y * (gn + 1) + x1] - t[y1 * (gn + 1) + x] + t[y * (gn + 1) + x]
    };
    let sn = 2 * s + 1;
    let n = (pn * pn) as f64;
    let scores: Vec<f32> = (0..sn * sn)
        .into_par_iter()
        .map(|o| {
            let (ox, oy) = (o % sn, o / sn);
            // The pattern has zero mean, so the window's mean drops out of the dot product.
            let mut dot = 0.0f32;
            for y in 0..pn {
                let row = &g[(oy + y) * gn + ox..][..pn];
                dot += row.iter().zip(&pat.v[y * pn..][..pn]).map(|(a, b)| a * b).sum::<f32>();
            }
            let (s1, s2) = (area(&sum, ox, oy), area(&sq, ox, oy));
            let var = (s2 - s1 * s1 / n).max(0.0);
            if var < (FLAT * FLAT) as f64 * n { 0.0 } else { (dot / var.sqrt() as f32).clamp(-1.0, 1.0) }
        })
        .collect();
    let best = (0..scores.len()).max_by(|&a, &b| scores[a].total_cmp(&scores[b])).unwrap_or(0);
    let (bx, by) = (best % sn, best / sn);
    // Parabola through the peak and its neighbours, per axis.
    let fit = |l: Option<f32>, c: f32, r: Option<f32>| match (l, r) {
        (Some(l), Some(r)) => parabola(l, c, r),
        _ => 0.0,
    };
    let at = |x: usize, y: usize| scores[y * sn + x];
    let fx = fit(bx.checked_sub(1).map(|x| at(x, by)), at(bx, by), (bx + 1 < sn).then(|| at(bx + 1, by)));
    let fy = fit(by.checked_sub(1).map(|y| at(bx, y)), at(bx, by), (by + 1 < sn).then(|| at(bx, by + 1)));
    let d = mul(m, [bx as f32 - s as f32 + fx, by as f32 - s as f32 + fy]);
    ([guess[0] + d[0], guess[1] + d[1]], at(bx, by))
}

/// A Tracker node's tracking settings.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Pattern and search box sizes, in pixels.
    pub pattern: usize,
    pub search: usize,
    /// Each track's own `[pattern, search]` sizes, where they differ from
    /// the shared ones (see [`box_sizes`]); empty means all shared.
    pub sizes: Vec<[usize; 2]>,
    /// Each track's pattern offset from its point (see [`offsets`]); empty
    /// means none.
    pub offsets: Vec<Pt>,
    /// Matches below this correlation lose the track.
    pub min_score: f32,
    /// Take a fresh pattern at every tracked frame, instead of keeping the
    /// one from the start frame. Follows gradual changes but drifts.
    pub update: bool,
    /// How the features may move: whether rotation and scale are tracked.
    pub motion: Solve,
}

impl Settings {
    pub fn from_params(p: &Value) -> Settings {
        let int = |k: &str, d: u64| p.get(k).and_then(Value::as_f64).map_or(d, |v| v.max(1.0) as u64) as usize;
        Settings {
            pattern: int("pattern", 31),
            search: int("search", 91),
            sizes: box_sizes(p).into_iter().map(|s| s.map(|x| x.max(1.0) as usize)).collect(),
            offsets: offsets(p),
            min_score: p.get("min_score").and_then(Value::as_f64).unwrap_or(0.7) as f32,
            update: p.get("update").and_then(Value::as_str) == Some("every frame"),
            motion: Solve::from_choice(p.get("motion").and_then(Value::as_str), Solve::Rotate),
        }
    }

    fn offset(&self, i: usize) -> Pt {
        self.offsets.get(i).copied().unwrap_or([0.0, 0.0])
    }

    /// Track `i`'s pattern radius and search reach around it.
    fn radii(&self, i: usize) -> (usize, usize) {
        let [pattern, search] = self.sizes.get(i).copied().unwrap_or([self.pattern, self.search]);
        let r = (pattern / 2).max(2);
        (r, (search / 2).saturating_sub(r).max(1))
    }
}

/// What a tracking run did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub frames: i64,
    /// Tracks that were lost, with the first frame they failed on.
    pub lost: Vec<(usize, i64)>,
}

/// One frame of a tracking run, per track: its position (`None` once
/// lost), its turn and growth since the run's start frame, and how well its
/// pattern matched (correlation, 1 = perfectly).
pub struct Step<'a> {
    pub pos: &'a [Option<Pt>],
    pub warps: &'a [Warp],
    pub scores: &'a [f32],
}

/// Track points through the frames `from` → `to` (either direction) on
/// `image(frame)`. `start` holds each track's position at `from` (`None`
/// skips it). `found(frame, step)` is called after each frame. Stops early
/// when every track is lost.
pub fn track(
    mut image: impl FnMut(i64) -> Result<Image>,
    start: &[Option<Pt>],
    from: i64,
    to: i64,
    settings: &Settings,
    progress: &Progress,
    mut found: impl FnMut(i64, Step),
) -> Result<Outcome> {
    let step = if to >= from { 1 } else { -1 };
    progress.total.store((to - from).abs(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut out = Outcome::default();

    let first = Luma::new(&image(from)?);
    // The features followed: each track's point plus its offset, which
    // turns and scales with the feature; `found` gets the points.
    let mut pos: Vec<Option<Pt>> = start.iter().enumerate().map(|(i, p)| p.map(|p| add(p, settings.offset(i)))).collect();
    let point = |i: usize, c: Pt, w: &Warp| {
        let o = mul(&w.matrix(), settings.offset(i));
        [c[0] - o[0], c[1] - o[1]]
    };
    let mut vel: Vec<Pt> = vec![[0.0; 2]; pos.len()];
    // Each feature's turn and growth since the start, and its last step.
    let mut warps = vec![Warp::default(); pos.len()];
    let mut turn = vec![Warp::default(); pos.len()];
    let mut scores = vec![1.0; pos.len()];
    let mut pats: Vec<Option<Pattern>> =
        pos.iter().enumerate().map(|(i, p)| p.and_then(|p| Pattern::new(&first, p, settings.radii(i).0, &IDENTITY))).collect();
    for (i, p) in pos.iter_mut().enumerate() {
        if p.is_some() && pats[i].is_none() {
            // Nothing to follow: lost before it starts.
            *p = None;
            out.lost.push((i, from));
        }
    }

    let mut frame = from;
    while frame != to && pos.iter().any(Option::is_some) {
        if progress.cancel.load(Ordering::Relaxed) {
            bail!("tracking cancelled at frame {frame}");
        }
        frame += step;
        let img = Luma::new(&image(frame)?);
        for i in 0..pos.len() {
            let (Some(p), Some(pat)) = (pos[i], &pats[i]) else { continue };
            let (r, s) = settings.radii(i);
            // Predict from the last step's motion, then search around it.
            let guess = [p[0] + vel[i][0], p[1] + vel[i][1]];
            let before = warps[i];
            let mut w = Warp { angle: before.angle + turn[i].angle, scale: before.scale * turn[i].scale };
            let (q, score) = find(pat, &img, guess, s, &mut w, settings.motion);
            if score < settings.min_score || !img.contains(q) {
                pos[i] = None;
                out.lost.push((i, frame));
                continue;
            }
            vel[i] = [q[0] - p[0], q[1] - p[1]];
            turn[i] = Warp { angle: w.angle - before.angle, scale: w.scale / before.scale };
            warps[i] = w;
            scores[i] = score;
            pos[i] = Some(q);
            if settings.update {
                pats[i] = Pattern::new(&img, q, r, &w.matrix());
            }
        }
        let points: Vec<Option<Pt>> = pos.iter().enumerate().map(|(i, c)| c.map(|c| point(i, c, &warps[i]))).collect();
        found(frame, Step { pos: &points, warps: &warps, scores: &scores });
        out.frames += 1;
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(out)
}

/// The image a Tracker node `node` tracks: its input, rendered by `graph`.
pub fn input_frames(graph: &Graph, node: &str) -> Result<impl FnMut(i64) -> Result<Image> + use<>> {
    let n = graph.node(node).with_context(|| format!("no node named '{node}'"))?;
    let Some(src) = n.inputs.first().filter(|s| !s.is_empty()).cloned() else {
        bail!("{node}: connect an input to track");
    };
    let graph = graph.clone();
    let mut eval = Evaluator::new();
    Ok(move |frame| {
        let ctx = EvalContext { frame, write_outputs: false };
        Ok(eval.evaluate(&graph, &src, &ctx)?.as_ref().clone())
    })
}

/// Each enabled track's turn and growth at `frame` (`track<i>_warp`, keyed
/// by tracking runs that follow rotation), or `None` where it was never
/// keyed. The warp param isn't in the op's spec, so it has no default.
pub fn warps(params: &Value, frame: i64) -> Vec<Option<Warp>> {
    (1..=TRACKS)
        .map(|i| {
            let on = params.get(format!("track{i}_on")).and_then(Value::as_bool).unwrap_or(i == 1);
            let v = crate::anim::sample(params.get(format!("track{i}_warp"))?, frame as f64);
            (on && v.len() >= 2).then(|| Warp { angle: (v[0] as f32).to_radians(), scale: v[1] as f32 })
        })
        .collect()
}

/// Each track's `[pattern, search]` box sizes: its own `track<i>_pattern` /
/// `track<i>_search` (set by sizing one track's box in the viewer), else
/// the shared `pattern` / `search`.
pub fn box_sizes(params: &Value) -> Vec<[f32; 2]> {
    let p = ops::params_with_defaults("tracker", params);
    let size = |name: String, shared: &str| p.get(&name).and_then(Value::as_f64).or(p[shared].as_f64()).unwrap_or(31.0) as f32;
    (1..=TRACKS).map(|i| [size(format!("track{i}_pattern"), "pattern"), size(format!("track{i}_search"), "search")]).collect()
}

/// Each track's offset from its point to the feature its pattern follows
/// (`track<i>_offset`, set by Ctrl-dragging a box in the viewer): for a
/// point that gets covered, or that has no texture of its own.
pub fn offsets(params: &Value) -> Vec<Pt> {
    (1..=TRACKS)
        .map(|i| {
            let v = crate::anim::sample(params.get(format!("track{i}_offset")).unwrap_or(&Value::Null), 0.0);
            if v.len() >= 2 { [v[0] as f32, v[1] as f32] } else { [0.0, 0.0] }
        })
        .collect()
}

/// Below this a match counts as weak: halfway from `min_score` (where a
/// track is lost) to a perfect 1.
pub fn weak_below(min_score: f32) -> f32 {
    (min_score + 1.0) / 2.0
}

/// The match scores keyed on enabled tracks (`track<i>_score`, keyed by
/// tracking runs): track index, frame and score.
fn scores(params: &Value) -> Vec<(usize, i64, f32)> {
    let p = ops::params_with_defaults("tracker", params);
    (0..TRACKS)
        .filter(|i| p[format!("track{}_on", i + 1)].as_bool() == Some(true))
        .flat_map(|i| crate::anim::keys(&p[format!("track{}_score", i + 1)]).into_iter().map(move |k| (i, k.frame, k.value[0] as f32)))
        .collect()
}

/// The worst match on an enabled track: track index, frame and score.
pub fn weakest(params: &Value) -> Option<(usize, i64, f32)> {
    scores(params).into_iter().min_by(|a, b| a.2.total_cmp(&b.2))
}

/// Frames where some enabled track matched weakly (see [`weak_below`]),
/// in order.
pub fn weak_frames(params: &Value) -> Vec<i64> {
    let min = ops::params_with_defaults("tracker", params)["min_score"].as_f64().unwrap_or(0.7) as f32;
    let mut frames: Vec<i64> = scores(params).into_iter().filter(|s| s.2 < weak_below(min)).map(|s| s.1).collect();
    frames.sort_unstable();
    frames.dedup();
    frames
}

/// Each enabled track's position at `frame`, from a Tracker node's params.
pub fn positions(params: &Value, frame: i64) -> Vec<Option<Pt>> {
    let p = ops::params_with_defaults("tracker", params);
    (1..=TRACKS)
        .map(|i| {
            let on = p[format!("track{i}_on")].as_bool().unwrap_or(false);
            let v = crate::anim::sample(&p[format!("track{i}")], frame as f64);
            (on && v.len() >= 2).then(|| [v[0] as f32, v[1] as f32])
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Solving a transform from tracks
// ---------------------------------------------------------------------------

/// What a transform solved from tracks may contain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Solve {
    Translate,
    Rotate,
    Scale,
}

impl Solve {
    /// From a Tracker's `motion` or `solve` choice.
    pub fn from_choice(choice: Option<&str>, default: Solve) -> Solve {
        match choice {
            Some("translate") => Solve::Translate,
            Some("translate, rotate") => Solve::Rotate,
            Some("translate, rotate, scale") => Solve::Scale,
            _ => default,
        }
    }
}

/// An affine map `x' = [a b; d e] x + [c f]`, stored `[a, b, c, d, e, f]`.
pub type Affine = [f32; 6];

pub fn apply(m: &Affine, p: Pt) -> Pt {
    [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
}

pub fn invert(m: &Affine) -> Affine {
    let det = m[0] * m[4] - m[1] * m[3];
    let det = if det.abs() < 1e-12 { 1e-12 } else { det };
    let (a, b, d, e) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    [a, b, -(a * m[2] + b * m[5]), d, e, -(d * m[2] + e * m[5])]
}

/// The least-squares similarity (translate, then optionally rotate and
/// uniform scale about the points' centroid) taking `from[i]` to `to[i]`.
/// One pair gives a translation only.
pub fn solve(from: &[Pt], to: &[Pt], kind: Solve) -> Affine {
    let n = from.len().min(to.len());
    if n == 0 {
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    }
    let mean = |v: &[Pt]| {
        let s = v[..n].iter().fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
        [s[0] / n as f32, s[1] / n as f32]
    };
    let (ca, cb) = (mean(from), mean(to));
    let (mut dot, mut cross, mut norm) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        let a = [from[i][0] - ca[0], from[i][1] - ca[1]];
        let b = [to[i][0] - cb[0], to[i][1] - cb[1]];
        dot += a[0] * b[0] + a[1] * b[1];
        cross += a[0] * b[1] - a[1] * b[0];
        norm += a[0] * a[0] + a[1] * a[1];
    }
    let (angle, scale) = if n < 2 || norm < 1e-6 || kind == Solve::Translate {
        (0.0, 1.0)
    } else {
        let scale = if kind == Solve::Scale { (dot * dot + cross * cross).sqrt() / norm } else { 1.0 };
        (cross.atan2(dot), scale)
    };
    // x' = s R (x − ca) + cb; positive angles turn clockwise on screen.
    let (sn, cs) = angle.sin_cos();
    let (a, b, d, e) = (scale * cs, -scale * sn, scale * sn, scale * cs);
    [a, b, cb[0] - (a * ca[0] + b * ca[1]), d, e, cb[1] - (d * ca[0] + e * ca[1])]
}

/// A perspective map `x' = (H x) / w`, row-major 3×3.
pub type Homography = [f64; 9];

/// The homography taking each `from[i]` to `to[i]`, or `None` when three
/// of either set's points are (nearly) in a line. The points may be in any
/// order, as long as both sets use the same one.
pub fn homography(from: &[Pt; 4], to: &[Pt; 4]) -> Option<Homography> {
    // Eight equations in h0..h7 (h8 = 1), solved by Gaussian elimination
    // with partial pivoting.
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ([x, y], [u, v]) = (from[i].map(f64::from), to[i].map(f64::from));
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    let scale = a.iter().flat_map(|r| r[..8].iter()).fold(0.0f64, |m, x| m.max(x.abs())).max(1.0);
    for c in 0..8 {
        let pivot = (c..8).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[pivot][c].abs() < 1e-9 * scale {
            return None;
        }
        a.swap(c, pivot);
        let row = a[c];
        for (_, other) in a.iter_mut().enumerate().filter(|&(r, _)| r != c) {
            let f = other[c] / row[c];
            other[c..].iter_mut().zip(&row[c..]).for_each(|(x, y)| *x -= f * y);
        }
    }
    let mut h = [1.0; 9];
    for (i, row) in a.iter().enumerate() {
        h[i] = row[8] / row[i];
    }
    // Scale so that w > 0 around the `from` points: the side of the
    // horizon they are on.
    let c = from.iter().fold([0.0, 0.0], |c, p| [c[0] + p[0] as f64 / 4.0, c[1] + p[1] as f64 / 4.0]);
    if h[6] * c[0] + h[7] * c[1] + h[8] < 0.0 {
        h.iter_mut().for_each(|x| *x = -*x);
    }
    Some(h)
}

/// `h` applied to `p`, or `None` for points that map to or past infinity.
pub fn project(h: &Homography, p: Pt) -> Option<Pt> {
    let (x, y) = (p[0] as f64, p[1] as f64);
    let w = h[6] * x + h[7] * y + h[8];
    (w > 1e-9).then(|| [((h[0] * x + h[1] * y + h[2]) / w) as f32, ((h[3] * x + h[4] * y + h[5]) / w) as f32])
}

/// [`solve`] over the tracks that have a position in both `from` and `to`;
/// `None` when there are none. A single track also turns and scales by
/// its own warp from `from_warps` to `to_warps`, where both are known.
pub fn solve_tracks(from: &[Option<Pt>], to: &[Option<Pt>], from_warps: &[Option<Warp>], to_warps: &[Option<Warp>], kind: Solve) -> Option<Affine> {
    let pairs: Vec<usize> = (0..from.len().min(to.len())).filter(|&i| from[i].is_some() && to[i].is_some()).collect();
    if let [i] = pairs[..] {
        let warp = |w: &[Option<Warp>]| w.get(i).copied().flatten();
        if let (Some(a), Some(b), true) = (warp(from_warps), warp(to_warps), kind != Solve::Translate) {
            let (p, q) = (from[i]?, to[i]?);
            let scale = if kind == Solve::Scale { b.scale / a.scale.max(1e-6) } else { 1.0 };
            // x' = s R (x − p) + q
            let [m0, m1, m3, m4] = Warp { angle: b.angle - a.angle, scale }.matrix();
            return Some([m0, m1, q[0] - (m0 * p[0] + m1 * p[1]), m3, m4, q[1] - (m3 * p[0] + m4 * p[1])]);
        }
    }
    let (a, b): (Vec<Pt>, Vec<Pt>) = pairs.iter().map(|&i| (from[i].unwrap(), to[i].unwrap())).unzip();
    (!a.is_empty()).then(|| solve(&a, &b, kind))
}

/// A Tracker's solved motion from its reference frame to `frame`, from its
/// raw (possibly keyed) params; `None` when no track is on.
pub fn solved(params: &Value, frame: i64) -> Option<Affine> {
    let p = ops::params_with_defaults("tracker", params);
    let kind = Solve::from_choice(p["solve"].as_str(), Solve::Scale);
    let reference = p["reference_frame"].as_i64().unwrap_or(1);
    solve_tracks(&positions(&p, reference), &positions(&p, frame), &warps(&p, reference), &warps(&p, frame), kind)
}

/// Where a Tracker's output shows each point of its input at `frame`:
/// the solved motion for a match-move, its inverse to stabilize, `None`
/// when the output is the input.
pub fn output_map(params: &Value, frame: i64) -> Option<Affine> {
    match params.get("output").and_then(Value::as_str) {
        Some("match-move") => solved(params, frame),
        Some("stabilize") => solved(params, frame).map(|m| invert(&m)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Exporting the solved motion
// ---------------------------------------------------------------------------

/// Params for a Transform node that reproduces a Tracker's solved motion on
/// an image of `size`: the match-move (or, with `stabilize`, its inverse),
/// keyed on every frame where an enabled track has a key (position or
/// warp). Rotation and scale are keyed only when the Tracker's `solve`
/// includes them.
pub fn transform_params(params: &Value, size: [usize; 2], stabilize: bool) -> Result<Map<String, Value>> {
    let p = ops::params_with_defaults("tracker", params);
    let kind = Solve::from_choice(p["solve"].as_str(), Solve::Scale);
    let mut frames: Vec<i64> = (1..=TRACKS)
        .filter(|i| p[format!("track{i}_on")].as_bool() == Some(true))
        .flat_map(|i| [format!("track{i}"), format!("track{i}_warp")])
        .flat_map(|name| crate::anim::key_frames(&p[name]))
        .collect();
    frames.sort_unstable();
    frames.dedup();
    if frames.is_empty() {
        bail!("no tracked frames: track first");
    }
    // Turn and scale about the image center, which in track coordinates
    // (pixel i spanning i..i+1) is half a pixel past the middle.
    let c = [size[0] as f32 / 2.0 + 0.5, size[1] as f32 / 2.0 + 0.5];
    let round = |x: f64| (x * 1e4).round() / 1e4;
    let (mut translate, mut rotate, mut scale) = (Vec::new(), Vec::new(), Vec::new());
    let mut last_angle = 0.0;
    for f in frames {
        let m = solved(&p, f).context("no tracks enabled")?;
        let m = if stabilize { invert(&m) } else { m };
        let o = apply(&m, c);
        translate.push(json!([f, [round((o[0] - c[0]) as f64), round((o[1] - c[1]) as f64)]]));
        // Unwrap, so the curve doesn't jump a full turn at ±180°.
        let mut angle = (m[3] as f64).atan2(m[0] as f64).to_degrees();
        angle -= ((angle - last_angle) / 360.0).round() * 360.0;
        last_angle = angle;
        rotate.push(json!([f, round(angle)]));
        let s = round((m[0] as f64).hypot(m[3] as f64));
        scale.push(json!([f, [s, s]]));
    }
    let curve = |keys: Vec<Value>| json!({ "keys": keys, "interp": "linear" });
    let mut out = Map::new();
    out.insert("translate".into(), curve(translate));
    // The pivot used above, whatever the input's data window.
    out.insert("center".into(), json!(c));
    if kind != Solve::Translate {
        out.insert("rotate".into(), curve(rotate));
    }
    if kind == Solve::Scale {
        out.insert("scale".into(), curve(scale));
    }
    Ok(out)
}

/// Params for a Corner Pin that reproduces four tracks' motion in
/// perspective: From is each track at the reference frame and To its
/// curve (match-move), or the other way round (stabilize).
pub fn corner_pin_params(params: &Value, stabilize: bool) -> Result<Map<String, Value>> {
    let p = ops::params_with_defaults("tracker", params);
    if (1..=TRACKS).any(|i| p[format!("track{i}_on")].as_bool() != Some(true)) {
        bail!("a corner pin needs all four tracks on");
    }
    let refs = positions(&p, p["reference_frame"].as_i64().unwrap_or(1));
    let mut out = Map::new();
    for (i, r) in refs.into_iter().enumerate() {
        let (still, moving) = (json!(r), p[format!("track{}", i + 1)].clone());
        let (from, to) = if stabilize { (moving, still) } else { (still, moving) };
        out.insert(format!("from{}", i + 1), from);
        out.insert(format!("to{}", i + 1), to);
    }
    Ok(out)
}

/// Params for a node of type `op` (`transform` or `corner_pin`) that
/// carries Tracker node `node`'s motion. A Transform is sized by the
/// tracker's input at the reference frame.
pub fn export(graph: &Graph, node: &str, op: &str, stabilize: bool) -> Result<Map<String, Value>> {
    let n = graph.node(node).with_context(|| format!("no node named '{node}'"))?;
    if n.op != "tracker" {
        bail!("'{node}' is not a Tracker");
    }
    match op {
        "transform" => {
            let reference = ops::params_with_defaults("tracker", &n.params)["reference_frame"].as_i64().unwrap_or(1);
            let img = input_frames(graph, node)?(reference)?;
            transform_params(&n.params, [img.format.width, img.format.height], stabilize)
        }
        "corner_pin" => corner_pin_params(&n.params, stabilize),
        other => bail!("can't export a tracker to '{other}' (transform or corner_pin)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth, textured test image: a few gaussian blobs, offset by `d`.
    fn blobs(w: usize, h: usize, d: Pt) -> Image {
        blobs_moved(w, h, &[1.0, 0.0, d[0], 0.0, 1.0, d[1]])
    }

    /// The blobs moved by the affine map `m` (image → screen).
    fn blobs_moved(w: usize, h: usize, m: &Affine) -> Image {
        let back = invert(m);
        let mut img = Image::new(w, h);
        let spots = [(30.0, 40.0, 6.0), (70.0, 30.0, 4.0), (50.0, 70.0, 8.0), (90.0, 80.0, 5.0), (40.0, 55.0, 3.0)];
        for y in 0..h {
            for x in 0..w {
                let [px, py] = apply(&back, [x as f32 + 0.5, y as f32 + 0.5]);
                let v: f32 = spots.iter().map(|&(cx, cy, r)| (-((px - cx).powi(2) + (py - cy).powi(2)) / (2.0 * r * r)).exp()).sum();
                img.set(x, y, [v, v, v, 1.0]);
            }
        }
        img
    }

    #[test]
    fn finds_a_sub_pixel_shift() {
        let a = Luma::new(&blobs(128, 128, [0.0, 0.0]));
        let b = Luma::new(&blobs(128, 128, [3.3, -2.6]));
        let pat = Pattern::new(&a, [50.0, 70.0], 10, &IDENTITY).unwrap();
        let (p, score) = find(&pat, &b, [50.0, 70.0], 8, &mut Warp::default(), Solve::Translate);
        assert!(score > 0.99, "{score}");
        assert!((p[0] - 53.3).abs() < 0.02 && (p[1] - 67.4).abs() < 0.02, "{p:?}");
    }

    #[test]
    fn flat_patterns_are_refused() {
        let flat = Luma::new(&Image::new(32, 32));
        assert!(Pattern::new(&flat, [16.0, 16.0], 5, &IDENTITY).is_none());
    }

    #[test]
    fn flat_areas_never_outscore_the_true_match() {
        // An edge to track, next to a flat area with a little rounding noise.
        let mut img = Image::new(64, 32);
        for y in 0..32 {
            for x in 0..64 {
                let v = if x < 20 { 0.2 } else { 0.6 + 1e-7 * ((x * 7 + y * 13) % 5) as f32 };
                img.set(x, y, [v, v, v, 1.0]);
            }
        }
        let l = Luma::new(&img);
        let pat = Pattern::new(&l, [20.0, 16.0], 6, &IDENTITY).unwrap();
        let (p, score) = find(&pat, &l, [26.0, 16.0], 10, &mut Warp::default(), Solve::Translate);
        assert!((p[0] - 20.0).abs() < 0.05 && score <= 1.0, "{p:?} {score}");
    }

    #[test]
    fn tracks_motion_over_frames_and_reports_lost_tracks() {
        // Moves 2.5 px right and 1 px down per frame.
        let frame = |f: i64| Ok(blobs(128, 128, [2.5 * (f - 1) as f32, (f - 1) as f32]));
        let settings = Settings { pattern: 21, search: 41, sizes: vec![], offsets: vec![], min_score: 0.8, update: false, motion: Solve::Rotate };
        let mut seen = Vec::new();
        // The second track sits on a flat corner and can't be followed.
        let start = [Some([30.0, 40.0]), Some([5.0, 120.0]), None];
        let out = track(frame, &start, 1, 9, &settings, &Progress::default(), |f, s| seen.push((f, s.pos.to_vec()))).unwrap();
        assert_eq!(out.frames, 8);
        assert_eq!(out.lost, vec![(1, 1)]);
        let (f, last) = seen.last().unwrap();
        let p = last[0].unwrap();
        assert_eq!(*f, 9);
        assert!((p[0] - 50.0).abs() < 0.02 && (p[1] - 48.0).abs() < 0.02, "{p:?}");
        assert!(last[1].is_none() && last[2].is_none());
    }

    #[test]
    fn tracks_backward() {
        let frame = |f: i64| Ok(blobs(128, 128, [2.0 * f as f32, 0.0]));
        let settings = Settings { pattern: 21, search: 41, sizes: vec![], offsets: vec![], min_score: 0.8, update: true, motion: Solve::Rotate };
        let mut last = None;
        track(frame, &[Some([70.0, 30.0])], 5, 2, &settings, &Progress::default(), |_, s| last = s.pos[0]).unwrap();
        let p = last.unwrap();
        assert!((p[0] - 64.0).abs() < 0.02 && (p[1] - 30.0).abs() < 0.02, "{p:?}");
    }

    /// The blobs turning `deg` per frame (and growing by `grow`) about
    /// (60, 55) while drifting right; returns the error at each frame
    /// tracking the point (50, 40), or `None` once it is lost, the track's
    /// warp at the last frame and its lowest match score.
    fn track_turning(deg: f32, grow: f32, motion: Solve, update: bool) -> (Vec<Option<f32>>, Warp, f32) {
        track_turning_from([50.0, 40.0], [0.0, 0.0], deg, grow, motion, update)
    }

    /// [`track_turning`] for the point `p`, its pattern placed `offset` away.
    fn track_turning_from(p: Pt, offset: Pt, deg: f32, grow: f32, motion: Solve, update: bool) -> (Vec<Option<f32>>, Warp, f32) {
        let at = |f: i64| {
            let t = (f - 1) as f32;
            let (s, c) = (deg * t).to_radians().sin_cos();
            let k = grow.powf(t);
            // Turn and scale about (60, 55), then move 1 px right per frame.
            let (a, b, d, e) = (k * c, -k * s, k * s, k * c);
            [a, b, 60.0 + t - (a * 60.0 + b * 55.0), d, e, 55.0 - (d * 60.0 + e * 55.0)]
        };
        let frame = |f: i64| Ok(blobs_moved(128, 128, &at(f)));
        let settings = Settings { pattern: 25, search: 45, sizes: vec![], offsets: vec![offset], min_score: 0.8, update, motion };
        let (mut errors, mut warp, mut worst) = (Vec::new(), Warp::default(), 1.0f32);
        track(frame, &[Some(p)], 1, 21, &settings, &Progress::default(), |f, s| {
            let truth = apply(&at(f), p);
            errors.push(s.pos[0].map(|p| ((p[0] - truth[0]).powi(2) + (p[1] - truth[1]).powi(2)).sqrt()));
            worst = worst.min(s.scores[0]);
            warp = s.warps[0];
        })
        .unwrap();
        (errors, warp, worst)
    }

    #[test]
    fn rotating_features_are_followed_without_drift() {
        // 2° a frame: 40° by the end.
        for update in [false, true] {
            let (errors, warp, score) = track_turning(2.0, 1.0, Solve::Rotate, update);
            assert!(score > 0.95, "update {update}: lowest score {score}");
            assert_eq!(errors.len(), 20);
            let worst = errors.iter().map(|e| e.expect("never lost")).fold(0.0, f32::max);
            assert!(worst < 0.1, "update {update}: worst error {worst} px");
            assert!((warp.to_param()[0] - 40.0).abs() < 0.2 && warp.scale == 1.0, "update {update}: {warp:?}");
        }
        // Matching shifts only can't follow it.
        let (errors, ..) = track_turning(2.0, 1.0, Solve::Translate, false);
        assert!(errors.iter().any(|e| e.is_none_or(|e| e > 1.0)), "{errors:?}");
    }

    #[test]
    fn an_offset_point_turns_with_the_feature() {
        // The point (40, 46) follows the pattern at (50, 40) as it turns 40°.
        let (errors, ..) = track_turning_from([40.0, 46.0], [10.0, -6.0], 2.0, 1.0, Solve::Rotate, false);
        let worst = errors.iter().map(|e| e.expect("never lost")).fold(0.0, f32::max);
        assert!(worst < 0.15, "worst error {worst} px");
        let s = Settings::from_params(&json!({ "track2_offset": [3, 4] }));
        assert_eq!(s.offsets[..2], [[0.0, 0.0], [3.0, 4.0]]);
    }

    #[test]
    fn scaling_features_are_followed() {
        // Turning 1° and growing 2% a frame: 49% bigger by the end.
        let (errors, warp, _) = track_turning(1.0, 1.02, Solve::Scale, false);
        let worst = errors.iter().map(|e| e.expect("never lost")).fold(0.0, f32::max);
        assert!(worst < 0.15, "worst error {worst} px");
        let [deg, scale] = warp.to_param();
        assert!((deg - 20.0).abs() < 0.2 && (scale - 1.02f64.powi(20)).abs() < 0.005, "{warp:?}");
    }

    #[test]
    fn similarity_solve_recovers_rotation_and_scale() {
        let from = [[10.0, 10.0], [50.0, 10.0], [30.0, 40.0]];
        let truth = solve(&[[0.0, 0.0], [1.0, 0.0]], &[[5.0, 7.0], [5.0, 9.0]], Solve::Scale);
        // 90° clockwise, scale 2, then moved.
        assert!((apply(&truth, [1.0, 0.0])[1] - 9.0).abs() < 1e-4);
        let to: Vec<Pt> = from.iter().map(|&p| apply(&truth, p)).collect();
        let m = solve(&from, &to, Solve::Scale);
        for (a, b) in m.iter().zip(&truth) {
            assert!((a - b).abs() < 1e-3, "{m:?} vs {truth:?}");
        }
        let back = apply(&invert(&m), to[2]);
        assert!((back[0] - 30.0).abs() < 1e-3 && (back[1] - 40.0).abs() < 1e-3);
        // Rotation only keeps the scale at 1.
        let r = solve(&from, &to, Solve::Rotate);
        assert!(((r[0] * r[0] + r[3] * r[3]).sqrt() - 1.0).abs() < 1e-4);
        // Translation only: the mean offset.
        let t = solve(&[[0.0, 0.0], [2.0, 0.0]], &[[1.0, 1.0], [5.0, 1.0]], Solve::Translate);
        assert_eq!(t, [1.0, 0.0, 2.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn homography_maps_the_corners_and_keeps_perspective() {
        let square = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];
        // A keystone: the top edge pinched in.
        let trap = [[30.0, 10.0], [70.0, 10.0], [100.0, 90.0], [0.0, 90.0]];
        let h = homography(&square, &trap).unwrap();
        for (a, b) in square.iter().zip(&trap) {
            let q = project(&h, *a).unwrap();
            assert!((q[0] - b[0]).abs() < 1e-3 && (q[1] - b[1]).abs() < 1e-3, "{q:?} vs {b:?}");
        }
        // The square's center goes where the trapezoid's diagonals cross
        // (x = 50; y from the line (30, 10)–(100, 90) meeting x = 50), not
        // to the corners' average (y = 50).
        let c = project(&h, [50.0, 50.0]).unwrap();
        let y = 10.0 + 80.0 * 20.0 / 70.0;
        assert!((c[0] - 50.0).abs() < 1e-3 && (c[1] - y).abs() < 1e-3, "{c:?}");
        // Same correspondences in another order give the same map.
        let order = [0, 2, 1, 3];
        let h2 = homography(&order.map(|i| square[i]), &order.map(|i| trap[i])).unwrap();
        assert!((project(&h2, [50.0, 50.0]).unwrap()[1] - y).abs() < 1e-3);
        // Three in a line: no map.
        assert!(homography(&[[0.0, 0.0], [50.0, 0.0], [100.0, 0.0], [0.0, 100.0]], &trap).is_none());
    }

    #[test]
    fn tracks_can_have_their_own_box_sizes() {
        let s = Settings::from_params(&json!({ "pattern": 21, "search": 41, "track2_pattern": 11, "track3_search": 81 }));
        assert_eq!(s.radii(0), (10, 10));
        assert_eq!(s.radii(1), (5, 15));
        assert_eq!(s.radii(2), (10, 30));
    }

    #[test]
    fn weakest_match_skips_tracks_that_are_off() {
        let params = json!({
            "track1_score": { "keys": [[2, 0.95], [3, 0.81]] },
            "track2_score": { "keys": [[2, 0.72]] },
        });
        assert_eq!(weakest(&params), Some((0, 3, 0.81)));
        assert_eq!(weakest(&json!({ "track2_on": true, "track2_score": params["track2_score"] })), Some((1, 2, 0.72)));
        assert_eq!(weakest(&json!({})), None);
        assert_eq!(weak_frames(&params), [3]);
        assert!(0.81 < weak_below(0.7) && weak_below(0.7) < 0.9);
    }

    #[test]
    fn one_track_solves_rotation_and_scale_from_its_warp() {
        let w = |deg: f32, scale: f32| Some(Warp { angle: deg.to_radians(), scale });
        let (from, to) = ([Some([10.0, 10.0]), None], [Some([20.0, 15.0]), None]);
        // 30° → 120°, 1.0 → 2.0: turns 90° and doubles about the track.
        let m = solve_tracks(&from, &to, &[w(30.0, 1.0)], &[w(120.0, 2.0)], Solve::Scale).unwrap();
        let p = apply(&m, [11.0, 10.0]);
        assert!((p[0] - 20.0).abs() < 1e-4 && (p[1] - 17.0).abs() < 1e-4, "{p:?}");
        let r = solve_tracks(&from, &to, &[w(30.0, 1.0)], &[w(120.0, 2.0)], Solve::Rotate).unwrap();
        assert!((apply(&r, [11.0, 10.0])[1] - 16.0).abs() < 1e-4);
        // Without warps (or solving translation only) it just moves.
        for (ws, kind) in [(&[None][..], Solve::Scale), (&[w(120.0, 2.0)][..], Solve::Translate)] {
            let t = solve_tracks(&from, &to, &[w(30.0, 1.0)], ws, kind).unwrap();
            assert_eq!(apply(&t, [11.0, 10.0]), [21.0, 15.0]);
        }
    }
}
