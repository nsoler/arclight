//! glTF 2.0 import (`.gltf` with its buffers and images beside it or
//! embedded as data URIs, or one binary `.glb`), into the same [`Part`]s as
//! [`mesh::read_obj`](crate::mesh::read_obj).
//!
//! - The default scene's node tree (or the first scene's) is walked and
//!   each node's transform applied down the tree; parts come out in world
//!   space. Each primitive is a part, named after its node (or mesh), with
//!   `.<material>` when the mesh has more than one.
//! - Triangles, strips and fans are read; points and lines skipped.
//!   Normals missing from the file give flat shading; texture coordinates
//!   are flipped to v up (glTF has v = 0 at the image's top).
//! - Materials (PBR metallic–roughness): base color (and alpha, when the
//!   material blends) to color and opacity, its texture to `texture`,
//!   roughness as is, emission (times `KHR_materials_emissive_strength`),
//!   metallic as the highlight's color (4 % gray for a dielectric up to the
//!   base color for a metal), Blinn shading; `KHR_materials_unlit` is
//!   Unlit. An embedded image (in a `.glb` or a data URI) is saved once to
//!   the temporary folder, as `Material::texture` is a file.
//! - COLOR_0 and application-specific attributes (`_name`, floats) come
//!   in as the mesh's point attributes (Cd, name), so what Write Geo
//!   writes reads back.

use crate::geom::{Mat4, Vec3};
use crate::mesh::{Material, Mesh, Part, Shading};
use ::gltf::{image::Source, material::AlphaMode, mesh::Mode};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// The parts of the glTF file at `path`.
pub fn read_gltf(path: &Path) -> Result<Vec<Part>> {
    let bytes = std::fs::read(path).with_context(|| format!("can't read {}", path.display()))?;
    parse_gltf(&bytes, path.parent().unwrap_or(Path::new("."))).with_context(|| format!("in {}", path.display()))
}

/// The parts of glTF data (JSON or binary) whose external files are in
/// `folder`.
pub fn parse_gltf(bytes: &[u8], folder: &Path) -> Result<Vec<Part>> {
    let ::gltf::Gltf { document, blob } = ::gltf::Gltf::from_slice(bytes).map_err(|e| anyhow!("not glTF: {e}"))?;
    let buffers = ::gltf::import_buffers(&document, Some(folder), blob).map_err(|e| anyhow!("reading buffers: {e}"))?;
    let mut out = Vec::new();
    let roots: Vec<::gltf::Node> = match document.default_scene().or_else(|| document.scenes().next()) {
        Some(scene) => scene.nodes().collect(),
        None => Vec::new(),
    };
    // Depth first, in the file's order (the stack pops from its end).
    let mut stack: Vec<(::gltf::Node, Mat4)> = roots.into_iter().rev().map(|n| (n, Mat4::identity())).collect();
    while let Some((node, parent)) = stack.pop() {
        let m = node.transform().matrix();
        let world = parent * Mat4::from_fn(|r, c| m[c][r] as f64);
        if let Some(mesh) = node.mesh() {
            let base = node.name().or(mesh.name()).map(str::to_string).unwrap_or_else(|| format!("mesh{}", mesh.index()));
            let several = mesh.primitives().len() > 1;
            for prim in mesh.primitives() {
                let Some(mut part) = primitive(&prim, &buffers, folder)? else { continue };
                part.mesh.transform(&world);
                part.name = if several { format!("{base}.{}", part.material.name) } else { base.clone() };
                out.push(part);
            }
        }
        stack.extend(node.children().map(|c| (c, world)).collect::<Vec<_>>().into_iter().rev());
    }
    Ok(out)
}

/// One primitive as a part (in its node's space; None for points and
/// lines).
fn primitive(prim: &::gltf::Primitive, buffers: &[::gltf::buffer::Data], folder: &Path) -> Result<Option<Part>> {
    let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
    let positions: Vec<Vec3> = reader.read_positions().context("a primitive without positions")?.map(|p| Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
    let indices: Vec<u32> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };
    if let Some(&bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
        anyhow::bail!("index {bad} past the {} positions", positions.len());
    }
    let triangles: Vec<[u32; 3]> = match prim.mode() {
        Mode::Triangles => indices.as_chunks::<3>().0.to_vec(),
        // Every other strip triangle is wound the other way.
        Mode::TriangleStrip => (2..indices.len()).map(|i| if i % 2 == 0 { [indices[i - 2], indices[i - 1], indices[i]] } else { [indices[i - 1], indices[i - 2], indices[i]] }).collect(),
        Mode::TriangleFan => (2..indices.len()).map(|i| [indices[0], indices[i - 1], indices[i]]).collect(),
        _ => return Ok(None),
    };
    let mut mesh = Mesh { positions, triangles, ..Mesh::default() };
    if let Some(uv) = reader.read_tex_coords(0) {
        mesh.uvs = uv.into_f32().map(|t| [t[0] as f64, 1.0 - t[1] as f64]).collect();
    }
    read_attributes(prim, buffers, &mut mesh);
    match reader.read_normals() {
        Some(n) => mesh.normals = n.map(|n| Vec3::new(n[0] as f64, n[1] as f64, n[2] as f64).try_normalize(1e-300).unwrap_or_else(Vec3::y)).collect(),
        None => mesh = mesh.flat(),
    }
    let material = material(&prim.material(), buffers, folder)?;
    Ok(Some(Part { name: String::new(), mesh, material }))
}

/// A primitive's colors (COLOR_0, as Cd) and application-specific
/// attributes (`_name`, as name: floats by their width), as point
/// attributes of `mesh`, as Write Geo writes them.
fn read_attributes(prim: &::gltf::Primitive, buffers: &[::gltf::buffer::Data], mesh: &mut Mesh) {
    use crate::geo::Attr;
    let n = mesh.positions.len();
    let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
    if let Some(c) = reader.read_colors(0) {
        let c: Vec<[f32; 4]> = c.into_rgba_f32().collect();
        let _ = mesh.point_attrs.set(crate::geo::CD, Attr::Vec4(c), n);
    }
    let data = |b: ::gltf::Buffer| buffers.get(b.index()).map(|d| &d.0[..]);
    for (semantic, accessor) in prim.attributes() {
        let ::gltf::Semantic::Extras(name) = semantic else { continue };
        if accessor.data_type() != ::gltf::accessor::DataType::F32 || name.is_empty() {
            continue;
        }
        let attr = match accessor.dimensions() {
            ::gltf::accessor::Dimensions::Scalar => ::gltf::accessor::Iter::<f32>::new(accessor, data).map(|i| Attr::Float(i.collect())),
            ::gltf::accessor::Dimensions::Vec3 => ::gltf::accessor::Iter::<[f32; 3]>::new(accessor, data).map(|i| Attr::Vec3(i.collect())),
            ::gltf::accessor::Dimensions::Vec4 => ::gltf::accessor::Iter::<[f32; 4]>::new(accessor, data).map(|i| Attr::Vec4(i.collect())),
            _ => None,
        };
        if let Some(a) = attr {
            let _ = mesh.point_attrs.set(&name, a, n);
        }
    }
}

/// A glTF material as ours.
fn material(m: &::gltf::Material, buffers: &[::gltf::buffer::Data], folder: &Path) -> Result<Material> {
    let pbr = m.pbr_metallic_roughness();
    let [r, g, b, a] = pbr.base_color_factor();
    let metal = pbr.metallic_factor().clamp(0.0, 1.0);
    let strength = m.emissive_strength().unwrap_or(1.0);
    let texture = match pbr.base_color_texture() {
        Some(info) => Some(image_file(&info.texture().source(), buffers, folder)?),
        None => None,
    };
    Ok(Material {
        name: m.name().map(str::to_string).unwrap_or_else(|| m.index().map_or("default".into(), |i| format!("material{i}"))),
        shading: if m.unlit() { Shading::Unlit } else { Shading::Blinn },
        color: [r, g, b],
        opacity: if m.alpha_mode() == AlphaMode::Blend { a } else { 1.0 },
        texture,
        specular: [r, g, b].map(|c| 0.04 + (c - 0.04) * metal),
        roughness: pbr.roughness_factor().clamp(0.0, 1.0),
        emission: m.emissive_factor().map(|c| c * strength),
    })
}

/// The file holding `image`: an external file beside the glTF, or the
/// embedded bytes saved (once, by content) to the temporary folder.
fn image_file(image: &::gltf::Image, buffers: &[::gltf::buffer::Data], folder: &Path) -> Result<PathBuf> {
    let (bytes, mime): (Vec<u8>, Option<&str>) = match image.source() {
        Source::View { view, mime_type } => {
            let data = &buffers.get(view.buffer().index()).context("an image's buffer is missing")?.0;
            (data.get(view.offset()..view.offset() + view.length()).context("an image's bytes are out of its buffer")?.to_vec(), Some(mime_type))
        }
        Source::Uri { uri, mime_type } => match uri.strip_prefix("data:") {
            Some(rest) => {
                let (head, data) = rest.split_once(',').context("a data URI without data")?;
                let mime = head.split(';').next().filter(|m| !m.is_empty()).or(mime_type);
                (base64(data).context("an image's data URI isn't base64")?, mime)
            }
            None => return Ok(folder.join(percent_decode(uri))),
        },
    };
    let ext = match mime {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some(m) => m.rsplit('/').next().unwrap_or("bin"),
        None => "png",
    };
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = std::env::temp_dir().join("arclight_gltf");
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    if !path.is_file() {
        std::fs::create_dir_all(&dir)?;
        // Written whole under another name first, so no reader sees half.
        let part = dir.join(format!("{:016x}.{}.part", h.finish(), std::process::id()));
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &path)?;
    }
    Ok(path)
}

/// `%xx` escapes in a URI turned back into bytes.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 (padding optional, whitespace ignored).
fn base64(s: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let digits: Vec<u32> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').map(value).collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, d)| n | d << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// Standard base64 with padding.
    pub(crate) fn encode64(b: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = c.iter().enumerate().fold(0u32, |n, (i, &v)| n | (v as u32) << (16 - 8 * i));
            for i in 0..4 {
                out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }

    /// A unit quad in XY facing +Z, as glTF buffer bytes: positions, UVs
    /// (v down, as glTF has them), indices; and their accessors.
    fn quad() -> (Vec<u8>, serde_json::Value, serde_json::Value) {
        let mut b = Vec::new();
        for p in [[-0.5f32, -0.5, 0.0], [0.5, -0.5, 0.0], [0.5, 0.5, 0.0], [-0.5, 0.5, 0.0]] {
            p.iter().for_each(|v| b.extend_from_slice(&v.to_le_bytes()));
        }
        for t in [[0.0f32, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]] {
            t.iter().for_each(|v| b.extend_from_slice(&v.to_le_bytes()));
        }
        for i in [0u16, 1, 2, 0, 2, 3] {
            b.extend_from_slice(&i.to_le_bytes());
        }
        let views = json!([
            { "buffer": 0, "byteOffset": 0, "byteLength": 48 },
            { "buffer": 0, "byteOffset": 48, "byteLength": 32 },
            { "buffer": 0, "byteOffset": 80, "byteLength": 12 }
        ]);
        let accessors = json!([
            { "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-0.5, -0.5, 0.0], "max": [0.5, 0.5, 0.0] },
            { "bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC2" },
            { "bufferView": 2, "componentType": 5123, "count": 6, "type": "SCALAR" }
        ]);
        (b, views, accessors)
    }

    fn png(color: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba(color)).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn a_gltf_with_a_node_tree_and_materials_reads_into_world_parts() {
        let dir = std::env::temp_dir().join(format!("arclight_gltf_test_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        std::fs::write(dir.join("tex/brick wall.png"), png([200, 50, 50, 255])).unwrap();
        let (buf, views, accessors) = quad();
        let red_png = png([255, 0, 0, 255]);
        let half = std::f64::consts::FRAC_1_SQRT_2;
        let doc = json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0, 3] }],
            "nodes": [
                { "name": "rig", "translation": [0.0, 1.0, 0.0], "scale": [2.0, 2.0, 2.0], "children": [1] },
                // Turned 90° about Y: the quad faces +X.
                { "name": "wall", "rotation": [0.0, half, 0.0, half], "mesh": 0, "children": [2] },
                { "translation": [0.0, 0.0, 1.0], "mesh": 1 },
                { "name": "sign", "translation": [5.0, 0.0, 0.0], "mesh": 2 }
            ],
            "meshes": [
                { "primitives": [{ "attributes": { "POSITION": 0, "TEXCOORD_0": 1 }, "indices": 2, "material": 0 }] },
                { "name": "tile", "primitives": [
                    { "attributes": { "POSITION": 0 }, "indices": 2, "material": 1 },
                    { "attributes": { "POSITION": 0 }, "indices": 2, "mode": 1 }
                ] },
                { "primitives": [{ "attributes": { "POSITION": 0, "TEXCOORD_0": 1 }, "indices": 2, "material": 2 }] }
            ],
            "materials": [
                { "name": "brick", "pbrMetallicRoughness": { "baseColorFactor": [0.8, 0.4, 0.2, 0.5], "baseColorTexture": { "index": 0 }, "metallicFactor": 0.0, "roughnessFactor": 0.7 } },
                { "name": "gold", "alphaMode": "BLEND", "pbrMetallicRoughness": { "baseColorFactor": [1.0, 0.8, 0.3, 0.5], "metallicFactor": 1.0, "roughnessFactor": 0.2 },
                  "emissiveFactor": [0.5, 0.25, 0.0], "extensions": { "KHR_materials_emissive_strength": { "emissiveStrength": 4.0 } } },
                { "name": "neon", "pbrMetallicRoughness": { "baseColorTexture": { "index": 1 } }, "extensions": { "KHR_materials_unlit": {} } }
            ],
            "textures": [{ "source": 0 }, { "source": 1 }],
            "images": [{ "uri": "tex/brick%20wall.png" }, { "uri": format!("data:image/png;base64,{}", encode64(&red_png)) }],
            "extensionsUsed": ["KHR_materials_unlit", "KHR_materials_emissive_strength"],
            "accessors": accessors,
            "bufferViews": views,
            "buffers": [{ "byteLength": buf.len(), "uri": format!("data:application/octet-stream;base64,{}", encode64(&buf)) }]
        });
        let path = dir.join("scene.gltf");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let parts = read_gltf(&path).unwrap();
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["wall", "tile.gold", "sign"], "the lines primitive skipped");
        let near = |a: Vec3, b: [f64; 3]| (a - Vec3::from(b)).norm() < 1e-6;
        // The wall: scaled 2, raised 1, turned to face +X.
        let wall = &parts[0].mesh;
        assert_eq!((wall.positions.len(), wall.triangles.len()), (6, 2), "flat: no normals in the file");
        assert_eq!(wall.bounds().map(|(lo, hi)| (lo.map(|v| (v * 1e6).round() / 1e6), hi.map(|v| (v * 1e6).round() / 1e6))), Some((Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 2.0, 1.0))));
        assert!(wall.normals.iter().all(|n| near(*n, [1.0, 0.0, 0.0])), "flat normals, turned: {:?}", wall.normals);
        assert!((0..2).all(|t| wall.face_normal(t).normalize().dot(&Vec3::x()) > 0.999));
        // Texture coordinates flipped to v up: the bottom left corner is (0, 0).
        let bl = wall.positions.iter().position(|p| near(*p, [0.0, 0.0, 1.0])).unwrap();
        assert_eq!(wall.uvs[bl], [0.0, 0.0]);
        // Its child, 1 further along its own Z (+X in the world, times 2).
        let tile = &parts[1].mesh;
        assert!(tile.positions.iter().all(|p| (p.x - 2.0).abs() < 1e-6) && tile.uvs.is_empty());
        assert!(parts[2].mesh.positions.iter().all(|p| p.x > 4.0 && p.x < 6.0));
        // Materials.
        let brick = &parts[0].material;
        assert_eq!((brick.name.as_str(), brick.shading, brick.color, brick.opacity), ("brick", Shading::Blinn, [0.8, 0.4, 0.2], 1.0));
        assert_eq!((brick.roughness, brick.specular), (0.7, [0.04; 3]));
        assert_eq!(brick.texture.as_deref(), Some(dir.join("tex/brick wall.png").as_path()));
        let gold = &parts[1].material;
        assert_eq!((gold.opacity, gold.specular, gold.emission), (0.5, [1.0, 0.8, 0.3], [2.0, 1.0, 0.0]));
        let neon = &parts[2].material;
        assert_eq!(neon.shading, Shading::Unlit);
        let saved = neon.texture.as_ref().unwrap();
        assert_eq!(std::fs::read(saved).unwrap(), red_png, "the data URI image, saved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_glb_holds_its_buffer_and_images() {
        let (mut buf, views, accessors) = quad();
        // The image after the geometry, 4-aligned.
        let blue = png([0, 0, 255, 255]);
        let at = buf.len();
        buf.extend_from_slice(&blue);
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
        let mut views = views.as_array().unwrap().clone();
        views.push(json!({ "buffer": 0, "byteOffset": at, "byteLength": blue.len() }));
        let doc = json!({
            "asset": { "version": "2.0" },
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "name": "card", "primitives": [{ "attributes": { "POSITION": 0, "TEXCOORD_0": 1 }, "indices": 2, "material": 0 }] }],
            "materials": [{ "pbrMetallicRoughness": { "baseColorTexture": { "index": 0 } } }],
            "textures": [{ "source": 0 }],
            "images": [{ "bufferView": 3, "mimeType": "image/png" }],
            "accessors": accessors,
            "bufferViews": views,
            "buffers": [{ "byteLength": buf.len() }]
        });
        let mut json = serde_json::to_vec(&doc).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + buf.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&buf);
        let parts = parse_gltf(&glb, Path::new(".")).unwrap();
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        // No normals in the file: flat, each triangle its own three vertices.
        assert_eq!((p.name.as_str(), p.mesh.positions.len(), p.material.name.as_str()), ("card", 6, "material0"));
        assert!(p.mesh.normals.iter().all(|n| *n == Vec3::z()));
        let tex = p.material.texture.as_ref().unwrap();
        assert!(tex.extension().is_some_and(|e| e == "png"));
        assert_eq!(std::fs::read(tex).unwrap(), blue);
        // Not glTF at all: an error.
        assert!(parse_gltf(b"hello", Path::new(".")).is_err());
    }

    #[test]
    fn base64_and_uris_decode() {
        for s in [&b""[..], b"a", b"ab", b"abc", b"any carnal pleas", &[0, 255, 128, 7, 9]] {
            assert_eq!(base64(&encode64(s)).as_deref(), Some(s));
        }
        assert_eq!(base64("aGk"), Some(b"hi".to_vec()), "padding optional");
        assert!(base64("a*b").is_none());
        assert_eq!(percent_decode("a%20b%2Fc%zz"), "a b/c%zz");
    }

    #[test]
    fn the_example_gltf_reads() {
        let parts = read_gltf(&Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geo/crate.gltf")).unwrap();
        assert_eq!(parts.len(), 1);
        let m = &parts[0].mesh;
        assert_eq!((parts[0].name.as_str(), m.positions.len(), m.triangles.len(), m.uvs.len()), ("crate", 24, 12, 24));
        assert_eq!(m.bounds(), Some((Vec3::repeat(-0.5), Vec3::repeat(0.5))));
        for t in 0..m.triangles.len() {
            let f = m.face_normal(t).normalize();
            let mid = m.triangles[t].iter().map(|&i| m.positions[i as usize]).sum::<Vec3>() / 3.0;
            assert!(f.dot(&mid) > 0.0 && m.triangles[t].iter().all(|&i| (m.normals[i as usize] - f).norm() < 1e-6), "triangle {t}");
        }
        assert_eq!(parts[0].material.color, [0.6, 0.4, 0.2]);
    }
}
