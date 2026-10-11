//! Hex Compare: two binary files side by side, byte differences highlighted.
//!
//! Rows are painted directly (no widget per byte) and only for the visible range. Scrolling is
//! row-based (not pixel-based) so files with tens of millions of rows stay precise.
//!
//! In "Diffs" mode only rows with differences (plus a little context) are shown; the hidden runs
//! of identical rows collapse into one separator row each. `top` and the overview marks are in
//! display rows; `selected` and the difference spans are in layout rows.

use std::sync::Arc;
use std::time::SystemTime;

use diffcraft_core::binary::{self, BinaryDiff, HexLayout};
use diffcraft_core::text::{Hunk, HunkKind, RowKind};
use diffcraft_session::{Session, SessionKind, Settings};
use egui::{Align, Align2, FontId, Key, Layout, Margin, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

use crate::icons::Icon;
use crate::jobs::{self, Job};
use crate::theme::{self, Palette};
use crate::toolbar;
use crate::view::{Cmd, Cx, View};
use crate::widgets::{self, OverviewMark};

const BYTES_PER_ROW_CHOICES: [usize; 3] = [8, 16, 32];
const STRIP_W: f32 = 14.0;
const DIVIDER_W: f32 = 8.0;
const HBAR_H: f32 = 10.0;
/// Left padding inside each side, in pixels.
const PAD: f32 = 6.0;
/// Rows of identical context kept around each difference in "Diffs" mode.
const CONTEXT: usize = 2;

/// One side's bytes, or why there are none.
#[derive(Clone, Default)]
struct Side {
    bytes: Arc<Vec<u8>>,
    /// A file was loaded (an empty path or a load error leaves this false).
    present: bool,
    error: Option<String>,
    modified: Option<SystemTime>,
}

/// Result of a background load/compare.
struct Loaded {
    left: Side,
    right: Side,
    diff: BinaryDiff,
}

/// A changed segment as rows of the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    rows: usize,
    kind: HunkKind,
}

/// What a display row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispRow {
    /// A layout row.
    Row(usize),
    /// A separator standing for `count` hidden identical rows starting at layout row `first`.
    Hidden { first: usize, count: usize },
}

/// A run of shown layout rows, preceded by a separator when rows before it are hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    /// Display row of the block's first entry (its separator, if any).
    disp: usize,
    hidden_before: usize,
    first: usize,
    len: usize,
}

/// The display rows of "Diffs" mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Filter {
    blocks: Vec<Block>,
    total: usize,
}

impl Filter {
    /// Show the rows of `spans` plus `context` rows around each, out of `count` layout rows.
    fn new(spans: &[Span], count: usize, context: usize) -> Self {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for s in spans {
            let a = s.start.saturating_sub(context).min(count);
            let b = s.start.saturating_add(s.rows).saturating_add(context).min(count);
            if a >= b {
                continue;
            }
            match ranges.last_mut() {
                Some(r) if a <= r.1 => r.1 = r.1.max(b),
                _ => ranges.push((a, b)),
            }
        }
        let (mut blocks, mut disp, mut prev_end) = (Vec::with_capacity(ranges.len() + 1), 0usize, 0usize);
        for (a, b) in ranges {
            let hidden = a.saturating_sub(prev_end);
            blocks.push(Block { disp, hidden_before: hidden, first: a, len: b - a });
            disp = disp.saturating_add(usize::from(hidden > 0)).saturating_add(b - a);
            prev_end = b;
        }
        if count > prev_end {
            blocks.push(Block { disp, hidden_before: count - prev_end, first: count, len: 0 });
            disp = disp.saturating_add(1);
        }
        Filter { blocks, total: disp }
    }

    fn get(&self, d: usize) -> Option<DispRow> {
        if d >= self.total {
            return None;
        }
        let b = self.blocks.get(self.blocks.partition_point(|b| b.disp <= d).checked_sub(1)?)?;
        let mut k = d - b.disp;
        if b.hidden_before > 0 {
            if k == 0 {
                return Some(DispRow::Hidden { first: b.first.saturating_sub(b.hidden_before), count: b.hidden_before });
            }
            k -= 1;
        }
        (k < b.len).then_some(DispRow::Row(b.first + k))
    }

    /// Display row showing layout row `row`, or the separator hiding it. The flag is true when
    /// the row itself is shown.
    fn locate(&self, row: usize) -> (usize, bool) {
        let Some(b) = self.blocks.get(self.blocks.partition_point(|b| b.first.saturating_add(b.len) <= row)) else {
            return (self.total.saturating_sub(1), false);
        };
        if row >= b.first { (b.disp + usize::from(b.hidden_before > 0) + (row - b.first), true) } else { (b.disp, false) }
    }
}

pub struct HexView {
    session: Session,
    left_path: String,
    right_path: String,
    bytes_per_row: usize,
    align: bool,
    /// Show every row ("All") or only differences with context ("Diffs").
    show_all: bool,
    left: Side,
    right: Side,
    diff: Option<BinaryDiff>,
    layout: HexLayout,
    spans: Vec<Span>,
    /// `Some` in "Diffs" mode.
    filter: Option<Filter>,
    /// Overview marks, as fractions of the display rows.
    marks: Vec<(f32, f32, HunkKind)>,
    job: Option<Job<Loaded>>,
    /// First visible display row.
    top: usize,
    scroll_accum: f32,
    visible_rows: usize,
    hscroll: f32,
    /// Selected layout row.
    selected: Option<usize>,
    /// Index into `spans`.
    current: Option<usize>,
    offset_text: String,
    /// The "Go to offset" strip is shown.
    goto_open: bool,
    focus_offset: bool,
    offset_focused: bool,
}

impl HexView {
    pub fn from_session(s: &Session, settings: &Settings, ctx: &egui::Context) -> Self {
        let bpr = if BYTES_PER_ROW_CHOICES.contains(&settings.hex_bytes_per_row) { settings.hex_bytes_per_row } else { 16 };
        let mut v = HexView {
            session: s.clone(),
            left_path: s.left.clone(),
            right_path: s.right.clone(),
            bytes_per_row: bpr,
            align: settings.hex_align,
            show_all: true,
            left: Side::default(),
            right: Side::default(),
            diff: None,
            layout: HexLayout::default(),
            spans: Vec::new(),
            filter: None,
            marks: Vec::new(),
            job: None,
            top: 0,
            scroll_accum: 0.0,
            visible_rows: 1,
            hscroll: 0.0,
            selected: None,
            current: None,
            offset_text: String::new(),
            goto_open: false,
            focus_offset: false,
            offset_focused: false,
        };
        v.reload(ctx);
        v
    }

    /// Load both files from disk and compare them.
    fn reload(&mut self, ctx: &egui::Context) {
        let (l, r, align) = (self.left_path.clone(), self.right_path.clone(), self.align);
        self.cancel_job();
        self.job = Some(Job::spawn("Loading files", ctx, move |p| {
            p.set_current(l.trim());
            let left = load_side(&l);
            p.set_current(r.trim());
            let right = load_side(&r);
            if p.is_cancelled() {
                return Err("cancelled".into());
            }
            p.set_current("comparing");
            let diff = binary::compare(&left.bytes, &right.bytes, align);
            Ok(Loaded { left, right, diff })
        }));
    }

    /// Compare the already-loaded bytes again (alignment changed).
    fn recompare(&mut self, ctx: &egui::Context) {
        let (left, right, align) = (self.left.clone(), self.right.clone(), self.align);
        self.cancel_job();
        self.job = Some(Job::spawn("Comparing", ctx, move |_| {
            let diff = binary::compare(&left.bytes, &right.bytes, align);
            Ok(Loaded { left, right, diff })
        }));
    }

    /// Drop the running job, if any: its result is never applied (a newer job replaces it).
    fn cancel_job(&mut self) {
        if let Some(j) = self.job.take() {
            j.cancel();
        }
    }

    fn poll_job(&mut self, cx: &mut Cx<'_>) {
        let Some(job) = self.job.as_mut() else { return };
        let Some(result) = job.poll() else { return };
        self.job = None;
        match result {
            Ok(loaded) => {
                for (name, side) in [("left", &loaded.left), ("right", &loaded.right)] {
                    if let Some(e) = &side.error {
                        cx.toasts.error(format!("Could not load the {name} file: {e}"));
                    }
                }
                self.left = loaded.left;
                self.right = loaded.right;
                self.diff = Some(loaded.diff);
                self.selected = None;
                self.current = None;
                self.rebuild_layout();
            }
            Err(e) => cx.toasts.error(format!("Hex compare failed: {e}")),
        }
    }

    /// The layout row at the top of the view (the first row of a separator's hidden run).
    fn top_row(&self) -> Option<usize> {
        match self.disp_row(self.top)? {
            DispRow::Row(r) => Some(r),
            DispRow::Hidden { first, .. } => Some(first),
        }
    }

    fn rebuild_layout(&mut self) {
        // Keep the same byte at the top when the row width or the display mode changes.
        let top_offset = self.top_row().and_then(|t| self.layout.row(t)).and_then(|r| r.left.or(r.right).map(|x| x.start));
        let Some(diff) = &self.diff else {
            self.layout = HexLayout::default();
            self.spans.clear();
            self.filter = None;
            self.marks.clear();
            return;
        };
        self.layout = HexLayout::new(diff, self.bytes_per_row);
        self.spans = change_spans(&diff.segments, self.layout.bytes_per_row);
        let count = self.layout.row_count();
        self.filter = (!self.show_all).then(|| Filter::new(&self.spans, count, CONTEXT));
        let disp_spans: Vec<Span> = self.spans.iter().map(|s| Span { start: self.locate(s.start).0, ..*s }).collect();
        self.marks = overview_marks(&disp_spans, self.disp_count());
        let top_row = top_offset.and_then(|o| self.layout.row_of_offset(o, false).or_else(|| self.layout.row_of_offset(o, true))).unwrap_or(0);
        self.top = self.locate(top_row).0;
        self.clamp_top();
        self.current = self.current.filter(|&c| c < self.spans.len());
        self.selected = self.selected.filter(|&s| s < count);
    }

    /// Switch between "All" and "Diffs" display.
    fn set_show_all(&mut self, all: bool) {
        if self.show_all != all {
            self.show_all = all;
            self.rebuild_layout();
        }
    }

    fn disp_count(&self) -> usize {
        self.filter.as_ref().map_or(self.layout.row_count(), |f| f.total)
    }

    fn disp_row(&self, d: usize) -> Option<DispRow> {
        match &self.filter {
            Some(f) => f.get(d),
            None => (d < self.layout.row_count()).then_some(DispRow::Row(d)),
        }
    }

    /// Display row of layout row `row` (or of the separator hiding it) and whether the row is shown.
    fn locate(&self, row: usize) -> (usize, bool) {
        match &self.filter {
            Some(f) => f.locate(row),
            None => (row, true),
        }
    }

    fn max_top(&self) -> usize {
        self.disp_count().saturating_sub(self.visible_rows.max(1))
    }

    fn clamp_top(&mut self) {
        self.top = self.top.min(self.max_top());
    }

    /// Scroll so layout row `row` is visible with a little context above it.
    fn reveal(&mut self, row: usize) {
        let (d, _) = self.locate(row);
        if d < self.top || d >= self.top + self.visible_rows.max(1) {
            self.top = d.saturating_sub(3);
            self.clamp_top();
        }
    }

    fn go_to_diff(&mut self, idx: usize) {
        if let Some(span) = self.spans.get(idx).copied() {
            self.current = Some(idx);
            self.selected = Some(span.start);
            self.reveal(span.start);
        }
    }

    fn next_diff(&mut self, forward: bool) {
        let idx = match (self.current, forward) {
            (Some(c), true) => Some(c + 1),
            (Some(c), false) => c.checked_sub(1),
            (None, true) => self.spans.iter().position(|s| self.locate(s.start).0 >= self.top),
            (None, false) => self.spans.iter().rposition(|s| self.locate(s.start).0 < self.top + self.visible_rows),
        };
        if let Some(i) = idx {
            self.go_to_diff(i);
        }
    }

    fn goto_offset(&mut self, cx: &mut Cx<'_>) {
        let Some(off) = parse_offset(&self.offset_text) else {
            cx.toasts.warning(format!("“{}” is not an offset (use decimal or 0x-prefixed hex)", self.offset_text.trim()));
            return;
        };
        match self.layout.row_of_offset(off, false).or_else(|| self.layout.row_of_offset(off, true)) {
            Some(row) => {
                if !self.locate(row).1 {
                    // The row is hidden in "Diffs" mode: show everything.
                    self.set_show_all(true);
                }
                self.selected = Some(row);
                self.top = self.locate(row).0.saturating_sub(self.visible_rows / 2);
                self.clamp_top();
            }
            None => cx.toasts.warning(format!("Offset {off} (0x{off:X}) is past the end of both files")),
        }
    }

    fn open_goto(&mut self) {
        self.goto_open = true;
        self.focus_offset = true;
    }

    fn swap(&mut self) {
        std::mem::swap(&mut self.left_path, &mut self.right_path);
        std::mem::swap(&mut self.left, &mut self.right);
        if let Some(d) = self.diff.take() {
            self.diff = Some(swap_diff(d));
        }
        self.rebuild_layout();
    }

    /// The row as two text lines (left, right) for the clipboard.
    fn row_text(&self, row: usize) -> Option<String> {
        let r = self.layout.row(row)?;
        let digits = offset_digits(self.left.bytes.len().max(self.right.bytes.len()));
        let line = |side: &Side, range: Option<std::ops::Range<usize>>| match range.and_then(|x| side.bytes.get(x.clone()).map(|b| (x.start, b))) {
            Some((start, b)) => format!("{start:0digits$X}  {}  {}", hex_text(b), ascii_text(b)),
            None => String::new(),
        };
        Some(format!("{}\n{}", line(&self.left, r.left), line(&self.right, r.right)))
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        toolbar::bar(ui, "hex-toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            if toolbar::toggle(ui, Icon::ShowAll, "All", "Show every row", self.show_all, true) {
                self.set_show_all(true);
            }
            if toolbar::toggle(ui, Icon::ShowDiffs, "Diffs", "Show only rows with differences, with a little context", !self.show_all, true) {
                self.set_show_all(false);
            }
            toolbar::separator(ui);
            let has = !self.spans.is_empty();
            if toolbar::button(ui, Icon::PrevDiff, "Prev Section", "Previous difference section (F7)", has) {
                self.next_diff(false);
            }
            if toolbar::button(ui, Icon::NextDiff, "Next Section", "Next difference section (F8)", has) {
                self.next_diff(true);
            }
            toolbar::separator(ui);
            let current = self.bytes_per_row;
            let mut pick = None;
            toolbar::menu(ui, Icon::Hex, "Bytes", &format!("Bytes per row ({current})"), true, |ui| {
                for n in BYTES_PER_ROW_CHOICES {
                    if ui.radio(current == n, format!("{n} bytes per row")).clicked() {
                        pick = Some(n);
                        ui.close();
                    }
                }
            });
            if let Some(n) = pick
                && n != self.bytes_per_row
            {
                self.bytes_per_row = n;
                self.rebuild_layout();
            }
            if toolbar::toggle(ui, Icon::Rules, "Align", "Find inserted and deleted bytes instead of comparing byte N with byte N", self.align, true) {
                self.align = !self.align;
                self.recompare(ui.ctx());
            }
            if toolbar::toggle(ui, Icon::Find, "Go To", "Go to an offset (Ctrl+G)", self.goto_open, self.layout.row_count() > 0 || self.goto_open) {
                if self.goto_open {
                    self.goto_open = false;
                } else {
                    self.open_goto();
                }
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Swap, "Swap", "Swap the left and right files", true) {
                self.swap();
            }
            if toolbar::button(ui, Icon::Reload, "Reload", "Load both files again (F5)", true) {
                self.reload(ui.ctx());
            }
        });
    }

    /// Path rows, file information, the go-to strip and job progress.
    fn header(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let pal = Palette::get(ui.ctx());
        egui::Panel::top("hex-paths").frame(egui::Frame::NONE.fill(pal.toolbar_bg)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let side_w = ((ui.available_width() - STRIP_W - DIVIDER_W) / 2.0).max(0.0);
            let mut reload = false;
            two_columns(ui, "hex-cols", side_w, DIVIDER_W, &pal, |ui, left| {
                let (path, side, id, hint) = if left {
                    (&mut self.left_path, &self.left, "hex-left", "Left file")
                } else {
                    (&mut self.right_path, &self.right, "hex-right", "Right file")
                };
                toolbar::header_strip(ui, 26.0, |ui| {
                    reload |= widgets::path_bar(ui, id, path, false, hint).submitted;
                });
                info_strip(ui, &side_info(side), side.error.as_deref(), &pal);
            });
            if reload {
                self.reload(ui.ctx());
            }
            if self.goto_open {
                self.goto_strip(ui, cx, &pal);
            }
            if let Some(job) = &self.job {
                let cancel = egui::Frame::NONE.fill(pal.toolbar_bg).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| jobs::progress_ui(ui, job)).inner;
                if cancel {
                    self.job = None;
                }
            }
        });
    }

    fn goto_strip(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, pal: &Palette) {
        egui::Frame::NONE.fill(pal.header_bg).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([pos2(r.left() - 8.0, r.top() - 2.0), pos2(r.right() + 8.0, r.top() - 2.0)], Stroke::new(1.0, pal.border));
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 24.0), Layout::left_to_right(Align::Center), |ui| {
                ui.label(RichText::new("Go to offset").color(pal.dim_text));
                let r = ui.add(egui::TextEdit::singleline(&mut self.offset_text).id_salt("hex-offset").hint_text("0x1F or 31").desired_width(140.0));
                if self.focus_offset {
                    r.request_focus();
                    self.focus_offset = false;
                }
                self.offset_focused = r.has_focus();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if enter || ui.button("Go").clicked() {
                    self.goto_offset(cx);
                }
                if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.goto_open = false;
                    self.offset_focused = false;
                }
                ui.label(RichText::new("Decimal, or hex with 0x, $ or h").size(11.5).color(pal.dim_text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if toolbar::small(ui, Icon::Close, "Close (Esc)", true) {
                        self.goto_open = false;
                        self.offset_focused = false;
                    }
                });
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        toolbar::status_bar(ui, "hex-status", |ui| {
            match &self.diff {
                None => {
                    ui.label(RichText::new(if self.job.is_some() { "Comparing…" } else { "Nothing compared" }).color(pal.dim_text));
                }
                Some(d) if d.is_identical() && self.left.present && self.right.present => {
                    toolbar::status_icon(ui, Icon::Check);
                    ui.label(RichText::new("Files are identical").color(pal.success));
                }
                Some(d) => {
                    let sections = d.segments.iter().filter(|s| s.is_change()).count() as u64;
                    toolbar::status_icon(ui, Icon::ShowDiffs);
                    ui.label(format!("{} difference section{}", widgets::thousands(sections), if sections == 1 { "" } else { "s" }));
                    toolbar::status_sep(ui);
                    let n = d.differing_bytes() as u64;
                    ui.label(format!("{} differing byte{}", widgets::thousands(n), if n == 1 { "" } else { "s" }));
                }
            }
            toolbar::status_sep(ui);
            let size = |s: &Side| if s.present { widgets::format_size(s.bytes.len() as u64) } else { "no file".to_owned() };
            ui.label(format!("Left {}", size(&self.left))).on_hover_text(format!("{} bytes", widgets::thousands(self.left.bytes.len() as u64)));
            toolbar::status_sep(ui);
            ui.label(format!("Right {}", size(&self.right))).on_hover_text(format!("{} bytes", widgets::thousands(self.right.bytes.len() as u64)));
            if let Some(d) = &self.diff {
                toolbar::status_sep(ui);
                if self.align && !d.aligned {
                    ui.label(RichText::new("Positional: files too large to align").color(pal.warning));
                } else {
                    let (label, tip) =
                        if d.aligned { ("Aligned", "Inserted and deleted bytes are found") } else { ("Positional", "Byte N is compared with byte N") };
                    ui.label(RichText::new(label).color(pal.dim_text)).on_hover_text(tip);
                }
            }
            if let Some(f) = &self.filter {
                toolbar::status_sep(ui);
                ui.label(
                    RichText::new(format!("Showing {} of {} rows", widgets::thousands(f.total as u64), widgets::thousands(self.layout.row_count() as u64)))
                        .color(pal.dim_text),
                );
            }
        });
    }

    fn body(&mut self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        let font = theme::mono(ui.ctx());
        let (text_h, cw) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
        let row_h = (text_h + 3.0).max(4.0);
        let cw = cw.max(1.0);
        let full = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(full, Sense::click());
        let bpr = self.layout.bytes_per_row.max(1);
        let digits = offset_digits(self.left.bytes.len().max(self.right.bytes.len()));
        let content_w = side_chars(digits, bpr) as f32 * cw + 2.0 * PAD;
        let hdr_h = row_h + 2.0;

        let main_w = (full.width() - STRIP_W).max(0.0);
        let side_w = ((main_w - DIVIDER_W) / 2.0).max(0.0);
        let needs_hbar = content_w > side_w;
        let rows_top = full.top() + hdr_h;
        let rows_h = (full.height() - hdr_h - if needs_hbar { HBAR_H } else { 0.0 }).max(0.0);
        self.visible_rows = ((rows_h / row_h).floor() as usize).max(1);
        let count = self.disp_count();

        // Input: wheel, keys, clicks.
        if resp.hovered() {
            let delta = ui.input(|i| i.smooth_scroll_delta);
            self.scroll_accum -= delta.y;
            let steps = (self.scroll_accum / row_h).trunc();
            if steps != 0.0 {
                self.scroll_accum -= steps * row_h;
                self.top = if steps > 0.0 { self.top.saturating_add(steps as usize) } else { self.top.saturating_sub((-steps) as usize) };
            }
            if delta.x != 0.0 {
                self.hscroll -= delta.x;
            }
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            let page = self.visible_rows.saturating_sub(1).max(1);
            ui.input(|i| {
                if i.modifiers.is_none() {
                    if i.key_pressed(Key::ArrowDown) {
                        self.top = self.top.saturating_add(1);
                    }
                    if i.key_pressed(Key::ArrowUp) {
                        self.top = self.top.saturating_sub(1);
                    }
                }
                if i.key_pressed(Key::PageDown) {
                    self.top = self.top.saturating_add(page);
                }
                if i.key_pressed(Key::PageUp) {
                    self.top = self.top.saturating_sub(page);
                }
                if i.modifiers.matches_logically(Modifiers::COMMAND) && i.key_pressed(Key::Home) {
                    self.top = 0;
                }
                if i.modifiers.matches_logically(Modifiers::COMMAND) && i.key_pressed(Key::End) {
                    self.top = usize::MAX;
                }
            });
        }
        self.clamp_top();
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && pos.x < full.left() + main_w
            && pos.y >= rows_top
        {
            let d = self.top + ((pos.y - rows_top) / row_h).max(0.0) as usize;
            match self.disp_row(d) {
                Some(DispRow::Row(r)) => self.selected = Some(r),
                Some(DispRow::Hidden { first, .. }) => {
                    // Expand: show every row, keeping the clicked place in view.
                    self.set_show_all(true);
                    self.selected = Some(first);
                    self.top = first.saturating_sub(3);
                    self.clamp_top();
                }
                None => self.selected = None,
            }
        }

        let left_rect = Rect::from_min_size(pos2(full.left(), rows_top), vec2(side_w, rows_h));
        let right_rect = Rect::from_min_size(pos2(full.left() + side_w + DIVIDER_W, rows_top), vec2(side_w, rows_h));
        let painter = ui.painter_at(full);
        let rows_area = Rect::from_min_size(pos2(full.left(), rows_top), vec2(main_w, rows_h));
        painter.rect_filled(Rect::from_min_size(full.min, vec2(main_w, full.height())), 0.0, pal.pane_bg);

        let geo = Geo { cw, row_h, digits, bpr, hscroll: self.hscroll };
        // Column header per pane.
        for rect in [left_rect, right_rect] {
            let hr = Rect::from_min_size(pos2(rect.left(), full.top()), vec2(rect.width(), hdr_h));
            paint_header(&ui.painter_at(hr), hr, &geo, &pal);
        }

        let end = (self.top + self.visible_rows + 1).min(count);
        let (lp, rp) = (ui.painter_at(left_rect), ui.painter_at(right_rect));
        let small = FontId::proportional(12.0);
        for d in self.top..end {
            let y = rows_top + (d - self.top) as f32 * row_h;
            match self.disp_row(d) {
                Some(DispRow::Row(r)) => {
                    let Some(row) = self.layout.row(r) else { continue };
                    let left = row.left.as_ref().and_then(|x| self.left.bytes.get(x.clone()));
                    let right = row.right.as_ref().and_then(|x| self.right.bytes.get(x.clone()));
                    let lo = row.left.as_ref().map(|x| x.start);
                    let ro = row.right.as_ref().map(|x| x.start);
                    paint_side(&lp, left_rect, y, &geo, &font, &pal, row.kind, lo.zip(left), right);
                    paint_side(&rp, right_rect, y, &geo, &font, &pal, row.kind, ro.zip(right), left);
                    if self.selected == Some(r) {
                        painter.rect_filled(Rect::from_min_size(pos2(full.left(), y), vec2(main_w, row_h)), 0.0, pal.selection_bg);
                    }
                }
                Some(DispRow::Hidden { count: n, .. }) => {
                    let label = format!("{} identical row{} hidden", widgets::thousands(n as u64), if n == 1 { "" } else { "s" });
                    for (p, rect) in [(&lp, left_rect), (&rp, right_rect)] {
                        let sr = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), row_h));
                        p.rect_filled(sr, 0.0, pal.separator_bg);
                        p.line_segment([sr.left_top(), sr.right_top()], Stroke::new(1.0, pal.border));
                        p.line_segment([sr.left_bottom(), sr.right_bottom()], Stroke::new(1.0, pal.border));
                        p.text(sr.center(), Align2::CENTER_CENTER, &label, small.clone(), pal.dim_text);
                    }
                }
                None => {}
            }
        }
        // Outline the current difference.
        if let Some(span) = self.current.and_then(|c| self.spans.get(c)) {
            let start = self.locate(span.start).0;
            let y0 = rows_top + (start as f32 - self.top as f32) * row_h;
            let y1 = y0 + span.rows as f32 * row_h;
            let rect = Rect::from_min_max(pos2(full.left() + 1.0, y0.max(rows_top - 2.0)), pos2(full.left() + main_w - 1.0, y1.min(rows_top + rows_h + 2.0)));
            if rect.height() > 0.0 {
                painter.with_clip_rect(rows_area).rect_stroke(rect, 2.0, Stroke::new(1.5, pal.current_outline), StrokeKind::Inside);
            }
        }
        if count == 0 && self.job.is_none() && (self.left.present || self.right.present) {
            painter.text(rows_area.center(), Align2::CENTER_CENTER, "Both files are empty", FontId::proportional(14.0), pal.dim_text);
        }
        for (rect, side) in [(left_rect, &self.left), (right_rect, &self.right)] {
            if !side.present && self.job.is_none() {
                let msg = if side.error.is_some() { "Could not load file" } else { "No file" };
                painter.text(pos2(rect.center().x, rect.top() + 24.0), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.dim_text);
            }
        }

        // Centred gutter between the panes, with thin borders.
        let gutter = Rect::from_min_size(pos2(full.left() + side_w, full.top()), vec2(DIVIDER_W, full.height()));
        painter.rect_filled(gutter, 0.0, pal.gutter_bg);
        let border = Stroke::new(1.0, pal.border);
        for x in [gutter.left() + 0.5, gutter.right() - 0.5, full.left() + main_w - 0.5] {
            painter.line_segment([pos2(x, full.top()), pos2(x, full.bottom())], border);
        }

        // Horizontal scrollbar shared by both sides.
        if needs_hbar {
            let bar = Rect::from_min_size(pos2(full.left(), rows_top + rows_h), vec2(main_w, HBAR_H));
            self.hscroll = widgets::hscrollbar(ui, bar, self.hscroll, content_w, side_w, &pal);
        } else {
            self.hscroll = 0.0;
        }

        // Overview strip: where the differences are; click to scroll.
        let strip = Rect::from_min_size(pos2(full.left() + main_w, full.top()), vec2(STRIP_W, full.height()));
        let total = count.max(1) as f32;
        let marks: Vec<OverviewMark> =
            self.marks.iter().map(|&(start, end, kind)| OverviewMark { start, end, color: pal.overview(row_kind(kind), false) }).collect();
        let view = (self.top as f32 / total, ((self.top + self.visible_rows) as f32 / total).min(1.0));
        if let Some(f) = widgets::overview_strip(ui, strip, &marks, view, &pal) {
            self.top = ((f * total) as usize).saturating_sub(self.visible_rows / 2);
            self.clamp_top();
        }
    }
}

/// Two side-by-side columns (left `left_w` wide, then a `gap`, then the rest) filled by `add`.
fn two_columns(ui: &mut Ui, id: &str, left_w: f32, gap: f32, pal: &Palette, mut add: impl FnMut(&mut Ui, bool)) {
    let origin = ui.cursor().min;
    let full_w = ui.available_width();
    let h = ui.available_height().max(1.0);
    let mut bottom = origin.y;
    for (x, w, left) in [(origin.x, left_w, true), (origin.x + left_w + gap, (full_w - left_w - gap).max(0.0), false)] {
        let rect = Rect::from_min_size(pos2(x, origin.y), vec2(w, h));
        let mut child = ui.new_child(UiBuilder::new().id_salt((id, left)).max_rect(rect).layout(Layout::top_down(Align::Min)));
        child.spacing_mut().item_spacing.y = 0.0;
        add(&mut child, left);
        bottom = bottom.max(child.min_rect().bottom());
    }
    let gutter = Rect::from_min_max(pos2(origin.x + left_w, origin.y), pos2(origin.x + left_w + gap, bottom));
    let p = ui.painter();
    p.rect_filled(gutter, 0.0, pal.gutter_bg);
    for x in [gutter.left() + 0.5, gutter.right() - 0.5] {
        p.line_segment([pos2(x, gutter.top()), pos2(x, gutter.bottom())], Stroke::new(1.0, pal.border));
    }
    p.line_segment([pos2(origin.x, bottom - 0.5), pos2(origin.x + full_w, bottom - 0.5)], Stroke::new(1.0, pal.border));
    ui.allocate_rect(Rect::from_min_max(origin, pos2(origin.x + full_w, bottom)), Sense::hover());
}

/// A slim strip of file facts under a path row.
fn info_strip(ui: &mut Ui, items: &[String], error: Option<&str>, pal: &Palette) {
    egui::Frame::NONE.fill(pal.toolbar_bg).inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| {
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 20.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if let Some(e) = error {
                ui.label(RichText::new(jobs::shorten(e, 120)).size(11.5).color(pal.error)).on_hover_text(e);
            }
            for it in items {
                ui.label(RichText::new(it).size(11.5).color(pal.dim_text));
            }
        });
    });
}

/// Size and modification time of a loaded side.
fn side_info(s: &Side) -> Vec<String> {
    if !s.present {
        return if s.error.is_some() { Vec::new() } else { vec!["No file".to_owned()] };
    }
    let n = s.bytes.len() as u64;
    let mut v = vec![format!("{} ({} bytes)", widgets::format_size(n), widgets::thousands(n))];
    let m = widgets::format_mtime(s.modified);
    if !m.is_empty() {
        v.push(format!("Modified {m}"));
    }
    v
}

/// Character-grid geometry shared by every row.
struct Geo {
    cw: f32,
    row_h: f32,
    digits: usize,
    bpr: usize,
    hscroll: f32,
}

/// The column header of one pane: "Offset", byte column numbers and "ASCII".
fn paint_header(p: &egui::Painter, rect: Rect, g: &Geo, pal: &Palette) {
    p.rect_filled(rect, 0.0, pal.header_bg);
    p.line_segment([rect.left_bottom() - vec2(0.0, 0.5), rect.right_bottom() - vec2(0.0, 0.5)], Stroke::new(1.0, pal.border));
    let font = FontId::monospace((g.row_h - 6.0).clamp(8.0, 13.0));
    let x0 = rect.left() + PAD - g.hscroll;
    let col_x = |col: usize| x0 + col as f32 * g.cw;
    let cy = rect.center().y;
    let hex0 = g.digits + 2;
    let ascii0 = hex0 + hex_width(g.bpr) + 2;
    p.text(pos2(x0, cy), Align2::LEFT_CENTER, "Offset", FontId::proportional(11.5), pal.dim_text);
    let cols: Vec<u8> = (0..g.bpr).map(|i| u8::try_from(i).unwrap_or(0)).collect();
    p.text(pos2(col_x(hex0), cy), Align2::LEFT_CENTER, hex_text(&cols), font, pal.dim_text);
    p.text(pos2(col_x(ascii0), cy), Align2::LEFT_CENTER, "ASCII", FontId::proportional(11.5), pal.dim_text);
}

/// Paint one side of a row: background, differing-byte highlights, offset, hex and ASCII.
/// `bytes` is (offset, bytes) when this side has bytes on the row; `other` is the other side's bytes.
#[allow(clippy::too_many_arguments)]
fn paint_side(
    p: &egui::Painter,
    rect: Rect,
    y: f32,
    g: &Geo,
    font: &FontId,
    pal: &Palette,
    kind: HunkKind,
    bytes: Option<(usize, &[u8])>,
    other: Option<&[u8]>,
) {
    let row_rect = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), g.row_h));
    let x0 = rect.left() + PAD - g.hscroll;
    let col_x = |col: usize| x0 + col as f32 * g.cw;
    let gutter_r = col_x(g.digits) + PAD;
    let Some((offset, b)) = bytes else {
        pal.hatch_rect(p, row_rect);
        return;
    };
    let changed = kind != HunkKind::Equal;
    if changed {
        p.rect_filled(row_rect, 0.0, pal.row_bg(row_kind(kind), true, false));
    }
    p.rect_filled(Rect::from_min_max(pos2(rect.left(), y), pos2(gutter_r, y + g.row_h)), 0.0, pal.gutter_bg);
    let hex0 = g.digits + 2;
    let ascii0 = hex0 + hex_width(g.bpr) + 2;
    let mut differs = [false; 64];
    if changed {
        for run in differing_runs(kind, b, other) {
            let (first, last) = (run.start, run.end.saturating_sub(1));
            let hx = Rect::from_min_max(pos2(col_x(hex0 + hex_col(first)) - 2.0, y + 1.0), pos2(col_x(hex0 + hex_col(last) + 2) + 2.0, y + g.row_h - 1.0));
            p.rect_filled(hx, 2.0, pal.changed_strong);
            let ax = Rect::from_min_max(pos2(col_x(ascii0 + first), y + 1.0), pos2(col_x(ascii0 + last + 1), y + g.row_h - 1.0));
            p.rect_filled(ax, 1.0, pal.changed_strong);
            for i in run {
                if let Some(d) = differs.get_mut(i) {
                    *d = true;
                }
            }
        }
        // Bytes the other side has past this side's end on the row: missing here.
        if let Some(o) = other
            && o.len() > b.len()
        {
            let hx = Rect::from_min_max(pos2(col_x(hex0 + hex_col(b.len())) - 2.0, y), pos2(col_x(hex0 + hex_col(o.len() - 1) + 2) + 2.0, y + g.row_h));
            pal.hatch_rect(p, hx);
            let ax = Rect::from_min_max(pos2(col_x(ascii0 + b.len()), y), pos2(col_x(ascii0 + o.len()), y + g.row_h));
            pal.hatch_rect(p, ax);
        }
    }
    let is_diff = |i: usize| differs.get(i).copied().unwrap_or(false);
    let ty = y + 1.5;
    p.text(pos2(x0, ty), Align2::LEFT_TOP, format!("{offset:0w$X}", w = g.digits), font.clone(), pal.gutter_text);
    p.text(pos2(col_x(hex0), ty), Align2::LEFT_TOP, hex_text_where(b, |i| !is_diff(i)), font.clone(), pal.text);
    p.text(pos2(col_x(ascii0), ty), Align2::LEFT_TOP, ascii_text_where(b, |i| !is_diff(i)), font.clone(), pal.text);
    if changed {
        p.text(pos2(col_x(hex0), ty), Align2::LEFT_TOP, hex_text_where(b, is_diff), font.clone(), pal.changed_text);
        p.text(pos2(col_x(ascii0), ty), Align2::LEFT_TOP, ascii_text_where(b, is_diff), font.clone(), pal.changed_text);
    }
}

fn row_kind(kind: HunkKind) -> RowKind {
    match kind {
        HunkKind::Equal => RowKind::Same,
        HunkKind::Replace => RowKind::Changed,
        HunkKind::Delete => RowKind::LeftOnly,
        HunkKind::Insert => RowKind::RightOnly,
    }
}

fn load_side(path: &str) -> Side {
    if path.trim().is_empty() {
        return Side::default();
    }
    let p = widgets::clean_path(path);
    match binary::load(&p) {
        Ok(b) => Side { bytes: Arc::new(b), present: true, error: None, modified: std::fs::metadata(&p).and_then(|m| m.modified()).ok() },
        Err(e) => Side { error: Some(e.to_string()), ..Side::default() },
    }
}

/// Number of hex digits for offsets: at least 8, more for files past 4 GiB.
fn offset_digits(len: usize) -> usize {
    let mut d = 1;
    let mut v = len;
    while v >= 16 {
        v /= 16;
        d += 1;
    }
    d.max(8)
}

/// Column (in characters) of byte `i` inside the hex area: `XX XX … XX  XX …` (extra gap every 8).
fn hex_col(i: usize) -> usize {
    i * 3 + i / 8
}

/// Width in characters of a full hex area of `bpr` bytes.
fn hex_width(bpr: usize) -> usize {
    if bpr == 0 { 0 } else { hex_col(bpr - 1) + 2 }
}

/// Width in characters of one side: offset, gap, hex, gap, ASCII.
fn side_chars(digits: usize, bpr: usize) -> usize {
    digits + 2 + hex_width(bpr) + 2 + bpr
}

fn hex_text(bytes: &[u8]) -> String {
    hex_text_where(bytes, |_| true)
}

/// Hex digits of the bytes where `keep(i)`, spaces for the others (same column layout).
fn hex_text_where(bytes: &[u8], keep: impl Fn(usize) -> bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(bytes.len() * 3 + bytes.len() / 8);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
            if i.is_multiple_of(8) {
                s.push(' ');
            }
        }
        if keep(i) {
            for nibble in [b >> 4, b & 0xF] {
                s.push(HEX.get(usize::from(nibble)).map_or('?', |&c| char::from(c)));
            }
        } else {
            s.push_str("  ");
        }
    }
    s
}

/// Printable ASCII as-is, everything else as '·'.
fn ascii_text(bytes: &[u8]) -> String {
    ascii_text_where(bytes, |_| true)
}

/// [`ascii_text`] with spaces in place of the bytes where `keep(i)` is false.
fn ascii_text_where(bytes: &[u8], keep: impl Fn(usize) -> bool) -> String {
    bytes
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if !keep(i) {
                ' '
            } else if (0x20..0x7f).contains(&b) {
                char::from(b)
            } else {
                '·'
            }
        })
        .collect()
}

/// Runs of byte indices on this side of a changed row that should be strongly highlighted.
fn differing_runs(kind: HunkKind, bytes: &[u8], other: Option<&[u8]>) -> Vec<std::ops::Range<usize>> {
    let differs = |i: usize| match kind {
        HunkKind::Equal => false,
        HunkKind::Insert | HunkKind::Delete => true,
        HunkKind::Replace => other.and_then(|o| o.get(i)) != bytes.get(i),
    };
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    for i in 0..bytes.len() {
        if differs(i) {
            match runs.last_mut() {
                Some(r) if r.end == i => r.end = i + 1,
                _ => runs.push(i..i + 1),
            }
        }
    }
    runs
}

/// Parse a user-typed offset: decimal, or hex with a `0x`/`$` prefix or `h` suffix.
fn parse_offset(s: &str) -> Option<usize> {
    let s = s.trim().replace(['_', ','], "");
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).or_else(|| s.strip_prefix('$')).or_else(|| s.strip_suffix(['h', 'H'])) {
        return usize::from_str_radix(h, 16).ok();
    }
    s.parse().ok()
}

/// The diff with left and right exchanged.
fn swap_diff(d: BinaryDiff) -> BinaryDiff {
    let segments = d
        .segments
        .into_iter()
        .map(|h| {
            let kind = match h.kind {
                HunkKind::Insert => HunkKind::Delete,
                HunkKind::Delete => HunkKind::Insert,
                k => k,
            };
            Hunk { kind, a: h.b, b: h.a, unimportant: h.unimportant }
        })
        .collect();
    BinaryDiff { segments, aligned: d.aligned }
}

/// Changed segments as row spans (rows are counted exactly as [`HexLayout`] does).
fn change_spans(segments: &[Hunk], bpr: usize) -> Vec<Span> {
    let bpr = bpr.max(1);
    let mut row = 0usize;
    let mut out = Vec::new();
    for s in segments {
        let rows = s.a.len().max(s.b.len()).div_ceil(bpr);
        if s.is_change() {
            out.push(Span { start: row, rows, kind: s.kind });
        }
        row = row.saturating_add(rows);
    }
    out
}

/// Overview marks as document fractions, merging marks closer than ~1/1000 so huge diffs stay cheap.
fn overview_marks(spans: &[Span], total_rows: usize) -> Vec<(f32, f32, HunkKind)> {
    const MIN_GAP: f32 = 0.001;
    let total = total_rows.max(1) as f32;
    let mut out: Vec<(f32, f32, HunkKind)> = Vec::new();
    for s in spans {
        let start = s.start as f32 / total;
        let end = s.start.saturating_add(s.rows) as f32 / total;
        match out.last_mut() {
            Some(last) if start - last.1 < MIN_GAP => {
                last.1 = last.1.max(end);
                if last.2 != s.kind {
                    last.2 = HunkKind::Replace;
                }
            }
            _ => out.push((start, end, s.kind)),
        }
    }
    out
}

impl View for HexView {
    fn kind(&self) -> SessionKind {
        SessionKind::HexCompare
    }

    fn title(&self) -> String {
        self.session().map(|s| s.title()).unwrap_or_default()
    }

    fn tooltip(&self) -> String {
        format!("Left: {}\nRight: {}", self.left_path, self.right_path)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        self.poll_job(cx);
        self.toolbar(ui, cx);
        self.header(ui, cx);
        if !self.goto_open {
            self.offset_focused = false;
        }
        self.status_bar(ui);
        self.body(ui);
    }

    fn supports(&self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::NextDiff | Cmd::PrevDiff | Cmd::FirstDiff | Cmd::LastDiff => !self.spans.is_empty(),
            Cmd::Refresh | Cmd::SwapSides => true,
            Cmd::GotoLine => self.layout.row_count() > 0,
            Cmd::Copy => self.selected.is_some(),
            _ => false,
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, _cx: &mut Cx<'_>) {
        match cmd {
            Cmd::NextDiff => self.next_diff(true),
            Cmd::PrevDiff => self.next_diff(false),
            Cmd::FirstDiff => self.go_to_diff(0),
            Cmd::LastDiff => self.go_to_diff(self.spans.len().saturating_sub(1)),
            Cmd::Refresh => self.reload(ctx),
            Cmd::SwapSides => self.swap(),
            Cmd::GotoLine => self.open_goto(),
            Cmd::Copy => {
                if let Some(t) = self.selected.and_then(|r| self.row_text(r)) {
                    ctx.copy_text(t);
                }
            }
            _ => {}
        }
    }

    fn session(&self) -> Option<Session> {
        Some(Session { kind: SessionKind::HexCompare, left: self.left_path.clone(), right: self.right_path.clone(), ..self.session.clone() })
    }

    fn wants_keyboard(&self) -> bool {
        self.offset_focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_ascii_formatting() {
        assert_eq!(hex_text(&[0x00, 0xAB, 0x7F]), "00 AB 7F");
        let nine: Vec<u8> = (0..9).collect();
        assert_eq!(hex_text(&nine), "00 01 02 03 04 05 06 07  08");
        assert_eq!(hex_text(&[]), "");
        assert_eq!(ascii_text(b"Hi\x00\xff~"), "Hi··~");
        // Byte columns match the text layout.
        let s = hex_text(&nine);
        assert_eq!(s.get(hex_col(8)..hex_col(8) + 2), Some("08"));
        assert_eq!(s.get(hex_col(7)..hex_col(7) + 2), Some("07"));
        let sixteen: Vec<u8> = (0..16).collect();
        assert_eq!(hex_text(&sixteen).len(), hex_width(16));
        assert_eq!(side_chars(8, 16), 8 + 2 + 48 + 2 + 16);
    }

    #[test]
    fn masked_text_keeps_columns() {
        let nine: Vec<u8> = (0..9).collect();
        let only = hex_text_where(&nine, |i| i == 8);
        assert_eq!(only.chars().count(), hex_text(&nine).chars().count());
        assert_eq!(only.trim(), "08");
        assert_eq!(only.get(hex_col(8)..hex_col(8) + 2), Some("08"));
        assert_eq!(ascii_text_where(b"abc", |i| i != 1), "a c");
    }

    #[test]
    fn diffs_filter_maps_rows() {
        // 100 rows, changes at rows 10..12 and 14..15 (merged with context) and 60.
        let spans = [
            Span { start: 10, rows: 2, kind: HunkKind::Replace },
            Span { start: 14, rows: 1, kind: HunkKind::Replace },
            Span { start: 60, rows: 1, kind: HunkKind::Insert },
        ];
        let f = Filter::new(&spans, 100, 2);
        // [sep 0..8] 8..17, [sep 17..58] 58..63, [sep 63..100]
        assert_eq!(f.total, 1 + 9 + 1 + 5 + 1);
        assert_eq!(f.get(0), Some(DispRow::Hidden { first: 0, count: 8 }));
        assert_eq!(f.get(1), Some(DispRow::Row(8)));
        assert_eq!(f.get(9), Some(DispRow::Row(16)));
        assert_eq!(f.get(10), Some(DispRow::Hidden { first: 17, count: 41 }));
        assert_eq!(f.get(11), Some(DispRow::Row(58)));
        assert_eq!(f.get(16), Some(DispRow::Hidden { first: 63, count: 37 }));
        assert_eq!(f.get(17), None);
        assert_eq!(f.locate(10), (3, true));
        assert_eq!(f.locate(60), (13, true));
        assert_eq!(f.locate(3), (0, false));
        assert_eq!(f.locate(30), (10, false));
        assert_eq!(f.locate(99), (16, false));
        assert_eq!(f.locate(1000), (16, false));
        for d in 0..f.total {
            if let Some(DispRow::Row(r)) = f.get(d) {
                assert_eq!(f.locate(r), (d, true));
            }
        }
        // Edge cases: no rows, no differences, a change at the very start.
        assert_eq!(Filter::new(&[], 0, 2).total, 0);
        let none = Filter::new(&[], 5, 2);
        assert_eq!((none.total, none.get(0)), (1, Some(DispRow::Hidden { first: 0, count: 5 })));
        let start = Filter::new(&[Span { start: 0, rows: 1, kind: HunkKind::Replace }], 3, 2);
        assert_eq!((start.total, start.get(0), start.get(2)), (3, Some(DispRow::Row(0)), Some(DispRow::Row(2))));
        // Spans past the end never panic.
        assert_eq!(Filter::new(&[Span { start: usize::MAX, rows: usize::MAX, kind: HunkKind::Replace }], 3, 2).total, 1);
    }

    #[test]
    fn offsets() {
        assert_eq!(offset_digits(0), 8);
        assert_eq!(offset_digits(0xFFFF_FFFF), 8);
        assert_eq!(offset_digits(0x1_0000_0000), 9);
        assert_eq!(parse_offset("0x1F"), Some(31));
        assert_eq!(parse_offset(" 31 "), Some(31));
        assert_eq!(parse_offset("1fh"), Some(31));
        assert_eq!(parse_offset("$10"), Some(16));
        assert_eq!(parse_offset("1,024"), Some(1024));
        assert_eq!(parse_offset("zz"), None);
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("0x"), None);
    }

    #[test]
    fn differing_runs_only_mark_changed_bytes() {
        assert_eq!(differing_runs(HunkKind::Replace, b"abcd", Some(b"aXcY")), vec![1..2, 3..4]);
        assert_eq!(differing_runs(HunkKind::Replace, b"abcd", Some(b"ab")), vec![2..4]);
        assert_eq!(differing_runs(HunkKind::Insert, b"ab", None), vec![0..2]);
        assert!(differing_runs(HunkKind::Equal, b"ab", Some(b"ab")).is_empty());
    }

    #[test]
    fn spans_marks_and_swap() {
        let d = binary::compare(b"0123456789", b"01234X6789ABCDEFGHIJ", false);
        let spans = change_spans(&d.segments, 4);
        let layout = HexLayout::new(&d, 4);
        assert_eq!(spans.iter().map(|s| s.start).collect::<Vec<_>>(), layout.difference_rows());
        let marks = overview_marks(&spans, layout.row_count());
        assert!(!marks.is_empty() && marks.iter().all(|m| m.0 <= m.1 && m.1 <= 1.0));
        let s = swap_diff(d.clone());
        assert_eq!(s.differing_bytes(), d.differing_bytes());
        assert_eq!(s.segments.last().map(|h| h.kind), Some(HunkKind::Delete));
        assert_eq!(swap_diff(s), d);
        // Many close spans collapse into few marks.
        let many: Vec<Span> = (0..10_000).map(|i| Span { start: i * 2, rows: 1, kind: HunkKind::Replace }).collect();
        assert!(overview_marks(&many, 20_000).len() < 10);
        assert!(overview_marks(&[], 0).is_empty());
    }

    #[test]
    fn missing_and_bad_files() {
        assert!(!load_side("").present);
        let s = load_side("/definitely/not/here.bin");
        assert!(!s.present && s.error.is_some());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, [1u8, 2, 3]).unwrap();
        let s = load_side(&p.display().to_string());
        assert_eq!(s.bytes.len(), 3);
        assert!(s.modified.is_some());
        assert!(side_info(&s).iter().any(|i| i.contains("3 bytes")));
        assert_eq!(side_info(&Side::default()), vec!["No file".to_owned()]);
    }

    #[test]
    fn view_loads_and_navigates() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
        std::fs::write(&a, vec![0u8; 100]).unwrap();
        let mut data = vec![0u8; 100];
        data[50] = 1;
        data[90] = 2;
        std::fs::write(&b, &data).unwrap();
        let ctx = egui::Context::default();
        let s = Session::new(SessionKind::HexCompare, a.display().to_string(), b.display().to_string());
        let mut settings = Settings { hex_align: false, ..Settings::default() };
        let mut v = HexView::from_session(&s, &settings, &ctx);
        let (mut changed, mut toasts, mut open, mut exit) = (false, crate::view::Toasts::default(), Vec::new(), 0);
        let mut cx = Cx { settings: &mut settings, settings_changed: &mut changed, toasts: &mut toasts, open: &mut open, exit_code: &mut exit };
        for _ in 0..1000 {
            v.poll_job(&mut cx);
            if v.job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(v.diff.as_ref().map(|d| d.differing_bytes()), Some(2));
        assert_eq!(v.spans.len(), 2);
        v.run(Cmd::NextDiff, &ctx, &mut cx);
        assert_eq!(v.current, Some(0));
        v.run(Cmd::LastDiff, &ctx, &mut cx);
        assert_eq!(v.current, Some(1));
        assert!(v.supports(Cmd::Copy));
        assert!(v.row_text(v.selected.unwrap()).unwrap().contains("02"));
        v.offset_text = "0x5A".into();
        v.goto_offset(&mut cx);
        assert_eq!(v.selected, v.layout.row_of_offset(90, false));
        assert_eq!(v.selected, Some(v.spans[1].start));
        v.run(Cmd::GotoLine, &ctx, &mut cx);
        assert!(v.goto_open && v.focus_offset);

        // "Diffs" mode: only the changed rows plus context; navigation still lands on them.
        v.bytes_per_row = 8;
        v.rebuild_layout();
        v.set_show_all(false);
        assert!(v.disp_count() < v.layout.row_count());
        v.run(Cmd::FirstDiff, &ctx, &mut cx);
        assert_eq!(v.current, Some(0));
        let (d, shown) = v.locate(v.spans[0].start);
        assert!(shown);
        assert_eq!(v.disp_row(d), Some(DispRow::Row(v.spans[0].start)));
        // Going to a hidden offset switches back to showing every row.
        v.offset_text = "0".into();
        v.goto_offset(&mut cx);
        assert!(v.show_all);
        assert_eq!(v.selected, Some(0));

        v.run(Cmd::SwapSides, &ctx, &mut cx);
        assert_eq!(v.session().unwrap().left, b.display().to_string());
        assert_eq!(v.kind(), SessionKind::HexCompare);
    }
}
