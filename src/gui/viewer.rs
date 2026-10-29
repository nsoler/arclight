//! The image viewer: shows the rendered frame with pan/zoom, channel isolation,
//! exposure, extra planes (depth) as gray, and a float pixel readout under
//! the cursor. Pixels stored past
//! the frame (overscan) show dimmed, and a data window smaller or larger than
//! the frame is outlined.

use crate::doc::Doc;
use crate::overlay::{Overlays, Xf};
use arclight::color;
use arclight::ops;
use arclight::image::{IRect, Image, Plane};
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

pub struct Viewer {
    texture: Option<TextureHandle>,
    /// The image currently uploaded, and the display settings it used.
    shown: Option<(Arc<Image>, Channel, f32, String)>,
    /// What the texture covers, in frame pixels: the frame and the data window.
    region: IRect,
    nearest: bool,
    pub channel: Channel,
    /// The plane shown with [`Channel::Plane`].
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
    pub rect: Rect,
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
            rect: Rect::NOTHING,
        }
    }
}

/// What the viewer should display this frame.
pub struct Display<'a> {
    pub image: Option<&'a Arc<Image>>,
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

    /// `doc` and `frame` drive the selected node's overlay handles; edits
    /// key animated params at `frame`.
    pub fn ui(&mut self, ui: &mut egui::Ui, d: Display<'_>, doc: &mut Doc, frame: i64) {
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
            return;
        }

        let Some(img) = d.image else {
            let msg = d.error.unwrap_or("Nothing to view — connect a node to a Viewer, or select one.");
            painter.text(rect.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), Color32::GRAY);
            return;
        };
        self.upload(ui.ctx(), img);
        let size = Vec2::new(img.format.width as f32, img.format.height as f32);

        if self.fit_pending {
            let s = (rect.width() / size.x).min(rect.height() / size.y) * 0.95;
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
            self.upload(ui.ctx(), img);
        }
        // Frame pixels → screen.
        let on_screen = |r: IRect| {
            Rect::from_min_max(
                img_rect.min + Vec2::new(r.x0 as f32, r.y0 as f32) * self.zoom,
                img_rect.min + Vec2::new(r.x1 as f32, r.y1 as f32) * self.zoom,
            )
        };
        if let Some(tex) = &self.texture {
            painter.image(tex.id(), on_screen(self.region), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        painter.rect_stroke(img_rect, 0.0, (1.0, Color32::from_gray(70)), egui::StrokeKind::Outside);
        let window = img.window();
        if window != img.frame() && !window.is_empty() {
            let r = on_screen(window);
            let ring = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.add(egui::Shape::dashed_line(&ring, egui::Stroke::new(1.0, Color32::from_rgb(120, 170, 220)), 6.0, 4.0));
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
                if let Some(plane) = self.shown_plane(img) {
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
            let pos = rect.left_top() + Vec2::new(8.0, 8.0);
            painter.text(pos, Align2::LEFT_TOP, err, FontId::proportional(13.0), Color32::from_rgb(255, 110, 100));
        }
    }

    /// The 3D view of the viewed node's scene (see [`scene_of`]).
    fn ui_3d(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, d: &Display<'_>, doc: &mut Doc, frame: i64) {
        let _ = (resp, frame);
        let painter = ui.painter_at(rect);
        let Some(node) = d.target else {
            painter.text(rect.center(), Align2::CENTER_CENTER, "Nothing to view — connect a node to a Viewer, or select one.", FontId::proportional(14.0), Color32::GRAY);
            return;
        };
        match scene_of(doc, node) {
            None => {
                let text = format!(
                    "'{node}' has no 3D scene.\nView a Camera, Card, Scene, Locator, Point Cloud or a Scanline Render to see one here,\nor press Tab (or 2D) for its image."
                );
                painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::GRAY);
            }
            // Placeholder until the viewport (src/gui/viewport3d.rs) lands.
            Some(scene) => {
                let text = format!("3D view of '{scene}'");
                painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::from_rgb(120, 190, 235));
            }
        }
    }

    /// The 2D / 3D switch, at the left of either toolbar.
    fn mode_switch(&mut self, ui: &mut egui::Ui) {
        let tip = "Show the viewed node's image (2D) or its 3D scene (3D). Tab switches while the pointer is over the viewer.";
        if ui.selectable_label(!self.mode3d, "2D").on_hover_text(tip).clicked() {
            self.mode3d = false;
        }
        if ui.selectable_label(self.mode3d, "3D").on_hover_text(tip).clicked() {
            self.mode3d = true;
        }
        ui.separator();
    }

    fn toolbar_3d(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        ui.horizontal(|ui| {
            self.mode_switch(ui);
            if let Some(t) = d.target {
                ui.weak(format!("Viewing {t}"));
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        let mut fits = true;
        ui.horizontal(|ui| {
            self.mode_switch(ui);
            for (ch, label) in Channel::ALL {
                ui.selectable_value(&mut self.channel, ch, label);
            }
            // The image's extra planes (a 3D render's depth), if any.
            for plane in d.image.map_or(&[][..], |img| &img.planes[..]) {
                let on = self.channel == Channel::Plane && self.plane == plane.name;
                if ui.selectable_label(on, &plane.name).on_hover_text(format!("Show the {} plane as gray, lowest to highest value", plane.name)).clicked() {
                    self.channel = Channel::Plane;
                    self.plane = plane.name.clone();
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

    /// The plane shown, when [`Channel::Plane`] is on and `img` has it.
    fn shown_plane<'a>(&self, img: &'a Image) -> Option<&'a Plane> {
        (self.channel == Channel::Plane).then(|| img.plane(&self.plane)).flatten()
    }

    fn upload(&mut self, ctx: &egui::Context, img: &Arc<Image>) {
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
        let ci = match self.shown_plane(img) {
            Some(plane) => plane_image(img, plane, self.region, step),
            None => to_color_image(img, self.region, self.channel, self.exposure, step),
        };
        let filter = if self.nearest { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
        match &mut self.texture {
            Some(t) => t.set(ci, filter),
            None => self.texture = Some(ctx.load_texture("viewer", ci, filter)),
        }
        self.shown = Some((img.clone(), self.channel, self.exposure, self.plane.clone()));
    }
}

/// Linear premultiplied float -> 8-bit sRGB for display, with exposure and
/// channel isolation applied, over `region` (frame pixels). Color is shown
/// premultiplied, i.e. over black; overscan (past the frame) is dimmed.
/// With `step` > 1, each texel is the mean of a `step`×`step` block, for
/// images larger than a texture can be.
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

/// The node whose 3D scene viewing `node` shows: `node` itself when it
/// outputs a scene (Camera, Card, Scene, …), else what feeds its first
/// scene input (a Scanline Render's or Camera Tracker's). A Viewer node
/// stands for what it shows. None: no scene.
pub fn scene_of(doc: &Doc, node: &str) -> Option<String> {
    let mut n = doc.node(node)?;
    if n.op == "viewer" {
        n = doc.node(n.inputs.first().filter(|s| !s.is_empty())?)?;
    }
    let node = n.id.as_str();
    if ops::output_kind(&n.op) == ops::Kind::Scene {
        return Some(node.to_string());
    }
    let ports = ops::info(&n.op)?.inputs;
    ports.iter().zip(&n.inputs).find(|(p, src)| p.kind == ops::Kind::Scene && !src.is_empty()).map(|(_, src)| src.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;

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
        // A Viewer shows what feeds it.
        let viewer = d.add_node("viewer", [600.0, 0.0]);
        assert_eq!(scene_of(&d, &viewer), None);
        d.connect(&render, &viewer, 0);
        assert_eq!(scene_of(&d, &viewer).as_deref(), Some(card.as_str()));
    }

    /// Tab over the viewer (main.rs) must not also move keyboard focus:
    /// egui reads Tab when the frame starts and focuses the first widget
    /// that wants focus, unless the move is called off before any widget.
    #[test]
    fn calling_off_the_focus_move_keeps_tab_from_focusing_a_field() {
        let tab = egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        for call_off in [false, true] {
            let ctx = egui::Context::default();
            let mut text = String::new();
            let mut focused = false;
            for events in [vec![], vec![tab.clone()], vec![]] {
                let raw = egui::RawInput { events, ..Default::default() };
                let _ = ctx.run_ui(raw, |ui| {
                    if call_off && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
                        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                    }
                    focused = ui.text_edit_singleline(&mut text).has_focus();
                });
            }
            assert_eq!(focused, !call_off, "call_off {call_off}");
        }
    }
}
