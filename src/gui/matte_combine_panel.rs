//! Matte Combine's Properties: a row per connected input, with its channel,
//! Invert and (from input 2) operation (see `arclight::matte_combine`).
//!
//! Inverts and operations are number lists that key as one value each,
//! always stepping: edits go through `Doc::edit_param` (a key at the frame
//! once keyed), shown as each frame holds them ([`arclight::anim::evaluate_step`]).

use crate::doc::Doc;
use crate::shuffle_routes::{committed_text, Layer};
use arclight::anim::{self, Interp};
use arclight::matte_combine::{CHANNELS, OPERATIONS};
use eframe::egui::{self, RichText};
use serde_json::{json, Value};

/// The params whose rows this panel replaces (they keep their key diamond
/// in Properties).
pub const LISTS: [&str; 2] = ["inverts", "operations"];

/// The channels input `k` offers: the five matte channels and its planes
/// (when known).
pub fn channel_choices(layers: Option<&[Layer]>) -> Vec<String> {
    CHANNELS.iter().map(|c| c.to_string()).chain(layers.into_iter().flatten().flat_map(|(_, planes)| planes.iter().cloned())).collect()
}

/// `list` with entry `k` set to `v`, padded with `pad` as needed.
pub fn set_entry(list: &[Value], k: usize, v: Value, pad: Value) -> Vec<Value> {
    let mut out = list.to_vec();
    if out.len() <= k {
        out.resize(k + 1, pad);
    }
    out[k] = v;
    out
}

/// Inverts / operations keyed with any other interpolation are set back
/// to Step (the node holds them anyway; this keeps the curve editor
/// honest). Called each frame the node's Properties show.
pub fn keep_stepping(doc: &mut Doc, id: &str) {
    for name in LISTS {
        let raw = doc.param(id, name);
        if anim::interp(&raw).is_some_and(|i| i != Interp::Step) {
            doc.set_interp(id, name, Interp::Step);
        }
    }
}

/// The rows, for Matte Combine `id` at `frame`; `layers[k]` is input k+1's
/// layers (None: unknown).
pub fn edit(ui: &mut egui::Ui, doc: &mut Doc, id: &str, frame: i64, layers: &[Option<Vec<Layer>>]) {
    let connected: Vec<bool> = doc.node(id).map_or(Vec::new(), |n| n.inputs.iter().map(|i| !i.is_empty()).collect());
    let channels: Vec<Value> = doc.param(id, "channels").as_array().cloned().unwrap_or_default();
    let at = |name: &str| -> Vec<Value> { anim::evaluate_step(&doc.param(id, name), frame as f64).as_array().cloned().unwrap_or_default() };
    let (inverts, operations) = (at("inverts"), at("operations"));
    let mut set_channels = None;
    let mut set_inverts = None;
    let mut set_operations = None;
    ui.vertical(|ui| {
        if connected.iter().all(|c| !c) {
            ui.weak("Connect mattes to combine");
        }
        for (k, on) in connected.iter().enumerate() {
            if !on {
                continue;
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}", k + 1)).strong()).on_hover_text(format!("Input {}", k + 1));
                // Channel: typed, or picked.
                let current = channels.get(k).and_then(Value::as_str).filter(|c| !c.is_empty()).unwrap_or("alpha").to_string();
                let tid = egui::Id::unique(("matte-channel", id, k));
                if let Some(t) = committed_text(ui, tid, &current, 90.0) {
                    set_channels = Some(set_entry(&channels, k, json!(t), json!("alpha")));
                }
                egui::ComboBox::from_id_salt(("matte-channel-pick", id, k)).selected_text("").width(18.0).show_ui(ui, |ui| {
                    for c in channel_choices(layers.get(k).and_then(|l| l.as_deref())) {
                        if ui.selectable_label(c == current, &c).clicked() {
                            set_channels = Some(set_entry(&channels, k, json!(c), json!("alpha")));
                        }
                    }
                });
                let mut inv = inverts.get(k).and_then(Value::as_f64).is_some_and(|v| v >= 0.5);
                if ui.checkbox(&mut inv, "Invert").changed() {
                    set_inverts = Some(set_entry(&inverts, k, json!(inv as u8 as f64), json!(0.0)));
                }
                if k > 0 {
                    let code = arclight::matte_combine::Op::from_code(operations.get(k).and_then(Value::as_f64).unwrap_or(0.0)) as usize;
                    egui::ComboBox::from_id_salt(("matte-op", id, k)).selected_text(OPERATIONS[code]).width(90.0).show_ui(ui, |ui| {
                        for (c, name) in OPERATIONS.iter().enumerate() {
                            if ui.selectable_label(c == code, *name).clicked() {
                                set_operations = Some(set_entry(&operations, k, json!(c as f64), json!(0.0)));
                            }
                        }
                    });
                }
            });
        }
    });
    if let Some(v) = set_channels {
        doc.set_param(id, "channels", json!(v));
    }
    for (name, v) in [("inverts", set_inverts), ("operations", set_operations)] {
        if let Some(v) = v {
            doc.edit_param(id, name, frame, json!(v));
        }
    }
    keep_stepping(doc, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_pad_with_defaults() {
        assert_eq!(set_entry(&[], 2, json!(1.0), json!(0.0)), vec![json!(0.0), json!(0.0), json!(1.0)]);
        assert_eq!(set_entry(&[json!("red")], 0, json!("blue"), json!("alpha")), vec![json!("blue")]);
        let l = vec![("matte".to_string(), vec!["matte.hero".to_string()])];
        assert_eq!(channel_choices(Some(&l)).last().map(String::as_str), Some("matte.hero"));
        assert_eq!(channel_choices(None).len(), 5);
    }

    /// The rows lay out for connected inputs, read keyed lists as held, and
    /// change nothing untouched.
    #[test]
    fn rows_lay_out_without_edits() {
        let mut d = Doc::blank(Default::default());
        let a = d.add_node("constant", [0.0, 0.0]);
        let id = d.add_node("matte_combine", [0.0, 100.0]);
        d.connect(&a, &id, 0);
        d.connect(&a, &id, 1);
        d.set_param(&id, "operations", json!({"keys": [[1, [0, 1]], [9, [0, 3]]], "interp": "step"}));
        let before = d.graph.clone();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| edit(ui, &mut d, &id, 5, &[None, None]));
        assert!(d.graph == before);
    }

    /// A saved project whose lists already step: showing the panel (and
    /// keep_stepping each frame) writes nothing: not modified, no new
    /// revision (no re-render), no undo step.
    #[test]
    fn showing_a_stepping_node_changes_nothing() {
        let g: arclight::graph::Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "a", "op": "constant" },
            { "id": "mc", "op": "matte_combine", "inputs": ["a", "a"], "params": {
                "operations": {"keys": [[1, [0, 1]], [9, [0, 3]]], "interp": "step"},
                "inverts": {"keys": [[1, [0, 0]], [5, [1, 0]]], "interp": "step"} } },
        ] }))
        .unwrap();
        let mut d = Doc::new(g, None);
        d.commit(false);
        let (revision, can_undo) = (d.revision, d.can_undo());
        assert!(!d.dirty);
        let ctx = egui::Context::default();
        for frame in [1, 5, 9] {
            keep_stepping(&mut d, "mc");
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| edit(ui, &mut d, "mc", frame, &[None, None]));
            d.commit(false);
        }
        assert_eq!((d.dirty, d.revision, d.can_undo()), (false, revision, can_undo));
    }

    #[test]
    fn keys_on_the_lists_always_step() {
        let mut d = Doc::blank(Default::default());
        let id = d.add_node("matte_combine", [0.0, 0.0]);
        d.add_key(&id, "operations", 1);
        assert_eq!(anim::interp(&d.param(&id, "operations")), Some(Interp::Smooth), "a new key starts smooth");
        keep_stepping(&mut d, &id);
        assert_eq!(anim::interp(&d.param(&id, "operations")), Some(Interp::Step));
        assert!(anim::interp(&d.param(&id, "inverts")).is_none(), "unkeyed lists stay as they are");
    }
}
