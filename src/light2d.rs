//! 2D lights: lighting a flat image (a lamp on a plate, a spot on a wall,
//! a sun direction).
//!
//! Each [`Light2d`] has a shape on the image ([`Shape`]): a spot's cone, an
//! area's rectangle or ellipse, a point's radius, a directional light's
//! gradient. That gives how much of it reaches each pixel, 0 to 1
//! ([`Light2d::reach`]). Without normals that is the light: a shaped glow.
//! With normals (a render's `N` pass, or a normal map) the image is
//! treated as a surface and lit as in 3D ([`crate::shade`]: Lambert, and a
//! Blinn–Phong highlight when the light has `specular`), the light
//! standing `height` pixels above it (a directional light at `elevation`
//! degrees); a depth pass can move the surface off its plane.
//!
//! Positions are frame pixels, x right and y down from the frame's
//! top-left, as for shapes; a pixel's middle is (x + 0.5, y + 0.5). Angles
//! are degrees, 0 pointing right, positive clockwise on screen. For
//! shading the image is the plane z = 0 seen from +z, with x right and
//! y up (normals use that space: a flat surface is (0, 0, 1)).
//!
//! The light (color × intensity × 2^exposure × reach × shading) goes onto
//! the image by its [`Blend`], light by light, where the image is (by its
//! alpha) and the mask lets it.

use crate::geom::{self, Vec3};
use crate::image::Image;
use crate::mesh::{Material, Shading};
use crate::shade::{self, Hit, LightKind};
use rayon::prelude::*;

/// Where a 2D light shines on the image.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// From `position`, aimed at `direction` (degrees) within a cone
    /// `cone` degrees across, its edge softened over `softness` degrees
    /// (half bright at the edge itself). Fades with distance: half at
    /// `reach` pixels (0: no fading).
    Spot { position: [f64; 2], direction: f64, cone: f64, softness: f64, reach: f64 },
    /// A rectangle (or the ellipse inside it) `size` across around
    /// `center`, turned `rotation` degrees, its edge softened over
    /// `softness` pixels (half bright at the edge itself).
    Area { center: [f64; 2], size: [f64; 2], rotation: f64, ellipse: bool, softness: f64 },
    /// Around `position`: full there, nothing past `radius`, falling as
    /// (1 − d / radius)^`falloff` (1 linear; higher, a tighter glow).
    Point { position: [f64; 2], radius: f64, falloff: f64 },
    /// From one side of the frame toward `angle` (degrees): full before
    /// the line through `position` across that way, fading to nothing
    /// over `falloff` pixels past it (0: even everywhere).
    Directional { angle: f64, position: [f64; 2], falloff: f64 },
}

/// How a light goes onto the image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blend {
    /// Added (a glow, or light on a dark plate).
    #[default]
    Add,
    /// Screened: like adding, but easing off toward white.
    Screen,
    /// Multiplied: the image lit by it alone (dark where it doesn't reach).
    Multiply,
}

/// A 2D light.
#[derive(Clone, Debug, PartialEq)]
pub struct Light2d {
    pub shape: Shape,
    /// Linear color, strength and exposure (stops).
    pub color: [f32; 3],
    pub intensity: f32,
    pub exposure: f32,
    pub blend: Blend,
    /// With normals: how high above the image the light stands (pixels),
    /// or for a directional light its angle above the image (degrees).
    pub height: f64,
    pub elevation: f64,
    /// With normals: a highlight this strong (0: none), sharp to broad by
    /// `roughness` (0 to 1).
    pub specular: f32,
    pub roughness: f32,
}

impl Light2d {
    pub fn new(shape: Shape) -> Light2d {
        Light2d {
            shape,
            color: [1.0; 3],
            intensity: 1.0,
            exposure: 0.0,
            blend: Blend::Add,
            height: 100.0,
            elevation: 45.0,
            specular: 0.0,
            roughness: 0.5,
        }
    }

    /// How much of the light reaches frame point (`x`, `y`), 0 to 1.
    pub fn reach(&self, x: f64, y: f64) -> f64 {
        match self.shape {
            Shape::Spot { position, direction, cone, softness, reach } => {
                let (dx, dy) = (x - position[0], y - position[1]);
                let d = dx.hypot(dy);
                let fade = if reach > 0.0 { 1.0 / (1.0 + (d / reach).powi(2)) } else { 1.0 };
                if d < 1e-9 {
                    return fade;
                }
                let aim = direction.to_radians();
                let cos = ((dx * aim.cos() + dy * aim.sin()) / d).clamp(-1.0, 1.0);
                edge(cos.acos().to_degrees(), cone / 2.0, softness) * fade
            }
            Shape::Area { center, size, rotation, ellipse, softness } => {
                // In the area's own axes, how far past its edge (pixels;
                // negative inside).
                let r = (-rotation).to_radians();
                let (dx, dy) = (x - center[0], y - center[1]);
                let (u, v) = (dx * r.cos() - dy * r.sin(), dx * r.sin() + dy * r.cos());
                let (a, b) = (size[0].abs() / 2.0, size[1].abs() / 2.0);
                let out = if ellipse {
                    // Along the ray from the middle: the point's distance
                    // less the ellipse's there.
                    let d = u.hypot(v);
                    if d < 1e-12 {
                        -a.min(b)
                    } else {
                        let rim = a * b / ((b * u).powi(2) + (a * v).powi(2)).sqrt().max(1e-300) * d;
                        d - rim
                    }
                } else {
                    let (qx, qy) = (u.abs() - a, v.abs() - b);
                    if qx > 0.0 || qy > 0.0 { qx.max(0.0).hypot(qy.max(0.0)) } else { qx.max(qy) }
                };
                edge(out, 0.0, softness)
            }
            Shape::Point { position, radius, falloff } => {
                if radius <= 0.0 {
                    return 0.0;
                }
                let d = (x - position[0]).hypot(y - position[1]);
                (1.0 - d / radius).max(0.0).powf(falloff.max(0.0))
            }
            Shape::Directional { angle, position, falloff } => {
                if falloff <= 0.0 {
                    return 1.0;
                }
                let a = angle.to_radians();
                let past = (x - position[0]) * a.cos() + (y - position[1]) * a.sin();
                edge(past, falloff / 2.0, falloff)
            }
        }
    }

    /// Color × intensity × exposure.
    fn strength(&self) -> [f32; 3] {
        let k = self.intensity * self.exposure.exp2();
        self.color.map(|c| c * k)
    }

    /// The 3D light shading the surface (white, no falloff with distance:
    /// [`reach`](Self::reach) does that).
    fn light3d(&self) -> shade::Light {
        let above = |p: [f64; 2]| geom::trs([p[0], -p[1], self.height], [0.0; 3], [1.0; 3]);
        let (kind, world) = match self.shape {
            Shape::Spot { position, .. } | Shape::Point { position, .. } => (LightKind::Point, above(position)),
            Shape::Area { center, size, rotation, ellipse, .. } => (
                LightKind::Area { size, disk: ellipse, two_sided: false, samples: 4 },
                // Facing down (its −Z is the world's), turned as on screen.
                geom::trs([center[0], -center[1], self.height], [0.0, 0.0, -rotation], [1.0; 3]),
            ),
            Shape::Directional { angle, .. } => {
                // Shining toward `angle` (y up here) and down at `elevation`.
                let (a, e) = (-angle.to_radians(), self.elevation.to_radians());
                let toward = Vec3::new(a.cos() * e.cos(), a.sin() * e.cos(), -e.sin());
                let z = -toward;
                let x = if z.x.abs() < 0.9 { Vec3::x() } else { Vec3::y() };
                let y = z.cross(&x).normalize();
                let x = y.cross(&z);
                let mut m = geom::Mat4::identity();
                m.fixed_view_mut::<3, 1>(0, 0).copy_from(&x);
                m.fixed_view_mut::<3, 1>(0, 1).copy_from(&y);
                m.fixed_view_mut::<3, 1>(0, 2).copy_from(&z);
                (LightKind::Directional, m)
            }
        };
        shade::Light { decay: 0.0, ..shade::Light::new(kind, world) }
    }
}

/// 1 well inside an edge at `at`, 0 well past it, ½ on it, eased over
/// `soft` (a hard edge for none), for a value `v` growing outward.
fn edge(v: f64, at: f64, soft: f64) -> f64 {
    if soft <= 0.0 {
        return if v < at { 1.0 } else if v > at { 0.0 } else { 0.5 };
    }
    let t = ((at + soft / 2.0 - v) / soft).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The image as a surface, for lighting it as in 3D.
#[derive(Clone, Copy)]
pub enum Normals<'a> {
    /// A render's normal pass: planes `N.x`, `N.y`, `N.z`.
    Pass(&'a Image),
    /// A normal map: RGB 0 to 1 for −1 to 1 (x right, y up, z out).
    Map(&'a Image),
}

impl Normals<'_> {
    /// The unit normal at frame pixel (`x`, `y`); facing out where there's
    /// none.
    fn at(&self, x: i32, y: i32) -> Vec3 {
        let n = match self {
            Normals::Pass(img) => Vec3::new(img.plane_at("N.x", x, y) as f64, img.plane_at("N.y", x, y) as f64, img.plane_at("N.z", x, y) as f64),
            Normals::Map(img) => {
                let c = pixel(img, x, y);
                Vec3::new(c[0] as f64 * 2.0 - 1.0, c[1] as f64 * 2.0 - 1.0, c[2] as f64 * 2.0 - 1.0)
            }
        };
        n.try_normalize(1e-9).unwrap_or_else(Vec3::z)
    }
}

/// Depth: the surface lies `depth × scale` pixels behind the image plane.
/// From the image's `depth` plane (or `depth.Z`, else its red).
#[derive(Clone, Copy)]
pub struct Depth<'a> {
    pub image: &'a Image,
    pub scale: f64,
}

impl Depth<'_> {
    fn at(&self, x: i32, y: i32) -> f64 {
        let img = self.image;
        let d = ["depth", "depth.Z"].iter().find_map(|n| img.plane(n).map(|p| img.plane_value(p, x, y))).unwrap_or_else(|| pixel(img, x, y)[0]);
        d as f64 * self.scale
    }
}

/// `img`'s pixel at frame position (`x`, `y`): transparent outside its
/// data window.
fn pixel(img: &Image, x: i32, y: i32) -> [f32; 4] {
    let (lx, ly) = (x - img.x0, y - img.y0);
    if lx < 0 || ly < 0 || lx >= img.width as i32 || ly >= img.height as i32 {
        [0.0; 4]
    } else {
        img.pixels[ly as usize * img.width + lx as usize]
    }
}

/// `image` lit by `lights`, in order. With `normals` it's shaded as a
/// surface (moved back by `depth`); `mask`'s alpha limits where light
/// goes.
pub fn light2d(image: &Image, lights: &[Light2d], normals: Option<Normals>, depth: Option<Depth>, mask: Option<&Image>) -> Image {
    let mut out = image.clone();
    let prepared: Vec<_> = lights
        .iter()
        .map(|l| {
            let material = Material {
                shading: if l.specular > 0.0 { Shading::Blinn } else { Shading::Lambert },
                specular: [l.specular; 3],
                roughness: l.roughness,
                ..Material::default()
            };
            (l, l.strength(), l.light3d(), material)
        })
        .collect();
    let (x0, y0, width) = (image.x0, image.y0, image.width);
    out.pixels.par_chunks_mut(width.max(1)).enumerate().for_each(|(row, pixels)| {
        let y = y0 + row as i32;
        for (col, px) in pixels.iter_mut().enumerate() {
            let x = x0 + col as i32;
            let m = mask.map_or(1.0, |m| pixel(m, x, y)[3]);
            if m <= 0.0 {
                continue;
            }
            let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
            let at = Vec3::new(cx, -cy, -depth.map_or(0.0, |d| d.at(x, y)));
            let normal = normals.map(|n| n.at(x, y));
            let src = *px;
            for (l, strength, light, material) in &prepared {
                let k = l.reach(cx, cy);
                // (Multiplying by no light darkens.)
                if k <= 0.0 && l.blend != Blend::Multiply {
                    continue;
                }
                let lit = match normal {
                    Some(n) if k > 0.0 => {
                        let s = shade::light(&Hit { position: at, normal: n, view: Vec3::z() }, material, [1.0; 3], light);
                        std::array::from_fn(|c| s.diffuse[c] + s.specular[c])
                    }
                    _ => [1.0; 3],
                };
                let amount: [f32; 3] = std::array::from_fn(|c| strength[c] * lit[c] * k as f32);
                let a = px[3];
                let blended: [f32; 3] = std::array::from_fn(|c| match l.blend {
                    Blend::Add => px[c] + amount[c] * a,
                    Blend::Screen => px[c] + amount[c] * a - px[c] * amount[c],
                    Blend::Multiply => px[c] * amount[c],
                });
                px[..3].copy_from_slice(&blended);
            }
            if m < 1.0 {
                for c in 0..3 {
                    px[c] = src[c] + (px[c] - src[c]) * m;
                }
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize) -> Image {
        Image::filled(w, h, [0.25, 0.25, 0.25, 1.0])
    }

    /// The red at frame pixel (x, y).
    fn red(img: &Image, x: i32, y: i32) -> f32 {
        pixel(img, x, y)[0]
    }

    #[test]
    fn a_spot_is_brightest_at_its_pixel_and_half_at_its_edge() {
        let spot = |reach: f64| Light2d::new(Shape::Spot { position: [50.5, 40.5], direction: 30.0, cone: 60.0, softness: 10.0, reach });
        let out = light2d(&gray(100, 80), &[spot(30.0)], None, None, None);
        let (mut best, mut at) = (0.0, (0, 0));
        for y in 0..80 {
            for x in 0..100 {
                if red(&out, x, y) > best {
                    (best, at) = (red(&out, x, y), (x, y));
                }
            }
        }
        assert_eq!(at, (50, 40));
        assert!((best - 1.25).abs() < 1e-6);
        assert_eq!(red(&out, 30, 40), 0.25, "behind it: dark");
        // Along its aim it fades to half at `reach`.
        let aim = 30f64.to_radians();
        let along = |d: f64| [50.5 + d * aim.cos(), 40.5 + d * aim.sin()];
        let l = spot(30.0);
        assert!((l.reach(along(30.0)[0], along(30.0)[1]) - 0.5).abs() < 1e-9);
        // Across: full inside the softness, half on the cone's edge, none past it.
        let l = spot(0.0);
        let at_angle = |deg: f64| {
            let a = (30.0 + deg).to_radians();
            l.reach(50.5 + 20.0 * a.cos(), 40.5 + 20.0 * a.sin())
        };
        assert!((at_angle(30.0) - 0.5).abs() < 1e-9 && (at_angle(-30.0) - 0.5).abs() < 1e-9);
        assert_eq!((at_angle(24.0), at_angle(36.0), at_angle(0.0)), (1.0, 0.0, 1.0));
        assert!(at_angle(28.0) > 0.5 && at_angle(32.0) < 0.5);
    }

    #[test]
    fn shapes_reach_where_they_say() {
        let area = |ellipse: bool| Light2d::new(Shape::Area { center: [50.0, 50.0], size: [40.0, 20.0], rotation: 0.0, ellipse, softness: 4.0 });
        for ellipse in [false, true] {
            let l = area(ellipse);
            assert_eq!((l.reach(50.0, 50.0), l.reach(50.0, 70.0)), (1.0, 0.0));
            assert!((l.reach(70.0, 50.0) - 0.5).abs() < 1e-9 && (l.reach(50.0, 40.0) - 0.5).abs() < 1e-9, "half on the edge");
            assert_eq!((l.reach(67.9, 50.0), l.reach(72.1, 50.0)), (1.0, 0.0));
        }
        // The rectangle's corner is in; the ellipse's isn't.
        assert_eq!((area(false).reach(68.0, 58.0), area(true).reach(68.0, 58.0)), (1.0, 0.0));
        // Turned 90°, it's taller than wide.
        let turned = Light2d::new(Shape::Area { center: [50.0, 50.0], size: [40.0, 20.0], rotation: 90.0, ellipse: false, softness: 0.0 });
        assert_eq!((turned.reach(50.0, 68.0), turned.reach(68.0, 50.0)), (1.0, 0.0));

        let point = Light2d::new(Shape::Point { position: [10.0, 10.0], radius: 20.0, falloff: 2.0 });
        assert_eq!((point.reach(10.0, 10.0), point.reach(10.0, 30.0)), (1.0, 0.0));
        assert!((point.reach(20.0, 10.0) - 0.25).abs() < 1e-12);

        // Light from the left, aimed right: full before x = 30, gone by 70.
        let sun = Light2d::new(Shape::Directional { angle: 0.0, position: [30.0, 0.0], falloff: 40.0 });
        assert_eq!((sun.reach(10.0, 99.0), sun.reach(30.0, 5.0), sun.reach(70.0, 0.0)), (1.0, 1.0, 0.0));
        assert!((sun.reach(50.0, 33.0) - 0.5).abs() < 1e-12);
        let down = Light2d::new(Shape::Directional { angle: 90.0, position: [0.0, 30.0], falloff: 40.0 });
        assert!((down.reach(5.0, 50.0) - 0.5).abs() < 1e-12 && down.reach(50.0, 10.0) == 1.0);
    }

    #[test]
    fn blends_color_exposure_and_the_mask() {
        let img = gray(4, 1);
        let mut l = Light2d::new(Shape::Directional { angle: 0.0, position: [0.0; 2], falloff: 0.0 });
        l.color = [1.0, 0.5, 0.0];
        l.exposure = 1.0;
        let px = |l: &Light2d, mask: Option<&Image>| pixel(&light2d(&img, std::slice::from_ref(l), None, None, mask), 1, 0);
        assert_eq!(px(&l, None), [2.25, 1.25, 0.25, 1.0], "added, twice as bright");
        l.blend = Blend::Screen;
        assert_eq!(px(&l, None), [1.75, 1.0, 0.25, 1.0]);
        l.blend = Blend::Multiply;
        assert_eq!(px(&l, None), [0.5, 0.25, 0.0, 1.0]);
        // Multiplied by a light that doesn't reach: dark.
        let away = Light2d { blend: Blend::Multiply, ..Light2d::new(Shape::Point { position: [100.0, 0.0], radius: 5.0, falloff: 1.0 }) };
        assert_eq!(px(&away, None), [0.0, 0.0, 0.0, 1.0]);
        // Half masked: halfway.
        let mut mask = Image::filled(4, 1, [0.0, 0.0, 0.0, 0.5]);
        mask.pixels[2] = [0.0; 4];
        assert_eq!(px(&l, Some(&mask)), [0.375, 0.25, 0.125, 1.0]);
        assert_eq!(pixel(&light2d(&img, &[l], None, None, Some(&mask)), 2, 0), img.pixels[2], "unmasked: untouched");
        // Only where the image is: nothing added on transparent pixels.
        let clear = Image::new(2, 1);
        let add = Light2d::new(Shape::Directional { angle: 0.0, position: [0.0; 2], falloff: 0.0 });
        assert_eq!(light2d(&clear, &[add], None, None, None).pixels[0], [0.0; 4]);
    }

    /// An image with a normal pass of `n` everywhere.
    fn with_normals(w: usize, h: usize, n: Vec3) -> Image {
        let mut img = Image::filled(w, h, [1.0; 4]);
        for (c, v) in ["N.x", "N.y", "N.z"].into_iter().zip([n.x, n.y, n.z]) {
            img.plane_mut(c).iter_mut().for_each(|p| *p = v as f32);
        }
        img
    }

    #[test]
    fn flat_normals_light_as_3d_lambert_does() {
        // A white card facing the camera, a point light 40 px over (30, 20).
        let img = with_normals(64, 48, Vec3::z());
        let light = Light2d { height: 40.0, blend: Blend::Multiply, ..Light2d::new(Shape::Point { position: [30.0, 20.0], radius: 1e9, falloff: 1.0 }) };
        let out = light2d(&img, std::slice::from_ref(&light), Some(Normals::Pass(&img)), None, None);
        // The same in 3D: shade's Lambert for a point light (no decay) at
        // that height over the plane, here x right, y up.
        let lamp = shade::Light { decay: 0.0, ..shade::Light::new(LightKind::Point, geom::trs([30.0, -20.0, 40.0], [0.0; 3], [1.0; 3])) };
        let lambert = Material { shading: Shading::Lambert, ..Material::default() };
        for (x, y) in [(29, 19), (0, 0), (63, 47), (40, 10)] {
            let p = Vec3::new(x as f64 + 0.5, -(y as f64 + 0.5), 0.0);
            let want = shade::light(&Hit { position: p, normal: Vec3::z(), view: Vec3::z() }, &lambert, [1.0; 3], &lamp).diffuse[0];
            let k = light.reach(x as f64 + 0.5, y as f64 + 0.5) as f32;
            assert!((red(&out, x, y) - want * k).abs() < 1e-5, "({x}, {y}): {} vs {}", red(&out, x, y), want * k);
            let cos = 40.0 / ((p.x - 30.0).powi(2) + (p.y + 20.0).powi(2) + 1600.0).sqrt();
            assert!((want as f64 - cos).abs() < 1e-6, "cosine law");
        }
        // Tilted toward the light it's brighter than tilted away.
        let toward = with_normals(64, 48, Vec3::new(-1.0, 0.0, 1.0));
        let away = with_normals(64, 48, Vec3::new(1.0, 0.0, 1.0));
        let at = |img: &Image| red(&light2d(img, std::slice::from_ref(&light), Some(Normals::Pass(img)), None, None), 50, 20);
        assert!(at(&toward) > at(&img) && at(&img) > at(&away));
        // A normal map says the same: (0.5, 0.5, 1) is flat.
        let map = Image::filled(64, 48, [0.5, 0.5, 1.0, 1.0]);
        let mapped = light2d(&img, std::slice::from_ref(&light), Some(Normals::Map(&map)), None, None);
        assert!((red(&mapped, 40, 10) - red(&out, 40, 10)).abs() < 1e-6);
        // A highlight adds; depth pushes the surface away from the light.
        let shiny = Light2d { specular: 1.0, roughness: 0.2, ..light.clone() };
        assert!(red(&light2d(&img, &[shiny], Some(Normals::Pass(&img)), None, None), 30, 20) > red(&out, 30, 20));
        let deep = Image::filled(64, 48, [10.0, 0.0, 0.0, 1.0]);
        let pushed = light2d(&img, std::slice::from_ref(&light), Some(Normals::Pass(&img)), Some(Depth { image: &deep, scale: 4.0 }), None);
        let p = Vec3::new(50.5, -40.5, -40.0);
        let cos = 80.0 / ((p.x - 30.0).powi(2) + (p.y + 20.0).powi(2) + 6400.0).sqrt();
        assert!((red(&pushed, 50, 40) as f64 - cos * light.reach(50.5, 40.5)).abs() < 1e-5);
    }

    #[test]
    fn a_sun_and_an_area_light_shade_flat_normals_evenly() {
        let img = with_normals(16, 16, Vec3::z());
        // A sun 30° above the image: cos 60° everywhere on a flat surface.
        let sun = Light2d { elevation: 30.0, blend: Blend::Multiply, ..Light2d::new(Shape::Directional { angle: 20.0, position: [0.0; 2], falloff: 0.0 }) };
        let out = light2d(&img, &[sun], Some(Normals::Pass(&img)), None, None);
        assert!(out.pixels.iter().all(|p| (p[0] - 0.5).abs() < 1e-6));
        // An area light right over a pixel lights it, and the light's own
        // shape still limits it.
        let area = Light2d { height: 5.0, blend: Blend::Multiply, ..Light2d::new(Shape::Area { center: [8.0, 8.0], size: [6.0, 6.0], rotation: 0.0, ellipse: false, softness: 0.0 }) };
        let out = light2d(&img, &[area], Some(Normals::Pass(&img)), None, None);
        assert!(red(&out, 7, 7) > 0.5 && red(&out, 0, 0) == 0.0);
    }

    #[test]
    fn data_windows_off_the_origin_use_frame_pixels() {
        let mut img = gray(10, 10);
        (img.x0, img.y0) = (100, 200);
        let spot = Light2d::new(Shape::Point { position: [105.5, 205.5], radius: 3.0, falloff: 1.0 });
        let out = light2d(&img, &[spot], None, None, None);
        assert_eq!(red(&out, 105, 205), 1.25);
        assert_eq!(red(&out, 100, 200), 0.25);
    }
}
