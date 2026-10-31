//! ArclightVFX Compositor — an original node-graph image compositor.
//!
//! A float-linear, premultiplied-alpha compositing engine driven by a JSON node
//! graph. This is original work, not derived from any existing product.

/// The product's name, and its short form (windows, menus). Commands,
/// crates, folders and environment variables keep "arclight".
pub const PRODUCT: &str = "ArclightVFX Compositor";
pub const SHORT_NAME: &str = "ArclightVFX";
/// How far along it is, shown beside the version.
pub const STAGE: &str = "beta";

pub mod alembic;
pub mod anim;
pub mod audio;
pub mod bundle;
pub mod camtrack;
#[cfg(unix)]
pub mod bridge;
pub mod color;
pub mod config;
pub mod crash;
pub mod dpx;
pub mod expr;
pub mod features;
pub mod geo;
pub mod geom;
pub mod gltf;
pub mod gpu;
pub mod graph;
pub mod image;
pub mod light2d;
pub mod link;
pub mod media;
pub mod mesh;
#[cfg(unix)]
pub mod mcp;
pub mod nodal;
pub mod ogawa;
pub mod ops;
pub mod particle_nodes;
pub mod particles;
pub mod particles_render;
pub mod plugin;
#[cfg(unix)]
pub mod plugin_host;
pub mod render;
pub mod render3d;
pub mod scene;
pub mod session;
pub mod shade;
pub mod sfm;
pub mod shape;
pub mod splat;
pub mod solve;
pub mod track;
