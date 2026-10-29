//! Arclight — an original node-graph image compositor.
//!
//! A float-linear, premultiplied-alpha compositing engine driven by a JSON node
//! graph. This is original work, not derived from any existing product.

pub mod anim;
pub mod bundle;
pub mod camtrack;
#[cfg(unix)]
pub mod bridge;
pub mod color;
pub mod config;
pub mod dpx;
pub mod expr;
pub mod features;
pub mod geom;
pub mod gltf;
pub mod graph;
pub mod image;
pub mod link;
pub mod media;
pub mod mesh;
#[cfg(unix)]
pub mod mcp;
pub mod nodal;
pub mod ops;
pub mod plugin;
pub mod render;
pub mod render3d;
pub mod scene;
pub mod session;
pub mod shade;
pub mod sfm;
pub mod shape;
pub mod solve;
pub mod track;
