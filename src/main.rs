//! ArclightVFX Compositor — an original node-graph image compositor.
//!
//! A float-linear, premultiplied-alpha compositing engine driven by a JSON node
//! graph. This is original work, not derived from any existing product.

use anyhow::{bail, Context, Result};
use arclight::graph::Graph;
use arclight::ops::EvalContext;
use arclight::render::{self, Progress};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser)]
#[command(
    name = "arclight",
    version,
    about = "ArclightVFX Compositor (beta): a node-graph image compositor (linear float, premultiplied alpha)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a JSON node graph. `write` nodes write the chosen frame;
    /// `media_out` nodes write their whole frame range.
    Render {
        /// Path to the graph JSON file.
        graph: PathBuf,
        /// Frame at which `write` nodes are evaluated.
        #[arg(short, long, default_value_t = 1)]
        frame: i64,
        /// Print each node as it is evaluated.
        #[arg(short, long)]
        verbose: bool,
        /// Render on the CPU only (exact, reproducible results), as with
        /// ARCLIGHT_GPU=0.
        #[arg(long)]
        cpu: bool,
    },
    /// Write a built-in demo graph and render it, proving the pipeline end to
    /// end. Produces `<dir>/arclight_demo.png`.
    Demo {
        /// Output directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Render on the CPU only.
        #[arg(long)]
        cpu: bool,
    },
    /// Run a Model Context Protocol server for AI agents: JSON-RPC 2.0 over
    /// stdio, or over HTTP with --http.
    Mcp {
        /// Graph to start from (otherwise an empty one).
        graph: Option<PathBuf>,
        /// Listen for HTTP on this address (e.g. 127.0.0.1:7878) instead of stdio.
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "")]
        http: Option<String>,
        /// Send tools to a running GUI (the newest, or the one with this pid).
        #[arg(long, value_name = "PID", num_args = 0..=1, default_missing_value = "0")]
        attach: Option<u32>,
    },
    /// Show whether GPU acceleration is in use: the adapter, its backend,
    /// limits and transfer path, or why it renders on the CPU.
    Gpu {
        /// Run the GPU self-test: every GPU path against the CPU, timed.
        /// Exits with status 1 if a case fails or the GPU isn't in use.
        #[arg(long)]
        check: bool,
        /// Print JSON (for pasting into an issue).
        #[arg(long)]
        json: bool,
        /// With --check: the 2D cases at one small size only (seconds).
        #[arg(long, requires = "check")]
        quick: bool,
    },
    /// List the plugin search path and the plugins found on it.
    Plugins,
    /// List every op (node type), or describe one as JSON.
    Ops {
        /// Op to describe.
        op: Option<String>,
    },
}

fn main() -> Result<()> {
    // A plugin host (see arclight::plugin_host), started by Arclight itself:
    // before anything else.
    #[cfg(unix)]
    if std::env::args_os().nth(1).is_some_and(|a| a == "plugin-host") {
        arclight::crash::install_host("plugin-host");
        let library = std::env::args_os().nth(2).map(PathBuf::from).unwrap_or_default();
        // (Out without exit-time teardown: see `gpu::exit_process`.)
        arclight::gpu::exit_process(arclight::plugin::run_host(&library));
    }
    // Crash reports (kept on this machine, see arclight::crash).
    arclight::crash::install("arclight");
    arclight::crash::crash_test_now();
    let cli = Cli::parse();
    if let Ok(exe) = std::env::current_exe() {
        arclight::plugin::set_host_command(vec![exe.into(), "plugin-host".into()]);
    }
    arclight::plugin::init();
    arclight::crash::note(format!("command: {}", std::env::args().nth(1).unwrap_or_default()));
    let result = match cli.command {
        Command::Render { graph, frame, verbose, cpu } => on_cpu_if(cpu, || render(graph, frame, verbose)),
        Command::Demo { dir, cpu } => on_cpu_if(cpu, || demo(dir)),
        Command::Gpu { check, json, quick } => gpu(check, json, quick),
        Command::Mcp { graph, http, attach } => mcp(graph, http, attach),
        Command::Plugins => plugins(),
        Command::Ops { op } => list_ops(op),
    };
    // Out without the C libraries' exit-time teardown (see
    // `gpu::exit_process`), as `main` returning would end it.
    if let Err(e) = &result {
        eprintln!("Error: {e:?}");
    }
    arclight::gpu::exit_process(i32::from(result.is_err()))
}

/// Run `f`, on the CPU only when `cpu` (`--cpu`).
fn on_cpu_if<R>(cpu: bool, f: impl FnOnce() -> R) -> R {
    if cpu { arclight::gpu::cpu_only(f) } else { f() }
}

fn gpu(check: bool, json: bool, quick: bool) -> Result<()> {
    use arclight::gpu;
    if check {
        let report = if quick { gpu::check::run_quick() } else { gpu::check::run() };
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", report.table());
        }
        if !report.passed() {
            gpu::exit_process(1);
        }
        return Ok(());
    }
    let info = gpu::info();
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    println!("{}", info["line"].as_str().unwrap_or_default());
    if let Some(a) = info.get("adapter") {
        let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
        println!("Adapter:   {} ({}, {})", s("name"), s("backend"), s("device_type"));
        println!("Driver:    {} {}", s("driver"), s("driver_info"));
        println!("Transfers: {}", info["transfer"].as_str().unwrap_or_default());
        let l = &a["limits"];
        let mib = |k: &str| l[k].as_u64().unwrap_or(0) >> 20;
        println!("Limits:    buffer {} MiB, storage binding {} MiB, 2D texture {}, workgroup {} invocations / {} KiB",
            mib("max_buffer_size"), mib("max_storage_buffer_binding_size"), l["max_texture_dimension_2d"],
            l["max_compute_invocations_per_workgroup"], l["max_compute_workgroup_storage_size"].as_u64().unwrap_or(0) >> 10);
        let features: Vec<&str> = a["features"].as_array().map(|f| f.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        if !features.is_empty() {
            println!("Features:  {}", features.join(", "));
        }
    }
    if let Some(e) = info["last_error"].as_str() {
        println!("Last GPU error: {e}");
    }
    Ok(())
}

fn mcp(graph: Option<PathBuf>, http: Option<String>, attach: Option<u32>) -> Result<()> {
    use arclight::mcp::{self, Server};
    use arclight::session::Session;
    let session = match &graph {
        Some(path) => Session::open(path)?,
        None => Session::default(),
    };
    let mut server = Server::new(session);
    if let Some(pid) = attach {
        server.attached = Some(arclight::bridge::find_instance((pid != 0).then_some(pid))?);
    }
    match http {
        Some(addr) => {
            let addr = if addr.is_empty() { arclight::config::Settings::load().mcp_http_addr } else { addr };
            mcp::serve_http(server, &addr)
        }
        None => mcp::serve_stdio(&mut server),
    }
}

fn plugins() -> Result<()> {
    println!("Plugin search path:");
    for dir in arclight::config::plugin_dirs() {
        println!("  {}{}", dir.display(), if dir.is_dir() { "" } else { "  (missing)" });
    }
    let reports = arclight::plugin::reports();
    if reports.is_empty() {
        println!("No plugins found.");
    }
    for r in reports {
        match r.result {
            Ok(ops) => println!("{}: {}", r.path.display(), ops.join(", ")),
            Err(e) => println!("{}: FAILED: {e}", r.path.display()),
        }
    }
    Ok(())
}

fn list_ops(op: Option<String>) -> Result<()> {
    use arclight::session;
    match op {
        Some(op) => println!("{}", serde_json::to_string_pretty(&session::op_schema(session::op_info(&op)?))?),
        None => {
            for info in arclight::ops::all() {
                let cat = format!("{:?}", info.category).to_lowercase();
                println!("{:<16} {:<10} {}", info.name, cat, info.label);
            }
        }
    }
    Ok(())
}

fn render(path: PathBuf, frame: i64, verbose: bool) -> Result<()> {
    let g = Graph::from_file(&path)?;
    if g.nodes.is_empty() {
        bail!("graph has no nodes");
    }
    if verbose {
        eprintln!("arclight: {} node(s)", g.nodes.len());
    }

    // Still writes (and a full evaluation, so errors anywhere surface).
    let has_media_out = g.nodes.iter().any(|n| n.op == "media_out");
    if g.nodes.iter().any(|n| matches!(n.op.as_str(), "write" | "write_geo")) || !has_media_out {
        let ctx = EvalContext { frame, write_outputs: true };
        // (Results the writes don't need may stay on the GPU.)
        let results = arclight::graph::Evaluator::new().run_all(&g, &ctx)?;
        if verbose {
            for n in &g.nodes {
                if let Some((_, _, window)) = results.iter().find(|(id, ..)| *id == n.id) {
                    eprintln!("  {} ({}) -> {}x{}", n.id, n.op, window.width(), window.height());
                }
            }
        }
        println!("arclight: rendered {} node(s) at frame {frame}", results.len());
    }

    for n in g.nodes.iter().filter(|n| n.op == "media_out") {
        if n.disabled {
            println!("arclight: {} is turned off, skipped", n.id);
            continue;
        }
        let progress = Progress::default();
        let frames = render::render_media_out(&g, &n.id, &progress)?;
        println!("arclight: {} wrote {frames} frame(s)", n.id);
    }
    Ok(())
}

fn demo(dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir).ok();
    let out = dir.join("arclight_demo.png");
    let graph_path = dir.join("arclight_demo.json");
    let graph_json = demo_graph(&out);
    std::fs::write(&graph_path, &graph_json)
        .with_context(|| format!("writing {}", graph_path.display()))?;

    let g = Graph::from_str(&graph_json)?;
    g.evaluate()?;
    println!(
        "arclight: demo rendered -> {}\n            graph written -> {}",
        out.display(),
        graph_path.display()
    );
    Ok(())
}

/// A self-contained demo: a checkerboard background, a soft-edged colored disc
/// built by blurring a constant through a transform, graded and merged over.
fn demo_graph(out: &std::path::Path) -> String {
    serde_json::json!({
        "nodes": [
            { "id": "bg", "op": "checkerboard",
              "params": { "width": 640, "height": 480, "size": 40 } },
            { "id": "fill", "op": "constant",
              "params": { "width": 640, "height": 480, "color": [0.9, 0.25, 0.1, 1.0] } },
            // Shrink the opaque fill so it sits on a transparent border, giving
            // the blur something to soften into, and rotate it a little.
            { "id": "card", "op": "transform", "inputs": ["fill"],
              "params": { "scale": [0.4, 0.5], "rotate": 12.0 } },
            { "id": "soft", "op": "blur", "inputs": ["card"],
              "params": { "radius": 16, "passes": 3 } },
            { "id": "warm", "op": "grade", "inputs": ["soft"],
              "params": { "multiply": [1.1, 1.0, 0.9], "gamma": [1.0, 1.0, 1.1] } },
            { "id": "comp", "op": "merge", "inputs": ["warm", "bg"],
              "params": { "operation": "over" } },
            { "id": "out", "op": "write", "inputs": ["comp"],
              "params": { "path": out.to_string_lossy() } }
        ]
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use arclight::color;
    use arclight::graph::Graph;
    use std::str::FromStr;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn srgb_round_trips() {
        for &v in &[0.0f32, 0.01, 0.2, 0.5, 0.75, 1.0] {
            let back = color::linear_to_srgb(color::srgb_to_linear(v));
            assert!(approx(v, back), "{v} -> {back}");
        }
    }

    #[test]
    fn premult_round_trips() {
        let straight = [0.4, 0.6, 0.8, 0.5];
        let back = color::unpremultiply(color::premultiply(straight));
        for c in 0..4 {
            assert!(approx(straight[c], back[c]));
        }
        // Zero alpha collapses to transparent black.
        assert_eq!(color::unpremultiply([0.0, 0.0, 0.0, 0.0]), [0.0; 4]);
    }

    #[test]
    fn opaque_over_replaces_background() {
        // Opaque red fg over green bg must equal fg (premultiplied "over").
        let g = r#"{
            "nodes": [
                { "id": "bg", "op": "constant",
                  "params": { "width": 4, "height": 4, "color": [0,1,0,1] } },
                { "id": "fg", "op": "constant",
                  "params": { "width": 4, "height": 4, "color": [1,0,0,1] } },
                { "id": "c", "op": "merge", "inputs": ["fg","bg"],
                  "params": { "operation": "over" } }
            ]
        }"#;
        let res = Graph::from_str(g).unwrap().evaluate().unwrap();
        let px = res["c"].get(0, 0);
        assert!(approx(px[0], 1.0) && approx(px[1], 0.0) && approx(px[2], 0.0) && approx(px[3], 1.0));
    }

    #[test]
    fn half_over_blends_premultiplied() {
        // 50% white over opaque black: rgb = 0.5 + 0*0.5 = 0.5, a = 1.
        let g = r#"{
            "nodes": [
                { "id": "bg", "op": "constant",
                  "params": { "width": 2, "height": 2, "color": [0,0,0,1] } },
                { "id": "fg", "op": "constant",
                  "params": { "width": 2, "height": 2, "color": [1,1,1,0.5] } },
                { "id": "c", "op": "merge", "inputs": ["fg","bg"],
                  "params": { "operation": "over" } }
            ]
        }"#;
        let res = Graph::from_str(g).unwrap().evaluate().unwrap();
        let px = res["c"].get(1, 1);
        assert!(approx(px[0], 0.5), "rgb was {}", px[0]);
        assert!(approx(px[3], 1.0));
    }

    #[test]
    fn cycle_is_rejected() {
        let g = r#"{
            "nodes": [
                { "id": "a", "op": "merge", "inputs": ["b","b"] },
                { "id": "b", "op": "merge", "inputs": ["a","a"] }
            ]
        }"#;
        let err = Graph::from_str(g).unwrap().evaluate().err().unwrap();
        assert!(err.to_string().contains("cycle"), "got: {err}");
    }

    #[test]
    fn blur_conserves_flat_region() {
        // Blurring a flat constant must leave interior pixels unchanged.
        let g = r#"{
            "nodes": [
                { "id": "f", "op": "constant",
                  "params": { "width": 64, "height": 64, "color": [0.3,0.4,0.5,1] } },
                { "id": "b", "op": "blur", "inputs": ["f"],
                  "params": { "radius": 5, "passes": 2 } }
            ]
        }"#;
        let res = Graph::from_str(g).unwrap().evaluate().unwrap();
        let px = res["b"].get(32, 32);
        assert!(approx(px[0], 0.3) && approx(px[1], 0.4) && approx(px[2], 0.5));
    }

    #[test]
    fn evaluator_reuses_unchanged_upstream() {
        use arclight::graph::Evaluator;
        use arclight::ops::EvalContext;
        use std::sync::Arc;
        let mut g = Graph::from_str(r#"{
            "nodes": [
                { "id": "src", "op": "checkerboard", "params": { "width": 32, "height": 32 } },
                { "id": "b", "op": "blur", "inputs": ["src"], "params": { "radius": 2 } },
                { "id": "c", "op": "grade", "inputs": ["b"], "params": { "multiply": [2,2,2] } }
            ]
        }"#).unwrap();
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let mut ev = Evaluator::new();
        let first = ev.evaluate_all(&g, &ctx).unwrap();
        // Editing the last node must not re-render the nodes above it.
        g.nodes[2].params["multiply"] = serde_json::json!([3, 3, 3]);
        let second = ev.evaluate_all(&g, &ctx).unwrap();
        assert!(Arc::ptr_eq(&first["b"], &second["b"]));
        assert!(!Arc::ptr_eq(&first["c"], &second["c"]));
        // A static graph is frame-independent, so a new frame is a cache hit.
        let k1 = Evaluator::key(&g, "c", &ctx).unwrap();
        let k2 = Evaluator::key(&g, "c", &EvalContext { frame: 50, ..ctx }).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn unconnected_inputs_are_empty_except_into_outputs() {
        use arclight::graph::Evaluator;
        use arclight::ops::EvalContext;
        // A Merge with A unplugged (a Keyer just added, say) passes B
        // through, so the viewer downstream still shows the comp…
        let g = Graph::from_str(r#"{
            "nodes": [
                { "id": "bg", "op": "constant", "params": { "width": 2, "height": 2, "color": [0.2, 0.4, 0.6, 1] } },
                { "id": "k", "op": "keyer" },
                { "id": "m", "op": "merge", "inputs": ["k", "bg"] },
                { "id": "v", "op": "viewer", "inputs": ["m"] },
                { "id": "lost", "op": "viewer" }
            ]
        }"#).unwrap();
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let mut ev = Evaluator::new();
        let img = ev.evaluate(&g, "v", &ctx).unwrap();
        assert_eq!(img.at(1, 1), [0.2, 0.4, 0.6, 1.0]);
        // …while an output with nothing to show says which port is empty.
        let err = ev.evaluate(&g, "lost", &ctx).err().unwrap();
        assert_eq!(err.node.as_deref(), Some("lost"));
        assert!(err.message.contains("'input'"), "{}", err.message);
    }

    #[test]
    fn keyframed_params_animate_and_hold() {
        use arclight::graph::Evaluator;
        use arclight::ops::EvalContext;
        let g = Graph::from_str(r#"{
            "nodes": [
                { "id": "c", "op": "constant", "params": {
                    "width": { "keys": [[1, 4], [11, 9]], "interp": "linear" },
                    "height": 2,
                    "color": { "keys": [[1, [0,0,0,1]], [11, [1,1,1,1]]], "interp": "linear" } } }
            ]
        }"#).unwrap();
        let at = |f| EvalContext { frame: f, write_outputs: false };
        let mut ev = Evaluator::new();
        let mid = ev.evaluate(&g, "c", &at(6)).ok().unwrap();
        assert!(approx(mid.get(0, 0)[0], 0.5));
        // Int params are rounded: width 4 -> 9 over 10 frames is 6.5 -> 7 at frame 6.
        assert_eq!(mid.width, 7);
        // After the last key the curve holds, so later frames share a cache key.
        assert_eq!(Evaluator::key(&g, "c", &at(11)).unwrap(), Evaluator::key(&g, "c", &at(40)).unwrap());
        assert_ne!(Evaluator::key(&g, "c", &at(5)).unwrap(), Evaluator::key(&g, "c", &at(6)).unwrap());
    }

    #[test]
    fn tracker_follows_an_animated_shape_through_the_graph() {
        use arclight::{render::Progress, track};
        // A rotated box gliding from (60, 60) to (100, 80) over frames 1–11.
        let g: Graph = serde_json::from_value(serde_json::json!({
            "nodes": [
                { "id": "bg", "op": "constant", "params": { "width": 160, "height": 120, "color": [0.1, 0.1, 0.1, 1] } },
                { "id": "box", "op": "rectangle", "inputs": ["bg"], "params": {
                    "center": { "keys": [[1, [60, 60]], [11, [100, 80]]], "interp": "linear" },
                    "size": [18, 12], "rotate": 20, "feather": 2 } },
                { "id": "trk", "op": "tracker", "inputs": ["box"], "params": { "track1": [60, 60], "pattern": 25, "search": 45 } }
            ]
        }))
        .unwrap();
        let frames = track::input_frames(&g, "trk").unwrap();
        let start = track::positions(&g.node("trk").unwrap().params, 1);
        let settings = track::Settings { pattern: 25, search: 45, sizes: vec![], offsets: vec![], min_score: 0.8, update: false, motion: track::Solve::Rotate };
        let mut found = Vec::new();
        let out = track::track(frames, &start, 1, 11, &settings, &Progress::default(), |f, s| found.push((f, s.pos[0]))).unwrap();
        assert_eq!((out.frames, out.lost.len()), (10, 0));
        for (f, p) in found {
            let t = (f - 1) as f32 / 10.0;
            let p = p.unwrap();
            assert!((p[0] - (60.0 + 40.0 * t)).abs() < 0.05 && (p[1] - (60.0 + 20.0 * t)).abs() < 0.05, "frame {f}: {p:?}");
        }
    }
}
