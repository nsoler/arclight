//! ArclightVFX GUI — interactive node editor, viewer, player and properties for
//! the ArclightVFX compositing engine.
//!
//! Layout: viewer + transport in the center, node graph along the bottom,
//! properties on the right.

mod camera_tracker;
mod crash_dialog;
mod bridge_calls;
mod curve_editor;
mod dialogs;
mod doc;
mod gizmo3d;
mod gpu_check;
mod help;
mod node_editor;
mod overlay;
mod preferences;
mod project_settings;
mod properties;
mod script_jobs;
mod sound;
mod transport;
mod viewer;
mod viewport3d;
mod worker;

use arclight::bridge;
use arclight::config::{RecentFiles, Settings, ViewPrefs};
use arclight::graph::{Graph, NodeError};
use arclight::image::Image;
use curve_editor::CurveEditor;
use doc::Doc;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use node_editor::NodeEditor;
use properties::Properties;
use bridge_calls::{BridgeCalls, Call};
use script_jobs::ScriptJobs;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use transport::{Play, Transport};
use viewer::{Display, Viewer};
use worker::{Request, Worker};

/// What a render request depends on: (revision, target, frame, probe).
type RequestKey = (u64, String, i64, Option<(String, String)>);

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const REDO_ALT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
const NODE_BROWSER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::E);
const SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);
const GROUP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::G);
const UNGROUP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::G);
const PROJECT_SETTINGS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::P);
const PREFERENCES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);

/// The player and edit keys (`App::shortcuts`) pressed this frame: with no
/// modifier, the arrows also with ⌥ (key to key). Over the 3D view, Home
/// and F are its own (frame everything, the selection).
fn player_keys(i: &egui::InputState, over_3d: bool) -> Vec<Key> {
    use Key::*;
    [Space, L, J, K, ArrowRight, ArrowLeft, Home, End, Delete, Backspace, V, F]
        .into_iter()
        .filter(|&k| pressed_with(i, k, Modifiers::NONE) || matches!(k, ArrowLeft | ArrowRight) && pressed_with(i, k, Modifiers::ALT))
        .filter(|k| !(over_3d && matches!(k, Home | F)))
        .collect()
}

/// `key` was pressed (or repeated) with exactly `modifiers` held.
fn pressed_with(i: &egui::InputState, key: Key, modifiers: Modifiers) -> bool {
    i.events.iter().any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.matches_exact(modifiers)))
}

/// Take `key` pressed with no modifier at all. egui's `consume_key` ignores
/// an extra Shift or Alt, so `Modifiers::NONE` there takes Shift+Tab too.
fn consume_plain(i: &mut egui::InputState, key: Key) -> bool {
    pressed_with(i, key, Modifiers::NONE) && i.consume_key(Modifiers::NONE, key)
}

fn main() -> eframe::Result {
    // A plugin host (see arclight::plugin_host), started by Arclight itself:
    // before anything else, so it never opens a window.
    #[cfg(unix)]
    if std::env::args_os().nth(1).is_some_and(|a| a == "--plugin-host") {
        arclight::crash::install_host("plugin-host");
        let library = std::env::args_os().nth(2).map(PathBuf::from).unwrap_or_default();
        // (Out without exit-time teardown, as the window is: see
        // `gpu::exit_process`.)
        arclight::gpu::exit_process(arclight::plugin::run_host(&library));
    }
    // Crash reports (kept on this machine, see arclight::crash), and the
    // open project saved to its autosave on a crash.
    arclight::crash::install("arclight-gui");
    arclight::crash::set_rescue(rescue);
    if let Ok(exe) = std::env::current_exe() {
        arclight::plugin::set_host_command(vec![exe.into(), "--plugin-host".into()]);
    }
    arclight::plugin::init();
    let doc = match std::env::args_os().nth(1).map(PathBuf::from) {
        Some(path) => match Graph::from_file(&path) {
            Ok(g) => Doc::new(g, Some(path)),
            Err(e) => {
                eprintln!("arclight-gui: {e:#}");
                std::process::exit(1);
            }
        },
        // The example project, unless new projects have been given
        // settings of their own (Preferences ▸ New project).
        None => match Settings::load().new_project {
            p if p == arclight::config::NewProjectPrefs::default() => Doc::new(starter_graph(), None),
            p => Doc::blank(new_project(&p)),
        },
    };
    // The window draws with the engine's GPU device when there is one, so
    // results still on the GPU are shown from there (see gpu::display);
    // else eframe makes its own and the viewer converts on the CPU.
    #[allow(unused_mut)]
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    #[cfg(feature = "gpu")]
    if let Some(gpu) = arclight::gpu::for_window() {
        wgpu_options.wgpu_setup = eframe::egui_wgpu::WgpuSetup::Existing(eframe::egui_wgpu::WgpuSetupExisting {
            instance: gpu.instance.clone(),
            adapter: gpu.adapter.clone(),
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
        });
    }
    let options = eframe::NativeOptions {
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title(arclight::SHORT_NAME)
            // Matches arclight.desktop (install.sh), so launchers and the
            // compositor pair the window with its icon.
            .with_app_id("arclight")
            .with_icon(app_icon()),
        ..Default::default()
    };
    let result = eframe::run_native(
        arclight::SHORT_NAME,
        options,
        Box::new(|cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                let i = rs.adapter.get_info();
                arclight::crash::set_gpu(format!("{} ({:?}, {:?}, driver {} {})", i.name, i.backend, i.device_type, i.driver, i.driver_info));
            }
            let mut app = App::new(cc.egui_ctx.clone(), doc);
            app.viewer.wgpu = cc.wgpu_render_state.clone();
            app.viewer.viewport3d.wgpu = cc.wgpu_render_state.clone();
            Ok(Box::new(app))
        }),
    );
    // Out without the C libraries' exit-time teardown (see
    // `gpu::exit_process`): the window's Vulkan driver is loaded here even
    // with GPU acceleration off.
    if let Err(e) = &result {
        eprintln!("Error: {e:?}");
    }
    arclight::gpu::exit_process(i32::from(result.is_err()))
}

/// The window icon (the placeholder in `assets/`).
fn app_icon() -> egui::IconData {
    let png = include_bytes!("../../assets/arclight.png");
    let img = image::load_from_memory(png).expect("bundled icon").to_rgba8();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}

/// A project as Preferences ▸ New project says.
fn new_project(p: &arclight::config::NewProjectPrefs) -> arclight::graph::Project {
    let mut project = arclight::graph::Project::default();
    (project.first_frame, project.last_frame, project.fps) = (p.first_frame, p.last_frame.max(p.first_frame), p.fps);
    project.format = arclight::image::Size { width: p.width.max(1), height: p.height.max(1) };
    project
}

/// [`new_project`]'s way back: `project`'s settings as the defaults.
fn new_project_prefs(project: &arclight::graph::Project) -> arclight::config::NewProjectPrefs {
    arclight::config::NewProjectPrefs { width: project.format.width, height: project.format.height, fps: project.fps, first_frame: project.first_frame, last_frame: project.last_frame }
}

/// Set the 3D view's navigation and default shading as Preferences ▸ 3D
/// view says.
fn apply_view3d(p: &arclight::config::View3dPrefs, v: &mut viewport3d::Viewport3d) {
    v.nav = viewport3d::Navigation { emulate_three_button: p.emulate_three_button, trackpad_pans: p.trackpad == "pan", zoom_to_pointer: p.zoom_to_pointer, invert_zoom: p.invert_zoom, orbit_selection: p.orbit_selection };
    v.default_shading = match p.default_shading.as_str() {
        "solid" => Some(viewport3d::Shading::Solid),
        "lit" => Some(viewport3d::Shading::Lit),
        _ => None,
    };
}

/// The view preferences `viewer` shows now.
fn view_prefs(viewer: &Viewer, transport: &Transport) -> ViewPrefs {
    let view = &viewer.viewport3d.view;
    let shading = view.shading.map(|s| if s == viewport3d::Shading::Lit { "lit" } else { "solid" }.to_string());
    ViewPrefs { show_frame_size: viewer.show_frame_size, shading, wireframe: view.wireframe, lit_shadows: view.shadows, sound: transport.sound }
}

/// Set `viewer` up as `prefs` say: its frame size labels now, and the 3D
/// view for projects without their own.
fn apply_prefs(prefs: &ViewPrefs, viewer: &mut Viewer) {
    viewer.show_frame_size = prefs.show_frame_size;
    let shading = match prefs.shading.as_deref() {
        Some("lit") => Some(viewport3d::Shading::Lit),
        Some("solid") => Some(viewport3d::Shading::Solid),
        _ => None,
    };
    let new = &mut viewer.viewport3d.new_view;
    (new.shading, new.wireframe, new.shadows) = (shading, prefs.wireframe, prefs.lit_shadows);
}

/// The square region of a `size` (pixels) texture centered on `uv` (a
/// region of it, as fractions) and just holding it, kept inside the texture
/// where it fits: drawn in a square, `uv` shows whole and unstretched.
fn square_uv(uv: egui::Rect, size: egui::Vec2) -> egui::Rect {
    let px = uv.size() * size;
    let side = px.x.max(px.y);
    let half = egui::vec2(side, side) / size / 2.0;
    // Slid back inside the texture (when it's big enough for the square).
    let lo = |c: f32, h: f32| if 2.0 * h <= 1.0 { c.clamp(h, 1.0 - h) } else { c };
    let c = uv.center();
    let c = egui::pos2(lo(c.x, half.x), lo(c.y, half.y));
    egui::Rect::from_center_size(c, half * 2.0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Graph,
    Curves,
}

struct App {
    doc: Doc,
    editor: NodeEditor,
    curves: CurveEditor,
    tab: BottomTab,
    viewer: Viewer,
    transport: Transport,
    sound: sound::Sound,
    props: Properties,
    worker: Worker,
    /// The last request sent to the worker.
    requested: Option<RequestKey>,
    image: Option<Arc<Image>>,
    /// The same, still on the GPU when it was made there (see worker).
    image_gpu: Option<Arc<arclight::gpu::Resident>>,
    /// The selected Transform and its input's data window, for its overlay.
    probe: Option<(String, Arc<Image>)>,
    error: Option<NodeError>,
    shown_frame: i64,
    millis: f32,
    cached: Vec<i64>,
    title: String,
    status: Option<String>,
    /// `ARCLIGHT_SCREENSHOT=<png>`: save one screenshot of the window after
    /// `ARCLIGHT_SCREENSHOT_DELAY` seconds (default 2), then quit. For docs/CI.
    screenshot: Option<(PathBuf, std::time::Instant, f32)>,
    /// The view preferences as last saved (see [`ViewPrefs`]), and where
    /// (None: not saved, as for screenshot runs).
    prefs: ViewPrefs,
    prefs_path: Option<PathBuf>,
    /// The user's settings (Edit ▸ Preferences), and where they're saved
    /// (None: not, as for screenshot runs).
    settings: Settings,
    settings_path: Option<PathBuf>,
    preferences: preferences::Preferences,
    /// File ▸ Open Recent, and where it's kept (None: not, as above).
    recent: RecentFiles,
    recent_path: Option<PathBuf>,
    /// When unsaved changes were last copied aside (or the app started),
    /// and the graph copied (see [`App::autosave`]).
    autosaved: (std::time::Instant, String),
    /// The points-per-pixel when a new interface scale was set, until it
    /// changes (a frame or so later): then the views fit again, framed at
    /// the new size.
    rescale: Option<f32>,
    /// Socket that MCP servers and Python scripts (`arclt.attach()`) drive
    /// this window through.
    bridge: Option<bridge::Server>,
    /// Bridged renders running on worker threads.
    scripts: ScriptJobs,
    /// Other bridged graph tools, run off the UI thread one at a time (the
    /// next waits in `bridge_queue`, so it sees the edits of the one in
    /// flight), with the document's revision when each was sent.
    calls: BridgeCalls,
    bridge_queue: std::collections::VecDeque<bridge::Pending>,
    in_flight: Option<(bridge::Pending, u64)>,
    /// Unsaved changes were dealt with; let the next close request through.
    closing: bool,
    /// The question about unsaved changes being asked, if any.
    ask: Option<Ask>,
    /// The file dialog open for the File menu, if any (see `dialogs`).
    file: Option<dialogs::Pending<FileFor>>,
    /// Close once the save the close prompt asked for is done.
    close_after_save: bool,
    /// Help > About is open.
    about: bool,
    /// "ArclightVFX closed unexpectedly" (at start after a crash) or Help ▸
    /// Report a problem.
    crash: crash_dialog::CrashDialog,
    /// The project as last registered for a crash's rescue (its revision).
    rescue_revision: Option<u64>,
    /// ARCLIGHT_CRASH_TEST's crash has had its chance (after a first frame).
    crash_tested: bool,
    /// Help > ArclightVFX Help (F1).
    help: help::Help,
    /// GPU acceleration's status for the About box.
    gpu_status: gpu_check::StatusProbe,
    /// Help ▸ GPU self-test.
    gpu_test: gpu_check::SelfTest,
    /// File ▸ Project Settings…
    project_settings: project_settings::ProjectSettings,
    /// The window icon as a texture, for the About dialog (made on first use).
    /// and the part of it with artwork (UV), its transparent border cut off.
    logo: Option<(egui::TextureHandle, egui::Rect)>,
    /// File ▸ New's question: the new project's name and folder.
    starting: Option<NewProjectAsk>,
}

impl App {
    fn new(ctx: egui::Context, doc: Doc) -> App {
        let ctx_for_settings = ctx.clone();
        let wake = ctx.clone();
        let bridge = bridge::Server::start(move || wake.request_repaint())
            .inspect_err(|e| eprintln!("arclight-gui: scripting bridge disabled: {e:#}"))
            .ok();
        let mut transport = Transport::default();
        transport.frame = doc.graph.project.first_frame;
        let prefs = ViewPrefs::load(&ViewPrefs::path());
        transport.sound = prefs.sound;
        let screenshot_run = std::env::var_os("ARCLIGHT_SCREENSHOT").is_some();
        let recent_path = (!screenshot_run).then(RecentFiles::path);
        let mut recent = recent_path.as_deref().map(RecentFiles::load).unwrap_or_default();
        let settings = Settings::load();
        if let (Some(path), Some(file)) = (&recent_path, &doc.path) {
            recent.add(file, settings.files.recent_count);
            let _ = recent.save(path);
        }
        // The viewer's evaluator, which bridged calls share (see bridge_calls).
        let eval = Arc::new(std::sync::Mutex::new(arclight::graph::Evaluator::new()));
        let mut viewer = Viewer::default();
        apply_prefs(&prefs, &mut viewer);
        apply_view3d(&settings.view3d, &mut viewer.viewport3d);
        // Until a project's own is restored (so the first frames don't
        // read as a change and save the defaults over them).
        viewer.viewport3d.view = viewer.viewport3d.fresh_view();
        let crash = crash_dialog::CrashDialog::at_start(settings.crash_dialog && !screenshot_run);
        let mut app = App {
            doc,
            editor: NodeEditor::default(),
            curves: CurveEditor::default(),
            tab: BottomTab::Graph,
            viewer,
            transport,
            sound: sound::Sound::default(),
            props: Properties::sharing(eval.clone()),
            worker: Worker::spawn(ctx.clone(), eval.clone()),
            requested: None,
            image: None,
            image_gpu: None,
            probe: None,
            error: None,
            shown_frame: 0,
            millis: 0.0,
            cached: Vec::new(),
            title: String::new(),
            status: None,
            screenshot: std::env::var_os("ARCLIGHT_SCREENSHOT").map(|p| {
                let delay = std::env::var("ARCLIGHT_SCREENSHOT_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(2.0);
                (PathBuf::from(p), std::time::Instant::now(), delay)
            }),
            prefs,
            prefs_path: std::env::var_os("ARCLIGHT_SCREENSHOT").is_none().then(ViewPrefs::path),
            settings,
            settings_path: (!screenshot_run).then(Settings::path),
            preferences: Default::default(),
            recent,
            recent_path,
            autosaved: (std::time::Instant::now(), String::new()),
            rescale: None,
            bridge,
            scripts: ScriptJobs::sharing(eval.clone()),
            calls: BridgeCalls::spawn(eval, move || ctx.request_repaint()),
            bridge_queue: Default::default(),
            in_flight: None,
            closing: false,
            ask: None,
            file: None,
            close_after_save: false,
            about: false,
            crash,
            rescue_revision: None,
            crash_tested: false,
            help: help::Help::default(),
            gpu_status: Default::default(),
            gpu_test: Default::default(),
            project_settings: Default::default(),
            logo: None,
            starting: None,
        };
        app.apply_settings(&ctx_for_settings);
        app
    }

    /// Make the settings take effect: the interface's scale, theme, grid
    /// and wires, and where file dialogs start.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        let ui = &self.settings.interface;
        let scale = ui.ui_scale.clamp(0.5, 3.0);
        if scale != ctx.zoom_factor() {
            self.rescale = Some(ctx.pixels_per_point());
            ctx.set_zoom_factor(scale);
        }
        ctx.set_theme(match ui.theme.as_str() {
            "dark" => egui::ThemePreference::Dark,
            "light" => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        });
        self.editor.snap_to_grid = ui.snap_to_grid;
        self.editor.straight_wires = ui.wire_style == "straight";
        let f = &self.settings.files;
        self.recent.paths.truncate(f.recent_count);
        self.doc.undo_levels = f.undo_levels;
        apply_view3d(&self.settings.view3d, &mut self.viewer.viewport3d);
        // GPU acceleration: the next evaluation follows the switch. Its
        // results have their own cache keys, so the viewer only asks again.
        let gpu_on = self.settings.gpu.enabled;
        if arclight::gpu::enabled() != gpu_on {
            arclight::gpu::set_enabled(gpu_on);
            self.requested = None;
            self.gpu_status.refresh(ctx);
        }
    }

    /// Settings from the Preferences window: kept, saved and applied.
    fn set_settings(&mut self, ctx: &egui::Context, settings: Settings) {
        self.settings = settings;
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save_to(path) {
                self.status = Some(format!("saving {}: {e}", path.display()));
            }
        }
        self.apply_settings(ctx);
        self.save_recent();
    }

    /// `file` at the top of File ▸ Open Recent.
    fn remember(&mut self, file: &std::path::Path) {
        self.recent.add(file, self.settings.files.recent_count);
        self.save_recent();
    }

    fn save_recent(&self) {
        if let Some(path) = &self.recent_path {
            if let Err(e) = self.recent.save(path) {
                eprintln!("arclight-gui: can't save {}: {e}", path.display());
            }
        }
    }

    /// Keep the project where a crash's rescue finds it (see [`RESCUE`]):
    /// the graph as of each edit while there are unsaved changes.
    fn register_rescue(&mut self) {
        let revision = self.doc.dirty.then_some(self.doc.revision);
        if revision == self.rescue_revision {
            return;
        }
        self.rescue_revision = revision;
        let project = revision.map(|_| (self.doc.path.clone(), Arc::new(self.doc.graph.clone())));
        *RESCUE.lock().unwrap_or_else(|e| e.into_inner()) = project;
    }

    /// Every Files ▸ Autosave minutes, copy unsaved changes aside (see
    /// [`autosave_path`]), never over the project itself.
    fn autosave(&mut self, ctx: &egui::Context) {
        let minutes = self.settings.files.autosave_minutes;
        if minutes == 0 || !self.doc.dirty || self.screenshot.is_some() {
            return;
        }
        let every = std::time::Duration::from_secs(minutes as u64 * 60);
        let since = self.autosaved.0.elapsed();
        if since < every {
            ctx.request_repaint_after(every - since);
            return;
        }
        let json = self.doc.graph.to_json_at(self.doc.path.as_deref());
        if json != self.autosaved.1 {
            if let Err(e) = write_autosave(self.doc.path.as_deref(), &json) {
                eprintln!("arclight-gui: autosave failed: {e:#}");
            }
        }
        self.autosaved = (std::time::Instant::now(), json);
    }

    /// `select_node`, `view_node`, `get_view`: the window's view state.
    fn view_call(&mut self, tool: &str, args: &Value) -> anyhow::Result<Value> {
        let node = args.get("node").and_then(Value::as_str);
        let known = |doc: &Doc, id: &str| match doc.node(id) {
            Some(_) => Ok(id.to_string()),
            None => Err(anyhow::anyhow!("no node named '{id}'")),
        };
        match tool {
            "select_node" => {
                self.doc.selected = node.map(|id| known(&self.doc, id)).transpose()?;
            }
            "view_node" => {
                let id = known(&self.doc, node.ok_or_else(|| anyhow::anyhow!("missing string argument 'node'"))?)?;
                self.doc.commit(false);
                self.doc.view_node(&id);
                self.doc.place_new_nodes();
                self.doc.commit(false);
                self.doc.selected = Some(id);
                if let Some(frame) = args.get("frame").and_then(Value::as_i64) {
                    self.transport.set_play(Play::Stopped);
                    self.transport.seek(frame, &self.doc.graph.project);
                }
            }
            _ => {}
        }
        if let Some(pos) = self.doc.selected.as_ref().and_then(|id| self.doc.node(id)).and_then(|n| n.pos) {
            if tool != "get_view" {
                self.editor.reveal(pos);
            }
        }
        Ok(self.view_state())
    }

    /// What the user is looking at.
    fn view_state(&self) -> Value {
        let viewer = self.doc.viewer();
        let viewing = self.doc.viewed();
        json!({
            "selected": self.doc.selected,
            "viewer": viewer,
            "viewing": viewing,
            "frame": self.transport.frame,
            "playing": self.transport.play != Play::Stopped,
        })
    }

    /// Run tool calls that arrived over the bridge, in order. Renders go
    /// to worker threads (on a snapshot of the graph) and other graph tools
    /// to the bridge call thread (one at a time); view tools and undo /
    /// redo run here.
    fn handle_bridge(&mut self, ctx: &egui::Context) {
        let Some(server) = &self.bridge else { return };
        self.bridge_queue.extend(server.poll());
        if let Some(done) = self.calls.poll() {
            if let Some((req, revision)) = self.in_flight.take() {
                let result = self.finish_call(&req.tool, done, revision).map_err(|e| format!("{e:#}"));
                req.reply(result);
            }
        }
        while self.in_flight.is_none() {
            let Some(req) = self.bridge_queue.pop_front() else { break };
            if arclight::mcp::is_render_tool(&req.tool) {
                let (tool, args) = (req.tool.clone(), req.arguments.clone());
                let wake = ctx.clone();
                self.scripts.spawn(
                    self.doc.graph.clone(),
                    self.doc.path.clone(),
                    &tool,
                    args,
                    Box::new(move |r| req.reply(r)),
                    move || wake.request_repaint(),
                );
                continue;
            }
            match self.bridge_call(&req.tool, &req.arguments) {
                Some(result) => req.reply(result.map_err(|e| format!("{e:#}"))),
                None => self.in_flight = Some((req, self.doc.revision)),
            }
        }
    }

    /// Run one bridged tool: the view tools and undo / redo here (Some
    /// result); any other goes to the bridge call thread on a copy of the
    /// graph (None: see [`App::finish_call`]).
    fn bridge_call(&mut self, tool: &str, args: &Value) -> Option<anyhow::Result<Value>> {
        if arclight::mcp::is_gui_tool(tool) {
            return Some(self.view_call(tool, args));
        }
        match tool {
            "undo" | "redo" => {
                self.doc.commit(false);
                let able = if tool == "undo" { self.doc.can_undo() } else { self.doc.can_redo() };
                if tool == "undo" { self.undo() } else { self.redo() }
                return Some(Ok(json!({ if tool == "undo" { "undone" } else { "redone" }: able })));
            }
            "load_graph" | "new_graph" if self.doc.dirty => {
                return Some(Err(anyhow::anyhow!("the ArclightVFX window has unsaved changes; save them first (save_graph)")));
            }
            _ => {}
        }
        self.calls.send(Call { tool: tool.to_string(), args: args.clone(), graph: self.doc.graph.clone(), path: self.doc.path.clone(), dirty: self.doc.dirty });
        None
    }

    /// Apply a finished bridged tool to the document (sent at `revision`):
    /// a changed graph comes back as one undo step. If the window's graph
    /// was edited meanwhile, a change is refused rather than undoing that
    /// edit.
    fn finish_call(&mut self, tool: &str, done: bridge_calls::Done, revision: u64) -> anyhow::Result<Value> {
        let out = done.result.map_err(|e| anyhow::anyhow!(e))?;
        let edited = self.doc.revision != revision;
        match tool {
            "load_graph" | "new_graph" if edited => anyhow::bail!("the graph was edited in the ArclightVFX window while {tool} ran; call it again"),
            "load_graph" | "new_graph" => self.load(Doc::new(done.graph, done.path)),
            "save_graph" => {
                self.doc.path = done.path;
                // (What was saved is the graph as the call found it.)
                self.doc.dirty = edited;
            }
            _ if done.changed && edited => anyhow::bail!("the graph was edited in the ArclightVFX window while {tool} ran; nothing changed, call it again"),
            _ if done.changed => {
                self.doc.commit(false);
                self.doc.graph = done.graph;
                if self.doc.selected.as_ref().is_some_and(|id| self.doc.node(id).is_none()) {
                    self.doc.selected = None;
                }
                self.doc.place_new_nodes();
                self.doc.changed();
                self.doc.commit(false);
                self.props.invalidate();
            }
            _ => {}
        }
        Ok(out)
    }

    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, t0, delay)) = &self.screenshot else { return };
        if t0.elapsed().as_secs_f32() < *delay {
            ctx.request_repaint();
            return;
        }
        let path = path.clone();
        self.screenshot = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|b| b.save(&path)) {
                Some(Ok(())) => eprintln!("arclight-gui: screenshot -> {}", path.display()),
                _ => eprintln!("arclight-gui: failed to save screenshot"),
            }
            arclight::gpu::exit_process(0);
        })));
    }

    fn load(&mut self, doc: Doc) {
        arclight::crash::note(format!("project loaded ({} nodes)", doc.graph.nodes.len()));
        if let Some(path) = doc.path.clone() {
            self.remember(&path);
        }
        self.autosaved = (std::time::Instant::now(), String::new());
        let sound = self.transport.sound;
        self.transport = Transport::default();
        (self.transport.frame, self.transport.sound) = (doc.graph.project.first_frame, sound);
        self.doc = doc;
        self.doc.undo_levels = self.settings.files.undo_levels;
        self.editor.fit();
        self.curves = CurveEditor::default();
        self.viewer.fit();
        self.props.reset();
        self.project_settings.cancel();
        self.requested = None;
    }

    /// On a close request with unsaved changes, ask whether to save them
    /// first (see [`App::ask_ui`]); Cancel (or a cancelled save) keeps the
    /// window open.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.closing || !self.doc.dirty {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.ask = Some(Ask::Close);
    }

    /// Close the window now (unsaved changes dealt with).
    fn close_now(&mut self, ctx: &egui::Context) {
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn new_doc(&mut self) {
        if self.doc.dirty {
            self.ask = Some(Ask::Discard(Then::New));
        } else {
            self.ask_new();
        }
    }

    /// Ask for the new project's name and folder (see [`App::new_ui`]).
    fn ask_new(&mut self) {
        self.starting = Some(NewProjectAsk::new(self.settings.files.project_dir.clone()));
    }

    /// Start a new project as Preferences ▸ New project says, in `folder`
    /// (made if it isn't there).
    fn start_new(&mut self, folder: Option<PathBuf>) {
        let mut project = new_project(&self.settings.new_project);
        if let Some(dir) = &folder {
            if let Err(e) = std::fs::create_dir_all(dir) {
                self.status = Some(format!("making {}: {e}", dir.display()));
                return;
            }
        }
        project.folder = folder;
        self.load(Doc::blank(project));
    }

    /// File ▸ New's question, while it's asked: a name for the project and
    /// its folder (in the Projects folder, unless chosen or typed), or no
    /// folder.
    fn new_ui(&mut self, ctx: &egui::Context) {
        let Some(ask) = &mut self.starting else { return };
        let projects = self.settings.files.project_dir.clone();
        let (mut create, mut cancel, mut browse) = (false, false, false);
        egui::Modal::new(egui::Id::unique("new_project")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("New project");
            ui.add_space(8.0);
            egui::Grid::new("new_project_ask").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Name");
                if ui.add(egui::TextEdit::singleline(&mut ask.name).desired_width(300.0)).changed() {
                    ask.follow_name(projects.as_deref());
                }
                ui.end_row();
                ui.label("Folder");
                ui.vertical(|ui| {
                    ui.add_enabled_ui(ask.use_folder, |ui| {
                        ui.horizontal(|ui| {
                            if ui.add(egui::TextEdit::singleline(&mut ask.folder).desired_width(220.0)).changed() {
                                ask.folder_typed = true;
                            }
                            browse = ui.add_enabled(!dialogs::busy(), egui::Button::new("Browse…")).clicked();
                        });
                    });
                    ui.checkbox(&mut ask.use_folder, "Keep the project in a folder")
                        .on_hover_text("Its media dialogs and renders start there, and files in it are saved relative to it, so the whole folder can move. Made if it isn't there.");
                    if ask.use_folder && projects.is_none() && !ask.folder_typed {
                        ui.weak("Set a Projects folder in Preferences ▸ Files and safety to have new ones made there.");
                    }
                });
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let ok = !ask.use_folder || !ask.folder.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Create")).clicked() || (ok && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    create = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        });
        if browse && self.file.is_none() {
            let start = ask.chosen().filter(|d| d.is_dir()).or(projects).filter(|d| d.is_dir());
            let dialog = start.map_or_else(dialogs::new, |d| dialogs::new().set_directory(d));
            self.file = dialogs::pick_folder(FileFor::NewFolder, dialog);
        }
        if create {
            let folder = self.starting.take().and_then(|a| a.chosen());
            self.start_new(folder);
        } else if cancel {
            self.starting = None;
        }
    }

    /// Open the project at `path` (File ▸ Open Recent), asking about
    /// unsaved changes first.
    fn open_path(&mut self, path: PathBuf) {
        if self.doc.dirty {
            self.ask = Some(Ask::Discard(Then::Load(path)));
            return;
        }
        match Graph::from_file(&path) {
            Ok(g) => self.load(Doc::new(g, Some(path))),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn open(&mut self) {
        if self.doc.dirty {
            self.ask = Some(Ask::Discard(Then::Open));
        } else {
            self.choose(FileFor::Open);
        }
    }

    /// Show the file dialog for `what` (nothing if one is open already).
    fn choose(&mut self, what: FileFor) {
        if self.file.is_some() {
            return;
        }
        self.file = match what {
            FileFor::Open => dialogs::pick_project(what, dialogs::filter(dialogs::new(), "ArclightVFX project", &["json"])),
            FileFor::SaveAs => {
                // Named after its folder, if it has one.
                let name = self.doc.graph.project.folder.as_deref().and_then(|f| f.file_name()).map_or("comp".into(), |n| n.to_string_lossy().into_owned());
                dialogs::save_project(what, dialogs::filter(dialogs::new(), "ArclightVFX project", &["json"]).set_file_name(format!("{name}.json")))
            }
            FileFor::Import => dialogs::pick(what, dialogs::new()),
            FileFor::NewFolder => None,
        };
    }

    /// The File menu's dialog, once it has closed: open, save or import
    /// what was chosen.
    fn poll_file(&mut self, ctx: &egui::Context) {
        let Some(answer) = self.file.as_ref().and_then(|p| p.poll()) else {
            if self.file.is_some() {
                // Keep looking while it's open.
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let what = self.file.take().map(|p| p.what);
        let after_save = std::mem::take(&mut self.close_after_save);
        let Some(path) = answer else { return };
        match what {
            Some(FileFor::Open) => match Graph::from_file(&path) {
                Ok(g) => self.load(Doc::new(g, Some(path))),
                Err(e) => self.status = Some(format!("{e:#}")),
            },
            Some(FileFor::SaveAs) => {
                self.write(path);
                if after_save && !self.doc.dirty {
                    self.close_now(ctx);
                }
            }
            Some(FileFor::Import) => {
                let id = self.doc.add_node("media_in", [0.0, -90.0]);
                properties::apply_media(&mut self.doc, &id, &path);
                self.doc.selected = Some(id);
            }
            Some(FileFor::NewFolder) => {
                if let Some(ask) = &mut self.starting {
                    ask.folder = path.to_string_lossy().into_owned();
                    ask.folder_typed = true;
                }
            }
            None => {}
        }
    }

    fn save(&mut self, save_as: bool) {
        match (&self.doc.path, save_as) {
            (Some(p), false) => self.write(p.clone()),
            _ => self.choose(FileFor::SaveAs),
        }
    }

    /// Save the project to `path`.
    fn write(&mut self, path: PathBuf) {
        // Files in the project's folder relative to it (see Graph::save).
        match self.doc.graph.save(&path) {
            Ok(()) => {
                arclight::crash::note("project saved");
                // Its autosave is behind it now.
                let _ = std::fs::remove_file(autosave_path(Some(&path)));
                self.remember(&path);
                self.doc.path = Some(path);
                self.doc.dirty = false;
                self.status = None;
            }
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    /// The question about unsaved changes, as a modal over the window (the
    /// system's message boxes would stop the app while they're up).
    fn ask_ui(&mut self, ctx: &egui::Context) {
        let Some(ask) = self.ask.clone() else { return };
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("unsaved_changes")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Unsaved changes");
            ui.add_space(6.0);
            ui.label(match &ask {
                Ask::Close => "Save changes to this project before closing?",
                Ask::Discard(_) => "Discard unsaved changes to this project?",
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| match &ask {
                Ask::Close => {
                    if ui.button("Save").clicked() {
                        answer = Some(Answer::Save);
                    }
                    if ui.button("Don't Save").clicked() {
                        answer = Some(Answer::Discard);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        answer = Some(Answer::Cancel);
                    }
                }
                Ask::Discard(_) => {
                    if ui.button("Discard").clicked() {
                        answer = Some(Answer::Discard);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        answer = Some(Answer::Cancel);
                    }
                }
            });
        });
        let Some(answer) = answer else { return };
        self.ask = None;
        match (ask, answer) {
            (_, Answer::Cancel) => {}
            (Ask::Close, Answer::Save) => {
                self.save(false);
                if !self.doc.dirty {
                    self.close_now(ctx);
                } else if self.file.is_some() {
                    // Saving as: close once it's saved.
                    self.close_after_save = true;
                }
            }
            (Ask::Close, _) => self.close_now(ctx),
            (Ask::Discard(Then::New), _) => self.ask_new(),
            (Ask::Discard(Then::Open), _) => self.choose(FileFor::Open),
            (Ask::Discard(Then::Load(path)), _) => match Graph::from_file(&path) {
                Ok(g) => self.load(Doc::new(g, Some(path))),
                Err(e) => self.status = Some(format!("{e:#}")),
            },
        }
    }

    fn undo(&mut self) {
        self.doc.undo();
        self.props.invalidate();
    }

    fn redo(&mut self) {
        self.doc.redo();
        self.props.invalidate();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // First, so Esc closes the help rather than deselecting.
        self.help.shortcuts(ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&SAVE_AS)) {
            self.save(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SAVE)) {
            self.save(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&OPEN)) {
            self.open();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&NEW)) {
            self.new_doc();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&PROJECT_SETTINGS)) {
            self.project_settings.open(&self.doc);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&PREFERENCES)) {
            self.preferences.show(&self.settings);
        }
        if ctx.text_edit_focused() {
            // Let the focused text field use ⌘Z for its own text undo.
            return;
        }
        // G / R / S in the 3D view takes the keys (Esc, Backspace, digits…).
        if self.viewer.is_transforming() {
            return;
        }
        // Redo first: shortcut matching ignores an extra Shift.
        if ctx.input_mut(|i| i.consume_shortcut(&REDO) || i.consume_shortcut(&REDO_ALT)) {
            self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&UNDO)) {
            self.undo();
        }
        // Tab over the viewer switches it between 2D and 3D (elsewhere, and
        // in text fields, which returned above, Tab keeps its usual job).
        // Plain Tab only: Shift+Tab is the node browser's, over the graph.
        let over_viewer = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| self.viewer.rect.contains(p));
        if over_viewer && ctx.input_mut(|i| consume_plain(i, Key::Tab)) {
            self.viewer.toggle_3d();
            // egui read the Tab already, to move keyboard focus this frame.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        // ⌘A: every node, over the node graph; else every vertex of the
        // selected shape.
        let over_graph = self.tab == BottomTab::Graph && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| self.editor.rect.contains(p));
        if ctx.input_mut(|i| i.consume_shortcut(&SELECT_ALL)) {
            if over_graph {
                self.doc.select_all();
            } else {
                self.viewer.select_all_points(&self.doc);
            }
        }
        // Ungroup first: shortcut matching ignores an extra Shift.
        if ctx.input_mut(|i| i.consume_shortcut(&UNGROUP)) {
            if let Some(b) = self.doc.selected_backdrop.clone() {
                self.doc.delete_backdrop(&b);
            }
        } else if ctx.input_mut(|i| i.consume_shortcut(&GROUP)) {
            self.doc.group_selection();
        }
        // Esc clears the node graph's selection (text fields returned above;
        // the node browser handles its own).
        if !self.editor.browser_open() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.doc.clear_selection();
        }
        // Shift+E before any plain-E binding: shortcut matching ignores Shift.
        if ctx.input_mut(|i| i.consume_shortcut(&NODE_BROWSER)) {
            self.tab = BottomTab::Graph;
            self.editor.open_browser(ctx.input(|i| i.pointer.hover_pos()));
        }
        let project = self.doc.graph.project.clone();
        let over_3d = over_viewer && self.viewer.mode3d;
        let taken = ctx.input(|i| player_keys(i, over_3d));
        let pressed = |k: Key| taken.contains(&k);
        let t = &mut self.transport;
        if pressed(Key::Space) {
            t.set_play(if t.play == Play::Stopped { Play::Forward } else { Play::Stopped });
        }
        if pressed(Key::L) {
            t.toggle(Play::Forward);
        }
        if pressed(Key::J) {
            t.toggle(Play::Backward);
        }
        if pressed(Key::K) {
            t.set_play(Play::Stopped);
        }
        // Alt+arrows jump between the selected node's keyframes.
        let alt = ctx.input(|i| i.modifiers.alt);
        let keys = self.doc.selected.as_ref().map(|id| self.doc.node_key_frames(id)).unwrap_or_default();
        if pressed(Key::ArrowRight) {
            match keys.iter().find(|&&f| f > t.frame) {
                Some(&f) if alt => {
                    t.set_play(Play::Stopped);
                    t.seek(f, &project);
                }
                _ if alt => {}
                _ => t.step(1, &project),
            }
        }
        if pressed(Key::ArrowLeft) {
            match keys.iter().rev().find(|&&f| f < t.frame) {
                Some(&f) if alt => {
                    t.set_play(Play::Stopped);
                    t.seek(f, &project);
                }
                _ if alt => {}
                _ => t.step(-1, &project),
            }
        }
        if pressed(Key::Home) {
            t.set_play(Play::Stopped);
            t.seek(project.first_frame, &project);
        }
        if pressed(Key::End) {
            t.set_play(Play::Stopped);
            t.seek(project.last_frame, &project);
        }
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            // Selected keys in the curve editor, or selected shape points in
            // the viewer, are deleted rather than the node.
            if self.tab == BottomTab::Curves && self.curves.has_selection() {
                self.curves.delete_selected(&mut self.doc);
            } else if self.viewer.delete_points(&mut self.doc) {
            } else {
                self.doc.delete_selection();
            }
        }
        if pressed(Key::V) {
            if let Some(id) = self.doc.selected.clone() {
                self.doc.view_node(&id);
            }
        }
        if pressed(Key::F) {
            if over_viewer {
                self.viewer.fit();
            } else if self.tab == BottomTab::Curves {
                self.curves.fit();
            } else {
                self.editor.fit();
            }
        }
    }

    /// Dropping files creates a Media In per file (wired to the viewer if
    /// nothing is being viewed yet).
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for (k, path) in dropped.into_iter().enumerate() {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
                if let Ok(g) = Graph::from_file(&path) {
                    if self.doc.dirty {
                        self.ask = Some(Ask::Discard(Then::Load(path)));
                    } else {
                        self.load(Doc::new(g, Some(path)));
                    }
                }
                return;
            }
            let x = self.doc.graph.nodes.iter().filter_map(|n| n.pos).map(|p| p[0]).fold(0.0f32, f32::min);
            let y = self.doc.graph.nodes.iter().filter_map(|n| n.pos).map(|p| p[1]).fold(0.0f32, f32::max);
            let id = self.doc.add_node("media_in", [x, y + 90.0 + k as f32 * 70.0]);
            properties::apply_media(&mut self.doc, &id, &path);
            if self.doc.view_target().is_none() || self.error.is_some() {
                self.doc.route_to_viewer(&id);
            }
            self.doc.selected = Some(id);
            self.transport.seek(self.doc.graph.project.first_frame, &self.doc.graph.project);
        }
    }

    /// Play the viewed node's sound along with forward playback (see
    /// [`sound`]): after [`App::sync_render`], which says what's shown.
    fn sync_sound(&mut self) {
        let project = &self.doc.graph.project;
        let want = self.transport.play_position(project).filter(|_| self.transport.sound).and_then(|pos| {
            let target = self.doc.view_target()?;
            sound::Cue::at(&self.doc.graph, &target, pos, project.fps)
        });
        // How far the picture is behind the playhead (round a loop's wrap).
        let (first, last) = (project.first_frame, project.last_frame.max(project.first_frame));
        let behind = self.transport.frame - self.shown_frame;
        let lag = if self.transport.looping { behind.rem_euclid(last - first + 1) } else { behind.max(0) };
        if let Some(message) = self.sound.update(self.transport.sound, want.as_ref(), lag) {
            self.status = Some(message);
        }
    }

    fn sync_render(&mut self) {
        let target = self.doc.view_target();
        match &target {
            Some(t) => {
                // A selected node with a Transform overlay wants its input's
                // data window; one with a color picker, its input's pixels;
                // a Corner Pin, its input's frame; a Shuffle, its layers.
                let probe = self.doc.selected.as_ref().and_then(|id| self.doc.node(id)).filter(|n| {
                    use arclight::ops::Overlay;
                    n.op == "shuffle" || arclight::ops::info(&n.op).is_some_and(|i| matches!(i.overlay, Overlay::Transform { .. } | Overlay::Pick(_) | Overlay::CornerPin { .. }))
                });
                let probe = probe.and_then(|n| n.inputs.first().filter(|i| !i.is_empty()).map(|i| (n.id.clone(), i.clone())));
                let key = (self.doc.revision, t.clone(), self.transport.frame, probe.clone());
                if self.requested.as_ref() != Some(&key) {
                    self.worker.request(Request {
                        graph: Arc::new(self.doc.graph.clone()),
                        target: t.clone(),
                        frame: self.transport.frame,
                        probe,
                    });
                    self.requested = Some(key);
                }
            }
            None => {
                self.image = None;
                self.image_gpu = None;
                self.error = None;
                self.requested = None;
                self.cached.clear();
            }
        }
        if let Some(resp) = self.worker.poll() {
            if Some(&resp.target) == target.as_ref() {
                match resp.result {
                    Ok(img) => {
                        let new_frame = self.image.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &img));
                        self.image = Some(img);
                        self.image_gpu = resp.gpu;
                        self.error = None;
                        if new_frame {
                            self.transport.frame_shown();
                        }
                    }
                    Err(e) => {
                        self.image = None;
                        self.image_gpu = None;
                        self.error = Some(e);
                    }
                }
                self.shown_frame = resp.frame;
                self.millis = resp.millis;
                self.cached = resp.cached_frames;
                self.probe = resp.probe;
                self.doc.view_format = self.image.as_ref().map(|img| img.format);
            }
        }
    }

    /// Save the view preferences when they've changed (also making them
    /// what a project without its own 3D view starts with).
    fn save_prefs(&mut self) {
        let now = view_prefs(&self.viewer, &self.transport);
        if now == self.prefs {
            return;
        }
        apply_prefs(&now, &mut self.viewer);
        if let Some(path) = &self.prefs_path {
            if let Err(e) = now.save(path) {
                eprintln!("arclight-gui: can't save {}: {e}", path.display());
            }
        }
        self.prefs = now;
    }

    /// Help > About: logo, name, author, build time, version, GPU, OK.
    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let gpu = self.gpu_status.line(ctx);
        let (logo, uv) = self
            .logo
            .get_or_insert_with(|| {
                let icon = app_icon();
                let (w, h) = (icon.width as usize, icon.height as usize);
                // The opaque pixels' bounds, so the margins around the
                // artwork are the dialog's own.
                let opaque = |x: usize, y: usize| icon.rgba[(y * w + x) * 4 + 3] > 8;
                let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| opaque(x, y))).collect();
                let cols: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| opaque(x, y))).collect();
                let uv = match (rows.first(), rows.last(), cols.first(), cols.last()) {
                    (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) => egui::Rect::from_min_max(
                        egui::pos2(x0 as f32 / w as f32, y0 as f32 / h as f32),
                        egui::pos2((x1 + 1) as f32 / w as f32, (y1 + 1) as f32 / h as f32),
                    ),
                    _ => egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                };
                let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &icon.rgba);
                (ctx.load_texture("about-logo", img, egui::TextureOptions::LINEAR), uv)
            })
            .clone();
        // The same margin on every side; each row is one centered label.
        const MARGIN: i8 = 24;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(MARGIN);
        let modal = egui::Modal::new(egui::Id::unique("about")).frame(frame).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.vertical_centered(|ui| {
                // The artwork in a 160 px square, centered in its own
                // proportions. (egui would keep the whole texture's aspect,
                // whatever the uv: the box is given exactly.)
                let side = 160.0;
                let uv = square_uv(uv, logo.size_vec2());
                ui.add(egui::Image::new(&logo).uv(uv).maintain_aspect_ratio(false).fit_to_exact_size(egui::vec2(side, side)));
                ui.add_space(8.0);
                ui.heading(arclight::PRODUCT);
                ui.add_space(4.0);
                // Beta: a badge under the name.
                ui.label(egui::RichText::new(" BETA ").strong().small().color(egui::Color32::WHITE).background_color(egui::Color32::from_rgb(200, 120, 40)));
                ui.add_space(6.0);
                let row = |ui: &mut egui::Ui, name: &str, value: &str| {
                    let style = ui.style().clone();
                    let mut job = egui::text::LayoutJob::default();
                    egui::RichText::new(format!("{name}  ")).weak().append_to(&mut job, &style, egui::FontSelection::Default, egui::Align::Center);
                    egui::RichText::new(value).append_to(&mut job, &style, egui::FontSelection::Default, egui::Align::Center);
                    ui.label(job);
                };
                row(ui, "Created by:", "Nelson Soler");
                row(ui, "Version:", &format!("{} ({})", env!("CARGO_PKG_VERSION"), arclight::STAGE));
                row(ui, "Built:", env!("ARCLIGHT_BUILD_TIME"));
                row(ui, "GPU:", &gpu);
                ui.add_space(12.0);
                if ui.add(egui::Button::new("OK").min_size(egui::vec2(80.0, 0.0))).clicked() {
                    ui.close();
                }
            });
        });
        if modal.should_close() {
            self.about = false;
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".to_string(), |n| n.to_string_lossy().into_owned());
        let title = format!("{}{name} — {}", if self.doc.dirty { "• " } else { "" }, arclight::SHORT_NAME);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New").shortcut_text(ui.ctx().format_shortcut(&NEW))).clicked() {
                    ui.close();
                    self.new_doc();
                }
                if ui.add(egui::Button::new("Open…").shortcut_text(ui.ctx().format_shortcut(&OPEN))).clicked() {
                    ui.close();
                    self.open();
                }
                let recent = self.recent.paths.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open Recent", |ui| {
                        for path in &recent {
                            let name = path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
                            if ui.button(name).on_hover_text(path.to_string_lossy()).clicked() {
                                ui.close();
                                self.open_path(path.clone());
                            }
                        }
                        ui.separator();
                        if ui.button("Clear the list").clicked() {
                            ui.close();
                            self.recent.paths.clear();
                            self.save_recent();
                        }
                    });
                });
                if ui.add_enabled(!dialogs::busy(), egui::Button::new("Import media…")).clicked() {
                    ui.close();
                    self.choose(FileFor::Import);
                }
                if ui.add(egui::Button::new("Project Settings…").shortcut_text(ui.ctx().format_shortcut(&PROJECT_SETTINGS))).clicked() {
                    ui.close();
                    self.project_settings.open(&self.doc);
                }
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text(ui.ctx().format_shortcut(&SAVE))).clicked() {
                    ui.close();
                    self.save(false);
                }
                if ui.add(egui::Button::new("Save As…").shortcut_text(ui.ctx().format_shortcut(&SAVE_AS))).clicked() {
                    ui.close();
                    self.save(true);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let undo = egui::Button::new("Undo").shortcut_text(ui.ctx().format_shortcut(&UNDO));
                if ui.add_enabled(self.doc.can_undo(), undo).clicked() {
                    self.undo();
                    ui.close();
                }
                let redo = egui::Button::new("Redo").shortcut_text(ui.ctx().format_shortcut(&REDO));
                if ui.add_enabled(self.doc.can_redo(), redo).clicked() {
                    self.redo();
                    ui.close();
                }
                ui.separator();
                let sel = self.doc.selected.clone();
                let any = !self.doc.selection().is_empty() || self.doc.selected_backdrop.is_some();
                if ui.add_enabled(any, egui::Button::new("Delete selected").shortcut_text("⌫")).clicked() {
                    self.doc.delete_selection();
                    ui.close();
                }
                if ui.add(egui::Button::new("Select all nodes").shortcut_text(ui.ctx().format_shortcut(&SELECT_ALL))).clicked() {
                    self.doc.select_all();
                    ui.close();
                }
                let group = egui::Button::new("Group in a backdrop").shortcut_text(ui.ctx().format_shortcut(&GROUP));
                if ui.add_enabled(!self.doc.selection().is_empty(), group).clicked() {
                    self.doc.group_selection();
                    ui.close();
                }
                let ungroup = egui::Button::new("Ungroup (delete the backdrop)").shortcut_text(ui.ctx().format_shortcut(&UNGROUP));
                if ui.add_enabled(self.doc.selected_backdrop.is_some(), ungroup).clicked() {
                    if let Some(b) = self.doc.selected_backdrop.clone() {
                        self.doc.delete_backdrop(&b);
                    }
                    ui.close();
                }
                ui.separator();
                let viewer = self.doc.viewer().is_some();
                let target = if self.doc.selected_wire.is_some() {
                    "into the selected wire"
                } else if viewer {
                    "just before the Viewer"
                } else {
                    "with a new Viewer"
                };
                let tip = format!(
                    "A Camera, a Scene and a Scanline Render, wired up, in a \"3D Setup\" backdrop, {target}, so the render shows straight away. \
                     The picture that was there (else the selected node, or the end of the chain) becomes the render's background. \
                     Add Cards, meshes and lights to the Scene. (Right-click a wire to put it into that wire instead.)"
                );
                if ui.button("Add 3D Camera Setup").on_hover_text(tip).clicked() {
                    let at = self.editor.view_center();
                    self.doc.add_camera_setup(at);
                    ui.close();
                }
                ui.separator();
                if ui.add_enabled(sel.is_some(), egui::Button::new("View node").shortcut_text("V")).clicked() {
                    self.doc.view_node(sel.as_deref().unwrap());
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Preferences…").shortcut_text(ui.ctx().format_shortcut(&PREFERENCES))).clicked() {
                    self.preferences.show(&self.settings);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Fit viewer").clicked() {
                    self.viewer.fit();
                    ui.close();
                }
                if ui.button("Frame node graph").clicked() {
                    self.editor.fit();
                    ui.close();
                }
                ui.checkbox(&mut self.viewer.show_frame_size, "Show frame size")
                    .on_hover_text("Label the viewed frame's corners with its size: 0,0 bottom-left, width,height top-right");
                ui.separator();
                let scene = self.viewer.viewport3d.has_scene();
                let look = egui::Button::new("Look through selected").shortcut_text("0");
                if ui.add_enabled(scene, look).on_hover_text("In the 3D view, look through the selected camera, spot light or area light (else the scene's camera); again to come back out").on_disabled_hover_text("View a node with a 3D scene first").clicked() {
                    self.viewer.mode3d = true;
                    self.viewer.viewport3d.look_through_selected(&self.doc.selection());
                    ui.close();
                }
                let vp = &mut self.viewer.viewport3d;
                let mut lit = vp.shading() == viewport3d::Shading::Lit;
                if ui.add(egui::Checkbox::new(&mut lit, "Lit shading")).on_hover_text("In the 3D view, light meshes by the scene's lights and materials instead of from the view (⇧Z)").changed() {
                    vp.set_shading(if lit { viewport3d::Shading::Lit } else { viewport3d::Shading::Solid });
                }
                ui.add_enabled(lit, egui::Checkbox::new(&mut vp.view.shadows, "Lit shadows")).on_hover_text("With Lit shading, show the lights' shadows (once the view stops moving)");
            });
            ui.menu_button("Help", |ui| {
                if ui.add(egui::Button::new(format!("{} Help…", arclight::SHORT_NAME)).shortcut_text("F1")).clicked() {
                    self.help.open = true;
                    ui.close();
                }
                if ui.button("Report a problem…").on_hover_text("The latest crash report, to copy or put in a GitHub issue (nothing is sent by ArclightVFX)").clicked() {
                    self.crash = crash_dialog::CrashDialog::for_help();
                    ui.close();
                }
                if ui.button("GPU self-test…").on_hover_text("Run every GPU operation against the CPU on this computer: differences, pass / fail and speed").clicked() {
                    self.gpu_test.start(ui.ctx());
                    ui.close();
                }
                if ui.button(format!("About {}…", arclight::PRODUCT)).clicked() {
                    self.about = true;
                    self.gpu_status.refresh(ui.ctx());
                    ui.close();
                }
                ui.separator();
                ui.weak(format!("{} {} ({})", arclight::SHORT_NAME, env!("CARGO_PKG_VERSION"), arclight::STAGE));
            });
            if let Some(s) = &self.status {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(235, 100, 90), s);
            }
            if dialogs::busy() {
                ui.separator();
                ui.spinner();
                ui.label("Choosing a file…");
            }
            if self.props.is_rendering() {
                ui.separator();
                ui.spinner();
                ui.label("Rendering…");
            }
            if self.props.is_tracking() {
                ui.separator();
                ui.spinner();
                ui.label("Tracking…");
            }
            let scripts = self.scripts.running();
            for job in scripts {
                ui.separator();
                ui.spinner();
                let p = &job.progress;
                let total = p.total.load(std::sync::atomic::Ordering::Relaxed);
                let done = p.done.load(std::sync::atomic::Ordering::Relaxed);
                let text = if total > 1 { format!("Script: {} {done}/{total}", job.label) } else { format!("Script: {}", job.label) };
                ui.label(text).on_hover_text("Requested by a script or AI agent (MCP / Python)");
                if ui.small_button("Cancel").clicked() {
                    job.cancel();
                }
            }
            if !scripts.is_empty() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
    }
}

/// The open project with unsaved changes (its file, if saved before, and
/// its graph), for [`rescue`].
static RESCUE: std::sync::Mutex<Option<(Option<PathBuf>, Arc<Graph>)>> = std::sync::Mutex::new(None);

/// On a crash (in the panic hook, see arclight::crash): write the open
/// project's unsaved changes to its autosave, where the next start's
/// autosave recovery finds them. Never waits for the project's lock.
fn rescue() -> Result<PathBuf, String> {
    let slot = RESCUE.try_lock().map_err(|_| "it was being changed when ArclightVFX closed".to_string())?;
    let (path, graph) = slot.as_ref().ok_or("there were no unsaved changes")?;
    write_autosave(path.as_deref(), &graph.to_json_at(path.as_deref())).map_err(|e| e.to_string())
}

/// Where the autosave of the project saved at `project` goes: beside it
/// as `<name>.autosave` (never the project itself), or for one not saved
/// yet, `Untitled.autosave` in the state dir.
fn autosave_path(project: Option<&std::path::Path>) -> PathBuf {
    match project.and_then(|p| Some((p.parent()?, p.file_name()?))) {
        Some((dir, name)) => dir.join(format!("{}.autosave", name.to_string_lossy())),
        None => arclight::config::state_dir().join("autosave").join("Untitled.autosave"),
    }
}

/// Write `json` as the autosave of the project at `project` (see
/// [`autosave_path`]), whole or not at all; where that can't be written
/// (a read-only folder), in the state dir under the project's name.
fn write_autosave(project: Option<&std::path::Path>, json: &str) -> std::io::Result<PathBuf> {
    let write = |path: PathBuf| -> std::io::Result<PathBuf> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("autosave.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    };
    let path = autosave_path(project);
    debug_assert!(project.is_none_or(|p| p != path), "never over the project");
    write(path).or_else(|e| match project.and_then(|p| p.file_name()) {
        Some(name) => write(arclight::config::state_dir().join("autosave").join(format!("{}.autosave", name.to_string_lossy()))),
        None => Err(e),
    })
}

/// What the File menu's file dialog is choosing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileFor {
    Open,
    SaveAs,
    Import,
    /// A folder for File ▸ New's project.
    NewFolder,
}

/// File ▸ New's question being asked (see [`App::new_ui`]).
#[derive(Clone, Debug, PartialEq)]
struct NewProjectAsk {
    name: String,
    /// The folder as typed or chosen.
    folder: String,
    /// The folder was typed or chosen, so it no longer follows the name.
    folder_typed: bool,
    use_folder: bool,
}

impl NewProjectAsk {
    /// Asked afresh: "Untitled", in a folder of that name in `projects` (a
    /// folder is on by default when there's a Projects folder).
    fn new(projects: Option<PathBuf>) -> NewProjectAsk {
        let mut ask = NewProjectAsk { name: "Untitled".into(), folder: String::new(), folder_typed: false, use_folder: projects.is_some() };
        ask.follow_name(projects.as_deref());
        ask
    }

    /// The folder after a new name: `<projects>/<name>`, unless one was
    /// typed or chosen.
    fn follow_name(&mut self, projects: Option<&std::path::Path>) {
        if self.folder_typed {
            return;
        }
        let name = self.name.trim().replace(['/', '\\'], "_");
        self.folder = match projects {
            Some(dir) if !name.is_empty() => dir.join(name).to_string_lossy().into_owned(),
            _ => String::new(),
        };
    }

    /// The folder the project gets, if any.
    fn chosen(&self) -> Option<PathBuf> {
        Some(PathBuf::from(self.folder.trim())).filter(|f| self.use_folder && !f.as_os_str().is_empty())
    }
}

/// A question about unsaved changes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ask {
    /// Closing the window: save first?
    Close,
    /// Throwing them away to do `Then`.
    Discard(Then),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Then {
    New,
    /// Choose a project to open.
    Open,
    /// Open this project (dropped on the window).
    Load(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Save,
    Discard,
    Cancel,
}


impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        dialogs::set_parent(frame);
        self.poll_file(&ctx);
        self.handle_close(&ctx);
        self.handle_bridge(&ctx);
        self.handle_drops(&ctx);
        self.shortcuts(&ctx);
        // A first 3D setup in this project: show it in the 3D view.
        if let Some(render) = self.doc.show_3d.take() {
            self.doc.view_node(&render);
            self.viewer.announce_3d(ctx.input(|i| i.time));
        }
        if self.transport.play != Play::Stopped {
            self.transport.tick(&self.doc.graph.project);
            ctx.request_repaint();
        }
        self.sync_render();
        self.sync_sound();
        self.update_title(&ctx);
        self.maybe_screenshot(&ctx);
        self.save_prefs();
        self.autosave(&ctx);
        self.register_rescue();
        if !std::mem::replace(&mut self.crash_tested, true) {
            // (ARCLIGHT_CRASH_TEST: crash here on purpose, see crash.rs.)
            arclight::crash::crash_test_now();
        }
        if self.rescale.is_some_and(|before| before != ctx.pixels_per_point()) {
            self.rescale = None;
            self.editor.fit();
            self.viewer.fit();
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        self.about_dialog(&ctx);
        match self.crash.ui(&ctx) {
            Some(crash_dialog::Choice::DontAskAgain) => {
                let mut settings = self.settings.clone();
                settings.crash_dialog = false;
                self.set_settings(&ctx, settings);
            }
            Some(crash_dialog::Choice::TurnOffGpu) => {
                let mut settings = self.settings.clone();
                settings.gpu.enabled = false;
                self.set_settings(&ctx, settings);
            }
            None => {}
        }
        self.help.ui(&ctx);
        self.gpu_test.ui(&ctx);
        self.project_settings.ui(&ctx, &mut self.doc);
        if let Some(project) = self.project_settings.take_defaults() {
            let settings = Settings { new_project: new_project_prefs(&project), ..self.settings.clone() };
            self.set_settings(&ctx, settings);
        }
        if let Some(settings) = self.preferences.ui(&ctx) {
            self.set_settings(&ctx, settings);
        }
        self.ask_ui(&ctx);
        self.new_ui(&ctx);
        // File dialogs start in this project's folder (or its file's).
        dialogs::set_folders(dialogs::Folders { project: self.doc.graph.project.folder_for(self.doc.path.as_deref()), projects: self.settings.files.project_dir.clone() });
        egui::Panel::right("properties")
            .default_size(380.0)
            .min_size(280.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("Properties");
                ui.separator();
                self.props.layer = self.probe.as_ref().map(|(n, img)| (n.clone(), img.window(), img.format));
                self.props.input_layers = self.probe.as_ref().map(|(n, img)| (n.clone(), img.layers()));
                self.props.looking_through = self.viewer.viewport3d.looking_through().filter(|_| self.viewer.mode3d);
                if let Some(f) = self.props.ui(ui, &mut self.doc, self.transport.frame) {
                    self.transport.set_play(Play::Stopped);
                    self.transport.seek(f, &self.doc.graph.project);
                }
                // A Look through button: into the 3D view through that
                // camera or light, or back out of it.
                if let Some(n) = self.props.look_through.take() {
                    let vp = &mut self.viewer.viewport3d;
                    if self.props.looking_through.as_ref() == Some(&n) {
                        vp.stop_looking_through();
                    } else {
                        self.viewer.mode3d = true;
                        vp.look_through_node(&n);
                    }
                }
            });
        let error_node = self.error.as_ref().and_then(|e| e.node.clone());
        egui::Panel::bottom("graph")
            .resizable(true)
            .default_size(340.0)
            .min_size(140.0)
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.selectable_value(&mut self.tab, BottomTab::Graph, "Node Graph");
                    let animated = self.doc.selected.as_ref().is_some_and(|id| self.doc.is_node_animated(id));
                    let label = if animated { "Curve Editor ◆" } else { "Curve Editor" };
                    ui.selectable_value(&mut self.tab, BottomTab::Curves, label)
                        .on_hover_text("Keyframe curves of the selected node");
                });
                match self.tab {
                    BottomTab::Graph => self.editor.ui(ui, &mut self.doc, error_node.as_deref()),
                    BottomTab::Curves => {
                        if let Some(f) = self.curves.ui(ui, &mut self.doc, self.transport.frame) {
                            self.transport.set_play(Play::Stopped);
                            self.transport.seek(f, &self.doc.graph.project);
                        }
                    }
                }
            });
        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::Panel::bottom("transport").show(ui, |ui| {
                ui.add_space(4.0);
                let selected = self.doc.selected.as_ref().and_then(|id| self.doc.node(id));
                let keys = self.doc.selected.as_ref().map(|id| self.doc.node_key_frames(id)).unwrap_or_default();
                let weak = selected.filter(|n| n.op == "tracker").map(|n| arclight::track::weak_frames(&n.params)).unwrap_or_default();
                if self.transport.ui(ui, &mut self.doc.graph.project, &self.cached, &keys, &weak) {
                    self.doc.changed();
                }
                ui.add_space(2.0);
            });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let error = self.error.as_ref().map(|e| e.to_string());
                let target = self.doc.view_target();
                let returns_to = self.doc.overridden().and_then(|_| self.doc.viewer());
                self.viewer.ui(
                    ui,
                    Display {
                        image: self.image.as_ref(),
                        gpu: self.image_gpu.as_ref(),
                        error: error.as_deref(),
                        target: target.as_deref(),
                        returns_to: returns_to.as_deref(),
                        frame: self.shown_frame,
                        millis: self.millis,
                        input: self.probe.as_ref(),
                    },
                    &mut self.doc,
                    self.transport.frame,
                );
            });
        });
        // One undo step per finished gesture (drag, typing, click).
        let gesture = ctx.input(|i| i.pointer.any_down()) || ctx.text_edit_focused() || self.props.is_tracking() || self.viewer.is_transforming();
        self.doc.commit(gesture);
    }
}

/// The project shown on a fresh launch: the bundled test clip, transformed
/// over a checkerboard, graded, viewed, and ready to render.
fn starter_graph() -> Graph {
    // The example clip: in the app bundle's Resources on macOS, in
    // share/arclight beside bin/ in a Linux release, else the source
    // checkout this was built from.
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let clip = exe_dir
        .into_iter()
        .flat_map(|d| ["../Resources", "../share/arclight"].map(|r| d.join(r).join("examples/media/testsrc.mp4")))
        .find(|p| p.exists())
        .unwrap_or_else(|| checkout.join("examples/media/testsrc.mp4"));
    // Renders go to the checkout's out/, or, installed, to ~/Movies/Arclight.
    let render = if checkout.is_dir() {
        checkout.join("out/render/starter.####.png")
    } else {
        arclight::config::home().join("Movies/Arclight/starter.####.png")
    };
    let mut g: Graph = serde_json::from_value(json!({
        "project": { "first_frame": 1, "last_frame": 120, "fps": 24.0 },
        "nodes": [
            { "id": "media_in1", "op": "media_in", "pos": [0, 0],
              "params": { "path": clip.to_string_lossy(), "first_frame": 1, "last_frame": 120 } },
            { "id": "transform1", "op": "transform", "inputs": ["media_in1"], "pos": [200, 0],
              "params": {
                  "scale": { "keys": [[1, [0.55, 0.55]], [60, [0.85, 0.85]], [120, [0.7, 0.7]]], "interp": "smooth" },
                  "rotate": { "keys": [[1, -8.0], [120, 10.0]], "interp": "smooth" } } },
            { "id": "checkerboard1", "op": "checkerboard", "pos": [200, 110],
              "params": { "width": 960, "height": 540, "size": 40 } },
            { "id": "merge1", "op": "merge", "inputs": ["transform1", "checkerboard1"], "pos": [400, 40],
              "params": { "operation": "over" } },
            { "id": "grade1", "op": "grade", "inputs": ["merge1"], "pos": [600, 40],
              "params": { "multiply": [1.05, 1.0, 0.92] } },
            { "id": "viewer1", "op": "viewer", "inputs": ["grade1"], "pos": [800, -20] },
            { "id": "media_out1", "op": "media_out", "inputs": ["grade1"], "pos": [800, 100],
              "params": { "path": render.to_string_lossy(), "first_frame": 1, "last_frame": 120, "fps": 24.0 } }
        ]
    }))
    .expect("starter graph");
    if !clip.exists() {
        // Without the bundled clip, start from a pattern instead of an error.
        g.nodes[0] = serde_json::from_value(json!({
            "id": "media_in1", "op": "constant", "pos": [0, 0],
            "params": { "width": 960, "height": 540, "color": [0.9, 0.3, 0.1, 1.0] }
        }))
        .unwrap();
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a crash the open project's unsaved changes go to its autosave
    /// (beside it), where the autosave recovery finds them; with none, or
    /// with the project being changed, nothing is written and the report
    /// says why.
    #[test]
    fn a_crash_saves_the_project_to_its_autosave() {
        let dir = std::env::temp_dir().join(format!("arclight_rescue_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("shot.json");
        let graph: Graph = serde_json::from_value(json!({ "nodes": [{ "id": "c", "op": "constant", "params": {} }] })).unwrap();
        *RESCUE.lock().unwrap() = None;
        assert_eq!(rescue(), Err("there were no unsaved changes".to_string()));
        *RESCUE.lock().unwrap() = Some((Some(project.clone()), Arc::new(graph)));
        let held = RESCUE.lock().unwrap();
        assert!(rescue().unwrap_err().contains("being changed"), "never waits");
        drop(held);
        let saved = rescue().unwrap();
        assert_eq!(saved, dir.join("shot.json.autosave"));
        let back = Graph::from_file(&saved).unwrap();
        assert_eq!(back.nodes[0].id, "c");
        *RESCUE.lock().unwrap() = None;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The player and edit keys need no modifier (⇧Space isn't Space), but
    /// ⌥ with an arrow; Home and F over the 3D view are left to it.
    #[test]
    fn player_keys_take_no_modifier_and_leave_the_3d_view_its_own() {
        let keys = |key, modifiers, over_3d| {
            let ctx = egui::Context::default();
            let mut taken = vec![];
            let event = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
            ctx.run_ui(egui::RawInput { events: vec![event], ..Default::default() }, |ui| taken = ui.input(|i| player_keys(i, over_3d))).drop_without_applying_deltas();
            taken
        };
        for k in [Key::Space, Key::L, Key::Delete, Key::Home, Key::F, Key::ArrowLeft] {
            assert_eq!(keys(k, Modifiers::NONE, false), [k]);
            assert!(keys(k, Modifiers::SHIFT, false).is_empty() && keys(k, Modifiers::COMMAND, false).is_empty(), "{k:?}");
        }
        assert_eq!(keys(Key::ArrowRight, Modifiers::ALT, false), [Key::ArrowRight]);
        assert!(keys(Key::Space, Modifiers::ALT, false).is_empty() && keys(Key::Home, Modifiers::ALT, false).is_empty());
        assert!(keys(Key::Home, Modifiers::NONE, true).is_empty() && keys(Key::F, Modifiers::NONE, true).is_empty());
        assert_eq!(keys(Key::End, Modifiers::NONE, true), [Key::End], "End still goes to the last frame");
    }

    /// Tab over the viewer toggles 2D / 3D; Shift+Tab (or Alt / Ctrl+Tab)
    /// is left alone and kept for whoever wants it.
    #[test]
    fn only_a_plain_tab_is_taken_as_plain() {
        let tab = |modifiers| egui::Event::Key { key: Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers };
        for (modifiers, plain) in [(Modifiers::NONE, true), (Modifiers::SHIFT, false), (Modifiers::ALT, false), (Modifiers::COMMAND, false)] {
            let ctx = egui::Context::default();
            let (mut took, mut left) = (false, false);
            let raw = egui::RawInput { events: vec![tab(modifiers)], ..Default::default() };
            ctx.run_ui(raw, |ui| {
                took = ui.input_mut(|i| consume_plain(i, Key::Tab));
                left = ui.input(|i| i.key_pressed(Key::Tab));
            }).drop_without_applying_deltas();
            assert_eq!((took, left), (plain, !plain), "{modifiers:?}");
        }
    }

    /// New projects and the 3D view as the preferences say; their defaults
    /// as without settings.
    #[test]
    fn new_project_and_3d_view_preferences_apply() {
        use arclight::config::{NewProjectPrefs, View3dPrefs};
        assert_eq!(new_project(&NewProjectPrefs::default()), arclight::graph::Project::default());
        let p = NewProjectPrefs { width: 1280, height: 720, fps: 25.0, first_frame: 1001, last_frame: 1100 };
        assert_eq!(new_project_prefs(&new_project(&p)), p, "Save as defaults keeps what New starts with");
        assert_eq!(new_project(&NewProjectPrefs { last_frame: 3, first_frame: 9, width: 0, ..p.clone() }).last_frame, 9);
        let mut v = viewport3d::Viewport3d::default();
        apply_view3d(&View3dPrefs::default(), &mut v);
        assert_eq!((v.nav, v.default_shading), (viewport3d::Navigation::default(), None));
        apply_view3d(&View3dPrefs { emulate_three_button: true, trackpad: "pan".into(), zoom_to_pointer: false, invert_zoom: true, orbit_selection: false, default_shading: "lit".into() }, &mut v);
        assert_eq!(v.nav, viewport3d::Navigation { emulate_three_button: true, trackpad_pans: true, zoom_to_pointer: false, invert_zoom: true, orbit_selection: false });
        assert_eq!(v.default_shading, Some(viewport3d::Shading::Lit));
    }

    /// File ▸ New's folder: <Projects folder>/<name> while the name is
    /// typed, until one is typed or chosen; none when unticked (and by
    /// default without a Projects folder).
    #[test]
    fn a_new_projects_folder_follows_its_name() {
        let mut ask = NewProjectAsk::new(Some("/shows".into()));
        assert_eq!((ask.use_folder, ask.chosen()), (true, Some("/shows/Untitled".into())));
        ask.name = "Spot 30/s".into();
        ask.follow_name(Some(std::path::Path::new("/shows")));
        assert_eq!(ask.chosen(), Some("/shows/Spot 30_s".into()), "a slash can't make a folder deeper");
        (ask.folder, ask.folder_typed) = ("/elsewhere/spot".into(), true);
        ask.name = "Spot 15".into();
        ask.follow_name(Some(std::path::Path::new("/shows")));
        assert_eq!(ask.chosen(), Some("/elsewhere/spot".into()), "typed: kept");
        ask.use_folder = false;
        assert_eq!(ask.chosen(), None);
        let plain = NewProjectAsk::new(None);
        assert_eq!((plain.use_folder, plain.chosen()), (false, None));
    }

    /// An autosave goes beside the project as <name>.autosave, never over
    /// it; one not saved yet in the state dir; a folder it can't write to
    /// sends it to the state dir under the project's name.
    #[test]
    fn autosaves_go_beside_the_project_never_over_it() {
        let dir = std::env::temp_dir().join(format!("arclight_autosave_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("comp.json");
        std::fs::write(&project, "the project").unwrap();
        assert_eq!(autosave_path(Some(&project)), dir.join("comp.json.autosave"));
        assert_eq!(write_autosave(Some(&project), "{\"nodes\": []}").unwrap(), dir.join("comp.json.autosave"));
        assert_eq!(std::fs::read_to_string(&project).unwrap(), "the project");
        assert_eq!(std::fs::read_to_string(dir.join("comp.json.autosave")).unwrap(), "{\"nodes\": []}");
        assert!(!dir.join("comp.json.autosave.tmp").exists() && !dir.join("comp.autosave.tmp").exists());
        assert!(autosave_path(None).starts_with(arclight::config::state_dir()));
        let gone = dir.join("no such folder").join("x").join("shot.json");
        let fallback = arclight::config::state_dir().join("autosave").join("shot.json.autosave");
        let _ = std::fs::write(dir.join("no such folder"), "a file, so no folder can be made here");
        assert_eq!(write_autosave(Some(&gone), "{}").unwrap(), fallback);
        std::fs::remove_file(&fallback).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bridged calls run off the UI thread, in order (each seeing the last
    /// one's edit), and an edit made in the window meanwhile isn't undone.
    #[test]
    fn bridged_calls_run_in_order_off_the_ui_thread() {
        let dir = std::env::temp_dir().join(format!("arclight-gui-bridge-test-{}", std::process::id()));
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let graph: Graph = serde_json::from_value(json!({ "nodes": [{ "id": "emit", "op": "particle_emitter", "params": { "rate": 20 } }] })).unwrap();
        let ctx = egui::Context::default();
        let mut app = App::new(ctx.clone(), Doc::new(graph, None));
        let inst = app.bridge.as_ref().expect("a bridge").instance.clone();
        let client = std::thread::spawn(move || {
            let set = bridge::call(&inst, "set_params", &json!({ "node": "emit", "params": { "rate": 5 } }));
            let read = bridge::call(&inst, "describe_node", &json!({ "node": "emit" }));
            let parts = bridge::call(&inst, "particles", &json!({ "node": "emit", "frame": 10 }));
            (set, read, parts)
        });
        while !client.is_finished() {
            app.handle_bridge(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (set, read, parts) = client.join().unwrap();
        assert!(set.is_ok(), "{set:?}");
        assert_eq!(read.unwrap()["params"]["rate"], 5.0, "the next call sees the edit");
        assert!(parts.unwrap()["total"].as_u64() > Some(0));
        assert_eq!((app.doc.node("emit").unwrap().params["rate"].clone(), app.doc.can_undo()), (json!(5.0), true));
        // A change sent before an edit in the window is refused.
        let stale = app.doc.revision - 1;
        let mut other = app.doc.graph.clone();
        other.nodes.clear();
        let done = bridge_calls::Done { result: Ok(json!({})), graph: other, changed: true, path: None };
        assert!(app.finish_call("delete_node", done, stale).unwrap_err().to_string().contains("edited in the ArclightVFX window"));
        assert!(app.doc.node("emit").is_some());
        drop(app);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn view_prefs_come_from_the_viewer_and_set_it_up() {
        let mut viewer = Viewer::default();
        let mut transport = Transport::default();
        assert_eq!(view_prefs(&viewer, &transport), ViewPrefs::default(), "the defaults are the viewer's and the transport's");
        viewer.show_frame_size = false;
        viewer.viewport3d.set_shading(viewport3d::Shading::Lit);
        viewer.viewport3d.view.wireframe = true;
        viewer.viewport3d.view.shadows = true;
        transport.sound = false;
        let prefs = view_prefs(&viewer, &transport);
        assert_eq!(prefs, ViewPrefs { show_frame_size: false, shading: Some("lit".into()), wireframe: true, lit_shadows: true, sound: false });
        // Next run: the labels off, and a project without its own 3D view
        // starts Lit, wireframe, with shadows.
        let mut next = Viewer::default();
        apply_prefs(&prefs, &mut next);
        let new = next.viewport3d.new_view;
        assert!(!next.show_frame_size && new.shading == Some(viewport3d::Shading::Lit) && new.wireframe && new.shadows);
        apply_prefs(&ViewPrefs { shading: Some("solid".into()), ..ViewPrefs::default() }, &mut next);
        assert_eq!(next.viewport3d.new_view.shading, Some(viewport3d::Shading::Solid));
        assert!(next.show_frame_size);
    }

    #[test]
    fn the_about_logo_shows_square_and_whole() {
        let size = egui::vec2(256.0, 256.0);
        // Wider than tall artwork, as the bundled one is.
        let art = egui::Rect::from_min_max(egui::pos2(10.0 / 256.0, 40.0 / 256.0), egui::pos2(246.0 / 256.0, 220.0 / 256.0));
        let uv = square_uv(art, size);
        let px = uv.size() * size;
        assert!((px.x - px.y).abs() < 1e-3 && (px.x - 236.0).abs() < 1e-3, "square, just holding the art: {px:?}");
        assert!(uv.contains_rect(art) && (uv.center() - art.center()).length() < 1e-6, "whole and centered");
        // Near an edge it slides back inside the texture.
        let edge = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 0.25));
        let uv = square_uv(edge, size);
        assert!(uv.min.y >= 0.0 && uv.max.y <= 1.0 + 1e-6 && uv.contains_rect(edge), "{uv:?}");
        // The bundled icon's square region too.
        let icon = app_icon();
        let uv = square_uv(art, egui::vec2(icon.width as f32, icon.height as f32));
        let px = uv.size() * egui::vec2(icon.width as f32, icon.height as f32);
        assert!((px.x - px.y).abs() < 1e-3);
    }
}
