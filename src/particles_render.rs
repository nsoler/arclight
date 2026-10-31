//! Drawing particles: sprites and object instances.
//!
//! Sprites are camera-facing squares or round points, one per
//! [`Particle`], its size its diameter (scene units through a camera,
//! pixels in 2D), tinted by its color, optionally textured by an image or
//! a sequence picked by age, faded by an opacity over life, and laid
//! over each other (nearest on top) or added. [`render_sprites`] draws
//! them through a [`Camera`] into a frame with a `depth` plane, hidden
//! behind whatever a background's `depth` plane puts nearer;
//! [`render_sprites_2d`] draws them straight onto frame pixels (x right,
//! y down, z ignored) with no camera.
//!
//! For 3D objects, [`instances`] gives a world matrix per particle (and
//! which of several meshes it shows), for the 3D renderer to draw.

use crate::geom::{Camera, Mat4, Vec3};
use crate::image::{Image, Size};
use crate::mesh::Mesh;
use crate::particles::Particle;
use nalgebra::{Rotation3, Unit};
use rayon::prelude::*;
use std::sync::Arc;

/// A sprite's outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outline {
    /// A disk.
    #[default]
    Round,
    /// A square, turned by the particle's rotation.
    Square,
}

/// How sprites combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blend {
    /// Each over those behind it.
    #[default]
    Over,
    /// Light added up (fire, sparks, glows).
    Additive,
}

/// Which image of a sequence a particle shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    /// Through the sequence once over its life.
    ByAge,
    /// Round and round, `rate` images a frame, each particle starting on
    /// its own one.
    Cycle { rate: f64 },
    /// Played from the first image as the particle ages, `rate` images a
    /// frame, round and round (`looped`) or holding the last; from an
    /// image of its own at random (`random`, the same every time).
    Play { rate: f64, looped: bool, random: bool },
}

/// The image (or sequence) on each sprite: its whole frame spans the
/// sprite; premultiplied, it multiplies the particle's color.
#[derive(Clone)]
pub struct Texture {
    pub images: Vec<Arc<Image>>,
    pub pick: Pick,
}

/// How sprites look.
#[derive(Clone)]
pub struct Sprites {
    pub outline: Outline,
    pub texture: Option<Texture>,
    /// Scales every particle's size.
    pub scale: f64,
    /// Opacity at birth and at the end of life (multiplying the color).
    pub opacity: f32,
    pub opacity_end: f32,
    pub blend: Blend,
}

impl Default for Sprites {
    fn default() -> Self {
        Sprites { outline: Outline::Round, texture: None, scale: 1.0, opacity: 1.0, opacity_end: 1.0, blend: Blend::Over }
    }
}

/// A sprite on the frame.
pub(crate) struct Splat {
    /// Middle (frame pixels) and half its width (pixels).
    pub(crate) at: [f64; 2],
    pub(crate) radius: f64,
    /// Distance in front of the camera (0 in 2D).
    pub(crate) depth: f64,
    /// Turn (radians, clockwise on screen).
    pub(crate) turn: f64,
    pub(crate) color: [f32; 4],
    pub(crate) image: Option<usize>,
}

impl Sprites {
    /// `p`'s color with its opacity over life.
    fn color(&self, p: &Particle) -> [f32; 4] {
        let t = if p.life.is_finite() && p.life > 0.0 { (p.age / p.life).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let k = self.opacity + (self.opacity_end - self.opacity) * t;
        p.color.map(|c| c * k)
    }

    /// The image of the texture `p` shows.
    fn image(&self, p: &Particle) -> Option<usize> {
        let t = self.texture.as_ref()?;
        let n = t.images.len();
        if n == 0 {
            return None;
        }
        let i = match t.pick {
            Pick::ByAge => {
                let f = if p.life.is_finite() && p.life > 0.0 { p.age / p.life } else { 0.0 };
                (f * n as f64) as usize
            }
            Pick::Cycle { rate } => (p.age * rate).max(0.0) as usize + (p.id as usize),
            Pick::Play { rate, looped, random } => {
                let start = if random { (crate::particles::random(0, p.id, 0) * n as f64) as usize } else { 0 };
                let i = (p.age * rate).max(0.0) as usize + start;
                return Some(if looped { i % n } else { i.min(n - 1) });
            }
        };
        Some(if matches!(t.pick, Pick::ByAge) { i.min(n - 1) } else { i % n })
    }

    fn splat(&self, p: &Particle, at: [f64; 2], radius: f64, depth: f64) -> Splat {
        Splat { at, radius, depth, turn: p.rotation.to_radians(), color: self.color(p), image: self.image(p) }
    }
}

/// `particles` as `sprites` seen through `cam`, in a frame of `format`,
/// over `background` (its `depth` plane, if any, hides sprites behind
/// it). The result covers the frame and has a `depth` plane: the nearest
/// sprite's where one shows, else the background's.
pub fn render_sprites(particles: &[Particle], sprites: &Sprites, cam: &Camera, format: Size, background: Option<&Image>) -> Image {
    draw(splats_through(particles, sprites, cam, format), sprites, format, background, true)
}

/// `particles` as `sprites` seen through `cam` in a frame of `format`:
/// the sprites on the frame, in the particles' order (those behind the
/// camera or past its far plane left out).
pub(crate) fn splats_through(particles: &[Particle], sprites: &Sprites, cam: &Camera, format: Size) -> Vec<Splat> {
    let f = cam.focal_pixels(format);
    let view = cam.view();
    particles
        .iter()
        .filter_map(|p| {
            let c = crate::geom::transform_point(&view, &p.position);
            let (at, depth) = cam.project_camera(&c, format)?;
            let half = p.size * sprites.scale / 2.0;
            let radius = if cam.is_ortho() { half * cam.pixels_per_unit(depth, format) } else { half * f / depth };
            (depth <= cam.far).then(|| sprites.splat(p, at, radius, depth))
        })
        .collect()
}

/// `splats` in the order `sprites` lays them: farthest first when laid
/// over each other (so nearer ones go over them), else as they are.
pub(crate) fn sort_for(splats: &mut [Splat], sprites: &Sprites) {
    if sprites.blend == Blend::Over {
        // (Stable: equally far ones keep the particles' order.)
        splats.par_sort_by(|a, b| b.depth.total_cmp(&a.depth));
    }
}

/// `particles` as `sprites` straight onto frame pixels of `format` (their
/// x and y; size in pixels), over `background`.
pub fn render_sprites_2d(particles: &[Particle], sprites: &Sprites, format: Size, background: Option<&Image>) -> Image {
    let splats = particles.iter().map(|p| sprites.splat(p, [p.position.x, p.position.y], p.size * sprites.scale / 2.0, 0.0)).collect();
    draw(splats, sprites, format, background, false)
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

/// `img` at (`u`, `v`), 0 to 1 across its frame (v down), bilinear.
fn sample(img: &Image, u: f64, v: f64) -> [f32; 4] {
    let (x, y) = (u * img.format.width as f64 - 0.5, v * img.format.height as f64 - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let at = |dx, dy| pixel(img, x0 + dx, y0 + dy);
    let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
    std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
}

/// Rasterize `splats` into a frame of `format` over `background`.
fn draw(mut splats: Vec<Splat>, sprites: &Sprites, format: Size, background: Option<&Image>, with_depth: bool) -> Image {
    let (w, h) = (format.width, format.height);
    let mut out = Image::new(w, h);
    let mut depth = vec![0.0f32; w * h];
    if let Some(bg) = background {
        let bg_depth = bg.plane("depth").filter(|_| with_depth);
        out.pixels.par_chunks_mut(w.max(1)).zip(depth.par_chunks_mut(w.max(1))).enumerate().for_each(|(y, (row, drow))| {
            for (x, (px, d)) in row.iter_mut().zip(drow.iter_mut()).enumerate() {
                *px = pixel(bg, x as i32, y as i32);
                if let Some(p) = bg_depth {
                    *d = bg.plane_value(p, x as i32, y as i32);
                }
            }
        });
    }
    // What the background puts in front, before sprites change `depth`.
    let wall = with_depth.then(|| depth.clone());
    sort_for(&mut splats, sprites);
    // On the GPU where it can (see render3d/gpu/sprites.rs).
    #[cfg(feature = "gpu")]
    if let Some((pixels, d)) = crate::render3d::sprites_on_gpu(&splats, sprites, format, &out.pixels, &depth, with_depth) {
        out.pixels = pixels;
        if with_depth {
            *out.plane_mut("depth") = d;
        }
        return out;
    }
    let images: &[Arc<Image>] = sprites.texture.as_ref().map_or(&[], |t| &t.images);
    for s in &splats {
        // Smaller than a pixel: drawn a pixel wide, fainter to keep its
        // total (a disk r wide with a pixel-wide soft edge covers π(r² + 1/12)).
        let r = s.radius.max(0.5);
        let faint = if s.radius < 0.5 { (s.radius * s.radius / (0.25 + 1.0 / 12.0)) as f32 } else { 1.0 };
        // Squares reach their corners.
        let reach = if sprites.outline == Outline::Square { r * std::f64::consts::SQRT_2 } else { r };
        let (cos, sin) = (s.turn.cos(), s.turn.sin());
        let x0 = ((s.at[0] - reach).floor() as i64).max(0);
        let y0 = ((s.at[1] - reach).floor() as i64).max(0);
        let x1 = ((s.at[0] + reach).ceil() as i64).min(w as i64 - 1);
        let y1 = ((s.at[1] + reach).ceil() as i64).min(h as i64 - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let i = y as usize * w + x as usize;
                if let Some(wall) = &wall {
                    if wall[i] > 0.0 && (wall[i] as f64) < s.depth {
                        continue;
                    }
                }
                let (dx, dy) = (x as f64 + 0.5 - s.at[0], y as f64 + 0.5 - s.at[1]);
                // In the sprite's own axes (turned back), −1 to 1 across.
                let (u, v) = ((dx * cos + dy * sin) / r, (-dx * sin + dy * cos) / r);
                // Coverage with a pixel-wide soft edge.
                let cover = match sprites.outline {
                    Outline::Round => (r - dx.hypot(dy) + 0.5).clamp(0.0, 1.0),
                    Outline::Square => ((1.0 - u.abs()) * r + 0.5).clamp(0.0, 1.0) * ((1.0 - v.abs()) * r + 0.5).clamp(0.0, 1.0),
                } as f32
                    * faint;
                if cover <= 0.0 {
                    continue;
                }
                let tex = s.image.and_then(|k| images.get(k)).map_or([1.0; 4], |img| sample(img, (u + 1.0) / 2.0, (v + 1.0) / 2.0));
                let src: [f32; 4] = std::array::from_fn(|c| s.color[c] * tex[c] * cover);
                if src[3] <= 0.0 && src[..3].iter().all(|&c| c <= 0.0) {
                    continue;
                }
                let dst = &mut out.pixels[i];
                match sprites.blend {
                    Blend::Over => *dst = std::array::from_fn(|c| src[c] + dst[c] * (1.0 - src[3])),
                    Blend::Additive => {
                        *dst = [dst[0] + src[0], dst[1] + src[1], dst[2] + src[2], dst[3] + src[3] * (1.0 - dst[3])];
                    }
                }
                if with_depth && (depth[i] == 0.0 || (s.depth as f32) < depth[i]) {
                    depth[i] = s.depth as f32;
                }
            }
        }
    }
    if with_depth {
        *out.plane_mut("depth") = depth;
    }
    out
}

/// Which way instanced objects face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orient {
    /// As modelled.
    #[default]
    None,
    /// Their +Z along the particle's velocity, rolled by its rotation.
    Velocity,
    /// Turned by the particle's rotation about its own random axis.
    Spin,
}

/// One particle's object: which mesh, placed how.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    /// Index into the meshes given.
    pub mesh: usize,
    pub world: Mat4,
    /// The particle's color (to tint the object), and its id.
    pub color: [f32; 4],
    pub id: u64,
}

/// An object per particle: one of `meshes` (picked at random per particle
/// when there are several), at the particle, turned as `orient` says,
/// scaled by its size × `scale`.
pub fn instances(particles: &[Particle], meshes: &[Arc<Mesh>], orient: Orient, scale: f64) -> Vec<Instance> {
    if meshes.is_empty() {
        return Vec::new();
    }
    particles
        .iter()
        .map(|p| {
            let turn = match orient {
                Orient::None => Rotation3::identity(),
                Orient::Velocity => {
                    let face = match p.velocity.try_normalize(1e-12) {
                        Some(v) => Rotation3::rotation_between(&Vec3::z(), &v).unwrap_or_else(|| Rotation3::from_axis_angle(&Vec3::x_axis(), std::f64::consts::PI)),
                        None => Rotation3::identity(),
                    };
                    face * Rotation3::from_axis_angle(&Vec3::z_axis(), p.rotation.to_radians())
                }
                Orient::Spin => Rotation3::from_axis_angle(&Unit::try_new(p.axis, 1e-12).unwrap_or(Vec3::z_axis()), p.rotation.to_radians()),
            };
            let s = p.size * scale;
            let mut world = turn.to_homogeneous() * Mat4::new_scaling(s);
            world.fixed_view_mut::<3, 1>(0, 3).copy_from(&p.position);
            // Several meshes: a repeatable pick by id.
            let mesh = (p.id.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 33) as usize % meshes.len();
            Instance { mesh, world, color: p.color, id: p.id }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn particle(position: Vec3, size: f64, color: [f32; 4]) -> Particle {
        Particle {
            id: 0,
            position,
            velocity: Vec3::zeros(),
            accel: Vec3::zeros(),
            age: 0.0,
            life: f64::INFINITY,
            size,
            color,
            rotation: 0.0,
            spin: 0.0,
            axis: Vec3::z(),
            start_size: size,
        }
    }

    const FORMAT: Size = Size { width: 160, height: 120 };

    /// Where the alpha's weight sits, and how much there is.
    fn centroid(img: &Image) -> ([f64; 2], f64) {
        let (mut sx, mut sy, mut sum) = (0.0, 0.0, 0.0);
        for y in 0..img.height {
            for x in 0..img.width {
                let a = img.pixels[y * img.width + x][3] as f64;
                (sx, sy, sum) = (sx + a * (x as f64 + 0.5), sy + a * (y as f64 + 0.5), sum + a);
            }
        }
        ([sx / sum, sy / sum], sum)
    }

    #[test]
    fn a_sprite_lands_where_the_camera_sees_its_particle() {
        let cam = Camera { world: crate::geom::trs([0.5, 0.2, 3.0], [5.0, -10.0, 0.0], [1.0; 3]), ..Camera::default() };
        let p = particle(Vec3::new(1.0, 0.5, -6.0), 0.4, [1.0, 0.5, 0.25, 1.0]);
        let (at, depth) = cam.project(&p.position, FORMAT).unwrap();
        let out = render_sprites(std::slice::from_ref(&p), &Sprites::default(), &cam, FORMAT, None);
        let (c, area) = centroid(&out);
        assert!((c[0] - at[0]).abs() < 0.05 && (c[1] - at[1]).abs() < 0.05, "{c:?} vs {at:?}");
        // Its size through the lens: a disk 0.4 across at that depth.
        let r = 0.2 * cam.focal_pixels(FORMAT) / depth;
        assert!((area - std::f64::consts::PI * r * r).abs() < 0.03 * area, "{area} vs {}", std::f64::consts::PI * r * r);
        let (x, y) = (at[0] as usize, at[1] as usize);
        assert_eq!(out.pixels[y * 160 + x], [1.0, 0.5, 0.25, 1.0]);
        assert!((out.plane_at("depth", x as i32, y as i32) as f64 - depth).abs() < 1e-4);
        assert_eq!(out.plane_at("depth", 0, 0), 0.0);
        // Behind the camera: nothing.
        let behind = particle(Vec3::new(0.0, 0.0, 10.0), 1.0, [1.0; 4]);
        assert!(render_sprites(&[behind], &Sprites::default(), &cam, FORMAT, None).pixels.iter().all(|p| p[3] == 0.0));
    }

    #[test]
    fn over_puts_the_nearest_on_top_and_additive_sums() {
        let cam = Camera::default();
        let near = particle(Vec3::new(0.0, 0.0, -5.0), 1.0, [0.5, 0.0, 0.0, 0.5]);
        let far = particle(Vec3::new(0.0, 0.0, -6.0), 1.0, [0.0, 1.0, 0.0, 1.0]);
        let mid = |s: &Sprites, ps: &[Particle]| render_sprites(ps, s, &cam, FORMAT, None).pixels[60 * 160 + 80];
        let over = Sprites::default();
        // Listed either way round, the near half-red goes over the green.
        for ps in [[near.clone(), far.clone()], [far.clone(), near.clone()]] {
            assert_eq!(mid(&over, &ps), [0.5, 0.5, 0.0, 1.0]);
        }
        let add = Sprites { blend: Blend::Additive, ..Sprites::default() };
        assert_eq!(mid(&add, &[near.clone(), far.clone()]), [0.5, 1.0, 0.0, 1.0]);
        let twice = mid(&add, &[far.clone(), far.clone()]);
        assert_eq!(twice[1], 2.0, "light adds up");
        // A background nearer than a sprite hides it; farther, it doesn't.
        let mut wall = Image::filled(160, 120, [0.0, 0.0, 1.0, 1.0]);
        wall.plane_mut("depth").iter_mut().for_each(|d| *d = 5.5);
        let px = |p: &Particle| render_sprites(std::slice::from_ref(p), &over, &cam, FORMAT, Some(&wall)).pixels[60 * 160 + 80];
        assert_eq!(px(&far), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(px(&near), [0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn the_2d_path_draws_on_frame_pixels() {
        let p = particle(Vec3::new(20.0, 30.0, 99.0), 10.0, [1.0; 4]);
        let out = render_sprites_2d(std::slice::from_ref(&p), &Sprites::default(), FORMAT, None);
        let (c, area) = centroid(&out);
        assert!((c[0] - 20.0).abs() < 1e-6 && (c[1] - 30.0).abs() < 1e-6);
        assert!((area - std::f64::consts::PI * 25.0).abs() < 0.5);
        assert!(out.plane("depth").is_none());
        // A square 10 wide: corners in; turned 45°, its corner reaches 7 out.
        let square = Sprites { outline: Outline::Square, ..Sprites::default() };
        let px = |p: &Particle, x: usize, y: usize| render_sprites_2d(std::slice::from_ref(p), &square, FORMAT, None).pixels[y * 160 + x][3];
        assert_eq!((px(&p, 15, 25), px(&p, 25, 30)), (1.0, 0.0));
        let turned = Particle { rotation: 45.0, ..p.clone() };
        assert_eq!((px(&turned, 15, 25), px(&turned, 25, 30)), (0.0, 1.0));
        // Under a pixel wide it still shows, fainter.
        let dot = particle(Vec3::new(50.5, 50.5, 0.0), 0.5, [1.0; 4]);
        let (_, area) = centroid(&render_sprites_2d(&[dot], &Sprites::default(), FORMAT, None));
        assert!((area - std::f64::consts::PI * 0.0625).abs() < 0.05);
    }

    #[test]
    fn textures_and_opacity_follow_age() {
        let red = Arc::new(Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]));
        let blue = Arc::new(Image::filled(4, 4, [0.0, 0.0, 0.5, 0.5]));
        let s = Sprites {
            texture: Some(Texture { images: vec![red, blue], pick: Pick::ByAge }),
            opacity: 1.0,
            opacity_end: 0.0,
            ..Sprites::default()
        };
        let at = |age: f64| {
            let p = Particle { age, life: 10.0, ..particle(Vec3::new(20.0, 20.0, 0.0), 8.0, [1.0; 4]) };
            render_sprites_2d(&[p], &s, FORMAT, None).pixels[20 * 160 + 20]
        };
        assert_eq!(at(0.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(at(2.5), [0.75, 0.0, 0.0, 0.75], "a quarter through: three quarters opaque");
        assert_eq!(at(5.0), [0.0, 0.0, 0.25, 0.25], "the second image for the second half");
        let cycle = Sprites { texture: Some(Texture { pick: Pick::Cycle { rate: 0.5 }, ..s.texture.clone().unwrap() }), ..Sprites::default() };
        let p = |age: f64| Particle { age, ..particle(Vec3::zeros(), 1.0, [1.0; 4]) };
        assert_eq!([0.0, 1.0, 2.0, 4.0].map(|a| cycle.image(&p(a))), [Some(0), Some(0), Some(1), Some(0)]);
        // Played by age: a frame of the sequence a frame of life, from the
        // first, looping or holding the last; from a random one each.
        let seq: Vec<Arc<Image>> = (0..4).map(|k| Arc::new(Image::filled(1, 1, [k as f32, 0.0, 0.0, 1.0]))).collect();
        let play = |looped: bool, random: bool| Sprites { texture: Some(Texture { images: seq.clone(), pick: Pick::Play { rate: 1.0, looped, random } }), ..Sprites::default() };
        let ages = [0.0, 1.0, 2.5, 3.0, 4.0, 9.0];
        assert_eq!(ages.map(|a| play(false, false).image(&p(a))), [0, 1, 2, 3, 3, 3].map(Some));
        assert_eq!(ages.map(|a| play(true, false).image(&p(a))), [0, 1, 2, 3, 0, 1].map(Some));
        let starts: Vec<usize> = (0..40).map(|id| play(true, true).image(&Particle { id, ..p(0.0) }).unwrap()).collect();
        assert!((0..4).all(|k| starts.contains(&k)), "{starts:?}");
        for id in 0..40 {
            let q = |age: f64| Particle { id, ..p(age) };
            assert_eq!(play(true, true).image(&q(1.0)), Some((starts[id as usize] + 1) % 4), "each steps on from its own start");
            assert_eq!(play(false, true).image(&q(9.0)), Some(3), "and holds the last");
        }
    }

    #[test]
    fn instances_sit_at_particles_scaled_and_turned() {
        let meshes = [Arc::new(Mesh::default()), Arc::new(Mesh::default())];
        let mut p = particle(Vec3::new(1.0, 2.0, 3.0), 2.0, [1.0; 4]);
        p.velocity = Vec3::new(0.0, 5.0, 0.0);
        let one = |p: &Particle, o: Orient| instances(std::slice::from_ref(p), &meshes, o, 0.5)[0].clone();
        let i = one(&p, Orient::Velocity);
        let apply = |m: &Mat4, v: Vec3| crate::geom::transform_point(m, &v);
        assert!((apply(&i.world, Vec3::zeros()) - p.position).norm() < 1e-12);
        assert!((apply(&i.world, Vec3::z()) - (p.position + Vec3::y())).norm() < 1e-12, "+Z along the velocity, size × scale = 1");
        // Spin: about the particle's axis by its rotation.
        p.axis = Vec3::z();
        p.rotation = 90.0;
        let i = one(&p, Orient::Spin);
        assert!((apply(&i.world, Vec3::x()) - (p.position + Vec3::y())).norm() < 1e-12);
        let i = one(&p, Orient::None);
        assert!((apply(&i.world, Vec3::x()) - (p.position + Vec3::x())).norm() < 1e-12);
        // Several meshes: each particle keeps its pick; both get used.
        let many: Vec<Particle> = (0..20).map(|id| Particle { id, ..p.clone() }).collect();
        let picks: Vec<usize> = instances(&many, &meshes, Orient::None, 1.0).iter().map(|i| i.mesh).collect();
        assert_eq!(picks, instances(&many, &meshes, Orient::None, 1.0).iter().map(|i| i.mesh).collect::<Vec<_>>());
        assert!(picks.contains(&0) && picks.contains(&1));
        assert!(instances(&many, &[], Orient::None, 1.0).is_empty());
    }
}
