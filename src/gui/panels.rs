//! Panels in windows of their own, for a second monitor: the Node Graph
//! (and, built the same way, the Viewer, Curve Editor and Properties) drawn
//! in an egui immediate viewport, an OS window, instead of in the main
//! window; docked back when that window closes. Whether each is out, its
//! size and (where the platform tells) its position and full screen are
//! kept in settings.json ([`PanelWindow`]).
//!
//! Each window handles its own keys: the app's shortcuts run once a
//! window, given the panes drawn there and the one under the pointer
//! ([`Here`]), so F frames, ⌘A selects and Delete deletes in the window
//! the key went to. Popups and the node browser are egui areas, so they
//! open in the window whose pane made them; dropped files arrive in the
//! window they were dropped on.

use arclight::config::PanelWindow;
use eframe::egui::{self, Pos2, Rect, Vec2, ViewportId};
use std::collections::BTreeMap;

/// A panel that can have a window of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pane {
    Graph,
    Curves,
    Viewer,
    Properties,
}

impl Pane {
    pub const ALL: [Pane; 4] = [Pane::Graph, Pane::Curves, Pane::Viewer, Pane::Properties];

    /// Its key in settings.json.
    pub fn key(self) -> &'static str {
        match self {
            Pane::Graph => "graph",
            Pane::Curves => "curves",
            Pane::Viewer => "viewer",
            Pane::Properties => "properties",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Pane::Graph => "Node Graph",
            Pane::Curves => "Curve Editor",
            Pane::Viewer => "Viewer",
            Pane::Properties => "Properties",
        }
    }

    /// Its window's size the first time it's opened, in points.
    fn default_size(self) -> Vec2 {
        match self {
            Pane::Graph | Pane::Curves => Vec2::new(1100.0, 600.0),
            Pane::Viewer => Vec2::new(1280.0, 760.0),
            Pane::Properties => Vec2::new(420.0, 760.0),
        }
    }

    pub fn viewport_id(self) -> ViewportId {
        ViewportId::from_hash_of(("arclight_pane", self.key()))
    }
}

/// The panes that can go out into a window of their own (so far).
pub const DETACHABLE: &[Pane] = &[Pane::Graph];

/// Which panes are out in windows of their own, and where those are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panels {
    windows: BTreeMap<Pane, PanelWindow>,
    /// Changed since [`take_changed`](Self::take_changed) last said so.
    changed: bool,
}

impl Panels {
    /// From settings.json's `panels` (names it doesn't know left out).
    pub fn from_settings(saved: &BTreeMap<String, PanelWindow>) -> Panels {
        let windows = Pane::ALL.into_iter().filter_map(|p| saved.get(p.key()).map(|w| (p, w.clone()))).collect();
        Panels { windows, changed: false }
    }

    /// As settings.json keeps them: docked panes' sizes too, for next time.
    pub fn to_settings(&self) -> BTreeMap<String, PanelWindow> {
        self.windows.iter().map(|(p, w)| (p.key().to_string(), w.clone())).collect()
    }

    pub fn detached(&self, pane: Pane) -> bool {
        self.windows.get(&pane).is_some_and(|w| w.detached)
    }

    /// Out into its own window, or docked back.
    pub fn set_detached(&mut self, pane: Pane, out: bool) {
        if self.detached(pane) != out {
            self.windows.entry(pane).or_default().detached = out;
            self.changed = true;
        }
    }

    /// The panes out now, in order.
    pub fn out(&self) -> Vec<Pane> {
        Pane::ALL.into_iter().filter(|p| self.detached(*p)).collect()
    }

    /// Whether anything changed since last asked (to save settings).
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// `pane`'s window as it was left: its size, and its place unless the
    /// compositor places windows (`wayland`); full screen if it was.
    pub fn builder(&self, pane: Pane, wayland: bool) -> egui::ViewportBuilder {
        let w = self.windows.get(&pane).cloned().unwrap_or_default();
        let size = w.size.map_or(pane.default_size(), Vec2::from);
        let mut b = egui::ViewportBuilder::default()
            .with_title(format!("{} — {}", pane.title(), arclight::SHORT_NAME))
            .with_app_id("arclight")
            .with_inner_size(size)
            .with_min_inner_size([240.0, 160.0])
            .with_fullscreen(w.fullscreen);
        if let Some(p) = w.pos.filter(|_| !wayland) {
            b = b.with_position(Pos2::from(p));
        }
        b
    }

    /// After drawing `pane`'s window: keep its size (`content`, the
    /// window's inside), its position (`outer`, None on Wayland) and full
    /// screen. True when the window asked to close: then it's docked.
    pub fn track(&mut self, pane: Pane, content: Rect, outer: Option<Rect>, fullscreen: bool, close: bool) -> bool {
        let w = self.windows.entry(pane).or_default();
        let before = w.clone();
        // (Full screen: its size and place stay those to come back to.)
        if !fullscreen && content.is_positive() {
            w.size = Some(content.size().round().into());
            if let Some(o) = outer {
                w.pos = Some(o.min.round().into());
            }
        }
        w.fullscreen = fullscreen;
        if close {
            w.detached = false;
        }
        if *w != before {
            self.changed = true;
        }
        close
    }
}

/// True when the GUI runs on Wayland, where windows can't place
/// themselves (and can't say where they are).
pub fn on_wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var("WINIT_UNIX_BACKEND").map_or(true, |b| b != "x11")
}

/// One window, for its shortcuts: the panes drawn there (the main
/// window's first the bottom panel's tab, then the viewer, then
/// Properties), and the one under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct Here {
    pub panes: Vec<Pane>,
    pub hover: Option<Pane>,
}

impl Here {
    /// The panes in `panes` (drawn in this window) and the one whose rect
    /// (`rects`, from when each was last drawn) holds `pointer`.
    pub fn new(panes: Vec<Pane>, rects: &BTreeMap<Pane, Rect>, pointer: Option<Pos2>) -> Here {
        let hover = pointer.and_then(|p| panes.iter().copied().find(|q| rects.get(q).is_some_and(|r| r.contains(p))));
        Here { panes, hover }
    }

    pub fn has(&self, pane: Pane) -> bool {
        self.panes.contains(&pane)
    }

    /// What F frames: the pane under the pointer, else this window's first
    /// that frames (not Properties).
    pub fn fit_target(&self) -> Option<Pane> {
        let frames = |p: &Pane| *p != Pane::Properties;
        self.hover.filter(frames).or_else(|| self.panes.iter().copied().find(frames))
    }

    /// What ⌘A selects all of: the nodes over the graph or in a window
    /// with the graph and no viewer; else the viewer's shape points.
    pub fn select_all_target(&self) -> Option<Pane> {
        if self.hover == Some(Pane::Graph) {
            Some(Pane::Graph)
        } else if self.has(Pane::Viewer) {
            Some(Pane::Viewer)
        } else if self.has(Pane::Graph) {
            Some(Pane::Graph)
        } else {
            None
        }
    }

    /// Whether the pointer is over the viewer in this window.
    pub fn over(&self, pane: Pane) -> bool {
        self.hover == Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Out and back, with the size and place it had; settings.json's form
    /// round trips; a change is reported once.
    #[test]
    fn panels_keep_their_windows() {
        let mut p = Panels::default();
        assert!(p.out().is_empty() && !p.detached(Pane::Graph));
        p.set_detached(Pane::Graph, true);
        assert!(p.detached(Pane::Graph) && p.take_changed() && !p.take_changed());
        p.set_detached(Pane::Graph, true);
        assert!(!p.take_changed(), "no change, nothing to save");
        // Tracked: size and place (X11 / macOS), then on Wayland (no place).
        let content = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.4, 500.0));
        assert!(!p.track(Pane::Graph, content, Some(Rect::from_min_size(Pos2::new(2000.0, 30.0), Vec2::splat(10.0))), false, false));
        assert!(p.take_changed());
        let saved = p.to_settings();
        assert_eq!(saved["graph"], PanelWindow { detached: true, size: Some([900.0, 500.0]), pos: Some([2000.0, 30.0]), fullscreen: false });
        assert_eq!(Panels::from_settings(&saved), Panels { changed: false, ..p.clone() });
        p.track(Pane::Graph, content, None, false, false);
        assert_eq!(p.to_settings()["graph"].pos, Some([2000.0, 30.0]), "Wayland: the place kept as it was");
        assert!(!p.take_changed());
        // Full screen: the size to come back to stays.
        p.track(Pane::Graph, Rect::from_min_size(Pos2::ZERO, Vec2::new(2560.0, 1440.0)), None, true, false);
        assert_eq!(p.to_settings()["graph"].size, Some([900.0, 500.0]));
        assert!(p.to_settings()["graph"].fullscreen);
        // Closed: docked, the window's size kept for next time.
        assert!(p.track(Pane::Graph, content, None, false, true));
        assert!(!p.detached(Pane::Graph) && p.take_changed());
        assert_eq!(p.to_settings()["graph"].size, Some([900.0, 500.0]));
        // Unknown names in settings.json are left out.
        let mut odd = BTreeMap::new();
        odd.insert("toolbox".to_string(), PanelWindow { detached: true, ..PanelWindow::default() });
        assert!(Panels::from_settings(&odd).out().is_empty());
    }

    /// The builder opens it where it was (not on Wayland), at its size.
    #[test]
    fn windows_open_where_they_were() {
        let mut saved = BTreeMap::new();
        saved.insert("viewer".to_string(), PanelWindow { detached: true, size: Some([800.0, 450.0]), pos: Some([1920.0, 0.0]), fullscreen: true });
        let p = Panels::from_settings(&saved);
        let b = p.builder(Pane::Viewer, false);
        assert_eq!((b.inner_size, b.position, b.fullscreen), (Some(Vec2::new(800.0, 450.0)), Some(Pos2::new(1920.0, 0.0)), Some(true)));
        assert_eq!(p.builder(Pane::Viewer, true).position, None, "Wayland places it");
        assert_eq!(p.builder(Pane::Graph, false).inner_size, Some(Pane::Graph.default_size()), "never out: the default size");
        assert!(Pane::ALL.iter().map(|p| p.viewport_id()).collect::<std::collections::HashSet<_>>().len() == 4);
    }

    /// Keys go to the window they were pressed in: F frames what's under
    /// the pointer there, else that window's first pane; ⌘A selects the
    /// nodes in the graph's window and over the graph, else the viewer's
    /// points.
    #[test]
    fn shortcuts_route_by_window() {
        let rect = |x: f32| Rect::from_min_size(Pos2::new(x, 0.0), Vec2::splat(100.0));
        let rects: BTreeMap<Pane, Rect> = [(Pane::Graph, rect(0.0)), (Pane::Viewer, rect(200.0)), (Pane::Properties, rect(400.0))].into();
        // The main window, everything docked.
        let main = |at: Option<f32>| Here::new(vec![Pane::Graph, Pane::Viewer, Pane::Properties], &rects, at.map(|x| Pos2::new(x, 50.0)));
        assert_eq!((main(Some(250.0)).fit_target(), main(Some(50.0)).fit_target()), (Some(Pane::Viewer), Some(Pane::Graph)));
        assert_eq!(main(Some(450.0)).fit_target(), Some(Pane::Graph), "over Properties: the bottom panel's");
        assert_eq!((main(Some(50.0)).select_all_target(), main(Some(250.0)).select_all_target()), (Some(Pane::Graph), Some(Pane::Viewer)));
        // The graph out: the main window frames and selects in the viewer.
        let main = Here::new(vec![Pane::Viewer, Pane::Properties], &rects, Some(Pos2::new(50.0, 50.0)));
        assert_eq!((main.hover, main.fit_target(), main.select_all_target()), (None, Some(Pane::Viewer), Some(Pane::Viewer)), "the graph's old rect isn't this window's");
        // Its own window: the graph, wherever the pointer is.
        let graph = Here::new(vec![Pane::Graph], &rects, None);
        assert_eq!((graph.fit_target(), graph.select_all_target()), (Some(Pane::Graph), Some(Pane::Graph)));
        let props = Here::new(vec![Pane::Properties], &rects, Some(Pos2::new(450.0, 50.0)));
        assert_eq!((props.fit_target(), props.select_all_target()), (None, None));
    }
}
