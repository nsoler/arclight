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
    /// Wall-clock time and frame at which playback (re)started.
    anchor: Option<(Instant, i64)>,
    /// Smoothed frames actually displayed per second while playing.
    pub shown_fps: f32,
    last_shown: Option<Instant>,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            frame: 1,
            play: Play::Stopped,
            looping: true,
            anchor: None,
            shown_fps: 0.0,
            last_shown: None,
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
        ui.horizontal(|ui| {
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
            ui.separator();
            ui.label("Format").on_hover_text("The frame size of Constants, Checkerboards and shapes whose own width/height is 0 (the default)");
            project_changed |= ui.add(egui::DragValue::new(&mut project.format.width).range(1..=16384)).changed();
            ui.label("×");
            project_changed |= ui.add(egui::DragValue::new(&mut project.format.height).range(1..=16384)).changed();
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
    }
    resp.on_hover_text(tip).clicked()
}
