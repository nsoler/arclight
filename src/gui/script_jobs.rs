//! Renders requested over the scripting bridge (MCP / Python), run on
//! worker threads so the window stays responsive. Each job renders a
//! snapshot of the graph taken when the request arrived, and answers the
//! script when it finishes.

use arclight::graph::Graph;
use arclight::mcp;
use arclight::render::Progress;
use arclight::session::Session;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub type Reply = Box<dyn FnOnce(Result<Value, String>) + Send>;

pub struct ScriptJob {
    /// e.g. "render_output media_out1".
    pub label: String,
    pub progress: Arc<Progress>,
    finished: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ScriptJobs {
    jobs: Vec<ScriptJob>,
}

impl ScriptJobs {
    /// Run render tool `tool` on a worker. `reply` gets the tool's result;
    /// `wake` is called afterwards so the UI drops the finished job.
    pub fn spawn(
        &mut self,
        graph: Graph,
        path: Option<PathBuf>,
        tool: &str,
        args: Value,
        reply: Reply,
        wake: impl Fn() + Send + 'static,
    ) {
        let node = args.get("node").and_then(Value::as_str).unwrap_or("");
        let label = format!("{tool} {node}").trim_end().to_string();
        let progress = Arc::new(Progress::default());
        let finished = Arc::new(AtomicBool::new(false));
        let (pg, done, tool) = (progress.clone(), finished.clone(), tool.to_string());
        let spawned = std::thread::Builder::new().name("arclight-script-render".into()).spawn(move || {
            let mut session = Session::new(graph);
            session.path = path;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                mcp::call_tool_with_progress(&mut session, &tool, &args, &pg).map_err(|e| format!("{e:#}"))
            }))
            .unwrap_or_else(|_| Err("render thread panicked".into()));
            reply(result);
            done.store(true, Ordering::Release);
            wake();
        });
        if let Err(e) = spawned {
            // The closure (and with it `reply`) is gone; the client sees the
            // connection's reply channel close and reports an error.
            eprintln!("arclight-gui: could not start render thread: {e}");
            return;
        }
        self.jobs.push(ScriptJob { label, progress, finished });
    }

    /// Jobs still running (finished ones are dropped).
    pub fn running(&mut self) -> &[ScriptJob] {
        self.jobs.retain(|j| !j.finished.load(Ordering::Acquire));
        &self.jobs
    }
}

impl ScriptJob {
    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    fn graph(out: &std::path::Path, last_frame: i64, size: i64) -> Graph {
        serde_json::from_value(json!({"nodes": [
            {"id": "c", "op": "checkerboard", "params": {"width": size, "height": size}},
            {"id": "out", "op": "media_out", "inputs": ["c"], "params": {
                "path": out.join("f.####.png").to_string_lossy(),
                "first_frame": 1, "last_frame": last_frame, "fps": 24.0}}
        ]}))
        .unwrap()
    }

    fn spawn(jobs: &mut ScriptJobs, g: Graph, tool: &str, args: Value) -> mpsc::Receiver<Result<Value, String>> {
        let (tx, rx) = mpsc::channel();
        jobs.spawn(g, None, tool, args, Box::new(move |r| tx.send(r).unwrap()), || {});
        rx
    }

    #[test]
    fn renders_in_the_background_and_replies() {
        let dir = std::env::temp_dir().join(format!("arclight-script-jobs-{}", std::process::id()));
        let mut jobs = ScriptJobs::default();
        let rx = spawn(&mut jobs, graph(&dir, 3, 32), "render_output", json!({"node": "out"}));
        assert_eq!(jobs.jobs[0].label, "render_output out");
        let reply = rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(reply["frames"], 3);
        assert!(dir.join("f.0003.png").exists());
        while !jobs.running().is_empty() {
            std::thread::yield_now();
        }

        let rx = spawn(&mut jobs, graph(&dir, 1, 32), "render_frame", json!({"node": "nope"}));
        assert!(rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap_err().contains("nope"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_stops_a_range_render() {
        let dir = std::env::temp_dir().join(format!("arclight-script-cancel-{}", std::process::id()));
        let mut jobs = ScriptJobs::default();
        let rx = spawn(&mut jobs, graph(&dir, 10_000, 256), "render_output", json!({"node": "out"}));
        jobs.running()[0].cancel();
        let err = rx.recv_timeout(Duration::from_secs(60)).unwrap().unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
