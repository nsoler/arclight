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
//! distance `d` as `1 / d^decay` (0 none, 1 linear, 2 physical).

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
    /// inward).
    Spot { cone: f64, penumbra: f64 },
    /// From a rectangle `size` across in the light's XY plane, facing −Z
    /// (sampled at a 4 × 4 grid of points).
    Area { size: [f64; 2] },
}

/// A light in the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub kind: LightKind,
    /// Color (linear) and how strong: the light reaching a surface facing
    /// it one unit away is `color × intensity`.
    pub color: [f32; 3],
    pub intensity: f32,
    /// Falloff exponent with distance.
    pub decay: f64,
    /// Placement: position its translation, pointing down its −Z.
    pub world: Mat4,
    /// The node it came from (for per-light passes; "" if none).
    pub node: String,
}

impl Light {
    /// A light of `kind` placed by `world`, white, intensity 1, physical
    /// (quadratic) falloff.
    pub fn new(kind: LightKind, world: Mat4) -> Light {
        Light { kind, color: [1.0; 3], intensity: 1.0, decay: 2.0, world, node: String::new() }
    }

    pub fn position(&self) -> Vec3 {
        self.world.fixed_view::<3, 1>(0, 3).into_owned()
    }

    /// Which way it shines (unit): its −Z in the world.
    pub fn direction(&self) -> Vec3 {
        let d: Vec3 = -self.world.fixed_view::<3, 1>(0, 2).into_owned();
        d.try_normalize(1e-300).unwrap_or(-Vec3::z())
    }

    /// The light arriving at `p` from point `from` (a direction to the light
    /// and the strength, color aside), with falloff.
    fn reaching(&self, from: Vec3, p: &Vec3) -> Option<(Vec3, f64)> {
        let to = from - p;
        let d = to.norm();
        (d > 1e-12).then(|| (to / d, self.intensity as f64 / d.powf(self.decay)))
    }

    /// Where the light comes from at `p`: (unit direction to it, strength)
    /// pairs, color aside; empty for ambient light.
    fn arriving(&self, p: &Vec3) -> Vec<(Vec3, f64)> {
        match self.kind {
            LightKind::Ambient => Vec::new(),
            LightKind::Directional => vec![(-self.direction(), self.intensity as f64)],
            LightKind::Point => self.reaching(self.position(), p).into_iter().collect(),
            LightKind::Spot { cone, penumbra } => {
                let Some((l, e)) = self.reaching(self.position(), p) else { return Vec::new() };
                let angle = (-l).angle(&self.direction()).to_degrees();
                let edge = cone / 2.0;
                let soft = penumbra.clamp(0.0, edge);
                let k = if angle >= edge {
                    0.0
                } else if angle <= edge - soft {
                    1.0
                } else {
                    smoothstep((edge - angle) / soft)
                };
                if k > 0.0 { vec![(l, e * k)] } else { Vec::new() }
            }
            LightKind::Area { size } => {
                // A grid of points over the rectangle, each a sixteenth of
                // the light, weighted by how squarely the light faces p.
                const N: usize = 4;
                let facing = self.direction();
                let mut out = Vec::with_capacity(N * N);
                for j in 0..N {
                    for i in 0..N {
                        let (x, y) = ((i as f64 + 0.5) / N as f64 - 0.5, (j as f64 + 0.5) / N as f64 - 0.5);
                        let at = crate::geom::transform_point(&self.world, &Vec3::new(x * size[0], y * size[1], 0.0));
                        if let Some((l, e)) = self.reaching(at, p) {
                            let cos = facing.dot(&-l);
                            if cos > 0.0 {
                                out.push((l, e * cos / (N * N) as f64));
                            }
                        }
                    }
                }
                out
            }
        }
    }
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
    let mut out = Shaded::default();
    if material.shading == Shading::Unlit {
        return out;
    }
    let tint = |k: f64| -> [f32; 3] { std::array::from_fn(|c| light.color[c] * k as f32) };
    if light.kind == LightKind::Ambient {
        let e = tint(light.intensity as f64);
        out.ambient = std::array::from_fn(|c| albedo[c] * e[c]);
        return out;
    }
    // Two-sided: the side facing the eye is lit.
    let n = if hit.normal.dot(&hit.view) < 0.0 { -hit.normal } else { hit.normal };
    let blinn = material.shading == Shading::Blinn;
    let exponent = shininess(material.roughness);
    for (l, e) in light.arriving(&hit.position) {
        let cos = n.dot(&l);
        if cos <= 0.0 {
            continue;
        }
        let e = tint(e);
        for c in 0..3 {
            out.diffuse[c] += albedo[c] * e[c] * cos as f32;
        }
        if blinn {
            let h = (l + hit.view).try_normalize(1e-300).unwrap_or(n);
            let k = n.dot(&h).max(0.0).powf(exponent) as f32;
            out.specular = std::array::from_fn(|c| out.specular[c] + material.specular[c] * e[c] * k);
        }
    }
    out
}

/// `hit` on `material` (color `albedo` there) lit by `lights`, in parts.
/// With no lights at all, or an Unlit material, the surface shows its color
/// as it is (in `emission`, with the material's own emission), as cards
/// render without lights.
pub fn shade(hit: &Hit, material: &Material, albedo: [f32; 3], lights: &[Light]) -> Shaded {
    let mut out = Shaded { emission: material.emission, ..Shaded::default() };
    if lights.is_empty() || material.shading == Shading::Unlit {
        out.emission = std::array::from_fn(|c| out.emission[c] + albedo[c]);
        return out;
    }
    for l in lights {
        out.add(&light(hit, material, albedo, l));
    }
    out
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
        let mut spot = placed(LightKind::Spot { cone: 60.0, penumbra: 10.0 }, [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0]);
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
        spot.kind = LightKind::Spot { cone: 60.0, penumbra: 0.0 };
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
        let mut area = placed(LightKind::Area { size: [1.0, 1.0] }, [0.0, 50.0, 0.0], [-90.0, 0.0, 0.0]);
        area.intensity = 2500.0;
        let a = shade(&floor(), &lambert(), [1.0; 3], &[area.clone()]).diffuse[0];
        assert!((a - 1.0).abs() < 1e-3, "{a}");
        // Turned away, it gives nothing.
        area.world = geom::trs([0.0, 50.0, 0.0], [90.0, 0.0, 0.0], [1.0; 3]);
        assert_eq!(shade(&floor(), &lambert(), [1.0; 3], &[area]).diffuse[0], 0.0);
    }

    #[test]
    fn the_parts_add_up_to_the_beauty() {
        let m = Material { shading: Shading::Blinn, color: [0.7, 0.5, 0.3], specular: [0.4; 3], roughness: 0.4, emission: [0.05, 0.0, 0.1], ..Material::default() };
        let albedo = m.color;
        let mut lights = vec![
            Light { color: [0.2, 0.2, 0.3], ..placed(LightKind::Ambient, [0.0; 3], [0.0; 3]) },
            Light { intensity: 3.0, ..placed(LightKind::Point, [1.0, 2.0, 0.5], [0.0; 3]) },
            placed(LightKind::Directional, [0.0; 3], [-60.0, 20.0, 0.0]),
            placed(LightKind::Spot { cone: 90.0, penumbra: 20.0 }, [-1.0, 3.0, 0.0], [-75.0, -20.0, 0.0]),
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
