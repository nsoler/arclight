//! Project 3D's projections (see `crate::ops::project3d`) as the CPU
//! renderer puts them on: each shaded sample's point, where it is at the
//! frame itself (a surface's [`rest`](Surface::rest) under motion blur, so
//! the image rides on it), through the projector's camera into the image,
//! mipmapped by how many of its pixels the sample covers; cropped, kept
//! off backfaces and off what the projector doesn't see first (a depth map
//! from it, `shadow.rs`'s); then onto the surface's color by the mode,
//! before lighting. Several on one surface go on in order.

use super::{mip, sample_covering, shadow, Surface, Tri};
use crate::color::Rgba;
use crate::geom::{Mat4, Vec3};
use crate::image::Size;
use crate::ops::project3d::Projection;
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

/// The projections on a render's surfaces, each with its depth map (with
/// Occlusion) and its image's mipmaps (made when a sample sees it small).
pub(super) struct Projectors<'a> {
    list: Vec<Projector<'a>>,
}

/// A projection, its depth map and its image's mipmaps.
type Projector<'a> = (&'a Projection, Option<shadow::Projector>, OnceLock<Arc<mip::Mips>>);

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
        let maps: Vec<Option<shadow::Projector>> = found.par_iter().map(|p| p.occlusion.then(|| shadow::Projector::draw(&then, &p.camera, p.image.format))).collect();
        Projectors { list: found.into_iter().zip(maps).map(|(p, m)| (p, m, OnceLock::new())).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// `color` (premultiplied) of surface `s` at barycentric `bary` on
    /// triangle `t`, seen at frame position `q` with the next sample
    /// `step` pixels on, its normal `n`: with `s`'s projections put on.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(&self, s: &Surface, t: &Tri, bary: &[f64; 3], q: [f64; 2], step: f64, n: &Vec3, mut color: Rgba) -> Rgba {
        let rest = |b: &[f64; 3]| t.v.iter().zip(b).map(|(v, w)| v.rest * *w).sum::<Vec3>();
        let p = rest(bary);
        for proj in &s.projections {
            let Some((pj, map, mips)) = self.list.iter().find(|e| std::ptr::eq(e.0, *proj)) else { continue };
            let img = &pj.image;
            let size = img.format;
            if !pj.backfaces && n.dot(&pj.camera.to_eye(&p)) < 0.0 {
                continue;
            }
            let Some((px, _)) = pj.camera.project(&p, size) else { continue };
            let mut w = pj.mix * pj.crop_at(px);
            if w <= 0.0 {
                continue;
            }
            if let Some(m) = map {
                w *= m.seen(&p, n);
                if w <= 0.0 {
                    continue;
                }
            }
            let texels = footprint(pj, size, px, [rest(&t.bary_at([q[0] + step, q[1]])), rest(&t.bary_at([q[0], q[1] + step]))]);
            let uv = [px[0] / size.width.max(1) as f64, px[1] / size.height.max(1) as f64];
            let plate = sample_covering(img, false, uv, texels, mips);
            color = pj.apply(color, plate, w);
        }
        color
    }
}

/// How many of projection `pj`'s image pixels (of `size`) a sample at
/// image position `px` covers, from where its neighbours `next` land.
fn footprint(pj: &Projection, size: Size, px: [f64; 2], next: [Vec3; 2]) -> f64 {
    next.iter()
        .map(|p| pj.camera.project(p, size).map_or(1.0, |(o, _)| (o[0] - px[0]).hypot(o[1] - px[1])))
        .fold(0.0, f64::max)
}
