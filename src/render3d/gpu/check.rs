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
    scene_at(graph, 1)
}

/// [`scene_of`] at `frame`.
pub(super) fn scene_at(graph: &str, frame: i64) -> (Arc<Scene>, Camera) {
    let graph: Graph = graph.parse().expect("graph");
    let mut e = Evaluator::new();
    let ctx = EvalContext { frame, write_outputs: false };
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

/// [`busy`] with every light casting shadows (soft ones from the point
/// light's radius) but the see-through card casting none (its layered
/// shadow is [`layered`]'s to try).
pub(super) fn busy_shadowed(camera: &str) -> String {
    busy(camera)
        .replace(r#""op": "light_directional", "params": {"#, r#""op": "light_directional", "params": {"shadows": true, "#)
        .replace(r#""op": "light_point", "params": {"#, r#""op": "light_point", "params": {"shadows": true, "#)
        .replace(r#""op": "light_spot", "params": {"#, r#""op": "light_spot", "params": {"shadows": true, "shadow_softness": 2.0, "#)
        .replace(r#""op": "light_area", "params": {"#, r#""op": "light_area", "params": {"shadows": true, "#)
        .replace(r#""opacity": 0.4"#, r#""opacity": 0.4, "casts_shadows": false"#)
}

/// [`busy`] (its see-through card made opaque) with a checkerboard
/// projected onto everything from a camera up and to the side (Project
/// 3D: occluded, cropped with a soft edge), and a patch over it
/// from the shot's own camera onto the front card (stacked, a
/// see-through checkerboard Over by its alpha, no occlusion).
pub(super) fn projected(camera: &str) -> String {
    busy(camera).replace(r#""opacity": 0.4"#, r#""opacity": 1.0"#).replace(r#"{"id": "scene", "op": "scene""#, r#"{"id": "all", "op": "scene""#).replace(
        camera,
        &format!(
            r#"{{"id": "plate", "op": "checkerboard", "params": {{"width": 320, "height": 180, "size": 9}}}},
        {{"id": "patch", "op": "checkerboard", "params": {{"width": 160, "height": 90, "size": 20, "color_a": [0.6, 0.1, 0.1, 0.6], "color_b": [0, 0, 0, 0]}}}},
        {{"id": "proj", "op": "camera", "params": {{"translate": [1.5, 1.2, 4], "rotate": [-12, 18, 0], "focal": 30}}}},
        {{"id": "plated", "op": "project_3d", "inputs": ["all", "plate", "proj"], "params": {{"edge": 12}}}},
        {{"id": "scene", "op": "project_3d", "inputs": ["plated", "patch", "cam"], "params": {{"mode": "over", "occlusion": false, "group": "card"}}}},
        {camera}"#
        ),
    )
}

/// [`projected`]'s scene with a second plate (noise) thrown onto
/// everything from up and to the other side, right after the first: the
/// two blended into one layer by how squarely each faces the surfaces
/// (Fade 20° to 75°), a soft seam where they meet.
pub(super) fn blended(scene: &Scene) -> Scene {
    let mut s = scene.clone();
    let first = crate::ops::project3d::Projection { blend: true, fade_start: 20.0, fade_end: 75.0, ..s.projections[0].clone() };
    let camera = Camera { world: crate::geom::trs([-2.0, 1.0, 3.5], [-10.0, -24.0, 0.0], [1.0; 3]), ..first.camera.clone() };
    let format = first.image.format;
    let image = Arc::new(crate::gpu::check::noise(format, crate::image::IRect::frame(format), false, 7));
    let second = crate::ops::project3d::Projection { camera, image, ..first.clone() };
    s.projections[0] = first;
    s.projections.insert(1, second);
    s
}

/// Shadows through casters that let light through, over a floor: a card
/// at opacity 0.4, a cutout (a feathered ellipse: clear, opaque and part
/// way between), three cards at 0.5 stacked, eight at 0.8 (deeper than a
/// map's texel keeps: those pixels are the CPU's), an opaque cube among
/// them; lit by a sun, a soft spot, a point with a radius (penumbra) and
/// an area light, all casting. The texture `size` texels across, the
/// floor `floor` units wide (the benchmark makes both bigger).
pub(super) fn layered(size: usize, floor: f64) -> String {
    let c = size / 2;
    let card = |id: &str, tex: &str, at: [f64; 3], scale: f64, opacity: f64| {
        format!(r#"{{"id": "{id}", "op": "card", "inputs": ["{tex}"], "params": {{"translate": [{}, {}, {}], "rotate": [-90, 0, 0], "scale": [{scale}, {scale}, 1], "opacity": {opacity}, "shading": "lambert"}}}},"#, at[0], at[1], at[2])
    };
    let mut nodes = format!(
        r#"{{"id": "tex", "op": "checkerboard", "params": {{"width": {size}, "height": {size}, "size": {}}}}},
        {{"id": "blob", "op": "ellipse", "params": {{"width": {size}, "height": {size}, "center": [{c}, {c}], "size": [{}, {}], "feather": {}}}}},
        {{"id": "floor", "op": "card", "inputs": ["tex"], "params": {{"translate": [0, -1, -3], "rotate": [-90, 0, 0], "scale": [{floor}, {floor}, 1], "shading": "lambert"}}}},
        {{"id": "cube", "op": "cube", "params": {{"translate": [-0.6, -0.6, -4.2], "scale": [0.5, 0.5, 0.5], "shading": "lambert"}}}},"#,
        (size / 16).max(1),
        size * 3 / 4,
        size * 3 / 4,
        size / 6
    );
    let mut ids = vec!["floor".to_string(), "cube".to_string()];
    let mut add = |id: String, text: String| {
        nodes.push_str(&text);
        ids.push(id);
    };
    add("glass".into(), card("glass", "tex", [-2.0, 0.0, -2.5], 1.4, 0.4));
    add("cutout".into(), card("cutout", "blob", [0.0, 0.2, -2.5], 1.6, 1.0));
    for k in 0..3 {
        add(format!("st{k}"), card(&format!("st{k}"), "tex", [2.0, -0.6 + 0.35 * k as f64, -2.5], 1.2, 0.5));
    }
    for k in 0..8 {
        add(format!("deep{k}"), card(&format!("deep{k}"), "tex", [0.4, -0.75 + 0.12 * k as f64, -4.8], 1.6, 0.8));
    }
    let lights = [
        r#"{"id": "amb", "op": "light_ambient", "params": {"intensity": 0.15}},"#,
        r#"{"id": "sun", "op": "light_directional", "params": {"shadows": true, "rotate": [-65, 25, 0], "intensity": 0.6}},"#,
        r#"{"id": "spot", "op": "light_spot", "params": {"shadows": true, "shadow_softness": 2.0, "translate": [-1, 4, 0], "rotate": [-70, -10, 0], "penumbra": 8.0, "falloff": 1.0}},"#,
        r#"{"id": "pt", "op": "light_point", "params": {"shadows": true, "translate": [2.5, 3, -2], "radius": 0.3, "decay": 1.0}},"#,
        r#"{"id": "area", "op": "light_area", "params": {"shadows": true, "translate": [0, 3.5, -3], "rotate": [-90, 0, 0], "shape": "ellipse", "intensity": 0.8}},"#,
    ];
    for l in lights {
        let id = l.split('"').nth(3).unwrap().to_string();
        add(id, l.to_string());
    }
    // Looking down at the floor.
    const CAMERA: &str = r#"{"id": "cam", "op": "camera", "params": {"translate": [0, 3.2, 2.5], "rotate": [-38, 0, 0]}}"#;
    let inputs: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
    format!(
        r#"{{"project": {{"format": {{"width": 320, "height": 180}}, "fps": 24.0, "first_frame": 1, "last_frame": 10}}, "nodes": [
        {nodes}
        {{"id": "scene", "op": "scene", "inputs": [{}]}},
        {CAMERA}
        ]}}"#,
        inputs.join(", ")
    )
}

/// `graph` (a [`busy`] scene) with its sphere moving across, frames 1 to
/// 10.
pub(super) fn moving(graph: &str) -> String {
    graph.replace(r#""op": "sphere", "inputs": ["tex"], "params": {"#, r#""op": "sphere", "inputs": ["tex"], "params": {"translate": {"keys": [[1, [-0.6, 0, 0]], [10, [0.6, 0.2, 0]]]}, "#)
}

/// `n` particles scattered in front of the default camera, every size from
/// sub-pixel up, turned, aged, in many colors (the same every time).
pub(super) fn scattered(n: usize) -> Vec<crate::particles::Particle> {
    let mut h = 12345u32;
    let mut r = move || {
        h ^= h << 13;
        h ^= h >> 17;
        h ^= h << 5;
        (h % 100_000) as f64 / 100_000.0
    };
    (0..n)
        .map(|i| {
            let position = crate::geom::Vec3::new(r() * 4.0 - 2.0, r() * 2.4 - 1.2, -2.0 - r() * 6.0);
            let a = (0.3 + 0.7 * r()) as f32;
            crate::particles::Particle {
                id: i as u64,
                position,
                velocity: crate::geom::Vec3::new(r() - 0.5, r(), 0.0),
                accel: crate::geom::Vec3::zeros(),
                age: r() * 40.0,
                life: 50.0,
                size: 0.005 + r() * 0.3,
                color: [r() as f32 * a, r() as f32 * a, r() as f32 * a, a],
                rotation: r() * 360.0,
                spin: 0.0,
                axis: crate::geom::Vec3::z(),
                start_size: 0.1,
                source: crate::particles::NO_SOURCE,
            }
        })
        .collect()
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
/// orthographic, and as the 3D view shades it; shadows, through see-through
/// casters too.
pub(crate) fn rows(gpu: &Gpu) -> Vec<CheckRow> {
    let hd = Size { width: 1920, height: 1080 };
    let (scene, cam) = scene_of(&busy(PERSPECTIVE));
    let (flat, ortho) = scene_of(&busy(ORTHO));
    let (shaded, _) = scene_of(&busy_shadowed(PERSPECTIVE));
    let (see, see_cam) = scene_of(&layered(256, 12.0));
    let (thrown, _) = scene_of(&projected(PERSPECTIVE));
    let mixed = blended(&thrown);
    let all = Passes::from_names(Passes::NAMES);
    vec![
        row(gpu, "3D render, lit, AA 3×3", &scene, &cam, hd, 3, Passes::DEPTH, false),
        row(gpu, "3D render, every pass", &scene, &cam, hd, 3, all, false),
        row(gpu, "3D render, orthographic", &flat, &ortho, hd, 2, Passes::DEPTH, false),
        row(gpu, "3D render, shadows (4 lights)", &shaded, &cam, hd, 2, Passes::from_names(["depth", "shadow", "lights"]), false),
        row(gpu, "3D render, see-through shadows", &see, &see_cam, hd, 2, Passes::from_names(["depth", "shadow"]), false),
        row(gpu, "3D render, projections (Project 3D)", &thrown, &cam, hd, 2, Passes::DEPTH, false),
        row(gpu, "3D render, projections blended by facing", &mixed, &cam, hd, 2, Passes::DEPTH, false),
        // (No shadows: slow on the CPU at every time; the shadows row has
        // them, and the tests blur with shadows.)
        blur_row(Size { width: 960, height: 540 }, "3D render, motion blur 6", &moving(&busy(PERSPECTIVE))),
        blur_row(Size { width: 960, height: 540 }, "3D render, motion blur 6, projections", &moving(&projected(PERSPECTIVE))),
        sprites_row(Size { width: 960, height: 540 }),
        splats_row(hd),
        instances_row(gpu, hd, false),
        instances_row(gpu, hd, true),
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
    let gpu_draw = || draw(gpu, &surfs, lights, &scene.points, cam, format, samples, passes, None, None, None);
    // Once each first: the GPU builds its pipelines, and textures go up.
    let _ = gpu_draw();
    let (cpu, cpu_ms) = timed(cpu_draw);
    let (out, gpu_ms) = timed(gpu_draw);
    judge(case, size, &cpu, out, cpu_ms, gpu_ms)
}

/// The motion-blur row: the busy scene, its sphere moving, 6
/// times jittered, at `size` (small: the CPU takes a while), through
/// Scanline Render's own entry point (which adds the times up on the GPU).
/// Motion blur over 6 times of `graph` (its sphere moving) at `hd`, as row
/// `case`.
fn blur_row(hd: Size, case: &str, graph: &str) -> CheckRow {
    let ((s0, cam), (s1, _)) = (scene_at(graph, 4), scene_at(graph, 5));
    let blur = super::super::Blur { times: super::super::Blur::times(0.0, 0.5, 6), frames: vec![(0, &*s0, cam.clone()), (1, &*s1, cam.clone())], jitter: true };
    let render = || super::super::render_blurred(&s0, &cam, hd, 2, None, Passes::DEPTH, None, Some(&blur));
    let _ = render();
    let (cpu, cpu_ms) = timed(|| crate::gpu::cpu_only(render));
    let (out, gpu_ms) = timed(render);
    judge(case, format!("{}x{}", hd.width, hd.height), &cpu, Ok(Some(out)), cpu_ms, gpu_ms)
}

/// The sprites row: 2000 big textured particle sprites laid over each
/// other, at `size` (where the CPU is slowest and the GPU takes them).
fn sprites_row(size: Size) -> CheckRow {
    use crate::particles_render::{render_sprites, Blend, Outline, Pick, Sprites, Texture};
    let frames: Vec<Arc<Image>> = (0..3).map(|k| Arc::new(crate::gpu::check::noise(Size { width: 64, height: 64 }, crate::image::IRect::new(0, 0, 64, 64), false, 7 + k))).collect();
    let sprites = Sprites { outline: Outline::Square, texture: Some(Texture { images: frames, pick: Pick::ByAge }), scale: 3.0, opacity: 0.6, opacity_end: 0.1, blend: Blend::Over };
    let particles = scattered(2000);
    let cam = Camera::default();
    let render = || render_sprites(&particles, &sprites, &cam, size, None);
    let _ = render();
    let (cpu, cpu_ms) = timed(|| crate::gpu::cpu_only(render));
    let (out, gpu_ms) = timed(render);
    judge("particle sprites, big, textured", format!("{}x{}", size.width, size.height), &cpu, Ok(Some(out)), cpu_ms, gpu_ms)
}

/// `n` Gaussian splats in a cloud in front of the default camera (SH
/// degree 1, every size and turn, the same every time).
pub(super) fn cloud(n: usize) -> Arc<crate::splat::Splats> {
    let mut h = 777u32;
    let mut r = move || {
        h ^= h << 13;
        h ^= h >> 17;
        h ^= h << 5;
        (h % 100_000) as f32 / 100_000.0
    };
    let mut parts = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n * 12));
    for _ in 0..n {
        parts.0.push([r() * 4.0 - 2.0, r() * 2.4 - 1.2, -3.0 - r() * 4.0]);
        let s = 0.003 + r() * r() * 0.08;
        parts.1.push([s, s * (0.3 + r()), s * (0.3 + r())]);
        parts.2.push([r() - 0.5, r() - 0.5, r() - 0.5, r() - 0.5]);
        parts.3.push(0.2 + 0.8 * r());
        for _ in 0..12 {
            parts.4.push(r() * 2.0 - 1.0);
        }
    }
    Arc::new(crate::splat::Splats {
        positions: Arc::new(parts.0),
        scales: Arc::new(parts.1),
        rotations: Arc::new(parts.2),
        opacity: Arc::new(parts.3),
        sh: Arc::new(parts.4),
        sh_degree: 1,
        world: crate::geom::Mat4::identity(),
        holdout: false,
        node: "cloud".into(),
    })
}

/// The splats row: 200 000 splats (a capture's worth) through Scanline
/// Render, the view prepared and blended on the GPU.
fn splats_row(size: Size) -> CheckRow {
    let scene = Scene { splats: vec![cloud(200_000)], ..Scene::default() };
    let cam = Camera::default();
    let render = |turn: f64| {
        // (Each render from a camera turned a little: a view not kept.)
        let cam = Camera { world: crate::geom::trs([0.0; 3], [0.0, turn, 0.0], [1.0; 3]), ..cam.clone() };
        super::super::render_passes(&scene, &cam, size, 1, None, Passes::DEPTH)
    };
    let _ = render(0.1);
    let (cpu, cpu_ms) = timed(|| crate::gpu::cpu_only(|| render(0.2)));
    let (out, gpu_ms) = timed(|| render(0.2));
    judge("3D render, 200k splats", format!("{}x{}", size.width, size.height), &cpu, Ok(Some(out)), cpu_ms, gpu_ms)
}

/// A row from a CPU render and the GPU's (`out`), and their times.
/// Instances over a floor (`n` cards and a twentieth as many spheres, lit,
/// in a grid going away from the camera): `cards` and `spheres` copies.
pub(super) fn instanced_field(n: usize) -> (Scene, Camera) {
    use crate::geo::{Instances, Prototype};
    use crate::geom::{trs, Mat4};
    use crate::mesh::{Material, Shading};
    let lambert = Material { shading: Shading::Lambert, ..Material::default() };
    let floor = crate::scene::Model::new(Arc::new(crate::mesh::plane([40.0, 40.0], [1, 1])), trs([0.0, -1.0, -16.0], [-90.0, 0.0, 0.0], [1.0; 3]), lambert.clone());
    let tex = Arc::new(crate::image::Image::filled(16, 16, [0.6, 0.7, 0.3, 1.0]));
    let card = crate::scene::Card { material: Some(lambert.clone()), ..crate::scene::Card::new(tex, Mat4::identity(), [0.25, 0.4]) };
    let ball = crate::scene::Model::new(Arc::new(crate::mesh::sphere(0.15, [16, 8])), Mat4::identity(), lambert);
    let grid = |count: usize, salt: f64| -> Vec<Mat4> {
        let side = (count as f64).sqrt().ceil() as usize;
        (0..count).map(|i| trs([(i % side) as f64 / side as f64 * 12.0 - 6.0 + salt, -0.8, -1.5 - (i / side) as f64 / side as f64 * 28.0], [0.0, i as f64 * 37.0 % 360.0, 0.0], [1.0; 3])).collect()
    };
    let cards = Instances { prototypes: vec![Prototype::Card(card)], which: vec![0; n], worlds: grid(n, 0.0), node: "cards".into(), ..Instances::default() };
    let balls = Instances { prototypes: vec![Prototype::Model(ball)], which: vec![0; n / 20], worlds: grid(n / 20, 0.05), node: "balls".into(), ..Instances::default() };
    let sun = crate::shade::Light { node: "sun".into(), ..crate::shade::Light::new(crate::shade::LightKind::Directional, trs([0.0; 3], [-50.0, 30.0, 0.0], [1.0; 3])) };
    let scene = Scene { meshes: vec![floor], instances: vec![cards, balls], lights: vec![sun], ..Scene::default() };
    let cam = Camera { focal: 24.0, world: trs([0.0, 0.6, 2.5], [-8.0, 0.0, 0.0], [1.0; 3]), ..Camera::default() };
    (scene, cam)
}

/// The instancing rows: 10k cards and 500 spheres drawn instanced, against
/// the CPU drawing each as a surface of its own; with `shadows`, the sun
/// casting shadows and a point light among them too.
fn instances_row(gpu: &Gpu, size: Size, shadows: bool) -> CheckRow {
    let (mut scene, cam) = instanced_field(10_000);
    if shadows {
        scene.lights[0].shadows = true;
        let mut lamp = crate::shade::Light::new(crate::shade::LightKind::Point, crate::geom::trs([0.5, 0.0, -6.0], [0.0; 3], [1.0; 3]));
        lamp.shadows = true;
        lamp.node = "lamp".into();
        scene.lights.push(lamp);
    }
    let passes = Passes::from_names(["depth", "id"]);
    let all = super::super::surfaces(&scene);
    let plain = super::super::plain_surfaces(&scene);
    let cpu_draw = || crate::gpu::cpu_only(|| super::super::draw_passes(&all, &scene.lights, &[], &cam, size, 2, passes, None, None, None));
    let gpu_draw = || -> anyhow::Result<Option<Image>> {
        let Some(mut plan) = super::instances::Plan::new(&plain, &scene.instances, false, &cam) else { return Ok(None) };
        plan.plan_maps(&scene.lights);
        super::draw_with(gpu, &plan.surfs, Some(&plan), &scene.lights, &[], &cam, size, 2, passes, None, None, None)
    };
    let _ = gpu_draw();
    let (cpu, cpu_ms) = timed(cpu_draw);
    let (out, gpu_ms) = timed(gpu_draw);
    let case = if shadows { "3D render, instances, shadows" } else { "3D render, 10k instanced cards" };
    judge(case, format!("{}x{}", size.width, size.height), &cpu, out, cpu_ms, gpu_ms)
}

fn judge(case: &str, size: String, cpu: &Image, out: anyhow::Result<Option<Image>>, cpu_ms: f64, gpu_ms: f64) -> CheckRow {
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
    for (name, mean, off) in differences(cpu, &out) {
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
