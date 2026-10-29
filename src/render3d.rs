//! Scanline Render: draws a 3D scene's cards through a camera into an
//! image, with a `depth` plane.
//!
//! Each card is clipped against the camera's near plane in camera space,
//! projected, and split into triangles. Every output pixel takes
//! `samples × samples` sample points; at each, the cards covering it are
//! found (barycentric coordinates in screen space, texture coordinates
//! interpolated perspective-correctly through 1/depth), sorted nearest
//! first and composited front to back (premultiplied), so a card's
//! transparent parts show what's behind. The samples' mean is the pixel,
//! over the background. Depth is the nearest covered sample's distance in
//! front of the camera, 0 where nothing is. Points draw as one-pixel dots.

use crate::color::{self, Rgba};
use crate::geom::{self, Camera, Vec3};
use crate::image::{IRect, Image, Size};
use crate::scene::Scene;
use rayon::prelude::*;

/// A projected triangle of card `card`: frame positions, texture
/// coordinates and 1 / depth at each corner, and the pixel rows and
/// columns it can touch.
struct Tri {
    card: usize,
    p: [[f64; 2]; 3],
    uv: [[f64; 2]; 3],
    inv_depth: [f64; 3],
    area: f64,
    bounds: IRect,
}

/// A vertex in camera space with its texture coordinates.
#[derive(Clone, Copy)]
struct Vert {
    p: Vec3,
    uv: [f64; 2],
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
            let t = (-near - a.p.z) / (b.p.z - a.p.z);
            out.push(Vert { p: a.p + (b.p - a.p) * t, uv: [a.uv[0] + (b.uv[0] - a.uv[0]) * t, a.uv[1] + (b.uv[1] - a.uv[1]) * t] });
        }
    }
    out
}

/// Twice the signed area of triangle (a, b, c).
fn edge(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Every card's triangles, projected into a frame of `format`.
fn triangles(scene: &Scene, cam: &Camera, format: Size) -> Vec<Tri> {
    let view = cam.view();
    let f = cam.focal_pixels(format);
    let (cx, cy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
    let near = cam.near.max(1e-6);
    let mut tris = Vec::new();
    for (ci, card) in scene.cards.iter().enumerate() {
        // Image corners: top left (0, 0) round to bottom left (0, 1).
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let quad: Vec<Vert> = card.corners().iter().zip(uvs).map(|(c, uv)| Vert { p: geom::transform_point(&view, c), uv }).collect();
        let poly = clip_near(&quad, near);
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
            let bounds = IRect::new(lo(0), lo(1), hi(0), hi(1)).intersect(&IRect::frame(format));
            if bounds.is_empty() {
                continue;
            }
            tris.push(Tri { card: ci, p, uv: idx.map(|i| poly[i].uv), inv_depth: idx.map(|i| projected[i].1), area, bounds });
        }
    }
    tris
}

/// Render `scene`'s cards and points through `cam` into a frame of
/// `format`, `samples × samples` samples a pixel, over `background` (whose
/// frame and data outside the render are kept). The result has a `depth`
/// plane over the frame.
pub fn render(scene: &Scene, cam: &Camera, format: Size, samples: usize, background: Option<&Image>) -> Image {
    let n = samples.clamp(1, 16);
    let tris = triangles(scene, cam, format);
    let frame = IRect::frame(format);
    let (w, h) = (format.width, format.height);
    let mut color = vec![color::TRANSPARENT; w * h];
    let mut depth = vec![0.0f32; w * h];
    color.par_chunks_mut(w.max(1)).zip(depth.par_chunks_mut(w.max(1))).enumerate().for_each(|(y, (crow, drow))| {
        let y = y as i32;
        let row: Vec<&Tri> = tris.iter().filter(|t| y >= t.bounds.y0 && y < t.bounds.y1).collect();
        if row.is_empty() {
            return;
        }
        let mut hits: Vec<(f64, usize, Rgba)> = Vec::new();
        for (x, (out, dout)) in crow.iter_mut().zip(drow.iter_mut()).enumerate() {
            let x = x as i32;
            let here: Vec<&&Tri> = row.iter().filter(|t| x >= t.bounds.x0 && x < t.bounds.x1).collect();
            if here.is_empty() {
                continue;
            }
            let mut sum = [0.0f32; 4];
            let mut nearest = f64::INFINITY;
            for j in 0..n {
                for i in 0..n {
                    let q = [x as f64 + (i as f64 + 0.5) / n as f64, y as f64 + (j as f64 + 0.5) / n as f64];
                    hits.clear();
                    for t in &here {
                        // One hit per card: its two triangles share an edge.
                        if hits.iter().any(|h| h.1 == t.card) {
                            continue;
                        }
                        let b0 = edge(t.p[1], t.p[2], q) / t.area;
                        let b1 = edge(t.p[2], t.p[0], q) / t.area;
                        let b2 = 1.0 - b0 - b1;
                        if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                            continue;
                        }
                        let inv = b0 * t.inv_depth[0] + b1 * t.inv_depth[1] + b2 * t.inv_depth[2];
                        let d = 1.0 / inv;
                        if !(d >= cam.near && d <= cam.far) {
                            continue;
                        }
                        let uv = |k: usize| (b0 * t.uv[0][k] * t.inv_depth[0] + b1 * t.uv[1][k] * t.inv_depth[1] + b2 * t.uv[2][k] * t.inv_depth[2]) / inv;
                        let img = &scene.cards[t.card].image;
                        // Clamped to the image's frame, so a card's edge is
                        // sharp (the samples soften it) rather than fading
                        // over half a texel.
                        let (fw, fh) = (img.format.width as f64, img.format.height as f64);
                        let tx = (uv(0) * fw - 0.5).clamp(0.0, (fw - 1.0).max(0.0));
                        let ty = (uv(1) * fh - 0.5).clamp(0.0, (fh - 1.0).max(0.0));
                        let px = img.sample_bilinear(tx as f32, ty as f32);
                        hits.push((d, t.card, px));
                    }
                    if hits.is_empty() {
                        continue;
                    }
                    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                    // Front to back, premultiplied.
                    let mut acc = [0.0f32; 4];
                    for (d, _, px) in &hits {
                        if px[3] > 0.0 {
                            nearest = nearest.min(*d);
                        }
                        let k = 1.0 - acc[3];
                        (0..4).for_each(|c| acc[c] += px[c] * k);
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
    for p in &scene.points {
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
    match background {
        Some(bg) => over_background(out, bg),
        None => out,
    }
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
        Card { image: Arc::new(img), world: geom::trs(translate, rotate, [1.0; 3]), size }
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
