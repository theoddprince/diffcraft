//! Sessions (a saved comparison: kind, paths and rules), application settings and
//! per-file-type rules, stored as JSON in the platform's config directory:
//!
//! * Windows: `%APPDATA%\theoddprince\DiffCraft\config\`
//! * macOS: `~/Library/Application Support/io.github.theoddprince.DiffCraft/`
//! * Linux/BSD: `$XDG_CONFIG_HOME/diffcraft/` (usually `~/.config/diffcraft/`)
//!
//! A corrupt settings file never stops the app: it is moved aside and defaults are used.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod git;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use diffcraft_core::folder::FolderCompareOptions;
use diffcraft_core::folder::sync::SyncMode;
use diffcraft_core::text::TextCompareOptions;
use serde::{Deserialize, Serialize};

/// Errors reading or writing settings and session files.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no configuration directory is available on this system")]
    NoConfigDir,
}

pub type Result<T> = std::result::Result<T, Error>;

/// The kind of a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionKind {
    #[default]
    TextCompare,
    TextMerge,
    FolderCompare,
    FolderSync,
    HexCompare,
    PictureCompare,
}

impl SessionKind {
    pub const ALL: [SessionKind; 6] = [
        SessionKind::FolderCompare,
        SessionKind::FolderSync,
        SessionKind::TextCompare,
        SessionKind::TextMerge,
        SessionKind::HexCompare,
        SessionKind::PictureCompare,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SessionKind::TextCompare => "Text Compare",
            SessionKind::TextMerge => "Text Merge",
            SessionKind::FolderCompare => "Folder Compare",
            SessionKind::FolderSync => "Folder Sync",
            SessionKind::HexCompare => "Hex Compare",
            SessionKind::PictureCompare => "Picture Compare",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SessionKind::TextCompare => "Compare and edit two text files side by side",
            SessionKind::TextMerge => "Merge two changed versions of a file against their common base",
            SessionKind::FolderCompare => "Compare two folder trees and copy, move or delete files",
            SessionKind::FolderSync => "Update or mirror one folder from another",
            SessionKind::HexCompare => "Compare two binary files byte by byte",
            SessionKind::PictureCompare => "Compare two images pixel by pixel",
        }
    }

    /// Takes folders rather than files.
    pub fn is_folder(self) -> bool {
        matches!(self, SessionKind::FolderCompare | SessionKind::FolderSync)
    }
}

/// A comparison that can be saved, reopened and listed under "recent".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub name: String,
    pub kind: SessionKind,
    pub left: String,
    pub right: String,
    /// Common ancestor (Text Merge).
    pub base: Option<String>,
    /// Merge output file (Text Merge).
    pub output: Option<String>,
    pub text: TextCompareOptions,
    pub folder: FolderCompareOptions,
    pub sync_mode: SyncMode,
    /// Seconds since the Unix epoch.
    pub last_used: u64,
}

impl Session {
    pub fn new(kind: SessionKind, left: impl Into<String>, right: impl Into<String>) -> Self {
        Session { kind, left: left.into(), right: right.into(), last_used: now_secs(), ..Session::default() }
    }

    /// A display name: the explicit name, or `left ↔ right` file names.
    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        let short = |p: &str| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_owned());
        if self.left.is_empty() && self.right.is_empty() {
            return self.kind.label().to_owned();
        }
        format!("{} ↔ {}", short(&self.left), short(&self.right))
    }

    /// Same comparison target (used to de-duplicate the recent list).
    pub fn same_target(&self, other: &Session) -> bool {
        self.kind == other.kind && self.left == other.left && self.right == other.right && self.base == other.base
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Colour theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Dark, Theme::Light];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "Follow system",
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }
}

/// Comparison rules for a set of file types (e.g. `*.c;*.h` ignore whitespace changes).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileFormatRule {
    pub name: String,
    /// `;`-separated masks such as `*.c;*.h;Makefile`.
    pub masks: String,
    pub text: TextCompareOptions,
    pub tab_width: Option<usize>,
}

impl FileFormatRule {
    /// Does the rule apply to this file name? Masks are `*.ext`, `name` or `prefix*`.
    pub fn matches(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return false };
        self.masks.split(';').map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).any(|m| mask_matches(&m, &name))
    }
}

/// Minimal `*`-only wildcard match (case already folded by the caller).
fn mask_matches(mask: &str, name: &str) -> bool {
    let parts: Vec<&str> = mask.split('*').collect();
    if parts.len() == 1 {
        return mask == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(pos) => rest = rest.get(pos + part.len()..).unwrap_or_default(),
                None => return false,
            }
        }
    }
    true
}

/// Application preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Monospace font size in points.
    pub font_size: f32,
    /// Overall UI zoom.
    pub ui_scale: f32,
    pub tab_width: usize,
    pub show_line_numbers: bool,
    pub intraline_highlight: bool,
    /// Ask before copying, deleting or overwriting files.
    pub confirm_file_operations: bool,
    /// Delete to the system trash instead of permanently.
    pub delete_to_trash: bool,
    /// Make a `.bak` copy before saving over a file.
    pub backup_on_save: bool,
    pub default_text: TextCompareOptions,
    pub default_folder: FolderCompareOptions,
    pub file_rules: Vec<FileFormatRule>,
    pub hex_bytes_per_row: usize,
    /// Align inserted/deleted bytes in the hex view (slower on large files).
    pub hex_align: bool,
    pub picture_tolerance: u8,
    /// Reload a file when it changes on disk (if it has no unsaved edits).
    pub auto_reload: bool,
    pub recent: Vec<Session>,
    pub saved: Vec<Session>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            font_size: 13.0,
            ui_scale: 1.0,
            tab_width: 4,
            show_line_numbers: true,
            intraline_highlight: true,
            confirm_file_operations: true,
            delete_to_trash: true,
            backup_on_save: false,
            default_text: TextCompareOptions::default(),
            default_folder: FolderCompareOptions::default(),
            file_rules: Vec::new(),
            hex_bytes_per_row: 16,
            hex_align: true,
            picture_tolerance: 0,
            auto_reload: true,
            recent: Vec::new(),
            saved: Vec::new(),
        }
    }
}

/// Maximum entries kept in the recent list.
pub const MAX_RECENT: usize = 30;

impl Settings {
    /// Remember a session at the top of the recent list (deduplicated).
    pub fn add_recent(&mut self, mut s: Session) {
        s.last_used = now_secs();
        self.recent.retain(|r| !r.same_target(&s));
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
    }

    /// Save (or replace by name) a named session.
    pub fn save_session(&mut self, s: Session) {
        match self.saved.iter_mut().find(|x| x.name == s.name) {
            Some(slot) => *slot = s,
            None => self.saved.push(s),
        }
        self.saved.sort_by_key(|s| s.name.to_lowercase());
    }

    /// The first file-format rule matching either path.
    pub fn rule_for(&self, left: &Path, right: &Path) -> Option<&FileFormatRule> {
        self.file_rules.iter().find(|r| r.matches(left) || r.matches(right))
    }

    /// Text rules for a file pair: a matching file-format rule, else the defaults.
    pub fn text_options_for(&self, left: &Path, right: &Path) -> TextCompareOptions {
        self.rule_for(left, right).map(|r| r.text.clone()).unwrap_or_else(|| self.default_text.clone())
    }

    /// Clamp values that would make the UI unusable (hand-edited or corrupt files).
    pub fn sanitize(&mut self) {
        if !self.font_size.is_finite() {
            self.font_size = 13.0;
        }
        self.font_size = self.font_size.clamp(6.0, 48.0);
        if !self.ui_scale.is_finite() {
            self.ui_scale = 1.0;
        }
        self.ui_scale = self.ui_scale.clamp(0.5, 3.0);
        self.tab_width = self.tab_width.clamp(1, 16);
        self.hex_bytes_per_row = match self.hex_bytes_per_row {
            8 | 16 | 32 => self.hex_bytes_per_row,
            _ => 16,
        };
        self.recent.truncate(MAX_RECENT);
    }
}

/// Where settings live.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The platform config directory (or `DIFFCRAFT_CONFIG_DIR` when set, for portable installs).
    pub fn platform() -> Result<Store> {
        if let Some(dir) = std::env::var_os("DIFFCRAFT_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return Ok(Store { dir: PathBuf::from(dir) });
        }
        let dirs = directories::ProjectDirs::from("io.github", "theoddprince", "DiffCraft").ok_or(Error::NoConfigDir)?;
        Ok(Store { dir: dirs.config_dir().to_path_buf() })
    }

    /// A store in an explicit directory (tests, portable installs).
    pub fn at(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    /// Load settings. Missing file: defaults. Corrupt file: moved aside, defaults, and a warning.
    pub fn load(&self) -> (Settings, Option<String>) {
        let path = self.settings_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
            Err(e) => return (Settings::default(), Some(format!("Could not read {}: {e}", path.display()))),
        };
        match serde_json::from_slice::<Settings>(&bytes) {
            Ok(mut s) => {
                s.sanitize();
                (s, None)
            }
            Err(e) => {
                let backup = self.dir.join(format!("settings.corrupt-{}.json", now_secs()));
                let moved = std::fs::rename(&path, &backup).is_ok();
                let note = if moved { format!(" It was saved as {}.", backup.display()) } else { String::new() };
                (Settings::default(), Some(format!("Settings file was unreadable ({e}); defaults are used.{note}")))
            }
        }
    }

    /// Save settings atomically.
    pub fn save(&self, s: &Settings) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|source| Error::Io { path: self.dir.clone(), source })?;
        let path = self.settings_path();
        let json = serde_json::to_vec_pretty(s).map_err(|source| Error::Json { path: path.clone(), source })?;
        diffcraft_core::fsutil::write_atomic(&path, &json).map_err(|e| Error::Io { path, source: std::io::Error::other(e.to_string()) })
    }
}

/// Export a session to a standalone `.dcsession` file.
pub fn save_session_file(path: &Path, s: &Session) -> Result<()> {
    let json = serde_json::to_vec_pretty(s).map_err(|source| Error::Json { path: path.to_path_buf(), source })?;
    diffcraft_core::fsutil::write_atomic(path, &json).map_err(|e| Error::Io { path: path.to_path_buf(), source: std::io::Error::other(e.to_string()) })
}

/// Import a session file.
pub fn load_session_file(path: &Path) -> Result<Session> {
    let bytes = std::fs::read(path).map_err(|source| Error::Io { path: path.to_path_buf(), source })?;
    serde_json::from_slice(&bytes).map_err(|source| Error::Json { path: path.to_path_buf(), source })
}

/// File extension for exported sessions.
pub const SESSION_EXTENSION: &str = "dcsession";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_corruption() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::at(d.path().join("cfg"));
        let (s, warn) = store.load();
        assert_eq!(s, Settings::default());
        assert!(warn.is_none());

        let mut s = Settings { font_size: 15.0, ..Settings::default() };
        s.add_recent(Session::new(SessionKind::TextCompare, "a", "b"));
        s.add_recent(Session::new(SessionKind::TextCompare, "a", "b"));
        assert_eq!(s.recent.len(), 1);
        store.save(&s).unwrap();
        let (back, warn) = store.load();
        assert!(warn.is_none());
        assert_eq!(back.font_size, 15.0);
        assert_eq!(back.recent.len(), 1);

        std::fs::write(store.settings_path(), b"{ not json").unwrap();
        let (back, warn) = store.load();
        assert_eq!(back, Settings::default());
        assert!(warn.unwrap().contains("unreadable"));
        assert!(!store.settings_path().exists(), "corrupt file moved aside");
    }

    #[test]
    fn partial_and_absurd_settings_load() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::at(d.path());
        std::fs::write(store.settings_path(), br#"{"font_size": 1000, "tab_width": 0, "hex_bytes_per_row": 7}"#).unwrap();
        let (s, warn) = store.load();
        assert!(warn.is_none());
        assert_eq!((s.font_size, s.tab_width, s.hex_bytes_per_row), (48.0, 1, 16));
    }

    #[test]
    fn rules_match_masks() {
        let r = FileFormatRule { masks: "*.c; *.H;Makefile;test_*".into(), ..Default::default() };
        assert!(r.matches(Path::new("/x/main.c")));
        assert!(r.matches(Path::new("x.h")));
        assert!(r.matches(Path::new("Makefile")));
        assert!(r.matches(Path::new("test_foo.rs")));
        assert!(!r.matches(Path::new("main.cpp")));
        assert!(mask_matches("a*b*c", "axxbyyc"));
        assert!(!mask_matches("a*b*c", "axxbyy"));
    }

    #[test]
    fn session_files_and_titles() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.dcsession");
        let s = Session::new(SessionKind::FolderCompare, "/a/left", "/b/right");
        assert_eq!(s.title(), "left ↔ right");
        save_session_file(&p, &s).unwrap();
        assert_eq!(load_session_file(&p).unwrap(), s);
        assert!(load_session_file(&d.path().join("missing")).is_err());
    }

    #[test]
    fn saved_sessions_replace_by_name() {
        let mut st = Settings::default();
        st.save_session(Session { name: "B".into(), ..Default::default() });
        st.save_session(Session { name: "a".into(), ..Default::default() });
        st.save_session(Session { name: "B".into(), left: "x".into(), ..Default::default() });
        assert_eq!(st.saved.len(), 2);
        assert_eq!(st.saved[0].name, "a");
        assert_eq!(st.saved[1].left, "x");
    }
}
