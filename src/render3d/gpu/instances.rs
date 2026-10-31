//! Instances drawn instanced on the GPU (docs/geo-plan.md, P4b): each
//! prototype's vertices and triangles once, a record per instance (its
//! placement, prototype, object and tint), and, made on the GPU
//! (`instances.wgsl`), each triangle seen through the camera (its corners
//! and depths: 40 bytes) and the frame's 8 × 8 bins' lists of them. The
//! raster (`raster.wgsl`) tests a pixel against its bin's triangles from
//! those, and sets up in full only the ones it keeps. No triangle table:
//! a million cards take their records (80 MB), their setups and lists.
//!
//! An instance crossing the camera's near plane is drawn as the CPU
//! renderer draws it (its own surface, its triangles clipped and set up on
//! the CPU, in the ordinary triangle table); so is one crossing a shadow
//! map's, in that map (a surface of its own drawn there only). Triangle numbers run over the instances drawn here in order,
//! each instance's prototype's triangles in order: the order the CPU
//! renderer's surfaces have them in, so ties go the same way.

use super::super::{instance_surface, instance_tint, shadow, Surface, Tri};
use crate::geo::{Instances, Prototype};
use crate::geom::{self, Camera, Mat4, Vec3};
use crate::gpu::Gpu;
use crate::image::Size;
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("instances.wgsl");
/// As in raster.wgsl and instances.wgsl.
pub(super) const INST_WORDS: usize = 20;
/// The bins instance triangles go into, BIN pixels across (as
/// instances.wgsl and raster.wgsl have it).
pub(super) const BIN: usize = 8;
const PROTO_WORDS: usize = 8;
/// The most triangle numbers a frame's instances can have (the raster
/// marks them with a bit above).
const MAX_TRIANGLES: u64 = 1 << 29;

/// A frame's instances, planned: the surfaces to draw (the ordinary ones,
/// then each prototype's, with no triangles of their own, then the
/// instances drawn on the CPU), and what goes up for the rest.
pub(super) struct Plan<'a> {
    pub surfs: Vec<Surface<'a>>,
    /// How many of `surfs` are the ordinary ones.
    pub plain: usize,
    /// The instances' nodes, in order (those with a mesh or card).
    pub nodes: Vec<String>,
    /// The prototypes' table, vertices and triangles (PROTO_WORDS each,
    /// then the rest; offsets from the table's start).
    pub protos: Vec<u32>,
    /// Each instance drawn here: its set and number in it, and its
    /// placement (the prototype's own after it).
    pub drawn: Vec<(usize, usize, Mat4)>,
    /// Their records (INST_WORDS each; objects filled in by `set_objects`).
    pub records: Vec<u32>,
    /// Their first triangle numbers.
    pub firsts: Vec<u32>,
    /// How many triangle numbers in all.
    pub triangles: u32,
    /// The number of each set's first instance across the scene's (for
    /// the id pass).
    pub ordinals: Vec<usize>,
    sets: &'a [Instances],
    /// Each instance drawn here: the box round it in the scene.
    boxes: Vec<(Vec3, Vec3)>,
    /// The shadow maps (see [`Plan::plan_shadows`]): as shadow.rs plans
    /// them, for the scene with every instance in it; each map's instances
    /// drawn there as surfaces of their own (by their number here), and
    /// those surfaces (in `surfs`, no triangles of their own: the map's
    /// are added for it alone).
    pub shadows: Option<Vec<Option<shadow::LightView>>>,
    pub map_own: Vec<Vec<u32>>,
    pub own_surface: std::collections::HashMap<u32, usize>,
    /// The instances drawn as surfaces of their own for the camera: their
    /// place in `surfs`, to their set and number there.
    cpu_of: std::collections::HashMap<usize, (usize, usize)>,
    counted: std::sync::OnceLock<(Vec<usize>, Vec<Vec<u32>>)>,
}

/// A prototype's key: its set and its number there.
type Key = (usize, usize);

impl<'a> Plan<'a> {
    /// `surfs` and the instances of `sets` seen through `cam` (`headlight`:
    /// shaded as the 3D view's Solid). None where there are too many
    /// triangles to number.
    pub fn new(surfs: &'a [Surface<'_>], sets: &'a [Instances], headlight: bool, cam: &Camera) -> Option<Plan<'a>> {
        let mut out: Vec<Surface<'a>> = surfs.iter().map(Surface::view).collect();
        let plain = out.len();
        let mut ordinals = Vec::with_capacity(sets.len());
        let mut k = 0;
        for g in sets {
            ordinals.push(k);
            k += g.len();
        }
        // Each instance: drawn here (all of it in front of the near
        // plane), on the CPU (crossing it) or not at all (behind it).
        let view = cam.view();
        let near = cam.near.max(1e-6);
        #[derive(Clone, Copy, PartialEq)]
        enum Way {
            Here,
            Cpu,
            None,
        }
        let ways: Vec<Vec<(Way, Mat4)>> = sets
            .iter()
            .map(|g| {
                let boxes: Vec<Option<[Vec3; 8]>> = g.prototypes.iter().map(|p| p.bounds().map(|(lo, hi)| crate::geo::corners(lo, hi))).collect();
                (0..g.len())
                    .into_par_iter()
                    .map(|i| {
                        let p = &g.prototypes[g.which[i] as usize];
                        let own = match p {
                            Prototype::Model(m) => m.world,
                            Prototype::Card(c) => c.world,
                            Prototype::Splats(_) => return (Way::None, Mat4::identity()),
                        };
                        let Some(corners) = boxes[g.which[i] as usize] else { return (Way::None, Mat4::identity()) };
                        let to_cam = view * g.worlds[i];
                        let depths = corners.map(|c| -geom::transform_point(&to_cam, &c).z);
                        // (Wholly nearer than the near plane, or behind the
                        // camera: drawn here all the same, for the shadow
                        // maps; the camera's view passes them by.)
                        let way = if depths.iter().all(|d| *d >= near) || depths.iter().all(|d| *d < near) { Way::Here } else { Way::Cpu };
                        (way, g.worlds[i] * own)
                    })
                    .collect()
            })
            .collect();
        // The prototypes the instances drawn here use, each once.
        let mut keys: Vec<Key> = Vec::new();
        let mut index = std::collections::HashMap::new();
        let mut protos: Vec<u32> = Vec::new();
        let mut table: Vec<[u32; PROTO_WORDS]> = Vec::new();
        let mut drawn = Vec::new();
        let mut records = Vec::new();
        let mut firsts = Vec::new();
        let mut cpu_of = std::collections::HashMap::new();
        let mut total: u64 = 0;
        let mut nodes: Vec<String> = Vec::new();
        for (si, g) in sets.iter().enumerate() {
            if ways[si].iter().any(|w| w.0 != Way::None) && !g.node.is_empty() && !nodes.contains(&g.node) {
                nodes.push(g.node.clone());
            }
            for (i, (way, world)) in ways[si].iter().enumerate() {
                match way {
                    Way::None => {}
                    Way::Cpu => {
                        if let Some(mut s) = instance_surface(g, i, (ordinals[si] + i) as u32) {
                            s.headlight = headlight;
                            cpu_of.insert(out.len(), (si, i));
                            out.push(s);
                        }
                    }
                    Way::Here => {
                        let key = (si, g.which[i] as usize);
                        let p = *index.entry(key).or_insert_with(|| {
                            keys.push(key);
                            table.push([0; PROTO_WORDS]);
                            table.len() - 1
                        });
                        let n_tris = match &g.prototypes[key.1] {
                            Prototype::Model(m) => m.mesh.triangles.len(),
                            _ => 2,
                        };
                        firsts.push(total as u32);
                        total += n_tris as u64;
                        let mult = instance_tint(g, i).unwrap_or([1.0; 4]);
                        let mut r = [0u32; INST_WORDS];
                        for row in 0..3 {
                            for c in 0..4 {
                                r[4 * row + c] = (world[(row, c)] as f32).to_bits();
                            }
                        }
                        r[12] = p as u32;
                        r[14] = *firsts.last().unwrap_or(&0);
                        for c in 0..4 {
                            r[16 + c] = mult[c].to_bits();
                        }
                        records.extend_from_slice(&r);
                        drawn.push((si, i, *world));
                    }
                }
            }
        }
        if total >= MAX_TRIANGLES {
            return None;
        }
        // Each prototype: a surface (for its color, texture and material;
        // no triangles: the instances draw them), its vertices and
        // triangles.
        let head = table.len() * PROTO_WORDS;
        let mut data: Vec<u32> = Vec::new();
        for (p, &(si, k)) in keys.iter().enumerate() {
            // (Its own color: an instance's tint goes in its record; its
            // placement in the instances'.)
            let mut s = match &sets[si].prototypes[k] {
                Prototype::Model(m) => super::super::model_surface(m),
                Prototype::Card(c) => super::super::card_surface(c),
                Prototype::Splats(_) => continue,
            };
            s.headlight = headlight;
            s.node = "";
            let has_normals = s.normals.len() == s.positions.len();
            table[p][0] = out.len() as u32;
            table[p][1] = (head + data.len()) as u32;
            for (v, pos) in s.positions.iter().enumerate() {
                let n = if has_normals { s.normals[v] } else { Vec3::zeros() };
                let uv = s.uvs.get(v).map_or([0.0, 0.0], |uv| if s.v_up { [uv[0], 1.0 - uv[1]] } else { *uv });
                let f = |x: f64| (x as f32).to_bits();
                data.extend_from_slice(&[f(pos.x), f(pos.y), f(pos.z), f(n.x), f(n.y), f(n.z), f(uv[0]), f(uv[1])]);
            }
            table[p][2] = (head + data.len()) as u32;
            table[p][3] = s.triangles.len() as u32;
            table[p][4] = has_normals as u32;
            for t in s.triangles.iter() {
                data.extend_from_slice(t);
            }
            s.triangles = Cow::Owned(Vec::new());
            out.push(s);
        }
        protos.extend(table.iter().flatten());
        protos.extend(data);
        let boxes = drawn
            .par_iter()
            .map(|&(si, i, world): &(usize, usize, Mat4)| {
                let g = &sets[si];
                let local = match &g.prototypes[g.which[i] as usize] {
                    Prototype::Model(m) => m.mesh.bounds(),
                    Prototype::Card(c) => {
                        let [w, h] = c.size.map(|v| v / 2.0);
                        Some((Vec3::new(-w, -h, 0.0), Vec3::new(w, h, 0.0)))
                    }
                    Prototype::Splats(_) => None,
                };
                let Some((lo, hi)) = local else { return (Vec3::zeros(), Vec3::zeros()) };
                let pts = crate::geo::corners(lo, hi).map(|c| geom::transform_point(&world, &c));
                pts.iter().fold((pts[0], pts[0]), |(a, b), p| (a.inf(p), b.sup(p)))
            })
            .collect();
        Some(Plan { surfs: out, plain, nodes, protos, drawn, records, firsts, triangles: total as u32, ordinals, sets, boxes, shadows: None, map_own: Vec::new(), own_surface: std::collections::HashMap::new(), cpu_of, counted: std::sync::OnceLock::new() })
    }

    /// The records with each one's object: 1 + its set's node's index in
    /// `nodes` (the frame's objects), 0 for none.
    pub fn records_with_objects(&self, nodes: &[String]) -> Vec<u32> {
        let objects: Vec<u32> = self.sets.iter().map(|g| nodes.iter().position(|n| *n == g.node).filter(|_| !g.node.is_empty()).map_or(0, |o| o as u32 + 1)).collect();
        let mut out = self.records.clone();
        for (k, d) in self.drawn.iter().enumerate() {
            out[k * INST_WORDS + 13] = objects[d.0];
        }
        out
    }

    /// Every instance (of a mesh or card) as a surface of its own, as the
    /// CPU renderer draws them.
    pub fn cpu_surfaces(&self) -> Vec<Surface<'a>> {
        super::super::instance_surfaces(self.sets)
    }

    /// Instance triangle number `g`: the CPU's triangle for it (seen
    /// through `cam` in a frame of `format`), and its id-pass number after
    /// `objects` objects.
    pub fn triangle(&self, g: u32, cam: &Camera, format: Size, objects: usize, later: Option<&Later>) -> Option<(Tri, f32)> {
        let k = self.firsts.partition_point(|&f| f <= g).checked_sub(1)?;
        let (si, i, _) = self.drawn[k];
        let set = &self.sets[si];
        let ordinal = self.ordinals[si] + i;
        let mut s = instance_surface(set, i, ordinal as u32)?;
        let next = later.map(|l| vec![l.positions(&s, self.cpu_index(si, i))]);
        let t = (g - self.firsts[k]) as usize;
        let one = *s.triangles.get(t)?;
        s.triangles = Cow::Owned(vec![one]);
        let tri = super::super::triangles(&[s], cam, format, next.as_deref()).into_iter().next()?;
        Some((tri, (objects + 1 + ordinal) as f32))
    }

    /// Where instance `i` of set `si` is in the CPU renderer's surfaces
    /// (the ordinary ones, then each instance of a mesh or card).
    fn cpu_index(&self, si: usize, i: usize) -> usize {
        // (Each set's instances of a mesh or card before each one, and
        // before the set: counted once.)
        let (bases, before) = self.counted.get_or_init(|| {
            let before: Vec<Vec<u32>> = self
                .sets
                .iter()
                .map(|g| {
                    let mut out = Vec::with_capacity(g.len() + 1);
                    let mut k = 0u32;
                    for j in 0..g.len() {
                        out.push(k);
                        k += u32::from(!matches!(g.prototypes[g.which[j] as usize], Prototype::Splats(_)));
                    }
                    out.push(k);
                    out
                })
                .collect();
            let mut base = 0usize;
            let bases = before
                .iter()
                .map(|b| {
                    let at = base;
                    base += *b.last().unwrap_or(&0) as usize;
                    at
                })
                .collect();
            (bases, before)
        });
        self.plain + bases[si] + before[si][i] as usize
    }

    /// Each of `surfs`' vertices a frame later (for the motion pass), as
    /// the CPU's `next_positions` finds them for its own surfaces.
    pub fn next_positions(&self, later: &Later) -> Vec<Vec<Vec3>> {
        self.surfs
            .iter()
            .enumerate()
            .map(|(j, s)| {
                if s.triangles.is_empty() {
                    return Vec::new();
                }
                let at = if j < self.plain { Some(j) } else { self.cpu_of.get(&j).map(|&(si, i)| self.cpu_index(si, i)) };
                match at {
                    Some(at) => later.positions(s, at),
                    None => s.positions.iter().map(|p| geom::transform_point(&s.world, p)).collect(),
                }
            })
            .collect()
    }

    /// Plan the shadow maps of `lights` (as shadow.rs does for the scene
    /// with every instance a surface of its own), and which instances each
    /// map draws as surfaces of their own: those reaching in front of its
    /// near plane (their triangles clipped on the CPU there).
    pub fn plan_shadows(&mut self, lights: &[crate::shade::Light]) {
        let sets = self.sets;
        let plain = &self.surfs[..self.plain];
        let extent = || {
            // Every point of the ordinary surfaces and of each instance (a
            // mesh's or card's) placed: what the CPU's extent takes.
            let mut pts: Vec<Vec3> = plain.iter().flat_map(|s| s.positions.iter().map(|p| geom::transform_point(&s.world, p))).collect();
            let more: Vec<Vec<Vec3>> = sets
                .iter()
                .map(|g| {
                    (0..g.len())
                        .into_par_iter()
                        .flat_map_iter(|i| {
                            let (local, own): (Vec<Vec3>, Mat4) = match &g.prototypes[g.which[i] as usize] {
                                Prototype::Model(m) => (m.mesh.positions.clone(), m.world),
                                Prototype::Card(c) => (super::super::card_surface(c).positions.into_owned(), c.world),
                                Prototype::Splats(_) => (Vec::new(), Mat4::identity()),
                            };
                            let w = g.worlds[i] * own;
                            local.into_iter().map(move |p| geom::transform_point(&w, &p))
                        })
                        .collect()
                })
                .collect();
            pts.extend(more.into_iter().flatten());
            shadow::extent_of(&pts)
        };
        let planned = shadow::plan_in(&self.surfs, lights, extent);
        let mut own: Vec<Vec<u32>> = Vec::new();
        for (_, v) in super::map_views(&planned) {
            let view = v.cam.view();
            let near = v.cam.near.max(1e-6);
            let row: Vec3 = Vec3::new(view[(2, 0)], view[(2, 1)], view[(2, 2)]);
            let reach = row.abs();
            let crossing: Vec<u32> = (0..self.drawn.len() as u32)
                .into_par_iter()
                .filter(|&k| {
                    let (lo, hi) = self.boxes[k as usize];
                    let (c, half) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
                    let depth = -(row.dot(&c) + view[(2, 3)]);
                    let spread = reach.dot(&half);
                    depth - spread < near && depth + spread >= near
                })
                .collect();
            for &k in &crossing {
                if !self.own_surface.contains_key(&k) {
                    let (si, i, _) = self.drawn[k as usize];
                    if let Some(mut s) = instance_surface(&sets[si], i, (self.ordinals[si] + i) as u32) {
                        s.triangles = Cow::Owned(Vec::new());
                        self.own_surface.insert(k, self.surfs.len());
                        self.surfs.push(s);
                    }
                }
            }
            own.push(crossing);
        }
        self.map_own = own;
        self.shadows = Some(planned);
    }

    /// Instance `k`'s surface with its own triangles (for a map drawing it
    /// so), and where it is in `surfs`.
    pub fn own_full(&self, k: u32) -> Option<(Surface<'a>, usize)> {
        let at = *self.own_surface.get(&k)?;
        let (si, i, _) = self.drawn[k as usize];
        Some((instance_surface(&self.sets[si], i, (self.ordinals[si] + i) as u32)?, at))
    }

    /// Whether prototype p's instances let light through anywhere (it does,
    /// or an instance's tint makes it less than opaque).
    pub fn sees_through(&self, proto_surface: usize) -> bool {
        (0..self.drawn.len()).any(|k| {
            let r = &self.records[k * INST_WORDS..(k + 1) * INST_WORDS];
            let a = f32::from_bits(r[19]);
            self.protos[r[12] as usize * PROTO_WORDS] as usize == proto_surface && a > 0.0 && a < 1.0
        })
    }

    pub fn is_empty(&self) -> bool {
        self.drawn.is_empty()
    }
}

/// The scene a frame later, as the CPU renderer lists its surfaces (the
/// ordinary ones, then each instance of a mesh or card), for the motion
/// pass.
pub(super) struct Later<'s> {
    plain: Vec<Surface<'s>>,
    sets: &'s [Instances],
    /// Each instance of a mesh or card in order: its set and number.
    drawable: Vec<(usize, usize)>,
}

impl<'s> Later<'s> {
    pub fn new(next: &'s crate::scene::Scene) -> Later<'s> {
        let drawable = next.instances.iter().enumerate().flat_map(|(si, g)| (0..g.len()).filter(move |&i| !matches!(g.prototypes[g.which[i] as usize], Prototype::Splats(_))).map(move |i| (si, i))).collect();
        Later { plain: super::super::plain_surfaces(next), sets: &next.instances, drawable }
    }

    /// Surface `s`'s vertices a frame later: those of the surface at its
    /// place `at` in the later list, if from the same node (its own
    /// vertices where it has as many), else where they are now.
    fn positions(&self, s: &Surface, at: usize) -> Vec<Vec3> {
        let own;
        let l = if at < self.plain.len() {
            Some(&self.plain[at])
        } else {
            own = self.drawable.get(at - self.plain.len()).and_then(|&(si, i)| instance_surface(&self.sets[si], i, 0));
            own.as_ref()
        };
        let (world, positions) = match l.filter(|l| l.node == s.node) {
            Some(l) if l.positions.len() == s.positions.len() => (l.world, &l.positions),
            Some(l) => (l.world, &s.positions),
            None => (s.world, &s.positions),
        };
        positions.iter().map(|p| geom::transform_point(&world, p)).collect()
    }
}

/// The bin lists `plan`'s instances make through `cam` in a frame of
/// `format` (their records in `records`): the bins' starts (bins + 1
/// words), the entries (triangle number, its pixels, its nearest depth:
/// four words; each bin's in no particular order), then each triangle set
/// up (10 words, its instance last); and how many entries. Errors where they won't fit a
/// binding.
pub(super) fn bin(gpu: &Gpu, plan: &Plan, cam: &Camera, format: Size, records: &wgpu::Buffer, skip: &[u32]) -> Result<(wgpu::Buffer, usize)> {
    let device = &gpu.device;
    let limit = gpu.limits.max_storage_buffer_binding_size as usize;
    let (tx, ty) = (format.width.div_ceil(BIN), format.height.div_ceil(BIN));
    let tiles = tx * ty;
    let storage = |size: usize, label: &str| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(16) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let protos = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("instance prototypes"), contents: bytemuck::cast_slice(if plan.protos.is_empty() { &[0u32][..] } else { &plan.protos }), usage: wgpu::BufferUsages::STORAGE });
    let counts = storage(tiles * 4, "instance tile counts");
    let starts = storage((tiles + 1) * 4, "instance tile starts");
    let cursors = storage(tiles * 4, "instance tile cursors");
    let mut p = [0u32; 32];
    p[..4].copy_from_slice(&[format.width as u32, format.height as u32, tx as u32, ty as u32]);
    p[4..8].copy_from_slice(&[plan.drawn.len() as u32, plan.triangles, tiles as u32, cam.is_ortho() as u32]);
    p[8..24].copy_from_slice(&lens_words(cam, format));
    p[24] = !skip.is_empty() as u32;
    let skips = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("instances left out"), contents: bytemuck::cast_slice(if skip.is_empty() { &[0u32][..] } else { skip }), usage: wgpu::BufferUsages::STORAGE });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("instance params"), contents: bytemuck::cast_slice(&p), usage: wgpu::BufferUsages::UNIFORM });
    let none = storage(16, "instance entries (none yet)");
    let setup_bytes = plan.triangles as usize * 40;
    let setups = storage(setup_bytes, "instance triangles set up");
    let pass = |encoder: &mut wgpu::CommandEncoder, entry: &'static str, pairs: &wgpu::Buffer, size: (u32, u32)| {
        let pipeline = gpu.compute_pipeline("render3d/instances", SHADER, entry);
        let all: [&wgpu::Buffer; 9] = [&uniform, &protos, records, &counts, &starts, &cursors, pairs, &setups, &skips];
        // (Each entry's layout has only the bindings it uses.)
        let used: &[u32] = match entry {
            "count" => &[0, 1, 2, 3, 7, 8],
            "offsets" => &[0, 3, 4, 5],
            _ => &[0, 4, 5, 6, 7],
        };
        let entries: Vec<wgpu::BindGroupEntry> = used.iter().map(|&i| wgpu::BindGroupEntry { binding: i, resource: all[i as usize].as_entire_binding() }).collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("instances"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(size.0, size.1, 1);
    };
    let groups = |n: usize| {
        let g = n.div_ceil(256).max(1);
        ((g.min(65535)) as u32, g.div_ceil(65535) as u32)
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.clear_buffer(&counts, 0, None);
    pass(&mut encoder, "count", &none, groups(plan.triangles as usize));
    pass(&mut encoder, "offsets", &none, (1, 1));
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("instance pairs count"), size: 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    encoder.copy_buffer_to_buffer(&starts, (tiles * 4) as u64, &staging, 0, 4);
    gpu.queue.submit([encoder.finish()]);
    let slice = staging.slice(..);
    let (tx_, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx_.send(r);
    });
    gpu.wait()?;
    rx.recv().map_err(|_| anyhow!("instances: no answer"))?.map_err(|e| anyhow!("instances: {e}"))?;
    let n = {
        let bytes = slice.get_mapped_range().map_err(|e| anyhow!("instances: {e:?}"))?;
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize
    };
    staging.unmap();
    if (tiles + 1) * 4 + n * 16 + setup_bytes > limit {
        return Err(anyhow!("instances: {n} bin entries and {} triangles are more than a binding holds", plan.triangles));
    }
    let pairs = storage(n * 16, "instance entries");
    let mut encoder = device.create_command_encoder(&Default::default());
    // (Unsorted: a bin's order doesn't change what the raster keeps, see
    // `add_hit` there.)
    if n > 0 {
        pass(&mut encoder, "scatter", &pairs, groups(plan.triangles as usize));
    }
    let lists = storage((tiles + 1) * 4 + n * 16 + setup_bytes, "instance lists");
    encoder.copy_buffer_to_buffer(&starts, 0, &lists, 0, ((tiles + 1) * 4) as u64);
    if n > 0 {
        encoder.copy_buffer_to_buffer(&pairs, 0, &lists, ((tiles + 1) * 4) as u64, (n * 16) as u64);
    }
    if setup_bytes > 0 {
        encoder.copy_buffer_to_buffer(&setups, 0, &lists, ((tiles + 1) * 4 + n * 16) as u64, setup_bytes as u64);
    }
    gpu.queue.submit([encoder.finish()]);
    Ok((lists, n))
}

/// The camera's view (three rows) and lens (focal length in pixels or an
/// orthographic camera's pixels a unit, the frame's middle), as f32 bits
/// (raster.wgsl's `view0`..`lens`).
pub(super) fn lens_words(cam: &Camera, format: Size) -> [u32; 16] {
    let view = cam.view();
    let f = |v: f64| (v as f32).to_bits();
    let mut out = [0u32; 16];
    for r in 0..3 {
        for c in 0..4 {
            out[4 * r + c] = f(view[(r, c)]);
        }
    }
    let k = if cam.is_ortho() { cam.pixels_per_unit(1.0, format) } else { cam.focal_pixels(format) };
    out[12..16].copy_from_slice(&[f(k), f(format.width as f64 / 2.0), f(format.height as f64 / 2.0), 0]);
    out
}
