//! The GPU self-test (`arclight gpu --check`, MCP / Python `gpu_check`,
//! Help ▸ GPU self-test): every GPU path run on fixed generated inputs on
//! both the CPU and the GPU, compared within its tolerance, and timed.
//! Each milestone adds its cases: the 2D ones here, the 3D and splat ones
//! through [`crate::render3d::gpu_checks`].

use super::Status;
use crate::image::{IRect, Image, Plane, Size};
use serde::Serialize;

/// One case at one size.
#[derive(Clone, Debug, Serialize)]
pub struct CheckRow {
    /// What ran: "upload / read back", "blur r 32 x 3", ...
    pub case: String,
    /// "1920x1080", "1001x777 window +13,+7", ...
    pub size: String,
    /// Largest difference between the GPU's and the CPU's values.
    pub max_abs: f64,
    /// Largest difference relative to the CPU's value (where it is over 1e-3).
    pub max_rel: f64,
    /// A value passes within `tol_abs` or within `tol_rel` of the CPU's.
    pub tol_abs: f64,
    pub tol_rel: f64,
    pub pass: bool,
    pub cpu_ms: f64,
    /// The GPU's work alone, its inputs already there.
    pub gpu_kernel_ms: f64,
    /// With the upload and the read-back.
    pub gpu_total_ms: f64,
    /// Why it failed, or "".
    pub note: String,
}

impl CheckRow {
    /// How many times faster the GPU was, transfers included.
    pub fn speedup(&self) -> f64 {
        if self.gpu_total_ms > 0.0 { self.cpu_ms / self.gpu_total_ms } else { 0.0 }
    }
}

/// The whole self-test.
#[derive(Clone, Debug, Serialize)]
pub struct CheckReport {
    pub status: Status,
    /// "unified memory", "discrete transfers", or "" when the GPU is off.
    pub transfer: String,
    pub rows: Vec<CheckRow>,
}

impl CheckReport {
    /// The GPU ran and every case passed.
    pub fn passed(&self) -> bool {
        self.status.is_active() && self.rows.iter().all(|r| r.pass)
    }

    /// One line: "8 / 8 passed on <adapter> (<backend>), <transfer>".
    pub fn summary(&self) -> String {
        match &self.status {
            Status::Active { adapter, backend, .. } => {
                let ok = self.rows.iter().filter(|r| r.pass).count();
                format!("{ok} / {} passed on {adapter} ({backend}), {}", self.rows.len(), self.transfer)
            }
            Status::Cpu { reason } => format!("not run: using the CPU: {reason}"),
        }
    }

    /// The rows as a plain-text table, then the summary.
    pub fn table(&self) -> String {
        if self.rows.is_empty() {
            return self.summary() + "\n";
        }
        let mut out = format!(
            "{:<28} {:<26} {:>9} {:>9} {:>17} {:>6} {:>9} {:>9} {:>9} {:>8}\n",
            "case", "size", "max abs", "max rel", "tolerance", "result", "CPU ms", "GPU ms", "+transfer", "speedup"
        );
        for r in &self.rows {
            out += &format!(
                "{:<28} {:<26} {:>9.2e} {:>9.2e} {:>17} {:>6} {:>9.2} {:>9.2} {:>9.2} {:>7.1}x\n",
                r.case,
                r.size,
                r.max_abs,
                r.max_rel,
                if r.tol_abs == 0.0 && r.tol_rel == 0.0 { "exact".into() } else { format!("{:.0e} / {:.0e}", r.tol_abs, r.tol_rel) },
                if r.pass { "pass" } else { "FAIL" },
                r.cpu_ms,
                r.gpu_kernel_ms,
                r.gpu_total_ms,
                r.speedup()
            );
            if !r.note.is_empty() {
                out += &format!("    {}\n", r.note);
            }
        }
        out + &self.summary() + "\n"
    }
}

/// Compare a GPU result with the CPU's over both data windows (outside
/// a window counts as transparent), planes by name, and make a row. A
/// value passes within `tol_abs` or within `tol_rel × |cpu|`; NaN only
/// matches NaN.
#[allow(clippy::too_many_arguments)]
pub fn compare(case: &str, size: &str, cpu: &Image, gpu: &Image, tol_abs: f64, tol_rel: f64, cpu_ms: f64, gpu_kernel_ms: f64, gpu_total_ms: f64) -> CheckRow {
    let mut row = CheckRow { case: case.into(), size: size.into(), max_abs: 0.0, max_rel: 0.0, tol_abs, tol_rel, pass: true, cpu_ms, gpu_kernel_ms, gpu_total_ms, note: String::new() };
    let mut fail = |note: String| {
        if row.note.is_empty() {
            row.note = note;
        }
    };
    if cpu.format != gpu.format {
        fail(format!("frame {:?} on the GPU, {:?} on the CPU", gpu.format, cpu.format));
    }
    let mut bad = 0usize;
    let mut value = |c: f32, g: f32| {
        let (c, g) = (c as f64, g as f64);
        if c.is_nan() || g.is_nan() {
            if c.is_nan() != g.is_nan() {
                bad += 1;
            }
            return;
        }
        let d = if c == g { 0.0 } else { (g - c).abs() };
        row.max_abs = row.max_abs.max(d);
        // (Relative to values big enough for it to mean something.)
        if c.abs() > 1e-3 {
            row.max_rel = row.max_rel.max(d / c.abs());
        }
        if d > tol_abs && d > tol_rel * c.abs() {
            bad += 1;
        }
    };
    let w = cpu.window().union(&gpu.window());
    for y in w.y0..w.y1 {
        for x in w.x0..w.x1 {
            for (c, g) in cpu.at(x, y).into_iter().zip(gpu.at(x, y)) {
                value(c, g);
            }
            for p in &cpu.planes {
                if gpu.plane(&p.name).is_some() {
                    value(cpu.plane_at(&p.name, x, y), gpu.plane_at(&p.name, x, y));
                }
            }
        }
    }
    let names = |img: &Image| img.planes.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    if names(cpu) != names(gpu) {
        fail(format!("planes {:?} on the GPU, {:?} on the CPU", names(gpu), names(cpu)));
    }
    if bad > 0 {
        fail(format!("{bad} values out of tolerance"));
    }
    row.pass = row.note.is_empty();
    row
}

/// The share of values a warp may have out of tolerance (D8's statistical
/// rule, as for 3D): f32 positions differ from the CPU's by a hair, which
/// matters where a footprint's tap count changes (at 1.001 px) and at hard
/// data edges, and decides ties for a plane's nearest pixel.
pub const WARP_SHARE: f64 = 1e-3;

/// [`compare`], but passing with up to `share` of the values out of
/// tolerance (the note then says how many).
#[allow(clippy::too_many_arguments)]
pub fn compare_share(case: &str, size: &str, cpu: &Image, gpu: &Image, tol_abs: f64, tol_rel: f64, share: f64, cpu_ms: f64, gpu_kernel_ms: f64, gpu_total_ms: f64) -> CheckRow {
    let mut row = compare(case, size, cpu, gpu, tol_abs, tol_rel, cpu_ms, gpu_kernel_ms, gpu_total_ms);
    let w = cpu.window().union(&gpu.window());
    let values = (w.width() * w.height() * (4 + cpu.planes.len())).max(1) as f64;
    let off = row.note.strip_suffix(" values out of tolerance").and_then(|n| n.parse::<f64>().ok());
    if let Some(off) = off {
        row.pass = off / values <= share;
        row.note = format!("{:.3} % of values out of tolerance (up to {:.1} % allowed)", 100.0 * off / values, 100.0 * share);
    }
    row
}

/// A deterministic test image of frame `format` over `window`: noise
/// with HDR values (up to 4) and every alpha, premultiplied, and a
/// "depth" plane when `plane`.
pub fn noise(format: Size, window: IRect, plane: bool, seed: u32) -> Image {
    let hash = |i: u32| {
        let mut h = i.wrapping_mul(0x9E37_79B9) ^ seed.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 16;
        h = h.wrapping_mul(0x7FEB_352D);
        h ^= h >> 15;
        (h as f32) / (u32::MAX as f32)
    };
    let mut img = Image::blank(format, window);
    for (i, px) in img.pixels.iter_mut().enumerate() {
        let i = i as u32 * 4;
        let a = hash(i + 3);
        *px = [hash(i) * 4.0 * a, hash(i + 1) * a, hash(i + 2) * a, a];
    }
    if plane {
        let data = (0..img.pixels.len() as u32).map(|i| 1.0 + 100.0 * hash(i ^ 0x5555)).collect();
        img.planes.push(Plane { name: "depth".into(), data });
    }
    img
}

/// A deterministic smooth test image (soft waves in color and alpha,
/// premultiplied, a "depth" plane when `plane`): for warps, whose f32
/// positions differ from the CPU's by a hair, which noise would turn into
/// big color differences.
pub fn smooth(format: Size, window: IRect, plane: bool, seed: u32) -> Image {
    let s = seed as f32;
    let mut img = Image::blank(format, window);
    let w = window.width().max(1);
    for (i, px) in img.pixels.iter_mut().enumerate() {
        let (x, y) = ((window.x0 + (i % w) as i32) as f32, (window.y0 + (i / w) as i32) as f32);
        let a = 0.6 + 0.4 * (x * 0.031 + s).sin() * (y * 0.023).cos();
        let c = [0.5 + 0.45 * (x * 0.05 + y * 0.02 + s).sin(), 0.5 + 0.45 * (y * 0.043 - s).cos(), 0.6 + 0.5 * ((x + y) * 0.017).sin()];
        *px = [c[0] * a, c[1] * a, c[2] * a, a];
    }
    if plane {
        // In 16-pixel blocks: planes are carried nearest, and where a
        // position lies within a hair of halfway between two pixels the
        // CPU and the GPU may pick different ones; inside a block that
        // changes nothing, while a carry gone wrong still shows.
        let data = (0..img.pixels.len())
            .map(|i| {
                let (x, y) = (window.x0 + (i % w) as i32, window.y0 + (i / w) as i32);
                10.0 + (x.div_euclid(16) * 7 + y.div_euclid(16) * 3) as f32
            })
            .collect();
        img.planes.push(Plane { name: "depth".into(), data });
    }
    img
}

/// `img` over another data window, on the CPU ([`super::GpuImage::rewindow`]'s
/// reference).
pub fn rewindowed(img: &Image, window: IRect) -> Image {
    let mut out = Image::blank(img.format, window);
    let w = window.width().max(1);
    for (i, px) in out.pixels.iter_mut().enumerate() {
        *px = img.at(window.x0 + (i % w) as i32, window.y0 + (i / w) as i32);
    }
    for p in &img.planes {
        let data = (0..out.pixels.len()).map(|i| img.plane_at(&p.name, window.x0 + (i % w) as i32, window.y0 + (i / w) as i32)).collect();
        out.planes.push(Plane { name: p.name.clone(), data });
    }
    out
}

/// Run `f` and how long it took, in ms.
pub fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = std::time::Instant::now();
    let out = f();
    (out, t.elapsed().as_secs_f64() * 1e3)
}

/// Run every case. Blocking (seconds at 4K): call it off the UI thread.
/// With the GPU off the report has no rows and says why.
pub fn run() -> CheckReport {
    let status = super::status();
    #[cfg(feature = "gpu")]
    if let Some(gpu) = super::get() {
        let sizes = sizes();
        let mut rows = image_rows(&gpu, &sizes);
        rows.extend(op_rows(&gpu, &sizes));
        rows.extend(crate::render3d::gpu_checks(&gpu));
        return CheckReport { status, transfer: gpu.transfer().into(), rows };
    }
    CheckReport { status, transfer: String::new(), rows: Vec::new() }
}

/// A size cases run at: its name, frame, data window, and whether the
/// image has a plane.
pub type Sized = (String, Size, IRect, bool);

/// [`run`] quickly (a few seconds): the 2D cases at the odd size only, no
/// 3D ones. For tests and a first look.
pub fn run_quick() -> CheckReport {
    let status = super::status();
    #[cfg(feature = "gpu")]
    if let Some(gpu) = super::get() {
        let small = vec![sizes().pop().expect("sizes")];
        let mut rows = image_rows(&gpu, &small);
        rows.extend(op_rows(&gpu, &small));
        return CheckReport { status, transfer: gpu.transfer().into(), rows };
    }
    CheckReport { status, transfer: String::new(), rows: Vec::new() }
}

/// The sizes cases run at: full HD, 4K, and an odd window off the frame's
/// corner (with a plane).
pub fn sizes() -> Vec<Sized> {
    let full = |w: usize, h: usize| (format!("{w}x{h}"), Size { width: w, height: h }, IRect::new(0, 0, w as i32, h as i32), false);
    vec![full(1920, 1080), full(3840, 2160), ("1001x777 window +13,+7".into(), Size { width: 1024, height: 800 }, IRect::new(13, 7, 1014, 784), true)]
}

/// Upload / read back (exact) and the window change kernel, at each size;
/// the other transfer path too when the device has it.
#[cfg(feature = "gpu")]
fn image_rows(gpu: &super::Gpu, sizes: &[Sized]) -> Vec<CheckRow> {
    use super::{GpuImage, Transfer};
    let failed = |case: &str, size: &str, e: anyhow::Error| CheckRow {
        case: case.into(),
        size: size.into(),
        max_abs: 0.0,
        max_rel: 0.0,
        tol_abs: 0.0,
        tol_rel: 0.0,
        pass: false,
        cpu_ms: 0.0,
        gpu_kernel_ms: 0.0,
        gpu_total_ms: 0.0,
        note: format!("{e:#}"),
    };
    let mut rows = Vec::new();
    // Pipelines and first buffers built before anything is timed.
    let warm = noise(Size { width: 64, height: 64 }, IRect::new(0, 0, 64, 64), true, 0);
    let _ = GpuImage::upload(gpu, &warm).and_then(|up| up.rewindow(gpu, IRect::new(-8, -8, 72, 72))?.read_back(gpu));
    let round_trip = |img: &Image, transfer: Transfer| -> anyhow::Result<(Image, f64)> {
        let (back, ms) = timed(|| GpuImage::upload_via(gpu, img, transfer)?.read_back(gpu));
        Ok((back?, ms))
    };
    for (i, (size, format, window, plane)) in sizes.iter().cloned().enumerate() {
        let img = noise(format, window, plane, i as u32 + 1);
        let (_, clone_ms) = timed(|| img.clone());
        match round_trip(&img, gpu.transfer_path()) {
            Ok((back, ms)) => rows.push(compare("upload / read back", &size, &img, &back, 0.0, 0.0, clone_ms, 0.0, ms)),
            Err(e) => rows.push(failed("upload / read back", &size, e)),
        }
        // Grown by 32 on each side and moved: transparent margins.
        let target = IRect::new(window.x0 - 40, window.y0 - 24, window.x1 + 24, window.y1 + 40);
        let (want, cpu_ms) = timed(|| rewindowed(&img, target));
        let run = || -> anyhow::Result<CheckRow> {
            let (result, total_ms) = timed(|| -> anyhow::Result<_> {
                let up = GpuImage::upload(gpu, &img)?;
                let (out, kernel_ms) = timed(|| -> anyhow::Result<_> {
                    let out = up.rewindow(gpu, target)?;
                    gpu.wait()?;
                    Ok(out)
                });
                Ok((out?.read_back(gpu)?, kernel_ms))
            });
            let (got, kernel_ms) = result?;
            Ok(compare("window change", &size, &want, &got, 0.0, 0.0, cpu_ms, kernel_ms, total_ms))
        };
        rows.push(run().unwrap_or_else(|e| failed("window change", &size, e)));
    }
    // The other transfer path, so both are exercised where the device can.
    if gpu.features.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS) {
        let other = if gpu.unified { Transfer::Staging } else { Transfer::Unified };
        let Some((size, format, window, plane)) = sizes.last().cloned() else { return rows };
        let img = noise(format, window, plane, 7);
        let case = format!("upload / read back ({})", if other == Transfer::Unified { "shared" } else { "staging" });
        match round_trip(&img, other) {
            Ok((back, ms)) => {
                let mut row = compare(&case, &size, &img, &back, 0.0, 0.0, timed(|| img.clone()).1, 0.0, ms);
                if row.pass && other == Transfer::Unified {
                    row.note = "checks the shared-memory path; slow on a discrete GPU by nature, not used there".into();
                }
                rows.push(row);
            }
            Err(e) => rows.push(failed(&case, &size, e)),
        }
    }
    rows
}

/// A Transform's params with 8 motion-blur samples (as
/// `ops::resolve_params` adds them): turning 0 to 7 degrees.
#[cfg(feature = "gpu")]
fn motion_blurred() -> serde_json::Value {
    let sample = |r: f64| crate::ops::params_with_defaults("transform", &serde_json::json!({"rotate": r}));
    serde_json::json!({"rotate": 3.5, "_motion": (0..8).map(|i| sample(i as f64)).collect::<Vec<_>>()})
}

/// The 2D ops on the GPU (`gpu::ops`) against `ops::eval`, at each size:
/// (case, op, params, tolerance abs / rel, with a second input, with a
/// mask).
#[cfg(feature = "gpu")]
fn op_rows(gpu: &super::Gpu, sizes: &[Sized]) -> Vec<CheckRow> {
    use serde_json::json;
    const POINT: (f64, f64) = (1e-6, 1e-5);
    const FILTER: (f64, f64) = (1e-4, 1e-4);
    // (case, op, params, tolerance, second input, mask, smooth input)
    let cases = [
        ("grade", "grade", json!({"multiply": [1.1, 0.9, 1.0], "offset": [0.01, 0.0, -0.01], "gamma": [2.2, 1.0, 0.8]}), POINT, false, false, false),
        ("grade + mask, mix 0.5", "grade", json!({"gamma": [1.6, 1.6, 1.6], "mix": 0.5, "mask_channel": "luminance"}), POINT, false, true, false),
        ("merge screen", "merge", json!({"operation": "screen"}), POINT, true, false, false),
        ("merge zmerge", "merge", json!({"operation": "zmerge"}), POINT, true, false, false),
        ("shuffle", "shuffle", json!({"red": "G", "green": "R", "blue": "0", "alpha": "1"}), (0.0, 0.0), false, false, false),
        ("blur r 8 x 3", "blur", json!({"radius": 8, "passes": 3}), FILTER, false, false, false),
        ("blur r 32 x 3", "blur", json!({"radius": 32, "passes": 3}), FILTER, false, false, false),
        ("blur r 100 x 3", "blur", json!({"radius": 100, "passes": 3}), FILTER, false, false, false),
        ("transform rotate 10", "transform", json!({"rotate": 10}), FILTER, false, false, true),
        ("transform, motion blur 8", "transform", motion_blurred(), FILTER, false, false, true),
        ("corner pin", "corner_pin", json!({"to1": [90, 60], "to2": [1700, 20], "to3": [1800, 1000], "to4": [40, 1060], "from1": [0, 0], "from2": [1920, 0], "from3": [1920, 1080], "from4": [0, 1080]}), FILTER, false, false, true),
        ("lens undistort", "lens_distort", json!({"mode": "undistort", "k1": -0.1, "k2": 0.02}), FILTER, false, false, true),
        ("lens distort", "lens_distort", json!({"mode": "distort", "k1": -0.1, "k2": 0.02}), FILTER, false, false, true),
        ("resize x2", "resize", json!({"width": 2048, "height": 2048, "fit": "fit"}), FILTER, false, false, true),
        ("resize x0.5", "resize", json!({"width": 512, "height": 512, "fit": "fit"}), FILTER, false, false, true),
        ("keyer, shrink / grow, soft", "keyer", json!({"screen": [0.1, 0.75, 0.15, 1.0], "shrink_grow": 2.5, "softness": 9}), FILTER, false, false, true),
    ];
    let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
    let mut rows = Vec::new();
    // Each case once on a small image first, so building its pipeline
    // isn't timed.
    let mut sizes = sizes.to_vec();
    sizes.insert(0, ("warm-up".into(), Size { width: 64, height: 48 }, IRect::new(0, 0, 64, 48), true));
    for (i, (size, format, window, plane)) in sizes.into_iter().enumerate() {
        let a = noise(format, window, plane, 10 + i as u32);
        let b = noise(format, IRect::new(window.x0 + 31, window.y0 - 17, window.x1 - 5, window.y1 + 9), false, 20 + i as u32);
        let soft = smooth(format, window, plane, 30 + i as u32);
        for (case, op, params, (tol_abs, tol_rel), two, masked, smooth_input) in &cases {
            let params = crate::ops::params_with_defaults(op, params);
            let mut inputs: Vec<Option<&Image>> = vec![Some(if *smooth_input { &soft } else { &a })];
            if *two {
                inputs.push(Some(&b));
            }
            if *masked || !matches!(*op, "merge" | "shuffle") {
                inputs.push(masked.then_some(&b));
            }
            let (want, cpu_ms) = timed(|| crate::ops::eval(op, &params, &inputs, &ctx));
            let run = || -> anyhow::Result<CheckRow> {
                let want = want.as_ref().map_err(|e| anyhow::anyhow!("on the CPU: {e:#}"))?;
                let (result, total_ms) = timed(|| -> anyhow::Result<_> {
                    let up: Vec<Option<super::GpuImage>> = inputs.iter().map(|i| i.map(|i| super::GpuImage::upload(gpu, i)).transpose()).collect::<anyhow::Result<_>>()?;
                    gpu.wait()?;
                    let refs: Vec<Option<&super::GpuImage>> = up.iter().map(Option::as_ref).collect();
                    let (out, kernel_ms) = timed(|| -> anyhow::Result<_> {
                        let cpu = |i: usize| inputs.get(i).copied().flatten().map(|i| std::sync::Arc::new(i.clone())).ok_or_else(|| anyhow::anyhow!("no input {i}"));
                        let out = super::ops::eval(gpu, op, &params, &refs, &cpu)?.ok_or_else(|| anyhow::anyhow!("declined"))?;
                        gpu.wait()?;
                        Ok(out)
                    });
                    Ok((out?.read_back(gpu)?, kernel_ms))
                });
                let (got, kernel_ms) = result?;
                Ok(if matches!(*op, "transform" | "corner_pin" | "lens_distort") {
                    compare_share(case, &size, want, &got, *tol_abs, *tol_rel, WARP_SHARE, cpu_ms, kernel_ms, total_ms)
                } else {
                    compare(case, &size, want, &got, *tol_abs, *tol_rel, cpu_ms, kernel_ms, total_ms)
                })
            };
            rows.push(run().unwrap_or_else(|e| CheckRow {
                case: case.to_string(),
                size: size.clone(),
                max_abs: 0.0,
                max_rel: 0.0,
                tol_abs: *tol_abs,
                tol_rel: *tol_rel,
                pass: false,
                cpu_ms,
                gpu_kernel_ms: 0.0,
                gpu_total_ms: 0.0,
                note: format!("{e:#}"),
            }));
        }
    }
    rows.retain(|r| r.size != "warm-up" || !r.pass);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerances: within either bound passes; NaN matches only NaN; a
    /// pixel only one side has counts against transparent.
    #[test]
    fn compare_applies_the_tolerances() {
        let f = Size { width: 4, height: 1 };
        let mut a = Image::blank(f, IRect::new(0, 0, 2, 1));
        a.pixels = vec![[1.0, 100.0, 0.0, f32::NAN], [0.5, 0.5, 0.5, 0.5]];
        let mut b = a.clone();
        b.pixels[0] = [1.0 + 5e-7, 100.0 + 5e-4, 5e-7, f32::NAN];
        let row = compare("c", "s", &a, &b, 1e-6, 1e-5, 2.0, 0.5, 1.0);
        assert!(row.pass, "{row:?}");
        assert!((row.max_abs - 5e-4).abs() < 1e-5 && row.speedup() == 2.0, "{row:?}");
        b.pixels[0][1] = 101.0;
        assert!(!compare("c", "s", &a, &b, 1e-6, 1e-5, 0.0, 0.0, 0.0).pass);
        b.pixels[0] = a.pixels[0];
        b.pixels[0][3] = 0.0;
        assert!(!compare("c", "s", &a, &b, 1e-6, 1e-5, 0.0, 0.0, 0.0).pass, "NaN against a number");
        let mut c = a.clone();
        c.x0 = 1;
        let row = compare("c", "s", &a, &c, 1e-6, 1e-5, 0.0, 0.0, 0.0);
        assert!(!row.pass && row.note.contains("out of tolerance"), "{row:?}");
        // A share of the values off: 1 of 8 here.
        let mut d = a.clone();
        d.pixels[1][0] = 0.9;
        assert!(compare_share("c", "s", &a, &d, 1e-6, 1e-5, 0.2, 0.0, 0.0, 0.0).pass);
        let row = compare_share("c", "s", &a, &d, 1e-6, 1e-5, 0.1, 0.0, 0.0, 0.0);
        assert!(!row.pass && row.note.starts_with("12.500 %"), "{row:?}");
    }

    /// The quick self-test passes on this machine's GPU (the full run, 4K
    /// and the 3D cases included, is `arclight gpu --check`: minutes in a
    /// debug build); skipped without a GPU.
    #[test]
    fn the_self_test_passes() {
        if super::super::get().is_none() {
            return eprintln!("skipped: no GPU adapter");
        }
        let report = run_quick();
        assert!(report.passed(), "{}", report.table());
        assert!(report.rows.len() >= 16 && report.summary().contains("passed on"), "{}", report.table());
    }
}
