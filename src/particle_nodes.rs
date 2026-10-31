//! Particle nodes: Particle Emitter, the forces and constraints
//! (Gravity, Wind, Turbulence, Attractor, Vortex, Bounce, Kill, Speed
//! Limit), Particle Render (how they look) and Particles 2D.
//!
//! The nodes are lazy: each passes on a [`ParticleStream`], a description
//! of its particle system as a function of the frame (its upstream's,
//! plus its own emitter or modifier with its params at that frame), and
//! simulates nothing. Whatever draws the particles asks the stream for
//! them ([`ParticleStream::particles`]), and the stream's cache simulates
//! only what it hasn't yet (see [`crate::particles::Cache`]).
//!
//! A node's params at any frame come from `own`, which the graph builds
//! (keyframes, links and expressions resolved there), so animated params
//! work at every frame the simulation steps through. Shapes from inputs
//! (geometry to emit from, a mask) are taken at the frame being shown and
//! kept while the input is unchanged ([`ParticleState`]); an animated
//! input means simulating again from the start.

use crate::geom::{self, Vec3};
use crate::image::{Image, Size};
use crate::mesh::{Material, Mesh};
use crate::ops::{Category, DefaultValue, Kind, OpInfo, Overlay, ParamKind, ParamSpec, Port};
use crate::particles::{Cache, Effect, Emitter, Modifier, Particle, Region, Shape, System};
use crate::particles_render::{self, Blend, Orient, Outline, Pick, Sprites, Texture};
use crate::scene::Scene;
use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// What particle ports carry.
const STREAM: Kind = Kind::Particles;

const fn port(label: &'static str, optional: bool, kind: Kind) -> Port {
    Port { label, optional, kind }
}

const PARTICLES_IN: Port = port("particles", false, STREAM);

macro_rules! p {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: DefaultValue::Fn(|| json!($default)), animatable: $kind.is_numeric() }
    };
}

macro_rules! fixed {
    ($name:literal, $label:literal, $kind:expr, $default:expr) => {
        ParamSpec { name: $name, label: $label, kind: $kind, default: DefaultValue::Fn(|| json!($default)), animatable: false }
    };
}

const FRAME: ParamKind = ParamKind::Int { min: -100_000, max: 1_000_000 };
/// Start / end frames that mean "always".
const ALWAYS_FROM: i64 = -100_000;
const ALWAYS_TO: i64 = 1_000_000;
const AMOUNT: ParamKind = ParamKind::Float { min: 0.0, max: 100.0 };
const FRACTION: ParamKind = ParamKind::Float { min: 0.0, max: 1.0 };
const SIGNED: ParamKind = ParamKind::Float { min: -100.0, max: 100.0 };
const ANGLE: ParamKind = ParamKind::Float { min: -360.0, max: 360.0 };

const SHAPES: &[&str] = &["point", "line", "rectangle", "disk", "sphere", "box", "geometry surface", "geometry vertices", "point set", "mask"];
const REGIONS: &[&str] = &["none", "inside sphere", "outside sphere", "inside box", "outside box"];
const LOOKS: &[&str] = &["points", "sprites", "objects"];
const OUTLINES: &[&str] = &["round", "square"];
const BLENDS: &[&str] = &["over", "additive"];
const ORIENTS: &[&str] = &["none", "velocity", "spin"];
const PICKS: &[&str] = &["by age", "cycle", "play", "loop"];
const TEXTURE_FRAMES: &[&str] = &["current frame", "sequence"];
/// The most frames a texture sequence takes.
pub const MAX_SEQUENCE: i64 = 1000;
/// The most frames an emitter takes its geometry (or mask) at.
pub const MAX_HISTORY: i64 = 100_000;

const STRENGTH: ParamSpec = p!("strength", "Strength", SIGNED, 1.0);
const START: ParamSpec = p!("start", "Start frame", FRAME, ALWAYS_FROM);
const END: ParamSpec = p!("end", "End frame", FRAME, ALWAYS_TO);

/// A modifier node's spec: its own params, then strength and frames.
macro_rules! modifier {
    ($name:literal, $label:literal, $help:literal, [$($param:expr),* $(,)?]) => {
        OpInfo {
            name: $name,
            label: $label,
            category: Category::Scene,
            inputs: &[PARTICLES_IN],
            params: &[$($param,)* STRENGTH, START, END],
            mask_base: None,
            overlay: Overlay::None,
            help: concat!($help, " Strength scales it (0: off); it acts from Start frame to End frame."),
        }
    };
}

/// The particle nodes.
pub static OPS: &[OpInfo] = &[
    OpInfo {
        name: "particle_emitter",
        label: "Particle Emitter",
        category: Category::Scene,
        inputs: &[port("geometry", true, Kind::Scene), port("mask", true, Kind::Image)],
        params: &[
            fixed!("shape", "Shape", ParamKind::Choice(SHAPES), "point"),
            p!("dimensions", "Dimensions", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            fixed!("surface", "On the surface", ParamKind::Bool, false),
            fixed!("each_point_once", "Each point once", ParamKind::Bool, true),
            p!("translate", "Translate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("rotate", "Rotate", ParamKind::Vec3, [0.0, 0.0, 0.0]),
            p!("scale", "Scale", ParamKind::Vec3, [1.0, 1.0, 1.0]),
            p!("rate", "Rate (per frame)", ParamKind::Float { min: 0.0, max: 10_000.0 }, 10.0),
            p!("burst", "Burst", ParamKind::Int { min: 0, max: 1_000_000 }, 0),
            p!("start", "Start frame", FRAME, 1),
            p!("end", "End frame", FRAME, ALWAYS_TO),
            p!("direction", "Direction", ParamKind::Vec3, [0.0, 1.0, 0.0]),
            p!("speed", "Speed", ParamKind::Float { min: 0.0, max: 1000.0 }, 1.0),
            p!("speed_var", "Speed variation", FRACTION, 0.0),
            p!("spread", "Spread (degrees)", ParamKind::Float { min: 0.0, max: 180.0 }, 0.0),
            p!("along_normal", "Speed along normal", SIGNED, 0.0),
            p!("life", "Life (frames)", ParamKind::Float { min: 0.0, max: 100_000.0 }, 50.0),
            p!("life_var", "Life variation", FRACTION, 0.0),
            p!("size", "Size", ParamKind::Float { min: 0.0, max: 1000.0 }, 0.05),
            p!("size_var", "Size variation", FRACTION, 0.0),
            p!("size_end", "Size at end of life", ParamKind::Float { min: 0.0, max: 10.0 }, 1.0),
            p!("color", "Color", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("color_end", "Color at end of life", ParamKind::Color, [1.0, 1.0, 1.0, 1.0]),
            p!("rotation", "Rotation", ANGLE, 0.0),
            p!("rotation_var", "Rotation variation", ANGLE, 0.0),
            p!("spin", "Spin (degrees/s)", ParamKind::Float { min: -3600.0, max: 3600.0 }, 0.0),
            p!("spin_var", "Spin variation", ParamKind::Float { min: 0.0, max: 3600.0 }, 0.0),
            fixed!("seed", "Seed", ParamKind::Int { min: 0, max: 1_000_000 }, 0),
            fixed!("substeps", "Substeps", ParamKind::Int { min: 1, max: 32 }, 1),
            fixed!("preroll", "Pre-roll (frames)", ParamKind::Int { min: 0, max: 10_000 }, 0),
        ],
        mask_base: None,
        overlay: Overlay::None,
        help: "Gives birth to particles: Rate a frame (fractions carry over) from Start frame to End frame, and a \
               Burst all at once at the start. They come from the Shape: a point, a line (Dimensions x long), a \
               rectangle (x by y), a disk or sphere (x across), a box (x, y, z), the surface or vertices of the \
               geometry input, its point sets (point set: Points to Particles, one particle at each point at the start \
               with Each point once, else Rate picking points at random; a point's v, pscale, Cd, life and orient \
               attributes give its particle's velocity, size, color, life and turn, and Particles to Points gives its \
               other attributes back), or where the mask input's alpha is (frame pixels, for Particles 2D). On the surface \
               emits from a sphere's or box's skin rather than inside it. Translate, Rotate and Scale place the shape. \
               Each leaves along Direction at Speed (give or take Speed variation, a fraction) within a cone Spread \
               degrees around it, plus Speed along normal out of the shape's surface; units per second. Life (frames, \
               give or take its variation), Size and Color change to their end-of-life values; Rotation and Spin turn \
               sprites and objects. The same Seed always gives the same particles. Substeps split each frame for fast \
               motion; Pre-roll starts it that many frames early.",
    },
    modifier!("particle_gravity", "Particle Gravity", "Pulls particles along Acceleration (units per second²).", [p!(
        "acceleration",
        "Acceleration",
        ParamKind::Vec3,
        [0.0, -9.8, 0.0]
    )]),
    modifier!("particle_wind", "Particle Wind", "Eases particles' velocity toward Wind, Drag of the difference a second (Wind 0: plain drag).", [
        p!("wind", "Wind", ParamKind::Vec3, [0.0, 0.0, 0.0]),
        p!("drag", "Drag", AMOUNT, 1.0),
    ]),
    modifier!("particle_turbulence", "Particle Turbulence", "Swirls particles in an ever-changing flow (curl noise) with eddies Scale units across, changing at Speed, pushing at Amount.", [
        p!("amount", "Amount", AMOUNT, 1.0),
        p!("scale", "Scale", ParamKind::Float { min: 0.001, max: 1000.0 }, 1.0),
        p!("speed", "Speed", AMOUNT, 0.5),
        fixed!("seed", "Seed", ParamKind::Int { min: 0, max: 1_000_000 }, 0),
    ]),
    modifier!("particle_attractor", "Particle Attractor", "Pulls particles toward Position at Amount (negative pushes), fading to nothing at Radius (0: everywhere).", [
        p!("position", "Position", ParamKind::Vec3, [0.0, 0.0, 0.0]),
        p!("amount", "Amount", SIGNED, 1.0),
        p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 10_000.0 }, 0.0),
    ]),
    modifier!("particle_vortex", "Particle Vortex", "Swirls particles around the line through Center along Axis at Amount, fading to nothing at Radius from it (0: everywhere).", [
        p!("center", "Center", ParamKind::Vec3, [0.0, 0.0, 0.0]),
        p!("axis", "Axis", ParamKind::Vec3, [0.0, 1.0, 0.0]),
        p!("amount", "Amount", SIGNED, 1.0),
        p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 10_000.0 }, 0.0),
    ]),
    modifier!("particle_bounce", "Particle Bounce", "A floor: particles reaching the plane through Point facing Normal bounce back, keeping Bounce of their speed into it and losing Friction of their speed along it.", [
        p!("point", "Point", ParamKind::Vec3, [0.0, 0.0, 0.0]),
        p!("normal", "Normal", ParamKind::Vec3, [0.0, 1.0, 0.0]),
        p!("bounce", "Bounce", FRACTION, 0.5),
        p!("friction", "Friction", FRACTION, 0.1),
    ]),
    modifier!("particle_kill", "Particle Kill", "Removes particles older than Age frames (0: no age limit), and those inside or outside a sphere (Radius) or box (Size) at Center.", [
        p!("age", "Age (frames)", ParamKind::Float { min: 0.0, max: 100_000.0 }, 0.0),
        fixed!("region", "Region", ParamKind::Choice(REGIONS), "none"),
        p!("center", "Center", ParamKind::Vec3, [0.0, 0.0, 0.0]),
        p!("radius", "Radius", ParamKind::Float { min: 0.0, max: 10_000.0 }, 1.0),
        p!("size", "Size", ParamKind::Vec3, [1.0, 1.0, 1.0]),
    ]),
    modifier!("particle_speed_limit", "Particle Speed Limit", "Slows particles to at most Max speed (units per second).", [p!(
        "max",
        "Max speed",
        ParamKind::Float { min: 0.0, max: 10_000.0 },
        5.0
    )]),
    OpInfo {
        name: "particle_render",
        label: "Particle Render",
        category: Category::Scene,
        inputs: &[PARTICLES_IN, port("texture", true, Kind::Image), port("objects", true, Kind::Scene)],
        params: &[
            fixed!("look", "Look", ParamKind::Choice(LOOKS), "sprites"),
            fixed!("outline", "Outline", ParamKind::Choice(OUTLINES), "round"),
            fixed!("blend", "Blend", ParamKind::Choice(BLENDS), "over"),
            p!("scale", "Size scale", ParamKind::Float { min: 0.0, max: 100.0 }, 1.0),
            p!("opacity", "Opacity", FRACTION, 1.0),
            p!("opacity_end", "Opacity at end of life", FRACTION, 1.0),
            fixed!("texture_frames", "Texture frames", ParamKind::Choice(TEXTURE_FRAMES), "current frame"),
            fixed!("sequence_first", "Sequence first frame", FRAME, 1),
            fixed!("sequence_last", "Sequence last frame", FRAME, 24),
            fixed!("pick", "Texture images", ParamKind::Choice(PICKS), "by age"),
            p!("cycle_rate", "Rate (images a frame)", ParamKind::Float { min: 0.0, max: 100.0 }, 1.0),
            fixed!("random_start", "Random start", ParamKind::Bool, false),
            fixed!("orient", "Objects face", ParamKind::Choice(ORIENTS), "velocity"),
        ],
        mask_base: None,
        overlay: Overlay::None,
        help: "How particles look where they're rendered. Points: small dots. Sprites: squares or disks facing the \
               camera, Size scale × each particle's size across, tinted by its color, textured by the texture input, \
               faded from Opacity to Opacity at end of life, laid Over each other or Additive (for sparks and fire). \
               Texture frames: the texture input at the frame rendered, or a sequence of it (its frames Sequence first \
               frame to Sequence last frame), each sprite showing an image of it as Texture images says: by age (the \
               sequence once over its life), cycle (round and round at Rate images a frame, each particle starting on \
               its own), play (from the first at Rate images a frame as it ages, holding the last) or loop (the same, \
               round and round); with Random start, play and loop start each particle on an image of its own. \
               Objects: a copy of the objects input's meshes at each particle (one at random per particle when there \
               are several), scaled by its size and facing along its velocity, spinning, or as modelled.",
    },
    OpInfo {
        name: "particles_2d",
        label: "Particles 2D",
        category: Category::Scene,
        inputs: &[PARTICLES_IN, port("background", true, Kind::Image)],
        params: &[],
        mask_base: None,
        overlay: Overlay::None,
        help: "Draws particles straight onto the image, no camera: their x and y are frame pixels (y down) and their \
               size is in pixels, looking as Particle Render says (sprites by default). Over the background, in its \
               frame, else the project's.",
    },
];

/// Whether `op` is a particle node.
pub fn is_particle(op: &str) -> bool {
    info(op).is_some()
}

pub fn info(op: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|i| i.name == op)
}

/// `params` with every missing one of `op`'s at its default.
pub fn with_defaults(op: &str, params: &Value) -> Value {
    let mut out = params.as_object().cloned().unwrap_or_default();
    for spec in info(op).map_or(&[][..], |i| i.params) {
        out.entry(spec.name).or_insert_with(|| match spec.default {
            DefaultValue::Fn(f) => f(),
            DefaultValue::Static(v) => v.clone(),
        });
    }
    Value::Object(out)
}

fn num(p: &Value, name: &str) -> f64 {
    p.get(name).and_then(Value::as_f64).unwrap_or(0.0)
}

fn int(p: &Value, name: &str) -> i64 {
    p.get(name).and_then(Value::as_f64).unwrap_or(0.0).round() as i64
}

fn text<'a>(p: &'a Value, name: &str) -> &'a str {
    p.get(name).and_then(Value::as_str).unwrap_or("")
}

fn vec3(p: &Value, name: &str) -> Vec3 {
    let a = |i: usize| p.get(name).and_then(|v| v.get(i)).and_then(Value::as_f64).unwrap_or(0.0);
    Vec3::new(a(0), a(1), a(2))
}

fn color(p: &Value, name: &str) -> [f32; 4] {
    std::array::from_fn(|i| p.get(name).and_then(|v| v.get(i)).and_then(Value::as_f64).unwrap_or(1.0) as f32)
}

fn arr3(v: Vec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

/// A start frame, "always" far enough back.
fn start_frame(p: &Value, name: &str) -> i64 {
    let f = int(p, name);
    if f <= ALWAYS_FROM { i64::MIN } else { f }
}

fn end_frame(p: &Value, name: &str) -> i64 {
    let f = int(p, name);
    if f >= ALWAYS_TO { i64::MAX } else { f }
}

/// What a particle stream looks like where it's drawn.
#[derive(Clone)]
pub enum Look {
    /// Small dots (as plain round sprites).
    Points,
    Sprites(Sprites),
    /// A mesh (with its material) per particle.
    Objects { meshes: Vec<Arc<Mesh>>, materials: Vec<Material>, orient: Orient, scale: f64 },
}

/// A node's params at any frame (defaults filled in by the stream's
/// nodes).
pub type Own = Arc<dyn Fn(i64) -> Value + Send + Sync>;

/// A system as a function of the frame.
pub type Sampler = Arc<dyn Fn(i64) -> System + Send + Sync>;

/// Particles on their way through the graph: a system to simulate, the
/// cache to simulate it in, how they look, and the frame being shown.
#[derive(Clone)]
pub struct ParticleStream {
    /// The node whose stream it is.
    pub node: String,
    pub system: Sampler,
    cache: Arc<Mutex<Cache>>,
    /// Another evaluator's cache for the same node, read (never written)
    /// to go on from what it has simulated (see [`ParticleState::borrowing`]).
    base: Option<Arc<Mutex<Cache>>>,
    pub look: Look,
    pub frame: i64,
    /// The nodes its system is made of, and which of its emitters and
    /// modifiers each made (for showing and moving them in 3D).
    pub parts: Vec<Part>,
    /// Frames past `frame` it is shown at (within a render's shutter, for
    /// motion blur): each particle moved on along its velocity, curving by
    /// its acceleration (see [`Particle::moved`]).
    pub shift: f64,
}

/// A node in a particle system: an emitter or a modifier (its index in
/// the system's list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub node: String,
    pub what: PartOf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartOf {
    Emitter(usize),
    Modifier(usize),
}

impl std::fmt::Debug for ParticleStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ParticleStream({} at {})", self.node, self.frame)
    }
}

impl ParticleStream {
    /// The particles at the stream's frame (simulated as needed), moved
    /// on by its shift.
    pub fn particles(&self) -> Vec<Particle> {
        let mut out = self.particles_at(self.frame);
        if self.shift != 0.0 {
            let secs = self.shift / self.fps();
            for p in &mut out {
                (p.position, p.velocity) = p.moved(secs);
                p.rotation += p.spin * secs;
            }
        }
        out
    }

    /// Frames a second (velocities are per second).
    pub fn fps(&self) -> f64 {
        (self.system)(self.frame).fps.max(1e-6)
    }

    /// The stream shown `frames` later than it is (see `shift`).
    pub fn shifted(&self, frames: f64) -> ParticleStream {
        ParticleStream { shift: self.shift + frames, ..self.clone() }
    }

    /// The particles at frame `f`.
    pub fn particles_at(&self, f: i64) -> Vec<Particle> {
        let system = self.system.clone();
        let params = move |g: i64| system(g);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // More than a step to go: take what the base has that still stands
        // for this system, if it's further on.
        let held = cache.latest(f);
        if let (Some(base), true) = (&self.base, held.is_none_or(|h| h + 1 < f)) {
            let found = base.lock().unwrap_or_else(|e| e.into_inner()).standing(&params, f);
            if let Some((at, state)) = found.filter(|(at, _)| held.is_none_or(|h| *at > h)) {
                cache.start_from(&params, at, state);
            }
        }
        cache.frame(&params, f).to_vec()
    }

    /// Frames simulated in its cache so far.
    pub fn steps(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).steps
    }

    /// How its sprites look (Points and Objects as plain disks).
    pub fn sprites(&self) -> Sprites {
        match &self.look {
            Look::Sprites(s) => s.clone(),
            _ => Sprites::default(),
        }
    }
}

/// What particle nodes keep between evaluations: a simulation cache per
/// node, and shapes made from inputs (kept while the input is unchanged,
/// so the simulation doesn't start over).
#[derive(Default)]
pub struct ParticleState {
    caches: HashMap<String, Arc<Mutex<Cache>>>,
    /// Per emitter, the shapes made from its input, by the input's key.
    shapes: HashMap<String, HashMap<u64, Shape>>,
    /// Another state's caches, read to go on from (see `borrowing`).
    base: HashMap<String, Arc<Mutex<Cache>>>,
}

impl ParticleState {
    fn cache(&mut self, node: &str) -> Arc<Mutex<Cache>> {
        self.caches.entry(node.to_string()).or_default().clone()
    }

    /// A state of its own that goes on from what this one has simulated,
    /// where that still stands for its graph, without ever changing this
    /// one's caches (a render alongside the viewer's simulation). Shapes
    /// are shared too: a mesh shape compares equal only to itself.
    pub fn borrowing(&self) -> ParticleState {
        ParticleState { caches: HashMap::new(), shapes: self.shapes.clone(), base: self.caches.clone() }
    }

    /// Forget nodes not in `keep`.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.caches.retain(|k, _| keep(k));
        self.shapes.retain(|k, _| keep(k));
        self.base.retain(|k, _| keep(k));
    }
}

/// What a particle node gives.
pub enum Output {
    Stream(ParticleStream),
    Image(Image),
}

/// Everything a particle node is evaluated with.
pub struct Context<'a> {
    pub node: &'a str,
    pub frame: i64,
    /// The project's frames per second and format.
    pub fps: f64,
    pub format: Size,
    /// The node's params at any frame.
    pub own: Own,
    /// Its inputs at this frame, and their cache keys (for keeping shapes).
    pub inputs: &'a [Option<&'a Image>],
    pub keys: &'a [u64],
    /// The stream on its particles input, if it has one.
    pub upstream: Option<&'a ParticleStream>,
    /// A Particle Render's texture input at its sequence's frames, when
    /// it takes a sequence (see [`texture_sequence`]).
    pub sequence: Option<&'a [Arc<Image>]>,
    /// An emitter's shape input at the frames it emits at (see
    /// [`input_frames`]).
    pub history: Option<History<'a>>,
}

/// An emitter's shape input (geometry or mask) at the frames it emits at:
/// their cache keys, and how to render it at one (asked only for frames
/// whose key has no shape kept).
pub struct History<'a> {
    pub frames: &'a [i64],
    pub keys: &'a [u64],
    pub render: &'a mut dyn FnMut(i64) -> Result<Arc<Image>>,
}

/// The input a particle node with (resolved) `params` at `frame` needs at
/// other frames, and those frames: a Particle Render's texture at its
/// sequence's (see [`texture_sequence`]); an emitter's geometry or mask
/// at every frame of its simulation so far (its first, see
/// [`System::first_frame`], to `frame` or its end), so the shape is the
/// one at each frame. None when it needs none.
pub fn input_frames(op: &str, p: &Value, frame: i64) -> Result<Option<(usize, std::ops::RangeInclusive<i64>)>> {
    if let Some(frames) = texture_sequence(op, p)? {
        return Ok(Some((1, frames)));
    }
    if op != "particle_emitter" {
        return Ok(None);
    }
    let p = with_defaults(op, p);
    let Some(port) = shape_port(&p) else { return Ok(None) };
    let first = int(&p, "start").saturating_sub(int(&p, "preroll").max(0).saturating_add(1));
    let last = frame.min(end_frame(&p, "end"));
    if last < first {
        return Ok(None);
    }
    if last - first >= MAX_HISTORY {
        bail!("it emits from its {} over {} frames, more than {MAX_HISTORY}", if port == 1 { "mask" } else { "geometry" }, last - first + 1);
    }
    Ok(Some((port, first..=last)))
}

/// The input an emitter with params `p` takes its shape from, if any.
fn shape_port(p: &Value) -> Option<usize> {
    match text(p, "shape") {
        "geometry surface" | "geometry vertices" | "point set" => Some(0),
        "mask" => Some(1),
        _ => None,
    }
}

/// The frames of its texture input a Particle Render with (resolved)
/// params `p` shows: None for the frame rendered, else Sequence first
/// frame to Sequence last frame (an error past [`MAX_SEQUENCE`] frames).
pub fn texture_sequence(op: &str, p: &Value) -> Result<Option<std::ops::RangeInclusive<i64>>> {
    if op != "particle_render" {
        return Ok(None);
    }
    let p = with_defaults(op, p);
    if text(&p, "texture_frames") != "sequence" || text(&p, "look") != "sprites" {
        return Ok(None);
    }
    let (first, last) = (int(&p, "sequence_first"), int(&p, "sequence_last"));
    if last < first {
        bail!("its texture sequence ends (frame {last}) before it starts (frame {first})");
    }
    if last - first >= MAX_SEQUENCE {
        bail!("its texture sequence is {} frames long, more than {MAX_SEQUENCE}", last - first + 1);
    }
    Ok(Some(first..=last))
}

/// Evaluate particle node `op`.
pub fn eval(op: &str, cx: Context, state: &mut ParticleState) -> Result<Output> {
    let at_frame = with_defaults(op, &(cx.own)(cx.frame));
    let own = {
        let (op, own) = (op.to_string(), cx.own.clone());
        move |g: i64| with_defaults(&op, &own(g))
    };
    let stream = |system: Sampler, look: Look, parts: Vec<Part>, state: &mut ParticleState| ParticleStream { node: cx.node.to_string(), system, cache: state.cache(cx.node), base: state.base.get(cx.node).cloned(), look, frame: cx.frame, parts, shift: 0.0 };
    let input = |i: usize| cx.inputs.get(i).copied().flatten();
    match op {
        "particle_emitter" => {
            let shape = emitter_shape(&at_frame, cx.node, input(0), input(1), cx.keys, state)?;
            // The shape at each frame (the input then), so births follow
            // the geometry as it moves and a frame's shape stays the same
            // Arc while its input's key holds: the cache steps on from
            // the frame before instead of starting over.
            let (shapes, used) = match (&shape, cx.history) {
                (Some(_), Some(history)) => {
                    let (shapes, used) = shape_history(&at_frame, cx.node, history, state)?;
                    (Some(Arc::new(shapes)), used)
                }
                _ => Default::default(),
            };
            keep_shapes(cx.node, &at_frame, cx.keys, &used, state);
            let fps = cx.fps;
            let system: Sampler = Arc::new(move |g| {
                let p = own(g);
                let shape = match &shapes {
                    Some(at) => at.range(..=g).next_back().or_else(|| at.iter().next()).map(|(_, s)| s.clone()),
                    None => shape.clone(),
                };
                System {
                    emitters: vec![emitter(&p, shape)],
                    modifiers: vec![],
                    fps,
                    substeps: int(&p, "substeps").max(1) as u32,
                    preroll: int(&p, "preroll").max(0),
                }
            });
            let parts = vec![Part { node: cx.node.to_string(), what: PartOf::Emitter(0) }];
            Ok(Output::Stream(stream(system, Look::Points, parts, state)))
        }
        "particle_render" => {
            let up = cx.upstream.ok_or_else(|| anyhow::anyhow!("connect particles to its particles input"))?;
            let look = look(&at_frame, input(1), cx.sequence, input(2).and_then(|i| i.scene.as_deref()))?;
            Ok(Output::Stream(ParticleStream { look, ..stream(up.system.clone(), Look::Points, up.parts.clone(), state) }))
        }
        "particles_2d" => {
            let up = cx.upstream.ok_or_else(|| anyhow::anyhow!("connect particles to its particles input"))?;
            let format = input(1).map_or(cx.format, |b| b.format);
            Ok(Output::Image(particles_render::render_sprites_2d(&up.particles(), &up.sprites(), format, input(1))))
        }
        _ if info(op).is_some() => {
            let up = cx.upstream.ok_or_else(|| anyhow::anyhow!("connect particles to its particles input"))?;
            let (upstream, op) = (up.system.clone(), op.to_string());
            let system: Sampler = Arc::new(move |g| {
                let mut s = upstream(g);
                s.modifiers.push(modifier(&op, &own(g)));
                s
            });
            let mut parts = up.parts.clone();
            let index = parts.iter().filter(|p| matches!(p.what, PartOf::Modifier(_))).count();
            parts.push(Part { node: cx.node.to_string(), what: PartOf::Modifier(index) });
            Ok(Output::Stream(stream(system, up.look.clone(), parts, state)))
        }
        _ => bail!("'{op}' isn't a particle node"),
    }
}

/// The emitter its params describe, from `shape` (for the geometry and
/// mask shapes; the others are made from the params).
pub fn emitter(p: &Value, shape: Option<Shape>) -> Emitter {
    let d = vec3(p, "dimensions");
    let each_once = p["each_point_once"].as_bool().unwrap_or(true);
    let shape = shape.unwrap_or_else(|| match text(p, "shape") {
        "line" => Shape::Line { length: d.x },
        "rectangle" => Shape::Rect { width: d.x, height: d.y },
        "disk" => Shape::Disk { radius: d.x / 2.0 },
        "sphere" => Shape::Sphere { radius: d.x / 2.0, surface: p["surface"] == true },
        "box" => Shape::Box { size: d, surface: p["surface"] == true },
        _ => Shape::Point,
    });
    // (From a point set, each point once: as many as it has.)
    let burst = match &shape {
        Shape::Points(src) if each_once => src.len() as u32,
        _ => int(p, "burst").max(0) as u32,
    };
    Emitter {
        shape,
        transform: geom::trs(arr3(vec3(p, "translate")), arr3(vec3(p, "rotate")), arr3(vec3(p, "scale"))),
        rate: num(p, "rate").max(0.0),
        burst,
        start: int(p, "start"),
        end: end_frame(p, "end"),
        direction: vec3(p, "direction"),
        speed: num(p, "speed"),
        speed_var: num(p, "speed_var"),
        spread: num(p, "spread"),
        along_normal: num(p, "along_normal"),
        life: num(p, "life").max(0.0),
        life_var: num(p, "life_var"),
        size: num(p, "size").max(0.0),
        size_var: num(p, "size_var"),
        size_end: num(p, "size_end"),
        color: color(p, "color"),
        color_end: color(p, "color_end"),
        rotation: num(p, "rotation"),
        rotation_var: num(p, "rotation_var"),
        spin: num(p, "spin"),
        spin_var: num(p, "spin_var"),
        seed: int(p, "seed").max(0) as u64,
        each_once,
    }
}

/// The shape an emitter takes from its inputs (None for the shapes its
/// params make), kept while the input is unchanged.
fn emitter_shape(p: &Value, node: &str, geometry: Option<&Image>, mask: Option<&Image>, keys: &[u64], state: &mut ParticleState) -> Result<Option<Shape>> {
    let kind = text(p, "shape");
    let Some(port) = shape_port(p) else { return Ok(None) };
    let Some(source) = (if port == 1 { mask } else { geometry }) else {
        bail!("its shape is '{kind}': connect the {} input", if kind == "mask" { "mask" } else { "geometry" });
    };
    let key = shape_key(kind, keys.get(port).copied().unwrap_or(0));
    if let Some(shape) = state.shapes.get(node).and_then(|s| s.get(&key)) {
        return Ok(Some(shape.clone()));
    }
    let shape = make_shape(kind, source)?;
    state.shapes.entry(node.to_string()).or_default().insert(key, shape.clone());
    Ok(Some(shape))
}

/// A shape's key: its input's, and which shape (so switching makes a new
/// one).
fn shape_key(kind: &str, input: u64) -> u64 {
    input ^ (kind.len() as u64).rotate_left(40)
}

/// The shape `kind` from `source` (a mask image, or a scene's geometry).
fn make_shape(kind: &str, source: &Image) -> Result<Shape> {
    Ok(match kind {
        "mask" => Shape::mask(source),
        _ => {
            let Some(scene) = source.scene.as_deref() else { bail!("the geometry input needs a scene (a card or a mesh)") };
            if kind == "point set" {
                // Every point set's points, placed in the scene.
                if scene.point_sets.is_empty() {
                    bail!("its shape is 'point set': the geometry input has no points (a Scatter makes them)");
                }
                let positions = scene.point_sets.iter().flat_map(|g| (0..g.len()).map(move |i| g.placed(i))).collect();
                let attrs = crate::geo::Attrs::concat(&scene.point_sets.iter().map(|g| (&g.attrs, g.len())).collect::<Vec<_>>());
                return Ok(Shape::Points(Arc::new(crate::particles::PointSource::new(positions, attrs))));
            }
            let mesh = Arc::new(world_mesh(scene));
            if kind == "geometry surface" { Shape::mesh_surface(mesh) } else { Shape::MeshVertices(mesh) }
        }
    })
}

/// An emitter's shape at each of `history`'s frames: kept ones by their
/// key, the rest made from its input rendered then. Also the keys used.
fn shape_history(p: &Value, node: &str, history: History, state: &mut ParticleState) -> Result<(BTreeMap<i64, Shape>, HashSet<u64>)> {
    let kind = text(p, "shape");
    let kept = state.shapes.entry(node.to_string()).or_default();
    let (mut out, mut used) = (BTreeMap::new(), HashSet::new());
    for (&g, &k) in history.frames.iter().zip(history.keys) {
        let key = shape_key(kind, k);
        used.insert(key);
        let shape = match kept.get(&key) {
            Some(shape) => shape.clone(),
            None => {
                let source = (history.render)(g).with_context(|| format!("its {} at frame {g}", if kind == "mask" { "mask" } else { "geometry" }))?;
                let shape = make_shape(kind, &source).with_context(|| format!("at frame {g}"))?;
                kept.insert(key, shape.clone());
                shape
            }
        };
        out.insert(g, shape);
    }
    Ok((out, used))
}

/// Forget an emitter's shapes but those in use: by key, `used` and its
/// input's now (params `p`, input keys `keys`).
fn keep_shapes(node: &str, p: &Value, keys: &[u64], used: &HashSet<u64>, state: &mut ParticleState) {
    let Some(kept) = state.shapes.get_mut(node) else { return };
    let now = shape_port(p).map(|port| shape_key(text(p, "shape"), keys.get(port).copied().unwrap_or(0)));
    kept.retain(|k, _| Some(*k) == now || used.contains(k));
}

/// Every card and mesh of `scene` as one mesh in scene space.
pub fn world_mesh(scene: &Scene) -> Mesh {
    let mut out = Mesh::default();
    let mut add = |positions: Vec<Vec3>, normals: Vec<Vec3>, triangles: &[[u32; 3]]| {
        let base = out.positions.len() as u32;
        out.positions.extend(positions);
        out.normals.extend(normals);
        out.triangles.extend(triangles.iter().map(|t| t.map(|i| i + base)));
    };
    for card in &scene.cards {
        let corners = card.corners();
        let n = (corners[1] - corners[0]).cross(&(corners[3] - corners[0])).try_normalize(1e-12).unwrap_or_else(Vec3::z);
        add(corners.to_vec(), vec![-n; 4], &[[0, 1, 2], [0, 2, 3]]);
    }
    for m in &scene.meshes {
        let turn = m.world.fixed_view::<3, 3>(0, 0).try_inverse().map(|t| t.transpose()).unwrap_or_else(nalgebra::Matrix3::identity);
        let positions = m.mesh.positions.iter().map(|p| geom::transform_point(&m.world, p)).collect();
        let normals = if m.mesh.normals.len() == m.mesh.positions.len() {
            m.mesh.normals.iter().map(|n| (turn * n).try_normalize(1e-12).unwrap_or(*n)).collect()
        } else {
            // Flat: each vertex the first triangle's normal it's in.
            let mut flat = vec![Vec3::z(); m.mesh.positions.len()];
            for t in m.mesh.triangles.iter().rev() {
                let [a, b, c] = t.map(|i| geom::transform_point(&m.world, &m.mesh.positions[i as usize]));
                let n = (b - a).cross(&(c - a)).try_normalize(1e-12).unwrap_or_else(Vec3::z);
                t.iter().for_each(|&i| flat[i as usize] = n);
            }
            flat
        };
        add(positions, normals, &m.mesh.triangles);
    }
    // Normals for every vertex or none.
    if out.normals.len() != out.positions.len() {
        out.normals.clear();
    }
    out
}

/// The modifier modifier node `op`'s params `p` describe.
pub fn modifier(op: &str, p: &Value) -> Modifier {
    let effect = match op {
        "particle_gravity" => Effect::Gravity { accel: vec3(p, "acceleration") },
        "particle_wind" => Effect::Drag { wind: vec3(p, "wind"), drag: num(p, "drag").max(0.0) },
        "particle_turbulence" => Effect::Turbulence { strength: num(p, "amount"), scale: num(p, "scale"), speed: num(p, "speed"), seed: int(p, "seed").max(0) as u64 },
        "particle_attractor" => Effect::Attractor { point: vec3(p, "position"), strength: num(p, "amount"), radius: num(p, "radius").max(0.0) },
        "particle_vortex" => Effect::Vortex { center: vec3(p, "center"), axis: vec3(p, "axis"), strength: num(p, "amount"), radius: num(p, "radius").max(0.0) },
        "particle_bounce" => Effect::Bounce { point: vec3(p, "point"), normal: vec3(p, "normal"), restitution: num(p, "bounce"), friction: num(p, "friction") },
        "particle_kill" => {
            let (center, size) = (vec3(p, "center"), vec3(p, "size").abs());
            let region = match text(p, "region") {
                "inside sphere" | "outside sphere" => Some(Region::Sphere { center, radius: num(p, "radius") }),
                "inside box" | "outside box" => Some(Region::Box { min: center - size / 2.0, max: center + size / 2.0 }),
                _ => None,
            };
            let inside = text(p, "region").starts_with("inside");
            let age = num(p, "age");
            Effect::Kill { age: (age > 0.0).then_some(age), region: region.map(|r| (r, inside)) }
        }
        _ => Effect::SpeedLimit { max: num(p, "max").max(0.0) },
    };
    Modifier { effect, strength: num(p, "strength"), start: start_frame(p, "start"), end: end_frame(p, "end") }
}

/// The look Particle Render's params `p` describe, with its texture input
/// (at the frame, or the sequence's frames) and objects input.
fn look(p: &Value, texture: Option<&Image>, sequence: Option<&[Arc<Image>]>, objects: Option<&Scene>) -> Result<Look> {
    Ok(match text(p, "look") {
        "points" => Look::Points,
        "objects" => {
            let Some(scene) = objects.filter(|s| !s.meshes.is_empty()) else { bail!("its look is objects: connect meshes to the objects input") };
            // Each mesh as placed in its node, so a primitive's own size
            // and turn carry over.
            let meshes = scene
                .meshes
                .iter()
                .map(|m| {
                    let one = Scene { meshes: vec![m.clone()], ..Scene::default() };
                    Arc::new(world_mesh(&one))
                })
                .collect();
            let materials = scene.meshes.iter().map(|m| m.material.clone()).collect();
            let orient = match text(p, "orient") {
                "velocity" => Orient::Velocity,
                "spin" => Orient::Spin,
                _ => Orient::None,
            };
            Look::Objects { meshes, materials, orient, scale: num(p, "scale") }
        }
        _ => Look::Sprites(Sprites {
            outline: if text(p, "outline") == "square" { Outline::Square } else { Outline::Round },
            texture: match sequence {
                Some(images) => Some(images.to_vec()),
                None => texture.map(|t| vec![Arc::new(t.clone())]),
            }
            .map(|images| Texture {
                images,
                pick: match text(p, "pick") {
                    "cycle" => Pick::Cycle { rate: num(p, "cycle_rate") },
                    pick @ ("play" | "loop") => Pick::Play { rate: num(p, "cycle_rate"), looped: pick == "loop", random: p["random_start"] == true },
                    _ => Pick::ByAge,
                },
            }),
            scale: num(p, "scale"),
            opacity: num(p, "opacity") as f32,
            opacity_end: num(p, "opacity_end") as f32,
            blend: if text(p, "blend") == "additive" { Blend::Additive } else { Blend::Over },
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node's params, the same at every frame.
    fn fixed(p: Value) -> Own {
        Arc::new(move |_| p.clone())
    }

    fn cx<'a>(node: &'a str, frame: i64, own: Own, inputs: &'a [Option<&'a Image>], keys: &'a [u64], upstream: Option<&'a ParticleStream>) -> Context<'a> {
        Context { node, frame, fps: 24.0, format: Size { width: 100, height: 80 }, own, inputs, keys, upstream, sequence: None, history: None }
    }

    fn run(op: &str, cx: Context, state: &mut ParticleState) -> ParticleStream {
        match eval(op, cx, state).unwrap() {
            Output::Stream(s) => s,
            Output::Image(_) => panic!("{op} made an image"),
        }
    }

    #[test]
    fn every_param_has_a_default_and_names_are_unique() {
        let mut names: Vec<&str> = OPS.iter().map(|o| o.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), OPS.len());
        for op in OPS {
            let p = with_defaults(op.name, &Value::Null);
            assert!(op.params.iter().all(|s| p.get(s.name).is_some()), "{}", op.name);
            assert!(!op.help.is_empty());
        }
        assert!(is_particle("particle_vortex") && !is_particle("merge"));
    }

    #[test]
    fn an_emitter_is_what_its_params_say() {
        let p = with_defaults("particle_emitter", &json!({ "shape": "box", "dimensions": [2, 4, 6], "surface": true, "translate": [1, 0, 0], "rate": 3, "end": 1_000_000, "color": [1, 0, 0, 1] }));
        let e = emitter(&p, None);
        assert_eq!(e.shape, Shape::Box { size: Vec3::new(2.0, 4.0, 6.0), surface: true });
        assert_eq!((e.rate, e.start, e.end, e.color), (3.0, 1, i64::MAX, [1.0, 0.0, 0.0, 1.0]));
        assert_eq!(geom::transform_point(&e.transform, &Vec3::zeros()), Vec3::new(1.0, 0.0, 0.0));
        let disk = emitter(&with_defaults("particle_emitter", &json!({ "shape": "disk", "dimensions": [3, 0, 0] })), None);
        assert_eq!(disk.shape, Shape::Disk { radius: 1.5 });
        let kill = modifier("particle_kill", &with_defaults("particle_kill", &json!({ "region": "outside box", "size": [2, 2, 2] })));
        assert_eq!(kill.effect, Effect::Kill { age: None, region: Some((Region::Box { min: Vec3::from_element(-1.0), max: Vec3::from_element(1.0) }, false)) });
        assert_eq!((kill.start, kill.end, kill.strength), (i64::MIN, i64::MAX, 1.0));
    }

    #[test]
    fn a_chain_simulates_once_at_its_end_with_animated_params() {
        let mut state = ParticleState::default();
        // Rate keyed: nothing before frame 5, then 2 a frame.
        let emitter_params: Own = Arc::new(|f| json!({ "rate": if f >= 5 { 2 } else { 0 }, "speed": 0, "life": 1000 }));
        let src = run("particle_emitter", cx("emit", 10, emitter_params, &[], &[], None), &mut state);
        let gravity = fixed(json!({ "acceleration": [0, -10, 0] }));
        let end = run("particle_gravity", cx("fall", 10, gravity, &[], &[], Some(&src)), &mut state);
        let ps = end.particles();
        assert_eq!(ps.len(), 12, "frames 5 to 10 at 2 a frame");
        assert_eq!(end.parts, [Part { node: "emit".into(), what: PartOf::Emitter(0) }, Part { node: "fall".into(), what: PartOf::Modifier(0) }]);
        for p in &ps {
            let t = p.age / 24.0;
            assert!((p.position.y + 5.0 * t * t).abs() < 1e-9, "falling from rest: {p:?}");
        }
        assert_eq!((src.steps(), end.steps()), (0, 10), "only the end of the chain simulated");
        // The next frame steps once more; the emitter alone is unaffected.
        let next = run("particle_gravity", cx("fall", 11, fixed(json!({ "acceleration": [0, -10, 0] })), &[], &[], Some(&src)), &mut state);
        assert_eq!(next.particles().len(), 14);
        assert_eq!(next.steps(), 11);
        assert!(src.particles_at(10).iter().all(|p| p.position.y == 0.0));
    }

    /// Within a shutter, falling particles curve as they do frame to
    /// frame: shifted a frame on, where the next frame has them.
    #[test]
    fn shifted_particles_keep_falling() {
        let mut state = ParticleState::default();
        let src = run("particle_emitter", cx("emit", 10, fixed(json!({ "burst": 3, "rate": 0, "speed": 4, "life": 1000 })), &[], &[], None), &mut state);
        let gravity = fixed(json!({ "acceleration": [0, -10, 0] }));
        let now = run("particle_gravity", cx("fall", 10, gravity.clone(), &[], &[], Some(&src)), &mut state);
        let next = run("particle_gravity", cx("fall", 11, gravity, &[], &[], Some(&src)), &mut state);
        assert!(now.particles().iter().all(|p| p.accel == Vec3::new(0.0, -10.0, 0.0)));
        for (a, b) in now.shifted(1.0).particles().iter().zip(next.particles()) {
            assert!((a.position - b.position).norm() < 1e-9 && (a.velocity - b.velocity).norm() < 1e-9, "{a:?} vs {b:?}");
        }
        // Back half a frame, as a centered shutter opens.
        let half = now.shifted(-0.5).particles()[0].clone();
        let p = &now.particles()[0];
        let t = -0.5 / 24.0;
        assert!((half.position - (p.position + p.velocity * t + Vec3::new(0.0, -5.0, 0.0) * t * t)).norm() < 1e-12);
    }

    #[test]
    fn shapes_from_inputs_are_kept_until_the_input_changes() {
        let mut state = ParticleState::default();
        let geo = scene_of("card", json!({ "translate": [0, 0, -3], "size": [2, 1] }), Some(&Image::new(2, 2)));
        let params = fixed(json!({ "shape": "geometry surface", "rate": 50, "speed": 0 }));
        let shape_of = |s: &ParticleStream| s.system.as_ref()(1).emitters[0].shape.clone();
        let a = run("particle_emitter", cx("e", 3, params.clone(), &[Some(&geo)], &[7], None), &mut state);
        let b = run("particle_emitter", cx("e", 4, params.clone(), &[Some(&geo)], &[7], None), &mut state);
        assert!(shape_of(&a) == shape_of(&b), "the same Arc while the input's key holds");
        let c = run("particle_emitter", cx("e", 4, params.clone(), &[Some(&geo)], &[8], None), &mut state);
        assert!(shape_of(&a) != shape_of(&c));
        // On the card: z = −3, inside its 2 × 1.
        let ps = c.particles();
        assert_eq!(ps.len(), 200);
        assert!(ps.iter().all(|p| (p.position.z + 3.0).abs() < 1e-12 && p.position.x.abs() <= 1.0 && p.position.y.abs() <= 0.5));
        // No geometry connected: a clear error.
        let err = eval("particle_emitter", cx("e", 4, params, &[], &[], None), &mut state).err().unwrap();
        assert!(err.to_string().contains("geometry"), "{err}");
    }

    /// What 3D op `op` makes.
    fn scene_of(op: &str, params: Value, input: Option<&Image>) -> Image {
        crate::ops::eval(op, &params, &[input], &crate::ops::EvalContext { frame: 1, write_outputs: false }).unwrap()
    }

    #[test]
    fn particle_render_sets_the_look_and_particles_2d_draws_it() {
        let mut state = ParticleState::default();
        // A burst of 1 at pixel (20, 30), 10 px across.
        let src = run("particle_emitter", cx("e", 1, fixed(json!({ "burst": 1, "rate": 0, "translate": [20, 30, 0], "size": 10, "speed": 0 })), &[], &[], None), &mut state);
        let look = fixed(json!({ "look": "sprites", "outline": "square", "blend": "additive" }));
        let styled = run("particle_render", cx("r", 1, look, &[], &[], Some(&src)), &mut state);
        assert!(matches!(&styled.look, Look::Sprites(s) if s.outline == Outline::Square && s.blend == Blend::Additive));
        let bg = Image::filled(100, 80, [0.0, 0.0, 0.5, 1.0]);
        let out = match eval("particles_2d", cx("d", 1, fixed(json!({})), &[None, Some(&bg)], &[], Some(&styled)), &mut state).unwrap() {
            Output::Image(img) => img,
            Output::Stream(_) => panic!("Particles 2D makes an image"),
        };
        assert_eq!(out.pixels[30 * 100 + 20], [1.0, 1.0, 1.5, 1.0], "added onto the background");
        assert_eq!(out.pixels[30 * 100 + 40], [0.0, 0.0, 0.5, 1.0]);
        // Objects need meshes.
        let objects = fixed(json!({ "look": "objects" }));
        assert!(eval("particle_render", cx("r", 1, objects.clone(), &[], &[], Some(&src)), &mut state).is_err());
        let geo = scene_of("cube", json!({ "scale": [2, 2, 2] }), None);
        let styled = run("particle_render", cx("r", 1, objects, &[None, None, Some(&geo)], &[], Some(&src)), &mut state);
        let Look::Objects { meshes, orient, .. } = &styled.look else { panic!("objects") };
        assert_eq!((meshes[0].bounds(), *orient), (Some((Vec3::from_element(-1.0), Vec3::from_element(1.0))), Orient::Velocity), "as placed in its node");
        // Modifiers keep the look.
        let fall = run("particle_gravity", cx("g", 1, fixed(json!({})), &[], &[], Some(&styled)), &mut state);
        assert!(matches!(fall.look, Look::Objects { .. }));
        assert!(eval("particle_gravity", cx("g", 1, fixed(json!({})), &[], &[], None), &mut state).is_err(), "needs particles");
    }
}
