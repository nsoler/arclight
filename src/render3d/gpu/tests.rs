//! The GPU renderer against the CPU one on the same scenes (D8 of
//! docs/gpu-plan.md): skipped without a GPU.

use super::check::{busy, busy_shadowed, differences, moving, nearest_pass, scene_at, scene_of, ORTHO, PERSPECTIVE};
use super::*;

thread_local! {
    /// Whether `draw_passes` may take the GPU on this thread (see there).
    pub(super) static GPU_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with [`super::draw_passes`] taking the GPU.
fn with_gpu<R>(f: impl FnOnce() -> R) -> R {
    GPU_ON.with(|on| on.set(true));
    let out = f();
    GPU_ON.with(|on| on.set(false));
    out
}

/// Render `scene` both ways with `passes` and check D8's statistics:
/// the picture and light passes mean ≤ 1e-3 and ≤ 0.5 % of values off;
/// the nearest-surface passes (which flip whole values where an edge
/// falls differently) ≤ 1 % of values off.
fn compare(gpu: &Gpu, scene: &Scene, cam: &Camera, format: Size, samples: usize, passes: Passes, cell: Option<Cell>) {
    let surfs = super::super::surfaces(scene);
    let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &scene.lights, &scene.points, cam, format, samples, passes, None, cell, None));
    let out = draw(gpu, &surfs, &scene.lights, &scene.points, cam, format, samples, passes, None, cell).expect("no GPU error").expect("rendered on the GPU");
    for (name, mean, off) in differences(&cpu, &out) {
        let (most_mean, most_off) = super::check::allowed(&name);
        assert!(mean <= most_mean && off <= most_off, "{name}: mean {mean:.2e}, {:.3} % of values off", off * 100.0);
    }
}

fn all_passes() -> Passes {
    Passes::from_names(Passes::NAMES)
}

#[test]
fn a_busy_scene_matches_the_cpu_render() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let format = Size { width: 320, height: 180 };
    compare(&gpu, &scene, &cam, format, 3, all_passes(), None);
    compare(&gpu, &scene, &cam, format, 1, Passes::DEPTH, None);
}

#[test]
fn orthographic_and_jittered_cells_match_too() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy(ORTHO));
    let format = Size { width: 320, height: 180 };
    compare(&gpu, &scene, &cam, format, 2, Passes::from_names(["depth", "P", "id", "mattes"]), None);
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    compare(&gpu, &scene, &cam, format, 2, Passes::DEPTH, Some(Cell { side: 2, x: 1, y: 0 }));
}

#[test]
fn the_3d_views_solid_shading_matches() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let mut surfs = super::super::surfaces(&scene);
    for s in surfs.iter_mut().skip(scene.cards.len()) {
        s.headlight = true;
    }
    let format = Size { width: 320, height: 180 };
    let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &[], &[], &cam, format, 1, Passes::DEPTH, None, None, None));
    let out = draw(&gpu, &surfs, &[], &[], &cam, format, 1, Passes::DEPTH, None, None).unwrap().unwrap();
    for (name, mean, off) in differences(&cpu, &out) {
        assert!(mean <= 1e-3 && off <= 0.01, "{name}: mean {mean:.2e}, {:.3} % off", off * 100.0);
    }
}

/// Away from edges the two agree to f32 precision: a card filling the
/// frame, lit, its texture minified. (A sharp checkerboard: f32 texture
/// coordinates move a bilinear tap across a black-to-white texel edge by a
/// little, so the color within 5e-4.)
#[test]
fn inside_a_surface_the_values_agree_closely() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(
        r#"{"project": {"format": {"width": 64, "height": 48}, "fps": 24.0, "first_frame": 1, "last_frame": 1}, "nodes": [
        {"id": "tex", "op": "checkerboard", "params": {"width": 512, "height": 512, "size": 8}},
        {"id": "card", "op": "card", "inputs": ["tex"], "params": {"translate": [0, 0, -2], "rotate": [-30, 10, 0], "scale": [8, 8, 1], "shading": "blinn", "specular": 0.5}},
        {"id": "pt", "op": "light_point", "params": {"translate": [0.5, 1, 1]}},
        {"id": "scene", "op": "scene", "inputs": ["card", "pt"]},
        {"id": "cam", "op": "camera"}
        ]}"#,
    );
    let format = Size { width: 64, height: 48 };
    let surfs = super::super::surfaces(&scene);
    let passes = Passes::from_names(["diffuse", "specular", "emission"]);
    let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None));
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 2, passes, None, None).unwrap().unwrap();
    let worst = cpu.pixels.iter().flatten().zip(out.pixels.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst <= 5e-4, "color off by {worst}");
    for p in cpu.planes.iter().filter(|p| !nearest_pass(&p.name)) {
        let q = out.plane(&p.name).unwrap();
        let worst = p.data.iter().zip(&q.data).map(|(a, b)| ((a - b) / a.abs().max(1.0)).abs()).fold(0.0f32, f32::max);
        assert!(worst <= 5e-4, "{}: off by {worst}", p.name);
    }
}

/// More see-through surfaces at a sample than the shader keeps, before
/// it's nearly opaque: those pixels come from the CPU, the frame matches
/// the CPU's; layers so faint behind that the light is all but used up
/// stay on the GPU.
#[test]
fn deep_see_through_stacks_take_their_pixels_from_the_cpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    for opacity in [0.05, 0.6] {
        let cards: Vec<String> = (0..24).map(|i| format!(r#"{{"id": "c{i}", "op": "card", "inputs": ["tex"], "params": {{"translate": [{}, 0, -{}], "scale": [0.6, 0.6, 1], "opacity": {opacity}}}}}"#, (i % 5) as f64 * 0.05, 1.0 + i as f64 * 0.1)).collect();
        let ids: Vec<String> = (0..24).map(|i| format!("\"c{i}\"")).collect();
        let graph = format!(
            r#"{{"project": {{"format": {{"width": 48, "height": 32}}, "fps": 24.0, "first_frame": 1, "last_frame": 1}}, "nodes": [
            {{"id": "tex", "op": "constant", "params": {{"width": 8, "height": 8, "color": [0.9, 0.6, 0.3, 1.0]}}}},
            {},
            {{"id": "scene", "op": "scene", "inputs": [{}]}},
            {{"id": "cam", "op": "camera"}}
            ]}}"#,
            cards.join(",\n"),
            ids.join(", ")
        );
        let (scene, cam) = scene_of(&graph);
        let surfs = super::super::surfaces(&scene);
        let format = Size { width: 48, height: 32 };
        let passes = Passes::from_names(["depth", "mattes"]);
        let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &[], &[], &cam, format, 2, passes, None, None, None));
        let out = draw(&gpu, &surfs, &[], &[], &cam, format, 2, passes, None, None).unwrap().expect("rendered");
        let worst = cpu.pixels.iter().flatten().zip(out.pixels.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst <= 1e-3, "opacity {opacity}: off by {worst}");
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "opacity {opacity}, {name}: mean {mean:.2e}, {:.3} % off", off * 100.0);
        }
    }
}

/// Tiles hold each triangle that may touch them, once, in order.
#[test]
fn tiles_list_their_triangles() {
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let format = Size { width: 320, height: 180 };
    let surfs = super::super::surfaces(&scene);
    let tris = super::super::triangles(&surfs, &cam, format, None);
    let (tx, ty) = (format.width.div_ceil(TILE), format.height.div_ceil(TILE));
    let bins = bin_tiles(&tris, tx, ty);
    for tile in 0..tx * ty {
        let list = &bins[bins[tile] as usize..bins[tile + 1] as usize];
        assert!(list.windows(2).all(|w| w[0] < w[1]), "in order, once");
        let rect = IRect::new((tile % tx * TILE) as i32, (tile / tx * TILE) as i32, ((tile % tx + 1) * TILE) as i32, ((tile / tx + 1) * TILE) as i32);
        for (i, t) in tris.iter().enumerate() {
            assert_eq!(list.contains(&(i as u32)), !t.bounds.intersect(&rect).is_empty(), "tile {tile}, triangle {i}");
        }
    }
}

/// Scanline Render's own entry point takes the GPU when it's there (the
/// same picture as on the CPU), and the CPU when it's turned off.
#[test]
fn render_passes_goes_through_the_gpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let format = Size { width: 320, height: 180 };
    let passes = Passes::from_names(["depth", "id"]);
    let cpu = super::super::render_passes(&scene, &cam, format, 2, None, passes);
    let surfs = super::super::surfaces(&scene);
    let direct = draw(&gpu, &surfs, &scene.lights, &scene.points, &cam, format, 2, passes, None, None).unwrap().unwrap();
    let through = with_gpu(|| super::super::render_passes(&scene, &cam, format, 2, None, passes));
    assert_eq!(through.pixels, direct.pixels, "rendered by the GPU");
    assert!(differences(&cpu, &through).iter().all(|(_, mean, off)| *mean <= 1e-2 && *off <= 0.01));
    let off = with_gpu(|| crate::gpu::cpu_only(|| super::super::render_passes(&scene, &cam, format, 2, None, passes)));
    assert_eq!(off.pixels, cpu.pixels, "the CPU when the GPU is off");
}

/// Shadows from every kind of light (a soft point light, a spot with a
/// wider filter, a sun, an area light's cube maps) match the CPU's, and so
/// do the shadow and per-light passes.
#[test]
fn shadows_match_the_cpu_render() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy_shadowed(PERSPECTIVE));
    let format = Size { width: 320, height: 180 };
    compare(&gpu, &scene, &cam, format, 2, all_passes(), None);
    // (There are shadows: a good share of pixels lose light to them.)
    let surfs = super::super::surfaces(&scene);
    let passes = Passes::from_names(["shadow"]);
    let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &scene.lights, &[], &cam, format, 1, passes, None, None, None));
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 1, passes, None, None).unwrap().unwrap();
    let shadowed = |img: &Image| img.plane("shadow.R").unwrap().data.iter().filter(|v| **v > 0.02).count();
    assert!(shadowed(&cpu) > 320 * 180 / 10, "{} shadowed pixels", shadowed(&cpu));
    assert!(shadowed(&cpu).abs_diff(shadowed(&out)) < 320 * 180 / 200, "{} against {}", shadowed(&cpu), shadowed(&out));
    let (scene, cam) = scene_of(&busy_shadowed(ORTHO));
    compare(&gpu, &scene, &cam, format, 1, Passes::from_names(["depth", "shadow"]), None);
}

/// A caster that lets some light through casts a layered shadow: the
/// CPU's still.
#[test]
fn see_through_casters_render_on_the_cpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&busy_shadowed(PERSPECTIVE).replace(r#", "casts_shadows": false"#, ""));
    let surfs = super::super::surfaces(&scene);
    let format = Size { width: 64, height: 36 };
    assert!(draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 1, Passes::DEPTH, None, None).unwrap().is_none());
}

/// Motion blur added up on the GPU (each pixel's times jittered by its
/// cell, or not; with shadows; the light passes and mattes too) matches
/// the CPU's render of each time, averaged.
#[test]
fn motion_blur_adds_up_on_the_gpu_as_on_the_cpu() {
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let format = Size { width: 160, height: 90 };
    for (graph, passes, jitter) in [
        (moving(&busy(PERSPECTIVE)), Passes::from_names(["depth", "mattes", "diffuse", "specular", "lights"]), true),
        (moving(&busy(PERSPECTIVE)), Passes::DEPTH, false),
        (moving(&busy_shadowed(PERSPECTIVE)), Passes::from_names(["depth", "shadow"]), true),
    ] {
        let ((s0, cam), (s1, _)) = (scene_at(&graph, 4), scene_at(&graph, 5));
        let blur = super::super::Blur { times: super::super::Blur::times(0.0, 0.5, 6), frames: vec![(0, &*s0, cam.clone()), (1, &*s1, cam.clone())], jitter };
        let frames: Vec<(i64, &Scene)> = vec![(0, &s0), (1, &s1)];
        let mixed = Passes { depth: false, ..passes };
        assert!(with_gpu(|| blurred(&s0, &cam, format, 2, mixed, &blur, &frames)).is_some(), "on the GPU");
        let cpu = crate::gpu::cpu_only(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        let out = with_gpu(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "jitter {jitter}, {name}: mean {mean:.2e}, {:.3} % of values off", off * 100.0);
        }
        // (It's blurred: the moving sphere smears.)
        let still = crate::gpu::cpu_only(|| super::super::render_passes(&s0, &cam, format, 2, None, passes));
        assert!(differences(&still, &out)[0].1 > 1e-3);
    }
}
