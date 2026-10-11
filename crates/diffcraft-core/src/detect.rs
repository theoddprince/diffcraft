//! Choosing the right comparison view for a pair of paths.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::image::is_image_path;
use crate::text::file::is_probably_binary;

/// The kind of comparison session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ViewKind {
    Folder,
    Text,
    Hex,
    Picture,
}

impl ViewKind {
    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Folder => "Folder Compare",
            ViewKind::Text => "Text Compare",
            ViewKind::Hex => "Hex Compare",
            ViewKind::Picture => "Picture Compare",
        }
    }
}

/// Read the first 8 KiB of a file.
fn head(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Does this file look binary (NUL bytes near the start)?
pub fn is_binary_file(path: &Path) -> bool {
    head(path).is_some_and(|h| is_probably_binary(&h))
}

/// Pick a view for two paths (either may be missing, e.g. an orphan file in a folder compare).
pub fn view_for(left: &Path, right: &Path) -> ViewKind {
    if left.is_dir() || right.is_dir() {
        return ViewKind::Folder;
    }
    let exists = |p: &Path| p.is_file();
    let paths: Vec<&Path> = [left, right].into_iter().filter(|p| exists(p)).collect();
    if paths.is_empty() {
        return ViewKind::Text;
    }
    if paths.iter().all(|p| is_image_path(p)) {
        return ViewKind::Picture;
    }
    if paths.iter().any(|p| is_binary_file(p)) {
        return ViewKind::Hex;
    }
    ViewKind::Text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_views() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("a.txt");
        let b = d.path().join("a.bin");
        std::fs::write(&t, b"hello").unwrap();
        std::fs::write(&b, b"\0\x01\x02").unwrap();
        assert_eq!(view_for(d.path(), d.path()), ViewKind::Folder);
        assert_eq!(view_for(&t, &t), ViewKind::Text);
        assert_eq!(view_for(&t, &b), ViewKind::Hex);
        assert_eq!(view_for(Path::new("x.png"), Path::new("y.png")), ViewKind::Text, "missing files fall back to text");
        let p = d.path().join("p.png");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(view_for(&p, &p), ViewKind::Picture);
    }
}
