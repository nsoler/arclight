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
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mxf", "mpg", "mpeg"];

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
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height,r_frame_rate,avg_frame_rate,nb_frames,duration:format=duration")
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
    let info = VideoInfo { width, height, fps, frames };
    PROBES.lock().unwrap().insert(path.to_path_buf(), info);
    Ok(info)
}

/// A running ffmpeg decode, kept alive so sequential frames stream without
/// re-seeking. Any non-sequential request restarts it at the new position.
struct Decoder {
    info: VideoInfo,
    child: Option<(Child, ChildStdout)>,
    next: usize,
    last: Option<(usize, Arc<Image>)>,
}

impl Decoder {
    fn stop(&mut self) {
        if let Some((mut child, _)) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn start(&mut self, path: &Path, index: usize) -> Result<()> {
        self.stop();
        // Seek a quarter frame early so rounding never skips the wanted frame.
        let t = ((index as f64 - 0.25) / self.info.fps).max(0.0);
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-ss", &format!("{t:.6}"), "-i"])
            .arg(path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting ffmpeg (is it installed?)")?;
        let stdout = child.stdout.take().expect("piped stdout");
        self.child = Some((child, stdout));
        self.next = index;
        Ok(())
    }

    fn read(&mut self, path: &Path, index: usize) -> Result<Arc<Image>> {
        if let Some((i, img)) = &self.last {
            if *i == index {
                return Ok(img.clone());
            }
        }
        if self.child.is_none() || self.next != index {
            self.start(path, index)?;
        }
        let (w, h) = (self.info.width, self.info.height);
        let mut buf = vec![0u8; w * h * 4];
        let (_, stdout) = self.child.as_mut().unwrap();
        if let Err(e) = stdout.read_exact(&mut buf) {
            self.stop();
            // Frame counts from container metadata can overshoot by a frame or
            // two; hold the last decoded frame rather than failing.
            if let Some((_, img)) = &self.last {
                return Ok(img.clone());
            }
            return Err(e).with_context(|| format!("decoding frame {index} of {}", path.display()));
        }
        self.next = index + 1;
        let img = Arc::new(rgba8_to_image(&buf, w, h));
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

/// Streams frames into an ffmpeg encode. `.mov` gets ProRes 422 HQ; every
/// other container gets H.264.
pub struct VideoWriter {
    child: Child,
    stdin: Option<ChildStdin>,
    width: usize,
    height: usize,
    buf: Vec<u8>,
}

impl VideoWriter {
    pub fn new(path: &Path, width: usize, height: usize, fps: f64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let is_mov = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mov"));
        let codec: &[&str] = if is_mov {
            &["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"]
        } else {
            &["-c:v", "libx264", "-crf", "16", "-preset", "medium", "-pix_fmt", "yuv420p"]
        };
        let mut child = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}"), "-r", &format!("{fps}"), "-i", "-"])
            // 4:2:0 / 4:2:2 chroma needs even dimensions.
            .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"])
            .args(codec)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting ffmpeg (is it installed?)")?;
        let stdin = child.stdin.take();
        Ok(VideoWriter {
            child,
            stdin,
            width,
            height,
            buf: vec![0; width * height * 4],
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
        // Video has no alpha: premultiplied color is already "over black".
        for (px, out) in img.pixels.iter().zip(self.buf.chunks_exact_mut(4)) {
            for c in 0..3 {
                out[c] = (color::linear_to_srgb(px[c]) * 255.0).round() as u8;
            }
            out[3] = 255;
        }
        self.stdin
            .as_mut()
            .unwrap()
            .write_all(&self.buf)
            .context("writing frame to ffmpeg")
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.stdin.take());
        let out = self.child.wait_with_output().context("waiting for ffmpeg")?;
        if !out.status.success() {
            bail!("ffmpeg encode failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_substitute() {
        assert_eq!(frame_path("a/shot.####.exr", 7), PathBuf::from("a/shot.0007.exr"));
        assert_eq!(frame_path("shot.%04d.png", 123), PathBuf::from("shot.0123.png"));
        assert_eq!(frame_path("shot.%d.png", 5), PathBuf::from("shot.5.png"));
        assert_eq!(frame_path("still.png", 5), PathBuf::from("still.png"));
        assert!(!is_sequence_pattern("still.png"));
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
