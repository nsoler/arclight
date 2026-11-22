//! Where the viewed image shows the selected node's own pixel space, for
//! its overlay handles.
//!
//! With a Viewer node, selecting a node leaves the viewer on the Viewer's
//! input, which can be downstream of the selected node past ops that move
//! pixels. [`space`] follows every path from the selected node to the
//! viewed one: Transforms, Crops, Resizes and match-moving Trackers on the
//! way compose into one affine map (the handles follow it, drags map back
//! through it); a 3D render, a Corner Pin or a Lens Distort can't be
//! followed, and the overlay then says so (press V to view the node).

use crate::doc::Doc;
use arclight::image::{IRect, Size};
use arclight::ops::{self, Category, Kind, Overlay};
use arclight::track::{self, Affine};
use std::collections::{HashMap, HashSet};

/// The frame and data window of a node's main input, by node id: what a
/// Transform's auto pivot, a shrunk Crop and a Resize need. Asked of the
/// worker for the nodes [`wanted`] lists.
pub type Shapes = HashMap<String, (Size, IRect)>;

/// How the viewed image shows the selected node's pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Space {
    /// As they are (or the node isn't upstream of the view at all).
    Same,
    /// Moved by this map (node pixels → viewed pixels).
    Affine(Affine),
    /// Past `node`, which moves them in a way the handles can't follow.
    Other { node: String, why: String },
}

pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// `a` after `b`.
pub fn compose(a: &Affine, b: &Affine) -> Affine {
    [
        a[0] * b[0] + a[1] * b[3],
        a[0] * b[1] + a[1] * b[4],
        a[0] * b[2] + a[1] * b[5] + a[2],
        a[3] * b[0] + a[4] * b[3],
        a[3] * b[1] + a[4] * b[4],
        a[3] * b[2] + a[4] * b[5] + a[5],
    ]
}

fn near(a: &Affine, b: &Affine) -> bool {
    // Matrix terms to 1e-4, offsets to a hundredth of a pixel.
    [0, 1, 3, 4].iter().all(|&i| (a[i] - b[i]).abs() < 1e-4) && [2, 5].iter().all(|&i| (a[i] - b[i]).abs() < 0.01)
}

/// Whether `op` shows handles in the 2D viewer.
fn has_handles(op: &str) -> bool {
    op == "particle_emitter" || ops::info(op).is_some_and(|i| !matches!(i.overlay, Overlay::None | Overlay::CameraTracker))
}

/// Whether [`space`] follows `selected` at all: a 2D node with handles,
/// other than the one viewed, upstream of it.
fn followed(doc: &Doc, selected: &str, viewed: &str) -> bool {
    selected != viewed && doc.node(selected).is_some_and(|n| ops::output_kind(&n.op) == Kind::Image && has_handles(&n.op)) && doc.is_upstream(selected, viewed)
}

/// The nodes strictly downstream of `from` and up to `to` (included),
/// inputs before the nodes they feed.
fn between(doc: &Doc, from: &str, to: &str) -> Vec<String> {
    fn visit(doc: &Doc, from: &str, id: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        if id == from || !seen.insert(id.to_string()) || !doc.is_upstream(from, id) {
            return;
        }
        if let Some(n) = doc.node(id) {
            for i in n.inputs.iter().filter(|i| !i.is_empty()) {
                visit(doc, from, i, seen, out);
            }
        }
        out.push(id.to_string());
    }
    let mut out = Vec::new();
    visit(doc, from, to, &mut HashSet::new(), &mut out);
    out
}

/// Ops [`space`] needs the input's frame and data window for.
const SHAPED: [&str; 3] = ["transform", "crop", "resize"];

/// The nodes (and their main inputs) whose input shapes [`space`] needs
/// for `selected` viewed through `viewed`: the Transforms, Crops and
/// Resizes between them. Empty (nothing to ask the worker) unless a node
/// with handles is selected, isn't the one viewed, and one of those lies
/// between.
pub fn wanted(doc: &Doc, selected: &str, viewed: &str) -> Vec<(String, String)> {
    if !followed(doc, selected, viewed) {
        return Vec::new();
    }
    between(doc, selected, viewed)
        .into_iter()
        .filter_map(|id| {
            let n = doc.node(&id)?;
            let input = n.inputs.first().filter(|i| !i.is_empty())?;
            (SHAPED.contains(&n.op.as_str()) && !n.disabled).then(|| (id.clone(), input.clone()))
        })
        .collect()
}

/// How node `id` shows what reaches its input `port`: a map, or why it
/// can't be followed.
fn step(doc: &Doc, id: &str, port: usize, frame: i64, shapes: &Shapes) -> Result<Affine, String> {
    let Some(n) = doc.node(id) else { return Ok(IDENTITY) };
    let info = ops::info(&n.op);
    let label = info.map_or(n.op.as_str(), |i| i.label);
    if n.disabled {
        return Ok(IDENTITY);
    }
    if ops::output_kind(&n.op) == Kind::Scene {
        return Err(format!("{id} takes it into a 3D scene"));
    }
    if info.is_some_and(|i| i.category == Category::Scene) {
        return Err(format!("{id} ({label}) renders it in 3D"));
    }
    // A warp's other inputs (its mask) aren't moved.
    if port != 0 {
        return Ok(IDENTITY);
    }
    match n.op.as_str() {
        op if SHAPED.contains(&op) => {
            let (format, window) = shapes.get(id).copied().unwrap_or((doc.graph.project.format, IRect::default()));
            let params = ops::resolve_params(op, &n.params, frame);
            match ops::transform_affine(op, &params, format, window) {
                Ok(Some(m)) => Ok(m),
                Ok(None) => Ok(IDENTITY),
                Err(e) => Err(format!("{id} ({label}) has invalid params: {e}")),
            }
        }
        "tracker" => Ok(track::output_map(&n.params, frame).unwrap_or(IDENTITY)),
        "corner_pin" | "lens_distort" => Err(format!("{id} ({label}) warps it")),
        _ => Ok(IDENTITY),
    }
}

/// How the image of `viewed` shows `selected`'s pixels at `frame`
/// (`shapes`: the input shapes [`wanted`] asked for; a missing one
/// counts as the project's frame, empty).
pub fn space(doc: &Doc, selected: &str, viewed: &str, frame: i64, shapes: &Shapes) -> Space {
    if !followed(doc, selected, viewed) {
        return Space::Same;
    }
    let mut maps: HashMap<String, Affine> = HashMap::new();
    for id in between(doc, selected, viewed) {
        let Some(n) = doc.node(&id) else { continue };
        let mut here: Option<Affine> = None;
        for (port, src) in n.inputs.iter().enumerate() {
            let before = if src == selected { IDENTITY } else if let Some(m) = maps.get(src) { *m } else { continue };
            let m = match step(doc, &id, port, frame, shapes) {
                Ok(m) => compose(&m, &before),
                Err(why) => return Space::Other { node: id.clone(), why },
            };
            match &here {
                Some(h) if !near(h, &m) => return Space::Other { node: id.clone(), why: format!("{id} shows it in more than one place") },
                _ => here = Some(m),
            }
        }
        if let Some(m) = here {
            maps.insert(id, m);
        }
    }
    match maps.get(viewed) {
        Some(m) if !near(m, &IDENTITY) => Space::Affine(*m),
        _ => Space::Same,
    }
}

/// What the viewer says while the handles can't follow: the node, what
/// it's seen through and why, and how to see its own space.
pub fn notice(selected: &str, viewed: &str, why: &str) -> String {
    format!("{selected}'s handles are in its own space, and the viewer shows {viewed}: {why}. Press V to view {selected}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;
    use serde_json::json;

    fn doc(nodes: serde_json::Value) -> Doc {
        let graph: Graph = serde_json::from_value(json!({ "nodes": nodes })).unwrap();
        Doc::new(graph, None)
    }

    #[test]
    fn a_node_viewed_itself_or_through_color_ops_is_in_its_own_space() {
        let d = doc(json!([
            { "id": "e", "op": "ellipse", "inputs": [""], "params": { "center": [100, 50], "size": [40, 20] } },
            { "id": "g", "op": "grade", "inputs": ["e"] },
            { "id": "b", "op": "blur", "inputs": ["g"] },
            { "id": "plate", "op": "constant" },
            { "id": "m", "op": "merge", "inputs": ["b", "plate"] },
            { "id": "v", "op": "viewer", "inputs": ["m"] },
        ]));
        let none = Shapes::new();
        assert_eq!(space(&d, "e", "e", 1, &none), Space::Same);
        assert_eq!(space(&d, "e", "m", 1, &none), Space::Same);
        // Not upstream of what's viewed (a shape being drawn over a plate).
        assert_eq!(space(&d, "e", "plate", 1, &none), Space::Same);
        assert!(wanted(&d, "e", "m").is_empty(), "nothing to ask the worker");
        assert!(wanted(&d, "e", "e").is_empty());
    }

    #[test]
    fn transforms_downstream_compose_and_are_asked_for_their_input() {
        let d = doc(json!([
            { "id": "r", "op": "rectangle", "inputs": [""], "params": { "center": [100, 50], "size": [40, 20] } },
            { "id": "t1", "op": "transform", "inputs": ["r"], "params": { "translate": [10, 0], "center": [0, 0], "scale": [2, 2] } },
            { "id": "g", "op": "grade", "inputs": ["t1"] },
            { "id": "t2", "op": "transform", "inputs": ["g"], "params": { "translate": [0, 5] } },
        ]));
        assert_eq!(wanted(&d, "r", "t2"), vec![("t1".to_string(), "r".to_string()), ("t2".to_string(), "g".to_string())]);
        // t2 pivots on its input's data window (scale 1, rotate 0: no matter).
        let shapes: Shapes = [("t1".into(), (Size { width: 200, height: 100 }, IRect::new(80, 40, 120, 60))), ("t2".into(), (Size { width: 200, height: 100 }, IRect::new(170, 80, 250, 120)))].into();
        let Space::Affine(m) = space(&d, "r", "t2", 1, &shapes) else { panic!() };
        assert_eq!(track::apply(&m, [100.0, 50.0]), [210.0, 105.0]);
        // The Transform itself selected: viewed downstream through t2 only.
        let Space::Affine(m) = space(&d, "t1", "t2", 1, &shapes) else { panic!() };
        assert_eq!(track::apply(&m, [3.0, 4.0]), [3.0, 9.0]);
    }

    #[test]
    fn a_3d_render_or_a_warp_in_between_cannot_be_followed() {
        let d = doc(json!([
            { "id": "t", "op": "transform", "inputs": [""], "params": { "translate": [5, 0] } },
            { "id": "card", "op": "card", "inputs": ["t"] },
            { "id": "scene", "op": "scene", "inputs": ["card"] },
            { "id": "cam", "op": "camera" },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam"] },
            { "id": "pin", "op": "corner_pin", "inputs": ["t"] },
            { "id": "v", "op": "viewer", "inputs": ["render"] },
        ]));
        let none = Shapes::new();
        assert_eq!(space(&d, "t", "render", 1, &none), Space::Other { node: "card".into(), why: "card takes it into a 3D scene".into() });
        assert_eq!(space(&d, "t", "pin", 1, &none), Space::Other { node: "pin".into(), why: "pin (Corner Pin) warps it".into() });
        assert!(wanted(&d, "t", "render").is_empty(), "no Transform between: nothing asked");
        let text = notice("t", "render", "card takes it into a 3D scene");
        assert!(text.contains("t's handles") && text.contains("render") && text.contains("Press V to view t"), "{text}");
    }

    #[test]
    fn two_paths_moving_it_differently_cannot_be_followed_alike_ones_can() {
        let d = doc(json!([
            { "id": "r", "op": "rectangle", "inputs": [""] },
            { "id": "a", "op": "transform", "inputs": ["r"], "params": { "translate": [5, 0], "center": [0, 0] } },
            { "id": "b", "op": "transform", "inputs": ["r"], "params": { "translate": [5, 0], "center": [0, 0] } },
            { "id": "c", "op": "transform", "inputs": ["r"], "params": { "translate": [0, 9], "center": [0, 0] } },
            { "id": "same", "op": "merge", "inputs": ["a", "b"] },
            { "id": "differ", "op": "merge", "inputs": ["a", "c"] },
            // A mask input isn't moved, and a turned-off Transform passes it on.
            { "id": "off", "op": "transform", "inputs": ["r"], "params": { "translate": [50, 0] }, "disabled": true },
        ]));
        let none = Shapes::new();
        assert_eq!(space(&d, "r", "same", 1, &none), Space::Affine([1.0, 0.0, 5.0, 0.0, 1.0, 0.0]));
        assert!(matches!(space(&d, "r", "differ", 1, &none), Space::Other { node, .. } if node == "differ"));
        assert_eq!(space(&d, "r", "off", 1, &none), Space::Same);
    }
}
