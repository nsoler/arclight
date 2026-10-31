//! The image viewer: shows the rendered frame with pan/zoom, channel isolation,
//! exposure, extra planes (depth) as gray, and a float pixel readout under
//! the cursor. Pixels stored past
//! the frame (overscan) show dimmed, and a data window smaller or larger than
//! the frame is outlined. The frame's corners can be labeled with its size.

use crate::doc::Doc;
use crate::gizmo3d::{self, Handle, Layout, Mode, Space, Tool, Transforming};
use crate::overlay::{Overlays, Xf};
use crate::viewport3d::Shading;
use arclight::color;
use arclight::ops;
use arclight::image::{IRect, Image, Plane, Size};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use rayon::prelude::*;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Rgb,
    Red,
    Green,
    Blue,
    Alpha,
    /// The extra plane named by [`Viewer::plane`], as gray from its lowest
    /// to its highest value (0, empty, stays black).
    Plane,
}

impl Channel {
    const ALL: [(Channel, &'static str); 5] = [
        (Channel::Rgb, "RGB"),
        (Channel::Red, "R"),
        (Channel::Green, "G"),
        (Channel::Blue, "B"),
        (Channel::Alpha, "A"),
    ];
}

/// The 3D button's mark when there's a scene to see, and its glow.
const SCENE_3D: Color32 = Color32::from_rgb(90, 175, 235);
/// How long the 3D button glows, and the hint shows, after a first 3D
/// setup (seconds).
const PULSE_SECONDS: f64 = 4.0;
const HINT_SECONDS: f64 = 7.0;
const HINT_3D: &str = "3D view: Tab switches 2D / 3D (pointer over the viewer)";
/// The frame size labels' text, and their gap from the frame's corners
/// (points).
const FRAME_SIZE_FONT: f32 = 11.0;
const FRAME_SIZE_GAP: f32 = 3.0;
/// The height a label takes past the frame, with the gap (points).
const FRAME_SIZE_ROOM: f32 = FRAME_SIZE_FONT + 2.0 * FRAME_SIZE_GAP;

pub struct Viewer {
    texture: Option<TextureHandle>,
    /// The window's wgpu (eframe's), to draw results still on the GPU when
    /// it is the engine's device (see `gpu::for_window`).
    pub wgpu: Option<eframe::egui_wgpu::RenderState>,
    /// The texture a GPU result was displayed into, and whether the frame
    /// shown now is in it (else in `texture`).
    native: Option<Native>,
    native_shown: bool,
    /// The image currently uploaded, and the display settings it used.
    shown: Option<(Arc<Image>, Channel, f32, String)>,
    /// What the texture covers, in frame pixels: the frame and the data window.
    region: IRect,
    nearest: bool,
    pub channel: Channel,
    /// The layer shown with [`Channel::Plane`]: one plane (`depth`), or
    /// several (`P`: `P.x`, `P.y`, `P.z`).
    pub plane: String,
    /// Exposure in stops.
    pub exposure: f32,
    /// Image pixels per screen point.
    zoom: f32,
    /// Offset of the image center from the canvas center, in points.
    pan: Vec2,
    fit_pending: bool,
    overlays: Overlays,
    /// The toolbar's status text width last frame, to tell whether it fits
    /// beside the controls or needs a row of its own.
    status_w: f32,
    /// The pixel last under the pointer, for the toolbar's color picker:
    /// position, value (linear, premultiplied) and color as displayed. When
    /// the pointer leaves, the same position keeps being read from each new
    /// frame, so it can be read at leisure (and follows playback).
    picked: Option<Picked>,
    /// Showing the viewed node's 3D scene instead of its image (the 2D / 3D
    /// toggle, or Tab over the viewer).
    pub mode3d: bool,
    /// Geo Info: the viewed scene's spreadsheet, in a strip under the view
    /// (see `spreadsheet.rs`).
    pub sheet: bool,
    /// The 3D view (`mode3d`).
    pub viewport3d: crate::viewport3d::Viewport3d,
    /// G / R / S, or a gizmo drag, in progress in the 3D view.
    transforming: Option<Transforming>,
    /// The 3D view's gizmo (toolbar tools) and axes (Global / Local).
    pub tool: Tool,
    pub space: Space,
    /// The gizmo handle being dragged.
    dragged: Option<Handle>,
    /// A message for the 3D view (why G / R / S can't), and when it was shown.
    notice: Option<(String, f64)>,
    /// Why the selection last had no (or a ghost's) gizmo, said once as a
    /// notice when it changes (see [`Viewer::gizmo_why`]).
    gizmo_said: Option<String>,
    /// The viewed node has a 3D scene (the 3D button shows it).
    has_scene: bool,
    /// The 3D button glows until this time (seconds), and a hint about
    /// the 3D view shows from this time: after a first 3D setup.
    pulse_until: f64,
    hint_since: Option<f64>,
    /// Labeling the frame's corners with its size (View > Show frame size).
    pub show_frame_size: bool,
    pub rect: Rect,
}


/// A texture a GPU result is displayed into (see `gpu::display`).
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
struct Native {
    id: egui::TextureId,
    texture: eframe::wgpu::Texture,
    nearest: bool,
}

#[derive(Clone, Copy)]
struct Picked {
    x: i32,
    y: i32,
    px: [f32; 4],
    shown: Color32,
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer {
            texture: None,
            wgpu: None,
            native: None,
            native_shown: false,
            shown: None,
            region: IRect::default(),
            nearest: false,
            channel: Channel::Rgb,
            plane: String::new(),
            exposure: 0.0,
            zoom: 1.0,
            pan: Vec2::ZERO,
            status_w: 0.0,
            picked: None,
            fit_pending: true,
            overlays: Overlays::default(),
            mode3d: false,
            sheet: false,
            viewport3d: Default::default(),
            transforming: None,
            tool: Tool::default(),
            space: Space::default(),
            dragged: None,
            notice: None,
            gizmo_said: None,
            has_scene: false,
            pulse_until: 0.0,
            hint_since: None,
            show_frame_size: true,
            rect: Rect::NOTHING,
        }
    }
}

/// What the viewer should display this frame.
pub struct Display<'a> {
    pub image: Option<&'a Arc<Image>>,
    /// The same image still on the GPU, when it was made there: displayed
    /// from there (the readout reads `image`).
    pub gpu: Option<&'a Arc<arclight::gpu::Resident>>,
    pub error: Option<&'a str>,
    pub target: Option<&'a str>,
    /// The Viewer node, while another node is shown in its place.
    pub returns_to: Option<&'a str>,
    pub frame: i64,
    pub millis: f32,
    /// A node and its input's image, for that node's overlay.
    pub input: Option<&'a (String, Arc<Image>)>,
}

impl Viewer {
    pub fn fit(&mut self) {
        self.fit_pending = true;
    }

    /// Select every vertex of the selected shape (see [`Overlays::select_all_points`]).
    pub fn select_all_points(&mut self, doc: &Doc) -> bool {
        self.overlays.select_all_points(doc)
    }

    /// Delete the selected overlay vertices, if any (see [`Overlays::delete_points`]).
    pub fn delete_points(&mut self, doc: &mut Doc) -> bool {
        self.overlays.delete_points(doc)
    }

    /// Switch between the image (2D) and the scene (3D).
    pub fn toggle_3d(&mut self) {
        self.mode3d = !self.mode3d;
    }

    /// A first 3D setup was added (`now`: the time, seconds): show the 3D
    /// view framed on the scene, make the 3D button glow for a few seconds
    /// and say how to switch.
    pub fn announce_3d(&mut self, now: f64) {
        self.mode3d = true;
        self.viewport3d.frame_next_scene();
        self.pulse_until = now + PULSE_SECONDS;
        self.hint_since = Some(now);
    }

    /// The hint after a first 3D setup, while it shows (`now`: seconds).
    fn hint(&self, now: f64) -> Option<&'static str> {
        self.hint_since.filter(|t| now - t < HINT_SECONDS).map(|_| HINT_3D)
    }

    /// G / R / S is running: it takes the keys and the pointer, and its
    /// edits are one undo step.
    pub fn is_transforming(&self) -> bool {
        self.transforming.is_some()
    }

    /// G / R / S over the 3D view (Blender's keys; see `gizmo3d`), each
    /// picking its gizmo's tool as it starts, and Alt + G / R / S to reset
    /// (the tool as it was). Runs before the viewport, so a gesture takes its
    /// keys first; [`Viewer::transform_overlay`] draws the header after.
    fn transform_keys(&mut self, ui: &mut egui::Ui, resp: &egui::Response, doc: &mut Doc, frame: i64) {
        let view = self.viewport3d.projector();
        if let Some(t) = &mut self.transforming {
            let by_drag = t.by_drag;
            let input = ui.input_mut(|i| gizmo3d::Input::take(i, by_drag));
            let outcome = match &view {
                Some(v) => t.step(&input, v, doc),
                None => gizmo3d::Outcome::Cancelled,
            };
            if outcome != gizmo3d::Outcome::Going {
                self.transforming = None;
                self.dragged = None;
            }
            ui.ctx().request_repaint();
            return;
        }
        if resp.hovered() && !ui.ctx().text_edit_focused() {
            use egui::{Key, Modifiers};
            let pressed = ui.input_mut(|i| {
                [(Key::G, Mode::Move), (Key::R, Mode::Rotate), (Key::S, Mode::Scale)].into_iter().find_map(|(k, m)| {
                    if i.consume_key(Modifiers::ALT, k) {
                        Some((m, true))
                    } else {
                        crate::consume_plain(i, k).then_some((m, false))
                    }
                })
            });
            if let Some((mode, reset)) = pressed {
                let now = ui.input(|i| i.time);
                let selection = doc.selection();
                let result = if selection.is_empty() {
                    Err(format!("Select an object to {} it", mode.name().to_lowercase()))
                } else if reset {
                    // Each that can be; the first refusal if none could.
                    let results: Vec<Result<(), String>> = selection.iter().map(|n| gizmo3d::reset(doc, n, mode, frame)).collect();
                    if results.iter().any(Result::is_ok) { Ok(()) } else { results.into_iter().next().unwrap_or(Ok(())) }
                } else {
                    match (&view, resp.hover_pos()) {
                        (Some(_), Some(at)) => {
                            let objects = self.objects(doc, &selection, frame);
                            // Its gizmo stays up after, for drag tweaks.
                            Transforming::start_many(doc, &objects, mode, at, frame).map(|t| {
                                self.transforming = Some(t);
                                self.tool = Tool::of(mode);
                            })
                        }
                        _ => Err("The 3D view isn't ready yet".into()),
                    }
                };
                if let Err(why) = result {
                    self.notice = Some((why, now));
                }
            }
        }
    }

    /// Each of `nodes` with its place in the viewed scene, or its own
    /// params' if it isn't in it.
    fn objects(&self, doc: &Doc, nodes: &[String], frame: i64) -> Vec<(String, arclight::geom::Mat4)> {
        nodes.iter().map(|n| (n.clone(), self.viewport3d.object_world(n).unwrap_or_else(|| gizmo3d::trs_at(doc, n, frame).matrix()))).collect()
    }

    /// The gizmo for the current tool on the selected objects in the view
    /// (at their median point, with the primary's axes), from the view as
    /// last drawn; the primary; and per mode why no handle can move any of
    /// them, if none can.
    fn gizmo(&self, doc: &Doc) -> Option<(Layout, String, [Option<String>; 3])> {
        let view = self.viewport3d.projector()?;
        let placed: Vec<(String, arclight::geom::Mat4)> = doc.selection().into_iter().filter_map(|n| Some((n.clone(), self.viewport3d.object_world(&n)?))).collect();
        let (node, world) = placed.last()?.clone();
        let origin = gizmo3d::median(placed.iter().map(|(_, w)| w))?;
        let layout = Layout::new(&view, self.tool, origin, gizmo3d::axes_of(&world, self.space))?;
        let refused = [Mode::Move, Mode::Rotate, Mode::Scale].map(|m| {
            let why: Vec<Option<String>> = placed.iter().map(|(n, _)| gizmo3d::refusal(doc, n, m)).collect();
            if why.iter().any(Option::is_none) { None } else { why.into_iter().last().flatten() }
        });
        Some((layout, node, refused))
    }

    /// Why the selected card, light, camera… has no gizmo in the view, or
    /// only a ghost's: it couldn't be made (its error), it isn't in the
    /// viewed scene (shown dashed so it can still be moved), it's behind
    /// the view, or it's what the view looks through. None when it has one,
    /// with the Select tool (no gizmos), or with nothing 3D selected.
    fn gizmo_why(&self, doc: &Doc) -> Option<String> {
        if self.tool == Tool::Select || self.viewport3d.projector().is_none() {
            return None;
        }
        let selection = doc.selection();
        let (ghosts, errors) = self.viewport3d.ghosts();
        if let Some((n, e)) = errors.iter().find(|(n, _)| selection.contains(n)) {
            return Some(format!("{n} can't be shown in 3D: {e}"));
        }
        let placed: Vec<&String> = selection.iter().filter(|n| self.viewport3d.object_world(n).is_some()).collect();
        let primary = placed.last()?;
        if self.gizmo(doc).is_none() {
            return Some(match self.viewport3d.looking_through() {
                Some(t) if &&t == primary => format!("You're looking through {primary}: press 0 to come out and move it"),
                _ => format!("{primary} is behind the view: press F to frame it"),
            });
        }
        placed.iter().find(|n| ghosts.contains(n)).map(|n| format!("{n} isn't in the viewed scene: it's shown dashed so you can move it"))
    }

    /// Hovering a gizmo handle, and pressing it to start a drag (releasing
    /// confirms; Esc or right-click cancels). The handle under the pointer,
    /// so the viewport leaves that click alone.
    fn gizmo_input(&mut self, ui: &egui::Ui, resp: &egui::Response, doc: &Doc, frame: i64) -> Option<Handle> {
        if self.transforming.is_some() {
            return self.dragged;
        }
        let (layout, _, refused) = self.gizmo(doc)?;
        let at = resp.hover_pos()?;
        let h = layout.hit(at)?;
        let mode = h.gesture(self.space).0;
        if ui.input(|i| i.pointer.primary_pressed()) {
            let placed: Vec<String> = doc.selection().into_iter().filter(|n| self.viewport3d.object_world(n).is_some()).collect();
            let objects = self.objects(doc, &placed, frame);
            match Transforming::drag_many(doc, &objects, h, self.space, at, frame) {
                Ok(mut t) => {
                    if let Handle::Ring(k) = h {
                        t.gesture = t.gesture.clone().along(layout.ring_tangent(k, at));
                    }
                    self.transforming = Some(t);
                    self.dragged = Some(h);
                }
                Err(why) => self.notice = Some((why, ui.input(|i| i.time))),
            }
        } else if let Some(why) = &refused[mode as usize] {
            self.notice = Some((why.clone(), ui.input(|i| i.time)));
        }
        Some(h)
    }

    /// Draw the gizmo over the view (gray when its params are linked).
    fn gizmo_draw(&self, ui: &egui::Ui, rect: Rect, doc: &Doc, hot: Option<Handle>) {
        if let Some((layout, _, refused)) = self.gizmo(doc) {
            let enabled = refused.iter().any(Option::is_none);
            layout.draw(&ui.painter_at(rect), hot, enabled);
        }
    }

    /// The running gesture's header (top left) and any notice (bottom
    /// left), over the 3D view.
    fn transform_overlay(&mut self, ui: &egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        if let Some(t) = &self.transforming {
            let text = format!("{}   ·   X / Y / Z axis, ⇧ to exclude, type a value, Ctrl snaps · Enter or click: done · Esc or right-click: cancel", t.gesture.describe(&t.now));
            let pos = rect.left_top() + Vec2::new(8.0, 8.0);
            let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::from_gray(235));
            painter.rect_filled(Rect::from_min_size(pos, galley.size()).expand(4.0), 3.0, Color32::from_black_alpha(170));
            painter.galley(pos, galley, Color32::from_gray(235));
        }
        // A notice fades after 4 s.
        if let Some((text, at)) = &self.notice {
            let age = ui.input(|i| i.time) - at;
            if age > 4.0 {
                self.notice = None;
            } else {
                let pos = rect.left_bottom() + Vec2::new(8.0, -8.0);
                let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(13.0), Color32::from_rgb(255, 190, 110));
                let r = Align2::LEFT_BOTTOM.anchor_size(pos, galley.size());
                painter.rect_filled(r.expand(4.0), 3.0, Color32::from_black_alpha(170));
                painter.galley(r.min, galley, Color32::from_rgb(255, 190, 110));
                ui.ctx().request_repaint();
            }
        }
    }

    /// `doc` and `frame` drive the selected node's overlay handles; edits
    /// key animated params at `frame`.
    pub fn ui(&mut self, ui: &mut egui::Ui, d: Display<'_>, doc: &mut Doc, frame: i64) {
        self.has_scene = d.target.is_some_and(|t| scene_of(doc, t).is_some());
        let now = ui.input(|i| i.time);
        if self.pulse_until > now || self.hint(now).is_some() {
            ui.ctx().request_repaint();
        }
        if self.mode3d {
            self.toolbar_3d(ui, &d);
        } else {
            self.toolbar(ui, &d);
        }
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(24));
        if self.mode3d {
            self.ui_3d(ui, rect, &resp, &d, doc, frame);
            self.draw_hint(ui, rect, now);
            return;
        }

        let Some(img) = d.image else {
            match d.error {
                Some(err) => view_message(ui, rect, Align2::CENTER_CENTER, err, ERROR_COLOR, 14.0),
                None => {
                    painter.text(rect.center(), Align2::CENTER_CENTER, "Nothing to view — connect a node to a Viewer, or select one.", FontId::proportional(14.0), Color32::GRAY);
                }
            }
            return;
        };
        self.upload(ui.ctx(), img, d.gpu);
        let size = Vec2::new(img.format.width as f32, img.format.height as f32);

        if self.fit_pending {
            // Room above and below for the frame size labels.
            let room = if self.show_frame_size { 2.0 * FRAME_SIZE_ROOM } else { 0.0 };
            let s = (rect.width() / size.x).min((rect.height() - room).max(1.0) / size.y) * 0.95;
            self.zoom = s.max(1e-3);
            self.pan = Vec2::ZERO;
            self.fit_pending = false;
        }

        self.overlays.set_input(d.input.cloned());
        // Overlay handles take a drag before panning does.
        let xf = |v: &Viewer| Xf::new(Rect::from_center_size(rect.center() + v.pan, size * v.zoom), v.zoom);
        self.overlays.interact(ui, &resp, &xf(self), size, doc, frame);

        // Interaction: drag to pan, scroll / pinch to zoom about the cursor.
        if resp.dragged() && !self.overlays.dragging() {
            self.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.fit();
        }
        if let Some(hover) = resp.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.002).exp();
            if factor != 1.0 {
                let new_zoom = (self.zoom * factor).clamp(0.02, 64.0);
                let c = rect.center() + self.pan;
                self.pan += (hover - c) * (1.0 - new_zoom / self.zoom);
                self.zoom = new_zoom;
            }
        }

        let img_rect = Rect::from_center_size(rect.center() + self.pan, size * self.zoom);
        // Nearest-neighbour when zoomed in, so individual pixels are visible.
        let nearest = self.zoom >= 2.0;
        if nearest != self.nearest {
            self.nearest = nearest;
            self.shown = None;
            self.upload(ui.ctx(), img, d.gpu);
        }
        // Frame pixels → screen.
        let on_screen = |r: IRect| {
            Rect::from_min_max(
                img_rect.min + Vec2::new(r.x0 as f32, r.y0 as f32) * self.zoom,
                img_rect.min + Vec2::new(r.x1 as f32, r.y1 as f32) * self.zoom,
            )
        };
        let shown = match (&self.native, &self.texture) {
            (Some(n), _) if self.native_shown => Some(n.id),
            (_, Some(t)) => Some(t.id()),
            _ => None,
        };
        if let Some(id) = shown {
            painter.image(id, on_screen(self.region), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        painter.rect_stroke(img_rect, 0.0, (1.0, Color32::from_gray(70)), egui::StrokeKind::Outside);
        let window = img.window();
        if window != img.frame() && !window.is_empty() {
            let r = on_screen(window);
            let ring = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.add(egui::Shape::dashed_line(&ring, egui::Stroke::new(1.0, Color32::from_rgb(120, 170, 220)), 6.0, 4.0));
        }
        if self.show_frame_size {
            for (pos, align, text) in frame_size_labels(img_rect, img.format) {
                painter.text(pos, align, text, FontId::proportional(FRAME_SIZE_FONT), Color32::from_gray(120));
            }
        }
        self.overlays.draw(ui, &painter, &xf(self), size, doc, frame);
        // A 3D scene has no pixels: say what's in it instead.
        if let Some(scene) = &img.scene {
            let text = format!("3D scene: {}\nConnect it to a Scanline Render to see it.", scene.describe());
            painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::from_rgb(120, 190, 235));
        }

        // Pixel readout, anywhere pixels are shown (overscan included).
        if let Some(hover) = resp.hover_pos() {
            let p = (hover - img_rect.min) / self.zoom;
            let (x, y) = (p.x.floor() as i32, p.y.floor() as i32);
            if self.region.contains(x, y) {
                let px = img.at(x, y);
                self.picked = Some(Picked { x, y, px, shown: Color32::BLACK });
                let mut text = format!(
                    "x {:<5} y {:<5}   R {:.4}  G {:.4}  B {:.4}  A {:.4}",
                    x, y, px[0], px[1], px[2], px[3]
                );
                for plane in self.shown_planes(img) {
                    text += &format!("   {} {:.4}", plane.name, img.plane_value(plane, x, y));
                }
                let pos = rect.left_bottom() + Vec2::new(8.0, -8.0);
                let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::from_gray(220));
                let r = Align2::LEFT_BOTTOM.anchor_size(pos, galley.size());
                painter.rect_filled(r.expand(4.0), 3.0, Color32::from_black_alpha(160));
                painter.galley(r.min, galley, Color32::from_gray(220));
            }
        }
        // The picker's pixel, read from this frame (also after the pointer
        // left), shown as displayed: exposure applied, over black.
        if let Some(p) = &mut self.picked {
            p.px = img.at(p.x, p.y);
            let gain = 2f32.powf(self.exposure);
            let enc = |v: f32| (color::linear_to_srgb(v * gain).clamp(0.0, 1.0) * 255.0).round() as u8;
            p.shown = Color32::from_rgb(enc(p.px[0]), enc(p.px[1]), enc(p.px[2]));
        }
        if let Some(err) = d.error {
            view_message(ui, rect, Align2::LEFT_TOP, err, ERROR_COLOR, 13.0);
        }
    }

    /// The 3D view of the viewed node's scene (see [`scene_of`]).
    fn ui_3d(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, d: &Display<'_>, doc: &mut Doc, frame: i64) {
        let painter = ui.painter_at(rect);
        let Some(node) = d.target else {
            painter.text(rect.center(), Align2::CENTER_CENTER, "Nothing to view — connect a node to a Viewer, or select one.", FontId::proportional(14.0), Color32::GRAY);
            return;
        };
        match scene_of(doc, node) {
            None => {
                let text = format!(
                    "'{node}' has no 3D scene.\nView a Camera, Card, Scene, Locator, Point Cloud, Camera Tracker or a Scanline Render to see one here,\nor press Tab (or 2D) for its image."
                );
                painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::GRAY);
            }
            Some(_) => {
                self.transform_keys(ui, resp, doc, frame);
                let why = self.gizmo_why(doc);
                if why != self.gizmo_said {
                    if let Some(w) = &why {
                        self.notice = Some((w.clone(), ui.input(|i| i.time)));
                    }
                    self.gizmo_said = why;
                }
                let hot = self.gizmo_input(ui, resp, doc, frame);
                let blocked = self.is_transforming() || hot.is_some();
                self.viewport3d.ui(ui, rect, resp, doc, frame, blocked);
                self.gizmo_draw(ui, rect, doc, hot);
                self.transform_overlay(ui, rect);
            }
        }
    }

    /// The 2D / 3D switch, at the left of either toolbar. 3D is marked
    /// (a dot, in the scene color) when the viewed node has a 3D scene and
    /// dimmed when it hasn't; after a first 3D setup it glows for a while.
    fn mode_switch(&mut self, ui: &mut egui::Ui) {
        let tip = "Show the viewed node's image (2D) or its 3D scene (3D). Tab switches while the pointer is over the viewer.";
        if ui.selectable_label(!self.mode3d, "2D").on_hover_text(tip).clicked() {
            self.mode3d = false;
        }
        let label = if self.has_scene {
            egui::RichText::new("3D ●").color(SCENE_3D)
        } else {
            egui::RichText::new("3D").color(ui.visuals().weak_text_color())
        };
        let tip3 = if self.has_scene { format!("{tip}\n\nThis node has a 3D scene.") } else { format!("{tip}\n\nThis node has no 3D scene.") };
        let r = ui.selectable_label(self.mode3d, label).on_hover_text(tip3);
        if r.clicked() {
            self.mode3d = true;
        }
        ui.toggle_value(&mut self.sheet, "▦ Sheet").on_hover_text("Geo Info: the viewed node's 3D scene as a table (points, faces, instances, curves and their attributes), under the view");
        let now = ui.input(|i| i.time);
        if self.pulse_until > now {
            // A glow breathing about once a second.
            let k = 0.5 + 0.5 * ((self.pulse_until - now) * std::f64::consts::TAU).cos() as f32;
            let glow = SCENE_3D.gamma_multiply(0.35 + 0.65 * k);
            ui.painter().rect_stroke(r.rect.expand(2.0 + 2.0 * k), 4.0, Stroke::new(2.0, glow), egui::StrokeKind::Outside);
        }
        ui.separator();
    }

    /// The hint after a first 3D setup, across the top of the view.
    fn draw_hint(&self, ui: &egui::Ui, rect: Rect, now: f64) {
        let Some(text) = self.hint(now) else { return };
        let t = now - self.hint_since.unwrap_or(now);
        // Fade out over the last second.
        let alpha = (HINT_SECONDS - t).clamp(0.0, 1.0) as f32;
        let painter = ui.painter_at(rect);
        let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(15.0), Color32::WHITE.gamma_multiply(alpha));
        let r = Align2::CENTER_TOP.anchor_size(rect.center_top() + Vec2::new(0.0, 14.0), galley.size()).expand2(Vec2::new(14.0, 8.0));
        painter.rect(r, 6.0, Color32::from_black_alpha((200.0 * alpha) as u8), Stroke::new(1.5, SCENE_3D.gamma_multiply(alpha)), egui::StrokeKind::Inside);
        painter.galley(r.min + Vec2::new(14.0, 8.0), galley, Color32::WHITE);
    }

    fn toolbar_3d(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        // Wrapped: a narrow view takes the controls onto more rows rather
        // than cutting them off (and widening everything under them).
        ui.horizontal_wrapped(|ui| {
            self.mode_switch(ui);
            for (tool, label, tip) in Tool::ALL {
                ui.selectable_value(&mut self.tool, tool, label).on_hover_text(tip);
            }
            ui.separator();
            let tip = "The gizmo's axes: the world's, or the object's own (G / R / S: press X / Y / Z twice for the object's)";
            ui.selectable_value(&mut self.space, Space::Global, "Global").on_hover_text(tip);
            ui.selectable_value(&mut self.space, Space::Local, "Local").on_hover_text(tip);
            ui.separator();
            let shading = self.viewport3d.shading();
            for (s, label, tip) in [(Shading::Solid, "Solid", "Meshes lit from the view, quick to work in (⇧Z)"), (Shading::Lit, "Lit", "Meshes lit by the scene's lights and materials, as a render is (⇧Z)")] {
                if ui.selectable_label(shading == s, label).on_hover_text(tip).clicked() {
                    self.viewport3d.set_shading(s);
                }
            }
            ui.add_enabled(shading == Shading::Lit, egui::Checkbox::new(&mut self.viewport3d.view.shadows, "Shadows")).on_hover_text("Lit: the lights' shadows (once the view stops moving)");
            ui.separator();
            if let Some(t) = d.target {
                ui.weak(format!("Viewing {t}"));
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        let mut fits = true;
        // Wrapped, as the 3D one.
        ui.horizontal_wrapped(|ui| {
            self.mode_switch(ui);
            for (ch, label) in Channel::ALL {
                ui.selectable_value(&mut self.channel, ch, label);
            }
            // The image's extra planes (a 3D render's depth and passes), a
            // button a layer.
            for (layer, planes) in d.image.map_or(Vec::new(), |img| img.layers()) {
                let on = self.channel == Channel::Plane && self.plane == layer;
                let tip = if planes.len() == 1 {
                    format!("Show the {layer} plane as gray, lowest to highest value")
                } else {
                    format!("Show the {layer} layer as color ({} as red, green, blue)", planes.join(", "))
                };
                if ui.selectable_label(on, &layer).on_hover_text(tip).clicked() {
                    self.channel = Channel::Plane;
                    self.plane = layer;
                }
            }
            ui.separator();
            ui.label("Exposure");
            ui.add(egui::DragValue::new(&mut self.exposure).speed(0.05).range(-12.0..=12.0).suffix(" stops"));
            if ui.small_button("0").on_hover_text("Reset exposure").clicked() {
                self.exposure = 0.0;
            }
            ui.separator();
            if ui.button("Fit").on_hover_text("Fit image (F, or double-click)").clicked() {
                self.fit();
            }
            if ui.button("1:1").clicked() {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
            fits = ui.available_width() >= self.status_w + ui.spacing().item_spacing.x * 2.0;
            if fits {
                self.status(ui, d);
            }
        });
        if !fits {
            ui.horizontal(|ui| self.status(ui, d));
        }
    }

    /// What's viewed, its size, frame and timing, right-aligned.
    fn status(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        self.status_w = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.color_picker(ui);
                ui.separator();
                if let Some(img) = d.image {
                    ui.label(format!("{}x{}  ·  frame {}  ·  {:.1} ms", img.format.width, img.format.height, d.frame, d.millis));
                    let w = img.window();
                    if w != img.frame() {
                        let text = if w.is_empty() { "no data".to_string() } else { format!("data {},{} {}×{}", w.x0, w.y0, w.width(), w.height()) };
                        ui.label(egui::RichText::new(text).color(Color32::from_rgb(120, 170, 220)))
                            .on_hover_text("The data window: where this image stores pixels (outlined, dashed). Outside it everything is transparent; past the frame it shows dimmed.");
                    }
                }
                if let Some(t) = d.target {
                    if let Some(v) = d.returns_to {
                        ui.weak(format!("(click {v} to go back)"))
                            .on_hover_text(format!("Double-clicked nodes show in place of {v}'s input until {v} is clicked or double-clicked"));
                    }
                    ui.strong(t);
                    ui.label("Viewing");
                }
                ui.min_rect().width()
            })
            .inner;
    }

    /// The hovered pixel: a swatch as displayed, then its R, G, B and A
    /// (linear, premultiplied, as the comp sees them). Laid out right to
    /// left, so the swatch ends the toolbar. Fixed-width figures keep the
    /// toolbar from shifting as values change.
    fn color_picker(&self, ui: &mut egui::Ui) {
        let p = self.picked;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0, 16.0), Sense::hover());
        match p {
            Some(p) => {
                ui.painter().rect_filled(rect, 2.0, p.shown);
                let [r, g, b] = [p.shown.r(), p.shown.g(), p.shown.b()];
                resp.on_hover_text(format!(
                    "Pixel {}, {}\nAs displayed: {r}, {g}, {b} (8-bit sRGB) · #{r:02X}{g:02X}{b:02X}\nValues: linear, premultiplied",
                    p.x, p.y
                ));
            }
            None => {
                resp.on_hover_text("Hover over the image to read a pixel's color");
            }
        }
        ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(110)), egui::StrokeKind::Outside);
        let channels = [("A", Color32::from_gray(200)), ("B", Color32::from_rgb(110, 150, 255)), ("G", Color32::from_rgb(110, 210, 110)), ("R", Color32::from_rgb(240, 95, 90))];
        for (i, (name, tint)) in channels.into_iter().enumerate() {
            let text = p.map_or("  —   ".to_string(), |p| format!("{:>6.3}", p.px[3 - i]));
            ui.label(egui::RichText::new(text).monospace().color(Color32::from_gray(225)));
            ui.label(egui::RichText::new(name).monospace().strong().color(tint));
        }
    }

    /// The planes shown, when [`Channel::Plane`] is on and `img` has the
    /// layer (else none).
    fn shown_planes<'a>(&self, img: &'a Image) -> Vec<&'a Plane> {
        if self.channel != Channel::Plane {
            return Vec::new();
        }
        img.layer(&self.plane).iter().filter_map(|n| img.plane(n)).collect()
    }

    fn upload(&mut self, ctx: &egui::Context, img: &Arc<Image>, gpu: Option<&Arc<arclight::gpu::Resident>>) {
        let up_to_date = self.shown.as_ref().is_some_and(|(shown, ch, ex, plane)| {
            Arc::ptr_eq(shown, img) && *ch == self.channel && *ex == self.exposure && *plane == self.plane
        });
        if up_to_date {
            return;
        }
        // A resolution change (e.g. a new clip) re-fits the view.
        if self.shown.as_ref().is_none_or(|(s, ..)| s.format != img.format) {
            self.fit_pending = true;
        }
        self.region = img.frame().union(&img.window());
        // Past the GPU's texture size limit, show it at a fraction of its
        // resolution (the readout still reads full-resolution pixels).
        let max_side = ctx.input(|i| i.max_texture_side).max(1);
        let step = self.region.width().max(self.region.height()).div_ceil(max_side).max(1);
        let planes = self.shown_planes(img);
        // Still on the GPU, on the window's own device: displayed there.
        self.native_shown = planes.is_empty() && gpu.is_some_and(|r| self.display_on_gpu(r, step));
        if self.native_shown {
            self.shown = Some((img.clone(), self.channel, self.exposure, self.plane.clone()));
            return;
        }
        let ci = match planes[..] {
            [] => to_color_image(img, self.region, self.channel, self.exposure, step),
            [plane] => plane_image(img, plane, self.region, step),
            _ => layer_image(img, &planes, self.region, self.exposure, step),
        };
        let filter = if self.nearest { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
        match &mut self.texture {
            Some(t) => t.set(ci, filter),
            None => self.texture = Some(ctx.load_texture("viewer", ci, filter)),
        }
        self.shown = Some((img.clone(), self.channel, self.exposure, self.plane.clone()));
    }
}

impl Viewer {
    /// Display `r` (the frame shown) into the native texture with
    /// `gpu::display`, over [`Viewer::region`] every `step` pixels; false
    /// (shown the CPU's way instead) when the window's device isn't the
    /// one it lives on, or on a GPU error.
    #[cfg(feature = "gpu")]
    fn display_on_gpu(&mut self, r: &Arc<arclight::gpu::Resident>, step: usize) -> bool {
        use arclight::gpu::display;
        let Some(rs) = &self.wgpu else { return false };
        let gpu = r.gpu();
        if rs.device != gpu.device {
            return false;
        }
        let channel = match self.channel {
            Channel::Rgb => display::Channel::Rgb,
            Channel::Red => display::Channel::Red,
            Channel::Green => display::Channel::Green,
            Channel::Blue => display::Channel::Blue,
            Channel::Alpha | Channel::Plane => display::Channel::Alpha,
        };
        let (tw, th) = (self.region.width().div_ceil(step).max(1) as u32, self.region.height().div_ceil(step).max(1) as u32);
        let filter = if self.nearest { eframe::wgpu::FilterMode::Nearest } else { eframe::wgpu::FilterMode::Linear };
        let fits = self.native.as_ref().is_some_and(|n| n.texture.width() == tw && n.texture.height() == th);
        if !fits {
            let texture = display::texture(gpu, tw, th);
            let view = display::view(&texture);
            let mut renderer = rs.renderer.write();
            let id = match self.native.take() {
                Some(old) => {
                    renderer.update_egui_texture_from_wgpu_texture(&rs.device, &view, filter, old.id);
                    old.id
                }
                None => renderer.register_native_texture(&rs.device, &view, filter),
            };
            self.native = Some(Native { id, texture, nearest: self.nearest });
        }
        let Some(native) = self.native.as_mut() else { return false };
        if native.nearest != self.nearest {
            rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, &display::view(&native.texture), filter, native.id);
            native.nearest = self.nearest;
        }
        match display::draw(gpu, &r.image, self.region, channel, self.exposure, step, &native.texture) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("arclight-gui: GPU display: {e:#}");
                false
            }
        }
    }
}

/// Built without GPU support: always the CPU's way.
#[cfg(not(feature = "gpu"))]
impl Viewer {
    fn display_on_gpu(&mut self, _r: &Arc<arclight::gpu::Resident>, _step: usize) -> bool {
        false
    }
}

/// Linear premultiplied float -> 8-bit sRGB for display, with exposure and
/// channel isolation applied, over `region` (frame pixels). Color is shown
/// premultiplied, i.e. over black; overscan (past the frame) is dimmed.
/// With `step` > 1, each texel is the mean of a `step`×`step` block, for
/// images larger than a texture can be.
/// An error's color over a view.
pub const ERROR_COLOR: Color32 = Color32::from_rgb(255, 110, 100);

/// A message over a view (a node's error, say, with a crash report's
/// path): wrapped to the part of `area` that shows, on a dark backing,
/// placed in it by `align` (a corner, or the middle), and selectable, so
/// it can be copied. Only the text takes the pointer (to select it); the
/// view around it keeps its clicks, drags and scrolls.
pub fn view_message(ui: &mut egui::Ui, area: Rect, align: Align2, text: &str, color: Color32, size: f32) {
    let room = area.intersect(ui.clip_rect()).shrink(10.0);
    // Laid out once, and the label given that very layout, so its text is
    // where the backing is (a label laid out again could wrap otherwise).
    let galley = ui.painter().layout(text.to_string(), FontId::proportional(size), color, room.width().max(40.0));
    let rect = align.anchor_size(align.pos_in_rect(&room), galley.size());
    ui.painter().rect_filled(rect.expand(4.0), 3.0, Color32::from_black_alpha(170));
    // (A plain layout: `Ui::put`'s centered and justified one would spread
    // the text.)
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.add(egui::Label::new(galley).selectable(true));
    });
}

/// The frame's size labels (Nuke's): "0,0" just below its bottom-left
/// corner and "W,H" just above its top-right one (each within the frame's
/// width), for a frame of `format` drawn at `frame` on screen; each a
/// position, the text's anchor there, and the text.
fn frame_size_labels(frame: Rect, format: Size) -> [(Pos2, Align2, String); 2] {
    let gap = Vec2::new(0.0, FRAME_SIZE_GAP);
    [
        (frame.left_bottom() + gap, Align2::LEFT_TOP, "0,0".into()),
        (frame.right_top() - gap, Align2::RIGHT_BOTTOM, format!("{},{}", format.width, format.height)),
    ]
}

fn to_color_image(img: &Image, region: IRect, channel: Channel, exposure: f32, step: usize) -> ColorImage {
    let gain = 2f32.powf(exposure);
    let (tw, th) = (region.width().div_ceil(step).max(1), region.height().div_ceil(step).max(1));
    let frame = img.frame();
    let mut bytes = vec![0u8; tw * th * 4];
    bytes
        .par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(i, out)| {
            let (x, y) = (region.x0 + ((i % tw) * step) as i32, region.y0 + ((i / tw) * step) as i32);
            let px = if step == 1 {
                img.at(x, y)
            } else {
                let mut sum = [0.0f32; 4];
                let (x1, y1) = ((x + step as i32).min(region.x1), (y + step as i32).min(region.y1));
                for sy in y..y1 {
                    for sx in x..x1 {
                        let p = img.at(sx, sy);
                        (0..4).for_each(|c| sum[c] += p[c]);
                    }
                }
                let n = ((x1 - x) * (y1 - y)).max(1) as f32;
                sum.map(|v| v / n)
            };
            let gain = if frame.contains(x, y) { gain } else { gain * 0.35 };
            let enc = |v: f32| (color::linear_to_srgb(v * gain) * 255.0 + 0.5) as u8;
            let rgb = match channel {
                Channel::Rgb => [enc(px[0]), enc(px[1]), enc(px[2])],
                Channel::Red => [enc(px[0]); 3],
                Channel::Green => [enc(px[1]); 3],
                Channel::Blue => [enc(px[2]); 3],
                // Alpha is coverage, not light: show it without the sRGB curve.
                Channel::Alpha | Channel::Plane => [(px[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8; 3],
            };
            out.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        });
    ColorImage::from_rgba_unmultiplied([tw, th], &bytes)
}

/// `plane` of `img` as gray over `region`: its lowest non-zero value dark,
/// its highest white, 0 (nothing there) black; overscan dimmed. With
/// `step` above 1, one sample per `step`×`step` block (values such as depth
/// don't average).
fn plane_image(img: &Image, plane: &Plane, region: IRect, step: usize) -> ColorImage {
    let (lo, hi) = plane.data.iter().filter(|v| **v != 0.0 && v.is_finite()).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let (tw, th) = (region.width().div_ceil(step).max(1), region.height().div_ceil(step).max(1));
    let frame = img.frame();
    let mut bytes = vec![0u8; tw * th * 4];
    bytes.par_chunks_exact_mut(4).enumerate().for_each(|(i, out)| {
        let (x, y) = (region.x0 + ((i % tw) * step) as i32, region.y0 + ((i / tw) * step) as i32);
        let v = img.plane_value(plane, x, y);
        let g = if v == 0.0 || !v.is_finite() { 0.0 } else { 0.15 + 0.85 * ((v - lo) / span).clamp(0.0, 1.0) };
        let g = if frame.contains(x, y) { g } else { g * 0.35 };
        let g = (g * 255.0 + 0.5) as u8;
        out.copy_from_slice(&[g, g, g, 255]);
    });
    ColorImage::from_rgba_unmultiplied([tw, th], &bytes)
}

/// A layer of two to four planes (`P`, `N`, `uv`, …) as color: its first
/// three planes as red, green and blue, values as they are (with exposure),
/// over `region` (frame pixels; overscan dimmed), sampled every `step`.
fn layer_image(img: &Image, planes: &[&Plane], region: IRect, exposure: f32, step: usize) -> ColorImage {
    let gain = 2f32.powf(exposure);
    let (tw, th) = (region.width().div_ceil(step).max(1), region.height().div_ceil(step).max(1));
    let frame = img.frame();
    let mut bytes = vec![0u8; tw * th * 4];
    bytes.par_chunks_exact_mut(4).enumerate().for_each(|(i, out)| {
        let (x, y) = (region.x0 + ((i % tw) * step) as i32, region.y0 + ((i / tw) * step) as i32);
        let gain = if frame.contains(x, y) { gain } else { gain * 0.35 };
        let c = |k: usize| planes.get(k).map_or(0, |p| (color::linear_to_srgb(img.plane_value(p, x, y) * gain).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.copy_from_slice(&[c(0), c(1), c(2), 255]);
    });
    ColorImage::from_rgba_unmultiplied([tw, th], &bytes)
}

/// The node whose 3D scene viewing `node` shows: the nearest scene-making
/// node up its inputs (see [`crate::worker::scene_source`]: a Grade or
/// Merge after a render shows the render's), itself when it outputs a
/// scene (Camera, Card, Scene, …) or solves one (a Camera Tracker: its
/// camera and points); for a Scanline Render, what feeds its first
/// connected scene input (its scene, camera or extra inputs: any of them
/// shows the render's whole scene). None: no scene.
pub fn scene_of(doc: &Doc, node: &str) -> Option<String> {
    let n = doc.node(&crate::worker::scene_source(&doc.graph, node)?)?;
    let node = n.id.as_str();
    if ops::output_kind(&n.op) == ops::Kind::Scene || n.op == "camera_tracker" {
        return Some(node.to_string());
    }
    let scene_in = |(i, src): &(usize, &String)| !src.is_empty() && ops::port(&n.op, *i).is_some_and(|p| p.kind == ops::Kind::Scene);
    n.inputs.iter().enumerate().find(scene_in).map(|(_, src)| src.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;

    #[test]
    fn the_frame_size_labels_sit_outside_its_corners_at_any_zoom() {
        let canvas = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        let format = Size { width: 3840, height: 2160 };
        for (zoom, pan) in [(0.2, Vec2::ZERO), (1.0, Vec2::new(-300.0, 120.0)), (8.0, Vec2::new(40.0, -10.0))] {
            // The frame on screen as the viewer draws it.
            let frame = Rect::from_center_size(canvas.center() + pan, Vec2::new(3840.0, 2160.0) * zoom);
            let [(p0, a0, t0), (p1, a1, t1)] = frame_size_labels(frame, format);
            assert_eq!((t0.as_str(), t1.as_str()), ("0,0", "3840,2160"));
            // Under the bottom-left corner, over the top-right one, the
            // same few points away whatever the zoom.
            assert_eq!((p0, a0), (frame.left_bottom() + Vec2::new(0.0, FRAME_SIZE_GAP), Align2::LEFT_TOP));
            assert_eq!((p1, a1), (frame.right_top() - Vec2::new(0.0, FRAME_SIZE_GAP), Align2::RIGHT_BOTTOM));
        }
        assert!(Viewer::default().show_frame_size, "on by default");
    }

    #[test]
    fn a_first_3d_setup_switches_to_3d_and_says_how() {
        let mut v = Viewer::default();
        assert!(!v.mode3d && v.hint(0.0).is_none());
        v.announce_3d(10.0);
        assert!(v.mode3d && v.pulse_until > 10.0);
        assert_eq!(v.hint(12.0), Some(HINT_3D));
        assert!(v.hint(10.0 + HINT_SECONDS + 0.1).is_none(), "and then it goes");
    }

    #[test]
    fn passes_show_a_button_a_layer_vectors_as_color() {
        let mut img = Image::filled(4, 2, [0.0, 0.0, 0.0, 1.0]);
        *img.plane_mut("depth") = vec![5.0; 8];
        for (n, v) in [("N.x", 1.0), ("N.y", 0.0), ("N.z", 0.5)] {
            *img.plane_mut(n) = vec![v; 8];
        }
        *img.plane_mut("matte.ball") = vec![1.0; 8];
        assert_eq!(img.layers().iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["depth", "N", "matte.ball"]);
        let mut v = Viewer { channel: Channel::Plane, plane: "N".into(), ..Viewer::default() };
        assert_eq!(v.shown_planes(&img).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["N.x", "N.y", "N.z"]);
        let planes = v.shown_planes(&img);
        let ci = layer_image(&img, &planes, img.frame(), 0.0, 1);
        let px = ci.pixels[0];
        assert!(px.r() == 255 && px.g() == 0 && px.b() > 150 && px.b() < 200, "{px:?}");
        v.plane = "depth".into();
        assert_eq!(v.shown_planes(&img).len(), 1);
        v.channel = Channel::Rgb;
        assert!(v.shown_planes(&img).is_empty());
    }

    /// A card, light or camera picked in the node graph gets the gizmo in
    /// the 3D view wherever it is: in the viewed scene, viewed through a 2D
    /// node after the render, or outside the scene (a ghost, said once);
    /// and when it can't, the viewer says why.
    #[test]
    fn a_node_picked_in_the_graph_gets_the_gizmo_or_a_reason() {
        use crate::viewport3d::{Projector, ViewCam};
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "card", "op": "card" },
            { "id": "scene", "op": "scene", "inputs": ["card"] },
            { "id": "cam", "op": "camera", "params": { "translate": [0, 0, 6] } },
            { "id": "sun", "op": "light_directional" },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam", "", "sun"] },
            { "id": "graded", "op": "grade", "inputs": ["render"] },
            { "id": "loose", "op": "card", "params": { "translate": [1, 0, 0] } },
            { "id": "spot", "op": "light_spot", "params": { "translate": [0, 2, 0] } },
            { "id": "cam2", "op": "camera", "params": { "translate": [3, 0, 4] } },
            { "id": "behind", "op": "card", "params": { "translate": [0, 0, 1000] } },
            { "id": "broken", "op": "read_geo", "params": { "file": "/no/such/model.obj" } },
        ] }))
        .unwrap();
        let mut doc = Doc::new(graph.clone(), None);
        assert_eq!(scene_of(&doc, "graded").as_deref(), Some("scene"), "a 2D node after the render shows its scene");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let view = ViewCam::default().projector(rect);
        let mut eval = arclight::graph::Evaluator::new();
        let scene = crate::worker::scene_of(&mut eval, &graph, "render", 1).unwrap();
        let mut v = Viewer::default();
        let mut pick = |v: &mut Viewer, doc: &mut Doc, node: &str, through: Option<Projector>| {
            doc.select_only(node);
            let (ghosts, errors) = crate::worker::ghosts_of(&mut eval, &graph, scene.as_deref(), &doc.selection(), 1);
            v.viewport3d.show_for_test(scene.clone(), ghosts, through.unwrap_or(view));
            v.viewport3d.set_ghost_errors_for_test(errors);
            (v.gizmo(doc).map(|g| g.1), v.gizmo_why(doc))
        };
        for n in ["card", "cam", "sun"] {
            assert_eq!(pick(&mut v, &mut doc, n, None), (Some(n.to_string()), None), "{n}: in the scene");
        }
        for n in ["loose", "spot", "cam2"] {
            let (gizmo, why) = pick(&mut v, &mut doc, n, None);
            assert_eq!(gizmo.as_deref(), Some(n), "{n}: not in the scene, still a gizmo");
            assert_eq!(why, Some(format!("{n} isn't in the viewed scene: it's shown dashed so you can move it")));
        }
        assert_eq!(pick(&mut v, &mut doc, "behind", None), (None, Some("behind is behind the view: press F to frame it".into())));
        let cam2 = crate::worker::ghosts_of(&mut arclight::graph::Evaluator::new(), &graph, scene.as_deref(), &["cam2".to_string()], 1).0.unwrap().cameras[0].clone();
        v.viewport3d.look_through_node("cam2");
        let through = Projector::through(&cam2, Size { width: 1920, height: 1080 }, rect);
        assert_eq!(pick(&mut v, &mut doc, "cam2", Some(through)), (None, Some("You're looking through cam2: press 0 to come out and move it".into())));
        v.viewport3d.stop_looking_through();
        let (gizmo, why) = pick(&mut v, &mut doc, "broken", None);
        assert!(gizmo.is_none() && why.as_deref().is_some_and(|w| w.starts_with("broken can't be shown in 3D: ")), "{why:?}");
        // The Select tool shows no gizmo, by choice: nothing to say.
        v.tool = Tool::Select;
        assert_eq!(pick(&mut v, &mut doc, "loose", None), (None, None));
    }

    /// A selected object shows the Move gizmo; G / R / S start their
    /// transform and pick that tool, which stays after Esc; Alt + a key
    /// resets and leaves the tool alone.
    #[test]
    fn g_r_s_pick_the_gizmo_they_start() {
        use crate::viewport3d::ViewCam;
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "card", "op": "card" },
            { "id": "scene", "op": "scene", "inputs": ["card"] },
        ] }))
        .unwrap();
        let mut doc = Doc::new(graph.clone(), None);
        doc.select_only("card");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut eval = arclight::graph::Evaluator::new();
        let scene = crate::worker::scene_of(&mut eval, &graph, "scene", 1).unwrap();
        let (ghosts, _) = crate::worker::ghosts_of(&mut eval, &graph, scene.as_deref(), &doc.selection(), 1);
        let mut v = Viewer::default();
        v.viewport3d.show_for_test(scene, ghosts, ViewCam::default().projector(rect));
        assert_eq!(v.tool, Tool::Move);
        assert_eq!(v.gizmo(&doc).map(|g| g.0.tool), Some(Tool::Move));
        let ctx = egui::Context::default();
        let key = |key: egui::Key, modifiers: egui::Modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let frame = |v: &mut Viewer, doc: &mut Doc, events: Vec<egui::Event>| {
            let mut events = events;
            events.insert(0, egui::Event::PointerMoved(Pos2::new(300.0, 200.0)));
            let raw = egui::RawInput { screen_rect: Some(rect), events, ..Default::default() };
            ctx.run_ui(raw, |ui| {
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                v.transform_keys(ui, &resp, doc, 1);
            })
            .drop_without_applying_deltas();
        };
        frame(&mut v, &mut doc, vec![]);
        for (k, tool) in [(egui::Key::R, Tool::Rotate), (egui::Key::S, Tool::Scale), (egui::Key::G, Tool::Move)] {
            for from in [Tool::Select, Tool::All, Tool::Move] {
                v.tool = from;
                frame(&mut v, &mut doc, vec![key(k, egui::Modifiers::NONE)]);
                assert!(v.is_transforming(), "{k:?} from {from:?}: started");
                assert_eq!(v.tool, tool, "{k:?} from {from:?}");
                frame(&mut v, &mut doc, vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
                assert!(!v.is_transforming(), "{k:?}: Esc cancels");
                assert_eq!(v.tool, tool, "{k:?}: the tool stays after Esc");
            }
        }
        v.tool = Tool::All;
        frame(&mut v, &mut doc, vec![key(egui::Key::R, egui::Modifiers::ALT)]);
        assert!(!v.is_transforming());
        assert_eq!(v.tool, Tool::All, "Alt + R resets, the tool as it was");
        // Nothing selected: no transform, no change.
        doc.clear_selection();
        frame(&mut v, &mut doc, vec![key(egui::Key::S, egui::Modifiers::NONE)]);
        assert_eq!(v.tool, Tool::All);
    }

    #[test]
    fn the_scene_of_a_node_is_its_own_or_its_scene_input() {
        let mut d = Doc::new(Graph::default(), None);
        let cam = d.add_node("camera", [0.0, 0.0]);
        let card = d.add_node("card", [0.0, 100.0]);
        let render = d.add_node("scanline_render", [200.0, 0.0]);
        let blur = d.add_node("blur", [400.0, 0.0]);
        assert_eq!(scene_of(&d, &cam).as_deref(), Some(cam.as_str()));
        assert_eq!(scene_of(&d, &card).as_deref(), Some(card.as_str()));
        assert_eq!(scene_of(&d, &render), None, "nothing in its scene input yet");
        let port = ops::info("scanline_render").unwrap().inputs.iter().position(|p| p.kind == ops::Kind::Scene).unwrap();
        d.connect(&card, &render, port);
        assert_eq!(scene_of(&d, &render).as_deref(), Some(card.as_str()));
        assert_eq!(scene_of(&d, &blur), None);
        let tracker = d.add_node("camera_tracker", [0.0, 300.0]);
        assert_eq!(scene_of(&d, &tracker).as_deref(), Some(tracker.as_str()), "its solved camera and points");
        // Only a light on a render's extra input: still a 3D scene.
        let light = d.add_node("light_area", [0.0, 400.0]);
        let lit = d.add_node("scanline_render", [200.0, 400.0]);
        d.commit(false);
        assert_eq!(scene_of(&d, &lit), None);
        assert!(d.connect(&light, &lit, 3));
        assert_eq!(scene_of(&d, &lit).as_deref(), Some(light.as_str()));
        // A Viewer shows what feeds it.
        let viewer = d.add_node("viewer", [600.0, 0.0]);
        assert_eq!(scene_of(&d, &viewer), None);
        d.connect(&render, &viewer, 0);
        assert_eq!(scene_of(&d, &viewer).as_deref(), Some(card.as_str()));
    }

    /// Tab over the viewer (main.rs) must not also move keyboard focus:
    /// egui reads Tab when the frame starts and focuses the first widget
    /// that wants focus, unless the move is called off before any widget.
    /// A long error over a view wraps within it, and only its text takes
    /// the pointer: a drag beside it still reaches the view underneath.
    #[test]
    fn a_long_error_wraps_and_leaves_the_view_its_drags() {
        let err = "soft: plugin /home/someone/plugins/libarclight_plugin_crashy.so crashed: killed by SIGSEGV; its crash report: /home/someone/.local/state/arclight/crashes/plugin-host-20261031-030000-12345-native.txt";
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let press = |at: Pos2, down: bool| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE };
        // Whether the view under the message takes a drag from `from`.
        let drag = |from: Pos2| {
            let ctx = egui::Context::default();
            let mut dragged = false;
            let to = from + Vec2::new(40.0, 0.0);
            for events in [vec![egui::Event::PointerMoved(from)], vec![press(from, true)], vec![egui::Event::PointerMoved(from + Vec2::new(20.0, 0.0))], vec![egui::Event::PointerMoved(to)], vec![press(to, false)]] {
                let raw = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
                ctx.run_ui(raw, |ui| {
                    let rect = ui.max_rect();
                    let view = ui.allocate_rect(rect, Sense::click_and_drag());
                    view_message(ui, rect, Align2::LEFT_TOP, err, ERROR_COLOR, 13.0);
                    dragged |= view.dragged();
                })
                .drop_without_applying_deltas();
            }
            dragged
        };
        let font = FontId::proportional(13.0);
        let one_line = egui::Context::default();
        let mut line_h = 0.0;
        one_line.run_ui(egui::RawInput::default(), |ui| line_h = ui.painter().layout_no_wrap("x".into(), font.clone(), ERROR_COLOR).size().y).drop_without_applying_deltas();
        // Wrapped: several lines within the view's width.
        let ctx = egui::Context::default();
        let mut size = Vec2::ZERO;
        ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| size = ui.painter().layout(err.into(), font.clone(), ERROR_COLOR, 400.0 - 20.0).size()).drop_without_applying_deltas();
        assert!(size.x <= 380.0 && size.y >= 3.0 * line_h, "{size:?}");
        // Beside the text (below it): the view pans. On the text: it selects.
        assert!(drag(Pos2::new(200.0, 250.0)), "a drag beside the message reaches the view");
        assert!(!drag(Pos2::new(20.0, 14.0)), "a drag on the text selects it instead");
    }

    #[test]
    fn the_toolbars_wrap_in_a_narrow_view() {
        use arclight::image::{IRect, Size};
        let img = Arc::new(Image::blank(Size { width: 1920, height: 1080 }, IRect::new(0, 0, 1920, 1080)));
        let d = Display { image: Some(&img), gpu: None, error: None, target: Some("v"), returns_to: None, frame: 1, millis: 56.2, input: None };
        for mode3d in [false, true] {
            for width in [360.0, 520.0, 1400.0] {
                let ctx = egui::Context::default();
                let mut v = Viewer::default();
                let mut right = 0.0;
                // Twice: the status's width is known from the first.
                for _ in 0..2 {
                    let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 600.0))), ..Default::default() };
                    ctx.run_ui(raw, |ui| {
                        if mode3d {
                            v.toolbar_3d(ui, &d);
                        } else {
                            v.toolbar(ui, &d);
                        }
                        right = ui.min_rect().right();
                    })
                    .drop_without_applying_deltas();
                }
                assert!(right <= width, "3D {mode3d}, {width} wide: the controls reach {right}");
            }
        }
    }

    #[test]
    fn calling_off_the_focus_move_keeps_tab_from_focusing_a_field() {
        let tab = egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        for call_off in [false, true] {
            let ctx = egui::Context::default();
            let mut text = String::new();
            let mut focused = false;
            for events in [vec![], vec![tab.clone()], vec![]] {
                let raw = egui::RawInput { events, ..Default::default() };
                ctx.run_ui(raw, |ui| {
                    if call_off && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
                        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                    }
                    focused = ui.text_edit_singleline(&mut text).has_focus();
                }).drop_without_applying_deltas();
            }
            assert_eq!(focused, !call_off, "call_off {call_off}");
        }
    }

    /// A result on the GPU displays as the CPU's `to_color_image` would
    /// show it: every channel, exposure, overscan dimmed, and blocks
    /// averaged past the texture size; a byte apart at most, where the
    /// GPU's `pow` rounds the other way.
    #[cfg(feature = "gpu")]
    #[test]
    fn a_gpu_result_displays_as_on_the_cpu() {
        use arclight::gpu::display;
        let Some(gpu) = arclight::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        // HDR noise over a window spilling past the frame (overscan).
        let img = arclight::gpu::check::noise(Size { width: 160, height: 90 }, IRect::new(-12, -5, 171, 97), false, 3);
        let up = arclight::gpu::GpuImage::upload(&gpu, &img).unwrap();
        let region = img.frame().union(&img.window());
        let cases = [
            (Channel::Rgb, display::Channel::Rgb, 0.0, 1),
            (Channel::Rgb, display::Channel::Rgb, -1.5, 1),
            (Channel::Red, display::Channel::Red, 2.0, 1),
            (Channel::Green, display::Channel::Green, 0.0, 1),
            (Channel::Blue, display::Channel::Blue, 0.0, 1),
            (Channel::Alpha, display::Channel::Alpha, 3.0, 1),
            (Channel::Rgb, display::Channel::Rgb, 0.5, 3),
        ];
        for (cpu_channel, channel, exposure, step) in cases {
            let want = to_color_image(&img, region, cpu_channel, exposure, step);
            let texture = display::texture(&gpu, want.size[0] as u32, want.size[1] as u32);
            // As egui draws it: Rgba8Unorm (its shader takes the bytes as
            // they are), through a valid view (a GPU error otherwise).
            assert_eq!(texture.format(), eframe::wgpu::TextureFormat::Rgba8Unorm);
            gpu.scoped(|| Ok(display::view(&texture))).unwrap();
            display::draw(&gpu, &up, region, channel, exposure, step, &texture).unwrap();
            let got = display::read_texture(&gpu, &texture).unwrap();
            let want: Vec<u8> = want.pixels.iter().flat_map(|c| c.to_array()).collect();
            assert_eq!(got.len(), want.len());
            let off = got.iter().zip(&want).filter(|(g, w)| g != w).count();
            let worst = got.iter().zip(&want).map(|(g, w)| g.abs_diff(*w)).max().unwrap();
            assert!(worst <= 1 && off * 1000 <= want.len(), "{channel:?} {exposure} step {step}: {off} bytes off, by up to {worst}");
        }
    }
}
