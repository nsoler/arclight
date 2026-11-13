//! Thumbnails over Read and Media In nodes in the node graph: where they go
//! and how they're drawn (the pictures come from `thumbnails`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, TextureId, Vec2};

/// Below this zoom a thumbnail is too small to read, so it isn't drawn.
pub const MIN_ZOOM: f32 = 0.4;
/// The tallest a thumbnail gets, as a fraction of its width.
const MAX_ASPECT: f32 = 0.9;
/// The gap between a thumbnail and its node, in graph units.
const GAP: f32 = 4.0;

/// Whether a node of this op shows a thumbnail.
pub fn has_thumbnail(op: &str) -> bool {
    matches!(op, "read" | "media_in")
}

/// Where the thumbnail of an image `size` pixels big goes over the node at
/// screen rect `node`: as wide as the node, its aspect kept, no taller than
/// 0.9 × its width (narrower then), centered and just above the node.
pub fn thumb_rect(node: Rect, zoom: f32, size: Vec2) -> Rect {
    let mut w = node.width();
    let aspect = if size.x > 0.0 && size.y > 0.0 { size.y / size.x } else { MAX_ASPECT };
    let mut h = w * aspect;
    if h > w * MAX_ASPECT {
        h = w * MAX_ASPECT;
        w = h / aspect;
    }
    let bottom = node.top() - GAP * zoom;
    Rect::from_min_max(Pos2::new(node.center().x - w * 0.5, bottom - h), Pos2::new(node.center().x + w * 0.5, bottom))
}

/// Whether a thumbnail at `thumb` is worth drawing: zoomed in enough, and
/// at least partly inside `clip` (the editor).
pub fn visible(zoom: f32, thumb: Rect, clip: Rect) -> bool {
    zoom >= MIN_ZOOM && thumb.intersects(clip)
}

/// Draw it, dimmed when the node is off, in a thin dark frame.
pub fn draw(painter: &egui::Painter, tex: TextureId, rect: Rect, off: bool) {
    let tint = if off { Color32::from_gray(90) } else { Color32::WHITE };
    painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(15)), egui::StrokeKind::Outside);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(zoom: f32) -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 200.0), Vec2::new(120.0, 40.0) * zoom)
    }

    #[test]
    fn it_sits_above_the_node_centered_and_scales_with_zoom() {
        let hd = Vec2::new(1920.0, 1080.0);
        let (n, t) = (node(1.0), thumb_rect(node(1.0), 1.0, hd));
        assert!(t.bottom() < n.top(), "above the node");
        assert!((t.center().x - n.center().x).abs() < 1e-3, "centered");
        assert!((t.width() - n.width()).abs() < 1e-3, "as wide as the node");
        assert!((t.height() / t.width() - 1080.0 / 1920.0).abs() < 1e-4, "aspect kept");
        let (n2, t2) = (node(2.0), thumb_rect(node(2.0), 2.0, hd));
        assert!((t2.size() - t.size() * 2.0).length() < 1e-3, "twice the zoom, twice the size");
        assert!(((n2.top() - t2.bottom()) - 2.0 * (n.top() - t.bottom())).abs() < 1e-3, "and twice the gap");
    }

    #[test]
    fn tall_pictures_are_capped_and_keep_their_aspect() {
        let n = node(1.0);
        let t = thumb_rect(n, 1.0, Vec2::new(1080.0, 1920.0));
        assert!((t.height() - 0.9 * n.width()).abs() < 1e-3, "height capped");
        assert!((t.height() / t.width() - 1920.0 / 1080.0).abs() < 1e-3, "narrower, same aspect");
        assert!(t.bottom() < n.top() && (t.center().x - n.center().x).abs() < 1e-3);
        let empty = thumb_rect(n, 1.0, Vec2::ZERO);
        assert!(empty.is_finite() && empty.bottom() < n.top(), "a zero size doesn't break it");
    }

    #[test]
    fn hidden_when_zoomed_out_or_offscreen() {
        let clip = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let t = thumb_rect(node(1.0), 1.0, Vec2::new(16.0, 9.0));
        assert!(visible(1.0, t, clip));
        assert!(visible(MIN_ZOOM, t, clip));
        assert!(!visible(0.39, t, clip), "too small");
        assert!(!visible(1.0, t.translate(Vec2::new(2000.0, 0.0)), clip), "offscreen");
        assert!(has_thumbnail("read") && has_thumbnail("media_in") && !has_thumbnail("grade"));
    }
}
