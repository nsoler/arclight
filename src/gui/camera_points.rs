//! Editing a Camera Tracker's solved points from the viewer and its
//! panel: Delete points (left out, and the camera solved again from the
//! remaining kept tracks, on a worker thread), Re-solve (again from the
//! kept tracks with the current settings) and Select worst (every point
//! over an error). See [`arclight::camtrack_tracks`].
//!
//! A finished solve lands in one UI frame: the solution, the tracks left
//! out, and the Point Clouds and Fit Planes made from it, so ⌘Z undoes it
//! as one step.

use crate::doc::{Doc, NODE_W};
use arclight::camtrack::{Settings, Solution};
use arclight::camtrack_tracks::{self as ct, Resolved};
use arclight::ops;
use eframe::egui;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::JoinHandle;

/// A solve running for a tracker.
struct Job {
    handle: JoinHandle<anyhow::Result<Resolved>>,
    excluded: Vec<usize>,
    before: Solution,
    /// Points deleted (None: a Re-solve).
    deleted: Option<usize>,
}

/// Running solves and the last finished one's message, by tracker.
static JOBS: Mutex<Option<HashMap<String, Job>>> = Mutex::new(None);
static MESSAGES: Mutex<Option<HashMap<String, (bool, String)>>> = Mutex::new(None);

fn say(id: &str, ok: bool, msg: String) {
    if let Ok(mut m) = MESSAGES.lock() {
        m.get_or_insert_with(HashMap::new).insert(id.to_string(), (ok, msg));
    }
}

/// The last message about tracker `id`'s points.
pub fn message(id: &str) -> Option<(bool, String)> {
    MESSAGES.lock().ok()?.as_ref()?.get(id).cloned()
}

/// Whether tracker `id` is being solved again.
pub fn busy(id: &str) -> bool {
    JOBS.lock().ok().and_then(|j| j.as_ref().map(|j| j.contains_key(id))).unwrap_or(false)
}

/// Why tracker `id`'s points can't be deleted or solved again, if they
/// can't.
pub fn unavailable(doc: &Doc, id: &str, sol: &Solution) -> Option<&'static str> {
    let n = doc.node(id)?;
    ct::can_delete(&n.params, sol).err()
}

/// Start solving tracker `id` (solve `sol`) again without the tracks in
/// `excluded`; `deleted` points were just deleted (None: a Re-solve).
fn start(doc: &Doc, id: &str, sol: &Solution, excluded: Vec<usize>, deleted: Option<usize>) -> Result<(), String> {
    if busy(id) {
        return Err("Already solving".into());
    }
    let node = doc.node(id).ok_or("no tracker")?;
    if let Err(why) = ct::can_delete(&node.params, sol) {
        return Err(why.into());
    }
    let p = ops::params_with_defaults("camera_tracker", &node.params);
    let settings = Settings::from_params(&p, doc.graph.project.first_frame, doc.graph.project.last_frame);
    let packed = node.params[ct::TRACKS].as_str().unwrap_or_default().to_string();
    let (before, ex) = (sol.clone(), excluded.clone());
    let handle = std::thread::spawn(move || {
        let tracks = ct::decode(&packed)?;
        ct::resolve(&tracks, &ex, &settings, &before, &mut |_| true)
    });
    if let Ok(mut j) = JOBS.lock() {
        j.get_or_insert_with(HashMap::new).insert(id.to_string(), Job { handle, excluded, before: sol.clone(), deleted });
    }
    say(id, true, if deleted.is_some() { "Deleting points and solving again…".into() } else { "Solving again…".into() });
    Ok(())
}

/// Delete points `ids` of tracker `id`'s solve `sol`: leave out their
/// tracks and solve again (on a worker; [`poll`] lands it).
pub fn delete(doc: &Doc, id: &str, sol: &Solution, ids: &[usize]) {
    if ids.is_empty() {
        return;
    }
    let excluded = ct::exclude(&doc.node(id).map(|n| ct::excluded(&n.params)).unwrap_or_default(), sol, ids);
    if let Err(e) = start(doc, id, sol, excluded, Some(ids.len())) {
        say(id, false, e);
    }
}

/// Solve tracker `id` again from its kept tracks with the current
/// settings, still without the deleted points.
pub fn resolve_again(doc: &Doc, id: &str, sol: &Solution) {
    let excluded = doc.node(id).map(|n| ct::excluded(&n.params)).unwrap_or_default();
    if let Err(e) = start(doc, id, sol, excluded, None) {
        say(id, false, e);
    }
}

/// Land tracker `id`'s finished solve, if there is one: the solution, the
/// tracks left out, and the clouds and Fit Planes made from it. Asks for a
/// repaint while it runs. True when it landed.
pub fn poll(ctx: &egui::Context, doc: &mut Doc, id: &str) -> bool {
    let job = {
        let Ok(mut jobs) = JOBS.lock() else { return false };
        let Some(jobs) = jobs.as_mut() else { return false };
        match jobs.get(id) {
            Some(j) if j.handle.is_finished() => jobs.remove(id),
            Some(_) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return false;
            }
            None => return false,
        }
    };
    let Some(job) = job else { return false };
    let result = match job.handle.join() {
        Ok(r) => r,
        Err(p) => Err(anyhow::anyhow!(arclight::crash::caught(&*p))),
    };
    match result {
        Ok(r) => {
            let note = land(doc, id, &job.before, &r.solution, &job.excluded);
            let msg = match job.deleted {
                Some(n) => ct::deleted_message(n, &job.before, &r),
                None => ct::resolved_message(&job.before, &r),
            };
            say(id, true, msg + &note);
            true
        }
        Err(e) => {
            say(id, false, format!("{e:#}"));
            false
        }
    }
}

/// Put solve `after` (replacing `before`) on tracker `id` with its
/// `excluded` tracks, and bring the clouds and Fit Planes made from it
/// along. A note about any Fit Plane kept on its old points.
fn land(doc: &mut Doc, id: &str, before: &Solution, after: &Solution, excluded: &[usize]) -> String {
    let refresh = ct::refresh_clouds(&doc.graph, id, before, after);
    doc.set_param(id, "solution", after.to_value());
    if excluded.is_empty() {
        doc.remove_param(id, ct::EXCLUDED);
    } else {
        doc.set_param(id, ct::EXCLUDED, json!(excluded));
    }
    for (node, k, v) in &refresh.set {
        doc.set_param(node, k, v.clone());
    }
    // A Fit Plane left with too few of its points: on a copy of its old
    // cloud, so it keeps its fit.
    for (plane, points) in &refresh.keep {
        let at = doc.node(plane).and_then(|n| n.pos).unwrap_or_default();
        let copy = doc.add_node("point_cloud", [at[0] - NODE_W - 30.0, at[1]]);
        doc.set_param(&copy, "points", points.clone());
        doc.connect(&copy, plane, 0);
    }
    refresh.note()
}

/// A full Track & Solve's result `sol` from `tracks` on tracker `id`: the
/// tracks kept and the exclusions cleared.
pub fn tracked(doc: &mut Doc, id: &str, tracks: &[arclight::features::FeatureTrack]) {
    doc.set_param(id, ct::TRACKS, json!(ct::encode(tracks)));
    doc.remove_param(id, ct::EXCLUDED);
}

/// Clear solve: the kept tracks and exclusions go too.
pub fn cleared(doc: &mut Doc, id: &str) {
    doc.remove_param(id, ct::TRACKS);
    doc.remove_param(id, ct::EXCLUDED);
}

/// The points panel's Delete points button, for points `selected` of
/// tracker `id`'s solve `sol`. True when clicked (the deletion started).
pub fn delete_button(ui: &mut egui::Ui, doc: &Doc, id: &str, sol: &Solution, selected: &[usize]) -> bool {
    let why = unavailable(doc, id, sol);
    let running = busy(id);
    let b = ui.add_enabled(!selected.is_empty() && why.is_none() && !running, egui::Button::new("Delete points"));
    let b = b.on_hover_text("Leave these points' tracks out and solve the camera again from the rest (⌫). The ground, origin and scale stay");
    let b = match why {
        Some(w) => b.on_disabled_hover_text(w),
        None => b,
    };
    if b.clicked() {
        delete(doc, id, sol, selected);
        return true;
    }
    false
}

/// The points panel's Select worst row: select every point of `sol` with
/// an error over the threshold (at first the node's Max error), else the
/// worst 5%. True when it changed the selection.
pub fn worst_row(ui: &mut egui::Ui, doc: &Doc, id: &str, sol: &Solution, selected: &mut Vec<usize>) -> Option<String> {
    let key = egui::Id::unique(("camera_worst_above", id));
    let max_error = doc.param(id, "max_error").as_f64().unwrap_or(1.0);
    let mut above = ui.data_mut(|d| *d.get_temp_mut_or(key, max_error));
    let known = sol.point_rms.len() == sol.points.len() && !sol.points.is_empty();
    let mut out = None;
    ui.horizontal(|ui| {
        let b = ui.add_enabled(known, egui::Button::new("Select worst"));
        let b = b.on_hover_text("Select every point whose error is over this many pixels (if none is, the worst 5%), to look over and delete").on_disabled_hover_text("This solve has no per-point errors: Track & Solve again");
        if ui.add_enabled(known, egui::DragValue::new(&mut above).speed(0.01).range(0.01..=100.0).prefix("over ").suffix(" px")).changed() {
            ui.data_mut(|d| d.insert_temp(key, above));
        }
        if b.clicked() {
            let (ids, fell_back) = ct::worst(sol, above);
            out = Some(if fell_back {
                format!("No point is over {above:.2} px: selected the worst {} (5%).", ids.len())
            } else {
                format!("Selected {} point(s) over {above:.2} px.", ids.len())
            });
            *selected = ids;
        }
    });
    out
}

/// The panel's Re-solve button.
pub fn resolve_button(ui: &mut egui::Ui, doc: &Doc, id: &str, sol: &Solution) {
    let why = unavailable(doc, id, sol);
    let b = ui.add_enabled(why.is_none() && !busy(id), egui::Button::new("Re-solve"));
    let b = b.on_hover_text("Solve the camera again from the kept tracks with the current settings, without tracking again; deleted points stay out, the ground, origin and scale stay");
    let b = match why {
        Some(w) => b.on_disabled_hover_text(w),
        None => b,
    };
    if b.clicked() {
        resolve_again(doc, id, sol);
    }
}

/// A solve's message, coloured.
pub fn show_message(ui: &mut egui::Ui, id: &str) {
    if let Some((ok, msg)) = message(id) {
        let color = if ok { egui::Color32::from_rgb(110, 200, 120) } else { egui::Color32::from_rgb(235, 100, 90) };
        if busy(id) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.colored_label(color, msg);
            });
        } else {
            ui.colored_label(color, msg);
        }
    }
}

/// Wait for tracker `id`'s solve (tests).
#[cfg(test)]
pub fn finish(ctx: &egui::Context, doc: &mut Doc, id: &str) -> bool {
    for _ in 0..3000 {
        if !busy(id) {
            return false;
        }
        if poll(ctx, doc, id) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the solve didn't finish");
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use arclight::features::FeatureTrack;
    use arclight::graph::Graph;
    use arclight::image::Size;
    use arclight::solve::{Intrinsics, Pose, V3};
    use nalgebra::Rotation3;

    /// A Camera Tracker on frames 0–29 solved from tracks it keeps: a
    /// camera sliding sideways over 150 points, track 7 wobbling ±1.6 px.
    /// The doc, the tracker (named `name`: solves run by name, so each
    /// test its own) selected.
    pub(crate) fn tracked_doc(name: &str) -> (Doc, String) {
        let format = Size { width: 960, height: 540 };
        let k = Intrinsics::new(30.0, 36.0, format);
        let mut seed = 99u64;
        let mut u = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let pts: Vec<V3> = (0..150).map(|_| V3::new(u() * 8.0 - 4.0, u() * 5.0 - 2.5, 4.0 + u() * 8.0)).collect();
        let poses: Vec<Pose> = (0..30).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.0, -0.004 * f as f64, 0.0), V3::new(0.1 * f as f64, 0.0, 0.0))).collect();
        let mut tracks: Vec<FeatureTrack> = pts
            .iter()
            .map(|x| FeatureTrack {
                start: 0,
                points: poses.iter().map_while(|p| k.project(&p.apply(x)).filter(|q| q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= 960.0 && q[1] <= 540.0)).map(|q| [(q[0] + 0.2 * (u() - 0.5)) as f32, (q[1] + 0.2 * (u() - 0.5)) as f32]).collect(),
            })
            .collect();
        for (f, p) in tracks[7].points.iter_mut().enumerate() {
            p[0] += if f % 2 == 0 { 1.6 } else { -1.6 };
        }
        let node = arclight::graph::NodeSpec { id: name.into(), op: "camera_tracker".into(), inputs: vec![String::new(); 2], params: json!({}), pos: Some([0.0, 0.0]), disabled: false };
        let mut doc = Doc::new(Graph { nodes: vec![node], ..Default::default() }, None);
        doc.graph.project.first_frame = 0;
        doc.graph.project.last_frame = 29;
        let id = name.to_string();
        doc.set_param(&id, "focal", json!(30.0));
        let p = ops::params_with_defaults("camera_tracker", &doc.node(&id).unwrap().params);
        let s = Settings::from_params(&p, 0, 29);
        let sol = arclight::camtrack::resolve(&tracks, &[], &s, format, &mut |_| true).unwrap();
        doc.set_param(&id, "solution", sol.to_value());
        tracked(&mut doc, &id, &tracks);
        doc.selected = Some(id.clone());
        doc.commit(false);
        (doc, id)
    }

    fn solution(doc: &Doc, id: &str) -> Solution {
        Solution::from_params(&doc.node(id).unwrap().params).unwrap()
    }

    /// Deleting the worst point solves again without its track: a lower
    /// error, the exclusion kept on the node and through a save and load,
    /// the message, a Fit Plane on the exported cloud still on its points;
    /// ⌘Z takes it all back in one step. A full Track & Solve's result
    /// clears the exclusion; an old solve can't delete.
    #[test]
    fn deleting_points_solves_again_as_one_undo_step() {
        let ctx = egui::Context::default();
        let (mut doc, id) = tracked_doc("ct_delete_test");
        let before = solution(&doc, &id);
        let (worst, _) = ct::worst(&before, 1.0);
        assert_eq!(worst.iter().map(|&i| before.point_tracks[i]).collect::<Vec<_>>(), vec![7], "the wobbling track is the worst");
        // A cloud and a Fit Plane on three points, made before.
        let cloud = doc.add_node("point_cloud", [0.0, 100.0]);
        for (k, v) in ct::cloud_params(&id, &before) {
            doc.set_param(&cloud, &k, v);
        }
        let plane = doc.add_node("fit_plane", [0.0, 200.0]);
        doc.connect(&cloud, &plane, 0);
        let on: Vec<usize> = vec![10, 20, 30, 40];
        let on_tracks: Vec<usize> = on.iter().map(|&i| before.point_tracks[i]).collect();
        assert!(!on_tracks.contains(&7));
        doc.set_param(&plane, "points", json!(arclight::geo_fit::indices_text(&on)));
        doc.commit(false);
        let saved = doc.graph.clone();

        delete(&doc, &id, &before, &worst);
        assert!(busy(&id));
        assert!(finish(&ctx, &mut doc, &id));
        doc.commit(false);
        let after = solution(&doc, &id);
        assert!(after.rms < before.rms && after.points.len() == before.points.len() - 1);
        assert!(!after.point_tracks.contains(&7));
        assert_eq!(doc.node(&id).unwrap().params[ct::EXCLUDED], json!([7]));
        let (ok, msg) = message(&id).unwrap();
        assert!(ok && msg.starts_with("Deleted 1 point: error ") && msg.contains(" → "), "{msg}");
        // The Fit Plane fits the same tracks, on the cloud brought up to date.
        let now: Vec<usize> = arclight::geo_fit::indices(doc.node(&plane).unwrap().params["points"].as_str().unwrap(), after.points.len()).unwrap();
        assert_eq!(now.iter().map(|&i| after.point_tracks[i]).collect::<Vec<_>>(), on_tracks);
        assert_eq!(doc.node(&cloud).unwrap().params["points"], ct::flat(&after));

        // Saved and loaded, the exclusion holds through a Re-solve.
        let json = serde_json::to_string(&doc.graph).unwrap();
        let mut loaded = Doc::new(serde_json::from_str(&json).unwrap(), None);
        resolve_again(&loaded, &id, &after);
        assert!(finish(&ctx, &mut loaded, &id));
        let again = solution(&loaded, &id);
        assert!(!again.point_tracks.contains(&7) && again.points.len() == after.points.len());
        assert_eq!(loaded.node(&id).unwrap().params[ct::EXCLUDED], json!([7]));

        // One undo step back to before the deletion.
        doc.undo();
        assert_eq!(doc.graph, saved);

        // A full Track & Solve clears it.
        tracked(&mut loaded, &id, &[]);
        assert!(loaded.node(&id).unwrap().params.get(ct::EXCLUDED).is_none());
        // A solve without kept tracks can't delete, and says why.
        let mut old = Doc::new(Graph::default(), None);
        let ot = old.add_node("camera_tracker", [0.0, 0.0]);
        assert_ne!(ot, id);
        old.set_param(&ot, "solution", Solution { point_tracks: Vec::new(), ..before.clone() }.to_value());
        assert!(unavailable(&old, &ot, &solution(&old, &ot)).is_some_and(|w| w.contains("Track & Solve again")));
        delete(&old, &ot, &solution(&old, &ot), &[0]);
        assert!(!busy(&ot) && message(&ot).is_some_and(|(ok, _)| !ok));
    }
}
