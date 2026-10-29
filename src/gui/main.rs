//! Arclight GUI — interactive node editor, viewer, player and properties for
//! the Arclight compositing engine.
//!
//! Layout: viewer + transport in the center, node graph along the bottom,
//! properties on the right.

mod curve_editor;
mod doc;
mod node_editor;
mod overlay;
mod properties;
mod script_jobs;
mod transport;
mod viewer;
mod worker;

use arclight::bridge;
use arclight::graph::{Graph, NodeError};
use arclight::session::Session;
use arclight::image::Image;
use curve_editor::CurveEditor;
use doc::Doc;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use node_editor::NodeEditor;
use properties::Properties;
use script_jobs::ScriptJobs;
use rfd::{MessageButtons, MessageDialog, MessageDialogResult};
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

fn main() -> eframe::Result {
    arclight::plugin::init();
    let doc = match std::env::args_os().nth(1).map(PathBuf::from) {
        Some(path) => match Graph::from_file(&path) {
            Ok(g) => Doc::new(g, Some(path)),
            Err(e) => {
                eprintln!("arclight-gui: {e:#}");
                std::process::exit(1);
            }
        },
        None => Doc::new(starter_graph(), None),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Arclight")
            // Matches arclight.desktop (install.sh), so launchers and the
            // compositor pair the window with its icon.
            .with_app_id("arclight")
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Arclight",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone(), doc)))),
    )
}

/// The window icon (the placeholder in `assets/`).
fn app_icon() -> egui::IconData {
    let png = include_bytes!("../../assets/arclight.png");
    let img = image::load_from_memory(png).expect("bundled icon").to_rgba8();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
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
    props: Properties,
    worker: Worker,
    /// The last request sent to the worker.
    requested: Option<RequestKey>,
    image: Option<Arc<Image>>,
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
    /// Socket that MCP servers and Python scripts (`arclt.attach()`) drive
    /// this window through.
    bridge: Option<bridge::Server>,
    /// Bridged renders running on worker threads.
    scripts: ScriptJobs,
    /// Unsaved changes were dealt with; let the next close request through.
    closing: bool,
}

impl App {
    fn new(ctx: egui::Context, doc: Doc) -> App {
        let wake = ctx.clone();
        let bridge = bridge::Server::start(move || wake.request_repaint())
            .inspect_err(|e| eprintln!("arclight-gui: scripting bridge disabled: {e:#}"))
            .ok();
        let mut transport = Transport::default();
        transport.frame = doc.graph.project.first_frame;
        App {
            doc,
            editor: NodeEditor::default(),
            curves: CurveEditor::default(),
            tab: BottomTab::Graph,
            viewer: Viewer::default(),
            transport,
            props: Properties::default(),
            worker: Worker::spawn(ctx),
            requested: None,
            image: None,
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
            bridge,
            scripts: ScriptJobs::default(),
            closing: false,
        }
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
        let viewer = self.doc.graph.nodes.iter().find(|n| n.op == "viewer");
        let viewing = self.doc.viewed();
        json!({
            "selected": self.doc.selected,
            "viewer": viewer.map(|v| &v.id),
            "viewing": viewing,
            "frame": self.transport.frame,
            "playing": self.transport.play != Play::Stopped,
        })
    }

    /// Run tool calls that arrived over the bridge. Renders go to worker
    /// threads (on a snapshot of the graph); everything else runs here.
    fn handle_bridge(&mut self, ctx: &egui::Context) {
        let Some(server) = &self.bridge else { return };
        for req in server.poll() {
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
            let result = self.bridge_call(&req.tool, &req.arguments).map_err(|e| format!("{e:#}"));
            req.reply(result);
        }
    }

    /// Apply one bridged tool to the document: the tool runs on a session
    /// over a copy of the graph, and a changed graph comes back as one undo step.
    fn bridge_call(&mut self, tool: &str, args: &Value) -> anyhow::Result<Value> {
        if arclight::mcp::is_gui_tool(tool) {
            return self.view_call(tool, args);
        }
        match tool {
            "undo" | "redo" => {
                self.doc.commit(false);
                let able = if tool == "undo" { self.doc.can_undo() } else { self.doc.can_redo() };
                if tool == "undo" { self.undo() } else { self.redo() }
                return Ok(json!({ if tool == "undo" { "undone" } else { "redone" }: able }));
            }
            "load_graph" | "new_graph" if self.doc.dirty => {
                anyhow::bail!("the Arclight window has unsaved changes; save them first (save_graph)")
            }
            _ => {}
        }
        let mut session = Session::new(self.doc.graph.clone());
        session.path = self.doc.path.clone();
        session.dirty = self.doc.dirty;
        let out = arclight::mcp::call_tool(&mut session, tool, args)?;
        match tool {
            "load_graph" | "new_graph" => self.load(Doc::new(session.graph, session.path)),
            "save_graph" => {
                self.doc.path = session.path;
                self.doc.dirty = false;
            }
            _ if session.graph != self.doc.graph => {
                self.doc.commit(false);
                self.doc.graph = session.graph;
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
            std::process::exit(0);
        })));
    }

    fn load(&mut self, doc: Doc) {
        self.transport = Transport::default();
        self.transport.frame = doc.graph.project.first_frame;
        self.doc = doc;
        self.editor.fit();
        self.curves = CurveEditor::default();
        self.viewer.fit();
        self.props = Properties::default();
        self.requested = None;
    }

    /// Ask before discarding unsaved changes. True = go ahead.
    fn confirm_discard(&self) -> bool {
        !self.doc.dirty
            || MessageDialog::new()
                .set_title("Unsaved changes")
                .set_description("Discard unsaved changes to this project?")
                .set_buttons(MessageButtons::YesNo)
                .show()
                == MessageDialogResult::Yes
    }

    /// On a close request with unsaved changes, ask whether to save them
    /// first; Cancel (or a cancelled save) keeps the window open.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.closing || !self.doc.dirty {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        let (save, discard) = ("Save".to_string(), "Don't Save".to_string());
        let answer = MessageDialog::new()
            .set_title("Unsaved changes")
            .set_description("Save changes to this project before closing?")
            .set_buttons(MessageButtons::YesNoCancelCustom(save.clone(), discard.clone(), "Cancel".into()))
            .show();
        let save_first = match answer {
            MessageDialogResult::Yes => true,
            MessageDialogResult::Custom(c) if c == save => true,
            MessageDialogResult::No => false,
            MessageDialogResult::Custom(c) if c == discard => false,
            _ => return,
        };
        if save_first {
            self.save(false);
            if self.doc.dirty {
                return;
            }
        }
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn new_doc(&mut self) {
        if self.confirm_discard() {
            self.load(Doc::new(Graph::default(), None));
        }
    }

    fn open(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        let Some(path) = rfd::FileDialog::new().add_filter("Arclight graph", &["json"]).pick_file() else {
            return;
        };
        match Graph::from_file(&path) {
            Ok(g) => self.load(Doc::new(g, Some(path))),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn save(&mut self, save_as: bool) {
        let path = match (&self.doc.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => match rfd::FileDialog::new()
                .add_filter("Arclight graph", &["json"])
                .set_file_name("comp.json")
                .save_file()
            {
                Some(p) => p,
                None => return,
            },
        };
        match std::fs::write(&path, self.doc.graph.to_json()) {
            Ok(()) => {
                self.doc.path = Some(path);
                self.doc.dirty = false;
                self.status = None;
            }
            Err(e) => self.status = Some(format!("saving {}: {e}", path.display())),
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
        if ctx.text_edit_focused() {
            // Let the focused text field use ⌘Z for its own text undo.
            return;
        }
        // Redo first: shortcut matching ignores an extra Shift.
        if ctx.input_mut(|i| i.consume_shortcut(&REDO) || i.consume_shortcut(&REDO_ALT)) {
            self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&UNDO)) {
            self.undo();
        }
        // ⌘A: every vertex of the selected shape.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::A)) {
            self.viewer.select_all_points(&self.doc);
        }
        // Shift+E before any plain-E binding: shortcut matching ignores Shift.
        if ctx.input_mut(|i| i.consume_shortcut(&NODE_BROWSER)) {
            self.tab = BottomTab::Graph;
            self.editor.open_browser(ctx.input(|i| i.pointer.hover_pos()));
        }
        let project = self.doc.graph.project.clone();
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.command);
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
            } else if let Some(id) = self.doc.selected.clone() {
                self.doc.delete_node(&id);
            }
        }
        if pressed(Key::V) {
            if let Some(id) = self.doc.selected.clone() {
                self.doc.view_node(&id);
            }
        }
        if pressed(Key::F) {
            let ptr = ctx.input(|i| i.pointer.hover_pos());
            if ptr.is_some_and(|p| self.viewer.rect.contains(p)) {
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
                    if self.confirm_discard() {
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

    fn sync_render(&mut self) {
        let target = self.doc.view_target();
        match &target {
            Some(t) => {
                // A selected node with a Transform overlay wants its input's
                // data window; one with a color picker, its input's pixels;
                // a Corner Pin, its input's frame.
                let probe = self.doc.selected.as_ref().and_then(|id| self.doc.node(id)).filter(|n| {
                    use arclight::ops::Overlay;
                    arclight::ops::info(&n.op).is_some_and(|i| matches!(i.overlay, Overlay::Transform { .. } | Overlay::Pick(_) | Overlay::CornerPin { .. }))
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
                        self.error = None;
                        if new_frame {
                            self.transport.frame_shown();
                        }
                    }
                    Err(e) => {
                        self.image = None;
                        self.error = Some(e);
                    }
                }
                self.shown_frame = resp.frame;
                self.millis = resp.millis;
                self.cached = resp.cached_frames;
                self.probe = resp.probe;
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".to_string(), |n| n.to_string_lossy().into_owned());
        let title = format!("{}{name} — Arclight", if self.doc.dirty { "• " } else { "" });
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
                if ui.button("Import media…").clicked() {
                    ui.close();
                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                        let id = self.doc.add_node("media_in", [0.0, -90.0]);
                        properties::apply_media(&mut self.doc, &id, &p);
                        self.doc.selected = Some(id);
                    }
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
                if ui.add_enabled(sel.is_some(), egui::Button::new("Delete node").shortcut_text("⌫")).clicked() {
                    self.doc.delete_node(sel.as_deref().unwrap());
                    ui.close();
                }
                if ui.add_enabled(sel.is_some(), egui::Button::new("View node").shortcut_text("V")).clicked() {
                    self.doc.view_node(sel.as_deref().unwrap());
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
            });
            ui.menu_button("Help", |ui| {
                ui.label(
                    "Node graph: right-click to add · drag ports to connect · \
                     double-click or V to view · ⌫ delete · F frame all\n\
                     Viewer: drag to pan · scroll / pinch to zoom · double-click or F to fit\n\
                     Player: Space play/pause · J / K / L back / stop / forward · \
                     ← → step · ⌥← ⌥→ previous / next key · Home / End\n\
                     Keyframes: click a parameter's ◇ to set or remove a key at the current frame; \
                     editing an animated parameter keys it automatically; right-click ◇ for \
                     interpolation and key navigation\n\
                     Curve editor: click / Shift-click / box-drag to select keys · drag to move · \
                     double-click a curve to add a key · ⌫ delete · scroll pan · ⌘-scroll or \
                     pinch zoom (⌥ value only, ⇧ time only) · middle-drag pan · F frame all\n\
                     Undo / redo: ⌘Z / ⇧⌘Z",
                );
            });
            if let Some(s) = &self.status {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(235, 100, 90), s);
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_close(&ctx);
        self.handle_bridge(&ctx);
        self.handle_drops(&ctx);
        self.shortcuts(&ctx);
        if self.transport.play != Play::Stopped {
            self.transport.tick(&self.doc.graph.project);
            ctx.request_repaint();
        }
        self.sync_render();
        self.update_title(&ctx);
        self.maybe_screenshot(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::right("properties")
            .default_size(380.0)
            .min_size(280.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("Properties");
                ui.separator();
                self.props.layer = self.probe.as_ref().map(|(n, img)| (n.clone(), img.window(), img.format));
                if let Some(f) = self.props.ui(ui, &mut self.doc, self.transport.frame) {
                    self.transport.set_play(Play::Stopped);
                    self.transport.seek(f, &self.doc.graph.project);
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
                let returns_to = self.doc.overridden().and_then(|_| self.doc.graph.nodes.iter().find(|n| n.op == "viewer")).map(|v| v.id.clone());
                self.viewer.ui(
                    ui,
                    Display {
                        image: self.image.as_ref(),
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
        let gesture = ctx.input(|i| i.pointer.any_down()) || ctx.text_edit_focused() || self.props.is_tracking();
        self.doc.commit(gesture);
    }
}

/// The project shown on a fresh launch: the bundled test clip, transformed
/// over a checkerboard, graded, viewed, and ready to render.
fn starter_graph() -> Graph {
    // The example clip: in the app bundle's Resources on macOS, else the
    // source checkout this was built from.
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let clip = exe_dir
        .map(|d| d.join("../Resources/examples/media/testsrc.mp4"))
        .filter(|p| p.exists())
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
