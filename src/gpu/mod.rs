//! GPU acceleration (wgpu): the device, the switch and its status.
//!
//! One device per process, created lazily the first time something asks
//! for it ([`get`]), headless (no window), on the high-performance adapter
//! of wgpu's primary backends (Vulkan / Metal), with the adapter's own
//! limits. Every evaluator thread shares it. The GPU is used when all
//! hold: built with the `gpu` feature, `ARCLIGHT_GPU` isn't `0`, the
//! Preferences switch is on ([`set_enabled`]), a hardware adapter came up,
//! no GPU error lost the device, and the calling thread isn't inside
//! [`cpu_only`]. Otherwise [`get`] is None and everything renders on the
//! CPU, which stays the reference. See `docs/gpu-plan.md`.
//!
//! Built without the feature this is a stub: [`status`] says "not built
//! with GPU support" and [`get`] is always None, so callers need no `cfg`.

pub mod check;
#[cfg(feature = "gpu")]
pub mod display;
#[cfg(feature = "gpu")]
pub mod image;
#[cfg(feature = "gpu")]
pub mod ops;

#[cfg(feature = "gpu")]
pub use image::{GpuImage, Transfer};

use serde::Serialize;
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Whether GPU acceleration is in use, and if not, why.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    /// On: the adapter's name, its backend ("Vulkan", "Metal") and whether
    /// images move by shared memory (integrated GPUs such as Apple Silicon)
    /// rather than copies over the bus.
    Active { adapter: String, backend: String, unified: bool },
    /// Off: "turned off in Preferences", "ARCLIGHT_GPU=0", "no GPU adapter
    /// (Vulkan / Metal)", "not built with GPU support", "GPU error: …, using
    /// the CPU", …
    Cpu { reason: String },
}

impl Status {
    pub fn is_active(&self) -> bool {
        matches!(self, Status::Active { .. })
    }

    /// "Using NVIDIA GeForce RTX 5070 (Vulkan)" or "Using the CPU: <reason>".
    pub fn line(&self) -> String {
        match self {
            Status::Active { adapter, backend, .. } => format!("Using {adapter} ({backend})"),
            Status::Cpu { reason } => format!("Using the CPU: {reason}"),
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line())
    }
}

/// The Preferences switch: [`UNREAD`] until first asked for, then
/// [`ON`] or [`OFF`].
static ENABLED: AtomicU8 = AtomicU8::new(UNREAD);
const UNREAD: u8 = 0;
const ON: u8 = 1;
const OFF: u8 = 2;
/// Why the GPU was turned off for the rest of the process (a lost device).
static LOST: Mutex<Option<String>> = Mutex::new(None);
/// The last GPU error, lost device or not (for `arclight gpu`).
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    /// How deep this thread is in [`cpu_only`].
    static CPU_ONLY: Cell<u32> = const { Cell::new(0) };
}

/// Turn GPU acceleration on or off (Preferences ▸ Performance). Read by
/// every evaluation, so the next render follows it; no restart.
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { ON } else { OFF }, Ordering::Relaxed);
    crate::crash::refresh_header();
}

/// The switch as last set; until then as `settings.json` has it
/// (`"gpu": {"enabled": false}` turns it off), so the CLI, MCP and Python
/// follow Preferences too. On by default.
pub fn enabled() -> bool {
    if ENABLED.load(Ordering::Relaxed) == UNREAD {
        let on = setting_in(&std::fs::read_to_string(crate::config::Settings::path()).unwrap_or_default());
        let _ = ENABLED.compare_exchange(UNREAD, if on { ON } else { OFF }, Ordering::Relaxed, Ordering::Relaxed);
    }
    ENABLED.load(Ordering::Relaxed) != OFF
}

/// `"gpu": {"enabled": ...}` in settings JSON `text`; on when missing or
/// unreadable.
fn setting_in(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).ok().and_then(|v| v["gpu"]["enabled"].as_bool()).unwrap_or(true)
}

/// `ARCLIGHT_GPU`'s value when it turns the GPU off (`0`, `off`, `false`
/// or `no`), read once.
fn env_off() -> Option<&'static str> {
    static OFF: OnceLock<Option<String>> = OnceLock::new();
    OFF.get_or_init(|| std::env::var("ARCLIGHT_GPU").ok().filter(|v| ["0", "off", "false", "no"].contains(&v.trim().to_lowercase().as_str())))
        .as_deref()
}

/// Whether `ARCLIGHT_GPU=0` forces the CPU (overriding Preferences).
pub fn forced_off() -> bool {
    env_off().is_some()
}

/// Why the GPU is off before any device is asked for, in order of
/// precedence; None when it may be used.
fn off_reason(built: bool, env: Option<&str>, enabled: bool, lost: Option<&str>) -> Option<String> {
    if !built {
        Some("not built with GPU support".into())
    } else if let Some(v) = env {
        Some(format!("ARCLIGHT_GPU={v}"))
    } else if !enabled {
        Some("turned off in Preferences".into())
    } else {
        lost.map(|e| format!("GPU error: {e}, using the CPU"))
    }
}

fn lost() -> Option<String> {
    LOST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn current_off_reason() -> Option<String> {
    off_reason(cfg!(feature = "gpu"), env_off(), enabled(), lost().as_deref())
}

/// The current status. Creates the device on the first call when the GPU
/// may be used (once per process, ~0.1–0.3 s).
pub fn status() -> Status {
    if let Some(reason) = current_off_reason() {
        return Status::Cpu { reason };
    }
    match device() {
        Ok(g) => device_status(&g),
        Err(reason) => Status::Cpu { reason },
    }
}

/// Whether this thread would use the GPU, without creating the device:
/// [`get`]'s answer as far as it can be known in advance (a device that
/// then fails to come up still renders on the CPU).
pub fn wanted() -> bool {
    CPU_ONLY.with(Cell::get) == 0 && current_off_reason().is_none() && !matches!(created(), Some(Err(_)))
}

/// The ops with a GPU path (see `gpu::ops`).
pub const OPS: &[&str] = &[
    "grade", "exposure", "saturation", "invert", "clamp", "premult", "unpremult", "channel_select", "switch", "dissolve", "keymix", "constant",
    "checkerboard", "merge", "blur", "transform", "corner_pin", "tracker", "lens_distort", "resize", "keyer",
];

/// What a node's cache key mixes in for the backend: 1 when `op` has a
/// GPU path and this thread would use the GPU, else 0. So turning the GPU
/// on or off re-renders those nodes (and what's below them), and CPU and
/// GPU results are never mixed up.
pub fn backend_tag(op: &str) -> u8 {
    u8::from(OPS.contains(&op) && wanted())
}

/// How much of the GPU's memory an evaluator's cached results may hold
/// (settings.json `"gpu": {"cache_mb": ...}`, default 2048); past it the
/// least recently used go back to the CPU.
pub fn cache_budget() -> u64 {
    static MB: OnceLock<u64> = OnceLock::new();
    let mb = *MB.get_or_init(|| {
        let text = std::fs::read_to_string(crate::config::Settings::path()).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["gpu"]["cache_mb"].as_u64()).unwrap_or(2048)
    });
    mb << 20
}

/// A cached result left on the GPU, read back on the first CPU need and
/// then kept (see `graph::Evaluator`).
#[cfg(feature = "gpu")]
pub struct Resident {
    pub image: GpuImage,
    gpu: Arc<Gpu>,
    cpu: OnceLock<Arc<crate::image::Image>>,
}

#[cfg(feature = "gpu")]
impl Resident {
    pub fn new(gpu: Arc<Gpu>, image: GpuImage) -> Resident {
        Resident { image, gpu, cpu: OnceLock::new() }
    }

    /// An upload of `cpu`, which it keeps (no read-back needed).
    pub fn uploaded(gpu: Arc<Gpu>, image: GpuImage, cpu: Arc<crate::image::Image>) -> Resident {
        Resident { image, gpu, cpu: OnceLock::from(cpu) }
    }

    /// The image on the CPU: read back once.
    pub fn cpu(&self) -> anyhow::Result<Arc<crate::image::Image>> {
        if let Some(img) = self.cpu.get() {
            return Ok(img.clone());
        }
        let img = Arc::new(self.image.read_back(&self.gpu)?);
        Ok(self.cpu.get_or_init(|| img).clone())
    }

    /// GPU memory held.
    pub fn bytes(&self) -> u64 {
        self.image.bytes()
    }

    /// Its frame and data window (no read-back).
    pub fn shape(&self) -> (crate::image::Size, crate::image::IRect) {
        (self.image.format, self.image.window)
    }

    /// The device it lives on.
    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }
}

/// Built without GPU support: nothing is ever left on a GPU.
#[cfg(not(feature = "gpu"))]
pub enum Resident {}

#[cfg(not(feature = "gpu"))]
impl Resident {
    pub fn cpu(&self) -> anyhow::Result<Arc<crate::image::Image>> {
        match *self {}
    }
    pub fn bytes(&self) -> u64 {
        match *self {}
    }
    pub fn shape(&self) -> (crate::image::Size, crate::image::IRect) {
        match *self {}
    }
}

/// The device for a window to draw with (the GUI hands it to eframe), so
/// the viewer can show results that stay on the GPU without reading them
/// back: whatever Preferences say, so turning GPU acceleration on later
/// still shares it. None with `ARCLIGHT_GPU=0`, without a hardware
/// adapter, on GL (`WGPU_BACKEND=gl`: a GL window needs a display handle
/// this headless device was made without), or not built with the
/// feature: the window then makes its own.
pub fn for_window() -> Option<Arc<Gpu>> {
    if !cfg!(feature = "gpu") || forced_off() || lost().is_some() {
        return None;
    }
    device().ok().filter(|g| !g.software() && !g.gl())
}

/// The device for GPU work, or None: off, forced off, no hardware
/// adapter, not built, inside [`cpu_only`], or after a lost device.
pub fn get() -> Option<Arc<Gpu>> {
    if CPU_ONLY.with(Cell::get) > 0 || current_off_reason().is_some() {
        return None;
    }
    device().ok().filter(|g| !g.software())
}

/// For tests: the device whatever the switch says (a software adapter
/// such as lavapipe counts); None without an adapter or the feature, or
/// after a lost device.
pub fn test_device() -> Option<Arc<Gpu>> {
    if !cfg!(feature = "gpu") || lost().is_some() {
        return None;
    }
    device().ok()
}

/// Run `f` with the GPU off for this thread (a `--cpu` render: exact CPU
/// results, as with `ARCLIGHT_GPU=0`, for one call).
pub fn cpu_only<R>(f: impl FnOnce() -> R) -> R {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            CPU_ONLY.with(|d| d.set(d.get() - 1));
        }
    }
    CPU_ONLY.with(|d| d.set(d.get() + 1));
    let _leave = Leave;
    f()
}

/// Record a GPU failure: the node that hit it renders on the CPU; a lost
/// device (`device_lost`) also turns the GPU off for the rest of the
/// process, which [`status`] then says.
pub fn report_error(msg: impl std::fmt::Display, device_lost: bool) {
    let msg = msg.to_string();
    eprintln!("arclight: GPU error{}: {msg}", if device_lost { " (device lost)" } else { "" });
    crate::crash::note(format!("GPU error{}: {msg}", if device_lost { " (device lost)" } else { "" }));
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
    if device_lost {
        LOST.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(msg);
        crate::crash::refresh_header();
    }
}

/// The last GPU error recorded, if any.
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The crash report's "GPU acceleration:" value, without creating a
/// device: "on (<adapter> (<backend>))", "on (not used yet)" or
/// "off (<reason>)".
pub fn crash_line() -> String {
    if let Some(reason) = current_off_reason() {
        return format!("off ({reason})");
    }
    match created().map(|d| d.map(|g| device_status(&g))) {
        Some(Ok(Status::Active { adapter, backend, .. })) => format!("on ({adapter} ({backend}))"),
        Some(Ok(Status::Cpu { reason }) | Err(reason)) => format!("off ({reason})"),
        None => "on (not used yet)".into(),
    }
}

/// What `arclight gpu`, MCP `gpu_info` and Python `arclt.gpu_info()`
/// report: the status, the switch, the adapter, its limits and the
/// transfer path. Creates the device when the GPU may be used.
pub fn info() -> serde_json::Value {
    let status = status();
    let mut out = serde_json::json!({
        "status": status,
        "line": status.line(),
        "enabled": enabled(),
        "forced_off": forced_off(),
        "built": cfg!(feature = "gpu"),
        "last_error": last_error(),
    });
    if let Some(Ok(g)) = created() {
        out["adapter"] = g.adapter_json();
        out["transfer"] = g.transfer().into();
    }
    out
}

// -- the device -------------------------------------------------------------

/// The device once asked for (or why there is none); taken by [`shutdown`].
#[cfg(feature = "gpu")]
static DEVICE: Mutex<Option<Result<Arc<Gpu>, String>>> = Mutex::new(None);

/// The device, created on first use (or why there is none).
#[cfg(feature = "gpu")]
fn device() -> Result<Arc<Gpu>, String> {
    let mut slot = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = slot.as_ref() {
        return d.clone();
    }
    let d = Gpu::create().map(Arc::new);
    *slot = Some(d.clone());
    drop(slot);
    if let Ok(g) = &d {
        crate::crash::set_gpu(g.describe());
    }
    crate::crash::refresh_header();
    d
}

/// Before the process exits: wait (up to 5 s) for the GPU to finish what
/// it was given. The device is kept, not torn down: the process's end
/// does that (see [`exit_process`]). After this the GPU is off ("shut
/// down").
pub fn shutdown() {
    #[cfg(feature = "gpu")]
    {
        let taken = DEVICE.lock().unwrap_or_else(|e| e.into_inner()).replace(Err("shut down".into()));
        if let Some(Ok(gpu)) = taken {
            let _ = gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(5)) });
            std::mem::forget(gpu);
        }
    }
}

/// End the process with `code`: [`shutdown`], standard output and error
/// flushed, then `_exit` (on Unix), which skips the C libraries'
/// exit-time teardown. NVIDIA's Vulkan driver keeps threads of its own
/// that crash when its libraries' destructors run under them at `exit`
/// (a jump to address 0x170 on Linux). For the end of a program's
/// `main`: nothing else is cleaned up (Rust drops no statics at exit
/// anyway; files written were closed by then).
pub fn exit_process(code: i32) -> ! {
    use std::io::Write;
    shutdown();
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: ends the process at once; nothing runs after it.
    #[cfg(unix)]
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

#[cfg(not(feature = "gpu"))]
fn device() -> Result<Arc<Gpu>, String> {
    Err("not built with GPU support".into())
}

/// The device if it was already created (or tried).
#[cfg(feature = "gpu")]
fn created() -> Option<Result<Arc<Gpu>, String>> {
    DEVICE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(not(feature = "gpu"))]
fn created() -> Option<Result<Arc<Gpu>, String>> {
    None
}

/// Built without GPU support: no device can exist.
#[cfg(not(feature = "gpu"))]
pub enum Gpu {}

#[cfg(not(feature = "gpu"))]
impl Gpu {
    fn software(&self) -> bool {
        match *self {}
    }
    fn gl(&self) -> bool {
        match *self {}
    }
    fn transfer(&self) -> &'static str {
        match *self {}
    }
    fn adapter_json(&self) -> serde_json::Value {
        match *self {}
    }
}

#[cfg(not(feature = "gpu"))]
fn device_status(g: &Gpu) -> Status {
    match *g {}
}

/// A device's status: active, unless it is a software adapter.
#[cfg(feature = "gpu")]
fn device_status(g: &Gpu) -> Status {
    if g.software() {
        Status::Cpu { reason: format!("only a software GPU adapter ({})", g.info.name) }
    } else {
        Status::Active { adapter: g.info.name.clone(), backend: g.backend(), unified: g.unified }
    }
}

/// The process's GPU: device, queue, what the adapter is and offers, and
/// the compute pipelines built so far.
#[cfg(feature = "gpu")]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// What the device came from, for a window to share it (see
    /// [`for_window`]).
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub info: wgpu::AdapterInfo,
    /// The limits the device was created with: the adapter's own.
    pub limits: wgpu::Limits,
    /// Optional features turned on: `MAPPABLE_PRIMARY_BUFFERS` and
    /// `FLOAT32_FILTERABLE` when the adapter has them.
    pub features: wgpu::Features,
    /// Images move through shared memory (an integrated GPU with mappable
    /// storage buffers, e.g. Apple Silicon) instead of staging copies.
    pub unified: bool,
    pipelines: Mutex<std::collections::HashMap<(&'static str, &'static str), Arc<wgpu::ComputePipeline>>>,
    /// Read-back staging buffers kept for the next read-back.
    staging: Mutex<Vec<wgpu::Buffer>>,
}

#[cfg(feature = "gpu")]
impl Gpu {
    fn create() -> Result<Gpu, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        // Vulkan / Metal (not GL: its compute support is too thin), unless
        // WGPU_BACKEND says otherwise.
        desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let instance = wgpu::Instance::new(desc);
        let options = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
            ..Default::default()
        };
        let adapter = pollster::block_on(instance.request_adapter(&options)).map_err(|_| "no GPU adapter (Vulkan / Metal)".to_string())?;
        let info = adapter.get_info();
        let features = adapter.features() & (wgpu::Features::MAPPABLE_PRIMARY_BUFFERS | wgpu::Features::FLOAT32_FILTERABLE);
        let limits = adapter.limits();
        let descriptor = wgpu::DeviceDescriptor { label: Some("arclight"), required_features: features, required_limits: limits.clone(), ..Default::default() };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).map_err(|e| format!("GPU error: {e}, using the CPU"))?;
        // wgpu panics on errors nobody catches; here they are recorded and
        // the node renders on the CPU (see `Gpu::scoped`).
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| report_error(e, false)));
        device.set_device_lost_callback(|reason, msg| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                report_error(format!("device lost: {msg}"), true);
            }
        });
        let unified = info.device_type == wgpu::DeviceType::IntegratedGpu && features.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        Ok(Gpu { device, queue, instance, adapter, info, limits, features, unified, pipelines: Mutex::default(), staging: Mutex::default() })
    }

    /// A software adapter (lavapipe, llvmpipe): fine for tests, slower
    /// than the CPU renderer for work.
    fn software(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }

    /// On OpenGL (only when asked for with `WGPU_BACKEND`).
    fn gl(&self) -> bool {
        self.info.backend == wgpu::Backend::Gl
    }

    /// "Vulkan", "Metal", ...
    pub fn backend(&self) -> String {
        format!("{:?}", self.info.backend)
    }

    /// How images move: "unified memory" or "discrete transfers".
    pub fn transfer(&self) -> &'static str {
        if self.unified { "unified memory" } else { "discrete transfers" }
    }

    /// The adapter for crash reports, as the GUI writes egui's.
    fn describe(&self) -> String {
        let i = &self.info;
        format!("{} ({:?}, {:?}, driver {} {})", i.name, i.backend, i.device_type, i.driver, i.driver_info)
    }

    fn adapter_json(&self) -> serde_json::Value {
        let (i, l) = (&self.info, &self.limits);
        let mut features = Vec::new();
        for (f, name) in [(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS, "mappable primary buffers"), (wgpu::Features::FLOAT32_FILTERABLE, "float32 filterable")] {
            if self.features.contains(f) {
                features.push(name);
            }
        }
        serde_json::json!({
            "name": i.name,
            "backend": self.backend(),
            "device_type": format!("{:?}", i.device_type),
            "driver": i.driver,
            "driver_info": i.driver_info,
            "vendor": i.vendor,
            "device": i.device,
            "features": features,
            "limits": {
                "max_buffer_size": l.max_buffer_size,
                "max_storage_buffer_binding_size": l.max_storage_buffer_binding_size,
                "max_texture_dimension_2d": l.max_texture_dimension_2d,
                "max_compute_invocations_per_workgroup": l.max_compute_invocations_per_workgroup,
                "max_compute_workgroup_storage_size": l.max_compute_workgroup_storage_size,
                "max_compute_workgroups_per_dimension": l.max_compute_workgroups_per_dimension,
            },
        })
    }

    /// The compute pipeline for `entry` in `wgsl`, built once per process
    /// (`key` names the module, e.g. "gpu/image"; layouts come from the
    /// shader).
    pub fn compute_pipeline(&self, key: &'static str, wgsl: &'static str, entry: &'static str) -> Arc<wgpu::ComputePipeline> {
        let mut cache = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry((key, entry))
            .or_insert_with(|| {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(key), source: wgpu::ShaderSource::Wgsl(wgsl.into()) });
                Arc::new(self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: None,
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                }))
            })
            .clone()
    }

    /// A staging buffer to read back at least `size` bytes through: a kept
    /// one when one is big enough (the smallest such), else a new one
    /// (rounded up to a MiB, so the next frame's fits).
    pub(crate) fn take_staging(&self, size: u64) -> wgpu::Buffer {
        let mut pool = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = (0..pool.len()).filter(|&i| pool[i].size() >= size).min_by_key(|&i| pool[i].size()) {
            return pool.swap_remove(i);
        }
        drop(pool);
        let size = size.max(16).next_multiple_of(1 << 20);
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("read back"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    /// Keep an (unmapped) staging buffer for later: the four biggest, up to
    /// 512 MiB in all.
    pub(crate) fn give_staging(&self, buffer: wgpu::Buffer) {
        let mut pool = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        pool.push(buffer);
        pool.sort_by_key(|b| std::cmp::Reverse(b.size()));
        pool.truncate(4);
        while pool.iter().map(wgpu::Buffer::size).sum::<u64>() > 512 << 20 && pool.len() > 1 {
            pool.remove(0);
        }
    }

    /// Wait until the GPU has done everything submitted.
    pub fn wait(&self) -> anyhow::Result<()> {
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| anyhow::anyhow!("GPU wait failed: {e}"))?;
        Ok(())
    }

    /// Run `f`, catching the validation and out-of-memory errors it
    /// causes as an `Err` (recorded with [`report_error`]) instead of
    /// wgpu's panic, so the caller can fall back to the CPU.
    pub fn scoped<T>(&self, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let memory = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let out = f();
        let errors = [pollster::block_on(memory.pop()), pollster::block_on(validation.pop())];
        if let Some(e) = errors.into_iter().flatten().next() {
            report_error(&e, false);
            anyhow::bail!("GPU error: {e}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reasons, in their order: not built beats the variable, which
    /// beats Preferences, which beats a lost device.
    #[test]
    fn the_off_reasons_take_their_turn() {
        assert_eq!(off_reason(false, Some("0"), false, Some("x")).as_deref(), Some("not built with GPU support"));
        assert_eq!(off_reason(true, Some("0"), false, Some("x")).as_deref(), Some("ARCLIGHT_GPU=0"));
        assert_eq!(off_reason(true, None, false, Some("x")).as_deref(), Some("turned off in Preferences"));
        assert_eq!(off_reason(true, None, true, Some("device lost")).as_deref(), Some("GPU error: device lost, using the CPU"));
        assert_eq!(off_reason(true, None, true, None), None);
        assert_eq!(Status::Cpu { reason: "ARCLIGHT_GPU=0".into() }.line(), "Using the CPU: ARCLIGHT_GPU=0");
        let on = Status::Active { adapter: "GPU X".into(), backend: "Vulkan".into(), unified: false };
        assert_eq!((on.to_string(), on.is_active()), ("Using GPU X (Vulkan)".into(), true));
        assert_eq!(serde_json::to_value(&on).unwrap()["state"], "active");
    }

    /// The switch's default comes from settings.json's "gpu" block.
    #[test]
    fn the_setting_is_read_from_settings_json() {
        assert!(setting_in("") && setting_in("{}") && setting_in(r#"{"gpu": {}}"#) && setting_in(r#"{"gpu": {"enabled": true}}"#));
        assert!(!setting_in(r#"{"interface": {}, "gpu": {"enabled": false}}"#));
    }

    /// `cpu_only` turns the GPU off for its call on this thread only, and
    /// nests.
    #[test]
    fn cpu_only_is_for_one_call_on_one_thread() {
        let on_here = || CPU_ONLY.with(Cell::get) == 0;
        cpu_only(|| {
            assert!(get().is_none() && !on_here());
            cpu_only(|| assert!(!on_here()));
            assert!(!on_here());
            assert!(std::thread::spawn(move || CPU_ONLY.with(Cell::get) == 0).join().unwrap(), "another thread is unaffected");
        });
        assert!(on_here());
        let _ = std::panic::catch_unwind(|| cpu_only(|| panic!("left by a panic")));
        assert!(on_here(), "a panic leaves it too");
    }

    /// Every WGSL shader in the crate translates to Metal's MSL (what
    /// wgpu does on macOS at pipeline creation), so a shader Metal would
    /// reject fails here first. Metal's own compiler isn't run.
    #[test]
    fn every_wgsl_shader_translates_to_msl() {
        fn wgsl_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    wgsl_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "wgsl") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        wgsl_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(!files.is_empty(), "no shaders found");
        for path in files {
            let src = std::fs::read_to_string(&path).unwrap();
            let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}: {}", path.display(), e.emit_to_string(&src)));
            let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
            // macOS 11, the oldest the DMG supports, has MSL 2.3.
            let options = naga::back::msl::Options { lang_version: (2, 3), fake_missing_bindings: true, ..Default::default() };
            naga::back::msl::write_string(&module, &info, &options, &naga::back::msl::PipelineOptions::default()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }
}
