//! The command line ends with `_exit` (see `gpu::exit_process`), which
//! skips the C libraries' exit-time teardown: nothing may be cut short by
//! it. A render's files are complete, its output and an MCP session's
//! replies come through a pipe whole, Media Out's ffmpeg has finished, and
//! the exit status says how it went.
#![cfg(unix)]

use arclight::graph::{Evaluator, Graph};
use arclight::image::Image;
use arclight::ops::EvalContext;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_arclight");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("arclight_cli_exit_{}_{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A blurred, graded checkerboard (on the GPU when there is one) written
/// as EXR and PNG, and as a 3-frame video when ffmpeg is there.
#[test]
fn a_render_s_files_and_output_are_complete() {
    let dir = scratch("render");
    let video = Command::new(arclight::media::tool("ffmpeg")).arg("-version").output().is_ok();
    let mut nodes = vec![
        serde_json::json!({"id": "cb", "op": "checkerboard", "params": {"width": 301, "height": 203, "size": 9}}),
        serde_json::json!({"id": "soft", "op": "blur", "inputs": ["cb"], "params": {"radius": 6}}),
        serde_json::json!({"id": "look", "op": "grade", "inputs": ["soft"], "params": {"gamma": [1.4, 1.0, 0.8]}}),
        serde_json::json!({"id": "exr", "op": "write", "inputs": ["look"], "params": {"path": dir.join("out.exr")}}),
        serde_json::json!({"id": "png", "op": "write", "inputs": ["look"], "params": {"path": dir.join("out.png")}}),
    ];
    if video {
        nodes.push(serde_json::json!({"id": "mov", "op": "media_out", "inputs": ["look"], "params": {"path": dir.join("out.mp4"), "first_frame": 1, "last_frame": 3}}));
    }
    let graph = serde_json::json!({ "nodes": nodes });
    let file = dir.join("g.json");
    std::fs::write(&file, graph.to_string()).unwrap();
    let out = Command::new(BIN).args(["render", "-v"]).arg(&file).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{:?}\n{stdout}\n{stderr}", out.status);
    // Everything printed came through the pipe.
    assert!(stdout.contains(&format!("rendered {} node(s) at frame 1", if video { 6 } else { 5 })), "{stdout}");
    assert_eq!(stderr.lines().filter(|l| l.starts_with("  ") && l.contains(") -> ")).count(), if video { 6 } else { 5 }, "a line a node: {stderr}");
    // The files are whole: they read back as the render, as on the CPU.
    let g: Graph = serde_json::from_value(graph).unwrap();
    let want = arclight::gpu::cpu_only(|| Evaluator::new().evaluate(&g, "look", &EvalContext { frame: 1, write_outputs: false }).unwrap());
    let exr = Image::read(&dir.join("out.exr")).unwrap();
    assert_eq!((exr.width, exr.height), (301, 203));
    let err = exr.pixels.iter().zip(&want.pixels).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs())).fold(0.0f32, f32::max);
    assert!(err < 1e-3, "the EXR differs from the render by {err}");
    let png = Image::read(&dir.join("out.png")).unwrap();
    assert_eq!((png.width, png.height), (301, 203));
    if video {
        assert!(stdout.contains("mov wrote 3 frame(s)"), "{stdout}");
        let info = arclight::media::probe_video(&dir.join("out.mp4")).unwrap();
        // (Odd sizes padded to even for the codec.)
        assert_eq!((info.width, info.height, info.frames), (302, 204, 3), "ffmpeg finished the file");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// An MCP session over stdio: every reply, the last render's included,
/// comes through the pipe before the process ends.
#[test]
fn every_mcp_reply_comes_through_a_pipe() {
    let mut child = Command::new(BIN).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let call = |id: usize, name: &str, args: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string() + "\n";
    let mut input = call(1, "create_node", serde_json::json!({"op": "checkerboard", "name": "cb", "params": {"width": 640, "height": 360}}));
    input += &call(2, "create_node", serde_json::json!({"op": "blur", "name": "soft", "inputs": ["cb"]}));
    for id in 3..300 {
        input += &call(id, "list_ops", serde_json::json!({}));
    }
    input += &call(300, "render_frame", serde_json::json!({"node": "soft"}));
    stdin.write_all(input.as_bytes()).unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 300);
    let last = &replies[299];
    assert_eq!((last["id"].as_u64(), last["result"]["isError"].as_bool()), (Some(300), Some(false)), "{last}");
    assert_eq!(last["result"]["structuredContent"]["width"], 640, "{last}");
}

/// A render that fails exits 1 with its error, as `main` returning would.
#[test]
fn a_failed_render_exits_1_with_its_error() {
    let dir = scratch("fail");
    let file = dir.join("g.json");
    std::fs::write(&file, serde_json::json!({"nodes": [{"id": "plate", "op": "read", "params": {"path": dir.join("missing.exr")}}]}).to_string()).unwrap();
    let out = Command::new(BIN).arg("render").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("Error: "), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).ok();
}

/// `arclight mcp --http` stops on SIGTERM (or SIGINT) after the request in
/// progress, and ends cleanly (exit 0, through `gpu::exit_process`).
#[test]
fn the_http_server_stops_on_a_signal() {
    use std::io::{BufRead, BufReader, Read};
    let mut child = Command::new(BIN).args(["mcp", "--http", "127.0.0.1:0"]).stderr(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    let addr = first.trim().strip_prefix("arclight: MCP server on http://").and_then(|a| a.strip_suffix("/mcp")).unwrap_or_else(|| panic!("{first}")).to_string();
    // One request first: a render (on the GPU when there is one).
    let post = |body: serde_json::Value| {
        let body = body.to_string();
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        reply
    };
    let call = |name: &str, args: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}});
    post(call("create_node", serde_json::json!({"op": "checkerboard", "name": "cb", "params": {"width": 320, "height": 200}})));
    post(call("create_node", serde_json::json!({"op": "blur", "name": "soft", "inputs": ["cb"]})));
    let reply = post(call("render_frame", serde_json::json!({"node": "soft"})));
    assert!(reply.starts_with("HTTP/1.1 200") && reply.contains("\"width\":320"), "{reply}");
    // SAFETY: a signal to our own child.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(t0.elapsed().as_secs() < 10, "the server didn't stop");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    assert!(status.success(), "{status:?}\n{rest}");
    assert!(rest.contains("stopping after the request in progress") && rest.contains("MCP server stopped"), "{rest}");
}
