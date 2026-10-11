//! Git integration: register DiffCraft as `git difftool` / `git mergetool`.

use std::path::Path;
use std::process::Command;

/// The `git config` entries that register `exe` as difftool and mergetool `diffcraft`.
/// With `make_default`, `diff.tool` and `merge.tool` are set too.
pub fn config_entries(exe: &Path, make_default: bool) -> Vec<(String, String)> {
    // Forward slashes work everywhere git runs (including Git for Windows' shell).
    let exe = exe.display().to_string().replace('\\', "/");
    let mut v = vec![
        ("difftool.diffcraft.cmd".to_owned(), format!("\"{exe}\" \"$LOCAL\" \"$REMOTE\"")),
        ("mergetool.diffcraft.cmd".to_owned(), format!("\"{exe}\" \"$LOCAL\" \"$REMOTE\" \"$BASE\" --output \"$MERGED\"")),
        ("mergetool.diffcraft.trustExitCode".to_owned(), "true".to_owned()),
        ("difftool.prompt".to_owned(), "false".to_owned()),
    ];
    if make_default {
        v.push(("diff.tool".to_owned(), "diffcraft".to_owned()));
        v.push(("merge.tool".to_owned(), "diffcraft".to_owned()));
    }
    v
}

/// The same entries as shell commands, for copying.
pub fn shell_commands(exe: &Path, make_default: bool) -> String {
    config_entries(exe, make_default).iter().map(|(k, v)| format!("git config --global {k} '{}'", v.replace('\'', r"'\''"))).collect::<Vec<_>>().join("\n")
}

/// Write the entries into the user's global git config by running `git config --global`.
pub fn apply_global(exe: &Path, make_default: bool) -> Result<(), String> {
    for (k, v) in config_entries(exe, make_default) {
        let out =
            Command::new("git").args(["config", "--global", &k, &v]).output().map_err(|e| format!("could not run git (is it installed and on PATH?): {e}"))?;
        if !out.status.success() {
            return Err(format!("git config {k} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_quote_paths() {
        let e = config_entries(Path::new(r"C:\Program Files\DiffCraft\diffcraft.exe"), true);
        assert_eq!(e[0].1, "\"C:/Program Files/DiffCraft/diffcraft.exe\" \"$LOCAL\" \"$REMOTE\"");
        assert!(e[1].1.ends_with("--output \"$MERGED\""));
        assert_eq!(e.len(), 6);
        assert!(shell_commands(Path::new("/usr/bin/diffcraft"), false).starts_with("git config --global difftool.diffcraft.cmd '\"/usr/bin/diffcraft\""));
    }
}
