//! Sequence diff algorithms: Myers (linear space, O((N+M)·D)), patience and histogram.
//!
//! All three work on interned `u32` ids, so they are generic over any `Eq + Hash` element
//! (lines, tokens, bytes). Recursion is replaced by explicit work stacks so pathological
//! inputs cannot overflow the call stack, and Myers carries an operation budget: when a
//! sub-problem would cost more than the budget it is reported as a plain replacement
//! instead of stalling the UI.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Which diff algorithm to run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Algorithm {
    /// Classic minimal diff (Myers 1986). Smallest edit script, can look noisy on code.
    Myers,
    /// Anchors on lines that are unique on both sides; reads best on source code.
    Patience,
    /// Git's histogram diff: like patience but also anchors on rare (not only unique) lines.
    #[default]
    Histogram,
}

impl Algorithm {
    pub const ALL: [Algorithm; 3] = [Algorithm::Histogram, Algorithm::Patience, Algorithm::Myers];

    pub fn label(self) -> &'static str {
        match self {
            Algorithm::Myers => "Myers (minimal)",
            Algorithm::Patience => "Patience",
            Algorithm::Histogram => "Histogram",
        }
    }
}

/// The kind of a [`Hunk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HunkKind {
    Equal,
    /// Present only on the right (`a` is empty).
    Insert,
    /// Present only on the left (`b` is empty).
    Delete,
    /// Both sides present but different.
    Replace,
}

/// A run of elements: `a` indexes the left sequence, `b` the right one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub kind: HunkKind,
    pub a: Range<usize>,
    pub b: Range<usize>,
    /// The change consists only of "unimportant" text (whitespace, blank lines, ignored patterns).
    #[serde(default)]
    pub unimportant: bool,
}

impl Hunk {
    pub fn new(kind: HunkKind, a: Range<usize>, b: Range<usize>) -> Self {
        Hunk { kind, a, b, unimportant: false }
    }

    pub fn is_change(&self) -> bool {
        self.kind != HunkKind::Equal
    }
}

/// Default number of Myers inner-loop steps before a sub-problem is reported as a replacement.
pub const DEFAULT_BUDGET: u64 = 200_000_000;

/// Diff two sequences. The hunks cover both sequences completely and in order.
pub fn diff<T: Eq + Hash>(a: &[T], b: &[T], algorithm: Algorithm) -> Vec<Hunk> {
    diff_with_budget(a, b, algorithm, DEFAULT_BUDGET)
}

/// Like [`diff`] with an explicit Myers budget (inner-loop steps).
pub fn diff_with_budget<T: Eq + Hash>(a: &[T], b: &[T], algorithm: Algorithm, budget: u64) -> Vec<Hunk> {
    let (ia, ib) = intern(a, b);
    let mut ctx = Ctx { a: &ia, b: &ib, matches: Vec::new(), budget };
    match algorithm {
        Algorithm::Myers => ctx.myers(0..ia.len(), 0..ib.len()),
        Algorithm::Patience => ctx.patience(0..ia.len(), 0..ib.len()),
        Algorithm::Histogram => ctx.histogram(0..ia.len(), 0..ib.len()),
    }
    let mut matches = ctx.matches;
    matches.sort_unstable();
    hunks_from_matches(a.len(), b.len(), &matches)
}

/// Map elements of both sequences to dense ids (equal elements get equal ids).
fn intern<'t, T: Eq + Hash>(a: &'t [T], b: &'t [T]) -> (Vec<u32>, Vec<u32>) {
    let mut ids: HashMap<&'t T, u32> = HashMap::with_capacity(a.len() + b.len());
    let mut id_of = |x: &'t T| -> u32 {
        let next = ids.len() as u32;
        *ids.entry(x).or_insert(next)
    };
    let ia: Vec<u32> = a.iter().map(&mut id_of).collect();
    let ib: Vec<u32> = b.iter().map(&mut id_of).collect();
    (ia, ib)
}

/// Build hunks from sorted, strictly increasing matched pairs `(ai, bi)`.
pub(crate) fn hunks_from_matches(n: usize, m: usize, matches: &[(usize, usize)]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let (mut ai, mut bi) = (0usize, 0usize);
    let push_change = |out: &mut Vec<Hunk>, a: Range<usize>, b: Range<usize>| {
        let kind = match (a.is_empty(), b.is_empty()) {
            (true, true) => return,
            (true, false) => HunkKind::Insert,
            (false, true) => HunkKind::Delete,
            (false, false) => HunkKind::Replace,
        };
        out.push(Hunk::new(kind, a, b));
    };
    for &(x, y) in matches {
        if x < ai || y < bi {
            // Not strictly increasing; ignore defensively (cannot happen with valid algorithms).
            continue;
        }
        push_change(&mut out, ai..x, bi..y);
        match out.last_mut() {
            Some(h) if h.kind == HunkKind::Equal && h.a.end == x && h.b.end == y => {
                h.a.end = x + 1;
                h.b.end = y + 1;
            }
            _ => out.push(Hunk::new(HunkKind::Equal, x..x + 1, y..y + 1)),
        }
        ai = x + 1;
        bi = y + 1;
    }
    push_change(&mut out, ai..n, bi..m);
    out
}

struct Ctx<'s> {
    a: &'s [u32],
    b: &'s [u32],
    matches: Vec<(usize, usize)>,
    budget: u64,
}

impl Ctx<'_> {
    /// Strip the common prefix and suffix of the two ranges, recording them as matches.
    fn trim(&mut self, mut ra: Range<usize>, mut rb: Range<usize>) -> (Range<usize>, Range<usize>) {
        while ra.start < ra.end && rb.start < rb.end && self.a.get(ra.start) == self.b.get(rb.start) {
            self.matches.push((ra.start, rb.start));
            ra.start += 1;
            rb.start += 1;
        }
        while ra.start < ra.end && rb.start < rb.end && self.a.get(ra.end - 1) == self.b.get(rb.end - 1) {
            self.matches.push((ra.end - 1, rb.end - 1));
            ra.end -= 1;
            rb.end -= 1;
        }
        (ra, rb)
    }

    fn myers(&mut self, ra: Range<usize>, rb: Range<usize>) {
        let mut stack = vec![(ra, rb)];
        while let Some((ra, rb)) = stack.pop() {
            let (ra, rb) = self.trim(ra, rb);
            if ra.is_empty() || rb.is_empty() {
                continue;
            }
            let Some((x, y)) = self.middle_snake(ra.clone(), rb.clone()) else {
                continue; // over budget: leave the sub-problem as a replacement
            };
            // Guard against a split that would not make progress.
            if (x == ra.start && y == rb.start) || (x == ra.end && y == rb.end) {
                continue;
            }
            stack.push((ra.start..x, rb.start..y));
            stack.push((x..ra.end, y..rb.end));
        }
    }

    /// Find a point on an optimal edit path through the middle of the edit graph
    /// (Myers' "middle snake", in the formulation used by diff-match-patch's bisect).
    /// Returns absolute coordinates.
    fn middle_snake(&mut self, ra: Range<usize>, rb: Range<usize>) -> Option<(usize, usize)> {
        let a = self.a.get(ra.clone())?;
        let b = self.b.get(rb.clone())?;
        let n = a.len() as isize;
        let m = b.len() as isize;
        let max_d = (n + m + 1) / 2;
        let off = max_d + 1;
        let width = (2 * off + 1) as usize;
        let mut vf = vec![-1isize; width];
        let mut vb = vec![-1isize; width];
        let idx = |k: isize| (k + off) as usize;
        *vf.get_mut(idx(1))? = 0;
        *vb.get_mut(idx(1))? = 0;
        let delta = n - m;
        let front = delta % 2 != 0;
        // Bounds for k so we never walk off the grid (dmp's k1start/k1end trick).
        let (mut k1s, mut k1e, mut k2s, mut k2e) = (0isize, 0isize, 0isize, 0isize);
        for d in 0..max_d {
            // Forward pass.
            let mut k1 = -d + k1s;
            while k1 <= d - k1e {
                let mut x =
                    if k1 == -d || (k1 != d && vf.get(idx(k1 - 1))? < vf.get(idx(k1 + 1))?) { *vf.get(idx(k1 + 1))? } else { *vf.get(idx(k1 - 1))? + 1 };
                let mut y = x - k1;
                let mut steps = 0u64;
                while x < n && y < m && a.get(x as usize) == b.get(y as usize) {
                    x += 1;
                    y += 1;
                    steps += 1;
                }
                self.budget = self.budget.checked_sub(steps + 1)?;
                *vf.get_mut(idx(k1))? = x;
                if x > n {
                    k1e += 2;
                } else if y > m {
                    k1s += 2;
                } else if front {
                    let k2 = delta - k1;
                    if k2 >= -max_d && k2 <= max_d {
                        let x2 = *vb.get(idx(k2))?;
                        if x2 != -1 && x >= n - x2 {
                            return Some((ra.start + x as usize, rb.start + y as usize));
                        }
                    }
                }
                k1 += 2;
            }
            // Backward pass (coordinates measured from the end).
            let mut k2 = -d + k2s;
            while k2 <= d - k2e {
                let mut x2 =
                    if k2 == -d || (k2 != d && vb.get(idx(k2 - 1))? < vb.get(idx(k2 + 1))?) { *vb.get(idx(k2 + 1))? } else { *vb.get(idx(k2 - 1))? + 1 };
                let mut y2 = x2 - k2;
                let mut steps = 0u64;
                while x2 < n && y2 < m && a.get((n - x2 - 1) as usize) == b.get((m - y2 - 1) as usize) {
                    x2 += 1;
                    y2 += 1;
                    steps += 1;
                }
                self.budget = self.budget.checked_sub(steps + 1)?;
                *vb.get_mut(idx(k2))? = x2;
                if x2 > n {
                    k2e += 2;
                } else if y2 > m {
                    k2s += 2;
                } else if !front {
                    let k1 = delta - k2;
                    if k1 >= -max_d && k1 <= max_d {
                        let x1 = *vf.get(idx(k1))?;
                        if x1 != -1 {
                            let y1 = x1 - k1;
                            if x1 >= n - x2 {
                                return Some((ra.start + x1 as usize, rb.start + y1 as usize));
                            }
                        }
                    }
                }
                k2 += 2;
            }
        }
        None
    }

    fn patience(&mut self, ra: Range<usize>, rb: Range<usize>) {
        let mut stack = vec![(ra, rb)];
        while let Some((ra, rb)) = stack.pop() {
            let (ra, rb) = self.trim(ra, rb);
            if ra.is_empty() || rb.is_empty() {
                continue;
            }
            let anchors = self.unique_lcs(ra.clone(), rb.clone());
            if anchors.is_empty() {
                self.myers(ra, rb);
                continue;
            }
            let (mut pa, mut pb) = (ra.start, rb.start);
            for &(x, y) in &anchors {
                stack.push((pa..x, pb..y));
                self.matches.push((x, y));
                pa = x + 1;
                pb = y + 1;
            }
            stack.push((pa..ra.end, pb..rb.end));
        }
    }

    /// Longest increasing run of elements that occur exactly once in both ranges.
    fn unique_lcs(&self, ra: Range<usize>, rb: Range<usize>) -> Vec<(usize, usize)> {
        // id -> (count in a, position in a, count in b, position in b)
        let mut seen: HashMap<u32, (u32, usize, u32, usize)> = HashMap::new();
        for i in ra.clone() {
            if let Some(&id) = self.a.get(i) {
                let e = seen.entry(id).or_insert((0, i, 0, 0));
                e.0 += 1;
                e.1 = i;
            }
        }
        for j in rb.clone() {
            if let Some(&id) = self.b.get(j)
                && let Some(e) = seen.get_mut(&id)
            {
                e.2 += 1;
                e.3 = j;
            }
        }
        let mut pairs: Vec<(usize, usize)> = seen.values().filter(|e| e.0 == 1 && e.2 == 1).map(|e| (e.1, e.3)).collect();
        pairs.sort_unstable();
        longest_increasing_by_second(&pairs)
    }

    fn histogram(&mut self, ra: Range<usize>, rb: Range<usize>) {
        const MAX_CHAIN: usize = 64;
        let mut stack = vec![(ra, rb)];
        while let Some((ra, rb)) = stack.pop() {
            let (ra, rb) = self.trim(ra, rb);
            if ra.is_empty() || rb.is_empty() {
                continue;
            }
            // Occurrences of each element in the left range (capped).
            let mut occ: HashMap<u32, Vec<usize>> = HashMap::new();
            for i in ra.clone() {
                if let Some(&id) = self.a.get(i) {
                    let v = occ.entry(id).or_default();
                    if v.len() <= MAX_CHAIN {
                        v.push(i);
                    }
                }
            }
            // best = (count, start a, start b, len)
            let mut best: Option<(usize, usize, usize, usize)> = None;
            let mut j = rb.start;
            while j < rb.end {
                let Some(id) = self.b.get(j) else { break };
                let mut next_j = j + 1;
                if let Some(pos) = occ.get(id)
                    && pos.len() <= MAX_CHAIN
                {
                    let count = pos.len();
                    if best.is_none_or(|b| count <= b.0) {
                        for &i in pos {
                            // Extend the match in both directions inside the ranges.
                            let (mut s_a, mut s_b) = (i, j);
                            while s_a > ra.start && s_b > rb.start && self.a.get(s_a - 1) == self.b.get(s_b - 1) {
                                s_a -= 1;
                                s_b -= 1;
                            }
                            let (mut e_a, mut e_b) = (i + 1, j + 1);
                            while e_a < ra.end && e_b < rb.end && self.a.get(e_a) == self.b.get(e_b) {
                                e_a += 1;
                                e_b += 1;
                            }
                            let len = e_a - s_a;
                            let better = match best {
                                None => true,
                                Some((bc, _, _, bl)) => count < bc || (count == bc && len > bl),
                            };
                            if better {
                                best = Some((count, s_a, s_b, len));
                            }
                            next_j = next_j.max(e_b);
                        }
                    }
                }
                j = next_j;
            }
            match best {
                None => self.myers(ra, rb),
                Some((_, sa, sb, len)) => {
                    for t in 0..len {
                        self.matches.push((sa + t, sb + t));
                    }
                    stack.push((ra.start..sa, rb.start..sb));
                    stack.push((sa + len..ra.end, sb + len..rb.end));
                }
            }
        }
    }
}

/// Patience sorting: the longest subsequence of `pairs` (sorted by `.0`) whose `.1` increases.
fn longest_increasing_by_second(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // tails[k] = index into pairs of the smallest tail of an increasing run of length k+1
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for (i, &(_, y)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs.get(t).is_some_and(|p| p.1 < y));
        if pos > 0
            && let Some(p) = prev.get_mut(i)
        {
            *p = tails.get(pos - 1).copied();
        }
        if pos == tails.len() {
            tails.push(i);
        } else if let Some(t) = tails.get_mut(pos) {
            *t = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        if let Some(&p) = pairs.get(i) {
            out.push(p);
        }
        cur = prev.get(i).copied().flatten();
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Rebuild `b` from `a` and the hunks; also checks equal hunks really are equal.
    fn apply(a: &[u8], b: &[u8], hunks: &[Hunk]) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut pa, mut pb) = (0, 0);
        for h in hunks {
            assert_eq!(h.a.start, pa, "hunks must be contiguous on a");
            assert_eq!(h.b.start, pb, "hunks must be contiguous on b");
            match h.kind {
                HunkKind::Equal => {
                    assert_eq!(&a[h.a.clone()], &b[h.b.clone()]);
                    out.extend_from_slice(&a[h.a.clone()]);
                }
                HunkKind::Insert => {
                    assert!(h.a.is_empty() && !h.b.is_empty());
                    out.extend_from_slice(&b[h.b.clone()]);
                }
                HunkKind::Delete => assert!(!h.a.is_empty() && h.b.is_empty()),
                HunkKind::Replace => {
                    assert!(!h.a.is_empty() && !h.b.is_empty());
                    out.extend_from_slice(&b[h.b.clone()]);
                }
            }
            pa = h.a.end;
            pb = h.b.end;
        }
        assert_eq!(pa, a.len());
        assert_eq!(pb, b.len());
        out
    }

    fn edit_cost(h: &[Hunk]) -> usize {
        h.iter().filter(|h| h.is_change()).map(|h| h.a.len() + h.b.len()).sum()
    }

    #[test]
    fn basic_cases() {
        for alg in Algorithm::ALL {
            assert!(diff::<u8>(&[], &[], alg).is_empty());
            let h = diff(b"abc", b"abc", alg);
            assert_eq!(h, vec![Hunk::new(HunkKind::Equal, 0..3, 0..3)]);
            let h = diff(b"", b"xy", alg);
            assert_eq!(h, vec![Hunk::new(HunkKind::Insert, 0..0, 0..2)]);
            let h = diff(b"xy", b"", alg);
            assert_eq!(h, vec![Hunk::new(HunkKind::Delete, 0..2, 0..0)]);
            let h = diff(b"abXcd", b"abYcd", alg);
            assert_eq!(h[1], Hunk::new(HunkKind::Replace, 2..3, 2..3));
        }
    }

    #[test]
    fn myers_is_minimal_on_classic_example() {
        // Myers' paper example: ABCABBA -> CBABAC has D = 5.
        let h = diff(b"ABCABBA", b"CBABAC", Algorithm::Myers);
        assert_eq!(edit_cost(&h), 5);
    }

    #[test]
    fn budget_exhaustion_still_produces_valid_script() {
        let a: Vec<u8> = (0..2000u32).map(|i| (i * 7 % 13) as u8).collect();
        let b: Vec<u8> = (0..2000u32).map(|i| (i * 5 % 11) as u8).collect();
        let h = diff_with_budget(&a, &b, Algorithm::Myers, 1000);
        assert_eq!(apply(&a, &b, &h), b);
    }

    #[test]
    fn patience_prefers_unique_anchors() {
        let a = ["fn a() {", "}", "", "fn b() {", "}"];
        let b = ["fn a() {", "}", "", "fn c() {", "}", "", "fn b() {", "}"];
        let h = diff(&a, &b, Algorithm::Patience);
        let inserted: usize = h.iter().filter(|h| h.kind == HunkKind::Insert).map(|h| h.b.len()).sum();
        assert_eq!(inserted, 3);
    }

    proptest! {
        #[test]
        fn hunks_reconstruct_b(a in proptest::collection::vec(0u8..6, 0..80), b in proptest::collection::vec(0u8..6, 0..80)) {
            for alg in Algorithm::ALL {
                let h = diff(&a, &b, alg);
                prop_assert_eq!(apply(&a, &b, &h), b.clone());
            }
        }

        #[test]
        fn myers_is_never_worse_than_others(a in proptest::collection::vec(0u8..4, 0..60), b in proptest::collection::vec(0u8..4, 0..60)) {
            let m = edit_cost(&diff(&a, &b, Algorithm::Myers));
            prop_assert!(m <= edit_cost(&diff(&a, &b, Algorithm::Patience)));
            prop_assert!(m <= edit_cost(&diff(&a, &b, Algorithm::Histogram)));
        }
    }
}
