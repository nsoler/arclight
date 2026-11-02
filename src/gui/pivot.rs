//! Pivot mode in the 2D viewer: "." over the viewer toggles it for the
//! selected Transform (its `center`) or the selected Roto / Polygon points'
//! transform box. A click, or a drag, places the pivot; only the pivot
//! moves, never the picture: a Transform's translate is compensated so its
//! output stays the same. ⇧. puts the pivot back (Auto); "." or Esc leaves.

use crate::doc::Doc;
use crate::overlay::{Overlays, Xf};
use arclight::image::Image;
use arclight::{anim, link, ops};
use eframe::egui::{self, Color32, FontId, Key, Pos2, Response, Stroke, Vec2};
use serde_json::{json, Value};
use std::sync::Arc;

const COLOR: Color32 = Color32::from_rgb(255, 140, 40);
pub const HINT: &str = "Pivot: click to place · . or Esc to exit · ⇧. resets";
const NOTHING: &str = "Select a Transform, or 2+ points of a Roto or Polygon, to set a pivot";
/// How long a note stays (seconds).
const NOTE_FOR: f64 = 3.0;

#[derive(Default)]
pub struct PivotMode {
    /// The node it's setting the pivot of, while on.
    node: Option<String>,
    /// The pivot is being dragged.
    dragging: bool,
    /// A passing note, until when.
    note: Option<(String, f64)>,
}

/// What "." sets the pivot of.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// A Transform's center.
    Transform(String),
    /// The transform box of a shape's selected points (UI state only).
    Points(String),
}

impl Target {
    fn id(&self) -> &str {
        match self {
            Target::Transform(id) | Target::Points(id) => id,
        }
    }
}

fn target(doc: &Doc, overlays: &Overlays, frame: i64, size: Vec2, xf: &Xf) -> Option<Target> {
    let id = doc.selected.clone()?;
    let op = &doc.node(&id)?.op;
    if matches!(ops::info(op)?.overlay, ops::Overlay::Transform { .. }) {
        Some(Target::Transform(id))
    } else {
        overlays.selection_pivot(doc, frame, size, xf).map(|_| Target::Points(id))
    }
}

/// A Transform's values at a frame, `center` resolved (Auto: the middle
/// of the input's data window, as the op pivots).
#[derive(Clone, Copy, Debug)]
struct Tf {
    center: [f64; 2],
    auto: bool,
    translate: [f64; 2],
    rotate: f64,
    scale: [f64; 2],
}

fn pair(v: &Value, or: f64) -> [f64; 2] {
    let f = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(or);
    [f(0), f(1)]
}

/// Where a Transform with no center pivots: [`ops::transform_pivot`] of
/// its input when known, else the middle of the viewed `size` (as the
/// overlay guesses).
fn auto_center(id: &str, input: Option<&(String, Arc<Image>)>, size: Vec2) -> [f64; 2] {
    match input.filter(|(n, _)| n == id) {
        Some((_, img)) => ops::transform_pivot(img).map(f64::from),
        None => [size.x as f64 / 2.0, size.y as f64 / 2.0],
    }
}

fn read(doc: &Doc, id: &str, frame: i64, input: Option<&(String, Arc<Image>)>, size: Vec2) -> Tf {
    let at = |n: &str| doc.param_at(id, n, frame);
    let c = at("center");
    Tf {
        auto: c.is_null(),
        center: if c.is_null() { auto_center(id, input, size) } else { pair(&c, 0.0) },
        translate: pair(&at("translate"), 0.0),
        rotate: at("rotate").as_f64().unwrap_or(0.0),
        scale: pair(&at("scale"), 1.0),
    }
}

/// M·v: scale, then turn by `rotate` degrees (y down), as the op does.
fn m(tf: &Tf, v: [f64; 2]) -> [f64; 2] {
    let (s, c) = tf.rotate.to_radians().sin_cos();
    let (x, y) = (v[0] * tf.scale[0], v[1] * tf.scale[1]);
    [x * c - y * s, x * s + y * c]
}

/// M⁻¹·v (a zero scale taken as the op does, nearly zero).
fn m_inv(tf: &Tf, v: [f64; 2]) -> [f64; 2] {
    let (s, c) = (-tf.rotate).to_radians().sin_cos();
    let (x, y) = (v[0] * c - v[1] * s, v[0] * s + v[1] * c);
    let nz = |k: f64| if k == 0.0 { 1e-6 } else { k };
    [x / nz(tf.scale[0]), y / nz(tf.scale[1])]
}

/// The translate that keeps the output unchanged when the center moves
/// from `tf.center` to `c2`: the op maps p to M(p − c) + c + t, so
/// t' = t + (M − I)(c' − c).
fn compensate(tf: &Tf, c2: [f64; 2]) -> [f64; 2] {
    let d = [c2[0] - tf.center[0], c2[1] - tf.center[1]];
    let md = m(tf, d);
    [tf.translate[0] + md[0] - d[0], tf.translate[1] + md[1] - d[1]]
}

/// Where the pivot shows in the output: c + t (the center lands there).
fn shown(tf: &Tf) -> [f64; 2] {
    [tf.center[0] + tf.translate[0], tf.center[1] + tf.translate[1]]
}

fn linked(doc: &Doc, id: &str) -> bool {
    ["center", "translate"].iter().any(|n| link::is_link(&doc.param(id, n)))
}

/// Put Transform `id`'s pivot where the output shows `q` (frame pixels),
/// keeping the picture: center c' = c + M⁻¹(q − (c + t)), translate as
/// [`compensate`] (so c' + t' = q). Animated params get keys at `frame`.
fn place_transform(doc: &mut Doc, id: &str, frame: i64, q: [f64; 2], input: Option<&(String, Arc<Image>)>, size: Vec2) -> Result<(), String> {
    if linked(doc, id) {
        return Err("Center or Translate is linked: unlink it to set the pivot".into());
    }
    let tf = read(doc, id, frame, input, size);
    let at = shown(&tf);
    let back = m_inv(&tf, [q[0] - at[0], q[1] - at[1]]);
    let c2 = [tf.center[0] + back[0], tf.center[1] + back[1]];
    let t2 = compensate(&tf, c2);
    doc.edit_param(id, "translate", frame, json!(t2));
    doc.edit_param(id, "center", frame, json!(c2));
    Ok(())
}

/// Back to Auto (the middle of the input layer), keeping the picture. An
/// animated center can't be keyed empty: it's keyed at the auto spot.
fn reset_transform(doc: &mut Doc, id: &str, frame: i64, input: Option<&(String, Arc<Image>)>, size: Vec2) -> Result<(), String> {
    if linked(doc, id) {
        return Err("Center or Translate is linked: unlink it to reset the pivot".into());
    }
    let tf = read(doc, id, frame, input, size);
    if tf.auto {
        return Ok(());
    }
    let c2 = auto_center(id, input, size);
    let t2 = compensate(&tf, c2);
    doc.edit_param(id, "translate", frame, json!(t2));
    if anim::is_animated(&doc.param(id, "center")) {
        doc.edit_param(id, "center", frame, json!(c2));
    } else {
        doc.remove_param(id, "center");
    }
    Ok(())
}

/// Take a "." press (⇧ or not) from the input: the key's logical or
/// physical name, as ⇧. types another character on most layouts.
fn take_period(i: &mut egui::InputState, shift: bool) -> bool {
    let mut hit = false;
    i.events.retain(|e| {
        let take = !hit
            && matches!(e, egui::Event::Key { key, physical_key, pressed: true, modifiers, .. }
                if (*key == Key::Period || *physical_key == Some(Key::Period)) && modifiers.shift == shift && !modifiers.command && !modifiers.ctrl && !modifiers.alt);
        hit |= take;
        !take
    });
    hit
}

impl PivotMode {
    pub fn active(&self) -> bool {
        self.node.is_some()
    }

    #[cfg(test)]
    fn dragging(&self) -> bool {
        self.dragging
    }

    fn say(&mut self, now: f64, text: impl Into<String>) {
        self.note = Some((text.into(), now + NOTE_FOR));
    }

    fn reset(&mut self, doc: &mut Doc, t: &Target, frame: i64, size: Vec2, overlays: &mut Overlays) -> Result<(), String> {
        match t {
            Target::Transform(id) => reset_transform(doc, id, frame, overlays.input(), size),
            Target::Points(_) => {
                overlays.set_selection_pivot(None);
                Ok(())
            }
        }
    }

    fn place(&mut self, doc: &mut Doc, t: &Target, frame: i64, size: Vec2, q: Vec2, overlays: &mut Overlays) -> Result<(), String> {
        match t {
            Target::Transform(id) => place_transform(doc, id, frame, [q.x as f64, q.y as f64], overlays.input(), size),
            Target::Points(_) => {
                overlays.set_selection_pivot(Some(q));
                Ok(())
            }
        }
    }

    /// The keys, and while on, the pointer. True while on: the overlay's
    /// handles and panning stand aside. `resp` is the viewer canvas.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(&mut self, ui: &egui::Ui, resp: &Response, xf: &Xf, size: Vec2, doc: &mut Doc, frame: i64, overlays: &mut Overlays) -> bool {
        let now = ui.input(|i| i.time);
        let t = target(doc, overlays, frame, size, xf);
        // Off when what it was on is no longer selected (or set-able).
        if self.node.is_some() && t.as_ref().map(Target::id) != self.node.as_deref() {
            self.node = None;
            self.dragging = false;
        }
        if !ui.ctx().text_edit_focused() {
            let hovered = resp.hovered();
            let active = self.active();
            let (plain, shifted, esc) = ui.input_mut(|i| {
                let plain = hovered && take_period(i, false);
                let shifted = hovered && take_period(i, true);
                let esc = active && i.consume_key(egui::Modifiers::NONE, Key::Escape);
                (plain, shifted, esc)
            });
            if plain || esc {
                if self.active() {
                    self.node = None;
                    self.dragging = false;
                } else if let (true, Some(t)) = (plain, &t) {
                    self.node = Some(t.id().to_string());
                } else if plain {
                    self.say(now, NOTHING);
                }
            }
            if shifted {
                match &t {
                    Some(t) => {
                        if let Err(e) = self.reset(doc, t, frame, size, overlays) {
                            self.say(now, e);
                        }
                    }
                    None => self.say(now, NOTHING),
                }
            }
        }
        let Some(t) = t.filter(|_| self.active()) else { return false };
        if ui.input(|i| i.pointer.primary_pressed()) && ui.input(|i| i.pointer.press_origin()).is_some_and(|o| resp.rect.contains(o)) {
            self.dragging = true;
        }
        if self.dragging {
            if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                if let Err(e) = self.place(doc, &t, frame, size, xf.image(p), overlays) {
                    self.say(now, e);
                    self.dragging = false;
                }
            }
            if ui.input(|i| !i.pointer.primary_down()) {
                self.dragging = false;
            }
        }
        ui.ctx().request_repaint();
        true
    }

    /// The hint and the pivot's crosshair while on (a shape box's placed
    /// pivot always), and any note.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, ui: &egui::Ui, painter: &egui::Painter, xf: &Xf, size: Vec2, doc: &Doc, frame: i64, overlays: &Overlays) {
        let now = ui.input(|i| i.time);
        let rect = painter.clip_rect();
        let t = target(doc, overlays, frame, size, xf);
        let on = t.as_ref().filter(|t| self.node.as_deref() == Some(t.id()));
        let pivot = match &t {
            Some(Target::Transform(id)) if on.is_some() => {
                let s = shown(&read(doc, id, frame, overlays.input(), size));
                Some(Vec2::new(s[0] as f32, s[1] as f32))
            }
            Some(Target::Points(_)) => overlays.selection_pivot(doc, frame, size, xf).filter(|(_, placed)| *placed || on.is_some()).map(|(p, _)| p),
            _ => None,
        };
        if let Some(p) = pivot {
            crosshair(painter, xf.screen(p), on.is_some());
        }
        let mut lines = Vec::new();
        if let Some(t) = on {
            let mut hint = HINT.to_string();
            if let Target::Transform(id) = t {
                if ["center", "translate"].iter().any(|n| anim::is_animated(&doc.param(id, n))) {
                    hint += &format!(" · pivot keyed at frame {frame}");
                }
            } else {
                hint += " · for the selected points' box";
            }
            lines.push((hint, COLOR));
        }
        if let Some((text, until)) = &self.note {
            if *until > now {
                lines.push((text.clone(), Color32::from_gray(230)));
                ui.ctx().request_repaint();
            }
        }
        let mut y = rect.top() + 8.0;
        for (text, color) in lines {
            let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), color);
            let at = Pos2::new(rect.center().x - galley.size().x / 2.0, y);
            painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(5.0), 4.0, Color32::from_black_alpha(190));
            y += galley.size().y + 12.0;
            painter.galley(at, galley, color);
        }
    }
}

/// The pivot handle: a ring, a cross through it and a dot (bigger while
/// placing).
fn crosshair(painter: &egui::Painter, c: Pos2, big: bool) {
    let (r, arm) = if big { (7.0, 12.0) } else { (5.0, 8.0) };
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(160));
    let line = Stroke::new(1.5, COLOR);
    for s in [shadow, line] {
        painter.circle_stroke(c, r, s);
        painter.line_segment([c - Vec2::new(arm, 0.0), c + Vec2::new(arm, 0.0)], s);
        painter.line_segment([c - Vec2::new(0.0, arm), c + Vec2::new(0.0, arm)], s);
    }
    painter.circle_filled(c, 1.8, COLOR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::{Graph, NodeSpec};
    use arclight::ops::EvalContext;

    const CTX: EvalContext = EvalContext { frame: 1, write_outputs: false };
    const SIZE: Vec2 = Vec2::new(64.0, 48.0);

    /// A busy picture: a checkerboard under a soft ellipse, on part of
    /// the frame (so Auto pivots off the frame's middle).
    fn picture() -> Arc<Image> {
        let ch = ops::eval("checkerboard", &json!({ "width": 64, "height": 48, "size": 5 }), &[None], &CTX).unwrap();
        let cut = ops::eval("crop", &json!({ "left": 6, "top": 4, "right": 20, "bottom": 10 }), &[Some(&ch)], &CTX).unwrap_or(ch);
        Arc::new(cut)
    }

    fn doc_with(params: Value) -> Doc {
        let node = NodeSpec { id: "t".into(), op: "transform".into(), inputs: vec![String::new()], params, pos: Some([0.0, 0.0]), disabled: false };
        let mut d = Doc::new(Graph { nodes: vec![node], ..Default::default() }, None);
        d.select_only("t");
        d
    }

    fn render(d: &Doc, img: &Image, frame: i64) -> Image {
        let raw = d.node("t").unwrap().params.as_object().cloned().unwrap_or_default();
        let p = Value::Object(raw.into_iter().map(|(k, v)| (k, anim::evaluate(&v, frame as f64))).collect());
        ops::eval("transform", &p, &[Some(img), None], &EvalContext { frame, write_outputs: false }).unwrap()
    }

    fn same(a: &Image, b: &Image) -> f32 {
        let (a, b) = (a.flat(), b.flat());
        assert_eq!(a.pixels.len(), b.pixels.len());
        a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |k| (p[k] - q[k]).abs())).fold(0.0, f32::max)
    }

    #[test]
    fn the_compensation_matches_the_op() {
        let tf = Tf { center: [10.0, 20.0], auto: false, translate: [3.0, -2.0], rotate: 30.0, scale: [1.5, 0.8] };
        let c2 = [25.0, 8.0];
        let t2 = compensate(&tf, c2);
        // Any point maps the same with either pivot.
        let map = |c: [f64; 2], t: [f64; 2], p: [f64; 2]| {
            let v = m(&tf, [p[0] - c[0], p[1] - c[1]]);
            [v[0] + c[0] + t[0], v[1] + c[1] + t[1]]
        };
        for p in [[0.0, 0.0], [40.0, -7.0], [12.5, 33.0]] {
            let (a, b) = (map(tf.center, tf.translate, p), map(c2, t2, p));
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} {b:?}");
        }
        // And M⁻¹ undoes M.
        let v = m_inv(&tf, m(&tf, [3.0, -4.0]));
        assert!((v[0] - 3.0).abs() < 1e-9 && (v[1] + 4.0).abs() < 1e-9);
    }

    /// Placing the pivot anywhere, then resetting it to Auto, leaves the
    /// rendered output as it was; the pivot shows where it was clicked.
    #[test]
    fn placing_and_resetting_the_pivot_keeps_the_picture() {
        let img = picture();
        let input = ("t".to_string(), img.clone());
        for start in [json!({ "translate": [4, -3], "rotate": 30, "scale": [1.5, 0.8] }), json!({ "translate": [4, -3], "rotate": -75, "scale": [0.7, 1.2], "center": [10, 30] })] {
            let mut d = doc_with(start);
            let before = render(&d, &img, 1);
            place_transform(&mut d, "t", 1, [50.0, 12.0], Some(&input), SIZE).unwrap();
            assert!(!d.param("t", "center").is_null());
            let tf = read(&d, "t", 1, Some(&input), SIZE);
            assert!((shown(&tf)[0] - 50.0).abs() < 1e-6 && (shown(&tf)[1] - 12.0).abs() < 1e-6, "{tf:?}");
            assert!(same(&before, &render(&d, &img, 1)) < 1e-3, "placed: {}", same(&before, &render(&d, &img, 1)));
            reset_transform(&mut d, "t", 1, Some(&input), SIZE).unwrap();
            assert!(d.param("t", "center").is_null(), "back to Auto");
            assert!(same(&before, &render(&d, &img, 1)) < 1e-3, "reset: {}", same(&before, &render(&d, &img, 1)));
        }
    }

    /// Animated center and translate get keys at the frame, and the move is
    /// one undo step; ⇧. on an animated center keys the auto spot.
    #[test]
    fn animated_params_get_keys_in_one_undo_step() {
        let img = picture();
        let input = ("t".to_string(), img.clone());
        let keys = |a: Value, b: Value| json!({ "keys": [[1, a], [10, b]], "interp": "linear" });
        let mut d = doc_with(json!({ "translate": keys(json!([0, 0]), json!([20, 0])), "center": keys(json!([30, 20]), json!([30, 30])), "rotate": 20 }));
        d.commit(false);
        let before = render(&d, &img, 5);
        place_transform(&mut d, "t", 5, [12.0, 9.0], Some(&input), SIZE).unwrap();
        d.commit(false);
        for n in ["translate", "center"] {
            let v = d.param("t", n);
            assert!(anim::is_animated(&v) && v["keys"].as_array().unwrap().iter().any(|k| k[0] == 5), "{n}: {v}");
        }
        assert!(same(&before, &render(&d, &img, 5)) < 1e-3);
        reset_transform(&mut d, "t", 5, Some(&input), SIZE).unwrap();
        assert!(anim::is_animated(&d.param("t", "center")), "an animated center is keyed, not cleared");
        assert!(same(&before, &render(&d, &img, 5)) < 1e-3);
        d.commit(false);
        d.undo();
        d.undo();
        assert!(!d.param("t", "translate")["keys"].as_array().unwrap().iter().any(|k| k[0] == 5), "two undos take both steps back");
    }

    fn period(shift: bool) -> egui::Event {
        let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        egui::Event::Key { key: Key::Period, physical_key: Some(Key::Period), pressed: true, repeat: false, modifiers }
    }

    /// Run one viewer frame over a 64 × 48 image shown at 1:1 from (0, 0).
    fn run(ctx: &egui::Context, pm: &mut PivotMode, ov: &mut Overlays, d: &mut Doc, events: Vec<egui::Event>, typing: bool) -> (bool, bool) {
        let rect = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 150.0));
        let xf = Xf::new(egui::Rect::from_min_size(Pos2::ZERO, SIZE), 1.0);
        let mut events = events;
        if !events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. })) {
            events.insert(0, egui::Event::PointerMoved(Pos2::new(30.0, 20.0)));
        }
        let raw = egui::RawInput { screen_rect: Some(rect), events, ..Default::default() };
        let (mut on, mut esc_left) = (false, false);
        let _ = ctx.run_ui(raw, |ui| {
            if typing {
                let mut s = String::new();
                let r = ui.text_edit_singleline(&mut s);
                r.request_focus();
            }
            let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            on = pm.ui(ui, &resp, &xf, SIZE, d, 1, ov);
            esc_left = ui.input(|i| i.key_pressed(Key::Escape));
        });
        (on, esc_left)
    }

    #[test]
    fn period_toggles_the_mode_but_not_while_typing() {
        let ctx = egui::Context::default();
        let (mut pm, mut ov) = (PivotMode::default(), Overlays::default());
        let mut d = doc_with(json!({}));
        run(&ctx, &mut pm, &mut ov, &mut d, vec![], false);
        assert!(run(&ctx, &mut pm, &mut ov, &mut d, vec![period(false)], false).0, ". turns it on");
        let esc = egui::Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let (on, esc_left) = run(&ctx, &mut pm, &mut ov, &mut d, vec![esc], false);
        assert!(!on && !esc_left, "Esc leaves it and is taken, so it doesn't clear the selection too");
        assert_eq!(d.selected.as_deref(), Some("t"));
        // In a text field "." is a character.
        run(&ctx, &mut pm, &mut ov, &mut d, vec![], true);
        run(&ctx, &mut pm, &mut ov, &mut d, vec![period(false)], true);
        assert!(!pm.active(), "typing a period");
        // Nothing to set: a note, and off.
        let ctx = egui::Context::default();
        d.selected = None;
        run(&ctx, &mut pm, &mut ov, &mut d, vec![], false);
        assert!(!run(&ctx, &mut pm, &mut ov, &mut d, vec![period(false)], false).0);
        assert_eq!(pm.note.as_ref().map(|n| n.0.as_str()), Some(NOTHING));
    }

    /// While on, a click places the pivot there (the picture stays); a
    /// click with it off doesn't.
    #[test]
    fn a_click_places_the_pivot() {
        let ctx = egui::Context::default();
        let (mut pm, mut ov) = (PivotMode::default(), Overlays::default());
        let mut d = doc_with(json!({ "rotate": 45, "scale": [2, 2] }));
        let click = |p: Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            ]
        };
        let release = |p: Pos2| vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }];
        run(&ctx, &mut pm, &mut ov, &mut d, vec![], false);
        assert!(run(&ctx, &mut pm, &mut ov, &mut d, vec![period(false)], false).0);
        run(&ctx, &mut pm, &mut ov, &mut d, click(Pos2::new(40.0, 10.0)), false);
        run(&ctx, &mut pm, &mut ov, &mut d, release(Pos2::new(40.0, 10.0)), false);
        let tf = read(&d, "t", 1, None, SIZE);
        assert!((shown(&tf)[0] - 40.0).abs() < 1e-4 && (shown(&tf)[1] - 10.0).abs() < 1e-4, "{tf:?}");
        assert!(!pm.dragging());
        // ⇧. puts it back to Auto.
        run(&ctx, &mut pm, &mut ov, &mut d, vec![period(true)], false);
        assert!(d.param("t", "center").is_null());
        assert!(pm.active(), "⇧. stays in the mode");
    }
}
