//! Side-by-side HTML report of a text comparison.

use std::ops::Range;
use std::time::SystemTime;

use diffcraft_core::text::{Row, RowKind, expand_tabs, intraline::intraline, stats};

use crate::util::{BASE_CSS, format_time, html_escape};

/// Options for [`text_html_report`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReportOptions {
    /// Page title and heading.
    pub title: String,
    /// Label of the left side (usually its path).
    pub left_label: String,
    /// Label of the right side (usually its path).
    pub right_label: String,
    /// `None` shows all lines; `Some(n)` shows only differences with `n` lines of context, and
    /// hidden runs of equal lines as a separator row.
    pub context: Option<usize>,
    /// Tab stops used to expand tabs (clamped to 1..=16).
    pub tab_width: usize,
    /// Highlight the differing parts of changed lines.
    pub intraline: bool,
    /// Comparison rules shown in the header, e.g. "Ignore whitespace changes". Empty hides it.
    pub rules_summary: String,
}

impl Default for TextReportOptions {
    fn default() -> Self {
        TextReportOptions {
            title: "Text Compare".to_owned(),
            left_label: "Left".to_owned(),
            right_label: "Right".to_owned(),
            context: None,
            tab_width: 4,
            intraline: true,
            rules_summary: String::new(),
        }
    }
}

const TEXT_CSS: &str = r#"table.diff{table-layout:fixed;font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
table.diff col.n{width:4.5em}
td.n{color:var(--muted);text-align:right;user-select:none;border-right:1px solid var(--border)}
td.t{white-space:pre-wrap;word-break:break-all}
tr.minor td.t{background:var(--minor)}
tr.changed td.t{background:var(--changed)}
tr.lonly td.l{background:var(--left)}
tr.ronly td.r{background:var(--right)}
td.blank{background:var(--gap)!important}
tr.gap td{background:var(--gap);color:var(--muted);text-align:center;font-style:italic;border-top:1px solid var(--border);border-bottom:1px solid var(--border)}
span.il{background:var(--changed-hl);border-radius:2px}
td.l.t{border-right:1px solid var(--border)}
.legend span{display:inline-block;padding:0 6px;margin-right:6px;border:1px solid var(--border);border-radius:3px}
"#;

/// Self-contained HTML side-by-side report (inline CSS, no JavaScript, works offline, light and
/// dark via `prefers-color-scheme`).
///
/// Rows get line numbers and a colour per [`RowKind`] (Same, Minor = unimportant, Changed,
/// LeftOnly, RightOnly); changed rows get intra-line highlights computed on tab-expanded text.
/// The header shows both labels, the rules, the generation time and the summary counts.
pub fn text_html_report(left: &[String], right: &[String], rows: &[Row], opts: &TextReportOptions) -> String {
    let st = stats(rows);
    let mut out = String::with_capacity(4096 + rows.len() * 96);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    out.push_str(&format!("<title>{}</title>\n<style>\n{BASE_CSS}{TEXT_CSS}</style>\n</head>\n<body>\n", html_escape(&opts.title)));
    out.push_str(&format!("<h1>{}</h1>\n<div class=\"meta\">\n", html_escape(&opts.title)));
    out.push_str(&format!("<div>Left: <strong>{}</strong></div>\n", html_escape(&opts.left_label)));
    out.push_str(&format!("<div>Right: <strong>{}</strong></div>\n", html_escape(&opts.right_label)));
    if !opts.rules_summary.trim().is_empty() {
        out.push_str(&format!("<div>Rules: {}</div>\n", html_escape(&opts.rules_summary)));
    }
    out.push_str(&format!("<div>Generated: {} UTC</div>\n</div>\n", format_time(SystemTime::now())));
    out.push_str(&format!(
        "<div class=\"summary\"><span>{} important difference block(s)</span><span>{} unimportant block(s)</span>\
<span>{} changed line(s)</span><span>{} left-only line(s)</span><span>{} right-only line(s)</span></div>\n",
        st.important_blocks, st.unimportant_blocks, st.changed_lines, st.left_only_lines, st.right_only_lines
    ));

    out.push_str("<table class=\"diff\">\n<colgroup><col class=\"n\"><col><col class=\"n\"><col></colgroup>\n");
    out.push_str(&format!(
        "<thead><tr><th></th><th>{}</th><th></th><th>{}</th></tr></thead>\n<tbody>\n",
        html_escape(&opts.left_label),
        html_escape(&opts.right_label)
    ));
    if rows.is_empty() {
        out.push_str("<tr class=\"gap\"><td colspan=\"4\">Both sides are empty</td></tr>\n");
    }
    let visible = visible_rows(rows, opts.context);
    let mut next = 0usize; // first row index not yet accounted for
    for r in visible {
        if r > next {
            gap_row(&mut out, r - next);
        }
        if let Some(row) = rows.get(r) {
            row_html(&mut out, left, right, row, opts);
        }
        next = r.saturating_add(1);
    }
    if rows.len() > next && !rows.is_empty() {
        gap_row(&mut out, rows.len() - next);
    }
    out.push_str("</tbody>\n</table>\n</body>\n</html>\n");
    out
}

/// Indices of the rows to show, ascending.
fn visible_rows(rows: &[Row], context: Option<usize>) -> Vec<usize> {
    let Some(ctx) = context else { return (0..rows.len()).collect() };
    let mut show = vec![false; rows.len()];
    for (i, r) in rows.iter().enumerate() {
        if r.kind.is_difference() {
            let lo = i.saturating_sub(ctx);
            let hi = i.saturating_add(ctx).saturating_add(1).min(rows.len());
            if let Some(s) = show.get_mut(lo..hi) {
                s.iter_mut().for_each(|x| *x = true);
            }
        }
    }
    show.iter().enumerate().filter_map(|(i, s)| s.then_some(i)).collect()
}

fn gap_row(out: &mut String, hidden: usize) {
    let s = if hidden == 1 { "" } else { "s" };
    out.push_str(&format!("<tr class=\"gap\"><td colspan=\"4\">&#8943; {hidden} identical line{s} hidden &#8943;</td></tr>\n"));
}

fn row_html(out: &mut String, left: &[String], right: &[String], row: &Row, opts: &TextReportOptions) {
    let class = match row.kind {
        RowKind::Same => "same",
        RowKind::Minor => "minor",
        RowKind::Changed => "changed",
        RowKind::LeftOnly => "lonly",
        RowKind::RightOnly => "ronly",
    };
    let unimportant = if row.unimportant && row.kind != RowKind::Minor { " minor" } else { "" };
    let lt = row.left.map(|i| expand_tabs(left.get(i).map_or("", String::as_str), opts.tab_width).into_owned());
    let rt = row.right.map(|j| expand_tabs(right.get(j).map_or("", String::as_str), opts.tab_width).into_owned());
    let (lr, rr) = match (&lt, &rt) {
        (Some(l), Some(r)) if opts.intraline && row.kind == RowKind::Changed => intraline(l, r),
        _ => (Vec::new(), Vec::new()),
    };
    out.push_str(&format!("<tr class=\"{class}{unimportant}\">"));
    cell(out, row.left, lt.as_deref(), &lr, "l");
    cell(out, row.right, rt.as_deref(), &rr, "r");
    out.push_str("</tr>\n");
}

fn cell(out: &mut String, num: Option<usize>, text: Option<&str>, ranges: &[Range<usize>], side: &str) {
    match (num, text) {
        (Some(n), Some(t)) => {
            out.push_str(&format!("<td class=\"n\">{}</td><td class=\"t {side}\">{}</td>", n.saturating_add(1), highlight(t, ranges)));
        }
        _ => out.push_str(&format!("<td class=\"n\"></td><td class=\"t {side} blank\"></td>")),
    }
}

/// Escape `text`, wrapping the byte `ranges` in `<span class="il">`. Ranges that are out of
/// order, out of bounds or not on character boundaries are skipped.
fn highlight(text: &str, ranges: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len() + ranges.len() * 24);
    let mut pos = 0usize;
    for r in ranges {
        if r.start < pos || r.start >= r.end {
            continue;
        }
        let (Some(before), Some(mid)) = (text.get(pos..r.start), text.get(r.clone())) else { continue };
        out.push_str(&html_escape(before));
        out.push_str("<span class=\"il\">");
        out.push_str(&html_escape(mid));
        out.push_str("</span>");
        pos = r.end;
    }
    out.push_str(&html_escape(text.get(pos..).unwrap_or("")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffcraft_core::text::{TextCompareOptions, align, compare_lines};

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    fn report(a: &[String], b: &[String], opts: &TextReportOptions) -> String {
        let d = compare_lines(a, b, &TextCompareOptions::default()).unwrap();
        let rows = align(&d, a, b);
        text_html_report(a, b, &rows, opts)
    }

    #[test]
    fn escapes_and_labels() {
        let a = v(&["<script>alert(1)</script>", "same"]);
        let b = v(&["<b>bold</b>", "same"]);
        let opts = TextReportOptions { left_label: "left<1>.txt".into(), right_label: "RIGHT-LABEL".into(), ..Default::default() };
        let html = report(&a, &b, &opts);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<b>bold"));
        assert!(html.contains("left&lt;1&gt;.txt"));
        assert!(html.contains("RIGHT-LABEL"));
        assert!(html.contains("prefers-color-scheme: dark"));
    }

    #[test]
    fn intraline_span_for_changed_word() {
        let a = v(&["let x = alpha;"]);
        let b = v(&["let x = omega;"]);
        let html = report(&a, &b, &TextReportOptions::default());
        assert!(html.contains("<span class=\"il\">alpha</span>"), "{html}");
        assert!(html.contains("<span class=\"il\">omega</span>"));
        let off = report(&a, &b, &TextReportOptions { intraline: false, ..Default::default() });
        assert!(!off.contains("<span class=\"il\">"));
    }

    #[test]
    fn context_omits_far_lines() {
        let a: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let mut b = a.clone();
        b[15] = "CHANGED".into();
        let all = report(&a, &b, &TextReportOptions::default());
        assert!(all.contains(">line2<"));
        let some = report(&a, &b, &TextReportOptions { context: Some(1), ..Default::default() });
        assert!(!some.contains(">line2<"));
        assert!(!some.contains(">line13<"));
        assert!(some.contains(">line14<") && some.contains(">line16<") && some.contains("CHANGED"));
        assert!(some.contains("14 identical lines hidden"), "{some}");
        assert_eq!(some.matches("class=\"gap\"").count(), 2);
    }

    #[test]
    fn tabs_and_bad_indices() {
        let a = v(&["\tx"]);
        let rows = vec![
            Row { left: Some(0), right: Some(99), kind: RowKind::Changed, hunk: 0, unimportant: false },
            Row { left: Some(usize::MAX), right: None, kind: RowKind::LeftOnly, hunk: 7, unimportant: false },
        ];
        let html = text_html_report(&a, &[], &rows, &TextReportOptions { tab_width: 2, ..Default::default() });
        assert!(html.contains("  x"));
    }

    #[test]
    fn highlight_skips_bad_ranges() {
        assert_eq!(highlight("héllo", &[1..2, 0..0]), "héllo");
        assert_eq!(highlight("a<b", &[1..2, 5..9]), "a<span class=\"il\">&lt;</span>b");
    }
}
