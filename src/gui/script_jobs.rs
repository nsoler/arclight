//! Renders requested over the scripting bridge (MCP / Python), run on
//! worker threads so the window stays responsive. Each job renders a
//! snapshot of the graph taken when the request arrived, and answers the
//! script when it finishes. Its particles go on from the viewer's
//! simulations (where they stand for the snapshot), never changing them.

use arclight::graph::{Evaluator, Graph};
use arclight::mcp;
use arclight::render::Progress;
use arclight::session::Session;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    /// The viewer's evaluator, whose particle simulations jobs go on from.
    viewer: Arc<Mutex<Evaluator>>,
}

impl ScriptJobs {
    /// Jobs going on from `viewer`'s particle simulations.
    pub fn sharing(viewer: Arc<Mutex<Evaluator>>) -> ScriptJobs {
        ScriptJobs { jobs: Vec::new(), viewer }
    }

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
        let (pg, done, tool, viewer) = (progress.clone(), finished.clone(), tool.to_string(), self.viewer.clone());
        let spawned = std::thread::Builder::new().name("arclight-script-render".into()).spawn(move || {
            // Its own evaluator: a long render doesn't hold the viewer's.
            let eval = viewer.lock().unwrap_or_else(|e| e.into_inner()).borrowing_particles();
            let mut session = Session::sharing(graph, Arc::new(Mutex::new(eval)));
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

    /// A render job goes on from the viewer's particle simulation, and
    /// one of a graph differing from the viewer's renders that graph and
    /// leaves the viewer's simulation as it was.
    #[test]
    fn a_job_borrows_the_viewer_s_particles_without_changing_them() {
        use arclight::graph::Evaluator;
        use arclight::ops::EvalContext;
        let graph = |fall: f64| -> Graph {
            serde_json::from_value(json!({ "nodes": [
                { "id": "emit", "op": "particle_emitter", "params": { "rate": 50, "speed": 2, "life": 1000, "size": 3 } },
                { "id": "fall", "op": "particle_gravity", "inputs": ["emit"], "params": { "acceleration": [0, -fall, 0] } },
                { "id": "dots", "op": "particles_2d", "inputs": ["fall"], "params": {} },
            ], "project": { "first_frame": 1, "last_frame": 100, "fps": 24.0, "format": { "width": 64, "height": 64 } } }))
            .unwrap()
        };
        let viewer = Arc::new(Mutex::new(Evaluator::new()));
        let fall = |g: &Graph| viewer.lock().unwrap().evaluate(g, "fall", &EvalContext { frame: 40, write_outputs: false }).unwrap().scene.as_ref().unwrap().particles[0].clone();
        let shown = fall(&graph(1.0));
        let (particles, steps) = (format!("{:?}", shown.particles()), shown.steps());
        let mut jobs = ScriptJobs::sharing(viewer.clone());
        let render = |jobs: &mut ScriptJobs, g: Graph| spawn(jobs, g, "render_frame", json!({"node": "dots", "frame": 40})).recv_timeout(Duration::from_secs(60)).unwrap().unwrap();
        let fresh = |g: Graph| mcp::call_tool(&mut Session::new(g), "render_frame", &json!({"node": "dots", "frame": 40})).unwrap();
        for g in [graph(1.0), graph(9.0)] {
            assert_eq!(render(&mut jobs, g.clone()), fresh(g));
            let now = fall(&graph(1.0));
            assert_eq!((format!("{:?}", now.particles()), now.steps()), (particles.clone(), steps), "the viewer's simulation as it was");
        }
        assert_ne!(fresh(graph(1.0)), fresh(graph(9.0)), "the graphs render differently");
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
