//! Particle sprites on the GPU: what `particles_render`'s `draw` does, one
//! thread a pixel (`sprites.wgsl`). The sprites arrive projected and
//! sorted as that function sorts them; here they're binned into 16 × 16
//! tiles in that order, so each pixel lays its sprites in the same order
//! as the CPU does.

use crate::color::Rgba;
use crate::gpu::Gpu;
use crate::image::{Image, Size};
use crate::particles_render::{Blend, Outline, Splat, Sprites};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::Arc;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("sprites.wgsl");
const TILE: usize = 16;
const SPRITE_WORDS: usize = 12;
const NONE: u32 = u32::MAX;

/// `splats` (sorted) drawn as `sprites` into a frame of `format` that
/// starts as `color` and `depth` (the background's): the frame's colors
/// and depths, or None where the GPU can't (then the CPU draws them).
pub(crate) fn draw(splats: &[Splat], sprites: &Sprites, format: Size, color: &[Rgba], depth: &[f32], with_depth: bool) -> Option<(Vec<Rgba>, Vec<f32>)> {
    #[cfg(test)]
    if !super::tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    if splats.is_empty() || format.width == 0 || format.height == 0 {
        return None;
    }
    // Small sprites are cheap on the CPU, which pays nothing to get the
    // frame there and back: the GPU only where they cover more than twice
    // the frame (big or many: smoke, dust; a 1080p frame costs the GPU
    // ~20-40 ms whatever it holds).
    let px = format.width * format.height;
    let square = sprites.outline == Outline::Square;
    let covered: f64 = splats.par_iter().map(|s| bounds(s, square, format.width, format.height).map_or(0.0, |b| ((b[2] - b[0] + 1) * (b[3] - b[1] + 1)) as f64)).sum();
    if covered < 2.0 * px as f64 {
        return None;
    }
    let gpu = crate::gpu::get()?;
    let limit = gpu.limits.max_storage_buffer_binding_size as usize;
    if px * 16 > limit {
        return None;
    }
    match gpu.scoped(|| run(&gpu, splats, sprites, format, color, depth, with_depth)) {
        Ok(out) => Some(out),
        Err(e) => {
            crate::gpu::report_error(format!("particle sprites: {e:#}"), false);
            None
        }
    }
}

/// The pixels a sprite may touch, as the CPU loops over them: x0..=x1,
/// y0..=y1, clipped to the frame (None: none of them).
fn bounds(s: &Splat, square: bool, w: usize, h: usize) -> Option<[usize; 4]> {
    let r = s.radius.max(0.5);
    let reach = if square { r * std::f64::consts::SQRT_2 } else { r };
    let x0 = ((s.at[0] - reach).floor() as i64).max(0);
    let y0 = ((s.at[1] - reach).floor() as i64).max(0);
    let x1 = ((s.at[0] + reach).ceil() as i64).min(w as i64 - 1);
    let y1 = ((s.at[1] + reach).ceil() as i64).min(h as i64 - 1);
    (x0 <= x1 && y0 <= y1).then_some([x0 as usize, y0 as usize, x1 as usize, y1 as usize])
}

/// Each tile's sprites in drawing order: tile starts (one more than there
/// are tiles, as indexes into the whole array), then the lists.
fn bin(boxes: &[Option<[usize; 4]>], tiles_x: usize, tiles_y: usize) -> Vec<u32> {
    let tiles = tiles_x * tiles_y;
    let span = |b: &[usize; 4]| (b[0] / TILE, b[1] / TILE, b[2] / TILE, b[3] / TILE);
    let mut counts = vec![0u32; tiles + 1];
    for b in boxes.iter().flatten() {
        let (tx0, ty0, tx1, ty1) = span(b);
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
    for (i, b) in boxes.iter().enumerate() {
        let Some(b) = b else { continue };
        let (tx0, ty0, tx1, ty1) = span(b);
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

fn run(gpu: &Gpu, splats: &[Splat], sprites: &Sprites, format: Size, color: &[Rgba], depth: &[f32], with_depth: bool) -> Result<(Vec<Rgba>, Vec<f32>)> {
    let device = &gpu.device;
    let (w, h) = (format.width, format.height);
    let px = w * h;
    let square = sprites.outline == Outline::Square;
    let boxes: Vec<Option<[usize; 4]>> = splats.par_iter().map(|s| bounds(s, square, w, h)).collect();
    let (tiles_x, tiles_y) = (w.div_ceil(TILE), h.div_ceil(TILE));
    let bins = bin(&boxes, tiles_x, tiles_y);
    let mut words = vec![0u32; splats.len() * SPRITE_WORDS];
    words.par_chunks_mut(SPRITE_WORDS).zip(splats).for_each(|(o, s)| {
        let f = |v: f64| (v as f32).to_bits();
        // Smaller than a pixel: drawn a pixel wide, fainter to keep its
        // total (as the CPU does).
        let faint = if s.radius < 0.5 { s.radius * s.radius / (0.25 + 1.0 / 12.0) } else { 1.0 };
        o[0] = f(s.at[0]);
        o[1] = f(s.at[1]);
        o[2] = f(s.radius.max(0.5));
        o[3] = f(faint);
        o[4] = f(s.depth);
        o[5] = f(s.turn.cos());
        o[6] = f(s.turn.sin());
        for c in 0..4 {
            o[7 + c] = s.color[c].to_bits();
        }
        o[11] = s.image.map_or(NONE, |i| i as u32);
    });
    // The texture's images, as the shader reads them.
    let found: &[Arc<Image>] = sprites.texture.as_ref().map_or(&[], |t| &t.images);
    let (mut table, mut texels) = (Vec::new(), Vec::new());
    for img in found {
        table.extend_from_slice(&[texels.len() as u32, img.format.width as u32, img.format.height as u32, img.x0 as u32, img.y0 as u32, img.width as u32, img.height as u32, 0]);
        texels.extend_from_slice(&img.pixels);
    }
    // Images past the texture's (a pick out of range) draw untextured, as
    // `images.get(k)` does on the CPU.
    words.par_chunks_mut(SPRITE_WORDS).for_each(|o| {
        if o[11] != NONE && o[11] as usize >= found.len() {
            o[11] = NONE;
        }
    });
    let read = |bytes: &[u8], label: &str| {
        let bytes = if bytes.is_empty() { &[0u8; 16][..] } else { bytes };
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytes, usage: wgpu::BufferUsages::STORAGE })
    };
    let sprite_buf = read(bytemuck::cast_slice(&words), "sprites");
    let bin_buf = read(bytemuck::cast_slice(&bins), "sprite tiles");
    let image_buf = read(bytemuck::cast_slice(&table), "sprite images");
    let texel_buf = read(bytemuck::cast_slice(&texels), "sprite texels");
    let out = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
    let color_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("sprite color"), contents: bytemuck::cast_slice(color), usage: out });
    let depth_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("sprite depth"), contents: bytemuck::cast_slice(depth), usage: out });
    let flags = (with_depth as u32) | ((sprites.blend == Blend::Additive) as u32) << 1 | (square as u32) << 2;
    let params = [w as u32, h as u32, tiles_x as u32, flags];
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("sprite params"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
    let pipeline = gpu.compute_pipeline("render3d/sprites", SHADER, "main");
    let buffers = [&uniform, &sprite_buf, &bin_buf, &image_buf, &texel_buf, &color_buf, &depth_buf];
    let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("sprites"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sprites read back"), size: (px * 20) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(tiles_x as u32, tiles_y as u32, 1);
    }
    encoder.copy_buffer_to_buffer(&color_buf, 0, &staging, 0, (px * 16) as u64);
    encoder.copy_buffer_to_buffer(&depth_buf, 0, &staging, (px * 16) as u64, (px * 4) as u64);
    gpu.queue.submit([encoder.finish()]);
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.wait()?;
    rx.recv().map_err(|_| anyhow!("reading the sprites back: no answer"))?.map_err(|e| anyhow!("reading the sprites back: {e}"))?;
    let (mut colors, mut depths) = (vec![[0.0f32; 4]; px], vec![0.0f32; px]);
    {
        let bytes = slice.get_mapped_range().map_err(|e| anyhow!("reading the sprites back: {e:?}"))?;
        super::copy_out(&bytes[..px * 16], bytemuck::cast_slice_mut(&mut colors));
        super::copy_out(&bytes[px * 16..px * 20], bytemuck::cast_slice_mut(&mut depths));
    }
    staging.unmap();
    Ok((colors, depths))
}
