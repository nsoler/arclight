//! A scene's geometry as a table, as Blender's spreadsheet shows it: one
//! domain at a time (point sets' points, mesh points, mesh faces,
//! instances, curves' points), a row per element, a column per value (P,
//! N, uv, then each attribute, vectors split into components). The GUI's
//! Geo Info sheet draws rows from it one at a time, and the `geometry` MCP
//! tool (and Python's `geometry`) pages its columns, so they agree.
//!
//! Values are in each object's own space, as stored (its `world` places
//! it); an instance's P, rotation and scale are its world matrix's.

use crate::geo::{Attr, Attrs};
use crate::geom::{Mat4, Vec3};
use crate::mesh::Mesh;
use crate::scene::Scene;
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
}

impl Domain {
    pub const ALL: [Domain; 5] = [Domain::Points, Domain::MeshPoints, Domain::Faces, Domain::Instances, Domain::CurvePoints];

    /// Its name in the MCP tool and Python.
    pub fn key(self) -> &'static str {
        match self {
            Domain::Points => "points",
            Domain::MeshPoints => "mesh_points",
            Domain::Faces => "faces",
            Domain::Instances => "instances",
            Domain::CurvePoints => "curve_points",
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
#[derive(Clone, Copy)]
enum Part<'a> {
    Points(&'a crate::geo::PointSet),
    MeshPoints(&'a Mesh, &'a str),
    Faces(&'a Mesh, &'a str),
    Instances(&'a crate::geo::Instances),
    Curves(&'a crate::geo::Curves),
}

impl<'a> Part<'a> {
    fn node(&self) -> &'a str {
        match *self {
            Part::Points(p) => &p.node,
            Part::MeshPoints(_, n) | Part::Faces(_, n) => n,
            Part::Instances(i) => &i.node,
            Part::Curves(c) => &c.node,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Part::Points(_) => "point set",
            Part::MeshPoints(..) | Part::Faces(..) => "mesh",
            Part::Instances(_) => "instances",
            Part::Curves(_) => "curves",
        }
    }

    fn len(&self) -> usize {
        match *self {
            Part::Points(p) => p.positions.len(),
            Part::MeshPoints(m, _) => m.positions.len(),
            Part::Faces(m, _) => m.triangles.len(),
            Part::Instances(i) => i.worlds.len(),
            Part::Curves(c) => c.positions.len(),
        }
    }

    fn attrs(&self) -> &'a Attrs {
        match *self {
            Part::Points(p) => &p.attrs,
            Part::MeshPoints(m, _) => &m.point_attrs,
            Part::Faces(m, _) => &m.face_attrs,
            Part::Instances(i) => &i.attrs,
            Part::Curves(c) => &c.attrs,
        }
    }
}

/// The objects of `scene` with elements in `domain`, kept by `filter`.
fn parts<'a>(scene: &'a Scene, domain: Domain, filter: &Filter) -> Vec<Part<'a>> {
    let all: Vec<Part<'a>> = match domain {
        Domain::Points => scene.point_sets.iter().map(Part::Points).collect(),
        Domain::MeshPoints => scene.meshes.iter().map(|m| Part::MeshPoints(&m.mesh, &m.node)).collect(),
        Domain::Faces => scene.meshes.iter().map(|m| Part::Faces(&m.mesh, &m.node)).collect(),
        Domain::Instances => scene.instances.iter().map(Part::Instances).collect(),
        Domain::CurvePoints => scene.curves.iter().map(Part::Curves).collect(),
    };
    all.into_iter().filter(|p| p.len() > 0 && filter.keeps(p.node())).collect()
}

/// How many rows each domain has in `scene`, kept by `filter`.
pub fn counts(scene: &Scene, filter: &Filter) -> Vec<(Domain, usize)> {
    Domain::ALL.into_iter().map(|d| (d, parts(scene, d, filter).iter().map(Part::len).sum())).collect()
}

/// The objects with rows in a domain: (node, kind, rows).
pub type Object<'a> = (&'a str, &'static str, usize);

/// One domain of a scene as rows and fields.
pub struct Table<'a> {
    pub domain: Domain,
    parts: Vec<Part<'a>>,
    /// Each part's first row, and the total after the last.
    starts: Vec<usize>,
    fields: Vec<Field>,
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
        let parts = parts(scene, domain, filter);
        let mut starts = vec![0];
        for p in &parts {
            starts.push(starts.last().unwrap() + p.len());
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
                if any(|p| matches!(p, Part::MeshPoints(m, _) if !m.normals.is_empty())) {
                    fields.push(field("N", FieldKind::Float, &xyz, Source::Normal));
                }
                if any(|p| matches!(p, Part::MeshPoints(m, _) if !m.uvs.is_empty())) {
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
            for (name, attr) in p.attrs().iter() {
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
        Table { domain, parts, starts, fields }
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
        self.parts.iter().map(|p| (p.node(), p.kind(), p.len())).collect()
    }

    /// Row `row`'s object (by its place in [`Table::objects`]) and element.
    fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len() {
            return None;
        }
        let k = self.starts.partition_point(|&s| s <= row) - 1;
        Some((k, row - self.starts[k]))
    }

    /// Component `comp` of field `field` in row `row`.
    pub fn cell(&self, row: usize, field: usize, comp: usize) -> Cell<'a> {
        let (Some((k, i)), Some(f)) = (self.locate(row), self.fields.get(field)) else { return Cell::Empty };
        let part = self.parts[k];
        let v3 = |v: Vec3| v.get(comp).map_or(Cell::Empty, |x| Cell::Float(*x));
        match f.source {
            Source::Index => Cell::Int(i as i64),
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
                Part::Points(p) => v3(p.positions[i]),
                Part::MeshPoints(m, _) => v3(m.positions[i]),
                Part::Faces(m, _) => v3(face(m, i).0),
                Part::Instances(n) => v3(decompose(&n.worlds[i]).0),
                Part::Curves(c) => v3(c.positions[i]),
            },
            Source::Normal => match part {
                Part::MeshPoints(m, _) => m.normals.get(i).map_or(Cell::Empty, |n| v3(*n)),
                Part::Faces(m, _) => v3(face(m, i).1),
                _ => Cell::Empty,
            },
            Source::Uv => match part {
                Part::MeshPoints(m, _) => m.uvs.get(i).and_then(|uv| uv.get(comp)).map_or(Cell::Empty, |x| Cell::Float(*x)),
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
            Source::Attr => attr_cell(part.attrs().get(&f.name), i, comp),
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
    use crate::scene::Model;
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
}
