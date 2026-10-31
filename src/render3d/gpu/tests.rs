//! The GPU renderer against the CPU one on the same scenes (D8 of
//! docs/gpu-plan.md): skipped without a GPU.

use super::check::{busy, busy_shadowed, differences, layered, moving, nearest_pass, scattered, scene_at, scene_of, ORTHO, PERSPECTIVE};
use super::*;

thread_local! {
    /// Whether `draw_passes` may take the GPU on this thread (see there).
    pub(super) static GPU_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The shadow maps' room where they have layers, in place of the
    /// device's (see `layer_room`).
    pub(super) static LAYER_ROOM: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// For a motion-blur test's failure message: whether `out` (a blurred
/// render) is the GPU's own sum of the times (`direct`, from
/// [`blurred`]), or came another way, and the GPU's last error.
fn blur_path(direct: Option<&Image>, out: &Image) -> String {
    let path = if direct.is_some_and(|d| d.pixels == out.pixels) { "the GPU sum" } else { "not the GPU sum (time by time, or the CPU)" };
    format!("rendered by {path}; the GPU's last error: {:?}", crate::gpu::last_error())
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
    let out = draw(gpu, &surfs, &scene.lights, &scene.points, cam, format, samples, passes, None, cell, None).expect("no GPU error").expect("rendered on the GPU");
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
    let out = draw(&gpu, &surfs, &[], &[], &cam, format, 1, Passes::DEPTH, None, None, None).unwrap().unwrap();
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
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None).unwrap().unwrap();
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
        let out = draw(&gpu, &surfs, &[], &[], &cam, format, 2, passes, None, None, None).unwrap().expect("rendered");
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
    let direct = draw(&gpu, &surfs, &scene.lights, &scene.points, &cam, format, 2, passes, None, None, None).unwrap().unwrap();
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
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 1, passes, None, None, None).unwrap().unwrap();
    let shadowed = |img: &Image| img.plane("shadow.R").unwrap().data.iter().filter(|v| **v > 0.02).count();
    assert!(shadowed(&cpu) > 320 * 180 / 10, "{} shadowed pixels", shadowed(&cpu));
    assert!(shadowed(&cpu).abs_diff(shadowed(&out)) < 320 * 180 / 200, "{} against {}", shadowed(&cpu), shadowed(&out));
    let (scene, cam) = scene_of(&busy_shadowed(ORTHO));
    compare(&gpu, &scene, &cam, format, 1, Passes::from_names(["depth", "shadow"]), None);
}

/// Casters that let some light through cast layered shadows on the GPU
/// as on the CPU: a card at 0.4, a feathered cutout, stacks of them (one
/// deeper than a texel keeps), under sharp, soft and penumbra lights.
#[test]
fn see_through_casters_shadow_as_on_the_cpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&layered(128, 12.0));
    let format = Size { width: 320, height: 180 };
    compare(&gpu, &scene, &cam, format, 2, Passes::from_names(["shadow", "diffuse", "depth"]), None);
    // (They have layers, and their shadows let light through part way.)
    let surfs = super::super::surfaces(&scene);
    let prep = prepare(&gpu, &surfs, &scene.lights, &cam, format, Passes::default(), None, None).expect("on the GPU");
    assert!(prep.tables.maps.iter().any(|m| m.layered));
    let passes = Passes::from_names(["shadow"]);
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 1, passes, None, None, None).unwrap().unwrap();
    let part = out.plane("shadow.R").unwrap().data.iter().filter(|v| **v > 0.02 && **v < 0.3).count();
    assert!(part > 320 * 180 / 50, "{part} pixels in part shadow");
    // The busy scene's glass card casting too, orthographic.
    let (scene, cam) = scene_of(&busy_shadowed(ORTHO).replace(r#", "casts_shadows": false"#, ""));
    compare(&gpu, &scene, &cam, format, 1, Passes::from_names(["depth", "shadow"]), None);
}

/// An area light of many samples (16 × 16 shadow cubes) shadows as on the
/// CPU, at softness 1 and 2 (whose filter taps fall a texel apart: each
/// texel of their footprint read once) and 1.5 (taps between texels).
#[test]
fn many_sample_area_lights_shadow_as_on_the_cpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    for soft in ["1.0", "2.0", "1.5"] {
        let graph = layered(128, 12.0).replace(r#""shape": "ellipse""#, &format!(r#""shape": "ellipse", "quality": 16, "shadow_softness": {soft}"#));
        let (scene, cam) = scene_of(&graph);
        compare(&gpu, &scene, &cam, Size { width: 160, height: 90 }, 2, Passes::from_names(["shadow", "depth"]), None);
    }
}

/// Where the layers won't all fit the maps' room, the lights keep fewer
/// a texel (more pixels go to the CPU, the same picture); where even one
/// won't, the frame is the CPU's.
#[test]
fn layers_shrink_to_fit_or_leave_the_frame_to_the_cpu() {
    let Some(gpu) = crate::gpu::test_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (scene, cam) = scene_of(&layered(128, 12.0));
    let surfs = super::super::surfaces(&scene);
    let format = Size { width: 160, height: 90 };
    let prep = prepare(&gpu, &surfs, &scene.lights, &cam, format, Passes::default(), None, None).expect("on the GPU");
    let depths: usize = prep.tables.maps.iter().map(|m| m.res * m.res).sum();
    let layered_texels: usize = prep.tables.maps.iter().filter(|m| m.layered).map(|m| m.res * m.res).sum();
    let layers = |prep: &Prepared| (0..prep.tables.maps.len()).filter(|&i| prep.tables.maps[i].layered).map(|i| prep.tables.tables[prep.tables.at[3] + i * MAP_WORDS + 26]).collect::<Vec<u32>>();
    assert!(layers(&prep).iter().all(|&k| k == MAX_LAYERS as u32));
    let passes = Passes::from_names(["shadow", "depth"]);
    let cpu = crate::gpu::cpu_only(|| super::super::draw_passes(&surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None));
    // Room for one layer a texel.
    LAYER_ROOM.with(|r| r.set(Some((depths + 3 * layered_texels) * 4)));
    let prep = prepare(&gpu, &surfs, &scene.lights, &cam, format, Passes::default(), None, None).expect("one layer each fits");
    assert!(layers(&prep).iter().all(|&k| k == 1), "{:?}", layers(&prep));
    let out = draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None).unwrap().expect("on the GPU");
    for (name, mean, off) in differences(&cpu, &out) {
        let (most_mean, most_off) = super::check::allowed(&name);
        assert!(mean <= most_mean && off <= most_off, "one layer, {name}: mean {mean:.2e}, {:.3} % of values off", off * 100.0);
    }
    // Not even that: the CPU's, the same picture.
    LAYER_ROOM.with(|r| r.set(Some((depths + 3 * layered_texels) * 4 - 4)));
    assert!(prepare(&gpu, &surfs, &scene.lights, &cam, format, Passes::default(), None, None).is_none());
    assert!(draw(&gpu, &surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None).unwrap().is_none());
    let out = with_gpu(|| super::super::draw_passes(&surfs, &scene.lights, &[], &cam, format, 2, passes, None, None, None));
    LAYER_ROOM.with(|r| r.set(None));
    assert!(differences(&cpu, &out).iter().all(|(_, mean, _)| *mean == 0.0));
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
        let direct = with_gpu(|| blurred(&s0, &cam, format, 2, mixed, &blur, &frames, false));
        assert!(direct.is_some(), "on the GPU: {:?}", crate::gpu::last_error());
        let cpu = crate::gpu::cpu_only(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        let out = with_gpu(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "jitter {jitter}, {name}: mean {mean:.2e}, {:.3} % of values off; {}", off * 100.0, blur_path(direct.as_ref(), &out));
        }
        // (It's blurred: the moving sphere smears.)
        let still = crate::gpu::cpu_only(|| super::super::render_passes(&s0, &cam, format, 2, None, passes));
        assert!(differences(&still, &out)[0].1 > 1e-3);
    }
}

/// Sprites drawn on the GPU match the CPU's: round and square, textured
/// by a sequence, laid over or added, over a render whose depth hides
/// some, and in 2D.
#[test]
fn sprites_match_the_cpu() {
    use crate::particles_render::{render_sprites, render_sprites_2d, Blend, Outline, Pick, Sprites, Texture};
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let format = Size { width: 240, height: 130 };
    let px = format.width * format.height;
    // (They do go to the GPU where they cover over twice the frame.)
    let big = || crate::particles_render::Splat { at: [120.0, 65.0], radius: 200.0, depth: 1.0, turn: 0.0, color: [0.5; 4], image: None };
    assert!(with_gpu(|| crate::render3d::sprites_on_gpu(&[big(), big(), big()], &Sprites::default(), format, &vec![[0.0; 4]; px], &vec![0.0; px], true)).is_some());
    let particles = scattered(3000);
    let cam = Camera::default();
    let frames: Vec<Arc<Image>> = (0..3).map(|k| Arc::new(crate::gpu::check::noise(Size { width: 16, height: 16 }, IRect::new(1, 0, 16, 15), false, 7 + k))).collect();
    let textured = Sprites { outline: Outline::Square, texture: Some(Texture { images: frames, pick: Pick::ByAge }), scale: 1.5, opacity: 1.0, opacity_end: 0.2, blend: Blend::Over };
    let (scene, scam) = scene_of(&busy(PERSPECTIVE));
    let behind = crate::gpu::cpu_only(|| super::super::render_passes(&scene, &scam, format, 1, None, Passes::DEPTH));
    for (sprites, background) in [
        (Sprites::default(), None),
        (textured.clone(), None),
        (Sprites { blend: Blend::Additive, ..Sprites::default() }, Some(&behind)),
        (textured, Some(&behind)),
    ] {
        let cpu = crate::gpu::cpu_only(|| render_sprites(&particles, &sprites, &cam, format, background));
        let out = with_gpu(|| render_sprites(&particles, &sprites, &cam, format, background));
        for (name, mean, off) in differences(&cpu, &out) {
            assert!(mean <= 1e-4 && off <= 0.001, "{name}: mean {mean:.2e}, {:.3} % of values off", off * 100.0);
        }
    }
    let flat: Vec<crate::particles::Particle> = particles.iter().map(|p| crate::particles::Particle { position: Vec3::new((p.position.x + 2.0) * 60.0, (p.position.y + 1.2) * 54.0, 0.0), size: p.size * 30.0, ..p.clone() }).collect();
    let cpu = crate::gpu::cpu_only(|| render_sprites_2d(&flat, &Sprites::default(), format, None));
    let out = with_gpu(|| render_sprites_2d(&flat, &Sprites::default(), format, None));
    let (_, mean, off) = differences(&cpu, &out)[0].clone();
    assert!(mean <= 1e-4 && off <= 0.001, "2D: mean {mean:.2e}, {:.3} % off", off * 100.0);
}

/// Motion blur over particle sprites adds up on the GPU (each time's
/// sprites laid over that time's render there) as the CPU renders it time
/// by time; sprites added up over a background render time by time.
#[test]
fn motion_blur_with_sprites_adds_up_on_the_gpu() {
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let format = Size { width: 160, height: 90 };
    for blend in ["over", "additive"] {
        let graph = moving(&busy(PERSPECTIVE))
            .replace(
                r#"{"id": "scene", "op": "scene", "inputs": ["#,
                &format!(
                    r#"{{"id": "em", "op": "particle_emitter", "params": {{"rate": 40, "life": 30, "speed": 2.0, "spread": 50.0, "size": 0.4, "translate": [0, -0.5, 0]}}}},
        {{"id": "pr", "op": "particle_render", "inputs": ["em", "", ""], "params": {{"blend": "{blend}", "opacity": 0.6}}}},
        {{"id": "scene", "op": "scene", "inputs": ["pr", "#
                ),
            );
        let ((s0, cam), (s1, _)) = (scene_at(&graph, 6), scene_at(&graph, 7));
        assert!(!s0.particles.is_empty(), "a particle stream");
        let blur = super::super::Blur { times: super::super::Blur::times(0.0, 0.5, 6), frames: vec![(0, &*s0, cam.clone()), (1, &*s1, cam.clone())], jitter: true };
        let frames: Vec<(i64, &Scene)> = vec![(0, &s0), (1, &s1)];
        let passes = Passes::from_names(["depth", "mattes"]);
        let mixed = Passes { depth: false, ..passes };
        let direct = with_gpu(|| blurred(&s0, &cam, format, 2, mixed, &blur, &frames, false));
        assert!(direct.is_some(), "{blend}: on the GPU: {:?}", crate::gpu::last_error());
        let cpu = crate::gpu::cpu_only(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        let out = with_gpu(|| super::super::render_blurred(&s0, &cam, format, 2, None, passes, None, Some(&blur)));
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "{blend}, {name}: mean {mean:.2e}, {:.3} % of values off; {}", off * 100.0, blur_path(direct.as_ref(), &out));
        }
        // Added up over a background: time by time.
        assert_eq!(with_gpu(|| blurred(&s0, &cam, format, 2, mixed, &blur, &frames, true)).is_some(), blend == "over");
    }
}

/// The lizard capture (examples/hornedlizard.json, cropped as there) and
/// its camera.
fn lizard() -> (Arc<Scene>, Camera) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hornedlizard.json");
    let graph = crate::graph::Graph::from_file(&path).expect("the example");
    let mut e = crate::graph::Evaluator::new();
    let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
    let scene = e.evaluate(&graph, "crop", &ctx).expect("splats").scene.clone().expect("a scene");
    let cam = e.evaluate(&graph, "camera", &ctx).expect("camera").scene.as_ref().expect("a camera").cameras[0].clone();
    (scene, cam)
}

/// A view of splats prepared on the GPU is the CPU's: nearly the same
/// splats (a borderline one may cull the other way in f32), the same
/// picture within D8's statistics, and the same lists every time.
#[test]
fn splats_prepared_on_the_gpu_render_as_on_the_cpu() {
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let (scene, cam) = lizard();
    for (format, degree) in [(Size { width: 320, height: 180 }, 3u8), (Size { width: 200, height: 300 }, 1)] {
        let cpu = super::super::splats::project_all(&scene.splats, &cam, format, degree).unwrap();
        let gpu = with_gpu(|| super::splat_view::project_all(&scene.splats, &cam, format, degree)).expect("on the GPU");
        let (a, b) = (cpu.splats.len() as f64, gpu.splats.len() as f64);
        assert!((a - b).abs() <= a * 1e-3 && a > 1000.0, "{a} splats on the CPU, {b} on the GPU");
        let again = with_gpu(|| super::splat_view::project_all(&scene.splats, &cam, format, degree)).unwrap();
        assert!(again.order == gpu.order && again.ranges == gpu.ranges, "the same lists every time");
        let cpu_img = crate::gpu::cpu_only(|| super::super::render_passes(&scene, &cam, format, 1, None, Passes::DEPTH));
        // (A new camera, a hair moved, so the view isn't one the CPU kept.)
        let moved = Camera { near: cam.near * (1.0 + 1e-9), ..cam.clone() };
        let gpu_img = with_gpu(|| super::super::render_passes(&scene, &moved, format, 1, None, Passes::DEPTH));
        for (name, mean, off) in differences(&cpu_img, &gpu_img) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "{name}: mean {mean:.2e}, {:.3} % of values off", off * 100.0);
        }
    }
}

/// Splats blended on the GPU (their view prepared there too) match the
/// CPU's: alone, and mixed in depth order with surfaces (a see-through
/// card, a holdout), every pass.
#[test]
fn splats_blend_on_the_gpu_as_on_the_cpu() {
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let (lizard, cam) = lizard();
    let (busy_scene, _) = scene_of(&busy(PERSPECTIVE));
    let mut mixed = (*lizard).clone();
    mixed.extend(&busy_scene);
    let format = Size { width: 240, height: 140 };
    for (scene, samples) in [(&*lizard, 1), (&mixed, 2)] {
        let passes = Passes::from_names(Passes::NAMES);
        let cpu = crate::gpu::cpu_only(|| super::super::render_passes(scene, &cam, format, samples, None, passes));
        let out = with_gpu(|| super::super::render_passes(scene, &cam, format, samples, None, passes));
        // (On the GPU: its view is the GPU's.)
        assert!(with_gpu(|| super::super::splats::prepare(&scene.splats, &cam, format)).is_some_and(|p| p.gpu().is_some()));
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "{} surfaces, {name}: mean {mean:.2e}, {:.3} % of values off", scene.cards.len(), off * 100.0);
        }
        // The splats' passes are there: P, N and id where the splats are.
        let id = out.plane("id").unwrap();
        assert!(id.data.iter().filter(|v| **v > 0.0).count() > format.width * format.height / 10);
    }
}

/// The 3D view's picture left on the GPU (render_preview_gpu) is
/// render_preview's: Solid, Lit with shadows, and splats; motion blur goes
/// the other way (None); each call a fresh image.
#[test]
fn the_3d_view_stays_on_the_gpu_and_matches() {
    use super::super::{render_preview, render_preview_gpu, Preview};
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let format = Size { width: 240, height: 140 };
    let (busy_scene, cam) = scene_of(&busy_shadowed(PERSPECTIVE));
    let (lizard, lcam) = lizard();
    for (scene, cam, look) in [(&busy_scene, &cam, Preview::Solid), (&busy_scene, &cam, Preview::Lit { shadows: true }), (&lizard, &lcam, Preview::Solid)] {
        let cpu = crate::gpu::cpu_only(|| render_preview(scene, cam, format, look));
        let resident = with_gpu(|| render_preview_gpu(scene, cam, format, look, None)).expect("on the GPU");
        let out = resident.cpu().expect("read back");
        assert_eq!((out.format, out.window()), (format, IRect::frame(format)));
        let (_, mean, off) = differences(&cpu, &out)[0].clone();
        assert!(mean <= 1e-3 && off <= 0.005, "{look:?}: mean {mean:.2e}, {:.3} % off", off * 100.0);
        let again = with_gpu(|| render_preview_gpu(scene, cam, format, look, None)).unwrap();
        assert!(again.image.rgba != resident.image.rgba, "a fresh image each call");
    }
    let blur = super::super::Blur { times: vec![0.0, 0.5], frames: vec![(0, &*busy_scene, cam.clone())], jitter: false };
    assert!(with_gpu(|| render_preview_gpu(&busy_scene, &cam, format, Preview::Solid, Some(&blur))).is_none(), "motion blur: the other path");
}

/// Motion blur over moving splats (and a camera that moves) adds up on
/// the GPU, each time's view prepared there, as the CPU renders it time
/// by time; mixed with surfaces too.
#[test]
fn motion_blur_over_splats_adds_up_on_the_gpu() {
    if crate::gpu::test_device().is_none() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    // (A small generated capture: the CPU renders every time of the
    // reference anew.)
    let lizard = Arc::new(Scene { splats: vec![super::check::cloud(30_000)], ..Scene::default() });
    let (busy_scene, cam) = scene_of(&busy(PERSPECTIVE));
    let format = Size { width: 160, height: 90 };
    // The capture a frame later: moved and turned a little.
    let later_of = |scene: &Scene| {
        let mut s = scene.clone();
        s.splats = s.splats.iter().map(|sp| Arc::new(crate::splat::Splats { world: crate::geom::trs([0.15, 0.05, 0.0], [0.0, 4.0, 0.0], [1.0; 3]) * sp.world, ..(**sp).clone() })).collect();
        s
    };
    let mut mixed = (*lizard).clone();
    mixed.extend(&busy_scene);
    for (s0, samples) in [((*lizard).clone(), 1), (mixed, 2)] {
        let s1 = later_of(&s0);
        let later_cam = Camera { world: crate::geom::trs([0.05, 0.0, 0.0], [0.0; 3], [1.0; 3]) * cam.world, ..cam.clone() };
        let blur = super::super::Blur { times: super::super::Blur::times(0.0, 0.5, 6), frames: vec![(0, &s0, cam.clone()), (1, &s1, later_cam)], jitter: true };
        let frames: Vec<(i64, &Scene)> = vec![(0, &s0), (1, &s1)];
        let passes = Passes::from_names(["depth", "mattes"]);
        let mixed_passes = Passes { depth: false, ..passes };
        let direct = with_gpu(|| blurred(&s0, &cam, format, samples, mixed_passes, &blur, &frames, false));
        assert!(direct.is_some(), "on the GPU: {:?}", crate::gpu::last_error());
        let cpu = crate::gpu::cpu_only(|| super::super::render_blurred(&s0, &cam, format, samples, None, passes, None, Some(&blur)));
        let out = with_gpu(|| super::super::render_blurred(&s0, &cam, format, samples, None, passes, None, Some(&blur)));
        for (name, mean, off) in differences(&cpu, &out) {
            let (most_mean, most_off) = super::check::allowed(&name);
            assert!(mean <= most_mean && off <= most_off, "{} cards, {name}: mean {mean:.2e}, {:.3} % of values off; {}", s0.cards.len(), off * 100.0, blur_path(direct.as_ref(), &out));
        }
    }
}
