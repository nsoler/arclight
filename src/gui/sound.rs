//! Sound with playback: the viewed node's audio
//! ([`arclight::audio::source_at`]) played along with the playhead while
//! playing forward, silent otherwise (paused, stepping, scrubbing, playing
//! backward).
//!
//! The playhead runs on the wall clock (frames are skipped, not slowed),
//! and the audio follows it: each update [`Sync`] compares where the
//! output is heard with where the playhead says it should be, and cues it
//! there when they're apart (a seek, a loop's wrap, another source). While
//! the picture falls behind the playhead (frames not rendered in time) the
//! audio goes silent, and comes back cued at the playhead once the picture
//! catches up: dropped, never stuttering or drifting.
//!
//! The output ([`Device`], cpal) is opened when first needed; without one
//! sound is off, said once in the status line. It plays windows of
//! [`WINDOW`] seconds decoded ahead of the playhead on a thread of its own
//! (ffmpeg resamples and mixes to the device's rate and channels); a window
//! not decoded in time plays as silence.

use arclight::audio;
#[cfg(test)]
use std::str::FromStr;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How far apart (seconds) what's heard and the playhead may get before the
/// output is cued again.
pub const TOLERANCE: f64 = 0.06;
/// Frames the picture may fall behind the playhead before the sound drops,
/// and how close it must come back for the sound to resume.
pub const BEHIND: i64 = 3;
pub const CAUGHT_UP: i64 = 1;
/// Seconds of sound decoded at a time.
pub const WINDOW: f64 = 2.0;
/// Windows decoded ahead of the one playing.
const AHEAD: i64 = 2;

/// Where the sound should be heard now: `secs` into `path`, which plays
/// `speed` times as fast as real time (a clip whose frame rate isn't the
/// project's plays at the picture's pace).
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub path: PathBuf,
    pub secs: f64,
    pub speed: f64,
}

impl Cue {
    /// Where the playhead at `pos` (a frame and how far into it) is heard in
    /// `node`'s audio: None when it has none there.
    pub fn at(graph: &arclight::graph::Graph, node: &str, pos: f64, fps: f64) -> Option<Cue> {
        let frame = pos.floor();
        let src = audio::source_at(graph, node, frame as i64)?;
        let speed = (fps * src.secs_per_frame).clamp(0.25, 4.0);
        Some(Cue { path: src.path, secs: src.secs + (pos - frame) * src.secs_per_frame, speed })
    }
}

/// Something that plays sound (the device, or a fake in tests).
pub trait Output {
    /// Play `path` from `secs` (heard now), at `speed`.
    fn cue(&mut self, path: &Path, secs: f64, speed: f64);
    fn stop(&mut self);
    /// Where in its source the output is heard now; None when stopped.
    fn heard(&self) -> Option<f64>;
    /// Keep sound coming (decode ahead); called each update.
    fn pump(&mut self) {}
}

/// What an update did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to play: silent.
    Silent,
    /// The picture is behind the playhead: silent until it catches up.
    Behind,
    /// Cued at the playhead.
    Cued,
    /// Playing on, in step.
    Playing,
}

/// Keeps an [`Output`] in step with the playhead.
#[derive(Default)]
pub struct Sync {
    behind: bool,
    /// What the output was cued with: path and speed.
    cued: Option<(PathBuf, f64)>,
}

impl Sync {
    /// `want`: where the playhead says the sound should be heard (None:
    /// silent); `lag`: frames the picture shown is behind the playhead.
    pub fn update(&mut self, want: Option<&Cue>, lag: i64, out: &mut dyn Output) -> Step {
        if lag >= BEHIND {
            self.behind = true;
        } else if lag <= CAUGHT_UP {
            self.behind = false;
        }
        let step = match want {
            None => Step::Silent,
            Some(_) if self.behind => Step::Behind,
            Some(c) => {
                let same = self.cued.as_ref().is_some_and(|(p, s)| *p == c.path && *s == c.speed);
                if same && out.heard().is_some_and(|h| (h - c.secs).abs() <= TOLERANCE) {
                    Step::Playing
                } else {
                    out.cue(&c.path, c.secs, c.speed);
                    self.cued = Some((c.path.clone(), c.speed));
                    Step::Cued
                }
            }
        };
        if matches!(step, Step::Silent | Step::Behind) && self.cued.take().is_some() {
            out.stop();
        }
        out.pump();
        step
    }
}

/// The GUI's sound: on or off, the device opened when first needed.
#[derive(Default)]
pub struct Sound {
    sync: Sync,
    device: Option<Device>,
    /// Opening the device failed (or it went away): off until sound is
    /// turned off and on again.
    failed: bool,
    was_on: bool,
}

impl Sound {
    /// Play what `want` says when `on`. Returns a message for the status
    /// line when the device can't be opened or stops working.
    pub fn update(&mut self, on: bool, want: Option<&Cue>, lag: i64) -> Option<String> {
        if on && !self.was_on {
            self.failed = false;
        }
        self.was_on = on;
        let want = want.filter(|_| on && !self.failed);
        let mut message = None;
        if want.is_some() && self.device.is_none() {
            match Device::open() {
                Ok(d) => self.device = Some(d),
                Err(e) => {
                    self.failed = true;
                    return Some(format!("sound is off: {e}"));
                }
            }
        }
        if let Some(d) = &mut self.device {
            self.sync.update(want, lag, d);
            if let Some(e) = d.error() {
                self.device = None;
                self.sync = Sync::default();
                self.failed = true;
                message = Some(format!("sound stopped: {e}"));
            }
        }
        message
    }
}

/// What the output callback plays, shared with the GUI thread.
struct Shared {
    playing: bool,
    /// The source's frames per second as decoded: the device's rate times
    /// the speed.
    rate: u32,
    /// The next source frame (at `rate`) to write.
    pos: i64,
    /// Decoded windows by index (window k starts k * WINDOW seconds in).
    windows: HashMap<i64, Arc<[f32]>>,
    /// `pos` at the last callback, when, and the latency then (seconds
    /// from written to heard).
    stamp: Option<(i64, Instant, f64)>,
    latency: f64,
    error: Option<String>,
}

impl Shared {
    /// Write the next frames into `data` (`channels` samples a frame) from
    /// the decoded windows, silence where one isn't decoded yet: the
    /// position moves on regardless, so the sound stays in step.
    fn fill<T: cpal::SizedSample + cpal::FromSample<f32>>(&mut self, data: &mut [T], channels: usize) {
        let win = window_frames(self.rate);
        let silence = T::from_sample(0.0f32);
        let mut window: Option<(i64, Option<Arc<[f32]>>)> = None;
        for frame in data.chunks_mut(channels) {
            let (k, i) = (self.pos.div_euclid(win), self.pos.rem_euclid(win) as usize);
            if window.as_ref().is_none_or(|(w, _)| *w != k) {
                window = Some((k, self.windows.get(&k).cloned()));
            }
            match window.as_ref().and_then(|(_, w)| w.as_ref()).and_then(|w| w.get(i * channels..(i + 1) * channels)) {
                Some(src) => frame.iter_mut().zip(src).for_each(|(d, &v)| *d = T::from_sample(v)),
                None => frame.fill(silence),
            }
            self.pos += 1;
        }
    }
}

/// A window to decode.
struct Job {
    source: u64,
    path: PathBuf,
    k: i64,
    rate: u32,
}

/// The system's default output device, playing decoded windows.
pub struct Device {
    _stream: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    rate: u32,
    /// The source the windows are for (path and decode rate); `source`
    /// counts changes so stale decodes are dropped.
    current: Option<(PathBuf, u32)>,
    source: Arc<Mutex<u64>>,
    requested: HashSet<i64>,
    jobs: Sender<Job>,
}

impl Device {
    pub fn open() -> anyhow::Result<Device> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
        let config = device.default_output_config()?;
        let (rate, channels) = (config.sample_rate(), config.channels() as u32);
        let shared = Arc::new(Mutex::new(Shared { playing: false, rate, pos: 0, windows: HashMap::new(), stamp: None, latency: 0.0, error: None }));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, config.config(), &shared)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, config.config(), &shared)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, config.config(), &shared)?,
            cpal::SampleFormat::I32 => build::<i32>(&device, config.config(), &shared)?,
            f => anyhow::bail!("the output device's sample format ({f}) isn't supported"),
        };
        stream.play()?;
        let source = Arc::new(Mutex::new(0));
        let (jobs, rx) = mpsc::channel();
        let (s, src) = (shared.clone(), source.clone());
        std::thread::Builder::new().name("sound decode".into()).spawn(move || decode_jobs(rx, s, src, channels))?;
        Ok(Device { _stream: stream, shared, rate, current: None, source, requested: HashSet::new(), jobs })
    }

    /// Why the stream stopped, if it did.
    fn error(&self) -> Option<String> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).error.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Output for Device {
    fn cue(&mut self, path: &Path, secs: f64, speed: f64) {
        let rate = (self.rate as f64 * speed).round().max(1.0) as u32;
        if self.current.as_ref().is_none_or(|(p, r)| p != path || *r != rate) {
            // Another source: what's decoded or asked for is for the old one.
            self.current = Some((path.to_path_buf(), rate));
            *self.source.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            self.requested.clear();
            self.lock().windows.clear();
        }
        let mut s = self.lock();
        // What's written now is heard after the latency.
        let pos = ((secs + s.latency * speed) * rate as f64).round() as i64;
        (s.rate, s.pos, s.playing) = (rate, pos, true);
        s.stamp = Some((pos, Instant::now(), s.latency));
    }

    fn stop(&mut self) {
        let mut s = self.lock();
        s.playing = false;
        s.stamp = None;
    }

    fn heard(&self) -> Option<f64> {
        let s = self.lock();
        let (pos, at, latency) = s.stamp.filter(|_| s.playing)?;
        let speed = s.rate as f64 / self.rate as f64;
        Some(pos as f64 / s.rate as f64 + (at.elapsed().as_secs_f64() - latency) * speed)
    }

    fn pump(&mut self) {
        let Some((path, rate)) = self.current.clone() else { return };
        let (playing, k) = {
            let s = self.lock();
            (s.playing, s.pos.div_euclid(window_frames(s.rate)))
        };
        if !playing {
            return;
        }
        let keep = k - 1..=k + AHEAD;
        self.lock().windows.retain(|w, _| keep.contains(w));
        self.requested.retain(|w| keep.contains(w));
        let source = *self.source.lock().unwrap_or_else(|e| e.into_inner());
        for w in k..=k + AHEAD {
            if self.requested.insert(w) {
                let _ = self.jobs.send(Job { source, path: path.clone(), k: w, rate });
            }
        }
    }
}

fn window_frames(rate: u32) -> i64 {
    (WINDOW * rate as f64).round() as i64
}

/// Decode windows as asked, skipping those for a source no longer played.
fn decode_jobs(rx: Receiver<Job>, shared: Arc<Mutex<Shared>>, source: Arc<Mutex<u64>>, channels: u32) {
    let current = || *source.lock().unwrap_or_else(|e| e.into_inner());
    for job in rx {
        if job.source != current() {
            continue;
        }
        let frames = window_frames(job.rate);
        let samples = audio::decode(&job.path, job.k as f64 * WINDOW, frames as f64 / job.rate as f64, job.rate, channels)
            // Undecodable: a window of silence (rather than asking again).
            .unwrap_or_else(|_| vec![0.0; frames as usize * channels as usize].into());
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        if job.source == current() {
            s.windows.insert(job.k, samples);
        }
    }
}

/// An output stream of `T` samples playing `shared`.
fn build<T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static>(device: &cpal::Device, config: cpal::StreamConfig, shared: &Arc<Mutex<Shared>>) -> anyhow::Result<cpal::Stream> {
    let channels = config.channels as usize;
    let (play, errors) = (shared.clone(), shared.clone());
    let silence = T::from_sample(0.0f32);
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            // Never wait in the audio callback: busy, this buffer is silent.
            let Ok(mut s) = play.try_lock() else {
                data.fill(silence);
                return;
            };
            let t = info.timestamp();
            s.latency = t.playback.duration_since(t.callback).as_secs_f64();
            if !s.playing {
                data.fill(silence);
                return;
            }
            s.stamp = Some((s.pos, Instant::now(), s.latency));
            s.fill(data, channels);
        },
        move |e: cpal::Error| {
            // Rerouted (to a new default device) or played without real-time
            // priority: still playing. Anything else stops the sound.
            if matches!(e.kind(), cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::Xrun) {
                return;
            }
            let mut s = errors.lock().unwrap_or_else(|e| e.into_inner());
            s.error.get_or_insert_with(|| e.to_string());
        },
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An output on a fake clock: heard where cued, plus the time since at
    /// its speed.
    #[derive(Default)]
    struct Fake {
        now: f64,
        cued: Option<(PathBuf, f64, f64, f64)>,
        cues: usize,
        stops: usize,
    }

    impl Output for Fake {
        fn cue(&mut self, path: &Path, secs: f64, speed: f64) {
            self.cued = Some((path.to_path_buf(), secs, speed, self.now));
            self.cues += 1;
        }
        fn stop(&mut self) {
            self.cued = None;
            self.stops += 1;
        }
        fn heard(&self) -> Option<f64> {
            self.cued.as_ref().map(|(_, secs, speed, at)| secs + (self.now - at) * speed)
        }
    }

    fn cue(path: &str, secs: f64) -> Cue {
        Cue { path: path.into(), secs, speed: 1.0 }
    }

    #[test]
    fn sound_follows_the_playhead_and_is_cued_again_when_apart() {
        let (mut sync, mut out) = (Sync::default(), Fake::default());
        assert_eq!(sync.update(Some(&cue("a.mov", 10.0)), 0, &mut out), Step::Cued);
        // Playing on in step: left alone, for as long as it stays in step.
        for i in 1..100 {
            out.now = i as f64 / 60.0;
            assert_eq!(sync.update(Some(&cue("a.mov", 10.0 + out.now + 0.01)), 1, &mut out), Step::Playing);
        }
        assert_eq!(out.cues, 1);
        // A loop's wrap (or a seek): cued at the loop point.
        assert_eq!(sync.update(Some(&cue("a.mov", 10.0)), 0, &mut out), Step::Cued);
        assert_eq!(out.heard(), Some(10.0));
        // Drifted past the tolerance: cued again; within it, not.
        out.now += 1.0;
        assert_eq!(sync.update(Some(&cue("a.mov", 11.0 + TOLERANCE / 2.0)), 0, &mut out), Step::Playing);
        assert_eq!(sync.update(Some(&cue("a.mov", 11.0 + 2.0 * TOLERANCE)), 0, &mut out), Step::Cued);
        // Another source, or the same at another speed.
        assert_eq!(sync.update(Some(&cue("b.wav", 11.0 + 2.0 * TOLERANCE)), 0, &mut out), Step::Cued);
        let slower = Cue { speed: 24.0 / 25.0, ..cue("b.wav", 11.0 + 2.0 * TOLERANCE) };
        assert_eq!(sync.update(Some(&slower), 0, &mut out), Step::Cued);
        assert_eq!(out.cued.as_ref().unwrap().2, 24.0 / 25.0);
        assert_eq!((out.cues, out.stops), (5, 0));
    }

    #[test]
    fn behind_the_picture_the_sound_drops_and_comes_back_in_step() {
        let (mut sync, mut out) = (Sync::default(), Fake::default());
        sync.update(Some(&cue("a.mov", 0.0)), 0, &mut out);
        // Frames come late: silent, and stays silent until the picture is
        // close again (not flickering on and off at the edge).
        out.now = 0.5;
        assert_eq!(sync.update(Some(&cue("a.mov", 0.5)), BEHIND, &mut out), Step::Behind);
        assert_eq!((out.heard(), out.stops), (None, 1));
        out.now = 0.6;
        assert_eq!(sync.update(Some(&cue("a.mov", 0.6)), BEHIND - 1, &mut out), Step::Behind);
        assert_eq!(out.stops, 1, "stopped once");
        // Caught up: back where the playhead is now, not where it dropped.
        out.now = 1.5;
        assert_eq!(sync.update(Some(&cue("a.mov", 1.5)), CAUGHT_UP, &mut out), Step::Cued);
        assert_eq!(out.heard(), Some(1.5));
        out.now = 2.0;
        assert_eq!(sync.update(Some(&cue("a.mov", 2.0)), 0, &mut out), Step::Playing);
    }

    #[test]
    fn paused_backward_or_no_audio_is_silent() {
        let (mut sync, mut out) = (Sync::default(), Fake::default());
        assert_eq!(sync.update(None, 0, &mut out), Step::Silent);
        assert_eq!(out.stops, 0, "nothing to stop");
        sync.update(Some(&cue("a.mov", 3.0)), 0, &mut out);
        // Paused, stepping, scrubbing, backward, sound off: no cue wanted.
        assert_eq!(sync.update(None, 0, &mut out), Step::Silent);
        assert_eq!((out.heard(), out.stops), (None, 1));
        assert_eq!(sync.update(None, 0, &mut out), Step::Silent);
        assert_eq!(out.stops, 1);
        // Playing again: cued where the playhead is.
        assert_eq!(sync.update(Some(&cue("a.mov", 7.0)), 0, &mut out), Step::Cued);
        assert_eq!(out.heard(), Some(7.0));
    }

    #[test]
    fn decoded_windows_play_in_order_and_missing_ones_as_silence() {
        // Windows of 2 s at 4 frames a second: 8 frames each, 2 channels.
        let mut s = Shared { playing: true, rate: 4, pos: 6, windows: HashMap::new(), stamp: None, latency: 0.0, error: None };
        let w = |v: f32| -> Arc<[f32]> { (0..16).map(|i| v + (i / 2) as f32).collect::<Vec<_>>().into() };
        s.windows.insert(0, w(0.0));
        s.windows.insert(2, w(100.0));
        let mut out = [9.0f32; 24];
        s.fill(&mut out, 2);
        // Frames 6, 7 from window 0; 8-15 (window 1) silent; 16, 17 from 2.
        assert_eq!(&out[..4], &[6.0, 6.0, 7.0, 7.0]);
        assert!(out[4..20].iter().all(|&v| v == 0.0));
        assert_eq!(&out[20..], &[100.0, 100.0, 101.0, 101.0]);
        assert_eq!(s.pos, 18, "moved on through the gap");
        // Integer devices get the samples converted.
        let mut ints = [0i16; 2];
        s.windows.insert(2, vec![0.5f32; 16].into());
        s.fill(&mut ints, 2);
        assert!(ints.iter().all(|&v| (v as f32 - 16384.0).abs() < 2.0), "{ints:?}");
    }

    #[test]
    fn a_clip_at_another_frame_rate_keeps_the_pictures_pace() {
        // A 25 fps clip in a 24 fps project: its frames play at 24 a second,
        // so its sound at 24/25 speed, and a frame's fraction is the clip's.
        let dir = std::env::temp_dir().join(format!("arclight_sound_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip25.mov");
        let made = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=32x32:rate=25:duration=1", "-f", "lavfi", "-i", "sine=f=440:d=1"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le", "-shortest"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let g = arclight::graph::Graph::from_str(&format!(r#"{{"nodes": [
            {{"id": "plate", "op": "read", "params": {{"path": {path:?}}}}},
            {{"id": "grade", "op": "grade", "inputs": ["plate"]}}]}}"#)).unwrap();
        let c = Cue::at(&g, "grade", 3.5, 24.0).expect("the plate's sound, through the grade");
        assert_eq!(c.path, path);
        assert!((c.secs - 2.5 / 25.0).abs() < 1e-9 && (c.speed - 24.0 / 25.0).abs() < 1e-9, "{c:?}");
        assert!(Cue::at(&g, "grade", 0.5, 24.0).is_none(), "before the clip");
        assert!(Cue::at(&g, "nope", 3.0, 24.0).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// On the real default device (run by hand: cargo test --release --bin
    /// arclight-gui -- --ignored plays_on_the_default_device): a clip's
    /// sound is decoded ahead and heard in step with real time.
    #[test]
    #[ignore]
    fn plays_on_the_default_device() {
        let dir = std::env::temp_dir().join(format!("arclight_sound_dev_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.mov");
        let made = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=32x32:rate=24:duration=6", "-f", "lavfi", "-i", "sine=f=440:d=6"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le", "-shortest"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        assert!(made, "needs ffmpeg");
        let mut d = match Device::open() {
            Ok(d) => d,
            Err(e) => return eprintln!("no device here: {e}"),
        };
        let (mut sync, start) = (Sync::default(), Instant::now());
        let mut steps = Vec::new();
        while start.elapsed().as_secs_f64() < 3.0 {
            let want = Cue { path: path.clone(), secs: 1.0 + start.elapsed().as_secs_f64(), speed: 1.0 };
            steps.push(sync.update(Some(&want), 0, &mut d));
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        assert_eq!(d.error(), None);
        let heard = d.heard().unwrap();
        let cues = steps.iter().filter(|s| **s == Step::Cued).count();
        let decoded: Vec<i64> = { let mut k: Vec<i64> = d.lock().windows.keys().copied().collect(); k.sort(); k };
        eprintln!("heard {heard:.3} at {:.3}, cued {cues} times, windows {decoded:?}, latency {:.3}", 1.0 + start.elapsed().as_secs_f64(), d.lock().latency);
        assert!((heard - (1.0 + start.elapsed().as_secs_f64())).abs() < TOLERANCE);
        assert!(cues <= 2, "cued once (maybe again once the latency is known): {cues}");
        assert!(decoded.contains(&2), "windows ahead decoded: {decoded:?}");
        d.stop();
        std::fs::remove_dir_all(&dir).ok();
    }
}
