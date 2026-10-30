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
                None => writer.insert(VideoWriter::new(path, img.format.width, img.format.height, p.fps, codec.unwrap())?),
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
        assert!(format!("{err:#}").contains("ProRes needs a .mov"), "{err:#}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
