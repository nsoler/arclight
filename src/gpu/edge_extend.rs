//! Edge Extend on the GPU (`edge_extend::edge_extend`): the same
//! pull-push passes as the CPU, a dispatch each, in edge_extend.wgsl.

use super::image::{dispatch_2d, placeholder, storage_buffer, GpuImage};
use super::Gpu;
use crate::edge_extend::{self, EdgeExtend};
use crate::image::IRect;
use anyhow::Result;
use serde_json::Value;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("edge_extend.wgsl");

/// A data window as the shader takes it: x0, y0, width, height.
fn win(r: IRect) -> [i32; 4] {
    [r.x0, r.y0, r.width() as i32, r.height() as i32]
}

/// edge_extend.wgsl's `Ext`.
#[derive(Default, Clone, Copy)]
struct Ext {
    src_win: [i32; 4],
    matte_win: [i32; 4],
    region: [i32; 4],
    dims: [u32; 4],
    flags: [u32; 4],
    more: [u32; 4],
}

impl Ext {
    fn bytes(&self) -> Vec<u32> {
        let i = |v: [i32; 4]| v.map(|x| x as u32);
        [i(self.src_win), i(self.matte_win), i(self.region), self.dims, self.flags, self.more].concat()
    }
}

/// `entry` over a `w` × `h` output (a new buffer), with these buffers at
/// their bindings (1 src, 2 matte, 3 a, 4 b): just the ones `entry` uses,
/// as its layout (derived from the shader) holds no others.
fn run(gpu: &Gpu, e: &Ext, entry: &'static str, (w, h): (usize, usize), buffers: &[(u32, &wgpu::Buffer)]) -> Result<wgpu::Buffer> {
    let dst = storage_buffer(gpu, entry, (w * h * 16) as u64, gpu.transfer_path(), false)?;
    if w == 0 || h == 0 {
        return Ok(dst);
    }
    let pipeline = gpu.compute_pipeline("gpu/edge_extend", SHADER, entry);
    gpu.scoped(|| {
        let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("edge_extend"), contents: bytemuck::cast_slice(&e.bytes()), usage: wgpu::BufferUsages::UNIFORM });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }, wgpu::BindGroupEntry { binding: 5, resource: dst.as_entire_binding() }];
        entries.extend(buffers.iter().map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }));
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        dispatch_2d(&mut encoder, &pipeline, &bind, IRect::new(0, 0, w as i32, h as i32));
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    })?;
    Ok(dst)
}

/// Edge Extend of `src` (with an optional `matte`) as on the CPU. None
/// when its params don't parse (the CPU then says why).
pub(crate) fn eval(gpu: &Gpu, params: &Value, src: &GpuImage, matte: Option<&GpuImage>) -> Result<Option<GpuImage>> {
    let Ok(p) = crate::ops::parse::<EdgeExtend>(params, "edge_extend") else { return Ok(None) };
    let Ok(plan) = edge_extend::plan(&p, src.format, src.window) else { return Ok(None) };
    let r = plan.region;
    let out = |rgba| GpuImage { format: src.format, window: r, rgba, planes: Vec::new(), scene: None, transfer: gpu.transfer_path() };
    if r.is_empty() {
        return Ok(Some(out(storage_buffer(gpu, "edge_extend", 0, gpu.transfer_path(), false)?)));
    }
    let none = placeholder(gpu);
    let matte_buf = matte.map_or(&none, |m| &m.rgba);
    let mut e = Ext {
        src_win: win(src.window),
        matte_win: matte.map_or([0; 4], |m| win(m.window)),
        region: win(r),
        flags: [plan.channel as u32, u32::from(plan.invert), u32::from(matte.is_some()), plan.alpha as u32],
        more: [plan.radius as u32, u32::from(plan.fill_all), 0, 0],
        ..Ext::default()
    };
    let size = |(w, h): (usize, usize)| [w as u32, h as u32];
    let level0 = plan.levels[0];
    e.dims = [level0.0 as u32, level0.1 as u32, 0, 0];
    let mut pyramid = vec![run(gpu, &e, "seed", level0, &[(1, &src.rgba), (2, matte_buf)])?];
    for k in 1..plan.levels.len() {
        let (f, c) = (size(plan.levels[k - 1]), size(plan.levels[k]));
        e.dims = [f[0], f[1], c[0], c[1]];
        let next = run(gpu, &e, "pull", plan.levels[k], &[(3, &pyramid[k - 1])])?;
        pyramid.push(next);
    }
    e.dims = [level0.0 as u32, level0.1 as u32, 0, 0];
    let reach = if plan.fill_all {
        None
    } else {
        let across = run(gpu, &e, "reach_h", level0, &[(3, &pyramid[0])])?;
        Some(run(gpu, &e, "reach_v", level0, &[(3, &across)])?)
    };
    let mut filled = pyramid.pop().expect("a pyramid has a level");
    for k in (0..pyramid.len()).rev() {
        let (f, c) = (size(plan.levels[k]), size(plan.levels[k + 1]));
        e.dims = [f[0], f[1], c[0], c[1]];
        filled = run(gpu, &e, "push", plan.levels[k], &[(3, &pyramid[k]), (4, &filled)])?;
    }
    e.dims = [level0.0 as u32, level0.1 as u32, 0, 0];
    let shaded = run(gpu, &e, "shade", level0, &[(1, &src.rgba), (2, &none), (3, &filled), (4, reach.as_ref().unwrap_or(&none))])?;
    Ok(Some(out(shaded)))
}

#[cfg(test)]
mod tests {
    use crate::gpu::check::{noise, smooth};
    use crate::gpu::GpuImage;
    use crate::image::{IRect, Image, Size};
    use crate::ops;
    use serde_json::{json, Value};

    /// The CPU and GPU agree on `params` over these inputs.
    fn agree(gpu: &crate::gpu::Gpu, params: Value, src: &Image, matte: Option<&Image>) {
        let params = ops::params_with_defaults("edge_extend", &params);
        let ctx = ops::EvalContext { frame: 1, write_outputs: false };
        let want = ops::eval("edge_extend", &params, &[Some(src), matte], &ctx).unwrap();
        let s = GpuImage::upload(gpu, src).unwrap();
        let m = matte.map(|m| GpuImage::upload(gpu, m).unwrap());
        let got = super::eval(gpu, &params, &s, m.as_ref()).unwrap().expect("declined").read_back(gpu).unwrap();
        assert_eq!((got.window(), got.format), (want.window(), want.format), "{params}");
        let row = crate::gpu::check::compare_share("edge_extend", "", &want, &got, 1e-4, 1e-4, 0.0, 0.0, 0.0, 0.0);
        assert!(row.pass, "{params}: {row:?}");
    }

    #[test]
    fn edge_extend_matches_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let f = Size { width: 97, height: 61 };
        // Odd, offset windows, partly off the frame.
        let a = noise(f, IRect::new(-5, 3, 80, 66), false, 1);
        let soft = smooth(f, IRect::new(10, 8, 60, 40), false, 2);
        let m = noise(f, IRect::new(20, 10, 60, 40), false, 3);
        for params in [
            json!({ "fill_all": true }),
            json!({ "fill_all": false, "distance": 7.5 }),
            json!({ "fill_all": true, "alpha": "original (unpremultiplied)" }),
            json!({ "fill_all": false, "distance": 3, "alpha": "original (unpremultiplied)" }),
        ] {
            agree(&gpu, params.clone(), &soft, None);
            agree(&gpu, params, &a, None);
        }
        agree(&gpu, json!({ "fill_all": true, "invert": true }), &a, Some(&m));
        agree(&gpu, json!({ "fill_all": false, "distance": 5, "matte_channel": "luminance" }), &a, Some(&m));
    }
}
