//! The core raster type and file I/O.

use crate::color::{self, Rgba};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::Path;

/// A frame size in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    pub width: usize,
    pub height: usize,
}

/// A rectangle of pixels in frame coordinates (y down), half-open:
/// `x0 <= x < x1`, `y0 <= y < y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> IRect {
        IRect { x0, y0, x1: x1.max(x0), y1: y1.max(y0) }
    }

    /// The whole frame of `size`.
    pub fn frame(size: Size) -> IRect {
        IRect::new(0, 0, size.width as i32, size.height as i32)
    }

    pub fn width(&self) -> usize {
        (self.x1 - self.x0).max(0) as usize
    }

    pub fn height(&self) -> usize {
        (self.y1 - self.y0).max(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// The smallest rectangle holding both (an empty one adds nothing).
    pub fn union(&self, o: &IRect) -> IRect {
        match (self.is_empty(), o.is_empty()) {
            (true, _) => *o,
            (_, true) => *self,
            _ => IRect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1)),
        }
    }

    pub fn intersect(&self, o: &IRect) -> IRect {
        IRect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }

    /// Grown by `n` pixels on every side (shrunk when negative).
    pub fn expand(&self, n: i32) -> IRect {
        IRect::new(self.x0 - n, self.y0 - n, self.x1 + n, self.y1 + n)
    }
}

/// An RGBA float image, linear + premultiplied: a frame (`format`) and the
/// pixels stored for part of it (the data window), which may also extend
/// past the frame. Everything outside the data window is transparent.
#[derive(Clone)]
pub struct Image {
    /// The frame: what a viewer shows and a file saves, from (0, 0).
    pub format: Size,
    /// The data window's top-left corner, in frame pixels.
    pub x0: i32,
    pub y0: i32,
    /// The data window's size: what `pixels` holds.
    pub width: usize,
    pub height: usize,
    /// `width * height` pixels, row by row from the window's top-left.
    pub pixels: Vec<Rgba>,
}

impl Image {
    /// A transparent image whose data window is its whole frame.
    pub fn new(width: usize, height: usize) -> Self {
        Image::filled(width, height, color::TRANSPARENT)
    }

    pub fn filled(width: usize, height: usize, value: Rgba) -> Self {
        Image { format: Size { width, height }, x0: 0, y0: 0, width, height, pixels: vec![value; width * height] }
    }

    /// A transparent image of frame `format` with pixels stored for `window`.
    pub fn blank(format: Size, window: IRect) -> Self {
        let (width, height) = (window.width(), window.height());
        Image { format, x0: window.x0, y0: window.y0, width, height, pixels: vec![color::TRANSPARENT; width * height] }
    }

    /// Where the stored pixels lie in the frame.
    pub fn window(&self) -> IRect {
        IRect::new(self.x0, self.y0, self.x0 + self.width as i32, self.y0 + self.height as i32)
    }

    /// The whole frame, as a rectangle.
    pub fn frame(&self) -> IRect {
        IRect::frame(self.format)
    }

    /// A stored pixel, by its position in the data window (not the frame).
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Rgba {
        self.pixels[y * self.width + x]
    }

    /// Set a stored pixel, by its position in the data window.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: Rgba) {
        self.pixels[y * self.width + x] = v;
    }

    /// The pixel at frame position (x, y): transparent outside the window.
    #[inline]
    pub fn at(&self, x: i32, y: i32) -> Rgba {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx >= self.width as i32 || ly >= self.height as i32 {
            color::TRANSPARENT
        } else {
            self.get(lx as usize, ly as usize)
        }
    }

    /// The frame alone as a plain buffer (window = frame): borrowed when it
    /// already is, else a copy that drops overscan and fills gaps with
    /// transparent. For code that wants `width × height` from (0, 0).
    pub fn flat(&self) -> Cow<'_, Image> {
        if self.window() == self.frame() {
            return Cow::Borrowed(self);
        }
        let mut out = Image::new(self.format.width, self.format.height);
        let w = out.width;
        for (i, px) in out.pixels.iter_mut().enumerate() {
            *px = self.at((i % w) as i32, (i / w) as i32);
        }
        Cow::Owned(out)
    }

    /// Bilinear sample in frame coordinates, with pixel centers on integers.
    /// Samples outside the data window are transparent, so edges dissolve
    /// cleanly instead of smearing.
    pub fn sample_bilinear(&self, x: f32, y: f32) -> Rgba {
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let x0 = x0 as i32;
        let y0 = y0 as i32;

        let tap = |px: i32, py: i32| -> Rgba { self.at(px, py) };

        let p00 = tap(x0, y0);
        let p10 = tap(x0 + 1, y0);
        let p01 = tap(x0, y0 + 1);
        let p11 = tap(x0 + 1, y0 + 1);

        let mut out = color::TRANSPARENT;
        for c in 0..4 {
            let top = p00[c] * (1.0 - fx) + p10[c] * fx;
            let bot = p01[c] * (1.0 - fx) + p11[c] * fx;
            out[c] = top * (1.0 - fy) + bot * fy;
        }
        out
    }

    /// Load an image from disk. `.exr` is read as-is (assumed linear,
    /// premultiplied); everything else goes through the `image` crate and is
    /// decoded from sRGB 8-bit, then premultiplied.
    pub fn read(path: &Path) -> Result<Image> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "exr" {
            Self::read_exr(path)
        } else {
            Self::read_ldr(path)
        }
    }

    fn read_ldr(path: &Path) -> Result<Image> {
        let dynimg = image::open(path)
            .with_context(|| format!("decoding image {}", path.display()))?;
        let rgba = dynimg.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let mut img = Image::new(w, h);
        for (i, px) in rgba.pixels().enumerate() {
            let [r, g, b, a] = px.0;
            let straight = [
                color::srgb_to_linear(r as f32 / 255.0),
                color::srgb_to_linear(g as f32 / 255.0),
                color::srgb_to_linear(b as f32 / 255.0),
                a as f32 / 255.0,
            ];
            img.pixels[i] = color::premultiply(straight);
        }
        Ok(img)
    }

    fn read_exr(path: &Path) -> Result<Image> {
        use exr::prelude::{read_first_rgba_layer_from_file, RgbaChannels, Vec2};
        let loaded = read_first_rgba_layer_from_file(
            path,
            |resolution: Vec2<usize>, _channels: &RgbaChannels| -> Image {
                Image::new(resolution.width(), resolution.height())
            },
            |img: &mut Image, pos: Vec2<usize>, (r, g, b, a): (f32, f32, f32, f32)| {
                img.set(pos.x(), pos.y(), [r, g, b, a]);
            },
        )
        .with_context(|| format!("reading EXR {}", path.display()))?;
        Ok(loaded.layer_data.channel_data.pixels)
    }

    /// Write to disk. `.exr` keeps full float; other extensions are encoded as
    /// 8-bit sRGB after unpremultiplying.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "exr" {
            self.write_exr(path)
        } else {
            self.write_ldr(path)
        }
    }

    fn write_ldr(&self, path: &Path) -> Result<()> {
        self.flat()
            .to_rgba8()
            .save(path)
            .with_context(|| format!("writing image {}", path.display()))?;
        Ok(())
    }

    /// 8-bit straight-alpha sRGB, as written to PNG/JPEG. The data window
    /// only; [`flat`](Self::flat) first for the frame.
    pub fn to_rgba8(&self) -> image::RgbaImage {
        let mut buf = image::RgbaImage::new(self.width as u32, self.height as u32);
        for (i, px) in buf.pixels_mut().enumerate() {
            let straight = color::unpremultiply(self.pixels[i]);
            px.0 = [
                (color::linear_to_srgb(straight[0]) * 255.0).round() as u8,
                (color::linear_to_srgb(straight[1]) * 255.0).round() as u8,
                (color::linear_to_srgb(straight[2]) * 255.0).round() as u8,
                (straight[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ];
        }
        buf
    }

    fn write_exr(&self, path: &Path) -> Result<()> {
        use exr::prelude::write_rgba_file;
        let frame = self.flat();
        write_rgba_file(path, frame.width, frame.height, |x, y| {
            let p = frame.get(x, y);
            (p[0], p[1], p[2], p[3])
        })
        .with_context(|| format!("writing EXR {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_sits_in_its_frame() {
        // A 2×1 window at (3, 1) in a 6×4 frame.
        let mut img = Image::blank(Size { width: 6, height: 4 }, IRect::new(3, 1, 5, 2));
        img.set(1, 0, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(img.window(), IRect::new(3, 1, 5, 2));
        assert_eq!(img.at(4, 1), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(img.at(0, 0), color::TRANSPARENT);
        assert_eq!(img.sample_bilinear(4.0, 1.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(img.sample_bilinear(3.5, 1.0)[3], 0.5);
        let flat = img.flat();
        assert_eq!((flat.width, flat.height, flat.window()), (6, 4, img.frame()));
        assert_eq!(flat.get(4, 1), [1.0, 0.0, 0.0, 1.0]);
        // Already a frame: borrowed.
        let full = Image::new(2, 2);
        assert!(matches!(full.flat(), Cow::Borrowed(_)));
    }

    #[test]
    fn rectangles_union_and_intersect() {
        let (a, b) = (IRect::new(0, 0, 4, 4), IRect::new(2, -2, 6, 3));
        assert_eq!(a.union(&b), IRect::new(0, -2, 6, 4));
        assert_eq!(a.intersect(&b), IRect::new(2, 0, 4, 3));
        assert!(a.intersect(&IRect::new(5, 5, 6, 6)).is_empty());
        assert_eq!(IRect::default().union(&b), b);
        assert_eq!(a.expand(1), IRect::new(-1, -1, 5, 5));
    }
}
