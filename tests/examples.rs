//! The example projects load and render as they say they do.

use arclight::graph::{Evaluator, Graph};
use arclight::image::Image;
use arclight::ops::EvalContext;
use std::path::Path;
use std::sync::Arc;

fn example(name: &str) -> Graph {
    Graph::from_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(name)).expect("the example loads")
}

fn eval(graph: &Graph, id: &str) -> Arc<Image> {
    eval_at(graph, id, 1)
}

fn eval_at(graph: &Graph, id: &str, frame: i64) -> Arc<Image> {
    let ctx = EvalContext { frame, write_outputs: false };
    Evaluator::new().evaluate(graph, id, &ctx).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// How many pixels of the lower half (the ground) of `img` are bush green.
fn ground_green(img: &Image) -> usize {
    let (w, h) = (img.width, img.height);
    img.pixels[h / 2 * w..].iter().filter(|[r, g, b, _]| *g > r + 0.1 && *g > b + 0.1).count()
}

/// geo-scatter.json: rocks and bush cards scattered in a soft patch (the
/// ellipse mask) of the ground, sized and colored by Attribute Set, one
/// copy per point, under a sun whose shadows darken the ground.
#[test]
fn the_geo_scatter_example_scatters_in_its_patch_and_renders_with_shadows() {
    let mut graph = example("geo-scatter.json");
    // Every point inside the patch: the mask's ellipse (radii 50 × 36
    // pixels and 20 of feather, of 512, over the 60-unit ground).
    let rocks = eval(&graph, "rock_look");
    let points = &rocks.scene.as_ref().expect("a scene").point_sets[0];
    let (rx, rz) = ((50.0 + 20.0) / 512.0 * 60.0, (36.0 + 20.0) / 512.0 * 60.0);
    assert!(points.len() > 150, "{} rocks", points.len());
    for i in 0..points.len() {
        let p = points.placed(i);
        assert!((p.x / rx).powi(2) + (p.z / rz).powi(2) <= 1.0 && p.y.abs() < 1e-6, "point {i} at {p:?} is off the patch");
    }
    assert!(points.attrs.float("pscale").is_some() && points.attrs.vec4("Cd").is_some(), "Attribute Set's pscale and Cd");
    // One copy per point.
    let placed = eval(&graph, "rocks_placed");
    let copies: usize = placed.scene.as_ref().unwrap().instances.iter().map(|i| i.len()).sum();
    assert_eq!(copies, points.len());
    // Rendered small: sky, ground and bushes; the sun's shadows darken the ground.
    for id in ["render", "sky_bg"] {
        let n = graph.nodes.iter_mut().find(|n| n.id == id).unwrap();
        n.params["width"] = 320.into();
        n.params["height"] = 180.into();
    }
    let lit = eval(&graph, "render");
    assert_eq!((lit.width, lit.height), (320, 180));
    let sky = lit.pixels[5 * 320 + 160];
    assert!(sky[2] > sky[0] + 0.1, "blue sky at the top: {sky:?}");
    let green = ground_green(&lit);
    assert!(green > 100, "bushes: {green} green pixels");
    graph.nodes.iter_mut().find(|n| n.id == "sun").unwrap().params["shadows"] = false.into();
    let open = eval(&graph, "render");
    let luma = |p: [f32; 4]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let darker = lit.pixels.iter().zip(&open.pixels).filter(|(a, b)| luma(**b) - luma(**a) > 0.03).count();
    assert!(darker > 100, "shadows darken the ground: {darker} pixels darker than without");
}

/// The mean column of `img`'s pixels that `pick` says are of a stream's
/// color, and how many there are.
fn mean_x(img: &Image, pick: impl Fn([f32; 4]) -> bool) -> (f64, usize) {
    let hits: Vec<usize> = img.pixels.iter().enumerate().filter(|(_, p)| pick(**p)).map(|(i, _)| i % img.width).collect();
    (hits.iter().sum::<usize>() as f64 / hits.len().max(1) as f64, hits.len())
}

/// particledirectionalforce.json: two streams rise off the same card, and
/// each one's own directional force pushes it its own way, teal toward +x
/// (right in the camera) and orange toward -x; without the forces they
/// rise together.
#[test]
fn the_particle_example_pushes_its_two_streams_apart() {
    let mut graph = example("particledirectionalforce.json");
    for id in ["render", "background"] {
        let n = graph.nodes.iter_mut().find(|n| n.id == id).unwrap();
        n.params["width"] = 320.into();
        n.params["height"] = 240.into();
    }
    let teal = |p: [f32; 4]| p[2] > p[0] + 0.1;
    let orange = |p: [f32; 4]| p[0] > p[2] + 0.1;
    let apart = |graph: &Graph| {
        let img = eval_at(graph, "render", 60);
        let ((t, nt), (o, no)) = (mean_x(&img, teal), mean_x(&img, orange));
        assert!(nt > 200 && no > 200, "both streams show: {nt} teal, {no} orange pixels");
        t - o
    };
    let pushed = apart(&graph);
    assert!(pushed > 60.0, "teal right of orange by {pushed:.0} pixels");
    for id in ["force_teal", "force_orange"] {
        graph.nodes.iter_mut().find(|n| n.id == id).unwrap().params["strength"] = 0.0.into();
    }
    let together = apart(&graph);
    assert!(together.abs() < 20.0, "without the forces they rise together ({together:.0} pixels apart)");
}

/// multi-projection.json: two projectors, left and right, blended where
/// they overlap: the wall's middle, which both see, mixes their colors
/// (orange and blue), rather than the later one's blue alone.
#[test]
fn the_multi_projection_example_blends_its_two_projectors() {
    let mut graph = example("multi-projection.json");
    for id in ["render", "backdrop"] {
        let n = graph.nodes.iter_mut().find(|n| n.id == id).unwrap();
        n.params["width"] = 240.into();
        n.params["height"] = 135.into();
    }
    let scene = eval(&graph, "project_right").scene.clone().expect("a scene");
    assert_eq!(scene.projections.len(), 2);
    assert!(scene.projections.iter().all(|p| p.blend && p.fade_start < p.fade_end));
    // Warm (orange) pixels over the wall's upper middle, seen through the
    // shot: both plates there when blended, the later's blue alone when
    // stacked.
    let warm = |graph: &Graph| {
        let img = eval(graph, "render");
        let (w, h) = (img.width, img.height);
        img.pixels[h / 10 * w..h / 3 * w].iter().enumerate().filter(|(i, p)| (w / 3..2 * w / 3).contains(&(i % w)) && p[0] > p[2] + 0.05).count()
    };
    let blended = warm(&graph);
    for n in graph.nodes.iter_mut().filter(|n| n.op == "project_3d") {
        n.params["blend"] = false.into();
    }
    let stacked = warm(&graph);
    assert!(blended > 4 * stacked.max(1) && blended > 200, "warm pixels: {blended} blended, {stacked} stacked");
}
