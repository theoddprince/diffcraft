# AGENTS.md: guide for AI agents and contributors

DiffCraft is an open-source diff and merge tool written **only in Rust**: a native
egui/eframe app on wgpu for Windows, Linux and macOS. The product name is **DiffCraft** in user-facing
text. Machine names are lowercase (`diffcraft-*` crates, the `diffcraft` binary, the
`io.github.theoddprince.diffcraft` id). Never add a webview/JS UI framework.

## Workspace

| Crate | Role | May use |
|---|---|---|
| `crates/diffcraft-core` | Diff algorithms, rules, alignment, 3-way merge, binary/image/folder compare, sync, text files | no UI crates |
| `crates/diffcraft-session` | Session, Settings, file-format rules, git integration | core |
| `crates/diffcraft-report` | Unified diff, HTML/CSV reports | core |
| `crates/diffcraft-ui` | egui views: home, text, merge, folder, sync, hex, picture, options | everything above + egui/eframe/rfd |
| `apps/diffcraft` | clap CLI, GUI launch, `diffcraft` + `diffcraft-cli` binaries | everything |

Only `diffcraft-ui` (and the app) may depend on egui, eframe, winit or rfd. Comparison logic belongs in
`diffcraft-core`, where it is testable without a window.

## Golden rules

1. **Never crash.** Non-test code has no `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!`. Every crate root
   carries `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, …)]`. Use `.get()` for
   indices that come from files, params or arithmetic. Cap allocations by input size, and use explicit
   stacks instead of recursion. Background work runs in `jobs::Job`, which turns a panic into an error.
   A bad file, regex, path or settings file produces a message, never a crash.
2. **No `unsafe`.** The workspace sets `unsafe_code = "forbid"`.
3. **Cross-platform.** Use `Path`/`PathBuf`, never string paths. Gate OS-specific code with `cfg`.
   Case-insensitive name matching defaults on for Windows and macOS. Paths must compile and work on
   Windows, Linux and macOS (CI runs all three).
4. **Heavy work off the UI thread.** Loading, diffing, scanning, syncing and decoding go through
   `Job::spawn`, with progress and Cancel.
5. **Colours come from `theme::Palette`**, never hard-coded in views. Glyphs must exist in egui's
   bundled fonts or in the system fallback fonts (`theme::install_fallback_fonts`). Check new icons
   visually.
6. **Every change has tests.** Core logic gets unit tests and property tests (`proptest`). Views
   keep their pure logic in testable functions (`text_model.rs`, `visible_rows`, …).
7. **Preserve user data.** Saving keeps encoding, BOM and line endings. Writes are atomic
   (`fsutil::write_atomic`). Deletes go to the trash by default.

## Before you finish

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# optional cross-checks (after `rustup target add …`):
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings
```

For UI changes, run `cargo run --release -- <left> <right>` and look at the result.

## Adding a feature

- New comparison logic goes in `diffcraft-core` with tests.
- New view commands are added to `view::Cmd`, handled in the view's `supports`/`run`, given a menu
  item in `app.rs` and, if needed, a shortcut in `DiffCraftApp::shortcuts` and the `SHORTCUTS` table.
- New settings go in `diffcraft_session::Settings` with `#[serde(default)]`, are clamped in `sanitize()`,
  and get an editor in `options.rs`.
