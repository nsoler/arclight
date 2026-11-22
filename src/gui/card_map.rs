//! Where a Scanline Render shows a Card's image: a projective map from
//! the card's input pixels to the render's, for overlay handles.
//!
//! A card's texture spans its image's frame (uv = pixel / format), its
//! corners at (−w, h), (w, h), (w, −h), (−w, −h) in card space for uv
//! (0, 0) … (0, 1); the render projects through its camera into its frame
//! (`Camera::project_camera`). Composed, image pixel → render pixel is one
//! 3×3 homography, built here so that its third row gives the point's
//! depth in front of the camera (1 for an orthographic camera, whose
//! depth is checked at the card's corners instead).

use arclight::geom::{Camera, Vec3};
use arclight::image::Size;
use arclight::scene::{Card, Scene};
use arclight::track;

/// A projective map of frame pixels (pixel i spanning i..i+1): row-major
/// 3×3, the third row the depth of a point in front of the camera; points
/// nearer than `near` aren't shown (−∞ for maps that aren't projective).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Proj {
    pub m: [f64; 9],
    pub near: f64,
}

impl Proj {
    pub const IDENTITY: Proj = Proj { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], near: f64::NEG_INFINITY };

    pub fn affine(a: &track::Affine) -> Proj {
        let a = a.map(f64::from);
        Proj { m: [a[0], a[1], a[2], a[3], a[4], a[5], 0.0, 0.0, 1.0], near: f64::NEG_INFINITY }
    }

    #[cfg(test)]
    /// Whether it's affine (no perspective).
    pub fn is_affine(&self) -> bool {
        self.m[6] == 0.0 && self.m[7] == 0.0 && self.m[8] == 1.0
    }

    #[cfg(test)]
    /// The affine part, when it is affine.
    pub fn as_affine(&self) -> Option<track::Affine> {
        self.is_affine().then(|| [self.m[0], self.m[1], self.m[2], self.m[3], self.m[4], self.m[5]].map(|v| v as f32))
    }

    /// This map, then `after` (`after ∘ self`).
    pub fn then(&self, after: &Proj) -> Proj {
        let (a, b) = (&after.m, &self.m);
        let mut m = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
            }
        }
        Proj { m, near: self.near.max(after.near) }
    }

    /// Where `p` goes; None behind the camera (nearer than `near`) or at
    /// the horizon.
    pub fn apply(&self, [x, y]: [f64; 2]) -> Option<[f64; 2]> {
        let m = &self.m;
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < 1e-12 || (self.near.is_finite() && w < self.near.max(1e-9)) {
            return None;
        }
        Some([(m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w])
    }

    /// The point that goes to `q` (in front of the camera), if any.
    pub fn unapply(&self, q: [f64; 2]) -> Option<[f64; 2]> {
        let p = self.inverse()?.apply_plain(q)?;
        self.apply(p).map(|_| p)
    }

    fn apply_plain(&self, [x, y]: [f64; 2]) -> Option<[f64; 2]> {
        let m = &self.m;
        let w = m[6] * x + m[7] * y + m[8];
        (w.abs() >= 1e-12).then(|| [(m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w])
    }

    fn inverse(&self) -> Option<Proj> {
        let m = &self.m;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        (det.abs() > 1e-300).then(|| Proj { m: c.map(|v| v / det), near: f64::NEG_INFINITY })
    }
}

/// How the chosen card of a node shows in a render: its map (card input
/// pixels → render pixels), its on-screen area (render pixels²) and how
/// many cards of that node there were to choose from.
#[derive(Clone, Debug, PartialEq)]
pub struct CardView {
    pub proj: Proj,
    pub area: f64,
    pub among: usize,
}

/// Card `card`'s map into a render through `cam` in a frame of `format`,
/// or why there's none: behind the camera, or edge-on.
pub fn card_proj(card: &Card, cam: &Camera, format: Size) -> Result<(Proj, f64), String> {
    let (fw, fh) = (card.image.format.width.max(1) as f64, card.image.format.height.max(1) as f64);
    let [w, h] = card.size.map(|v| v / 2.0);
    let to_cam = cam.view() * card.world;
    // Card space of image pixel (x, y): (2w/fw · x − w, h − 2h/fh · y, 0, 1);
    // in camera space, rows of a 3×3 on (x, y, 1).
    let col = |c: usize| Vec3::new(to_cam[(0, c)], to_cam[(1, c)], to_cam[(2, c)]);
    let (cx, cy, t) = (col(0), col(1), Vec3::new(to_cam[(0, 3)], to_cam[(1, 3)], to_cam[(2, 3)]));
    let ax = cx * (2.0 * w / fw);
    let ay = cy * (-2.0 * h / fh);
    let a0 = t - cx * w + cy * h;
    // Camera point = ax·x + ay·y + a0.
    let (ox, oy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
    let corners = [[0.0, 0.0], [fw, 0.0], [fw, fh], [0.0, fh]];
    let cam_at = |[x, y]: [f64; 2]| ax * x + ay * y + a0;
    if corners.iter().any(|&c| -cam_at(c).z < cam.near.max(1e-9)) {
        return Err(format!("{} reaches behind the camera", card.node));
    }
    let proj = if cam.is_ortho() {
        let s = cam.pixels_per_unit(1.0, format);
        Proj { m: [s * ax.x, s * ay.x, s * a0.x + ox, -s * ax.y, -s * ay.y, -s * a0.y + oy, 0.0, 0.0, 1.0], near: f64::NEG_INFINITY }
    } else {
        let f = cam.focal_pixels(format);
        // X = f·Cx + ox·(−Cz), Y = −f·Cy + oy·(−Cz), W = −Cz (the depth).
        let row = |k: Vec3| [k.x, k.y, k.z];
        let (px, py, pz) = (row(Vec3::new(ax.x, ay.x, a0.x)), row(Vec3::new(ax.y, ay.y, a0.y)), row(Vec3::new(ax.z, ay.z, a0.z)));
        let mut m = [0.0; 9];
        for c in 0..3 {
            m[c] = f * px[c] - ox * pz[c];
            m[3 + c] = -f * py[c] - oy * pz[c];
            m[6 + c] = -pz[c];
        }
        Proj { m, near: cam.near.max(1e-9) }
    };
    let quad: Vec<[f64; 2]> = corners.iter().filter_map(|&c| proj.apply(c)).collect();
    let area = on_screen_area(&quad, format);
    if polygon_area(&quad) < 1.0 {
        return Err(format!("{} is edge-on to the camera", card.node));
    }
    Ok((proj, area))
}

/// The card of node `node` in `scene` the handles go on: of those that can
/// be mapped, the one covering the most of the frame (ties: the first);
/// or why none can (the first's reason).
pub fn view_of(scene: &Scene, cam: &Camera, format: Size, node: &str) -> Result<CardView, String> {
    let cards: Vec<&Card> = scene.cards.iter().filter(|c| c.node == node).collect();
    if cards.is_empty() {
        return Err(format!("{node} isn't in the rendered scene"));
    }
    let mut best: Option<(Proj, f64)> = None;
    let mut why = None;
    for c in &cards {
        match card_proj(c, cam, format) {
            Ok((p, a)) if best.as_ref().is_none_or(|b| a > b.1) => best = Some((p, a)),
            Ok(_) => {}
            Err(e) => why = why.or(Some(e)),
        }
    }
    match best {
        Some((proj, area)) => Ok(CardView { proj, area, among: cards.len() }),
        None => Err(why.unwrap_or_default()),
    }
}

fn polygon_area(q: &[[f64; 2]]) -> f64 {
    if q.len() < 3 {
        return 0.0;
    }
    (0..q.len()).map(|i| {
        let (a, b) = (q[i], q[(i + 1) % q.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f64>().abs() / 2.0
}

/// The area of polygon `q` inside the frame (Sutherland–Hodgman).
fn on_screen_area(q: &[[f64; 2]], format: Size) -> f64 {
    let (w, h) = (format.width as f64, format.height as f64);
    let mut poly = q.to_vec();
    // How far inside each edge of the frame a point is (left, top, right,
    // bottom).
    let inside = |e: usize, p: [f64; 2]| [p[0], p[1], w - p[0], h - p[1]][e];
    for e in 0..4 {
        let mut out = Vec::new();
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (da, db) = (inside(e, a), inside(e, b));
            if da >= 0.0 {
                out.push(a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                let t = da / (da - db);
                out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        poly = out;
        if poly.is_empty() {
            return 0.0;
        }
    }
    polygon_area(&poly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arclight::graph::{Evaluator, Graph};
    use arclight::image::Image;
    use arclight::ops::{self, EvalContext};
    use serde_json::{json, Value};
    use std::sync::Arc;

    const CTX: EvalContext = EvalContext { frame: 1, write_outputs: false };

    /// A 400 × 300 image with a 12 px white square at (`x`, `y`), on a card
    /// turned `turn`° about y and moved, seen by a camera, rendered at
    /// 640 × 360.
    fn graph(x: f64, y: f64, turn: f64, cam: Value) -> Graph {
        serde_json::from_value(json!({ "nodes": [
            { "id": "sq", "op": "rectangle", "inputs": [""], "params": { "width": 400, "height": 300, "center": [x, y], "size": [12, 12] } },
            { "id": "card", "op": "card", "inputs": ["sq"], "params": { "rotate": [10, turn, 5], "translate": [0.3, -0.1, -0.4] } },
            { "id": "scene", "op": "scene", "inputs": ["card"] },
            { "id": "cam", "op": "camera", "params": cam },
            { "id": "render", "op": "scanline_render", "inputs": ["scene", "cam", ""], "params": { "width": 640, "height": 360, "samples": 4 } },
        ] }))
        .unwrap()
    }

    fn view(g: &Graph) -> (Arc<Scene>, Camera, Size) {
        let mut ev = Evaluator::new();
        let (scene, cam) = (ev.evaluate(g, "scene", &CTX).unwrap(), ev.evaluate(g, "cam", &CTX).unwrap());
        let params = ops::resolve_params("scanline_render", &g.node("render").unwrap().params, 1);
        let (s, c, f) = ops::render_view(&params, &[Some(&scene), Some(&cam), None]).unwrap();
        (s.unwrap(), c, f)
    }

    fn centroid(img: &Image) -> [f64; 2] {
        let w = img.window();
        let (mut sum, mut at) = (0.0, [0.0; 2]);
        for y in w.y0..w.y1 {
            for x in w.x0..w.x1 {
                let a = img.at(x, y)[3] as f64;
                sum += a;
                at[0] += a * (x as f64 + 0.5);
                at[1] += a * (y as f64 + 0.5);
            }
        }
        [at[0] / sum, at[1] / sum]
    }

    /// The map puts a point where the render draws it: a small square's
    /// centroid through a tilted, turned, moved card, perspective and
    /// orthographic, to a fraction of a pixel.
    #[test]
    fn the_map_lands_where_the_render_draws() {
        for (cam, turn) in [(json!({ "translate": [0, 0, 2] }), 35.0), (json!({ "translate": [0.2, 0.1, 1.5], "rotate": [5, -10, 0] }), -50.0), (json!({ "translate": [0, 0, 2], "projection": "orthographic", "ortho_width": 2.5 }), 30.0)] {
            for (x, y) in [(200.0, 150.0), (60.0, 40.0), (300.0, 200.0)] {
                let g = graph(x, y, turn, cam.clone());
                let (scene, c, format) = view(&g);
                let v = view_of(&scene, &c, format, "card").unwrap();
                assert_eq!(v.among, 1);
                let want = v.proj.apply([x, y]).unwrap();
                assert!(want[0] > 10.0 && want[0] < 630.0 && want[1] > 10.0 && want[1] < 350.0, "{cam} {turn} ({x}, {y}): off the frame at {want:?}");
                let got = centroid(&Evaluator::new().evaluate(&g, "render", &CTX).unwrap());
                assert!((got[0] - want[0]).hypot(got[1] - want[1]) < 0.35, "{cam} {turn} ({x}, {y}): rendered {got:?}, mapped {want:?}");
                // And back.
                let back = v.proj.unapply(want).unwrap();
                assert!((back[0] - x).abs() < 1e-6 && (back[1] - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_card_behind_the_camera_or_edge_on_has_no_map() {
        let behind = graph(200.0, 150.0, 0.0, json!({ "translate": [0, 0, -3], "rotate": [0, 0, 0] }));
        let (s, c, f) = view(&behind);
        assert_eq!(view_of(&s, &c, f, "card"), Err("card reaches behind the camera".into()));
        let mut edge = graph(200.0, 150.0, 0.0, json!({ "translate": [0, 0, 2] }));
        edge.nodes[1].params = json!({ "rotate": [0, 90, 0], "translate": [0, 0, 0] });
        let (s, c, f) = view(&edge);
        assert_eq!(view_of(&s, &c, f, "card"), Err("card is edge-on to the camera".into()));
        assert_eq!(view_of(&s, &c, f, "nope"), Err("nope isn't in the rendered scene".into()));
    }

    /// Of several cards of one node, the one covering the most of the frame.
    #[test]
    fn the_largest_card_on_screen_is_chosen() {
        let g = graph(200.0, 150.0, 0.0, json!({ "translate": [0, 0, 2] }));
        let (s, c, f) = view(&g);
        let mut two = (*s).clone();
        let mut far = two.cards[0].clone();
        far.world[(2, 3)] -= 3.0;
        two.cards.insert(0, far);
        let v = view_of(&two, &c, f, "card").unwrap();
        assert_eq!(v.among, 2);
        assert_eq!(v.proj, card_proj(&s.cards[0], &c, f).unwrap().0, "the near (bigger) one");
    }

    #[test]
    fn maps_compose_and_invert() {
        let p = Proj { m: [2.0, 0.1, 5.0, -0.2, 1.5, 3.0, 0.001, 0.002, 1.0], near: 0.5 };
        let a = Proj::affine(&[1.0, 0.0, 10.0, 0.0, 2.0, -4.0]);
        let both = p.then(&a);
        let q = [30.0, 40.0];
        let via = a.apply(p.apply(q).unwrap()).unwrap();
        let got = both.apply(q).unwrap();
        assert!((via[0] - got[0]).abs() < 1e-9 && (via[1] - got[1]).abs() < 1e-9);
        let back = both.unapply(got).unwrap();
        assert!((back[0] - q[0]).abs() < 1e-9 && (back[1] - q[1]).abs() < 1e-9);
        assert!(a.is_affine() && !p.is_affine() && a.as_affine() == Some([1.0, 0.0, 10.0, 0.0, 2.0, -4.0]));
        // Behind: w below near.
        assert!(p.apply([-2000.0, 0.0]).is_none());
    }
}
