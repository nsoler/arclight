//! The node graph editor.
//!
//! Inputs sit on a node's left edge, its output on the right. Drag from an
//! output to an input (or the reverse) to connect; drag a connected input away
//! to reroute or disconnect. Right-click the canvas to add nodes.

use crate::doc::{Doc, NODE_H, NODE_W};
use arclight::ops::{self, Category, OPS};
use eframe::egui::{
    self, epaint::CubicBezierShape, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2,
};

const PORT_R: f32 = 5.5;

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
    pub rect: Rect,
}

impl Default for NodeEditor {
    fn default() -> Self {
        NodeEditor {
            pan: Vec2::new(40.0, 40.0),
            zoom: 1.0,
            wire: None,
            menu_at: Pos2::ZERO,
            fit_pending: true,
            rect: Rect::NOTHING,
        }
    }
}

pub fn category_color(c: Category) -> Color32 {
    match c {
        Category::Source => Color32::from_rgb(58, 104, 70),
        Category::Color => Color32::from_rgb(104, 76, 136),
        Category::Merge => Color32::from_rgb(56, 86, 136),
        Category::Transform => Color32::from_rgb(140, 104, 48),
        Category::Filter => Color32::from_rgb(132, 74, 58),
        Category::Output => Color32::from_rgb(84, 86, 92),
    }
}

fn has_output(op: &str) -> bool {
    op != "viewer"
}

impl NodeEditor {
    pub fn fit(&mut self) {
        self.fit_pending = true;
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

    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, error_node: Option<&str>) {
        let (rect, bg) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(30));

        if self.fit_pending {
            self.fit_to(doc);
        }
        self.navigate(ui, &bg);
        self.draw_grid(&painter);

        if bg.clicked() {
            doc.selected = None;
        }
        if bg.secondary_clicked() {
            if let Some(p) = bg.interact_pointer_pos() {
                self.menu_at = self.to_graph(p);
            }
        }
        bg.context_menu(|ui| self.add_menu(ui, doc));

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

        // Wires first, beneath the nodes.
        let wire_stroke = Stroke::new(2.0 * self.zoom.sqrt(), Color32::from_gray(150));
        for n in &doc.graph.nodes {
            let Some(to) = rect_of(&n.id) else { continue };
            for (i, src) in n.inputs.iter().enumerate() {
                if let Some(from) = rect_of(src) {
                    let a = Self::out_port(from);
                    let b = Self::in_port(to, i, n.inputs.len());
                    painter.add(bezier(a, b, wire_stroke));
                }
            }
        }

        // Nodes.
        for (id, op, r, _) in &geo {
            let resp = ui.interact(*r, Id::unique(("node", id)), Sense::click_and_drag());
            if resp.clicked() || resp.drag_started() {
                doc.selected = Some(id.clone());
            }
            if resp.dragged() {
                let d = resp.drag_delta() / self.zoom;
                if let Some(n) = doc.node_mut(id) {
                    let p = n.pos.get_or_insert([0.0, 0.0]);
                    p[0] += d.x;
                    p[1] += d.y;
                }
                doc.touched();
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
                let resp = ui.interact(Rect::from_center_size(p, Vec2::splat(PORT_R * 3.0)), Id::unique(("in", id, i)), Sense::drag());
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
                port_dot(&painter, p, self.zoom, hot);
                if *n_in > 1 && self.zoom > 0.6 {
                    let label = ops::info(op).and_then(|o| o.inputs.get(i)).copied().unwrap_or("");
                    painter.text(p + Vec2::new(PORT_R * 1.6, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(10.0 * self.zoom), Color32::from_gray(210));
                }
            }
            if has_output(op) {
                let p = Self::out_port(*r);
                let resp = ui.interact(Rect::from_center_size(p, Vec2::splat(PORT_R * 3.0)), Id::unique(("out", id)), Sense::drag());
                if resp.drag_started() {
                    self.wire = Some(Wire::FromOutput(id.clone()));
                }
                port_dot(&painter, p, self.zoom, resp.hovered() || resp.dragged());
            }
        }

        // An in-progress wire follows the pointer and connects on release.
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
                let near = |p: Pos2| p.distance(ptr) < PORT_R * 2.5;
                match wire {
                    Wire::FromOutput(src) => {
                        let hit = geo.iter().find_map(|(id, _, r, n)| {
                            (0..*n).find(|&i| near(Self::in_port(*r, i, *n))).map(|i| (id.clone(), i))
                        });
                        if let Some((dst, i)) = hit {
                            doc.connect(&src, &dst, i);
                        }
                    }
                    Wire::FromInput(dst, i) => {
                        let hit = geo.iter().find(|(_, op, r, _)| has_output(op) && near(Self::out_port(*r)));
                        if let Some((src, ..)) = hit {
                            doc.connect(src, &dst, i);
                        }
                    }
                }
            }
        }
        if self.wire.is_some() {
            ui.ctx().request_repaint();
        }
    }

    fn navigate(&mut self, ui: &egui::Ui, bg: &egui::Response) {
        if bg.dragged() {
            self.pan += bg.drag_delta();
        }
        if !ui.rect_contains_pointer(self.rect) {
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
        let groups = [
            (Category::Source, "Source"),
            (Category::Color, "Color"),
            (Category::Merge, "Merge"),
            (Category::Transform, "Transform"),
            (Category::Filter, "Filter"),
            (Category::Output, "Output"),
        ];
        for (cat, name) in groups {
            ui.menu_button(name, |ui| {
                for op in OPS.iter().filter(|o| o.category == cat) {
                    if ui.button(op.label).on_hover_text(op.help).clicked() {
                        let at = [self.menu_at.x, self.menu_at.y];
                        let upstream = doc.selected.clone();
                        let id = doc.add_node(op.name, at);
                        // Adding with a node selected wires it in as the first input.
                        if let Some(up) = upstream {
                            if !op.inputs.is_empty() && doc.node(&up).is_some_and(|n| has_output(&n.op)) {
                                doc.connect(&up, &id, 0);
                            }
                        }
                        doc.selected = Some(id);
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

fn port_dot(painter: &egui::Painter, p: Pos2, zoom: f32, hot: bool) {
    let r = PORT_R * zoom.max(0.6);
    painter.circle_filled(p, r, if hot { Color32::from_rgb(250, 190, 70) } else { Color32::from_gray(210) });
    painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_black_alpha(160)));
}
