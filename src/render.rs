//! Frame-range rendering for `media_out` nodes, shared by the CLI and GUI.

use crate::graph::{Evaluator, Graph};
use crate::media::{self, VideoWriter};
use crate::ops::{self, EvalContext};
use anyhow::{bail, Result};
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
}

/// Render the frame range of `media_out` node `out_id` to its file(s).
/// Returns the number of frames written.
pub fn render_media_out(graph: &Graph, out_id: &str, progress: &Progress) -> Result<i64> {
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
    if !video && last > first && !media::is_sequence_pattern(&p.path) {
        bail!("{out_id}: to write several frames, put #### or %04d in the file name");
    }

    progress.total.store(last - first + 1, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut eval = Evaluator::new();
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
                None => writer.insert(VideoWriter::new(path, img.width, img.height, p.fps)?),
            };
            w.push(&img)?;
        } else {
            img.write(&media::frame_path(&p.path, frame))?;
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(w) = writer {
        w.finish()?;
    }
    Ok(last - first + 1)
}
