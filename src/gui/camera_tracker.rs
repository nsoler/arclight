//! The Camera Tracker in the GUI: its panel (Track & Solve on a worker
//! thread with progress and Stop, the result, Export) and the viewer's
//! point picker (the solved points drawn through the frame's camera;
//! click, ⇧-click or drag a box to select, then place a Card or a Locator
//! on them, or set the origin, ground and scale).

use crate::doc::{Doc, NODE_H, NODE_W};
use crate::overlay::Xf;
use arclight::camtrack::{self, Solution};
use arclight::geom::Vec3;
use arclight::image::Size;
use arclight::ops;
use arclight::render::Progress;
use eframe::egui::{self, Color32, Pos2, Rect, Response, Stroke, Vec2};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

/// A Track & Solve run, or how the last one ended.
pub struct CamJob {
    progress: Arc<Progress>,
    handle: Option<JoinHandle<anyhow::Result<Solution>>>,
    /// (succeeded, message).
    result: Option<(bool, String)>,
}

/// Start tracking and solving Camera Tracker `id` over the project's range.
fn start(doc: &Doc, id: &str) -> CamJob {
    let progress = Arc::new(Progress::default());
    let fail = |e: anyhow::Error| CamJob { progress: Default::default(), handle: None, result: Some((false, format!("{e:#}"))) };
    let Some(node) = doc.node(id) else { return fail(anyhow::anyhow!("no node {id}")) };
    let p = ops::params_with_defaults("camera_tracker", &node.params);
    let settings = camtrack::Settings::from_params(&p, doc.graph.project.first_frame, doc.graph.project.last_frame);
    let mut frames = match arclight::track::input_frames(&doc.graph, id) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    let pg = progress.clone();
    let handle = std::thread::spawn(move || camtrack::run(&mut frames, &settings, &pg));
    CamJob { progress, handle: Some(handle), result: None }
}

/// A free spot below `id` in the node editor.
fn spot_below(doc: &Doc, id: &str) -> [f32; 2] {
    let at = doc.node(id).and_then(|n| n.pos).unwrap_or_default();
    let mut pos = [at[0], at[1] + NODE_H + 40.0];
    while doc.graph.nodes.iter().filter_map(|n| n.pos).any(|q| (q[0] - pos[0]).abs() < NODE_W + 20.0 && (q[1] - pos[1]).abs() < NODE_H + 20.0) {
        pos[0] += NODE_W + 30.0;
    }
    pos
}

/// The Camera Tracker panel's extras. `jobs` holds runs by node.
pub fn panel(ui: &mut egui::Ui, doc: &mut Doc, id: &str, jobs: &mut std::collections::HashMap<String, CamJob>) {
    ui.add_space(4.0);
    // A finished run: keep its solve on the node.
    if let Some(job) = jobs.get_mut(id).filter(|j| j.handle.as_ref().is_some_and(|h| h.is_finished())) {
        job.result = Some(match job.handle.take().unwrap().join() {
            Ok(Ok(sol)) => {
                let first = sol.frames.first().copied().unwrap_or(0);
                let msg = format!(
                    "Solved {} frame(s) from {first}: {} points, error {:.2} px, focal {:.1} mm.",
                    sol.frames.len(),
                    sol.points.len(),
                    sol.rms,
                    sol.focal
                );
                doc.set_param(id, "solution", sol.to_value());
                (true, msg)
            }
            Ok(Err(e)) => (false, format!("{e:#}")),
            Err(_) => (false, "the solve panicked".into()),
        });
    }
    if let Some(job) = jobs.get(id).filter(|j| j.handle.is_some()) {
        let p = &job.progress;
        let (done, total) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed).max(1));
        let stage = if done * 2 < total { "Tracking" } else { "Solving" };
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(p.fraction()).text(stage).desired_width(ui.available_width() - 70.0));
            if ui.button("Stop").clicked() {
                p.cancel.store(true, Ordering::Relaxed);
            }
        });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        return;
    }
    let connected = doc.node(id).is_some_and(|n| n.inputs.first().is_some_and(|i| !i.is_empty()));
    let (first, last) = (doc.graph.project.first_frame, doc.graph.project.last_frame);
    ui.horizontal(|ui| {
        let b = ui.add_enabled(connected, egui::Button::new("Track & Solve ▶"));
        let b = b.on_hover_text(format!("Find and follow features over frames {first}–{last}, then solve the camera")).on_disabled_hover_text("Connect an input to track");
        if b.clicked() {
            jobs.insert(id.to_string(), start(doc, id));
        }
    });
    let sol = doc.node(id).and_then(|n| Solution::from_params(&n.params));
    if let Some(sol) = &sol {
        let worst = sol.frame_rms.iter().copied().fold(0.0, f64::max);
        ui.label(format!("{} frames, {} points · error {:.2} px (worst frame {:.2}) · {:.1} mm", sol.frames.len(), sol.points.len(), sol.rms, worst, sol.focal));
        ui.weak("Select points in the viewer (click, ⇧-click, drag a box) to place a Card or Locator, or to set the origin, ground and scale.");
        ui.horizontal(|ui| {
            if ui.button("Export").on_hover_text("Add a keyed Camera, the point cloud, a Scene and a Scanline Render over the plate").clicked() {
                let msg = export(doc, id, sol);
                jobs.insert(id.to_string(), CamJob { progress: Default::default(), handle: None, result: Some((true, msg)) });
            }
            if ui.button("Clear solve").clicked() {
                doc.remove_param(id, "solution");
            }
        });
    }
    if let Some((ok, msg)) = jobs.get(id).and_then(|j| j.result.as_ref()) {
        let color = if *ok { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(235, 100, 90) };
        ui.colored_label(color, msg);
    }
}

/// Add a keyed Camera, a Point Cloud, a Scene of both and a Scanline Render
/// over the tracker's plate (as `Session::export_camera`). A message.
fn export(doc: &mut Doc, id: &str, sol: &Solution) -> String {
    let base = spot_below(doc, id);
    let at = |dx: f32, dy: f32| [base[0] + dx, base[1] + dy];
    let camera = doc.add_node("camera", at(0.0, 0.0));
    for (k, v) in camtrack::camera_params(sol) {
        doc.set_param(&camera, &k, v);
    }
    let points = doc.add_node("point_cloud", at(NODE_W + 30.0, 0.0));
    doc.set_param(&points, "points", json!(sol.points.iter().flatten().collect::<Vec<_>>()));
    let scene = doc.add_node("scene", at(0.0, NODE_H + 40.0));
    doc.connect(&camera, &scene, 0);
    doc.connect(&points, &scene, 1);
    let render = doc.add_node("scanline_render", at(0.0, 2.0 * (NODE_H + 40.0)));
    doc.connect(&scene, &render, 0);
    doc.connect(id, &render, 2);
    doc.selected = Some(render.clone());
    format!("Added {camera}, {points}, {scene} and {render}.")
}

/// Add a Card lying on points `ids` of `sol` (Camera Tracker `id`'s),
/// facing the camera of `frame`. Its name.
fn place_card(doc: &mut Doc, id: &str, sol: &Solution, ids: &[usize], frame: i64) -> anyhow::Result<String> {
    let params = camtrack::card_on_points(sol, ids, frame)?;
    let card = doc.add_node("card", spot_below(doc, id));
    for (k, v) in params {
        doc.set_param(&card, &k, v);
    }
    doc.selected = Some(card.clone());
    Ok(card)
}

/// Add a Locator at the middle of points `ids`. Its name.
fn place_locator(doc: &mut Doc, id: &str, sol: &Solution, ids: &[usize]) -> Option<String> {
    let c = sol.centroid(ids)?;
    let loc = doc.add_node("locator", spot_below(doc, id));
    doc.set_param(&loc, "translate", json!([c.x, c.y, c.z]));
    Some(loc)
}

/// The viewer's point picker for a selected, solved Camera Tracker.
#[derive(Default)]
pub struct PointPicker {
    node: Option<String>,
    selected: Vec<usize>,
    /// A box being dragged: screen start, now, and the selection it adds to.
    marquee: Option<(Pos2, Pos2, Vec<usize>)>,
    distance: f64,
    message: Option<(bool, String)>,
}

/// Points of `sol` on screen at `frame`: (index, screen position).
fn on_screen(sol: &Solution, xf: &Xf, frame: i64) -> Vec<(usize, Pos2)> {
    let Some(cam) = sol.camera(frame) else { return Vec::new() };
    let format = Size { width: sol.format[0], height: sol.format[1] };
    sol.points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let (q, _) = cam.project(&Vec3::from(*p), format)?;
            Some((i, xf.screen(Vec2::new(q[0] as f32, q[1] as f32))))
        })
        .collect()
}

impl PointPicker {
    /// Whether a box selection is being dragged (the viewer mustn't pan).
    pub fn dragging(&self) -> bool {
        self.marquee.is_some()
    }

    /// The selected node's solve, when it's a solved Camera Tracker.
    fn solve(&mut self, doc: &Doc) -> Option<(String, Solution)> {
        let id = doc.selected.clone()?;
        let node = doc.node(&id).filter(|n| n.op == "camera_tracker")?;
        let sol = Solution::from_params(&node.params)?;
        if self.node.as_deref() != Some(&id) {
            *self = PointPicker { node: Some(id.clone()), distance: 1.0, ..PointPicker::default() };
        }
        self.selected.retain(|&i| i < sol.points.len());
        Some((id, sol))
    }

    /// Handle the pointer and the actions panel. True when it applies (a
    /// solved Camera Tracker is selected), so other overlays stand aside.
    pub fn interact(&mut self, ui: &egui::Ui, resp: &Response, xf: &Xf, doc: &mut Doc, frame: i64) -> bool {
        let Some((id, sol)) = self.solve(doc) else { return false };
        let pts = on_screen(&sol, xf, frame);
        let shift = ui.input(|i| i.modifiers.shift);
        if ui.input(|i| i.pointer.primary_pressed()) {
            if let Some(o) = ui.input(|i| i.pointer.press_origin()).filter(|o| resp.rect.contains(*o)) {
                self.press(&pts, o, shift);
            }
        }
        if let Some(p) = resp.interact_pointer_pos() {
            self.drag_to(&pts, p);
        }
        if ui.input(|i| i.pointer.primary_released()) || resp.drag_stopped() {
            self.marquee = None;
        }
        self.actions(ui, resp.rect, doc, &id, &sol, frame);
        true
    }

    /// The pointer went down at `o` over points `pts` (index, screen):
    /// select the one there (⇧ toggles it), or start a box (⇧ adds to the
    /// selection).
    fn press(&mut self, pts: &[(usize, Pos2)], o: Pos2, shift: bool) {
        let near = pts.iter().map(|(i, p)| (*i, p.distance(o))).filter(|(_, d)| *d < 7.0).min_by(|a, b| a.1.total_cmp(&b.1));
        match near {
            Some((i, _)) if shift => match self.selected.iter().position(|&j| j == i) {
                Some(k) => {
                    self.selected.remove(k);
                }
                None => self.selected.push(i),
            },
            Some((i, _)) => self.selected = vec![i],
            None => {
                if !shift {
                    self.selected.clear();
                }
                self.marquee = Some((o, o, self.selected.clone()));
            }
        }
    }

    /// The pointer moved to `p`: a box being dragged selects what's in it.
    fn drag_to(&mut self, pts: &[(usize, Pos2)], p: Pos2) {
        let Some(m) = &mut self.marquee else { return };
        m.1 = p;
        let r = Rect::from_two_pos(m.0, m.1);
        self.selected = m.2.clone();
        self.selected.extend(pts.iter().filter(|(i, p)| r.contains(*p) && !m.2.contains(i)).map(|(i, _)| *i));
    }

    /// The floating panel of things to do with the selection.
    fn actions(&mut self, ui: &egui::Ui, canvas: Rect, doc: &mut Doc, id: &str, sol: &Solution, frame: i64) {
        let n = self.selected.len();
        if n == 0 && self.message.is_none() {
            return;
        }
        let mut result: Option<anyhow::Result<String>> = None;
        egui::Area::new(egui::Id::unique(("camera_points", id))).fixed_pos(canvas.left_top() + Vec2::new(10.0, 10.0)).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(format!("{n} point(s) selected"));
                ui.horizontal(|ui| {
                    if ui.add_enabled(n > 0, egui::Button::new("Place Card here")).on_hover_text("A Card on the points' plane (3+ points), else facing the camera, sized to them").clicked() {
                        result = Some(place_card(doc, id, sol, &self.selected, frame).map(|card| format!("Added {card}: connect an image to it and add it to a Scene.")));
                    }
                    if ui.add_enabled(n > 0, egui::Button::new("Place locator")).on_hover_text("A point at their middle, to wire into a Scene or link params to").clicked() {
                        result = place_locator(doc, id, sol, &self.selected).map(|loc| Ok(format!("Added {loc}.")));
                    }
                });
                ui.horizontal(|ui| {
                    let mut moved = sol.clone();
                    if ui.add_enabled(n == 1, egui::Button::new("Set origin")).on_hover_text("Move the solve so this point is at 0, 0, 0").clicked() {
                        result = Some(moved.set_origin(self.selected[0]).map(|_| "Origin set.".to_string()));
                    }
                    if ui.add_enabled(n >= 3, egui::Button::new("Ground plane")).on_hover_text("Turn the solve so these points' plane is level at height 0").clicked() {
                        result = Some(moved.set_ground(&self.selected).map(|_| "Ground set.".to_string()));
                    }
                    ui.add_enabled(n == 2, egui::DragValue::new(&mut self.distance).speed(0.01).range(0.001..=1e6).prefix("apart "));
                    if ui.add_enabled(n == 2, egui::Button::new("Set scale")).on_hover_text("Scale the solve so these two points are that far apart").clicked() {
                        result = Some(moved.set_scale(self.selected[0], self.selected[1], self.distance).map(|_| "Scale set.".to_string()));
                    }
                    if moved != *sol {
                        doc.set_param(id, "solution", moved.to_value());
                    }
                });
                if let Some((ok, msg)) = &self.message {
                    let color = if *ok { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(235, 100, 90) };
                    ui.colored_label(color, msg);
                }
            });
        });
        if let Some(r) = result {
            self.message = Some(match r {
                Ok(m) => (true, m),
                Err(e) => (false, format!("{e:#}")),
            });
        }
    }

    /// Draw the points (selected ones larger, in yellow) and a box being
    /// dragged.
    pub fn draw(&self, painter: &egui::Painter, xf: &Xf, doc: &Doc, frame: i64) {
        let Some(id) = doc.selected.as_deref() else { return };
        let Some(sol) = doc.node(id).filter(|n| n.op == "camera_tracker").and_then(|n| Solution::from_params(&n.params)) else { return };
        for (i, p) in on_screen(&sol, xf, frame) {
            if !painter.clip_rect().contains(p) {
                continue;
            }
            if self.node.as_deref() == Some(id) && self.selected.contains(&i) {
                painter.circle(p, 3.5, Color32::from_rgb(255, 210, 60), Stroke::new(1.0, Color32::BLACK));
            } else {
                painter.circle(p, 2.0, Color32::from_rgb(90, 175, 225), Stroke::new(0.5, Color32::from_black_alpha(200)));
            }
        }
        if let Some((a, b, _)) = &self.marquee {
            painter.rect(Rect::from_two_pos(*a, *b), 0.0, Color32::from_white_alpha(18), Stroke::new(1.0, Color32::from_white_alpha(170)), egui::StrokeKind::Middle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::{Graph, NodeSpec};

    /// A Camera Tracker solved by hand: a 3 × 3 grid of points on the
    /// ground in front of one camera.
    fn solved() -> Doc {
        let sol = Solution {
            format: [200, 100], focal: 35.0, film_back: [36.0, 24.0], frames: vec![1], cameras: vec![[0.0, 0.0, 0.0, -15.0, 0.0, 0.0]],
            points: (0..9).map(|i| [(i % 3) as f64 - 1.0, -2.0, -8.0 - (i / 3) as f64]).collect(), rms: 0.1, frame_rms: vec![0.1],
        };
        let node = NodeSpec { id: "ct".into(), op: "camera_tracker".into(), inputs: vec![String::new()], params: json!({ "solution": sol.to_value() }), pos: Some([0.0, 0.0]) };
        let mut d = Doc::new(Graph { nodes: vec![node], ..Default::default() }, None);
        d.selected = Some("ct".into());
        d
    }

    #[test]
    fn points_are_picked_by_click_shift_click_and_box_then_placed_on() {
        let mut d = solved();
        let mut pick = PointPicker::default();
        let (_, sol) = pick.solve(&d).unwrap();
        let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0)), 1.0);
        let pts = on_screen(&sol, &xf, 1);
        assert_eq!(pts.len(), 9, "all in view");
        let at = |i: usize| pts.iter().find(|p| p.0 == i).unwrap().1;
        // A click picks one, ⇧-click adds and removes.
        pick.press(&pts, at(4) + Vec2::new(2.0, 1.0), false);
        assert_eq!(pick.selected, vec![4]);
        pick.press(&pts, at(0), true);
        pick.press(&pts, at(8), true);
        pick.press(&pts, at(4), true);
        assert_eq!(pick.selected, vec![0, 8]);
        // A box from empty space takes what it covers; ⇧ keeps the rest.
        let corner = |i: usize, d: f32| at(i) + Vec2::splat(d);
        pick.press(&pts, Pos2::new(1.0, 1.0), false);
        assert!(pick.selected.is_empty() && pick.dragging());
        pick.drag_to(&pts, corner(5, 2.0));
        let mut boxed = pick.selected.clone();
        boxed.sort();
        // (Farther rows sit higher: the box covers the far two.)
        assert_eq!(boxed, vec![3, 4, 5, 6, 7, 8]);
        pick.marquee = None;
        // (Started clear of the point, so it's a box, not a ⇧-click.)
        pick.press(&pts, corner(0, 9.0), true);
        assert!(pick.dragging());
        pick.drag_to(&pts, corner(0, -9.0));
        assert!(pick.selected.contains(&0) && pick.selected.len() == 7, "{:?}", pick.selected);
        // Placing a card on three ground points lays it on the ground.
        let card = place_card(&mut d, "ct", &sol, &[0, 1, 3], 1).unwrap();
        let p = d.node(&card).unwrap().params.clone();
        assert_eq!((p["translate"][1].as_f64(), d.selected.as_deref()), (Some(-2.0), Some(card.as_str())));
        let normal = arclight::geom::rotation(arclight::scene::numbers::<3>(&p["rotate"], 0.0)) * Vec3::z();
        assert!((normal - Vec3::y()).norm() < 1e-9, "{normal}");
        let loc = place_locator(&mut d, "ct", &sol, &[4]).unwrap();
        assert_eq!(d.node(&loc).unwrap().params["translate"], json!([0.0, -2.0, -9.0]));
    }
}
