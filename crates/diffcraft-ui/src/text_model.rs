//! The editable model behind the Text Compare view: both files, the diff, display rows,
//! line edits with undo/redo, navigation and search. Pure logic, no egui.

use std::ops::Range;
use std::path::PathBuf;
use std::time::SystemTime;

use diffcraft_core::text::file::TextFile;
use diffcraft_core::text::{self, DiffStats, Row, TextCompareOptions, TextDiff};

/// One side of the comparison.
#[derive(Clone, Debug, Default)]
pub struct Side {
    pub file: TextFile,
    pub path: Option<PathBuf>,
    /// The file existed when loaded (a missing file compares as empty).
    pub exists: bool,
    pub dirty: bool,
    pub mtime: Option<SystemTime>,
}

/// One undoable change: lines `at..at + inserted.len()` replaced `removed`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    side: usize,
    at: usize,
    removed: Vec<String>,
    inserted: Vec<String>,
}

/// Which rows are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    #[default]
    All,
    /// Only differences.
    Diffs,
    /// Differences with a few lines of context.
    Context,
}

/// Lines of context shown around differences in [`Display::Context`].
pub const CONTEXT_LINES: usize = 3;
const MAX_UNDO: usize = 500;

/// A displayed row: a real row, or a placeholder for hidden identical rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Row(usize),
    Gap { first: usize, count: usize },
}

pub struct TextModel {
    pub sides: [Side; 2],
    pub opts: TextCompareOptions,
    pub diff: TextDiff,
    pub rows: Vec<Row>,
    /// Important difference blocks (row ranges) for navigation.
    pub blocks: Vec<Range<usize>>,
    pub stats: DiffStats,
    /// Line index → row index, per side.
    pub row_of_line: [Vec<usize>; 2],
    pub display: Display,
    pub visible: Vec<Vis>,
    /// The comparison rules were invalid (e.g. a bad regex); the diff fell back to plain rules.
    pub rule_error: Option<String>,
    undo: Vec<Vec<Edit>>,
    redo: Vec<Vec<Edit>>,
    /// Bumped on every change so the view can drop caches.
    pub revision: u64,
}

impl TextModel {
    pub fn new(sides: [Side; 2], opts: TextCompareOptions) -> Self {
        let mut m = TextModel {
            sides,
            opts,
            diff: TextDiff::default(),
            rows: Vec::new(),
            blocks: Vec::new(),
            stats: DiffStats::default(),
            row_of_line: [Vec::new(), Vec::new()],
            display: Display::All,
            visible: Vec::new(),
            rule_error: None,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
        };
        m.rediff();
        m
    }

    pub fn lines(&self, side: usize) -> &[String] {
        self.sides.get(side).map(|s| s.file.lines.as_slice()).unwrap_or_default()
    }

    pub fn line(&self, side: usize, idx: usize) -> Option<&str> {
        self.lines(side).get(idx).map(String::as_str)
    }

    /// Recompute the diff and everything derived from it.
    pub fn rediff(&mut self) {
        let [sa, sb] = &self.sides;
        let (a, b) = (sa.file.lines.as_slice(), sb.file.lines.as_slice());
        let (diff, rule_error) = match text::compare_lines(a, b, &self.opts) {
            Ok(d) => (d, None),
            Err(e) => {
                let plain = TextCompareOptions { unimportant_patterns: Vec::new(), ..self.opts.clone() };
                (text::compare_lines(a, b, &plain).unwrap_or_default(), Some(e.to_string()))
            }
        };
        self.rows = text::align(&diff, a, b);
        self.rule_error = rule_error;
        self.diff = diff;
        self.blocks = text::difference_blocks(&self.rows, true);
        self.stats = text::stats(&self.rows);
        let mut maps = [vec![0usize; self.lines(0).len()], vec![0usize; self.lines(1).len()]];
        for (r, row) in self.rows.iter().enumerate() {
            for (side, line) in [(0usize, row.left), (1, row.right)] {
                if let Some(l) = line
                    && let Some(slot) = maps.get_mut(side).and_then(|m| m.get_mut(l))
                {
                    *slot = r;
                }
            }
        }
        self.row_of_line = maps;
        self.revision += 1;
        self.rebuild_visible();
    }

    pub fn set_display(&mut self, d: Display) {
        self.display = d;
        self.rebuild_visible();
    }

    pub fn rebuild_visible(&mut self) {
        let n = self.rows.len();
        let show: Vec<bool> = match self.display {
            Display::All => vec![true; n],
            Display::Diffs => self.rows.iter().map(Row::is_important).collect(),
            Display::Context => {
                let mut v = vec![false; n];
                for (i, r) in self.rows.iter().enumerate() {
                    if r.is_important() {
                        let lo = i.saturating_sub(CONTEXT_LINES);
                        let hi = (i + CONTEXT_LINES + 1).min(n);
                        for s in v.get_mut(lo..hi).into_iter().flatten() {
                            *s = true;
                        }
                    }
                }
                v
            }
        };
        let mut vis = Vec::with_capacity(n);
        let mut gap: Option<(usize, usize)> = None;
        for (i, s) in show.iter().enumerate() {
            if *s {
                if let Some((first, count)) = gap.take() {
                    vis.push(Vis::Gap { first, count });
                }
                vis.push(Vis::Row(i));
            } else {
                gap = Some(gap.map_or((i, 1), |(f, c)| (f, c + 1)));
            }
        }
        if let Some((first, count)) = gap {
            vis.push(Vis::Gap { first, count });
        }
        self.visible = vis;
    }

    /// Index into `visible` showing `row` (or the gap hiding it).
    pub fn vis_index_of_row(&self, row: usize) -> Option<usize> {
        let first = |v: &Vis| match *v {
            Vis::Row(r) => r,
            Vis::Gap { first, .. } => first,
        };
        let i = self.visible.partition_point(|v| first(v) <= row).checked_sub(1)?;
        Some(i)
    }

    pub fn block_of_row(&self, row: usize) -> Option<usize> {
        self.blocks.iter().position(|b| b.contains(&row))
    }

    /// The next (or previous) block strictly after (before) `row`; from the edges if `row` is None.
    pub fn next_block(&self, row: Option<usize>, forward: bool) -> Option<usize> {
        if forward {
            match row {
                None => (!self.blocks.is_empty()).then_some(0),
                Some(r) => self.blocks.iter().position(|b| b.start > r),
            }
        } else {
            match row {
                None => self.blocks.len().checked_sub(1),
                Some(r) => self.blocks.iter().rposition(|b| b.end <= r),
            }
        }
    }

    /// The line range of each side covered by `rows` (empty at the insertion point when a side
    /// has no lines there).
    pub fn line_ranges(&self, rows: Range<usize>) -> [Range<usize>; 2] {
        let mut out = [0..0, 0..0];
        for (side, slot) in out.iter_mut().enumerate() {
            let lines: Vec<usize> = self.rows.get(rows.clone()).unwrap_or_default().iter().filter_map(|r| if side == 0 { r.left } else { r.right }).collect();
            *slot = match (lines.first(), lines.last()) {
                (Some(&a), Some(&b)) => a..b + 1,
                _ => {
                    let p = self.insertion_point(side, rows.start);
                    p..p
                }
            };
        }
        out
    }

    /// Where a line inserted at `row` on `side` would go.
    pub fn insertion_point(&self, side: usize, row: usize) -> usize {
        self.rows.iter().skip(row).find_map(|r| if side == 0 { r.left } else { r.right }).unwrap_or(self.lines(side).len())
    }

    fn apply(&mut self, e: &Edit) -> Edit {
        let Some(s) = self.sides.get_mut(e.side) else { return e.clone() };
        let len = s.file.lines.len();
        let at = e.at.min(len);
        let end = (at + e.removed.len()).min(len);
        let removed: Vec<String> = s.file.lines.splice(at..end, e.inserted.iter().cloned()).collect();
        s.dirty = true;
        Edit { side: e.side, at, removed, inserted: e.inserted.clone() }
    }

    /// Replace `range` of `side` with `lines`. With `group`, the change joins the previous undo step.
    pub fn splice(&mut self, side: usize, range: Range<usize>, lines: Vec<String>, group: bool) {
        if side > 1 {
            return;
        }
        let len = self.lines(side).len();
        let at = range.start.min(len);
        let end = range.end.clamp(at, len);
        let removed = self.lines(side).get(at..end).map(<[String]>::to_vec).unwrap_or_default();
        if removed == lines {
            return;
        }
        let done = self.apply(&Edit { side, at, removed, inserted: lines });
        match self.undo.last_mut() {
            Some(g) if group => g.push(done),
            _ => self.undo.push(vec![done]),
        }
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.rediff();
    }

    /// Copy the lines of `rows` from one side over the other.
    pub fn copy_rows(&mut self, rows: Range<usize>, to_right: bool) -> bool {
        let [l, r] = self.line_ranges(rows);
        let (src, src_range, dst, dst_range) = if to_right { (0, l, 1, r) } else { (1, r, 0, l) };
        let lines = self.lines(src).get(src_range).map(<[String]>::to_vec).unwrap_or_default();
        let before = self.revision;
        self.splice(dst, dst_range, lines, false);
        self.revision != before
    }

    /// Delete the lines of `rows` on `side`.
    pub fn delete_rows(&mut self, rows: Range<usize>, side: usize) -> bool {
        let ranges = self.line_ranges(rows);
        let Some(range) = ranges.get(side).cloned() else { return false };
        if range.is_empty() {
            return false;
        }
        self.splice(side, range, Vec::new(), false);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(group) = self.undo.pop() else { return false };
        let mut inverse = Vec::new();
        for e in group.iter().rev() {
            let inv = Edit { side: e.side, at: e.at, removed: e.inserted.clone(), inserted: e.removed.clone() };
            self.apply(&inv);
            inverse.push(e.clone());
        }
        inverse.reverse();
        self.redo.push(inverse);
        self.rediff();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(group) = self.redo.pop() else { return false };
        for e in &group {
            self.apply(e);
        }
        self.undo.push(group);
        self.rediff();
        true
    }

    /// Swap left and right (paths, contents, undo history).
    pub fn swap(&mut self) {
        self.sides.swap(0, 1);
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()).flatten() {
            e.side = 1 - e.side.min(1);
        }
        self.rediff();
    }

    /// Find `query` after (or before) `from` = (row, side), wrapping around.
    pub fn find(&self, query: &str, match_case: bool, from: Option<(usize, usize)>, forward: bool) -> Option<(usize, usize)> {
        if query.is_empty() || self.rows.is_empty() {
            return None;
        }
        let q = if match_case { query.to_owned() } else { query.to_lowercase() };
        let hit = |row: usize, side: usize| -> bool {
            let Some(r) = self.rows.get(row) else { return false };
            let line = if side == 0 { r.left } else { r.right };
            line.and_then(|l| self.line(side, l)).is_some_and(|t| if match_case { t.contains(&q) } else { t.to_lowercase().contains(&q) })
        };
        // Positions are ordered (row, side); walk all of them once starting after `from`.
        let total = self.rows.len() * 2;
        let start = from.map_or(if forward { total - 1 } else { 0 }, |(r, s)| r * 2 + s.min(1));
        for step in 1..=total {
            let pos = if forward { (start + step) % total } else { (start + total - step % total) % total };
            if hit(pos / 2, pos % 2) {
                return Some((pos / 2, pos % 2));
            }
        }
        None
    }

    pub fn is_dirty(&self) -> bool {
        self.sides.iter().any(|s| s.dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::text::RowKind;

    fn side(text: &str) -> Side {
        Side { file: TextFile::from_text(text), exists: true, ..Side::default() }
    }

    fn model(a: &str, b: &str) -> TextModel {
        TextModel::new([side(a), side(b)], TextCompareOptions::default())
    }

    #[test]
    fn copy_block_and_undo_redo() {
        let mut m = model("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(m.blocks, vec![1..2]);
        assert!(m.copy_rows(1..2, true));
        assert_eq!(m.lines(1), ["a", "b", "c"]);
        assert!(m.blocks.is_empty());
        assert!(m.sides[1].dirty);
        assert!(m.undo());
        assert_eq!(m.lines(1), ["a", "X", "c"]);
        assert!(m.redo());
        assert_eq!(m.lines(1), ["a", "b", "c"]);
    }

    #[test]
    fn copy_insertions_and_deletions() {
        // Right-only lines copied to the left are inserted at the right place.
        let mut m = model("1\n4\n", "1\n2\n3\n4\n");
        let b = m.blocks[0].clone();
        assert!(m.copy_rows(b, false));
        assert_eq!(m.lines(0), ["1", "2", "3", "4"]);
        // Copying "nothing" to the right deletes the right lines.
        let mut m = model("1\n4\n", "1\n2\n3\n4\n");
        let b = m.blocks[0].clone();
        assert!(m.copy_rows(b, true));
        assert_eq!(m.lines(1), ["1", "4"]);
    }

    #[test]
    fn grouped_edits_undo_together() {
        let mut m = model("ab\n", "ab\n");
        m.splice(0, 0..1, vec!["a".into()], false);
        m.splice(0, 1..1, vec!["b".into()], true);
        assert_eq!(m.lines(0), ["a", "b"]);
        m.undo();
        assert_eq!(m.lines(0), ["ab"]);
        assert!(!m.can_undo());
    }

    #[test]
    fn display_filters() {
        let a: String = (0..20).map(|i| format!("{i}\n")).collect();
        let b = a.replace("10\n", "ten\n");
        let mut m = model(&a, &b);
        assert_eq!(m.visible.len(), 20);
        m.set_display(Display::Diffs);
        assert_eq!(m.visible, vec![Vis::Gap { first: 0, count: 10 }, Vis::Row(10), Vis::Gap { first: 11, count: 9 }]);
        m.set_display(Display::Context);
        assert_eq!(m.visible.len(), 2 * CONTEXT_LINES + 1 + 2);
        assert_eq!(m.vis_index_of_row(10), Some(1 + CONTEXT_LINES));
        assert_eq!(m.vis_index_of_row(0), Some(0));
    }

    #[test]
    fn navigation() {
        let m = model("a\nb\nc\nd\ne\n", "a\nB\nc\nD\ne\n");
        assert_eq!(m.next_block(None, true), Some(0));
        assert_eq!(m.next_block(Some(1), true), Some(1));
        assert_eq!(m.next_block(Some(3), true), None);
        assert_eq!(m.next_block(Some(3), false), Some(0));
        assert_eq!(m.next_block(None, false), Some(1));
        assert_eq!(m.block_of_row(3), Some(1));
    }

    #[test]
    fn find_wraps_both_sides() {
        let m = model("alpha\nbeta\n", "alpha\nBETA gamma\n");
        assert_eq!(m.find("beta", false, None, true), Some((1, 0)));
        assert_eq!(m.find("beta", false, Some((1, 0)), true), Some((1, 1)));
        assert_eq!(m.find("beta", false, Some((1, 1)), true), Some((1, 0)));
        assert_eq!(m.find("beta", true, None, true), Some((1, 0)));
        assert_eq!(m.find("gamma", false, None, false), Some((1, 1)));
        assert_eq!(m.find("zzz", false, None, true), None);
    }

    #[test]
    fn swap_and_delete() {
        let mut m = model("x\ny\n", "x\n");
        assert_eq!(m.rows[1].kind, RowKind::LeftOnly);
        assert!(m.delete_rows(1..2, 0));
        assert_eq!(m.lines(0), ["x"]);
        m.swap();
        m.undo();
        assert_eq!(m.lines(1), ["x", "y"], "undo follows the swapped side");
        assert!(!m.delete_rows(0..0, 0));
    }

    #[test]
    fn out_of_range_edits_are_clamped() {
        let mut m = model("a\n", "a\n");
        m.splice(0, 50..90, vec!["z".into()], false);
        assert_eq!(m.lines(0), ["a", "z"]);
        m.splice(7, 0..1, vec![], false);
        assert_eq!(m.insertion_point(1, 99), 1);
    }
}
