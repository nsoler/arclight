//! Help ▸ ArclightVFX Help (F1): `docs/help.md`, built into the app, shown as
//! formatted markdown (headings, lists, tables, bold, code) in a scrollable,
//! resizable window. Esc or its close button closes it.

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

/// The help text.
pub const HELP: &str = include_str!("../../docs/help.md");

#[derive(Default)]
pub struct Help {
    pub open: bool,
    cache: CommonMarkCache,
}

impl Help {
    /// F1 opens the window; Esc closes it (taking the key, so it doesn't
    /// also deselect). Call before the app's other shortcuts.
    pub fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
            self.open = true;
        }
        if self.open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(format!("{} Help", arclight::SHORT_NAME))
            .id(egui::Id::unique("arclight_help"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 600.0])
            .min_size([360.0, 240.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    CommonMarkViewer::new().show(ui, &mut self.cache, HELP);
                });
            });
        self.open &= open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    /// Every heading and list item of the help, as plain text.
    fn outline() -> (Vec<(HeadingLevel, String)>, usize, usize) {
        let (mut headings, mut items, mut rows) = (Vec::new(), 0, 0);
        let mut current: Option<(HeadingLevel, String)> = None;
        let opts = pulldown_cmark::Options::ENABLE_TABLES;
        for e in Parser::new_ext(HELP, opts) {
            match e {
                Event::Start(Tag::Heading { level, .. }) => current = Some((level, String::new())),
                Event::Text(t) | Event::Code(t) => {
                    if let Some((_, s)) = &mut current {
                        s.push_str(&t);
                    }
                }
                Event::End(TagEnd::Heading(_)) => headings.extend(current.take()),
                Event::Start(Tag::Item) => items += 1,
                Event::Start(Tag::TableRow) => rows += 1,
                _ => {}
            }
        }
        (headings, items, rows)
    }

    #[test]
    fn the_help_parses_into_its_sections() {
        let (headings, items, rows) = outline();
        assert_eq!(headings.first().map(|h| h.1.as_str()), Some("ArclightVFX Help"));
        assert!(headings.iter().all(|(_, t)| !t.trim().is_empty()), "no empty headings");
        let names: Vec<&str> = headings.iter().map(|h| h.1.as_str()).collect();
        for want in ["Node graph", "Viewer", "Player", "Keyframes", "Curve editor", "3D view", "Navigating", "Views", "Selecting in 3D", "Moving, rotating, scaling", "Undo"] {
            assert!(names.contains(&want), "missing section {want}: {names:?}");
        }
        assert!(items > 20 && rows > 30, "{items} list items, {rows} table rows");
        // What the old Help menu said is still here.
        for key in ["**J** / **K** / **L**", "**⌘G**", "◇", "**⇧⌘Z**", "**F1**"] {
            assert!(HELP.contains(key), "{key}");
        }
    }

    /// Run `frames` of the help window headlessly, with `events` on the
    /// first; the window's open state after.
    fn run(help: &mut Help, events: Vec<egui::Event>, frames: usize) -> bool {
        let ctx = egui::Context::default();
        let mut events = Some(events);
        for _ in 0..frames {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
                events: events.take().unwrap_or_default(),
                ..Default::default()
            };
            let _ = ctx.run_ui(raw, |ui| {
                let ctx = ui.ctx().clone();
                help.shortcuts(&ctx);
                help.ui(&ctx);
            });
        }
        help.open
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn f1_opens_the_help_and_esc_closes_it() {
        let mut help = Help::default();
        assert!(!run(&mut help, vec![], 1));
        assert!(run(&mut help, vec![key(egui::Key::F1)], 3), "F1 opens it (and it renders)");
        assert!(!run(&mut help, vec![key(egui::Key::Escape)], 1), "Esc closes it");
    }
}
