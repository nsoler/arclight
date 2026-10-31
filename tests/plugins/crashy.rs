//! A plugin that misbehaves on purpose, for tests/plugin_isolation.rs
//! (built there with rustc; no dependencies). Its ops (named `inproc_…`
//! instead when built with `--cfg inproc`, to load it in-process beside
//! itself):
//!
//! - `crashy_ok`: inverts its input, and adds the frame / 1000 to red:
//!   plain work, to check results and that a host lives on.
//! - `crashy_twice`: an output twice its input's width (bigger than the room
//!   a host keeps for it).
//! - `crashy_print`: prints to stdout, then works (a host's replies own it).
//! - `crashy_error`: fails, saying why.
//! - `crashy_segv`, `crashy_abort`, `crashy_hang`: crash or never return.

use std::ffi::{c_char, c_void, CStr};

#[repr(C)]
pub struct ArclightImage {
    width: u32,
    height: u32,
    pixels: *const f32,
}

type Alloc = unsafe extern "C" fn(*mut c_void, u32, u32) -> *mut f32;

#[repr(C)]
pub struct ArclightPlugin {
    abi_version: u32,
    ops_json: *const c_char,
    render: unsafe extern "C" fn(*const c_char, *const c_char, *const ArclightImage, u32, i64, *mut c_void, Alloc, *mut c_char, usize) -> i32,
}

unsafe impl Sync for ArclightPlugin {}

macro_rules! ops_json {
    ($p:literal) => {
        concat!(
            r#"[{"name": ""#, $p, r#"ok", "time_varying": true, "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"twice", "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"print", "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"error", "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"segv", "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"abort", "inputs": [{"label": "input"}]},"#,
            r#"{"name": ""#, $p, r#"hang", "inputs": [{"label": "input"}]}]"#,
            "\0"
        )
    };
}

#[cfg(not(inproc))]
const OPS_JSON: &str = ops_json!("crashy_");
#[cfg(inproc)]
const OPS_JSON: &str = ops_json!("inproc_");

static PLUGIN: ArclightPlugin = ArclightPlugin { abi_version: 1, ops_json: OPS_JSON.as_ptr() as *const c_char, render };

#[no_mangle]
pub extern "C" fn arclight_plugin_entry() -> *const ArclightPlugin {
    &PLUGIN
}

unsafe extern "C" fn render(
    op: *const c_char,
    _params: *const c_char,
    inputs: *const ArclightImage,
    n_inputs: u32,
    frame: i64,
    host: *mut c_void,
    alloc: Alloc,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    let op = CStr::from_ptr(op).to_str().unwrap_or("");
    let op = op.split_once('_').map_or(op, |(_, o)| o);
    if n_inputs < 1 || (*inputs).pixels.is_null() {
        return fail(error, error_len, "input is not connected");
    }
    let src = &*inputs;
    let (w, h) = (src.width as usize, src.height as usize);
    let input = std::slice::from_raw_parts(src.pixels, w * h * 4);
    match op {
        "segv" => {
            std::ptr::null_mut::<u32>().write_volatile(1);
            0
        }
        "abort" => std::process::abort(),
        "hang" => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        "error" => fail(error, error_len, "crashy says no"),
        "twice" => {
            let out = std::slice::from_raw_parts_mut(alloc(host, src.width * 2, src.height), w * 2 * h * 4);
            for y in 0..h {
                for x in 0..w * 2 {
                    let i = (y * w + x % w) * 4;
                    out[(y * w * 2 + x) * 4..][..4].copy_from_slice(&input[i..i + 4]);
                }
            }
            0
        }
        "ok" | "print" => {
            if op == "print" {
                println!("crashy was here");
            }
            let out = std::slice::from_raw_parts_mut(alloc(host, src.width, src.height), w * h * 4);
            for (o, i) in out.chunks_mut(4).zip(input.chunks(4)) {
                o[0] = i[3] - i[0] + frame as f32 / 1000.0;
                o[1] = i[3] - i[1];
                o[2] = i[3] - i[2];
                o[3] = i[3];
            }
            0
        }
        _ => fail(error, error_len, "unknown op"),
    }
}

unsafe fn fail(buf: *mut c_char, len: usize, msg: &str) -> i32 {
    let n = msg.len().min(len - 1);
    std::ptr::copy_nonoverlapping(msg.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
    1
}
