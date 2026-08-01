//! Unit tests for the parent module ([`super`], `main.rs`), split into their own
//! file via `#[cfg(test)] #[path = "main_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

// `super::` refers to the parent module (this file), pulling in the private
// function under test.
use super::safe_relative_path;
use std::path::{Component, Path, PathBuf};

/// A path is "contained" if it is relative and has no `..`, root, or drive
/// component — i.e. it can never escape the directory it is joined to.
// `p: &Path` is a borrowed (read-only) path. `.components()` yields each path
// segment; `.all(|c| ...)` is true only if the closure holds for every one.
fn contained(p: &Path) -> bool {
    !p.is_absolute()
        && p.components()
            .all(|c| !matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
}

// `#[test]` marks a function as a test case the test runner will execute.
// `assert_eq!`/`assert!` fail (and thus fail the test) if their condition is not met.
#[test]
fn safe_path_normal_tree() {
    let p = safe_relative_path("/statements/2026", "q1.pdf", "id");
    assert_eq!(p, PathBuf::from("statements/2026/q1.pdf"));
    assert!(contained(&p));
}

#[test]
fn compact_target_rejects_implicit_default_when_value_flag_present() {
    use super::{CompactFlags, compact_target, default_vault_path, vault_file};
    let s = |v: &str| v.to_string();

    // No DIR + a value-taking flag that may have eaten it → refuse (the footgun).
    let f = CompactFlags { backup_dest: Some(s("/some/dir")), ..Default::default() };
    assert!(compact_target(&[s("compact")], &f).is_err(), "swallowed-DIR case must be rejected");
    let f2 = CompactFlags { history_before: Some(s("2026-01-01")), ..Default::default() };
    assert!(compact_target(&[s("compact")], &f2).is_err());

    // Explicit DIR alongside the value-flag → unambiguous, accepted.
    let f = CompactFlags { backup_dest: Some(s("/dest")), ..Default::default() };
    assert_eq!(
        compact_target(&[s("compact"), s("/my/vault")], &f).unwrap(),
        vault_file("/my/vault")
    );

    // Implicit default with NO value-flag → still allowed (the common case).
    let none = CompactFlags::default();
    assert_eq!(compact_target(&[s("compact")], &none).unwrap(), default_vault_path());
}

#[test]
fn safe_path_rejects_all_traversal() {
    let cases = [
        ("../../etc", "passwd"),
        ("..\\..\\windows", "system32"),
        ("a/../../b", "f"),
        ("/abs/path", "/etc/shadow"),
        ("C:\\Windows", "x.dll"),
        ("....//....//", ".."),
    ];
    for (loc, name) in cases {
        let p = safe_relative_path(loc, name, "fallbackid");
        assert!(contained(&p), "must stay contained: {loc:?} {name:?} -> {p:?}");
    }
}

#[test]
fn safe_path_strips_bidi_and_control_chars() {
    // A crafted manifest path must not write a SPOOFED on-disk filename or inject escapes
    // into the printed extract list: U+202E (RLO) and control chars become '_', matching
    // export_document_into.
    let p = safe_relative_path("docs", "invoice\u{202e}fdp.exe", "id");
    let s = p.to_string_lossy();
    assert!(!s.contains('\u{202e}'), "bidi override stripped from extract filename: {s}");
    assert!(s.contains('_'), "spoof char replaced with '_': {s}");
    assert!(contained(&p));
    // ...also in a location (directory) component.
    let p2 = safe_relative_path("a\u{202e}b", "f.txt", "id");
    assert!(!p2.to_string_lossy().contains('\u{202e}'));
    assert!(contained(&p2));
}

#[test]
fn safe_path_empty_filename_uses_id() {
    assert_eq!(safe_relative_path("/d", "", "abc123"), PathBuf::from("d/abc123.bin"));
    assert_eq!(safe_relative_path("", "..", "abc123"), PathBuf::from("abc123.bin"));
}

#[test]
fn safe_path_drive_letter_dropped() {
    let p = safe_relative_path("C:", "x.txt", "id");
    assert!(contained(&p));
    assert_eq!(p, PathBuf::from("x.txt"));
}

#[test]
fn safe_path_repairs_windows_reserved_names_exactly_like_the_core_exporter() {
    // Audit 2026-07-25 round 2. This function used to carry its OWN copy of the
    // reserved-name rule, and the copy had drifted from the core's `doc_tree_relpath`
    // in two ways: it knew a shorter list, and it DROPPED the offending component where
    // the core RENAMES it with a `_` prefix. So one vault extracted to two different
    // trees depending on which front-end wrote it, and dropping a directory level
    // collapsed documents that differ only by that level into a single folder.
    //
    // Both now call `records::is_windows_reserved_name` and both prefix `_`.
    assert_eq!(safe_relative_path("", "CON", "id1"), PathBuf::from("_CON"));
    assert_eq!(safe_relative_path("", "nul.txt", "id2"), PathBuf::from("_nul.txt"));
    assert_eq!(safe_relative_path("", "COM1", "id3"), PathBuf::from("_COM1"));
    // A reserved DIRECTORY component is renamed, so the level survives and two documents
    // that differ only by it stay in separate folders.
    assert_eq!(safe_relative_path("CON/sub", "f.txt", "id5"), PathBuf::from("_CON/sub/f.txt"));
    assert_ne!(
        safe_relative_path("CON/sub", "f.txt", "id"),
        safe_relative_path("sub", "f.txt", "id"),
        "a reserved level must not collapse onto the path that never had it"
    );
    // The widened list reaches this front-end too (console handles + superscript COM).
    assert_eq!(safe_relative_path("", "CONIN$.txt", "id6"), PathBuf::from("_CONIN$.txt"));
    assert_eq!(safe_relative_path("", "com\u{00B9}.log", "id7"), PathBuf::from("_com\u{00B9}.log"));
    // Trailing dots/spaces are still stripped (Windows aliases them away), and COM0/LPT0
    // are still ordinary names.
    assert_eq!(safe_relative_path("", "report.pdf. .", "id8"), PathBuf::from("report.pdf"));
    assert_eq!(safe_relative_path("", "LPT0.log", "id9"), PathBuf::from("LPT0.log"));
}

/// The two path sanitizers — the core's `doc_tree_relpath` (used by the GUI/TUI
/// `export_document_into` and by `export_tree`'s `documents/` view) and this CLI's
/// `safe_relative_path` — must lay out the same stored path identically, or `vaultis
/// extract` and the windowed app produce different trees for one vault. Silent drift
/// between these duplicated rules is exactly what audit 2026-07-25 round 2 found, so
/// pin the agreement rather than trusting the two to be edited in step.
#[test]
fn safe_path_agrees_with_the_core_exporter_on_reserved_and_spoofy_paths() {
    for vpath in [
        "CON/sub/f.txt",
        "docs/CONIN$/deed.pdf",
        "com\u{00B9}/x.log",
        "trust-will/con/nul.pdf",
        "docs/invoice\u{202e}fdp.exe",
        "a\u{061C}b/deed\u{E0041}.pdf",
        "MK/real-estate/main-st/20260725-120000_deed.pdf",
        "plain/ordinary.pdf",
    ] {
        let (location, filename) = vpath.rsplit_once('/').unwrap_or(("", vpath));
        let mine = safe_relative_path(location, filename, "theid");
        let core = vaultis::vault::doc_tree_relpath(vpath, "theid");
        assert_eq!(mine, core, "sanitizers disagree on {vpath:?}");
    }
}

#[test]
fn unique_path_avoids_existing() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vaultis-uniq-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("doc.txt");
    // Non-existing path is returned as-is.
    assert_eq!(super::unique_path(p.clone(), None), p);
    std::fs::write(&p, b"x").unwrap();
    // Existing path gets a `_N` suffix that doesn't yet exist.
    let u = super::unique_path(p.clone(), None);
    assert_ne!(u, p);
    assert!(!u.exists());
    assert_eq!(u.file_name().unwrap().to_str().unwrap(), "doc_1.txt");
    // With a fallback token, an existing path falls back to the id-disambiguated name
    // (the _1..9999 range still wins first; this just verifies the token is wired).
    let u2 = super::unique_path(p.clone(), Some("abc123"));
    assert_ne!(u2, p);
    assert!(!u2.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_backup_copies_file() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // backup() copies the whole vault tree as-is; a dummy file suffices here.
    let vdir = std::env::temp_dir().join(format!("vaultis-clibk-{nanos}"));
    std::fs::create_dir_all(&vdir).unwrap();
    let vault = vdir.join("vault.pmv");
    std::fs::write(&vault, b"PMVAULT\0 fake").unwrap();
    let dest = std::env::temp_dir().join(format!("vaultis-clibk-dest-{nanos}"));
    super::cli_backup(vault.clone(), dest.clone()).unwrap();
    let n = std::fs::read_dir(&dest).unwrap().count();
    assert_eq!(n, 1, "one timestamped backup directory created");
    let _ = std::fs::remove_dir_all(&vdir);
    let _ = std::fs::remove_dir_all(&dest);
}

// (path-helper tests moved to `vaultis::launch`, which now owns them.)

// ---- compact CLI flag parsing & guards ---------------------------------

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn extract_compact_flags_parses_every_flag_form() {
    let (f, rest) = super::extract_compact_flags(argv(&[
        "compact", "DIR", "--volume", "--json", "--history-before", "2025-01-01", "--no-backup", "--dry-run",
        "--backup", "/tmp/x", "extra",
    ]))
    .unwrap();
    assert!(f.volume && f.json && f.no_backup && f.dry_run && f.any());
    assert_eq!(f.history_before.as_deref(), Some("2025-01-01"));
    assert_eq!(f.backup_dest.as_deref(), Some("/tmp/x"));
    // Non-flag args are preserved in order for positional/dispatch handling.
    assert_eq!(rest, argv(&["compact", "DIR", "extra"]));
}

#[test]
fn extract_compact_flags_accepts_equals_forms_and_history_all() {
    let (f, _) = super::extract_compact_flags(argv(&["--history-before=2024-12-31", "--backup=/d"])).unwrap();
    assert_eq!(f.history_before.as_deref(), Some("2024-12-31"));
    assert_eq!(f.backup_dest.as_deref(), Some("/d"));
    let (g, _) = super::extract_compact_flags(argv(&["--history-all"])).unwrap();
    assert!(g.history_all && g.any());
}

#[test]
fn extract_compact_flags_errors_on_missing_values() {
    assert!(super::extract_compact_flags(argv(&["--history-before"])).is_err());
    assert!(super::extract_compact_flags(argv(&["--backup"])).is_err());
}

#[test]
fn extract_compact_flags_absent_means_none_and_passthrough() {
    let (f, rest) = super::extract_compact_flags(argv(&["decrypt", "DIR"])).unwrap();
    assert!(!f.any());
    assert_eq!(rest, argv(&["decrypt", "DIR"]));
}

#[test]
fn default_backup_dir_is_a_sibling_outside_the_vault() {
    let vault_dir = Path::new("/home/u/myvault");
    let d = super::default_backup_dir(vault_dir);
    assert_eq!(d, PathBuf::from("/home/u/myvault-backups"));
    assert!(!d.starts_with(vault_dir), "default backup dir must be outside the vault dir");
}

#[test]
fn dest_inside_flags_self_and_children_allows_siblings() {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let vault_dir = std::env::temp_dir().join(format!("pmdi-{nanos}"));
    std::fs::create_dir_all(&vault_dir).unwrap();
    // The vault dir itself, and an existing child, are "inside".
    assert!(super::dest_inside(&vault_dir, &vault_dir));
    let child = vault_dir.join("volume");
    std::fs::create_dir_all(&child).unwrap();
    assert!(super::dest_inside(&vault_dir, &child));
    // A not-yet-existing absolute child is still caught (lexical fallback).
    assert!(super::dest_inside(&vault_dir, &vault_dir.join("backups")));
    // A sibling directory outside the vault is allowed.
    let sibling = vault_dir.parent().unwrap().join(format!("pmdi-out-{nanos}"));
    assert!(!super::dest_inside(&vault_dir, &sibling));
    let _ = std::fs::remove_dir_all(&vault_dir);
}

#[test]
fn dest_inside_catches_dot_slash_relative_child() {
    // Regression: the lexical fallback used a raw component-wise starts_with that
    // missed a leading "./", wrongly allowing a backup INSIDE the vault tree.
    // These names don't exist, so canonicalize() fails and the lexical path runs.
    let vault = Path::new("pmrel-vault-zzz");
    assert!(super::dest_inside(vault, Path::new("./pmrel-vault-zzz/inside")));
    assert!(super::dest_inside(vault, Path::new("pmrel-vault-zzz/inside")));
    // A genuinely separate relative dir is still allowed.
    assert!(!super::dest_inside(vault, Path::new("pmrel-other-zzz")));
}

#[test]
fn compact_flags_any_detects_each_flag() {
    use super::CompactFlags;
    assert!(!CompactFlags::default().any(), "no flags set -> any() is false");
    // Each flag ALONE makes any() true. (Kills the `||`->`&&` mutants: with `&&`,
    // a single set flag would yield false.)
    assert!(CompactFlags { volume: true, ..Default::default() }.any());
    assert!(CompactFlags { json: true, ..Default::default() }.any());
    assert!(CompactFlags { history_before: Some("2025-01-01".into()), ..Default::default() }.any());
    assert!(CompactFlags { history_all: true, ..Default::default() }.any());
    assert!(CompactFlags { no_backup: true, ..Default::default() }.any());
    assert!(CompactFlags { backup_dest: Some("/tmp/x".into()), ..Default::default() }.any());
    assert!(CompactFlags { dry_run: true, ..Default::default() }.any());
}

#[test]
fn safe_path_drops_dot_and_dotdot_components() {
    // A location or filename component that is exactly "." or ".." must be dropped,
    // never kept as a path component (kills the `||`->`&&` mutant in clean's
    // empty/"."/".." guard, which would otherwise admit traversal).
    let traversal = |p: &Path| {
        p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir | Component::RootDir | Component::Prefix(_)))
    };
    for (loc, name) in [("..", "f.txt"), (".", "f.txt"), ("ok", ".."), ("ok", "."), ("..", "..")] {
        let p = safe_relative_path(loc, name, "fallbackid");
        assert!(!traversal(&p), "({loc:?},{name:?}) produced a traversal component: {p:?}");
    }
}

#[cfg(unix)]
#[test]
fn dest_inside_resolves_a_symlinked_dest_via_canonical() {
    // A dest that is a SYMLINK pointing into the vault dir must be detected as
    // inside — the canonical-both arm resolves it; a purely lexical check would
    // miss it. (Kills the deletion of the `(Some(v), Some(d))` arm in dest_inside.)
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let vault = std::env::temp_dir().join(format!("pmdi-sym-{nanos}"));
    std::fs::create_dir_all(vault.join("sub")).unwrap();
    let link = std::env::temp_dir().join(format!("pmdi-symlink-{nanos}"));
    std::os::unix::fs::symlink(vault.join("sub"), &link).unwrap();
    assert!(super::dest_inside(&vault, &link), "a symlink into the vault resolves to inside");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
#[cfg(unix)]
fn dest_inside_sees_through_a_symlinked_parent_of_a_dest_that_does_not_exist_yet() {
    // The realistic shape, and the one that used to slip through: the destination
    // has NOT been created yet (it is a fresh export/backup dir), and it is named
    // through a symlink that points into the vault directory.
    //
    // With no destination to canonicalize, the old code fell back to comparing the
    // paths as TEXT, which cannot see a symlink — so it answered "outside" and
    // `extract`/`export-tree` wrote a full cleartext mirror inside the live vault
    // directory, next to vault.pmv.
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let vault = std::env::temp_dir().join(format!("pmdi-symparent-{nanos}"));
    std::fs::create_dir_all(&vault).unwrap();

    // `link` -> the vault dir itself. `link/plaintext` does not exist.
    let link = std::env::temp_dir().join(format!("pmdi-symparentlink-{nanos}"));
    std::os::unix::fs::symlink(&vault, &link).unwrap();
    let dest = link.join("plaintext");
    assert!(!dest.exists(), "the destination must NOT exist — that is the whole case");

    assert!(
        super::dest_inside(&vault, &dest),
        "a not-yet-created destination reached through a symlinked parent resolves \
         INSIDE the vault and must be refused"
    );

    // Control: the same shape pointing somewhere else must still be allowed, so the
    // guard is not simply rejecting everything.
    let outside = std::env::temp_dir().join(format!("pmdi-symparent-out-{nanos}"));
    std::fs::create_dir_all(&outside).unwrap();
    let outlink = std::env::temp_dir().join(format!("pmdi-symparent-outlink-{nanos}"));
    std::os::unix::fs::symlink(&outside, &outlink).unwrap();
    assert!(
        !super::dest_inside(&vault, &outlink.join("plaintext")),
        "control: a symlinked parent pointing OUTSIDE the vault stays allowed"
    );

    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_file(&outlink);
    let _ = std::fs::remove_dir_all(&vault);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn cli_compact_rejects_missing_vault_then_bad_flag_combos() {
    use super::CompactFlags;
    // Missing vault: bails before any prompt/validation.
    let f = CompactFlags { volume: true, ..Default::default() };
    assert!(super::cli_compact(&argv(&["compact", "/no/such/pmvault/dir"]), &f).is_err());

    // A dummy (non-empty) vault dir so path.exists() passes; the flag-combination
    // validation below all bails BEFORE opening the vault or prompting.
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("pmclic-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("vault.pmv"), b"PMVAULT\0 dummy").unwrap();
    let d = dir.to_str().unwrap();
    let bad: &[CompactFlags] = &[
        // no mode flag
        CompactFlags::default(),
        // --json without a retention choice
        CompactFlags { json: true, ..Default::default() },
        // retention flag without --json
        CompactFlags { volume: true, history_all: true, ..Default::default() },
        // both retention choices at once
        CompactFlags { json: true, history_before: Some("2025-01-01".into()), history_all: true, ..Default::default() },
        // unparseable cutoff date
        CompactFlags { json: true, history_before: Some("not-a-date".into()), ..Default::default() },
    ];
    for f in bad {
        assert!(super::cli_compact(&argv(&["compact", d]), f).is_err(), "expected validation error");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn part_flag_is_parsed_and_stripped() {
    let s = |v: &str| v.to_string();
    // `--part N` form: value consumed, rest preserved in order.
    let (p, rest) = super::extract_part_flag(vec![s("manifest"), s("--part"), s("2"), s("dir")]).unwrap();
    assert_eq!(p, Some(2));
    assert_eq!(rest, vec![s("manifest"), s("dir")]);
    // `--part=N` form.
    let (p, rest) = super::extract_part_flag(vec![s("--part=5"), s("x")]).unwrap();
    assert_eq!(p, Some(5));
    assert_eq!(rest, vec![s("x")]);
    // Absent → None, args untouched.
    let (p, rest) = super::extract_part_flag(vec![s("extract")]).unwrap();
    assert_eq!(p, None);
    assert_eq!(rest, vec![s("extract")]);
    // Missing or non-numeric values are errors.
    assert!(super::extract_part_flag(vec![s("--part")]).is_err());
    assert!(super::extract_part_flag(vec![s("--part"), s("abc")]).is_err());
    assert!(super::extract_part_flag(vec![s("--part=-1")]).is_err());
}

#[test]
fn partial_plaintext_warning_attributes_only_new_entries() {
    // Audit F1: after a failed plaintext export, only the entries THIS run created are
    // flagged — never the user's pre-existing files (so the shred advice can never
    // target their unrelated data).
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("vaultis-f1-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    // Pre-existing, unrelated user files in the chosen output directory.
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
    std::fs::write(dir.join("photo.jpg"), b"mine").unwrap();
    let before = super::dir_entry_names(&dir);
    // Before any of our writes, nothing is attributed to this run.
    assert!(super::new_entries_since(&dir, &before).is_empty(), "pre-existing files are never attributed");
    // Simulate this run writing a partial mirror (a file and a subdir).
    std::fs::write(dir.join("vault.json"), b"secret").unwrap();
    std::fs::create_dir_all(dir.join("volume")).unwrap();
    let new: std::collections::HashSet<std::ffi::OsString> = super::new_entries_since(&dir, &before)
        .into_iter()
        .map(|p| p.file_name().unwrap().to_owned())
        .collect();
    assert!(new.contains(std::ffi::OsStr::new("vault.json")), "newly-written file is flagged");
    assert!(new.contains(std::ffi::OsStr::new("volume")), "newly-written dir is flagged");
    assert!(!new.contains(std::ffi::OsStr::new("notes.txt")), "pre-existing file NOT flagged");
    assert!(!new.contains(std::ffi::OsStr::new("photo.jpg")), "pre-existing file NOT flagged");
    let _ = std::fs::remove_dir_all(&dir);
}
