//! Ops on the GPU: [`eval`] is `ops::eval` for the ops in [`super::OPS`],
//! on [`GpuImage`]s, with the same data windows, planes, Merge swap and
//! mask / mix. Pointwise ops share one shader (pointwise.wgsl, an op
//! code per op); Blur has its own (blur.wgsl). Data windows are worked
//! out here on the CPU with `ops`' own rules, so they always match.

use super::image::{dispatch_2d, placeholder, storage_buffer, GpuImage};
use super::Gpu;
use crate::color::{self, Rgba};
use crate::image::{IRect, Size};
use crate::ops::{self, Source};
use anyhow::{bail, Result};
use serde_json::Value;
use wgpu::util::DeviceExt;

const POINTWISE: &str = include_str!("pointwise.wgsl");
const BLUR: &str = include_str!("blur.wgsl");

/// When an op is worth running on the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// Always: it costs far more on the CPU than a transfer (Blur).
    Always,
    /// Only when an input is already on the GPU: on the CPU it runs at
    /// memory speed, about what moving the image would cost.
    WithResidentInput,
    /// Not yet: sources with no input would only add a read-back for
    /// whoever shows them (until the viewer draws GPU results, G7).
    Never,
}

pub fn preference(op: &str) -> Prefer {
    match op {
        "blur" => Prefer::Always,
        "constant" | "checkerboard" => Prefer::Never,
        _ => Prefer::WithResidentInput,
    }
}

/// Whether `op` with `params` has a GPU path (decided before any input
/// is uploaded). Unknown values fall to the CPU, which reports them.
pub fn supports(op: &str, params: &Value) -> bool {
    match op {
        "merge" => params.get("operation").and_then(Value::as_str).is_some_and(|o| merge_mode(o).is_some()),
        "invert" => params.get("channels").and_then(Value::as_str).is_some_and(|c| ops::invert_channels(c).is_ok()),
        "channel_select" => ["red", "green", "blue", "alpha"].iter().all(|k| params.get(*k).and_then(Value::as_str).is_some_and(|n| ops::channel_source(n).is_ok())),
        _ => super::OPS.contains(&op),
    }
}

fn merge_mode(op: &str) -> Option<u32> {
    ["over", "under", "add", "multiply", "screen", "max", "min"].iter().position(|m| *m == op).map(|i| i as u32)
}

/// Run `op` on the GPU: `inputs` one per port as for `ops::eval` (an
/// unconnected optional port None). Ok(None) when it can't here after
/// all (a missing input: the CPU then says so).
pub fn eval(gpu: &Gpu, op: &str, params: &Value, inputs: &[Option<&GpuImage>]) -> Result<Option<GpuImage>> {
    // Merge with Swap A / B: A and B trade places for everything below.
    let swapped: Vec<Option<&GpuImage>>;
    let inputs = if op == "merge" && params.get("swap").and_then(Value::as_bool) == Some(true) && inputs.len() >= 2 {
        swapped = [inputs[1], inputs[0]].into_iter().chain(inputs[2..].iter().copied()).collect();
        &swapped[..]
    } else {
        inputs
    };
    let Some(mut out) = eval_op(gpu, op, params, inputs)? else { return Ok(None) };
    if let Some(src) = ops::planes_source(op).and_then(|n| inputs.get(n).copied().flatten()) {
        if out.planes.is_empty() && out.format == src.format {
            out.keep_planes(gpu, src)?;
        }
    }
    match ops::info(op).and_then(|i| i.mask_base.map(|b| (b, i.inputs.len() - 1))) {
        Some((base, mask)) => {
            let Some(base) = inputs.get(base).copied().flatten() else { return Ok(None) };
            let mix = params.get("mix").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let channel = params.get("mask_channel").and_then(Value::as_str).unwrap_or("alpha");
            let invert = params.get("mask_invert").and_then(Value::as_bool).unwrap_or(false);
            let mask = inputs.get(mask).copied().flatten();
            Ok(Some(mask_mix(gpu, base, out, mask, channel, invert, mix)?))
        }
        None => Ok(Some(out)),
    }
}

fn eval_op(gpu: &Gpu, op: &str, params: &Value, inputs: &[Option<&GpuImage>]) -> Result<Option<GpuImage>> {
    let input = |n: usize| inputs.get(n).copied().flatten();
    let mut k = Kernel::default();
    Ok(Some(match op {
        _ if ops::POINTWISE.contains(&op) => {
            let Some(src) = input(0) else { return Ok(None) };
            let window = ops::pointwise_window(src.format, &[src.window], ops::pointwise_blank(op, params)?);
            match op {
                "grade" => {
                    let p: ops::Grade = ops::parse(params, op)?;
                    k.op = GRADE;
                    (k.v0, k.v1, k.v2) = (vec3(p.multiply), vec3(p.offset), vec3(p.gamma));
                }
                "exposure" => {
                    let p: ops::Exposure = ops::parse(params, op)?;
                    (k.op, k.v0[0]) = (EXPOSURE, 2f32.powf(p.stops));
                }
                "saturation" => {
                    let p: ops::Saturation = ops::parse(params, op)?;
                    (k.op, k.v0[0]) = (SATURATION, p.saturation);
                }
                "invert" => {
                    let p: ops::Channels = ops::parse(params, op)?;
                    let (rgb, alpha) = ops::invert_channels(&p.channels)?;
                    (k.op, k.flags) = (INVERT, u32::from(rgb) | u32::from(alpha) << 1);
                }
                "clamp" => {
                    let p: ops::Clamp = ops::parse(params, op)?;
                    (k.op, k.flags) = (CLAMP, u32::from(p.channels == "rgba"));
                    k.v0[..2].copy_from_slice(&[p.min.min(p.max), p.max.max(p.min)]);
                }
                "premult" => k.op = PREMULT,
                _ => k.op = UNPREMULT,
            }
            let mut out = k.run(gpu, src.format, window, [Some(src), None, None])?;
            out.keep_planes(gpu, src)?;
            out
        }
        "channel_select" => {
            let p: ops::ChannelSelect = ops::parse(params, op)?;
            let Some(a) = input(0) else { return Ok(None) };
            let b = input(1);
            let mut blank = color::TRANSPARENT;
            for (c, name) in [&p.red, &p.green, &p.blue, &p.alpha].into_iter().enumerate() {
                k.sel[c] = match ops::channel_source(name)? {
                    Source::Channel(0, ch) => ch as u32,
                    Source::Channel(_, _) if b.is_none() => return Ok(None),
                    Source::Channel(_, ch) => 4 + ch as u32,
                    Source::Value(v) => {
                        blank[c] = v;
                        if v == 0.0 { 8 } else { 9 }
                    }
                };
            }
            let windows: Vec<IRect> = [Some(a), b].into_iter().flatten().map(|i| i.window).collect();
            k.op = CHANNEL_SELECT;
            k.run(gpu, a.format, ops::pointwise_window(a.format, &windows, blank), [Some(a), b, None])?
        }
        "merge" => {
            let p: ops::Merge = ops::parse(params, op)?;
            let (Some(fg), Some(bg), Some(mode)) = (input(0), input(1), merge_mode(&p.operation)) else { return Ok(None) };
            let blank = ops::blend(&p.operation, color::TRANSPARENT, color::TRANSPARENT)?;
            let window = ops::pointwise_window(bg.format, &[fg.window, bg.window], blank);
            (k.op, k.mode) = (MERGE, mode);
            let mut out = k.run(gpu, bg.format, window, [Some(fg), Some(bg), None])?;
            out.planes = merge_planes(gpu, &out, fg, bg, p.operation == "under")?;
            out
        }
        "dissolve" => {
            let p: ops::Dissolve = ops::parse(params, op)?;
            let (Some(a), Some(b)) = (input(0), input(1)) else { return Ok(None) };
            let t = p.amount.clamp(0.0, 1.0);
            (k.op, k.v0[0]) = (DISSOLVE, t);
            let mut out = k.run(gpu, b.format, a.window.union(&b.window), [Some(a), Some(b), None])?;
            // Planes can't be blended: the nearer input's.
            out.keep_planes(gpu, if t < 0.5 { a } else { b })?;
            out
        }
        // A in B's frame (the mask / mix step then blends it over B).
        "keymix" => {
            let (Some(a), Some(b)) = (input(0), input(1)) else { return Ok(None) };
            GpuImage { format: b.format, window: a.window, rgba: a.rgba.clone(), planes: Vec::new(), scene: None, transfer: a.transfer }
        }
        "switch" => {
            let p: ops::Switch = ops::parse(params, op)?;
            let Some(src) = input(p.which.clamp(0, ops::SWITCH_PORTS as i64 - 1) as usize) else { return Ok(None) };
            src.clone()
        }
        "constant" => {
            let p: ops::Constant = ops::parse(params, op)?;
            let f = ops::source_size(p.width, p.height);
            (k.op, k.v0) = (CONSTANT, color::premultiply(p.color));
            k.run(gpu, f, IRect::frame(f), [None, None, None])?
        }
        "checkerboard" => {
            let p: ops::Checkerboard = ops::parse(params, op)?;
            let f = ops::source_size(p.width, p.height);
            (k.op, k.n, k.v0, k.v1) = (CHECKERBOARD, p.size.max(1) as u32, color::premultiply(p.color_a), color::premultiply(p.color_b));
            k.run(gpu, f, IRect::frame(f), [None, None, None])?
        }
        "blur" => {
            let p: ops::Blur = ops::parse(params, op)?;
            let Some(src) = input(0) else { return Ok(None) };
            blur(gpu, src, p.radius.max(0.0).round() as usize, p.passes.max(1))?
        }
        other => bail!("no GPU path for '{other}'"),
    }))
}

fn vec3(v: [f32; 3]) -> Rgba {
    [v[0], v[1], v[2], 0.0]
}

/// `result` blended back toward `base` by `mix × mask` (`ops::mask_mix`).
#[allow(clippy::too_many_arguments)]
fn mask_mix(gpu: &Gpu, base: &GpuImage, result: GpuImage, mask: Option<&GpuImage>, channel: &str, invert: bool, mix: f32) -> Result<GpuImage> {
    let mix = mix.clamp(0.0, 1.0);
    if mask.is_none() && mix >= 1.0 {
        return Ok(result);
    }
    let mut k = Kernel { op: MASK_MIX, ..Kernel::default() };
    k.v0[0] = mix;
    k.mode = match channel {
        "red" => 1,
        "green" => 2,
        "blue" => 3,
        "luminance" => 4,
        _ => 0,
    };
    k.flags = u32::from(invert) | u32::from(mask.is_some()) << 1;
    let mut out = k.run(gpu, result.format, result.window.union(&base.window), [Some(base), Some(&result), mask])?;
    out.keep_planes(gpu, if result.planes.is_empty() { base } else { &result })?;
    Ok(out)
}

/// A Merge's planes (`ops::merge_planes`): the back input's names, then
/// the front's it lacks; each the front's where it is more than half
/// opaque, the back's elsewhere (A in front, B with `under`).
fn merge_planes(gpu: &Gpu, out: &GpuImage, fg: &GpuImage, bg: &GpuImage, under: bool) -> Result<Vec<(String, wgpu::Buffer)>> {
    let (front, back) = if under { (bg, fg) } else { (fg, bg) };
    let mut names: Vec<&str> = back.planes.iter().map(|(n, _)| n.as_str()).collect();
    names.extend(front.planes.iter().map(|(n, _)| n.as_str()).filter(|n| back.plane(n).is_none()));
    let none = placeholder(gpu);
    let pipeline = gpu.compute_pipeline("gpu/pointwise", POINTWISE, "merge_plane");
    names
        .into_iter()
        .map(|name| {
            let dst = storage_buffer(gpu, "merged plane", (out.len() * 4) as u64, gpu.transfer_path(), false)?;
            if !out.is_empty() {
                let (fp, bp) = (front.plane(name), back.plane(name));
                let k = Kernel {
                    out_win: win(out.window),
                    win_a: win(front.window),
                    win_m: fp.map_or([0; 4], |_| win(front.window)),
                    win_b: bp.map_or([0; 4], |_| win(back.window)),
                    ..Kernel::default()
                };
                let bufs = [(1, &front.rgba), (5, fp.unwrap_or(&none)), (6, bp.unwrap_or(&none)), (7, &dst)];
                k.dispatch(gpu, &pipeline, &bufs, out.window)?;
            }
            Ok((name.to_string(), dst))
        })
        .collect()
}

/// Blur (`ops::blur`): the data grown by the reach (except where it meets
/// the frame's edge, whose pixels then repeat), then `passes` box passes
/// across and down.
fn blur(gpu: &Gpu, src: &GpuImage, r: usize, passes: usize) -> Result<GpuImage> {
    if r == 0 || src.window.is_empty() {
        return Ok(src.clone());
    }
    let reach = (r * passes) as i32;
    let (w, f) = (src.window, IRect::frame(src.format));
    let region = IRect::new(
        w.x0 - if w.x0 > f.x0 { reach } else { 0 },
        w.y0 - if w.y0 > f.y0 { reach } else { 0 },
        w.x1 + if w.x1 < f.x1 { reach } else { 0 },
        w.y1 + if w.y1 < f.y1 { reach } else { 0 },
    );
    // (The grown image may share the input's buffer: it is only read.)
    let grown = src.rewindow(gpu, region)?;
    let size = (grown.len() * 16) as u64;
    let mut img = GpuImage { rgba: storage_buffer(gpu, "blur", size, gpu.transfer_path(), false)?, planes: Vec::new(), scene: None, ..grown.clone() };
    let mut other = storage_buffer(gpu, "blur", size, gpu.transfer_path(), false)?;
    let pipeline = gpu.compute_pipeline("gpu/blur", BLUR, "box_pass");
    gpu.scoped(|| {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let mut uniforms = Vec::new();
        let mut first = true;
        for _ in 0..passes {
            for horizontal in [1u32, 0] {
                let pass: [i32; 4] = [region.width() as i32, region.height() as i32, r as i32, horizontal as i32];
                let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("blur pass"), contents: bytemuck::cast_slice(&pass), usage: wgpu::BufferUsages::UNIFORM });
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("blur"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: if first { &grown.rgba } else { &img.rgba }.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: other.as_entire_binding() },
                    ],
                });
                dispatch_2d(&mut encoder, &pipeline, &bind, region);
                std::mem::swap(&mut img.rgba, &mut other);
                first = false;
                uniforms.push(uniform);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    })?;
    Ok(img)
}

// -- the pointwise kernel ---------------------------------------------------

const GRADE: u32 = 0;
const EXPOSURE: u32 = 1;
const SATURATION: u32 = 2;
const INVERT: u32 = 3;
const CLAMP: u32 = 4;
const PREMULT: u32 = 5;
const UNPREMULT: u32 = 6;
const CHANNEL_SELECT: u32 = 7;
const MERGE: u32 = 8;
const DISSOLVE: u32 = 9;
const CONSTANT: u32 = 10;
const CHECKERBOARD: u32 = 11;
const MASK_MIX: u32 = 12;

/// A data window as pointwise.wgsl takes it: x0, y0, width, height.
fn win(r: IRect) -> [i32; 4] {
    [r.x0, r.y0, r.width() as i32, r.height() as i32]
}

/// pointwise.wgsl's `Params`.
#[derive(Default)]
struct Kernel {
    out_win: [i32; 4],
    win_a: [i32; 4],
    win_b: [i32; 4],
    win_m: [i32; 4],
    op: u32,
    mode: u32,
    flags: u32,
    n: u32,
    v0: [f32; 4],
    v1: [f32; 4],
    v2: [f32; 4],
    sel: [u32; 4],
}

impl Kernel {
    fn bytes(&self) -> Vec<u32> {
        let i = |v: [i32; 4]| v.map(|x| x as u32);
        let f = |v: [f32; 4]| v.map(f32::to_bits);
        [i(self.out_win), i(self.win_a), i(self.win_b), i(self.win_m), [self.op, self.mode, self.flags, self.n], f(self.v0), f(self.v1), f(self.v2), self.sel].concat()
    }

    /// A new image of frame `format` over `window`, each pixel from
    /// inputs A, B and the mask (absent ones read as transparent).
    fn run(mut self, gpu: &Gpu, format: Size, window: IRect, inputs: [Option<&GpuImage>; 3]) -> Result<GpuImage> {
        let rgba = storage_buffer(gpu, "pointwise", (window.width() * window.height() * 16) as u64, gpu.transfer_path(), false)?;
        let out = GpuImage { format, window, rgba, planes: Vec::new(), scene: None, transfer: gpu.transfer_path() };
        if window.is_empty() {
            return Ok(out);
        }
        self.out_win = win(window);
        [self.win_a, self.win_b, self.win_m] = inputs.map(|i| i.map_or([0; 4], |i| win(i.window)));
        let none = placeholder(gpu);
        let [a, b, m] = inputs.map(|i| i.map_or(&none, |i| &i.rgba));
        let pipeline = gpu.compute_pipeline("gpu/pointwise", POINTWISE, "pointwise");
        self.dispatch(gpu, &pipeline, &[(1, a), (2, b), (3, m), (4, &out.rgba)], window)?;
        Ok(out)
    }

    /// One pass of `pipeline` over `window`, with these params and
    /// buffers at their bindings.
    fn dispatch(&self, gpu: &Gpu, pipeline: &wgpu::ComputePipeline, buffers: &[(u32, &wgpu::Buffer)], window: IRect) -> Result<()> {
        gpu.scoped(|| {
            let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("pointwise"), contents: bytemuck::cast_slice(&self.bytes()), usage: wgpu::BufferUsages::UNIFORM });
            let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
            entries.extend(buffers.iter().map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }));
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("pointwise"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            dispatch_2d(&mut encoder, pipeline, &bind, window);
            gpu.queue.submit([encoder.finish()]);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::check::{compare, noise};
    use crate::image::Image;
    use serde_json::json;

    /// `op` with `params` (defaults filled) on the CPU and the GPU, the
    /// inputs given; the results agree within (abs, rel).
    fn agree(gpu: &Gpu, op: &str, params: Value, inputs: &[Option<&Image>], tol: (f64, f64)) {
        let params = ops::params_with_defaults(op, &params);
        let ctx = ops::EvalContext { frame: 1, write_outputs: false };
        let want = ops::eval(op, &params, inputs, &ctx).unwrap();
        let up: Vec<Option<GpuImage>> = inputs.iter().map(|i| i.map(|i| GpuImage::upload(gpu, i).unwrap())).collect();
        let refs: Vec<Option<&GpuImage>> = up.iter().map(Option::as_ref).collect();
        assert!(supports(op, &params), "{op} {params}");
        let got = eval(gpu, op, &params, &refs).unwrap().unwrap_or_else(|| panic!("{op} declined")).read_back(gpu).unwrap();
        assert_eq!((got.window(), got.format), (want.window(), want.format), "{op} {params}");
        let row = compare(op, "", &want, &got, tol.0, tol.1, 0.0, 0.0, 0.0);
        assert!(row.pass, "{op} {params}: {row:?}");
    }

    const POINT: (f64, f64) = (1e-6, 1e-5);

    fn images() -> (Image, Image, Image) {
        let f = Size { width: 97, height: 61 };
        // Odd, offset windows, partly off the frame; A with a plane.
        let a = noise(f, IRect::new(-5, 3, 80, 66), true, 1);
        let b = noise(f, IRect::new(10, -4, 104, 50), false, 2);
        let m = noise(f, IRect::new(20, 10, 60, 40), false, 3);
        (a, b, m)
    }

    #[test]
    fn pointwise_ops_match_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let (a, b, m) = images();
        let cases = [
            ("grade", json!({"multiply": [1.2, 0.8, 1.0], "offset": [0.01, -0.02, 0.0], "gamma": [1.0, 2.2, 0.7]})),
            ("exposure", json!({"stops": 1.5})),
            ("saturation", json!({"saturation": 0.3})),
            ("invert", json!({"channels": "rgb"})),
            ("invert", json!({"channels": "rgba"})),
            ("clamp", json!({"min": 0.1, "max": 0.9, "channels": "rgba"})),
            ("clamp", json!({"min": 0.0, "max": 1.0, "channels": "rgb"})),
            ("premult", json!({})),
            ("unpremult", json!({})),
        ];
        for (op, params) in cases {
            agree(&gpu, op, params.clone(), &[Some(&a), None], POINT);
            // Masked and mixed: through the mask's channels.
            for (channel, invert) in [("alpha", false), ("luminance", true), ("green", false)] {
                let mut p = params.clone();
                p["mix"] = json!(0.7);
                p["mask_channel"] = json!(channel);
                p["mask_invert"] = json!(invert);
                agree(&gpu, op, p, &[Some(&a), Some(&m)], POINT);
            }
        }
        for (op, params) in [("merge", json!({"operation": "over"})), ("dissolve", json!({"amount": 0.3})), ("keymix", json!({}))] {
            agree(&gpu, op, params, &[Some(&a), Some(&b), Some(&m)], POINT);
        }
        for operation in ["over", "under", "add", "multiply", "screen", "max", "min"] {
            agree(&gpu, "merge", json!({"operation": operation}), &[Some(&a), Some(&b), None], POINT);
            agree(&gpu, "merge", json!({"operation": operation, "swap": true, "mix": 0.4}), &[Some(&a), Some(&b), Some(&m)], POINT);
        }
        agree(&gpu, "dissolve", json!({"amount": 0.8}), &[Some(&b), Some(&a), None], POINT);
        agree(&gpu, "channel_select", json!({"red": "B green", "green": "A alpha", "blue": "one", "alpha": "zero"}), &[Some(&a), Some(&b)], POINT);
        agree(&gpu, "channel_select", json!({"red": "A blue", "green": "A red", "blue": "A green", "alpha": "A alpha"}), &[Some(&a), None], POINT);
        agree(&gpu, "switch", json!({"which": 1}), &[Some(&a), Some(&b), None, None, None], POINT);
        agree(&gpu, "constant", json!({"width": 33, "height": 17, "color": [0.2, 0.4, 0.8, 0.5]}), &[], POINT);
        agree(&gpu, "checkerboard", json!({"width": 70, "height": 45, "size": 8}), &[], POINT);
    }

    /// Merge carries depth from whichever input is in front; under swaps
    /// who that is.
    #[test]
    fn merge_planes_match_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let (a, _, _) = images();
        let b = noise(a.format, IRect::new(12, 8, 90, 70), true, 4);
        for operation in ["over", "under", "screen"] {
            agree(&gpu, "merge", json!({"operation": operation}), &[Some(&a), Some(&b), None], POINT);
            agree(&gpu, "merge", json!({"operation": operation}), &[Some(&b), Some(&a), None], POINT);
        }
    }

    #[test]
    fn blur_matches_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let (a, _, m) = images();
        // A plate filling its frame (edges repeat) and a layer inside it.
        let plate = noise(Size { width: 120, height: 70 }, IRect::new(0, 0, 120, 70), true, 6);
        for (img, r, passes) in [(&a, 3.0, 1), (&a, 8.0, 3), (&plate, 30.0, 2), (&plate, 0.0, 3), (&a, 100.0, 1)] {
            agree(&gpu, "blur", json!({"radius": r, "passes": passes}), &[Some(img), None], (1e-4, 1e-4));
        }
        agree(&gpu, "blur", json!({"radius": 5, "passes": 2, "mix": 0.5}), &[Some(&a), Some(&m)], (1e-4, 1e-4));
        // The input is left as it was (a plate's blur starts from its very
        // buffer: the passes must not write into it).
        let up = GpuImage::upload(&gpu, &plate).unwrap();
        for passes in [1, 2, 3] {
            eval(&gpu, "blur", &ops::params_with_defaults("blur", &json!({"radius": 4, "passes": passes})), &[Some(&up), None]).unwrap();
            assert!(up.read_back(&gpu).unwrap().pixels == plate.pixels, "passes {passes}");
        }
    }

    /// Inputs that can't be met here are declined (the CPU reports them),
    /// and unknown values aren't claimed.
    #[test]
    fn what_the_gpu_declines() {
        assert!(!supports("merge", &json!({"operation": "zmerge"})) && !supports("invert", &json!({"channels": "rg"})));
        assert!(!supports("transform", &json!({})) && supports("grade", &json!({})));
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let a = GpuImage::upload(&gpu, &images().0).unwrap();
        let p = ops::params_with_defaults("channel_select", &json!({"red": "B red"}));
        assert!(eval(&gpu, "channel_select", &p, &[Some(&a), None]).unwrap().is_none());
        assert!(eval(&gpu, "dissolve", &ops::params_with_defaults("dissolve", &json!({})), &[Some(&a), None]).unwrap().is_none());
    }
}
