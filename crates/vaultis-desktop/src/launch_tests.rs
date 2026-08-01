//! Unit tests for the parent module ([`super`], `launch.rs`), split into their own
//! file via `#[cfg(test)] #[path = "launch_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use super::*;

#[test]
fn default_vault_path_ends_with_vault_pmv() {
    assert!(default_vault_path().ends_with("vault.pmv"));
}

#[test]
fn vault_file_appends_vault_pmv_to_the_dir() {
    assert_eq!(vault_file("/some/dir"), PathBuf::from("/some/dir/vault.pmv"));
}

/// Unwrap an expected `Open` outcome into the `(path, writable)` pair the rest of
/// these assertions are written against.
fn opened(args: &[String]) -> (PathBuf, bool) {
    match resolve_interactive(args) {
        Ok(Interactive::Open { path, writable }) => (path, writable),
        Ok(_) => panic!("expected an Open outcome for {args:?}"),
        Err(e) => panic!("{args:?} failed to resolve: {e}"),
    }
}

#[test]
fn resolve_interactive_reads_dir_and_write_flag() {
    // No args → default path, read-only.
    let (p, w) = opened(&[]);
    assert!(p.ends_with("vault.pmv"));
    assert!(!w);

    // A positional dir is used; flag order doesn't matter.
    let (p, w) = opened(&["--write".into(), "/v".into()]);
    assert_eq!(p, PathBuf::from("/v/vault.pmv"));
    assert!(w);

    // The first NON-flag argument is the directory.
    let (p, w) = opened(&["/v".into(), "--write".into()]);
    assert_eq!(p, PathBuf::from("/v/vault.pmv"));
    assert!(w);

    // A directory whose NAME begins with '-' is still recognized (only the exact known
    // flags are treated as flags), so this binary opens the same vault the console
    // binary would — not the silent default.
    let (p, w) = opened(&["-weird-dir".into()]);
    assert_eq!(p, PathBuf::from("-weird-dir/vault.pmv"));
    assert!(!w);
}

/// The four print-and-exit tokens must be ANSWERED, never resolved as a vault
/// directory. Before audit 2026-07-29 round 2 (L-1) only the console binary knew
/// them, so `vaultis-gui --version` opened `--version/vault.pmv` — and on Windows,
/// where the GUI subsystem swallows stderr, it did so with no message at all.
#[test]
fn resolve_interactive_answers_help_and_version_instead_of_opening_them() {
    for tok in ["--help", "-h"] {
        assert!(
            matches!(resolve_interactive(&[tok.into()]), Ok(Interactive::Help)),
            "{tok} must be answered, not treated as a vault DIR"
        );
    }
    for tok in ["--version", "-V"] {
        assert!(
            matches!(resolve_interactive(&[tok.into()]), Ok(Interactive::Version)),
            "{tok} must be answered, not treated as a vault DIR"
        );
    }
    // They win from any position, and over the other flags — the same precedence the
    // console binary applies, so a shared command line means the same thing to both.
    assert!(matches!(
        resolve_interactive(&["/v".into(), "--write".into(), "--version".into()]),
        Ok(Interactive::Version)
    ));
    assert!(
        matches!(resolve_interactive(&["--version".into(), "--help".into()]), Ok(Interactive::Help)),
        "--help outranks --version, as in main.rs"
    );
    // And a directory whose name merely RESEMBLES one of them is still a directory.
    let Ok(Interactive::Open { path, .. }) = resolve_interactive(&["--versions".into()]) else {
        panic!("--versions is not one of the four tokens; it is a directory name");
    };
    assert_eq!(path, PathBuf::from("--versions/vault.pmv"));
}

#[test]
fn resolve_interactive_rejects_extra_positionals() {
    // More than one vault DIR is an error (arg-count validation), not a silent open of the
    // first with the rest ignored. Flags don't count toward the positional total.
    assert!(resolve_interactive(&["/a".into(), "/b".into()]).is_err());
    assert!(resolve_interactive(&["/a".into(), "--write".into(), "/b".into()]).is_err());
    // Exactly one positional (with any flags) is still fine.
    assert!(resolve_interactive(&["/a".into(), "--write".into(), "--tui".into()]).is_ok());
}

#[test]
fn join_root_name_combines_or_falls_back_to_root() {
    assert_eq!(join_root_name("/a/b", "vault1"), PathBuf::from("/a/b/vault1").display().to_string());
    // The ROOT is trimmed, but the NAME is joined VERBATIM, so a folder name round-trips
    // through discovery → join (trimming it would make a whitespace-named folder unopenable).
    assert_eq!(join_root_name("  /a/b ", "vault1"), PathBuf::from("/a/b/vault1").display().to_string());
    assert_eq!(join_root_name("/a/b", " vault1 "), PathBuf::from("/a/b/ vault1 ").display().to_string());
    // Empty / all-whitespace name → the root itself (a vault sitting directly at the root).
    assert_eq!(join_root_name("/a/b", ""), "/a/b");
    assert_eq!(join_root_name("/a/b", "   "), "/a/b");
}

#[test]
fn quoted_root_is_accepted_by_join_and_discovery() {
    // A root pasted with surrounding double quotes ("Copy as path" in a file manager, or a
    // path copied out of a shell command) must resolve to the folder it NAMES — both when
    // deriving the open target and when scanning for vaults, or the dropdown and the
    // unlock target would disagree.
    let root = std::env::temp_dir().join(format!("pmv-quoted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha").join(VAULT_FILE), b"x").unwrap();
    let plain = root.to_str().unwrap().to_string();
    let quoted = format!("\"{plain}\"");

    assert_eq!(join_root_name(&quoted, "alpha"), join_root_name(&plain, "alpha"));
    assert!(vault_file(&join_root_name(&quoted, "alpha")).exists(), "the quoted root opens the real vault");
    assert_eq!(discover_vaults(&quoted).vaults, vec!["alpha".to_string()], "the quoted root scans the real folder");
    // Whitespace around the quotes is trimmed too, and an empty name still yields the root.
    assert_eq!(join_root_name(&format!("  {quoted}  "), ""), plain);
    // A LONE quote at one end is a legitimate (if odd) path character — left alone, so a
    // folder actually named `"weird` is still reachable.
    assert_eq!(join_root_name("\"/a/b", "v"), PathBuf::from("\"/a/b/v").display().to_string());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn discovered_whitespace_named_vault_round_trips_through_join() {
    // Regression: a vault folder whose name has surrounding whitespace must be OPENABLE
    // from the dropdown — discover_vaults returns the raw name, and join_root_name must
    // produce the path that actually holds its vault.pmv (not a trimmed, non-existent path,
    // which would silently flip the start page to "Create").
    let root = std::env::temp_dir().join(format!("pmv-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let weird = " spaced "; // leading + trailing space in the folder name
    std::fs::create_dir_all(root.join(weird)).unwrap();
    std::fs::write(root.join(weird).join(VAULT_FILE), b"x").unwrap();

    let scan = discover_vaults(root.to_str().unwrap());
    assert!(scan.vaults.contains(&weird.to_string()), "whitespace-named vault is discovered: {:?}", scan.vaults);
    let joined = join_root_name(root.to_str().unwrap(), weird);
    assert!(vault_file(&joined).exists(), "join must resolve to the discovered vault's vault.pmv: {joined}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn initial_root_and_name_honors_an_explicit_launch() {
    // An explicit launch (path != default) always wins: parent is the root, folder the name.
    let p = PathBuf::from("/vaults/work/vault.pmv");
    assert_eq!(initial_root_and_name(&p, Some("/elsewhere")), ("/vaults".to_string(), "work".to_string()));
}

/// A bare launch with no remembered root starts EMPTY.
///
/// This used to fall back to the per-user default vault's own parent
/// (`~/.local/share/vaultis`). That fallback is gone: pre-filling the box with a path the
/// user never chose invites them to create a vault somewhere they will not think to back
/// up. Empty is the honest answer to "you have not said where your vaults are, and none
/// was remembered either."
#[test]
fn initial_root_and_name_is_empty_without_an_argument_or_remembered_root() {
    let (root, name) = initial_root_and_name(&default_vault_path(), None);
    assert!(root.is_empty(), "no argument or remembered root -> empty root, got {root:?}");
    assert!(name.is_empty(), "...and no pre-selected vault, got {name:?}");
}

/// The remembered root seeds only the ROOT — never a specific vault. The user still picks
/// (or types) which vault inside it to open.
#[test]
fn initial_root_and_name_never_pre_selects_a_vault_from_the_remembered_root() {
    let (root, name) = initial_root_and_name(&default_vault_path(), Some("/remembered"));
    assert_eq!(root, "/remembered");
    assert!(name.is_empty(), "no pre-selection from the remembered root, got {name:?}");

    // An explicit DIR argument still wins over the remembered root.
    let p = PathBuf::from("/vaults/work/vault.pmv");
    assert_eq!(initial_root_and_name(&p, Some("/remembered")), ("/vaults".to_string(), "work".to_string()));
}

/// With no argument, the last root a vault was successfully opened from (persisted outside
/// any vault root — see `save_last_root`) seeds the start page: browsed, nothing selected.
#[test]
fn initial_root_and_name_falls_back_to_the_remembered_root() {
    let (root, name) = initial_root_and_name(&default_vault_path(), Some("/my/vaults"));
    assert_eq!(root, "/my/vaults");
    assert!(name.is_empty(), "the remembered root never pre-selects a vault, got {name:?}");
}

/// REGRESSION (audit 2026-07-29, M-1). The working directory must never seed the start
/// page — not even when it is a perfectly good folder of vaults.
///
/// `make-shortcuts.ps1` points both Desktop shortcuts' working directory at the INSTALL
/// folder, and since the sample vault ships beside the executables that folder contains
/// `sample-vault/vault.pmv`. Under the old `argument > cwd > last_root` precedence every
/// shortcut launch therefore resolved to the install directory showing `sample-vault`,
/// and — because cwd outranked it — permanently shadowed the user's real remembered root.
#[test]
fn the_working_directory_never_seeds_the_start_page() {
    // A cwd that IS a textbook folder of vaults, made current for this process.
    let base = std::env::temp_dir().join(format!("pmv-nocwd-{}-{}", std::process::id(), nanos_for_test()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sample-vault")).unwrap();
    std::fs::write(base.join("sample-vault").join(VAULT_FILE), b"x").unwrap();
    // It really does look like a root — otherwise this test would pass vacuously.
    assert_eq!(discover_vaults(base.to_str().unwrap()).vaults, vec!["sample-vault".to_string()]);

    let restore = std::env::current_dir().unwrap();
    std::env::set_current_dir(&base).unwrap();

    // Nothing remembered: EMPTY, not the install folder full of practice data.
    let (root, name) = initial_root_and_name(&default_vault_path(), None);
    assert!(root.is_empty(), "the cwd must not seed the root, got {root:?}");
    assert!(name.is_empty(), "...and must not pre-select a vault, got {name:?}");

    // A remembered root wins outright — it can no longer be shadowed by the cwd.
    let (root, _) = initial_root_and_name(&default_vault_path(), Some("/my/real/vaults"));
    assert_eq!(root, "/my/real/vaults", "the remembered root must not be shadowed by the cwd");

    std::env::set_current_dir(restore).unwrap();
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn last_root_round_trips_through_a_plain_file() {
    let path = std::env::temp_dir().join(format!("pmv-last-root-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // Missing file -> None, not an error.
    assert_eq!(load_last_root_from(&path), None);

    // Saving creates the file (and would create its parent dir, were one missing) and it
    // reads back exactly what was saved.
    save_last_root_to(&path, "/my/vaults");
    assert!(path.is_file());
    assert_eq!(load_last_root_from(&path), Some("/my/vaults".to_string()));

    // Saving again overwrites rather than appending.
    save_last_root_to(&path, "/other/root");
    assert_eq!(load_last_root_from(&path), Some("/other/root".to_string()));

    // Blank contents (whitespace only) read back as None, same as a missing file.
    std::fs::write(&path, "   \n").unwrap();
    assert_eq!(load_last_root_from(&path), None);

    let _ = std::fs::remove_file(&path);
}

/// REGRESSION (audit 2026-07-29, L-1). `last_root.txt` must be read through the same
/// hardened path as `prefs.json`: `O_NOFOLLOW` + a hard size cap.
///
/// It originally used `std::fs::read_to_string`, which FOLLOWS a final-component symlink
/// and allocates without bound — while the WRITER for the same file already used the
/// hardened `write_atomic`. Both halves are asserted here because the bug was precisely
/// that one half had the guard and the other did not.
#[cfg(unix)]
#[test]
fn last_root_read_refuses_a_symlink_and_caps_the_size() {
    let base = std::env::temp_dir().join(format!("pmv-l1-{}-{}", std::process::id(), nanos_for_test()));
    std::fs::create_dir_all(&base).unwrap();

    // (1) A symlink planted at the path is REFUSED, not read through. Before the fix this
    // returned the target's contents, which then land in the start page's vault-root box.
    let pointed_at = base.join("pointed-at.txt");
    std::fs::write(&pointed_at, "/contents/of/the/symlink/target\n").unwrap();
    let link = base.join("last_root.txt");
    std::os::unix::fs::symlink(&pointed_at, &link).unwrap();
    assert_eq!(load_last_root_from(&link), None, "a symlinked last_root.txt must not be followed");

    // ...while a REAL file at the same path still works, so the guard did not just
    // disable the feature.
    std::fs::remove_file(&link).unwrap();
    save_last_root_to(&link, "/my/vaults");
    assert_eq!(load_last_root_from(&link), Some("/my/vaults".to_string()), "a real file still reads");

    // (2) An over-size file is refused rather than allocated. Before the fix a 1 MiB file
    // was read whole; there was no ceiling at all.
    let big = base.join("big.txt");
    std::fs::write(&big, "x".repeat(MAX_LAST_ROOT_SIZE as usize + 1)).unwrap();
    assert_eq!(load_last_root_from(&big), None, "a file past the cap must be refused, not read");

    // A file exactly AT the cap is still fine — the cap is a ceiling, not an off-by-one.
    let at_cap = base.join("at-cap.txt");
    std::fs::write(&at_cap, "y".repeat(MAX_LAST_ROOT_SIZE as usize)).unwrap();
    assert_eq!(
        load_last_root_from(&at_cap).map(|s| s.len()),
        Some(MAX_LAST_ROOT_SIZE as usize),
        "exactly at the cap is accepted"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn discover_vaults_unreadable_root_is_empty_with_warning() {
    // A root that does not exist can't be read → empty list, explanatory warning.
    let scan = discover_vaults("/nonexistent-vaultis-root-zzz-9173");
    assert!(scan.vaults.is_empty());
    assert!(scan.warning.is_some(), "missing root should warn");
}

#[test]
fn discover_vaults_finds_only_subdirs_with_a_vault_file() {
    // Build a throwaway root: two vault subdirs, one empty subdir, one loose file.
    let root = std::env::temp_dir().join(format!("pmv-scan-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Bravo")).unwrap();
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::create_dir_all(root.join("not-a-vault")).unwrap();
    std::fs::write(root.join("Bravo").join(VAULT_FILE), b"x").unwrap();
    std::fs::write(root.join("alpha").join(VAULT_FILE), b"x").unwrap();
    std::fs::write(root.join("loose.txt"), b"x").unwrap();

    let scan = discover_vaults(root.to_str().unwrap());
    // Only the two dirs holding a vault.pmv, sorted case-insensitively.
    assert_eq!(scan.vaults, vec!["alpha".to_string(), "Bravo".to_string()]);
    assert!(scan.warning.is_none(), "no inaccessible entries expected: {:?}", scan.warning);

    let _ = std::fs::remove_dir_all(&root);
}

/// Both layouts a real copy of vaultis arrives in are found, and neither is invented:
/// only a directory that actually holds a `vault.pmv` counts, because the caller uses
/// `None` to HIDE the "Sample vault" button rather than offer one that fails on click.
#[test]
fn sample_vault_is_found_beside_the_exe_and_one_level_up() {
    let base = std::env::temp_dir().join(format!("pmv-sample-{}-{}", std::process::id(), nanos_for_test()));
    let _ = std::fs::remove_dir_all(&base);
    // The from-source layout: exe at target/<profile>/, sample at target/sample-vault.
    let target = base.join("target");
    let exe_dir = target.join("release");
    std::fs::create_dir_all(&exe_dir).unwrap();

    // Nothing anywhere yet.
    assert_eq!(sample_vault_beside(&exe_dir), None, "no sample -> no path guessed");

    // An empty directory of the right NAME is still not a sample vault.
    std::fs::create_dir_all(target.join("sample-vault")).unwrap();
    assert_eq!(sample_vault_beside(&exe_dir), None, "a directory with no vault.pmv does not count");

    std::fs::write(target.join("sample-vault").join(VAULT_FILE), b"x").unwrap();
    assert_eq!(sample_vault_beside(&exe_dir), Some(target.join("sample-vault")), "from-source layout");

    // The installed layout: the release zip extracts sample-vault beside the two exes.
    let installed = base.join("Programs").join("vaultis");
    std::fs::create_dir_all(installed.join("sample-vault")).unwrap();
    assert_eq!(sample_vault_beside(&installed), None, "still needs a real vault.pmv");
    std::fs::write(installed.join("sample-vault").join(VAULT_FILE), b"x").unwrap();
    assert_eq!(sample_vault_beside(&installed), Some(installed.join("sample-vault")), "installed layout");

    // When BOTH could match, the one beside the exe wins: it belongs to the copy of the
    // program actually running, which is the sample its Help describes.
    let both_exe = base.join("both").join("release");
    std::fs::create_dir_all(both_exe.join("sample-vault")).unwrap();
    std::fs::create_dir_all(base.join("both").join("sample-vault")).unwrap();
    std::fs::write(both_exe.join("sample-vault").join(VAULT_FILE), b"x").unwrap();
    std::fs::write(base.join("both").join("sample-vault").join(VAULT_FILE), b"x").unwrap();
    assert_eq!(sample_vault_beside(&both_exe), Some(both_exe.join("sample-vault")), "beside the exe wins");

    let _ = std::fs::remove_dir_all(&base);
}

/// Distinct temp paths within one test binary run, so this test can't collide with a
/// sibling that used the same pid-derived name.
fn nanos_for_test() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}
