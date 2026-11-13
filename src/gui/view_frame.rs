//! The frame the viewer shows, without reading it back from the GPU
//! until something needs its pixels on the CPU.
//!
//! A result made on the GPU ([`Frame::Gpu`]) is drawn there by the viewer
//! (`gpu::display`), so playing it back needs no per-frame read-back. What
//! the viewer and the app read of it besides — its size, data window,
//! layers and scene — is kept here as it's made; the color picker reads
//! single pixels ([`ViewFrame::pixel`], remembered); and [`ViewFrame::image`]
//! reads it all back, once, for what needs it (a plane, a window on
//! another device). Built without the GPU, a ViewFrame is just the CPU
//! image.

use arclight::graph::Frame;
use arclight::image::{IRect, Image, Size};
use arclight::scene::Scene;
use std::sync::{Arc, Mutex, OnceLock};

/// Picked pixels remembered per frame.
const PICKS: usize = 16;

/// A picked pixel: its position and value.
type Pick = ((i32, i32), [f32; 4]);

pub struct ViewFrame {
    format: Size,
    window: IRect,
    layers: Vec<(String, Vec<String>)>,
    scene: Option<Arc<Scene>>,
    source: Source,
    /// Pixels read from the GPU, most recent last.
    picks: Mutex<Vec<Pick>>,
}

enum Source {
    Cpu(Arc<Image>),
    /// On the GPU; read back on the first [`ViewFrame::image`] (its
    /// outcome kept, so a failure isn't retried).
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    Gpu(Arc<arclight::gpu::Resident>, OnceLock<Result<Arc<Image>, String>>),
}

impl ViewFrame {
    pub fn from_cpu(img: Arc<Image>) -> ViewFrame {
        ViewFrame { format: img.format, window: img.window(), layers: img.layers(), scene: img.scene.clone(), source: Source::Cpu(img), picks: Mutex::default() }
    }

    /// A result left on the GPU: its metadata now, its pixels when asked.
    pub fn from_gpu(r: Arc<arclight::gpu::Resident>) -> ViewFrame {
        let (format, window) = r.shape();
        // (Layers from plane names alone, as `Image::layers` orders them.)
        let mut names = Image::blank(Size { width: 0, height: 0 }, IRect::default());
        names.planes = r.plane_names().into_iter().map(|name| arclight::image::Plane { name, data: Vec::new() }).collect();
        let done = OnceLock::new();
        if let Some(img) = r.read_back_done() {
            let _ = done.set(Ok(img));
        }
        ViewFrame { format, window, layers: names.layers(), scene: r.scene(), source: Source::Gpu(r, done), picks: Mutex::default() }
    }

    pub fn from_frame(frame: Frame) -> ViewFrame {
        match frame {
            Frame::Cpu(img) => ViewFrame::from_cpu(img),
            Frame::Gpu(r) => ViewFrame::from_gpu(r),
        }
    }

    pub fn format(&self) -> Size {
        self.format
    }

    /// The data window.
    pub fn window(&self) -> IRect {
        self.window
    }

    /// The frame (0, 0, width, height).
    pub fn frame(&self) -> IRect {
        IRect::frame(self.format)
    }

    /// Its planes by layer (see `Image::layers`).
    pub fn layers(&self) -> &[(String, Vec<String>)] {
        &self.layers
    }

    pub fn scene(&self) -> Option<&Arc<Scene>> {
        self.scene.as_ref()
    }

    /// Still on the GPU: the result there (for drawing it there).
    pub fn gpu(&self) -> Option<&Arc<arclight::gpu::Resident>> {
        match &self.source {
            Source::Gpu(r, _) => Some(r),
            Source::Cpu(_) => None,
        }
    }

    /// The image on the CPU: read back the first time (once; a failure
    /// stays one: see [`ViewFrame::read_failed`]).
    pub fn image(&self) -> Result<Arc<Image>, String> {
        match &self.source {
            Source::Cpu(img) => Ok(img.clone()),
            Source::Gpu(r, done) => done.get_or_init(|| r.cpu().map_err(|e| format!("reading the frame back from the GPU: {e:#}"))).clone(),
        }
    }

    /// The image on the CPU if it's there already (never reads back).
    pub fn image_if_read(&self) -> Option<Arc<Image>> {
        match &self.source {
            Source::Cpu(img) => Some(img.clone()),
            Source::Gpu(_, done) => done.get().and_then(|r| r.as_ref().ok()).cloned(),
        }
    }

    /// Why reading it back failed, when it did.
    pub fn read_failed(&self) -> Option<String> {
        match &self.source {
            Source::Gpu(_, done) => done.get().and_then(|r| r.as_ref().err()).cloned(),
            Source::Cpu(_) => None,
        }
    }

    /// The pixel at frame position (`x`, `y`) (`Image::at`): from the CPU
    /// image when there is one, else read alone from the GPU and
    /// remembered.
    pub fn pixel(&self, x: i32, y: i32) -> Result<[f32; 4], String> {
        if let Some(img) = self.image_if_read() {
            return Ok(img.at(x, y));
        }
        let Source::Gpu(r, _) = &self.source else { unreachable!("a CPU frame has its image") };
        let mut picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, px)) = picks.iter().find(|(at, _)| *at == (x, y)) {
            return Ok(*px);
        }
        let px = r.read_pixel(x, y).map_err(|e| format!("reading a pixel from the GPU: {e:#}"))?;
        if picks.len() >= PICKS {
            picks.remove(0);
        }
        picks.push(((x, y), px));
        Ok(px)
    }

    /// Bytes held on the CPU (what's been read back too).
    pub fn cpu_bytes(&self) -> usize {
        self.image_if_read().map_or(0, |img| img.bytes())
    }

    /// Bytes held on the GPU.
    pub fn gpu_bytes(&self) -> u64 {
        self.gpu().map_or(0, |r| r.bytes())
    }

    /// The same frame without its GPU copy, read back first if it has to
    /// be (Err: it couldn't be).
    pub fn without_gpu(&self) -> Result<ViewFrame, String> {
        Ok(ViewFrame::from_cpu(self.image()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> Arc<Image> {
        let mut img = Image::blank(Size { width: 64, height: 48 }, IRect::new(-4, 2, 60, 50));
        for (i, px) in img.pixels.iter_mut().enumerate() {
            *px = [(i % 7) as f32 / 7.0, (i % 5) as f32 / 5.0, 0.25, 1.0];
        }
        img.planes.push(arclight::image::Plane { name: "depth.Z".into(), data: vec![2.0; img.pixels.len()] });
        Arc::new(img)
    }

    /// A CPU frame is its image: metadata, pixels and no GPU.
    #[test]
    fn a_cpu_frame_is_its_image() {
        let img = checker();
        let f = ViewFrame::from_frame(Frame::Cpu(img.clone()));
        assert!(f.gpu().is_none() && f.read_failed().is_none());
        assert_eq!((f.format(), f.window(), f.frame()), (img.format, img.window(), img.frame()));
        assert_eq!(f.layers(), &img.layers()[..]);
        assert!(Arc::ptr_eq(&f.image().unwrap(), &img));
        assert_eq!(f.pixel(3, 4).unwrap(), img.at(3, 4));
        assert_eq!(f.pixel(-10, 4).unwrap(), [0.0; 4]);
        assert_eq!(f.cpu_bytes(), img.bytes());
        assert_eq!(f.gpu_bytes(), 0);
    }

    /// A GPU frame: metadata without a read-back, pixels read alone (and
    /// remembered), the whole image read back once on demand, the same as
    /// the CPU's.
    #[cfg(feature = "gpu")]
    #[test]
    fn a_gpu_frame_reads_back_only_what_is_asked() {
        let Some(gpu) = arclight::gpu::test_device() else { return eprintln!("skipped: no GPU adapter") };
        let img = checker();
        let up = arclight::gpu::GpuImage::upload(&gpu, &img).unwrap();
        let r = Arc::new(arclight::gpu::Resident::new(gpu, up));
        let f = ViewFrame::from_gpu(r.clone());
        assert!(f.gpu().is_some());
        assert_eq!((f.format(), f.window()), (img.format, img.window()));
        assert_eq!(f.layers(), &img.layers()[..], "layers from the plane names");
        assert_eq!(f.cpu_bytes(), 0, "nothing read back yet");
        for (x, y) in [(0, 2), (30, 20), (59, 49), (60, 49), (-5, 2)] {
            assert_eq!(f.pixel(x, y).unwrap(), img.at(x, y), "({x}, {y})");
        }
        assert!(r.read_back_done().is_none(), "pixels alone didn't read it back");
        let back = f.image().unwrap();
        assert_eq!(back.pixels, img.pixels);
        assert_eq!(back.planes, img.planes);
        assert!(Arc::ptr_eq(&back, &f.image().unwrap()), "read back once");
        assert_eq!(f.cpu_bytes(), img.bytes());
        let cpu = f.without_gpu().unwrap();
        assert!(cpu.gpu().is_none() && cpu.image().unwrap().pixels == img.pixels);
    }

    /// Playback, before and after: frames of a plate blurred and graded
    /// on the GPU, each new (the blur moves), the old way (the result read
    /// back every frame) against the new (left on the GPU, waited on).
    /// `cargo test --release --bin arclight-gui playback_bench -- --ignored --nocapture`
    #[cfg(feature = "gpu")]
    #[test]
    #[ignore]
    fn playback_bench() {
        use arclight::graph::{Evaluator, Graph};
        use arclight::ops::EvalContext;
        let Some(gpu) = arclight::gpu::get() else { return eprintln!("skipped: no GPU adapter") };
        const FRAMES: usize = 60;
        for (name, w, h) in [("1080p", 1920, 1080), ("4K", 3840, 2160)] {
            let graph = |i: usize| -> Graph {
                serde_json::from_value(serde_json::json!({ "nodes": [
                    { "id": "plate", "op": "checkerboard", "params": { "width": w, "height": h, "size": 32 } },
                    { "id": "soft", "op": "blur", "inputs": ["plate"], "params": { "radius": 3.0 + i as f64 * 0.01 } },
                    { "id": "look", "op": "grade", "inputs": ["soft"], "params": { "gain": [1.2, 1.1, 0.9, 1.0] } },
                ] }))
                .unwrap()
            };
            let ctx = EvalContext { frame: 1, write_outputs: false };
            let run = |lazy: bool| {
                let mut ev = Evaluator::new();
                // Warm: pipelines, the plate's upload.
                let _ = ev.evaluate_resident(&graph(FRAMES + 1), "look", &ctx).unwrap();
                let t = std::time::Instant::now();
                for i in 0..FRAMES {
                    if lazy {
                        let f = ViewFrame::from_frame(ev.evaluate_lazy(&graph(i), "look", &ctx).unwrap());
                        assert!(f.gpu().is_some(), "made on the GPU");
                        gpu.wait().unwrap();
                    } else {
                        let (img, r) = ev.evaluate_resident(&graph(i), "look", &ctx).unwrap();
                        assert!(r.is_some() && img.format.width == w);
                    }
                }
                t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64
            };
            let before = run(false);
            let after = run(true);
            println!("{name}: read back every frame {before:.2} ms ({:.1} fps); left on the GPU {after:.2} ms ({:.1} fps)", 1000.0 / before, 1000.0 / after);
        }
    }
}
