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
use serde_json::Value;
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

    /// The `(2r+1)²` samples of a square window centered on `c`, row-major.
    pub fn window(&self, c: Pt, r: usize) -> Vec<f32> {
        let n = 2 * r + 1;
        (0..n * n)
            .map(|i| self.at(c[0] + (i % n) as f32 - r as f32, c[1] + (i / n) as f32 - r as f32))
            .collect()
    }

    pub fn contains(&self, p: Pt) -> bool {
        p[0] >= 0.0 && p[1] >= 0.0 && p[0] < self.width as f32 && p[1] < self.height as f32
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
    /// The `(2r+1)²` pattern centered on `c`, or `None` when it is flat
    /// (nothing to lock onto).
    pub fn new(img: &Luma, c: Pt, r: usize) -> Option<Pattern> {
        let mut v = img.window(c, r);
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

/// Search for `pat` within `s` pixels of `guess` in `img`. Returns the best
/// match position (sub-pixel) and its correlation, −1 to 1.
pub fn find(pat: &Pattern, img: &Luma, guess: Pt, s: usize) -> (Pt, f32) {
    let (mut p, mut score) = search(pat, img, guess, s);
    // A parabola through whole-pixel neighbours misplaces an asymmetric
    // peak; fitting again through ever closer samples shrinks that error.
    for h in [0.5, 0.25, 0.125] {
        let at = |dx: f32, dy: f32| ncc(pat, img, [p[0] + dx, p[1] + dy]);
        let c = at(0.0, 0.0);
        let fx = parabola(at(-h, 0.0), c, at(h, 0.0));
        let fy = parabola(at(0.0, -h), c, at(0.0, h));
        let q = [p[0] + fx * h, p[1] + fy * h];
        let sc = ncc(pat, img, q);
        if sc >= c {
            (p, score) = (q, sc);
        } else {
            score = score.max(c);
        }
    }
    (p, score)
}

/// Vertex offset (in sample steps, within ±½) of the parabola through
/// three equally spaced samples; 0 when they don't form a peak.
fn parabola(l: f32, c: f32, r: f32) -> f32 {
    let d = l - 2.0 * c + r;
    if d < 0.0 { (0.5 * (l - r) / d).clamp(-0.5, 0.5) } else { 0.0 }
}

/// Correlation of `pat` with the window centered on `c`.
fn ncc(pat: &Pattern, img: &Luma, c: Pt) -> f32 {
    let w = img.window(c, pat.r);
    let mean = w.iter().sum::<f32>() / w.len() as f32;
    let (mut dot, mut var) = (0.0, 0.0);
    for (a, b) in w.iter().zip(&pat.v) {
        dot += (a - mean) * b;
        var += (a - mean) * (a - mean);
    }
    if var < FLAT * FLAT * w.len() as f32 { 0.0 } else { (dot / var.sqrt()).clamp(-1.0, 1.0) }
}

/// One correlation search on the integer grid around `guess`, with a
/// parabola fit through the peak.
fn search(pat: &Pattern, img: &Luma, guess: Pt, s: usize) -> (Pt, f32) {
    let r = pat.r;
    let pn = 2 * r + 1;
    let gn = 2 * (r + s) + 1;
    // The search area, sampled once on the guess's sub-pixel grid.
    let g = img.window(guess, r + s);
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
    let p = [guess[0] + bx as f32 - s as f32 + fx, guess[1] + by as f32 - s as f32 + fy];
    (p, at(bx, by))
}

/// A Tracker node's tracking settings.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Pattern and search box sizes, in pixels.
    pub pattern: usize,
    pub search: usize,
    /// Matches below this correlation lose the track.
    pub min_score: f32,
    /// Take a fresh pattern at every tracked frame, instead of keeping the
    /// one from the start frame. Follows gradual changes but drifts.
    pub update: bool,
}

impl Settings {
    pub fn from_params(p: &Value) -> Settings {
        let int = |k: &str, d: u64| p.get(k).and_then(Value::as_f64).map_or(d, |v| v.max(1.0) as u64) as usize;
        Settings {
            pattern: int("pattern", 31),
            search: int("search", 91),
            min_score: p.get("min_score").and_then(Value::as_f64).unwrap_or(0.7) as f32,
            update: p.get("update").and_then(Value::as_str) == Some("every frame"),
        }
    }

    fn radii(&self) -> (usize, usize) {
        let r = (self.pattern / 2).max(2);
        (r, (self.search / 2).saturating_sub(r).max(1))
    }
}

/// What a tracking run did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub frames: i64,
    /// Tracks that were lost, with the first frame they failed on.
    pub lost: Vec<(usize, i64)>,
}

/// Track points through the frames `from` → `to` (either direction) on
/// `image(frame)`. `start` holds each track's position at `from` (`None`
/// skips it). `found(frame, positions)` is called after each frame with
/// the tracks still alive. Stops early when every track is lost.
pub fn track(
    mut image: impl FnMut(i64) -> Result<Image>,
    start: &[Option<Pt>],
    from: i64,
    to: i64,
    settings: &Settings,
    progress: &Progress,
    mut found: impl FnMut(i64, &[Option<Pt>]),
) -> Result<Outcome> {
    let (r, s) = settings.radii();
    let step = if to >= from { 1 } else { -1 };
    progress.total.store((to - from).abs(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut out = Outcome::default();

    let first = Luma::new(&image(from)?);
    let mut pos: Vec<Option<Pt>> = start.to_vec();
    let mut vel: Vec<Pt> = vec![[0.0; 2]; pos.len()];
    let mut pats: Vec<Option<Pattern>> = pos.iter().map(|p| p.and_then(|p| Pattern::new(&first, p, r))).collect();
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
            // Predict from the last step's motion, then search around it.
            let guess = [p[0] + vel[i][0], p[1] + vel[i][1]];
            let (q, score) = find(pat, &img, guess, s);
            if score < settings.min_score || !img.contains(q) {
                pos[i] = None;
                out.lost.push((i, frame));
                continue;
            }
            vel[i] = [q[0] - p[0], q[1] - p[1]];
            pos[i] = Some(q);
            if settings.update {
                pats[i] = Pattern::new(&img, q, r);
            }
        }
        found(frame, &pos);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth, textured test image: a few gaussian blobs, offset by `d`.
    fn blobs(w: usize, h: usize, d: Pt) -> Image {
        let mut img = Image::new(w, h);
        let spots = [(30.0, 40.0, 6.0), (70.0, 30.0, 4.0), (50.0, 70.0, 8.0), (90.0, 80.0, 5.0), (40.0, 55.0, 3.0)];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5 - d[0], y as f32 + 0.5 - d[1]);
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
        let pat = Pattern::new(&a, [50.0, 70.0], 10).unwrap();
        let (p, score) = find(&pat, &b, [50.0, 70.0], 8);
        assert!(score > 0.99, "{score}");
        assert!((p[0] - 53.3).abs() < 0.02 && (p[1] - 67.4).abs() < 0.02, "{p:?}");
    }

    #[test]
    fn flat_patterns_are_refused() {
        let flat = Luma::new(&Image::new(32, 32));
        assert!(Pattern::new(&flat, [16.0, 16.0], 5).is_none());
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
        let pat = Pattern::new(&l, [20.0, 16.0], 6).unwrap();
        let (p, score) = find(&pat, &l, [26.0, 16.0], 10);
        assert!((p[0] - 20.0).abs() < 0.05 && score <= 1.0, "{p:?} {score}");
    }

    #[test]
    fn tracks_motion_over_frames_and_reports_lost_tracks() {
        // Moves 2.5 px right and 1 px down per frame.
        let frame = |f: i64| Ok(blobs(128, 128, [2.5 * (f - 1) as f32, (f - 1) as f32]));
        let settings = Settings { pattern: 21, search: 41, min_score: 0.8, update: false };
        let mut seen = Vec::new();
        // The second track sits on a flat corner and can't be followed.
        let start = [Some([30.0, 40.0]), Some([5.0, 120.0]), None];
        let out = track(frame, &start, 1, 9, &settings, &Progress::default(), |f, p| seen.push((f, p.to_vec()))).unwrap();
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
        let settings = Settings { pattern: 21, search: 41, min_score: 0.8, update: true };
        let mut last = None;
        track(frame, &[Some([70.0, 30.0])], 5, 2, &settings, &Progress::default(), |_, p| last = p[0]).unwrap();
        let p = last.unwrap();
        assert!((p[0] - 64.0).abs() < 0.02 && (p[1] - 30.0).abs() < 0.02, "{p:?}");
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
}
