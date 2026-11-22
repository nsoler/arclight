//! How a masked op reads its mask: one of the mask channels (alpha, red,
//! green, blue, luminance: `mask_channel`), or, when `mask_plane` names
//! one, a plane of the mask image (`matte.hero`, `depth`); clamped to 0–1
//! and flipped by `mask_invert`. Outside the mask's data window it reads
//! 0 (so inverted, 1), as masks always have.
//!
//! Without `mask_plane` (every project from before it) the value is worked
//! out exactly as it always was.

use crate::image::{Image, Plane};
use anyhow::{anyhow, Result};
use serde_json::Value;

/// A masked op's mask settings, from its params.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskSource<'a> {
    /// One of `ops::MASK_CHANNELS` (anything else reads as alpha).
    pub channel: &'a str,
    /// A plane of the mask image to read instead; None when unset.
    pub plane: Option<&'a str>,
    pub invert: bool,
}

impl<'a> MaskSource<'a> {
    pub fn from_params(params: &'a Value) -> MaskSource<'a> {
        MaskSource {
            channel: params.get("mask_channel").and_then(Value::as_str).unwrap_or("alpha"),
            plane: params.get("mask_plane").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()),
            invert: params.get("mask_invert").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Ready to read `image`; an error when it lacks the plane asked for.
    pub fn on<'i>(&self, image: &'i Image) -> Result<Mask<'i>> {
        let plane = match self.plane {
            Some(name) => Some(image.plane(name).ok_or_else(|| anyhow!("mask has no plane '{name}'"))?),
            None => None,
        };
        let channel = match self.channel {
            "red" => Channel::Red,
            "green" => Channel::Green,
            "blue" => Channel::Blue,
            "luminance" => Channel::Luminance,
            _ => Channel::Alpha,
        };
        Ok(Mask { image, plane, channel, invert: self.invert })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Channel {
    Alpha,
    Red,
    Green,
    Blue,
    Luminance,
}

/// A mask image and how to read it.
#[derive(Clone, Copy)]
pub struct Mask<'a> {
    image: &'a Image,
    plane: Option<&'a Plane>,
    channel: Channel,
    invert: bool,
}

impl Mask<'_> {
    /// How much the mask lets through at (x, y), 0–1.
    #[inline]
    pub fn at(&self, x: i32, y: i32) -> f32 {
        let v = match self.plane {
            Some(p) => self.image.plane_value(p, x, y),
            None => {
                let px = self.image.at(x, y);
                let luma = crate::ops::LUMA;
                match self.channel {
                    Channel::Red => px[0],
                    Channel::Green => px[1],
                    Channel::Blue => px[2],
                    Channel::Luminance => px[0] * luma[0] + px[1] * luma[1] + px[2] * luma[2],
                    Channel::Alpha => px[3],
                }
            }
        }
        .clamp(0.0, 1.0);
        if self.invert { 1.0 - v } else { v }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{IRect, Size};
    use serde_json::json;

    fn img() -> Image {
        let mut i = Image::blank(Size { width: 4, height: 1 }, IRect::new(1, 0, 3, 1));
        i.pixels = vec![[0.2, 0.4, 0.6, 0.8], [2.0, -1.0, 0.5, 0.25]];
        i.plane_mut("matte.hero").copy_from_slice(&[0.3, 1.5]);
        i
    }

    #[test]
    fn channels_read_as_before() {
        let i = img();
        let read = |p: Value, x| MaskSource::from_params(&p).on(&i).unwrap().at(x, 0);
        assert_eq!(read(json!({}), 1), 0.8);
        assert_eq!(read(json!({"mask_channel": "red"}), 2), 1.0, "clamped");
        assert_eq!(read(json!({"mask_channel": "green"}), 2), 0.0);
        assert_eq!(read(json!({"mask_channel": "blue", "mask_invert": true}), 1), 1.0 - 0.6f32);
        let l = read(json!({"mask_channel": "luminance"}), 1);
        assert_eq!(l.to_bits(), (0.2f32 * 0.2126 + 0.4 * 0.7152 + 0.6 * 0.0722).clamp(0.0, 1.0).to_bits());
        // Outside the window: empty, so inverted, full.
        assert_eq!((read(json!({}), 0), read(json!({"mask_invert": true}), 3)), (0.0, 1.0));
        // An empty plane name is no plane.
        assert_eq!(read(json!({"mask_plane": " "}), 1), 0.8);
    }

    #[test]
    fn planes_read_clamped_and_inverted() {
        let i = img();
        let read = |p: Value, x| MaskSource::from_params(&p).on(&i).unwrap().at(x, 0);
        assert_eq!(read(json!({"mask_plane": "matte.hero", "mask_channel": "red"}), 1), 0.3, "the plane wins");
        assert_eq!(read(json!({"mask_plane": "matte.hero"}), 2), 1.0);
        assert_eq!(read(json!({"mask_plane": "matte.hero", "mask_invert": true}), 0), 1.0);
        let e = MaskSource::from_params(&json!({"mask_plane": "depth"})).on(&i).err().unwrap().to_string();
        assert!(e.contains("mask has no plane 'depth'"), "{e}");
    }
}
