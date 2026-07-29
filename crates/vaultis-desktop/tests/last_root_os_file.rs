//! The one file vaultis writes outside a vault root — `<OS data dir>/last_root.txt` —
//! exercised through its REAL code path.
//!
//! Why this lives in `tests/` rather than beside the unit tests: `launch::load_last_root`
//! and `launch::save_last_root` both short-circuit under `cfg!(test)` (returning `None` /
//! doing nothing), deliberately, so that a unit-test run can neither read nor clobber the
//! developer's actual `last_root.txt` at its one fixed OS path. The consequence is that the
//! wrapper — the `cfg!(test)` guard, the `ProjectDirs` path resolution in `last_root_file`,
//! and the delegation to the `_from`/`_to` helpers — was reachable by **no test at all**.
//!
//! `cargo mutants` (audit 2026-07-29) demonstrated exactly that: replacing `load_last_root`
//! with `Some("xyzzy")`, or `last_root_file` with `None`, left the entire 671-test suite
//! green. Those are not equivalent mutants — either one visibly changes what the start page
//! does on launch — so they are a genuine coverage hole, and this is their kill-test.
//!
//! An INTEGRATION test compiles `vaultis` as an ordinary dependency, so `cfg!(test)` is
//! **false** inside the library and the real path runs. `XDG_DATA_HOME` redirects
//! `ProjectDirs` at a scratch directory, so the developer's real file is never touched.
//! Linux-only because that redirection is XDG-specific: macOS resolves to `~/Library` and
//! Windows to a known folder, neither of which honours the variable.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

/// Point `ProjectDirs` at a scratch dir for this process, before anything reads it.
///
/// `set_var` is `unsafe` in edition 2024 because it races other threads reading the
/// environment. This is sound here: it is the first statement of a single-test binary, so
/// no other thread exists yet, and the whole file is one `#[test]` for that reason.
fn redirect_data_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vaultis-lastroot-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
    dir
}

#[test]
fn last_root_round_trips_through_the_real_os_file() {
    let data_home = redirect_data_home();

    // Nothing saved yet: a first run must report "no remembered root", not a guessed path.
    // (Kills `load_last_root -> Some(..)`.)
    assert_eq!(vaultis::launch::load_last_root(), None, "first run has no remembered root");

    // A real root round-trips verbatim — including a space, which a naive shell-quoting
    // implementation would mangle. (Kills `last_root_file -> None`, which would make the
    // save a silent no-op, and both `load_last_root -> Some(<wrong>)` mutants.)
    let root = "/home/somebody/my vaults";
    vaultis::launch::save_last_root(root);
    assert_eq!(
        vaultis::launch::load_last_root().as_deref(),
        Some(root),
        "the saved root must come back exactly"
    );

    // It lands where the module comment says it does, as a regular file, and holds nothing
    // but that one path. (Kills `last_root_file -> Some(Default::default())`, whose empty
    // path cannot produce this file.)
    let file = data_home.join("vaultis").join("last_root.txt");
    let meta = std::fs::symlink_metadata(&file).expect("last_root.txt exists at the OS data dir");
    assert!(meta.is_file(), "a regular file, never a symlink");
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), root);

    // Saving again REPLACES rather than appends, so the file never accumulates history of
    // every vault root this machine has opened.
    vaultis::launch::save_last_root("/second/root");
    assert_eq!(vaultis::launch::load_last_root().as_deref(), Some("/second/root"));
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "/second/root");

    // And the hardening holds on the real path too, not just on `load_last_root_from`: a
    // symlink planted at the file is refused rather than followed (audit 2026-07-29 L-1).
    std::fs::remove_file(&file).unwrap();
    let elsewhere = data_home.join("pointed-at.txt");
    std::fs::write(&elsewhere, "/contents/of/the/target\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
    assert_eq!(
        vaultis::launch::load_last_root(),
        None,
        "a symlinked last_root.txt must not be followed, even via the real OS path"
    );

    let _ = std::fs::remove_dir_all(&data_home);
}
