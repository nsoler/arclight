//! Picking single points of point sets and mesh vertices in the 3D view
//! (the toolbar's Points toggle), for Geo Info's Selected only.
//!
//! Hit-testing is on the CPU in screen space, only when a click or a box
//! ends: every candidate is projected with the view's own projector, so
//! picks land where the points are drawn. A click takes the nearest within
//! the pick radius, skipping mesh vertices behind the surface under the
//! pointer; a box takes those inside it, mesh vertices only when they face
//! the camera (vertices hidden behind other geometry can still be boxed).
//! Picks are UI state: indices into the scene's point sets / meshes, with
//! the node they came from, dropped when the scene no longer matches.

use crate::viewport3d::Projector;
use arclight::geo_table::Domain;
use arclight::geom::{self, Vec3};
use arclight::scene::Scene;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke};

/// How near (screen points) a click must be to a point.
pub const RADIUS: f32 = 7.0;
const PICKED: Color32 = Color32::from_rgb(255, 170, 40);

/// One picked element: a point of point set `object`, or a vertex of mesh
/// `object` (indices into `Scene::point_sets` / `Scene::meshes`).
#[derive(Clone, Debug, PartialEq)]
pub struct ElementPick {
    pub domain: Domain,
    pub node: String,
    pub object: usize,
    pub index: usize,
}

/// Every candidate element, placed in the scene: (domain, object, index,
/// position, normal if a mesh vertex has one).
fn candidates(scene: &Scene) -> impl Iterator<Item = (Domain, usize, usize, Vec3, Option<Vec3>)> + '_ {
    let points = scene.point_sets.iter().enumerate().flat_map(|(k, g)| (0..g.positions.len()).map(move |i| (Domain::Points, k, i, g.placed(i), None)));
    let verts = scene.meshes.iter().enumerate().flat_map(|(k, m)| {
        let turn = m.world.fixed_view::<3, 3>(0, 0).into_owned();
        (0..m.mesh.positions.len()).map(move |i| (Domain::MeshPoints, k, i, geom::transform_point(&m.world, &m.mesh.positions[i]), m.mesh.normals.get(i).map(|n| turn * n)))
    });
    points.chain(verts)
}

fn node_of(scene: &Scene, domain: Domain, object: usize) -> String {
    match domain {
        Domain::Points => scene.point_sets.get(object).map(|g| g.node.clone()),
        _ => scene.meshes.get(object).map(|m| m.node.clone()),
    }
    .unwrap_or_default()
}

/// The element a click at `at` picks: the nearest within [`RADIUS`] (ties:
/// the nearer the camera); a mesh vertex only if it isn't behind the
/// surface under the pointer (`surface`: how far along the pointer's ray
/// a mesh is hit, as the view's ray test gives).
pub fn pick(scene: &Scene, p: &Projector, at: Pos2, surface: &dyn Fn(usize, &Vec3, &Vec3) -> Option<f64>) -> Option<ElementPick> {
    let (o, d) = p.ray(at);
    // The nearest surface along the ray, over every mesh.
    let wall = (0..scene.meshes.len()).filter_map(|k| surface(k, &o, &d)).fold(f64::INFINITY, f64::min);
    let best = candidates(scene)
        .filter_map(|(domain, k, i, at3, _)| {
            let (q, depth) = p.project(at3)?;
            let dist = q.distance(at);
            if dist > RADIUS {
                return None;
            }
            // Behind the nearest surface (with a little slack for the vertex
            // on it): hidden.
            let along = (at3 - o).dot(&d);
            if domain == Domain::MeshPoints && along > wall * (1.0 + 1e-3) + 1e-6 {
                return None;
            }
            Some((dist, depth, domain, k, i))
        })
        .min_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal))?;
    let (_, _, domain, object, index) = best;
    Some(ElementPick { domain, node: node_of(scene, domain, object), object, index })
}

/// The elements inside the screen box `a`–`b`: point-set points, and mesh
/// vertices facing the camera (or without normals).
pub fn in_box(scene: &Scene, p: &Projector, a: Pos2, b: Pos2) -> Vec<ElementPick> {
    let r = Rect::from_two_pos(a, b);
    let (eye, look) = p.ray(r.center());
    candidates(scene)
        .filter(|(domain, _, _, at3, n)| {
            let Some((q, _)) = p.project(*at3) else { return false };
            if !r.contains(q) {
                return false;
            }
            match (domain, n) {
                (Domain::MeshPoints, Some(n)) => {
                    let toward = if p.ortho { -look } else { eye - at3 };
                    n.dot(&toward) >= 0.0
                }
                _ => true,
            }
        })
        .map(|(domain, k, i, _, _)| ElementPick { domain, node: node_of(scene, domain, k), object: k, index: i })
        .collect()
}

/// A click's result applied to `picks`: replace, or with `add` toggle; a
/// click on nothing clears (unless adding).
pub fn apply(picks: &mut Vec<ElementPick>, hit: Option<ElementPick>, add: bool) {
    match (hit, add) {
        (Some(h), false) => *picks = vec![h],
        (Some(h), true) => match picks.iter().position(|p| *p == h) {
            Some(k) => {
                picks.remove(k);
            }
            None => picks.push(h),
        },
        (None, false) => picks.clear(),
        (None, true) => {}
    }
}

/// A box's elements added to `picks` (`add`), or in their place.
pub fn apply_box(picks: &mut Vec<ElementPick>, boxed: Vec<ElementPick>, add: bool) {
    if !add {
        picks.clear();
    }
    for b in boxed {
        if !picks.contains(&b) {
            picks.push(b);
        }
    }
}

/// Whether `pick` still names an element of `scene` (same node, in range).
pub fn valid(scene: &Scene, pick: &ElementPick) -> bool {
    match pick.domain {
        Domain::Points => scene.point_sets.get(pick.object).is_some_and(|g| g.node == pick.node && pick.index < g.positions.len()),
        Domain::MeshPoints => scene.meshes.get(pick.object).is_some_and(|m| m.node == pick.node && pick.index < m.mesh.positions.len()),
        _ => false,
    }
}

/// The picked elements as dots over the view.
pub fn draw(painter: &egui::Painter, scene: &Scene, p: &Projector, picks: &[ElementPick]) {
    for pick in picks.iter().filter(|q| valid(scene, q)) {
        let at3 = match pick.domain {
            Domain::Points => scene.point_sets[pick.object].placed(pick.index),
            _ => {
                let m = &scene.meshes[pick.object];
                geom::transform_point(&m.world, &m.mesh.positions[pick.index])
            }
        };
        if let Some((q, _)) = p.project(at3) {
            painter.circle(q, 3.5, PICKED, Stroke::new(1.0, Color32::BLACK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport3d::ViewCam;
    use arclight::geo::PointSet;
    use arclight::scene::Model;
    use std::sync::Arc;

    fn view() -> (Projector, Rect) {
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        (ViewCam::default().projector(rect), rect)
    }

    fn scene_with(points: Vec<Vec3>) -> Scene {
        let mut s = Scene::default();
        s.point_sets.push(PointSet { positions: points, node: "pts".into(), ..PointSet::default() });
        s
    }

    fn no_surface(_: usize, _: &Vec3, _: &Vec3) -> Option<f64> {
        None
    }

    #[test]
    fn a_click_picks_the_nearest_point_within_reach() {
        let (p, _) = view();
        let s = scene_with(vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 0.0)]);
        let at = p.project(Vec3::new(1.0, 0.0, 0.0)).unwrap().0;
        let hit = pick(&s, &p, at + egui::vec2(2.0, 1.0), &no_surface).unwrap();
        assert_eq!((hit.domain, hit.node.as_str(), hit.object, hit.index), (Domain::Points, "pts", 0, 1));
        assert!(pick(&s, &p, at + egui::vec2(40.0, 40.0), &no_surface).is_none(), "out of reach");
    }

    #[test]
    fn mesh_vertices_behind_the_surface_are_skipped() {
        let (p, _) = view();
        let mut s = Scene::default();
        s.meshes.push(Model::new(Arc::new(arclight::mesh::cube([2.0; 3], [1; 3])), geom::Mat4::identity(), Default::default()));
        s.meshes[0].node = "box".into();
        // The vertex nearest the camera is visible; aim at it.
        let (o, _) = p.ray(Pos2::new(400.0, 300.0));
        let front = (0..s.meshes[0].mesh.positions.len()).min_by(|&a, &b| (s.meshes[0].mesh.positions[a] - o).norm().total_cmp(&(s.meshes[0].mesh.positions[b] - o).norm())).unwrap();
        let at = p.project(s.meshes[0].mesh.positions[front]).unwrap().0;
        // A wall just in front of everything hides every vertex…
        let hit = pick(&s, &p, at, &|_, _, _| Some(0.5));
        assert!(hit.is_none());
        // …the surface at the vertex itself doesn't.
        let at3 = s.meshes[0].mesh.positions[front];
        let hit = pick(&s, &p, at, &|_, o, d| Some((at3 - o).dot(d))).unwrap();
        assert_eq!((hit.domain, hit.index, hit.node.as_str()), (Domain::MeshPoints, front, "box"));
    }

    #[test]
    fn boxes_take_what_is_inside_and_facing() {
        let (p, rect) = view();
        let mut s = scene_with(vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(100.0, 0.0, 0.0)]);
        s.meshes.push(Model::new(Arc::new(arclight::mesh::cube([2.0; 3], [1; 3])), geom::Mat4::identity(), Default::default()));
        let all = in_box(&s, &p, rect.min, rect.max);
        assert!(all.iter().any(|e| e.domain == Domain::Points && e.index == 0));
        assert!(!all.iter().any(|e| e.domain == Domain::Points && e.index == 1), "off screen");
        let verts = all.iter().filter(|e| e.domain == Domain::MeshPoints).count();
        let total = s.meshes[0].mesh.positions.len();
        assert!(verts > 0 && verts < total, "{verts} of {total}: back-facing ones left out");
    }

    #[test]
    fn clicks_and_boxes_replace_toggle_and_add() {
        let e = |i| ElementPick { domain: Domain::Points, node: "n".into(), object: 0, index: i };
        let mut picks = Vec::new();
        apply(&mut picks, Some(e(1)), false);
        apply(&mut picks, Some(e(2)), true);
        apply(&mut picks, Some(e(1)), true);
        assert_eq!(picks, vec![e(2)]);
        apply(&mut picks, None, true);
        assert_eq!(picks.len(), 1, "a shift-click on nothing keeps them");
        apply(&mut picks, None, false);
        assert!(picks.is_empty());
        apply_box(&mut picks, vec![e(3), e(4)], false);
        apply_box(&mut picks, vec![e(4), e(5)], true);
        assert_eq!(picks, vec![e(3), e(4), e(5)]);
    }

    #[test]
    fn picks_from_another_scene_are_dropped() {
        let s = scene_with(vec![Vec3::zeros(); 3]);
        let ok = ElementPick { domain: Domain::Points, node: "pts".into(), object: 0, index: 2 };
        let gone = [ElementPick { index: 3, ..ok.clone() }, ElementPick { node: "other".into(), ..ok.clone() }, ElementPick { domain: Domain::MeshPoints, ..ok.clone() }];
        assert!(valid(&s, &ok) && gone.iter().all(|g| !valid(&s, g)));
    }

    /// A sanity bound, not a benchmark: 200k points picked by a click and
    /// by a box well within a frame's worth of slack even on a loaded
    /// machine (typically a few ms; the bound is 2 s).
    #[test]
    fn large_point_sets_pick_quickly() {
        let (p, rect) = view();
        let pts: Vec<Vec3> = (0..200_000).map(|i| Vec3::new((i % 500) as f64 * 0.01 - 2.5, (i / 500) as f64 * 0.01 - 2.0, 0.0)).collect();
        let s = scene_with(pts);
        let t = std::time::Instant::now();
        let hit = pick(&s, &p, rect.center(), &no_surface);
        let boxed = in_box(&s, &p, rect.center() - egui::vec2(50.0, 50.0), rect.center() + egui::vec2(50.0, 50.0));
        assert!(hit.is_some() && !boxed.is_empty());
        assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    }
}
