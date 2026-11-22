//! The Mask channel picker of a masked op: the five mask channels, then
//! the mask input's planes (as probed), and a field to type a pass by
//! name. Picking a channel sets `mask_channel` and clears `mask_plane`;
//! picking or typing a plane sets `mask_plane` (see `arclight::mask_source`).

use crate::doc::Doc;
use crate::shuffle_routes::{committed_text, Layer};
use arclight::ops::MASK_CHANNELS;
use eframe::egui;
use serde_json::{json, Value};

/// What the picker offers: the channels, then the mask's planes.
pub fn choices(layers: Option<&[Layer]>) -> (Vec<String>, Vec<String>) {
    let channels = MASK_CHANNELS.iter().map(|c| c.to_string()).collect();
    let planes = layers.into_iter().flatten().flat_map(|(_, p)| p.iter().cloned()).collect();
    (channels, planes)
}

/// The (mask_channel, mask_plane) a pick leaves: a channel clears the
/// plane, anything else is a plane (the channel kept for when it's cleared).
pub fn pick(channel: &str, picked: &str) -> (String, String) {
    if MASK_CHANNELS.contains(&picked) { (picked.to_string(), String::new()) } else { (channel.to_string(), picked.trim().to_string()) }
}

/// The picker for node `id`, with its mask input's `layers` (None:
/// unknown or no mask connected).
pub fn edit(ui: &mut egui::Ui, doc: &mut Doc, id: &str, layers: Option<&[Layer]>) {
    let channel = doc.param(id, "mask_channel").as_str().unwrap_or("alpha").to_string();
    let plane = doc.param(id, "mask_plane").as_str().unwrap_or("").trim().to_string();
    let shown = if plane.is_empty() { channel.clone() } else { plane.clone() };
    let (channels, planes) = choices(layers);
    let mut picked: Option<String> = None;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("mask-pick", id)).selected_text(&shown).width(110.0).show_ui(ui, |ui| {
            for c in &channels {
                if ui.selectable_label(plane.is_empty() && *c == channel, c).clicked() {
                    picked = Some(c.clone());
                }
            }
            if !planes.is_empty() {
                ui.separator();
                for p in &planes {
                    if ui.selectable_label(*p == plane, p).clicked() {
                        picked = Some(p.clone());
                    }
                }
            }
        });
        // Any pass by name (the mask may not be known yet).
        let tid = egui::Id::unique(("mask-plane-text", id));
        if let Some(t) = committed_text(ui, tid, &plane, 90.0) {
            picked = Some(if t.is_empty() { channel.clone() } else { t });
        }
    })
    .response
    .on_hover_text("Which of the mask's channels masks, or one of its passes (matte.hero, …): type a pass name in the field");
    if let Some(p) = picked {
        let (c, pl) = pick(&channel, &p);
        if c != channel {
            doc.set_param(id, "mask_channel", json!(c));
        }
        if pl != plane {
            doc.set_param(id, "mask_plane", Value::String(pl));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_clear_the_plane_and_planes_keep_the_channel() {
        assert_eq!(pick("red", "luminance"), ("luminance".into(), String::new()));
        assert_eq!(pick("red", " matte.hero "), ("red".into(), "matte.hero".into()));
        let l = vec![("matte".to_string(), vec!["matte.hero".to_string()])];
        let (c, p) = choices(Some(&l));
        assert_eq!((c.len(), p), (5, vec!["matte.hero".to_string()]));
        assert!(choices(None).1.is_empty());
    }

    #[test]
    fn showing_it_changes_nothing() {
        let mut d = Doc::blank(Default::default());
        let id = d.add_node("grade", [0.0, 0.0]);
        let before = d.graph.clone();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| edit(ui, &mut d, &id, None));
        assert!(d.graph == before);
    }
}
