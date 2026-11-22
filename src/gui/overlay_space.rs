//! Where the viewed image shows the selected node's own pixel space, for
//! its overlay handles.
//!
//! With a Viewer node, selecting a node leaves the viewer on the Viewer's
//! input, which can be downstream of the selected node past ops that move
//! pixels. [`space`] follows every path from the selected node to the
//! viewed one: Transforms, Crops, Resizes and match-moving Trackers on the
//! way compose into one map, and so does a Card the image goes onto in a
//! Scanline Render (a projective map through its camera, see `card_map`):
//! the handles follow it, drags map back through it. A Corner Pin, a Lens
//! Distort, another mesh's texture, a card behind the camera or edge-on
//! can't be followed, and the overlay then says so (press V to view the
//! node).

use crate::doc::Doc;
use arclight::image::{IRect, Size};
use arclight::ops::{self, Category, Kind, Overlay};
use crate::card_map::{CardView, Proj};
use arclight::track::{self, Affine};
use std::collections::{HashMap, HashSet};

/// The frame and data window of a node's main input, by node id: what a
/// Transform's auto pivot, a shrunk Crop and a Resize need. Asked of the
/// worker for the nodes [`wanted`] lists.
pub type Shapes = HashMap<String, (Size, IRect)>;

/// The chosen card's view of each (Card, Scanline Render) pair between
/// the selected node and the viewed one, or why it can't be mapped. Asked
/// of the worker for the pairs [`wanted_cards`] lists.
pub type Cards = HashMap<(String, String), Result<CardView, String>>;

/// How the viewed image shows the selected node's pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Space {
    /// As they are (or the node isn't upstream of the view at all).
    Same,
    /// Moved by this map (node pixels → viewed pixels): affine through 2D
    /// ops, projective through a card in a render. `on`: the card the
    /// handles are on, when there were several to choose from.
    Map { proj: Proj, on: Option<String> },
    /// Past `node`, which moves them in a way the handles can't follow.
    Other { node: String, why: String },
}

pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

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

/// The Scanline Renders a Card's image goes into on the way to `viewed`.
fn renders_after(doc: &Doc, card: &str, between: &[String]) -> Vec<String> {
    between.iter().filter(|r| doc.node(r).is_some_and(|n| n.op == "scanline_render" && !n.disabled) && doc.is_upstream(card, r)).cloned().collect()
}

/// The (Card, Scanline Render) pairs whose views [`space`] needs: each
/// card between `selected` and `viewed` taking it as its image, and the
/// render after it. Empty (nothing to ask the worker) unless a node with
/// handles is selected, isn't the one viewed, and a Card lies between.
pub fn wanted_cards(doc: &Doc, selected: &str, viewed: &str) -> Vec<(String, String)> {
    if !followed(doc, selected, viewed) {
        return Vec::new();
    }
    let between = between(doc, selected, viewed);
    between
        .iter()
        .filter(|id| doc.node(id).is_some_and(|n| n.op == "card" && !n.disabled))
        .flat_map(|card| renders_after(doc, card, &between).into_iter().map(move |r| (card.clone(), r)))
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

/// Where the selected node's pixels have got to at a node: the map so
/// far; while inside a 3D scene, the render they're headed for; and the
/// card they went through with its on-screen area (to choose among
/// cards) and whether it was chosen among several.
#[derive(Clone)]
struct Reach {
    proj: Proj,
    into: Option<String>,
    card: Option<(String, f64, bool)>,
}

fn same_map(a: &Proj, b: &Proj) -> bool {
    // Scaled alike, then matrix terms to 1e-4 and offsets to 0.01 px.
    let (sa, sb) = (a.m[8], b.m[8]);
    if sa.abs() < 1e-12 || sb.abs() < 1e-12 {
        return false;
    }
    (0..9).all(|i| {
        let (x, y) = (a.m[i] / sa, b.m[i] / sb);
        let tol = if i == 2 || i == 5 { 0.01 } else { 1e-4 };
        (x - y).abs() < tol
    })
}

/// What [`advance`] goes by: the frame, the worker's answers, and the
/// nodes between the selected one and the viewed one.
struct Walk<'a> {
    frame: i64,
    shapes: &'a Shapes,
    cards: &'a Cards,
    between: &'a [String],
}

/// Through node `id`'s input `port`, from `before`: where they get to, or
/// why the handles can't follow.
fn advance(doc: &Doc, id: &str, port: usize, before: &Reach, walk: &Walk) -> Result<Reach, String> {
    let (frame, shapes, cards, between) = (walk.frame, walk.shapes, walk.cards, walk.between);
    let n = doc.node(id).ok_or_else(String::new)?;
    // Inside a scene, headed for render `r`: scene nodes carry them (their
    // effect is in the card's place in the scene); the render takes them
    // on any input but its background.
    if let Some(r) = &before.into {
        if id == r && port != 2 {
            return Ok(Reach { into: None, ..before.clone() });
        }
        if ops::output_kind(&n.op) == Kind::Scene {
            return Ok(before.clone());
        }
        return Err(format!("{id} takes the 3D scene somewhere the handles can't follow"));
    }
    if n.op == "card" && port == 0 && !n.disabled {
        let renders = renders_after(doc, id, between);
        let [r] = renders.as_slice() else {
            return Err(if renders.is_empty() { format!("{id} takes it into a 3D scene") } else { format!("{id} goes into more than one render") });
        };
        return match cards.get(&(id.to_string(), r.clone())) {
            Some(Ok(v)) => Ok(Reach { proj: before.proj.then(&v.proj), into: Some(r.clone()), card: Some((id.to_string(), v.area, v.among > 1)) }),
            Some(Err(why)) => Err(why.clone()),
            None => Err(format!("{id}'s place in {r} isn't known yet")),
        };
    }
    let m = step(doc, id, port, frame, shapes)?;
    Ok(Reach { proj: before.proj.then(&Proj::affine(&m)), ..before.clone() })
}

/// How the image of `viewed` shows `selected`'s pixels at `frame`
/// (`shapes`: the input shapes [`wanted`] asked for, a missing one
/// counting as the project's frame, empty; `cards`: the card views
/// [`wanted_cards`] asked for). Of paths through different cards, the
/// card largest on screen wins.
pub fn space(doc: &Doc, selected: &str, viewed: &str, frame: i64, shapes: &Shapes, cards: &Cards) -> Space {
    if !followed(doc, selected, viewed) {
        return Space::Same;
    }
    let between = between(doc, selected, viewed);
    let start = Reach { proj: Proj::IDENTITY, into: None, card: None };
    let walk = Walk { frame, shapes, cards, between: &between };
    let mut reached: HashMap<String, Reach> = HashMap::new();
    for id in &between {
        let Some(n) = doc.node(id) else { continue };
        let mut here: Option<Reach> = None;
        for (port, src) in n.inputs.iter().enumerate() {
            let before = if src == selected { &start } else if let Some(r) = reached.get(src) { r } else { continue };
            let r = match advance(doc, id, port, before, &walk) {
                Ok(r) => r,
                Err(why) => return Space::Other { node: id.clone(), why },
            };
            here = match here {
                None => Some(r),
                Some(h) if same_map(&h.proj, &r.proj) && h.into == r.into => Some(h),
                // Through different cards: the bigger one on screen.
                Some(h) => match (&h.card, &r.card) {
                    (Some(a), Some(b)) if h.into == r.into => {
                        let (keep, other) = if b.1 > a.1 { (r, a.0.clone()) } else { (h, b.0.clone()) };
                        let mut keep = keep;
                        if let Some(c) = keep.card.as_mut().filter(|c| c.0 != other) {
                            c.2 = true;
                        }
                        Some(keep)
                    }
                    _ => return Space::Other { node: id.clone(), why: format!("{id} shows it in more than one place") },
                },
            };
        }
        if let Some(r) = here {
            reached.insert(id.clone(), r);
        }
    }
    match reached.get(viewed) {
        Some(r) if r.into.is_some() => Space::Other { node: viewed.to_string(), why: format!("{viewed} is a 3D scene") },
        Some(r) if !same_map(&r.proj, &Proj::IDENTITY) => Space::Map { proj: r.proj, on: r.card.as_ref().filter(|c| c.2).map(|c| c.0.clone()) },
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
        assert_eq!(space(&d, "e", "e", 1, &none, &Cards::new()), Space::Same);
        assert_eq!(space(&d, "e", "m", 1, &none, &Cards::new()), Space::Same);
        // Not upstream of what's viewed (a shape being drawn over a plate).
        assert_eq!(space(&d, "e", "plate", 1, &none, &Cards::new()), Space::Same);
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
        let Space::Map { proj, on: None } = space(&d, "r", "t2", 1, &shapes, &Cards::new()) else { panic!() };
        assert_eq!(proj.apply([100.0, 50.0]), Some([210.0, 105.0]));
        // The Transform itself selected: viewed downstream through t2 only.
        let Space::Map { proj, .. } = space(&d, "t1", "t2", 1, &shapes, &Cards::new()) else { panic!() };
        assert_eq!(proj.apply([3.0, 4.0]), Some([3.0, 9.0]));
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
        assert_eq!(space(&d, "t", "render", 1, &none, &Cards::new()), Space::Other { node: "card".into(), why: "card's place in render isn't known yet".into() });
        assert_eq!(space(&d, "t", "pin", 1, &none, &Cards::new()), Space::Other { node: "pin".into(), why: "pin (Corner Pin) warps it".into() });
        assert!(wanted(&d, "t", "render").is_empty(), "no Transform between: nothing asked");
        assert_eq!(wanted_cards(&d, "t", "render"), vec![("card".to_string(), "render".to_string())]);
        assert!(wanted_cards(&d, "t", "pin").is_empty());
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
        assert_eq!(space(&d, "r", "same", 1, &none, &Cards::new()), Space::Map { proj: Proj::affine(&[1.0, 0.0, 5.0, 0.0, 1.0, 0.0]), on: None });
        assert!(matches!(space(&d, "r", "differ", 1, &none, &Cards::new()), Space::Other { node, .. } if node == "differ"));
        assert_eq!(space(&d, "r", "off", 1, &none, &Cards::new()), Space::Same);
    }

    fn view(proj: Proj, area: f64, among: usize) -> Result<CardView, String> {
        Ok(CardView { proj, area, among })
    }

    /// Through a card in a render, then a Transform after it: the card's
    /// map, then the Transform's. A card that can't be mapped says why; of
    /// two cards showing it, the bigger on screen wins and is named.
    #[test]
    fn a_card_in_a_render_composes_with_what_is_around_it() {
        let d = doc(json!([
            { "id": "r", "op": "rectangle", "inputs": [""] },
            { "id": "t", "op": "transform", "inputs": ["r"], "params": { "translate": [10, 0], "center": [0, 0] } },
            { "id": "c1", "op": "card", "inputs": ["t"] },
            { "id": "c2", "op": "card", "inputs": ["t"] },
            { "id": "move", "op": "transform_geo", "inputs": ["c1"] },
            { "id": "scene", "op": "scene", "inputs": ["move", "c2"] },
            { "id": "cam", "op": "camera" },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam", ""] },
            { "id": "after", "op": "transform", "inputs": ["render"], "params": { "scale": [2, 2], "center": [0, 0] } },
        ]));
        let h = Proj { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.001, 0.0, 1.0], near: 0.1 };
        let small = Proj::affine(&[0.5, 0.0, 0.0, 0.0, 0.5, 0.0]);
        let cards: Cards = [(("c1".into(), "render".into()), view(h, 5000.0, 1)), (("c2".into(), "render".into()), view(small, 100.0, 1))].into();
        let Space::Map { proj, on } = space(&d, "r", "after", 1, &Shapes::new(), &cards) else { panic!() };
        assert_eq!(on.as_deref(), Some("c1"), "chosen among two cards: named");
        // (40, 20) → +10 → (50, 20) → through h: /1.05 → ×2.
        let got = proj.apply([40.0, 20.0]).unwrap();
        let w = 1.0 + 0.001 * 50.0;
        assert!((got[0] - 2.0 * 50.0 / w).abs() < 1e-9 && (got[1] - 2.0 * 20.0 / w).abs() < 1e-9, "{got:?}");
        let back = proj.unapply(got).unwrap();
        assert!((back[0] - 40.0).abs() < 1e-9 && (back[1] - 20.0).abs() < 1e-9);
        // One card only, chosen among instances: named too; plain: not.
        let d1 = doc(json!([
            { "id": "r", "op": "rectangle", "inputs": [""] },
            { "id": "c1", "op": "card", "inputs": ["r"] },
            { "id": "scene", "op": "scene", "inputs": ["c1"] },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "", ""] },
        ]));
        let one = |among| -> Cards { [(("c1".into(), "render".into()), view(h, 10.0, among))].into() };
        assert!(matches!(space(&d1, "r", "render", 1, &Shapes::new(), &one(1)), Space::Map { on: None, .. }));
        assert!(matches!(space(&d1, "r", "render", 1, &Shapes::new(), &one(3)), Space::Map { on: Some(c), .. } if c == "c1"));
        let behind: Cards = [(("c1".into(), "render".into()), Err("c1 reaches behind the camera".into()))].into();
        assert_eq!(space(&d1, "r", "render", 1, &Shapes::new(), &behind), Space::Other { node: "c1".into(), why: "c1 reaches behind the camera".into() });
        // Viewing the scene itself (not a render): can't be shown in 2D.
        assert!(matches!(space(&d1, "r", "scene", 1, &Shapes::new(), &one(1)), Space::Other { .. }));
    }
}
