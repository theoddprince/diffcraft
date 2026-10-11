//! Unified diff output (`diff -u` / git style).

use diffcraft_core::text::{HunkKind, TextDiff};

/// One line of the edit script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// Context: left line `.0` equals right line `.1`.
    Ctx(usize, usize),
    /// Left line removed.
    Del(usize),
    /// Right line added.
    Ins(usize),
}

impl Op {
    fn is_change(self) -> bool {
        !matches!(self, Op::Ctx(..))
    }
}

/// Lines of one side plus whether its last line ends with a newline.
struct Side<'a> {
    lines: &'a [String],
    final_newline: bool,
}

impl Side<'_> {
    fn text(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", String::as_str)
    }

    /// Whether line `i` is followed by a line terminator in the file.
    fn terminated(&self, i: usize) -> bool {
        self.final_newline || i.saturating_add(1) < self.lines.len()
    }

    fn clamp(&self, r: &std::ops::Range<usize>) -> std::ops::Range<usize> {
        let end = r.end.min(self.lines.len());
        r.start.min(end)..end
    }
}

/// Unified diff (like `diff -u` / git), with `---`/`+++` headers and `@@ -a,b +c,d @@` hunks.
///
/// `context` lines are shown around changes, and changes closer than `2 * context` lines share a
/// hunk. Unimportant hunks (`Hunk::unimportant`) are still emitted, and so are lines that the
/// comparison rules called equal but whose raw text differs: the output is always a patch that
/// turns `left` into `right` exactly. As in GNU diff, a count of 1 is written without `,1`.
///
/// If `left_final_newline`/`right_final_newline` is false, `\ No newline at end of file` follows
/// the last line of that side when it appears in a hunk. Identical input gives an empty string.
#[allow(clippy::too_many_arguments)]
pub fn unified_diff(
    left_name: &str,
    right_name: &str,
    left: &[String],
    right: &[String],
    diff: &TextDiff,
    context: usize,
    left_final_newline: bool,
    right_final_newline: bool,
) -> String {
    let a = Side { lines: left, final_newline: left_final_newline };
    let b = Side { lines: right, final_newline: right_final_newline };
    let ops = edit_script(&a, &b, diff);
    let groups = group(&ops, context);
    if groups.is_empty() {
        return String::new();
    }

    // Lines of each side consumed before op k.
    let mut a_before = Vec::with_capacity(ops.len() + 1);
    let mut b_before = Vec::with_capacity(ops.len() + 1);
    let (mut na, mut nb) = (0usize, 0usize);
    for op in &ops {
        a_before.push(na);
        b_before.push(nb);
        match op {
            Op::Ctx(..) => {
                na += 1;
                nb += 1;
            }
            Op::Del(_) => na += 1,
            Op::Ins(_) => nb += 1,
        }
    }
    a_before.push(na);
    b_before.push(nb);

    let mut out = format!("--- {left_name}\n+++ {right_name}\n");
    for (lo, hi) in groups {
        let Some(window) = ops.get(lo..hi) else { continue };
        let a_count = window.iter().filter(|o| !matches!(o, Op::Ins(_))).count();
        let b_count = window.iter().filter(|o| !matches!(o, Op::Del(_))).count();
        let a_start = a_before.get(lo).copied().unwrap_or(0);
        let b_start = b_before.get(lo).copied().unwrap_or(0);
        out.push_str(&format!("@@ -{} +{} @@\n", range_spec(a_start, a_count), range_spec(b_start, b_count)));
        for op in window {
            let (prefix, text, terminated) = match *op {
                Op::Ctx(i, _) => (' ', a.text(i), a.terminated(i)),
                Op::Del(i) => ('-', a.text(i), a.terminated(i)),
                Op::Ins(j) => ('+', b.text(j), b.terminated(j)),
            };
            out.push(prefix);
            out.push_str(text);
            out.push('\n');
            if !terminated {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }
    out
}

/// `start,count` of a hunk header; an empty range names the line before it.
fn range_spec(before: usize, count: usize) -> String {
    match count {
        0 => format!("{before},0"),
        1 => format!("{}", before.saturating_add(1)),
        n => format!("{},{n}", before.saturating_add(1)),
    }
}

/// Flatten the hunks into a line edit script. Runs of removed lines come before runs of
/// added lines. Equal-hunk pairs whose raw text (or line terminator) differs become changes.
fn edit_script(a: &Side<'_>, b: &Side<'_>, diff: &TextDiff) -> Vec<Op> {
    let mut ops = Vec::with_capacity(a.lines.len().max(b.lines.len()));
    let mut dels: Vec<Op> = Vec::new();
    let mut inss: Vec<Op> = Vec::new();
    let flush = |ops: &mut Vec<Op>, dels: &mut Vec<Op>, inss: &mut Vec<Op>| {
        ops.append(dels);
        ops.append(inss);
    };
    for h in &diff.hunks {
        let ra = a.clamp(&h.a);
        let rb = b.clamp(&h.b);
        match h.kind {
            HunkKind::Equal => {
                for (i, j) in ra.zip(rb) {
                    if a.text(i) == b.text(j) && a.terminated(i) == b.terminated(j) {
                        flush(&mut ops, &mut dels, &mut inss);
                        ops.push(Op::Ctx(i, j));
                    } else {
                        dels.push(Op::Del(i));
                        inss.push(Op::Ins(j));
                    }
                }
            }
            HunkKind::Insert | HunkKind::Delete | HunkKind::Replace => {
                dels.extend(ra.map(Op::Del));
                inss.extend(rb.map(Op::Ins));
            }
        }
        // Keep consecutive change hunks merged (dels before inss) but flush at equal context.
    }
    flush(&mut ops, &mut dels, &mut inss);
    ops
}

/// Op-index windows `[lo, hi)` of the hunks to print.
fn group(ops: &[Op], context: usize) -> Vec<(usize, usize)> {
    let n = ops.len();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !ops.get(i).is_some_and(|o| o.is_change()) {
            i += 1;
            continue;
        }
        let start = i;
        while ops.get(i).is_some_and(|o| o.is_change()) {
            i += 1;
        }
        let lo = start.saturating_sub(context);
        let hi = i.saturating_add(context).min(n);
        match groups.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => groups.push((lo, hi)),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::text::{TextCompareOptions, WhitespaceMode, compare_lines};
    use proptest::prelude::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    fn udiff(a: &[String], b: &[String], ctx: usize) -> String {
        let d = compare_lines(a, b, &TextCompareOptions::default()).unwrap();
        unified_diff("a.txt", "b.txt", a, b, &d, ctx, true, true)
    }

    /// Minimal patch applier: returns the new lines and whether the new file ends in a newline.
    fn apply(old: &[String], old_final_nl: bool, patch: &str) -> Result<(Vec<String>, bool), String> {
        let mut out: Vec<String> = Vec::new();
        let mut new_nl = true;
        let mut pos = 0usize; // next unread old line
        let mut lines = patch.lines();
        if patch.is_empty() {
            return Ok((old.to_vec(), old_final_nl));
        }
        let h1 = lines.next().ok_or("no header")?;
        let h2 = lines.next().ok_or("no header")?;
        if !h1.starts_with("--- ") || !h2.starts_with("+++ ") {
            return Err("bad header".into());
        }
        let mut last_prefix = ' ';
        let mut saw_old_no_nl = false;
        for line in lines {
            if let Some(rest) = line.strip_prefix("@@ -") {
                let old_spec = rest.split(' ').next().ok_or("bad @@")?;
                let (s, c) = match old_spec.split_once(',') {
                    Some((s, c)) => (s.parse::<usize>().unwrap(), c.parse::<usize>().unwrap()),
                    None => (old_spec.parse::<usize>().unwrap(), 1),
                };
                let first = if c == 0 { s } else { s - 1 };
                if first < pos {
                    return Err(format!("overlapping hunk at {first} < {pos}"));
                }
                out.extend_from_slice(&old[pos..first]);
                pos = first;
                continue;
            }
            if line == "\\ No newline at end of file" {
                match last_prefix {
                    '-' => saw_old_no_nl = true,
                    '+' => new_nl = false,
                    _ => {
                        saw_old_no_nl = true;
                        new_nl = false;
                    }
                }
                continue;
            }
            let (p, text) = line.split_at(1);
            last_prefix = p.chars().next().unwrap();
            match p {
                " " | "-" => {
                    if old.get(pos).map(String::as_str) != Some(text) {
                        return Err(format!("mismatch at old line {pos}: {text:?}"));
                    }
                    pos += 1;
                    if p == " " {
                        out.push(text.to_owned());
                    }
                }
                "+" => out.push(text.to_owned()),
                _ => return Err(format!("bad line {line:?}")),
            }
        }
        out.extend_from_slice(&old[pos..]);
        if pos < old.len() {
            // Tail copied unchanged: the final newline state carries over.
            new_nl = old_final_nl;
        }
        if saw_old_no_nl && old_final_nl {
            return Err("marker on a side that has a final newline".into());
        }
        Ok((out, new_nl))
    }

    #[test]
    fn simple_change_exact_text() {
        let a = v(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
        let b = v(&["1", "2", "3", "4", "X", "6", "7", "8", "9"]);
        let expected = "--- a.txt\n+++ b.txt\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+X\n 6\n 7\n 8\n";
        assert_eq!(udiff(&a, &b, 3), expected);
        assert_eq!(udiff(&a, &b, 0), "--- a.txt\n+++ b.txt\n@@ -5 +5 @@\n-5\n+X\n");
    }

    #[test]
    fn empty_sides() {
        let a: Vec<String> = Vec::new();
        let b = v(&["x", "y"]);
        assert_eq!(udiff(&a, &b, 3), "--- a.txt\n+++ b.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n");
        assert_eq!(udiff(&b, &a, 3), "--- a.txt\n+++ b.txt\n@@ -1,2 +0,0 @@\n-x\n-y\n");
        // Pure insertion in the middle with no context names the line before it.
        let a = v(&["1", "2"]);
        let b = v(&["1", "new", "2"]);
        assert_eq!(udiff(&a, &b, 0), "--- a.txt\n+++ b.txt\n@@ -1,0 +2 @@\n+new\n");
    }

    #[test]
    fn identical_is_empty() {
        let a = v(&["same", "lines"]);
        assert_eq!(udiff(&a, &a, 3), "");
        assert_eq!(udiff(&[], &[], 3), "");
    }

    #[test]
    fn nearby_hunks_merge() {
        let a: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let mut b = a.clone();
        b[5] = "five".into();
        b[10] = "ten".into(); // gap of 4 equal lines between changes
        let two = udiff(&a, &b, 2);
        assert_eq!(two.matches("@@ -").count(), 1, "{two}");
        assert!(two.contains("@@ -4,10 +4,10 @@"), "{two}");
        let one = udiff(&a, &b, 1);
        assert_eq!(one.matches("@@ -").count(), 2, "{one}");
        assert!(one.contains("@@ -5,3 +5,3 @@") && one.contains("@@ -10,3 +10,3 @@"), "{one}");
    }

    #[test]
    fn no_newline_marker() {
        let a = v(&["a", "b"]);
        let b = v(&["a", "c"]);
        let d = compare_lines(&a, &b, &TextCompareOptions::default()).unwrap();
        let out = unified_diff("l", "r", &a, &b, &d, 3, false, true);
        assert_eq!(out, "--- l\n+++ r\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n");
        // Same text, only the final newline differs: still a change.
        let d = compare_lines(&a, &a, &TextCompareOptions::default()).unwrap();
        let out = unified_diff("l", "r", &a, &a, &d, 3, true, false);
        assert_eq!(out, "--- l\n+++ r\n@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file\n");
        // Both lack it: context line carries the marker.
        let b2 = v(&["x", "b"]);
        let d = compare_lines(&a, &b2, &TextCompareOptions::default()).unwrap();
        let out = unified_diff("l", "r", &a, &b2, &d, 3, false, false);
        assert_eq!(out, "--- l\n+++ r\n@@ -1,2 +1,2 @@\n-a\n+x\n b\n\\ No newline at end of file\n");
    }

    #[test]
    fn minor_lines_are_emitted_as_changes() {
        let a = v(&["a  b", "c"]);
        let b = v(&["a b", "c"]);
        let opts = TextCompareOptions { whitespace: WhitespaceMode::IgnoreChanges, ..Default::default() };
        let d = compare_lines(&a, &b, &opts).unwrap();
        assert!(d.is_identical());
        let out = unified_diff("l", "r", &a, &b, &d, 1, true, true);
        assert_eq!(apply(&a, true, &out).unwrap().0, b);
    }

    #[test]
    fn inconsistent_diff_does_not_panic() {
        let a = v(&["a"]);
        let bogus = TextDiff { hunks: vec![diffcraft_core::text::Hunk::new(HunkKind::Replace, 5..usize::MAX, 0..3)] };
        let _ = unified_diff("l", "r", &a, &a, &bogus, usize::MAX, false, false);
    }

    fn line_vec() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "d", "", " a", "a "]).prop_map(str::to_owned), 0..25)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn round_trip(a in line_vec(), b in line_vec(), ctx in 0usize..4, anl in any::<bool>(), bnl in any::<bool>(), loose in any::<bool>()) {
            let opts = if loose {
                TextCompareOptions { whitespace: WhitespaceMode::IgnoreAll, ignore_blank_lines: true, ..Default::default() }
            } else {
                TextCompareOptions::default()
            };
            let d = compare_lines(&a, &b, &opts).unwrap();
            let patch = unified_diff("l", "r", &a, &b, &d, ctx, anl, bnl);
            let (got, got_nl) = apply(&a, anl, &patch).map_err(|e| TestCaseError::fail(format!("{e}\n{patch}")))?;
            prop_assert_eq!(&got, &b, "{}", patch);
            if !b.is_empty() && !patch.is_empty() {
                prop_assert_eq!(got_nl, bnl, "{}", patch);
            }
            let same_bytes = a == b && (a.is_empty() || anl == bnl);
            prop_assert_eq!(patch.is_empty(), same_bytes);
        }
    }
}
