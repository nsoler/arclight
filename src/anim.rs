//! Keyframe animation of numeric parameters.
//!
//! A static parameter is a plain JSON value (`2.5`, `[1, 1, 1]`). An animated
//! one replaces it with a curve:
//!
//! ```json
//! { "keys": [[1, 0.0], [24, 12.5], [48, -4.0, { "in": 0.0, "out": 0.5 }]],
//!   "interp": "smooth" }
//! ```
//!
//! Each key is `[frame, value]` or `[frame, value, tangents]`, where the value
//! has the same shape as the static form (a number or an array of numbers).
//! `interp` is `smooth` (cubic Hermite), `linear` or `step`. Before the first
//! key and after the last, the curve holds.
//!
//! Tangents only affect `smooth` curves. They are slopes in value units per
//! frame, shaped like the value, with `null` (or absence) meaning *auto*: the
//! slope across the neighbouring keys, flat at the first and last key. A key's
//! in- and out-tangents move together unless the key is `"broken": true`.

use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Smooth,
    Linear,
    Step,
}

impl Interp {
    pub const ALL: [(Interp, &'static str); 3] =
        [(Interp::Smooth, "smooth"), (Interp::Linear, "linear"), (Interp::Step, "step")];

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(i, _)| *i == self).unwrap().1
    }

    fn parse(s: &str) -> Interp {
        Self::ALL.iter().find(|(_, n)| *n == s).map_or(Interp::Smooth, |(i, _)| *i)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    In,
    Out,
}

/// One key, with its value flattened to numbers (one per channel).
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub frame: i64,
    pub value: Vec<f64>,
    /// Per-channel user slopes; `None` = auto.
    pub tan_in: Vec<Option<f64>>,
    pub tan_out: Vec<Option<f64>>,
    /// In- and out-tangents are edited independently.
    pub broken: bool,
}

impl Key {
    pub fn new(frame: i64, value: Vec<f64>) -> Key {
        let n = value.len();
        Key { frame, value, tan_in: vec![None; n], tan_out: vec![None; n], broken: false }
    }

    fn has_tangents(&self) -> bool {
        self.broken || self.tan_in.iter().chain(&self.tan_out).any(Option::is_some)
    }

    fn tangent_mut(&mut self, side: Side) -> &mut Vec<Option<f64>> {
        let n = self.value.len();
        let t = match side {
            Side::In => &mut self.tan_in,
            Side::Out => &mut self.tan_out,
        };
        t.resize(n, None);
        t
    }
}

/// A parsed curve. `scalar` remembers whether values are written back as a
/// number or an array.
#[derive(Clone, Debug)]
struct Curve {
    keys: Vec<Key>,
    interp: Interp,
    scalar: bool,
}

fn flatten(v: &Value) -> Option<(Vec<f64>, bool)> {
    match v {
        Value::Number(n) => Some((vec![n.as_f64()?], true)),
        Value::Array(a) => a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>().map(|x| (x, false)),
        _ => None,
    }
}

fn unflatten(x: &[f64], scalar: bool) -> Value {
    if scalar {
        json!(x[0])
    } else {
        json!(x)
    }
}

/// Tangents as stored: a number/array shaped like the value, `null` = auto.
fn parse_tangent(v: Option<&Value>, n: usize) -> Vec<Option<f64>> {
    match v {
        Some(Value::Number(x)) => vec![x.as_f64(); n],
        Some(Value::Array(a)) => (0..n).map(|i| a.get(i).and_then(Value::as_f64)).collect(),
        _ => vec![None; n],
    }
}

fn tangent_value(t: &[Option<f64>], scalar: bool) -> Value {
    if scalar {
        json!(t.first().copied().flatten())
    } else {
        json!(t)
    }
}

impl Curve {
    fn parse(v: &Value) -> Option<Curve> {
        let obj = v.as_object()?;
        let mut scalar = true;
        let mut keys: Vec<Key> = obj
            .get("keys")?
            .as_array()?
            .iter()
            .filter_map(|k| {
                let k = k.as_array()?;
                let frame = k.first()?.as_f64()?.round() as i64;
                let (value, s) = flatten(k.get(1)?)?;
                scalar = s;
                let n = value.len();
                let t = k.get(2);
                Some(Key {
                    frame,
                    value,
                    tan_in: parse_tangent(t.and_then(|t| t.get("in")), n),
                    tan_out: parse_tangent(t.and_then(|t| t.get("out")), n),
                    broken: t.and_then(|t| t.get("broken")).and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        if keys.is_empty() {
            return None;
        }
        keys.sort_by_key(|k| k.frame);
        keys.dedup_by_key(|k| k.frame);
        let interp = Interp::parse(obj.get("interp").and_then(Value::as_str).unwrap_or("smooth"));
        Some(Curve { keys, interp, scalar })
    }

    fn to_value(&self) -> Value {
        let keys: Vec<Value> = self
            .keys
            .iter()
            .map(|k| {
                let v = unflatten(&k.value, self.scalar);
                if !k.has_tangents() {
                    return json!([k.frame, v]);
                }
                let mut t = Map::new();
                if k.tan_in.iter().any(Option::is_some) {
                    t.insert("in".into(), tangent_value(&k.tan_in, self.scalar));
                }
                if k.tan_out.iter().any(Option::is_some) {
                    t.insert("out".into(), tangent_value(&k.tan_out, self.scalar));
                }
                if k.broken {
                    t.insert("broken".into(), json!(true));
                }
                json!([k.frame, v, t])
            })
            .collect();
        json!({ "keys": keys, "interp": self.interp.name() })
    }

    /// Automatic slope of key `j`, channel `d`: across the neighbours
    /// (Catmull-Rom for uneven spacing), flat at the ends so motion eases.
    fn auto_slope(&self, j: usize, d: usize) -> f64 {
        let k = &self.keys;
        if j == 0 || j + 1 >= k.len() {
            return 0.0;
        }
        let dt = (k[j + 1].frame - k[j - 1].frame) as f64;
        let (a, b) = (k[j - 1].value.get(d), k[j + 1].value.get(d));
        match (a, b) {
            (Some(a), Some(b)) => (b - a) / dt,
            _ => 0.0,
        }
    }

    /// Effective slope of key `j`, channel `d`, on one side.
    fn slope(&self, j: usize, d: usize, side: Side) -> f64 {
        let k = &self.keys[j];
        let t = match side {
            Side::In => &k.tan_in,
            Side::Out => &k.tan_out,
        };
        t.get(d).copied().flatten().unwrap_or_else(|| self.auto_slope(j, d))
    }

    fn sample(&self, frame: f64) -> Vec<f64> {
        let k = &self.keys;
        let n = k.len();
        if n == 1 || frame <= k[0].frame as f64 {
            return k[0].value.clone();
        }
        if frame >= k[n - 1].frame as f64 {
            return k[n - 1].value.clone();
        }
        // Segment [i, i+1] containing `frame`.
        let i = k.partition_point(|key| key.frame as f64 <= frame) - 1;
        let (t0, t1) = (k[i].frame as f64, k[i + 1].frame as f64);
        let u = (frame - t0) / (t1 - t0);
        let dims = k[i].value.len().min(k[i + 1].value.len());
        (0..dims)
            .map(|d| {
                let (p0, p1) = (k[i].value[d], k[i + 1].value[d]);
                match self.interp {
                    Interp::Step => p0,
                    Interp::Linear => p0 + (p1 - p0) * u,
                    Interp::Smooth => {
                        // Cubic Hermite, slopes scaled to the segment length.
                        let m0 = self.slope(i, d, Side::Out) * (t1 - t0);
                        let m1 = self.slope(i + 1, d, Side::In) * (t1 - t0);
                        let (u2, u3) = (u * u, u * u * u);
                        (2.0 * u3 - 3.0 * u2 + 1.0) * p0
                            + (u3 - 2.0 * u2 + u) * m0
                            + (-2.0 * u3 + 3.0 * u2) * p1
                            + (u3 - u2) * m1
                    }
                }
            })
            .collect()
    }

    fn index_of(&self, frame: i64) -> Option<usize> {
        self.keys.binary_search_by_key(&frame, |k| k.frame).ok()
    }
}

/// Apply `f` to the key at `frame`, if `v` is a curve with such a key.
fn edit_key(v: &Value, frame: i64, f: impl FnOnce(&mut Key)) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    let Some(i) = c.index_of(frame) else { return v.clone() };
    f(&mut c.keys[i]);
    c.to_value()
}

pub fn is_animated(v: &Value) -> bool {
    Curve::parse(v).is_some()
}

/// The value of a (possibly animated) parameter at `frame`.
pub fn evaluate(v: &Value, frame: f64) -> Value {
    match Curve::parse(v) {
        Some(c) => unflatten(&c.sample(frame), c.scalar),
        None => v.clone(),
    }
}

/// The value at `frame`, flattened to one number per channel.
pub fn sample(v: &Value, frame: f64) -> Vec<f64> {
    match Curve::parse(v) {
        Some(c) => c.sample(frame),
        None => flatten(v).map(|(x, _)| x).unwrap_or_default(),
    }
}

/// Frames that carry a key, in order. Empty for static values.
pub fn key_frames(v: &Value) -> Vec<i64> {
    Curve::parse(v).map(|c| c.keys.iter().map(|k| k.frame).collect()).unwrap_or_default()
}

pub fn has_key_at(v: &Value, frame: i64) -> bool {
    key_frames(v).contains(&frame)
}

/// The keys of an animated value, in frame order. Empty for static values.
pub fn keys(v: &Value) -> Vec<Key> {
    Curve::parse(v).map(|c| c.keys).unwrap_or_default()
}

/// Replace an animated value's keys, keeping its interpolation and shape.
/// Keys are sorted; on duplicate frames the later one in `keys` wins. Static
/// values, or an empty `keys`, are returned unchanged.
pub fn replace_keys(v: &Value, mut keys: Vec<Key>) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    if keys.is_empty() {
        return v.clone();
    }
    keys.reverse();
    keys.sort_by_key(|k| k.frame); // stable: for equal frames the later input stays first
    keys.dedup_by_key(|k| k.frame);
    c.keys = keys;
    c.to_value()
}

/// Apply a structural edit (e.g. inserting or removing points) to a static
/// array value, or to every key of an animated one, so all keys keep the
/// same layout. Edited keys go back to auto tangents.
pub fn edit_all(v: &Value, f: impl Fn(&[f64]) -> Vec<f64>) -> Value {
    match Curve::parse(v) {
        Some(mut c) => {
            for k in &mut c.keys {
                *k = Key::new(k.frame, f(&k.value));
            }
            c.to_value()
        }
        None => match flatten(v) {
            Some((x, false)) => json!(f(&x)),
            _ => v.clone(),
        },
    }
}

/// Set a key at `frame`. A static parameter becomes animated, keyed at this
/// frame only. Re-keying an existing frame keeps that key's tangents.
pub fn set_key(v: &Value, frame: i64, value: &Value) -> Value {
    let Some((val, scalar)) = flatten(value) else { return v.clone() };
    let mut c = Curve::parse(v).unwrap_or(Curve { keys: Vec::new(), interp: Interp::Smooth, scalar });
    match c.keys.binary_search_by_key(&frame, |k| k.frame) {
        Ok(i) => c.keys[i].value = val,
        Err(i) => c.keys.insert(i, Key::new(frame, val)),
    }
    c.to_value()
}

/// Remove the key at `frame`. Removing the last key turns the parameter back
/// into a static value (that key's value).
pub fn remove_key(v: &Value, frame: i64) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    let Some(i) = c.index_of(frame) else { return v.clone() };
    let removed = c.keys.remove(i);
    if c.keys.is_empty() {
        unflatten(&removed.value, c.scalar)
    } else {
        c.to_value()
    }
}

/// Remove every key whose frame `drop` picks. When none remain, the
/// parameter keeps its value at frame `hold` as a static value.
pub fn remove_keys(v: &Value, drop: impl Fn(i64) -> bool, hold: f64) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    let held = c.sample(hold);
    c.keys.retain(|k| !drop(k.frame));
    if c.keys.is_empty() {
        unflatten(&held, c.scalar)
    } else {
        c.to_value()
    }
}

pub fn interp(v: &Value) -> Option<Interp> {
    Curve::parse(v).map(|c| c.interp)
}

pub fn set_interp(v: &Value, interp: Interp) -> Value {
    match Curve::parse(v) {
        Some(mut c) => {
            c.interp = interp;
            c.to_value()
        }
        None => v.clone(),
    }
}

/// Effective (in, out) slopes of the key at `frame`, per channel, with auto
/// tangents resolved. `None` if there is no key there.
pub fn slopes(v: &Value, frame: i64) -> Option<(Vec<f64>, Vec<f64>)> {
    let c = Curve::parse(v)?;
    let j = c.index_of(frame)?;
    let n = c.keys[j].value.len();
    Some((
        (0..n).map(|d| c.slope(j, d, Side::In)).collect(),
        (0..n).map(|d| c.slope(j, d, Side::Out)).collect(),
    ))
}

pub fn is_broken(v: &Value, frame: i64) -> bool {
    Curve::parse(v).and_then(|c| c.index_of(frame).map(|j| c.keys[j].broken)).unwrap_or(false)
}

/// Set one side's slope for a channel of the key at `frame`. Unless the key
/// is broken, the other side gets the same slope.
pub fn set_tangent(v: &Value, frame: i64, channel: usize, side: Side, slope: f64) -> Value {
    edit_key(v, frame, |k| {
        let other = if side == Side::In { Side::Out } else { Side::In };
        let unify = !k.broken;
        if let Some(t) = k.tangent_mut(side).get_mut(channel) {
            *t = Some(slope);
        }
        if unify {
            if let Some(t) = k.tangent_mut(other).get_mut(channel) {
                *t = Some(slope);
            }
        }
    })
}

/// Return a channel of the key at `frame` to automatic tangents.
pub fn set_auto_tangent(v: &Value, frame: i64, channel: usize) -> Value {
    edit_key(v, frame, |k| {
        for side in [Side::In, Side::Out] {
            if let Some(t) = k.tangent_mut(side).get_mut(channel) {
                *t = None;
            }
        }
    })
}

/// Break or re-unify the key at `frame`. Unifying sets each channel's
/// in-tangent to its out-tangent, auto or not, resolved as it was.
pub fn set_broken(v: &Value, frame: i64, broken: bool) -> Value {
    let Some(c) = Curve::parse(v) else { return v.clone() };
    let Some(j) = c.index_of(frame) else { return v.clone() };
    let out: Vec<f64> = (0..c.keys[j].value.len()).map(|d| c.slope(j, d, Side::Out)).collect();
    let had_user_out = c.keys[j].tan_out.clone();
    edit_key(v, frame, |k| {
        k.broken = broken;
        if !broken {
            // Keep auto channels auto; pin user channels to the out slope.
            for (d, slope) in out.iter().enumerate() {
                let user = had_user_out.get(d).copied().flatten().is_some()
                    || k.tan_in.get(d).copied().flatten().is_some();
                let v = user.then_some(*slope);
                k.tangent_mut(Side::In)[d] = v;
                k.tangent_mut(Side::Out)[d] = v;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn removing_keys_keeps_the_rest_or_holds_the_value() {
        let c = json!({ "keys": [[1, 0.0], [5, 4.0], [9, 8.0]], "interp": "linear" });
        assert_eq!(key_frames(&remove_keys(&c, |f| f > 5, 5.0)), [1, 5]);
        // Nothing left: the value at the hold frame stays.
        assert_eq!(remove_keys(&c, |_| true, 3.0), json!(2.0));
        assert_eq!(remove_keys(&json!(7.0), |_| true, 3.0), json!(7.0));
    }

    #[test]
    fn static_values_pass_through() {
        assert_eq!(evaluate(&json!(3.0), 10.0), json!(3.0));
        assert_eq!(evaluate(&json!([1, 2]), 10.0), json!([1, 2]));
        assert!(!is_animated(&json!("over")));
    }

    #[test]
    fn linear_and_step_and_hold() {
        let c = json!({ "keys": [[10, 0.0], [20, 10.0]], "interp": "linear" });
        assert!(approx(evaluate(&c, 15.0).as_f64().unwrap(), 5.0));
        assert!(approx(evaluate(&c, 0.0).as_f64().unwrap(), 0.0)); // hold before
        assert!(approx(evaluate(&c, 99.0).as_f64().unwrap(), 10.0)); // hold after
        let s = set_interp(&c, Interp::Step);
        assert!(approx(evaluate(&s, 19.0).as_f64().unwrap(), 0.0));
    }

    #[test]
    fn smooth_hits_keys_and_eases() {
        let c = json!({ "keys": [[0, [0.0, 0.0]], [10, [10.0, -10.0]]] });
        assert_eq!(evaluate(&c, 0.0), json!([0.0, 0.0]));
        assert_eq!(evaluate(&c, 10.0), json!([10.0, -10.0]));
        let mid = evaluate(&c, 5.0);
        assert!(approx(mid[0].as_f64().unwrap(), 5.0));
        // Flat ends: early motion is slower than linear.
        assert!(evaluate(&c, 1.0)[0].as_f64().unwrap() < 1.0);
    }

    #[test]
    fn keying_round_trip() {
        let v = set_key(&json!(1.0), 5, &json!(2.0));
        assert!(is_animated(&v));
        let v = set_key(&v, 15, &json!(4.0));
        assert_eq!(key_frames(&v), vec![5, 15]);
        let v = set_key(&v, 5, &json!(3.0)); // replace, not duplicate
        assert_eq!(key_frames(&v), vec![5, 15]);
        let v = remove_key(&v, 15);
        let v = remove_key(&v, 5);
        assert_eq!(v, json!(3.0));
    }

    #[test]
    fn replace_keys_sorts_and_later_wins() {
        let c = json!({ "keys": [[1, 0.0], [10, 1.0]], "interp": "linear" });
        let mut ks = keys(&c);
        ks[0].frame = 20; // move frame 1 past frame 10
        ks.push(Key::new(10, vec![5.0])); // collides with the existing key 10
        let v = replace_keys(&c, ks);
        assert_eq!(key_frames(&v), vec![10, 20]);
        assert_eq!(sample(&v, 10.0), vec![5.0]);
        assert_eq!(interp(&v), Some(Interp::Linear));
    }

    #[test]
    fn user_tangents_shape_the_curve() {
        // Two flat keys: auto tangents give a flat line.
        let c = json!({ "keys": [[0, 0.0], [10, 0.0]] });
        assert!(approx(sample(&c, 5.0)[0], 0.0));
        // An out-slope of 1/frame on the first key lifts the start of the segment.
        let t = set_tangent(&c, 0, 0, Side::Out, 1.0);
        assert!(sample(&t, 2.0)[0] > 0.5);
        // Unified by default: the in-side got the same slope.
        assert_eq!(slopes(&t, 0), Some((vec![1.0], vec![1.0])));
        // Stored on the key, and survives a JSON round trip and re-keying.
        assert_eq!(t["keys"][0][2]["out"], json!(1.0));
        let t = set_key(&t, 0, &json!(0.0));
        assert_eq!(slopes(&t, 0).unwrap().1, vec![1.0]);
        // Back to auto.
        let a = set_auto_tangent(&t, 0, 0);
        assert_eq!(a["keys"], c["keys"]);
    }

    #[test]
    fn broken_tangents_are_independent_until_unified() {
        let c = json!({ "keys": [[0, [0.0, 0.0]], [10, [5.0, 5.0]], [20, [0.0, 0.0]]] });
        let b = set_broken(&c, 10, true);
        assert!(is_broken(&b, 10));
        let b = set_tangent(&b, 10, 0, Side::In, 2.0);
        let b = set_tangent(&b, 10, 0, Side::Out, -2.0);
        let (i, o) = slopes(&b, 10).unwrap();
        assert_eq!((i[0], o[0]), (2.0, -2.0));
        assert_eq!((i[1], o[1]), (0.0, 0.0)); // channel 1 untouched, auto
        let u = set_broken(&b, 10, false);
        let (i, o) = slopes(&u, 10).unwrap();
        assert_eq!(i[0], o[0]);
        assert_eq!(o[0], -2.0);
    }
}
