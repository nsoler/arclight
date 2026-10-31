//! Gaussian splats prepared for a view on the GPU (docs/gpu-plan.md, G6):
//! what `render3d::splats`'s `project_all` does, in `splat_view.wgsl`. Each
//! splat is seen through the camera (culled, its ellipse and color as
//! there), its tiles counted, each tile's list filled and sorted nearest
//! first (ties by splat: the same order every time). The view comes back
//! as the CPU's own [`View`], so everything after (blending, passes) is
//! as before. The captures' data goes up once and stays while they do.

#[cfg(test)]
use super::super::splats::{Projected, View};
use crate::geom::{self, Camera, Mat4, Vec3};
use crate::gpu::Gpu;
use crate::image::Size;
use crate::splat::Splats;
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::{Arc, Mutex, Weak};
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("splat_view.wgsl");
const TILE: usize = 16;
const SET_WORDS: usize = 24;
const PROJ_WORDS: usize = 12;
/// A splat's data before its SH coefficients: position, scales, rotation,
/// opacity.
const HEAD: usize = 11;

/// A view's splats prepared on the GPU and kept there, for the 3D
/// renderer's shader (raster.wgsl): each splat seen (`proj`: PROJ_WORDS a
/// splat, by `g`), and `lists`: each tile's start (tiles + 1 words, as
/// indexes into the pairs), then the (depth, splat) pairs, nearest first.
pub(in crate::render3d) struct GpuView {
    pub proj: wgpu::Buffer,
    pub lists: wgpu::Buffer,
    pub tiles: usize,
    /// How many (splat, tile) pairs there are: 0, nothing shows.
    pub pairs: usize,
    /// Each set's first splat (`g`).
    pub firsts: Vec<usize>,
}

/// `sets` seen through `cam` in a frame of `format`, colors up to SH
/// degree `degree`, prepared on the GPU and read back as the CPU's
/// [`View`] (for the tests, which compare it with the CPU's own); None
/// where it can't.
#[cfg(test)]
pub(in crate::render3d) fn project_all(sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Option<View> {
    let gpu = usable()?;
    match gpu.scoped(|| run(&gpu, sets, cam, format, degree)) {
        Ok(v) => v,
        Err(e) => {
            crate::gpu::report_error(format!("splats: {e:#}"), false);
            None
        }
    }
}

/// [`project_all`] kept on the GPU (nothing read back but how many pairs).
pub(in crate::render3d) fn prepare(sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Option<GpuView> {
    let gpu = usable()?;
    if format.width == 0 || format.height == 0 {
        return None;
    }
    match gpu.scoped(|| lists(&gpu, sets, cam, format, degree)) {
        Ok(v) => v.map(|(v, _)| v),
        Err(e) => {
            crate::gpu::report_error(format!("splats: {e:#}"), false);
            None
        }
    }
}

/// The GPU, if it may be used here.
pub(in crate::render3d) fn usable() -> Option<Arc<Gpu>> {
    #[cfg(test)]
    if !super::tests::GPU_ON.with(|on| on.get()) {
        return None;
    }
    crate::gpu::get()
}

/// How many of set `s`'s splats count (as the CPU counts them: every array
/// long enough).
fn count(s: &Splats) -> usize {
    let n = s.positions.len().min(s.scales.len()).min(s.rotations.len()).min(s.opacity.len());
    n.min(s.sh.len() / s.sh_stride().max(1))
}

/// The captures' data on the GPU, as the shader reads it, and where each
/// set's starts: kept while the same captures are drawn (the 3D view
/// orbiting, a scene re-rendered).
struct Uploaded {
    key: Vec<Key>,
    buffer: Arc<wgpu::Buffer>,
    offsets: Vec<usize>,
}

/// Which arrays a set's data came from.
type Key = (Weak<Vec<[f32; 3]>>, Weak<Vec<[f32; 3]>>, Weak<Vec<[f32; 4]>>, Weak<Vec<f32>>, Weak<Vec<f32>>, u8);

fn key_of(s: &Splats) -> Key {
    (Arc::downgrade(&s.positions), Arc::downgrade(&s.scales), Arc::downgrade(&s.rotations), Arc::downgrade(&s.opacity), Arc::downgrade(&s.sh), s.sh_degree)
}

fn same_key(a: &Key, b: &Key) -> bool {
    a.0.strong_count() > 0 && Weak::ptr_eq(&a.0, &b.0) && Weak::ptr_eq(&a.1, &b.1) && Weak::ptr_eq(&a.2, &b.2) && Weak::ptr_eq(&a.3, &b.3) && Weak::ptr_eq(&a.4, &b.4) && a.5 == b.5
}

static KEPT: Mutex<Option<Uploaded>> = Mutex::new(None);

/// `sets`' data on the GPU (uploaded, or kept from before) and where
/// each set's starts (in words).
fn upload(gpu: &Gpu, sets: &[Arc<Splats>]) -> (Arc<wgpu::Buffer>, Vec<usize>) {
    let key: Vec<Key> = sets.iter().map(|s| key_of(s)).collect();
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = kept.as_ref().filter(|u| u.key.len() == key.len() && u.key.iter().zip(&key).all(|(a, b)| same_key(a, b))) {
        return (u.buffer.clone(), u.offsets.clone());
    }
    let mut offsets = Vec::with_capacity(sets.len());
    let mut total = 0usize;
    for s in sets {
        offsets.push(total);
        total += count(s) * (HEAD + s.sh_stride());
    }
    let mut words = vec![0.0f32; total.max(1)];
    for (s, &at) in sets.iter().zip(&offsets) {
        let stride = HEAD + s.sh_stride();
        let n = count(s);
        words[at..at + n * stride].par_chunks_mut(stride).enumerate().for_each(|(i, o)| {
            o[..3].copy_from_slice(&s.positions[i]);
            o[3..6].copy_from_slice(&s.scales[i]);
            o[6..10].copy_from_slice(&s.rotations[i]);
            o[10] = s.opacity[i];
            o[HEAD..].copy_from_slice(s.sh_of(i));
        });
    }
    let buffer = Arc::new(gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("splat data"), contents: bytemuck::cast_slice(&words), usage: wgpu::BufferUsages::STORAGE }));
    *kept = Some(Uploaded { key, buffer: buffer.clone(), offsets: offsets.clone() });
    (buffer, offsets)
}

/// Workgroups to cover `n` (256 a group), as x (and y past 65 535).
fn groups(n: usize) -> (u32, u32) {
    let g = n.div_ceil(256).max(1);
    ((g.min(65535)) as u32, g.div_ceil(65535) as u32)
}

#[cfg(test)]
fn run(gpu: &Gpu, sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Result<Option<View>> {
    if format.width == 0 || format.height == 0 {
        return Ok(None);
    }
    let Some((v, starts)) = lists(gpu, sets, cam, format, degree)? else { return Ok(None) };
    let total: usize = sets.iter().map(|s| count(s)).sum();
    let [proj_words, pair_words, start_words] = read(gpu, [(&v.proj, 0, total * PROJ_WORDS * 4), (&v.lists, (v.tiles + 1) * 4, v.pairs * 8), (&starts, 0, (v.tiles + 1) * 4)])?;
    Ok(Some(view_of(sets, &proj_words, &pair_words, &start_words, format.width.div_ceil(TILE))))
}

/// The splats' lists made on the GPU (see [`GpuView`]), and the tiles'
/// starts on their own.
fn lists(gpu: &Gpu, sets: &[Arc<Splats>], cam: &Camera, format: Size, degree: u8) -> Result<Option<(GpuView, wgpu::Buffer)>> {
    let device = &gpu.device;
    let total: usize = sets.iter().map(|s| count(s)).sum();
    let limit = gpu.limits.max_storage_buffer_binding_size as usize;
    let data_words: usize = sets.iter().map(|s| count(s) * (HEAD + s.sh_stride())).sum();
    if total == 0 || total * PROJ_WORDS * 4 > limit || data_words * 4 > limit || sets.iter().any(|s| s.sh_degree > 3) {
        return Ok(None);
    }
    let (data, offsets) = upload(gpu, sets);
    let view = cam.view();
    let eye = geom::transform_point(&cam.world, &Vec3::zeros());
    let f = |v: f64| (v as f32).to_bits();
    let mut table = vec![0u32; sets.len() * SET_WORDS];
    let mut first = 0usize;
    for ((o, s), &at) in table.chunks_mut(SET_WORDS).zip(sets).zip(&offsets) {
        let to_cam = view * s.world;
        let own = s.world.try_inverse().unwrap_or_else(Mat4::identity);
        let eye_own = if cam.is_ortho() { own.fixed_view::<3, 3>(0, 0).into_owned() * -cam.to_eye(&Vec3::zeros()) } else { geom::transform_point(&own, &eye) };
        for r in 0..3 {
            for c in 0..4 {
                o[4 * r + c] = f(to_cam[(r, c)]);
            }
        }
        (0..3).for_each(|k| o[12 + k] = f(eye_own[k]));
        o[15] = first as u32;
        o[16] = count(s) as u32;
        o[17] = at as u32;
        o[18] = s.sh_degree as u32;
        o[19] = degree.min(s.sh_degree) as u32;
        first += count(s);
    }
    let (tiles_x, tiles_y) = (format.width.div_ceil(TILE), format.height.div_ceil(TILE));
    let tiles = tiles_x * tiles_y;
    let focal = if cam.is_ortho() { cam.pixels_per_unit(1.0, format) } else { cam.focal_pixels(format) };
    #[rustfmt::skip]
    let params: [u32; 12] = [
        format.width as u32, format.height as u32, tiles_x as u32, tiles_y as u32,
        total as u32, sets.len() as u32, cam.is_ortho() as u32, tiles as u32,
        f(focal), f(cam.near), f(cam.far), 0,
    ];
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("splat params"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
    let table_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("splat sets"), contents: bytemuck::cast_slice(&table), usage: wgpu::BufferUsages::STORAGE });
    let rw = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let buffer = |bytes: usize, label: &str| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16) as u64, usage: rw, mapped_at_creation: false });
    let proj = buffer(total * PROJ_WORDS * 4, "splat projected");
    let counts = buffer(tiles * 4, "splat tile counts");
    let starts = buffer((tiles + 1) * 4, "splat tile starts");
    let cursors = buffer(tiles * 4, "splat tile cursors");
    // (The pairs' buffer isn't known yet: a stand-in for the first passes.)
    let none = buffer(16, "splat pairs (none yet)");
    let pass = |encoder: &mut wgpu::CommandEncoder, entry: &'static str, pairs: &wgpu::Buffer, size: (u32, u32)| {
        let pipeline = gpu.compute_pipeline("render3d/splat_view", SHADER, entry);
        let all: [(u32, &wgpu::Buffer); 8] = [(0, &uniform), (1, &table_buf), (2, &data), (3, &proj), (4, &counts), (5, &starts), (6, &cursors), (7, pairs)];
        let used: &[u32] = match entry {
            "project" => &[0, 1, 2, 3, 4],
            "offsets" => &[0, 4, 5, 6],
            "scatter" => &[0, 3, 5, 6, 7],
            _ => &[0, 5, 7],
        };
        let entries: Vec<wgpu::BindGroupEntry> = all.iter().filter(|(b, _)| used.contains(b)).map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }).collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut p = encoder.begin_compute_pass(&Default::default());
        p.set_pipeline(&pipeline);
        p.set_bind_group(0, &group, &[]);
        p.dispatch_workgroups(size.0, size.1, 1);
    };
    // Project and count, then the tiles' starts; the total read back to
    // size the lists.
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.clear_buffer(&counts, 0, None);
    pass(&mut encoder, "project", &none, groups(total));
    pass(&mut encoder, "offsets", &none, (1, 1));
    gpu.queue.submit([encoder.finish()]);
    let [last] = read(gpu, [(&starts, tiles * 4, 4)])?;
    let pairs_n = last[0] as usize;
    if pairs_n * 8 > limit {
        return Ok(None);
    }
    let pairs = buffer(pairs_n * 8, "splat pairs");
    let mut encoder = device.create_command_encoder(&Default::default());
    pass(&mut encoder, "scatter", &pairs, groups(total));
    let (tx, ty) = (tiles.min(65535) as u32, tiles.div_ceil(65535) as u32);
    pass(&mut encoder, "sort_tiles", &pairs, (tx, ty));
    // The starts, then the pairs, in one buffer for the renderer.
    let lists = buffer((tiles + 1) * 4 + pairs_n * 8, "splat lists");
    encoder.copy_buffer_to_buffer(&starts, 0, &lists, 0, ((tiles + 1) * 4) as u64);
    if pairs_n > 0 {
        encoder.copy_buffer_to_buffer(&pairs, 0, &lists, ((tiles + 1) * 4) as u64, (pairs_n * 8) as u64);
    }
    gpu.queue.submit([encoder.finish()]);
    let mut firsts = Vec::with_capacity(sets.len());
    let mut first = 0usize;
    for s in sets {
        firsts.push(first);
        first += count(s);
    }
    Ok(Some((GpuView { proj, lists, tiles, pairs: pairs_n, firsts }, starts)))
}

/// The CPU's [`View`] from what the GPU worked out: the splats that show,
/// in order, and each tile's list of them.
#[cfg(test)]
fn view_of(sets: &[Arc<Splats>], proj: &[u32], pairs: &[u32], starts: &[u32], tiles_x: usize) -> View {
    let total = proj.len() / PROJ_WORDS;
    let shows: Vec<u32> = (0..total).into_par_iter().map(|g| (proj[g * PROJ_WORDS + 11] != 0) as u32).collect();
    // Each shown splat's place among those shown.
    let mut index = vec![0u32; total];
    let mut at = 0u32;
    for (g, s) in shows.iter().enumerate() {
        index[g] = at;
        at += s;
    }
    let mut firsts = Vec::with_capacity(sets.len());
    let mut first = 0usize;
    for s in sets {
        firsts.push(first);
        first += count(s);
    }
    let fl = |w: u32| f32::from_bits(w);
    let splats: Vec<Projected> = (0..total)
        .into_par_iter()
        .filter(|&g| shows[g] != 0)
        .map(|g| {
            let o = &proj[g * PROJ_WORDS..(g + 1) * PROJ_WORDS];
            let set = o[11] - 1;
            Projected { center: [fl(o[0]), fl(o[1])], conic: [fl(o[2]), fl(o[3]), fl(o[4])], depth: fl(o[5]), color: [fl(o[6]), fl(o[7]), fl(o[8])], opacity: fl(o[9]), set, src: (g - firsts[set as usize]) as u32 }
        })
        .collect();
    let order: Vec<u32> = pairs.par_chunks(2).map(|p| index[p[1] as usize]).collect();
    let ranges = starts.windows(2).map(|w| (w[0], w[1])).collect();
    View { splats, tiles_x, ranges, order }
}

/// `bufs`' bytes (from each offset, so many) as 32-bit words.
fn read<const N: usize>(gpu: &Gpu, bufs: [(&wgpu::Buffer, usize, usize); N]) -> Result<[Vec<u32>; N]> {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let staging: Vec<wgpu::Buffer> = bufs
        .iter()
        .map(|(b, from, len)| {
            let s = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("splats read back"), size: (*len).max(4) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            if *len > 0 {
                encoder.copy_buffer_to_buffer(b, *from as u64, &s, 0, *len as u64);
            }
            s
        })
        .collect();
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    for s in &staging {
        let tx = tx.clone();
        s.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
    }
    drop(tx);
    gpu.wait()?;
    for r in rx.iter() {
        r.map_err(|e| anyhow!("reading the splats back: {e}"))?;
    }
    let mut out: [Vec<u32>; N] = std::array::from_fn(|_| Vec::new());
    for (k, (s, (_, _, len))) in staging.iter().zip(&bufs).enumerate() {
        let mut words = vec![0u32; len / 4];
        {
            let bytes = s.slice(..).get_mapped_range().map_err(|e| anyhow!("reading the splats back: {e:?}"))?;
            super::copy_out(&bytes[..*len], bytemuck::cast_slice_mut(&mut words));
        }
        s.unmap();
        out[k] = words;
    }
    Ok(out)
}
