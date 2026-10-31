//! Scanline Render: draws a 3D scene's cards and meshes through a camera
//! into an image, with a `depth` plane.
//!
//! Everything drawn is a triangle mesh (a [`Surface`]): a card is two
//! triangles. Each triangle is clipped against the camera's near plane in
//! camera space and projected; the projected triangles are binned by the
//! pixel rows they touch, and each row is swept left to right keeping the
//! triangles under the current pixel. Every output pixel takes
//! `samples × samples` sample points; at each, the triangles covering it
//! are found (barycentric coordinates in screen space, texture coordinates
//! interpolated perspective-correctly through 1/depth), sorted nearest
//! first and composited front to back (premultiplied), so a card's
//! transparent parts show what's behind. Where two triangles of one
//! surface meet, a sample on the shared edge counts once. The samples'
//! mean is the pixel, over the background. Depth is the nearest covered
//! sample's distance in front of the camera (of a surface at least half
//! opaque, where there is one), 0 where nothing is. Points
//! draw as one-pixel dots.

use crate::color::{self, Rgba};
use crate::geom::{self, Camera, Mat4, Vec3};
use crate::image::{IRect, Image, Size};
use crate::mesh::{Material, Shading};
use crate::scene::Scene;
use crate::shade::{self, Light, Shaded};
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

mod mip;
mod particles;
mod shadow;
mod splats;

/// The 3D renderer's rows in the GPU self-test ([`crate::gpu::check`]):
/// none until the renderer runs on the GPU.
#[cfg(feature = "gpu")]
pub fn gpu_checks(_gpu: &crate::gpu::Gpu) -> Vec<crate::gpu::check::CheckRow> {
    Vec::new()
}

/// A triangle mesh to draw, in its own space, placed by `world`, colored
/// by `color` times its texture (if any).
pub(crate) struct Surface<'a> {
    pub world: Mat4,
    pub positions: Cow<'a, [Vec3]>,
    /// Per vertex (empty: each triangle's own facing).
    pub normals: Cow<'a, [Vec3]>,
    /// Per vertex: (0, 0) the texture's top left, (1, 1) its bottom right
    /// (empty: all (0, 0)); with `v_up`, (0, 0) is its bottom left (a
    /// mesh's, as OBJ has them).
    pub uvs: Cow<'a, [[f64; 2]]>,
    pub v_up: bool,
    pub triangles: Cow<'a, [[u32; 3]]>,
    pub color: Rgba,
    pub texture: Option<&'a Arc<Image>>,
    /// Texture coordinates past 0–1 repeat the texture (else they clamp to
    /// its edge, as a card's do).
    pub wrap: bool,
    /// Shade by a light at the camera (the 3D viewport's solid view).
    pub headlight: bool,
    /// Draws nothing, but hides what is behind it (alpha 0 there).
    pub holdout: bool,
    /// The node it came from: its object id and matte.
    pub node: &'a str,
    /// How it takes light (None: unlit, as a card is by default).
    pub material: Option<&'a Material>,
    /// Whether it blocks shadow-casting lights, and takes their shadows.
    pub casts: bool,
    pub receives: bool,
    /// Lit from either side (a card): its normal turned to the viewer.
    pub two_sided: bool,
}

/// Card `card` as two triangles (top left, top right, bottom right; top
/// left, bottom right, bottom left), its image (premultiplied) times its
/// opacity.
fn card_surface(card: &crate::scene::Card) -> Surface<'_> {
    let [w, h] = card.size.map(|v| v / 2.0);
    Surface {
        world: card.world,
        positions: Cow::Owned(vec![Vec3::new(-w, h, 0.0), Vec3::new(w, h, 0.0), Vec3::new(w, -h, 0.0), Vec3::new(-w, -h, 0.0)]),
        normals: Cow::Owned(vec![Vec3::z(); 4]),
        uvs: Cow::Owned(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        triangles: Cow::Owned(vec![[0, 1, 2], [0, 2, 3]]),
        color: [card.opacity; 4],
        texture: Some(&card.image),
        wrap: false,
        v_up: false,
        headlight: false,
        holdout: false,
        node: &card.node,
        material: card.material.as_ref(),
        casts: card.casts_shadows,
        receives: card.receives_shadows,
        two_sided: true,
    }
}

/// Model `m`: its mesh, its material's color (premultiplied by its
/// opacity) times its texture, which repeats.
fn model_surface(m: &crate::scene::Model) -> Surface<'_> {
    let [r, g, b] = m.material.color;
    let a = m.material.opacity;
    Surface {
        world: m.world,
        positions: Cow::Borrowed(&m.mesh.positions),
        normals: Cow::Borrowed(&m.mesh.normals),
        uvs: Cow::Borrowed(&m.mesh.uvs),
        triangles: Cow::Borrowed(&m.mesh.triangles),
        color: [r * a, g * a, b * a, a],
        texture: m.texture.as_ref(),
        wrap: true,
        v_up: true,
        headlight: false,
        holdout: m.holdout,
        node: &m.node,
        material: Some(&m.material),
        casts: m.casts_shadows,
        receives: m.receives_shadows,
        two_sided: false,
    }
}

/// Everything in `scene` to draw, in order: cards, then meshes.
pub(crate) fn surfaces(scene: &Scene) -> Vec<Surface<'_>> {
    scene.cards.iter().map(card_surface).chain(scene.meshes.iter().map(model_surface)).collect()
}

/// A triangle corner: in camera space and in the scene, with its texture
/// coordinates and (scene) normal.
#[derive(Clone, Copy)]
struct Vert {
    p: Vec3,
    world: Vec3,
    uv: [f64; 2],
    n: Vec3,
    /// Where this point of the surface is in the scene a frame later (for
    /// motion vectors).
    next: Vec3,
}

impl Vert {
    fn lerp(&self, o: &Vert, t: f64) -> Vert {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Vert {
            p: self.p + (o.p - self.p) * t,
            world: self.world + (o.world - self.world) * t,
            uv: [l(self.uv[0], o.uv[0]), l(self.uv[1], o.uv[1])],
            n: self.n + (o.n - self.n) * t,
            next: self.next + (o.next - self.next) * t,
        }
    }
}

/// A projected triangle of surface `surf`: frame positions and 1 / depth
/// at each corner, its corners, and the pixel rows and columns it can
/// touch. `ortho`: projected orthographically, so depth and everything
/// else vary linearly across the frame.
struct Tri {
    surf: usize,
    p: [[f64; 2]; 3],
    v: [Vert; 3],
    inv_depth: [f64; 3],
    area: f64,
    bounds: IRect,
    ortho: bool,
}

/// The part of `poly` at least `near` in front of the camera
/// (Sutherland–Hodgman against the plane z = −near).
fn clip_near(poly: &[Vert], near: f64) -> Vec<Vert> {
    let inside = |v: &Vert| -v.p.z >= near;
    let mut out = Vec::with_capacity(poly.len() + 2);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if inside(&a) {
            out.push(a);
        }
        if inside(&a) != inside(&b) {
            // Where the edge crosses the plane.
            out.push(a.lerp(&b, (-near - a.p.z) / (b.p.z - a.p.z)));
        }
    }
    out
}

/// Twice the signed area of triangle (a, b, c).
fn edge(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Every surface's triangles, projected into a frame of `format`.
/// `next[i]`, when given: surface `i`'s vertices in the scene a frame later.
fn triangles(surfs: &[Surface], cam: &Camera, format: Size, next: Option<&[Vec<Vec3>]>) -> Vec<Tri> {
    let view = cam.view();
    let f = cam.focal_pixels(format);
    let (ortho, k) = (cam.is_ortho(), cam.pixels_per_unit(1.0, format));
    let (cx, cy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
    let near = cam.near.max(1e-6);
    let frame = IRect::frame(format);
    let mut tris = Vec::new();
    for (si, s) in surfs.iter().enumerate() {
        let to_cam = view * s.world;
        // Normals by the inverse transpose (scaling bends them).
        let normal_m = s.world.fixed_view::<3, 3>(0, 0).try_inverse().map(|m| m.transpose()).unwrap_or_else(nalgebra::Matrix3::identity);
        let cam_p: Vec<Vec3> = s.positions.iter().map(|p| geom::transform_point(&to_cam, p)).collect();
        let world_p: Vec<Vec3> = s.positions.iter().map(|p| geom::transform_point(&s.world, p)).collect();
        let world_n: Vec<Vec3> = s.normals.iter().map(|n| (normal_m * n).try_normalize(1e-300).unwrap_or(*n)).collect();
        let next_p = next.and_then(|n| n.get(si)).filter(|n| n.len() == world_p.len()).unwrap_or(&world_p);
        for t in s.triangles.iter() {
            let [a, b, c] = t.map(|i| i as usize);
            if a.max(b).max(c) >= cam_p.len() {
                continue;
            }
            let face = (world_p[b] - world_p[a]).cross(&(world_p[c] - world_p[a])).try_normalize(1e-300).unwrap_or_else(Vec3::z);
            let corner = |i: usize| Vert {
                p: cam_p[i],
                world: world_p[i],
                uv: s.uvs.get(i).map_or([0.0, 0.0], |uv| if s.v_up { [uv[0], 1.0 - uv[1]] } else { *uv }),
                n: world_n.get(i).copied().unwrap_or(face),
                next: next_p[i],
            };
            let tri = [corner(a), corner(b), corner(c)];
            let poly = if tri.iter().all(|v| -v.p.z >= near) { tri.to_vec() } else { clip_near(&tri, near) };
            if poly.len() < 3 {
                continue;
            }
            let project = |v: &Vert| {
                let d = -v.p.z;
                if ortho {
                    ([cx + k * v.p.x, cy - k * v.p.y], 1.0 / d)
                } else {
                    ([cx + f * v.p.x / d, cy - f * v.p.y / d], 1.0 / d)
                }
            };
            let projected: Vec<([f64; 2], f64)> = poly.iter().map(project).collect();
            for k in 1..poly.len() - 1 {
                let idx = [0, k, k + 1];
                let p = idx.map(|i| projected[i].0);
                let area = edge(p[0], p[1], p[2]);
                if area.abs() < 1e-12 || p.iter().flatten().any(|v| !v.is_finite()) {
                    continue;
                }
                let lo = |k: usize| p.iter().map(|q| q[k]).fold(f64::INFINITY, f64::min).floor().max(-1e9) as i32;
                let hi = |k: usize| p.iter().map(|q| q[k]).fold(f64::NEG_INFINITY, f64::max).ceil().min(1e9) as i32;
                let bounds = IRect::new(lo(0), lo(1), hi(0), hi(1)).intersect(&frame);
                if bounds.is_empty() {
                    continue;
                }
                tris.push(Tri { surf: si, p, v: idx.map(|i| poly[i]), inv_depth: idx.map(|i| projected[i].1), area, bounds, ortho });
            }
        }
    }
    tris
}

/// One surface seen at one sample: how far, through which triangle, where
/// on it (perspective-correct barycentric coordinates), and its color
/// there (premultiplied).
#[derive(Clone, Copy)]
struct Hit {
    depth: f64,
    tri: usize,
    bary: [f64; 3],
    color: Rgba,
}

impl Tri {
    /// Where this triangle covers frame position `q`, if it does, between
    /// `near` and `far`: the depth and the perspective-correct barycentric
    /// coordinates.
    fn cover(&self, q: [f64; 2], near: f64, far: f64) -> Option<(f64, [f64; 3])> {
        let b0 = edge(self.p[1], self.p[2], q) / self.area;
        let b1 = edge(self.p[2], self.p[0], q) / self.area;
        let b2 = 1.0 - b0 - b1;
        if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
            return None;
        }
        if self.ortho {
            let d = b0 / self.inv_depth[0] + b1 / self.inv_depth[1] + b2 / self.inv_depth[2];
            return (d >= near && d <= far).then_some((d, [b0, b1, b2]));
        }
        let inv = b0 * self.inv_depth[0] + b1 * self.inv_depth[1] + b2 * self.inv_depth[2];
        let d = 1.0 / inv;
        if !(d >= near && d <= far) {
            return None;
        }
        Some((d, [b0 * self.inv_depth[0] / inv, b1 * self.inv_depth[1] / inv, b2 * self.inv_depth[2] / inv]))
    }

    /// The (perspective-correct) barycentric coordinates of frame position
    /// `q`, inside the triangle or not.
    fn bary_at(&self, q: [f64; 2]) -> [f64; 3] {
        let b0 = edge(self.p[1], self.p[2], q) / self.area;
        let b1 = edge(self.p[2], self.p[0], q) / self.area;
        let b = [b0, b1, 1.0 - b0 - b1];
        if self.ortho {
            return b;
        }
        let w: [f64; 3] = std::array::from_fn(|k| b[k] * self.inv_depth[k]);
        let inv = w[0] + w[1] + w[2];
        w.map(|w| w / inv)
    }

    /// How many texels of a `size` texture a sample at `q` with texture
    /// coordinates `uv` covers, `step` pixels apart from the next: the
    /// longer of its steps across and down, through the texture.
    fn footprint(&self, q: [f64; 2], uv: [f64; 2], step: f64, size: [f64; 2]) -> f64 {
        let along = |d: [f64; 2]| {
            let o = self.uv(&self.bary_at([q[0] + d[0], q[1] + d[1]]));
            ((o[0] - uv[0]) * size[0]).hypot((o[1] - uv[1]) * size[1])
        };
        along([step, 0.0]).max(along([0.0, step]))
    }

    /// Texture coordinates at barycentric `b`.
    fn uv(&self, b: &[f64; 3]) -> [f64; 2] {
        std::array::from_fn(|k| b[0] * self.v[0].uv[k] + b[1] * self.v[1].uv[k] + b[2] * self.v[2].uv[k])
    }
}

/// Surface `s`'s color at texture coordinates `uv`.
fn shade(s: &Surface, uv: [f64; 2]) -> Rgba {
    let Some(img) = s.texture else { return s.color };
    tint(s, sample_level(img, s.wrap, uv))
}

/// Surface `s`'s color at texture coordinates `uv` for a sample covering
/// `texels` of its texture: where that's more than one, the mean over
/// them from its mipmaps (`mips`, made when first needed), blending the
/// two levels nearest that size.
fn shade_covering(s: &Surface, uv: [f64; 2], texels: f64, mips: &OnceLock<Arc<mip::Mips>>) -> Rgba {
    let Some(img) = s.texture else { return s.color };
    // (1:1 within rounding is 1:1.)
    if texels <= 1.0 + 1e-6 || !texels.is_finite() {
        return tint(s, sample_level(img, s.wrap, uv));
    }
    let levels = &mips.get_or_init(|| mip::of(img, s.wrap)).levels;
    let lod = texels.log2().min(levels.len() as f64);
    let (k, w) = (lod.floor() as usize, (lod - lod.floor()) as f32);
    let level = |k: usize| if k == 0 { sample_level(img, s.wrap, uv) } else { sample_level(&levels[k - 1], s.wrap, uv) };
    let a = level(k);
    if w == 0.0 || k >= levels.len() {
        return tint(s, a);
    }
    let b = level(k + 1);
    tint(s, std::array::from_fn(|c| a[c] + (b[c] - a[c]) * w))
}

/// `px` times surface `s`'s color.
fn tint(s: &Surface, px: Rgba) -> Rgba {
    if s.color == [1.0; 4] { px } else { std::array::from_fn(|c| px[c] * s.color[c]) }
}

/// `img` at texture coordinates `uv`, bilinear: repeating (`wrap`) or
/// clamped to its frame, so a card's edge is sharp (the samples soften
/// it) rather than fading over half a texel.
fn sample_level(img: &Image, wrap: bool, uv: [f64; 2]) -> Rgba {
    let (fw, fh) = (img.format.width as f64, img.format.height as f64);
    if wrap {
        return sample_wrapped(img, uv[0] * fw - 0.5, uv[1] * fh - 0.5);
    }
    let (tx, ty) = ((uv[0] * fw - 0.5).clamp(0.0, (fw - 1.0).max(0.0)), (uv[1] * fh - 0.5).clamp(0.0, (fh - 1.0).max(0.0)));
    img.sample_bilinear(tx as f32, ty as f32)
}

/// Bilinear sample of `img` at frame position (x, y) (pixel centers on
/// integers), the image repeating across and down: the taps past an edge
/// wrap round to the other.
fn sample_wrapped(img: &Image, x: f64, y: f64) -> Rgba {
    let (w, h) = (img.format.width.max(1) as i64, img.format.height.max(1) as i64);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let tap = |dx: i64, dy: i64| img.at((x0 as i64 + dx).rem_euclid(w) as i32, (y0 as i64 + dy).rem_euclid(h) as i32);
    let (p00, p10, p01, p11) = (tap(0, 0), tap(1, 0), tap(0, 1), tap(1, 1));
    std::array::from_fn(|c| (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy)
}

/// Per pixel row, the triangles touching it, left edges in order.
fn bin_rows(tris: &[Tri], height: usize) -> Vec<Vec<u32>> {
    let mut rows: Vec<Vec<u32>> = vec![Vec::new(); height];
    for (i, t) in tris.iter().enumerate() {
        for y in t.bounds.y0.max(0)..t.bounds.y1.min(height as i32) {
            rows[y as usize].push(i as u32);
        }
    }
    rows.par_iter_mut().for_each(|r| r.sort_by_key(|&i| (tris[i as usize].bounds.x0, i)));
    rows
}

/// Render `scene`'s cards and points through `cam` into a frame of
/// `format`, `samples × samples` samples a pixel, over `background` (whose
/// frame and data outside the render are kept). The result has a `depth`
/// plane over the frame.
pub fn render(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>) -> Image {
    render_passes(scene, cam, format, samples, background, Passes::DEPTH)
}

/// The extra planes a render writes besides the beauty (RGBA).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Passes {
    /// `depth`: distance in front of the camera, 0 where nothing is.
    pub depth: bool,
    /// `P.x` `P.y` `P.z`: the scene position seen.
    pub position: bool,
    /// `N.x` `N.y` `N.z`: the surface's (unit) normal in the scene.
    pub normal: bool,
    /// `uv.u` `uv.v`: texture coordinates (v up, as OBJ has them; a card's
    /// image runs from (0, 1) at its top left to (1, 0) at its bottom right).
    pub uv: bool,
    /// `id`: which object (1, 2, … by node, in the scene's order; 0 none).
    pub id: bool,
    /// `matte.<node>`: each object node's coverage.
    pub mattes: bool,
    /// The light in parts, adding up to the beauty's color: `diffuse`,
    /// `specular`, `emission` (with unlit surfaces' color) and `ambient`
    /// (each `.R` `.G` `.B`).
    pub diffuse: bool,
    pub specular: bool,
    pub emission: bool,
    pub ambient: bool,
    /// `light.<node>` (`.R` `.G` `.B`): what each light node adds.
    pub lights: bool,
    /// `shadow.R` `.G` `.B`: the light shadows took away (diffuse and
    /// specular), so the beauty plus it is the picture without shadows.
    pub shadow: bool,
    /// `motion.u` `motion.v`: how far (pixels, right and down) the surface
    /// seen moves by the next frame, with the camera's move.
    pub motion: bool,
}

impl Passes {
    /// What a plain render writes.
    pub const DEPTH: Passes = Passes { depth: true, ..Passes::NONE };
    const NONE: Passes = Passes { depth: false, position: false, normal: false, uv: false, id: false, mattes: false, diffuse: false, specular: false, emission: false, ambient: false, lights: false, shadow: false, motion: false };
    /// The pass names, as a Scanline Render's Passes list holds them.
    pub const NAMES: [&'static str; 13] = ["depth", "P", "N", "uv", "motion", "id", "mattes", "diffuse", "specular", "emission", "ambient", "lights", "shadow"];

    /// The passes named in `names` (others are ignored).
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Passes {
        let mut p = Passes::default();
        for n in names {
            match n {
                "depth" => p.depth = true,
                "P" => p.position = true,
                "N" => p.normal = true,
                "uv" => p.uv = true,
                "id" => p.id = true,
                "mattes" => p.mattes = true,
                "diffuse" => p.diffuse = true,
                "specular" => p.specular = true,
                "emission" => p.emission = true,
                "ambient" => p.ambient = true,
                "lights" => p.lights = true,
                "shadow" => p.shadow = true,
                "motion" => p.motion = true,
                _ => {}
            }
        }
        p
    }
}

/// [`render`] writing `passes`: depth, `P`, `N`, `uv` and `id` from the
/// nearest surface any sample sees in each pixel (averaging them across an
/// edge would make values no surface has) that is at least half opaque
/// there (its texture's alpha times its opacity), or where none is, the
/// nearest fainter one (so a faint card in front doesn't take them from
/// what shows through it); mattes as coverage (adding up,
/// with the other objects', to the beauty's alpha). Over a background,
/// where the render shows nothing each pass is the background's plane of
/// that name, if it has one; its other planes are kept.
pub fn render_passes(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>, passes: Passes) -> Image {
    render_motion(scene, cam, format, samples, background, passes, None)
}

/// [`render_passes`], with `next`: the scene and camera a frame later, for
/// the motion pass (without it, motion is the camera's and objects' move
/// as if nothing moved: all 0).
pub fn render_motion(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>, passes: Passes, next: Option<(&Scene, &Camera)>) -> Image {
    render_cell(scene, cam, format, samples, background, passes, next, None)
}

/// One pixel of every `side` × `side` tile, the one at (`x`, `y`) in it:
/// the pixels a render at one of [`render_blurred`]'s jittered times fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    side: usize,
    x: usize,
    y: usize,
}

impl Cell {
    fn has(&self, x: usize, y: usize) -> bool {
        x % self.side == self.x && y % self.side == self.y
    }
}

/// [`render_motion`], only `cell`'s pixels drawn (the rest left empty), or
/// all of them.
#[allow(clippy::too_many_arguments)]
fn render_cell(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>) -> Image {
    // Particles drawn as objects render as meshes; as points or sprites,
    // over the rest, hidden by its depth (made for them if not asked for).
    let objects = particles::with_objects(scene);
    let scene = objects.as_ref().unwrap_or(scene);
    let sprites = scene.particles.iter().any(|s| !matches!(s.look, crate::particle_nodes::Look::Objects { .. }));
    let depth_for_sprites = sprites && !passes.depth;
    let passes = Passes { depth: passes.depth || sprites, ..passes };
    // Particle objects a frame later: the same particles moved on along
    // their velocities (the next frame's own may be others).
    let next_objects = next.filter(|_| objects.is_some() && passes.motion).and_then(|(n, _)| particles::with_objects(&Scene { particles: scene.particles.iter().map(|s| s.shifted(1.0)).collect(), ..n.clone() }));
    let next = next.map(|(n, c)| (next_objects.as_ref().unwrap_or(n), c));
    let mut splats = splats::prepare(&scene.splats, cam, format);
    if let Some(sp) = &mut splats {
        if passes.normal {
            sp.add_normals(&scene.splats, cam);
        }
        if passes.motion {
            sp.add_motion(&scene.splats, next.map(|(n, c)| (&n.splats[..], c)), cam, format);
        }
    }
    let out = draw_passes(&surfaces(scene), &scene.lights, &scene.points, cam, format, samples, passes, next, cell, splats.as_ref());
    let mut out = match background {
        Some(bg) => over_background(out, bg),
        None => out,
    };
    if sprites {
        out = particles::draw(scene, cam, format, out, passes.motion.then(|| next.map_or(cam, |n| n.1)));
        if depth_for_sprites {
            out.planes.retain(|p| p.name != "depth");
        }
    }
    out
}

/// How the 3D viewport shades its meshes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preview {
    /// Lit from the camera, so their shape shows without lights (quick).
    #[default]
    Solid,
    /// By the scene's lights and their materials, as a render is, with
    /// the lights' shadows or none.
    Lit { shadows: bool },
}

/// Motion blur for [`render_blurred`]: the times to render at (frames
/// from the current one) and the scenes and cameras at the whole frames
/// round them (offset from the current frame; 0 is the current one).
pub struct Blur<'a> {
    pub times: Vec<f64>,
    pub frames: Vec<(i64, &'a Scene, Camera)>,
    /// Each pixel's times jittered within their shares of the shutter (see
    /// [`render_blurred`]); off, every pixel at the same times.
    pub jitter: bool,
}

impl Blur<'_> {
    /// `n` times evenly across a shutter `shutter` frames open from
    /// `start` frames (each in the middle of its share).
    pub fn times(start: f64, shutter: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| start + shutter * (i as f64 + 0.5) / n as f64).collect()
    }

    /// The camera `t` frames on: between those at the whole frames round
    /// it, else `cam`.
    fn camera(&self, cam: &Camera, t: f64) -> Camera {
        let k = t.floor() as i64;
        let w = t - t.floor();
        let find = |k: i64| self.frames.iter().find(|f| f.0 == k).map(|f| &f.2);
        match (find(k), find(k + 1)) {
            (Some(a), Some(b)) => Camera { world: crate::scene::between(&a.world, &b.world, w), focal: a.focal + (b.focal - a.focal) * w, ortho: a.ortho + (b.ortho - a.ortho) * w, ..cam.clone() },
            (Some(a), None) if w == 0.0 => a.clone(),
            _ => cam.clone(),
        }
    }
}

/// [`render_motion`] with motion blur: the picture, mattes and light passes
/// the mean of renders at each of `blur`'s times (the scene and camera
/// in between their whole frames: see [`Scene::at_time`]); depth, P, N, uv,
/// motion and id from the render at the frame itself, as without blur.
/// No blur (or one time): [`render_motion`].
///
/// Each pixel's times are jittered within their shares of the shutter, so
/// a very fast move smears into fine grain instead of a few sharp copies:
/// the pixels of each tile (see [`jitter_side`]) take turns at offsets
/// spread across a share (in ordered-dither order, so neighbours differ),
/// each turn rendering just its pixels. The same every time. Without
/// [`Blur::jitter`], every pixel renders at the times themselves.
#[allow(clippy::too_many_arguments)]
pub fn render_blurred(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>, passes: Passes, next: Option<(&Scene, &Camera)>, blur: Option<&Blur>) -> Image {
    let out = render_motion(scene, cam, format, samples, background, passes, next);
    let Some(blur) = blur.filter(|b| b.times.len() > 1) else { return out };
    let mixed = Passes { depth: false, position: false, normal: false, uv: false, motion: false, id: false, ..passes };
    let frames: Vec<(i64, &Scene)> = blur.frames.iter().map(|f| (f.0, f.1)).collect();
    match blur_sum(&blur.times, blur.jitter, |t, cell| render_cell(&scene.at_time(&frames, t), &blur.camera(cam, t), format, samples, background, mixed, None, cell)) {
        Some(sum) if sum.pixels.len() == out.pixels.len() => mean_into(out, sum, blur.times.len()),
        _ => out,
    }
}

/// The tile side [`render_blurred`] jitters `n` times over: 4 × 4 (16
/// offsets) for few times, where copies would show most, fewer as there
/// are more (each turn costs a scene setup), none from 32.
fn jitter_side(n: usize) -> usize {
    match n {
        0..=7 => 4,
        8..=31 => 2,
        _ => 1,
    }
}

/// Where (x, y) of a `side` × `side` tile comes in ordered-dither order
/// (`side` a power of two): 0 .. side².
fn dither_rank(side: usize, x: usize, y: usize) -> usize {
    // The coarsest 2 × 2 the last digit, as in Bayer's matrices.
    let (mut r, mut s, mut weight) = (0, side, 1);
    let (mut x, mut y) = (x, y);
    while s > 1 {
        s /= 2;
        r += weight * [[0, 2], [3, 1]][y / s][x / s];
        weight *= 4;
        x %= s;
        y %= s;
    }
    r
}

/// The sum over `times` (a share of the shutter apart) of renders by
/// `render` (time, cell), each tile pixel's times moved within their
/// shares by its jitter (with `jitter`; else whole renders at the times).
/// None if nothing rendered.
fn blur_sum(times: &[f64], jitter: bool, render: impl Fn(f64, Option<Cell>) -> Image) -> Option<Image> {
    let side = if jitter { jitter_side(times.len()) } else { 1 };
    let share = if times.len() > 1 { times[1] - times[0] } else { 0.0 };
    let mut sum: Option<Image> = None;
    for cy in 0..side {
        for cx in 0..side {
            let cell = (side > 1).then_some(Cell { side, x: cx, y: cy });
            let shift = share * ((dither_rank(side, cx, cy) as f64 + 0.5) / (side * side) as f64 - 0.5);
            for &t in times {
                let img = render(t + shift, cell);
                // (Starting from nothing: a turn's render has more than its
                // pixels, sprites and the background drawn all over.)
                let acc = sum.get_or_insert_with(|| {
                    let mut zero = img.clone();
                    zero.pixels.fill(color::TRANSPARENT);
                    zero.planes.iter_mut().for_each(|p| p.data.fill(0.0));
                    zero
                });
                if acc.pixels.len() != img.pixels.len() {
                    continue;
                }
                // (By frame position: a background can reach past it.)
                let (win, w) = (img.window(), img.width.max(1));
                let mine = |i: usize| cell.is_none_or(|c| c.has((win.x0 + (i % w) as i32).rem_euclid(c.side as i32) as usize, (win.y0 + (i / w) as i32).rem_euclid(c.side as i32) as usize));
                acc.pixels.iter_mut().zip(&img.pixels).enumerate().filter(|(i, _)| mine(*i)).for_each(|(_, (a, b))| (0..4).for_each(|c| a[c] += b[c]));
                for p in &mut acc.planes {
                    if let Some(q) = img.plane(&p.name) {
                        p.data.iter_mut().zip(&q.data).enumerate().filter(|(i, _)| mine(*i)).for_each(|(_, (a, b))| *a += b);
                    }
                }
            }
        }
    }
    sum
}

/// `out` with its picture and `sum`'s planes the mean of `sum` over `n`.
fn mean_into(mut out: Image, sum: Image, n: usize) -> Image {
    let k = 1.0 / n as f32;
    out.pixels = sum.pixels.iter().map(|p| p.map(|v| v * k)).collect();
    for p in sum.planes {
        if p.data.len() == out.pixels.len() {
            *out.plane_mut(&p.name) = p.data.iter().map(|v| v * k).collect();
        }
    }
    out
}

/// The highest SH degree the 3D view's splats take their color from (the
/// rest of a capture's view-dependent color is subtle, and slow).
const PREVIEW_SH: u8 = 1;

/// `scene`'s cards, meshes and splats through `cam`, one sample a pixel,
/// the meshes shaded as `preview` says (Lit without lights is Solid, so
/// nothing goes black), the splats' color from SH up to [`PREVIEW_SH`]:
/// what the 3D viewport shows under its outlines.
pub fn render_preview(scene: &Scene, cam: &Camera, format: Size, preview: Preview) -> Image {
    preview_cell(scene, cam, format, preview, None, splats::prepare_upto(&scene.splats, cam, format, PREVIEW_SH).as_ref())
}

/// [`render_preview`] with motion blur: the mean of previews at each of
/// `blur`'s times, jittered and with the camera moving between its
/// frames' as [`render_blurred`] does (the 3D view gives its own camera at
/// every frame unless it looks through a scene's). No blur (or one time):
/// [`render_preview`].
pub fn render_preview_blurred(scene: &Scene, cam: &Camera, format: Size, preview: Preview, blur: Option<&Blur>) -> Image {
    let Some(blur) = blur.filter(|b| b.times.len() > 1) else { return render_preview(scene, cam, format, preview) };
    let frames: Vec<(i64, &Scene)> = blur.frames.iter().map(|f| (f.0, f.1)).collect();
    // The splats projected once, for the times where neither they nor the
    // camera move.
    let still = splats::prepare_upto(&scene.splats, cam, format, PREVIEW_SH);
    let sum = blur_sum(&blur.times, blur.jitter, |t, cell| {
        let (now, c) = (scene.at_time(&frames, t), blur.camera(cam, t));
        let moved = c != *cam || now.splats.iter().zip(&scene.splats).any(|(a, b)| a.world != b.world);
        let own = moved.then(|| splats::prepare_upto(&now.splats, &c, format, PREVIEW_SH)).flatten();
        let mut img = preview_cell(&now, &c, format, preview, cell, if moved { own.as_ref() } else { still.as_ref() });
        img.planes.clear();
        img
    });
    let out = Image::blank(format, IRect::frame(format));
    match sum {
        Some(sum) if sum.pixels.len() == out.pixels.len() => mean_into(out, sum, blur.times.len()),
        _ => render_preview(scene, cam, format, preview),
    }
}

/// [`render_preview`], only `cell`'s pixels drawn, or all of them, with
/// `splats` (the scene's, prepared for `cam`).
fn preview_cell(scene: &Scene, cam: &Camera, format: Size, preview: Preview, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Image {
    let mut surfs = surfaces(scene);
    match preview {
        Preview::Lit { shadows } if !scene.lights.is_empty() => {
            let lights: Vec<Light> = scene.lights.iter().map(|l| Light { shadows: l.shadows && shadows, ..l.clone() }).collect();
            draw_passes(&surfs, &lights, &[], cam, format, 1, Passes::DEPTH, None, cell, splats)
        }
        _ => {
            for s in surfs.iter_mut().skip(scene.cards.len()) {
                s.headlight = true;
            }
            draw_passes(&surfs, &[], &[], cam, format, 1, Passes::DEPTH, None, cell, splats)
        }
    }
}

/// The light at a hit, in parts (premultiplied, as `color` is): `color`
/// unchanged, all in emission, for a surface without a material, or one
/// that's unlit (or no lights) and gives off no light of its own; else
/// [`shade::shade`] of its unpremultiplied color, times its alpha. With
/// `per_light`, also each light's share alone (diffuse, specular and
/// ambient added up). Shadows from `shadows` fall on surfaces that receive
/// them; also the light they took away (premultiplied).
fn light_hit(s: &Surface, lights: &[Light], shadows: &shadow::Shadows, color: Rgba, at: impl Fn() -> shade::Hit, per_light: Option<&mut Vec<[f32; 3]>>) -> (Rgba, Shaded, [f32; 3]) {
    let rgb = [color[0], color[1], color[2]];
    let unlit = Shaded { emission: rgb, ..Shaded::default() };
    let Some(m) = s.material else { return (color, unlit, [0.0; 3]) };
    let lit = !lights.is_empty() && m.shading != Shading::Unlit;
    if (!lit && m.emission == [0.0; 3]) || color[3] <= 0.0 {
        return (color, unlit, [0.0; 3]);
    }
    let a = color[3];
    let albedo = rgb.map(|v| v / a);
    let hit = at();
    let premult = |v: [f32; 3]| v.map(|x| x * a);
    let shadowed = s.receives && shadows.any();
    let visible = |li: usize, k: usize, l: &Vec3| if shadowed { shadows.visible(li, k, &hit.position, &hit.normal, l) } else { 1.0 };
    let (sh, lost) = shade::shade_shadowed(&hit, m, albedo, lights, &visible);
    let parts = Shaded { diffuse: premult(sh.diffuse), specular: premult(sh.specular), emission: premult(sh.emission), ambient: premult(sh.ambient) };
    if let Some(out) = per_light {
        out.clear();
        out.extend(lights.iter().enumerate().map(|(li, l)| premult(shade::light_shadowed(&hit, m, albedo, l, &|k, d| visible(li, k, d)).0.total())));
    }
    let t = parts.total();
    ([t[0], t[1], t[2], a], parts, premult(lost))
}

/// The light nodes of `lights` in order (each once; unnamed lights left
/// out), and for each light its index among them.
fn light_nodes(lights: &[Light]) -> (Vec<&str>, Vec<Option<usize>>) {
    let mut nodes: Vec<&str> = Vec::new();
    for l in lights {
        if !l.node.is_empty() && !nodes.contains(&l.node.as_str()) {
            nodes.push(&l.node);
        }
    }
    let which = lights.iter().map(|l| nodes.iter().position(|n| *n == l.node)).collect();
    (nodes, which)
}

/// The next `n` channels of a pixel's extra ones when `on`: where they
/// start (`k` counts those taken).
fn slot(k: &mut usize, on: bool, n: usize) -> usize {
    let at = *k;
    if on {
        *k += n;
    }
    at
}

/// Surfaces `surfs` and dots at `points` through `cam` into a frame of
/// `format`, with depth (see [`render`]).
#[cfg(test)]
fn draw(surfs: &[Surface], points: &[Vec3], cam: &Camera, format: Size, samples: usize) -> Image {
    draw_passes(surfs, &[], points, cam, format, samples, Passes::DEPTH, None, None, None)
}

/// The object nodes of `surfs` in order (each once, unnamed ones left out):
/// object ids are 1 + an index into it.
fn object_nodes<'a>(surfs: &[Surface<'a>]) -> Vec<&'a str> {
    let mut nodes: Vec<&str> = Vec::new();
    for s in surfs {
        if !s.node.is_empty() && !nodes.contains(&s.node) {
            nodes.push(s.node);
        }
    }
    nodes
}

/// [`draw`] writing `passes` (see [`render_passes`]).
#[allow(clippy::too_many_arguments)]
fn draw_passes(surfs: &[Surface], lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Image {
    let n = samples.clamp(1, 16);
    // With the motion pass: each surface's vertices a frame later (the
    // surface at the same place in the next frame's list, if it's from the
    // same node; its own vertices if it has as many, so deforming meshes
    // move too), else where they are now.
    let next_cam = next.map_or(cam, |n| n.1);
    let next_world: Option<Vec<Vec<Vec3>>> = passes.motion.then(|| {
        let later = next.map(|n| surfaces(n.0)).unwrap_or_default();
        surfs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (world, positions) = match later.get(i).filter(|l| l.node == s.node) {
                    Some(l) if l.positions.len() == s.positions.len() => (l.world, &l.positions),
                    Some(l) => (l.world, &s.positions),
                    None => (s.world, &s.positions),
                };
                positions.iter().map(|p| geom::transform_point(&world, p)).collect()
            })
            .collect()
    });
    let tris = triangles(surfs, cam, format, next_world.as_deref());
    let rows = bin_rows(&tris, format.height);
    let frame = IRect::frame(format);
    let (w, h) = (format.width, format.height);
    // Objects (for id and mattes): the surfaces' nodes, then the splat
    // sets' (by set).
    let mut nodes = object_nodes(surfs);
    for n in splats.map_or(&[][..], |sp| sp.nodes()) {
        if !n.is_empty() && !nodes.contains(&n.as_str()) {
            nodes.push(n);
        }
    }
    let object: Vec<Option<usize>> = surfs.iter().map(|s| nodes.iter().position(|n| *n == s.node)).collect();
    let splat_object: Vec<Option<usize>> = splats.map_or(&[][..], |sp| sp.nodes()).iter().map(|s| nodes.iter().position(|n| n == s)).collect();
    let (light_names, light_of) = light_nodes(lights);
    // Per pixel, the extra channels, as asked for: P (3), N (3), uv (2), id,
    // a matte per node, the four lighting parts (3 each), each light node
    // (3 each).
    let mut k = 0;
    let p_at = slot(&mut k, passes.position, 3);
    let n_at = slot(&mut k, passes.normal, 3);
    let uv_at = slot(&mut k, passes.uv, 2);
    let motion_at = slot(&mut k, passes.motion, 2);
    let id_at = slot(&mut k, passes.id, 1);
    let matte_at = slot(&mut k, passes.mattes, nodes.len());
    let part_on = [passes.diffuse, passes.specular, passes.emission, passes.ambient];
    let part_at = part_on.map(|on| slot(&mut k, on, 3));
    let light_at = slot(&mut k, passes.lights, 3 * light_names.len());
    let shadow_at = slot(&mut k, passes.shadow, 3);
    let any_part = part_on.iter().any(|b| *b) || passes.lights || passes.shadow;
    let shadows = shadow::Shadows::build(if lights.is_empty() { &[] } else { surfs }, lights);
    // Each surface's texture's mipmaps, made if a sample sees it small.
    let mips: Vec<OnceLock<Arc<mip::Mips>>> = surfs.iter().map(|_| OnceLock::new()).collect();
    let mut color = vec![color::TRANSPARENT; w * h];
    let mut depth = vec![0.0f32; w * h];
    // (At least one channel, so every row has its chunk.)
    let stride = k.max(1);
    let mut extra = vec![0.0f32; w * h * stride];
    color
        .par_chunks_mut(w.max(1))
        .zip(depth.par_chunks_mut(w.max(1)))
        .zip(extra.par_chunks_mut((w * stride).max(1)))
        .enumerate()
        .for_each(|(y, ((crow, drow), xrow))| {
            let row = &rows[y];
            if (row.is_empty() && splats.is_none()) || cell.is_some_and(|c| y % c.side != c.y) {
                return;
            }
            let (yu, y) = (y, y as i32);
            // The splats at the current pixel, nearest first, and their
            // mattes blended alone.
            let mut frags: Vec<splats::Fragment> = Vec::new();
            let mut alone_mattes = vec![0.0f32; if passes.mattes { nodes.len() } else { 0 }];
            let mut hits: Vec<Hit> = Vec::new();
            let mut mattes = vec![0.0f32; nodes.len()];
            // Each hit's light in parts (and per light), when those passes
            // are asked for, and their sums over the pixel.
            let (mut parts, mut per_light, mut scratch): (Vec<Shaded>, Vec<[f32; 3]>, Vec<[f32; 3]>) = (Vec::new(), Vec::new(), Vec::new());
            let mut losts: Vec<[f32; 3]> = Vec::new();
            let mut light_sum = vec![[0.0f32; 3]; light_names.len()];
            // The triangles under the current pixel: those whose columns
            // reach it, added as the sweep passes their left edge.
            let (mut next, mut here): (usize, Vec<u32>) = (0, Vec::new());
            for (x, (out, dout)) in crow.iter_mut().zip(drow.iter_mut()).enumerate() {
                let xi = x as i32;
                while next < row.len() && tris[row[next] as usize].bounds.x0 <= xi {
                    here.push(row[next]);
                    next += 1;
                }
                here.retain(|&i| tris[i as usize].bounds.x1 > xi);
                if cell.is_some_and(|c| x % c.side != c.x) {
                    continue;
                }
                if let Some(sp) = splats {
                    sp.fragments(x, yu, &mut frags);
                }
                if here.is_empty() && frags.is_empty() {
                    continue;
                }
                // The splats alone, where a sample meets no surface.
                let alone = splats.filter(|_| !frags.is_empty()).map(|sp| blend_alone(&frags, sp, &splat_object, &mut alone_mattes));
                // Where the samples' blends first got half opaque (and the
                // splat there, if it was one), nearest.
                let mut median: Option<(f64, Option<u32>)> = None;
                let mut sum = [0.0f32; 4];
                // The nearest surface seen at least half opaque, for depth
                // and the other nearest-sample passes, and the nearest
                // fainter one (used where nothing more opaque is).
                let (mut nearest, mut faint): (Option<Hit>, Option<Hit>) = (None, None);
                mattes.iter_mut().for_each(|m| *m = 0.0);
                let mut part_sum = [[0.0f32; 3]; 4];
                let mut shadow_sum = [0.0f32; 3];
                light_sum.iter_mut().for_each(|v| *v = [0.0; 3]);
                for j in 0..n {
                    for i in 0..n {
                        let q = [xi as f64 + (i as f64 + 0.5) / n as f64, y as f64 + (j as f64 + 0.5) / n as f64];
                        hits.clear();
                        parts.clear();
                        losts.clear();
                        per_light.clear();
                        for &ti in &here {
                            let t = &tris[ti as usize];
                            let Some((d, bary)) = t.cover(q, cam.near, cam.far) else { continue };
                            // One hit where two triangles of a surface meet.
                            if hits.iter().any(|h| tris[h.tri].surf == t.surf && (h.depth - d).abs() <= 1e-9 * d) {
                                continue;
                            }
                            let s = &surfs[t.surf];
                            let uv = t.uv(&bary);
                            let texels = s.texture.map_or(0.0, |img| t.footprint(q, uv, 1.0 / n as f64, [img.format.width as f64, img.format.height as f64]));
                            let mut color = shade_covering(s, uv, texels, &mips[t.surf]);
                            let at = |f: fn(&Vert) -> Vec3| t.v.iter().zip(&bary).map(|(v, b)| f(v) * *b).sum::<Vec3>();
                            if s.headlight {
                                let (n, p) = (at(|v| v.n), at(|v| v.world));
                                let facing = n.try_normalize(1e-300).map_or(1.0, |n| n.dot(&cam.to_eye(&p)).abs());
                                let k = (0.25 + 0.75 * facing) as f32;
                                (0..3).for_each(|c| color[c] *= k);
                            }
                            let hit_at = || {
                                let p = at(|v| v.world);
                                let view = cam.to_eye(&p);
                                let n = at(|v| v.n).try_normalize(1e-300).unwrap_or_else(Vec3::z);
                                let normal = if s.two_sided && n.dot(&view) < 0.0 { -n } else { n };
                                shade::Hit { position: p, normal, view }
                            };
                            let (color, lit, lost) = light_hit(s, lights, &shadows, color, hit_at, passes.lights.then_some(&mut scratch));
                            if any_part {
                                parts.push(lit);
                                losts.push(lost);
                                if passes.lights {
                                    // Lights' shares, by light node (none
                                    // for a surface that takes no light).
                                    let mut by_node = vec![[0.0f32; 3]; light_names.len()];
                                    if scratch.len() == lights.len() {
                                        for (l, v) in scratch.iter().enumerate() {
                                            if let Some(o) = light_of[l] {
                                                (0..3).for_each(|c| by_node[o][c] += v[c]);
                                            }
                                        }
                                    }
                                    scratch.clear();
                                    per_light.extend(by_node);
                                }
                            }
                            hits.push(Hit { depth: d, tri: ti as usize, bary, color });
                        }
                        if hits.is_empty() {
                            if let Some(a) = &alone {
                                (0..4).for_each(|c| sum[c] += a.acc[c]);
                                median = nearer(median, a.half.map(|(d, i)| (d, Some(i))));
                                mattes.iter_mut().zip(&alone_mattes).for_each(|(m, a)| *m += a);
                                (0..3).for_each(|c| part_sum[2][c] += a.emission[c]);
                            }
                            continue;
                        }
                        let mut order: Vec<usize> = (0..hits.len()).collect();
                        order.sort_by(|&a, &b| hits[a].depth.total_cmp(&hits[b].depth).then(hits[a].tri.cmp(&hits[b].tri)));
                        // Front to back, premultiplied, with the splats in
                        // depth order among them; a holdout takes its share
                        // of what's left without adding to it.
                        let mut acc = [0.0f32; 4];
                        let mut held = 0.0f32;
                        let mut half: Option<(f64, Option<u32>)> = None;
                        let (mut oi, mut fi) = (0, 0);
                        loop {
                            let splat_next = match (order.get(oi), frags.get(fi)) {
                                (None, None) => break,
                                (Some(&hi), Some(f)) => f.depth < hits[hi].depth,
                                (h, _) => h.is_none(),
                            };
                            if let (true, Some(sp)) = (splat_next, splats) {
                                let f = frags[fi];
                                fi += 1;
                                let m = if passes.mattes { &mut mattes[..] } else { &mut [][..] };
                                add_splat(&f, sp, &splat_object, &mut acc, &mut held, m, &mut part_sum[2]);
                                if half.is_none() && acc[3] >= 0.5 {
                                    half = Some((f.depth, Some(f.splat)));
                                }
                                if acc[3] + held >= 1.0 {
                                    break;
                                }
                                continue;
                            }
                            let hi = order[oi];
                            oi += 1;
                            let hit = &hits[hi];
                            let surf = tris[hit.tri].surf;
                            let seen = if hit.color[3] >= 0.5 { &mut nearest } else { &mut faint };
                            if hit.color[3] > 0.0 && seen.is_none_or(|h| hit.depth < h.depth) {
                                *seen = Some(*hit);
                            }
                            let k = 1.0 - acc[3] - held;
                            if let Some(o) = object[surf].filter(|_| passes.mattes) {
                                mattes[o] += hit.color[3] * k;
                            }
                            if surfs[surf].holdout {
                                held += hit.color[3] * k;
                            } else {
                                (0..4).for_each(|c| acc[c] += hit.color[c] * k);
                                if any_part {
                                    let p = &parts[hi];
                                    for (sum, part) in part_sum.iter_mut().zip([p.diffuse, p.specular, p.emission, p.ambient]) {
                                        (0..3).for_each(|c| sum[c] += part[c] * k);
                                    }
                                    (0..3).for_each(|c| shadow_sum[c] += losts[hi][c] * k);
                                    if passes.lights {
                                        let nl = light_names.len();
                                        for (o, sum) in light_sum.iter_mut().enumerate() {
                                            (0..3).for_each(|c| sum[c] += per_light[hi * nl + o][c] * k);
                                        }
                                    }
                                }
                            }
                            if half.is_none() && acc[3] >= 0.5 {
                                half = Some((hit.depth, None));
                            }
                            if acc[3] + held >= 1.0 {
                                break;
                            }
                        }
                        median = nearer(median, half);
                        (0..4).for_each(|c| sum[c] += acc[c]);
                    }
                }
                let norm = 1.0 / (n * n) as f32;
                *out = sum.map(|v| v * norm);
                {
                    let px = &mut xrow[x * stride..x * stride + k];
                    for (p, sum) in part_sum.iter().enumerate().filter(|(p, _)| part_on[*p]) {
                        (0..3).for_each(|c| px[part_at[p] + c] = sum[c] * norm);
                    }
                    if passes.lights {
                        for (o, sum) in light_sum.iter().enumerate() {
                            (0..3).for_each(|c| px[light_at + 3 * o + c] = sum[c] * norm);
                        }
                    }
                    if passes.shadow {
                        (0..3).for_each(|c| px[shadow_at + c] = shadow_sum[c] * norm);
                    }
                }
                if passes.mattes {
                    let px = &mut xrow[x * stride..x * stride + k];
                    for (o, m) in mattes.iter().enumerate() {
                        px[matte_at + o] = m * norm;
                    }
                }
                // With splats here, depth is where the blend got half
                // opaque; else (and for the other passes) the surface's.
                let median = median.filter(|_| !frags.is_empty());
                if let Some((d, splat)) = median {
                    *dout = d as f32;
                    // Got half opaque at a splat: P there, the rest from
                    // the splat that gives the most.
                    if let (Some(sp), Some(_)) = (splats, splat) {
                        let px = &mut xrow[x * stride..x * stride + k];
                        let top = alone.as_ref().and_then(|a| a.dominant).or(splat).unwrap_or(0);
                        if passes.position {
                            let p = cam.unproject([xi as f64 + 0.5, y as f64 + 0.5], d, format);
                            px[p_at..p_at + 3].copy_from_slice(&[p.x as f32, p.y as f32, p.z as f32]);
                        }
                        if let Some(nv) = sp.normal(top).filter(|_| passes.normal) {
                            px[n_at..n_at + 3].copy_from_slice(&nv);
                        }
                        if let Some(m) = sp.motion(top).filter(|_| passes.motion) {
                            px[motion_at..motion_at + 2].copy_from_slice(&m);
                        }
                        if passes.id {
                            px[id_at] = splat_object[sp.set_of(top)].map_or(0.0, |o| (o + 1) as f32);
                        }
                        continue;
                    }
                }
                let Some(hit) = nearest.or(faint) else { continue };
                if median.is_none() {
                    *dout = hit.depth as f32;
                }
                let px = &mut xrow[x * stride..x * stride + k];
                let t = &tris[hit.tri];
                let at = |f: fn(&Vert) -> Vec3| t.v.iter().zip(&hit.bary).map(|(v, b)| f(v) * *b).sum::<Vec3>();
                if passes.position {
                    let p = at(|v| v.world);
                    px[p_at..p_at + 3].copy_from_slice(&[p.x as f32, p.y as f32, p.z as f32]);
                }
                if passes.normal {
                    let nv = at(|v| v.n).try_normalize(1e-300).unwrap_or_else(Vec3::zeros);
                    px[n_at..n_at + 3].copy_from_slice(&[nv.x as f32, nv.y as f32, nv.z as f32]);
                }
                if passes.uv {
                    let uv = t.uv(&hit.bary);
                    px[uv_at] = uv[0] as f32;
                    px[uv_at + 1] = (1.0 - uv[1]) as f32;
                }
                if passes.motion {
                    // Where the point lands next frame, less where it is
                    // now (0 if it goes behind the camera).
                    let (now, later) = (cam.project(&at(|v| v.world), format), next_cam.project(&at(|v| v.next), format));
                    if let (Some((a, _)), Some((b, _))) = (now, later) {
                        px[motion_at] = (b[0] - a[0]) as f32;
                        px[motion_at + 1] = (b[1] - a[1]) as f32;
                    }
                }
                if passes.id {
                    px[id_at] = object[t.surf].map_or(0.0, |o| (o + 1) as f32);
                }
            }
        });
    // Points: a one-pixel dot each, where nothing nearer is.
    for p in points {
        if let Some((q, d)) = cam.project(p, format) {
            let (x, y) = (q[0].floor() as i32, q[1].floor() as i32);
            if frame.contains(x, y) && d <= cam.far {
                let i = y as usize * w + x as usize;
                if depth[i] == 0.0 || (d as f32) < depth[i] {
                    color[i] = [1.0, 1.0, 1.0, 1.0];
                    depth[i] = d as f32;
                }
            }
        }
    }
    let mut out = Image::blank(format, frame);
    out.pixels = color;
    if passes.depth {
        *out.plane_mut("depth") = depth;
    }
    let mut plane = |name: String, c: usize| *out.plane_mut(&name) = (0..w * h).map(|i| extra[i * stride + c]).collect();
    if passes.position {
        ["x", "y", "z"].iter().enumerate().for_each(|(i, a)| plane(format!("P.{a}"), p_at + i));
    }
    if passes.normal {
        ["x", "y", "z"].iter().enumerate().for_each(|(i, a)| plane(format!("N.{a}"), n_at + i));
    }
    if passes.uv {
        plane("uv.u".into(), uv_at);
        plane("uv.v".into(), uv_at + 1);
    }
    if passes.motion {
        plane("motion.u".into(), motion_at);
        plane("motion.v".into(), motion_at + 1);
    }
    if passes.id {
        plane("id".into(), id_at);
    }
    if passes.mattes {
        nodes.iter().enumerate().for_each(|(o, node)| plane(format!("matte.{node}"), matte_at + o));
    }
    for (p, name) in ["diffuse", "specular", "emission", "ambient"].iter().enumerate().filter(|(p, _)| part_on[*p]) {
        ["R", "G", "B"].iter().enumerate().for_each(|(c, ch)| plane(format!("{name}.{ch}"), part_at[p] + c));
    }
    if passes.lights {
        for (o, node) in light_names.iter().enumerate() {
            ["R", "G", "B"].iter().enumerate().for_each(|(c, ch)| plane(format!("light.{node}.{ch}"), light_at + 3 * o + c));
        }
    }
    if passes.shadow {
        ["R", "G", "B"].iter().enumerate().for_each(|(c, ch)| plane(format!("shadow.{ch}"), shadow_at + c));
    }
    out
}

/// Splat fragments alone, blended: see [`blend_alone`].
struct Alone {
    /// The blend (premultiplied), and what of it is emission (all of it).
    acc: Rgba,
    emission: [f32; 3],
    /// Where it first got half opaque, and the splat there.
    half: Option<(f64, u32)>,
    /// The splat giving the most to it.
    dominant: Option<u32>,
}

/// Splat fragments `frags` (nearest first, from `sp`) blended front to
/// back, their sets' shares put in `mattes` (by object, `object` by set;
/// empty: no mattes).
fn blend_alone(frags: &[splats::Fragment], sp: &splats::Prepared, object: &[Option<usize>], mattes: &mut [f32]) -> Alone {
    mattes.iter_mut().for_each(|m| *m = 0.0);
    let (mut acc, mut held, mut emission) = ([0.0f32; 4], 0.0f32, [0.0f32; 3]);
    let (mut half, mut dominant) = (None, None::<(f32, u32)>);
    for f in frags {
        let share = f.color[3] * (1.0 - acc[3] - held);
        if dominant.is_none_or(|d| share > d.0) {
            dominant = Some((share, f.splat));
        }
        add_splat(f, sp, object, &mut acc, &mut held, mattes, &mut emission);
        if half.is_none() && acc[3] >= 0.5 {
            half = Some((f.depth, f.splat));
        }
    }
    Alone { acc, emission, half, dominant: dominant.map(|d| d.1) }
}

/// Splat fragment `f` (from `sp`) into a front-to-back blend `acc`
/// (premultiplied) with `held` taken by holdouts so far: its share to its
/// set's object in `mattes` (if there), and, unless it's a holdout's, to
/// `acc` and `emission`.
fn add_splat(f: &splats::Fragment, sp: &splats::Prepared, object: &[Option<usize>], acc: &mut Rgba, held: &mut f32, mattes: &mut [f32], emission: &mut [f32; 3]) {
    let k = 1.0 - acc[3] - *held;
    let set = sp.set_of(f.splat);
    if let Some(m) = object[set].and_then(|o| mattes.get_mut(o)) {
        *m += f.color[3] * k;
    }
    if sp.holdout(set) {
        *held += f.color[3] * k;
    } else {
        (0..4).for_each(|c| acc[c] += f.color[c] * k);
        (0..3).for_each(|c| emission[c] += f.color[c] * k);
    }
}

/// The nearer of two depths (each with what's there), either maybe
/// missing.
fn nearer<T>(a: Option<(f64, T)>, b: Option<(f64, T)>) -> Option<(f64, T)> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// `fg` (frame-sized) over `bg`: the union of both windows. Each of the
/// render's planes, where it shows nothing (no depth there), is the
/// background's plane of that name if it has one; the background's other
/// planes are kept.
fn over_background(fg: Image, bg: &Image) -> Image {
    let window = fg.window().union(&bg.window());
    let mut out = Image::blank(fg.format, window);
    let ww = window.width().max(1);
    let xy = |i: usize| (window.x0 + (i % ww) as i32, window.y0 + (i / ww) as i32);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let (x, y) = xy(i);
        let (f, b) = (fg.at(x, y), bg.at(x, y));
        *px = std::array::from_fn(|c| f[c] + b[c] * (1.0 - f[3]));
    });
    // Where the render saw a surface: it has depth there, or (without a
    // depth pass) any alpha.
    let fg_depth = fg.plane("depth");
    let shows = |x: i32, y: i32| fg_depth.map_or(fg.at(x, y)[3] > 0.0, |d| fg.plane_value(d, x, y) != 0.0);
    let names: Vec<String> = fg.planes.iter().chain(&bg.planes).map(|p| p.name.clone()).fold(Vec::new(), |mut v, n| {
        if !v.contains(&n) {
            v.push(n);
        }
        v
    });
    for name in names {
        let (f, b) = (fg.plane(&name), bg.plane(&name));
        let mut d = vec![0.0f32; out.width * out.height];
        d.par_iter_mut().enumerate().for_each(|(i, v)| {
            let (x, y) = xy(i);
            *v = match f {
                Some(f) if shows(x, y) => fg.plane_value(f, x, y),
                _ => b.map_or(0.0, |b| bg.plane_value(b, x, y)),
            };
        });
        *out.plane_mut(&name) = d;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Card;
    use std::sync::Arc;

    const FMT: Size = Size { width: 64, height: 32 };

    /// A 90° camera at the origin: at distance d it sees 2d across.
    fn camera() -> Camera {
        Camera { focal: 18.0, ..Camera::default() }
    }

    fn checker() -> Image {
        let mut img = Image::new(FMT.width, FMT.height);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let (x, y) = (i % FMT.width, i / FMT.width);
            *p = if (x / 4 + y / 4) % 2 == 0 { [0.9, 0.5, 0.1, 1.0] } else { [0.1, 0.2, 0.7, 1.0] };
        }
        img
    }

    fn card(img: Image, translate: [f64; 3], rotate: [f64; 3], size: [f64; 2]) -> Card {
        Card::new(Arc::new(img), geom::trs(translate, rotate, [1.0; 3]), size)
    }

    #[test]
    fn a_card_filling_the_frame_renders_like_its_image() {
        // 5 away, 10 × 5: exactly the frame, each pixel center on a texel's.
        let img = checker();
        let scene = Scene { cards: vec![card(img.clone(), [0.0, 0.0, -5.0], [0.0; 3], [10.0, 5.0])], ..Scene::default() };
        let out = render(&scene, &camera(), FMT, 1, None);
        let worst = out.pixels.iter().zip(&img.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{worst}");
        let d = &out.plane("depth").unwrap().data;
        assert!(d.iter().all(|v| (v - 5.0).abs() < 1e-4), "{:?}", &d[..4]);
        // Anti-aliased, the inside is unchanged.
        let aa = render(&scene, &camera(), FMT, 4, None);
        assert!((aa.at(30, 16)[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn perspective_depth_order_transparency_and_edges() {
        let white = Image::filled(8, 8, [1.0; 4]);
        // Turned 40° about Y: the right edge swings away, so it draws
        // shorter than the left.
        let tilted = Scene { cards: vec![card(white.clone(), [0.0, 0.0, -5.0], [0.0, 40.0, 0.0], [6.0, 3.0])], ..Scene::default() };
        let out = render(&tilted, &camera(), FMT, 1, None);
        let height = |x: i32| (0..32).filter(|&y| out.at(x, y)[3] > 0.5).count();
        let cols: Vec<i32> = (0..64).filter(|&x| height(x) > 0).collect();
        let (left, right) = (*cols.first().unwrap(), *cols.last().unwrap());
        assert!(height(left + 1) > height(right - 1) + 2, "{} vs {}", height(left + 1), height(right - 1));
        let d = |x: i32| out.plane_at("depth", x, 16);
        assert!(d(left + 1) < d(right - 1), "nearer on the left");
        // A small red card in front of a big blue one, in either order.
        let red = Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]);
        let blue = Image::filled(4, 4, [0.0, 0.0, 1.0, 1.0]);
        let front = card(red, [0.0, 0.0, -3.0], [0.0; 3], [1.0, 1.0]);
        let back = card(blue, [0.0, 0.0, -6.0], [0.0; 3], [20.0, 20.0]);
        for cards in [vec![front.clone(), back.clone()], vec![back.clone(), front.clone()]] {
            let out = render(&Scene { cards, ..Scene::default() }, &camera(), FMT, 1, None);
            assert_eq!((out.at(32, 16), out.at(5, 5)), ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]));
            assert!((out.plane_at("depth", 32, 16) - 3.0).abs() < 1e-4 && (out.plane_at("depth", 5, 5) - 6.0).abs() < 1e-4);
        }
        // Half-transparent in front: half of each.
        let ghost = card(Image::filled(4, 4, [0.5, 0.0, 0.0, 0.5]), [0.0, 0.0, -3.0], [0.0; 3], [1.0, 1.0]);
        let out = render(&Scene { cards: vec![back.clone(), ghost], ..Scene::default() }, &camera(), FMT, 1, None);
        assert_eq!(out.at(32, 16), [0.5, 0.0, 0.5, 1.0]);
        // Behind the camera, or cut by the near plane: drawn only in front.
        let behind = card(white.clone(), [0.0, 0.0, 5.0], [0.0; 3], [10.0, 10.0]);
        assert!(render(&Scene { cards: vec![behind], ..Scene::default() }, &camera(), FMT, 1, None).pixels.iter().all(|p| p[3] == 0.0));
        let floor = card(white, [0.0, -1.0, 0.0], [-90.0, 0.0, 0.0], [4.0, 40.0]);
        let out = render(&Scene { cards: vec![floor], ..Scene::default() }, &camera(), FMT, 1, None);
        assert!(out.at(32, 31)[3] > 0.5 && out.at(32, 2)[3] == 0.0, "a floor reaching past the camera fills the bottom");
        // Anti-aliasing: a card edge halfway across a pixel covers it half
        // (31 px wide at 6.4 px a unit: its left edge at x = 16.5).
        let half = card(Image::filled(4, 4, [1.0; 4]), [0.0, 0.0, -5.0], [0.0; 3], [31.0 / 6.4, 5.0]);
        let out = render(&Scene { cards: vec![half], ..Scene::default() }, &camera(), FMT, 4, None);
        assert!((out.at(16, 16)[3] - 0.5).abs() < 1e-6 && out.at(17, 16)[3] == 1.0 && out.at(15, 16)[3] == 0.0, "{:?}", out.at(16, 16));
    }

    /// A mesh from its corners and triangles, flat-shaded, in one color.
    fn mesh(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>, world: Mat4, color: Rgba) -> Surface<'static> {
        Surface { world, positions: positions.into(), normals: Cow::Owned(Vec::new()), uvs: Cow::Owned(Vec::new()), triangles: triangles.into(), color, texture: None, wrap: false, v_up: false, headlight: false, holdout: false, node: "", material: None, casts: true, receives: true, two_sided: false }
    }

    /// A cube `s` across, centered on the origin, its faces wound
    /// counter-clockwise from outside.
    fn cube(s: f64) -> (Vec<Vec3>, Vec<[u32; 3]>) {
        let h = s / 2.0;
        let p = (0..8).map(|i| Vec3::new(if i & 1 == 0 { -h } else { h }, if i & 2 == 0 { -h } else { h }, if i & 4 == 0 { -h } else { h })).collect();
        let quads = [[4, 5, 7, 6], [1, 0, 2, 3], [5, 1, 3, 7], [0, 4, 6, 2], [6, 7, 3, 2], [0, 1, 5, 4]];
        (p, quads.iter().flat_map(|q: &[u32; 4]| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect())
    }

    #[test]
    fn meshes_render_with_cards_in_depth_order() {
        // A unit cube 5 away, face on: its front, 4.5 away, is 32 / 4.5 =
        // 7.1 px across, centered (x 28.4–35.6, y 12.4–19.6).
        let (p, t) = cube(1.0);
        let red = mesh(p.clone(), t.clone(), geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]), [1.0, 0.0, 0.0, 1.0]);
        let out = draw(&[red], &[], &camera(), FMT, 1);
        let covered: Vec<(i32, i32)> = (0..32).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| out.at(x, y)[3] > 0.0).collect();
        let (xs, ys): (Vec<i32>, Vec<i32>) = covered.iter().copied().unzip();
        assert_eq!((xs.iter().min(), xs.iter().max(), ys.iter().min(), ys.iter().max()), (Some(&28), Some(&35), Some(&12), Some(&19)));
        assert_eq!(covered.len(), 64, "an 8 × 8 square");
        assert!((out.plane_at("depth", 32, 16) - 4.5).abs() < 1e-4);
        // Turned 45° about Y: two faces show, and the edge between them is
        // nearer than either face's middle.
        let turned = mesh(p.clone(), t.clone(), geom::trs([0.0, 0.0, -5.0], [0.0, 45.0, 0.0], [1.0; 3]), [1.0, 0.0, 0.0, 1.0]);
        let out = draw(&[turned], &[], &camera(), FMT, 1);
        let d = |x: i32| out.plane_at("depth", x, 16);
        assert!(d(32) < d(29) && d(32) < d(35) && (d(32) - (5.0 - 0.5f32.sqrt())).abs() < 0.1, "{} {} {}", d(29), d(32), d(35));
        // A half-transparent cube shows its front face once, not once per
        // triangle along the diagonal (and its back face behind it).
        let ghost = mesh(p.clone(), t.clone(), geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]), [0.5, 0.0, 0.0, 0.5]);
        let out = draw(&[ghost], &[], &camera(), FMT, 3);
        let a: Vec<f32> = (29..35).map(|x| out.at(x, 47 - x)[3]).collect();
        assert!(a.iter().all(|v| (v - 0.75).abs() < 1e-6), "front then back: {a:?}");
        // In front of a card and behind another.
        let white = Image::filled(4, 4, [1.0; 4]);
        let cards = Scene { cards: vec![card(white.clone(), [0.0, 0.0, -8.0], [0.0; 3], [20.0, 20.0]), card(Image::filled(4, 4, [0.0, 0.0, 1.0, 1.0]), [0.0, 0.0, -3.0], [0.0; 3], [0.2, 0.2])], ..Scene::default() };
        let mut surfs = surfaces(&cards);
        surfs.push(mesh(p, t, geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]), [1.0, 0.0, 0.0, 1.0]));
        let out = draw(&surfs, &[], &camera(), FMT, 1);
        assert_eq!((out.at(32, 16), out.at(30, 14), out.at(5, 5)), ([0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0], [1.0; 4]));
    }

    #[test]
    fn a_finely_divided_plane_renders_like_a_card() {
        // 200 × 200 squares (80 000 triangles) with the card's texture
        // coordinates: the same picture as the card, the texture repeated
        // where the coordinates run past 1.
        let img = Arc::new(checker());
        let n = 200;
        let positions: Vec<Vec3> = (0..=n).flat_map(|j| (0..=n).map(move |i| Vec3::new(10.0 * i as f64 / n as f64 - 5.0, 2.5 - 5.0 * j as f64 / n as f64, 0.0))).collect();
        let uvs: Vec<[f64; 2]> = (0..=n).flat_map(|j| (0..=n).map(move |i| [i as f64 / n as f64, j as f64 / n as f64])).collect();
        let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let triangles: Vec<[u32; 3]> = (0..n).flat_map(|j| (0..n).flat_map(move |i| [[at(i, j), at(i, j + 1), at(i + 1, j + 1)], [at(i, j), at(i + 1, j + 1), at(i + 1, j)]])).collect();
        let world = geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]);
        let plane = Surface { world, positions: positions.into(), normals: Cow::Owned(Vec::new()), uvs: uvs.clone().into(), triangles: triangles.clone().into(), color: [1.0; 4], texture: Some(&img), wrap: false, v_up: false, headlight: false, holdout: false, node: "", material: None, casts: true, receives: true, two_sided: false };
        let card = card_surface_of(&img, world);
        for samples in [1, 3] {
            let (a, b) = (draw(&[plane_ref(&plane)], &[], &camera(), FMT, samples), draw(&[card_surface(&card)], &[], &camera(), FMT, samples));
            let worst = a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
            assert!(worst < 1e-4, "{samples} samples: {worst}");
        }
        // Twice the coordinates, repeating: the checker twice over.
        let doubled: Vec<[f64; 2]> = uvs.iter().map(|uv| [uv[0] * 2.0, uv[1] * 2.0]).collect();
        let tiled = Surface { world, positions: plane.positions.clone(), normals: Cow::Owned(Vec::new()), uvs: doubled.into(), triangles: triangles.into(), color: [1.0; 4], texture: Some(&img), wrap: true, v_up: false, headlight: false, holdout: false, node: "", material: None, casts: true, receives: true, two_sided: false };
        let out = draw(&[tiled], &[], &camera(), FMT, 1);
        assert!((0..32).all(|y| (0..32).all(|x| out.at(x, y) == out.at(x + 32, y))), "repeats across");
    }

    /// A card of `img`, 10 × 5, placed by `world`.
    fn card_surface_of(img: &Image, world: Mat4) -> Card {
        Card::new(Arc::new(img.clone()), world, [10.0, 5.0])
    }

    /// A borrowed copy of `s`.
    fn plane_ref<'a>(s: &'a Surface) -> Surface<'a> {
        Surface { world: s.world, positions: Cow::Borrowed(&s.positions), normals: Cow::Borrowed(&s.normals), uvs: Cow::Borrowed(&s.uvs), triangles: Cow::Borrowed(&s.triangles), color: s.color, texture: s.texture, wrap: s.wrap, v_up: s.v_up, headlight: s.headlight, holdout: s.holdout, node: s.node, material: s.material, casts: s.casts, receives: s.receives, two_sided: s.two_sided }
    }

    /// Primitives and Read Geo through the graph, each at 5 in front of the
    /// usual 90° camera (64 × 32).
    #[test]
    fn primitives_and_read_geo_render_through_the_graph() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        let obj = format!("{}/examples/geo/crate.obj", env!("CARGO_MANIFEST_DIR"));
        let g: Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 1, "fps": 24, "format": { "width": 64, "height": 32 } },
            "nodes": [
                { "id": "cam", "op": "camera", "params": { "focal": 18 } },
                { "id": "box", "op": "cube", "params": { "translate": [0, 0, -5], "color": [1, 0, 0, 1] } },
                { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 32, "size": 4 } },
                { "id": "flat", "op": "plane", "inputs": ["plate"], "params": { "translate": [0, 0, -5], "size": [10, 5] } },
                { "id": "card1", "op": "card", "inputs": ["plate"], "params": { "translate": [0, 0, -5], "size": [10, 5] } },
                { "id": "crate", "op": "read_geo", "params": { "file": obj, "translate": [0, 0, -5] } },
                { "id": "crate_gltf", "op": "read_geo", "params": { "file": obj.replace(".obj", ".gltf"), "translate": [0, 0, -5] } },
                { "id": "nofile", "op": "read_geo", "params": { "file": "/no/such/file.obj" } },
                { "id": "r_box", "op": "scanline_render", "inputs": ["box", "cam", ""], "params": { "samples": 1 } },
                { "id": "r_flat", "op": "scanline_render", "inputs": ["flat", "cam", ""], "params": { "samples": 1 } },
                { "id": "r_card", "op": "scanline_render", "inputs": ["card1", "cam", ""], "params": { "samples": 1 } },
                { "id": "r_crate", "op": "scanline_render", "inputs": ["crate", "cam", ""], "params": { "samples": 1 } },
                { "id": "r_gltf", "op": "scanline_render", "inputs": ["crate_gltf", "cam", ""], "params": { "samples": 1 } },
                { "id": "r_nofile", "op": "scanline_render", "inputs": ["nofile", "cam", ""] },
            ] })).unwrap();
        let mut ev = Evaluator::new();
        let at = |ev: &mut Evaluator, node: &str| ev.evaluate(&g, node, &EvalContext { frame: 1, write_outputs: false });
        // A unit cube face on: its front (4.5 away) an 8 × 8 square, red.
        let boxed = at(&mut ev, "r_box").unwrap();
        let covered = |img: &Image| (0..32).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| img.at(x, y)[3] > 0.0).collect::<Vec<_>>();
        let square = covered(&boxed);
        assert_eq!((square.len(), square[0], *square.last().unwrap()), (64, (28, 12), (35, 19)));
        assert_eq!(boxed.at(32, 16), [1.0, 0.0, 0.0, 1.0]);
        // A plane with the plate is the card with the plate.
        let (flat, card) = (at(&mut ev, "r_flat").unwrap(), at(&mut ev, "r_card").unwrap());
        let worst = flat.pixels.iter().zip(&card.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{worst}");
        // The crate file: the same cube, in its material's color.
        let s = at(&mut ev, "crate").unwrap();
        assert_eq!(s.scene.as_ref().unwrap().describe(), "0 cameras, 0 cards, 1 mesh");
        let crate_img = at(&mut ev, "r_crate").unwrap();
        assert_eq!(covered(&crate_img), square);
        let c = crate_img.at(32, 16);
        assert!((c[0] - 0.6).abs() < 1e-6 && (c[1] - 0.4).abs() < 1e-6 && (c[2] - 0.2).abs() < 1e-6 && c[3] == 1.0, "{c:?}");
        // The same crate as glTF: the same square.
        assert_eq!(covered(&at(&mut ev, "r_gltf").unwrap()), square);
        let e = at(&mut ev, "r_nofile").err().unwrap();
        assert!(e.message.contains("can't read /no/such/file.obj"), "{}", e.message);
    }

    #[test]
    fn the_preview_lights_meshes_from_the_camera() {
        let sphere = crate::scene::primitive_model("sphere", &serde_json::json!({ "radius": 2.0, "segments": [48, 24], "translate": [0, 0, -5] }), None).unwrap();
        let red = card(Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]), [4.5, 0.0, -5.0], [0.0, 60.0, 0.0], [2.0, 2.0]);
        let scene = Scene { cards: vec![red], meshes: vec![sphere], ..Scene::default() };
        let out = render_preview(&scene, &camera(), FMT, Preview::Solid);
        // Facing the camera in the middle, turning away at the rim; the
        // card as it renders.
        let (mid, rim) = (out.at(32, 16)[0], out.at(32, 16 - 13)[0]);
        assert!(mid > 0.98 && rim < mid - 0.05 && rim > 0.25, "{mid} {rim}");
        assert_eq!(out.at(60, 16), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(render(&scene, &camera(), FMT, 1, None).at(32, 16 - 13), [1.0; 4], "unlit in a render");
    }

    #[test]
    fn a_textured_sphere_has_no_seam() {
        // A texture whose color only changes across, and comes back round:
        // red 1 at its left and right edges, 0 in the middle.
        let (w, h) = (64, 16);
        let mut tex = Image::new(w, h);
        for (i, p) in tex.pixels.iter_mut().enumerate() {
            let u = ((i % w) as f64 + 0.5) / w as f64;
            *p = [(0.5 + 0.5 * (std::f64::consts::TAU * u).cos()) as f32, 0.0, 0.0, 1.0];
        }
        let sphere = crate::scene::primitive_model("sphere", &serde_json::json!({ "radius": 2.0, "segments": [48, 24], "translate": [0, 0, -5], "rotate": [0, 180, 0] }), Some(&tex)).unwrap();
        let out = render(&Scene { meshes: vec![sphere.clone()], ..Scene::default() }, &camera(), FMT, 3, None);
        // Turned round, its seam faces the camera down the middle: no crack
        // (full coverage) and no jump in color across it.
        for y in 10..22 {
            for x in 28..36 {
                let p = out.at(x, y);
                assert!(p[3] > 0.999 && p[0] > 0.9, "({x}, {y}): {p:?}");
            }
        }
        // Facing the other way, the texture's middle (red 0) shows there.
        let front = crate::scene::Model { world: geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]), ..sphere };
        let out = render(&Scene { meshes: vec![front], ..Scene::default() }, &camera(), FMT, 3, None);
        assert!(out.at(32, 16)[0] < 0.1 && out.at(32, 16)[3] > 0.999);
    }

    /// A model of primitive `op` from `params`, named `node`.
    fn model(op: &str, params: serde_json::Value, node: &str) -> crate::scene::Model {
        crate::scene::Model { node: node.into(), ..crate::scene::primitive_model(op, &params, None).unwrap() }
    }

    /// The geometry passes.
    const ALL: Passes = Passes { depth: true, position: true, normal: true, uv: true, id: true, mattes: true, ..Passes::NONE };

    #[test]
    fn position_normal_uv_and_id_passes() {
        use serde_json::json;
        // A floor seen from above at an angle, a ball on it, and a card
        // standing behind; through a camera that isn't at the origin.
        let cam = Camera { focal: 18.0, world: geom::trs([0.5, 2.0, 3.0], [-25.0, 5.0, 0.0], [1.0; 3]), ..Camera::default() };
        let floor = model("plane", json!({ "size": [20, 20], "rotate": [-90, 0, 0] }), "floor");
        let ball = model("sphere", json!({ "radius": 0.7, "translate": [0, 0.7, -2] }), "ball");
        let mut back = card(checker(), [0.0, 2.0, -6.0], [0.0; 3], [8.0, 4.0]);
        back.node = "back".into();
        let scene = Scene { cards: vec![back], meshes: vec![floor, ball], ..Scene::default() };
        let out = render_passes(&scene, &cam, FMT, 1, None, ALL);
        let names: Vec<&str> = out.planes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["depth", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "uv.u", "uv.v", "id", "matte.back", "matte.floor", "matte.ball"]);
        let at = |n: &str, x: i32, y: i32| out.plane_at(n, x, y) as f64;
        let p = |x: i32, y: i32| Vec3::new(at("P.x", x, y), at("P.y", x, y), at("P.z", x, y));
        let nrm = |x: i32, y: i32| Vec3::new(at("N.x", x, y), at("N.y", x, y), at("N.z", x, y));
        let mut seen = [0usize; 4];
        for y in 0..32 {
            for x in 0..64 {
                let id = at("id", x, y) as usize;
                seen[id] += 1;
                if id == 0 {
                    assert_eq!((at("depth", x, y), p(x, y)), (0.0, Vec3::zeros()), "nothing there");
                    continue;
                }
                // P is the point at the pixel's depth along its ray (one
                // sample, at the pixel's center).
                let want = cam.unproject([x as f64 + 0.5, y as f64 + 0.5], at("depth", x, y), FMT);
                assert!((p(x, y) - want).norm() < 1e-4 * want.norm().max(1.0), "({x}, {y}): {} vs {want}", p(x, y));
                let n = nrm(x, y);
                assert!((n.norm() - 1.0).abs() < 1e-5);
                match id {
                    // The card faces +Z, the floor up; P on them lies in
                    // their planes.
                    1 => assert!((n - Vec3::z()).norm() < 1e-5 && (p(x, y).z + 6.0).abs() < 1e-4),
                    2 => assert!((n - Vec3::y()).norm() < 1e-5 && p(x, y).y.abs() < 1e-4),
                    // The ball's normal points from its center out to P.
                    _ => assert!((n - (p(x, y) - Vec3::new(0.0, 0.7, -2.0)).normalize()).norm() < 0.02, "{n}"),
                }
            }
        }
        assert!(seen.iter().all(|&c| c > 20), "each object shows, and the sky: {seen:?}");
        // uv on the card: its texture coordinates, v up (its top edge at 1).
        let card_px = (0..32).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| at("id", x, y) == 1.0);
        for (x, y) in card_px {
            let q = p(x, y);
            let (u, v) = ((q.x + 4.0) / 8.0, (q.y - 0.0) / 4.0);
            assert!((at("uv.u", x, y) - u).abs() < 1e-4 && (at("uv.v", x, y) - v).abs() < 1e-4, "({x}, {y})");
        }
        // Without passes asked for, none are written (depth as before).
        let plain = render(&scene, &cam, FMT, 1, None);
        assert_eq!(plain.planes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["depth"]);
        assert_eq!(plain.pixels, out.pixels, "the beauty doesn't change");
    }

    #[test]
    fn mattes_cover_their_objects_and_add_up_to_alpha() {
        use serde_json::json;
        // Two cubes side by side, apart, and a card behind (not overlapping
        // them), anti-aliased.
        let a = model("cube", json!({ "translate": [-1.5, 0, -5], "rotate": [10, 30, 0] }), "a");
        let b = model("cube", json!({ "translate": [1.5, 0, -5], "color": [1, 1, 1, 0.5] }), "b");
        let mut far = card(Image::filled(4, 4, [1.0; 4]), [0.0, 0.0, -9.0], [0.0; 3], [2.0, 1.0]);
        far.node = "far".into();
        let scene = Scene { cards: vec![far], meshes: vec![a, b], ..Scene::default() };
        let out = render_passes(&scene, &camera(), FMT, 4, None, Passes { mattes: true, ..Passes::default() });
        let m = |n: &str, x: i32, y: i32| out.plane_at(&format!("matte.{n}"), x, y);
        let (mut fractional, mut worst) = (0, 0.0f32);
        for y in 0..32 {
            for x in 0..64 {
                let total = m("a", x, y) + m("b", x, y) + m("far", x, y);
                worst = worst.max((total - out.at(x, y)[3]).abs());
                fractional += (m("a", x, y) > 0.01 && m("a", x, y) < 0.99) as usize;
            }
        }
        assert!(worst < 1e-5, "the mattes add up to alpha: {worst}");
        assert!(fractional > 8, "soft edges: {fractional}");
        // 1 inside the opaque cube, 0 outside; the half-transparent one
        // covers three quarters (its front face, then its back); the card
        // shows between.
        assert_eq!((m("a", 22, 16), m("a", 42, 16), m("a", 2, 2)), (1.0, 0.0, 0.0));
        assert!((m("b", 42, 16) - 0.75).abs() < 1e-6);
        assert_eq!(m("far", 32, 16), 1.0);
        // No depth pass asked for: none written.
        assert!(out.plane("depth").is_none());
    }

    #[test]
    fn a_holdout_cuts_out_what_is_behind_it() {
        use serde_json::json;
        // A big red card at the back, a holdout cube in front of its
        // middle, and a small blue card in front of the cube's edge.
        let mut back = card(Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]), [0.0, 0.0, -8.0], [0.0; 3], [20.0, 10.0]);
        back.node = "back".into();
        let mut front = card(Image::filled(4, 4, [0.0, 0.0, 1.0, 1.0]), [0.6, 0.0, -3.0], [0.0; 3], [0.5, 0.5]);
        front.node = "front".into();
        let hold = crate::scene::Model { holdout: true, ..model("cube", json!({ "translate": [0, 0, -5], "size": [2, 2, 2] }), "hold") };
        let scene = Scene { cards: vec![back, front], meshes: vec![hold], ..Scene::default() };
        let out = render_passes(&scene, &camera(), FMT, 2, None, ALL);
        // Inside the cube's outline: nothing (alpha 0), where only the
        // back card would show; outside it, the back card; in front of it,
        // the blue card.
        assert_eq!(out.at(28, 16), [0.0; 4], "cut out");
        assert_eq!(out.at(4, 4), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(out.at(38, 16), [0.0, 0.0, 1.0, 1.0], "in front of the holdout");
        // Mattes still work: the back card's is 0 where it's hidden, the
        // holdout's is its coverage; depth and id are the holdout's there.
        let m = |n: &str, x: i32, y: i32| out.plane_at(&format!("matte.{n}"), x, y);
        assert_eq!((m("back", 28, 16), m("hold", 28, 16), m("back", 4, 4), m("front", 38, 16)), (0.0, 1.0, 1.0, 1.0));
        assert!((out.plane_at("depth", 28, 16) - 4.0).abs() < 1e-4);
        assert_eq!(out.plane_at("id", 28, 16), 3.0, "back, front, hold");
    }

    #[test]
    fn passes_over_a_background_keep_its_planes() {
        let mut bg = Image::filled(64, 32, [0.0, 0.5, 0.0, 1.0]);
        *bg.plane_mut("depth") = vec![50.0; 64 * 32];
        *bg.plane_mut("id") = vec![7.0; 64 * 32];
        *bg.plane_mut("other") = vec![3.0; 64 * 32];
        let mut red = card(Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]), [0.0, 0.0, -3.0], [0.0; 3], [1.0, 1.0]);
        red.node = "red".into();
        let out = render_passes(&Scene { cards: vec![red], ..Scene::default() }, &camera(), FMT, 1, Some(&bg), Passes { id: true, mattes: true, ..Passes::DEPTH });
        let v = |n: &str, x: i32, y: i32| out.plane_at(n, x, y);
        assert_eq!((v("id", 32, 16), v("id", 2, 2)), (1.0, 7.0), "the render's id, else the background's");
        assert_eq!((v("matte.red", 32, 16), v("matte.red", 2, 2)), (1.0, 0.0));
        assert_eq!((v("other", 32, 16), v("other", 2, 2), v("depth", 2, 2)), (3.0, 3.0, 50.0), "the background's other planes kept");
    }

    /// The Passes list and Holdout through the graph and the session.
    #[test]
    fn the_scanline_render_writes_the_passes_it_lists() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        let g: Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 1, "fps": 24, "format": { "width": 64, "height": 32 } },
            "nodes": [
                { "id": "cam", "op": "camera", "params": { "focal": 18 } },
                { "id": "ball", "op": "sphere", "params": { "translate": [0, 0, -5], "radius": 2 } },
                { "id": "hole", "op": "cube", "params": { "translate": [0.6, 0, -3], "size": [0.4, 0.4, 0.4], "holdout": true } },
                { "id": "all", "op": "scene", "inputs": ["ball", "hole", "", ""] },
                { "id": "plain", "op": "scanline_render", "inputs": ["all", "cam", ""], "params": { "samples": 1 } },
                { "id": "passes", "op": "scanline_render", "inputs": ["all", "cam", ""], "params": { "samples": 1, "passes": ["P", "mattes"] } },
            ] })).unwrap();
        let mut ev = Evaluator::new();
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let plain = ev.evaluate(&g, "plain", &ctx).unwrap();
        let names = |img: &Image| img.planes.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&plain), ["depth"], "no Passes param (an older project): depth, as before");
        let out = ev.evaluate(&g, "passes", &ctx).unwrap();
        assert_eq!(names(&out), ["P.x", "P.y", "P.z", "matte.ball", "matte.hole"]);
        // The holdout cube (in front of the ball's right side) cuts it out.
        let hole = (32.0 + 0.6 / 3.0 * 32.0) as i32;
        assert_eq!((out.at(hole, 16)[3], out.plane_at("matte.hole", hole, 16), out.plane_at("matte.ball", hole, 16)), (0.0, 1.0, 0.0));
        assert!(out.at(26, 16)[3] == 1.0 && out.plane_at("matte.ball", 26, 16) == 1.0);
        // The session takes a list of pass names, and refuses others.
        let mut s = crate::session::Session::new(g);
        let set = |s: &mut crate::session::Session, v: serde_json::Value| s.set_params("passes", json!({ "passes": v }).as_object().unwrap());
        set(&mut s, json!(["N", "id", "N"])).unwrap();
        assert_eq!(s.graph.node("passes").unwrap().params["passes"], json!(["N", "id"]));
        assert!(set(&mut s, json!(["normals"])).unwrap_err().to_string().contains("not one of 'passes'"));
        assert!(set(&mut s, json!("N")).is_err());
        let schema = crate::session::op_schema(crate::ops::info("scanline_render").unwrap());
        let p = schema["params"].as_array().unwrap().iter().find(|p| p["name"] == "passes").unwrap().clone();
        assert_eq!((p["kind"].as_str(), p["default"].clone(), p["choices"].as_array().map(Vec::len)), (Some("flags"), json!(["depth"]), Some(Passes::NAMES.len())));
    }

    /// A light of `op` from `params`, named `node`.
    fn light(op: &str, params: serde_json::Value, node: &str) -> Light {
        Light { node: node.into(), ..crate::scene::light(op, &params, None).unwrap() }
    }

    const LIGHTING: Passes = Passes { diffuse: true, specular: true, emission: true, ambient: true, lights: true, ..Passes::NONE };

    /// A white card 1 × 1 at height 1 over a gray floor, seen from 10
    /// straight above (6 units across); `l` the only light.
    fn shadow_scene(l: Light) -> (Scene, Camera) {
        use serde_json::json;
        let floor = model("plane", json!({ "size": [12, 12], "rotate": [-90, 0, 0], "color": [0.5, 0.5, 0.5, 1] }), "floor");
        let mut c = card(Image::filled(4, 4, [1.0; 4]), [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0], [1.0, 1.0]);
        c.node = "card".into();
        let cam = Camera { focal: 60.0, world: geom::trs([0.0, 10.0, 0.0], [-90.0, 0.0, 0.0], [1.0; 3]), ..Camera::default() };
        (Scene { cards: vec![c], meshes: vec![floor], lights: vec![l], ..Scene::default() }, cam)
    }

    /// The floor's brightness (red) at scene point (x, 0, z), and the
    /// shadow pass there.
    fn floor_at(out: &Image, cam: &Camera, x: f64, z: f64) -> (f32, f32) {
        let (q, _) = cam.project(&Vec3::new(x, 0.0, z), SHADOW_FMT).unwrap();
        let (px, py) = (q[0].floor() as i32, q[1].floor() as i32);
        (out.at(px, py)[0], out.plane_at("shadow.R", px, py))
    }

    const SHADOW_FMT: Size = Size { width: 160, height: 120 };

    #[test]
    fn a_card_over_a_floor_casts_a_shadow_where_projection_says() {
        use serde_json::json;
        let passes = Passes { shadow: true, ..Passes::DEPTH };
        // Light coming in 45° from +X: the card's shadow lands 1 to the
        // left of it, (−1.5 … −0.5, ±0.5); a point light 2 above the card
        // throws it twice as big, straight down; a spot the same.
        let sun = light("light_directional", json!({ "rotate": [-90, 0, -45], "shadows": true, "shadow_softness": 0 }), "sun");
        let bulb = light("light_point", json!({ "translate": [0, 3, 0], "shadows": true, "shadow_softness": 0 }), "bulb");
        let spot = light("light_spot", json!({ "translate": [0, 3, 0], "rotate": [-90, 0, 0], "cone": 120, "penumbra": 0, "shadows": true, "shadow_softness": 0 }), "spot");
        for (l, inside, outside) in [(&sun, [(-1.0, 0.0), (-1.3, 0.3), (-0.7, -0.3)], [(-1.8, 0.0), (1.0, 0.0), (-1.0, 0.8)]), (&bulb, [(0.65, 0.0), (0.0, -0.65), (-0.65, 0.65)], [(0.9, 0.0), (0.0, 1.1), (-0.85, 0.85)]), (&spot, [(0.65, 0.0), (0.0, -0.65), (-0.65, 0.65)], [(0.9, 0.0), (0.0, 1.1), (-0.85, 0.85)])] {
            let (scene, cam) = shadow_scene(l.clone());
            let out = render_passes(&scene, &cam, SHADOW_FMT, 2, None, passes);
            let lit = render_passes(&Scene { lights: vec![Light { shadows: false, ..l.clone() }], ..scene.clone() }, &cam, SHADOW_FMT, 2, None, passes);
            for (x, z) in inside {
                let (v, gone) = floor_at(&out, &cam, x, z);
                let (full, _) = floor_at(&lit, &cam, x, z);
                assert!(v < 0.02 * full && (gone - full).abs() < 0.02 * full, "{}: ({x}, {z}) in shadow: {v} of {full}, the pass {gone}", l.node);
            }
            for (x, z) in outside {
                let (v, gone) = floor_at(&out, &cam, x, z);
                let (full, _) = floor_at(&lit, &cam, x, z);
                assert!(full > 0.0 && (v - full).abs() < 1e-4 && gone == 0.0, "{}: ({x}, {z}) lit: {v} of {full}", l.node);
            }
            // The beauty plus the shadow pass is the picture without shadows,
            // everywhere (and the card itself is unaffected).
            for y in 0..SHADOW_FMT.height as i32 {
                for x in 0..SHADOW_FMT.width as i32 {
                    let back = out.at(x, y)[0] + out.plane_at("shadow.R", x, y);
                    assert!((back - lit.at(x, y)[0]).abs() < 1e-4, "{}: ({x}, {y})", l.node);
                }
            }
        }
        // A card that doesn't cast, or a floor that doesn't receive: no
        // shadow.
        let (mut scene, cam) = shadow_scene(sun.clone());
        scene.cards[0].casts_shadows = false;
        assert!((floor_at(&render_passes(&scene, &cam, SHADOW_FMT, 1, None, passes), &cam, -1.0, 0.0).0 - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        let (mut scene, cam) = shadow_scene(sun.clone());
        scene.meshes[0].receives_shadows = false;
        assert!(floor_at(&render_passes(&scene, &cam, SHADOW_FMT, 1, None, passes), &cam, -1.0, 0.0).0 > 0.3);
        // Shadows off on the light (the default): none.
        let (scene, cam) = shadow_scene(light("light_directional", json!({ "rotate": [-90, 0, -45] }), "sun"));
        assert!(floor_at(&render(&scene, &cam, SHADOW_FMT, 1, None), &cam, -1.0, 0.0).0 > 0.3);
    }

    #[test]
    fn a_card_takes_light_when_shaded_from_either_side() {
        use serde_json::json;
        let white = Image::filled(4, 4, [1.0; 4]);
        let at = |p: serde_json::Value| crate::scene::card(&p, &white);
        // Unlit by default (and when asked): the image as it is, lights
        // or not.
        assert!(at(json!({})).material.is_none() && at(json!({ "shading": "unlit" })).material.is_none());
        let place = json!({ "translate": [0, 0, -5], "size": [20, 20] });
        let shaded = |shading: &str, rotate: [i32; 3]| {
            let mut p = place.clone();
            p["shading"] = json!(shading);
            p["rotate"] = json!(rotate);
            at(p)
        };
        // A sun shining away from the camera (onto the card's face), or
        // toward it (from behind the card).
        let from_camera = light("light_directional", json!({}), "sun");
        let from_behind = light("light_directional", json!({ "rotate": [0, 180, 0] }), "sun");
        let seen = |card: Card, l: &Light| render(&Scene { cards: vec![card], lights: vec![l.clone()], ..Scene::default() }, &camera(), FMT, 1, None).at(32, 16);
        assert_eq!(seen(shaded("unlit", [0, 0, 0]), &from_behind), [1.0; 4], "unlit ignores the light");
        let lit = seen(shaded("lambert", [0, 0, 0]), &from_camera);
        assert!(lit[0] > 0.5 && lit[3] == 1.0, "{lit:?}");
        assert!(seen(shaded("lambert", [0, 0, 0]), &from_behind)[0] < 0.01, "lit from behind, dark in front");
        // Seen from its back, lit from the camera: lit as its front is (two
        // sided, like a real card).
        assert_eq!(seen(shaded("lambert", [0, 180, 0]), &from_camera), lit);
        // Blinn adds a highlight; Emission glows without lights.
        let mut shiny = place.clone();
        shiny["shading"] = json!("blinn");
        shiny["specular"] = json!(1.0);
        assert!(seen(at(shiny), &from_camera)[0] > lit[0]);
        let mut glow = place.clone();
        glow["shading"] = json!("lambert");
        glow["emission"] = json!([0.25, 0, 0, 1]);
        let dark = render(&Scene { cards: vec![at(glow)], ..Scene::default() }, &camera(), FMT, 1, None).at(32, 16);
        assert!(dark[0] >= 1.0 && dark[1] == 1.0, "no lights: the image, plus its glow: {dark:?}");
        // Its light shows in the passes.
        let out = render_passes(&Scene { cards: vec![shaded("lambert", [0, 0, 0])], lights: vec![from_camera.clone()], ..Scene::default() }, &camera(), FMT, 1, None, LIGHTING);
        assert!((out.plane_at("diffuse.R", 32, 16) - lit[0]).abs() < 1e-5 && out.plane_at("light.sun.R", 32, 16) > 0.5);
        // And the 3D view's Lit preview lights it too; Solid shows its image.
        assert_eq!(render_preview(&Scene { cards: vec![shaded("lambert", [0, 0, 0])], lights: vec![from_behind.clone()], ..Scene::default() }, &camera(), FMT, Preview::Lit { shadows: false }).at(32, 16)[0], 0.0);
        assert_eq!(render_preview(&Scene { cards: vec![shaded("lambert", [0, 0, 0])], lights: vec![from_behind], ..Scene::default() }, &camera(), FMT, Preview::Solid).at(32, 16), [1.0; 4]);
    }

    #[test]
    fn a_lit_card_takes_shadows_unless_told_not_to() {
        use serde_json::json;
        let sun = light("light_directional", json!({ "rotate": [-90, 0, -45], "shadows": true, "shadow_softness": 0 }), "sun");
        let (mut scene, cam) = shadow_scene(sun);
        // A lit card for a floor instead of the mesh.
        let floor = |receives: bool| crate::scene::card(&json!({ "shading": "lambert", "rotate": [-90, 0, 0], "size": [12, 12], "receives_shadows": receives }), &Image::filled(4, 4, [0.5, 0.5, 0.5, 1.0]));
        scene.meshes.clear();
        for receives in [true, false] {
            scene.cards.truncate(1);
            scene.cards.push(floor(receives));
            let out = render(&scene, &cam, SHADOW_FMT, 1, None);
            let (inside, outside) = (floor_at(&out, &cam, -1.0, 0.0).0, floor_at(&out, &cam, 2.0, 2.0).0);
            assert!(outside > 0.2, "{outside}");
            if receives {
                assert!(inside < 0.1 * outside, "{inside} {outside}");
            } else {
                assert!((inside - outside).abs() < 1e-4, "{inside} {outside}");
            }
        }
    }

    #[test]
    fn a_cards_opacity_fades_it_and_its_shadow() {
        use serde_json::json;
        let red = Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]);
        let o = crate::scene::card(&json!({ "opacity": 0.5 }), &red).opacity;
        assert!(o == 0.5 && crate::scene::card(&json!({}), &red).opacity == 1.0, "{o}");
        // Half a red card over blue: half of each.
        let faded = |img: &Image, z: f64, size: f64, opacity: f32| Card { opacity, ..card(img.clone(), [0.0, 0.0, z], [0.0; 3], [size, size]) };
        let blue = Image::filled(64, 32, [0.0, 0.0, 1.0, 1.0]);
        let out = render(&Scene { cards: vec![faded(&red, -5.0, 20.0, 0.5)], ..Scene::default() }, &camera(), FMT, 1, Some(&blue));
        assert_eq!(out.at(32, 16), [0.5, 0.0, 0.5, 1.0]);
        // The 3D view's preview shows it faded too.
        assert_eq!(render_preview(&Scene { cards: vec![faded(&red, -5.0, 20.0, 0.5)], ..Scene::default() }, &camera(), FMT, Preview::Solid).at(32, 16), [0.5, 0.0, 0.0, 0.5]);
        // Two half cards stack front to back in either order.
        let green = Image::filled(4, 4, [0.0, 1.0, 0.0, 1.0]);
        let (front, back) = (faded(&red, -3.0, 20.0, 0.5), faded(&green, -6.0, 40.0, 0.5));
        for cards in [vec![front.clone(), back.clone()], vec![back.clone(), front.clone()]] {
            assert_eq!(render(&Scene { cards, ..Scene::default() }, &camera(), FMT, 1, None).at(32, 16), [0.5, 0.25, 0.0, 0.75]);
        }
        // The data passes skip a card under half opaque for what shows
        // through it; alone, it has them.
        let depth_at = |cards: Vec<Card>| render_passes(&Scene { cards, ..Scene::default() }, &camera(), FMT, 1, None, Passes::DEPTH).plane_at("depth", 32, 16);
        assert_eq!(depth_at(vec![faded(&red, -3.0, 20.0, 0.3), faded(&green, -6.0, 40.0, 1.0)]), 6.0);
        assert_eq!(depth_at(vec![faded(&red, -3.0, 20.0, 0.5), faded(&green, -6.0, 40.0, 1.0)]), 3.0);
        assert_eq!(depth_at(vec![faded(&red, -3.0, 20.0, 0.3)]), 3.0);
        // Its shadow is as strong as it is opaque (opacity times its
        // image's alpha), and two let through the product of theirs: under
        // a sun and a point light, at a floor point inside the card's shadow.
        let sun = light("light_directional", json!({ "rotate": [-90, 0, -45], "shadows": true, "shadow_softness": 0 }), "sun");
        let bulb = light("light_point", json!({ "translate": [0, 3, 0], "shadows": true, "shadow_softness": 0 }), "bulb");
        for (l, at) in [(&sun, (-1.0, 0.0)), (&bulb, (0.65, 0.0))] {
            let lit = |scene: &Scene| {
                let (scene, cam) = (scene.clone(), shadow_scene(l.clone()).1);
                let full = floor_at(&render(&Scene { lights: vec![Light { shadows: false, ..l.clone() }], ..scene.clone() }, &cam, SHADOW_FMT, 1, None), &cam, at.0, at.1).0;
                floor_at(&render(&scene, &cam, SHADOW_FMT, 1, None), &cam, at.0, at.1).0 / full
            };
            let (mut scene, _) = shadow_scene(l.clone());
            scene.cards[0].opacity = 0.3;
            assert!((lit(&scene) - 0.7).abs() < 1e-3, "{}: {}", l.node, lit(&scene));
            scene.cards[0].opacity = 1.0;
            scene.cards[0].image = Arc::new(Image::filled(4, 4, [0.4; 4]));
            assert!((lit(&scene) - 0.6).abs() < 1e-3, "{}: {}", l.node, lit(&scene));
            if l.node == "sun" {
                // A second 30% card higher up whose shadow lands there too
                // (and that the camera doesn't see the point through).
                scene.cards[0] = Card { opacity: 0.3, ..card(Image::filled(4, 4, [1.0; 4]), [0.0, 1.0, 0.0], [-90.0, 0.0, 0.0], [1.0, 1.0]) };
                scene.cards.push(Card { opacity: 0.3, ..card(Image::filled(4, 4, [1.0; 4]), [1.0, 2.0, 0.0], [-90.0, 0.0, 0.0], [1.0, 1.0]) });
                assert!((lit(&scene) - 0.49).abs() < 1e-3, "two: {}", lit(&scene));
            }
        }
    }

    #[test]
    fn soft_shadows_from_softness_radius_and_area_lights() {
        use serde_json::json;
        // Across the shadow's edge at x = −0.5, beside the card (a sun tilted
        // 20° from straight down, so the shadow reaches 0.36 past the card
        // toward −z, where the camera sees it): hard, it jumps; soft, it
        // ramps.
        let row = |l: Light| -> Vec<f32> {
            let (scene, cam) = shadow_scene(l);
            let out = render(&scene, &cam, SHADOW_FMT, 2, None);
            (0..=20).map(|i| floor_at(&out, &cam, -0.8 + i as f64 * 0.03, -0.71).0).collect()
        };
        let hard = row(light("light_directional", json!({ "rotate": [-70, 0, 0], "shadows": true, "shadow_softness": 0 }), "sun"));
        let soft = row(light("light_directional", json!({ "rotate": [-70, 0, 0], "shadows": true, "shadow_softness": 8 }), "sun"));
        let partial = |r: &[f32]| r.iter().filter(|v| **v > 0.05 * r[0] && **v < 0.95 * r[0]).count();
        assert!(hard[20] < 0.02 * hard[0] && soft[20] < 0.02 * soft[0], "in shadow at the end: {hard:?}\n{soft:?}");
        assert!(partial(&hard) <= 2 && partial(&soft) >= 3, "hard {hard:?}\nsoft {soft:?}");
        // A point light with a radius: the edge of the card's shadow (at
        // x = 0.75 for a light 2 above it) spreads by about the radius ×
        // (3 − 2) / 2 = 0.15 each way.
        let across = |radius: f64| -> Vec<f32> {
            let (scene, cam) = shadow_scene(light("light_point", json!({ "translate": [0, 3, 0], "radius": radius, "shadows": true, "shadow_softness": 0 }), "bulb"));
            let out = render(&scene, &cam, SHADOW_FMT, 2, None);
            (0..=20).map(|i| floor_at(&out, &cam, 0.57 + i as f64 * 0.018, 0.0).0).collect()
        };
        let (sharp, wide) = (across(0.0), across(0.3));
        let partial = |r: &[f32]| r.iter().filter(|v| **v > 0.05 * r[20] && **v < 0.95 * r[20]).count();
        assert!(partial(&sharp) <= 3 && partial(&wide) >= 8, "sharp {sharp:?}\nwide {wide:?}");
        // An area light 2 across, 2 above the floor: past the card, the
        // floor is darkest by it and lighter outward (each point sees more
        // of the light), in full light from 1.5 out.
        let panel = light("light_area", json!({ "translate": [0, 2, 0], "rotate": [-90, 0, 0], "size": [2, 2], "intensity": 4, "decay": 0, "shadows": true, "shadow_softness": 0 }), "panel");
        let (scene, cam) = shadow_scene(panel.clone());
        let out = render(&scene, &cam, SHADOW_FMT, 2, None);
        let lit = render(&Scene { lights: vec![Light { shadows: false, ..panel }], ..scene }, &cam, SHADOW_FMT, 2, None);
        let ratio = |x: f64| floor_at(&out, &cam, x, 0.0).0 / floor_at(&lit, &cam, x, 0.0).0;
        let (near, mid, far) = (ratio(0.62), ratio(1.0), ratio(2.0));
        assert!(near < mid && mid < far && (far - 1.0).abs() < 1e-3, "{near} {mid} {far}");
        assert!(mid > 0.2 && mid < 0.8, "a penumbra: {mid}");
    }

    #[test]
    fn lights_shade_meshes_in_parts_that_add_up() {
        use serde_json::json;
        // A shiny ball on a dull floor, a glowing box, an unlit card; an
        // ambient fill, a sun, a point light and a spot.
        let ball = model("sphere", json!({ "radius": 1, "translate": [-1, 1, -6], "shading": "blinn", "specular": 0.6, "roughness": 0.2, "color": [0.8, 0.3, 0.2, 1] }), "ball");
        let floor = model("plane", json!({ "size": [20, 20], "rotate": [-90, 0, 0], "color": [0.5, 0.5, 0.5, 1] }), "floor");
        let glow = model("cube", json!({ "translate": [1.5, 0.5, -6], "emission": [0.2, 0.4, 0.1], "color": [0.3, 0.3, 0.3, 0.8] }), "glow");
        let mut sign = card(checker(), [0.0, 3.0, -9.0], [0.0; 3], [4.0, 2.0]);
        sign.node = "sign".into();
        let lights = vec![
            light("light_ambient", json!({ "intensity": 0.1 }), "fill"),
            light("light_directional", json!({ "rotate": [-50, 30, 0], "intensity": 0.8 }), "sun"),
            light("light_point", json!({ "translate": [2, 3, -3], "color": [1, 0.8, 0.6, 1], "intensity": 4, "decay": 1 }), "bulb"),
            light("light_spot", json!({ "translate": [-1, 5, -6], "rotate": [-90, 0, 0], "cone": 40, "penumbra": 10, "intensity": 2 }), "spot"),
        ];
        let cam = Camera { focal: 18.0, world: geom::trs([0.0, 2.0, 0.0], [-12.0, 0.0, 0.0], [1.0; 3]), ..Camera::default() };
        let scene = Scene { cards: vec![sign], meshes: vec![ball, floor, glow], lights, ..Scene::default() };
        let out = render_passes(&scene, &cam, FMT, 3, None, Passes { id: true, ..LIGHTING });
        let ch = |n: &str, x: i32, y: i32| out.plane_at(n, x, y);
        let rgb = |layer: &str, x: i32, y: i32| [ch(&format!("{layer}.R"), x, y), ch(&format!("{layer}.G"), x, y), ch(&format!("{layer}.B"), x, y)];
        let (mut worst_parts, mut worst_lights) = (0.0f32, 0.0f32);
        for y in 0..32 {
            for x in 0..64 {
                let beauty = out.at(x, y);
                let parts: Vec<[f32; 3]> = ["diffuse", "specular", "emission", "ambient"].iter().map(|l| rgb(l, x, y)).collect();
                let lit: Vec<[f32; 3]> = ["fill", "sun", "bulb", "spot"].iter().map(|n| rgb(&format!("light.{n}"), x, y)).collect();
                for c in 0..3 {
                    worst_parts = worst_parts.max((parts.iter().map(|p| p[c]).sum::<f32>() - beauty[c]).abs());
                    worst_lights = worst_lights.max((lit.iter().map(|p| p[c]).sum::<f32>() + parts[2][c] - beauty[c]).abs());
                }
            }
        }
        assert!(worst_parts < 1e-5, "diffuse + specular + emission + ambient = beauty: {worst_parts}");
        assert!(worst_lights < 1e-5, "the lights' shares + emission = beauty: {worst_lights}");
        // Each part shows up somewhere: the highlight on the ball, the
        // box's glow, the card's colors (unlit: all emission), the fill.
        let any = |layer: &str, id: f32| (0..32).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| ch("id", x, y) == id).map(|(x, y)| rgb(layer, x, y)[0]).fold(0.0f32, f32::max);
        assert!(any("specular", 2.0) > 0.05, "a highlight on the ball");
        assert!(any("specular", 3.0) == 0.0, "none on the dull floor");
        assert!(any("emission", 4.0) > 0.1 && any("emission", 1.0) > 0.5 && any("diffuse", 1.0) == 0.0, "the glow; the card unlit");
        assert!(any("ambient", 3.0) > 0.0 && any("light.spot", 3.0) > 0.05 && any("light.bulb", 3.0) > 0.05, "the fill, the spot and the bulb reach the floor");
        // With no lights the same scene renders as it did unlit.
        let dark = Scene { lights: Vec::new(), ..scene.clone() };
        let unlit_meshes: Vec<crate::scene::Model> = dark.meshes.iter().map(|m| crate::scene::Model { material: Material { shading: Shading::Unlit, emission: [0.0; 3], ..m.material.clone() }, ..m.clone() }).collect();
        let (a, b) = (render(&Scene { meshes: unlit_meshes, ..dark.clone() }, &cam, FMT, 3, None), render(&Scene { meshes: dark.meshes.iter().map(|m| crate::scene::Model { material: Material { emission: [0.0; 3], ..m.material.clone() }, ..m.clone() }).collect(), ..dark }, &cam, FMT, 3, None));
        assert_eq!(a.pixels, b.pixels, "Lambert and Blinn with no lights: their color as it is");
    }

    #[test]
    fn lambert_follows_the_cosine_law_and_blinn_adds_a_highlight() {
        use serde_json::json;
        // A gray plane facing the camera, lit by a sun 60° off its normal:
        // half its color.
        let plane = model("plane", json!({ "size": [20, 10], "translate": [0, 0, -5], "color": [0.8, 0.6, 0.4, 1] }), "p");
        let sun = light("light_directional", json!({ "rotate": [0, 60, 0] }), "sun");
        let scene = Scene { meshes: vec![plane.clone()], lights: vec![sun.clone()], ..Scene::default() };
        let px = render(&scene, &camera(), FMT, 1, None).at(32, 16);
        assert!((px[0] - 0.4).abs() < 1e-5 && (px[1] - 0.3).abs() < 1e-5 && (px[2] - 0.2).abs() < 1e-5 && px[3] == 1.0, "{px:?}");
        // Lit from behind: black (the camera sees its unlit side).
        let behind = light("light_directional", json!({ "rotate": [0, 180, 0] }), "back");
        let px = render(&Scene { lights: vec![behind], ..scene.clone() }, &camera(), FMT, 1, None).at(32, 16);
        assert_eq!(px, [0.0, 0.0, 0.0, 1.0]);
        // Blinn, lit from the camera: a highlight in the middle, fading out.
        let shiny = crate::scene::Model { material: Material { shading: Shading::Blinn, specular: [1.0; 3], roughness: 0.2, ..plane.material.clone() }, ..plane };
        let head = light("light_directional", json!({}), "head");
        let out = render_passes(&Scene { meshes: vec![shiny], lights: vec![head], ..Scene::default() }, &camera(), FMT, 1, None, LIGHTING);
        let spec = |x: i32| out.plane_at("specular.R", x, 16);
        assert!(spec(32) > 0.9 && spec(32) > spec(48) && spec(48) > spec(62), "{} {} {}", spec(32), spec(48), spec(62));
        assert!((out.plane_at("diffuse.R", 32, 16) - 0.8).abs() < 1e-3);
    }

    /// Read Geo keeps its file's material (crate.mtl: Blinn) unless told
    /// to override it.
    #[test]
    fn read_geo_materials_follow_the_file_unless_overridden() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        let obj = format!("{}/examples/geo/crate.obj", env!("CARGO_MANIFEST_DIR"));
        let g: Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 1, "fps": 24, "format": { "width": 64, "height": 32 } },
            "nodes": [
                { "id": "cam", "op": "camera", "params": { "focal": 18 } },
                { "id": "sun", "op": "light_directional", "params": { "rotate": [0, 60, 0] } },
                { "id": "file", "op": "read_geo", "params": { "file": obj, "translate": [0, 0, -5] } },
                { "id": "flat", "op": "read_geo", "params": { "file": obj, "translate": [0, 0, -5], "override_material": true, "shading": "unlit" } },
                { "id": "s1", "op": "scene", "inputs": ["file", "sun", "", ""] },
                { "id": "s2", "op": "scene", "inputs": ["flat", "sun", "", ""] },
                { "id": "r1", "op": "scanline_render", "inputs": ["s1", "cam", ""], "params": { "samples": 1 } },
                { "id": "r2", "op": "scanline_render", "inputs": ["s2", "cam", ""], "params": { "samples": 1 } },
            ] })).unwrap();
        let mut ev = Evaluator::new();
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let s = ev.evaluate(&g, "file", &ctx).unwrap();
        assert_eq!(s.scene.as_ref().unwrap().meshes[0].material.shading, Shading::Blinn, "illum 2");
        let (lit, flat) = (ev.evaluate(&g, "r1", &ctx).unwrap().at(32, 16), ev.evaluate(&g, "r2", &ctx).unwrap().at(32, 16));
        // The front face lit at 60°: half its color, plus the same gray
        // highlight (Ks 0.1) on each channel.
        let spec = lit[0] - 0.3;
        assert!(spec > 0.0 && spec < 0.02 && (lit[1] - 0.2 - spec).abs() < 1e-5 && (lit[2] - 0.1 - spec).abs() < 1e-5, "{lit:?}");
        assert!((flat[0] - 0.6).abs() < 1e-6 && (flat[1] - 0.4).abs() < 1e-6, "overridden to unlit: its color ({flat:?})");
    }

    /// Motion vectors through the graph: the scene and camera are taken a
    /// frame later too.
    #[test]
    fn motion_vectors_follow_the_camera_and_objects() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        let keys = |a: [f64; 3], b: [f64; 3]| json!({ "keys": [[1, a], [10, b]], "interp": "linear" });
        let graph = |cam: serde_json::Value, card: serde_json::Value| -> Graph {
            serde_json::from_value(json!({
                "project": { "first_frame": 1, "last_frame": 10, "fps": 24, "format": { "width": 64, "height": 32 } },
                "nodes": [
                    { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 32, "size": 4 } },
                    { "id": "cam", "op": "camera", "params": cam },
                    { "id": "card1", "op": "card", "inputs": ["plate"], "params": card },
                    { "id": "floor", "op": "plane", "params": { "size": [40, 40], "translate": [0, -2, -10], "rotate": [-90, 0, 0] } },
                    { "id": "all", "op": "scene", "inputs": ["card1", "floor", "", ""] },
                    { "id": "r", "op": "scanline_render", "inputs": ["all", "cam", ""], "params": { "samples": 1, "passes": ["depth", "P", "motion"] } },
                ] })).unwrap()
        };
        let ctx = |frame: i64| EvalContext { frame, write_outputs: false };
        let card = json!({ "translate": [0, 0, -5], "size": [6, 3] });
        // A camera moving right and up, and turning: each pixel's motion is
        // where its point lands through the next frame's camera, less where
        // it is.
        let moving = json!({ "focal": 18, "translate": keys([0.0, 0.0, 0.0], [1.8, 0.9, -0.9]), "rotate": keys([0.0, 0.0, 0.0], [-9.0, 18.0, 0.0]) });
        let g = graph(moving, card.clone());
        let mut ev = Evaluator::new();
        let out = ev.evaluate(&g, "r", &ctx(3)).unwrap();
        let next_cam = ev.evaluate(&g, "cam", &ctx(4)).unwrap().scene.as_ref().unwrap().cameras[0].clone();
        let mut covered = 0;
        for y in 0..32 {
            for x in 0..64 {
                if out.plane_at("depth", x, y) == 0.0 {
                    assert_eq!((out.plane_at("motion.u", x, y), out.plane_at("motion.v", x, y)), (0.0, 0.0), "nothing there");
                    continue;
                }
                covered += 1;
                let p = Vec3::new(out.plane_at("P.x", x, y) as f64, out.plane_at("P.y", x, y) as f64, out.plane_at("P.z", x, y) as f64);
                let (to, _) = next_cam.project(&p, FMT).unwrap();
                let want = [to[0] - (x as f64 + 0.5), to[1] - (y as f64 + 0.5)];
                let got = [out.plane_at("motion.u", x, y) as f64, out.plane_at("motion.v", x, y) as f64];
                assert!((got[0] - want[0]).abs() < 2e-3 && (got[1] - want[1]).abs() < 2e-3, "({x}, {y}): {got:?} vs {want:?}");
            }
        }
        let most = out.plane("motion.u").unwrap().data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(covered > 1000 && most > 1.0, "it moves: {covered} px, up to {most}");
        // The card sliding right 10 px a frame (1.5625 units at 5 away, 6.4
        // px a unit) past a still camera: (10, 0) on it; the floor still.
        let still = json!({ "focal": 18 });
        let sliding = json!({ "translate": keys([-7.0, 0.0, -5.0], [7.0625, 0.0, -5.0]), "size": [6, 3] });
        let g = graph(still.clone(), sliding);
        let out = Evaluator::new().evaluate(&g, "r", &ctx(5)).unwrap();
        let card_at = (32.0 + (-7.0 + 4.0 * 1.5625) * 6.4) as i32;
        assert!((out.plane_at("motion.u", card_at, 16) - 10.0).abs() < 1e-3 && out.plane_at("motion.v", card_at, 16).abs() < 1e-3, "{}", out.plane_at("motion.u", card_at, 16));
        assert!(out.plane_at("motion.u", 60, 31).abs() < 1e-6 && out.plane_at("depth", 60, 31) > 0.0, "the floor doesn't move");
        // All still: 0 everywhere.
        let g = graph(still, card);
        let out = Evaluator::new().evaluate(&g, "r", &ctx(5)).unwrap();
        assert!(out.plane("motion.u").unwrap().data.iter().chain(&out.plane("motion.v").unwrap().data).all(|v| *v == 0.0));
        // A change only at the next frame changes the render's key (and so
        // its cached result).
        let with = |to: f64| {
            let mut g = g.clone();
            g.nodes.iter_mut().find(|n| n.id == "card1").unwrap().params["translate"] = json!({ "keys": [[5, [0, 0, -5]], [6, [to, 0, -5]]], "interp": "linear" });
            g
        };
        let (held, later) = (with(0.0), with(1.0));
        assert_eq!(Evaluator::key(&held, "card1", &ctx(5)).unwrap(), Evaluator::key(&later, "card1", &ctx(5)).unwrap(), "the card is the same at frame 5");
        assert_ne!(Evaluator::key(&held, "r", &ctx(5)).unwrap(), Evaluator::key(&later, "r", &ctx(5)).unwrap(), "but not at 6");
        let out = Evaluator::new().evaluate(&later, "r", &ctx(5)).unwrap();
        assert!(out.plane_at("motion.u", 32, 16) > 1.0, "so it moves");
    }

    #[test]
    fn motion_blur_averages_renders_across_the_shutter() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        // A white card 3 across (19.2 px) sliding right 10 px a frame (as in
        // the motion test) past a still camera, centered at x 27.2 at frame 5.
        let graph = |card: serde_json::Value, render: serde_json::Value| -> Graph {
            serde_json::from_value(json!({
                "project": { "first_frame": 1, "last_frame": 10, "fps": 24, "format": { "width": 64, "height": 32 } },
                "nodes": [
                    { "id": "plate", "op": "constant", "params": { "width": 16, "height": 16 } },
                    { "id": "cam", "op": "camera", "params": { "focal": 18 } },
                    { "id": "card1", "op": "card", "inputs": ["plate"], "params": card },
                    { "id": "r", "op": "scanline_render", "inputs": ["card1", "cam", ""], "params": render },
                ] })).unwrap()
        };
        let sliding = json!({ "translate": { "keys": [[1, [-7.0, 0, -5]], [10, [7.0625, 0, -5]]], "interp": "linear" }, "size": [3, 3] });
        let at5 = |card: &serde_json::Value, render: serde_json::Value| Evaluator::new().evaluate(&graph(card.clone(), render), "r", &EvalContext { frame: 5, write_outputs: false }).unwrap();
        // Off (missing, 0 or 1): the render as it was.
        let sharp = at5(&sliding, json!({ "samples": 4 }));
        for mb in [0, 1] {
            let off = at5(&sliding, json!({ "samples": 4, "motion_blur": mb, "shutter": 1.0 }));
            assert!(off.pixels == sharp.pixels && off.planes.iter().zip(&sharp.planes).all(|(a, b)| a.name == b.name && a.data == b.data), "motion_blur {mb}");
        }
        // On, a half-frame shutter: each edge smears over 5 px (the card
        // from 2.5 px left to 2.5 px right of where it is), the row's total
        // the same, and the depth pass the frame's own.
        let blurred = at5(&sliding, json!({ "samples": 4, "motion_blur": 16, "shutter": 0.5 }));
        let row = |img: &Image| (0..64).map(|x| img.at(x, 16)[3]).collect::<Vec<f32>>();
        let (a, b) = (row(&sharp), row(&blurred));
        for (x, got) in b.iter().enumerate() {
            let left = ((x as f32 + 0.5 - (17.6 - 2.5)) / 5.0).clamp(0.0, 1.0);
            let right = (((36.8 + 2.5) - (x as f32 + 0.5)) / 5.0).clamp(0.0, 1.0);
            assert!((got - left.min(right)).abs() < 0.06, "x {x}: {got} vs {}", left.min(right));
        }
        let soft = b.iter().filter(|&&v| v > 0.02 && v < 0.98).count();
        assert!((9..=12).contains(&soft), "{soft} px partly covered (two 5 px edges)");
        assert!((a.iter().sum::<f32>() - b.iter().sum::<f32>()).abs() < 0.1, "{} vs {}", a.iter().sum::<f32>(), b.iter().sum::<f32>());
        assert_eq!(blurred.plane("depth").unwrap().data, sharp.plane("depth").unwrap().data);
        // Starting at the frame: all of the smear to the right of it.
        let late = row(&at5(&sliding, json!({ "samples": 4, "motion_blur": 16, "shutter": 0.5, "shutter_offset": "start" })));
        assert!(late[16] < 0.02 && late[38] > 0.3 && b[16] > 0.2, "{} {} {}", late[16], late[38], b[16]);
        // A still scene with blur on renders as without.
        let still = json!({ "translate": [0, 0, -5], "size": [3, 3] });
        let (s0, s1) = (at5(&still, json!({ "samples": 4 })), at5(&still, json!({ "samples": 4, "motion_blur": 8 })));
        assert!(s0.pixels.iter().zip(&s1.pixels).all(|(p, q)| (0..4).all(|c| (p[c] - q[c]).abs() < 1e-6)));
        // Jitter shutter: on unless turned off (a render from before it
        // renders as it did); off, not the same picture.
        let jitter = |on: Option<bool>| {
            let mut p = json!({ "samples": 2, "motion_blur": 2, "shutter": 1.0 });
            if let Some(on) = on {
                p["jitter"] = json!(on);
            }
            at5(&sliding, p).pixels.clone()
        };
        assert!(jitter(None) == jitter(Some(true)) && jitter(None) != jitter(Some(false)));
    }

    /// Jitter shutter off: every pixel at the shutter's times themselves,
    /// the picture the mean of whole renders at them, the passes that
    /// don't blur the frame's own: exactly the render of 0.9.48–0.9.54
    /// (before jitter), done here as it was then. The 3D view's blurred
    /// preview too: the mean of previews at the times.
    #[test]
    fn unjittered_motion_blur_renders_whole_frames_at_the_times() {
        let white = Image::filled(4, 4, [1.0; 4]);
        let at = |x: f64| Scene { cards: vec![card(white.clone(), [x, 0.0, -5.0], [0.0; 3], [1.25, 2.5])], ..Scene::default() };
        let (a, b) = (at(-3.5), at(2.75));
        let cam = camera();
        let frames = [(0, &a), (1, &b)];
        let passes = Passes { depth: true, emission: true, ..Passes::NONE };
        for n in [2, 3, 8] {
            let blur = Blur { times: Blur::times(0.0, 1.0, n), frames: vec![(0, &a, cam.clone()), (1, &b, cam.clone())], jitter: false };
            let got = render_blurred(&a, &cam, FMT, 2, None, passes, None, Some(&blur));
            // As 0.9.54 rendered it.
            let mut want = render_motion(&a, &cam, FMT, 2, None, passes, None);
            let mixed = Passes { depth: false, ..passes };
            let mut sum: Option<Image> = None;
            for &t in &blur.times {
                let img = render_motion(&a.at_time(&frames, t), &blur.camera(&cam, t), FMT, 2, None, mixed, None);
                match &mut sum {
                    None => sum = Some(img),
                    Some(acc) => {
                        acc.pixels.iter_mut().zip(&img.pixels).for_each(|(p, q)| (0..4).for_each(|c| p[c] += q[c]));
                        for p in &mut acc.planes {
                            p.data.iter_mut().zip(&img.plane(&p.name).unwrap().data).for_each(|(p, q)| *p += q);
                        }
                    }
                }
            }
            let (sum, k) = (sum.unwrap(), 1.0 / n as f32);
            want.pixels = sum.pixels.iter().map(|p| p.map(|v| v * k)).collect();
            for p in sum.planes {
                *want.plane_mut(&p.name) = p.data.iter().map(|v| v * k).collect();
            }
            assert!(got.pixels == want.pixels, "{n} times");
            assert!(got.planes.len() == want.planes.len() && got.planes.iter().all(|p| want.plane(&p.name).is_some_and(|q| q.data == p.data)), "{n} times");
            // Jittered, it isn't that.
            let jittered = render_blurred(&a, &cam, FMT, 2, None, passes, None, Some(&Blur { jitter: true, ..blur }));
            assert!(jittered.pixels != want.pixels, "{n} times");

            let blur = Blur { times: Blur::times(0.0, 1.0, n), frames: vec![(0, &a, cam.clone()), (1, &b, cam.clone())], jitter: false };
            let got = render_preview_blurred(&a, &cam, FMT, Preview::Solid, Some(&blur));
            let previews: Vec<Image> = blur.times.iter().map(|&t| render_preview(&a.at_time(&frames, t), &cam, FMT, Preview::Solid)).collect();
            let want: Vec<Rgba> = (0..got.pixels.len()).map(|i| {
                let mut s = [0.0f32; 4];
                previews.iter().for_each(|p| (0..4).for_each(|c| s[c] += p.pixels[i][c]));
                s.map(|v| v * k)
            }).collect();
            assert!(got.pixels == want, "preview, {n} times");
        }
    }

    /// Two times across a frame of a card moving 40 px a frame: without
    /// jitter, two sharp copies 20 px apart; with it, grain along the whole
    /// streak, nearer the smear many times give. The same every time.
    #[test]
    fn jittered_times_turn_copies_into_grain() {
        let white = Image::filled(4, 4, [1.0; 4]);
        let at = |x: f64| Scene { cards: vec![card(white.clone(), [x, 0.0, -5.0], [0.0; 3], [1.25, 2.5])], ..Scene::default() };
        let (a, b) = (at(-3.5), at(2.75));
        let cam = camera();
        let blur = |n: usize| Blur { times: Blur::times(0.0, 1.0, n), frames: vec![(0, &a, cam.clone()), (1, &b, cam.clone())], jitter: true };
        let render = |n: usize| render_blurred(&a, &cam, FMT, 2, None, Passes::NONE, None, Some(&blur(n)));
        let row = |img: &Image| (0..64).map(|x| img.at(x, 16)[3]).collect::<Vec<f32>>();
        let (smear_img, jittered_img) = (render(64), render(2));
        let (smear, jittered) = (row(&smear_img), row(&jittered_img));
        assert_eq!(jittered, row(&render(2)), "repeatable");
        let frames = [(0, &a), (1, &b)];
        let copies: Vec<Image> = Blur::times(0.0, 1.0, 2).iter().map(|&t| render_motion(&a.at_time(&frames, t), &cam, FMT, 2, None, Passes::NONE, None)).collect();
        let copies: Vec<f32> = row(&copies[0]).iter().zip(row(&copies[1])).map(|(p, q)| (p + q) / 2.0).collect();
        // Seen from a step back (over the 4 px tile), nearer the smear.
        let soft = |r: &[f32]| (0..61).map(|x| r[x..x + 4].iter().sum::<f32>() / 4.0).collect::<Vec<f32>>();
        let off = |r: &[f32]| soft(r).iter().zip(soft(&smear)).map(|(p, q)| (p - q).abs()).sum::<f32>();
        assert!(off(&jittered) < 0.6 * off(&copies), "{} vs {} without jitter\n{jittered:?}\n{copies:?}", off(&jittered), off(&copies));
        // The gap between the copies is no longer empty, and the picture's
        // total stays.
        assert!(copies[29] == 0.0 && jittered[27..33].iter().any(|v| *v > 0.0), "{jittered:?}");
        let total = |img: &Image| img.pixels.iter().map(|p| p[3]).sum::<f32>();
        assert!((total(&jittered_img) - total(&smear_img)).abs() < 0.02 * total(&smear_img), "{} vs {}", total(&jittered_img), total(&smear_img));
        // A bigger tile for fewer times; none for many.
        assert_eq!([jitter_side(2), jitter_side(8), jitter_side(32)], [4, 2, 1]);
        let ranks: Vec<usize> = (0..16).map(|i| dither_rank(4, i % 4, i / 4)).collect();
        assert_eq!(ranks, [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5]);
    }

    #[test]
    fn pass_names_parse() {
        let every = Passes { diffuse: true, specular: true, emission: true, ambient: true, lights: true, shadow: true, motion: true, ..ALL };
        assert_eq!(Passes::from_names(Passes::NAMES), every);
        assert_eq!(Passes::from_names(["depth", "nonsense"]), Passes::DEPTH);
        assert_eq!(Passes::from_names([]), Passes::default());
    }

    /// Through the graph: a plate on a card, an animated camera, the
    /// render in the project's format; no camera is an error.
    #[test]
    fn the_scanline_render_node_follows_the_camera() {
        use crate::graph::{Evaluator, Graph};
        use crate::ops::EvalContext;
        use serde_json::json;
        let g: Graph = serde_json::from_value(json!({
            "project": { "first_frame": 1, "last_frame": 10, "fps": 24, "format": { "width": 64, "height": 32 } },
            "nodes": [
                { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 32, "size": 4 } },
                { "id": "card1", "op": "card", "inputs": ["plate"], "params": { "translate": [0, 0, -5], "scale": [10, 10, 1] } },
                { "id": "cam", "op": "camera", "params": { "focal": 18,
                    "translate": { "keys": [[1, [0, 0, 0]], [10, [5, 0, 0]]], "interp": "linear" } } },
                { "id": "all", "op": "scene", "inputs": ["card1", "cam", "", ""] },
                { "id": "render", "op": "scanline_render", "inputs": ["all", "", ""], "params": { "samples": 1 } },
                { "id": "blind", "op": "scanline_render", "inputs": ["card1", "", ""] },
            ] })).unwrap();
        let mut ev = Evaluator::new();
        let at = |ev: &mut Evaluator, node: &str, frame: i64| ev.evaluate(&g, node, &EvalContext { frame, write_outputs: false });
        let first = at(&mut ev, "render", 1).unwrap();
        let plate = at(&mut ev, "plate", 1).unwrap();
        assert_eq!(first.format, Size { width: 64, height: 32 }, "the project's format");
        let worst = first.pixels.iter().zip(&plate.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        assert!(worst < 1e-4, "frame 1 sees the plate square on: {worst}");
        // By frame 10 the camera has moved 5 right: the card's left half
        // shows, on the left of the frame.
        let last = at(&mut ev, "render", 10).unwrap();
        assert!(last.at(10, 16)[3] == 1.0 && last.at(50, 16)[3] == 0.0);
        let e = at(&mut ev, "blind", 1).err().unwrap();
        assert!(e.message.contains("no camera"), "{}", e.message);
    }

    #[test]
    fn renders_over_a_background_and_draws_points() {
        let mut bg = Image::filled(64, 32, [0.0, 0.5, 0.0, 1.0]);
        *bg.plane_mut("depth") = vec![50.0; 64 * 32];
        let red = card(Image::filled(4, 4, [1.0, 0.0, 0.0, 1.0]), [0.0, 0.0, -3.0], [0.0; 3], [1.0, 1.0]);
        let scene = Scene { cards: vec![red], points: vec![Vec3::new(-4.0, 0.0, -5.0)], ..Scene::default() };
        let out = render(&scene, &camera(), FMT, 2, Some(&bg));
        assert_eq!((out.at(32, 16), out.at(2, 2)), ([1.0, 0.0, 0.0, 1.0], [0.0, 0.5, 0.0, 1.0]));
        assert_eq!((out.plane_at("depth", 2, 2), out.plane_at("depth", 32, 16)), (50.0, 3.0));
        // The point 4 left at 5 away: 32 − 32 × 4 / 5 = 6.4 px across.
        assert_eq!(out.at(6, 16), [1.0; 4]);
    }

    /// An orthographic camera sees a card the same size at any distance,
    /// its texture mapped without perspective even when the card slants,
    /// its depth and P passes back on the surface; a Camera node set to
    /// Orthographic gives one.
    #[test]
    fn orthographic_cameras_see_without_perspective() {
        let ortho = Camera { ortho: 10.0, near: 0.1, ..Camera::default() };
        let img = checker();
        let worst = |out: &Image| out.pixels.iter().zip(&img.pixels).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0, f32::max);
        // 10 across fills the frame's 64 pixels, 5 away or 500.
        for z in [-5.0, -500.0] {
            let scene = Scene { cards: vec![card(img.clone(), [0.0, 0.0, z], [0.0; 3], [10.0, 5.0])], ..Scene::default() };
            let out = render(&scene, &ortho, FMT, 1, None);
            assert!(worst(&out) < 1e-4, "at {z}: {}", worst(&out));
            assert!((out.plane_at("depth", 10, 10) as f64 + z).abs() < 1e-3);
        }
        // Turned 60° about Y and twice as wide: it still spans the frame,
        // texel for texel (a pinhole would bunch up the far side).
        let slanted = Scene { cards: vec![card(img.clone(), [0.0, 0.0, -20.0], [0.0, 60.0, 0.0], [20.0, 5.0])], ..Scene::default() };
        let out = render_passes(&slanted, &ortho, FMT, 1, None, ALL);
        assert!(worst(&out) < 1e-3, "{}", worst(&out));
        let normal = geom::rotation([0.0, 60.0, 0.0]) * Vec3::z();
        for (x, y) in [(3, 4), (32, 16), (60, 28)] {
            let at = |n: &str| out.plane_at(n, x, y) as f64;
            let p = Vec3::new(at("P.x"), at("P.y"), at("P.z"));
            assert!((p - ortho.unproject([x as f64 + 0.5, y as f64 + 0.5], at("depth"), FMT)).norm() < 1e-4, "({x}, {y})");
            assert!((p - Vec3::new(0.0, 0.0, -20.0)).dot(&normal).abs() < 1e-4, "on the card");
        }
        // Lit, its view is the same everywhere: straight back along the axis.
        assert_eq!(ortho.to_eye(&Vec3::new(3.0, -2.0, -9.0)), Vec3::z());
        let cam = crate::scene::camera(&serde_json::json!({ "projection": "orthographic", "ortho_width": 7.5 }));
        assert_eq!((cam.ortho, crate::scene::camera(&serde_json::json!({ "ortho_width": 7.5 })).ortho), (7.5, 0.0));
    }

    /// A card seen eight times smaller than its texture takes the
    /// texture's mean over each pixel (from its mipmaps), not whichever
    /// texel falls under it; seen 1:1 it's exactly its image still, and
    /// the 3D view's preview does the same.
    #[test]
    fn minified_cards_average_their_texture() {
        // Texel noise: no pattern a pixel's samples could happen to average.
        let mut img = checker();
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let v = ((i as u32).wrapping_mul(2654435761) >> 24) as f32 / 255.0;
            *p = [v, 1.0 - v, 0.5 * v, 1.0];
        }
        let mean: Rgba = std::array::from_fn(|c| img.pixels.iter().map(|p| p[c]).sum::<f32>() / img.pixels.len() as f32);
        // 40 away the camera sees 80 across: the 10-wide card is 8 pixels.
        let far = Scene { cards: vec![card(img.clone(), [0.0, 0.0, -40.0], [0.0; 3], [10.0, 5.0])], ..Scene::default() };
        let off = |out: &Image| {
            // The card's inside: pixels 29..35 across, 15..17 down.
            (29..35).flat_map(|x| (15..17).map(move |y| (x, y))).map(|(x, y)| (0..3).map(|c| (out.at(x, y)[c] - mean[c]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max)
        };
        for samples in [1, 3] {
            let out = render(&far, &camera(), FMT, samples, None);
            assert!(off(&out) < 0.03, "{samples} samples: {} off the mean", off(&out));
        }
        assert!(off(&render_preview(&far, &camera(), FMT, Preview::Solid)) < 0.03);
        // A wrapped texture repeating eight times over a card's width: as
        // even, seams and all.
        let tiles: Vec<[f64; 2]> = vec![[0.0, 0.0], [8.0, 0.0], [8.0, 4.0], [0.0, 4.0]];
        let tex = Arc::new(img.clone());
        let plane = Surface {
            world: geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]),
            positions: Cow::Owned(vec![Vec3::new(-5.0, 2.5, 0.0), Vec3::new(5.0, 2.5, 0.0), Vec3::new(5.0, -2.5, 0.0), Vec3::new(-5.0, -2.5, 0.0)]),
            normals: Cow::Owned(Vec::new()),
            uvs: tiles.into(),
            triangles: Cow::Owned(vec![[0, 1, 2], [0, 2, 3]]),
            color: [1.0; 4],
            texture: Some(&tex),
            wrap: true,
            v_up: false,
            headlight: false,
            holdout: false,
            node: "",
            material: None,
            casts: true,
            receives: true,
            two_sided: false,
        };
        let out = draw(&[plane], &[], &camera(), FMT, 1);
        let worst = out.pixels.iter().map(|p| (0..3).map(|c| (p[c] - mean[c]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
        assert!(worst < 0.03, "{worst}");
    }
}
