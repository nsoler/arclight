//! Color Difference Key: a green or blue screen key by weighted color
//! difference, against one screen color or, with a screen plate plugged
//! in, the screen's own color at each pixel.
//!
//! Standard color-difference keying. For a green screen the difference is
//! `d(c) = g − (wr·r + wb·b)`: how far green rises above a weighted mix of
//! the other two. A pixel's matte is `1 − d(c) / d(s)`, against the
//! screen color `s` there. With Subtract screen on, the screen's share is
//! taken out of the color (a pixel is `αF + (1 − α)S`, so
//! `αF = c − (1 − α)S`), which clears screen-colored edges. Despill then
//! holds the screen channel down to the weighted mix of the other two.
//! A blue screen swaps green and blue. Pointwise: no shrink / grow or
//! softness (follow it with a Keyer, a Blur or an Erode for those).

use crate::color::{self, Rgba};
use crate::image::{IRect, Image, Size};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

/// The op's name.
pub const OP: &str = "color_difference_key";
/// Screen modes: a preset green or blue, or the Screen color param (picked
/// in the viewer; estimated from the plate when unset).
pub const SCREEN_MODES: &[&str] = &["green", "blue", "picked"];
/// What the node outputs.
pub const OUTPUTS: &[&str] = &["keyed", "matte", "despill only"];
/// The presets' screen colors (linear, straight).
pub const GREEN: Rgba = [0.1, 0.6, 0.1, 1.0];
pub const BLUE: Rgba = [0.1, 0.15, 0.6, 1.0];

#[derive(Deserialize)]
pub struct ColorDifferenceKey {
    pub screen_mode: String,
    /// The picked color; None: estimate it from the plate.
    #[serde(default)]
    pub screen: Option<[f32; 4]>,
    pub red_weight: f32,
    pub blue_weight: f32,
    pub clip_black: f32,
    pub clip_white: f32,
    pub subtract_screen: bool,
    pub despill: f32,
    pub output: String,
}

/// How a Color Difference Key runs, on the CPU or the GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// An index in [`OUTPUTS`].
    pub output: usize,
    /// The screen color (straight) where no screen plate shows one.
    pub screen: Rgba,
    /// The screen channel (1 green, 2 blue) and the other two: the one
    /// the red weight takes, then the one the second weight takes.
    pub chans: [usize; 3],
    pub weights: [f32; 2],
    pub clip_black: f32,
    /// Clip white − clip black (never 0).
    pub span: f32,
    pub subtract: bool,
    pub despill: f32,
    /// The output's data window.
    pub window: IRect,
}

/// The plan for `p` on a plate of frame `format`, its data windows
/// `windows` (the plate's, and the screen plate's if plugged in).
/// `estimate` gives the screen color when the mode is picked and none
/// was picked (it reads the plate).
pub fn plan(p: &ColorDifferenceKey, format: Size, windows: &[IRect], estimate: impl FnOnce() -> Result<Rgba>) -> Result<Plan> {
    let output = OUTPUTS.iter().position(|o| *o == p.output).ok_or_else(|| anyhow!("unknown output '{}'", p.output))?;
    let screen = match p.screen_mode.as_str() {
        "green" => GREEN,
        "blue" => BLUE,
        "picked" => match p.screen {
            Some(s) => s,
            None => estimate()?,
        },
        other => return Err(anyhow!("unknown screen mode '{other}'")),
    };
    // A picked color keys as blue when blue leads green, else as green.
    let blue = match p.screen_mode.as_str() {
        "blue" => true,
        "picked" => screen[2] > screen[1],
        _ => false,
    };
    let chans = if blue { [2, 0, 1] } else { [1, 0, 2] };
    let mut plan = Plan {
        output,
        screen,
        chans,
        weights: [p.red_weight, p.blue_weight],
        clip_black: p.clip_black,
        span: (p.clip_white - p.clip_black).max(1e-4),
        subtract: p.subtract_screen,
        despill: p.despill,
        window: IRect::default(),
    };
    let blank = shade(&plan, color::TRANSPARENT, None);
    plan.window = crate::ops::pointwise_window(format, windows, blank);
    Ok(plan)
}

impl Plan {
    /// `c`'s screen channel above the weighted other two.
    fn diff(&self, c: Rgba) -> f32 {
        let [p, a, b] = self.chans;
        c[p] - (self.weights[0] * c[a] + self.weights[1] * c[b])
    }

    /// `c` with its screen channel held down to the other two's weighted
    /// mix, by Despill.
    fn despilled(&self, mut c: Rgba) -> Rgba {
        c[self.chans[0]] -= self.despill * self.diff(c).max(0.0);
        c
    }
}

/// The output for an input pixel `px` (premultiplied) against the screen
/// plate's pixel `plate` there (premultiplied), if plugged in.
pub fn shade(plan: &Plan, px: Rgba, plate: Option<Rgba>) -> Rgba {
    let pa = px[3];
    let c = color::unpremultiply(px);
    if plan.output == 2 {
        let f = plan.despilled(c);
        return [f[0] * pa, f[1] * pa, f[2] * pa, pa];
    }
    // The screen here: the screen plate's, where it shows one.
    let s = plate.map(color::unpremultiply).filter(|s| s[3] > 0.0).unwrap_or(plan.screen);
    let ds = plan.diff(s);
    let raw = if ds > 1e-6 { (1.0 - plan.diff(c) / ds).clamp(0.0, 1.0) } else { 1.0 };
    let alpha = ((raw - plan.clip_black) / plan.span).clamp(0.0, 1.0);
    let m = alpha * pa;
    if plan.output == 1 {
        return [m, m, m, 1.0];
    }
    // The foreground times its matte: the screen's share taken out, or
    // the color scaled.
    let u: Rgba = if plan.subtract {
        std::array::from_fn(|i| if i < 3 { (c[i] - (1.0 - alpha) * s[i]).max(0.0) } else { alpha })
    } else {
        [c[0] * alpha, c[1] * alpha, c[2] * alpha, alpha]
    };
    // Despill is linear in the scale, so it holds on αF as on F.
    let u = plan.despilled(u);
    [u[0] * pa, u[1] * pa, u[2] * pa, m]
}

/// The op on the CPU (`ops::eval`'s arm). Mask and mix are applied by
/// `ops::eval`.
pub fn eval(params: &Value, inputs: &[Option<&Image>]) -> Result<Image> {
    let p: ColorDifferenceKey = crate::ops::parse(params, OP)?;
    let src = inputs.first().copied().flatten().ok_or_else(|| anyhow!("{OP}: input 0 not connected"))?;
    let plate = inputs.get(1).copied().flatten();
    let windows: Vec<IRect> = [Some(src), plate].into_iter().flatten().map(Image::window).collect();
    let plan = plan(&p, src.format, &windows, || Ok(crate::ops::estimate_screen(src)))?;
    let mut out = Image::blank(src.format, plan.window);
    let (w, x0, y0) = (plan.window.width().max(1), plan.window.x0, plan.window.y0);
    out.pixels.par_iter_mut().enumerate().for_each(|(i, o)| {
        let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
        *o = shade(&plan, src.at(x, y), plate.map(|pl| pl.at(x, y)));
    });
    Ok(out)
}

/// The GPU path (`gpu::ops::eval`'s arm), with key_color.wgsl.
#[cfg(feature = "gpu")]
pub mod gpu {
    use super::{ColorDifferenceKey, Plan, OP};
    use crate::gpu::image::{dispatch_2d, placeholder, storage_buffer, GpuImage};
    use crate::gpu::Gpu;
    use crate::image::{IRect, Image};
    use anyhow::Result;
    use serde_json::Value;
    use std::sync::Arc;
    use wgpu::util::DeviceExt;

    const SHADER: &str = include_str!("gpu/key_color.wgsl");

    fn win(r: IRect) -> [i32; 4] {
        [r.x0, r.y0, r.width() as i32, r.height() as i32]
    }

    /// key_color.wgsl's `Params`, as words.
    fn uniform(plan: &Plan, src: &GpuImage, plate: Option<&GpuImage>) -> Vec<u32> {
        let i = |v: [i32; 4]| v.map(|x| x as u32);
        let f = |v: [f32; 4]| v.map(f32::to_bits);
        let [p, a, b] = plan.chans.map(|c| c as u32);
        [
            i(win(plan.window)),
            i(win(src.window)),
            i(plate.map_or([0; 4], |pl| win(pl.window))),
            [plan.output as u32, u32::from(plate.is_some()), u32::from(plan.subtract), 0],
            [p, a, b, 0],
            f(plan.screen),
            f([plan.weights[0], plan.weights[1], plan.clip_black, plan.span]),
            f([plan.despill, 0.0, 0.0, 0.0]),
        ]
        .concat()
    }

    /// Run the op: None when it can't here (bad params, no input: the CPU
    /// then reports it). `cpu` reads input `n` back, for a screen estimate.
    pub fn eval(gpu: &Gpu, params: &Value, inputs: &[Option<&GpuImage>], cpu: &dyn Fn(usize) -> Result<Arc<Image>>) -> Result<Option<GpuImage>> {
        let Ok(p) = crate::ops::parse::<ColorDifferenceKey>(params, OP) else { return Ok(None) };
        let Some(src) = inputs.first().copied().flatten() else { return Ok(None) };
        let plate = inputs.get(1).copied().flatten();
        let windows: Vec<IRect> = [Some(src), plate].into_iter().flatten().map(|i| i.window).collect();
        // The screen estimate is a reduction over the plate: on the CPU.
        let Ok(plan) = super::plan(&p, src.format, &windows, || Ok(crate::ops::estimate_screen(&*cpu(0)?))) else { return Ok(None) };
        let window = plan.window;
        let dst = storage_buffer(gpu, OP, (window.width() * window.height() * 16) as u64, gpu.transfer_path(), false)?;
        if !window.is_empty() {
            let none = placeholder(gpu);
            let plate_buf = plate.map_or(&none, |pl| &pl.rgba);
            let pipeline = gpu.compute_pipeline("gpu/key_color", SHADER, "key");
            gpu.scoped(|| {
                let words = uniform(&plan, src, plate);
                let u = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(OP), contents: bytemuck::cast_slice(&words), usage: wgpu::BufferUsages::UNIFORM });
                let entries = [
                    wgpu::BindGroupEntry { binding: 0, resource: u.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: src.rgba.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: plate_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: dst.as_entire_binding() },
                ];
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(OP), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                dispatch_2d(&mut encoder, &pipeline, &bind, window);
                gpu.queue.submit([encoder.finish()]);
                Ok(())
            })?;
        }
        Ok(Some(GpuImage { format: src.format, window, rgba: dst, planes: Vec::new(), scene: None, transfer: gpu.transfer_path() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(mode: &str, subtract: bool, output: usize) -> Plan {
        let p = ColorDifferenceKey {
            screen_mode: mode.into(),
            screen: None,
            red_weight: 0.5,
            blue_weight: 0.5,
            clip_black: 0.0,
            clip_white: 1.0,
            subtract_screen: subtract,
            despill: 1.0,
            output: OUTPUTS[output].into(),
        };
        let f = Size { width: 4, height: 4 };
        plan(&p, f, &[IRect::frame(f)], || Ok([0.2, 0.3, 0.7, 1.0])).unwrap()
    }

    fn close(a: Rgba, b: Rgba) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn the_screen_keys_out_and_the_subject_stays() {
        let plan = plan_for("green", true, 0);
        assert!(close(shade(&plan, GREEN, None), [0.0; 4]));
        let skin = [0.8, 0.5, 0.35, 1.0];
        assert!(close(shade(&plan, skin, None), skin));
        // Matte output: black where the screen shows, opaque.
        let matte = plan_for("green", true, 1);
        assert!(close(shade(&matte, GREEN, None), [0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn a_half_mix_unmixes_to_the_subject() {
        let plan = plan_for("green", true, 0);
        // An orange with no color difference of its own (g = (r + b) / 2),
        // half over the screen.
        let orange = [0.6, 0.3, 0.0, 1.0];
        let mixed: Rgba = std::array::from_fn(|i| if i < 3 { 0.5 * orange[i] + 0.5 * GREEN[i] } else { 1.0 });
        let out = shade(&plan, mixed, None);
        // Half covered, and exactly the orange's half: no green cast.
        assert!(close(out, [0.3, 0.15, 0.0, 0.5]), "{out:?}");
        // Without subtraction the screen's color stays in the edge (its
        // blue and red), the green only despilled.
        let plain = shade(&plan_for("green", false, 0), mixed, None);
        assert!(close(plain, [0.175, 0.1, 0.025, 0.5]), "{plain:?}");
    }

    #[test]
    fn the_screen_plate_evens_out_uneven_lighting() {
        let plan = plan_for("green", true, 1);
        // A dim corner of the screen: against the preset it reads as part
        // matte, against the screen plate it's clear.
        let dim = [0.05, 0.3, 0.05, 1.0];
        assert!(shade(&plan, dim, None)[0] > 0.4);
        assert!(shade(&plan, dim, Some(dim))[0] < 1e-5);
        // A transparent screen-plate pixel falls back to the screen color.
        assert!(shade(&plan, GREEN, Some([0.0; 4]))[0] < 1e-5);
    }

    #[test]
    fn blue_and_picked_screens_and_despill() {
        let blue = plan_for("blue", true, 0);
        assert_eq!(blue.chans, [2, 0, 1]);
        assert!(close(shade(&blue, BLUE, None), [0.0; 4]));
        // Picked with nothing picked: the estimate, blue leading, keys blue.
        let picked = plan_for("picked", true, 1);
        assert_eq!((picked.screen, picked.chans), ([0.2, 0.3, 0.7, 1.0], [2, 0, 1]));
        // Despill only: the green cast held to the others' mix, alpha kept.
        let spill = [0.4, 0.7, 0.3, 0.5];
        let out = shade(&plan_for("green", true, 2), spill, None);
        assert!(close(out, [0.4, 0.35, 0.3, 0.5]), "{out:?}");
    }

    #[test]
    fn runs_as_an_op_with_defaults() {
        let f = Size { width: 8, height: 6 };
        let mut plate = Image::blank(f, IRect::frame(f));
        for (i, px) in plate.pixels.iter_mut().enumerate() {
            *px = if i % 8 < 4 { GREEN } else { [0.7, 0.4, 0.3, 1.0] };
        }
        let params = crate::ops::params_with_defaults(OP, &serde_json::json!({}));
        let out = crate::ops::eval(OP, &params, &[Some(&plate), None, None], &crate::ops::EvalContext { frame: 1, write_outputs: false }).unwrap();
        assert_eq!(out.window(), IRect::frame(f));
        assert!(out.at(1, 1)[3] < 1e-5 && (out.at(6, 1)[3] - 1.0).abs() < 1e-5);
        // A screen plate and a matte output cover the whole frame.
        let clean = Image { pixels: vec![GREEN; 48], ..Image::blank(f, IRect::frame(f)) };
        let params = crate::ops::params_with_defaults(OP, &serde_json::json!({"output": "matte"}));
        let out = crate::ops::eval(OP, &params, &[Some(&plate), Some(&clean), None], &crate::ops::EvalContext { frame: 1, write_outputs: false }).unwrap();
        assert_eq!(out.at(0, 0), [0.0, 0.0, 0.0, 1.0]);
    }
}
