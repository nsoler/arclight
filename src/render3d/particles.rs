//! Particles in a render: object looks become meshes before it, points
//! and sprites are drawn over it after, hidden where its depth is nearer.
//! With a motion pass, points and sprites move by their velocities (a
//! frame's worth, seen through the next frame's camera).

use crate::geom::Camera;
use crate::image::{Image, Size};
use crate::particle_nodes::{Look, ParticleStream};
use crate::particles::Particle;
use crate::particles_render::{self, Blend, Sprites};
use crate::scene::{Model, Scene};

/// `scene` with each object-look particle stream's objects as meshes (each
/// tinted by its particle's color), or None when it has none.
pub(super) fn with_objects(scene: &Scene) -> Option<Scene> {
    if !scene.particles.iter().any(|s| matches!(s.look, Look::Objects { .. })) {
        return None;
    }
    let mut out = scene.clone();
    for stream in &scene.particles {
        let Look::Objects { meshes, materials, orient, scale } = &stream.look else { continue };
        for i in particles_render::instances(&stream.particles(), meshes, *orient, *scale) {
            let mut material = materials[i.mesh].clone();
            material.color = std::array::from_fn(|c| material.color[c] * i.color[c]);
            material.opacity *= i.color[3];
            out.meshes.push(Model { node: stream.node.clone(), ..Model::new(meshes[i.mesh].clone(), i.world, material) });
        }
    }
    Some(out)
}

/// `scene`'s point and sprite particle streams drawn over `render` (a
/// render of the rest through `cam`, in a frame of `format`, with a depth
/// plane), where its depth doesn't hide them; its depth is the nearest
/// sprite's where one shows. With `next_cam` (the next frame's camera)
/// and a motion pass in `render`, the sprites' motion goes over it too.
pub(super) fn draw(scene: &Scene, cam: &Camera, format: Size, mut render: Image, next_cam: Option<&Camera>) -> Image {
    for stream in scene.particles.iter().filter(|s| !matches!(s.look, Look::Objects { .. })) {
        let particles = stream.particles();
        if let Some(next_cam) = next_cam.filter(|_| render.plane("motion.u").is_some()) {
            motion(stream, &particles, cam, next_cam, format, &mut render);
        }
        let drawn = particles_render::render_sprites(&particles, &stream.sprites(), cam, format, Some(&render));
        // Back into the render (which may reach past the frame, and has
        // other planes): over the frame.
        let window = render.window();
        let depth = drawn.plane("depth").map(|p| p.data.clone());
        let mut new_depth = render.plane("depth").map(|p| p.data.clone());
        for y in 0..format.height {
            for x in 0..format.width {
                let (fx, fy) = (x as i32, y as i32);
                if !window.contains(fx, fy) {
                    continue;
                }
                let i = (fy - render.y0) as usize * render.width + (fx - render.x0) as usize;
                render.pixels[i] = drawn.pixels[y * format.width + x];
                if let (Some(d), Some(nd)) = (&depth, new_depth.as_mut()) {
                    nd[i] = d[y * format.width + x];
                }
            }
        }
        if let Some(nd) = new_depth {
            *render.plane_mut("depth") = nd;
        }
    }
    render
}

/// Lay `particles`' motion (pixels each moves by the next frame: where it
/// is a frame on, along its velocity and acceleration, through `next_cam`,
/// less where it is through `cam`)
/// over `render`'s motion pass as their sprites lie (untextured, laid over
/// each other), hidden where its depth is nearer. Where a sprite is only
/// partly over nothing, its motion is its own; over a surface, the two are
/// mixed by its coverage.
fn motion(stream: &ParticleStream, particles: &[Particle], cam: &Camera, next_cam: &Camera, format: Size, render: &mut Image) {
    let fps = stream.fps();
    let colored: Vec<Particle> = particles
        .iter()
        .map(|p| {
            let m = match (cam.project(&p.position, format), next_cam.project(&p.moved(1.0 / fps).0, format)) {
                (Some((a, _)), Some((b, _))) => [(b[0] - a[0]) as f32, (b[1] - a[1]) as f32],
                _ => [0.0; 2],
            };
            let a = p.color[3];
            Particle { color: [m[0] * a, m[1] * a, 0.0, a], ..p.clone() }
        })
        .collect();
    // The surfaces' motion (opaque where there is a surface) and depth.
    let mut under = Image::new(format.width, format.height);
    let mut depth = vec![0.0f32; format.width * format.height];
    for y in 0..format.height {
        for x in 0..format.width {
            let (fx, fy) = (x as i32, y as i32);
            let d = render.plane_at("depth", fx, fy);
            under.pixels[y * format.width + x] = [render.plane_at("motion.u", fx, fy), render.plane_at("motion.v", fx, fy), 0.0, if d > 0.0 { 1.0 } else { 0.0 }];
            depth[y * format.width + x] = d;
        }
    }
    *under.plane_mut("depth") = depth;
    let flat = Sprites { texture: None, blend: Blend::Over, ..stream.sprites() };
    let drawn = particles_render::render_sprites(&colored, &flat, cam, format, Some(&under));
    let window = render.window();
    let (mut u, mut v) = (render.plane("motion.u").map(|p| p.data.clone()).unwrap_or_default(), render.plane("motion.v").map(|p| p.data.clone()).unwrap_or_default());
    for y in 0..format.height {
        for x in 0..format.width {
            let (fx, fy) = (x as i32, y as i32);
            let px = drawn.pixels[y * format.width + x];
            if !window.contains(fx, fy) || px[3] <= 0.0 {
                continue;
            }
            let i = (fy - render.y0) as usize * render.width + (fx - render.x0) as usize;
            (u[i], v[i]) = (px[0] / px[3], px[1] / px[3]);
        }
    }
    *render.plane_mut("motion.u") = u;
    *render.plane_mut("motion.v") = v;
}

#[cfg(test)]
mod tests {
    use super::super::{render, render_passes, Passes};
    use crate::geom::{self, Camera};
    use crate::image::{Image, Size};
    use crate::particle_nodes::{eval, Context, Output, ParticleState, ParticleStream};
    use crate::scene::{Card, Scene};
    use serde_json::{json, Value};
    use std::sync::Arc;

    const FMT: Size = Size { width: 64, height: 32 };

    /// Particle node `op` with `params`, after `upstream`.
    fn node(op: &str, params: Value, inputs: &[Option<&Image>], upstream: Option<&ParticleStream>, state: &mut ParticleState) -> ParticleStream {
        let cx = Context { node: op, frame: 1, fps: 24.0, format: FMT, own: Arc::new(move |_| params.clone()), inputs, keys: &[], upstream };
        match eval(op, cx, state).unwrap() {
            Output::Stream(s) => s,
            Output::Image(_) => unreachable!(),
        }
    }

    /// A red particle 1 across, 5 in front of the camera.
    fn one(state: &mut ParticleState) -> ParticleStream {
        node("particle_emitter", json!({ "burst": 1, "rate": 0, "translate": [0, 0, -5], "size": 1, "speed": 0, "color": [1, 0, 0, 1] }), &[], None, state)
    }

    #[test]
    fn sprites_hide_behind_nearer_surfaces() {
        let mut state = ParticleState::default();
        let cam = Camera { focal: 18.0, ..Camera::default() };
        // A blue card over the left half of the view, nearer than the particle.
        let card = Card::new(Arc::new(Image::filled(2, 2, [0.0, 0.0, 1.0, 1.0])), geom::trs([-1.5, 0.0, -3.0], [0.0; 3], [1.0; 3]), [3.0, 4.0]);
        let scene = Scene { cards: vec![card], particles: vec![one(&mut state)], ..Scene::default() };
        let img = render(&scene, &cam, FMT, 1, None);
        assert_eq!(img.at(30, 16), [0.0, 0.0, 1.0, 1.0], "behind the card");
        assert_eq!(img.at(33, 16), [1.0, 0.0, 0.0, 1.0], "in the open");
        assert!((img.plane_at("depth", 33, 16) - 5.0).abs() < 1e-4 && (img.plane_at("depth", 30, 16) - 3.0).abs() < 1e-4);
        // Without a depth pass asked for, none comes out (one was made for the sprites).
        let bare = render_passes(&scene, &cam, FMT, 1, None, Passes::default());
        assert!(bare.plane("depth").is_none());
        assert_eq!(bare.at(33, 16), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn fast_particles_streak_and_have_motion_vectors() {
        use super::super::{render_blurred, render_motion, Blur};
        let mut state = ParticleState::default();
        // A red particle 0.3 across (1.9 px) 5 in front of the camera, going
        // right 37.5 units a second: 10 px a frame at 24 fps.
        let p = json!({ "burst": 1, "rate": 0, "translate": [0, 0, -5], "size": 0.3, "speed": 37.5, "direction": [1, 0, 0], "color": [1, 0, 0, 1] });
        let stream = node("particle_emitter", p, &[], None, &mut state);
        let at = stream.particles()[0].position;
        let cam = Camera { focal: 18.0, ..Camera::default() };
        let (q, _) = cam.project(&at, FMT).unwrap();
        let (cx, cy) = (q[0].floor() as i32, q[1].floor() as i32);
        let scene = Scene { particles: vec![stream], ..Scene::default() };
        // Its motion: 10 px right where it shows (and nothing where it doesn't).
        let passes = Passes { motion: true, ..Passes::DEPTH };
        let out = render_motion(&scene, &cam, FMT, 1, None, passes, Some((&Scene::default(), &cam)));
        let (u, v) = (out.plane_at("motion.u", cx, cy), out.plane_at("motion.v", cx, cy));
        assert!((u - 10.0).abs() < 1e-3 && v.abs() < 1e-3, "{u} {v}");
        assert_eq!(out.plane_at("motion.u", cx, cy + 4), 0.0);
        // As objects, the same move (seen on the cube's front, nearer: 32
        // px a unit at 1 away, 1.5625 units a frame).
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let cube = crate::ops::eval("cube", &json!({ "shading": "unlit" }), &[None], &ctx).unwrap();
        let cubes = node("particle_render", json!({ "look": "objects", "orient": "none" }), &[None, None, Some(&cube)], Some(&scene.particles[0]), &mut state);
        let out = render_motion(&Scene { particles: vec![cubes], ..Scene::default() }, &cam, FMT, 1, None, passes, Some((&Scene::default(), &cam)));
        let (u, d) = (out.plane_at("motion.u", cx, cy), out.plane_at("depth", cx, cy));
        assert!(d > 4.5 && (u - 1.5625 * 32.0 / d).abs() < 1e-3, "{u} at {d}");
        // Blurred over half a frame: a streak 5 px long along its way, as
        // bright in all (up to how a sprite's soft edge falls on the
        // pixels); none across it.
        let sharp = render(&scene, &cam, FMT, 4, None);
        let blur = Blur { times: Blur::times(-0.25, 0.5, 16), frames: vec![(0, &scene, cam.clone())], jitter: true };
        let smear = render_blurred(&scene, &cam, FMT, 4, None, Passes::NONE, None, Some(&blur));
        let red = |img: &Image, x: i32, y: i32| img.at(x, y)[0];
        let row: Vec<f32> = (cx - 5..=cx + 5).map(|x| red(&smear, x, cy)).collect();
        assert!(red(&sharp, cx + 2, cy) == 0.0 && red(&smear, cx + 2, cy) > 0.1 && red(&smear, cx - 2, cy) > 0.1, "{row:?}");
        assert!(red(&smear, cx, cy + 3) == 0.0 && red(&smear, cx + 5, cy) == 0.0);
        let total = |img: &Image| img.pixels.iter().map(|p| p[0]).sum::<f32>();
        assert!((total(&sharp) - total(&smear)).abs() < 0.06 * total(&sharp), "{} vs {}", total(&sharp), total(&smear));
    }

    #[test]
    fn objects_render_as_meshes_at_the_particles() {
        let mut state = ParticleState::default();
        let src = node("particle_emitter", json!({ "burst": 1, "rate": 0, "translate": [0, 0, -5], "size": 1, "speed": 0, "color": [1, 1, 0, 1] }), &[], None, &mut state);
        let ctx = crate::ops::EvalContext { frame: 1, write_outputs: false };
        let cube = crate::ops::eval("cube", &json!({ "shading": "unlit", "color": [0, 1, 0, 1] }), &[None], &ctx).unwrap();
        let look = node("particle_render", json!({ "look": "objects", "orient": "none" }), &[None, None, Some(&cube)], Some(&src), &mut state);
        let scene = Scene { particles: vec![look], ..Scene::default() };
        let img = render(&scene, &Camera { focal: 18.0, ..Camera::default() }, FMT, 1, None);
        // A green unit cube 5 away (scaled by the particle's size, 1 here
        // as Size scale is), tinted by the particle's yellow: still green.
        assert_eq!(img.at(32, 16), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(img.at(2, 2)[3], 0.0);
    }
}
