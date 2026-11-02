//! How good a Camera Tracker's solve is, at a glance: the overall error,
//! large and graded green / amber / red, with the worst frame and the
//! point count; a strip chart of each frame's error (click a bar to go to
//! that frame); and the same grading for colouring points by their error.

use crate::doc::Doc;
use arclight::camtrack::Solution;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

/// How usable a reprojection error (pixels) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    /// Below 0.5 px.
    Good,
    /// 0.5 px up to 1 px.
    Usable,
    /// 1 px and over.
    High,
}

/// The grade of an error of `px` pixels.
pub fn grade(px: f64) -> Grade {
    if px < 0.5 {
        Grade::Good
    } else if px < 1.0 {
        Grade::Usable
    } else {
        Grade::High
    }
}

impl Grade {
    pub fn color(self) -> Color32 {
        match self {
            Grade::Good => Color32::from_rgb(90, 205, 105),
            Grade::Usable => Color32::from_rgb(240, 180, 50),
            Grade::High => Color32::from_rgb(235, 80, 65),
        }
    }

    /// What it means, in a line.
    pub fn meaning(self) -> &'static str {
        match self {
            Grade::Good => "good",
            Grade::Usable => "usable",
            Grade::High => "high: delete bad points or mask movers",
        }
    }
}

/// The colour of an error of `px` pixels.
pub fn error_color(px: f64) -> Color32 {
    grade(px).color()
}

/// Point `i`'s colour by its error, or None when the solve has no
/// per-point errors (one saved before they were kept): draw it as usual.
pub fn point_color(sol: &Solution, i: usize) -> Option<Color32> {
    sol.point_rms.get(i).filter(|e| e.is_finite() && **e >= 0.0).map(|e| error_color(*e))
}

/// The point (index, screen position, error) nearest `hover` among `pts`
/// (index, screen position), within 7 px, when it has an error.
pub fn hovered_error(sol: &Solution, pts: &[(usize, Pos2)], hover: Option<Pos2>) -> Option<(usize, Pos2, f64)> {
    let h = hover?;
    let (i, p) = pts.iter().filter(|(_, p)| p.distance(h) < 7.0).min_by(|a, b| a.1.distance(h).total_cmp(&b.1.distance(h)))?;
    let e = sol.point_rms.get(*i).copied().filter(|e| e.is_finite() && *e >= 0.0)?;
    Some((*i, *p, e))
}

/// How to set the floor once solved.
pub const HOW_TO_FLOOR: &str =
    "Set the floor: select 3+ floor points in the viewer → Ground plane, then Set origin / Set scale; Fit plane adds a floor surface.";

/// The worst solved frame and its error.
fn worst_frame(sol: &Solution) -> Option<(i64, f64)> {
    sol.frames.iter().zip(&sol.frame_rms).map(|(f, e)| (*f, *e)).max_by(|a, b| a.1.total_cmp(&b.1))
}

/// The frame under `x` on a graph spanning `rect` with one bar per frame
/// from `lo` to `hi` (clamped to them).
pub fn frame_at(x: f32, rect: Rect, lo: i64, hi: i64) -> i64 {
    let n = (hi - lo + 1).max(1);
    let u = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    lo + ((u * n as f32).floor() as i64).min(n - 1)
}

/// The top of a Camera Tracker's Properties: the readout and the graph
/// when it has a solve. A frame to go to when a bar was clicked.
pub fn top(ui: &mut egui::Ui, doc: &Doc, id: &str, frame: i64) -> Option<i64> {
    let sol = doc.node(id).and_then(|n| Solution::from_params(&n.params))?;
    readout(ui, &sol);
    let seek = error_graph(ui, &sol, frame);
    ui.add_space(6.0);
    seek
}

/// The overall error, large and graded, its meaning, the worst frame and
/// the point count.
pub fn readout(ui: &mut egui::Ui, sol: &Solution) {
    let g = grade(sol.rms);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{:.2} px", sol.rms)).size(26.0).strong().color(g.color()))
            .on_hover_text("The solve's overall reprojection error (RMS): how far, on average, the solved points land from where they were tracked");
        ui.vertical(|ui| {
            ui.colored_label(g.color(), g.meaning());
            let worst = worst_frame(sol).map_or(String::new(), |(f, e)| format!("worst frame {f} ({e:.2} px) · "));
            ui.weak(format!("{worst}{} points", sol.points.len()));
        });
    });
}

/// Each solved frame's error as a bar (gaps where a frame wasn't solved),
/// the 1 px line dashed, the current frame marked. Hover for the value;
/// a click gives the frame to go to.
pub fn error_graph(ui: &mut egui::Ui, sol: &Solution, current: i64) -> Option<i64> {
    let (Some(&lo), Some(&hi)) = (sol.frames.first(), sol.frames.last()) else { return None };
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(60.0), 48.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_black_alpha(60));
    let worst = sol.frame_rms.iter().copied().fold(0.0, f64::max);
    let scale = worst.max(1.5) as f32;
    let y = |e: f64| rect.bottom() - rect.height() * ((e as f32) / scale).clamp(0.0, 1.0);
    let n = (hi - lo + 1) as f32;
    let bar = rect.width() / n;
    let x0 = |f: i64| rect.left() + (f - lo) as f32 * bar;
    if (lo..=hi).contains(&current) {
        painter.rect_filled(Rect::from_x_y_ranges(x0(current)..=x0(current) + bar.max(1.0), rect.y_range()), 0.0, Color32::from_white_alpha(30));
    }
    for (f, e) in sol.frames.iter().zip(&sol.frame_rms) {
        let r = Rect::from_x_y_ranges(x0(*f) + if bar > 3.0 { 0.5 } else { 0.0 }..=x0(*f) + bar.max(1.0), y(*e)..=rect.bottom());
        painter.rect_filled(r, 0.0, error_color(*e));
    }
    let one = y(1.0);
    painter.extend(egui::Shape::dashed_line(&[Pos2::new(rect.left(), one), Pos2::new(rect.right(), one)], Stroke::new(1.0, Color32::from_white_alpha(170)), 4.0, 3.0));
    painter.text(Pos2::new(rect.right() - 2.0, one - 1.0), egui::Align2::RIGHT_BOTTOM, "1 px", egui::FontId::proportional(10.0), Color32::from_white_alpha(170));
    let at = resp.hover_pos().map(|p| frame_at(p.x, rect, lo, hi));
    let clicked = resp.clicked();
    if let Some(f) = at {
        let text = match sol.frames.iter().position(|&g| g == f) {
            Some(i) => format!("frame {f}: {:.2} px", sol.frame_rms.get(i).copied().unwrap_or(0.0)),
            None => format!("frame {f}: not solved"),
        };
        resp.on_hover_text_at_pointer(text);
    }
    at.filter(|_| clicked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solve(point_rms: Vec<f64>) -> Solution {
        // From JSON, so fields added later with defaults don't matter here.
        serde_json::from_value(serde_json::json!({
            "format": [200, 100], "focal": 35.0, "film_back": [36.0, 24.0], "frames": [1, 2, 4], "cameras": [[0, 0, 0, 0, 0, 0], [0, 0, 0, 0, 0, 0], [0, 0, 0, 0, 0, 0]],
            "points": [[0.0, 0.0, -5.0], [0.0, 0.0, -5.0], [0.0, 0.0, -5.0]], "rms": 0.6, "frame_rms": [0.3, 1.4, 0.7], "point_rms": point_rms,
        }))
        .unwrap()
    }

    #[test]
    fn errors_are_graded_green_amber_red() {
        assert_eq!(grade(0.0), Grade::Good);
        assert_eq!(grade(0.49), Grade::Good);
        assert_eq!(grade(0.5), Grade::Usable);
        assert_eq!(grade(0.99), Grade::Usable);
        assert_eq!(grade(1.0), Grade::High);
        assert_eq!(grade(7.0), Grade::High);
        let (g, a, r) = (Grade::Good.color(), Grade::Usable.color(), Grade::High.color());
        assert!(g.g() > g.r() && a.r() > a.b() && a.g() > a.b() && r.r() > r.g());
        assert!(Grade::High.meaning().contains("delete bad points"));
    }

    #[test]
    fn a_click_on_the_graph_is_the_frame_under_it() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(40.0, 48.0));
        // Frames 1..=4: a bar 10 px wide each.
        assert_eq!(frame_at(10.0, rect, 1, 4), 1);
        assert_eq!(frame_at(19.9, rect, 1, 4), 1);
        assert_eq!(frame_at(20.0, rect, 1, 4), 2);
        assert_eq!(frame_at(45.0, rect, 1, 4), 4);
        assert_eq!(frame_at(50.0, rect, 1, 4), 4, "the right edge is the last frame");
        assert_eq!((frame_at(-100.0, rect, 1, 4), frame_at(500.0, rect, 1, 4)), (1, 4), "clamped");
        assert_eq!(frame_at(30.0, rect, 7, 7), 7, "one frame");
    }

    #[test]
    fn points_without_errors_are_drawn_as_usual() {
        let old = solve(Vec::new());
        assert!((0..3).all(|i| point_color(&old, i).is_none()));
        let new = solve(vec![0.2, 0.7, 2.0]);
        assert_eq!(point_color(&new, 0), Some(Grade::Good.color()));
        assert_eq!(point_color(&new, 1), Some(Grade::Usable.color()));
        assert_eq!(point_color(&new, 2), Some(Grade::High.color()));
        assert_eq!(point_color(&new, 3), None);
        assert_eq!(worst_frame(&new), Some((2, 1.4)));
        let pts = [(0, Pos2::new(10.0, 10.0)), (2, Pos2::new(30.0, 10.0))];
        assert_eq!(hovered_error(&new, &pts, Some(Pos2::new(28.0, 11.0))).map(|h| h.0), Some(2));
        assert_eq!(hovered_error(&new, &pts, Some(Pos2::new(60.0, 60.0))), None, "nothing near");
        assert_eq!(hovered_error(&old, &pts, Some(Pos2::new(10.0, 10.0))), None, "no error to show");
    }
}
