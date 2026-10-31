//! Time-based media: numbered image sequences and video files.
//!
//! Sequences are addressed by a path pattern — `plate.####.exr` (one `#` per
//! digit of padding) or printf-style `plate.%04d.exr`. Video files are decoded
//! and encoded by piping raw RGBA frames through the system `ffmpeg`, so no
//! codec libraries are linked into Arclight.

use crate::color;
use crate::image::Image;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, LazyLock, Mutex};

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mxf", "mpg", "mpeg"];

/// Where to run an external tool (`ffmpeg`, `ffprobe`) from: next to this
/// executable (a bundled copy), else on `PATH`, else the usual package
/// manager prefixes. Apps opened from the macOS Finder get a bare `PATH`
/// (`/usr/bin:/bin:…`) without Homebrew's or MacPorts' bin directory.
pub fn tool(name: &str) -> PathBuf {
    static FOUND: LazyLock<Mutex<HashMap<String, PathBuf>>> = LazyLock::new(Default::default);
    let mut found = FOUND.lock().unwrap();
    if let Some(p) = found.get(name) {
        return p.clone();
    }
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name)));
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join(name));
    let usual = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"].map(|d| Path::new(d).join(name));
    let path = beside.into_iter().chain(on_path).chain(usual).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(name));
    found.insert(name.to_string(), path.clone());
    path
}

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Sequence patterns
// ---------------------------------------------------------------------------

/// A parsed sequence pattern: `prefix` + zero-padded frame number + `suffix`.
struct Pattern<'a> {
    prefix: &'a str,
    pad: usize,
    suffix: &'a str,
}

fn parse_pattern(s: &str) -> Option<Pattern<'_>> {
    // `#` run: the last one in the string, so directories containing `#` are rare
    // false positives rather than silently wrong.
    if let Some(end) = s.rfind('#') {
        let start = s[..=end].trim_end_matches('#').len();
        return Some(Pattern {
            prefix: &s[..start],
            pad: end + 1 - start,
            suffix: &s[end + 1..],
        });
    }
    // printf style: %d or %0Nd
    let pct = s.rfind('%')?;
    let rest = &s[pct + 1..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let after = &rest[digits.len()..];
    if !after.starts_with('d') {
        return None;
    }
    let pad = if digits.is_empty() { 1 } else { digits.parse().ok()? };
    Some(Pattern {
        prefix: &s[..pct],
        pad,
        suffix: &after[1..],
    })
}

pub fn is_sequence_pattern(s: &str) -> bool {
    parse_pattern(s).is_some()
}

/// Substitute a frame number into a sequence pattern. Non-patterns are returned
/// unchanged, so a still image path works everywhere a pattern does.
pub fn frame_path(pattern: &str, frame: i64) -> PathBuf {
    match parse_pattern(pattern) {
        Some(p) => PathBuf::from(format!("{}{:0pad$}{}", p.prefix, frame, p.suffix, pad = p.pad)),
        None => PathBuf::from(pattern),
    }
}

/// The file for `frame` of a sequence, or, when it's missing, the nearest
/// one before it within `first..=last` (else the nearest after): a gap in
/// a sequence holds the last frame instead of failing. Stills and complete
/// sequences get `frame_path`.
pub fn existing_frame_path(pattern: &str, frame: i64, first: i64, last: i64) -> PathBuf {
    let want = frame_path(pattern, frame);
    if want.exists() || !is_sequence_pattern(pattern) {
        return want;
    }
    let before = (first..frame).rev().map(|f| frame_path(pattern, f));
    let after = (frame + 1..=last).map(|f| frame_path(pattern, f));
    before.chain(after).find(|p| p.exists()).unwrap_or(want)
}

/// Scan the pattern's directory for matching files and return the frame range.
pub fn detect_sequence_range(pattern: &str) -> Option<(i64, i64)> {
    let p = parse_pattern(pattern)?;
    let prefix_path = Path::new(p.prefix);
    // A prefix ending in a separator means the frame number starts the file name.
    let (dir, name_prefix) = if p.prefix.ends_with(std::path::MAIN_SEPARATOR) {
        (prefix_path.to_path_buf(), String::new())
    } else {
        (
            prefix_path.parent().map(Path::to_path_buf).unwrap_or_default(),
            prefix_path.file_name()?.to_string_lossy().into_owned(),
        )
    };
    let dir = if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir };

    let mut range: Option<(i64, i64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(mid) = name
            .strip_prefix(name_prefix.as_str())
            .and_then(|r| r.strip_suffix(p.suffix))
        else {
            continue;
        };
        if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) || mid.len() < p.pad {
            continue;
        }
        if let Ok(f) = mid.parse::<i64>() {
            range = Some(match range {
                Some((a, b)) => (a.min(f), b.max(f)),
                None => (f, f),
            });
        }
    }
    range
}

/// Given one frame of a sequence (e.g. `shot.0012.exr`), return the `####`
/// pattern and the detected frame range. Returns `None` for files with no
/// frame number, which should be treated as stills.
pub fn sequence_from_file(path: &Path) -> Option<(String, i64, i64)> {
    let name = path.file_name()?.to_str()?;
    let stem_len = name.rfind('.').unwrap_or(name.len());
    let stem = &name[..stem_len];
    let num_len = stem.bytes().rev().take_while(u8::is_ascii_digit).count();
    if num_len == 0 {
        return None;
    }
    let start = stem.len() - num_len;
    let pattern_name = format!("{}{}{}", &stem[..start], "#".repeat(num_len), &name[stem_len..]);
    let pattern = path.with_file_name(pattern_name).to_string_lossy().into_owned();
    let (first, last) = detect_sequence_range(&pattern)?;
    Some((pattern, first, last))
}

// ---------------------------------------------------------------------------
// Video probing and decoding (ffmpeg)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    pub frames: usize,
    /// Bits per component of the stream (8, 10, 12, …). Deeper than 8 is
    /// decoded at 16 bits so none of it is lost.
    pub bits: u32,
}

/// Bits per component of an ffmpeg pixel format, from its name:
/// `yuv420p10le` 10, `p010le` 10, `rgb48le` 16, `gbrpf32le` 32,
/// `yuv420p` / `nv12` / `rgb24` 8.
pub fn pix_fmt_bits(name: &str) -> u32 {
    let base = name.strip_suffix("le").or_else(|| name.strip_suffix("be")).unwrap_or(name);
    let digits = base.len() - base.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let n: u32 = base[base.len() - digits..].parse().unwrap_or(0);
    match n {
        // nv12, nv16, … name their chroma layout, not a depth.
        9..=16 if !base.starts_with("nv") => n,
        // Whole-pixel sizes: rgb48 / rgba64 are 16 a component.
        48 | 64 => 16,
        32 if base.contains('f') => 32,
        _ => 8,
    }
}

static PROBES: LazyLock<Mutex<HashMap<PathBuf, VideoInfo>>> = LazyLock::new(Default::default);

fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0.0 && d > 0.0).then(|| n / d)
}

/// Read a video's resolution, frame rate and frame count with `ffprobe`.
pub fn probe_video(path: &Path) -> Result<VideoInfo> {
    if let Some(info) = PROBES.lock().unwrap().get(path) {
        return Ok(*info);
    }
    let out = Command::new(tool("ffprobe"))
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height,r_frame_rate,avg_frame_rate,nb_frames,duration,pix_fmt,bits_per_raw_sample:format=duration")
        .args(["-of", "json"])
        .arg(path)
        .output()
        .context("running ffprobe (is ffmpeg installed?)")?;
    if !out.status.success() {
        bail!("ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("parsing ffprobe output")?;
    let s = v["streams"]
        .get(0)
        .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?;
    let width = s["width"].as_u64().unwrap_or(0) as usize;
    let height = s["height"].as_u64().unwrap_or(0) as usize;
    let fps = s["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .or_else(|| s["r_frame_rate"].as_str().and_then(parse_rate))
        .unwrap_or(24.0);
    let num = |x: &serde_json::Value| x.as_str().and_then(|t| t.parse::<f64>().ok());
    let frames = num(&s["nb_frames"])
        .or_else(|| num(&s["duration"]).map(|d| (d * fps).round()))
        .or_else(|| num(&v["format"]["duration"]).map(|d| (d * fps).round()))
        .unwrap_or(1.0)
        .max(1.0) as usize;
    if width == 0 || height == 0 {
        bail!("could not read the resolution of {}", path.display());
    }
    // The pixel format names the depth; bits_per_raw_sample backs it up.
    let bits = s["pix_fmt"].as_str().map_or(8, pix_fmt_bits).max(num(&s["bits_per_raw_sample"]).map_or(0, |b| b as u32));
    let info = VideoInfo { width, height, fps, frames, bits };
    PROBES.lock().unwrap().insert(path.to_path_buf(), info);
    Ok(info)
}

/// A running ffmpeg decode, kept alive so sequential frames stream without
/// re-seeking. A jump a little way ahead (playback skipping frames to keep
/// time) reads on to it; any other restarts ffmpeg at the new position.
/// A thread reads and converts the next frame while the last is used (a 4K
/// frame takes tens of milliseconds each to decode and to convert), holding
/// it until asked: one frame ahead, no more.
struct Decoder {
    info: VideoInfo,
    child: Option<Stream>,
    next: usize,
    last: Option<(usize, Arc<Image>)>,
}

/// The ffmpeg decoding, and its frames from the reader thread: each with
/// its index. The reader doesn't convert frames before `want`.
struct Stream {
    child: Child,
    frames: Receiver<(usize, std::io::Result<Image>)>,
    want: Arc<AtomicUsize>,
}

/// How far ahead a frame can be to be read on to rather than sought: a seek
/// restarts at a key frame, costing as much as decoding a second or so.
const READ_AHEAD: usize = 24;

impl Decoder {
    fn stop(&mut self) {
        if let Some(Stream { mut child, frames, .. }) = self.child.take() {
            // The reader stops at the end of the pipe, or when it can't hand
            // a frame over.
            drop(frames);
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn start(&mut self, path: &Path, index: usize) -> Result<()> {
        self.stop();
        // Seek a quarter frame early so rounding never skips the wanted frame.
        let t = ((index as f64 - 0.25) / self.info.fps).max(0.0);
        let mut child = Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-nostdin", "-ss", &format!("{t:.6}"), "-i"])
            .arg(path)
            // The first video stream only (cameras add audio and data streams).
            .args(["-map", "0:v:0", "-an", "-sn", "-dn"])
            .args(["-f", "rawvideo", "-pix_fmt", if self.info.bits > 8 { "rgba64le" } else { "rgba" }, "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting ffmpeg (is it installed?)")?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (w, h, deep) = (self.info.width, self.info.height, self.info.bits > 8);
        let (tx, frames) = mpsc::sync_channel(0);
        let want = Arc::new(AtomicUsize::new(index));
        let wanted = want.clone();
        let reader = std::thread::Builder::new().name("video decode".into()).spawn(move || {
            let mut buf = vec![0u8; w * h * if deep { 8 } else { 4 }];
            for i in index.. {
                let read = stdout.read_exact(&mut buf);
                if read.is_ok() && i < wanted.load(Ordering::Acquire) {
                    continue;
                }
                let read = read.map(|_| if deep { rgba16_to_image(&buf, w, h) } else { rgba8_to_image(&buf, w, h) });
                let end = read.is_err();
                if tx.send((i, read)).is_err() || end {
                    break;
                }
            }
        });
        if let Err(e) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e).context("starting a video reader");
        }
        self.child = Some(Stream { child, frames, want });
        self.next = index;
        Ok(())
    }

    fn read(&mut self, path: &Path, index: usize) -> Result<Arc<Image>> {
        if let Some((i, img)) = &self.last {
            if *i == index {
                return Ok(img.clone());
            }
        }
        if self.child.is_none() || index < self.next || index > self.next + READ_AHEAD {
            self.start(path, index)?;
        }
        let stream = self.child.as_ref().unwrap();
        stream.want.store(index, Ordering::Release);
        // Frames before it (one read ahead already) are passed over.
        let read = loop {
            match stream.frames.recv() {
                Ok((i, Ok(_))) if i < index => continue,
                Ok((_, read)) => break read,
                Err(_) => break Err(std::io::ErrorKind::UnexpectedEof.into()),
            }
        };
        let img = match read {
            Ok(img) => Arc::new(img),
            Err(e) => {
                self.stop();
                // Frame counts from container metadata can overshoot by a
                // frame or two; hold the last decoded frame rather than failing.
                if let Some((_, img)) = &self.last {
                    return Ok(img.clone());
                }
                return Err(e).with_context(|| format!("decoding frame {index} of {}", path.display()));
            }
        };
        self.next = index + 1;
        self.last = Some((index, img.clone()));
        Ok(img)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();
    }
}

static DECODERS: LazyLock<Mutex<HashMap<PathBuf, Decoder>>> = LazyLock::new(Default::default);

/// Decode frame `index` (0-based) of a video file.
pub fn read_video_frame(path: &Path, index: usize) -> Result<Arc<Image>> {
    let info = probe_video(path)?;
    let index = index.min(info.frames - 1);
    let mut decoders = DECODERS.lock().unwrap();
    let dec = decoders.entry(path.to_path_buf()).or_insert_with(|| Decoder {
        info,
        child: None,
        next: 0,
        last: None,
    });
    dec.read(path, index)
}

/// 16-bit little-endian sRGB RGBA (straight alpha) -> linear premultiplied
/// float, for sources deeper than 8 bits.
fn rgba16_to_image(buf: &[u8], w: usize, h: usize) -> Image {
    use rayon::prelude::*;
    static LUT: LazyLock<Vec<f32>> = LazyLock::new(|| (0..65536).map(|v| color::srgb_to_linear(v as f32 / 65535.0)).collect());
    let mut img = Image::new(w, h);
    img.pixels
        .par_iter_mut()
        .zip(buf.par_chunks_exact(8))
        .for_each(|(px, s)| {
            let c = |i: usize| u16::from_le_bytes([s[i * 2], s[i * 2 + 1]]) as usize;
            *px = color::premultiply([LUT[c(0)], LUT[c(1)], LUT[c(2)], c(3) as f32 / 65535.0]);
        });
    img
}

/// 8-bit sRGB RGBA (straight alpha) -> linear premultiplied float.
fn rgba8_to_image(buf: &[u8], w: usize, h: usize) -> Image {
    use rayon::prelude::*;
    // Pre-computed decode table: 256 lookups beat 3 powf calls per pixel.
    let lut: Vec<f32> = (0..256).map(|v| color::srgb_to_linear(v as f32 / 255.0)).collect();
    let mut img = Image::new(w, h);
    img.pixels
        .par_iter_mut()
        .zip(buf.par_chunks_exact(4))
        .for_each(|(px, s)| {
            *px = color::premultiply([
                lut[s[0] as usize],
                lut[s[1] as usize],
                lut[s[2] as usize],
                s[3] as f32 / 255.0,
            ]);
        });
    img
}

// ---------------------------------------------------------------------------
// Video encoding
// ---------------------------------------------------------------------------

/// Media Out's codec choices.
pub const VIDEO_CODECS: &[&str] = &[
    "auto", "H.264 8-bit", "HEVC 10-bit", "ProRes 422 HQ 10-bit", "ProRes 4444 + alpha",
    "DNxHR HQX 10-bit", "DNxHR 444 10-bit", "VP9 10-bit", "VP9 + alpha", "AV1 10-bit",
];

/// The video containers Media Out writes.
pub const VIDEO_WRITE_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "mxf", "m4v", "avi"];

/// The image sequence formats Media Out writes (see [`Image::write_as`]).
pub const SEQUENCE_WRITE_EXTS: &[&str] = &["exr", "dpx", "tif", "png", "jpg", "webp", "tga"];

/// Every format Media Out writes, as (name, extension): the sequences
/// (`SEQUENCE_WRITE_EXTS`), then the videos (`VIDEO_WRITE_EXTS`).
pub const OUTPUT_FORMATS: &[(&str, &str)] = &[
    ("EXR", "exr"), ("DPX", "dpx"), ("TIFF", "tif"), ("PNG", "png"), ("JPEG", "jpg"), ("WebP", "webp"), ("TGA", "tga"),
    ("MP4", "mp4"), ("MOV", "mov"), ("MKV", "mkv"), ("WebM", "webm"), ("MXF", "mxf"), ("M4V", "m4v"), ("AVI", "avi"),
];

/// The output format (an extension of `OUTPUT_FORMATS`) a Media Out path
/// is written as, if it names one (`.tiff` and `.jpeg` count as `tif`
/// and `jpg`).
pub fn output_format(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    let ext = match ext.as_str() {
        "tiff" => "tif",
        "jpeg" => "jpg",
        e => e,
    };
    OUTPUT_FORMATS.iter().map(|&(_, e)| e).find(|&e| e == ext)
}

/// `path` written as format `ext` instead: its extension replaced, and a
/// frame pattern added for a sequence (`shot.mov` → `shot.####.exr`) or
/// dropped for a video (`shot_%04d.png` → `shot.mov`); one already there
/// stays as typed. An empty path gives `render.####.exr`, `render.mov`….
pub fn with_format(path: &str, ext: &str) -> String {
    let cut = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let (dir, name) = path.split_at(cut);
    // An extension, unless what follows the dot is a frame pattern.
    let stem = match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() && !e.contains(['#', '%']) => stem,
        _ => name,
    };
    // A trailing frame pattern: #### or %04d, with the . or _ before it.
    let digits = stem.trim_end_matches('#');
    let base = if digits.len() < stem.len() {
        digits
    } else {
        match stem.strip_suffix('d').and_then(|s| s.rsplit_once('%')) {
            Some((before, pad)) if pad.chars().all(|c| c.is_ascii_digit()) => before,
            _ => stem,
        }
    };
    let pattern = base.len() < stem.len();
    let base = if pattern { base.strip_suffix(['.', '_']).unwrap_or(base) } else { base };
    let base = if base.is_empty() { "render" } else { base };
    if VIDEO_WRITE_EXTS.contains(&ext) {
        format!("{dir}{base}.{ext}")
    } else if pattern {
        format!("{dir}{stem}.{ext}")
    } else {
        format!("{dir}{base}.####.{ext}")
    }
}

/// Media Out's codec choices for a video in a `.ext` file: auto, then
/// those that container holds.
pub fn video_codec_choices(ext: &str) -> Vec<&'static str> {
    VIDEO_CODECS.iter().copied().filter(|&c| c == "auto" || VideoCodec::ALL.iter().any(|v| v.name() == c && v.containers().contains(&ext))).collect()
}

/// What a video file is encoded as.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VideoCodec {
    H264,
    Hevc10,
    ProRes422Hq,
    ProRes4444,
    DnxhrHqx,
    Dnxhr444,
    Vp9,
    /// 8-bit 4:2:0 with alpha: the transparent video browsers play.
    Vp9Alpha,
    Av1,
}

impl VideoCodec {
    const ALL: [VideoCodec; 9] = [
        VideoCodec::H264, VideoCodec::Hevc10, VideoCodec::ProRes422Hq, VideoCodec::ProRes4444,
        VideoCodec::DnxhrHqx, VideoCodec::Dnxhr444, VideoCodec::Vp9, VideoCodec::Vp9Alpha, VideoCodec::Av1,
    ];

    /// The codec for a `VIDEO_CODECS` choice; "auto" picks by container:
    /// ProRes 422 HQ in `.mov`, VP9 in `.webm`, DNxHR HQX in `.mxf`,
    /// H.264 elsewhere. Err for a container Media Out can't write, or one
    /// the codec can't go in.
    pub fn choose(choice: &str, path: &Path) -> Result<VideoCodec> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let dotted = |exts: &[&str]| {
            let all: Vec<String> = exts.iter().map(|e| format!(".{e}")).collect();
            match all.split_last() {
                Some((last, [])) => last.clone(),
                Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
                None => String::new(),
            }
        };
        if !VIDEO_WRITE_EXTS.contains(&ext.as_str()) {
            bail!("can't write a .{ext} video: use {}", dotted(VIDEO_WRITE_EXTS));
        }
        let codec = match choice {
            "auto" | "" => match ext.as_str() {
                "mov" => VideoCodec::ProRes422Hq,
                "webm" => VideoCodec::Vp9,
                "mxf" => VideoCodec::DnxhrHqx,
                _ => VideoCodec::H264,
            },
            name => *VideoCodec::ALL.iter().find(|c| c.name() == name).ok_or_else(|| anyhow!("unknown codec '{name}'"))?,
        };
        if !codec.containers().contains(&ext.as_str()) {
            bail!("{} can't go in a .{ext} file: use {}", codec.name(), dotted(codec.containers()));
        }
        Ok(codec)
    }

    /// Its name among `VIDEO_CODECS`.
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264 8-bit",
            VideoCodec::Hevc10 => "HEVC 10-bit",
            VideoCodec::ProRes422Hq => "ProRes 422 HQ 10-bit",
            VideoCodec::ProRes4444 => "ProRes 4444 + alpha",
            VideoCodec::DnxhrHqx => "DNxHR HQX 10-bit",
            VideoCodec::Dnxhr444 => "DNxHR 444 10-bit",
            VideoCodec::Vp9 => "VP9 10-bit",
            VideoCodec::Vp9Alpha => "VP9 + alpha",
            VideoCodec::Av1 => "AV1 10-bit",
        }
    }

    /// The containers it can go in (VP9's alpha only survives in WebM and
    /// Matroska).
    pub fn containers(self) -> &'static [&'static str] {
        match self {
            VideoCodec::H264 => &["mp4", "m4v", "mov", "mkv", "avi"],
            VideoCodec::Hevc10 => &["mp4", "mov", "mkv"],
            VideoCodec::ProRes422Hq | VideoCodec::ProRes4444 => &["mov", "mkv"],
            VideoCodec::DnxhrHqx | VideoCodec::Dnxhr444 => &["mov", "mxf", "mkv"],
            VideoCodec::Vp9 => &["webm", "mkv", "mp4"],
            VideoCodec::Vp9Alpha => &["webm", "mkv"],
            VideoCodec::Av1 => &["mp4", "mkv", "webm"],
        }
    }

    /// The ffmpeg encoders that make it, the preferred first.
    fn encoders(self) -> &'static [&'static str] {
        match self {
            VideoCodec::H264 => &["libx264"],
            VideoCodec::Hevc10 => &["libx265"],
            VideoCodec::ProRes422Hq | VideoCodec::ProRes4444 => &["prores_ks"],
            VideoCodec::DnxhrHqx | VideoCodec::Dnxhr444 => &["dnxhd"],
            VideoCodec::Vp9 | VideoCodec::Vp9Alpha => &["libvpx-vp9"],
            VideoCodec::Av1 => &["libsvtav1", "libaom-av1"],
        }
    }

    /// The encoder the installed ffmpeg has for it. Err naming the
    /// encoder(s) it needs if it has none.
    pub fn encoder(self) -> Result<&'static str> {
        self.encoder_in(ffmpeg_encoders().as_ref().map_err(|e| anyhow!("{e}"))?)
    }

    /// The encoder for it among `have`, an ffmpeg's encoders.
    fn encoder_in(self, have: &[String]) -> Result<&'static str> {
        let wanted = self.encoders();
        wanted.iter().copied().find(|e| have.iter().any(|h| h == e)).ok_or_else(|| {
            let names = wanted.join(" or ");
            anyhow!("{} needs ffmpeg's {names} encoder, which this ffmpeg ({}) doesn't have", self.name(), tool("ffmpeg").display())
        })
    }

    /// Fed 16-bit frames, so the encode keeps more than 8 bits.
    fn deep(self) -> bool {
        !matches!(self, VideoCodec::H264 | VideoCodec::Vp9Alpha)
    }

    /// Keeps alpha (fed straight color).
    fn alpha(self) -> bool {
        matches!(self, VideoCodec::ProRes4444 | VideoCodec::Vp9Alpha)
    }

    /// The smallest frame its encoder takes.
    fn min_size(self) -> (usize, usize) {
        match self {
            VideoCodec::DnxhrHqx | VideoCodec::Dnxhr444 => (256, 120),
            _ => (1, 1),
        }
    }

    /// The encoder's pixel format.
    fn pix_fmt(self) -> &'static str {
        match self {
            VideoCodec::H264 => "yuv420p",
            VideoCodec::Hevc10 | VideoCodec::Vp9 | VideoCodec::Av1 => "yuv420p10le",
            VideoCodec::ProRes422Hq | VideoCodec::DnxhrHqx => "yuv422p10le",
            VideoCodec::ProRes4444 => "yuva444p10le",
            VideoCodec::Dnxhr444 => "yuv444p10le",
            VideoCodec::Vp9Alpha => "yuva420p",
        }
    }

    /// The encode's arguments, with `encoder` (one of its `encoders`).
    fn args(self, encoder: &str) -> &'static [&'static str] {
        match (self, encoder) {
            (VideoCodec::H264, _) => &["-c:v", "libx264", "-crf", "16", "-preset", "medium", "-pix_fmt", "yuv420p"],
            (VideoCodec::Hevc10, _) => &[
                "-c:v", "libx265", "-crf", "16", "-preset", "medium", "-pix_fmt", "yuv420p10le",
                "-x265-params", "log-level=error", "-tag:v", "hvc1",
            ],
            (VideoCodec::ProRes422Hq, _) => &["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"],
            (VideoCodec::ProRes4444, _) => &["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuva444p10le", "-alpha_bits", "16"],
            (VideoCodec::DnxhrHqx, _) => &["-c:v", "dnxhd", "-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"],
            (VideoCodec::Dnxhr444, _) => &["-c:v", "dnxhd", "-profile:v", "dnxhr_444", "-pix_fmt", "yuv444p10le"],
            (VideoCodec::Vp9, _) => &["-c:v", "libvpx-vp9", "-crf", "18", "-b:v", "0", "-row-mt", "1", "-pix_fmt", "yuv420p10le"],
            (VideoCodec::Vp9Alpha, _) => &["-c:v", "libvpx-vp9", "-crf", "18", "-b:v", "0", "-row-mt", "1", "-auto-alt-ref", "0", "-pix_fmt", "yuva420p"],
            (VideoCodec::Av1, "libsvtav1") => &["-c:v", "libsvtav1", "-crf", "22", "-preset", "6", "-pix_fmt", "yuv420p10le"],
            (VideoCodec::Av1, _) => &["-c:v", "libaom-av1", "-crf", "22", "-b:v", "0", "-cpu-used", "6", "-row-mt", "1", "-pix_fmt", "yuv420p10le"],
        }
    }
}

/// The installed ffmpeg's encoders (`ffmpeg -encoders`, read once).
fn ffmpeg_encoders() -> &'static Result<Vec<String>, String> {
    static ENCODERS: LazyLock<Result<Vec<String>, String>> = LazyLock::new(|| {
        let out = Command::new(tool("ffmpeg")).args(["-hide_banner", "-encoders"]).output().map_err(|e| format!("running ffmpeg (is it installed?): {e}"))?;
        // After the legend, " V....D libx264   description" per encoder.
        let text = String::from_utf8_lossy(&out.stdout);
        let list = text.lines().skip_while(|l| !l.trim_start().starts_with("---")).skip(1);
        Ok(list.filter_map(|l| l.split_whitespace().nth(1).map(str::to_string)).collect())
    });
    &ENCODERS
}

/// The sound muxed with a video: `secs` of `path` from `from_secs`,
/// sped up or slowed by `tempo` (source seconds per second of video),
/// starting `delay_secs` into the video, silent after it ends, and cut
/// to `total_secs` (the video's length).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    pub path: PathBuf,
    pub from_secs: f64,
    pub secs: f64,
    pub tempo: f64,
    pub delay_secs: f64,
    pub total_secs: f64,
}

/// The audio encoder and its arguments for a video in a `.ext` file: AAC
/// in MP4 / M4V, FLAC in Matroska (AAC's priming there plays 21 ms
/// early), 24-bit PCM at 48 kHz in QuickTime, MXF and AVI, Opus at
/// 48 kHz in WebM.
fn audio_codec(ext: &str) -> (&'static str, &'static [&'static str]) {
    match ext {
        "mov" | "mxf" | "avi" => ("pcm_s24le", &["-c:a", "pcm_s24le", "-ar", "48000"]),
        "mkv" => ("flac", &["-c:a", "flac", "-sample_fmt", "s32"]),
        "webm" => ("libopus", &["-c:a", "libopus", "-b:a", "192k", "-ar", "48000"]),
        _ => ("aac", &["-c:a", "aac", "-b:a", "256k"]),
    }
}

/// The audio encoder a video at `path` needs, if the installed ffmpeg has
/// it; Err naming it if not.
pub fn audio_encoder(path: &Path) -> Result<&'static str> {
    audio_encoder_in(path, ffmpeg_encoders().as_ref().map_err(|e| anyhow!("{e}"))?)
}

fn audio_encoder_in(path: &Path, have: &[String]) -> Result<&'static str> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let (encoder, _) = audio_codec(&ext);
    if !have.iter().any(|h| h == encoder) {
        bail!("sound in a .{ext} file needs ffmpeg's {encoder} encoder, which this ffmpeg ({}) doesn't have", tool("ffmpeg").display());
    }
    Ok(encoder)
}

/// The audio filters for `track`: its tempo (atempo takes 0.5 to 2 a
/// step), its delay, then silence to the video's end and no further
/// (-shortest overshoots: ffmpeg buffers the longer stream), counted in
/// samples (after atempo and adelay the timestamps are off).
fn audio_filters(track: &AudioTrack) -> String {
    let mut parts = Vec::new();
    let mut t = track.tempo;
    if (t - 1.0).abs() > 1e-9 {
        while t > 2.0 {
            parts.push("atempo=2".to_string());
            t /= 2.0;
        }
        while t < 0.5 {
            parts.push("atempo=0.5".to_string());
            t /= 0.5;
        }
        parts.push(format!("atempo={t}"));
    }
    if track.delay_secs > 0.0 {
        parts.push(format!("adelay={}:all=1", track.delay_secs * 1000.0));
    }
    parts.push(format!("asetpts=N/SR/TB,apad=whole_dur={0},atrim=end={0}", track.total_secs));
    parts.join(",")
}

/// Streams frames into an ffmpeg encode (see [`VideoCodec`]). Dropped
/// before [`VideoWriter::finish`] (a failed or cancelled render), the
/// encode is stopped and its file removed.
pub struct VideoWriter {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    width: usize,
    height: usize,
    codec: VideoCodec,
    buf: Vec<u8>,
}

impl VideoWriter {
    /// With `audio`, its sound is muxed in (see [`AudioTrack`]).
    pub fn new(path: &Path, width: usize, height: usize, fps: f64, codec: VideoCodec, audio: Option<&AudioTrack>) -> Result<Self> {
        let (min_w, min_h) = codec.min_size();
        if width < min_w || height < min_h {
            bail!("{} needs frames at least {min_w}x{min_h} (these are {width}x{height})", codec.name());
        }
        let encoder = codec.encoder()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let input = if codec.deep() { "rgba64le" } else { "rgba" };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let mut cmd = Command::new(tool("ffmpeg"));
        cmd.args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", input])
            .args(["-s", &format!("{width}x{height}"), "-r", &format!("{fps}"), "-i", "-"]);
        if let Some(a) = audio {
            audio_encoder(path)?;
            cmd.args(["-ss", &format!("{}", a.from_secs), "-t", &format!("{}", a.secs), "-i"]).arg(&a.path);
            cmd.args(["-map", "0:v", "-map", "1:a:0", "-af", &audio_filters(a)]).args(audio_codec(&ext).1);
        }
        let mut child = cmd
            // 4:2:0 / 4:2:2 chroma needs even dimensions. RGB → YUV with the
            // BT.709 matrix (swscale's default is BT.601, which players
            // decoding HD as BT.709 show shifted: 200,100,50 came out 209,105,45).
            // …and say so, on the frames (ffmpeg takes these from them, not
            // from output options): BT.709 primaries and matrix, sRGB-coded
            // values (Arclight's display encoding), video range.
            .args(["-vf", &format!(
                "pad=ceil(iw/2)*2:ceil(ih/2)*2,scale=out_color_matrix=bt709:out_range=tv,format={},\
                 setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv",
                codec.pix_fmt()
            )])
            .args(codec.args(encoder))
            .arg(path)
            // (SVT-AV1 logs its banner at info level otherwise.)
            .env("SVT_LOG", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting ffmpeg (is it installed?)")?;
        let stdin = child.stdin.take();
        Ok(VideoWriter {
            child: Some(child),
            stdin,
            path: path.to_path_buf(),
            width,
            height,
            codec,
            buf: vec![0; width * height * if codec.deep() { 8 } else { 4 }],
        })
    }

    pub fn push(&mut self, img: &Image) -> Result<()> {
        let img = img.flat();
        if img.width != self.width || img.height != self.height {
            bail!(
                "frame size changed mid-render ({}x{} -> {}x{})",
                self.width, self.height, img.width, img.height
            );
        }
        // Without alpha in the file, premultiplied color is already "over
        // black"; ProRes 4444 and VP9 + alpha keep alpha, so they get
        // straight color.
        let alpha = self.codec.alpha();
        let encode = |px: &[f32; 4]| -> [f32; 4] {
            let px = if alpha { color::unpremultiply(*px) } else { *px };
            [color::linear_to_srgb(px[0]), color::linear_to_srgb(px[1]), color::linear_to_srgb(px[2]), if alpha { px[3] } else { 1.0 }]
        };
        if self.codec.deep() {
            for (px, out) in img.pixels.iter().zip(self.buf.as_chunks_mut::<8>().0) {
                for (c, v) in encode(px).into_iter().enumerate() {
                    out[c * 2..c * 2 + 2].copy_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes());
                }
            }
        } else {
            for (px, out) in img.pixels.iter().zip(self.buf.as_chunks_mut::<4>().0) {
                for (c, v) in encode(px).into_iter().enumerate() {
                    out[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        let wrote = self.stdin.as_mut().unwrap().write_all(&self.buf);
        // ffmpeg stopped (say it can't write this container): its reason,
        // not just the broken pipe.
        wrote.or_else(|e| self.end().and_then(|_| Err(anyhow!("ffmpeg stopped taking frames: {e}")))).context("writing frame to ffmpeg")
    }

    pub fn finish(mut self) -> Result<()> {
        self.end()
    }

    /// Close ffmpeg's input and wait for it; if it failed, remove what it
    /// wrote and say why.
    fn end(&mut self) -> Result<()> {
        drop(self.stdin.take());
        let Some(child) = self.child.take() else { return Ok(()) };
        let out = child.wait_with_output().context("waiting for ffmpeg")?;
        if !out.status.success() {
            std::fs::remove_file(&self.path).ok();
            bail!("ffmpeg encode failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }
}

impl Drop for VideoWriter {
    /// Not finished: stop the encode and remove its file.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            drop(self.stdin.take());
            child.kill().ok();
            child.wait().ok();
            std::fs::remove_file(&self.path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AV1 prefers SVT and falls back to libaom; an ffmpeg with neither
    /// (a Homebrew build, say) gets an error naming them.
    #[test]
    fn a_missing_encoder_is_named() {
        let have = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(VideoCodec::Av1.encoder_in(&have(&["libaom-av1", "libsvtav1"])).unwrap(), "libsvtav1");
        assert_eq!(VideoCodec::Av1.encoder_in(&have(&["libaom-av1"])).unwrap(), "libaom-av1");
        assert!(VideoCodec::Av1.args("libaom-av1").contains(&"libaom-av1"));
        let err = VideoCodec::Av1.encoder_in(&have(&["libx264"])).unwrap_err().to_string();
        assert!(err.starts_with("AV1 10-bit needs ffmpeg's libsvtav1 or libaom-av1 encoder, which this ffmpeg"), "{err}");
        let err = VideoCodec::Dnxhr444.encoder_in(&[]).unwrap_err().to_string();
        assert!(err.starts_with("DNxHR 444 10-bit needs ffmpeg's dnxhd encoder"), "{err}");
        // Sound: Opus for WebM, AAC for MP4, PCM for QuickTime.
        let err = audio_encoder_in(Path::new("a.webm"), &have(&["aac", "pcm_s24le"])).unwrap_err().to_string();
        assert!(err.starts_with("sound in a .webm file needs ffmpeg's libopus encoder"), "{err}");
        assert_eq!(audio_encoder_in(Path::new("a.MP4"), &have(&["aac"])).unwrap(), "aac");
        assert_eq!(audio_encoder_in(Path::new("a.mov"), &have(&["pcm_s24le"])).unwrap(), "pcm_s24le");
        let track = |tempo: f64, delay_secs: f64| AudioTrack { path: "a.mov".into(), from_secs: 0.0, secs: 1.0, tempo, delay_secs, total_secs: 1.5 };
        let end = "asetpts=N/SR/TB,apad=whole_dur=1.5,atrim=end=1.5";
        assert_eq!(audio_filters(&track(1.0, 0.0)), end);
        assert_eq!(audio_filters(&track(2.0, 0.5)), format!("atempo=2,adelay=500:all=1,{end}"));
        assert_eq!(audio_filters(&track(5.0, 0.0)), format!("atempo=2,atempo=2,atempo=1.25,{end}"));
        assert_eq!(audio_filters(&track(0.2, 0.0)), format!("atempo=0.5,atempo=0.5,atempo=0.8,{end}"));
    }

    /// The formats offered are exactly the ones written, and each
    /// sequence format writes a frame that reads back.
    #[test]
    fn output_formats_are_what_media_out_writes() {
        let exts: Vec<&str> = OUTPUT_FORMATS.iter().map(|&(_, e)| e).collect();
        assert_eq!(exts, [SEQUENCE_WRITE_EXTS, VIDEO_WRITE_EXTS].concat());
        let dir = std::env::temp_dir().join(format!("arclight_formats_{}", std::process::id()));
        let mut img = Image::new(4, 2);
        img.pixels.fill([0.2, 0.4, 0.6, 1.0]);
        for ext in SEQUENCE_WRITE_EXTS {
            let path = dir.join(format!("f.0001.{ext}"));
            img.write(&path).unwrap_or_else(|e| panic!("{ext}: {e:#}"));
            assert_eq!(Image::read(&path).unwrap_or_else(|e| panic!("{ext}: {e:#}")).width, 4, "{ext}");
        }
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(output_format("/a/shot.####.EXR"), Some("exr"));
        assert_eq!(output_format("shot.tiff"), Some("tif"));
        assert_eq!(output_format("shot.jpeg"), Some("jpg"));
        assert_eq!((output_format("shot.mpg"), output_format(""), output_format("dir.v2/shot")), (None, None, None));
    }

    #[test]
    fn a_new_format_rewrites_the_name_and_its_frame_pattern() {
        for (path, ext, want) in [
            ("/shots/a/shot.mov", "exr", "/shots/a/shot.####.exr"),
            ("/shots/a/shot.####.exr", "mov", "/shots/a/shot.mov"),
            ("shot_%04d.png", "mp4", "shot.mp4"),
            ("shot.%04d.png", "exr", "shot.%04d.exr"),
            ("shot.####.png", "dpx", "shot.####.dpx"),
            ("shot_####.tif", "jpg", "shot_####.jpg"),
            ("shot.mp4", "webm", "shot.webm"),
            ("shot", "exr", "shot.####.exr"),
            ("shot", "mov", "shot.mov"),
            ("", "exr", "render.####.exr"),
            ("", "mov", "render.mov"),
            ("/out/", "png", "/out/render.####.png"),
            ("####.exr", "mov", "render.mov"),
            ("C:\\renders\\v2.1\\shot.mov", "png", "C:\\renders\\v2.1\\shot.####.png"),
            ("dir.v2/take.3.mov", "tif", "dir.v2/take.3.####.tif"),
        ] {
            assert_eq!(with_format(path, ext), want, "{path} -> {ext}");
        }
    }

    #[test]
    fn codec_choices_follow_the_container() {
        assert_eq!(video_codec_choices("webm"), ["auto", "VP9 10-bit", "VP9 + alpha", "AV1 10-bit"]);
        assert_eq!(video_codec_choices("mxf"), ["auto", "DNxHR HQX 10-bit", "DNxHR 444 10-bit"]);
        assert_eq!(video_codec_choices("avi"), ["auto", "H.264 8-bit"]);
        assert_eq!(video_codec_choices("mov"), ["auto", "H.264 8-bit", "HEVC 10-bit", "ProRes 422 HQ 10-bit", "ProRes 4444 + alpha", "DNxHR HQX 10-bit", "DNxHR 444 10-bit"]);
        // Each offered is one VideoCodec::choose takes for it.
        for ext in VIDEO_WRITE_EXTS {
            let path = PathBuf::from(format!("x.{ext}"));
            for c in video_codec_choices(ext) {
                assert!(VideoCodec::choose(c, &path).is_ok(), "{c} in .{ext}");
            }
            let refused = VIDEO_CODECS.iter().filter(|c| !video_codec_choices(ext).contains(c));
            assert!(refused.clone().all(|c| VideoCodec::choose(c, &path).is_err()), ".{ext}");
        }
    }

    #[test]
    fn patterns_substitute() {
        assert_eq!(frame_path("a/shot.####.exr", 7), PathBuf::from("a/shot.0007.exr"));
        assert_eq!(frame_path("shot.%04d.png", 123), PathBuf::from("shot.0123.png"));
        assert_eq!(frame_path("shot.%d.png", 5), PathBuf::from("shot.5.png"));
        assert_eq!(frame_path("still.png", 5), PathBuf::from("still.png"));
        assert!(!is_sequence_pattern("still.png"));
    }

    #[test]
    fn pixel_format_depths() {
        for (name, bits) in [
            ("yuv420p", 8), ("yuvj422p", 8), ("yuv410p", 8), ("nv12", 8), ("nv16", 8), ("rgb24", 8), ("rgba", 8), ("rgb565le", 8),
            ("yuv420p10le", 10), ("yuv422p10be", 10), ("p010le", 10), ("x2rgb10le", 10), ("gbrp10le", 10), ("yuva444p12le", 12),
            ("gray16le", 16), ("rgb48le", 16), ("rgba64be", 16), ("gbrpf32le", 32),
        ] {
            assert_eq!(pix_fmt_bits(name), bits, "{name}");
        }
    }

    /// A 10-bit video keeps its 10 bits: a ramp of 1024 levels comes back
    /// as 1024 distinct values (8 bits would give at most 256). Needs ffmpeg;
    /// skipped without it.
    #[test]
    fn ten_bit_video_decodes_at_full_depth() {
        let dir = std::env::temp_dir().join(format!("arclight_10bit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("ramp.mkv");
        // gbrp10le FFV1: lossless, so the codes are exact.
        let made = Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "nullsrc=s=1024x2:d=0.1,format=gbrp10le,geq=r='X':g='X':b='X'"])
            .args(["-frames:v", "1", "-c:v", "ffv1", "-pix_fmt", "gbrp10le"])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipping: ffmpeg couldn't make a 10-bit clip");
            return;
        }
        let info = probe_video(&clip).unwrap();
        assert_eq!(info.bits, 10);
        let img = read_video_frame(&clip, 0).unwrap();
        let mut levels: Vec<u32> = (0..1024).map(|x| img.at(x, 0)[0].to_bits()).collect();
        levels.dedup();
        assert!(levels.len() > 1000, "only {} distinct levels", levels.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Playback skipping a few frames reads on to the wanted one (the same
    /// ffmpeg, no seek), and gets the frame a seek would.
    #[test]
    fn short_jumps_ahead_read_on_without_seeking() {
        let dir = std::env::temp_dir().join(format!("arclight_jump_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("jump.mp4");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/media/testsrc.mp4"), &clip).unwrap();
        let pid = || DECODERS.lock().unwrap()[&clip].child.as_ref().map(|s| s.child.id());
        read_video_frame(&clip, 0).unwrap();
        let first = pid();
        read_video_frame(&clip, 1).unwrap();
        let read_on = read_video_frame(&clip, 7).unwrap();
        assert_eq!(pid(), first, "read on, not restarted");
        read_video_frame(&clip, 7 + READ_AHEAD + 10).unwrap();
        assert_ne!(pid(), first, "a long jump seeks");
        let restarted = pid();
        let sought = read_video_frame(&clip, 7).unwrap();
        assert_ne!(pid(), restarted, "back: seeks");
        assert!(read_on.pixels == sought.pixels, "the same frame either way");
        assert!(read_on.pixels != read_video_frame(&clip, 6).unwrap().pixels);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sequence_detection() {
        let dir = std::env::temp_dir().join(format!("arclight_seq_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [3, 4, 9] {
            std::fs::write(dir.join(format!("plate.{f:04}.png")), b"").unwrap();
        }
        std::fs::write(dir.join("plate.notes.png"), b"").unwrap();
        let (pat, a, b) = sequence_from_file(&dir.join("plate.0004.png")).unwrap();
        assert!(pat.ends_with("plate.####.png"), "{pat}");
        assert_eq!((a, b), (3, 9));
        // Frames 5–8 are missing: they hold 4; before the first, 3 is used.
        let at = |f| existing_frame_path(&pat, f, 2, 9).file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!([at(4), at(5), at(8), at(9), at(2)], ["plate.0004.png", "plate.0004.png", "plate.0004.png", "plate.0009.png", "plate.0003.png"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
