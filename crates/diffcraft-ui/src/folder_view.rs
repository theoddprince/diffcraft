//! Folder Compare: two folder trees side by side with status colours, filters, expand/collapse,
//! file operations (copy, move, delete, rename, new folder) and reports.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use diffcraft_core::folder::{self, FolderCompareOptions, FolderComparison, Node, Status};
use diffcraft_core::fsutil;
use diffcraft_session::{Session, SessionKind, Settings};
use egui::{Align, Align2, Color32, FontId, Key, Layout, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::icons::{self, Icon};
use crate::jobs::{self, Job};
use crate::theme::Palette;
use crate::toolbar;
use crate::view::{Cmd, Cx, OpenRequest, Toasts, View};
use crate::widgets;

/// Which entries are listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShowFilter {
    #[default]
    All,
    Differences,
    Same,
    Orphans,
    LeftNewer,
    RightNewer,
}

impl ShowFilter {
    fn label(self) -> &'static str {
        match self {
            ShowFilter::All => "Show all",
            ShowFilter::Differences => "Show differences",
            ShowFilter::Same => "Show same",
            ShowFilter::Orphans => "Show orphans",
            ShowFilter::LeftNewer => "Show left newer",
            ShowFilter::RightNewer => "Show right newer",
        }
    }

    /// Does an entry with this status pass?
    fn passes(self, s: Status) -> bool {
        match self {
            ShowFilter::All => true,
            ShowFilter::Differences => s != Status::Same,
            ShowFilter::Same => s == Status::Same,
            ShowFilter::Orphans => s.is_orphan(),
            ShowFilter::LeftNewer => s == Status::LeftNewer,
            ShowFilter::RightNewer => s == Status::RightNewer,
        }
    }
}

/// The rows to list: depth-first order, honouring expansion and the show filter.
/// Folders are listed when they themselves pass (orphan folders, empty folders) or when
/// anything inside them does.
pub fn visible_rows(cmp: &FolderComparison, expanded: &HashSet<PathBuf>, show: ShowFilter) -> Vec<usize> {
    let n = cmp.nodes.len();
    let mut passes = vec![false; n];
    for i in (0..n).rev() {
        let Some(node) = cmp.nodes.get(i) else { continue };
        let own = if node.is_dir() { (node.status.is_orphan() || node.children.is_empty()) && show.passes(node.status) } else { show.passes(node.status) };
        let child = node.children.iter().any(|&c| passes.get(c).copied().unwrap_or(false));
        if let Some(p) = passes.get_mut(i) {
            *p = own || child;
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<usize> = cmp.nodes.first().map(|r| r.children.iter().rev().copied().collect()).unwrap_or_default();
    while let Some(i) = stack.pop() {
        if !passes.get(i).copied().unwrap_or(false) {
            continue;
        }
        out.push(i);
        if let Some(node) = cmp.nodes.get(i)
            && node.is_dir()
            && expanded.contains(&node.rel)
        {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileOp {
    Copy { to_right: bool },
    Move { to_right: bool },
    Delete { left: bool },
}

impl FileOp {
    fn verb(&self) -> String {
        match self {
            FileOp::Copy { to_right } => format!("Copy to {}", if *to_right { "right" } else { "left" }),
            FileOp::Move { to_right } => format!("Move to {}", if *to_right { "right" } else { "left" }),
            FileOp::Delete { left } => format!("Delete from {}", if *left { "left" } else { "right" }),
        }
    }
}

struct PendingOp {
    op: FileOp,
    nodes: Vec<usize>,
    /// (source, target) pairs; deletions have no target.
    pairs: Vec<(PathBuf, Option<PathBuf>)>,
}

struct OpResult {
    failures: Vec<String>,
    done: usize,
    /// Only plain files were touched: refresh them in place instead of a full recompare.
    files_only: bool,
    nodes: Vec<usize>,
}

struct NameDialog {
    title: String,
    name: String,
    /// Rename: the entry; New folder: the parent directory.
    path: PathBuf,
    rename: bool,
    focus: bool,
}

/// Context-menu choices that need `&mut self` after painting.
#[derive(Clone, Copy)]
enum MenuChoice {
    Cmd(Cmd),
    Op(OpChoice),
}

#[derive(Clone, Copy)]
enum OpChoice {
    MoveRight,
    MoveLeft,
}

pub struct FolderView {
    roots: [String; 2],
    opts: FolderCompareOptions,
    filter_text: String,
    job: Option<Job<FolderComparison>>,
    cmp: Option<FolderComparison>,
    error: Option<String>,
    expanded: HashSet<PathBuf>,
    first_load: bool,
    show: ShowFilter,
    visible: Vec<usize>,
    visible_dirty: bool,
    sel: BTreeSet<usize>,
    anchor: Option<usize>,
    focus_side: usize,
    scroll_to: Option<usize>,
    confirm: Option<PendingOp>,
    op_job: Option<Job<OpResult>>,
    name_dialog: Option<NameDialog>,
    restore_sel: Vec<PathBuf>,
    elapsed: Option<f32>,
    /// Item counts per side for the info strips.
    info: [String; 2],
}

impl FolderView {
    pub fn from_session(s: &Session, _settings: &Settings, ctx: &egui::Context) -> Self {
        let mut v = FolderView {
            roots: [s.left.clone(), s.right.clone()],
            filter_text: s.folder.filter.to_spec(),
            opts: s.folder.clone(),
            job: None,
            cmp: None,
            error: None,
            expanded: HashSet::new(),
            first_load: true,
            show: ShowFilter::All,
            visible: Vec::new(),
            visible_dirty: true,
            sel: BTreeSet::new(),
            anchor: None,
            focus_side: 0,
            scroll_to: None,
            confirm: None,
            op_job: None,
            name_dialog: None,
            restore_sel: Vec::new(),
            elapsed: None,
            info: Default::default(),
        };
        if !s.left.trim().is_empty() && !s.right.trim().is_empty() {
            v.compare(ctx);
        }
        v
    }

    fn compare(&mut self, ctx: &egui::Context) {
        self.error = None;
        if self.roots.iter().any(|p| p.trim().is_empty()) {
            self.error = Some("Choose a folder for each side.".into());
            return;
        }
        let (l, r) = (widgets::clean_path(&self.roots[0]), widgets::clean_path(&self.roots[1]));
        // Remember the selection by path so it survives the new comparison.
        if let Some(c) = &self.cmp {
            self.restore_sel = self.sel.iter().filter_map(|&i| c.nodes.get(i).map(|n| n.rel.clone())).collect();
        }
        if let Some(j) = &self.job {
            j.cancel();
        }
        let opts = self.opts.clone();
        self.job = Some(Job::spawn("Comparing folders", ctx, move |p| folder::compare_folders(&l, &r, &opts, p).map_err(|e| e.to_string())));
    }

    fn poll(&mut self, toasts: &mut Toasts, ctx: &egui::Context) {
        if let Some(job) = &mut self.job
            && let Some(res) = job.poll()
        {
            self.elapsed = Some(job.elapsed().as_secs_f32());
            self.job = None;
            match res {
                Ok(c) => {
                    if self.first_load {
                        self.first_load = false;
                        // Open the folders that contain differences.
                        let big = c.nodes.len() > 20_000;
                        for n in &c.nodes {
                            if n.is_dir() && n.status != Status::Same && n.left.is_some() && n.right.is_some() && (!big || n.depth <= 1) {
                                self.expanded.insert(n.rel.clone());
                            }
                        }
                    }
                    let keep: HashSet<PathBuf> = std::mem::take(&mut self.restore_sel).into_iter().collect();
                    self.sel = c.nodes.iter().enumerate().filter(|(_, n)| keep.contains(&n.rel)).map(|(i, _)| i).collect();
                    self.anchor = None;
                    self.cmp = Some(c);
                    self.visible_dirty = true;
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(job) = &mut self.op_job
            && let Some(res) = job.poll()
        {
            self.op_job = None;
            match res {
                Ok(r) => {
                    for f in r.failures.iter().take(5) {
                        toasts.error(f.clone());
                    }
                    if r.failures.len() > 5 {
                        toasts.error(format!("…and {} more errors", r.failures.len() - 5));
                    }
                    if r.done > 0 {
                        toasts.success(format!("{} item{} done", r.done, if r.done == 1 { "" } else { "s" }));
                    }
                    if r.files_only
                        && let Some(c) = &mut self.cmp
                    {
                        for i in r.nodes {
                            c.refresh_file(i);
                        }
                        self.visible_dirty = true;
                    } else {
                        self.compare(ctx);
                    }
                }
                Err(e) => toasts.error(e),
            }
        }
    }

    fn ensure_visible(&mut self) {
        if !self.visible_dirty {
            return;
        }
        self.visible_dirty = false;
        self.visible = self.cmp.as_ref().map(|c| visible_rows(c, &self.expanded, self.show)).unwrap_or_default();
        self.info = self.cmp.as_ref().map(side_info).unwrap_or_default();
        self.sel.retain(|i| self.visible.contains(i));
    }

    /// Selected nodes without those inside another selected folder.
    fn top_selection(&self) -> Vec<usize> {
        let Some(c) = &self.cmp else { return Vec::new() };
        self.sel
            .iter()
            .copied()
            .filter(|&i| {
                let mut p = c.nodes.get(i).and_then(|n| n.parent);
                while let Some(k) = p {
                    if self.sel.contains(&k) {
                        return false;
                    }
                    p = c.nodes.get(k).and_then(|n| n.parent);
                }
                true
            })
            .collect()
    }

    fn prepare_op(&mut self, op: FileOp, settings: &Settings, ctx: &egui::Context) {
        let Some(c) = &self.cmp else { return };
        let mut pairs = Vec::new();
        let mut used = Vec::new();
        for i in self.top_selection() {
            match &op {
                FileOp::Copy { to_right } | FileOp::Move { to_right } => {
                    let from = if *to_right { c.left_path(i) } else { c.right_path(i) };
                    if let (Some(from), Some(to)) = (from, c.target_path(i, *to_right)) {
                        pairs.push((from, Some(to)));
                        used.push(i);
                    }
                }
                FileOp::Delete { left } => {
                    if let Some(p) = if *left { c.left_path(i) } else { c.right_path(i) } {
                        pairs.push((p, None));
                        used.push(i);
                    }
                }
            }
        }
        if pairs.is_empty() {
            return;
        }
        let pending = PendingOp { op, nodes: used, pairs };
        let permanent_delete = matches!(pending.op, FileOp::Delete { .. }) && !settings.delete_to_trash;
        if settings.confirm_file_operations || permanent_delete {
            self.confirm = Some(pending);
        } else {
            self.run_op(pending, settings.delete_to_trash, ctx);
        }
    }

    fn run_op(&mut self, p: PendingOp, to_trash: bool, ctx: &egui::Context) {
        let files_only = self.cmp.as_ref().is_some_and(|c| p.nodes.iter().all(|&i| c.nodes.get(i).is_some_and(|n| !n.is_dir())));
        let PendingOp { op, nodes, pairs } = p;
        let label = op.verb();
        self.op_job = Some(Job::spawn(label, ctx, move |progress| {
            progress.files_to_compare.store(pairs.len() as u64, Ordering::Relaxed);
            let mut failures = Vec::new();
            let mut done = 0;
            for (src, dst) in &pairs {
                if progress.is_cancelled() {
                    failures.push("cancelled".to_owned());
                    break;
                }
                progress.set_current(src.display().to_string());
                let res = match (&op, dst) {
                    (FileOp::Copy { .. }, Some(dst)) => fsutil::copy_entry(src, dst),
                    (FileOp::Move { .. }, Some(dst)) => fsutil::copy_entry(src, dst).and_then(|()| fsutil::delete_entry(src, false)),
                    (FileOp::Delete { .. }, _) => fsutil::delete_entry(src, to_trash),
                    _ => Ok(()),
                };
                match res {
                    Ok(()) => done += 1,
                    Err(e) => failures.push(e.to_string()),
                }
                progress.files_compared.fetch_add(1, Ordering::Relaxed);
            }
            Ok(OpResult { failures, done, files_only: files_only && !matches!(op, FileOp::Move { .. }), nodes })
        }));
    }

    fn open_selected(&self, cx: &mut Cx<'_>, kind: Option<SessionKind>) {
        let Some(c) = &self.cmp else { return };
        for &i in self.sel.iter().take(10) {
            let Some(n) = c.nodes.get(i) else { continue };
            let (Some(l), Some(r)) = (c.target_path(i, false), c.target_path(i, true)) else { continue };
            if n.is_dir() {
                cx.open.push(OpenRequest::Paths { left: l, right: r, kind: Some(SessionKind::FolderCompare) });
            } else {
                cx.open.push(OpenRequest::Paths { left: l, right: r, kind });
            }
        }
    }

    fn toggle(&mut self, i: usize) {
        let Some(n) = self.cmp.as_ref().and_then(|c| c.nodes.get(i)) else { return };
        if !n.is_dir() {
            return;
        }
        if !self.expanded.remove(&n.rel) {
            self.expanded.insert(n.rel.clone());
        }
        self.visible_dirty = true;
    }

    fn set_all_expanded(&mut self, on: bool) {
        self.expanded.clear();
        if on && let Some(c) = &self.cmp {
            self.expanded.extend(c.nodes.iter().filter(|n| n.is_dir()).map(|n| n.rel.clone()));
        }
        self.visible_dirty = true;
    }

    fn select_row(&mut self, vi: usize, ctrl: bool, shift: bool) {
        let Some(&i) = self.visible.get(vi) else { return };
        if shift && let Some(a) = self.anchor {
            let (lo, hi) = (a.min(vi), a.max(vi));
            if !ctrl {
                self.sel.clear();
            }
            self.sel.extend(self.visible.get(lo..=hi).unwrap_or_default().iter().copied());
        } else if ctrl {
            if !self.sel.remove(&i) {
                self.sel.insert(i);
            }
            self.anchor = Some(vi);
        } else {
            self.sel.clear();
            self.sel.insert(i);
            self.anchor = Some(vi);
        }
    }

    fn keyboard(&mut self, ui: &Ui) {
        if ui.ctx().egui_wants_keyboard_input() || self.visible.is_empty() || self.name_dialog.is_some() || self.confirm.is_some() {
            return;
        }
        let (up, down, left, right, shift) = ui.input(|i| {
            (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight), i.modifiers.shift)
        });
        let cur = self.anchor.filter(|&a| a < self.visible.len());
        if up || down {
            let next = match cur {
                None => 0,
                Some(c) if up => c.saturating_sub(1),
                Some(c) => (c + 1).min(self.visible.len() - 1),
            };
            if shift {
                self.select_row(next, false, true);
            } else {
                self.select_row(next, false, false);
            }
            self.anchor = Some(next);
            self.scroll_to = Some(next);
        }
        if (left || right)
            && let Some(c) = cur
            && let Some(&i) = self.visible.get(c)
            && let Some(n) = self.cmp.as_ref().and_then(|cmp| cmp.nodes.get(i))
            && n.is_dir()
            && self.expanded.contains(&n.rel) != right
        {
            self.toggle(i);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let has = self.cmp.is_some();
        let sel = !self.sel.is_empty() && self.op_job.is_none();
        let mut act: Option<Cmd> = None;
        let mut show = self.show;
        let mut rules_changed = false;
        let mut copy_summary = false;
        let rules_tip = format!("Comparison rules and filters (now comparing by {})", self.opts.mode.label().to_lowercase());
        let (opts, filter_text) = (&mut self.opts, &mut self.filter_text);
        toolbar::bar(ui, "folder-toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            for (f, icon, label, tip) in [
                (ShowFilter::All, Icon::ShowAll, "All", "Show all files and folders"),
                (ShowFilter::Differences, Icon::ShowDiffs, "Diffs", "Show only differences and orphans"),
                (ShowFilter::Same, Icon::ShowSame, "Same", "Show only identical files"),
                (ShowFilter::Orphans, Icon::Context, "Orphans", "Show only items that exist on one side"),
            ] {
                if toolbar::toggle(ui, icon, label, tip, show == f, true) {
                    show = f;
                }
            }
            let newer_tip = match show {
                ShowFilter::LeftNewer => "Showing files that are newer on the left",
                ShowFilter::RightNewer => "Showing files that are newer on the right",
                _ => "Show only files that are newer on one side",
            };
            toolbar::menu(ui, Icon::Filter, "Newer", newer_tip, true, |ui| {
                for f in [ShowFilter::LeftNewer, ShowFilter::RightNewer] {
                    if ui.selectable_label(show == f, f.label()).clicked() {
                        show = f;
                        ui.close();
                    }
                }
            });
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::ExpandAll, "Expand", "Expand all folders", has) {
                act = Some(Cmd::ExpandAll);
            }
            if toolbar::button(ui, Icon::CollapseAll, "Collapse", "Collapse all folders", has) {
                act = Some(Cmd::CollapseAll);
            }
            toolbar::menu(ui, Icon::Rules, "Rules", &rules_tip, true, |ui| {
                ui.set_min_width(340.0);
                rules_changed |= crate::rules_ui::folder_rules(ui, opts, filter_text, "folder-rules");
            });
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::CopyRight, "Copy →", "Copy the selection to the right (Ctrl+R)", sel) {
                act = Some(Cmd::CopyToRight);
            }
            if toolbar::button(ui, Icon::CopyLeft, "← Copy", "Copy the selection to the left (Ctrl+L)", sel) {
                act = Some(Cmd::CopyToLeft);
            }
            if toolbar::button(ui, Icon::Delete, "Delete", "Delete the selection on the side you clicked (Del)", sel) {
                act = Some(Cmd::Delete);
            }
            if toolbar::button(ui, Icon::Sync, "Sync", "Open these folders in a Folder Sync session", has) {
                act = Some(Cmd::Sync);
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Swap, "Swap", "Swap the left and right folders", true) {
                act = Some(Cmd::SwapSides);
            }
            if toolbar::button(ui, Icon::Reload, "Reload", "Compare the folders again (F5)", true) {
                act = Some(Cmd::Refresh);
            }
            toolbar::separator(ui);
            toolbar::menu(ui, Icon::Report, "Report", "Export or copy a report of this comparison", has, |ui| {
                if toolbar::menu_item(ui, Icon::Report, "HTML Report…", "", has) {
                    act = Some(Cmd::ExportHtmlReport);
                    ui.close();
                }
                if toolbar::menu_item(ui, Icon::File, "CSV Report…", "", has) {
                    act = Some(Cmd::ExportCsv);
                    ui.close();
                }
                if toolbar::menu_item(ui, Icon::Edit, "Copy Summary", "", has) {
                    copy_summary = true;
                    ui.close();
                }
            });
        });
        if show != self.show {
            self.show = show;
            self.visible_dirty = true;
        }
        if rules_changed {
            self.compare(&ctx);
        }
        if copy_summary && let Some(c) = &self.cmp {
            ctx.copy_text(diffcraft_report::folder_text_summary(c));
        }
        if let Some(cmd) = act {
            self.run(cmd, &ctx, cx);
        }
    }

    fn status_bar(&self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        toolbar::status_bar(ui, "folder-status", |ui| {
            let Some(c) = &self.cmp else {
                ui.label(RichText::new(if self.job.is_some() { "Comparing…" } else { "Ready" }).color(pal.dim_text));
                return;
            };
            let s = c.summary();
            let n = |v: usize| widgets::thousands(v as u64);
            let diffs = s.different + s.left_only + s.right_only + s.errors;
            if diffs == 0 {
                toolbar::status_icon(ui, Icon::Check);
                ui.colored_label(pal.success, "No differences");
            } else {
                toolbar::status_icon(ui, Icon::ShowDiffs);
                ui.label(format!("{} difference{}", n(diffs), if diffs == 1 { "" } else { "s" }));
            }
            toolbar::status_sep(ui);
            ui.label(format!("{} same", n(s.same)));
            ui.colored_label(if s.different > 0 { pal.different_text } else { pal.dim_text }, format!("{} different", n(s.different)));
            let orphan = |v: usize| if v > 0 { pal.orphan_text } else { pal.dim_text };
            ui.colored_label(orphan(s.left_only), format!("{} left only", n(s.left_only)));
            ui.colored_label(orphan(s.right_only), format!("{} right only", n(s.right_only)));
            if s.errors > 0 {
                ui.colored_label(pal.warning, format!("{} error{}", n(s.errors), if s.errors == 1 { "" } else { "s" }));
            }
            ui.label(RichText::new(format!("{} folders", n(s.folders))).color(pal.dim_text));
            toolbar::status_sep(ui);
            ui.label(format!("Compared by {}", c.options.mode.label().to_lowercase()));
            if let Some(t) = self.elapsed {
                toolbar::status_sep(ui);
                ui.label(format!("{t:.2} s"));
            }
            if !self.sel.is_empty() {
                toolbar::status_sep(ui);
                ui.label(format!("{} selected", n(self.sel.len())));
            }
        });
    }

    fn body(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        self.ensure_visible();
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        let Some(c) = &self.cmp else { return };
        let font = FontId::proportional(13.0);
        let full = ui.available_rect_before_wrap();
        let g = Geom::new(full.width(), ui.spacing().scroll.bar_width);
        let stroke = Stroke::new(1.0, pal.border);
        // Column headers.
        let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 20.0), Sense::hover());
        {
            let p = ui.painter();
            p.rect_filled(hdr, 0.0, pal.header_bg);
            let hf = FontId::proportional(12.0);
            for side in 0..2 {
                let x0 = g.x0(hdr.left(), side);
                p.text(pos2(x0 + 8.0, hdr.center().y), Align2::LEFT_CENTER, "Name", hf.clone(), pal.dim_text);
                p.vline(x0 + g.name_w, hdr.y_range().shrink(3.0), stroke);
                p.text(pos2(x0 + g.name_w + g.size_w - 6.0, hdr.center().y), Align2::RIGHT_CENTER, "Size", hf.clone(), pal.dim_text);
                if g.date_w > 0.0 {
                    p.vline(x0 + g.name_w + g.size_w, hdr.y_range().shrink(3.0), stroke);
                    p.text(pos2(x0 + g.name_w + g.size_w + 8.0, hdr.center().y), Align2::LEFT_CENTER, "Modified (UTC)", hf.clone(), pal.dim_text);
                }
            }
            p.vline(hdr.left() + g.half + 0.5, hdr.y_range(), stroke);
            p.vline(hdr.left() + g.half + CENTER_W - 0.5, hdr.y_range(), stroke);
            p.hline(hdr.x_range(), hdr.bottom() - 0.5, stroke);
        }
        let grid = Rect::from_min_max(pos2(full.left(), hdr.bottom()), full.max);
        {
            // Pane backgrounds and the centre column continue below the last row.
            let p = ui.painter();
            p.rect_filled(grid, 0.0, pal.pane_bg);
            let center = Rect::from_min_size(pos2(grid.left() + g.half, grid.top()), vec2(CENTER_W, grid.height()));
            p.rect_filled(center, 0.0, pal.gutter_bg);
            p.vline(center.left() + 0.5, grid.y_range(), stroke);
            p.vline(center.right() - 0.5, grid.y_range(), stroke);
        }
        let mut area = egui::ScrollArea::vertical().id_salt("folder-grid").auto_shrink(false);
        if let Some(vi) = self.scroll_to.take() {
            area = area.vertical_scroll_offset((vi as f32 * ROW_H - grid.height() / 2.0).max(0.0));
        }
        let mut clicked: Option<(usize, bool, bool, usize)> = None;
        let mut toggled: Option<usize> = None;
        let mut double: Option<usize> = None;
        let mut menu: Option<MenuChoice> = None;
        let mut menu_open: Option<SessionKind> = None;
        let mut reveal: Option<PathBuf> = None;
        let mut copy_paths: Option<String> = None;
        let visible = &self.visible;
        let sel = &self.sel;
        let expanded = &self.expanded;
        let mut child = ui.new_child(UiBuilder::new().max_rect(grid));
        child.spacing_mut().item_spacing.y = 0.0;
        area.show_rows(&mut child, ROW_H, visible.len(), |ui, range| {
            for vi in range {
                let Some(&i) = visible.get(vi) else { continue };
                let Some(n) = c.nodes.get(i) else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let painter = ui.painter_at(rect);
                let selected = sel.contains(&i);
                let is_dir = n.is_dir();
                let level = n.depth.saturating_sub(1);
                let indent = 6.0 + level as f32 * INDENT;
                let cy = rect.center().y;
                for side in 0..2 {
                    let x0 = g.x0(rect.left(), side);
                    let cell = Rect::from_min_size(pos2(x0, rect.top()), vec2(g.half, ROW_H));
                    let meta = if side == 0 { n.left.as_ref() } else { n.right.as_ref() };
                    let Some(meta) = meta else {
                        pal.hatch_rect(&painter, cell);
                        if selected {
                            painter.rect_filled(cell, 0.0, pal.selection_bg);
                        }
                        continue;
                    };
                    painter.rect_filled(cell, 0.0, if vi % 2 == 1 { pal.gutter_bg } else { pal.pane_bg });
                    if n.status.is_orphan() {
                        painter.rect_filled(cell, 0.0, pal.orphan_text.gamma_multiply(if pal.dark { 0.14 } else { 0.09 }));
                    } else if n.status == Status::Error {
                        painter.rect_filled(cell, 0.0, pal.conflict_bg);
                    }
                    if selected {
                        painter.rect_filled(cell, 0.0, pal.selection_bg);
                    }
                    // Indentation guides through the ancestors' expand triangles.
                    for k in 0..level {
                        let x = (x0 + 6.0 + k as f32 * INDENT + 5.0).round() + 0.5;
                        painter.vline(x, rect.y_range(), stroke);
                    }
                    let color = pal.status_text(n.status, side == 0);
                    let tx = x0 + indent;
                    if is_dir {
                        paint_triangle(&painter, pos2(tx + 5.0, cy), expanded.contains(&n.rel), pal.dim_text);
                    }
                    let ir = Rect::from_min_size(pos2(tx + 13.0, cy - 7.0), vec2(14.0, 14.0));
                    icons::paint(&painter, ir, if is_dir { Icon::Folder } else { Icon::File }, &pal, true);
                    let name = meta.name.to_string_lossy();
                    let name_rect = Rect::from_min_max(pos2(tx + 31.0, rect.top()), pos2(x0 + g.name_w - 4.0, rect.bottom()));
                    if name_rect.width() > 4.0 {
                        painter.with_clip_rect(name_rect.intersect(rect)).text(pos2(name_rect.left(), cy), Align2::LEFT_CENTER, name, font.clone(), color);
                    }
                    if !is_dir {
                        painter.text(pos2(x0 + g.name_w + g.size_w - 6.0, cy), Align2::RIGHT_CENTER, widgets::thousands(meta.size), font.clone(), color);
                    }
                    if g.date_w > 0.0 {
                        painter.text(pos2(x0 + g.name_w + g.size_w + 8.0, cy), Align2::LEFT_CENTER, widgets::format_mtime(meta.modified), font.clone(), color);
                    }
                }
                // Centre status marker between the two panes.
                let center = Rect::from_min_size(pos2(rect.left() + g.half, rect.top()), vec2(CENTER_W, ROW_H));
                painter.rect_filled(center, 0.0, pal.gutter_bg);
                painter.vline(center.left() + 0.5, rect.y_range(), stroke);
                painter.vline(center.right() - 0.5, rect.y_range(), stroke);
                paint_marker(&painter, center, n.status, &pal);
                let tip = match &n.error {
                    Some(e) => format!("{}: {e}", n.status.label()),
                    None => n.status.label().to_owned(),
                };
                let r = resp.on_hover_text_at_pointer(tip);
                if r.clicked()
                    && let Some(p) = r.interact_pointer_pos()
                {
                    let side = usize::from(p.x > center.center().x);
                    let x0 = g.x0(rect.left(), side);
                    let arrow_hit = is_dir && p.x >= x0 + indent - 4.0 && p.x <= x0 + indent + 12.0;
                    if arrow_hit {
                        toggled = Some(i);
                    } else {
                        let m = ui.input(|i| i.modifiers);
                        clicked = Some((vi, m.command, m.shift, side));
                    }
                }
                if r.double_clicked() {
                    double = Some(i);
                }
                if r.secondary_clicked() && !sel.contains(&i) {
                    let side = r.interact_pointer_pos().map_or(0, |p| usize::from(p.x > center.center().x));
                    clicked = Some((vi, false, false, side));
                }
                r.context_menu(|ui| {
                    if toolbar::menu_item(ui, if is_dir { Icon::FolderOpen } else { Icon::File }, "Open", "Enter", true) {
                        double = Some(i);
                        ui.close();
                    }
                    if !is_dir {
                        ui.menu_button("Open With", |ui| {
                            for k in [SessionKind::TextCompare, SessionKind::HexCompare, SessionKind::PictureCompare] {
                                if toolbar::menu_item(ui, icons::session_icon(k), k.label(), "", true) {
                                    menu_open = Some(k);
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.separator();
                    for (icon, label, choice) in [
                        (Icon::CopyRight, "Copy to Right", MenuChoice::Cmd(Cmd::CopyToRight)),
                        (Icon::CopyLeft, "Copy to Left", MenuChoice::Cmd(Cmd::CopyToLeft)),
                        (Icon::CopyRight, "Move to Right", MenuChoice::Op(OpChoice::MoveRight)),
                        (Icon::CopyLeft, "Move to Left", MenuChoice::Op(OpChoice::MoveLeft)),
                        (Icon::Delete, "Delete Left", MenuChoice::Cmd(Cmd::DeleteLeft)),
                        (Icon::Delete, "Delete Right", MenuChoice::Cmd(Cmd::DeleteRight)),
                        (Icon::Edit, "Rename…", MenuChoice::Cmd(Cmd::Rename)),
                        (Icon::Plus, "New Folder…", MenuChoice::Cmd(Cmd::NewFolder)),
                    ] {
                        if toolbar::menu_item(ui, icon, label, "", true) {
                            menu = Some(choice);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if toolbar::menu_item(ui, Icon::Edit, "Copy Path", "", true) {
                        copy_paths = c.left_path(i).or_else(|| c.right_path(i)).map(|p| p.display().to_string());
                        ui.close();
                    }
                    if toolbar::menu_item(ui, Icon::FolderOpen, "Show in File Manager", "", true) {
                        reveal = c.left_path(i).or_else(|| c.right_path(i));
                        ui.close();
                    }
                });
            }
        });
        if let Some(i) = toggled {
            self.toggle(i);
        }
        if let Some((vi, ctrl, shift, side)) = clicked {
            self.focus_side = side;
            self.select_row(vi, ctrl, shift);
        }
        if let Some(i) = double {
            let is_dir = self.cmp.as_ref().and_then(|c| c.nodes.get(i)).is_some_and(Node::is_dir);
            if is_dir {
                self.toggle(i);
            } else {
                self.sel.clear();
                self.sel.insert(i);
                self.open_selected(cx, None);
            }
        }
        if let Some(k) = menu_open {
            self.open_selected(cx, Some(k));
        }
        if let Some(t) = copy_paths {
            ctx.copy_text(t);
        }
        if let Some(p) = reveal {
            reveal_in_file_manager(&p, cx.toasts);
        }
        match menu {
            Some(MenuChoice::Op(OpChoice::MoveRight)) => self.prepare_op(FileOp::Move { to_right: true }, cx.settings, &ctx),
            Some(MenuChoice::Op(OpChoice::MoveLeft)) => self.prepare_op(FileOp::Move { to_right: false }, cx.settings, &ctx),
            Some(MenuChoice::Cmd(cmd)) => self.run(cmd, &ctx, cx),
            None => {}
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if let Some(p) = &self.confirm {
            let mut body = String::new();
            for (a, b) in p.pairs.iter().take(12) {
                match b {
                    Some(b) => body.push_str(&format!("{}\n   → {}\n", a.display(), b.display())),
                    None => body.push_str(&format!("{}\n", a.display())),
                }
            }
            if p.pairs.len() > 12 {
                body.push_str(&format!("…and {} more\n", p.pairs.len() - 12));
            }
            if let FileOp::Delete { .. } = p.op {
                body.push_str(if cx.settings.delete_to_trash { "\nItems go to the Recycle Bin / Trash." } else { "\nItems are deleted permanently." });
            }
            let title = format!("{} {} item{}?", p.op.verb(), p.pairs.len(), if p.pairs.len() == 1 { "" } else { "s" });
            match widgets::confirm_modal(ctx, "folder-confirm", &title, &body, "OK") {
                Some(true) => {
                    if let Some(p) = self.confirm.take() {
                        self.run_op(p, cx.settings.delete_to_trash, ctx);
                    }
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        if let Some(d) = &mut self.name_dialog {
            let mut done = None;
            egui::Modal::new(egui::Id::new("folder-name")).show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.heading(&d.title);
                let r = ui.text_edit_singleline(&mut d.name);
                if d.focus {
                    r.request_focus();
                    d.focus = false;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    let name = d.name.trim().to_owned();
                    let res = if d.rename {
                        fsutil::rename_entry(&d.path, &name).map(|_| ())
                    } else if name.is_empty() || name.contains(['/', '\\']) {
                        Err(diffcraft_core::Error::InvalidInput(format!("invalid folder name `{name}`")))
                    } else {
                        fsutil::create_dir(&d.path.join(&name))
                    };
                    match res {
                        Ok(()) => {
                            self.name_dialog = None;
                            self.compare(ctx);
                        }
                        Err(e) => cx.toasts.error(e.to_string()),
                    }
                }
                Some(false) => self.name_dialog = None,
                None => {}
            }
        }
    }
}

/// Row height of the folder trees and the sync plan.
pub(crate) const ROW_H: f32 = 19.0;
/// Width of the status column between the two trees.
const CENTER_W: f32 = 26.0;
/// Indentation per tree level.
const INDENT: f32 = 16.0;

/// Column geometry of the two side-by-side trees.
#[derive(Clone, Copy)]
struct Geom {
    half: f32,
    name_w: f32,
    size_w: f32,
    date_w: f32,
}

impl Geom {
    fn new(width: f32, bar_w: f32) -> Self {
        let half = ((width - CENTER_W - bar_w) / 2.0).floor().max(120.0);
        let size_w = 78.0;
        let date_w = if half > 400.0 { 124.0 } else { 0.0 };
        Geom { half, name_w: (half - size_w - date_w).max(60.0), size_w, date_w }
    }

    /// Left edge of a side's pane.
    fn x0(&self, left: f32, side: usize) -> f32 {
        left + side as f32 * (self.half + CENTER_W)
    }
}

/// An expand/collapse triangle centred on `c`.
fn paint_triangle(p: &Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 2.5)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(2.5, 0.0), c + vec2(-2.0, 4.0)]
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// The status marker in the centre column.
fn paint_marker(p: &Painter, r: Rect, status: Status, pal: &Palette) {
    let c = r.center();
    match status {
        Status::Same => {
            let s = Stroke::new(1.4, pal.dim_text.gamma_multiply(0.8));
            p.hline(c.x - 4.0..=c.x + 4.0, c.y - 2.0, s);
            p.hline(c.x - 4.0..=c.x + 4.0, c.y + 2.0, s);
        }
        Status::Different => icons::paint(p, Rect::from_center_size(c, vec2(13.0, 13.0)), Icon::ShowDiffs, pal, true),
        Status::LeftNewer | Status::RightNewer => {
            // An arrow pointing from the newer side to the older one.
            let d = if status == Status::LeftNewer { 1.0 } else { -1.0 };
            let s = Stroke::new(1.6, pal.newer_text);
            p.line_segment([c + vec2(-4.5 * d, 0.0), c + vec2(2.0 * d, 0.0)], s);
            p.add(Shape::convex_polygon(vec![c + vec2(4.5 * d, 0.0), c + vec2(0.5 * d, -3.5), c + vec2(0.5 * d, 3.5)], pal.newer_text, Stroke::NONE));
        }
        Status::LeftOnly | Status::RightOnly => {
            let d = if status == Status::LeftOnly { -1.0 } else { 1.0 };
            p.circle_filled(c + vec2(3.0 * d, 0.0), 3.2, pal.orphan_text);
        }
        Status::Error => {
            p.circle_filled(c, 6.0, pal.warning);
            p.line_segment([c + vec2(0.0, -3.5), c + vec2(0.0, 1.0)], Stroke::new(1.6, pal.pane_bg));
            p.circle_filled(c + vec2(0.0, 3.2), 0.9, pal.pane_bg);
        }
    }
}

/// Item counts and total size of one side, for the info strips.
pub(crate) fn side_info(c: &FolderComparison) -> [String; 2] {
    let mut files = [0u64; 2];
    let mut dirs = [0u64; 2];
    let mut bytes = [0u64; 2];
    for n in c.nodes.iter().skip(1) {
        for (side, meta) in [n.left.as_ref(), n.right.as_ref()].into_iter().enumerate() {
            let Some(m) = meta else { continue };
            let (Some(f), Some(d), Some(b)) = (files.get_mut(side), dirs.get_mut(side), bytes.get_mut(side)) else { continue };
            if m.is_dir {
                *d += 1;
            } else {
                *f += 1;
                *b = b.saturating_add(m.size);
            }
        }
    }
    let one = |side: usize| {
        let (f, d, b) = (files.get(side).copied().unwrap_or(0), dirs.get(side).copied().unwrap_or(0), bytes.get(side).copied().unwrap_or(0));
        format!(
            "{} file{}    {} folder{}    {}",
            widgets::thousands(f),
            if f == 1 { "" } else { "s" },
            widgets::thousands(d),
            if d == 1 { "" } else { "s" },
            widgets::format_size(b)
        )
    };
    [one(0), one(1)]
}

/// The two folder path rows (folder icon, path, browse) with a slim info strip under each.
/// The left side is `left_w` wide, then a `gap`, then the right side. Returns true when a path
/// was submitted.
pub(crate) fn path_rows(ui: &mut Ui, id: &str, roots: &mut [String; 2], info: &[String; 2], left_w: f32, gap: f32) -> bool {
    let pal = Palette::get(ui.ctx());
    let mut go = false;
    let full = ui.available_rect_before_wrap();
    let bg = ui.painter().add(Shape::Noop);
    let mut bottom = full.top();
    for side in 0..2 {
        let (x0, x1) = if side == 0 { (full.left(), full.left() + left_w) } else { (full.left() + left_w + gap, full.right()) };
        let r = Rect::from_min_max(pos2(x0, full.top()), pos2(x1.max(x0 + 40.0), full.bottom()));
        let mut child = ui.new_child(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Min)));
        child.spacing_mut().item_spacing.y = 0.0;
        toolbar::header_strip(&mut child, 26.0, |ui| {
            let (ir, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            icons::paint(ui.painter(), ir, Icon::Folder, &pal, true);
            if let Some(p) = roots.get_mut(side) {
                go |= widgets::path_bar(ui, &format!("{id}-{side}"), p, true, if side == 0 { "Left folder" } else { "Right folder" }).submitted;
            }
        });
        let text = info.get(side).map(String::as_str).unwrap_or("");
        toolbar::header_strip(&mut child, 16.0, |ui| {
            ui.add_space(22.0);
            ui.label(RichText::new(text).size(11.5).color(pal.dim_text));
        });
        bottom = bottom.max(child.min_rect().bottom());
    }
    let rect = Rect::from_min_max(full.min, pos2(full.right(), bottom + 1.0));
    ui.painter().set(bg, Shape::rect_filled(rect, 0.0, pal.header_bg));
    let stroke = Stroke::new(1.0, pal.border);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, stroke);
    if gap > 0.0 {
        ui.painter().vline(rect.left() + left_w + 0.5, rect.y_range(), stroke);
        ui.painter().vline(rect.left() + left_w + gap - 0.5, rect.y_range(), stroke);
    }
    ui.advance_cursor_after_rect(rect);
    go
}
/// Open the system file manager at `path`.
fn reveal_in_file_manager(path: &Path, toasts: &mut Toasts) {
    let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    let res = if cfg!(windows) {
        std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&dir).spawn()
    };
    if let Err(e) = res {
        toasts.error(format!("Could not open the file manager: {e}"));
    }
}

impl View for FolderView {
    fn kind(&self) -> SessionKind {
        SessionKind::FolderCompare
    }

    fn title(&self) -> String {
        let name = |p: &str| {
            let p = widgets::clean_path(p);
            p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
        };
        if self.roots.iter().all(|r| r.is_empty()) {
            return "Folder Compare".into();
        }
        format!("{} ↔ {}", name(&self.roots[0]), name(&self.roots[1]))
    }

    fn tooltip(&self) -> String {
        format!("Left: {}\nRight: {}", self.roots[0], self.roots[1])
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        self.poll(cx.toasts, &ctx);
        self.ensure_visible();
        self.toolbar(ui, cx);
        self.status_bar(ui);
        let g = Geom::new(ui.available_width(), ui.spacing().scroll.bar_width);
        if path_rows(ui, "folder-root", &mut self.roots, &self.info, g.half, CENTER_W) {
            self.compare(&ctx);
        }
        let busy = self.op_job.is_some() || self.job.is_some();
        if busy || self.error.is_some() {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                if let Some(job) = &self.op_job {
                    jobs::progress_ui(ui, job);
                }
                if let Some(job) = &self.job
                    && jobs::progress_ui(ui, job)
                {
                    self.job = None;
                }
                if let Some(e) = &self.error {
                    ui.colored_label(pal.error, e);
                }
            });
        }
        if self.cmp.is_none() {
            if self.error.is_none() && self.job.is_none() {
                widgets::empty_message(ui, "Choose two folders to compare.");
            }
            return;
        }
        self.keyboard(ui);
        self.body(ui, cx);
        self.dialogs(&ctx, cx);
    }

    fn supports(&self, cmd: Cmd) -> bool {
        let has = self.cmp.is_some();
        let sel = !self.sel.is_empty() && self.op_job.is_none();
        match cmd {
            Cmd::Refresh | Cmd::SwapSides => true,
            Cmd::ExpandAll | Cmd::CollapseAll | Cmd::ExportHtmlReport | Cmd::ExportCsv | Cmd::Sync | Cmd::SelectAll | Cmd::NewFolder => has,
            Cmd::NextDiff | Cmd::PrevDiff | Cmd::FirstDiff | Cmd::LastDiff => has,
            Cmd::CopyToRight | Cmd::CopyToLeft | Cmd::Delete | Cmd::DeleteLeft | Cmd::DeleteRight | Cmd::OpenSelected | Cmd::Copy => sel,
            Cmd::Rename => self.sel.len() == 1,
            _ => false,
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, cx: &mut Cx<'_>) {
        self.ensure_visible();
        match cmd {
            Cmd::Refresh => self.compare(ctx),
            Cmd::ExpandAll => self.set_all_expanded(true),
            Cmd::CollapseAll => self.set_all_expanded(false),
            Cmd::CopyToRight => self.prepare_op(FileOp::Copy { to_right: true }, cx.settings, ctx),
            Cmd::CopyToLeft => self.prepare_op(FileOp::Copy { to_right: false }, cx.settings, ctx),
            Cmd::Delete => {
                let left = self.focus_side == 0;
                self.prepare_op(FileOp::Delete { left }, cx.settings, ctx);
            }
            Cmd::DeleteLeft => self.prepare_op(FileOp::Delete { left: true }, cx.settings, ctx),
            Cmd::DeleteRight => self.prepare_op(FileOp::Delete { left: false }, cx.settings, ctx),
            Cmd::OpenSelected => {
                let single = if self.sel.len() == 1 { self.sel.iter().next().copied() } else { None };
                let single_dir = single.filter(|&i| self.cmp.as_ref().and_then(|c| c.nodes.get(i)).is_some_and(Node::is_dir));
                match single_dir {
                    Some(i) => self.toggle(i),
                    None => self.open_selected(cx, None),
                }
            }
            Cmd::SelectAll => {
                self.sel = self.visible.iter().copied().collect();
            }
            Cmd::Copy => {
                if let Some(c) = &self.cmp {
                    let text: Vec<String> = self
                        .sel
                        .iter()
                        .filter_map(|&i| if self.focus_side == 0 { c.left_path(i) } else { c.right_path(i) })
                        .map(|p| p.display().to_string())
                        .collect();
                    ctx.copy_text(text.join("\n"));
                }
            }
            Cmd::Rename => {
                let Some(&i) = self.sel.iter().next() else { return };
                let Some(c) = &self.cmp else { return };
                let path = if self.focus_side == 0 { c.left_path(i).or_else(|| c.right_path(i)) } else { c.right_path(i).or_else(|| c.left_path(i)) };
                if let Some(path) = path {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.name_dialog = Some(NameDialog { title: format!("Rename {}", path.display()), name, path, rename: true, focus: true });
                }
            }
            Cmd::NewFolder => {
                let Some(c) = &self.cmp else { return };
                let side_right = self.focus_side == 1;
                let parent = self
                    .sel
                    .iter()
                    .next()
                    .and_then(|&i| {
                        let n = c.nodes.get(i)?;
                        let p = if side_right { c.right_path(i) } else { c.left_path(i) }?;
                        Some(if n.is_dir() { p } else { p.parent()?.to_path_buf() })
                    })
                    .unwrap_or_else(|| if side_right { c.right_root.clone() } else { c.left_root.clone() });
                self.name_dialog = Some(NameDialog {
                    title: format!("New folder in {}", parent.display()),
                    name: "New Folder".into(),
                    path: parent,
                    rename: false,
                    focus: true,
                });
            }
            Cmd::SwapSides => {
                self.roots.swap(0, 1);
                self.compare(ctx);
            }
            Cmd::NextDiff | Cmd::PrevDiff | Cmd::FirstDiff | Cmd::LastDiff => {
                let Some(c) = &self.cmp else { return };
                let diffs: Vec<usize> = self
                    .visible
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| c.nodes.get(**i).is_some_and(|n| !n.is_dir() && n.status != Status::Same))
                    .map(|(vi, _)| vi)
                    .collect();
                let cur = self.anchor;
                let target = match cmd {
                    Cmd::FirstDiff => diffs.first().copied(),
                    Cmd::LastDiff => diffs.last().copied(),
                    Cmd::NextDiff => diffs.iter().copied().find(|&d| cur.is_none_or(|c| d > c)),
                    _ => diffs.iter().rev().copied().find(|&d| cur.is_none_or(|c| d < c)),
                };
                if let Some(vi) = target {
                    self.select_row(vi, false, false);
                    self.scroll_to = Some(vi);
                }
            }
            Cmd::Sync => {
                let mut s = Session::new(SessionKind::FolderSync, self.roots[0].clone(), self.roots[1].clone());
                s.folder = self.opts.clone();
                cx.open.push(OpenRequest::Session(Box::new(s)));
            }
            Cmd::ExportHtmlReport | Cmd::ExportCsv => {
                let Some(c) = &self.cmp else { return };
                let html = cmd == Cmd::ExportHtmlReport;
                let name = if html { "folder-report.html" } else { "folder-report.csv" };
                let filter: (&str, &[&str]) = if html { ("HTML", &["html", "htm"]) } else { ("CSV", &["csv"]) };
                if let Some(path) = widgets::save_file(None, name, Some(filter)) {
                    let only_diff = self.show != ShowFilter::All;
                    let content = if html {
                        let opts = diffcraft_report::FolderReportOptions {
                            title: format!("Folder Compare: {}", self.title()),
                            only_differences: only_diff,
                            include_sizes: true,
                            include_times: true,
                        };
                        diffcraft_report::folder_html_report(c, &opts)
                    } else {
                        diffcraft_report::folder_csv_report(c, only_diff)
                    };
                    match fsutil::write_atomic(&path, content.as_bytes()) {
                        Ok(()) => cx.toasts.success(format!("Exported {}", path.display())),
                        Err(e) => cx.toasts.error(format!("Export failed: {e}")),
                    }
                }
            }
            _ => {}
        }
    }

    fn session(&self) -> Option<Session> {
        let mut s = Session::new(SessionKind::FolderCompare, self.roots[0].clone(), self.roots[1].clone());
        s.folder = self.opts.clone();
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::folder::{CompareMode, Progress, compare_folders};

    fn setup() -> (tempfile::TempDir, FolderComparison) {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        for (p, c) in [("L/a.txt", "1"), ("L/sub/x.txt", "x"), ("L/sub/y.txt", "y"), ("R/a.txt", "2!"), ("R/sub/x.txt", "x"), ("R/only.txt", "o")] {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        let c = compare_folders(&l, &r, &FolderCompareOptions { mode: CompareMode::Binary, ..Default::default() }, &Progress::default()).unwrap();
        (d, c)
    }

    fn names(c: &FolderComparison, rows: &[usize]) -> Vec<String> {
        rows.iter().map(|&i| c.nodes[i].rel.to_string_lossy().replace('\\', "/")).collect()
    }

    #[test]
    fn visibility_follows_expansion_and_filter() {
        let (_d, c) = setup();
        let mut exp = HashSet::new();
        assert_eq!(names(&c, &visible_rows(&c, &exp, ShowFilter::All)), vec!["sub", "a.txt", "only.txt"]);
        exp.insert(PathBuf::from("sub"));
        assert_eq!(names(&c, &visible_rows(&c, &exp, ShowFilter::All)), vec!["sub", "sub/x.txt", "sub/y.txt", "a.txt", "only.txt"]);
        assert_eq!(names(&c, &visible_rows(&c, &exp, ShowFilter::Differences)), vec!["sub", "sub/y.txt", "a.txt", "only.txt"]);
        assert_eq!(names(&c, &visible_rows(&c, &exp, ShowFilter::Same)), vec!["sub", "sub/x.txt"]);
        assert_eq!(names(&c, &visible_rows(&c, &exp, ShowFilter::Orphans)), vec!["sub", "sub/y.txt", "only.txt"]);
    }
}
