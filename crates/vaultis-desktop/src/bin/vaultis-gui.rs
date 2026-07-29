//! Windowed launcher for the graphical UI.
//!
//! This is the same as `vaultis [DIR] [--write]`, but built as a Windows
//! **GUI-subsystem** executable. A normal Rust binary uses the *console* subsystem,
//! so when Windows launches it from Explorer or a shortcut it first allocates a
//! command window and *then* the GUI opens on top — the "two windows" you'd
//! otherwise see. Marking this binary `windows_subsystem = "windows"` tells the
//! linker it is a GUI app, so no console window is ever created.
//!
//! The console `vaultis` binary is unchanged and remains the home for the CLI
//! subcommands and the `--tui` terminal UI — those genuinely need a console, which
//! is exactly what a GUI-subsystem app does not have. (Mirrors the classic
//! `python.exe` vs `pythonw.exe` split.) The attribute is inert on non-Windows
//! targets, so this binary is just "the GUI" everywhere else.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::process::ExitCode;

use vaultis::launch::Interactive;

fn main() -> ExitCode {
    // Everything after the program name. The windowed launcher only understands an
    // interactive launch (optional vault DIR + `--write`); CLI subcommands belong to
    // the console binary, which can actually show their output.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Reject a malformed command line (e.g. more than one vault DIR) before opening a window.
    let (path, writable) = match vaultis::launch::resolve_interactive(&args) {
        Ok(Interactive::Open { path, writable }) => (path, writable),
        // `--version` / `--help`: print and exit rather than open anything. Under the Windows
        // GUI subsystem there is no console for this to land in, so it prints nothing there and
        // simply exits 0 — attaching to the parent console needs `unsafe`, which this crate
        // forbids, and the console binary is the documented place to ask. Exiting quietly still
        // beats what this used to do, which was to open a window on a vault directory named
        // `--version`.
        Ok(Interactive::Version) => {
            println!("{}", vaultis::launch::VERSION_LINE);
            return ExitCode::SUCCESS;
        }
        Ok(Interactive::Help) => {
            println!("{}", vaultis::launch::GUI_USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("vaultis error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match vaultis::gui::run(path, writable) {
        Ok(()) => ExitCode::SUCCESS,
        // No console is attached under the Windows GUI subsystem, so this `eprintln`
        // is a no-op there; it still surfaces a fatal launch error on other
        // platforms (and the GUI handles ordinary errors — bad password, locked
        // vault — inside its own window, not via this path).
        Err(e) => {
            eprintln!("vaultis error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
