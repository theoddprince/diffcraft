//! Folder comparison: scan two trees, pair entries by name, compare files by the chosen
//! criteria, and roll results up to folders.

pub mod filter;
pub mod sync;

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use filter::{CompiledFilter, Filter};

use crate::text::file::{TextFile, is_probably_binary};
use crate::text::{self, TextCompareOptions};
use crate::{Error, Result};

/// Maximum folder nesting followed (protects against symlink loops and absurd trees).
pub const MAX_DEPTH: usize = 256;

/// How file pairs are compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompareMode {
    /// Same size and modification time (fast; the default).
    #[default]
    SizeAndTime,
    Size,
    Timestamp,
    /// CRC-32 of the contents.
    Crc32,
    /// Byte-for-byte contents.
    Binary,
    /// Text files compared with the text rules (whitespace/case/patterns); others byte-for-byte.
    TextRules,
}

impl CompareMode {
    pub const ALL: [CompareMode; 6] =
        [CompareMode::SizeAndTime, CompareMode::Size, CompareMode::Timestamp, CompareMode::Crc32, CompareMode::Binary, CompareMode::TextRules];

    pub fn label(self) -> &'static str {
        match self {
            CompareMode::SizeAndTime => "Size & timestamp",
            CompareMode::Size => "Size only",
            CompareMode::Timestamp => "Timestamp only",
            CompareMode::Crc32 => "CRC-32",
            CompareMode::Binary => "Binary contents",
            CompareMode::TextRules => "Rules-based (text)",
        }
    }

    pub fn reads_contents(self) -> bool {
        matches!(self, CompareMode::Crc32 | CompareMode::Binary | CompareMode::TextRules)
    }
}

/// Options for a folder comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderCompareOptions {
    pub mode: CompareMode,
    /// Timestamps closer than this are equal (FAT and network shares round to 2 s).
    pub time_tolerance_secs: u32,
    /// Treat timestamps exactly one hour apart as equal (daylight-saving shifts on FAT).
    pub ignore_dst_shift: bool,
    /// Match names case-sensitively (default: Linux yes, Windows/macOS no).
    pub case_sensitive: bool,
    pub follow_symlinks: bool,
    pub filter: Filter,
    /// Rules used by [`CompareMode::TextRules`].
    pub text: TextCompareOptions,
}

impl Default for FolderCompareOptions {
    fn default() -> Self {
        FolderCompareOptions {
            mode: CompareMode::default(),
            time_tolerance_secs: 2,
            ignore_dst_shift: false,
            case_sensitive: !cfg!(any(windows, target_os = "macos")),
            follow_symlinks: false,
            filter: Filter { show_hidden: true, ..Filter::default() },
            text: TextCompareOptions::default(),
        }
    }
}

/// Metadata of one side of an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryMeta {
    /// Name as spelled on this side.
    pub name: OsString,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
}

impl EntryMeta {
    fn from_fs(name: OsString, meta: &fs::Metadata, is_symlink: bool) -> Self {
        EntryMeta {
            name,
            is_dir: meta.is_dir(),
            is_symlink,
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            readonly: meta.permissions().readonly(),
        }
    }
}

/// Result status of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    Same,
    Different,
    LeftNewer,
    RightNewer,
    LeftOnly,
    RightOnly,
    /// Could not be compared (unreadable, file vs folder...). See `Node::error`.
    Error,
}

impl Status {
    pub fn is_orphan(self) -> bool {
        matches!(self, Status::LeftOnly | Status::RightOnly)
    }

    pub fn is_difference(self) -> bool {
        !matches!(self, Status::Same)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Same => "Same",
            Status::Different => "Different",
            Status::LeftNewer => "Left newer",
            Status::RightNewer => "Right newer",
            Status::LeftOnly => "Left only",
            Status::RightOnly => "Right only",
            Status::Error => "Error",
        }
    }
}

/// One paired entry of the comparison tree.
#[derive(Clone, Debug)]
pub struct Node {
    /// Path relative to the roots (as spelled on the left when present).
    pub rel: PathBuf,
    pub left: Option<EntryMeta>,
    pub right: Option<EntryMeta>,
    pub status: Status,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    pub error: Option<String>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.left.as_ref().or(self.right.as_ref()).is_some_and(|m| m.is_dir)
    }

    pub fn name(&self) -> String {
        self.left.as_ref().or(self.right.as_ref()).map(|m| m.name.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Relative path as spelled on the right side.
    pub fn rel_right(&self, nodes: &[Node]) -> PathBuf {
        let mut parts: Vec<OsString> = Vec::new();
        let mut cur = Some(self);
        while let Some(n) = cur {
            if n.parent.is_none() {
                break;
            }
            let name = n.right.as_ref().or(n.left.as_ref()).map(|m| m.name.clone()).unwrap_or_default();
            parts.push(name);
            cur = n.parent.and_then(|p| nodes.get(p));
        }
        parts.iter().rev().collect()
    }
}

/// Live progress of a running comparison or sync.
#[derive(Debug, Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    pub items_scanned: AtomicU64,
    pub files_to_compare: AtomicU64,
    pub files_compared: AtomicU64,
    pub bytes_done: AtomicU64,
    pub current: Mutex<String>,
}

impl Progress {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn set_current(&self, s: impl Into<String>) {
        if let Ok(mut c) = self.current.lock() {
            *c = s.into();
        }
    }

    pub fn current(&self) -> String {
        self.current.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

/// Totals for status bars and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub same: usize,
    pub different: usize,
    pub left_only: usize,
    pub right_only: usize,
    pub errors: usize,
    pub folders: usize,
}

/// A finished folder comparison. `nodes[0]` is the root pair.
#[derive(Clone, Debug)]
pub struct FolderComparison {
    pub left_root: PathBuf,
    pub right_root: PathBuf,
    pub nodes: Vec<Node>,
    pub options: FolderCompareOptions,
}

impl FolderComparison {
    pub fn left_path(&self, idx: usize) -> Option<PathBuf> {
        let n = self.nodes.get(idx)?;
        n.left.as_ref()?;
        Some(self.left_root.join(&n.rel))
    }

    pub fn right_path(&self, idx: usize) -> Option<PathBuf> {
        let n = self.nodes.get(idx)?;
        n.right.as_ref()?;
        Some(self.right_root.join(n.rel_right(&self.nodes)))
    }

    /// Where a copy of the left entry would land on the right (and vice versa).
    pub fn target_path(&self, idx: usize, to_right: bool) -> Option<PathBuf> {
        let n = self.nodes.get(idx)?;
        Some(if to_right { self.right_root.join(n.rel_right(&self.nodes)) } else { self.left_root.join(&n.rel) })
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for n in self.nodes.iter().skip(1) {
            if n.is_dir() {
                s.folders += 1;
                continue;
            }
            match n.status {
                Status::Same => s.same += 1,
                Status::Different | Status::LeftNewer | Status::RightNewer => s.different += 1,
                Status::LeftOnly => s.left_only += 1,
                Status::RightOnly => s.right_only += 1,
                Status::Error => s.errors += 1,
            }
        }
        s
    }

    /// Re-read one file pair from disk and update its status and its ancestors'.
    pub fn refresh_file(&mut self, idx: usize) {
        let (lp, rp) = (self.left_root.join(self.nodes.get(idx).map(|n| n.rel.clone()).unwrap_or_default()), self.right_path_any(idx));
        let opts = self.options.clone();
        let stat = |p: &Path| fs::symlink_metadata(p).ok().map(|m| (p.file_name().map(OsString::from).unwrap_or_default(), m));
        let Some(node) = self.nodes.get_mut(idx) else { return };
        node.left = stat(&lp).map(|(name, m)| EntryMeta::from_fs(name, &m, m.file_type().is_symlink()));
        node.right = rp.as_deref().and_then(stat).map(|(name, m)| EntryMeta::from_fs(name, &m, m.file_type().is_symlink()));
        if node.left.is_none() && node.right.is_none() {
            // Both gone: drop from the parent.
            if let Some(p) = node.parent
                && let Some(parent) = self.nodes.get_mut(p)
            {
                parent.children.retain(|&c| c != idx);
            }
        } else {
            let (status, error) = compare_pair(node, Some(&lp), rp.as_deref(), &opts);
            node.status = status;
            node.error = error;
        }
        let mut cur = self.nodes.get(idx).and_then(|n| n.parent);
        while let Some(p) = cur {
            self.rollup_one(p);
            cur = self.nodes.get(p).and_then(|n| n.parent);
        }
    }

    fn right_path_any(&self, idx: usize) -> Option<PathBuf> {
        self.nodes.get(idx).map(|n| self.right_root.join(n.rel_right(&self.nodes)))
    }

    fn rollup_one(&mut self, idx: usize) {
        let Some(n) = self.nodes.get(idx) else { return };
        let status = match (&n.left, &n.right) {
            (Some(_), None) => Status::LeftOnly,
            (None, Some(_)) => Status::RightOnly,
            _ => {
                if n.error.is_some() {
                    Status::Error
                } else if n.children.iter().all(|&c| self.nodes.get(c).is_none_or(|c| c.status == Status::Same)) {
                    Status::Same
                } else {
                    Status::Different
                }
            }
        };
        if let Some(n) = self.nodes.get_mut(idx) {
            n.status = status;
        }
    }

    /// Recompute folder statuses bottom-up (children always have larger indices).
    fn rollup(&mut self) {
        for i in (0..self.nodes.len()).rev() {
            if self.nodes.get(i).is_some_and(|n| n.is_dir()) {
                self.rollup_one(i);
            }
        }
    }
}

/// Name used to pair entries across sides.
fn match_key(name: &OsString, case_sensitive: bool) -> String {
    let s = name.to_string_lossy();
    if case_sensitive { s.into_owned() } else { s.to_lowercase() }
}

/// A directory entry: name, metadata, is-symlink.
type Listed = (OsString, fs::Metadata, bool);

struct Listing {
    entries: Vec<Listed>,
}

/// List one directory, applying hidden/symlink rules. Errors become an empty listing + message.
fn list_dir(dir: &Path, opts: &FolderCompareOptions, filter: &CompiledFilter) -> std::result::Result<Listing, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    for ent in rd {
        let Ok(ent) = ent else { continue };
        let name = ent.file_name();
        let path = ent.path();
        let Ok(lmeta) = fs::symlink_metadata(&path) else { continue };
        let is_symlink = lmeta.file_type().is_symlink();
        let meta = if is_symlink && opts.follow_symlinks { fs::metadata(&path).unwrap_or(lmeta) } else { lmeta };
        if !filter.show_hidden && filter::is_hidden(&name, &meta) {
            continue;
        }
        entries.push((name, meta, is_symlink));
    }
    Ok(Listing { entries })
}

/// Compare two folder trees.
pub fn compare_folders(left: &Path, right: &Path, opts: &FolderCompareOptions, progress: &Progress) -> Result<FolderComparison> {
    for p in [left, right] {
        if !p.is_dir() {
            return Err(Error::InvalidInput(format!("{} is not a folder", p.display())));
        }
    }
    let filter = CompiledFilter::new(&opts.filter)?;
    let root_meta = |p: &Path| -> Result<EntryMeta> {
        let m = fs::metadata(p).map_err(|e| Error::io(p, e))?;
        Ok(EntryMeta::from_fs(p.file_name().map(OsString::from).unwrap_or_else(|| p.as_os_str().to_owned()), &m, false))
    };
    let mut cmp = FolderComparison {
        left_root: left.to_path_buf(),
        right_root: right.to_path_buf(),
        nodes: vec![Node {
            rel: PathBuf::new(),
            left: Some(root_meta(left)?),
            right: Some(root_meta(right)?),
            status: Status::Same,
            parent: None,
            children: Vec::new(),
            depth: 0,
            error: None,
        }],
        options: opts.clone(),
    };

    // Breadth-first scan of both trees in step.
    let mut seen_l: HashSet<PathBuf> = HashSet::new();
    let mut seen_r: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<(usize, Option<PathBuf>, Option<PathBuf>)> = VecDeque::new();
    queue.push_back((0, Some(left.to_path_buf()), Some(right.to_path_buf())));
    while let Some((parent, lp, rp)) = queue.pop_front() {
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let depth = cmp.nodes.get(parent).map_or(0, |n| n.depth) + 1;
        if depth > MAX_DEPTH {
            if let Some(n) = cmp.nodes.get_mut(parent) {
                n.error = Some(format!("nesting deeper than {MAX_DEPTH} folders"));
            }
            continue;
        }
        // Symlink loop protection when following links.
        if opts.follow_symlinks {
            let fresh = |p: &Option<PathBuf>, seen: &mut HashSet<PathBuf>| match p.as_ref().and_then(|p| fs::canonicalize(p).ok()) {
                Some(c) => seen.insert(c),
                None => true,
            };
            if !fresh(&lp, &mut seen_l) || !fresh(&rp, &mut seen_r) {
                continue;
            }
        }
        if let Some(p) = &lp {
            progress.set_current(p.display().to_string());
        }
        let mut errors = Vec::new();
        let mut list = |p: &Option<PathBuf>| -> Vec<Listed> {
            match p {
                None => Vec::new(),
                Some(p) => match list_dir(p, opts, &filter) {
                    Ok(l) => l.entries,
                    Err(e) => {
                        errors.push(e);
                        Vec::new()
                    }
                },
            }
        };
        let left_entries = list(&lp);
        let right_entries = list(&rp);
        if !errors.is_empty()
            && let Some(n) = cmp.nodes.get_mut(parent)
        {
            n.error = Some(errors.join("; "));
        }

        // Pair by name.
        let mut pairs: Vec<(Option<Listed>, Option<Listed>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for e in left_entries {
            index.insert(match_key(&e.0, opts.case_sensitive), pairs.len());
            pairs.push((Some(e), None));
        }
        for e in right_entries {
            match index.get(&match_key(&e.0, opts.case_sensitive)).and_then(|&i| pairs.get_mut(i)) {
                Some(slot) if slot.1.is_none() => slot.1 = Some(e),
                _ => pairs.push((None, Some(e))),
            }
        }
        // Folders first, then files; alphabetical (case-insensitive) within each.
        pairs.sort_by_cached_key(|(l, r)| {
            let (name, meta) = l.as_ref().or(r.as_ref()).map(|(n, m, _)| (n.to_string_lossy().to_lowercase(), m.is_dir())).unwrap_or_default();
            (!meta, name)
        });

        let parent_rel = cmp.nodes.get(parent).map(|n| n.rel.clone()).unwrap_or_default();
        for (l, r) in pairs {
            let name = l.as_ref().or(r.as_ref()).map(|(n, _, _)| n.clone()).unwrap_or_default();
            let rel = parent_rel.join(&name);
            let l_dir = l.as_ref().is_some_and(|(_, m, _)| m.is_dir());
            let r_dir = r.as_ref().is_some_and(|(_, m, _)| m.is_dir());
            let is_dir = l_dir || r_dir;
            // Filters.
            if is_dir {
                if !filter.accepts_dir(&name, &rel) {
                    continue;
                }
            } else {
                let size = l.as_ref().or(r.as_ref()).map_or(0, |(_, m, _)| m.len());
                if !filter.accepts_file(&name, &rel, size) {
                    continue;
                }
            }
            progress.items_scanned.fetch_add(1, Ordering::Relaxed);
            let lmeta = l.as_ref().map(|(n, m, s)| EntryMeta::from_fs(n.clone(), m, *s));
            let rmeta = r.as_ref().map(|(n, m, s)| EntryMeta::from_fs(n.clone(), m, *s));
            let mismatch = l.is_some() && r.is_some() && l_dir != r_dir;
            let idx = cmp.nodes.len();
            cmp.nodes.push(Node {
                rel: rel.clone(),
                left: lmeta,
                right: rmeta,
                status: Status::Same,
                parent: Some(parent),
                children: Vec::new(),
                depth,
                error: mismatch.then(|| "a file on one side and a folder on the other".to_owned()),
            });
            if let Some(p) = cmp.nodes.get_mut(parent) {
                p.children.push(idx);
            }
            if is_dir && !mismatch {
                let lchild = l.as_ref().and(lp.as_ref()).map(|p| p.join(&name));
                let rchild = r.as_ref().and(rp.as_ref()).map(|p| p.join(r.as_ref().map(|x| &x.0).unwrap_or(&name)));
                queue.push_back((idx, lchild, rchild));
            }
        }
    }

    // Compare file pairs in parallel.
    let file_pairs: Vec<usize> = cmp.nodes.iter().enumerate().filter(|(_, n)| !n.is_dir() && n.error.is_none()).map(|(i, _)| i).collect();
    progress.files_to_compare.store(file_pairs.len() as u64, Ordering::Relaxed);
    let results: Vec<(usize, Status, Option<String>)> = file_pairs
        .par_iter()
        .map(|&i| {
            if progress.is_cancelled() {
                return (i, Status::Error, Some("cancelled".to_owned()));
            }
            let Some(n) = cmp.nodes.get(i) else { return (i, Status::Error, Some("internal: missing node".to_owned())) };
            let lp = n.left.as_ref().map(|_| cmp.left_root.join(&n.rel));
            let rp = n.right.as_ref().map(|_| cmp.right_root.join(n.rel_right(&cmp.nodes)));
            let (s, e) = compare_pair(n, lp.as_deref(), rp.as_deref(), opts);
            progress.files_compared.fetch_add(1, Ordering::Relaxed);
            (i, s, e)
        })
        .collect();
    if progress.is_cancelled() {
        return Err(Error::Cancelled);
    }
    for (i, s, e) in results {
        if let Some(n) = cmp.nodes.get_mut(i) {
            n.status = s;
            n.error = e;
        }
    }
    // Mismatched file/folder pairs.
    for n in cmp.nodes.iter_mut() {
        if n.error.is_some() && n.left.is_some() && n.right.is_some() && n.children.is_empty() {
            n.status = Status::Error;
        }
    }
    cmp.rollup();
    Ok(cmp)
}

fn times_equal(a: Option<SystemTime>, b: Option<SystemTime>, opts: &FolderCompareOptions) -> std::cmp::Ordering {
    let (Some(a), Some(b)) = (a, b) else { return std::cmp::Ordering::Equal };
    let (diff, ord) = match a.duration_since(b) {
        Ok(d) => (d, std::cmp::Ordering::Greater),
        Err(e) => (e.duration(), std::cmp::Ordering::Less),
    };
    let tol = Duration::from_secs(u64::from(opts.time_tolerance_secs));
    if diff <= tol {
        return std::cmp::Ordering::Equal;
    }
    if opts.ignore_dst_shift {
        let hour = Duration::from_secs(3600);
        let off = diff.abs_diff(hour);
        if off <= tol {
            return std::cmp::Ordering::Equal;
        }
    }
    ord
}

/// Compare one file pair (orphans are handled here too).
fn compare_pair(n: &Node, lp: Option<&Path>, rp: Option<&Path>, opts: &FolderCompareOptions) -> (Status, Option<String>) {
    let (l, r) = match (&n.left, &n.right) {
        (Some(_), None) => return (Status::LeftOnly, None),
        (None, Some(_)) => return (Status::RightOnly, None),
        (None, None) => return (Status::Error, Some("missing on both sides".into())),
        (Some(l), Some(r)) => (l, r),
    };
    if l.is_dir != r.is_dir {
        return (Status::Error, Some("a file on one side and a folder on the other".into()));
    }
    let newer = || match times_equal(l.modified, r.modified, opts) {
        std::cmp::Ordering::Greater => Status::LeftNewer,
        std::cmp::Ordering::Less => Status::RightNewer,
        std::cmp::Ordering::Equal => Status::Different,
    };
    let time_same = times_equal(l.modified, r.modified, opts) == std::cmp::Ordering::Equal;
    match opts.mode {
        CompareMode::Size => (if l.size == r.size { Status::Same } else { Status::Different }, None),
        CompareMode::Timestamp => (if time_same { Status::Same } else { newer() }, None),
        CompareMode::SizeAndTime => {
            if l.size == r.size && time_same {
                (Status::Same, None)
            } else {
                (newer(), None)
            }
        }
        mode => {
            let (Some(lp), Some(rp)) = (lp, rp) else { return (Status::Error, Some("missing path".into())) };
            let res = match mode {
                CompareMode::Crc32 => {
                    if l.size != r.size {
                        Ok(false)
                    } else {
                        crc32(lp).and_then(|a| crc32(rp).map(|b| a == b))
                    }
                }
                CompareMode::TextRules => text_equal(lp, rp, l.size, r.size, &opts.text),
                _ => {
                    if l.size != r.size {
                        Ok(false)
                    } else {
                        bytes_equal(lp, rp)
                    }
                }
            };
            match res {
                Ok(true) => (Status::Same, None),
                Ok(false) => (Status::Different, None),
                Err(e) => (Status::Error, Some(e.to_string())),
            }
        }
    }
}

fn open(p: &Path) -> Result<fs::File> {
    fs::File::open(p).map_err(|e| Error::io(p, e))
}

/// Streamed byte comparison.
pub fn bytes_equal(a: &Path, b: &Path) -> Result<bool> {
    let (mut fa, mut fb) = (open(a)?, open(b)?);
    let mut ba = vec![0u8; 256 * 1024];
    let mut bb = vec![0u8; 256 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba).map_err(|e| Error::io(a, e))?;
        let nb = read_full(&mut fb, &mut bb).map_err(|e| Error::io(b, e))?;
        if na != nb || ba.get(..na) != bb.get(..nb) {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let Some(rest) = buf.get_mut(n..) else { break };
        match f.read(rest)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

pub fn crc32(p: &Path) -> Result<u32> {
    let mut f = open(p)?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(p, e))?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize())
}

/// SHA-256 of a file, hex-encoded (used by reports).
pub fn sha256_hex(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(p, e))?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn text_equal(a: &Path, b: &Path, sa: u64, sb: u64, rules: &TextCompareOptions) -> Result<bool> {
    const LIMIT: u64 = 64 * 1024 * 1024;
    if sa > LIMIT || sb > LIMIT {
        return if sa != sb { Ok(false) } else { bytes_equal(a, b) };
    }
    let ba = fs::read(a).map_err(|e| Error::io(a, e))?;
    let bb = fs::read(b).map_err(|e| Error::io(b, e))?;
    if ba == bb {
        return Ok(true);
    }
    if is_probably_binary(&ba) || is_probably_binary(&bb) {
        return Ok(false);
    }
    let ta = TextFile::from_bytes(&ba);
    let tb = TextFile::from_bytes(&bb);
    Ok(text::compare_lines(&ta.lines, &tb.lines, rules)?.important_changes() == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tree(root: &Path, files: &[(&str, &str)]) {
        for (p, c) in files {
            let path = root.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if p.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::write(&path, c).unwrap();
            }
        }
    }

    fn find<'c>(c: &'c FolderComparison, rel: &str) -> &'c Node {
        c.nodes.iter().find(|n| n.rel == Path::new(rel)).unwrap_or_else(|| panic!("no node {rel}"))
    }

    #[test]
    fn compares_trees() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("same.txt", "x"), ("diff.txt", "aaa"), ("lonly.txt", "1"), ("sub/a.txt", "a"), ("ldir/x", "x")]);
        tree(&r, &[("same.txt", "x"), ("diff.txt", "bbbb"), ("ronly.txt", "1"), ("sub/a.txt", "b")]);
        // Same timestamps for the "same" files so size+time sees them as equal.
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        for p in [l.join("same.txt"), r.join("same.txt")] {
            crate::fsutil::set_mtime(&p, t);
        }
        let opts = FolderCompareOptions { mode: CompareMode::Binary, ..Default::default() };
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        assert_eq!(find(&c, "same.txt").status, Status::Same);
        assert_eq!(find(&c, "diff.txt").status, Status::Different);
        assert_eq!(find(&c, "lonly.txt").status, Status::LeftOnly);
        assert_eq!(find(&c, "ronly.txt").status, Status::RightOnly);
        assert_eq!(find(&c, "sub/a.txt").status, Status::Different);
        assert_eq!(find(&c, "sub").status, Status::Different);
        assert_eq!(find(&c, "ldir").status, Status::LeftOnly);
        assert_eq!(c.nodes[0].status, Status::Different);
        let s = c.summary();
        assert_eq!((s.same, s.different, s.left_only, s.right_only), (1, 2, 2, 1));
        // Folders come first among siblings.
        let first = &c.nodes[c.nodes[0].children[0]];
        assert!(first.is_dir());

        let st = compare_folders(&l, &r, &FolderCompareOptions::default(), &Progress::default()).unwrap();
        assert_eq!(find(&st, "same.txt").status, Status::Same);
    }

    #[test]
    fn text_rules_mode() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("a.txt", "x  =  1\n")]);
        tree(&r, &[("a.txt", "x = 1\r\n")]);
        let mut opts = FolderCompareOptions { mode: CompareMode::TextRules, ..Default::default() };
        opts.text.whitespace = text::WhitespaceMode::IgnoreChanges;
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        assert_eq!(find(&c, "a.txt").status, Status::Same);
        opts.mode = CompareMode::Crc32;
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        assert_eq!(find(&c, "a.txt").status, Status::Different);
    }

    #[test]
    fn filters_and_hidden() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("a.rs", ""), ("b.txt", ""), ("target/x.rs", ""), (".hidden.rs", "")]);
        tree(&r, &[]);
        fs::create_dir_all(&r).unwrap();
        let mut opts = FolderCompareOptions { filter: Filter::parse("*.rs;-target/"), ..Default::default() };
        opts.filter.show_hidden = false;
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        let names: Vec<String> = c.nodes.iter().skip(1).map(|n| n.name()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn case_insensitive_matching() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("Readme.TXT", "x")]);
        tree(&r, &[("readme.txt", "x")]);
        let opts = FolderCompareOptions { case_sensitive: false, mode: CompareMode::Binary, ..Default::default() };
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[1].status, Status::Same);
        assert_eq!(c.right_path(1).unwrap(), r.join("readme.txt"));
        let opts = FolderCompareOptions { case_sensitive: true, ..opts };
        let c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        assert_eq!(c.nodes.len(), 3);
    }

    #[test]
    fn refresh_after_copy() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("sub/a.txt", "new")]);
        tree(&r, &[("sub/a.txt", "old!")]);
        let opts = FolderCompareOptions { mode: CompareMode::Binary, ..Default::default() };
        let mut c = compare_folders(&l, &r, &opts, &Progress::default()).unwrap();
        let i = c.nodes.iter().position(|n| n.rel == Path::new("sub/a.txt")).unwrap();
        crate::fsutil::copy_file(&l.join("sub/a.txt"), &r.join("sub/a.txt")).unwrap();
        c.refresh_file(i);
        assert_eq!(c.nodes[i].status, Status::Same);
        assert_eq!(c.nodes[0].status, Status::Same);
    }

    #[test]
    fn errors_not_panics() {
        let d = tempfile::tempdir().unwrap();
        assert!(compare_folders(&d.path().join("nope"), d.path(), &FolderCompareOptions::default(), &Progress::default()).is_err());
        let bad = FolderCompareOptions { filter: Filter { include_files: vec!["[".into()], ..Filter::default() }, ..Default::default() };
        assert!(compare_folders(d.path(), d.path(), &bad, &Progress::default()).is_err());
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(compare_folders(d.path(), d.path(), &FolderCompareOptions::default(), &p), Err(Error::Cancelled)));
    }

    #[test]
    fn file_vs_folder() {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("L"), d.path().join("R"));
        tree(&l, &[("x", "file")]);
        tree(&r, &[("x/inner", "f")]);
        let c = compare_folders(&l, &r, &FolderCompareOptions::default(), &Progress::default()).unwrap();
        assert_eq!(find(&c, "x").status, Status::Error);
    }
}
