//! Arclight GUI — interactive node editor, viewer, player and properties for
//! the Arclight compositing engine.
//!
//! Layout: viewer + transport in the center, node graph along the bottom,
//! properties on the right.

mod doc;
mod node_editor;
mod properties;
mod transport;
mod viewer;
mod worker;

use arclight::graph::{Graph, NodeError};
use arclight::image::Image;
use doc::Doc;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use node_editor::NodeEditor;
use properties::Properties;
use rfd::{MessageButtons, MessageDialog, MessageDialogResult};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use transport::{Play, Transport};
use viewer::{Display, Viewer};
use worker::{Request, Worker};

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const REDO_ALT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);

fn main() -> eframe::Result {
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
            .with_title("Arclight"),
        ..Default::default()
    };
    eframe::run_native(
        "Arclight",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone(), doc)))),
    )
}

struct App {
    doc: Doc,
    editor: NodeEditor,
    viewer: Viewer,
    transport: Transport,
    props: Properties,
    worker: Worker,
    /// (revision, target, frame) of the last request sent to the worker.
    requested: Option<(u64, String, i64)>,
    image: Option<Arc<Image>>,
    error: Option<NodeError>,
    shown_frame: i64,
    millis: f32,
    cached: Vec<i64>,
    title: String,
    status: Option<String>,
    /// `ARCLIGHT_SCREENSHOT=<png>`: save one screenshot of the window after
    /// `ARCLIGHT_SCREENSHOT_DELAY` seconds (default 2), then quit. For docs/CI.
    screenshot: Option<(PathBuf, std::time::Instant, f32)>,
}

impl App {
    fn new(ctx: egui::Context, doc: Doc) -> App {
        let mut transport = Transport::default();
        transport.frame = doc.graph.project.first_frame;
        App {
            doc,
            editor: NodeEditor::default(),
            viewer: Viewer::default(),
            transport,
            props: Properties::default(),
            worker: Worker::spawn(ctx),
            requested: None,
            image: None,
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
        }
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
            if let Some(id) = self.doc.selected.clone() {
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
                self.doc.view_node(&id);
            }
            self.doc.selected = Some(id);
            self.transport.seek(self.doc.graph.project.first_frame, &self.doc.graph.project);
        }
    }

    fn sync_render(&mut self) {
        let target = self.doc.view_target();
        match &target {
            Some(t) => {
                let key = (self.doc.revision, t.clone(), self.transport.frame);
                if self.requested.as_ref() != Some(&key) {
                    self.worker.request(Request {
                        graph: Arc::new(self.doc.graph.clone()),
                        target: t.clone(),
                        frame: self.transport.frame,
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
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.editor.ui(ui, &mut self.doc, error_node.as_deref()));
        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::Panel::bottom("transport").show(ui, |ui| {
                ui.add_space(4.0);
                let keys = self.doc.selected.as_ref().map(|id| self.doc.node_key_frames(id)).unwrap_or_default();
                if self.transport.ui(ui, &mut self.doc.graph.project, &self.cached, &keys) {
                    self.doc.changed();
                }
                ui.add_space(2.0);
            });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let error = self.error.as_ref().map(|e| e.to_string());
                let target = self.doc.view_target();
                self.viewer.ui(
                    ui,
                    Display {
                        image: self.image.as_ref(),
                        error: error.as_deref(),
                        target: target.as_deref(),
                        frame: self.shown_frame,
                        millis: self.millis,
                    },
                );
            });
        });
        // One undo step per finished gesture (drag, typing, click).
        let gesture = ctx.input(|i| i.pointer.any_down()) || ctx.text_edit_focused();
        self.doc.commit(gesture);
    }
}

/// The project shown on a fresh launch: the bundled test clip, transformed
/// over a checkerboard, graded, viewed, and ready to render.
fn starter_graph() -> Graph {
    let clip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/media/testsrc.mp4");
    let render = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out/render/starter.####.png");
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
