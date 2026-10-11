//! Folder synchronisation: turn a comparison into a list of file operations, preview it,
//! then run it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use super::{FolderComparison, Progress, Status};
use crate::fsutil;

/// Synchronisation direction and style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SyncMode {
    /// Copy newer and left-only items to the right. Never deletes.
    #[default]
    UpdateRight,
    /// Copy newer and right-only items to the left. Never deletes.
    UpdateLeft,
    /// Copy newer items in both directions and orphans to the other side. Never deletes.
    UpdateBoth,
    /// Make the right identical to the left (copies and deletes on the right).
    MirrorRight,
    /// Make the left identical to the right (copies and deletes on the left).
    MirrorLeft,
}

impl SyncMode {
    pub const ALL: [SyncMode; 5] = [SyncMode::UpdateRight, SyncMode::UpdateLeft, SyncMode::UpdateBoth, SyncMode::MirrorRight, SyncMode::MirrorLeft];

    pub fn label(self) -> &'static str {
        match self {
            SyncMode::UpdateRight => "Update right (left → right)",
            SyncMode::UpdateLeft => "Update left (right → left)",
            SyncMode::UpdateBoth => "Update both",
            SyncMode::MirrorRight => "Mirror left → right",
            SyncMode::MirrorLeft => "Mirror right → left",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SyncMode::UpdateRight => "Copies newer files and left-only files to the right. Nothing is deleted.",
            SyncMode::UpdateLeft => "Copies newer files and right-only files to the left. Nothing is deleted.",
            SyncMode::UpdateBoth => "Copies newer files and orphans in both directions. Nothing is deleted.",
            SyncMode::MirrorRight => "Makes the right side identical to the left: copies differences and deletes right-only items.",
            SyncMode::MirrorLeft => "Makes the left side identical to the right: copies differences and deletes left-only items.",
        }
    }
}

/// One planned operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncAction {
    /// Copy a file or a whole folder.
    Copy { from: PathBuf, to: PathBuf },
    /// Delete a file or folder.
    Delete { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncOp {
    pub action: SyncAction,
    /// Relative path (for display).
    pub rel: PathBuf,
    /// The operation changes the right side.
    pub to_right: bool,
    /// Bytes involved (for progress).
    pub bytes: u64,
}

impl SyncOp {
    pub fn verb(&self) -> &'static str {
        match (&self.action, self.to_right) {
            (SyncAction::Copy { .. }, true) => "Copy →",
            (SyncAction::Copy { .. }, false) => "← Copy",
            (SyncAction::Delete { .. }, true) => "Delete right",
            (SyncAction::Delete { .. }, false) => "Delete left",
        }
    }
}

/// Plan the operations for `mode` over the whole comparison.
pub fn plan(cmp: &FolderComparison, mode: SyncMode) -> Vec<SyncOp> {
    plan_subset(cmp, mode, &[0])
}

/// Plan for selected subtrees only (`roots` are node indices).
pub fn plan_subset(cmp: &FolderComparison, mode: SyncMode, roots: &[usize]) -> Vec<SyncOp> {
    let mut ops = Vec::new();
    // Depth-first, pre-order, keeping sibling order.
    let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
    let mut visited = vec![false; cmp.nodes.len()];
    while let Some(i) = stack.pop() {
        let Some(n) = cmp.nodes.get(i) else { continue };
        if visited.get(i).copied().unwrap_or(true) {
            continue;
        }
        if let Some(v) = visited.get_mut(i) {
            *v = true;
        }
        let size = |left: bool| subtree_bytes(cmp, i, left);
        let copy = |to_right: bool| -> Option<SyncOp> {
            let from = if to_right { cmp.left_path(i)? } else { cmp.right_path(i)? };
            let to = cmp.target_path(i, to_right)?;
            Some(SyncOp { action: SyncAction::Copy { from, to }, rel: n.rel.clone(), to_right, bytes: size(to_right) })
        };
        let delete = |on_right: bool| -> Option<SyncOp> {
            let path = if on_right { cmp.right_path(i)? } else { cmp.left_path(i)? };
            Some(SyncOp { action: SyncAction::Delete { path }, rel: n.rel.clone(), to_right: on_right, bytes: 0 })
        };
        let both_dirs = n.left.as_ref().is_some_and(|m| m.is_dir) && n.right.as_ref().is_some_and(|m| m.is_dir);
        if i == 0 || (both_dirs && n.status != Status::Same) {
            for &c in n.children.iter().rev() {
                stack.push(c);
            }
            continue;
        }
        if n.status == Status::Same {
            continue;
        }
        let mismatch = n.status == Status::Error && n.left.is_some() && n.right.is_some();
        let mut push = |op: Option<SyncOp>| ops.extend(op);
        match mode {
            SyncMode::UpdateRight | SyncMode::UpdateLeft | SyncMode::UpdateBoth => {
                let right_ok = mode != SyncMode::UpdateLeft;
                let left_ok = mode != SyncMode::UpdateRight;
                match n.status {
                    Status::LeftOnly if right_ok => push(copy(true)),
                    Status::RightOnly if left_ok => push(copy(false)),
                    Status::LeftNewer if right_ok => push(copy(true)),
                    Status::RightNewer if left_ok => push(copy(false)),
                    Status::Different => {
                        // Content differs: the newer side wins; equal times are left alone.
                        let (lt, rt) = (n.left.as_ref().and_then(|m| m.modified), n.right.as_ref().and_then(|m| m.modified));
                        match (lt, rt) {
                            (Some(a), Some(b)) if a > b && right_ok => push(copy(true)),
                            (Some(a), Some(b)) if b > a && left_ok => push(copy(false)),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            SyncMode::MirrorRight | SyncMode::MirrorLeft => {
                let to_right = mode == SyncMode::MirrorRight;
                let source_present = if to_right { n.left.is_some() } else { n.right.is_some() };
                let target_present = if to_right { n.right.is_some() } else { n.left.is_some() };
                if mismatch {
                    push(delete(to_right));
                    push(copy(to_right));
                } else if source_present {
                    push(copy(to_right));
                } else if target_present {
                    push(delete(to_right));
                }
            }
        }
    }
    ops
}

fn subtree_bytes(cmp: &FolderComparison, i: usize, left: bool) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![i];
    while let Some(k) = stack.pop() {
        let Some(n) = cmp.nodes.get(k) else { continue };
        let m = if left { n.left.as_ref() } else { n.right.as_ref() };
        if let Some(m) = m {
            total = total.saturating_add(m.size);
            stack.extend(n.children.iter().copied());
        }
    }
    total
}

/// A failed operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncFailure {
    pub op: usize,
    pub message: String,
}

/// Run the plan. Continues past failures and returns them; stops early when cancelled.
pub fn execute(ops: &[SyncOp], to_trash: bool, progress: &Progress) -> Vec<SyncFailure> {
    let mut failures = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        if progress.is_cancelled() {
            failures.push(SyncFailure { op: k, message: "cancelled".into() });
            break;
        }
        progress.set_current(op.rel.display().to_string());
        let res = match &op.action {
            SyncAction::Copy { from, to } => fsutil::copy_entry(from, to),
            SyncAction::Delete { path } => fsutil::delete_entry(path, to_trash),
        };
        if let Err(e) = res {
            failures.push(SyncFailure { op: k, message: e.to_string() });
        }
        progress.files_compared.fetch_add(1, Ordering::Relaxed);
        progress.bytes_done.fetch_add(op.bytes, Ordering::Relaxed);
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::super::tests::tree;
    use super::super::{CompareMode, FolderCompareOptions, compare_folders};
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("same.txt", "s"), ("newer.txt", "left new"), ("lonly/deep.txt", "1"), ("sub/x.txt", "x")]);
        tree(&r, &[("same.txt", "s"), ("newer.txt", "right old"), ("ronly.txt", "1"), ("sub/x.txt", "x")]);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        for p in [l.join("same.txt"), r.join("same.txt"), l.join("sub/x.txt"), r.join("sub/x.txt")] {
            fsutil::set_mtime(&p, t0);
        }
        fsutil::set_mtime(&r.join("newer.txt"), t0);
        fsutil::set_mtime(&l.join("newer.txt"), t0 + Duration::from_secs(100));
        (d, l, r)
    }

    fn cmp(l: &Path, r: &Path) -> FolderComparison {
        compare_folders(l, r, &FolderCompareOptions { mode: CompareMode::SizeAndTime, ..Default::default() }, &Progress::default()).unwrap()
    }

    #[test]
    fn update_right_plan_and_run() {
        let (_d, l, r) = setup();
        let c = cmp(&l, &r);
        let ops = plan(&c, SyncMode::UpdateRight);
        let rels: Vec<String> = ops.iter().map(|o| o.rel.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(rels, vec!["lonly", "newer.txt"]);
        assert!(ops.iter().all(|o| o.to_right && matches!(o.action, SyncAction::Copy { .. })));
        assert!(execute(&ops, false, &Progress::default()).is_empty());
        assert_eq!(fs::read_to_string(r.join("newer.txt")).unwrap(), "left new");
        assert!(r.join("lonly/deep.txt").exists());
        assert!(r.join("ronly.txt").exists(), "update never deletes");
    }

    #[test]
    fn mirror_makes_identical() {
        let (_d, l, r) = setup();
        let ops = plan(&cmp(&l, &r), SyncMode::MirrorRight);
        assert!(ops.iter().any(|o| matches!(o.action, SyncAction::Delete { .. })));
        assert!(execute(&ops, false, &Progress::default()).is_empty());
        let after = compare_folders(&l, &r, &FolderCompareOptions { mode: CompareMode::Binary, ..Default::default() }, &Progress::default()).unwrap();
        assert_eq!(after.nodes[0].status, Status::Same, "{:#?}", after.nodes);
    }

    #[test]
    fn update_left_and_both() {
        let (_d, l, r) = setup();
        let c = cmp(&l, &r);
        let left_ops = plan(&c, SyncMode::UpdateLeft);
        assert_eq!(left_ops.len(), 1);
        assert!(!left_ops[0].to_right);
        assert_eq!(plan(&c, SyncMode::UpdateBoth).len(), 3);
    }

    #[test]
    fn failures_are_reported() {
        let op = SyncOp { action: SyncAction::Copy { from: "/no/such/file".into(), to: "/no/such/target".into() }, rel: "x".into(), to_right: true, bytes: 0 };
        let f = execute(&[op], false, &Progress::default());
        assert_eq!(f.len(), 1);
    }
}
