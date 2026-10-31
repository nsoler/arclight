//! Native plugin host: loads shared libraries implementing the C ABI in
//! `include/arclight_plugin.h` and registers their ops next to the built-ins.
//!
//! A plugin's ops behave like built-in ones everywhere: they appear in the
//! GUI's node menus with generated property panels, can be keyframed, are
//! cached by the evaluator and are scriptable from Python and MCP. Libraries
//! are never unloaded, so the registry can hand out `&'static` op infos.
//!
//! Where a front end says how to start a helper ([`set_host_command`]: the
//! CLI's `plugin-host`, the GUI's `--plugin-host`, arclt's interpreter),
//! each library runs in helper processes of its own (see
//! [`crate::plugin_host`]): a plugin that crashes, hangs or scribbles over
//! memory gives a node error instead of taking Arclight down.
//! `ARCLIGHT_PLUGINS_IN_PROCESS=1` loads them in-process instead (trusted
//! plugins, a little faster), as does every embedding that sets no command.

use crate::image::Image;
use crate::ops::{self, Category, DefaultValue, EvalContext, OpInfo, Overlay, ParamKind, ParamSpec, Port};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::ffi::{c_char, c_void, CStr, CString, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Once, RwLock};
use std::time::Duration;

pub const ABI_VERSION: u32 = 1;

#[repr(C)]
pub(crate) struct ArclightImage {
    pub width: u32,
    pub height: u32,
    pub pixels: *const f32,
}

pub(crate) type Alloc = unsafe extern "C" fn(host: *mut c_void, width: u32, height: u32) -> *mut f32;

#[repr(C)]
pub(crate) struct ArclightPlugin {
    abi_version: u32,
    ops_json: *const c_char,
    render: unsafe extern "C" fn(
        op: *const c_char,
        params_json: *const c_char,
        inputs: *const ArclightImage,
        n_inputs: u32,
        frame: i64,
        host: *mut c_void,
        alloc: Alloc,
        error: *mut c_char,
        error_len: usize,
    ) -> i32,
}

// The entry table is immutable static data in the plugin; the ABI requires
// `render` to be callable from several threads at once.
unsafe impl Sync for ArclightPlugin {}

/// Where an op's library runs.
#[derive(Clone, Copy)]
enum Backend {
    InProcess(&'static ArclightPlugin),
    #[cfg(unix)]
    Hosted(&'static crate::plugin_host::Pool),
}

struct PluginOp {
    info: &'static OpInfo,
    backend: Backend,
    time_varying: bool,
    library: &'static Path,
}

static REGISTRY: RwLock<Vec<PluginOp>> = RwLock::new(Vec::new());
static REPORT: RwLock<Vec<LoadReport>> = RwLock::new(Vec::new());
static INIT: Once = Once::new();
static HOST_COMMAND: RwLock<Option<Vec<OsString>>> = RwLock::new(None);
static TIMEOUT: RwLock<Option<Duration>> = RwLock::new(None);

/// How to start a plugin host: a program and its arguments, to which the
/// library's path is added; it must call [`run_host`] with it. Set before
/// [`init`] (or [`load`]) for plugins to run out of process.
pub fn set_host_command(command: Vec<OsString>) {
    *HOST_COMMAND.write().unwrap() = Some(command);
}

/// The host command, unless plugins load in-process (none was set, or
/// `ARCLIGHT_PLUGINS_IN_PROCESS` is, or there's no host on this system).
pub(crate) fn host_command() -> Option<Vec<OsString>> {
    if !cfg!(unix) || std::env::var_os("ARCLIGHT_PLUGINS_IN_PROCESS").is_some_and(|v| !v.is_empty() && v != "0") {
        return None;
    }
    HOST_COMMAND.read().unwrap().clone()
}

/// How long a plugin host may take over one render (or starting) before
/// it's stopped: [`set_timeout`], else `ARCLIGHT_PLUGIN_TIMEOUT` seconds,
/// else 60 s.
pub fn timeout() -> Duration {
    if let Some(t) = *TIMEOUT.read().unwrap() {
        return t;
    }
    let secs = std::env::var("ARCLIGHT_PLUGIN_TIMEOUT").ok().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s > 0.0);
    Duration::from_secs_f64(secs.unwrap_or(60.0))
}

pub fn set_timeout(t: Duration) {
    *TIMEOUT.write().unwrap() = Some(t);
}

/// A plugin host's whole life: load `library` and render for the process
/// that started it, over stdin and stdout, until it closes them. Returns the
/// exit code.
#[cfg(unix)]
pub fn run_host(library: &Path) -> i32 {
    crate::plugin_host::serve(library)
}

/// What happened to one library during discovery.
#[derive(Clone, Debug)]
pub struct LoadReport {
    pub path: PathBuf,
    pub result: Result<Vec<String>, String>,
}

/// Load every plugin from [`crate::config::plugin_dirs`], once per process.
/// Failures are recorded (see [`reports`]) rather than returned, so one bad
/// library can't keep the application from starting.
pub fn init() {
    INIT.call_once(|| {
        // A plugin host serves one library and loads no others.
        #[cfg(unix)]
        if std::env::var_os(crate::plugin_host::HOST_ENV).is_some() {
            return;
        }
        let mut reports = Vec::new();
        for dir in crate::config::plugin_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut libs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(is_plugin_file))
                .collect();
            libs.sort();
            for path in libs {
                let result = load(&path).map_err(|e| format!("{e:#}"));
                if let Err(e) = &result {
                    eprintln!("arclight: plugin {}: {e}", path.display());
                }
                reports.push(LoadReport { path, result });
            }
        }
        *REPORT.write().unwrap() = reports;
    });
}

/// Plugin libraries are named `libarclight_plugin_*.so` or `*.arclight.so`
/// (`.dylib` on macOS), so other libraries sharing a directory are never
/// loaded (loading runs code).
pub fn is_plugin_file(name: &str) -> bool {
    let ext = std::env::consts::DLL_SUFFIX;
    (name.starts_with("libarclight_plugin_") && name.ends_with(ext)) || name.ends_with(&format!(".arclight{ext}"))
}

/// Every library [`init`] tried to load.
pub fn reports() -> Vec<LoadReport> {
    REPORT.read().unwrap().clone()
}

/// Load one plugin library and register its ops. Returns the op names.
/// Out of process (see [`set_host_command`]), its ops come from a host
/// started to describe them, or, for a library unchanged since, from the
/// last time; a library whose host can't start then still has its ops,
/// which give node errors saying why.
pub fn load(path: &Path) -> Result<Vec<String>> {
    #[cfg(unix)]
    if let Some(command) = host_command() {
        let library: &'static Path = Box::leak(path.to_path_buf().into_boxed_path());
        let pool: &'static crate::plugin_host::Pool = Box::leak(Box::new(crate::plugin_host::Pool::new(library, command)));
        let (json, failed) = pool.describe()?;
        let names = register(library, &json, Backend::Hosted(pool))?;
        return match failed {
            Some(e) => Err(anyhow!("{e} (its ops are kept from the last time it loaded)")),
            None => Ok(names),
        };
    }
    let (entry, json) = open_library(path)?;
    let library: &'static Path = Box::leak(path.to_path_buf().into_boxed_path());
    register(library, json, Backend::InProcess(entry))
}

/// Load `path` into this process: its entry table and ops JSON.
pub(crate) fn open_library(path: &Path) -> Result<(&'static ArclightPlugin, &'static str)> {
    // SAFETY: loading a library runs its initializers; plugins are trusted
    // code the user installed, as with any native plugin host.
    let lib = unsafe { libloading::Library::new(path) }.with_context(|| format!("loading {}", path.display()))?;
    let lib: &'static libloading::Library = Box::leak(Box::new(lib));
    let entry: &'static ArclightPlugin = unsafe {
        let f = lib
            .get::<unsafe extern "C" fn() -> *const ArclightPlugin>(b"arclight_plugin_entry\0")
            .context("missing symbol arclight_plugin_entry")?;
        f().as_ref().ok_or_else(|| anyhow!("arclight_plugin_entry returned NULL"))?
    };
    if entry.abi_version != ABI_VERSION {
        bail!("plugin ABI {} is not supported (expected {ABI_VERSION})", entry.abi_version);
    }
    if entry.ops_json.is_null() {
        bail!("ops_json is NULL");
    }
    let json = unsafe { CStr::from_ptr(entry.ops_json) }.to_str().context("ops_json is not UTF-8")?;
    Ok((entry, json))
}

/// Register the ops `json` describes, run by `backend`.
fn register(library: &'static Path, json: &str, backend: Backend) -> Result<Vec<String>> {
    let descs: Vec<OpDesc> = serde_json::from_str(json).context("parsing ops_json")?;
    let mut names = Vec::new();
    for desc in descs {
        let name = desc.name.clone();
        if ops::info(&name).is_some() {
            bail!("op '{name}' is already defined");
        }
        let time_varying = desc.time_varying;
        let info = Box::leak(Box::new(desc.into_info()?));
        REGISTRY.write().unwrap().push(PluginOp { info, backend, time_varying, library });
        names.push(name);
    }
    Ok(names)
}

pub fn info(op: &str) -> Option<&'static OpInfo> {
    REGISTRY.read().unwrap().iter().find(|p| p.info.name == op).map(|p| p.info)
}

pub fn infos() -> Vec<&'static OpInfo> {
    REGISTRY.read().unwrap().iter().map(|p| p.info).collect()
}

/// The library an op was loaded from, if it's a plugin op.
pub fn library(op: &str) -> Option<&'static Path> {
    REGISTRY.read().unwrap().iter().find(|p| p.info.name == op).map(|p| p.library)
}

pub fn is_time_varying(op: &str) -> bool {
    REGISTRY.read().unwrap().iter().any(|p| p.info.name == op && p.time_varying)
}

/// Render plugin op `op`, or `None` if no plugin defines it.
pub fn eval(op: &str, params: &Value, inputs: &[Option<&Image>], ctx: &EvalContext) -> Option<Result<Image>> {
    let backend = REGISTRY.read().unwrap().iter().find(|p| p.info.name == op).map(|p| p.backend)?;
    let params = ops::params_with_defaults(op, params).to_string();
    Some(match backend {
        Backend::InProcess(entry) => render(entry, op, &params, inputs, ctx.frame),
        #[cfg(unix)]
        Backend::Hosted(pool) => pool.render(op, &params, inputs, ctx.frame),
    })
}

struct HostOut {
    image: Option<Image>,
}

unsafe extern "C" fn alloc(host: *mut c_void, width: u32, height: u32) -> *mut f32 {
    let out = unsafe { &mut *(host as *mut HostOut) };
    let img = out.image.insert(Image::new(width as usize, height as usize));
    img.pixels.as_mut_ptr() as *mut f32
}

/// Render in this process.
fn render(entry: &ArclightPlugin, op: &str, params: &str, inputs: &[Option<&Image>], frame: i64) -> Result<Image> {
    // ABI v1 has no data windows: plugins see whole frames.
    let frames: Vec<Option<std::borrow::Cow<Image>>> = inputs.iter().map(|i| i.map(Image::flat)).collect();
    let images: Vec<ArclightImage> = frames
        .iter()
        .map(|i| match i {
            Some(img) => ArclightImage { width: img.width as u32, height: img.height as u32, pixels: img.pixels.as_ptr() as *const f32 },
            None => ArclightImage { width: 0, height: 0, pixels: std::ptr::null() },
        })
        .collect();
    let mut out = HostOut { image: None };
    // SAFETY: every pointer stays valid for the call.
    unsafe { call(entry, op, params, &images, frame, &mut out as *mut HostOut as *mut c_void, alloc)? };
    out.image.ok_or_else(|| anyhow!("plugin returned no image"))
}

/// Call `entry`'s render with `images`, its output made by `alloc(host, ..)`.
/// The plugin's error message (or status) on failure.
///
/// # Safety
/// The images' pixels and `host` must be valid for the call; the ABI
/// forbids the plugin from keeping them.
pub(crate) unsafe fn call(entry: &ArclightPlugin, op: &str, params: &str, images: &[ArclightImage], frame: i64, host: *mut c_void, alloc: Alloc) -> Result<()> {
    let c_op = CString::new(op)?;
    let c_params = CString::new(params)?;
    let mut error = vec![0u8; 1024];
    let status = unsafe {
        (entry.render)(c_op.as_ptr(), c_params.as_ptr(), images.as_ptr(), images.len() as u32, frame, host, alloc, error.as_mut_ptr() as *mut c_char, error.len())
    };
    if status != 0 {
        let msg = CStr::from_bytes_until_nul(&error).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        bail!("{}", if msg.is_empty() { format!("plugin failed with status {status}") } else { msg });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Descriptors
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpDesc {
    name: String,
    label: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    help: String,
    #[serde(default)]
    time_varying: bool,
    #[serde(default)]
    inputs: Vec<PortDesc>,
    #[serde(default)]
    params: Vec<ParamDesc>,
}

#[derive(Deserialize)]
struct PortDesc {
    label: String,
    #[serde(default)]
    optional: bool,
}

#[derive(Deserialize)]
struct ParamDesc {
    name: String,
    label: Option<String>,
    kind: String,
    #[serde(default)]
    default: Value,
    min: Option<f64>,
    max: Option<f64>,
    #[serde(default)]
    choices: Vec<String>,
    animatable: Option<bool>,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn category(name: Option<&str>) -> Result<Category> {
    Ok(match name.unwrap_or("filter") {
        "source" => Category::Source,
        "color" => Category::Color,
        "channel" => Category::Channel,
        "keyer" => Category::Keyer,
        "merge" => Category::Merge,
        "shape" => Category::Shape,
        "transform" => Category::Transform,
        "filter" => Category::Filter,
        "output" => Category::Output,
        other => bail!("unknown category '{other}'"),
    })
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl OpDesc {
    fn into_info(self) -> Result<OpInfo> {
        if !valid_name(&self.name) {
            bail!("op name '{}' must be lowercase letters, digits and _", self.name);
        }
        let params = self
            .params
            .into_iter()
            .map(|p| p.into_spec().with_context(|| format!("op '{}'", self.name)))
            .collect::<Result<Vec<_>>>()?;
        let inputs: Vec<Port> = self
            .inputs
            .into_iter()
            .map(|p| Port { label: leak(p.label), optional: p.optional, kind: ops::Kind::Image })
            .collect();
        Ok(OpInfo {
            label: leak(self.label.unwrap_or_else(|| self.name.clone())),
            name: leak(self.name),
            category: category(self.category.as_deref())?,
            inputs: Box::leak(inputs.into_boxed_slice()),
            params: Box::leak(params.into_boxed_slice()),
            mask_base: None,
            overlay: Overlay::None,
            help: leak(self.help),
        })
    }
}

impl ParamDesc {
    fn into_spec(self) -> Result<ParamSpec> {
        if !valid_name(&self.name) {
            bail!("param name '{}' must be lowercase letters, digits and _", self.name);
        }
        let (min, max) = (self.min.unwrap_or(0.0), self.max.unwrap_or(1.0));
        let (kind, fallback) = match self.kind.as_str() {
            "float" => (ParamKind::Float { min: min as f32, max: max as f32 }, Value::from(0.0)),
            "int" => (
                ParamKind::Int { min: self.min.unwrap_or(0.0) as i64, max: self.max.unwrap_or(100.0) as i64 },
                Value::from(0),
            ),
            "vec2" => (ParamKind::Vec2, serde_json::json!([0.0, 0.0])),
            "vec3" => (ParamKind::Vec3, serde_json::json!([0.0, 0.0, 0.0])),
            "color" => (ParamKind::Color, serde_json::json!([1.0, 1.0, 1.0, 1.0])),
            "bool" => (ParamKind::Bool, Value::from(false)),
            "path" => (ParamKind::Path { save: false }, Value::from("")),
            "choice" => {
                if self.choices.is_empty() {
                    bail!("choice param '{}' has no choices", self.name);
                }
                let first = Value::from(self.choices[0].clone());
                let choices: Vec<&'static str> = self.choices.into_iter().map(leak).collect();
                (ParamKind::Choice(Box::leak(choices.into_boxed_slice())), first)
            }
            other => bail!("param '{}' has unknown kind '{other}'", self.name),
        };
        let default = if self.default.is_null() { fallback } else { self.default };
        Ok(ParamSpec {
            label: leak(self.label.unwrap_or_else(|| self.name.clone())),
            name: leak(self.name),
            kind,
            default: DefaultValue::Static(Box::leak(Box::new(default))),
            animatable: self.animatable.unwrap_or(kind.is_numeric()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_builds_op_info() {
        let desc: OpDesc = serde_json::from_str(
            r#"{"name": "test_desc_op", "category": "color",
                "inputs": [{"label": "input"}, {"label": "mask", "optional": true}],
                "params": [{"name": "gain", "kind": "float", "min": 0, "max": 4, "default": 2},
                           {"name": "mode", "kind": "choice", "choices": ["a", "b"]}]}"#,
        )
        .unwrap();
        let info = desc.into_info().unwrap();
        assert_eq!(info.label, "test_desc_op");
        assert_eq!(info.category, Category::Color);
        assert!(info.inputs[1].optional);
        assert_eq!(info.params[0].default_value(), serde_json::json!(2));
        assert_eq!(info.params[1].default_value(), serde_json::json!("a"));
        assert!(info.params[0].animatable && !info.params[1].animatable);
    }

    #[test]
    fn only_plugin_named_libraries_are_loaded() {
        let ext = std::env::consts::DLL_SUFFIX;
        assert!(is_plugin_file(&format!("libarclight_plugin_example{ext}")));
        assert!(is_plugin_file(&format!("grain.arclight{ext}")));
        assert!(!is_plugin_file(&format!("lib_arclt{ext}")));
        assert!(!is_plugin_file(&format!("libarclight_plugin_example{ext}.d")));
    }

    #[test]
    fn bad_names_are_rejected() {
        let desc: OpDesc = serde_json::from_str(r#"{"name": "Bad Name"}"#).unwrap();
        assert!(desc.into_info().is_err());
    }
}
