//! GPU acceleration in the GUI: the status line shown in Preferences ▸
//! Performance and the About box (found off the UI thread, since the first
//! look creates the device), and Help ▸ GPU self-test, a window with the
//! table `arclight gpu --check` prints, run on a thread of its own.

use arclight::gpu::{self, check::{CheckReport, CheckRow}};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};

/// [`gpu::status`] found on a thread, so a first look (which creates the
/// device) never holds up the window.
#[derive(Default)]
pub struct StatusProbe {
    line: Option<String>,
    pending: Option<Receiver<String>>,
}

impl StatusProbe {
    /// Look again (after the switch changed, or when a window opens).
    pub fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(gpu::status().line());
            ctx.request_repaint();
        });
        self.pending = Some(rx);
    }

    /// The status line, or "Looking for a GPU…" until it's known; the
    /// first call starts the look.
    pub fn line(&mut self, ctx: &egui::Context) -> String {
        if self.line.is_none() && self.pending.is_none() {
            self.refresh(ctx);
        }
        if let Some(line) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.line = Some(line);
            self.pending = None;
        }
        self.line.clone().unwrap_or_else(|| "Looking for a GPU…".into())
    }
}

/// Help ▸ GPU self-test: every GPU op against the CPU, as a table.
#[derive(Default)]
pub struct SelfTest {
    pub open: bool,
    report: Option<CheckReport>,
    running: Option<Receiver<CheckReport>>,
    copied_until: f64,
}

impl SelfTest {
    /// Open the window and start a run (unless one is going).
    pub fn start(&mut self, ctx: &egui::Context) {
        self.open = true;
        if self.running.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("gpu self-test".into())
            .spawn(move || {
                let _ = tx.send(gpu::check::run());
                ctx.request_repaint();
            })
            .expect("a thread for the GPU self-test");
        self.running = Some(rx);
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if let Some(report) = self.running.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.report = Some(report);
            self.running = None;
        }
        let mut open = true;
        let mut again = false;
        egui::Window::new("GPU self-test").open(&mut open).collapsible(false).resizable(true).default_size([880.0, 380.0]).show(ctx, |ui| {
            ui.label("Each GPU operation against the CPU on the same pictures: the largest difference, whether it's within tolerance, and the time each takes.");
            ui.add_space(6.0);
            if self.running.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Running (a few seconds at 4K)…");
                });
            }
            if let Some(r) = &self.report {
                let (text, color) = if r.passed() {
                    (format!("✔ {}", r.summary()), egui::Color32::from_rgb(90, 180, 100))
                } else if r.rows.is_empty() {
                    (r.summary(), ui.visuals().weak_text_color())
                } else {
                    (format!("✘ {}", r.summary()), egui::Color32::from_rgb(235, 100, 90))
                };
                ui.colored_label(color, text);
                ui.add_space(6.0);
                if !r.rows.is_empty() {
                    egui::ScrollArea::both().max_height(ui.available_height() - 40.0).show(ui, |ui| table(ui, &r.rows));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(self.running.is_none(), egui::Button::new("Run again")).clicked() {
                    again = true;
                }
                if let Some(r) = self.report.as_ref().filter(|_| self.running.is_none()) {
                    if ui.button("Copy").on_hover_text("The table as text, to paste into an issue").clicked() {
                        ui.ctx().copy_text(r.table());
                        self.copied_until = ui.input(|i| i.time) + 2.0;
                    }
                }
                if ui.input(|i| i.time) < self.copied_until {
                    ui.weak("Copied");
                }
            });
        });
        if again {
            self.start(ctx);
        }
        if !open {
            self.open = false;
        }
    }
}

/// The rows: case, size, differences against their tolerances, times.
fn table(ui: &mut egui::Ui, rows: &[CheckRow]) {
    egui::Grid::new("gpu_check_rows").striped(true).num_columns(8).spacing([14.0, 4.0]).show(ui, |ui| {
        for h in ["Case", "Size", "Max abs (tol)", "Max rel (tol)", "", "CPU", "GPU (kernel)", "Speedup"] {
            ui.strong(h);
        }
        ui.end_row();
        for r in rows {
            ui.label(&r.case);
            ui.label(&r.size);
            ui.label(format!("{:.1e} ({:.0e})", r.max_abs, r.tol_abs));
            ui.label(format!("{:.1e} ({:.0e})", r.max_rel, r.tol_rel));
            if r.pass {
                ui.colored_label(egui::Color32::from_rgb(90, 180, 100), "pass");
            } else {
                ui.colored_label(egui::Color32::from_rgb(235, 100, 90), "FAIL").on_hover_text(&r.note);
            }
            ui.label(format!("{:.1} ms", r.cpu_ms));
            ui.label(format!("{:.1} ms ({:.1})", r.gpu_total_ms, r.gpu_kernel_ms));
            ui.label(format!("{:.1}×", r.speedup()));
            ui.end_row();
        }
    });
}
