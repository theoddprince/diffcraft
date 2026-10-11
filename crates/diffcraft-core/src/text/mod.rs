//! Text comparison: line diff with comparison rules, side-by-side alignment, intra-line
//! highlighting and text file loading/saving that preserves encoding and line endings.

pub mod diff;
pub mod file;
pub mod intraline;

use std::borrow::Cow;
use std::ops::Range;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub use diff::{Algorithm, Hunk, HunkKind};

use crate::{Error, Result};

/// How whitespace takes part in the comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WhitespaceMode {
    /// Every whitespace difference is important.
    #[default]
    Compare,
    /// Leading and trailing whitespace is unimportant.
    IgnoreLeadingTrailing,
    /// Changes in the amount of whitespace are unimportant (`a  b` == `a b`).
    IgnoreChanges,
    /// All whitespace is unimportant (`ab` == `a b`).
    IgnoreAll,
}

impl WhitespaceMode {
    pub const ALL: [WhitespaceMode; 4] =
        [WhitespaceMode::Compare, WhitespaceMode::IgnoreLeadingTrailing, WhitespaceMode::IgnoreChanges, WhitespaceMode::IgnoreAll];

    pub fn label(self) -> &'static str {
        match self {
            WhitespaceMode::Compare => "Compare whitespace",
            WhitespaceMode::IgnoreLeadingTrailing => "Ignore leading/trailing whitespace",
            WhitespaceMode::IgnoreChanges => "Ignore whitespace changes",
            WhitespaceMode::IgnoreAll => "Ignore all whitespace",
        }
    }
}

/// Rules for a text comparison: what counts as an important difference.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextCompareOptions {
    pub algorithm: Algorithm,
    pub whitespace: WhitespaceMode,
    pub ignore_case: bool,
    /// Inserted or deleted runs of blank lines are unimportant.
    pub ignore_blank_lines: bool,
    /// Regular expressions; matching text is unimportant (e.g. `\$Id:.*\$`, timestamps).
    pub unimportant_patterns: Vec<String>,
}

impl TextCompareOptions {
    pub fn has_rules(&self) -> bool {
        self.whitespace != WhitespaceMode::Compare || self.ignore_case || !self.unimportant_patterns.is_empty()
    }
}

/// Compiled form of [`TextCompareOptions`].
pub struct Normalizer {
    whitespace: WhitespaceMode,
    ignore_case: bool,
    patterns: Vec<Regex>,
}

impl Normalizer {
    pub fn new(opts: &TextCompareOptions) -> Result<Self> {
        let mut patterns = Vec::new();
        for p in opts.unimportant_patterns.iter().filter(|p| !p.trim().is_empty()) {
            let re =
                regex::RegexBuilder::new(p).size_limit(1 << 20).build().map_err(|e| Error::InvalidInput(format!("unimportant-text pattern `{p}`: {e}")))?;
            patterns.push(re);
        }
        Ok(Normalizer { whitespace: opts.whitespace, ignore_case: opts.ignore_case, patterns })
    }

    /// The comparison key of a line: equal keys mean "equal or only unimportantly different".
    pub fn key<'l>(&self, line: &'l str) -> Cow<'l, str> {
        let mut s: Cow<'l, str> = Cow::Borrowed(line);
        for re in &self.patterns {
            if re.is_match(&s) {
                s = Cow::Owned(re.replace_all(&s, "").into_owned());
            }
        }
        s = match self.whitespace {
            WhitespaceMode::Compare => s,
            WhitespaceMode::IgnoreLeadingTrailing => match s {
                Cow::Borrowed(b) => Cow::Borrowed(b.trim()),
                Cow::Owned(o) => Cow::Owned(o.trim().to_owned()),
            },
            WhitespaceMode::IgnoreChanges => Cow::Owned(s.split_whitespace().collect::<Vec<_>>().join(" ")),
            WhitespaceMode::IgnoreAll => Cow::Owned(s.chars().filter(|c| !c.is_whitespace()).collect()),
        };
        if self.ignore_case && s.chars().any(char::is_uppercase) {
            s = Cow::Owned(s.to_lowercase());
        }
        s
    }
}

/// Result of a line comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextDiff {
    pub hunks: Vec<Hunk>,
}

impl TextDiff {
    /// Number of important (non-equal, not unimportant) hunks.
    pub fn important_changes(&self) -> usize {
        self.hunks.iter().filter(|h| h.is_change() && !h.unimportant).count()
    }

    pub fn is_identical(&self) -> bool {
        self.hunks.iter().all(|h| !h.is_change())
    }
}

/// Diff two line lists under `opts`.
pub fn compare_lines<S: AsRef<str>>(a: &[S], b: &[S], opts: &TextCompareOptions) -> Result<TextDiff> {
    let norm = Normalizer::new(opts)?;
    let ka: Vec<Cow<'_, str>> = a.iter().map(|l| norm.key(l.as_ref())).collect();
    let kb: Vec<Cow<'_, str>> = b.iter().map(|l| norm.key(l.as_ref())).collect();
    let mut hunks = diff::diff(&ka, &kb, opts.algorithm);
    if opts.ignore_blank_lines {
        let blank_a = |r: &Range<usize>| ka.get(r.clone()).is_some_and(|s| s.iter().all(|l| l.trim().is_empty()));
        let blank_b = |r: &Range<usize>| kb.get(r.clone()).is_some_and(|s| s.iter().all(|l| l.trim().is_empty()));
        for h in &mut hunks {
            if h.is_change() && blank_a(&h.a) && blank_b(&h.b) {
                h.unimportant = true;
            }
        }
    }
    Ok(TextDiff { hunks })
}

/// Classification of one side-by-side row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// Identical text.
    Same,
    /// Equal under the comparison rules but the raw text differs (unimportant difference).
    Minor,
    /// Different text on both sides.
    Changed,
    /// Only the left side has a line here.
    LeftOnly,
    /// Only the right side has a line here.
    RightOnly,
}

impl RowKind {
    pub fn is_difference(self) -> bool {
        !matches!(self, RowKind::Same)
    }
}

/// One row of the side-by-side view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub kind: RowKind,
    /// Index of the hunk the row came from.
    pub hunk: usize,
    /// The row belongs to a change made only of unimportant text.
    pub unimportant: bool,
}

impl Row {
    /// Rows that should be shown as an important difference.
    pub fn is_important(&self) -> bool {
        matches!(self.kind, RowKind::Changed | RowKind::LeftOnly | RowKind::RightOnly) && !self.unimportant
    }
}

/// Lay out hunks as side-by-side rows (replaced lines are paired by position).
pub fn align<S: AsRef<str>>(diff: &TextDiff, a: &[S], b: &[S]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(a.len().max(b.len()));
    for (hi, h) in diff.hunks.iter().enumerate() {
        match h.kind {
            HunkKind::Equal => {
                for (i, j) in h.a.clone().zip(h.b.clone()) {
                    let same = match (a.get(i), b.get(j)) {
                        (Some(x), Some(y)) => x.as_ref() == y.as_ref(),
                        _ => false,
                    };
                    let kind = if same { RowKind::Same } else { RowKind::Minor };
                    rows.push(Row { left: Some(i), right: Some(j), kind, hunk: hi, unimportant: !same });
                }
            }
            _ => {
                let n = h.a.len().max(h.b.len());
                for t in 0..n {
                    let l = (t < h.a.len()).then_some(h.a.start + t);
                    let r = (t < h.b.len()).then_some(h.b.start + t);
                    let kind = match (l, r) {
                        (Some(_), Some(_)) => RowKind::Changed,
                        (Some(_), None) => RowKind::LeftOnly,
                        _ => RowKind::RightOnly,
                    };
                    rows.push(Row { left: l, right: r, kind, hunk: hi, unimportant: h.unimportant });
                }
            }
        }
    }
    rows
}

/// Contiguous runs of difference rows, for next/previous-difference navigation.
/// With `important_only`, unimportant differences are skipped.
pub fn difference_blocks(rows: &[Row], important_only: bool) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev_hunk = usize::MAX;
    for (i, r) in rows.iter().enumerate() {
        let counts = if important_only { r.is_important() } else { r.kind.is_difference() };
        if !counts {
            prev_hunk = usize::MAX;
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == i && (r.hunk == prev_hunk || r.kind == RowKind::Minor) => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
        prev_hunk = r.hunk;
    }
    out
}

/// Summary counts used by status bars and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStats {
    pub important_blocks: usize,
    pub unimportant_blocks: usize,
    pub changed_lines: usize,
    pub left_only_lines: usize,
    pub right_only_lines: usize,
}

pub fn stats(rows: &[Row]) -> DiffStats {
    let mut s = DiffStats::default();
    for r in rows {
        match r.kind {
            RowKind::Changed => s.changed_lines += 1,
            RowKind::LeftOnly => s.left_only_lines += 1,
            RowKind::RightOnly => s.right_only_lines += 1,
            _ => {}
        }
    }
    s.important_blocks = difference_blocks(rows, true).len();
    s.unimportant_blocks = difference_blocks(rows, false).len().saturating_sub(s.important_blocks);
    s
}

/// Expand tabs to spaces for display.
pub fn expand_tabs(s: &str, tab_width: usize) -> Cow<'_, str> {
    if !s.contains('\t') {
        return Cow::Borrowed(s);
    }
    let tw = tab_width.clamp(1, 16);
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let n = tw - col % tw;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    #[test]
    fn whitespace_rules() {
        let a = lines("int  x = 1;\nfoo\n");
        let b = lines("int x = 1;  \nfoo\n");
        let strict = compare_lines(&a, &b, &TextCompareOptions::default()).unwrap();
        assert_eq!(strict.important_changes(), 1);
        let opts = TextCompareOptions { whitespace: WhitespaceMode::IgnoreChanges, ..Default::default() };
        let loose = compare_lines(&a, &b, &opts).unwrap();
        assert!(loose.is_identical());
        let rows = align(&loose, &a, &b);
        assert_eq!(rows[0].kind, RowKind::Minor);
        assert_eq!(rows[1].kind, RowKind::Same);
        assert_eq!(difference_blocks(&rows, true).len(), 0);
        assert_eq!(difference_blocks(&rows, false).len(), 1);
    }

    #[test]
    fn case_and_patterns() {
        let a = lines("Hello\n// $Id: 12 $\n");
        let b = lines("hello\n// $Id: 99 $\n");
        let opts = TextCompareOptions { ignore_case: true, unimportant_patterns: vec![r"\$Id:[^$]*\$".into()], ..Default::default() };
        assert!(compare_lines(&a, &b, &opts).unwrap().is_identical());
    }

    #[test]
    fn bad_pattern_is_an_error_not_a_panic() {
        let opts = TextCompareOptions { unimportant_patterns: vec!["(".into()], ..Default::default() };
        assert!(compare_lines(&["a"], &["b"], &opts).is_err());
    }

    #[test]
    fn blank_lines_unimportant() {
        let a = lines("a\nb");
        let b = lines("a\n\n\nb");
        let opts = TextCompareOptions { ignore_blank_lines: true, ..Default::default() };
        let d = compare_lines(&a, &b, &opts).unwrap();
        assert_eq!(d.important_changes(), 0);
        let rows = align(&d, &a, &b);
        assert!(rows.iter().all(|r| !r.is_important()));
    }

    #[test]
    fn align_pairs_replacements() {
        let a = lines("1\n2\n3\n4");
        let b = lines("1\nX\nY\nZ\n4");
        let d = compare_lines(&a, &b, &TextCompareOptions::default()).unwrap();
        let rows = align(&d, &a, &b);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RowKind::Same, RowKind::Changed, RowKind::Changed, RowKind::RightOnly, RowKind::Same]);
        assert_eq!(difference_blocks(&rows, true), vec![1..4]);
        let s = stats(&rows);
        assert_eq!((s.changed_lines, s.right_only_lines, s.important_blocks), (2, 1, 1));
    }

    #[test]
    fn tabs() {
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        assert_eq!(expand_tabs("\t", 0), " ");
    }
}
