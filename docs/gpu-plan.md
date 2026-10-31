# GPU plan: wgpu compute and rendering

Status: **approved** by the user 2026-10-30 (answers to Q1–Q5 at the
end; the Supervisor's additions marked "(Supervisor)").
Phase 12: **0.12.x** on branch **`wip-ns-gpu`** (user-approved
2026-10-30). Written by Dev2 with Dev1's prep notes folded in (where we
differ, it says so). The work goes one milestone at a time per owner, as
in `splat-plan.md`; Dev1 and Dev2 run in parallel and never edit the same
file at once (see "Owners and files").

## Goal

Make the slow parts fast on the GPU, everywhere Arclight renders (GUI,
CLI, MCP, Python), without giving up the CPU renderer. The CPU path stays
the reference and the fallback, always correct; the GPU path matches it
within stated tolerances, tested. The user can turn GPU acceleration off
in Preferences (or `ARCLIGHT_GPU=0`) and see which adapter is in use or
why it isn't.

**Out of scope here:** particle *simulation* on the GPU, shapes / roto
coverage, Light 2D, zmerge, plugins on the GPU, tiled / ROI processing
(its own TODO item), Windows / DX12 (on hold).

## Where the time goes (measured)

Release build (`lto = "thin"`), this machine: 28 cores (rayon 28
threads), RTX 5070, driver 615.78, Vulkan. Median of 3 runs. Scratch
harness outside the repo: `../.verify/gpubench` (`gpubench 2d|1080p|4K`
for the CPU, `wgpubench` for wgpu); it calls `ops::eval` directly for the
2D ops and `Evaluator` with the upstream nodes already cached for the 3D
ones, so file reads and simulation stay out of the numbers unless the
table says otherwise.

### 3D (Scanline Render, splats, particles), CPU

Scene: a textured card, a 40×40 textured floor, a 128×64 sphere (animated,
specular), a cube, `crate.gltf`; ambient + directional + point + spot
lights. AA = `samples` 3 (3×3 = 9 samples a pixel, the default).

| Case | 1080p | 4K |
|------|------:|---:|
| 4 objects, 3 lights, AA 3, no shadows | 280 ms | 992 ms |
| same, `samples` 1 | 69 ms | 141 ms |
| + passes depth, P, N, motion, id | 381 ms | 1.26 s |
| + shadows (sun, point, spot) | 764 ms | 2.31 s |
| motion blur 8, no shadows | **3.48 s** | **12.3 s** |
| motion blur 8 + shadows | **11.2 s** | **28.5 s** |
| splats: hornedlizard 786 k, cropped, AA 3 | 78 ms | 154 ms |
| splats: same, `View` cache hit (blend only) | 28 ms | 94 ms |
| splats: `samples` 1 | 81 ms | 151 ms |
| particles ~50 k sprites | 60 ms | 85 ms |
| particles ~500 k sprites | 155 ms | 297 ms |
| particles ~50 k, motion blur 8 | 1.40 s | 3.98 s |
| particles ~500 k, motion blur 8 | **5.45 s** | **11.0 s** |
| particle sim to frame 100, ~500 k (cold, not render) | 3.6 s | 3.1 s |

Readings:
- The scanline renderer costs ~4× per AA step (9 samples shaded each),
  shadows add ~2.5×, and **motion blur multiplies everything by its sample
  count, shadows rebuilt per sample**: 11–28 s a frame. That is where the
  user waits.
- Splats are already fast on the CPU (S5's binning and LRU), and `samples`
  doesn't change their cost: ~50 ms of 78 is projection / binning / sort
  (cached per view), the rest blending.
- Particle sprite rendering is cheap; particle *motion blur* is the same
  "render N times" cost as meshes. Simulation (3 s cold) is CPU-serial
  frame stepping and isn't touched by this plan.

### 2D ops (`src/ops.rs`), CPU

Noise RGBA f32 input, full frame.

| Op | 1080p | 4K |
|----|------:|---:|
| `Image` clone (memcpy baseline, 33 / 133 MB) | 4.7 ms | 18 ms |
| grade (gamma, multiply, offset) | 5.0 ms | 27 ms |
| grade + mask, mix 0.5 | 17 ms | 58 ms |
| saturation, merge over / screen | 4–5 ms | 23–26 ms |
| keyer (screen pick, softness) | 38 ms | 212 ms |
| keyer shrink / grow 3 | 23 ms | 133 ms |
| blur r 8 × 3 (default) | 34 ms | 209 ms |
| blur r 32 × 3 | 67 ms | 645 ms |
| blur r 100 × 3 | **210 ms** | **2.08 s** |
| transform rotate 10 (bilinear) | 8 ms | 28 ms |
| transform rotate, motion blur 8 | 71 ms | 237 ms |
| corner pin | 12 ms | 42 ms |
| lens distort, undistort / distort | 9 / 25 ms | 31 / 96 ms |
| resize ×0.5 / ×2 | 2.5 / 17 ms | 10 / 61 ms |
| light 2D spot | 6 ms | 31 ms |
| `to_rgba8` (8-bit encode for PNG) | 51 ms | 192 ms |

Readings:
- **Pointwise ops run at memcpy speed** (memory-bound). Alone, one gains
  nothing from the GPU once the upload and readback are paid (below).
  They gain only as part of a chain kept on the GPU.
- **Blur is O(radius) per pixel** (`box_pass_h/v` re-sum the whole
  window): 2 s at 4K, r 100. A running sum makes the CPU O(1) per pixel
  too, which the CPU fallback should get anyway (G2).
- The keyer, motion-blurred warps and lens distort are the other 2D ops
  worth porting for themselves.
- There is **no Defocus op** today (the task listed one). A GPU-first,
  depth-aware Defocus is a natural later op (open question Q4).

### wgpu on this machine (`wgpubench`)

Storage buffers of `vec4<f32>` laid out exactly like `Image.pixels`.

| | 1080p | 4K |
|---|------:|---:|
| Instance + adapter + device (first use) | 80–260 ms total | |
| WGSL module + 4 compute pipelines | 31 ms | |
| upload (`write_buffer`, wait) | 3.4 ms (9.8 GB/s) | 21 ms (6.3 GB/s) |
| readback (copy, map, to `Vec`) | 3.7 ms (9.1 GB/s) | 21 ms (6.4 GB/s) |
| grade kernel (`pow`) | 0.12 ms | 0.48 ms |
| blur r 8 × 3, naive O(r) | 0.73 ms | 3.4 ms |
| blur r 32 × 3, naive O(r) | 1.9 ms | 6.9 ms |
| blur r 100 × 3, naive O(r) | 4.7 ms | 19 ms |
| warp rotate 10, bilinear by hand | 0.13 ms | 0.47 ms |
| grade GPU vs CPU, max relative error | 9.4e-7 | 1.6e-6 |

Adapter limits: storage binding 2047 MiB, buffer 4095 MiB, 2D texture
32768. (wgpu's *default* `Limits` cap a storage binding at 128 MiB: a 4K
RGBA f32 frame is 126.6 MiB and just fits; 6K / 8K don't. We request the
adapter's limits.)

So: **a round trip costs ~7 ms at 1080p and ~42 ms at 4K**, about one
or two pointwise ops on the CPU. Kernels are 50–200× faster than the
CPU. Residency across a chain is what makes 2D pay off: e.g. grade →
transform → blur 8 → merge → grade at 4K is ~314 ms on the CPU, ~70 ms on
the GPU kept resident (2 uploads, 1 readback), and ~220 ms with a round
trip per node.

### Ranked by what the GPU would gain

GPU figures for 2D are measured kernels + transfers; for 3D they are
estimates (a render pass per AA / time sample, one readback), to be
checked in G4–G6.

| Rank | Path | CPU now (4K) | GPU (est.) | Why |
|------|------|-------------:|-----------:|-----|
| 1 | Scanline Render with motion blur (± shadows) | 12–28 s | < 0.3 s | N full renders + shadow maps per sample |
| 2 | Particles with motion blur | 4–11 s | < 0.3 s | same machinery |
| 3 | Scanline Render, shadows / passes / AA 3 | 1–2.3 s | ~50 ms | per-sample shading on the CPU, f64 |
| 4 | 3D view (Lit, shadows) while orbiting | ~70–760 ms at 1080p | < 16 ms | same renderer via `render_preview`: interactive, not just faster |
| 5 | Big blurs | 0.2–2 s | 25–60 ms | O(r) CPU; also fixed on the CPU in G2 |
| 6 | Keyer, motion-blurred warps, lens distort, resize up | 60–240 ms | 25–45 ms | compute-bound gathers |
| 7 | Splats | 78–154 ms | ~10–30 ms | already good; GPU makes the 3D view's full render interactive |
| 8 | Pointwise chains (grade, merge, mask mix…) | 25–60 ms each | ~1 ms each | only when resident |
| 9 | Viewer display (f32 → sRGB 8-bit texture each change) | ~50–190 ms (`to_rgba8`; the viewer's own parallel path is lighter, unmeasured) | ~0 | zero-copy display (G7) |

**Consequence for the split:** the biggest wins are in the 3D renderer,
so it is not a side task. The suggested split stands: Dev1 the GPU core
and 2D ops, Dev2 the 3D renderer and splats. 3D needs only the core
(G1), not the 2D image work, so both run in parallel after G1.

## What exists to build on

- `Image` (`src/image.rs`): `pixels: Vec<[f32; 4]>` linear premultiplied
  over a data window (`x0, y0, width, height`) inside `format`; outside
  the window reads as transparent. Extra `planes` (f32 each, same window),
  `scene: Option<Arc<Scene>>` on 3D outputs. **214 direct `.pixels`
  uses** in `src/` (55 in `ops.rs`, 34 in `render3d.rs`), so `Image`
  must stay CPU-complete: GPU residency lives beside it, not inside it
  (D4).
- `ops::eval` → `eval_op` is the single dispatch, with generic wrappers:
  Merge swap, `_motion` averaging (transform / corner pin / tracker),
  `planes_source` carry, `mask_mix`. `Evaluator.cache: HashMap<String,
  (u64, Arc<Image>)>`, keyed by params and inputs, never by pixels.
- `render3d`: `render_cell` / `render_blurred` / `render_preview(_blurred)`
  are the entry points; everything goes through `draw_passes` (row-binned
  triangles, n×n samples per pixel, all fragments per sample sorted and
  blended front to back, splats merged per sample). `shadow.rs` already
  works with shadow *maps* (depth maps per light, cube maps for point
  lights), `mip.rs` with an analytic per-sample footprint; both port to
  GPU idioms directly.
- eframe 0.37 already runs on **wgpu 30.0.1** (`egui-wgpu`);
  `gui/main.rs` reads `cc.wgpu_render_state` to put the GPU in crash
  reports (`crash::set_gpu`).
- Plugins run in helper processes and get pixels through shared memory
  (`plugin_host.rs`, 0.11.2).

## Decisions (proposed)

- **D1, module and dependency.** A `gpu` module (`src/gpu/`) in the
  library crate: `wgpu = "30"` (same major as eframe's, or the types
  don't match) and `pollster` as direct dependencies behind a cargo
  feature **`gpu`**, **on by default**, independent of `gui` (the CLI
  renders too). `python/Cargo.toml` (which takes `default-features =
  false`) adds `features = ["gpu"]`. Built without the feature, `gpu`
  compiles to a stub whose status is "not built with GPU support", so
  callers don't need `cfg`s.
- **D2, the device.** Created **lazily** on first use (the first
  GPU-capable node, never at start-up, so `arclight --help`, the
  `list_ops` MCP call and plugin helper processes never touch a driver),
  **headless** (no surface), backends Vulkan / Metal (wgpu's PRIMARY; not
  GL: its compute support is too thin), high-performance adapter, the
  adapter's own limits. One `Gpu { device, queue, info, limits,
  pipelines }` per process behind a `OnceLock`, `Send + Sync`, shared by
  every evaluator thread (GUI worker, script jobs, MCP, Python). Device
  creation measured at 80–260 ms, once.
- **D3, the GUI shares one device, the engine's.** *Differs from Dev1's
  notes*, which take the device from egui's `wgpu_render_state`. Instead
  the engine creates the device (as in the CLI) and, from G7 on, hands it
  to eframe with `WgpuSetup::Existing`. Why: egui's default device asks
  for default limits (128 MiB storage, 8192 textures) and may come up on
  GL; one creation path for GUI / CLI / Python keeps tests
  representative; and the engine's device must exist without a window
  anyway. Until G7 the GUI simply has two devices on the same adapter
  (egui's and the engine's), which is fine: nothing is shared before
  zero-copy display. Dev1 and I agree on the end state (one device, the
  viewer drawing GPU-resident results without readback).
- **D4, images on the GPU.** A `GpuImage { format, window, rgba:
  wgpu::Buffer, planes: Vec<(String, wgpu::Buffer)>, scene }` mirroring
  `Image` exactly: storage buffers in `Image`'s layout (row-major
  `vec4<f32>`, the data window only), so upload / readback are single
  copies with no row padding (texture copies need 256-byte rows; data
  windows have any width). Bilinear sampling and "transparent outside the
  window" are done in WGSL by hand (rgba32float isn't filterable without
  an optional feature, and the window rule needs custom code anyway; Dev1
  noted the same). Windows (`IRect` maths: `pointwise_window`,
  `moved_window`, `bent_window`) stay on the CPU as metadata.
- **D5, residency in the Evaluator, not in `Image`.** The cache entry
  becomes `Cpu(Arc<Image>)` or `Gpu(Arc<GpuImage>, OnceLock<Arc<Image>>)`
  (readback on first CPU need, then kept). A node runs on the GPU when
  `gpu::ops::eval(op, params, inputs)` supports that op *and* those
  params (it returns `None` otherwise: e.g. an unported blend mode), with
  inputs uploaded if they are CPU-side; otherwise the CPU `ops::eval`
  runs on read-back inputs. `Evaluator::evaluate` still returns
  `Arc<Image>` (reads back the target), so every caller (viewer, writers,
  MCP, Python, tracker, plugins) is unchanged. A GPU memory budget
  (`gpu.cache_mb`, default 2048) evicts least-recently-used GPU entries by
  reading them back to CPU entries (cache semantics unchanged).
- **D6, the 3D renderer renders on the GPU and reads back once.**
  *(Revised in G4, 2026-10-31, accepted by the Supervisor: a compute
  rasterizer instead of a render pipeline with fragment lists.)*
  `render3d::draw_passes` tries `render3d/gpu` first, so Scanline Render,
  the 3D view's `render_preview`, particles' render and Camera Tracker
  point picking all get it without changes elsewhere. It mirrors the CPU
  renderer's algorithm sample for sample, in compute: triangles set up on
  the CPU in f64 as now (`triangles()`: clipping, projection), binned into
  16 × 16 tiles; one GPU thread a pixel gathers each sample's covering
  triangles, keeps the nearest 16 sorted by (depth, triangle), and blends
  them front to back with the CPU's rules (holdouts, one hit where two
  triangles of a surface meet, the nearest half-opaque surface for the
  passes), shading each hit (mipmapped texture, every light type). The
  n×n sample grid is the CPU's; a dispatch takes one row of samples, so
  no dispatch runs long. Why not hardware raster: the CPU blends *every*
  fragment of a sample in depth order, which a depth buffer can't, and
  per-pixel fragment lists cost 4K × 9 × K memory; this way results stay
  deterministic, close to the CPU, and need no atomics. Shared edges are
  evaluated in a fixed vertex order, so no sample falls between two
  triangles. Where a sample has more than 16 surfaces and more than 1e-4
  of the light still comes through them, the frame runs again keeping 64,
  and pixels past that are rendered on the CPU and put in (per pixel, not
  per frame). P, N, uv, motion and id come from the surface each pixel
  kept, worked out on the CPU by the CPU's code. Textures are uploaded
  once per set (kept while the same `Arc`s) and their mipmaps made on the
  GPU; output buffers are kept between renders.
- **D7, the switch and fallback.** GPU used when all hold: built with
  `gpu`; `ARCLIGHT_GPU` isn't `0` (`ARCLIGHT_GPU=0` forces the CPU, and
  overrides the preference); Preferences ▸ Performance ▸ "GPU
  acceleration" is on (`settings.json` key `"gpu": {"enabled": true}`,
  default on); an adapter and device were created. Any GPU error on a
  node (out of memory, validation, device lost) makes **that node** run
  on the CPU and is recorded; a lost device turns the GPU off for the
  rest of the process. Results never fail because of the GPU.
- **D8, precision.** f32 everywhere on the GPU (the CPU stores f32
  pixels; `render3d` does geometry in f64). Tolerances, each tested GPU
  vs CPU on the same inputs:
  - pointwise ops: relative ≤ 1e-5, absolute ≤ 1e-6 (measured 1.6e-6
    worst for `pow`);
  - filters and warps (blur, transform, corner pin, lens distort,
    resize, keyer): absolute ≤ 1e-4 per channel for values in 0..1
    (summation order, f32 weights);
  - 3D: identical away from edges on hand-made scenes (≤ 1e-4); on full
    scenes mean absolute error ≤ 1e-3 and ≤ 0.1 % of pixels over 1e-2
    (coverage at triangle edges and shadow-map texels can flip between
    f32 and f64), depth / P relative ≤ 1e-4;
  - splats: same statistics as 3D (sort ties broken by index, so stable).
  GPU results must be **deterministic run to run** (no float atomics in
  sums), so cache keys and renders stay reproducible. Cache keys mix in
  the backend in use (CPU / GPU), so switching the preference never mixes
  results.
- **D9, tests.** `gpu::test_device()` returns `Option<Arc<Gpu>>`; GPU
  tests start with `let Some(gpu) = gpu::test_device() else {
  eprintln!("skipped: no GPU adapter"); return; };`, so `cargo test`
  passes on machines without an adapter. Each ported op gets a CPU-vs-GPU
  test at odd sizes and offset data windows (not just full frames).
  Optional for CI (Actions, when the repo is public): Mesa's lavapipe
  (software Vulkan) runs the GPU tests without a GPU.
- **D10, platforms.**
  - *Linux tarball:* wgpu dlopens `libvulkan.so.1` at run time; no new
    link dependency. A machine without a Vulkan driver gets no adapter
    and runs on the CPU (README: "GPU acceleration needs a Vulkan driver:
    NVIDIA, or Mesa's `vulkan-radeon` / `vulkan-intel`").
  - *macOS universal DMG:* Metal on both slices; see "macOS and Apple
    Silicon (Metal)" below.
  - *Plugins:* unchanged. Helper processes never create a device (lazy
    init, and they only call the plugin's render); a plugin's inputs are
    read back like any CPU op's; a plugin crash stays in its helper.
  - *Crash reports:* `gpu` calls `crash::set_gpu` with the engine's
    adapter, so CLI / Python crashes name the GPU too.
- **D11, threads and the UI.** Long submissions (a 4K motion-blurred
  render) are split per AA / time sample so the GPU stays responsive for
  egui; the GUI's worker, script jobs and MCP renders keep their own
  threads and `Progress` / cancel (checked between submissions).

## macOS and Apple Silicon (Metal)

The user runs the DMG on an M4 Pro and on an Intel Mac. We can't run
Metal on this Linux machine, so everything below is from wgpu-hal
30.0.1's Metal backend source (`wgpu-hal-30.0.1/src/metal/`), plus
checks the user runs on the Macs.

- **Backend.** wgpu's PRIMARY backends on macOS are Metal only (no
  MoltenVK). The same code and WGSL shaders run there: naga translates
  WGSL to MSL at pipeline creation. On a MacBook Pro with two GPUs (Intel
  + AMD), the high-performance request picks the AMD one.
- **Which Metal we rely on: plain Metal, no Metal 4.** wgpu 30 drives
  Metal through the classic API (`MTLDevice`, `MTLCommandQueue`, compute
  and render command encoders). It checks `MTLGPUFamily::Metal4` only to
  turn on optional features (shader barycentrics, memoryless storage),
  which this plan doesn't use. It compiles MSL at the newest language
  version the OS offers (4.0 on macOS 26, 3.x before, down to 2.x), but
  naga's output for our kernels (storage buffers, uniforms, workgroup
  memory, barriers, plain f32 maths) needs no Metal 3 or Metal 4 API.
  The floor is what Arclight already ships with: **macOS 11**
  (`MACOSX_DEPLOYMENT_TARGET=11.0`), the Mac2 GPU family on Intel Macs
  and Apple7+ on Apple Silicon (M1 and later; the M4 Pro is Apple9). If a
  later milestone wanted Metal 3 or Metal 4 features (mesh shaders,
  ray tracing), that would be a new decision; nothing here needs them.
- **Unified memory.** On Apple Silicon the CPU and GPU share RAM (the
  M4 Pro has ~273 GB/s), and wgpu reports the adapter as
  `IntegratedGpu`. wgpu-hal still puts ordinary buffers in
  `StorageModePrivate` and only mappable ones in `StorageModeShared`, so
  by default an upload is memcpy → staging → blit, in the same RAM. G1
  adds a unified-memory path: when the adapter is integrated and offers
  `MAPPABLE_PRIMARY_BUFFERS` (Metal does), `GpuImage` buffers are created
  `STORAGE | MAP_READ | MAP_WRITE`, i.e. shared. Then upload and readback
  are one memcpy each with no copy pass (est. 4K under ~5 ms each way,
  against 21 ms over PCIe here). On discrete GPUs (NVIDIA here, AMD in
  Intel Macs) the staging path stays, because mappable storage buffers
  live in slow host memory there. Both paths sit behind
  `upload` / `read_back`, so ops don't see the difference; the self-test
  reports which one ran.
- **f32 formats and filtering.** Our images are storage *buffers* (D4),
  so texture-format limits don't touch the 2D ops. For the 3D renderer's
  textures: wgpu-hal enables `FLOAT32_FILTERABLE` on every macOS Mac
  (hardware filtering of `rgba32float`), and `rgba32float` storage
  textures have full read / write there (iOS has less). G4 requests
  `FLOAT32_FILTERABLE` when present and filters by hand in WGSL when not,
  so no adapter is required to have it.
- **Workgroup limits.** Metal on every Mac allows up to 1024 threads per
  threadgroup and 32 KiB of threadgroup memory (16 KiB on the oldest
  families); wgpu's portable defaults are 256 invocations and 16 KiB. Our
  kernels use **16×16 = 256** (8×8 for heavy ones) and **at most 16 KiB**
  of workgroup memory (the blur's row tiles, the splat sort), so they run
  unchanged on Metal, Vulkan and any adapter that meets wgpu's defaults.
  Apple GPUs run 32-wide SIMD groups, as NVIDIA does, so 256 is a good
  size on both. Dispatches stay under 65 535 workgroups a dimension
  (a 1D dispatch of 6 M splats at 256 per group is ~23 k).
- **Buffer size limits.** On Metal `max_storage_buffer_binding_size` is
  the device's `maxBufferLength`: a large share of RAM on Apple Silicon,
  at least 1 GiB on older Intel Macs. An 8K RGBA f32 frame is ~506 MiB,
  so it fits everywhere we ship; a buffer over the limit runs on the CPU
  (D7).
- **Precision on Metal.** wgpu 30 doesn't set Metal's math mode, so
  Metal's default **fast math** applies (approximate `pow` / `exp` /
  divides; it may assume no NaN or infinity). D8's tolerances must hold
  on Metal too, and the self-test checks them on the user's Macs. Kernels
  must not rely on NaN / inf behaviour: HDR plates can hold inf, so the
  ops that care (divide by alpha in unpremult, `pow` in grade) clamp or
  guard as the CPU code does. If a tolerance fails on Metal, the fix is
  per kernel (rewrite the expression, or the op falls back to the CPU on
  Metal); it never loosens the test.
- **Building the universal DMG.** Unchanged: `packaging/macos/build-dmg.sh`
  builds `aarch64-apple-darwin` and `x86_64-apple-darwin` and joins them
  with `lipo`. wgpu's Metal backend is already in both slices (eframe uses
  it for the window), so the `gpu` feature adds code but no new
  framework, library or entitlement. Metal shaders are compiled at run
  time from WGSL, so nothing new goes into the bundle.
- **Testing what we can't run here.**
  1. *On Linux, in `cargo test`:* every WGSL module is translated to MSL
     with naga (`naga` as a dev-dependency with `wgsl-in` and `msl-out`,
     the version wgpu 30 uses), so a shader Metal would reject fails here
     first. (Metal's own compiler isn't run; that's step 3.)
  2. *On Linux, the wgpu tests:* run on Vulkan (and lavapipe in CI);
     they check the logic, not Metal.
  3. *On the Mac, `cargo test --release`:* the user already builds the
     DMG on a Mac, so the same checkout runs the GPU tests on Metal
     (they don't skip there).
  4. *On the Mac, the self-test from the installed app:*
     `/Applications/ArclightVFX.app/Contents/MacOS/arclight gpu --check`
     (and Help ▸ GPU self-test in the GUI, which shows the same table).
     See G1.
- **Hand checks for the user** (go into TODO.md at each release):
  - M4 Pro: `arclight gpu` names "Apple M4 Pro (Metal)"; `arclight gpu
    --check` passes every row and shows "unified memory"; Preferences ▸
    Performance shows the same adapter; the toggle off / on changes the
    status and the viewer re-renders.
  - Intel Mac: same, with its GPU named (AMD if it has one, else the
    Intel GPU) and "discrete transfers" (or "unified" on Intel graphics);
    if no Metal device comes up, the status says why and everything still
    renders on the CPU.
  - Both: the measured 3D scene (motion blur 8 + shadows) and a
    blur-heavy 2D chain in the GUI, timed with the GPU on and off;
    `ARCLIGHT_GPU=0 open -a ArclightVFX` (or the CLI with
    `ARCLIGHT_GPU=0`) shows "Using the CPU: ARCLIGHT_GPU=0".

## Milestones

Each ships alone as 0.12.x versions, `cargo test` and `cargo clippy
--all-targets` clean, reported to the Supervisor. Owners in brackets;
"Owners and files" below lists who edits what.

### G1: The GPU core and the switch [Dev1 core, Dev2 Preferences]

The base everything else needs; ships with no op on the GPU yet, but the
switch, the status and the tests' skip work end to end.

- [Dev1] `Cargo.toml` feature `gpu` (default) + `wgpu`, `pollster`,
  `bytemuck`; `python/Cargo.toml` `features = ["gpu"]`.
- [Dev1] `src/gpu/mod.rs`: lazy `gpu::get() -> Option<Arc<Gpu>>`,
  `gpu::status() -> Status` (`Active { adapter, backend }` or
  `Cpu { reason }`: "turned off in Preferences", "ARCLIGHT_GPU=0", "no
  GPU adapter (Vulkan / Metal)", "not built with GPU support", "GPU
  error: …, using the CPU"), `gpu::set_enabled(bool)` (atomic; read by
  every evaluation, so it applies **without a restart**: the next render
  uses the new setting), `gpu::test_device()`, `crash::set_gpu`.
- [Dev1] `src/gpu/image.rs`: `GpuImage`, `upload(&Image)` /
  `read_back() -> Image` (planes included), the pipeline cache
  (WGSL modules built once per process), a round-trip test.
- [Dev1] `src/gpu/image.rs` also has the unified-memory path (shared,
  mappable buffers on integrated adapters such as Apple Silicon; see the
  macOS section), chosen at device creation.
- [Dev1] CLI `arclight gpu` (prints the status, adapter, backend, limits
  and transfer path), MCP tool `gpu_info`, Python `arclt.gpu_info()`.
- [Dev1] **GPU self-test, `arclight gpu --check`** (`src/gpu/check.rs`):
  runs every GPU-ported op and render case on fixed generated inputs
  (1080p and 4K, odd sizes, offset data windows) on both the CPU and the
  GPU, and prints one row each: case, size, max absolute error, max
  relative error, the tolerance (D8), pass / FAIL, CPU ms, GPU ms
  (kernel only and with transfers), speedup. Then a summary line with the
  adapter and the transfer path. Exit status 1 if any row fails; `--json`
  for pasting into an issue. Each later milestone adds its rows: Dev1's
  2D cases in `src/gpu/check.rs`, Dev2's 3D and splat cases in
  `src/render3d/gpu/check.rs` through a `render3d::gpu_checks()` hook that
  G1 adds as an empty stub (so neither edits the other's file). In G1 it
  checks upload / read-back and one trivial kernel. MCP / Python
  `gpu_check`; Help ▸ GPU self-test in the GUI shows the table (Dev2,
  `src/gui/main.rs` menu entry + a small window).
- [Dev1] Test: every WGSL module translates to MSL with naga (macOS
  section).
- [Dev1] **`--cpu`** (Q2) on `arclight render` and the other CLI
  commands that render, a `cpu` argument on MCP / Python renders: that
  render runs on the CPU only, for exact reproducibility (as
  `ARCLIGHT_GPU=0`, for one call).
- [Dev2] (Supervisor) The crash dialog: when the report names a GPU
  adapter and the crash looks like it was in the driver (or GPU
  acceleration was on), suggest turning it off, with a button that
  does so (`src/gui/crash_dialog.rs`).
- [Dev2] **Preferences entry (user request):** `config.rs` `GpuPrefs {
  enabled: bool }` under `"gpu"` in `settings.json` (default on; kept by
  older versions through `Settings.other`); Edit ▸ Preferences ▸
  **Performance** ▸ "GPU acceleration" checkbox, applied at once through
  `gpu::set_enabled` and clearing the viewer's frame cache so the change
  shows; under it the status line ("Using NVIDIA GeForce RTX 5070
  (Vulkan)" / "Using the CPU: <reason>"; with `ARCLIGHT_GPU=0` set, the
  checkbox greyed with that reason). The **About box** shows the same
  line. `docs/help.md` Preferences section.
- Tests: status reasons (env override beats the preference), the
  settings key round trip, upload / read-back identity (skips without an
  adapter).

### G2: Images on the GPU, pointwise ops, blur [Dev1]

- Evaluator residency (D5): `Cpu` / `Gpu` cache entries, read-back on
  demand, `gpu.cache_mb` budget with LRU eviction to CPU.
- `gpu::ops::eval` for: grade, exposure, saturation, invert, clamp,
  premult, unpremult, channel select, shuffle, dissolve, keymix, switch,
  constant, checkerboard, merge (all blend modes but zmerge), and the
  generic `mask_mix` and plane carry, as one pointwise uber-shader with
  an op enum (Dev1's proposal).
- Blur on the GPU (separable, shared-memory row tiles; frame-edge repeat
  as on the CPU) and **the CPU blur fixed to a running sum** (O(1) per
  pixel; f64 accumulator per row so it doesn't drift) so the fallback is
  fast too.
- Tests: every ported op GPU vs CPU (D8), at odd sizes and offset
  windows, with masks and mix; a mixed chain (GPU → CPU-only op → GPU)
  gives the CPU result.

### G3: Warps, filters and the keyer [Dev1]

- transform / corner pin / tracker `warp` with `sample_warped`'s
  footprint taps (up to 16×16), `_motion` sample averaging, lens distort
  (both modes), resize / `resample` (with `hard_edges`), `carry_planes`
  for warps (depth sampled the same way).
- keyer: pointwise part plus `morph` as separable min / max passes;
  `estimate_screen` stays on the CPU (a reduction over a read-back,
  only when Screen is unset).
- Tests as G2.

### G4: Scanline Render on the GPU [Dev2]

- `src/render3d/gpu/`: the compute rasterizer of D6 (`raster.wgsl`),
  mipmaps on the GPU (`mip.wgsl`), cards and meshes, perspective and
  ortho cameras, unlit / Lambert / Blinn shading and every light type as
  in `shade.rs`, holdouts, the 3D view's Solid shading, jittered cells,
  every pass (depth, P, N, uv, motion, id, mattes, light parts, per
  light, shadow). See-through stacks as in D6 (16, then 64, then the CPU
  per pixel).
- `draw_passes` takes the GPU when `gpu::get()` is `Some` and the frame
  has no shadow-casting light (G5) and no splats (G6), at most 32 objects
  with mattes and 16 light nodes with the lights pass. Particle sprites
  are drawn after it on the CPU as now (particle objects are meshes, so
  on the GPU). Motion blur works (each time a GPU render), not yet fast
  (G5).
- Measured (RTX 5070, warm: the camera moving between renders, scene and
  textures kept, as in the GUI): 4K AA 3×3, the 4-object 3-light scene,
  **83 ms** against the CPU's 1004 ms (12×; the plan's ~50 ms needs the
  read-back gone: ~15 ms GPU, ~35 ms read-back, ~15 ms CPU finishing);
  1080p one sample (the 3D view) **19 ms** against 44 ms; 4K with P, N,
  motion and id 382 ms against 1233 ms (the CPU works those passes out
  per pixel). Smoke of 30–60 see-through cards: all on the GPU, 1080p AA
  3×3 63–84 ms against 222–445 ms; 200 cards at 10 %: 8 % of pixels on the
  CPU, 1.0 s against 1.8 s.
- Tests: hand-made scenes exact away from edges, the M-series test scenes
  statistically (D8), passes compared channel by channel.

### G5: Shadows, motion blur, particles [Dev2]

- **Done (shadows + motion blur, one version):** shadow maps on the GPU,
  as `shadow::plan` places them (the CPU draws the same ones): a
  depth-only `shadow_map` entry in `raster.wgsl` draws every map (sun,
  spot, point cubes, area-light cubes), and the light loop looks them up
  as `Shadows::visible` does (offset off the surface, cube face, bilinear
  compare, PCF, the blocker search for a light's radius), with the shadow
  and per-light passes. Casters that let light through (opacity below 1,
  a texture with partial alpha or a window short of its frame) keep the
  frame on the CPU for now: their layered shadows are a follow-up.
- Motion blur and the jittered shutter added up on the GPU
  (`gpu::blurred`): every (cell, time) render into one frame's buffers,
  each cell's pixels packed onto threads, one read-back; the triangles
  and shadow maps set up per time. Frames with sprites, points, splats or
  a background with planes take the per-time path as before.
- Measured, 4K, the measured scene, AA 3×3 (CPU / GPU): shadows 2455 /
  143 ms (17×); motion blur 8 12.5 s / 593 ms (21×); motion blur 8 +
  shadows 30.2 s / 873 ms (35×). The plan's < 0.3 s for the last is not
  met yet: the frame's own render (~120 ms, for depth and the other
  nearest-surface passes) and 32 sets of shadow maps (8 times × 4 jitter
  cells) remain; sharing maps between nearby times is the next step.
- **Done (particles):** particle sprites on the GPU (`sprites.wgsl`):
  sorted and binned in drawing order on the CPU, one thread a pixel lays
  its tile's sprites as `particles_render` does (outlines, soft edges,
  textures and sequences, over / additive, hidden behind the render's
  depth), for Scanline Render, its motion pass and 2D particles. Taken
  only where the sprites cover more than twice the frame: small ones are
  cheaper on the CPU, which needn't send the frame across. Measured: 20 k
  big textured sprites 1080p 85 ms against 15.8 s, 4K 297 ms against
  61.7 s; 500 k dust-sized sprites stay on the CPU (~0.15–0.24 s).
  Particle objects were already meshes on the GPU.
- **Done (motion blur with sprites):** each time renders into a scratch
  frame on the GPU, `blur.wgsl` turns it into a picture and depth, that
  time's sprites go over it there (`sprites::Job`, whatever their size:
  nothing crosses to the CPU), and the jitter cell's pixels join the sum.
  Added-up sprites over a background still render time by time (the mean
  over a background differs there). Measured, 4K, the particle scene,
  motion blur 8 (CPU / GPU): 50 k sprites 6.7 s / 334 ms (20×), 500 k
  10.4 s / 2.65 s (4×). For 500 k, ~75 ms a time is CPU work (the
  particles at each time, projecting and sorting them, binning): moving
  the sort and binning to the GPU is the next step if it matters.
- Target (4K, the measured scene): motion blur 8 + shadows from 28 s to
  under 0.3 s.

### G6: Splats on the GPU [Dev2]

- `render3d/splats.rs` keeps its interface; a GPU path behind it:
  per-view projection + SH in compute, tile binning, a GPU radix sort
  (keys = tile, depth; ties by index), per-tile front-to-back blending,
  merged with G4's surface fragments by depth (D2 of `splat-plan.md`),
  splat sets uploaded once per `Arc`.
- The 3D view's settled splat render becomes interactive (the dots stay
  for very heavy captures while moving).
- Tests: the S1–S3 cases GPU vs CPU (D8).

### G7: Zero-copy viewing [Dev1]

- The GUI hands the engine's device to eframe (`WgpuSetup::Existing`,
  D3); the viewer draws a GPU-resident result directly (registered as a
  native texture) with a small shader for exposure, channel, sRGB and the
  overscan dimming, no read-back; read-back only for the pixel readout
  and pickers. With the GPU off, the current `to_color_image` path.
- Same for the 3D view's settled render (drawn from its GPU output).

Order and parallelism: **G1 first** (Dev1 and Dev2 at once, disjoint
files), then **Dev1: G2 → G3 → G7** and **Dev2: G4 → G5 → G6** in
parallel. Dev2's milestones need only G1's `gpu::get()`; using
GPU-resident textures from G2 in the renderer is a small follow-up once
both land.

## Owners and files

| Milestone | Owner | Files (new ones marked +) |
|-----------|-------|---------------------------|
| G1 core | Dev1 | `Cargo.toml` (deps, feature, naga dev-dep), `python/Cargo.toml`, `src/lib.rs`, + `src/gpu/mod.rs`, + `src/gpu/image.rs`, + `src/gpu/check.rs`, `src/render3d.rs` (only the empty `gpu_checks()` stub; Dev2 isn't in that file during G1), `src/main.rs`, `src/mcp.rs`, `python/arclt/__init__.py`, `src/crash.rs` (one call) |
| G1 prefs | Dev2 | `src/config.rs`, `src/gui/preferences.rs`, `src/gui/main.rs` (About box, Help ▸ GPU self-test), `src/gui/crash_dialog.rs`, `docs/help.md` |
| G2, G3 | Dev1 | `src/graph.rs`, `src/ops.rs`, + `src/gpu/ops.rs`, + `src/gpu/*.wgsl` |
| G4–G6 | Dev2 | `src/render3d.rs` (dispatch only), + `src/render3d/gpu/*.rs`, + `src/render3d/gpu/*.wgsl`, `src/render3d/splats.rs`, `src/render3d/shadow.rs`, `src/render3d/particles.rs`, `src/particles_render.rs` |
| G7 | Dev1 | `src/gui/main.rs` (wgpu setup), `src/gui/viewer.rs`, `src/gui/viewport3d.rs` (display only) |

Overlaps to sequence: `src/gui/main.rs` (Dev2 in G1, Dev1 in G7: far
apart in time) and `src/gui/viewport3d.rs` (Dev1's G7 touches only how a
finished render is shown; it waits until Dev2 has no open change there).
README's GPU section: Dev1 in G1; Dev2 adds 3D lines through its
reports.

## Risks and open questions

Risks:
- **R1, matching the CPU in 3D.** The CPU renderer is f64 with exact
  per-sample fragment sorting; the GPU will differ at edges and in
  shadow-map texels. D8's statistical tolerance is the contract; if the
  user needs bit-exact renders, the CPU stays available (Q2).
- **R2, transparency on the GPU.** *(Settled in G4, see D6.)* A sample
  keeps 16 surfaces, then 64 in a second try; past that, those pixels
  render on the CPU. Measured: 30–60 overlapping see-through cards stay
  on the GPU; 200 at 10 % opacity send 8 % of pixels to the CPU (still
  faster than all-CPU).
- **R3, transfer cost.** 4K round trip ~42 ms; a chain alternating GPU and
  CPU-only ops (shapes, plugins, Light 2D) can be slower than all-CPU. The
  dispatcher should keep a node on the CPU when its inputs are CPU-side
  and the op is pointwise (cheap either way); measured in G2.
- **R4, memory.** 4K RGBA f32 = 127 MiB per image, planes extra; splat
  sets up to ~1.5 GB. The budget (D5) and per-`Arc` caches need limits;
  wgpu doesn't report free VRAM, so out-of-memory is handled as an error
  → CPU (D7).
- **R5, drivers.** A driver crash takes the process down (no isolation
  like plugins). Crash reports carry the adapter; `ARCLIGHT_GPU=0` and
  the preference are the user's way out; the crash dialog could suggest
  turning the GPU off when the report's adapter is set (small, G1 if
  wanted).
- **R6, macOS untested here.** No Metal on this machine: Metal's
  fast-math precision, Intel Mac limits and Apple Silicon speed are only
  known once the user runs `cargo test` and `arclight gpu --check` on the
  Macs. The naga MSL test catches shader translation errors here first;
  the rest is the hand checks in the macOS section.
- **R7, build size and time.** wgpu is already compiled for the GUI; the
  CLI-only and Python builds grow (~several MB). Acceptable unless the
  user objects (Q1).

Answers (user, 2026-10-30): **Q1 yes**, `gpu` on by default in the CLI
and the Python wheels. **Q2 yes**, final renders use the GPU by default,
with `--cpu` (CLI) / a `cpu` argument (MCP, Python) for an exact CPU
render. **Q3 yes**, 3D (G4, G5) has priority over 2D (G2, G3) if only
part ships; both still run in parallel. **Q4 later**: a GPU Defocus op
goes to TODO, not this plan. **Q5 agreed**: particle simulation stays on
the CPU.

The questions as asked:
- **Q1.** `gpu` feature on by default for the CLI and the Python wheels
  too (proposed: yes; they are where batch renders run)?
- **Q2.** Final renders (Media Out, Write, `arclight render`, MCP /
  Python renders) on the GPU by default like the viewer (proposed), or
  always on the CPU for exact reproducibility, with the GPU only for
  interactive viewing?
- **Q3.** Priority if only part ships soon: proposed order of value is
  G4 + G5 (3D, motion blur) over G2 + G3 (2D). Agree?
- **Q4.** A new GPU-first **Defocus** op (depth-aware bokeh, using the
  depth plane) after G3? It doesn't exist today.
- **Q5.** Particle simulation on the GPU (3 s cold for 500 k particles to
  frame 100) is left out: it would change results and the simulation
  cache. Leave it out?
