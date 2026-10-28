//! Keyframe animation of numeric parameters.
//!
//! A static parameter is a plain JSON value (`2.5`, `[1, 1, 1]`). An animated
//! one replaces it with a curve:
//!
//! ```json
//! { "keys": [[1, 0.0], [24, 12.5], [48, -4.0]], "interp": "smooth" }
//! ```
//!
//! Each key is `[frame, value]`, where the value has the same shape as the
//! static form (a number or an array of numbers). `interp` is `smooth`
//! (cubic, flat at the ends), `linear` or `step`. Before the first key and
//! after the last, the curve holds.

use serde_json::{json, Value};

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

/// A parsed curve. Values are flattened to `Vec<f64>`; `scalar` remembers
/// whether to write them back as a number or an array.
#[derive(Clone, Debug)]
struct Curve {
    keys: Vec<(i64, Vec<f64>)>,
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

impl Curve {
    fn parse(v: &Value) -> Option<Curve> {
        let obj = v.as_object()?;
        let mut scalar = true;
        let mut keys: Vec<(i64, Vec<f64>)> = obj
            .get("keys")?
            .as_array()?
            .iter()
            .filter_map(|k| {
                let k = k.as_array()?;
                let frame = k.first()?.as_f64()?.round() as i64;
                let (val, s) = flatten(k.get(1)?)?;
                scalar = s;
                Some((frame, val))
            })
            .collect();
        if keys.is_empty() {
            return None;
        }
        keys.sort_by_key(|k| k.0);
        keys.dedup_by_key(|k| k.0);
        let interp = Interp::parse(obj.get("interp").and_then(Value::as_str).unwrap_or("smooth"));
        Some(Curve { keys, interp, scalar })
    }

    fn to_value(&self) -> Value {
        let keys: Vec<Value> = self.keys.iter().map(|(f, v)| json!([f, unflatten(v, self.scalar)])).collect();
        json!({ "keys": keys, "interp": self.interp.name() })
    }

    fn sample(&self, frame: f64) -> Vec<f64> {
        let k = &self.keys;
        let n = k.len();
        if n == 1 || frame <= k[0].0 as f64 {
            return k[0].1.clone();
        }
        if frame >= k[n - 1].0 as f64 {
            return k[n - 1].1.clone();
        }
        // Segment [i, i+1] containing `frame`.
        let i = k.partition_point(|key| key.0 as f64 <= frame) - 1;
        let (t0, t1) = (k[i].0 as f64, k[i + 1].0 as f64);
        let u = (frame - t0) / (t1 - t0);
        let dims = k[i].1.len().min(k[i + 1].1.len());
        (0..dims)
            .map(|d| {
                let (p0, p1) = (k[i].1[d], k[i + 1].1[d]);
                match self.interp {
                    Interp::Step => p0,
                    Interp::Linear => p0 + (p1 - p0) * u,
                    Interp::Smooth => {
                        // Cubic Hermite. Interior tangents are the slope across
                        // the neighbouring keys (Catmull-Rom for uneven
                        // spacing); end keys are flat, so motion eases in/out.
                        let slope = |j: usize| -> f64 {
                            if j == 0 || j == n - 1 {
                                return 0.0;
                            }
                            let dt = (k[j + 1].0 - k[j - 1].0) as f64;
                            (k[j + 1].1[d] - k[j - 1].1[d]) / dt
                        };
                        let (m0, m1) = (slope(i) * (t1 - t0), slope(i + 1) * (t1 - t0));
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

/// Frames that carry a key, in order. Empty for static values.
pub fn key_frames(v: &Value) -> Vec<i64> {
    Curve::parse(v).map(|c| c.keys.iter().map(|k| k.0).collect()).unwrap_or_default()
}

pub fn has_key_at(v: &Value, frame: i64) -> bool {
    key_frames(v).contains(&frame)
}

/// Set a key at `frame`. A static parameter becomes animated, keyed at this
/// frame only.
pub fn set_key(v: &Value, frame: i64, value: &Value) -> Value {
    let Some((val, scalar)) = flatten(value) else { return v.clone() };
    let mut c = Curve::parse(v).unwrap_or(Curve { keys: Vec::new(), interp: Interp::Smooth, scalar });
    match c.keys.binary_search_by_key(&frame, |k| k.0) {
        Ok(i) => c.keys[i].1 = val,
        Err(i) => c.keys.insert(i, (frame, val)),
    }
    c.to_value()
}

/// Remove the key at `frame`. Removing the last key turns the parameter back
/// into a static value (that key's value).
pub fn remove_key(v: &Value, frame: i64) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    let Ok(i) = c.keys.binary_search_by_key(&frame, |k| k.0) else { return v.clone() };
    let removed = c.keys.remove(i);
    if c.keys.is_empty() {
        unflatten(&removed.1, c.scalar)
    } else {
        c.to_value()
    }
}

/// One key with its value flattened to numbers (one per channel).
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub frame: i64,
    pub value: Vec<f64>,
}

/// The keys of an animated value, in frame order. Empty for static values.
pub fn keys(v: &Value) -> Vec<Key> {
    Curve::parse(v)
        .map(|c| c.keys.into_iter().map(|(frame, value)| Key { frame, value }).collect())
        .unwrap_or_default()
}

/// Replace an animated value's keys, keeping its interpolation and shape.
/// Keys are sorted; on duplicate frames the later one in `keys` wins. Static
/// values, or an empty `keys`, are returned unchanged.
pub fn replace_keys(v: &Value, keys: Vec<Key>) -> Value {
    let Some(mut c) = Curve::parse(v) else { return v.clone() };
    if keys.is_empty() {
        return v.clone();
    }
    let mut ks: Vec<(i64, Vec<f64>)> = keys.into_iter().map(|k| (k.frame, k.value)).collect();
    ks.reverse();
    ks.sort_by_key(|k| k.0); // stable: for equal frames the later input stays first
    ks.dedup_by_key(|k| k.0);
    c.keys = ks;
    c.to_value()
}

/// The value at `frame`, flattened to one number per channel.
pub fn sample(v: &Value, frame: f64) -> Vec<f64> {
    match Curve::parse(v) {
        Some(c) => c.sample(frame),
        None => flatten(v).map(|(x, _)| x).unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
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
    fn replace_keys_sorts_and_later_wins() {
        let c = json!({ "keys": [[1, 0.0], [10, 1.0]], "interp": "linear" });
        let mut ks = keys(&c);
        ks[0].frame = 20; // move frame 1 past frame 10
        ks.push(Key { frame: 10, value: vec![5.0] }); // collides with the existing key 10
        let v = replace_keys(&c, ks);
        assert_eq!(key_frames(&v), vec![10, 20]);
        assert_eq!(sample(&v, 10.0), vec![5.0]);
        assert_eq!(interp(&v), Some(Interp::Linear));
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
}
