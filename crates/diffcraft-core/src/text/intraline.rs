//! Intra-line differences: which parts of two changed lines differ.

use std::ops::Range;

use super::diff::{self, Algorithm, HunkKind};

/// Character class used to split a line into tokens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// Split into words, whitespace runs and single punctuation characters; returns byte ranges.
fn tokenize(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev: Option<Class> = None;
    for (i, c) in s.char_indices() {
        let cl = class(c);
        let joins = prev == Some(cl) && cl != Class::Other;
        match out.last_mut() {
            Some(last) if joins => last.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        prev = Some(cl);
    }
    out
}

/// Byte ranges that differ in `a` and in `b`. Adjacent ranges are merged.
/// Lines longer than `MAX_LEN` bytes are reported as entirely different.
pub fn intraline(a: &str, b: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    const MAX_LEN: usize = 20_000;
    if a == b {
        return (Vec::new(), Vec::new());
    }
    if a.len() > MAX_LEN || b.len() > MAX_LEN {
        return (whole(a), whole(b));
    }
    let ta = tokenize(a);
    let tb = tokenize(b);
    let wa: Vec<&str> = ta.iter().map(|r| a.get(r.clone()).unwrap_or("")).collect();
    let wb: Vec<&str> = tb.iter().map(|r| b.get(r.clone()).unwrap_or("")).collect();
    let hunks = diff::diff_with_budget(&wa, &wb, Algorithm::Myers, 2_000_000);

    let mut ra: Vec<Range<usize>> = Vec::new();
    let mut rb: Vec<Range<usize>> = Vec::new();
    let mut equal_chars = 0usize;
    for h in &hunks {
        if h.kind == HunkKind::Equal {
            // Only count matches that carry meaning; matching spaces alone should not
            // make two unrelated lines look related.
            equal_chars += wa.get(h.a.clone()).map_or(0, |t| t.iter().filter(|w| !w.trim().is_empty()).map(|w| w.len()).sum());
            continue;
        }
        if let Some(r) = span(&ta, h.a.clone()) {
            push_merged(&mut ra, r);
        }
        if let Some(r) = span(&tb, h.b.clone()) {
            push_merged(&mut rb, r);
        }
    }
    // Lines with almost nothing in common are clearer highlighted as a whole.
    let longest = a.trim().len().max(b.trim().len()).max(1);
    if equal_chars * 4 < longest {
        return (whole(a), whole(b));
    }
    (ra, rb)
}

#[allow(clippy::single_range_in_vec_init)]
fn whole(s: &str) -> Vec<Range<usize>> {
    if s.is_empty() { Vec::new() } else { vec![0..s.len()] }
}

fn span(tokens: &[Range<usize>], r: Range<usize>) -> Option<Range<usize>> {
    if r.is_empty() {
        return None;
    }
    let first = tokens.get(r.start)?;
    let last = tokens.get(r.end - 1)?;
    Some(first.start..last.end)
}

fn push_merged(v: &mut Vec<Range<usize>>, r: Range<usize>) {
    match v.last_mut() {
        Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
        _ => v.push(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_changed_word() {
        let (a, b) = intraline("let total = price * qty;", "let total = cost * qty;");
        assert_eq!(a, vec![12..17]);
        assert_eq!(b, vec![12..16]);
    }

    #[test]
    fn unrelated_lines_are_whole() {
        let (a, b) = intraline("alpha beta", "gamma delta");
        assert_eq!(a, vec![0..10]);
        assert_eq!(b, vec![0..11]);
    }

    #[test]
    fn unicode_boundaries() {
        let (a, b) = intraline("héllo wörld ünïcode", "héllo wörld ûnïcode");
        for r in a.iter().chain(&b) {
            assert!("héllo wörld ünïcode".is_char_boundary(r.start) || "héllo wörld ûnïcode".is_char_boundary(r.start));
        }
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn identical() {
        assert_eq!(intraline("x", "x"), (vec![], vec![]));
    }
}
