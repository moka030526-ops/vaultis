//! Unit tests for the parent module ([`super`], `lib.rs`), split into their own
//! file via `#[cfg(test)] #[path = "lib_tests.rs"] mod tests;` so the tests do not sit
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

// Hermetic prefs round-trip via the path-parametrized helpers (never touches the
// real `~/.config` prefs). Uses a nanosecond-tagged temp dir for isolation.
fn tmp_prefs_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("pmprefs-export-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fmt_money_formats_and_never_shows_negative_zero() {
    assert_eq!(fmt_money(0.0), "$0");
    assert_eq!(fmt_money(1_234_567.8), "$1,234,568");
    assert_eq!(fmt_money(-2500.0), "-$2,500");
    // Anything that rounds to zero — including a tiny NEGATIVE f64 residue (realistic on the
    // Summary Net column when assets exactly equal liabilities) — renders "$0", never "-$0".
    assert_eq!(fmt_money(-1e-16), "$0");
    assert_eq!(fmt_money(-0.4), "$0");
    assert_eq!(fmt_money(0.4), "$0");
    assert_eq!(fmt_money(-0.6), "-$1"); // rounds to a nonzero magnitude -> keeps its sign
}

/// `export_dir` is a SESSION value now, never read from a file.
///
/// It names where cleartext exports land (the CSV carries every password), and the only
/// file the app writes lives beside the vault folders where anyone with media access but
/// no passwords can edit it. So even a `prefs.json` that explicitly sets the key must be
/// ignored — this pins that.
#[test]
fn export_dir_is_never_loaded_from_a_file() {
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    std::fs::write(&p, br#"{"export_dir":"/attacker/drop"}"#).unwrap();
    assert_eq!(load_export_dir(dir.to_str().unwrap()), "", "a planted export_dir is ignored");
    assert!(
        !effective_prefs_obj_from(&p).contains_key("export_dir"),
        "export_dir is filtered out of the effective prefs entirely"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same rule for `reveal_all_default`: a planted value must not unmask passwords.
#[test]
fn reveal_all_default_is_never_loaded_from_a_file() {
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    std::fs::write(&p, br#"{"reveal_all_default":true}"#).unwrap();
    assert!(!load_reveal_all_default(dir.to_str().unwrap()), "a planted reveal-all is ignored");
    assert!(
        !effective_prefs_obj_from(&p).contains_key("reveal_all_default"),
        "reveal_all_default is filtered out of the effective prefs entirely"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The read filter is an ALLOWLIST: only the five cosmetic keys survive, whatever else
/// the file carries. This is the security boundary for a file the app does not control.
#[test]
fn prefs_read_filter_admits_only_the_cosmetic_keys() {
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    std::fs::write(
        &p,
        br#"{"theme":"nord","ui_scale":"large","font":"monospace",
             "group_assets_default":true,"group_accounts_default":true,
             "export_dir":"/attacker/drop","reveal_all_default":true,
             "vault_root":"/attacker","last_vault":"evil","unknown_key":1}"#,
    )
    .unwrap();
    let obj = effective_prefs_obj_from(&p);
    let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
    got.sort_unstable();
    let mut want: Vec<&str> = PREFS_KEYS.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "only the cosmetic allowlist survives the read filter");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn view_default_bool_prefs_round_trip_and_default_false() {
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    // Absent file -> false for each flag.
    assert!(!load_group_assets_default_from(&p));
    assert!(!load_group_accounts_default_from(&p));
    // Save true, then load round-trips for each.
    save_group_assets_default_to(&p, true);
    save_group_accounts_default_to(&p, true);
    assert!(load_group_assets_default_from(&p));
    assert!(load_group_accounts_default_from(&p));
    // Toggling back to false is preserved as false.
    save_group_assets_default_to(&p, false);
    save_group_accounts_default_to(&p, false);
    assert!(!load_group_assets_default_from(&p));
    assert!(!load_group_accounts_default_from(&p));
    // A non-bool value falls back to false rather than panicking.
    std::fs::write(&p, br#"{"group_assets_default":"yes"}"#).unwrap();
    assert!(!load_group_assets_default_from(&p), "non-bool value falls back to false");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn view_default_bool_prefs_coexist_with_other_keys() {
    // Each flag's read-modify-write must not clobber a co-resident cosmetic key.
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    std::fs::write(&p, br#"{"theme":"solarized"}"#).unwrap();
    save_group_assets_default_to(&p, true);
    save_group_accounts_default_to(&p, true);
    let obj = read_prefs_obj(&p);
    assert_eq!(obj.get("theme").and_then(|v| v.as_str()), Some("solarized"), "theme preserved");
    assert_eq!(obj.get("group_assets_default").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(obj.get("group_accounts_default").and_then(|v| v.as_bool()), Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_prefs_obj_is_bounded_and_symlink_safe() {
    let dir = tmp_prefs_dir();
    let p = dir.join("prefs.json");
    // Over-cap file is rejected before the body is parsed (DoS guard) -> empty map.
    std::fs::write(&p, vec![b'{'; (MAX_PREFS_SIZE as usize) + 1]).unwrap();
    assert!(read_prefs_obj(&p).is_empty(), "over-cap prefs rejected");
    // A symlinked prefs file is refused even if its target is valid.
    #[cfg(unix)]
    {
        let real = dir.join("real.json");
        std::fs::write(&real, br#"{"export_dir":"/x"}"#).unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_prefs_obj(&link).is_empty(), "symlinked prefs refused");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `prefs.json` is resolved relative to the VAULT ROOT and nowhere else, and a quoted
/// pasted root still finds it.
#[test]
fn prefs_path_is_the_vault_root_and_nothing_else() {
    assert!(prefs_path("").is_none(), "no root -> no prefs path");
    assert!(prefs_path("   ").is_none(), "whitespace-only root is treated as unset");
    assert_eq!(prefs_path("/my/vaults"), Some(PathBuf::from("/my/vaults/prefs.json")));
    assert_eq!(
        prefs_path("  \"/my/vaults\"  "),
        Some(PathBuf::from("/my/vaults/prefs.json")),
        "a pasted \"Copy as path\" root still resolves"
    );
}

#[test]
fn a_foreign_vaults_prefs_cannot_choose_where_cleartext_exports_go() {
    // THE security property behind `PREFS_KEYS`. The vault-root `prefs.json`
    // ships with the vault media (a USB stick, a restored archive, a vault handed to an
    // heir), so its author is whoever produced the vault. It must not be able to set:
    //   * `export_dir`        — where the front-ends write the per-tab CSV (every
    //                           account and portal password in the CLEAR) and every
    //                           decrypted document. A foreign value could redirect
    //                           those secrets to a world-readable folder, a synced
    //                           folder, a Windows UNC share, or the vault dir itself.
    //   * `reveal_all_default` — opens every password tab UNMASKED.
    let dir = tmp_prefs_dir();
    let vault_root = dir.join("vault");
    std::fs::create_dir_all(&vault_root).unwrap();
    std::fs::write(
        vault_root.join("prefs.json"),
        br#"{"export_dir":"/tmp/attacker-drop","reveal_all_default":true,
             "vault_root":"/tmp/elsewhere","last_vault":"decoy","theme":"solarized",
             "group_accounts_default":true}"#,
    )
    .unwrap();
    let vroot = vault_root.to_str().unwrap();
    let eff = effective_prefs_obj(vroot);

    assert_eq!(eff.get("export_dir"), None, "a tampered prefs.json must NOT choose the cleartext export destination");
    assert_eq!(eff.get("reveal_all_default"), None, "a tampered prefs.json must NOT unmask passwords");
    assert_eq!(eff.get("vault_root"), None, "prefs.json cannot relocate the vault root");
    assert_eq!(eff.get("last_vault"), None, "prefs.json cannot choose a vault");
    // Cosmetic keys DO travel with the root — that is the whole point of the file.
    assert_eq!(eff.get("theme").and_then(|v| v.as_str()), Some("solarized"), "theme travels with the root");
    assert_eq!(
        eff.get("group_accounts_default").and_then(|v| v.as_bool()),
        Some(true),
        "cosmetic grouping default travels with the root"
    );
    // And the loader that feeds the export path reports "unset", so the UI asks the user.
    assert_eq!(load_export_dir(vroot), "", "load_export_dir ignores any stored value");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checked_export_dir_refuses_unset_and_anything_inside_the_vault_folder() {
    // A per-tab CSV holds every account and portal password in the CLEAR, and a document
    // export is the decrypted file. Either one written INSIDE the vault folder is swept
    // into the user's next backup or folder sync of that vault. The CLI has refused this
    // since the extract/export-tree guards; this is the shared check the GUI and TUI use.
    let dir = tmp_prefs_dir();
    let vault_dir = dir.join("myvault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let vault_path = vault_dir.join("vault.pmv");

    // Unset -> the "pick a folder" prompt, not a write.
    assert!(checked_export_dir(&vault_path, "").is_err(), "unset is refused");
    assert!(checked_export_dir(&vault_path, "   ").is_err(), "whitespace-only is refused");

    // The vault folder itself, and anything beneath it, are refused.
    for inside in [vault_dir.clone(), vault_dir.join("exports"), vault_dir.join("volume").join("deep")] {
        let err = checked_export_dir(&vault_path, &inside.to_string_lossy())
            .expect_err("a destination inside the vault folder must be refused");
        assert!(err.contains("OUTSIDE the vault folder"), "actionable message: {err}");
    }

    // A sibling directory outside the vault is accepted, and comes back normalized
    // (trimmed, with a pasted "Copy as path" quote pair stripped).
    let outside = dir.join("exports");
    assert_eq!(checked_export_dir(&vault_path, &outside.to_string_lossy()).unwrap(), outside);
    let quoted = format!("  \"{}\"  ", outside.display());
    assert_eq!(checked_export_dir(&vault_path, &quoted).unwrap(), outside, "quotes/space normalized");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn checked_export_dir_sees_through_a_symlink_pointing_back_into_the_vault() {
    // The realistic evasion: the export directory is named through a symlink that
    // resolves into the vault folder. A purely textual comparison would call it
    // "outside"; `dest_inside` resolves both sides through the filesystem.
    let dir = tmp_prefs_dir();
    let vault_dir = dir.join("myvault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let vault_path = vault_dir.join("vault.pmv");

    let link = dir.join("looks-external");
    std::os::unix::fs::symlink(&vault_dir, &link).unwrap();
    // The destination itself does not exist yet — the normal case for a fresh export dir.
    let dest = link.join("csvs");
    assert!(!dest.exists());
    assert!(
        checked_export_dir(&vault_path, &dest.to_string_lossy()).is_err(),
        "a symlinked export dir resolving into the vault folder must be refused"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_bounded_nofollow_enforces_the_exact_cap() {
    // Pins the `> max` boundary of the bounded read that replaced `fs::read` in
    // `read_prefs_obj`: exactly `max` bytes is accepted, one over is refused — without
    // ever allocating past the ceiling, however large the file is.
    let dir = tmp_prefs_dir();
    let p = dir.join("blob");
    std::fs::write(&p, vec![b'x'; 100]).unwrap();
    assert_eq!(read_bounded_nofollow(&p, 100).unwrap().len(), 100, "exactly max is accepted");
    assert!(read_bounded_nofollow(&p, 99).is_err(), "max + 1 is refused");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn prefs_read_does_not_follow_a_symlink_even_past_the_stat() {
    // `read_prefs_obj`'s `symlink_metadata` pre-check is an early reject, not the
    // boundary — it is a separate syscall from the read, and the old `fs::read` both
    // FOLLOWED a symlink and allocated without bound. The read itself is now
    // O_NOFOLLOW-capped, which is what makes the check race-proof; assert the read
    // primitive refuses a symlink on its own, with no stat in front of it.
    let dir = tmp_prefs_dir();
    let real = dir.join("real.json");
    std::fs::write(&real, br#"{"export_dir":"/x"}"#).unwrap();
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(
        read_bounded_nofollow(&link, MAX_PREFS_SIZE).is_err(),
        "the read itself refuses a symlink, so winning the stat/read race gains nothing"
    );
    // Reading the real file still works (the guard is about the link, not the content).
    assert!(read_bounded_nofollow(&real, MAX_PREFS_SIZE).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_prefs_obj_replaces_a_symlink_instead_of_writing_through_it() {
    // Atomic temp -> rename: a rename REPLACES a planted symlink at the prefs path
    // rather than writing to whatever it points at, and never leaves a truncated file
    // for a concurrent front-end to read.
    let dir = tmp_prefs_dir();
    let target = dir.join("victim.txt");
    std::fs::write(&target, b"do not touch").unwrap();
    let p = dir.join("prefs.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &p).unwrap();

    save_group_assets_default_to(&p, true);
    assert!(load_group_assets_default_from(&p), "the prefs write landed");
    #[cfg(unix)]
    {
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch", "the symlink target is untouched");
        assert!(!std::fs::symlink_metadata(&p).unwrap().file_type().is_symlink(), "symlink replaced by a real file");
    }
    // No temp files are left behind on the success path.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no .tmp leftovers: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_money_reports_a_non_finite_total_as_out_of_range() {
    // `parse_approx_value` rejects a non-finite FIELD, but the Summary SUMS many finite
    // values, so two near-f64::MAX entries add to +inf. `f64 as u64` saturates, so inf
    // used to render as the literal "$18,446,744,073,709,551,615" and NaN as "$0" — a
    // fabricated figure shown as a real one in a financial view.
    assert_eq!(fmt_money(f64::INFINITY), "$—");
    assert_eq!(fmt_money(f64::NEG_INFINITY), "$—");
    assert_eq!(fmt_money(f64::NAN), "$—");
    assert_eq!(fmt_money(f64::MAX + f64::MAX), "$—", "an overflowing sum is not a number we invent");
    // Ordinary values are untouched by the guard.
    assert_eq!(fmt_money(1500.0), "$1,500");
    assert_eq!(fmt_money(f64::MAX), "$18,446,744,073,709,551,615", "finite-but-huge still saturates, as before");
}

#[test]
fn fmt_money_groups_thousands_and_signs() {
    assert_eq!(fmt_money(0.0), "$0");
    assert_eq!(fmt_money(1500.0), "$1,500");
    assert_eq!(fmt_money(1_234_567.8), "$1,234,568"); // rounds, groups
    assert_eq!(fmt_money(-2500.0), "-$2,500");
    assert_eq!(fmt_money(999.0), "$999");
}

#[test]
fn clipboard_tick_decision_obeys_deadline_and_preserves_unseen_status() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let future = now + Duration::from_secs(60);
    // Nothing scheduled, or deadline not reached → no action.
    assert_eq!(clipboard_tick_decision(None, now, ""), None);
    assert_eq!(clipboard_tick_decision(Some(future), now, "anything"), None);
    // Deadline reached + a blank or "Copied …" status → wipe and show the cleared notice.
    assert_eq!(
        clipboard_tick_decision(Some(now), now, ""),
        Some(Some("Clipboard cleared.".to_string()))
    );
    assert_eq!(
        clipboard_tick_decision(Some(now), now, "Copied password to clipboard."),
        Some(Some("Clipboard cleared.".to_string()))
    );
    // Deadline reached but an important status is showing → wipe, but DON'T clobber it
    // (the core rule: a "Save failed: …" the user hasn't seen must survive the auto-clear).
    assert_eq!(clipboard_tick_decision(Some(now), now, "Save failed: disk full"), Some(None));
}
