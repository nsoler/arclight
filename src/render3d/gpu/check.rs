//! The 3D renderer's rows in the GPU self-test (`arclight gpu --check`,
//! Help ▸ GPU self-test): a lit scene with every kind of surface and light
//! rendered on the CPU and the GPU, compared by D8's statistics (see
//! docs/gpu-plan.md), and timed. The scene is the GPU tests' too.

use super::{draw, Passes, Size};
use crate::geom::Camera;
use crate::gpu::check::{timed, CheckRow};
use crate::gpu::Gpu;
use crate::graph::{Evaluator, Graph};
use crate::image::Image;
use crate::ops::EvalContext;
use crate::scene::Scene;
use std::sync::Arc;

/// A scene and its camera from a graph whose node `scene` makes the
/// scene and `cam` the camera.
pub(super) fn scene_of(graph: &str) -> (Arc<Scene>, Camera) {
    let graph: Graph = graph.parse().expect("graph");
    let mut e = Evaluator::new();
    let ctx = EvalContext { frame: 1, write_outputs: false };
    let scene = e.evaluate(&graph, "scene", &ctx).expect("scene").scene.clone().expect("a scene");
    let cam = e.evaluate(&graph, "cam", &ctx).expect("camera").scene.as_ref().expect("a camera").cameras[0].clone();
    (scene, cam)
}

/// Every kind of surface and light: textured cards (one minified, one
/// see-through), a lit sphere with a highlight, a cube, a holdout, and
/// ambient, directional, point (with a radius), spot and area lights; the
/// camera node `camera`.
pub(super) fn busy(camera: &str) -> String {
    format!(
        r#"{{"project": {{"format": {{"width": 320, "height": 180}}, "fps": 24.0, "first_frame": 1, "last_frame": 10}}, "nodes": [
        {{"id": "tex", "op": "checkerboard", "params": {{"width": 256, "height": 256, "size": 16}}}},
        {{"id": "card", "op": "card", "inputs": ["tex"], "params": {{"translate": [0, 0, -3], "scale": [5, 5, 1]}}}},
        {{"id": "floor", "op": "card", "inputs": ["tex"], "params": {{"translate": [0, -1, -10], "rotate": [-90, 0, 0], "scale": [40, 40, 1], "shading": "lambert"}}}},
        {{"id": "glass", "op": "card", "inputs": ["tex"], "params": {{"translate": [0.8, 0.2, 0.5], "rotate": [0, 30, 0], "opacity": 0.4}}}},
        {{"id": "sph", "op": "sphere", "inputs": ["tex"], "params": {{"segments": [48, 24], "shading": "blinn", "specular": 0.6, "roughness": 0.3}}}},
        {{"id": "cube", "op": "cube", "params": {{"translate": [1.2, 0, -1], "rotate": [20, 30, 0], "emission": [0.1, 0.0, 0.0, 1.0]}}}},
        {{"id": "hold", "op": "sphere", "params": {{"translate": [-1.2, 0.3, 0], "radius": 0.3, "holdout": true}}}},
        {{"id": "amb", "op": "light_ambient", "params": {{"intensity": 0.2}}}},
        {{"id": "sun", "op": "light_directional", "params": {{"rotate": [-50, 30, 0]}}}},
        {{"id": "pt", "op": "light_point", "params": {{"translate": [2, 2, 1], "radius": 0.3, "decay": 1.0}}}},
        {{"id": "spot", "op": "light_spot", "params": {{"translate": [-2, 3, 2], "rotate": [-45, -30, 0], "penumbra": 8.0, "falloff": 1.0}}}},
        {{"id": "area", "op": "light_area", "params": {{"translate": [0, 3, 1], "rotate": [-70, 0, 0], "shape": "ellipse", "two_sided": true}}}},
        {{"id": "scene", "op": "scene", "inputs": ["card", "floor", "glass", "sph", "cube", "hold", "amb", "sun", "pt", "spot", "area"]}},
        {camera}
        ]}}"#
    )
}

pub(super) const PERSPECTIVE: &str = r#"{"id": "cam", "op": "camera", "params": {"translate": [0, 0.3, 3.5]}}"#;
pub(super) const ORTHO: &str = r#"{"id": "cam", "op": "camera", "params": {"translate": [0, 0.3, 3.5], "projection": "orthographic", "ortho_width": 6.0}}"#;

/// The passes taken from the nearest surface a pixel's samples see: which
/// sample that is can differ (where samples tie, the CPU's own choice is
/// f64 rounding), so their values may differ by up to a pixel's worth.
pub(super) fn nearest_pass(name: &str) -> bool {
    ["depth", "P.", "N.", "uv.", "motion.", "id"].iter().any(|p| name.starts_with(p))
}

/// How two renders differ: per plane (the picture first, as "rgba"), the
/// mean absolute difference and the share of values off: by more than
/// 1e-2, or for a nearest-surface pass, by more than the CPU's own change
/// to a neighbouring pixel (plus 1e-3).
pub(super) fn differences(cpu: &Image, gpu: &Image) -> Vec<(String, f64, f64)> {
    let w = cpu.width.max(1);
    let stats = |a: &[f32], b: &[f32], near: bool| {
        let (mut sum, mut off) = (0.0f64, 0usize);
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            let d = (x - y).abs();
            sum += d as f64;
            let allowed = if near {
                let spread = [i.wrapping_sub(1), i + 1, i.wrapping_sub(w), i + w].iter().filter_map(|&j| a.get(j)).map(|v| (v - x).abs()).fold(0.0f32, f32::max);
                spread + 1e-3
            } else {
                1e-2
            };
            if d > allowed {
                off += 1;
            }
        }
        (sum / a.len().max(1) as f64, off as f64 / a.len().max(1) as f64)
    };
    let flat = |img: &Image| img.pixels.iter().flatten().copied().collect::<Vec<f32>>();
    let (m, o) = stats(&flat(cpu), &flat(gpu), false);
    let mut out = vec![("rgba".to_string(), m, o)];
    for p in &cpu.planes {
        match gpu.plane(&p.name) {
            Some(q) => {
                let (m, o) = stats(&p.data, &q.data, nearest_pass(&p.name));
                out.push((p.name.clone(), m, o));
            }
            None => out.push((p.name.clone(), f64::INFINITY, 1.0)),
        }
    }
    out
}

/// The share of values a plane may have off (see [`differences`]), and
/// the most its mean may be: the picture and light passes 0.5 % and 1e-3,
/// the nearest-surface passes 1 %.
pub(super) fn allowed(name: &str) -> (f64, f64) {
    if nearest_pass(name) {
        (f64::INFINITY, 0.01)
    } else {
        (1e-3, 0.005)
    }
}

/// The rows: the busy scene at 1080p (the picture and depth; every pass),
/// orthographic, and as the 3D view shades it.
pub(crate) fn rows(gpu: &Gpu) -> Vec<CheckRow> {
    let hd = Size { width: 1920, height: 1080 };
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let (flat, ortho) = scene_of(&busy(ORTHO));
    let all = Passes::from_names(Passes::NAMES);
    vec![
        row(gpu, "3D render, lit, AA 3×3", &scene, &cam, hd, 3, Passes::DEPTH, false),
        row(gpu, "3D render, every pass", &scene, &cam, hd, 3, all, false),
        row(gpu, "3D render, orthographic", &flat, &ortho, hd, 2, Passes::DEPTH, false),
        row(gpu, "3D view, solid", &scene, &cam, hd, 1, Passes::DEPTH, true),
    ]
}

/// One row: `scene` through `cam` both ways, compared and timed; with
/// `solid`, shaded as the 3D view's Solid (lit from the camera).
#[allow(clippy::too_many_arguments)]
fn row(gpu: &Gpu, case: &str, scene: &Scene, cam: &Camera, format: Size, samples: usize, passes: Passes, solid: bool) -> CheckRow {
    let mut surfs = super::super::surfaces(scene);
    let lights = if solid { &[][..] } else { &scene.lights[..] };
    if solid {
        surfs.iter_mut().skip(scene.cards.len()).for_each(|s| s.headlight = true);
    }
    let size = format!("{}x{}", format.width, format.height);
    let cpu_draw = || crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, lights, &scene.points, cam, format, samples, passes, None, None, None));
    let gpu_draw = || draw(gpu, &surfs, lights, &scene.points, cam, format, samples, passes, None, None);
    // Once each first: the GPU builds its pipelines, and textures go up.
    let _ = gpu_draw();
    let (cpu, cpu_ms) = timed(cpu_draw);
    let (out, gpu_ms) = timed(gpu_draw);
    let mut r = CheckRow { case: case.into(), size, max_abs: 0.0, max_rel: 0.0, tol_abs: 1e-3, tol_rel: 0.005, pass: true, cpu_ms, gpu_kernel_ms: gpu_ms, gpu_total_ms: gpu_ms, note: String::new() };
    let out = match out {
        Ok(Some(img)) => img,
        Ok(None) => {
            r.pass = false;
            r.note = "rendered on the CPU instead".into();
            return r;
        }
        Err(e) => {
            r.pass = false;
            r.note = format!("{e:#}");
            return r;
        }
    };
    // Max abs: the worst mean difference; max rel: the worst share of
    // values off (edges fall a little differently in f32), of the picture
    // and light passes; the nearest-surface passes are held to theirs.
    for (name, mean, off) in differences(&cpu, &out) {
        let (most_mean, most_off) = allowed(&name);
        if !nearest_pass(&name) {
            r.max_abs = r.max_abs.max(mean);
            r.max_rel = r.max_rel.max(off);
        }
        if (mean > most_mean || off > most_off) && r.pass {
            r.pass = false;
            r.note = format!("{name}: mean difference {mean:.2e}, {:.2} % of values off", off * 100.0);
        }
    }
    if r.pass {
        r.note = "mean difference and share of values off (edges fall a little differently in f32)".into();
    }
    r
}
