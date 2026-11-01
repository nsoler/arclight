//! Bake Projection's core (docs/geo-plan.md, P8 "Bake"): what Project 3D
//! puts on a card or mesh, frozen into an image in that surface's uv
//! space, to set as its texture (no projector needed any more) or hand to
//! other tools with Write Geo.
//!
//! Each matched surface's triangles are filled in the space the renderer
//! samples textures in: texel (x, y) is texture coordinates
//! ((x + ½) / width, (y + ½) / height), after the renderer's own flip of
//! v for meshes (`Surface::v_up`). So image row 0 is a card's top, and v
//! = 1 for a mesh (as glTF and OBJ images run). Per texel: the point and
//! normal of the surface there, where it is at the frame, through the
//! renderer's own projection code ([`Projectors::apply_at`]: crop and
//! edge, facing fade, occlusion by the projectors' depth maps, blend
//! layers, mix) over the surface's unlit color (its texture times its
//! color, as the renderer shades it before lighting). Unlit: no lights,
//! no shadows. Several surfaces share one image (where their uvs overlap,
//! the later one's texels win). Padding grows what's filled into the
//! texels round it, so filtering at uv seams doesn't pull in black.

use super::project::Projectors;
use super::{mip, shade_covering, Surface};
use crate::color::{self, Rgba};
use crate::geom::{self, Vec3};
use crate::image::{IRect, Image, Size};
use crate::scene::Scene;
use anyhow::{bail, Result};
use rayon::prelude::*;
use std::sync::{Arc, OnceLock};

/// A bake: three images' worth of texels, row by row, `surface`'s size.
pub struct Baked {
    /// The surfaces' color after their projections (premultiplied; alpha
    /// the surface's own where a triangle covers, 0 off every triangle):
    /// set as their texture, it shows what the projection did.
    pub surface: Image,
    /// The same without the projections: the surfaces' own unlit color.
    pub base: Image,
    /// How much projection landed at each texel, 0 to 1.
    pub coverage: Vec<f32>,
}

impl Baked {
    /// Just what the projections added: alpha the coverage, color the bake
    /// less the base it covers (`surface - base × (1 - coverage)`), so laid
    /// Over the base it gives `surface` back. Exact for Replace and Over;
    /// a Multiply only over the same base.
    pub fn texture(&self) -> Image {
        let mut out = self.surface.clone();
        out.planes.clear();
        out.pixels.par_iter_mut().zip(&self.base.pixels).zip(&self.coverage).for_each(|((o, b), &c)| {
            let rgb: [f32; 3] = std::array::from_fn(|k| (o[k] - b[k] * (1.0 - c)).max(0.0));
            *o = [rgb[0], rgb[1], rgb[2], c];
        });
        out
    }
}

/// A triangle of a matched surface in uv space: its corners there (in
/// texels), where they are at the frame, their normals, and the texel
/// rows and columns it can touch.
struct UvTri {
    surf: usize,
    q: [[f64; 2]; 3],
    p: [Vec3; 3],
    n: [Vec3; 3],
    face: Vec3,
    area: f64,
    bounds: IRect,
}

impl UvTri {
    /// Barycentric coordinates at texel position `q` (outside the
    /// triangle too: the plane's).
    fn bary(&self, q: [f64; 2]) -> [f64; 3] {
        let e = |a: [f64; 2], b: [f64; 2]| (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0]);
        let b0 = e(self.q[1], self.q[2]) / self.area;
        let b1 = e(self.q[2], self.q[0]) / self.area;
        [b0, b1, 1.0 - b0 - b1]
    }

    fn point(&self, b: &[f64; 3]) -> Vec3 {
        self.p[0] * b[0] + self.p[1] * b[1] + self.p[2] * b[2]
    }
}

/// Bake the projections on `scene`'s cards and meshes whose nodes `take`
/// accepts into one `size` image (see the module's help), grown `padding`
/// texels into the empty ones round them. Errors when none is taken, or
/// a taken mesh has no texture coordinates.
pub fn bake(scene: &Scene, take: impl Fn(&str) -> bool, size: Size, padding: u32) -> Result<Baked> {
    if size.width == 0 || size.height == 0 {
        bail!("the bake's width and height must be at least 1");
    }
    // As the renderer sees the scene: particles drawn as objects among it.
    let objects = super::particles::with_objects(scene);
    let scene = objects.as_ref().unwrap_or(scene);
    let surfs = super::surfaces(scene);
    // (Its own cards and meshes come first; instances after, occluding
    // only.)
    let own = scene.cards.len() + scene.meshes.len();
    let picked: Vec<usize> = (0..own.min(surfs.len())).filter(|&k| take(surfs[k].node)).collect();
    if picked.is_empty() {
        bail!("no card or mesh to bake: Group matches none");
    }
    for &k in &picked {
        let s = &surfs[k];
        if s.uvs.len() < s.positions.len() {
            bail!("{} has no texture coordinates (uvs) to bake into", if s.node.is_empty() { "a mesh" } else { s.node });
        }
    }
    let projectors = Projectors::build(&surfs);
    let tris: Vec<UvTri> = picked.iter().flat_map(|&k| uv_triangles(&surfs[k], k, size)).collect();
    let (w, h) = (size.width, size.height);
    // Each row's triangles, in order (later surfaces over earlier ones).
    let mut rows: Vec<Vec<u32>> = vec![Vec::new(); h];
    for (i, t) in tris.iter().enumerate() {
        for row in rows.iter_mut().take(t.bounds.y1.max(0) as usize).skip(t.bounds.y0.max(0) as usize) {
            row.push(i as u32);
        }
    }
    let mips: Vec<OnceLock<Arc<mip::Mips>>> = surfs.iter().map(|_| OnceLock::new()).collect();
    let mut surface = vec![color::TRANSPARENT; w * h];
    let mut base = vec![color::TRANSPARENT; w * h];
    let mut coverage = vec![0.0f32; w * h];
    let mut filled = vec![false; w * h];
    surface
        .par_chunks_mut(w)
        .zip(base.par_chunks_mut(w))
        .zip(coverage.par_chunks_mut(w))
        .zip(filled.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (((surface, base), coverage), filled))| {
            for &ti in &rows[y] {
                let t = &tris[ti as usize];
                let s = &surfs[t.surf];
                for x in t.bounds.x0.max(0) as usize..(t.bounds.x1.max(0) as usize).min(w) {
                    let q = [x as f64 + 0.5, y as f64 + 0.5];
                    let b = t.bary(q);
                    // (A hair outside an edge counts: no cracks between
                    // triangles sharing it.)
                    if b.iter().any(|v| *v < -1e-9) {
                        continue;
                    }
                    let p = t.point(&b);
                    let n = (t.n[0] * b[0] + t.n[1] * b[1] + t.n[2] * b[2]).try_normalize(1e-300).unwrap_or(t.face);
                    let next = [t.point(&t.bary([q[0] + 1.0, q[1]])), t.point(&t.bary([q[0], q[1] + 1.0]))];
                    let uv = [q[0] / w as f64, q[1] / h as f64];
                    // (Its texture seen a bake texel at a time.)
                    let texels = s.texture.map_or(0.0, |img| (img.format.width as f64 / w as f64).max(img.format.height as f64 / h as f64));
                    let own = shade_covering(s, uv, texels, &mips[t.surf]);
                    let (lit, c) = if s.projections.is_empty() { (own, 0.0) } else { projectors.apply_at(s, &p, next, &n, own) };
                    surface[x] = lit;
                    base[x] = own;
                    coverage[x] = c;
                    filled[x] = true;
                }
            }
        });
    grow(&mut surface, &mut base, &mut coverage, &mut filled, size, padding);
    let image = |pixels: Vec<Rgba>| Image { pixels, ..Image::blank(size, IRect::frame(size)) };
    Ok(Baked { surface: image(surface), base: image(base), coverage })
}

/// Surface `s`'s triangles (it is number `k`) in uv space for a `size`
/// bake: corners at the frame, normals in the scene.
fn uv_triangles(s: &Surface, k: usize, size: Size) -> Vec<UvTri> {
    let normal_m = s.world.fixed_view::<3, 3>(0, 0).try_inverse().map(|m| m.transpose()).unwrap_or_else(nalgebra::Matrix3::identity);
    let world: Vec<Vec3> = s.positions.iter().map(|p| geom::transform_point(&s.world, p)).collect();
    let at = s.rest.as_ref().filter(|r| r.len() == world.len()).unwrap_or(&world);
    let normals: Vec<Vec3> = s.normals.iter().map(|n| (normal_m * n).try_normalize(1e-300).unwrap_or(*n)).collect();
    let (fw, fh) = (size.width as f64, size.height as f64);
    let frame = IRect::frame(size);
    s.triangles
        .iter()
        .filter_map(|tri| {
            let [a, b, c] = tri.map(|i| i as usize);
            if a.max(b).max(c) >= world.len() {
                return None;
            }
            let idx = [a, b, c];
            // (As the renderer: v flipped for meshes, normals or the face's.)
            let q = idx.map(|i| {
                let uv = s.uvs[i];
                let v = if s.v_up { 1.0 - uv[1] } else { uv[1] };
                [uv[0] * fw, v * fh]
            });
            let face = (world[b] - world[a]).cross(&(world[c] - world[a])).try_normalize(1e-300).unwrap_or_else(Vec3::z);
            let area = (q[1][0] - q[0][0]) * (q[2][1] - q[0][1]) - (q[1][1] - q[0][1]) * (q[2][0] - q[0][0]);
            if area.abs() < 1e-12 || q.iter().flatten().any(|v| !v.is_finite()) {
                return None;
            }
            let lo = |d: usize| q.iter().map(|p| p[d]).fold(f64::INFINITY, f64::min).floor().max(-1e9) as i32;
            let hi = |d: usize| q.iter().map(|p| p[d]).fold(f64::NEG_INFINITY, f64::max).ceil().min(1e9) as i32;
            let bounds = IRect::new(lo(0), lo(1), hi(0), hi(1)).intersect(&frame);
            (!bounds.is_empty()).then(|| UvTri { surf: k, q, p: idx.map(|i| at[i]), n: idx.map(|i| normals.get(i).copied().unwrap_or(face)), face, area, bounds })
        })
        .collect()
}

/// Grow the filled texels `padding` rings into the empty ones round them:
/// each newly filled one the mean of its filled neighbours (all three
/// images alike).
fn grow(surface: &mut [Rgba], base: &mut [Rgba], coverage: &mut [f32], filled: &mut [bool], size: Size, padding: u32) {
    let (w, h) = (size.width as i64, size.height as i64);
    for _ in 0..padding {
        let was = filled.to_vec();
        let new: Vec<(usize, Rgba, Rgba, f32)> = (0..(w * h) as usize)
            .into_par_iter()
            .filter(|&i| !was[i])
            .filter_map(|i| {
                let (x, y) = (i as i64 % w, i as i64 / w);
                let (mut s, mut b, mut c, mut n) = ([0.0f32; 4], [0.0f32; 4], 0.0f32, 0.0f32);
                for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w || ny >= h || !was[(ny * w + nx) as usize] {
                        continue;
                    }
                    let j = (ny * w + nx) as usize;
                    (0..4).for_each(|k| {
                        s[k] += surface[j][k];
                        b[k] += base[j][k];
                    });
                    c += coverage[j];
                    n += 1.0;
                }
                (n > 0.0).then(|| (i, s.map(|v| v / n), b.map(|v| v / n), c / n))
            })
            .collect();
        if new.is_empty() {
            break;
        }
        for (i, s, b, c) in new {
            (surface[i], base[i], coverage[i], filled[i]) = (s, b, c, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Camera;
    use crate::ops::project3d::project_3d;
    use crate::scene::{Card, Model};
    use serde_json::json;

    const FMT: Size = Size { width: 48, height: 32 };

    /// A plate with a different color at every pixel, opaque.
    fn plate() -> Image {
        let mut img = Image::blank(FMT, IRect::frame(FMT));
        for y in 0..FMT.height {
            for x in 0..FMT.width {
                img.pixels[y * FMT.width + x] = [x as f32 / 47.0, y as f32 / 31.0, ((x * 7 + y * 3) % 11) as f32 / 10.0, 1.0];
            }
        }
        img
    }

    fn gray_card(node: &str, z: f64, size: [f64; 2]) -> Card {
        let s = Size { width: 4, height: 4 };
        let gray = Image { pixels: vec![[0.5, 0.5, 0.5, 1.0]; 16], ..Image::blank(s, IRect::frame(s)) };
        Card { node: node.into(), ..Card::new(Arc::new(gray), geom::trs([0.0, 0.0, z], [0.0; 3], [1.0; 3]), size) }
    }

    /// The projector, down -Z from the origin; the card at `z` exactly
    /// filling its frame.
    fn projector() -> Camera {
        Camera::default()
    }

    fn filling(cam: &Camera, z: f64) -> [f64; 2] {
        let (a, b) = (cam.unproject([0.0, 0.0], -z, FMT), cam.unproject([FMT.width as f64, FMT.height as f64], -z, FMT));
        [(b.x - a.x).abs(), (b.y - a.y).abs()]
    }

    fn projected(scene: Scene, params: serde_json::Value, img: &Image) -> Scene {
        let p = Scene { cameras: vec![projector()], ..Scene::default() };
        project_3d(&params, scene, Some(img), Some(&p)).unwrap()
    }

    fn close(a: Rgba, b: Rgba, tol: f32) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    /// A card the projector sees head on, filling its frame, baked at the
    /// plate's size: the plate, texel for texel, the right way up; all of
    /// it covered; the base its own gray.
    #[test]
    fn a_card_seen_head_on_bakes_to_the_plate() {
        let cam = projector();
        let card = gray_card("wall", -10.0, filling(&cam, -10.0));
        let scene = projected(Scene { cards: vec![card], ..Scene::default() }, json!({}), &plate());
        let b = bake(&scene, |n| n == "wall", FMT, 0).unwrap();
        let p = plate();
        for (i, (a, want)) in b.surface.pixels.iter().zip(&p.pixels).enumerate() {
            assert!(close(*a, *want, 1e-4), "texel {i}: {a:?} vs {want:?}");
        }
        assert!(b.coverage.iter().all(|c| (*c - 1.0).abs() < 1e-6));
        assert!(b.base.pixels.iter().all(|px| close(*px, [0.5, 0.5, 0.5, 1.0], 1e-6)));
        // Bigger than the plate: still it, filtered.
        let big = bake(&scene, |n| n == "wall", Size { width: 96, height: 64 }, 0).unwrap();
        assert!(close(big.surface.pixels[33 * 96 + 50], p.sample_bilinear(24.75, 16.25), 1e-3));
    }

    /// What the texture output holds: laid Over the base, the bake back
    /// (Replace with a feathered edge, Over with a see-through plate);
    /// none of it where nothing landed.
    #[test]
    fn the_texture_over_the_base_is_the_bake() {
        let cam = projector();
        let wide = gray_card("wall", -10.0, filling(&cam, -10.0).map(|v| v * 1.6));
        let mut see_through = plate();
        see_through.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = p.map(|v| v * (0.2 + 0.6 * (i % 5) as f32 / 4.0)));
        for (params, img) in [(json!({ "edge": 6 }), plate()), (json!({ "mode": "over", "mix": 0.7 }), see_through)] {
            let scene = projected(Scene { cards: vec![wide.clone()], ..Scene::default() }, params.clone(), &img);
            let b = bake(&scene, |_| true, Size { width: 64, height: 40 }, 0).unwrap();
            let t = b.texture();
            let partial = b.coverage.iter().filter(|c| **c > 0.01 && **c < 0.99).count();
            assert!(partial > 0 && b.coverage.contains(&0.0), "{params}: some partly covered, some not at all");
            for i in 0..t.pixels.len() {
                let (tex, base) = (t.pixels[i], b.base.pixels[i]);
                let over: Rgba = std::array::from_fn(|k| tex[k] + base[k] * (1.0 - tex[3]));
                assert!(close(over, b.surface.pixels[i], 1e-5), "{params} texel {i}: {over:?} vs {:?}", b.surface.pixels[i]);
                if b.coverage[i] == 0.0 {
                    assert_eq!(tex, [0.0; 4]);
                }
            }
        }
    }

    /// Occlusion: the part of the back card a smaller card in front hides
    /// from the projector bakes its own gray, uncovered; without Occlusion
    /// the plate goes through.
    #[test]
    fn what_the_projector_doesnt_see_bakes_its_own_color() {
        let cam = projector();
        let back = gray_card("back", -10.0, filling(&cam, -10.0));
        let front = gray_card("front", -5.0, [1.0, 1.0]);
        let middle = (FMT.height / 2) * FMT.width + FMT.width / 2;
        for (occlusion, hidden) in [(true, true), (false, false)] {
            let scene = projected(Scene { cards: vec![back.clone(), front.clone()], ..Scene::default() }, json!({ "occlusion": occlusion }), &plate());
            let b = bake(&scene, |n| n == "back", FMT, 0).unwrap();
            assert_eq!(close(b.surface.pixels[middle], [0.5, 0.5, 0.5, 1.0], 1e-4), hidden, "occlusion {occlusion}: {:?}", b.surface.pixels[middle]);
            assert_eq!(b.coverage[middle] == 0.0, hidden);
            assert!(close(b.surface.pixels[0], plate().pixels[0], 1e-4), "the corner the front card doesn't hide");
        }
    }

    /// Baked and set as the card's texture, the projector gone: the card
    /// seen from the side looks as with the projection live.
    #[test]
    fn baked_then_rendered_looks_like_the_live_projection() {
        let cam = projector();
        let card = gray_card("wall", -10.0, filling(&cam, -10.0).map(|v| v * 1.3));
        let live = projected(Scene { cards: vec![card.clone()], ..Scene::default() }, json!({ "edge": 4 }), &plate());
        let b = bake(&live, |_| true, Size { width: 256, height: 256 }, 2).unwrap();
        let frozen = Scene { cards: vec![Card { image: Arc::new(b.surface.clone()), ..card }], ..Scene::default() };
        let side = Camera { world: geom::trs([3.0, 1.0, 0.0], [-4.0, 15.0, 0.0], [1.0; 3]), ..cam };
        let size = Size { width: 160, height: 100 };
        let (a, f) = crate::gpu::cpu_only(|| (super::super::render(&live, &side, size, 2, None), super::super::render(&frozen, &side, size, 2, None)));
        let diff: f64 = a.pixels.iter().zip(&f.pixels).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs() as f64).sum::<f64>()).sum::<f64>() / (a.pixels.len() * 4) as f64;
        let changed = a.pixels.iter().filter(|p| !close(**p, [0.5, 0.5, 0.5, 1.0], 0.02) && p[3] > 0.0).count();
        assert!(changed > a.pixels.len() / 4, "the projection shows: {changed}");
        assert!(diff < 2e-3, "mean difference {diff}");
    }

    /// A Cube with UV layout atlas bakes every face it shows the projector
    /// to its own part of one texture: baked (as Bake Projection sets it:
    /// the texture, its color white) and seen from the side, it looks as
    /// with the projection live. Per face, the faces overwrite one another
    /// in the shared uv square, and it doesn't.
    #[test]
    fn an_atlas_cube_bakes_every_face() {
        let cam = projector();
        let side = filling(&cam, -5.0)[1] * 0.45;
        let mean_diff = |layout: &str| {
            let params = json!({ "uv_layout": layout, "size": [side, side, side], "translate": [0.0, 0.0, -6.0], "rotate": [25.0, 35.0, 0.0], "color": [0.5, 0.5, 0.5, 1.0] });
            let block = Model { node: "block".into(), ..crate::scene::primitive_model("cube", &params, None).unwrap() };
            let live = projected(Scene { meshes: vec![block.clone()], ..Scene::default() }, json!({}), &plate());
            let b = bake(&live, |_| true, Size { width: 512, height: 512 }, 4).unwrap();
            let frozen = Model { texture: Some(Arc::new(b.surface)), material: crate::mesh::Material { color: [1.0; 3], ..block.material.clone() }, ..block };
            let frozen = Scene { meshes: vec![frozen], ..Scene::default() };
            let view = Camera { world: geom::trs([1.5, 0.8, -1.0], [-8.0, 12.0, 0.0], [1.0; 3]), ..cam.clone() };
            let size = Size { width: 160, height: 100 };
            let (a, f) = crate::gpu::cpu_only(|| (super::super::render(&live, &view, size, 2, None), super::super::render(&frozen, &view, size, 2, None)));
            let shown = a.pixels.iter().filter(|p| p[3] > 0.5).count();
            assert!(shown > a.pixels.len() / 10, "{layout}: the cube is in view ({shown})");
            a.pixels.iter().zip(&f.pixels).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs() as f64).sum::<f64>()).sum::<f64>() / (shown * 4) as f64
        };
        let (atlas, per_face) = (mean_diff("atlas"), mean_diff("per face"));
        // (Not nil: the plate's blue, different at every pixel, is filtered
        // twice, into the texture and out of it.)
        assert!(atlas < 6e-3, "atlas: mean difference {atlas}");
        assert!(per_face > 0.03, "per face: mean difference {per_face}");
    }

    /// Padding grows the filled texels into empty uv space; a mesh without
    /// uvs, or a Group matching nothing, is an error.
    #[test]
    fn padding_and_errors() {
        let mut mesh = crate::mesh::plane([4.0, 4.0], [1, 1]);
        // (Its uvs squeezed into the left half.)
        mesh.uvs.iter_mut().for_each(|uv| uv[0] *= 0.5);
        let floor = Model { node: "floor".into(), ..Model::new(Arc::new(mesh.clone()), geom::trs([0.0, 0.0, -10.0], [0.0; 3], [1.0; 3]), crate::mesh::Material::default()) };
        let scene = projected(Scene { meshes: vec![floor.clone()], ..Scene::default() }, json!({ "crop": false }), &plate());
        let size = Size { width: 32, height: 32 };
        let (bare, padded) = (bake(&scene, |_| true, size, 0).unwrap(), bake(&scene, |_| true, size, 3).unwrap());
        let row = 16 * 32;
        assert!(bare.surface.pixels[row + 15][3] > 0.0 && bare.surface.pixels[row + 17][3] == 0.0, "filled to u = 0.5");
        assert!(padded.surface.pixels[row + 18][3] > 0.0 && padded.coverage[row + 18] > 0.0, "grown 3 texels");
        assert_eq!(padded.surface.pixels[row + 20][3], 0.0, "and no further");
        assert_eq!(padded.surface.pixels[row + 10], bare.surface.pixels[row + 10], "what was filled stays");
        mesh.uvs.clear();
        let bare_mesh = Scene { meshes: vec![Model { mesh: Arc::new(mesh), ..floor }], ..Scene::default() };
        let err = bake(&bare_mesh, |_| true, size, 0).err().expect("no uvs");
        assert!(format!("{err}").contains("floor has no texture coordinates"), "{err}");
        assert!(bake(&scene, |n| n == "nothing", size, 0).is_err());
    }

    /// How long the multi-projection example's ground (two blended
    /// projections, occluded by the block) takes to bake at 2048 and 4096.
    #[test]
    #[ignore]
    fn bake_timing() {
        let g: crate::graph::Graph = crate::graph::Graph::from_file(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/multi-projection.json"))).unwrap();
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let scene = crate::graph::Evaluator::new().evaluate(&g, "world", &ctx).unwrap().scene.clone().unwrap();
        for side in [2048, 4096] {
            let t = std::time::Instant::now();
            let b = bake(&scene, |n| n == "ground" || n == "wall", Size { width: side, height: side }, 4).unwrap();
            eprintln!("bake {side}²: {:.2} s, {:.0} % covered", t.elapsed().as_secs_f64(), 100.0 * b.coverage.iter().filter(|c| **c > 0.0).count() as f64 / b.coverage.len() as f64);
        }
    }
}
