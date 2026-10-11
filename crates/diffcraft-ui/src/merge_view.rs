//! Text Merge: a three-way merge of two changed versions of a file (Left, Right) against their
//! common ancestor (Base).
//!
//! The upper area shows Left | Base | Right aligned chunk by chunk (one shared vertical scroll),
//! the lower area shows the output assembled from the current resolution of every chunk.
//! Conflicts can be resolved per chunk (take left/base/right/both or edit by hand); every
//! resolution change can be undone. Saving keeps the left file's encoding and line endings.

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};

use diffcraft_core::fsutil;
use diffcraft_core::merge::{self, ChunkKind, MarkerLabels, MergeChunk, Resolution};
use diffcraft_core::text::file::{self as tfile, TextFile};
use diffcraft_core::text::{self, TextCompareOptions};
use diffcraft_session::{Session, SessionKind, Settings};
use egui::{Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

use crate::icons::{self, Icon};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette};
use crate::toolbar;
use crate::view::{Cmd, Cx, View};
use crate::widgets::{self, OverviewMark};

// ---------------------------------------------------------------------------------------------
// Model (pure, unit-tested)
// ---------------------------------------------------------------------------------------------

/// One of the three inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Base,
    Right,
}

impl Side {
    const ALL: [Side; 3] = [Side::Left, Side::Base, Side::Right];

    fn label(self) -> &'static str {
        match self {
            Side::Left => "Left",
            Side::Base => "Base",
            Side::Right => "Right",
        }
    }

    fn range(self, c: &MergeChunk) -> &Range<usize> {
        match self {
            Side::Left => &c.left,
            Side::Base => &c.base,
            Side::Right => &c.right,
        }
    }
}

/// A loaded input file.
#[derive(Clone, Debug)]
struct Input {
    file: TextFile,
    /// The path was empty or did not exist: treated as an empty file.
    missing: bool,
}

impl Input {
    fn missing() -> Self {
        Input { file: TextFile::default(), missing: true }
    }
}

/// Resolution used when a chunk index has no entry (never expected).
static FALLBACK_RESOLUTION: Resolution = Resolution::Base;

/// The merge state: inputs, chunks, resolutions, undo history and row layouts.
struct MergeModel {
    left: Input,
    base: Input,
    right: Input,
    labels: MarkerLabels,
    chunks: Vec<MergeChunk>,
    res: Vec<Resolution>,
    /// Resolutions at the last load or save (for `is_dirty`).
    saved: Vec<Resolution>,
    undo: Vec<(usize, Resolution)>,
    redo: Vec<(usize, Resolution)>,
    /// First row of each chunk in the upper (three-pane) area.
    up_starts: Vec<usize>,
    up_total: usize,
    /// First row of each chunk in the output pane (headers included).
    out_starts: Vec<usize>,
    /// First output line (0-based) of each chunk.
    out_line_starts: Vec<usize>,
    out_total: usize,
    out_lines: usize,
    /// Longest displayed line (characters, tabs expanded) of the inputs.
    max_chars: usize,
    /// Ordinal (1-based) of each chunk among the changes, and among the conflicts.
    change_no: Vec<usize>,
    conflict_no: Vec<usize>,
}

/// Rows a chunk occupies in the three-pane area (non-Unchanged chunks always get one row so
/// empty sides show a filler line).
fn chunk_height(c: &MergeChunk) -> usize {
    let n = c.left.len().max(c.base.len()).max(c.right.len());
    if c.kind == ChunkKind::Unchanged { n } else { n.max(1) }
}

/// Exclusive prefix sums: start of each item, and the total.
fn prefix_starts(heights: impl Iterator<Item = usize>) -> (Vec<usize>, usize) {
    let mut starts = Vec::new();
    let mut total = 0usize;
    for h in heights {
        starts.push(total);
        total = total.saturating_add(h);
    }
    (starts, total)
}

/// The item containing `row` and the offset inside it (zero-height items are skipped).
fn locate(starts: &[usize], total: usize, row: usize) -> Option<(usize, usize)> {
    if row >= total {
        return None;
    }
    let k = starts.partition_point(|&s| s <= row).checked_sub(1)?;
    Some((k, row - starts.get(k)?))
}

/// Number of lines `merge::render_chunk` produces for `res`.
fn rendered_len(c: &MergeChunk, res: &Resolution) -> usize {
    match res {
        Resolution::Base => c.base.len(),
        Resolution::Left => c.left.len(),
        Resolution::Right => c.right.len(),
        Resolution::LeftThenRight | Resolution::RightThenLeft => c.left.len() + c.right.len(),
        Resolution::Custom(v) => v.len(),
        Resolution::Unresolved => c.left.len() + c.base.len() + c.right.len() + 4,
    }
}

/// Which inputs a resolution takes its text from (Left, Base, Right).
fn sides_taken(res: &Resolution) -> [bool; 3] {
    match res {
        Resolution::Left => [true, false, false],
        Resolution::Base => [false, true, false],
        Resolution::Right => [false, false, true],
        Resolution::LeftThenRight | Resolution::RightThenLeft => [true, false, true],
        Resolution::Unresolved | Resolution::Custom(_) => [false, false, false],
    }
}

/// One row of the output pane.
#[derive(Debug, PartialEq, Eq)]
enum OutRow<'a> {
    /// The strip with resolution buttons above a non-Unchanged chunk.
    Header(usize),
    Line {
        chunk: usize,
        /// 1-based output line number.
        line_no: usize,
        text: Cow<'a, str>,
        /// A conflict marker line (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`).
        marker: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    Next,
    Prev,
    First,
    Last,
}

/// The next/previous/first/last non-Unchanged chunk (no wrap-around).
fn find_diff(chunks: &[MergeChunk], cur: Option<usize>, dir: Dir) -> Option<usize> {
    let is_diff = |c: &MergeChunk| c.kind != ChunkKind::Unchanged;
    match dir {
        Dir::First => chunks.iter().position(is_diff),
        Dir::Last => chunks.iter().rposition(is_diff),
        Dir::Next => {
            let from = cur.map_or(0, |c| c.saturating_add(1));
            chunks.iter().enumerate().skip(from).find(|(_, c)| is_diff(c)).map(|(i, _)| i)
        }
        Dir::Prev => {
            let to = cur.unwrap_or(chunks.len()).min(chunks.len());
            chunks.iter().enumerate().take(to).rev().find(|(_, c)| is_diff(c)).map(|(i, _)| i)
        }
    }
}

/// The next/previous conflict, preferring unresolved ones, wrapping around. When every
/// conflict is resolved, all conflicts are candidates.
fn find_conflict(chunks: &[MergeChunk], res: &[Resolution], cur: Option<usize>, forward: bool) -> Option<usize> {
    let conflicts: Vec<usize> = chunks.iter().enumerate().filter(|(_, c)| c.kind == ChunkKind::Conflict).map(|(i, _)| i).collect();
    let unresolved: Vec<usize> = conflicts.iter().copied().filter(|&i| res.get(i).is_some_and(|r| *r == Resolution::Unresolved)).collect();
    let pool = if unresolved.is_empty() { conflicts } else { unresolved };
    if forward {
        pool.iter().copied().find(|&i| cur.is_none_or(|c| i > c)).or_else(|| pool.first().copied())
    } else {
        pool.iter().copied().rev().find(|&i| cur.is_none_or(|c| i < c)).or_else(|| pool.last().copied())
    }
}

/// Digits of a line number (for gutter widths).
fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// The displayed part of a line: tabs expanded, `n` characters starting at character `start`.
fn visible_slice(s: &str, tab_width: usize, start: usize, n: usize) -> String {
    text::expand_tabs(s, tab_width).chars().skip(start).take(n).collect()
}

/// `<path>.bak`.
fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// File name of a path string (or the whole string).
fn short_name(p: &str) -> String {
    let p = p.trim();
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_owned())
}

/// Lines of hand-edited text (`\r\n` tolerated; no text means no lines).
fn text_to_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Read one input. An empty or nonexistent path is an empty file (git passes a missing BASE
/// for add/add conflicts); binary, oversized or unreadable files are errors.
fn load_input(path: &str, side: Side) -> Result<Input, String> {
    if path.trim().is_empty() {
        return Ok(Input::missing());
    }
    let p = widgets::clean_path(path);
    let meta = match std::fs::metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Input::missing()),
        Err(e) => return Err(format!("{} file {}: {e}", side.label(), p.display())),
    };
    if meta.is_dir() {
        return Err(format!("{} file {} is a folder. Text Merge needs three text files.", side.label(), p.display()));
    }
    if meta.len() > tfile::MAX_TEXT_BYTES {
        return Err(format!("{} file {} is too large to merge as text ({} bytes).", side.label(), p.display(), meta.len()));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("{} file {}: {e}", side.label(), p.display()))?;
    if tfile::is_probably_binary(&bytes) {
        return Err(format!("{} file {} looks like a binary file. Text Merge needs text files.", side.label(), p.display()));
    }
    Ok(Input { file: TextFile::from_bytes(&bytes), missing: false })
}

/// Write the merge result, keeping a `.bak` copy of an existing file first if asked.
fn write_output(path: &Path, file: &TextFile, backup: bool) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("No output file is set.".to_owned());
    }
    if path.is_dir() {
        return Err(format!("{} is a folder, not a file.", path.display()));
    }
    if backup && path.is_file() {
        let bak = backup_path(path);
        fsutil::copy_file(path, &bak).map_err(|e| format!("Could not back up {}: {e}", path.display()))?;
    }
    file.save(path).map_err(|e| format!("Could not save {}: {e}", path.display()))
}

impl MergeModel {
    fn new(left: Input, base: Input, right: Input, labels: MarkerLabels, opts: &TextCompareOptions, tab_width: usize) -> Result<Self, String> {
        let m = merge::merge3(&base.file.lines, &left.file.lines, &right.file.lines, opts).map_err(|e| format!("Merge failed: {e}"))?;
        let res = m.default_resolutions();
        let chunks = m.chunks;
        let (up_starts, up_total) = prefix_starts(chunks.iter().map(chunk_height));
        let max_chars = [&left, &base, &right]
            .iter()
            .flat_map(|i| i.file.lines.iter())
            .map(|l| if l.contains('\t') { text::expand_tabs(l, tab_width).chars().count() } else { l.chars().count() })
            .max()
            .unwrap_or(0);
        let (mut change_no, mut conflict_no) = (Vec::with_capacity(chunks.len()), Vec::with_capacity(chunks.len()));
        let (mut nc, mut nk) = (0usize, 0usize);
        for c in &chunks {
            if c.kind != ChunkKind::Unchanged {
                nc += 1;
            }
            if c.kind == ChunkKind::Conflict {
                nk += 1;
            }
            change_no.push(nc);
            conflict_no.push(nk);
        }
        let mut model = MergeModel {
            left,
            base,
            right,
            labels,
            chunks,
            saved: res.clone(),
            res,
            undo: Vec::new(),
            redo: Vec::new(),
            up_starts,
            up_total,
            out_starts: Vec::new(),
            out_line_starts: Vec::new(),
            out_total: 0,
            out_lines: 0,
            max_chars,
            change_no,
            conflict_no,
        };
        model.relayout_out();
        Ok(model)
    }

    fn lines(&self, side: Side) -> &[String] {
        &self.input(side).file.lines
    }

    fn input(&self, side: Side) -> &Input {
        match side {
            Side::Left => &self.left,
            Side::Base => &self.base,
            Side::Right => &self.right,
        }
    }

    fn res_of(&self, k: usize) -> &Resolution {
        self.res.get(k).unwrap_or(&FALLBACK_RESOLUTION)
    }

    fn kind_of(&self, k: usize) -> Option<ChunkKind> {
        self.chunks.get(k).map(|c| c.kind)
    }

    /// Non-Unchanged chunks get a header strip in the output and can be resolved.
    fn has_header(&self, k: usize) -> bool {
        self.kind_of(k).is_some_and(|kind| kind != ChunkKind::Unchanged)
    }

    fn out_len(&self, k: usize) -> usize {
        self.chunks.get(k).map_or(0, |c| rendered_len(c, self.res_of(k)))
    }

    fn relayout_out(&mut self) {
        let mut starts = Vec::with_capacity(self.chunks.len());
        let mut line_starts = Vec::with_capacity(self.chunks.len());
        let (mut row, mut line) = (0usize, 0usize);
        for k in 0..self.chunks.len() {
            starts.push(row);
            line_starts.push(line);
            let n = self.out_len(k);
            row = row.saturating_add(n).saturating_add(usize::from(self.has_header(k)));
            line = line.saturating_add(n);
        }
        self.out_starts = starts;
        self.out_line_starts = line_starts;
        self.out_total = row;
        self.out_lines = line;
    }

    /// Line `t` of chunk `k` in the output (consistent with `merge::render_chunk`).
    fn out_line(&self, k: usize, t: usize) -> Option<(Cow<'_, str>, bool)> {
        let c = self.chunks.get(k)?;
        let get = |side: Side, i: usize| -> Option<(Cow<'_, str>, bool)> {
            let r = side.range(c);
            if i >= r.len() {
                return None;
            }
            self.lines(side).get(r.start.checked_add(i)?).map(|s| (Cow::Borrowed(s.as_str()), false))
        };
        let (nl, nb, nr) = (c.left.len(), c.base.len(), c.right.len());
        match self.res_of(k) {
            Resolution::Base => get(Side::Base, t),
            Resolution::Left => get(Side::Left, t),
            Resolution::Right => get(Side::Right, t),
            Resolution::LeftThenRight => {
                if t < nl {
                    get(Side::Left, t)
                } else {
                    get(Side::Right, t - nl)
                }
            }
            Resolution::RightThenLeft => {
                if t < nr {
                    get(Side::Right, t)
                } else {
                    get(Side::Left, t - nr)
                }
            }
            Resolution::Custom(v) => v.get(t).map(|s| (Cow::Borrowed(s.as_str()), false)),
            Resolution::Unresolved => {
                let marker = |s: String| Some((Cow::Owned(s), true));
                if t == 0 {
                    marker(format!("<<<<<<< {}", self.labels.left))
                } else if t <= nl {
                    get(Side::Left, t - 1)
                } else if t == nl + 1 {
                    marker(format!("||||||| {}", self.labels.base))
                } else if t <= nl + 1 + nb {
                    get(Side::Base, t - nl - 2)
                } else if t == nl + nb + 2 {
                    marker("=======".to_owned())
                } else if t <= nl + nb + 2 + nr {
                    get(Side::Right, t - nl - nb - 3)
                } else if t == nl + nb + nr + 3 {
                    marker(format!(">>>>>>> {}", self.labels.right))
                } else {
                    None
                }
            }
        }
    }

    fn out_row(&self, row: usize) -> Option<OutRow<'_>> {
        let (k, t) = locate(&self.out_starts, self.out_total, row)?;
        let t = if self.has_header(k) {
            if t == 0 {
                return Some(OutRow::Header(k));
            }
            t - 1
        } else {
            t
        };
        let (text, marker) = self.out_line(k, t)?;
        let line_no = self.out_line_starts.get(k)?.saturating_add(t).saturating_add(1);
        Some(OutRow::Line { chunk: k, line_no, text, marker })
    }

    /// First output row of chunk `k` (its header, if any).
    fn out_row_of(&self, k: usize) -> Option<usize> {
        self.out_starts.get(k).copied()
    }

    /// The current output text of chunk `k` (to prefill the edit dialog).
    fn chunk_text(&self, k: usize) -> String {
        (0..self.out_len(k)).filter_map(|t| self.out_line(k, t).map(|(s, _)| s.into_owned())).collect::<Vec<_>>().join("\n")
    }

    /// Change the resolution of chunk `k`. Returns true if anything changed.
    fn set_res(&mut self, k: usize, new: Resolution) -> bool {
        let Some(slot) = self.res.get_mut(k) else { return false };
        if *slot == new {
            return false;
        }
        let old = std::mem::replace(slot, new);
        self.undo.push((k, old));
        self.redo.clear();
        self.relayout_out();
        true
    }

    /// Undo the last resolution change; returns the chunk it affected.
    fn undo(&mut self) -> Option<usize> {
        let (k, old) = self.undo.pop()?;
        let slot = self.res.get_mut(k)?;
        let cur = std::mem::replace(slot, old);
        self.redo.push((k, cur));
        self.relayout_out();
        Some(k)
    }

    /// Redo the last undone resolution change; returns the chunk it affected.
    fn redo(&mut self) -> Option<usize> {
        let (k, new) = self.redo.pop()?;
        let slot = self.res.get_mut(k)?;
        let cur = std::mem::replace(slot, new);
        self.undo.push((k, cur));
        self.relayout_out();
        Some(k)
    }

    fn is_dirty(&self) -> bool {
        self.res != self.saved
    }

    fn mark_saved(&mut self) {
        self.saved = self.res.clone();
    }

    fn conflicts(&self) -> usize {
        self.chunks.iter().filter(|c| c.kind == ChunkKind::Conflict).count()
    }

    fn unresolved(&self) -> usize {
        self.chunks.iter().zip(&self.res).filter(|(c, r)| c.kind == ChunkKind::Conflict && **r == Resolution::Unresolved).count()
    }

    fn changes(&self) -> usize {
        self.chunks.iter().filter(|c| c.kind != ChunkKind::Unchanged).count()
    }

    /// All output lines under the current resolutions.
    fn output_lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.out_lines);
        for (k, c) in self.chunks.iter().enumerate() {
            out.extend(merge::render_chunk(c, self.res_of(k), &self.base.file.lines, &self.left.file.lines, &self.right.file.lines, &self.labels));
        }
        out
    }

    /// The input whose encoding, BOM and line endings the output uses: the left file, or the
    /// first other input that exists and has content.
    fn template(&self) -> &TextFile {
        [&self.left, &self.right, &self.base].into_iter().find(|i| !i.missing && !i.file.lines.is_empty()).map_or(&self.left.file, |i| &i.file)
    }

    fn output_file(&self) -> TextFile {
        let tpl = self.template();
        TextFile {
            lines: self.output_lines(),
            encoding: tpl.encoding,
            bom: tpl.bom,
            line_ending: tpl.line_ending,
            final_newline: tpl.final_newline || tpl.lines.is_empty(),
            mixed_endings: false,
            had_decode_errors: false,
        }
    }

    fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        for side in Side::ALL {
            let i = self.input(side);
            if i.missing {
                w.push(format!("{} file is missing: treated as an empty file.", side.label()));
            }
            if i.file.had_decode_errors {
                w.push(format!("{} file has bytes that could not be decoded as {}; they are shown as \u{FFFD}.", side.label(), i.file.encoding.name()));
            }
        }
        w
    }
}

// ---------------------------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------------------------

/// A confirmation waiting for an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    SaveWithConflicts,
    Reload,
}

/// What a click in the output pane asked for.
enum OutAction {
    Select(usize),
    Set(usize, Resolution),
    Edit(usize),
}

/// Height of the pane title bars.
const TITLE_H: f32 = 22.0;

struct Metrics {
    font: FontId,
    row_h: f32,
    char_w: f32,
}

impl Metrics {
    fn get(ctx: &egui::Context) -> Self {
        let font = theme::mono(ctx);
        let (h, w) = ctx.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
        Metrics { font, row_h: (h + 4.0).max(18.0), char_w: w.max(1.0) }
    }
}

/// Horizontal geometry of one pane: gutter from `x0` to `text_x`, text up to `x1`.
#[derive(Clone, Copy, Debug)]
struct PaneCols {
    x0: f32,
    text_x: f32,
    x1: f32,
}

fn pane_columns(left: f32, width: f32, n: usize, gutter_w: f32, gap: f32) -> Vec<PaneCols> {
    let n_f = n.max(1) as f32;
    let w = ((width - gap * (n_f - 1.0)) / n_f).max(10.0);
    (0..n)
        .map(|i| {
            let x0 = left + i as f32 * (w + gap);
            PaneCols { x0, text_x: x0 + gutter_w, x1: x0 + w }
        })
        .collect()
}

/// Background of a present line in the three-pane area.
fn chunk_bg(pal: &Palette, kind: ChunkKind, side: Side) -> Color32 {
    match (kind, side) {
        (ChunkKind::Unchanged, _) => pal.pane_bg,
        (ChunkKind::Conflict, _) => pal.conflict_bg,
        (ChunkKind::LeftChange, Side::Left) => pal.left_only_bg,
        (ChunkKind::RightChange, Side::Right) => pal.right_only_bg,
        (ChunkKind::BothSame, Side::Left | Side::Right) => pal.changed_bg,
        _ => pal.pane_bg,
    }
}

/// Paint one line of text in a pane row, clipped to the pane, honouring the horizontal offset.
struct LinePainter<'a> {
    painter: &'a egui::Painter,
    font: &'a FontId,
    row_h: f32,
    tab: usize,
    start_char: usize,
    n_chars: usize,
    x_shift: f32,
}

impl LinePainter<'_> {
    fn text(&self, s: &str, text_x: f32, x1: f32, y: f32, color: Color32) {
        let vis = visible_slice(s, self.tab, self.start_char, self.n_chars);
        if vis.is_empty() {
            return;
        }
        let g = self.painter.layout_no_wrap(vis, self.font.clone(), color);
        let clip = Rect::from_min_max(pos2(text_x + 4.0, y), pos2(x1, y + self.row_h));
        self.painter.with_clip_rect(clip).galley(pos2(text_x + 6.0 + self.x_shift, y + (self.row_h - g.size().y) / 2.0), g, color);
    }

    fn gutter(&self, cols: PaneCols, y: f32, line_no: Option<usize>, pal: &Palette) {
        let g_rect = Rect::from_min_max(pos2(cols.x0, y), pos2(cols.text_x, y + self.row_h));
        self.painter.rect_filled(g_rect, 0.0, pal.gutter_bg);
        if let Some(n) = line_no {
            let g = self.painter.layout_no_wrap(n.to_string(), self.font.clone(), pal.gutter_text);
            self.painter.galley(pos2(cols.text_x - 5.0 - g.size().x, y + (self.row_h - g.size().y) / 2.0), g, pal.gutter_text);
        }
    }
}

pub struct MergeView {
    session: Session,
    left_text: String,
    base_text: String,
    right_text: String,
    output_text: String,
    job: Option<Job<MergeModel>>,
    model: Option<MergeModel>,
    error: Option<String>,
    /// The selected chunk.
    current: Option<usize>,
    hoff_up: f32,
    hoff_out: f32,
    scroll_up: Option<f32>,
    scroll_out: Option<f32>,
    /// Scroll both areas to this chunk on the next frame.
    scroll_chunk: Option<usize>,
    pending: Option<Pending>,
    /// Chunk being hand-edited and its text.
    edit: Option<(usize, String)>,
    tab_width: usize,
}

impl MergeView {
    pub fn from_session(s: &Session, settings: &Settings, ctx: &egui::Context) -> Self {
        let mut session = s.clone();
        session.kind = SessionKind::TextMerge;
        let mut v = MergeView {
            left_text: session.left.clone(),
            base_text: session.base.clone().unwrap_or_default(),
            right_text: session.right.clone(),
            output_text: session.output.clone().unwrap_or_default(),
            session,
            job: None,
            model: None,
            error: None,
            current: None,
            hoff_up: 0.0,
            hoff_out: 0.0,
            scroll_up: None,
            scroll_out: None,
            scroll_chunk: None,
            pending: None,
            edit: None,
            tab_width: settings.tab_width.clamp(1, 16),
        };
        v.reload(ctx);
        v
    }

    /// (Re)load the three inputs and merge them in the background.
    fn reload(&mut self, ctx: &egui::Context) {
        self.session.left = self.left_text.trim().to_owned();
        self.session.right = self.right_text.trim().to_owned();
        self.session.base = Some(self.base_text.trim().to_owned());
        self.session.output = Some(self.output_text.trim().to_owned()).filter(|s| !s.is_empty());
        self.model = None;
        self.error = None;
        self.current = None;
        self.edit = None;
        self.pending = None;
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if self.left_text.trim().is_empty() && self.right_text.trim().is_empty() && self.base_text.trim().is_empty() {
            return;
        }
        let (l, b, r) = (self.left_text.clone(), self.base_text.clone(), self.right_text.clone());
        let label = |p: &str, fallback: &str| Some(short_name(p)).filter(|s| !s.is_empty()).unwrap_or_else(|| fallback.to_owned());
        let labels = MarkerLabels { left: label(&l, "left"), base: label(&b, "base"), right: label(&r, "right") };
        let opts = self.session.text.clone();
        let tab = self.tab_width;
        self.job = Some(Job::spawn("Merging", ctx, move |p| {
            p.set_current(short_name(&l));
            let left = load_input(&l, Side::Left)?;
            let base = load_input(&b, Side::Base)?;
            let right = load_input(&r, Side::Right)?;
            if p.is_cancelled() {
                return Err("Cancelled.".into());
            }
            MergeModel::new(left, base, right, labels, &opts, tab)
        }));
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        let Some(result) = job.poll() else { return };
        self.job = None;
        match result {
            Ok(m) => {
                let first = find_conflict(&m.chunks, &m.res, None, true).or_else(|| find_diff(&m.chunks, None, Dir::First));
                self.model = Some(m);
                self.select(first);
            }
            Err(e) => {
                self.model = None;
                self.error = Some(e);
            }
        }
    }

    fn select(&mut self, k: Option<usize>) {
        if let Some(k) = k {
            self.current = Some(k);
            self.scroll_chunk = Some(k);
        }
    }

    /// The model, when loaded and no reload is running.
    fn loaded(&self) -> Option<&MergeModel> {
        if self.job.is_some() { None } else { self.model.as_ref() }
    }

    fn current_is_change(&self) -> bool {
        match (self.loaded(), self.current) {
            (Some(m), Some(k)) => m.has_header(k),
            _ => false,
        }
    }

    fn take(&mut self, r: Resolution) {
        let Some(k) = self.current else { return };
        if !self.current_is_change() {
            return;
        }
        if let Some(m) = &mut self.model
            && m.set_res(k, r)
        {
            self.scroll_chunk = Some(k);
        }
    }

    fn output_path(&self) -> Option<PathBuf> {
        let t = self.output_text.trim();
        if t.is_empty() { None } else { Some(widgets::clean_path(t)) }
    }

    /// Save interactively: ask for an output path if none, confirm if conflicts remain.
    fn request_save(&mut self, cx: &mut Cx<'_>) {
        let Some(m) = self.loaded() else { return };
        let unresolved = m.unresolved();
        if self.output_path().is_none() {
            let left = widgets::clean_path(&self.left_text);
            let start = left.parent().filter(|d| d.is_dir()).map(Path::to_path_buf);
            let name = left.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "merged.txt".to_owned());
            match widgets::save_file(start.as_deref(), &name, None) {
                Some(p) => {
                    self.output_text = p.display().to_string();
                    self.session.output = Some(self.output_text.clone());
                }
                None => return,
            }
        }
        if unresolved > 0 {
            self.pending = Some(Pending::SaveWithConflicts);
        } else {
            self.do_save(cx);
        }
    }

    /// Write the output now. Sets the process exit code: 0 when no conflicts remain, else 1.
    fn do_save(&mut self, cx: &mut Cx<'_>) -> bool {
        let Some(path) = self.output_path() else {
            cx.toasts.error("No output file is set: enter a path in the Output bar.");
            return false;
        };
        let backup = cx.settings.backup_on_save;
        let Some(m) = &mut self.model else { return false };
        let file = m.output_file();
        match write_output(&path, &file, backup) {
            Ok(()) => {
                m.mark_saved();
                self.session.output = Some(self.output_text.trim().to_owned());
                let u = m.unresolved();
                *cx.exit_code = if u == 0 { 0 } else { 1 };
                if u == 0 {
                    cx.toasts.success(format!("Saved {}", path.display()));
                } else {
                    cx.toasts.warning(format!("Saved {} with {u} unresolved conflict(s)", path.display()));
                }
                true
            }
            Err(e) => {
                cx.toasts.error(e);
                false
            }
        }
    }

    // --- drawing ------------------------------------------------------------------------------

    fn toolbar(&self, ui: &mut Ui, cx: &mut Cx<'_>) -> Option<Cmd> {
        let mut cmd = None;
        toolbar::bar(ui, "merge_toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            let mut b = |ui: &mut Ui, icon: Icon, label: &str, tip: &str, c: Cmd| {
                if toolbar::button(ui, icon, label, tip, self.supports(c)) {
                    cmd = Some(c);
                }
            };
            b(ui, Icon::Save, "Save", "Save the merge output (Ctrl+S)", Cmd::Save);
            toolbar::separator(ui);
            b(ui, Icon::PrevDiff, "Prev Conflict", "Go to the previous conflict (Ctrl+Shift+P)", Cmd::PrevConflict);
            b(ui, Icon::NextDiff, "Next Conflict", "Go to the next conflict (Ctrl+Shift+N)", Cmd::NextConflict);
            toolbar::separator(ui);
            b(ui, Icon::PrevDiff, "Prev Change", "Go to the previous change (Ctrl+P)", Cmd::PrevDiff);
            b(ui, Icon::NextDiff, "Next Change", "Go to the next change (Ctrl+N)", Cmd::NextDiff);
            toolbar::separator(ui);
            b(ui, Icon::CopyRight, "Take Left", "Take the left text for the current change (Alt+1)", Cmd::TakeLeft);
            b(ui, Icon::Merge, "Take Base", "Take the base text for the current change (Alt+2)", Cmd::TakeBase);
            b(ui, Icon::CopyLeft, "Take Right", "Take the right text for the current change (Alt+3)", Cmd::TakeRight);
            b(ui, Icon::ShowAll, "Take Both", "Take the left then the right text for the current change (Alt+4)", Cmd::TakeBoth);
            toolbar::separator(ui);
            b(ui, Icon::Undo, "Undo", "Undo the last resolution change (Ctrl+Z)", Cmd::Undo);
            b(ui, Icon::Redo, "Redo", "Redo the last undone resolution change (Ctrl+Shift+Z)", Cmd::Redo);
            toolbar::separator(ui);
            b(ui, Icon::Reload, "Reload", "Reload the input files (F5)", Cmd::Refresh);
        });
        cmd
    }

    /// Path rows (Left | Base | Right, their file info, and the output). Returns (an input path
    /// was submitted, the output save icon was clicked).
    fn path_bars(&mut self, ui: &mut Ui, pal: &Palette) -> (bool, bool) {
        let mut submit = false;
        let gap = 8.0;
        // Roughly aligned with the panes below (they leave 12 px for the overview strip).
        let col_w = |ui: &Ui| ((ui.available_width() - 6.0 - 2.0 * gap) / 3.0).max(60.0);
        let (l, b, r) = (&mut self.left_text, &mut self.base_text, &mut self.right_text);
        toolbar::header_strip(ui, 24.0, |ui| {
            let w = col_w(ui);
            let fields: [(Side, &str, &mut String); 3] = [(Side::Left, "merge_left", l), (Side::Base, "merge_base", b), (Side::Right, "merge_right", r)];
            for (i, (side, id, text)) in fields.into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(gap);
                }
                ui.allocate_ui_with_layout(vec2(w, 24.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add_sized(vec2(34.0, 18.0), egui::Label::new(RichText::new(side.label()).strong()));
                    submit |= widgets::path_bar(ui, id, text, false, &format!("{} file", side.label())).submitted;
                });
            }
        });
        // File info per input.
        let infos: Vec<(String, bool)> = match self.loaded() {
            Some(m) => Side::ALL.iter().map(|s| info_text(m.input(*s))).collect(),
            None => Vec::new(),
        };
        toolbar::header_strip(ui, 16.0, |ui| {
            let w = col_w(ui);
            for i in 0..3 {
                if i > 0 {
                    ui.add_space(gap);
                }
                ui.allocate_ui_with_layout(vec2(w, 16.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add_space(38.0);
                    if let Some((t, warn)) = infos.get(i) {
                        let c = if *warn { pal.warning } else { pal.dim_text };
                        ui.add(egui::Label::new(RichText::new(t).small().color(c)).truncate());
                    }
                });
            }
        });
        let mut save = false;
        let save_enabled = self.supports(Cmd::Save);
        toolbar::header_strip(ui, 24.0, |ui| {
            ui.add_sized(vec2(50.0, 18.0), egui::Label::new(RichText::new("Output").strong()));
            let w = (ui.available_width() - 30.0).max(60.0);
            ui.allocate_ui_with_layout(vec2(w, 24.0), Layout::left_to_right(Align::Center), |ui| {
                if widgets::path_bar(ui, "merge_output", &mut self.output_text, false, "Ask where to save").submitted {
                    self.session.output = Some(self.output_text.trim().to_owned()).filter(|s| !s.is_empty());
                }
            });
            save = toolbar::small(ui, Icon::Save, "Save the merge output (Ctrl+S)", save_enabled);
        });
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.border));
        (submit, save)
    }

    fn status_bar(&self, ui: &mut Ui, pal: &Palette) {
        toolbar::status_bar(ui, "merge_status", |ui| {
            let Some(m) = self.loaded() else {
                let t = if self.job.is_some() { "Merging…" } else { "No files loaded" };
                ui.label(RichText::new(t).color(pal.dim_text));
                return;
            };
            let (n, u) = (m.conflicts(), m.unresolved());
            toolbar::status_icon(ui, if u > 0 { Icon::Conflict } else { Icon::Check });
            let color = if u > 0 { pal.conflict_strong } else { pal.success };
            ui.colored_label(color, format!("{n} conflict{} ({u} unresolved)", if n == 1 { "" } else { "s" }));
            toolbar::status_sep(ui);
            let changes = m.changes();
            ui.label(format!("{changes} change{} in {} chunks", if changes == 1 { "" } else { "s" }, m.chunks.len()));
            if let Some(k) = self.current
                && m.has_header(k)
                && let Some(no) = m.change_no.get(k)
            {
                toolbar::status_sep(ui);
                ui.label(format!("Change {no} of {changes}"));
            }
            toolbar::status_sep(ui);
            let t = m.template();
            ui.label(format!("Output: {} lines   {}{}   {}", m.out_lines, t.encoding.name(), if t.bom { " BOM" } else { "" }, t.line_ending.label()));
            if m.is_dirty() {
                toolbar::status_sep(ui);
                ui.colored_label(pal.warning, "Modified");
            }
        });
    }

    /// The three aligned panes Left | Base | Right.
    fn upper_ui(&mut self, ui: &mut Ui, pal: &Palette, mt: &Metrics, show_ln: bool) {
        let Some(m) = &self.model else { return };
        let full = ui.available_rect_before_wrap();
        let (strip_w, hbar_h) = (12.0, 10.0);
        let header_h = TITLE_H;
        let area = Rect::from_min_max(pos2(full.left(), full.top() + header_h), pos2(full.right() - strip_w, full.bottom() - hbar_h));
        if area.height() < mt.row_h || area.width() < 60.0 {
            return;
        }
        let strip = Rect::from_min_max(pos2(area.right(), area.top()), pos2(full.right(), area.bottom()));
        let hbar_rect = Rect::from_min_max(pos2(area.left(), area.bottom()), pos2(area.right(), full.bottom()));
        let max_lines = Side::ALL.iter().map(|s| m.lines(*s).len()).max().unwrap_or(0);
        let gutter_w = if show_ln { digits(max_lines) as f32 * mt.char_w + 10.0 } else { 0.0 };
        let cols = pane_columns(area.left(), area.width(), 3, gutter_w, 1.0);
        let text_w = cols.first().map_or(0.0, |c| c.x1 - c.text_x - 6.0).max(1.0);

        // Pane title bars, and the thin borders between the panes.
        {
            let painter = ui.painter();
            let title_rect = Rect::from_min_max(full.min, pos2(full.right(), full.top() + header_h));
            painter.rect_filled(title_rect, 0.0, pal.header_bg);
            painter.hline(full.x_range(), title_rect.bottom() - 0.5, Stroke::new(1.0, pal.border));
            for (side, c) in Side::ALL.iter().zip(&cols) {
                let r = Rect::from_min_max(pos2(c.x0, full.top()), pos2(c.x1, title_rect.bottom() - 1.0));
                let name = match side {
                    Side::Left => &self.left_text,
                    Side::Base => &self.base_text,
                    Side::Right => &self.right_text,
                };
                let name = short_name(name);
                let input = m.input(*side);
                let mut job = egui::text::LayoutJob::default();
                let font = FontId::proportional(12.5);
                let fmt = |color: Color32| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
                job.append(&format!("{}: ", side.label()), 0.0, fmt(pal.dim_text));
                job.append(if name.is_empty() { "(none)" } else { &name }, 0.0, fmt(pal.text));
                if input.missing {
                    job.append("  (missing)", 0.0, fmt(pal.warning));
                }
                let g = painter.layout_job(job);
                let icon_r = Rect::from_center_size(pos2(r.left() + 13.0, r.center().y), vec2(14.0, 14.0));
                icons::paint(painter, icon_r, Icon::File, pal, !input.missing);
                painter.with_clip_rect(r).galley(pos2(r.left() + 25.0, r.center().y - g.size().y / 2.0), g, pal.text);
            }
            let border = Stroke::new(1.0, pal.border);
            for c in cols.iter().take(2) {
                painter.vline(c.x1 + 0.5, full.top()..=area.bottom(), border);
            }
            painter.vline(area.right() + 0.5, full.top()..=area.bottom(), border);
        }

        // Shift+wheel / horizontal trackpad scroll (read before the scroll area consumes it).
        if ui.rect_contains_pointer(area) {
            let dx = ui.input(|i| i.smooth_scroll_delta.x);
            if dx != 0.0 {
                self.hoff_up = (self.hoff_up - dx).max(0.0);
            }
        }
        let content_w = m.max_chars as f32 * mt.char_w + 20.0;
        self.hoff_up = widgets::hscrollbar(ui, hbar_rect, self.hoff_up, content_w, text_w, pal);

        let row_h = mt.row_h;
        let total = m.up_total;
        if let Some(k) = self.scroll_chunk
            && let Some(&start) = m.up_starts.get(k)
        {
            self.scroll_up = Some((start as f32 - 3.0).max(0.0) * row_h);
        }
        let mut sa = egui::ScrollArea::vertical().id_salt("merge_upper").auto_shrink([false, false]);
        if let Some(off) = self.scroll_up.take() {
            sa = sa.vertical_scroll_offset(off.max(0.0));
        }
        let hoff = self.hoff_up;
        let start_char = (hoff / mt.char_w).max(0.0) as usize;
        let x_shift = start_char as f32 * mt.char_w - hoff;
        let n_chars = (text_w / mt.char_w) as usize + 2;
        let current = self.current;
        let tab = self.tab_width;
        let mut clicked: Option<usize> = None;
        let out = ui
            .scope_builder(UiBuilder::new().max_rect(area), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                sa.show_rows(ui, row_h, total, |ui, rows| {
                    let top = ui.max_rect().top();
                    let rows_rect = Rect::from_min_max(pos2(area.left(), top), pos2(area.right(), top + rows.len() as f32 * row_h));
                    let resp = ui.interact(rows_rect, ui.id().with("merge_upper_click"), Sense::click());
                    let painter = ui.painter();
                    let lp = LinePainter { painter, font: &mt.font, row_h, tab, start_char, n_chars, x_shift };
                    for row in rows.clone() {
                        let y = top + (row - rows.start) as f32 * row_h;
                        let Some((k, t)) = locate(&m.up_starts, m.up_total, row) else { continue };
                        let Some(c) = m.chunks.get(k) else { continue };
                        let res = m.res_of(k);
                        let taken = sides_taken(res);
                        let unresolved = c.kind == ChunkKind::Conflict && *res == Resolution::Unresolved;
                        for ((side, col), side_taken) in Side::ALL.iter().zip(&cols).zip(taken) {
                            let range = side.range(c);
                            let line = (t < range.len()).then(|| range.start + t);
                            let text_rect = Rect::from_min_max(pos2(col.text_x, y), pos2(col.x1, y + row_h));
                            let bg = match line {
                                Some(_) => Some(chunk_bg(pal, c.kind, *side)),
                                None if c.kind == ChunkKind::Unchanged => Some(pal.pane_bg),
                                None => None,
                            };
                            match bg {
                                Some(bg) => {
                                    painter.rect_filled(text_rect, 0.0, bg);
                                }
                                None => pal.hatch_rect(painter, text_rect),
                            }
                            let fg = if c.kind == ChunkKind::Conflict || bg.is_some_and(|b| b != pal.pane_bg) { pal.changed_text } else { pal.text };
                            if show_ln {
                                lp.gutter(*col, y, line.map(|i| i + 1), pal);
                            }
                            let edge = if unresolved {
                                Some(pal.conflict_strong)
                            } else if c.kind != ChunkKind::Unchanged && side_taken {
                                Some(pal.accent)
                            } else {
                                None
                            };
                            if let Some(e) = edge {
                                painter.rect_filled(Rect::from_min_size(pos2(col.text_x, y), vec2(3.0, row_h)), 0.0, e);
                            }
                            if let Some(i) = line
                                && let Some(s) = m.lines(*side).get(i)
                            {
                                lp.text(s, col.text_x, col.x1, y, fg);
                            }
                        }
                    }
                    // Current chunk outline.
                    if let Some(k) = current
                        && let Some(c) = m.chunks.get(k)
                        && let Some(&start) = m.up_starts.get(k)
                    {
                        let h = chunk_height(c);
                        if h > 0 && start < rows.end && start + h > rows.start {
                            let y0 = top + (start as f32 - rows.start as f32) * row_h;
                            let r = Rect::from_min_max(pos2(area.left() + 1.0, y0), pos2(area.right() - 1.0, y0 + h as f32 * row_h));
                            painter.rect_stroke(r, 2.0, Stroke::new(1.5, pal.current_outline), StrokeKind::Inside);
                        }
                    }
                    if resp.clicked()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let row = rows.start + ((p.y - top) / row_h).max(0.0) as usize;
                        clicked = locate(&m.up_starts, m.up_total, row).map(|(k, _)| k);
                    }
                })
            })
            .inner;

        // Overview strip: conflicts strong, resolved conflicts green, other changes amber.
        let total_f = total.max(1) as f32;
        let marks: Vec<OverviewMark> = m
            .chunks
            .iter()
            .zip(&m.up_starts)
            .enumerate()
            .filter(|(_, (c, _))| c.kind != ChunkKind::Unchanged)
            .map(|(k, (c, &s))| {
                let color = match (c.kind, m.res_of(k)) {
                    (ChunkKind::Conflict, Resolution::Unresolved) => pal.conflict_strong,
                    (ChunkKind::Conflict, _) => pal.success,
                    _ => pal.changed_strong,
                };
                OverviewMark { start: s as f32 / total_f, end: (s + chunk_height(c)) as f32 / total_f, color }
            })
            .collect();
        let content_h = out.content_size.y.max(1.0);
        let view = (out.state.offset.y / content_h, (out.state.offset.y + out.inner_rect.height()) / content_h);
        if let Some(f) = widgets::overview_strip(ui, strip, &marks, view, pal) {
            self.scroll_up = Some((f * content_h - out.inner_rect.height() / 2.0).max(0.0));
        }
        if let Some(k) = clicked {
            self.current = Some(k);
            self.scroll_out = m.out_row_of(k).map(|r| (r as f32 - 2.0).max(0.0) * row_h);
        }
        ui.allocate_rect(full, Sense::hover());
    }

    /// The output pane: the merge result with a header strip per change.
    fn output_ui(&mut self, ui: &mut Ui, pal: &Palette, mt: &Metrics, show_ln: bool) {
        let Some(m) = &self.model else { return };
        let full = ui.available_rect_before_wrap();
        let hbar_h = 10.0;
        let area = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - hbar_h));
        if area.height() < mt.row_h || area.width() < 60.0 {
            return;
        }
        let hbar_rect = Rect::from_min_max(pos2(area.left(), area.bottom()), full.max);
        let gutter_w = if show_ln { digits(m.out_lines) as f32 * mt.char_w + 10.0 } else { 0.0 };
        // Leave room on the right for the vertical scrollbar.
        let col = PaneCols { x0: area.left(), text_x: area.left() + gutter_w, x1: area.right() - 14.0 };
        let text_w = (col.x1 - col.text_x - 6.0).max(1.0);

        if ui.rect_contains_pointer(area) {
            let dx = ui.input(|i| i.smooth_scroll_delta.x);
            if dx != 0.0 {
                self.hoff_out = (self.hoff_out - dx).max(0.0);
            }
        }
        let content_w = m.max_chars as f32 * mt.char_w + 40.0;
        self.hoff_out = widgets::hscrollbar(ui, hbar_rect, self.hoff_out, content_w, text_w, pal);

        let row_h = mt.row_h;
        if let Some(k) = self.scroll_chunk
            && let Some(r) = m.out_row_of(k)
        {
            self.scroll_out = Some((r as f32 - 2.0).max(0.0) * row_h);
        }
        let mut sa = egui::ScrollArea::vertical().id_salt("merge_output").auto_shrink([false, false]);
        if let Some(off) = self.scroll_out.take() {
            sa = sa.vertical_scroll_offset(off.max(0.0));
        }
        let hoff = self.hoff_out;
        let start_char = (hoff / mt.char_w).max(0.0) as usize;
        let x_shift = start_char as f32 * mt.char_w - hoff;
        let n_chars = (text_w / mt.char_w) as usize + 2;
        let current = self.current;
        let tab = self.tab_width;
        let total = m.out_total;
        let mut action: Option<OutAction> = None;
        ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            sa.show_rows(ui, row_h, total, |ui, rows| {
                let top = ui.max_rect().top();
                let rows_rect = Rect::from_min_max(pos2(area.left(), top), pos2(area.right(), top + rows.len() as f32 * row_h));
                let resp = ui.interact(rows_rect, ui.id().with("merge_out_click"), Sense::click());
                let mut headers: Vec<(usize, f32)> = Vec::new();
                {
                    let painter = ui.painter();
                    let lp = LinePainter { painter, font: &mt.font, row_h, tab, start_char, n_chars, x_shift };
                    for row in rows.clone() {
                        let y = top + (row - rows.start) as f32 * row_h;
                        let Some(r) = m.out_row(row) else { continue };
                        let OutRow::Line { chunk, line_no, text, marker } = r else {
                            if let OutRow::Header(k) = r {
                                headers.push((k, y));
                            }
                            continue;
                        };
                        let kind = m.kind_of(chunk).unwrap_or(ChunkKind::Unchanged);
                        let unresolved = kind == ChunkKind::Conflict && *m.res_of(chunk) == Resolution::Unresolved;
                        let (bg, edge) = match kind {
                            ChunkKind::Unchanged => (pal.pane_bg, None),
                            ChunkKind::Conflict if unresolved => (pal.conflict_bg, Some(pal.conflict_strong)),
                            ChunkKind::Conflict => (pal.resolved_bg, Some(pal.success)),
                            _ => (pal.changed_bg, Some(pal.changed_strong)),
                        };
                        painter.rect_filled(Rect::from_min_max(pos2(col.text_x, y), pos2(area.right(), y + row_h)), 0.0, bg);
                        if show_ln {
                            lp.gutter(col, y, Some(line_no), pal);
                        }
                        if let Some(e) = edge {
                            painter.rect_filled(Rect::from_min_size(pos2(col.text_x, y), vec2(3.0, row_h)), 0.0, e);
                        }
                        lp.text(&text, col.text_x, area.right(), y, if marker { pal.conflict_strong } else { pal.text });
                    }
                    if let Some(k) = current
                        && let Some(&start) = m.out_starts.get(k)
                    {
                        let h = m.out_len(k) + usize::from(m.has_header(k));
                        if h > 0 && start < rows.end && start + h > rows.start {
                            let y0 = top + (start as f32 - rows.start as f32) * row_h;
                            let r = Rect::from_min_max(pos2(area.left() + 1.0, y0), pos2(area.right() - 1.0, y0 + h as f32 * row_h));
                            painter.rect_stroke(r, 2.0, Stroke::new(1.5, pal.current_outline), StrokeKind::Inside);
                        }
                    }
                }
                if let Some(p) = resp.interact_pointer_pos()
                    && (resp.clicked() || resp.double_clicked())
                {
                    let row = rows.start + ((p.y - top) / row_h).max(0.0) as usize;
                    if let Some((k, _)) = locate(&m.out_starts, m.out_total, row) {
                        action = Some(if resp.double_clicked() && m.has_header(k) { OutAction::Edit(k) } else { OutAction::Select(k) });
                    }
                }
                // Header strips with resolution buttons (added after the click area so they win).
                for (k, y) in headers {
                    let rect = Rect::from_min_max(pos2(area.left(), y), pos2(area.right(), y + row_h));
                    if let Some(a) = header_strip(ui, m, k, rect, pal, current == Some(k)) {
                        action = Some(a);
                    }
                }
            });
        });
        match action {
            Some(OutAction::Select(k)) => {
                self.current = Some(k);
                self.scroll_up = m.up_starts.get(k).map(|&s| (s as f32 - 3.0).max(0.0) * row_h);
            }
            Some(OutAction::Edit(k)) => {
                self.current = Some(k);
                self.edit = Some((k, m.chunk_text(k)));
            }
            Some(OutAction::Set(k, r)) => {
                self.current = Some(k);
                if let Some(m) = &mut self.model {
                    m.set_res(k, r);
                }
            }
            None => {}
        }
        ui.allocate_rect(full, Sense::hover());
    }

    fn modals(&mut self, ctx: &egui::Context, cx: &mut Cx<'_>) {
        match self.pending {
            Some(Pending::SaveWithConflicts) => {
                let n = self.model.as_ref().map_or(0, MergeModel::unresolved);
                let body = if n == 1 {
                    "1 conflict is still unresolved. It will be written with conflict markers (<<<<<<< ======= >>>>>>>) and the merge will not count as resolved.".to_owned()
                } else {
                    format!(
                        "{n} conflicts are still unresolved. They will be written with conflict markers (<<<<<<< ======= >>>>>>>) and the merge will not count as resolved."
                    )
                };
                match widgets::confirm_modal(ctx, "merge_save_conflicts", "Save with conflict markers?", &body, "Save anyway") {
                    Some(true) => {
                        self.pending = None;
                        self.do_save(cx);
                    }
                    Some(false) => self.pending = None,
                    None => {}
                }
            }
            Some(Pending::Reload) => {
                let body = "Reloading the input files discards the resolutions made since the last save.";
                match widgets::confirm_modal(ctx, "merge_reload", "Discard resolutions?", body, "Reload") {
                    Some(true) => {
                        self.pending = None;
                        self.reload(ctx);
                    }
                    Some(false) => self.pending = None,
                    None => {}
                }
            }
            None => {}
        }

        let mut result: Option<bool> = None;
        if let Some((k, text)) = &mut self.edit {
            let no = self.model.as_ref().and_then(|m| m.change_no.get(*k)).copied().unwrap_or(0);
            let resp = egui::Modal::new(egui::Id::new("merge_edit_chunk")).show(ctx, |ui| {
                ui.set_width(640.0);
                ui.heading(format!("Edit merged text (change {no})"));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(text).code_editor().desired_rows(12).desired_width(f32::INFINITY));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        result = Some(false);
                    }
                });
            });
            if resp.should_close() && result.is_none() {
                result = Some(false);
            }
        }
        match result {
            Some(true) => {
                if let Some((k, text)) = self.edit.take()
                    && let Some(m) = &mut self.model
                {
                    m.set_res(k, Resolution::Custom(text_to_lines(&text)));
                    self.current = Some(k);
                }
            }
            Some(false) => self.edit = None,
            None => {}
        }
    }
}

/// The info line under an input's path: encoding, line endings and line count. The flag is
/// true when it is a warning (missing file).
fn info_text(i: &Input) -> (String, bool) {
    if i.missing {
        return ("Missing: treated as an empty file".to_owned(), true);
    }
    let f = &i.file;
    let eol = if f.mixed_endings { "Mixed line endings" } else { f.line_ending.label() };
    let n = f.lines.len();
    (
        format!(
            "{}{}      {eol}      {} line{}",
            f.encoding.name(),
            if f.bom { " BOM" } else { "" },
            widgets::thousands(n as u64),
            if n == 1 { "" } else { "s" }
        ),
        false,
    )
}

/// The strip above a chunk in the output pane: what happened, plus resolution buttons.
fn header_strip(ui: &mut Ui, m: &MergeModel, k: usize, rect: Rect, pal: &Palette, is_current: bool) -> Option<OutAction> {
    let kind = m.kind_of(k)?;
    let res = m.res_of(k);
    let unresolved = kind == ChunkKind::Conflict && *res == Resolution::Unresolved;
    let bg = if unresolved {
        pal.conflict_bg
    } else if kind == ChunkKind::Conflict {
        pal.resolved_bg
    } else {
        pal.header_bg
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    let line = Stroke::new(1.0, pal.border);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, line);
    let mut action = None;
    let mut child =
        ui.new_child(UiBuilder::new().id_salt(("merge_hdr", k)).max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    let clip = rect.intersect(ui.clip_rect());
    child.set_clip_rect(clip);
    child.spacing_mut().item_spacing.x = 2.0;
    child.spacing_mut().interact_size.y = (rect.height() - 2.0).max(8.0);
    // A conflict marker (unresolved), a check (resolved conflict), or nothing.
    let (marker_rect, _) = child.allocate_exact_size(vec2(16.0, rect.height()), Sense::hover());
    let marker = if unresolved {
        Some(Icon::Conflict)
    } else if kind == ChunkKind::Conflict {
        Some(Icon::Check)
    } else {
        None
    };
    if let Some(icon) = marker {
        icons::paint(child.painter(), Rect::from_center_size(marker_rect.center(), vec2(14.0, 14.0)), icon, pal, true);
    }
    child.add_space(2.0);
    let what = match kind {
        ChunkKind::Conflict => format!("Conflict {} of {}", m.conflict_no.get(k).copied().unwrap_or(0), m.conflicts()),
        ChunkKind::LeftChange => "Changed on the left".to_owned(),
        ChunkKind::RightChange => "Changed on the right".to_owned(),
        ChunkKind::BothSame => "Same change on both sides".to_owned(),
        ChunkKind::Unchanged => "Unchanged".to_owned(),
    };
    let color = if unresolved { pal.conflict_strong } else { pal.dim_text };
    let label = RichText::new(format!("{what}: {}", res.label())).small().color(color);
    child.add(egui::Label::new(if is_current { label.strong() } else { label }).selectable(false));
    child.add_space(10.0);
    let choices = [
        (Icon::CopyRight, "Left", "Take the left text", Resolution::Left),
        (Icon::Merge, "Base", "Take the base text", Resolution::Base),
        (Icon::CopyLeft, "Right", "Take the right text", Resolution::Right),
        (Icon::ShowAll, "Both", "Take the left text, then the right text", Resolution::LeftThenRight),
        (Icon::Swap, "Both (R+L)", "Take the right text, then the left text", Resolution::RightThenLeft),
    ];
    for (icon, text, tip, r) in choices {
        let selected = *res == r;
        if chip(&mut child, pal, clip, icon, text, tip, selected) && !selected {
            action = Some(OutAction::Set(k, r));
        }
    }
    let custom = matches!(res, Resolution::Custom(_));
    if chip(&mut child, pal, clip, Icon::Edit, "Edit", "Edit this chunk's output by hand (or double-click it)", custom) {
        action = Some(OutAction::Edit(k));
    }
    action
}

/// A small icon button with a short label, highlighted when `selected`. Returns true when clicked.
fn chip(ui: &mut Ui, pal: &Palette, clip: Rect, icon: Icon, label: &str, tip: &str, selected: bool) -> bool {
    let bg = ui.painter().add(egui::Shape::Noop);
    let icon_resp = toolbar::small_resp(ui, icon, tip, true);
    ui.add_space(-4.0);
    let color = if selected { pal.text } else { pal.dim_text };
    let text_resp = ui.add(egui::Label::new(RichText::new(label).small().color(color)).selectable(false).sense(Sense::click())).on_hover_text(tip);
    ui.add_space(6.0);
    let r = icon_resp.rect.union(text_resp.rect).expand2(vec2(2.0, 0.0)).intersect(clip.shrink2(vec2(0.0, 1.0)));
    if selected {
        ui.painter().set(bg, egui::Shape::rect_filled(r, 3.0, pal.active_bg));
        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, pal.accent.gamma_multiply(0.6)), StrokeKind::Inside);
    } else if text_resp.hovered() {
        ui.painter().set(bg, egui::Shape::rect_filled(r, 3.0, pal.hover_bg));
    }
    icon_resp.clicked() || text_resp.clicked()
}

impl View for MergeView {
    fn kind(&self) -> SessionKind {
        SessionKind::TextMerge
    }

    fn title(&self) -> String {
        let name = if self.output_text.trim().is_empty() { short_name(&self.left_text) } else { short_name(&self.output_text) };
        if name.is_empty() { "Merge".to_owned() } else { format!("Merge: {name}") }
    }

    fn tooltip(&self) -> String {
        let out = self.output_text.trim();
        format!(
            "Left: {}\nBase: {}\nRight: {}\nOutput: {}",
            self.left_text.trim(),
            self.base_text.trim(),
            self.right_text.trim(),
            if out.is_empty() { "(ask on save)" } else { out }
        )
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let pal = Palette::get(&ctx);
        self.tab_width = cx.settings.tab_width.clamp(1, 16);
        self.poll_job();

        if let Some(cmd) = self.toolbar(ui, cx) {
            self.run(cmd, &ctx, cx);
        }
        self.status_bar(ui, &pal);
        let (submit, save) = self.path_bars(ui, &pal);
        if save {
            self.run(Cmd::Save, &ctx, cx);
        }
        if submit {
            if self.model.as_ref().is_some_and(MergeModel::is_dirty) {
                self.pending = Some(Pending::Reload);
            } else {
                self.reload(&ctx);
            }
        }
        let pad = egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 6));
        if let Some(job) = &self.job {
            let cancelled = pad.show(ui, |ui| jobs::progress_ui(ui, job)).inner;
            if cancelled {
                self.job = None;
                self.error = Some("Loading was cancelled.".to_owned());
            }
            self.modals(&ctx, cx);
            return;
        }
        if let Some(e) = &self.error {
            pad.show(ui, |ui| {
                ui.horizontal(|ui| {
                    toolbar::status_icon(ui, Icon::Close);
                    ui.colored_label(pal.error, e);
                });
            });
        }
        let Some(m) = &self.model else {
            if self.error.is_none() {
                widgets::empty_message(ui, "Choose the Left, Base and Right files, then press Enter.");
            }
            self.modals(&ctx, cx);
            return;
        };
        let warnings = m.warnings();
        if !warnings.is_empty() {
            toolbar::header_strip(ui, 18.0 * warnings.len() as f32, |ui| {
                ui.vertical(|ui| {
                    for w in &warnings {
                        ui.horizontal(|ui| {
                            toolbar::status_icon(ui, Icon::Conflict);
                            ui.label(RichText::new(w).small().color(pal.warning));
                        });
                    }
                });
            });
        }
        let (conflicts, unresolved) = (m.conflicts(), m.unresolved());
        let out_name = short_name(&self.output_text);

        let mt = Metrics::get(&ctx);
        let show_ln = cx.settings.show_line_numbers;
        let avail = ui.available_height();
        egui::Panel::bottom(ui.id().with("merge_output_panel"))
            .resizable(true)
            .default_size((avail * 0.45).max(80.0))
            .size_range(60.0..=(avail - 80.0).max(80.0))
            .frame(egui::Frame::NONE.fill(pal.pane_bg))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let top = ui.max_rect().top();
                ui.painter().hline(ui.max_rect().x_range(), top + 0.5, Stroke::new(1.0, pal.border));
                toolbar::header_strip(ui, TITLE_H - 2.0, |ui| {
                    toolbar::status_icon(ui, Icon::Merge);
                    ui.label(RichText::new("Output").strong());
                    if !out_name.is_empty() {
                        ui.label(RichText::new(format!(": {out_name}")).color(pal.dim_text));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(4.0);
                        if conflicts == 0 {
                            ui.label(RichText::new("No conflicts").small().color(pal.dim_text));
                        } else if unresolved == 0 {
                            ui.label(RichText::new(format!("All {conflicts} conflicts resolved")).small().color(pal.success));
                            toolbar::status_icon(ui, Icon::Check);
                        } else {
                            ui.label(RichText::new(format!("{unresolved} of {conflicts} conflicts unresolved")).small().color(pal.conflict_strong));
                            toolbar::status_icon(ui, Icon::Conflict);
                        }
                    });
                });
                let r = ui.cursor();
                ui.painter().hline(ui.max_rect().x_range(), r.top() - 0.5, Stroke::new(1.0, pal.border));
                self.output_ui(ui, &pal, &mt, show_ln);
            });
        egui::CentralPanel::no_frame().show(ui, |ui| self.upper_ui(ui, &pal, &mt, show_ln));
        self.scroll_chunk = None;
        self.modals(&ctx, cx);
    }

    fn supports(&self, cmd: Cmd) -> bool {
        let Some(m) = self.loaded() else { return cmd == Cmd::Refresh && self.job.is_none() };
        match cmd {
            Cmd::Save | Cmd::Refresh => true,
            Cmd::NextDiff => find_diff(&m.chunks, self.current, Dir::Next).is_some(),
            Cmd::PrevDiff => find_diff(&m.chunks, self.current, Dir::Prev).is_some(),
            Cmd::FirstDiff | Cmd::LastDiff => m.changes() > 0,
            Cmd::NextConflict | Cmd::PrevConflict => m.conflicts() > 0,
            Cmd::TakeLeft | Cmd::TakeRight | Cmd::TakeBase | Cmd::TakeBoth => self.current_is_change(),
            Cmd::Undo => !m.undo.is_empty(),
            Cmd::Redo => !m.redo.is_empty(),
            _ => false,
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if !self.supports(cmd) {
            return;
        }
        let cur = self.current;
        let nav = |m: &MergeModel| -> Option<usize> {
            match cmd {
                Cmd::NextDiff => find_diff(&m.chunks, cur, Dir::Next),
                Cmd::PrevDiff => find_diff(&m.chunks, cur, Dir::Prev),
                Cmd::FirstDiff => find_diff(&m.chunks, cur, Dir::First),
                Cmd::LastDiff => find_diff(&m.chunks, cur, Dir::Last),
                Cmd::NextConflict => find_conflict(&m.chunks, &m.res, cur, true),
                Cmd::PrevConflict => find_conflict(&m.chunks, &m.res, cur, false),
                _ => None,
            }
        };
        match cmd {
            Cmd::Save => self.request_save(cx),
            Cmd::Refresh => {
                if self.is_dirty() {
                    self.pending = Some(Pending::Reload);
                } else {
                    self.reload(ctx);
                }
            }
            Cmd::NextDiff | Cmd::PrevDiff | Cmd::FirstDiff | Cmd::LastDiff | Cmd::NextConflict | Cmd::PrevConflict => {
                let k = self.model.as_ref().and_then(nav);
                self.select(k);
            }
            Cmd::TakeLeft => self.take(Resolution::Left),
            Cmd::TakeRight => self.take(Resolution::Right),
            Cmd::TakeBase => self.take(Resolution::Base),
            Cmd::TakeBoth => self.take(Resolution::LeftThenRight),
            Cmd::Undo => {
                let k = self.model.as_mut().and_then(MergeModel::undo);
                self.select(k);
            }
            Cmd::Redo => {
                let k = self.model.as_mut().and_then(MergeModel::redo);
                self.select(k);
            }
            _ => {}
        }
    }

    fn is_dirty(&self) -> bool {
        self.model.as_ref().is_some_and(MergeModel::is_dirty)
    }

    fn save_all(&mut self, cx: &mut Cx<'_>) -> bool {
        if !self.is_dirty() {
            return true;
        }
        if self.output_path().is_none() {
            cx.toasts.error("The merge has no output file: enter one in the Output bar, then save.");
            return false;
        }
        self.do_save(cx)
    }

    fn session(&self) -> Option<Session> {
        let mut s = self.session.clone();
        s.kind = SessionKind::TextMerge;
        s.base = Some(s.base.unwrap_or_default());
        s.output = Some(self.output_text.trim().to_owned()).filter(|o| !o.is_empty());
        Some(s)
    }

    fn wants_keyboard(&self) -> bool {
        self.edit.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::text::file::{LineEnding, TextEncoding};

    fn input(s: &str) -> Input {
        Input { file: TextFile::from_text(s), missing: false }
    }

    fn model(base: &str, left: &str, right: &str) -> MergeModel {
        MergeModel::new(input(left), input(base), input(right), MarkerLabels::default(), &TextCompareOptions::default(), 4).unwrap()
    }

    fn conflict_index(m: &MergeModel) -> usize {
        m.chunks.iter().position(|c| c.kind == ChunkKind::Conflict).unwrap()
    }

    fn engine_render(m: &MergeModel) -> Vec<String> {
        let merge3 = merge::Merge3 { chunks: m.chunks.clone() };
        merge::render(&merge3, &m.res, &m.base.file.lines, &m.left.file.lines, &m.right.file.lines, &m.labels)
    }

    #[test]
    fn upper_layout_rows() {
        // Left replaces the middle line with two lines, right deletes it: a 2-row conflict.
        let m = model("a\nb\nc\n", "a\nX\nY\nc\n", "a\nc\n");
        let heights: Vec<usize> = m.chunks.iter().map(chunk_height).collect();
        assert_eq!(heights, vec![1, 2, 1]);
        assert_eq!(m.up_starts, vec![0, 1, 3]);
        assert_eq!(m.up_total, 4);
        assert_eq!(locate(&m.up_starts, m.up_total, 2), Some((1, 1)));
        assert_eq!(locate(&m.up_starts, m.up_total, 4), None);
        // A change with all sides empty still takes one (filler) row.
        let c = MergeChunk { kind: ChunkKind::LeftChange, base: 0..0, left: 0..0, right: 0..0 };
        assert_eq!(chunk_height(&c), 1);
        // Zero-height items are skipped by `locate`.
        assert_eq!(locate(&[0, 2, 2], 5, 2), Some((2, 0)));
        assert_eq!(locate(&[], 0, 0), None);
    }

    #[test]
    fn output_rows_match_engine_render() {
        let all = [
            Resolution::Unresolved,
            Resolution::Left,
            Resolution::Right,
            Resolution::Base,
            Resolution::LeftThenRight,
            Resolution::RightThenLeft,
            Resolution::Custom(vec!["z1".into(), "z2".into()]),
            Resolution::Custom(vec![]),
        ];
        for r in all {
            let mut m = model("a\nb\nc\nd\n", "a\nL1\nL2\nc\nd\n", "a\nR\nc\nD\n");
            let k = conflict_index(&m);
            m.set_res(k, r.clone());
            let mut lines = Vec::new();
            let mut numbers = Vec::new();
            for row in 0..m.out_total {
                if let Some(OutRow::Line { text, line_no, .. }) = m.out_row(row) {
                    lines.push(text.into_owned());
                    numbers.push(line_no);
                }
            }
            assert_eq!(lines, engine_render(&m), "{r:?}");
            assert_eq!(m.output_lines(), lines, "{r:?}");
            assert_eq!(numbers, (1..=lines.len()).collect::<Vec<_>>());
            assert_eq!(m.out_lines, lines.len());
            assert_eq!(m.out_total, lines.len() + m.changes());
        }
    }

    #[test]
    fn headers_and_markers() {
        let m = model("a\nb\nc\n", "a\nL\nc\n", "a\nR\nc\n");
        let k = conflict_index(&m);
        assert_eq!(m.out_row(1), Some(OutRow::Header(k)));
        assert!(matches!(m.out_row(2), Some(OutRow::Line { marker: true, chunk, ref text, .. }) if chunk == k && text.starts_with("<<<<<<<")));
        assert!(matches!(m.out_row(3), Some(OutRow::Line { marker: false, .. })));
        assert_eq!(m.out_row(10_000), None);
        assert_eq!(m.chunk_text(k), "<<<<<<< left\nL\n||||||| base\nb\n=======\nR\n>>>>>>> right");
        assert_eq!(m.out_row_of(k), Some(1));
    }

    #[test]
    fn navigation() {
        // Chunks: U, Conflict, U, LeftChange, U, Conflict, U
        let m = model("1\n2\n3\n4\n5\n6\n7\n", "1\nA\n3\nB\n5\nC\n7\n", "1\nX\n3\n4\n5\nY\n7\n");
        let kinds: Vec<ChunkKind> = m.chunks.iter().map(|c| c.kind).collect();
        use ChunkKind::*;
        assert_eq!(kinds, vec![Unchanged, Conflict, Unchanged, LeftChange, Unchanged, Conflict, Unchanged]);
        assert_eq!(find_diff(&m.chunks, None, Dir::Next), Some(1));
        assert_eq!(find_diff(&m.chunks, Some(1), Dir::Next), Some(3));
        assert_eq!(find_diff(&m.chunks, Some(5), Dir::Next), None);
        assert_eq!(find_diff(&m.chunks, Some(3), Dir::Prev), Some(1));
        assert_eq!(find_diff(&m.chunks, Some(1), Dir::Prev), None);
        assert_eq!(find_diff(&m.chunks, None, Dir::Prev), Some(5));
        assert_eq!(find_diff(&m.chunks, Some(3), Dir::First), Some(1));
        assert_eq!(find_diff(&m.chunks, None, Dir::Last), Some(5));
        assert_eq!(find_diff(&m.chunks, Some(usize::MAX), Dir::Next), None);
        assert_eq!(find_diff(&[], None, Dir::First), None);

        let mut res = m.res.clone();
        assert_eq!(find_conflict(&m.chunks, &res, None, true), Some(1));
        assert_eq!(find_conflict(&m.chunks, &res, Some(1), true), Some(5));
        // Wraps around.
        assert_eq!(find_conflict(&m.chunks, &res, Some(5), true), Some(1));
        assert_eq!(find_conflict(&m.chunks, &res, Some(1), false), Some(5));
        // Resolved conflicts are skipped while others remain unresolved.
        res[5] = Resolution::Left;
        assert_eq!(find_conflict(&m.chunks, &res, Some(1), true), Some(1));
        assert_eq!(find_conflict(&m.chunks, &res, Some(3), true), Some(1));
        // All resolved: any conflict.
        res[1] = Resolution::Right;
        assert_eq!(find_conflict(&m.chunks, &res, Some(1), true), Some(5));
        let none = model("a\n", "a\n", "a\n");
        assert_eq!(find_conflict(&none.chunks, &none.res, None, true), None);
    }

    #[test]
    fn resolution_changes_undo_redo_and_dirty() {
        let mut m = model("a\nb\nc\n", "a\nL\nc\n", "a\nR\nc\n");
        let k = conflict_index(&m);
        assert!(!m.is_dirty());
        assert_eq!(m.out_total, 2 + 1 + 7);
        assert!(m.set_res(k, Resolution::Left));
        assert!(!m.set_res(k, Resolution::Left), "same resolution is not a change");
        assert!(!m.set_res(999, Resolution::Left), "out of range is ignored");
        assert!(m.is_dirty());
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.out_total, 2 + 1 + 1);
        assert_eq!(m.output_lines(), vec!["a", "L", "c"]);
        assert!(m.set_res(k, Resolution::LeftThenRight));
        assert_eq!(m.output_lines(), vec!["a", "L", "R", "c"]);
        assert_eq!(m.undo(), Some(k));
        assert_eq!(m.output_lines(), vec!["a", "L", "c"]);
        assert_eq!(m.undo(), Some(k));
        assert_eq!(m.unresolved(), 1);
        assert!(!m.is_dirty());
        assert_eq!(m.undo(), None);
        assert_eq!(m.redo(), Some(k));
        assert_eq!(m.redo(), Some(k));
        assert_eq!(m.redo(), None);
        assert_eq!(m.output_lines(), vec!["a", "L", "R", "c"]);
        // A new change clears redo.
        m.undo();
        m.set_res(k, Resolution::Custom(text_to_lines("x\r\ny")));
        assert!(m.redo.is_empty());
        assert_eq!(m.output_lines(), vec!["a", "x", "y", "c"]);
        m.mark_saved();
        assert!(!m.is_dirty());
        assert!(text_to_lines("").is_empty());
    }

    #[test]
    fn output_keeps_left_format_and_saves_with_backup() {
        let left = input("a\r\nL\r\nc\r\nd\r\n");
        let m = MergeModel::new(left, input("a\nb\nc\nd\n"), input("a\nb\nc\nD\n"), MarkerLabels::default(), &TextCompareOptions::default(), 4).unwrap();
        assert_eq!(m.conflicts(), 0);
        let f = m.output_file();
        assert_eq!(f.line_ending, LineEnding::CrLf);
        assert_eq!(f.encoding, TextEncoding::Utf8);
        assert_eq!(f.to_text(), "a\r\nL\r\nc\r\nD\r\n");

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("merged.txt");
        std::fs::write(&out, "old").unwrap();
        write_output(&out, &f, true).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "a\r\nL\r\nc\r\nD\r\n");
        assert_eq!(std::fs::read_to_string(backup_path(&out)).unwrap(), "old");
        let fresh = dir.path().join("fresh.txt");
        write_output(&fresh, &f, true).unwrap();
        assert!(!backup_path(&fresh).exists());
        assert!(write_output(Path::new(""), &f, false).is_err());
        assert!(write_output(dir.path(), &f, false).is_err());

        // Missing left: the right file's format is used.
        let m2 = MergeModel::new(Input::missing(), input(""), input("x\ry\r"), MarkerLabels::default(), &TextCompareOptions::default(), 4).unwrap();
        assert_eq!(m2.output_file().line_ending, LineEnding::Cr);
        assert_eq!(m2.warnings().len(), 1);
    }

    #[test]
    fn loading_inputs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_input(&dir.path().join("nope.txt").display().to_string(), Side::Base).unwrap().missing);
        assert!(load_input("  ", Side::Base).unwrap().missing);
        let bin = dir.path().join("b.bin");
        std::fs::write(&bin, b"\x7fELF\0\0\0").unwrap();
        let err = load_input(&bin.display().to_string(), Side::Left).unwrap_err();
        assert!(err.contains("binary"), "{err}");
        assert!(load_input(&dir.path().display().to_string(), Side::Right).is_err());
        let t = dir.path().join("t.txt");
        std::fs::write(&t, "one\ntwo\n").unwrap();
        let ok = load_input(&t.display().to_string(), Side::Right).unwrap();
        assert!(!ok.missing);
        assert_eq!(ok.file.lines, vec!["one", "two"]);
    }

    #[test]
    fn helpers() {
        assert_eq!(visible_slice("\tabc", 4, 2, 3), "  a");
        assert_eq!(visible_slice("héllo", 4, 1, 100), "éllo");
        assert_eq!(visible_slice("x", 4, 5, 3), "");
        assert_eq!(digits(0), 1);
        assert_eq!(digits(1234), 4);
        assert_eq!(backup_path(Path::new("/tmp/a.txt")), PathBuf::from("/tmp/a.txt.bak"));
        assert_eq!(short_name("/x/y/z.rs"), "z.rs");
        assert_eq!(sides_taken(&Resolution::RightThenLeft), [true, false, true]);
        let cols = pane_columns(0.0, 304.0, 3, 10.0, 2.0);
        assert_eq!(cols.len(), 3);
        assert!((cols[2].x1 - 304.0).abs() < 0.01);
        assert!((cols[1].text_x - cols[1].x0 - 10.0).abs() < 0.01);
    }

    #[test]
    fn empty_and_add_add_inputs() {
        let m = MergeModel::new(Input::missing(), Input::missing(), Input::missing(), MarkerLabels::default(), &TextCompareOptions::default(), 4).unwrap();
        assert_eq!((m.up_total, m.out_total, m.changes()), (0, 0, 0));
        assert_eq!(m.out_row(0), None);
        assert!(m.output_file().lines.is_empty());
        assert_eq!(m.warnings().len(), 3);
        // add/add: both sides add different text over an empty (missing) base.
        let m = MergeModel::new(input("a\n"), Input::missing(), input("b\n"), MarkerLabels::default(), &TextCompareOptions::default(), 4).unwrap();
        assert_eq!(m.unresolved(), 1);
        assert_eq!(m.up_total, 1);
    }

    #[test]
    fn view_draws_and_runs_commands_headless() {
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let s = Session { kind: SessionKind::TextMerge, output: Some(out.display().to_string()), ..Session::default() };
        let mut v = MergeView::from_session(&s, &Settings::default(), &ctx);
        v.left_text = "l.txt".into();
        v.model = Some(model("1\n\t2\n3\n4\n5\n", "1\nA\n3\nB\n5\n", "1\nX\n3\n4\n5\n"));
        v.select(Some(1));
        struct Harness {
            settings: Settings,
            changed: bool,
            toasts: crate::view::Toasts,
            open: Vec<crate::view::OpenRequest>,
            exit: i32,
        }
        impl Harness {
            fn cx(&mut self) -> Cx<'_> {
                Cx {
                    settings: &mut self.settings,
                    settings_changed: &mut self.changed,
                    toasts: &mut self.toasts,
                    open: &mut self.open,
                    exit_code: &mut self.exit,
                }
            }
        }
        let mut h = Harness { settings: Settings::default(), changed: false, toasts: Default::default(), open: Vec::new(), exit: 1 };
        let frame = |h: &mut Harness, v: &mut MergeView, cmd: Option<Cmd>| {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))), ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| {
                let mut cx = h.cx();
                if let Some(c) = cmd {
                    v.run(c, ui.ctx(), &mut cx);
                }
                v.ui(ui, &mut cx);
            });
            output.textures_delta.clear();
        };
        let h = &mut h;
        frame(h, &mut v, None);
        frame(h, &mut v, Some(Cmd::NextDiff));
        assert_eq!(v.current, Some(3));
        frame(h, &mut v, Some(Cmd::NextConflict));
        assert_eq!(v.current, Some(1));
        frame(h, &mut v, Some(Cmd::TakeRight));
        assert!(v.is_dirty());
        frame(h, &mut v, Some(Cmd::Undo));
        assert!(!v.is_dirty());
        frame(h, &mut v, Some(Cmd::Redo));
        frame(h, &mut v, Some(Cmd::Save));
        assert_eq!(h.exit, 0);
        assert!(!v.is_dirty());
        assert_eq!(std::fs::read_to_string(&out).unwrap().lines().collect::<Vec<_>>(), vec!["1", "X", "3", "B", "5"]);
        // Unresolved conflict saved non-interactively: exit code 1.
        frame(h, &mut v, Some(Cmd::Undo));
        assert!(v.save_all(&mut h.cx()));
        assert_eq!(h.exit, 1);
        assert!(std::fs::read_to_string(&out).unwrap().contains("<<<<<<<"));
    }

    #[test]
    fn view_session_and_commands_without_files() {
        let ctx = egui::Context::default();
        let s = Session { kind: SessionKind::TextMerge, ..Session::default() };
        let v = MergeView::from_session(&s, &Settings::default(), &ctx);
        assert!(v.job.is_none());
        assert!(!v.supports(Cmd::Save));
        assert!(v.supports(Cmd::Refresh));
        assert!(!v.is_dirty());
        let sess = v.session().unwrap();
        assert_eq!(sess.kind, SessionKind::TextMerge);
        assert_eq!(sess.base, Some(String::new()));
        assert_eq!(sess.output, None);
        assert_eq!(v.title(), "Merge");
    }
}
