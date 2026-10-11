//! Byte-level comparison and the row layout used by the hex view.

use std::ops::Range;
use std::path::Path;

use crate::text::diff::{self, Algorithm, Hunk, HunkKind};
use crate::{Error, Result};

/// Largest file the hex view loads.
pub const MAX_BINARY_BYTES: u64 = 1024 * 1024 * 1024;
/// Above this combined size, comparison is positional (byte N against byte N) instead of aligned.
pub const MAX_ALIGNED_BYTES: usize = 16 * 1024 * 1024;

pub fn load(path: &Path) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path).map_err(|e| Error::io(path, e))?.len();
    if len > MAX_BINARY_BYTES {
        return Err(Error::TooLarge { path: path.to_path_buf(), size: len, limit: MAX_BINARY_BYTES });
    }
    std::fs::read(path).map_err(|e| Error::io(path, e))
}

/// Result of a byte comparison: contiguous segments covering both inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BinaryDiff {
    pub segments: Vec<Hunk>,
    /// Inserted/deleted bytes were aligned (otherwise byte N was compared with byte N).
    pub aligned: bool,
}

impl BinaryDiff {
    pub fn is_identical(&self) -> bool {
        self.segments.iter().all(|s| !s.is_change())
    }

    /// Number of bytes that differ (counted on the longer side of each change).
    pub fn differing_bytes(&self) -> usize {
        self.segments.iter().filter(|s| s.is_change()).map(|s| s.a.len().max(s.b.len())).sum()
    }
}

/// Compare two byte buffers. With `align`, insertions and deletions are found (Myers on bytes,
/// within an operation budget); otherwise, or for large inputs, comparison is positional.
pub fn compare(a: &[u8], b: &[u8], align: bool) -> BinaryDiff {
    if align && a.len().saturating_add(b.len()) <= MAX_ALIGNED_BYTES {
        let segments = diff::diff_with_budget(a, b, Algorithm::Myers, 40_000_000);
        return BinaryDiff { segments, aligned: true };
    }
    BinaryDiff { segments: positional(a, b), aligned: false }
}

fn positional(a: &[u8], b: &[u8]) -> Vec<Hunk> {
    let n = a.len().min(b.len());
    let mut out: Vec<Hunk> = Vec::new();
    let mut i = 0usize;
    // Compare in 64-byte blocks for speed, then refine to bytes inside differing blocks.
    while i < n {
        let end = (i + 64).min(n);
        let (Some(x), Some(y)) = (a.get(i..end), b.get(i..end)) else { break };
        if x == y {
            extend(&mut out, HunkKind::Equal, i..end, i..end);
        } else {
            for (k, (p, q)) in x.iter().zip(y).enumerate() {
                let kind = if p == q { HunkKind::Equal } else { HunkKind::Replace };
                extend(&mut out, kind, i + k..i + k + 1, i + k..i + k + 1);
            }
        }
        i = end;
    }
    if a.len() > n {
        out.push(Hunk::new(HunkKind::Delete, n..a.len(), b.len()..b.len()));
    } else if b.len() > n {
        out.push(Hunk::new(HunkKind::Insert, a.len()..a.len(), n..b.len()));
    }
    out
}

fn extend(out: &mut Vec<Hunk>, kind: HunkKind, a: Range<usize>, b: Range<usize>) {
    match out.last_mut() {
        Some(h) if h.kind == kind && h.a.end == a.start && h.b.end == b.start => {
            h.a.end = a.end;
            h.b.end = b.end;
        }
        _ => out.push(Hunk::new(kind, a, b)),
    }
}

/// One row of the side-by-side hex view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexRow {
    pub segment: usize,
    pub kind: HunkKind,
    pub left: Option<Range<usize>>,
    pub right: Option<Range<usize>>,
}

/// Maps view rows to byte ranges without materialising every row.
#[derive(Clone, Debug, Default)]
pub struct HexLayout {
    pub bytes_per_row: usize,
    /// First row of each segment, plus a final total.
    starts: Vec<usize>,
    segments: Vec<Hunk>,
}

impl HexLayout {
    pub fn new(diff: &BinaryDiff, bytes_per_row: usize) -> Self {
        let bpr = bytes_per_row.clamp(1, 64);
        let mut starts = Vec::with_capacity(diff.segments.len() + 1);
        let mut row = 0usize;
        for s in &diff.segments {
            starts.push(row);
            row += s.a.len().max(s.b.len()).div_ceil(bpr);
        }
        starts.push(row);
        HexLayout { bytes_per_row: bpr, starts, segments: diff.segments.clone() }
    }

    pub fn row_count(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    pub fn row(&self, i: usize) -> Option<HexRow> {
        if i >= self.row_count() {
            return None;
        }
        let seg = self.starts.partition_point(|&s| s <= i).checked_sub(1)?;
        let h = self.segments.get(seg)?;
        let off = (i - self.starts.get(seg)?) * self.bytes_per_row;
        let side = |r: &Range<usize>| {
            let s = r.start + off;
            (s < r.end).then(|| s..(s + self.bytes_per_row).min(r.end))
        };
        Some(HexRow { segment: seg, kind: h.kind, left: side(&h.a), right: side(&h.b) })
    }

    /// First row of every changed segment, for next/previous-difference navigation.
    pub fn difference_rows(&self) -> Vec<usize> {
        self.segments.iter().enumerate().filter(|(_, s)| s.is_change()).filter_map(|(k, _)| self.starts.get(k).copied()).collect()
    }

    /// Row showing byte `offset` of the left (or right) side.
    pub fn row_of_offset(&self, offset: usize, right: bool) -> Option<usize> {
        for (k, s) in self.segments.iter().enumerate() {
            let r = if right { &s.b } else { &s.a };
            if r.contains(&offset) {
                return Some(self.starts.get(k)? + (offset - r.start) / self.bytes_per_row);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_diff() {
        let d = compare(b"abcdef", b"abXdefgh", false);
        assert!(!d.aligned);
        assert_eq!(d.segments[1], Hunk::new(HunkKind::Replace, 2..3, 2..3));
        assert_eq!(d.segments.last().unwrap().kind, HunkKind::Insert);
        assert_eq!(d.differing_bytes(), 3);
    }

    #[test]
    fn aligned_diff_finds_insertion() {
        let a: Vec<u8> = (0..=255u8).collect();
        let mut b = a.clone();
        b.insert(100, 0xAA);
        let d = compare(&a, &b, true);
        assert!(d.aligned);
        assert_eq!(d.differing_bytes(), 1);
    }

    #[test]
    fn layout_rows() {
        let d = compare(&[0u8; 40], &[0u8; 40], false);
        let l = HexLayout::new(&d, 16);
        assert_eq!(l.row_count(), 3);
        assert_eq!(l.row(2).unwrap().left, Some(32..40));
        assert!(l.row(3).is_none());
        assert_eq!(l.row_of_offset(33, false), Some(2));

        let d = compare(b"0123456789", b"01234X6789ABCDEFGHIJ", false);
        let l = HexLayout::new(&d, 4);
        let rows: Vec<HexRow> = (0..l.row_count()).map(|i| l.row(i).unwrap()).collect();
        assert!(rows.iter().any(|r| r.left.is_none() && r.right.is_some()));
        assert_eq!(l.difference_rows().len(), 2);
    }

    #[test]
    fn empty_inputs() {
        let d = compare(b"", b"", true);
        assert!(d.is_identical());
        assert_eq!(HexLayout::new(&d, 16).row_count(), 0);
    }
}
