//! The image viewer: shows the rendered frame with pan/zoom, channel isolation,
//! exposure, and a float pixel readout under the cursor. Pixels stored past
//! the frame (overscan) show dimmed, and a data window smaller or larger than
//! the frame is outlined.

use crate::doc::Doc;
use crate::overlay::{Overlays, Xf};
use arclight::color;
use arclight::image::{IRect, Image};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, TextureHandle, TextureOptions,
    Vec2,
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
    shown: Option<(Arc<Image>, Channel, f32)>,
    /// What the texture covers, in frame pixels: the frame and the data window.
    region: IRect,
    nearest: bool,
    pub channel: Channel,
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
    pub rect: Rect,
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer {
            texture: None,
            shown: None,
            region: IRect::default(),
            nearest: false,
            channel: Channel::Rgb,
            exposure: 0.0,
            zoom: 1.0,
            pan: Vec2::ZERO,
            status_w: 0.0,
            fit_pending: true,
            overlays: Overlays::default(),
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

    /// `doc` and `frame` drive the selected node's overlay handles; edits
    /// key animated params at `frame`.
    pub fn ui(&mut self, ui: &mut egui::Ui, d: Display<'_>, doc: &mut Doc, frame: i64) {
        self.toolbar(ui, &d);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(24));

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

        // Pixel readout, anywhere pixels are shown (overscan included).
        if let Some(hover) = resp.hover_pos() {
            let p = (hover - img_rect.min) / self.zoom;
            let (x, y) = (p.x.floor() as i32, p.y.floor() as i32);
            if self.region.contains(x, y) {
                let px = img.at(x, y);
                let text = format!(
                    "x {:<5} y {:<5}   R {:.4}  G {:.4}  B {:.4}  A {:.4}",
                    x, y, px[0], px[1], px[2], px[3]
                );
                let pos = rect.left_bottom() + Vec2::new(8.0, -8.0);
                let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::from_gray(220));
                let r = Align2::LEFT_BOTTOM.anchor_size(pos, galley.size());
                painter.rect_filled(r.expand(4.0), 3.0, Color32::from_black_alpha(160));
                painter.galley(r.min, galley, Color32::from_gray(220));
            }
        }
        if let Some(err) = d.error {
            let pos = rect.left_top() + Vec2::new(8.0, 8.0);
            painter.text(pos, Align2::LEFT_TOP, err, FontId::proportional(13.0), Color32::from_rgb(255, 110, 100));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, d: &Display<'_>) {
        let mut fits = true;
        ui.horizontal(|ui| {
            for (ch, label) in Channel::ALL {
                ui.selectable_value(&mut self.channel, ch, label);
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

    fn upload(&mut self, ctx: &egui::Context, img: &Arc<Image>) {
        let up_to_date = self.shown.as_ref().is_some_and(|(shown, ch, ex)| {
            Arc::ptr_eq(shown, img) && *ch == self.channel && *ex == self.exposure
        });
        if up_to_date {
            return;
        }
        // A resolution change (e.g. a new clip) re-fits the view.
        if self.shown.as_ref().is_none_or(|(s, ..)| s.format != img.format) {
            self.fit_pending = true;
        }
        self.region = img.frame().union(&img.window());
        let ci = to_color_image(img, self.region, self.channel, self.exposure);
        let filter = if self.nearest { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
        match &mut self.texture {
            Some(t) => t.set(ci, filter),
            None => self.texture = Some(ctx.load_texture("viewer", ci, filter)),
        }
        self.shown = Some((img.clone(), self.channel, self.exposure));
    }
}

/// Linear premultiplied float -> 8-bit sRGB for display, with exposure and
/// channel isolation applied, over `region` (frame pixels). Color is shown
/// premultiplied, i.e. over black; overscan (past the frame) is dimmed.
fn to_color_image(img: &Image, region: IRect, channel: Channel, exposure: f32) -> ColorImage {
    let gain = 2f32.powf(exposure);
    let (w, frame) = (region.width().max(1), img.frame());
    let mut bytes = vec![0u8; region.width() * region.height() * 4];
    bytes
        .par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(i, out)| {
            let (x, y) = (region.x0 + (i % w) as i32, region.y0 + (i / w) as i32);
            let px = img.at(x, y);
            let gain = if frame.contains(x, y) { gain } else { gain * 0.35 };
            let enc = |v: f32| (color::linear_to_srgb(v * gain) * 255.0 + 0.5) as u8;
            let rgb = match channel {
                Channel::Rgb => [enc(px[0]), enc(px[1]), enc(px[2])],
                Channel::Red => [enc(px[0]); 3],
                Channel::Green => [enc(px[1]); 3],
                Channel::Blue => [enc(px[2]); 3],
                // Alpha is coverage, not light: show it without the sRGB curve.
                Channel::Alpha => [(px[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8; 3],
            };
            out.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        });
    ColorImage::from_rgba_unmultiplied([region.width(), region.height()], &bytes)
}
