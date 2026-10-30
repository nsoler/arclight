//! Gaussian splats (3D Gaussian Splatting captures): what a Read Splat
//! node puts in a scene ([`crate::scene::Scene::splats`]) and the renderer
//! draws. Each splat is a 3D Gaussian: a center, a size along each of its
//! own axes, a turn, an opacity, and a color that can change with the
//! direction it's seen from (spherical harmonics, degree 0 to 3).
//!
//! Kept as arrays of f32 (captures run to millions of splats), each array
//! shared ([`Arc`]): placing a capture (its [`world`](Splats::world)) or
//! tagging it copies none of them.

use crate::geom::{self, Mat4, Vec3};
use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Gaussian splats read from a file, placed by `world`.
#[derive(Clone, Debug, Default)]
pub struct Splats {
    /// Centers, in the capture's own space.
    pub positions: Arc<Vec<[f32; 3]>>,
    /// Sizes (1σ) along each splat's own x, y and z: linear (a file's log
    /// scales, exp'd).
    pub scales: Arc<Vec<[f32; 3]>>,
    /// Turns, unit quaternions as [w, x, y, z] (a file's order).
    pub rotations: Arc<Vec<[f32; 4]>>,
    /// 0 to 1 (a file's logits, sigmoid'd).
    pub opacity: Arc<Vec<f32>>,
    /// Color as spherical harmonics, [`sh_stride`](Splats::sh_stride) per
    /// splat: coefficient by coefficient, each as R, G, B (so the first
    /// three are the DC term). See [`Splats::color`].
    pub sh: Arc<Vec<f32>>,
    /// 0 to 3.
    pub sh_degree: u8,
    /// Where the capture's space sits in the scene.
    pub world: Mat4,
    /// The node it came from ("" until tagged).
    pub node: String,
}

/// The SH basis' constants (as in the reference 3DGS rasterizer).
const SH_C0: f32 = 0.282_094_8;
const SH_C1: f32 = 0.488_602_5;
const SH_C2: [f32; 5] = [1.092_548_4, -1.092_548_4, 0.315_391_57, -1.092_548_4, 0.546_274_2];
const SH_C3: [f32; 7] = [-0.590_043_6, 2.890_611_4, -0.457_045_8, 0.373_176_33, -0.457_045_8, 1.445_305_7, -0.590_043_6];

/// SH coefficients per color channel at `degree`: (degree + 1)².
pub fn sh_coeffs(degree: u8) -> usize {
    (degree as usize + 1).pow(2)
}

impl Splats {
    /// How many splats.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Floats of [`sh`](Splats::sh) per splat: 3 × (degree + 1)².
    pub fn sh_stride(&self) -> usize {
        3 * sh_coeffs(self.sh_degree)
    }

    /// Splat `i`'s SH coefficients.
    pub fn sh_of(&self, i: usize) -> &[f32] {
        let n = self.sh_stride();
        &self.sh[i * n..(i + 1) * n]
    }

    /// Roughly the memory its arrays hold.
    pub fn bytes(&self) -> usize {
        self.len() * (12 + 12 + 16 + 4) + self.sh.len() * 4 + 256
    }

    /// A box around the splats' centers in scene space (the capture's own
    /// box, placed), or None when there are none.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let first = self.positions.first()?;
        let (mut lo, mut hi) = (*first, *first);
        for p in self.positions.iter() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let corner = |i: usize| Vec3::new([lo[0], hi[0]][i & 1] as f64, [lo[1], hi[1]][(i >> 1) & 1] as f64, [lo[2], hi[2]][(i >> 2) & 1] as f64);
        let placed: Vec<Vec3> = (0..8).map(|i| geom::transform_point(&self.world, &corner(i))).collect();
        let fold = |f: fn(f64, f64) -> f64, init: f64| Vec3::new(placed.iter().map(|p| p.x).fold(init, f), placed.iter().map(|p| p.y).fold(init, f), placed.iter().map(|p| p.z).fold(init, f));
        Some((fold(f64::min, f64::INFINITY), fold(f64::max, f64::NEG_INFINITY)))
    }

    /// Splat `i`'s color (linear RGB, as the capture stored it) seen along
    /// `dir`: the unit direction from the viewer to the splat, in the
    /// capture's own space. The SH sum plus 0.5, at least 0 (as the 3DGS
    /// reference does); degree 0 doesn't depend on `dir`.
    pub fn color(&self, i: usize, dir: [f32; 3]) -> [f32; 3] {
        sh_color(self.sh_of(i), self.sh_degree, dir)
    }
}

/// The color the SH coefficients `sh` (degree `degree`, laid out as in
/// [`Splats::sh`]) give along unit direction `dir`.
pub fn sh_color(sh: &[f32], degree: u8, dir: [f32; 3]) -> [f32; 3] {
    let c = |k: usize, ch: usize| sh[k * 3 + ch];
    let mut out = [0.0f32; 3];
    let [x, y, z] = dir;
    for (ch, o) in out.iter_mut().enumerate() {
        let mut v = SH_C0 * c(0, ch);
        if degree >= 1 {
            v += SH_C1 * (-y * c(1, ch) + z * c(2, ch) - x * c(3, ch));
        }
        if degree >= 2 {
            let (xx, yy, zz, xy, yz, xz) = (x * x, y * y, z * z, x * y, y * z, x * z);
            v += SH_C2[0] * xy * c(4, ch) + SH_C2[1] * yz * c(5, ch) + SH_C2[2] * (2.0 * zz - xx - yy) * c(6, ch) + SH_C2[3] * xz * c(7, ch) + SH_C2[4] * (xx - yy) * c(8, ch);
            if degree >= 3 {
                v += SH_C3[0] * y * (3.0 * xx - yy) * c(9, ch)
                    + SH_C3[1] * xy * z * c(10, ch)
                    + SH_C3[2] * y * (4.0 * zz - xx - yy) * c(11, ch)
                    + SH_C3[3] * z * (2.0 * zz - 3.0 * xx - 3.0 * yy) * c(12, ch)
                    + SH_C3[4] * x * (4.0 * zz - xx - yy) * c(13, ch)
                    + SH_C3[5] * z * (xx - yy) * c(14, ch)
                    + SH_C3[6] * x * (xx - 3.0 * yy) * c(15, ch);
            }
        }
        *o = (v + 0.5).max(0.0);
    }
    out
}

/// The DC coefficient that gives color `c` (0..1) at degree 0: what
/// [`sh_color`] undoes.
pub fn dc_of(c: f32) -> f32 {
    (c - 0.5) / SH_C0
}

/// Captures read so far, by file and SH degree cap, with the file's change
/// time (a few kept: captures are big).
type Cached = (PathBuf, u8, Option<std::time::SystemTime>, Arc<Splats>);
static CACHE: Mutex<Vec<Cached>> = Mutex::new(Vec::new());
const CACHED: usize = 3;

impl Splats {
    /// The splats in the file at `path` (see [`read_ply`]), their colors
    /// at most SH degree `max_degree`, read again only when the file
    /// changes. Placed nowhere yet (`world` the identity).
    pub fn read(path: &Path, max_degree: u8) -> Result<Arc<Splats>> {
        let modified = std::fs::metadata(path).with_context(|| format!("can't read {}", path.display()))?.modified().ok();
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = cache.iter().position(|c| c.0 == path && c.1 == max_degree && c.2 == modified) {
            let hit = cache.remove(i);
            let splats = hit.3.clone();
            cache.push(hit);
            return Ok(splats);
        }
        drop(cache);
        let splats = Arc::new(read_ply(path, max_degree)?);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|c| !(c.0 == path && c.1 == max_degree));
        if cache.len() >= CACHED {
            cache.remove(0);
        }
        cache.push((path.to_path_buf(), max_degree, modified, splats.clone()));
        Ok(splats)
    }
}

/// A property's type in a PLY file.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(name: &str) -> Result<Ty> {
        Ok(match name {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            other => bail!("unknown PLY type '{other}'"),
        })
    }

    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }

    /// The value at the start of `b` (little-endian).
    fn get(self, b: &[u8]) -> f64 {
        match self {
            Ty::I8 => b[0] as i8 as f64,
            Ty::U8 => b[0] as f64,
            Ty::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Ty::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Ty::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::F64 => f64::from_le_bytes(b[..8].try_into().unwrap()),
        }
    }
}

/// A property: its name and type, or for a list, its count's and items'
/// types.
#[derive(Clone, Debug)]
struct Prop {
    name: String,
    ty: Ty,
    list: Option<Ty>,
}

/// An element: its name, how many, and its properties.
#[derive(Clone, Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

/// Read the Gaussian splats in the PLY file at `path` (as the INRIA 3DGS
/// code and the trainers that follow it write them: binary little-endian
/// or ASCII; a `vertex` element with x, y, z, f_dc_0–2, f_rest_*, opacity,
/// scale_0–2 and rot_0–3), their colors at most SH degree `max_degree`
/// (the file's degree from how many f_rest it has; none: degree 0). A
/// plain point cloud's red, green and blue stand in for f_dc.
pub fn read_ply(path: &Path, max_degree: u8) -> Result<Splats> {
    let file = std::fs::File::open(path).with_context(|| format!("can't read {}", path.display()))?;
    let mut r = BufReader::new(file);
    read_ply_from(&mut r, max_degree).with_context(|| format!("reading {}", path.display()))
}

fn read_ply_from(r: &mut impl BufRead, max_degree: u8) -> Result<Splats> {
    // The header.
    let mut line = String::new();
    let mut next = |r: &mut dyn BufRead| -> Result<String> {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            bail!("the header ends early");
        }
        Ok(line.trim().to_string())
    };
    if next(r)? != "ply" {
        bail!("not a PLY file");
    }
    let (mut binary, mut elements) = (None, Vec::<Element>::new());
    loop {
        let l = next(r)?;
        let words: Vec<&str> = l.split_whitespace().collect();
        match words.as_slice() {
            ["format", "binary_little_endian", ..] => binary = Some(true),
            ["format", "ascii", ..] => binary = Some(false),
            ["format", other, ..] => bail!("PLY format '{other}' isn't supported (binary_little_endian or ascii)"),
            ["element", name, count] => elements.push(Element { name: name.to_string(), count: count.parse().context("element count")?, props: Vec::new() }),
            ["property", "list", count, item, name] => {
                let e = elements.last_mut().ok_or_else(|| anyhow!("a property before any element"))?;
                e.props.push(Prop { name: name.to_string(), ty: Ty::parse(item)?, list: Some(Ty::parse(count)?) });
            }
            ["property", ty, name] => {
                let e = elements.last_mut().ok_or_else(|| anyhow!("a property before any element"))?;
                e.props.push(Prop { name: name.to_string(), ty: Ty::parse(ty)?, list: None });
            }
            ["end_header"] => break,
            _ => {} // comments, obj_info
        }
    }
    let binary = binary.ok_or_else(|| anyhow!("no format line"))?;
    let vertex = elements.iter().position(|e| e.name == "vertex").ok_or_else(|| anyhow!("no vertex element"))?;
    let v = &elements[vertex];
    let at = |name: &str| v.props.iter().position(|p| p.name == name && p.list.is_none());
    let need = |name: &str| at(name).ok_or_else(|| anyhow!("not a Gaussian splat file: the vertices have no '{name}'"));
    let pos = [need("x")?, need("y")?, need("z")?];
    let scale = [need("scale_0")?, need("scale_1")?, need("scale_2")?];
    let rot = [need("rot_0")?, need("rot_1")?, need("rot_2")?, need("rot_3")?];
    let opacity = at("opacity");
    // Color: the DC term, else a point cloud's 8-bit (or 0–1) color.
    let dc: [usize; 3] = match (at("f_dc_0"), at("f_dc_1"), at("f_dc_2")) {
        (Some(a), Some(b), Some(c)) => [a, b, c],
        _ => [need("red")?, need("green")?, need("blue")?],
    };
    let rgb_dc = at("f_dc_0").is_none();
    let rest_count = (0..).take_while(|k| at(&format!("f_rest_{k}")).is_some()).count();
    let file_degree = [0, 9, 24, 45].iter().rposition(|&n| n <= rest_count).unwrap_or(0) as u8;
    let degree = file_degree.min(max_degree.min(3));
    let file_rest = sh_coeffs(file_degree) - 1;
    let rest: Vec<usize> = (0..rest_count).map(|k| at(&format!("f_rest_{k}")).unwrap()).collect();
    let n = v.count;
    let coeffs = sh_coeffs(degree);
    let stride = 3 * coeffs;
    // One vertex's row (every property, as f64) into a splat.
    let convert = |row: &[f64], position: &mut [f32; 3], size: &mut [f32; 3], turn: &mut [f32; 4], alpha: &mut f32, sh: &mut [f32]| {
        *position = pos.map(|i| row[i] as f32);
        *size = scale.map(|i| (row[i] as f32).exp());
        let q = rot.map(|i| row[i] as f32);
        let len = q.iter().map(|c| c * c).sum::<f32>().sqrt();
        *turn = if len > 1e-12 { q.map(|c| c / len) } else { [1.0, 0.0, 0.0, 0.0] };
        *alpha = opacity.map_or(1.0, |i| 1.0 / (1.0 + (-row[i] as f32).exp()));
        for (ch, &i) in dc.iter().enumerate() {
            sh[ch] = if rgb_dc { dc_of(row[i] as f32 / if row[i] > 1.0 { 255.0 } else { 1.0 }) } else { row[i] as f32 };
        }
        // The file's rest: channel by channel; ours: coefficient by coefficient.
        for k in 1..coeffs {
            for ch in 0..3 {
                sh[k * 3 + ch] = row[rest[ch * file_rest + k - 1]] as f32;
            }
        }
    };
    let (mut positions, mut scales, mut rotations, mut opacities, mut sh) = (vec![[0.0f32; 3]; n], vec![[0.0f32; 3]; n], vec![[1.0f32, 0.0, 0.0, 0.0]; n], vec![0.0f32; n], vec![0.0f32; n * stride]);
    for (e_index, e) in elements.iter().enumerate() {
        let lists = e.props.iter().any(|p| p.list.is_some());
        if e_index == vertex && binary && !lists {
            // The common case, fast: the whole block read at once, the
            // splats converted in parallel.
            use rayon::prelude::*;
            let offsets: Vec<usize> = e.props.iter().scan(0, |o, p| Some(std::mem::replace(o, *o + p.ty.size()))).collect();
            let size: usize = e.props.iter().map(|p| p.ty.size()).sum();
            let mut block = vec![0u8; n * size];
            r.read_exact(&mut block).context("the data ends early")?;
            positions
                .par_iter_mut()
                .zip(scales.par_iter_mut())
                .zip(rotations.par_iter_mut())
                .zip(opacities.par_iter_mut())
                .zip(sh.par_chunks_mut(stride))
                .zip(block.par_chunks_exact(size))
                .for_each(|(((((p, s), q), a), c), rec)| {
                    let row: Vec<f64> = e.props.iter().zip(&offsets).map(|(prop, &o)| prop.ty.get(&rec[o..])).collect();
                    convert(&row, p, s, q, a, c);
                });
            break;
        }
        let mut row = vec![0.0f64; e.props.len()];
        for k in 0..e.count {
            if binary {
                for (j, p) in e.props.iter().enumerate() {
                    let mut b = [0u8; 8];
                    match p.list {
                        None => {
                            r.read_exact(&mut b[..p.ty.size()]).context("the data ends early")?;
                            row[j] = p.ty.get(&b);
                        }
                        Some(count_ty) => {
                            r.read_exact(&mut b[..count_ty.size()]).context("the data ends early")?;
                            let items = count_ty.get(&b).max(0.0) as usize;
                            std::io::copy(&mut r.by_ref().take((items * p.ty.size()) as u64), &mut std::io::sink())?;
                        }
                    }
                }
            } else {
                let mut l = String::new();
                if r.read_line(&mut l)? == 0 {
                    bail!("the data ends early");
                }
                let mut words = l.split_whitespace();
                for (j, p) in e.props.iter().enumerate() {
                    let mut word = || -> Result<f64> { words.next().ok_or_else(|| anyhow!("a short line in the data"))?.parse().context("a number in the data") };
                    match p.list {
                        None => row[j] = word()?,
                        Some(_) => {
                            for _ in 0..word()? as usize {
                                word()?;
                            }
                        }
                    }
                }
            }
            if e_index == vertex {
                convert(&row, &mut positions[k], &mut scales[k], &mut rotations[k], &mut opacities[k], &mut sh[k * stride..(k + 1) * stride]);
            }
        }
        if e_index == vertex {
            // Nothing past the vertices is wanted.
            break;
        }
    }
    Ok(Splats {
        positions: Arc::new(positions),
        scales: Arc::new(scales),
        rotations: Arc::new(rotations),
        opacity: Arc::new(opacities),
        sh: Arc::new(sh),
        sh_degree: degree,
        world: Mat4::identity(),
        node: String::new(),
    })
}

/// Write `splats` (their own space; `world` isn't written) as a binary
/// little-endian 3DGS PLY file, as [`read_ply`] reads: log scales, logit
/// opacities, the SH rest channel by channel.
pub fn write_ply(path: &Path, splats: &Splats) -> Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).with_context(|| format!("can't write {}", path.display()))?);
    let rest = sh_coeffs(splats.sh_degree) - 1;
    let mut header = format!("ply\nformat binary_little_endian 1.0\nelement vertex {}\n", splats.len());
    let mut names: Vec<String> = ["x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2"].map(String::from).to_vec();
    names.extend((0..3 * rest).map(|k| format!("f_rest_{k}")));
    names.extend(["opacity", "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3"].map(String::from));
    for n in &names {
        header += &format!("property float {n}\n");
    }
    header += "end_header\n";
    w.write_all(header.as_bytes())?;
    for i in 0..splats.len() {
        let sh = splats.sh_of(i);
        let mut row: Vec<f32> = splats.positions[i].to_vec();
        row.extend([0.0; 3]);
        row.extend(&sh[..3]);
        for ch in 0..3 {
            row.extend((1..=rest).map(|k| sh[k * 3 + ch]));
        }
        let a = splats.opacity[i].clamp(1e-6, 1.0 - 1e-6);
        row.push((a / (1.0 - a)).ln());
        row.extend(splats.scales[i].map(|s| s.max(1e-30).ln()));
        row.extend(splats.rotations[i]);
        for v in row {
            w.write_all(&v.to_le_bytes())?;
        }
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three splats, degree 1, the middle one tinted by direction.
    pub(crate) fn three() -> Splats {
        let mut sh = vec![0.0f32; 3 * 3 * 4];
        for (i, c) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].iter().enumerate() {
            for ch in 0..3 {
                sh[i * 12 + ch] = dc_of(c[ch]);
            }
        }
        // Splat 1: redder seen along +x (its coefficient 3 is −x).
        sh[12 + 3 * 3] = -0.5;
        Splats {
            positions: Arc::new(vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-1.0, -2.0, -3.0]]),
            scales: Arc::new(vec![[0.1; 3]; 3]),
            rotations: Arc::new(vec![[1.0, 0.0, 0.0, 0.0]; 3]),
            opacity: Arc::new(vec![1.0, 0.5, 0.25]),
            sh: Arc::new(sh),
            sh_degree: 1,
            world: Mat4::identity(),
            node: String::new(),
        }
    }

    #[test]
    fn colors_bounds_and_layout() {
        let s = three();
        assert_eq!((s.len(), s.sh_stride(), s.sh_of(1).len()), (3, 12, 12));
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert!(close(s.color(0, [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]) && close(s.color(2, [1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]));
        // Degree 1 changes with the direction; degree 0 doesn't.
        let (along, against) = (s.color(1, [1.0, 0.0, 0.0]), s.color(1, [-1.0, 0.0, 0.0]));
        assert!(along[0] > 0.2 && against[0] == 0.0, "{along:?} {against:?}");
        assert_eq!(sh_color(s.sh_of(1), 0, [1.0, 0.0, 0.0]), sh_color(s.sh_of(1), 0, [-1.0, 0.0, 0.0]));
        assert_eq!(s.bounds(), Some((Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0))));
        let placed = Splats { world: geom::trs([10.0, 0.0, 0.0], [0.0; 3], [2.0; 3]), ..s.clone() };
        assert_eq!(placed.bounds(), Some((Vec3::new(8.0, -4.0, -6.0), Vec3::new(12.0, 4.0, 6.0))));
        assert!(Arc::ptr_eq(&placed.positions, &s.positions), "placing shares the data");
        assert_eq!(Splats::default().bounds(), None);
    }

    /// Written and read back: the same splats, in either PLY format, at
    /// the file's degree or capped lower; the file's channel-by-channel SH
    /// rest in our layout.
    #[test]
    fn ply_round_trips() {
        let dir = std::env::temp_dir().join(format!("arclight_splat_ply_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = three();
        let path = dir.join("three.ply");
        write_ply(&path, &s).unwrap();
        let back = read_ply(&path, 3).unwrap();
        let close = |a: &[f32], b: &[f32]| a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert_eq!((back.len(), back.sh_degree), (3, 1));
        assert!(close(back.positions.as_flattened(), s.positions.as_flattened()) && close(back.scales.as_flattened(), s.scales.as_flattened()));
        assert!(close(back.rotations.as_flattened(), s.rotations.as_flattened()) && close(&back.opacity, &s.opacity) && close(&back.sh, &s.sh));
        // Capped: degree 0, the DC terms only.
        let flat = read_ply(&path, 0).unwrap();
        assert_eq!((flat.sh_degree, flat.sh.len()), (0, 9));
        assert!(close(&flat.sh, &[&s.sh[0..3], &s.sh[12..15], &s.sh[24..27]].concat()));
        // ASCII, with the f_rest written channel by channel (R's three, then
        // G's, then B's), a face list after, and a comment.
        let ascii = dir.join("ascii.ply");
        let props = "x y z f_dc_0 f_dc_1 f_dc_2 f_rest_0 f_rest_1 f_rest_2 f_rest_3 f_rest_4 f_rest_5 f_rest_6 f_rest_7 f_rest_8 opacity scale_0 scale_1 scale_2 rot_0 rot_1 rot_2 rot_3";
        let header: String = props.split(' ').map(|p| format!("property float {p}\n")).collect();
        std::fs::write(&ascii, format!("ply\nformat ascii 1.0\ncomment made by hand\nelement vertex 1\n{header}element face 1\nproperty list uchar int vertex_indices\nend_header\n1 2 3 0.1 0.2 0.3 1 2 3 4 5 6 7 8 9 0 0 0 0 2 0 0 0\n3 0 0 0\n")).unwrap();
        let a = read_ply(&ascii, 3).unwrap();
        assert_eq!((a.len(), a.sh_degree, a.positions[0], a.opacity[0]), (1, 1, [1.0, 2.0, 3.0], 0.5));
        assert_eq!(*a.sh, [0.1, 0.2, 0.3, 1.0, 4.0, 7.0, 2.0, 5.0, 8.0, 3.0, 6.0, 9.0]);
        assert_eq!((a.scales[0], a.rotations[0]), ([1.0; 3], [1.0, 0.0, 0.0, 0.0]), "exp'd; normalized");
        // A plain point cloud isn't splats; one with splats but no f_rest is
        // degree 0, its 8-bit color the DC term.
        let cloud = dir.join("cloud.ply");
        std::fs::write(&cloud, "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n0 0 0\n").unwrap();
        assert!(format!("{:#}", read_ply(&cloud, 3).unwrap_err()).contains("not a Gaussian splat file"));
        let rgb = dir.join("rgb.ply");
        let header: String = "x y z scale_0 scale_1 scale_2 rot_0 rot_1 rot_2 rot_3".split(' ').map(|p| format!("property float {p}\n")).collect();
        std::fs::write(&rgb, format!("ply\nformat ascii 1.0\nelement vertex 1\n{header}property uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n0 0 0 0 0 0 1 0 0 0 255 0 51\n")).unwrap();
        let c = read_ply(&rgb, 3).unwrap();
        assert_eq!((c.sh_degree, c.opacity[0]), (0, 1.0));
        assert!(close(&c.color(0, [0.0, 0.0, 1.0]), &[1.0, 0.0, 0.2]));
        assert!(read_ply(&dir.join("missing.ply"), 3).is_err());
        std::fs::write(dir.join("short.ply"), &std::fs::read(&path).unwrap()[..400]).unwrap();
        assert!(format!("{:#}", read_ply(&dir.join("short.ply"), 3).unwrap_err()).contains("ends early"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The sample capture in examples/geo: a ball of flat splats colored
    /// round the hue circle, lying on its surface (each turned so its thin
    /// axis faces out), over a gray disk on the ground. Degree 1: a little
    /// brighter seen from above. The same every time.
    pub(crate) fn sample() -> Splats {
        let (ball, ground) = (256, 64);
        let n = ball + ground;
        let (mut positions, mut scales, mut rotations, mut opacity, mut sh) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // The turn taking +z to `d` (unit), as [w, x, y, z].
        let facing = |d: [f32; 3]| {
            let (w, x, y) = (1.0 + d[2], -d[1], d[0]);
            let len = (w * w + x * x + y * y).sqrt();
            if len < 1e-6 { [0.0, 1.0, 0.0, 0.0] } else { [w / len, x / len, y / len, 0.0] }
        };
        let hue = |h: f32| [0.0f32, 2.0, 4.0].map(|k| (0.5 + 0.5 * (std::f32::consts::TAU * h - k * std::f32::consts::FRAC_PI_3).cos()).clamp(0.0, 1.0));
        for i in 0..ball {
            // Spread evenly over a sphere of radius 1 at (0, 1, 0).
            let t = (i as f32 + 0.5) / ball as f32;
            let (z, a) = (1.0 - 2.0 * t, i as f32 * 2.399_963);
            let r = (1.0 - z * z).sqrt();
            let d = [r * a.cos(), z, r * a.sin()];
            positions.push([d[0], 1.0 + d[1], d[2]]);
            scales.push([0.12, 0.12, 0.01]);
            rotations.push(facing(d));
            opacity.push(0.9);
            let c = hue(a / std::f32::consts::TAU);
            sh.extend(c.map(dc_of));
            // y (coefficient 1 is −y): brighter seen from above, looking down.
            sh.extend([0.0; 3]);
            sh.extend([0.0; 3]);
            sh.extend([0.0; 3]);
            let at = sh.len() - 9;
            sh[at..at + 3].copy_from_slice(&[0.15; 3]);
        }
        for i in 0..ground {
            let (r, a) = (2.0 * ((i as f32 + 0.5) / ground as f32).sqrt(), i as f32 * 2.399_963);
            positions.push([r * a.cos(), 0.0, r * a.sin()]);
            scales.push([0.25, 0.25, 0.02]);
            rotations.push(facing([0.0, 1.0, 0.0]));
            opacity.push(0.7);
            sh.extend([0.4f32; 3].map(dc_of));
            sh.extend([0.0; 9]);
        }
        assert_eq!(positions.len(), n);
        Splats { positions: Arc::new(positions), scales: Arc::new(scales), rotations: Arc::new(rotations), opacity: Arc::new(opacity), sh: Arc::new(sh), sh_degree: 1, world: Mat4::identity(), node: String::new() }
    }

    /// The path of the sample capture.
    fn sample_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/geo/splats.ply")
    }

    /// Write examples/geo/splats.ply (run by hand: `cargo test --release
    /// --lib write_the_sample_capture -- --ignored`).
    #[test]
    #[ignore]
    fn write_the_sample_capture() {
        write_ply(&sample_path(), &sample()).unwrap();
    }

    /// The committed sample reads as written.
    #[test]
    fn the_sample_capture_reads() {
        let s = Splats::read(&sample_path(), 3).unwrap();
        let made = sample();
        assert_eq!((s.len(), s.sh_degree), (320, 1));
        assert!(s.positions.iter().zip(made.positions.iter()).all(|(a, b)| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6)));
        let (lo, hi) = s.bounds().unwrap();
        assert!(lo.y.abs() < 1e-6 && (hi.y - 2.0).abs() < 0.01 && hi.x < 2.0 && lo.x > -2.0, "{lo} {hi}");
        // Read once: the second time from the cache.
        assert!(Arc::ptr_eq(&s, &Splats::read(&sample_path(), 3).unwrap()));
        assert!(!Arc::ptr_eq(&s, &Splats::read(&sample_path(), 0).unwrap()), "another cap, another read");
    }

    /// In a scene: gathered, tagged with their node, moved between frames
    /// (for motion blur) and counted, the data shared all the while.
    #[test]
    fn splats_travel_in_scenes() {
        use crate::scene::Scene;
        let s = Arc::new(three());
        let at = |x: f64| Scene { splats: vec![Arc::new(Splats { world: geom::trs([x, 0.0, 0.0], [0.0; 3], [1.0; 3]), ..(*s).clone() })], ..Scene::default() };
        let mut a = at(0.0);
        a.tag("capture");
        let mut gathered = Scene::default();
        gathered.extend(&a);
        gathered.tag("scene1");
        assert_eq!(gathered.splats[0].node, "capture", "kept its own node");
        assert!(Arc::ptr_eq(&gathered.splats[0].positions, &s.positions));
        assert!(gathered.describe().contains("1 splat capture (3 splats)"));
        let mut b = at(4.0);
        b.tag("capture");
        let half = a.at_time(&[(0, &a), (1, &b)], 0.5);
        assert_eq!(geom::transform_point(&half.splats[0].world, &Vec3::zeros()), Vec3::new(2.0, 0.0, 0.0));
        assert!(Arc::ptr_eq(&half.splats[0].sh, &s.sh));
        let img = crate::image::Image { scene: Some(Arc::new(a)), ..crate::image::Image::blank(crate::image::Size { width: 1, height: 1 }, crate::image::IRect::default()) };
        assert!(img.bytes() >= s.bytes());
    }
}
