//! Text Compare: two editable files side by side with aligned rows, intra-line highlights,
//! copy left/right, inline line editing, undo/redo, find, go to line, reports and saving.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use diffcraft_core::text::file::{LineEnding, MAX_TEXT_BYTES, TextEncoding, TextFile, is_probably_binary};
use diffcraft_core::text::intraline::intraline;
use diffcraft_core::text::{self, RowKind, TextCompareOptions};
use diffcraft_session::{Session, SessionKind, Settings};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Align2, Color32, FontId, Key, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::icons::{self, Icon};
use crate::jobs::{self, Job};
use crate::text_model::{Display, Side, TextModel, Vis};
use crate::theme::{self, Palette};
use crate::toolbar;
use crate::view::{Cmd, Cx, OpenRequest, View};
use crate::widgets;

/// Longest line prefix rendered (longer lines are cut for display only).
const MAX_DISPLAY_CHARS: usize = 4000;
const SIDE_NAMES: [&str; 2] = ["Left", "Right"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    side: usize,
    anchor: usize,
    cursor: usize,
}

impl Selection {
    fn rows(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor) + 1
    }
}

struct LineEdit {
    side: usize,
    row: usize,
    buffer: String,
    focus: bool,
    cursor_at_start: bool,
}

#[derive(Default)]
struct FindBar {
    open: bool,
    query: String,
    match_case: bool,
    hit: Option<(usize, usize)>,
    focus: bool,
    not_found: bool,
}

struct WholeEdit {
    side: usize,
    text: String,
}

/// Movement after committing a line edit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    Stay,
    Up,
    Down,
    /// Enter: split the line at this character index and edit the new line.
    Split(usize),
}

/// Deferred UI actions (collected while painting, applied afterwards).
enum Action {
    Select { row: usize, side: usize, extend: bool },
    DragTo(usize),
    StartEdit { row: usize, side: usize },
    Commit(Move),
    Cancel,
    CopyRows { rows: Range<usize>, to_right: bool },
    DeleteRows { rows: Range<usize>, side: usize },
    InsertLine { row: usize, side: usize, below: bool },
    CopyText { rows: Range<usize>, side: usize },
    ScrollToFrac(f32),
    ExpandGap,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Reload([bool; 2]),
}

/// Intra-line highlight ranges (left, right) of one row.
type Highlights = (Vec<Range<usize>>, Vec<Range<usize>>);

pub struct TextView {
    paths: [String; 2],
    opts: TextCompareOptions,
    model: Option<TextModel>,
    load: Option<Job<TextModel>>,
    load_error: Option<String>,
    binary_hint: bool,
    current: Option<usize>,
    sel: Option<Selection>,
    dragging: bool,
    edit: Option<LineEdit>,
    find: FindBar,
    goto: Option<String>,
    goto_focus: bool,
    h_scroll: f32,
    content_w: f32,
    scroll_to_vis: Option<usize>,
    view_frac: (f32, f32),
    intra: HashMap<usize, Highlights>,
    intra_rev: u64,
    disk_changed: [bool; 2],
    last_disk_check: f64,
    whole: Option<WholeEdit>,
    confirm: Option<Pending>,
    tab_width: usize,
    show_minor: bool,
    load_started: Option<std::time::Instant>,
    load_time: Option<f32>,
}

impl TextView {
    pub fn from_session(s: &Session, settings: &Settings, ctx: &egui::Context) -> Self {
        let tab_width = settings.rule_for(Path::new(&s.left), Path::new(&s.right)).and_then(|r| r.tab_width).unwrap_or(settings.tab_width);
        let mut v = TextView {
            paths: [s.left.clone(), s.right.clone()],
            opts: s.text.clone(),
            model: None,
            load: None,
            load_error: None,
            binary_hint: false,
            current: None,
            sel: None,
            dragging: false,
            edit: None,
            find: FindBar::default(),
            goto: None,
            goto_focus: false,
            h_scroll: 0.0,
            content_w: 0.0,
            scroll_to_vis: None,
            view_frac: (0.0, 1.0),
            intra: HashMap::new(),
            intra_rev: 0,
            disk_changed: [false; 2],
            last_disk_check: 0.0,
            whole: None,
            confirm: None,
            tab_width,
            show_minor: true,
            load_started: None,
            load_time: None,
        };
        v.reload(ctx, [true, true]);
        v
    }

    fn reload(&mut self, ctx: &egui::Context, which: [bool; 2]) {
        self.edit = None;
        self.load_error = None;
        self.binary_hint = false;
        let old: [Option<Side>; 2] = match &self.model {
            Some(m) => [(!which[0]).then(|| m.sides[0].clone()), (!which[1]).then(|| m.sides[1].clone())],
            None => [None, None],
        };
        let paths = self.paths.clone();
        let opts = self.opts.clone();
        let display = self.model.as_ref().map(|m| m.display).unwrap_or_default();
        self.load_started = Some(std::time::Instant::now());
        self.load = Some(Job::spawn("Loading files", ctx, move |_p| {
            let [o0, o1] = old;
            let [p0, p1] = paths;
            let left = match o0 {
                Some(s) => s,
                None => load_side(&p0)?,
            };
            let right = match o1 {
                Some(s) => s,
                None => load_side(&p1)?,
            };
            let mut m = TextModel::new([left, right], opts);
            m.set_display(display);
            Ok(m)
        }));
        for (i, w) in which.iter().enumerate() {
            if *w && let Some(d) = self.disk_changed.get_mut(i) {
                *d = false;
            }
        }
    }

    fn poll(&mut self) {
        let Some(job) = &mut self.load else { return };
        let Some(res) = job.poll() else { return };
        self.load = None;
        self.load_time = self.load_started.take().map(|t| t.elapsed().as_secs_f32());
        match res {
            Ok(m) => {
                let first = m.next_block(None, true);
                let keep = self.current.is_some_and(|c| c < m.blocks.len());
                self.model = Some(m);
                self.intra.clear();
                if !keep {
                    self.current = None;
                    if let Some(b) = first {
                        self.select_block(b);
                    }
                }
            }
            Err(e) => {
                self.binary_hint = e.contains("binary");
                self.load_error = Some(e);
            }
        }
    }

    fn is_loading(&self) -> bool {
        self.load.is_some()
    }

    /// Ask before discarding edits, else reload now.
    fn request_reload(&mut self, ctx: &egui::Context, which: [bool; 2]) {
        let dirty = self.model.as_ref().is_some_and(|m| m.sides.iter().zip(which).any(|(s, w)| w && s.dirty));
        if dirty {
            self.confirm = Some(Pending::Reload(which));
        } else {
            self.reload(ctx, which);
        }
    }

    fn check_disk(&mut self, ctx: &egui::Context, settings: &Settings, toasts: &mut crate::view::Toasts) {
        let now = ctx.input(|i| i.time);
        if now - self.last_disk_check < 2.0 || self.is_loading() {
            return;
        }
        self.last_disk_check = now;
        let Some(m) = &self.model else { return };
        let mut reload = [false; 2];
        for (i, s) in m.sides.iter().enumerate() {
            let Some(p) = &s.path else { continue };
            let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
            if mtime.is_some() && mtime != s.mtime {
                if !s.dirty && settings.auto_reload {
                    if let Some(r) = reload.get_mut(i) {
                        *r = true;
                    }
                } else if let Some(d) = self.disk_changed.get_mut(i) {
                    *d = true;
                }
            }
        }
        if reload.iter().any(|r| *r) {
            toasts.info("A file changed on disk and was reloaded.");
            self.reload(ctx, reload);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }

    fn save_side(&mut self, side: usize, save_as: bool, cx: &mut Cx<'_>) -> bool {
        let Some(m) = &mut self.model else { return false };
        let Some(s) = m.sides.get_mut(side) else { return false };
        let target = match (&s.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            (p, _) => {
                let name = p.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled.txt".into());
                widgets::save_file(p.as_ref().and_then(|p| p.parent()), &name, None)
            }
        };
        let Some(path) = target else { return false };
        if cx.settings.backup_on_save && path.exists() {
            let mut bak = path.clone().into_os_string();
            bak.push(".bak");
            if let Err(e) = std::fs::copy(&path, PathBuf::from(bak)) {
                cx.toasts.warning(format!("Could not write a backup: {e}"));
            }
        }
        match s.file.save(&path) {
            Ok(()) => {
                s.dirty = false;
                s.exists = true;
                s.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                s.path = Some(path.clone());
                if let Some(p) = self.paths.get_mut(side) {
                    *p = path.display().to_string();
                }
                if let Some(d) = self.disk_changed.get_mut(side) {
                    *d = false;
                }
                cx.toasts.success(format!("Saved {}", path.display()));
                true
            }
            Err(e) => {
                cx.toasts.error(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn select_block(&mut self, b: usize) {
        let Some(m) = &self.model else { return };
        let Some(block) = m.blocks.get(b) else { return };
        self.current = Some(b);
        self.sel = Some(Selection { side: self.sel.map_or(0, |s| s.side), anchor: block.start, cursor: block.end.saturating_sub(1) });
        self.scroll_to_vis = m.vis_index_of_row(block.start);
    }

    fn navigate(&mut self, forward: bool, edge: bool) {
        let Some(m) = &self.model else { return };
        let from = if edge {
            None
        } else {
            match (self.current.and_then(|c| m.blocks.get(c)), self.sel) {
                (Some(b), _) if forward => Some(b.end.saturating_sub(1)),
                (Some(b), _) => Some(b.start),
                (None, Some(s)) => Some(s.cursor),
                (None, None) => None,
            }
        };
        if let Some(b) = m.next_block(from, forward) {
            self.select_block(b);
        }
    }

    /// Rows a copy/delete command applies to: a multi-row selection, else the current block.
    fn target_rows(&self) -> Option<Range<usize>> {
        let m = self.model.as_ref()?;
        if let Some(s) = self.sel
            && s.rows().len() > 1
        {
            return Some(s.rows());
        }
        if let Some(s) = self.sel
            && let Some(b) = m.block_of_row(s.cursor)
        {
            return m.blocks.get(b).cloned();
        }
        self.current.and_then(|c| m.blocks.get(c).cloned())
    }

    fn start_edit(&mut self, row: usize, side: usize) {
        let Some(m) = &self.model else { return };
        let Some(r) = m.rows.get(row) else { return };
        let line = if side == 0 { r.left } else { r.right };
        let buffer = line.and_then(|l| m.line(side, l)).unwrap_or_default().to_owned();
        self.edit = Some(LineEdit { side, row, buffer, focus: true, cursor_at_start: false });
        self.sel = Some(Selection { side, anchor: row, cursor: row });
    }

    fn commit_edit(&mut self, mv: Move) {
        let Some(ed) = self.edit.take() else { return };
        let Some(m) = &mut self.model else { return };
        let Some(r) = m.rows.get(ed.row).copied() else { return };
        let line = if ed.side == 0 { r.left } else { r.right };
        let (at, len, existed) = match line {
            Some(l) => (l, 1, true),
            None => (m.insertion_point(ed.side, ed.row), 0, false),
        };
        let mut new_line_index = at;
        match mv {
            Move::Split(c) => {
                let split = ed.buffer.char_indices().nth(c).map_or(ed.buffer.len(), |(i, _)| i);
                let (before, after) = ed.buffer.split_at(split);
                m.splice(ed.side, at..at + len, vec![before.to_owned(), after.to_owned()], false);
                new_line_index = at + 1;
            }
            _ => {
                let unchanged = existed && m.line(ed.side, at) == Some(ed.buffer.as_str());
                if !unchanged && (existed || !ed.buffer.is_empty()) {
                    m.splice(ed.side, at..at + len, vec![ed.buffer.clone()], false);
                }
            }
        }
        // Where to continue editing.
        let next_row = match mv {
            Move::Stay => None,
            Move::Split(_) => m.row_of_line.get(ed.side).and_then(|v| v.get(new_line_index)).copied(),
            Move::Up | Move::Down => {
                let row = if existed || !ed.buffer.is_empty() { m.row_of_line.get(ed.side).and_then(|v| v.get(at)).copied().unwrap_or(ed.row) } else { ed.row };
                let vi = m.vis_index_of_row(row).unwrap_or(0);
                let target = if mv == Move::Up { vi.checked_sub(1) } else { Some(vi + 1) };
                target.and_then(|t| m.visible.get(t)).and_then(|v| match v {
                    Vis::Row(r) => Some(*r),
                    Vis::Gap { .. } => None,
                })
            }
        };
        if let Some(row) = next_row {
            self.start_edit(row, ed.side);
            if let (Move::Split(_), Some(e)) = (mv, &mut self.edit) {
                e.cursor_at_start = true;
            }
            self.scroll_to_vis_if_needed(row);
        }
    }

    fn scroll_to_vis_if_needed(&mut self, row: usize) {
        let Some(m) = &self.model else { return };
        let Some(vi) = m.vis_index_of_row(row) else { return };
        let n = m.visible.len().max(1) as f32;
        let f = vi as f32 / n;
        let margin = 1.5 / n;
        if f < self.view_frac.0 || f + margin > self.view_frac.1 {
            self.scroll_to_vis = Some(vi);
        }
    }

    fn apply(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for a in actions {
            match a {
                Action::Select { row, side, extend } => {
                    if self.edit.is_some() {
                        self.commit_edit(Move::Stay);
                    }
                    self.sel = Some(match (self.sel, extend) {
                        (Some(s), true) => Selection { side: s.side, anchor: s.anchor, cursor: row },
                        _ => Selection { side, anchor: row, cursor: row },
                    });
                    if let Some(m) = &self.model
                        && let Some(b) = m.block_of_row(row)
                    {
                        self.current = Some(b);
                    }
                }
                Action::DragTo(row) => {
                    if let Some(s) = &mut self.sel {
                        s.cursor = row;
                    }
                }
                Action::StartEdit { row, side } => {
                    if self.edit.is_some() {
                        self.commit_edit(Move::Stay);
                    }
                    self.start_edit(row, side);
                }
                Action::Commit(mv) => self.commit_edit(mv),
                Action::Cancel => self.edit = None,
                Action::CopyRows { rows, to_right } => {
                    if let Some(m) = &mut self.model {
                        m.copy_rows(rows, to_right);
                    }
                }
                Action::DeleteRows { rows, side } => {
                    if let Some(m) = &mut self.model {
                        m.delete_rows(rows, side);
                    }
                    self.sel = None;
                }
                Action::InsertLine { row, side, below } => {
                    if let Some(m) = &mut self.model {
                        let at = match m.rows.get(row).and_then(|r| if side == 0 { r.left } else { r.right }) {
                            Some(l) => l + usize::from(below),
                            None => m.insertion_point(side, row),
                        };
                        m.splice(side, at..at, vec![String::new()], false);
                        if let Some(r) = m.row_of_line.get(side).and_then(|v| v.get(at)).copied() {
                            self.start_edit(r, side);
                        }
                    }
                }
                Action::CopyText { rows, side } => {
                    if let Some(m) = &self.model {
                        let text = m
                            .rows
                            .get(rows)
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|r| if side == 0 { r.left } else { r.right })
                            .filter_map(|l| m.line(side, l))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ctx.copy_text(text);
                    }
                }
                Action::ScrollToFrac(f) => {
                    if let Some(m) = &self.model {
                        let n = m.visible.len();
                        self.scroll_to_vis = Some(((f * n as f32) as usize).min(n.saturating_sub(1)));
                    }
                }
                Action::ExpandGap => {
                    if let Some(m) = &mut self.model {
                        m.set_display(Display::All);
                    }
                }
            }
        }
    }

    fn header(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let pal = Palette::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let has = self.model.is_some();
        let blocks = self.model.as_ref().map_or(0, |m| m.blocks.len());
        let display = self.model.as_ref().map(|m| m.display).unwrap_or_default();
        let target = self.target_rows().is_some();
        let (can_undo, can_redo) = self.model.as_ref().map_or((false, false), |m| (m.can_undo(), m.can_redo()));
        let dirty = self.is_dirty();
        let mut cmd: Option<Cmd> = None;
        let mut new_display = None;
        let mut rules_changed = false;
        let mut copy_patch = false;
        toolbar::bar(ui, "text-toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            for (d, icon, label, tip) in [
                (Display::All, Icon::ShowAll, "All", "Show all lines"),
                (Display::Diffs, Icon::ShowDiffs, "Diffs", "Show only the differences"),
                (Display::Context, Icon::Context, "Context", "Show the differences with a few lines of context"),
            ] {
                if toolbar::toggle(ui, icon, label, tip, has && display == d, has) {
                    new_display = Some(d);
                }
            }
            if toolbar::toggle(ui, Icon::Minor, "Minor", "Highlight unimportant differences (whitespace, case, ignored text)", self.show_minor, has) {
                self.show_minor = !self.show_minor;
            }
            toolbar::separator(ui);
            let rules_tip = format!("Comparison rules: {}", rules_summary(&self.opts));
            toolbar::menu(ui, Icon::Rules, "Rules", &rules_tip, true, |ui| {
                ui.set_min_width(320.0);
                rules_changed |= crate::rules_ui::text_rules(ui, &mut self.opts, "text-rules");
            });
            toolbar::menu(ui, Icon::Format, "Format", "Encoding, line endings and tab width", has, |ui| self.format_menu(ui));
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::CopyRight, "Copy Right", "Copy the difference to the right (Ctrl+R)", target) {
                cmd = Some(Cmd::CopyToRight);
            }
            if toolbar::button(ui, Icon::CopyLeft, "Copy Left", "Copy the difference to the left (Ctrl+L)", target) {
                cmd = Some(Cmd::CopyToLeft);
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::NextDiff, "Next Section", "Next difference (Ctrl+N)", blocks > 0) {
                cmd = Some(Cmd::NextDiff);
            }
            if toolbar::button(ui, Icon::PrevDiff, "Prev Section", "Previous difference (Ctrl+P)", blocks > 0) {
                cmd = Some(Cmd::PrevDiff);
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Swap, "Swap", "Swap left and right", has) {
                cmd = Some(Cmd::SwapSides);
            }
            if toolbar::button(ui, Icon::Reload, "Reload", "Reload both files (F5)", true) {
                cmd = Some(Cmd::Refresh);
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Undo, "Undo", "Undo (Ctrl+Z)", can_undo) {
                cmd = Some(Cmd::Undo);
            }
            if toolbar::button(ui, Icon::Redo, "Redo", "Redo (Ctrl+Y)", can_redo) {
                cmd = Some(Cmd::Redo);
            }
            if toolbar::button(ui, Icon::Save, "Save", "Save changed files (Ctrl+S)", dirty) {
                cmd = Some(Cmd::Save);
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Find, "Find", "Find text (Ctrl+F)", has) {
                cmd = Some(Cmd::Find);
            }
            toolbar::menu(ui, Icon::Report, "Export", "Reports, patches and whole-file editing", has, |ui| {
                for (icon, label, c) in [
                    (Icon::Report, "HTML Report…", Cmd::ExportHtmlReport),
                    (Icon::Report, "Unified Patch…", Cmd::ExportPatch),
                    (Icon::Edit, "Edit Left File…", Cmd::EditLeftWhole),
                    (Icon::Edit, "Edit Right File…", Cmd::EditRightWhole),
                ] {
                    if toolbar::menu_item(ui, icon, label, "", true) {
                        cmd = Some(c);
                        ui.close();
                    }
                }
                if toolbar::menu_item(ui, Icon::ShowAll, "Copy Unified Patch", "", true) {
                    copy_patch = true;
                    ui.close();
                }
            });
        });
        if let Some(d) = new_display
            && let Some(m) = &mut self.model
        {
            m.set_display(d);
            self.scroll_to_vis = self.current.and_then(|c| m.blocks.get(c)).and_then(|b| m.vis_index_of_row(b.start));
        }
        if rules_changed && let Some(m) = &mut self.model {
            m.opts = self.opts.clone();
            m.rediff();
            self.intra.clear();
        }
        if copy_patch && let Some(p) = self.patch_text() {
            ctx.copy_text(p);
        }
        if let Some(c) = cmd {
            self.run(c, &ctx, cx);
        }

        // Path and file-information strips, one column per side.
        let mut reload = [false; 2];
        let mut save = [false; 2];
        egui::Frame::NONE.fill(pal.header_bg).show(ui, |ui| {
            ui.columns(2, |cols| {
                for (side, col) in cols.iter_mut().enumerate() {
                    let s = self.model.as_ref().and_then(|m| m.sides.get(side));
                    let dirty = s.is_some_and(|s| s.dirty);
                    let missing = s.is_some_and(|s| !s.exists && s.path.is_some());
                    let info = s.map(|s| side_info(s, &self.opts));
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 6, right: 4, top: 4, bottom: 2 }).show(col, |ui| {
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 30.0;
                            ui.allocate_ui(vec2(w, 22.0), |ui| {
                                if let Some(path) = self.paths.get_mut(side)
                                    && widgets::path_bar(ui, &format!("text-path-{side}"), path, false, "File path").submitted
                                    && let Some(r) = reload.get_mut(side)
                                {
                                    *r = true;
                                }
                            });
                            let name = SIDE_NAMES.get(side).copied().unwrap_or_default();
                            if toolbar::small(ui, Icon::Save, &format!("Save the {} file", name.to_lowercase()), dirty)
                                && let Some(s) = save.get_mut(side)
                            {
                                *s = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            if dirty {
                                ui.label(RichText::new("Modified").color(pal.accent).size(12.0));
                            }
                            if missing {
                                ui.label(RichText::new("New file").color(pal.warning).size(12.0))
                                    .on_hover_text("This file does not exist yet; saving creates it.");
                            }
                            for part in info.unwrap_or_default() {
                                ui.label(RichText::new(part).color(pal.dim_text).size(12.0));
                            }
                        });
                    });
                }
            });
            let r = ui.max_rect();
            ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, pal.border));
        });
        for (side, s) in save.iter().enumerate() {
            if *s {
                self.save_side(side, false, cx);
            }
        }
        if reload.iter().any(|r| *r) {
            self.request_reload(&ctx, reload);
        }

        if self.find.open {
            ui.horizontal(|ui| {
                ui.label("Find:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.find.query).id_salt("text-find").desired_width(260.0));
                if self.find.focus {
                    r.request_focus();
                    self.find.focus = false;
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if r.changed() {
                    self.find.not_found = false;
                }
                ui.checkbox(&mut self.find.match_case, "Match case");
                if ui.button("Next").clicked() || enter {
                    self.find_step(true);
                    if enter {
                        r.request_focus();
                    }
                }
                if ui.button("Previous").clicked() {
                    self.find_step(false);
                }
                if self.find.not_found {
                    ui.colored_label(pal.warning, "Not found");
                }
                if ui.button("×").clicked() || (r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape))) {
                    self.find.open = false;
                    self.find.hit = None;
                }
            });
        }
        if let Some(g) = &mut self.goto {
            let mut close = false;
            let mut go = None;
            ui.horizontal(|ui| {
                ui.label("Go to line:");
                let r = ui.add(egui::TextEdit::singleline(g).id_salt("text-goto").desired_width(80.0));
                if self.goto_focus {
                    r.request_focus();
                    self.goto_focus = false;
                }
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("Go").clicked() {
                    go = g.trim().parse::<usize>().ok();
                    close = true;
                }
                if ui.button("×").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
            });
            if let (Some(n), Some(m)) = (go, &self.model) {
                let side = self.sel.map_or(0, |s| s.side);
                let idx = n.saturating_sub(1);
                let row = m.row_of_line.get(side).and_then(|v| v.get(idx).or(v.last())).copied();
                if let Some(row) = row {
                    self.sel = Some(Selection { side, anchor: row, cursor: row });
                    self.scroll_to_vis = m.vis_index_of_row(row);
                }
            }
            if close {
                self.goto = None;
            }
        }
        for side in 0..2 {
            if self.disk_changed.get(side).copied().unwrap_or(false) {
                ui.horizontal(|ui| {
                    let name = SIDE_NAMES.get(side).copied().unwrap_or_default().to_lowercase();
                    ui.colored_label(pal.warning, format!("⚠ The {name} file changed on disk."));
                    if ui.button("Reload").clicked() {
                        let mut which = [false; 2];
                        if let Some(w) = which.get_mut(side) {
                            *w = true;
                        }
                        self.request_reload(&ctx, which);
                    }
                    if ui.button("Ignore").clicked()
                        && let Some(d) = self.disk_changed.get_mut(side)
                    {
                        *d = false;
                        if let Some(m) = &mut self.model
                            && let Some(s) = m.sides.get_mut(side)
                            && let Some(p) = &s.path
                        {
                            s.mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                        }
                    }
                });
            }
        }
        if let Some(e) = self.model.as_ref().and_then(|m| m.rule_error.clone()) {
            ui.colored_label(pal.warning, format!("⚠ Rule ignored: {e}"));
        }
    }

    fn find_step(&mut self, forward: bool) {
        let Some(m) = &mut self.model else { return };
        let from = self.find.hit.or_else(|| self.sel.map(|s| (s.cursor, s.side)));
        match m.find(&self.find.query, self.find.match_case, from, forward) {
            Some((row, side)) => {
                self.find.hit = Some((row, side));
                self.find.not_found = false;
                if m.vis_index_of_row(row).is_some_and(|vi| !matches!(m.visible.get(vi), Some(Vis::Row(r)) if *r == row)) {
                    m.set_display(Display::All);
                }
                self.sel = Some(Selection { side, anchor: row, cursor: row });
                self.scroll_to_vis = m.vis_index_of_row(row);
            }
            None => self.find.not_found = true,
        }
    }

    /// The Format menu: per-side encoding, line endings, BOM, final newline, and the tab width.
    fn format_menu(&mut self, ui: &mut Ui) {
        let mut tab = self.tab_width;
        ui.horizontal(|ui| {
            ui.label("Tab width");
            ui.add(egui::DragValue::new(&mut tab).range(1..=16));
        });
        if tab != self.tab_width {
            self.tab_width = tab;
            self.intra.clear();
            self.content_w = 0.0;
        }
        let Some(m) = &mut self.model else { return };
        for side in 0..2 {
            let Some(s) = m.sides.get_mut(side) else { continue };
            ui.separator();
            ui.label(RichText::new(SIDE_NAMES.get(side).copied().unwrap_or_default()).strong());
            ui.menu_button(format!("Line endings: {}", s.file.line_ending.label()), |ui| {
                for le in LineEnding::ALL {
                    if ui.radio(s.file.line_ending == le, le.label()).clicked() && s.file.line_ending != le {
                        s.file.line_ending = le;
                        s.dirty = true;
                    }
                }
            });
            ui.menu_button(format!("Encoding: {}", s.file.encoding.name()), |ui| {
                for enc in TextEncoding::choices() {
                    if ui.radio(s.file.encoding == enc, enc.name()).clicked() && s.file.encoding != enc {
                        s.file.encoding = enc;
                        s.dirty = true;
                    }
                }
            });
            if ui.checkbox(&mut s.file.bom, "Byte order mark").changed() {
                s.dirty = true;
            }
            if ui.checkbox(&mut s.file.final_newline, "Newline at end of file").changed() {
                s.dirty = true;
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        let load_time = self.load_time;
        toolbar::status_bar(ui, "text-status", |ui| {
            let Some(m) = &self.model else {
                ui.label(" ");
                return;
            };
            let st = m.stats;
            if st.important_blocks == 0 && st.unimportant_blocks == 0 {
                toolbar::status_icon(ui, Icon::Check);
                ui.colored_label(pal.success, "Identical");
            } else if st.important_blocks == 0 {
                toolbar::status_icon(ui, Icon::Minor);
                ui.colored_label(pal.minor_text, format!("Only unimportant differences ({})", st.unimportant_blocks));
            } else {
                toolbar::status_icon(ui, Icon::ShowDiffs);
                ui.label(format!("{} difference section{}", st.important_blocks, if st.important_blocks == 1 { "" } else { "s" }));
            }
            toolbar::status_sep(ui);
            let row = self.sel.map(|s| s.cursor).or_else(|| self.current.and_then(|c| m.blocks.get(c)).map(|b| b.start));
            let row_kind = row.and_then(|r| m.rows.get(r));
            let (label, color) = match row_kind {
                None => ("", pal.dim_text),
                Some(r) if r.unimportant || r.kind == RowKind::Minor => ("Unimportant difference", pal.minor_text),
                Some(r) => match r.kind {
                    RowKind::Same => ("Same", pal.text),
                    RowKind::Changed => ("Changed", pal.changed_text),
                    RowKind::LeftOnly => ("Left only", pal.changed_text),
                    RowKind::RightOnly => ("Right only", pal.changed_text),
                    RowKind::Minor => ("Unimportant difference", pal.minor_text),
                },
            };
            ui.add_sized([150.0, 16.0], egui::Label::new(RichText::new(label).color(color)));
            toolbar::status_sep(ui);
            ui.label(format!("{} changed · {} left-only · {} right-only lines", st.changed_lines, st.left_only_lines, st.right_only_lines));
            if let (Some(s), Some(r)) = (self.sel, row_kind) {
                let line = if s.side == 0 { r.left } else { r.right };
                if let Some(l) = line {
                    toolbar::status_sep(ui);
                    ui.label(format!("{} line {}", SIDE_NAMES.get(s.side).copied().unwrap_or_default(), l + 1));
                }
            }
            if let Some(e) = &m.rule_error {
                toolbar::status_sep(ui);
                ui.colored_label(pal.warning, format!("Rule ignored: {e}"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(t) = load_time {
                    ui.label(RichText::new(format!("Load time: {t:.2} s")).color(pal.dim_text));
                    toolbar::status_sep(ui);
                }
                for side in (0..2).rev() {
                    if let Some(s) = m.sides.get(side) {
                        let mut txt = format!("{} · {}", s.file.encoding.name(), le_short(s.file.line_ending));
                        if s.file.mixed_endings {
                            txt.push_str(" (mixed)");
                        }
                        ui.label(RichText::new(txt).color(pal.dim_text)).on_hover_text(format!(
                            "{} file encoding and line endings (change them in Format)",
                            SIDE_NAMES.get(side).copied().unwrap_or_default()
                        ));
                        toolbar::status_sep(ui);
                    }
                }
            });
        });
    }

    /// The line-details pane: the selected row of both sides, one above the other, with the
    /// differing characters highlighted.
    fn details_pane(&mut self, ui: &mut Ui, settings: &Settings) {
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        let font = theme::mono(&ctx);
        let row_h = ctx.fonts_mut(|f| f.row_height(&font)).ceil() + 4.0;
        let row = self.sel.map(|s| s.cursor).or_else(|| self.current.and_then(|c| self.model.as_ref()?.blocks.get(c)).map(|b| b.start));
        let tab_width = self.tab_width;
        egui::Panel::bottom("text-details").frame(egui::Frame::NONE.fill(pal.pane_bg)).show(ui, |ui| {
            let r0 = ui.max_rect();
            ui.painter().line_segment([r0.left_top(), r0.right_top()], Stroke::new(1.0, pal.border));
            let Some(m) = &self.model else { return };
            let found = row.and_then(|r| m.rows.get(r).map(|x| (r, *x)));
            for side in 0..2 {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let p = ui.painter_at(rect);
                let gutter = Rect::from_min_size(rect.min, vec2(46.0, row_h));
                p.rect_filled(gutter, 0.0, pal.gutter_bg);
                let icon = if side == 0 { Icon::CopyRight } else { Icon::CopyLeft };
                icons::paint(&p, Rect::from_center_size(gutter.center() - vec2(10.0, 0.0), vec2(13.0, 13.0)), icon, &pal, true);
                p.text(gutter.center() + vec2(9.0, 0.0), Align2::CENTER_CENTER, if side == 0 { "L" } else { "R" }, font.clone(), pal.gutter_text);
                let text_rect = Rect::from_min_max(pos2(gutter.right(), rect.top()), rect.max);
                let Some((ri, r)) = found else { continue };
                let line = if side == 0 { r.left } else { r.right };
                let Some(l) = line else {
                    pal.hatch_rect(&p, text_rect);
                    continue;
                };
                let shown = display_text(m.line(side, l).unwrap_or_default(), tab_width);
                let hl: Vec<Range<usize>> = if r.kind == RowKind::Changed && settings.intraline_highlight {
                    let entry = self.intra.entry(ri).or_insert_with(|| {
                        let a = r.left.and_then(|x| m.line(0, x)).map(|s| display_text(s, tab_width)).unwrap_or_default();
                        let b = r.right.and_then(|x| m.line(1, x)).map(|s| display_text(s, tab_width)).unwrap_or_default();
                        intraline(&a, &b)
                    });
                    if side == 0 { entry.0.clone() } else { entry.1.clone() }
                } else {
                    Vec::new()
                };
                let color = if r.kind.is_difference() && !r.unimportant && r.kind != RowKind::Minor { pal.changed_text } else { pal.text };
                if r.kind.is_difference() {
                    p.rect_filled(text_rect, 0.0, pal.row_bg(r.kind, true, r.unimportant));
                }
                let galley = p.layout_job(line_job(&shown, font.clone(), color, &hl, pal.changed_strong, &[], pal.find_bg));
                p.galley(pos2(text_rect.left() + 6.0, rect.top() + 2.0), galley, color);
            }
            ui.add_space(2.0);
        });
    }

    fn patch_text(&self) -> Option<String> {
        let m = self.model.as_ref()?;
        let name = |i: usize| self.paths.get(i).map(|p| p.replace('\\', "/")).unwrap_or_default();
        Some(diffcraft_report::unified_diff(
            &name(0),
            &name(1),
            m.lines(0),
            m.lines(1),
            &m.diff,
            3,
            m.sides[0].file.final_newline,
            m.sides[1].file.final_newline,
        ))
    }

    fn file_stem(&self, side: usize) -> String {
        self.paths.get(side).and_then(|p| Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_else(|| "file".into())
    }

    fn body(&mut self, ui: &mut Ui, settings: &Settings) -> Vec<Action> {
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        let font = theme::mono(&ctx);
        let (row_h, char_w) = ctx.fonts_mut(|f| (f.row_height(&font).ceil() + 2.0, f.glyph_width(&font, '0').max(1.0)));
        let Some(m) = &self.model else { return actions };
        if m.revision != self.intra_rev {
            self.intra.clear();
            self.intra_rev = m.revision;
        }

        let full = ui.available_rect_before_wrap();
        let thumb_w = 46.0;
        let hbar_h = 12.0;
        let grid = Rect::from_min_max(pos2(full.left() + thumb_w + 1.0, full.top()), pos2(full.right(), full.bottom() - hbar_h));
        let max_lines = m.lines(0).len().max(m.lines(1).len()).max(1);
        let digits = (max_lines as f32).log10().floor() as usize + 1;
        let gutter_w = if settings.show_line_numbers { digits as f32 * char_w + 12.0 } else { 6.0 };
        let center_w = 30.0;
        let scrollbar_w = ui.spacing().scroll.bar_width + 4.0;
        let half = ((grid.width() - center_w - scrollbar_w) / 2.0).max(40.0);
        let text_w = (half - gutter_w).max(20.0);
        let tab_width = self.tab_width;
        let intraline_on = settings.intraline_highlight;
        let show_minor = self.show_minor;

        // Horizontal scroll: shift+wheel or trackpad over the grid.
        if ui.rect_contains_pointer(grid) {
            let dx = ui.input(|i| i.smooth_scroll_delta.x);
            if dx != 0.0 {
                self.h_scroll = (self.h_scroll - dx).max(0.0);
            }
        }
        let h_scroll = self.h_scroll;
        let sel = self.sel;
        let current_rows = self.current.and_then(|c| m.blocks.get(c)).cloned();
        let find_hit = self.find.hit;
        let find_q = if self.find.open && !self.find.query.is_empty() { Some((self.find.query.clone(), self.find.match_case)) } else { None };
        let mut max_w = 0.0f32;
        let mut edit_rendered = false;
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        let primary_down = ctx.input(|i| i.pointer.primary_down());
        if !primary_down {
            self.dragging = false;
        }
        let dragging = self.dragging;
        let mut start_drag = false;
        let shift = ctx.input(|i| i.modifiers.shift);

        let mut area = egui::ScrollArea::vertical().id_salt("text-grid").auto_shrink(false);
        if let Some(vi) = self.scroll_to_vis.take() {
            let offset = (vi as f32 * row_h - grid.height() / 3.0).max(0.0);
            area = area.vertical_scroll_offset(offset);
        }
        let total = m.visible.len();
        let mut child = ui.new_child(UiBuilder::new().max_rect(grid));
        child.spacing_mut().item_spacing.y = 0.0;
        let intra = &mut self.intra;
        let edit = &mut self.edit;
        let out = area.show_rows(&mut child, row_h, total, |ui, range| {
            for vi in range {
                let Some(vis) = m.visible.get(vi).copied() else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                let left_gutter = Rect::from_min_size(rect.min, vec2(gutter_w, row_h));
                let left_text = Rect::from_min_size(pos2(left_gutter.right(), rect.top()), vec2(text_w, row_h));
                let center = Rect::from_min_size(pos2(left_text.right(), rect.top()), vec2(center_w, row_h));
                let right_gutter = Rect::from_min_size(pos2(center.right(), rect.top()), vec2(gutter_w, row_h));
                let right_text = Rect::from_min_size(pos2(right_gutter.right(), rect.top()), vec2(text_w, row_h));
                let row_idx = match vis {
                    Vis::Gap { count, .. } => {
                        let bar = rect.shrink2(vec2(0.0, 2.0));
                        painter.rect_filled(bar, 0.0, pal.separator_bg);
                        painter.line_segment([bar.left_center(), bar.right_center()], Stroke::new(1.0, pal.border));
                        let label = format!("{count} IDENTICAL LINE{}", if count == 1 { "" } else { "S" });
                        let small = egui::FontId::proportional(10.5);
                        for x in [left_text.center().x, right_text.center().x] {
                            let g = painter.layout_no_wrap(label.clone(), small.clone(), pal.dim_text);
                            let pill = Rect::from_center_size(pos2(x, bar.center().y), g.size() + vec2(14.0, 2.0));
                            painter.rect(pill, 7.0, pal.separator_bg, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                            painter.galley(pill.center() - g.size() / 2.0, g, pal.dim_text);
                        }
                        let plus = Rect::from_center_size(pos2(left_gutter.left() + 9.0, bar.center().y), vec2(11.0, 11.0));
                        painter.rect(plus, 1.0, pal.separator_bg, Stroke::new(1.0, pal.dim_text), egui::StrokeKind::Inside);
                        painter.line_segment([plus.left_center() + vec2(2.5, 0.0), plus.right_center() - vec2(2.5, 0.0)], Stroke::new(1.0, pal.dim_text));
                        painter.line_segment([plus.center_top() + vec2(0.0, 2.5), plus.center_bottom() - vec2(0.0, 2.5)], Stroke::new(1.0, pal.dim_text));
                        let resp = resp.on_hover_text("Identical lines hidden by the display filter. Click to show all lines.");
                        if resp.clicked() || resp.double_clicked() {
                            actions.push(Action::ExpandGap);
                        }
                        continue;
                    }
                    Vis::Row(r) => r,
                };
                let Some(row) = m.rows.get(row_idx).copied() else { continue };
                let side_at = |x: f32| usize::from(x >= center.center().x);
                let in_center = pointer.is_some_and(|p| center.contains(p));
                if resp.drag_started()
                    && let Some(p) = pointer
                    && !in_center
                {
                    actions.push(Action::Select { row: row_idx, side: side_at(p.x), extend: shift });
                    start_drag = true;
                } else if resp.clicked()
                    && let Some(p) = pointer
                    && !in_center
                {
                    actions.push(Action::Select { row: row_idx, side: side_at(p.x), extend: shift });
                }
                if dragging && primary_down && pointer.is_some_and(|p| p.y >= rect.top() && p.y < rect.bottom()) {
                    actions.push(Action::DragTo(row_idx));
                }
                if resp.double_clicked()
                    && let Some(p) = pointer
                    && !in_center
                {
                    actions.push(Action::StartEdit { row: row_idx, side: side_at(p.x) });
                }

                for (side, gutter, text_rect) in [(0usize, left_gutter, left_text), (1, right_gutter, right_text)] {
                    let line = if side == 0 { row.left } else { row.right };
                    painter.rect_filled(gutter, 0.0, pal.gutter_bg);
                    let minor = row.kind == RowKind::Minor || row.unimportant;
                    if line.is_none() {
                        pal.hatch_rect(&painter, text_rect);
                    } else if minor && !show_minor {
                        painter.rect_filled(text_rect, 0.0, pal.pane_bg);
                    } else {
                        painter.rect_filled(text_rect, 0.0, pal.row_bg(row.kind, true, row.unimportant));
                    }
                    painter.line_segment([gutter.right_top(), gutter.right_bottom()], Stroke::new(1.0, pal.border));
                    if let Some(s) = sel
                        && s.side == side
                        && s.rows().contains(&row_idx)
                    {
                        painter.rect_filled(text_rect, 0.0, pal.selection_bg);
                    }
                    if find_hit == Some((row_idx, side)) {
                        painter.rect_filled(gutter, 0.0, pal.find_bg);
                    }
                    if let Some(cr) = &current_rows
                        && cr.contains(&row_idx)
                    {
                        painter.rect_filled(Rect::from_min_size(text_rect.min, vec2(3.0, row_h)), 0.0, pal.current_outline);
                        if row_idx == cr.start {
                            painter.line_segment([text_rect.left_top(), text_rect.right_top()], Stroke::new(1.0, pal.current_outline));
                        }
                        if row_idx + 1 == cr.end {
                            painter.line_segment([text_rect.left_bottom(), text_rect.right_bottom()], Stroke::new(1.0, pal.current_outline));
                        }
                    }
                    let Some(l) = line else { continue };
                    if settings.show_line_numbers {
                        painter.text(pos2(gutter.right() - 6.0, gutter.center().y), Align2::RIGHT_CENTER, (l + 1).to_string(), font.clone(), pal.gutter_text);
                    }
                    if edit.as_ref().is_some_and(|ed| ed.row == row_idx && ed.side == side) {
                        continue;
                    }
                    let raw = m.line(side, l).unwrap_or_default();
                    let shown = display_text(raw, tab_width);
                    let hl: Vec<Range<usize>> = if row.kind == RowKind::Changed && intraline_on && !row.unimportant {
                        let entry = intra.entry(row_idx).or_insert_with(|| {
                            let a = row.left.and_then(|x| m.line(0, x)).map(|s| display_text(s, tab_width)).unwrap_or_default();
                            let b = row.right.and_then(|x| m.line(1, x)).map(|s| display_text(s, tab_width)).unwrap_or_default();
                            intraline(&a, &b)
                        });
                        if side == 0 { entry.0.clone() } else { entry.1.clone() }
                    } else {
                        Vec::new()
                    };
                    let color = if minor {
                        if show_minor { pal.minor_text } else { pal.text }
                    } else if row.kind.is_difference() {
                        pal.changed_text
                    } else {
                        pal.text
                    };
                    let finds = find_q.as_ref().map(|(q, case)| find_ranges(&shown, q, *case)).unwrap_or_default();
                    let job = line_job(&shown, font.clone(), color, &hl, pal.changed_strong, &finds, pal.find_bg);
                    let galley = painter.layout_job(job);
                    max_w = max_w.max(galley.size().x);
                    painter.with_clip_rect(text_rect.intersect(rect)).galley(pos2(text_rect.left() + 6.0 - h_scroll, rect.top() + 1.0), galley, color);
                }

                // Editor widget (after painting so it sits on top).
                if let Some(ed) = edit.as_mut()
                    && ed.row == row_idx
                {
                    edit_rendered = true;
                    let text_rect = if ed.side == 0 { left_text } else { right_text };
                    let id = egui::Id::new(("line-edit", ed.side, ed.row));
                    if ed.cursor_at_start {
                        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
                        state.store(ui.ctx(), id);
                        ed.cursor_at_start = false;
                    }
                    let mut edit_ui = ui.new_child(UiBuilder::new().max_rect(text_rect.shrink2(vec2(2.0, 0.0))));
                    let out = egui::TextEdit::singleline(&mut ed.buffer)
                        .id(id)
                        .font(font.clone())
                        .frame(egui::Frame::NONE)
                        .margin(vec2(4.0, 1.0))
                        .desired_width(text_rect.width() - 4.0)
                        .background_color(pal.pane_bg)
                        .show(&mut edit_ui);
                    painter.rect_stroke(text_rect, 0.0, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
                    if ed.focus {
                        out.response.request_focus();
                        ed.focus = false;
                    }
                    let (enter, esc, up, down) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
                    if out.response.lost_focus() {
                        if esc {
                            actions.push(Action::Cancel);
                        } else if enter {
                            let c = out.cursor_range.map_or(ed.buffer.chars().count(), |r| r.primary.index.0);
                            actions.push(Action::Commit(Move::Split(c)));
                        } else {
                            actions.push(Action::Commit(Move::Stay));
                        }
                    } else if out.response.has_focus() && up {
                        actions.push(Action::Commit(Move::Up));
                    } else if out.response.has_focus() && down {
                        actions.push(Action::Commit(Move::Down));
                    }
                }

                // Centre strip: copy arrows on the first row of each block.
                painter.rect_filled(center, 0.0, pal.gutter_bg);
                painter.line_segment([center.left_top(), center.left_bottom()], Stroke::new(1.0, pal.border));
                painter.line_segment([center.right_top(), center.right_bottom()], Stroke::new(1.0, pal.border));
                if row.is_important()
                    && let Some(b) = m.block_of_row(row_idx)
                    && let Some(block) = m.blocks.get(b)
                    && block.start == row_idx
                {
                    let r_btn = Rect::from_min_size(center.min, vec2(center_w / 2.0, row_h));
                    let l_btn = Rect::from_min_size(pos2(center.center().x, center.top()), vec2(center_w / 2.0, row_h));
                    let rr = ui.interact(r_btn, ui.id().with(("cr", row_idx)), Sense::click()).on_hover_text("Copy this difference to the right");
                    let lr = ui.interact(l_btn, ui.id().with(("cl", row_idx)), Sense::click()).on_hover_text("Copy this difference to the left");
                    for (btn, resp, icon) in [(r_btn, &rr, Icon::CopyRight), (l_btn, &lr, Icon::CopyLeft)] {
                        if resp.hovered() {
                            painter.rect_filled(btn.shrink(1.0), 2.0, pal.hover_bg);
                        }
                        icons::paint(&painter, Rect::from_center_size(btn.center(), vec2(12.0, 12.0)), icon, &pal, true);
                    }
                    if rr.clicked() {
                        actions.push(Action::CopyRows { rows: block.clone(), to_right: true });
                    }
                    if lr.clicked() {
                        actions.push(Action::CopyRows { rows: block.clone(), to_right: false });
                    }
                }

                // Context menu.
                let menu_side = pointer.map_or(0, |p| side_at(p.x));
                resp.context_menu(|ui| {
                    let name = SIDE_NAMES.get(menu_side).copied().unwrap_or_default();
                    let in_sel = |s: &Selection| s.rows().contains(&row_idx);
                    let rows = match sel {
                        Some(s) if in_sel(&s) && s.rows().len() > 1 => s.rows(),
                        _ => m.block_of_row(row_idx).and_then(|b| m.blocks.get(b).cloned()).unwrap_or(row_idx..row_idx + 1),
                    };
                    let sel_rows = match sel {
                        Some(s) if in_sel(&s) => s.rows(),
                        _ => row_idx..row_idx + 1,
                    };
                    if ui.button("Copy to Right").clicked() {
                        actions.push(Action::CopyRows { rows: rows.clone(), to_right: true });
                        ui.close();
                    }
                    if ui.button("Copy to Left").clicked() {
                        actions.push(Action::CopyRows { rows: rows.clone(), to_right: false });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("Edit {name} Line")).clicked() {
                        actions.push(Action::StartEdit { row: row_idx, side: menu_side });
                        ui.close();
                    }
                    if ui.button("Insert Line Above").clicked() {
                        actions.push(Action::InsertLine { row: row_idx, side: menu_side, below: false });
                        ui.close();
                    }
                    if ui.button("Insert Line Below").clicked() {
                        actions.push(Action::InsertLine { row: row_idx, side: menu_side, below: true });
                        ui.close();
                    }
                    if ui.button(format!("Delete {name} Lines")).clicked() {
                        actions.push(Action::DeleteRows { rows: sel_rows.clone(), side: menu_side });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Copy Text").clicked() {
                        actions.push(Action::CopyText { rows: sel_rows.clone(), side: menu_side });
                        ui.close();
                    }
                });
            }
        });
        if start_drag {
            self.dragging = true;
        }
        if self.edit.is_some() && !edit_rendered {
            // The edited row scrolled out of view: commit it.
            actions.push(Action::Commit(Move::Stay));
        }
        self.content_w = self.content_w.max(max_w + 12.0);
        let first = out.state.offset.y / row_h;
        let shown = out.inner_rect.height() / row_h;
        let n = total.max(1) as f32;
        self.view_frac = (first / n, (first + shown) / n);

        // Thumbnail overview on the left: both files in miniature, the visible part framed.
        let thumb = Rect::from_min_max(full.min, pos2(full.left() + thumb_w, full.bottom() - hbar_h));
        if let Some(f) = self.thumbnail(ui, thumb, &pal) {
            actions.push(Action::ScrollToFrac(f));
        }
        // Horizontal scrollbar under the text.
        let hb = Rect::from_min_max(pos2(full.left(), full.bottom() - hbar_h + 2.0), pos2(grid.right(), full.bottom()));
        self.h_scroll = widgets::hscrollbar(ui, hb, self.h_scroll, self.content_w, text_w, &pal);
        ui.allocate_rect(full, Sense::hover());
        actions
    }

    /// Paint the two-column miniature of both files. Returns a scroll target when clicked.
    fn thumbnail(&self, ui: &mut Ui, rect: Rect, pal: &Palette) -> Option<f32> {
        let m = self.model.as_ref()?;
        let resp = ui.interact(rect, ui.id().with("text-thumb"), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, pal.gutter_bg);
        p.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, pal.border));
        let n = m.visible.len().max(1);
        let h = rect.height().max(1.0);
        let col_w = (rect.width() - 10.0) / 2.0;
        let cols = [
            Rect::from_min_size(rect.min + vec2(4.0, 2.0), vec2(col_w, h - 4.0)),
            Rect::from_min_size(rect.min + vec2(6.0 + col_w, 2.0), vec2(col_w, h - 4.0)),
        ];
        for c in cols {
            p.rect_filled(c, 0.0, pal.pane_bg);
        }
        // One slice per pixel row, or per displayed row when there are fewer rows than pixels.
        let inner_h = h - 4.0;
        let slices = n.min((inner_h as usize).max(1));
        for k in 0..slices {
            let lo = k * n / slices;
            let hi = ((k + 1) * n / slices).max(lo + 1).min(n);
            // Strongest status in this slice, per side: missing > difference > minor > same.
            let mut kinds = [None::<(RowKind, bool)>; 2];
            for v in m.visible.get(lo..hi).unwrap_or_default() {
                let Vis::Row(r) = v else { continue };
                let Some(row) = m.rows.get(*r) else { continue };
                for (side, present) in [(0, row.left.is_some()), (1, row.right.is_some())] {
                    let kind = if row.unimportant && row.kind != RowKind::Same { RowKind::Minor } else { row.kind };
                    let k = (kind, present);
                    let rank = |x: &(RowKind, bool)| {
                        if !x.1 {
                            3
                        } else {
                            match x.0 {
                                RowKind::Same => 0,
                                RowKind::Minor => 1,
                                _ => 2,
                            }
                        }
                    };
                    if let Some(slot) = kinds.get_mut(side)
                        && slot.as_ref().is_none_or(|old| rank(&k) > rank(old))
                    {
                        *slot = Some(k);
                    }
                }
            }
            let y0 = rect.top() + 2.0 + inner_h * k as f32 / slices as f32;
            let dy = (inner_h / slices as f32).max(1.0);
            for (side, c) in cols.iter().enumerate() {
                let Some((kind, present)) = kinds.get(side).copied().flatten() else { continue };
                let color = if !present {
                    pal.hatch
                } else if kind == RowKind::Same {
                    pal.dim_text.gamma_multiply(0.35)
                } else if kind == RowKind::Minor {
                    pal.minor_text.gamma_multiply(0.7)
                } else {
                    pal.icon_red
                };
                // Unchanged text is drawn as a short grey bar so differences stand out.
                let w = if kind == RowKind::Same && present { c.width() * 0.7 } else { c.width() };
                p.rect_filled(Rect::from_min_size(pos2(c.left(), y0), vec2(w, dy)), 0.0, color);
            }
        }
        // Visible window.
        let (a, b) = self.view_frac;
        let win = Rect::from_min_max(pos2(rect.left() + 1.0, rect.top() + a.clamp(0.0, 1.0) * h), pos2(rect.right() - 1.0, rect.top() + b.clamp(0.0, 1.0) * h));
        p.rect(win, 1.0, pal.accent.gamma_multiply(0.12), Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let span = (b - a).max(0.0);
            return Some((((pos.y - rect.top()) / h) - span / 2.0).clamp(0.0, 1.0));
        }
        None
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(w) = &mut self.whole {
            let mut open = true;
            let mut apply = false;
            let mut cancel = false;
            egui::Window::new(format!("Edit {} file", SIDE_NAMES.get(w.side).copied().unwrap_or_default().to_lowercase()))
                .open(&mut open)
                .default_size([760.0, 540.0])
                .resizable(true)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            apply = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut w.text).code_editor().desired_width(f32::INFINITY).desired_rows(30));
                    });
                });
            if apply && let Some(m) = &mut self.model {
                let lines = TextFile::from_text(&w.text).lines;
                let len = m.lines(w.side).len();
                m.splice(w.side, 0..len, lines, false);
            }
            if apply || cancel || !open {
                self.whole = None;
            }
        }
        if let Some(Pending::Reload(which)) = self.confirm {
            match widgets::confirm_modal(ctx, "text-reload", "Discard changes?", "Reloading discards your unsaved edits.", "Reload") {
                Some(true) => {
                    self.confirm = None;
                    self.reload(ctx, which);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
    }
}

/// File facts for the info strip: modified time, size, rule and line count.
fn side_info(s: &Side, opts: &TextCompareOptions) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(t) = s.mtime {
        v.push(widgets::format_mtime(Some(t)));
    }
    if let Some(p) = &s.path
        && let Ok(meta) = std::fs::metadata(p)
    {
        v.push(format!("{} bytes", widgets::thousands(meta.len())));
    }
    v.push(format!("{} lines", widgets::thousands(s.file.lines.len() as u64)));
    v.push(s.file.encoding.name().to_owned());
    v.push(le_short(s.file.line_ending).to_owned());
    if opts.has_rules() {
        v.push("Rules active".to_owned());
    }
    v
}

fn le_short(le: LineEnding) -> &'static str {
    match le {
        LineEnding::Lf => "LF",
        LineEnding::CrLf => "CRLF",
        LineEnding::Cr => "CR",
    }
}

/// Load one side; a missing file is an empty "new file", a folder or binary file is an error.
fn load_side(path: &str) -> Result<Side, String> {
    if path.trim().is_empty() {
        return Ok(Side::default());
    }
    let p = widgets::clean_path(path);
    let meta = match std::fs::metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Side { path: Some(p), exists: false, ..Side::default() }),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    if meta.is_dir() {
        return Err(format!("{} is a folder. Use Folder Compare for folders.", p.display()));
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(format!("{} is too large for text comparison ({}). Open it in Hex Compare (binary).", p.display(), widgets::format_size(meta.len())));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    if is_probably_binary(&bytes) {
        return Err(format!("{} looks like a binary file. Open it in Hex Compare.", p.display()));
    }
    Ok(Side { file: TextFile::from_bytes(&bytes), mtime: meta.modified().ok(), path: Some(p), exists: true, dirty: false })
}

/// Tab-expanded, length-limited display text.
fn display_text(s: &str, tab_width: usize) -> String {
    let t = text::expand_tabs(s, tab_width);
    match t.char_indices().nth(MAX_DISPLAY_CHARS) {
        Some((i, _)) => format!("{}…", t.get(..i).unwrap_or_default()),
        None => t.into_owned(),
    }
}

/// Byte ranges of `q` in `s` (case-insensitive when the lowercase text keeps byte offsets).
fn find_ranges(s: &str, q: &str, match_case: bool) -> Vec<Range<usize>> {
    if q.is_empty() {
        return Vec::new();
    }
    let (hay, needle) = if match_case {
        (s.to_owned(), q.to_owned())
    } else {
        let h = s.to_lowercase();
        if h.len() != s.len() {
            return Vec::new();
        }
        (h, q.to_lowercase())
    };
    hay.match_indices(&needle).map(|(i, m)| i..i + m.len()).filter(|r| s.is_char_boundary(r.start) && s.is_char_boundary(r.end)).collect()
}

/// A layout job with background highlights (find matches win over intra-line highlights).
fn line_job(text: &str, font: FontId, color: Color32, hl: &[Range<usize>], hl_color: Color32, finds: &[Range<usize>], find_color: Color32) -> LayoutJob {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for r in hl.iter().chain(finds) {
        cuts.push(r.start.min(text.len()));
        cuts.push(r.end.min(text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob::default();
    for w in cuts.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else { continue };
        let Some(seg) = text.get(a..b) else { continue };
        let inside = |rs: &[Range<usize>]| rs.iter().any(|r| r.start <= a && b <= r.end);
        let background = if inside(finds) {
            find_color
        } else if inside(hl) {
            hl_color
        } else {
            Color32::TRANSPARENT
        };
        job.append(seg, 0.0, TextFormat { font_id: font.clone(), color, background, ..TextFormat::default() });
    }
    job
}

fn rules_summary(o: &TextCompareOptions) -> String {
    let mut parts = vec![o.whitespace.label().to_owned()];
    if o.ignore_case {
        parts.push("ignore case".into());
    }
    if o.ignore_blank_lines {
        parts.push("blank lines unimportant".into());
    }
    if !o.unimportant_patterns.is_empty() {
        parts.push(format!("{} unimportant pattern(s)", o.unimportant_patterns.len()));
    }
    parts.push(o.algorithm.label().to_owned());
    parts.join(", ")
}

fn write_export(path: &Path, bytes: &[u8], cx: &mut Cx<'_>) {
    match diffcraft_core::fsutil::write_atomic(path, bytes) {
        Ok(()) => cx.toasts.success(format!("Exported {}", path.display())),
        Err(e) => cx.toasts.error(format!("Export failed: {e}")),
    }
}

impl View for TextView {
    fn kind(&self) -> SessionKind {
        SessionKind::TextCompare
    }

    fn title(&self) -> String {
        let name = |p: &str| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "(none)".into());
        if self.paths.iter().all(|p| p.is_empty()) {
            return "Text Compare".into();
        }
        format!("{} ↔ {}", name(&self.paths[0]), name(&self.paths[1]))
    }

    fn tooltip(&self) -> String {
        format!("Left: {}\nRight: {}", self.paths[0], self.paths[1])
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.check_disk(&ctx, cx.settings, cx.toasts);
        self.header(ui, cx);
        self.status_bar(ui);
        if self.model.is_some() && self.load.is_none() {
            self.details_pane(ui, cx.settings);
        }
        if let Some(job) = &self.load {
            if jobs::progress_ui(ui, job) {
                self.load = None;
            }
            return;
        }
        if let Some(e) = self.load_error.clone() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.colored_label(Palette::get(&ctx).error, &e);
                if self.binary_hint && ui.button("Open in Hex Compare").clicked() {
                    cx.open.push(OpenRequest::Paths {
                        left: widgets::clean_path(&self.paths[0]),
                        right: widgets::clean_path(&self.paths[1]),
                        kind: Some(SessionKind::HexCompare),
                    });
                }
            });
            return;
        }
        if self.model.is_none() {
            widgets::empty_message(ui, "Choose a file for each side.");
            return;
        }
        let actions = self.body(ui, cx.settings);
        self.apply(actions, &ctx);
        self.dialogs(&ctx);
    }

    fn supports(&self, cmd: Cmd) -> bool {
        let has = self.model.is_some();
        let m = self.model.as_ref();
        match cmd {
            Cmd::Save => self.is_dirty(),
            Cmd::SaveLeftAs | Cmd::SaveRightAs | Cmd::Find | Cmd::FindNext | Cmd::FindPrev | Cmd::GotoLine | Cmd::SwapSides | Cmd::SelectAll => has,
            Cmd::EditLeftWhole | Cmd::EditRightWhole | Cmd::ExportHtmlReport | Cmd::ExportPatch => has,
            Cmd::Refresh => true,
            Cmd::NextDiff | Cmd::PrevDiff | Cmd::FirstDiff | Cmd::LastDiff => m.is_some_and(|m| !m.blocks.is_empty()),
            Cmd::CopyToRight | Cmd::CopyToLeft => self.target_rows().is_some(),
            Cmd::Undo => m.is_some_and(TextModel::can_undo),
            Cmd::Redo => m.is_some_and(TextModel::can_redo),
            Cmd::Copy | Cmd::Delete => self.sel.is_some(),
            _ => false,
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if self.edit.is_some() {
            self.commit_edit(Move::Stay);
        }
        match cmd {
            Cmd::Save => {
                self.save_all(cx);
            }
            Cmd::SaveLeftAs => {
                self.save_side(0, true, cx);
            }
            Cmd::SaveRightAs => {
                self.save_side(1, true, cx);
            }
            Cmd::Refresh => self.request_reload(ctx, [true, true]),
            Cmd::NextDiff => self.navigate(true, false),
            Cmd::PrevDiff => self.navigate(false, false),
            Cmd::FirstDiff => self.navigate(true, true),
            Cmd::LastDiff => self.navigate(false, true),
            Cmd::CopyToRight | Cmd::CopyToLeft => {
                if let Some(rows) = self.target_rows() {
                    self.apply(vec![Action::CopyRows { rows, to_right: cmd == Cmd::CopyToRight }], ctx);
                }
            }
            Cmd::Undo => {
                if let Some(m) = &mut self.model {
                    m.undo();
                }
            }
            Cmd::Redo => {
                if let Some(m) = &mut self.model {
                    m.redo();
                }
            }
            Cmd::Find => {
                self.find.open = true;
                self.find.focus = true;
            }
            Cmd::FindNext => {
                if self.find.query.is_empty() {
                    self.find.open = true;
                    self.find.focus = true;
                } else {
                    self.find_step(true);
                }
            }
            Cmd::FindPrev => self.find_step(false),
            Cmd::GotoLine => {
                self.goto = Some(String::new());
                self.goto_focus = true;
            }
            Cmd::SwapSides => {
                self.paths.swap(0, 1);
                self.disk_changed.swap(0, 1);
                if let Some(m) = &mut self.model {
                    m.swap();
                }
                self.sel = None;
            }
            Cmd::SelectAll => {
                let n = self.model.as_ref().map_or(0, |m| m.rows.len());
                if n > 0 {
                    self.sel = Some(Selection { side: self.sel.map_or(0, |s| s.side), anchor: 0, cursor: n - 1 });
                }
            }
            Cmd::Copy => {
                if let Some(s) = self.sel {
                    self.apply(vec![Action::CopyText { rows: s.rows(), side: s.side }], ctx);
                }
            }
            Cmd::Delete => {
                if let Some(s) = self.sel {
                    self.apply(vec![Action::DeleteRows { rows: s.rows(), side: s.side }], ctx);
                }
            }
            Cmd::EditLeftWhole | Cmd::EditRightWhole => {
                let side = usize::from(cmd == Cmd::EditRightWhole);
                if let Some(m) = &self.model {
                    self.whole = Some(WholeEdit { side, text: m.lines(side).join("\n") });
                }
            }
            Cmd::ExportHtmlReport => {
                let Some(m) = &self.model else { return };
                let name = format!("{}-vs-{}.html", self.file_stem(0), self.file_stem(1));
                if let Some(path) = widgets::save_file(None, &name, Some(("HTML", &["html", "htm"]))) {
                    let opts = diffcraft_report::TextReportOptions {
                        title: format!("Text Compare: {}", self.title()),
                        left_label: self.paths[0].clone(),
                        right_label: self.paths[1].clone(),
                        context: if m.display == Display::All { None } else { Some(crate::text_model::CONTEXT_LINES) },
                        tab_width: self.tab_width,
                        intraline: cx.settings.intraline_highlight,
                        rules_summary: rules_summary(&self.opts),
                    };
                    let html = diffcraft_report::text_html_report(m.lines(0), m.lines(1), &m.rows, &opts);
                    write_export(&path, html.as_bytes(), cx);
                }
            }
            Cmd::ExportPatch => {
                let name = format!("{}.patch", self.file_stem(1));
                if let Some(text) = self.patch_text()
                    && let Some(path) = widgets::save_file(None, &name, Some(("Patch", &["patch", "diff"])))
                {
                    write_export(&path, text.as_bytes(), cx);
                }
            }
            _ => {}
        }
    }

    fn is_dirty(&self) -> bool {
        self.model.as_ref().is_some_and(TextModel::is_dirty)
    }

    fn save_all(&mut self, cx: &mut Cx<'_>) -> bool {
        let dirty: Vec<usize> = self.model.as_ref().map(|m| (0..2).filter(|&i| m.sides.get(i).is_some_and(|s| s.dirty)).collect()).unwrap_or_default();
        let mut ok = true;
        for side in dirty {
            ok &= self.save_side(side, false, cx);
        }
        ok
    }

    fn session(&self) -> Option<Session> {
        Some(Session { text: self.opts.clone(), ..Session::new(SessionKind::TextCompare, self.paths[0].clone(), self.paths[1].clone()) })
    }

    fn wants_keyboard(&self) -> bool {
        self.edit.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_ranges_respect_case_and_boundaries() {
        assert_eq!(find_ranges("Hello hello", "hello", false), vec![0..5, 6..11]);
        assert_eq!(find_ranges("Hello hello", "hello", true), vec![6..11]);
        assert!(find_ranges("x", "", false).is_empty());
        // Lowercasing changes the byte length (İ): no highlights rather than wrong ones.
        assert!(find_ranges("İx", "x", false).is_empty());
    }

    #[test]
    fn display_text_limits() {
        let long = "a".repeat(MAX_DISPLAY_CHARS + 10);
        assert_eq!(display_text(&long, 4).chars().count(), MAX_DISPLAY_CHARS + 1);
        assert_eq!(display_text("\tx", 4), "    x");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn line_job_sections() {
        let job = line_job("abcdef", FontId::monospace(12.0), Color32::WHITE, &[1..3], Color32::RED, &[2..4], Color32::YELLOW);
        let backgrounds: Vec<Color32> = job.sections.iter().map(|s| s.format.background).collect();
        assert_eq!(backgrounds, vec![Color32::TRANSPARENT, Color32::RED, Color32::YELLOW, Color32::TRANSPARENT]);
    }

    #[test]
    fn load_side_cases() {
        let d = tempfile::tempdir().unwrap();
        assert!(!load_side("").unwrap().exists);
        let missing = load_side(&d.path().join("nope.txt").display().to_string()).unwrap();
        assert!(!missing.exists && missing.path.is_some());
        assert!(load_side(&d.path().display().to_string()).unwrap_err().contains("folder"));
        let bin = d.path().join("b.bin");
        std::fs::write(&bin, b"\0\0\x01").unwrap();
        assert!(load_side(&bin.display().to_string()).unwrap_err().contains("binary"));
        let t = d.path().join("t.txt");
        std::fs::write(&t, "a\r\nb\r\n").unwrap();
        let s = load_side(&t.display().to_string()).unwrap();
        assert_eq!(s.file.lines, vec!["a", "b"]);
        assert_eq!(s.file.line_ending, LineEnding::CrLf);
    }
}
