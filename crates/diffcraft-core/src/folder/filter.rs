//! Name filters for folder comparisons (`*.rs;*.toml`, `-target`, hidden files, sizes).

use std::ffi::OsStr;
use std::path::Path;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// User-editable filter settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    /// File name globs to include (empty = all files). Patterns containing `/` match the
    /// relative path instead of the name.
    pub include_files: Vec<String>,
    /// File name globs to exclude.
    pub exclude_files: Vec<String>,
    /// Folder name globs to exclude (e.g. `.git`, `target`, `node_modules`).
    pub exclude_folders: Vec<String>,
    /// Include hidden files and folders (dot-files; the hidden attribute on Windows).
    pub show_hidden: bool,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
}

impl Filter {
    /// Parse a filter string: `*.rs;*.toml;-*.bak;-target\`.
    /// A leading `-` excludes; a trailing `/` or `\` makes it a folder pattern.
    pub fn parse(spec: &str) -> Filter {
        let mut f = Filter { show_hidden: true, ..Filter::default() };
        for raw in spec.split([';', ',']).map(str::trim).filter(|s| !s.is_empty()) {
            let (exclude, pat) = match raw.strip_prefix('-') {
                Some(p) => (true, p.trim()),
                None => (false, raw),
            };
            let folder = pat.ends_with('/') || pat.ends_with('\\');
            let pat = pat.trim_end_matches(['/', '\\']).to_owned();
            if pat.is_empty() {
                continue;
            }
            match (exclude, folder) {
                (true, true) => f.exclude_folders.push(pat),
                (true, false) => f.exclude_files.push(pat),
                (false, true) => {} // folder includes are implied
                (false, false) => f.include_files.push(pat),
            }
        }
        f
    }

    /// The inverse of [`Filter::parse`] (hidden/size settings are not part of the string).
    pub fn to_spec(&self) -> String {
        let mut parts: Vec<String> = self.include_files.clone();
        parts.extend(self.exclude_files.iter().map(|p| format!("-{p}")));
        parts.extend(self.exclude_folders.iter().map(|p| format!("-{p}/")));
        parts.join(";")
    }
}

/// A compiled [`Filter`].
#[derive(Clone, Debug)]
pub struct CompiledFilter {
    include: Option<GlobSet>,
    include_has_paths: bool,
    exclude: GlobSet,
    exclude_dirs: GlobSet,
    pub show_hidden: bool,
    min_size: Option<u64>,
    max_size: Option<u64>,
}

fn glob(p: &str) -> Result<Glob> {
    GlobBuilder::new(p)
        .case_insensitive(cfg!(any(windows, target_os = "macos")))
        .literal_separator(false)
        .build()
        .map_err(|e| Error::InvalidInput(format!("filter `{p}`: {e}")))
}

fn set(pats: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in pats {
        b.add(glob(p)?);
    }
    b.build().map_err(|e| Error::InvalidInput(e.to_string()))
}

impl CompiledFilter {
    pub fn new(f: &Filter) -> Result<Self> {
        Ok(CompiledFilter {
            include: if f.include_files.is_empty() { None } else { Some(set(&f.include_files)?) },
            include_has_paths: f.include_files.iter().any(|p| p.contains('/')),
            exclude: set(&f.exclude_files)?,
            exclude_dirs: set(&f.exclude_folders)?,
            show_hidden: f.show_hidden,
            min_size: f.min_size,
            max_size: f.max_size,
        })
    }

    pub fn accepts_dir(&self, name: &OsStr, rel: &Path) -> bool {
        !self.exclude_dirs.is_match(name) && !self.exclude_dirs.is_match(rel)
    }

    pub fn accepts_file(&self, name: &OsStr, rel: &Path, size: u64) -> bool {
        if self.exclude.is_match(name) || self.exclude.is_match(rel) {
            return false;
        }
        if let Some(inc) = &self.include
            && !(inc.is_match(name) || (self.include_has_paths && inc.is_match(rel)))
        {
            return false;
        }
        if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
            return false;
        }
        true
    }
}

/// Is this entry hidden (dot-file, or the Windows hidden attribute)?
pub fn is_hidden(name: &OsStr, meta: &std::fs::Metadata) -> bool {
    if name.to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec() {
        let f = Filter::parse("*.rs; *.toml ;-*.bak;-target/;-.git\\");
        assert_eq!(f.include_files, vec!["*.rs", "*.toml"]);
        assert_eq!(f.exclude_files, vec!["*.bak"]);
        assert_eq!(f.exclude_folders, vec!["target", ".git"]);
        assert_eq!(Filter::parse(&f.to_spec()).exclude_folders, f.exclude_folders);
    }

    #[test]
    fn matching() {
        let c = CompiledFilter::new(&Filter::parse("*.rs;-main.rs;-target/")).unwrap();
        assert!(c.accepts_file(OsStr::new("lib.rs"), Path::new("src/lib.rs"), 1));
        assert!(!c.accepts_file(OsStr::new("main.rs"), Path::new("src/main.rs"), 1));
        assert!(!c.accepts_file(OsStr::new("a.txt"), Path::new("a.txt"), 1));
        assert!(!c.accepts_dir(OsStr::new("target"), Path::new("target")));
        assert!(c.accepts_dir(OsStr::new("src"), Path::new("src")));
    }

    #[test]
    fn invalid_glob_is_error() {
        assert!(CompiledFilter::new(&Filter { include_files: vec!["a[".into()], ..Filter::default() }).is_err());
    }
}
