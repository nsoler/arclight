//! A scene's geometry as a table, as Blender's spreadsheet shows it: one
//! domain at a time (point sets' points, mesh points, mesh faces,
//! instances, curves' points), a row per element, a column per value (P,
//! N, uv, then each attribute, vectors split into components). The GUI's
//! Geo Info sheet draws rows from it one at a time, and the `geometry` MCP
//! tool (and Python's `geometry`) pages its columns, so they agree.
//!
//! Values are in each object's own space, as stored (its `world` places
//! it); an instance's P, rotation and scale are its world matrix's. The
//! sheet can also show P and N in world space ([`View::world`]) and a
//! Cloud tab (Camera Trackers' points, [`Domain::Cloud`]); the MCP tool
//! uses neither.

use crate::geo::{Attr, Attrs};
use crate::geom::{Mat4, Vec3};
use crate::mesh::Mesh;
use crate::scene::{Model, Scene};
use serde_json::{json, Value};

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Domain {
    /// The points of point sets.
    Points,
    /// Mesh vertices.
    MeshPoints,
    /// Mesh triangles.
    Faces,
    Instances,
    /// The points of curves.
    CurvePoints,
    /// The scene's point cloud (Camera Trackers' solved points), by node.
    /// The sheet's only: not in [`Domain::ALL`], so the MCP tool's
    /// domains stay as they were.
    Cloud,
}

impl Domain {
    pub const ALL: [Domain; 5] = [Domain::Points, Domain::MeshPoints, Domain::Faces, Domain::Instances, Domain::CurvePoints];
    /// The sheet's tabs: [`Domain::ALL`], then the Cloud.
    pub const SHEET: [Domain; 6] = [Domain::Points, Domain::MeshPoints, Domain::Faces, Domain::Instances, Domain::CurvePoints, Domain::Cloud];

    /// Its name in the MCP tool and Python.
    pub fn key(self) -> &'static str {
        match self {
            Domain::Points => "points",
            Domain::MeshPoints => "mesh_points",
            Domain::Faces => "faces",
            Domain::Instances => "instances",
            Domain::CurvePoints => "curve_points",
            Domain::Cloud => "cloud",
        }
    }

    /// Its name in the sheet's tabs.
    pub fn label(self) -> &'static str {
        match self {
            Domain::Points => "Points",
            Domain::MeshPoints => "Mesh points",
            Domain::Faces => "Faces",
            Domain::Instances => "Instances",
            Domain::CurvePoints => "Curve points",
            Domain::Cloud => "Cloud",
        }
    }

    pub fn from_key(key: &str) -> Option<Domain> {
        Domain::ALL.into_iter().find(|d| d.key() == key)
    }
}

/// Which objects the rows come from: one node's (`object`), and when
/// `nodes` is set only those nodes' (the selection).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub object: Option<String>,
    pub nodes: Option<Vec<String>>,
}

impl Filter {
    fn keeps(&self, node: &str) -> bool {
        self.object.as_deref().is_none_or(|o| o == node) && self.nodes.as_ref().is_none_or(|n| n.iter().any(|s| s == node))
    }
}

/// How the sheet shows the rows (the MCP tool uses the default).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// P and N in world space: each object's `world` applied (a point
    /// set's, a mesh's, a curve set's). Instances and the Cloud are in
    /// world space already; attributes stay as stored.
    pub world: bool,
    /// Points of the scene's point cloud picked in the 3D view (indices
    /// into `Scene::points`): a node with any picked shows only those in
    /// the Cloud tab.
    pub picked: Option<Vec<usize>>,
    /// Points of point sets and mesh vertices picked in the 3D view:
    /// (domain, index into `Scene::point_sets` / `Scene::meshes`, element).
    /// An object with any picked shows only those in its tab.
    pub picked_elements: Option<Vec<(Domain, usize, usize)>>,
}

/// One value of a row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cell<'a> {
    /// The object has no such value.
    Empty,
    Int(i64),
    Float(f64),
    Text(&'a str),
}

/// What a field's values are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Whole numbers (index, prototype, int attributes).
    Int,
    Float,
    /// The object's node.
    Text,
}

/// A named value of each row, 1 to 4 components wide (P is x, y, z).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    /// Its components' names (one, empty, for a scalar).
    pub comps: Vec<&'static str>,
    /// A color (r g b a): the sheet shows a swatch.
    pub color: bool,
    source: Source,
}

impl Field {
    pub fn width(&self) -> usize {
        self.comps.len().max(1)
    }

    /// Its header: the name, or name.component.
    pub fn header(&self, comp: usize) -> String {
        match self.comps.get(comp) {
            Some(c) if !c.is_empty() => format!("{}.{c}", self.name),
            _ => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Source {
    Index,
    Object,
    /// A curve point's curve.
    Curve,
    /// An instance's prototype.
    Prototype,
    Position,
    Normal,
    Uv,
    Rotation,
    Scale,
    Attr,
}

/// One object's elements in a domain.
#[derive(Clone)]
enum Part<'a> {
    Points(&'a crate::geo::PointSet),
    MeshPoints(&'a Model),
    Faces(&'a Model),
    Instances(&'a crate::geo::Instances),
    Curves(&'a crate::geo::Curves),
    /// A node's points of the scene's cloud: (index in `Scene::points`,
    /// its number among the node's points) for each row.
    Cloud(&'a [Vec3], &'a str, Vec<(usize, usize)>),
}

impl<'a> Part<'a> {
    fn node(&self) -> &'a str {
        match *self {
            Part::Points(p) => &p.node,
            Part::MeshPoints(m) | Part::Faces(m) => &m.node,
            Part::Instances(i) => &i.node,
            Part::Curves(c) => &c.node,
            Part::Cloud(_, n, _) => n,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Part::Points(_) => "point set",
            Part::MeshPoints(..) | Part::Faces(..) => "mesh",
            Part::Instances(_) => "instances",
            Part::Curves(_) => "curves",
            Part::Cloud(..) => "point cloud",
        }
    }

    fn len(&self) -> usize {
        match self {
            Part::Points(p) => p.positions.len(),
            Part::MeshPoints(m) => m.mesh.positions.len(),
            Part::Faces(m) => m.mesh.triangles.len(),
            Part::Instances(i) => i.worlds.len(),
            Part::Curves(c) => c.positions.len(),
            Part::Cloud(_, _, rows) => rows.len(),
        }
    }

    fn attrs(&self) -> Option<&'a Attrs> {
        match *self {
            Part::Points(p) => Some(&p.attrs),
            Part::MeshPoints(m) => Some(&m.mesh.point_attrs),
            Part::Faces(m) => Some(&m.mesh.face_attrs),
            Part::Instances(i) => Some(&i.attrs),
            Part::Curves(c) => Some(&c.attrs),
            Part::Cloud(..) => None,
        }
    }

    /// The matrix placing it in the scene, for [`View::world`] (None:
    /// already in world space).
    fn world(&self) -> Option<&'a Mat4> {
        match *self {
            Part::Points(p) => Some(&p.world),
            Part::MeshPoints(m) | Part::Faces(m) => Some(&m.world),
            Part::Curves(c) => Some(&c.world),
            Part::Instances(_) | Part::Cloud(..) => None,
        }
    }
}

/// The scene's point cloud by node, in order of each node's first point:
/// each point's index and its number among the node's points (a Camera
/// Tracker's own numbering). With `picked`, a node that has any picked
/// keeps only those.
fn cloud<'a>(scene: &'a Scene, picked: Option<&[usize]>) -> Vec<Part<'a>> {
    let mut nodes: Vec<(&'a str, Vec<(usize, usize)>)> = Vec::new();
    for i in 0..scene.points.len() {
        let node = scene.point_node(i);
        let k = match nodes.iter().position(|(n, _)| *n == node) {
            Some(k) => k,
            None => {
                nodes.push((node, Vec::new()));
                nodes.len() - 1
            }
        };
        let number = nodes[k].1.len();
        nodes[k].1.push((i, number));
    }
    if let Some(picked) = picked {
        for (_, rows) in &mut nodes {
            if rows.iter().any(|(i, _)| picked.contains(i)) {
                rows.retain(|(i, _)| picked.contains(i));
            }
        }
    }
    nodes.into_iter().map(|(n, rows)| Part::Cloud(&scene.points, n, rows)).collect()
}

/// The objects of `scene` with elements in `domain`, kept by `filter`.
fn parts<'a>(scene: &'a Scene, domain: Domain, filter: &Filter, view: &View) -> Vec<Part<'a>> {
    let all: Vec<Part<'a>> = match domain {
        Domain::Points => scene.point_sets.iter().map(Part::Points).collect(),
        Domain::MeshPoints => scene.meshes.iter().map(Part::MeshPoints).collect(),
        Domain::Faces => scene.meshes.iter().map(Part::Faces).collect(),
        Domain::Instances => scene.instances.iter().map(Part::Instances).collect(),
        Domain::CurvePoints => scene.curves.iter().map(Part::Curves).collect(),
        Domain::Cloud => cloud(scene, view.picked.as_deref()),
    };
    all.into_iter().filter(|p| p.len() > 0 && filter.keeps(p.node())).collect()
}

/// How many rows each domain has in `scene`, kept by `filter`.
pub fn counts(scene: &Scene, filter: &Filter) -> Vec<(Domain, usize)> {
    Domain::ALL.into_iter().map(|d| (d, parts(scene, d, filter, &View::default()).iter().map(Part::len).sum())).collect()
}

/// The sheet's tabs' counts ([`Domain::SHEET`]), as `view` shows them.
pub fn sheet_counts(scene: &Scene, filter: &Filter, view: &View) -> Vec<(Domain, usize)> {
    Domain::SHEET
        .into_iter()
        .map(|d| (d, parts(scene, d, filter, view).iter().map(|p| subset(scene, p, view).map_or(p.len(), |r| r.len())).sum()))
        .collect()
}

/// The elements of `part` a view's picks keep (sorted), when it has any
/// picked ([`View::picked_elements`]); None: all of them.
fn subset(scene: &Scene, part: &Part, view: &View) -> Option<Vec<usize>> {
    let picks = view.picked_elements.as_ref()?;
    let (domain, object) = match *part {
        Part::Points(g) => (Domain::Points, scene.point_sets.iter().position(|x| std::ptr::eq(x, g))?),
        Part::MeshPoints(m) => (Domain::MeshPoints, scene.meshes.iter().position(|x| std::ptr::eq(x, m))?),
        _ => return None,
    };
    let mut rows: Vec<usize> = picks.iter().filter(|(d, o, i)| *d == domain && *o == object && *i < part.len()).map(|p| p.2).collect();
    rows.sort_unstable();
    rows.dedup();
    (!rows.is_empty()).then_some(rows)
}

/// The objects with rows in a domain: (node, kind, rows).
pub type Object<'a> = (&'a str, &'static str, usize);

/// One domain of a scene as rows and fields.
pub struct Table<'a> {
    pub domain: Domain,
    parts: Vec<Part<'a>>,
    /// Each part's first row, and the total after the last.
    starts: Vec<usize>,
    /// Each part's picked elements, when only those show ([`subset`]).
    subsets: Vec<Option<Vec<usize>>>,
    fields: Vec<Field>,
    /// P and N in world space ([`View::world`]).
    world: bool,
}

/// An attribute's components and whether it's a color.
fn attr_comps(name: &str, attr: &Attr) -> (FieldKind, Vec<&'static str>, bool) {
    match attr {
        Attr::Float(_) => (FieldKind::Float, vec![""], false),
        Attr::Int(_) => (FieldKind::Int, vec![""], false),
        Attr::Vec3(_) => (FieldKind::Float, vec!["x", "y", "z"], false),
        Attr::Vec4(_) if is_color(name) => (FieldKind::Float, vec!["r", "g", "b", "a"], true),
        Attr::Vec4(_) => (FieldKind::Float, vec!["x", "y", "z", "w"], false),
    }
}

/// Whether a 4-wide attribute is a color (Cd and its like), not a
/// quaternion (orient) or other.
fn is_color(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with(crate::geo::CD) || name.starts_with(crate::geo::ALPHA) || lower.contains("color") || lower.contains("colour")
}

fn field(name: &str, kind: FieldKind, comps: &[&'static str], source: Source) -> Field {
    Field { name: name.to_string(), kind, comps: comps.to_vec(), color: false, source }
}

impl<'a> Table<'a> {
    pub fn new(scene: &'a Scene, domain: Domain, filter: &Filter) -> Table<'a> {
        Table::with_view(scene, domain, filter, &View::default())
    }

    /// The table as the sheet's `view` shows it: world space, picks.
    pub fn with_view(scene: &'a Scene, domain: Domain, filter: &Filter, view: &View) -> Table<'a> {
        let parts = parts(scene, domain, filter, view);
        let subsets: Vec<Option<Vec<usize>>> = parts.iter().map(|p| subset(scene, p, view)).collect();
        let mut starts = vec![0];
        for (p, sub) in parts.iter().zip(&subsets) {
            starts.push(starts.last().unwrap() + sub.as_ref().map_or(p.len(), Vec::len));
        }
        let xyz = ["x", "y", "z"];
        let mut fields = vec![field("index", FieldKind::Int, &[""], Source::Index)];
        if parts.len() > 1 {
            fields.push(field("object", FieldKind::Text, &[""], Source::Object));
        }
        let any = |f: fn(&Part) -> bool| parts.iter().any(f);
        match domain {
            Domain::CurvePoints => fields.push(field("curve", FieldKind::Int, &[""], Source::Curve)),
            Domain::Instances => fields.push(field("prototype", FieldKind::Int, &[""], Source::Prototype)),
            _ => {}
        }
        fields.push(field("P", FieldKind::Float, &xyz, Source::Position));
        match domain {
            Domain::MeshPoints => {
                if any(|p| matches!(p, Part::MeshPoints(m) if !m.mesh.normals.is_empty())) {
                    fields.push(field("N", FieldKind::Float, &xyz, Source::Normal));
                }
                if any(|p| matches!(p, Part::MeshPoints(m) if !m.mesh.uvs.is_empty())) {
                    fields.push(field("uv", FieldKind::Float, &["u", "v"], Source::Uv));
                }
            }
            Domain::Faces => fields.push(field("N", FieldKind::Float, &xyz, Source::Normal)),
            Domain::Instances => {
                fields.push(field("rotation", FieldKind::Float, &xyz, Source::Rotation));
                fields.push(field("scale", FieldKind::Float, &xyz, Source::Scale));
            }
            _ => {}
        }
        // Attributes: every name any object has, in name order (the first
        // object to have it decides its kind).
        let mut names: Vec<(&str, &Attr)> = Vec::new();
        for p in &parts {
            for (name, attr) in p.attrs().into_iter().flat_map(Attrs::iter) {
                if !names.iter().any(|(n, _)| *n == name) {
                    names.push((name, attr));
                }
            }
        }
        names.sort_by_key(|(n, _)| *n);
        for (name, attr) in names {
            let (kind, comps, color) = attr_comps(name, attr);
            fields.push(Field { name: name.to_string(), kind, comps, color, source: Source::Attr });
        }
        Table { domain, parts, starts, subsets, fields, world: view.world }
    }

    /// Rows in all.
    pub fn len(&self) -> usize {
        *self.starts.last().unwrap_or(&0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The objects the rows come from, in row order.
    pub fn objects(&self) -> Vec<Object<'a>> {
        self.parts.iter().enumerate().map(|(k, p)| (p.node(), p.kind(), self.starts[k + 1] - self.starts[k])).collect()
    }

    /// Row `row`'s object (by its place in [`Table::objects`]) and element.
    fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len() {
            return None;
        }
        let k = self.starts.partition_point(|&s| s <= row) - 1;
        let i = row - self.starts[k];
        // Only the picked elements: the row's element.
        Some((k, self.subsets[k].as_ref().map_or(i, |r| r[i])))
    }

    /// Component `comp` of field `field` in row `row`.
    pub fn cell(&self, row: usize, field: usize, comp: usize) -> Cell<'a> {
        let (Some((k, i)), Some(f)) = (self.locate(row), self.fields.get(field)) else { return Cell::Empty };
        let part = &self.parts[k];
        let v3 = |v: Vec3| v.get(comp).map_or(Cell::Empty, |x| Cell::Float(*x));
        // In world space (the sheet's World): points placed, normals
        // turned (by the inverse transpose, kept unit length).
        let world = part.world().filter(|_| self.world);
        let point = |p: Vec3| v3(world.map_or(p, |m| crate::geom::transform_point(m, &p)));
        let normal = |n: Vec3| {
            v3(match world.and_then(|m| m.fixed_view::<3, 3>(0, 0).try_inverse()) {
                Some(inv) => {
                    let w = inv.transpose() * n;
                    if w.norm() > 0.0 { w.normalize() } else { w }
                }
                None => n,
            })
        };
        match f.source {
            Source::Index => match part {
                Part::Cloud(_, _, rows) => Cell::Int(rows[i].1 as i64),
                _ => Cell::Int(i as i64),
            },
            Source::Object => Cell::Text(part.node()),
            Source::Curve => match part {
                Part::Curves(c) => Cell::Int((c.starts.partition_point(|&s| s as usize <= i) as i64) - 1),
                _ => Cell::Empty,
            },
            Source::Prototype => match part {
                Part::Instances(n) => n.which.get(i).map_or(Cell::Empty, |w| Cell::Int(*w as i64)),
                _ => Cell::Empty,
            },
            Source::Position => match part {
                Part::Points(p) => point(p.positions[i]),
                Part::MeshPoints(m) => point(m.mesh.positions[i]),
                Part::Faces(m) => point(face(&m.mesh, i).0),
                Part::Instances(n) => v3(decompose(&n.worlds[i]).0),
                Part::Curves(c) => point(c.positions[i]),
                Part::Cloud(pts, _, rows) => v3(pts[rows[i].0]),
            },
            Source::Normal => match part {
                Part::MeshPoints(m) => m.mesh.normals.get(i).map_or(Cell::Empty, |n| normal(*n)),
                Part::Faces(m) => normal(face(&m.mesh, i).1),
                _ => Cell::Empty,
            },
            Source::Uv => match part {
                Part::MeshPoints(m) => m.mesh.uvs.get(i).and_then(|uv| uv.get(comp)).map_or(Cell::Empty, |x| Cell::Float(*x)),
                _ => Cell::Empty,
            },
            Source::Rotation => match part {
                Part::Instances(n) => v3(decompose(&n.worlds[i]).1),
                _ => Cell::Empty,
            },
            Source::Scale => match part {
                Part::Instances(n) => v3(decompose(&n.worlds[i]).2),
                _ => Cell::Empty,
            },
            Source::Attr => attr_cell(part.attrs().and_then(|a| a.get(&f.name)), i, comp),
        }
    }

    /// Rows `offset..offset + limit` as JSON columns: each field's values
    /// in row order (a vector field's as arrays; null where an object
    /// lacks it), with the domain, the objects and the paging.
    pub fn page(&self, offset: usize, limit: usize) -> Value {
        let end = offset.saturating_add(limit).min(self.len());
        let rows = offset.min(end)..end;
        let objects: Vec<Value> = self.objects().iter().map(|(node, kind, count)| json!({"node": node, "kind": kind, "count": count})).collect();
        let mut columns = serde_json::Map::new();
        for (k, f) in self.fields.iter().enumerate() {
            let value = |c: Cell| match c {
                Cell::Empty => Value::Null,
                Cell::Int(v) => json!(v),
                Cell::Float(v) => json!(v),
                Cell::Text(t) => json!(t),
            };
            let col: Vec<Value> = rows
                .clone()
                .map(|r| {
                    if f.source == Source::Object {
                        // An index into `objects`.
                        return json!(self.locate(r).map(|(k, _)| k));
                    }
                    if f.width() == 1 {
                        return value(self.cell(r, k, 0));
                    }
                    let comps: Vec<Value> = (0..f.width()).map(|c| value(self.cell(r, k, c))).collect();
                    if comps.iter().all(Value::is_null) { Value::Null } else { Value::Array(comps) }
                })
                .collect();
            columns.insert(f.name.clone(), Value::Array(col));
        }
        json!({
            "domain": self.domain.key(),
            "objects": objects,
            "columns": columns,
            "count": rows.len(),
            "total": self.len(),
            "next_offset": (end < self.len()).then_some(end),
        })
    }
}

fn attr_cell(attr: Option<&Attr>, i: usize, comp: usize) -> Cell<'static> {
    let f = |x: Option<&f32>| x.map_or(Cell::Empty, |v| Cell::Float(*v as f64));
    match attr {
        None => Cell::Empty,
        Some(Attr::Float(v)) => f(v.get(i).filter(|_| comp == 0)),
        Some(Attr::Int(v)) => v.get(i).filter(|_| comp == 0).map_or(Cell::Empty, |x| Cell::Int(*x as i64)),
        Some(Attr::Vec3(v)) => f(v.get(i).and_then(|x| x.get(comp))),
        Some(Attr::Vec4(v)) => f(v.get(i).and_then(|x| x.get(comp))),
    }
}

/// Triangle `i`'s center and unit normal (counter-clockwise front).
fn face(m: &Mesh, i: usize) -> (Vec3, Vec3) {
    let [a, b, c] = m.triangles[i].map(|k| m.positions.get(k as usize).copied().unwrap_or_default());
    let n = (b - a).cross(&(c - a));
    let n = if n.norm() > 0.0 { n.normalize() } else { n };
    ((a + b + c) / 3.0, n)
}

/// A world matrix's translation, rotation (degrees, as `geom::trs` takes
/// them) and scale.
fn decompose(m: &Mat4) -> (Vec3, Vec3, Vec3) {
    let t = Vec3::new(m[(0, 3)], m[(1, 3)], m[(2, 3)]);
    let col = |k: usize| Vec3::new(m[(0, k)], m[(1, k)], m[(2, k)]);
    let s = Vec3::new(col(0).norm(), col(1).norm(), col(2).norm());
    let unit = |k: usize| if s[k] > 0.0 { col(k) / s[k] } else { col(k) };
    let r = nalgebra::Matrix3::from_columns(&[unit(0), unit(1), unit(2)]);
    let r = crate::geom::angles(&nalgebra::Rotation3::from_matrix_unchecked(r));
    (t, Vec3::from(r), s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{Curves, Instances, PointSet};
    use crate::geom::trs;
    use std::sync::Arc;

    fn attrs(list: Vec<(&str, Attr)>) -> Attrs {
        let mut a = Attrs::default();
        for (name, attr) in list {
            let n = attr.len();
            a.set(name, attr, n).unwrap();
        }
        a
    }

    /// Two meshes (one with a `w` point attribute and normals, one with
    /// uvs and a face attribute), a point set with Cd and orient, two
    /// instances, a two-curve set.
    fn scene() -> Scene {
        let tri = |dx: f64| Mesh {
            positions: vec![Vec3::new(dx, 0.0, 0.0), Vec3::new(dx + 1.0, 0.0, 0.0), Vec3::new(dx, 1.0, 0.0)],
            triangles: vec![[0, 1, 2]],
            ..Mesh::default()
        };
        let a = Mesh { normals: vec![Vec3::z(); 3], point_attrs: attrs(vec![("w", Attr::Float(vec![0.5, 1.5, 2.5]))]), ..tri(0.0) };
        let b = Mesh { uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], face_attrs: attrs(vec![("id", Attr::Int(vec![7]))]), ..tri(10.0) };
        let model = |m: Mesh, node: &str| Model { node: node.into(), ..Model::new(Arc::new(m), Mat4::identity(), Default::default()) };
        let points = PointSet {
            positions: vec![Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0)],
            attrs: attrs(vec![("Cd", Attr::Vec4(vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.5]])), ("orient", Attr::Vec4(vec![[0.0, 0.0, 0.0, 1.0]; 2]))]),
            node: "scatter".into(),
            ..PointSet::default()
        };
        let instances = Instances {
            which: vec![0, 1],
            worlds: vec![trs([1.0, 2.0, 3.0], [0.0, 90.0, 0.0], [2.0, 2.0, 2.0]), trs([0.0; 3], [30.0, 0.0, 0.0], [1.0, 3.0, 1.0])],
            node: "inst".into(),
            ..Instances::default()
        };
        let mut curves = Curves { node: "road".into(), ..Curves::default() };
        curves.push(&[Vec3::zeros(), Vec3::x()], false);
        curves.push(&[Vec3::y(), Vec3::y() * 2.0, Vec3::y() * 3.0], false);
        Scene { meshes: vec![model(a, "rockA"), model(b, "rockB")], point_sets: vec![points], instances: vec![instances], curves: vec![curves], ..Scene::default() }
    }

    fn headers(t: &Table) -> Vec<String> {
        t.fields().iter().flat_map(|f| (0..f.width()).map(move |c| f.header(c))).collect()
    }

    fn f(t: &Table, row: usize, name: &str, comp: usize) -> Cell<'static> {
        let k = t.fields().iter().position(|f| f.name == name).unwrap_or_else(|| panic!("no field {name}"));
        match t.cell(row, k, comp) {
            Cell::Text(_) => panic!("text"),
            Cell::Empty => Cell::Empty,
            Cell::Int(v) => Cell::Int(v),
            Cell::Float(v) => Cell::Float(v),
        }
    }

    #[test]
    fn domains_count_their_elements_and_the_filter_narrows_them() {
        let s = scene();
        let n = |filter: &Filter| counts(&s, filter).into_iter().map(|(d, n)| (d.key(), n)).collect::<Vec<_>>();
        assert_eq!(n(&Filter::default()), [("points", 2), ("mesh_points", 6), ("faces", 2), ("instances", 2), ("curve_points", 5)]);
        assert_eq!(n(&Filter { object: Some("rockB".into()), nodes: None }), [("points", 0), ("mesh_points", 3), ("faces", 1), ("instances", 0), ("curve_points", 0)]);
        let sel = Filter { object: None, nodes: Some(vec!["rockA".into(), "road".into()]) };
        assert_eq!(n(&sel), [("points", 0), ("mesh_points", 3), ("faces", 1), ("instances", 0), ("curve_points", 5)]);
        assert_eq!(Domain::from_key("faces"), Some(Domain::Faces));
        assert!(Domain::ALL.iter().all(|d| Domain::from_key(d.key()) == Some(*d)));
    }

    #[test]
    fn mesh_points_are_rows_across_objects_with_the_union_of_their_values() {
        let s = scene();
        let t = Table::new(&s, Domain::MeshPoints, &Filter::default());
        assert_eq!(headers(&t), ["index", "object", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "uv.u", "uv.v", "w"]);
        assert_eq!(t.len(), 6);
        assert_eq!(t.objects(), [("rockA", "mesh", 3), ("rockB", "mesh", 3)]);
        // Row 3 is rockB's first point: index restarts, its own P, no N or w, its uv.
        let k = |name: &str| t.fields().iter().position(|f| f.name == name).unwrap();
        assert_eq!(t.cell(2, k("object"), 0), Cell::Text("rockA"));
        assert_eq!(t.cell(3, k("object"), 0), Cell::Text("rockB"));
        assert_eq!((f(&t, 3, "index", 0), f(&t, 3, "P", 0), f(&t, 3, "N", 2), f(&t, 3, "w", 0)), (Cell::Int(0), Cell::Float(10.0), Cell::Empty, Cell::Empty));
        assert_eq!((f(&t, 5, "uv", 1), f(&t, 1, "uv", 0)), (Cell::Float(1.0), Cell::Empty));
        assert_eq!((f(&t, 2, "w", 0), f(&t, 2, "N", 2)), (Cell::Float(2.5), Cell::Float(1.0)));
        assert_eq!(t.cell(6, 0, 0), Cell::Empty, "past the end");
        // One object: no object column.
        let one = Table::new(&s, Domain::MeshPoints, &Filter { object: Some("rockA".into()), nodes: None });
        assert_eq!(headers(&one), ["index", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "w"]);
    }

    #[test]
    fn faces_points_instances_and_curves_have_their_own_values() {
        let s = scene();
        let faces = Table::new(&s, Domain::Faces, &Filter::default());
        assert_eq!(headers(&faces), ["index", "object", "P.x", "P.y", "P.z", "N.x", "N.y", "N.z", "id"]);
        assert_eq!((f(&faces, 1, "P", 0), f(&faces, 1, "P", 1), f(&faces, 1, "N", 2), f(&faces, 1, "id", 0)), (Cell::Float(10.0 + 1.0 / 3.0), Cell::Float(1.0 / 3.0), Cell::Float(1.0), Cell::Int(7)));
        let points = Table::new(&s, Domain::Points, &Filter::default());
        assert_eq!(headers(&points), ["index", "P.x", "P.y", "P.z", "Cd.r", "Cd.g", "Cd.b", "Cd.a", "orient.x", "orient.y", "orient.z", "orient.w"]);
        assert!(points.fields()[2].color && !points.fields()[3].color, "Cd a color, orient not");
        assert_eq!(f(&points, 1, "Cd", 3), Cell::Float(0.5));
        let inst = Table::new(&s, Domain::Instances, &Filter::default());
        assert_eq!(headers(&inst), ["index", "prototype", "P.x", "P.y", "P.z", "rotation.x", "rotation.y", "rotation.z", "scale.x", "scale.y", "scale.z"]);
        let near = |c: Cell, v: f64| matches!(c, Cell::Float(x) if (x - v).abs() < 1e-9);
        assert!(near(f(&inst, 0, "P", 2), 3.0) && near(f(&inst, 0, "rotation", 1), 90.0) && near(f(&inst, 0, "scale", 0), 2.0));
        assert!(near(f(&inst, 1, "rotation", 0), 30.0) && near(f(&inst, 1, "scale", 1), 3.0) && f(&inst, 1, "prototype", 0) == Cell::Int(1));
        let curves = Table::new(&s, Domain::CurvePoints, &Filter::default());
        assert_eq!(headers(&curves), ["index", "curve", "P.x", "P.y", "P.z"]);
        let which: Vec<_> = (0..5).map(|r| f(&curves, r, "curve", 0)).collect();
        assert_eq!(which, [0, 0, 1, 1, 1].map(Cell::Int));
    }

    #[test]
    fn pages_are_columns_with_nulls_where_an_object_lacks_a_value() {
        let s = scene();
        let t = Table::new(&s, Domain::MeshPoints, &Filter::default());
        let p = t.page(2, 2);
        assert_eq!((p["count"].as_u64(), p["total"].as_u64(), p["next_offset"].as_u64()), (Some(2), Some(6), Some(4)));
        let c = &p["columns"];
        assert_eq!(c["index"], json!([2, 0]));
        assert_eq!(c["object"], json!([0, 1]));
        assert_eq!(c["P"], json!([[0.0, 1.0, 0.0], [10.0, 0.0, 0.0]]));
        assert_eq!((c["N"][1].clone(), c["w"][1].clone(), c["uv"][0].clone()), (Value::Null, Value::Null, Value::Null));
        assert_eq!(p["objects"][1], json!({"node": "rockB", "kind": "mesh", "count": 3}));
        let last = t.page(4, 1000);
        assert_eq!((last["count"].as_u64(), last["next_offset"].clone()), (Some(2), Value::Null));
        assert_eq!(t.page(99, 10)["count"], 0);
    }

    /// World shows P and N placed by each object's `world`; instances made
    /// on points that were moved are in world space already, so the same.
    #[test]
    fn world_places_points_and_turns_normals() {
        let mut s = scene();
        let moved = trs([10.0, 0.0, 0.0], [0.0, 0.0, 90.0], [2.0, 1.0, 1.0]);
        s.meshes[0].world = moved;
        s.point_sets[0].world = trs([0.0, 5.0, 0.0], [0.0; 3], [1.0; 3]);
        s.curves[0].world = trs([0.0, 0.0, -1.0], [0.0; 3], [3.0; 3]);
        let world = View { world: true, picked: None, ..View::default() };
        let near = |c: Cell, v: f64| matches!(c, Cell::Float(x) if (x - v).abs() < 1e-9);
        let rock = Filter { object: Some("rockA".into()), nodes: None };
        let (local, placed) = (Table::new(&s, Domain::MeshPoints, &rock), Table::with_view(&s, Domain::MeshPoints, &rock, &world));
        // (1, 0, 0): stretched ×2 along x, turned 90° about z, moved +10 x.
        assert!(near(f(&local, 1, "P", 0), 1.0) && near(f(&placed, 1, "P", 0), 10.0) && near(f(&placed, 1, "P", 1), 2.0));
        assert!(near(f(&placed, 0, "N", 2), 1.0), "+Z stays +Z");
        let faces = Table::with_view(&s, Domain::Faces, &rock, &world);
        assert!(near(f(&faces, 0, "P", 0), 10.0 - 1.0 / 3.0) && near(f(&faces, 0, "N", 2), 1.0));
        // A normal under a non-uniform stretch: the inverse transpose, unit.
        let mut tilted = scene();
        tilted.meshes[0].world = trs([0.0; 3], [0.0; 3], [2.0, 1.0, 1.0]);
        Arc::make_mut(&mut tilted.meshes[0].mesh).normals = vec![Vec3::new(1.0, 1.0, 0.0).normalize(); 3];
        let t = Table::with_view(&tilted, Domain::MeshPoints, &rock, &world);
        let n = Vec3::new(0.5, 1.0, 0.0).normalize();
        assert!(near(f(&t, 0, "N", 0), n.x) && near(f(&t, 0, "N", 1), n.y));
        let points = Table::with_view(&s, Domain::Points, &Filter::default(), &world);
        assert!(near(f(&points, 0, "P", 1), 7.0) && near(f(&points, 0, "Cd", 0), 1.0), "attributes as stored");
        let curves = Table::with_view(&s, Domain::CurvePoints, &Filter::default(), &world);
        assert!(near(f(&curves, 2, "P", 1), 3.0) && near(f(&curves, 2, "P", 2), -1.0));
        // Instances on points placed by a moved instancer: already world.
        let mut pts = PointSet { positions: vec![Vec3::new(1.0, 0.0, 0.0)], node: "pts".into(), ..PointSet::default() };
        pts.world = trs([0.0, 0.0, 4.0], [0.0, 90.0, 0.0], [1.0; 3]);
        let protos = Scene { meshes: vec![Model { node: "cube".into(), ..Model::new(Arc::new(crate::mesh::cube([1.0; 3], [1; 3])), Mat4::identity(), Default::default()) }], ..Scene::default() };
        let mut out = crate::ops::geo_ops::instance_on_points(&json!({}), Scene { point_sets: vec![pts], ..Scene::default() }, Some(&protos), None).unwrap();
        out.instances[0].node = "inst".into();
        let (a, b) = (Table::new(&out, Domain::Instances, &Filter::default()), Table::with_view(&out, Domain::Instances, &Filter::default(), &world));
        fn row(t: &Table) -> Vec<String> {
            (0..t.fields().len()).flat_map(|k| (0..t.fields()[k].width()).map(move |c| (k, c))).map(|(k, c)| format!("{:?}", t.cell(0, k, c))).collect()
        }
        assert_eq!(row(&a), row(&b), "instances the same in World");
        assert!(near(f(&b, 0, "P", 0), 0.0) && near(f(&b, 0, "P", 2), 3.0), "(1, 0, 0) turned 90° about y, then +4 z: the instancer's move is in");
    }

    /// The Cloud: the scene's points by node, numbered as each Camera
    /// Tracker numbers its own; picks narrow a node; the MCP domains don't
    /// change.
    #[test]
    fn the_cloud_is_by_node_numbered_per_node_and_picks_narrow_it() {
        let v = |x: f64| Vec3::new(x, 0.0, 0.0);
        let s = Scene { points: vec![v(0.0), v(1.0), v(2.0), v(10.0), v(11.0)], point_nodes: ["trackA", "trackA", "trackA", "trackB", "trackB"].map(String::from).to_vec(), ..scene() };
        let none = View::default();
        let t = Table::with_view(&s, Domain::Cloud, &Filter::default(), &none);
        assert_eq!(t.objects(), [("trackA", "point cloud", 3), ("trackB", "point cloud", 2)]);
        assert_eq!(headers(&t), ["index", "object", "P.x", "P.y", "P.z"]);
        assert_eq!((f(&t, 3, "index", 0), f(&t, 3, "P", 0)), (Cell::Int(0), Cell::Float(10.0)), "trackB's point 0 is scene point 3");
        // Picks: trackA's scene point 2 (its number 2) and trackB's 4.
        let picked = View { world: false, picked: Some(vec![2, 4]), ..View::default() };
        let t = Table::with_view(&s, Domain::Cloud, &Filter::default(), &picked);
        assert_eq!(t.len(), 2);
        assert_eq!((f(&t, 0, "index", 0), f(&t, 0, "P", 0), f(&t, 1, "index", 0), f(&t, 1, "P", 0)), (Cell::Int(2), Cell::Float(2.0), Cell::Int(1), Cell::Float(11.0)));
        // A node without picks keeps all its points.
        let t = Table::with_view(&s, Domain::Cloud, &Filter::default(), &View { world: true, picked: Some(vec![0]), ..View::default() });
        assert_eq!(t.objects(), [("trackA", "point cloud", 1), ("trackB", "point cloud", 2)]);
        let sel = Filter { object: None, nodes: Some(vec!["trackB".into()]) };
        assert_eq!(sheet_counts(&s, &sel, &none).last(), Some(&(Domain::Cloud, 2)));
        // The MCP tool: no Cloud domain, and the same pages as before.
        assert_eq!(counts(&s, &Filter::default()).len(), 5);
        assert_eq!(Domain::from_key("cloud"), None);
        assert_eq!(&sheet_counts(&s, &Filter::default(), &none)[..5], &counts(&s, &Filter::default())[..]);
        for d in Domain::ALL {
            assert_eq!(Table::new(&s, d, &Filter::default()).page(0, 100), Table::with_view(&s, d, &Filter::default(), &View { world: false, picked: Some(vec![1]), ..View::default() }).page(0, 100));
        }
    }

    /// A million rows: any row is found without walking the others.
    #[test]
    fn any_row_of_a_big_table_is_found_at_once() {
        let n = 1_000_000;
        let big = PointSet { positions: (0..n).map(|i| Vec3::new(i as f64, 0.0, 0.0)).collect(), node: "big".into(), ..PointSet::default() };
        let s = Scene { point_sets: vec![PointSet { positions: vec![Vec3::zeros(); 3], node: "small".into(), ..PointSet::default() }, big], ..Scene::default() };
        let t = Table::new(&s, Domain::Points, &Filter::default());
        let t0 = std::time::Instant::now();
        for r in (0..t.len()).step_by(997) {
            let _ = t.cell(r, 2, 0);
        }
        assert_eq!(f(&t, n + 2, "P", 0), Cell::Float((n - 1) as f64));
        assert!(t0.elapsed().as_millis() < 500, "{:?}", t0.elapsed());
    }

    /// Points and mesh vertices picked in the 3D view narrow their tabs:
    /// an object with any picked shows only those (by index), the others
    /// all theirs; other tabs and the MCP tool's tables are untouched.
    #[test]
    fn picked_elements_narrow_points_and_mesh_points() {
        let s = scene();
        let rock_a = s.meshes[0].mesh.positions.len();
        let view = View { picked_elements: Some(vec![(Domain::MeshPoints, 1, 0), (Domain::Points, 0, 1), (Domain::Points, 0, 99)]), ..View::default() };
        let pts = Table::with_view(&s, Domain::Points, &Filter::default(), &view);
        assert_eq!(pts.len(), 1);
        assert_eq!((f(&pts, 0, "index", 0), f(&pts, 0, "P", 0)), (Cell::Int(1), Cell::Float(4.0)));
        let mesh = Table::with_view(&s, Domain::MeshPoints, &Filter::default(), &view);
        assert_eq!(mesh.len(), rock_a + 1, "rockA whole, rockB's one picked");
        assert_eq!(mesh.objects(), [("rockA", "mesh", rock_a), ("rockB", "mesh", 1)]);
        assert_eq!(f(&mesh, rock_a, "index", 0), Cell::Int(0));
        let tabs = sheet_counts(&s, &Filter::default(), &view);
        assert_eq!(tabs[0], (Domain::Points, 1));
        assert_eq!(tabs[1], (Domain::MeshPoints, rock_a + 1));
        // Other tabs, and no picks at all: as before.
        for d in Domain::SHEET {
            if !matches!(d, Domain::Points | Domain::MeshPoints) {
                assert_eq!(Table::with_view(&s, d, &Filter::default(), &view).page(0, 100), Table::new(&s, d, &Filter::default()).page(0, 100));
            }
        }
        for d in Domain::ALL {
            let none = View { picked_elements: None, ..View::default() };
            assert_eq!(Table::new(&s, d, &Filter::default()).page(0, 100), Table::with_view(&s, d, &Filter::default(), &none).page(0, 100));
        }
        assert_eq!(counts_of(&s), counts(&s, &Filter::default()));
    }

    /// The MCP tool's counts, worked out as before picks existed.
    fn counts_of(s: &Scene) -> Vec<(Domain, usize)> {
        Domain::ALL.into_iter().map(|d| (d, Table::new(s, d, &Filter::default()).len())).collect()
    }

}
