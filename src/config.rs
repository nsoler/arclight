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

/// User settings (Edit ▸ Preferences in the GUI, or edited by hand).
/// Every field has a default, so a missing or partial file is fine, and
/// the defaults are how the app behaves without one. Keys it doesn't know
/// (from a newer version, say) are kept when it writes the file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Extra plugin directories.
    pub plugin_paths: Vec<PathBuf>,
    /// Memory for the GUI's played-frame cache, in MiB.
    pub frame_cache_mb: usize,
    /// Address `arclight mcp --http` listens on when none is given.
    pub mcp_http_addr: String,
    pub interface: InterfacePrefs,
    pub new_project: NewProjectPrefs,
    pub files: FilePrefs,
    pub view3d: View3dPrefs,
    pub gpu: GpuPrefs,
    /// Ask at the next start after a crash ("ArclightVFX closed
    /// unexpectedly"); off with its "Don't ask again".
    pub crash_dialog: bool,
    /// The GUI's panels that have their own window (View ▸ Detach…), by
    /// name ("graph", "viewer", "curves", "properties"): whether they're
    /// out, and where. A panel not listed is docked.
    pub panels: std::collections::BTreeMap<String, PanelWindow>,
    /// Keys this version doesn't know, kept as they are.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            plugin_paths: Vec::new(),
            frame_cache_mb: 1536,
            mcp_http_addr: "127.0.0.1:7878".into(),
            interface: InterfacePrefs::default(),
            new_project: NewProjectPrefs::default(),
            files: FilePrefs::default(),
            view3d: View3dPrefs::default(),
            gpu: GpuPrefs::default(),
            crash_dialog: true,
            panels: std::collections::BTreeMap::new(),
            other: serde_json::Map::new(),
        }
    }
}

/// A GUI panel's own window: out of the main window or docked, the size
/// (points) it had and whether it filled its screen. Not its place: the
/// OS places it (a saved place can be off every monitor there is now; a
/// `pos` from 0.14.2 is ignored).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct PanelWindow {
    pub detached: bool,
    pub size: Option<[f32; 2]>,
    pub fullscreen: bool,
}

/// How the GUI looks and handles.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct InterfacePrefs {
    /// Everything's size: 1 as the system has it.
    pub ui_scale: f32,
    /// "dark", "light" or "system" (follow the desktop's).
    pub theme: String,
    /// Nodes dropped in the node graph land on its grid.
    pub snap_to_grid: bool,
    /// The node graph's wires: "curved" or "straight".
    pub wire_style: String,
}

impl Default for InterfacePrefs {
    fn default() -> Self {
        InterfacePrefs { ui_scale: 1.0, theme: "system".into(), snap_to_grid: false, wire_style: "curved".into() }
    }
}

/// What File ▸ New starts with.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct NewProjectPrefs {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    pub first_frame: i64,
    pub last_frame: i64,
}

impl Default for NewProjectPrefs {
    fn default() -> Self {
        NewProjectPrefs { width: 1920, height: 1080, fps: 24.0, first_frame: 1, last_frame: 100 }
    }
}

/// Saving, history and where new projects go.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FilePrefs {
    /// Save a copy of unsaved changes this often, in minutes (0: never).
    /// The copy goes beside the project (never over it), or in the state
    /// dir for one not saved yet.
    pub autosave_minutes: u32,
    /// Projects listed in File ▸ Open Recent.
    pub recent_count: usize,
    /// Steps of undo kept (0: no limit).
    pub undo_levels: usize,
    /// The Projects folder: where new projects' folders go (File ▸ New),
    /// and where Open starts.
    pub project_dir: Option<PathBuf>,
}

impl Default for FilePrefs {
    fn default() -> Self {
        FilePrefs { autosave_minutes: 0, recent_count: 10, undo_levels: 200, project_dir: None }
    }
}

/// How the 3D view takes the mouse and trackpad.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct View3dPrefs {
    /// Alt + left-drag does what the middle button does.
    pub emulate_three_button: bool,
    /// What two fingers scrolling on a trackpad do: "orbit" or "pan".
    pub trackpad: String,
    /// Zoom toward the pointer (else the middle of the view).
    pub zoom_to_pointer: bool,
    pub invert_zoom: bool,
    /// Orbit about the selected objects (else about the view's middle).
    pub orbit_selection: bool,
    /// How meshes shade in a project that hasn't chosen: "auto" (Lit when
    /// the scene has lights), "solid" or "lit".
    pub default_shading: String,
}

impl Default for View3dPrefs {
    fn default() -> Self {
        View3dPrefs { emulate_three_button: false, trackpad: "orbit".into(), zoom_to_pointer: true, invert_zoom: false, orbit_selection: true, default_shading: "auto".into() }
    }
}

/// GPU acceleration (Preferences ▸ Performance). `ARCLIGHT_GPU=0` in the
/// environment turns it off whatever this says.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct GpuPrefs {
    /// Render on the GPU where it can (else everything on the CPU).
    pub enabled: bool,
    /// GPU memory for an evaluator's cached results, in MiB (see
    /// `gpu::cache_budget`).
    pub cache_mb: u64,
    /// GPU memory for the GUI's played frames, in MiB, apart from
    /// `cache_mb`: frames kept there play straight from the GPU (see
    /// `gpu::frame_cache_budget`).
    pub frame_cache_mb: u64,
}

impl Default for GpuPrefs {
    fn default() -> Self {
        GpuPrefs { enabled: true, cache_mb: 2048, frame_cache_mb: 1024 }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    /// The saved settings, or the defaults if there are none. A malformed
    /// file is reported on stderr and ignored rather than failing startup.
    pub fn load() -> Settings {
        Self::load_from(&Self::path())
    }

    /// [`load`](Self::load) from `path`.
    pub fn load_from(path: &std::path::Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("arclight: ignoring {}: {e}", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    /// Write them to `path` as pretty JSON (still easy to edit by hand),
    /// making its directory if needed.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        write_json(path, self)
    }
}

/// `value` as pretty JSON at `path` (its directory made if needed),
/// written whole or not at all: to a temporary file first, then moved over
/// the old one.
fn write_json(path: &std::path::Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(value).expect("serializable") + "\n")?;
    std::fs::rename(&tmp, path)
}

/// The projects opened or saved last, newest first (File ▸ Open Recent).
/// Kept in the state dir, apart from settings and projects.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct RecentFiles {
    pub paths: Vec<PathBuf>,
}

impl RecentFiles {
    pub fn path() -> PathBuf {
        state_dir().join("recent.json")
    }

    /// The list saved at `path` (empty if there's none, or it's unreadable).
    pub fn load(path: &std::path::Path) -> RecentFiles {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        write_json(path, self)
    }

    /// `file` at the top (moved there if it's listed already), keeping at
    /// most `count`.
    pub fn add(&mut self, file: &std::path::Path, count: usize) {
        let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        self.paths.retain(|p| *p != file);
        self.paths.insert(0, file);
        self.paths.truncate(count);
    }
}

/// How the GUI's views were last set up, so the next run starts the same:
/// the 2D viewer's frame size labels, and the 3D view's shading ("solid" or
/// "lit"; None: Lit when a scene has lights), wireframe and Lit shadows,
/// for projects that haven't chosen their own; and whether playback plays
/// sound. Kept apart from projects.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ViewPrefs {
    pub show_frame_size: bool,
    pub shading: Option<String>,
    pub wireframe: bool,
    pub lit_shadows: bool,
    pub sound: bool,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        ViewPrefs { show_frame_size: true, shading: None, wireframe: false, lit_shadows: false, sound: true }
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
        assert!(ViewPrefs::default().show_frame_size && ViewPrefs::default().sound, "frame size labels and sound on by default");
        let p = ViewPrefs { show_frame_size: false, shading: Some("lit".into()), wireframe: true, lit_shadows: true, sound: false };
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

    /// A settings.json from before Preferences loads as it was, with the
    /// new sections at their defaults (today's behaviour); a section given
    /// in part fills in the rest; keys it doesn't know survive a save.
    #[test]
    fn settings_load_old_files_and_keep_unknown_keys() {
        let old: Settings = serde_json::from_str(r#"{"plugin_paths": ["/opt/p"], "frame_cache_mb": 512, "mcp_http_addr": "0.0.0.0:9000"}"#).unwrap();
        assert_eq!(old, Settings { plugin_paths: vec!["/opt/p".into()], frame_cache_mb: 512, mcp_http_addr: "0.0.0.0:9000".into(), ..Settings::default() });
        let d = Settings::default();
        assert_eq!((d.interface.ui_scale, d.interface.theme.as_str(), d.interface.snap_to_grid, d.interface.wire_style.as_str()), (1.0, "system", false, "curved"));
        assert_eq!((d.new_project.width, d.new_project.height, d.new_project.fps, d.new_project.first_frame, d.new_project.last_frame), (1920, 1080, 24.0, 1, 100));
        assert_eq!((d.files.autosave_minutes, d.files.recent_count, d.files.undo_levels, d.files.project_dir.clone()), (0, 10, 200, None));
        assert_eq!((d.view3d.trackpad.as_str(), d.view3d.zoom_to_pointer, d.view3d.default_shading.as_str()), ("orbit", true, "auto"));
        assert!(d.gpu.enabled, "GPU acceleration on by default");
        let off: Settings = serde_json::from_str(r#"{"gpu": {"enabled": false}}"#).unwrap();
        assert_eq!(off, Settings { gpu: GpuPrefs { enabled: false, ..GpuPrefs::default() }, ..Settings::default() });
        // The budgets, set by hand, survive a save from Preferences.
        let set: Settings = serde_json::from_str(r#"{"gpu": {"cache_mb": 4096, "frame_cache_mb": 512}}"#).unwrap();
        assert_eq!((set.gpu.cache_mb, set.gpu.frame_cache_mb), (4096, 512));
        let again: Settings = serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(again, set);
        assert_eq!((d.gpu.cache_mb, d.gpu.frame_cache_mb), (2048, 1024));
        let part: Settings = serde_json::from_str(r#"{"interface": {"theme": "light"}, "files": {"project_dir": "/shows"}, "from_the_future": {"x": 1}}"#).unwrap();
        assert_eq!(part.interface, InterfacePrefs { theme: "light".into(), ..InterfacePrefs::default() });
        assert_eq!(part.files.project_dir, Some(PathBuf::from("/shows")));
        let dir = std::env::temp_dir().join(format!("arclight_settings_{}", std::process::id()));
        let path = dir.join("config/settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default(), "none: the defaults");
        part.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n  \"interface\": {") && text.contains("\"from_the_future\""), "pretty, unknown key kept:\n{text}");
        assert_eq!(Settings::load_from(&path), part);
        std::fs::write(&path, "{ broken").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Detached panels' windows: none by default (all docked, as before);
    /// what's kept comes back; a part fills in the rest.
    #[test]
    fn panel_windows_round_trip() {
        assert!(Settings::default().panels.is_empty());
        let mut s = Settings::default();
        s.panels.insert("graph".into(), PanelWindow { detached: true, size: Some([900.0, 500.0]), fullscreen: false });
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let part: Settings = serde_json::from_str(r#"{"panels": {"viewer": {"detached": true}}}"#).unwrap();
        assert_eq!(part.panels["viewer"], PanelWindow { detached: true, ..PanelWindow::default() });
    }

    #[test]
    fn recent_files_put_the_newest_first_once_and_keep_count() {
        let mut r = RecentFiles::default();
        for f in ["/p/a.json", "/p/b.json", "/p/c.json", "/p/a.json"] {
            r.add(std::path::Path::new(f), 3);
        }
        assert_eq!(r.paths, ["/p/a.json", "/p/c.json", "/p/b.json"].map(PathBuf::from));
        r.add(std::path::Path::new("/p/d.json"), 2);
        assert_eq!(r.paths, ["/p/d.json", "/p/a.json"].map(PathBuf::from));
        let dir = std::env::temp_dir().join(format!("arclight_recent_{}", std::process::id()));
        let path = dir.join("recent.json");
        assert_eq!(RecentFiles::load(&path), RecentFiles::default());
        r.save(&path).unwrap();
        assert_eq!(RecentFiles::load(&path), r);
        std::fs::remove_dir_all(&dir).ok();
    }
}
