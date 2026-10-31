//! The viewer's display of a result still on the GPU: what the viewer's
//! `to_color_image` does on the CPU (8-bit sRGB, exposure, a channel,
//! overscan dimmed), done by a kernel into a texture egui draws directly,
//! so a GPU result needs no conversion and no upload on the UI thread.

use super::{Gpu, GpuImage};
use crate::image::{IRect, Size};
use anyhow::Result;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("display.wgsl");

/// Which channels are shown (the viewer's, less its planes view, which
/// stays on the CPU), or the 3D view's picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Rgb,
    Red,
    Green,
    Blue,
    Alpha,
    /// The 3D view's render: its color sRGB-encoded and premultiplied in 8
    /// bits, alpha kept (no exposure, nothing dimmed).
    Premultiplied,
}

/// A texture to display into, `width` × `height` texels, `Rgba8Unorm`
/// holding the sRGB-encoded bytes as computed: what egui's own textures
/// hold (its shader works on them as they are) and what it requires of a
/// native one.
pub fn texture(gpu: &Gpu, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewer"),
        size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// The view egui draws `texture` through: sampled only.
pub fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor { usage: Some(wgpu::TextureUsages::TEXTURE_BINDING), ..Default::default() })
}

/// Display `img` into `target` (sized `region` / `step`, rounded up):
/// `region` in frame pixels, exposure in stops, each texel the mean of a
/// `step` × `step` block.
pub fn draw(gpu: &Gpu, img: &GpuImage, region: IRect, channel: Channel, exposure: f32, step: usize, target: &wgpu::Texture) -> Result<()> {
    let size = [target.width(), target.height()];
    let i = |v: i32| v as u32;
    let w = img.window;
    let params: [u32; 16] = [
        i(w.x0),
        i(w.y0),
        w.width() as u32,
        w.height() as u32,
        i(region.x0),
        i(region.y0),
        i(region.x1),
        i(region.y1),
        img.format.width as u32,
        img.format.height as u32,
        step.max(1) as u32,
        match channel {
            Channel::Rgb => 0,
            Channel::Red => 1,
            Channel::Green => 2,
            Channel::Blue => 3,
            Channel::Alpha => 4,
            Channel::Premultiplied => 5,
        },
        size[0],
        size[1],
        2f32.powf(exposure).to_bits(),
        0,
    ];
    gpu.scoped(|| {
        let pipeline = gpu.compute_pipeline("gpu/display", SHADER, "display");
        let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("display"), contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM });
        let view = target.create_view(&Default::default());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: img.rgba.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        super::image::dispatch_2d(&mut encoder, &pipeline, &bind, IRect::frame(Size { width: size[0] as usize, height: size[1] as usize }));
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    })
}

/// `texture`'s texels, row by row, 4 bytes each (for tests).
pub fn read_texture(gpu: &Gpu, texture: &wgpu::Texture) -> Result<Vec<u8>> {
    let (w, h) = (texture.width(), texture.height());
    // Rows are copied 256-byte aligned.
    let row = (w * 4).next_multiple_of(256);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("texture read"), size: (row * h) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    gpu.wait()?;
    rx.recv()??;
    let view = buffer.get_mapped_range(..)?;
    Ok((0..h as usize).flat_map(|y| view[y * row as usize..y * row as usize + w as usize * 4].to_vec()).collect())
}
