//! DiffCraft reports: everything that turns a comparison into text another tool or a person
//! reads outside the app.
//!
//! * [`unified_diff`]: a `diff -u` / git-style patch of a text comparison.
//! * [`text_html_report`]: a self-contained side-by-side HTML report of a text comparison.
//! * [`folder_html_report`], [`folder_csv_report`], [`folder_text_summary`]: folder comparison
//!   reports for people (HTML), spreadsheets (CSV) and the command line (plain text).
//!
//! Nothing here panics on bad input: indices that do not fit the given lines are rendered as
//! empty text instead.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod folder;
mod html_text;
mod unified;
mod util;

pub use folder::{FolderReportOptions, folder_csv_report, folder_html_report, folder_text_summary};
pub use html_text::{TextReportOptions, text_html_report};
pub use unified::unified_diff;
pub use util::{format_time, html_escape};
