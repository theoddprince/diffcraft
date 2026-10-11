//! DiffCraft engine: everything that compares, merges or synchronises, with no UI.
//!
//! * [`text`]: line diffs (Myers / patience / histogram), comparison rules, side-by-side
//!   alignment, intra-line highlights, encoding-preserving text files.
//! * [`merge`]: three-way merge with conflict detection.
//! * [`binary`]: byte-level comparison for the hex view.
//! * [`image`]: pixel comparison with tolerance.
//! * [`folder`]: folder scanning, comparison, filters and synchronisation.
//!
//! Nothing in this crate panics on bad input: every failure is an [`Error`].

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod binary;
pub mod detect;
mod error;
pub mod folder;
pub mod fsutil;
pub mod image;
pub mod merge;
pub mod text;

pub use error::{Error, Result};
