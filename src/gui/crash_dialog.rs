//! "Arclight closed unexpectedly": at the start after a crash (and from
//! Help ▸ Report a problem), the crash report (see `arclight::crash`) to
//! view, copy, or open as a prefilled GitHub issue the user reviews and
//! submits. Nothing is sent by Arclight; file paths read `<path>` unless
//! the user includes them.

use arclight::crash;
use eframe::egui;
use std::path::PathBuf;

/// The most of a report put in an issue's link (browsers and GitHub cut
/// long URLs); the rest is in the file.
const ISSUE_REPORT_CHARS: usize = 5000;

#[derive(Default)]
pub struct CrashDialog {
    pub open: bool,
    /// Asking about a crash at start (else from the Help menu).
    pub crashed: bool,
    /// The report shown, and its text as written.
    pub report: Option<PathBuf>,
    text: String,
    /// Show (and copy, and put in the issue) file paths as they are.
    pub include_paths: bool,
    /// The report's text is unfolded.
    pub viewing: bool,
    /// "Copied" shows until this time.
    copied_until: f64,
}

/// What the user chose that the app acts on.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Don't ask at the next start after a crash again.
    DontAskAgain,
}

impl CrashDialog {
    /// The dialog for the last crash, if there's one not asked about yet
    /// and asking is on (`ask`).
    pub fn at_start(ask: bool) -> CrashDialog {
        match crash::pending().filter(|_| ask) {
            Some(report) => CrashDialog::showing(Some(report), true),
            None => CrashDialog::default(),
        }
    }

    /// Help ▸ Report a problem: the latest report (a crash's or a caught
    /// error's), or none to show but the way to report.
    pub fn for_help() -> CrashDialog {
        CrashDialog::showing(crash::reports().pop(), false)
    }

    fn showing(report: Option<PathBuf>, crashed: bool) -> CrashDialog {
        let text = report.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        CrashDialog { open: true, crashed, report, text, ..CrashDialog::default() }
    }

    /// The report as shown, copied and put in an issue: paths scrubbed
    /// unless they're included.
    pub fn shown(&self) -> String {
        if self.include_paths {
            self.text.clone()
        } else {
            crash::scrub(&self.text)
        }
    }

    /// A new GitHub issue prefilled with the report (cut to fit a link),
    /// for the user to review and submit.
    pub fn issue_url(&self) -> String {
        let shown = self.shown();
        let panic = shown.lines().find_map(|l| l.strip_prefix("Panic: ").or_else(|| l.strip_prefix("Signal: "))).unwrap_or("");
        let title = if self.report.is_none() {
            "Problem: ".to_string()
        } else if panic.is_empty() {
            "Crash".to_string()
        } else {
            format!("Crash: {}", panic.chars().take(80).collect::<String>())
        };
        let mut report: String = shown.chars().take(ISSUE_REPORT_CHARS).collect();
        if shown.chars().count() > ISSUE_REPORT_CHARS {
            report += "\n… (cut: the whole report is in Arclight's crash reports folder)";
        }
        let body = if self.report.is_none() {
            format!("**What happened?**\n\n\n**What did you expect?**\n\n\n**Steps to reproduce**\n\n\nArclight {}", env!("CARGO_PKG_VERSION"))
        } else {
            format!("**What were you doing when it happened?**\n\n\n<details><summary>Crash report</summary>\n\n```\n{report}\n```\n</details>\n\n<!-- Check the report for anything private before submitting. -->")
        };
        format!("{}?title={}&body={}", crash::ISSUES_URL, encode(&title), encode(&body))
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Choice> {
        if !self.open {
            return None;
        }
        let mut choice = None;
        let mut open = true;
        let title = if self.crashed { "Arclight closed unexpectedly" } else { "Report a problem" };
        egui::Window::new(title).id(egui::Id::unique("crash dialog")).collapsible(false).resizable(true).default_width(560.0).open(&mut open).show(ctx, |ui| {
            if self.crashed {
                ui.label("Arclight closed unexpectedly the last time it ran. A report of what happened was saved on this computer; nothing was sent.");
                if let Some(line) = self.text.lines().find(|l| l.starts_with("Project: ")) {
                    ui.label(line.trim_start_matches("Project: ").replace("saved to its autosave", "Your project was saved to its autosave"));
                }
            } else if self.report.is_some() {
                ui.label("The latest crash report saved on this computer (nothing is sent). Add it to a GitHub issue, or copy it.");
            } else {
                ui.label("No crash reports. Describe the problem in a GitHub issue.");
            }
            if let Some(p) = &self.report {
                ui.weak(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.report.is_some() {
                    let label = if self.viewing { "Hide report" } else { "View report" };
                    if ui.button(label).clicked() {
                        self.viewing = !self.viewing;
                    }
                    if ui.button("Copy").on_hover_text("Copy the report, to paste into an issue or a message").clicked() {
                        ui.ctx().copy_text(self.shown());
                        self.copied_until = ui.input(|i| i.time) + 2.0;
                    }
                }
                if ui.button("Report on GitHub…").on_hover_text("Open a new issue in your browser, filled in with the report, to review and submit").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(self.issue_url()));
                }
                if ui.input(|i| i.time) < self.copied_until {
                    ui.weak("Copied");
                }
            });
            if self.report.is_some() {
                ui.checkbox(&mut self.include_paths, "Include file names").on_hover_text("Off: file paths read <path>, so the report shows nothing about your files");
            }
            if self.viewing {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    let mut shown = self.shown();
                    ui.add(egui::TextEdit::multiline(&mut shown).code_editor().desired_width(f32::INFINITY).interactive(true));
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.crashed && ui.button("Don't ask again").on_hover_text("Reports are still saved; Help ▸ Report a problem shows them").clicked() {
                    choice = Some(Choice::DontAskAgain);
                    self.close();
                }
                if ui.button("Close").clicked() {
                    self.close();
                }
            });
        });
        if !open {
            self.close();
        }
        choice
    }

    fn close(&mut self) {
        self.open = false;
        if self.crashed {
            crash::clear_pending();
        }
    }
}

/// `s` for a URL's query: everything but unreserved characters
/// percent-encoded (UTF-8).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(text: &str) -> CrashDialog {
        CrashDialog { open: true, crashed: true, report: Some("r.txt".into()), text: text.into(), ..CrashDialog::default() }
    }

    #[test]
    fn the_report_shows_no_files_unless_asked() {
        let mut d = with("Panic: can't read /home/someone/shots/a.exr\nAt: src/ops.rs:3");
        assert!(d.shown().contains("can't read <path>") && d.shown().contains("src/ops.rs:3"));
        d.include_paths = true;
        assert!(d.shown().contains("/home/someone/shots/a.exr"));
    }

    #[test]
    fn the_issue_link_is_prefilled_encoded_and_short_enough() {
        let mut long = String::from("Arclight closed unexpectedly\nPanic: index out of bounds & more\n");
        long += &"   9: arclight::frame\n".repeat(2000);
        let d = with(&long);
        let url = d.issue_url();
        assert!(url.starts_with(&format!("{}?title=Crash%3A%20index%20out%20of%20bounds%20%26%20more&body=", crash::ISSUES_URL)), "{}", &url[..120]);
        assert!(url.len() < 20_000, "{}", url.len());
        assert!(url.contains(&encode("(cut: the whole report")), "cut, and says so");
        assert!(!url.contains(' ') && !url.contains('\n'));
        let help = CrashDialog { open: true, ..CrashDialog::default() };
        assert!(help.issue_url().contains("title=Problem%3A%20") && help.issue_url().contains(&encode("Steps to reproduce")));
        assert_eq!(encode("a b/é"), "a%20b%2F%C3%A9");
    }

    #[test]
    fn a_crash_is_asked_about_once_unless_turned_off() {
        let dir = std::env::temp_dir().join(format!("arclight_crash_dialog_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        // (The crash dir follows ARCLIGHT_CRASH_DIR; only this test sets it
        // in this process.)
        std::env::set_var("ARCLIGHT_CRASH_DIR", &dir);
        assert!(!CrashDialog::at_start(true).open, "no crash: no question");
        let report = crash::write_report(crash::Kind::Crash, "Arclight closed unexpectedly\nPanic: boom\nProject: saved to its autosave, ~/a.autosave").unwrap();
        std::fs::write(dir.join("pending"), report.file_name().unwrap().to_string_lossy().as_bytes()).unwrap();
        assert!(!CrashDialog::at_start(false).open, "asking turned off");
        let mut d = CrashDialog::at_start(true);
        assert!(d.open && d.crashed && d.report.as_deref() == Some(report.as_path()) && d.shown().contains("Panic: boom"));
        d.close();
        assert!(!d.open && crash::pending().is_none(), "asked once");
        assert!(!CrashDialog::at_start(true).open);
        // Help ▸ Report a problem: the latest report, crash or not.
        let caught = crash::write_report(crash::Kind::Recovered, "An internal error was caught\nPanic: later").unwrap();
        let h = CrashDialog::for_help();
        assert!(h.open && !h.crashed && h.report.as_deref() == Some(caught.as_path()));
        std::env::remove_var("ARCLIGHT_CRASH_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }
}
