//! The core raster type and file I/O.

use crate::color::{self, Rgba};
use anyhow::{anyhow, bail, Context, Result};
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
    /// Extra channels over the same data window (a 3D render's `depth`):
    /// empty for most images. Ops that don't know about them drop them.
    pub planes: Vec<Plane>,
    /// A 3D scene, on the output of the Camera, Card and Scene nodes (whose
    /// pixels are empty); None on every other image.
    pub scene: Option<std::sync::Arc<crate::scene::Scene>>,
}

/// A named extra channel: one float per pixel of its image's data window,
/// in the same order as `pixels`.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub name: String,
    pub data: Vec<f32>,
}

impl Image {
    /// A transparent image whose data window is its whole frame.
    pub fn new(width: usize, height: usize) -> Self {
        Image::filled(width, height, color::TRANSPARENT)
    }

    pub fn filled(width: usize, height: usize, value: Rgba) -> Self {
        Image { format: Size { width, height }, x0: 0, y0: 0, width, height, pixels: vec![value; width * height], planes: Vec::new(), scene: None }
    }

    /// A transparent image of frame `format` with pixels stored for `window`.
    pub fn blank(format: Size, window: IRect) -> Self {
        let (width, height) = (window.width(), window.height());
        Image { format, x0: window.x0, y0: window.y0, width, height, pixels: vec![color::TRANSPARENT; width * height], planes: Vec::new(), scene: None }
    }

    /// Roughly how much memory this image holds: pixels, planes and a
    /// scene's card textures, meshes (with their textures), splats and points, plus
    /// a little for the struct itself, so an image carrying only a scene
    /// never counts as nothing. A mesh or texture shared by several objects
    /// counts once for each.
    pub fn bytes(&self) -> usize {
        let planes: usize = self.planes.iter().map(|p| p.data.len() * 4).sum();
        let scene = self.scene.as_ref().map_or(0, |s| {
            let meshes: usize = s
                .meshes
                .iter()
                .map(|m| {
                    let g = &m.mesh;
                    (g.positions.len() + g.normals.len()) * 24 + g.uvs.len() * 16 + g.triangles.len() * 12 + m.texture.as_ref().map_or(0, |t| t.bytes()) + 256
                })
                .sum();
            let splats: usize = s.splats.iter().map(|g| g.bytes()).sum();
            s.cards.iter().map(|c| c.image.bytes() + 256).sum::<usize>() + meshes + splats + s.cameras.len() * 256 + s.points.len() * 24
        });
        std::mem::size_of::<Image>() + self.pixels.len() * std::mem::size_of::<Rgba>() + planes + scene
    }

    /// The extra plane called `name`.
    pub fn plane(&self, name: &str) -> Option<&Plane> {
        self.planes.iter().find(|p| p.name == name)
    }

    /// Plane `name`'s data, added (all zero) when missing.
    pub fn plane_mut(&mut self, name: &str) -> &mut Vec<f32> {
        let i = match self.planes.iter().position(|p| p.name == name) {
            Some(i) => i,
            None => {
                self.planes.push(Plane { name: name.to_string(), data: vec![0.0; self.width * self.height] });
                self.planes.len() - 1
            }
        };
        &mut self.planes[i].data
    }

    /// Plane `name` at frame position (x, y): 0 outside the window or when
    /// there is no such plane.
    #[inline]
    pub fn plane_at(&self, name: &str, x: i32, y: i32) -> f32 {
        self.plane(name).map_or(0.0, |p| self.plane_value(p, x, y))
    }

    /// `plane` (one of this image's) at frame position (x, y): 0 outside
    /// the window.
    #[inline]
    pub fn plane_value(&self, plane: &Plane, x: i32, y: i32) -> f32 {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx >= self.width as i32 || ly >= self.height as i32 {
            0.0
        } else {
            plane.data[ly as usize * self.width + lx as usize]
        }
    }

    /// Give this image `src`'s planes, each pixel taking the value nearest
    /// to where `source` says it came from (frame coordinates, pixel
    /// centers on integers; None: nothing there). Nearest, not blended: a
    /// depth halfway between a near and a far surface is neither.
    pub fn carry_planes(&mut self, src: &Image, source: impl Fn(i32, i32) -> Option<[f32; 2]> + Sync) {
        use rayon::prelude::*;
        let (w, x0, y0) = (self.width.max(1), self.x0, self.y0);
        self.planes = src
            .planes
            .iter()
            .map(|p| {
                let mut data = vec![0.0; self.width * self.height];
                data.par_iter_mut().enumerate().for_each(|(i, v)| {
                    if let Some([sx, sy]) = source(x0 + (i % w) as i32, y0 + (i / w) as i32) {
                        if sx.is_finite() && sy.is_finite() {
                            *v = src.plane_value(p, sx.round() as i32, sy.round() as i32);
                        }
                    }
                });
                Plane { name: p.name.clone(), data }
            })
            .collect();
    }

    /// Each plane's name and lowest, highest and mean value over the data
    /// window, for reporting a render (MCP / Python stats).
    pub fn plane_stats(&self) -> serde_json::Value {
        let round = |x: f64| (x * 1e5).round() / 1e5;
        self.planes
            .iter()
            .map(|p| {
                let (lo, hi, sum) = p.data.iter().fold((f32::INFINITY, f32::NEG_INFINITY, 0f64), |(lo, hi, s), &v| (lo.min(v), hi.max(v), s + v as f64));
                let n = p.data.len();
                let (lo, hi) = if n == 0 { (0.0, 0.0) } else { (lo as f64, hi as f64) };
                serde_json::json!({ "name": p.name, "min": round(lo), "max": round(hi), "mean": round(sum / n.max(1) as f64) })
            })
            .collect()
    }

    /// [`carry_planes`](Self::carry_planes) from the same place in `src`:
    /// a copy when the windows match.
    pub fn keep_planes(&mut self, src: &Image) {
        if src.planes.is_empty() {
            self.planes.clear();
        } else if self.window() == src.window() {
            self.planes = src.planes.clone();
        } else {
            self.carry_planes(src, |x, y| Some([x as f32, y as f32]));
        }
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
        out.keep_planes(self);
        out.scene = self.scene.clone();
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
    /// premultiplied); everything else goes through the `image` crate:
    /// 8- and 16-bit files are decoded from sRGB at their own precision,
    /// float files (`.hdr`) taken as linear, then premultiplied.
    pub fn read(path: &Path) -> Result<Image> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "exr" => Self::read_exr(path),
            "dpx" => crate::dpx::read(path),
            _ => Self::read_ldr(path),
        }
    }

    fn read_ldr(path: &Path) -> Result<Image> {
        // The image crate's default 512 MB decode cap refuses e.g. 16-bit
        // RGBA past ~8K square; allow up to 4 GB (a 16384² 16-bit RGBA still
        // is 2 GB).
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(4 << 30);
        let mut reader = image::ImageReader::open(path)
            .and_then(|r| r.with_guessed_format())
            .with_context(|| format!("opening image {}", path.display()))?;
        reader.limits(limits);
        let dynimg = reader.decode().with_context(|| format!("decoding image {}", path.display()))?;
        let (w, h) = (dynimg.width() as usize, dynimg.height() as usize);
        let mut img = Image::new(w, h);
        let to_linear = |v: [f32; 4]| color::premultiply([color::srgb_to_linear(v[0]), color::srgb_to_linear(v[1]), color::srgb_to_linear(v[2]), v[3]]);
        let bits = dynimg.color().bytes_per_pixel() / dynimg.color().channel_count();
        if bits == 4 {
            // Float images (e.g. Radiance .hdr) are linear already.
            for (out, px) in img.pixels.iter_mut().zip(dynimg.to_rgba32f().pixels()) {
                *out = color::premultiply(px.0);
            }
        } else if bits == 2 {
            // 16-bit: keep the precision.
            for (out, px) in img.pixels.iter_mut().zip(dynimg.to_rgba16().pixels()) {
                *out = to_linear(px.0.map(|v| v as f32 / 65535.0));
            }
        } else {
            for (out, px) in img.pixels.iter_mut().zip(dynimg.to_rgba8().pixels()) {
                *out = to_linear(px.0.map(|v| v as f32 / 255.0));
            }
        }
        Ok(img)
    }

    /// An EXR's display window is the frame; its data window (the first
    /// RGBA layer's position and size) is where the pixels go, which may be
    /// smaller than the frame, offset, or reach past it.
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
        let display = loaded.attributes.display_window;
        let origin = loaded.layer_data.attributes.layer_position;
        let mut img = loaded.layer_data.channel_data.pixels;
        img.format = Size { width: display.size.width(), height: display.size.height() };
        img.x0 = origin.x() - display.position.x();
        img.y0 = origin.y() - display.position.y();
        Self::read_exr_planes(path, &mut img)?;
        Ok(img)
    }

    /// Channels other than R, G, B and A become planes, named as
    /// [`exr_plane_name`] says; read only when the header lists some.
    fn read_exr_planes(path: &Path, img: &mut Image) -> Result<()> {
        let meta = exr::meta::MetaData::read_from_file(path, false).with_context(|| format!("reading EXR {}", path.display()))?;
        let extra = |name: &str| !matches!(name, "R" | "G" | "B" | "A");
        let has_extra = meta.headers.first().is_some_and(|h| h.channels.list.iter().any(|c| extra(&c.name.to_string())));
        if !has_extra {
            return Ok(());
        }
        let flat = exr::prelude::read_first_flat_layer_from_file(path).with_context(|| format!("reading EXR {}", path.display()))?;
        let n = img.width * img.height;
        for c in flat.layer_data.channel_data.list.iter() {
            let name = c.name.to_string();
            if !extra(&name) {
                continue;
            }
            let data: Vec<f32> = c.sample_data.values_as_f32().take(n).collect();
            if data.len() == n {
                img.planes.push(Plane { name: exr_plane_name(&name, true), data });
            }
        }
        Ok(())
    }

    /// Write to disk. `.exr` keeps full float; `.tif`/`.tiff` are 16-bit
    /// sRGB with alpha; other extensions are 8-bit sRGB after
    /// unpremultiplying (JPEG, which has no alpha, over black).
    pub fn write(&self, path: &Path) -> Result<()> {
        self.write_as(path, crate::dpx::Options::default())
    }

    /// [`write`](Self::write), choosing how a `.dpx` is encoded.
    pub fn write_as(&self, path: &Path, dpx: crate::dpx::Options) -> Result<()> {
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
        } else if ext == "dpx" {
            crate::dpx::write(self, path, dpx)
        } else {
            self.write_ldr(path)
        }
    }

    fn write_ldr(&self, path: &Path) -> Result<()> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let flat = self.flat();
        match ext.as_str() {
            "tif" | "tiff" => flat.write_tiff(path),
            // No alpha in JPEG: the color over black.
            "jpg" | "jpeg" => {
                let mut rgb = image::RgbImage::new(flat.width as u32, flat.height as u32);
                for (out, px) in rgb.pixels_mut().zip(&flat.pixels) {
                    out.0 = std::array::from_fn(|c| (color::linear_to_srgb(px[c].max(0.0)) * 255.0).round() as u8);
                }
                Ok(rgb.save(path)?)
            }
            _ => Ok(flat.to_rgba8().save(path)?),
        }
        .with_context(|| format!("writing image {}", path.display()))
    }

    /// 16-bit sRGB TIFF with straight alpha, marked as alpha (the
    /// ExtraSamples tag, which other apps need to see it), LZW-compressed.
    fn write_tiff(&self, path: &Path) -> Result<()> {
        use tiff::encoder::{colortype::RGB16, Compression, TiffEncoder};
        use tiff::tags::ExtraSamples;
        let to16 = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        let data: Vec<u16> = self
            .pixels
            .iter()
            .flat_map(|&px| {
                let s = color::unpremultiply(px);
                [to16(color::linear_to_srgb(s[0])), to16(color::linear_to_srgb(s[1])), to16(color::linear_to_srgb(s[2])), to16(s[3])]
            })
            .collect();
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut tiff = TiffEncoder::new(file)?.with_compression(Compression::Lzw);
        let mut img = tiff.new_image::<RGB16>(self.width as u32, self.height as u32)?;
        img.extra_samples(&[ExtraSamples::UnassociatedAlpha])?;
        img.write_data(&data)?;
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

    /// The frame as the display window and only the data window's pixels
    /// (an empty window: one transparent pixel, as EXR needs some), 32-bit
    /// float, losslessly compressed.
    fn write_exr(&self, path: &Path) -> Result<()> {
        use exr::meta::attribute::IntegerBounds;
        use exr::prelude::{Encoding, Image as ExrImage, Layer, LayerAttributes, SpecificChannels, Vec2, WritableImage};
        let w = self.window();
        let (origin, size) = if w.is_empty() { ((0, 0), (1, 1)) } else { ((w.x0, w.y0), (self.width, self.height)) };
        let attributes = LayerAttributes { layer_position: Vec2(origin.0, origin.1), ..Default::default() };
        let channels = SpecificChannels::rgba(|p: Vec2<usize>| {
            let px = if w.is_empty() { color::TRANSPARENT } else { self.get(p.x(), p.y()) };
            (px[0], px[1], px[2], px[3])
        });
        let display = IntegerBounds::new((0, 0), (self.format.width, self.format.height));
        if !self.planes.is_empty() {
            return self.write_exr_planes(path, size, attributes, display);
        }
        let mut image = ExrImage::from_layer(Layer::new(size, attributes, Encoding::FAST_LOSSLESS, channels));
        image.attributes.display_window = display;
        image.write().to_file(path).with_context(|| format!("writing EXR {}", path.display()))?;
        Ok(())
    }

    /// [`write_exr`](Self::write_exr) with the planes as extra channels.
    fn write_exr_planes(&self, path: &Path, size: (usize, usize), attributes: exr::prelude::LayerAttributes, display: exr::meta::attribute::IntegerBounds) -> Result<()> {
        use exr::prelude::{AnyChannel, AnyChannels, Encoding, FlatSamples, Image as ExrImage, Layer, WritableImage};
        let n = size.0 * size.1;
        let empty = self.window().is_empty();
        let mut list: Vec<AnyChannel<FlatSamples>> = ["R", "G", "B", "A"]
            .into_iter()
            .enumerate()
            .map(|(c, name)| {
                let data = if empty { vec![0.0; n] } else { self.pixels.iter().map(|p| p[c]).collect() };
                AnyChannel::new(name, FlatSamples::F32(data))
            })
            .collect();
        for p in &self.planes {
            let data = if empty { vec![0.0; n] } else { p.data.clone() };
            list.push(AnyChannel::new(exr_plane_name(&p.name, false).as_str(), FlatSamples::F32(data)));
        }
        let mut image = ExrImage::from_layer(Layer::new(size, attributes, Encoding::FAST_LOSSLESS, AnyChannels::sort(list.into())));
        image.attributes.display_window = display;
        image.write().to_file(path).with_context(|| format!("writing EXR {}", path.display()))?;
        Ok(())
    }
}

/// A plane's channel name in an EXR file and back (`reading`), in the
/// `layer.channel` form other compositing and 3D apps read as layers:
/// `depth` is `depth.Z` (and a bare `Z` reads as depth too), a matte
/// `matte.<node>` is `matte.<node>.A`; others (`P.x`, `N.y`, `motion.u`,
/// `diffuse.R`, `id`) keep their names.
fn exr_plane_name(name: &str, reading: bool) -> String {
    if reading {
        match name {
            "Z" | "depth.Z" => "depth".into(),
            _ => match name.strip_prefix("matte.").and_then(|m| m.strip_suffix(".A")) {
                Some(node) if !node.is_empty() => format!("matte.{node}"),
                _ => name.into(),
            },
        }
    } else if name == "depth" {
        "depth.Z".into()
    } else if name.starts_with("matte.") && split_plane(name).1.is_none() {
        format!("{name}.A")
    } else {
        name.into()
    }
}

/// The component names a plane name may end in after a dot, making it one
/// channel of a layer (`P.x` of `P`), in the order a layer lists them.
const COMPONENTS: [&str; 10] = ["R", "G", "B", "A", "x", "y", "z", "w", "u", "v"];

/// A plane name's layer and component: `P.x` → (`P`, Some(`x`)), `depth`
/// → (`depth`, None), `matte.card1` → (`matte.card1`, None).
pub fn split_plane(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((layer, c)) if !layer.is_empty() && COMPONENTS.contains(&c) => (layer, Some(c)),
        _ => (name, None),
    }
}

/// The components a new layer of `count` channels gets: x, y, z for
/// positions and normals (`P`, `N`, `Pref`), u, v for `uv` and `motion`,
/// none for one channel, else R, G, B, A.
fn default_components(layer: &str, count: usize) -> Vec<&'static str> {
    let names: &[&'static str] = match layer {
        _ if count <= 1 => &[],
        "P" | "N" | "Pref" | "Nref" => &["x", "y", "z", "w"],
        "uv" | "motion" => &["u", "v", "w"],
        _ => &["R", "G", "B", "A"],
    };
    names.iter().take(count).copied().collect()
}

/// Shuffling channels: what the Shuffle node does. A channel is named `R`,
/// `G`, `B` or `A` (the pixels), `0` or `1` (a constant), or a plane's
/// name; a layer is a plane or the planes `layer.<component>` together
/// (see [`split_plane`]).
impl Image {
    /// The image's planes as layers, in the order they first appear, each
    /// with its plane names in component order.
    pub fn layers(&self) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for p in &self.planes {
            let layer = split_plane(&p.name).0;
            match out.iter_mut().find(|l| l.0 == layer) {
                Some(l) => l.1.push(p.name.clone()),
                None => out.push((layer.to_string(), vec![p.name.clone()])),
            }
        }
        let rank = |n: &str| split_plane(n).1.and_then(|c| COMPONENTS.iter().position(|k| *k == c)).unwrap_or(0);
        for l in &mut out {
            l.1.sort_by_key(|n| rank(n));
        }
        out
    }

    /// The names of the planes making up `layer`, in component order (empty
    /// when there's no such layer).
    pub fn layer(&self, layer: &str) -> Vec<String> {
        self.layers().into_iter().find(|l| l.0 == layer).map(|l| l.1).unwrap_or_default()
    }

    /// Channel `name`'s values over the data window (None when there's no
    /// such channel).
    pub fn channel(&self, name: &str) -> Option<Vec<f32>> {
        let n = self.width * self.height;
        match name {
            "R" | "G" | "B" | "A" => {
                let c = ["R", "G", "B", "A"].iter().position(|k| *k == name).unwrap();
                Some(self.pixels.iter().map(|p| p[c]).collect())
            }
            "0" => Some(vec![0.0; n]),
            "1" => Some(vec![1.0; n]),
            _ => self.plane(name).map(|p| p.data.clone()),
        }
    }

    /// A copy with R, G, B and A taken from channels `rgba` (planes kept).
    pub fn shuffle(&self, rgba: [&str; 4]) -> Result<Image> {
        let chans: Vec<Vec<f32>> = rgba.iter().map(|c| self.channel(c).ok_or_else(|| anyhow!("no channel '{c}'"))).collect::<Result<_>>()?;
        let mut out = self.clone();
        for (i, px) in out.pixels.iter_mut().enumerate() {
            *px = std::array::from_fn(|c| chans[c][i]);
        }
        Ok(out)
    }

    /// Copy channel `source` into plane `plane`, new or replacing one.
    pub fn copy_to_plane(&mut self, source: &str, plane: &str) -> Result<()> {
        let data = self.channel(source).ok_or_else(|| anyhow!("no channel '{source}'"))?;
        *self.plane_mut(plane) = data;
        Ok(())
    }

    /// A copy with `layer` in its color: one channel into R, G and B; two
    /// into R and G (B 0); three into R, G, B; four into R, G, B, A. Alpha
    /// stays unless the layer has four channels; planes are kept.
    pub fn layer_to_rgba(&self, layer: &str) -> Result<Image> {
        let names = self.layer(layer);
        let r = |i: usize| names.get(i).map(String::as_str);
        let rgba = match names.len() {
            0 => bail!("no layer '{layer}'"),
            1 => [r(0).unwrap(); 3].into_iter().chain(["A"]).collect::<Vec<_>>(),
            2 => vec![r(0).unwrap(), r(1).unwrap(), "0", "A"],
            3 => vec![r(0).unwrap(), r(1).unwrap(), r(2).unwrap(), "A"],
            _ => vec![r(0).unwrap(), r(1).unwrap(), r(2).unwrap(), r(3).unwrap()],
        };
        self.shuffle([rgba[0], rgba[1], rgba[2], rgba[3]])
    }

    /// The first `count` (1 to 4) of R, G, B, A into `layer`'s planes: the
    /// layer's own components when it exists with that many, else the usual
    /// ones for its name (x, y, z for `P` and `N`; u, v for `uv` and
    /// `motion`; R, G, B, A otherwise; a single plane named `layer` for
    /// one).
    pub fn rgba_to_layer(&mut self, layer: &str, count: usize) -> Result<()> {
        if !(1..=4).contains(&count) {
            bail!("a layer takes 1 to 4 channels, not {count}");
        }
        let existing = self.layer(layer);
        let names: Vec<String> = if existing.len() == count {
            existing
        } else {
            self.remove_layers(&[layer]);
            let comps = default_components(layer, count);
            if comps.is_empty() { vec![layer.to_string()] } else { comps.iter().map(|c| format!("{layer}.{c}")).collect() }
        };
        for (c, name) in names.iter().enumerate() {
            self.copy_to_plane(["R", "G", "B", "A"][c], name)?;
        }
        Ok(())
    }

    /// `a` and `b` merged by depth (their `depth` planes; 0 or no plane is
    /// nothing there, as far as can be): at each pixel the nearer goes over
    /// the farther, so an antialiased edge in front blends with what's
    /// behind it and both images' coverage adds up as in Merge's over. With
    /// no depth on either side, `a` goes over `b`. The result covers both
    /// windows in `a`'s frame; its depth is the nearer one. Other planes:
    /// coverage passes (mattes, and layers of R, G, B, A such as `diffuse`)
    /// merge the same way; data passes (`P`, `N`, `uv`, `motion`, `id`,
    /// ...) take the nearer image's value where it covers the pixel, else
    /// the other's.
    pub fn zmerge(a: &Image, b: &Image) -> Image {
        let window = a.window().union(&b.window());
        let mut out = Image::blank(a.format, window);
        let ww = window.width().max(1);
        let at = |i: usize| (window.x0 + (i % ww) as i32, window.y0 + (i / ww) as i32);
        let depth = |img: &Image, x: i32, y: i32| img.plane("depth").map_or(0.0, |p| img.plane_value(p, x, y));
        let n = out.width * out.height;
        // Whether a is in front at each pixel: nearer, or the only one
        // with depth, or (neither has) as given.
        let a_front: Vec<bool> = (0..n)
            .map(|i| {
                let (x, y) = at(i);
                let (da, db) = (depth(a, x, y), depth(b, x, y));
                if da > 0.0 && db > 0.0 { da <= db } else { db <= 0.0 }
            })
            .collect();
        let pair = |i: usize| if a_front[i] { (a, b) } else { (b, a) };
        for (i, px) in out.pixels.iter_mut().enumerate() {
            let (x, y) = at(i);
            let (f, k) = pair(i);
            let (fp, kp) = (f.at(x, y), k.at(x, y));
            *px = std::array::from_fn(|c| fp[c] + kp[c] * (1.0 - fp[3]));
        }
        let mut names: Vec<String> = Vec::new();
        for p in a.planes.iter().chain(&b.planes) {
            if !names.contains(&p.name) {
                names.push(p.name.clone());
            }
        }
        for name in names {
            let (pa, pb) = (a.plane(&name), b.plane(&name));
            let value = |img: &Image, p: Option<&Plane>, x: i32, y: i32| p.map_or(0.0, |p| img.plane_value(p, x, y));
            let coverage = name.starts_with("matte.") || matches!(split_plane(&name).1, Some("R" | "G" | "B" | "A"));
            let data: Vec<f32> = (0..n)
                .map(|i| {
                    let (x, y) = at(i);
                    let (va, vb) = (value(a, pa, x, y), value(b, pb, x, y));
                    let (vf, vk, f, fp) = if a_front[i] { (va, vb, a, pa) } else { (vb, va, b, pb) };
                    if name == "depth" {
                        [va, vb].into_iter().filter(|d| *d > 0.0).fold(0.0, |m: f32, d| if m == 0.0 { d } else { m.min(d) })
                    } else if coverage {
                        vf + vk * (1.0 - f.at(x, y)[3])
                    } else if fp.is_some() && f.at(x, y)[3] > 0.0 {
                        vf
                    } else {
                        vk
                    }
                })
                .collect();
            *out.plane_mut(&name) = data;
        }
        out
    }

    /// Drop layers `layers` (each a plane, or all of a layer's planes);
    /// `*` drops every plane.
    pub fn remove_layers(&mut self, layers: &[&str]) {
        if layers.contains(&"*") {
            self.planes.clear();
        } else {
            self.planes.retain(|p| !layers.contains(&split_plane(&p.name).0) && !layers.contains(&p.name.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_bit_and_float_files_keep_their_precision() {
        let dir = std::env::temp_dir().join(format!("arclight_depth_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Two greys one 16-bit step apart: 8 bits would make them equal.
        let deep = image::ImageBuffer::<image::Rgba<u16>, _>::from_fn(2, 1, |x, _| image::Rgba([30000 + x as u16, 30000, 30000, 65535]));
        let png = dir.join("deep.png");
        deep.save(&png).unwrap();
        let img = Image::read(&png).unwrap();
        let (a, b) = (img.at(0, 0)[0], img.at(1, 0)[0]);
        assert!(b > a && (a - color::srgb_to_linear(30000.0 / 65535.0)).abs() < 1e-6, "{a} {b}");
        // Radiance HDR is linear float: read as is, past 1.0.
        let hdr = image::ImageBuffer::<image::Rgb<f32>, _>::from_pixel(1, 1, image::Rgb([4.0, 0.5, 0.25]));
        let path = dir.join("bright.hdr");
        image::DynamicImage::ImageRgb32F(hdr).save(&path).unwrap();
        let px = Image::read(&path).unwrap().at(0, 0);
        assert!((px[0] - 4.0).abs() < 0.05 && (px[1] - 0.5).abs() < 0.01 && px[3] == 1.0, "{px:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tiff_tga_webp_and_jpeg_write_and_read_back() {
        let dir = std::env::temp_dir().join(format!("arclight_formats_{}", std::process::id()));
        let src = Image::filled(4, 3, [0.1, 0.25, 0.4, 0.5]);
        for ext in ["tif", "tga", "webp", "png", "jpg"] {
            let path = dir.join(format!("frame.{ext}"));
            src.write(&path).unwrap();
            let back = Image::read(&path).unwrap().at(1, 1);
            // 16-bit TIFF is near exact; 8-bit within rounding; JPEG, with no
            // alpha, comes back opaque (over black).
            let tol = if ext == "tif" { 1e-4 } else { 0.01 };
            let want = if ext == "jpg" { [0.1, 0.25, 0.4, 1.0] } else { [0.1, 0.25, 0.4, 0.5] };
            assert!(back.iter().zip(want).all(|(a, b)| (a - b).abs() < tol), "{ext}: {back:?}");
        }
        // The TIFF marks its fourth channel as alpha, and is 16-bit.
        let tif = image::open(dir.join("frame.tif")).unwrap();
        assert_eq!(tif.color(), image::ColorType::Rgba16);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exr_keeps_the_data_window() {
        let dir = std::env::temp_dir().join(format!("arclight_exrdw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 3×2 layer at (-1, 4) in a 6×5 frame (past its left and bottom
        // edges), with distinct pixels.
        let mut img = Image::blank(Size { width: 6, height: 5 }, IRect::new(-1, 4, 2, 6));
        for (i, p) in img.pixels.iter_mut().enumerate() {
            *p = [i as f32, 0.5, 2.5, 1.0];
        }
        let path = dir.join("layer.exr");
        img.write(&path).unwrap();
        let back = Image::read(&path).unwrap();
        assert_eq!((back.format, back.window()), (img.format, img.window()));
        assert_eq!(back.pixels, img.pixels);
        // Nothing stored: a transparent frame of the right size.
        let empty = Image::blank(Size { width: 6, height: 5 }, IRect::default());
        empty.write(&path).unwrap();
        let back = Image::read(&path).unwrap();
        assert_eq!(back.format, empty.format);
        assert!(back.flat().pixels.iter().all(|p| *p == color::TRANSPARENT));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exr_keeps_extra_planes() {
        let dir = std::env::temp_dir().join(format!("arclight_exrplanes_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = Image::blank(Size { width: 5, height: 4 }, IRect::new(1, 1, 4, 3));
        img.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = [i as f32, 0.0, 0.0, 1.0]);
        *img.plane_mut("depth") = (0..6).map(|i| 10.0 + i as f32).collect();
        *img.plane_mut("id") = vec![7.0; 6];
        let path = dir.join("planes.exr");
        img.write(&path).unwrap();
        let back = Image::read(&path).unwrap();
        assert_eq!((back.format, back.window(), &back.pixels), (img.format, img.window(), &img.pixels));
        assert_eq!(back.plane("depth"), img.plane("depth"), "depth.Z in the file, depth here");
        assert_eq!(back.plane("id"), img.plane("id"));
        assert_eq!(back.plane_at("depth", 2, 1), 11.0);
        assert_eq!(back.plane_at("depth", 0, 0), 0.0, "outside the window");
        assert_eq!(back.plane_stats()[0], serde_json::json!({ "name": "depth", "min": 10.0, "max": 15.0, "mean": 12.5 }));
        let names: Vec<String> = exr::meta::MetaData::read_from_file(&path, false).unwrap().headers[0].channels.list.iter().map(|c| c.name.to_string()).collect();
        assert!(names.contains(&"depth.Z".to_string()), "{names:?}");
        // The frame copy keeps them, moved into place.
        let flat = back.flat();
        assert_eq!(flat.plane_at("depth", 3, 2), 15.0);
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// A 3 × 2 image carrying render passes: position, normal, motion,
    /// depth, a matte, a lighting layer and an id.
    fn passes() -> Image {
        let mut img = Image::blank(Size { width: 4, height: 3 }, IRect::new(1, 0, 4, 2));
        img.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = [0.1 * i as f32, 0.2, 0.3, 1.0]);
        let ramp = |k: f32| (0..6).map(|i| k + i as f32).collect::<Vec<f32>>();
        for (c, k) in ["P.z", "P.x", "P.y", "N.x", "N.y", "N.z", "motion.u", "motion.v", "diffuse.R", "diffuse.G", "diffuse.B"].iter().zip(1..) {
            *img.plane_mut(c) = ramp(10.0 * k as f32);
        }
        *img.plane_mut("depth") = ramp(-5.0);
        *img.plane_mut("matte.card1") = vec![0.0, 0.5, 1.0, 1.0, 0.25, 0.0];
        *img.plane_mut("id") = vec![3.0; 6];
        img
    }

    #[test]
    fn exr_writes_every_plane_with_standard_names_and_reads_them_back() {
        let dir = std::env::temp_dir().join(format!("arclight_exrpasses_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = passes();
        let path = dir.join("passes.exr");
        img.write(&path).unwrap();
        let names: Vec<String> = exr::meta::MetaData::read_from_file(&path, false).unwrap().headers[0].channels.list.iter().map(|c| c.name.to_string()).collect();
        for want in ["R", "G", "B", "A", "P.x", "P.y", "P.z", "N.x", "motion.u", "motion.v", "depth.Z", "matte.card1.A", "diffuse.R", "id"] {
            assert!(names.iter().any(|n| n == want), "{want} not in {names:?}");
        }
        let back = Image::read(&path).unwrap();
        assert_eq!((back.window(), &back.pixels), (img.window(), &img.pixels));
        let mut a: Vec<&Plane> = img.planes.iter().collect();
        let mut b: Vec<&Plane> = back.planes.iter().collect();
        a.sort_by(|x, y| x.name.cmp(&y.name));
        b.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(a, b, "every plane back under its own name");
        // A bare Z, as other apps may write depth, reads as depth too.
        assert_eq!((exr_plane_name("Z", true), exr_plane_name("matte.a.b.A", true)), ("depth".to_string(), "matte.a.b".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn layers_group_planes_by_component() {
        let img = passes();
        let layers: Vec<(String, Vec<String>)> = img.layers();
        let names: Vec<&str> = layers.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(names, ["P", "N", "motion", "diffuse", "depth", "matte.card1", "id"]);
        assert_eq!(img.layer("P"), ["P.x", "P.y", "P.z"], "in component order");
        assert_eq!(img.layer("diffuse"), ["diffuse.R", "diffuse.G", "diffuse.B"]);
        assert!(img.layer("nothing").is_empty());
        assert_eq!((split_plane("matte.card1"), split_plane("N.x")), (("matte.card1", None), ("N", Some("x"))));
    }

    #[test]
    fn shuffles_copy_channels_and_planes_round_trip() {
        let img = passes();
        // Any channel into any of R, G, B, A.
        let s = img.shuffle(["A", "N.y", "0", "1"]).unwrap();
        assert!(s.pixels.iter().zip(&img.plane("N.y").unwrap().data).all(|(p, n)| p == &[1.0, *n, 0.0, 1.0]));
        assert_eq!(s.planes, img.planes, "planes kept");
        assert!(img.shuffle(["R", "G", "B", "nope"]).err().is_some_and(|e| e.to_string().contains("nope")));
        // A pass to RGBA and back is unchanged; its color shows the pass.
        for layer in ["P", "N", "motion", "diffuse", "depth", "matte.card1"] {
            let mut rgba = img.layer_to_rgba(layer).unwrap();
            let names = img.layer(layer);
            for (c, name) in names.iter().enumerate() {
                assert_eq!(rgba.channel(["R", "G", "B", "A"][c]), rgba.channel(name), "{layer}: {name}");
            }
            rgba.rgba_to_layer(layer, names.len()).unwrap();
            assert_eq!(rgba.planes, img.planes, "{layer} back unchanged");
        }
        // One channel shows as gray, alpha kept; two leave blue at 0.
        let d = img.layer_to_rgba("depth").unwrap();
        assert!(d.pixels.iter().zip(&img.plane("depth").unwrap().data).all(|(p, z)| p == &[*z, *z, *z, 1.0]));
        assert!(img.layer_to_rgba("motion").unwrap().pixels.iter().all(|p| p[2] == 0.0));
        assert!(img.layer_to_rgba("nothing").is_err());
        // The color into new layers, named as such passes are.
        let mut out = img.clone();
        out.rgba_to_layer("Pref", 3).unwrap();
        out.rgba_to_layer("uv", 2).unwrap();
        out.rgba_to_layer("glow", 4).unwrap();
        out.rgba_to_layer("mask", 1).unwrap();
        assert_eq!(out.layer("Pref"), ["Pref.x", "Pref.y", "Pref.z"]);
        assert_eq!(out.layer("uv"), ["uv.u", "uv.v"]);
        assert_eq!(out.layer("glow"), ["glow.R", "glow.G", "glow.B", "glow.A"]);
        assert_eq!(out.channel("mask"), img.channel("R"));
        assert!(out.rgba_to_layer("x", 5).is_err());
        // Replacing a layer with fewer channels drops the old ones.
        out.rgba_to_layer("diffuse", 1).unwrap();
        assert_eq!(out.layer("diffuse"), ["diffuse"]);
        // One channel into a plane; layers removed.
        out.copy_to_plane("G", "P.x").unwrap();
        assert_eq!(out.channel("P.x"), img.channel("G"));
        out.remove_layers(&["P", "matte.card1", "id"]);
        assert!(out.layer("P").is_empty() && out.plane("matte.card1").is_none() && out.plane("id").is_none() && !out.layer("N").is_empty());
        out.remove_layers(&["*"]);
        assert!(out.planes.is_empty());
    }

    #[test]
    fn a_scene_counts_its_meshes() {
        let mut img = Image::new(1, 1);
        let bare = img.bytes();
        let mesh = std::sync::Arc::new(crate::mesh::sphere(1.0, [16, 8]));
        let model = |texture: Option<std::sync::Arc<Image>>| crate::scene::Model {
            mesh: mesh.clone(),
            world: crate::geom::Mat4::identity(),
            material: crate::mesh::Material::default(),
            texture,
            holdout: false,
            casts_shadows: true,
            receives_shadows: true,
            name: String::new(),
            node: String::new(),
        };
        img.scene = Some(std::sync::Arc::new(crate::scene::Scene { meshes: vec![model(None)], ..Default::default() }));
        let one = img.bytes() - bare;
        // 153 vertices (positions, normals, uvs) and 224 triangles.
        assert_eq!(one, 153 * (24 + 24 + 16) + 224 * 12 + 256);
        let tex = std::sync::Arc::new(Image::new(64, 64));
        img.scene = Some(std::sync::Arc::new(crate::scene::Scene { meshes: vec![model(None), model(Some(tex.clone()))], ..Default::default() }));
        assert_eq!(img.bytes() - bare, 2 * one + tex.bytes());
    }

    /// A 6 × 1 strip in an 8 × 2 frame: color `c` with alpha per pixel,
    /// at `depth` where covered (0 elsewhere).
    fn strip(x0: i32, alpha: [f32; 6], c: [f32; 3], depth: f32) -> Image {
        let mut img = Image::blank(Size { width: 8, height: 2 }, IRect::new(x0, 0, x0 + 6, 1));
        img.pixels = alpha.iter().map(|&a| [c[0] * a, c[1] * a, c[2] * a, a]).collect();
        *img.plane_mut("depth") = alpha.iter().map(|&a| if a > 0.0 { depth } else { 0.0 }).collect();
        img
    }

    fn near(a: Rgba, b: Rgba) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() < 1e-6)
    }

    #[test]
    fn zmerge_puts_the_nearer_in_front() {
        // Red 5 away on x 0..6, blue 3 away on x 2..8; overlapping on 2..6.
        let red = strip(0, [1.0; 6], [1.0, 0.0, 0.0], 5.0);
        let blue = strip(2, [1.0; 6], [0.0, 0.0, 1.0], 3.0);
        let m = Image::zmerge(&red, &blue);
        assert_eq!(m.window(), IRect::new(0, 0, 8, 1));
        for x in 0..8 {
            let want = if x < 2 { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] };
            assert!(near(m.at(x, 0), want), "x {x}: {:?}", m.at(x, 0));
            assert_eq!(m.plane_at("depth", x, 0), if x < 2 { 5.0 } else { 3.0 });
        }
        // Either order: the same where the depths differ.
        let n = Image::zmerge(&blue, &red);
        assert!((0..8).all(|x| near(m.at(x, 0), n.at(x, 0)) && m.plane_at("depth", x, 0) == n.plane_at("depth", x, 0)));
        // Moved behind, red shows through.
        let far = strip(2, [1.0; 6], [0.0, 0.0, 1.0], 9.0);
        let m = Image::zmerge(&red, &far);
        assert!(near(m.at(4, 0), [1.0, 0.0, 0.0, 1.0]) && near(m.at(7, 0), [0.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn zmerge_blends_edges_and_coverage() {
        // A soft-edged blue in front (depth 2) of opaque red (8): the edge
        // pixels are blue over red; where only one covers, it alone.
        let red = strip(0, [1.0; 6], [1.0, 0.0, 0.0], 8.0);
        let blue = strip(0, [0.0, 0.25, 0.5, 1.0, 0.5, 0.0], [0.0, 0.0, 1.0], 2.0);
        let m = Image::zmerge(&red, &blue);
        for (x, a) in [0.0f32, 0.25, 0.5, 1.0, 0.5, 0.0].iter().enumerate() {
            assert!(near(m.at(x as i32, 0), [1.0 - a, 0.0, *a, 1.0]), "x {x}: {:?}", m.at(x as i32, 0));
        }
        // A soft edge behind an opaque front is hidden.
        let front = strip(0, [1.0; 6], [0.0, 1.0, 0.0], 1.0);
        assert!((0..6).all(|x| near(Image::zmerge(&blue, &front).at(x, 0), [0.0, 1.0, 0.0, 1.0])));
        // Where both are partly covered, coverage adds as in over.
        let half_red = strip(0, [0.5; 6], [1.0, 0.0, 0.0], 8.0);
        let m = Image::zmerge(&half_red, &blue);
        assert!(near(m.at(2, 0), [0.25, 0.0, 0.5, 0.75]), "{:?}", m.at(2, 0));
        // An image without depth is as far as can be: behind.
        let mut bg = Image::filled(8, 2, [0.0, 1.0, 0.0, 1.0]);
        bg.planes.clear();
        let m = Image::zmerge(&bg, &blue);
        assert!(near(m.at(3, 0), [0.0, 0.0, 1.0, 1.0]) && near(m.at(2, 0), [0.0, 0.5, 0.5, 1.0]) && near(m.at(0, 1), [0.0, 1.0, 0.0, 1.0]));
        assert_eq!((m.plane_at("depth", 3, 0), m.plane_at("depth", 0, 1)), (2.0, 0.0));
        // Neither with depth: a over b.
        let (mut a, mut b) = (strip(0, [0.5; 6], [1.0, 0.0, 0.0], 1.0), strip(0, [1.0; 6], [0.0, 0.0, 1.0], 1.0));
        a.planes.clear();
        b.planes.clear();
        assert!(near(Image::zmerge(&a, &b).at(1, 0), [0.5, 0.0, 0.5, 1.0]));
    }

    #[test]
    fn zmerge_carries_passes_by_kind() {
        let mut red = strip(0, [1.0; 6], [1.0, 0.0, 0.0], 5.0);
        let mut blue = strip(2, [1.0, 1.0, 0.5, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 3.0);
        *red.plane_mut("N.x") = vec![1.0; 6];
        *blue.plane_mut("N.x") = vec![-1.0; 6];
        *red.plane_mut("matte.red") = vec![1.0; 6];
        *blue.plane_mut("matte.blue") = vec![1.0, 1.0, 0.5, 0.0, 0.0, 0.0];
        let m = Image::zmerge(&red, &blue);
        // Normals from whichever is nearer where it covers.
        assert_eq!((0..6).map(|x| m.plane_at("N.x", x, 0)).collect::<Vec<_>>(), [1.0, 1.0, -1.0, -1.0, -1.0, 1.0]);
        // Mattes as coverage: blue's whole; red's cut where blue is in
        // front, half at blue's soft edge.
        assert_eq!((0..6).map(|x| m.plane_at("matte.blue", x, 0)).collect::<Vec<_>>(), [0.0, 0.0, 1.0, 1.0, 0.5, 0.0]);
        assert_eq!((0..6).map(|x| m.plane_at("matte.red", x, 0)).collect::<Vec<_>>(), [1.0, 1.0, 0.0, 0.0, 0.5, 1.0]);
        // The mattes still add up to the alpha.
        assert!((0..6).all(|x| (m.plane_at("matte.red", x, 0) + m.plane_at("matte.blue", x, 0) - m.at(x, 0)[3]).abs() < 1e-6));
    }
}
