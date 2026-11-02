//! Per-point feather for Roto and Polygon: a soft edge whose width varies
//! along the outline.
//!
//! Each vertex carries its own feather (pixels, `point_feather`, one per
//! vertex in the order of `points`; missing entries count as 0). The width
//! at a point of the outline is the shape's overall Feather plus the local
//! amount, which runs linearly between neighbouring vertices: by the
//! bezier parameter along a Roto segment, by position along a Polygon
//! edge. Coverage is the usual distance-based soft edge, centred on the
//! outline: a pixel at distance `d` from an edge whose width at the
//! closest point is `w` sits at `n = d / w` across the ramp, the smallest
//! `n` over the edges wins, and coverage is a smoothstep of `0.5 ± n`.
//! With one width everywhere that is exactly [`shape::coverage`]; shapes
//! whose per-point values are all 0 keep using that.

use crate::shape::{self, FillRule, Pt};
use rayon::prelude::*;

/// Vertex `i`'s own feather (0 when missing, never negative).
pub fn local_at(local: &[f32], i: usize) -> f32 {
    local.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0)
}

/// Whether any of the first `n` vertices has a feather of its own.
pub fn any_local(local: &[f32], n: usize) -> bool {
    (0..n).any(|i| local_at(local, i) > 0.0)
}

/// The largest per-point feather of the first `n` vertices.
pub fn max_local(local: &[f32], n: usize) -> f32 {
    (0..n).map(|i| local_at(local, i)).fold(0.0, f32::max)
}

/// A closed roto spline (flat points, stride 6) flattened exactly as
/// [`shape::spline`] does, with each flattened vertex's local feather.
pub fn spline(points: &[f32], local: &[f32]) -> (Vec<Pt>, Vec<f32>) {
    let segments = shape::spline_segments(points);
    let n = segments.len();
    let mut outline = Vec::new();
    let mut widths = Vec::new();
    for (i, c) in segments.iter().enumerate() {
        let (a, b) = (local_at(local, i), local_at(local, (i + 1) % n));
        let k = shape::pieces(c, 0.05);
        for j in 0..k {
            let t = j as f32 / k as f32;
            outline.push(shape::cubic_at(c, t));
            widths.push(a + (b - a) * t);
        }
    }
    (outline, widths)
}

/// A polygon's (flat points, stride 2) vertices with their local feathers.
pub fn polygon(points: &[f32], local: &[f32]) -> (Vec<Pt>, Vec<f32>) {
    let outline = points.as_chunks::<2>().0.to_vec();
    let widths = (0..outline.len()).map(|i| local_at(local, i)).collect();
    (outline, widths)
}

/// Distance from `p` to the segment `a`–`b`, and where along it (0–1) the
/// closest point is.
fn closest(p: Pt, a: Pt, b: Pt) -> (f32, f32) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (px, py) = (p[0] - a[0], p[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (px - t * dx, py - t * dy);
    ((ex * ex + ey * ey).sqrt(), t)
}

struct Edge {
    a: Pt,
    b: Pt,
    /// Soft-edge widths at `a` and `b` (at least 1: the anti-aliasing ramp).
    wa: f32,
    wb: f32,
    /// Half the larger width: how far this edge reaches.
    half: f32,
    lo: Pt,
    hi: Pt,
}

/// Per-pixel coverage (0–1, row-major, `width` × `height`) of the closed
/// `outline`, its soft edge `global + local[i]` wide at vertex `i` (see
/// the module notes), filled by `rule`.
pub fn coverage(outline: &[Pt], local: &[f32], global: f32, width: usize, height: usize, rule: FillRule) -> Vec<f32> {
    let mut out = vec![0.0; width * height];
    let n = outline.len();
    if n < 3 || width == 0 {
        return out;
    }
    let w = |i: usize| (global.max(0.0) + local.get(i).copied().unwrap_or(0.0).max(0.0)).max(1.0);
    let edges: Vec<Edge> = (0..n)
        .map(|i| (outline[i], outline[(i + 1) % n], w(i), w((i + 1) % n)))
        .filter(|(a, b, _, _)| a != b && a.iter().chain(b).all(|v| v.is_finite()))
        .map(|(a, b, wa, wb)| Edge { a, b, wa, wb, half: wa.max(wb) / 2.0, lo: [a[0].min(b[0]), a[1].min(b[1])], hi: [a[0].max(b[0]), a[1].max(b[1])] })
        .collect();
    if edges.is_empty() {
        return out;
    }
    let half = edges.iter().map(|e| e.half).fold(0.5, f32::max);
    let lo = edges.iter().fold([f32::MAX; 2], |m, e| [m[0].min(e.lo[0]), m[1].min(e.lo[1])]);
    let hi = edges.iter().fold([f32::MIN; 2], |m, e| [m[0].max(e.hi[0]), m[1].max(e.hi[1])]);
    let x0 = (lo[0] - half).floor().max(0.0) as usize;
    let x1 = ((hi[0] + half).ceil().max(0.0) as usize).min(width);

    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let yc = y as f32 + 0.5;
        if yc < lo[1] - half || yc > hi[1] + half {
            return;
        }
        let mut crossings: Vec<(f32, i32)> = edges
            .iter()
            .filter(|e| e.lo[1] <= yc && yc < e.hi[1])
            .map(|e| {
                let t = (yc - e.a[1]) / (e.b[1] - e.a[1]);
                (e.a[0] + t * (e.b[0] - e.a[0]), if e.b[1] > e.a[1] { 1 } else { -1 })
            })
            .collect();
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let near: Vec<&Edge> = edges.iter().filter(|e| e.lo[1] - e.half <= yc && yc <= e.hi[1] + e.half).collect();

        let (mut k, mut winding) = (0, 0);
        for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
            let xc = x as f32 + 0.5;
            while k < crossings.len() && crossings[k].0 < xc {
                winding += crossings[k].1;
                k += 1;
            }
            let inside = match rule {
                FillRule::NonZero => winding != 0,
                FillRule::EvenOdd => winding % 2 != 0,
            };
            // How far across its edge's ramp the pixel is (0.5: the ramp's end).
            let mut across = 0.5f32;
            for e in &near {
                if xc >= e.lo[0] - e.half && xc <= e.hi[0] + e.half {
                    let (d, t) = closest([xc, yc], e.a, e.b);
                    across = across.min(d / (e.wa + (e.wb - e.wa) * t));
                }
            }
            *px = if across >= 0.5 {
                inside as u8 as f32
            } else {
                let t = (0.5 + if inside { across } else { -across }).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(c: &[f32], w: usize, x: usize, y: usize) -> f32 {
        c[y * w + x]
    }

    fn square() -> Vec<f32> {
        vec![20.0, 20.0, 80.0, 20.0, 80.0, 80.0, 20.0, 80.0]
    }

    #[test]
    fn one_width_everywhere_matches_the_plain_coverage() {
        let (outline, _) = polygon(&square(), &[]);
        for (global, local) in [(0.0, 0.0), (6.0, 0.0), (0.0, 9.0), (4.0, 5.5)] {
            let mine = coverage(&outline, &[local; 4], global, 100, 100, FillRule::NonZero);
            let plain = shape::coverage(std::slice::from_ref(&outline), 100, 100, global + local, FillRule::NonZero);
            let worst = mine.iter().zip(&plain).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 1e-5, "global {global} local {local}: {worst}");
        }
    }

    #[test]
    fn the_edge_is_hard_by_a_hard_point_and_soft_by_a_soft_one() {
        // Hard at the top-left corner, 40 px soft at the bottom-right.
        let (outline, local) = polygon(&square(), &[0.0, 20.0, 40.0, 20.0]);
        let c = coverage(&outline, &local, 0.0, 100, 100, FillRule::NonZero);
        // Along the top edge near the hard corner: a 1 px ramp.
        assert!(at(&c, 100, 25, 18) == 0.0 && at(&c, 100, 25, 22) == 1.0);
        // Along the right edge near the soft corner: still ramping 10 px in.
        let inside = at(&c, 100, 70, 75);
        assert!(inside > 0.5 && inside < 1.0, "{inside}");
        assert!(at(&c, 100, 90, 75) > 0.0);
    }

    #[test]
    fn the_width_changes_smoothly_along_the_edge() {
        let (outline, local) = polygon(&square(), &[0.0, 30.0, 30.0, 0.0]);
        let c = coverage(&outline, &local, 2.0, 100, 100, FillRule::NonZero);
        // Down the column just outside the top edge, x from 20 to 80: no jumps.
        let row: Vec<f32> = (20..80).map(|x| at(&c, 100, x, 17)).collect();
        let jump = row.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(jump < 0.05, "{jump}");
        assert!(row[0] < row[59], "{row:?}");
    }

    #[test]
    fn roto_flattening_matches_the_plain_spline() {
        let pts = [256.0, 136.0, -66.3, 0.0, 66.3, 0.0, 376.0, 256.0, 0.0, -66.3, 0.0, 66.3, 256.0, 376.0, 66.3, 0.0, -66.3, 0.0, 136.0, 256.0, 0.0, 66.3, 0.0, -66.3];
        let (outline, widths) = spline(&pts, &[0.0, 8.0]);
        assert_eq!(outline, shape::spline(&pts));
        // The first segment runs from 0 to 8, the second from 8 down to 0
        // (the missing third and fourth read as 0).
        assert_eq!(widths[0], 0.0);
        assert!(widths.iter().all(|w| (0.0..=8.0).contains(w)));
        assert!(widths.iter().any(|w| *w > 7.0));
    }

    use crate::ops::{eval, resolve_params, EvalContext};
    use serde_json::json;

    const CTX: EvalContext = EvalContext { frame: 1, write_outputs: false };

    fn render(op: &str, params: serde_json::Value, frame: i64) -> crate::image::Image {
        eval(op, &resolve_params(op, &params, frame), &[None], &CTX).unwrap()
    }

    /// Projects saved before per-point feather (no `point_feather`) render
    /// exactly as an all-zero list does, and as an empty one: the plain path.
    #[test]
    fn old_projects_render_the_same() {
        let poly = json!({"width": 96, "height": 80, "points": [10, 10, 80, 15, 60, 70, 15, 60], "feather": 6});
        let roto = json!({"width": 96, "height": 80, "feather": 3, "fill_rule": "even-odd",
            "points": [48, 10, -20, 0, 20, 0, 85, 40, 0, -15, 0, 15, 48, 70, 20, 0, -20, 0, 10, 40, 0, 15, 0, -15]});
        let moving = json!({"width": 96, "height": 80, "motion_blur": 4,
            "points": {"keys": [[1, [10, 10, 80, 15, 60, 70]], [3, [20, 10, 90, 15, 70, 70]]], "interp": "linear"}});
        for (op, old) in [("polygon", poly), ("roto", roto), ("polygon", moving)] {
            let before = render(op, old.clone(), 2);
            for list in [json!([]), json!([0, 0, 0, 0])] {
                let mut p = old.clone();
                p["point_feather"] = list;
                let after = render(op, p, 2);
                assert!(before.window() == after.window() && before.pixels == after.pixels, "{op}");
            }
        }
    }

    /// A Roto soft at one point and hard at the others, and a Polygon too:
    /// the soft side spreads past the outline, the hard side doesn't.
    #[test]
    fn per_point_feather_softens_only_near_its_point() {
        // A square-ish roto with straight sides (zero tangents).
        let pts = json!([20, 20, 0, 0, 0, 0, 80, 20, 0, 0, 0, 0, 80, 80, 0, 0, 0, 0, 20, 80, 0, 0, 0, 0]);
        for (op, points) in [("roto", pts), ("polygon", json!([20, 20, 80, 20, 80, 80, 20, 80]))] {
            let img = render(op, json!({"width": 100, "height": 100, "points": points, "point_feather": [0, 0, 30]}), 1);
            let a = |x: i32, y: i32| img.at(x, y)[3];
            // Beside the soft corner (80, 80): coverage well outside the edge.
            assert!(a(86, 72) > 0.05, "{op}: {}", a(86, 72));
            // Beside the hard corner (20, 20): nothing two pixels out.
            assert_eq!(a(17, 25), 0.0, "{op}");
            assert!(img.window().x1 > 90, "{op}: the bounds take in the soft edge");
        }
    }

    /// Per-point feather animates as one value, with its own keys.
    #[test]
    fn point_feather_animates() {
        let p = json!({"width": 100, "height": 100, "points": [20, 20, 80, 20, 80, 80, 20, 80],
            "point_feather": {"keys": [[1, [0, 0, 0, 0]], [11, [0, 0, 40, 0]]], "interp": "linear"}});
        let (start, mid) = (render("polygon", p.clone(), 1), render("polygon", p, 6));
        assert_eq!(start.at(86, 72)[3], 0.0);
        assert!(mid.at(86, 72)[3] > 0.0);
    }

    #[test]
    fn missing_negative_and_odd_values_read_safely() {
        assert_eq!((local_at(&[3.0], 5), local_at(&[-2.0], 0), local_at(&[f32::NAN], 0)), (0.0, 0.0, 0.0));
        assert!(!any_local(&[0.0, 0.0, 7.0], 2) && any_local(&[0.0, 0.0, 7.0], 3));
        assert_eq!(max_local(&[1.0, 9.0, 4.0], 3), 9.0);
    }
}
