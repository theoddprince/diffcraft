//! Small, careful file-system helpers shared by text saving and folder sync.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{Error, Result};

/// Write `bytes` to `path` atomically: write a sibling temp file, then rename it over `path`.
/// Existing permissions are kept.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| Error::InvalidInput(format!("not a file path: {}", path.display())))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".diffcraft-{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let perms = fs::metadata(path).ok().map(|m| m.permissions());
    fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    if let Some(p) = perms {
        // Keep the executable bit (Unix). On Windows a read-only target would refuse the
        // rename, so the replacement is written without the read-only flag.
        let mut p2 = p;
        if cfg!(windows) {
            #[allow(clippy::permissions_set_readonly_false)]
            p2.set_readonly(false);
        }
        let _ = fs::set_permissions(&tmp, p2);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path, e));
    }
    Ok(())
}

/// Copy a file, keeping permissions (via `fs::copy`) and the modification time.
pub fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    // A read-only destination would make the copy fail; clear the flag first.
    if let Ok(meta) = fs::metadata(to)
        && meta.permissions().readonly()
    {
        let mut p = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        let _ = fs::set_permissions(to, p);
    }
    fs::copy(from, to).map_err(|e| Error::io(to, e))?;
    if let Ok(mtime) = fs::metadata(from).and_then(|m| m.modified()) {
        set_mtime(to, mtime);
    }
    Ok(())
}

/// Set a file's modification time (best effort).
pub fn set_mtime(path: &Path, mtime: SystemTime) {
    if let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(mtime);
    }
}

/// Copy a file or a whole directory tree. Symlinks inside trees are not followed.
pub fn copy_entry(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from).map_err(|e| Error::io(from, e))?;
    if !meta.is_dir() {
        return copy_file(from, to);
    }
    if to.starts_with(from) {
        return Err(Error::InvalidInput(format!("cannot copy {} into itself", from.display())));
    }
    for entry in walkdir::WalkDir::new(from).follow_links(false) {
        let entry = entry.map_err(|e| Error::InvalidInput(e.to_string()))?;
        let rel = entry.path().strip_prefix(from).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))?;
        } else if entry.file_type().is_file() {
            copy_file(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Delete a file or directory, to the system trash when `to_trash` is set.
pub fn delete_entry(path: &Path, to_trash: bool) -> Result<()> {
    if to_trash {
        return trash::delete(path).map_err(|e| Error::Io { path: path.to_path_buf(), source: std::io::Error::other(e.to_string()) });
    }
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }.map_err(|e| Error::io(path, e))
}

/// Rename `path` to `new_name` within its directory.
pub fn rename_entry(path: &Path, new_name: &str) -> Result<PathBuf> {
    if new_name.is_empty() || new_name.contains(['/', '\\']) || new_name == "." || new_name == ".." {
        return Err(Error::InvalidInput(format!("invalid file name `{new_name}`")));
    }
    let target = path.with_file_name(new_name);
    if target.exists() {
        return Err(Error::InvalidInput(format!("{} already exists", target.display())));
    }
    fs::rename(path, &target).map_err(|e| Error::io(path, e))?;
    Ok(target)
}

/// Create a directory (and parents).
pub fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f.txt");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1, "no temp file left behind");
    }

    #[test]
    fn copy_tree_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/a.txt"), b"A").unwrap();
        let dst = d.path().join("dst");
        copy_entry(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("sub/a.txt")).unwrap(), b"A");
        assert!(copy_entry(&src, &src.join("sub")).is_err());
        delete_entry(&dst, false).unwrap();
        assert!(!dst.exists());
    }

    #[test]
    fn copy_keeps_mtime() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        fs::write(&a, b"x").unwrap();
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        set_mtime(&a, old);
        let b = d.path().join("b");
        copy_file(&a, &b).unwrap();
        assert_eq!(fs::metadata(&b).unwrap().modified().unwrap(), old);
    }

    #[test]
    fn rename_rejects_bad_names() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        fs::write(&a, b"x").unwrap();
        assert!(rename_entry(&a, "../x").is_err());
        assert!(rename_entry(&a, "").is_err());
        let b = rename_entry(&a, "b").unwrap();
        assert!(b.exists());
    }
}
