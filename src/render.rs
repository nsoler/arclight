//! Frame-range rendering for `media_out` nodes, shared by the CLI and GUI.

use crate::graph::{Evaluator, Graph};
use crate::media::{self, VideoWriter};
use crate::ops::{self, EvalContext};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// Shared render progress, readable from another thread while rendering.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicI64,
    pub total: AtomicI64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed).max(1);
        self.done.load(Ordering::Relaxed) as f32 / total as f32
    }
}

#[derive(Deserialize)]
struct MediaOut {
    path: String,
    first_frame: i64,
    last_frame: i64,
    fps: f64,
    codec: String,
    dpx: String,
    dpx_alpha: bool,
    audio: String,
}

/// Render the frame range of `media_out` node `out_id` to its file(s).
/// Returns the number of frames written.
pub fn render_media_out(graph: &Graph, out_id: &str, progress: &Progress) -> Result<i64> {
    render_media_out_with(graph, out_id, progress, Evaluator::new())
}

/// [`render_media_out`] with `eval` (say one going on from the viewer's
/// particle simulations, see [`Evaluator::borrowing_particles`]).
pub fn render_media_out_with(graph: &Graph, out_id: &str, progress: &Progress, mut eval: Evaluator) -> Result<i64> {
    let Some(node) = graph.node(out_id) else {
        bail!("no node named '{out_id}'");
    };
    if node.op != "media_out" {
        bail!("'{out_id}' is a '{}' node, not media_out", node.op);
    }
    let p: MediaOut = ops::parse(&node.params, "media_out")?;
    if p.path.is_empty() {
        bail!("{out_id}: no output file set");
    }
    let (first, last) = (p.first_frame, p.last_frame.max(p.first_frame));
    let path = Path::new(&p.path);
    let video = media::is_video(path);
    // Checked before rendering anything: a codec the container can't hold
    // fails here, not after the last frame.
    let codec = if video { Some(media::VideoCodec::choose(&p.codec, path).with_context(|| out_id.to_string())?) } else { None };
    // (So is an encoder the installed ffmpeg lacks.)
    if let Some(c) = codec {
        c.encoder().with_context(|| out_id.to_string())?;
    }
    let audio = if video { audio_track(graph, out_id, &p.audio, first, last, p.fps)? } else { None };
    if audio.is_some() {
        media::audio_encoder(path).with_context(|| out_id.to_string())?;
    }
    if !video && last > first && !media::is_sequence_pattern(&p.path) {
        bail!("{out_id}: to write several frames, put #### or %04d in the file name");
    }

    progress.total.store(last - first + 1, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut writer: Option<VideoWriter> = None;
    for frame in first..=last {
        if progress.cancel.load(Ordering::Relaxed) {
            bail!("render cancelled at frame {frame}");
        }
        let ctx = EvalContext { frame, write_outputs: false };
        let img = eval.evaluate(graph, out_id, &ctx)?;
        if video {
            let w = match writer.as_mut() {
                Some(w) => w,
                None => writer.insert(VideoWriter::new(path, img.format.width, img.format.height, p.fps, codec.unwrap(), audio.as_ref())?),
            };
            w.push(&img)?;
        } else {
            img.write_as(&media::frame_path(&p.path, frame), crate::dpx::Options { encoding: crate::dpx::Encoding::from_choice(&p.dpx), alpha: p.dpx_alpha })?;
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(w) = writer {
        w.finish()?;
    }
    Ok(last - first + 1)
}

/// The sound Media Out `out_id` muxes into frames `first..=last` at
/// `fps`: `choice` "auto" plays what its input plays, "none" nothing, else
/// that node's (see [`crate::audio::source_at`]). From the first frame
/// with sound to the last, mapped to source time so it stays in sync;
/// None if no frame has any.
fn audio_track(graph: &Graph, out_id: &str, choice: &str, first: i64, last: i64, fps: f64) -> Result<Option<media::AudioTrack>> {
    let node = match choice.trim() {
        "none" => return Ok(None),
        "auto" | "" => match graph.node(out_id).and_then(|n| n.inputs.first()).filter(|i| !i.is_empty()) {
            Some(input) => input.clone(),
            None => return Ok(None),
        },
        other if graph.node(other).is_some() => other.to_string(),
        other => bail!("{out_id}: no node named '{other}' to take the sound from"),
    };
    let at = |f: i64| crate::audio::source_at(graph, &node, f);
    let Some(f0) = (first..=last).find(|&f| at(f).is_some()) else { return Ok(None) };
    let f1 = (f0..=last).rev().find(|&f| at(f).is_some()).unwrap_or(f0);
    let src = at(f0).unwrap();
    Ok(Some(media::AudioTrack {
        secs: (f1 - f0 + 1) as f64 * src.secs_per_frame,
        tempo: src.secs_per_frame * fps,
        delay_secs: (f0 - first) as f64 / fps,
        total_secs: (last - first + 1) as f64 / fps,
        from_secs: src.secs,
        path: src.path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Image;
    use std::process::Command;

    /// Each codec through Media Out and back: the 10-bit ones keep far
    /// more than 8 bits of a smooth ramp, ProRes 4444 keeps alpha, and
    /// ProRes refuses an .mp4. Needs ffmpeg (with libx265); skipped without.
    #[test]
    fn video_codecs_keep_their_depth() {
        if Command::new(media::tool("ffmpeg")).arg("-version").output().is_err() {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("arclight_codecs_{}", std::process::id()));
        // A 1024-step ramp, half transparent, as a 16-bit TIFF.
        let mut ramp = Image::new(1024, 16);
        for (i, p) in ramp.pixels.iter_mut().enumerate() {
            let v = crate::color::srgb_to_linear((i % 1024) as f32 / 1023.0);
            *p = [v * 0.5, v * 0.5, v * 0.5, 0.5];
        }
        ramp.write(&dir.join("ramp.tif")).unwrap();
        let render = |file: &str, codec: &str| {
            let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
                { "id": "in", "op": "media_in", "params": { "path": dir.join("ramp.tif").to_string_lossy() } },
                { "id": "out", "op": "media_out", "inputs": ["in"], "params": {
                    "path": dir.join(file).to_string_lossy(), "first_frame": 1, "last_frame": 2, "codec": codec } },
            ]}))
            .unwrap();
            render_media_out(&graph, "out", &Progress::default())
        };
        for (file, codec, bits, min_levels) in [
            ("h264.mp4", "auto", 8, 0),
            ("hevc.mp4", "HEVC 10-bit", 10, 600),
            ("hq.mov", "auto", 10, 600),
            ("alpha.mov", "ProRes 4444 + alpha", 12, 600),
        ] {
            render(file, codec).unwrap();
            let path = dir.join(file);
            // Tagged as what it is: BT.709 matrix and primaries, sRGB-coded,
            // video range.
            let tags = Command::new(media::tool("ffprobe"))
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
                .args(["stream=color_space,color_transfer,color_primaries,color_range", "-of", "csv=p=0"])
                .arg(&path)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&tags.stdout).trim(), "tv,bt709,iec61966-2-1,bt709", "{file}");
            assert_eq!(media::probe_video(&path).unwrap().bits, bits, "{file}");
            let img = media::read_video_frame(&path, 0).unwrap();
            let mut levels: Vec<u32> = (0..1024).map(|x| img.at(x, 8)[0].to_bits()).collect();
            levels.dedup();
            if codec == "auto" && bits == 8 {
                assert!(levels.len() <= 256, "{file}: {}", levels.len());
            } else {
                assert!(levels.len() >= min_levels, "{file}: only {} levels", levels.len());
            }
            let a = img.at(512, 8)[3];
            if codec == "ProRes 4444 + alpha" {
                assert!((a - 0.5).abs() < 0.01, "{file}: alpha {a}");
            } else {
                assert_eq!(a, 1.0, "{file}");
            }
        }
        let err = render("bad.mp4", "ProRes 422 HQ 10-bit").unwrap_err();
        assert!(format!("{err:#}").contains("ProRes 422 HQ 10-bit can't go in a .mp4 file: use .mov or .mkv"), "{err:#}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Media Out of the clip `clip` (a Media In from frame 101, or as
    /// `media_in` says) over frames `first..=last` to `file`.
    fn audio_out(dir: &std::path::Path, clip: &std::path::Path, file: &str, range: (i64, i64), fps: f64, audio: &str, media_in: serde_json::Value) -> Result<i64> {
        let mut clip_params = serde_json::json!({ "path": clip.to_string_lossy(), "first_frame": 101, "last_frame": 196 });
        for (k, v) in media_in.as_object().unwrap() {
            clip_params[k] = v.clone();
        }
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "clip", "op": "media_in", "params": clip_params },
            { "id": "graded", "op": "grade", "inputs": ["clip"], "params": {} },
            { "id": "bg", "op": "constant", "params": { "width": 64, "height": 64 } },
            { "id": "out", "op": "media_out", "inputs": ["graded"], "params": {
                "path": dir.join(file).to_string_lossy(), "first_frame": range.0, "last_frame": range.1, "fps": fps, "audio": audio } },
        ]}))
        .unwrap();
        render_media_out(&graph, "out", &Progress::default())
    }

    /// Media Out carries the sound of the clip upstream for exactly the
    /// frames it renders: frames 137–160 of a Media In from 101 are 1.5 s
    /// to 2.5 s of the clip, so its click (2 s in) is half a second in;
    /// at twice the clip's rate the half second is a quarter. Each
    /// container gets its codec; 'none', or a clip without sound, gives
    /// none; frames before the clip starts are silent.
    #[test]
    fn media_out_carries_the_sound_of_its_frames() {
        use crate::audio::{self, tests as clips};
        let dir = std::env::temp_dir().join(format!("arclight_audio_out_{}", std::process::id()));
        let Some(clip) = clips::clip(&dir, "tone.mov", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let streams = |file: &str| {
            let out = Command::new(media::tool("ffprobe"))
                .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name", "-of", "csv=p=0"])
                .arg(dir.join(file))
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect::<Vec<_>>()
        };
        // Lossless: the click to within a sample or two.
        audio_out(&dir, &clip, "pcm.mov", (137, 160), 24.0, "auto", serde_json::json!({})).unwrap();
        let info = audio::probe(&dir.join("pcm.mov")).unwrap();
        assert!((info.duration_secs - 1.0).abs() < 0.03, "{info:?}");
        let s = audio::decode(&dir.join("pcm.mov"), 0.0, 1.0, 48000, 1).unwrap();
        let at = clips::loudest(&s, 1) as f64 / 48000.0;
        assert!((at - 0.5).abs() < 0.002, "click at {at}");
        assert_eq!(streams("pcm.mov"), ["prores,video", "pcm_s24le,audio"]);
        let rms = |a: &[f32]| (a.iter().map(|v| v * v).sum::<f32>() / a.len() as f32).sqrt();
        // At twice the clip's rate (sped up, so a click is smeared): 24
        // frames are half a second; frames 91–100, before the clip
        // starts, are silent.
        audio_out(&dir, &clip, "fast.mov", (91, 114), 48.0, "auto", serde_json::json!({})).unwrap();
        let info = audio::probe(&dir.join("fast.mov")).unwrap();
        assert!((info.duration_secs - 0.5).abs() < 0.03, "{info:?}");
        let s = audio::decode(&dir.join("fast.mov"), 0.0, 0.5, 48000, 1).unwrap();
        let lead = 10 * 1000;
        assert!(rms(&s[..lead - 200]) < 1e-4 && (rms(&s[lead + 400..]) - 0.25).abs() < 0.01, "{} {}", rms(&s[..lead - 200]), rms(&s[lead + 400..]));
        // Lossy codecs: the click within a few milliseconds.
        for (file, codec) in [("aac.mp4", "aac"), ("aac.m4v", "aac"), ("flac.mkv", "flac"), ("opus.webm", "opus"), ("pcm.mxf", "pcm_s24le"), ("pcm.avi", "pcm_s24le")] {
            if media::audio_encoder(std::path::Path::new(file)).is_err() || media::VideoCodec::choose("auto", std::path::Path::new(file)).unwrap().encoder().is_err() {
                eprintln!("skipping {file}: an encoder is missing");
                continue;
            }
            // (DNxHR, MXF's video, needs 256x120 frames.)
            let fit = if file.ends_with(".mxf") { serde_json::json!({ "fit": "stretch" }) } else { serde_json::json!({}) };
            audio_out(&dir, &clip, file, (137, 160), 24.0, "auto", fit).unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert_eq!(streams(file).last().map(String::as_str), Some(format!("{codec},audio").as_str()), "{file}");
            let info = audio::probe(&dir.join(file)).unwrap();
            assert!((info.duration_secs - 1.0).abs() < 0.06, "{file}: {info:?}");
            let s = audio::decode(&dir.join(file), 0.0, 1.0, 48000, 1).unwrap();
            let at = clips::loudest(&s, 1) as f64 / 48000.0;
            assert!((at - 0.5).abs() < 0.01, "{file}: click at {at}");
        }
        // Frames 91–120 with the clip starting at 101: ten frames of silence first.
        audio_out(&dir, &clip, "late.mov", (91, 120), 24.0, "clip", serde_json::json!({})).unwrap();
        let s = audio::decode(&dir.join("late.mov"), 0.0, 1.25, 48000, 1).unwrap();
        let lead = 10 * 2000;
        assert!(rms(&s[..lead - 200]) < 1e-4 && (rms(&s[lead + 200..]) - 0.25).abs() < 0.01, "{} {}", rms(&s[..lead - 200]), rms(&s[lead + 200..]));
        // None asked for, or none to take.
        audio_out(&dir, &clip, "none.mov", (137, 160), 24.0, "none", serde_json::json!({})).unwrap();
        assert_eq!(streams("none.mov"), ["prores,video"]);
        let silent = clips::clip(&dir, "silent.mp4", true).unwrap();
        audio_out(&dir, &silent, "mute.mp4", (137, 160), 24.0, "auto", serde_json::json!({})).unwrap();
        assert_eq!(streams("mute.mp4"), ["h264,video"]);
        audio_out(&dir, &clip, "bg.mp4", (137, 160), 24.0, "bg", serde_json::json!({})).unwrap();
        assert_eq!(streams("bg.mp4"), ["h264,video"], "a node with no sound upstream");
        let err = audio_out(&dir, &clip, "nope.mp4", (137, 160), 24.0, "nope", serde_json::json!({})).unwrap_err();
        assert!(format!("{err:#}").contains("no node named 'nope' to take the sound from"), "{err:#}");
        assert!(!dir.join("nope.mp4").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What ffprobe reads of a video's first stream: codec, pixel format
    /// and, for VP9 with alpha, its alpha_mode tag.
    fn probe(path: &std::path::Path) -> String {
        let out = Command::new(media::tool("ffprobe"))
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,pix_fmt:stream_tags=alpha_mode", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn checker_out(dir: &std::path::Path, file: &str, codec: &str, size: [i64; 2], fps: f64) -> Graph {
        serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "c", "op": "checkerboard", "params": { "width": size[0], "height": size[1] } },
            { "id": "out", "op": "media_out", "inputs": ["c"], "params": {
                "path": dir.join(file).to_string_lossy(), "first_frame": 1, "last_frame": 2, "fps": fps, "codec": codec } },
        ]}))
        .unwrap()
    }

    /// Every codec in every container it goes in writes a video ffprobe
    /// reads back as that codec and pixel format (each skipped if ffmpeg
    /// lacks its encoder); every other container, and one Media Out can't
    /// write, is refused before anything is written; 'auto' picks by
    /// container (VP9 for .webm, which once wrote a broken file).
    #[test]
    fn every_codec_goes_in_its_containers_and_no_other() {
        if Command::new(media::tool("ffmpeg")).arg("-version").output().is_err() {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("arclight_containers_{}", std::process::id()));
        let render = |file: &str, codec: &str| render_media_out(&checker_out(&dir, file, codec, [320, 128], 24.0), "out", &Progress::default());
        for (codec, probed) in [
            ("H.264 8-bit", "h264,yuv420p"),
            ("HEVC 10-bit", "hevc,yuv420p10le"),
            ("ProRes 422 HQ 10-bit", "prores,yuv422p10le"),
            ("ProRes 4444 + alpha", "prores,yuva444p12le"),
            ("DNxHR HQX 10-bit", "dnxhd,yuv422p10le"),
            ("DNxHR 444 10-bit", "dnxhd,yuv444p10le"),
            ("VP9 10-bit", "vp9,yuv420p10le"),
            ("VP9 + alpha", "vp9,yuv420p,1"),
            ("AV1 10-bit", "av1,yuv420p10le"),
        ] {
            let c = media::VideoCodec::choose(codec, std::path::Path::new("x.mkv")).unwrap();
            if let Err(e) = c.encoder() {
                eprintln!("skipping {codec}: {e:#}");
                continue;
            }
            for ext in media::VIDEO_WRITE_EXTS {
                let file = format!("{}.{ext}", codec.replace([' ', '+', '.'], "_"));
                if c.containers().contains(ext) {
                    render(&file, codec).unwrap_or_else(|e| panic!("{file}: {e:#}"));
                    assert_eq!(probe(&dir.join(&file)), probed, "{file}");
                } else {
                    let err = format!("{:#}", render(&file, codec).unwrap_err());
                    assert!(err.contains(&format!("{codec} can't go in a .{ext} file")), "{file}: {err}");
                    assert!(!dir.join(&file).exists(), "{file}: nothing written");
                }
            }
        }
        for (file, probed) in [("auto.webm", "vp9,yuv420p10le"), ("auto.mxf", "dnxhd,yuv422p10le"), ("auto.mov", "prores,yuv422p10le"), ("auto.mkv", "h264,yuv420p"), ("auto.avi", "h264,yuv420p")] {
            let codec = media::VideoCodec::choose("auto", std::path::Path::new(file)).unwrap();
            if codec.encoder().is_ok() {
                render(file, "auto").unwrap_or_else(|e| panic!("{file}: {e:#}"));
                assert_eq!(probe(&dir.join(file)), probed, "{file}");
            }
        }
        let err = format!("{:#}", render("x.mpg", "auto").unwrap_err());
        assert!(err.contains("can't write a .mpg video: use .mp4, .mov, .mkv, .webm, .mxf, .m4v or .avi"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An encode ffmpeg fails (MXF takes only broadcast frame rates) fails
    /// the render with ffmpeg's reason and leaves no file; so does a frame
    /// too small for DNxHR, before ffmpeg starts.
    #[test]
    fn a_failed_encode_fails_the_render_and_leaves_no_file() {
        if media::VideoCodec::DnxhrHqx.encoder().is_err() {
            eprintln!("skipping: no ffmpeg with dnxhd");
            return;
        }
        let dir = std::env::temp_dir().join(format!("arclight_failed_encode_{}", std::process::id()));
        for size in [[320, 128], [1920, 1080]] {
            let err = render_media_out(&checker_out(&dir, "odd.mxf", "auto", size, 13.0), "out", &Progress::default()).unwrap_err();
            assert!(format!("{err:#}").contains("Unsupported frame rate"), "{size:?}: {err:#}");
            assert!(!dir.join("odd.mxf").exists(), "{size:?}: no broken file left");
        }
        let err = render_media_out(&checker_out(&dir, "small.mov", "DNxHR HQX 10-bit", [64, 64], 24.0), "out", &Progress::default()).unwrap_err();
        assert!(format!("{err:#}").contains("DNxHR HQX 10-bit needs frames at least 256x120 (these are 64x64)"), "{err:#}");
        assert!(!dir.join("small.mov").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
