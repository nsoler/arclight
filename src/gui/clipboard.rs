//! Copy, cut, paste and duplicate of nodes and backdrops (⌘C ⌘X ⌘V ⌘D):
//! the selection goes on the system clipboard as a project of its own
//! ([`templates::excerpt`]: its file paths whole, so a paste into another
//! project, or another running app, finds the files), and a paste merges
//! it in ([`Doc::import`]: clashing names renamed, one undo step, the new
//! nodes selected). Text on the clipboard that isn't ours is left alone.

use crate::doc::Doc;
use arclight::graph::Graph;
use arclight::graph_merge::{Merge, Place};
use arclight::templates;

/// How far a paste with the pointer elsewhere, or a duplicate, lands from
/// the originals (graph units).
pub const OFFSET: [f32; 2] = [40.0, 40.0];

/// The selection (its nodes, or the selected backdrop and those in it) as
/// clipboard text; None with nothing selected.
pub fn copy(doc: &Doc) -> Option<String> {
    excerpt(doc).map(|g| g.to_json())
}

/// [`copy`], then the selection deleted (one undo step with it: both land
/// in the same frame's commit).
pub fn cut(doc: &mut Doc) -> Option<String> {
    let text = copy(doc)?;
    doc.delete_selection();
    Some(text)
}

/// `text` merged into `doc` with its top-left at `at` (graph units; None:
/// [`OFFSET`] from where it was copied), the new nodes selected; false
/// (nothing done) if the text isn't ours.
pub fn paste(doc: &mut Doc, text: &str, at: Option<[f32; 2]>) -> bool {
    let Some(g) = templates::parse(text) else { return false };
    put(doc, &g, at);
    true
}

/// A copy of the selection [`OFFSET`] from it, selected (the clipboard
/// untouched); false with nothing selected.
pub fn duplicate(doc: &mut Doc) -> bool {
    let Some(g) = excerpt(doc) else { return false };
    put(doc, &g, None);
    true
}

/// `g` merged into `doc`: its top-left at `at`, or [`OFFSET`] from where
/// it is.
pub fn put(doc: &mut Doc, g: &Graph, at: Option<[f32; 2]>) {
    let at = at.unwrap_or_else(|| {
        let lo = top_left(g);
        [lo[0] + OFFSET[0], lo[1] + OFFSET[1]]
    });
    doc.import(g, &Merge { only: None, place: Place::At(at) });
}

fn excerpt(doc: &Doc) -> Option<Graph> {
    let nodes = doc.selection();
    let backdrops: Vec<String> = doc.selected_backdrop.iter().cloned().collect();
    if nodes.is_empty() && backdrops.is_empty() {
        return None;
    }
    Some(templates::excerpt(&doc.graph, doc.path.as_deref(), &nodes, &backdrops))
}

/// The top-left of `g`'s nodes and backdrops (where `Place::At` puts it).
fn top_left(g: &Graph) -> [f32; 2] {
    let corners = g.nodes.iter().filter_map(|n| n.pos).chain(g.backdrops.iter().map(|b| b.pos));
    corners.fold(None, |lo: Option<[f32; 2]>, p| Some(lo.map_or(p, |l| [l[0].min(p[0]), l[1].min(p[1])]))).unwrap_or([0.0, 0.0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn doc() -> Doc {
        let g = Graph::from_str(
            &serde_json::json!({
                "nodes": [
                    { "id": "read", "op": "read", "params": { "path": "/plates/a.exr" }, "pos": [0, 0] },
                    { "id": "grade", "op": "grade", "inputs": ["read"], "params": { "gain": { "link": { "node": "read", "param": "x" } } }, "pos": [0, 100] },
                    { "id": "blur", "op": "blur", "inputs": ["grade"], "pos": [0, 200] }
                ],
                "backdrops": [{ "id": "backdrop1", "title": "look", "pos": [-20, 60], "size": [200, 220] }]
            })
            .to_string(),
        )
        .unwrap();
        let mut d = Doc::new(g, None);
        d.commit(false);
        d
    }

    fn pos(d: &Doc, id: &str) -> [f32; 2] {
        d.node(id).unwrap().pos.unwrap()
    }

    /// Copy then paste: new names, wired among themselves, at the pointer,
    /// selected, one undo step; again without a pointer: off the originals.
    #[test]
    fn copy_and_paste() {
        let mut d = doc();
        assert_eq!(copy(&d), None, "nothing selected");
        d.select_nodes(&["grade".into(), "blur".into()], false);
        let text = copy(&d).unwrap();
        assert!(paste(&mut d, &text, Some([500.0, 50.0])));
        assert_eq!(d.selection().len(), 2);
        assert!(d.selection().iter().all(|id| id != "grade" && id != "blur"), "{:?}", d.selection());
        let (g2, b2) = (d.selection()[0].clone(), d.selection()[1].clone());
        // (Their backdrop holds just them, so it comes too: its corner,
        // 20 left of and 40 above grade, at the pointer.)
        assert_eq!(d.graph.backdrops.len(), 2);
        assert_eq!((pos(&d, &g2), pos(&d, &b2)), ([520.0, 90.0], [520.0, 190.0]));
        assert_eq!(d.node(&b2).unwrap().inputs[0], g2, "wired among themselves");
        assert_eq!(d.node(&g2).unwrap().inputs[0], "", "read wasn't copied");
        assert_eq!(d.graph.nodes.len(), 5);
        d.commit(false);
        d.undo();
        assert_eq!(d.graph.nodes.len(), 3, "one undo step");
        assert!(paste(&mut d, &text, None));
        assert_eq!(pos(&d, &d.selection()[0]), [40.0, 140.0], "off where they were copied");
        assert_eq!(d.graph.backdrops[1].pos, [20.0, 100.0]);
        // Not ours: nothing.
        let before = d.graph.clone();
        for other in ["hello", "{\"x\": 1}", ""] {
            assert!(!paste(&mut d, other, Some([0.0, 0.0])));
        }
        assert_eq!(d.graph, before);
    }

    /// Cut: copied and gone, one undo step; a backdrop copies with its
    /// nodes; duplicate leaves the clipboard alone and lands off the
    /// originals.
    #[test]
    fn cut_backdrops_and_duplicate() {
        let mut d = doc();
        d.select_nodes(&["blur".into()], false);
        let text = cut(&mut d).unwrap();
        assert!(d.node("blur").is_none());
        d.commit(false);
        d.undo();
        assert!(d.node("blur").is_some(), "one undo step");
        assert!(paste(&mut d, &text, Some([300.0, 0.0])));
        assert_eq!(d.selection(), ["blur1"]);
        // The backdrop brings grade and blur, and comes too.
        let mut d = doc();
        d.select_backdrop("backdrop1");
        let text = copy(&d).unwrap();
        assert!(paste(&mut d, &text, Some([400.0, 0.0])));
        assert_eq!(d.graph.backdrops.len(), 2);
        assert_eq!(d.selection().len(), 2);
        let bd = &d.graph.backdrops[1];
        assert_eq!(bd.pos, [400.0, 0.0], "its top-left at the pointer");
        assert!(d.selection().iter().all(|id| {
            let (a, z) = d.node(id).unwrap().bounds();
            bd.contains(a, z)
        }));
        // Duplicate.
        let mut d = doc();
        assert!(!duplicate(&mut d), "nothing selected");
        d.select_nodes(&["read".into()], false);
        assert!(duplicate(&mut d));
        assert_eq!(d.selection(), ["read1"]);
        assert_eq!(pos(&d, "read1"), [40.0, 40.0]);
        assert_eq!(d.node("read1").unwrap().params["path"], "/plates/a.exr");
    }
}
