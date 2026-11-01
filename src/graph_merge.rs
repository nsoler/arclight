//! Merging one graph into another: File ▸ Import project in the GUI, and
//! copy / paste. A [`merge`] takes a source graph's nodes and backdrops
//! (all, or a chosen set) into a target graph, beside its work or at a
//! given place. Ids that clash get new ones (grade1 → grade2) and every
//! reference to a renamed node follows ([`rename_refs`]); wires from nodes
//! not taken are dropped. The target keeps its own project (format, fps,
//! range, folder) and view.
//!
//! Paths: a graph [`read`] for merging has its file paths absolute, so
//! the merged Reads find the same files; saving the target makes those
//! inside its folder relative to it again (see [`Graph::to_json_at`]).

use crate::graph::{Backdrop, Graph, NodeSpec};
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Where the merged nodes go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// To the right of the target's nodes and backdrops, tops aligned
    /// (where they were, into an empty graph).
    Beside,
    /// The merged set's top-left node position here (a paste at the
    /// pointer).
    At([f32; 2]),
}

/// What to merge, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Merge {
    /// The ids of the source's nodes and backdrops to take; None: all.
    pub only: Option<Vec<String>>,
    pub place: Place,
}

/// What a merge added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Merged {
    /// The new nodes' ids (as in the target), in the source's order.
    pub nodes: Vec<String>,
    /// The new backdrops' ids.
    pub backdrops: Vec<String>,
    /// Source ids that clashed, and what they became.
    pub renamed: BTreeMap<String, String>,
}

/// Gap between the target's work and what's put beside it (graph units).
const GAP: f32 = 160.0;

/// A node's width as the node editor draws it (for the target's right
/// edge).
const NODE_W: f32 = 110.0;

/// A project file read for merging: as [`Graph::from_file`], with any file
/// path still relative (a project without a folder) made whole against
/// its file's folder, so it means the same file in the target.
pub fn read(path: &Path) -> Result<Graph> {
    let mut g = Graph::from_file(path)?;
    let project = g.project.clone();
    for n in &mut g.nodes {
        if let Some(map) = n.params.as_object_mut() {
            project.resolve_paths(Some(path), &n.op, map);
        }
    }
    Ok(g)
}

/// Merge `source`'s nodes and backdrops (those `how.only` names, or all)
/// into `target`, placed as `how.place` says.
pub fn merge(target: &mut Graph, source: &Graph, how: &Merge) -> Merged {
    let take = |id: &str| how.only.as_ref().is_none_or(|ids| ids.iter().any(|i| i == id));
    let nodes: Vec<&NodeSpec> = source.nodes.iter().filter(|n| take(&n.id)).collect();
    let backdrops: Vec<&Backdrop> = source.backdrops.iter().filter(|b| take(&b.id)).collect();
    let taken: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    // New ids for those that clash: never one in the target, in the taken
    // set, or given already (so renames can't chain).
    let mut used: HashSet<String> = target.nodes.iter().map(|n| n.id.clone()).chain(taken.iter().map(|s| s.to_string())).collect();
    let mut renamed = BTreeMap::new();
    for n in &nodes {
        if target.node(&n.id).is_some() {
            let new = unique(&n.id, &used);
            used.insert(new.clone());
            renamed.insert(n.id.clone(), new);
        }
    }

    let offset = offset(target, &nodes, &backdrops, how.place);
    let mut added = Merged { renamed: renamed.clone(), ..Merged::default() };
    for n in &nodes {
        let mut n = (*n).clone();
        // Wires from nodes not taken: unconnected.
        for input in &mut n.inputs {
            if !input.is_empty() && !taken.contains(input.as_str()) {
                input.clear();
            }
        }
        for (old, new) in &renamed {
            if n.id == *old {
                n.id = new.clone();
            }
            for input in &mut n.inputs {
                if input == old {
                    *input = new.clone();
                }
            }
            rename_refs(&n.op, &mut n.params, old, new);
        }
        n.pos = n.pos.map(|[x, y]| [x + offset[0], y + offset[1]]);
        added.nodes.push(n.id.clone());
        target.nodes.push(n);
    }
    let mut backdrop_ids: HashSet<String> = target.backdrops.iter().map(|b| b.id.clone()).collect();
    for b in backdrops {
        let mut b = b.clone();
        if backdrop_ids.contains(&b.id) {
            b.id = unique(&b.id, &backdrop_ids);
        }
        backdrop_ids.insert(b.id.clone());
        b.pos = [b.pos[0] + offset[0], b.pos[1] + offset[1]];
        added.backdrops.push(b.id.clone());
        target.backdrops.push(b);
    }
    added
}

/// `id`'s base (its trailing digits off) numbered from 1: the first not in
/// `used` (grade1 → grade2, plate → plate1).
fn unique(id: &str, used: &HashSet<String>) -> String {
    let base = id.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = if base.is_empty() { id } else { base };
    (1..).map(|i| format!("{base}{i}")).find(|c| !used.contains(c)).expect("a free id")
}

/// How far to move the merged set: beside the target's bounds, or to the
/// given point.
fn offset(target: &Graph, nodes: &[&NodeSpec], backdrops: &[&Backdrop], place: Place) -> [f32; 2] {
    let corners = |pos: [f32; 2], size: [f32; 2]| [pos, [pos[0] + size[0], pos[1] + size[1]]];
    let bounds = |points: Vec<[f32; 2]>| {
        points.into_iter().fold(None, |b: Option<([f32; 2], [f32; 2])>, p| {
            Some(match b {
                None => (p, p),
                Some((lo, hi)) => ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]),
            })
        })
    };
    let src = bounds(nodes.iter().filter_map(|n| n.pos).chain(backdrops.iter().flat_map(|b| corners(b.pos, b.size))).collect());
    let Some((src_lo, _)) = src else { return [0.0, 0.0] };
    match place {
        Place::At(p) => [p[0] - src_lo[0], p[1] - src_lo[1]],
        Place::Beside => {
            let dst = bounds(target.nodes.iter().filter_map(|n| n.pos).flat_map(|p| corners(p, [NODE_W, 0.0])).chain(target.backdrops.iter().flat_map(|b| corners(b.pos, b.size))).collect());
            match dst {
                Some((lo, hi)) => [hi[0] + GAP - src_lo[0], lo[1] - src_lo[1]],
                None => [0.0, 0.0],
            }
        }
    }
}

/// Point every reference to node `old` in node params `params` (of op
/// `op`) at `new`: links and link formulas, geometry formulas
/// (`loc.translate`), a Curve's Path of, a Media Out's Audio, and a Group
/// or Source group naming exactly that node (a pattern such as rock* is
/// left as written: it matches by name).
pub fn rename_refs(op: &str, params: &mut Value, old: &str, new: &str) {
    crate::link::rename(params, old, new);
    let Some(map) = params.as_object_mut() else { return };
    let mut set = |key: &str, f: &dyn Fn(&str) -> Option<String>| {
        if let Some(v) = map.get(key).and_then(Value::as_str).and_then(f) {
            map.insert(key.into(), Value::String(v));
        }
    };
    let exact = |s: &str| (s.trim() == old).then(|| new.to_string());
    if matches!(op, "delete_geo" | "attribute_set") {
        for key in crate::ops::FORMULA_PARAMS {
            set(key, &|s: &str| Some(crate::expr::rename(s, old, new)).filter(|r| r != s));
        }
    }
    match op {
        "curve" => set("path_of", &exact),
        // ("auto" and "none" aren't nodes.)
        "media_out" => set("audio", &|s: &str| (s != "auto" && s != "none").then(|| exact(s)).flatten()),
        _ => {}
    }
    for key in ["group", "source_group"] {
        set(key, &exact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graph(nodes: Value) -> Graph {
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    fn node<'a>(g: &'a Graph, id: &str) -> &'a NodeSpec {
        g.node(id).unwrap_or_else(|| panic!("no {id}"))
    }

    /// A clash renames the incoming node and everything naming it follows:
    /// inputs, links, link formulas, geometry formulas, a curve's path,
    /// Media Out's audio, an exact Group (not a pattern), a Scanline
    /// Render's extra inputs. The target keeps its project.
    #[test]
    fn clashing_ids_are_renamed_and_references_follow() {
        let mut target = graph(json!([
            { "id": "grade1", "op": "grade", "pos": [0, 0] },
            { "id": "loc", "op": "locator", "pos": [0, 100] },
        ]));
        target.project.fps = 25.0;
        let source = graph(json!([
            { "id": "grade1", "op": "grade", "pos": [0, 0] },
            { "id": "loc", "op": "locator", "pos": [0, 80] },
            { "id": "blur1", "op": "blur", "inputs": ["grade1"], "pos": [150, 0],
              "params": { "size": { "link": { "node": "grade1", "param": "gain" } }, "angle": { "link": { "expr": "grade1.gain * 2 + loc.translate.x" } } } },
            { "id": "set", "op": "attribute_set", "params": { "attr1_name": "Cd", "attr1_expr": "dist(@P, loc.translate)", "group": "loc" } },
            { "id": "del", "op": "delete_geo", "params": { "mode": "expression", "expression": "@P.y < loc.translate.y", "group": "loc*" } },
            { "id": "path", "op": "curve", "params": { "source": "node path", "path_of": "loc" } },
            { "id": "out", "op": "media_out", "inputs": ["blur1"], "params": { "audio": "grade1" } },
            { "id": "render", "op": "scanline_render", "inputs": ["set", "", "", "loc", "grade1"] },
        ]));
        let added = merge(&mut target, &source, &Merge { only: None, place: Place::Beside });
        assert_eq!(added.renamed, BTreeMap::from([("grade1".into(), "grade2".into()), ("loc".into(), "loc1".into())]));
        assert_eq!(added.nodes, ["grade2", "loc1", "blur1", "set", "del", "path", "out", "render"]);
        let blur = node(&target, "blur1");
        assert_eq!(blur.inputs, ["grade2"]);
        assert_eq!(blur.params["size"]["link"]["node"], "grade2");
        assert_eq!(blur.params["angle"]["link"]["expr"], "grade2.gain * 2 + loc1.translate.x");
        let set = &node(&target, "set").params;
        assert_eq!((set["attr1_expr"].as_str(), set["group"].as_str()), (Some("dist(@P, loc1.translate)"), Some("loc1")));
        let del = &node(&target, "del").params;
        assert_eq!((del["expression"].as_str(), del["group"].as_str()), (Some("@P.y < loc1.translate.y"), Some("loc*")), "a pattern stays");
        assert_eq!(node(&target, "path").params["path_of"], "loc1");
        assert_eq!(node(&target, "out").params["audio"], "grade2");
        assert_eq!(node(&target, "render").inputs, ["set", "", "", "loc1", "grade2"]);
        // The target's own nodes and project are untouched.
        assert_eq!(node(&target, "grade1").pos, Some([0.0, 0.0]));
        assert_eq!(target.project.fps, 25.0);
        assert_eq!(target.nodes.len(), 10);
        // Audio "auto" isn't a node named auto.
        let mut p = json!({ "audio": "auto" });
        rename_refs("media_out", &mut p, "auto", "auto1");
        assert_eq!(p["audio"], "auto");
    }

    /// A partial merge (a copy): only the chosen nodes and backdrops; wires
    /// from the others dropped, links to them left as written. Placed at a
    /// point, or beside the target's work; backdrops move with their nodes
    /// and get new ids when they clash.
    #[test]
    fn a_chosen_set_placed_beside_or_at_a_point() {
        let mut source = graph(json!([
            { "id": "read1", "op": "read", "pos": [0, 0] },
            { "id": "grade1", "op": "grade", "inputs": ["read1"], "pos": [200, 50], "params": { "gain": { "link": { "node": "read1", "param": "x" } } } },
            { "id": "blur1", "op": "blur", "inputs": ["grade1"], "pos": [400, 50] },
        ]));
        source.backdrops.push(Backdrop { id: "backdrop1".into(), title: "Look".into(), pos: [180.0, 20.0], size: [400.0, 120.0], color: [0.3; 3], font_size: 14.0 });
        let mut target = graph(json!([{ "id": "plate", "op": "read", "pos": [-100, 300] }, { "id": "comp", "op": "merge", "pos": [300, 400] }]));
        target.backdrops.push(Backdrop { id: "backdrop1".into(), title: "Plates".into(), pos: [-120.0, 280.0], size: [100.0, 50.0], color: [0.3; 3], font_size: 14.0 });
        let only = Some(vec!["grade1".to_string(), "blur1".to_string(), "backdrop1".to_string()]);
        let added = merge(&mut target, &source, &Merge { only: only.clone(), place: Place::Beside });
        assert_eq!((added.nodes.clone(), added.backdrops.clone()), (vec!["grade1".to_string(), "blur1".to_string()], vec!["backdrop2".to_string()]));
        let grade = node(&target, "grade1");
        assert_eq!(grade.inputs, [""], "read1 not taken: unconnected");
        assert_eq!(grade.params["gain"]["link"]["node"], "read1", "a link stays as written");
        assert_eq!(node(&target, "blur1").inputs, ["grade1"]);
        // Beside: right of comp (300 + its width + the gap), tops with plate's backdrop (280).
        let at = grade.pos.unwrap();
        let bd = target.backdrops.iter().find(|b| b.id == "backdrop2").unwrap();
        assert_eq!(bd.pos, [300.0 + NODE_W + GAP, 280.0], "the set's top-left (its backdrop) beside the work");
        assert_eq!([at[0] - bd.pos[0], at[1] - bd.pos[1]], [20.0, 30.0], "the backdrop moved with its nodes");
        // At a point: the set's top-left there.
        let mut empty = Graph::default();
        let added = merge(&mut empty, &source, &Merge { only: Some(vec!["grade1".into(), "blur1".into()]), place: Place::At([1000.0, -40.0]) });
        assert_eq!(added.nodes.len(), 2);
        assert_eq!((node(&empty, "grade1").pos, node(&empty, "blur1").pos), (Some([1000.0, -40.0]), Some([1200.0, -40.0])));
        // Into an empty graph, beside: where they were.
        let mut empty = Graph::default();
        merge(&mut empty, &source, &Merge { only: None, place: Place::Beside });
        assert_eq!(node(&empty, "blur1").pos, Some([400.0, 50.0]));
        assert_eq!(empty.backdrops[0].id, "backdrop1");
    }

    /// Renames never chain: the source's own ids are kept out of the new
    /// ones (grade1 clashing can't become a grade2 the source also has).
    #[test]
    fn new_ids_avoid_the_sources_own() {
        let mut target = graph(json!([{ "id": "grade1", "op": "grade" }, { "id": "grade2", "op": "grade" }]));
        let source = graph(json!([
            { "id": "grade1", "op": "grade" },
            { "id": "grade3", "op": "grade", "inputs": ["grade1"] },
            { "id": "x", "op": "blur", "inputs": ["grade3"], "params": { "size": { "link": { "expr": "grade1.gain + grade3.gain" } } } },
        ]));
        let added = merge(&mut target, &source, &Merge { only: None, place: Place::Beside });
        assert_eq!(added.renamed, BTreeMap::from([("grade1".into(), "grade4".into())]));
        assert_eq!(node(&target, "grade3").inputs, ["grade4"]);
        assert_eq!(node(&target, "x").params["size"]["link"]["expr"], "grade4.gain + grade3.gain");
        let ids: HashSet<&str> = target.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), target.nodes.len(), "every id once");
    }

    /// Paths: a project in another folder with a plate given relative to
    /// it, merged into a project saved in a different folder: the Read
    /// still names the same file, when read back too. A project without a
    /// folder: its relative paths are its file's folder's.
    #[test]
    fn imported_reads_keep_their_files() {
        let tmp = std::env::temp_dir().join(format!("arclight_merge_paths_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let shot = tmp.join("shots/sh010");
        std::fs::create_dir_all(shot.join("plates")).unwrap();
        std::fs::write(shot.join("plates/bg.0001.png"), b"").unwrap();
        let plate = shot.join("plates/bg.####.png");
        let src = json!({ "project": { "first_frame": 1, "last_frame": 10, "fps": 24.0, "folder": "." }, "nodes": [{ "id": "bg", "op": "read", "params": { "path": "plates/bg.####.png" } }] });
        std::fs::write(shot.join("sh010.json"), src.to_string()).unwrap();
        let loose = json!({ "project": { "first_frame": 1, "last_frame": 10, "fps": 24.0 }, "nodes": [{ "id": "fg", "op": "read", "params": { "path": "plates/bg.####.png" } }] });
        std::fs::write(shot.join("loose.json"), loose.to_string()).unwrap();

        let comp = tmp.join("comp");
        std::fs::create_dir_all(&comp).unwrap();
        let mut target: Graph = serde_json::from_value(json!({ "project": { "first_frame": 1, "last_frame": 50, "fps": 25.0, "folder": comp.to_string_lossy() }, "nodes": [] })).unwrap();
        merge(&mut target, &read(&shot.join("sh010.json")).unwrap(), &Merge { only: None, place: Place::Beside });
        merge(&mut target, &read(&shot.join("loose.json")).unwrap(), &Merge { only: None, place: Place::Beside });
        assert_eq!(node(&target, "bg").params["path"], plate.to_string_lossy().as_ref());
        assert_eq!(node(&target, "fg").params["path"], plate.to_string_lossy().as_ref(), "no folder: its file's");
        assert_eq!((target.project.last_frame, target.project.fps), (50, 25.0), "the target's project");
        // Saved into the comp's folder (the plate outside it: absolute) and read back.
        let file = comp.join("comp.json");
        target.save(&file).unwrap();
        let back = Graph::from_file(&file).unwrap();
        assert_eq!(node(&back, "bg").params["path"], plate.to_string_lossy().as_ref());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
