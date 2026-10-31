//! Native core of the Python `arclt` module. The Python package
//! (`python/arclt/__init__.py`) builds the public API on two calls:
//! `call(tool, args_json)`, which runs one of the MCP tools against the
//! module's session (or an attached GUI), and `pixels`, which returns a
//! rendered frame's float data.

use arclight::mcp::Server;
use arclight::session::Session;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::Value;
use std::sync::{LazyLock, Mutex};

static SERVER: LazyLock<Mutex<Server>> = LazyLock::new(|| Mutex::new(Server::new(Session::default())));

fn err(e: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Run tool `tool` with JSON arguments; returns the JSON result.
#[pyfunction]
fn call(py: Python<'_>, tool: &str, args_json: &str) -> PyResult<String> {
    let args: Value = serde_json::from_str(args_json).map_err(err)?;
    py.detach(|| {
        let mut server = SERVER.lock().map_err(err)?;
        let out = server.call(tool, &args).map_err(|e| err(format!("{e:#}")))?;
        Ok(out.to_string())
    })
}

/// Render `node` at `frame`: (width, height, bytes of float32 RGBA, row-major
/// from the top-left, linear, premultiplied).
#[pyfunction]
fn pixels(py: Python<'_>, node: &str, frame: i64) -> PyResult<(usize, usize, Py<PyBytes>)> {
    let img = py.detach(|| {
        let mut server = SERVER.lock().map_err(err)?;
        if server.attached.is_some() {
            return Err(err("pixels() needs the local session; call arclt.detach() first"));
        }
        server.session.render(node, frame).map_err(|e| err(format!("{e:#}")))
    })?;
    let img = img.flat();
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.iter().flat_map(|c| c.to_ne_bytes())).collect();
    Ok((img.width, img.height, PyBytes::new(py, &bytes).unbind()))
}

/// At interpreter exit (registered with `atexit` by the package): the
/// module's results let go and the GPU's work waited for (see
/// arclight::gpu::shutdown), so the driver isn't still at work as the
/// process ends. The device itself is left to the process's end; never
/// `_exit` here (other atexit handlers, Python's buffered output and open
/// files would be cut short).
#[pyfunction]
fn _gpu_shutdown(py: Python<'_>) {
    py.detach(|| {
        if let Ok(mut server) = SERVER.lock() {
            *server = Server::new(Session::default());
        }
        arclight::gpu::shutdown();
    });
}

/// Run a plugin host for `library` (see arclight::plugin_host): what the
/// interpreter started by this module to host a plugin runs. Returns the
/// exit code.
#[pyfunction]
fn _plugin_host(library: std::path::PathBuf) -> i32 {
    #[cfg(unix)]
    {
        arclight::crash::install_host("plugin-host");
        arclight::plugin::run_host(&library)
    }
    #[cfg(not(unix))]
    {
        let _ = library;
        2
    }
}

#[pymodule]
fn _arclt(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Plugins run in hosts of their own: this interpreter, running this
    // module's _plugin_host (the wheel has no other program to start).
    // It finds arclt where this one did: its sys.path first.
    let sys = m.py().import("sys")?;
    let exe: Option<String> = sys.getattr("executable")?.extract().ok().filter(|e: &String| !e.is_empty());
    let path: Vec<String> = sys.getattr("path")?.extract().unwrap_or_default();
    if let Some(exe) = exe {
        let path = serde_json::to_string(&path).unwrap_or_else(|_| "[]".into());
        // A process of its own for the host (never the user's interpreter):
        // it leaves with os._exit after flushing, without the exit-time
        // teardown (see arclight::gpu::exit_process).
        let code = format!("import os, sys; sys.path[:0] = {path}; from arclt import _arclt; code = _arclt._plugin_host(sys.argv[1]); sys.stdout.flush(); sys.stderr.flush(); os._exit(code)");
        arclight::plugin::set_host_command(vec![exe.into(), "-c".into(), code.into()]);
    }
    arclight::plugin::init();
    m.add_function(wrap_pyfunction!(_plugin_host, m)?)?;
    m.add_function(wrap_pyfunction!(call, m)?)?;
    m.add_function(wrap_pyfunction!(pixels, m)?)?;
    m.add_function(wrap_pyfunction!(_gpu_shutdown, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
