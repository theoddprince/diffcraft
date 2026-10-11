// The GUI binary has no console window on Windows (release builds); `diffcraft-cli` is its
// console twin for scripts and terminals.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    std::process::exit(diffcraft_app::main_entry());
}
