//! Curve nodes (docs/geo-plan.md, P5): the Curve node, which makes a
//! curve from its own control points (drawn and edited in the 3D view) or
//! from a node's path over frames; Resample, Points on Curve and Curve to
//! Mesh. `ops.rs` registers them.
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

/// How many points a curve gets again: Count of them, or one every
/// Length (see [`even_points`]).
#[derive(Clone, Copy)]
pub enum Spacing {
    Count(usize),
    Length(f64),
}

impl Spacing {
    /// From a node's `by`, `count` and `length` params (`what`: the node,
    /// for the error).
    fn of(params: &Value, what: &str) -> Result<Spacing> {
        if params["by"].as_str() == Some("length") {
            let length = params["length"].as_f64().unwrap_or(0.1);
            if length.is_nan() || length <= 0.0 {
                bail!("{what}: Length must be more than 0");
            }
            Ok(Spacing::Length(length))
        } else {
            Ok(Spacing::Count(params["count"].as_u64().unwrap_or(20).max(2) as usize))
        }
    }
}

/// Curve `k` of `c` with points evenly along its length as `spacing` says
/// (ends kept; a closed curve's last not repeating its first): the points
/// and, for each, the two old points it's between and how far along.
fn even_points(c: &Curves, k: usize, spacing: Spacing) -> (Vec<Vec3>, Vec<(usize, usize, f64)>) {
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
    let n = match spacing {
        Spacing::Length(length) => ((total / length).round() as usize).max(1) + 1,
        Spacing::Count(count) => count,
    };
    // (A closed curve's last point would be its first again.)
    let n_out = if closed && pts.len() > 3 { n.max(4) - 1 } else { n };
    let mut new_pts = Vec::with_capacity(n_out);
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
    (new_pts, at)
}

/// The directions at each of `pts` (a curve, closed or not) from its
/// neighbours, for [`frame_attrs`].
fn chords(pts: &[Vec3], closed: bool) -> Vec<Vec3> {
    let m = pts.len();
    (0..m)
        .map(|i| {
            let prev = if i > 0 { pts[i - 1] } else if closed && m > 2 { pts[m - 1] } else { pts[i] };
            let next = if i + 1 < m { pts[i + 1] } else if closed && m > 2 { pts[0] } else { pts[i] };
            next - prev
        })
        .collect()
}

/// Curve `k` of `c` evenly again (see [`even_points`]): its points, and
/// their attributes (`c`'s blended, then tangent, u, N and up worked out
/// for them).
fn evenly(c: &Curves, k: usize, spacing: Spacing) -> (Vec<Vec3>, Attrs) {
    let (pts, at) = even_points(c, k, spacing);
    let mut attrs = blended(&c.attrs, &at);
    let fresh = frame_attrs(&pts, &chords(&pts, c.closed[k]), None);
    for (name, v) in fresh.iter() {
        let _ = attrs.set(name, v.clone(), pts.len());
    }
    (pts, attrs)
}

/// A Resample: each curve (of Group's) again with points evenly by length:
/// Count of them, or one every Length; ends kept, attributes blended
/// (whole numbers from the nearer), the frame worked out again.
pub fn resample(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let spacing = Spacing::of(params, "Resample")?;
    for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
        let mut out = Curves { world: c.world, node: c.node.clone(), ..Curves::default() };
        let mut parts: Vec<(Attrs, usize)> = Vec::new();
        for k in 0..c.len() {
            let (pts, attrs) = evenly(c, k, spacing);
            parts.push((attrs, pts.len()));
            out.push(&pts, c.closed[k]);
        }
        out.attrs = Attrs::concat(&parts.iter().map(|(a, n)| (a, *n)).collect::<Vec<_>>());
        *c = out;
    }
    Ok(scene)
}

/// A Points on Curve: a point set for each set of curves (of Group's), its
/// points along them (by count, spacing, or at the curves' own points),
/// each with its curve's frame (tangent, N, up), u, which curve, and the
/// curves' other attributes; the curves kept unless Keep curves is off.
pub fn points_on_curve(params: &Value, mut scene: Scene) -> Result<Scene> {
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let own = params["by"].as_str() == Some("points");
    let spacing = if own { Spacing::Count(2) } else { Spacing::of(params, "Points on Curve")? };
    let mut made = Vec::new();
    for c in scene.curves.iter().filter(|c| take(&c.node)) {
        let mut positions = Vec::new();
        let mut parts: Vec<(Attrs, usize)> = Vec::new();
        for k in 0..c.len() {
            let (pts, mut attrs) = if own {
                // Its own points, its own attributes (its frame, where it
                // hasn't one, worked out).
                let first = c.starts[k] as usize;
                let pts = c.curve(k).to_vec();
                let mut attrs = c.attrs.gathered(&(first as u32..(first + pts.len()) as u32).collect::<Vec<_>>());
                if attrs.get("tangent").is_none() {
                    for (name, v) in frame_attrs(&pts, &chords(&pts, c.closed[k]), None).iter() {
                        let _ = attrs.set(name, v.clone(), pts.len());
                    }
                }
                (pts, attrs)
            } else {
                evenly(c, k, spacing)
            };
            let _ = attrs.set("curve", Attr::Int(vec![k as i32; pts.len()]), pts.len());
            parts.push((attrs, pts.len()));
            positions.extend(pts);
        }
        let attrs = Attrs::concat(&parts.iter().map(|(a, n)| (a, *n)).collect::<Vec<_>>());
        made.push(geo::PointSet { positions, attrs, world: c.world, node: String::new() });
    }
    if !params["keep_curves"].as_bool().unwrap_or(true) {
        scene.curves.retain(|c| !take(&c.node));
    }
    scene.point_sets.extend(made);
    Ok(scene)
}

/// A profile swept along curves: its points across and up (in the curve's
/// frame), whether it's closed, and its 2D normals.
struct Profile {
    points: Vec<[f64; 2]>,
    closed: bool,
}

impl Profile {
    /// Its normals (in the profile's plane, out of a counter-clockwise
    /// one), from its neighbours.
    fn normals(&self) -> Vec<[f64; 2]> {
        let m = self.points.len();
        (0..m)
            .map(|j| {
                let prev = if j > 0 { self.points[j - 1] } else if self.closed { self.points[m - 1] } else { self.points[j] };
                let next = if j + 1 < m { self.points[j + 1] } else if self.closed { self.points[0] } else { self.points[j] };
                let t = [next[0] - prev[0], next[1] - prev[1]];
                let l = t[0].hypot(t[1]);
                if l > 1e-300 { [t[1] / l, -t[0] / l] } else { [0.0, 1.0] }
            })
            .collect()
    }
}

/// A Curve to Mesh: each set of curves (of Group's) as a mesh: a profile
/// (a circle, a ribbon, or the profile input's first curve) swept along
/// each curve in its frame (see the node's help in ops.rs).
pub fn curve_to_mesh(params: &Value, mut scene: Scene, profile_scene: Option<&Scene>) -> Result<Scene> {
    use std::f64::consts::TAU;
    let group = params["group"].as_str().unwrap_or("").trim();
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    let scale = params["scale"].as_f64().unwrap_or(1.0);
    let profile = match params["profile"].as_str() {
        Some("ribbon") => {
            let w = params["width"].as_f64().unwrap_or(0.2) / 2.0;
            Profile { points: vec![[-w, 0.0], [w, 0.0]], closed: false }
        }
        Some("curve") => {
            let c = profile_scene.and_then(|s| s.curves.iter().find(|c| !c.is_empty())).ok_or_else(|| anyhow::anyhow!("Curve to Mesh: its profile is 'curve': connect a scene with a curve to its profile input"))?;
            Profile { points: c.curve(0).iter().map(|p| [p.x, p.y]).collect(), closed: c.closed[0] }
        }
        _ => {
            let r = params["radius"].as_f64().unwrap_or(0.1);
            let sides = params["sides"].as_u64().unwrap_or(12).clamp(3, 1024) as usize;
            Profile { points: (0..sides).map(|j| { let a = TAU * j as f64 / sides as f64; [r * a.cos(), r * a.sin()] }).collect(), closed: true }
        }
    };
    if profile.points.len() < 2 {
        bail!("Curve to Mesh: its profile needs two points at least");
    }
    let caps = params["caps"].as_bool().unwrap_or(true);
    let pn = profile.normals();
    // Around the profile: a closed one's first point again at the end (a
    // seam, so the texture wraps).
    let around: Vec<usize> = if profile.closed { (0..profile.points.len()).chain([0]).collect() } else { (0..profile.points.len()).collect() };
    let ring = around.len();
    let reach = profile.points.iter().map(|p| p[0].hypot(p[1])).fold(1e-12, f64::max);
    let mut models = Vec::new();
    for c in scene.curves.iter().filter(|c| take(&c.node)) {
        let mut mesh = crate::mesh::Mesh::default();
        // Each vertex's curve point (for the attributes).
        let mut from: Vec<u32> = Vec::new();
        let pscale = c.attrs.float(geo::PSCALE);
        let us = c.attrs.float("u");
        for k in 0..c.len() {
            let first = c.starts[k] as usize;
            let pts = c.curve(k).to_vec();
            let m = pts.len();
            if m < 2 {
                continue;
            }
            let closed = c.closed[k] && m > 2;
            // The frame: the curve's own (N, up), or worked out.
            let v3 = |a: [f32; 3]| Vec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
            let (ns, ups): (Vec<Vec3>, Vec<Vec3>) = match (c.attrs.vec3(geo::N), c.attrs.vec3(geo::UP)) {
                (Some(n), Some(u)) => ((first..first + m).map(|i| v3(n[i])).collect(), (first..first + m).map(|i| v3(u[i])).collect()),
                _ => {
                    let f = frame_attrs(&pts, &chords(&pts, closed), None);
                    (f.vec3(geo::N).unwrap().iter().map(|a| v3(*a)).collect(), f.vec3(geo::UP).unwrap().iter().map(|a| v3(*a)).collect())
                }
            };
            let mut along = vec![0.0];
            for w in pts.windows(2) {
                along.push(along.last().unwrap() + (w[1] - w[0]).norm());
            }
            let total = along.last().copied().unwrap_or(0.0).max(1e-300);
            let rows = if closed { m + 1 } else { m };
            let base = mesh.positions.len() as u32;
            for row in 0..rows {
                let i = row % m;
                let (n, up) = (ns[i], ups[i]);
                let across = up.cross(&n);
                let s = scale * pscale.map_or(1.0, |p| p[first + i] as f64);
                let u = if closed && row == m { 1.0 } else { us.map_or(along[i] / total, |u| u[first + i] as f64) };
                for (j, &q) in around.iter().enumerate() {
                    let [x, y] = profile.points[q];
                    mesh.positions.push(pts[i] + (across * x + up * y) * s);
                    mesh.normals.push((across * pn[q][0] + up * pn[q][1]).try_normalize(1e-12).unwrap_or(up));
                    mesh.uvs.push([u, j as f64 / (ring - 1) as f64]);
                    from.push((first + i) as u32);
                }
            }
            for row in 0..rows - 1 {
                for j in 0..ring - 1 {
                    let a = base + (row * ring + j) as u32;
                    let (b, cc, d) = (a + 1, a + ring as u32, a + ring as u32 + 1);
                    mesh.triangles.push([a, b, cc]);
                    mesh.triangles.push([b, d, cc]);
                }
            }
            // Caps on an open tube: a fan from the middle, its own vertices.
            if caps && profile.closed && !closed {
                for (row, out) in [(0usize, -1.0), (m - 1, 1.0)] {
                    let (n, up) = (ns[row], ups[row]);
                    let across = up.cross(&n);
                    let s = scale * pscale.map_or(1.0, |p| p[first + row] as f64);
                    let center = mesh.positions.len() as u32;
                    mesh.positions.push(pts[row]);
                    mesh.normals.push(n * out);
                    mesh.uvs.push([0.5, 0.5]);
                    from.push((first + row) as u32);
                    let sides = profile.points.len();
                    for &[x, y] in &profile.points {
                        mesh.positions.push(pts[row] + (across * x + up * y) * s);
                        mesh.normals.push(n * out);
                        mesh.uvs.push([0.5 + x / (2.0 * reach), 0.5 + y / (2.0 * reach)]);
                        from.push((first + row) as u32);
                    }
                    for j in 0..sides as u32 {
                        let (r0, r1) = (center + 1 + j, center + 1 + (j + 1) % sides as u32);
                        mesh.triangles.push(if out < 0.0 { [center, r1, r0] } else { [center, r0, r1] });
                    }
                }
            }
        }
        if mesh.triangles.is_empty() {
            continue;
        }
        mesh.point_attrs = c.attrs.gathered(&from);
        let model = crate::scene::Model { node: String::new(), ..crate::scene::Model::new(std::sync::Arc::new(mesh), c.world, crate::scene::material(params)) }.flags(params);
        models.push(model);
    }
    if !params["keep_curves"].as_bool().unwrap_or(false) {
        scene.curves.retain(|c| !take(&c.node));
    }
    scene.meshes.extend(models);
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

    /// A straight curve along +X, its frame's up +Y (as a Curve gives it).
    fn line(closed: bool) -> Scene {
        let pts = if closed {
            json!([0, 0, 0, 0, 0, 0, 0, 0, 0,  4, 0, 0, 0, 0, 0, 0, 0, 0,  4, 0, 4, 0, 0, 0, 0, 0, 0,  0, 0, 4, 0, 0, 0, 0, 0, 0])
        } else {
            json!([0, 0, 0, 0, 0, 0, 0, 0, 0,  2, 0, 0, 0, 0, 0, 0, 0, 0,  4, 0, 0, 0, 0, 0, 0, 0, 0])
        };
        let mut s = curve(&json!({ "points": pts, "kind": "polyline", "closed": closed })).unwrap();
        s.curves[0].node = "c".into();
        s
    }

    #[test]
    fn curves_become_tubes_and_ribbons() {
        let mesh = |p: Value, s: Scene| curve_to_mesh(&p, s, None).unwrap().meshes.remove(0).mesh;
        // An open tube, 8 sides: 3 rings of 9 (a seam), caps of 1 + 8 each.
        let m = mesh(json!({ "radius": 0.5, "sides": 8 }), line(false));
        assert_eq!(m.positions.len(), 3 * 9 + 2 * 9);
        assert_eq!(m.triangles.len(), 2 * 8 * 2 + 2 * 8);
        // The rings stand square to +X, 0.5 out; the profile's +x is
        // across (up x N: +Y x +X = -Z), its +y up.
        assert!(m.positions[..27].iter().all(|p| ((p.y * p.y + p.z * p.z).sqrt() - 0.5).abs() < 1e-9));
        assert!((m.positions[0] - Vec3::new(0.0, 0.0, -0.5)).norm() < 1e-9 && (m.positions[2] - Vec3::new(0.0, 0.5, 0.0)).norm() < 1e-9);
        // Faces out: each triangle's normal away from the axis.
        for t in &m.triangles[..32] {
            let [a, b, c] = t.map(|i| m.positions[i as usize]);
            let n = (b - a).cross(&(c - a));
            let mid = (a + b + c) / 3.0;
            assert!(n.dot(&Vec3::new(0.0, mid.y, mid.z)) > 0.0, "outward");
        }
        // uv: u along 0..1, v around 0..1 with the seam.
        assert_eq!((m.uvs[0], m.uvs[8], m.uvs[26]), ([0.0, 0.0], [0.0, 1.0], [1.0, 1.0]));
        // The caps face out of the ends.
        let ends: Vec<Vec3> = m.normals[27..].to_vec();
        assert!(ends[..9].iter().all(|n| (n.x + 1.0).abs() < 1e-9) && ends[9..].iter().all(|n| (n.x - 1.0).abs() < 1e-9));
        assert!(m.point_attrs.check(m.positions.len()).is_ok() && m.point_attrs.float("u").is_some(), "the curve's attributes on the rings");
        // Closed: no caps, the last ring joined to the first (5 rings).
        let m = mesh(json!({ "radius": 0.5, "sides": 8 }), line(true));
        assert_eq!((m.positions.len(), m.triangles.len()), (5 * 9, 4 * 8 * 2));
        // pscale tapers it; a ribbon is two wide, flat, facing up.
        let mut tapered = line(false);
        tapered.curves[0].attrs.set(geo::PSCALE, Attr::Float(vec![1.0, 0.5, 0.0]), 3).unwrap();
        let m = mesh(json!({ "radius": 1.0, "sides": 4, "caps": false }), tapered);
        assert!(m.positions[5..10].iter().all(|p| ((p.y * p.y + p.z * p.z).sqrt() - 0.5).abs() < 1e-9));
        let m = mesh(json!({ "profile": "ribbon", "width": 2.0 }), line(false));
        assert_eq!((m.positions.len(), m.triangles.len()), (6, 4));
        assert!(m.positions.iter().all(|p| p.y.abs() < 1e-9 && (p.z.abs() - 1.0).abs() < 1e-9) && m.normals.iter().all(|n| (n.y.abs() - 1.0).abs() < 1e-9));
        // A profile curve: a triangle's three corners each ring.
        let mut tri = Curves::default();
        tri.push(&[Vec3::new(0.0, 1.0, 0.0), Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0)], true);
        let profile = Scene { curves: vec![tri], ..Scene::default() };
        let m = curve_to_mesh(&json!({ "profile": "curve", "caps": false }), line(false), Some(&profile)).unwrap().meshes.remove(0).mesh;
        assert_eq!(m.positions.len(), 3 * 4);
        assert!(curve_to_mesh(&json!({ "profile": "curve" }), line(false), None).is_err());
        assert!(curve_to_mesh(&json!({ "keep_curves": true }), line(false), None).unwrap().curves.len() == 1);
    }

    #[test]
    fn points_go_along_curves_with_their_frame() {
        let run = |p: Value| points_on_curve(&p, line(false)).unwrap();
        let out = run(json!({ "count": 5 }));
        assert_eq!(out.curves.len(), 1, "kept");
        let ps = &out.point_sets[0];
        assert!(ps.validate().is_ok());
        assert_eq!(ps.positions.iter().map(|p| p.x).collect::<Vec<_>>(), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert!(ps.attrs.vec3(geo::N).unwrap().iter().all(|n| (n[0] - 1.0).abs() < 1e-6));
        assert!(ps.attrs.vec3(geo::UP).unwrap().iter().all(|u| (u[1] - 1.0).abs() < 1e-6));
        assert_eq!(ps.attrs.int("curve"), Some(&[0; 5][..]));
        assert_eq!(ps.attrs.float("u").unwrap()[4], 1.0);
        assert_eq!(run(json!({ "by": "length", "length": 0.5 })).point_sets[0].len(), 9);
        let own = run(json!({ "by": "points", "keep_curves": false }));
        assert!(own.curves.is_empty() && own.point_sets[0].len() == 3 && own.point_sets[0].attrs.vec3("tangent").is_some());
        // Instance on Points then aims copies along the curve, upright.
        let card = crate::scene::Card::new(std::sync::Arc::new(crate::image::Image::new(2, 2)), crate::geom::Mat4::identity(), [1.0, 1.0]);
        let protos = Scene { cards: vec![card], ..Scene::default() };
        let copies = crate::ops::geo_ops::instance_on_points(&json!({}), out, Some(&protos), None).unwrap();
        let w = copies.instances[0].worlds[2];
        let dir = |v: Vec3| (w.fixed_view::<3, 3>(0, 0) * v).normalize();
        assert!((dir(Vec3::z()) - Vec3::x()).norm() < 1e-6 && (dir(Vec3::y()) - Vec3::y()).norm() < 1e-6);
    }
}
