//! Command-line front end: opens the GUI for paths, or runs headless subcommands
//! (`diff`, `merge`, `report`, `sync`, `setup-git`, `sessions`).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use diffcraft_core::folder::sync::{self, SyncMode};
use diffcraft_core::folder::{self, CompareMode, Filter, FolderCompareOptions, Progress};
use diffcraft_core::merge::{self, MarkerLabels};
use diffcraft_core::text::file::{TextFile, is_probably_binary};
use diffcraft_core::text::{self, Algorithm, TextCompareOptions, WhitespaceMode};
use diffcraft_session::{SessionKind, Store};
use diffcraft_ui::{OpenRequest, Startup};

/// Exit codes for headless commands (like `diff`/`cmp`).
pub const EXIT_SAME: i32 = 0;
pub const EXIT_DIFFERENT: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

#[derive(Parser, Debug)]
#[command(
    name = "diffcraft",
    version,
    about = "Compare and merge files, folders, binaries and pictures.",
    long_about = "DiffCraft compares and merges files, folders, binaries and pictures.\n\n\
                  Without a subcommand it opens the window:\n  \
                  diffcraft                       Home screen\n  \
                  diffcraft LEFT RIGHT            compare two files or folders\n  \
                  diffcraft LEFT RIGHT BASE -o OUT   three-way merge (git mergetool)\n\n\
                  On Windows use `diffcraft-cli` for the headless subcommands so output reaches the console.",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Files or folders to open: LEFT RIGHT, or LEFT RIGHT BASE for a merge.
    #[arg(value_name = "PATH", num_args = 0..=3)]
    pub paths: Vec<PathBuf>,

    /// Merge result file (three-way merge).
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Force the kind of comparison instead of detecting it.
    #[arg(long, value_enum)]
    pub view: Option<ViewArg>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ViewArg {
    Text,
    Folder,
    Sync,
    Hex,
    Picture,
}

impl ViewArg {
    fn kind(self) -> SessionKind {
        match self {
            ViewArg::Text => SessionKind::TextCompare,
            ViewArg::Folder => SessionKind::FolderCompare,
            ViewArg::Sync => SessionKind::FolderSync,
            ViewArg::Hex => SessionKind::HexCompare,
            ViewArg::Picture => SessionKind::PictureCompare,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Compare two files or folders and print the differences (exit 0 same, 1 different, 2 error).
    Diff(DiffArgs),
    /// Three-way merge without the window: writes the result, with conflict markers where
    /// both sides changed the same lines (exit 0 clean, 1 conflicts, 2 error).
    Merge {
        left: PathBuf,
        right: PathBuf,
        base: PathBuf,
        /// Output file (default: standard output).
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        rules: RuleArgs,
    },
    /// Write a report: HTML side-by-side or unified patch for files, HTML or CSV for folders.
    Report {
        left: PathBuf,
        right: PathBuf,
        #[arg(long, value_name = "FILE")]
        html: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        csv: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        patch: Option<PathBuf>,
        /// Only list differences (folders) / show differences with context (text).
        #[arg(long)]
        only_differences: bool,
        #[command(flatten)]
        rules: RuleArgs,
        #[command(flatten)]
        folder: FolderArgs,
    },
    /// Synchronize two folders.
    Sync {
        left: PathBuf,
        right: PathBuf,
        #[arg(long, value_enum, default_value_t = SyncArg::UpdateRight)]
        mode: SyncArg,
        /// Print the plan without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Delete permanently instead of moving to the trash.
        #[arg(long)]
        no_trash: bool,
        #[command(flatten)]
        folder: FolderArgs,
    },
    /// Register DiffCraft as git difftool/mergetool (prints the commands unless --apply).
    SetupGit {
        /// Run `git config --global` now.
        #[arg(long)]
        apply: bool,
        /// Don't make DiffCraft the default diff.tool/merge.tool.
        #[arg(long)]
        no_default: bool,
    },
    /// List saved and recent sessions.
    Sessions,
    /// Open a saved session (by name) or a .dcsession file in the window.
    Open { session: String },
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    pub left: PathBuf,
    pub right: PathBuf,
    /// Lines of context.
    #[arg(short = 'U', long, default_value_t = 3)]
    pub unified: usize,
    /// Only say whether the inputs differ.
    #[arg(short = 'q', long)]
    pub brief: bool,
    #[arg(long, value_enum, default_value_t = ColorArg::Auto)]
    pub color: ColorArg,
    #[command(flatten)]
    pub rules: RuleArgs,
    #[command(flatten)]
    pub folder: FolderArgs,
}

#[derive(Args, Debug, Clone, Default)]
pub struct RuleArgs {
    /// Whitespace handling.
    #[arg(short = 'w', long, value_enum, default_value_t = WsArg::Compare)]
    pub whitespace: WsArg,
    /// Ignore case differences.
    #[arg(short = 'i', long)]
    pub ignore_case: bool,
    /// Inserted/deleted blank lines are unimportant.
    #[arg(short = 'B', long)]
    pub ignore_blank_lines: bool,
    /// Text matching this regular expression is unimportant (repeatable).
    #[arg(long = "ignore-pattern", value_name = "REGEX")]
    pub ignore_patterns: Vec<String>,
    #[arg(long, value_enum, default_value_t = AlgArg::Histogram)]
    pub algorithm: AlgArg,
}

impl RuleArgs {
    pub fn options(&self) -> TextCompareOptions {
        TextCompareOptions {
            algorithm: match self.algorithm {
                AlgArg::Myers => Algorithm::Myers,
                AlgArg::Patience => Algorithm::Patience,
                AlgArg::Histogram => Algorithm::Histogram,
            },
            whitespace: match self.whitespace {
                WsArg::Compare => WhitespaceMode::Compare,
                WsArg::Trailing => WhitespaceMode::IgnoreLeadingTrailing,
                WsArg::Changes => WhitespaceMode::IgnoreChanges,
                WsArg::All => WhitespaceMode::IgnoreAll,
            },
            ignore_case: self.ignore_case,
            ignore_blank_lines: self.ignore_blank_lines,
            unimportant_patterns: self.ignore_patterns.clone(),
        }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct FolderArgs {
    /// How folder entries are compared.
    #[arg(long = "compare", value_enum, default_value_t = CompareArg::SizeTime)]
    pub compare: CompareArg,
    /// Filter such as "*.rs;*.toml;-target/;-*.bak".
    #[arg(long)]
    pub filter: Option<String>,
    /// Skip hidden files and folders.
    #[arg(long)]
    pub no_hidden: bool,
}

impl FolderArgs {
    pub fn options(&self, text: TextCompareOptions) -> FolderCompareOptions {
        let mut o = FolderCompareOptions {
            mode: match self.compare {
                CompareArg::SizeTime => CompareMode::SizeAndTime,
                CompareArg::Size => CompareMode::Size,
                CompareArg::Time => CompareMode::Timestamp,
                CompareArg::Crc => CompareMode::Crc32,
                CompareArg::Binary => CompareMode::Binary,
                CompareArg::Rules => CompareMode::TextRules,
            },
            text,
            ..FolderCompareOptions::default()
        };
        if let Some(f) = &self.filter {
            o.filter = Filter::parse(f);
        }
        o.filter.show_hidden = !self.no_hidden;
        o
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum WsArg {
    #[default]
    Compare,
    Trailing,
    Changes,
    All,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum AlgArg {
    Myers,
    Patience,
    #[default]
    Histogram,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CompareArg {
    #[default]
    SizeTime,
    Size,
    Time,
    Crc,
    Binary,
    Rules,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SyncArg {
    #[default]
    UpdateRight,
    UpdateLeft,
    UpdateBoth,
    MirrorRight,
    MirrorLeft,
}

impl SyncArg {
    fn mode(self) -> SyncMode {
        match self {
            SyncArg::UpdateRight => SyncMode::UpdateRight,
            SyncArg::UpdateLeft => SyncMode::UpdateLeft,
            SyncArg::UpdateBoth => SyncMode::UpdateBoth,
            SyncArg::MirrorRight => SyncMode::MirrorRight,
            SyncArg::MirrorLeft => SyncMode::MirrorLeft,
        }
    }
}

/// Parse arguments and run. Returns the process exit code.
pub fn main_entry() -> i32 {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).try_init();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { EXIT_SAME };
        }
    };
    let mut out = std::io::stdout().lock();
    match run(cli, &mut out) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("diffcraft: {e}");
            EXIT_ERROR
        }
    }
}

/// Run a parsed command line, writing headless output to `out`.
pub fn run(cli: Cli, out: &mut dyn Write) -> Result<i32, String> {
    match cli.command {
        None => gui(cli.paths, cli.output, cli.view),
        Some(Command::Diff(a)) => diff(&a, out),
        Some(Command::Merge { left, right, base, output, rules }) => merge_cmd(&left, &right, &base, output.as_deref(), &rules.options(), out),
        Some(Command::Report { left, right, html, csv, patch, only_differences, rules, folder }) => {
            report(&left, &right, html.as_deref(), csv.as_deref(), patch.as_deref(), only_differences, &rules.options(), &folder, out)
        }
        Some(Command::Sync { left, right, mode, dry_run, no_trash, folder }) => sync_cmd(&left, &right, mode.mode(), dry_run, !no_trash, &folder, out),
        Some(Command::SetupGit { apply, no_default }) => setup_git(apply, !no_default, out),
        Some(Command::Sessions) => sessions(out),
        Some(Command::Open { session }) => open_session(&session),
    }
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

fn gui(paths: Vec<PathBuf>, output: Option<PathBuf>, view: Option<ViewArg>) -> Result<i32, String> {
    let mut open = Vec::new();
    match paths.as_slice() {
        [] => {}
        [one] => {
            let kind = view.map(ViewArg::kind).unwrap_or(if one.is_dir() { SessionKind::FolderCompare } else { SessionKind::TextCompare });
            open.push(OpenRequest::Paths { left: one.clone(), right: PathBuf::new(), kind: Some(kind) });
        }
        [l, r] => open.push(OpenRequest::Paths { left: l.clone(), right: r.clone(), kind: view.map(ViewArg::kind) }),
        [l, r, base, ..] => open.push(OpenRequest::Merge { left: l.clone(), right: r.clone(), base: base.clone(), output }),
    }
    diffcraft_ui::run(Startup { open })
}

fn use_color(c: ColorArg) -> bool {
    match c {
        ColorArg::Always => true,
        ColorArg::Never => false,
        ColorArg::Auto => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    }
}

/// Load a text file, refusing binary files.
fn load_text(p: &Path) -> Result<Option<TextFile>, String> {
    let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if is_probably_binary(&bytes) {
        return Ok(None);
    }
    Ok(Some(TextFile::from_bytes(&bytes)))
}

pub fn diff(a: &DiffArgs, out: &mut dyn Write) -> Result<i32, String> {
    let (l, r) = (&a.left, &a.right);
    let rules = a.rules.options();
    if l.is_dir() || r.is_dir() {
        if !(l.is_dir() && r.is_dir()) {
            return Err("cannot compare a folder with a file".into());
        }
        let cmp = folder::compare_folders(l, r, &a.folder.options(rules), &Progress::default()).map_err(|e| e.to_string())?;
        let differs = cmp.nodes.first().is_some_and(|n| n.status != folder::Status::Same);
        if a.brief {
            if differs {
                writeln!(out, "Folders {} and {} differ", l.display(), r.display()).map_err(io_err)?;
            }
        } else {
            write!(out, "{}", diffcraft_report::folder_text_summary(&cmp)).map_err(io_err)?;
        }
        return Ok(if differs { EXIT_DIFFERENT } else { EXIT_SAME });
    }
    let (lt, rt) = (load_text(l)?, load_text(r)?);
    let (Some(lt), Some(rt)) = (lt, rt) else {
        let same = folder::bytes_equal(l, r).map_err(|e| e.to_string())?;
        if !same {
            writeln!(out, "Binary files {} and {} differ", l.display(), r.display()).map_err(io_err)?;
        }
        return Ok(if same { EXIT_SAME } else { EXIT_DIFFERENT });
    };
    let d = text::compare_lines(&lt.lines, &rt.lines, &rules).map_err(|e| e.to_string())?;
    let same = d.important_changes() == 0 && (rules.has_rules() || rules.ignore_blank_lines || lt.final_newline == rt.final_newline);
    if same {
        return Ok(EXIT_SAME);
    }
    if a.brief {
        writeln!(out, "Files {} and {} differ", l.display(), r.display()).map_err(io_err)?;
        return Ok(EXIT_DIFFERENT);
    }
    let patch = diffcraft_report::unified_diff(
        &l.display().to_string(),
        &r.display().to_string(),
        &lt.lines,
        &rt.lines,
        &d,
        a.unified,
        lt.final_newline,
        rt.final_newline,
    );
    write_patch(&patch, use_color(a.color), out)?;
    Ok(EXIT_DIFFERENT)
}

fn write_patch(patch: &str, color: bool, out: &mut dyn Write) -> Result<(), String> {
    for line in patch.lines() {
        let code = if !color {
            ""
        } else if line.starts_with("+++") || line.starts_with("---") {
            "\x1b[1m"
        } else if line.starts_with('+') {
            "\x1b[32m"
        } else if line.starts_with('-') {
            "\x1b[31m"
        } else if line.starts_with("@@") {
            "\x1b[36m"
        } else {
            ""
        };
        let reset = if code.is_empty() { "" } else { "\x1b[0m" };
        writeln!(out, "{code}{line}{reset}").map_err(io_err)?;
    }
    Ok(())
}

pub fn merge_cmd(left: &Path, right: &Path, base: &Path, output: Option<&Path>, rules: &TextCompareOptions, out: &mut dyn Write) -> Result<i32, String> {
    let load = |p: &Path| -> Result<TextFile, String> {
        if !p.exists() {
            return Ok(TextFile::default());
        }
        load_text(p)?.ok_or_else(|| format!("{} is a binary file; text merge needs text files", p.display()))
    };
    let (lt, rt, bt) = (load(left)?, load(right)?, load(base)?);
    let m = merge::merge3(&bt.lines, &lt.lines, &rt.lines, rules).map_err(|e| e.to_string())?;
    let labels = MarkerLabels { left: left.display().to_string(), base: base.display().to_string(), right: right.display().to_string() };
    let lines = merge::render(&m, &m.default_resolutions(), &bt.lines, &lt.lines, &rt.lines, &labels);
    let result = TextFile { lines, ..lt.clone() };
    match output {
        Some(p) => result.save(p).map_err(|e| e.to_string())?,
        None => out.write_all(&result.to_bytes()).map_err(io_err)?,
    }
    let conflicts = m.conflict_count();
    if conflicts > 0 {
        eprintln!("diffcraft: {conflicts} conflict{} left with markers", if conflicts == 1 { "" } else { "s" });
        return Ok(EXIT_DIFFERENT);
    }
    Ok(EXIT_SAME)
}

#[allow(clippy::too_many_arguments)]
pub fn report(
    left: &Path,
    right: &Path,
    html: Option<&Path>,
    csv: Option<&Path>,
    patch: Option<&Path>,
    only_differences: bool,
    rules: &TextCompareOptions,
    folder_args: &FolderArgs,
    out: &mut dyn Write,
) -> Result<i32, String> {
    if html.is_none() && csv.is_none() && patch.is_none() {
        return Err("choose at least one of --html, --csv or --patch".into());
    }
    let write = |p: &Path, s: &str| diffcraft_core::fsutil::write_atomic(p, s.as_bytes()).map_err(|e| e.to_string());
    let differs;
    if left.is_dir() && right.is_dir() {
        if patch.is_some() {
            return Err("--patch needs two files".into());
        }
        let cmp = folder::compare_folders(left, right, &folder_args.options(rules.clone()), &Progress::default()).map_err(|e| e.to_string())?;
        differs = cmp.nodes.first().is_some_and(|n| n.status != folder::Status::Same);
        if let Some(p) = html {
            let opts = diffcraft_report::FolderReportOptions {
                title: format!("Folder Compare: {} ↔ {}", left.display(), right.display()),
                only_differences,
                include_sizes: true,
                include_times: true,
            };
            write(p, &diffcraft_report::folder_html_report(&cmp, &opts))?;
            writeln!(out, "Wrote {}", p.display()).map_err(io_err)?;
        }
        if let Some(p) = csv {
            write(p, &diffcraft_report::folder_csv_report(&cmp, only_differences))?;
            writeln!(out, "Wrote {}", p.display()).map_err(io_err)?;
        }
    } else {
        if csv.is_some() {
            return Err("--csv needs two folders".into());
        }
        let (Some(lt), Some(rt)) = (load_text(left)?, load_text(right)?) else {
            return Err("text reports need text files".into());
        };
        let d = text::compare_lines(&lt.lines, &rt.lines, rules).map_err(|e| e.to_string())?;
        differs = d.important_changes() > 0;
        if let Some(p) = html {
            let rows = text::align(&d, &lt.lines, &rt.lines);
            let opts = diffcraft_report::TextReportOptions {
                title: format!("Text Compare: {} ↔ {}", left.display(), right.display()),
                left_label: left.display().to_string(),
                right_label: right.display().to_string(),
                context: only_differences.then_some(3),
                ..Default::default()
            };
            write(p, &diffcraft_report::text_html_report(&lt.lines, &rt.lines, &rows, &opts))?;
            writeln!(out, "Wrote {}", p.display()).map_err(io_err)?;
        }
        if let Some(p) = patch {
            let s = diffcraft_report::unified_diff(
                &left.display().to_string(),
                &right.display().to_string(),
                &lt.lines,
                &rt.lines,
                &d,
                3,
                lt.final_newline,
                rt.final_newline,
            );
            write(p, &s)?;
            writeln!(out, "Wrote {}", p.display()).map_err(io_err)?;
        }
    }
    Ok(if differs { EXIT_DIFFERENT } else { EXIT_SAME })
}

pub fn sync_cmd(
    left: &Path,
    right: &Path,
    mode: SyncMode,
    dry_run: bool,
    to_trash: bool,
    folder_args: &FolderArgs,
    out: &mut dyn Write,
) -> Result<i32, String> {
    let opts = folder_args.options(TextCompareOptions::default());
    let cmp = folder::compare_folders(left, right, &opts, &Progress::default()).map_err(|e| e.to_string())?;
    let ops = sync::plan(&cmp, mode);
    writeln!(out, "{} ({} operation{})", mode.label(), ops.len(), if ops.len() == 1 { "" } else { "s" }).map_err(io_err)?;
    for op in &ops {
        writeln!(out, "  {:<13} {}", op.verb(), op.rel.display()).map_err(io_err)?;
    }
    if dry_run || ops.is_empty() {
        if dry_run {
            writeln!(out, "Dry run: nothing changed.").map_err(io_err)?;
        }
        return Ok(EXIT_SAME);
    }
    let failures = sync::execute(&ops, to_trash, &Progress::default());
    for f in &failures {
        let what = ops.get(f.op).map(|o| o.rel.display().to_string()).unwrap_or_default();
        writeln!(out, "FAILED {what}: {}", f.message).map_err(io_err)?;
    }
    writeln!(out, "Done: {} succeeded, {} failed.", ops.len() - failures.len().min(ops.len()), failures.len()).map_err(io_err)?;
    Ok(if failures.is_empty() { EXIT_SAME } else { EXIT_ERROR })
}

fn gui_exe() -> PathBuf {
    // From diffcraft-cli, point git at the GUI binary next to it.
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("diffcraft"));
    let gui_name = if cfg!(windows) { "diffcraft.exe" } else { "diffcraft" };
    let sibling = me.with_file_name(gui_name);
    if sibling.exists() { sibling } else { me }
}

fn setup_git(apply: bool, make_default: bool, out: &mut dyn Write) -> Result<i32, String> {
    let exe = gui_exe();
    if apply {
        diffcraft_session::git::apply_global(&exe, make_default)?;
        writeln!(out, "Configured git to use {} as difftool/mergetool \"diffcraft\".", exe.display()).map_err(io_err)?;
    } else {
        writeln!(out, "{}", diffcraft_session::git::shell_commands(&exe, make_default)).map_err(io_err)?;
        writeln!(out, "\n# Run `diffcraft-cli setup-git --apply` to apply these.").map_err(io_err)?;
    }
    Ok(EXIT_SAME)
}

fn sessions(out: &mut dyn Write) -> Result<i32, String> {
    let store = Store::platform().map_err(|e| e.to_string())?;
    let (settings, warning) = store.load();
    if let Some(w) = warning {
        eprintln!("diffcraft: {w}");
    }
    writeln!(out, "Saved sessions:").map_err(io_err)?;
    for s in &settings.saved {
        writeln!(out, "  {:<24} {:<16} {} ↔ {}", s.name, s.kind.label(), s.left, s.right).map_err(io_err)?;
    }
    writeln!(out, "Recent:").map_err(io_err)?;
    for s in settings.recent.iter().take(15) {
        writeln!(out, "  {:<16} {} ↔ {}", s.kind.label(), s.left, s.right).map_err(io_err)?;
    }
    writeln!(out, "(settings: {})", store.settings_path().display()).map_err(io_err)?;
    Ok(EXIT_SAME)
}

fn open_session(name: &str) -> Result<i32, String> {
    let p = Path::new(name);
    let session = if p.is_file() {
        diffcraft_session::load_session_file(p).map_err(|e| e.to_string())?
    } else {
        let store = Store::platform().map_err(|e| e.to_string())?;
        let (settings, _) = store.load();
        settings
            .saved
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no saved session named \"{name}\" (see `diffcraft sessions`)"))?
    };
    diffcraft_ui::run(Startup { open: vec![OpenRequest::Session(Box::new(session))] })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> (i32, String) {
        let cli = Cli::try_parse_from(std::iter::once("diffcraft").chain(args.iter().copied())).unwrap();
        let mut out = Vec::new();
        let code = run(cli, &mut out).unwrap_or_else(|e| {
            out.extend_from_slice(e.as_bytes());
            EXIT_ERROR
        });
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    fn write(dir: &Path, name: &str, s: &str) -> String {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, s).unwrap();
        p.display().to_string()
    }

    #[test]
    fn diff_files_exit_codes_and_output() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.txt", "one\ntwo\nthree\n");
        let b = write(d.path(), "b.txt", "one\n2\nthree\n");
        let (code, out) = run_args(&["diff", "--color", "never", &a, &b]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(out.contains("-two\n+2"), "{out}");
        assert_eq!(run_args(&["diff", &a, &a]).0, EXIT_SAME);
        let (code, out) = run_args(&["diff", "-q", &a, &b]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(out.starts_with("Files"));
        // Whitespace rules.
        let c = write(d.path(), "c.txt", "one  \ntwo\nthree\n");
        assert_eq!(run_args(&["diff", &a, &c]).0, EXIT_DIFFERENT);
        assert_eq!(run_args(&["diff", "-w", "trailing", &a, &c]).0, EXIT_SAME);
        // Missing file is an error, not a panic.
        assert_eq!(run_args(&["diff", &a, &d.path().join("nope").display().to_string()]).0, EXIT_ERROR);
    }

    #[test]
    fn diff_binary_and_folders() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.bin", "\0\x01");
        let b = write(d.path(), "b.bin", "\0\x02");
        let (code, out) = run_args(&["diff", &a, &b]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(out.contains("Binary files"));
        write(d.path(), "L/x.txt", "1");
        write(d.path(), "R/x.txt", "1");
        let (l, r) = (d.path().join("L").display().to_string(), d.path().join("R").display().to_string());
        assert_eq!(run_args(&["diff", "--compare", "binary", &l, &r]).0, EXIT_SAME);
        write(d.path(), "R/y.txt", "new");
        let (code, out) = run_args(&["diff", "--compare", "binary", &l, &r]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(out.contains("y.txt"), "{out}");
    }

    #[test]
    fn merge_clean_and_conflicting() {
        let d = tempfile::tempdir().unwrap();
        let base = write(d.path(), "base", "a\nb\nc\n");
        let left = write(d.path(), "left", "A\nb\nc\n");
        let right = write(d.path(), "right", "a\nb\nC\n");
        let out_path = d.path().join("out");
        let (code, _) = run_args(&["merge", &left, &right, &base, "-o", &out_path.display().to_string()]);
        assert_eq!(code, EXIT_SAME);
        assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "A\nb\nC\n");
        let right2 = write(d.path(), "right2", "X\nb\nc\n");
        let (code, out) = run_args(&["merge", &left, &right2, &base]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(out.contains("<<<<<<<") && out.contains(">>>>>>>"));
    }

    #[test]
    fn report_and_sync() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.txt", "x\n");
        let b = write(d.path(), "b.txt", "y\n");
        let html = d.path().join("r.html");
        let patch = d.path().join("r.patch");
        let (code, _) = run_args(&["report", &a, &b, "--html", &html.display().to_string(), "--patch", &patch.display().to_string()]);
        assert_eq!(code, EXIT_DIFFERENT);
        assert!(std::fs::read_to_string(&html).unwrap().contains("<html"));
        assert!(std::fs::read_to_string(&patch).unwrap().contains("-x"));
        assert_eq!(run_args(&["report", &a, &b]).0, EXIT_ERROR);

        write(d.path(), "L/new.txt", "n");
        std::fs::create_dir_all(d.path().join("R")).unwrap();
        let (l, r) = (d.path().join("L").display().to_string(), d.path().join("R").display().to_string());
        let (code, out) = run_args(&["sync", &l, &r, "--dry-run"]);
        assert_eq!(code, EXIT_SAME);
        assert!(out.contains("new.txt") && out.contains("Dry run"));
        assert!(!d.path().join("R/new.txt").exists());
        let (code, _) = run_args(&["sync", &l, &r, "--mode", "mirror-right"]);
        assert_eq!(code, EXIT_SAME);
        assert!(d.path().join("R/new.txt").exists());
    }

    #[test]
    fn setup_git_prints_commands() {
        let (code, out) = run_args(&["setup-git"]);
        assert_eq!(code, EXIT_SAME);
        assert!(out.contains("mergetool.diffcraft.cmd"));
    }

    #[test]
    fn bad_regex_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.txt", "x\n");
        assert_eq!(run_args(&["diff", "--ignore-pattern", "(", &a, &a]).0, EXIT_ERROR);
    }
}
