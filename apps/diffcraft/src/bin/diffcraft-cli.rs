//! Console build of `diffcraft`: identical behaviour, but always attached to the terminal
//! (needed on Windows, where the GUI binary has no console).

fn main() {
    std::process::exit(diffcraft_app::main_entry());
}
