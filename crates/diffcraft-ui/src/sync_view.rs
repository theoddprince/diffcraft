//! Folder Sync: compare two folders, preview the copy/delete plan for the chosen mode,
//! untick anything you want to skip, then run it with progress.

use std::sync::atomic::Ordering;

use diffcraft_core::folder::sync::{self, SyncAction, SyncFailure, SyncMode, SyncOp};
use diffcraft_core::folder::{self, FolderCompareOptions, FolderComparison};
use diffcraft_session::{Session, SessionKind, Settings};
use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::folder_view::ROW_H;
use crate::icons::{self, Icon};
use crate::jobs::{self, Job};
use crate::theme::Palette;
use crate::toolbar;
use crate::view::{Cmd, Cx, OpenRequest, View};
use crate::widgets;

/// Totals of a plan (only included operations).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanTotals {
    pub copies: usize,
    pub deletes: usize,
    pub bytes: u64,
}

/// Gap between the two path rows.
const GAP: f32 = 8.0;

pub fn totals(ops: &[SyncOp], include: &[bool]) -> PlanTotals {
    let mut t = PlanTotals::default();
    for (op, inc) in ops.iter().zip(include) {
        if !inc {
            continue;
        }
        match op.action {
            SyncAction::Copy { .. } => {
                t.copies += 1;
                t.bytes = t.bytes.saturating_add(op.bytes);
            }
            SyncAction::Delete { .. } => t.deletes += 1,
        }
    }
    t
}

pub struct SyncView {
    roots: [String; 2],
    opts: FolderCompareOptions,
    filter_text: String,
    mode: SyncMode,
    cmp_job: Option<Job<FolderComparison>>,
    cmp: Option<FolderComparison>,
    error: Option<String>,
    ops: Vec<SyncOp>,
    include: Vec<bool>,
    run_job: Option<Job<Vec<SyncFailure>>>,
    /// The ops of the last run and their failures.
    last_run: Option<(Vec<SyncOp>, Vec<SyncFailure>)>,
    confirm: bool,
    /// Item counts per side for the info strips.
    info: [String; 2],
}

impl SyncView {
    pub fn from_session(s: &Session, _settings: &Settings, ctx: &egui::Context) -> Self {
        let mut v = SyncView {
            roots: [s.left.clone(), s.right.clone()],
            filter_text: s.folder.filter.to_spec(),
            opts: s.folder.clone(),
            mode: s.sync_mode,
            cmp_job: None,
            cmp: None,
            error: None,
            ops: Vec::new(),
            include: Vec::new(),
            run_job: None,
            last_run: None,
            confirm: false,
            info: Default::default(),
        };
        if !s.left.trim().is_empty() && !s.right.trim().is_empty() {
            v.compare(ctx);
        }
        v
    }

    fn compare(&mut self, ctx: &egui::Context) {
        self.error = None;
        if self.roots.iter().any(|r| r.trim().is_empty()) {
            self.error = Some("Choose a folder for each side.".into());
            return;
        }
        let (l, r) = (widgets::clean_path(&self.roots[0]), widgets::clean_path(&self.roots[1]));
        if let Some(j) = &self.cmp_job {
            j.cancel();
        }
        let opts = self.opts.clone();
        self.cmp_job = Some(Job::spawn("Comparing folders", ctx, move |p| folder::compare_folders(&l, &r, &opts, p).map_err(|e| e.to_string())));
    }

    fn replan(&mut self) {
        self.ops = self.cmp.as_ref().map(|c| sync::plan(c, self.mode)).unwrap_or_default();
        self.include = vec![true; self.ops.len()];
    }

    fn poll(&mut self, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if let Some(j) = &mut self.cmp_job
            && let Some(res) = j.poll()
        {
            self.cmp_job = None;
            match res {
                Ok(c) => {
                    self.info = crate::folder_view::side_info(&c);
                    self.cmp = Some(c);
                    self.replan();
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(j) = &mut self.run_job
            && let Some(res) = j.poll()
        {
            self.run_job = None;
            let ran: Vec<SyncOp> = self.ops.iter().zip(&self.include).filter(|(_, i)| **i).map(|(o, _)| o.clone()).collect();
            match res {
                Ok(failures) => {
                    if failures.is_empty() {
                        cx.toasts.success(format!("Synchronized: {} operation{}", ran.len(), if ran.len() == 1 { "" } else { "s" }));
                    } else {
                        cx.toasts.error(format!("{} operation(s) failed", failures.len()));
                    }
                    self.last_run = Some((ran, failures));
                }
                Err(e) => cx.toasts.error(e),
            }
            self.compare(ctx);
        }
    }

    fn start_run(&mut self, ctx: &egui::Context, to_trash: bool) {
        let ops: Vec<SyncOp> = self.ops.iter().zip(&self.include).filter(|(_, i)| **i).map(|(o, _)| o.clone()).collect();
        if ops.is_empty() {
            return;
        }
        self.run_job = Some(Job::spawn("Synchronizing", ctx, move |p| {
            p.files_to_compare.store(ops.len() as u64, Ordering::Relaxed);
            Ok(sync::execute(&ops, to_trash, p))
        }));
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, busy: bool, can_run: bool) {
        let ctx = ui.ctx().clone();
        let mut mode = self.mode;
        let mut rules_changed = false;
        let mut reload = false;
        let mut swap = false;
        let mut open_compare = false;
        let mut sync = false;
        let has = self.cmp.is_some();
        let rules_tip = format!("Comparison rules and filters (now comparing by {})", self.opts.mode.label().to_lowercase());
        let mode_tip = format!("Synchronization mode: {}\n{}", mode.label(), mode.description());
        let (opts, filter_text) = (&mut self.opts, &mut self.filter_text);
        toolbar::bar(ui, "sync-toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            toolbar::menu(ui, mode_icon(mode), mode_short(mode), &mode_tip, !busy, |ui| {
                ui.set_min_width(260.0);
                for m in SyncMode::ALL {
                    if ui.selectable_label(mode == m, m.label()).on_hover_text(m.description()).clicked() {
                        mode = m;
                        ui.close();
                    }
                }
            });
            toolbar::menu(ui, Icon::Rules, "Rules", &rules_tip, !busy, |ui| {
                ui.set_min_width(340.0);
                rules_changed |= crate::rules_ui::folder_rules(ui, opts, filter_text, "sync-rules");
            });
            toolbar::separator(ui);
            reload = toolbar::button(ui, Icon::Reload, "Compare", "Compare the folders again (F5)", !busy);
            swap = toolbar::button(ui, Icon::Swap, "Swap", "Swap the left and right folders", !busy);
            open_compare = toolbar::button(ui, Icon::Folder, "Folders", "Open these folders in Folder Compare", has);
            toolbar::separator(ui);
            sync = toolbar::toggle(ui, Icon::Sync, "Synchronize", "Run the included operations", can_run, can_run);
        });
        if mode != self.mode {
            self.mode = mode;
            self.replan();
        }
        if rules_changed || reload {
            self.compare(&ctx);
        }
        if swap {
            self.roots.swap(0, 1);
            self.compare(&ctx);
        }
        if open_compare {
            let mut s = Session::new(SessionKind::FolderCompare, self.roots[0].clone(), self.roots[1].clone());
            s.folder = self.opts.clone();
            cx.open.push(OpenRequest::Session(Box::new(s)));
        }
        if sync {
            self.confirm = true;
        }
    }

    fn status_bar(&self, ui: &mut Ui, t: &PlanTotals) {
        let pal = Palette::get(ui.ctx());
        toolbar::status_bar(ui, "sync-status", |ui| {
            if self.run_job.is_some() {
                toolbar::status_icon(ui, Icon::Sync);
                ui.label("Synchronizing…");
                return;
            }
            let Some(c) = &self.cmp else {
                ui.label(RichText::new(if self.cmp_job.is_some() { "Comparing…" } else { "Ready" }).color(pal.dim_text));
                return;
            };
            let n = self.ops.len();
            if n == 0 {
                toolbar::status_icon(ui, Icon::Check);
                ui.colored_label(pal.success, "In sync");
            } else {
                toolbar::status_icon(ui, Icon::Sync);
                ui.label(format!("{} operation{} planned", widgets::thousands(n as u64), if n == 1 { "" } else { "s" }));
                toolbar::status_sep(ui);
                let inc = self.include.iter().filter(|i| **i).count();
                ui.label(format!("{} included", widgets::thousands(inc as u64)));
                ui.label(RichText::new(format!("{} cop{}", t.copies, if t.copies == 1 { "y" } else { "ies" })).color(pal.text));
                ui.colored_label(
                    if t.deletes > 0 { pal.error } else { pal.dim_text },
                    format!("{} deletion{}", t.deletes, if t.deletes == 1 { "" } else { "s" }),
                );
                ui.label(RichText::new(widgets::format_size(t.bytes)).color(pal.dim_text));
            }
            toolbar::status_sep(ui);
            ui.label(self.mode.label());
            toolbar::status_sep(ui);
            ui.label(format!("Compared by {}", c.options.mode.label().to_lowercase()));
        });
    }

    fn plan_table(&mut self, ui: &mut Ui, pal: &Palette) {
        let full = ui.available_rect_before_wrap();
        let bar_w = ui.spacing().scroll.bar_width;
        let (cb_w, act_w, size_w) = (28.0, 130.0, 90.0);
        let stroke = Stroke::new(1.0, pal.border);
        let hf = FontId::proportional(12.0);
        let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 20.0), Sense::hover());
        let size_x = (hdr.right() - bar_w - size_w).max(hdr.left() + cb_w + act_w + 60.0);
        {
            let p = ui.painter();
            p.rect_filled(hdr, 0.0, pal.header_bg);
            let cy = hdr.center().y;
            p.text(pos2(hdr.left() + cb_w + 6.0, cy), Align2::LEFT_CENTER, "Action", hf.clone(), pal.dim_text);
            p.vline(hdr.left() + cb_w + act_w, hdr.y_range().shrink(3.0), stroke);
            p.text(pos2(hdr.left() + cb_w + act_w + 8.0, cy), Align2::LEFT_CENTER, "Relative path", hf.clone(), pal.dim_text);
            p.vline(size_x, hdr.y_range().shrink(3.0), stroke);
            p.text(pos2(size_x + size_w - 8.0, cy), Align2::RIGHT_CENTER, "Size", hf, pal.dim_text);
            p.hline(hdr.x_range(), hdr.bottom() - 0.5, stroke);
        }
        let grid = Rect::from_min_max(pos2(full.left(), hdr.bottom()), full.max);
        ui.painter().rect_filled(grid, 0.0, pal.pane_bg);
        if self.ops.is_empty() {
            let msg = if self.cmp.is_none() && self.cmp_job.is_none() { "Choose two folders to synchronize." } else { "" };
            ui.painter().text(grid.center(), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.dim_text);
            return;
        }
        let font = FontId::proportional(13.0);
        let mut child = ui.new_child(UiBuilder::new().max_rect(grid));
        child.spacing_mut().item_spacing.y = 0.0;
        let (ops, include) = (&self.ops, &mut self.include);
        egui::ScrollArea::vertical().id_salt("sync-plan").auto_shrink(false).show_rows(&mut child, ROW_H, ops.len(), |ui, range| {
            for k in range {
                let (Some(op), Some(inc)) = (ops.get(k), include.get_mut(k)) else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let painter = ui.painter_at(rect);
                if k % 2 == 1 {
                    painter.rect_filled(rect, 0.0, pal.gutter_bg);
                }
                if resp.hovered() {
                    painter.rect_filled(rect, 0.0, pal.hover_bg.gamma_multiply(0.6));
                }
                let cy = rect.center().y;
                let delete = matches!(op.action, SyncAction::Delete { .. });
                let on = *inc;
                let fade = |c: Color32| if on { c } else { c.gamma_multiply(0.55) };
                let icon = match (delete, op.to_right) {
                    (true, _) => Icon::Delete,
                    (false, true) => Icon::CopyRight,
                    (false, false) => Icon::CopyLeft,
                };
                let ir = Rect::from_min_size(pos2(rect.left() + cb_w + 4.0, cy - 7.0), vec2(14.0, 14.0));
                icons::paint(&painter, ir, icon, pal, on);
                let action_color = fade(if delete { pal.error } else { pal.text });
                painter.text(pos2(ir.right() + 6.0, cy), Align2::LEFT_CENTER, op.verb(), font.clone(), action_color);
                let path_rect = Rect::from_min_max(pos2(rect.left() + cb_w + act_w + 8.0, rect.top()), pos2(size_x - 6.0, rect.bottom()));
                if path_rect.width() > 4.0 {
                    let path_color = fade(if delete { pal.error } else { pal.text });
                    painter.with_clip_rect(path_rect).text(
                        pos2(path_rect.left(), cy),
                        Align2::LEFT_CENTER,
                        op.rel.display().to_string(),
                        font.clone(),
                        path_color,
                    );
                }
                if op.bytes > 0 {
                    painter.text(pos2(size_x + size_w - 8.0, cy), Align2::RIGHT_CENTER, widgets::format_size(op.bytes), font.clone(), fade(pal.dim_text));
                }
                let cb = Rect::from_min_size(pos2(rect.left() + 6.0, rect.top()), vec2(cb_w - 6.0, ROW_H));
                let changed = ui.put(cb, egui::Checkbox::without_text(inc)).changed();
                if resp.clicked() && !changed {
                    *inc = !*inc;
                }
            }
        });
    }
}

/// Short toolbar label for a sync mode.
fn mode_short(m: SyncMode) -> &'static str {
    match m {
        SyncMode::UpdateRight => "Update →",
        SyncMode::UpdateLeft => "← Update",
        SyncMode::UpdateBoth => "Update Both",
        SyncMode::MirrorRight => "Mirror →",
        SyncMode::MirrorLeft => "← Mirror",
    }
}

fn mode_icon(m: SyncMode) -> Icon {
    match m {
        SyncMode::UpdateRight | SyncMode::MirrorRight => Icon::CopyRight,
        SyncMode::UpdateLeft | SyncMode::MirrorLeft => Icon::CopyLeft,
        SyncMode::UpdateBoth => Icon::Swap,
    }
}

/// The result of the last run, docked above the status bar.
fn last_run_panel(ui: &mut Ui, ran: &[SyncOp], failures: &[SyncFailure], pal: &Palette) {
    egui::Panel::bottom("sync-last-run").frame(egui::Frame::NONE.fill(pal.panel_bg).inner_margin(egui::Margin::symmetric(8, 4))).show(ui, |ui| {
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.top() - 4.0, Stroke::new(1.0, pal.border));
        ui.horizontal(|ui| {
            if failures.is_empty() {
                toolbar::status_icon(ui, Icon::Check);
                ui.colored_label(pal.success, format!("Last run: {} operations completed.", ran.len()));
            } else {
                toolbar::status_icon(ui, Icon::Conflict);
                ui.colored_label(pal.error, format!("Last run: {} of {} operations failed:", failures.len(), ran.len()));
            }
        });
        if !failures.is_empty() {
            egui::ScrollArea::vertical().id_salt("sync-fail").max_height(90.0).show(ui, |ui| {
                for f in failures {
                    let what = ran.get(f.op).map(|o| o.rel.display().to_string()).unwrap_or_default();
                    ui.label(RichText::new(format!("{what}: {}", f.message)).size(12.0));
                }
            });
        }
    });
}

impl View for SyncView {
    fn kind(&self) -> SessionKind {
        SessionKind::FolderSync
    }

    fn title(&self) -> String {
        let name = |p: &str| widgets::clean_path(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_owned());
        if self.roots.iter().all(|r| r.is_empty()) {
            return "Folder Sync".into();
        }
        format!("Sync {} → {}", name(&self.roots[0]), name(&self.roots[1]))
    }

    fn tooltip(&self) -> String {
        format!("{}\nLeft: {}\nRight: {}", self.mode.label(), self.roots[0], self.roots[1])
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        self.poll(&ctx, cx);
        let busy = self.run_job.is_some() || self.cmp_job.is_some();
        let t = totals(&self.ops, &self.include);
        let can_run = !busy && t.copies + t.deletes > 0;
        self.toolbar(ui, cx, busy, can_run);
        self.status_bar(ui, &t);
        if let Some((ran, failures)) = &self.last_run {
            last_run_panel(ui, ran, failures, &pal);
        }
        let w = ui.available_width();
        let left_w = ((w - GAP) / 2.0).floor();
        if crate::folder_view::path_rows(ui, "sync-root", &mut self.roots, &self.info, left_w, GAP) {
            self.compare(&ctx);
        }
        // Summary strip: what the plan does, select all/none, and the mode's description.
        toolbar::header_strip(ui, 24.0, |ui| {
            if self.cmp.is_some() && self.ops.is_empty() {
                toolbar::status_icon(ui, Icon::Check);
                ui.colored_label(pal.success, "Nothing to do: the folders are already in sync for this mode.");
            } else if !self.ops.is_empty() {
                toolbar::status_icon(ui, Icon::CopyRight);
                ui.label(format!("{} cop{} ({})", t.copies, if t.copies == 1 { "y" } else { "ies" }, widgets::format_size(t.bytes)));
                toolbar::status_icon(ui, Icon::Delete);
                ui.colored_label(
                    if t.deletes > 0 { pal.error } else { pal.dim_text },
                    format!("{} deletion{}", t.deletes, if t.deletes == 1 { "" } else { "s" }),
                );
                ui.add_space(8.0);
                if ui.small_button("Include all").clicked() {
                    self.include.iter_mut().for_each(|i| *i = true);
                }
                if ui.small_button("Include none").clicked() {
                    self.include.iter_mut().for_each(|i| *i = false);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(self.mode.description()).size(11.5).color(pal.dim_text)).truncate());
            });
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 0.5, Stroke::new(1.0, pal.border));
        if busy || self.error.is_some() {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(pal.error, e);
                }
                if let Some(j) = &self.cmp_job
                    && jobs::progress_ui(ui, j)
                {
                    self.cmp_job = None;
                }
                if let Some(j) = &self.run_job {
                    jobs::progress_ui(ui, j);
                }
            });
        }
        self.plan_table(ui, &pal);

        if self.confirm {
            let body = format!(
                "{}\n\n{} copies ({}) and {} deletions.{}",
                self.mode.description(),
                t.copies,
                widgets::format_size(t.bytes),
                t.deletes,
                if t.deletes > 0 {
                    if cx.settings.delete_to_trash { "\nDeleted items go to the Recycle Bin / Trash." } else { "\nDeleted items are removed permanently." }
                } else {
                    ""
                }
            );
            match widgets::confirm_modal(&ctx, "sync-confirm", "Synchronize folders?", &body, "Synchronize") {
                Some(true) => {
                    self.confirm = false;
                    self.start_run(&ctx, cx.settings.delete_to_trash);
                }
                Some(false) => self.confirm = false,
                None => {}
            }
        }
    }
    fn supports(&self, cmd: Cmd) -> bool {
        let idle = self.run_job.is_none();
        match cmd {
            Cmd::Refresh | Cmd::SwapSides => idle,
            Cmd::Sync => idle && self.include.iter().any(|i| *i),
            _ => false,
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, _cx: &mut Cx<'_>) {
        match cmd {
            Cmd::Refresh => self.compare(ctx),
            Cmd::SwapSides => {
                self.roots.swap(0, 1);
                self.compare(ctx);
            }
            Cmd::Sync => self.confirm = true,
            _ => {}
        }
    }

    fn session(&self) -> Option<Session> {
        let mut s = Session::new(SessionKind::FolderSync, self.roots[0].clone(), self.roots[1].clone());
        s.folder = self.opts.clone();
        s.sync_mode = self.mode;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn totals_count_included_only() {
        let copy = SyncOp { action: SyncAction::Copy { from: PathBuf::from("a"), to: PathBuf::from("b") }, rel: "a".into(), to_right: true, bytes: 10 };
        let del = SyncOp { action: SyncAction::Delete { path: PathBuf::from("c") }, rel: "c".into(), to_right: true, bytes: 0 };
        let ops = vec![copy.clone(), del, copy];
        assert_eq!(totals(&ops, &[true, true, false]), PlanTotals { copies: 1, deletes: 1, bytes: 10 });
        assert_eq!(totals(&ops, &[]), PlanTotals::default());
    }
}
