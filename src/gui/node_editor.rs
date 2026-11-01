//! The node graph editor.
//!
//! Inputs sit on a node's left edge, its output on the right. Drag from an
//! output to an input (or the reverse) to connect, or drop the wire anywhere
//! on a node: from an output it goes into the node's first free input that
//! takes it (else its first that does; a mask only on its own port), from an
//! input it takes the node's output. Drag a connected input away to reroute or disconnect. Drop a
//! wire dragged from an output onto another wire to join the two with a new
//! Merge (the dragged node into A, over the wire's source in B). Inputs are
//! yellow squares, outputs green circles.
//! Right-click the canvas, or press Shift+E (or Shift+Tab over the graph) for a
//! searchable node browser, to add nodes; over a wire, the new node goes into
//! it. Over the graph Shift+R, Shift+T and Shift+M add a Media In, a
//! Transform and a Merge at the pointer, wired the same way. Drop an
//! unconnected node on a wire to splice it in; shake a node while dragging
//! it to pull it out of its wires. Optional ports draw hollow. The
//! minimap (lower right) pans on click/drag, zooms on scroll, fits on
//! double-click. The Fit button (top right) or F frames every node.
//!
//! Click selects a node, Shift- or Cmd/Ctrl-click adds or removes one, and
//! a drag on empty space draws a selection box (Shift adds); dragging any
//! selected node moves them all. Middle-drag or Alt-drag pans. Backdrops
//! (group boxes) sit behind everything: drag the title bar to move one with
//! the nodes fully inside it, a corner to resize it; double-click the title
//! to rename it, click the swatch for its color.

use crate::doc::{Doc, NODE_H, NODE_W};
use crate::properties::{backdrop_color32, backdrop_palette};
use arclight::graph::Backdrop;
use arclight::ops::{self, Category};
use arclight::ops::OpInfo;
use eframe::egui::{
    self, epaint::CubicBezierShape, Align2, Color32, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2,
};

const PORT_R: f32 = 5.5;
/// The closest a fit (on opening, F, the Fit button, the minimap's
/// double-click) zooms.
const FIT_MAX_ZOOM: f32 = 0.7;
/// The editor's zoom limits.
const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 3.0;
const IN_PORT_COLOR: Color32 = Color32::from_rgb(235, 205, 60);
const IN_PORT_HOT: Color32 = Color32::from_rgb(255, 240, 150);
const OUT_PORT_COLOR: Color32 = Color32::from_rgb(140, 225, 140);
const OUT_PORT_HOT: Color32 = Color32::from_rgb(210, 255, 210);
/// How close (screen points) the pointer must be to a wire to pick it.
const WIRE_GRAB: f32 = 7.0;
/// A shake: this many reversals of drag direction within `SHAKE_SECS`.
const SHAKE_FLIPS: usize = 3;
const SHAKE_SECS: f64 = 0.7;
const MINIMAP: Vec2 = Vec2::new(190.0, 120.0);

/// What a wire's right-click menu offers.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WireAction {
    CameraSetup,
    InsertNode,
    Delete,
}

impl WireAction {
    const ALL: [(WireAction, &'static str, &'static str); 3] = [
        (
            WireAction::CameraSetup,
            "Insert 3D Camera Setup",
            "Add a Camera, a Scene and a Scanline Render here: the picture on this wire becomes the render's background, \
             and the render goes on to where the wire went. Add Cards, meshes and lights to the Scene.",
        ),
        (WireAction::InsertNode, "Insert Node…", "Pick a node to put into this wire"),
        (WireAction::Delete, "Delete connection", "Disconnect this wire (⌫ with it selected)"),
    ];
}

/// A wire: `src`'s output into input `port` of `dst`.
#[derive(Clone, Debug, PartialEq)]
struct WireRef {
    src: String,
    dst: String,
    port: usize,
}

/// Direction reversals while dragging a node, to spot a shake.
#[derive(Default)]
struct Shake {
    node: String,
    last: Vec2,
    flips: Vec<f64>,
    done: bool,
}

enum Wire {
    /// Dragging from a node's output toward an input.
    FromOutput(String),
    /// Dragging from an unconnected input toward an output.
    FromInput(String, usize),
}

pub struct NodeEditor {
    pan: Vec2,
    zoom: f32,
    wire: Option<Wire>,
    fit_pending: bool,
    /// The editor's size when the view was last fitted, while it hasn't
    /// been moved since: a new size (the window's real one after the first
    /// frames, a new interface scale, a resize) fits it again.
    fitted_size: Option<Vec2>,
    browser: Option<Browser>,
    /// The wire a node added from the menu or browser goes into.
    target_wire: Option<WireRef>,
    /// The wire a dragged, unconnected node would be dropped into.
    drop_wire: Option<WireRef>,
    shake: Shake,
    /// The minimap's graph bounds, held while dragging in it.
    minimap_bounds: Option<Rect>,
    /// A selection box being dragged: where it started (screen) and whether
    /// it adds to the selection (Shift).
    select_box: Option<(Pos2, bool)>,
    /// A backdrop being moved by its title bar: it, and the nodes and
    /// backdrops fully inside it when the drag started.
    backdrop_drag: Option<(String, Vec<String>, Vec<String>)>,
    /// A backdrop's title being edited in place: id, text, and whether the
    /// field still needs focus.
    title_edit: Option<(String, String, bool)>,
    /// A wire's right-click menu: the wire, and where it opened (screen).
    wire_menu: Option<(WireRef, Pos2)>,
    pub rect: Rect,
    /// Moved nodes land on the grid (Preferences ▸ Interface).
    pub snap_to_grid: bool,
    /// Wires drawn straight rather than curved (Preferences ▸ Interface).
    pub straight_wires: bool,
}

/// The zoom and pan framing `bounds` (graph coordinates) centered in a
/// view `size` big, with a margin. Small graphs stop at `FIT_MAX_ZOOM`:
/// fitted any closer, a few nodes fill the editor (scroll or pinch to zoom
/// in further); huge ones at the editor's farthest zoom.
fn fit_view(bounds: Rect, size: Vec2) -> (f32, Vec2) {
    let b = bounds.expand(30.0);
    let zoom = (size.x / b.width()).min(size.y / b.height()).clamp(ZOOM_MIN, FIT_MAX_ZOOM);
    (zoom, size / 2.0 - b.center().to_vec2() * zoom)
}

/// Every node and backdrop, in graph coordinates.
fn content_bounds(doc: &Doc) -> Rect {
    let nodes = doc.graph.nodes.iter().map(|n| {
        let (a, b) = Doc::node_box(n);
        Rect::from_min_max(Pos2::new(a[0], a[1]), Pos2::new(b[0], b[1]))
    });
    let boxes = doc.graph.backdrops.iter().map(|b| Rect::from_min_size(Pos2::new(b.pos[0], b.pos[1]), Vec2::new(b.size[0], b.size[1])));
    nodes.chain(boxes).fold(Rect::NOTHING, |acc, r| acc.union(r))
}

/// A backdrop's smallest size (graph units).
const BACKDROP_MIN: Vec2 = Vec2::new(120.0, 60.0);
const SELECTED: Color32 = Color32::from_rgb(250, 190, 70);

/// The node browser's width, its list's tallest, and the room its search
/// field and margins take (points).
const BROWSER_WIDTH: f32 = 340.0;
const BROWSER_LIST_MAX: f32 = 560.0;
const BROWSER_CHROME: f32 = 120.0;

/// A node browser row: the node's name, then the start of its help (its
/// first sentence, cut to fit), smaller and dimmer.
fn browser_row(ui: &egui::Ui, op: &OpInfo) -> egui::text::LayoutJob {
    let style = ui.style();
    let mut job = egui::text::LayoutJob::default();
    egui::RichText::new(op.label).size(14.0).append_to(&mut job, style, egui::FontSelection::Default, egui::Align::Center);
    let first = op.help.split(". ").next().unwrap_or("").trim_end_matches('.');
    // Roughly what fits beside the name (characters).
    let room = ((BROWSER_WIDTH - 40.0 - op.label.chars().count() as f32 * 7.5) / 5.6).max(0.0) as usize;
    let mut text: String = first.chars().take(room).collect();
    if first.chars().count() > room {
        text = text.trim_end().to_string() + "…";
    }
    if !text.is_empty() {
        egui::RichText::new(format!("   {text}")).size(11.0).color(style.visuals.weak_text_color()).append_to(&mut job, style, egui::FontSelection::Default, egui::Align::Center);
    }
    job
}

/// The Shift+E node browser: a search field over every op.
struct Browser {
    query: String,
    /// Where the browser opened, in screen coordinates; nodes are placed here.
    at: Pos2,
    /// Index into the filtered list.
    sel: usize,
    /// Focus the search field on the first frame only.
    just_opened: bool,
}

impl Default for NodeEditor {
    fn default() -> Self {
        NodeEditor {
            pan: Vec2::new(40.0, 40.0),
            zoom: 1.0,
            wire: None,
            fit_pending: true,
            fitted_size: None,
            browser: None,
            target_wire: None,
            drop_wire: None,
            shake: Shake::default(),
            minimap_bounds: None,
            select_box: None,
            backdrop_drag: None,
            title_edit: None,
            wire_menu: None,
            rect: Rect::NOTHING,
            snap_to_grid: false,
            straight_wires: false,
        }
    }
}

pub fn category_color(c: Category) -> Color32 {
    match c {
        Category::Source => Color32::from_rgb(58, 104, 70),
        Category::Color => Color32::from_rgb(104, 76, 136),
        Category::Channel => Color32::from_rgb(52, 118, 124),
        Category::Keyer => Color32::from_rgb(96, 124, 44),
        Category::Merge => Color32::from_rgb(56, 86, 136),
        Category::Shape => Color32::from_rgb(140, 70, 108),
        Category::Transform => Color32::from_rgb(140, 104, 48),
        Category::Filter => Color32::from_rgb(132, 74, 58),
        Category::Output => Color32::from_rgb(84, 86, 92),
        Category::Scene => Color32::from_rgb(48, 110, 150),
    }
}

const CATEGORIES: [(Category, &str); 10] = [
    (Category::Source, "Source"),
    (Category::Color, "Color"),
    (Category::Channel, "Channel"),
    (Category::Keyer, "Keyer"),
    (Category::Merge, "Merge"),
    (Category::Shape, "Shape"),
    (Category::Transform, "Transform"),
    (Category::Filter, "Filter"),
    (Category::Output, "Output"),
    (Category::Scene, "3D"),
];

fn category_name(c: Category) -> &'static str {
    CATEGORIES.iter().find(|(k, _)| *k == c).map_or("", |(_, n)| n)
}

fn has_output(op: &str) -> bool {
    op != "viewer"
}

/// Wires carrying a 3D scene (and the ports taking one) are drawn in this.
const SCENE_COLOR: Color32 = Color32::from_rgb(90, 175, 225);
/// A mask input (the generic one of masked ops) and its label.
const MASK_PORT_COLOR: Color32 = Color32::from_rgb(200, 150, 235);

/// Whether `src`'s output may go into input `port` of `dst`: a scene only
/// into a scene port, an image only into an image port.
fn fits(doc: &Doc, src: &str, dst: &str, port: usize) -> bool {
    match (doc.node(src), doc.node(dst)) {
        (Some(s), Some(d)) => ops::wire_error(&s.op, &d.op, port).is_none(),
        _ => false,
    }
}

/// Whether `node` may go into the wire from `src` to input `port` of `dst`.
fn fits_between(doc: &Doc, node: &str, src: &str, dst: &str, port: usize) -> bool {
    fits(doc, src, node, 0) && fits(doc, node, dst, port)
}

/// The input of `dst` a wire from `src`'s output dropped on `dst`'s body
/// goes into: its first free input that takes it, else its first that
/// takes it (that one's wire replaced); never the mask (only on its own
/// port). None when it would close a cycle (`src` itself included: a node
/// is upstream of itself), or no input takes it.
fn body_input(doc: &Doc, src: &str, dst: &str) -> Option<usize> {
    if doc.is_upstream(dst, src) {
        return None;
    }
    let n = doc.node(dst)?;
    let takes: Vec<usize> = (0..n.inputs.len()).filter(|&i| !is_mask_port(&n.op, i) && fits(doc, src, dst, i)).collect();
    takes.iter().find(|&&i| n.inputs[i].is_empty()).or(takes.first()).copied()
}

/// Whether input `port` of `dst`, dragged onto `src`'s body, takes `src`'s
/// output (not when that closes a cycle, `dst` itself included).
fn body_output(doc: &Doc, src: &str, dst: &str, port: usize) -> bool {
    doc.node(src).is_some_and(|n| has_output(&n.op)) && fits(doc, src, dst, port) && !doc.is_upstream(dst, src)
}

/// Whether `node`'s output is a scene.
fn is_scene(doc: &Doc, node: &str) -> bool {
    doc.node(node).is_some_and(|n| ops::output_kind(&n.op) == ops::Kind::Scene)
}

/// Ops matching `query`, in category order. Every word of the query must
/// appear in the op's label, name or category; label-prefix matches sort
/// first within the list.
fn browse(query: &str) -> Vec<&'static OpInfo> {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(bool, usize, &'static OpInfo)> = CATEGORIES
        .iter()
        .flat_map(|(cat, _)| ops::all().into_iter().filter(move |o| o.category == *cat))
        .enumerate()
        .filter_map(|(order, op)| {
            let hay = format!("{} {} {}", op.label, op.name, category_name(op.category)).to_lowercase();
            words.iter().all(|w| hay.contains(w)).then(|| {
                let prefix = !q.is_empty() && op.label.to_lowercase().starts_with(&q);
                (!prefix, order, op)
            })
        })
        .collect();
    hits.sort_by_key(|h| (h.0, h.1));
    hits.into_iter().map(|h| h.2).collect()
}

impl NodeEditor {
    pub fn fit(&mut self) {
        self.fit_pending = true;
    }

    /// Scroll so the node at graph position `pos` is centered, unless it's
    /// already fully on screen.
    pub fn reveal(&mut self, pos: [f32; 2]) {
        let node = Rect::from_min_size(Pos2::new(pos[0], pos[1]), Vec2::new(NODE_W, NODE_H));
        let on_screen = Rect::from_min_max(self.to_screen(node.min), self.to_screen(node.max));
        if !self.rect.is_positive() || self.rect.contains_rect(on_screen) {
            return;
        }
        self.pan = self.rect.size() / 2.0 - node.center().to_vec2() * self.zoom;
        self.fitted_size = None;
    }

    /// The middle of the view, in graph coordinates (where a node added from
    /// a menu goes).
    pub fn view_center(&self) -> [f32; 2] {
        let c = if self.rect.is_positive() { self.to_graph(self.rect.center()) } else { Pos2::new(200.0, 200.0) };
        [c.x - NODE_W / 2.0, c.y - NODE_H / 2.0]
    }

    /// The node browser is open (it takes Esc and the arrow keys).
    pub fn browser_open(&self) -> bool {
        self.browser.is_some()
    }

    /// Open the node browser at the pointer, or mid-editor if the pointer
    /// is elsewhere.
    pub fn open_browser(&mut self, pointer: Option<Pos2>) {
        let at = pointer.filter(|p| self.rect.contains(*p)).unwrap_or(self.rect.center());
        self.browser = Some(Browser { query: String::new(), at, sel: 0, just_opened: true });
    }

    /// Add `op` at graph position `at`. Into the target wire when there is
    /// one; else, with a node selected, the new node takes it as its first
    /// input.
    ///
    /// A Camera comes with what it needs (`Doc::add_camera_setup`): a
    /// Scene and a Scanline Render, which goes into the target wire, else
    /// just before the Viewer (made if there's none); the Camera is
    /// selected. `bare` (Shift held): just the Camera. With a
    /// Scanline Render selected, a node making a scene or particles goes
    /// into its spare input.
    fn place(&mut self, doc: &mut Doc, op: &OpInfo, at: Pos2, bare: bool) {
        let upstream = doc.selected.clone();
        if op.name == "camera" && !bare {
            doc.selected_wire = self.target_wire.take().map(|w| (w.src, w.dst, w.port));
            doc.add_camera_setup([at.x, at.y]);
            return;
        }
        let id = doc.add_node(op.name, [at.x, at.y]);
        if let Some(w) = self.target_wire.take() {
            if fits_between(doc, &id, &w.src, &w.dst, w.port) && doc.insert_on_wire(&id, &w.src, &w.dst, w.port) {
                doc.selected = Some(id);
                return;
            }
        }
        // Added with a Scanline Render selected, a scene or particles go
        // into its spare input.
        if let Some(render) = upstream.as_deref().and_then(|u| doc.node(u)).filter(|n| ops::growable(&n.op).is_some()) {
            let (render, spare) = (render.id.clone(), render.inputs.len() - 1);
            if fits(doc, &id, &render, spare) && doc.connect(&id, &render, spare) {
                doc.selected = Some(id);
                return;
            }
        }
        if let Some(up) = upstream {
            if !op.inputs.is_empty() && doc.node(&up).is_some_and(|n| has_output(&n.op)) && fits(doc, &up, &id, 0) {
                doc.connect(&up, &id, 0);
            }
        }
        doc.selected = Some(id);
    }

    /// Keys over the graph (not while typing): D turns the selected nodes
    /// off or on; Shift+Tab opens the node
    /// browser at the pointer, as a right-click does (plain Tab does
    /// nothing here: over the viewer it switches 2D / 3D); Shift+R, Shift+T and Shift+M
    /// add a Media In, a Transform and a Merge there, placed as the browser
    /// would (into the wire under the pointer or the selected one, else
    /// after the selected node).
    fn hotkeys(&mut self, ui: &egui::Ui, doc: &mut Doc) {
        let ctx = ui.ctx();
        if !ui.rect_contains_pointer(self.rect) || ctx.text_edit_focused() || self.browser.is_some() {
            return;
        }
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        // egui read a Tab already, to move keyboard focus (back, with
        // Shift) this frame: called off either way, and a plain Tab lets go
        // of what it focused (no text field had focus, see above). Shift+Tab
        // first: matching a plain Tab ignores Shift.
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab)) {
            self.open_browser(Some(p));
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
            ctx.memory_mut(|m| {
                m.move_focus(egui::FocusDirection::None);
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            return;
        }
        // D: the selected nodes off (bypassed), or on again.
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::D)) {
            doc.toggle_disabled_selection();
            return;
        }
        let keys = [(Key::R, "media_in"), (Key::T, "transform"), (Key::M, "merge")];
        let Some(op) = ctx.input_mut(|i| keys.into_iter().find(|(k, _)| i.consume_key(Modifiers::SHIFT, *k))).and_then(|(_, op)| ops::info(op)) else {
            return;
        };
        let selected = doc.selected_wire.clone().map(|(src, dst, port)| WireRef { src, dst, port });
        self.target_wire = self.wire_at(doc, p, None).or(selected);
        self.place(doc, op, self.to_graph(p), false);
    }

    fn browser_ui(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let into = self.target_wire.as_ref().map(|w| format!("Insert between {} and {}", w.src, w.dst));
        let Some(b) = &mut self.browser else { return };
        let hits = browse(&b.query);
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down && !hits.is_empty() {
            b.sel = (b.sel + 1).min(hits.len() - 1);
        }
        if up {
            b.sel = b.sel.saturating_sub(1);
        }
        let mut chosen = enter.then(|| hits.get(b.sel).copied()).flatten();
        let mut close = esc || enter;

        // Grow upward in the lower half of the screen so the list fits; as
        // tall as there's room for from the pointer, up to a limit.
        let screen = ctx.content_rect();
        let upward = b.at.y > screen.center().y;
        let room = if upward { b.at.y - screen.top() } else { screen.bottom() - b.at.y };
        let list_height = (room - BROWSER_CHROME).clamp(160.0, BROWSER_LIST_MAX);
        let area = egui::Area::new(Id::unique("node_browser"))
            .order(egui::Order::Foreground)
            .fixed_pos(b.at)
            .pivot(if upward { Align2::LEFT_BOTTOM } else { Align2::LEFT_TOP })
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(BROWSER_WIDTH);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let edit = ui.add(egui::TextEdit::singleline(&mut b.query).hint_text("Search nodes…").desired_width(f32::INFINITY));
                    if b.just_opened {
                        edit.request_focus();
                        b.just_opened = false;
                    }
                    if let Some(into) = &into {
                        ui.weak(into);
                    }
                    if edit.changed() {
                        b.sel = 0;
                    }
                    ui.separator();
                    if hits.is_empty() {
                        ui.weak("No matching nodes");
                    }
                    egui::ScrollArea::vertical().max_height(list_height).min_scrolled_height(list_height).show(ui, |ui| {
                        let mut last_cat = None;
                        for (i, op) in hits.iter().enumerate() {
                            if last_cat != Some(op.category) {
                                last_cat = Some(op.category);
                                ui.label(egui::RichText::new(category_name(op.category)).small().color(category_color(op.category).gamma_multiply(1.8)));
                            }
                            let help = if op.name == "camera" {
                                format!("{}\n\nComes with a Scene and a Scanline Render, wired in. Shift: the Camera alone.", op.help)
                            } else {
                                op.help.to_string()
                            };
                            let resp = ui.selectable_label(i == b.sel, browser_row(ui, op)).on_hover_text(help);
                            if i == b.sel && (down || up) {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                chosen = Some(op);
                                close = true;
                            }
                        }
                    });
                });
            });
        // A click anywhere else dismisses the browser.
        if ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() {
            close = true;
        }
        let at = b.at;
        if close {
            self.browser = None;
            if chosen.is_none() {
                self.target_wire = None;
            }
        }
        if let Some(op) = chosen {
            let bare = ctx.input(|i| i.modifiers.shift);
            self.place(doc, op, self.to_graph(at), bare);
        }
    }

    fn to_screen(&self, p: Pos2) -> Pos2 {
        self.rect.min + self.pan + p.to_vec2() * self.zoom
    }

    /// Screen point `p` (in the window the graph is drawn in) in graph
    /// units: where a paste at the pointer lands.
    pub fn graph_point(&self, p: Pos2) -> [f32; 2] {
        let g = self.to_graph(p);
        [g.x, g.y]
    }

    fn to_graph(&self, p: Pos2) -> Pos2 {
        ((p - self.rect.min - self.pan) / self.zoom).to_pos2()
    }

    fn node_rect(&self, pos: [f32; 2]) -> Rect {
        Rect::from_min_size(self.to_screen(Pos2::new(pos[0], pos[1])), Vec2::new(NODE_W, NODE_H) * self.zoom)
    }

    fn in_port(r: Rect, i: usize, n: usize) -> Pos2 {
        Pos2::new(r.left(), r.top() + r.height() * (i + 1) as f32 / (n + 1) as f32)
    }

    fn out_port(r: Rect) -> Pos2 {
        Pos2::new(r.right(), r.center().y)
    }

    /// How far (screen points) from one of a node's `n` input ports it can
    /// be grabbed: within half the gap to the next port, so neighbors (a
    /// Merge's A, B and mask) never overlap.
    fn port_reach(r: Rect, n: usize) -> f32 {
        let gap = r.height() / (n + 1) as f32;
        (PORT_R * 1.5).min(gap * 0.5)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, error_node: Option<&str>) {
        let (rect, bg) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(30));

        if self.fit_pending || self.fitted_size.is_some_and(|s| s != rect.size()) {
            self.fit_to(doc);
        }
        let minimap = self.minimap_rect(doc);
        let view = (self.zoom, self.pan);
        self.navigate(ui, &bg, minimap);
        if (self.zoom, self.pan) != view {
            self.fitted_size = None;
        }
        self.draw_grid(&painter);

        let mods = ui.input(|i| i.modifiers);
        // (Before this frame's clicks, so the one opening it doesn't close it.)
        self.wire_menu_ui(ui.ctx(), doc);
        // A click on a wire selects it; elsewhere it deselects.
        if bg.clicked() {
            match bg.interact_pointer_pos().and_then(|p| self.wire_at(doc, p, None)) {
                Some(w) => doc.select_wire(&w.src, &w.dst, w.port),
                None if !mods.shift && !mods.command => doc.clear_selection(),
                None => {}
            }
        }
        // Right-click on a wire: its menu; elsewhere the node browser.
        if bg.secondary_clicked() {
            match bg.interact_pointer_pos().and_then(|p| Some((self.wire_at(doc, p, None)?, p))) {
                Some((w, p)) => {
                    doc.select_wire(&w.src, &w.dst, w.port);
                    self.wire_menu = Some((w, p));
                }
                None => self.open_browser(bg.interact_pointer_pos()),
            }
        }
        self.hotkeys(ui, doc);
        if let Some(at) = self.browser.as_ref().filter(|b| b.just_opened).map(|b| b.at) {
            self.target_wire = self.wire_at(doc, at, None);
        }
        self.browser_ui(ui.ctx(), doc);

        if doc.graph.nodes.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Right-click to add nodes, or drop image / video files here",
                FontId::proportional(14.0),
                Color32::GRAY,
            );
        }

        self.backdrops(ui, &painter, doc);

        // Snapshot geometry so we can mutate the doc while handling input.
        let geo: Vec<(String, String, Rect, usize)> = doc
            .graph
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.op.clone(), self.node_rect(n.pos.unwrap_or([0.0, 0.0])), n.inputs.len()))
            .collect();
        let rect_of = |id: &str| geo.iter().find(|g| g.0 == id).map(|g| g.2);

        // Wires first, beneath the nodes. The one a node would go into is lit.
        let wire_stroke = Stroke::new(2.0 * self.zoom.sqrt(), Color32::from_gray(150));
        let lit = self.drop_wire.as_ref().or(self.target_wire.as_ref().filter(|_| self.browser.is_some()));
        for n in &doc.graph.nodes {
            let Some(to) = rect_of(&n.id) else { continue };
            for (i, src) in n.inputs.iter().enumerate() {
                if let Some(from) = rect_of(src) {
                    let a = Self::out_port(from);
                    let b = Self::in_port(to, i, n.inputs.len());
                    let on = lit.is_some_and(|w| w.src == *src && w.dst == n.id && w.port == i);
                    let picked = doc.selected_wire.as_ref().is_some_and(|(s, d, p)| s == src && *d == n.id && *p == i);
                    let plain = if is_scene(doc, src) { Stroke::new(wire_stroke.width, SCENE_COLOR) } else { wire_stroke };
                    let stroke = if on || picked { Stroke::new(3.0 * self.zoom.sqrt().max(1.0), SELECTED) } else { plain };
                    painter.add(bezier(a, b, stroke, self.straight_wires));
                }
            }
        }

        // Nodes.
        for (id, op, r, _) in &geo {
            let resp = ui.interact(*r, Id::unique(("node", id)), Sense::click_and_drag());
            let toggle = mods.shift || mods.command;
            // Click: just this node; Shift / Cmd-click: add or remove it. A
            // drag takes an unselected node into the selection first.
            if resp.clicked() || (resp.drag_started_by(egui::PointerButton::Primary) && !doc.is_selected(id)) {
                if toggle {
                    doc.toggle_selected(id);
                } else {
                    doc.select_only(id);
                }
            }
            // Selecting a Viewer shows it, with its own input again.
            if (resp.clicked() || resp.drag_started()) && op == "viewer" && doc.is_selected(id) {
                doc.view_node(id);
            }
            resp.context_menu(|ui| self.node_menu(ui, doc, id));
            if resp.drag_started() {
                self.shake = Shake { node: id.clone(), ..Default::default() };
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                let d = resp.drag_delta() / self.zoom;
                let moving = doc.selection();
                let alone = moving.len() <= 1;
                doc.move_nodes(if alone { std::slice::from_ref(id) } else { &moving }, [d.x, d.y]);
                // Shaking out of wires and dropping into one: one node only.
                let now = ui.input(|i| i.time);
                if alone && self.shake.node == *id && self.shake.shaken(resp.drag_delta(), now) {
                    doc.extract(id);
                }
                // An unconnected node can be dropped into a wire.
                self.drop_wire = None;
                if alone && is_free(doc, id) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.drop_wire = self.wire_at(doc, p, Some(id));
                    }
                }
            }
            if resp.drag_stopped() {
                // The node held lands on the grid, the others moved with it.
                if self.snap_to_grid {
                    if let Some([x, y]) = doc.node(id).and_then(|n| n.pos) {
                        let moving = doc.selection();
                        let alone = moving.len() <= 1;
                        doc.move_nodes(if alone { std::slice::from_ref(id) } else { &moving }, [snap(x) - x, snap(y) - y]);
                    }
                }
                if let Some(w) = self.drop_wire.take().filter(|w| fits_between(doc, id, &w.src, &w.dst, w.port)) {
                    doc.insert_on_wire(id, &w.src, &w.dst, w.port);
                }
            }
            if resp.double_clicked() {
                doc.view_node(id);
            }
            // Its on/off switch (over the node, so it takes the click).
            let off = doc.node(id).is_some_and(|n| n.disabled);
            let switch = ui.interact(self.switch_rect(*r).expand(2.0), Id::unique(("node-switch", id)), Sense::click());
            let tip = if off { "Turned off: passes its input on unchanged (click or D to turn on)" } else { "On (click or D to turn off: bypass it)" };
            if switch.on_hover_text(tip).clicked() {
                doc.set_disabled(std::slice::from_ref(id), !off);
            }
            let selected = doc.is_selected(id);
            self.draw_node(&painter, doc, id, op, *r, selected, error_node == Some(id.as_str()));
        }

        // Ports (interacted after nodes so they take priority over node drags).
        let pointer = ui.input(|i| i.pointer.interact_pos());
        for (id, op, r, n_in) in &geo {
            for i in 0..*n_in {
                let p = Self::in_port(*r, i, *n_in);
                let reach = Self::port_reach(*r, *n_in);
                let resp = ui.interact(Rect::from_center_size(p, Vec2::splat(reach * 2.0)), Id::unique(("in", id, i)), Sense::drag());
                if resp.drag_started() {
                    let upstream = doc.node(id).map(|n| n.inputs[i].clone()).unwrap_or_default();
                    if upstream.is_empty() {
                        self.wire = Some(Wire::FromInput(id.clone(), i));
                    } else {
                        doc.disconnect(id, i);
                        self.wire = Some(Wire::FromOutput(upstream));
                    }
                }
                let hot = resp.hovered() || resp.dragged();
                let port = ops::port(op, i);
                let mask = is_mask_port(op, i);
                if mask {
                    resp.on_hover_text("mask: where this node's effect applies (its alpha by default; Mask channel and Invert mask in Properties)");
                }
                port_dot(&painter, p, self.zoom, PortKind::Input, hot, port.is_some_and(|p| p.optional), port.is_some_and(|p| p.kind != ops::Kind::Image), mask);
                if (*n_in > 1 || mask) && self.zoom > 0.6 {
                    let label = port.map_or("", |p| p.label);
                    let color = if mask { MASK_PORT_COLOR } else { Color32::from_gray(210) };
                    // Outside the node, just above the incoming wire, clear of the title.
                    painter.text(p + Vec2::new(-PORT_R * 1.4, -PORT_R * 0.6), Align2::RIGHT_BOTTOM, label, FontId::proportional((10.0 * self.zoom).max(8.0)), color);
                }
            }
            if has_output(op) {
                let p = Self::out_port(*r);
                let resp = ui.interact(Rect::from_center_size(p, Vec2::splat(PORT_R * 3.0)), Id::unique(("out", id)), Sense::drag());
                if resp.drag_started() {
                    self.wire = Some(Wire::FromOutput(id.clone()));
                }
                port_dot(&painter, p, self.zoom, PortKind::Output, resp.hovered() || resp.dragged(), false, ops::output_kind(op) == ops::Kind::Scene, false);
            }
        }

        // An in-progress wire follows the pointer and connects on release:
        // on a port, else on a node's body (the node lit, the wire's end
        // snapped to the port it would take), else (from an output) on a
        // wire, lit as the drop target.
        let body = match (&self.wire, pointer) {
            (Some(wire), Some(ptr)) => self.body_target(doc, &geo, wire, ptr),
            _ => None,
        };
        let on_node = pointer.is_some_and(|p| Self::node_at(&geo, p).is_some());
        if let (Some(Wire::FromOutput(src)), Some(ptr)) = (&self.wire, pointer) {
            let lit = (self.input_at(&geo, ptr).is_none() && !on_node).then(|| self.wire_at(doc, ptr, Some(src))).flatten();
            if let Some(w) = lit {
                if let (Some(from), Some(to)) = (rect_of(&w.src), geo.iter().find(|g| g.0 == w.dst)) {
                    painter.add(bezier(Self::out_port(from), Self::in_port(to.2, w.port, to.3), Stroke::new(3.0, Color32::from_rgb(250, 190, 70)), self.straight_wires));
                }
            }
        }
        let in_port = |dst: &str, i: usize| geo.iter().find(|g| g.0 == dst).map(|g| Self::in_port(g.2, i, g.3));
        if let (Some(wire), Some(ptr)) = (&self.wire, pointer) {
            let anchor = match wire {
                Wire::FromOutput(src) => rect_of(src).map(Self::out_port),
                Wire::FromInput(dst, i) => in_port(dst, *i),
            };
            let color = Color32::from_rgb(240, 170, 60);
            // Over a node that takes it: lit, and the wire's end on the port.
            let mut end = ptr;
            if let Some((src, dst, i)) = &body {
                let (node, port) = match wire {
                    Wire::FromOutput(_) => (dst, in_port(dst, *i)),
                    Wire::FromInput(..) => (src, rect_of(src).map(Self::out_port)),
                };
                if let Some(r) = rect_of(node) {
                    painter.rect(r, 6.0 * self.zoom, color.gamma_multiply(0.15), Stroke::new(2.5, color), egui::StrokeKind::Outside);
                }
                end = port.unwrap_or(ptr);
            }
            if let Some(a) = anchor {
                let s = Stroke::new(2.0, color);
                match wire {
                    Wire::FromOutput(_) => painter.add(bezier(a, end, s, self.straight_wires)),
                    Wire::FromInput(..) => painter.add(bezier(end, a, s, self.straight_wires)),
                };
            }
        }
        if self.wire.is_some() && ui.input(|i| i.pointer.any_released()) {
            if let (Some(wire), Some(ptr)) = (self.wire.take(), pointer) {
                match wire {
                    Wire::FromOutput(src) => {
                        if let Some((dst, i)) = self.input_at(&geo, ptr) {
                            if fits(doc, &src, &dst, i) {
                                doc.connect(&src, &dst, i);
                            }
                        } else if on_node {
                            if let Some((src, dst, i)) = body {
                                doc.connect(&src, &dst, i);
                            }
                        } else if let Some(w) = self.wire_at(doc, ptr, Some(&src)).filter(|w| !is_scene(doc, &src) && !is_scene(doc, &w.src)) {
                            // Onto a wire: a Merge joins the two there.
                            if let Some(m) = doc.merge_into_wire(&src, &w.src, &w.dst, w.port) {
                                doc.selected = Some(m);
                            }
                        }
                    }
                    Wire::FromInput(dst, i) => match Self::output_at(&geo, ptr) {
                        Some(src) => {
                            if fits(doc, &src, &dst, i) {
                                doc.connect(&src, &dst, i);
                            }
                        }
                        None => {
                            if let Some((src, dst, i)) = body {
                                doc.connect(&src, &dst, i);
                            }
                        }
                    },
                }
            }
        }
        if self.wire.is_some() {
            ui.ctx().request_repaint();
        }
        self.box_select(ui, &painter, &bg, doc, &geo);
        if let Some(mm) = minimap {
            self.minimap(ui, &painter, doc, mm);
        }
        self.fit_button(ui, doc);
    }

    /// A drag on empty space draws a box; on release the nodes it touches
    /// are selected (added to the selection with Shift).
    fn box_select(&mut self, ui: &egui::Ui, painter: &egui::Painter, bg: &egui::Response, doc: &mut Doc, geo: &[(String, String, Rect, usize)]) {
        let (alt, shift, origin, ptr) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift, i.pointer.press_origin(), i.pointer.interact_pos()));
        if bg.drag_started_by(egui::PointerButton::Primary) && !alt {
            self.select_box = origin.map(|o| (o, shift));
        }
        let Some((start, add)) = self.select_box else { return };
        let Some(ptr) = ptr else { return };
        let r = Rect::from_two_pos(start, ptr);
        painter.rect(r, 0.0, SELECTED.gamma_multiply(0.08), Stroke::new(1.0, SELECTED.gamma_multiply(0.8)), egui::StrokeKind::Inside);
        if !bg.dragged() {
            self.select_box = None;
            let hit: Vec<String> = geo.iter().filter(|g| g.2.intersects(r)).map(|g| g.0.clone()).collect();
            if !hit.is_empty() || !add {
                doc.select_nodes(&hit, add);
            }
        }
    }

    /// A wire's right-click menu (see [`WireAction`]); it closes on a
    /// choice, Esc or a click elsewhere.
    fn wire_menu_ui(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let Some((w, at)) = self.wire_menu.clone() else { return };
        if !doc.wire_exists(&(w.src.clone(), w.dst.clone(), w.port)) || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.wire_menu = None;
            return;
        }
        let mut chosen = None;
        let area = egui::Area::new(Id::unique(("wire_menu", &w.src, &w.dst, w.port))).fixed_pos(at).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_min_width(200.0);
                ui.weak(format!("{} → {}", w.src, w.dst));
                for (action, label, tip) in WireAction::ALL {
                    if ui.button(label).on_hover_text(tip).clicked() {
                        chosen = Some(action);
                    }
                }
            });
        });
        if let Some(action) = chosen {
            self.wire_menu = None;
            self.wire_action(doc, &w, at, action);
        } else if area.response.clicked_elsewhere() {
            self.wire_menu = None;
        }
    }

    /// Do `action` to wire `w` (its menu opened at screen point `at`).
    fn wire_action(&mut self, doc: &mut Doc, w: &WireRef, at: Pos2, action: WireAction) {
        match action {
            WireAction::CameraSetup => {
                doc.select_wire(&w.src, &w.dst, w.port);
                let g = self.to_graph(at);
                doc.add_camera_setup([g.x, g.y]);
            }
            WireAction::InsertNode => {
                self.open_browser(Some(at));
                self.target_wire = Some(w.clone());
            }
            WireAction::Delete => {
                doc.disconnect(&w.dst, w.port);
                doc.selected_wire = None;
            }
        }
    }

    /// Right-click on a node: group or delete the selection.
    fn node_menu(&mut self, ui: &mut egui::Ui, doc: &mut Doc, id: &str) {
        if !doc.is_selected(id) {
            doc.select_only(id);
        }
        let n = doc.selection().len();
        let what = if n == 1 { "this node".to_string() } else { format!("{n} nodes") };
        if ui.add(egui::Button::new(format!("Group {what} in a backdrop")).shortcut_text(ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::G)))).clicked() {
            doc.group_selection();
            ui.close();
        }
        if ui.add(egui::Button::new(format!("Delete {what}")).shortcut_text("⌫")).clicked() {
            doc.delete_selection();
            ui.close();
        }
    }

    /// Draw the backdrops, back to front, and handle them: the title bar
    /// selects and moves (with what's inside), double-click renames, the
    /// swatch picks a color, corners resize.
    fn backdrops(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, doc: &mut Doc) {
        let z = self.zoom;
        for b in doc.graph.backdrops.clone() {
            let r = Rect::from_min_size(self.to_screen(Pos2::new(b.pos[0], b.pos[1])), Vec2::new(b.size[0], b.size[1]) * z);
            let bar = Rect::from_min_size(r.min, Vec2::new(r.width(), (Backdrop::TITLE_PAD * z).min(r.height())));
            let color = backdrop_color32(b.color);
            let selected = doc.selected_backdrop.as_deref() == Some(b.id.as_str());
            let round = 6.0 * z.sqrt();
            painter.rect_filled(r, round, color.gamma_multiply(0.30));
            let top = egui::CornerRadius { nw: round as u8, ne: round as u8, sw: 0, se: 0 };
            painter.rect_filled(bar, top, color.gamma_multiply(0.85));
            let edge = if selected { Stroke::new(2.0, SELECTED) } else { Stroke::new(1.0, color) };
            painter.rect_stroke(r, round, edge, egui::StrokeKind::Inside);
            let sw_side = (bar.height() * 0.5).clamp(6.0, 16.0);
            let swatch = Rect::from_center_size(Pos2::new(bar.right() - bar.height() * 0.5 - 10.0 * z, bar.center().y), Vec2::splat(sw_side));

            let bar_resp = ui.interact(bar, Id::unique(("backdrop", &b.id)), Sense::click_and_drag());
            if bar_resp.clicked() || bar_resp.drag_started() {
                doc.select_backdrop(&b.id);
            }
            if bar_resp.double_clicked() {
                self.title_edit = Some((b.id.clone(), b.title.clone(), true));
            }
            if bar_resp.drag_started() {
                let inner = doc.graph.backdrops.iter().filter(|o| o.id != b.id && b.contains(o.pos, [o.pos[0] + o.size[0], o.pos[1] + o.size[1]])).map(|o| o.id.clone()).collect();
                self.backdrop_drag = Some((b.id.clone(), doc.backdrop_nodes(&b.id), inner));
            }
            if bar_resp.dragged() {
                if let Some((_, nodes, inner)) = self.backdrop_drag.clone().filter(|d| d.0 == b.id) {
                    let d = bar_resp.drag_delta() / z;
                    for bid in std::iter::once(&b.id).chain(&inner) {
                        if let Some(mut o) = doc.graph.backdrop(bid).cloned() {
                            o.pos = [o.pos[0] + d.x, o.pos[1] + d.y];
                            doc.set_backdrop(o);
                        }
                    }
                    doc.move_nodes(&nodes, [d.x, d.y]);
                }
            }
            if bar_resp.drag_stopped() {
                self.backdrop_drag = None;
            }
            bar_resp.context_menu(|ui| {
                doc.select_backdrop(&b.id);
                if ui.button("Rename").clicked() {
                    self.title_edit = Some((b.id.clone(), b.title.clone(), true));
                    ui.close();
                }
                if ui.button("Select its nodes").on_hover_text("The nodes fully inside it").clicked() {
                    doc.select_nodes(&doc.backdrop_nodes(&b.id), false);
                    ui.close();
                }
                let ungroup = egui::Button::new("Ungroup (delete the box)").shortcut_text(ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::G)));
                if ui.add(ungroup).on_hover_text("The nodes stay").clicked() {
                    doc.delete_backdrop(&b.id);
                    ui.close();
                }
            });

            // The title, or the field renaming it.
            let font = FontId::proportional((b.font_size * z).max(7.0));
            let text_at = Pos2::new(bar.left() + 8.0 * z, bar.center().y);
            match &mut self.title_edit {
                Some((id, text, focus)) if *id == b.id => {
                    let field = Rect::from_min_max(Pos2::new(text_at.x, bar.top() + 2.0), Pos2::new(swatch.left() - 6.0, bar.bottom() - 2.0));
                    let fid = Id::unique(("backdrop-title", &b.id));
                    let resp = ui.put(field, egui::TextEdit::singleline(text).id(fid).font(font.clone()).margin(Vec2::new(2.0, 0.0)));
                    if *focus {
                        resp.request_focus();
                        *focus = false;
                    } else if resp.lost_focus() {
                        let mut nb = b.clone();
                        nb.title = text.trim().to_string();
                        self.title_edit = None;
                        doc.set_backdrop(nb);
                    }
                }
                _ => {
                    let shown = if b.title.is_empty() { "Backdrop" } else { b.title.as_str() };
                    let clip = painter.with_clip_rect(Rect::from_min_max(bar.min, Pos2::new(swatch.left() - 4.0, bar.max.y)).intersect(painter.clip_rect()));
                    clip.text(text_at, Align2::LEFT_CENTER, shown, font, Color32::from_gray(235));
                }
            }

            // The color swatch and its palette.
            painter.rect(swatch, 3.0, color, Stroke::new(1.0, Color32::from_gray(230)), egui::StrokeKind::Inside);
            let sw_resp = ui.interact(swatch.expand(2.0), Id::unique(("backdrop-color", &b.id)), Sense::click()).on_hover_text("Color");
            egui::Popup::from_toggle_button_response(&sw_resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                let mut nb = b.clone();
                if backdrop_palette(ui, &mut nb.color) {
                    doc.set_backdrop(nb);
                }
            });

            // Corners resize: top-left, top-right, bottom-left, bottom-right.
            let handle = (10.0 * z.sqrt()).max(6.0);
            for (k, corner) in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].into_iter().enumerate() {
                let (right, bottom) = (k % 2 == 1, k >= 2);
                let at = corner + Vec2::new(if right { -handle } else { handle }, if bottom { -handle } else { handle }) * 0.5;
                let resp = ui.interact(Rect::from_center_size(at, Vec2::splat(handle)), Id::unique(("backdrop-corner", &b.id, k)), Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(if right == bottom { egui::CursorIcon::ResizeNwSe } else { egui::CursorIcon::ResizeNeSw });
                    painter.circle_filled(at, handle * 0.3, Color32::from_gray(230));
                }
                if resp.drag_started() {
                    doc.select_backdrop(&b.id);
                }
                if resp.dragged() {
                    let d = resp.drag_delta() / z;
                    let Some(mut o) = doc.graph.backdrop(&b.id).cloned() else { continue };
                    let (mut x0, mut y0, mut x1, mut y1) = (o.pos[0], o.pos[1], o.pos[0] + o.size[0], o.pos[1] + o.size[1]);
                    if right { x1 = (x1 + d.x).max(x0 + BACKDROP_MIN.x) } else { x0 = (x0 + d.x).min(x1 - BACKDROP_MIN.x) }
                    if bottom { y1 = (y1 + d.y).max(y0 + BACKDROP_MIN.y) } else { y0 = (y0 + d.y).min(y1 - BACKDROP_MIN.y) }
                    o.pos = [x0, y0];
                    o.size = [x1 - x0, y1 - y0];
                    doc.set_backdrop(o);
                }
            }
        }
    }

    /// The input port in reach of screen point `p`: the nearest, not the
    /// first, since a Merge's ports sit closer together than the reach.
    /// The output port within reach of screen point `p` (nearest first).
    fn output_at(geo: &[(String, String, Rect, usize)], p: Pos2) -> Option<String> {
        geo.iter()
            .filter(|(_, op, ..)| has_output(op))
            .map(|(id, _, r, _)| (id, Self::out_port(*r).distance(p)))
            .filter(|&(_, d)| d < PORT_R * 2.5)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id.clone())
    }

    /// The node under screen point `p` (the topmost: drawn last).
    fn node_at(geo: &[(String, String, Rect, usize)], p: Pos2) -> Option<&str> {
        geo.iter().rev().find(|g| g.2.contains(p)).map(|g| g.0.as_str())
    }

    /// Where `wire` released at `p` would connect by the body rule, as
    /// (output's node, input's node, input): over a node's body or title
    /// and away from any port of the wire's kind (a port wins).
    fn body_target(&self, doc: &Doc, geo: &[(String, String, Rect, usize)], wire: &Wire, p: Pos2) -> Option<(String, String, usize)> {
        let node = Self::node_at(geo, p)?;
        match wire {
            Wire::FromOutput(src) => {
                if self.input_at(geo, p).is_some() {
                    return None;
                }
                body_input(doc, src, node).map(|i| (src.clone(), node.to_string(), i))
            }
            Wire::FromInput(dst, i) => {
                if Self::output_at(geo, p).is_some() {
                    return None;
                }
                body_output(doc, node, dst, *i).then(|| (node.to_string(), dst.clone(), *i))
            }
        }
    }

    fn input_at(&self, geo: &[(String, String, Rect, usize)], p: Pos2) -> Option<(String, usize)> {
        geo.iter()
            .flat_map(|(id, _, r, n)| (0..*n).map(move |i| (id, i, Self::in_port(*r, i, *n).distance(p))))
            .filter(|&(.., d)| d < PORT_R * 2.5)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(id, i, _)| (id.clone(), i))
    }

    /// The wire within reach of screen point `p` (nearest first), leaving
    /// out wires to or from `skip`.
    fn wire_at(&self, doc: &Doc, p: Pos2, skip: Option<&str>) -> Option<WireRef> {
        let rect_of = |id: &str| doc.node(id).map(|n| self.node_rect(n.pos.unwrap_or([0.0, 0.0])));
        let mut best: Option<(f32, WireRef)> = None;
        for n in &doc.graph.nodes {
            let Some(to) = rect_of(&n.id) else { continue };
            for (i, src) in n.inputs.iter().enumerate() {
                let Some(from) = rect_of(src).filter(|_| skip != Some(src.as_str()) && skip != Some(n.id.as_str())) else { continue };
                let d = bezier_distance(Self::out_port(from), Self::in_port(to, i, n.inputs.len()), p, self.straight_wires);
                if d < WIRE_GRAB && best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, WireRef { src: src.clone(), dst: n.id.clone(), port: i }));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// The graph's bounds and the current view, in graph coordinates.
    fn graph_bounds(&self, doc: &Doc) -> Rect {
        let view = Rect::from_min_max(self.to_graph(self.rect.min), self.to_graph(self.rect.max));
        view.union(content_bounds(doc)).expand(40.0)
    }

    /// Where the minimap sits, when there's a graph and room for it.
    fn minimap_rect(&self, doc: &Doc) -> Option<Rect> {
        let fits = self.rect.width() > MINIMAP.x * 2.0 && self.rect.height() > MINIMAP.y * 1.6;
        (!doc.graph.nodes.is_empty() && fits).then(|| Rect::from_min_size(self.rect.right_bottom() - MINIMAP - Vec2::splat(10.0), MINIMAP))
    }

    /// Draw the minimap and handle it: click or drag to center the view
    /// there, scroll to zoom about that spot, double-click to fit.
    fn minimap(&mut self, ui: &egui::Ui, painter: &egui::Painter, doc: &Doc, mm: Rect) {
        let resp = ui.interact(mm, Id::unique("node_minimap"), Sense::click_and_drag());
        if resp.drag_started() {
            self.minimap_bounds = Some(self.graph_bounds(doc));
        }
        if !resp.dragged() && !resp.drag_started() {
            self.minimap_bounds = None;
        }
        let b = self.minimap_bounds.unwrap_or_else(|| self.graph_bounds(doc));
        let s = (mm.width() / b.width()).min(mm.height() / b.height());
        let to_map = |g: Pos2| mm.center() + (g - b.center()) * s;
        let to_graph = |m: Pos2| b.center() + (m - mm.center()) / s;
        let center_on = |ed: &mut NodeEditor, g: Pos2| ed.pan = ed.rect.size() / 2.0 - g.to_vec2() * ed.zoom;

        if resp.double_clicked() {
            self.fit();
        } else if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                center_on(self, to_graph(p));
            }
        }
        if let Some(h) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 200.0);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (scroll * 0.004).exp()).clamp(ZOOM_MIN, ZOOM_MAX);
                center_on(self, to_graph(h));
            }
        }

        painter.rect(mm, 4.0, Color32::from_black_alpha(170), Stroke::new(1.0, Color32::from_gray(80)), egui::StrokeKind::Inside);
        let clip = painter.with_clip_rect(mm);
        for b in &doc.graph.backdrops {
            let r = Rect::from_min_max(to_map(Pos2::new(b.pos[0], b.pos[1])), to_map(Pos2::new(b.pos[0] + b.size[0], b.pos[1] + b.size[1])));
            clip.rect_filled(r, 1.0, backdrop_color32(b.color).gamma_multiply(0.45));
        }
        for n in &doc.graph.nodes {
            let p = n.pos.unwrap_or([0.0, 0.0]);
            let r = Rect::from_min_max(to_map(Pos2::new(p[0], p[1])), to_map(Pos2::new(p[0] + NODE_W, p[1] + NODE_H)));
            let color = ops::info(&n.op).map_or(Color32::from_gray(80), |i| category_color(i.category));
            clip.rect_filled(r, 1.0, color);
            if doc.is_selected(&n.id) {
                clip.rect_stroke(r, 1.0, Stroke::new(1.0, SELECTED), egui::StrokeKind::Outside);
            }
        }
        let view = Rect::from_min_max(to_map(self.to_graph(self.rect.min)), to_map(self.to_graph(self.rect.max)));
        let hot = resp.hovered() || resp.dragged();
        clip.rect_stroke(view, 1.0, Stroke::new(1.0, Color32::from_white_alpha(if hot { 230 } else { 150 })), egui::StrokeKind::Inside);
    }

    /// Middle-drag or Alt-drag on empty space pans (a plain drag selects);
    /// scroll pans, pinch or Cmd-scroll zooms about the pointer.
    fn navigate(&mut self, ui: &egui::Ui, bg: &egui::Response, minimap: Option<Rect>) {
        let alt = ui.input(|i| i.modifiers.alt);
        if bg.dragged_by(egui::PointerButton::Middle) || (alt && bg.dragged_by(egui::PointerButton::Primary)) {
            self.pan += bg.drag_delta();
        }
        if !ui.rect_contains_pointer(self.rect) || minimap.is_some_and(|m| ui.rect_contains_pointer(m)) {
            return;
        }
        let (scroll, zoom, hover) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        if zoom != 1.0 {
            if let Some(h) = hover {
                let before = self.to_graph(h);
                self.zoom = (self.zoom * zoom).clamp(ZOOM_MIN, ZOOM_MAX);
                self.pan = h - self.rect.min - before.to_vec2() * self.zoom;
            }
        } else if scroll != Vec2::ZERO {
            self.pan += scroll;
        }
    }

    fn fit_to(&mut self, doc: &Doc) {
        self.fit_pending = false;
        if doc.graph.nodes.is_empty() || !self.rect.is_positive() {
            return;
        }
        (self.zoom, self.pan) = fit_view(content_bounds(doc), self.rect.size());
        self.fitted_size = Some(self.rect.size());
    }

    /// The Fit button in the top-right corner: frames every node, as F does.
    fn fit_button(&mut self, ui: &mut egui::Ui, doc: &Doc) {
        if doc.graph.nodes.is_empty() {
            return;
        }
        let r = Rect::from_min_size(self.rect.right_top() + Vec2::new(-44.0, 8.0), Vec2::new(36.0, 20.0));
        if ui.put(r, egui::Button::new("Fit").small()).on_hover_text("Frame all nodes (F)").clicked() {
            self.fit();
        }
    }

    fn draw_grid(&self, painter: &egui::Painter) {
        let step = GRID * self.zoom;
        if step < 8.0 {
            return;
        }
        let r = self.rect;
        let off = Vec2::new((self.pan.x).rem_euclid(step), (self.pan.y).rem_euclid(step));
        let color = Color32::from_gray(42);
        let mut x = r.left() + off.x;
        while x < r.right() {
            painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], (1.0, color));
            x += step;
        }
        let mut y = r.top() + off.y;
        while y < r.bottom() {
            painter.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], (1.0, color));
            y += step;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_node(&self, painter: &egui::Painter, doc: &Doc, id: &str, op: &str, r: Rect, selected: bool, error: bool) {
        let info = ops::info(op);
        let off = doc.node(id).is_some_and(|n| n.disabled);
        let base = info.map_or(Color32::from_gray(80), |i| category_color(i.category));
        // Turned off: dimmed toward the background, struck through.
        let base = if off { base.lerp_to_gamma(Color32::from_gray(40), 0.6) } else { base };
        let z = self.zoom;
        painter.rect_filled(r.translate(Vec2::splat(3.0 * z)), 6.0 * z, Color32::from_black_alpha(90));
        painter.rect_filled(r, 6.0 * z, base);
        if off {
            painter.line_segment([r.left_bottom() + Vec2::new(6.0, -6.0) * z, r.right_top() + Vec2::new(-22.0, 6.0) * z], Stroke::new(2.0 * z.sqrt(), Color32::from_rgb(235, 90, 70)));
        }
        // The on/off switch (unlike the ports): a light dot in a dark ring
        // when on, the empty ring when off.
        let sw = self.switch_rect(r);
        let ring = Stroke::new(1.3, Color32::from_black_alpha(150));
        if off {
            painter.circle_stroke(sw.center(), sw.width() / 2.0, Stroke::new(1.3, Color32::from_gray(150)));
        } else {
            painter.circle(sw.center(), sw.width() / 2.0, Color32::from_white_alpha(210), ring);
        }
        let outline = if error {
            Stroke::new(2.5, Color32::from_rgb(235, 80, 70))
        } else if selected {
            Stroke::new(2.0, Color32::from_rgb(250, 190, 70))
        } else {
            Stroke::new(1.0, Color32::from_black_alpha(140))
        };
        painter.rect_stroke(r, 6.0 * z, outline, egui::StrokeKind::Outside);

        painter.text(
            Pos2::new(r.center().x, r.top() + 15.0 * z),
            Align2::CENTER_CENTER,
            id,
            // Scaled with the zoom, but kept readable.
            FontId::proportional((13.0 * z).max(10.0)),
            if off { Color32::from_white_alpha(120) } else { Color32::WHITE },
        );
        // With several Viewers, which one the viewer shows.
        let several = op == "viewer" && doc.graph.nodes.iter().filter(|n| n.op == "viewer").count() > 1;
        let active = several && doc.viewer().as_deref() == Some(id);
        if doc.overridden() == Some(id) || (active && doc.overridden().is_none()) {
            // Shown in the viewer (in place of the Viewer's input, or the
            // Viewer shown of several): an eye.
            let c = Pos2::new(r.left() + 11.0 * z, r.top() + 10.0 * z);
            let eye = Color32::from_rgb(120, 200, 240);
            painter.add(egui::Shape::convex_polygon(
                (0..16).map(|i| {
                    let t = i as f32 / 16.0 * std::f32::consts::TAU;
                    c + Vec2::new(5.5 * t.cos(), 3.2 * t.sin() * t.sin().abs().sqrt()) * z
                }).collect(),
                Color32::TRANSPARENT,
                Stroke::new(1.2, eye),
            ));
            painter.circle_filled(c, 1.8 * z, eye);
        }
        if doc.is_node_animated(id) {
            // Keyframed params: a small diamond by the switch.
            let c = Pos2::new(r.right() - 24.0 * z, r.top() + 10.0 * z);
            let s = 4.0 * z;
            painter.add(egui::Shape::convex_polygon(
                vec![c + Vec2::new(0.0, -s), c + Vec2::new(s, 0.0), c + Vec2::new(0.0, s), c + Vec2::new(-s, 0.0)],
                Color32::from_rgb(240, 175, 50),
                Stroke::NONE,
            ));
        }
        if z > 0.6 {
            // Subtitle: the op, plus the file name for media nodes.
            let mut sub = info.map_or(op.to_string(), |i| i.label.to_string());
            if several && !active {
                sub = format!("{sub} (not shown)");
            }
            if let Some(path) = doc.node(id).and_then(|n| n.params.get("path")).and_then(|p| p.as_str()) {
                if let Some(name) = std::path::Path::new(path).file_name() {
                    sub = name.to_string_lossy().into_owned();
                }
            }
            let max_chars = 22;
            if sub.chars().count() > max_chars {
                sub = format!("{}…", sub.chars().take(max_chars - 1).collect::<String>());
            }
            painter.text(
                Pos2::new(r.center().x, r.top() + 33.0 * z),
                Align2::CENTER_CENTER,
                sub,
                FontId::proportional((10.5 * z).max(8.5)),
                Color32::from_white_alpha(if off { 90 } else { 170 }),
            );
        }
    }

    /// Where node box `r`'s on/off switch is: its top-right corner (away
    /// from the ports).
    fn switch_rect(&self, r: Rect) -> Rect {
        let z = self.zoom;
        Rect::from_center_size(Pos2::new(r.right() - 10.0 * z, r.top() + 10.0 * z), Vec2::splat(8.0 * z.max(0.5)))
    }
}

/// The node graph's grid spacing (graph units; lines through 0).
const GRID: f32 = 40.0;

/// `v` on the nearest grid line.
fn snap(v: f32) -> f32 {
    (v / GRID).round() * GRID
}

/// How far a wire from `a` to `b` leaves and enters its ports level
/// (none when `straight`).
fn reach(a: Pos2, b: Pos2, straight: bool) -> f32 {
    if straight { 0.0 } else { ((b.x - a.x).abs() * 0.5).max(40.0) }
}

/// The wire from `a` to `b`: a curve leaving and entering level, or a
/// straight line.
fn bezier(a: Pos2, b: Pos2, stroke: Stroke, straight: bool) -> CubicBezierShape {
    let dx = reach(a, b, straight);
    CubicBezierShape::from_points_stroke(
        [a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b],
        false,
        Color32::TRANSPARENT,
        stroke,
    )
}

impl Shake {
    /// Note a drag step `d` (screen points) at time `now`; true once, when
    /// the drag direction has reversed `SHAKE_FLIPS` times within
    /// `SHAKE_SECS`.
    fn shaken(&mut self, d: Vec2, now: f64) -> bool {
        if self.done || d.length() < 3.0 {
            return false;
        }
        let dir = d.normalized();
        if self.last != Vec2::ZERO && dir.dot(self.last) < -0.5 {
            self.flips.push(now);
        }
        self.last = dir;
        self.flips.retain(|&t| now - t <= SHAKE_SECS);
        self.done = self.flips.len() >= SHAKE_FLIPS;
        self.done
    }
}

/// A node with an input and an output and no connections either way: one
/// that can be dropped into a wire.
fn is_free(doc: &Doc, id: &str) -> bool {
    let Some(n) = doc.node(id) else { return false };
    !n.inputs.is_empty() && has_output(&n.op) && n.inputs.iter().all(String::is_empty) && !doc.graph.nodes.iter().any(|m| m.inputs.iter().any(|i| i == id))
}

/// Distance from `p` to the wire drawn from `a` to `b` (see [`bezier`]).
fn bezier_distance(a: Pos2, b: Pos2, p: Pos2, straight: bool) -> f32 {
    let dx = reach(a, b, straight);
    let [c1, c2] = [a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0)];
    let at = |t: f32| {
        let u = 1.0 - t;
        (a.to_vec2() * (u * u * u) + c1.to_vec2() * (3.0 * u * u * t) + c2.to_vec2() * (3.0 * u * t * t) + b.to_vec2() * (t * t * t)).to_pos2()
    };
    let pts: Vec<Pos2> = (0..=32).map(|i| at(i as f32 / 32.0)).collect();
    pts.windows(2)
        .map(|w| {
            let (s, e) = (w[0], w[1]);
            let t = ((p - s).dot(e - s) / (e - s).length_sq().max(1e-6)).clamp(0.0, 1.0);
            s.lerp(e, t).distance(p)
        })
        .fold(f32::INFINITY, f32::min)
}

#[derive(Clone, Copy)]
enum PortKind {
    /// A yellow square.
    Input,
    /// A green circle.
    Output,
}

/// `scene`: the port takes or makes a 3D scene (drawn blue). `mask`: a
/// masked op's mask input (drawn violet).
#[allow(clippy::too_many_arguments)]
fn port_dot(painter: &egui::Painter, p: Pos2, zoom: f32, kind: PortKind, hot: bool, optional: bool, scene: bool, mask: bool) {
    let r = PORT_R * zoom.max(0.6);
    let color = match (kind, hot) {
        _ if scene && !hot => SCENE_COLOR,
        _ if mask && !hot => MASK_PORT_COLOR,
        (PortKind::Input, false) => IN_PORT_COLOR,
        (PortKind::Input, true) => IN_PORT_HOT,
        (PortKind::Output, false) => OUT_PORT_COLOR,
        (PortKind::Output, true) => OUT_PORT_HOT,
    };
    let (fill, stroke) = if optional && !hot {
        (Color32::from_gray(45), Stroke::new(1.5, color))
    } else {
        (color, Stroke::new(1.0, Color32::from_black_alpha(160)))
    };
    match kind {
        PortKind::Input => {
            // Squares read larger than circles of the same radius.
            let sq = Rect::from_center_size(p, Vec2::splat(r * 1.8));
            painter.rect_filled(sq, 1.0, fill);
            painter.rect_stroke(sq, 1.0, stroke, egui::StrokeKind::Inside);
        }
        PortKind::Output => {
            painter.circle_filled(p, r, fill);
            painter.circle_stroke(p, r - 0.75, stroke);
        }
    }
}

/// Whether input `i` of `op` is its generic mask (the last port of an op
/// with a mask base).
fn is_mask_port(op: &str, i: usize) -> bool {
    ops::info(op).is_some_and(|o| o.mask_base.is_some() && i + 1 == o.inputs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use arclight::graph::Graph;

    /// Drives a `NodeEditor` headlessly with pointer events, one egui pass
    /// per frame.
    struct Harness {
        ctx: egui::Context,
        ed: NodeEditor,
        time: f64,
        /// Held modifier keys, for the next events.
        mods: Modifiers,
    }

    impl Harness {
        /// At 1:1 zoom (where a Merge's three ports sit 11.5 points apart).
        fn new() -> Harness {
            let ed = NodeEditor { zoom: 1.0, fit_pending: false, ..NodeEditor::default() };
            Harness { ctx: egui::Context::default(), ed, time: 0.0, mods: Modifiers::NONE }
        }

        fn frame(&mut self, doc: &mut Doc, events: Vec<egui::Event>) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 700.0))),
                time: Some(self.time),
                events: std::iter::once(egui::Event::ModifiersChanged(self.mods)).chain(events).collect(),
                ..Default::default()
            };
            self.time += 1.0 / 60.0;
            let ed = &mut self.ed;
            self.ctx.run_ui(raw, |ui| ed.ui(ui, doc, None)).drop_without_applying_deltas();
        }

        fn button(pos: Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
        }

        /// Press at `from`, move to `to` in steps, release.
        fn drag(&mut self, doc: &mut Doc, from: Pos2, to: Pos2) {
            self.frame(doc, vec![egui::Event::PointerMoved(from)]);
            self.frame(doc, vec![Self::button(from, true)]);
            for i in 1..=8 {
                self.frame(doc, vec![egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0))]);
            }
            self.frame(doc, vec![Self::button(to, false)]);
            self.frame(doc, vec![]);
        }

        fn rect(&self, doc: &Doc, id: &str) -> Rect {
            self.ed.node_rect(doc.node(id).unwrap().pos.unwrap())
        }
    }

    #[test]
    fn wires_are_picked_and_have_a_menu() {
        let mut doc = Doc::new(Graph::default(), None);
        let plate = doc.add_node("checkerboard", [100.0, 100.0]);
        let grade = doc.add_node("grade", [100.0, 300.0]);
        doc.connect(&plate, &grade, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        // Halfway down the wire (from the plate's output to the grade's input).
        let (a, b) = (h.rect(&doc, &plate), h.rect(&doc, &grade));
        let mid = NodeEditor::out_port(a).lerp(NodeEditor::in_port(b, 0, 2), 0.5);
        let click = |h: &mut Harness, doc: &mut Doc, at: Pos2, button: egui::PointerButton| {
            h.frame(doc, vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                h.frame(doc, vec![egui::Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }]);
            }
            h.frame(doc, vec![]);
        };
        click(&mut h, &mut doc, mid, egui::PointerButton::Primary);
        assert_eq!(doc.selected_wire, Some((plate.clone(), grade.clone(), 0)), "a click selects it");
        click(&mut h, &mut doc, mid + Vec2::new(300.0, 0.0), egui::PointerButton::Primary);
        assert_eq!(doc.selected_wire, None, "a click on nothing deselects");
        // Right-click: its menu.
        click(&mut h, &mut doc, mid, egui::PointerButton::Secondary);
        assert!(h.ed.wire_menu.is_some() && doc.selected_wire.is_some() && !h.ed.browser_open());
        // Its actions: a camera setup into it; a node browser aimed at it;
        // disconnecting it.
        let w = h.ed.wire_menu.clone().unwrap().0;
        h.ed.wire_action(&mut doc, &w, mid, WireAction::InsertNode);
        assert!(h.ed.browser_open() && h.ed.target_wire.as_ref() == Some(&w));
        h.ed.browser = None;
        h.ed.wire_action(&mut doc, &w, mid, WireAction::CameraSetup);
        let render = doc.node(&grade).unwrap().inputs[0].clone();
        assert_eq!(doc.node(&render).map(|n| n.op.as_str()), Some("scanline_render"));
        assert_eq!(doc.node(&render).unwrap().inputs[2], plate);
        let into = WireRef { src: render.clone(), dst: grade.clone(), port: 0 };
        h.ed.wire_action(&mut doc, &into, mid, WireAction::Delete);
        assert_eq!(doc.node(&grade).unwrap().inputs[0], "");
    }

    #[test]
    fn an_inserted_merge_takes_more_inputs() {
        // At these zooms a Merge's three ports are 11.5 to 5.75 points apart.
        for zoom in [1.0, 0.7, 0.5] {
            let mut doc = Doc::new(Graph::default(), None);
            let src = doc.add_node("constant", [0.0, 0.0]);
            let blur = doc.add_node("blur", [500.0, 0.0]);
            let other = doc.add_node("constant", [0.0, 200.0]);
            let third = doc.add_node("constant", [0.0, 400.0]);
            let merge = doc.add_node("merge", [250.0, 100.0]);
            doc.connect(&src, &blur, 0);
            assert!(doc.insert_on_wire(&merge, &src, &blur, 0));
            let mut h = Harness::new();
            h.ed.zoom = zoom;
            h.frame(&mut doc, vec![]);
            // From the other constant's output to the merge's B input…
            let from = NodeEditor::out_port(h.rect(&doc, &other));
            let to = NodeEditor::in_port(h.rect(&doc, &merge), 1, 3);
            h.drag(&mut doc, from, to);
            let inputs = |doc: &Doc| doc.node(&merge).unwrap().inputs.clone();
            assert_eq!(inputs(&doc)[..2], [src.clone(), other.clone()], "zoom {zoom}: B takes it, A keeps its input");
            // …and a third onto A, replacing what the wire brought in.
            let from = NodeEditor::out_port(h.rect(&doc, &third));
            let to = NodeEditor::in_port(h.rect(&doc, &merge), 0, 3);
            h.drag(&mut doc, from, to);
            assert_eq!(inputs(&doc)[..2], [third.clone(), other.clone()], "zoom {zoom}");
        }
    }

    #[test]
    fn a_merge_dropped_on_a_wire_takes_more_inputs() {
        let mut doc = Doc::new(Graph::default(), None);
        let src = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [500.0, 0.0]);
        let other = doc.add_node("constant", [0.0, 200.0]);
        let merge = doc.add_node("merge", [250.0, 250.0]);
        doc.connect(&src, &blur, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        h.frame(&mut doc, vec![]);
        // Drag the merge by its title onto the middle of the wire.
        let grab = h.rect(&doc, &merge).center_top() + Vec2::new(0.0, 8.0);
        let (a, b) = (NodeEditor::out_port(h.rect(&doc, &src)), NodeEditor::in_port(h.rect(&doc, &blur), 0, 2));
        h.drag(&mut doc, grab, a.lerp(b, 0.5));
        let m = doc.node(&merge).unwrap().inputs.clone();
        assert_eq!((m[0].as_str(), doc.node(&blur).unwrap().inputs[0].as_str()), (src.as_str(), merge.as_str()), "spliced");
        // Then B from another node, both ways.
        let to = NodeEditor::in_port(h.rect(&doc, &merge), 1, 3);
        let out = NodeEditor::out_port(h.rect(&doc, &other));
        h.drag(&mut doc, out, to);
        assert_eq!(doc.node(&merge).unwrap().inputs[1], other, "output → B");
        doc.disconnect(&merge, 1);
        h.frame(&mut doc, vec![]);
        h.drag(&mut doc, to, out);
        assert_eq!(doc.node(&merge).unwrap().inputs[1], other, "B → output");
    }

    #[test]
    fn a_merge_added_on_a_wire_from_the_menu_takes_more_inputs() {
        let mut doc = Doc::new(Graph::default(), None);
        let src = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [500.0, 0.0]);
        let other = doc.add_node("constant", [0.0, 200.0]);
        doc.connect(&src, &blur, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        h.frame(&mut doc, vec![]);
        let (a, b) = (NodeEditor::out_port(h.rect(&doc, &src)), NodeEditor::in_port(h.rect(&doc, &blur), 0, 2));
        h.ed.target_wire = h.ed.wire_at(&doc, a.lerp(b, 0.5), None);
        assert!(h.ed.target_wire.is_some());
        let at = h.ed.to_graph(a.lerp(b, 0.5) + Vec2::new(0.0, 60.0));
        h.ed.place(&mut doc, ops::info("merge").unwrap(), at, false);
        let merge = doc.selected.clone().unwrap();
        h.frame(&mut doc, vec![]);
        let to = NodeEditor::in_port(h.rect(&doc, &merge), 1, 3);
        let out = NodeEditor::out_port(h.rect(&doc, &other));
        h.drag(&mut doc, out, to);
        assert_eq!(doc.node(&merge).unwrap().inputs[1], other, "output → B");
    }

    /// A wire dropped on a node's body (or title) goes into its first free
    /// input that takes it, else its first that does; a port, if it's
    /// dropped on one, wins. One undo step.
    #[test]
    fn a_wire_dropped_on_a_node_s_body_connects_it() {
        let mut doc = Doc::new(Graph::default(), None);
        let c: Vec<String> = (0..4).map(|k| doc.add_node("constant", [0.0, 150.0 * k as f32])).collect();
        let merge = doc.add_node("merge", [400.0, 200.0]);
        // The setup is one undo step (the app records one each frame).
        doc.commit(false);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let inputs = |doc: &Doc| doc.node(&merge).unwrap().inputs.clone();
        let body = |h: &Harness, doc: &Doc| h.rect(doc, &merge).center();
        let title = |h: &Harness, doc: &Doc| h.rect(doc, &merge).center_top() + Vec2::new(0.0, 6.0);
        // Free inputs in port order, A then B (the mask never); on the body
        // or the title.
        assert!(is_mask_port("merge", 2));
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[0])), body(&h, &doc));
        h.drag(&mut doc, from, to);
        assert_eq!(inputs(&doc), [c[0].as_str(), "", ""]);
        doc.undo();
        assert_eq!(inputs(&doc), ["", "", ""], "one undo step");
        doc.redo();
        h.frame(&mut doc, vec![]);
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[1])), title(&h, &doc));
        h.drag(&mut doc, from, to);
        assert_eq!(inputs(&doc), [c[0].as_str(), &c[1], ""]);
        // A and B taken: A, its wire replaced, though the mask is free.
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[2])), body(&h, &doc));
        h.drag(&mut doc, from, to);
        assert_eq!(inputs(&doc), [c[2].as_str(), &c[1], ""]);
        // On a port: that port (the mask's too), though A is free.
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[3])), NodeEditor::in_port(h.rect(&doc, &merge), 2, 3));
        h.drag(&mut doc, from, to);
        doc.disconnect(&merge, 0);
        h.frame(&mut doc, vec![]);
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[0])), NodeEditor::in_port(h.rect(&doc, &merge), 1, 3));
        h.drag(&mut doc, from, to);
        assert_eq!(inputs(&doc), ["", c[0].as_str(), &c[3]]);
        // A Blur with its input wired: a second image replaces it, not the mask.
        let blur = doc.add_node("blur", [700.0, 200.0]);
        doc.connect(&c[1], &blur, 0);
        h.frame(&mut doc, vec![]);
        let (from, to) = (NodeEditor::out_port(h.rect(&doc, &c[3])), h.rect(&doc, &blur).center());
        h.drag(&mut doc, from, to);
        assert_eq!(doc.node(&blur).unwrap().inputs, [c[3].as_str(), ""]);
    }

    /// No input takes it (a scene into a 2D node), it's the node itself, or
    /// it would close a cycle: nothing connects. A scene finds the scene
    /// port.
    #[test]
    fn a_wire_dropped_on_a_node_that_cant_take_it_connects_nothing() {
        let mut doc = Doc::new(Graph::default(), None);
        let c = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [400.0, 0.0]);
        let grade = doc.add_node("grade", [800.0, 0.0]);
        let card = doc.add_node("card", [0.0, 300.0]);
        let render = doc.add_node("scanline_render", [400.0, 300.0]);
        doc.connect(&c, &blur, 0);
        doc.connect(&blur, &grade, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let before = doc.graph.clone();
        let out = |h: &Harness, doc: &Doc, id: &str| NodeEditor::out_port(h.rect(doc, id));
        let body = |h: &Harness, doc: &Doc, id: &str| h.rect(doc, id).center();
        let (from, to) = (out(&h, &doc, &card), body(&h, &doc, &blur));
        h.drag(&mut doc, from, to);
        assert_eq!(doc.graph, before, "a scene into a 2D node");
        let (from, to) = (out(&h, &doc, &grade), body(&h, &doc, &blur));
        h.drag(&mut doc, from, to);
        assert_eq!(doc.graph, before, "a cycle (grade is downstream of blur)");
        let geo: Vec<(String, String, Rect, usize)> = doc.graph.nodes.iter().map(|n| (n.id.clone(), n.op.clone(), h.rect(&doc, &n.id), n.inputs.len())).collect();
        assert_eq!(h.ed.body_target(&doc, &geo, &Wire::FromOutput(grade.clone()), body(&h, &doc, &blur)), None, "nor shown as a target");
        let (from, to) = (out(&h, &doc, &blur), body(&h, &doc, &blur));
        h.drag(&mut doc, from, to);
        assert_eq!(doc.graph, before, "itself");
        let (from, to) = (out(&h, &doc, &card), body(&h, &doc, &render));
        h.drag(&mut doc, from, to);
        assert_eq!(doc.node(&render).unwrap().inputs[0], card, "the scene port");
    }

    /// A node lying over a wire takes the drop, not the wire (no Merge),
    /// even when it can't take it.
    #[test]
    fn a_node_over_a_wire_takes_the_drop_not_the_wire() {
        let mut doc = Doc::new(Graph::default(), None);
        let c = doc.add_node("constant", [0.0, 0.0]);
        let grade = doc.add_node("grade", [800.0, 0.0]);
        let blur = doc.add_node("blur", [400.0, 0.0]);
        let d = doc.add_node("constant", [0.0, 300.0]);
        doc.connect(&c, &grade, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let over = h.rect(&doc, &blur).center();
        assert!(h.ed.wire_at(&doc, over, None).is_some(), "the wire runs under the blur");
        let nodes = doc.graph.nodes.len();
        let from = NodeEditor::out_port(h.rect(&doc, &blur));
        h.drag(&mut doc, from, over);
        assert_eq!((doc.graph.nodes.len(), doc.node(&grade).unwrap().inputs[0].as_str()), (nodes, c.as_str()), "itself: nothing");
        let from = NodeEditor::out_port(h.rect(&doc, &d));
        h.drag(&mut doc, from, over);
        assert_eq!((doc.graph.nodes.len(), doc.node(&blur).unwrap().inputs[0].as_str()), (nodes, d.as_str()), "into the blur");
    }

    /// While a wire is dragged over a node it would go into, that's the
    /// target the editor draws (the node lit, the end on the port); away
    /// from nodes, none.
    #[test]
    fn hovering_a_node_with_a_wire_shows_where_it_lands() {
        let mut doc = Doc::new(Graph::default(), None);
        let c = doc.add_node("constant", [0.0, 0.0]);
        let merge = doc.add_node("merge", [400.0, 0.0]);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let geo: Vec<(String, String, Rect, usize)> = doc.graph.nodes.iter().map(|n| (n.id.clone(), n.op.clone(), h.rect(&doc, &n.id), n.inputs.len())).collect();
        let wire = Wire::FromOutput(c.clone());
        let over = h.rect(&doc, &merge).center();
        assert_eq!(h.ed.body_target(&doc, &geo, &wire, over), Some((c.clone(), merge.clone(), 0)));
        assert_eq!(h.ed.body_target(&doc, &geo, &wire, over + Vec2::new(0.0, 200.0)), None, "off the node");
        let port = NodeEditor::in_port(h.rect(&doc, &merge), 1, 3);
        assert_eq!(h.ed.body_target(&doc, &geo, &wire, port), None, "on a port: the port's");
    }

    /// The other way: an input dragged onto a node's body takes its output
    /// (not a Viewer's: it has none).
    #[test]
    fn an_input_dropped_on_a_node_s_body_takes_its_output() {
        let mut doc = Doc::new(Graph::default(), None);
        let c = doc.add_node("constant", [0.0, 0.0]);
        let viewer = doc.add_node("viewer", [0.0, 300.0]);
        let blur = doc.add_node("blur", [400.0, 150.0]);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let input = NodeEditor::in_port(h.rect(&doc, &blur), 0, 2);
        let (from, to) = (input, h.rect(&doc, &viewer).center());
        h.drag(&mut doc, from, to);
        assert_eq!(doc.node(&blur).unwrap().inputs[0], "", "a Viewer has no output");
        let (from, to) = (input, h.rect(&doc, &c).center());
        h.drag(&mut doc, from, to);
        assert_eq!(doc.node(&blur).unwrap().inputs[0], c);
    }

    #[test]
    fn an_output_dropped_on_a_wire_merges_into_it() {
        let mut doc = Doc::new(Graph::default(), None);
        let src = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [500.0, 0.0]);
        let other = doc.add_node("constant", [0.0, 400.0]);
        let grade = doc.add_node("grade", [250.0, 250.0]);
        doc.connect(&src, &blur, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        // The middle of the wire into `dst`'s input `port`.
        let mid = |h: &Harness, doc: &Doc, dst: &str, port: usize| {
            let d = doc.node(dst).unwrap();
            NodeEditor::out_port(h.rect(doc, &d.inputs[port])).lerp(NodeEditor::in_port(h.rect(doc, dst), port, d.inputs.len()), 0.5)
        };
        let inputs = |doc: &Doc, id: &str| doc.node(id).unwrap().inputs.clone();
        // A source's output onto src → blur: a Merge, it into A over src in B.
        let (out, to) = (NodeEditor::out_port(h.rect(&doc, &other)), mid(&h, &doc, &blur, 0));
        h.drag(&mut doc, out, to);
        let m1 = inputs(&doc, &blur)[0].clone();
        assert_eq!(doc.node(&m1).unwrap().op, "merge");
        assert_eq!(inputs(&doc, &m1)[..2], [other.clone(), src.clone()]);
        // A node with an input works the same, onto the new wire m1 → blur.
        let (out, to) = (NodeEditor::out_port(h.rect(&doc, &grade)), mid(&h, &doc, &blur, 0));
        h.drag(&mut doc, out, to);
        let m2 = inputs(&doc, &blur)[0].clone();
        assert_eq!(inputs(&doc, &m2)[..2], [grade.clone(), m1.clone()]);
        // A node downstream of the wire would close a cycle: nothing changes.
        let before = doc.graph.nodes.len();
        let (out, to) = (NodeEditor::out_port(h.rect(&doc, &blur)), mid(&h, &doc, &m1, 0));
        h.drag(&mut doc, out, to);
        assert_eq!((doc.graph.nodes.len(), inputs(&doc, &m1)[0].clone()), (before, other.clone()));
    }

    #[test]
    fn right_click_opens_the_node_browser_or_a_wires_menu() {
        let mut doc = Doc::new(Graph::default(), None);
        let src = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [500.0, 0.0]);
        doc.connect(&src, &blur, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let right = |pos: Pos2, pressed: bool| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
        let click = |h: &mut Harness, doc: &mut Doc, at: Pos2| {
            h.frame(doc, vec![egui::Event::PointerMoved(at)]);
            h.frame(doc, vec![right(at, true)]);
            h.frame(doc, vec![right(at, false)]);
            h.frame(doc, vec![]);
        };
        // Empty canvas: the browser opens there, with no wire to go into.
        let empty = Pos2::new(300.0, 400.0);
        click(&mut h, &mut doc, empty);
        assert_eq!(h.ed.browser.as_ref().map(|b| b.at), Some(empty));
        assert!(h.ed.target_wire.is_none());
        // On the wire: its menu, whose Insert Node… opens the browser aimed
        // at it; placing from it splices the new node in.
        h.ed.browser = None;
        let mid = NodeEditor::out_port(h.rect(&doc, &src)).lerp(NodeEditor::in_port(h.rect(&doc, &blur), 0, 2), 0.5);
        click(&mut h, &mut doc, mid);
        assert!(h.ed.browser.is_none() && h.ed.wire_menu.is_some());
        let w = h.ed.wire_menu.take().unwrap().0;
        h.ed.wire_action(&mut doc, &w, mid, WireAction::InsertNode);
        h.frame(&mut doc, vec![]);
        assert!(h.ed.browser.is_some());
        assert_eq!(h.ed.target_wire, Some(WireRef { src: src.clone(), dst: blur.clone(), port: 0 }));
        h.ed.place(&mut doc, ops::info("grade").unwrap(), h.ed.to_graph(mid), false);
        let grade = doc.selected.clone().unwrap();
        assert_eq!((doc.node(&grade).unwrap().inputs[0].as_str(), doc.node(&blur).unwrap().inputs[0].as_str()), (src.as_str(), grade.as_str()));
    }

    /// Placing a Camera brings its Scene and Scanline Render, with the
    /// selected node as the background and into a Viewer (made, as there's
    /// none), and selects the Camera; with Shift, just the Camera.
    #[test]
    fn a_camera_comes_with_its_rig_unless_shift_is_held() {
        let mut doc = Doc::new(Graph::default(), None);
        let plate = doc.add_node("checkerboard", [0.0, 0.0]);
        doc.selected = Some(plate.clone());
        let mut ed = NodeEditor::default();
        ed.place(&mut doc, ops::info("camera").unwrap(), Pos2::new(0.0, 100.0), false);
        let cam = doc.selected.clone().unwrap();
        assert_eq!(doc.node(&cam).unwrap().op, "camera");
        let render = doc.graph.nodes.iter().find(|n| n.op == "scanline_render").unwrap();
        assert_eq!(render.inputs[2], plate);
        let render = render.id.clone();
        assert!(doc.graph.nodes.iter().any(|n| n.op == "viewer" && n.inputs[0] == render), "shown in a new Viewer");
        let n = doc.graph.nodes.len();
        ed.place(&mut doc, ops::info("camera").unwrap(), Pos2::new(400.0, 100.0), true);
        assert_eq!(doc.graph.nodes.len(), n + 1, "Shift: the Camera alone");
    }

    #[test]
    fn shaking_needs_quick_reversals() {
        let mut s = Shake::default();
        let (r, l) = (Vec2::new(10.0, 0.0), Vec2::new(-10.0, 0.0));
        // Steady motion and tiny jitters don't count.
        assert!(!(0..10).any(|i| s.shaken(r, i as f64 * 0.01)));
        assert!(!s.shaken(Vec2::new(-1.0, 0.0), 0.11));
        // Three reversals in quick succession do, once.
        assert!(!s.shaken(l, 0.2));
        assert!(!s.shaken(r, 0.3));
        assert!(s.shaken(l, 0.4));
        assert!(!s.shaken(r, 0.5));
        // Slow back-and-forth doesn't.
        let mut s = Shake::default();
        assert!(!s.shaken(r, 0.0) && !s.shaken(l, 1.0) && !s.shaken(r, 2.0) && !s.shaken(l, 3.0));
    }

    #[test]
    fn wire_distance_follows_the_curve() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0));
        // The curve passes through its midpoint (by symmetry).
        assert!(bezier_distance(a, b, Pos2::new(100.0, 50.0), false) < 0.5);
        assert!(bezier_distance(a, b, Pos2::new(100.0, 80.0), false) > 10.0);
        assert!(bezier_distance(a, b, a, false) < 1e-3);
        // Straight: along the line; a point near the curve's bend is off it.
        assert!(bezier_distance(a, b, Pos2::new(50.0, 25.0), true) < 1e-3);
        assert!(bezier_distance(a, b, Pos2::new(38.9, 6.1), false) < 1.0 && bezier_distance(a, b, Pos2::new(38.9, 6.1), true) > 10.0);
    }

    #[test]
    fn snapping_lands_on_the_drawn_grid() {
        assert_eq!([snap(0.0), snap(19.0), snap(21.0), snap(-21.0), snap(118.0)], [0.0, 0.0, 40.0, -40.0, 120.0]);
    }

    #[test]
    fn browser_search_matches_words_and_ranks_prefixes() {
        assert_eq!(browse("").len(), arclight::ops::all().len());
        let names: Vec<_> = browse("pre").iter().map(|o| o.name).collect();
        assert_eq!(names.first(), Some(&"premult"));
        assert!(names.contains(&"unpremult"));
        assert_eq!(browse("channel sel").iter().map(|o| o.name).collect::<Vec<_>>(), ["channel_select"]);
        assert!(browse("zzz").is_empty());
    }

    impl Harness {
        fn press(&self, pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
            egui::Event::PointerButton { pos, button, pressed, modifiers: self.mods }
        }

        fn click(&mut self, doc: &mut Doc, at: Pos2) {
            self.frame(doc, vec![egui::Event::PointerMoved(at)]);
            let (down, up) = (self.press(at, egui::PointerButton::Primary, true), self.press(at, egui::PointerButton::Primary, false));
            self.frame(doc, vec![down]);
            self.frame(doc, vec![up]);
            self.frame(doc, vec![]);
        }

        /// `drag`, with any button and the held modifiers.
        fn drag_with(&mut self, doc: &mut Doc, button: egui::PointerButton, from: Pos2, to: Pos2) {
            self.frame(doc, vec![egui::Event::PointerMoved(from)]);
            let down = self.press(from, button, true);
            self.frame(doc, vec![down]);
            for i in 1..=8 {
                self.frame(doc, vec![egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0))]);
            }
            let up = self.press(to, button, false);
            self.frame(doc, vec![up]);
            self.frame(doc, vec![]);
        }

        fn screen(&self, p: [f32; 2]) -> Pos2 {
            self.ed.to_screen(Pos2::new(p[0], p[1]))
        }

        fn pos(doc: &Doc, id: &str) -> [f32; 2] {
            doc.node(id).unwrap().pos.unwrap()
        }
    }

    #[test]
    fn clicks_boxes_and_drags_select_and_move_several_nodes() {
        let mut doc = Doc::new(Graph::default(), None);
        let a = doc.add_node("constant", [0.0, 0.0]);
        let b = doc.add_node("constant", [300.0, 0.0]);
        let c = doc.add_node("constant", [0.0, 200.0]);
        let d = doc.add_node("constant", [600.0, 400.0]);
        doc.commit(false);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let mid = |h: &Harness, doc: &Doc, id: &str| h.rect(doc, id).center();
        let sel = |doc: &Doc| {
            let mut s = doc.selection();
            s.sort();
            s
        };

        // Click, Shift-click adds, Cmd/Ctrl-click takes out.
        let at = mid(&h, &doc, &a);
        h.click(&mut doc, at);
        assert_eq!(sel(&doc), std::slice::from_ref(&a));
        h.mods = Modifiers::SHIFT;
        let at = mid(&h, &doc, &b);
        h.click(&mut doc, at);
        assert_eq!(sel(&doc), [a.clone(), b.clone()]);
        assert_eq!(doc.selected.as_ref(), Some(&b), "the last picked is the primary");
        h.mods = Modifiers::COMMAND;
        let at = mid(&h, &doc, &a);
        h.click(&mut doc, at);
        assert_eq!(sel(&doc), std::slice::from_ref(&b));

        // A box on empty space: what it touches; Shift adds.
        h.mods = Modifiers::NONE;
        let (from, to) = (h.screen([-20.0, -20.0]), h.screen([320.0, 30.0]));
        h.drag(&mut doc, from, to);
        assert_eq!(sel(&doc), [a.clone(), b.clone()]);
        h.mods = Modifiers::SHIFT;
        let (from, to) = (h.screen([-20.0, 180.0]), h.screen([40.0, 220.0]));
        h.drag(&mut doc, from, to);
        assert_eq!(sel(&doc), [a.clone(), b.clone(), c.clone()]);
        assert_eq!(h.ed.pan, Vec2::new(40.0, 40.0), "a plain drag on empty space doesn't pan");

        // Dragging one selected node moves them all, as one undo step.
        h.mods = Modifiers::NONE;
        let grab = mid(&h, &doc, &b);
        h.drag(&mut doc, grab, grab + Vec2::new(100.0, 50.0));
        doc.commit(false);
        for (id, was) in [(&a, [0.0, 0.0]), (&b, [300.0, 0.0]), (&c, [0.0, 200.0])] {
            let p = Harness::pos(&doc, id);
            assert!((p[0] - was[0] - 100.0).abs() < 0.01 && (p[1] - was[1] - 50.0).abs() < 0.01, "{id}: {p:?}");
        }
        assert_eq!(Harness::pos(&doc, &d), [600.0, 400.0]);
        // One undo puts every node back (a step per frame would only undo
        // the last bit of the drag).
        doc.undo();
        assert_eq!((Harness::pos(&doc, &a), Harness::pos(&doc, &b), Harness::pos(&doc, &c)), ([0.0, 0.0], [300.0, 0.0], [0.0, 200.0]));

        // Middle-drag and Alt-drag pan; the selection stays.
        let before = sel(&doc);
        let e = h.screen([900.0, 0.0]);
        // (The first few points go to egui's drag threshold.)
        let moved = |pan: Vec2, by: Vec2| (pan.x - 40.0) / by.x > 0.8 && (pan.y - 40.0) / by.y > 0.8;
        h.drag_with(&mut doc, egui::PointerButton::Middle, e, e + Vec2::new(30.0, 20.0));
        assert!(moved(h.ed.pan, Vec2::new(30.0, 20.0)), "{:?}", h.ed.pan);
        h.ed.pan = Vec2::new(40.0, 40.0);
        h.mods = Modifiers::ALT;
        let e = h.screen([900.0, 0.0]);
        h.drag(&mut doc, e, e + Vec2::new(-30.0, -20.0));
        assert!(moved(h.ed.pan, Vec2::new(-30.0, -20.0)), "{:?}", h.ed.pan);
        assert_eq!(sel(&doc), before);
        assert_eq!(Harness::pos(&doc, &a), [0.0, 0.0]);

        // A click on empty space selects nothing.
        h.mods = Modifiers::NONE;
        let at = h.screen([900.0, 0.0]);
        h.click(&mut doc, at);
        assert!(doc.selection().is_empty());
    }

    #[test]
    fn backdrops_move_what_is_inside_resize_and_rename() {
        let mut doc = Doc::new(Graph::default(), None);
        let a = doc.add_node("constant", [0.0, 0.0]);
        let b = doc.add_node("blur", [200.0, 100.0]);
        let outside = doc.add_node("constant", [700.0, 0.0]);
        let partly = doc.add_node("constant", [300.0, 120.0]);
        doc.select_nodes(&[a.clone(), b.clone()], false);
        let bid = doc.group_selection().unwrap();
        assert_eq!(doc.selected_backdrop.as_ref(), Some(&bid));
        let bd = doc.graph.backdrop(&bid).unwrap().clone();
        assert_eq!((bd.pos, bd.size), ([-20.0, -50.0], [390.0, 216.0]), "fits a and b plus padding and the title");
        assert_eq!(doc.backdrop_nodes(&bid), [a.clone(), b.clone()], "partly inside doesn't count");
        doc.commit(false);

        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        // Drag the title bar: the box and the nodes inside it move.
        let grab = h.screen([bd.pos[0] + 60.0, bd.pos[1] + 15.0]);
        h.drag(&mut doc, grab, grab + Vec2::new(100.0, 40.0));
        doc.commit(false);
        let near = |p: [f32; 2], q: [f32; 2]| (p[0] - q[0]).abs() < 0.01 && (p[1] - q[1]).abs() < 0.01;
        assert!(near(doc.graph.backdrop(&bid).unwrap().pos, [80.0, -10.0]));
        assert!(near(Harness::pos(&doc, &a), [100.0, 40.0]) && near(Harness::pos(&doc, &b), [300.0, 140.0]));
        assert_eq!((Harness::pos(&doc, &outside), Harness::pos(&doc, &partly)), ([700.0, 0.0], [300.0, 120.0]));

        // The bottom-right corner resizes it.
        let bd = doc.graph.backdrop(&bid).unwrap().clone();
        let corner = h.screen([bd.pos[0] + bd.size[0] - 5.0, bd.pos[1] + bd.size[1] - 5.0]);
        h.drag(&mut doc, corner, corner + Vec2::new(50.0, 30.0));
        doc.commit(false);
        let grown = doc.graph.backdrop(&bid).unwrap().clone();
        assert!(near(grown.pos, bd.pos) && near(grown.size, [bd.size[0] + 50.0, bd.size[1] + 30.0]), "{grown:?}");

        // Double-click the title to rename it in place.
        let title = h.screen([grown.pos[0] + 60.0, grown.pos[1] + 15.0]);
        h.frame(&mut doc, vec![egui::Event::PointerMoved(title)]);
        for _ in 0..2 {
            h.frame(&mut doc, vec![Harness::button(title, true)]);
            h.frame(&mut doc, vec![Harness::button(title, false)]);
        }
        h.frame(&mut doc, vec![]);
        h.frame(&mut doc, vec![egui::Event::Text(" A".into())]);
        h.frame(&mut doc, vec![egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        h.frame(&mut doc, vec![]);
        assert_eq!(doc.graph.backdrop(&bid).unwrap().title, "Group A");
        doc.commit(false);

        // Each gesture undoes on its own; deleting the box leaves the nodes.
        doc.undo();
        assert_eq!(doc.graph.backdrop(&bid).unwrap().title, "Group");
        doc.undo();
        assert!(near(doc.graph.backdrop(&bid).unwrap().size, bd.size));
        doc.delete_backdrop(&bid);
        doc.commit(false);
        assert!(doc.graph.backdrops.is_empty() && doc.node(&a).is_some() && doc.node(&b).is_some());
        doc.undo();
        assert!(doc.graph.backdrop(&bid).is_some());
    }

    #[test]
    fn fitting_centers_the_graph_in_any_panel() {
        // Graph bounds → zoom and pan, for several panel sizes.
        let bounds = Rect::from_min_max(Pos2::new(-100.0, 50.0), Pos2::new(1900.0, 650.0));
        let framed = bounds.expand(30.0);
        for size in [Vec2::new(1200.0, 700.0), Vec2::new(500.0, 900.0), Vec2::new(2000.0, 200.0), Vec2::new(300.0, 120.0)] {
            let (zoom, pan) = fit_view(bounds, size);
            let on_screen = |g: Pos2| pan + g.to_vec2() * zoom;
            let r = Rect::from_min_max(on_screen(framed.min).to_pos2(), on_screen(framed.max).to_pos2());
            assert!((r.center() - (size / 2.0).to_pos2()).length() < 1e-3, "{size:?}: centered");
            if zoom > ZOOM_MIN {
                assert!(r.width() <= size.x + 1e-3 && r.height() <= size.y + 1e-3, "{size:?}: all in view with the margin");
                assert!((r.width() - size.x).abs() < 1e-3 || (r.height() - size.y).abs() < 1e-3, "{size:?}: proportional, touching one side");
            }
            assert!((ZOOM_MIN..=FIT_MAX_ZOOM).contains(&zoom));
        }
        assert_eq!(fit_view(bounds, Vec2::new(300.0, 120.0)).0, ZOOM_MIN, "a huge graph stops at the farthest zoom");
        let small = Rect::from_min_size(Pos2::ZERO, Vec2::new(NODE_W, NODE_H));
        assert_eq!(fit_view(small, Vec2::new(1200.0, 700.0)).0, FIT_MAX_ZOOM, "a node or two isn't blown up");
    }

    #[test]
    fn the_fit_button_frames_every_node_and_backdrop() {
        let mut doc = Doc::new(Graph::default(), None);
        let mut h = Harness::new();
        // An empty graph: no button, and fitting does nothing.
        h.ed.pan = Vec2::new(12.0, 34.0);
        h.ed.fit();
        h.frame(&mut doc, vec![]);
        assert_eq!((h.ed.zoom, h.ed.pan), (1.0, Vec2::new(12.0, 34.0)));

        let a = doc.add_node("constant", [0.0, 0.0]);
        doc.add_node("blur", [2400.0, 900.0]);
        doc.select_nodes(&[a], false);
        doc.group_selection().unwrap();
        h.frame(&mut doc, vec![]);
        assert_eq!(h.ed.zoom, 1.0, "nothing moves until asked");
        let button = h.ed.rect.right_top() + Vec2::new(-26.0, 18.0);
        h.frame(&mut doc, vec![egui::Event::PointerMoved(button)]);
        h.frame(&mut doc, vec![Harness::button(button, true)]);
        h.frame(&mut doc, vec![Harness::button(button, false)]);
        h.frame(&mut doc, vec![]);
        let (zoom, pan) = fit_view(content_bounds(&doc), h.ed.rect.size());
        assert!(zoom < 1.0 && (h.ed.zoom, h.ed.pan) == (zoom, pan), "the button fits");
        assert!(doc.graph.nodes.iter().all(|n| h.ed.rect.contains_rect(h.ed.node_rect(n.pos.unwrap()))));
        let bd = &doc.graph.backdrops[0];
        assert!(h.ed.rect.contains(h.ed.to_screen(Pos2::new(bd.pos[0], bd.pos[1]))), "and the backdrops");
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn keys_over_the_graph_open_the_browser_and_add_nodes() {
        let mut doc = Doc::new(Graph::default(), None);
        let plate = doc.add_node("checkerboard", [100.0, 100.0]);
        let grade = doc.add_node("grade", [100.0, 300.0]);
        doc.connect(&plate, &grade, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let shift = |h: &mut Harness, doc: &mut Doc, at: Pos2, k: Key| {
            h.mods = Modifiers::SHIFT;
            h.frame(doc, vec![egui::Event::PointerMoved(at), key(k, Modifiers::SHIFT)]);
            h.mods = Modifiers::NONE;
            h.frame(doc, vec![]);
        };
        let op = |doc: &Doc, id: &Option<String>| doc.node(id.as_ref().unwrap()).unwrap().op.clone();

        // Plain Tab: nothing here (main.rs takes it first, anywhere, to
        // switch 2D / 3D), and keyboard focus stays put.
        let (a, b) = (h.rect(&doc, &plate), h.rect(&doc, &grade));
        let mid = NodeEditor::out_port(a).lerp(NodeEditor::in_port(b, 0, 2), 0.5);
        h.frame(&mut doc, vec![egui::Event::PointerMoved(mid), key(Key::Tab, Modifiers::NONE)]);
        h.frame(&mut doc, vec![]);
        assert!(!h.ed.browser_open() && h.ctx.memory(|m| m.focused()).is_none(), "plain Tab does nothing");
        // Shift+Tab: the browser at the pointer, aimed at the wire under it.
        h.mods = Modifiers::SHIFT;
        h.frame(&mut doc, vec![key(Key::Tab, Modifiers::SHIFT)]);
        h.mods = Modifiers::NONE;
        assert!(h.ed.browser.as_ref().is_some_and(|b| b.at == mid), "Shift+Tab opens the browser there");
        assert_eq!(h.ed.target_wire.as_ref().map(|w| (w.src.as_str(), w.dst.as_str())), Some((plate.as_str(), grade.as_str())));
        h.frame(&mut doc, vec![]);
        h.frame(&mut doc, vec![key(Key::Escape, Modifiers::NONE)]);
        h.frame(&mut doc, vec![]);
        assert!(!h.ed.browser_open() && !h.ctx.text_edit_focused());

        // Shift+T over the wire: a Transform spliced into it, at the pointer.
        shift(&mut h, &mut doc, mid, Key::T);
        let t = doc.selected.clone();
        assert_eq!(op(&doc, &t), "transform");
        assert_eq!(doc.node(&grade).unwrap().inputs[0], *t.as_ref().unwrap());
        assert_eq!(doc.node(t.as_ref().unwrap()).unwrap().inputs[0], plate);
        let pos = doc.node(t.as_ref().unwrap()).unwrap().pos.unwrap();
        assert!((h.ed.to_screen(Pos2::new(pos[0], pos[1])) - mid).length() < 0.01);
        // Shift+M in empty space: a Merge after the selected Transform.
        let empty = h.ed.rect.center() + Vec2::new(300.0, 0.0);
        shift(&mut h, &mut doc, empty, Key::M);
        let m = doc.selected.clone();
        assert_eq!(op(&doc, &m), "merge");
        assert_eq!(doc.node(m.as_ref().unwrap()).unwrap().inputs[0], *t.as_ref().unwrap());
        // Shift+R: a Media In (no inputs to wire).
        shift(&mut h, &mut doc, empty + Vec2::new(0.0, 120.0), Key::R);
        assert_eq!(op(&doc, &doc.selected), "media_in");
        assert_eq!(doc.graph.nodes.len(), 5);
        // A selected wire takes the new node when the pointer is off wires.
        doc.select_wire(&plate, t.as_ref().unwrap(), 0);
        shift(&mut h, &mut doc, empty + Vec2::new(0.0, -150.0), Key::T);
        let t2 = doc.selected.clone().unwrap();
        assert_eq!(doc.node(t.as_ref().unwrap()).unwrap().inputs[0], t2);
        // Plain R/T/M add nothing; nor does Shift+M typed in the browser's
        // search field.
        h.frame(&mut doc, vec![egui::Event::PointerMoved(empty), key(Key::R, Modifiers::NONE), key(Key::M, Modifiers::NONE)]);
        assert_eq!(doc.graph.nodes.len(), 6);
        h.mods = Modifiers::SHIFT;
        h.frame(&mut doc, vec![key(Key::Tab, Modifiers::SHIFT)]);
        h.mods = Modifiers::NONE;
        h.frame(&mut doc, vec![]);
        assert!(h.ctx.text_edit_focused(), "the browser's search field has focus");
        shift(&mut h, &mut doc, empty, Key::M);
        assert_eq!(doc.graph.nodes.len(), 6);
    }

    #[test]
    fn keys_elsewhere_leave_the_graph_alone() {
        // The editor in the lower half; the upper half stands in for the
        // viewer, whose own R (rotate) needs the pointer over it.
        let mut doc = Doc::new(Graph::default(), None);
        doc.add_node("checkerboard", [100.0, 100.0]);
        let ctx = egui::Context::default();
        let mut ed = NodeEditor { zoom: 1.0, fit_pending: false, ..NodeEditor::default() };
        let run = |ed: &mut NodeEditor, doc: &mut Doc, events: Vec<egui::Event>, mods: Modifiers| {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 700.0))),
                events: std::iter::once(egui::Event::ModifiersChanged(mods)).chain(events).collect(),
                ..Default::default()
            };
            let mut viewer_got_r = false;
            ctx.run_ui(raw, |ui| {
                let (top, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 300.0), Sense::hover());
                ed.ui(ui, doc, None);
                viewer_got_r = ui.rect_contains_pointer(top) && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::R));
            }).drop_without_applying_deltas();
            viewer_got_r
        };
        let over_viewer = Pos2::new(600.0, 150.0);
        assert!(run(&mut ed, &mut doc, vec![egui::Event::PointerMoved(over_viewer), key(Key::R, Modifiers::SHIFT)], Modifiers::SHIFT), "the viewer still gets its key");
        run(&mut ed, &mut doc, vec![egui::Event::PointerMoved(over_viewer), key(Key::Tab, Modifiers::SHIFT), key(Key::M, Modifiers::SHIFT)], Modifiers::SHIFT);
        run(&mut ed, &mut doc, vec![], Modifiers::NONE);
        assert!(ed.browser.is_none() && doc.graph.nodes.len() == 1, "nothing added from over the viewer");
        run(&mut ed, &mut doc, vec![egui::Event::PointerMoved(Pos2::new(600.0, 500.0)), key(Key::M, Modifiers::SHIFT)], Modifiers::SHIFT);
        assert_eq!(doc.graph.nodes.len(), 2, "over the graph it adds");
    }

    #[test]
    fn particles_go_into_a_scanline_renders_spare_input() {
        let mut doc = Doc::new(Graph::default(), None);
        let render = doc.add_node("scanline_render", [400.0, 0.0]);
        let inputs = |doc: &Doc| doc.node(&render).unwrap().inputs.clone();
        assert_eq!(inputs(&doc).len(), 4, "scene, camera, background and one spare");
        // Added with the render selected, an Emitter takes the spare, and
        // a new spare appears.
        doc.selected = Some(render.clone());
        let mut ed = NodeEditor::default();
        ed.place(&mut doc, ops::info("particle_emitter").unwrap(), Pos2::ZERO, false);
        let emitter = doc.selected.clone().unwrap();
        assert_eq!(inputs(&doc), ["", "", "", emitter.as_str(), ""]);
        assert_eq!(ops::port("scanline_render", 4).map(|p| p.label), Some("scene"));
        // Disconnected: back to one spare.
        doc.disconnect(&render, 3);
        assert_eq!(inputs(&doc).len(), 4);
        // A particles input takes particles only.
        let gravity = doc.add_node("particle_gravity", [200.0, 0.0]);
        let scene = doc.add_node("scene", [0.0, 200.0]);
        assert!(fits(&doc, &emitter, &gravity, 0) && !fits(&doc, &scene, &gravity, 0));
        assert!(fits(&doc, &gravity, &render, 3), "and a stream goes where scenes go");
        assert_eq!(ops::wire_error("scene", "particle_gravity", 0).as_deref(), Some("input 'particles' takes particles, not a scene"));
        assert!(ops::wire_error("particle_gravity", "merge", 0).is_some());
    }

    /// A node's switch turns it off and on; D does it for the selection,
    /// each one undo step.
    #[test]
    fn the_switch_and_d_turn_nodes_off_and_on() {
        let mut doc = Doc::new(Graph::default(), None);
        let a = doc.add_node("constant", [0.0, 0.0]);
        let b = doc.add_node("grade", [300.0, 0.0]);
        doc.commit(false);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let off = |doc: &Doc, id: &str| doc.node(id).unwrap().disabled;
        // The switch: just that node, without selecting it.
        let at = h.ed.switch_rect(h.rect(&doc, &a)).center();
        h.click(&mut doc, at);
        doc.commit(false);
        assert!(off(&doc, &a) && !off(&doc, &b));
        assert!(doc.selection().is_empty(), "the switch doesn't select");
        // D over the graph: the selection off (one on still), then on.
        doc.select_nodes(&[a.clone(), b.clone()], false);
        let over = h.rect(&doc, &b).center() + Vec2::new(0.0, 120.0);
        h.frame(&mut doc, vec![egui::Event::PointerMoved(over), key(Key::D, Modifiers::NONE)]);
        doc.commit(false);
        assert!(off(&doc, &a) && off(&doc, &b));
        h.frame(&mut doc, vec![key(Key::D, Modifiers::NONE)]);
        doc.commit(false);
        assert!(!off(&doc, &a) && !off(&doc, &b));
        // Undo: one step each.
        doc.undo();
        assert!(off(&doc, &a) && off(&doc, &b));
        doc.undo();
        assert!(off(&doc, &a) && !off(&doc, &b));
        doc.undo();
        assert!(!off(&doc, &a));
    }
}
