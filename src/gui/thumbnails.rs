//! Node-graph thumbnails for Read and Media In nodes: the clip's first
//! frame (a still: the file; a sequence: its first frame; a video: frame
//! 0), decoded on a background thread, shrunk to [`LONG_SIDE`] pixels on
//! its long side, display-referred (sRGB) as the viewer shows it, and
//! kept as a small texture.
//!
//! [`Thumbnails::get`] is called every frame for each visible node, so it
//! never blocks: a miss queues a decode and returns None until it lands
//! (the worker asks for a repaint). A thumbnail is keyed by the node's op,
//! path, first frame and the file's modification time, so a new path or a
//! rewritten file shows anew; a failed decode is kept too (no retry until
//! the key changes), and nodes that are gone are dropped.

use crate::doc::Doc;
use arclight::image::Image;
use arclight::media;
use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

/// Thumbnail pixels on the long side.
pub const LONG_SIDE: u32 = 160;
/// How often a shown thumbnail's file is looked at again (its mtime), and
/// nodes that are gone are dropped.
const RECHECK: Duration = Duration::from_secs(1);

/// What a thumbnail shows, as its node's params say.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Params {
    op: String,
    path: String,
    /// Media In's first frame (0 for a Read).
    first: i64,
}

/// The file to decode for a thumbnail.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Source {
    /// An image file: a still, or a sequence's first frame.
    Still(PathBuf),
    /// A video's frame 0.
    Video(PathBuf),
}

impl Source {
    fn path(&self) -> &Path {
        match self {
            Source::Still(p) | Source::Video(p) => p,
        }
    }
}

/// A thumbnail's identity: a different one is decoded anew.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    params: Params,
    source: Source,
    /// The file's modification time (None: it can't be read).
    mtime: Option<SystemTime>,
}

enum State {
    Pending,
    Ready(egui::TextureHandle, egui::Vec2),
    Failed,
}

struct Entry {
    key: Key,
    state: State,
    /// When the file's mtime was last read.
    checked: Instant,
}

/// A decoded thumbnail: straight-alpha sRGB RGBA, `size` pixels.
struct Decoded {
    size: [usize; 2],
    rgba: Vec<u8>,
}

struct Job {
    id: String,
    key: Key,
    ctx: egui::Context,
}

struct Done {
    id: String,
    key: Key,
    result: Result<Decoded, String>,
}

/// The node graph's thumbnails (see the module docs).
#[derive(Default)]
pub struct Thumbnails {
    entries: HashMap<String, Entry>,
    worker: Option<(Sender<Job>, Receiver<Done>)>,
    /// When nodes that are gone were last dropped.
    swept: Option<Instant>,
    /// Decodes queued so far (for tests).
    queued: usize,
}

impl Thumbnails {
    /// The thumbnail of node `id` (its texture and size in pixels), or None
    /// while it loads, without a file, for other ops, or when it can't be
    /// read. Never blocks.
    pub fn get(&mut self, ctx: &egui::Context, doc: &Doc, id: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        self.receive(ctx);
        let now = Instant::now();
        if self.swept.is_none_or(|t| now - t >= RECHECK) {
            self.swept = Some(now);
            self.entries.retain(|n, _| doc.node(n).is_some());
        }
        let node = doc.node(id)?;
        let Some(params) = params_of(&node.op, &node.params) else {
            self.entries.remove(id);
            return None;
        };
        let key = match self.entries.get(id) {
            // Same params: only the file's mtime may have changed.
            Some(e) if e.key.params == params => {
                if now - e.checked < RECHECK {
                    e.key.clone()
                } else {
                    Key { mtime: mtime(e.key.source.path()), ..e.key.clone() }
                }
            }
            _ => key_of(params),
        };
        match self.entries.get_mut(id) {
            Some(e) if e.key == key => {
                if now - e.checked >= RECHECK {
                    e.checked = now;
                }
            }
            _ => {
                self.queue(ctx, id, key.clone());
                self.entries.insert(id.to_string(), Entry { key, state: State::Pending, checked: now });
            }
        }
        match &self.entries.get(id)?.state {
            State::Ready(tex, size) => Some((tex.id(), *size)),
            State::Pending | State::Failed => None,
        }
    }

    /// Queue a decode, starting the worker the first time.
    fn queue(&mut self, ctx: &egui::Context, id: &str, key: Key) {
        let (jobs, _) = self.worker.get_or_insert_with(spawn);
        self.queued += 1;
        // A worker that's gone (it can't be: each job's panic is caught)
        // leaves the entry pending.
        let _ = jobs.send(Job { id: id.to_string(), key, ctx: ctx.clone() });
    }

    /// Take the decodes that landed: upload the ones still wanted.
    fn receive(&mut self, ctx: &egui::Context) {
        let Some((_, done)) = &self.worker else { return };
        let landed: Vec<Done> = done.try_iter().collect();
        for d in landed {
            let Some(e) = self.entries.get_mut(&d.id).filter(|e| e.key == d.key) else { continue };
            e.state = match d.result {
                Ok(img) => {
                    let color = egui::ColorImage::from_rgba_unmultiplied(img.size, &img.rgba);
                    let tex = ctx.load_texture(format!("thumbnail {}", d.id), color, egui::TextureOptions::LINEAR);
                    State::Ready(tex, egui::vec2(img.size[0] as f32, img.size[1] as f32))
                }
                Err(_) => State::Failed,
            };
        }
    }

    #[cfg(test)]
    fn failed(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| matches!(e.state, State::Failed))
    }
}

/// The background decoder: one thread, a job at a time; a panic in a
/// decode is that thumbnail's failure, not the thread's end.
fn spawn() -> (Sender<Job>, Receiver<Done>) {
    let (jobs, jobs_rx) = channel::<Job>();
    let (done_tx, done) = channel::<Done>();
    let spawned = std::thread::Builder::new().name("thumbnails".into()).spawn(move || {
        for job in jobs_rx {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&job.key.source)))
                .unwrap_or_else(|_| Err("the decoder panicked".into()));
            if done_tx.send(Done { id: job.id, key: job.key, result }).is_err() {
                return;
            }
            job.ctx.request_repaint();
        }
    });
    if let Err(e) = spawned {
        eprintln!("thumbnails: can't start the decoder thread: {e}");
    }
    (jobs, done)
}

/// The params a thumbnail follows, for a Read or Media In with a file.
fn params_of(op: &str, params: &Value) -> Option<Params> {
    if !matches!(op, "read" | "media_in") {
        return None;
    }
    let path = params.get("path").and_then(Value::as_str).filter(|p| !p.is_empty())?;
    let first = if op == "media_in" { params.get("first_frame").and_then(Value::as_i64).unwrap_or(1) } else { 0 };
    Some(Params { op: op.to_string(), path: path.to_string(), first })
}

/// The file a thumbnail decodes and its mtime, as the op reads its first
/// frame (see ops.rs `read` and `read_media`). Looks at the disk (a Read
/// of a sequence scans its folder), so only when the params change.
fn key_of(params: Params) -> Key {
    let source = source(&params);
    let mtime = mtime(source.path());
    Key { params, source, mtime }
}

fn source(p: &Params) -> Source {
    let path = Path::new(&p.path);
    if media::is_video(path) {
        return Source::Video(path.to_path_buf());
    }
    Source::Still(if p.op == "media_in" {
        media::existing_frame_path(&p.path, p.first, p.first, p.first)
    } else if media::is_sequence_pattern(&p.path) {
        media::frame_path(&p.path, media::detect_sequence_range(&p.path).map_or(1, |(a, _)| a))
    } else {
        path.to_path_buf()
    })
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Decode `source` and shrink it to [`LONG_SIDE`], display-referred.
fn decode(source: &Source) -> Result<Decoded, String> {
    let img: std::sync::Arc<Image> = match source {
        Source::Video(p) => media::read_video_frame(p, 0).map_err(|e| format!("{e:#}"))?,
        Source::Still(p) => std::sync::Arc::new(Image::read(p).map_err(|e| format!("{e:#}"))?),
    };
    Ok(shrink(&img))
}

/// `img`'s frame as sRGB RGBA, at most [`LONG_SIDE`] on its long side.
fn shrink(img: &Image) -> Decoded {
    let rgba = img.flat().to_rgba8();
    let long = rgba.width().max(rgba.height()).max(1);
    let rgba = if long > LONG_SIDE {
        let scale = LONG_SIDE as f64 / long as f64;
        let (w, h) = ((rgba.width() as f64 * scale).round().max(1.0) as u32, (rgba.height() as f64 * scale).round().max(1.0) as u32);
        image::imageops::thumbnail(&rgba, w, h)
    } else {
        rgba
    };
    Decoded { size: [rgba.width() as usize, rgba.height() as usize], rgba: rgba.into_raw() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::Graph;

    /// A test's folder under $TMP, removed when it goes out of scope (a
    /// failing test's too, as the panic unwinds).
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir(name: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("arclight-thumbnails-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }

    fn png(path: &Path, w: u32, h: u32) {
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 100, 50, 255])).save(path).unwrap();
    }

    fn doc_with(op: &str, path: &Path) -> Doc {
        let graph: Graph = serde_json::from_value(serde_json::json!({ "nodes": [
            { "id": "clip", "op": op, "params": { "path": path.to_string_lossy(), "first_frame": 1, "last_frame": 1 } },
            { "id": "blur", "op": "blur" },
        ] }))
        .unwrap();
        Doc::new(graph, None)
    }

    /// get() until the thumbnail lands, within 10 s.
    fn wait(t: &mut Thumbnails, ctx: &egui::Context, doc: &Doc) -> Option<(egui::TextureId, egui::Vec2)> {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if let Some(got) = t.get(ctx, doc, "clip") {
                return Some(got);
            }
            if t.failed("clip") {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the thumbnail didn't land in 10 s");
    }

    #[test]
    fn the_key_follows_the_path_and_the_files_mtime() {
        let d = dir("key");
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        png(&a, 8, 8);
        png(&b, 8, 8);
        let ka = key_of(params_of("read", &serde_json::json!({ "path": a.to_string_lossy() })).unwrap());
        let kb = key_of(params_of("read", &serde_json::json!({ "path": b.to_string_lossy() })).unwrap());
        assert_ne!(ka, kb, "another path");
        assert_eq!(ka, key_of(ka.params.clone()), "the same file");
        let later = SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options().write(true).open(&a).unwrap().set_modified(later).unwrap();
        let rewritten = key_of(ka.params.clone());
        assert_ne!(ka, rewritten, "rewritten");
        assert_eq!(rewritten.mtime, Some(later));
        // Media In's first frame is part of it; other ops and no file have none.
        let m = |first: i64| params_of("media_in", &serde_json::json!({ "path": a.to_string_lossy(), "first_frame": first })).unwrap();
        assert_ne!(m(1), m(5));
        assert!(params_of("blur", &serde_json::json!({ "path": "x.png" })).is_none());
        assert!(params_of("read", &serde_json::json!({ "path": "" })).is_none());
    }

    #[test]
    fn a_sequence_shows_its_first_frame() {
        let d = dir("seq");
        for f in [3, 4] {
            png(&d.join(format!("plate.{f:04}.png")), 4, 4);
        }
        let pattern = d.join("plate.####.png").to_string_lossy().to_string();
        let k = key_of(params_of("read", &serde_json::json!({ "path": pattern })).unwrap());
        assert_eq!(k.source, Source::Still(d.join("plate.0003.png")));
        let k = key_of(params_of("media_in", &serde_json::json!({ "path": pattern, "first_frame": 4 })).unwrap());
        assert_eq!(k.source, Source::Still(d.join("plate.0004.png")));
    }

    #[test]
    fn a_missing_file_gives_none_and_isnt_retried_every_frame() {
        let d = dir("missing");
        let doc = doc_with("read", &d.join("nope.png"));
        let ctx = egui::Context::default();
        let mut t = Thumbnails::default();
        assert_eq!(wait(&mut t, &ctx, &doc), None);
        assert!(t.failed("clip"));
        for _ in 0..50 {
            assert_eq!(t.get(&ctx, &doc, "clip"), None);
        }
        assert_eq!(t.queued, 1, "one decode, no retry loop");
        // Not a Read / Media In: nothing.
        assert_eq!(t.get(&ctx, &doc, "blur"), None);
        assert_eq!(t.queued, 1);
    }

    #[test]
    fn a_png_thumbnail_keeps_its_aspect() {
        let d = dir("png");
        let file = d.join("wide.png");
        png(&file, 320, 80);
        let shrunk = decode(&Source::Still(file.clone())).unwrap();
        assert_eq!(shrunk.size, [160, 40]);
        assert_eq!(shrunk.rgba.len(), 160 * 40 * 4);
        let doc = doc_with("read", &file);
        let ctx = egui::Context::default();
        let mut t = Thumbnails::default();
        assert_eq!(t.get(&ctx, &doc, "clip"), None, "the first call queues it");
        let (_, size) = wait(&mut t, &ctx, &doc).expect("a thumbnail");
        assert_eq!(size, egui::vec2(160.0, 40.0));
        // A node that's gone is dropped (at the next sweep).
        let mut empty = Thumbnails { swept: None, ..Thumbnails::default() };
        empty.entries = std::mem::take(&mut t.entries);
        let other = Doc::new(Graph::default(), None);
        assert_eq!(empty.get(&ctx, &other, "clip"), None);
        assert!(empty.entries.is_empty());
    }
}
