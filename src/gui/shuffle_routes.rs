//! A Shuffle's Properties: what each input carries, and its routing table
//! (`routes`, see `arclight::shuffle_route`): any input's channel into any
//! output channel or pass.

use eframe::egui::{self, RichText};
use serde_json::{json, Value};

/// A layer's name and its planes' names (as `Image::layers` lists them).
pub type Layer = (String, Vec<String>);

const COLOR: [&str; 4] = ["R", "G", "B", "A"];
/// Pass channels offered when an input's own aren't known, and as new
/// destinations.
const PASSES: &[&str] = &["depth", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "uv.u", "uv.v", "motion.u", "motion.v", "id", "diffuse.R", "diffuse.G", "diffuse.B", "specular.R", "specular.G", "specular.B", "emission.R", "emission.G", "emission.B"];

/// The channels an input offers: R, G, B, A and its planes (when its
/// layers are known), else R, G, B, A and the usual pass names.
pub fn channels(layers: Option<&[Layer]>) -> Vec<String> {
    let color = COLOR.iter().map(|c| c.to_string());
    match layers {
        Some(l) => color.chain(l.iter().flat_map(|(_, planes)| planes.iter().cloned())).collect(),
        None => color.chain(PASSES.iter().map(|p| p.to_string())).collect(),
    }
}

/// Where a route can write: R, G, B, A, input 1's planes, then the usual
/// pass names it hasn't got.
pub fn destinations(first: Option<&[Layer]>) -> Vec<String> {
    let mut out: Vec<String> = COLOR.iter().map(|c| c.to_string()).collect();
    out.extend(first.into_iter().flatten().flat_map(|(_, planes)| planes.iter().cloned()));
    for p in PASSES {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// The routes as (source, dest) text pairs; anything malformed reads as
/// empty text (the node reports it).
pub fn rows(value: &Value) -> Vec<(String, String)> {
    let text = |v: &Value| v.as_str().unwrap_or("").to_string();
    value.as_array().map_or(Vec::new(), |a| a.iter().map(|r| (text(&r[0]), text(&r[1]))).collect())
}

fn to_value(rows: &[(String, String)]) -> Value {
    json!(rows.iter().map(|(s, d)| [s, d]).collect::<Vec<_>>())
}

/// A new route: input 2's alpha into the alpha when input 2 is connected,
/// else input 1's.
pub fn new_route(connected: &[bool]) -> (String, String) {
    let k = if connected.get(1).copied().unwrap_or(false) { 2 } else { 1 };
    (format!("{k}:A"), "A".to_string())
}

/// A text field that keeps what's typed (in egui's memory, under `id`)
/// and gives it back on Enter or when it loses focus.
pub(crate) fn committed_text(ui: &mut egui::Ui, id: egui::Id, current: &str, width: f32) -> Option<String> {
    let mut buf: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_else(|| current.to_string());
    let resp = ui.add(egui::TextEdit::singleline(&mut buf).id(id).desired_width(width));
    if resp.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf));
        None
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
        (resp.lost_focus() && buf.trim() != current).then(|| buf.trim().to_string())
    }
}

/// The Inputs listing and routing table for Shuffle `node`. `layers[k]` is
/// input k+1's layers (None: unknown), `connected[k]` whether it's wired.
/// Returns the new `routes` when edited.
pub fn edit(ui: &mut egui::Ui, node: &str, value: &Value, layers: &[Option<Vec<Layer>>], connected: &[bool]) -> Option<Value> {
    let mut rows = rows(value);
    let mut changed = false;
    ui.vertical(|ui| {
        // What each connected input carries.
        for (k, on) in connected.iter().enumerate() {
            if !on {
                continue;
            }
            let l = layers.get(k).and_then(|l| l.as_deref());
            let list = match l {
                Some(l) => channels(Some(l)).join(" "),
                None => "…".to_string(),
            };
            ui.label(RichText::new(format!("{}: {list}", k + 1)).small().weak()).on_hover_text(format!("Input {}'s channels", k + 1));
        }
        let dests = destinations(layers.first().and_then(|l| l.as_deref()));
        let mut remove = None;
        for (r, (src, dest)) in rows.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let sid = egui::Id::unique(("route-src", node, r));
                if let Some(t) = committed_text(ui, sid, src, 70.0) {
                    *src = t;
                    changed = true;
                }
                egui::ComboBox::from_id_salt(("route-src-pick", node, r)).selected_text("").width(18.0).show_ui(ui, |ui| {
                    for (k, on) in connected.iter().enumerate() {
                        if !on {
                            continue;
                        }
                        ui.label(RichText::new(format!("Input {}", k + 1)).strong());
                        for c in channels(layers.get(k).and_then(|l| l.as_deref())) {
                            let s = format!("{}:{c}", k + 1);
                            if ui.selectable_label(*src == s, &s).clicked() {
                                *src = s;
                                changed = true;
                            }
                        }
                        ui.separator();
                    }
                    for c in ["0", "1"] {
                        let s = format!("1:{c}");
                        if ui.selectable_label(*src == s, format!("{c} (constant)")).clicked() {
                            *src = s;
                            changed = true;
                        }
                    }
                });
                ui.label("→");
                let did = egui::Id::unique(("route-dest", node, r));
                if let Some(t) = committed_text(ui, did, dest, 80.0) {
                    *dest = t;
                    changed = true;
                }
                egui::ComboBox::from_id_salt(("route-dest-pick", node, r)).selected_text("").width(18.0).show_ui(ui, |ui| {
                    for d in &dests {
                        if ui.selectable_label(dest == d, d).clicked() {
                            *dest = d.clone();
                            changed = true;
                        }
                    }
                });
                if ui.small_button("✕").on_hover_text("Remove this route").clicked() {
                    remove = Some(r);
                }
            });
        }
        if let Some(r) = remove {
            rows.remove(r);
            changed = true;
        }
        let tip = "Copy another channel in: <input>:<channel> → R, G, B, A or a pass (connect more inputs, 2, 3, …, to read from them)";
        if ui.button("+ Add route").on_hover_text(tip).clicked() {
            rows.push(new_route(connected));
            changed = true;
        }
    });
    changed.then(|| to_value(&rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> Vec<Layer> {
        vec![("depth".into(), vec!["depth".into()]), ("N".into(), vec!["N.x".into(), "N.y".into(), "N.z".into()])]
    }

    #[test]
    fn inputs_offer_their_own_channels() {
        assert_eq!(channels(Some(&layers())), ["R", "G", "B", "A", "depth", "N.x", "N.y", "N.z"]);
        let unknown = channels(None);
        assert!(unknown.starts_with(&["R".into(), "G".into(), "B".into(), "A".into()]) && unknown.contains(&"specular.R".into()));
        let d = destinations(Some(&layers()));
        assert_eq!(&d[..8], &channels(Some(&layers()))[..]);
        assert_eq!(d.iter().filter(|x| *x == "depth").count(), 1, "no repeats");
    }

    #[test]
    fn rows_round_trip_and_new_routes_pick_input_2() {
        let v = json!([["2:A", "A"], ["3:depth", "depth"]]);
        assert_eq!(to_value(&rows(&v)), v);
        assert!(rows(&json!([])).is_empty() && rows(&Value::Null).is_empty());
        assert_eq!(new_route(&[true, true]), ("2:A".into(), "A".into()));
        assert_eq!(new_route(&[true]), ("1:A".into(), "A".into()));
    }

    #[test]
    fn add_route_button_adds_one() {
        let ctx = egui::Context::default();
        let mut got = None;
        for pass in 0..2 {
            let mut events = Vec::new();
            if pass == 1 {
                // Click where the button was laid out (found on the first pass).
                let at = ctx.data(|d| d.get_temp::<egui::Pos2>(egui::Id::unique("btn"))).unwrap();
                events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
                    egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
                ];
            }
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))), events, ..Default::default() };
            let _ = ctx.run_ui(raw, |ui| {
                let r = edit(ui, "sh", &json!([["2:A", "A"]]), &[Some(layers()), None], &[true, true]);
                if r.is_some() {
                    got = r;
                }
                // The button is the last widget: remember its centre.
                let rect = ui.min_rect();
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("btn"), egui::pos2(rect.left() + 30.0, rect.bottom() - 10.0)));
            });
        }
        assert_eq!(got, Some(json!([["2:A", "A"], ["2:A", "A"]])));
    }
}
