//! The core raster type and file I/O.

use crate::color::{self, Rgba};
use anyhow::{Context, Result};
use std::path::Path;

/// An RGBA float image stored row-major, linear + premultiplied.
#[derive(Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// `width * height` pixels, row by row from top-left.
    pub pixels: Vec<Rgba>,
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Image {
            width,
            height,
            pixels: vec![color::TRANSPARENT; width * height],
        }
    }

    pub fn filled(width: usize, height: usize, value: Rgba) -> Self {
        Image {
            width,
            height,
            pixels: vec![value; width * height],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Rgba {
        self.pixels[y * self.width + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: Rgba) {
        self.pixels[y * self.width + x] = v;
    }

    /// Bilinear sample in pixel coordinates. Samples outside the image are
    /// transparent, so edges dissolve cleanly instead of smearing.
    pub fn sample_bilinear(&self, x: f32, y: f32) -> Rgba {
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let x0 = x0 as isize;
        let y0 = y0 as isize;

        let tap = |px: isize, py: isize| -> Rgba {
            if px < 0 || py < 0 || px >= self.width as isize || py >= self.height as isize {
                color::TRANSPARENT
            } else {
                self.get(px as usize, py as usize)
            }
        };

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
        buf.save(path)
            .with_context(|| format!("writing image {}", path.display()))?;
        Ok(())
    }

    fn write_exr(&self, path: &Path) -> Result<()> {
        use exr::prelude::write_rgba_file;
        write_rgba_file(path, self.width, self.height, |x, y| {
            let p = self.get(x, y);
            (p[0], p[1], p[2], p[3])
        })
        .with_context(|| format!("writing EXR {}", path.display()))?;
        Ok(())
    }
}
