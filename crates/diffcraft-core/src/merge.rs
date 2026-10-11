//! Three-way merge (diff3): combine the changes made on the left and on the right relative to a
//! common base, and report the places where both sides changed the same lines differently.

use std::ops::Range;

use crate::Result;
use crate::text::{self, TextCompareOptions};

/// What happened to a region of the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkKind {
    /// Neither side changed it.
    Unchanged,
    /// Only the left side changed it.
    LeftChange,
    /// Only the right side changed it.
    RightChange,
    /// Both sides made the identical change.
    BothSame,
    /// Both sides changed it differently.
    Conflict,
}

/// A region of the merge, as line ranges in each input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeChunk {
    pub kind: ChunkKind,
    pub base: Range<usize>,
    pub left: Range<usize>,
    pub right: Range<usize>,
}

/// How a chunk is resolved in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Conflict not decided yet: written with conflict markers.
    Unresolved,
    Left,
    Right,
    Base,
    LeftThenRight,
    RightThenLeft,
    /// Hand-edited text.
    Custom(Vec<String>),
}

impl Resolution {
    pub fn label(&self) -> &'static str {
        match self {
            Resolution::Unresolved => "Unresolved",
            Resolution::Left => "Left",
            Resolution::Right => "Right",
            Resolution::Base => "Base",
            Resolution::LeftThenRight => "Left then right",
            Resolution::RightThenLeft => "Right then left",
            Resolution::Custom(_) => "Edited",
        }
    }
}

/// The result of [`merge3`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merge3 {
    pub chunks: Vec<MergeChunk>,
}

impl Merge3 {
    pub fn conflict_count(&self) -> usize {
        self.chunks.iter().filter(|c| c.kind == ChunkKind::Conflict).count()
    }

    /// The automatic resolution for each chunk (conflicts stay unresolved).
    pub fn default_resolutions(&self) -> Vec<Resolution> {
        self.chunks.iter().map(|c| default_resolution(c.kind)).collect()
    }
}

pub fn default_resolution(kind: ChunkKind) -> Resolution {
    match kind {
        ChunkKind::Unchanged => Resolution::Base,
        ChunkKind::LeftChange | ChunkKind::BothSame => Resolution::Left,
        ChunkKind::RightChange => Resolution::Right,
        ChunkKind::Conflict => Resolution::Unresolved,
    }
}

/// Labels written into conflict markers.
#[derive(Clone, Debug)]
pub struct MarkerLabels {
    pub left: String,
    pub base: String,
    pub right: String,
}

impl Default for MarkerLabels {
    fn default() -> Self {
        MarkerLabels { left: "left".into(), base: "base".into(), right: "right".into() }
    }
}

/// Merge `left` and `right`, both derived from `base`. Only `opts.algorithm` is used:
/// a merge must be exact, so whitespace and case rules do not apply.
pub fn merge3<S: AsRef<str>>(base: &[S], left: &[S], right: &[S], opts: &TextCompareOptions) -> Result<Merge3> {
    let exact = TextCompareOptions { algorithm: opts.algorithm, ..TextCompareOptions::default() };
    let changes = |other: &[S]| -> Result<Vec<(Range<usize>, Range<usize>)>> {
        Ok(text::compare_lines(base, other, &exact)?.hunks.into_iter().filter(|h| h.is_change()).map(|h| (h.a, h.b)).collect())
    };
    let cl = changes(left)?;
    let cr = changes(right)?;

    let mut chunks = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut base_pos = 0usize;
    // Offsets (other position - base position) valid in unchanged regions.
    let (mut dl, mut dr) = (0isize, 0isize);
    let at = |pos: usize, d: isize| (pos as isize + d).max(0) as usize;

    loop {
        let next_l = cl.get(i).map(|c| c.0.start);
        let next_r = cr.get(j).map(|c| c.0.start);
        let lo = match (next_l, next_r) {
            (None, None) => break,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (Some(a), Some(b)) => a.min(b),
        };
        let mut hi = lo;
        let (mut took_l, mut took_r) = (false, false);
        let (mut sum_l, mut sum_r) = (0isize, 0isize);
        // A change joins the group when it starts inside or right at the end of it
        // (touching changes from both sides conflict, as in git).
        let joins = |b: &Range<usize>, hi: usize, any: bool| if any { b.start <= hi } else { b.start == lo };
        loop {
            let mut grew = false;
            if let Some((b, o)) = cl.get(i)
                && joins(b, hi, took_l || took_r)
            {
                hi = hi.max(b.end);
                sum_l += o.len() as isize - b.len() as isize;
                took_l = true;
                i += 1;
                grew = true;
            }
            if let Some((b, o)) = cr.get(j)
                && joins(b, hi, took_l || took_r)
            {
                hi = hi.max(b.end);
                sum_r += o.len() as isize - b.len() as isize;
                took_r = true;
                j += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        if base_pos < lo {
            chunks.push(MergeChunk { kind: ChunkKind::Unchanged, base: base_pos..lo, left: at(base_pos, dl)..at(lo, dl), right: at(base_pos, dr)..at(lo, dr) });
        }
        let left_r = at(lo, dl)..at(hi, dl + sum_l);
        let right_r = at(lo, dr)..at(hi, dr + sum_r);
        let kind = match (took_l, took_r) {
            (true, false) => ChunkKind::LeftChange,
            (false, true) => ChunkKind::RightChange,
            _ => {
                let same = match (left.get(left_r.clone()), right.get(right_r.clone())) {
                    (Some(x), Some(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.as_ref() == q.as_ref()),
                    _ => false,
                };
                if same { ChunkKind::BothSame } else { ChunkKind::Conflict }
            }
        };
        chunks.push(MergeChunk { kind, base: lo..hi, left: left_r, right: right_r });
        dl += sum_l;
        dr += sum_r;
        base_pos = hi;
    }
    if base_pos < base.len() {
        chunks.push(MergeChunk {
            kind: ChunkKind::Unchanged,
            base: base_pos..base.len(),
            left: at(base_pos, dl)..left.len(),
            right: at(base_pos, dr)..right.len(),
        });
    }
    Ok(Merge3 { chunks })
}

fn lines_of<S: AsRef<str>>(src: &[S], r: &Range<usize>) -> Vec<String> {
    src.get(r.clone()).map(|s| s.iter().map(|l| l.as_ref().to_owned()).collect()).unwrap_or_default()
}

/// The output lines of one chunk under `res`.
pub fn render_chunk<S: AsRef<str>>(chunk: &MergeChunk, res: &Resolution, base: &[S], left: &[S], right: &[S], labels: &MarkerLabels) -> Vec<String> {
    match res {
        Resolution::Base => lines_of(base, &chunk.base),
        Resolution::Left => lines_of(left, &chunk.left),
        Resolution::Right => lines_of(right, &chunk.right),
        Resolution::LeftThenRight => {
            let mut v = lines_of(left, &chunk.left);
            v.extend(lines_of(right, &chunk.right));
            v
        }
        Resolution::RightThenLeft => {
            let mut v = lines_of(right, &chunk.right);
            v.extend(lines_of(left, &chunk.left));
            v
        }
        Resolution::Custom(v) => v.clone(),
        Resolution::Unresolved => {
            let mut v = vec![format!("<<<<<<< {}", labels.left)];
            v.extend(lines_of(left, &chunk.left));
            v.push(format!("||||||| {}", labels.base));
            v.extend(lines_of(base, &chunk.base));
            v.push("=======".to_owned());
            v.extend(lines_of(right, &chunk.right));
            v.push(format!(">>>>>>> {}", labels.right));
            v
        }
    }
}

/// All output lines.
pub fn render<S: AsRef<str>>(merge: &Merge3, resolutions: &[Resolution], base: &[S], left: &[S], right: &[S], labels: &MarkerLabels) -> Vec<String> {
    let mut out = Vec::new();
    for (k, c) in merge.chunks.iter().enumerate() {
        let fallback = default_resolution(c.kind);
        let res = resolutions.get(k).unwrap_or(&fallback);
        out.extend(render_chunk(c, res, base, left, right, labels));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Vec<String> {
        s.split(',').filter(|x| !x.is_empty()).map(str::to_owned).collect()
    }

    fn merged(base: &str, left: &str, right: &str) -> (Vec<String>, usize) {
        let (b, lf, rt) = (l(base), l(left), l(right));
        let m = merge3(&b, &lf, &rt, &TextCompareOptions::default()).unwrap();
        let out = render(&m, &m.default_resolutions(), &b, &lf, &rt, &MarkerLabels::default());
        (out, m.conflict_count())
    }

    #[test]
    fn non_overlapping_changes_merge_cleanly() {
        let (out, c) = merged("a,b,c,d,e,f", "A,b,c,d,e,f", "a,b,c,d,e,F");
        assert_eq!(c, 0);
        assert_eq!(out, l("A,b,c,d,e,F"));
    }

    #[test]
    fn insertions_and_deletions() {
        let (out, c) = merged("a,b,c,d,e", "a,c,d,e", "a,b,c,d,x,e");
        assert_eq!(c, 0);
        assert_eq!(out, l("a,c,d,x,e"));
    }

    #[test]
    fn identical_changes_are_not_conflicts() {
        let (out, c) = merged("a,b,c", "a,X,c", "a,X,c");
        assert_eq!(c, 0);
        assert_eq!(out, l("a,X,c"));
    }

    #[test]
    fn overlapping_changes_conflict() {
        let (out, c) = merged("a,b,c", "a,L,c", "a,R,c");
        assert_eq!(c, 1);
        assert_eq!(out, l("a,<<<<<<< left,L,||||||| base,b,=======,R,>>>>>>> right,c"));
    }

    #[test]
    fn resolutions() {
        let (b, lf, rt) = (l("a,b,c"), l("a,L,c"), l("a,R,c"));
        let m = merge3(&b, &lf, &rt, &TextCompareOptions::default()).unwrap();
        let k = m.chunks.iter().position(|c| c.kind == ChunkKind::Conflict).unwrap();
        let mut res = m.default_resolutions();
        res[k] = Resolution::RightThenLeft;
        assert_eq!(render(&m, &res, &b, &lf, &rt, &MarkerLabels::default()), l("a,R,L,c"));
        res[k] = Resolution::Custom(vec!["Z".into()]);
        assert_eq!(render(&m, &res, &b, &lf, &rt, &MarkerLabels::default()), l("a,Z,c"));
    }

    #[test]
    fn empty_inputs() {
        let (out, c) = merged("", "", "x");
        assert_eq!((out, c), (l("x"), 0));
        let (out, c) = merged("", "y", "x");
        assert_eq!(c, 1);
        assert!(out[0].starts_with("<<<<<<<"));
    }

    #[test]
    fn chunks_cover_all_inputs() {
        let (b, lf, rt) = (l("1,2,3,4,5,6,7,8"), l("1,x,3,4,6,7,y,8,9"), l("0,1,2,3,z,5,6,8"));
        let m = merge3(&b, &lf, &rt, &TextCompareOptions::default()).unwrap();
        let (mut pb, mut pl, mut pr) = (0, 0, 0);
        for c in &m.chunks {
            assert_eq!((c.base.start, c.left.start, c.right.start), (pb, pl, pr), "{:?}", m.chunks);
            pb = c.base.end;
            pl = c.left.end;
            pr = c.right.end;
        }
        assert_eq!((pb, pl, pr), (b.len(), lf.len(), rt.len()));
    }
}
