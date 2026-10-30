//! Example Arclight plugin implementing the C ABI in
//! `include/arclight_plugin.h`: a Vignette and an animated Film Grain op.
//!
//! Build with `cargo build --release -p arclight-plugin-example` and copy
//! `target/release/libarclight_plugin_example.so` into a plugin directory
//! (see `arclight plugins`), or point `ARCLIGHT_PLUGIN_PATH` at `target/release`.

use serde_json::Value;
use std::ffi::{c_char, c_void, CStr};

#[repr(C)]
pub struct ArclightImage {
    width: u32,
    height: u32,
    pixels: *const f32,
}

type Alloc = unsafe extern "C" fn(host: *mut c_void, width: u32, height: u32) -> *mut f32;

#[repr(C)]
pub struct ArclightPlugin {
    abi_version: u32,
    ops_json: *const c_char,
    render: unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const ArclightImage,
        u32,
        i64,
        *mut c_void,
        Alloc,
        *mut c_char,
        usize,
    ) -> i32,
}

unsafe impl Sync for ArclightPlugin {}

const OPS_JSON: &str = concat!(
    r#"[
  {"name": "vignette", "label": "Vignette", "category": "color",
   "help": "Darkens the image toward its corners. Radius is where darkening starts, as a fraction of the half-diagonal.",
   "inputs": [{"label": "input"}],
   "params": [
     {"name": "amount", "label": "Amount", "kind": "float", "min": 0, "max": 1, "default": 0.5},
     {"name": "radius", "label": "Radius", "kind": "float", "min": 0, "max": 1.5, "default": 0.6},
     {"name": "softness", "label": "Softness", "kind": "float", "min": 0.01, "max": 1.5, "default": 0.5}
   ]},
  {"name": "film_grain", "label": "Film Grain", "category": "filter", "time_varying": true,
   "help": "Adds per-frame luminance noise, stronger in the midtones.",
   "inputs": [{"label": "input"}],
   "params": [
     {"name": "amount", "label": "Amount", "kind": "float", "min": 0, "max": 0.5, "default": 0.05},
     {"name": "seed", "label": "Seed", "kind": "int", "min": 0, "max": 100000, "default": 1}
   ]}
]"#,
    "\0"
);

static PLUGIN: ArclightPlugin = ArclightPlugin { abi_version: 1, ops_json: OPS_JSON.as_ptr() as *const c_char, render };

#[no_mangle]
pub extern "C" fn arclight_plugin_entry() -> *const ArclightPlugin {
    &PLUGIN
}

unsafe fn set_error(buf: *mut c_char, len: usize, msg: &str) {
    if buf.is_null() || len == 0 {
        return;
    }
    let n = msg.len().min(len - 1);
    std::ptr::copy_nonoverlapping(msg.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
}

unsafe extern "C" fn render(
    op: *const c_char,
    params: *const c_char,
    inputs: *const ArclightImage,
    n_inputs: u32,
    frame: i64,
    host: *mut c_void,
    alloc: Alloc,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    let op = CStr::from_ptr(op).to_str().unwrap_or("");
    let params: Value = serde_json::from_str(CStr::from_ptr(params).to_str().unwrap_or("{}")).unwrap_or_default();
    let num = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    if n_inputs < 1 || (*inputs).pixels.is_null() {
        set_error(error, error_len, "input is not connected");
        return 1;
    }
    let src = &*inputs;
    let (w, h) = (src.width as usize, src.height as usize);
    let input = std::slice::from_raw_parts(src.pixels, w * h * 4);
    let out = std::slice::from_raw_parts_mut(alloc(host, src.width, src.height), w * h * 4);
    match op {
        "vignette" => vignette(input, out, w, h, num("amount"), num("radius"), num("softness").max(1e-3)),
        "film_grain" => grain(input, out, num("amount"), num("seed") as u64, frame),
        _ => {
            set_error(error, error_len, &format!("unknown op '{op}'"));
            return 1;
        }
    }
    0
}

fn vignette(input: &[f32], out: &mut [f32], w: usize, h: usize, amount: f32, radius: f32, softness: f32) {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let half_diag = (cx * cx + cy * cy).sqrt().max(1.0);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt() / half_diag;
            let t = ((d - radius) / softness).clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t);
            let gain = 1.0 - amount * t;
            let i = (y * w + x) * 4;
            // Color is premultiplied, so scaling RGB alone darkens without touching coverage.
            for c in 0..3 {
                out[i + c] = input[i + c] * gain;
            }
            out[i + 3] = input[i + 3];
        }
    }
}

/// SplitMix64: a small, well-distributed hash for per-pixel noise.
fn hash(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn grain(input: &[f32], out: &mut [f32], amount: f32, seed: u64, frame: i64) {
    let base = hash(seed ^ hash(frame as u64));
    for (i, (src, dst)) in input.as_chunks::<4>().0.iter().zip(out.as_chunks_mut::<4>().0.iter_mut()).enumerate() {
        let noise = (hash(base ^ i as u64) >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
        let a = src[3];
        let luma = if a > 0.0 { (0.2126 * src[0] + 0.7152 * src[1] + 0.0722 * src[2]) / a } else { 0.0 };
        let weight = 4.0 * luma.clamp(0.0, 1.0) * (1.0 - luma.clamp(0.0, 1.0));
        let n = noise * amount * (0.25 + 0.75 * weight) * a;
        for c in 0..3 {
            dst[c] = (src[c] + n).max(0.0);
        }
        dst[3] = a;
    }
}
