//! Where Arclight keeps its files, following the XDG Base Directory spec, the
//! user settings read from `$XDG_CONFIG_HOME/arclight/settings.json`, and the
//! view preferences the app keeps in `$XDG_STATE_HOME/arclight/view.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const APP: &str = "arclight";

/// The user's home directory (`$HOME`).
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$var/arclight` if `$var` is set to an absolute path, else `~/<fallback>/arclight`.
fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => p.join(APP),
        _ => home().join(fallback).join(APP),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// What the app remembers between runs (not settings a user edits).
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

/// Per-session files such as the running-instance registry and sockets.
/// Falls back to the cache dir when `$XDG_RUNTIME_DIR` isn't set.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p.join(APP),
        _ => cache_dir().join("run"),
    }
}

/// Directories searched for native plugins, highest priority first:
/// `$ARCLIGHT_PLUGIN_PATH` (colon-separated), the settings' `plugin_paths`,
/// the user data dir, then the system locations.
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(var) = std::env::var_os("ARCLIGHT_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&var).filter(|p| !p.as_os_str().is_empty()));
    }
    dirs.extend(Settings::load().plugin_paths);
    dirs.push(data_dir().join("plugins"));
    dirs.push(PathBuf::from("/usr/local/lib/arclight/plugins"));
    dirs.push(PathBuf::from("/usr/lib/arclight/plugins"));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// User settings. Every field has a default, so a missing or partial file is fine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Extra plugin directories.
    pub plugin_paths: Vec<PathBuf>,
    /// Memory for the GUI's played-frame cache, in MiB.
    pub frame_cache_mb: usize,
    /// Address `arclight mcp --http` listens on when none is given.
    pub mcp_http_addr: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { plugin_paths: Vec::new(), frame_cache_mb: 1536, mcp_http_addr: "127.0.0.1:7878".into() }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    /// The saved settings, or the defaults if there are none. A malformed
    /// file is reported on stderr and ignored rather than failing startup.
    pub fn load() -> Settings {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("arclight: ignoring {}: {e}", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }
}

/// How the GUI's views were last set up, so the next run starts the same:
/// the 2D viewer's frame size labels, and the 3D view's shading ("solid" or
/// "lit"; None: Lit when a scene has lights), wireframe and Lit shadows,
/// for projects that haven't chosen their own. Kept apart from projects.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ViewPrefs {
    pub show_frame_size: bool,
    pub shading: Option<String>,
    pub wireframe: bool,
    pub lit_shadows: bool,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        ViewPrefs { show_frame_size: true, shading: None, wireframe: false, lit_shadows: false }
    }
}

impl ViewPrefs {
    pub fn path() -> PathBuf {
        state_dir().join("view.json")
    }

    /// The prefs saved at `path`, or the defaults if there are none (or the
    /// file is unreadable: they're only conveniences).
    pub fn load(path: &std::path::Path) -> ViewPrefs {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Save them at `path`, making its directory if needed.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).expect("prefs serialize"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_prefs_round_trip_and_default_when_missing_or_bad() {
        let dir = std::env::temp_dir().join(format!("arclight_view_prefs_{}", std::process::id()));
        let path = dir.join("state/view.json");
        assert_eq!(ViewPrefs::load(&path), ViewPrefs::default());
        assert!(ViewPrefs::default().show_frame_size, "frame size labels on by default");
        let p = ViewPrefs { show_frame_size: false, shading: Some("lit".into()), wireframe: true, lit_shadows: true };
        p.save(&path).unwrap();
        assert_eq!(ViewPrefs::load(&path), p);
        std::fs::write(&path, r#"{"wireframe": true}"#).unwrap();
        assert_eq!(ViewPrefs::load(&path), ViewPrefs { wireframe: true, ..ViewPrefs::default() }, "partial: the rest default");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(ViewPrefs::load(&path), ViewPrefs::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_settings_fill_defaults() {
        let s: Settings = serde_json::from_str(r#"{"frame_cache_mb": 64}"#).unwrap();
        assert_eq!(s.frame_cache_mb, 64);
        assert_eq!(s.mcp_http_addr, Settings::default().mcp_http_addr);
    }
}
