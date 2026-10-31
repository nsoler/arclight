//! Audio from the clips a graph reads: probed and decoded with the system
//! `ffmpeg` (as video is, see [`crate::media`]), and the source a node
//! plays at a frame (the clip upstream, and where in it).

use crate::graph::Graph;
use crate::media::{self, tool};
use crate::ops;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

/// A file's first audio stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_secs: f64,
    /// Where it starts in the file's timeline (seconds).
    pub start_secs: f64,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `path`'s first audio stream (None without one, or if ffprobe can't
/// read it). Kept until the file changes.
pub fn probe(path: &Path) -> Option<AudioInfo> {
    type Probes = HashMap<PathBuf, (Option<SystemTime>, Option<AudioInfo>)>;
    static PROBES: LazyLock<Mutex<Probes>> = LazyLock::new(Default::default);
    let when = mtime(path);
    if let Some((t, info)) = PROBES.lock().unwrap().get(path) {
        if *t == when {
            return *info;
        }
    }
    let info = probe_uncached(path);
    PROBES.lock().unwrap().insert(path.to_path_buf(), (when, info));
    info
}

fn probe_uncached(path: &Path) -> Option<AudioInfo> {
    let out = Command::new(tool("ffprobe"))
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate,channels,start_time,duration:format=duration", "-of", "json"])
        .arg(path)
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let s = v["streams"].get(0)?;
    let num = |x: &serde_json::Value| x.as_str().and_then(|t| t.parse::<f64>().ok()).or_else(|| x.as_f64());
    let sample_rate = num(&s["sample_rate"])? as u32;
    let channels = s["channels"].as_u64()? as u32;
    let duration_secs = num(&s["duration"]).or_else(|| num(&v["format"]["duration"])).unwrap_or(0.0);
    (sample_rate > 0 && channels > 0).then(|| AudioInfo { sample_rate, channels, duration_secs, start_secs: num(&s["start_time"]).unwrap_or(0.0) })
}

/// `secs` of `path`'s audio from `from_secs` (seconds from the file's
/// start, as ffmpeg's -ss counts), at `rate` with `channels`, interleaved:
/// exactly round(secs × rate) frames, resampled and mixed by ffmpeg (its
/// mix keeps loudness: stereo to mono is each side at −3 dB, so a tone
/// in both sides comes out 1.41 times as high),
/// silent where the file has no sound (before 0, past its end). The last
/// few asked for are kept, for playback asking again.
pub fn decode(path: &Path, from_secs: f64, secs: f64, rate: u32, channels: u32) -> Result<Arc<[f32]>> {
    type Key = (PathBuf, Option<SystemTime>, u64, u64, u32, u32);
    type Kept = VecDeque<(Key, Arc<[f32]>)>;
    static KEPT: LazyLock<Mutex<Kept>> = LazyLock::new(Default::default);
    const KEEP: usize = 16;
    if rate == 0 || channels == 0 {
        bail!("decoding audio needs a sample rate and channels");
    }
    let key = (path.to_path_buf(), mtime(path), from_secs.to_bits(), secs.to_bits(), rate, channels);
    if let Some((_, samples)) = KEPT.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return Ok(samples.clone());
    }
    let frames = (secs.max(0.0) * rate as f64).round() as usize;
    let ch = channels as usize;
    // Before the file starts: silence, then the file from 0.
    let lead = ((-from_secs).max(0.0) * rate as f64).round() as usize;
    let mut out = vec![0.0f32; frames * ch];
    if lead < frames {
        let start = from_secs.max(0.0);
        let want = (frames - lead) as f64 / rate as f64;
        // Seeking the input lands a frame or so off in Ogg / WebM (Opus,
        // Vorbis), even at 0: there it's decoded from the start and cut
        // exactly. Elsewhere (PCM, AAC, FLAC…) the input seek is exact.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let exact_input = !matches!(ext.as_str(), "webm" | "ogg" | "oga" | "opus");
        let seek = ["-ss".to_string(), format!("{start}")];
        let mut cmd = Command::new(tool("ffmpeg"));
        cmd.args(["-v", "error"]);
        if start > 0.0 && exact_input {
            cmd.args(&seek);
        }
        cmd.arg("-i").arg(path);
        if start > 0.0 && !exact_input {
            cmd.args(&seek);
        }
        let run = cmd
            .args(["-t", &format!("{want}"), "-map", "0:a:0", "-ac", &channels.to_string(), "-ar", &rate.to_string(), "-f", "f32le", "-"])
            .output()
            .context("running ffmpeg (is it installed?)")?;
        if !run.status.success() {
            bail!("decoding the audio of {}: {}", path.display(), String::from_utf8_lossy(&run.stderr).trim());
        }
        let got = run.stdout.as_chunks::<4>().0;
        for (o, b) in out[lead * ch..].iter_mut().zip(got) {
            *o = f32::from_le_bytes(*b);
        }
    }
    let samples: Arc<[f32]> = out.into();
    let mut kept = KEPT.lock().unwrap();
    kept.push_back((key, samples.clone()));
    if kept.len() > KEEP {
        kept.pop_front();
    }
    Ok(samples)
}

/// Where a node's sound comes from at a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSource {
    pub path: PathBuf,
    /// The source time that frame shows (seconds from the file's start).
    pub secs: f64,
    /// How far source time moves per frame (1 ÷ the clip's frame rate):
    /// played at another rate, the sound is sped up or slowed to match.
    pub secs_per_frame: f64,
}

/// The audio node `node` plays at `frame`: a Media In's or Read's clip
/// with sound (its frames mapped to source time as its video is), or, for
/// any other node, what its first input plays (a node turned off, what it
/// passes on). None where there's none,
/// or where the clip holds a frame (outside a Media In's range, past a
/// clip's end).
///
/// (No node retimes yet. One that does must map `frame` here before
/// following its input.)
pub fn source_at(graph: &Graph, node: &str, frame: i64) -> Option<AudioSource> {
    let mut id = node;
    for _ in 0..=graph.nodes.len() {
        let n = graph.node(id)?;
        // Turned off: what it passes on plays (nothing, for a source).
        if n.disabled {
            id = n.inputs.get(ops::bypass_port(&n.op)?).filter(|i| !i.is_empty())?;
            continue;
        }
        let first = match n.op.as_str() {
            "media_in" => {
                let p = ops::params_with_defaults("media_in", &n.params);
                let first = p["first_frame"].as_i64().unwrap_or(1);
                let last = p["last_frame"].as_i64().unwrap_or(first).max(first);
                return (first..=last).contains(&frame).then(|| clip_source(p["path"].as_str()?, frame - first)).flatten();
            }
            "read" => return clip_source(n.params["path"].as_str()?, frame - 1),
            _ => n.inputs.first().filter(|i| !i.is_empty())?,
        };
        id = first;
    }
    None
}

/// A video file's sound at frame `index` of its picture (None before the
/// first frame or past the last, or without sound).
fn clip_source(path: &str, index: i64) -> Option<AudioSource> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !media::is_video(path) || index < 0 {
        return None;
    }
    probe(path)?;
    let video = media::probe_video(path).ok()?;
    if index as usize >= video.frames {
        return None;
    }
    let secs_per_frame = 1.0 / video.fps;
    Some(AudioSource { path: path.to_path_buf(), secs: index as f64 * secs_per_frame, secs_per_frame })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub const RATE: u32 = 48000;
    /// The click in [`clip`]'s sound, two seconds in.
    pub const CLICK: usize = 2 * RATE as usize;

    /// The tone in [`clip`]'s sound at sample `n`.
    pub fn tone(n: f64) -> f64 {
        0.25 * (2.0 * std::f64::consts::PI * 1000.0 * n / RATE as f64).sin()
    }

    /// A 4-second, 24 fps test clip in `dir`, with (unless `silent`) a
    /// stereo 1 kHz tone at 48 kHz and a click at [`CLICK`]; None without
    /// ffmpeg.
    pub fn clip(dir: &Path, name: &str, silent: bool) -> Option<PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(name);
        let mut cmd = Command::new(tool("ffmpeg"));
        cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=64x64:rate=24:duration=4"]);
        if !silent {
            cmd.args(["-f", "lavfi", "-i", &format!("aevalsrc=0.25*sin(2*PI*1000*t)+if(eq(n\\,{CLICK})\\,0.7\\,0):s={RATE}:c=stereo:d=4")]);
            cmd.args(["-c:a", "pcm_s24le"]);
        }
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]).arg(&path);
        cmd.status().ok()?.success().then_some(path)
    }

    /// The index of the loudest sample of one channel.
    pub fn loudest(samples: &[f32], channels: usize) -> usize {
        (0..samples.len() / channels).max_by(|&a, &b| samples[a * channels].abs().total_cmp(&samples[b * channels].abs())).unwrap()
    }

    #[test]
    fn a_clip_s_sound_is_probed_and_decoded_to_the_sample() {
        let dir = std::env::temp_dir().join(format!("arclight_audio_{}", std::process::id()));
        let Some(path) = clip(&dir, "tone.mov", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let info = probe(&path).unwrap();
        assert_eq!((info.sample_rate, info.channels, info.start_secs), (RATE, 2, 0.0));
        assert!((info.duration_secs - 4.0).abs() < 0.01, "{info:?}");
        // Half a second either side of the click, as stored.
        let s = decode(&path, 1.5, 1.0, RATE, 2).unwrap();
        assert_eq!(s.len(), RATE as usize * 2);
        assert_eq!(loudest(&s, 2), CLICK - 72000);
        for k in [0usize, 1, 7, 100, 30000] {
            let want = tone((72000 + k) as f64);
            assert!((s[k * 2] as f64 - want).abs() < 1e-4 && s[k * 2] == s[k * 2 + 1], "sample {k}: {} vs {want}", s[k * 2]);
        }
        assert!(Arc::ptr_eq(&s, &decode(&path, 1.5, 1.0, RATE, 2).unwrap()), "kept for asking again");
        // Mixed down and resampled; silent before the start and past the end.
        let mono = decode(&path, 1.5, 1.0, 24000, 1).unwrap();
        assert_eq!(mono.len(), 24000);
        assert!(loudest(&mono, 1).abs_diff(12000) <= 2, "{}", loudest(&mono, 1));
        let edges = decode(&path, -0.5, 1.0, RATE, 1).unwrap();
        assert!(edges[..24000].iter().all(|&v| v == 0.0) && edges[24001..].iter().any(|&v| v.abs() > 0.2));
        let past = decode(&path, 3.75, 0.5, RATE, 1).unwrap();
        assert!(past[..11000].iter().any(|&v| v.abs() > 0.2) && past[13000..].iter().all(|&v| v == 0.0));
        // Opus in WebM, where seeking the input lands off: still in place.
        let webm = dir.join("tone.webm");
        let made = Command::new(tool("ffmpeg")).args(["-y", "-v", "error", "-i"]).arg(&path).args(["-map", "0:a", "-c:a", "libopus"]).arg(&webm).status();
        if made.is_ok_and(|s| s.success()) {
            for from in [0.0, 1.5, 1.9] {
                let s = decode(&webm, from, 0.5, RATE, 1).unwrap();
                let want = CLICK as f64 - from * RATE as f64;
                if want < s.len() as f64 {
                    assert!((loudest(&s, 1) as f64 - want).abs() <= 2.0, "from {from}: {} vs {want}", loudest(&s, 1));
                }
            }
            let s = decode(&webm, 1.5, 1.0, RATE, 1).unwrap();
            assert!((loudest(&s, 1) as f64 - 24000.0).abs() <= 2.0, "{}", loudest(&s, 1));
        }
        // No sound: none.
        let silent = clip(&dir, "silent.mp4", true).unwrap();
        assert_eq!(probe(&silent), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_node_plays_the_sound_of_the_clip_upstream() {
        let dir = std::env::temp_dir().join(format!("arclight_audio_src_{}", std::process::id()));
        let Some(path) = clip(&dir, "tone.mov", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let silent = clip(&dir, "silent.mp4", true).unwrap();
        let graph: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "clip", "op": "media_in", "params": { "path": path, "first_frame": 101, "last_frame": 196 } },
            { "id": "read", "op": "read", "params": { "path": path } },
            { "id": "mute", "op": "read", "params": { "path": silent } },
            { "id": "graded", "op": "grade", "inputs": ["clip"], "params": {} },
            { "id": "moved", "op": "transform", "inputs": ["graded"], "params": {} },
            { "id": "over", "op": "merge", "inputs": ["read", "clip"], "params": {} },
            { "id": "bg", "op": "constant", "params": {} },
        ] }))
        .unwrap();
        let at = |n: &str, f: i64| source_at(&graph, n, f).map(|s| (s.secs * 24.0).round() as i64);
        assert_eq!(source_at(&graph, "clip", 101), Some(AudioSource { path: path.clone(), secs: 0.0, secs_per_frame: 1.0 / 24.0 }));
        assert_eq!((at("clip", 149), at("moved", 149), at("read", 49), at("over", 49)), (Some(48), Some(48), Some(48), Some(48)));
        assert_eq!((at("clip", 100), at("clip", 197), at("read", 0), at("read", 97)), (None, None, None, None), "where the picture holds");
        assert_eq!((at("mute", 10), at("bg", 10), at("nope", 10)), (None, None, None));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A node turned off plays what it passes on (a merge its B); a source
    /// turned off, nothing.
    #[test]
    fn a_disabled_node_plays_what_it_passes_on() {
        let dir = std::env::temp_dir().join(format!("arclight_audio_off_{}", std::process::id()));
        let Some(path) = clip(&dir, "tone.mov", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let mut graph: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "clip", "op": "media_in", "params": { "path": path, "first_frame": 101, "last_frame": 196 } },
            { "id": "read", "op": "read", "params": { "path": path } },
            { "id": "over", "op": "merge", "inputs": ["read", "clip"], "params": {}, "disabled": true },
            { "id": "graded", "op": "grade", "inputs": ["clip"], "params": {}, "disabled": true },
        ] }))
        .unwrap();
        let at = |g: &Graph, n: &str, f: i64| source_at(g, n, f).map(|s| (s.secs * 24.0).round() as i64);
        // At 149 the merge's A (the Read) is at 148, its B (the clip) at 48.
        assert_eq!((at(&graph, "over", 149), at(&graph, "graded", 149)), (Some(48), Some(48)));
        graph.nodes[0].disabled = true;
        assert_eq!((at(&graph, "clip", 149), at(&graph, "over", 149), at(&graph, "graded", 149)), (None, None, None));
        std::fs::remove_dir_all(&dir).ok();
    }
}
