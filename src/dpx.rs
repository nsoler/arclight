//! DPX (SMPTE 268M) images: the common film and video variants.
//!
//! Reading takes the first image element: 8, 10, 12 or 16 bits per
//! component, or 32/64-bit float; RGB, RGBA, ABGR or luma; 10- and 12-bit
//! data filled into
//! words (method A or B) or packed as a continuous bit stream; either byte
//! order; any orientation. Log data (printing density) is converted to
//! linear with the Cineon curve (black 95, white 685 on the 10-bit scale),
//! linear data is taken as is, and video-coded data is decoded like the
//! other 8/16-bit formats (sRGB); float data is linear unless marked as
//! video. Writing is RGB or RGBA (see [`Options`]): 10-bit log, the usual
//! film deliverable (values above 1.0 survive it), 10- or 16-bit
//! video-coded (sRGB, full range, marked Rec. 709), or 32-bit float linear
//! (exact). Alpha is written straight (not premultiplied), as DPX readers
//! expect.

use crate::color;
use crate::image::Image;
use anyhow::{bail, ensure, Context, Result};
use std::path::Path;

/// Header layout offsets (bytes from the start of the file).
const MAGIC: usize = 0;
const DATA_OFFSET: usize = 4;
const ORIENTATION: usize = 768;
const ELEMENTS: usize = 770;
const WIDTH: usize = 772;
const HEIGHT: usize = 776;
/// The first image element; its fields are relative to this.
const ELEMENT: usize = 780;
const DESCRIPTOR: usize = 20;
const TRANSFER: usize = 21;
const BIT_DEPTH: usize = 23;
const PACKING: usize = 24;
const ENCODING: usize = 26;
const ELEMENT_OFFSET: usize = 28;
/// Where the image data starts in files we write (headers end at 2048).
const HEADER_SIZE: usize = 2048;

/// Media Out's and Write's DPX choices.
pub const ENCODINGS: &[&str] = &["10-bit log", "10-bit video", "16-bit video", "32-bit float linear"];

/// How DPX is written.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Encoding {
    /// Printing density (Cineon log), 10-bit: keeps values above 1.0.
    #[default]
    Log10,
    /// sRGB-coded video, full range, 10-bit.
    Video10,
    /// sRGB-coded video, full range, 16-bit.
    Video16,
    /// Linear 32-bit float: exact, any range.
    Float32,
}

impl Encoding {
    /// The encoding for an [`ENCODINGS`] choice (anything else: log).
    pub fn from_choice(choice: &str) -> Encoding {
        match choice {
            "10-bit video" => Encoding::Video10,
            "16-bit video" => Encoding::Video16,
            "32-bit float linear" => Encoding::Float32,
            _ => Encoding::Log10,
        }
    }
}

/// How to write a DPX: the encoding, and whether to store alpha (RGBA,
/// straight) or only color (RGB, over black).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    pub encoding: Encoding,
    pub alpha: bool,
}

/// Cineon printing density: code values for black and white.
const LOG_BLACK: f32 = 95.0;
const LOG_WHITE: f32 = 685.0;
/// Density per code value, and the film gamma.
const DENSITY_STEP: f32 = 0.002;
const FILM_GAMMA: f32 = 0.6;

fn log_offset() -> f32 {
    10f32.powf((LOG_BLACK - LOG_WHITE) * DENSITY_STEP / FILM_GAMMA)
}

/// A 10-bit log code value (0–1023) to linear light.
fn log_to_linear(code: f32) -> f32 {
    let off = log_offset();
    (10f32.powf((code - LOG_WHITE) * DENSITY_STEP / FILM_GAMMA) - off) / (1.0 - off)
}

/// Linear light to a 10-bit log code value, clamped to 0–1023.
fn linear_to_log(v: f32) -> f32 {
    let off = log_offset();
    let x = (v * (1.0 - off) + off).max(1e-10);
    (LOG_WHITE + x.log10() * FILM_GAMMA / DENSITY_STEP).clamp(0.0, 1023.0)
}

struct Reader<'a> {
    data: &'a [u8],
    big: bool,
}

impl Reader<'_> {
    fn u16(&self, at: usize) -> Result<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2).context("DPX header is cut short")?.try_into()?;
        Ok(if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32(&self, at: usize) -> Result<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4).context("DPX header is cut short")?.try_into()?;
        Ok(if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }
}

pub fn read(path: &Path) -> Result<Image> {
    let data = std::fs::read(path).with_context(|| format!("reading DPX {}", path.display()))?;
    decode(&data).with_context(|| format!("decoding DPX {}", path.display()))
}

/// How a pixel's components are laid out, by the element's descriptor.
fn layout(descriptor: u8) -> Result<&'static [usize]> {
    // Indices into RGBA for each stored component; luma fills R, G and B.
    Ok(match descriptor {
        6 => &[0],
        50 => &[0, 1, 2],
        51 => &[0, 1, 2, 3],
        52 => &[3, 2, 1, 0],
        4 => &[3],
        other => bail!("unsupported DPX descriptor {other} (RGB, RGBA, ABGR, luma and alpha are)"),
    })
}

fn decode(data: &[u8]) -> Result<Image> {
    let big = match data.get(MAGIC..MAGIC + 4) {
        Some(b"SDPX") => true,
        Some(b"XPDS") => false,
        _ => bail!("not a DPX file"),
    };
    let r = Reader { data, big };
    ensure!(r.u16(ELEMENTS)? >= 1, "no image element");
    let (w, h) = (r.u32(WIDTH)? as usize, r.u32(HEIGHT)? as usize);
    ensure!(w > 0 && h > 0 && w <= 65536 && h <= 65536, "bad size {w}×{h}");
    let el = |field: usize| ELEMENT + field;
    let descriptor = data[el(DESCRIPTOR)];
    let transfer = data[el(TRANSFER)];
    let bits = data[el(BIT_DEPTH)] as u32;
    let packing = r.u16(el(PACKING))?;
    ensure!(r.u16(el(ENCODING))? == 0, "run-length encoded DPX isn't supported");
    let offset = match r.u32(el(ELEMENT_OFFSET))? {
        0 | u32::MAX => r.u32(DATA_OFFSET)?,
        o => o,
    } as usize;
    let comps = layout(descriptor)?;
    let nc = comps.len();
    let stored = data.get(offset..).context("image data offset past the end")?;
    // Each component as a number: 0–1 for integer data, as stored for float.
    let float = matches!(bits, 32 | 64);
    let values: Vec<f32> = if float {
        unpack_float(stored, big, bits, w * nc, h)?
    } else {
        let max = ((1u64 << bits) - 1) as f32;
        unpack(stored, big, bits, packing, w * nc, h)?.into_iter().map(|v| v as f32 / max).collect()
    };
    let to_linear = |n: f32| -> f32 {
        match transfer {
            // Printing density or logarithmic: the Cineon curve.
            1 | 3 => log_to_linear(n * 1023.0),
            2 => n,
            // Float that isn't marked as video (4–10) is linear.
            t if float && !(4..=10).contains(&t) => n,
            _ => color::srgb_to_linear(n),
        }
    };
    let mut img = Image::new(w, h);
    let flip_x = matches!(r.u16(ORIENTATION)?, 1 | 3 | 5 | 7);
    let flip_y = matches!(r.u16(ORIENTATION)?, 2 | 3 | 6 | 7);
    for y in 0..h {
        for x in 0..w {
            let src = &values[(y * w + x) * nc..][..nc];
            let mut px = [0.0, 0.0, 0.0, 1.0];
            for (&c, &v) in comps.iter().zip(src) {
                if c == 3 {
                    px[3] = v;
                } else if nc == 1 {
                    px = [to_linear(v), to_linear(v), to_linear(v), 1.0];
                } else {
                    px[c] = to_linear(v);
                }
            }
            let (dx, dy) = (if flip_x { w - 1 - x } else { x }, if flip_y { h - 1 - y } else { y });
            img.set(dx, dy, color::premultiply(px));
        }
    }
    Ok(img)
}

/// `rows` lines of `per_row` components each, as stored: `bits` per
/// component, `packing` 0 (a continuous bit stream within each line), 1
/// (filled, method A: padding in the low bits) or 2 (method B: padding in
/// the high bits). Lines start on 32-bit boundaries.
fn unpack(data: &[u8], big: bool, bits: u32, packing: u16, per_row: usize, rows: usize) -> Result<Vec<u32>> {
    let word = |i: usize| -> Option<u32> {
        let b: [u8; 4] = data.get(i * 4..i * 4 + 4)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    let half = |i: usize| -> Option<u16> {
        let b: [u8; 2] = data.get(i * 2..i * 2 + 2)?.try_into().ok()?;
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let short = || anyhow::anyhow!("image data is cut short");
    let mut out = Vec::with_capacity(per_row * rows);
    match (bits, packing) {
        (8, _) => {
            let stride = per_row.div_ceil(4) * 4;
            for y in 0..rows {
                let line = data.get(y * stride..y * stride + per_row).ok_or_else(short)?;
                out.extend(line.iter().map(|&b| b as u32));
            }
        }
        (16, _) => {
            let stride = per_row.div_ceil(2) * 2;
            for y in 0..rows {
                for x in 0..per_row {
                    out.push(half(y * stride + x).ok_or_else(short)? as u32);
                }
            }
        }
        (10, 1 | 2) => {
            // Three components per 32-bit word.
            let stride = per_row.div_ceil(3);
            let shifts: [u32; 3] = if packing == 1 { [22, 12, 2] } else { [20, 10, 0] };
            for y in 0..rows {
                for x in 0..per_row {
                    let w = word(y * stride + x / 3).ok_or_else(short)?;
                    out.push((w >> shifts[x % 3]) & 0x3ff);
                }
            }
        }
        (12, 1 | 2) => {
            // One component per 16-bit half-word.
            let stride = per_row.div_ceil(2) * 2;
            for y in 0..rows {
                for x in 0..per_row {
                    let v = half(y * stride + x).ok_or_else(short)?;
                    out.push(if packing == 1 { (v >> 4) as u32 } else { (v & 0xfff) as u32 });
                }
            }
        }
        (10 | 12, 0) => {
            // A continuous stream, most significant bit first within each
            // 32-bit word; each line starts on a word.
            let stride = (per_row * bits as usize).div_ceil(32);
            for y in 0..rows {
                for x in 0..per_row {
                    let bit = x * bits as usize;
                    let (wi, off) = (y * stride + bit / 32, (bit % 32) as u32);
                    let hi = word(wi).ok_or_else(short)? as u64;
                    let lo = if off + bits > 32 { word(wi + 1).ok_or_else(short)? as u64 } else { 0 };
                    let both = (hi << 32) | lo;
                    out.push(((both >> (64 - off - bits)) & ((1 << bits) - 1)) as u32);
                }
            }
        }
        (b, p) => bail!("unsupported DPX layout: {b}-bit, packing {p}"),
    }
    Ok(out)
}

/// `rows` lines of `per_row` float components (`bits` 32 or 64).
fn unpack_float(data: &[u8], big: bool, bits: u32, per_row: usize, rows: usize) -> Result<Vec<f32>> {
    let size = bits as usize / 8;
    let need = per_row * rows * size;
    let data = data.get(..need).context("image data is cut short")?;
    Ok(data
        .chunks_exact(size)
        .map(|b| match (size, big) {
            (4, true) => f32::from_be_bytes(b.try_into().unwrap()),
            (4, false) => f32::from_le_bytes(b.try_into().unwrap()),
            (_, true) => f64::from_be_bytes(b.try_into().unwrap()) as f32,
            (_, false) => f64::from_le_bytes(b.try_into().unwrap()) as f32,
        })
        .collect())
}

/// Write `img` (its frame) as a big-endian RGB or RGBA DPX (see
/// [`Options`]): 10-bit filled method A (three components a word, lines on
/// word boundaries), 16-bit, or 32-bit float.
pub fn write(img: &Image, path: &Path, options: Options) -> Result<()> {
    let Options { encoding, alpha } = options;
    let flat = img.flat();
    let (w, h) = (flat.width, flat.height);
    let nc = if alpha { 4 } else { 3 };
    let bits: usize = match encoding {
        Encoding::Log10 | Encoding::Video10 => 10,
        Encoding::Video16 => 16,
        Encoding::Float32 => 32,
    };
    let line = match bits {
        10 => (w * nc).div_ceil(3) * 4,
        16 => (w * nc * 2).div_ceil(4) * 4,
        _ => w * nc * 4,
    };
    let mut out = vec![0u8; HEADER_SIZE + line * h];
    let size = out.len() as u32;
    let put32 = |out: &mut Vec<u8>, at: usize, v: u32| out[at..at + 4].copy_from_slice(&v.to_be_bytes());
    let put16 = |out: &mut Vec<u8>, at: usize, v: u16| out[at..at + 2].copy_from_slice(&v.to_be_bytes());
    // File information.
    out[MAGIC..MAGIC + 4].copy_from_slice(b"SDPX");
    put32(&mut out, DATA_OFFSET, HEADER_SIZE as u32);
    out[8..12].copy_from_slice(b"V2.0");
    put32(&mut out, 16, size);
    put32(&mut out, 20, 1); // new image, not a ditto of the last
    put32(&mut out, 24, 1664); // generic header size
    put32(&mut out, 28, 384); // industry header size
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = name.as_bytes();
    out[36..36 + name.len().min(99)].copy_from_slice(&name[..name.len().min(99)]);
    out[160..168].copy_from_slice(b"Arclight");
    put32(&mut out, 660, u32::MAX); // not encrypted
    // Image information: one element, top to bottom.
    put16(&mut out, ORIENTATION, 0);
    put16(&mut out, ELEMENTS, 1);
    put32(&mut out, WIDTH, w as u32);
    put32(&mut out, HEIGHT, h as u32);
    let el = |field: usize| ELEMENT + field;
    let max = ((1u64 << bits.min(16)) - 1) as u32;
    let (low, high) = match encoding {
        Encoding::Log10 => (LOG_BLACK as u32, LOG_WHITE as u32),
        Encoding::Float32 => (0, 0),
        _ => (0, max),
    };
    put32(&mut out, el(4), low); // reference low data code
    put32(&mut out, el(12), high); // reference high data code
    out[el(DESCRIPTOR)] = if alpha { 51 } else { 50 }; // RGBA or RGB
    // Printing density, Rec. 709 video, or linear (Rec. 709 primaries).
    let (transfer, colorimetric) = match encoding {
        Encoding::Log10 => (1, 1),
        Encoding::Float32 => (2, 6),
        _ => (6, 6),
    };
    out[el(TRANSFER)] = transfer;
    out[el(22)] = colorimetric;
    out[el(BIT_DEPTH)] = bits as u8;
    put16(&mut out, el(PACKING), if bits == 10 { 1 } else { 0 }); // 10-bit: filled, method A
    put16(&mut out, el(ENCODING), 0);
    put32(&mut out, el(ELEMENT_OFFSET), HEADER_SIZE as u32);
    // The other seven elements and the orientation/film/TV headers stay zero.
    // A color component as stored (alpha is always linear).
    let code = |v: f32| -> u32 {
        match encoding {
            Encoding::Log10 => linear_to_log(v).round() as u32,
            Encoding::Float32 => v.to_bits(),
            _ => (color::linear_to_srgb(v).clamp(0.0, 1.0) * max as f32).round() as u32,
        }
    };
    let alpha_code = |a: f32| -> u32 {
        match encoding {
            Encoding::Float32 => a.to_bits(),
            _ => (a.clamp(0.0, 1.0) * max as f32).round() as u32,
        }
    };
    let mut row = Vec::with_capacity(w * nc);
    for y in 0..h {
        row.clear();
        for px in &flat.pixels[y * w..(y + 1) * w] {
            // With alpha: straight color. Without: premultiplied, over black.
            let c = if alpha { color::unpremultiply(*px) } else { *px };
            row.extend(c.iter().take(3).map(|&v| code(v)));
            if alpha {
                row.push(alpha_code(c[3]));
            }
        }
        let at = HEADER_SIZE + y * line;
        match bits {
            10 => {
                for (k, three) in row.chunks(3).enumerate() {
                    let word = three.iter().zip([22, 12, 2]).fold(0, |wd, (&v, s)| wd | (v << s));
                    put32(&mut out, at + k * 4, word);
                }
            }
            16 => {
                for (k, &v) in row.iter().enumerate() {
                    put16(&mut out, at + k * 2, v as u16);
                }
            }
            _ => {
                for (k, &v) in row.iter().enumerate() {
                    put32(&mut out, at + k * 4, v);
                }
            }
        }
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, out).with_context(|| format!("writing DPX {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_curve_round_trips_and_anchors() {
        assert!(log_to_linear(LOG_BLACK).abs() < 1e-6, "95 is black");
        assert!((log_to_linear(LOG_WHITE) - 1.0).abs() < 1e-6, "685 is white");
        for v in [0.0, 0.01, 0.18, 1.0, 4.0] {
            assert!((log_to_linear(linear_to_log(v)) - v).abs() < 1e-3 * v.max(1.0), "{v}");
        }
    }

    #[test]
    fn written_dpx_reads_back_including_highlights() {
        let mut img = Image::new(5, 3);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let v = i as f32 * 0.3;
            *p = [v, 0.18, 0.02 * i as f32, 1.0];
        }
        let dir = std::env::temp_dir().join(format!("arclight_dpx_{}", std::process::id()));
        let path = dir.join("frame.dpx");
        write(&img, &path, Options::default()).unwrap();
        let back = read(&path).unwrap();
        assert_eq!((back.width, back.height), (5, 3));
        for (a, b) in img.pixels.iter().zip(&back.pixels) {
            for c in 0..3 {
                // A 10-bit log step is about 1% of the value.
                assert!((a[c] - b[c]).abs() <= 0.012 * a[c].max(0.05), "{a:?} vs {b:?}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn video_coded_dpx_round_trips_and_clips_highlights() {
        let mut img = Image::new(7, 2); // odd width: 16-bit lines get padded
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let v = i as f32 / 13.0;
            *p = [v, 0.18, 1.0 - v * 0.5, 1.0];
        }
        img.pixels[13] = [3.0, 0.5, 0.0, 1.0]; // a highlight video can't hold
        let dir = std::env::temp_dir().join(format!("arclight_dpxv_{}", std::process::id()));
        for (enc, bits, tol) in [(Encoding::Video10, 10, 2e-3), (Encoding::Video16, 16, 1e-4)] {
            let path = dir.join(format!("v{bits}.dpx"));
            write(&img, &path, Options { encoding: enc, alpha: false }).unwrap();
            let data = std::fs::read(&path).unwrap();
            assert_eq!((data[ELEMENT + TRANSFER], data[ELEMENT + BIT_DEPTH]), (6, bits), "Rec. 709 video, {bits}-bit");
            let back = read(&path).unwrap();
            for (i, (a, b)) in img.pixels.iter().zip(&back.pixels).enumerate() {
                let want = if i == 13 { [1.0, 0.5, 0.0] } else { [a[0], a[1], a[2]] };
                // Compare in the coded (sRGB) domain, where the steps are even.
                let err = (0..3).map(|c| (color::linear_to_srgb(want[c]) - color::linear_to_srgb(b[c])).abs()).fold(0.0, f32::max);
                assert!(err < tol, "{bits}-bit pixel {i}: {a:?} vs {b:?}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_encoding_with_and_without_alpha_reads_back() {
        // Half-transparent pixels and a highlight, at an odd width (lines of
        // 10-bit RGBA end mid-word).
        let mut img = Image::new(5, 2);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let a = [1.0, 0.5, 0.25, 0.75, 1.0][i % 5];
            *p = color::premultiply([0.05 + 0.08 * i as f32, 0.18, 0.6, a]);
        }
        img.pixels[9] = [3.5, 0.5, 0.25, 1.0]; // above 1.0
        let dir = std::env::temp_dir().join(format!("arclight_dpxa_{}", std::process::id()));
        for encoding in [Encoding::Log10, Encoding::Video10, Encoding::Video16, Encoding::Float32] {
            for alpha in [false, true] {
                let path = dir.join(format!("{encoding:?}_{alpha}.dpx"));
                write(&img, &path, Options { encoding, alpha }).unwrap();
                let back = read(&path).unwrap();
                for (i, (a, b)) in img.pixels.iter().zip(&back.pixels).enumerate() {
                    // Without alpha the color is over black (premultiplied),
                    // and comes back opaque.
                    let want = if alpha { *a } else { [a[0], a[1], a[2], 1.0] };
                    // Video clips at 1.0; float is exact; the rest within a
                    // code step (log steps are ~1% of the value).
                    let want = match encoding {
                        Encoding::Video10 | Encoding::Video16 => {
                            let s = color::unpremultiply(want);
                            let c = s.map(|v| v.min(1.0));
                            if alpha { color::premultiply([c[0], c[1], c[2], s[3]]) } else { [c[0], c[1], c[2], 1.0] }
                        }
                        _ => want,
                    };
                    let tol = |v: f32| match encoding {
                        Encoding::Float32 => 1e-6,
                        Encoding::Log10 => 0.012 * v.abs().max(0.05),
                        Encoding::Video10 => 0.004,
                        Encoding::Video16 => 1e-4,
                    };
                    for c in 0..4 {
                        assert!((want[c] - b[c]).abs() <= tol(want[c]), "{encoding:?} alpha {alpha}, pixel {i}: {want:?} vs {b:?}");
                    }
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacks_every_layout() {
        // Two rows of four 10-bit components, filled method A: two words a
        // row (the second half empty).
        let mut a = Vec::new();
        for row in [[1u32, 2, 3, 4], [5, 6, 7, 8]] {
            a.extend(((row[0] << 22) | (row[1] << 12) | (row[2] << 2)).to_be_bytes());
            a.extend((row[3] << 22).to_be_bytes());
        }
        assert_eq!(unpack(&a, true, 10, 1, 4, 2).unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
        // Method B, little-endian.
        let w = (9u32 << 20) | (10 << 10) | 11;
        assert_eq!(unpack(&w.to_le_bytes(), false, 10, 2, 3, 1).unwrap(), [9, 10, 11]);
        // A continuous 10-bit stream: four components span 40 bits.
        let stream: u64 = (1 << 54) | (2 << 44) | (3 << 34) | (4 << 24);
        let bytes = stream.to_be_bytes();
        assert_eq!(unpack(&bytes, true, 10, 0, 4, 1).unwrap(), [1, 2, 3, 4]);
        // 12-bit method A in 16-bit halves; 16-bit; 8-bit with line padding.
        assert_eq!(unpack(&[0x12, 0x30, 0xab, 0xc0], true, 12, 1, 2, 1).unwrap(), [0x123, 0xabc]);
        assert_eq!(unpack(&[0xff, 0xff, 0x00, 0x01], true, 16, 0, 2, 1).unwrap(), [65535, 1]);
        assert_eq!(unpack(&[1, 2, 3, 0, 4, 5, 6, 0], true, 8, 1, 3, 2).unwrap(), [1, 2, 3, 4, 5, 6]);
    }
}
