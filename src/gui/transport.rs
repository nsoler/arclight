//! Playback transport: go-to-start/end, step, play forward/backward, pause,
//! a scrubbable timeline, and the playback clock.

use arclight::graph::Project;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Play {
    Stopped,
    Forward,
    Backward,
}

pub struct Transport {
    pub frame: i64,
    pub play: Play,
    pub looping: bool,
    /// Sound with forward playback (the speaker button).
    pub sound: bool,
    /// Wall-clock time and frame at which playback (re)started.
    anchor: Option<(Instant, i64)>,
    /// Smoothed frames actually displayed per second while playing.
    pub shown_fps: f32,
    last_shown: Option<Instant>,
    /// The control row's width last frame (None: not measured yet, or too
    /// wide for the panel), to center it by.
    row_width: Option<f32>,
    /// Where the control row was drawn last frame (screen).
    row: Rect,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            frame: 1,
            play: Play::Stopped,
            looping: true,
            sound: true,
            anchor: None,
            shown_fps: 0.0,
            last_shown: None,
            row_width: None,
            row: Rect::NOTHING,
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    ToStart,
    StepBack,
    PlayBack,
    Pause,
    PlayFwd,
    StepFwd,
    ToEnd,
    /// A speaker: with sound waves, or (off) crossed out.
    Sound(bool),
}

impl Transport {
    pub fn set_play(&mut self, play: Play) {
        self.play = play;
        self.anchor = (play != Play::Stopped).then(|| (Instant::now(), self.frame));
        self.shown_fps = 0.0;
        self.last_shown = None;
    }

    pub fn toggle(&mut self, dir: Play) {
        self.set_play(if self.play == dir { Play::Stopped } else { dir });
    }

    pub fn seek(&mut self, frame: i64, project: &Project) {
        self.frame = frame.clamp(project.first_frame, project.last_frame.max(project.first_frame));
        if self.play != Play::Stopped {
            self.anchor = Some((Instant::now(), self.frame));
        }
    }

    pub fn step(&mut self, delta: i64, project: &Project) {
        self.set_play(Play::Stopped);
        let (first, last) = (project.first_frame, project.last_frame.max(project.first_frame));
        let mut f = self.frame + delta;
        if self.looping {
            if f > last {
                f = first;
            } else if f < first {
                f = last;
            }
        }
        self.seek(f, project);
    }

    /// Advance the playback clock. Real-time: if rendering is slower than the
    /// project rate, frames are skipped rather than the clock slowing down.
    pub fn tick(&mut self, project: &Project) {
        let Some((t0, f0)) = self.anchor else { return };
        let (first, last) = (project.first_frame, project.last_frame.max(project.first_frame));
        let elapsed = (t0.elapsed().as_secs_f64() * project.fps.max(0.1)).floor() as i64;
        let dir = if self.play == Play::Backward { -1 } else { 1 };
        let mut f = f0 + dir * elapsed;
        if f > last || f < first {
            if self.looping {
                let len = last - first + 1;
                f = first + (f - first).rem_euclid(len);
            } else {
                f = f.clamp(first, last);
                self.set_play(Play::Stopped);
            }
        }
        self.frame = f;
    }

    /// Where forward playback is now: the frame and how far into it, for
    /// sound to follow; None unless playing forward.
    pub fn play_position(&self, project: &Project) -> Option<f64> {
        let (t0, f0) = self.anchor.filter(|_| self.play == Play::Forward)?;
        Some(forward_position(f0, t0.elapsed().as_secs_f64(), project, self.looping))
    }

    /// Call when a newly rendered frame reaches the screen during playback.
    pub fn frame_shown(&mut self) {
        if self.play == Play::Stopped {
            return;
        }
        let now = Instant::now();
        if let Some(prev) = self.last_shown {
            let inst = 1.0 / now.duration_since(prev).as_secs_f32().max(1e-4);
            self.shown_fps = if self.shown_fps == 0.0 { inst } else { self.shown_fps * 0.9 + inst * 0.1 };
        }
        self.last_shown = Some(now);
    }

    /// Draw the transport. Returns true if the project settings were edited.
    /// `keys` are keyframe frames to mark on the timeline, `weak` frames to
    /// flag (a Tracker's weak matches).
    pub fn ui(&mut self, ui: &mut egui::Ui, project: &mut Project, cached: &[i64], keys: &[i64], weak: &[i64]) -> bool {
        let mut project_changed = false;
        self.timeline(ui, project, cached, keys, weak);
        let avail = ui.available_width();
        // Centered by last frame's width; a row too wide for the panel
        // starts at the left and wraps.
        let indent = self.row_width.map_or(0.0, |w| indent(avail, w));
        ui.horizontal_wrapped(|ui| {
            if indent > 0.0 {
                ui.add_space(indent);
            }
            let start = ui.cursor().min;
            let (first, last) = (project.first_frame, project.last_frame);
            if icon_button(ui, Icon::ToStart, false, "Go to start (Home)") {
                self.set_play(Play::Stopped);
                self.seek(first, project);
            }
            if icon_button(ui, Icon::StepBack, false, "Previous frame (←)") {
                self.step(-1, project);
            }
            if icon_button(ui, Icon::PlayBack, self.play == Play::Backward, "Play backward (J)") {
                self.toggle(Play::Backward);
            }
            if icon_button(ui, Icon::Pause, false, "Pause (K / Space)") {
                self.set_play(Play::Stopped);
            }
            if icon_button(ui, Icon::PlayFwd, self.play == Play::Forward, "Play forward (L / Space)") {
                self.toggle(Play::Forward);
            }
            if icon_button(ui, Icon::StepFwd, false, "Next frame (→)") {
                self.step(1, project);
            }
            if icon_button(ui, Icon::ToEnd, false, "Go to end (End)") {
                self.set_play(Play::Stopped);
                self.seek(last, project);
            }
            ui.separator();

            let mut f = self.frame;
            ui.label("Frame");
            if ui.add(egui::DragValue::new(&mut f).range(first..=last.max(first))).changed() {
                self.seek(f, project);
            }
            ui.separator();
            ui.label("In");
            project_changed |= ui.add(egui::DragValue::new(&mut project.first_frame)).changed();
            ui.label("Out");
            project_changed |= ui.add(egui::DragValue::new(&mut project.last_frame)).changed();
            ui.label("FPS");
            project_changed |= ui
                .add(egui::DragValue::new(&mut project.fps).range(1.0..=240.0).speed(0.1).max_decimals(3))
                .changed();
            ui.checkbox(&mut self.looping, "Loop");
            let tip = if self.sound { "Sound on: plays the viewed clip's audio while playing forward (click to mute)" } else { "Sound off (click to hear the viewed clip's audio while playing forward)" };
            if icon_button(ui, Icon::Sound(self.sound), self.sound, tip) {
                self.sound = !self.sound;
            }
            ui.separator();
            ui.label("Format").on_hover_text("The frame size of Constants, Checkerboards and shapes whose own width/height is 0 (the default)");
            project_changed |= ui.add(egui::DragValue::new(&mut project.format.width).range(1..=16384)).changed();
            ui.label("×");
            project_changed |= ui.add(egui::DragValue::new(&mut project.format.height).range(1..=16384)).changed();
            // Measured before the fps readout, which hangs off the right so
            // the row doesn't shift when playback starts.
            let end = ui.min_rect().max;
            let one_line = ui.cursor().min.y < start.y + 1.0;
            self.row = Rect::from_min_max(start, end);
            let width = one_line.then_some(end.x - start.x);
            if width.is_some_and(|w| self.row_width.is_none_or(|old| (w - old).abs() > 0.5)) || (width.is_none() && self.row_width.is_some()) {
                ui.ctx().request_discard("transport row width changed");
            }
            self.row_width = width;
            if self.play != Play::Stopped && self.shown_fps > 0.0 {
                ui.separator();
                let color = if self.shown_fps + 0.5 < project.fps as f32 {
                    Color32::from_rgb(230, 170, 60)
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(color, format!("{:.1} fps", self.shown_fps));
            }
        });
        if project_changed {
            project.last_frame = project.last_frame.max(project.first_frame);
            self.seek(self.frame, project);
        }
        project_changed
    }

    fn timeline(&mut self, ui: &mut egui::Ui, project: &Project, cached: &[i64], keys: &[i64], weak: &[i64]) {
        let (first, last) = (project.first_frame, project.last_frame.max(project.first_frame));
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(32));

        let span = (last - first + 1) as f32;
        let x_of = |f: i64| rect.left() + (f - first) as f32 / span * rect.width();
        let frame_w = rect.width() / span;

        // Cached frames: a thin green strip along the bottom.
        for &f in cached {
            let r = Rect::from_min_max(
                Pos2::new(x_of(f), rect.bottom() - 4.0),
                Pos2::new(x_of(f) + frame_w.max(1.0), rect.bottom()),
            );
            painter.rect_filled(r, 0.0, Color32::from_rgb(70, 160, 90));
        }

        // Flagged frames: an orange band behind the keys.
        for &f in weak.iter().filter(|&&f| f >= first && f <= last) {
            let r = Rect::from_min_max(Pos2::new(x_of(f), rect.bottom() - 16.0), Pos2::new(x_of(f) + frame_w.max(2.0), rect.bottom() - 4.0));
            painter.rect_filled(r, 0.0, Color32::from_rgb(240, 120, 50).gamma_multiply(0.7));
        }

        // Ticks with labels at a readable spacing.
        let step = [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 5000]
            .into_iter()
            .find(|s| *s as f32 * frame_w >= 48.0)
            .unwrap_or(10_000);
        let mut f = first + (step - first.rem_euclid(step)) % step;
        while f <= last {
            let x = x_of(f) + frame_w * 0.5;
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.top() + 6.0)], (1.0, Color32::from_gray(90)));
            // Keep edge labels inside the bar.
            let anchor = if x > rect.right() - 14.0 {
                egui::Align2::RIGHT_TOP
            } else if x < rect.left() + 14.0 {
                egui::Align2::LEFT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(Pos2::new(x, rect.top() + 7.0), anchor, f, FontId::proportional(10.0), Color32::from_gray(130));
            f += step;
        }

        // Keyframes of the selected node.
        for &f in keys.iter().filter(|&&f| f >= first && f <= last) {
            let c = Pos2::new(x_of(f) + frame_w * 0.5, rect.bottom() - 10.0);
            let r = 4.5;
            painter.add(Shape::convex_polygon(
                vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)],
                Color32::from_rgb(240, 175, 50),
                Stroke::new(1.0, Color32::from_black_alpha(160)),
            ));
        }

        // Playhead.
        let x = x_of(self.frame) + frame_w * 0.5;
        let head = Color32::from_rgb(240, 160, 40);
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(x, rect.center().y), Vec2::new(frame_w.max(2.0), rect.height())),
            0.0,
            head.gamma_multiply(0.35),
        );
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, head));

        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.clicked() {
                let f = first + ((pos.x - rect.left()) / frame_w).floor() as i64;
                self.seek(f, project);
            }
        }
    }
}

/// The space before a row `width` wide to center it in `avail` (none if
/// it doesn't fit).
fn indent(avail: f32, width: f32) -> f32 {
    ((avail - width) / 2.0).max(0.0)
}

/// A button with a vector-drawn transport glyph (no reliance on font glyphs).
fn icon_button(ui: &mut egui::Ui, icon: Icon, active: bool, tip: &str) -> bool {
    let size = Vec2::new(30.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let visuals = ui.style().interact_selectable(&resp, active);
    ui.painter().rect_filled(rect, 4.0, visuals.weak_bg_fill);
    let c = visuals.fg_stroke.color;
    let p = ui.painter();
    let ctr = rect.center();
    let tri = |dir: f32, cx: f32| {
        let h = 6.0;
        Shape::convex_polygon(
            vec![
                Pos2::new(cx - 5.0 * dir, ctr.y - h),
                Pos2::new(cx + 5.0 * dir, ctr.y),
                Pos2::new(cx - 5.0 * dir, ctr.y + h),
            ],
            c,
            Stroke::NONE,
        )
    };
    let bar = |x: f32| Rect::from_center_size(Pos2::new(x, ctr.y), Vec2::new(2.5, 12.0));
    match icon {
        Icon::ToStart => {
            p.rect_filled(bar(ctr.x - 6.0), 0.0, c);
            p.add(tri(-1.0, ctr.x + 1.0));
        }
        Icon::StepBack => {
            p.add(tri(-1.0, ctr.x - 1.0));
            p.rect_filled(bar(ctr.x + 6.0), 0.0, c);
        }
        Icon::PlayBack => {
            p.add(tri(-1.0, ctr.x));
        }
        Icon::Pause => {
            p.rect_filled(bar(ctr.x - 3.0), 0.0, c);
            p.rect_filled(bar(ctr.x + 3.0), 0.0, c);
        }
        Icon::PlayFwd => {
            p.add(tri(1.0, ctr.x));
        }
        Icon::StepFwd => {
            p.rect_filled(bar(ctr.x - 6.0), 0.0, c);
            p.add(tri(1.0, ctr.x + 1.0));
        }
        Icon::ToEnd => {
            p.add(tri(1.0, ctr.x - 1.0));
            p.rect_filled(bar(ctr.x + 6.0), 0.0, c);
        }
        Icon::Sound(on) => {
            let x = ctr.x - 4.0;
            p.rect_filled(Rect::from_center_size(Pos2::new(x - 4.0, ctr.y), Vec2::new(4.0, 5.0)), 0.0, c);
            let cone = vec![Pos2::new(x - 2.0, ctr.y - 2.5), Pos2::new(x + 3.0, ctr.y - 6.5), Pos2::new(x + 3.0, ctr.y + 6.5), Pos2::new(x - 2.0, ctr.y + 2.5)];
            p.add(Shape::convex_polygon(cone, c, Stroke::NONE));
            let stroke = Stroke::new(1.5, c);
            if on {
                for r in [4.0f32, 7.5] {
                    let arc = (0..=8).map(|i| {
                        let a = (i as f32 / 8.0 - 0.5) * 1.6;
                        Pos2::new(x + 3.0 + r * a.cos(), ctr.y + r * a.sin())
                    });
                    p.add(Shape::line(arc.collect(), stroke));
                }
            } else {
                let (a, b) = (Pos2::new(x + 6.0, ctr.y - 3.5), Pos2::new(x + 12.0, ctr.y + 3.5));
                p.line_segment([a, b], stroke);
                p.line_segment([Pos2::new(a.x, b.y), Pos2::new(b.x, a.y)], stroke);
            }
        }
    }
    resp.on_hover_text(tip).clicked()
}

/// Forward playback from frame `f0`, `secs` later: the frame and how far
/// into it, wrapped when `looping` (as [`Transport::tick`] counts frames).
fn forward_position(f0: i64, secs: f64, project: &Project, looping: bool) -> f64 {
    let (first, last) = (project.first_frame as f64, project.last_frame.max(project.first_frame) as f64);
    let f = f0 as f64 + secs * project.fps.max(0.1);
    match f >= last + 1.0 {
        true if looping => first + (f - first).rem_euclid(last + 1.0 - first),
        true => last,
        false => f,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forward_position_is_the_frame_and_a_fraction_wrapping_as_the_playhead_does() {
        let project = Project { first_frame: 1, last_frame: 10, fps: 24.0, ..Project::default() };
        let at = |f0, secs| forward_position(f0, secs, &project, true);
        assert!((at(1, 0.5 / 24.0) - 1.5).abs() < 1e-9);
        assert!((at(9, 1.25 / 24.0) - 10.25).abs() < 1e-9);
        // Past the out frame: back at the in, as tick() puts the playhead.
        assert!((at(10, 1.5 / 24.0) - 1.5).abs() < 1e-9);
        assert!((forward_position(10, 1.5 / 24.0, &project, false) - 10.0).abs() < 1e-9, "not looping: held at the end");
        let mut t = Transport::default();
        assert_eq!(t.play_position(&project), None, "stopped");
        t.set_play(Play::Backward);
        assert_eq!(t.play_position(&project), None, "backward: silent");
        t.set_play(Play::Forward);
        assert!(t.play_position(&project).is_some_and(|p| (1.0..1.5).contains(&p)));
    }

    #[test]
    fn indent_centers_or_gives_up() {
        assert_eq!(indent(1000.0, 600.0), 200.0);
        assert_eq!(indent(600.0, 600.0), 0.0);
        assert_eq!(indent(400.0, 600.0), 0.0, "too wide: from the left");
    }

    /// The control row, drawn in a window `width` wide for a few frames,
    /// and the panel it's in.
    fn row_in(width: f32, t: &mut Transport, project: &mut Project) -> (Rect, Rect) {
        let ctx = egui::Context::default();
        let mut panel = Rect::NOTHING;
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 300.0))),
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| {
                panel = ui.max_rect();
                t.ui(ui, project, &[], &[], &[]);
            }).drop_without_applying_deltas();
        }
        (t.row, panel)
    }

    #[test]
    fn the_control_row_is_centered_and_wraps_when_too_wide() {
        let (mut t, mut project) = (Transport::default(), Project::default());
        let mut widths = vec![];
        for panel in [1400.0, 1000.0, 2200.0] {
            let (row, area) = row_in(panel, &mut t, &mut project);
            let (left, right) = (row.left() - area.left(), area.right() - row.right());
            assert!((left - right).abs() < 2.0 && left > 50.0, "panel {panel}: {row:?} in {area:?}");
            assert!(row.height() < 40.0, "one line");
            widths.push(row.width());
        }
        assert!(widths.iter().all(|w| (w - widths[0]).abs() < 0.5));
        // Narrower than the row: starts at the left and wraps.
        let (row, area) = row_in(widths[0] - 200.0, &mut t, &mut project);
        assert!(row.height() > 40.0 && row.left() == area.left() && row.right() <= area.right(), "{row:?} in {area:?}");
        // And wide again: centered.
        let (row, area) = row_in(1400.0, &mut t, &mut project);
        assert!(row.height() < 40.0 && ((row.left() - area.left()) - (area.right() - row.right())).abs() < 2.0, "{row:?}");
    }
}
