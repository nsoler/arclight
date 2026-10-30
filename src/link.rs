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
//! - `{"link": {"node": N, "solve": S}}`: Camera Tracker N's solved camera
//!   as a Camera's `translate`, `rotate`, `focal` or `film_back` (S),
//!   between solved frames as their keys would be (see
//!   [`crate::camtrack::solved_value`]).
//!
//! - `{"link": {"expr": F}}`: formula F (see [`crate::expr`]), reading any
//!   number of nodes' params, e.g. `"sin(frame / 8) * 5 + t.track1.x"`.
//!
//! Any link may also carry `"multiply"` and `"add"` (a number for every
//! component, or one per component): the value is then
//! `linked × multiply + add`, e.g. half a rotation (`"multiply": 0.5`) or a
//! track followed 10 px to its right (`"add": [10, 0]`).
//!
//! The graph resolves links before each node renders, so the cache key
//! sees the values: changing the source re-renders what follows it.

pub use crate::expr::Clock;
use crate::graph::NodeSpec;
use crate::ops;
use crate::track;
use serde_json::{json, Value};

/// How far a chain of links is followed before giving up (a loop).
const MAX_DEPTH: usize = 8;

pub fn is_link(v: &Value) -> bool {
    v.get("link").is_some_and(Value::is_object)
}

/// The node a link reads from (not a formula's: see [`sources`]).
pub fn source(v: &Value) -> Option<&str> {
    v.get("link")?.get("node")?.as_str()
}

/// A formula link's text.
pub fn formula(v: &Value) -> Option<&str> {
    v.get("link")?.get("expr")?.as_str()
}

/// Every node a link reads from: its node, or those its formula names.
pub fn sources(v: &Value) -> Vec<String> {
    if let Some(n) = source(v) {
        return vec![n.to_string()];
    }
    let Some(e) = formula(v).and_then(|f| crate::expr::parse(f).ok()) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for (node, _) in e.refs() {
        if !out.iter().any(|n| n == node) {
            out.push(node.to_string());
        }
    }
    out
}

/// A link to the value of formula `f`.
pub fn expr(f: &str) -> Value {
    json!({ "link": { "expr": f } })
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

/// A link to Camera Tracker `node`'s solved camera, as a Camera's
/// `translate`, `rotate`, `focal` or `film_back` (`what`).
pub fn solve(node: &str, what: &str) -> Value {
    json!({ "link": { "node": node, "solve": what } })
}

/// A link's value at frame `t` (`clock`: the project's timing, for a
/// formula's `time` and `fps`), reading nodes through `lookup`; any other
/// value is returned with its keyframes evaluated. Null when the link's
/// node is gone (see [`missing`]), the chain loops or a formula fails
/// (see [`error`]).
pub fn resolve<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, v: &Value, t: f64, clock: Clock) -> Value {
    evaluate(lookup, v, t, clock, 0).unwrap_or(Value::Null)
}

/// Why `v` can't be worked out at time `t`, if it can't.
pub fn error<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, v: &Value, t: f64, clock: Clock) -> Option<String> {
    evaluate(lookup, v, t, clock, 0).err()
}

fn evaluate<'a>(lookup: &dyn Fn(&str) -> Option<&'a NodeSpec>, v: &Value, t: f64, clock: Clock, depth: usize) -> Result<Value, String> {
    if !is_link(v) {
        return Ok(crate::anim::evaluate(v, t));
    }
    if depth >= MAX_DEPTH {
        return Err("it loops: its links lead back to it".into());
    }
    let l = &v["link"];
    // Node `n`'s param `name` at `t`, its own links followed.
    let read = |n: &NodeSpec, name: &str, t: f64| -> Result<Value, String> {
        let p = ops::params_with_defaults(&n.op, &n.params);
        let raw = p.get(name).ok_or_else(|| format!("'{}' has no param '{name}'", n.id))?;
        evaluate(lookup, raw, t, clock, depth + 1)
    };
    if let Some(src) = l.get("expr") {
        let src = src.as_str().ok_or("a formula must be text")?;
        let e = crate::expr::parse(src)?;
        let refs = |node: &str, name: &str, t: f64| read(lookup(node).ok_or_else(|| format!("no node '{node}'"))?, name, t);
        let value = e.eval(&refs, clock, t)?.to_json();
        return Ok(adjust(value, &l["multiply"], &l["add"]));
    }
    let node = l["node"].as_str().ok_or("a link needs a \"node\"")?;
    let n = lookup(node).ok_or_else(|| format!("no node '{node}'"))?;
    let value = if let Some(what) = l["solve"].as_str() {
        if n.op != "camera_tracker" {
            return Err(format!("'{node}' isn't a Camera Tracker"));
        }
        crate::camtrack::solved_value(&n.params, what, t).map_err(|e| format!("'{node}': {e:#}"))?
    } else if let Some(what) = l["motion"].as_str() {
        let pivot = [l["pivot"][0].as_f64().unwrap_or(0.0) as f32, l["pivot"][1].as_f64().unwrap_or(0.0) as f32];
        let (translate, rotate, scale) = track::motion_at(&n.params, t, l["stabilize"].as_bool().unwrap_or(false), pivot);
        match what {
            "translate" => json!(translate),
            "rotate" => json!(rotate),
            "scale" => json!([scale, scale]),
            other => return Err(format!("unknown motion '{other}'")),
        }
    } else {
        let name = l["param"].as_str().ok_or("a link needs a \"param\"")?;
        let t = match l["at"].as_str() {
            Some("reference") => ops::params_with_defaults(&n.op, &n.params)["reference_frame"].as_f64().unwrap_or(1.0),
            _ => t,
        };
        read(n, name, t)?
    };
    Ok(adjust(value, &l["multiply"], &l["add"]))
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
        let gone = sources(v).into_iter().find(|n| lookup(n).is_none())?;
        Some((k.clone(), gone))
    })
}

/// Point links to node `old` at `new` instead (after a rename).
pub fn rename(params: &mut Value, old: &str, new: &str) {
    let Some(map) = params.as_object_mut() else { return };
    for v in map.values_mut() {
        if source(v) == Some(old) {
            v["link"]["node"] = json!(new);
        } else if let Some(f) = formula(v) {
            v["link"]["expr"] = json!(crate::expr::rename(f, old, new));
        }
    }
}

/// What a link reads, for the panel: "tracker1 · track1",
/// "tracker1 · track1 at the reference frame", "tracker1 · stabilize",
/// "camera_tracker1 · solved rotate".
pub fn describe(v: &Value) -> String {
    let l = &v["link"];
    let node = l["node"].as_str().unwrap_or("?");
    let what = if let Some(f) = l["expr"].as_str() {
        format!("= {f}")
    } else if let Some(what) = l["solve"].as_str() {
        format!("{node} · solved {what}")
    } else if l.get("motion").is_some() {
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
        let at = |node: &str, param: &str, t: f64| resolve(&lookup, &g.node(node).unwrap().params[param], t, Clock::default());
        assert_eq!(at("b", "center", 6.0), json!([50.0, 25.0]));
        assert_eq!(at("c", "center", 6.0), json!([50.0, 25.0]), "a link to a link");
        assert_eq!(resolve(&lookup, &at_reference("t", "track1"), 1.0, Clock::default()), json!([15.0, 25.0]), "at the reference frame");
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
        let at = |node: &str, param: &str, t: f64| resolve(&lookup, &g.node(node).unwrap().params[param], t, Clock::default());
        assert_eq!(at("b", "center", 6.0), json!([60.0, 25.0]));
        // The second track swings 90° about the first by frame 5: half is 45°.
        let full = resolve(&lookup, &motion("t", "rotate", false, [10.5, 10.5]), 5.0, Clock::default()).as_f64().unwrap();
        let half = at("half", "rotate", 5.0).as_f64().unwrap();
        assert!((full - 90.0).abs() < 1e-3 && (half - 45.0).abs() < 1e-3, "{full} {half}");
        // Removing them, and how they read in the panel.
        let l = with_adjust(&param("a", "center"), json!(0.5), json!([10, 0]));
        assert_eq!(describe(&l), "a · center × 0.5 + [10, 0]");
        assert_eq!(with_adjust(&l, Value::Null, Value::Null), param("a", "center"));
    }

    #[test]
    fn formula_links_read_several_nodes_and_rename() {
        let g = graph(json!([
            { "id": "a", "op": "rectangle", "params": { "center": { "keys": [[1, [0, 0]], [11, [100, 50]]], "interp": "linear" } } },
            { "id": "plate-2", "op": "rectangle", "params": { "center": [7, 3] } },
            { "id": "b", "op": "rectangle", "params": {
                "center": expr("a.center + [\"plate-2\".center.x, 0] + [wiggle(0.1, 0), 0]"),
                "softness": with_adjust(&expr("at(a.center.y, 1) + frame"), json!(2), Value::Null) } },
        ]));
        let lookup = |id: &str| g.node(id);
        let at = |param: &str, t: f64| resolve(&lookup, &g.node("b").unwrap().params[param], t, Clock::default());
        assert_eq!(at("center", 6.0), json!([57.0, 25.0]));
        assert_eq!(at("softness", 6.0), json!(12.0), "(0 + 6) × 2");
        assert_eq!(sources(&g.node("b").unwrap().params["center"]), ["a", "plate-2"]);
        assert_eq!(missing(&lookup, &json!({ "x": expr("a.center + gone.center") })), Some(("x".into(), "gone".into())));
        assert_eq!(error(&lookup, &expr("a.nope"), 1.0, Clock::default()), Some("'a' has no param 'nope'".into()));
        assert_eq!(error(&lookup, &expr("a.center +"), 1.0, Clock::default()), Some("unexpected end".into()));
        let mut p = g.node("b").unwrap().params.clone();
        rename(&mut p, "plate-2", "p2");
        rename(&mut p, "a", "src");
        assert_eq!(formula(&p["center"]), Some("src.center + [p2.center.x, 0] + [wiggle(0.1, 0), 0]"));
        assert_eq!(describe(&p["softness"]), "= at(src.center.y, 1) + frame × 2");
    }

    /// A formula's value is in the cache key, so editing it or its source
    /// re-renders; one that reads itself fails, naming the param.
    #[test]
    fn formula_links_rerender_on_change_and_report_loops() {
        let mut g = graph(json!([
            { "id": "a", "op": "constant", "params": { "color": [0.2, 0.2, 0.2, 1] } },
            { "id": "b", "op": "constant", "params": { "color": expr("a.color * [2, 1, 1, 1]") } },
        ]));
        let ctx = EvalContext { frame: 1, write_outputs: false };
        let key = |g: &Graph| Evaluator::key(g, "b", &ctx).unwrap();
        let k0 = key(&g);
        assert_eq!(key(&g), k0, "stable");
        let px = |g: &Graph| Evaluator::new().evaluate(g, "b", &ctx).unwrap().pixels[0];
        assert!((px(&g)[0] - 0.4).abs() < 1e-6);
        // Change the source…
        g.nodes[0].params["color"] = json!([0.3, 0.2, 0.2, 1]);
        let k1 = key(&g);
        assert_ne!(k1, k0);
        assert!((px(&g)[0] - 0.6).abs() < 1e-6);
        // …or the formula.
        g.nodes[1].params["color"] = expr("a.color * [3, 1, 1, 1]");
        assert_ne!(key(&g), k1);
        assert!((px(&g)[0] - 0.9).abs() < 1e-6);
        // Wobble is the same every time it's worked out.
        g.nodes[1].params["color"] = expr("a.color + noise(frame * 0.3, 4) * 0.1");
        assert_eq!(key(&g), key(&g));
        // Itself, directly or through another node.
        for (id, f) in [("b", "b.color + 1"), ("a", "b.color")] {
            let n = g.nodes.iter_mut().find(|n| n.id == id).unwrap();
            n.params["color"] = expr(f);
            if id == "b" {
                // Just b loops; next, b → a → b.
                let e = Evaluator::new().evaluate(&g, "b", &ctx).err().unwrap().to_string();
                assert!(e.contains("'color'") && e.contains("loops"), "{e}");
                g.nodes[1].params["color"] = expr("a.color");
            }
        }
        let e = Evaluator::new().evaluate(&g, "b", &ctx).err().unwrap().to_string();
        assert!(e.contains("'color'") && e.contains("loops"), "{e}");
    }

    /// A link giving one number for a list param fills every component.
    #[test]
    fn a_number_for_a_list_param_fills_every_component() {
        let g = graph(json!([
            { "id": "a", "op": "rectangle", "params": { "rotate": 0.25 } },
            { "id": "c", "op": "constant", "params": { "width": 2, "height": 2, "color": expr("0.5") } },
            { "id": "d", "op": "constant", "params": { "width": 2, "height": 2, "color": param("a", "rotate") } },
            { "id": "s", "op": "rectangle", "params": { "size": expr("frame * 10"), "rotate": expr("frame") } },
            { "id": "half", "op": "constant", "params": { "width": 2, "height": 2, "color": [0.5, 0.5, 0.5, 0.5] } },
            { "id": "quarter", "op": "constant", "params": { "width": 2, "height": 2, "color": [0.25, 0.25, 0.25, 0.25] } },
        ]));
        let ctx = EvalContext { frame: 3, write_outputs: false };
        let mut ev = Evaluator::new();
        let mut px = |id: &str| ev.evaluate(&g, id, &ctx).unwrap().pixels[0];
        assert_eq!(px("c"), px("half"));
        assert_eq!(px("d"), px("quarter"));
        let p = ops::resolve_params_linked("rectangle", &g.node("s").unwrap().params, 3, Some(&|v: &Value, t| resolve(&|id: &str| g.node(id), v, t, Clock::default())));
        assert_eq!((&p["size"], &p["rotate"]), (&json!([30.0, 30.0]), &json!(3.0)), "a number param stays one");
    }

    /// `time` and `fps` come from the graph's project.
    #[test]
    fn formulas_tell_time_from_the_project() {
        let mut g: Graph = serde_json::from_value(json!({
            "project": { "first_frame": 101, "last_frame": 200, "fps": 10 },
            "nodes": [{ "id": "c", "op": "constant", "params": { "width": 2, "height": 2, "color": expr("[time, fps / 100, 0, 1]") } }],
        }))
        .unwrap();
        let px = |g: &Graph, frame| Evaluator::new().evaluate(g, "c", &EvalContext { frame, write_outputs: false }).unwrap().pixels[0];
        let p = px(&g, 106);
        assert!((p[0] - 0.5).abs() < 1e-6 && (p[1] - 0.1).abs() < 1e-6, "{p:?}");
        g.project.fps = 5.0;
        assert!((px(&g, 106)[0] - 1.0).abs() < 1e-6, "a new rate re-renders");
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

    /// A Camera linked to a Camera Tracker's solve matches one keyed with a
    /// copy, between frames too (and so renders and blurs the same), and
    /// follows a re-solve that the copy misses.
    #[test]
    fn cameras_linked_to_a_solve_match_a_keyed_copy_and_follow_re_solves() {
        use crate::camtrack::{self, Solution};
        let mut sol = Solution {
            format: [64, 40],
            focal: 30.0,
            film_back: [36.0, 24.0],
            frames: vec![1, 3, 6],
            cameras: vec![[0.0, 0.0, 0.0, 0.0, 0.0, 0.0], [0.5, 0.1, -0.2, 1.0, 4.0, 0.0], [1.5, 0.2, -0.4, 2.0, 9.0, 1.0]],
            points: vec![[0.0, 0.0, -5.0]],
            rms: 0.1,
            frame_rms: vec![0.1; 3],
            point_rms: Vec::new(),
            pnp_frames: Vec::new(),
            tripod: false,
            k1: 0.0,
            k2: 0.0,
        };
        let mut g = graph(json!([
            { "id": "plate", "op": "checkerboard", "params": { "width": 64, "height": 40, "size": 4 } },
            { "id": "ct", "op": "camera_tracker", "inputs": ["plate"], "params": { "solution": sol.to_value() } },
            { "id": "live", "op": "camera", "params": camtrack::camera_links("ct") },
            { "id": "copy", "op": "camera", "params": camtrack::camera_params(&sol) },
            { "id": "card", "op": "card", "inputs": ["plate"], "params": { "translate": [0, 0, -5], "size": [4, 2.5] } },
        ]));
        for (cam, blur) in [("live", 0), ("copy", 0), ("live", 4), ("copy", 4)] {
            g.nodes.push(serde_json::from_value(json!({ "id": format!("set_{cam}{blur}"), "op": "scene", "inputs": ["card", cam] })).unwrap());
            let params = json!({ "width": 64, "height": 40, "samples": 1, "motion_blur": blur, "shutter": 1.0, "jitter": false });
            g.nodes.push(serde_json::from_value(json!({ "id": format!("r_{cam}{blur}"), "op": "scanline_render", "inputs": [format!("set_{cam}{blur}"), "", ""], "params": params })).unwrap());
        }
        let at = |g: &Graph, id: &str, what: &str, t: f64| {
            let v = resolve(&|n: &str| g.node(n), &g.node(id).unwrap().params[what], t, Clock::default());
            crate::anim::sample(&v, t)
        };
        for what in camtrack::SOLVED {
            for t in [0.0, 1.0, 2.0, 2.25, 3.0, 4.5, 6.0, 8.0] {
                let (a, b) = (at(&g, "live", what, t), at(&g, "copy", what, t));
                assert!(a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-12), "{what} at {t}: {a:?} vs {b:?}");
            }
        }
        assert_eq!(at(&g, "live", "translate", 2.0), vec![0.25, 0.05, -0.1]);
        assert_eq!(describe(&g.node("live").unwrap().params["rotate"]), "ct · solved rotate");
        let render = |g: &Graph, id: &str, frame: i64| Evaluator::new().evaluate(g, id, &EvalContext { frame, write_outputs: false }).unwrap().flat().into_owned();
        let diff = |a: &Image, b: &Image| a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
        for blur in [0, 4] {
            let (live, copy) = (render(&g, &format!("r_live{blur}"), 2), render(&g, &format!("r_copy{blur}"), 2));
            assert!(diff(&live, &copy) < 1e-5, "blur {blur}: {}", diff(&live, &copy));
        }
        let d = diff(&render(&g, "r_live0", 2), &render(&g, "r_live4", 2));
        assert!(d > 0.05, "the move blurs: {d}");
        // Reoriented (as Set origin does): the linked Camera follows.
        let before = render(&g, "r_live0", 3);
        sol.transform(1.0, &nalgebra::Rotation3::identity(), &crate::geom::Vec3::new(0.0, 0.3, 0.0));
        g.nodes[1].params["solution"] = sol.to_value();
        assert_eq!(at(&g, "live", "translate", 3.0), vec![0.5, 0.4, -0.2]);
        assert!(diff(&before, &render(&g, "r_live0", 3)) > 0.05, "the linked render moved");
        assert!(diff(&before, &render(&g, "r_copy0", 3)) < 1e-6, "the copy stays");
        // Baked, it's the copy's keys; tripod solves hold the camera still.
        let baked = camtrack::baked_value(&g.nodes[1].params, "rotate").unwrap();
        assert_eq!(baked, camtrack::camera_params(&sol)["rotate"]);
        sol.tripod = true;
        g.nodes[1].params["solution"] = sol.to_value();
        assert_eq!(at(&g, "live", "translate", 4.5), at(&g, "live", "translate", 1.0));
        // No solve, or not a Camera Tracker: a message, not a value.
        g.nodes[1].params = json!({});
        let e = error(&|n: &str| g.node(n), &g.node("live").unwrap().params["focal"], 1.0, Clock::default()).unwrap();
        assert!(e.contains("no solve yet"), "{e}");
        let e = error(&|n: &str| g.node(n), &solve("plate", "focal"), 1.0, Clock::default()).unwrap();
        assert!(e.contains("isn't a Camera Tracker"), "{e}");
    }
}
