//! Stamps the build time into `ARCLIGHT_BUILD_TIME` (UTC, "YYYY-MM-DD
//! HH:MM UTC") for the GUI's About dialog. `SOURCE_DATE_EPOCH`, when set,
//! wins, for reproducible builds.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Restamp on a new commit or branch switch, not on every build (an
    // always-rerunning script would rebuild the library each time).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // Only paths that exist: a missing one makes Cargo rerun every time
    // (in a `git worktree`, `.git` is a file, so these are skipped).
    let head = std::path::Path::new(".git/HEAD");
    if head.is_file() {
        println!("cargo:rerun-if-changed=.git/HEAD");
        let branch = std::fs::read_to_string(head).ok().and_then(|s| s.strip_prefix("ref: ").map(|r| format!(".git/{}", r.trim())));
        if let Some(b) = branch.filter(|b| std::path::Path::new(b).is_file()) {
            println!("cargo:rerun-if-changed={b}");
        }
    }
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    println!(
        "cargo:rustc-env=ARCLIGHT_BUILD_TIME={y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    );
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day)
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
