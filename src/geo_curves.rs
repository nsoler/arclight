//! Curve nodes (docs/geo-plan.md, P5): the Curve node, which makes a
//! curve from its own control points (drawn and edited in the 3D view) or
//! from a node's path over frames, and Resample. `ops.rs` registers them.
//!
//! A curve comes out as polylines ([`Curves`]) sampled from its control
//! points, each point with its `tangent` (unit), its `u` (0 at the start, 1
//! at the end, by length along it), `cp` (the control point it came after)
//! and a rotation-minimizing frame: `N` (along the curve, as Houdini's
//! curves have it, so Instance on Points aims copies along it) and `up`
//! (square to it, turning as little as it can from point to point).

use crate::geo::{self, Attr, Attrs, Curves};
use crate::geom::Vec3;
use crate::scene::{self, Scene};
use anyhow::{bail, Result};
use serde_json::Value;

/// How a curve goes between its control points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Straight lines.
    Polyline,
    /// Through the points, smoothly (Catmull-Rom: handles worked out).
    Smooth,
    /// Through the points, along their own handles.
    Bezier,
}

impl Kind {
    pub fn from_name(s: &str) -> Kind {
        match s {
            "smooth" => Kind::Smooth,
            "bezier" => Kind::Bezier,
            _ => Kind::Polyline,
        }
    }
}

/// A control point: where it is and its handles (offsets from it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub at: Vec3,
    pub before: Vec3,
    pub after: Vec3,
}

/// The control points of a Curve's `points` param (9 numbers each:
/// position, handle before, handle after).
pub fn controls(points: &Value) -> Vec<Control> {
    let v: Vec<f64> = points.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    v.as_chunks::<9>().0.iter().map(|c| Control { at: Vec3::new(c[0], c[1], c[2]), before: Vec3::new(c[3], c[4], c[5]), after: Vec3::new(c[6], c[7], c[8]) }).collect()
}

/// `points` as a Curve's `points` param.
pub fn points_value(points: &[Control]) -> Value {
    Value::from(points.iter().flat_map(|c| [c.at, c.before, c.after]).flat_map(|v| [v.x, v.y, v.z]).collect::<Vec<f64>>())
}

/// The handles a smooth curve has at each of `points` (Catmull-Rom: a
/// third of the way to the neighbours' difference, half of it; an open
/// curve's ends toward their one neighbour).
pub fn smooth_handles(points: &[Vec3], closed: bool) -> Vec<(Vec3, Vec3)> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let prev = if i > 0 { points[i - 1] } else if closed { points[n - 1] } else { points[i] };
            let next = if i + 1 < n { points[i + 1] } else if closed { points[0] } else { points[i] };
            let t = (next - prev) / 6.0;
            (-t, t)
        })
        .collect()
}

/// The point `t` of the way along cubic Bézier a, b, c, d, and its
/// direction there (not unit).
fn bezier(a: Vec3, b: Vec3, c: Vec3, d: Vec3, t: f64) -> (Vec3, Vec3) {
    let s = 1.0 - t;
    let p = a * (s * s * s) + b * (3.0 * s * s * t) + c * (3.0 * s * t * t) + d * (t * t * t);
    let dp = (b - a) * (3.0 * s * s) + (c - b) * (6.0 * s * t) + (d - c) * (3.0 * t * t);
    (p, dp)
}

/// A curve through `points` as `kind` (closed or not), `detail` points a
/// span for the curved kinds: its points, their directions (not unit) and
/// the control point each came after.
pub fn sample(points: &[Control], kind: Kind, closed: bool, detail: usize) -> (Vec<Vec3>, Vec<Vec3>, Vec<i32>) {
    let n = points.len();
    let (mut pts, mut dirs, mut cps) = (Vec::new(), Vec::new(), Vec::new());
    if n == 0 {
        return (pts, dirs, cps);
    }
    let spans = if closed && n > 1 { n } else { n - 1 };
    if kind == Kind::Polyline {
        for i in 0..n {
            let prev = if i > 0 { points[i - 1].at } else if closed { points[n - 1].at } else { points[i].at };
            let next = if i + 1 < n { points[i + 1].at } else if closed { points[0].at } else { points[i].at };
            pts.push(points[i].at);
            dirs.push(next - prev);
            cps.push(i as i32);
        }
        return (pts, dirs, cps);
    }
    let handles: Vec<(Vec3, Vec3)> = match kind {
        Kind::Smooth => smooth_handles(&points.iter().map(|c| c.at).collect::<Vec<_>>(), closed),
        _ => points.iter().map(|c| (c.before, c.after)).collect(),
    };
    let detail = detail.max(1);
    for s in 0..spans {
        let (i, j) = (s, (s + 1) % n);
        let (a, d) = (points[i].at, points[j].at);
        let (b, c) = (a + handles[i].1, d + handles[j].0);
        for k in 0..detail {
            let (p, dp) = bezier(a, b, c, d, k as f64 / detail as f64);
            pts.push(p);
            dirs.push(dp);
            cps.push(i as i32);
        }
    }
    if !closed || n == 1 {
        let last = n - 1;
        let dir = if spans > 0 {
            let i = last - 1;
            bezier(points[i].at, points[i].at + handles[i].1, points[last].at + handles[last].0, points[last].at, 1.0).1
        } else {
            Vec3::zeros()
        };
        pts.push(points[last].at);
        dirs.push(dir);
        cps.push(last as i32);
    }
    (pts, dirs, cps)
}

/// Each of `dirs` made unit; a zero one takes its neighbour's (a straight
/// curve's along it; none at all: +X).
fn unit_tangents(pts: &[Vec3], dirs: &[Vec3]) -> Vec<Vec3> {
    let n = pts.len();
    let mut out: Vec<Option<Vec3>> = dirs.iter().map(|d| d.try_normalize(1e-12)).collect();
    // A zero direction (a handle on its point): along the chord there.
    for i in 0..n {
        if out[i].is_none() {
            let prev = if i > 0 { pts[i - 1] } else { pts[i] };
            let next = if i + 1 < n { pts[i + 1] } else { pts[i] };
            out[i] = (next - prev).try_normalize(1e-12);
        }
    }
    let fallback = out.iter().flatten().next().copied().unwrap_or_else(Vec3::x);
    out.into_iter().map(|t| t.unwrap_or(fallback)).collect()
}

/// A rotation-minimizing `up` at each of `pts` (tangents `ts`, unit): the
/// first square to the tangent and as near the curve's own +Y as it can be
/// (+X where the tangent is along Y), then carried along by double
/// reflection (Wang et al. 2008), so it twists as little as it can.
pub fn rotation_minimizing(pts: &[Vec3], ts: &[Vec3]) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(pts.len());
    let Some(&t0) = ts.first() else { return out };
    let square = |r: Vec3, t: Vec3| (r - t * r.dot(&t)).try_normalize(1e-12);
    let mut r = square(Vec3::y(), t0).or_else(|| square(Vec3::x(), t0)).unwrap_or_else(Vec3::y);
    out.push(r);
    for i in 0..pts.len() - 1 {
        let v1 = pts[i + 1] - pts[i];
        let c1 = v1.dot(&v1);
        let (r_l, t_l) = if c1 > 1e-24 { (r - v1 * (2.0 / c1 * v1.dot(&r)), ts[i] - v1 * (2.0 / c1 * v1.dot(&ts[i]))) } else { (r, ts[i]) };
        let v2 = ts[i + 1] - t_l;
        let c2 = v2.dot(&v2);
        let next = if c2 > 1e-24 { r_l - v2 * (2.0 / c2 * v2.dot(&r_l)) } else { r_l };
        r = square(next, ts[i + 1]).unwrap_or(r);
        out.push(r);
    }
    out
}

/// A curve's attributes from its points: tangent, u, N and up (see the
/// module's notes); `cp` when given.
pub fn frame_attrs(pts: &[Vec3], dirs: &[Vec3], cps: Option<Vec<i32>>) -> Attrs {
    let n = pts.len();
    let ts = unit_tangents(pts, dirs);
    let ups = rotation_minimizing(pts, &ts);
    let mut along = Vec::with_capacity(n);
    let mut total = 0.0;
    for i in 0..n {
        if i > 0 {
            total += (pts[i] - pts[i - 1]).norm();
        }
        along.push(total);
    }
    let f3 = |v: &Vec3| [v.x as f32, v.y as f32, v.z as f32];
    let mut a = Attrs::default();
    let _ = a.set("tangent", Attr::Vec3(ts.iter().map(f3).collect()), n);
    let _ = a.set(geo::N, Attr::Vec3(ts.iter().map(f3).collect()), n);
    let _ = a.set(geo::UP, Attr::Vec3(ups.iter().map(f3).collect()), n);
    let _ = a.set("u", Attr::Float(along.iter().map(|d| if total > 0.0 { (d / total) as f32 } else { 0.0 }).collect()), n);
    if let Some(cps) = cps {
        let _ = a.set("cp", Attr::Int(cps), n);
    }
    a
}

/// A Curve: its control points (or `path`, a node's positions over frames,
/// worked out by the graph into `_path`) sampled into one curve, placed by
/// its translate, rotate and scale.
pub fn curve(params: &Value) -> Result<Scene> {
    let kind = Kind::from_name(params["kind"].as_str().unwrap_or("polyline"));
    let closed = params["closed"].as_bool().unwrap_or(false);
    let detail = params["detail"].as_u64().unwrap_or(16).clamp(1, 1000) as usize;
    let points: Vec<Control> = if params["source"].as_str() == Some("node path") {
        let path = params["_path"].as_array().cloned().unwrap_or_default();
        if path.is_empty() {
            bail!("Curve: name a node in Path of (a Camera Tracker, a camera, a locator...) whose position to follow");
        }
        let mut pts = Vec::new();
        for (k, p) in path.iter().enumerate() {
            let v: Vec<f64> = p.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
            if v.len() != 3 {
                bail!("Curve: Path of has no position at its {} frame ({p})", k + 1);
            }
            pts.push(Control { at: Vec3::new(v[0], v[1], v[2]), before: Vec3::zeros(), after: Vec3::zeros() });
        }
        pts
    } else {
        controls(&params["points"])
    };
    let (pts, dirs, cps) = sample(&points, kind, closed, detail);
    let mut c = Curves { world: scene::placement(params), ..Curves::default() };
    if pts.len() >= 2 {
        c.attrs = frame_attrs(&pts, &dirs, Some(cps));
        c.push(&pts, closed && points.len() > 2);
    }
    Ok(Scene { curves: vec![c], ..Scene::default() })
}

/// The frames a Curve following a node's path samples it at: Start to End,
/// every Step.
pub fn path_frames(params: &Value) -> Vec<f64> {
    let start = params["start"].as_f64().unwrap_or(1.0);
    let end = params["end"].as_f64().unwrap_or(100.0);
    let step = params["step"].as_f64().unwrap_or(1.0).max(1e-3);
    let mut out = Vec::new();
    let mut t = start;
    while t <= end + 1e-9 && out.len() < 100_000 {
        out.push(t);
        t += step;
    }
    out
}

/// A Resample: each curve (of Group's) again with points evenly by length:
/// Count of them, or one every Length; ends kept, attributes blended
/// (whole numbers from the nearer), the frame worked out again.
pub fn resample(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let by_length = params["by"].as_str() == Some("length");
    let count = params["count"].as_u64().unwrap_or(20).max(2) as usize;
    let length = params["length"].as_f64().unwrap_or(0.1);
    if by_length && (length.is_nan() || length <= 0.0) {
        bail!("Resample: Length must be more than 0");
    }
    for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
        let mut out = Curves { world: c.world, node: c.node.clone(), ..Curves::default() };
        let mut parts: Vec<(Attrs, usize)> = Vec::new();
        let mut frames: Vec<(Vec<Vec3>, Vec<Vec3>)> = Vec::new();
        for k in 0..c.len() {
            let closed = c.closed[k];
            let first = c.starts[k] as usize;
            let mut pts: Vec<Vec3> = c.curve(k).to_vec();
            let mut index: Vec<usize> = (first..first + pts.len()).collect();
            if closed && pts.len() > 2 {
                pts.push(pts[0]);
                index.push(first);
            }
            // Distance along it at each point.
            let mut along = vec![0.0];
            for w in pts.windows(2) {
                along.push(along.last().unwrap() + (w[1] - w[0]).norm());
            }
            let total = *along.last().unwrap_or(&0.0);
            let n = if by_length { ((total / length).round() as usize).max(1) + 1 } else { count };
            // (A closed curve's last point would be its first again.)
            let n_out = if closed && pts.len() > 3 { n.max(4) - 1 } else { n };
            let mut new_pts = Vec::with_capacity(n_out);
            // Each new point: the segment it's on and how far along.
            let mut at: Vec<(usize, usize, f64)> = Vec::with_capacity(n_out);
            for j in 0..n_out {
                let d = if n > 1 { total * j as f64 / (n - 1) as f64 } else { 0.0 };
                let s = along.partition_point(|&a| a <= d).clamp(1, along.len().max(2) - 1) - 1;
                let span = (along.get(s + 1).copied().unwrap_or(along[s]) - along[s]).max(1e-300);
                let w = ((d - along[s]) / span).clamp(0.0, 1.0);
                let s2 = (s + 1).min(pts.len() - 1);
                new_pts.push(pts[s] + (pts[s2] - pts[s]) * w);
                at.push((index[s], index[s2], w));
            }
            parts.push((blended(&c.attrs, &at), new_pts.len()));
            let dirs: Vec<Vec3> = (0..new_pts.len())
                .map(|i| {
                    let m = new_pts.len();
                    let prev = if i > 0 { new_pts[i - 1] } else if closed && m > 2 { new_pts[m - 1] } else { new_pts[i] };
                    let next = if i + 1 < m { new_pts[i + 1] } else if closed && m > 2 { new_pts[0] } else { new_pts[i] };
                    next - prev
                })
                .collect();
            frames.push((new_pts.clone(), dirs));
            out.push(&new_pts, closed);
        }
        let mut attrs = Attrs::concat(&parts.iter().map(|(a, n)| (a, *n)).collect::<Vec<_>>());
        // The frame (and u) worked out again for the new points.
        let fresh: Vec<(Attrs, usize)> = frames.iter().map(|(p, d)| (frame_attrs(p, d, None), p.len())).collect();
        let fresh = Attrs::concat(&fresh.iter().map(|(a, n)| (a, *n)).collect::<Vec<_>>());
        for name in ["tangent", geo::N, geo::UP, "u"] {
            if let Some(v) = fresh.get(name) {
                attrs.set(name, v.clone(), out.positions.len())?;
            }
        }
        out.attrs = attrs;
        *c = out;
    }
    Ok(scene)
}

/// `attrs` (per point) at new points `at`, each between two old points
/// (and how far from the first): numbers and vectors blended, whole
/// numbers the nearer's.
fn blended(attrs: &Attrs, at: &[(usize, usize, f64)]) -> Attrs {
    let mut out = Attrs::default();
    let mix = |a: f32, b: f32, w: f64| (a as f64 + (b as f64 - a as f64) * w) as f32;
    for (name, a) in attrs.iter() {
        let v = match a {
            Attr::Float(v) => Attr::Float(at.iter().map(|&(i, j, w)| mix(v[i], v[j], w)).collect()),
            Attr::Vec3(v) => Attr::Vec3(at.iter().map(|&(i, j, w)| std::array::from_fn(|k| mix(v[i][k], v[j][k], w))).collect()),
            Attr::Vec4(v) => Attr::Vec4(at.iter().map(|&(i, j, w)| std::array::from_fn(|k| mix(v[i][k], v[j][k], w))).collect()),
            Attr::Int(v) => Attr::Int(at.iter().map(|&(i, j, w)| if w < 0.5 { v[i] } else { v[j] }).collect()),
        };
        let _ = out.set(name, v, at.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom;
    use serde_json::json;

    fn pts(c: &Curves) -> Vec<Vec3> {
        c.positions.clone()
    }

    #[test]
    fn curves_sample_their_control_points() {
        let p = |x: f64, y: f64| [x, y, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let square: Vec<f64> = [p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)].concat();
        let run = |extra: Value| {
            let mut params = json!({ "points": square.clone() });
            params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            curve(&params).unwrap().curves.pop().unwrap()
        };
        // A polyline: its points, exactly.
        let c = run(json!({}));
        assert_eq!(pts(&c), controls(&json!(square)).iter().map(|c| c.at).collect::<Vec<_>>());
        assert!(c.validate().is_ok());
        let u = c.attrs.float("u").unwrap();
        assert_eq!((u[0], u[3]), (0.0, 1.0));
        assert!(u.windows(2).all(|w| w[1] > w[0]));
        // Smooth: through every point, 16 a span; closed: wraps, no repeat.
        let c = run(json!({ "kind": "smooth" }));
        assert_eq!(c.positions.len(), 3 * 16 + 1);
        for (k, at) in [Vec3::zeros(), Vec3::x(), Vec3::new(1.0, 1.0, 0.0), Vec3::y()].iter().enumerate() {
            assert!((c.positions[(k * 16).min(48)] - at).norm() < 1e-12);
        }
        let c = run(json!({ "kind": "smooth", "closed": true, "detail": 8 }));
        assert_eq!((c.positions.len(), c.closed.clone()), (32, vec![true]));
        // Bezier: its handles (a quarter circle's, near enough).
        let k = 0.5523;
        let quarter = json!([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, k, 0.0, 0.0, 1.0, 0.0, k, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let c = curve(&json!({ "points": quarter, "kind": "bezier", "detail": 16 })).unwrap().curves.pop().unwrap();
        assert!(c.positions.iter().all(|p| (p.norm() - 1.0).abs() < 3e-4), "on the circle");
        // Frames: tangents unit; up square to them; N along them.
        let t = c.attrs.vec3("tangent").unwrap();
        let up = c.attrs.vec3(geo::UP).unwrap();
        let v = |a: [f32; 3]| Vec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
        assert!(t.iter().zip(up).all(|(t, u)| (v(*t).norm() - 1.0).abs() < 1e-5 && v(*t).dot(&v(*u)).abs() < 1e-5));
        assert_eq!(c.attrs.vec3(geo::N), Some(t));
        // Starting along +Y, up starts at +X (+Y can't be square to it)
        // and, the curve flat, stays in its plane, turning with it.
        assert!((v(up[0]) - Vec3::x()).norm() < 1e-5, "{:?}", up[0]);
        assert!(up.iter().all(|u| u[2].abs() < 1e-5) && up.windows(2).all(|w| v(w[0]).dot(&v(w[1])) > 0.99), "{up:?}");
        // Placed by its own transform.
        let c = curve(&json!({ "points": square, "translate": [0, 5, 0] })).unwrap().curves.pop().unwrap();
        assert_eq!(geom::transform_point(&c.world, &c.positions[1]), Vec3::new(1.0, 5.0, 0.0));
        assert!(curve(&json!({ "source": "node path" })).is_err(), "no path to follow");
    }

    #[test]
    fn a_twisting_curve_frame_turns_as_little_as_it_can() {
        // A helix: its up stays square to the tangent and turns smoothly.
        let ctrl: Vec<Control> = (0..40).map(|i| {
            let a = i as f64 * 0.3;
            Control { at: Vec3::new(a.cos(), i as f64 * 0.05, a.sin()), before: Vec3::zeros(), after: Vec3::zeros() }
        }).collect();
        let (p, d, _) = sample(&ctrl, Kind::Smooth, false, 8);
        let ts = unit_tangents(&p, &d);
        let ups = rotation_minimizing(&p, &ts);
        assert!(ups.iter().zip(&ts).all(|(u, t)| u.dot(t).abs() < 1e-9 && (u.norm() - 1.0).abs() < 1e-9));
        assert!(ups.windows(2).all(|w| w[0].dot(&w[1]) > 0.98), "no sudden flips");
    }

    #[test]
    fn resample_spaces_points_evenly() {
        let mut c = Curves { node: "road".into(), ..Curves::default() };
        c.push(&[Vec3::zeros(), Vec3::new(1.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0)], false);
        let n = c.positions.len();
        c.attrs.set("w", Attr::Float(vec![0.0, 1.0, 4.0]), n).unwrap();
        c.attrs.set("k", Attr::Int(vec![1, 2, 3]), n).unwrap();
        let scene = Scene { curves: vec![c], ..Scene::default() };
        let out = resample(&json!({ "count": 5 }), scene.clone()).unwrap();
        let r = &out.curves[0];
        assert!(r.validate().is_ok());
        assert_eq!(r.positions.iter().map(|p| p.x).collect::<Vec<_>>(), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(r.attrs.float("w"), Some(&[0.0, 1.0, 2.0, 3.0, 4.0][..]), "blended");
        assert_eq!(r.attrs.int("k"), Some(&[1, 2, 2, 3, 3][..]), "the nearer's (x 2 is nearer 1 than 4)");
        assert!(r.attrs.vec3("tangent").is_some() && r.attrs.float("u").unwrap()[4] == 1.0);
        // A point every 0.5: 9.
        assert_eq!(resample(&json!({ "by": "length", "length": 0.5 }), scene.clone()).unwrap().curves[0].positions.len(), 9);
        assert!(resample(&json!({ "by": "length", "length": 0 }), scene.clone()).is_err());
        // A closed square: evenly round it, still closed, its corner kept.
        let mut sq = Curves::default();
        sq.push(&[Vec3::zeros(), Vec3::x(), Vec3::new(1.0, 1.0, 0.0), Vec3::y()], true);
        let out = resample(&json!({ "count": 9 }), Scene { curves: vec![sq], ..Scene::default() }).unwrap();
        let r = &out.curves[0];
        assert_eq!((r.positions.len(), r.closed.clone()), (8, vec![true]));
        assert!((r.positions[2] - Vec3::x()).norm() < 1e-12 && (r.positions[1] - Vec3::new(0.5, 0.0, 0.0)).norm() < 1e-12);
    }
}
