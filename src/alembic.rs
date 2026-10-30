//! Alembic import (`.abc`, the Ogawa kind; see [`crate::ogawa`]): the
//! objects' transforms and polygon meshes at any time, as the same
//! [`Part`]s as [`mesh::read_obj`](crate::mesh::read_obj), in world space.
//!
//! - The archive's root group holds the format and library versions, the
//!   root object, the archive's metadata, its time samplings and the
//!   metadata strings properties and objects refer to by index.
//! - An object's group holds its properties (a compound), its children's
//!   groups and, last, their headers (name and metadata). A compound's
//!   group holds its properties' groups and, last, their headers, packed:
//!   kind, value type, extent, sample count, the first and last sample that
//!   differ (samples outside them aren't stored), time sampling and name.
//! - A scalar property's group holds its stored samples; an array's, pairs
//!   of samples and their dimensions. Every sample starts with a 16-byte
//!   key (a hash of it), skipped.
//! - Transforms (`AbcGeom_Xform_v3`): `.ops` (each a kind: scale,
//!   translate, rotate about an axis, a matrix, or rotate about X, Y or Z)
//!   and `.vals` (their values, a matrix in Imath's row-vector order),
//!   applied first to last as written, and `.inherits` (off: not under the
//!   parent's).
//! - Meshes (`AbcGeom_PolyMesh_v1`, and `AbcGeom_SubD_v1` read as its
//!   cage): `P`, `.faceIndices` and `.faceCounts`, polygons fanned into
//!   triangles, turned counter-clockwise (Alembic's are clockwise); `N` and
//!   `uv`, per vertex or per face corner, plain or indexed, when there.
//! - Objects whose `visible` is 0 are left out, with what's under them.
//!   Curves, points, cameras, lights and materials aren't read; meshes get
//!   a plain white Lambert material, as an OBJ without one does.
//!
//! Times are seconds; a property's sample for a time is its nearest.

use crate::geom::{Mat4, Vec3};
use crate::mesh::{Material, Mesh, Part, Shading};
use crate::ogawa::{Child, Ogawa};
use anyhow::{anyhow, bail, Context, Result};
use nalgebra::{Rotation3, Unit};
use std::path::Path;

/// When a property's samples are: every `per_cycle` seconds from the
/// first of `times` (one time), the same times every cycle (several), or
/// just `times` (acyclic).
#[derive(Clone, Debug, PartialEq)]
struct TimeSampling {
    per_cycle: f64,
    times: Vec<f64>,
}

/// Alembic's mark for acyclic sampling's time per cycle.
const ACYCLIC: f64 = f64::MAX / 32.0;

impl TimeSampling {
    /// Sample `i`'s time.
    fn time(&self, i: usize) -> f64 {
        let k = self.times.len().max(1);
        if self.per_cycle >= ACYCLIC * 0.5 {
            return self.times.get(i).or(self.times.last()).copied().unwrap_or(0.0);
        }
        self.times.get(i % k).copied().unwrap_or(0.0) + (i / k) as f64 * self.per_cycle
    }

    /// The sample of `n` nearest `t`.
    fn nearest(&self, n: usize, t: f64) -> usize {
        if n <= 1 {
            return 0;
        }
        // Times only grow: the first at or past t, or the one before it.
        let (mut lo, mut hi) = (0, n - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.time(mid) < t { lo = mid + 1 } else { hi = mid }
        }
        if lo > 0 && t - self.time(lo - 1) <= self.time(lo) - t { lo - 1 } else { lo }
    }
}

/// A property's stored samples.
#[derive(Clone, Debug)]
struct Samples {
    pod: u8,
    count: u32,
    /// The first and last sample index that differ from the one before.
    first: u32,
    last: u32,
    sampling: u32,
    group: Child,
}

#[derive(Clone, Debug)]
enum Kind {
    Compound(Vec<Prop>),
    Scalar(Samples),
    Array(Samples),
}

#[derive(Clone, Debug)]
struct Prop {
    name: String,
    meta: String,
    kind: Kind,
}

/// An object in the archive's tree.
#[derive(Clone, Debug)]
pub struct Object {
    pub name: String,
    /// Its path from the root: "/rig/box".
    pub path: String,
    pub meta: String,
    props: Vec<Prop>,
    pub children: Vec<Object>,
}

impl Object {
    /// Its schema, if any ("AbcGeom_Xform_v3", ...).
    pub fn schema(&self) -> &str {
        meta_value(&self.meta, "schema").unwrap_or("")
    }
}

/// An open Alembic archive: its object tree read, samples read as needed.
pub struct Archive {
    file: Ogawa,
    samplings: Vec<TimeSampling>,
    pub meta: String,
    pub root: Object,
}

/// `key`'s value in Alembic metadata ("k=v;k=v").
fn meta_value<'a>(meta: &'a str, key: &str) -> Option<&'a str> {
    meta.split(';').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

/// Reads through a byte buffer, every read checked.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| anyhow!("a header ends early"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// An unsigned number of 1, 2 or 4 bytes (`hint` 0, 1, 2).
    fn sized(&mut self, hint: u32) -> Result<u32> {
        Ok(match hint {
            0 => self.u8()? as u32,
            1 => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as u32,
            _ => self.u32()?,
        })
    }
    fn string(&mut self, n: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn done(&self) -> bool {
        self.at >= self.b.len()
    }
}

/// Bytes per value of POD type `pod` (bool, u8, i8, u16, i16, u32, i32,
/// u64, i64, f16, f32, f64); None for strings.
fn pod_size(pod: u8) -> Option<usize> {
    Some(match pod {
        0..=2 => 1,
        3 | 4 | 9 => 2,
        5 | 6 | 10 => 4,
        7 | 8 | 11 => 8,
        _ => return None,
    })
}

/// Values of POD type `pod` as numbers.
fn numbers(pod: u8, b: &[u8]) -> Result<Vec<f64>> {
    let size = pod_size(pod).ok_or_else(|| anyhow!("a string property where numbers were expected"))?;
    Ok(b.chunks_exact(size)
        .map(|c| match pod {
            0 | 1 => c[0] as f64,
            2 => c[0] as i8 as f64,
            3 => u16::from_le_bytes([c[0], c[1]]) as f64,
            4 => i16::from_le_bytes([c[0], c[1]]) as f64,
            5 => u32::from_le_bytes(c.try_into().unwrap()) as f64,
            6 => i32::from_le_bytes(c.try_into().unwrap()) as f64,
            7 => u64::from_le_bytes(c.try_into().unwrap()) as f64,
            8 => i64::from_le_bytes(c.try_into().unwrap()) as f64,
            9 => half(u16::from_le_bytes([c[0], c[1]])),
            10 => f32::from_le_bytes(c.try_into().unwrap()) as f64,
            _ => f64::from_le_bytes(c.try_into().unwrap()),
        })
        .collect())
}

/// An IEEE half float's value.
fn half(h: u16) -> f64 {
    let (sign, exp, frac) = (if h >> 15 == 1 { -1.0 } else { 1.0 }, ((h >> 10) & 31) as i32, (h & 1023) as f64);
    sign * match exp {
        0 => frac / 1024.0 * 2f64.powi(-14),
        31 => if frac == 0.0 { f64::INFINITY } else { f64::NAN },
        _ => (1.0 + frac / 1024.0) * 2f64.powi(exp - 15),
    }
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive> {
        let mut head = [0u8; 4];
        let hdf5 = std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head == b"\x89HDF";
        if hdf5 {
            bail!("{}: an HDF5 Alembic file isn't supported: export it as Ogawa (the default since Alembic 1.5)", path.display());
        }
        let file = Ogawa::open(path).with_context(|| format!("reading {}", path.display()))?;
        Self::read(file).with_context(|| format!("reading {}", path.display()))
    }

    fn read(file: Ogawa) -> Result<Archive> {
        let root = file.root()?;
        if root.len() < 6 {
            bail!("not an Alembic archive ({} root entries)", root.len());
        }
        let meta = String::from_utf8_lossy(&file.data_of(root[3])?).into_owned();
        // Time samplings: max sample, time per cycle, then the times.
        let ts = file.data_of(root[4])?;
        let mut r = Reader { b: &ts, at: 0 };
        let mut samplings = Vec::new();
        while !r.done() {
            let _max = r.u32()?;
            let per_cycle = r.f64()?;
            let n = r.u32()? as usize;
            let times = (0..n).map(|_| r.f64()).collect::<Result<Vec<_>>>()?;
            samplings.push(TimeSampling { per_cycle, times });
        }
        // Indexed metadata: entry i (from 1) as a u8 size and the text; 0 is
        // none.
        let im = file.data_of(root[5])?;
        let mut r = Reader { b: &im, at: 0 };
        let mut indexed = vec![String::new()];
        while !r.done() {
            let n = r.u8()? as usize;
            indexed.push(r.string(n)?);
        }
        let mut archive = Archive { file, samplings, meta, root: Object { name: String::new(), path: "/".into(), meta: String::new(), props: Vec::new(), children: Vec::new() } };
        archive.root = archive.object(root[2], String::new(), "/".into(), String::new(), &indexed, 0)?;
        Ok(archive)
    }

    fn object(&self, at: Child, name: String, path: String, meta: String, indexed: &[String], depth: usize) -> Result<Object> {
        if depth > 256 {
            bail!("objects nested too deep");
        }
        let kids = self.file.group_of(at)?;
        let props = match kids.first() {
            Some(&c) => self.compound(c, indexed, 0)?,
            None => Vec::new(),
        };
        let mut children = Vec::new();
        if kids.len() >= 2 {
            let headers = self.file.data_of(kids[kids.len() - 1])?;
            // The children's headers, then two 16-byte hashes.
            let body = &headers[..headers.len().saturating_sub(32)];
            let mut r = Reader { b: body, at: 0 };
            for &child in &kids[1..kids.len() - 1] {
                let n = r.u32()? as usize;
                let name = r.string(n)?;
                let mi = r.u8()?;
                let meta = if mi == 0xff {
                    let n = r.u32()? as usize;
                    r.string(n)?
                } else {
                    indexed.get(mi as usize).cloned().unwrap_or_default()
                };
                let p = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
                children.push(self.object(child, name, p, meta, indexed, depth + 1)?);
            }
        }
        Ok(Object { name, path, meta, props, children })
    }

    fn compound(&self, at: Child, indexed: &[String], depth: usize) -> Result<Vec<Prop>> {
        if depth > 256 {
            bail!("properties nested too deep");
        }
        let kids = self.file.group_of(at)?;
        let Some((&last, groups)) = kids.split_last() else { return Ok(Vec::new()) };
        let headers = self.file.data_of(last)?;
        let mut r = Reader { b: &headers, at: 0 };
        let mut out = Vec::new();
        for &g in groups {
            let info = r.u32()?;
            let (kind, hint) = (info & 3, (info >> 2) & 3);
            let mut samples = None;
            if kind != 0 {
                let count = r.sized(hint)?;
                let (first, last) = if info & 0x200 != 0 {
                    (r.sized(hint)?, r.sized(hint)?)
                } else if info & 0x800 != 0 {
                    (0, 0)
                } else {
                    (1, count.saturating_sub(1))
                };
                let sampling = if info & 0x100 != 0 { r.sized(hint)? } else { 0 };
                samples = Some(Samples { pod: ((info >> 4) & 15) as u8, count, first, last, sampling, group: g });
            }
            let n = r.sized(hint)? as usize;
            let name = r.string(n)?;
            let mi = (info >> 20) & 255;
            let meta = if mi == 255 {
                let n = r.sized(hint)? as usize;
                r.string(n)?
            } else {
                indexed.get(mi as usize).cloned().unwrap_or_default()
            };
            let kind = match (kind, samples) {
                (0, _) => Kind::Compound(self.compound(g, indexed, depth + 1)?),
                (1, Some(s)) => Kind::Scalar(s),
                (_, Some(s)) => Kind::Array(s),
                _ => unreachable!(),
            };
            out.push(Prop { name, meta, kind });
        }
        Ok(out)
    }

    /// Sample `i` of `s`'s stored samples (array samples sit with their
    /// dimensions, two to a sample), its key dropped.
    fn sample(&self, s: &Samples, array: bool, i: usize) -> Result<Vec<u8>> {
        let (first, last) = (s.first as usize, s.last as usize);
        // Sample 0, then first..=last; all the same when both are 0.
        let stored = if i < first || last == 0 { 0 } else if i >= last { last + 1 - first } else { i + 1 - first };
        let kids = self.file.group_of(s.group)?;
        let k = if array { 2 * stored } else { stored };
        let c = *kids.get(k).ok_or_else(|| anyhow!("sample {i} is missing"))?;
        let mut b = self.file.data_of(c)?;
        Ok(if b.len() >= 16 { b.split_off(16) } else { Vec::new() })
    }

    /// The sample of `s` nearest time `t`, as numbers.
    fn values(&self, s: &Samples, array: bool, t: f64) -> Result<Vec<f64>> {
        let ts = self.samplings.get(s.sampling as usize).ok_or_else(|| anyhow!("time sampling {} is missing", s.sampling))?;
        let i = ts.nearest(s.count as usize, t);
        numbers(s.pod, &self.sample(s, array, i)?)
    }

    /// The earliest and latest sample of anything that changes.
    pub fn time_range(&self) -> Option<(f64, f64)> {
        fn walk(a: &Archive, props: &[Prop], r: &mut Option<(f64, f64)>) {
            for p in props {
                match &p.kind {
                    Kind::Compound(c) => walk(a, c, r),
                    // Constant ones (all samples the same) don't say.
                    Kind::Scalar(s) | Kind::Array(s) if s.count > 1 && s.last > 0 => {
                        if let Some(ts) = a.samplings.get(s.sampling as usize) {
                            let (lo, hi) = (ts.time(0), ts.time(s.count as usize - 1));
                            *r = Some(r.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                        }
                    }
                    _ => {}
                }
            }
        }
        fn objects(a: &Archive, o: &Object, r: &mut Option<(f64, f64)>) {
            walk(a, &o.props, r);
            o.children.iter().for_each(|c| objects(a, c, r));
        }
        let mut r = None;
        objects(self, &self.root, &mut r);
        r
    }

    /// Every visible mesh at time `t` (seconds), in world space.
    pub fn parts(&self, t: f64) -> Result<Vec<Part>> {
        let mut out = Vec::new();
        for c in &self.root.children {
            self.collect(c, &Mat4::identity(), t, &mut out).with_context(|| format!("in {}", c.path))?;
        }
        Ok(out)
    }

    fn collect(&self, o: &Object, parent: &Mat4, t: f64, out: &mut Vec<Part>) -> Result<()> {
        if let Some(Kind::Scalar(s)) = find(&o.props, "visible").map(|p| &p.kind) {
            if self.values(s, false, t)?.first() == Some(&0.0) {
                return Ok(());
            }
        }
        let mut world = *parent;
        match o.schema() {
            "AbcGeom_Xform_v3" => {
                if let Some(Kind::Compound(x)) = find(&o.props, ".xform").map(|p| &p.kind) {
                    let (local, inherits) = self.xform(x, t)?;
                    world = if inherits { parent * local } else { local };
                }
            }
            "AbcGeom_PolyMesh_v1" | "AbcGeom_SubD_v1" => {
                if let Some(Kind::Compound(g)) = find(&o.props, ".geom").map(|p| &p.kind) {
                    let mesh = self.mesh(g, t)?;
                    if !mesh.triangles.is_empty() {
                        let material = Material { name: o.name.clone(), shading: Shading::Lambert, ..Material::default() };
                        out.push(Part { name: o.path.trim_start_matches('/').to_string(), mesh: placed(mesh, &world), material });
                    }
                }
            }
            _ => {}
        }
        for c in &o.children {
            self.collect(c, &world, t, out).with_context(|| format!("in {}", c.path))?;
        }
        Ok(())
    }

    /// A transform's matrix at `t` (column vectors) and whether it's under
    /// its parent's.
    fn xform(&self, x: &[Prop], t: f64) -> Result<(Mat4, bool)> {
        let get = |name: &str| -> Result<Option<Vec<f64>>> {
            match find(x, name).map(|p| &p.kind) {
                Some(Kind::Scalar(s)) => self.values(s, false, t).map(Some),
                Some(Kind::Array(s)) => self.values(s, true, t).map(Some),
                _ => Ok(None),
            }
        };
        let inherits = get(".inherits")?.and_then(|v| v.first().copied()).is_none_or(|v| v != 0.0);
        let ops = get(".ops")?.unwrap_or_default();
        let vals = get(".vals")?;
        let mut at = 0;
        let mut m = Mat4::identity();
        for op in ops {
            let kind = op as u8 >> 4;
            let (n, default) = match kind {
                0 => (3, 1.0),
                1 => (3, 0.0),
                2 => (4, 0.0),
                3 => (16, f64::NAN),
                4..=6 => (1, 0.0),
                k => bail!("unknown transform op {k}"),
            };
            // Without values (all at their defaults), defaults.
            let v: Vec<f64> = match &vals {
                Some(vals) => {
                    let v = vals.get(at..at + n).ok_or_else(|| anyhow!("transform values run short"))?.to_vec();
                    at += n;
                    v
                }
                None if kind == 3 => Mat4::identity().transpose().as_slice().to_vec(),
                None => vec![default; n],
            };
            let op_m = match kind {
                0 => Mat4::new_nonuniform_scaling(&Vec3::new(v[0], v[1], v[2])),
                1 => Mat4::new_translation(&Vec3::new(v[0], v[1], v[2])),
                2 => {
                    let axis = Vec3::new(v[0], v[1], v[2]);
                    if axis.norm() < 1e-12 { Mat4::identity() } else { Rotation3::from_axis_angle(&Unit::new_normalize(axis), v[3].to_radians()).to_homogeneous() }
                }
                // Imath's row-vector matrix, row by row: transposed.
                3 => Mat4::from_row_slice(&v).transpose(),
                k => Rotation3::from_axis_angle(&[Vec3::x_axis(), Vec3::y_axis(), Vec3::z_axis()][(k - 4) as usize], v[0].to_radians()).to_homogeneous(),
            };
            m *= op_m;
        }
        Ok((m, inherits))
    }

    /// A mesh schema's mesh at `t`, in its object's space.
    fn mesh(&self, g: &[Prop], t: f64) -> Result<Mesh> {
        let array = |name: &str| -> Result<Option<Vec<f64>>> {
            match find(g, name).map(|p| &p.kind) {
                Some(Kind::Array(s)) => self.values(s, true, t).map(Some),
                _ => Ok(None),
            }
        };
        let p = array("P")?.ok_or_else(|| anyhow!("a mesh without P"))?;
        let points: Vec<Vec3> = p.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0], c[1], c[2])).collect();
        let indices: Vec<usize> = array(".faceIndices")?.unwrap_or_default().iter().map(|&i| i as usize).collect();
        let counts: Vec<usize> = array(".faceCounts")?.unwrap_or_default().iter().map(|&c| c.max(0.0) as usize).collect();
        if counts.iter().sum::<usize>() != indices.len() {
            bail!("face counts add up to {}, not the {} face indices", counts.iter().sum::<usize>(), indices.len());
        }
        if let Some(&bad) = indices.iter().find(|&&i| i >= points.len()) {
            bail!("face index {bad} past the {} points", points.len());
        }
        let normals = self.geom_param(g, "N", 3, t, points.len(), indices.len())?;
        let uvs = self.geom_param(g, "uv", 2, t, points.len(), indices.len())?;
        // Per corner values need a vertex per corner; otherwise shared.
        let per_corner = matches!(normals, Some((_, true))) || matches!(uvs, Some((_, true)));
        let mut mesh = Mesh::default();
        let corner = |vals: &Option<(Vec<f64>, bool)>, dim: usize, c: usize| -> Option<Vec<f64>> {
            let (v, fv) = vals.as_ref()?;
            let i = if *fv { c } else { indices[c] };
            v.get(i * dim..i * dim + dim).map(<[f64]>::to_vec)
        };
        if per_corner {
            for c in 0..indices.len() {
                mesh.positions.push(points[indices[c]]);
                if let Some(n) = corner(&normals, 3, c) {
                    mesh.normals.push(Vec3::new(n[0], n[1], n[2]));
                }
                if let Some(uv) = corner(&uvs, 2, c) {
                    mesh.uvs.push([uv[0], uv[1]]);
                }
            }
        } else {
            mesh.positions = points.clone();
            if let Some((n, _)) = &normals {
                mesh.normals = n.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0], c[1], c[2])).collect();
            }
            if let Some((uv, _)) = &uvs {
                mesh.uvs = uv.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]).collect();
            }
        }
        // All or nothing: a short list is dropped.
        if mesh.normals.len() != mesh.positions.len() {
            mesh.normals.clear();
        }
        mesh.normals.iter_mut().for_each(|n| *n = n.try_normalize(1e-12).unwrap_or_else(Vec3::y));
        if mesh.uvs.len() != mesh.positions.len() {
            mesh.uvs.clear();
        }
        // Fans, turned to counter-clockwise.
        let mut start = 0;
        for &n in &counts {
            let v = |k: usize| (if per_corner { start + k } else { indices[start + k] }) as u32;
            for k in 1..n.saturating_sub(1) {
                mesh.triangles.push([v(0), v(k + 1), v(k)]);
            }
            start += n;
        }
        Ok(mesh)
    }

    /// Geometry parameter `name` (`dim` numbers a value) at `t`: its
    /// values, one per vertex or (true) per face corner, its indices
    /// applied. None when it isn't there or its scope is another.
    fn geom_param(&self, g: &[Prop], name: &str, dim: usize, t: f64, vertices: usize, corners: usize) -> Result<Option<(Vec<f64>, bool)>> {
        let Some(p) = find(g, name) else { return Ok(None) };
        let (vals, meta) = match &p.kind {
            Kind::Array(s) => (self.values(s, true, t)?, p.meta.as_str()),
            Kind::Compound(c) => {
                let Some(Prop { kind: Kind::Array(s), meta, .. }) = find(c, ".vals") else { return Ok(None) };
                let vals = self.values(s, true, t)?;
                match find(c, ".indices").map(|p| &p.kind) {
                    Some(Kind::Array(si)) => {
                        let mut out = Vec::new();
                        for i in self.values(si, true, t)? {
                            let i = i as usize;
                            out.extend_from_slice(vals.get(i * dim..i * dim + dim).ok_or_else(|| anyhow!("{name} index {i} past its values"))?);
                        }
                        (out, if p.meta.is_empty() { meta.as_str() } else { p.meta.as_str() })
                    }
                    _ => (vals, meta.as_str()),
                }
            }
            Kind::Scalar(_) => return Ok(None),
        };
        let n = vals.len() / dim;
        Ok(match meta_value(meta, "geoScope") {
            Some("fvr") if n == corners => Some((vals, true)),
            Some("vtx" | "var") | None if n == vertices => Some((vals, false)),
            _ => None,
        })
    }
}

fn find<'a>(props: &'a [Prop], name: &str) -> Option<&'a Prop> {
    props.iter().find(|p| p.name == name)
}

/// `mesh` moved by `world`, its normals turned with it.
fn placed(mut mesh: Mesh, world: &Mat4) -> Mesh {
    if *world == Mat4::identity() {
        return mesh;
    }
    let normal = world.fixed_view::<3, 3>(0, 0).try_inverse().map(|m| m.transpose()).unwrap_or_else(nalgebra::Matrix3::identity);
    mesh.positions.iter_mut().for_each(|p| *p = crate::geom::transform_point(world, p));
    mesh.normals.iter_mut().for_each(|n| *n = (normal * *n).try_normalize(1e-12).unwrap_or(*n));
    // A mirroring transform turns faces inside out: turn them back.
    if world.fixed_view::<3, 3>(0, 0).determinant() < 0.0 {
        mesh.triangles.iter_mut().for_each(|t| t.swap(1, 2));
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Archive {
        Archive::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geo/anim.abc")).unwrap()
    }

    /// examples/geo/anim.abc, from Blender 5.2 (Z up there, Y up in the
    /// file), 24 fps, frames 1–10 at 1/24 s on: an empty "rig" moving 3
    /// along X and turning 90° about the up axis, a 1-unit cube "box" under
    /// it 1 up; a 2 × 2 grid "sheet" 1 down, waving up to 0.5 sin 2x by
    /// frame 10 (shape key). Made by running, with `out` the file:
    ///
    /// ```text
    /// blender -b --factory-startup --python make.py -- out
    /// ```
    ///
    /// (make.py: read factory settings empty, the empty keyed at frames 1
    /// and 10 (Blender eases them), the cube added at (0, 0, 1) and parented, the
    /// grid with x/y subdivisions 4/3 at (0, 0, −1) with a "wave" shape key
    /// keyed 0 → 1, then `wm.alembic_export` with uvs and normals.)
    fn at_frame(a: &Archive, f: f64) -> Vec<Part> {
        a.parts(f / 24.0).unwrap()
    }

    #[test]
    fn the_sample_reads_its_tree_transforms_and_meshes() {
        let a = sample();
        assert!(a.meta.contains("FramesPerTimeUnit=24"));
        let names: Vec<&str> = a.root.children.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(names, ["/rig", "/sheet"]);
        assert_eq!((a.root.children[0].schema(), a.root.children[0].children[0].children[0].schema()), ("AbcGeom_Xform_v3", "AbcGeom_PolyMesh_v1"));
        let (lo, hi) = a.time_range().unwrap();
        assert!((lo - 1.0 / 24.0).abs() < 1e-9 && (hi - 10.0 / 24.0).abs() < 1e-9, "{lo} {hi}");
        let parts = at_frame(&a, 1.0);
        assert_eq!(parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["rig/box/Cube", "sheet/Grid"]);
        let center = |m: &Mesh| m.positions.iter().sum::<Vec3>() / m.positions.len() as f64;
        let cube = &parts[0].mesh;
        // 6 quads: 12 triangles; a normal per corner, so 24 vertices.
        assert_eq!((cube.triangles.len(), cube.positions.len(), cube.normals.len()), (12, 24, 24));
        assert!((center(cube) - Vec3::new(0.0, 1.0, 0.0)).norm() < 1e-6, "{}", center(cube));
        // Counter-clockwise: each triangle faces along its normals.
        for t in &cube.triangles {
            let [a, b, c] = t.map(|i| cube.positions[i as usize]);
            let n = (b - a).cross(&(c - a)).normalize();
            assert!(n.dot(&cube.normals[t[0] as usize]) > 0.99, "{n} vs {}", cube.normals[t[0] as usize]);
            assert!(n.dot(&(a - Vec3::new(0.0, 1.0, 0.0))) > 0.0, "facing out");
        }
        // Frame 10: moved 3 along X and turned 90° about Y.
        let end = at_frame(&a, 10.0);
        let cube = &end[0].mesh;
        assert!((center(cube) - Vec3::new(3.0, 1.0, 0.0)).norm() < 1e-6, "{}", center(cube));
        let corner = |m: &Mesh, want: Vec3| m.positions.iter().any(|p| (p - want).norm() < 1e-5);
        // A corner at (0.5, 1.5, 0.5) under the rig turns to (0.5, 1.5, −0.5).
        assert!(corner(&parts[0].mesh, Vec3::new(0.5, 1.5, 0.5)) && corner(cube, Vec3::new(3.5, 1.5, -0.5)));
        let normal_turned = cube.normals.iter().zip(&parts[0].mesh.normals).all(|(n, m)| (n - Vec3::new(m.z, m.y, -m.x)).norm() < 1e-5);
        assert!(normal_turned, "normals turn with it");
        // Between (Blender eases the keys): on the way, frame by frame;
        // between frames, the nearest.
        let xs: Vec<f64> = (1..=10).map(|f| center(&at_frame(&a, f as f64)[0].mesh).x).collect();
        assert!(xs.windows(2).all(|w| w[1] > w[0]) && xs[4] > 0.5 && xs[4] < 2.5, "{xs:?}");
        assert_eq!((center(&at_frame(&a, 5.4)[0].mesh).x, center(&at_frame(&a, 5.6)[0].mesh).x), (xs[4], xs[5]));
        // Before and after the samples: the first and last.
        assert_eq!(at_frame(&a, -20.0)[0].mesh.positions, parts[0].mesh.positions);
        assert_eq!(at_frame(&a, 99.0)[0].mesh.positions, cube.positions);
        // The sheet deforms: flat at frame 1, the wave at 10; with UVs.
        let (flat, wave) = (&parts[1].mesh, &end[1].mesh);
        assert_eq!((flat.triangles.len(), flat.uvs.len()), (wave.triangles.len(), wave.positions.len()));
        assert!(flat.triangles.len() >= 12 && flat.positions.iter().all(|p| (p.y + 1.0).abs() < 1e-6 && p.x.abs() <= 1.0 + 1e-6 && p.z.abs() <= 1.0 + 1e-6));
        assert!(wave.positions.iter().all(|p| (p.y - (-1.0 + 0.5 * (2.0 * p.x).sin())).abs() < 1e-5));
        assert!(wave.uvs.iter().all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])) && wave.uvs.iter().any(|uv| uv[0] > 0.9));
    }

    /// Not Ogawa, HDF5 or cut short: errors that say so.
    #[test]
    fn broken_and_hdf5_files_say_so() {
        let dir = std::env::temp_dir().join(format!("arclight_alembic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = |name: &str, b: &[u8]| {
            std::fs::write(dir.join(name), b).unwrap();
            format!("{:#}", Archive::open(&dir.join(name)).err().unwrap())
        };
        assert!(err("h.abc", b"\x89HDF\r\n\x1a\n more").contains("HDF5"));
        let good = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geo/anim.abc")).unwrap();
        assert!(err("cut.abc", &good[..good.len() / 2]).contains("ends early"));
        // An Ogawa file that isn't an archive.
        let mut w = crate::ogawa::tests::Writer::new();
        let d = w.data(b"x");
        let root = w.group(&[d]);
        assert!(err("o.abc", &w.finish(root)).contains("not an Alembic archive"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn time_samplings_find_the_nearest_sample() {
        let uniform = TimeSampling { per_cycle: 1.0 / 24.0, times: vec![1.0 / 24.0] };
        assert_eq!((uniform.nearest(10, 0.0), uniform.nearest(10, 3.0 / 24.0 + 0.01), uniform.nearest(10, 5.0)), (0, 2, 9));
        let cyclic = TimeSampling { per_cycle: 1.0, times: vec![0.0, 0.25] };
        assert_eq!((cyclic.time(3), cyclic.nearest(6, 1.3)), (1.25, 3));
        let acyclic = TimeSampling { per_cycle: ACYCLIC, times: vec![0.0, 0.1, 0.5] };
        assert_eq!((acyclic.nearest(3, 0.32), acyclic.nearest(3, 0.29), acyclic.time(7)), (2, 1, 0.5));
        assert_eq!((meta_value("a=1;schema=X", "schema"), meta_value("a=1", "b")), (Some("X"), None));
        assert!((half(0x3c00) - 1.0).abs() < 1e-12 && (half(0xc000) + 2.0).abs() < 1e-12 && half(0x7c00).is_infinite());
    }
}
