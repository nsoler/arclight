//! Feature tracking for the camera tracker: find corners in a frame and
//! follow them through the next frames.
//!
//! - **Detection:** Shi–Tomasi "good features to track": the smaller
//!   eigenvalue of the image's structure tensor (gradient products summed
//!   over a small window) is large only where the image changes in two
//!   directions, i.e. at corners and blobs. Local maxima above a fraction
//!   of the strongest are taken, strongest first, at least a minimum
//!   distance apart.
//! - **Tracking:** pyramidal Lucas–Kanade: at each level of an image
//!   pyramid (coarse to fine), the shift that best lines a window around
//!   the feature up with the next frame, by Gauss–Newton steps on the
//!   brightness difference; the coarse levels catch large motion, the fine
//!   ones the sub-pixel detail.
//! - **Checking:** each feature is also tracked back from the new frame;
//!   if it doesn't come back to where it started (forward–backward error),
//!   it drifted or was hidden, and is dropped.
//! - **Refining:** a window that only shifts can't follow content that
//!   stretches, turns or shears as the camera moves (toward the edges of a
//!   pan, or all over in a push-in); the best shift then lands a little
//!   off, differently for each feature, and frame after frame the error
//!   adds up. So each new position is refined against the frame the
//!   feature was found on, with the window allowed an affine warp (Shi and
//!   Tomasi's affine feature tracking): the deformation is modelled
//!   instead of becoming error, and nothing accumulates.
//!
//! Positions are frame pixels, y down, pixel i spanning i..i+1 (as the 2D
//! tracker's and shapes'), so a pixel's center is at i + 0.5.

use crate::image::Image;
use rayon::prelude::*;

pub type Pt = [f32; 2];

/// A grayscale image (Rec. 709 luma of the premultiplied color).
#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl Gray {
    /// The frame of `img` (overscan dropped) as luma.
    pub fn new(img: &Image) -> Gray {
        let flat = img.flat();
        let data = flat.pixels.par_iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
        Gray { w: flat.width, h: flat.height, data }
    }

    /// The pixel at (x, y), edges extended.
    #[inline]
    fn get(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.data[y * self.w + x]
    }

    /// Bilinear, pixel centers on integers, edges extended.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = self.get(x0, y0) * (1.0 - fx) + self.get(x0 + 1, y0) * fx;
        let bottom = self.get(x0, y0 + 1) * (1.0 - fx) + self.get(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// Half the size: blurred with a 5-tap binomial filter, then every
    /// other pixel.
    fn half(&self) -> Gray {
        const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        // Horizontally on the source rows, then vertically at the kept ones.
        let mut rows = vec![0.0f32; w * self.h];
        rows.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = (0..5).map(|k| K[k] * self.get(2 * x as isize + k as isize - 2, y as isize)).sum();
            }
        });
        let src = Gray { w, h: self.h, data: rows };
        let mut data = vec![0.0f32; w * h];
        data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = (0..5).map(|k| K[k] * src.get(x as isize, 2 * y as isize + k as isize - 2)).sum();
            }
        });
        Gray { w, h, data }
    }
}

/// An image and its successive halvings, finest first.
pub struct Pyramid {
    pub levels: Vec<Gray>,
}

impl Pyramid {
    /// Up to `levels` levels, stopping before one gets smaller than 32
    /// pixels on a side.
    pub fn new(gray: Gray, levels: usize) -> Pyramid {
        let mut out = vec![gray];
        while out.len() < levels.max(1) {
            let last = out.last().unwrap();
            if last.w.min(last.h) < 64 {
                break;
            }
            let next = last.half();
            out.push(next);
        }
        Pyramid { levels: out }
    }
}

/// Settings for finding and following features.
#[derive(Clone, Debug)]
pub struct Settings {
    /// How many features to keep alive.
    pub max_features: usize,
    /// The least distance between features, pixels.
    pub min_distance: f32,
    /// Corners weaker than this fraction of the strongest are ignored.
    pub quality: f32,
    /// Lucas–Kanade window half-size: a (2r + 1)² window.
    pub radius: usize,
    pub levels: usize,
    /// The most a feature tracked forward and back may miss its start by.
    pub max_fb_error: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { max_features: 300, min_distance: 12.0, quality: 0.01, radius: 7, levels: 4, max_fb_error: 0.5 }
    }
}

/// Corners of `g`: up to `max` of them, strongest first, at least
/// `min_distance` apart and from every point of `avoid`, and `border`
/// pixels clear of the edges. Each with its strength (the structure
/// tensor's smaller eigenvalue).
pub fn detect(g: &Gray, max: usize, min_distance: f32, quality: f32, border: usize, avoid: &[Pt]) -> Vec<(Pt, f32)> {
    let (w, h) = (g.w, g.h);
    if w < 2 * border + 3 || h < 2 * border + 3 || max == 0 {
        return Vec::new();
    }
    // Sobel gradients, then their products summed over a 5×5 window.
    const R: isize = 2;
    let grads: Vec<(f32, f32)> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let p = |dx: isize, dy: isize| g.get(x + dx, y + dy);
            let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1) - p(-1, -1) - 2.0 * p(-1, 0) - p(-1, 1)) / 8.0;
            let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1) - p(-1, -1) - 2.0 * p(0, -1) - p(1, -1)) / 8.0;
            (gx, gy)
        })
        .collect();
    let score: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            if x < border as isize || y < border as isize || x >= (w - border) as isize || y >= (h - border) as isize {
                return 0.0;
            }
            let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
            for dy in -R..=R {
                for dx in -R..=R {
                    let (gx, gy) = grads[((y + dy).clamp(0, h as isize - 1) as usize) * w + (x + dx).clamp(0, w as isize - 1) as usize];
                    a += gx * gx;
                    b += gx * gy;
                    c += gy * gy;
                }
            }
            (a + c) / 2.0 - (((a - c) / 2.0).powi(2) + b * b).sqrt()
        })
        .collect();
    let best = score.iter().copied().fold(0.0, f32::max);
    if best <= 0.0 {
        return Vec::new();
    }
    let floor = best * quality;
    // Local maxima (3×3) above the floor, strongest first.
    let mut candidates: Vec<(usize, f32)> = (0..w * h)
        .into_par_iter()
        .filter_map(|i| {
            let s = score[i];
            if s < floor || s <= 0.0 {
                return None;
            }
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (x + dx, y + dy);
                    if (dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize {
                        let n = score[ny as usize * w + nx as usize];
                        // Ties go to the first in reading order.
                        if n > s || (n == s && (ny as usize * w + nx as usize) < i) {
                            return None;
                        }
                    }
                }
            }
            Some((i, s))
        })
        .collect();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // Greedy spacing, with a grid of min_distance cells to look nearby.
    let cell = min_distance.max(1.0);
    let (gw, gh) = ((w as f32 / cell).ceil() as usize + 1, (h as f32 / cell).ceil() as usize + 1);
    let mut grid: Vec<Vec<Pt>> = vec![Vec::new(); gw * gh];
    let cell_of = |p: Pt| ((p[0] / cell).max(0.0) as usize).min(gw - 1) + ((p[1] / cell).max(0.0) as usize).min(gh - 1) * gw;
    let far_enough = |grid: &Vec<Vec<Pt>>, p: Pt| {
        let (cx, cy) = (((p[0] / cell) as isize), ((p[1] / cell) as isize));
        for y in cy - 1..=cy + 1 {
            for x in cx - 1..=cx + 1 {
                if x < 0 || y < 0 || x >= gw as isize || y >= gh as isize {
                    continue;
                }
                if grid[y as usize * gw + x as usize].iter().any(|q| (q[0] - p[0]).hypot(q[1] - p[1]) < min_distance) {
                    return false;
                }
            }
        }
        true
    };
    for &p in avoid {
        grid[cell_of(p)].push(p);
    }
    let mut out = Vec::new();
    for (i, s) in candidates {
        let p = [(i % w) as f32 + 0.5, (i / w) as f32 + 0.5];
        if far_enough(&grid, p) {
            grid[cell_of(p)].push(p);
            out.push((p, s));
            if out.len() == max {
                break;
            }
        }
    }
    out
}

/// Lucas–Kanade at one level: the shift `d` (refining `guess`) that best
/// lines up `a`'s window around `p` (pixel centers on integers) with `b`'s
/// window around `p + d`. None where the window is flat or edge-like (no
/// unique answer) or the shift leaves the image.
fn lk_level(a: &Gray, b: &Gray, p: Pt, guess: Pt, r: usize) -> Option<Pt> {
    let r = r as isize;
    let n = ((2 * r + 1) * (2 * r + 1)) as usize;
    let (mut vals, mut gx, mut gy) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut gxx, mut gxy, mut gyy) = (0.0f64, 0.0f64, 0.0f64);
    for dy in -r..=r {
        for dx in -r..=r {
            let (x, y) = (p[0] + dx as f32, p[1] + dy as f32);
            let ix = (a.sample(x + 1.0, y) - a.sample(x - 1.0, y)) / 2.0;
            let iy = (a.sample(x, y + 1.0) - a.sample(x, y - 1.0)) / 2.0;
            vals.push(a.sample(x, y));
            gx.push(ix);
            gy.push(iy);
            gxx += (ix * ix) as f64;
            gxy += (ix * iy) as f64;
            gyy += (iy * iy) as f64;
        }
    }
    let det = gxx * gyy - gxy * gxy;
    let min_eig = (gxx + gyy) / 2.0 - (((gxx - gyy) / 2.0).powi(2) + gxy * gxy).sqrt();
    if det <= 0.0 || min_eig / (n as f64) < 1e-7 {
        return None;
    }
    let mut d = guess;
    for _ in 0..30 {
        let (qx, qy) = (p[0] + d[0], p[1] + d[1]);
        if qx < -(r as f32) || qy < -(r as f32) || qx > (b.w as isize + r) as f32 || qy > (b.h as isize + r) as f32 {
            return None;
        }
        let (mut bx, mut by) = (0.0f64, 0.0f64);
        let mut k = 0;
        for dy in -r..=r {
            for dx in -r..=r {
                let diff = (vals[k] - b.sample(qx + dx as f32, qy + dy as f32)) as f64;
                bx += diff * gx[k] as f64;
                by += diff * gy[k] as f64;
                k += 1;
            }
        }
        let step = [((gyy * bx - gxy * by) / det) as f32, ((gxx * by - gxy * bx) / det) as f32];
        d = [d[0] + step[0], d[1] + step[1]];
        if step[0].hypot(step[1]) < 1e-3 {
            break;
        }
    }
    Some(d)
}

/// Where feature `p` (frame position) of `from` is in `to`: pyramidal
/// Lucas–Kanade, coarse to fine. None when it can't be followed or ends
/// outside the frame.
pub fn follow(from: &Pyramid, to: &Pyramid, p: Pt, r: usize) -> Option<Pt> {
    let levels = from.levels.len().min(to.levels.len());
    let mut guess = [0.0f32; 2];
    for l in (0..levels).rev() {
        let s = (1u32 << l) as f32;
        // Frame position → level l, pixel centers on integers.
        let pl = [p[0] / s - 0.5, p[1] / s - 0.5];
        let d = lk_level(&from.levels[l], &to.levels[l], pl, guess, r)?;
        guess = if l > 0 { [d[0] * 2.0, d[1] * 2.0] } else { d };
    }
    let q = [p[0] + guess[0], p[1] + guess[1]];
    let (w, h) = (to.levels[0].w as f32, to.levels[0].h as f32);
    (q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= w && q[1] <= h && q.iter().all(|v| v.is_finite())).then_some(q)
}

/// [`follow`], kept only if following back from `to` lands within
/// `max_error` of `p`.
pub fn follow_checked(from: &Pyramid, to: &Pyramid, p: Pt, r: usize, max_error: f32) -> Option<Pt> {
    let q = follow(from, to, p, r)?;
    let back = follow(to, from, q, r)?;
    ((back[0] - p[0]).hypot(back[1] - p[1]) <= max_error).then_some(q)
}

/// Refine where the window of `anchor` around `p` (pixel centers on
/// integers) is in `img`: an affine warp `a` (2×2, about the window's
/// center) and a position `q`, starting from the given ones, by Gauss–Newton
/// on the brightness difference. None if it doesn't settle or the warp
/// degenerates.
fn affine_refine(anchor: &Gray, img: &Gray, p: Pt, a: [f32; 4], q: Pt, r: usize) -> Option<([f32; 4], Pt)> {
    use nalgebra::{Matrix6, Vector6};
    let r = r as isize;
    let mut tmpl = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
    for dy in -r..=r {
        for dx in -r..=r {
            tmpl.push((dx as f64, dy as f64, anchor.sample(p[0] + dx as f32, p[1] + dy as f32) as f64));
        }
    }
    let mut x = Vector6::new(a[0] as f64, a[1] as f64, a[2] as f64, a[3] as f64, q[0] as f64, q[1] as f64);
    for _ in 0..20 {
        let (mut h, mut b) = (Matrix6::<f64>::zeros(), Vector6::<f64>::zeros());
        for &(dx, dy, t) in &tmpl {
            let u = (x[0] * dx + x[1] * dy + x[4]) as f32;
            let v = (x[2] * dx + x[3] * dy + x[5]) as f32;
            let gx = ((img.sample(u + 1.0, v) - img.sample(u - 1.0, v)) / 2.0) as f64;
            let gy = ((img.sample(u, v + 1.0) - img.sample(u, v - 1.0)) / 2.0) as f64;
            let e = t - img.sample(u, v) as f64;
            let j = Vector6::new(gx * dx, gx * dy, gy * dx, gy * dy, gx, gy);
            h += j * j.transpose();
            b += j * e;
        }
        let step = h.cholesky()?.solve(&b);
        x += step;
        if step[4].hypot(step[5]) < 1e-4 && step.rows(0, 4).norm() < 1e-5 {
            let det = x[0] * x[3] - x[1] * x[2];
            let ok = (0.5..2.0).contains(&det) && (x[4] as f32) >= 0.0 && (x[5] as f32) >= 0.0 && x[4] <= img.w as f64 && x[5] <= img.h as f64;
            return ok.then(|| ([x[0] as f32, x[1] as f32, x[2] as f32, x[3] as f32], [x[4] as f32, x[5] as f32]));
        }
    }
    None
}

/// A feature's positions on consecutive frames from `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureTrack {
    pub start: i64,
    pub points: Vec<Pt>,
}

impl FeatureTrack {
    /// Its position on `frame`, if it was tracked there.
    pub fn at(&self, frame: i64) -> Option<Pt> {
        let i = frame - self.start;
        (i >= 0).then(|| self.points.get(i as usize).copied()).flatten()
    }

    /// The last frame it was tracked on.
    pub fn end(&self) -> i64 {
        self.start + self.points.len() as i64 - 1
    }
}

/// Follows features through frames given one at a time, finding new ones
/// as old ones are lost.
pub struct FeatureTracker {
    pub settings: Settings,
    pub tracks: Vec<FeatureTrack>,
    /// Indices into `tracks` still being followed.
    live: Vec<usize>,
    /// Each track's window warp from its first frame to its last.
    warps: Vec<[f32; 4]>,
    prev: Option<(i64, Pyramid)>,
    /// The finest level of each frame a live feature was found on.
    anchors: std::collections::HashMap<i64, std::sync::Arc<Gray>>,
}

impl FeatureTracker {
    pub fn new(settings: Settings) -> FeatureTracker {
        FeatureTracker { settings, tracks: Vec::new(), live: Vec::new(), warps: Vec::new(), prev: None, anchors: Default::default() }
    }

    /// The tracks still being followed.
    pub fn live(&self) -> impl Iterator<Item = &FeatureTrack> {
        self.live.iter().map(|&i| &self.tracks[i])
    }

    /// Take the next frame (`frame` must follow the last one by 1): follow
    /// the live features into it, drop those that fail the check, and top
    /// up with new features when fewer than nine in ten are left.
    pub fn push(&mut self, frame: i64, img: &Image) {
        let s = self.settings.clone();
        let pyr = Pyramid::new(Gray::new(img), s.levels);
        if let Some((pf, prev)) = &self.prev {
            assert_eq!(frame, pf + 1, "frames must come in order");
            // Followed from the last frame, then refined against the first
            // (a larger window than the search's, to pin the warp down);
            // a refinement far from the followed position is a lost track.
            let moved: Vec<Option<([f32; 4], Pt)>> = self
                .live
                .par_iter()
                .map(|&i| {
                    let t = &self.tracks[i];
                    let q = follow_checked(prev, &pyr, *t.points.last().unwrap(), s.radius, s.max_fb_error)?;
                    let half = |p: Pt| [p[0] - 0.5, p[1] - 0.5];
                    let (a, r) = affine_refine(&self.anchors[&t.start], &pyr.levels[0], half(t.points[0]), self.warps[i], half(q), s.radius + 2)?;
                    let r = [r[0] + 0.5, r[1] + 0.5];
                    ((r[0] - q[0]).hypot(r[1] - q[1]) < 2.0).then_some((a, r))
                })
                .collect();
            let mut still = Vec::with_capacity(self.live.len());
            for (&i, m) in self.live.iter().zip(moved) {
                if let Some((a, q)) = m {
                    self.tracks[i].points.push(q);
                    self.warps[i] = a;
                    still.push(i);
                }
            }
            self.live = still;
        }
        if self.live.len() * 10 < s.max_features * 9 {
            let avoid: Vec<Pt> = self.live().map(|t| *t.points.last().unwrap()).collect();
            let border = s.radius + 2;
            let found = detect(&pyr.levels[0], s.max_features - self.live.len(), s.min_distance, s.quality, border, &avoid);
            if !found.is_empty() {
                self.anchors.insert(frame, std::sync::Arc::new(pyr.levels[0].clone()));
            }
            for (p, _) in found {
                self.live.push(self.tracks.len());
                self.tracks.push(FeatureTrack { start: frame, points: vec![p] });
                self.warps.push([1.0, 0.0, 0.0, 1.0]);
            }
        }
        // Frames no live feature was found on aren't needed any more.
        let starts: std::collections::HashSet<i64> = self.live().map(|t| t.start).collect();
        self.anchors.retain(|f, _| starts.contains(f));
        self.prev = Some((frame, pyr));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::geom::{self, Camera, Vec3};
    use crate::image::Size;
    use crate::scene::{Card, Scene};
    use std::sync::Arc;

    /// Repeatable pseudo-random numbers in 0..1.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// A `w` × `h` texture of `n` soft gray blobs (seeded), for test scenes.
    pub(crate) fn blob_texture(n: usize, w: usize, h: usize, seed: u64) -> Image {
        Blobs::new(n, w as f32, h as f32, seed).image(w, h, 0.0, 0.0)
    }

    /// Soft blobs at random places and sizes: a smooth texture with plenty
    /// of trackable spots, defined everywhere (`at`), so a shifted copy is
    /// exact.
    struct Blobs(Vec<(f32, f32, f32, f32)>);
    impl Blobs {
        fn new(n: usize, w: f32, h: f32, seed: u64) -> Blobs {
            let mut r = Lcg(seed);
            Blobs((0..n).map(|_| (r.next() * w, r.next() * h, 2.0 + r.next() * 5.0, 0.2 + r.next() * 0.8)).collect())
        }
        fn at(&self, x: f32, y: f32) -> f32 {
            self.0.iter().map(|&(bx, by, s, a)| a * (-((x - bx).powi(2) + (y - by).powi(2)) / (2.0 * s * s)).exp()).sum()
        }
        /// Pixel (i, j) samples the texture at (i + 0.5 + dx, j + 0.5 + dy).
        fn image(&self, w: usize, h: usize, dx: f32, dy: f32) -> Image {
            let mut img = Image::new(w, h);
            img.pixels.par_iter_mut().enumerate().for_each(|(i, p)| {
                let v = self.at((i % w) as f32 + 0.5 + dx, (i / w) as f32 + 0.5 + dy);
                *p = [v, v, v, 1.0];
            });
            img
        }
    }

    #[test]
    fn corners_are_found_spaced_and_away_from_edges() {
        // A white square on black: its four corners, nothing along its edges.
        let mut img = Image::filled(80, 60, [0.0, 0.0, 0.0, 1.0]);
        for y in 20..40 {
            for x in 30..55 {
                img.set(x, y, [1.0; 4]);
            }
        }
        let found = detect(&Gray::new(&img), 10, 5.0, 0.1, 4, &[]);
        assert_eq!(found.len(), 4, "{found:?}");
        for c in [[30.0, 20.0], [55.0, 20.0], [55.0, 40.0], [30.0, 40.0]] {
            assert!(found.iter().any(|(p, _)| (p[0] - c[0]).abs() <= 2.0 && (p[1] - c[1]).abs() <= 2.0), "{c:?} in {found:?}");
        }
        // Avoiding a corner, or with a spacing wider than the square, fewer.
        assert_eq!(detect(&Gray::new(&img), 10, 5.0, 0.1, 4, &[[30.0, 20.0]]).len(), 3);
        assert_eq!(detect(&Gray::new(&img), 10, 40.0, 0.1, 4, &[]).len(), 1);
        // Plenty of blobs: spaced as asked, kept off the border.
        let blobs = Blobs::new(300, 320.0, 240.0, 7);
        let found = detect(&Gray::new(&blobs.image(320, 240, 0.0, 0.0)), 100, 10.0, 0.01, 9, &[]);
        assert_eq!(found.len(), 100);
        for (i, (p, _)) in found.iter().enumerate() {
            assert!(p[0] > 9.0 && p[1] > 9.0 && p[0] < 311.0 && p[1] < 231.0, "{p:?}");
            assert!(found[..i].iter().all(|(q, _)| (q[0] - p[0]).hypot(q[1] - p[1]) >= 10.0));
        }
    }

    #[test]
    fn features_follow_shifts_small_and_large() {
        // 640 × 480: four pyramid levels, as real footage gets.
        let blobs = Blobs::new(1200, 700.0, 540.0, 3);
        let a = Pyramid::new(Gray::new(&blobs.image(640, 480, 0.0, 0.0)), 4);
        assert_eq!(a.levels.len(), 4);
        let found = detect(&a.levels[0], 60, 12.0, 0.01, 40, &[]);
        assert!(found.len() >= 40, "{}", found.len());
        // Content moving by (−d): features move by −d too.
        for d in [[0.3f32, -0.2], [3.6, 1.25], [-18.4, 11.7]] {
            let b = Pyramid::new(Gray::new(&blobs.image(640, 480, d[0], d[1])), 4);
            let mut errs: Vec<f32> = found
                .iter()
                .filter_map(|(p, _)| follow_checked(&a, &b, *p, 7, 0.5).map(|q| (q[0] - (p[0] - d[0])).hypot(q[1] - (p[1] - d[1]))))
                .collect();
            assert!(errs.len() * 10 >= found.len() * 9, "{d:?}: {} of {} kept", errs.len(), found.len());
            errs.sort_by(f32::total_cmp);
            let (median, worst) = (errs[errs.len() / 2], errs[errs.len() - 1]);
            assert!(median < 0.02 && worst < 0.1, "{d:?}: median {median}, worst {worst}");
        }
        // Into a different image: the check drops (nearly) everything.
        let other = Pyramid::new(Gray::new(&Blobs::new(1200, 700.0, 540.0, 99).image(640, 480, 0.0, 0.0)), 4);
        let kept = found.iter().filter(|(p, _)| follow_checked(&a, &other, *p, 7, 0.5).is_some()).count();
        assert!(kept * 10 < found.len(), "{kept} kept");
    }

    /// A camera moving past a textured wall, rendered with Scanline
    /// Render: every position each tracked feature took, with where the
    /// wall point it started on really projects that frame.
    fn rendered_camera_move() -> (FeatureTracker, Vec<(i64, f64)>) {
        let fmt = Size { width: 320, height: 180 };
        let tex = Blobs::new(1500, 512.0, 512.0, 11).image(512, 512, 0.0, 0.0);
        let wall = Card { image: Arc::new(tex), world: geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]), size: [16.0, 16.0], node: String::new() };
        let scene = Scene { cards: vec![wall], ..Scene::default() };
        let cam = |t: f64| Camera { focal: 18.0, world: geom::trs([0.06 * t, 0.03 * t, -0.08 * t], [0.0, 0.4 * t, 0.2 * t], [1.0; 3]), ..Camera::default() };
        let mut tracker = FeatureTracker::new(Settings { max_features: 150, min_distance: 10.0, ..Settings::default() });
        for f in 0..12 {
            tracker.push(f, &crate::render3d::render(&scene, &cam(f as f64), fmt, 3, None));
        }
        // Where a feature first seen at `p` on frame `s` (on the wall,
        // z = −5) should be on frame `t`.
        let truth = |s: i64, p: Pt, t: i64| {
            let c = cam(s as f64);
            let o = geom::transform_point(&c.world, &Vec3::zeros());
            let dir = c.unproject([p[0] as f64, p[1] as f64], 1.0, fmt) - o;
            let x = o + dir * ((-5.0 - o.z) / dir.z);
            cam(t as f64).project(&x, fmt).unwrap().0
        };
        let mut errs = Vec::new();
        for tr in &tracker.tracks {
            for t in tr.start + 1..=tr.end() {
                let (q, want) = (tr.at(t).unwrap(), truth(tr.start, tr.points[0], t));
                errs.push((t - tr.start, (q[0] as f64 - want[0]).hypot(q[1] as f64 - want[1])));
            }
        }
        (tracker, errs)
    }

    fn median_and_p95(errs: impl Iterator<Item = f64>) -> (f64, f64) {
        let mut e: Vec<f64> = errs.collect();
        e.sort_by(f64::total_cmp);
        (e[e.len() / 2], e[e.len() * 95 / 100])
    }

    #[test]
    fn features_track_a_rendered_camera_move() {
        let (tracker, errs) = rendered_camera_move();
        let long = tracker.tracks.iter().filter(|t| t.start == 0 && t.end() == 11).count();
        assert!(long >= 60, "{long} features lasted all 12 frames");
        assert!(tracker.live().count() >= 135, "topped up: {}", tracker.live().count());
        // One frame on, features land within a few hundredths of a pixel.
        let (median, p95) = median_and_p95(errs.iter().filter(|e| e.0 == 1).map(|e| e.1));
        assert!(median < 0.06 && p95 < 0.3, "after one frame: median {median}, 95% {p95}");
    }

    /// Over the whole shot (a push-in with a pan and a roll, up to 11
    /// frames a track) the features stay on their wall points: the affine
    /// refinement against each feature's first frame keeps the window's
    /// stretching from turning into drift (shift-only tracking reached a
    /// 0.22 px median, 0.94 px at the 95th percentile, growing every frame;
    /// now about 0.015 and 0.12).
    #[test]
    fn rendered_tracks_drift_under_a_tenth_of_a_pixel() {
        let (_, errs) = rendered_camera_move();
        let (median, p95) = median_and_p95(errs.into_iter().map(|e| e.1));
        assert!(median < 0.05 && p95 < 0.2, "median {median}, 95% {p95}");
    }
}
