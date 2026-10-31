//! Images on the GPU: [`GpuImage`] mirrors [`Image`] with storage buffers
//! in its exact layout (row-major `vec4<f32>` over the data window, one
//! `f32` buffer per extra plane), so moving one is a single copy each way
//! with no row padding. Bilinear sampling and "transparent outside the
//! window" are done in WGSL by hand; windows stay CPU-side metadata.
//!
//! Two ways to move pixels ([`Transfer`]), chosen at device creation and
//! hidden behind [`GpuImage::upload`] / [`GpuImage::read_back`]: staging
//! copies (discrete GPUs: the buffers live in video memory) or shared
//! memory (integrated GPUs such as Apple Silicon: buffers the CPU maps
//! directly, one memcpy each way).

use super::Gpu;
use crate::image::{IRect, Image, Plane, Size};
use crate::scene::Scene;
use anyhow::{anyhow, bail, Result};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// How pixels move between [`Image`] and [`GpuImage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// Through a staging buffer and a copy pass (discrete GPUs).
    Staging,
    /// Into and out of mapped storage buffers (unified memory).
    Unified,
}

impl Gpu {
    /// The transfer path images use on this device.
    pub fn transfer_path(&self) -> Transfer {
        if self.unified { Transfer::Unified } else { Transfer::Staging }
    }
}

/// An [`Image`] on the GPU: the same frame, data window, planes and scene.
/// Cloning shares the buffers (results are never changed in place).
#[derive(Clone)]
pub struct GpuImage {
    pub format: Size,
    pub window: IRect,
    /// `window.width() × window.height()` pixels, `vec4<f32>` each.
    pub rgba: wgpu::Buffer,
    /// Extra planes, one `f32` per pixel of the window, in `Image`'s order.
    pub planes: Vec<(String, wgpu::Buffer)>,
    pub scene: Option<Arc<Scene>>,
    /// How its buffers were made (shared ones can be mapped).
    pub transfer: Transfer,
}

/// The WGSL of [`GpuImage`]'s own kernels.
const SHADER: &str = include_str!("image.wgsl");

/// Pixels per side of a 2D workgroup (16 × 16 = 256 threads, as every
/// adapter allows).
pub const TILE: u32 = 16;

impl GpuImage {
    /// `img` on the GPU, by the device's transfer path.
    pub fn upload(gpu: &Gpu, img: &Image) -> Result<GpuImage> {
        Self::upload_via(gpu, img, gpu.transfer_path())
    }

    /// [`upload`](Self::upload) by a given path (`Unified` needs
    /// `MAPPABLE_PRIMARY_BUFFERS`).
    pub fn upload_via(gpu: &Gpu, img: &Image, transfer: Transfer) -> Result<GpuImage> {
        gpu.scoped(|| {
            let rgba = buffer_with(gpu, "image rgba", bytemuck::cast_slice(&img.pixels), transfer)?;
            let planes = img.planes.iter().map(|p| Ok((p.name.clone(), buffer_with(gpu, "image plane", bytemuck::cast_slice(&p.data), transfer)?))).collect::<Result<_>>()?;
            Ok(GpuImage { format: img.format, window: img.window(), rgba, planes, scene: img.scene.clone(), transfer })
        })
    }

    /// A transparent image of frame `format` over `window`, planes named
    /// `planes` (all zero).
    pub fn blank(gpu: &Gpu, format: Size, window: IRect, planes: &[String]) -> Result<GpuImage> {
        let transfer = gpu.transfer_path();
        let n = window.width() * window.height();
        gpu.scoped(|| {
            let rgba = storage_buffer(gpu, "image rgba", (n * 16) as u64, transfer, false)?;
            let planes = planes.iter().map(|name| Ok((name.clone(), storage_buffer(gpu, "image plane", (n * 4) as u64, transfer, false)?))).collect::<Result<_>>()?;
            Ok(GpuImage { format, window, rgba, planes, scene: None, transfer })
        })
    }

    /// Pixels in the data window.
    pub fn len(&self) -> usize {
        self.window.width() * self.window.height()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes held on the GPU (pixels and planes).
    pub fn bytes(&self) -> u64 {
        self.len() as u64 * (16 + 4 * self.planes.len() as u64)
    }

    /// Back to an [`Image`], planes and scene included.
    pub fn read_back(&self, gpu: &Gpu) -> Result<Image> {
        let n = self.len();
        let mut img = Image::blank(self.format, self.window);
        img.planes = self.planes.iter().map(|(name, _)| Plane { name: name.clone(), data: vec![0.0; n] }).collect();
        img.scene = self.scene.clone();
        if n == 0 {
            return Ok(img);
        }
        let buffers: Vec<&wgpu::Buffer> = std::iter::once(&self.rgba).chain(self.planes.iter().map(|(_, b)| b)).collect();
        let mut targets: Vec<&mut [u8]> = std::iter::once(bytemuck::cast_slice_mut(&mut img.pixels)).chain(img.planes.iter_mut().map(|p| bytemuck::cast_slice_mut(&mut p.data))).collect();
        gpu.scoped(|| {
            match self.transfer {
                Transfer::Unified => {
                    map_all(gpu, &buffers, wgpu::MapMode::Read)?;
                    for (b, t) in buffers.iter().zip(targets.iter_mut()) {
                        copy_out(b, t)?;
                    }
                }
                Transfer::Staging => {
                    let mut encoder = gpu.device.create_command_encoder(&Default::default());
                    let staging: Vec<wgpu::Buffer> = buffers
                        .iter()
                        .zip(targets.iter())
                        .map(|(b, t)| {
                            let s = gpu.take_staging(t.len() as u64);
                            encoder.copy_buffer_to_buffer(b, 0, &s, 0, t.len() as u64);
                            s
                        })
                        .collect();
                    gpu.queue.submit([encoder.finish()]);
                    let refs: Vec<&wgpu::Buffer> = staging.iter().collect();
                    map_all(gpu, &refs, wgpu::MapMode::Read)?;
                    for (b, t) in refs.iter().zip(targets.iter_mut()) {
                        copy_out(b, t)?;
                    }
                    for s in staging {
                        gpu.give_staging(s);
                    }
                }
            }
            Ok(())
        })?;
        Ok(img)
    }

    /// The same pixels over data window `window` (transparent where this
    /// one has none): what `generate(format, window, |x, y| src.at(x, y))`
    /// is on the CPU. Planes come along.
    pub fn rewindow(&self, gpu: &Gpu, window: IRect) -> Result<GpuImage> {
        if window == self.window {
            return Ok(self.clone());
        }
        let rgba = rewindowed(gpu, "rewindow", self.window, &self.rgba, window, 16)?;
        let planes = self.planes.iter().map(|(n, b)| Ok((n.clone(), rewindowed(gpu, "rewindow_plane", self.window, b, window, 4)?))).collect::<Result<_>>()?;
        Ok(GpuImage { format: self.format, window, rgba, planes, scene: self.scene.clone(), transfer: gpu.transfer_path() })
    }

    /// `src`'s planes over this image's window (shared when the windows
    /// match): `Image::keep_planes`.
    pub fn keep_planes(&mut self, gpu: &Gpu, src: &GpuImage) -> Result<()> {
        self.planes = if self.window == src.window {
            src.planes.clone()
        } else {
            src.planes.iter().map(|(n, b)| Ok((n.clone(), rewindowed(gpu, "rewindow_plane", src.window, b, self.window, 4)?))).collect::<Result<_>>()?
        };
        Ok(())
    }

    /// The buffer of plane `name`, if it has one.
    pub fn plane(&self, name: &str) -> Option<&wgpu::Buffer> {
        self.planes.iter().find(|(n, _)| n == name).map(|(_, b)| b)
    }
}

/// A small buffer to bind where a kernel's input is absent (its window
/// is then empty, so it is never read).
pub fn placeholder(gpu: &Gpu) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("absent"), size: 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false })
}

/// `src` (pixels over `from`, `stride` bytes each: 16 for color, 4 for a
/// plane) over window `to`, by `entry` of image.wgsl.
fn rewindowed(gpu: &Gpu, entry: &'static str, from: IRect, src: &wgpu::Buffer, to: IRect, stride: u64) -> Result<wgpu::Buffer> {
    let dst = storage_buffer(gpu, "rewindowed", (to.width() * to.height()) as u64 * stride, gpu.transfer_path(), false)?;
    if to.is_empty() {
        return Ok(dst);
    }
    let windows: [i32; 8] = [from.x0, from.y0, from.width() as i32, from.height() as i32, to.x0, to.y0, to.width() as i32, to.height() as i32];
    let slots = if stride == 16 { [1, 2] } else { [3, 4] };
    gpu.scoped(|| {
        let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("windows"), contents: bytemuck::cast_slice(&windows), usage: wgpu::BufferUsages::UNIFORM });
        let pipeline = gpu.compute_pipeline("gpu/image", SHADER, entry);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(entry),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: slots[0], resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: slots[1], resource: dst.as_entire_binding() },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        dispatch_2d(&mut encoder, &pipeline, &bind, to);
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    })?;
    Ok(dst)
}

/// One pass of `pipeline` over `window`, a thread per pixel.
pub fn dispatch_2d(encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, bind: &wgpu::BindGroup, window: IRect) {
    let mut cp = encoder.begin_compute_pass(&Default::default());
    cp.set_pipeline(pipeline);
    cp.set_bind_group(0, bind, &[]);
    cp.dispatch_workgroups((window.width() as u32).div_ceil(TILE), (window.height() as u32).div_ceil(TILE), 1);
}

/// A storage buffer of `size` bytes (at least 16: wgpu binds no empty
/// buffer) that can be copied both ways, and mapped when `transfer` is
/// `Unified`; mapped for writing at creation when `mapped`. Refused when
/// over the device's limits (the caller falls back to the CPU).
pub(crate) fn storage_buffer(gpu: &Gpu, label: &str, size: u64, transfer: Transfer, mapped: bool) -> Result<wgpu::Buffer> {
    let limit = gpu.limits.max_buffer_size.min(gpu.limits.max_storage_buffer_binding_size);
    if size > limit {
        bail!("image too large for the GPU ({} MiB, limit {} MiB)", size >> 20, limit >> 20);
    }
    let mut usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    if transfer == Transfer::Unified {
        if !gpu.features.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS) {
            bail!("this GPU can't map storage buffers");
        }
        usage |= wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE;
    }
    Ok(gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(16), usage, mapped_at_creation: mapped }))
}

/// A storage buffer holding `data`.
fn buffer_with(gpu: &Gpu, label: &str, data: &[u8], transfer: Transfer) -> Result<wgpu::Buffer> {
    match transfer {
        Transfer::Unified => {
            let buf = storage_buffer(gpu, label, data.len() as u64, transfer, true)?;
            buf.get_mapped_range_mut(..)?.slice(..data.len()).copy_from_slice(data);
            buf.unmap();
            Ok(buf)
        }
        Transfer::Staging => {
            let buf = storage_buffer(gpu, label, data.len() as u64, transfer, false)?;
            if !data.is_empty() {
                gpu.queue.write_buffer(&buf, 0, data);
            }
            Ok(buf)
        }
    }
}

/// Map every buffer whole and wait until all are.
fn map_all(gpu: &Gpu, buffers: &[&wgpu::Buffer], mode: wgpu::MapMode) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    for b in buffers {
        let tx = tx.clone();
        b.map_async(mode, .., move |r| {
            let _ = tx.send(r);
        });
    }
    gpu.wait()?;
    for _ in buffers {
        rx.recv()?.map_err(|e| anyhow!("GPU buffer map failed: {e}"))?;
    }
    Ok(())
}

/// Copy a mapped buffer's first `target.len()` bytes out, then unmap it.
fn copy_out(buffer: &wgpu::Buffer, target: &mut [u8]) -> Result<()> {
    {
        let view = buffer.get_mapped_range(..)?;
        target.copy_from_slice(&view[..target.len()]);
    }
    buffer.unmap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::check;

    fn device() -> Option<Arc<Gpu>> {
        let gpu = crate::gpu::test_device();
        if gpu.is_none() {
            eprintln!("skipped: no GPU adapter");
        }
        gpu
    }

    /// Up and back unchanged by both transfer paths: pixels, a plane and
    /// the scene, an offset odd-sized window, and an empty one.
    #[test]
    fn images_go_up_and_come_back_unchanged() {
        let Some(gpu) = device() else { return };
        let img = check::noise(Size { width: 300, height: 200 }, IRect::new(-7, 13, 290, 211), true, 5);
        let mut paths = vec![Transfer::Staging];
        if gpu.features.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS) {
            paths.push(Transfer::Unified);
        }
        for transfer in paths {
            let up = GpuImage::upload_via(&gpu, &img, transfer).unwrap();
            assert_eq!((up.len(), up.bytes()), (297 * 198, 297 * 198 * 20));
            let back = up.read_back(&gpu).unwrap();
            assert_eq!((back.format, back.window(), back.planes.clone()), (img.format, img.window(), img.planes.clone()), "{transfer:?}");
            assert!(back.pixels == img.pixels, "{transfer:?}");
        }
        let empty = Image::blank(img.format, IRect::default());
        let back = GpuImage::upload(&gpu, &empty).unwrap().read_back(&gpu).unwrap();
        assert!(back.pixels.is_empty() && back.window().is_empty());
    }

    /// `rewindow` is `Image::at` over the new window: grown, shrunk and
    /// moved off the data, planes too.
    #[test]
    fn rewindow_matches_the_cpu() {
        let Some(gpu) = device() else { return };
        let img = check::noise(Size { width: 120, height: 90 }, IRect::new(10, 5, 101, 77), true, 9);
        let up = GpuImage::upload(&gpu, &img).unwrap();
        for window in [IRect::new(-20, -10, 140, 100), IRect::new(30, 30, 47, 41), IRect::new(200, 0, 233, 9), IRect::default()] {
            let got = up.rewindow(&gpu, window).unwrap().read_back(&gpu).unwrap();
            let want = check::rewindowed(&img, window);
            assert_eq!(got.window(), window);
            assert!(got.pixels == want.pixels && got.planes == want.planes, "{window:?}");
        }
    }

    /// An image over the device's buffer limit is refused (the node then
    /// runs on the CPU), not a wgpu panic.
    #[test]
    fn an_image_too_large_is_refused() {
        let Some(gpu) = device() else { return };
        let limit = gpu.limits.max_buffer_size.min(gpu.limits.max_storage_buffer_binding_size);
        let err = storage_buffer(&gpu, "big", limit + 16, Transfer::Staging, false).err().unwrap();
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
