//! Pieces of a graph standing on their own, for copy / paste and
//! templates (docs: TODO.md, "Import project, copy / paste, templates"):
//! an [`excerpt`] of some nodes and backdrops as a project of their own
//! (the text the clipboard carries, a template's file), [`parse`] to tell
//! such text from anything else, and the templates kept in
//! [`config::templates_dir`](crate::config::templates_dir) (and the
//! examples shipped with the app, listed as built-in ones). Putting one
//! into a project is `graph_merge`'s.

use crate::graph::{Graph, NodeSpec};
use crate::ops;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Nodes `nodes` and backdrops `backdrops` of `graph` (saved at `file`, if
/// it is) as a project of their own: the nodes inside the backdrops too,
/// and any other backdrop all of whose nodes are taken; inputs from nodes
/// left out unconnected; every file path whole (so another project, or
/// another running app, finds the files); the project's format, rate and
/// frames, but not its folder or view state.
pub fn excerpt(graph: &Graph, file: Option<&Path>, nodes: &[String], backdrops: &[String]) -> Graph {
    let inside = |b: &crate::graph::Backdrop| -> Vec<&NodeSpec> {
        graph.nodes.iter().filter(|n| {
            let (a, z) = n.bounds();
            b.contains(a, z)
        }).collect()
    };
    let mut take: Vec<&str> = nodes.iter().map(String::as_str).collect();
    for b in graph.backdrops.iter().filter(|b| backdrops.contains(&b.id)) {
        take.extend(inside(b).into_iter().map(|n| n.id.as_str()));
    }
    let taken = |id: &str| take.contains(&id);
    let mut out = Graph { project: crate::graph::Project { folder: None, ..graph.project.clone() }, ..Graph::default() };
    out.nodes = graph.nodes.iter().filter(|n| taken(&n.id)).cloned().collect();
    for n in &mut out.nodes {
        for i in &mut n.inputs {
            if !taken(i) {
                i.clear();
            }
        }
        whole_paths(n, &graph.project, file);
    }
    out.backdrops = graph
        .backdrops
        .iter()
        .filter(|b| backdrops.contains(&b.id) || {
            let members = inside(b);
            !members.is_empty() && members.iter().all(|n| taken(&n.id))
        })
        .cloned()
        .collect();
    out
}

/// `n`'s file params made whole: given relative to `project`'s folder (the
/// project saved at `file`), or failing that, to the working folder.
fn whole_paths(n: &mut NodeSpec, project: &crate::graph::Project, file: Option<&Path>) {
    let Some(info) = ops::info(&n.op) else { return };
    for spec in info.params.iter().filter(|s| matches!(s.kind, ops::ParamKind::Path { .. })) {
        if let Some(Value::String(p)) = n.params.get_mut(spec.name) {
            if !p.is_empty() {
                let resolved = project.resolve_path(file, p);
                *p = std::path::absolute(&resolved).map_or(resolved, |a| a.to_string_lossy().into_owned());
            }
        }
    }
}

/// The project in `text` if it's one of ours with something in it (as
/// [`excerpt`] makes); None for anything else (text copied from
/// elsewhere), quietly.
pub fn parse(text: &str) -> Option<Graph> {
    let text = text.trim();
    if !text.starts_with('{') {
        return None;
    }
    let g = Graph::from_str(text).ok()?;
    let mut ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    ids.sort_unstable();
    let unique = ids.windows(2).all(|w| w[0] != w[1]);
    ((!g.nodes.is_empty() || !g.backdrops.is_empty()) && unique && ids.iter().all(|id| !id.is_empty())).then_some(g)
}

/// A template: a project to insert, or start a new one from.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub name: String,
    pub path: PathBuf,
    /// One of the examples shipped with the app (not renamed or deleted).
    pub builtin: bool,
}

impl Template {
    /// Its project, file paths whole (those of a project without a
    /// folder too: see [`graph_merge::read`](crate::graph_merge::read)).
    pub fn load(&self) -> Result<Graph> {
        crate::graph_merge::read(&self.path)
    }
}

/// The templates in `dir` (each `<name>.json`), by name, case aside.
pub fn list_in(dir: &Path) -> Vec<Template> {
    let mut out: Vec<Template> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .filter_map(|path| Some(Template { name: path.file_stem()?.to_str()?.to_string(), path, builtin: false }))
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// The user's templates (in [`config::templates_dir`](crate::config::templates_dir)).
pub fn list() -> Vec<Template> {
    list_in(&crate::config::templates_dir())
}

/// The examples shipped with the app (label, file) as built-in templates,
/// those that are there.
pub fn builtin(examples: &[(&str, PathBuf)]) -> Vec<Template> {
    examples.iter().filter(|(_, p)| p.is_file()).map(|(name, path)| Template { name: name.to_string(), path: path.clone(), builtin: true }).collect()
}

/// `name` as a template's file name (without .json): trimmed; not empty,
/// not hidden, nothing that would put it in another folder.
fn file_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("give the template a name");
    }
    if name.starts_with('.') || name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        bail!("a template's name can't start with . or have any of / \\ : * ? \" < > | in it");
    }
    if name.chars().count() > 100 {
        bail!("a template's name is at most 100 characters");
    }
    Ok(name.to_string())
}

/// Where template `name` is (or would be) kept in `dir`.
pub fn path_in(dir: &Path, name: &str) -> Result<PathBuf> {
    Ok(dir.join(format!("{}.json", file_name(name)?)))
}

/// Save `graph` as template `name` in `dir` (made if need be), replacing
/// one of that name only with `replace`.
pub fn save_in(dir: &Path, name: &str, graph: &Graph, replace: bool) -> Result<Template> {
    let path = path_in(dir, name)?;
    if path.exists() && !replace {
        bail!("there's a template called {} already", name.trim());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    graph.save(&path)?;
    Ok(Template { name: name.trim().to_string(), path, builtin: false })
}

/// Template `t` renamed `name` (in the same folder); not over another.
pub fn rename(t: &Template, name: &str) -> Result<Template> {
    if t.builtin {
        bail!("{} comes with Arclight: it can't be renamed", t.name);
    }
    let dir = t.path.parent().unwrap_or(Path::new("."));
    let path = path_in(dir, name)?;
    // (Only its case changed: the same file on a case-blind disk.)
    if path.exists() && !name.trim().eq_ignore_ascii_case(&t.name) {
        bail!("there's a template called {} already", name.trim());
    }
    std::fs::rename(&t.path, &path).with_context(|| format!("renaming {}", t.path.display()))?;
    Ok(Template { name: name.trim().to_string(), path, builtin: false })
}

/// Delete template `t`'s file.
pub fn delete(t: &Template) -> Result<()> {
    if t.builtin {
        bail!("{} comes with Arclight: it can't be deleted", t.name);
    }
    std::fs::remove_file(&t.path).with_context(|| format!("deleting {}", t.path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Backdrop;
    use serde_json::json;

    fn graph() -> Graph {
        Graph::from_str(
            &json!({
                "project": { "first_frame": 1, "last_frame": 48, "fps": 25.0, "format": { "width": 640, "height": 360 }, "folder": "/shots/a" },
                "nodes": [
                    { "id": "read", "op": "read", "params": { "path": "plates/a.####.exr" }, "pos": [0, 0] },
                    { "id": "grade", "op": "grade", "inputs": ["read"], "pos": [0, 100] },
                    { "id": "blur", "op": "blur", "inputs": ["grade"], "pos": [0, 200] },
                    { "id": "out", "op": "write", "inputs": ["blur"], "params": { "path": "/renders/out.####.png" }, "pos": [0, 300] }
                ],
                "backdrops": [
                    { "id": "bd1", "title": "look", "pos": [-20, 60], "size": [200, 220] },
                    { "id": "bd2", "title": "all", "pos": [-40, -40], "size": [240, 420] }
                ]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn ids(g: &Graph) -> Vec<&str> {
        g.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    /// An excerpt: the nodes asked for, inputs from outside cut, paths
    /// whole, the backdrop all of whose nodes are in it; a backdrop asked
    /// for brings its nodes; the project's format but not its folder.
    #[test]
    fn an_excerpt_stands_on_its_own() {
        let g = graph();
        let e = excerpt(&g, None, &["read".into(), "grade".into()], &[]);
        assert_eq!(ids(&e), ["read", "grade"]);
        assert_eq!(e.nodes[1].inputs, ["read"]);
        assert_eq!(e.nodes[0].params["path"], "/shots/a/plates/a.####.exr");
        assert!(e.backdrops.is_empty(), "bd1 holds blur too: {:?}", e.backdrops);
        assert_eq!((e.project.format.width, e.project.fps, e.project.folder.clone()), (640, 25.0, None));
        let e = excerpt(&g, None, &["blur".into()], &[]);
        assert_eq!(e.nodes[0].inputs, [""], "grade wasn't taken");
        // The backdrop "look" brings grade and blur.
        let e = excerpt(&g, None, &[], &["bd1".into()]);
        assert_eq!(ids(&e), ["grade", "blur"]);
        assert_eq!(e.backdrops.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["bd1"]);
        assert_eq!(e.nodes[1].inputs, ["grade"]);
        // Everything: both backdrops come along.
        let all: Vec<String> = g.nodes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(excerpt(&g, None, &all, &[]).backdrops.len(), 2);
        // An empty backdrop isn't "all of its nodes taken".
        let mut lone = g.clone();
        lone.backdrops.push(Backdrop { id: "empty".into(), pos: [900.0, 900.0], size: [100.0, 100.0], ..g.backdrops[0].clone() });
        assert!(excerpt(&lone, None, &all, &[]).backdrops.iter().all(|b| b.id != "empty"));
        // Without a folder, a relative path is made whole from the
        // working folder.
        let mut loose = g.clone();
        loose.project.folder = None;
        let e = excerpt(&loose, None, &["read".into()], &[]);
        assert_eq!(e.nodes[0].params["path"], std::path::absolute("plates/a.####.exr").unwrap().to_string_lossy().as_ref());
    }

    /// Ours, read back; anything else (plain text, other JSON, nothing in
    /// it, the same id twice), None.
    #[test]
    fn only_our_text_parses() {
        let e = excerpt(&graph(), None, &["grade".into(), "blur".into()], &[]);
        assert_eq!(parse(&e.to_json()), Some(e.clone()));
        assert_eq!(parse(&format!("\n  {}\n", e.to_json())), Some(e));
        for text in ["", "grade", "[1, 2]", "{\"a\": 1}", "{\"nodes\": []}", "{\"nodes\": [{\"id\": \"a\", \"op\": \"blur\"}, {\"id\": \"a\", \"op\": \"blur\"}]}", "{\"nodes\": [{\"id\": \"\", \"op\": \"blur\"}]}"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    /// Saved, listed by name, loaded, renamed, deleted; names that would
    /// escape the folder or clash refused; built-ins neither renamed nor
    /// deleted.
    #[test]
    fn templates_are_kept_in_their_folder() {
        let dir = std::env::temp_dir().join(format!("arclight-templates-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(list_in(&dir).is_empty(), "no folder yet: none");
        let e = excerpt(&graph(), None, &["read".into(), "grade".into()], &[]);
        let b = save_in(&dir, " Plate grade ", &e, false).unwrap();
        assert_eq!(b.name, "Plate grade");
        save_in(&dir, "another", &e, false).unwrap();
        std::fs::write(dir.join("notes.txt"), "not one").unwrap();
        assert_eq!(list_in(&dir).iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["another", "Plate grade"]);
        assert_eq!(b.load().unwrap().nodes, e.nodes, "paths whole as saved");
        assert!(save_in(&dir, "another", &e, false).is_err(), "taken");
        assert!(save_in(&dir, "another", &e, true).is_ok(), "replaced when asked");
        for bad in ["", "  ", "../up", "a/b", ".hidden", "c:d"] {
            assert!(save_in(&dir, bad, &e, false).is_err(), "{bad:?}");
        }
        assert!(rename(&b, "another").is_err(), "not over another");
        let r = rename(&b, "Grade").unwrap();
        assert!(r.path.is_file() && !b.path.exists());
        assert!(rename(&r, "grade").is_ok(), "its case alone");
        delete(&list_in(&dir)[0]).unwrap();
        assert_eq!(list_in(&dir).len(), 1);
        let shipped = builtin(&[("Missing", dir.join("nope.json")), ("Here", list_in(&dir)[0].path.clone())]);
        assert_eq!(shipped.iter().map(|t| (t.name.as_str(), t.builtin)).collect::<Vec<_>>(), [("Here", true)]);
        assert!(rename(&shipped[0], "x").is_err() && delete(&shipped[0]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
