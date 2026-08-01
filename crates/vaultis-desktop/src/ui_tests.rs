//! Unit tests for the parent module ([`super`], `ui.rs`), split into their own
//! file via `#[cfg(test)] #[path = "ui_tests.rs"] mod tests;` so the tests do not sit
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
use crate::crypto::KdfParams;
use std::time::{SystemTime, UNIX_EPOCH};

fn fast() -> KdfParams {
    KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }
}

fn tmp_vault(tag: &str) -> PathBuf {
    // A unique per-test directory; the vault file name is fixed (vault.pmv),
    // matching production where the user controls only the directory.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("vaultis-ui-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("vault.pmv")
}

/// An `App` with a freshly-created, unlocked vault on the Browse screen — the
/// state a user reaches after a successful create/unlock, without rendering.
fn app_unlocked(tag: &str) -> (App, PathBuf) {
    let path = tmp_vault(tag);
    let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut app = App::new(path.clone(), true);
    app.vault = Some(ov);
    app.screen = Screen::Browse;
    (app, path)
}

/// A read-only `App` unlocked over an existing vault that already has one
/// account, on the Browse screen.
fn app_read_only(tag: &str) -> (App, PathBuf) {
    let path = tmp_vault(tag);
    {
        let mut ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        records::upsert(&mut ov.vault.accounts, Account::new().unwrap());
        ov.save().unwrap();
    }
    let ov = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    let mut app = App::new(path.clone(), false);
    app.vault = Some(ov);
    app.screen = Screen::Browse;
    (app, path)
}

fn cleanup(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
        // Also drop any sibling export directory the test used (see `export_out`).
        // The vault dir name is nanosecond-tagged and unique, so this only ever
        // matches directories this test created.
        if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
            && let Ok(rd) = std::fs::read_dir(parent)
        {
            let prefix = format!("{name}-");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
}

/// An export destination OUTSIDE the vault directory — a sibling named
/// `<vault-dir>-<tag>`. The front-ends refuse an export directory inside the vault
/// folder ([`crate::checked_export_dir`]): a CSV holds every password in the clear and a
/// document export is the decrypted file, so either one landing next to `vault.pmv`
/// would be swept up by the user's next backup of the vault. Export tests therefore
/// write where a real user has to. `cleanup` removes these siblings too.
fn export_out(vault_path: &Path, tag: &str) -> PathBuf {
    let dir = vault_path.parent().expect("vault path has a parent");
    PathBuf::from(format!("{}-{tag}", dir.display()))
}

// Tiny constructors for synthetic key events: `key(code)` = a plain key,
// `ctrl(c)` = Ctrl + a character. Used to drive `handle_key` in tests.
// (`.unwrap()` is used liberally throughout the tests below: it panics on
// `Err`/`None`, which is exactly how a test should fail.)
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn start_page_vault_dir_field_switches_mode_and_creates() {
    // The collapsed start page exposes a Root field (focus 0) and a "Vault" name row
    // (focus 1); the open target is <root>/<name>. It pre-fills root=parent, name=folder,
    // flips Unlock<->Create as the name changes, and (in --write mode) creates the vault.
    let base = std::env::temp_dir()
        .join(format!("vaultis-ui-startdir-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&base).unwrap();

    let mut app = App::new(base.join("fresh").join("vault.pmv"), true);
    assert_eq!(app.auth.mode, AuthMode::Create, "no vault yet -> Create");
    assert!(app.auth_start_page(), "start page shows the Root + Vault rows");
    assert_eq!(app.auth_dir, base.join("fresh").display().to_string(), "dir = root/name");
    assert_eq!(app.auth_root, base.display().to_string(), "root = parent");
    assert_eq!(app.auth_name, "fresh", "name = launch folder");
    // Focus 0 is the editable Root field; typing there edits the root, not a password.
    assert!(app.on_auth_root_field());
    app.handle_auth_key(key(KeyCode::Char('X')));
    assert!(app.auth_root.ends_with('X'), "typing on focus 0 edits the root");
    app.handle_auth_key(key(KeyCode::Backspace));
    // Focus 1 is the editable Vault name row; typing there edits the name → directory.
    app.auth.focus = 1;
    assert!(app.on_auth_vault_field());
    app.handle_auth_key(key(KeyCode::Char('X')));
    assert!(app.auth_name.ends_with('X'), "typing on the Vault row edits the name");
    assert!(app.auth_dir.ends_with('X'), "...which re-derives the directory");
    app.handle_auth_key(key(KeyCode::Backspace));

    // Type a brand-new vault name and create the vault there.
    let fresh = base.join("brandnew");
    app.auth_name = "brandnew".into();
    app.recompute_auth_path();
    assert_eq!(app.auth_dir, fresh.display().to_string());
    assert_eq!(app.auth.mode, AuthMode::Create);
    // Create has 4 password fields (pw1, confirm1, pw2, confirm2); the lead rows are
    // separate, so fill auth.fields directly.
    app.auth.fields[0].value = "a".into();
    app.auth.fields[1].value = "a".into();
    app.auth.fields[2].value = "b".into();
    app.auth.fields[3].value = "b".into();
    app.submit_auth();
    assert!(app.vault.is_some(), "vault created in the new dir; error: {:?}", app.auth.error);
    assert!(fresh.join("vault.pmv").exists(), "vault.pmv created on disk");

    // A fresh App pointed at that now-existing dir resolves to Unlock.
    let app2 = App::new(fresh.join("vault.pmv"), true);
    assert_eq!(app2.auth.mode, AuthMode::Unlock, "existing vault -> Unlock");
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn start_page_vault_picker_scans_root_and_selects() {
    // Two existing vaults plus an empty dir under a common root. The picker should list
    // exactly the two vaults; cycling it with ←/→ adopts a vault and flips to Unlock.
    let root = std::env::temp_dir()
        .join(format!("vaultis-ui-picker-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::create_dir_all(root.join("beta")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("alpha").join("vault.pmv"), b"x").unwrap();
    std::fs::write(root.join("beta").join("vault.pmv"), b"x").unwrap();

    // Launch pointed at the empty (vault-less) subdir → Create, picker not yet scanned here.
    let mut app = App::new(root.join("empty").join("vault.pmv"), true);
    assert_eq!(app.auth.mode, AuthMode::Create);
    // Point the Root field at the shared root and rescan.
    app.auth_root = root.display().to_string();
    app.refresh_auth_vaults();
    assert_eq!(app.auth_vaults, vec!["alpha".to_string(), "beta".to_string()]);
    assert!(app.auth_scan_warning.is_none());

    // Cycle the picker (focus 1) right: select "alpha" → existing vault → Unlock mode,
    // and auth_dir/path now point inside the root.
    app.auth.focus = 1;
    assert!(app.on_auth_vault_field());
    app.handle_auth_key(key(KeyCode::Right)); // 0 -> 1 (beta)
    assert_eq!(app.auth_name, "beta");
    assert_eq!(app.auth_dir, root.join("beta").display().to_string());
    assert_eq!(app.auth.mode, AuthMode::Unlock, "an existing vault flips to Unlock");
    app.handle_auth_key(key(KeyCode::Left)); // 1 -> 0 (alpha)
    assert_eq!(app.auth_name, "alpha");
    assert_eq!(app.auth_dir, root.join("alpha").display().to_string());
    assert_eq!(app.path, root.join("alpha").join("vault.pmv"));
    // Focus survived the AuthState rebuild on the mode change.
    assert!(app.on_auth_vault_field(), "focus stays on the Vault row after selecting");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn start_page_read_only_cannot_create_in_empty_dir_tui() {
    let base = std::env::temp_dir()
        .join(format!("vaultis-ui-rodir-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&base).unwrap();
    let mut app = App::new(base.join("empty").join("vault.pmv"), false); // read-only
    assert_eq!(app.auth.mode, AuthMode::Create);
    // Two fields, not four: a read-only session cannot create, so it is never asked to
    // CONFIRM a password. (See `read_only_auth_screen_never_offers_to_create_a_vault_tui`.)
    assert_eq!(app.auth.fields.len(), 2, "no confirmation fields in a read-only session");
    app.auth.fields[0].value = "a".into();
    app.auth.fields[1].value = "b".into();
    app.submit_auth();
    assert!(app.vault.is_none(), "read-only must not create a vault");
    assert!(
        app.auth.error.as_deref().unwrap_or("").contains("--write"),
        "error explains --write is needed; got {:?}",
        app.auth.error
    );
    std::fs::remove_dir_all(&base).ok();
}

/// The TUI half of `gui::tests::read_only_lock_screen_never_offers_to_create_a_vault`.
/// A read-only session cannot create a vault, so its auth screen must not present
/// creating one: no "Create vault" title, no "choose two passwords" instruction, and no
/// confirmation fields. The two password fields stay, because retyping the root to find
/// the real vault is exactly what a read-only heir needs to do.
#[test]
fn read_only_auth_screen_never_offers_to_create_a_vault_tui() {
    let base = std::env::temp_dir()
        .join(format!("vaultis-ui-ronc-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&base).unwrap();
    let missing = base.join("nothing-here").join("vault.pmv");

    let ro = App::new(missing.clone(), false);
    assert_eq!(ro.auth.mode, AuthMode::Create, "nothing exists at the target");
    assert_eq!(ro.auth.fields.len(), 2, "read-only: no confirmation fields");
    let ro_screen = render_text(&ro);
    assert!(!ro_screen.contains("Create vault"), "read-only must not offer to create:\n{ro_screen}");
    assert!(ro_screen.contains("Unlock vault"), "it reads as the way in:\n{ro_screen}");
    assert!(!ro_screen.contains("Choose two passwords"), "nothing is being set:\n{ro_screen}");
    assert!(!ro_screen.contains("Confirm password"), "nothing to confirm:\n{ro_screen}");
    assert!(ro_screen.contains("Password 1"), "the way in stays usable:\n{ro_screen}");

    // The writable session at the same path is unchanged: creating IS on offer there.
    let rw = App::new(missing, true);
    assert_eq!(rw.auth.mode, AuthMode::Create);
    assert_eq!(rw.auth.fields.len(), 4, "writable: both confirmations are asked for");
    let rw_screen = render_text(&rw);
    assert!(rw_screen.contains("Create vault"), "writable still offers it:\n{rw_screen}");
    assert!(rw_screen.contains("Confirm password"), "writable still confirms:\n{rw_screen}");

    std::fs::remove_dir_all(&base).ok();
}

// `#[test]` marks a function the test runner executes; it takes no args and a
// panic (e.g. a failed `assert_eq!`) means failure.
#[test]
fn cycle_filter_wraps_through_none() {
    let opts = vec!["a".to_string(), "b".to_string()];
    let s = cycle_filter(&None, &opts);
    assert_eq!(s.as_deref(), Some("a"));
    let s = cycle_filter(&s, &opts);
    assert_eq!(s.as_deref(), Some("b"));
    let s = cycle_filter(&s, &opts);
    assert_eq!(s, None); // wraps back to "no filter"
    assert_eq!(cycle_filter(&Some("gone".into()), &opts), None);
    assert_eq!(cycle_filter(&None, &[]), None);
}

#[test]
fn account_username_search_via_slash_key() {
    let (mut app, path) = app_unlocked("uisearch");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        for u in ["alice", "alice2", "bob"] {
            let mut a = Account::new().unwrap();
            a.username = u.into();
            records::upsert(&mut v.accounts, a);
        }
    }
    app.tab = Tab::Accounts;
    assert_eq!(app.current_labels().len(), 3);

    // '/' enters search input mode; typed letters edit the query (not commands).
    assert!(!app.handle_key(key(KeyCode::Char('/'))));
    assert!(app.search_active);
    for c in "ali".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.current_labels().len(), 2, "matches alice + alice2");

    // Enter keeps the query and exits input mode.
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.search_active);
    assert_eq!(app.acct_search, "ali");
    assert_eq!(app.current_labels().len(), 2, "query persists after Enter");

    // Re-enter and Esc CLEARS the query without quitting the app.
    app.handle_key(key(KeyCode::Char('/')));
    let quit = app.handle_key(key(KeyCode::Esc));
    assert!(!quit, "Esc in search mode must not quit");
    assert!(!app.search_active && app.acct_search.is_empty());
    assert_eq!(app.current_labels().len(), 3, "cleared → all accounts");
    cleanup(&path);
}

#[test]
fn account_search_matches_title_too() {
    let (mut app, path) = app_unlocked("uisearchtitle");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.username = "u1".into();
        a.title = "Brokerage account".into();
        records::upsert(&mut v.accounts, a);
        let mut b = Account::new().unwrap();
        b.username = "u2".into();
        b.title = "Email".into();
        records::upsert(&mut v.accounts, b);
    }
    app.tab = Tab::Accounts;
    assert_eq!(app.current_labels().len(), 2);
    // The free-text search matches the TITLE as well as the username.
    app.acct_search = "broker".into();
    let labels = app.current_labels();
    assert_eq!(labels.len(), 1, "title substring matches");
    assert!(labels[0].1.contains("Brokerage"), "the brokerage account: {labels:?}");
    // And still matches by username.
    app.acct_search = "u2".into();
    assert_eq!(app.current_labels().len(), 1, "username substring still matches");
    cleanup(&path);
}

#[test]
fn bool_choice_round_trips() {
    assert_eq!(bool_choice(true), "Yes");
    assert_eq!(bool_choice(false), "No");
    assert_eq!(yes_no(), vec!["No".to_string(), "Yes".to_string()]);
}

#[test]
fn new_account_prepopulates_from_active_filters() {
    let (mut app, path) = app_unlocked("uifilterprefill");
    app.tab = Tab::Accounts;
    app.acct_filter_title = Some("Bank login".into());
    app.acct_filter_type = Some("Financial".into());
    app.acct_filter_subtype = Some("IRA".into());
    app.acct_filter_owner = Some("Alice".into());
    app.acct_search = "alice99".into();
    app.start_edit(false); // "New"
    let es = app.edit.as_ref().unwrap();
    // Account fields: [0] title, [1] type, [2] subtype, [3] owner, [4] username.
    assert_eq!(es.fields[0].value, "Bank login", "title prefilled from filter");
    assert_eq!(es.fields[1].value, "Financial", "type prefilled from filter");
    assert_eq!(es.fields[2].value, "IRA", "subtype prefilled from filter");
    assert_eq!(es.fields[3].value, "Alice", "owner prefilled from filter");
    assert_eq!(es.fields[4].value, "alice99", "username prefilled from search");
    assert!(es.id.is_none(), "still a new (unsaved) record");
    assert!(app.vault.as_ref().unwrap().vault.accounts.is_empty(), "nothing persisted yet");

    // With no filters/search active, a new account starts blank.
    app.acct_filter_title = None;
    app.acct_filter_type = None;
    app.acct_filter_subtype = None;
    app.acct_filter_owner = None;
    app.acct_search.clear();
    app.start_edit(false);
    let es = app.edit.as_ref().unwrap();
    assert_eq!(es.fields[0].value, "", "title blank");
    assert_eq!(es.fields[1].value, "", "type blank");
    assert_eq!(es.fields[4].value, "", "username blank");
    cleanup(&path);
}

#[test]
fn saving_account_moves_active_filter_to_keep_it_visible() {
    let (mut app, path) = app_unlocked("uifsync");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.account_type = "Email".into();
        a.username = "existing".into();
        records::upsert(&mut v.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.acct_filter_type = Some("Email".into()); // active filter
    app.start_edit(false); // New (prefills type=Email from the filter)
    // Title is mandatory; change the type and give it a username.
    app.edit.as_mut().unwrap().fields[0].value = "Work".into(); // title (field 0)
    app.edit.as_mut().unwrap().fields[1].value = "Bank".into(); // type (field 1)
    app.edit.as_mut().unwrap().fields[3].value = "Alice".into(); // owner (field 3, mandatory)
    app.edit.as_mut().unwrap().fields[4].value = "newuser".into(); // username (field 4)
    app.save_edit();
    // The active type filter followed the saved entry instead of hiding it.
    assert_eq!(app.acct_filter_type.as_deref(), Some("Bank"));
    let labels = app.current_labels();
    assert!(labels.iter().any(|(_, l)| l.contains("newuser")), "saved account is visible: {labels:?}");
    cleanup(&path);
}

#[test]
fn saving_account_relaxes_review_and_search_to_keep_it_visible() {
    let (mut app, path) = app_unlocked("uirelax");
    app.tab = Tab::Accounts;
    app.acct_filter_review = true; // review-only active
    app.acct_search = "alice".into(); // username search active
    app.start_edit(false); // New (prefills username=alice from the search)
    app.edit.as_mut().unwrap().fields[0].value = "Mail".into(); // title (mandatory)
    app.edit.as_mut().unwrap().fields[3].value = "Alice".into(); // owner (mandatory)
    app.edit.as_mut().unwrap().fields[4].value = "bob".into(); // username (no longer matches)
    // review (field 9) stays default "No" — saved record is NOT flagged.
    app.save_edit();
    assert!(!app.acct_filter_review, "review-only relaxed so a non-flagged save stays visible");
    assert_eq!(app.acct_search, "", "username search relaxed when it no longer matches the save");
    let labels = app.current_labels();
    assert!(labels.iter().any(|(_, l)| l.contains("bob")), "saved account is visible: {labels:?}");
    cleanup(&path);
}

#[test]
fn reveal_all_overrides_per_account_masking_in_tui() {
    let (mut app, path) = app_unlocked("uirevealall");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.username = "u".into();
        a.password = "SECRETPW".into();
        records::upsert(&mut v.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.selected = 0;
    app.start_edit(true);
    // Masked by default (per-record reveal is off).
    assert!(!render_to_string(&app).contains("SECRETPW"), "password masked by default");
    // The global reveal-all overrides the per-record reveal.
    app.reveal_all = true;
    assert!(render_to_string(&app).contains("SECRETPW"), "reveal-all shows the password");
    cleanup(&path);
}

#[test]
fn re_reveal_all_overrides_portal_masking_and_is_scoped_in_tui() {
    let (mut app, path) = app_unlocked("uirereveal");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut r = RealEstate::new().unwrap();
        r.address = "1 Main St".into();
        r.property_mgmt_password = "PORTALPW".into();
        records::upsert(&mut v.real_estate, r);
    }
    app.tab = Tab::RealEstate;
    app.selected = 0;
    app.start_edit(true);
    // Masked by default (global reveal off).
    assert!(!render_to_string(&app).contains("PORTALPW"), "portal password masked by default");
    // The ACCOUNT reveal-all must NOT reveal RE portals (scoped per tab).
    app.reveal_all = true;
    assert!(!render_to_string(&app).contains("PORTALPW"), "account reveal-all does not leak into RE");
    // The RE reveal-all does reveal them.
    app.re_reveal_all = true;
    assert!(render_to_string(&app).contains("PORTALPW"), "re reveal-all shows the portal password");
    cleanup(&path);
}

#[test]
fn switching_tabs_resets_reveal_all_toggles() {
    // Reveal is momentary: switching tabs must clear `reveal_all`/`re_reveal_all`
    // so a sticky toggle can't silently expose every password on a later visit.
    let (mut app, path) = app_unlocked("revealreset");
    app.tab = Tab::Accounts;
    app.reveal_all = true;
    app.re_reveal_all = true;
    // Any tab-change key routes through `switch_tab`, which clears both toggles.
    app.handle_key(key(KeyCode::Char('6'))); // jump to the Real Estate tab
    assert_eq!(app.tab, Tab::RealEstate);
    assert!(!app.reveal_all, "reveal_all cleared on tab switch");
    assert!(!app.re_reveal_all, "re_reveal_all cleared on tab switch");
    // Arrow-key tab navigation clears them too.
    app.re_reveal_all = true;
    app.handle_key(key(KeyCode::Right));
    assert!(!app.re_reveal_all, "arrow tab-switch also clears reveal");
    cleanup(&path);
}

#[test]
fn choice_keep_current_keeps_off_list_value_selectable() {
    // An off-list value (legacy data / type removed from Config) is prepended so it stays
    // selectable AND arrow-cycling never silently snaps it to a different option.
    let f = Field::choice_keep_current("Type", "Legacy".into(), vec!["A".into(), "B".into()]);
    match &f.kind {
        FieldKind::Choice(opts) => {
            assert_eq!(opts[0], "Legacy", "off-list current value is prepended");
            assert_eq!(opts.len(), 3);
        }
        _ => panic!("expected a Choice field"),
    }
    // Cycle forward then back returns to the original off-list value (not lost).
    let mut g = Field::choice_keep_current("Type", "Legacy".into(), vec!["A".into(), "B".into()]);
    g.cycle(1);
    assert_eq!(g.value, "A");
    g.cycle(-1);
    assert_eq!(g.value, "Legacy", "off-list value preserved through a cycle round-trip");
    // A value already in the list is not duplicated; an empty (new-record) value is not added.
    let h = Field::choice_keep_current("Type", "A".into(), vec!["A".into(), "B".into()]);
    match &h.kind {
        FieldKind::Choice(opts) => assert_eq!(opts.len(), 2, "in-list value not duplicated"),
        _ => unreachable!(),
    }
    let e = Field::choice_keep_current("Type", String::new(), vec!["A".into()]);
    match &e.kind {
        FieldKind::Choice(opts) => assert_eq!(opts.len(), 1, "empty value not prepended"),
        _ => unreachable!(),
    }
}

#[test]
fn reveal_default_pref_persists_reveal_across_tab_switches() {
    // With the "reveal all by default" preference on, switching tabs RE-APPLIES reveal
    // (to the pref) instead of clearing it, so every password tab re-opens revealed.
    let (mut app, path) = app_unlocked("revealdefault");
    app.reveal_default = true;
    app.reveal_all = true;
    app.re_reveal_all = true;
    app.tab = Tab::Accounts;
    app.handle_key(key(KeyCode::Char('6'))); // jump to Real Estate
    assert_eq!(app.tab, Tab::RealEstate);
    assert!(app.reveal_all, "reveal_all re-applied from pref on tab switch");
    assert!(app.re_reveal_all, "re_reveal_all re-applied from pref on tab switch");
    app.handle_key(key(KeyCode::Char('5'))); // back to Accounts
    assert!(app.reveal_all && app.re_reveal_all, "still revealed after another switch");
    cleanup(&path);
}

#[test]
fn config_view_default_checkboxes_toggle_and_apply() {
    // Space/Enter on a view-default checkbox flips the saved-default flag AND applies it
    // to the live view state immediately. (The prefs WRITE is a no-op under cfg(test);
    // the in-memory flag + applied state are what the UI reads.)
    let (mut app, path) = app_unlocked("cfgcheckbox");
    app.handle_key(key(KeyCode::Char('c'))); // open Config (focus resets to 0)
    assert_eq!(app.screen, Screen::Config);
    // Step focus from the first text field onto the first checkbox row.
    for _ in 0..App::CFG_TEXT_FIELDS {
        app.handle_key(key(KeyCode::Down));
    }
    assert_eq!(app.cfg_focus, App::CFG_TEXT_FIELDS);
    // The FIRST checkbox is now group-assets: "reveal all by default" was removed
    // because it is no longer persisted (a `prefs.json` beside the vault folders is
    // writable by anyone with media access but no passwords, so a stored reveal-all
    // could unmask every password on open — see the prefs comment in `lib.rs`).
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.group_assets_default && app.asset_grouped, "group-assets default toggled + applied");
    app.handle_key(key(KeyCode::Down)); // group-accounts checkbox (Enter also toggles)
    app.handle_key(key(KeyCode::Enter));
    assert!(app.group_accounts_default && app.acct_grouped, "group-accounts default toggled + applied");
    // A char typed on a checkbox row must NOT be routed into a text field.
    let export_before = app.cfg_export_dir.clone();
    app.handle_key(key(KeyCode::Char('z')));
    assert_eq!(app.cfg_export_dir, export_before, "typing on a checkbox row leaves text fields untouched");
    // Navigation wraps over all focus rows back to the first text field.
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.cfg_focus, 0, "focus wraps after the last checkbox");
    cleanup(&path);
}

#[test]
fn generate_reveals_via_the_global_toggle_in_tui() {
    // Reveal is global-only now: generating a password turns on the single global
    // reveal (`reveal_all` on Accounts) so the new value is visible — there is no
    // per-record reveal to flip.
    let (mut app, path) = app_unlocked("genglobal");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('n'))); // new record
    assert!(!app.reveal_all, "reveal starts off");
    app.handle_key(ctrl('g')); // generate into the account's password field
    assert!(app.reveal_all, "generate turns on the global reveal");
    assert!(
        !app.edit.as_ref().unwrap().fields[5].value.is_empty(),
        "a password was generated into the password field"
    );
    cleanup(&path);
}

#[test]
fn copy_generate_target_the_focused_password_field() {
    // Mirrors the Real Estate edit form: text fields interleaved with THREE
    // independent portal password fields. The FOCUSED password must be the one
    // copied/generated — not always the first (which would leak/overwrite the
    // Property Mgmt password regardless of which portal the user is editing).
    let fields = vec![
        Field::text("Address", String::new()),
        Field::password("Property Mgmt password", "pm".into()), // index 1
        Field::text("Insurance URL", String::new()),
        Field::password("Insurance password", "ins".into()), // index 3
        Field::password("HOA password", "hoa".into()),       // index 4
    ];
    // Focused on a specific portal password → that exact field.
    assert_eq!(target_password_index(&fields, 3), Some(3), "Insurance focused → Insurance");
    assert_eq!(target_password_index(&fields, 4), Some(4), "HOA focused → HOA");
    // Focused on a non-password field → fall back to the first password.
    assert_eq!(target_password_index(&fields, 0), Some(1));
    assert_eq!(target_password_index(&fields, 2), Some(1));
    // Out-of-range focus → first password (no panic).
    assert_eq!(target_password_index(&fields, 99), Some(1));
    // No password fields at all → None.
    let no_pw = vec![Field::text("a", String::new()), Field::text("b", String::new())];
    assert_eq!(target_password_index(&no_pw, 0), None);
}

#[test]
fn parse_doc_index_validates_one_based_range() {
    // 1-based user input → 0-based index, accepted only within 1..=len.
    assert_eq!(parse_doc_index("1", 3), Some(0));
    assert_eq!(parse_doc_index("3", 3), Some(2));
    assert_eq!(parse_doc_index(" 2 ", 3), Some(1)); // surrounding whitespace trimmed
    assert_eq!(parse_doc_index("0", 3), None); // below range (no zero-based input)
    assert_eq!(parse_doc_index("4", 3), None); // above range
    assert_eq!(parse_doc_index("1", 0), None); // empty list: nothing is valid
    assert_eq!(parse_doc_index("x", 3), None); // not a number
    assert_eq!(parse_doc_index("", 3), None);
}

#[test]
fn tab_titles_are_correct_and_unique() {
    // Pins each tab's on-screen title (kills the "" / "xyzzy" title mutants).
    let titles: Vec<&str> = Tab::ALL.iter().map(|t| t.title()).collect();
    assert_eq!(
        titles,
        vec![
            "URGENT",
            "Instructions",
            "Trust and Will",
            "Assets & Liabilities",
            "Accounts",
            "Real Estate",
            "Taxes",
            "General Documents",
            "Summary",
        ]
    );
    for t in Tab::ALL {
        assert!(!t.title().is_empty());
    }
    let mut uniq = titles.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), titles.len(), "every tab title is distinct");
}

/// Render the current screen to a flat string of cell symbols, so tests can
/// assert on what is actually drawn (a draw fn replaced by a no-op then fails).
fn render_to_string(app: &App) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(160, 200)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
}

#[test]
fn real_estate_edit_screen_renders_its_fields() {
    let (mut app, path) = app_unlocked("uirerender");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.real_estate, RealEstate::new().unwrap());
    }
    app.tab = Tab::RealEstate;
    app.selected = 0;
    app.start_edit(true);
    let screen = render_to_string(&app);
    // Real-Estate-specific labels must be drawn (kills the tab_realestate /
    // draw_edit / portal-rendering no-op mutants).
    assert!(screen.contains("Financing balance"), "RE edit form renders its fields");
    assert!(screen.contains("Property Mgmt"), "RE edit form renders the portal sections");
    cleanup(&path);
}

#[test]
fn summary_tab_renders_aggregated_owner_table() {
    let (mut app, path) = app_unlocked("uisummary");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut mk = |owner: &str, kind: &str, ty: &str, inst: &str, val: &str| {
            let mut a = AssetLiability::new().unwrap();
            a.owner = owner.into();
            a.kind = kind.into();
            a.asset_type = ty.into();
            a.institution = inst.into();
            a.approx_value = val.into();
            records::upsert(&mut v.assets, a);
        };
        mk("Alice", "Asset", "Real Estate", "", "500000");
        mk("Alice", "Asset", "401k", "Fidelity", "200000"); // before-tax (retirement)
        mk("Alice", "Liability", "Mortgage", "", "300000"); // after-tax liability
    }
    app.tab = Tab::Summary;
    let screen = render_to_string(&app);
    assert!(screen.contains("Summary of Assets"), "the Summary title is drawn");
    assert!(screen.contains("Asset Bef-Tax"), "the bucket column headers are drawn");
    assert!(screen.contains("Alice"), "the owner row is drawn");
    assert!(screen.contains("$500,000"), "the real-estate aggregate is shown");
    assert!(screen.contains("$200,000"), "the before-tax (401k) aggregate is shown");
    assert!(screen.contains("$300,000"), "the liability aggregate is shown");
    assert!(screen.contains("TOTAL"), "the grand-total row is drawn");
    cleanup(&path);
}

#[test]
fn taxes_edit_screen_renders_its_fields() {
    let (mut app, path) = app_unlocked("uitaxrender");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.tax_filings, TaxFiling::new().unwrap());
    }
    app.tab = Tab::Taxes;
    app.selected = 0;
    app.start_edit(true);
    let screen = render_to_string(&app);
    assert!(screen.contains("Owner"), "Taxes edit form renders the new Owner field");
    assert!(screen.contains("Filing year"), "Taxes edit form renders its fields");
    cleanup(&path);
}

#[test]
fn tax_owner_survives_edit_commit_in_tui() {
    let (mut app, path) = app_unlocked("uitaxowner");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.tax_filings, TaxFiling::new().unwrap());
    }
    app.tab = Tab::Taxes;
    app.selected = 0;
    app.start_edit(true);
    app.edit.as_mut().unwrap().fields[0].value = "Jane".into(); // owner
    app.edit.as_mut().unwrap().fields[1].value = "2024".into(); // filing year
    let es = app.edit.take().unwrap();
    app.commit_edit_record(&es);
    // f(0)=owner / f(1)=year mapping must survive a UI save (a field reorder
    // that silently dropped owner would fail here).
    let r = &app.vault.as_ref().unwrap().vault.tax_filings[0];
    assert_eq!(r.owner, "Jane");
    assert_eq!(r.year, "2024");
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_writes_accounts_with_password() {
    let (mut app, path) = app_unlocked("uicsvacct");
    let outdir = export_out(&path, "csv");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut v.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.cfg_export_dir = outdir.to_string_lossy().into();
    app.export_current_tab_csv();
    assert!(app.status.starts_with("Exported 1 record"), "status: {}", app.status);
    let entry = std::fs::read_dir(&outdir).unwrap().next().unwrap().unwrap();
    let name = entry.file_name().to_string_lossy().into_owned();
    assert!(name.starts_with("accounts-") && name.ends_with(".csv"), "timestamped name: {name}");
    let body = std::fs::read_to_string(entry.path()).unwrap();
    assert!(body.contains(",password,"), "header has password column");
    assert!(body.contains("hunter2"), "password exported in plaintext (user opted in)");
    assert!(body.contains("Bank"));
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_lists_document_file_names() {
    let (mut app, path) = app_unlocked("uicsvdoc");
    let dir = path.parent().unwrap().to_path_buf();
    let outdir = export_out(&path, "csv");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.tax_filings, TaxFiling::new().unwrap());
    }
    app.tab = Tab::Taxes;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;
    app.edit.as_mut().unwrap().fields[1].value = "2024".into(); // filing year (field 0 is Owner)
    let src = dir.join("w2.txt");
    std::fs::write(&src, b"data").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        es.fields[rc].value = "w2.txt".into();
        es.fields[rc + 1].value = src.to_string_lossy().into();
    }
    app.attach_tax_document(); // uploads the doc and persists the filing
    // The CSV's documents column must show the file NAME, not the internal blob id.
    app.cfg_export_dir = outdir.to_string_lossy().into();
    app.export_current_tab_csv();
    let entry = std::fs::read_dir(&outdir).unwrap().next().unwrap().unwrap();
    let body = std::fs::read_to_string(entry.path()).unwrap();
    assert!(body.contains("w2.txt"), "document file name listed; body: {body}");
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_empty_dir_reports_and_writes_nothing() {
    let (mut app, path) = app_unlocked("uicsvnodir");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.accounts, Account::new().unwrap());
    }
    app.tab = Tab::Accounts;
    app.cfg_export_dir = "   ".into(); // blank/whitespace export dir
    app.export_current_tab_csv();
    assert!(app.status.contains("export directory"), "guides the user to Config: {}", app.status);
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_on_summary_tab_is_a_noop() {
    let (mut app, path) = app_unlocked("uicsvsummary");
    let outdir = export_out(&path, "csv");
    app.tab = Tab::Summary; // the 'e' key fires on any tab; Summary has no records
    app.cfg_export_dir = outdir.to_string_lossy().into();
    assert!(app.build_tab_csv().is_none(), "Summary -> no CSV");
    app.export_current_tab_csv();
    assert_eq!(app.status, "Nothing to export on this tab.");
    assert!(!outdir.exists(), "no file written for the Summary tab");
    cleanup(&path);
}

#[test]
fn exports_into_the_vault_folder_are_refused_in_tui() {
    // TUI twin of the GUI guard: a CSV export carries every account password in the
    // CLEAR, so it must never land inside the vault folder, where the user's next backup
    // or folder sync of that vault would carry the plaintext with it.
    let (mut app, path) = app_unlocked("uioutguard");
    let vault_dir = path.parent().unwrap().to_path_buf();
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut v.accounts, a);
    }
    app.tab = Tab::Accounts;

    for inside in [vault_dir.clone(), vault_dir.join("exports")] {
        app.cfg_export_dir = inside.to_string_lossy().into();
        app.status.clear();
        app.export_current_tab_csv();
        assert!(
            app.status.contains("OUTSIDE the vault folder"),
            "refused with an actionable message: {}",
            app.status
        );
        let stray: Vec<_> = std::fs::read_dir(&vault_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".csv"))
            .collect();
        assert!(stray.is_empty(), "no cleartext CSV written into the vault folder: {stray:?}");
    }

    // The same session exports fine once the directory is outside the vault.
    let outside = export_out(&path, "csv");
    app.cfg_export_dir = outside.to_string_lossy().into();
    app.export_current_tab_csv();
    assert!(app.status.starts_with("Exported"), "an outside directory still works: {}", app.status);
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_works_in_read_only_mode() {
    // CSV export is deliberately available to a READ-ONLY session (the vault owner
    // asked for it). The file is unencrypted and may hold plaintext passwords, so the
    // status line has to say so rather than report a bare success.
    let (mut app, path) = app_unlocked("uicsvro");
    let outdir = export_out(&path, "csv");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.password = "hunter2".into();
        records::upsert(&mut v.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.cfg_export_dir = outdir.to_string_lossy().into();
    app.writable = false; // a read-only session
    app.export_current_tab_csv();
    assert!(app.status.starts_with("Exported"), "export ran read-only: {}", app.status);
    assert!(
        app.status.contains("UNENCRYPTED"),
        "the plaintext warning must ride along with the success: {}",
        app.status
    );
    let written: Vec<_> = std::fs::read_dir(&outdir).unwrap().filter_map(Result::ok).collect();
    assert_eq!(written.len(), 1, "one CSV written");
    let body = std::fs::read_to_string(written[0].path()).unwrap();
    assert!(body.contains("hunter2"), "the CSV carries the plaintext password it warns about");
    cleanup(&path);
}

#[test]
fn tax_document_attach_export_remove_round_trip() {
    let (mut app, path) = app_unlocked("uitaxdoc");
    let dir = path.parent().unwrap().to_path_buf();
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.tax_filings, TaxFiling::new().unwrap());
    }
    app.tab = Tab::Taxes;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;
    app.edit.as_mut().unwrap().fields[0].value = "Jane".into(); // owner
    app.edit.as_mut().unwrap().fields[1].value = "2024".into(); // filing year

    // --- attach ---
    let src = dir.join("w2.txt");
    std::fs::write(&src, b"taxable income").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        es.fields[rc].value = "w2.txt".into(); // doc filename
        es.fields[rc + 1].value = src.to_string_lossy().into(); // upload from
    }
    app.attach_tax_document();
    assert_eq!(app.edit.as_ref().unwrap().tax_docs.len(), 1, "attached one document");
    assert_eq!(app.vault.as_ref().unwrap().vault.tax_filings[0].documents.len(), 1, "and persisted it");

    // --- export: an out-of-range number exports nothing ---
    // Export goes to the configured export dir, recreating the volume folder structure.
    let export_root = export_out(&path, "exports");
    app.cfg_export_dir = export_root.to_string_lossy().into();
    let tax_id = app.vault.as_ref().unwrap().vault.tax_filings[0].documents[0].clone();
    let vpath = app.vault.as_ref().unwrap().doc_path(&tax_id).unwrap();
    app.edit.as_mut().unwrap().fields[rc + 3].value = "2".into(); // doc # (only 1 exists)
    app.export_tax_document();
    assert!(!export_root.exists(), "out-of-range doc # exports nothing");
    assert!(app.status.contains("between 1 and 1"));

    app.edit.as_mut().unwrap().fields[rc + 3].value = "1".into();
    app.export_tax_document();
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(std::fs::read(&exported).unwrap(), b"taxable income", "exported bytes round-trip (status: {})", app.status);

    // --- remove ---
    app.edit.as_mut().unwrap().fields[rc + 3].value = "1".into();
    app.remove_tax_document();
    assert!(app.edit.as_ref().unwrap().tax_docs.is_empty(), "removed the document");
    assert!(app.vault.as_ref().unwrap().vault.tax_filings[0].documents.is_empty(), "and unlinked it");
    cleanup(&path);
}

#[test]
fn real_estate_document_attach_export_remove_round_trip() {
    let (mut app, path) = app_unlocked("uiredoc");
    let dir = path.parent().unwrap().to_path_buf();
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut re = RealEstate::new().unwrap();
        re.address = "1 Main".into();
        records::upsert(&mut v.real_estate, re);
    }
    app.tab = Tab::RealEstate;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;

    let src = dir.join("deed.txt");
    std::fs::write(&src, b"the deed").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        es.fields[rc].value = "deed.txt".into();
        es.fields[rc + 1].value = src.to_string_lossy().into();
    }
    app.attach_re_document();
    assert_eq!(app.edit.as_ref().unwrap().re_docs.len(), 1, "attached one document");
    assert_eq!(app.vault.as_ref().unwrap().vault.real_estate[0].documents.len(), 1, "and persisted it");

    // Export into the configured export dir, recreating the volume folder structure.
    let export_root = export_out(&path, "exports");
    app.cfg_export_dir = export_root.to_string_lossy().into();
    let re_id = app.vault.as_ref().unwrap().vault.real_estate[0].documents[0].clone();
    let vpath = app.vault.as_ref().unwrap().doc_path(&re_id).unwrap();
    app.edit.as_mut().unwrap().fields[rc + 3].value = "1".into();
    app.export_re_document();
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(std::fs::read(&exported).unwrap(), b"the deed", "exported bytes round-trip (status: {})", app.status);

    app.edit.as_mut().unwrap().fields[rc + 3].value = "1".into();
    app.remove_re_document();
    assert!(app.edit.as_ref().unwrap().re_docs.is_empty(), "removed the document");
    assert!(app.vault.as_ref().unwrap().vault.real_estate[0].documents.is_empty(), "and unlinked it");
    cleanup(&path);
}

#[test]
fn general_document_attach_export_detach_and_path_layout() {
    let (mut app, path) = app_unlocked("uigendoc");
    let dir = path.parent().unwrap().to_path_buf();
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.general_documents, GeneralDocument::new().unwrap());
    }
    app.tab = Tab::GeneralDocuments;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields; // Title, Description -> rc == 2
    app.edit.as_mut().unwrap().fields[0].value = "Passport".into(); // drives the auto-group

    let src = dir.join("passport.pdf");
    std::fs::write(&src, b"passport bytes").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        es.fields[rc].value = "passport.pdf".into(); // filename
        es.fields[rc + 1].value = src.to_string_lossy().into(); // upload from
        es.fields[rc + 2].value = "ids".into(); // subfolder
    }
    app.attach_document();
    let id = app.edit.as_ref().unwrap().attached_file_id.clone();
    assert!(id.is_some(), "attached; status: {}", app.status);
    let id = id.unwrap();

    // Uniform layout: /general-documents/<title>/<subfolder>/<ts>_<filename>
    // (no owner on this tab; the timestamp is folded into the filename).
    let vpath = app.vault.as_ref().unwrap().doc_path(&id).unwrap();
    let parts: Vec<&str> = vpath.trim_start_matches('/').split('/').collect();
    assert_eq!(parts.len(), 4, "no timestamp folder level anymore: {vpath}");
    assert_eq!(parts[0], "general-documents");
    assert_eq!(parts[1], "passport", "auto-group from title");
    assert_eq!(parts[2], "ids", "user subfolder");
    let fname = parts[3];
    assert!(fname.ends_with("_passport.pdf"), "user filename, got {fname}");
    assert!(
        records::is_compact_utc(&fname[..15]) && fname.as_bytes()[15] == b'_',
        "filename carries the timestamp prefix, got {fname}"
    );

    // Export into the configured export dir, recreating the volume folder structure.
    let export_root = export_out(&path, "exports");
    app.cfg_export_dir = export_root.to_string_lossy().into();
    app.export_document();
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(std::fs::read(&exported).unwrap(), b"passport bytes", "exported bytes round-trip (status: {})", app.status);

    app.detach_document();
    assert!(app.edit.as_ref().unwrap().attached_file_id.is_none(), "detached");
    assert!(!app.vault.as_ref().unwrap().has_document(&id), "blob reclaimed");
    assert!(app.vault.as_ref().unwrap().vault.general_documents[0].file.is_none(), "unlinked + persisted");
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn attach_document_reports_failure_when_save_fails_in_tui() {
    // Regression (deep-search HIGH): attach_document set "Document uploaded"
    // unconditionally even when persist() failed — a false success that lost the
    // record→document link. With the disk full at the vault write, the status
    // must report the failure and the link must NOT be persisted.
    let (mut app, path) = app_unlocked("uifailattach");
    let dir = path.parent().unwrap().to_path_buf();
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.general_documents, GeneralDocument::new().unwrap());
    }
    app.vault.as_mut().unwrap().save().unwrap(); // persist the seed cleanly first
    app.tab = Tab::GeneralDocuments;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;
    app.edit.as_mut().unwrap().fields[0].value = "Passport".into();
    let src = dir.join("p.pdf");
    std::fs::write(&src, b"bytes").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        es.fields[rc].value = "p.pdf".into();
        es.fields[rc + 1].value = src.to_string_lossy().into();
    }
    // Fail the vault.pmv write (add_document's blob write still succeeds first).
    crate::fault::fail_at("vault.write", 1);
    app.attach_document();
    crate::fault::clear();
    assert!(app.status.contains("Save failed"), "must report failure, not success; was: {}", app.status);
    drop(app); // release the lock
    // The link never reached disk: reopening shows no attached file (no false link).
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.vault.general_documents[0].file.is_none(), "no false link persisted");
    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn start_edit_loads_the_selected_taxes_record() {
    let (mut app, path) = app_unlocked("uitaxsel");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        for y in ["2020", "2021", "2022"] {
            let mut tf = TaxFiling::new().unwrap();
            tf.year = y.into();
            records::upsert(&mut v.tax_filings, tf);
        }
    }
    app.tab = Tab::Taxes;
    let labels = app.current_labels();
    assert_eq!(labels.len(), 3);
    let target_id = labels[1].0.clone();
    app.selected = 1;
    app.start_edit(true);
    let es = app.edit.as_ref().unwrap();
    // Must edit the *selected* record, not a different one (kills the id-lookup
    // `==`→`!=` mutant, which would resolve to the wrong filing).
    assert_eq!(es.id.as_deref(), Some(target_id.as_str()));
    let expected =
        app.vault.as_ref().unwrap().vault.tax_filings.iter().find(|r| r.id == target_id).unwrap().year.clone();
    // field 0 is Owner, field 1 is the filing year.
    assert_eq!(es.fields[1].value, expected, "loads the selected record's fields");
    cleanup(&path);
}

#[test]
fn start_edit_loads_the_selected_real_estate_record() {
    let (mut app, path) = app_unlocked("uiresel");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        for a in ["1 First St", "2 Second St", "3 Third St"] {
            let mut re = RealEstate::new().unwrap();
            re.address = a.into();
            records::upsert(&mut v.real_estate, re);
        }
    }
    app.tab = Tab::RealEstate;
    let labels = app.current_labels();
    assert_eq!(labels.len(), 3);
    let target_id = labels[1].0.clone();
    app.selected = 1;
    app.start_edit(true);
    let es = app.edit.as_ref().unwrap();
    assert_eq!(es.id.as_deref(), Some(target_id.as_str()));
    let expected =
        app.vault.as_ref().unwrap().vault.real_estate.iter().find(|r| r.id == target_id).unwrap().address.clone();
    assert_eq!(es.fields[0].value, expected, "loads the selected property's fields");
    cleanup(&path);
}

#[test]
fn real_estate_tax_portal_and_comments_round_trip_in_tui() {
    // Pins the RealEstate build↔commit field-index mapping for the NEW fields:
    // edit a property, fill the tax portal + every per-portal comment by LABEL
    // (so the test doesn't hard-code indices), save, and confirm each lands in the
    // right struct field. A build/commit index mismatch would cross the values.
    let (mut app, path) = app_unlocked("uitaxportal");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut re = RealEstate::new().unwrap();
        re.address = "9 Audit Ave".into();
        records::upsert(&mut v.real_estate, re);
    }
    app.tab = Tab::RealEstate;
    app.selected = 0;
    app.start_edit(true);
    // Set fields by label so the assertion below is index-agnostic.
    let set = |app: &mut App, label: &str, val: &str| {
        let es = app.edit.as_mut().unwrap();
        let f = es.fields.iter_mut().find(|f| f.label == label).unwrap_or_else(|| panic!("no field {label:?}"));
        f.value = val.into();
    };
    set(&mut app, "Tax URL", "https://tax.example");
    set(&mut app, "Tax username", "taxuser");
    set(&mut app, "Tax password", "taxpw");
    set(&mut app, "Tax comment", "tax notes");
    set(&mut app, "Property Mgmt comment", "pm notes");
    set(&mut app, "Insurance comment", "ins notes");
    set(&mut app, "HOA comment", "hoa notes");
    app.handle_key(ctrl('s'));
    let re = &app.vault.as_ref().unwrap().vault.real_estate[0];
    assert_eq!(re.tax_portal_url, "https://tax.example");
    assert_eq!(re.tax_portal_username, "taxuser");
    assert_eq!(re.tax_portal_password, "taxpw");
    assert_eq!(re.tax_portal_comment, "tax notes");
    assert_eq!(re.property_mgmt_comment, "pm notes");
    assert_eq!(re.insurance_comment, "ins notes");
    assert_eq!(re.hoa_comment, "hoa notes");
    // The address (field 0) must be untouched — i.e. no field shifted onto another.
    assert_eq!(re.address, "9 Audit Ave");
    cleanup(&path);
}

#[test]
fn tax_attach_requires_filename_and_source() {
    let (mut app, path) = app_unlocked("uitaxreq");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.tax_filings, TaxFiling::new().unwrap());
    }
    app.tab = Tab::Taxes;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;
    // Filename present, source empty → rejected as missing input, nothing attached
    // (kills the `||`→`&&` mutant, which would let a one-sided-empty input through).
    app.edit.as_mut().unwrap().fields[rc].value = "w2.txt".into();
    app.edit.as_mut().unwrap().fields[rc + 1].value = String::new();
    app.attach_tax_document();
    assert!(app.edit.as_ref().unwrap().tax_docs.is_empty(), "missing source → no upload");
    assert!(app.status.contains("required"), "rejected as a missing-input error");
    cleanup(&path);
}

#[test]
fn re_attach_requires_source_and_defaults_filename_in_tui() {
    let (mut app, path) = app_unlocked("uirereq");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut re = RealEstate::new().unwrap();
        re.address = "1 Main".into();
        records::upsert(&mut v.real_estate, re);
    }
    app.tab = Tab::RealEstate;
    app.selected = 0;
    app.start_edit(true);
    let rc = app.edit.as_ref().unwrap().record_fields;
    // (1) A source is required even when a filename IS given.
    app.edit.as_mut().unwrap().fields[rc].value = "deed.pdf".into();
    app.edit.as_mut().unwrap().fields[rc + 1].value = String::new();
    app.attach_re_document();
    assert!(app.edit.as_ref().unwrap().re_docs.is_empty(), "missing source → no upload");
    assert!(app.status.contains("required"), "rejected as a missing-input error");
    // (2) Empty filename + a real source file → uploads using the source's basename.
    let src = path.parent().unwrap().join("Deed.PDF");
    std::fs::write(&src, b"x").unwrap();
    app.edit.as_mut().unwrap().fields[rc].value = String::new();
    app.edit.as_mut().unwrap().fields[rc + 1].value = src.to_string_lossy().into();
    app.attach_re_document();
    assert_eq!(app.edit.as_ref().unwrap().re_docs.len(), 1, "uploaded with defaulted filename (status: {})", app.status);
    let id = app.edit.as_ref().unwrap().re_docs[0].clone();
    let vpath = app.vault.as_ref().unwrap().doc_path(&id).unwrap();
    assert!(vpath.ends_with("_Deed.PDF"), "empty filename used the source basename: {vpath}");
    cleanup(&path);
}

#[test]
fn formats_timestamps() {
    assert_eq!(format_time(0), "never");
    assert_eq!(format_time(-5), "never");
    assert_eq!(format_time(1_609_459_200), "2021-01-01 00:00:00 UTC");
    assert_eq!(format_time(1_609_459_201), "2021-01-01 00:00:01 UTC");
}

#[test]
fn tabs_cycle_and_number_select() {
    let (mut app, path) = app_unlocked("tabs");
    // URGENT is the first tab and the default landing tab.
    assert_eq!(app.tab, Tab::Urgent);
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.tab, Tab::Instructions);
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.tab, Tab::Urgent);
    // Digit keys are 1-based over Tab::ALL, so '5' is now Accounts (URGENT shifted them).
    app.handle_key(key(KeyCode::Char('5')));
    assert_eq!(app.tab, Tab::Accounts);
    cleanup(&path);
}

#[test]
fn urgent_tab_is_first_and_creates_a_free_text_note() {
    let (mut app, path) = app_unlocked("urgent");
    // URGENT is the default landing tab (tab 1).
    assert_eq!(app.tab, Tab::Urgent);
    app.handle_key(key(KeyCode::Char('n'))); // new -> Edit screen
    assert_eq!(app.screen, Screen::Edit);
    // Field order: 0 title, 1 details (both free text).
    for c in "Call the lawyer".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Down)); // -> details
    for c in "Safe key in desk".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(ctrl('s')); // save
    assert_eq!(app.screen, Screen::Browse);
    let v = &app.vault.as_ref().unwrap().vault;
    assert_eq!(v.urgent.len(), 1);
    assert_eq!(v.urgent[0].title, "Call the lawyer");
    assert_eq!(v.urgent[0].description, "Safe key in desk");
    // It shows in the URGENT list and NOT in Instructions (separate collections).
    assert_eq!(app.current_labels().len(), 1);
    assert!(v.instructions.is_empty());
    cleanup(&path);
}

#[test]
fn create_account_via_keys_persists_fields_in_order() {
    let (mut app, path) = app_unlocked("acct");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts tab
    app.handle_key(key(KeyCode::Char('n'))); // new -> Edit screen
    assert_eq!(app.screen, Screen::Edit);

    // Field order: 0 title 1 type(choice) 2 subtype(choice) 3 owner 4 username
    // 5 password 6 url 7 closed_as_of 8 description 9 review(choice).
    let typ = |app: &mut App, c: char| app.handle_key(key(KeyCode::Char(c)));
    // title (focus 0) — mandatory.
    for c in "My login".chars() { typ(&mut app, c); }
    // owner (focus 3)
    app.handle_key(key(KeyCode::Down)); // 0->1
    app.handle_key(key(KeyCode::Down)); // 1->2
    app.handle_key(key(KeyCode::Down)); // 2->3 owner
    for c in "Jane".chars() { typ(&mut app, c); }
    app.handle_key(key(KeyCode::Down)); // username
    for c in "jane".chars() { typ(&mut app, c); }
    app.handle_key(key(KeyCode::Down)); // password
    for c in "pw".chars() { typ(&mut app, c); }
    app.handle_key(key(KeyCode::Down)); // url
    app.handle_key(key(KeyCode::Down)); // closed_as_of (focus 7)
    for c in "2026-06-18".chars() { typ(&mut app, c); }
    app.handle_key(ctrl('s')); // save

    assert_eq!(app.screen, Screen::Browse);
    let v = &app.vault.as_ref().unwrap().vault;
    assert_eq!(v.accounts.len(), 1);
    // Verify field-index mapping: owner/username/password/closed_as_of landed correctly.
    assert_eq!(v.accounts[0].title, "My login");
    assert_eq!(v.accounts[0].owner, "Jane");
    assert_eq!(v.accounts[0].username, "jane");
    assert_eq!(v.accounts[0].password, "pw");
    assert_eq!(v.accounts[0].closed_as_of, "2026-06-18");
    cleanup(&path);
}

#[test]
fn saving_an_account_trims_every_field_in_tui() {
    let (mut app, path) = app_unlocked("trimsave");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('n'))); // new
    // title (focus 0) — mandatory; also exercises trimming.
    for c in "  Brokerage  ".chars() { app.handle_key(key(KeyCode::Char(c))); }
    // owner (focus 3) with surrounding spaces
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    for c in "  Jane  ".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(key(KeyCode::Down)); // username
    for c in " jane ".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(key(KeyCode::Down)); // password
    for c in "  pw  ".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(ctrl('s'));
    let a = &app.vault.as_ref().unwrap().vault.accounts[0];
    assert_eq!(a.title, "Brokerage");
    assert_eq!(a.owner, "Jane");
    assert_eq!(a.username, "jane");
    assert_eq!(a.password, "pw", "the password is trimmed too (configured policy)");
    cleanup(&path);
}

#[test]
fn trim_all_key_bulk_trims_every_tab_in_tui() {
    let (mut app, path) = app_unlocked("trimallkey");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.owner = "  Alice  ".into();
        a.title = " Brokerage ".into();
        a.password = "  s3cret  ".into();
        records::upsert(&mut v.accounts, a);
        // A dirty record on a DIFFERENT tab must be trimmed by the same key.
        let mut re = RealEstate::new().unwrap();
        re.address = "  1 Main St  ".into();
        re.hoa_password = "  hoapw  ".into();
        records::upsert(&mut v.real_estate, re);
    }
    // Press T from a tab OTHER than the records being trimmed — it is whole-vault.
    app.handle_key(key(KeyCode::Char('7'))); // Taxes tab
    app.handle_key(key(KeyCode::Char('T'))); // one-off trim-all (whole vault)
    let a = &app.vault.as_ref().unwrap().vault.accounts[0];
    assert_eq!(a.owner, "Alice");
    assert_eq!(a.title, "Brokerage");
    assert_eq!(a.password, "s3cret");
    let re = &app.vault.as_ref().unwrap().vault.real_estate[0];
    assert_eq!(re.address, "1 Main St");
    assert_eq!(re.hoa_password, "hoapw", "portal passwords are trimmed too");
    assert!(app.status.contains("Trimmed 2"), "status reports the count: {}", app.status);
    // Idempotent: a second pass finds nothing to trim.
    app.handle_key(key(KeyCode::Char('T')));
    assert!(app.status.contains("Nothing to trim"), "second pass is a no-op: {}", app.status);
    cleanup(&path);
}

#[test]
fn trim_all_key_is_blocked_in_read_only_tui() {
    let (mut app, path) = app_read_only("trimro");
    app.handle_key(key(KeyCode::Char('5')));
    app.handle_key(key(KeyCode::Char('T')));
    assert!(app.status.contains("Read-only"), "read-only blocks the bulk trim: {}", app.status);
    cleanup(&path);
}

#[test]
fn tui_config_delete_type_unused_blocked_when_used_or_has_subtypes() {
    let (mut app, path) = app_unlocked("cfgdel");
    {
        let v = app.vault.as_mut().unwrap();
        v.add_asset_type("Crypto").unwrap();
        v.add_account_type("Bank").unwrap();
        v.add_account_subtype("Bank", "Checking").unwrap();
        v.save().unwrap();
    }
    app.screen = Screen::Config;

    // Delete an UNUSED asset type (focus 0, type the name, Del).
    app.cfg_focus = 0;
    app.cfg_asset_type = "Crypto".into();
    app.handle_config_key(key(KeyCode::Delete));
    assert!(app.status.contains("Deleted asset"), "status: {}", app.status);
    assert!(!app.vault.as_ref().unwrap().categories().asset.contains(&"Crypto".to_string()));
    assert!(app.cfg_asset_type.is_empty(), "field cleared on success");

    // Deleting an account type WITH subtypes is blocked.
    app.cfg_focus = 1;
    app.cfg_account_type = "Bank".into();
    app.handle_config_key(key(KeyCode::Delete));
    assert!(app.status.contains("delete its subtypes first"), "status: {}", app.status);
    assert!(app.vault.as_ref().unwrap().categories().account_type_names().contains(&"Bank".to_string()));

    // A subtype IN USE by a live account is blocked.
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.account_type = "Bank".into();
        a.account_subtype = "Checking".into();
        records::upsert(&mut v.accounts, a);
    }
    app.vault.as_mut().unwrap().save().unwrap();
    app.cfg_focus = 2;
    app.cfg_subtype_type = "Bank".into();
    app.cfg_subtype_name = "Checking".into();
    app.handle_config_key(key(KeyCode::Delete));
    assert!(app.status.contains("still used by 1"), "status: {}", app.status);
    assert_eq!(app.vault.as_ref().unwrap().categories().subtypes_for("Bank"), vec!["Checking".to_string()]);
    cleanup(&path);
}

#[test]
fn tui_config_delete_is_blocked_read_only() {
    let (mut app, path) = app_read_only("cfgdelro");
    app.screen = Screen::Config;
    app.cfg_focus = 1;
    app.cfg_account_type = "Email".into();
    app.handle_config_key(key(KeyCode::Delete));
    assert!(app.status.contains("Read-only"), "read-only blocks delete: {}", app.status);
    cleanup(&path);
}

#[test]
fn review_choice_maps_to_bool_on_save() {
    let (mut app, path) = app_unlocked("review");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('n')));
    // title (focus 0) — mandatory.
    for c in "Acct".chars() { app.handle_key(key(KeyCode::Char(c))); }
    // owner (field 3) is also mandatory — set it directly.
    app.edit.as_mut().unwrap().fields[3].value = "Alice".into();
    // focus 9 = review choice (0 title .. 7 closed_as_of, 8 description); cycle to "Yes".
    for _ in 0..9 {
        app.handle_key(key(KeyCode::Down));
    }
    app.handle_key(key(KeyCode::Right)); // cycle choice No -> Yes
    app.handle_key(ctrl('s'));
    assert!(app.vault.as_ref().unwrap().vault.accounts[0].review);
    cleanup(&path);
}

#[test]
fn account_save_requires_a_title_in_tui() {
    let (mut app, path) = app_unlocked("titlereq");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('n'))); // new
    // Fill the username but leave the title (field 0) blank.
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Down));
    }
    for c in "notitle".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(ctrl('s')); // save — must be rejected
    assert_eq!(app.screen, Screen::Edit, "stays in the edit form when the title is blank");
    assert!(app.status.contains("Title is required"), "status: {}", app.status);
    assert!(app.vault.as_ref().unwrap().vault.accounts.is_empty(), "nothing saved without a title");
    cleanup(&path);
}

#[test]
fn account_save_requires_an_owner_in_tui() {
    let (mut app, path) = app_unlocked("ownerreq");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('n'))); // new
    // Give a title (field 0) but leave the owner (field 3) blank.
    for c in "Acct".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(ctrl('s')); // save — must be rejected for the missing owner
    assert_eq!(app.screen, Screen::Edit, "stays in the edit form when the owner is blank");
    assert!(app.status.contains("Owner is required"), "status: {}", app.status);
    assert!(app.vault.as_ref().unwrap().vault.accounts.is_empty(), "nothing saved without an owner");
    // Supplying an owner lets it save.
    app.edit.as_mut().unwrap().fields[3].value = "Alice".into();
    app.handle_key(ctrl('s'));
    assert_eq!(app.screen, Screen::Browse, "saves once title + owner are present");
    assert_eq!(app.vault.as_ref().unwrap().vault.accounts.len(), 1);
    cleanup(&path);
}

#[test]
fn read_only_edit_form_is_not_editable() {
    let (mut app, path) = app_read_only("roedit");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.selected = 0;
    app.handle_key(key(KeyCode::Enter)); // Enter opens the record as a VIEW
    assert_eq!(app.screen, Screen::Edit);

    // Typing + backspace into the focused (title) text field is inert.
    for c in "HACK".chars() { app.handle_key(key(KeyCode::Char(c))); }
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.edit.as_ref().unwrap().fields[0].value, "", "text field not editable in read-only");

    // Cycling a Choice field (account type, field 1) is inert too.
    app.handle_key(key(KeyCode::Down)); // focus -> 1 (account type choice)
    let before = app.edit.as_ref().unwrap().fields[1].value.clone();
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.edit.as_ref().unwrap().fields[1].value, before, "choice not cyclable in read-only");

    // Reads still work: Ctrl+R flips the global reveal (the only reveal control) even
    // in read-only — on the Accounts tab that is `reveal_all`.
    assert!(!app.reveal_all);
    app.handle_key(ctrl('r'));
    assert!(app.reveal_all, "global reveal still works in read-only");
    cleanup(&path);
}

#[test]
fn tui_accounts_grouped_tree_expands_then_edits_leaf() {
    let (mut app, path) = app_unlocked("uitree");
    let want_id = {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.account_type = "Financial".into();
        a.account_subtype = "Bank".into();
        a.owner = "Alice".into();
        a.title = "Joint brokerage".into();
        let id = a.id.clone();
        records::upsert(&mut v.accounts, a);
        id
    };
    app.handle_key(key(KeyCode::Char('5'))); // Accounts tab
    app.handle_key(key(KeyCode::Char('g'))); // switch to grouped
    assert!(app.acct_grouped);
    // Collapsed by default: only the top-level owner row is visible, and the leaf
    // title is NOT shown yet.
    assert_eq!(app.account_rows().len(), 1);
    assert!(!render_to_string(&app).contains("Joint brokerage"), "leaf hidden while collapsed");
    // Expand owner → type → subtype, stepping down to each newly revealed child.
    app.handle_key(key(KeyCode::Enter)); // expand Alice (selected 0)
    app.handle_key(key(KeyCode::Down)); // -> Financial
    app.handle_key(key(KeyCode::Enter)); // expand Financial
    app.handle_key(key(KeyCode::Down)); // -> Bank
    app.handle_key(key(KeyCode::Enter)); // expand Bank
    app.handle_key(key(KeyCode::Down)); // -> the leaf
    assert!(render_to_string(&app).contains("Joint brokerage"), "leaf title shown when expanded");
    // Enter on the leaf edits that account.
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.screen, Screen::Edit);
    assert_eq!(app.edit.as_ref().unwrap().id.as_deref(), Some(want_id.as_str()));
    cleanup(&path);
}

#[test]
fn grouped_and_flat_views_show_the_same_record_set() {
    // Differential property: for any records + filters, the grouped tree (fully expanded)
    // and the flat list must contain EXACTLY the same record ids — the two code paths
    // must never disagree about which records are shown.
    use std::collections::HashSet;
    let (mut app, path) = app_unlocked("uidiff");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let owners = ["", "Alice", "Bob"];
        let types = ["", "Bank", "Crypto"];
        let subs = ["", "Checking"];
        let kinds = ["Asset", "Liability"];
        for i in 0..24usize {
            let mut a = Account::new().unwrap();
            a.owner = owners[i % owners.len()].into();
            a.account_type = types[(i / 3) % types.len()].into();
            a.account_subtype = subs[i % subs.len()].into();
            a.title = format!("t{i}");
            a.review = i % 4 == 0;
            records::upsert(&mut v.accounts, a);
            let mut as_ = AssetLiability::new().unwrap();
            as_.owner = owners[i % owners.len()].into();
            as_.kind = kinds[i % kinds.len()].into();
            as_.asset_type = types[i % types.len()].into();
            as_.review = i % 3 == 0;
            records::upsert(&mut v.assets, as_);
        }
    }

    // Helper: fully expand the current tab's tree, then return the leaf-id set.
    fn expanded_leaf_ids(app: &mut App) -> HashSet<String> {
        loop {
            let collapsed: Vec<Vec<String>> = app
                .grouped_rows()
                .iter()
                .filter_map(|r| match &r.kind {
                    AcctRowKind::Group { path, expanded: false } => Some(path.clone()),
                    _ => None,
                })
                .collect();
            if collapsed.is_empty() {
                break;
            }
            for p in collapsed {
                app.toggle_group_expanded(p);
            }
        }
        app.grouped_rows()
            .iter()
            .filter_map(|r| match &r.kind {
                AcctRowKind::Leaf { id } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    // Accounts: across several filter combinations, grouped == flat.
    app.tab = Tab::Accounts;
    app.acct_grouped = true;
    for (ft, rev) in [(None, false), (Some("Bank".to_string()), false), (None, true)] {
        app.acct_filter_type = ft.clone();
        app.acct_filter_review = rev;
        app.acct_expanded.clear();
        let grouped = expanded_leaf_ids(&mut app);
        let flat: HashSet<String> = app.current_labels().into_iter().map(|(id, _)| id).collect();
        assert_eq!(grouped, flat, "accounts grouped vs flat diverged (type={ft:?}, review={rev})");
    }

    // Assets: with and without the review filter, grouped == flat.
    app.tab = Tab::Assets;
    app.asset_grouped = true;
    for rev in [false, true] {
        app.asset_filter_review = rev;
        app.asset_expanded.clear();
        let grouped = expanded_leaf_ids(&mut app);
        let flat: HashSet<String> = app.current_labels().into_iter().map(|(id, _)| id).collect();
        assert_eq!(grouped, flat, "assets grouped vs flat diverged (review={rev})");
    }
    cleanup(&path);
}

#[test]
fn tui_assets_grouped_tree_expands_then_edits_leaf() {
    let (mut app, path) = app_unlocked("uiassettree");
    let want_id = {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = AssetLiability::new().unwrap();
        a.owner = "Alice".into();
        a.kind = "Liability".into();
        a.asset_type = "Mortgage".into();
        a.title = "Beach house".into();
        let id = a.id.clone();
        records::upsert(&mut v.assets, a);
        id
    };
    app.handle_key(key(KeyCode::Char('4'))); // Assets tab
    app.handle_key(key(KeyCode::Char('g'))); // grouped
    assert!(app.asset_grouped);
    // Collapsed: only the top-level owner row; the leaf title is hidden.
    assert_eq!(app.asset_rows().len(), 1);
    assert!(!render_to_string(&app).contains("Beach house"), "leaf hidden while collapsed");
    // Expand owner → kind → type, stepping into each newly revealed child.
    app.handle_key(key(KeyCode::Enter)); // expand Alice
    app.handle_key(key(KeyCode::Down)); // -> Liability
    app.handle_key(key(KeyCode::Enter)); // expand Liability
    app.handle_key(key(KeyCode::Down)); // -> Mortgage
    app.handle_key(key(KeyCode::Enter)); // expand Mortgage
    app.handle_key(key(KeyCode::Down)); // -> the leaf
    assert!(render_to_string(&app).contains("Beach house"), "leaf shown when expanded");
    // Enter on the leaf edits that asset.
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.screen, Screen::Edit);
    assert_eq!(app.edit.as_ref().unwrap().id.as_deref(), Some(want_id.as_str()));
    cleanup(&path);
}

#[test]
fn grouped_tree_expand_keys_do_not_collide_in_tui() {
    // Regression for the expand-key collision (audit fix): keying `acct_expanded`
    // on a separator-JOINED string let two distinct group paths share state — a
    // nested ["Alice","Bank"] collided with a single top-level owner label
    // "Alice\x1fBank". Keying on the label STACK (Vec) makes them distinct.
    let (mut app, path) = app_unlocked("treekeycollide");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a = Account::new().unwrap();
        a.owner = "Alice".into();
        a.account_type = "Bank".into();
        a.title = "Nested".into();
        records::upsert(&mut v.accounts, a);
        let mut b = Account::new().unwrap();
        b.owner = "Alice\u{1f}Bank".into(); // ONE owner label containing the old separator
        b.title = "TopLevel".into();
        records::upsert(&mut v.accounts, b);
    }
    app.tab = Tab::Accounts;
    app.acct_grouped = true;
    // Expand owner "Alice" and then the NESTED "Bank" group beneath it.
    app.acct_expanded.insert(vec!["Alice".to_string()]);
    app.acct_expanded.insert(vec!["Alice".to_string(), "Bank".to_string()]);
    let labels: Vec<String> = app.account_rows().iter().map(|r| r.label.clone()).collect();
    // The nested group's leaf is visible (its path IS expanded)...
    assert!(labels.iter().any(|l| l == "Nested"), "nested expanded group shows its leaf: {labels:?}");
    // ...but the separate top-level "Alice\x1fBank" group must stay COLLAPSED — if its
    // key collided with ["Alice","Bank"] it would wrongly expand and show "TopLevel".
    assert!(!labels.iter().any(|l| l == "TopLevel"), "colliding top-level group must stay collapsed: {labels:?}");
    cleanup(&path);
}

#[test]
fn edit_existing_resolves_by_id_under_filter() {
    let (mut app, path) = app_unlocked("filteredit");
    // Two accounts: one flagged for review, one not.
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        let mut a1 = Account::new().unwrap();
        a1.owner = "Plain".into();
        let mut a2 = Account::new().unwrap();
        a2.owner = "Flagged".into();
        a2.review = true;
        records::upsert(&mut v.accounts, a1);
        records::upsert(&mut v.accounts, a2);
    }
    app.handle_key(key(KeyCode::Char('5'))); // Accounts
    app.handle_key(key(KeyCode::Char('v'))); // review-only filter
    assert!(app.acct_filter_review);
    assert_eq!(app.current_labels().len(), 1); // only the flagged one
    app.selected = 0;
    app.handle_key(key(KeyCode::Enter)); // edit selected (filtered index 0)
    assert_eq!(app.screen, Screen::Edit);
    // The edit buffer must be the *flagged* account, not accounts[0].
    let es = app.edit.as_ref().unwrap();
    assert_eq!(es.id.as_deref(), Some(app.vault.as_ref().unwrap().vault.accounts.iter().find(|a| a.review).unwrap().id.as_str()));
    cleanup(&path);
}

#[test]
fn delete_via_key_removes_record() {
    let (mut app, path) = app_unlocked("del");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.instructions, Instruction::new().unwrap());
    }
    app.tab = Tab::Instructions; // the default landing tab is now URGENT
    assert_eq!(app.current_labels().len(), 1);
    app.selected = 0;
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.vault.as_ref().unwrap().vault.instructions.is_empty());
    cleanup(&path);
}

#[test]
fn config_screen_adds_type_and_subtype() {
    let (mut app, path) = app_unlocked("cfg");
    app.handle_key(key(KeyCode::Char('c'))); // open Config
    assert_eq!(app.screen, Screen::Config);
    // focus 0 = new asset type
    for c in "Annuity".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert!(app.vault.as_ref().unwrap().categories().asset.contains(&"Annuity".to_string()));
    // focus 2/3 = subtype type + name
    app.handle_key(key(KeyCode::Down)); // 0->1
    app.handle_key(key(KeyCode::Down)); // 1->2 subtype type
    for c in "Financial".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Down)); // 2->3 subtype name
    for c in "HSA".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert!(
        app.vault
            .as_ref()
            .unwrap()
            .categories()
            .subtypes_for("Financial")
            .contains(&"HSA".to_string())
    );
    cleanup(&path);
}

#[test]
fn tui_subtype_reconstrains_on_type_change() {
    let (mut app, path) = app_unlocked("subrecon");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts tab
    app.handle_key(key(KeyCode::Char('n'))); // new record -> Edit
    assert_eq!(app.screen, Screen::Edit);
    {
        let es = app.edit.as_ref().unwrap();
        // Field 0 is now Title; type/subtype follow.
        assert_eq!(es.fields[0].label, "Title");
        assert_eq!(es.fields[1].label, "Account type");
        assert_eq!(es.fields[2].label, "Subtype");
    }
    // Move focus to the account-type field, then cycle it; this must reconstrain
    // the dependent subtype field's options to the newly-selected type.
    app.handle_key(key(KeyCode::Down)); // Title -> Account type
    app.handle_key(key(KeyCode::Right));
    let es = app.edit.as_ref().unwrap();
    let chosen_type = es.fields[1].value.clone();
    let expected = app.vault.as_ref().unwrap().categories().subtypes_for(&chosen_type);
    match &es.fields[2].kind {
        FieldKind::Choice(opts) => assert_eq!(opts, &expected),
        _ => panic!("subtype field is not a Choice"),
    }
    cleanup(&path);
}

#[test]
fn detach_on_non_doc_tab_is_noop() {
    let (mut app, path) = app_unlocked("detachnoop");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts (no docs)
    app.handle_key(key(KeyCode::Char('n')));
    app.handle_key(ctrl('k')); // detach — should be a harmless no-op
    // Still on the edit screen with an intact buffer; no account created yet.
    assert_eq!(app.screen, Screen::Edit);
    assert!(app.vault.as_ref().unwrap().vault.accounts.is_empty());
    cleanup(&path);
}

#[test]
fn read_only_keys_are_inert_but_reads_work() {
    let (mut app, path) = app_read_only("ro");
    app.handle_key(key(KeyCode::Char('5'))); // Accounts tab
    assert_eq!(app.current_labels().len(), 1, "existing record is viewable");

    // New / delete / change-password do nothing and report read-only.
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.screen, Screen::Browse, "new is inert in read-only");
    assert!(app.status.contains("Read-only"));
    app.selected = 0;
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.vault.as_ref().unwrap().vault.accounts.len(), 1, "delete is inert");

    // Viewing (Enter -> Edit) is allowed; saving is not.
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.screen, Screen::Edit);
    app.handle_key(ctrl('s'));
    assert!(app.status.contains("Read-only"), "save is inert in read-only");
    cleanup(&path);
}

#[test]
fn read_only_config_blocks_type_add() {
    let (mut app, path) = app_read_only("rocfg");
    app.handle_key(key(KeyCode::Char('c')));
    assert_eq!(app.screen, Screen::Config);
    for c in "Annuity".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter)); // focus 0 = add asset type
    assert!(
        !app.vault.as_ref().unwrap().categories().asset.contains(&"Annuity".to_string()),
        "type add blocked in read-only"
    );
    assert!(app.status.contains("Read-only"));
    cleanup(&path);
}

#[test]
fn over_long_filename_is_capped_in_tui() {
    // The uniform layout caps every path component (filename 120, group/subfolder
    // 40, timestamp fixed), so a huge filename can no longer push the virtual path
    // past MAX_PATH_LEN — it is sanitized and truncated, and the upload succeeds.
    let (mut app, path) = app_unlocked("uipath");
    app.tab = Tab::Assets;
    app.start_edit(false); // builds the edit form, appending the doc fields
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let src = std::env::temp_dir().join(format!("vaultis-uipath-{nanos}.txt"));
    std::fs::write(&src, b"x").unwrap();
    {
        let es = app.edit.as_mut().unwrap();
        let rc = es.record_fields;
        es.fields[2].value = "Jane Doe".into(); // owner (now required before attach)
        es.fields[5].value = "1000".into(); // approx value (numeric, now required)
        es.fields[rc].value = "f".repeat(crate::storage::MAX_PATH_LEN); // filename (huge)
        es.fields[rc + 1].value = src.display().to_string(); // upload from
    }
    app.attach_document();
    let id = app.edit.as_ref().unwrap().attached_file_id.clone();
    assert!(id.is_some(), "upload should succeed with a capped name; status: {}", app.status);
    let vpath = app.vault.as_ref().unwrap().doc_path(&id.unwrap()).unwrap_or_default();
    assert!(vpath.len() <= crate::storage::MAX_PATH_LEN, "vpath within limit: {} bytes", vpath.len());
    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn volume_size_config_sets_cap_in_tui() {
    let (mut app, path) = app_unlocked("uivol");
    app.cfg_focus = 4; // the volume-size field
    app.cfg_volume_size = "8".into();
    app.submit_config();
    assert_eq!(app.vault.as_ref().unwrap().volume_max_size(), 8 * 1024 * 1024);
    assert!(app.cfg_volume_size.is_empty(), "input cleared on success");
    // A non-numeric value is rejected and leaves the cap unchanged.
    app.cfg_volume_size = "abc".into();
    app.submit_config();
    assert_eq!(app.vault.as_ref().unwrap().volume_max_size(), 8 * 1024 * 1024);
    assert!(app.status.contains("MiB"), "status was: {}", app.status);
    cleanup(&path);
}

#[test]
fn read_only_config_typing_gated_to_export_and_backup_fields() {
    // Regression (deep-hunt): in read-only, typing must only edit the fields whose
    // action is reachable read-only — backup dest (5) and export dir (7). The
    // write-only fields (volume size, etc.) must stay inert, matching submit_config's
    // allow-list and the GUI which hides them.
    let (mut app, path) = app_read_only("rocfgtype");
    app.handle_key(key(KeyCode::Char('c'))); // Config
    assert_eq!(app.screen, Screen::Config);

    // Write-only field (volume size, focus 4): typing is ignored.
    app.cfg_focus = 4;
    for c in "8".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert!(app.cfg_volume_size.is_empty(), "read-only can't type into the volume-size field");

    // Export dir (focus 7): typing IS allowed (a local, non-vault preference).
    app.cfg_focus = 7;
    for c in "/tmp/exp".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.cfg_export_dir, "/tmp/exp", "read-only can edit the export directory");
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.cfg_export_dir, "/tmp/ex", "backspace works on the export-dir field");
    cleanup(&path);
}

#[test]
fn read_only_config_blocks_volume_size() {
    let (mut app, path) = app_read_only("rovol");
    app.handle_key(key(KeyCode::Char('c'))); // Config
    app.cfg_focus = 4;
    app.cfg_volume_size = "8".into();
    app.submit_config();
    assert_eq!(
        app.vault.as_ref().unwrap().volume_max_size(),
        crate::storage::DEFAULT_VOLUME_MAX_SIZE,
        "volume size change blocked in read-only"
    );
    assert!(app.status.contains("Read-only"));
    cleanup(&path);
}

/// Render the current screen to an in-memory backend; asserts no panic.
fn render(app: &App) {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
}

/// As [`render`], but hands back what the screen actually SAYS: every cell's symbol,
/// row by row. For asserting on wording rather than on internal state — the screen is
/// what the user is confronted with, and it is the thing that can be wrong on its own.
fn render_text(app: &App) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let buf = term.backend().buffer();
    let width = buf.area.width;
    // Joined per row, so a phrase that wrapped is not silently reassembled across lines
    // into a match the user never actually saw.
    (0..buf.area.height)
        .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_every_screen_without_panicking() {
    // Auth screen, all three modes — in BOTH session kinds, since `writable` now changes
    // which fields the screen has (read-only Create drops the two confirmations).
    let path = tmp_vault("render");
    for writable in [true, false] {
        for mode in [AuthMode::Create, AuthMode::Unlock, AuthMode::ChangePassword] {
            let mut app = App::new(path.clone(), writable);
            app.auth = AuthState::new(mode, writable);
            app.auth.error = Some("err".into());
            render(&app);
        }
    }

    let (mut app, _p) = app_unlocked("render2");
    // Populate one record per tab so list/label rendering is exercised.
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        records::upsert(&mut v.instructions, Instruction::new().unwrap());
        records::upsert(&mut v.trust_wills, TrustWill::new().unwrap());
        records::upsert(&mut v.assets, AssetLiability::new().unwrap());
        let mut acc = Account::new().unwrap();
        acc.account_type = "Financial".into();
        acc.review = true;
        records::upsert(&mut v.accounts, acc);
        records::upsert(&mut v.real_estate, RealEstate::new().unwrap());
    }
    // Browse each tab.
    for t in Tab::ALL {
        app.tab = t;
        app.selected = 0;
        render(&app);
    }
    // Browse with Accounts filters active (exercises the filter title).
    app.tab = Tab::Accounts;
    app.acct_filter_type = Some("Financial".into());
    app.acct_filter_review = true;
    render(&app);
    app.tab = Tab::Assets;
    app.asset_filter_review = true;
    render(&app);

    // Edit screen for a doc-bearing tab (history + attached-doc lines) and a
    // plain tab; plus the config screen.
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    render(&app);
    app.screen = Screen::Browse;
    app.tab = Tab::Accounts;
    app.start_edit(false);
    render(&app);
    app.screen = Screen::Config;
    render(&app);
    cleanup(&path);
}

#[test]
fn merge_ctrl_u_entry_preview_and_apply() {
    // SOURCE vault in its own dir: a newer shared account + a doc-bearing record.
    let s_path = tmp_vault("merge-src");
    let s_dir = s_path.parent().unwrap().to_path_buf();
    let blob_id;
    {
        let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
        let mut a = crate::records::Account::new().unwrap();
        a.id = "shared".into();
        a.title = "Shared".into();
        a.owner = "o".into();
        a.account_type = "Checking".into();
        a.username = "alice".into();
        a.password = "NEWPW".into();
        a.updated_at = 2_000;
        s.vault.accounts.push(a);
        let f = std::env::temp_dir().join(format!("pmtui-doc-{}.txt", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::write(&f, b"deed-bytes").unwrap();
        blob_id = s.add_document("general-documents/deed", "deed.pdf", &f).unwrap();
        let mut gd = crate::records::GeneralDocument::new().unwrap();
        gd.id = "gd-1".into();
        gd.title = "Deed".into();
        gd.file = Some(blob_id.clone());
        gd.updated_at = 3_000;
        s.vault.general_documents.push(gd);
        s.save().unwrap();
    }

    // CURRENT vault (open, writable) with the OLDER shared account; on Config.
    let (mut app, c_path) = app_unlocked("merge-cur");
    {
        let cur = app.vault.as_mut().unwrap();
        let mut a = crate::records::Account::new().unwrap();
        a.id = "shared".into();
        a.title = "Shared".into();
        a.owner = "o".into();
        a.account_type = "Checking".into();
        a.username = "alice".into();
        a.password = "OLDPW".into();
        a.updated_at = 1_000;
        cur.vault.accounts.push(a);
        cur.save().unwrap();
    }
    app.screen = Screen::Config;

    // Ctrl+U on Config enters the merge screen (writable).
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.screen, Screen::Merge);

    // Clear the pre-filled folder (enter_merge seeds it with the vault root), then type
    // the source folder into focus 0 via the real key handler.
    app.merge_src_dir.clear();
    for c in s_dir.display().to_string().chars() {
        app.handle_merge_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(app.merge_src_dir, s_dir.display().to_string());
    app.merge_pw1 = "s1".into();
    app.merge_pw2 = "s2".into();
    app.merge_preview();
    assert!(app.merge_error.is_none(), "preview error: {:?}", app.merge_error);
    let plan = app.merge_plan.as_ref().expect("plan built");
    assert_eq!(plan.updated_count(), 1);
    assert_eq!(plan.new_count(), 1);
    assert_eq!(plan.blobs_to_copy(), 1);
    assert!(app.merge_pw1.is_empty(), "passwords wiped after preview");

    // Enter applies from the preview phase.
    app.handle_merge_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.screen, Screen::Config);
    assert!(app.merge_plan.is_none());
    let cur = app.vault.as_ref().unwrap();
    assert_eq!(cur.vault.accounts.iter().find(|a| a.id == "shared").unwrap().password, "NEWPW");
    assert_eq!(&**cur.read_document(&blob_id).unwrap(), b"deed-bytes");

    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_ctrl_u_inert_read_only() {
    // Read-only session: Ctrl+U on Config must NOT enter the merge screen.
    let path = tmp_vault("merge-ro");
    let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    drop(ov);
    let mut app = App::new(path.clone(), false); // read-only
    app.vault = Some(OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap());
    app.screen = Screen::Config;
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.screen, Screen::Config, "read-only stays on Config");
    cleanup(&path);
}

#[test]
fn ctrl_t_syncs_category_types_from_records() {
    let (mut app, path) = app_unlocked("sync-types");
    {
        let cur = app.vault.as_mut().unwrap();
        let mut a = crate::records::Account::new().unwrap();
        a.account_type = "Brokerage".into();
        cur.vault.accounts.push(a);
        cur.save().unwrap();
        assert!(!cur.categories().account_type_names().iter().any(|t| t == "Brokerage"));
    }
    app.screen = Screen::Config;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(app.vault.as_ref().unwrap().categories().account_type_names().iter().any(|t| t == "Brokerage"));
    assert!(app.status.contains("Added 1 type"), "status: {}", app.status);
    cleanup(&path);
}

/// A full disk during the merge's final save poisons the handle; the front-end must
/// drop the vault and return to the unlock screen so reopening loads the clean on-disk
/// vault (the merge did not persist).
#[cfg(feature = "fault-injection")]
#[test]
fn merge_apply_save_failure_drops_handle_to_unlock() {
    let s_path = tmp_vault("merge-poison-src");
    let s_dir = s_path.parent().unwrap().to_path_buf();
    {
        let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
        let f = std::env::temp_dir().join(format!("pmtui-poison-{}.txt", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::write(&f, b"doc-bytes").unwrap();
        let blob = s.add_document("general-documents/x", "x.pdf", &f).unwrap();
        let mut gd = crate::records::GeneralDocument::new().unwrap();
        gd.id = "gd".into();
        gd.updated_at = 2000;
        gd.file = Some(blob);
        s.vault.general_documents.push(gd);
        s.save().unwrap();
    }
    let (mut app, c_path) = app_unlocked("merge-poison-cur");
    {
        let cur = app.vault.as_mut().unwrap();
        let mut gd = crate::records::GeneralDocument::new().unwrap();
        gd.id = "gd".into();
        gd.updated_at = 1000;
        gd.file = None;
        cur.vault.general_documents.push(gd);
        cur.save().unwrap();
    }
    app.screen = Screen::Config;
    app.merge_src_dir = s_dir.display().to_string();
    app.merge_pw1 = "s1".into();
    app.merge_pw2 = "s2".into();
    app.merge_preview();
    assert!(app.merge_plan.is_some(), "preview built");
    // The blob copy succeeds; the FINAL vault.pmv save hits a full disk → poison.
    crate::fault::fail_at("vault.write", 1);
    app.merge_apply();
    crate::fault::clear();
    assert!(app.vault.is_none(), "poisoned handle dropped");
    assert_eq!(app.screen, Screen::Auth, "returned to the unlock screen");
    assert!(app.auth.error.as_deref().unwrap_or("").contains("interrupted"), "recovery message shown");
    // The on-disk vault is the clean old one: it reopens and the merge did NOT persist.
    let re = OpenVault::open(c_path.clone(), b"a", b"b").unwrap();
    assert_eq!(re.vault.general_documents.iter().find(|g| g.id == "gd").unwrap().updated_at, 1000, "merge not persisted");
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn presize_secret_keeps_headroom_and_content() {
    // Audit L3: a secret String must always carry >= 128 bytes of spare capacity so the
    // next per-keystroke push can't reallocate (a realloc frees the old buffer WITHOUT
    // zeroizing, stranding cleartext password fragments in freed heap).
    let mut s = String::from("hunter2");
    presize_secret(&mut s);
    assert!(s.capacity() >= s.len() + 128, "headroom reserved");
    assert_eq!(s, "hunter2", "content preserved by presize");
    // Simulate per-keystroke typing well past the initial headroom: the invariant must
    // hold before every push, and the content must stay exactly what was typed.
    let mut expected = String::from("hunter2");
    for i in 0..400 {
        presize_secret(&mut s);
        assert!(s.capacity() >= s.len() + 128, "headroom holds before push #{i}");
        let c = (b'a' + (i % 26) as u8) as char;
        s.push(c);
        expected.push(c);
    }
    assert_eq!(s, expected, "content intact across managed growth");
}

// --- Asset ↔ account links (Assets edit screen) --------------------------

/// Seed one account (returning its id) with a distinct title/owner so the
/// picker options and jump-target labels are recognisable in assertions.
fn seed_account(app: &mut App, title: &str, owner: &str) -> String {
    let v = &mut app.vault.as_mut().unwrap().vault;
    let mut a = Account::new().unwrap();
    a.title = title.into();
    a.owner = owner.into();
    let id = a.id.clone();
    records::upsert(&mut v.accounts, a);
    id
}

/// Seed one asset with the save-mandatory fields (owner + numeric value) and
/// the given pre-existing links, returning its id.
fn seed_asset(app: &mut App, title: &str, linked: Vec<String>) -> String {
    let v = &mut app.vault.as_mut().unwrap().vault;
    let mut a = AssetLiability::new().unwrap();
    a.title = title.into();
    a.owner = "Jane".into();
    a.approx_value = "100".into();
    a.linked_accounts = linked;
    let id = a.id.clone();
    records::upsert(&mut v.assets, a);
    id
}

/// The index of an edit field by label (panics if absent — a renamed label
/// would silently break the key handlers that look fields up the same way).
fn field_idx(app: &App, label: &str) -> usize {
    let es = app.edit.as_ref().unwrap();
    es.fields.iter().position(|f| f.label == label).unwrap_or_else(|| panic!("no field {label:?}"))
}

#[test]
fn ctrl_l_stages_link_and_ctrl_s_persists_it() {
    let (mut app, path) = app_unlocked("uilinkadd");
    let _a1 = seed_account(&mut app, "Bank A", "Jane");
    let a2 = seed_account(&mut app, "Bank B", "Jane");
    seed_asset(&mut app, "House", Vec::new());
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    // Cycle the picker to the SECOND account, then link it: the persisted id
    // must be a2 — proves selection resolves through the aligned candidate vec
    // (a first-match label lookup would also pass here, hence the raw-id and
    // duplicate-guard assertions below pin the rest of the contract).
    let pi = field_idx(&app, "Add link");
    app.edit.as_mut().unwrap().focus = pi;
    app.handle_key(key(KeyCode::Right));
    app.handle_key(ctrl('l'));
    assert_eq!(app.edit.as_ref().unwrap().linked, vec![a2.clone()], "staged in the edit state");
    assert!(app.status.contains("Ctrl+S to save"), "status: {}", app.status);
    // A duplicate add is refused with a hint — the list must not grow.
    app.handle_key(ctrl('l'));
    assert!(app.status.contains("Already linked"), "status: {}", app.status);
    assert_eq!(app.edit.as_ref().unwrap().linked.len(), 1);
    // Nothing reached the vault yet (edit-state only)…
    assert!(app.vault.as_ref().unwrap().vault.assets[0].linked_accounts.is_empty());
    // …until Ctrl+S commits the whole record, links included.
    app.handle_key(ctrl('s'));
    assert_eq!(app.screen, Screen::Browse, "saved; status: {}", app.status);
    assert_eq!(app.vault.as_ref().unwrap().vault.assets[0].linked_accounts, vec![a2]);
    cleanup(&path);
}

#[test]
fn ctrl_x_unlinks_by_number_and_blank_defaults_to_first() {
    let (mut app, path) = app_unlocked("uilinkdel");
    let a1 = seed_account(&mut app, "Bank A", "Jane");
    let a2 = seed_account(&mut app, "Bank B", "Jane");
    seed_asset(&mut app, "House", vec![a1.clone(), a2.clone()]);
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    // "2" removes the second link; the consumed # is cleared afterwards.
    let li = field_idx(&app, "Link # (open/unlink)");
    app.edit.as_mut().unwrap().fields[li].value = "2".into();
    app.handle_key(ctrl('x'));
    assert_eq!(app.edit.as_ref().unwrap().linked, vec![a1.clone()]);
    assert!(app.edit.as_ref().unwrap().fields[li].value.is_empty(), "consumed # cleared");
    // Blank defaults to #1 (the only remaining link).
    app.handle_key(ctrl('x'));
    assert!(app.edit.as_ref().unwrap().linked.is_empty());
    // Nothing left: a further unlink reports so instead of erroring.
    app.handle_key(ctrl('x'));
    assert!(app.status.contains("No linked accounts"), "status: {}", app.status);
    // The removal persists on save like any other field.
    app.handle_key(ctrl('s'));
    assert!(app.vault.as_ref().unwrap().vault.assets[0].linked_accounts.is_empty());
    cleanup(&path);
}

#[test]
fn ctrl_o_jumps_to_the_linked_account_relaxing_filters_and_grouping() {
    let (mut app, path) = app_unlocked("uilinkjump");
    let _a1 = seed_account(&mut app, "Alpha", "Jane");
    let a2 = seed_account(&mut app, "Beta", "Jane");
    seed_asset(&mut app, "House", vec![a2.clone()]);
    // Hostile view state: an owner filter that hides the target and grouped
    // mode active — the jump must still land on a visible, selected target.
    app.acct_filter_owner = Some("Nobody".into());
    app.acct_grouped = true;
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    app.handle_key(ctrl('o')); // blank Link # defaults to #1
    assert_eq!(app.screen, Screen::Browse);
    assert_eq!(app.tab, Tab::Accounts);
    assert!(app.edit.is_none(), "editor discarded (the Esc rule)");
    assert!(!app.acct_grouped, "grouped mode falls back to the flat list");
    let labels = app.current_labels();
    assert_eq!(labels.get(app.selected).map(|(id, _)| id.as_str()), Some(a2.as_str()), "target row selected");
    cleanup(&path);
}

#[test]
fn ctrl_o_on_dangling_link_stays_put_with_a_status_hint() {
    let (mut app, path) = app_unlocked("uilinkghost");
    seed_asset(&mut app, "House", vec!["ghost-123".into()]);
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    app.handle_key(ctrl('o'));
    assert_eq!(app.screen, Screen::Edit, "stays in the editor");
    assert!(app.edit.is_some(), "edit buffer kept");
    assert_eq!(app.tab, Tab::Assets);
    assert!(app.status.contains("not found"), "status: {}", app.status);
    cleanup(&path);
}

#[test]
fn linked_list_renders_raw_id_for_a_dangling_link() {
    let (mut app, path) = app_unlocked("uilinkraw");
    let a1 = seed_account(&mut app, "Bank A", "Jane");
    seed_asset(&mut app, "House", vec![a1.clone(), "ghost-123".into()]);
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    let screen = render_to_string(&app);
    assert!(screen.contains("Linked accounts (2)"), "numbered header drawn");
    // Assert the LIST-ROW format ("  #N  <label>"), not a bare substring: the
    // "Add link" picker on the same screen also renders "1. Bank A", which
    // would satisfy a plain contains("Bank A") even if list resolution broke
    // (mutation-verified: a raw-id-only list passed the old assertion).
    assert!(screen.contains("#1  Bank A"), "resolved link row shows the account label");
    // The dangling link is visible-but-unresolved (raw id), never hidden.
    assert!(screen.contains("#2  ghost-123"), "dangling link row shows the raw id");
    cleanup(&path);
}

#[test]
fn account_edit_screen_lists_the_assets_linking_it() {
    let (mut app, path) = app_unlocked("uilinkrev");
    let a1 = seed_account(&mut app, "Bank A", "Jane");
    seed_asset(&mut app, "House", vec![a1.clone()]);
    app.tab = Tab::Accounts;
    app.selected = 0;
    app.start_edit(true);
    let screen = render_to_string(&app);
    assert!(screen.contains("Linked from (1)"), "reverse view drawn");
    assert!(screen.contains("House"), "linking asset's label listed");
    // An UNLINKED account shows no header at all (nothing actionable there).
    app.handle_key(key(KeyCode::Esc));
    seed_account(&mut app, "Lonely", "Jane");
    app.selected = 1;
    app.start_edit(true);
    assert!(!render_to_string(&app).contains("Linked from"), "empty reverse view hidden");
    cleanup(&path);
}

#[test]
fn deleting_a_linked_account_warns_but_never_cascades() {
    let (mut app, path) = app_unlocked("uilinkwarn");
    let a1 = seed_account(&mut app, "Bank A", "Jane");
    seed_asset(&mut app, "House", vec![a1.clone()]);
    app.tab = Tab::Accounts;
    app.selected = 0;
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.vault.as_ref().unwrap().vault.accounts.is_empty(), "delete is allowed");
    assert!(
        app.status.contains("Linked from 1 asset/liability record(s)"),
        "still-linked warning surfaced: {}",
        app.status
    );
    // No cascade: the asset keeps its (now unresolved) link verbatim.
    assert_eq!(app.vault.as_ref().unwrap().vault.assets[0].linked_accounts, vec![a1]);
    cleanup(&path);
}

#[test]
fn ctrl_o_works_read_only_while_link_and_unlink_are_gated() {
    // Vault with a linked account+asset, reopened READ-ONLY: navigation (Ctrl+O)
    // must work, the mutating link keys (Ctrl+L / Ctrl+X) must be inert.
    let path = tmp_vault("uilinkro");
    let acct_id;
    {
        let mut ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        acct_id = a.id.clone();
        records::upsert(&mut ov.vault.accounts, a);
        let mut asset = AssetLiability::new().unwrap();
        asset.owner = "Jane".into();
        asset.approx_value = "100".into();
        asset.linked_accounts = vec![acct_id.clone()];
        records::upsert(&mut ov.vault.assets, asset);
        ov.save().unwrap();
    }
    let ov = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    let mut app = App::new(path.clone(), false);
    app.vault = Some(ov);
    app.screen = Screen::Browse;
    app.tab = Tab::Assets;
    app.selected = 0;
    app.start_edit(true);
    app.handle_key(ctrl('l'));
    assert!(app.status.contains("Read-only"), "link gated: {}", app.status);
    app.handle_key(ctrl('x'));
    assert!(app.status.contains("Read-only"), "unlink gated: {}", app.status);
    assert_eq!(app.edit.as_ref().unwrap().linked, vec![acct_id.clone()], "links untouched");
    app.handle_key(ctrl('o'));
    assert_eq!(app.tab, Tab::Accounts, "navigation allowed read-only");
    assert_eq!(app.screen, Screen::Browse);
    let labels = app.current_labels();
    assert_eq!(labels.get(app.selected).map(|(id, _)| id.as_str()), Some(acct_id.as_str()));
    cleanup(&path);
}
