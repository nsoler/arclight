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
//! sample's distance in front of the camera, 0 where nothing is. Points
//! draw as one-pixel dots.

use crate::color::{self, Rgba};
use crate::geom::{self, Camera, Mat4, Vec3};
use crate::image::{IRect, Image, Size};
use crate::scene::Scene;
use rayon::prelude::*;
use std::borrow::Cow;

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
    pub texture: Option<&'a Image>,
    /// Texture coordinates past 0–1 repeat the texture (else they clamp to
    /// its edge, as a card's do).
    pub wrap: bool,
    /// Shade by a light at the camera (the 3D viewport's solid view).
    pub headlight: bool,
}

/// Card `card` as two triangles (top left, top right, bottom right; top
/// left, bottom right, bottom left).
fn card_surface(card: &crate::scene::Card) -> Surface<'_> {
    let [w, h] = card.size.map(|v| v / 2.0);
    Surface {
        world: card.world,
        positions: Cow::Owned(vec![Vec3::new(-w, h, 0.0), Vec3::new(w, h, 0.0), Vec3::new(w, -h, 0.0), Vec3::new(-w, -h, 0.0)]),
        normals: Cow::Owned(vec![Vec3::z(); 4]),
        uvs: Cow::Owned(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        triangles: Cow::Owned(vec![[0, 1, 2], [0, 2, 3]]),
        color: [1.0; 4],
        texture: Some(&card.image),
        wrap: false,
        v_up: false,
        headlight: false,
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
        texture: m.texture.as_deref(),
        wrap: true,
        v_up: true,
        headlight: false,
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
}

impl Vert {
    fn lerp(&self, o: &Vert, t: f64) -> Vert {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Vert { p: self.p + (o.p - self.p) * t, world: self.world + (o.world - self.world) * t, uv: [l(self.uv[0], o.uv[0]), l(self.uv[1], o.uv[1])], n: self.n + (o.n - self.n) * t }
    }
}

/// A projected triangle of surface `surf`: frame positions and 1 / depth
/// at each corner, its corners, and the pixel rows and columns it can
/// touch.
struct Tri {
    surf: usize,
    p: [[f64; 2]; 3],
    v: [Vert; 3],
    inv_depth: [f64; 3],
    area: f64,
    bounds: IRect,
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
fn triangles(surfs: &[Surface], cam: &Camera, format: Size) -> Vec<Tri> {
    let view = cam.view();
    let f = cam.focal_pixels(format);
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
            };
            let tri = [corner(a), corner(b), corner(c)];
            let poly = if tri.iter().all(|v| -v.p.z >= near) { tri.to_vec() } else { clip_near(&tri, near) };
            if poly.len() < 3 {
                continue;
            }
            let project = |v: &Vert| {
                let d = -v.p.z;
                ([cx + f * v.p.x / d, cy - f * v.p.y / d], 1.0 / d)
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
                tris.push(Tri { surf: si, p, v: idx.map(|i| poly[i]), inv_depth: idx.map(|i| projected[i].1), area, bounds });
            }
        }
    }
    tris
}

/// One surface seen at one sample: how far, through which triangle, and
/// its color there (premultiplied).
#[derive(Clone, Copy)]
struct Hit {
    depth: f64,
    tri: usize,
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
        let inv = b0 * self.inv_depth[0] + b1 * self.inv_depth[1] + b2 * self.inv_depth[2];
        let d = 1.0 / inv;
        if !(d >= near && d <= far) {
            return None;
        }
        Some((d, [b0 * self.inv_depth[0] / inv, b1 * self.inv_depth[1] / inv, b2 * self.inv_depth[2] / inv]))
    }

    /// Texture coordinates at barycentric `b`.
    fn uv(&self, b: &[f64; 3]) -> [f64; 2] {
        std::array::from_fn(|k| b[0] * self.v[0].uv[k] + b[1] * self.v[1].uv[k] + b[2] * self.v[2].uv[k])
    }
}

/// Surface `s`'s color at texture coordinates `uv`.
fn shade(s: &Surface, uv: [f64; 2]) -> Rgba {
    let Some(img) = s.texture else { return s.color };
    let (fw, fh) = (img.format.width as f64, img.format.height as f64);
    if s.wrap {
        let px = sample_wrapped(img, uv[0] * fw - 0.5, uv[1] * fh - 0.5);
        return if s.color == [1.0; 4] { px } else { std::array::from_fn(|c| px[c] * s.color[c]) };
    }
    let (tx, ty) = {
        // Clamped to the image's frame, so a card's edge is sharp (the
        // samples soften it) rather than fading over half a texel.
        ((uv[0] * fw - 0.5).clamp(0.0, (fw - 1.0).max(0.0)), (uv[1] * fh - 0.5).clamp(0.0, (fh - 1.0).max(0.0)))
    };
    let px = img.sample_bilinear(tx as f32, ty as f32);
    if s.color == [1.0; 4] { px } else { std::array::from_fn(|c| px[c] * s.color[c]) }
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
    let out = draw(&surfaces(scene), &scene.points, cam, format, samples);
    match background {
        Some(bg) => over_background(out, bg),
        None => out,
    }
}

/// `scene`'s cards and meshes through `cam`, one sample a pixel, the meshes
/// lit from the camera (so their shape shows without lights): what the 3D
/// viewport shows under its outlines.
pub fn render_preview(scene: &Scene, cam: &Camera, format: Size) -> Image {
    let mut surfs = surfaces(scene);
    for s in surfs.iter_mut().skip(scene.cards.len()) {
        s.headlight = true;
    }
    draw(&surfs, &[], cam, format, 1)
}

/// Surfaces `surfs` and dots at `points` through `cam` into a frame of
/// `format` (see [`render`]).
fn draw(surfs: &[Surface], points: &[Vec3], cam: &Camera, format: Size, samples: usize) -> Image {
    let n = samples.clamp(1, 16);
    let tris = triangles(surfs, cam, format);
    let rows = bin_rows(&tris, format.height);
    let eye = geom::transform_point(&cam.world, &Vec3::zeros());
    let frame = IRect::frame(format);
    let (w, h) = (format.width, format.height);
    let mut color = vec![color::TRANSPARENT; w * h];
    let mut depth = vec![0.0f32; w * h];
    color.par_chunks_mut(w.max(1)).zip(depth.par_chunks_mut(w.max(1))).enumerate().for_each(|(y, (crow, drow))| {
        let row = &rows[y];
        if row.is_empty() {
            return;
        }
        let y = y as i32;
        let mut hits: Vec<Hit> = Vec::new();
        // The triangles under the current pixel: those whose columns
        // reach it, added as the sweep passes their left edge.
        let (mut next, mut here): (usize, Vec<u32>) = (0, Vec::new());
        for (x, (out, dout)) in crow.iter_mut().zip(drow.iter_mut()).enumerate() {
            let x = x as i32;
            while next < row.len() && tris[row[next] as usize].bounds.x0 <= x {
                here.push(row[next]);
                next += 1;
            }
            here.retain(|&i| tris[i as usize].bounds.x1 > x);
            if here.is_empty() {
                continue;
            }
            let mut sum = [0.0f32; 4];
            let mut nearest = f64::INFINITY;
            for j in 0..n {
                for i in 0..n {
                    let q = [x as f64 + (i as f64 + 0.5) / n as f64, y as f64 + (j as f64 + 0.5) / n as f64];
                    hits.clear();
                    for &ti in &here {
                        let t = &tris[ti as usize];
                        let Some((d, bary)) = t.cover(q, cam.near, cam.far) else { continue };
                        // One hit where two triangles of a surface meet.
                        if hits.iter().any(|h| tris[h.tri].surf == t.surf && (h.depth - d).abs() <= 1e-9 * d) {
                            continue;
                        }
                        let s = &surfs[t.surf];
                        let mut color = shade(s, t.uv(&bary));
                        if s.headlight {
                            let at = |f: fn(&Vert) -> Vec3| t.v.iter().zip(&bary).map(|(v, b)| f(v) * *b).sum::<Vec3>();
                            let (n, p) = (at(|v| v.n), at(|v| v.world));
                            let facing = n.try_normalize(1e-300).zip((eye - p).try_normalize(1e-300)).map_or(1.0, |(n, v)| n.dot(&v).abs());
                            let k = (0.25 + 0.75 * facing) as f32;
                            (0..3).for_each(|c| color[c] *= k);
                        }
                        hits.push(Hit { depth: d, tri: ti as usize, color });
                    }
                    if hits.is_empty() {
                        continue;
                    }
                    hits.sort_by(|a, b| a.depth.total_cmp(&b.depth).then(a.tri.cmp(&b.tri)));
                    // Front to back, premultiplied.
                    let mut acc = [0.0f32; 4];
                    for hit in &hits {
                        if hit.color[3] > 0.0 {
                            nearest = nearest.min(hit.depth);
                        }
                        let k = 1.0 - acc[3];
                        (0..4).for_each(|c| acc[c] += hit.color[c] * k);
                        if acc[3] >= 1.0 {
                            break;
                        }
                    }
                    (0..4).for_each(|c| sum[c] += acc[c]);
                }
            }
            let norm = 1.0 / (n * n) as f32;
            *out = sum.map(|v| v * norm);
            if nearest.is_finite() {
                *dout = nearest as f32;
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
    *out.plane_mut("depth") = depth;
    out
}

/// `fg` (frame-sized, with depth) over `bg`: the union of both windows;
/// where the render shows nothing, the background's depth (if it has one).
fn over_background(fg: Image, bg: &Image) -> Image {
    let window = fg.window().union(&bg.window());
    let mut out = Image::blank(fg.format, window);
    let ww = window.width().max(1);
    let fg_depth = fg.plane("depth").expect("rendered with depth");
    let bg_depth = bg.plane("depth");
    out.pixels.par_iter_mut().enumerate().for_each(|(i, px)| {
        let (x, y) = (window.x0 + (i % ww) as i32, window.y0 + (i / ww) as i32);
        let (f, b) = (fg.at(x, y), bg.at(x, y));
        *px = std::array::from_fn(|c| f[c] + b[c] * (1.0 - f[3]));
    });
    let mut d = vec![0.0f32; out.width * out.height];
    d.par_iter_mut().enumerate().for_each(|(i, v)| {
        let (x, y) = (window.x0 + (i % ww) as i32, window.y0 + (i / ww) as i32);
        let near = fg.plane_value(fg_depth, x, y);
        *v = if near != 0.0 { near } else { bg_depth.map_or(0.0, |p| bg.plane_value(p, x, y)) };
    });
    *out.plane_mut("depth") = d;
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
        Card { image: Arc::new(img), world: geom::trs(translate, rotate, [1.0; 3]), size, node: String::new() }
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
        Surface { world, positions: positions.into(), normals: Cow::Owned(Vec::new()), uvs: Cow::Owned(Vec::new()), triangles: triangles.into(), color, texture: None, wrap: false, v_up: false, headlight: false }
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
        let img = checker();
        let n = 200;
        let positions: Vec<Vec3> = (0..=n).flat_map(|j| (0..=n).map(move |i| Vec3::new(10.0 * i as f64 / n as f64 - 5.0, 2.5 - 5.0 * j as f64 / n as f64, 0.0))).collect();
        let uvs: Vec<[f64; 2]> = (0..=n).flat_map(|j| (0..=n).map(move |i| [i as f64 / n as f64, j as f64 / n as f64])).collect();
        let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let triangles: Vec<[u32; 3]> = (0..n).flat_map(|j| (0..n).flat_map(move |i| [[at(i, j), at(i, j + 1), at(i + 1, j + 1)], [at(i, j), at(i + 1, j + 1), at(i + 1, j)]])).collect();
        let world = geom::trs([0.0, 0.0, -5.0], [0.0; 3], [1.0; 3]);
        let plane = Surface { world, positions: positions.into(), normals: Cow::Owned(Vec::new()), uvs: uvs.clone().into(), triangles: triangles.clone().into(), color: [1.0; 4], texture: Some(&img), wrap: false, v_up: false, headlight: false };
        let card = card_surface_of(&img, world);
        for samples in [1, 3] {
            let (a, b) = (draw(&[plane_ref(&plane)], &[], &camera(), FMT, samples), draw(&[card_surface(&card)], &[], &camera(), FMT, samples));
            let worst = a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
            assert!(worst < 1e-4, "{samples} samples: {worst}");
        }
        // Twice the coordinates, repeating: the checker twice over.
        let doubled: Vec<[f64; 2]> = uvs.iter().map(|uv| [uv[0] * 2.0, uv[1] * 2.0]).collect();
        let tiled = Surface { world, positions: plane.positions.clone(), normals: Cow::Owned(Vec::new()), uvs: doubled.into(), triangles: triangles.into(), color: [1.0; 4], texture: Some(&img), wrap: true, v_up: false, headlight: false };
        let out = draw(&[tiled], &[], &camera(), FMT, 1);
        assert!((0..32).all(|y| (0..32).all(|x| out.at(x, y) == out.at(x + 32, y))), "repeats across");
    }

    /// A card of `img`, 10 × 5, placed by `world`.
    fn card_surface_of(img: &Image, world: Mat4) -> Card {
        Card { image: Arc::new(img.clone()), world, size: [10.0, 5.0], node: String::new() }
    }

    /// A borrowed copy of `s`.
    fn plane_ref<'a>(s: &'a Surface) -> Surface<'a> {
        Surface { world: s.world, positions: Cow::Borrowed(&s.positions), normals: Cow::Borrowed(&s.normals), uvs: Cow::Borrowed(&s.uvs), triangles: Cow::Borrowed(&s.triangles), color: s.color, texture: s.texture, wrap: s.wrap, v_up: s.v_up, headlight: s.headlight }
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
        let out = render_preview(&scene, &camera(), FMT);
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
}
