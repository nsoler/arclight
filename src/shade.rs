//! Shading: how lit a surface point is, split into the lighting passes.
//!
//! [`shade`] lights one hit (a point on a surface, its normal and the
//! direction to the eye) with a [`Material`] against a list of [`Light`]s
//! and returns the parts separately: `diffuse` (Lambert's cosine law),
//! `specular` (a Blinn–Phong highlight), `emission` and `ambient`, which
//! add up to the beauty ([`Shaded::total`]). Each light's share alone is
//! [`light`], for per-light passes.
//!
//! Lights are placed like other 3D nodes, by a world matrix: a light sits
//! at its translation and points down its −Z. Intensity falls off with
//! distance `d` as `1 / d^decay` (0 none, 1 linear, 2 physical). Lights
//! with a size (an area light, or a point or spot with a radius) are
//! sampled at a grid of points over it, each a share of the light.

use crate::geom::{Mat4, Vec3};
use crate::mesh::{Material, Shading};

/// What kind of light, and its shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightKind {
    /// Light from everywhere: no direction, no falloff.
    Ambient,
    /// Parallel light down the light's −Z (a distant sun): no falloff.
    Directional,
    /// From a point, every way.
    Point,
    /// From a point, down −Z within a cone `cone` degrees across, its edge
    /// softened over the last `penumbra` degrees (from each side's edge
    /// inward), and dimmed toward the edge by the cosine from the axis to
    /// the power `falloff` (0: even across).
    Spot { cone: f64, penumbra: f64, falloff: f64 },
    /// From a rectangle `size` across in the light's XY plane (or the
    /// ellipse inside it, if `disk`), facing −Z (and +Z too if
    /// `two_sided`), sampled at a `samples` × `samples` grid of points.
    Area { size: [f64; 2], disk: bool, two_sided: bool, samples: usize },
}

impl LightKind {
    /// A one-sided 4 × 4-sampled rectangle `size` across.
    pub fn rect(size: [f64; 2]) -> LightKind {
        LightKind::Area { size, disk: false, two_sided: false, samples: 4 }
    }
}

/// A light in the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub kind: LightKind,
    /// Color (linear) and how strong: the light reaching a surface facing
    /// it one unit away is `color × intensity` (exposure included).
    pub color: [f32; 3],
    pub intensity: f32,
    /// Falloff exponent with distance.
    pub decay: f64,
    /// A point or spot light's soft size: it shines from a disk this
    /// radius, turned to face the lit point (0: a true point).
    pub radius: f64,
    /// Off, it lights nothing (but is still drawn and picked in 3D).
    pub enabled: bool,
    /// Whether objects block it (a render draws shadow maps for it), and
    /// how soft their edges are: the filter's radius in shadow-map texels.
    pub shadows: bool,
    pub shadow_softness: f64,
    /// Placement: position its translation, pointing down its −Z.
    pub world: Mat4,
    /// The node it came from (for per-light passes; "" if none).
    pub node: String,
}

/// The samples of a light with a size: a 4 × 4 grid.
const SOFT_SAMPLES: usize = 4;

impl Light {
    /// A light of `kind` placed by `world`, white, intensity 1, physical
    /// (quadratic) falloff, no radius, on, casting no shadows.
    pub fn new(kind: LightKind, world: Mat4) -> Light {
        Light { kind, color: [1.0; 3], intensity: 1.0, decay: 2.0, radius: 0.0, enabled: true, shadows: false, shadow_softness: 1.0, world, node: String::new() }
    }

    pub fn position(&self) -> Vec3 {
        self.world.fixed_view::<3, 1>(0, 3).into_owned()
    }

    /// Which way it shines (unit): its −Z in the world.
    pub fn direction(&self) -> Vec3 {
        let d: Vec3 = -self.world.fixed_view::<3, 1>(0, 2).into_owned();
        d.try_normalize(1e-300).unwrap_or(-Vec3::z())
    }

    /// The light's own X or Y axis in the world (unit).
    pub fn axis(&self, k: usize) -> Vec3 {
        self.world.fixed_view::<3, 1>(0, k).into_owned().try_normalize(1e-300).unwrap_or(if k == 0 { Vec3::x() } else { Vec3::y() })
    }

    /// The light arriving at `p` from point `from` (a direction to the light
    /// and the strength, color aside), with falloff.
    fn reaching(&self, from: Vec3, p: &Vec3) -> Option<(Vec3, f64)> {
        let to = from - p;
        let d = to.norm();
        (d > 1e-12).then(|| (to / d, self.intensity as f64 / d.powf(self.decay)))
    }

    /// The light from its position at `p`, or with a radius, from a grid
    /// of points over a disk that wide facing `p`, each a share of it (all
    /// shadow sample 0: shadowed as from its middle).
    fn sampled(&self, p: &Vec3) -> Vec<(Vec3, f64, usize)> {
        let at = self.position();
        if self.radius <= 0.0 {
            return self.reaching(at, p).map(|(l, e)| (l, e, 0)).into_iter().collect();
        }
        let Some(w) = (p - at).try_normalize(1e-12) else { return Vec::new() };
        let u = w.cross(&if w.x.abs() < 0.9 { Vec3::x() } else { Vec3::y() }).normalize();
        let v = w.cross(&u);
        let share = 1.0 / (SOFT_SAMPLES * SOFT_SAMPLES) as f64;
        grid(SOFT_SAMPLES)
            .filter_map(|(x, y)| {
                let [dx, dy] = disk(x, y);
                self.reaching(at + (u * dx + v * dy) * self.radius, p).map(|(l, e)| (l, e * share, 0))
            })
            .collect()
    }

    /// Where the light comes from at `p`: (unit direction to it, strength,
    /// shadow sample) triples, color aside; empty for ambient light or when
    /// it's off. The shadow sample is which of [`Light::shadow_points`]
    /// the light comes through.
    fn arriving(&self, p: &Vec3) -> Vec<(Vec3, f64, usize)> {
        if !self.enabled {
            return Vec::new();
        }
        match self.kind {
            LightKind::Ambient => Vec::new(),
            LightKind::Directional => vec![(-self.direction(), self.intensity as f64, 0)],
            LightKind::Point => self.sampled(p),
            LightKind::Spot { cone, penumbra, falloff } => {
                let Some(to) = (p - self.position()).try_normalize(1e-12) else { return Vec::new() };
                let cos = to.dot(&self.direction()).clamp(-1.0, 1.0);
                let angle = cos.acos().to_degrees();
                let edge = cone / 2.0;
                let soft = penumbra.clamp(0.0, edge);
                let mut k = if angle >= edge {
                    0.0
                } else if angle <= edge - soft {
                    1.0
                } else {
                    smoothstep((edge - angle) / soft)
                };
                if falloff > 0.0 {
                    k *= cos.max(0.0).powf(falloff);
                }
                if k <= 0.0 {
                    return Vec::new();
                }
                let mut out = self.sampled(p);
                out.iter_mut().for_each(|s| s.1 *= k);
                out
            }
            LightKind::Area { size, disk: round, two_sided, samples } => {
                // A grid of points over the shape, each an equal share of
                // the light, weighted by how squarely the light faces p.
                let n = samples.max(1);
                let facing = self.direction();
                let share = 1.0 / (n * n) as f64;
                let mut out = Vec::with_capacity(n * n);
                for (k, at) in self.area_points(size, round, n).into_iter().enumerate() {
                    if let Some((l, e)) = self.reaching(at, p) {
                        let cos = facing.dot(&-l);
                        let cos = if two_sided { cos.abs() } else { cos };
                        if cos > 0.0 {
                            out.push((l, e * cos * share, k));
                        }
                    }
                }
                out
            }
        }
    }

    /// An area light's sample points in the scene: an `n` × `n` grid over
    /// its rectangle, or mapped onto its disk.
    fn area_points(&self, size: [f64; 2], round: bool, n: usize) -> Vec<Vec3> {
        grid(n)
            .map(|(x, y)| {
                let [x, y] = if round { disk(x, y) } else { [x, y] };
                crate::geom::transform_point(&self.world, &Vec3::new(x * size[0] / 2.0, y * size[1] / 2.0, 0.0))
            })
            .collect()
    }

    /// Where its shadows are cast from, by shadow sample: its position (a
    /// point or spot light; one for a directional light, which casts along
    /// its direction), or an area light's sample points; none for ambient
    /// light.
    pub fn shadow_points(&self) -> Vec<Vec3> {
        match self.kind {
            LightKind::Ambient => Vec::new(),
            LightKind::Directional | LightKind::Point | LightKind::Spot { .. } => vec![self.position()],
            LightKind::Area { size, disk, samples, .. } => self.area_points(size, disk, samples.max(1)),
        }
    }
}

/// The middles of an `n` × `n` grid of cells over the square −1 to 1.
fn grid(n: usize) -> impl Iterator<Item = (f64, f64)> {
    (0..n * n).map(move |k| ((2.0 * (k % n) as f64 + 1.0) / n as f64 - 1.0, (2.0 * (k / n) as f64 + 1.0) / n as f64 - 1.0))
}

/// Square point (`x`, `y`), −1 to 1, onto the unit disk, keeping equal
/// areas equal (Shirley and Chiu's concentric map).
fn disk(x: f64, y: f64) -> [f64; 2] {
    use std::f64::consts::FRAC_PI_4;
    if x == 0.0 && y == 0.0 {
        return [0.0, 0.0];
    }
    let (r, phi) = if x.abs() > y.abs() { (x, FRAC_PI_4 * y / x) } else { (y, 2.0 * FRAC_PI_4 - FRAC_PI_4 * x / y) };
    [r * phi.cos(), r * phi.sin()]
}

/// 0 to 1 smoothly over `t` = 0 to 1.
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A point being shaded, in world space.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub position: Vec3,
    /// The surface's unit normal (either side: it's turned to face the eye).
    pub normal: Vec3,
    /// Unit direction from the point to the eye.
    pub view: Vec3,
}

/// A point's light, in parts that add up to its color (linear RGB).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shaded {
    pub diffuse: [f32; 3],
    pub specular: [f32; 3],
    pub emission: [f32; 3],
    pub ambient: [f32; 3],
}

impl Shaded {
    /// The beauty: every part added up.
    pub fn total(&self) -> [f32; 3] {
        std::array::from_fn(|c| self.diffuse[c] + self.specular[c] + self.emission[c] + self.ambient[c])
    }

    fn add(&mut self, o: &Shaded) {
        for c in 0..3 {
            self.diffuse[c] += o.diffuse[c];
            self.specular[c] += o.specular[c];
            self.emission[c] += o.emission[c];
            self.ambient[c] += o.ambient[c];
        }
    }
}

/// The Blinn–Phong exponent for a roughness (0 sharp to 1 broad): the
/// inverse of the MTL mapping roughness = √(2 / (Ns + 2)).
pub fn shininess(roughness: f32) -> f64 {
    let r = (roughness as f64).clamp(0.02, 1.0);
    (2.0 / (r * r) - 2.0).max(1.0)
}

/// What `light` alone adds at `hit` on `material`, whose color there is
/// `albedo` (its color times its texture): diffuse and specular, or
/// ambient for an ambient light. Unlit materials take no light.
pub fn light(hit: &Hit, material: &Material, albedo: [f32; 3], light: &Light) -> Shaded {
    light_shadowed(hit, material, albedo, light, &|_, _| 1.0).0
}

/// [`light`] with shadows: `visible(sample, l)` is how much of the light
/// through shadow sample `sample` (see [`Light::shadow_points`]), coming
/// from direction `l` (unit, toward the light), reaches the hit, 0 to 1. Also the light the shadows took away (diffuse and
/// specular), for the shadow pass.
pub fn light_shadowed(hit: &Hit, material: &Material, albedo: [f32; 3], light: &Light, visible: &dyn Fn(usize, &Vec3) -> f32) -> (Shaded, [f32; 3]) {
    let mut out = Shaded::default();
    let mut lost = [0.0f32; 3];
    if material.shading == Shading::Unlit {
        return (out, lost);
    }
    let tint = |k: f64| -> [f32; 3] { std::array::from_fn(|c| light.color[c] * k as f32) };
    if light.kind == LightKind::Ambient {
        let e = tint(if light.enabled { light.intensity as f64 } else { 0.0 });
        out.ambient = std::array::from_fn(|c| albedo[c] * e[c]);
        return (out, lost);
    }
    // Two-sided: the side facing the eye is lit.
    let n = if hit.normal.dot(&hit.view) < 0.0 { -hit.normal } else { hit.normal };
    let blinn = material.shading == Shading::Blinn;
    let exponent = shininess(material.roughness);
    let mut looked: Vec<(usize, f32)> = Vec::new();
    for (l, e, sample) in light.arriving(&hit.position) {
        let cos = n.dot(&l);
        if cos <= 0.0 {
            continue;
        }
        let e = tint(e);
        // (A light with a radius sends every disk sample through one
        // shadow sample: look it up once.)
        let seen = if !light.shadows {
            1.0
        } else if let Some(&(_, v)) = looked.iter().find(|(k, _)| *k == sample) {
            v
        } else {
            let v = visible(sample, &l).clamp(0.0, 1.0);
            looked.push((sample, v));
            v
        };
        let mut add = [0.0f32; 3];
        for c in 0..3 {
            add[c] = albedo[c] * e[c] * cos as f32;
            out.diffuse[c] += add[c] * seen;
        }
        if blinn {
            let h = (l + hit.view).try_normalize(1e-300).unwrap_or(n);
            let k = n.dot(&h).max(0.0).powf(exponent) as f32;
            for c in 0..3 {
                let sp = material.specular[c] * e[c] * k;
                out.specular[c] += sp * seen;
                add[c] += sp;
            }
        }
        for c in 0..3 {
            lost[c] += add[c] * (1.0 - seen);
        }
    }
    (out, lost)
}

/// `hit` on `material` (color `albedo` there) lit by `lights`, in parts.
/// With no lights at all, or an Unlit material, the surface shows its color
/// as it is (in `emission`, with the material's own emission), as cards
/// render without lights.
pub fn shade(hit: &Hit, material: &Material, albedo: [f32; 3], lights: &[Light]) -> Shaded {
    shade_shadowed(hit, material, albedo, lights, &|_, _, _| 1.0).0
}

/// [`shade`] with shadows: `visible(light, sample, l)` is how much of
/// light `light` (its index in `lights`) through its shadow sample
/// `sample`, from direction `l`, reaches the hit (see [`light_shadowed`]). Also the light the shadows
/// took away.
pub fn shade_shadowed(hit: &Hit, material: &Material, albedo: [f32; 3], lights: &[Light], visible: &dyn Fn(usize, usize, &Vec3) -> f32) -> (Shaded, [f32; 3]) {
    let mut out = Shaded { emission: material.emission, ..Shaded::default() };
    let mut lost = [0.0f32; 3];
    if lights.is_empty() || material.shading == Shading::Unlit {
        out.emission = std::array::from_fn(|c| out.emission[c] + albedo[c]);
        return (out, lost);
    }
    for (i, l) in lights.iter().enumerate() {
        let (lit, gone) = light_shadowed(hit, material, albedo, l, &|k, d| visible(i, k, d));
        out.add(&lit);
        (0..3).for_each(|c| lost[c] += gone[c]);
    }
    (out, lost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom;

    /// A light at `t` turned by `r` (degrees, as nodes have them).
    fn placed(kind: LightKind, t: [f64; 3], r: [f64; 3]) -> Light {
        Light::new(kind, geom::trs(t, r, [1.0; 3]))
    }

    fn lambert() -> Material {
        Material { shading: Shading::Lambert, ..Material::default() }
    }

    /// A point on the floor (normal up) at the origin, seen from straight
    /// above.
    fn floor() -> Hit {
        Hit { position: Vec3::zeros(), normal: Vec3::y(), view: Vec3::y() }
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() < 1e-5)
    }

    #[test]
    fn diffuse_follows_the_cosine_law() {
        let red = [1.0, 0.2, 0.0];
        // Straight down (turned −90° about X, −Z points down).
        let sun = placed(LightKind::Directional, [0.0; 3], [-90.0, 0.0, 0.0]);
        assert!((sun.direction() + Vec3::y()).norm() < 1e-12);
        let s = shade(&floor(), &lambert(), red, std::slice::from_ref(&sun));
        assert!(close(s.diffuse, red) && s.specular == [0.0; 3]);
        // 60° from straight down: half.
        let low = placed(LightKind::Directional, [0.0; 3], [-30.0, 0.0, 0.0]);
        let s = shade(&floor(), &lambert(), red, &[low]);
        assert!(close(s.diffuse, [0.5, 0.1, 0.0]), "{s:?}");
        // From below the floor: nothing (a point light too).
        let under = placed(LightKind::Directional, [0.0; 3], [90.0, 0.0, 0.0]);
        assert_eq!(shade(&floor(), &lambert(), red, &[under]).total(), [0.0; 3]);
        let mut below = placed(LightKind::Point, [0.0, -1.0, 0.0], [0.0; 3]);
        below.decay = 0.0;
        assert_eq!(shade(&floor(), &lambert(), red, &[below]).diffuse, [0.0; 3]);
        // Seen from below, the underside faces the eye and is the lit side.
        let from_below = Hit { view: -Vec3::y(), ..floor() };
        let up = placed(LightKind::Directional, [0.0; 3], [90.0, 0.0, 0.0]);
        assert!(close(shade(&from_below, &lambert(), red, &[up]).diffuse, red));
        // Color and intensity scale it.
        let tinted = Light { color: [0.5, 1.0, 1.0], intensity: 2.0, ..sun };
        assert!(close(shade(&floor(), &lambert(), red, &[tinted]).diffuse, [1.0, 0.4, 0.0]));
    }

    #[test]
    fn the_highlight_peaks_in_the_mirror_direction() {
        let shiny = |roughness: f32| Material { shading: Shading::Blinn, specular: [0.8; 3], roughness, ..Material::default() };
        // Light coming in 45° from +X (from up and to the right): pointing
        // down, then turned 45° about Z.
        let sun = placed(LightKind::Directional, [0.0; 3], [-90.0, 0.0, -45.0]);
        let l = -sun.direction();
        assert!((l - Vec3::new(1.0, 1.0, 0.0).normalize()).norm() < 1e-9, "{l}");
        let seen = |angle: f64, m: &Material| {
            let a = angle.to_radians();
            let hit = Hit { view: Vec3::new(-a.sin(), a.cos(), 0.0), ..floor() };
            shade(&hit, m, [0.0; 3], std::slice::from_ref(&sun)).specular[0]
        };
        // The mirror of l about the normal: 45° the other way, where it's
        // exactly the specular color times the light.
        let m = shiny(0.3);
        let peak = seen(45.0, &m);
        assert!((peak - 0.8).abs() < 1e-5, "{peak}");
        for off in [30.0, 40.0, 50.0, 60.0, 0.0] {
            assert!(seen(off, &m) < peak, "{off}° beats the mirror direction");
        }
        // Sharper with less roughness; none on a Lambert material.
        assert!(seen(35.0, &shiny(0.1)) < seen(35.0, &shiny(0.5)));
        assert_eq!(seen(45.0, &Material { shading: Shading::Lambert, ..m }), 0.0);
        assert!((shininess(0.1) - 198.0).abs() < 1e-4, "Ns 198 ↔ roughness 0.1");
    }

    #[test]
    fn a_spot_lights_its_cone_with_a_soft_edge() {
        // A spot 1 above the floor pointing down, 60° across, the last 10°
        // soft; no falloff.
        let mut spot = placed(LightKind::Spot { cone: 60.0, penumbra: 10.0, falloff: 0.0 }, [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0]);
        spot.decay = 0.0;
        let at = |spot: &Light, deg: f64| {
            let x = deg.to_radians().tan();
            let hit = Hit { position: Vec3::new(x, 0.0, 0.0), ..floor() };
            shade(&hit, &lambert(), [1.0; 3], std::slice::from_ref(spot)).diffuse[0] as f64 / deg.to_radians().cos()
        };
        // Full inside 20°, half at the penumbra's middle (25°), none from
        // the edge (30°) out.
        for deg in [0.0, 10.0, 19.9] {
            assert!((at(&spot, deg) - 1.0).abs() < 1e-6, "{deg}°: {}", at(&spot, deg));
        }
        assert!((at(&spot, 25.0) - 0.5).abs() < 1e-6, "{}", at(&spot, 25.0));
        assert!(at(&spot, 22.0) > at(&spot, 25.0) && at(&spot, 25.0) > at(&spot, 28.0) && at(&spot, 28.0) > 0.0);
        assert!(at(&spot, 30.0) < 1e-12 && at(&spot, 45.0) == 0.0);
        // A hard edge with no penumbra.
        spot.kind = LightKind::Spot { cone: 60.0, penumbra: 0.0, falloff: 0.0 };
        assert!((at(&spot, 29.9) - 1.0).abs() < 1e-6 && at(&spot, 30.1) == 0.0);
    }

    #[test]
    fn falloff_follows_the_decay() {
        let lit = |decay: f64, d: f64| {
            let mut p = placed(LightKind::Point, [0.0, d, 0.0], [0.0; 3]);
            p.decay = decay;
            shade(&floor(), &lambert(), [1.0; 3], &[p]).diffuse[0] as f64
        };
        for (decay, ratio) in [(0.0, 1.0), (1.0, 2.0), (2.0, 4.0), (3.0, 8.0)] {
            assert!((lit(decay, 1.0) / lit(decay, 2.0) - ratio).abs() < 1e-5, "decay {decay}");
        }
        assert!((lit(2.0, 1.0) - 1.0).abs() < 1e-6, "one unit away: the intensity");
        // An area light far away is nearly a point light facing the floor.
        let mut area = placed(LightKind::rect([1.0, 1.0]), [0.0, 50.0, 0.0], [-90.0, 0.0, 0.0]);
        area.intensity = 2500.0;
        let a = shade(&floor(), &lambert(), [1.0; 3], &[area.clone()]).diffuse[0];
        assert!((a - 1.0).abs() < 1e-3, "{a}");
        // Turned away, it gives nothing.
        area.world = geom::trs([0.0, 50.0, 0.0], [90.0, 0.0, 0.0], [1.0; 3]);
        assert_eq!(shade(&floor(), &lambert(), [1.0; 3], &[area]).diffuse[0], 0.0);
    }

    /// An area light 1 × 1, 2 above the floor, facing down, with physical
    /// falloff, at Quality `samples`.
    fn panel(disk: bool, two_sided: bool, samples: usize) -> Light {
        let mut l = placed(LightKind::Area { size: [1.0, 1.0], disk, two_sided, samples }, [0.0, 2.0, 0.0], [-90.0, 0.0, 0.0]);
        l.intensity = 4.0;
        l
    }

    #[test]
    fn an_area_light_matches_a_dense_reference() {
        let lit = |l: &Light, x: f64| {
            let hit = Hit { position: Vec3::new(x, 0.0, 0.3), ..floor() };
            shade(&hit, &lambert(), [1.0; 3], std::slice::from_ref(l)).diffuse[0] as f64
        };
        for disk in [false, true] {
            let reference = panel(disk, false, 128);
            for x in [0.0, 0.5, 1.0, 2.0, 4.0] {
                let (got, want) = (lit(&panel(disk, false, 4), x), lit(&reference, x));
                assert!(((got - want) / want).abs() < 0.01, "disk {disk}, x {x}: {got} vs {want}");
            }
        }
        // Under its middle, nearly a point light of the same intensity 2
        // away (a little less: its edges are further and slanted); a disk,
        // smaller than the square, a little more.
        let (square, round) = (lit(&panel(false, false, 64), 0.0), lit(&panel(true, false, 64), 0.0));
        assert!(square < 1.0 && square > 0.85 && round > square && round < 1.0, "{square} {round}");
        // Turned over, one-sided gives nothing and two-sided the same.
        let mut flipped = panel(false, true, 4);
        flipped.world = geom::trs([0.0, 2.0, 0.0], [90.0, 0.0, 0.0], [1.0; 3]);
        assert!((lit(&flipped, 0.5) - lit(&panel(false, false, 4), 0.5)).abs() < 1e-6);
        flipped.kind = LightKind::Area { size: [1.0, 1.0], disk: false, two_sided: false, samples: 4 };
        assert_eq!(lit(&flipped, 0.5), 0.0);
    }

    #[test]
    fn falloff_dims_a_spot_toward_its_edge() {
        let mut spot = placed(LightKind::Spot { cone: 90.0, penumbra: 0.0, falloff: 2.0 }, [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0]);
        spot.decay = 0.0;
        let at = |deg: f64| {
            let hit = Hit { position: Vec3::new(deg.to_radians().tan(), 0.0, 0.0), ..floor() };
            shade(&hit, &lambert(), [1.0; 3], std::slice::from_ref(&spot)).diffuse[0] as f64 / deg.to_radians().cos()
        };
        // cos² of the angle from the axis.
        assert!((at(0.0) - 1.0).abs() < 1e-6);
        assert!((at(30.0) - 0.75).abs() < 1e-6 && (at(44.0) - 44f64.to_radians().cos().powi(2)).abs() < 1e-6, "{}", at(30.0));
    }

    #[test]
    fn a_radius_softens_the_highlight_not_the_light() {
        let shiny = Material { shading: Shading::Blinn, specular: [1.0; 3], roughness: 0.1, ..Material::default() };
        let bulb = |radius: f64| Light { radius, decay: 2.0, ..placed(LightKind::Point, [0.0, 4.0, 0.0], [0.0; 3]) };
        // Seen from the mirror direction of the light, then off it.
        let seen = |l: &Light, x: f64| {
            let hit = Hit { position: Vec3::new(x, 0.0, 0.0), normal: Vec3::y(), view: Vec3::new(-x, 4.0, 0.0).normalize() };
            shade(&hit, &shiny, [0.5; 3], std::slice::from_ref(l))
        };
        let (sharp, soft) = (bulb(0.0), bulb(0.5));
        let d = |l: &Light| seen(l, 0.0).diffuse[0];
        assert!(((d(&soft) - d(&sharp)) / d(&sharp)).abs() < 0.02, "the diffuse barely changes: {} {}", d(&soft), d(&sharp));
        let s = |l: &Light, x: f64| seen(l, x).specular[0];
        assert!(s(&soft, 0.0) < s(&sharp, 0.0), "a lower peak");
        assert!(s(&soft, 0.6) > s(&sharp, 0.6) && s(&soft, 1.0) > 1.5 * s(&sharp, 1.0), "spread wider");
        // A soft spot keeps its cone.
        let spot = Light { radius: 0.5, decay: 0.0, ..placed(LightKind::Spot { cone: 40.0, penumbra: 0.0, falloff: 0.0 }, [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0]) };
        let lit = |deg: f64| shade(&Hit { position: Vec3::new(deg.to_radians().tan(), 0.0, 0.0), ..floor() }, &lambert(), [1.0; 3], std::slice::from_ref(&spot)).diffuse[0];
        assert!(lit(19.0) > 0.5 && lit(21.0) == 0.0);
    }

    #[test]
    fn a_light_off_lights_nothing() {
        let sun = placed(LightKind::Directional, [0.0; 3], [-90.0, 0.0, 0.0]);
        let off = Light { enabled: false, ..sun.clone() };
        let amb = Light { enabled: false, ..placed(LightKind::Ambient, [0.0; 3], [0.0; 3]) };
        let s = shade(&floor(), &lambert(), [1.0; 3], &[off.clone(), amb]);
        assert_eq!(s.total(), [0.0; 3], "still a lit scene, only dark");
        assert!(close(shade(&floor(), &lambert(), [1.0; 3], &[off, sun]).diffuse, [1.0; 3]));
    }

    #[test]
    fn the_parts_add_up_to_the_beauty() {
        let m = Material { shading: Shading::Blinn, color: [0.7, 0.5, 0.3], specular: [0.4; 3], roughness: 0.4, emission: [0.05, 0.0, 0.1], ..Material::default() };
        let albedo = m.color;
        let mut lights = vec![
            Light { color: [0.2, 0.2, 0.3], ..placed(LightKind::Ambient, [0.0; 3], [0.0; 3]) },
            Light { intensity: 3.0, ..placed(LightKind::Point, [1.0, 2.0, 0.5], [0.0; 3]) },
            placed(LightKind::Directional, [0.0; 3], [-60.0, 20.0, 0.0]),
            placed(LightKind::Spot { cone: 90.0, penumbra: 20.0, falloff: 0.0 }, [-1.0, 3.0, 0.0], [-75.0, -20.0, 0.0]),
        ];
        lights[1].node = "key".into();
        let hit = Hit { view: Vec3::new(0.3, 1.0, 0.4).normalize(), ..floor() };
        let s = shade(&hit, &m, albedo, &lights);
        assert!(s.diffuse.iter().all(|&v| v > 0.0) && s.specular.iter().all(|&v| v > 0.0));
        assert!(s.emission == m.emission && close(s.ambient, [0.14, 0.1, 0.09]), "{s:?}");
        let sum = s.total();
        assert!(close(sum, std::array::from_fn(|c| s.diffuse[c] + s.specular[c] + s.emission[c] + s.ambient[c])));
        // Each light's share alone, plus the emission, makes the whole.
        let mut parts = Shaded { emission: m.emission, ..Shaded::default() };
        for l in &lights {
            parts.add(&light(&hit, &m, albedo, l));
        }
        assert!(close(parts.total(), sum) && close(parts.diffuse, s.diffuse) && close(parts.specular, s.specular));
        // Without lights, or unlit, the color shows as it is.
        let plain = shade(&hit, &m, albedo, &[]);
        assert!(close(plain.total(), [0.75, 0.5, 0.4]) && plain.diffuse == [0.0; 3]);
        let unlit = Material { shading: Shading::Unlit, ..m.clone() };
        assert!(close(shade(&hit, &unlit, albedo, &lights).total(), [0.75, 0.5, 0.4]));
    }
}
