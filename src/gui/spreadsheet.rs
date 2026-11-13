//! Geo Info: the viewed node's 3D scene as a table (Blender's spreadsheet),
//! in a strip under the viewer (its Sheet toggle). One domain at a time
//! (tabs, with their counts): point sets' points, mesh points, faces,
//! instances, curves' points, and the Cloud (Camera Trackers' points); a
//! row per element, a column per value (see `arclight::geo_table`). An
//! object picker narrows it to one node's, and Selected only to the nodes
//! selected in the graph or the 3D view and the cloud points picked there.
//! World shows P and N in world space. Only the rows on screen are built,
//! so a million scroll as smoothly as ten.

use crate::doc::Doc;
use crate::worker::{SceneRequest, SceneWorker};
use arclight::geo_table::{Cell, Domain, FieldKind, Filter, Table, View};
use arclight::scene::Scene;
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Vec2};
use std::sync::Arc;

/// A row's height.
const ROW_H: f32 = 18.0;
/// The swatch before a color's components.
const SWATCH_W: f32 = 18.0;

#[derive(Default)]
pub struct Spreadsheet {
    /// Its own thread for the scene (the 3D view's kind, asked for no
    /// render), so it works in 2D as in 3D.
    worker: Option<SceneWorker>,
    asked: Option<(u64, String, i64)>,
    /// The node shown, its scene (None: it has none) and why it failed.
    node: Option<String>,
    scene: Option<Arc<Scene>>,
    error: Option<String>,
    /// The tab chosen (None: the first with rows).
    pub domain: Option<Domain>,
    /// One object's rows only (its node), else all.
    pub object: Option<String>,
    /// Only the selected nodes' rows (and the picked cloud points).
    pub selected_only: bool,
    /// P and N in world space rather than each object's own.
    pub world: bool,
    /// Cells built last frame (only the visible rows' are).
    cells_drawn: usize,
}

/// One column: a field's component, how wide, and whether it's a color
/// swatch before the components.
struct Col {
    field: usize,
    comp: usize,
    width: f32,
    swatch: bool,
}

/// `n` with thousands separators.
fn grouped(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The picks that are `scene`'s cloud points (the 3D view may show
/// another node's scene): the rest are dropped.
fn valid_picks(scene: &Scene, picks: &[(String, usize)]) -> Vec<(String, usize)> {
    picks.iter().filter(|(n, i)| *i < scene.points.len() && scene.point_node(*i) == n).cloned().collect()
}

fn text(cell: Cell) -> String {
    match cell {
        Cell::Empty => String::new(),
        Cell::Int(v) => v.to_string(),
        Cell::Float(v) => format!("{v:.3}"),
        Cell::Text(t) => t.to_string(),
    }
}

impl Spreadsheet {
    /// Ask for the viewed node's scene at `frame` when it changed, and take
    /// what has arrived.
    fn sync(&mut self, ctx: &egui::Context, doc: &Doc, frame: i64) {
        let worker = self.worker.get_or_insert_with(|| SceneWorker::spawn(ctx.clone()));
        while let Some(r) = worker.poll() {
            if Some(&r.node) == self.node.as_ref() {
                (self.scene, self.error) = match r.scene {
                    Ok(s) => (s, None),
                    Err(e) => (None, Some(e)),
                };
            }
        }
        let node = doc.viewed();
        if node != self.node {
            (self.node, self.scene, self.error) = (node.clone(), None, None);
        }
        let Some(node) = node else {
            self.asked = None;
            return;
        };
        let ask = (doc.revision, node.clone(), frame);
        if self.asked.as_ref() != Some(&ask) {
            worker.request(SceneRequest {
                graph: Arc::new(doc.graph.clone()),
                node,
                frame,
                render: None,
                solid: false,
                preview: Default::default(),
                blur: false,
                splats: false,
                through: None,
                rect: None,
                ghosts: Vec::new(),
            });
            self.asked = Some(ask);
        }
    }

    /// The rows' filter: the picked object, the selection (with the nodes
    /// of the cloud points picked in the 3D view: picking a point doesn't
    /// select its node).
    fn filter(&self, doc: &Doc, picks: &[(String, usize)]) -> Filter {
        let nodes = self.selected_only.then(|| {
            let mut nodes = doc.selection();
            for (n, _) in picks {
                if !nodes.contains(n) {
                    nodes.push(n.clone());
                }
            }
            nodes
        });
        Filter { object: self.object.clone(), nodes }
    }

    /// How the rows show: World, and under Selected only the picked cloud
    /// points.
    fn view(&self, picks: &[(String, usize)]) -> View {
        View { world: self.world, picked: self.selected_only.then(|| picks.iter().map(|(_, i)| *i).collect()) }
    }

    /// `picks` (the 3D view's picked cloud points: node, index in the
    /// scene's points) narrow the Cloud under Selected only.
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &Doc, frame: i64, picks: &[(String, usize)]) {
        self.sync(ui.ctx(), doc, frame);
        match self.scene.clone() {
            Some(scene) => {
                let picks = valid_picks(&scene, picks);
                self.draw(ui, doc, &scene, &picks)
            }
            None => {
                let why = match (&self.error, &self.node) {
                    (Some(e), _) => e.clone(),
                    (None, Some(n)) => format!("{n} has no 3D scene."),
                    (None, None) => "Nothing viewed.".into(),
                };
                ui.weak(why);
            }
        }
    }

    /// The header (tabs, object, Selected only, World) and the table of
    /// `scene`.
    fn draw(&mut self, ui: &mut egui::Ui, doc: &Doc, scene: &Scene, picks: &[(String, usize)]) {
        let (filter, view) = (self.filter(doc, picks), self.view(picks));
        let counts = arclight::geo_table::sheet_counts(scene, &Filter { object: None, ..filter.clone() }, &view);
        // The chosen tab, else the first with rows.
        let domain = self.domain.unwrap_or_else(|| counts.iter().find(|(_, n)| *n > 0).map_or(Domain::Points, |(d, _)| *d));
        ui.horizontal_wrapped(|ui| {
            for (d, n) in &counts {
                let label = egui::RichText::new(format!("{} {}", d.label(), grouped(*n)));
                let label = if *n == 0 { label.weak() } else { label };
                if ui.selectable_label(*d == domain, label).clicked() {
                    self.domain = Some(*d);
                }
            }
            ui.separator();
            let all = Table::with_view(scene, domain, &Filter { object: None, ..filter.clone() }, &view);
            let objects = all.objects();
            // A picked object that's gone: all of them again.
            if self.object.as_ref().is_some_and(|o| !objects.iter().any(|(n, ..)| n == o)) {
                self.object = None;
            }
            let shown = self.object.clone().unwrap_or_else(|| "All objects".into());
            egui::ComboBox::from_id_salt("sheet_object").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.object, None, "All objects");
                for (node, kind, n) in &objects {
                    ui.selectable_value(&mut self.object, Some(node.to_string()), format!("{node} ({kind}) {}", grouped(*n)));
                }
            });
            ui.checkbox(&mut self.selected_only, "Selected only").on_hover_text("Only the rows of the nodes selected in the node graph or the 3D view (in Cloud, the points picked there)");
            ui.checkbox(&mut self.world, "World").on_hover_text("P and N in world space (each object placed), rather than its own");
        });
        let table = Table::with_view(scene, domain, &self.filter(doc, picks), &self.view(picks));
        self.table(ui, &table);
    }

    /// The columns of `table`: widths from the header and the kind.
    fn columns(ui: &egui::Ui, table: &Table) -> Vec<Col> {
        let font = FontId::monospace(11.0);
        let mut cols = Vec::new();
        for (k, f) in table.fields().iter().enumerate() {
            if f.color {
                cols.push(Col { field: k, comp: 0, width: SWATCH_W, swatch: true });
            }
            for c in 0..f.width() {
                let header = ui.painter().layout_no_wrap(f.header(c), font.clone(), Color32::WHITE).size().x;
                let values = match f.kind {
                    FieldKind::Int => 56.0,
                    FieldKind::Float => 72.0,
                    FieldKind::Text => 120.0,
                };
                cols.push(Col { field: k, comp: c, width: header.max(values) + 12.0, swatch: false });
            }
        }
        cols
    }

    fn table(&mut self, ui: &mut egui::Ui, table: &Table) {
        self.cells_drawn = 0;
        if table.is_empty() {
            ui.weak("No rows.");
            return;
        }
        let cols = Self::columns(ui, table);
        let width: f32 = cols.iter().map(|c| c.width).sum();
        let font = FontId::monospace(11.0);
        let (text_color, weak) = (ui.visuals().text_color(), ui.visuals().weak_text_color());
        let stripe = ui.visuals().faint_bg_color;
        // Sideways, the header and the rows scroll together; down, only
        // the rows (the header stays).
        egui::ScrollArea::horizontal().id_salt("sheet_h").auto_shrink([false, false]).show(ui, |ui| {
            let (header, _) = ui.allocate_exact_size(Vec2::new(width, ROW_H + 2.0), Sense::hover());
            let mut x = header.left();
            for c in &cols {
                if !c.swatch {
                    // Over its values: text from the left, numbers to the right.
                    let f = &table.fields()[c.field];
                    let (align, at) = match f.kind {
                        FieldKind::Text => (Align2::LEFT_CENTER, x + 6.0),
                        _ => (Align2::RIGHT_CENTER, x + c.width - 6.0),
                    };
                    ui.painter().text(egui::pos2(at, header.center().y), align, f.header(c.comp), font.clone(), weak);
                }
                x += c.width;
            }
            ui.painter().hline(header.x_range(), header.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);
            egui::ScrollArea::vertical().id_salt("sheet_v").auto_shrink([false, false]).show_rows(ui, ROW_H, table.len(), |ui, rows| {
                for row in rows {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
                    if row % 2 == 1 {
                        ui.painter().rect_filled(r, 0.0, stripe);
                    }
                    let mut x = r.left();
                    for c in &cols {
                        let cell = Rect::from_min_size(egui::pos2(x, r.top()), Vec2::new(c.width, ROW_H));
                        x += c.width;
                        self.cells_drawn += 1;
                        if !ui.is_rect_visible(cell) {
                            continue;
                        }
                        if c.swatch {
                            let rgba: Vec<f32> = (0..4).map(|k| match table.cell(row, c.field, k) {
                                Cell::Float(v) => v as f32,
                                _ => 0.0,
                            }).collect();
                            let color = egui::Rgba::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                            ui.painter().rect_filled(cell.shrink(3.0), 2.0, color);
                            continue;
                        }
                        let v = table.cell(row, c.field, c.comp);
                        let (align, at) = match v {
                            Cell::Text(_) => (Align2::LEFT_CENTER, egui::pos2(cell.left() + 6.0, cell.center().y)),
                            _ => (Align2::RIGHT_CENTER, egui::pos2(cell.right() - 6.0, cell.center().y)),
                        };
                        ui.painter().text(at, align, text(v), font.clone(), text_color);
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::geo::PointSet;
    use arclight::geom::Vec3;
    use arclight::graph::Graph;

    fn points(node: &str, n: usize) -> PointSet {
        PointSet { positions: (0..n).map(|i| Vec3::new(i as f64, 0.0, 0.0)).collect(), node: node.into(), ..PointSet::default() }
    }

    /// The sheet drawn for `scene` in a 600 × 400 area.
    fn draw(sheet: &mut Spreadsheet, doc: &Doc, scene: &Scene) {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(600.0, 400.0))), ..Default::default() };
            ctx.run_ui(raw, |ui| sheet.draw(ui, doc, scene, &[])).drop_without_applying_deltas();
        }
    }

    /// A million rows: only the visible ones are drawn.
    #[test]
    fn only_the_visible_rows_are_drawn() {
        let doc = Doc::new(Graph::default(), None);
        let scene = Scene { point_sets: vec![points("big", 1_000_000)], ..Scene::default() };
        let mut sheet = Spreadsheet::default();
        let t0 = std::time::Instant::now();
        draw(&mut sheet, &doc, &scene);
        // Four columns (index, P x y z) of ~20 rows.
        assert!(sheet.cells_drawn > 20 && sheet.cells_drawn < 200, "{}", sheet.cells_drawn);
        assert!(t0.elapsed().as_millis() < 1000, "{:?}", t0.elapsed());
    }

    /// The first tab with rows is shown; Selected only keeps the selected
    /// nodes' rows; a picked object that's gone goes back to all.
    #[test]
    fn the_tab_the_object_and_the_selection_choose_the_rows() {
        let mut doc = Doc::new(Graph::default(), None);
        let a = doc.add_node("constant", [0.0, 0.0]);
        let scene = Scene { point_sets: vec![points(&a, 3), points("other", 5)], ..Scene::default() };
        let mut sheet = Spreadsheet::default();
        draw(&mut sheet, &doc, &scene);
        assert!(sheet.cells_drawn >= 8 * 5, "points: 8 rows of index, object, P {}", sheet.cells_drawn);
        sheet.selected_only = true;
        draw(&mut sheet, &doc, &scene);
        assert_eq!(sheet.cells_drawn, 0, "nothing selected");
        doc.select_only(&a);
        assert_eq!(Table::new(&scene, Domain::Points, &sheet.filter(&doc, &[])).len(), 3);
        draw(&mut sheet, &doc, &scene);
        assert_eq!(sheet.cells_drawn, 3 * 4, "a's 3 rows: index and P (one object: no object column)");
        sheet.selected_only = false;
        sheet.object = Some("gone".into());
        draw(&mut sheet, &doc, &scene);
        assert_eq!(sheet.object, None);
    }

    /// Under Selected only, the 3D view's picked cloud points (their nodes
    /// unselected) narrow the Cloud; picks from another scene are dropped;
    /// World is passed on.
    #[test]
    fn picked_cloud_points_narrow_the_cloud_under_selected_only() {
        let doc = Doc::new(Graph::default(), None);
        let v = |x: f64| Vec3::new(x, 0.0, 0.0);
        let scene = Scene { points: vec![v(0.0), v(1.0), v(2.0)], point_nodes: vec!["track".into(); 3], ..Scene::default() };
        let picks = vec![("track".to_string(), 1), ("other".to_string(), 0), ("track".to_string(), 9)];
        let picks = valid_picks(&scene, &picks);
        assert_eq!(picks, [("track".to_string(), 1)], "only this scene's");
        let mut sheet = Spreadsheet { domain: Some(Domain::Cloud), ..Spreadsheet::default() };
        let rows = |sheet: &Spreadsheet| Table::with_view(&scene, Domain::Cloud, &sheet.filter(&doc, &picks), &sheet.view(&picks)).len();
        assert_eq!(rows(&sheet), 3, "all, without Selected only");
        sheet.selected_only = true;
        assert_eq!(rows(&sheet), 1, "the picked one, its node not selected");
        assert_eq!(Table::with_view(&scene, Domain::Cloud, &sheet.filter(&doc, &[]), &sheet.view(&[])).len(), 0, "nothing selected or picked");
        sheet.world = true;
        assert!(sheet.view(&picks).world);
        let ctx = egui::Context::default();
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(600.0, 400.0))), ..Default::default() };
        ctx.run_ui(raw, |ui| sheet.draw(ui, &doc, &scene, &picks)).drop_without_applying_deltas();
        assert_eq!(sheet.cells_drawn, 4, "one row: index and P");
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!([grouped(0), grouped(999), grouped(1000), grouped(1_234_567)], ["0", "999", "1,000", "1,234,567"]);
    }
}
