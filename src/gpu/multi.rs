//! Shuffle routes reading inputs 2+ (`shuffle_route`) and Matte Combine
//! (`matte_combine`) on the GPU, as chained passes in multi.wgsl: each
//! reads one source (one input's color or one plane) and writes the output
//! being built, so no pass binds more than four storage buffers, which
//! every adapter allows (Metal's and the WebGPU baseline's included),
//! however many inputs there are.
//!
//! Routes are copies: exact. Matte Combine's min / max steps are exact;
//! luminance and the Soft forms agree within float rounding.

use super::image::{dispatch_2d, placeholder, storage_buffer, GpuImage};
use super::Gpu;
use crate::image::{IRect, Image, Plane, Size};
use crate::matte_combine::{Op, Plan, Source};
use crate::shuffle_route::{self, Route};
use anyhow::Result;
use serde_json::Value;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("multi.wgsl");

fn win(r: IRect) -> [i32; 4] {
    [r.x0, r.y0, r.width() as i32, r.height() as i32]
}

/// multi.wgsl's `M`.
#[derive(Default, Clone, Copy)]
struct M {
    out_win: [i32; 4],
    src_win: [i32; 4],
    a: [u32; 4],
    b: [u32; 4],
    value: [f32; 4],
}

impl M {
    fn bytes(&self) -> Vec<u32> {
        let i = |v: [i32; 4]| v.map(|x| x as u32);
        [i(self.out_win), i(self.src_win), self.a, self.b, self.value.map(f32::to_bits)].concat()
    }
}

/// `entry` over the output window, these buffers at their bindings (1 src
/// color, 2 src plane, 3 out color, 4 out plane: just those `entry` uses).
fn run(gpu: &Gpu, m: &M, entry: &'static str, buffers: &[(u32, &wgpu::Buffer)]) -> Result<()> {
    let w = m.out_win;
    let window = IRect::new(w[0], w[1], w[0] + w[2], w[1] + w[3]);
    if window.is_empty() {
        return Ok(());
    }
    let pipeline = gpu.compute_pipeline("gpu/multi", SHADER, entry);
    gpu.scoped(|| {
        let uniform = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("multi"), contents: bytemuck::cast_slice(&m.bytes()), usage: wgpu::BufferUsages::UNIFORM });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
        entries.extend(buffers.iter().map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }));
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        dispatch_2d(&mut encoder, &pipeline, &bind, window);
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    })
}

/// A source for a pass: its window, kind (0 constant, 1 color component,
/// 2 plane, 3 luminance), component, constant, and buffers (color, plane).
struct Src<'a> {
    window: IRect,
    kind: u32,
    comp: u32,
    value: f32,
    color: Option<&'a wgpu::Buffer>,
    plane: Option<&'a wgpu::Buffer>,
}

impl<'a> Src<'a> {
    /// Channel `name` (R, G, B, A or a plane) of `img`; None if it has none.
    fn channel(img: &'a GpuImage, name: &str) -> Option<Src<'a>> {
        let color = |comp| Src { window: img.window, kind: 1, comp, value: 0.0, color: Some(&img.rgba), plane: None };
        match name {
            "R" => Some(color(0)),
            "G" => Some(color(1)),
            "B" => Some(color(2)),
            "A" => Some(color(3)),
            _ => img.plane(name).map(|p| Src { window: img.window, kind: 2, comp: 0, value: 0.0, color: None, plane: Some(p) }),
        }
    }

    fn constant(value: f32) -> Src<'a> {
        Src { window: IRect::default(), kind: 0, comp: 0, value, color: None, plane: None }
    }

    fn bindings<'b>(&'b self, none: &'b wgpu::Buffer) -> [(u32, &'b wgpu::Buffer); 2] {
        [(1, self.color.unwrap_or(none)), (2, self.plane.unwrap_or(none))]
    }
}

/// A fresh color buffer holding `color` (over `from`) on window `to`.
fn rewindow_color(gpu: &Gpu, color: &wgpu::Buffer, from: IRect, to: IRect) -> Result<wgpu::Buffer> {
    let out = storage_buffer(gpu, "rewindowed", (to.width() * to.height() * 16) as u64, gpu.transfer_path(), false)?;
    let m = M { out_win: win(to), src_win: win(from), ..M::default() };
    run(gpu, &m, "rewindow_color", &[(1, color), (3, &out)])?;
    Ok(out)
}

/// A fresh plane buffer holding `plane` (over `from`) on window `to`.
fn rewindow_plane(gpu: &Gpu, plane: &wgpu::Buffer, from: IRect, to: IRect) -> Result<wgpu::Buffer> {
    let out = storage_buffer(gpu, "rewindowed plane", (to.width() * to.height() * 4) as u64, gpu.transfer_path(), false)?;
    let m = M { out_win: win(to), src_win: win(from), ..M::default() };
    run(gpu, &m, "rewindow_plane", &[(2, plane), (4, &out)])?;
    Ok(out)
}

/// The plane names `remove` (Shuffle's Remove) leaves of `names`, by the
/// CPU's own rules (a 1 × 1 probe carrying the names).
fn kept_after_remove(names: &[String], remove: &str) -> Vec<String> {
    let mut probe = Image::blank(Size { width: 1, height: 1 }, IRect::new(0, 0, 1, 1));
    probe.planes = names.iter().map(|n| Plane { name: n.clone(), data: vec![0.0] }).collect();
    let drop: Vec<&str> = remove.split([',', ' ']).filter(|s| !s.is_empty()).collect();
    probe.remove_layers(&drop);
    probe.planes.into_iter().map(|p| p.name).collect()
}

/// Whether a Shuffle's `params` route channels from inputs 2+ (the case
/// this module runs; the rest stays on the single-input path).
pub(crate) fn reads_other_inputs(params: &Value) -> bool {
    let routes = shuffle_route::parse(params.get("routes").unwrap_or(&Value::Null)).unwrap_or_default();
    shuffle_route::inputs_read(&routes).iter().any(|&k| k > 0)
}

/// A Shuffle whose routes read inputs 2+: `base` is its result from input
/// 1 with no routes and no Remove (the single-input path's), `inputs` its
/// own. None when a route can't be followed (the CPU then says why).
pub(crate) fn shuffle(gpu: &Gpu, params: &Value, base: GpuImage, inputs: &[Option<&GpuImage>]) -> Result<Option<GpuImage>> {
    let Ok(routes) = shuffle_route::parse(params.get("routes").unwrap_or(&Value::Null)) else { return Ok(None) };
    // Every route's source, before anything runs.
    let mut sources: Vec<(Src, &Route)> = Vec::new();
    for r in &routes {
        let src = match r.channel.as_str() {
            "0" => Src::constant(0.0),
            "1" => Src::constant(1.0),
            ch => {
                let Some(img) = inputs.get(r.input).copied().flatten() else { return Ok(None) };
                let Some(s) = Src::channel(img, ch) else { return Ok(None) };
                s
            }
        };
        sources.push((src, r));
    }
    let window = shuffle_route::inputs_read(&routes).into_iter().filter_map(|k| inputs.get(k).copied().flatten()).fold(base.window, |u, img| u.union(&img.window));
    // Fresh buffers on the output window: the base's may be its input's
    // own, which must not change.
    let mut rgba: Option<wgpu::Buffer> = None;
    let mut planes: Vec<(String, wgpu::Buffer)> = if window == base.window {
        base.planes.clone()
    } else {
        base.planes.iter().map(|(n, b)| Ok((n.clone(), rewindow_plane(gpu, b, base.window, window)?))).collect::<Result<_>>()?
    };
    let none = placeholder(gpu);
    let n = (window.width() * window.height()) as u64;
    for (src, r) in &sources {
        let m = M { out_win: win(window), src_win: win(src.window), a: [src.kind, src.comp, 0, 0], value: [src.value, 0.0, 0.0, 0.0], ..M::default() };
        let [c, p] = src.bindings(&none);
        match ["R", "G", "B", "A"].iter().position(|d| *d == r.dest) {
            Some(comp) => {
                if rgba.is_none() {
                    rgba = Some(rewindow_color(gpu, &base.rgba, base.window, window)?);
                }
                let out = rgba.as_ref().unwrap();
                run(gpu, &M { a: [src.kind, src.comp, comp as u32, 0], ..m }, "route_to_color", &[c, p, (3, out)])?;
            }
            None => {
                let out = storage_buffer(gpu, "routed plane", n * 4, gpu.transfer_path(), false)?;
                run(gpu, &m, "route_to_plane", &[c, p, (4, &out)])?;
                match planes.iter_mut().find(|(name, _)| *name == r.dest) {
                    Some(slot) => slot.1 = out,
                    None => planes.push((r.dest.clone(), out)),
                }
            }
        }
    }
    let rgba = match rgba {
        Some(b) => b,
        None if window == base.window => base.rgba.clone(),
        None => rewindow_color(gpu, &base.rgba, base.window, window)?,
    };
    // Remove last, as on the CPU.
    let remove = params.get("remove").and_then(Value::as_str).unwrap_or("");
    let keep = kept_after_remove(&planes.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(), remove);
    planes.retain(|(n, _)| keep.contains(n));
    Ok(Some(GpuImage { format: base.format, window, rgba, planes, scene: base.scene.clone(), transfer: gpu.transfer_path() }))
}

/// Matte Combine of `inputs` (`params` resolved at the frame). None when
/// it can't run here (a missing input or plane, bad params: the CPU then
/// says why).
pub(crate) fn matte_combine(gpu: &Gpu, params: &Value, inputs: &[Option<&GpuImage>]) -> Result<Option<GpuImage>> {
    let connected: Vec<bool> = inputs.iter().map(Option::is_some).collect();
    let Ok(plan) = Plan::new(params, &connected) else { return Ok(None) };
    let base = inputs[0].expect("input 1 is connected");
    let mut sources = Vec::new();
    for s in &plan.steps {
        let img = inputs[s.input].expect("a step's input is connected");
        let src = match &s.source {
            Source::Color(c) => Src { window: img.window, kind: 1, comp: *c as u32, value: 0.0, color: Some(&img.rgba), plane: None },
            Source::Luminance => Src { window: img.window, kind: 3, comp: 0, value: 0.0, color: Some(&img.rgba), plane: None },
            Source::Plane(name) => {
                let Some(p) = img.plane(name) else { return Ok(None) };
                Src { window: img.window, kind: 2, comp: 0, value: 0.0, color: None, plane: Some(p) }
            }
        };
        sources.push(src);
    }
    let windows: Vec<IRect> = sources.iter().map(|s| s.window).collect();
    let window = plan.window(base.format, &windows, base.window);
    let n = (window.width() * window.height()) as u64;
    let none = placeholder(gpu);
    let acc = storage_buffer(gpu, "matte", n * 4, gpu.transfer_path(), false)?;
    for (k, (s, src)) in plan.steps.iter().zip(&sources).enumerate() {
        let op = match s.op {
            Op::Union => 0,
            Op::Subtract => 1,
            Op::Intersect => 2,
            Op::Difference => 3,
        };
        let m = M { out_win: win(window), src_win: win(src.window), a: [src.kind, src.comp, 0, 0], b: [u32::from(s.invert), op, u32::from(plan.soft), u32::from(k == 0)], ..M::default() };
        let [c, p] = src.bindings(&none);
        run(gpu, &m, "matte_step", &[c, p, (4, &acc)])?;
    }
    let rgba = storage_buffer(gpu, "matte combined", n * 16, gpu.transfer_path(), false)?;
    let m = M { out_win: win(window), src_win: win(base.window), b: [u32::from(plan.keep_input), 0, 0, 0], ..M::default() };
    run(gpu, &m, "matte_output", &[(1, &base.rgba), (2, &acc), (3, &rgba)])?;
    let mut out = GpuImage { format: base.format, window, rgba, planes: Vec::new(), scene: None, transfer: gpu.transfer_path() };
    if plan.keep_input {
        out.keep_planes(gpu, base)?;
    }
    if !plan.plane.is_empty() {
        match out.planes.iter_mut().find(|(name, _)| *name == plan.plane) {
            Some(slot) => slot.1 = acc,
            None => out.planes.push((plan.plane.clone(), acc)),
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use crate::gpu::check::noise;
    use crate::gpu::GpuImage;
    use crate::image::{IRect, Image, Plane, Size};
    use crate::ops;
    use serde_json::{json, Value};

    /// `op` with `params` on these inputs: the GPU (`gpu::ops::eval`, not
    /// declined) as the CPU, within `tol` (0: exact), windows and planes too.
    fn agree(gpu: &crate::gpu::Gpu, op: &str, params: Value, inputs: &[Option<&Image>], tol: f64) {
        let params = ops::params_with_defaults(op, &params);
        let ctx = ops::EvalContext { frame: 1, write_outputs: false };
        let want = ops::eval(op, &params, inputs, &ctx).unwrap();
        let up: Vec<Option<GpuImage>> = inputs.iter().map(|i| i.map(|i| GpuImage::upload(gpu, i).unwrap())).collect();
        let refs: Vec<Option<&GpuImage>> = up.iter().map(Option::as_ref).collect();
        let cpu = |_| anyhow::bail!("not needed");
        let got = crate::gpu::ops::eval(gpu, op, &params, &refs, &cpu).unwrap().expect("declined").read_back(gpu).unwrap();
        assert_eq!((got.window(), got.format), (want.window(), want.format), "{op} {params}");
        let names = |i: &Image| i.planes.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&got), names(&want), "{op} {params}: planes");
        let row = crate::gpu::check::compare(op, "", &want, &got, tol, tol, 0.0, 0.0, 0.0);
        assert!(row.pass, "{op} {params}: {row:?}");
        // The inputs are as they were (nothing written into them).
        for (img, gi) in inputs.iter().zip(&up) {
            if let (Some(img), Some(gi)) = (img, gi) {
                let back = gi.read_back(gpu).unwrap();
                assert!(back.pixels == img.pixels && back.planes == img.planes, "{op} {params}: an input changed");
            }
        }
    }

    fn with_plane(mut img: Image, name: &str, seed: u32) -> Image {
        let src = noise(img.format, img.window(), false, seed);
        img.planes.push(Plane { name: name.into(), data: src.pixels.iter().map(|p| p[1]).collect() });
        img
    }

    fn images() -> (Image, Image, Image) {
        let f = Size { width: 97, height: 61 };
        let a = with_plane(noise(f, IRect::new(-5, 3, 80, 66), false, 1), "depth", 11);
        let b = with_plane(noise(f, IRect::new(10, -4, 104, 50), false, 2), "matte.hero", 12);
        let c = with_plane(noise(f, IRect::new(20, 10, 60, 40), false, 3), "N.x", 13);
        (a, b, c)
    }

    #[test]
    fn multi_input_routes_match_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let (a, b, c) = images();
        for params in [
            json!({"routes": [["2:A", "A"]]}),
            json!({"routes": [["2:matte.hero", "R"], ["3:N.x", "specular.R"], ["1:depth", "B"], ["4:1", "G"]]}),
            json!({"routes": [["3:R", "depth"], ["2:A", "glow.A"]], "remove": "depth"}),
            json!({"in": "depth", "out": "spec", "routes": [["2:G", "A"], ["3:A", "spec.R"]], "remove": "matte"}),
            json!({"red": "G", "routes": [["1:G", "R"], ["2:R", "G"]]}),
        ] {
            agree(&gpu, "shuffle", params, &[Some(&a), Some(&b), Some(&c)], 0.0);
        }
        // An unconnected input in between, and a route to a missing channel: left to the CPU.
        agree(&gpu, "shuffle", json!({"routes": [["3:A", "A"]]}), &[Some(&a), None, Some(&c)], 0.0);
        let up = GpuImage::upload(&gpu, &a).unwrap();
        let p = ops::params_with_defaults("shuffle", &json!({"routes": [["2:nope", "A"]]}));
        let cpu = |_| anyhow::bail!("not needed");
        assert!(crate::gpu::ops::eval(&gpu, "shuffle", &p, &[Some(&up), Some(&up)], &cpu).unwrap().is_none());
    }

    #[test]
    fn matte_combine_matches_the_cpu() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let (a, b, c) = images();
        for soft in [false, true] {
            for params in [
                json!({"operations": [0, 1, 2]}),
                json!({"operations": [0, 3, 0], "inverts": [0, 1, 0]}),
                json!({"channels": ["luminance", "matte.hero", "red"], "operations": [0, 2, 1]}),
                json!({"inverts": [1], "operations": [0, 0, 2]}),
                json!({"output": "input 1 + alpha", "plane": "matte.combined", "operations": [0, 1, 0]}),
                json!({"output": "input 1 + alpha", "plane": "depth", "channels": ["", "", "N.x"]}),
            ] {
                let mut p = params.clone();
                p["soft"] = json!(soft);
                let exact = !soft && !p.to_string().contains("luminance");
                agree(&gpu, "matte_combine", p, &[Some(&a), Some(&b), Some(&c)], if exact { 0.0 } else { 1e-6 });
            }
        }
        agree(&gpu, "matte_combine", json!({"operations": [0, 0, 1]}), &[Some(&a), None, Some(&c)], 0.0);
        let up = GpuImage::upload(&gpu, &a).unwrap();
        let p = ops::params_with_defaults("matte_combine", &json!({"channels": ["nope"]}));
        let cpu = |_| anyhow::bail!("not needed");
        assert!(crate::gpu::ops::eval(&gpu, "matte_combine", &p, &[Some(&up)], &cpu).unwrap().is_none());
    }

    /// Per-pass cost with many inputs (printed; `cargo test -- --nocapture
    /// --ignored passes_with_many_inputs`): 32 inputs at 4K.
    #[test]
    #[ignore]
    fn passes_with_many_inputs() {
        let Some(gpu) = crate::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let f = Size { width: 3840, height: 2160 };
        let imgs: Vec<Image> = (0..32).map(|k| noise(f, IRect::frame(f), false, 100 + k)).collect();
        let up: Vec<GpuImage> = imgs.iter().map(|i| GpuImage::upload(&gpu, i).unwrap()).collect();
        let refs: Vec<Option<&GpuImage>> = up.iter().map(Some).collect();
        let cpu = |_| anyhow::bail!("not needed");
        let ops_list: Vec<u32> = (0..32).map(|k| k % 4).collect();
        let routes: Vec<Value> = (1..32).map(|k| json!([format!("{}:A", k + 1), format!("pass{k}")])).collect();
        for (op, params) in [("matte_combine", json!({"operations": ops_list})), ("shuffle", json!({"routes": routes}))] {
            let params = ops::params_with_defaults(op, &params);
            for run in 0..3 {
                let t = std::time::Instant::now();
                let out = crate::gpu::ops::eval(&gpu, op, &params, &refs, &cpu).unwrap().unwrap();
                gpu.wait().unwrap();
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                eprintln!("{op}, 32 inputs, 4K, run {run}: {ms:.2} ms total, {:.3} ms a pass", ms / 32.0);
                drop(out);
            }
        }
    }
}
