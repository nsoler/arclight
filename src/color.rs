//! Pixel type and color-space helpers.
//!
//! Arclight works internally in **linear light, premultiplied (associated)
//! alpha**, 32-bit float per channel. sRGB and premultiply conversions live
//! here and are applied only at the 8-bit I/O boundary (e.g. PNG).

/// A single RGBA pixel in linear, premultiplied float.
pub type Rgba = [f32; 4];

pub const TRANSPARENT: Rgba = [0.0, 0.0, 0.0, 0.0];

/// sRGB electro-optical transfer: encoded [0,1] -> linear.
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer: linear -> encoded [0,1].
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Multiply color by alpha (straight/unassociated -> premultiplied).
#[inline]
pub fn premultiply(p: Rgba) -> Rgba {
    let a = p[3];
    [p[0] * a, p[1] * a, p[2] * a, a]
}

/// Divide color by alpha (premultiplied -> straight/unassociated).
///
/// Alpha of zero yields zero color rather than a division by zero.
#[inline]
pub fn unpremultiply(p: Rgba) -> Rgba {
    let a = p[3];
    if a > 0.0 {
        [p[0] / a, p[1] / a, p[2] / a, a]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    }
}
