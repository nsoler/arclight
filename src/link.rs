//! Live links: a param whose value comes from another node, worked out at
//! every frame (and between frames, for motion blur) instead of being
//! stored. A link is a JSON object in place of the value:
//!
//! - `{"link": {"node": N, "param": P}}`: N's param P at this time (its
//!   keyframes evaluated; a link there is followed in turn).
//! - `{"link": {"node": N, "param": P, "at": "reference"}}`: P at N's
//!   `reference_frame` (a Corner Pin's From corners).
//! - `{"link": {"node": N, "motion": M, "stabilize": B, "pivot": [x, y]}}`:
//!   Tracker N's solved motion as a Transform's `translate`, `rotate` or
//!   `scale` (M) about `pivot`, match-move or (B) stabilize.
//!
//! Any link may also carry `"multiply"` and `"add"` (a number for every
//! component, or one per component): the value is then
//! `linked × multiply + add`, e.g. half a rotation (`"multiply": 0.5`) or a
//! track followed 10 px to its right (`"add": [10, 0]`).
//!
//! The graph resolves links before each node renders, so the cache key
//! sees the values: changing the source re-renders what follows it.

use crate::graph::NodeSpec;
use crate::ops;
use crate::track;
use serde_json::{json, Value};

/// How far a chain of links is followed before giving up (a loop).
const MAX_DEPTH: usize = 8;

pub fn is_link(v: &Value) -> bool {
    v.get("link").is_some_and(Value::is_object)
}

/// The node a link reads from.
pub fn source(v: &Value) -> Option<&str> {
    v.get("link")?.get("node")?.as_str()
}

/// A link to `node`'s param `param`.
pub fn param(node: &str, param: &str) -> Value {
    json!({ "link": { "node": node, "param": param } })
}

/// A link to `node`'s param `param` at its reference frame.
pub fn at_reference(node: &str, param: &str) -> Value {
    json!({ "link": { "node": node, "param": param, "at": "reference" } })
}

/// A link to Tracker `node`'s motion, as a Transform's `translate`,
/// `rotate` or `scale` about `pivot`.
pub fn motion(node: &str, what: &str, stabilize: bool, pivot: track::Pt) -> Value {
    json!({ "link": { "node": node, "motion": what, "stabilize": stabilize, "pivot": pivot } })
}

/// A link's value at time `t`, reading nodes through `lookup`; any other
/// value is returned with its keyframes evaluated. Null when the link's
/// node is gone (see [`missing`]) or the chain loops.
pub fn resolve<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, v: &Value, t: f64) -> Value {
    resolve_depth(lookup, v, t, 0)
}

fn resolve_depth<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, v: &Value, t: f64, depth: usize) -> Value {
    if !is_link(v) {
        return crate::anim::evaluate(v, t);
    }
    let l = &v["link"];
    let Some(n) = l["node"].as_str().and_then(lookup) else { return Value::Null };
    if depth >= MAX_DEPTH {
        return Value::Null;
    }
    let value = if let Some(what) = l["motion"].as_str() {
        let pivot = [l["pivot"][0].as_f64().unwrap_or(0.0) as f32, l["pivot"][1].as_f64().unwrap_or(0.0) as f32];
        let (translate, rotate, scale) = track::motion_at(&n.params, t, l["stabilize"].as_bool().unwrap_or(false), pivot);
        match what {
            "translate" => json!(translate),
            "rotate" => json!(rotate),
            "scale" => json!([scale, scale]),
            _ => Value::Null,
        }
    } else {
        let Some(name) = l["param"].as_str() else { return Value::Null };
        let p = ops::params_with_defaults(&n.op, &n.params);
        let t = match l["at"].as_str() {
            Some("reference") => p["reference_frame"].as_f64().unwrap_or(1.0),
            _ => t,
        };
        resolve_depth(lookup, &p[name], t, depth + 1)
    };
    adjust(value, &l["multiply"], &l["add"])
}

/// `v × multiply + add`, per component: `multiply` and `add` are a number
/// for every component or a list (missing ones: 1 and 0). Anything that
/// isn't a number or a list of numbers passes through.
pub fn adjust(v: Value, multiply: &Value, add: &Value) -> Value {
    if multiply.is_null() && add.is_null() {
        return v;
    }
    let pick = |x: &Value, i: usize, default: f64| match x {
        Value::Array(a) => a.get(i).and_then(Value::as_f64).unwrap_or(default),
        other => other.as_f64().unwrap_or(default),
    };
    let one = |x: f64, i: usize| x * pick(multiply, i, 1.0) + pick(add, i, 0.0);
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), |x| json!(one(x, 0))),
        Value::Array(a) if a.iter().all(Value::is_number) => {
            Value::Array(a.iter().enumerate().map(|(i, x)| json!(one(x.as_f64().unwrap_or(0.0), i))).collect())
        }
        other => other,
    }
}

/// `link` with its multiply and add set (null removes one).
pub fn with_adjust(link: &Value, multiply: Value, add: Value) -> Value {
    let mut l = link.clone();
    for (key, v) in [("multiply", multiply), ("add", add)] {
        match v {
            Value::Null => {
                l["link"].as_object_mut().map(|m| m.remove(key));
            }
            v => l["link"][key] = v,
        }
    }
    l
}

/// The first of `params` linked to a node that `lookup` can't find: the
/// param's name and the missing node's.
pub fn missing<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, params: &Value) -> Option<(String, String)> {
    params.as_object()?.iter().find_map(|(k, v)| {
        let node = source(v)?;
        lookup(node).is_none().then(|| (k.clone(), node.to_string()))
    })
}

/// Point links to node `old` at `new` instead (after a rename).
pub fn rename(params: &mut Value, old: &str, new: &str) {
    let Some(map) = params.as_object_mut() else { return };
    for v in map.values_mut() {
        if source(v) == Some(old) {
            v["link"]["node"] = json!(new);
        }
    }
}

/// What a link reads, for the panel: "tracker1 · track1",
/// "tracker1 · track1 at the reference frame", "tracker1 · stabilize".
pub fn describe(v: &Value) -> String {
    let l = &v["link"];
    let node = l["node"].as_str().unwrap_or("?");
    let what = if l.get("motion").is_some() {
        let how = if l["stabilize"].as_bool() == Some(true) { "stabilize" } else { "match-move" };
        format!("{node} · {how}")
    } else {
        let param = l["param"].as_str().unwrap_or("?");
        match l["at"].as_str() {
            Some("reference") => format!("{node} · {param} at the reference frame"),
            _ => format!("{node} · {param}"),
        }
    };
    // "× 0.5", "+ [10, 0]" when set.
    let show = |x: &Value| match x {
        Value::Array(a) => format!("[{}]", a.iter().map(|v| format!("{}", v.as_f64().unwrap_or(0.0))).collect::<Vec<_>>().join(", ")),
        other => format!("{}", other.as_f64().unwrap_or(0.0)),
    };
    let mut out = what;
    if !l["multiply"].is_null() {
        out += &format!(" × {}", show(&l["multiply"]));
    }
    if !l["add"].is_null() {
        out += &format!(" + {}", show(&l["add"]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Evaluator, Graph};
    use crate::image::Image;
    use crate::ops::EvalContext;

    fn graph(nodes: Value) -> Graph {
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    #[test]
    fn links_follow_params_chains_and_reference_frames() {
        let g = graph(json!([
            { "id": "a", "op": "rectangle", "params": { "center": { "keys": [[1, [0, 0]], [11, [100, 50]]], "interp": "linear" } } },
            { "id": "b", "op": "rectangle", "params": { "center": param("a", "center") } },
            { "id": "c", "op": "rectangle", "params": { "center": param("b", "center") } },
            { "id": "t", "op": "tracker", "params": { "reference_frame": 11, "track1": { "keys": [[1, [5, 5]], [11, [15, 25]]], "interp": "linear" } } },
            { "id": "loop", "op": "rectangle", "params": { "center": param("loop", "center") } },
        ]));
        let lookup = |id: &str| g.node(id);
        let at = |node: &str, param: &str, t: f64| resolve(&lookup, &g.node(node).unwrap().params[param], t);
        assert_eq!(at("b", "center", 6.0), json!([50.0, 25.0]));
        assert_eq!(at("c", "center", 6.0), json!([50.0, 25.0]), "a link to a link");
        assert_eq!(resolve(&lookup, &at_reference("t", "track1"), 1.0), json!([15.0, 25.0]), "at the reference frame");
        assert_eq!(at("loop", "center", 1.0), Value::Null, "a loop gives up");
        assert_eq!(missing(&lookup, &json!({ "center": param("gone", "center") })), Some(("center".into(), "gone".into())));
        let mut p = json!({ "center": param("a", "center"), "size": [1, 2] });
        rename(&mut p, "a", "z");
        assert_eq!(source(&p["center"]), Some("z"));
        assert_eq!(describe(&at_reference("t", "track1")), "t · track1 at the reference frame");
    }

    #[test]
    fn links_multiply_and_add() {
        // Per component, a number for all, or missing ones left alone.
        assert_eq!(adjust(json!([2.0, 4.0]), &json!(0.5), &json!([10, 0])), json!([11.0, 2.0]));
        assert_eq!(adjust(json!(90.0), &json!(0.5), &Value::Null), json!(45.0));
        assert_eq!(adjust(json!([1.0, 1.0, 1.0]), &json!([2]), &json!(1)), json!([3.0, 2.0, 2.0]));
        assert_eq!(adjust(json!("text"), &json!(2), &json!(1)), json!("text"));
        let g = graph(json!([
            { "id": "a", "op": "rectangle", "params": { "center": { "keys": [[1, [0, 0]], [11, [100, 50]]], "interp": "linear" } } },
            // Followed 10 px to the right…
            { "id": "b", "op": "rectangle", "params": { "center": with_adjust(&param("a", "center"), Value::Null, json!([10, 0])) } },
            // …and a track's rotation, halved.
            { "id": "t", "op": "tracker", "params": {
                "track1_on": true, "track1": [10.5, 10.5], "track2_on": true,
                "track2": { "keys": [[1, [30.5, 10.5]], [5, [10.5, 30.5]]], "interp": "linear" }, "solve": "translate, rotate" } },
            { "id": "half", "op": "transform", "params": {
                "rotate": with_adjust(&motion("t", "rotate", false, [10.5, 10.5]), json!(0.5), Value::Null) } },
        ]));
        let lookup = |id: &str| g.node(id);
        let at = |node: &str, param: &str, t: f64| resolve(&lookup, &g.node(node).unwrap().params[param], t);
        assert_eq!(at("b", "center", 6.0), json!([60.0, 25.0]));
        // The second track swings 90° about the first by frame 5: half is 45°.
        let full = resolve(&lookup, &motion("t", "rotate", false, [10.5, 10.5]), 5.0).as_f64().unwrap();
        let half = at("half", "rotate", 5.0).as_f64().unwrap();
        assert!((full - 90.0).abs() < 1e-3 && (half - 45.0).abs() < 1e-3, "{full} {half}");
        // Removing them, and how they read in the panel.
        let l = with_adjust(&param("a", "center"), json!(0.5), json!([10, 0]));
        assert_eq!(describe(&l), "a · center × 0.5 + [10, 0]");
        assert_eq!(with_adjust(&l, Value::Null, Value::Null), param("a", "center"));
    }

    /// A Corner Pin linked to four tracks renders like one keyed with a
    /// copy of them; a linked, motion-blurred Transform blurs along the
    /// track's motion.
    #[test]
    fn linked_nodes_render_like_copies_and_blur_along_the_link() {
        let tracks = |stride: f32| {
            let mut p = json!({ "reference_frame": 1 });
            for (i, [x, y]) in [[10.5, 10.5], [50.5, 10.5], [50.5, 30.5], [10.5, 30.5]].into_iter().enumerate() {
                p[format!("track{}", i + 1)] = json!({ "keys": [[1, [x, y]], [5, [x + stride, y + 2.0]]], "interp": "linear" });
                p[format!("track{}_on", i + 1)] = json!(true);
            }
            p
        };
        let mut g = graph(json!([
            { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 40, "size": 4 } },
            { "id": "t", "op": "tracker", "inputs": ["plate"], "params": tracks(8.0) },
        ]));
        for (id, linked) in [("live", true), ("copy", false)] {
            let params = crate::track::export(&g, "t", "corner_pin", false, linked).unwrap();
            g.nodes.push(serde_json::from_value(json!({ "id": id, "op": "corner_pin", "inputs": ["plate"], "params": params })).unwrap());
        }
        let mut ev = Evaluator::new();
        for frame in [1, 3, 5] {
            let ctx = EvalContext { frame, write_outputs: false };
            let (a, b) = (ev.evaluate(&g, "live", &ctx).unwrap(), ev.evaluate(&g, "copy", &ctx).unwrap());
            assert_eq!(a.pixels, b.pixels, "frame {frame}");
        }
        // Motion blur on a linked Transform: sampled between frames too.
        let mut xf = crate::track::export(&g, "t", "transform", false, true).unwrap();
        g.nodes.push(serde_json::from_value(json!({ "id": "sharp", "op": "transform", "inputs": ["plate"], "params": xf.clone() })).unwrap());
        xf.insert("motion_blur".into(), json!(8));
        xf.insert("shutter".into(), json!(1.0));
        g.nodes.push(serde_json::from_value(json!({ "id": "blurred", "op": "transform", "inputs": ["plate"], "params": xf })).unwrap());
        // …the same as a keyed copy blurs (its keys interpolate the same way).
        let mut copy = crate::track::export(&g, "t", "transform", false, false).unwrap();
        copy.insert("motion_blur".into(), json!(8));
        copy.insert("shutter".into(), json!(1.0));
        g.nodes.push(serde_json::from_value(json!({ "id": "copy_blurred", "op": "transform", "inputs": ["plate"], "params": copy })).unwrap());
        let ctx = EvalContext { frame: 3, write_outputs: false };
        let get = |ev: &mut Evaluator, id: &str| ev.evaluate(&g, id, &ctx).unwrap().flat().into_owned();
        let diff = |a: &Image, b: &Image| a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
        let (sharp, blurred, copied) = (get(&mut ev, "sharp"), get(&mut ev, "blurred"), get(&mut ev, "copy_blurred"));
        assert!(diff(&sharp, &blurred) > 0.1, "blurred: {}", diff(&sharp, &blurred));
        assert!(diff(&blurred, &copied) < 2e-3, "as a keyed copy blurs: {}", diff(&blurred, &copied));
    }
}
