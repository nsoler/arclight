//! Shape rasterization: anti-aliased, feathered coverage of closed polygons.
//!
//! Coverage comes from the signed distance to the outline, so one routine
//! gives the anti-aliased edge (a 1 px ramp) and the feathered edge (a wider
//! ramp) for any polygon. Inside/outside comes from scanline crossings, so
//! self-intersecting outlines follow the chosen fill rule. Curved shapes are
//! flattened to polygons first, finely enough that the error is far below a
//! pixel.

use rayon::prelude::*;

/// A point in image pixels, y down.
pub type Pt = [f32; 2];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

struct Edge {
    a: Pt,
    b: Pt,
    lo: Pt,
    hi: Pt,
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (px, py) = (p[0] - a[0], p[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (px - t * dx, py - t * dy);
    (ex * ex + ey * ey).sqrt()
}

/// Per-pixel coverage (0–1, row-major) of the area enclosed by `polys`,
/// treated as one compound path (so an inner polygon can cut a hole).
///
/// `feather` is the width in pixels of the soft edge, centered on the
/// outline; below 1 it is a 1 px anti-aliasing ramp.
pub fn coverage(polys: &[Vec<Pt>], width: usize, height: usize, feather: f32, rule: FillRule) -> Vec<f32> {
    let mut out = vec![0.0; width * height];
    let edges: Vec<Edge> = polys
        .iter()
        .filter(|p| p.len() >= 3)
        .flat_map(|p| (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()])))
        .filter(|(a, b)| a != b && a.iter().chain(b).all(|v| v.is_finite()))
        .map(|(a, b)| Edge { a, b, lo: [a[0].min(b[0]), a[1].min(b[1])], hi: [a[0].max(b[0]), a[1].max(b[1])] })
        .collect();
    if edges.is_empty() || width == 0 {
        return out;
    }
    let ramp = feather.max(1.0);
    let half = ramp / 2.0;
    let lo = edges.iter().fold([f32::MAX; 2], |m, e| [m[0].min(e.lo[0]), m[1].min(e.lo[1])]);
    let hi = edges.iter().fold([f32::MIN; 2], |m, e| [m[0].max(e.hi[0]), m[1].max(e.hi[1])]);
    let x0 = (lo[0] - half).floor().max(0.0) as usize;
    let x1 = ((hi[0] + half).ceil().max(0.0) as usize).min(width);

    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let yc = y as f32 + 0.5;
        if yc < lo[1] - half || yc > hi[1] + half {
            return;
        }
        // Where the scanline crosses the outline, and in which direction.
        let mut crossings: Vec<(f32, i32)> = edges
            .iter()
            .filter(|e| e.lo[1] <= yc && yc < e.hi[1])
            .map(|e| {
                let t = (yc - e.a[1]) / (e.b[1] - e.a[1]);
                (e.a[0] + t * (e.b[0] - e.a[0]), if e.b[1] > e.a[1] { 1 } else { -1 })
            })
            .collect();
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Edges close enough to affect this row's soft edge.
        let near: Vec<&Edge> = edges.iter().filter(|e| e.lo[1] - half <= yc && yc <= e.hi[1] + half).collect();

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
            let mut d = half;
            for e in &near {
                if xc >= e.lo[0] - half && xc <= e.hi[0] + half {
                    d = d.min(segment_distance([xc, yc], e.a, e.b));
                }
            }
            *px = if d >= half {
                inside as u8 as f32
            } else {
                let t = (0.5 + if inside { d } else { -d } / ramp).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
        }
    });
    out
}

/// `v` rotated by `deg` degrees; positive is clockwise on screen (y down).
pub fn rotate(v: Pt, deg: f32) -> Pt {
    let (s, c) = deg.to_radians().sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

/// The corners of a `size` box centered on `center`, rotated by `deg`.
pub fn rectangle(center: Pt, size: Pt, deg: f32) -> Vec<Pt> {
    let (hx, hy) = (size[0] / 2.0, size[1] / 2.0);
    [[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]]
        .into_iter()
        .map(|c| {
            let r = rotate(c, deg);
            [center[0] + r[0], center[1] + r[1]]
        })
        .collect()
}

/// An ellipse filling a `size` box centered on `center`, rotated by `deg`,
/// as a polygon with segments about 4 px long.
pub fn ellipse(center: Pt, size: Pt, deg: f32) -> Vec<Pt> {
    let (rx, ry) = (size[0].abs() / 2.0, size[1].abs() / 2.0);
    let perimeter = std::f32::consts::PI * (rx + ry);
    let n = ((perimeter / 4.0).ceil() as usize).clamp(24, 4096);
    (0..n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            let r = rotate([rx * a.cos(), ry * a.sin()], deg);
            [center[0] + r[0], center[1] + r[1]]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(c: &[f32], w: usize, x: usize, y: usize) -> f32 {
        c[y * w + x]
    }

    #[test]
    fn square_is_solid_inside_and_soft_only_at_the_edge() {
        let sq = vec![rectangle([8.0, 8.0], [8.0, 8.0], 0.0)];
        let c = coverage(&sq, 16, 16, 0.0, FillRule::NonZero);
        assert_eq!(at(&c, 16, 8, 8), 1.0);
        assert_eq!(at(&c, 16, 1, 1), 0.0);
        // Pixel-aligned edges: pixel centers sit half a pixel from the outline.
        assert_eq!(at(&c, 16, 4, 8), 1.0);
        assert_eq!(at(&c, 16, 3, 8), 0.0);
        // Total area is the square's area.
        let area: f32 = c.iter().sum();
        assert!((area - 64.0).abs() < 0.5, "{area}");
    }

    #[test]
    fn half_pixel_edge_gives_half_coverage() {
        let sq = vec![rectangle([8.0, 8.0], [7.0, 20.0], 0.0)];
        let c = coverage(&sq, 16, 16, 0.0, FillRule::NonZero);
        // The left edge at x = 4.5 runs through the center of pixel 4.
        assert!((at(&c, 16, 4, 8) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn feather_spreads_the_edge_symmetrically() {
        let sq = vec![rectangle([32.0, 32.0], [32.0, 32.0], 0.0)];
        let c = coverage(&sq, 64, 64, 10.0, FillRule::NonZero);
        let (inner, outer) = (at(&c, 64, 18, 32), at(&c, 64, 13, 32));
        assert!(inner > 0.5 && inner < 1.0 && outer > 0.0 && outer < 0.5);
        assert!((inner + outer - 1.0).abs() < 1e-4, "symmetric about the edge");
        assert_eq!(at(&c, 64, 32, 32), 1.0);
        assert_eq!(at(&c, 64, 5, 32), 0.0);
    }

    #[test]
    fn fill_rules_differ_on_overlapping_loops() {
        // The same square twice, same direction: winding 2 inside.
        let sq = rectangle([8.0, 8.0], [8.0, 8.0], 0.0);
        let polys = vec![sq.clone(), sq];
        assert_eq!(at(&coverage(&polys, 16, 16, 0.0, FillRule::NonZero), 16, 8, 8), 1.0);
        assert_eq!(at(&coverage(&polys, 16, 16, 0.0, FillRule::EvenOdd), 16, 8, 8), 0.0);
    }

    #[test]
    fn ellipse_area_matches() {
        let e = vec![ellipse([50.0, 50.0], [60.0, 40.0], 30.0)];
        let c = coverage(&e, 100, 100, 0.0, FillRule::NonZero);
        let area: f32 = c.iter().sum();
        let exact = std::f32::consts::PI * 30.0 * 20.0;
        assert!((area - exact).abs() / exact < 0.005, "{area} vs {exact}");
    }

    #[test]
    fn rotation_is_clockwise_on_screen() {
        let r = rotate([1.0, 0.0], 90.0);
        assert!(r[0].abs() < 1e-6 && (r[1] - 1.0).abs() < 1e-6);
    }
}
