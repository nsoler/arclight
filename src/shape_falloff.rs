//! Feather falloff for Roto: how coverage runs across a shape's soft edge.
//!
//! The coverage routines ([`crate::shape::coverage`],
//! [`crate::shape_feather::coverage`]) find, for each pixel near the
//! outline, how far across the soft edge it sits: `t`, 0 at the edge's
//! outer end, 0.5 on the outline, 1 at its inner end. [`Falloff::ramp`]
//! turns that into coverage:
//!
//! - the curve (`feather_falloff`): "smooth" (smoothstep, the default and
//!   what every shape has always used), "linear", "ease in" (`t²`: slow to
//!   start outside, quick to fill inside) or "ease out" (`1 − (1 − t)²`);
//! - the strength (`feather_strength`, default 1): coverage raised to
//!   `1 / strength`, so above 1 the soft edge carries further out and
//!   below 1 it pulls in toward the solid core;
//! - the cut-off (`feather_cutoff`, 0–1, default 0): coverage below it
//!   becomes 0 and the rest is stretched back over 0–1, a hard outer limit
//!   to the feather.
//!
//! The defaults give exactly the old smoothstep (the same expression), so
//! old projects render bit for bit as before. Edges narrower than a pixel
//! (the anti-aliasing ramp) always use the plain smoothstep, so a cut-off
//! never makes an unfeathered edge jagged.

use serde_json::Value;

/// The shape of the coverage ramp across the soft edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Curve {
    #[default]
    Smooth,
    Linear,
    EaseIn,
    EaseOut,
}

/// The `feather_falloff` choices, in menu order.
pub const CURVES: &[&str] = &["smooth", "linear", "ease in", "ease out"];

impl Curve {
    pub fn from_name(s: &str) -> Curve {
        match s {
            "linear" => Curve::Linear,
            "ease in" => Curve::EaseIn,
            "ease out" => Curve::EaseOut,
            _ => Curve::Smooth,
        }
    }
}

/// A shape's feather falloff (see the module notes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Falloff {
    pub curve: Curve,
    pub strength: f32,
    pub cutoff: f32,
}

impl Default for Falloff {
    fn default() -> Self {
        Falloff { curve: Curve::Smooth, strength: 1.0, cutoff: 0.0 }
    }
}

/// The plain smoothstep of `t` (already clamped to 0–1): the soft edge
/// every shape had before falloff controls, and the anti-aliasing ramp.
#[inline]
pub fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

impl Falloff {
    /// The falloff a shape's resolved params ask for; missing or odd values
    /// read as the defaults (so every op without these params, and every
    /// old project, gets the plain smoothstep).
    pub fn from_params(params: &Value) -> Falloff {
        let num = |name: &str, default: f32| params.get(name).and_then(Value::as_f64).map(|v| v as f32).filter(|v| v.is_finite()).unwrap_or(default);
        Falloff {
            curve: Curve::from_name(params.get("feather_falloff").and_then(Value::as_str).unwrap_or("smooth")),
            strength: num("feather_strength", 1.0).clamp(0.01, 100.0),
            cutoff: num("feather_cutoff", 0.0).clamp(0.0, 0.999),
        }
    }

    /// Whether this is the plain smoothstep.
    pub fn is_default(&self) -> bool {
        *self == Falloff::default()
    }

    /// Coverage at `t` (0–1, already clamped) across a soft edge `width`
    /// pixels wide.
    #[inline]
    pub fn ramp(&self, t: f32, width: f32) -> f32 {
        if width <= 1.0 || self.is_default() {
            return smoothstep(t);
        }
        let c = match self.curve {
            Curve::Smooth => smoothstep(t),
            Curve::Linear => t,
            Curve::EaseIn => t * t,
            Curve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        };
        let c = if self.strength != 1.0 { c.powf(1.0 / self.strength) } else { c };
        if self.cutoff > 0.0 { ((c - self.cutoff) / (1.0 - self.cutoff)).max(0.0) } else { c }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_the_plain_smoothstep_bit_for_bit() {
        let f = Falloff::from_params(&json!({}));
        assert!(f.is_default());
        for i in 0..=1000 {
            let t = i as f32 / 1000.0;
            assert_eq!(f.ramp(t, 40.0).to_bits(), (t * t * (3.0 - 2.0 * t)).to_bits());
        }
        let explicit = json!({"feather_falloff": "smooth", "feather_strength": 1, "feather_cutoff": 0});
        assert!(Falloff::from_params(&explicit).is_default());
    }

    #[test]
    fn curves_follow_their_formulas() {
        let f = |curve| Falloff { curve, ..Falloff::default() };
        for t in [0.0, 0.2, 0.5, 0.8, 1.0f32] {
            assert_eq!(f(Curve::Linear).ramp(t, 10.0), t);
            assert_eq!(f(Curve::EaseIn).ramp(t, 10.0), t * t);
            assert!((f(Curve::EaseOut).ramp(t, 10.0) - (1.0 - (1.0 - t).powi(2))).abs() < 1e-6);
        }
    }

    #[test]
    fn strength_spreads_and_cutoff_clips() {
        let strong = Falloff { strength: 2.0, ..Falloff::default() };
        let weak = Falloff { strength: 0.5, ..Falloff::default() };
        let t = 0.3;
        assert!(strong.ramp(t, 10.0) > smoothstep(t) && weak.ramp(t, 10.0) < smoothstep(t));
        let cut = Falloff { cutoff: 0.5, ..Falloff::default() };
        assert_eq!(cut.ramp(0.2, 10.0), 0.0);
        assert_eq!(cut.ramp(1.0, 10.0), 1.0);
        assert!((cut.ramp(0.5, 10.0) - 0.0).abs() < 1e-6, "smoothstep(0.5) = 0.5 sits on the cut-off");
        // Ends stay put for every setting.
        for f in [strong, weak, cut, Falloff { curve: Curve::EaseIn, strength: 3.0, cutoff: 0.2 }] {
            assert_eq!((f.ramp(0.0, 10.0), f.ramp(1.0, 10.0)), (0.0, 1.0));
        }
    }

    #[test]
    fn the_antialiasing_ramp_is_left_alone() {
        let f = Falloff { curve: Curve::Linear, strength: 4.0, cutoff: 0.7 };
        assert_eq!(f.ramp(0.3, 1.0), smoothstep(0.3));
    }

    #[test]
    fn odd_values_read_safely() {
        let f = Falloff::from_params(&json!({"feather_falloff": "nope", "feather_strength": -3, "feather_cutoff": 5}));
        assert_eq!(f.curve, Curve::Smooth);
        assert!(f.strength > 0.0 && f.cutoff < 1.0);
    }
}
