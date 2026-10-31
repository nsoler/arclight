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
//! [`draw_passes`] returns None, and the CPU renders, where this can't yet:
//! shadows (G5), splats (G6), and more light nodes or objects than the
//! shader keeps for the lights and mattes passes. A GPU error is recorded
//! (see `crate::gpu::report_error`) and the CPU renders too.

use super::{splats, Cell, Passes, Slots, Surface, Tri, Vert};
use crate::color::Rgba;
use crate::geom::{Camera, Vec3};
use crate::gpu::Gpu;
use crate::image::{IRect, Image, Size};
use crate::mesh::Shading;
use crate::scene::Scene;
use crate::shade::{Light, LightKind};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::{Arc, Mutex, Weak};
use wgpu::util::DeviceExt;

pub(super) mod check;
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

// As in raster.wgsl.
const MAX_OBJECTS: usize = 32;
const MAX_LIGHT_NODES: usize = 16;
const TILE: usize = 16;
const TRI_WORDS: usize = 42;
const SURF_WORDS: usize = 16;
const LIGHT_WORDS: usize = 36;
const LEVEL_WORDS: usize = 8;
const NONE: u32 = u32::MAX;
/// A pixel's depth (as bits) when its samples met too many surfaces.
const OVERFLOWED: u32 = u32::MAX;

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
    if splats.is_some() || format.width == 0 || format.height == 0 || surfs.is_empty() {
        return None;
    }
    // Shadows: G5.
    if lights.iter().any(|l| l.shadows && l.enabled && l.kind != LightKind::Ambient) && surfs.iter().any(|s| s.casts) {
        return None;
    }
    let gpu = crate::gpu::get()?;
    match draw(&gpu, surfs, lights, points, cam, format, samples, passes, next, cell) {
        Ok(img) => img,
        Err(e) => {
            crate::gpu::report_error(format!("Scanline Render: {e:#}"), false);
            None
        }
    }
}

/// Which object and light node each surface and light counts for, and
/// where the passes' channels are.
struct Layout {
    nodes: Vec<String>,
    object: Vec<Option<usize>>,
    light_names: Vec<String>,
    light_of: Vec<Option<usize>>,
    slots: Slots,
}

impl Layout {
    fn new(surfs: &[Surface], lights: &[Light], passes: Passes) -> Layout {
        let nodes: Vec<String> = super::object_nodes(surfs).into_iter().map(String::from).collect();
        let object = surfs.iter().map(|s| nodes.iter().position(|n| *n == s.node)).collect();
        let (names, light_of) = super::light_nodes(lights);
        let light_names: Vec<String> = names.into_iter().map(String::from).collect();
        let slots = Slots::new(passes, nodes.len(), light_names.len());
        Layout { nodes, object, light_names, light_of, slots }
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

#[allow(clippy::too_many_arguments)]
fn draw(gpu: &Gpu, surfs: &[Surface], lights: &[Light], points: &[Vec3], cam: &Camera, format: Size, samples: usize, passes: Passes, next: Option<(&Scene, &Camera)>, cell: Option<Cell>) -> Result<Option<Image>> {
    let n = samples.clamp(1, 16);
    let layout = Layout::new(surfs, lights, passes);
    if (passes.mattes && layout.nodes.len() > MAX_OBJECTS) || (passes.lights && layout.light_names.len() > MAX_LIGHT_NODES) {
        return Ok(None);
    }
    let next_cam = next.map_or(cam, |n| n.1);
    let next_world = passes.motion.then(|| super::next_positions(surfs, next));
    let tris = super::triangles(surfs, cam, format, next_world.as_deref());
    let px = format.width * format.height;
    let textures = Textures::plan(surfs);
    // Every buffer the shader takes must fit a binding.
    let limit = gpu.limits.max_storage_buffer_binding_size as usize;
    if px * 64 > limit || px * layout.slots.stride * 4 > limit || textures.texels * 16 > limit {
        return Ok(None);
    }
    let tiles_x = format.width.div_ceil(TILE);
    let mut bins = bin_tiles(&tris, tiles_x, format.height.div_ceil(TILE));
    let levels_at = bins.len();
    bins.extend_from_slice(&textures.levels);
    let tables = Tables { tris: tri_table(&tris), bins, levels_at, surfs: surf_table(surfs, &textures.base, &layout), lights: light_table(lights, &layout) };
    let wants_kept = passes.position || passes.normal || passes.uv || passes.motion || passes.id;
    let mut out = gpu.scoped(|| run(gpu, &tables, &textures, cam, format, n, lights.len(), &layout, cell, wants_kept, false))?;
    if out.depth.par_iter().any(|d| d.to_bits() == OVERFLOWED) {
        // Deep see-through stacks somewhere: again keeping more surfaces.
        out = gpu.scoped(|| run(gpu, &tables, &textures, cam, format, n, lights.len(), &layout, cell, wants_kept, true))?;
    }
    let overflowed: Vec<bool> = out.depth.par_iter().map(|d| d.to_bits() == OVERFLOWED).collect();
    let mut img = finish(&tris, points, cam, next_cam, format, n, passes, &layout, out);
    if overflowed.contains(&true) {
        // Those pixels on the CPU, which keeps every surface.
        let cpu = super::draw_cpu(surfs, lights, points, cam, format, samples, passes, next, cell, None, Some(&overflowed));
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

/// The tables the shader reads, as u32 words (see raster.wgsl).
struct Tables {
    tris: Vec<u32>,
    /// The tiles' triangle lists, then the texture levels from `levels_at`.
    bins: Vec<u32>,
    levels_at: usize,
    surfs: Vec<u32>,
    lights: Vec<u32>,
}

/// Upload, dispatch a sample row at a time, read back.
#[allow(clippy::too_many_arguments)]
fn run(gpu: &Gpu, tables: &Tables, textures: &Textures, cam: &Camera, format: Size, n: usize, n_lights: usize, layout: &Layout, cell: Option<Cell>, wants_kept: bool, deep: bool) -> Result<Readout> {
    let device = &gpu.device;
    let (w, h) = (format.width, format.height);
    let stride = layout.slots.stride;
    let read = |words: &[u32], label: &str| {
        // (Bindings can't be empty.)
        let words = if words.is_empty() { &[0u32][..] } else { words };
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytemuck::cast_slice(words), usage: wgpu::BufferUsages::STORAGE })
    };
    let tri_buf = read(&tables.tris, "render3d tris");
    let bin_buf = read(&tables.bins, "render3d bins");
    let surf_buf = read(&tables.surfs, "render3d surfaces");
    let light_buf = read(&tables.lights, "render3d lights");
    let texel_buf = textures.buffer(gpu);
    let frame = Frame::take(gpu, w, h, stride);

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.clear_buffer(&frame.color, 0, None);
    encoder.clear_buffer(&frame.extra, 0, None);
    encoder.clear_buffer(&frame.hits, 0, None);
    gpu.queue.submit([encoder.finish()]);

    let pipeline = if deep { gpu.compute_pipeline("render3d/raster-deep", deep_raster(), "main") } else { gpu.compute_pipeline("render3d/raster", RASTER, "main") };
    let bgl = pipeline.get_bind_group_layout(0);
    let s = &layout.slots;
    let on = |b: bool, bit: u32| if b { bit } else { 0 };
    let flags = on(s.part_on[0], 1) | on(s.part_on[1], 2) | on(s.part_on[2], 4) | on(s.part_on[3], 8) | on(s.lights, 16) | on(s.mattes, 32) | on(s.any_part, 64);
    let eye = crate::geom::transform_point(&cam.world, &Vec3::zeros());
    let back: Vec3 = cam.world.fixed_view::<3, 1>(0, 2).into_owned();
    let (side, cx, cy) = cell.map_or((0, 0, 0), |c| (c.side as u32, c.x as u32, c.y as u32));
    let f = |v: f64| (v as f32).to_bits();
    for j in 0..n {
        #[rustfmt::skip]
        let params: [u32; 36] = [
            w as u32, h as u32, n as u32, w.div_ceil(TILE) as u32,
            side, cx, cy, j as u32,
            stride as u32, flags, s.matte_at as u32, s.light_at as u32,
            s.part_at[0] as u32, s.part_at[1] as u32, s.part_at[2] as u32, s.part_at[3] as u32,
            n_lights as u32, layout.light_names.len() as u32, layout.nodes.len() as u32, 0,
            0, tables.levels_at as u32, 0, 0,
            f(cam.near), f(cam.far), f(if cam.is_ortho() { 1.0 } else { 0.0 }), 0,
            f(eye.x), f(eye.y), f(eye.z), 0,
            f(back.x), f(back.y), f(back.z), 0,
        ];
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("render3d params"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
        let buffers = [&uniform, &tri_buf, &bin_buf, &surf_buf, &light_buf, &*texel_buf, &frame.color, &frame.extra, &frame.hits];
        let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("render3d"), layout: &bgl, entries: &entries });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w.div_ceil(16) as u32, h.div_ceil(16) as u32, 1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
    let out = frame.read(gpu, wants_kept);
    frame.give_back();
    out
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
            hits: make((3 * px + px.div_ceil(4)) * 16, "render3d hits", out),
            staging: make(Frame::sizes(w, h, stride).iter().sum(), "render3d read back", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
        }
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

    /// Wait for the render and copy its results out.
    fn read(&self, gpu: &Gpu, wants_kept: bool) -> Result<Readout> {
        let (w, h, stride) = self.key;
        let px = w * h;
        let [cb, db, eb, kb] = Frame::sizes(w, h, stride);
        let kb = if wants_kept { kb } else { 0 };
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.color, 0, &self.staging, 0, cb as u64);
        encoder.copy_buffer_to_buffer(&self.hits, (3 * px * 16) as u64, &self.staging, cb as u64, db as u64);
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
fn surf_table(surfs: &[Surface], tex_base: &[Option<(u32, u32)>], layout: &Layout) -> Vec<u32> {
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
        flags |= (s.holdout as u32) << 3 | (s.headlight as u32) << 4 | (s.two_sided as u32) << 5 | (s.wrap as u32) << 6;
        o[12] = flags;
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
    fn plan(surfs: &[Surface]) -> Textures {
        let mut images: Vec<(Arc<Image>, bool, u32)> = Vec::new();
        let (mut levels, mut texels) = (Vec::new(), 0usize);
        let base = surfs
            .iter()
            .map(|s| {
                let img = s.texture?;
                let mips = mip_sizes(img).len() as u32;
                if let Some(&(_, _, first)) = images.iter().find(|(i, wrap, _)| Arc::ptr_eq(i, img) && *wrap == s.wrap) {
                    return Some((first, mips));
                }
                let first = (levels.len() / LEVEL_WORDS) as u32;
                levels.extend_from_slice(&[texels as u32, img.format.width as u32, img.format.height as u32, img.x0 as u32, img.y0 as u32, img.width as u32, img.height as u32, 0]);
                texels += img.pixels.len();
                for (w, h) in mip_sizes(img) {
                    levels.extend_from_slice(&[texels as u32, w as u32, h as u32, 0, 0, w as u32, h as u32, 0]);
                    texels += w * h;
                }
                images.push((img.clone(), s.wrap, first));
                Some((first, mips))
            })
            .collect();
        Textures { levels, base, images, texels }
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
fn finish(tris: &[Tri], points: &[Vec3], cam: &Camera, next_cam: &Camera, format: Size, n: usize, passes: Passes, layout: &Layout, out: Readout) -> Image {
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
        let t = &tris[hit[0] as usize - 1];
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
            px[s.id_at] = layout.object[t.surf].map_or(0.0, |o| (o + 1) as f32);
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
