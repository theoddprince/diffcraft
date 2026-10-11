//! Folder comparison reports: HTML, CSV and a plain-text summary.

use std::path::Path;
use std::time::SystemTime;

use diffcraft_core::folder::{EntryMeta, FolderComparison, Node, Status};

use crate::util::{BASE_CSS, csv_field, format_time, group_thousands, html_escape};

/// Options for [`folder_html_report`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderReportOptions {
    /// Page title and heading.
    pub title: String,
    /// Skip files and folders whose status is `Same`.
    pub only_differences: bool,
    /// Show the size columns.
    pub include_sizes: bool,
    /// Show the modification-time columns.
    pub include_times: bool,
}

impl Default for FolderReportOptions {
    fn default() -> Self {
        FolderReportOptions { title: "Folder Compare".to_owned(), only_differences: false, include_sizes: true, include_times: true }
    }
}

const FOLDER_CSS: &str = r#"table.tree td{white-space:nowrap;border-bottom:1px solid var(--border)}
td.num{text-align:right;font-variant-numeric:tabular-nums}
td.name{white-space:pre}
tr.dir td.name{font-weight:700}
tr.different td.st,tr.leftnewer td.st,tr.rightnewer td.st{background:var(--changed)}
tr.leftonly td.st,tr.leftonly td.name{background:var(--left)}
tr.rightonly td.st,tr.rightonly td.name{background:var(--right)}
tr.error td{background:var(--error)}
td.err{color:var(--muted);font-style:italic;white-space:normal}
"#;

/// Node indices in depth-first pre-order from the root, following `children` with an explicit
/// stack. The root itself (index 0) is not included; each node is visited at most once.
fn tree_order(cmp: &FolderComparison) -> Vec<usize> {
    let mut out = Vec::with_capacity(cmp.nodes.len());
    let mut seen = vec![false; cmp.nodes.len()];
    let Some(root) = cmp.nodes.first() else { return out };
    if let Some(s) = seen.first_mut() {
        *s = true;
    }
    let mut stack: Vec<usize> = root.children.iter().rev().copied().collect();
    while let Some(i) = stack.pop() {
        let (Some(node), Some(flag)) = (cmp.nodes.get(i), seen.get_mut(i)) else { continue };
        if *flag {
            continue;
        }
        *flag = true;
        out.push(i);
        stack.extend(node.children.iter().rev().copied());
    }
    out
}

/// Relative path of a node with `/` separators (folders get a trailing `/`).
fn slash_path(node: &Node) -> String {
    let mut s = rel_slash(&node.rel);
    if s.is_empty() {
        s = node.name();
    }
    if node.is_dir() {
        s.push('/');
    }
    s
}

fn rel_slash(p: &Path) -> String {
    p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

fn status_class(s: Status) -> &'static str {
    match s {
        Status::Same => "same",
        Status::Different => "different",
        Status::LeftNewer => "leftnewer",
        Status::RightNewer => "rightnewer",
        Status::LeftOnly => "leftonly",
        Status::RightOnly => "rightonly",
        Status::Error => "error",
    }
}

fn size_of(m: Option<&EntryMeta>) -> Option<u64> {
    m.filter(|m| !m.is_dir).map(|m| m.size)
}

fn time_of(m: Option<&EntryMeta>) -> Option<String> {
    m.and_then(|m| m.modified).map(format_time)
}

/// HTML folder report: the summary, then a table in tree order (indented by depth) with name,
/// left size, left modified, status, right size and right modified. Rows are coloured by
/// [`Status`] and folders are bold. With `only_differences`, items whose status is `Same` are
/// skipped.
pub fn folder_html_report(cmp: &FolderComparison, opts: &FolderReportOptions) -> String {
    let sum = cmp.summary();
    let mut out = String::with_capacity(4096 + cmp.nodes.len() * 160);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    out.push_str(&format!("<title>{}</title>\n<style>\n{BASE_CSS}{FOLDER_CSS}</style>\n</head>\n<body>\n", html_escape(&opts.title)));
    out.push_str(&format!("<h1>{}</h1>\n<div class=\"meta\">\n", html_escape(&opts.title)));
    out.push_str(&format!("<div>Left: <strong>{}</strong></div>\n", html_escape(&cmp.left_root.to_string_lossy())));
    out.push_str(&format!("<div>Right: <strong>{}</strong></div>\n", html_escape(&cmp.right_root.to_string_lossy())));
    out.push_str(&format!("<div>Compared by: {}</div>\n", html_escape(cmp.options.mode.label())));
    out.push_str(&format!("<div>Generated: {} UTC</div>\n</div>\n", format_time(SystemTime::now())));
    out.push_str(&format!(
        "<div class=\"summary\"><span>{} same</span><span>{} different</span><span>{} left only</span>\
<span>{} right only</span><span>{} error(s)</span><span>{} folder(s)</span></div>\n",
        sum.same, sum.different, sum.left_only, sum.right_only, sum.errors, sum.folders
    ));

    out.push_str("<table class=\"tree\">\n<thead><tr><th>Name</th>");
    if opts.include_sizes {
        out.push_str("<th>Left size</th>");
    }
    if opts.include_times {
        out.push_str("<th>Left modified</th>");
    }
    out.push_str("<th>Status</th>");
    if opts.include_sizes {
        out.push_str("<th>Right size</th>");
    }
    if opts.include_times {
        out.push_str("<th>Right modified</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");

    let mut shown = 0usize;
    for i in tree_order(cmp) {
        let Some(n) = cmp.nodes.get(i) else { continue };
        if opts.only_differences && n.status == Status::Same {
            continue;
        }
        shown += 1;
        let dir = if n.is_dir() { " dir" } else { "" };
        out.push_str(&format!("<tr class=\"{}{dir}\">", status_class(n.status)));
        let indent = "    ".repeat(n.depth.saturating_sub(1).min(64));
        let suffix = if n.is_dir() { "/" } else { "" };
        out.push_str(&format!("<td class=\"name\">{indent}{}{suffix}</td>", html_escape(&n.name())));
        let cells = |out: &mut String, m: Option<&EntryMeta>, sizes: bool, times: bool| {
            if sizes {
                out.push_str(&format!("<td class=\"num\">{}</td>", size_of(m).map(group_thousands).unwrap_or_default()));
            }
            if times {
                out.push_str(&format!("<td>{}</td>", time_of(m).unwrap_or_default()));
            }
        };
        cells(&mut out, n.left.as_ref(), opts.include_sizes, opts.include_times);
        out.push_str(&format!("<td class=\"st\">{}", html_escape(n.status.label())));
        if let Some(e) = &n.error {
            out.push_str(&format!("<div class=\"err\">{}</div>", html_escape(e)));
        }
        out.push_str("</td>");
        cells(&mut out, n.right.as_ref(), opts.include_sizes, opts.include_times);
        out.push_str("</tr>\n");
    }
    if shown == 0 {
        let cols = 2 + if opts.include_sizes { 2 } else { 0 } + if opts.include_times { 2 } else { 0 };
        let msg = if opts.only_differences { "No differences" } else { "Both folders are empty" };
        out.push_str(&format!("<tr><td colspan=\"{cols}\">{msg}</td></tr>\n"));
    }
    out.push_str("</tbody>\n</table>\n</body>\n</html>\n");
    out
}

/// CSV (RFC 4180 quoting, CRLF line ends) with the header
/// `path,status,left_size,left_modified,right_size,right_modified,error`, one row per item in
/// tree order. Paths use `/` separators; folders end in `/` and have empty sizes. With
/// `only_differences`, items whose status is `Same` are skipped.
pub fn folder_csv_report(cmp: &FolderComparison, only_differences: bool) -> String {
    let mut out = String::from("path,status,left_size,left_modified,right_size,right_modified,error\r\n");
    for i in tree_order(cmp) {
        let Some(n) = cmp.nodes.get(i) else { continue };
        if only_differences && n.status == Status::Same {
            continue;
        }
        let fields = [
            slash_path(n),
            n.status.label().to_owned(),
            size_of(n.left.as_ref()).map(|s| s.to_string()).unwrap_or_default(),
            time_of(n.left.as_ref()).unwrap_or_default(),
            size_of(n.right.as_ref()).map(|s| s.to_string()).unwrap_or_default(),
            time_of(n.right.as_ref()).unwrap_or_default(),
            n.error.clone().unwrap_or_default(),
        ];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

/// Plain-text summary for the command line: one `<status-label>\t<relative path>` line per
/// differing item in tree order, then a totals line. Folders are listed only when they are
/// orphans or errors (a differing folder's contents are listed instead); folder paths end in `/`.
pub fn folder_text_summary(cmp: &FolderComparison) -> String {
    let mut out = String::new();
    for i in tree_order(cmp) {
        let Some(n) = cmp.nodes.get(i) else { continue };
        let listed = if n.is_dir() { n.status.is_orphan() || n.status == Status::Error } else { n.status.is_difference() };
        if listed {
            out.push_str(n.status.label());
            out.push('\t');
            out.push_str(&slash_path(n));
            out.push('\n');
        }
    }
    let s = cmp.summary();
    out.push_str(&format!(
        "Same: {}, Different: {}, Left only: {}, Right only: {}, Errors: {}, Folders: {}\n",
        s.same, s.different, s.left_only, s.right_only, s.errors, s.folders
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::folder::{CompareMode, FolderCompareOptions, Progress, compare_folders};
    use std::fs;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, FolderComparison) {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        fs::write(l.path().join("same.txt"), "same").unwrap();
        fs::write(r.path().join("same.txt"), "same").unwrap();
        fs::write(l.path().join("diff.txt"), "left").unwrap();
        fs::write(r.path().join("diff.txt"), "right!").unwrap();
        fs::write(l.path().join("we,ird.txt"), "x").unwrap();
        fs::create_dir(l.path().join("sub")).unwrap();
        fs::create_dir(r.path().join("sub")).unwrap();
        fs::write(l.path().join("sub").join("in&ner.txt"), "1").unwrap();
        fs::write(r.path().join("sub").join("in&ner.txt"), "2").unwrap();
        fs::create_dir(r.path().join("samedir")).unwrap();
        fs::create_dir(l.path().join("samedir")).unwrap();
        fs::write(l.path().join("samedir").join("a.txt"), "a").unwrap();
        fs::write(r.path().join("samedir").join("a.txt"), "a").unwrap();
        fs::write(r.path().join("right_only.bin"), [0u8, 1, 2]).unwrap();
        let opts = FolderCompareOptions { mode: CompareMode::Binary, ..Default::default() };
        let mut cmp = compare_folders(l.path(), r.path(), &opts, &Progress::default()).unwrap();
        // `"`, `<` and `>` are not allowed in Windows file names: rename in the result instead.
        for n in &mut cmp.nodes {
            let name = n.name().replace("we,ird", "we,ird\"name").replace("in&ner", "<in&ner>");
            if name != n.name() {
                n.rel.set_file_name(&name);
                for m in [n.left.as_mut(), n.right.as_mut()].into_iter().flatten() {
                    m.name = name.clone().into();
                }
            }
        }
        (l, r, cmp)
    }

    #[test]
    fn csv_rows_and_quoting() {
        let (_l, _r, cmp) = setup();
        let csv = folder_csv_report(&cmp, false);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "path,status,left_size,left_modified,right_size,right_modified,error");
        assert!(csv.contains("\"we,ird\"\"name.txt\",Left only,1,"), "{csv}");
        assert!(lines.iter().any(|l| l.starts_with("diff.txt,Different,4,") && l.contains(",6,")), "{csv}");
        assert!(lines.iter().any(|l| l.starts_with("sub/<in&ner>.txt,Different,1,")), "{csv}");
        assert!(lines.iter().any(|l| l.starts_with("sub/,Different,,")), "{csv}");
        assert!(lines.iter().any(|l| l.starts_with("right_only.bin,Right only,,,3,")), "{csv}");
        assert!(csv.contains("same.txt,Same"));
        // Header + 9 items (sub, sub/<inner>, samedir, samedir/a, same, diff, weird, right_only) + trailing empty.
        assert_eq!(lines.len(), 1 + 8 + 1, "{csv}");
        // Tree order: a folder's children follow it directly.
        let pos = |p: &str| lines.iter().position(|l| l.starts_with(p)).unwrap();
        assert_eq!(pos("sub/<in&ner>.txt"), pos("sub/,") + 1);

        let only = folder_csv_report(&cmp, true);
        assert!(!only.contains("same.txt,Same"));
        assert!(!only.contains("samedir"));
        assert!(only.contains("diff.txt"));
    }

    #[test]
    fn html_rows() {
        let (_l, _r, cmp) = setup();
        let html = folder_html_report(&cmp, &FolderReportOptions::default());
        assert!(html.contains("&lt;in&amp;ner&gt;.txt"));
        assert!(!html.contains("<in&ner>"));
        assert!(html.contains("we,ird&quot;name.txt"));
        assert!(html.contains("<tr class=\"different dir\"><td class=\"name\">sub/</td>"), "{html}");
        assert!(html.contains("<td class=\"name\">    &lt;in&amp;ner&gt;.txt</td>"), "{html}");
        assert!(html.contains("<tr class=\"rightonly\">"));
        assert!(html.contains("same.txt"));
        let only = folder_html_report(&cmp, &FolderReportOptions { only_differences: true, include_times: false, ..Default::default() });
        assert!(!only.contains(">same.txt<"));
        assert!(!only.contains("samedir"));
        assert!(!only.contains("Left modified"));
        assert!(only.contains("diff.txt"));
    }

    #[test]
    fn text_summary() {
        let (_l, _r, cmp) = setup();
        let s = folder_text_summary(&cmp);
        assert!(s.contains("Different\tdiff.txt\n"));
        assert!(s.contains("Different\tsub/<in&ner>.txt\n"));
        assert!(s.contains("Right only\tright_only.bin\n"));
        assert!(!s.contains("Same\t"));
        assert!(!s.contains("\tsub/\n"));
        assert!(s.ends_with("Same: 2, Different: 2, Left only: 1, Right only: 1, Errors: 0, Folders: 2\n"), "{s}");
    }

    #[test]
    fn hostile_tree_does_not_hang_or_panic() {
        let (_l, _r, mut cmp) = setup();
        // A cycle and a dangling child index.
        if let Some(n) = cmp.nodes.get_mut(1) {
            n.children.push(0);
            n.children.push(1);
            n.children.push(10_000);
        }
        let _ = folder_csv_report(&cmp, false);
        let _ = folder_html_report(&cmp, &FolderReportOptions::default());
        cmp.nodes.clear();
        assert_eq!(folder_csv_report(&cmp, false).lines().count(), 1);
        let _ = folder_text_summary(&cmp);
    }
}
