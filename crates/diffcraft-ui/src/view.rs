//! The contract between the app shell (tabs, menus, shortcuts) and each comparison view.

use std::path::PathBuf;

use diffcraft_session::{Session, SessionKind, Settings};

/// Commands the shell routes to the active view (from menus, toolbars and shortcuts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    Save,
    SaveLeftAs,
    SaveRightAs,
    Refresh,
    NextDiff,
    PrevDiff,
    FirstDiff,
    LastDiff,
    NextConflict,
    PrevConflict,
    CopyToRight,
    CopyToLeft,
    Undo,
    Redo,
    Find,
    FindNext,
    FindPrev,
    GotoLine,
    SwapSides,
    SelectAll,
    Copy,
    /// Delete the selection on the side that has focus.
    Delete,
    DeleteLeft,
    DeleteRight,
    Rename,
    NewFolder,
    ExpandAll,
    CollapseAll,
    ExportHtmlReport,
    ExportPatch,
    ExportCsv,
    EditLeftWhole,
    EditRightWhole,
    TakeLeft,
    TakeRight,
    TakeBase,
    TakeBoth,
    Sync,
    OpenSelected,
}

/// Something the shell should open in a new tab.
#[derive(Clone, Debug)]
pub enum OpenRequest {
    Session(Box<Session>),
    /// Open two paths, choosing the view from their contents unless `kind` is given.
    Paths {
        left: PathBuf,
        right: PathBuf,
        kind: Option<SessionKind>,
    },
    /// Show the Home tab (activating an existing one).
    Home,
    /// Three-way merge.
    Merge {
        left: PathBuf,
        right: PathBuf,
        base: PathBuf,
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub level: ToastLevel,
    /// Seconds (egui time) when the toast disappears.
    pub until: f64,
}

/// Transient notifications, bottom-right.
#[derive(Default, Debug)]
pub struct Toasts {
    pub items: Vec<Toast>,
    now: f64,
}

impl Toasts {
    pub fn set_time(&mut self, now: f64) {
        self.now = now;
        self.items.retain(|t| t.until > now);
    }

    pub fn push(&mut self, level: ToastLevel, text: impl Into<String>) {
        let secs = match level {
            ToastLevel::Error => 8.0,
            ToastLevel::Warning => 6.0,
            _ => 3.5,
        };
        let text = text.into();
        log::info!("{level:?}: {text}");
        self.items.push(Toast { text, level, until: self.now + secs });
        if self.items.len() > 6 {
            self.items.remove(0);
        }
    }

    pub fn info(&mut self, t: impl Into<String>) {
        self.push(ToastLevel::Info, t);
    }
    pub fn success(&mut self, t: impl Into<String>) {
        self.push(ToastLevel::Success, t);
    }
    pub fn warning(&mut self, t: impl Into<String>) {
        self.push(ToastLevel::Warning, t);
    }
    pub fn error(&mut self, t: impl Into<String>) {
        self.push(ToastLevel::Error, t);
    }
}

/// Shared state a view may use while drawing or running a command.
pub struct Cx<'a> {
    pub settings: &'a mut Settings,
    /// Set when `settings` changed and should be saved.
    pub settings_changed: &'a mut bool,
    pub toasts: &'a mut Toasts,
    pub open: &'a mut Vec<OpenRequest>,
    /// Process exit code (used by `git mergetool`: 0 = merged and saved).
    pub exit_code: &'a mut i32,
}

/// A comparison view living in a tab.
pub trait View {
    fn kind(&self) -> SessionKind;
    /// Tab title.
    fn title(&self) -> String;
    /// Tab tooltip (full paths).
    fn tooltip(&self) -> String {
        self.title()
    }
    /// Draw the view into the tab's area.
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx<'_>);
    /// Whether the command applies to this view right now (menus grey out the rest).
    fn supports(&self, cmd: Cmd) -> bool;
    fn run(&mut self, cmd: Cmd, ui: &egui::Context, cx: &mut Cx<'_>);
    /// Unsaved edits.
    fn is_dirty(&self) -> bool {
        false
    }
    /// Save all unsaved edits. Returns false if anything failed (errors go to toasts).
    fn save_all(&mut self, _cx: &mut Cx<'_>) -> bool {
        true
    }
    /// The session describing this view (for recent/saved sessions).
    fn session(&self) -> Option<Session>;
    /// The launcher tab (replaced when a comparison opens).
    fn is_home(&self) -> bool {
        false
    }
    /// The view has a text field focused (so single-key shortcuts must not fire).
    fn wants_keyboard(&self) -> bool {
        false
    }
}
