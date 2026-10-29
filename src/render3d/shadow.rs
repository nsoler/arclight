//! Shadow maps for Scanline Render's lights.
//!
//! For each light that casts shadows, the shadow casters (the surfaces
//! with `casts`) are drawn from the light into depth images holding, per
//! texel, how far the nearest caster is: one through a spot light's cone;
//! a cube of six around a point light, and around each sample point of an
//! area light (so its shadows soften as its samples see past an edge);
//! for a directional light one looking down its direction from far away,
//! covering the scene. A point is in shadow where something in the map is
//! nearer the light than it. Edges are filtered over a square of texels
//! (percentage-closer filtering, `shadow_softness` texels across each way,
//! each tap bilinear); a point or spot light with a radius widens the
//! filter by how far the point is behind its blocker (the penumbra a light
//! that wide casts), found by averaging the blockers near it first.
//! A card's transparent parts (alpha under a half) don't cast.
//!
//! The point looked up is moved off its surface toward the light by about
//! a texel, so a surface doesn't shadow itself.

use super::{shade as surface_color, triangles, Surface};
use crate::geom::{self, Camera, Vec3};
use crate::image::Size;
use crate::scene::aim;
use crate::shade::{Light, LightKind};
use rayon::prelude::*;

/// Texels across a spot light's map, a cube face (a point light's; an
/// area light's sample, depending on how many there are) and a
/// directional light's map.
const SPOT_RES: usize = 1024;
const CUBE_RES: usize = 512;
const AREA_RES: usize = 256;
const AREA_RES_MANY: usize = 128;
const SUN_RES: usize = 2048;

/// One depth image from a light.
struct Map {
    cam: Camera,
    res: usize,
    /// Per texel, the nearest caster's distance (see [`Map::key`]); infinite
    /// where nothing is.
    depth: Vec<f32>,
    /// A directional light's direction: distances are along it from `origin`
    /// rather than straight from the light.
    along: Option<Vec3>,
    origin: Vec3,
    /// A texel's width at one unit away (perspective), or in scene units
    /// (directional).
    texel: f64,
}

impl Map {
    /// Draw the casters of `surfs` through `cam` into a `res` square.
    fn draw(surfs: &[Surface], cam: Camera, res: usize, along: Option<Vec3>, texel: f64) -> Map {
        let origin = geom::transform_point(&cam.world, &Vec3::zeros());
        let size = Size { width: res, height: res };
        let mut map = Map { cam, res, depth: vec![f32::INFINITY; res * res], along, origin, texel };
        for t in triangles(surfs, &map.cam, size, None) {
            let s = &surfs[t.surf];
            if !s.casts || s.holdout {
                continue;
            }
            for y in t.bounds.y0.max(0)..t.bounds.y1.min(res as i32) {
                for x in t.bounds.x0.max(0)..t.bounds.x1.min(res as i32) {
                    let Some((_, b)) = t.cover([x as f64 + 0.5, y as f64 + 0.5], map.cam.near, map.cam.far) else { continue };
                    if s.texture.is_some() && surface_color(s, t.uv(&b))[3] < 0.5 {
                        continue;
                    }
                    let p: Vec3 = t.v.iter().zip(&b).map(|(v, w)| v.world * *w).sum();
                    let k = map.key(&p) as f32;
                    let d = &mut map.depth[y as usize * res + x as usize];
                    if k < *d {
                        *d = k;
                    }
                }
            }
        }
        map
    }

    /// How far `p` is from the light, as the map measures it.
    fn key(&self, p: &Vec3) -> f64 {
        match self.along {
            Some(d) => (p - self.origin).dot(&d),
            None => (p - self.origin).norm(),
        }
    }

    /// A texel's width in the scene at `p`.
    fn texel_at(&self, p: &Vec3) -> f64 {
        if self.along.is_some() { self.texel } else { self.texel * self.key(p) }
    }

    /// Where `p` falls in the map (texel units), if in front of it.
    fn place(&self, p: &Vec3) -> Option<[f64; 2]> {
        let (q, _) = self.cam.project(p, Size { width: self.res, height: self.res })?;
        Some(q)
    }

    fn at(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x >= self.res as i64 || y >= self.res as i64 {
            return f32::INFINITY;
        }
        self.depth[y as usize * self.res + x as usize]
    }

    /// How lit (0 to 1) a point `key` away is at texel position `q`: the
    /// four texels round it compared, weighted bilinearly.
    fn lit_at(&self, q: [f64; 2], key: f32) -> f32 {
        let (x, y) = (q[0] - 0.5, q[1] - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
        let lit = |dx: i64, dy: i64| if self.at(x0 as i64 + dx, y0 as i64 + dy) < key { 0.0 } else { 1.0 };
        (lit(0, 0) * (1.0 - fx) + lit(1, 0) * fx) * (1.0 - fy) + (lit(0, 1) * (1.0 - fx) + lit(1, 1) * fx) * fy
    }

    /// The mean distance of the casters nearer the light than `key` within
    /// `r` texels of `q`, if any.
    fn blockers(&self, q: [f64; 2], key: f32, r: f64) -> Option<f32> {
        let m = r.ceil().clamp(1.0, 4.0) as i64;
        let step = r / m as f64;
        let (mut sum, mut n) = (0.0, 0);
        for j in -m..=m {
            for i in -m..=m {
                let d = self.at((q[0] + i as f64 * step).floor() as i64, (q[1] + j as f64 * step).floor() as i64);
                if d < key {
                    sum += d;
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / n as f32)
    }

    /// How lit `p` is, filtered over `soft` texels each way (and, for a
    /// light `radius` wide, the penumbra its blockers make).
    fn lit(&self, p: &Vec3, soft: f64, radius: f64) -> f32 {
        let Some(q) = self.place(p) else { return 1.0 };
        let key = self.key(p);
        let texel = self.texel_at(p);
        let k = key as f32;
        let mut r = soft;
        if radius > 0.0 && self.along.is_none() {
            let search = (radius / texel).clamp(1.0, 16.0);
            match self.blockers(q, k, search) {
                Some(b) if b > 0.0 => r += radius * (key - b as f64) / b as f64 / texel,
                _ => return 1.0,
            }
        }
        let r = r.min(24.0);
        if r < 0.5 {
            return self.lit_at(q, k);
        }
        let m = r.ceil().min(4.0) as i64;
        let step = r / m as f64;
        let mut sum = 0.0;
        for j in -m..=m {
            for i in -m..=m {
                sum += self.lit_at([q[0] + i as f64 * step, q[1] + j as f64 * step], k);
            }
        }
        sum / ((2 * m + 1) * (2 * m + 1)) as f32
    }
}

/// The shadow maps of one point the light shines from.
enum Source {
    One(Box<Map>),
    /// Six faces round `at`: +X, −X, +Y, −Y, +Z, −Z.
    Cube { at: Vec3, faces: Vec<Map> },
}

/// A pinhole at `world` seeing `fov` degrees across a square frame.
fn pinhole(world: geom::Mat4, fov: f64, near: f64, far: f64) -> Camera {
    let film = 36.0;
    Camera { focal: film / (2.0 * (fov / 2.0).to_radians().tan()), film_back: [film, film], world, near, far }
}

/// Texel width at one unit away for a square map `fov` across.
fn texel_of(fov: f64, res: usize) -> f64 {
    2.0 * (fov / 2.0).to_radians().tan() / res as f64
}

const AXES: [[f64; 3]; 6] = [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];

impl Source {
    /// A cube round `at` (each face a little over 90° across, so filters
    /// near an edge stay inside one).
    fn cube(surfs: &[Surface], at: Vec3, res: usize, near: f64, far: f64) -> Source {
        const FOV: f64 = 96.0;
        let faces = AXES.par_iter().map(|a| Map::draw(surfs, pinhole(aim(at, at + Vec3::from(*a)).expect("an axis"), FOV, near, far), res, None, texel_of(FOV, res))).collect();
        Source::Cube { at, faces }
    }

    fn lit(&self, p: &Vec3, soft: f64, radius: f64) -> f32 {
        match self {
            Source::One(m) => m.lit(p, soft, radius),
            Source::Cube { at, faces } => {
                let v = p - at;
                let a = v.abs();
                let face = if a.x >= a.y && a.x >= a.z {
                    if v.x > 0.0 { 0 } else { 1 }
                } else if a.y >= a.z {
                    if v.y > 0.0 { 2 } else { 3 }
                } else if v.z > 0.0 {
                    4
                } else {
                    5
                };
                faces[face].lit(p, soft, radius)
            }
        }
    }

    fn texel_at(&self, p: &Vec3) -> f64 {
        match self {
            Source::One(m) => m.texel_at(p),
            Source::Cube { faces, .. } => faces[0].texel * (p - faces[0].origin).norm(),
        }
    }
}

/// A light's shadow maps, by shadow sample (see
/// [`Light::shadow_points`]).
struct LightShadows {
    sources: Vec<Source>,
    soft: f64,
    radius: f64,
}

/// The shadow maps of a render's lights.
pub(super) struct Shadows {
    lights: Vec<Option<LightShadows>>,
}

/// A sphere holding every surface's corners in the scene.
fn extent(surfs: &[Surface]) -> Option<(Vec3, f64)> {
    let pts: Vec<Vec3> = surfs.iter().flat_map(|s| s.positions.iter().map(|p| geom::transform_point(&s.world, p))).collect();
    let first = *pts.first()?;
    let (lo, hi) = pts.iter().fold((first, first), |(lo, hi), p| (lo.inf(p), hi.sup(p)));
    let c = (lo + hi) / 2.0;
    Some((c, pts.iter().map(|p| (p - c).norm()).fold(0.0, f64::max).max(1e-6)))
}

impl Shadows {
    /// Shadow maps for each of `lights` that casts shadows (and is on),
    /// of the casters among `surfs`.
    pub fn build(surfs: &[Surface], lights: &[Light]) -> Shadows {
        let wanted = lights.iter().any(|l| l.shadows && l.enabled && l.kind != LightKind::Ambient) && surfs.iter().any(|s| s.casts);
        let scene = if wanted { extent(surfs) } else { None };
        let lights = lights
            .iter()
            .map(|l| {
                let (c, r) = scene?;
                if !l.shadows || !l.enabled {
                    return None;
                }
                let reach = |at: Vec3| ((at - c).norm() + r) * 1.01;
                let near_for = |at: Vec3| (reach(at) * 1e-5).max(1e-4);
                let sources = match l.kind {
                    LightKind::Ambient => return None,
                    LightKind::Directional => {
                        let dir = l.direction();
                        let d = r * 1000.0;
                        let from = c - dir * d;
                        let fov = 2.0 * (r * 1.02 / d).atan().to_degrees();
                        let cam = pinhole(aim(from, c)?, fov, d - r * 1.1, d + r * 1.1);
                        vec![Source::One(Box::new(Map::draw(surfs, cam, SUN_RES, Some(dir), 2.0 * r * 1.02 / SUN_RES as f64)))]
                    }
                    LightKind::Spot { cone, .. } if cone + 10.0 <= 170.0 => {
                        let fov = cone + 10.0;
                        let at = l.position();
                        let cam = pinhole(l.world, fov, near_for(at), reach(at));
                        vec![Source::One(Box::new(Map::draw(surfs, cam, SPOT_RES, None, texel_of(fov, SPOT_RES))))]
                    }
                    LightKind::Spot { .. } | LightKind::Point => {
                        let at = l.position();
                        vec![Source::cube(surfs, at, CUBE_RES, near_for(at), reach(at))]
                    }
                    LightKind::Area { samples, .. } => {
                        let res = if samples <= 4 { AREA_RES } else { AREA_RES_MANY };
                        l.shadow_points().into_par_iter().map(|at| Source::cube(surfs, at, res, near_for(at), reach(at))).collect()
                    }
                };
                Some(LightShadows { sources, soft: l.shadow_softness, radius: if matches!(l.kind, LightKind::Point | LightKind::Spot { .. }) { l.radius } else { 0.0 } })
            })
            .collect();
        Shadows { lights }
    }

    /// How much of light `light`'s shadow sample `sample` reaches point `p`
    /// of a surface with normal `n` (either way), coming from direction
    /// `l` (unit, toward the light): 1 for a light without shadows.
    pub fn visible(&self, light: usize, sample: usize, p: &Vec3, n: &Vec3, l: &Vec3) -> f32 {
        let Some(Some(ls)) = self.lights.get(light) else { return 1.0 };
        let Some(src) = ls.sources.get(sample) else { return 1.0 };
        // Off the surface toward the light, more the more it slants.
        let n = if n.dot(l) < 0.0 { -n } else { *n };
        let texel = src.texel_at(p);
        let slant = (1.0 - n.dot(l).clamp(0.0, 1.0).powi(2)).sqrt() / n.dot(l).max(0.2);
        let off = p + n * texel * (1.5 + ls.soft) + l * texel * (1.0 + slant);
        src.lit(&off, ls.soft, ls.radius)
    }

    /// Whether any light has shadow maps.
    pub fn any(&self) -> bool {
        self.lights.iter().any(Option::is_some)
    }
}
