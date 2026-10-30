//! App bridge: lets the MCP server and Python scripts drive a running GUI.
//!
//! Each GUI instance listens on a Unix socket in [`crate::config::runtime_dir`]
//! and records itself in the instance registry (`instances/<pid>.json` next to
//! it). Clients send one JSON object per line, `{"tool": ..., "arguments": ...}`
//! — the same tools [`crate::mcp`] serves — and get back `{"ok": result}` or
//! `{"error": message}`. The GUI runs the calls in order, each through a
//! [`crate::session::Session`] over its document (off its UI thread, sharing
//! its viewer's evaluator), so edits land in its undo history like any other.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A running GUI, as recorded in the registry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Instance {
    pub pid: u32,
    pub socket: PathBuf,
    /// Seconds since the Unix epoch.
    pub started: u64,
}

fn registry_dir() -> PathBuf {
    crate::config::runtime_dir().join("instances")
}

/// Live GUI instances, oldest first. Entries whose socket no longer accepts
/// connections (a crashed GUI) are removed.
pub fn list_instances() -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(registry_dir()) else { return Vec::new() };
    let mut out: Vec<Instance> = Vec::new();
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let inst = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Instance>(&t).ok());
        match inst {
            Some(i) if UnixStream::connect(&i.socket).is_ok() => out.push(i),
            Some(i) => {
                std::fs::remove_file(&i.socket).ok();
                std::fs::remove_file(&path).ok();
            }
            None => {}
        }
    }
    out.sort_by_key(|i| (i.started, i.pid));
    out
}

/// The instance with `pid`, or the most recently started one.
pub fn find_instance(pid: Option<u32>) -> Result<Instance> {
    let all = list_instances();
    match pid {
        Some(pid) => all.into_iter().find(|i| i.pid == pid).ok_or_else(|| anyhow!("no running Arclight GUI with pid {pid}")),
        None => all.into_iter().last().ok_or_else(|| anyhow!("no running Arclight GUI found")),
    }
}

/// Call `tool` on a running instance.
pub fn call(inst: &Instance, tool: &str, arguments: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(&inst.socket)
        .with_context(|| format!("connecting to Arclight GUI (pid {})", inst.pid))?;
    let mut line = serde_json::to_string(&json!({"tool": tool, "arguments": arguments}))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.is_empty() {
        bail!("Arclight GUI (pid {}) closed the connection", inst.pid);
    }
    let reply: Value = serde_json::from_str(&reply).context("bad reply from GUI")?;
    match reply.get("error").and_then(Value::as_str) {
        Some(e) => bail!("{e}"),
        None => Ok(reply.get("ok").cloned().unwrap_or(Value::Null)),
    }
}

/// A tool call waiting for the GUI to run it.
pub struct Pending {
    pub tool: String,
    pub arguments: Value,
    reply: Sender<Result<Value, String>>,
}

impl Pending {
    pub fn reply(self, result: Result<Value, String>) {
        self.reply.send(result).ok();
    }
}

/// The GUI side: owns the socket and registry entry, removed on drop.
pub struct Server {
    pub instance: Instance,
    registry_file: PathBuf,
    requests: Receiver<Pending>,
}

impl Server {
    /// Listen for clients. `wake` is called whenever a request arrives so the
    /// GUI can repaint and pick it up via [`Server::poll`].
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Result<Server> {
        let dir = crate::config::runtime_dir();
        std::fs::create_dir_all(registry_dir()).with_context(|| format!("creating {}", dir.display()))?;
        let pid = std::process::id();
        let socket = dir.join(format!("gui-{pid}.sock"));
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
        let started = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let instance = Instance { pid, socket, started };
        let registry_file = registry_dir().join(format!("{pid}.json"));
        std::fs::write(&registry_file, serde_json::to_string_pretty(&instance)?)?;

        let (tx, requests) = mpsc::channel::<Pending>();
        let wake = std::sync::Arc::new(wake);
        std::thread::Builder::new().name("arclight-bridge".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, wake) = (tx.clone(), wake.clone());
                std::thread::spawn(move || serve_client(stream, tx, &*wake));
            }
        })?;
        Ok(Server { instance, registry_file, requests })
    }

    /// Requests that have arrived since the last poll.
    pub fn poll(&self) -> Vec<Pending> {
        self.requests.try_iter().collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        std::fs::remove_file(&self.registry_file).ok();
        std::fs::remove_file(&self.instance.socket).ok();
    }
}

fn serve_client(stream: UnixStream, tx: Sender<Pending>, wake: &(dyn Fn() + Send + Sync)) {
    let Ok(mut writer) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                let tool = req.get("tool").and_then(Value::as_str).unwrap_or_default().to_string();
                let arguments = req.get("arguments").cloned().unwrap_or(json!({}));
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send(Pending { tool, arguments, reply: reply_tx }).is_err() {
                    return;
                }
                wake();
                // Renders run on the GUI thread and can take a while.
                match reply_rx.recv_timeout(Duration::from_secs(3600)) {
                    Ok(Ok(v)) => json!({"ok": v}),
                    Ok(Err(e)) => json!({"error": e}),
                    Err(_) => json!({"error": "the GUI did not answer"}),
                }
            }
            Err(e) => json!({"error": format!("bad request: {e}")}),
        };
        let mut text = reply.to_string();
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_a_server() {
        let dir = std::env::temp_dir().join(format!("arclight-bridge-test-{}", std::process::id()));
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let server = Server::start(|| {}).unwrap();
        let inst = server.instance.clone();
        assert!(list_instances().contains(&inst));

        let client = std::thread::spawn(move || call(&inst, "echo", &json!({"x": 1})));
        let pending = loop {
            if let Some(p) = server.poll().pop() {
                break p;
            }
            std::thread::yield_now();
        };
        assert_eq!(pending.tool, "echo");
        let args = pending.arguments.clone();
        pending.reply(Ok(args));
        assert_eq!(client.join().unwrap().unwrap(), json!({"x": 1}));

        drop(server);
        assert!(list_instances().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
