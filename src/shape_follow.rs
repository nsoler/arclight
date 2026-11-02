//! Shapes that follow a Tracker: a Rectangle, Ellipse, Polygon or Roto
//! with `follow` naming a Tracker node moves, turns and scales with the
//! motion that Tracker solves, from `follow_reference` (the Tracker's
//! own reference frame when unset) to the frame being drawn.
//!
//! The shape's own params (and their keys) stay in the reference frame's
//! space: the drawn outline is `motion(t) · shape(t)`, so a key set on
//! some frame corrects the shape under the track rather than fighting it.
//! Neither Feather nor a per-point feather is scaled by the motion.
//!
//! The motion is a live link (`crate::link`, `{"link": {"node": N,
//! "follow": true, "reference": R}}`) resolving to the affine matrix, so it
//! is worked out like any other link: through the graph at each frame and
//! each motion-blur sample, in the node's cache key, renamed with the
//! Tracker. `ops` puts it in the resolved params as `_follow` (see
//! [`inject`]); the shape ops map their outlines through it ([`affine`],
//! [`map_point`], [`map_outline`]), and the viewer draws (and drags) the
//! shape's handles through it.

use crate::track::{self, Affine, Pt};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

/// The ops that can follow a Tracker.
pub const OPS: [&str; 4] = ["rectangle", "ellipse", "polygon", "roto"];

/// The resolved-params key holding the followed motion (a matrix, or null
/// when the Tracker can't be read).
pub const RESOLVED: &str = "_follow";

/// The live link to Tracker `node`'s motion from frame `reference` (None:
/// its own reference frame) to the time it's read at, as an affine matrix.
pub fn link(node: &str, reference: Option<i64>) -> Value {
    json!({ "link": { "node": node, "follow": true, "reference": reference } })
}

/// The Tracker `node` a shape's raw (or resolved) params follow, if any.
pub fn followed(params: &Value) -> Option<&str> {
    params.get("follow").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty())
}

/// The link a shape's raw params follow through, if they follow a Tracker.
pub fn link_of(params: &Value) -> Option<Value> {
    let node = followed(params)?;
    Some(link(node, params.get("follow_reference").and_then(Value::as_i64)))
}

/// Tracker params' solved motion from frame `reference` (None: their
/// `reference_frame`) to time `t`, with their own Solve (translate, plus
/// rotate and scale when it has them); None when no track is on. Exactly
/// the identity at the reference frame.
pub fn motion(tracker: &Value, reference: Option<i64>, t: f64) -> Option<Affine> {
    let p = crate::ops::params_with_defaults("tracker", tracker);
    let kind = track::Solve::from_choice(p["solve"].as_str(), track::Solve::Scale);
    let r = reference.unwrap_or_else(|| p["reference_frame"].as_i64().unwrap_or(1));
    if t == r as f64 {
        return Some(IDENTITY);
    }
    track::solve_tracks(&track::positions(&p, r), &track::positions_at(&p, t), &track::warps(&p, r), &track::warps_at(&p, t), kind)
}

pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// A matrix as a link's value (and back, [`from_value`]).
pub fn to_value(m: &Affine) -> Value {
    json!(m)
}

pub fn from_value(v: &Value) -> Option<Affine> {
    let a = v.as_array().filter(|a| a.len() == 6)?;
    let mut m = [0.0; 6];
    for (o, x) in m.iter_mut().zip(a) {
        *o = x.as_f64()? as f32;
    }
    Some(m)
}

/// A shape op's resolved params given the followed motion at `t` as
/// [`RESOLVED`], when they follow a Tracker and `links` can read the graph
/// (`follow(link, t)`); a matrix, or null when it can't be read (the op
/// then says why).
pub fn inject(op: &str, raw: &Map<String, Value>, t: f64, follow: &dyn Fn(&Value, f64) -> Value, out: &mut Value) {
    if !OPS.contains(&op) {
        return;
    }
    let raw = Value::Object(raw.clone());
    let (Some(l), Value::Object(map)) = (link_of(&raw), out) else { return };
    let v = follow(&l, t);
    map.insert(RESOLVED.into(), if from_value(&v).is_some() { v } else { Value::Null });
}

/// The motion a shape op's resolved `params` follow: None when they follow
/// nothing (or were resolved without the graph, e.g. on their own);
/// an error when the Tracker can't be read.
pub fn affine(params: &Value) -> Result<Option<Affine>> {
    let Some(node) = followed(params) else { return Ok(None) };
    match params.get(RESOLVED) {
        None => Ok(None),
        Some(v) => match from_value(v) {
            Some(m) => Ok(Some(m)),
            None => bail!("follows '{node}', which doesn't exist or isn't a Tracker"),
        },
    }
}

/// A point of a shape moved by the followed motion `m`.
pub fn map_point(m: &Affine, p: Pt) -> Pt {
    track::apply(m, p)
}

/// A shape's outline moved by what its resolved `params` follow (as it is
/// when they follow nothing).
pub fn map_outline(outline: Vec<Pt>, params: &Value) -> Result<Vec<Pt>> {
    Ok(match affine(params)? {
        Some(m) => outline.into_iter().map(|p| map_point(&m, p)).collect(),
        None => outline,
    })
}

/// A link value's motion for `crate::link`: Tracker `tracker`'s (its node's
/// params) as `l` (the link object) asks, at time `t`.
pub fn link_value(op: &str, tracker: &Value, l: &Value, t: f64) -> Result<Value, String> {
    if op != "tracker" {
        return Err("isn't a Tracker".into());
    }
    let reference = l.get("reference").and_then(Value::as_i64);
    let m = motion(tracker, reference, t).unwrap_or(IDENTITY);
    Ok(to_value(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Pt, b: Pt) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    /// Two tracks: the second swings 90° about the first by frame 5, and
    /// both move 20 px right.
    fn turning() -> Value {
        json!({
            "track1_on": true, "track1": { "keys": [[1, [10.0, 10.0]], [5, [30.0, 10.0]]], "interp": "linear" },
            "track2_on": true, "track2": { "keys": [[1, [30.0, 10.0]], [5, [30.0, 30.0]]], "interp": "linear" },
            "solve": "translate, rotate",
        })
    }

    #[test]
    fn motion_is_identity_at_the_reference_and_follows_the_tracks() {
        let t = turning();
        assert_eq!(motion(&t, None, 1.0), Some(IDENTITY));
        let m = motion(&t, None, 5.0).unwrap();
        // The tracks' own points land on their frame-5 positions.
        assert!(close(map_point(&m, [10.0, 10.0]), [30.0, 10.0]));
        assert!(close(map_point(&m, [30.0, 10.0]), [30.0, 30.0]));
        // From another reference: identity there, the rest relative to it.
        assert_eq!(motion(&t, Some(5), 5.0), Some(IDENTITY));
        let back = motion(&t, Some(5), 1.0).unwrap();
        assert!(close(map_point(&back, [30.0, 30.0]), [30.0, 10.0]));
        // Translate only: no turn.
        let mut tr = t.clone();
        tr["solve"] = json!("translate");
        let m = motion(&tr, None, 5.0).unwrap();
        assert!((m[0] - 1.0).abs() < 1e-5 && m[1].abs() < 1e-5, "{m:?}");
        // No track on: none.
        assert_eq!(motion(&json!({ "track1_on": false }), None, 3.0), None);
    }

    #[test]
    fn the_link_reads_the_tracker_and_follows_renames() {
        let g: crate::graph::Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "t", "op": "tracker", "params": turning() },
            { "id": "box", "op": "rectangle", "params": { "follow": "t" } },
            { "id": "notrack", "op": "grade" },
        ] }))
        .unwrap();
        let lookup = |id: &str| g.node(id);
        let read = |l: &Value, t: f64| crate::link::resolve(&lookup, l, t, crate::link::Clock::default());
        let m = from_value(&read(&link("t", None), 5.0)).unwrap();
        assert!(close(map_point(&m, [10.0, 10.0]), [30.0, 10.0]));
        // Between frames too (motion blur): halfway along, turned 45°, the
        // tracks' midpoint (a rotate-only fit pins it) on theirs then.
        let m = from_value(&read(&link("t", None), 3.0)).unwrap();
        assert!(close(map_point(&m, [20.0, 10.0]), [25.0, 15.0]), "{m:?}");
        assert!((m[3].atan2(m[0]).to_degrees() - 45.0).abs() < 1e-3, "{m:?}");
        assert_eq!(read(&link("gone", None), 3.0), Value::Null);
        let e = crate::link::error(&lookup, &link("notrack", None), 3.0, crate::link::Clock::default()).unwrap();
        assert!(e.contains("isn't a Tracker"), "{e}");
        assert_eq!(crate::link::describe(&link("t", Some(4))), "t · followed");
        // Renaming the Tracker renames what follows it.
        let mut p = json!({ "follow": "t", "follow_reference": 3 });
        crate::graph_merge::rename_refs("roto", &mut p, "t", "track_a");
        assert_eq!(p["follow"], json!("track_a"));
    }

    fn graph(nodes: Value) -> crate::graph::Graph {
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    fn render(g: &crate::graph::Graph, id: &str, frame: i64) -> crate::image::Image {
        let ctx = crate::ops::EvalContext { frame, write_outputs: false };
        crate::graph::Evaluator::new().evaluate(g, id, &ctx).unwrap().as_ref().clone()
    }

    /// The two images agree pixel for pixel over the frame.
    fn same(a: &crate::image::Image, b: &crate::image::Image) {
        for y in 0..a.format.height as i32 {
            for x in 0..a.format.width as i32 {
                let (p, q) = (a.at(x, y), b.at(x, y));
                assert!(p.iter().zip(q).all(|(u, v)| (u - v).abs() < 1e-4), "({x}, {y}): {p:?} vs {q:?}");
            }
        }
    }

    /// A Tracker moving 20 px right from frame 1 to 5 (one track).
    fn sliding() -> Value {
        json!({ "track1_on": true, "track1": { "keys": [[1, [10.0, 10.0]], [5, [30.0, 10.0]]], "interp": "linear" }, "solve": "translate" })
    }

    /// `points` moved by `m`.
    fn moved(m: &Affine, points: &[f32]) -> Vec<f32> {
        points.chunks(2).flat_map(|c| map_point(m, [c[0], c[1]])).collect()
    }

    #[test]
    fn a_shape_lands_where_the_track_says_and_is_unchanged_at_the_reference() {
        let rect = |follow: &str| json!({ "width": 64, "height": 32, "center": [10, 10], "size": [6, 6], "follow": follow });
        let g = graph(json!([
            { "id": "t", "op": "tracker", "params": sliding() },
            { "id": "r", "op": "rectangle", "params": rect("t") },
            { "id": "still", "op": "rectangle", "params": rect("") },
            { "id": "there", "op": "rectangle", "params": { "width": 64, "height": 32, "center": [30, 10], "size": [6, 6] } },
        ]));
        same(&render(&g, "r", 1), &render(&g, "still", 1));
        same(&render(&g, "r", 5), &render(&g, "there", 5));
        // A reference frame of its own: identity there instead.
        let mut g2 = g.clone();
        g2.nodes[1].params["follow_reference"] = json!(5);
        same(&render(&g2, "r", 5), &render(&g, "still", 5));
    }

    #[test]
    fn a_turning_track_turns_the_shape_and_corrections_compose_under_it() {
        let tri = [12.0, 8.0, 26.0, 9.0, 18.0, 20.0];
        // Keyed: frame 5's points nudged 3 px right of frame 1's (a fix).
        let fixed = [15.0, 8.0, 29.0, 9.0, 21.0, 20.0];
        let keyed = json!({ "keys": [[1, tri], [5, fixed]], "interp": "linear" });
        let g = graph(json!([
            { "id": "t", "op": "tracker", "params": turning() },
            { "id": "p", "op": "polygon", "params": { "width": 48, "height": 48, "points": tri, "follow": "t" } },
            { "id": "k", "op": "polygon", "params": { "width": 48, "height": 48, "points": keyed, "follow": "t" } },
        ]));
        let m = motion(&turning(), None, 5.0).unwrap();
        let want = |points: Vec<f32>| {
            let g = graph(json!([{ "id": "w", "op": "polygon", "params": { "width": 48, "height": 48, "points": points } }]));
            render(&g, "w", 5)
        };
        same(&render(&g, "p", 5), &want(moved(&m, &tri)));
        // M · (corrected), not M · shape + correction (they differ: M turns).
        same(&render(&g, "k", 5), &want(moved(&m, &fixed)));
    }

    #[test]
    fn motion_blur_follows_the_track() {
        let blur = json!({ "motion_blur": 5, "shutter": 1.0 });
        let mut follows = json!({ "width": 64, "height": 32, "center": [10, 10], "size": [6, 6], "follow": "t" });
        let mut keyed = json!({ "width": 64, "height": 32, "center": { "keys": [[1, [10, 10]], [5, [30, 10]]], "interp": "linear" }, "size": [6, 6] });
        for p in [&mut follows, &mut keyed] {
            p.as_object_mut().unwrap().extend(blur.as_object().unwrap().clone());
        }
        let g = graph(json!([
            { "id": "t", "op": "tracker", "params": sliding() },
            { "id": "r", "op": "rectangle", "params": follows },
            { "id": "k", "op": "rectangle", "params": keyed },
        ]));
        same(&render(&g, "r", 3), &render(&g, "k", 3));
    }

    #[test]
    fn old_projects_are_unchanged_and_a_missing_tracker_is_named() {
        let p = json!({ "center": [10, 10] });
        let follow = |_: &Value, _: f64| -> Value { panic!("nothing to follow") };
        let resolved = crate::ops::resolve_params_linked("rectangle", &p, 1, Some(&follow));
        assert!(resolved.get(RESOLVED).is_none(), "{resolved}");
        let g = graph(json!([{ "id": "r", "op": "roto", "params": { "follow": "gone" } }]));
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let e = crate::graph::Evaluator::new().evaluate(&g, "r", &ctx).err().unwrap().to_string();
        assert!(e.contains("'gone'"), "{e}");
    }

    #[test]
    fn outlines_map_only_when_following_and_say_why_they_cant() {
        let outline = vec![[0.0, 0.0], [10.0, 0.0]];
        let shift = to_value(&[1.0, 0.0, 5.0, 0.0, 1.0, -2.0]);
        // Following nothing, or not resolved through the graph: unchanged.
        assert_eq!(map_outline(outline.clone(), &json!({})).unwrap(), outline);
        assert_eq!(map_outline(outline.clone(), &json!({ "follow": "t" })).unwrap(), outline);
        let p = json!({ "follow": "t", "_follow": shift });
        assert_eq!(map_outline(outline.clone(), &p).unwrap(), vec![[5.0, -2.0], [15.0, -2.0]]);
        let e = map_outline(outline, &json!({ "follow": "gone", "_follow": null })).unwrap_err();
        assert!(e.to_string().contains("'gone'"), "{e}");
        // The link a shape follows through.
        assert_eq!(link_of(&json!({ "follow": " t ", "follow_reference": 12 })), Some(link("t", Some(12))));
        assert_eq!(link_of(&json!({ "follow": "" })), None);
    }
}
