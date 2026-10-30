//! Graph tool calls over the app bridge (MCP / Python), run on one
//! long-lived thread so the window never waits on them. Each runs on a
//! session over a snapshot of the document that evaluates with the
//! viewer's own evaluator, so a call reuses what the viewer has already
//! rendered and simulated (and the viewer what the call has), rather than
//! starting over on a fresh one.

use arclight::graph::{Evaluator, Graph};
use arclight::mcp;
use arclight::session::Session;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// A tool to run on a snapshot of the document.
pub struct Call {
    pub tool: String,
    pub args: Value,
    pub graph: Graph,
    pub path: Option<PathBuf>,
    pub dirty: bool,
}

/// A finished call: its result, and the session's graph and path after it
/// (`changed`: the graph isn't the snapshot's any more).
pub struct Done {
    pub result: Result<Value, String>,
    pub graph: Graph,
    pub changed: bool,
    pub path: Option<PathBuf>,
}

pub struct BridgeCalls {
    tx: Sender<Call>,
    rx: Receiver<Done>,
}

impl BridgeCalls {
    /// The thread, evaluating with `eval`; `wake` is called after each call.
    pub fn spawn(eval: Arc<Mutex<Evaluator>>, wake: impl Fn() + Send + 'static) -> BridgeCalls {
        let (tx, calls) = channel::<Call>();
        let (done, rx) = channel::<Done>();
        std::thread::Builder::new()
            .name("arclight-bridge-calls".into())
            .spawn(move || {
                for call in calls {
                    if done.send(run(&eval, call)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .expect("spawn bridge call thread");
        BridgeCalls { tx, rx }
    }

    pub fn send(&self, call: Call) {
        let _ = self.tx.send(call);
    }

    /// The call that finished, if one did since the last poll.
    pub fn poll(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
}

/// Run `call` on a session sharing `eval` (which it holds one evaluation
/// at a time, never for the whole call).
fn run(eval: &Arc<Mutex<Evaluator>>, call: Call) -> Done {
    let mut session = Session::sharing(call.graph, eval.clone());
    let before = session.graph.clone();
    session.path = call.path;
    session.dirty = call.dirty;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mcp::call_tool(&mut session, &call.tool, &call.args).map_err(|e| format!("{e:#}"))))
        .unwrap_or_else(|_| Err(format!("{} panicked", call.tool)));
    let changed = session.graph != before;
    Done { result, graph: session.graph, changed, path: session.path }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::ops::EvalContext;
    use serde_json::json;
    use std::time::Duration;

    fn graph() -> Graph {
        serde_json::from_value(json!({ "nodes": [
            { "id": "emit", "op": "particle_emitter", "params": { "rate": 20, "speed": 2, "life": 1000 } },
            { "id": "fall", "op": "particle_gravity", "inputs": ["emit"], "params": {} },
        ] }))
        .unwrap()
    }

    fn call(calls: &BridgeCalls, tool: &str, args: Value) -> Done {
        calls.send(Call { tool: tool.into(), args, graph: graph(), path: None, dirty: false });
        calls.rx.recv_timeout(Duration::from_secs(60)).expect("the call finishes")
    }

    /// Frames simulated so far by the stream `eval` holds for "fall".
    fn steps(eval: &Arc<Mutex<Evaluator>>) -> usize {
        let img = eval.lock().unwrap().evaluate(&graph(), "fall", &EvalContext { frame: 1, write_outputs: false }).unwrap();
        img.scene.as_ref().unwrap().particles[0].steps()
    }

    #[test]
    fn particles_step_on_from_the_viewer_s_simulation() {
        let eval = Arc::new(Mutex::new(Evaluator::new()));
        let calls = BridgeCalls::spawn(eval.clone(), || {});
        // The viewer has shown frame 30.
        let shown = eval.lock().unwrap().evaluate(&graph(), "fall", &EvalContext { frame: 30, write_outputs: false }).unwrap();
        shown.scene.as_ref().unwrap().particles[0].particles();
        let viewed = steps(&eval);
        assert!(viewed >= 29, "{viewed}");
        // A call there simulates nothing more; one at 40 the 10 frames on.
        let at30 = call(&calls, "particles", json!({ "node": "fall", "frame": 30 }));
        assert_eq!(steps(&eval), viewed, "frame 30 is the viewer's");
        let at40 = call(&calls, "particles", json!({ "node": "fall", "frame": 40 }));
        assert_eq!(steps(&eval), viewed + 10, "on from frame 30, not over from the start");
        assert!(!at40.changed && at30.result.unwrap()["total"].as_u64() < at40.result.as_ref().unwrap()["total"].as_u64());
        // As a session of its own sees it.
        let fresh = mcp::call_tool(&mut Session::new(graph()), "particles", &json!({ "node": "fall", "frame": 40 })).unwrap();
        assert_eq!(at40.result.unwrap(), fresh);
    }

    /// A Scanline Render of particles at a late frame simulates them a
    /// run at a time, not inside its evaluation: the viewer gets the
    /// evaluator between runs and sees the simulation going on.
    #[test]
    fn a_late_render_of_particles_lets_the_viewer_in() {
        let g: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "emit", "op": "particle_emitter", "params": { "rate": 200, "speed": 2, "life": 1000 } },
            { "id": "fall", "op": "particle_gravity", "inputs": ["emit"], "params": {} },
            { "id": "scene", "op": "scene", "inputs": ["fall"], "params": {} },
            { "id": "cam", "op": "camera", "params": {} },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam"], "params": { "width": 32, "height": 32, "samples": 1 } },
        ] }))
        .unwrap();
        let eval = Arc::new(Mutex::new(Evaluator::new()));
        let calls = BridgeCalls::spawn(eval.clone(), || {});
        calls.send(Call { tool: "render_frame".into(), args: json!({ "node": "render", "frame": 300 }), graph: g.clone(), path: None, dirty: false });
        // What the viewer finds each time it gets the evaluator.
        let mut seen = std::collections::BTreeSet::new();
        let done = loop {
            if let Ok(done) = calls.rx.try_recv() {
                break done;
            }
            let img = eval.lock().unwrap().evaluate(&g, "fall", &EvalContext { frame: 1, write_outputs: false }).unwrap();
            seen.insert(img.scene.as_ref().unwrap().particles[0].steps());
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(done.result.is_ok(), "{:?}", done.result);
        let between = seen.iter().filter(|&&n| n > 0 && n < 299).count();
        assert!(between >= 2, "the viewer got in during the simulation: {seen:?}");
    }

    #[test]
    fn edits_come_back_with_the_graph() {
        let calls = BridgeCalls::spawn(Arc::default(), || {});
        let done = call(&calls, "set_params", json!({ "node": "emit", "params": { "rate": 5 } }));
        assert!(done.result.is_ok() && done.changed, "{:?}", done.result);
        assert_eq!(done.graph.node("emit").unwrap().params["rate"], 5.0);
        let done = call(&calls, "particles", json!({ "node": "nope", "frame": 3 }));
        assert!(done.result.unwrap_err().contains("nope") && !done.changed);
    }
}
