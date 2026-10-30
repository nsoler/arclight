//! Particles: the simulation engine (no nodes, no drawing).
//!
//! A [`System`] is a set of [`Emitter`]s and [`Modifier`]s. Emitters give
//! birth to [`Particle`]s on a [`Shape`] (a point, a line, a rectangle,
//! disk, sphere or box, a mesh's surface or vertices, or an image's alpha),
//! at a rate per frame and/or in a burst, with a velocity in a cone,
//! a lifetime, and a size and color that change over it. Modifiers push
//! them (gravity, wind and drag, curl-noise turbulence, attractors,
//! vortices) or constrain them (bounce off a plane, kill by age or region,
//! a speed limit), each with a strength and the frames it acts in.
//!
//! Time: frames for when things happen (start, end, lifetimes, ages),
//! seconds for motion (velocities in units per second, accelerations in
//! units per second²), joined by [`System::fps`]. The state at frame `f`
//! is the simulation run up to time `f`; a particle born at frame `f` has
//! age 0 there. Each frame steps in [`System::substeps`]; a particle born
//! inside a substep moves only for the rest of it, so a steady rate gives
//! an evenly spaced stream. Positions update as `x + v·dt + ½·a·dt²`, exact
//! for constant forces (gravity) at any substep count.
//!
//! Repeatable: every random choice is a hash of the emitter's seed, the
//! particle's id and what the number is for, so the same system always
//! gives the same particles, whatever was simulated before (no global
//! state). [`Cache`] keeps each simulated frame: going back is a lookup,
//! going forward simulates only what's missing, and a changed system is
//! re-simulated only from the first frame the change affects.
//!
//! Params can be animated: give the cache a [`Params`] (any
//! `Fn(frame) -> System`) instead of a fixed [`System`], and each step
//! samples it at both ends of the frame (an emitter's transform is
//! interpolated for particles born in between).

use crate::geom::{Mat4, Vec3};
use crate::image::Image;
use crate::mesh::Mesh;
use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

/// One particle.
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    /// Unique in its system: the emitter's index in the high 32 bits, the
    /// birth count in the low ones.
    pub id: u64,
    pub position: Vec3,
    /// Units per second.
    pub velocity: Vec3,
    /// Units per second per second: what the modifiers pulled it by in its
    /// last step (0 when born), to move it on between frames.
    pub accel: Vec3,
    /// Frames since birth.
    pub age: f64,
    /// Frames it lives (it's removed once its age reaches this).
    pub life: f64,
    /// Its size now (the start size scaled over life).
    pub size: f64,
    /// Its color now (premultiplied linear RGBA).
    pub color: [f32; 4],
    /// Degrees about [`axis`](Self::axis) (a sprite's roll about the view
    /// axis), and how fast it turns (degrees per second).
    pub rotation: f64,
    pub spin: f64,
    /// A random unit axis, for spinning instanced objects.
    pub axis: Vec3,
    /// The size it was born with.
    pub start_size: f64,
}

impl Particle {
    /// It `secs` seconds on (or back), along its velocity and pulled by its
    /// acceleration: where it is, and how fast it goes then.
    pub fn moved(&self, secs: f64) -> (Vec3, Vec3) {
        (self.position + self.velocity * secs + self.accel * (0.5 * secs * secs), self.velocity + self.accel * secs)
    }
}

/// Where an emitter's particles are born, in its own space (moved by
/// [`Emitter::transform`]). Shapes holding a mesh or mask compare equal
/// only when they share it (the same `Arc`), so keep one per source.
#[derive(Clone, Debug)]
pub enum Shape {
    Point,
    /// Along X, centered.
    Line { length: f64 },
    /// In the XY plane, centered, facing +Z.
    Rect { width: f64, height: f64 },
    /// In the XY plane, centered, facing +Z.
    Disk { radius: f64 },
    /// On its surface or throughout it.
    Sphere { radius: f64, surface: bool },
    /// Centered; on its faces or throughout it.
    Box { size: Vec3, surface: bool },
    /// Anywhere on a mesh's triangles (evenly by area); see [`Shape::mesh_surface`].
    MeshSurface(Arc<Surface>),
    /// At a mesh's vertices (one picked at random per particle).
    MeshVertices(Arc<Mesh>),
    /// Where an image's alpha is, as likely as the alpha is high; see
    /// [`Shape::mask`].
    Mask(Arc<Mask>),
}

impl PartialEq for Shape {
    fn eq(&self, other: &Shape) -> bool {
        match (self, other) {
            (Shape::MeshSurface(a), Shape::MeshSurface(b)) => Arc::ptr_eq(a, b),
            (Shape::MeshVertices(a), Shape::MeshVertices(b)) => Arc::ptr_eq(a, b),
            (Shape::Mask(a), Shape::Mask(b)) => Arc::ptr_eq(a, b),
            (Shape::Point, Shape::Point) => true,
            (Shape::Line { length: a }, Shape::Line { length: b }) => a == b,
            (Shape::Rect { width: a, height: b }, Shape::Rect { width: c, height: d }) => (a, b) == (c, d),
            (Shape::Disk { radius: a }, Shape::Disk { radius: b }) => a == b,
            (Shape::Sphere { radius: a, surface: b }, Shape::Sphere { radius: c, surface: d }) => (a, b) == (c, d),
            (Shape::Box { size: a, surface: b }, Shape::Box { size: c, surface: d }) => (a, b) == (c, d),
            _ => false,
        }
    }
}

/// A mesh ready to emit from its surface.
#[derive(Clone, Debug)]
pub struct Surface {
    pub mesh: Arc<Mesh>,
    /// Running total of the triangles' areas.
    areas: Vec<f64>,
}

/// An image's alpha ready to emit from. Positions are frame pixels: x to
/// the right, y down from the frame's top-left, z 0 (as the image stores
/// its rows); a particle lands anywhere in its pixel.
#[derive(Clone, Debug)]
pub struct Mask {
    /// The pixels with any alpha, and the running total of their alpha.
    pixels: Vec<(i32, i32)>,
    weights: Vec<f64>,
}

impl Shape {
    /// Emit from `mesh`'s surface.
    pub fn mesh_surface(mesh: Arc<Mesh>) -> Shape {
        let mut total = 0.0;
        let areas = mesh
            .triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
                total += (b - a).cross(&(c - a)).norm() / 2.0;
                total
            })
            .collect();
        Shape::MeshSurface(Arc::new(Surface { mesh, areas }))
    }

    /// Emit where `image`'s alpha is.
    pub fn mask(image: &Image) -> Shape {
        let (mut pixels, mut weights, mut total) = (vec![], vec![], 0.0);
        for y in 0..image.height {
            for x in 0..image.width {
                let a = image.pixels[y * image.width + x][3];
                if a > 0.0 {
                    total += a as f64;
                    pixels.push((image.x0 + x as i32, image.y0 + y as i32));
                    weights.push(total);
                }
            }
        }
        Shape::Mask(Arc::new(Mask { pixels, weights }))
    }

    /// A point on the shape and the way out of it there (zero where there
    /// isn't one), from random numbers `r` in 0..1. None for a shape with
    /// nowhere to emit (an empty mesh or mask).
    fn sample(&self, r: [f64; 4]) -> Option<(Vec3, Vec3)> {
        let z = Vec3::z();
        Some(match self {
            Shape::Point => (Vec3::zeros(), Vec3::zeros()),
            Shape::Line { length } => (Vec3::new((r[0] - 0.5) * length, 0.0, 0.0), Vec3::zeros()),
            Shape::Rect { width, height } => (Vec3::new((r[0] - 0.5) * width, (r[1] - 0.5) * height, 0.0), z),
            Shape::Disk { radius } => {
                let (d, a) = (radius * r[0].sqrt(), TAU * r[1]);
                (Vec3::new(d * a.cos(), d * a.sin(), 0.0), z)
            }
            Shape::Sphere { radius, surface } => {
                let n = unit_vector(r[0], r[1]);
                let d = if *surface { *radius } else { radius * r[2].cbrt() };
                (n * d, n)
            }
            Shape::Box { size, surface: false } => (Vec3::new(r[0] - 0.5, r[1] - 0.5, r[2] - 0.5).component_mul(size), Vec3::zeros()),
            Shape::Box { size, surface: true } => {
                // A face, as likely as its area: YZ, XZ or XY, then a side.
                let areas = [size.y * size.z, size.x * size.z, size.x * size.y];
                let total: f64 = areas.iter().sum();
                if total <= 0.0 {
                    return Some((Vec3::zeros(), Vec3::zeros()));
                }
                let pick = r[3] * total;
                let axis = if pick < areas[0] { 0 } else if pick < areas[0] + areas[1] { 1 } else { 2 };
                // The face's side from where in its share the pick fell.
                let start: f64 = areas[..axis].iter().sum();
                let side = if (pick - start) < areas[axis] / 2.0 { -1.0 } else { 1.0 };
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let mut p = Vec3::zeros();
                p[axis] = side * size[axis] / 2.0;
                p[u] = (r[0] - 0.5) * size[u];
                p[v] = (r[1] - 0.5) * size[v];
                let mut n = Vec3::zeros();
                n[axis] = side;
                (p, n)
            }
            Shape::MeshSurface(s) => {
                let total = *s.areas.last()?;
                if total <= 0.0 {
                    return None;
                }
                let i = pick(&s.areas, r[2] * total);
                let t = s.mesh.triangles[i].map(|i| i as usize);
                let [a, b, c] = t.map(|i| s.mesh.positions[i]);
                // Even over the triangle: fold the unit square in half.
                let (mut u, mut v) = (r[0], r[1]);
                if u + v > 1.0 {
                    (u, v) = (1.0 - u, 1.0 - v);
                }
                let w = 1.0 - u - v;
                let n = if s.mesh.normals.len() == s.mesh.positions.len() {
                    s.mesh.normals[t[0]] * w + s.mesh.normals[t[1]] * u + s.mesh.normals[t[2]] * v
                } else {
                    (b - a).cross(&(c - a))
                };
                (a * w + b * u + c * v, n.try_normalize(1e-12).unwrap_or_default())
            }
            Shape::MeshVertices(m) => {
                if m.positions.is_empty() {
                    return None;
                }
                let i = ((r[2] * m.positions.len() as f64) as usize).min(m.positions.len() - 1);
                (m.positions[i], m.normals.get(i).copied().unwrap_or_default())
            }
            Shape::Mask(m) => {
                let total = *m.weights.last()?;
                let (x, y) = m.pixels[pick(&m.weights, r[2] * total)];
                (Vec3::new(x as f64 + r[0], y as f64 + r[1], 0.0), z)
            }
        })
    }
}

/// The first index whose running total passes `at`.
fn pick(totals: &[f64], at: f64) -> usize {
    totals.partition_point(|&t| t <= at).min(totals.len() - 1)
}

/// An even direction on the sphere from `u`, `v` in 0..1.
fn unit_vector(u: f64, v: f64) -> Vec3 {
    let z = 1.0 - 2.0 * u;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let a = TAU * v;
    Vec3::new(r * a.cos(), r * a.sin(), z)
}

/// Where particles come from and what they're born with.
#[derive(Clone, Debug, PartialEq)]
pub struct Emitter {
    pub shape: Shape,
    /// Places, turns and scales the shape (and turns the direction).
    /// Animated, it's interpolated between frames for particles born
    /// in between.
    pub transform: Mat4,
    /// Particles per frame from `start` to `end`, evenly spread through
    /// each frame (fractions carry over: 0.5 is one every other frame).
    pub rate: f64,
    /// Particles all born at `start`.
    pub burst: u32,
    /// The frames it emits in (inclusive); [`System::preroll`] moves the
    /// start earlier.
    pub start: i64,
    pub end: i64,
    /// The way particles go (in the emitter's space), how fast (units per
    /// second), give or take `speed_var` (a fraction), within a cone
    /// `spread` degrees around it.
    pub direction: Vec3,
    pub speed: f64,
    pub speed_var: f64,
    pub spread: f64,
    /// Extra speed out of the shape's surface (a sphere's radius, a mesh's
    /// normal, +Z of a rectangle, disk or mask).
    pub along_normal: f64,
    /// Frames each lives, give or take `life_var` (a fraction).
    pub life: f64,
    pub life_var: f64,
    /// The start size, give or take `size_var` (a fraction), and how much
    /// of it is left at the end of life.
    pub size: f64,
    pub size_var: f64,
    pub size_end: f64,
    /// The color at birth, fading to `color_end` over life.
    pub color: [f32; 4],
    pub color_end: [f32; 4],
    /// Starting angle and spin (degrees, degrees per second), each give or
    /// take its `_var` (degrees, degrees per second).
    pub rotation: f64,
    pub rotation_var: f64,
    pub spin: f64,
    pub spin_var: f64,
    pub seed: u64,
}

impl Default for Emitter {
    fn default() -> Self {
        Emitter {
            shape: Shape::Point,
            transform: Mat4::identity(),
            rate: 10.0,
            burst: 0,
            start: 1,
            end: i64::MAX,
            direction: Vec3::y(),
            speed: 1.0,
            speed_var: 0.0,
            spread: 0.0,
            along_normal: 0.0,
            life: 50.0,
            life_var: 0.0,
            size: 0.05,
            size_var: 0.0,
            size_end: 1.0,
            color: [1.0; 4],
            color_end: [1.0; 4],
            rotation: 0.0,
            rotation_var: 0.0,
            spin: 0.0,
            spin_var: 0.0,
            seed: 0,
        }
    }
}

/// What each random number of a particle is for.
#[derive(Clone, Copy)]
enum Draw {
    Shape = 0,
    Cone = 4,
    Speed = 6,
    Life,
    Size,
    Rotation,
    Spin,
    Axis,
}

/// A repeatable random number in 0..1 for (`seed`, `id`, `slot`).
pub(crate) fn random(seed: u64, id: u64, slot: u64) -> f64 {
    let mut h = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ id.rotate_left(21) ^ slot.wrapping_mul(0xd6e8_feb8_6659_fd93);
    for _ in 0..2 {
        // splitmix64's finalizer, twice so nearby ids decorrelate.
        h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        h ^= h >> 31;
    }
    (h >> 11) as f64 / (1u64 << 53) as f64
}

impl Emitter {
    /// Particle `id` as born (age 0), placed by `transform`.
    fn particle(&self, id: u64, transform: &Mat4) -> Option<Particle> {
        let r = |slot: Draw, k: u64| random(self.seed, id, slot as u64 + k);
        let vary = |slot: Draw| 2.0 * r(slot, 0) - 1.0;
        let s = Draw::Shape;
        let (local, normal) = self.shape.sample([r(s, 0), r(s, 1), r(s, 2), r(s, 3)])?;
        let m = transform;
        let position = crate::geom::transform_point(m, &local);
        let turn = |v: Vec3| m.fixed_view::<3, 3>(0, 0) * v;
        let normal = turn(normal).try_normalize(1e-12).unwrap_or_default();
        let dir = turn(self.direction).try_normalize(1e-12).unwrap_or_default();
        let dir = cone(dir, self.spread.to_radians(), r(Draw::Cone, 0), r(Draw::Cone, 1));
        let speed = self.speed * (1.0 + self.speed_var * vary(Draw::Speed));
        let life = (self.life * (1.0 + self.life_var * vary(Draw::Life))).max(0.0);
        let size = (self.size * (1.0 + self.size_var * vary(Draw::Size))).max(0.0);
        Some(Particle {
            id,
            position,
            velocity: dir * speed + normal * self.along_normal,
            accel: Vec3::zeros(),
            age: 0.0,
            life,
            size,
            color: self.color,
            rotation: self.rotation + self.rotation_var * vary(Draw::Rotation),
            spin: self.spin + self.spin_var * vary(Draw::Spin),
            axis: unit_vector(r(Draw::Axis, 0), r(Draw::Axis, 1)),
            start_size: size,
        })
    }

    /// Size and color for `p`'s age.
    fn grow(&self, p: &mut Particle) {
        let t = if p.life > 0.0 { (p.age / p.life).clamp(0.0, 1.0) } else { 1.0 };
        p.size = p.start_size * (1.0 + (self.size_end - 1.0) * t);
        p.color = std::array::from_fn(|i| self.color[i] + (self.color_end[i] - self.color[i]) * t as f32);
    }
}

/// A direction within `angle` (radians) of unit `dir`, evenly over the
/// cap, from `u`, `v` in 0..1. `dir` itself for no angle (or no `dir`).
fn cone(dir: Vec3, angle: f64, u: f64, v: f64) -> Vec3 {
    if angle <= 0.0 || dir == Vec3::zeros() {
        return dir;
    }
    let cos = 1.0 - u * (1.0 - angle.min(PI).cos());
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let a = TAU * v;
    let helper = if dir.x.abs() < 0.9 { Vec3::x() } else { Vec3::y() };
    let e1 = dir.cross(&helper).normalize();
    let e2 = dir.cross(&e1);
    dir * cos + (e1 * a.cos() + e2 * a.sin()) * sin
}

/// A part of space, for killing particles in or out of it.
#[derive(Clone, Debug, PartialEq)]
pub enum Region {
    Sphere { center: Vec3, radius: f64 },
    Box { min: Vec3, max: Vec3 },
}

impl Region {
    pub fn contains(&self, p: &Vec3) -> bool {
        match self {
            Region::Sphere { center, radius } => (p - center).norm() <= *radius,
            Region::Box { min, max } => (0..3).all(|i| min[i] <= p[i] && p[i] <= max[i]),
        }
    }
}

/// What a modifier does. Forces are accelerations (units per second²),
/// scaled by the modifier's strength.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// A constant pull.
    Gravity { accel: Vec3 },
    /// Velocity eased toward `wind` at `drag` per second (`drag` 0: none).
    Drag { wind: Vec3, drag: f64 },
    /// Swirling, incompressible flow (curl noise): `scale` is the size of
    /// its eddies (units), `speed` how fast it changes (per second).
    Turbulence { strength: f64, scale: f64, speed: f64, seed: u64 },
    /// Pulls toward `point` (pushes away with a negative strength), fading
    /// to nothing at `radius` (no fade for radius 0).
    Attractor { point: Vec3, strength: f64, radius: f64 },
    /// Swirls around the line through `center` along `axis`, fading to
    /// nothing at `radius` from it (no fade for radius 0).
    Vortex { center: Vec3, axis: Vec3, strength: f64, radius: f64 },
    /// A floor: particles going through the plane through `point` facing
    /// `normal` bounce back, keeping `restitution` of their speed into it
    /// and losing `friction` (0..1) of their speed along it.
    Bounce { point: Vec3, normal: Vec3, restitution: f64, friction: f64 },
    /// Removes particles older than `age` frames, and/or `inside` (or
    /// outside) `region`.
    Kill { age: Option<f64>, region: Option<(Region, bool)> },
    /// No faster than this (units per second).
    SpeedLimit { max: f64 },
}

/// A force or constraint on every particle, from frame `start` to `end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Modifier {
    pub effect: Effect,
    /// Scales a force (0: off); a constraint acts whenever it's above 0.
    pub strength: f64,
    pub start: i64,
    pub end: i64,
}

impl Modifier {
    /// `effect` at full strength, all the time.
    pub fn new(effect: Effect) -> Modifier {
        Modifier { effect, strength: 1.0, start: i64::MIN, end: i64::MAX }
    }

    fn active(&self, t: f64) -> bool {
        self.strength != 0.0 && self.start as f64 <= t && t <= self.end as f64
    }

    /// The acceleration it gives particle `p` at time `t` (seconds `secs`).
    fn accel(&self, p: &Particle, secs: f64) -> Vec3 {
        let s = self.strength;
        match &self.effect {
            Effect::Gravity { accel } => accel * s,
            Effect::Drag { wind, drag } => (wind - p.velocity) * (drag * s),
            Effect::Turbulence { strength, scale, speed, seed } => {
                let scale = scale.max(1e-9);
                curl(p.position / scale + Vec3::new(0.0, 0.0, speed * secs), *seed) * (strength * s)
            }
            Effect::Attractor { point, strength, radius } => {
                let d = point - p.position;
                let dist = d.norm();
                let fade = if *radius > 0.0 { (1.0 - dist / radius).max(0.0) } else { 1.0 };
                d.try_normalize(1e-12).unwrap_or_default() * (strength * s * fade)
            }
            Effect::Vortex { center, axis, strength, radius } => {
                let Some(axis) = axis.try_normalize(1e-12) else { return Vec3::zeros() };
                let off = p.position - center;
                let off = off - axis * axis.dot(&off);
                let dist = off.norm();
                let fade = if *radius > 0.0 { (1.0 - dist / radius).max(0.0) } else { 1.0 };
                axis.cross(&off).try_normalize(1e-12).unwrap_or_default() * (strength * s * fade)
            }
            _ => Vec3::zeros(),
        }
    }

    /// Constraints, after a move: bounce or limit `p`; false to kill it.
    fn constrain(&self, p: &mut Particle) -> bool {
        match &self.effect {
            Effect::Bounce { point, normal, restitution, friction } => {
                let Some(n) = normal.try_normalize(1e-12) else { return true };
                let depth = (p.position - point).dot(&n);
                let into = p.velocity.dot(&n);
                if depth < 0.0 && into < 0.0 {
                    p.position -= n * (depth * (1.0 + restitution));
                    let along = p.velocity - n * into;
                    p.velocity = along * (1.0 - friction.clamp(0.0, 1.0)) - n * (into * restitution);
                }
                true
            }
            Effect::Kill { age, region } => {
                let old = age.is_some_and(|a| p.age >= a - 1e-9);
                let placed = region.as_ref().is_some_and(|(r, inside)| r.contains(&p.position) == *inside);
                !(old || placed)
            }
            Effect::SpeedLimit { max } => {
                let speed = p.velocity.norm();
                if speed > *max {
                    p.velocity *= max.max(0.0) / speed;
                }
                true
            }
            _ => true,
        }
    }
}

/// Smooth value noise in -1..1 on a unit lattice.
fn noise3(p: Vec3, seed: u64) -> f64 {
    let i = p.map(f64::floor);
    let f = p - i;
    let fade = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let corner = |dx: i64, dy: i64, dz: i64| {
        let key = ((i.x as i64 + dx) as u64).wrapping_mul(73_856_093)
            ^ ((i.y as i64 + dy) as u64).wrapping_mul(19_349_663)
            ^ ((i.z as i64 + dz) as u64).wrapping_mul(83_492_791);
        random(seed, key, 0) * 2.0 - 1.0
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x = |dy, dz| lerp(corner(0, dy, dz), corner(1, dy, dz), fade.x);
    let y = |dz| lerp(x(0, dz), x(1, dz), fade.y);
    lerp(y(0), y(1), fade.z)
}

/// The curl of a noise vector field at `p`: a flow with no sources or
/// sinks, so particles swirl rather than bunch up.
fn curl(p: Vec3, seed: u64) -> Vec3 {
    let field = |q: Vec3| Vec3::new(noise3(q, seed), noise3(q + Vec3::new(31.4, 0.0, 0.0), seed), noise3(q + Vec3::new(0.0, 0.0, 27.1), seed));
    let e = 1e-4;
    let d = |axis: Vec3| (field(p + axis * e) - field(p - axis * e)) / (2.0 * e);
    let (dx, dy, dz) = (d(Vec3::x()), d(Vec3::y()), d(Vec3::z()));
    Vec3::new(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x)
}

/// Everything a simulation needs, as it is at one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct System {
    pub emitters: Vec<Emitter>,
    pub modifiers: Vec<Modifier>,
    /// Frames per second, joining frames to seconds.
    pub fps: f64,
    /// Steps per frame (1 or more), for fast motion and stiff forces.
    pub substeps: u32,
    /// Frames each emitter starts early, so at its start frame its stream
    /// is already going.
    pub preroll: i64,
}

impl Default for System {
    fn default() -> Self {
        System { emitters: vec![], modifiers: vec![], fps: 24.0, substeps: 1, preroll: 0 }
    }
}

/// A system whose params may change over time: what it is at frame `f`.
/// Its emitters' starts, the preroll, fps, substeps and how many emitters
/// and modifiers there are should stay put; the rest can be animated.
pub trait Params {
    fn at(&self, f: i64) -> System;
}

/// A system that doesn't change.
impl Params for System {
    fn at(&self, _: i64) -> System {
        self.clone()
    }
}

impl<F: Fn(i64) -> System> Params for F {
    fn at(&self, f: i64) -> System {
        self(f)
    }
}

/// The particles at a frame, and how far each emitter's rate has got.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub particles: Vec<Particle>,
    /// Per emitter: particles owed by its rate so far (with the fraction).
    emitted: Vec<f64>,
}

impl System {
    /// The last frame with no particles: the simulation starts from here.
    pub fn first_frame(&self) -> i64 {
        self.emitters.iter().map(|e| e.start).min().unwrap_or(0).saturating_sub(self.preroll.saturating_add(1))
    }

    /// Step `state` from frame `f - 1` (where the system was `prev`) to
    /// `f` (where it is `self`). Births use `self`'s emitters, placed by
    /// their transforms interpolated from `prev`'s; forces and constraints
    /// use `prev`'s modifiers (as they are at the start of the step).
    pub fn step(&self, prev: &System, state: &mut State, f: i64) {
        let n = self.substeps.max(1);
        let (t0, t1) = (f as f64 - 1.0, f as f64);
        let fps = self.fps.max(1e-6);
        state.emitted.resize(self.emitters.len(), 0.0);

        // Births in (t0, t1], each with its time.
        let mut births = vec![];
        for (i, e) in self.emitters.iter().enumerate() {
            let begin = e.start.saturating_sub(self.preroll);
            if f < begin || f > e.end {
                continue;
            }
            let from = prev.emitters.get(i).map_or(&e.transform, |p| &p.transform);
            let place = |t: f64| from + (e.transform - from) * (t - t0);
            let base = (i as u64) << 32;
            if f == begin {
                births.extend((0..e.burst as u64).filter_map(|k| Some((t1, e.particle(base + k, &place(t1))?))));
            }
            // The rate's k-th particle is born as its running count passes k.
            let (owed, rate) = (state.emitted[i], e.rate.max(0.0));
            state.emitted[i] = owed + rate;
            let done = (owed + 1e-9).floor() as u64;
            for k in done + 1..=((owed + rate + 1e-9).floor() as u64) {
                let t = (t0 + (k as f64 - owed) / rate).min(t1);
                if let Some(p) = e.particle(base + e.burst as u64 + k - 1, &place(t)) {
                    births.push((t, p));
                }
            }
        }
        births.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut births = births.into_iter().peekable();

        for k in 0..n {
            let ts = t0 + k as f64 / n as f64;
            let te = if k + 1 == n { t1 } else { t0 + (k + 1) as f64 / n as f64 };
            // Those born in this substep move for what's left of it.
            let mut moving: Vec<(f64, Particle)> = std::mem::take(&mut state.particles).into_iter().map(|p| (ts, p)).collect();
            while let Some(b) = births.next_if(|(t, _)| *t <= te || k + 1 == n) {
                moving.push(b);
            }
            for (from, mut p) in moving {
                let dt = te - from;
                if dt > 0.0 {
                    let secs = dt / fps;
                    let a: Vec3 = prev.modifiers.iter().filter(|m| m.active(from)).map(|m| m.accel(&p, from / fps)).sum();
                    p.position += p.velocity * secs + a * (0.5 * secs * secs);
                    p.velocity += a * secs;
                    p.accel = a;
                    p.rotation += p.spin * secs;
                    p.age += dt;
                }
                if p.age >= p.life - 1e-9 || !prev.modifiers.iter().filter(|m| m.active(te)).all(|m| m.constrain(&mut p)) {
                    continue;
                }
                if let Some(e) = self.emitters.get((p.id >> 32) as usize) {
                    e.grow(&mut p);
                }
                state.particles.push(p);
            }
        }
    }

    /// The first frame whose particles change when the system as sampled
    /// at frame `f` goes from `old` to `self` (None if nothing does). An
    /// emitter changes frame `f` on from when it has started; a modifier
    /// changes the steps after `f` while it acts; anything else changes
    /// everything.
    pub fn changed_at(&self, old: &System, f: i64) -> Option<i64> {
        if self == old {
            return None;
        }
        if (self.fps, self.substeps, self.preroll) != (old.fps, old.substeps, old.preroll)
            || self.emitters.len() != old.emitters.len()
            || self.modifiers.len() != old.modifiers.len()
            || self.emitters.iter().zip(&old.emitters).any(|(a, b)| a.start != b.start)
        {
            return Some(i64::MIN);
        }
        let begun = self.emitters.iter().zip(&old.emitters).any(|(a, b)| a != b && f >= a.start.saturating_sub(self.preroll));
        // Used for the step from f to f + 1: forces from time f, before
        // f + 1; constraints after each substep, up to f + 1.
        let acts = |m: &Modifier| {
            let constraint = matches!(m.effect, Effect::Bounce { .. } | Effect::Kill { .. } | Effect::SpeedLimit { .. });
            m.strength != 0.0 && m.start <= if constraint { f.saturating_add(1) } else { f } && f <= m.end
        };
        let pushes = self.modifiers.iter().zip(&old.modifiers).any(|(a, b)| a != b && (acts(a) || acts(b)));
        if begun {
            Some(f)
        } else if pushes {
            Some(f + 1)
        } else {
            None
        }
    }
}

/// Simulated frames, so scrubbing back is a lookup and forward only
/// simulates what's missing.
#[derive(Default)]
pub struct Cache {
    /// Per frame: the system as sampled there, and the state.
    frames: BTreeMap<i64, (System, State)>,
    /// Frames simulated so far (for tests and stats).
    pub steps: usize,
}

impl Cache {
    /// The particles at frame `f` of the system `params` gives. Frames
    /// held from before are checked against `params` first: from the first
    /// one whose params changed in a way that matters, they're simulated
    /// again.
    pub fn frame(&mut self, params: &impl Params, f: i64) -> &[Particle] {
        let now = params.at(f);
        let first = now.first_frame();
        if f <= first {
            return &[];
        }
        if self.frames.first_key_value().is_some_and(|(&k, _)| k != first) {
            self.frames.clear();
        }
        // Frames that stand keep the fresh params too: the next step
        // starts from them.
        let mut stale = None;
        for (&g, (old, _)) in self.frames.range_mut(..=f) {
            let new = if g == f { now.clone() } else { params.at(g) };
            stale = new.changed_at(old, g);
            *old = new;
            if stale.is_some() {
                break;
            }
        }
        if let Some(from) = stale {
            self.invalidate_from(from);
        }
        if !self.frames.contains_key(&f) {
            let (mut at, (mut prev, mut state)) = match self.frames.range(..f).next_back() {
                Some((&at, held)) => (at, held.clone()),
                None => (first, (params.at(first), State::default())),
            };
            self.frames.entry(first).or_insert_with(|| (prev.clone(), State::default()));
            while at < f {
                at += 1;
                let cur = if at == f { now.clone() } else { params.at(at) };
                cur.step(&prev, &mut state, at);
                self.steps += 1;
                self.frames.insert(at, (cur.clone(), state.clone()));
                prev = cur;
            }
        }
        &self.frames[&f].1.particles
    }

    /// Forget frame `f` and every one after it.
    pub fn invalidate_from(&mut self, f: i64) {
        self.frames.split_off(&f);
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// The frames held (the empty one before the first included).
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Particles from a point at the origin.
    fn point(rate: f64) -> Emitter {
        Emitter { rate, life: f64::INFINITY, ..Emitter::default() }
    }

    fn system(emitters: Vec<Emitter>, modifiers: Vec<Modifier>) -> System {
        System { emitters, modifiers, ..System::default() }
    }

    fn at(sys: &impl Params, f: i64) -> Vec<Particle> {
        Cache::default().frame(sys, f).to_vec()
    }

    #[test]
    fn the_same_seed_gives_the_same_particles() {
        let e = Emitter { speed: 2.0, speed_var: 0.5, spread: 40.0, life: 30.0, life_var: 0.3, size_var: 0.4, ..Emitter::default() };
        let turb = Modifier::new(Effect::Turbulence { strength: 3.0, scale: 0.5, speed: 1.0, seed: 4 });
        let a = system(vec![e.clone()], vec![turb.clone()]);
        assert!(at(&a, 20) == at(&a, 20));
        assert!(!at(&a, 20).is_empty());
        let b = system(vec![Emitter { seed: 1, ..e }], vec![turb]);
        assert!(at(&a, 20) != at(&b, 20), "another seed, other particles");
        // Stepped one frame at a time or straight to the end: the same.
        let mut c = Cache::default();
        for f in 1..20 {
            c.frame(&a, f);
        }
        assert!(c.frame(&a, 20) == at(&a, 20));
    }

    #[test]
    fn rates_and_bursts_emit_the_right_counts() {
        let e = Emitter { start: 5, ..point(3.0) };
        let sys = system(vec![e.clone()], vec![]);
        for f in [3, 4, 5, 6, 12] {
            assert_eq!(at(&sys, f).len() as i64, 3 * (f - 4).max(0), "frame {f}");
        }
        // Fractions carry over; nothing after the end.
        let half = system(vec![Emitter { rate: 0.5, end: 10, ..e.clone() }], vec![]);
        assert_eq!([5, 6, 7, 10, 20].map(|f| at(&half, f).len()), [0, 1, 1, 3, 3]);
        // A burst once, at the start; preroll moves it (and the rate) back.
        let burst = system(vec![Emitter { burst: 7, ..point(0.0) }], vec![]);
        assert_eq!([0, 1, 2, 9].map(|f| at(&burst, f).len()), [0, 7, 7, 7]);
        let pre = System { preroll: 3, ..system(vec![e], vec![]) };
        assert_eq!(at(&pre, 5).len(), 12, "already four frames in at its start");
        // Deaths by lifetime: life 5 at 2 a frame holds 10.
        let lived = system(vec![Emitter { life: 5.0, ..point(2.0) }], vec![]);
        assert_eq!([3, 5, 6, 30].map(|f| at(&lived, f).len()), [6, 10, 10, 10]);
        // Every id is different.
        let mut ids: Vec<u64> = at(&sys, 12).iter().map(|p| p.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 24);
    }

    #[test]
    fn gravity_follows_the_closed_form() {
        let g = 9.8;
        let gravity = Modifier::new(Effect::Gravity { accel: Vec3::new(0.0, -g, 0.0) });
        // A stream (births in mid-frame too) thrown up and sideways at 3.
        let e = Emitter {
            rate: 4.0,
            transform: crate::geom::trs([0.0, 2.0, 0.0], [0.0; 3], [1.0; 3]),
            direction: Vec3::new(1.0, 1.0, 0.0),
            speed: 3.0 * 2f64.sqrt(),
            ..point(0.0)
        };
        for substeps in [1, 3, 8] {
            let sys = System { substeps, ..system(vec![e.clone()], vec![gravity.clone()]) };
            let f = 30;
            let ps = at(&sys, f);
            assert_eq!(ps.len(), 120);
            for p in &ps {
                let t = p.age / sys.fps;
                let y = 2.0 + 3.0 * t - 0.5 * g * t * t;
                assert!((p.position.y - y).abs() < 1e-6 && (p.position.x - 3.0 * t).abs() < 1e-6, "substeps {substeps}: {p:?}");
                assert!((p.velocity.y - (3.0 - g * t)).abs() < 1e-6);
            }
            // The k-th is born at 1 - 1 + (k + 1) / 4 (frames).
            let ages: Vec<f64> = ps.iter().map(|p| p.age).collect();
            assert!((ages[0] - 29.75).abs() < 1e-9 && ages[119].abs() < 1e-9, "{:?}", &ages[..2]);
        }
    }

    #[test]
    fn bounces_kills_and_limits() {
        // Coming down at 2 (and along at 1) from 0.05 above a floor.
        let e = Emitter {
            burst: 1,
            transform: crate::geom::trs([0.0, 0.05, 0.0], [0.0; 3], [1.0; 3]),
            direction: Vec3::new(1.0, -2.0, 0.0),
            speed: 5f64.sqrt(),
            ..point(0.0)
        };
        let floor = Modifier::new(Effect::Bounce { point: Vec3::zeros(), normal: Vec3::y(), restitution: 0.5, friction: 0.25 });
        let sys = System { fps: 10.0, ..system(vec![e.clone()], vec![floor]) };
        let p = &at(&sys, 2)[0];
        assert!((p.velocity - Vec3::new(0.75, 1.0, 0.0)).norm() < 1e-12, "{:?}", p.velocity);
        // Went 0.2 down in the frame: 0.15 under, and back up half that.
        assert!((p.position.y - 0.075).abs() < 1e-12 && p.position.y > 0.0, "{:?}", p.position);

        // Killed at age 3: there at frames 1–3, gone at 4.
        let old = Modifier::new(Effect::Kill { age: Some(3.0), region: None });
        let sys = system(vec![Emitter { burst: 5, ..point(0.0) }], vec![old]);
        assert_eq!([1, 3, 4].map(|f| at(&sys, f).len()), [5, 5, 0]);
        // A lifetime does the same.
        let sys = system(vec![Emitter { burst: 5, life: 3.0, ..point(0.0) }], vec![]);
        assert_eq!([1, 3, 4].map(|f| at(&sys, f).len()), [5, 5, 0]);
        // Killed once out of a sphere of radius 1 (moving at 6/s = 0.25 a frame).
        let out = Modifier::new(Effect::Kill { age: None, region: Some((Region::Sphere { center: Vec3::zeros(), radius: 1.0 }, false)) });
        let sys = system(vec![Emitter { burst: 1, speed: 6.0, ..point(0.0) }], vec![out]);
        assert_eq!([4, 6].map(|f| at(&sys, f).len()), [1, 0]);

        // A speed limit, after gravity.
        let fall = Modifier::new(Effect::Gravity { accel: Vec3::new(0.0, -100.0, 0.0) });
        let limit = Modifier::new(Effect::SpeedLimit { max: 2.0 });
        let sys = system(vec![Emitter { burst: 1, ..point(0.0) }], vec![fall, limit]);
        assert!((at(&sys, 10)[0].velocity.norm() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn forces_push_the_right_way() {
        let still = Emitter { burst: 1, speed: 0.0, transform: crate::geom::trs([2.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..point(0.0) };
        let run_to = |m: Effect, f: i64| at(&system(vec![still.clone()], vec![Modifier::new(m)]), f)[0].clone();
        let run = |m: Effect| run_to(m, 5);
        let p = run(Effect::Attractor { point: Vec3::zeros(), strength: 4.0, radius: 0.0 });
        assert!(p.position.x < 2.0 && p.position.y.abs() < 1e-12);
        let p = run(Effect::Attractor { point: Vec3::zeros(), strength: -4.0, radius: 0.0 });
        assert!(p.position.x > 2.0);
        let p = run(Effect::Attractor { point: Vec3::zeros(), strength: 4.0, radius: 1.5 });
        assert_eq!(p.position.x, 2.0, "out of reach");
        // A vortex about Y turns it around Y: +X goes toward -Z.
        let p = run_to(Effect::Vortex { center: Vec3::zeros(), axis: Vec3::y(), strength: 4.0, radius: 0.0 }, 2);
        assert!(p.velocity.z < 0.0 && p.velocity.x.abs() < 1e-12 && p.velocity.y == 0.0);
        let p = run(Effect::Vortex { center: Vec3::zeros(), axis: Vec3::y(), strength: 4.0, radius: 0.0 });
        assert!(p.position.z < 0.0 && ((p.position - Vec3::new(0.0, 0.0, 0.0)).norm() - 2.0).abs() < 0.1, "round about");
        // Drag eases toward the wind.
        let p = run(Effect::Drag { wind: Vec3::new(0.0, 0.0, 3.0), drag: 2.0 });
        assert!(p.velocity.z > 0.0 && p.velocity.z < 3.0);
        // Turbulence: the flow has no sources or sinks, and it moves things.
        let div = |q: Vec3| {
            let e = 1e-3;
            (0..3).map(|i| {
                let mut d = Vec3::zeros();
                d[i] = e;
                (curl(q + d, 9)[i] - curl(q - d, 9)[i]) / (2.0 * e)
            }).sum::<f64>()
        };
        for q in [Vec3::new(0.3, 1.7, -2.2), Vec3::new(5.5, 0.1, 0.9)] {
            assert!(div(q).abs() < 1e-3, "divergence {}", div(q));
            assert!(curl(q, 9).norm() > 1e-3);
        }
        let p = run(Effect::Turbulence { strength: 2.0, scale: 1.0, speed: 0.5, seed: 9 });
        assert!(p.velocity.norm() > 0.0);
    }

    #[test]
    fn size_color_and_spin_change_over_life() {
        let e = Emitter {
            burst: 1,
            life: 11.0,
            size: 2.0,
            size_end: 0.5,
            color: [1.0, 0.0, 0.0, 1.0],
            color_end: [0.0, 0.0, 1.0, 0.0],
            spin: 90.0,
            ..point(0.0)
        };
        let sys = System { fps: 10.0, ..system(vec![e], vec![]) };
        let p = &at(&sys, 6)[0];
        assert_eq!(p.age, 5.0);
        let t = 5.0 / 11.0;
        assert!((p.size - 2.0 * (1.0 - 0.5 * t)).abs() < 1e-12);
        assert!((p.color[0] - (1.0 - t as f32)).abs() < 1e-6 && (p.color[2] - t as f32).abs() < 1e-6);
        assert!((p.rotation - 45.0).abs() < 1e-9, "half a second at 90°/s");
    }

    #[test]
    fn shapes_emit_where_they_are() {
        let sample = |shape: Shape| -> Vec<(Vec3, Vec3)> {
            (0..400).map(|i| shape.sample(std::array::from_fn(|k| random(3, i, k as u64))).unwrap()).collect()
        };
        assert!(sample(Shape::Line { length: 2.0 }).iter().all(|(p, _)| p.x.abs() <= 1.0 && p.y == 0.0 && p.z == 0.0));
        assert!(sample(Shape::Rect { width: 2.0, height: 4.0 }).iter().all(|(p, n)| p.x.abs() <= 1.0 && p.y.abs() <= 2.0 && *n == Vec3::z()));
        assert!(sample(Shape::Disk { radius: 3.0 }).iter().all(|(p, _)| p.norm() <= 3.0 && p.z == 0.0));
        assert!(sample(Shape::Sphere { radius: 2.0, surface: true }).iter().all(|(p, n)| (p.norm() - 2.0).abs() < 1e-12 && (p / 2.0 - n).norm() < 1e-12));
        assert!(sample(Shape::Sphere { radius: 2.0, surface: false }).iter().all(|(p, _)| p.norm() <= 2.0));
        let size = Vec3::new(1.0, 2.0, 4.0);
        let faces = sample(Shape::Box { size, surface: true });
        assert!(faces.iter().all(|(p, n)| {
            let i = n.iamax();
            (p[i] - n[i] * size[i] / 2.0).abs() < 1e-12 && (0..3).all(|k| p[k].abs() <= size[k] / 2.0 + 1e-12)
        }));
        // Every face gets some, the big ones most (the 2×4 faces are X's).
        let on_x = faces.iter().filter(|(_, n)| n.x != 0.0).count();
        assert!(on_x > faces.iter().filter(|(_, n)| n.z != 0.0).count() && faces.iter().any(|(_, n)| n.z < 0.0));

        // A mesh: one big triangle at z = 1 and a tiny one at z = -1.
        let mesh = Arc::new(Mesh {
            positions: vec![Vec3::new(0.0, 0.0, 1.0), Vec3::new(4.0, 0.0, 1.0), Vec3::new(0.0, 4.0, 1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.01, 0.0, -1.0), Vec3::new(0.0, 0.01, -1.0)],
            triangles: vec![[0, 1, 2], [3, 4, 5]],
            ..Mesh::default()
        });
        let on = sample(Shape::mesh_surface(mesh.clone()));
        assert!(on.iter().all(|(p, n)| p.z == 1.0 && p.x + p.y <= 4.0 + 1e-9 && *n == Vec3::z()), "by area: all on the big one");
        assert!(sample(Shape::MeshVertices(mesh.clone())).iter().all(|(p, _)| mesh.positions.contains(p)));
        assert_eq!(Shape::mesh_surface(Arc::new(Mesh::default())).sample([0.5; 4]), None);

        // A mask: only where the alpha is.
        let mut img = Image::filled(8, 6, [0.0; 4]);
        img.x0 = 10;
        img.pixels[2 * 8 + 3] = [1.0; 4];
        img.pixels[4 * 8 + 6] = [0.0, 0.0, 0.0, 0.25];
        let hits = sample(Shape::mask(&img));
        assert!(hits.iter().all(|(p, _)| (p.x.floor(), p.y.floor()) == (13.0, 2.0) || (p.x.floor(), p.y.floor()) == (16.0, 4.0)));
        let dim = hits.iter().filter(|(p, _)| p.y.floor() == 4.0).count();
        assert!((40..120).contains(&dim), "a fifth of them: {dim}");
    }

    #[test]
    fn velocities_stay_in_the_cone() {
        let e = Emitter { burst: 300, spread: 20.0, speed: 2.0, direction: Vec3::new(0.0, 0.0, -3.0), ..point(0.0) };
        let ps = at(&system(vec![e], vec![]), 1);
        let cos = 20f64.to_radians().cos();
        assert!(ps.iter().all(|p| (p.velocity.norm() - 2.0).abs() < 1e-12 && -p.velocity.z / 2.0 >= cos - 1e-12));
        assert!(ps.iter().any(|p| -p.velocity.z / 2.0 < 0.95), "spread out, not all straight");
    }

    #[test]
    fn the_cache_keeps_frames_and_resimulates_only_what_changed() {
        let gravity = |g: f64| Modifier { start: 10, ..Modifier::new(Effect::Gravity { accel: Vec3::new(0.0, -g, 0.0) }) };
        let a = system(vec![point(2.0)], vec![gravity(9.8)]);
        let mut cache = Cache::default();
        let at20 = cache.frame(&a, 20).to_vec();
        assert_eq!(cache.steps, 20);
        // Back is a lookup; forward simulates only what's missing.
        assert!(cache.frame(&a, 5) == at(&a, 5));
        cache.frame(&a, 25);
        assert_eq!(cache.steps, 25);
        // Gravity from frame 10 changed: frames to 10 stand, 11 on redone.
        let b = system(vec![point(2.0)], vec![gravity(1.0)]);
        let fresh = at(&b, 20);
        assert!(cache.frame(&b, 20) == fresh);
        assert_eq!(cache.steps, 35);
        assert!(fresh != at20);
        assert!(cache.frame(&b, 10) == at(&a, 10));
        // An emitter change redoes it all; invalidating by hand drops the rest.
        let c = system(vec![point(3.0)], vec![gravity(1.0)]);
        cache.frame(&c, 4);
        assert_eq!(cache.steps, 39);
        cache.invalidate_from(3);
        assert_eq!(cache.len(), 3, "frames 0 (empty), 1 and 2");
    }

    #[test]
    fn animated_params_are_sampled_each_frame() {
        // Moving along X at 1 a frame, emitting from frame 5 at 2 a frame.
        let moving = |f: i64| {
            let e = Emitter {
                transform: crate::geom::trs([f as f64, 0.0, 0.0], [0.0; 3], [1.0; 3]),
                rate: if f >= 5 { 2.0 } else { 0.0 },
                speed: 0.0,
                ..point(0.0)
            };
            system(vec![e], vec![])
        };
        let ps = at(&moving, 8);
        assert_eq!(ps.len(), 8);
        // Born between frames, placed between where the emitter was.
        for p in &ps {
            assert!((p.position.x - (8.0 - p.age)).abs() < 1e-9, "{p:?}");
        }
        // A key changed at frame 12 leaves the frames before it cached.
        let edited = |f: i64| {
            let mut s = moving(f);
            if f >= 12 {
                s.emitters[0].rate = 5.0;
            }
            s
        };
        let mut cache = Cache::default();
        cache.frame(&moving, 15);
        assert_eq!(cache.steps, 15);
        assert!(cache.frame(&edited, 15) == at(&edited, 15));
        assert_eq!(cache.steps, 19, "frames 12 to 15 again");
        assert_eq!(at(&edited, 15).len(), 7 * 2 + 4 * 5);
    }
}
