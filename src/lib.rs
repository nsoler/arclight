//! Arclight — an original node-graph image compositor.
//!
//! A float-linear, premultiplied-alpha compositing engine driven by a JSON node
//! graph. This is original work, not derived from any existing product.

pub mod anim;
#[cfg(unix)]
pub mod bridge;
pub mod color;
pub mod config;
pub mod dpx;
pub mod expr;
pub mod features;
pub mod geom;
pub mod graph;
pub mod image;
pub mod link;
pub mod media;
#[cfg(unix)]
pub mod mcp;
pub mod ops;
pub mod plugin;
pub mod render;
pub mod render3d;
pub mod scene;
pub mod session;
pub mod shape;
pub mod solve;
pub mod track;
