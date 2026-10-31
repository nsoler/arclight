//! Attribute nodes (docs/geo-plan.md, P3) besides Attribute Set: each
//! writes one attribute on the elements of a scene's geometry (point sets'
//! and curves' points, mesh points, or instances) and passes the rest
//! through. `ops.rs` registers them.
//!
//! - Attribute Noise: 3-D noise (the formulas' `noise3`), set, added or
//!   multiplied in, optionally only in a region with a soft edge.
//! - Falloff: a 0–1 weight by distance to curves, points (a locator, a
//!   tracker's) or a region, as MoGraph's effectors.
//! - Attribute from Image: an image sampled at each element, where a
//!   camera sees it or at its uv.
//! - Attribute Transfer: attributes from the nearest elements of another
//!   input (within a radius, with a falloff), through a k-d tree.

use crate::geo::{self, Attr, Attrs};
use crate::geom::{self, Mat4, Vec3};
use crate::scene::{self, Region, Scene};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::sync::Arc;

/// The elements a node writes to, by its Elements param.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Elements {
    /// Point sets' and curves' points.
    Points,
    MeshPoints,
    Instances,
}

impl Elements {
    fn from(params: &Value) -> Elements {
        match params["elements"].as_str() {
            Some("mesh points") => Elements::MeshPoints,
            Some("instances") => Elements::Instances,
            _ => Elements::Points,
        }
    }
}

/// One object's elements: their places in the scene, their texture
/// coordinates if they have them, and its attributes to change.
struct Target<'a> {
    placed: Vec<Vec3>,
    uvs: Option<Vec<[f64; 2]>>,
    attrs: &'a mut Attrs,
}

/// The `uv` attribute as texture coordinates (Scatter's, a vec3).
fn uv_attr(attrs: &Attrs) -> Option<Vec<[f64; 2]>> {
    attrs.vec3("uv").map(|v| v.iter().map(|u| [u[0] as f64, u[1] as f64]).collect())
}



/// Call `f` with each object of `scene` (of `group`'s nodes, a name or a
/// pattern, or all) that has `elements`.
fn each_target(scene: &mut Scene, elements: Elements, group: &str, mut f: impl FnMut(Target) -> Result<()>) -> Result<()> {
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    match elements {
        Elements::Points => {
            for g in scene.point_sets.iter_mut().filter(|g| take(&g.node)) {
                let placed = (0..g.len()).map(|i| g.placed(i)).collect();
                f(Target { placed, uvs: uv_attr(&g.attrs), attrs: &mut g.attrs })?;
            }
            for c in scene.curves.iter_mut().filter(|c| take(&c.node)) {
                let placed = c.positions.iter().map(|p| geom::transform_point(&c.world, p)).collect();
                f(Target { placed, uvs: uv_attr(&c.attrs), attrs: &mut c.attrs })?;
            }
        }
        Elements::MeshPoints => {
            for m in scene.meshes.iter_mut().filter(|m| take(&m.node)) {
                let placed = m.mesh.positions.iter().map(|p| geom::transform_point(&m.world, p)).collect();
                let uvs = (!m.mesh.uvs.is_empty()).then(|| m.mesh.uvs.clone());
                f(Target { placed, uvs, attrs: &mut Arc::make_mut(&mut m.mesh).point_attrs })?;
            }
        }
        Elements::Instances => {
            for g in scene.instances.iter_mut().filter(|g| take(&g.node)) {
                let placed = g.worlds.iter().map(|w| Vec3::new(w[(0, 3)], w[(1, 3)], w[(2, 3)])).collect();
                f(Target { placed, uvs: uv_attr(&g.attrs), attrs: &mut g.attrs })?;
            }
        }
    }
    Ok(())
}

/// How many components an attribute kind has.
fn width(kind: &str) -> usize {
    match kind {
        "vec3" => 3,
        "vec4" => 4,
        _ => 1,
    }
}

/// `values` (`n` rows of `w` components) as an attribute of `kind`.
fn attr_of(kind: &str, w: usize, values: &[f64]) -> Attr {
    let row = |i: usize| &values[i * w..(i + 1) * w];
    let n = values.len() / w.max(1);
    match kind {
        "vec3" => Attr::Vec3((0..n).map(|i| std::array::from_fn(|k| row(i)[k] as f32)).collect()),
        "vec4" => Attr::Vec4((0..n).map(|i| std::array::from_fn(|k| row(i)[k] as f32)).collect()),
        "int" => Attr::Int(values.iter().map(|v| v.round() as i32).collect()),
        _ => Attr::Float(values.iter().map(|v| *v as f32).collect()),
    }
}

/// Element `i`'s value of `attr` as `w` components (a scalar fills them
/// all; missing components are `fill`).
fn components(attr: &Attr, i: usize, w: usize, fill: f64) -> Vec<f64> {
    let all: Vec<f64> = match attr {
        Attr::Float(v) => vec![v[i] as f64; w],
        Attr::Int(v) => vec![v[i] as f64; w],
        Attr::Vec3(v) => v[i].iter().map(|x| *x as f64).collect(),
        Attr::Vec4(v) => v[i].iter().map(|x| *x as f64).collect(),
    };
    (0..w).map(|k| all.get(k).copied().unwrap_or(fill)).collect()
}

/// A box or ball a node acts in, with a soft edge.
pub struct Falloff {
    /// Scene space into the region's, where it's the unit cube or ball.
    to_region: Mat4,
    sphere: bool,
    /// The share of the region (from its edge in) over which the weight
    /// rises from 0 to 1.
    softness: f64,
}

impl Falloff {
    /// The region `params` describe (Falloff none, sphere or box; its
    /// region_center, region_size (whole, not half) and region_rotate;
    /// softness 0–1), and it as a Region for the 3D view to show. None for
    /// none.
    fn from(params: &Value) -> Option<(Falloff, Region)> {
        let sphere = match params["falloff"].as_str() {
            Some("sphere") => true,
            Some("box") => false,
            _ => return None,
        };
        let half = scene::numbers::<3>(&params["region_size"], 2.0).map(|v| (v.abs() / 2.0).max(1e-12));
        let world = geom::trs(scene::numbers(&params["region_center"], 0.0), scene::numbers(&params["region_rotate"], 0.0), half);
        let softness = params["softness"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        Some((Falloff { to_region: world.try_inverse().unwrap_or_else(Mat4::identity), sphere, softness }, Region { node: String::new(), world, sphere }))
    }

    /// The weight at `p`: 1 well inside, 0 outside, smooth across the
    /// soft edge.
    fn weight(&self, p: &Vec3) -> f64 {
        let q = geom::transform_point(&self.to_region, p);
        // 0 at the middle, 1 at the edge.
        let d = if self.sphere { q.norm() } else { q.abs().max() };
        if d >= 1.0 {
            return 0.0;
        }
        if self.softness <= 0.0 {
            return 1.0;
        }
        smooth(((1.0 - d) / self.softness).clamp(0.0, 1.0))
    }
}

/// Smoothstep from 0 to 1.
fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Fractal noise at `p`: `octaves` of noise3, each twice the frequency
/// and half the weight of the last, in -1..1.
fn fbm(p: Vec3, octaves: usize, seed: f64) -> f64 {
    let (mut sum, mut weight, mut total, mut f) = (0.0, 1.0, 0.0, 1.0);
    for _ in 0..octaves.max(1) {
        sum += weight * crate::expr::noise3([p.x * f, p.y * f, p.z * f], seed);
        total += weight;
        weight *= 0.5;
        f *= 2.0;
    }
    sum / total
}

/// An Attribute Noise: on each element, Center + Amplitude × noise (noise
/// in -1..1 at its place × Frequency + Offset; per component for vectors,
/// each its own), set, added to or multiplied into the attribute; in a
/// region only, if Falloff is on.
pub fn attribute_noise(params: &Value, mut scene: Scene) -> Result<Scene> {
    let name = params["name"].as_str().unwrap_or("pscale").trim();
    if name.is_empty() || name == "P" {
        bail!("Attribute Noise: give it an attribute to write (not P: move points with Attribute Set or Transform Geo)");
    }
    let kind = params["kind"].as_str().unwrap_or("float");
    let w = width(kind);
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let (freq, amp, center) = (num("frequency", 1.0), num("amplitude", 0.5), num("center", 1.0));
    let offset = Vec3::from(scene::numbers::<3>(&params["offset"], 0.0));
    let octaves = params["octaves"].as_u64().unwrap_or(1).clamp(1, 8) as usize;
    let seed = params["seed"].as_f64().unwrap_or(0.0);
    let mode = params["mode"].as_str().unwrap_or("set");
    let falloff = Falloff::from(params);
    let group = params["group"].as_str().unwrap_or("").trim();
    each_target(&mut scene, Elements::from(params), group, |t| {
        let n = t.placed.len();
        let old = t.attrs.get(name);
        if let Some(a) = old.filter(|_| mode != "set") {
            if width(a.kind()) != w {
                bail!("Attribute Noise: {name} is a {} here, not a {kind}", a.kind());
            }
        }
        // What's there now (0, or 1 to multiply, where it isn't).
        let fill = if mode == "multiply" { 1.0 } else { 0.0 };
        let values: Vec<f64> = (0..n)
            .into_par_iter()
            .flat_map_iter(|i| {
                let p = t.placed[i] * freq + offset;
                let before = old.map_or_else(|| vec![fill; w], |a| components(a, i, w, fill));
                let k = falloff.as_ref().map_or(1.0, |(f, _)| f.weight(&t.placed[i]));
                (0..w).map(move |c| {
                    let v = center + amp * fbm(p, octaves, seed + 101.0 * c as f64);
                    let new = match mode {
                        "add" => before[c] + v,
                        "multiply" => before[c] * v,
                        _ => v,
                    };
                    before[c] + (new - before[c]) * k
                })
            })
            .collect();
        t.attrs.set(name, attr_of(kind, w, &values), n)
    })?;
    if let Some((_, region)) = falloff {
        scene.regions.push(region);
    }
    Ok(scene)
}

/// Points to find the nearest of quickly: a k-d tree, split at the
/// median along x, y, z in turn (built in O(n log² n), queried in about
/// O(log n)).
pub struct KdTree {
    points: Vec<Vec3>,
    /// `points`' indices, each subtree a range with its median in the
    /// middle.
    order: Vec<usize>,
}

impl KdTree {
    pub fn new(points: Vec<Vec3>) -> KdTree {
        let mut order: Vec<usize> = (0..points.len()).collect();
        fn build(points: &[Vec3], order: &mut [usize], axis: usize) {
            if order.len() <= 1 {
                return;
            }
            let mid = order.len() / 2;
            order.select_nth_unstable_by(mid, |&a, &b| points[a][axis].total_cmp(&points[b][axis]));
            let (lo, rest) = order.split_at_mut(mid);
            build(points, lo, (axis + 1) % 3);
            build(points, &mut rest[1..], (axis + 1) % 3);
        }
        build(&points, &mut order, 0);
        KdTree { points, order }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The point nearest `p`: its index and distance (None if empty).
    pub fn nearest(&self, p: &Vec3) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        let mut reach = f64::INFINITY;
        self.walk(0, self.order.len(), 0, p, &mut |i, d2, best_d2: &mut f64| {
            if d2 < *best_d2 {
                *best_d2 = d2;
                best = Some((i, d2));
            }
        }, &mut reach);
        best.map(|(i, d2)| (i, d2.sqrt()))
    }

    /// Every point within `r` of `p`: index and distance.
    pub fn within(&self, p: &Vec3, r: f64) -> Vec<(usize, f64)> {
        let mut out = Vec::new();
        let mut reach = r * r;
        self.walk(0, self.order.len(), 0, p, &mut |i, d2, _: &mut f64| {
            if d2 <= r * r {
                out.push((i, d2.sqrt()));
            }
        }, &mut reach);
        out
    }

    /// Visit the points of `order[lo..hi]` (split on `axis`) that could be
    /// within `reach` (squared, which `visit` may shrink) of `p`.
    fn walk(&self, lo: usize, hi: usize, axis: usize, p: &Vec3, visit: &mut impl FnMut(usize, f64, &mut f64), reach: &mut f64) {
        if lo >= hi {
            return;
        }
        let mid = lo + (hi - lo) / 2;
        let i = self.order[mid];
        let q = &self.points[i];
        visit(i, (q - p).norm_squared(), reach);
        let d = p[axis] - q[axis];
        let (near, far) = if d < 0.0 { ((lo, mid), (mid + 1, hi)) } else { ((mid + 1, hi), (lo, mid)) };
        self.walk(near.0, near.1, (axis + 1) % 3, p, visit, reach);
        if d * d <= *reach {
            self.walk(far.0, far.1, (axis + 1) % 3, p, visit, reach);
        }
    }
}

/// The distance from `p` to the segment `a`–`b`.
fn to_segment(p: &Vec3, a: &Vec3, b: &Vec3) -> f64 {
    let ab = b - a;
    let t = if ab.norm_squared() > 0.0 { ((p - a).dot(&ab) / ab.norm_squared()).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).norm()
}

/// What a Falloff measures the distance to.
enum From {
    /// Polyline segments (curves, placed).
    Segments(Vec<(Vec3, Vec3)>),
    Points(KdTree),
    /// A box or ball: scene space into the region's (the unit cube or
    /// ball), and its half size (the scale back).
    Region { to_region: Mat4, half: Vec3, sphere: bool },
}

impl From {
    /// The distance from `p` (0 inside a region).
    fn distance(&self, p: &Vec3) -> f64 {
        match self {
            From::Segments(segs) => segs.iter().map(|(a, b)| to_segment(p, a, b)).fold(f64::INFINITY, f64::min),
            From::Points(tree) => tree.nearest(p).map_or(f64::INFINITY, |(_, d)| d),
            From::Region { to_region, half, sphere } => {
                let q = geom::transform_point(to_region, p);
                if *sphere {
                    // Past the surface, in the region's mean radius.
                    (q.norm() - 1.0).max(0.0) * (half.x + half.y + half.z) / 3.0
                } else {
                    Vec3::new((q.x.abs() - 1.0).max(0.0) * half.x, (q.y.abs() - 1.0).max(0.0) * half.y, (q.z.abs() - 1.0).max(0.0) * half.z).norm()
                }
            }
        }
    }
}

/// A Falloff's weight curve from `t` (0 at Inner, 1 at Outer) to 1..0.
fn falloff_curve(shape: &str, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - match shape {
        "smooth" => smooth(t),
        "ease in" => t * t,
        "ease out" => 1.0 - (1.0 - t) * (1.0 - t),
        _ => t,
    }
}

/// A Falloff: on each element a 0–1 weight attribute (Name), 1 within
/// Inner of what it measures from and 0 past Outer, shaped between: from
/// the second input's curves, else its points (locators, tracker points,
/// point sets), else (nothing connected) the node's own region. Combined
/// with what's there by Combine; Invert turns it over.
pub fn falloff(params: &Value, mut scene: Scene, from: Option<&Scene>) -> Result<Scene> {
    let name = params["name"].as_str().unwrap_or("weight").trim();
    if name.is_empty() || name == "P" {
        bail!("Falloff: give it an attribute to write (not P)");
    }
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let (inner, outer) = (num("inner", 0.0).max(0.0), num("outer", 1.0).max(0.0));
    let shape = params["shape"].as_str().unwrap_or("smooth");
    let invert = params["invert"].as_bool().unwrap_or(false);
    let combine = params["combine"].as_str().unwrap_or("set");
    let source = match from {
        Some(s) if !s.curves.is_empty() => From::Segments(s.curves.iter().flat_map(|c| c.segments().into_iter().map(|(a, b)| (geom::transform_point(&c.world, &a), geom::transform_point(&c.world, &b)))).collect()),
        Some(s) => {
            let pts: Vec<Vec3> = s.points.iter().copied().chain(s.point_sets.iter().flat_map(|g| (0..g.len()).map(move |i| g.placed(i)))).collect();
            if pts.is_empty() {
                bail!("Falloff: the scene on its from input has no curves or points to measure from");
            }
            From::Points(KdTree::new(pts))
        }
        None => {
            let half = Vec3::from(scene::numbers::<3>(&params["region_size"], 2.0).map(|v| (v.abs() / 2.0).max(1e-12)));
            let sphere = params["region"].as_str() != Some("box");
            let world = geom::trs(scene::numbers(&params["region_center"], 0.0), scene::numbers(&params["region_rotate"], 0.0), half.into());
            scene.regions.push(Region { node: String::new(), world, sphere });
            From::Region { to_region: world.try_inverse().unwrap_or_else(Mat4::identity), half, sphere }
        }
    };
    let group = params["group"].as_str().unwrap_or("").trim();
    each_target(&mut scene, Elements::from(params), group, |t| {
        let n = t.placed.len();
        let old = t.attrs.float(name).map(<[f32]>::to_vec);
        let values: Vec<f64> = (0..n)
            .into_par_iter()
            .map(|i| {
                let d = source.distance(&t.placed[i]);
                let t01 = if outer > inner { (d - inner) / (outer - inner) } else if d <= inner { 0.0 } else { 1.0 };
                let w = falloff_curve(shape, t01);
                let w = if invert { 1.0 - w } else { w };
                match (combine, old.as_ref().map(|o| o[i] as f64)) {
                    ("multiply", Some(o)) => o * w,
                    ("max", Some(o)) => o.max(w),
                    ("min", Some(o)) => o.min(w),
                    _ => w,
                }
            })
            .collect();
        t.attrs.set(name, attr_of("float", 1, &values), n)
    })?;
    Ok(scene)
}

/// `img` at frame position `x`, `y` (pixels, y down), blended from the
/// four nearest pixel centers; transparent off its data window.
pub fn sample(img: &crate::image::Image, x: f64, y: f64) -> [f64; 4] {
    let (fx, fy) = (x - 0.5 - img.x0 as f64, y - 0.5 - img.y0 as f64);
    let (ix, iy) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - ix, fy - iy);
    let at = |cx: f64, cy: f64| -> [f64; 4] {
        if cx < 0.0 || cy < 0.0 || cx >= img.width as f64 || cy >= img.height as f64 {
            return [0.0; 4];
        }
        img.pixels[cy as usize * img.width + cx as usize].map(f64::from)
    };
    let (a, b, c, d) = (at(ix, iy), at(ix + 1.0, iy), at(ix, iy + 1.0), at(ix + 1.0, iy + 1.0));
    std::array::from_fn(|k| (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty)
}

/// An Attribute from Image: on each element `image` sampled where the
/// camera input's camera sees it (Via camera) or at its texture
/// coordinates (Via uv), as RGBA, RGB, luminance or alpha. Elements off
/// the image, behind the camera or without uvs keep their value or take
/// Fill (Outside).
pub fn attribute_from_image(params: &Value, mut scene: Scene, image: Option<&crate::image::Image>, camera: Option<&Scene>) -> Result<Scene> {
    let image = image.ok_or_else(|| anyhow::anyhow!("Attribute from Image: connect an image to its image input"))?;
    let name = params["name"].as_str().unwrap_or("Cd").trim();
    if name.is_empty() || name == "P" {
        bail!("Attribute from Image: give it an attribute to write (not P)");
    }
    let take = params["take"].as_str().unwrap_or("rgba");
    let (kind, w) = match take {
        "rgb" => ("vec3", 3),
        "luminance" | "alpha" => ("float", 1),
        _ => ("vec4", 4),
    };
    let via_camera = params["via"].as_str() == Some("camera");
    let cam = match camera.and_then(|s| s.cameras.first()) {
        Some(c) => Some(c.clone()),
        None if via_camera => bail!("Attribute from Image: Via is camera, but nothing with a camera is connected to its camera input"),
        None => None,
    };
    let keep = params["outside"].as_str() != Some("fill");
    let fill = params["fill"].as_f64().unwrap_or(0.0);
    let f = image.format;
    let group = params["group"].as_str().unwrap_or("").trim();
    each_target(&mut scene, Elements::from(params), group, |t| {
        let n = t.placed.len();
        let old = t.attrs.get(name).filter(|a| width(a.kind()) == w);
        let at = |i: usize| -> Option<[f64; 2]> {
            let q = match &cam {
                Some(c) if via_camera => c.project(&t.placed[i], f)?.0,
                _ => {
                    let [u, v] = t.uvs.as_ref()?[i];
                    [u * f.width as f64, (1.0 - v) * f.height as f64]
                }
            };
            // In the frame, its edge pixels reach to its edge (as a
            // texture clamps), so a uv of 0 or 1 isn't half transparent.
            let inside = q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= f.width as f64 && q[1] <= f.height as f64;
            inside.then(|| [q[0].clamp(0.5, f.width as f64 - 0.5), q[1].clamp(0.5, f.height as f64 - 0.5)])
        };
        let values: Vec<f64> = (0..n)
            .into_par_iter()
            .flat_map_iter(|i| {
                let v: Vec<f64> = match at(i) {
                    Some([x, y]) => {
                        let p = sample(image, x, y);
                        match take {
                            "rgb" => p[..3].to_vec(),
                            "luminance" => vec![0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]],
                            "alpha" => vec![p[3]],
                            _ => p.to_vec(),
                        }
                    }
                    None => match old.filter(|_| keep) {
                        Some(a) => components(a, i, w, fill),
                        None => vec![fill; w],
                    },
                };
                v.into_iter()
            })
            .collect();
        t.attrs.set(name, attr_of(kind, w, &values), n)
    })?;
    Ok(scene)
}

/// The elements of `scene` (`elements`, of objects in `group`), read only:
/// each object's placed positions and attributes.
fn sources<'a>(scene: &'a Scene, elements: Elements, group: &str) -> Vec<(Vec<Vec3>, &'a Attrs)> {
    let take = |node: &str| group.is_empty() || geo::glob(group, node);
    match elements {
        Elements::Points => scene
            .point_sets
            .iter()
            .filter(|g| take(&g.node))
            .map(|g| ((0..g.len()).map(|i| g.placed(i)).collect(), &g.attrs))
            .chain(scene.curves.iter().filter(|c| take(&c.node)).map(|c| (c.positions.iter().map(|p| geom::transform_point(&c.world, p)).collect(), &c.attrs)))
            .collect(),
        Elements::MeshPoints => scene.meshes.iter().filter(|m| take(&m.node)).map(|m| (m.mesh.positions.iter().map(|p| geom::transform_point(&m.world, p)).collect(), &m.mesh.point_attrs)).collect(),
        Elements::Instances => scene.instances.iter().filter(|g| take(&g.node)).map(|g| (g.worlds.iter().map(|w| Vec3::new(w[(0, 3)], w[(1, 3)], w[(2, 3)])).collect(), &g.attrs)).collect(),
    }
}

/// One attribute gathered from every source element: its kind and each
/// element's components (None where its object hasn't it).
struct Gathered {
    name: String,
    kind: &'static str,
    values: Vec<Option<Vec<f64>>>,
}

/// An Attribute Transfer: Attributes ("*" for all) of the Source
/// elements of the second input copied onto the elements of the first,
/// from the nearest (Mode nearest) or the inverse-distance mean of those
/// within Radius (average); faded by Falloff across the radius toward
/// what was there. Elements with nothing within Radius (0: no limit) keep
/// their value, or get 0 (Missing zero).
pub fn attribute_transfer(params: &Value, mut scene: Scene, source: Option<&Scene>) -> Result<Scene> {
    let source = source.ok_or_else(|| anyhow::anyhow!("Attribute Transfer: connect the scene to take attributes from to its source input"))?;
    let src_elements = match params["source_elements"].as_str() {
        Some("mesh points") => Elements::MeshPoints,
        Some("instances") => Elements::Instances,
        _ => Elements::Points,
    };
    let objects = sources(source, src_elements, params["source_group"].as_str().unwrap_or("").trim());
    let wanted = params["attributes"].as_str().unwrap_or("*").trim();
    let mut names: Vec<(String, &'static str)> = Vec::new();
    for (_, attrs) in &objects {
        for (n, a) in attrs.iter() {
            let asked = wanted == "*" || wanted.split([' ', ',']).any(|w| w == n);
            if asked && n != "P" && !names.iter().any(|(m, _)| m == n) {
                names.push((n.to_string(), a.kind()));
            }
        }
    }
    if names.is_empty() {
        bail!("Attribute Transfer: the source has no attributes {} to take", if wanted == "*" { String::new() } else { format!("named {wanted}") });
    }
    let mut positions = Vec::new();
    let mut gathered: Vec<Gathered> = names.iter().map(|(n, k)| Gathered { name: n.clone(), kind: k, values: Vec::new() }).collect();
    for (placed, attrs) in &objects {
        for g in &mut gathered {
            let attr = attrs.get(&g.name).filter(|a| a.kind() == g.kind);
            g.values.extend((0..placed.len()).map(|i| attr.map(|a| components(a, i, width(g.kind), 0.0))));
        }
        positions.extend(placed.iter().copied());
    }
    let tree = KdTree::new(positions);
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let radius = num("radius", 0.0).max(0.0);
    let average = params["mode"].as_str() == Some("average");
    if average && radius <= 0.0 {
        bail!("Attribute Transfer: Mode average needs a Radius above 0");
    }
    let falloff = params["falloff"].as_str().unwrap_or("none");
    let zero = params["missing"].as_str() == Some("zero");
    // How much of the source a neighbor `d` away gives.
    let fade = |d: f64| -> f64 {
        if radius <= 0.0 {
            return 1.0;
        }
        let t = (1.0 - d / radius).clamp(0.0, 1.0);
        match falloff {
            "linear" => t,
            "smooth" => smooth(t),
            _ => 1.0,
        }
    };
    let group = params["group"].as_str().unwrap_or("").trim();
    each_target(&mut scene, Elements::from(params), group, |t| {
        let n = t.placed.len();
        // Each element's neighbors: (source index, distance); empty: none.
        let near: Vec<Vec<(usize, f64)>> = t
            .placed
            .par_iter()
            .map(|p| {
                if average {
                    tree.within(p, radius)
                } else {
                    tree.nearest(p).filter(|(_, d)| radius <= 0.0 || *d <= radius).map(|(j, d)| vec![(j, d)]).unwrap_or_default()
                }
            })
            .collect();
        for g in &gathered {
            let w = width(g.kind);
            let old = t.attrs.get(&g.name).filter(|a| width(a.kind()) == w);
            let values: Vec<f64> = (0..n)
                .flat_map(|i| {
                    let before = match old {
                        Some(a) if !zero => components(a, i, w, 0.0),
                        _ => vec![0.0; w],
                    };
                    let have: Vec<(&Vec<f64>, f64, f64)> = near[i]
                        .iter()
                        .filter_map(|&(j, d)| g.values[j].as_ref().map(|v| (v, if average { 1.0 / (d + 1e-9) } else { 1.0 }, d)))
                        .collect();
                    if have.is_empty() {
                        return before;
                    }
                    // Ints come from the nearest only.
                    let have = if g.kind == "int" { vec![*have.iter().min_by(|a, b| a.2.total_cmp(&b.2)).unwrap()] } else { have };
                    let total: f64 = have.iter().map(|h| h.1).sum();
                    let nearest = have.iter().map(|h| h.2).fold(f64::INFINITY, f64::min);
                    let k = fade(nearest);
                    (0..w).map(|c| {
                        let v = have.iter().map(|h| h.0[c] * h.1).sum::<f64>() / total;
                        before[c] + (v - before[c]) * k
                    }).collect()
                })
                .collect();
            t.attrs.set(&g.name, attr_of(g.kind, w, &values), n)?;
        }
        Ok(())
    })?;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::PointSet;
    use serde_json::json;

    fn grid() -> PointSet {
        PointSet { positions: (0..100).map(|i| Vec3::new((i % 10) as f64 * 0.37, (i / 10) as f64 * 0.29, 0.5)).collect(), node: "pts".into(), ..PointSet::default() }
    }

    fn floats(s: &Scene, name: &str) -> Vec<f32> {
        s.point_sets[0].attrs.float(name).expect(name).to_vec()
    }

    #[test]
    fn noise_is_center_plus_amplitude_times_the_formulas_noise3() {
        let scene = Scene { point_sets: vec![grid()], ..Scene::default() };
        let out = attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 4}), scene.clone()).unwrap();
        let v = floats(&out, "pscale");
        for (i, p) in grid().positions.iter().enumerate() {
            let q = p * 2.0 + Vec3::new(0.1, 0.0, 0.0);
            let want = 1.0 + 0.5 * crate::expr::noise3([q.x, q.y, q.z], 4.0);
            assert!((v[i] as f64 - want).abs() < 1e-6, "{i}: {} vs {want}", v[i]);
        }
        assert!(v.iter().all(|x| (0.5..=1.5).contains(x)) && v.iter().any(|x| *x > 1.05) && v.iter().any(|x| *x < 0.95), "{v:?}");
        // The same again; another seed differs.
        assert_eq!(floats(&attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 4}), scene.clone()).unwrap(), "pscale"), v);
        assert_ne!(floats(&attribute_noise(&json!({"name": "pscale", "center": 1.0, "amplitude": 0.5, "frequency": 2.0, "offset": [0.1, 0.0, 0.0], "seed": 5}), scene).unwrap(), "pscale"), v);
    }

    /// Octaves add finer detail and stay in range.
    #[test]
    fn octaves_add_detail_within_the_range() {
        let scene = Scene { point_sets: vec![grid()], ..Scene::default() };
        let one = floats(&attribute_noise(&json!({"name": "k", "center": 0.0, "amplitude": 1.0, "frequency": 3.0}), scene.clone()).unwrap(), "k");
        let four = floats(&attribute_noise(&json!({"name": "k", "center": 0.0, "amplitude": 1.0, "frequency": 3.0, "octaves": 4}), scene).unwrap(), "k");
        assert!(one.iter().zip(&four).any(|(a, b)| (a - b).abs() > 0.01), "different");
        assert!(four.iter().all(|v| v.abs() <= 1.0));
        // Its first octave weighs most: still near the one-octave noise.
        let diff: f32 = one.iter().zip(&four).map(|(a, b)| (a - b).abs()).sum::<f32>() / 100.0;
        assert!(diff < 0.5, "{diff}");
    }

    #[test]
    fn modes_add_and_multiply_and_vectors_vary_per_component() {
        let mut g = grid();
        g.attrs.set("w", Attr::Float(vec![2.0; 100]), 100).unwrap();
        let scene = Scene { point_sets: vec![g], ..Scene::default() };
        let set = floats(&attribute_noise(&json!({"name": "w"}), scene.clone()).unwrap(), "w");
        let add = floats(&attribute_noise(&json!({"name": "w", "mode": "add"}), scene.clone()).unwrap(), "w");
        let mul = floats(&attribute_noise(&json!({"name": "w", "mode": "multiply"}), scene.clone()).unwrap(), "w");
        for i in 0..100 {
            assert!((add[i] - (2.0 + set[i])).abs() < 1e-6 && (mul[i] - 2.0 * set[i]).abs() < 1e-6);
        }
        let out = attribute_noise(&json!({"name": "Cd", "kind": "vec4", "center": 0.5, "amplitude": 0.4}), scene.clone()).unwrap();
        let cd = out.point_sets[0].attrs.vec4("Cd").unwrap();
        assert!(cd.iter().any(|c| (c[0] - c[1]).abs() > 1e-3), "components differ");
        let wrong = attribute_noise(&json!({"name": "w", "kind": "vec3", "mode": "add"}), scene.clone()).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(wrong.contains("is a float"), "{wrong}");
        assert!(attribute_noise(&json!({"name": "P"}), scene).is_err());
    }

    #[test]
    fn a_falloff_region_limits_it_with_a_soft_edge_and_shows() {
        let mut g = PointSet { positions: (0..41).map(|i| Vec3::new(i as f64 * 0.1 - 2.0, 0.0, 0.0)).collect(), ..PointSet::default() };
        g.attrs.set("w", Attr::Float(vec![1.0; 41]), 41).unwrap();
        let scene = Scene { point_sets: vec![g], ..Scene::default() };
        // A 2-wide ball at 0 (radius 1), soft over its outer half.
        let p = json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": "sphere", "region_size": [2.0, 2.0, 2.0], "softness": 0.5});
        let out = attribute_noise(&p, scene.clone()).unwrap();
        let w = floats(&out, "w");
        let at = |x: f64| w[((x + 2.0) / 0.1).round() as usize];
        assert_eq!((at(0.0), at(0.4), at(1.5), at(-1.5)), (5.0, 5.0, 1.0, 1.0), "full inside, untouched outside");
        assert!(at(0.7) > 1.0 && at(0.7) < 5.0 && at(0.7) == at(-0.7), "between on the soft edge: {} {}", at(0.7), at(-0.7));
        assert_eq!(out.regions.len(), 1);
        assert!(out.regions[0].sphere);
        // A box instead: its corners count.
        let boxed = attribute_noise(&json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": "box", "region_size": [3.0, 1.0, 1.0]}), scene).unwrap();
        let w = floats(&boxed, "w");
        assert_eq!((w[((1.4 + 2.0) / 0.1f64).round() as usize], w[((1.6 + 2.0) / 0.1f64).round() as usize]), (5.0, 1.0));
        // Toward a corner: inside the box, outside the ball the same size.
        let mut corner = PointSet { positions: vec![Vec3::new(1.2, 0.4, 0.4)], ..PointSet::default() };
        corner.attrs.set("w", Attr::Float(vec![1.0]), 1).unwrap();
        let at = |shape: &str| {
            let p = json!({"name": "w", "center": 5.0, "amplitude": 0.0, "falloff": shape, "region_size": [3.0, 1.0, 1.0], "softness": 0.0});
            floats(&attribute_noise(&p, Scene { point_sets: vec![corner.clone()], ..Scene::default() }).unwrap(), "w")[0]
        };
        assert_eq!((at("box"), at("sphere")), (5.0, 1.0));
    }

    /// As a node in a graph, with its defaults (pscale around 1).
    #[test]
    fn the_node_in_a_graph() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "ground", "op": "plane", "params": {"size": [4, 4]}},
            {"id": "pts", "op": "scatter", "inputs": ["ground", "", ""], "params": {"density": 20}},
            {"id": "sizes", "op": "attribute_noise", "inputs": ["pts"], "params": {"frequency": 2}},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let out = crate::graph::Evaluator::new().evaluate(&graph, "sizes", &ctx).unwrap();
        let s = &out.scene.as_ref().unwrap().point_sets[0];
        let k = s.attrs.float("pscale").expect("pscale");
        assert!(k.len() > 100 && k.iter().all(|v| (0.5..=1.5).contains(v)), "{} points", k.len());
    }

    #[test]
    fn mesh_points_instances_and_groups() {
        let m = crate::mesh::plane([2.0, 2.0], [1, 1]);
        let model = scene::Model { node: "floor".into(), ..scene::Model::new(Arc::new(m), Mat4::identity(), Default::default()) };
        let inst = geo::Instances { worlds: vec![geom::trs([1.0, 2.0, 3.0], [0.0; 3], [1.0; 3]); 2], which: vec![0, 0], node: "copies".into(), ..geo::Instances::default() };
        let scene = Scene { meshes: vec![model], instances: vec![inst], point_sets: vec![grid()], ..Scene::default() };
        let out = attribute_noise(&json!({"name": "k", "elements": "mesh points"}), scene.clone()).unwrap();
        assert_eq!(out.meshes[0].mesh.point_attrs.float("k").map(<[f32]>::len), Some(4));
        assert!(out.point_sets[0].attrs.get("k").is_none(), "only the elements asked for");
        let out = attribute_noise(&json!({"name": "k", "elements": "instances"}), scene.clone()).unwrap();
        let k = out.instances[0].attrs.float("k").unwrap();
        assert_eq!(k[0], k[1], "at the same place");
        let out = attribute_noise(&json!({"name": "k", "group": "other*"}), scene).unwrap();
        assert!(out.point_sets[0].attrs.get("k").is_none(), "not in Group");
    }

    /// The k-d tree finds what a brute-force search finds.
    #[test]
    fn the_kd_tree_matches_brute_force() {
        let rnd = |k: u64| geo::stable_unit(&[k]) * 10.0 - 5.0;
        let pts: Vec<Vec3> = (0..2000).map(|i| Vec3::new(rnd(3 * i), rnd(3 * i + 1), rnd(3 * i + 2))).collect();
        let tree = KdTree::new(pts.clone());
        for q in 0..300u64 {
            let p = Vec3::new(rnd(100_000 + 3 * q), rnd(100_001 + 3 * q), rnd(100_002 + 3 * q)) * 1.2;
            let (i, d) = tree.nearest(&p).unwrap();
            let best = pts.iter().map(|x| (x - p).norm()).fold(f64::INFINITY, f64::min);
            assert!((d - best).abs() < 1e-12 && ((pts[i] - p).norm() - d).abs() < 1e-12, "{q}");
            let mut near: Vec<usize> = tree.within(&p, 1.5).into_iter().map(|(i, _)| i).collect();
            near.sort();
            let want: Vec<usize> = (0..pts.len()).filter(|&i| (pts[i] - p).norm() <= 1.5).collect();
            assert_eq!(near, want, "{q}");
        }
        assert!(KdTree::new(Vec::new()).nearest(&Vec3::zeros()).is_none());
    }

    /// A row of points from x = 0 to 4 (step 0.5) with a weight of 0.5.
    fn row() -> Scene {
        let mut g = PointSet { positions: (0..9).map(|i| Vec3::new(i as f64 * 0.5, 0.0, 0.0)).collect(), node: "row".into(), ..PointSet::default() };
        g.attrs.set("weight", Attr::Float(vec![0.5; 9]), 9).unwrap();
        Scene { point_sets: vec![g], ..Scene::default() }
    }

    fn weights(s: &Scene) -> Vec<f64> {
        floats(s, "weight").iter().map(|v| *v as f64).collect()
    }

    #[test]
    fn falloff_from_a_point_with_each_shape() {
        // A locator at the origin: full within 1, none past 3.
        let loc = Scene { points: vec![Vec3::zeros()], ..Scene::default() };
        let w = |shape: &str| weights(&falloff(&json!({"inner": 1.0, "outer": 3.0, "shape": shape}), row(), Some(&loc)).unwrap());
        let lin = w("linear");
        let want = [1.0, 1.0, 1.0, 0.75, 0.5, 0.25, 0.0, 0.0, 0.0];
        assert!(lin.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6), "{lin:?}");
        let sm = w("smooth");
        assert!((sm[3] - 1.0 + smooth(0.25)).abs() < 1e-6 && (sm[4] - 0.5).abs() < 1e-6 && sm[3] > lin[3]);
        let (ein, eout) = (w("ease in"), w("ease out"));
        assert!((ein[3] - (1.0 - 0.0625)).abs() < 1e-6 && (eout[3] - (1.0 - 0.4375)).abs() < 1e-6, "{ein:?} {eout:?}");
        let inv = weights(&falloff(&json!({"inner": 1.0, "outer": 3.0, "shape": "linear", "invert": true}), row(), Some(&loc)).unwrap());
        assert!(inv.iter().zip(&lin).all(|(a, b)| (a + b - 1.0).abs() < 1e-6));
    }

    #[test]
    fn falloff_from_curves_and_from_a_region() {
        // A curve along y at x = 4: the far end is near it.
        let mut c = geo::Curves { node: "path".into(), ..geo::Curves::default() };
        c.push(&[Vec3::new(4.0, -5.0, 0.0), Vec3::new(4.0, 5.0, 0.0)], false);
        let from = Scene { curves: vec![c], ..Scene::default() };
        let w = weights(&falloff(&json!({"inner": 0.0, "outer": 2.0, "shape": "linear"}), row(), Some(&from)).unwrap());
        assert!((w[8] - 1.0).abs() < 1e-6 && (w[6] - 0.5).abs() < 1e-6 && w[0] == 0.0, "{w:?}");
        // No input: a ball of radius 1 at x = 2, then out to 1 more.
        let out = falloff(&json!({"inner": 0.0, "outer": 1.0, "shape": "linear", "region": "sphere", "region_center": [2.0, 0.0, 0.0], "region_size": [2.0, 2.0, 2.0]}), row(), None).unwrap();
        let w = weights(&out);
        assert!(w[2..7].iter().all(|v| (v - 1.0).abs() < 1e-6) && (w[7] - 0.5).abs() < 1e-6 && w[0] == 0.0, "{w:?}");
        assert_eq!(out.regions.len(), 1);
        // A box 1 wide: x 1.5..2.5 inside.
        let w = weights(&falloff(&json!({"inner": 0.0, "outer": 1.0, "shape": "linear", "region": "box", "region_center": [2.0, 0.0, 0.0], "region_size": [1.0, 4.0, 4.0]}), row(), None).unwrap());
        assert!(w[3..6].iter().all(|v| (v - 1.0).abs() < 1e-6) && (w[2] - 0.5).abs() < 1e-6 && (w[6] - 0.5).abs() < 1e-6 && w[1] == 0.0 && w[8] == 0.0, "{w:?}");
        assert!(falloff(&json!({}), row(), Some(&Scene::default())).is_err(), "nothing to measure from");
    }

    /// As a node: weights by distance to a Locator wired to its from input.
    #[test]
    fn falloff_in_a_graph_from_a_locator() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "ground", "op": "plane", "params": {"size": [6, 6]}},
            {"id": "pts", "op": "scatter", "inputs": ["ground", "", ""], "params": {"density": 10}},
            {"id": "here", "op": "locator", "params": {"translate": [1.0, 0.0, 0.0]}},
            {"id": "near", "op": "falloff", "inputs": ["pts", "here"], "params": {"inner": 0.5, "outer": 2.0, "shape": "linear"}},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let out = crate::graph::Evaluator::new().evaluate(&graph, "near", &ctx).unwrap();
        let s = &out.scene.as_ref().unwrap().point_sets[0];
        let w = s.attrs.float("weight").expect("weight");
        for (i, k) in w.iter().enumerate() {
            let d = (s.placed(i) - Vec3::new(1.0, 0.0, 0.0)).norm();
            let want = 1.0 - ((d - 0.5) / 1.5).clamp(0.0, 1.0);
            assert!((*k as f64 - want).abs() < 1e-5, "{i}: {k} at {d}");
        }
        assert!(w.contains(&1.0) && w.contains(&0.0));
    }

    #[test]
    fn falloff_combines_with_what_is_there() {
        let loc = Scene { points: vec![Vec3::zeros()], ..Scene::default() };
        let w = |combine: &str| weights(&falloff(&json!({"inner": 1.0, "outer": 3.0, "shape": "linear", "combine": combine}), row(), Some(&loc)).unwrap());
        let (set, mul, max, min) = (w("set"), w("multiply"), w("max"), w("min"));
        for i in 0..9 {
            assert!((mul[i] - 0.5 * set[i]).abs() < 1e-6 && (max[i] - set[i].max(0.5)).abs() < 1e-6 && (min[i] - set[i].min(0.5)).abs() < 1e-6, "{i}");
        }
    }

    /// A 4 × 2 image whose red is its column and green its row (× 0.1),
    /// with a data window starting at (1, 0) of a 5 × 2 frame.
    fn ramp() -> crate::image::Image {
        let mut img = crate::image::Image::blank(crate::image::Size { width: 5, height: 2 }, crate::image::IRect::new(1, 0, 5, 2));
        for y in 0..2 {
            for x in 0..4 {
                img.pixels[y * 4 + x] = [(x + 1) as f32 * 0.1, y as f32 * 0.1, 0.0, 1.0];
            }
        }
        img
    }

    #[test]
    fn sampling_blends_pixel_centers_within_the_data_window() {
        let img = ramp();
        let near = |a: [f64; 4], b: [f64; 4]| (0..4).all(|k| (a[k] - b[k]).abs() < 1e-6);
        // Pixel (2, 1)'s center, and halfway between (2, 0) and (3, 0).
        assert!(near(sample(&img, 2.5, 1.5), [0.2, 0.1, 0.0, 1.0]));
        assert!(near(sample(&img, 3.0, 0.5), [0.25, 0.0, 0.0, 1.0]));
        // Left of the data window: transparent, blended at its edge.
        assert!(near(sample(&img, 0.5, 0.5), [0.0; 4]));
        assert!(near(sample(&img, 1.0, 0.5), [0.05, 0.0, 0.0, 0.5]));
    }

    /// As a node: a Plane's points take a red Constant's color by their uvs.
    #[test]
    fn image_in_a_graph_by_uv() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "ground", "op": "plane", "params": {"size": [2, 2], "segments": [3, 3]}},
            {"id": "red", "op": "constant", "params": {"width": 64, "height": 64, "color": [1.0, 0.0, 0.0, 1.0]}},
            {"id": "paint", "op": "attribute_from_image", "inputs": ["ground", "red", ""], "params": {"elements": "mesh points"}},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let out = crate::graph::Evaluator::new().evaluate(&graph, "paint", &ctx).unwrap();
        let cd = out.scene.as_ref().unwrap().meshes[0].mesh.point_attrs.vec4("Cd").expect("Cd").to_vec();
        assert_eq!(cd.len(), 16);
        // Red everywhere, its edges and corners too.
        assert!(cd.iter().all(|c| c == &[1.0, 0.0, 0.0, 1.0]), "{cd:?}");
    }

    #[test]
    fn image_via_uv_and_via_a_camera() {
        // Points with uvs at pixel centers of the frame (u = x / 5, v = 1 - y / 2).
        let mut g = PointSet { positions: vec![Vec3::zeros(); 3], node: "p".into(), ..PointSet::default() };
        g.attrs.set("uv", Attr::Vec3(vec![[2.5 / 5.0, 1.0 - 1.5 / 2.0, 0.0], [4.5 / 5.0, 1.0 - 0.5 / 2.0, 0.0], [1.5, 0.5, 0.0]]), 3).unwrap();
        let scene = Scene { point_sets: vec![g], ..Scene::default() };
        let out = attribute_from_image(&json!({}), scene.clone(), Some(&ramp()), None).unwrap();
        let cd = out.point_sets[0].attrs.vec4("Cd").unwrap();
        assert!((cd[0][0] - 0.2).abs() < 1e-6 && (cd[0][1] - 0.1).abs() < 1e-6 && (cd[1][0] - 0.4).abs() < 1e-6, "{cd:?}");
        assert_eq!(cd[2], [0.0; 4], "off the image: Fill");
        let lum = attribute_from_image(&json!({"take": "luminance", "name": "k"}), scene.clone(), Some(&ramp()), None).unwrap();
        assert!((lum.point_sets[0].attrs.float("k").unwrap()[0] as f64 - (0.2126 * 0.2 + 0.7152 * 0.1)).abs() < 1e-6);
        // Outside keeps what's there.
        let mut kept = scene.clone();
        kept.point_sets[0].attrs.set("k", Attr::Float(vec![7.0; 3]), 3).unwrap();
        let k = attribute_from_image(&json!({"take": "alpha", "name": "k"}), kept, Some(&ramp()), None).unwrap();
        assert_eq!(k.point_sets[0].attrs.float("k").unwrap(), [1.0, 1.0, 7.0]);
        // A camera at z = 10 looking down -z: a point on its axis lands at
        // the frame's middle; one behind it is outside.
        let cam = crate::geom::Camera { world: geom::trs([0.0, 0.0, 10.0], [0.0; 3], [1.0; 3]), ..crate::geom::Camera::default() };
        let seen = Scene { cameras: vec![cam], ..Scene::default() };
        let pts = Scene { point_sets: vec![PointSet { positions: vec![Vec3::zeros(), Vec3::new(0.0, 0.0, 20.0)], ..PointSet::default() }], ..Scene::default() };
        let out = attribute_from_image(&json!({"via": "camera", "take": "rgb", "outside": "fill", "fill": -1.0}), pts.clone(), Some(&ramp()), Some(&seen)).unwrap();
        let c = out.point_sets[0].attrs.vec3("Cd").unwrap();
        let mid = sample(&ramp(), 2.5, 1.0);
        assert!((c[0][0] as f64 - mid[0]).abs() < 1e-5 && (c[0][1] as f64 - mid[1]).abs() < 1e-5, "{c:?} vs {mid:?}");
        assert_eq!(c[1], [-1.0; 3], "behind the camera");
        assert!(attribute_from_image(&json!({"via": "camera"}), pts.clone(), Some(&ramp()), None).is_err(), "no camera");
        assert!(attribute_from_image(&json!({}), pts, None, None).is_err(), "no image");
    }

    /// Two source points (x = 0 with k 1 and Cd red, x = 10 with k 3 and an
    /// id), and targets along x.
    fn transfer_scenes() -> (Scene, Scene) {
        let mut src = PointSet { positions: vec![Vec3::zeros(), Vec3::new(10.0, 0.0, 0.0)], node: "src".into(), ..PointSet::default() };
        src.attrs.set("k", Attr::Float(vec![1.0, 3.0]), 2).unwrap();
        src.attrs.set("Cd", Attr::Vec4(vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]), 2).unwrap();
        src.attrs.set("id", Attr::Int(vec![5, 9]), 2).unwrap();
        let mut dst = PointSet { positions: [1.0, 4.0, 6.0, 20.0].iter().map(|x| Vec3::new(*x, 0.0, 0.0)).collect(), node: "dst".into(), ..PointSet::default() };
        dst.attrs.set("k", Attr::Float(vec![-1.0; 4]), 4).unwrap();
        (Scene { point_sets: vec![dst], ..Scene::default() }, Scene { point_sets: vec![src], ..Scene::default() })
    }

    #[test]
    fn transfer_takes_the_nearest_within_a_radius() {
        let (dst, src) = transfer_scenes();
        let out = attribute_transfer(&json!({}), dst.clone(), Some(&src)).unwrap();
        let a = &out.point_sets[0].attrs;
        assert_eq!(a.float("k").unwrap(), [1.0, 1.0, 3.0, 3.0], "nearest, no limit");
        assert_eq!(a.int("id").unwrap(), [5, 5, 9, 9]);
        assert_eq!(a.vec4("Cd").unwrap()[2], [0.0, 0.0, 1.0, 1.0], "all attributes by default");
        // Within 5: the far one keeps its value, or gets 0.
        let near = attribute_transfer(&json!({"radius": 5.0, "attributes": "k"}), dst.clone(), Some(&src)).unwrap();
        assert_eq!(near.point_sets[0].attrs.float("k").unwrap(), [1.0, 1.0, 3.0, -1.0]);
        assert!(near.point_sets[0].attrs.get("Cd").is_none(), "only those named");
        let zero = attribute_transfer(&json!({"radius": 5.0, "attributes": "k", "missing": "zero"}), dst.clone(), Some(&src)).unwrap();
        assert_eq!(zero.point_sets[0].attrs.float("k").unwrap(), [1.0, 1.0, 3.0, 0.0]);
        // A linear falloff over the radius fades toward what was there.
        let faded = attribute_transfer(&json!({"radius": 5.0, "attributes": "k", "falloff": "linear"}), dst.clone(), Some(&src)).unwrap();
        let k = faded.point_sets[0].attrs.float("k").unwrap();
        assert!((k[0] - (-1.0 + 2.0 * 0.8)).abs() < 1e-6 && (k[1] - (-1.0 + 2.0 * 0.2)).abs() < 1e-6, "{k:?}");
        assert!(attribute_transfer(&json!({"attributes": "nope"}), dst.clone(), Some(&src)).is_err());
        assert!(attribute_transfer(&json!({}), dst, None).is_err());
    }

    /// As a node: one Scatter's normals and ids carried to another's points.
    #[test]
    fn transfer_in_a_graph() {
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "ground", "op": "plane", "params": {"size": [4, 4]}},
            {"id": "coarse", "op": "scatter", "inputs": ["ground", "", ""], "params": {"density": 4, "seed": 1}},
            {"id": "dense", "op": "scatter", "inputs": ["ground", "", ""], "params": {"density": 30, "seed": 2}},
            {"id": "carry", "op": "attribute_transfer", "inputs": ["dense", "coarse"], "params": {"attributes": "N id"}},
        ]})).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let mut ev = crate::graph::Evaluator::new();
        let src = ev.evaluate(&graph, "coarse", &ctx).unwrap().scene.clone().unwrap();
        let out = ev.evaluate(&graph, "carry", &ctx).unwrap();
        let (dst, src) = (&out.scene.as_ref().unwrap().point_sets[0], &src.point_sets[0]);
        let (ids, src_ids) = (dst.attrs.int("id").unwrap(), src.attrs.int("id").unwrap());
        let tree = KdTree::new((0..src.len()).map(|i| src.placed(i)).collect());
        for (i, id) in ids.iter().enumerate() {
            let (j, _) = tree.nearest(&dst.placed(i)).unwrap();
            assert_eq!(*id, src_ids[j], "{i}: the nearest's id");
        }
        assert!(dst.attrs.vec3("N").unwrap().iter().all(|n| (n[2] - 1.0).abs() < 1e-6), "the plane's normal");
    }

    #[test]
    fn transfer_averages_by_inverse_distance() {
        let (dst, src) = transfer_scenes();
        let out = attribute_transfer(&json!({"mode": "average", "radius": 12.0, "attributes": "k id"}), dst.clone(), Some(&src)).unwrap();
        let a = &out.point_sets[0].attrs;
        // At x = 4: 4 from k 1 and 6 from k 3: (1/4 + 3/6) / (1/4 + 1/6) = 1.8.
        assert!((a.float("k").unwrap()[1] - 1.8).abs() < 1e-4, "{:?}", a.float("k"));
        assert_eq!(a.int("id").unwrap()[..3], [5, 5, 9], "ints from the nearest");
        assert!(attribute_transfer(&json!({"mode": "average"}), dst, Some(&src)).is_err(), "needs a radius");
    }
}
