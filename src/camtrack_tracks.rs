//! A Camera Tracker's kept feature tracks, and deleting solved points.
//!
//! Track & Solve keeps the 2D feature tracks it solved from on the node
//! (its `tracks` param, packed by [`encode`]) so the camera can be solved
//! again without tracking the shot again: [`resolve`] solves from them,
//! leaving out the tracks in `excluded_tracks` (the points the user
//! deleted). A full Track & Solve writes new tracks and clears the
//! exclusions.
//!
//! Each solved point knows its track ([`Solution::point_tracks`]), which is
//! what deletion, re-solving and the Point Clouds made from a solve go by:
//!
//! - A re-solve is fitted onto the solve before it (a similarity over the
//!   points both share, the reliable ones only, see [`align_to`]), so the
//!   ground, origin and scale the user set stay where they were, and a
//!   Camera linked to the solve doesn't jump.
//! - A Point Cloud made from the solve (Export, a placed Fit Plane) is
//!   tagged with the tracker and its points' tracks; [`refresh_clouds`]
//!   updates it to the new solve, and the Fit Planes on it keep fitting the
//!   same physical points (their numbers remapped through the tracks).

use crate::camtrack::{self, Settings, Solution};
use crate::features::FeatureTrack;
use crate::geom::Vec3;
use crate::graph::Graph;
use crate::image::Size;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

/// The Camera Tracker params holding its tracks and its left-out tracks.
pub const TRACKS: &str = "tracks";
pub const EXCLUDED: &str = "excluded_tracks";
/// The Point Cloud params naming the tracker it was made from and each of
/// its points' tracks.
pub const FROM_TRACKER: &str = "from_tracker";
pub const POINT_TRACKS: &str = "point_tracks";

const MAGIC: &str = "ft1:";
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= c.len() { B64[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    let mut val = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        val[c as usize] = i as u8;
    }
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chunks(4) {
        let mut n = 0u32;
        for (k, &b) in c.iter().enumerate() {
            let v = val[b as usize];
            if v == 255 {
                bail!("broken tracks");
            }
            n |= (v as u32) << (18 - 6 * k);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    Ok(out)
}

/// `tracks` packed into one string: "ft1:" and base64 of, little-endian,
/// the track count, then each track's start frame (i64), length (u32) and
/// x, y (f32) per frame. (One string, so the undo history copies it in
/// one piece.)
pub fn encode(tracks: &[FeatureTrack]) -> String {
    let mut b = Vec::with_capacity(4 + tracks.iter().map(|t| 12 + 8 * t.points.len()).sum::<usize>());
    b.extend_from_slice(&(tracks.len() as u32).to_le_bytes());
    for t in tracks {
        b.extend_from_slice(&t.start.to_le_bytes());
        b.extend_from_slice(&(t.points.len() as u32).to_le_bytes());
        for p in &t.points {
            b.extend_from_slice(&p[0].to_le_bytes());
            b.extend_from_slice(&p[1].to_le_bytes());
        }
    }
    format!("{MAGIC}{}", b64_encode(&b))
}

/// Tracks from [`encode`]'s string.
pub fn decode(s: &str) -> Result<Vec<FeatureTrack>> {
    let b = b64_decode(s.strip_prefix(MAGIC).context("not a tracker's tracks")?)?;
    let mut at = 0;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = b.get(at..at + n).context("broken tracks")?;
        at += n;
        Ok(s)
    };
    let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
    let count = u32_(take(4)?) as usize;
    let mut tracks = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let start = i64::from_le_bytes(take(8)?.try_into().unwrap());
        let len = u32_(take(4)?) as usize;
        let raw = take(8 * len)?;
        let points = raw.chunks(8).map(|c| [f32::from_le_bytes(c[..4].try_into().unwrap()), f32::from_le_bytes(c[4..].try_into().unwrap())]).collect();
        tracks.push(FeatureTrack { start, points });
    }
    Ok(tracks)
}

/// Whether Camera Tracker params `p` keep tracks to solve again from.
pub fn has_tracks(p: &Value) -> bool {
    p[TRACKS].as_str().is_some_and(|s| s.starts_with(MAGIC))
}

/// Camera Tracker params `p`'s tracks, if it keeps them.
pub fn tracks(p: &Value) -> Option<Result<Vec<FeatureTrack>>> {
    p[TRACKS].as_str().map(decode)
}

/// The tracks Camera Tracker params `p` leaves out of its solves.
pub fn excluded(p: &Value) -> Vec<usize> {
    let mut out: Vec<usize> = p[EXCLUDED].as_array().map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()).unwrap_or_default();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether points of the solve in params `p` can be deleted (it keeps its
/// tracks and knows each point's), else why not.
pub fn can_delete(p: &Value, sol: &Solution) -> Result<(), &'static str> {
    if !has_tracks(p) || sol.point_tracks.len() != sol.points.len() || sol.points.is_empty() {
        return Err("Track & Solve again to delete points (this solve was made before its tracks were kept)");
    }
    Ok(())
}

/// `excluded` with the tracks of points `ids` of `sol` added (sorted).
pub fn exclude(excluded: &[usize], sol: &Solution, ids: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = excluded.iter().copied().chain(ids.iter().filter_map(|&i| sol.point_tracks.get(i).copied())).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The points to select as the worst of `sol`: every one with an error
/// above `above` pixels, or, if none is, the worst 5% (at least one). The
/// bool: whether it fell back to the 5%. Nothing without per-point errors.
pub fn worst(sol: &Solution, above: f64) -> (Vec<usize>, bool) {
    if sol.point_rms.len() != sol.points.len() || sol.points.is_empty() {
        return (Vec::new(), false);
    }
    let over: Vec<usize> = (0..sol.points.len()).filter(|&i| sol.point_rms[i] > above).collect();
    if !over.is_empty() {
        return (over, false);
    }
    let mut order: Vec<usize> = (0..sol.points.len()).collect();
    order.sort_by(|&a, &b| sol.point_rms[b].total_cmp(&sol.point_rms[a]));
    order.truncate((sol.points.len() as f64 * 0.05).ceil().max(1.0) as usize);
    order.sort_unstable();
    (order, true)
}

/// A re-solve: the solve, and whether it was fitted onto the previous one
/// (not when they shared too few reliable points).
pub struct Resolved {
    pub solution: Solution,
    pub aligned: bool,
}

/// Solve again from `tracks` without the `excluded` ones, as `s` says,
/// fitted onto `prev` (the solve it replaces) so its ground, origin and
/// scale stay. `progress` as [`camtrack::run`]'s.
pub fn resolve(tracks: &[FeatureTrack], excluded: &[usize], s: &Settings, prev: &Solution, progress: &mut dyn FnMut(f32) -> bool) -> Result<Resolved> {
    let format = Size { width: prev.format[0].max(1), height: prev.format[1].max(1) };
    let mut solution = camtrack::resolve(tracks, excluded, s, format, progress)?;
    let aligned = align_to(&mut solution, prev, s.max_error);
    Ok(Resolved { solution, aligned })
}

/// The fewest shared points a re-solve is fitted onto its previous solve
/// by.
pub const MIN_SHARED: usize = 4;

/// Move, turn and scale `new` onto `old` by the points both share (by
/// track): the reliable ones (an error at or below `max_error` in both,
/// else the best 80%), then again without the fifth that fits worst, so
/// points being thrown out can't pull it. False (`new` left as it is)
/// when fewer than [`MIN_SHARED`] are shared.
pub fn align_to(new: &mut Solution, old: &Solution, max_error: f64) -> bool {
    let at: HashMap<usize, usize> = old.point_tracks.iter().enumerate().map(|(i, &t)| (t, i)).collect();
    let err = |s: &Solution, i: usize| s.point_rms.get(i).copied().unwrap_or(0.0);
    // (new index, old index, worse of the two errors).
    let mut shared: Vec<(usize, usize, f64)> =
        new.point_tracks.iter().enumerate().filter(|(j, _)| *j < new.points.len()).filter_map(|(j, t)| at.get(t).filter(|&&i| i < old.points.len()).map(|&i| (j, i, err(new, j).max(err(old, i))))).collect();
    if shared.len() < MIN_SHARED {
        return false;
    }
    let good: Vec<_> = shared.iter().copied().filter(|p| p.2 <= max_error).collect();
    if good.len() >= MIN_SHARED.max(shared.len() / 2) {
        shared = good;
    } else {
        shared.sort_by(|a, b| a.2.total_cmp(&b.2));
        shared.truncate((shared.len() * 4 / 5).max(MIN_SHARED));
    }
    let fit = |pairs: &[(usize, usize, f64)]| {
        let from: Vec<Vec3> = pairs.iter().map(|p| Vec3::from(new.points[p.0])).collect();
        let to: Vec<Vec3> = pairs.iter().map(|p| Vec3::from(old.points[p.1])).collect();
        crate::sfm::similarity(&from, &to)
    };
    let (s, r, t) = fit(&shared);
    // Trimmed: again without the worst-fitting fifth.
    let mut res: Vec<(f64, (usize, usize, f64))> = shared.iter().map(|p| (((s * (r * Vec3::from(new.points[p.0]))) + t - Vec3::from(old.points[p.1])).norm(), *p)).collect();
    res.sort_by(|a, b| a.0.total_cmp(&b.0));
    res.truncate((res.len() * 4 / 5).max(MIN_SHARED));
    let kept: Vec<_> = res.into_iter().map(|(_, p)| p).collect();
    let (s, r, t) = fit(&kept);
    if !(s.is_finite() && s > 0.0) {
        return false;
    }
    new.transform(s, &r, &t);
    true
}

/// "Deleted 4 points: error 0.84 → 0.61 px", and a note if the new solve
/// couldn't be fitted onto the old.
pub fn deleted_message(n: usize, before: &Solution, after: &Resolved) -> String {
    let mut m = format!("Deleted {n} point{}: error {:.2} → {:.2} px.", if n == 1 { "" } else { "s" }, before.rms, after.solution.rms);
    if !after.aligned {
        m += &not_aligned();
    }
    m
}

/// A re-solve's message.
pub fn resolved_message(before: &Solution, after: &Resolved) -> String {
    let mut m = format!("Solved again: {} points, error {:.2} → {:.2} px.", after.solution.points.len(), before.rms, after.solution.rms);
    if !after.aligned {
        m += &not_aligned();
    }
    m
}

fn not_aligned() -> String {
    format!(" Too few points (under {MIN_SHARED}) were shared with the previous solve to keep its ground, origin and scale: set them again.")
}

/// A Point Cloud's params made from `sol`, Camera Tracker `tracker`'s:
/// its points, tagged so [`refresh_clouds`] can find it.
pub fn cloud_params(tracker: &str, sol: &Solution) -> serde_json::Map<String, Value> {
    let mut p = serde_json::Map::new();
    p.insert("points".into(), flat(sol));
    p.insert(FROM_TRACKER.into(), json!(tracker));
    if sol.point_tracks.len() == sol.points.len() {
        p.insert(POINT_TRACKS.into(), json!(sol.point_tracks));
    }
    p
}

/// `sol`'s points as a Point Cloud takes them.
pub fn flat(sol: &Solution) -> Value {
    json!(sol.points.iter().flatten().collect::<Vec<_>>())
}

/// What [`refresh_clouds`] changes: params to set, and Fit Planes left
/// with fewer than 3 of their points, to put on a copy of their old
/// cloud (`plane`, the old cloud's points) so they keep their last fit.
#[derive(Debug, Default, PartialEq)]
pub struct Refresh {
    pub set: Vec<(String, String, Value)>,
    pub keep: Vec<(String, Value)>,
}

impl Refresh {
    /// A note on Fit Planes kept on their old points, for the message.
    pub fn note(&self) -> String {
        if self.keep.is_empty() {
            return String::new();
        }
        let names: Vec<&str> = self.keep.iter().map(|(p, _)| p.as_str()).collect();
        format!(" {} kept {} old points (fewer than 3 of them are left): pick new ones.", names.join(", "), if names.len() == 1 { "its" } else { "their" })
    }
}

/// The Point Clouds made from Camera Tracker `tracker`'s solve `before`
/// (tagged with it, or untagged and holding exactly its points), brought
/// up to `after`, and the Fit Planes on them remapped to the same tracks.
pub fn refresh_clouds(graph: &Graph, tracker: &str, before: &Solution, after: &Solution) -> Refresh {
    let mut out = Refresh::default();
    if after.point_tracks.len() != after.points.len() {
        return out;
    }
    let new_at: HashMap<usize, usize> = after.point_tracks.iter().enumerate().map(|(i, &t)| (t, i)).collect();
    let old_flat = flat(before);
    for cloud in graph.nodes.iter().filter(|n| n.op == "point_cloud") {
        let tagged = cloud.params[FROM_TRACKER].as_str() == Some(tracker);
        let old_tracks: Vec<usize> = if tagged {
            match cloud.params[POINT_TRACKS].as_array() {
                Some(a) => a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect(),
                None => continue,
            }
        } else if cloud.params[FROM_TRACKER].is_null() && cloud.params["points"] == old_flat && before.point_tracks.len() == before.points.len() {
            before.point_tracks.clone()
        } else {
            continue;
        };
        for (k, v) in cloud_params(tracker, after) {
            out.set.push((cloud.id.clone(), k, v));
        }
        for plane in graph.nodes.iter().filter(|n| n.op == "fit_plane" && n.inputs.first() == Some(&cloud.id)) {
            let text = plane.params["points"].as_str().unwrap_or("");
            let Ok(old) = crate::geo_fit::indices(text, old_tracks.len()) else { continue };
            if old.is_empty() {
                continue;
            }
            let new: Vec<usize> = old.iter().filter_map(|&i| new_at.get(&old_tracks[i]).copied()).collect();
            if new.len() >= 3 {
                out.set.push((plane.id.clone(), "points".into(), json!(crate::geo_fit::indices_text(&new))));
            } else {
                out.keep.push((plane.id.clone(), cloud.params["points"].clone()));
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::solve::tests::{cloud, synthetic_tracks, FMT};
    use crate::solve::{Intrinsics, Pose};
    use nalgebra::Rotation3;

    #[test]
    fn tracks_pack_and_unpack() {
        let tracks = [
            FeatureTrack { start: -3, points: vec![[1.5, 2.25], [3.0, -4.0]] },
            FeatureTrack { start: 7, points: vec![] },
            FeatureTrack { start: 1_000_000, points: (0..5).map(|i| [i as f32 * 0.1, 960.0 - i as f32]).collect() },
        ];
        for n in 0..=tracks.len() {
            assert_eq!(decode(&encode(&tracks[..n])).unwrap(), tracks[..n]);
        }
        assert!(decode("ft1:AAAA").is_err() && decode("nope").is_err() && decode("ft1:!!").is_err());
        for len in 0..10 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&bytes)).unwrap(), bytes);
        }
    }

    /// A camera sliding sideways over a cloud of points, tracked with a
    /// little noise, plus `bad` tracks that wobble ±1.6 px frame to frame:
    /// the tracks, and the bad ones' numbers.
    pub(crate) fn shot_with_outliers(bad: usize) -> (Intrinsics, Vec<FeatureTrack>, Vec<usize>) {
        let k = Intrinsics::new(30.0, 36.0, FMT);
        let slide: Vec<Pose> = (0..30).map(|f| Pose::from_center(Rotation3::from_euler_angles(0.0, -0.004 * f as f64, 0.0), crate::solve::V3::new(0.1 * f as f64, 0.0, 0.0))).collect();
        let mut tracks = synthetic_tracks(&k, &slide, &cloud(300, 4), 0.2);
        let long: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].points.len() >= 25).collect();
        let which: Vec<usize> = long.iter().step_by(long.len() / bad.max(1)).take(bad).copied().collect();
        for &i in &which {
            for (f, p) in tracks[i].points.iter_mut().enumerate() {
                p[0] += if f % 2 == 0 { 1.6 } else { -1.6 };
                p[1] += if f % 4 < 2 { 1.0 } else { -1.0 };
            }
        }
        (k, tracks, which)
    }

    fn settings() -> Settings {
        Settings { focal: 30.0, ..Settings::from_params(&json!({}), 0, 29) }
    }

    /// Each point's error picks out the wobbling track; deleting it and
    /// solving again lowers the error, and the points, their errors and
    /// their tracks stay in step.
    #[test]
    fn the_worst_point_is_found_and_deleting_it_lowers_the_error() {
        let (_, tracks, bad) = shot_with_outliers(1);
        let s = settings();
        let sol = camtrack::resolve(&tracks, &[], &s, FMT, &mut |_| true).unwrap();
        assert!(sol.points.len() == sol.point_rms.len() && sol.points.len() == sol.point_tracks.len());
        let worst_i = (0..sol.points.len()).max_by(|&a, &b| sol.point_rms[a].total_cmp(&sol.point_rms[b])).unwrap();
        assert_eq!(sol.point_tracks[worst_i], bad[0], "the wobbling track has the worst error");
        let mut rest = sol.point_rms.clone();
        rest.sort_by(f64::total_cmp);
        assert!(sol.point_rms[worst_i] > 3.0 * rest[rest.len() / 2], "{} vs a median of {}", sol.point_rms[worst_i], rest[rest.len() / 2]);
        let (picked, fell_back) = worst(&sol, 1.0);
        assert!(!fell_back && picked == vec![worst_i], "{picked:?}");
        let (top, fell_back) = worst(&sol, 100.0);
        assert!(fell_back && top.contains(&worst_i) && top.len() == (sol.points.len() as f64 * 0.05).ceil() as usize);

        let excluded = exclude(&[], &sol, &[worst_i]);
        assert_eq!(excluded, vec![bad[0]]);
        let after = resolve(&tracks, &excluded, &s, &sol, &mut |_| true).unwrap();
        let new = &after.solution;
        assert!(after.aligned);
        assert!(new.rms < sol.rms, "error {} → {}", sol.rms, new.rms);
        assert!(!new.point_tracks.contains(&bad[0]));
        assert!(new.points.len() == new.point_rms.len() && new.points.len() == new.point_tracks.len());
        let msg = deleted_message(1, &sol, &after);
        assert!(msg.starts_with("Deleted 1 point: error ") && msg.contains(" → "), "{msg}");
    }

    /// The user's ground, origin and scale survive deleting a point and
    /// solving again.
    #[test]
    fn a_resolve_keeps_the_ground_origin_and_scale() {
        let (_, tracks, bad) = shot_with_outliers(3);
        let s = settings();
        let mut sol = camtrack::resolve(&tracks, &[], &s, FMT, &mut |_| true).unwrap();
        let by_track = |sol: &Solution, t: usize| sol.point_tracks.iter().position(|&x| x == t).unwrap();
        let good: Vec<usize> = sol.point_tracks.iter().copied().filter(|t| !bad.contains(t)).collect();
        let (ground, origin, (a, b)) = (vec![good[0], good[10], good[20], good[30], good[40]], good[5], (good[50], good[60]));
        sol.set_ground(&ground.iter().map(|&t| by_track(&sol, t)).collect::<Vec<_>>()).unwrap();
        sol.set_origin(by_track(&sol, origin)).unwrap();
        sol.set_scale(by_track(&sol, a), by_track(&sol, b), 5.0).unwrap();
        let ids: Vec<usize> = bad.iter().map(|&t| by_track(&sol, t)).collect();
        let after = resolve(&tracks, &exclude(&[], &sol, &ids), &s, &sol, &mut |_| true).unwrap();
        assert!(after.aligned);
        let new = &after.solution;
        let at = |t: usize| Vec3::from(new.points[by_track(new, t)]);
        let pts: Vec<Vec3> = ground.iter().map(|&t| at(t)).collect();
        let (_, n) = camtrack::fit_plane(&pts).unwrap();
        let tilt = n.dot(&Vec3::y()).abs().clamp(0.0, 1.0).acos().to_degrees();
        assert!(tilt < 0.5, "the ground tilts {tilt}°");
        assert!(at(origin).norm() < 0.05, "the origin point is {} off", at(origin).norm());
        let d = (at(a) - at(b)).norm();
        assert!((d - 5.0).abs() < 0.05, "the scale points are {d} apart");
        // The camera's where it was.
        let (f, c0, c1) = (new.frames[10], sol.camera(new.frames[10]).unwrap(), new.camera(new.frames[10]).unwrap());
        let moved = (c0.world.fixed_view::<3, 1>(0, 3) - c1.world.fixed_view::<3, 1>(0, 3)).norm();
        assert!(moved < 0.05, "the camera on frame {f} moved {moved}");
    }

    #[test]
    fn too_few_shared_points_leave_the_solve_raw() {
        let mut new = Solution { points: vec![[1.0, 2.0, 3.0]; 3], point_tracks: vec![0, 1, 2], ..Solution::default() };
        let old = Solution { points: vec![[0.0; 3]; 3], point_tracks: vec![0, 1, 2], ..Solution::default() };
        let before = new.clone();
        assert!(!align_to(&mut new, &old, 1.0));
        assert_eq!(new, before);
        let r = Resolved { solution: new, aligned: false };
        assert!(deleted_message(2, &old, &r).contains("Too few points"));
    }

    /// A Fit Plane on a cloud exported from the solve keeps fitting the
    /// same physical points after some are deleted (its numbers remapped,
    /// the cloud brought up to the new solve); one left with fewer than
    /// three keeps its old points; an untagged copy of the old points
    /// counts as made from it, any other cloud is left alone.
    #[test]
    fn clouds_and_fit_planes_follow_a_resolve_by_track() {
        let pt = |i: usize| [i as f64, 0.0, 0.0];
        let before = Solution { points: (0..6).map(pt).collect(), point_tracks: vec![10, 11, 12, 13, 14, 15], ..Solution::default() };
        // Tracks 11 and 13 deleted; the rest move a little.
        let after = Solution { points: [10, 12, 14, 15].iter().map(|&t| [t as f64 + 0.5, 1.0, 0.0]).collect(), point_tracks: vec![10, 12, 14, 15], ..Solution::default() };
        let tagged = Value::Object(cloud_params("ct", &before));
        let mut old_untagged = serde_json::Map::new();
        old_untagged.insert("points".into(), flat(&before));
        let graph: Graph = serde_json::from_value(json!({ "nodes": [
            { "id": "ct", "op": "camera_tracker" },
            { "id": "cloud", "op": "point_cloud", "params": tagged },
            { "id": "old", "op": "point_cloud", "params": old_untagged },
            { "id": "other", "op": "point_cloud", "params": { "points": [9, 9, 9] } },
            { "id": "floor", "op": "fit_plane", "inputs": ["cloud"], "params": { "points": "0 2 4 5" } },
            { "id": "wall", "op": "fit_plane", "inputs": ["old"], "params": { "points": "1 3 4" } },
            { "id": "all", "op": "fit_plane", "inputs": ["cloud"], "params": { "points": "" } },
            { "id": "else", "op": "fit_plane", "inputs": ["other"], "params": { "points": "0" } }
        ]})).unwrap();
        let r = refresh_clouds(&graph, "ct", &before, &after);
        let get = |n: &str, k: &str| r.set.iter().find(|(a, b, _)| a == n && b == k).map(|x| x.2.clone());
        // floor: tracks 10 12 14 15 → the new points 0 1 2 3.
        assert_eq!(get("floor", "points"), Some(json!("0 1 2 3")));
        for c in ["cloud", "old"] {
            assert_eq!(get(c, "points"), Some(flat(&after)));
            assert_eq!(get(c, POINT_TRACKS), Some(json!([10, 12, 14, 15])));
            assert_eq!(get(c, FROM_TRACKER), Some(json!("ct")));
        }
        // wall: tracks 11 13 14 → only 14 is left.
        assert_eq!(r.keep, vec![("wall".to_string(), flat(&before))]);
        assert!(get("wall", "points").is_none() && get("all", "points").is_none());
        assert!(r.set.iter().all(|(n, _, _)| n != "other" && n != "else"));
        assert!(r.note().contains("wall"));
    }

    /// Renaming the tracker renames its clouds' tag.
    #[test]
    fn a_renamed_tracker_keeps_its_clouds() {
        let mut p = Value::Object(cloud_params("ct", &Solution { points: vec![[0.0; 3]], point_tracks: vec![4], ..Solution::default() }));
        crate::graph_merge::rename_refs("point_cloud", &mut p, "ct", "solver");
        assert_eq!((p[FROM_TRACKER].as_str(), &p[POINT_TRACKS]), (Some("solver"), &json!([4])));
    }
}
