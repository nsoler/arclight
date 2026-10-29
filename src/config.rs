//! Where Arclight keeps its files, following the XDG Base Directory spec, and
//! the user settings read from `$XDG_CONFIG_HOME/arclight/settings.json`.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_fill_defaults() {
        let s: Settings = serde_json::from_str(r#"{"frame_cache_mb": 64}"#).unwrap();
        assert_eq!(s.frame_cache_mb, 64);
        assert_eq!(s.mcp_http_addr, Settings::default().mcp_http_addr);
    }
}
