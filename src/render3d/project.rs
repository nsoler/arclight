//! Project 3D's projections (see `crate::ops::project3d`) as the CPU
//! renderer puts them on: each shaded sample's point, where it is at the
//! frame itself (a surface's [`rest`](Surface::rest) under motion blur, so
//! the image rides on it), through the projector's camera into the image,
//! mipmapped by how many of its pixels the sample covers; cropped, kept
//! off backfaces and off what the projector doesn't see first (a depth map
//! from it, `shadow.rs`'s); then onto the surface's color by the mode,
//! before lighting. Several on one surface go on in order.
//!
//! Under motion blur every time sample renders anew, but a projector's
//! depth map is drawn where the surfaces are at the frame itself: while a
//! [`SameMaps`] is held, the maps drawn are kept and handed out again for
//! the same projector over the same surfaces.

use super::{mip, sample_covering, shadow, Surface, Tri};
use crate::color::Rgba;
use crate::geom::{Mat4, Vec3};
use crate::image::Size;
use crate::ops::project3d::Projection;
use rayon::prelude::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

/// The projections on a render's surfaces, each with its depth map (with
/// Occlusion) and its image's mipmaps (made when a sample sees it small).
pub(super) struct Projectors<'a> {
    list: Vec<Projector<'a>>,
}

/// A projection, its depth map and its image's mipmaps.
type Projector<'a> = (&'a Projection, Option<Arc<shadow::Projector>>, OnceLock<Arc<mip::Mips>>);

impl<'a> Projectors<'a> {
    /// Those on `surfs`, their depth maps drawn from the surfaces where
    /// they are at the frame itself.
    pub fn build(surfs: &[Surface<'a>]) -> Projectors<'a> {
        let mut found: Vec<&'a Projection> = Vec::new();
        for p in surfs.iter().flat_map(|s| s.projections.iter()) {
            if !found.iter().any(|f| std::ptr::eq(*f, *p)) {
                found.push(p);
            }
        }
        if found.is_empty() {
            return Projectors { list: Vec::new() };
        }
        let then: Vec<Surface> = if found.iter().any(|p| p.occlusion) {
            surfs.iter().map(|s| match &s.rest {
                Some(r) => Surface { world: Mat4::identity(), positions: Cow::Borrowed(&r[..]), ..s.view() },
                None => s.view(),
            }).collect()
        } else {
            Vec::new()
        };
        let maps = SameMaps::get_or_draw(&found, &then);
        Projectors { list: found.into_iter().zip(maps).map(|(p, m)| (p, m, OnceLock::new())).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// `color` (premultiplied) of surface `s` at barycentric `bary` on
    /// triangle `t`, seen at frame position `q` with the next sample
    /// `step` pixels on, its normal `n`: with `s`'s projections put on, in
    /// order; consecutive ones with Blend on as one layer, their plates
    /// mixed by how much of each lands (see [`Layer`]).
    #[allow(clippy::too_many_arguments)]
    pub fn apply(&self, s: &Surface, t: &Tri, bary: &[f64; 3], q: [f64; 2], step: f64, n: &Vec3, mut color: Rgba) -> Rgba {
        let rest = |b: &[f64; 3]| t.v.iter().zip(b).map(|(v, w)| v.rest * *w).sum::<Vec3>();
        let p = rest(bary);
        let mut layer: Option<Layer> = None;
        for proj in &s.projections {
            let Some((pj, map, mips)) = self.list.iter().find(|e| std::ptr::eq(e.0, *proj)) else { continue };
            if !(pj.blend && layer.is_some()) {
                if let Some(l) = layer.take() {
                    color = l.put(color);
                }
            }
            let landed = (|| {
                let img = &pj.image;
                let size = img.format;
                let cos = n.dot(&pj.camera.to_eye(&p));
                if !pj.backfaces && cos < 0.0 {
                    return None;
                }
                let (px, _) = pj.camera.project(&p, size)?;
                let mut w = pj.mix * pj.crop_at(px) * pj.facing(cos.abs() as f32);
                if w <= 0.0 {
                    return None;
                }
                if let Some(m) = map {
                    w *= m.seen(&p, n);
                    if w <= 0.0 {
                        return None;
                    }
                }
                let texels = footprint(pj, size, px, [rest(&t.bary_at([q[0] + step, q[1]])), rest(&t.bary_at([q[0], q[1] + step]))]);
                let uv = [px[0] / size.width.max(1) as f64, px[1] / size.height.max(1) as f64];
                Some((w, sample_covering(img, false, uv, texels, mips)))
            })();
            if pj.blend {
                let l = layer.get_or_insert(Layer { first: pj, sum: [0.0; 4], weight: 0.0, only: None, count: 0 });
                if let Some((w, plate)) = landed {
                    l.add(w, plate);
                }
            } else if let Some((w, plate)) = landed {
                color = pj.apply(color, plate, w);
            }
        }
        if let Some(l) = layer {
            color = l.put(color);
        }
        color
    }
}

/// Consecutive projections with Blend on, as one: the plate the mean of
/// theirs by how much of each lands (w: mix, crop, facing, occlusion), as
/// much of it as they add up to (at most all), by the first one's mode.
struct Layer<'a> {
    first: &'a Projection,
    /// Each plate times its w, added up; the ws added up.
    sum: Rgba,
    weight: f32,
    /// The plate and w of the only one landing (as it is, unmixed).
    only: Option<(f32, Rgba)>,
    count: usize,
}

impl Layer<'_> {
    fn add(&mut self, w: f32, plate: Rgba) {
        (0..4).for_each(|c| self.sum[c] += w * plate[c]);
        self.weight += w;
        self.only = Some((w, plate));
        self.count += 1;
    }

    /// `color` with it put on.
    fn put(&self, color: Rgba) -> Rgba {
        match self.only {
            Some((w, plate)) if self.count == 1 => self.first.apply(color, plate, w),
            _ if self.weight > 0.0 => self.first.apply(color, self.sum.map(|v| v / self.weight), self.weight.min(1.0)),
            _ => color,
        }
    }
}

/// What a projector's depth map is drawn from: its image (by identity:
/// clones of a scene share it), its camera, and a fingerprint of the
/// surfaces (see [`fingerprint`]).
type MapKey = (usize, crate::geom::Camera, u64);

/// The depth maps kept, by what each was drawn from.
type Kept = Vec<(MapKey, Arc<shadow::Projector>)>;

thread_local! {
    /// While a [`SameMaps`] is held on this thread: the depth maps drawn.
    static MAPS: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

/// Keeps the projectors' depth maps drawn on this thread (by
/// [`Projectors::build`]) until dropped, to hand out again for the same
/// projector over the same surfaces: motion blur's time samples, whose
/// projected surfaces are where they are at the frame itself.
pub(super) struct SameMaps {
    /// Whether this one started the keeping (an inner one leaves it on).
    first: bool,
}

impl SameMaps {
    pub fn hold() -> SameMaps {
        let first = MAPS.with(|m| {
            let mut m = m.borrow_mut();
            let first = m.is_none();
            m.get_or_insert_with(Vec::new);
            first
        });
        SameMaps { first }
    }

    /// The depth maps of `found` (those with Occlusion) drawn from `then`:
    /// kept ones where held, the rest drawn (and kept, where held).
    fn get_or_draw(found: &[&Projection], then: &[Surface]) -> Vec<Option<Arc<shadow::Projector>>> {
        let held = MAPS.with(|m| m.borrow().is_some());
        let print = if held && found.iter().any(|p| p.occlusion) { fingerprint(then) } else { 0 };
        let key = |p: &Projection| (Arc::as_ptr(&p.image) as usize, p.camera.clone(), print);
        let kept: Vec<Option<Arc<shadow::Projector>>> = found
            .iter()
            .map(|p| if held && p.occlusion { MAPS.with(|m| m.borrow().as_ref().and_then(|v| v.iter().find(|e| e.0 == key(p)).map(|e| e.1.clone()))) } else { None })
            .collect();
        let maps: Vec<Option<Arc<shadow::Projector>>> = found.par_iter().zip(kept).map(|(p, k)| k.or_else(|| p.occlusion.then(|| Arc::new(shadow::Projector::draw(then, &p.camera, p.image.format))))).collect();
        if held {
            MAPS.with(|m| {
                if let Some(v) = m.borrow_mut().as_mut() {
                    for (p, map) in found.iter().zip(&maps) {
                        if let Some(map) = map.as_ref().filter(|_| !v.iter().any(|e| e.0 == key(p))) {
                            v.push((key(p), map.clone()));
                        }
                    }
                }
            });
        }
        maps
    }
}

impl Drop for SameMaps {
    fn drop(&mut self) {
        if self.first {
            MAPS.with(|m| *m.borrow_mut() = None);
        }
    }
}

/// A hash of what a depth map is drawn from in `surfs`: placements,
/// corners, triangles, uvs, colors, textures (by identity), holdouts.
fn fingerprint(surfs: &[Surface]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(29);
    for s in surfs {
        s.world.iter().for_each(|v| mix(v.to_bits()));
        mix(s.positions.len() as u64);
        s.positions.iter().for_each(|p| p.iter().for_each(|v| mix(v.to_bits())));
        mix(s.triangles.len() as u64);
        s.triangles.iter().for_each(|t| t.iter().for_each(|&i| mix(i as u64)));
        mix(s.uvs.len() as u64);
        s.uvs.iter().for_each(|u| u.iter().for_each(|v| mix(v.to_bits())));
        s.color.iter().for_each(|v| mix(v.to_bits() as u64));
        mix(s.texture.map_or(0, |t| Arc::as_ptr(t) as usize as u64));
        mix(s.holdout as u64);
    }
    h
}

/// How many of projection `pj`'s image pixels (of `size`) a sample at
/// image position `px` covers, from where its neighbours `next` land.
fn footprint(pj: &Projection, size: Size, px: [f64; 2], next: [Vec3; 2]) -> f64 {
    next.iter()
        .map(|p| pj.camera.project(p, size).map_or(1.0, |(o, _)| (o[0] - px[0]).hypot(o[1] - px[1])))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use crate::geom::{Camera, Vec3};
    use crate::image::{IRect, Image, Size};
    use crate::ops::project3d::{project_3d, Projection};
    use crate::scene::{Card, Scene};
    use std::sync::Arc;

    const FMT: Size = Size { width: 48, height: 32 };

    fn flat(c: [f32; 4]) -> Image {
        let mut img = Image::blank(FMT, IRect::frame(FMT));
        img.pixels.fill(c);
        img
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    /// A camera at (x, 0, z) turned `yaw` degrees about Y (0: down -Z).
    fn camera(x: f64, z: f64, yaw: f64) -> Camera {
        Camera { world: crate::geom::trs([x, 0.0, z], [0.0, yaw, 0.0], [1.0; 3]), ..Camera::default() }
    }

    /// A camera `deg` degrees round from straight in front of the wall's
    /// middle (0, 0, -10), 10 away, looking at it.
    fn round(deg: f64) -> Camera {
        let r = deg.to_radians();
        camera(10.0 * r.sin(), -10.0 + 10.0 * r.cos(), deg)
    }

    /// A gray wall facing +Z at z = -10, 60 across, with `plates`
    /// projected from their cameras in order, each changed by `set`.
    fn wall(plates: &[(Camera, [f32; 4])], set: impl Fn(&mut Projection)) -> Scene {
        let gray = Image { pixels: vec![[0.5, 0.5, 0.5, 1.0]; 16], ..Image::blank(Size { width: 4, height: 4 }, IRect::frame(Size { width: 4, height: 4 })) };
        let mut card = Card::new(Arc::new(gray), crate::geom::trs([0.0, 0.0, -10.0], [0.0; 3], [1.0; 3]), [60.0, 60.0]);
        card.node = "wall".into();
        let mut scene = Scene { cards: vec![card], ..Scene::default() };
        for (cam, c) in plates {
            let projector = Scene { cameras: vec![cam.clone()], ..Scene::default() };
            scene = project_3d(&serde_json::json!({}), scene, Some(&flat(*c)), Some(&projector)).unwrap();
        }
        scene.projections.iter_mut().for_each(&set);
        scene
    }

    fn render(scene: &Scene) -> Image {
        crate::gpu::cpu_only(|| crate::render3d::render(scene, &camera(0.0, 0.0, 0.0), FMT, 1, None))
    }

    fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    const MID: usize = 16 * 48 + 24;

    /// Facing: a projector at 30° past Fade end leaves the wall to the
    /// head-on one's plate, though it comes after; without fading, it
    /// wins, as stacking does.
    #[test]
    fn the_head_on_projector_wins_where_they_disagree() {
        // (Both aimed at the wall's middle.)
        for cam in [round(0.0), round(30.0)] {
            let (q, _) = cam.project(&Vec3::new(0.0, 0.0, -10.0), FMT).unwrap();
            assert!((q[0] - 24.0).abs() < 1e-6 && (q[1] - 16.0).abs() < 1e-6, "{q:?}");
        }
        let plates = [(round(0.0), RED), (round(30.0), BLUE)];
        assert!(close(render(&wall(&plates, |_| {})).pixels[MID], BLUE, 1e-4), "stacked: the later one");
        let faded = |blend: bool| render(&wall(&plates, |p| (p.fade_start, p.fade_end, p.blend) = (10.0, 25.0, blend))).pixels[MID];
        assert!(close(faded(false), RED, 1e-4), "{:?}", faded(false));
        assert!(close(faded(true), RED, 1e-4), "{:?}", faded(true));
    }

    /// Blend: two projectors either side at the same angle mix 50/50 in
    /// the middle, the nearer to head-on taking more either side of it
    /// (a soft seam); off, the later one covers the other.
    #[test]
    fn blended_projectors_mix_by_facing() {
        let plates = [(round(-30.0), RED), (round(30.0), BLUE)];
        let blended = render(&wall(&plates, |p| (p.fade_start, p.fade_end, p.blend) = (0.0, 60.0, true)));
        // (The middle is between pixels 23 and 24, their centres either
        // side of it.)
        let (l, r) = (blended.pixels[MID - 1], blended.pixels[MID]);
        assert!(l[0] > 0.5 && r[0] < 0.5 && ((l[0] + r[0]) / 2.0 - 0.5).abs() < 2e-3, "{l:?} {r:?}");
        assert!((0..4).all(|c| (l[c] + r[c] - [1.0, 0.0, 1.0, 2.0][c]).abs() < 2e-3), "red and blue add up to all: {l:?} {r:?}");
        // Red falls off from left to right across the middle row, softly.
        let row: Vec<f32> = (8..40).map(|x| blended.pixels[16 * 48 + x][0]).collect();
        assert!(row.windows(2).all(|w| w[1] < w[0]), "{row:?}");
        assert!(row.windows(2).all(|w| w[0] - w[1] < 0.1), "no hard seam: {row:?}");
        assert!(row[0] > 0.6 && row[31] < 0.4, "{row:?}");
        // Fade off, blend on: the mean everywhere both land.
        let even = render(&wall(&plates, |p| p.blend = true));
        assert!(close(even.pixels[MID], [0.5, 0.0, 0.5, 1.0], 1e-3), "{:?}", even.pixels[MID]);
        // Off: stacking, to the bit (a lone blended one too).
        let stacked = render(&wall(&plates, |_| {}));
        assert!(close(stacked.pixels[MID], BLUE, 1e-4));
        let mut first_alone = wall(&plates, |_| {});
        first_alone.projections[0].blend = true;
        assert_eq!(render(&first_alone).pixels, stacked.pixels, "a layer of one: as without Blend");
        let mixed = wall(&plates, |p| p.mix = 0.5);
        let by_hand = |c: [f32; 4]| -> [f32; 4] { std::array::from_fn(|k| 0.5 * c[k] + 0.5 * [0.5, 0.5, 0.5, 1.0][k]) };
        let want = { let a = by_hand(RED); std::array::from_fn(|k| 0.5 * a[k] + 0.5 * BLUE[k]) };
        assert!(close(render(&mixed).pixels[MID], want, 1e-4));
    }

    /// Under motion blur's [`super::SameMaps`], a projector's depth map is
    /// drawn once for the same surfaces, again where they've moved.
    #[test]
    fn depth_maps_are_kept_while_held() {
        let scene = wall(&[(round(0.0), RED)], |_| {});
        let surfs = crate::render3d::surfaces(&scene);
        let map = |surfs: &[crate::render3d::Surface]| super::Projectors::build(surfs).list[0].1.clone().unwrap();
        assert!(!Arc::ptr_eq(&map(&surfs), &map(&surfs)), "not held: drawn each time");
        let held = super::SameMaps::hold();
        let first = map(&surfs);
        let again = scene.clone();
        assert!(Arc::ptr_eq(&first, &map(&crate::render3d::surfaces(&again))), "a clone of the scene: the same map");
        let mut moved = scene.clone();
        moved.cards[0].world = crate::geom::trs([0.0, 0.0, -12.0], [0.0; 3], [1.0; 3]);
        assert!(!Arc::ptr_eq(&first, &map(&crate::render3d::surfaces(&moved))), "moved: drawn again");
        drop(held);
        assert!(super::MAPS.with(|m| m.borrow().is_none()));
    }
}
