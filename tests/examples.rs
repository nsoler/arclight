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
    let ctx = EvalContext { frame: 1, write_outputs: false };
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
