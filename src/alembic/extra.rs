//! Alembic curves, points, cameras and face sets (the parent module reads
//! transforms and meshes; this reads the rest of what a Read Geo asks for).
//!
//! - Curves (`AbcGeom_Curve_v2`): `P`, `nVertices` and
//!   `curveBasisAndType` (type, wrap, basis, step). Linear curves keep
//!   their points; cubic ones are evaluated into polylines, [`STEPS`] points
//!   a span: Bezier (step 3), B-spline and Catmull-Rom (step 1). Variable
//!   order curves (NURBS) use `.orders`, `.knots` and `w` by de Boor;
//!   Hermite and power bases are read as linear. Periodic curves are
//!   closed. `width`, when it has a value per output point (linear
//!   curves), becomes the "width" point attribute.
//! - Points (`AbcGeom_Points_v1`): `P`; `.pointIds` → "id", `.velocities`
//!   → "v", `.widths` → "width", and the `.arbGeomParams` the attribute
//!   model holds (float, int, 3 and 4 wide, one value per point).
//! - Cameras (`AbcGeom_Camera_v1`): `.core`'s focal length (mm),
//!   apertures (cm, so × 10 for the film back) and clipping planes, placed
//!   by the transforms above them; film offsets, squeeze and overscan
//!   aren't used. They look down −Z, as ours do.
//! - Face sets (`AbcGeom_FaceSet_v1`, a mesh's children): `.faces`, the
//!   mesh's polygons in the set, as a per-triangle Int attribute
//!   "faceset.<name>" (1 in it); or the mesh split into a mesh per set
//!   ("<mesh>/<set>", its material named after the set) and one of the
//!   triangles in none (keeping the mesh's name).
//!
//! Objects whose `visible` is 0 are left out with what's under them, as
//! meshes are.

use super::*;
use crate::geo::{Attr, Attrs, Curves, PointSet};
use crate::geom::Camera;

/// Points a cubic span is evaluated into.
pub const STEPS: usize = 8;

/// What to read besides meshes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Want {
    pub curves: bool,
    pub points: bool,
    pub cameras: bool,
}

/// An archive's curves, points and cameras at a time.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub curves: Vec<Curves>,
    pub points: Vec<PointSet>,
    /// Each camera with its object's name.
    pub cameras: Vec<(String, Camera)>,
}

/// The cubic spans' weights at `u` (0–1) for four control points.
fn bezier(u: f64) -> [f64; 4] {
    let v = 1.0 - u;
    [v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u]
}

fn bspline(u: f64) -> [f64; 4] {
    let (u2, u3) = (u * u, u * u * u);
    [(1.0 - 3.0 * u + 3.0 * u2 - u3) / 6.0, (4.0 - 6.0 * u2 + 3.0 * u3) / 6.0, (1.0 + 3.0 * u + 3.0 * u2 - 3.0 * u3) / 6.0, u3 / 6.0]
}

fn catmull_rom(u: f64) -> [f64; 4] {
    let (u2, u3) = (u * u, u * u * u);
    [(-u3 + 2.0 * u2 - u) / 2.0, (3.0 * u3 - 5.0 * u2 + 2.0) / 2.0, (-3.0 * u3 + 4.0 * u2 + u) / 2.0, (u3 - u2) / 2.0]
}

/// A cubic curve's control points `p` (basis 1 Bezier, 2 B-spline, 3
/// Catmull-Rom) as a polyline; `periodic` wraps the last span round.
pub fn cubic(p: &[Vec3], basis: u8, periodic: bool) -> Vec<Vec3> {
    let n = p.len();
    let (weights, step): (fn(f64) -> [f64; 4], usize) = match basis {
        1 => (bezier, 3),
        2 => (bspline, 1),
        3 => (catmull_rom, 1),
        _ => return p.to_vec(),
    };
    if n < 4 {
        return p.to_vec();
    }
    let at = |i: usize| p[i % n];
    // Spans: starts every `step`, as many as fit (all round when periodic).
    let spans: Vec<usize> = if periodic { (0..n).step_by(step).collect() } else { (0..=n - 4).step_by(step).collect() };
    let mut out = Vec::new();
    for (k, &s) in spans.iter().enumerate() {
        let first = if k == 0 { 0 } else { 1 };
        let last = if periodic && k + 1 == spans.len() { STEPS - 1 } else { STEPS };
        for j in first..=last {
            let w = weights(j as f64 / STEPS as f64);
            out.push((0..4).fold(Vec3::zeros(), |acc, c| acc + at(s + c) * w[c]));
        }
    }
    out
}

/// A NURBS curve of `order` (control points `p`, `weights` or all 1,
/// `knots`: points + order of them) as a polyline, [`STEPS`] points a span,
/// by de Boor.
pub fn nurbs(p: &[Vec3], weights: &[f64], order: usize, knots: &[f64]) -> Vec<Vec3> {
    let n = p.len();
    if order < 2 || n < order || knots.len() < n + order {
        return p.to_vec();
    }
    let k = order - 1;
    let w = |i: usize| weights.get(i).copied().filter(|w| *w > 0.0).unwrap_or(1.0);
    let (lo, hi) = (knots[k], knots[n]);
    let samples = STEPS * (n - k);
    (0..=samples)
        .map(|j| {
            let u = lo + (hi - lo) * j as f64 / samples as f64;
            // The span holding u (the last one at its end).
            let mut span = k;
            while span < n - 1 && knots[span + 1] <= u {
                span += 1;
            }
            let mut d: Vec<(Vec3, f64)> = (0..=k).map(|r| (p[span - k + r] * w(span - k + r), w(span - k + r))).collect();
            for r in 1..=k {
                for jj in (r..=k).rev() {
                    let i = span - k + jj;
                    let den = knots[i + k + 1 - r] - knots[i];
                    let a = if den.abs() < 1e-12 { 0.0 } else { (u - knots[i]) / den };
                    d[jj] = (d[jj - 1].0 * (1.0 - a) + d[jj].0 * a, d[jj - 1].1 * (1.0 - a) + d[jj].1 * a);
                }
            }
            d[k].0 / d[k].1
        })
        .collect()
}

fn vec3s(v: &[f64]) -> Vec<Vec3> {
    v.as_chunks::<3>().0.iter().map(|c| Vec3::new(c[0], c[1], c[2])).collect()
}

impl Archive {
    /// An array property of `g` at `t` (None: not there or not an array).
    fn array_at(&self, g: &[Prop], name: &str, t: f64) -> Result<Option<Vec<f64>>> {
        match find(g, name).map(|p| &p.kind) {
            Some(Kind::Array(s)) => self.values(s, true, t).map(Some),
            _ => Ok(None),
        }
    }

    /// The curves, points and cameras `want` asks for, at `t` (seconds).
    pub fn extras(&self, t: f64, want: Want) -> Result<Extras> {
        let mut out = Extras::default();
        if want != Want::default() {
            for c in &self.root.children {
                self.gather(c, &Mat4::identity(), t, want, &mut out).with_context(|| format!("in {}", c.path))?;
            }
        }
        Ok(out)
    }

    /// The names of the archive's cameras, in file order (as
    /// [`Extras::cameras`] has them, visible or not).
    pub fn camera_names(&self) -> Vec<String> {
        fn walk(o: &Object, out: &mut Vec<String>) {
            if o.schema() == "AbcGeom_Camera_v1" {
                out.push(o.name.clone());
            }
            o.children.iter().for_each(|c| walk(c, out));
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    fn gather(&self, o: &Object, parent: &Mat4, t: f64, want: Want, out: &mut Extras) -> Result<()> {
        if let Some(Kind::Scalar(s)) = find(&o.props, "visible").map(|p| &p.kind) {
            if self.values(s, false, t)?.first() == Some(&0.0) {
                return Ok(());
            }
        }
        let mut world = *parent;
        let geom = match find(&o.props, ".geom").map(|p| &p.kind) {
            Some(Kind::Compound(g)) => Some(g.as_slice()),
            _ => None,
        };
        match (o.schema(), geom) {
            ("AbcGeom_Xform_v3", _) => {
                if let Some(Kind::Compound(x)) = find(&o.props, ".xform").map(|p| &p.kind) {
                    let (local, inherits) = self.xform(x, t)?;
                    world = if inherits { parent * local } else { local };
                }
            }
            ("AbcGeom_Curve_v2", Some(g)) if want.curves => {
                let mut c = self.curves(g, t)?;
                c.world = world;
                if !c.positions.is_empty() {
                    out.curves.push(c);
                }
            }
            ("AbcGeom_Points_v1", Some(g)) if want.points => {
                let mut p = self.points(g, t)?;
                p.world = world;
                if !p.positions.is_empty() {
                    out.points.push(p);
                }
            }
            ("AbcGeom_Camera_v1", Some(g)) if want.cameras => {
                if let Some(Kind::Scalar(s)) = find(g, ".core").map(|p| &p.kind) {
                    let core = self.values(s, false, t)?;
                    let at = |i: usize, d: f64| core.get(i).copied().filter(|v| v.is_finite()).unwrap_or(d);
                    let camera = Camera { focal: at(0, 35.0), film_back: [at(1, 3.6) * 10.0, at(3, 2.4) * 10.0], world, near: at(14, 0.1), far: at(15, 10000.0), ortho: 0.0 };
                    out.cameras.push((o.name.clone(), camera));
                }
            }
            _ => {}
        }
        for c in &o.children {
            self.gather(c, &world, t, want, out).with_context(|| format!("in {}", c.path))?;
        }
        Ok(())
    }

    /// A curve schema's curves at `t`, in its object's space.
    fn curves(&self, g: &[Prop], t: f64) -> Result<Curves> {
        let p = vec3s(&self.array_at(g, "P", t)?.ok_or_else(|| anyhow!("curves without P"))?);
        let counts: Vec<usize> = self.array_at(g, "nVertices", t)?.unwrap_or_default().iter().map(|&c| c.max(0.0) as usize).collect();
        if counts.iter().sum::<usize>() != p.len() {
            bail!("curve vertex counts add up to {}, not the {} points", counts.iter().sum::<usize>(), p.len());
        }
        let kind = match find(g, "curveBasisAndType").map(|p| &p.kind) {
            Some(Kind::Scalar(s)) => self.values(s, false, t)?,
            _ => vec![1.0, 0.0, 0.0, 0.0],
        };
        let (ty, periodic, basis) = (kind.first().copied().unwrap_or(1.0) as u8, kind.get(1).copied() == Some(1.0), kind.get(2).copied().unwrap_or(0.0) as u8);
        let orders = self.array_at(g, ".orders", t)?.unwrap_or_default();
        let knots = self.array_at(g, ".knots", t)?.unwrap_or_default();
        let weights = self.array_at(g, "w", t)?.unwrap_or_default();
        let mut out = Curves::default();
        let (mut start, mut knot) = (0, 0);
        for (i, &n) in counts.iter().enumerate() {
            let cp = &p[start..start + n];
            let pts = match ty {
                0 => cubic(cp, basis, periodic),
                2 => {
                    let order = orders.get(i).copied().unwrap_or(4.0) as usize;
                    let ks = knots.get(knot..knot + n + order).unwrap_or(&[]);
                    knot += n + order;
                    nurbs(cp, weights.get(start..start + n).unwrap_or(&[]), order, ks)
                }
                _ => cp.to_vec(),
            };
            out.push(&pts, periodic);
            start += n;
        }
        // Widths only where they match the points (linear curves).
        if let Some(w) = self.array_at(g, "width", t)? {
            if w.len() == out.positions.len() && !out.positions.is_empty() {
                out.attrs.set("width", Attr::Float(w.iter().map(|&v| v as f32).collect()), out.positions.len())?;
            }
        }
        Ok(out)
    }

    /// A points schema's points at `t`, in its object's space.
    fn points(&self, g: &[Prop], t: f64) -> Result<PointSet> {
        let positions = vec3s(&self.array_at(g, "P", t)?.ok_or_else(|| anyhow!("points without P"))?);
        let n = positions.len();
        let mut attrs = Attrs::default();
        if let Some(ids) = self.array_at(g, ".pointIds", t)?.filter(|v| v.len() == n) {
            attrs.set("id", Attr::Int(ids.iter().map(|&v| v as i32).collect()), n)?;
        }
        if let Some(v) = self.array_at(g, ".velocities", t)?.filter(|v| v.len() == 3 * n) {
            attrs.set("v", Attr::Vec3(v.as_chunks::<3>().0.iter().map(|c| c.map(|x| x as f32)).collect()), n)?;
        }
        if let Some(w) = self.array_at(g, ".widths", t)?.filter(|v| v.len() == n) {
            attrs.set("width", Attr::Float(w.iter().map(|&v| v as f32).collect()), n)?;
        }
        if let Some(Kind::Compound(arb)) = find(g, ".arbGeomParams").map(|p| &p.kind) {
            for p in arb {
                let Kind::Array(s) = &p.kind else { continue };
                let v = self.values(s, true, t)?;
                let extent = meta_value(&p.meta, "podExtent").or(meta_value(&p.meta, "arrayExtent")).and_then(|e| e.parse::<usize>().ok()).unwrap_or(1);
                let int = matches!(meta_value(&p.meta, "podName"), Some(name) if name.contains("int"));
                if v.len() != extent * n || attrs.get(&p.name).is_some() {
                    continue;
                }
                let attr = match (extent, int) {
                    (1, true) => Attr::Int(v.iter().map(|&x| x as i32).collect()),
                    (1, false) => Attr::Float(v.iter().map(|&x| x as f32).collect()),
                    (3, _) => Attr::Vec3(v.as_chunks::<3>().0.iter().map(|c| c.map(|x| x as f32)).collect()),
                    (4, _) => Attr::Vec4(v.as_chunks::<4>().0.iter().map(|c| c.map(|x| x as f32)).collect()),
                    _ => continue,
                };
                attrs.set(&p.name, attr, n)?;
            }
        }
        Ok(PointSet { positions, attrs, ..PointSet::default() })
    }

    /// The object at `path` ("/box/Cube").
    fn object_at(&self, path: &str) -> Option<&Object> {
        fn walk<'a>(o: &'a Object, path: &str) -> Option<&'a Object> {
            if o.path == path {
                return Some(o);
            }
            o.children.iter().find_map(|c| walk(c, path))
        }
        walk(&self.root, path)
    }

    /// `parts` (from [`Archive::parts`] at `t`) with their meshes' face
    /// sets: as "faceset.<name>" triangle attributes, or with `split` as a
    /// part per set and one for the rest.
    pub fn with_face_sets(&self, parts: Vec<Part>, t: f64, split: bool) -> Result<Vec<Part>> {
        let mut out = Vec::new();
        for part in parts {
            let sets = self.face_sets(&part, t)?;
            if sets.is_empty() {
                out.push(part);
                continue;
            }
            if !split {
                let mut part = part;
                let n = part.mesh.triangles.len();
                for (name, inside) in &sets {
                    part.mesh.face_attrs.set(&format!("faceset.{name}"), Attr::Int(inside.iter().map(|&b| b as i32).collect()), n)?;
                }
                out.push(part);
                continue;
            }
            let mut taken = vec![false; part.mesh.triangles.len()];
            for (name, inside) in &sets {
                let tris: Vec<usize> = (0..inside.len()).filter(|&i| inside[i] && !taken[i]).collect();
                tris.iter().for_each(|&i| taken[i] = true);
                if !tris.is_empty() {
                    let material = Material { name: name.clone(), ..part.material.clone() };
                    out.push(Part { name: format!("{}/{name}", part.name), mesh: sub_mesh(&part.mesh, &tris), material });
                }
            }
            let rest: Vec<usize> = (0..taken.len()).filter(|&i| !taken[i]).collect();
            if !rest.is_empty() {
                out.push(Part { name: part.name.clone(), mesh: sub_mesh(&part.mesh, &rest), material: part.material.clone() });
            }
        }
        Ok(out)
    }

    /// The face sets of `part`'s mesh object at `t`: each set's name and,
    /// per triangle of the part's mesh, whether it's in it. Empty when the
    /// object has none or its polygons don't match the mesh's triangles.
    fn face_sets(&self, part: &Part, t: f64) -> Result<Vec<(String, Vec<bool>)>> {
        let Some(o) = self.object_at(&format!("/{}", part.name)) else { return Ok(Vec::new()) };
        let sets: Vec<&Object> = o.children.iter().filter(|c| c.schema() == "AbcGeom_FaceSet_v1").collect();
        let Some(Kind::Compound(g)) = find(&o.props, ".geom").map(|p| &p.kind) else { return Ok(Vec::new()) };
        if sets.is_empty() {
            return Ok(Vec::new());
        }
        // Each polygon's first triangle (fanned: n − 2 each).
        let counts = self.array_at(g, ".faceCounts", t)?.unwrap_or_default();
        let mut first = Vec::with_capacity(counts.len() + 1);
        let mut total = 0;
        for &c in &counts {
            first.push(total);
            total += (c.max(0.0) as usize).saturating_sub(2);
        }
        first.push(total);
        if total != part.mesh.triangles.len() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for s in sets {
            let Some(Kind::Compound(f)) = find(&s.props, ".faceset").map(|p| &p.kind) else { continue };
            let mut inside = vec![false; total];
            for face in self.array_at(f, ".faces", t)?.unwrap_or_default() {
                let face = face as usize;
                if face + 1 < first.len() {
                    inside[first[face]..first[face + 1]].iter_mut().for_each(|b| *b = true);
                }
            }
            out.push((s.name.clone(), inside));
        }
        Ok(out)
    }
}

/// The triangles `tris` of `mesh` as a mesh of their own (only the
/// vertices they use, attributes along).
fn sub_mesh(mesh: &Mesh, tris: &[usize]) -> Mesh {
    let mut map = vec![u32::MAX; mesh.positions.len()];
    let mut used: Vec<u32> = Vec::new();
    let triangles = tris
        .iter()
        .map(|&t| {
            mesh.triangles[t].map(|v| {
                if map[v as usize] == u32::MAX {
                    map[v as usize] = used.len() as u32;
                    used.push(v);
                }
                map[v as usize]
            })
        })
        .collect();
    let pick = |list: &[Vec3]| if list.len() == mesh.positions.len() { used.iter().map(|&v| list[v as usize]).collect() } else { Vec::new() };
    Mesh {
        positions: pick(&mesh.positions),
        normals: pick(&mesh.normals),
        uvs: if mesh.uvs.len() == mesh.positions.len() { used.iter().map(|&v| mesh.uvs[v as usize]).collect() } else { Vec::new() },
        triangles,
        point_attrs: mesh.point_attrs.gathered(&used),
        face_attrs: mesh.face_attrs.gathered(&tris.iter().map(|&t| t as u32).collect::<Vec<_>>()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras_file() -> Archive {
        Archive::open(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geo/extras.abc")).unwrap()
    }

    const ALL: Want = Want { curves: true, points: true, cameras: true };

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).norm() < 1e-5
    }

    /// examples/geo/extras.abc, from Blender 5.2 (Z up there, Y up in the
    /// file), 24 fps, frames 1–10: a Bezier curve "wave" (0,0,0) → (2,0,0)
    /// with handles up and down; a closed square poly curve "loop" at
    /// z = 3; a 2-unit cube "box" at x = 5 with materials Sides and Top
    /// (two face sets); a grid "emitter" at y = −4 emitting 5 particles
    /// at frame 1 (exported as points, with ids, velocities and widths); a
    /// camera "shot" moving from (0, −10, 1) to (4, −10, 1) looking along
    /// +Y, focal keyed 35 → 50, clipping 0.5–200; a still camera "side" at
    /// (10, 0, 1), focal 24. Made with
    /// `blender -b --factory-startup --python make.py -- out`, make.py:
    ///
    /// ```text
    /// (bpy, factory settings empty; the curves built from splines, FREE
    /// handles; the cube's top polygon given material index 1; the grid's
    /// particle system 5 particles at frame 1, no gravity; the cameras
    /// keyed at frames 1 and 10; then wm.alembic_export(start=1, end=10,
    /// export_particles=True, face_sets=True, curves_as_mesh=False,
    /// uvs=False, normals=False))
    /// ```
    #[test]
    fn the_extras_file_reads_curves_points_cameras() {
        let a = extras_file();
        let e = a.extras(1.0 / 24.0, ALL).unwrap();
        // Curves: the Bezier evaluated (one span, STEPS + 1 points), the
        // square closed with its 4 corners (Blender's Y up: z → −y).
        assert_eq!(e.curves.len(), 2);
        let wave = &e.curves[0];
        assert_eq!((wave.positions.len(), wave.closed.clone()), (STEPS + 1, vec![false]));
        assert!(near(wave.positions[0], Vec3::zeros()) && near(wave.positions[STEPS], Vec3::new(2.0, 0.0, 0.0)));
        // Halfway along: (1, 0.75, 0) (control points 0,0 / .5,1 / 1.5,1 / 2,0).
        assert!(near(wave.positions[STEPS / 2], Vec3::new(1.0, 0.75, 0.0)), "{}", wave.positions[STEPS / 2]);
        let lp = &e.curves[1];
        assert_eq!((lp.positions.len(), lp.closed.clone()), (4, vec![true]));
        assert!(near(lp.positions[2], Vec3::new(1.0, 3.0, -1.0)));
        assert_eq!(lp.attrs.float("width").map(<[f32]>::len), Some(4));
        // Points: 5, with ids 0–4, velocities and widths, under the
        // emitter at y = −4 (file z = 4).
        assert_eq!(e.points.len(), 1);
        let p = &e.points[0];
        assert_eq!(p.positions.len(), 5);
        assert_eq!(p.attrs.int("id"), Some(&[0, 1, 2, 3, 4][..]));
        assert!(p.attrs.vec3("v").is_some() && p.attrs.float("width").is_some());
        assert!((p.placed(0).z - 4.0 - p.positions[0].z).abs() < 1e-9);
        // Cameras: shot and side, in file order.
        assert_eq!(e.cameras.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["shot", "side"]);
        assert_eq!(a.camera_names(), ["shot", "side"]);
        let shot = &e.cameras[0].1;
        assert!((shot.focal - 35.0).abs() < 1e-6 && (shot.film_back[0] - 36.0).abs() < 1e-6 && (shot.near - 0.5).abs() < 1e-6 && (shot.far - 200.0).abs() < 1e-6);
        let eye = |c: &Camera| crate::geom::transform_point(&c.world, &Vec3::zeros());
        let look = |c: &Camera| c.world.fixed_view::<3, 3>(0, 0) * -Vec3::z();
        assert!(near(eye(shot), Vec3::new(0.0, 1.0, 10.0)), "{}", eye(shot));
        assert!(look(shot).z < -0.99, "looks toward the origin (Blender +Y: file −Z): {}", look(shot));
        // Frame 10: moved 4 along x, focal 50.
        let end = a.extras(10.0 / 24.0, ALL).unwrap();
        let shot = &end.cameras[0].1;
        assert!(near(eye(shot), Vec3::new(4.0, 1.0, 10.0)) && (shot.focal - 50.0).abs() < 1e-6, "{} {}", eye(shot), shot.focal);
        assert!((end.cameras[1].1.focal - 24.0).abs() < 1e-6);
        // Nothing asked: nothing read.
        let none = a.extras(1.0 / 24.0, Want::default()).unwrap();
        assert!(none.curves.is_empty() && none.points.is_empty() && none.cameras.is_empty());
    }

    #[test]
    fn face_sets_become_attributes_or_parts() {
        let a = extras_file();
        let t = 1.0 / 24.0;
        let parts = a.parts(t).unwrap();
        let cube = parts.iter().find(|p| p.name == "box/Cube").unwrap().clone();
        let tagged = a.with_face_sets(vec![cube.clone()], t, false).unwrap();
        assert_eq!(tagged.len(), 1);
        let (top, sides) = (tagged[0].mesh.face_attrs.int("faceset.Top").unwrap(), tagged[0].mesh.face_attrs.int("faceset.Sides").unwrap());
        assert_eq!((top.iter().sum::<i32>(), sides.iter().sum::<i32>()), (2, 10), "the top quad's 2 triangles, the sides' 10");
        // The triangles in Top face up (file +Y).
        let m = &tagged[0].mesh;
        for (k, tri) in m.triangles.iter().enumerate().filter(|(k, _)| top[*k] == 1) {
            let [a, b, c] = tri.map(|i| m.positions[i as usize]);
            assert!((b - a).cross(&(c - a)).normalize().y > 0.99, "triangle {k}");
        }
        // Split: a part per set, named after the mesh and set, materials too.
        let split = a.with_face_sets(vec![cube.clone()], t, true).unwrap();
        assert_eq!(split.iter().map(|p| (p.name.as_str(), p.material.name.as_str(), p.mesh.triangles.len())).collect::<Vec<_>>(), [("box/Cube/Sides", "Sides", 10), ("box/Cube/Top", "Top", 2)]);
        assert_eq!(split[1].mesh.positions.len(), 4, "only the vertices it uses");
        // A mesh without sets is left as it is.
        let mut plain = cube;
        plain.name = "nowhere".into();
        assert_eq!(a.with_face_sets(vec![plain.clone()], t, true).unwrap()[0].mesh.triangles, plain.mesh.triangles);
    }

    #[test]
    fn cubic_bases_and_nurbs_follow_their_formulas() {
        let p = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 2.0, 0.0), Vec3::new(3.0, 2.0, 0.0), Vec3::new(4.0, 0.0, 0.0)];
        // Bezier ends on its end points; B-spline starts at (p0 + 4 p1 + p2) / 6.
        let b = cubic(&p, 1, false);
        assert!(near(b[0], p[0]) && near(*b.last().unwrap(), p[3]));
        let s = cubic(&p, 2, false);
        assert!(near(s[0], (p[0] + p[1] * 4.0 + p[2]) / 6.0));
        // Catmull-Rom passes through p1 and p2.
        let c = cubic(&p, 3, false);
        assert!(near(c[0], p[1]) && near(*c.last().unwrap(), p[2]));
        // Periodic: STEPS a span, all round, no repeated end point.
        assert_eq!(cubic(&p, 2, true).len(), 4 * STEPS);
        // A clamped cubic NURBS with unit weights is the Bezier.
        let n = nurbs(&p, &[], 4, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(n.len(), b.len());
        assert!(n.iter().zip(&b).all(|(x, y)| near(*x, *y)));
        // Too few points: as they are.
        assert_eq!(cubic(&p[..2], 1, false), p[..2].to_vec());
    }
}
