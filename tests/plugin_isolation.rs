//! Plugins in helper processes (src/plugin_host.rs), with the CLI binary
//! as the host and tests/plugins/crashy.rs as the plugin: crashes, aborts
//! and hangs become node errors and the next render works; results match
//! the same plugin in-process bit for bit.
#![cfg(unix)]

use arclight::graph::{Evaluator, Graph};
use arclight::image::Image;
use arclight::ops::EvalContext;
use arclight::plugin;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const HOST: &str = env!("CARGO_BIN_EXE_arclight");

/// tests/plugins/crashy.rs built into `dir`, its ops named `prefix…`.
fn build_crashy(dir: &Path, prefix: &str) -> PathBuf {
    let out = dir.join(format!("libarclight_plugin_{prefix}{}", std::env::consts::DLL_SUFFIX));
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let mut cmd = Command::new(rustc);
    cmd.args(["--edition", "2021", "--crate-type", "cdylib", "-O", "-o"]).arg(&out).arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/plugins/crashy.rs"));
    if prefix == "inproc" {
        cmd.args(["--cfg", "inproc"]);
    }
    assert!(cmd.status().expect("rustc runs").success(), "building the test plugin");
    out
}

/// `op` over a 64 x 48 checkerboard at `frame`.
fn render(op: &str, frame: i64) -> Result<Arc<Image>, String> {
    let g: Graph = serde_json::from_value(serde_json::json!({"nodes": [
        {"id": "cb", "op": "checkerboard", "params": {"width": 64, "height": 48, "size": 5}},
        {"id": "p", "op": op, "inputs": ["cb"]},
    ]}))
    .unwrap();
    Evaluator::new().evaluate(&g, "p", &EvalContext { frame, write_outputs: false }).map_err(|e| e.to_string())
}

fn bits(img: &Image) -> (usize, usize, Vec<u32>) {
    (img.width, img.height, img.pixels.iter().flatten().map(|v| v.to_bits()).collect())
}

/// One test, as the plugin registry is the process's.
#[test]
fn plugins_run_in_hosts_that_crash_alone() {
    let dir = std::env::temp_dir().join(format!("arclight_isolation_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Remembered ops and crash reports kept here.
    std::env::set_var("XDG_STATE_HOME", dir.join("state"));
    std::env::set_var("ARCLIGHT_CRASH_DIR", dir.join("crashes"));
    std::env::remove_var("ARCLIGHT_PLUGINS_IN_PROCESS");
    let (crashy, inproc) = (build_crashy(&dir, "crashy"), build_crashy(&dir, "inproc"));

    // The same plugin in-process (no host command yet), then hosted.
    plugin::load(&inproc).unwrap();
    plugin::set_host_command(vec![HOST.into(), "plugin-host".into()]);
    plugin::set_timeout(Duration::from_secs(3));
    let ops = plugin::load(&crashy).unwrap();
    assert!(ops.contains(&"crashy_ok".to_string()), "{ops:?}");

    // Bit for bit as in-process; an output bigger than its input; a plugin
    // printing to stdout doesn't garble the replies.
    for (hosted, local, frame) in [("crashy_ok", "inproc_ok", 1), ("crashy_ok", "inproc_ok", 7), ("crashy_twice", "inproc_twice", 1), ("crashy_print", "inproc_print", 1)] {
        let (a, b) = (render(hosted, frame).unwrap(), render(local, frame).unwrap());
        assert_eq!(bits(&a), bits(&b), "{hosted} at {frame}");
    }
    assert_eq!(render("crashy_twice", 1).unwrap().width, 128);

    // The plugin's own error: a node error, the host lives on.
    let e = render("crashy_error", 1).err().expect("an error");
    assert!(e.contains("crashy says no"), "{e}");

    // Crashes and a hang: node errors naming the plugin; the next render
    // works (in a new host).
    let name = crashy.file_name().unwrap().to_string_lossy().into_owned();
    for (op, says) in [("crashy_segv", "SIGSEGV"), ("crashy_abort", "SIGABRT"), ("crashy_hang", "timed out after 3 s")] {
        let t = Instant::now();
        let e = render(op, 1).err().expect("an error");
        assert!(e.contains(&format!("plugin {name}")) && e.contains(says), "{op}: {e}");
        assert!(t.elapsed() < Duration::from_secs(10), "{op} took {:?}", t.elapsed());
        if says.starts_with("SIG") {
            assert!(e.contains("(report: "), "the crash report is named: {e}");
        }
        assert_eq!(bits(&render("crashy_ok", 2).unwrap()), bits(&render("inproc_ok", 2).unwrap()), "after {op}");
    }

    // Several threads at once: hosts side by side, all right.
    let want = bits(&render("inproc_ok", 3).unwrap());
    let threads: Vec<_> = (0..6).map(|_| std::thread::spawn(|| (0..4).map(|_| bits(&render("crashy_ok", 3).unwrap())).collect::<Vec<_>>())).collect();
    for t in threads {
        assert!(t.join().unwrap().iter().all(|b| *b == want));
    }

    // Its ops are remembered: unchanged, a host that can't start still has
    // them (renders then say why); changed, it's described again.
    let library: &'static Path = Box::leak(crashy.clone().into_boxed_path());
    let pool = arclight::plugin_host::Pool::new(library, vec!["/nonexistent/arclight".into()]);
    let (ops_json, failed) = pool.describe().unwrap();
    assert!(ops_json.contains("crashy_hang") && failed.is_none(), "from the last time, no host needed");
    let e = pool.render("crashy_ok", "{}", &[None], 1).err().expect("an error").to_string();
    assert!(e.contains("can't start"), "{e}");
    std::fs::write(&crashy, std::fs::read(&crashy).unwrap()).unwrap();
    let file = std::fs::File::options().write(true).open(&crashy).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
    let (ops_json, failed) = pool.describe().unwrap();
    assert!(ops_json.contains("crashy_hang") && failed.is_some_and(|f| f.contains("can't start")), "changed: kept, saying why");
    std::fs::remove_dir_all(&dir).ok();
}

/// The example plugin through the CLI, hosted and in-process: the same
/// pixels. Needs it built (cargo build --release -p
/// arclight-plugin-example); skipped otherwise.
#[test]
fn the_example_plugin_renders_the_same_hosted() {
    let lib_dir = Path::new(HOST).parent().unwrap();
    let lib = lib_dir.join(format!("libarclight_plugin_example{}", std::env::consts::DLL_SUFFIX));
    if !lib.exists() {
        eprintln!("skipping: {} isn't built", lib.display());
        return;
    }
    let dir = std::env::temp_dir().join(format!("arclight_isolation_example_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let only = dir.join("plugins");
    std::fs::create_dir_all(&only).unwrap();
    std::fs::copy(&lib, only.join(lib.file_name().unwrap())).unwrap();
    let mut out = Vec::new();
    for in_process in ["1", "0"] {
        let file = dir.join(format!("grain{in_process}.exr"));
        let graph = dir.join("g.json");
        std::fs::write(&graph, serde_json::json!({"nodes": [
            {"id": "cb", "op": "checkerboard", "params": {"width": 320, "height": 200}},
            {"id": "g", "op": "film_grain", "inputs": ["cb"], "params": {"amount": 0.3}},
            {"id": "v", "op": "vignette", "inputs": ["g"]},
            {"id": "w", "op": "write", "inputs": ["v"], "params": {"path": file.to_string_lossy()}},
        ]}).to_string()).unwrap();
        let ok = Command::new(HOST)
            .args(["render"])
            .arg(&graph)
            .args(["--frame", "5"])
            .env("ARCLIGHT_PLUGIN_PATH", &only)
            .env("ARCLIGHT_PLUGINS_IN_PROCESS", in_process)
            .env("XDG_STATE_HOME", dir.join("state"))
            .status()
            .unwrap()
            .success();
        assert!(ok, "render with ARCLIGHT_PLUGINS_IN_PROCESS={in_process}");
        out.push(bits(&Image::read(&file).unwrap()));
    }
    assert!(out[0] == out[1], "hosted and in-process differ");
    std::fs::remove_dir_all(&dir).ok();
}

/// How long the example plugin's film_grain takes on a 4K frame, hosted or
/// (ARCLIGHT_PLUGINS_IN_PROCESS=1) in-process. By hand:
///   cargo build --release -p arclight-plugin-example
///   cargo test --release --test plugin_isolation -- --ignored --nocapture film_grain_4k
#[test]
#[ignore]
fn film_grain_4k() {
    let lib = Path::new(HOST).parent().unwrap().join(format!("libarclight_plugin_example{}", std::env::consts::DLL_SUFFIX));
    for mode in ["1", "0"] {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "film_grain_4k_one", "--ignored", "--nocapture"])
            .env("ARCLIGHT_PLUGINS_IN_PROCESS", mode)
            .env("GRAIN_LIB", &lib)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.starts_with("film_grain")).unwrap_or("(no timing)");
        println!("{} {line}", if mode == "1" { "in-process:" } else { "hosted:    " });
    }
}

#[test]
#[ignore]
fn film_grain_4k_one() {
    let Some(lib) = std::env::var_os("GRAIN_LIB") else { return };
    let dir = std::env::temp_dir().join(format!("arclight_grain_{}", std::process::id()));
    std::env::set_var("XDG_STATE_HOME", dir.join("state"));
    plugin::set_host_command(vec![HOST.into(), "plugin-host".into()]);
    plugin::load(Path::new(&lib)).unwrap();
    let g: Graph = serde_json::from_value(serde_json::json!({"nodes": [
        {"id": "cb", "op": "checkerboard", "params": {"width": 3840, "height": 2160}},
        {"id": "p", "op": "film_grain", "inputs": ["cb"], "params": {"amount": 0.2}},
    ]}))
    .unwrap();
    let mut eval = Evaluator::new();
    let mut times = Vec::new();
    for frame in 1..=6 {
        let t = Instant::now();
        eval.evaluate(&g, "p", &EvalContext { frame, write_outputs: false }).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // The first starts the host and draws the checkerboard: left out.
    let rest = &times[1..];
    println!("film_grain 4K: {:.1} ms a frame (first {:.1} ms)", rest.iter().sum::<f64>() / rest.len() as f64, times[0]);
    std::fs::remove_dir_all(&dir).ok();
}
