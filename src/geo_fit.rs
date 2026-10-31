//! Fit Plane (docs/geo-plan.md, P5): a plane fitted to points, most often
//! a ground plane or a wall through a camera tracker's solved points, to
//! catch shadows, hold out, or place things on. Least squares (see
//! [`camtrack::fit_plane`]), sized to the points, facing the camera (else
//! up). `ops.rs` registers it; the plane itself is the Plane primitive's,
//! with all its surface params.

use crate::camtrack;
use crate::geom::{self, Vec3};
use crate::image::Image;
use crate::scene::{self, Scene};
use anyhow::{anyhow, bail, Result};
use nalgebra::{Matrix3, Rotation3};
use serde_json::{json, Value};

/// How the plane's sides are turned within it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    /// Its X as near world X as the plane allows (world Z for a plane
    /// facing along X).
    World,
    /// Its X along the points' main spread: the tightest rectangle.
    Spread,
}

/// Where the fitted plane goes: Plane's translate, rotate (degrees, as
/// [`geom::trs`] takes them) and size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub translate: [f64; 3],
    pub rotate: [f64; 3],
    pub size: [f64; 2],
}

/// The indices in `text` ("0 3 5", commas too), each checked against
/// `count`. Empty: none.
pub fn indices(text: &str, count: usize) -> Result<Vec<usize>> {
    text.split([' ', ',', '\t', '\n'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let i: usize = s.parse().map_err(|_| anyhow!("Points: '{s}' isn't a point number"))?;
            if i >= count {
                bail!("Points: there's no point {i} (there are {count})");
            }
            Ok(i)
        })
        .collect()
}

/// `indices` as Points text.
pub fn indices_text(indices: &[usize]) -> String {
    indices.iter().map(usize::to_string).collect::<Vec<_>>().join(" ")
}

/// The plane through `points` (least squares): its normal toward `eye` if
/// given, else the side facing up; sides by `align`; sized to the points'
/// extent and `margin` past it (a share of it), or `size` when given;
/// moved `offset` along the normal.
pub fn fit(points: &[Vec3], align: Align, margin: f64, size: Option<[f64; 2]>, offset: f64, eye: Option<Vec3>) -> Result<Placement> {
    let (center, mut n) = camtrack::fit_plane(points).ok_or_else(|| anyhow!("Fit Plane needs 3 or more points not in a line ({} given)", points.len()))?;
    let toward = eye.map_or(Vec3::y(), |e| e - center);
    if n.dot(&toward) < 0.0 {
        n = -n;
    }
    // In the plane: `u` its X, `v` its Y (so u × v = n).
    let flat = |d: Vec3| d - n * n.dot(&d);
    let u = match align {
        Align::World => [Vec3::x(), Vec3::z(), Vec3::y()].into_iter().map(flat).find(|d| d.norm() > 1e-6).unwrap_or_else(Vec3::x),
        Align::Spread => {
            let cov = points.iter().fold(Matrix3::zeros(), |m, p| {
                let d = flat(p - center);
                m + d * d.transpose()
            });
            let e = nalgebra::SymmetricEigen::new(cov);
            let k = (0..3).max_by(|&a, &b| e.eigenvalues[a].total_cmp(&e.eigenvalues[b])).unwrap_or(0);
            flat(e.eigenvectors.column(k).into_owned())
        }
    }
    .normalize();
    let v = n.cross(&u);
    // The points' extent along u and v.
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in points {
        let d = p - center;
        for (k, axis) in [u, v].iter().enumerate() {
            let t = d.dot(axis);
            lo[k] = lo[k].min(t);
            hi[k] = hi[k].max(t);
        }
    }
    let mid = u * (lo[0] + hi[0]) / 2.0 + v * (lo[1] + hi[1]) / 2.0;
    let size = size.unwrap_or_else(|| [0, 1].map(|k| ((hi[k] - lo[k]) * (1.0 + margin.max(0.0))).max(1e-6)));
    let rotate = geom::angles(&Rotation3::from_matrix_unchecked(Matrix3::from_columns(&[u, v, n])));
    Ok(Placement { translate: (center + mid + n * offset).into(), rotate, size })
}

/// The points Fit Plane fits in `scene`: its tracker points (the cloud),
/// else its point sets' (placed), else its meshes' vertices (placed); the
/// ones numbered in `points`, or all.
fn gather(scene: &Scene, points: &str) -> Result<Vec<Vec3>> {
    let all: Vec<Vec3> = if !scene.points.is_empty() {
        scene.points.clone()
    } else if scene.point_sets.iter().any(|s| !s.is_empty()) {
        scene.point_sets.iter().flat_map(|s| (0..s.len()).map(move |i| s.placed(i))).collect()
    } else {
        scene.meshes.iter().flat_map(|m| m.mesh.positions.iter().map(move |p| geom::transform_point(&m.world, p))).collect()
    };
    let picked = indices(points, all.len())?;
    Ok(if picked.is_empty() { all } else { picked.into_iter().map(|i| all[i]).collect() })
}

/// The Fit Plane node: a Plane (`params` its surface params too) fitted to
/// the points of the scene `input`, textured by `texture`; the plane
/// alone, or with Keep input the input scene and the plane.
pub fn fit_plane(params: &Value, input: Scene, texture: Option<&Image>) -> Result<Scene> {
    let points = gather(&input, params["points"].as_str().unwrap_or(""))?;
    let align = if params["align"].as_str() == Some("spread") { Align::Spread } else { Align::World };
    let fixed = (params["size_mode"].as_str() == Some("fixed")).then(|| scene::numbers::<2>(&params["size"], 1.0).map(f64::abs));
    let eye = input.cameras.first().map(|c| geom::transform_point(&c.world, &Vec3::zeros()));
    let num = |k: &str, d: f64| params[k].as_f64().filter(|v| v.is_finite()).unwrap_or(d);
    let at = fit(&points, align, num("margin", 0.2), fixed, num("offset", 0.0), eye)?;
    let mut p = params.clone();
    p["translate"] = json!(at.translate);
    p["rotate"] = json!(at.rotate);
    p["scale"] = json!([1.0, 1.0, 1.0]);
    p["size"] = json!(at.size);
    let model = scene::primitive_model("plane", &p, texture).ok_or_else(|| anyhow!("no plane"))?;
    let mut out = if params["keep_input"].as_bool() == Some(true) { input } else { Scene::default() };
    out.meshes.push(model);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::PointSet;

    fn near(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6)
    }

    /// Where the placement puts the plane's local `p` (its X, Y, normal).
    fn world(at: &Placement, p: Vec3) -> [f64; 3] {
        geom::transform_point(&geom::trs(at.translate, at.rotate, [1.0; 3]), &p).into()
    }

    fn normal(at: &Placement) -> [f64; 3] {
        let o = Vec3::from(world(at, Vec3::zeros()));
        (Vec3::from(world(at, Vec3::z())) - o).into()
    }

    /// A 4 × 2 patch of points on y = 2, from x 1..5, z -3..-1.
    fn ground() -> Vec<Vec3> {
        (0..15).map(|i| Vec3::new(1.0 + (i % 5) as f64, 2.0, -3.0 + (i / 5) as f64)).collect()
    }

    #[test]
    fn ground_points_give_a_level_plane_facing_up_sized_past_them() {
        let at = fit(&ground(), Align::World, 0.5, None, 0.0, None).unwrap();
        assert!(near(at.translate, [3.0, 2.0, -2.0]), "{at:?}");
        assert!(near(normal(&at), [0.0, 1.0, 0.0]), "up: {:?}", normal(&at));
        assert!(near(world(&at, Vec3::x()), [4.0, 2.0, -2.0]), "its X along world X");
        assert!((at.size[0] - 6.0).abs() < 1e-9 && (at.size[1] - 3.0).abs() < 1e-9, "4 × 2 and half again: {:?}", at.size);
        // Lopsided (more points at one end): centered on their extent, not
        // their average.
        let mut heavy = ground();
        heavy.extend((0..10).map(|k| Vec3::new(1.0 + 0.01 * k as f64, 2.0, -3.0)));
        let at = fit(&heavy, Align::World, 0.0, None, 0.0, None).unwrap();
        assert!(near(at.translate, [3.0, 2.0, -2.0]), "{at:?}");
        // Offset lifts it along the normal; a fixed size is kept.
        let lifted = fit(&ground(), Align::World, 0.5, Some([10.0, 10.0]), 0.25, None).unwrap();
        assert!(near(lifted.translate, [3.0, 2.25, -2.0]) && lifted.size == [10.0, 10.0]);
    }

    #[test]
    fn a_wall_faces_the_camera_on_either_side() {
        // On x = 4, spread in y and z.
        let wall: Vec<Vec3> = (0..9).map(|i| Vec3::new(4.0, (i % 3) as f64, (i / 3) as f64)).collect();
        for (eye, n) in [(Vec3::new(10.0, 1.0, 1.0), [1.0, 0.0, 0.0]), (Vec3::new(-10.0, 1.0, 1.0), [-1.0, 0.0, 0.0])] {
            let at = fit(&wall, Align::World, 0.0, None, 0.0, Some(eye)).unwrap();
            assert!(near(normal(&at), n), "{eye:?}: {:?}", normal(&at));
            // World X is along the normal: its X is world Z's direction.
            let x = Vec3::from(world(&at, Vec3::x())) - Vec3::from(at.translate);
            assert!(x.z.abs() > 0.999, "{x:?}");
        }
    }

    #[test]
    fn spread_lines_the_plane_up_with_a_diagonal_strip() {
        // A strip along x = z on the ground, 8 long and 1 wide.
        let strip: Vec<Vec3> = (0..18).map(|i| {
            let (along, across) = ((i / 2) as f64, (i % 2) as f64);
            Vec3::new(along - across, 0.0, along + across) / 2f64.sqrt()
        }).collect();
        let world_fit = fit(&strip, Align::World, 0.0, None, 0.0, None).unwrap();
        let spread = fit(&strip, Align::Spread, 0.0, None, 0.0, None).unwrap();
        let area = |a: &Placement| a.size[0] * a.size[1];
        assert!((spread.size[0].max(spread.size[1]) - 8.0).abs() < 1e-6 && (spread.size[0].min(spread.size[1]) - 1.0).abs() < 1e-6, "{:?}", spread.size);
        assert!(area(&spread) < area(&world_fit) / 3.0, "{:?} vs {:?}", spread.size, world_fit.size);
    }

    #[test]
    fn noisy_points_fit_in_the_least_squares_sense() {
        let mut pts = ground();
        for (k, p) in pts.iter_mut().enumerate() {
            p.y += if k % 2 == 0 { 0.01 } else { -0.01 };
        }
        let at = fit(&pts, Align::World, 0.0, None, 0.0, None).unwrap();
        assert!((at.translate[1] - 2.0).abs() < 0.01 && normal(&at)[1] > 0.999, "{at:?}");
    }

    #[test]
    fn too_few_or_lined_up_points_are_an_error() {
        let two = fit(&ground()[..2], Align::World, 0.0, None, 0.0, None).unwrap_err().to_string();
        assert!(two.contains("3 or more") && two.contains("2 given"), "{two}");
        let line: Vec<Vec3> = (0..5).map(|i| Vec3::new(i as f64, 0.0, 0.0)).collect();
        assert!(fit(&line, Align::World, 0.0, None, 0.0, None).is_err());
    }

    #[test]
    fn points_are_numbered_by_text() {
        assert_eq!(indices("0 3, 5\t8", 9).unwrap(), [0, 3, 5, 8]);
        assert!(indices("", 9).unwrap().is_empty());
        assert!(indices("9", 9).unwrap_err().to_string().contains("no point 9"));
        assert!(indices("two", 9).is_err());
        assert_eq!(indices_text(&[2, 7]), "2 7");
    }

    /// The node: picked cloud points (the others are off the plane), point
    /// sets and meshes when there's no cloud; the plane alone, or with Keep
    /// input the scene too; Plane's surface params come along.
    #[test]
    fn the_node_fits_picked_points_of_the_cloud_point_sets_or_meshes() {
        let mut cloud = ground();
        cloud.push(Vec3::new(0.0, 9.0, 0.0));
        let scene = Scene { points: cloud.clone(), ..Scene::default() };
        let picked = (0..15).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        let out = fit_plane(&json!({"points": picked, "holdout": true}), scene.clone(), None).unwrap();
        assert_eq!((out.meshes.len(), out.points.len()), (1, 0), "the plane alone");
        let m = &out.meshes[0];
        assert!(m.holdout, "Plane's params");
        let corner = geom::transform_point(&m.world, &m.mesh.positions[0]);
        assert!((corner.y - 2.0).abs() < 1e-9, "on y = 2: {corner:?}");
        // All 16: the stray point tilts it off y = 2.
        let all = fit_plane(&json!({}), scene.clone(), None).unwrap();
        let c = geom::transform_point(&all.meshes[0].world, &Vec3::zeros());
        assert!((c.y - 2.0).abs() > 0.1, "{c:?}");
        let kept = fit_plane(&json!({"points": picked, "keep_input": true}), scene, None).unwrap();
        assert_eq!((kept.meshes.len(), kept.points.len()), (1, 16));
        // A point set placed 1 up; a mesh when there are no points.
        let set = PointSet { positions: ground(), world: geom::trs([0.0, 1.0, 0.0], [0.0; 3], [1.0; 3]), ..PointSet::default() };
        let on_set = fit_plane(&json!({}), Scene { point_sets: vec![set], ..Scene::default() }, None).unwrap();
        assert!((geom::transform_point(&on_set.meshes[0].world, &Vec3::zeros()).y - 3.0).abs() < 1e-9);
        let floor = scene::primitive_model("plane", &json!({"rotate": [-90.0, 0.0, 0.0], "translate": [0.0, -1.0, 0.0], "size": [4.0, 4.0]}), None).unwrap();
        let on_mesh = fit_plane(&json!({}), Scene { meshes: vec![floor], ..Scene::default() }, None).unwrap();
        let at = &on_mesh.meshes[0];
        assert!((geom::transform_point(&at.world, &Vec3::zeros()).y + 1.0).abs() < 1e-9);
        let bad = fit_plane(&json!({"points": "40"}), Scene { points: cloud, ..Scene::default() }, None).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(bad.contains("no point 40"), "{bad}");
    }
}
