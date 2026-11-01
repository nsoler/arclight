//! Scanline Render on the GPU (docs/gpu-plan.md, G4): what
//! [`super::draw_passes`] does, on the graphics card, step for step.
//!
//! The triangles are set up on the CPU as for the CPU renderer (in f64:
//! clipped at the near plane and projected, [`super::triangles`]) and
//! binned into 16 × 16-pixel tiles. Then one GPU thread a pixel
//! (`raster.wgsl`) takes its samples a row at a time (a dispatch a row, so
//! no one dispatch runs long), gathers the triangles covering each sample,
//! sorts them nearest first and blends them front to back with the CPU's
//! rules: holdouts, one hit where two triangles of a surface meet, the
//! nearest surface at least half opaque (else the nearest fainter one) kept
//! for the passes. Each hit is shaded there, its texture mipmapped (the
//! levels made on the GPU, `mip.wgsl`, as `mip.rs` makes them) and lit by
//! every light as `shade.rs` does. P, N, uv, motion and id are worked out
//! back on the CPU from the surface each pixel kept, by the CPU renderer's
//! own rules; points too.
//!
//! A sample keeps its 16 nearest surfaces. Where more lie behind them and
//! more than 1e-4 of the light still comes through those 16 (deep stacks of
//! see-through cards: smoke, glass), the frame is rendered again keeping
//! 64; pixels that overflow even those are rendered on the CPU, which keeps
//! them all, and put into the GPU's frame.
//!
//! Shadow casters that let light through (less than opaque, cutout
//! textures) go into their maps' layers: up to [`MAX_LAYERS`] a texel,
//! fewer where the maps would take more than [`layer_room`] (the CPU
//! renders the frame where even one won't fit). A pixel whose shadow
//! lookup needs more than a texel kept overflows the same way.
//!
//! Project 3D's projections go on each hit as `render3d/project.rs` puts
//! them on: their images among the textures, their depth maps drawn with
//! the shadow maps (every surface but holdouts in them). Under motion blur,
//! or where a see-through surface would be in a projector's map, the CPU
//! renders.
//!
//! [`draw_passes`] returns None, and the CPU renders, where this can't yet:
//! shadows (G5), splats (G6), and more light nodes or objects than the
//! shader keeps for the lights and mattes passes. A GPU error is recorded
//! (see `crate::gpu::report_error`) and the CPU renders too.

use super::{shadow, splats, Cell, Passes, Slots, Surface, Tri, Vert};
use crate::color::Rgba;
use crate::geom::{Camera, Vec3};
use crate::gpu::Gpu;
use crate::image::{IRect, Image, Size};
use crate::mesh::Shading;
use crate::ops::project3d::{Mode, Projection};
use crate::scene::Scene;
use crate::shade::{Light, LightKind};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::{Arc, Mutex, Weak};
use wgpu::util::DeviceExt;

pub(super) mod check;
pub(super) mod instances;
pub(super) mod sprites;
pub(super) mod splat_view;
#[cfg(test)]
mod tests;

const RASTER: &str = include_str!("raster.wgsl");
/// How many surfaces a sample keeps in the second try, where the first's
/// 16 overflowed somewhere.
const DEEP_HITS: u32 = 64;

/// `raster.wgsl` keeping [`DEEP_HITS`] surfaces a sample.
fn deep_raster() -> &'static str {
    static DEEP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DEEP.get_or_init(|| RASTER.replace("const MAX_HITS: u32 = 16u;", &format!("const MAX_HITS: u32 = {DEEP_HITS}u;")))
}
const MIP: &str = include_str!("mip.wgsl");
const BLUR: &str = include_str!("blur.wgsl");

// As in raster.wgsl.
const MAX_OBJECTS: usize = 32;
const MAX_LIGHT_NODES: usize = 16;
const TILE: usize = 16;
const TRI_WORDS: usize = 42;
const SURF_WORDS: usize = 16;
const LIGHT_WORDS: usize = 36;
const LEVEL_WORDS: usize = 8;
const PJ_WORDS: usize = 32;
const MAP_WORDS: usize = 32;
const NONE: u32 = u32::MAX;
/// A kept surface that's a splat (see raster.wgsl).
const SPLAT: u32 = 0x4000_0000;
/// A pixel's depth (as bits) when its samples met too many surfaces.
const OVERFLOWED: u32 = u32::MAX;
/// A kept hit on an instance's triangle (raster.wgsl's INSTANCE).
const INSTANCE: u32 = 0x2000_0000;

/// [`super::draw_passes`] on the GPU, or None where it can't (see the
/// module's notes): then the CPU renders.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_passes(surfs: &[Surface], lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Option<Image> {
    // (In the library's tests the renderer's own tests check the CPU
    // renderer to the last bit; the GPU's tests turn this on.)
    #[cfg(test)]
    if !tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    if format.width == 0 || format.height == 0 || (surfs.is_empty() && splats.is_none()) {
        return None;
    }
    // Splats prepared on the CPU are blended there.
    if splats.is_some_and(|sp| sp.gpu().is_none()) {
        return None;
    }
    let gpu = crate::gpu::get()?;
    match draw_with(&gpu, surfs, None, lights, points, cam, format, samples, passes, next, cell, splats) {
        Ok(img) => img,
        Err(e) => {
            crate::gpu::report_error(format!("Scanline Render: {e:#}"), false);
            None
        }
    }
}

/// [`draw_passes`] with `sets`' instances drawn instanced (see
/// instances.rs; `headlight`: theirs shaded as the 3D view's Solid), in
/// the shadow maps too. None where it can't (then the caller draws them as
/// surfaces of their own).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_instanced(surfs: &[Surface], sets: &[crate::geo::Instances], headlight: bool, lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Option<Image> {
    #[cfg(test)]
    if !tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    if format.width == 0 || format.height == 0 || splats.is_some_and(|sp| sp.gpu().is_none()) {
        return None;
    }
    let mut plan = instances::Plan::new(surfs, sets, headlight, cam)?;
    if lights.iter().any(|l| l.shadows) {
        plan.plan_shadows(lights);
    }
    let gpu = crate::gpu::get()?;
    match draw_with(&gpu, &plan.surfs, Some(&plan), lights, points, cam, format, samples, passes, next, cell, splats) {
        Ok(img) => {
            #[cfg(test)]
            tests::INSTANCED.with(|n| n.set(n.get() + usize::from(img.is_some())));
            img
        }
        Err(e) => {
            crate::gpu::report_error(format!("Scanline Render (instances): {e:#}"), false);
            None
        }
    }
}

/// [`super::render_preview`] left on the GPU, for the 3D view to show
/// without reading it back (see [`super::render_preview_gpu`]); None
/// where the GPU can't take it.
pub(super) fn preview(scene: &Scene, cam: &Camera, format: Size, look: super::Preview, blur: Option<&super::Blur>) -> Option<crate::gpu::Resident> {
    #[cfg(test)]
    if !tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    // (Motion blur: the CPU path's, for now.)
    if blur.is_some_and(|b| b.times.len() > 1) || format.width == 0 || format.height == 0 {
        return None;
    }
    let gpu = crate::gpu::get()?;
    // As `preview_cell`: Lit by the scene's lights (their shadows if asked
    // for), else Solid, the meshes (and instances) lit from the camera.
    let mut surfs = super::plain_surfaces(scene);
    let solid = !matches!(look, super::Preview::Lit { .. }) || scene.lights.is_empty();
    let lights: Vec<Light> = match look {
        super::Preview::Lit { shadows } if !scene.lights.is_empty() => scene.lights.iter().map(|l| Light { shadows: l.shadows && shadows, ..l.clone() }).collect(),
        _ => {
            for s in surfs.iter_mut().skip(scene.cards.len()) {
                s.headlight = true;
            }
            Vec::new()
        }
    };
    let all = super::all_splats(scene);
    let splats = splats::prepare_upto(&all, cam, format, super::PREVIEW_SH);
    if splats.as_ref().is_some_and(|sp| sp.gpu().is_none()) {
        return None;
    }
    // Instances drawn instanced.
    let sets = &scene.instances;
    let plan = sets.iter().any(|g| !g.is_empty()).then(|| instances::Plan::new(&surfs, sets, solid, cam)).flatten().map(|mut p| {
        if lights.iter().any(|l| l.shadows) {
            p.plan_shadows(&lights);
        }
        p
    });
    let expanded: Vec<Surface> = match plan {
        Some(_) => Vec::new(),
        None => surfs
            .iter()
            .map(Surface::view)
            .chain(super::instance_surfaces(sets).into_iter().map(|mut s| {
                s.headlight = solid;
                s
            }))
            .collect(),
    };
    let drawn = plan.as_ref().map_or(&expanded[..], |p| &p.surfs[..]);
    if drawn.is_empty() && splats.is_none() {
        return None;
    }
    match preview_on(&gpu, drawn, plan.as_ref(), &lights, cam, format, splats.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            crate::gpu::report_error(format!("3D view: {e:#}"), false);
            None
        }
    }
}

fn preview_on(gpu: &Arc<Gpu>, surfs: &[Surface], plan: Option<&instances::Plan>, lights: &[Light], cam: &Camera, format: Size, splats: Option<&splats::Prepared>) -> Result<Option<crate::gpu::Resident>> {
    let Some(prep) = prepare(gpu, surfs, plan, lights, cam, format, Passes::default(), None, splats) else { return Ok(None) };
    let records = instance_records(gpu, &prep, plan);
    let maps = gpu.scoped(|| draw_maps(gpu, &prep.tables, &prep.textures, records.as_ref().zip(plan)))?;
    let inst = instance_buffers(gpu, records, plan, cam, format)?;
    let image = gpu.scoped(|| {
        let frame = Frame::take(gpu, format.width, format.height, prep.layout.slots.stride);
        frame.clear(gpu);
        dispatch(gpu, &prep, &maps, &frame, cam, format, 1, None, false, splats, inst.as_ref().map(|(r, l, n, p)| (r, l, *n, *p)));
        // (Only the depths come back: an overflowed pixel sends the view
        // to the other path.)
        let depth = frame.read_depth(gpu);
        let image = match depth {
            Ok(d) if !d.iter().any(|v| v.to_bits() == OVERFLOWED) => {
                let image = crate::gpu::image::GpuImage::blank(gpu, format, IRect::frame(format), &[])?;
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                encoder.copy_buffer_to_buffer(&frame.color, 0, &image.rgba, 0, (format.width * format.height * 16) as u64);
                gpu.queue.submit([encoder.finish()]);
                Ok(Some(image))
            }
            Ok(_) => Ok(None),
            Err(e) => Err(e),
        };
        frame.give_back();
        image
    })?;
    Ok(image.map(|img| crate::gpu::Resident::new(gpu.clone(), img)))
}

/// Which object and light node each surface and light counts for, and
/// where the passes' channels are.
struct Layout {
    nodes: Vec<String>,
    object: Vec<Option<usize>>,
    /// Each surface's id in the id pass (see `render3d::surface_ids`).
    ids: Vec<f32>,
    light_names: Vec<String>,
    light_of: Vec<Option<usize>>,
    slots: Slots,
}

impl Layout {
    /// (`splat_nodes`: the splat sets' nodes, objects after the surfaces',
    /// as the CPU renderer counts them.)
    fn new(surfs: &[Surface], lights: &[Light], passes: Passes, splat_nodes: &[String]) -> Layout {
        Layout::with_instances(surfs, lights, passes, splat_nodes, surfs.len(), &[])
    }

    /// [`Layout::new`], with the instances' `nodes` among the objects after
    /// the first `plain` surfaces' (as the CPU renderer counts them, the
    /// instances each a surface after those).
    fn with_instances(surfs: &[Surface], lights: &[Light], passes: Passes, splat_nodes: &[String], plain: usize, instance_nodes: &[String]) -> Layout {
        let mut nodes: Vec<String> = super::object_nodes(&surfs[..plain]).into_iter().map(String::from).collect();
        for n in instance_nodes.iter().map(String::as_str).chain(super::object_nodes(&surfs[plain..])) {
            if !n.is_empty() && !nodes.iter().any(|o| o == n) {
                nodes.push(n.to_string());
            }
        }
        for n in splat_nodes {
            if !n.is_empty() && !nodes.contains(n) {
                nodes.push(n.clone());
            }
        }
        let object: Vec<Option<usize>> = surfs.iter().map(|s| nodes.iter().position(|n| *n == s.node)).collect();
        let ids = super::surface_ids(surfs, &object, nodes.len());
        let (names, light_of) = super::light_nodes(lights);
        let light_names: Vec<String> = names.into_iter().map(String::from).collect();
        let slots = Slots::new(passes, nodes.len(), light_names.len());
        Layout { nodes, object, ids, light_names, light_of, slots }
    }
}

/// What the GPU hands back: per pixel the color and extra channels summed
/// over its samples, the depth, and (when a pass needs it) the surface
/// kept for the passes.
struct Readout {
    color: Vec<Rgba>,
    depth: Vec<f32>,
    extra: Vec<f32>,
    kept: Option<Vec<[u32; 4]>>,
}

/// A frame's render set up for the GPU: what the shader reads (its
/// triangles, tiles, surfaces, lights and shadow maps' plans), the
/// textures, and where the passes go.
struct Prepared {
    /// Where the splat sets' table starts in `tables` (see raster.wgsl).
    splat_table: usize,
    /// Where the projections' table starts in `tables`.
    proj_at: usize,
    /// The instances' records (their objects filled in), if any.
    records: Vec<u32>,
    layout: Layout,
    tris: Vec<Tri>,
    textures: Textures,
    tables: Tables,
    lights: usize,
}

/// Set up `surfs` lit by `lights` through `cam` for the GPU, or None where
/// this renderer can't take it (see the module's notes).
#[allow(clippy::too_many_arguments)]
fn prepare(gpu: &Gpu, surfs: &[Surface], plan: Option<&instances::Plan>, lights: &[Light], cam: &Camera, format: Size, passes: Passes, next: Option<(&Scene, &Camera)>, splats: Option<&splats::Prepared>) -> Option<Prepared> {
    let splat_nodes = splats.map_or(&[][..], |sp| sp.nodes());
    let layout = match plan {
        Some(p) => Layout::with_instances(surfs, lights, passes, splat_nodes, p.plain, &p.nodes),
        None => Layout::new(surfs, lights, passes, splat_nodes),
    };
    // The instances' records, each with its object; with the prototypes,
    // within a binding.
    let records = plan.map_or_else(Vec::new, |p| p.records_with_objects(&layout.nodes));
    if records.len() * 4 > gpu.limits.max_storage_buffer_binding_size as usize {
        return None;
    }
    if (passes.mattes && layout.nodes.len() > MAX_OBJECTS) || (passes.lights && layout.light_names.len() > MAX_LIGHT_NODES) {
        return None;
    }
    // (With instances: as the CPU's surfaces, every instance one, line up
    // with the next frame's.)
    let next_world = passes.motion.then(|| match (plan, next) {
        (Some(p), Some((n, _))) => p.next_positions(&instances::Later::new(n)),
        _ => super::next_positions(surfs, next),
    });
    let tris = super::triangles(surfs, cam, format, next_world.as_deref());
    let px = format.width * format.height;
    // The projections on the surfaces, each once (their images among the
    // textures, their depth maps among the shadow maps); under motion
    // blur (a surface at another time than its frame), on the CPU.
    let mut projs: Vec<&Projection> = Vec::new();
    for p in surfs.iter().flat_map(|s| s.projections.iter()) {
        if !projs.iter().any(|q| std::ptr::eq(*q, *p)) {
            projs.push(p);
        }
    }
    if surfs.iter().any(|s| s.rest.is_some()) {
        return None;
    }
    // (A projector's map with see-through surfaces in it: on the CPU,
    // which keeps their layers.)
    let through = |k: usize, s: &Surface| !s.holdout && ((s.color[3] > 0.0 && s.color[3] < 1.0) || s.texture.is_some_and(|t| !opaque(t)) || plan.is_some_and(|p| k >= p.plain && s.triangles.is_empty() && s.instance.is_none() && p.sees_through(k)));
    if projs.iter().any(|p| p.occlusion) && surfs.iter().enumerate().any(|(k, s)| through(k, s)) {
        return None;
    }
    let plates: Vec<&Arc<Image>> = projs.iter().map(|p| &p.image).collect();
    let textures = Textures::plan(surfs, &plates);
    // Every buffer the shader takes must fit a binding.
    let limit = gpu.limits.max_storage_buffer_binding_size as usize;
    if px * 64 > limit || px * layout.slots.stride * 4 > limit || textures.texels * 16 > limit {
        return None;
    }
    // Shadow maps, as shadow.rs plans them, with layers for casters that
    // let some light through.
    // (With instances: planned for the scene with every one in it.)
    let shadows = plan.and_then(|p| p.shadows.clone()).unwrap_or_else(|| shadow::plan(surfs, lights));
    // (A prototype lets light through where it does, or an instance's tint
    // makes it.)
    let see: Vec<bool> = if shadows.iter().any(Option::is_some) { surfs.iter().enumerate().map(|(k, s)| sees_through(s) || plan.is_some_and(|p| k >= p.plain && s.triangles.is_empty() && s.instance.is_none() && p.sees_through(k))).collect() } else { vec![false; surfs.len()] };
    let (bins, levels_at) = binned(&tris, format.width, format.height, &textures.levels);
    let mut tables = Tables { tris: tri_table(&tris), bins, levels_at, tables: surf_table(surfs, &textures.base, &layout, &see), at: [0; 4], maps: Vec::new(), map_words: 0 };
    let projector_views: Vec<shadow::View> = projs.iter().filter(|p| p.occlusion).map(|p| shadow::Projector::view(&p.camera, p.image.format)).collect();
    let light_maps = map_views(&shadows).len();
    tables.add_lights(surfs, plan, lights, &layout, &shadows, &projector_views, &textures.levels, &see);
    // Each splat set's object (+ 1) and whether it's a holdout.
    let splat_table = tables.tables.len();
    for (k, n) in splat_nodes.iter().enumerate() {
        let object = layout.nodes.iter().position(|o| o == n).filter(|_| !n.is_empty()).map_or(0, |o| o as u32 + 1);
        tables.tables.extend_from_slice(&[object, splats.is_some_and(|sp| sp.holdout(k)) as u32]);
    }
    let proj_at = tables.add_projections(surfs, &projs, &textures.plates, light_maps);
    // The maps within a binding; with layers, within a budget too (else
    // the CPU renders the frame, as it does all of a light's layers).
    let room = if tables.maps.iter().any(|m| m.layered) { layer_room(gpu).min(limit) } else { limit };
    if !tables.add_layers(room) {
        return None;
    }
    Some(Prepared { layout, tris, textures, tables, lights: lights.len(), splat_table, proj_at, records })
}

#[allow(clippy::too_many_arguments)]
fn draw(gpu: &Gpu, surfs: &[Surface], lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Result<Option<Image>> {
    draw_with(gpu, surfs, None, lights, points, cam, format, samples, passes, next, cell, splats)
}

/// [`draw`], with `plan`'s instances (its surfaces `surfs`) drawn
/// instanced.
#[allow(clippy::too_many_arguments)]
fn draw_with(gpu: &Gpu, surfs: &[Surface], plan: Option<&instances::Plan>, lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>, splats: Option<&splats::Prepared>) -> Result<Option<Image>> {
    let n = samples.clamp(1, 16);
    let Some(prep) = prepare(gpu, surfs, plan, lights, cam, format, passes, next, splats) else { return Ok(None) };
    let next_cam = next.map_or(cam, |n| n.1);
    let wants_kept = passes.position || passes.normal || passes.uv || passes.motion || passes.id;
    let records = instance_records(gpu, &prep, plan);
    let maps = gpu.scoped(|| draw_maps(gpu, &prep.tables, &prep.textures, records.as_ref().zip(plan)))?;
    let inst = instance_buffers(gpu, records, plan, cam, format)?;
    let render = |deep: bool| {
        gpu.scoped(|| {
            let frame = Frame::take(gpu, format.width, format.height, prep.layout.slots.stride);
            frame.clear(gpu);
            dispatch(gpu, &prep, &maps, &frame, cam, format, n, cell, deep, splats, inst.as_ref().map(|(r, l, n, p)| (r, l, *n, *p)));
            let out = frame.read(gpu, wants_kept);
            frame.give_back();
            out
        })
    };
    let mut out = render(false)?;
    if out.depth.par_iter().any(|d| d.to_bits() == OVERFLOWED) {
        // Deep see-through stacks somewhere: again keeping more surfaces.
        out = render(true)?;
    }
    let overflowed: Vec<bool> = out.depth.par_iter().map(|d| d.to_bits() == OVERFLOWED).collect();
    let later = next.filter(|_| passes.motion && plan.is_some()).map(|(n, _)| instances::Later::new(n));
    let mut img = finish(&prep.tris, plan, later.as_ref(), points, cam, next_cam, format, n, passes, &prep.layout, out, splats);
    if overflowed.contains(&true) {
        // Those pixels on the CPU, which keeps every surface (the
        // instances each its own).
        let expanded: Vec<Surface> = match plan {
            Some(p) => p.surfs[..p.plain].iter().map(Surface::view).chain(p.cpu_surfaces()).collect(),
            None => surfs.iter().map(Surface::view).collect(),
        };
        let cpu = super::draw_cpu(&expanded, lights, points, cam, format, samples, passes, next, cell, splats, Some(&overflowed));
        let take = |to: &mut [f32], from: &[f32]| to.par_iter_mut().zip(from).zip(&overflowed).filter(|(_, o)| **o).for_each(|((t, f), _)| *t = *f);
        img.pixels.par_iter_mut().zip(&cpu.pixels).zip(&overflowed).filter(|(_, o)| **o).for_each(|((t, f), _)| *t = *f);
        for p in &mut img.planes {
            if let Some(q) = cpu.plane(&p.name) {
                take(&mut p.data, &q.data);
            }
        }
    }
    Ok(Some(img))
}

/// The records of `plan`'s instances drawn here (their objects filled in,
/// in `prep`), if there are any.
fn instance_records(gpu: &Gpu, prep: &Prepared, plan: Option<&instances::Plan>) -> Option<wgpu::Buffer> {
    plan.filter(|p| !p.is_empty())?;
    Some(gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("instance records"), contents: bytemuck::cast_slice(&prep.records), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC }))
}

/// `records` and their bins' lists for the camera (binned on the GPU), if
/// there are any.
#[allow(clippy::type_complexity)]
fn instance_buffers<'p>(gpu: &Gpu, records: Option<wgpu::Buffer>, plan: Option<&'p instances::Plan>, cam: &Camera, format: Size) -> Result<Option<(wgpu::Buffer, wgpu::Buffer, usize, &'p instances::Plan<'p>)>> {
    let (Some(records), Some(p)) = (records, plan) else { return Ok(None) };
    gpu.scoped(|| {
        let (lists, entries) = instances::bin(gpu, p, cam, format, &records, &[])?;
        Ok(Some((records, lists, entries, p)))
    })
}

/// [`super::render_blurred`]'s blurred part on the GPU: `scene` at each of
/// `blur`'s times (each pixel's times jittered by its cell, as
/// [`super::blur_sum`] does), every render added into one frame on the GPU
/// and read back once; the mean, with the passes in `passes` (the
/// picture's: mattes and the light passes), before any background. None
/// where the GPU can't take it: then each time renders as before.
#[allow(clippy::too_many_arguments)]
pub(super) fn blurred(scene: &Scene, cam: &Camera, format: Size, samples: usize, passes: Passes, blur: &super::Blur, frames: &[(i64, &Scene)], background: bool) -> Option<Image> {
    #[cfg(test)]
    if !tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    if format.width == 0 || format.height == 0 || !scene.points.is_empty() {
        return None;
    }
    let gpu = crate::gpu::get()?;
    match blur_on(&gpu, scene, cam, format, samples, passes, blur, frames, background) {
        Ok(img) => img,
        Err(e) => {
            crate::gpu::report_error(format!("Scanline Render motion blur: {e:#}"), false);
            None
        }
    }
}

/// A time's render where sprites go over it before it joins the sum: the
/// render, and it as a picture and depth.
struct Scratch {
    frame: Frame,
    picture: wgpu::Buffer,
    depth: wgpu::Buffer,
}

#[allow(clippy::too_many_arguments)]
fn blur_on(gpu: &Gpu, scene: &Scene, cam: &Camera, format: Size, samples: usize, passes: Passes, blur: &super::Blur, frames: &[(i64, &Scene)], background: bool) -> Result<Option<Image>> {
    use crate::particle_nodes::Look;
    use crate::particles_render::Blend;
    let n = samples.clamp(1, 16);
    let times = &blur.times;
    let side = if blur.jitter { super::jitter_side(times.len()) } else { 1 };
    let share = if times.len() > 1 { times[1] - times[0] } else { 0.0 };
    let mut frame: Option<Frame> = None;
    let mut scratch: Option<Scratch> = None;
    let mut layout: Option<Layout> = None;
    for cy in 0..side {
        for cx in 0..side {
            let cell = (side > 1).then_some(Cell { side, x: cx, y: cy });
            let shift = share * ((super::dither_rank(side, cx, cy) as f64 + 0.5) / (side * side) as f64 - 0.5);
            for &t in times {
                let (now, c) = (scene.at_time(frames, t + shift), blur.camera(cam, t + shift));
                let objects = super::particles::with_objects(&now);
                let now = objects.as_ref().unwrap_or(&now);
                if !now.points.is_empty() {
                    return Ok(None);
                }
                // The splats at this time, seen from this time's camera,
                // prepared on the GPU (their data stays up there).
                let now_splats = splats::prepare(&super::all_splats(now), &c, format);
                if now_splats.as_ref().is_some_and(|sp| sp.gpu().is_none()) {
                    return Ok(None);
                }
                // Points and sprites go over each time's render (as
                // `particles::draw` lays them on the CPU). Added up over a
                // background they'd be the same as over each time, but for
                // added-up sprites: those render time by time.
                let streams: Vec<_> = now.particles.iter().filter(|s| !matches!(s.look, Look::Objects { .. })).collect();
                if background && streams.iter().any(|s| s.sprites().blend == Blend::Additive) {
                    return Ok(None);
                }
                // The instances drawn instanced (at this time's places).
                let plain = super::plain_surfaces(now);
                let mut plan = now.instances.iter().any(|g| !g.is_empty()).then(|| instances::Plan::new(&plain, &now.instances, false, &c)).flatten();
                if let Some(p) = plan.as_mut().filter(|_| now.lights.iter().any(|l| l.shadows)) {
                    p.plan_shadows(&now.lights);
                }
                let expanded: Vec<Surface>;
                let surfs: &[Surface] = match &plan {
                    Some(p) => &p.surfs,
                    None => {
                        expanded = super::surfaces(now);
                        &expanded
                    }
                };
                if surfs.is_empty() && streams.is_empty() && now_splats.is_none() {
                    continue;
                }
                let plan = plan.as_ref();
                #[cfg(test)]
                if plan.is_some() {
                    tests::INSTANCED.with(|n| n.set(n.get() + 1));
                }
                let Some(prep) = prepare(gpu, surfs, plan, &now.lights, &c, format, passes, None, now_splats.as_ref()) else { return Ok(None) };
                // Every time's passes in the same channels.
                let same = layout.as_ref().is_none_or(|l| l.nodes == prep.layout.nodes && l.light_names == prep.layout.light_names);
                if !same {
                    return Ok(None);
                }
                let f = frame.get_or_insert_with(|| {
                    let f = Frame::take(gpu, format.width, format.height, prep.layout.slots.stride);
                    f.clear(gpu);
                    f
                });
                let draw_time = |into: &Frame| -> Result<()> {
                    let records = instance_records(gpu, &prep, plan);
                    let maps = draw_maps(gpu, &prep.tables, &prep.textures, records.as_ref().zip(plan))?;
                    let inst = instance_buffers(gpu, records, plan, &c, format)?;
                    dispatch(gpu, &prep, &maps, into, &c, format, n, cell, false, now_splats.as_ref(), inst.as_ref().map(|(r, l, k, p)| (r, l, *k, *p)));
                    Ok(())
                };
                if streams.is_empty() {
                    gpu.scoped(|| draw_time(f))?;
                } else {
                    let px = (format.width * format.height) as u64;
                    let x = scratch.get_or_insert_with(|| {
                        let buffer = |bytes: u64, label: &str| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16), usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
                        Scratch { frame: Frame::take(gpu, format.width, format.height, prep.layout.slots.stride), picture: buffer(px * 16, "render3d blur picture"), depth: buffer(px * 4, "render3d blur depth") }
                    });
                    gpu.scoped(|| {
                        x.frame.clear(gpu);
                        if !surfs.is_empty() || now_splats.is_some() {
                            draw_time(&x.frame)?;
                        }
                        let mut encoder = gpu.device.create_command_encoder(&Default::default());
                        let params = [format.width as u32, format.height as u32, prep.layout.slots.stride as u32, (n * n) as u32, cell.map_or(0, |c| c.side as u32), cell.map_or(0, |c| c.x as u32), cell.map_or(0, |c| c.y as u32), 0];
                        blur_pass(gpu, &mut encoder, "resolve", &params, &x.frame, &x.picture, &x.depth, f);
                        for stream in &streams {
                            let sprites = stream.sprites();
                            let mut splats = crate::particles_render::splats_through(&stream.particles(), &sprites, &c, format);
                            crate::particles_render::sort_for(&mut splats, &sprites);
                            if !splats.is_empty() {
                                sprites::Job::new(&splats, &sprites, format, true).encode(gpu, &mut encoder, format, &x.picture, &x.depth);
                            }
                        }
                        blur_pass(gpu, &mut encoder, "add", &params, &x.frame, &x.picture, &x.depth, f);
                        gpu.queue.submit([encoder.finish()]);
                        Ok(())
                    })?;
                }
                layout.get_or_insert(prep.layout);
            }
        }
    }
    if let Some(x) = scratch {
        x.frame.give_back();
    }
    let (Some(frame), Some(layout)) = (frame, layout) else { return Ok(None) };
    let out = gpu.scoped(|| frame.read(gpu, false));
    frame.give_back();
    let out = out?;
    if out.depth.par_iter().any(|d| d.to_bits() == OVERFLOWED) {
        return Ok(None);
    }
    // The mean: over each render's samples and the times.
    let Readout { mut color, extra, .. } = out;
    let norm = 1.0 / ((n * n) as f32 * times.len() as f32);
    color.par_iter_mut().for_each(|c| *c = c.map(|v| v * norm));
    let stride = layout.slots.stride.max(1);
    let mut extra = extra;
    extra.resize(color.len() * stride, 0.0);
    extra.par_iter_mut().for_each(|v| *v *= norm);
    let nodes: Vec<&str> = layout.nodes.iter().map(String::as_str).collect();
    let light_names: Vec<&str> = layout.light_names.iter().map(String::as_str).collect();
    let mut img = Image::blank(format, IRect::frame(format));
    img.pixels = color;
    super::add_planes(&mut img, Vec::new(), &extra, stride, passes, &layout.slots, &nodes, &light_names);
    Ok(Some(img))
}

/// Entry `entry` of blur.wgsl over a frame: a time's render `time` (and
/// its `picture` and `depth`) and the frame `sum` adding the times up.
#[allow(clippy::too_many_arguments)]
fn blur_pass(gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, entry: &'static str, params: &[u32; 8], time: &Frame, picture: &wgpu::Buffer, depth: &wgpu::Buffer, sum: &Frame) {
    let pipeline = gpu.compute_pipeline("render3d/blur", BLUR, entry);
    let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("render3d blur params"), contents: bytemuck::cast_slice(params), usage: wgpu::BufferUsages::UNIFORM });
    // Each entry takes only the buffers it uses (see blur.wgsl).
    let all: [(u32, &wgpu::Buffer); 9] = [(0, &uniform), (1, &time.color), (2, &time.extra), (3, &time.hits), (4, picture), (5, depth), (6, &sum.color), (7, &sum.extra), (8, &sum.hits)];
    let used: &[u32] = if entry == "resolve" { &[0, 1, 3, 4, 5] } else { &[0, 2, 3, 4, 6, 7, 8] };
    let entries: Vec<wgpu::BindGroupEntry> = all.iter().filter(|(b, _)| used.contains(b)).map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }).collect();
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d blur"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
    let mut pass = encoder.begin_compute_pass(&Default::default());
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.dispatch_workgroups((params[0] as usize).div_ceil(16) as u32, (params[1] as usize).div_ceil(16) as u32, 1);
}

/// The tables the shader reads, as u32 words (see raster.wgsl).
struct Tables {
    tris: Vec<u32>,
    /// The tiles' triangle lists, then the texture levels from `levels_at`.
    bins: Vec<u32>,
    levels_at: usize,
    /// The surfaces, then from `at`: the lights, their shadows, the shadow
    /// sources and the maps.
    tables: Vec<u32>,
    at: [usize; 4],
    /// Each shadow map to draw.
    maps: Vec<MapJob>,
    /// The words of all of them (depths, then layers).
    map_words: usize,
}

/// A shadow map to draw: its triangles (set up through its camera) and
/// tiles as the shadow kernel reads them, how many texels across, and its
/// camera's near and far; its light, and whether it sees a see-through
/// caster (then it has layers).
struct MapJob {
    tris: Vec<u32>,
    bins: Vec<u32>,
    levels_at: usize,
    res: usize,
    near: f64,
    far: f64,
    light: usize,
    layered: bool,
    /// Its camera (for binning the instances), and the instances it draws
    /// as surfaces of their own instead (a bit each; empty: none).
    cam: Camera,
    skip: Vec<u32>,
}

/// The shadow maps `plan` makes, in order, each with its light.
pub(super) fn map_views(plan: &[Option<shadow::LightView>]) -> Vec<(usize, &shadow::View)> {
    plan.iter()
        .enumerate()
        .filter_map(|(li, l)| l.as_ref().map(|l| (li, l)))
        .flat_map(|(li, l)| l.sources.iter().map(move |src| (li, src)))
        .flat_map(|(li, src)| match src {
            shadow::SourceView::One(v) => vec![(li, v)],
            shadow::SourceView::Cube { faces, .. } => faces.iter().map(|v| (li, v)).collect(),
        })
        .collect()
}

/// The most see-through casters a shadow map's texel keeps (as many as
/// fit the maps' room: see [`Tables::add_layers`]); a lookup needing more
/// sends its pixel to the CPU.
const MAX_LAYERS: usize = 4;

/// The most memory the shadow maps may take where they have layers: some
/// of a discrete GPU's, less of an integrated one's (which shares the
/// system's), within the device's buffers.
fn layer_room(gpu: &Gpu) -> usize {
    #[cfg(test)]
    if let Some(room) = tests::LAYER_ROOM.with(|r| r.get()) {
        return room;
    }
    let mb: usize = if gpu.info.device_type == wgpu::DeviceType::DiscreteGpu { 384 } else { 160 };
    (mb << 20).min(gpu.limits.max_buffer_size.min(usize::MAX as u64) as usize)
}

/// `tris` binned into a `width` × `height` frame's tiles, then the texture
/// `levels`: the words and where the levels start.
fn binned(tris: &[Tri], width: usize, height: usize, levels: &[u32]) -> (Vec<u32>, usize) {
    let mut bins = bin_tiles(tris, width.div_ceil(TILE), height.div_ceil(TILE));
    let at = bins.len();
    bins.extend_from_slice(levels);
    (bins, at)
}

impl Tables {
    /// The lights, their shadows, their shadow sources and maps after the
    /// surfaces, and the maps to draw (each set up on the CPU as shadow.rs
    /// sets up its own).
    #[allow(clippy::too_many_arguments)]
    /// After the lights' maps, `projectors`' (the projections' depth maps,
    /// every surface but holdouts drawing into them).
    fn add_lights(&mut self, surfs: &[Surface], inst: Option<&instances::Plan>, lights: &[Light], layout: &Layout, plan: &[Option<shadow::LightView>], projectors: &[shadow::View], levels: &[u32], see: &[bool]) {
        self.at[0] = self.tables.len();
        self.tables.extend(light_table(lights, layout));
        // Each map, with its light (a projector's: one past the lights).
        let mut views = map_views(plan);
        let light_maps = views.len();
        views.extend(projectors.iter().enumerate().map(|(j, v)| (lights.len() + j, v)));
        let (mut shl, mut srcs, mut maps) = (Vec::new(), Vec::new(), Vec::new());
        let (mut first_src, mut first_map) = (0u32, 0u32);
        let f = |v: f64| (v as f32).to_bits();
        for l in plan {
            let Some(l) = l else {
                shl.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            };
            shl.extend_from_slice(&[first_src, l.sources.len() as u32, f(l.soft), f(l.radius)]);
            for src in &l.sources {
                match src {
                    shadow::SourceView::One(_) => {
                        srcs.extend_from_slice(&[0, first_map, 0, 0, 0, 0, 0, 0]);
                        first_map += 1;
                    }
                    shadow::SourceView::Cube { at, .. } => {
                        srcs.extend_from_slice(&[1, first_map, f(at.x), f(at.y), f(at.z), 0, 0, 0]);
                        first_map += 6;
                    }
                }
                first_src += 1;
            }
        }
        let mut texels = 0usize;
        for (m, (_, v)) in views.iter().enumerate() {
            let size = Size { width: v.res, height: v.res };
            let view = v.cam.view();
            let mut w = [0u32; MAP_WORDS];
            for r in 0..3 {
                for c in 0..4 {
                    w[4 * r + c] = f(view[(r, c)]);
                }
            }
            w[12] = v.cam.is_ortho() as u32;
            w[13] = f(if v.cam.is_ortho() { v.cam.pixels_per_unit(1.0, size) } else { v.cam.focal_pixels(size) });
            w[14] = v.res as u32;
            w[15] = f(v.cam.near);
            if let Some(d) = v.along {
                w[16] = 1;
                (0..3).for_each(|k| w[17 + k] = f(d[k]));
            }
            (0..3).for_each(|k| w[20 + k] = f(v.origin[k]));
            w[23] = f(v.texel);
            w[24] = texels as u32;
            // (25, 26: its layers, 27: its blocks, from `add_layers`.)
            w[28] = (m >= light_maps) as u32;
            maps.extend_from_slice(&w);
            texels += v.res * v.res;
        }
        self.at[1] = self.tables.len();
        self.tables.extend(shl);
        self.at[2] = self.tables.len();
        self.tables.extend(srcs);
        self.at[3] = self.tables.len();
        self.tables.extend(maps);
        self.map_words = texels;
        // (Instances drawn here: in every map, those that see through any
        // making it layered.)
        let inst_see = inst.is_some_and(|p| !p.is_empty() && see.iter().enumerate().any(|(k, s)| *s && k >= p.plain && surfs[k].instance.is_none()));
        self.maps = views
            .par_iter()
            .enumerate()
            .map(|(m, &(light, v))| {
                let size = Size { width: v.res, height: v.res };
                let mut tris = super::triangles(surfs, &v.cam, size, None);
                // The instances this map draws as surfaces of their own:
                // their triangles (clipped by its near plane) here alone.
                let own = inst.map_or(&[][..], |p| p.map_own.get(m).map_or(&[][..], Vec::as_slice));
                for &k in own {
                    if let Some((s, at)) = inst.and_then(|p| p.own_full(k)) {
                        tris.extend(super::triangles(&[s], &v.cam, size, None).into_iter().map(|mut t| {
                            t.surf = at;
                            t
                        }));
                    }
                }
                let layered = inst_see || tris.iter().any(|t| see[t.surf]);
                let (bins, levels_at) = binned(&tris, v.res, v.res, levels);
                // The rest of the instances, binned on the GPU: those this
                // map draws itself left out (one bit each).
                let mut skip = Vec::new();
                if !own.is_empty() {
                    skip = vec![0u32; inst.map_or(0, |p| p.drawn.len()).div_ceil(32)];
                    for &k in own {
                        skip[k as usize / 32] |= 1 << (k % 32);
                    }
                }
                MapJob { tris: tri_table(&tris), bins, levels_at, res: v.res, near: v.cam.near, far: v.cam.far, light, layered, cam: v.cam.clone(), skip }
            })
            .collect();
    }

    /// The projections `projs` (PJ_WORDS each: as raster.wgsl reads them;
    /// their images at `plates`, their depth maps, those with Occlusion,
    /// after the lights' `light_maps`), then each surface's list of them
    /// (how many, then which), its place in its surface's words; where
    /// the projections start.
    fn add_projections(&mut self, surfs: &[Surface], projs: &[&Projection], plates: &[(u32, u32)], light_maps: usize) -> usize {
        let at = self.tables.len();
        let f = |v: f64| (v as f32).to_bits();
        let mut map = light_maps as u32;
        for (p, &(base, mips)) in projs.iter().zip(plates) {
            let mut w = [0u32; PJ_WORDS];
            let (cam, size) = (&p.camera, p.image.format);
            let view = cam.view();
            for r in 0..3 {
                for c in 0..4 {
                    w[4 * r + c] = f(view[(r, c)]);
                }
            }
            for k in 0..3 {
                w[12 + k] = f(cam.world[(k, 3)]);
                w[15 + k] = f(cam.world[(k, 2)]);
            }
            w[18] = f(if cam.is_ortho() { cam.pixels_per_unit(1.0, size) } else { cam.focal_pixels(size) });
            w[19] = cam.is_ortho() as u32 | (p.crop as u32) << 1 | (p.backfaces as u32) << 2 | (p.blend as u32) << 3;
            w[20] = f(size.width as f64);
            w[21] = f(size.height as f64);
            w[22] = f(cam.near);
            w[23] = base;
            w[24] = mips;
            w[25] = match p.mode {
                Mode::Replace => 0,
                Mode::Over => 1,
                Mode::Multiply => 2,
            };
            w[26] = p.mix.to_bits();
            w[27] = f(p.edge);
            w[28] = if p.occlusion {
                map += 1;
                map - 1
            } else {
                NONE
            };
            w[29] = p.fade_start.to_bits();
            w[30] = p.fade_end.to_bits();
            self.tables.extend_from_slice(&w);
        }
        for (si, s) in surfs.iter().enumerate().filter(|(_, s)| !s.projections.is_empty()) {
            self.tables[si * SURF_WORDS + 11] = (self.tables.len() - at) as u32;
            self.tables.push(s.projections.len() as u32);
            for p in &s.projections {
                self.tables.push(projs.iter().position(|q| std::ptr::eq(*q, *p)).unwrap_or(0) as u32);
            }
        }
        at
    }

    /// Each map's blocks (see `map_blocks` in raster.wgsl: 2 words a 4 × 4
    /// texels) after all the depths; then layers for the maps that see a
    /// see-through caster: [`MAX_LAYERS`] a texel, fewer (2, then 1) for the light
    /// whose layers take the most while the maps take more than `room`
    /// bytes. False where they don't fit even so.
    fn add_layers(&mut self, room: usize) -> bool {
        let blocks = |m: &MapJob| 2 * m.res.div_ceil(4).pow(2);
        let depths: usize = self.maps.iter().map(|m| m.res * m.res + blocks(m)).sum();
        let lights = self.maps.iter().map(|m| m.light + 1).max().unwrap_or(0);
        let mut texels = vec![0usize; lights];
        for m in self.maps.iter().filter(|m| m.layered) {
            texels[m.light] += m.res * m.res;
        }
        let mut k: Vec<usize> = texels.iter().map(|&t| if t > 0 { MAX_LAYERS } else { 0 }).collect();
        let words = |k: &[usize]| depths + texels.iter().zip(k).map(|(t, k)| if *k > 0 { t * (2 * k + 1) } else { 0 }).sum::<usize>();
        while words(&k) * 4 > room {
            let Some(l) = (0..lights).filter(|&l| k[l] > 1).max_by_key(|&l| texels[l] * (2 * k[l] + 1)) else { return false };
            k[l] /= 2;
        }
        let mut at = self.maps.iter().map(|m| m.res * m.res).sum::<usize>();
        for (i, m) in self.maps.iter().enumerate() {
            self.tables[self.at[3] + i * MAP_WORDS + 27] = at as u32;
            at += blocks(m);
        }
        for (i, m) in self.maps.iter().enumerate() {
            let k = if m.layered { k[m.light] } else { 0 };
            let w = self.at[3] + i * MAP_WORDS;
            self.tables[w + 25] = at as u32;
            self.tables[w + 26] = k as u32;
            if k > 0 {
                at += m.res * m.res * (2 * k + 1);
            }
        }
        self.map_words = at;
        true
    }

    /// The uniform words the shader starts with (see `Params` in
    /// raster.wgsl), for a frame `w` × `h` with `n` samples a side.
    #[allow(clippy::too_many_arguments)]
    fn params(&self, w: usize, h: usize, n: usize, cell: [u32; 4], slots: [u32; 4], parts: [u32; 4], counts: [u32; 4], at: [usize; 3], cam: [f32; 3], eye: Vec3, back: Vec3, splat: [u32; 4]) -> [u32; 64] {
        let f = |v: f64| (v as f32).to_bits();
        // (44 on: the instances' words and the camera's view and lens, set
        // where there are instances.)
        let mut p = [0u32; 64];
        p[..4].copy_from_slice(&[w as u32, h as u32, n as u32, w.div_ceil(TILE) as u32]);
        p[4..8].copy_from_slice(&cell);
        p[8..12].copy_from_slice(&slots);
        p[12..16].copy_from_slice(&parts);
        p[16..20].copy_from_slice(&counts);
        p[20..24].copy_from_slice(&[0, at[0] as u32, at[1] as u32, at[2] as u32]);
        p[24..28].copy_from_slice(&[cam[0].to_bits(), cam[1].to_bits(), cam[2].to_bits(), 0]);
        p[28..32].copy_from_slice(&[f(eye.x), f(eye.y), f(eye.z), 0]);
        p[32..36].copy_from_slice(&[f(back.x), f(back.y), f(back.z), 0]);
        p[36..40].copy_from_slice(&self.at.map(|a| a as u32));
        p[40..44].copy_from_slice(&splat);
        p
    }
}

/// Whether shadow caster `s` lets light through somewhere: less than
/// opaque, or a texture with a texel neither clear nor opaque or a data
/// window short of its frame (its edges blend part way).
fn sees_through(s: &Surface) -> bool {
    s.casts && !s.holdout && ((s.color[3] > 0.0 && s.color[3] < 1.0) || s.texture.is_some_and(|t| !opaque(t)))
}

/// Whether texture `img` is opaque everywhere in its frame (asked once
/// per texture: kept by `Arc`).
fn opaque(img: &Arc<Image>) -> bool {
    static SEEN: Mutex<Vec<(Weak<Image>, bool)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|(w, _)| w.strong_count() > 0);
    if let Some((_, o)) = seen.iter().find(|(w, _)| std::ptr::eq(w.as_ptr(), Arc::as_ptr(img))) {
        return *o;
    }
    let o = img.window() == img.frame() && img.pixels.par_iter().all(|p| p[3] >= 1.0);
    seen.push((Arc::downgrade(img), o));
    if seen.len() > 64 {
        seen.remove(0);
    }
    o
}

/// Draw every shadow map of `tables` (a dispatch each) into one buffer.
fn draw_maps(gpu: &Gpu, tables: &Tables, textures: &Textures, inst: Option<(&wgpu::Buffer, &instances::Plan)>) -> Result<wgpu::Buffer> {
    let device = &gpu.device;
    let maps = device.create_buffer(&wgpu::BufferDescriptor { label: Some("render3d shadow maps"), size: (tables.map_words.max(1) * 4) as u64, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
    if tables.maps.is_empty() {
        return Ok(maps);
    }
    let pipeline = gpu.compute_pipeline("render3d/raster", RASTER, "shadow_map");
    let no_records = storage(gpu, &[], "render3d shadow (no instances)");
    let inst = inst.filter(|(_, p)| !p.is_empty());
    let blocks = gpu.compute_pipeline("render3d/raster", RASTER, "map_blocks");
    let texel_buf = textures.buffer(gpu);
    let out = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d shadow out"), layout: &pipeline.get_bind_group_layout(1), entries: &[wgpu::BindGroupEntry { binding: 0, resource: maps.as_entire_binding() }] });
    let mut encoder = device.create_command_encoder(&Default::default());
    for (k, m) in tables.maps.iter().enumerate() {
        let (mut all, at) = joined(&m.bins, m.levels_at, &tables.tables, &m.tris);
        let mut params = tables.params(m.res, m.res, 1, [0, 0, 0, k as u32], [0; 4], [0; 4], [0; 4], at, [m.near as f32, m.far as f32, if m.cam.is_ortho() { 1.0 } else { 0.0 }], Vec3::zeros(), Vec3::zeros(), [0; 4]);
        // The instances, binned for this map (those it draws itself left
        // out); their records and lists where the raster has them.
        let mut lists = None;
        if let Some((records, p)) = inst {
            let size = Size { width: m.res, height: m.res };
            let protos_at = all.len();
            all.extend_from_slice(&p.protos);
            let (l, entries) = instances::bin(gpu, p, &m.cam, size, records, &m.skip)?;
            let bins = m.res.div_ceil(instances::BIN).pow(2);
            params[44..48].copy_from_slice(&[(bins + 1) as u32, 0, 0, protos_at as u32]);
            params[48..64].copy_from_slice(&instances::lens_words(&m.cam, size));
            params[63] = entries as u32;
            lists = Some(l);
        }
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("render3d shadow params"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
        let word_buf = storage(gpu, &all, "render3d shadow words");
        // (And the instances' records and lists, or stand-ins.)
        let records = inst.map_or(&no_records, |(r, _)| r);
        let buffers = [(0, &uniform), (1, &word_buf), (2, &*texel_buf), (7, records), (8, lists.as_ref().unwrap_or(&no_records))];
        let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().map(|(i, b)| wgpu::BindGroupEntry { binding: *i, resource: b.as_entire_binding() }).collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d shadow"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(1, &out, &[]);
        pass.dispatch_workgroups(m.res.div_ceil(16) as u32, m.res.div_ceil(16) as u32, 1);
        drop(pass);
        // Then its blocks, from what it drew (the uniform and words only).
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d shadow blocks"), layout: &blocks.get_bind_group_layout(0), entries: &entries[..2] });
        let out = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d shadow blocks out"), layout: &blocks.get_bind_group_layout(1), entries: &[wgpu::BindGroupEntry { binding: 0, resource: maps.as_entire_binding() }] });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&blocks);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(1, &out, &[]);
        pass.dispatch_workgroups(m.res.div_ceil(64) as u32, m.res.div_ceil(64) as u32, 1);
        drop(pass);
        // (With instances, each map's lists go once its passes are in.)
        if inst.is_some() {
            gpu.queue.submit([std::mem::replace(&mut encoder, device.create_command_encoder(&Default::default())).finish()]);
        }
    }
    gpu.queue.submit([encoder.finish()]);
    Ok(maps)
}

/// The words the shader reads (see `words` in raster.wgsl): `bins` (tile
/// lists, then texture levels from `levels_at`), `tables`, `tris`; and
/// where the levels, tables and triangles start.
fn joined(bins: &[u32], levels_at: usize, tables: &[u32], tris: &[u32]) -> (Vec<u32>, [usize; 3]) {
    let mut all = Vec::with_capacity(bins.len() + tables.len() + tris.len());
    all.extend_from_slice(bins);
    all.extend_from_slice(tables);
    all.extend_from_slice(tris);
    (all, [levels_at, bins.len(), bins.len() + tables.len()])
}

/// A read-only storage buffer of `words` (one word if none: bindings can't
/// be empty).
fn storage(gpu: &Gpu, words: &[u32], label: &str) -> wgpu::Buffer {
    let words = if words.is_empty() { &[0u32][..] } else { words };
    gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytemuck::cast_slice(words), usage: wgpu::BufferUsages::STORAGE })
}

/// Render `prep` into `frame` (adding to what's there), a sample row a
/// dispatch, with its shadow `maps`.
#[allow(clippy::too_many_arguments)]
fn dispatch(gpu: &Gpu, prep: &Prepared, maps: &wgpu::Buffer, frame: &Frame, cam: &Camera, format: Size, n: usize, cell: Option<Cell>, deep: bool, splats: Option<&splats::Prepared>, inst: Option<(&wgpu::Buffer, &wgpu::Buffer, usize, &instances::Plan)>) {
    let device = &gpu.device;
    let (w, h) = (format.width, format.height);
    let (tables, layout) = (&prep.tables, &prep.layout);
    let stride = layout.slots.stride;
    let (mut all, at) = joined(&tables.bins, tables.levels_at, &tables.tables, &tables.tris);
    // The prototypes after the triangles.
    let protos_at = all.len();
    if let Some((_, _, _, p)) = inst {
        all.extend_from_slice(&p.protos);
    }
    let word_buf = storage(gpu, &all, "render3d words");
    // The view's splats (or stand-ins when there are none); the
    // instances' records after the splats' projections, their lists after
    // the splats'.
    let none = storage(gpu, &[], "render3d no splats");
    let view = splats.and_then(|sp| sp.gpu());
    let (mut proj, mut lists) = view.map_or((&none, &none), |v| (&v.proj, &v.lists));
    let mut splat = view.map_or([0; 4], |v| [1, (v.tiles + 1) as u32, (at[1] + prep.splat_table) as u32, 0]);
    splat[3] = prep.proj_at as u32;
    let joined_bufs: (wgpu::Buffer, wgpu::Buffer);
    let mut inst_words = [0u32; 4];
    if let Some((records, inst_lists, _, _)) = inst {
        let both = |a: &wgpu::Buffer, b: &wgpu::Buffer, label: &str| {
            let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: a.size() + b.size(), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(a, 0, &out, 0, a.size());
            encoder.copy_buffer_to_buffer(b, 0, &out, a.size(), b.size());
            gpu.queue.submit([encoder.finish()]);
            out
        };
        let tiles = w.div_ceil(instances::BIN) * h.div_ceil(instances::BIN);
        match view {
            Some(v) => {
                joined_bufs = (both(&v.proj, records, "render3d splats and instances"), both(&v.lists, inst_lists, "render3d splat and instance lists"));
                inst_words = [(tiles + 1) as u32, (v.proj.size() / 4) as u32, (v.lists.size() / 4) as u32, protos_at as u32];
                proj = &joined_bufs.0;
                lists = &joined_bufs.1;
            }
            None => {
                inst_words = [(tiles + 1) as u32, 0, 0, protos_at as u32];
                proj = records;
                lists = inst_lists;
            }
        }
    }
    let mut lens = instances::lens_words(cam, format);
    if let Some((_, _, entries, _)) = inst {
        lens[15] = entries as u32;
    }
    let texel_buf = prep.textures.buffer(gpu);
    let pipeline = if deep { gpu.compute_pipeline("render3d/raster-deep", deep_raster(), "main") } else { gpu.compute_pipeline("render3d/raster", RASTER, "main") };
    let bgl = pipeline.get_bind_group_layout(0);
    let s = &layout.slots;
    let on = |b: bool, bit: u32| if b { bit } else { 0 };
    let flags = on(s.part_on[0], 1) | on(s.part_on[1], 2) | on(s.part_on[2], 4) | on(s.part_on[3], 8) | on(s.lights, 16) | on(s.mattes, 32) | on(s.any_part, 64) | on(s.shadow, 128);
    let eye = crate::geom::transform_point(&cam.world, &Vec3::zeros());
    let back: Vec3 = cam.world.fixed_view::<3, 1>(0, 2).into_owned();
    let (side, cx, cy) = cell.map_or((0, 0, 0), |c| (c.side as u32, c.x as u32, c.y as u32));
    let near_far = [cam.near as f32, cam.far as f32, if cam.is_ortho() { 1.0 } else { 0.0 }];
    for j in 0..n {
        let params = tables.params(
            w,
            h,
            n,
            [side, cx, cy, j as u32],
            [stride as u32, flags, s.matte_at as u32, s.light_at as u32],
            s.part_at.map(|a| a as u32),
            [prep.lights as u32, layout.light_names.len() as u32, layout.nodes.len() as u32, s.shadow_at as u32],
            at,
            near_far,
            eye,
            back,
            splat,
        );
        let mut params = params;
        params[44..48].copy_from_slice(&inst_words);
        params[48..64].copy_from_slice(&lens);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("render3d params"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
        let buffers = [&uniform, &word_buf, &*texel_buf, maps, &frame.color, &frame.extra, &frame.hits, proj, lists];
        let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d"), layout: &bgl, entries: &entries });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            // (A cell's pixels only: see `main`.)
            let every = cell.map_or(1, |c| c.side);
            pass.dispatch_workgroups(w.div_ceil(every).div_ceil(16) as u32, h.div_ceil(every).div_ceil(16) as u32, 1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

/// A frame's output buffers and the staging buffer they're read back
/// through: kept for the next render of the same size (one render at a
/// time each: one on another thread meanwhile makes its own).
struct Frame {
    /// Width, height, extra channels a pixel.
    key: (usize, usize, usize),
    color: wgpu::Buffer,
    extra: wgpu::Buffer,
    hits: wgpu::Buffer,
    staging: wgpu::Buffer,
}

static FRAMES: Mutex<Vec<Frame>> = Mutex::new(Vec::new());

impl Frame {
    /// The byte sizes of the parts read back: color, depth, extra, the
    /// surfaces kept.
    fn sizes(w: usize, h: usize, stride: usize) -> [usize; 4] {
        let px = w * h;
        [px * 16, px * 4, px * stride * 4, px * 16]
    }

    fn take(gpu: &Gpu, w: usize, h: usize, stride: usize) -> Frame {
        let key = (w, h, stride);
        let mut kept = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = kept.iter().position(|f| f.key == key) {
            return kept.swap_remove(i);
        }
        drop(kept);
        let px = w * h;
        let make = |bytes: usize, label: &str, usage: wgpu::BufferUsages| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16) as u64, usage, mapped_at_creation: false });
        let out = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        Frame {
            key,
            color: make(px * 16, "render3d color", out),
            extra: make(px * stride * 4, "render3d extra", out),
            hits: make((4 * px + px.div_ceil(4)) * 16, "render3d hits", out),
            staging: make(Frame::sizes(w, h, stride).iter().sum(), "render3d read back", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
        }
    }

    /// Clear its buffers for a new render.
    fn clear(&self, gpu: &Gpu) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.clear_buffer(&self.color, 0, None);
        encoder.clear_buffer(&self.extra, 0, None);
        encoder.clear_buffer(&self.hits, 0, None);
        gpu.queue.submit([encoder.finish()]);
    }

    /// Keep it for the next render (the two latest sizes).
    fn give_back(self) {
        let mut kept = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|f| f.key != self.key);
        kept.push(self);
        while kept.len() > 2 {
            kept.remove(0);
        }
    }

    /// Wait for the render and copy only its depths out.
    fn read_depth(&self, gpu: &Gpu) -> Result<Vec<f32>> {
        let px = self.key.0 * self.key.1;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.hits, (4 * px * 16) as u64, &self.staging, 0, (px * 4) as u64);
        gpu.queue.submit([encoder.finish()]);
        let slice = self.staging.slice(..(px * 4) as u64);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.wait()?;
        rx.recv().map_err(|_| anyhow!("reading the depths back: no answer"))?.map_err(|e| anyhow!("reading the depths back: {e}"))?;
        let mut depth = vec![0.0f32; px];
        {
            let bytes = slice.get_mapped_range().map_err(|e| anyhow!("reading the depths back: {e:?}"))?;
            copy_out(&bytes[..px * 4], bytemuck::cast_slice_mut(&mut depth));
        }
        self.staging.unmap();
        Ok(depth)
    }

    /// Wait for the render and copy its results out.
    fn read(&self, gpu: &Gpu, wants_kept: bool) -> Result<Readout> {
        let (w, h, stride) = self.key;
        let px = w * h;
        let [cb, db, eb, kb] = Frame::sizes(w, h, stride);
        let kb = if wants_kept { kb } else { 0 };
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.color, 0, &self.staging, 0, cb as u64);
        encoder.copy_buffer_to_buffer(&self.hits, (4 * px * 16) as u64, &self.staging, cb as u64, db as u64);
        if eb > 0 {
            encoder.copy_buffer_to_buffer(&self.extra, 0, &self.staging, (cb + db) as u64, eb as u64);
        }
        if kb > 0 {
            encoder.copy_buffer_to_buffer(&self.hits, (2 * px * 16) as u64, &self.staging, (cb + db + eb) as u64, kb as u64);
        }
        gpu.queue.submit([encoder.finish()]);
        let total = (cb + db + eb + kb) as u64;
        let slice = self.staging.slice(..total);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.wait()?;
        rx.recv().map_err(|_| anyhow!("reading the render back: no answer"))?.map_err(|e| anyhow!("reading the render back: {e}"))?;
        let mut out = Readout { color: vec![[0.0; 4]; px], depth: vec![0.0; px], extra: vec![0.0; px * stride], kept: wants_kept.then(|| vec![[0u32; 4]; px]) };
        {
            let bytes = slice.get_mapped_range().map_err(|e| anyhow!("reading the render back: {e:?}"))?;
            copy_out(&bytes[..cb], bytemuck::cast_slice_mut::<Rgba, u8>(&mut out.color));
            copy_out(&bytes[cb..cb + db], bytemuck::cast_slice_mut::<f32, u8>(&mut out.depth));
            copy_out(&bytes[cb + db..cb + db + eb], bytemuck::cast_slice_mut::<f32, u8>(&mut out.extra));
            if let Some(kept) = &mut out.kept {
                copy_out(&bytes[cb + db + eb..cb + db + eb + kb], bytemuck::cast_slice_mut::<[u32; 4], u8>(kept));
            }
        }
        self.staging.unmap();
        Ok(out)
    }
}

/// `from` into `to` (as long), on every core: a 4K frame is over 100 MB.
fn copy_out(from: &[u8], to: &mut [u8]) {
    const CHUNK: usize = 4 << 20;
    to.par_chunks_mut(CHUNK).zip(from.par_chunks(CHUNK)).for_each(|(t, f)| t.copy_from_slice(f));
}

/// The triangles as the shader reads them (see raster.wgsl). Corners are
/// in frame pixels for every triangle (origin 0), so two triangles sharing
/// an edge see its ends as the same f32 values.
fn tri_table(tris: &[Tri]) -> Vec<u32> {
    let mut words = vec![0u32; tris.len() * TRI_WORDS];
    words.par_chunks_mut(TRI_WORDS).zip(tris).for_each(|(o, t)| {
        let f = |v: f64| (v as f32).to_bits();
        let (ox, oy) = (0, 0);
        for k in 0..3 {
            o[2 * k] = f(t.p[k][0] - ox as f64);
            o[2 * k + 1] = f(t.p[k][1] - oy as f64);
            o[7 + k] = f(t.inv_depth[k]);
            for c in 0..3 {
                o[18 + 3 * k + c] = f(t.v[k].world[c]);
                o[27 + 3 * k + c] = f(t.v[k].n[c]);
            }
            o[36 + 2 * k] = f(t.v[k].uv[0]);
            o[37 + 2 * k] = f(t.v[k].uv[1]);
        }
        // The area from the same f32 corners the shader's edges use.
        let p: [[f32; 2]; 3] = std::array::from_fn(|k| [f32::from_bits(o[2 * k]), f32::from_bits(o[2 * k + 1])]);
        o[6] = ((p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0])).to_bits();
        o[10] = t.surf as u32;
        o[11] = t.ortho as u32;
        o[12] = ox as u32;
        o[13] = oy as u32;
        o[14] = t.bounds.x0 as u32;
        o[15] = t.bounds.y0 as u32;
        o[16] = t.bounds.x1 as u32;
        o[17] = t.bounds.y1 as u32;
    });
    words
}

/// Each 16 × 16 tile's triangles, in order: tile starts (one more than
/// there are tiles, as indexes into the whole list), then the lists.
fn bin_tiles(tris: &[Tri], tiles_x: usize, tiles_y: usize) -> Vec<u32> {
    let tiles = tiles_x * tiles_y;
    let span = |t: &Tri| {
        let b = t.bounds;
        let tx0 = b.x0.max(0) as usize / TILE;
        let ty0 = b.y0.max(0) as usize / TILE;
        let tx1 = ((b.x1.max(1) - 1) as usize / TILE).min(tiles_x - 1);
        let ty1 = ((b.y1.max(1) - 1) as usize / TILE).min(tiles_y - 1);
        (tx0, ty0, tx1, ty1)
    };
    let mut counts = vec![0u32; tiles + 1];
    for t in tris {
        let (tx0, ty0, tx1, ty1) = span(t);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                counts[ty * tiles_x + tx + 1] += 1;
            }
        }
    }
    for i in 0..tiles {
        counts[i + 1] += counts[i];
    }
    let starts = tiles + 1;
    let mut out = vec![0u32; starts + counts[tiles] as usize];
    let mut fill: Vec<u32> = counts[..tiles].to_vec();
    for (i, t) in tris.iter().enumerate() {
        let (tx0, ty0, tx1, ty1) = span(t);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let k = ty * tiles_x + tx;
                out[starts + fill[k] as usize] = i as u32;
                fill[k] += 1;
            }
        }
    }
    for (o, c) in out.iter_mut().zip(&counts) {
        *o = c + starts as u32;
    }
    out
}

/// The surfaces as the shader reads them.
fn surf_table(surfs: &[Surface], tex_base: &[Option<(u32, u32)>], layout: &Layout, see: &[bool]) -> Vec<u32> {
    let mut words = vec![0u32; surfs.len() * SURF_WORDS];
    for (si, (o, s)) in words.chunks_mut(SURF_WORDS).zip(surfs).enumerate() {
        for (w, c) in o[..4].iter_mut().zip(s.color) {
            *w = c.to_bits();
        }
        let mut flags = 0u32;
        if let Some(m) = s.material {
            flags |= 1;
            flags |= match m.shading {
                Shading::Unlit => 0,
                Shading::Lambert => 1,
                Shading::Blinn => 2,
            } << 1;
            for c in 0..3 {
                o[4 + c] = m.specular[c].to_bits();
                o[8 + c] = m.emission[c].to_bits();
            }
            o[7] = (crate::shade::shininess(m.roughness) as f32).to_bits();
        }
        flags |= (s.holdout as u32) << 3 | (s.headlight as u32) << 4 | (s.two_sided as u32) << 5 | (s.wrap as u32) << 6 | (s.casts as u32) << 7 | (s.receives as u32) << 8 | (see[si] as u32) << 9;
        o[12] = flags;
        // (Its projections' list, from `add_projections`.)
        o[11] = NONE;
        let (base, levels) = tex_base[si].unwrap_or((NONE, 0));
        o[13] = base;
        o[14] = levels;
        o[15] = layout.object[si].map_or(0, |k| k as u32 + 1);
    }
    words
}

/// The lights as the shader reads them.
fn light_table(lights: &[Light], layout: &Layout) -> Vec<u32> {
    let mut words = vec![0u32; lights.len() * LIGHT_WORDS];
    for (li, (o, l)) in words.chunks_mut(LIGHT_WORDS).zip(lights).enumerate() {
        let f = |v: f64| (v as f32).to_bits();
        o[0] = match l.kind {
            LightKind::Ambient => 0,
            LightKind::Directional => 1,
            LightKind::Point => 2,
            LightKind::Spot { .. } => 3,
            LightKind::Area { .. } => 4,
        };
        o[1] = l.enabled as u32;
        o[2] = layout.light_of[li].map_or(0, |k| k as u32 + 1);
        for c in 0..3 {
            o[4 + c] = l.color[c].to_bits();
        }
        o[7] = l.intensity.to_bits();
        let (p, d) = (l.position(), l.direction());
        for c in 0..3 {
            o[8 + c] = f(p[c]);
            o[12 + c] = f(d[c]);
        }
        o[11] = f(l.decay);
        o[15] = f(l.radius);
        match l.kind {
            LightKind::Spot { cone, penumbra, falloff } => {
                o[16] = f(cone);
                o[17] = f(penumbra);
                o[18] = f(falloff);
            }
            LightKind::Area { size, disk, two_sided, samples } => {
                o[3] = samples as u32;
                o[19] = (two_sided as u32) | (disk as u32) << 1;
                o[20] = f(size[0]);
                o[21] = f(size[1]);
            }
            _ => {}
        }
        for r in 0..3 {
            for c in 0..4 {
                o[24 + 4 * r + c] = f(l.world[(r, c)]);
            }
        }
    }
    words
}

/// The textures the surfaces show, with their mipmaps, in one GPU buffer
/// of texels: where each level goes (the table the shader reads), and per
/// surface its first level and how many mipmap levels follow it (None: no
/// texture).
struct Textures {
    levels: Vec<u32>,
    base: Vec<Option<(u32, u32)>>,
    /// Each projection's image: its first level and mipmaps.
    plates: Vec<(u32, u32)>,
    /// Each texture once (with whether it wraps) and its first level.
    images: Vec<(Arc<Image>, bool, u32)>,
    texels: usize,
}

/// The sizes of `img`'s mipmap levels, as `mip.rs` makes them: half its
/// frame each way (rounding up), down to a texel.
fn mip_sizes(img: &Image) -> Vec<(usize, usize)> {
    let (mut pw, mut ph) = (img.format.width.max(1), img.format.height.max(1));
    let mut out = Vec::new();
    while pw > 1 || ph > 1 {
        (pw, ph) = (pw.div_ceil(2), ph.div_ceil(2));
        out.push((pw, ph));
    }
    out
}

/// The GPU's copy of the textures last uploaded, while they're the same
/// ones: a render after another of the same scene (scrubbing, the 3D view
/// orbiting, motion blur's times) doesn't send them again.
type KeptTexels = Option<(Vec<(Weak<Image>, bool)>, Arc<wgpu::Buffer>)>;
static KEPT: Mutex<KeptTexels> = Mutex::new(None);

impl Textures {
    /// The surfaces' textures, then `plates` (clamped).
    fn plan(surfs: &[Surface], plates: &[&Arc<Image>]) -> Textures {
        let mut images: Vec<(Arc<Image>, bool, u32)> = Vec::new();
        let (mut levels, mut texels) = (Vec::new(), 0usize);
        let mut add = |img: &Arc<Image>, wrap: bool| {
            let mips = mip_sizes(img).len() as u32;
            if let Some(&(_, _, first)) = images.iter().find(|(i, w, _)| Arc::ptr_eq(i, img) && *w == wrap) {
                return (first, mips);
            }
            let first = (levels.len() / LEVEL_WORDS) as u32;
            levels.extend_from_slice(&[texels as u32, img.format.width as u32, img.format.height as u32, img.x0 as u32, img.y0 as u32, img.width as u32, img.height as u32, 0]);
            texels += img.pixels.len();
            for (w, h) in mip_sizes(img) {
                levels.extend_from_slice(&[texels as u32, w as u32, h as u32, 0, 0, w as u32, h as u32, 0]);
                texels += w * h;
            }
            images.push((img.clone(), wrap, first));
            (first, mips)
        };
        let base = surfs.iter().map(|s| s.texture.map(|img| add(img, s.wrap))).collect();
        let plates = plates.iter().map(|img| add(img, false)).collect();
        Textures { levels, base, plates, images, texels }
    }

    /// The texels on the GPU: kept from the last render, else uploaded
    /// and their mipmaps made.
    fn buffer(&self, gpu: &Gpu) -> Arc<wgpu::Buffer> {
        let key: Vec<(Weak<Image>, bool)> = self.images.iter().map(|(i, wrap, _)| (Arc::downgrade(i), *wrap)).collect();
        let same = |k: &[(Weak<Image>, bool)]| k.len() == key.len() && k.iter().zip(&key).all(|(a, b)| a.1 == b.1 && a.0.strong_count() > 0 && Weak::ptr_eq(&a.0, &b.0));
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, buf)) = kept.as_ref().filter(|(k, _)| !key.is_empty() && same(k)) {
            return buf.clone();
        }
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let buf = Arc::new(gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("render3d textures"), size: (self.texels.max(1) * 16) as u64, usage, mapped_at_creation: false }));
        let level = |l: u32| &self.levels[l as usize * LEVEL_WORDS..(l as usize + 1) * LEVEL_WORDS];
        let pipeline = gpu.compute_pipeline("render3d/mip", MIP, "main");
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (img, wrap, first) in &self.images {
            if !img.pixels.is_empty() {
                gpu.queue.write_buffer(&buf, level(*first)[0] as u64 * 16, bytemuck::cast_slice(&img.pixels));
            }
            for k in 0..mip_sizes(img).len() as u32 {
                let (src, dst) = (level(first + k), level(first + k + 1));
                let params: [u32; 12] = [src[0], src[1], src[2], *wrap as u32, src[3], src[4], src[5], src[6], dst[0], dst[1], dst[2], 0];
                let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("render3d mip"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
                let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("render3d mip"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: buf.as_entire_binding() }],
                });
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(dst[1].div_ceil(16), dst[2].div_ceil(16), 1);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        *kept = (!key.is_empty()).then(|| (key, buf.clone()));
        buf
    }
}

/// The render from what the GPU summed: colors and the light passes over
/// the samples, the passes from the surface each pixel kept (as the CPU
/// renderer works them out), the points, then the planes.
#[allow(clippy::too_many_arguments)]
fn finish(tris: &[Tri], plan: Option<&instances::Plan>, later: Option<&instances::Later>, points: &[Vec3], cam: &Camera, next_cam: &Camera, format: Size, n: usize, passes: Passes, layout: &Layout, out: Readout, splats: Option<&splats::Prepared>) -> Image {
    let Readout { mut color, mut depth, mut extra, kept } = out;
    let s = &layout.slots;
    let norm = 1.0 / (n * n) as f32;
    let stride = s.stride.max(1);
    if extra.len() < color.len() * stride {
        extra.resize(color.len() * stride, 0.0);
    }
    color.par_iter_mut().for_each(|c| *c = c.map(|v| v * norm));
    // (Each camera's inverse once, not at every pixel.)
    let (view, next_view) = (cam.view(), next_cam.view());
    extra.par_chunks_mut(stride).enumerate().for_each(|(i, px)| {
        px[..s.stride].iter_mut().for_each(|v| *v *= norm);
        let Some(hit) = kept.as_ref().map(|k| k[i]).filter(|h| h[0] != 0) else { return };
        if hit[0] & SPLAT != 0 {
            // Got half opaque at a splat: P there, the rest from the splat
            // that gives the most (as the CPU renderer does).
            let (Some(sp), Some(view)) = (splats, splats.and_then(|sp| sp.gpu())) else { return };
            let g = (hit[0] & !SPLAT) as usize;
            let k = view.firsts.iter().rposition(|&f| f <= g).unwrap_or(0);
            let d = f32::from_bits(hit[3]) as f64;
            if passes.position {
                let p = cam.unproject([(i % format.width) as f64 + 0.5, (i / format.width) as f64 + 0.5], d, format);
                px[s.p_at..s.p_at + 3].copy_from_slice(&[p.x as f32, p.y as f32, p.z as f32]);
            }
            let (normal, motion) = sp.splat_passes(k, g - view.firsts[k]);
            if let Some(nv) = normal.filter(|_| passes.normal) {
                px[s.n_at..s.n_at + 3].copy_from_slice(&nv);
            }
            if let Some(m) = motion.filter(|_| passes.motion) {
                px[s.motion_at..s.motion_at + 2].copy_from_slice(&m);
            }
            if passes.id {
                let node = &sp.nodes()[k];
                px[s.id_at] = layout.nodes.iter().position(|o| o == node).filter(|_| !node.is_empty()).map_or(0.0, |o| (o + 1) as f32);
            }
            return;
        }
        // An instance's triangle: set up again here, as the CPU would.
        let kept = hit[0] - 1;
        let rebuilt;
        let (t, id) = if kept & INSTANCE != 0 {
            let Some(r) = plan.and_then(|p| p.triangle(kept & !INSTANCE, cam, format, layout.nodes.len(), later)) else { return };
            rebuilt = r;
            (&rebuilt.0, rebuilt.1)
        } else {
            let t = &tris[kept as usize];
            (t, layout.ids[t.surf])
        };
        let (b0, b1) = (f32::from_bits(hit[1]) as f64, f32::from_bits(hit[2]) as f64);
        let bary = [b0, b1, 1.0 - b0 - b1];
        let at = |f: fn(&Vert) -> Vec3| t.v.iter().zip(&bary).map(|(v, b)| f(v) * *b).sum::<Vec3>();
        if passes.position {
            let p = at(|v| v.world);
            px[s.p_at..s.p_at + 3].copy_from_slice(&[p.x as f32, p.y as f32, p.z as f32]);
        }
        if passes.normal {
            let nv = at(|v| v.n).try_normalize(1e-300).unwrap_or_else(Vec3::zeros);
            px[s.n_at..s.n_at + 3].copy_from_slice(&[nv.x as f32, nv.y as f32, nv.z as f32]);
        }
        if passes.uv {
            let uv = t.uv(&bary);
            px[s.uv_at] = uv[0] as f32;
            px[s.uv_at + 1] = (1.0 - uv[1]) as f32;
        }
        if passes.motion {
            let now = cam.project_camera(&crate::geom::transform_point(&view, &at(|v| v.world)), format);
            let later = next_cam.project_camera(&crate::geom::transform_point(&next_view, &at(|v| v.next)), format);
            if let (Some((a, _)), Some((b, _))) = (now, later) {
                px[s.motion_at] = (b[0] - a[0]) as f32;
                px[s.motion_at + 1] = (b[1] - a[1]) as f32;
            }
        }
        if passes.id {
            px[s.id_at] = id;
        }
    });
    super::draw_points(points, cam, format, &mut color, &mut depth);
    let nodes: Vec<&str> = layout.nodes.iter().map(String::as_str).collect();
    let light_names: Vec<&str> = layout.light_names.iter().map(String::as_str).collect();
    let mut img = Image::blank(format, IRect::frame(format));
    img.pixels = color;
    super::add_planes(&mut img, depth, &extra, stride, passes, s, &nodes, &light_names);
    img
}
