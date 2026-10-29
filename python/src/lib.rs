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
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.iter().flat_map(|c| c.to_ne_bytes())).collect();
    Ok((img.width, img.height, PyBytes::new(py, &bytes).unbind()))
}

#[pymodule]
fn _arclt(m: &Bound<'_, PyModule>) -> PyResult<()> {
    arclight::plugin::init();
    m.add_function(wrap_pyfunction!(call, m)?)?;
    m.add_function(wrap_pyfunction!(pixels, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
