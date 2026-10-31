//! Procedural geometry's data (docs/geo-plan.md, P1): named attributes per
//! element, and the scene's point sets, instances and curves. Geometry
//! nodes take a [`crate::scene::Scene`] and give one back, acting on its
//! meshes and these.
//!
//! An attribute holds one value per element (a point, a face, an
//! instance), in element order. Some names mean something to the nodes
//! and renderers that read them (see the `consts` below); any other name
//! is the user's own.

use crate::geom::{self, Mat4, Vec3};
use crate::scene::{Card, Model};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A point's normal (on a mesh, its `normals` instead).
pub const N: &str = "N";
/// The up direction an instance is turned to, with N.
pub const UP: &str = "up";
/// A uniform size.
pub const PSCALE: &str = "pscale";
/// A turn, as a quaternion (x, y, z, w), a [`Attr::Vec4`].
pub const ORIENT: &str = "orient";
/// A size per axis.
pub const SCALE: &str = "scale";
/// A color, linear RGBA, a [`Attr::Vec4`] (an instance's tint).
pub const CD: &str = "Cd";
/// An opacity.
pub const ALPHA: &str = "Alpha";
/// A stable number per element (motion blur and random picks follow it).
pub const ID: &str = "id";
/// Which prototype an instance takes.
pub const VARIANT: &str = "variant";
/// The names geometry nodes give a meaning to ("P" is the positions
/// themselves).
pub const RESERVED: [&str; 10] = ["P", N, UP, PSCALE, ORIENT, SCALE, CD, ALPHA, ID, VARIANT];

/// One value per element.
#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Float(Vec<f32>),
    Vec3(Vec<[f32; 3]>),
    /// Four values: a color ([`CD`]) or a quaternion ([`ORIENT`]).
    Vec4(Vec<[f32; 4]>),
    Int(Vec<i32>),
}

impl Attr {
    /// How many elements it has values for.
    pub fn len(&self) -> usize {
        match self {
            Attr::Float(v) => v.len(),
            Attr::Vec3(v) => v.len(),
            Attr::Vec4(v) => v.len(),
            Attr::Int(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Its kind's name ("float", "vec3", "vec4", "int").
    pub fn kind(&self) -> &'static str {
        match self {
            Attr::Float(_) => "float",
            Attr::Vec3(_) => "vec3",
            Attr::Vec4(_) => "vec4",
            Attr::Int(_) => "int",
        }
    }

    /// `n` zeros of the same kind.
    pub fn zeros(&self, n: usize) -> Attr {
        match self {
            Attr::Float(_) => Attr::Float(vec![0.0; n]),
            Attr::Vec3(_) => Attr::Vec3(vec![[0.0; 3]; n]),
            Attr::Vec4(_) => Attr::Vec4(vec![[0.0; 4]; n]),
            Attr::Int(_) => Attr::Int(vec![0; n]),
        }
    }

    /// The values at `indices`, in that order.
    pub fn gathered(&self, indices: &[u32]) -> Attr {
        fn pick<T: Copy>(v: &[T], indices: &[u32]) -> Vec<T> {
            indices.iter().map(|&i| v[i as usize]).collect()
        }
        match self {
            Attr::Float(v) => Attr::Float(pick(v, indices)),
            Attr::Vec3(v) => Attr::Vec3(pick(v, indices)),
            Attr::Vec4(v) => Attr::Vec4(pick(v, indices)),
            Attr::Int(v) => Attr::Int(pick(v, indices)),
        }
    }

    /// `other`'s values after these (None when the kinds differ).
    fn appended(&self, other: &Attr) -> Option<Attr> {
        Some(match (self, other) {
            (Attr::Float(a), Attr::Float(b)) => Attr::Float([&a[..], b].concat()),
            (Attr::Vec3(a), Attr::Vec3(b)) => Attr::Vec3([&a[..], b].concat()),
            (Attr::Vec4(a), Attr::Vec4(b)) => Attr::Vec4([&a[..], b].concat()),
            (Attr::Int(a), Attr::Int(b)) => Attr::Int([&a[..], b].concat()),
            _ => return None,
        })
    }
}

/// Attributes by name, each with one value per element of what holds
/// them (the holder checks the count: see [`Attrs::set`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs(BTreeMap<String, Attr>);

impl Attrs {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Set `name` to `attr`, for `count` elements: an error if it has
    /// another number of values.
    pub fn set(&mut self, name: &str, attr: Attr, count: usize) -> Result<()> {
        if attr.len() != count {
            bail!("attribute {name}: {} values for {count} elements", attr.len());
        }
        self.0.insert(name.to_string(), attr);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Attr> {
        self.0.get(name)
    }

    pub fn remove(&mut self, name: &str) -> Option<Attr> {
        self.0.remove(name)
    }

    pub fn float(&self, name: &str) -> Option<&[f32]> {
        match self.0.get(name)? {
            Attr::Float(v) => Some(v),
            _ => None,
        }
    }

    pub fn vec3(&self, name: &str) -> Option<&[[f32; 3]]> {
        match self.0.get(name)? {
            Attr::Vec3(v) => Some(v),
            _ => None,
        }
    }

    pub fn vec4(&self, name: &str) -> Option<&[[f32; 4]]> {
        match self.0.get(name)? {
            Attr::Vec4(v) => Some(v),
            _ => None,
        }
    }

    pub fn int(&self, name: &str) -> Option<&[i32]> {
        match self.0.get(name)? {
            Attr::Int(v) => Some(v),
            _ => None,
        }
    }

    /// The names and attributes, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Attr)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// An error naming the first attribute without `count` values.
    pub fn check(&self, count: usize) -> Result<()> {
        match self.0.iter().find(|(_, a)| a.len() != count) {
            Some((name, a)) => bail!("attribute {name}: {} values for {count} elements", a.len()),
            None => Ok(()),
        }
    }

    /// Each attribute's values at `indices` (elements copied, e.g. a
    /// point's to each face corner it's on).
    pub fn gathered(&self, indices: &[u32]) -> Attrs {
        Attrs(self.0.iter().map(|(k, a)| (k.clone(), a.gathered(indices))).collect())
    }

    /// The attributes of `parts` (each its attributes and its element
    /// count) one after the other: a part without one of them gets zeros
    /// there; a name with different kinds in two parts is left out.
    pub fn concat(parts: &[(&Attrs, usize)]) -> Attrs {
        let mut out = BTreeMap::new();
        let names: std::collections::BTreeSet<&String> = parts.iter().flat_map(|(a, _)| a.0.keys()).collect();
        'names: for name in names {
            let Some(model) = parts.iter().find_map(|(a, _)| a.0.get(name)) else { continue };
            let mut all = model.zeros(0);
            for (a, n) in parts {
                let next = a.0.get(name).cloned().unwrap_or_else(|| model.zeros(*n));
                match all.appended(&next) {
                    Some(joined) => all = joined,
                    None => continue 'names,
                }
            }
            out.insert(name.clone(), all);
        }
        Attrs(out)
    }

    /// Turned and stretched as `m` moves what holds them (see
    /// [`Mesh::transform`](crate::mesh::Mesh::transform)): N as a normal
    /// (the inverse transpose), up as a direction, both made unit again,
    /// orient turned by `m`'s rotation. The rest stay as they are.
    pub fn transform(&mut self, m: &Mat4) {
        let linear = m.fixed_view::<3, 3>(0, 0).into_owned();
        let normal = linear.try_inverse().map(|i| i.transpose()).unwrap_or(linear);
        let turn = |l: &nalgebra::Matrix3<f64>, v: &mut [f32; 3]| {
            let t = l * Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64);
            if let Some(t) = t.try_normalize(1e-300) {
                *v = [t.x as f32, t.y as f32, t.z as f32];
            }
        };
        if let Some(Attr::Vec3(v)) = self.0.get_mut(N) {
            v.iter_mut().for_each(|v| turn(&normal, v));
        }
        if let Some(Attr::Vec3(v)) = self.0.get_mut(UP) {
            v.iter_mut().for_each(|v| turn(&linear, v));
        }
        if let Some(Attr::Vec4(v)) = self.0.get_mut(ORIENT) {
            use nalgebra::{Quaternion, Rotation3, UnitQuaternion};
            let r = UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_eps(&linear, 1e-12, 100, Rotation3::identity()));
            for q in v.iter_mut() {
                let t = r * UnitQuaternion::new_normalize(Quaternion::new(q[3] as f64, q[0] as f64, q[1] as f64, q[2] as f64));
                *q = [t.i as f32, t.j as f32, t.k as f32, t.w as f32];
            }
        }
    }
}

/// Points with attributes and no faces (what Scatter makes and Instance
/// on Points takes), in their own space, placed by `world`.
#[derive(Clone, Debug, PartialEq)]
pub struct PointSet {
    pub positions: Vec<Vec3>,
    pub attrs: Attrs,
    pub world: Mat4,
    /// The node it came from ("" until tagged).
    pub node: String,
}

impl Default for PointSet {
    fn default() -> PointSet {
        PointSet { positions: Vec::new(), attrs: Attrs::default(), world: Mat4::identity(), node: String::new() }
    }
}

impl PointSet {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Point `i` in scene space.
    pub fn placed(&self, i: usize) -> Vec3 {
        geom::transform_point(&self.world, &self.positions[i])
    }

    /// A box around the placed points (None with none).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        boxed((0..self.len()).map(|i| self.placed(i)))
    }

    /// An error if an attribute isn't one value a point.
    pub fn validate(&self) -> Result<()> {
        self.attrs.check(self.len())
    }
}

/// Polylines: curve k is `positions[starts[k] .. starts[k + 1]]`, closed
/// (back to its first point) or not; attributes per point.
#[derive(Clone, Debug, PartialEq)]
pub struct Curves {
    pub positions: Vec<Vec3>,
    pub attrs: Attrs,
    /// Where each curve starts, and one more: the number of points.
    pub starts: Vec<u32>,
    pub closed: Vec<bool>,
    pub world: Mat4,
    /// The node they came from ("" until tagged).
    pub node: String,
}

impl Default for Curves {
    fn default() -> Curves {
        Curves { positions: Vec::new(), attrs: Attrs::default(), starts: vec![0], closed: Vec::new(), world: Mat4::identity(), node: String::new() }
    }
}

impl Curves {
    /// How many curves.
    pub fn len(&self) -> usize {
        self.closed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    /// Add a curve through `points`.
    pub fn push(&mut self, points: &[Vec3], closed: bool) {
        self.positions.extend_from_slice(points);
        self.starts.push(self.positions.len() as u32);
        self.closed.push(closed);
    }

    /// Curve `k`'s points.
    pub fn curve(&self, k: usize) -> &[Vec3] {
        &self.positions[self.starts[k] as usize..self.starts[k + 1] as usize]
    }

    /// Every segment of every curve in scene space (a closed one's last
    /// back to its first).
    pub fn segments(&self) -> Vec<(Vec3, Vec3)> {
        let place = |p: &Vec3| geom::transform_point(&self.world, p);
        let mut out = Vec::new();
        for k in 0..self.len() {
            let pts: Vec<Vec3> = self.curve(k).iter().map(place).collect();
            out.extend(pts.windows(2).map(|w| (w[0], w[1])));
            if self.closed[k] && pts.len() > 2 {
                out.push((pts[pts.len() - 1], pts[0]));
            }
        }
        out
    }

    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        boxed(self.positions.iter().map(|p| geom::transform_point(&self.world, p)))
    }

    /// An error if the starts don't run up to the points in order, a
    /// curve has no closed flag, or an attribute isn't one value a point.
    pub fn validate(&self) -> Result<()> {
        if self.starts.first() != Some(&0) || self.starts.last().copied() != Some(self.positions.len() as u32) || self.starts.windows(2).any(|w| w[0] > w[1]) {
            bail!("curves: starts {:?} don't run 0 to {} in order", self.starts, self.positions.len());
        }
        if self.closed.len() + 1 != self.starts.len() {
            bail!("curves: {} closed flags for {} curves", self.closed.len(), self.starts.len() - 1);
        }
        self.attrs.check(self.positions.len())
    }
}

/// What an instance is a copy of.
#[derive(Clone)]
pub enum Prototype {
    Model(Model),
    Card(Card),
    Splats(Arc<crate::splat::Splats>),
}

impl Prototype {
    /// A box around it in its own space (as its world places it, before
    /// an instance's).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        match self {
            Prototype::Model(m) => m.bounds(),
            Prototype::Card(c) => boxed(c.corners().into_iter()),
            Prototype::Splats(s) => s.bounds(),
        }
    }
}

/// Copies of prototypes at many placements, kept as copies (the renderers
/// draw them instanced): instance i is `prototypes[which[i]]` placed by
/// `worlds[i]` (after the prototype's own world); attributes per instance
/// (its tint is [`CD`]).
#[derive(Clone, Default)]
pub struct Instances {
    pub prototypes: Vec<Prototype>,
    pub which: Vec<u32>,
    pub worlds: Vec<Mat4>,
    pub attrs: Attrs,
    /// The node they came from ("" until tagged).
    pub node: String,
}

impl Instances {
    pub fn len(&self) -> usize {
        self.worlds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.worlds.is_empty()
    }

    /// Instance `i`'s middle in scene space (its prototype's box's, placed).
    pub fn center(&self, i: usize) -> Vec3 {
        let mid = self.prototypes.get(self.which[i] as usize).and_then(Prototype::bounds).map_or_else(Vec3::zeros, |(lo, hi)| (lo + hi) / 2.0);
        geom::transform_point(&self.worlds[i], &mid)
    }

    /// A box around every instance (each prototype's box through its
    /// placement).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let boxes: Vec<Option<(Vec3, Vec3)>> = self.prototypes.iter().map(Prototype::bounds).collect();
        let corners = self.worlds.iter().zip(&self.which).flat_map(|(w, &k)| {
            let b = boxes.get(k as usize).copied().flatten();
            b.map(|(lo, hi)| corners(lo, hi).map(|c| geom::transform_point(w, &c)).to_vec()).unwrap_or_default()
        });
        boxed(corners)
    }

    /// An error if the lists don't agree in length or an instance names a
    /// prototype there isn't.
    pub fn validate(&self) -> Result<()> {
        if self.which.len() != self.len() {
            bail!("instances: {} placements, {} prototype picks", self.len(), self.which.len());
        }
        if let Some(k) = self.which.iter().find(|&&k| k as usize >= self.prototypes.len()) {
            bail!("instances: prototype {k} of {}", self.prototypes.len());
        }
        self.attrs.check(self.len())
    }
}

/// The eight corners of the box `lo`–`hi`.
pub fn corners(lo: Vec3, hi: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }))
}

/// The box around `points` (None for none).
fn boxed(mut points: impl Iterator<Item = Vec3>) -> Option<(Vec3, Vec3)> {
    let first = points.next()?;
    Some(points.fold((first, first), |(lo, hi), p| (lo.inf(&p), hi.sup(&p))))
}

/// Element `i` of a, taken to element `j` of b, `w` of the way, for the
/// elements a and b have in common: by their `id` attribute when both
/// have one (each id once), else by index when they have as many; None
/// when they can't be matched (then nothing blurs).
pub fn matched(a: (&Attrs, usize), b: (&Attrs, usize)) -> Option<Vec<Option<usize>>> {
    if let (Some(ia), Some(ib)) = (a.0.int(ID), b.0.int(ID)) {
        let mut at: std::collections::HashMap<i32, usize> = std::collections::HashMap::with_capacity(ib.len());
        for (j, id) in ib.iter().enumerate() {
            if at.insert(*id, j).is_some() {
                return None;
            }
        }
        return Some(ia.iter().map(|id| at.get(id).copied()).collect());
    }
    (a.1 == b.1).then(|| (0..a.1).map(Some).collect())
}

/// Whether `name` matches `pattern`, where each * stands for any run of
/// characters (none too).
pub fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name[first.len()..].ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(k) => rest = &rest[k + part.len()..],
            None => return false,
        }
    }
    true
}

/// A number from `parts` that's the same every run (on every machine)
/// and changes completely with any of them: what makes geometry nodes'
/// random choices stable (see docs/geo-plan.md, D5).
pub fn stable_hash(parts: &[u64]) -> u64 {
    parts.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &p| mix(h ^ mix(p.wrapping_add(0x632b_e59b_d9b4_e019))))
}

/// A stable number in 0..1 from `parts` (see [`stable_hash`]).
pub fn stable_unit(parts: &[u64]) -> f64 {
    (stable_hash(parts) >> 11) as f64 / (1u64 << 53) as f64
}

/// SplitMix64's finalizer: every bit of `z` into every bit out.
pub fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_check_their_length_and_concatenate() {
        let mut a = Attrs::default();
        a.set("w", Attr::Float(vec![1.0, 2.0]), 2).unwrap();
        assert!(a.set("bad", Attr::Int(vec![1]), 2).is_err(), "one value for two elements");
        a.set(CD, Attr::Vec4(vec![[1.0; 4]; 2]), 2).unwrap();
        a.set("k", Attr::Int(vec![5, 6]), 2).unwrap();
        assert!(a.check(2).is_ok() && a.check(3).is_err());
        let mut b = Attrs::default();
        b.set("w", Attr::Float(vec![3.0]), 1).unwrap();
        b.set("k", Attr::Float(vec![7.0]), 1).unwrap();
        // b has no Cd (zeros), and its k is a float (left out).
        let c = Attrs::concat(&[(&a, 2), (&b, 1)]);
        assert_eq!(c.float("w"), Some(&[1.0, 2.0, 3.0][..]));
        assert_eq!(c.vec4(CD), Some(&[[1.0; 4], [1.0; 4], [0.0; 4]][..]));
        assert!(c.get("k").is_none());
        assert!(c.check(3).is_ok());
        // Per corner, from points 1, 0, 1.
        assert_eq!(a.gathered(&[1, 0, 1]).int("k"), Some(&[6, 5, 6][..]));
        // A part with no attributes at all gets zeros.
        let none = Attrs::default();
        assert_eq!(Attrs::concat(&[(&none, 2), (&b, 1)]).float("w"), Some(&[0.0, 0.0, 3.0][..]));
    }

    #[test]
    fn a_move_turns_normals_up_and_orient() {
        let mut a = Attrs::default();
        a.set(N, Attr::Vec3(vec![[1.0, 1.0, 0.0]]), 1).unwrap();
        a.set(UP, Attr::Vec3(vec![[0.0, 1.0, 0.0]]), 1).unwrap();
        a.set(ORIENT, Attr::Vec4(vec![[0.0, 0.0, 0.0, 1.0]]), 1).unwrap();
        a.set(SCALE, Attr::Vec3(vec![[1.0, 2.0, 3.0]]), 1).unwrap();
        // Stretched 2× in x: the 45° normal leans toward y (inverse
        // transpose), up stays up; then 90° about z.
        let m = geom::trs([5.0, 0.0, 0.0], [0.0, 0.0, 90.0], [2.0, 1.0, 1.0]);
        a.transform(&m);
        let n = a.vec3(N).unwrap()[0];
        let want = (m.fixed_view::<3, 3>(0, 0).try_inverse().unwrap().transpose() * Vec3::new(1.0, 1.0, 0.0)).normalize();
        assert!((Vec3::new(n[0] as f64, n[1] as f64, n[2] as f64) - want).norm() < 1e-6, "{n:?} against {want}");
        let up = a.vec3(UP).unwrap()[0];
        assert!((up[0] + 1.0).abs() < 1e-6 && up[1].abs() < 1e-6, "{up:?}: +y turned to -x");
        let q = a.vec4(ORIENT).unwrap()[0];
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!((q[2] - half).abs() < 1e-6 && (q[3] - half).abs() < 1e-6, "{q:?}: 90° about z");
        assert_eq!(a.vec3(SCALE).unwrap()[0], [1.0, 2.0, 3.0], "scale isn't a direction");
    }

    #[test]
    fn curves_points_and_instances_check_themselves() {
        let mut c = Curves::default();
        c.push(&[Vec3::zeros(), Vec3::x(), Vec3::new(1.0, 1.0, 0.0)], true);
        c.push(&[Vec3::z(), Vec3::new(0.0, 0.0, 2.0)], false);
        assert!(c.validate().is_ok());
        assert_eq!(c.len(), 2);
        assert_eq!(c.segments().len(), 3 + 1);
        c.closed.pop();
        assert!(c.validate().is_err());
        let p = PointSet { positions: vec![Vec3::zeros(), Vec3::x()], world: geom::trs([0.0, 1.0, 0.0], [0.0; 3], [1.0; 3]), ..PointSet::default() };
        assert_eq!(p.bounds(), Some((Vec3::y(), Vec3::new(1.0, 1.0, 0.0))));
        let mut bad = p.clone();
        bad.attrs.0.insert(PSCALE.into(), Attr::Float(vec![1.0]));
        assert!(bad.validate().is_err());
        let cube = Model::new(Arc::new(crate::mesh::cube([2.0; 3], [1; 3])), Mat4::identity(), crate::mesh::Material::default());
        let inst = Instances { prototypes: vec![Prototype::Model(cube)], which: vec![0, 0], worlds: vec![Mat4::identity(), geom::trs([10.0, 0.0, 0.0], [0.0; 3], [1.0; 3])], ..Instances::default() };
        assert!(inst.validate().is_ok());
        assert_eq!(inst.bounds(), Some((Vec3::new(-1.0, -1.0, -1.0), Vec3::new(11.0, 1.0, 1.0))));
        assert_eq!(inst.center(1), Vec3::new(10.0, 0.0, 0.0));
        assert!(Instances { which: vec![1], ..inst.clone() }.validate().is_err());
    }

    #[test]
    fn elements_match_by_id_or_index() {
        let ids = |v: Vec<i32>| {
            let mut a = Attrs::default();
            let n = v.len();
            a.set(ID, Attr::Int(v), n).unwrap();
            a
        };
        let (a, b) = (ids(vec![7, 3, 9]), ids(vec![9, 7]));
        assert_eq!(matched((&a, 3), (&b, 2)), Some(vec![Some(1), None, Some(0)]));
        let none = Attrs::default();
        assert_eq!(matched((&none, 2), (&none, 2)), Some(vec![Some(0), Some(1)]));
        assert_eq!(matched((&none, 2), (&none, 3)), None);
        assert_eq!(matched((&a, 3), (&ids(vec![1, 1]), 2)), None, "an id twice: no matching");
    }
}
