//! The path a node-editor wire takes through its bend points.
//!
//! A wire runs from its source's output port `a` to an input port `b`
//! through its bends (in order from the source). Straight wires are a
//! polyline through them; curved wires leave and enter their ports level,
//! as they do without bends, and pass smoothly through every bend (a
//! Catmull-Rom spline as cubic Bézier segments). Without bends a wire is
//! exactly the one cubic it always was.

use eframe::egui::{epaint::CubicBezierShape, Color32, Pos2, Stroke, Vec2};

/// Samples per segment for hit-testing.
const SAMPLES: usize = 32;

/// How far a wire from `a` to `b` leaves and enters its ports level
/// (none when `straight`).
pub fn reach(a: Pos2, b: Pos2, straight: bool) -> f32 {
    if straight { 0.0 } else { ((b.x - a.x).abs() * 0.5).max(40.0) }
}

/// The wire's cubic segments, one per stretch between `a`, each bend and
/// `b`.
pub fn segments(a: Pos2, bends: &[Pos2], b: Pos2, straight: bool) -> Vec<[Pos2; 4]> {
    if bends.is_empty() {
        let dx = reach(a, b, straight);
        return vec![[a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b]];
    }
    let pts: Vec<Pos2> = std::iter::once(a).chain(bends.iter().copied()).chain(std::iter::once(b)).collect();
    if straight {
        return pts.windows(2).map(|w| [w[0], w[0], w[1], w[1]]).collect();
    }
    let n = pts.len() - 1;
    // Tangents (Hermite): level at the ports, Catmull-Rom at the bends.
    let level = |p: Pos2, q: Pos2| Vec2::new(3.0 * ((q.x - p.x).abs() * 0.5).max(20.0), 0.0);
    let tangent = |i: usize| match i {
        0 => level(pts[0], pts[1]),
        i if i == n => level(pts[n - 1], pts[n]),
        i => (pts[i + 1] - pts[i - 1]) * 0.5,
    };
    (0..n).map(|i| [pts[i], pts[i] + tangent(i) / 3.0, pts[i + 1] - tangent(i + 1) / 3.0, pts[i + 1]]).collect()
}

/// The shapes that draw `segs`.
pub fn shapes(segs: &[[Pos2; 4]], stroke: Stroke) -> Vec<CubicBezierShape> {
    segs.iter().map(|s| CubicBezierShape::from_points_stroke(*s, false, Color32::TRANSPARENT, stroke)).collect()
}

fn at(s: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    (s[0].to_vec2() * (u * u * u) + s[1].to_vec2() * (3.0 * u * u * t) + s[2].to_vec2() * (3.0 * u * t * t) + s[3].to_vec2() * (t * t * t)).to_pos2()
}

fn segment_distance(s: &[Pos2; 4], p: Pos2) -> f32 {
    let pts: Vec<Pos2> = (0..=SAMPLES).map(|i| at(s, i as f32 / SAMPLES as f32)).collect();
    pts.windows(2)
        .map(|w| {
            let (s, e) = (w[0], w[1]);
            let t = ((p - s).dot(e - s) / (e - s).length_sq().max(1e-6)).clamp(0.0, 1.0);
            s.lerp(e, t).distance(p)
        })
        .fold(f32::INFINITY, f32::min)
}

/// The segment nearest `p` (its index: it runs from bend `i − 1`, or the
/// source, to bend `i`, or the input) and how far `p` is from it.
pub fn nearest(segs: &[[Pos2; 4]], p: Pos2) -> (usize, f32) {
    segs.iter().enumerate().map(|(i, s)| (i, segment_distance(s, p))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, f32::INFINITY))
}

/// Distance from `p` to the wire.
pub fn distance(segs: &[[Pos2; 4]], p: Pos2) -> f32 {
    nearest(segs, p).1
}

/// How near square a leg snaps to: within this many screen points of
/// level or plumb, or this angle (whichever reaches further, so long legs
/// a few degrees off square snap too).
pub const SQUARE_PX: f32 = 6.0;
pub const SQUARE_DEGREES: f32 = 4.0;

/// Bend `p` between its neighbours along the wire (`prev`: the bend before
/// it or the source's port; `next`: the bend after it or the input's port)
/// moved onto the same x or y as one of them where its leg to that one is
/// nearly plumb or level (within `px` points, or [`SQUARE_DEGREES`] of its
/// length), the nearer one winning, so right-angle routes come out exactly
/// square. Which axes it snapped ([x, y]).
pub fn snap_square(p: Pos2, prev: Pos2, next: Pos2, px: f32) -> (Pos2, [bool; 2]) {
    let slack = SQUARE_DEGREES.to_radians().tan();
    // The coordinate `axis` takes from `n`, and how far off it is, if the
    // leg from `n` is nearly along the other axis.
    let near = |n: Pos2, axis: usize| {
        let (off, along) = if axis == 0 { (p.x - n.x, p.y - n.y) } else { (p.y - n.y, p.x - n.x) };
        let tol = px.max(along.abs() * slack);
        (off.abs() <= tol).then_some((off.abs(), if axis == 0 { n.x } else { n.y }))
    };
    let mut out = p;
    let mut snapped = [false; 2];
    for (axis, snapped) in snapped.iter_mut().enumerate() {
        let best = [near(prev, axis), near(next, axis)].into_iter().flatten().min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, v)) = best {
            if axis == 0 { out.x = v } else { out.y = v }
            *snapped = true;
        }
    }
    (out, snapped)
}

/// A wire's bend points split in two: those before a node dropped into it
/// (graph coordinates), and those after.
pub type Split = (Vec<[f32; 2]>, Vec<[f32; 2]>);

/// A wire's bends split where a node goes into it, on segment `seg`: those
/// before it (the source's new wire) and after (the input's).
pub fn split<T: Clone>(bends: &[T], seg: usize) -> (Vec<T>, Vec<T>) {
    let k = seg.min(bends.len());
    (bends[..k].to_vec(), bends[k..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_bends_it_is_the_wire_it_always_was() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(300.0, 100.0));
        assert_eq!(segments(a, &[], b, false), vec![[a, Pos2::new(150.0, 0.0), Pos2::new(150.0, 100.0), b]]);
        assert_eq!(segments(a, &[], b, true), vec![[a, a, b, b]]);
    }

    #[test]
    fn straight_wires_are_a_polyline_through_the_bends() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0));
        let bends = [Pos2::new(100.0, 0.0), Pos2::new(100.0, 100.0)];
        let segs = segments(a, &bends, b, true);
        assert_eq!(segs.len(), 3);
        // On the corner's legs, not across it.
        assert!(distance(&segs, Pos2::new(100.0, 50.0)) < 1e-3);
        assert!(distance(&segs, Pos2::new(50.0, 50.0)) > 49.0);
        assert_eq!(nearest(&segs, Pos2::new(150.0, 101.0)).0, 2);
        assert_eq!(nearest(&segs, Pos2::new(40.0, -2.0)).0, 0);
    }

    #[test]
    fn curved_wires_leave_level_and_pass_through_every_bend() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(400.0, 0.0));
        let bends = [Pos2::new(150.0, 120.0), Pos2::new(260.0, -80.0)];
        let segs = segments(a, &bends, b, false);
        assert_eq!(segs.len(), 3);
        for (i, p) in bends.iter().enumerate() {
            assert_eq!(segs[i][3], *p);
            assert_eq!(segs[i + 1][0], *p);
            // Smooth: the tangent carries on through the bend.
            let (into, out) = (segs[i][3] - segs[i][2], segs[i + 1][1] - segs[i + 1][0]);
            assert!((into.normalized() - out.normalized()).length() < 1e-4 && (into.length() - out.length()).abs() < 1e-3);
            assert!(distance(&segs, *p) < 1e-3);
        }
        // Level at both ports.
        assert_eq!(segs[0][1].y, a.y);
        assert_eq!(segs[2][2].y, b.y);
        assert!(segs[0][1].x > a.x && segs[2][2].x < b.x);
    }

    /// Nearly level or plumb legs come out exactly so; others stay.
    #[test]
    fn bends_snap_to_square_legs() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(400.0, 300.0));
        // 3 points under the port's level over 100: level.
        assert_eq!(snap_square(Pos2::new(100.0, 3.0), a, b, SQUARE_PX), (Pos2::new(100.0, 0.0), [false, true]));
        // 95°: 4° of a 300-point leg is 21 points, more than the 6.
        let (p, s) = snap_square(Pos2::new(385.0, 0.0), a, b, SQUARE_PX);
        assert_eq!((p, s), (Pos2::new(400.0, 0.0), [true, true]), "a corner under the input, level with the output");
        // 10° off isn't snapped.
        let off = Pos2::new(200.0, 200.0 * 10f32.to_radians().tan());
        assert_eq!(snap_square(off, a, Pos2::new(900.0, 900.0), SQUARE_PX).1, [false, false]);
        // The nearer neighbour wins.
        let (p, _) = snap_square(Pos2::new(50.0, 100.0), Pos2::new(48.0, 0.0), Pos2::new(53.0, 200.0), SQUARE_PX);
        assert_eq!(p.x, 48.0);
        let (p, _) = snap_square(Pos2::new(52.0, 100.0), Pos2::new(48.0, 0.0), Pos2::new(53.0, 200.0), SQUARE_PX);
        assert_eq!(p.x, 53.0);
    }

    #[test]
    fn a_split_hands_each_new_wire_its_side_s_bends() {
        let bends = [1, 2, 3];
        assert_eq!(split(&bends, 0), (vec![], vec![1, 2, 3]));
        assert_eq!(split(&bends, 2), (vec![1, 2], vec![3]));
        assert_eq!(split(&bends, 3), (vec![1, 2, 3], vec![]));
        assert_eq!(split(&bends, 9), (vec![1, 2, 3], vec![]));
    }
}
