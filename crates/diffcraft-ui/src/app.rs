//! The application shell: tabs, menus, keyboard shortcuts, dialogs, drag & drop and settings.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use diffcraft_core::detect::{self, ViewKind};
use diffcraft_session::{Session, SessionKind, Settings, Store, Theme};
use egui::{Key, KeyboardShortcut, Modifiers, RichText};

use crate::home::HomeView;
use crate::icons::{self, Icon};
use crate::theme::{self, Palette};
use crate::view::{Cmd, Cx, OpenRequest, ToastLevel, Toasts, View};
use crate::{folder_view, hex_view, merge_view, options, picture_view, sync_view, text_view, toolbar};

struct Tab {
    id: u64,
    view: Box<dyn View>,
}

/// Pending "close" that needs confirmation because of unsaved edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseTarget {
    Tab(u64),
    App,
}

pub struct DiffCraftApp {
    store: Option<Store>,
    settings: Settings,
    settings_changed: bool,
    applied_style: Option<(Theme, u32, u32)>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    toasts: Toasts,
    pending: Vec<OpenRequest>,
    exit_code: Arc<AtomicI32>,
    exit_code_local: i32,
    options: Option<options::OptionsState>,
    about: bool,
    shortcuts_help: bool,
    git_dialog: Option<GitDialog>,
    save_session_name: Option<String>,
    close_confirm: Option<CloseTarget>,
    allow_close: bool,
    window_title: String,
}

struct GitDialog {
    make_default: bool,
    result: Option<Result<(), String>>,
}

impl DiffCraftApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Vec<OpenRequest>, exit_code: Arc<AtomicI32>) -> Self {
        let store = Store::platform().ok();
        let (settings, warning) = store.as_ref().map(Store::load).unwrap_or_default();
        let mut toasts = Toasts::default();
        if let Some(w) = warning {
            toasts.warning(w);
        }
        theme::install_fallback_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &settings);
        let mut app = DiffCraftApp {
            store,
            settings,
            settings_changed: false,
            applied_style: None,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            toasts,
            pending: open,
            exit_code,
            exit_code_local: 0,
            options: None,
            about: false,
            shortcuts_help: false,
            git_dialog: None,
            save_session_name: None,
            close_confirm: None,
            allow_close: false,
            window_title: String::new(),
        };
        if app.pending.is_empty() {
            app.add_tab(Box::new(HomeView::default()));
        }
        app
    }

    fn add_tab(&mut self, view: Box<dyn View>) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab { id, view });
        self.active = self.tabs.len() - 1;
    }

    /// Turn an open request into a session (choosing the view for plain path pairs).
    fn session_for(&self, req: OpenRequest) -> Session {
        match req {
            OpenRequest::Session(s) => *s,
            OpenRequest::Home => Session::default(),
            OpenRequest::Paths { left, right, kind } => {
                let kind = kind.unwrap_or_else(|| match detect::view_for(&left, &right) {
                    ViewKind::Folder => SessionKind::FolderCompare,
                    ViewKind::Text => SessionKind::TextCompare,
                    ViewKind::Hex => SessionKind::HexCompare,
                    ViewKind::Picture => SessionKind::PictureCompare,
                });
                let mut s = Session::new(kind, path_string(&left), path_string(&right));
                s.text = self.settings.text_options_for(&left, &right);
                s.folder = self.settings.default_folder.clone();
                s
            }
            OpenRequest::Merge { left, right, base, output } => {
                let mut s = Session::new(SessionKind::TextMerge, path_string(&left), path_string(&right));
                s.base = Some(path_string(&base));
                s.output = output.map(|o| path_string(&o));
                s.text = self.settings.text_options_for(&left, &right);
                s
            }
        }
    }

    fn open_session(&mut self, ctx: &egui::Context, s: Session) {
        let has_paths = !s.left.is_empty() || !s.right.is_empty();
        let view: Box<dyn View> = match s.kind {
            SessionKind::TextCompare => Box::new(text_view::TextView::from_session(&s, &self.settings, ctx)),
            SessionKind::TextMerge => Box::new(merge_view::MergeView::from_session(&s, &self.settings, ctx)),
            SessionKind::FolderCompare => Box::new(folder_view::FolderView::from_session(&s, &self.settings, ctx)),
            SessionKind::FolderSync => Box::new(sync_view::SyncView::from_session(&s, &self.settings, ctx)),
            SessionKind::HexCompare => Box::new(hex_view::HexView::from_session(&s, &self.settings, ctx)),
            SessionKind::PictureCompare => Box::new(picture_view::PictureView::from_session(&s, &self.settings, ctx)),
        };
        if has_paths {
            self.settings.add_recent(s);
            self.settings_changed = true;
        }
        self.add_tab(view);
    }

    fn run_cmd(&mut self, ctx: &egui::Context, cmd: Cmd) {
        let active = self.active;
        let (settings, changed, toasts, open, exit) =
            (&mut self.settings, &mut self.settings_changed, &mut self.toasts, &mut self.pending, &mut self.exit_code_local);
        if let Some(tab) = self.tabs.get_mut(active)
            && tab.view.supports(cmd)
        {
            let mut cx = Cx { settings, settings_changed: changed, toasts, open, exit_code: exit };
            tab.view.run(cmd, ctx, &mut cx);
        }
    }

    fn supports(&self, cmd: Cmd) -> bool {
        self.tabs.get(self.active).is_some_and(|t| t.view.supports(cmd))
    }

    fn request_close_tab(&mut self, id: u64) {
        let dirty = self.tabs.iter().find(|t| t.id == id).is_some_and(|t| t.view.is_dirty());
        if dirty {
            self.close_confirm = Some(CloseTarget::Tab(id));
        } else {
            self.close_tab(id);
        }
    }

    fn close_tab(&mut self, id: u64) {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if i < self.active {
                self.active -= 1;
            }
        }
        if self.tabs.is_empty() {
            self.add_tab(Box::new(HomeView::default()));
        }
    }

    fn save_settings_if_needed(&mut self) {
        if !self.settings_changed {
            return;
        }
        self.settings_changed = false;
        if let Some(store) = &self.store
            && let Err(e) = store.save(&self.settings)
        {
            self.toasts.error(format!("Could not save settings: {e}"));
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input() || self.tabs.get(self.active).is_some_and(|t| t.view.wants_keyboard());
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let fire = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let mut cmds: Vec<Cmd> = Vec::new();
        // App-level.
        if fire(cmd, Key::T) {
            self.pending.push(OpenRequest::Home);
        }
        if fire(cmd, Key::W)
            && let Some(id) = self.tabs.get(self.active).map(|t| t.id)
        {
            self.request_close_tab(id);
        }
        if fire(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) && !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        } else if fire(Modifiers::CTRL, Key::Tab) && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if fire(cmd, Key::Plus) || fire(cmd, Key::Equals) {
            self.settings.font_size = (self.settings.font_size + 1.0).min(48.0);
            self.settings_changed = true;
        }
        if fire(cmd, Key::Minus) {
            self.settings.font_size = (self.settings.font_size - 1.0).max(6.0);
            self.settings_changed = true;
        }
        if fire(cmd, Key::Comma) {
            self.options = Some(options::OptionsState::new(&self.settings));
        }
        // View commands that are safe while typing.
        let table: &[(Modifiers, Key, Cmd)] = &[
            (cmd, Key::S, Cmd::Save),
            (cmd, Key::F, Cmd::Find),
            (Modifiers::NONE, Key::F3, Cmd::FindNext),
            (Modifiers::SHIFT, Key::F3, Cmd::FindPrev),
            (cmd, Key::G, Cmd::GotoLine),
            (shift_cmd, Key::N, Cmd::NextConflict),
            (shift_cmd, Key::P, Cmd::PrevConflict),
            (cmd, Key::N, Cmd::NextDiff),
            (cmd, Key::P, Cmd::PrevDiff),
            (Modifiers::ALT, Key::ArrowDown, Cmd::NextDiff),
            (Modifiers::ALT, Key::ArrowUp, Cmd::PrevDiff),
            (Modifiers::NONE, Key::F8, Cmd::NextDiff),
            (Modifiers::NONE, Key::F7, Cmd::PrevDiff),
            (cmd, Key::R, Cmd::CopyToRight),
            (cmd, Key::L, Cmd::CopyToLeft),
            (Modifiers::NONE, Key::F5, Cmd::Refresh),
            (Modifiers::ALT, Key::Num1, Cmd::TakeLeft),
            (Modifiers::ALT, Key::Num2, Cmd::TakeBase),
            (Modifiers::ALT, Key::Num3, Cmd::TakeRight),
            (Modifiers::ALT, Key::Num4, Cmd::TakeBoth),
        ];
        for &(m, k, c) in table {
            if fire(m, k) {
                cmds.push(c);
            }
        }
        if !typing {
            let quiet: &[(Modifiers, Key, Cmd)] = &[
                (shift_cmd, Key::Z, Cmd::Redo),
                (cmd, Key::Z, Cmd::Undo),
                (cmd, Key::Y, Cmd::Redo),
                (cmd, Key::A, Cmd::SelectAll),
                (cmd, Key::C, Cmd::Copy),
                (Modifiers::NONE, Key::Delete, Cmd::Delete),
                (Modifiers::NONE, Key::F2, Cmd::Rename),
                (Modifiers::NONE, Key::Enter, Cmd::OpenSelected),
            ];
            for &(m, k, c) in quiet {
                if fire(m, k) {
                    cmds.push(c);
                }
            }
        }
        for c in cmds {
            self.run_cmd(ctx, c);
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        match dropped.as_slice() {
            [] => {}
            [one] => {
                let kind = if one.is_dir() { SessionKind::FolderCompare } else { SessionKind::TextCompare };
                let s = Session::new(kind, path_string(one), String::new());
                let s = Session { text: self.settings.text_options_for(one, one), folder: self.settings.default_folder.clone(), ..s };
                self.open_session(ctx, s);
                self.toasts.info("Drop or choose the file for the right side.");
            }
            [l, r] => self.pending.push(OpenRequest::Paths { left: l.clone(), right: r.clone(), kind: None }),
            [l, base, r, ..] => self.pending.push(OpenRequest::Merge { left: l.clone(), right: r.clone(), base: base.clone(), output: None }),
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        egui::Panel::top("menu_bar").frame(egui::Frame::NONE.fill(pal.chrome_bg).inner_margin(egui::Margin::symmetric(4, 2))).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let (mark, _) = ui.allocate_exact_size(egui::vec2(20.0, 18.0), egui::Sense::hover());
                crate::brand::paint_mark(ui, egui::Rect::from_center_size(mark.center(), egui::vec2(17.0, 17.0)));
                ui.menu_button("Session", |ui| self.session_menu(ui, &ctx));
                ui.menu_button("File", |ui| self.file_menu(ui, &ctx));
                ui.menu_button("Edit", |ui| self.edit_menu(ui, &ctx));
                ui.menu_button("Search", |ui| {
                    self.item(ui, &ctx, Icon::Find, "Find…", "Ctrl+F", Cmd::Find);
                    self.item(ui, &ctx, Icon::NextDiff, "Find Next", "F3", Cmd::FindNext);
                    self.item(ui, &ctx, Icon::PrevDiff, "Find Previous", "Shift+F3", Cmd::FindPrev);
                    ui.separator();
                    self.item(ui, &ctx, Icon::ShowAll, "Go to Line…", "Ctrl+G", Cmd::GotoLine);
                });
                ui.menu_button("View", |ui| self.view_menu(ui, &ctx));
                ui.menu_button("Merge", |ui| {
                    self.item(ui, &ctx, Icon::CopyRight, "Take Left", "Alt+1", Cmd::TakeLeft);
                    self.item(ui, &ctx, Icon::Merge, "Take Base", "Alt+2", Cmd::TakeBase);
                    self.item(ui, &ctx, Icon::CopyLeft, "Take Right", "Alt+3", Cmd::TakeRight);
                    self.item(ui, &ctx, Icon::ShowAll, "Take Left then Right", "Alt+4", Cmd::TakeBoth);
                    ui.separator();
                    self.item(ui, &ctx, Icon::NextDiff, "Next Conflict", "Ctrl+Shift+N", Cmd::NextConflict);
                    self.item(ui, &ctx, Icon::PrevDiff, "Previous Conflict", "Ctrl+Shift+P", Cmd::PrevConflict);
                });
                ui.menu_button("Tools", |ui| {
                    self.item(ui, &ctx, Icon::Sync, "Synchronize Folders…", "", Cmd::Sync);
                    ui.separator();
                    if toolbar::menu_item(ui, Icon::Git, "Git Integration…", "", true) {
                        self.git_dialog = Some(GitDialog { make_default: true, result: None });
                        ui.close();
                    }
                    if toolbar::menu_item(ui, Icon::Settings, "Options…", &shortcut_label("Ctrl+,"), true) {
                        self.options = Some(options::OptionsState::new(&self.settings));
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if toolbar::menu_item(ui, Icon::Help, "Keyboard Shortcuts", "", true) {
                        self.shortcuts_help = true;
                        ui.close();
                    }
                    if toolbar::menu_item(ui, Icon::Home, "About DiffCraft", "", true) {
                        self.about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// A menu item bound to a view command (disabled when the view does not support it).
    fn item(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, icon: Icon, label: &str, shortcut: &str, cmd: Cmd) {
        let enabled = self.supports(cmd);
        if toolbar::menu_item(ui, icon, label, &shortcut_label(shortcut), enabled) {
            ui.close();
            self.run_cmd(ctx, cmd);
        }
    }

    fn session_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if toolbar::menu_item(ui, Icon::Home, "Home", &shortcut_label("Ctrl+T"), true) {
            self.pending.push(OpenRequest::Home);
            ui.close();
        }
        ui.menu_button("New", |ui| {
            for k in SessionKind::ALL {
                if toolbar::menu_item(ui, icons::session_icon(k), k.label(), "", true) {
                    let s = Session { kind: k, text: self.settings.default_text.clone(), folder: self.settings.default_folder.clone(), ..Session::default() };
                    self.open_session(ctx, s);
                    ui.close();
                }
            }
        });
        if toolbar::menu_item(ui, Icon::FolderOpen, "Open Session File…", "", true) {
            ui.close();
            if let Some(p) = rfd::FileDialog::new().add_filter("DiffCraft session", &[diffcraft_session::SESSION_EXTENSION]).pick_file() {
                match diffcraft_session::load_session_file(&p) {
                    Ok(s) => self.open_session(ctx, s),
                    Err(e) => self.toasts.error(e.to_string()),
                }
            }
        }
        ui.menu_button("Recent", |ui| {
            if self.settings.recent.is_empty() {
                ui.label(RichText::new("No recent sessions").weak());
            }
            let recent = self.settings.recent.clone();
            for s in recent.iter().take(15) {
                if toolbar::menu_item(ui, icons::session_icon(s.kind), &s.title(), "", true) {
                    self.open_session(ctx, s.clone());
                    ui.close();
                }
            }
        });
        ui.separator();
        let session = self.tabs.get(self.active).and_then(|t| t.view.session());
        if toolbar::menu_item(ui, Icon::Save, "Save Session…", "", session.is_some()) {
            ui.close();
            self.save_session_name = Some(session.as_ref().map(Session::title).unwrap_or_default());
        }
        if toolbar::menu_item(ui, Icon::Report, "Export Session File…", "", session.is_some()) {
            ui.close();
            if let Some(s) = &session
                && let Some(p) = crate::widgets::save_file(
                    None,
                    &format!("session.{}", diffcraft_session::SESSION_EXTENSION),
                    Some(("DiffCraft session", &[diffcraft_session::SESSION_EXTENSION])),
                )
            {
                match diffcraft_session::save_session_file(&p, s) {
                    Ok(()) => self.toasts.success(format!("Session saved to {}", p.display())),
                    Err(e) => self.toasts.error(e.to_string()),
                }
            }
        }
        ui.separator();
        if toolbar::menu_item(ui, Icon::Close, "Close Tab", &shortcut_label("Ctrl+W"), true) {
            ui.close();
            if let Some(id) = self.tabs.get(self.active).map(|t| t.id) {
                self.request_close_tab(id);
            }
        }
        if toolbar::menu_item(ui, Icon::Close, "Exit", "", true) {
            ui.close();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn file_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.item(ui, ctx, Icon::Save, "Save", "Ctrl+S", Cmd::Save);
        self.item(ui, ctx, Icon::Save, "Save Left As…", "", Cmd::SaveLeftAs);
        self.item(ui, ctx, Icon::Save, "Save Right As…", "", Cmd::SaveRightAs);
        ui.separator();
        self.item(ui, ctx, Icon::Edit, "Edit Left File…", "", Cmd::EditLeftWhole);
        self.item(ui, ctx, Icon::Edit, "Edit Right File…", "", Cmd::EditRightWhole);
        ui.separator();
        self.item(ui, ctx, Icon::Report, "HTML Report…", "", Cmd::ExportHtmlReport);
        self.item(ui, ctx, Icon::Report, "Unified Patch…", "", Cmd::ExportPatch);
        self.item(ui, ctx, Icon::Report, "CSV Report…", "", Cmd::ExportCsv);
        ui.separator();
        self.item(ui, ctx, Icon::Reload, "Reload", "F5", Cmd::Refresh);
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.item(ui, ctx, Icon::Undo, "Undo", "Ctrl+Z", Cmd::Undo);
        self.item(ui, ctx, Icon::Redo, "Redo", "Ctrl+Y", Cmd::Redo);
        ui.separator();
        self.item(ui, ctx, Icon::ShowAll, "Copy", "Ctrl+C", Cmd::Copy);
        self.item(ui, ctx, Icon::ShowAll, "Select All", "Ctrl+A", Cmd::SelectAll);
        ui.separator();
        self.item(ui, ctx, Icon::CopyRight, "Copy to Right", "Ctrl+R", Cmd::CopyToRight);
        self.item(ui, ctx, Icon::CopyLeft, "Copy to Left", "Ctrl+L", Cmd::CopyToLeft);
        ui.separator();
        self.item(ui, ctx, Icon::Delete, "Delete", "Del", Cmd::Delete);
        self.item(ui, ctx, Icon::Delete, "Delete Left", "", Cmd::DeleteLeft);
        self.item(ui, ctx, Icon::Delete, "Delete Right", "", Cmd::DeleteRight);
        self.item(ui, ctx, Icon::Edit, "Rename…", "F2", Cmd::Rename);
        self.item(ui, ctx, Icon::Folder, "New Folder…", "", Cmd::NewFolder);
    }

    fn view_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.item(ui, ctx, Icon::NextDiff, "Next Difference", "Ctrl+N", Cmd::NextDiff);
        self.item(ui, ctx, Icon::PrevDiff, "Previous Difference", "Ctrl+P", Cmd::PrevDiff);
        self.item(ui, ctx, Icon::FirstDiff, "First Difference", "", Cmd::FirstDiff);
        self.item(ui, ctx, Icon::LastDiff, "Last Difference", "", Cmd::LastDiff);
        ui.separator();
        self.item(ui, ctx, Icon::ExpandAll, "Expand All", "", Cmd::ExpandAll);
        self.item(ui, ctx, Icon::CollapseAll, "Collapse All", "", Cmd::CollapseAll);
        self.item(ui, ctx, Icon::Swap, "Swap Sides", "", Cmd::SwapSides);
        ui.separator();
        ui.menu_button("Theme", |ui| {
            for t in Theme::ALL {
                if ui.radio(self.settings.theme == t, t.label()).clicked() {
                    self.settings.theme = t;
                    self.settings_changed = true;
                    ui.close();
                }
            }
        });
        if toolbar::menu_item(ui, Icon::ZoomIn, "Larger Text", &shortcut_label("Ctrl++"), true) {
            self.settings.font_size = (self.settings.font_size + 1.0).min(48.0);
            self.settings_changed = true;
        }
        if toolbar::menu_item(ui, Icon::ZoomOut, "Smaller Text", &shortcut_label("Ctrl+-"), true) {
            self.settings.font_size = (self.settings.font_size - 1.0).max(6.0);
            self.settings_changed = true;
        }
        if ui.checkbox(&mut self.settings.show_line_numbers, "Line Numbers").changed() {
            self.settings_changed = true;
        }
    }

    /// Document tabs: icon, title, unsaved dot and close button; the active tab joins the toolbar below.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let pal = Palette::get(ui.ctx());
        egui::Panel::top("tab_bar").frame(egui::Frame::NONE.fill(pal.chrome_bg).inner_margin(egui::Margin { left: 4, right: 4, top: 3, bottom: 0 })).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut close = None;
                    let mut activate = None;
                    let font = egui::FontId::proportional(12.5);
                    for (i, t) in self.tabs.iter().enumerate() {
                        let selected = i == self.active;
                        let title = crate::jobs::shorten(&t.view.title(), 42);
                        let tw = ui.fonts_mut(|f| f.layout_no_wrap(title.clone(), font.clone(), pal.text).size().x);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(tw + 64.0, 28.0), egui::Sense::click());
                        let p = ui.painter();
                        let bg = if selected {
                            pal.toolbar_bg
                        } else if resp.hovered() {
                            pal.hover_bg
                        } else {
                            pal.chrome_bg
                        };
                        p.rect_filled(rect, egui::CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, bg);
                        if selected {
                            p.line_segment(
                                [rect.left_top() + egui::vec2(4.0, 0.0), rect.right_top() - egui::vec2(4.0, 0.0)],
                                egui::Stroke::new(2.0, pal.accent),
                            );
                        }
                        let icon = if t.view.is_home() { Icon::Home } else { icons::session_icon(t.view.kind()) };
                        icons::paint(
                            p,
                            egui::Rect::from_center_size(egui::pos2(rect.left() + 16.0, rect.center().y), egui::vec2(15.0, 15.0)),
                            icon,
                            &pal,
                            true,
                        );
                        let tc = if selected { pal.text } else { pal.dim_text };
                        p.text(egui::pos2(rect.left() + 28.0, rect.center().y), egui::Align2::LEFT_CENTER, &title, font.clone(), tc);
                        let close_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(16.0, 16.0));
                        let cr = ui.interact(close_rect, ui.id().with(("tab-close", t.id)), egui::Sense::click()).on_hover_text("Close tab (Ctrl+W)");
                        if t.view.is_dirty() && !cr.hovered() {
                            ui.painter().circle_filled(close_rect.center(), 3.5, pal.accent);
                        } else if selected || resp.hovered() || cr.hovered() {
                            if cr.hovered() {
                                ui.painter().rect_filled(close_rect, 3.0, pal.hover_bg);
                            }
                            icons::paint(ui.painter(), close_rect.shrink(2.0), Icon::Close, &pal, true);
                        }
                        let tip = if t.view.is_home() { t.view.tooltip() } else { format!("{}\n{}", t.view.kind().label(), t.view.tooltip()) };
                        let resp = resp.on_hover_text(tip);
                        if cr.clicked() || resp.middle_clicked() {
                            close = Some(t.id);
                        } else if resp.clicked() {
                            activate = Some(i);
                        }
                    }
                    if toolbar::small(ui, Icon::Plus, "New tab (Ctrl+T)", true) {
                        self.add_tab(Box::new(HomeView::default()));
                    }
                    if let Some(i) = activate {
                        self.active = i;
                    }
                    if let Some(id) = close {
                        self.request_close_tab(id);
                    }
                });
            },
        );
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(state) = &mut self.options {
            match options::show(ctx, state) {
                options::Outcome::Open => {}
                options::Outcome::Apply(s) => {
                    self.settings = *s;
                    self.settings_changed = true;
                    self.options = None;
                }
                options::Outcome::Cancel => self.options = None,
            }
        }
        if self.about {
            egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                crate::brand::wordmark(ui, 44.0);
                ui.add_space(4.0);
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label("Compare and merge files, folders, binaries and pictures.");
                ui.label("Free and open source under the MIT or Apache-2.0 licence. No activation, no telemetry.");
                ui.add_space(4.0);
                if let Some(store) = &self.store {
                    ui.label(RichText::new(format!("Settings: {}", store.settings_path().display())).small().weak());
                }
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    self.about = false;
                }
            });
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.about = false;
            }
        }
        if self.shortcuts_help {
            let mut open = true;
            egui::Window::new("Keyboard Shortcuts").open(&mut open).collapsible(false).resizable(true).show(ctx, |ui| {
                egui::Grid::new("keys").striped(true).show(ui, |ui| {
                    for (k, what) in SHORTCUTS {
                        ui.monospace(shortcut_label(k));
                        ui.label(*what);
                        ui.end_row();
                    }
                });
            });
            self.shortcuts_help = open;
        }
        if let Some(name) = &mut self.save_session_name {
            let mut done = None;
            egui::Modal::new(egui::Id::new("save-session")).show(ctx, |ui| {
                ui.heading("Save Session");
                ui.label("Name:");
                let r = ui.text_edit_singleline(name);
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    let name = name.trim().to_owned();
                    if let Some(mut s) = self.tabs.get(self.active).and_then(|t| t.view.session())
                        && !name.is_empty()
                    {
                        s.name = name.clone();
                        self.settings.save_session(s);
                        self.settings_changed = true;
                        self.toasts.success(format!("Saved session \"{name}\""));
                    }
                    self.save_session_name = None;
                }
                Some(false) => self.save_session_name = None,
                None => {}
            }
        }
        if let Some(g) = &mut self.git_dialog {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("diffcraft"));
            let mut open = true;
            egui::Window::new("Git Integration").open(&mut open).collapsible(false).default_width(560.0).show(ctx, |ui| {
                ui.label("Use DiffCraft for `git difftool` and `git mergetool`.");
                ui.checkbox(&mut g.make_default, "Make DiffCraft the default diff and merge tool");
                ui.add_space(4.0);
                let cmds = diffcraft_session::git::shell_commands(&exe, g.make_default);
                ui.add(egui::TextEdit::multiline(&mut cmds.as_str()).code_editor().desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    if ui.button("Copy Commands").clicked() {
                        ui.ctx().copy_text(cmds.clone());
                    }
                    if ui.button("Apply to Global Git Config").clicked() {
                        g.result = Some(diffcraft_session::git::apply_global(&exe, g.make_default));
                    }
                });
                match &g.result {
                    Some(Ok(())) => {
                        ui.colored_label(Palette::get(ui.ctx()).success, "Done. Try `git difftool` or `git mergetool`.");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(Palette::get(ui.ctx()).error, e);
                    }
                    None => {}
                }
            });
            if !open {
                self.git_dialog = None;
            }
        }
        if let Some(target) = self.close_confirm {
            let names: Vec<String> = match target {
                CloseTarget::Tab(id) => self.tabs.iter().filter(|t| t.id == id).map(|t| t.view.title()).collect(),
                CloseTarget::App => self.tabs.iter().filter(|t| t.view.is_dirty()).map(|t| t.view.title()).collect(),
            };
            let mut choice = None;
            egui::Modal::new(egui::Id::new("close-confirm")).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.heading("Save changes?");
                ui.label("These comparisons have unsaved edits:");
                for n in &names {
                    ui.label(format!("  • {n}"));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Don't Save").clicked() {
                        choice = Some(2);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        choice = Some(0);
                    }
                });
            });
            match choice {
                Some(0) => self.close_confirm = None,
                Some(c) => {
                    self.close_confirm = None;
                    let ids: Vec<u64> = match target {
                        CloseTarget::Tab(id) => vec![id],
                        CloseTarget::App => self.tabs.iter().filter(|t| t.view.is_dirty()).map(|t| t.id).collect(),
                    };
                    let mut ok = true;
                    if c == 1 {
                        let (settings, changed, toasts, open, exit) =
                            (&mut self.settings, &mut self.settings_changed, &mut self.toasts, &mut self.pending, &mut self.exit_code_local);
                        let mut cx = Cx { settings, settings_changed: changed, toasts, open, exit_code: exit };
                        for t in self.tabs.iter_mut().filter(|t| ids.contains(&t.id)) {
                            ok &= t.view.save_all(&mut cx);
                        }
                    }
                    if ok {
                        match target {
                            CloseTarget::Tab(id) => self.close_tab(id),
                            CloseTarget::App => {
                                self.allow_close = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    }
                }
                None => {}
            }
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let pal = Palette::get(ctx);
        self.toasts.set_time(ctx.input(|i| i.time));
        if self.toasts.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0)).interactable(true).show(ctx, |ui| {
            let mut dismiss = None;
            for (i, t) in self.toasts.items.iter().enumerate() {
                let color = match t.level {
                    ToastLevel::Info => pal.accent,
                    ToastLevel::Success => pal.success,
                    ToastLevel::Warning => pal.warning,
                    ToastLevel::Error => pal.error,
                };
                egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.0, color)).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    if ui.add(egui::Label::new(RichText::new(&t.text).color(pal.text)).sense(egui::Sense::click())).on_hover_text("Click to dismiss").clicked()
                    {
                        dismiss = Some(i);
                    }
                });
                ui.add_space(4.0);
            }
            if let Some(i) = dismiss {
                self.toasts.items.remove(i);
            }
        });
    }
}

impl eframe::App for DiffCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let style_key = (self.settings.theme, self.settings.font_size.to_bits(), self.settings.ui_scale.to_bits());
        if self.applied_style != Some(style_key) {
            theme::apply(ctx, &self.settings);
            self.applied_style = Some(style_key);
        }
        for req in std::mem::take(&mut self.pending) {
            if let OpenRequest::Home = req {
                match self.tabs.iter().position(|t| t.view.is_home()) {
                    Some(i) => self.active = i,
                    None => self.add_tab(Box::new(HomeView::default())),
                }
                continue;
            }
            let s = self.session_for(req);
            self.open_session(ctx, s);
        }
        // Window title follows the active tab.
        let title = match self.tabs.get(self.active) {
            Some(t) if !t.view.is_home() => format!("{} - {} - DiffCraft", t.view.title(), t.view.kind().label()),
            _ => "Home - DiffCraft".to_owned(),
        };
        if self.window_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
        self.handle_drops(ctx);
        self.shortcuts(ctx);
        // Closing the window with unsaved edits asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.tabs.iter().any(|t| t.view.is_dirty()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_confirm = Some(CloseTarget::App);
        }
        self.exit_code.store(self.exit_code_local, Ordering::Relaxed);
        self.save_settings_if_needed();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.menu_bar(ui);
        self.tab_bar(ui);
        let active = self.active;
        let (settings, changed, toasts, open, exit) =
            (&mut self.settings, &mut self.settings_changed, &mut self.toasts, &mut self.pending, &mut self.exit_code_local);
        let pal = Palette::get(&ctx);
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(pal.panel_bg)).show(ui, |ui| {
            if let Some(tab) = self.tabs.get_mut(active) {
                let mut cx = Cx { settings, settings_changed: changed, toasts, open, exit_code: exit };
                let id = tab.id;
                ui.push_id(id, |ui| tab.view.ui(ui, &mut cx));
            }
        });
        self.dialogs(&ctx);
        self.toasts_ui(&ctx);
        self.exit_code.store(self.exit_code_local, Ordering::Relaxed);
        self.save_settings_if_needed();
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.settings_changed = true;
        self.save_settings_if_needed();
    }
}

pub fn path_string(p: &Path) -> String {
    p.display().to_string()
}

/// Show `Ctrl` as `Cmd` on macOS.
pub fn shortcut_label(s: &str) -> String {
    if cfg!(target_os = "macos") { s.replace("Ctrl+", "⌘").replace("Alt+", "⌥") } else { s.to_owned() }
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+T", "New tab (Home)"),
    ("Ctrl+W", "Close tab"),
    ("Ctrl+Tab", "Next tab"),
    ("Ctrl+S", "Save"),
    ("Ctrl+N / F8 / Alt+Down", "Next difference"),
    ("Ctrl+P / F7 / Alt+Up", "Previous difference"),
    ("Ctrl+Shift+N", "Next conflict (merge)"),
    ("Ctrl+Shift+P", "Previous conflict (merge)"),
    ("Ctrl+R", "Copy to right"),
    ("Ctrl+L", "Copy to left"),
    ("Alt+1 / 2 / 3 / 4", "Merge: take left / base / right / both"),
    ("Ctrl+Z", "Undo"),
    ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
    ("Ctrl+F", "Find"),
    ("F3 / Shift+F3", "Find next / previous"),
    ("Ctrl+G", "Go to line"),
    ("F5", "Refresh / recompare"),
    ("Del", "Delete selection (folder view)"),
    ("F2", "Rename (folder view)"),
    ("Enter", "Open selected item"),
    ("Double-click line", "Edit the line (text view)"),
    ("Ctrl++ / Ctrl+-", "Larger / smaller text"),
    ("Ctrl+,", "Options"),
];
