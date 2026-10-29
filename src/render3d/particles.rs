//! Particles in a render: object looks become meshes before it, points
//! and sprites are drawn over it after, hidden where its depth is nearer.

use crate::geom::Camera;
use crate::image::{Image, Size};
use crate::particle_nodes::Look;
use crate::particles_render;
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
/// sprite's where one shows.
pub(super) fn draw(scene: &Scene, cam: &Camera, format: Size, mut render: Image) -> Image {
    for stream in scene.particles.iter().filter(|s| !matches!(s.look, Look::Objects { .. })) {
        let drawn = particles_render::render_sprites(&stream.particles(), &stream.sprites(), cam, format, Some(&render));
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
