//! Write Geo: a scene's geometry to a file other 3D tools read, Wavefront
//! OBJ (with its MTL) or glTF 2.0 (`.gltf` with a `.bin` beside it, or one
//! `.glb`), chosen by the file's extension.
//!
//! - Meshes, cards (a textured quad each), instances, point sets and
//!   curves (each kind can be left out); Group narrows them to its nodes.
//! - OBJ has no placements: everything is written where it is in the
//!   scene. glTF keeps each object's placement on its node, and copies of
//!   one prototype share its mesh (Instances instance), or each gets its
//!   own (realize).
//! - Materials: color, opacity, specular, roughness, emission and the
//!   color texture (a PNG written beside the file, or inside a `.glb`).
//! - Attributes: Cd as vertex colors (OBJ's `v x y z r g b`, glTF's
//!   COLOR_0); in glTF the other point attributes too, as `_name`
//!   attributes (Read Geo reads them back). Face attributes aren't
//!   written (neither format has them).

use crate::geo::{self, Attr, Attrs, Prototype};
use crate::geom::{self, Mat4, Vec3};
use crate::image::Image;
use crate::mesh::{Material, Mesh, Shading};
use crate::scene::{Card, Scene};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Something to write, in its own space.
#[derive(Clone)]
enum Shape {
    Mesh { mesh: Arc<Mesh>, material: Material, texture: Option<Arc<Image>> },
    Points { positions: Vec<Vec3>, attrs: Attrs },
    /// Polylines (and whether each closes).
    Lines { curves: Vec<Vec<Vec3>>, closed: Vec<bool> },
}

/// A shape placed: its name, its placement and, for a copy, its tint (Cd).
struct Item {
    name: String,
    world: Mat4,
    shape: usize,
    tint: Option<[f32; 4]>,
}

/// What a Write Geo writes: shapes, and the items placing them.
#[derive(Default)]
struct Export {
    shapes: Vec<Shape>,
    items: Vec<Item>,
}

/// A card as a quad mesh (its image the texture, v up, facing +Z).
fn card_shape(c: &Card) -> Shape {
    let [w, h] = c.size.map(|v| v / 2.0);
    let mesh = Mesh {
        positions: vec![Vec3::new(-w, h, 0.0), Vec3::new(w, h, 0.0), Vec3::new(w, -h, 0.0), Vec3::new(-w, -h, 0.0)],
        normals: vec![Vec3::z(); 4],
        uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        triangles: vec![[3, 2, 1], [3, 1, 0]],
        ..Mesh::default()
    };
    let mut material = c.material.clone().unwrap_or(Material { shading: Shading::Unlit, ..Material::default() });
    material.opacity *= c.opacity;
    Shape::Mesh { mesh: Arc::new(mesh), material, texture: Some(c.image.clone()) }
}

impl Export {
    fn add(&mut self, shape: Shape) -> usize {
        self.shapes.push(shape);
        self.shapes.len() - 1
    }

    /// What of `scene` Write Geo's `params` ask for.
    fn of(scene: &Scene, params: &Value) -> Export {
        let include: Vec<&str> = params["include"].as_array().map_or(vec!["meshes", "cards", "instances", "points", "curves"], |a| a.iter().filter_map(Value::as_str).collect());
        let wants = |k: &str| include.contains(&k);
        let group = params["group"].as_str().unwrap_or("").trim();
        let take = |node: &str| group.is_empty() || geo::glob(group, node);
        let realize = params["instances"].as_str() == Some("realize");
        let name = |node: &str, inner: &str, k: usize| match (node.is_empty(), inner.is_empty()) {
            (false, true) => node.to_string(),
            (false, false) => format!("{node}.{inner}"),
            (true, false) => inner.to_string(),
            (true, true) => format!("object{k}"),
        };
        let mut e = Export::default();
        if wants("meshes") {
            for m in scene.meshes.iter().filter(|m| take(&m.node)) {
                let shape = e.add(Shape::Mesh { mesh: m.mesh.clone(), material: m.material.clone(), texture: m.texture.clone() });
                e.items.push(Item { name: name(&m.node, &m.name, shape), world: m.world, shape, tint: None });
            }
        }
        if wants("cards") {
            for c in scene.cards.iter().filter(|c| take(&c.node)) {
                let shape = e.add(card_shape(c));
                e.items.push(Item { name: name(&c.node, "", shape), world: c.world, shape, tint: None });
            }
        }
        if wants("instances") {
            for g in scene.instances.iter().filter(|g| take(&g.node)) {
                // Each prototype as a shape (splat captures aren't written),
                // with its own placement.
                let protos: Vec<Option<(Shape, Mat4)>> = g
                    .prototypes
                    .iter()
                    .map(|p| match p {
                        Prototype::Model(m) => Some((Shape::Mesh { mesh: m.mesh.clone(), material: m.material.clone(), texture: m.texture.clone() }, m.world)),
                        Prototype::Card(c) => Some((card_shape(c), c.world)),
                        Prototype::Splats(_) => None,
                    })
                    .collect();
                let shared: Vec<Option<usize>> = protos.iter().map(|p| p.as_ref().filter(|_| !realize).map(|(s, _)| e.add(s.clone()))).collect();
                let tints = g.attrs.vec4(geo::CD);
                for i in 0..g.len() {
                    let k = g.which[i] as usize;
                    let Some((shape, own)) = &protos[k] else { continue };
                    let shape = shared[k].unwrap_or_else(|| e.add(shape.clone()));
                    e.items.push(Item { name: format!("{}.{i}", name(&g.node, "", shape)), world: g.worlds[i] * own, shape, tint: tints.map(|t| t[i]) });
                }
            }
        }
        if wants("points") {
            for p in scene.point_sets.iter().filter(|p| take(&p.node)) {
                let shape = e.add(Shape::Points { positions: p.positions.clone(), attrs: p.attrs.clone() });
                e.items.push(Item { name: name(&p.node, "", shape), world: p.world, shape, tint: None });
            }
        }
        if wants("curves") {
            for c in scene.curves.iter().filter(|c| take(&c.node)) {
                let curves = (0..c.closed.len()).map(|k| c.curve(k).to_vec()).collect();
                let shape = e.add(Shape::Lines { curves, closed: c.closed.clone() });
                e.items.push(Item { name: name(&c.node, "", shape), world: c.world, shape, tint: None });
            }
        }
        e
    }
}

/// The 3 × 3 that turns normals as `world` turns points.
fn normal_matrix(world: &Mat4) -> nalgebra::Matrix3<f64> {
    let m = world.fixed_view::<3, 3>(0, 0).into_owned();
    m.try_inverse().map(|i| i.transpose()).unwrap_or(m)
}

/// A material's color and opacity, times a copy's tint.
fn tinted(m: &Material, tint: Option<[f32; 4]>) -> ([f32; 3], f32) {
    let t = tint.unwrap_or([1.0; 4]);
    ([m.color[0] * t[0], m.color[1] * t[1], m.color[2] * t[2]], m.opacity * t[3])
}

/// Textures written as PNG files beside `path` (`<stem>_tex<k>.png`),
/// once each.
struct Textures {
    files: HashMap<*const Image, String>,
    folder: PathBuf,
    stem: String,
}

impl Textures {
    fn new(path: &Path) -> Textures {
        let stem = path.file_stem().map_or("geo".into(), |s| s.to_string_lossy().into_owned());
        Textures { files: HashMap::new(), folder: path.parent().unwrap_or(Path::new(".")).to_path_buf(), stem }
    }

    /// `img`'s file name (relative to the folder), written the first time.
    fn file(&mut self, img: &Arc<Image>) -> Result<String> {
        let key = Arc::as_ptr(img);
        if let Some(f) = self.files.get(&key) {
            return Ok(f.clone());
        }
        let name = format!("{}_tex{}.png", self.stem, self.files.len());
        img.write_as(&self.folder.join(&name), Default::default()).with_context(|| format!("writing the texture {name}"))?;
        self.files.insert(key, name.clone());
        Ok(name)
    }
}

/// Roughness as an MTL Ns (what `mesh::parse_mtl` reads back).
fn ns(roughness: f32) -> f64 {
    let r = (roughness as f64).clamp(0.01, 1.0);
    2.0 / (r * r) - 2.0
}

fn write_obj(e: &Export, path: &Path) -> Result<()> {
    use std::fmt::Write;
    let mut tex = Textures::new(path);
    let mtl_name = format!("{}.mtl", tex.stem);
    let (mut obj, mut mtl) = (format!("# ArclightVFX\nmtllib {mtl_name}\n"), String::from("# ArclightVFX\n"));
    let (mut nv, mut nt, mut nn) = (1usize, 1usize, 1usize);
    for (k, item) in e.items.iter().enumerate() {
        let w = &item.world;
        let at = |p: &Vec3| geom::transform_point(w, p);
        writeln!(obj, "o {}", item.name.replace(char::is_whitespace, "_"))?;
        match &e.shapes[item.shape] {
            Shape::Mesh { mesh, material, texture } => {
                let colors = mesh.point_attrs.vec4(geo::CD).filter(|c| c.len() == mesh.positions.len());
                for (i, p) in mesh.positions.iter().enumerate() {
                    let q = at(p);
                    match colors {
                        Some(c) => writeln!(obj, "v {} {} {} {} {} {}", q.x, q.y, q.z, c[i][0], c[i][1], c[i][2])?,
                        None => writeln!(obj, "v {} {} {}", q.x, q.y, q.z)?,
                    }
                }
                let uvs = mesh.uvs.len() == mesh.positions.len();
                let normals = mesh.normals.len() == mesh.positions.len();
                if uvs {
                    for [u, v] in &mesh.uvs {
                        writeln!(obj, "vt {u} {v}")?;
                    }
                }
                if normals {
                    let nm = normal_matrix(w);
                    for n in &mesh.normals {
                        let n = (nm * n).try_normalize(1e-300).unwrap_or(*n);
                        writeln!(obj, "vn {} {} {}", n.x, n.y, n.z)?;
                    }
                }
                // A material for each object (a copy's tint is in it).
                let (color, opacity) = tinted(material, item.tint);
                writeln!(mtl, "newmtl m{k}\nKd {} {} {}\nKs {} {} {}\nNs {}\nKe {} {} {}\nd {opacity}\nillum {}", color[0], color[1], color[2], material.specular[0], material.specular[1], material.specular[2], ns(material.roughness), material.emission[0], material.emission[1], material.emission[2], match material.shading {
                    Shading::Unlit => 0,
                    Shading::Lambert => 1,
                    _ => 2,
                })?;
                if let Some(t) = texture {
                    writeln!(mtl, "map_Kd {}", tex.file(t)?)?;
                }
                writeln!(obj, "usemtl m{k}")?;
                for t in &mesh.triangles {
                    let c = |i: u32| {
                        let i = i as usize;
                        match (uvs, normals) {
                            (true, true) => format!("{}/{}/{}", nv + i, nt + i, nn + i),
                            (true, false) => format!("{}/{}", nv + i, nt + i),
                            (false, true) => format!("{}//{}", nv + i, nn + i),
                            (false, false) => format!("{}", nv + i),
                        }
                    };
                    writeln!(obj, "f {} {} {}", c(t[0]), c(t[1]), c(t[2]))?;
                }
                nv += mesh.positions.len();
                nt += if uvs { mesh.uvs.len() } else { 0 };
                nn += if normals { mesh.normals.len() } else { 0 };
            }
            Shape::Points { positions, attrs } => {
                let colors = attrs.vec4(geo::CD);
                for (i, p) in positions.iter().enumerate() {
                    let q = at(p);
                    match colors {
                        Some(c) => writeln!(obj, "v {} {} {} {} {} {}", q.x, q.y, q.z, c[i][0], c[i][1], c[i][2])?,
                        None => writeln!(obj, "v {} {} {}", q.x, q.y, q.z)?,
                    }
                }
                if !positions.is_empty() {
                    writeln!(obj, "p {}", (0..positions.len()).map(|i| (nv + i).to_string()).collect::<Vec<_>>().join(" "))?;
                }
                nv += positions.len();
            }
            Shape::Lines { curves, closed } => {
                for (c, pts) in curves.iter().enumerate() {
                    for p in pts {
                        let q = at(p);
                        writeln!(obj, "v {} {} {}", q.x, q.y, q.z)?;
                    }
                    let mut idx: Vec<String> = (0..pts.len()).map(|i| (nv + i).to_string()).collect();
                    if closed.get(c).copied().unwrap_or(false) && pts.len() > 2 {
                        idx.push(nv.to_string());
                    }
                    if idx.len() >= 2 {
                        writeln!(obj, "l {}", idx.join(" "))?;
                    }
                    nv += pts.len();
                }
            }
        }
    }
    std::fs::write(tex.folder.join(&mtl_name), mtl).with_context(|| format!("writing {mtl_name}"))?;
    std::fs::write(path, obj).with_context(|| format!("writing {}", path.display()))
}

/// glTF's binary buffer, its views and accessors, as they're added.
#[derive(Default)]
struct Buffer {
    data: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffer {
    /// A view of `bytes` (4-byte aligned), for vertices (34962), indices
    /// (34963) or anything (None).
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while !self.data.len().is_multiple_of(4) {
            self.data.push(0);
        }
        let mut v = json!({"buffer": 0, "byteOffset": self.data.len(), "byteLength": bytes.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.data.extend_from_slice(bytes);
        self.views.push(v);
        self.views.len() - 1
    }

    /// Floats, `width` a vertex (SCALAR, VEC2, VEC3, VEC4), with their
    /// bounds when asked (POSITION must have them).
    fn floats(&mut self, values: &[f32], width: usize, bounds: bool) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(34962));
        let kind = ["SCALAR", "VEC2", "VEC3", "VEC4"][width - 1];
        let mut a = json!({"bufferView": view, "componentType": 5126, "count": values.len() / width, "type": kind});
        if bounds && !values.is_empty() {
            let lo: Vec<f32> = (0..width).map(|k| values.iter().skip(k).step_by(width).copied().fold(f32::INFINITY, f32::min)).collect();
            let hi: Vec<f32> = (0..width).map(|k| values.iter().skip(k).step_by(width).copied().fold(f32::NEG_INFINITY, f32::max)).collect();
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn indices(&mut self, values: &[u32]) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(34963));
        self.accessors.push(json!({"bufferView": view, "componentType": 5125, "count": values.len(), "type": "SCALAR"}));
        self.accessors.len() - 1
    }
}

fn flat3(v: &[Vec3]) -> Vec<f32> {
    v.iter().flat_map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect()
}

/// A shape's point attributes as glTF attributes: Cd as COLOR_0, N (on
/// points) as NORMAL, the rest as `_name` when `extra`.
fn attribute_accessors(buf: &mut Buffer, attrs: &Attrs, count: usize, points: bool, extra: bool, out: &mut serde_json::Map<String, Value>) {
    for (name, a) in attrs.iter() {
        if a.len() != count {
            continue;
        }
        let (values, width): (Vec<f32>, usize) = match a {
            Attr::Float(v) => (v.clone(), 1),
            Attr::Int(v) => (v.iter().map(|x| *x as f32).collect(), 1),
            Attr::Vec3(v) => (v.iter().flatten().copied().collect(), 3),
            Attr::Vec4(v) => (v.iter().flatten().copied().collect(), 4),
        };
        let key = match (name, width) {
            (geo::CD, 4) => "COLOR_0".to_string(),
            (geo::N, 3) if points => "NORMAL".to_string(),
            (geo::N, _) => continue,
            _ if extra => format!("_{name}"),
            _ => continue,
        };
        out.insert(key, json!(buf.floats(&values, width, false)));
    }
}

/// A shape's primitive: its attributes (accessors), indices and mode.
type Prim = (serde_json::Map<String, Value>, Option<usize>, u32);

fn write_gltf(e: &Export, path: &Path, binary: bool, extra: bool) -> Result<()> {
    let mut buf = Buffer::default();
    let mut tex = Textures::new(path);
    let (mut images, mut textures, mut materials, mut meshes, mut nodes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut image_of: HashMap<*const Image, usize> = HashMap::new();
    let mut unlit = false;
    // Each shape's attributes (accessors), made once and shared.
    let mut prims: HashMap<usize, Prim> = HashMap::new();
    // A mesh per shape and tint.
    let mut mesh_of: HashMap<(usize, [u32; 4]), usize> = HashMap::new();
    for item in &e.items {
        let shape = &e.shapes[item.shape];
        if let std::collections::hash_map::Entry::Vacant(slot) = prims.entry(item.shape) {
            let mut attributes = serde_json::Map::new();
            let made = match shape {
                Shape::Mesh { mesh, .. } => {
                    attributes.insert("POSITION".into(), json!(buf.floats(&flat3(&mesh.positions), 3, true)));
                    if mesh.normals.len() == mesh.positions.len() {
                        attributes.insert("NORMAL".into(), json!(buf.floats(&flat3(&mesh.normals), 3, false)));
                    }
                    if mesh.uvs.len() == mesh.positions.len() {
                        // glTF's v runs down the image.
                        let uv: Vec<f32> = mesh.uvs.iter().flat_map(|[u, v]| [*u as f32, 1.0 - *v as f32]).collect();
                        attributes.insert("TEXCOORD_0".into(), json!(buf.floats(&uv, 2, false)));
                    }
                    attribute_accessors(&mut buf, &mesh.point_attrs, mesh.positions.len(), false, extra, &mut attributes);
                    let idx: Vec<u32> = mesh.triangles.iter().flatten().copied().collect();
                    (Some(buf.indices(&idx)), 4)
                }
                Shape::Points { positions, attrs } => {
                    attributes.insert("POSITION".into(), json!(buf.floats(&flat3(positions), 3, true)));
                    attribute_accessors(&mut buf, attrs, positions.len(), true, extra, &mut attributes);
                    (None, 0)
                }
                Shape::Lines { curves, closed } => {
                    let all: Vec<Vec3> = curves.iter().flatten().copied().collect();
                    attributes.insert("POSITION".into(), json!(buf.floats(&flat3(&all), 3, true)));
                    let mut pairs = Vec::new();
                    let mut base = 0u32;
                    for (c, pts) in curves.iter().enumerate() {
                        let n = pts.len() as u32;
                        for i in 1..n {
                            pairs.extend([base + i - 1, base + i]);
                        }
                        if closed.get(c).copied().unwrap_or(false) && n > 2 {
                            pairs.extend([base + n - 1, base]);
                        }
                        base += n;
                    }
                    (Some(buf.indices(&pairs)), 1)
                }
            };
            slot.insert((attributes, made.0, made.1));
        }
        let tint_key = item.tint.unwrap_or([1.0; 4]).map(f32::to_bits);
        let mesh = match mesh_of.get(&(item.shape, tint_key)) {
            Some(m) => *m,
            None => {
                let (attributes, indices, mode) = &prims[&item.shape];
                let mut prim = json!({"attributes": attributes, "mode": mode});
                if let Some(i) = indices {
                    prim["indices"] = json!(i);
                }
                if let Shape::Mesh { material, texture, .. } = shape {
                    let (color, opacity) = tinted(material, item.tint);
                    let mut pbr = json!({"baseColorFactor": [color[0], color[1], color[2], opacity], "metallicFactor": 0.0, "roughnessFactor": material.roughness});
                    if let Some(t) = texture {
                        let image = match image_of.get(&Arc::as_ptr(t)) {
                            Some(i) => *i,
                            None => {
                                let entry = if binary {
                                    // The PNG a Write would make, encoded in memory.
                                    let mut bytes = std::io::Cursor::new(Vec::new());
                                    t.flat().to_rgba8().write_to(&mut bytes, image::ImageFormat::Png).context("encoding a texture as PNG")?;
                                    json!({"bufferView": buf.view(bytes.get_ref(), None), "mimeType": "image/png"})
                                } else {
                                    json!({"uri": tex.file(t)?})
                                };
                                images.push(entry);
                                textures.push(json!({"source": images.len() - 1}));
                                image_of.insert(Arc::as_ptr(t), textures.len() - 1);
                                textures.len() - 1
                            }
                        };
                        pbr["baseColorTexture"] = json!({"index": image});
                    }
                    let mut m = json!({"name": format!("material{}", materials.len()), "pbrMetallicRoughness": pbr, "emissiveFactor": material.emission});
                    if opacity < 1.0 {
                        m["alphaMode"] = json!("BLEND");
                    }
                    if material.shading == Shading::Unlit {
                        m["extensions"] = json!({"KHR_materials_unlit": {}});
                        unlit = true;
                    }
                    materials.push(m);
                    prim["material"] = json!(materials.len() - 1);
                }
                meshes.push(json!({"primitives": [prim]}));
                mesh_of.insert((item.shape, tint_key), meshes.len() - 1);
                meshes.len() - 1
            }
        };
        let mut node = json!({"name": item.name, "mesh": mesh});
        if item.world != Mat4::identity() {
            node["matrix"] = json!((0..16).map(|k| item.world[(k % 4, k / 4)]).collect::<Vec<f64>>());
        }
        nodes.push(node);
    }
    while buf.data.len() % 4 != 0 {
        buf.data.push(0);
    }
    let stem = tex.stem.clone();
    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "ArclightVFX"},
        "scene": 0,
        "scenes": [{"nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes,
        "meshes": meshes,
        "accessors": buf.accessors,
        "bufferViews": buf.views,
        "buffers": [{"byteLength": buf.data.len()}],
    });
    for (k, v) in [("materials", materials), ("textures", textures), ("images", images)] {
        if !v.is_empty() {
            doc[k] = json!(v);
        }
    }
    if unlit {
        doc["extensionsUsed"] = json!(["KHR_materials_unlit"]);
    }
    if binary {
        let mut text = serde_json::to_vec(&doc)?;
        while text.len() % 4 != 0 {
            text.push(b' ');
        }
        let total = 12 + 8 + text.len() + 8 + buf.data.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(text.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&text);
        out.extend_from_slice(&(buf.data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&buf.data);
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
    } else {
        let bin = format!("{stem}.bin");
        doc["buffers"][0]["uri"] = json!(bin);
        std::fs::write(tex.folder.join(&bin), &buf.data).with_context(|| format!("writing {bin}"))?;
        std::fs::write(path, serde_json::to_string_pretty(&doc)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// Write Geo's file for `scene` at `frame` (its path's #### the frame):
/// OBJ, glTF or GLB by the extension.
pub fn write_geo(params: &Value, scene: &Scene, frame: i64) -> Result<PathBuf> {
    let pattern = params["path"].as_str().unwrap_or("").trim();
    if pattern.is_empty() {
        bail!("Write Geo: no output file set");
    }
    let path = crate::media::frame_path(pattern, frame);
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let e = Export::of(scene, params);
    let extra = params["attributes"].as_bool().unwrap_or(true);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("can't make {}", dir.display()))?;
    }
    match ext.as_str() {
        "obj" => write_obj(&e, &path)?,
        "gltf" => write_gltf(&e, &path, false, extra)?,
        "glb" => write_gltf(&e, &path, true, extra)?,
        _ => bail!("Write Geo: {} isn't .obj, .gltf or .glb", path.display()),
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{Curves, Instances, PointSet};
    use crate::image::{IRect, Size};
    use crate::scene::Model;

    /// A fresh folder for a test's files.
    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arclight_geo_write_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A cube 2 up (red, roughness 0.3, a point attribute w and Cd), a card
    /// with a 2 × 1 image, two tinted copies of a small cube, three colored
    /// points and a closed four-point curve.
    fn scene() -> Scene {
        let mut cube = crate::mesh::cube([1.0; 3], [1, 1, 1]);
        let n = cube.positions.len();
        cube.point_attrs.set("w", Attr::Float((0..n).map(|i| i as f32).collect()), n).unwrap();
        cube.point_attrs.set(geo::CD, Attr::Vec4(vec![[0.5, 0.25, 1.0, 1.0]; n]), n).unwrap();
        let red = Material { color: [0.8, 0.2, 0.1], shading: Shading::Lambert, roughness: 0.3, ..Material::default() };
        let model = Model { node: "box".into(), ..Model::new(Arc::new(cube), geom::trs([0.0, 2.0, 0.0], [0.0; 3], [1.0; 3]), red) };
        let mut img = Image::blank(Size { width: 2, height: 1 }, IRect::new(0, 0, 2, 1));
        img.pixels = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        let card = Card { node: "sign".into(), ..Card::new(Arc::new(img), geom::trs([3.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), [2.0, 1.0]) };
        let small = Model::new(Arc::new(crate::mesh::cube([0.2; 3], [1, 1, 1])), Mat4::identity(), Material { shading: Shading::Lambert, ..Material::default() });
        let mut copies = Instances { prototypes: vec![Prototype::Model(small)], which: vec![0, 0], worlds: vec![geom::trs([-3.0, 0.0, 0.0], [0.0; 3], [1.0; 3]), geom::trs([-4.0, 0.0, 0.0], [0.0; 3], [1.0; 3])], node: "rocks".into(), ..Instances::default() };
        copies.attrs.set(geo::CD, Attr::Vec4(vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]]), 2).unwrap();
        let mut pts = PointSet { positions: vec![Vec3::zeros(), Vec3::x(), Vec3::y()], node: "pts".into(), ..PointSet::default() };
        pts.attrs.set(geo::CD, Attr::Vec4(vec![[0.1, 0.2, 0.3, 1.0]; 3]), 3).unwrap();
        pts.attrs.set("k", Attr::Float(vec![1.0, 2.0, 3.0]), 3).unwrap();
        let mut curve = Curves { node: "path".into(), ..Curves::default() };
        curve.push(&[Vec3::zeros(), Vec3::x(), Vec3::new(1.0, 0.0, 1.0), Vec3::z()], true);
        Scene { meshes: vec![model], cards: vec![card], instances: vec![copies], point_sets: vec![pts], curves: vec![curve], ..Scene::default() }
    }

    fn bounds(m: &Mesh) -> (Vec3, Vec3) {
        m.bounds().unwrap()
    }

    #[test]
    fn obj_round_trips_through_the_reader() {
        let dir = folder("obj");
        let path = write_geo(&json!({"path": dir.join("set.obj").to_string_lossy()}), &scene(), 1).unwrap();
        let parts = crate::mesh::read_obj(&path).unwrap();
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["box", "sign", "rocks.0", "rocks.1"], "meshes, the card, the copies; points and curves aren't faces");
        // Placed: the cube 2 up, the copies where they were.
        let (lo, hi) = bounds(&parts[0].mesh);
        assert!((lo.y - 1.5).abs() < 1e-9 && (hi.y - 2.5).abs() < 1e-9, "{lo:?} {hi:?}");
        assert!((bounds(&parts[3].mesh).0.x + 4.1).abs() < 1e-9);
        // Materials: color, roughness, shading; the card's texture; tints.
        let m = &parts[0].material;
        assert_eq!((m.color, m.shading), ([0.8, 0.2, 0.1], Shading::Lambert));
        assert!((m.roughness - 0.3).abs() < 1e-3, "{}", m.roughness);
        assert!(parts[1].material.texture.as_ref().is_some_and(|t| t.is_file()), "the card's image beside it");
        assert_eq!((parts[2].material.color, parts[3].material.color), ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]));
        assert_eq!(parts[0].mesh.uvs.len(), parts[0].mesh.positions.len());
        // Points, a closed line and vertex colors in the text.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().filter(|l| l.starts_with("p ")).map(|l| l.split_whitespace().count() - 1).collect::<Vec<_>>(), [3]);
        assert_eq!(text.lines().filter(|l| l.starts_with("l ")).map(|l| l.split_whitespace().count() - 1).collect::<Vec<_>>(), [5], "closed: back to the start");
        assert!(text.lines().any(|l| l.starts_with("v ") && l.split_whitespace().count() == 7 && l.ends_with("0.5 0.25 1")), "Cd as vertex colors");
    }

    #[test]
    fn gltf_and_glb_round_trip_with_shared_meshes_and_attributes() {
        for ext in ["gltf", "glb"] {
            let dir = folder(ext);
            let path = write_geo(&json!({"path": dir.join(format!("set.{ext}")).to_string_lossy()}), &scene(), 1).unwrap();
            let parts = crate::gltf::read_gltf(&path).unwrap_or_else(|e| panic!("{ext}: {e:#}"));
            assert_eq!(parts.len(), 4, "{ext}");
            let (lo, _) = bounds(&parts[0].mesh);
            assert!((lo.y - 1.5).abs() < 1e-6, "{ext}: placed by its node");
            assert!((parts[0].material.color[0] - 0.8).abs() < 1e-6 && (parts[0].material.roughness - 0.3).abs() < 1e-6);
            let uvs = crate::mesh::cube([1.0; 3], [1, 1, 1]).uvs;
            assert!(parts[0].mesh.uvs.iter().zip(&uvs).all(|(a, b)| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6), "{ext}: the same uvs");
            // Our attributes come back.
            let a = &parts[0].mesh.point_attrs;
            assert_eq!(a.float("w").map(|w| w[5]), Some(5.0), "{ext}: _w");
            assert_eq!(a.vec4(geo::CD).map(|c| c[0]), Some([0.5, 0.25, 1.0, 1.0]), "{ext}: COLOR_0");
            let tex = parts[1].material.texture.as_ref().expect("the card's texture");
            let back = Image::read(tex).unwrap();
            assert!(back.pixels[0][0] > 0.9 && back.pixels[1][2] > 0.9, "{ext}: its pixels");
            assert_eq!((parts[2].material.color, parts[3].material.color), ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), "{ext}: tints");
        }
        // In the file: the copies' meshes share one set of accessors; the
        // points are POINTS with COLOR_0 and _k; the curve LINES, closed.
        let dir = folder("json");
        let path = write_geo(&json!({"path": dir.join("set.gltf").to_string_lossy()}), &scene(), 1).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let prim = |m: usize| doc["meshes"][m]["primitives"][0].clone();
        let node_mesh = |name: &str| doc["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap()["mesh"].as_u64().unwrap() as usize;
        let (r0, r1) = (prim(node_mesh("rocks.0")), prim(node_mesh("rocks.1")));
        assert_ne!(node_mesh("rocks.0"), node_mesh("rocks.1"), "two tints, two meshes");
        assert_eq!(r0["attributes"]["POSITION"], r1["attributes"]["POSITION"], "one set of vertices");
        let p = prim(node_mesh("pts"));
        assert!(p["mode"] == 0 && p["attributes"]["COLOR_0"].is_u64() && p["attributes"]["_k"].is_u64(), "{p}");
        let l = prim(node_mesh("path"));
        assert_eq!((l["mode"].as_u64(), doc["accessors"][l["indices"].as_u64().unwrap() as usize]["count"].as_u64()), (Some(1), Some(8)));
        let pos = &doc["accessors"][r0["attributes"]["POSITION"].as_u64().unwrap() as usize];
        assert!(pos["min"].is_array() && pos["max"].is_array(), "POSITION bounds");
        // Realize: each copy its own vertices.
        let dir = folder("realize");
        let path = write_geo(&json!({"path": dir.join("set.gltf").to_string_lossy(), "instances": "realize", "include": ["instances"]}), &scene(), 1).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_ne!(doc["meshes"][0]["primitives"][0]["attributes"]["POSITION"], doc["meshes"][1]["primitives"][0]["attributes"]["POSITION"]);
        assert_eq!(doc["nodes"].as_array().unwrap().len(), 2, "only what's included");
    }

    #[test]
    fn frames_paths_and_errors() {
        let dir = folder("frames");
        let path = write_geo(&json!({"path": dir.join("geo.####.obj").to_string_lossy()}), &scene(), 7).unwrap();
        assert!(path.ends_with("geo.0007.obj") && path.is_file());
        assert!(write_geo(&json!({}), &scene(), 1).unwrap_err().to_string().contains("no output file"));
        assert!(write_geo(&json!({"path": dir.join("x.fbx").to_string_lossy()}), &scene(), 1).unwrap_err().to_string().contains(".obj, .gltf or .glb"));
    }

    /// As a node: written only when outputs are (in the same evaluator
    /// too, after the viewer cached it), the scene passed on; a relative
    /// path is the project folder's, as other file nodes'.
    #[test]
    fn the_node_writes_when_rendered() {
        let dir = folder("node");
        let file = dir.join("out.glb");
        let graph: crate::graph::Graph = serde_json::from_value(json!({"nodes": [
            {"id": "grid", "op": "grid_points", "params": {"count": [3, 3]}},
            {"id": "save", "op": "write_geo", "inputs": ["grid"], "params": {"path": file.to_string_lossy()}},
        ]})).unwrap();
        let mut eval = crate::graph::Evaluator::new();
        let mut run = |write: bool| eval.evaluate(&graph, "save", &crate::ops::EvalContext { frame: 1, write_outputs: write }).unwrap();
        let out = run(false);
        assert!(!file.exists() && out.scene.as_ref().unwrap().point_sets[0].len() == 9, "not written in the viewer");
        run(true);
        let doc = std::fs::read(&file).unwrap();
        assert_eq!(&doc[..4], b"glTF");
        // A project in its folder: the file goes in there.
        let project = dir.join("shot");
        std::fs::create_dir_all(&project).unwrap();
        let json = json!({"project": {"folder": ".", "first_frame": 1, "last_frame": 24, "fps": 24.0, "format": {"width": 64, "height": 64}}, "nodes": [
            {"id": "c", "op": "cube"},
            {"id": "save", "op": "write_geo", "inputs": ["c"], "params": {"path": "geo/cube.####.obj"}},
        ]});
        std::fs::write(project.join("shot.json"), json.to_string()).unwrap();
        let g = crate::graph::Graph::from_file(&project.join("shot.json")).unwrap();
        crate::graph::Evaluator::new().evaluate(&g, "save", &crate::ops::EvalContext { frame: 12, write_outputs: true }).unwrap();
        assert!(project.join("geo/cube.0012.obj").is_file());
    }
}
