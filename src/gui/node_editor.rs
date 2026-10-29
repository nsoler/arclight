//! The node graph editor.
//!
//! Inputs sit on a node's left edge, its output on the right. Drag from an
//! output to an input (or the reverse) to connect; drag a connected input away
//! to reroute or disconnect. Drop a wire dragged from an output onto another
//! wire to put the node into it (or, when it can't go in, to feed that
//! wire's input instead). Inputs are yellow squares, outputs green circles.
//! Right-click the canvas, or press Shift+E for a
//! searchable node browser, to add nodes; over a wire, the new node goes into
//! it. Drop an unconnected node on a wire to splice it in; shake a node while
//! dragging it to pull it out of its wires. Optional ports draw hollow. The
//! minimap (lower right) pans on click/drag, zooms on scroll, fits on
//! double-click.

use crate::doc::{Doc, NODE_H, NODE_W};
use arclight::ops::{self, Category};
use arclight::ops::OpInfo;
use eframe::egui::{
    self, epaint::CubicBezierShape, Align2, Color32, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2,
};

const PORT_R: f32 = 5.5;
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
    /// Where the add-node menu was opened, in graph coordinates.
    menu_at: Pos2,
    fit_pending: bool,
    browser: Option<Browser>,
    /// The wire a node added from the menu or browser goes into.
    target_wire: Option<WireRef>,
    /// The wire a dragged, unconnected node would be dropped into.
    drop_wire: Option<WireRef>,
    shake: Shake,
    /// The minimap's graph bounds, held while dragging in it.
    minimap_bounds: Option<Rect>,
    pub rect: Rect,
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
            menu_at: Pos2::ZERO,
            fit_pending: true,
            browser: None,
            target_wire: None,
            drop_wire: None,
            shake: Shake::default(),
            minimap_bounds: None,
            rect: Rect::NOTHING,
        }
    }
}

pub fn category_color(c: Category) -> Color32 {
    match c {
        Category::Source => Color32::from_rgb(58, 104, 70),
        Category::Color => Color32::from_rgb(104, 76, 136),
        Category::Channel => Color32::from_rgb(52, 118, 124),
        Category::Merge => Color32::from_rgb(56, 86, 136),
        Category::Shape => Color32::from_rgb(140, 70, 108),
        Category::Transform => Color32::from_rgb(140, 104, 48),
        Category::Filter => Color32::from_rgb(132, 74, 58),
        Category::Output => Color32::from_rgb(84, 86, 92),
    }
}

const CATEGORIES: [(Category, &str); 8] = [
    (Category::Source, "Source"),
    (Category::Color, "Color"),
    (Category::Channel, "Channel"),
    (Category::Merge, "Merge"),
    (Category::Shape, "Shape"),
    (Category::Transform, "Transform"),
    (Category::Filter, "Filter"),
    (Category::Output, "Output"),
];

fn category_name(c: Category) -> &'static str {
    CATEGORIES.iter().find(|(k, _)| *k == c).map_or("", |(_, n)| n)
}

fn has_output(op: &str) -> bool {
    op != "viewer"
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
    fn place(&mut self, doc: &mut Doc, op: &OpInfo, at: Pos2) {
        let upstream = doc.selected.clone();
        let id = doc.add_node(op.name, [at.x, at.y]);
        if let Some(w) = self.target_wire.take() {
            if doc.insert_on_wire(&id, &w.src, &w.dst, w.port) {
                doc.selected = Some(id);
                return;
            }
        }
        if let Some(up) = upstream {
            if !op.inputs.is_empty() && doc.node(&up).is_some_and(|n| has_output(&n.op)) {
                doc.connect(&up, &id, 0);
            }
        }
        doc.selected = Some(id);
    }

    fn browser_ui(&mut self, ctx: &egui::Context, doc: &mut Doc) {
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

        let area = egui::Area::new(Id::unique("node_browser"))
            .order(egui::Order::Foreground)
            .fixed_pos(b.at)
            // Grow upward in the lower half of the screen so the list fits.
            .pivot(if b.at.y > ctx.content_rect().center().y { Align2::LEFT_BOTTOM } else { Align2::LEFT_TOP })
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(230.0);
                    let edit = ui.add(egui::TextEdit::singleline(&mut b.query).hint_text("Search nodes…"));
                    if b.just_opened {
                        edit.request_focus();
                        b.just_opened = false;
                    }
                    if edit.changed() {
                        b.sel = 0;
                    }
                    ui.separator();
                    if hits.is_empty() {
                        ui.weak("No matching nodes");
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        let mut last_cat = None;
                        for (i, op) in hits.iter().enumerate() {
                            if last_cat != Some(op.category) {
                                last_cat = Some(op.category);
                                ui.label(egui::RichText::new(category_name(op.category)).small().color(category_color(op.category).gamma_multiply(1.8)));
                            }
                            let resp = ui.selectable_label(i == b.sel, op.label).on_hover_text(op.help);
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
            self.place(doc, op, self.to_graph(at));
        }
    }

    fn to_screen(&self, p: Pos2) -> Pos2 {
        self.rect.min + self.pan + p.to_vec2() * self.zoom
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

        if self.fit_pending {
            self.fit_to(doc);
        }
        let minimap = self.minimap_rect(doc);
        self.navigate(ui, &bg, minimap);
        self.draw_grid(&painter);

        if bg.clicked() {
            doc.selected = None;
        }
        if bg.secondary_clicked() {
            if let Some(p) = bg.interact_pointer_pos() {
                self.menu_at = self.to_graph(p);
                self.target_wire = self.wire_at(doc, p, None);
            }
        }
        bg.context_menu(|ui| self.add_menu(ui, doc));
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
                    painter.add(bezier(a, b, if on { Stroke::new(3.0, Color32::from_rgb(250, 190, 70)) } else { wire_stroke }));
                }
            }
        }

        // Nodes.
        for (id, op, r, _) in &geo {
            let resp = ui.interact(*r, Id::unique(("node", id)), Sense::click_and_drag());
            if resp.clicked() || resp.drag_started() {
                doc.selected = Some(id.clone());
            }
            if resp.drag_started() {
                self.shake = Shake { node: id.clone(), ..Default::default() };
            }
            if resp.dragged() {
                let d = resp.drag_delta() / self.zoom;
                if let Some(n) = doc.node_mut(id) {
                    let p = n.pos.get_or_insert([0.0, 0.0]);
                    p[0] += d.x;
                    p[1] += d.y;
                }
                doc.touched();
                let now = ui.input(|i| i.time);
                if self.shake.node == *id && self.shake.shaken(resp.drag_delta(), now) {
                    doc.extract(id);
                }
                // An unconnected node can be dropped into a wire.
                self.drop_wire = None;
                if is_free(doc, id) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.drop_wire = self.wire_at(doc, p, Some(id));
                    }
                }
            }
            if resp.drag_stopped() {
                if let Some(w) = self.drop_wire.take() {
                    doc.insert_on_wire(id, &w.src, &w.dst, w.port);
                }
            }
            if resp.double_clicked() {
                doc.view_node(id);
            }
            let selected = doc.selected.as_deref() == Some(id.as_str());
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
                let port = ops::info(op).and_then(|o| o.inputs.get(i));
                port_dot(&painter, p, self.zoom, PortKind::Input, hot, port.is_some_and(|p| p.optional));
                if *n_in > 1 && self.zoom > 0.6 {
                    let label = port.map_or("", |p| p.label);
                    // Outside the node, just above the incoming wire, clear of the title.
                    painter.text(p + Vec2::new(-PORT_R * 1.4, -PORT_R * 0.6), Align2::RIGHT_BOTTOM, label, FontId::proportional(10.0 * self.zoom), Color32::from_gray(210));
                }
            }
            if has_output(op) {
                let p = Self::out_port(*r);
                let resp = ui.interact(Rect::from_center_size(p, Vec2::splat(PORT_R * 3.0)), Id::unique(("out", id)), Sense::drag());
                if resp.drag_started() {
                    self.wire = Some(Wire::FromOutput(id.clone()));
                }
                port_dot(&painter, p, self.zoom, PortKind::Output, resp.hovered() || resp.dragged(), false);
            }
        }

        // An in-progress wire follows the pointer and connects on release.
        // From an output, a wire under the pointer (away from any input) is
        // lit as the drop target.
        if let (Some(Wire::FromOutput(src)), Some(ptr)) = (&self.wire, pointer) {
            let lit = self.input_at(&geo, ptr).is_none().then(|| self.wire_at(doc, ptr, Some(src))).flatten();
            if let Some(w) = lit {
                if let (Some(from), Some(to)) = (rect_of(&w.src), geo.iter().find(|g| g.0 == w.dst)) {
                    painter.add(bezier(Self::out_port(from), Self::in_port(to.2, w.port, to.3), Stroke::new(3.0, Color32::from_rgb(250, 190, 70))));
                }
            }
        }
        if let (Some(wire), Some(ptr)) = (&self.wire, pointer) {
            let anchor = match wire {
                Wire::FromOutput(src) => rect_of(src).map(Self::out_port),
                Wire::FromInput(dst, i) => rect_of(dst).map(|r| Self::in_port(r, *i, geo.iter().find(|g| &g.0 == dst).map_or(1, |g| g.3))),
            };
            if let Some(a) = anchor {
                let s = Stroke::new(2.0, Color32::from_rgb(240, 170, 60));
                match wire {
                    Wire::FromOutput(_) => painter.add(bezier(a, ptr, s)),
                    Wire::FromInput(..) => painter.add(bezier(ptr, a, s)),
                };
            }
        }
        if self.wire.is_some() && ui.input(|i| i.pointer.any_released()) {
            if let (Some(wire), Some(ptr)) = (self.wire.take(), pointer) {
                match wire {
                    Wire::FromOutput(src) => {
                        if let Some((dst, i)) = self.input_at(&geo, ptr) {
                            doc.connect(&src, &dst, i);
                        } else if let Some(w) = self.wire_at(doc, ptr, Some(&src)) {
                            // Onto a wire: into it, or failing that, feed its input.
                            if !(is_spliceable(doc, &src) && doc.insert_on_wire(&src, &w.src, &w.dst, w.port)) {
                                doc.connect(&src, &w.dst, w.port);
                            }
                        }
                    }
                    Wire::FromInput(dst, i) => {
                        let hit = geo
                            .iter()
                            .filter(|(_, op, ..)| has_output(op))
                            .map(|(id, _, r, _)| (id, Self::out_port(*r).distance(ptr)))
                            .filter(|&(_, d)| d < PORT_R * 2.5)
                            .min_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((src, _)) = hit {
                            doc.connect(src, &dst, i);
                        }
                    }
                }
            }
        }
        if self.wire.is_some() {
            ui.ctx().request_repaint();
        }
        if let Some(mm) = minimap {
            self.minimap(ui, &painter, doc, mm);
        }
    }

    /// The input port in reach of screen point `p`: the nearest, not the
    /// first, since a Merge's ports sit closer together than the reach.
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
                let d = bezier_distance(Self::out_port(from), Self::in_port(to, i, n.inputs.len()), p);
                if d < WIRE_GRAB && best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, WireRef { src: src.clone(), dst: n.id.clone(), port: i }));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// The graph's bounds and the current view, in graph coordinates.
    fn graph_bounds(&self, doc: &Doc) -> Rect {
        let mut b = Rect::from_min_max(self.to_graph(self.rect.min), self.to_graph(self.rect.max));
        for n in &doc.graph.nodes {
            let p = n.pos.unwrap_or([0.0, 0.0]);
            b = b.union(Rect::from_min_size(Pos2::new(p[0], p[1]), Vec2::new(NODE_W, NODE_H)));
        }
        b.expand(40.0)
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
                self.zoom = (self.zoom * (scroll * 0.004).exp()).clamp(0.25, 3.0);
                center_on(self, to_graph(h));
            }
        }

        painter.rect(mm, 4.0, Color32::from_black_alpha(170), Stroke::new(1.0, Color32::from_gray(80)), egui::StrokeKind::Inside);
        let clip = painter.with_clip_rect(mm);
        for n in &doc.graph.nodes {
            let p = n.pos.unwrap_or([0.0, 0.0]);
            let r = Rect::from_min_max(to_map(Pos2::new(p[0], p[1])), to_map(Pos2::new(p[0] + NODE_W, p[1] + NODE_H)));
            let color = ops::info(&n.op).map_or(Color32::from_gray(80), |i| category_color(i.category));
            clip.rect_filled(r, 1.0, color);
            if doc.selected.as_deref() == Some(n.id.as_str()) {
                clip.rect_stroke(r, 1.0, Stroke::new(1.0, Color32::from_rgb(250, 190, 70)), egui::StrokeKind::Outside);
            }
        }
        let view = Rect::from_min_max(to_map(self.to_graph(self.rect.min)), to_map(self.to_graph(self.rect.max)));
        let hot = resp.hovered() || resp.dragged();
        clip.rect_stroke(view, 1.0, Stroke::new(1.0, Color32::from_white_alpha(if hot { 230 } else { 150 })), egui::StrokeKind::Inside);
    }

    fn navigate(&mut self, ui: &egui::Ui, bg: &egui::Response, minimap: Option<Rect>) {
        if bg.dragged() {
            self.pan += bg.drag_delta();
        }
        if !ui.rect_contains_pointer(self.rect) || minimap.is_some_and(|m| ui.rect_contains_pointer(m)) {
            return;
        }
        let (scroll, zoom, hover) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        if zoom != 1.0 {
            if let Some(h) = hover {
                let before = self.to_graph(h);
                self.zoom = (self.zoom * zoom).clamp(0.25, 3.0);
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
        let mut b = Rect::NOTHING;
        for n in &doc.graph.nodes {
            let p = n.pos.unwrap_or([0.0, 0.0]);
            b = b.union(Rect::from_min_size(Pos2::new(p[0], p[1]), Vec2::new(NODE_W, NODE_H)));
        }
        let b = b.expand(30.0);
        self.zoom = (self.rect.width() / b.width()).min(self.rect.height() / b.height()).clamp(0.3, 1.25);
        self.pan = self.rect.size() / 2.0 - b.center().to_vec2() * self.zoom;
    }

    fn draw_grid(&self, painter: &egui::Painter) {
        let step = 40.0 * self.zoom;
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
        let base = info.map_or(Color32::from_gray(80), |i| category_color(i.category));
        let z = self.zoom;
        painter.rect_filled(r.translate(Vec2::splat(3.0 * z)), 6.0 * z, Color32::from_black_alpha(90));
        painter.rect_filled(r, 6.0 * z, base);
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
            FontId::proportional(13.0 * z),
            Color32::WHITE,
        );
        if doc.is_node_animated(id) {
            // Keyframed params: a small diamond in the top-right corner.
            let c = Pos2::new(r.right() - 10.0 * z, r.top() + 10.0 * z);
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
                FontId::proportional(10.5 * z),
                Color32::from_white_alpha(170),
            );
        }
    }

    fn add_menu(&mut self, ui: &mut egui::Ui, doc: &mut Doc) {
        if let Some(w) = &self.target_wire {
            ui.label(egui::RichText::new(format!("Insert between {} and {}", w.src, w.dst)).weak());
            ui.separator();
        }
        for (cat, name) in CATEGORIES {
            ui.menu_button(name, |ui| {
                for op in ops::all().into_iter().filter(|o| o.category == cat) {
                    if ui.button(op.label).on_hover_text(op.help).clicked() {
                        self.place(doc, op, self.menu_at);
                        ui.close();
                    }
                }
            });
        }
    }
}

fn bezier(a: Pos2, b: Pos2, stroke: Stroke) -> CubicBezierShape {
    let dx = ((b.x - a.x).abs() * 0.5).max(40.0);
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

/// A node whose output can be dropped into a wire: one with an input, the
/// first of which is free (splicing it in sets that input).
fn is_spliceable(doc: &Doc, id: &str) -> bool {
    doc.node(id).is_some_and(|n| n.inputs.first().is_some_and(String::is_empty) && has_output(&n.op))
}

/// Distance from `p` to the wire drawn from `a` to `b` (see [`bezier`]).
fn bezier_distance(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    let dx = ((b.x - a.x).abs() * 0.5).max(40.0);
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

fn port_dot(painter: &egui::Painter, p: Pos2, zoom: f32, kind: PortKind, hot: bool, optional: bool) {
    let r = PORT_R * zoom.max(0.6);
    let color = match (kind, hot) {
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
    }

    impl Harness {
        /// At 1:1 zoom (where a Merge's three ports sit 11.5 points apart).
        fn new() -> Harness {
            let ed = NodeEditor { zoom: 1.0, fit_pending: false, ..NodeEditor::default() };
            Harness { ctx: egui::Context::default(), ed, time: 0.0 }
        }

        fn frame(&mut self, doc: &mut Doc, events: Vec<egui::Event>) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 700.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.time += 1.0 / 60.0;
            let ed = &mut self.ed;
            let _ = self.ctx.run_ui(raw, |ui| ed.ui(ui, doc, None));
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
        h.ed.place(&mut doc, ops::info("merge").unwrap(), at);
        let merge = doc.selected.clone().unwrap();
        h.frame(&mut doc, vec![]);
        let to = NodeEditor::in_port(h.rect(&doc, &merge), 1, 3);
        let out = NodeEditor::out_port(h.rect(&doc, &other));
        h.drag(&mut doc, out, to);
        assert_eq!(doc.node(&merge).unwrap().inputs[1], other, "output → B");
    }

    #[test]
    fn an_output_dropped_on_a_wire_goes_into_it() {
        let mut doc = Doc::new(Graph::default(), None);
        let src = doc.add_node("constant", [0.0, 0.0]);
        let blur = doc.add_node("blur", [500.0, 0.0]);
        let grade = doc.add_node("grade", [250.0, 250.0]);
        let other = doc.add_node("constant", [0.0, 400.0]);
        doc.connect(&src, &blur, 0);
        let mut h = Harness::new();
        h.frame(&mut doc, vec![]);
        let mid = |h: &Harness, doc: &Doc| {
            let b = doc.node(&blur).unwrap();
            NodeEditor::out_port(h.rect(doc, &b.inputs[0])).lerp(NodeEditor::in_port(h.rect(doc, &blur), 0, b.inputs.len()), 0.5)
        };
        // A free node's output spliced in between…
        let out = NodeEditor::out_port(h.rect(&doc, &grade));
        let to = mid(&h, &doc);
        h.drag(&mut doc, out, to);
        assert_eq!(doc.node(&grade).unwrap().inputs[0], src, "src → grade");
        assert_eq!(doc.node(&blur).unwrap().inputs[0], grade, "grade → blur");
        // …and a source, which can't go in, feeds the wire's input instead.
        let out = NodeEditor::out_port(h.rect(&doc, &other));
        let to = mid(&h, &doc);
        h.drag(&mut doc, out, to);
        assert_eq!(doc.node(&blur).unwrap().inputs[0], other, "other → blur");
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
        assert!(bezier_distance(a, b, Pos2::new(100.0, 50.0)) < 0.5);
        assert!(bezier_distance(a, b, Pos2::new(100.0, 80.0)) > 10.0);
        assert!(bezier_distance(a, b, a) < 1e-3);
    }

    #[test]
    fn browser_search_matches_words_and_ranks_prefixes() {
        assert_eq!(browse("").len(), arclight::ops::OPS.len());
        let names: Vec<_> = browse("pre").iter().map(|o| o.name).collect();
        assert_eq!(names.first(), Some(&"premult"));
        assert!(names.contains(&"unpremult"));
        assert_eq!(browse("channel sel").iter().map(|o| o.name).collect::<Vec<_>>(), ["channel_select"]);
        assert!(browse("zzz").is_empty());
    }
}
