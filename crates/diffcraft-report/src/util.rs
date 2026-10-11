//! Small shared helpers: HTML escaping, CSV quoting, timestamps and number formatting.

use std::time::{SystemTime, UNIX_EPOCH};

/// Escape `&`, `<`, `>`, `"` and `'` so `s` can be placed in HTML text or attribute values.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Quote one CSV field per RFC 4180: fields containing `,`, `"`, CR or LF are wrapped in
/// double quotes with embedded quotes doubled.
pub(crate) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_owned() }
}

/// Format `t` as UTC `YYYY-MM-DD HH:MM:SS`. Times before 1970 are supported.
pub fn format_time(t: SystemTime) -> String {
    let secs: i64 = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => {
            let d = e.duration();
            // Round towards negative infinity so sub-second parts land in the previous second.
            let s = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            if d.subsec_nanos() > 0 { s.saturating_neg().saturating_sub(1) } else { s.saturating_neg() }
        }
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day) (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era.saturating_mul(400);
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y.saturating_add(1) } else { y };
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// `1234567` -> `1,234,567`.
pub(crate) fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Colour scheme shared by the HTML reports: light by default, dark via `prefers-color-scheme`.
pub(crate) const BASE_CSS: &str = r#":root{--bg:#ffffff;--fg:#1f2328;--muted:#656d76;--border:#d0d7de;--head:#f6f8fa;
--same:transparent;--minor:#eef2f7;--changed:#fff5d6;--changed-hl:#f5c84c;--left:#ffe3e3;--right:#dff5e3;--error:#ffd0d0;--gap:#f0f3f6;}
@media (prefers-color-scheme: dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#8d96a0;--border:#30363d;--head:#161b22;
--same:transparent;--minor:#1a2230;--changed:#3a2f12;--changed-hl:#7a5c10;--left:#4a1d22;--right:#14381f;--error:#5c1a1a;--gap:#161b22;}}
*{box-sizing:border-box}
body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif}
h1{font-size:20px;margin:0 0 8px}
.meta{color:var(--muted);margin:0 0 12px}
.meta div{margin:2px 0}
.summary{display:flex;flex-wrap:wrap;gap:8px 16px;margin:0 0 16px;padding:8px 12px;background:var(--head);border:1px solid var(--border);border-radius:6px}
table{border-collapse:collapse;width:100%}
th{position:sticky;top:0;background:var(--head);text-align:left;font-weight:600;border-bottom:1px solid var(--border);padding:4px 8px}
td{padding:1px 8px;vertical-align:top}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn epoch_times() {
        assert_eq!(format_time(UNIX_EPOCH + Duration::from_secs(1_000_000_000)), "2001-09-09 01:46:40");
        assert_eq!(format_time(UNIX_EPOCH), "1970-01-01 00:00:00");
        assert_eq!(format_time(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29 00:00:00");
        assert_eq!(format_time(UNIX_EPOCH - Duration::from_secs(1)), "1969-12-31 23:59:59");
        assert_eq!(format_time(UNIX_EPOCH - Duration::from_millis(500)), "1969-12-31 23:59:59");
    }

    #[test]
    fn escaping_and_quoting() {
        assert_eq!(html_escape("<a href=\"x\">&'</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,\"b\""), "\"a,\"\"b\"\"\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
