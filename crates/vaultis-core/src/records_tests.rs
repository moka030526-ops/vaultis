//! Unit tests for the parent module ([`super`], `records.rs`), split into their own
//! file via `#[cfg(test)] #[path = "records_tests.rs"] mod tests;` so the tests do not sit
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
fn matches_search_is_case_insensitive_substring() {
    assert!(matches_search("alice@example.com", "ALICE"));
    assert!(matches_search("Bob", "b"));
    assert!(matches_search("a.user", "USER"));
    assert!(matches_search("anything", ""), "empty query matches all");
    assert!(matches_search("anything", "   "), "whitespace query matches all");
    assert!(matches_search("john", "  JOHN  "), "query is trimmed");
    assert!(!matches_search("alice", "bob"));
    assert!(!matches_search("", "x"));
}

#[test]
fn soundex_codes_words_and_rejects_letterless_ones() {
    // The textbook cases (Knuth): first letter + 3 consonant-class digits.
    assert_eq!(soundex("Robert").as_deref(), Some("R163"));
    assert_eq!(soundex("Rupert").as_deref(), Some("R163"), "sounds like Robert");
    assert_eq!(soundex("Ashcraft").as_deref(), Some("A261"), "h is transparent");
    assert_eq!(soundex("Ashcroft").as_deref(), Some("A261"));
    assert_eq!(soundex("Tymczak").as_deref(), Some("T522"), "a vowel breaks a repeat");
    assert_eq!(soundex("Pfister").as_deref(), Some("P236"));
    // Short words are zero-padded to 4; case and punctuation don't matter.
    assert_eq!(soundex("Lee").as_deref(), Some("L000"));
    assert_eq!(soundex("o'brien").as_deref(), soundex("OBrien").as_deref());
    // Nothing to code -> None (the caller then falls back to the substring rule).
    assert_eq!(soundex("2024"), None);
    assert_eq!(soundex("@#$"), None);
    assert_eq!(soundex(""), None);
}

#[test]
fn soundex_key_folds_the_initial_consonant_but_not_a_vowel() {
    // The key differs from the textbook code only in its first character: a consonant
    // becomes its class digit, so sound-alike initials meet...
    assert_eq!(soundex_key("Katherine"), soundex_key("Catherine"));
    assert_eq!(soundex_key("Chris"), soundex_key("Kris"));
    assert_eq!(soundex_key("Fisher"), soundex_key("Visher"));
    assert_eq!(soundex_key("Robert").as_deref(), Some("6163"), "R folds to its class 6");
    // ...while a vowel/h/w/y initial is kept verbatim (those really do sound different).
    assert_ne!(soundex_key("Allen"), soundex_key("Ellen"));
    assert_eq!(soundex_key("Ellen").as_deref(), Some("E450"));
    // Unrelated initials still differ (l is class 4, r is class 6).
    assert_ne!(soundex_key("Robert"), soundex_key("Lobert"));
    assert_eq!(soundex_key("2024"), None);
}

#[test]
fn matches_search_soundlike_is_a_superset_of_the_substring_search() {
    // Every substring hit still hits — the sound-alike arm only ADDS matches.
    for (hay, q) in [("alice@example.com", "ALICE"), ("Bob", "b"), ("a.user", "USER"), ("john", "  JOHN  ")] {
        assert!(matches_search(hay, q), "precondition: the substring rule matched");
        assert!(matches_search_soundlike(hay, q), "{q:?} must still match {hay:?}");
    }
    // Empty / whitespace-only query matches everything (no filter).
    assert!(matches_search_soundlike("anything", ""));
    assert!(matches_search_soundlike("anything", "   "));
}

#[test]
fn matches_search_soundlike_finds_names_spelled_as_they_sound() {
    // The point of the feature: a name the user cannot spell still finds the record.
    assert!(matches_search_soundlike("Johnson", "jonson"));
    assert!(matches_search_soundlike("Katherine", "catherine"));
    assert!(matches_search_soundlike("Smyth", "smith"));
    // Words inside an email/handle are coded individually, not as one run.
    assert!(matches_search_soundlike("alice.smyth@example.com", "smith"));
    // A substring hit needs no word boundary: letters may match ANYWHERE in the value.
    assert!(matches_search_soundlike("Fidelity Brokerage", "elit"));
    // A multi-word query narrows: EVERY word must be satisfied.
    assert!(matches_search_soundlike("Katherine Smyth", "catherine smith"));
    assert!(!matches_search_soundlike("Katherine Jones", "catherine smith"));
    // Genuinely unrelated text still doesn't match.
    assert!(!matches_search_soundlike("alice", "zebra"));
    assert!(!matches_search_soundlike("", "x"));
    // A letterless query word has no code, so it must appear literally.
    assert!(matches_search_soundlike("invoice 2024", "2024"));
    assert!(!matches_search_soundlike("invoice 2023", "2024"));
    // A word too short to code is substring-only: two different short logins must NOT be
    // confused (u2 would otherwise code the same as u1 and pull in the wrong account).
    assert!(matches_search_soundlike("u2", "u2"));
    assert!(!matches_search_soundlike("u1", "u2"));
    assert!(!matches_search_soundlike("Wu", "Ho"), "2-letter words are not phonetically compared");
}

#[test]
fn unix_now_is_a_realistic_timestamp() {
    // Guards the clock source (and kills a "return a constant" mutation): the
    // value must be after 2023-11-14 and before 2100.
    let now = unix_now();
    assert!(now > 1_700_000_000, "timestamp implausibly early: {now}");
    assert!(now < 4_102_444_800, "timestamp implausibly late: {now}");
}

#[test]
fn upsert_inserts_then_edits_with_history() {
    let mut list: Vec<Account> = Vec::new();
    let mut a = Account::new().unwrap();
    a.account_type = "Checking".into();
    a.username = "alice".into();
    let id = a.id.clone();
    upsert(&mut list, a);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].history.len(), 1); // created

    let mut edit = list[0].clone();
    edit.username = "bob".into();
    edit.password = "s3cret".into();
    upsert(&mut list, edit);

    assert_eq!(list.len(), 1, "same id replaces, not appends");
    assert_eq!(list[0].id, id, "id stable");
    let h = &list[0].history;
    assert!(h.iter().any(|c| c.detail.contains("username")));
    // Password value is recorded in history (accepted decision).
    assert!(h.iter().any(|c| c.detail.contains("s3cret")));
}

#[test]
fn remove_logs_audit() {
    let mut list: Vec<Instruction> = Vec::new();
    let mut i = Instruction::new().unwrap();
    i.title = "Read me".into();
    let id = i.id.clone();
    upsert(&mut list, i);
    let mut audit = Vec::new();
    assert!(remove(&mut list, &id, &mut audit, "Instruction"));
    assert!(audit.iter().any(|c| c.action == "deleted" && c.detail.contains("Read me")));
    assert!(!remove(&mut list, &id, &mut audit, "Instruction"));
}

#[test]
fn account_diff_tracks_subtype_and_review() {
    let mut old = Account::new().unwrap();
    old.account_type = "Financial".into();
    let mut new = old.clone();
    new.account_subtype = "IRA".into();
    new.review = true;
    new.closed_as_of = "2026-06-18".into();
    let now = unix_now();
    let changes = old.diff(&new, now);
    assert!(changes.iter().any(|c| c.detail.contains("subtype") && c.detail.contains("IRA")));
    assert!(changes.iter().any(|c| c.detail.contains("review") && c.detail.contains("true")));
    assert!(changes.iter().any(|c| c.detail.contains("closed_as_of") && c.detail.contains("2026-06-18")));
    // Unchanged record yields no changes.
    assert!(old.diff(&old.clone(), now).is_empty());
}

#[test]
fn unquote_path_strips_only_a_matched_double_quote_pair() {
    // A quoted "Copy as path" is accepted; the inner content (incl. spaces) is kept.
    assert_eq!(unquote_path("\"/home/me/My File.pdf\""), "/home/me/My File.pdf");
    assert_eq!(unquote_path("\"C:\\Users\\me\\a b.pdf\""), "C:\\Users\\me\\a b.pdf");
    // Surrounding whitespace around the quotes is trimmed first.
    assert_eq!(unquote_path("  \"/x/y.txt\"  "), "/x/y.txt");
    // An unquoted path is only trimmed.
    assert_eq!(unquote_path("  /x/y.txt  "), "/x/y.txt");
    // A lone quote at one end is a real path char — left alone (still outer-trimmed).
    assert_eq!(unquote_path("\"/x/y.txt"), "\"/x/y.txt");
    assert_eq!(unquote_path("/x/y.txt\""), "/x/y.txt\"");
    // Degenerate inputs don't panic.
    assert_eq!(unquote_path("\""), "\"");
    assert_eq!(unquote_path("\"\""), "");
    assert_eq!(unquote_path(""), "");
}

#[test]
fn effective_doc_filename_uses_the_basename_of_a_quoted_source() {
    // With no explicit name, the default filename is the source's basename — and a
    // quoted source resolves to the right basename, not one with a stray quote.
    assert_eq!(effective_doc_filename("", "\"/home/me/My File.pdf\""), "My File.pdf");
    assert_eq!(effective_doc_filename("keep.pdf", "\"/x/other.pdf\""), "keep.pdf");
}

#[test]
fn urgent_record_new_diff_label_and_trim() {
    let u = Urgent::new().unwrap();
    assert_eq!(u.label(), "(urgent note)", "blank urgent shows a placeholder label");
    let mut edited = u.clone();
    edited.title = "  Call the lawyer  ".into();
    edited.description = "Safe key is in the desk drawer.".into();
    // Diff tracks both free-text fields.
    let c = u.diff(&edited, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("title")));
    assert!(c.iter().any(|x| x.detail.contains("description")));
    // Trim tidies the fields; the trimmed title becomes the list label.
    assert!(edited.trim_fields());
    assert_eq!(edited.title, "Call the lawyer");
    assert_eq!(edited.label(), "Call the lawyer");
    // Unchanged record yields no diff.
    assert!(edited.diff(&edited.clone(), unix_now()).is_empty());
}

#[test]
fn asset_diff_tracks_new_fields() {
    let old = AssetLiability::new().unwrap();
    let mut new = old.clone();
    new.url = "https://x".into();
    new.beneficiary = "Spouse".into();
    new.review = true;
    new.statement = Some("blob1".into());
    new.linked_accounts = vec!["acc1".into()];
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("url")));
    assert!(c.iter().any(|x| x.detail.contains("beneficiary")));
    assert!(c.iter().any(|x| x.detail.contains("review")));
    assert!(c.iter().any(|x| x.detail.contains("statement document changed")));
    // Link changes are logged generically, never exposing the raw ids.
    assert!(c.iter().any(|x| x.detail == "linked accounts changed"));
    assert!(!c.iter().any(|x| x.detail.contains("acc1")), "raw link ids stay out of history");
}

#[test]
fn account_link_helpers_resolve_and_reverse_lookup() {
    let mut acc = Account::new().unwrap();
    acc.id = "acc1".into();
    acc.title = "Brokerage".into();
    acc.account_type = "Financial".into();
    acc.username = "jane".into();
    let accounts = vec![acc];

    // Forward: id -> label, None for a dangling id.
    assert_eq!(account_label(&accounts, "acc1").unwrap(), "Brokerage - Financial - jane");
    assert!(account_label(&accounts, "gone").is_none(), "dangling id resolves to None");

    // Reverse: which assets link to the account (in list order).
    let mut a1 = AssetLiability::new().unwrap();
    a1.id = "ast1".into();
    a1.title = "Index fund".into();
    a1.linked_accounts = vec!["acc1".into(), "other".into()];
    let mut a2 = AssetLiability::new().unwrap();
    a2.id = "ast2".into();
    a2.linked_accounts = vec!["other".into()];
    let assets = vec![a1, a2];
    let linked = assets_linking_account(&assets, "acc1");
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].0, "ast1");
    assert_eq!(linked[0].1, "[Asset] Index fund");
    assert!(assets_linking_account(&assets, "nobody").is_empty());
}

#[test]
fn trim_fields_leaves_linked_account_ids_untouched() {
    // Link ids are bookkeeping, not free text: a save-time trim must never
    // rewrite them (a "trimmed" id would dangle).
    let mut a = AssetLiability::new().unwrap();
    a.linked_accounts = vec![" acc1 ".into()];
    a.owner = " Jane ".into();
    assert!(a.trim_fields(), "owner is trimmed");
    assert_eq!(a.owner, "Jane");
    assert_eq!(a.linked_accounts, vec![" acc1 ".to_string()], "ids untouched");
}

#[test]
fn labels_are_meaningful_per_type() {
    let mut acc = Account::new().unwrap();
    acc.account_type = "Financial".into();
    acc.username = "jane".into();
    // "Title - Account Type - Username", title omitted when blank.
    assert_eq!(acc.label(), "Financial - jane");
    acc.title = "Joint brokerage".into();
    assert_eq!(acc.label(), "Joint brokerage - Financial - jane");
    acc.title = "   ".into();
    assert_eq!(acc.label(), "Financial - jane", "blank title is dropped");
    // Owner stands in for a missing username; an empty record shows a placeholder.
    let mut bare = Account::new().unwrap();
    bare.owner = "Bob".into();
    assert_eq!(bare.label(), "Bob");
    assert_eq!(Account::new().unwrap().label(), "(account)");

    let mut al = AssetLiability::new().unwrap();
    al.kind = "Liability".into();
    al.description = "Mortgage".into();
    assert_eq!(al.label(), "[Liability] Mortgage");
    // A title (the new field) takes precedence over the description in the label.
    al.title = "Beach house loan".into();
    assert_eq!(al.label(), "[Liability] Beach house loan");

    let re = RealEstate::new().unwrap();
    assert_eq!(re.label(), "(no address)");
    let tw = TrustWill::new().unwrap();
    assert_eq!(tw.label(), "(untitled)");
}

#[test]
fn asset_title_diffs_trims_and_round_trips() {
    // An edit that sets the title records a "title" history entry (and nothing leaks
    // a secret — there are none here).
    let mut old = AssetLiability::new().unwrap();
    old.owner = "Bob".into();
    let mut new = old.clone();
    new.title = "  Vanguard IRA  ".into();
    let changes = old.diff(&new, 1);
    assert!(changes.iter().any(|c| c.detail.starts_with("title:")), "title change tracked: {changes:?}");
    // trim_fields trims the title in place.
    new.trim_fields();
    assert_eq!(new.title, "Vanguard IRA");
    // upsert round-trips the title through a record list.
    let mut list: Vec<AssetLiability> = Vec::new();
    upsert(&mut list, new.clone());
    assert_eq!(list[0].title, "Vanguard IRA");
}

#[test]
fn history_display_detail_masks_password_values_only() {
    // A real password change entry, as `track` would format it.
    let pw = "password: \"hunter2\" -> \"Tr0ub4dor&3\"";
    assert!(detail_is_secret(pw));
    let shown = display_detail(pw);
    assert!(!shown.contains("hunter2"), "old password value is masked: {shown}");
    assert!(!shown.contains("Tr0ub4dor"), "new password value is masked: {shown}");
    assert!(shown.starts_with("password:"), "field name is kept for the audit trail: {shown}");
    // The RealEstate portal passwords are masked too.
    for f in ["property_mgmt_password", "insurance_password", "hoa_password", "tax_portal_password"] {
        let d = format!("{f}: \"SEKRET1\" -> \"SEKRET2\"");
        assert!(detail_is_secret(&d), "{f} is secret");
        let shown = display_detail(&d);
        assert!(!shown.contains("SEKRET"), "{f} values masked: {shown}");
        assert!(shown.starts_with(f), "{f} name kept: {shown}");
    }
    // Non-secret fields pass through verbatim.
    let owner = "owner: \"\" -> \"Jane\"";
    assert!(!detail_is_secret(owner));
    assert_eq!(display_detail(owner), owner);
    // A "created" entry whose label happens not to be a password is untouched.
    assert_eq!(display_detail("Financial - jane"), "Financial - jane");
}

#[test]
fn account_trim_fields_trims_every_text_field_including_password() {
    let mut a = Account::new().unwrap();
    a.title = "  Brokerage  ".into();
    a.account_type = " Financial ".into();
    a.account_subtype = "\tIRA\t".into();
    a.owner = " Jane ".into();
    a.username = "  jane@x.com ".into();
    a.password = "  s3cret  ".into(); // password IS trimmed (configured policy)
    a.url = " https://x ".into();
    a.closed_as_of = " 2026-06-18 ".into();
    a.description = "\n notes \n".into();
    assert!(a.trim_fields(), "fields with surrounding whitespace report a change");
    assert_eq!(a.title, "Brokerage");
    assert_eq!(a.account_type, "Financial");
    assert_eq!(a.account_subtype, "IRA");
    assert_eq!(a.owner, "Jane");
    assert_eq!(a.username, "jane@x.com");
    assert_eq!(a.password, "s3cret");
    assert_eq!(a.url, "https://x");
    assert_eq!(a.closed_as_of, "2026-06-18");
    assert_eq!(a.description, "notes");
    // Interior whitespace is preserved; only the ends are trimmed.
    let mut b = Account::new().unwrap();
    b.username = "a b".into();
    b.password = "p w".into();
    assert!(!b.trim_fields(), "already-trimmed fields report no change");
    assert_eq!(b.username, "a b");
    assert_eq!(b.password, "p w");
}

#[test]
fn trim_all_accounts_trims_in_bulk_and_records_history() {
    let mut accts = Vec::new();
    let mut a = Account::new().unwrap();
    a.owner = "  Alice  ".into();
    let mut b = Account::new().unwrap();
    b.owner = "Bob".into(); // already clean
    accts.push(a);
    accts.push(b);
    let n = trim_all_accounts(&mut accts);
    assert_eq!(n, 1, "only the dirty account is counted");
    assert_eq!(accts[0].owner, "Alice");
    assert_eq!(accts[1].owner, "Bob");
    // The change is auditable: the trimmed account's history records owner old->new.
    assert!(
        accts[0].history.iter().any(|c| c.detail.contains("owner") && c.detail.contains("Alice")),
        "the bulk trim is recorded in history"
    );
    // Running it again is a no-op (nothing left to trim).
    assert_eq!(trim_all_accounts(&mut accts), 0);
}

#[test]
fn trim_fields_works_for_every_record_type() {
    // RealEstate: a portal password and ordinary fields are all trimmed.
    let mut re = RealEstate::new().unwrap();
    re.address = "  1 Main St  ".into();
    re.hoa_password = "  hoapw  ".into();
    re.comments = " hi ".into();
    assert!(re.trim_fields());
    assert_eq!(re.address, "1 Main St");
    assert_eq!(re.hoa_password, "hoapw", "portal passwords are trimmed too");
    assert_eq!(re.comments, "hi");

    let mut tw = TrustWill::new().unwrap();
    tw.document = " Will ".into();
    tw.usage = "  notes  ".into();
    assert!(tw.trim_fields());
    assert_eq!((tw.document.as_str(), tw.usage.as_str()), ("Will", "notes"));

    let mut al = AssetLiability::new().unwrap();
    al.owner = "  Bob  ".into();
    al.approx_value = " 100 ".into();
    assert!(al.trim_fields());
    assert_eq!((al.owner.as_str(), al.approx_value.as_str()), ("Bob", "100"));

    let mut ins = Instruction::new().unwrap();
    ins.title = " T ".into();
    assert!(ins.trim_fields());
    assert_eq!(ins.title, "T");

    let mut tax = TaxFiling::new().unwrap();
    tax.year = " 2024 ".into();
    assert!(tax.trim_fields());
    assert_eq!(tax.year, "2024");

    let mut gd = GeneralDocument::new().unwrap();
    gd.title = " Deed ".into();
    assert!(gd.trim_fields());
    assert_eq!(gd.title, "Deed");

    // An already-clean record reports no change.
    let mut clean = Instruction::new().unwrap();
    clean.title = "Done".into();
    assert!(!clean.trim_fields());
}

#[test]
fn trim_all_records_trims_every_tab_of_the_vault() {
    // Put EXACTLY ONE dirty record in every one of the seven collections, so the
    // expected total (7) pins down each `+` in the `trim_all_records` sum: any
    // operator mutation (`+`→`-`/`*`) changes the total and fails this assert.
    // (Caught a real mutation-testing gap where zero-count tabs let those mutants
    // survive.)
    let mut v = Vault::default();
    let mut ins = Instruction::new().unwrap();
    ins.title = " Note ".into();
    v.instructions.push(ins);
    let mut tw = TrustWill::new().unwrap();
    tw.document = " Will ".into();
    v.trust_wills.push(tw);
    let mut al = AssetLiability::new().unwrap();
    al.owner = "  Bob  ".into();
    v.assets.push(al);
    let mut a = Account::new().unwrap();
    a.owner = "  Alice  ".into();
    v.accounts.push(a);
    let mut re = RealEstate::new().unwrap();
    re.address = "  Home  ".into();
    v.real_estate.push(re);
    let mut tax = TaxFiling::new().unwrap();
    tax.year = " 2024 ".into();
    v.tax_filings.push(tax);
    let mut gd = GeneralDocument::new().unwrap();
    gd.title = " Deed ".into();
    v.general_documents.push(gd);

    let n = trim_all_records(&mut v);
    assert_eq!(n, 7, "one dirty record in each of the 7 collections is trimmed (pins every `+`)");
    assert_eq!(v.instructions[0].title, "Note");
    assert_eq!(v.trust_wills[0].document, "Will");
    assert_eq!(v.assets[0].owner, "Bob");
    assert_eq!(v.accounts[0].owner, "Alice");
    assert_eq!(v.real_estate[0].address, "Home");
    assert_eq!(v.tax_filings[0].year, "2024");
    assert_eq!(v.general_documents[0].title, "Deed");
    // The trim is auditable in the changed record's own history.
    assert!(
        v.accounts[0].history.iter().any(|c| c.detail.contains("owner") && c.detail.contains("Alice")),
        "the whole-vault trim is recorded in history"
    );
    // Idempotent.
    assert_eq!(trim_all_records(&mut v), 0);
}

#[test]
fn account_title_diffs_and_is_serde_backward_compatible() {
    // The new title field is tracked in the history diff.
    let mut a = Account::new().unwrap();
    a.account_type = "Financial".into();
    let mut b = a.clone();
    b.title = "Brokerage".into();
    let c = a.diff(&b, 100);
    assert!(c.iter().any(|x| x.detail.contains("title") && x.detail.contains("Brokerage")));
    // An older account JSON that predates `title` still deserializes (the field
    // is #[serde(default)]), with title defaulting to "".
    let old = serde_json::json!({
        "id": "acc1", "account_type": "Financial", "account_subtype": "", "owner": "Jane",
        "username": "jane", "password": "pw", "description": "", "url": "",
        "review": false, "created_at": 1, "updated_at": 1, "history": []
    });
    let acc: Account = serde_json::from_value(old).expect("old account without title must load");
    assert_eq!(acc.title, "", "missing title defaults to empty");
    assert_eq!(acc.closed_as_of, "", "missing closed_as_of defaults to empty");
    assert_eq!(acc.username, "jane", "old fields preserved");
}

#[test]
fn asset_tree_groups_by_owner_kind_type_and_skips_empty() {
    let mk = |id: &str, owner: &str, kind: &str, atype: &str, title: &str| {
        let mut a = AssetLiability::new().unwrap();
        a.id = id.into();
        a.owner = owner.into();
        a.kind = kind.into();
        a.asset_type = atype.into();
        a.title = title.into();
        a
    };
    let assets = vec![
        mk("1", "Bob", "Asset", "Bank", "Savings"),
        mk("2", "Bob", "Asset", "Bank", "Checking"),
        mk("3", "Bob", "Liability", "Loan", "Car"),
        mk("4", "", "Asset", "", "Cash"), // no owner/type → kind-group at the root, then a leaf
    ];
    let root = asset_tree(&assets);
    // Top level: the owner-less entry's kind group "Asset" + "Bob", sorted.
    let tops: Vec<&str> = root.children.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(tops, vec!["Asset", "Bob"]);
    // The owner-less "Cash" sits as a leaf under the top-level "Asset" group.
    let asset_grp = root.children.iter().find(|c| c.label == "Asset").unwrap();
    assert_eq!(asset_grp.leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), vec!["Cash"]);
    // Bob → [Asset, Liability]; Bob/Asset/Bank → [Checking, Savings] (sorted, no [kind] prefix).
    let bob = root.children.iter().find(|c| c.label == "Bob").unwrap();
    assert_eq!(bob.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), vec!["Asset", "Liability"]);
    let bank = bob.children.iter().find(|c| c.label == "Asset").unwrap().children.iter().find(|c| c.label == "Bank").unwrap();
    assert_eq!(bank.leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), vec!["Checking", "Savings"]);
}

#[test]
fn account_tree_owner_first_skips_empty_levels_and_sorts() {
    let mk = |owner: &str, ty: &str, st: &str, title: &str| {
        let mut a = Account::new().unwrap();
        a.owner = owner.into();
        a.account_type = ty.into();
        a.account_subtype = st.into();
        a.title = title.into();
        a
    };
    let accts = vec![
        mk("Alice", "Financial", "Bank", "Joint brokerage"),
        mk("Alice", "Financial", "Bank", "Emergency fund"),
        mk("Alice", "Financial", "IRA", "Retirement"),
        mk("Bob", "Email", "", "Personal gmail"), // no subtype -> leaf directly under Email
        mk("", "Email", "", "Orphan mail"),       // no owner -> Email at the top level
        mk("", "", "", "Loose account"),          // no grouping at all -> top-level leaf
    ];
    let root = account_tree(&accts);

    // Top-level groups are OWNERS, plus the no-owner account's type "Email"
    // promoted up. Sorted case-insensitively: Alice, Bob, Email.
    assert_eq!(root.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Alice", "Bob", "Email"]);
    // The fully-ungrouped account is a leaf at the root.
    assert_eq!(root.leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["Loose account"]);

    // Alice → Financial → {Bank, IRA}.
    let alice = &root.children[0];
    assert_eq!(alice.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Financial"]);
    let fin = &alice.children[0];
    assert_eq!(fin.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Bank", "IRA"]);
    // Bank's leaves sorted by title; every leaf carries its account id.
    assert_eq!(
        fin.children[0].leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(),
        ["Emergency fund", "Joint brokerage"]
    );
    assert!(!fin.children[0].leaves[0].id.is_empty());

    // Bob → Email → leaf directly (the empty subtype level is skipped, no node).
    let bob = &root.children[1];
    assert_eq!(bob.children[0].label, "Email");
    assert!(bob.children[0].children.is_empty(), "empty subtype produces no child node");
    assert_eq!(bob.children[0].leaves[0].title, "Personal gmail");

    // The no-owner Email account: its type is a top-level group; the leaf hangs
    // directly off it (owner and subtype both skipped).
    let email_top = &root.children[2];
    assert_eq!(email_top.label, "Email");
    assert_eq!(email_top.leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["Orphan mail"]);
}

#[test]
fn account_tree_treats_whitespace_only_levels_as_empty() {
    // Regression (deep-hunt): a whitespace-only grouping value (e.g. legacy/imported
    // data not yet re-saved) must NOT create a blank group node, and " " vs "  " must
    // not split into two groups. Both should behave exactly like an empty level.
    let mk = |owner: &str, title: &str| {
        let mut a = Account::new().unwrap();
        a.owner = owner.into();
        a.account_type = "Email".into();
        a.title = title.into();
        a
    };
    let accts = vec![mk(" ", "Spacey one"), mk("  ", "Spacey two"), mk("", "Empty owner")];
    let root = account_tree(&accts);
    // No top-level OWNER groups: every owner is blank/whitespace, so all three are
    // grouped only by their (real) type "Email" promoted to the top level.
    assert_eq!(root.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Email"]);
    assert!(root.children.iter().all(|c| !c.label.trim().is_empty()), "no blank group node");
    let email = &root.children[0];
    assert_eq!(
        email.leaves.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(),
        ["Empty owner", "Spacey one", "Spacey two"],
        "all three land under the single Email group, none under a whitespace owner"
    );
}

#[test]
fn account_facets_cross_filter() {
    let mk = |t: &str, o: &str, ti: &str| {
        let mut a = Account::new().unwrap();
        a.account_type = t.into();
        a.owner = o.into();
        a.title = ti.into();
        a
    };
    let accts = vec![mk("Email", "Alice", "Personal"), mk("Email", "Bob", "Work"), mk("Bank", "Alice", "Savings")];

    // No filters: every distinct value (sorted).
    let f = account_facets(&accts, "", "", "", "", "", false);
    assert_eq!(f.types, vec!["Bank", "Email"]);
    assert_eq!(f.owners, vec!["Alice", "Bob"]);
    assert_eq!(f.titles, vec!["Personal", "Savings", "Work"]);

    // type=Email narrows owners + titles to Email accounts; the TYPE list itself
    // still shows both (its own selection is ignored when building its options).
    let f = account_facets(&accts, "Email", "", "", "", "", false);
    assert_eq!(f.owners, vec!["Alice", "Bob"]);
    assert_eq!(f.titles, vec!["Personal", "Work"]);
    assert_eq!(f.types, vec!["Bank", "Email"], "type's own facet ignores the type selection");

    // owner=Alice narrows types + titles to Alice's accounts.
    let f = account_facets(&accts, "", "", "Alice", "", "", false);
    assert_eq!(f.types, vec!["Bank", "Email"]);
    assert_eq!(f.titles, vec!["Personal", "Savings"]);

    // Combined type=Email + owner=Bob -> only the matching title.
    let f = account_facets(&accts, "Email", "", "Bob", "", "", false);
    assert_eq!(f.titles, vec!["Work"]);
    assert_eq!(f.types, vec!["Email"], "owner=Bob means only Email has a Bob account");
}

#[test]
fn tax_filing_new_diff_label_and_folder() {
    let mut t = TaxFiling::new().unwrap();
    assert!(t.year.is_empty() && t.documents.is_empty());
    assert_eq!(t.label(), "(no year)");
    t.year = "2024".into();
    assert_eq!(t.label(), "Taxes 2024");
    // With an owner the label becomes "<owner> - <year>".
    t.owner = "Jane".into();
    assert_eq!(t.label(), "Jane - 2024");

    let mut edited = t.clone();
    edited.owner = "Joint".into();
    edited.notes = "filed late".into();
    edited.documents.push("blobid".into());
    let changes = t.diff(&edited, unix_now());
    assert!(changes.iter().any(|c| c.detail.contains("owner")));
    assert!(changes.iter().any(|c| c.detail.contains("notes")));
    assert!(changes.iter().any(|c| c.detail.contains("documents") && c.detail.contains("0 -> 1")));
    assert!(t.diff(&t.clone(), unix_now()).is_empty(), "unchanged record yields no diff");

    // Folder convention: taxes/<sanitized-year>, with a safe fallback.
    assert_eq!(tax_doc_location("2024"), "taxes/2024");
    assert_eq!(tax_doc_location(" 2023/ "), "taxes/2023");
    assert_eq!(tax_doc_location(""), "taxes/unspecified");
}

#[test]
fn compact_history_includes_urgent_notes() {
    // The URGENT collection must be trimmed by compact_history and counted by
    // history_stats like every other record type.
    //
    // Found by mutation testing: `n += count(...)` in the urgent loop of
    // `history_stats` could be changed to `-=` or `*=` and the whole suite still
    // passed. That loop runs FIRST, while `n` is still 0, so with no urgent history
    // anywhere in the tests all three operators are indistinguishable. `history_stats`
    // is what `compact --json` reports as "would remove N history entries", so an
    // undetected break there is a dry-run that lies about a destructive operation.
    let mut vault = Vault::default();
    let mut u = Urgent::default();
    u.history = vec![
        Change { at: 1, action: "u".into(), detail: String::new() },
        Change { at: 500, action: "u".into(), detail: String::new() },
    ];
    vault.urgent.push(u);

    // Counted, and counted BEFORE any other collection contributes — so an urgent-only
    // vault pins the first loop on its own.
    assert_eq!(history_stats(&vault, None, true), 2, "drop-all counts both entries");
    assert_eq!(history_stats(&vault, Some(300), false), 1, "cutoff counts only the older one");

    // And the count agrees with what removal actually does.
    assert_eq!(compact_history(&mut vault, Some(300), false), 1);
    assert_eq!(vault.urgent[0].history.len(), 1, "the newer entry is kept");
    assert_eq!(compact_history(&mut vault, None, true), 1);
    assert!(vault.urgent[0].history.is_empty());
}

#[test]
fn compact_history_includes_tax_filings() {
    // The Taxes collection must be trimmed by compact_history and counted by
    // history_stats like the other five record types.
    let mut vault = Vault::default();
    let mut t = TaxFiling::default();
    t.history = vec![Change { at: 1, action: "u".into(), detail: String::new() }];
    vault.tax_filings.push(t);
    assert_eq!(history_stats(&vault, None, true), 1);
    assert_eq!(compact_history(&mut vault, None, true), 1);
    assert!(vault.tax_filings[0].history.is_empty());
}

#[test]
fn real_estate_diff_tracks_portals_docs_and_folder() {
    let old = RealEstate::new().unwrap();
    let mut new = old.clone();
    new.financing_balance = "250000".into();
    new.property_mgmt_url = "https://pm.example".into();
    new.insurance_password = "s3cret".into();
    new.hoa_username = "owner1".into();
    new.comments = "tenant occupied".into();
    new.documents.push("blob".into());
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("financing_balance")));
    assert!(c.iter().any(|x| x.detail.contains("property_mgmt_url")));
    assert!(c.iter().any(|x| x.detail.contains("insurance_password") && x.detail.contains("s3cret")));
    assert!(c.iter().any(|x| x.detail.contains("hoa_username")));
    assert!(c.iter().any(|x| x.detail.contains("comments")));
    assert!(c.iter().any(|x| x.detail.contains("documents") && x.detail.contains("0 -> 1")));

    // Folder convention: real-estate/<sanitized-address>, with a fallback.
    assert_eq!(real_estate_doc_location("123 Main St"), "real-estate/123mainst");
    assert_eq!(real_estate_doc_location(""), "real-estate/property");
}

#[test]
fn new_records_have_distinct_ids_and_timestamps() {
    let a = Account::new().unwrap();
    let b = Account::new().unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.id.len(), 32); // 128-bit hex
    assert!(a.created_at > 0 && a.created_at == a.updated_at);
    assert_eq!(AssetLiability::new().unwrap().kind, "Asset"); // default kind
}

#[test]
fn civil_from_unix_known_dates() {
    assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    assert_eq!(civil_from_unix(1_609_459_200), (2021, 1, 1, 0, 0, 0));
    // A leap day: 2024-02-29T00:00:00Z = 1709164800.
    assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    // The day AFTER the leap day exercises the Feb->Mar month transition.
    assert_eq!(civil_from_unix(1_709_251_200), (2024, 3, 1, 0, 0, 0));
    // Non-zero time-of-day pins the h/m/s extraction (sod/3600, %3600/60, %60).
    assert_eq!(civil_from_unix(1_609_459_200 + 3600 + 120 + 45), (2021, 1, 1, 1, 2, 45));
    // The last second of a year (year rollover boundary).
    assert_eq!(civil_from_unix(1_609_459_199), (2020, 12, 31, 23, 59, 59));
    assert_eq!(civil_from_unix(-100), (1970, 1, 1, 0, 0, 0)); // clamps to epoch
}

#[test]
fn parse_ymd_utc_known_dates_and_roundtrip() {
    assert_eq!(parse_ymd_utc("1970-01-01"), Some(0));
    assert_eq!(parse_ymd_utc("2021-01-01"), Some(1_609_459_200));
    // Leap day.
    assert_eq!(parse_ymd_utc("2024-02-29"), Some(1_709_164_800));
    // Whitespace is trimmed; unpadded fields parse.
    assert_eq!(parse_ymd_utc("  2021-1-1  "), Some(1_609_459_200));
    // Round-trips against the civil formatter at midnight.
    for ts in [0, 1_609_459_200, 1_709_164_800, 4_102_444_800] {
        let (y, m, d, ..) = civil_from_unix(ts);
        assert_eq!(unix_from_civil(y, m, d, 0, 0, 0), ts);
    }
}

#[test]
fn parse_ymd_utc_rejects_invalid() {
    for s in ["2026-02-31", "2026-13-01", "2026-01-32", "1969-12-31", "not-a-date", "2026/01/01", "20260101", "2026-01", ""] {
        assert!(parse_ymd_utc(s).is_none(), "{s:?} must be rejected");
    }
}

#[test]
fn compact_history_cutoff_and_drop_all_preserve_audit() {
    let mut vault = Vault::default();
    let mut a = Account::default();
    a.history = vec![
        Change { at: 100, action: "updated".into(), detail: "a".into() },
        Change { at: 500, action: "updated".into(), detail: "b".into() },
    ];
    vault.accounts.push(a);
    vault.audit.push(Change::new("opened", String::new()));

    // Counting matches the actual trim, and the audit is never counted/touched.
    assert_eq!(history_stats(&vault, Some(300), false), 1);
    assert_eq!(history_stats(&vault, None, true), 2);

    let removed = compact_history(&mut vault, Some(300), false);
    assert_eq!(removed, 1);
    assert_eq!(vault.accounts[0].history.len(), 1);
    assert_eq!(vault.accounts[0].history[0].at, 500, "kept the at >= cutoff entry");
    assert_eq!(vault.audit.len(), 1, "audit untouched by record-history trim");

    let removed2 = compact_history(&mut vault, None, true);
    assert_eq!(removed2, 1);
    assert!(vault.accounts[0].history.is_empty());
    assert_eq!(vault.audit.len(), 1, "audit still untouched after drop-all");
}

#[test]
fn parse_ymd_utc_boundaries_have_no_overflow() {
    assert_eq!(parse_ymd_utc("1970-01-01"), Some(0));
    // The far-future date stays within i64 (no multiplication overflow/panic)
    // and round-trips through the civil formatter.
    let secs = parse_ymd_utc("9999-12-31").expect("9999-12-31 is valid");
    assert!(secs > 0);
    assert_eq!(civil_from_unix(secs), (9999, 12, 31, 0, 0, 0));
}

#[test]
fn civil_from_unix_clamps_to_the_four_digit_year_calendar() {
    // A huge/odd created_at or updated_at (up to i64::MAX) must NOT widen the year past 4
    // digits, or the fixed-width {:04}/{:02} formatters (compact_utc, csv::iso_utc,
    // ui::format_time) would desync a CSV column or a timestamped export filename.
    assert_eq!(civil_from_unix(i64::MAX), (9999, 12, 31, 23, 59, 59));
    assert_eq!(compact_utc(i64::MAX), "99991231-235959");
    assert_eq!(compact_utc(i64::MAX).len(), 15, "YYYYMMDD-HHMMSS stays fixed-width");
    // Negatives still clamp to the epoch (unchanged low bound).
    assert_eq!(civil_from_unix(i64::MIN), (1970, 1, 1, 0, 0, 0));
    assert_eq!(compact_utc(-1), "19700101-000000");
}

#[test]
fn days_from_civil_inverts_civil_from_unix() {
    // Round-trip midnight timestamps across centuries + leap days.
    for ts in [0i64, 86_400, 951_782_400, 1_709_164_800, 4_102_444_800, 253_370_764_800] {
        let (y, m, d, _, _, _) = civil_from_unix(ts);
        assert_eq!(unix_from_civil(y, m, d, 0, 0, 0), ts, "round-trip failed for ts={ts}");
    }
}

#[test]
fn compact_history_cutoff_is_inclusive_keep() {
    let mut vault = Vault::default();
    let mut a = Account::default();
    a.history = vec![
        Change { at: 999, action: "u".into(), detail: String::new() },
        Change { at: 1000, action: "u".into(), detail: String::new() },
        Change { at: 1001, action: "u".into(), detail: String::new() },
    ];
    vault.accounts.push(a);
    // cutoff == 1000: only at=999 is older (dropped); at=1000 is kept (inclusive).
    let removed = compact_history(&mut vault, Some(1000), false);
    assert_eq!(removed, 1);
    assert_eq!(vault.accounts[0].history.iter().map(|c| c.at).collect::<Vec<_>>(), vec![1000, 1001]);
}

#[test]
fn compact_history_handles_empty_and_every_record_type() {
    let mut vault = Vault::default();
    // Empty vault: nothing to do, no panic.
    assert_eq!(history_stats(&vault, Some(0), false), 0);
    assert_eq!(compact_history(&mut vault, None, true), 0);
    // One+ history entries in each of the five record types.
    let mk = |at| Change { at, action: "u".into(), detail: String::new() };
    let mut ins = Instruction::default();
    ins.history = vec![mk(1)];
    let mut tw = TrustWill::default();
    tw.history = vec![mk(1), mk(2)];
    let mut al = AssetLiability::default();
    al.history = vec![mk(1)];
    let mut ac = Account::default();
    ac.history = vec![mk(1)];
    let mut re = RealEstate::default();
    re.history = vec![mk(1)];
    vault.instructions.push(ins);
    vault.trust_wills.push(tw);
    vault.assets.push(al);
    vault.accounts.push(ac);
    vault.real_estate.push(re);
    // history_stats must agree with the actual removal count across all types.
    assert_eq!(history_stats(&vault, None, true), 6);
    assert_eq!(compact_history(&mut vault, None, true), 6, "all five record types trimmed");
    assert!(vault.trust_wills[0].history.is_empty());
}

// ---- Added: hardening tests for Taxes + expanded Real Estate -------------

/// `TaxFiling::new()` produces a stamped, empty filing with a 128-bit hex id
/// and equal created/updated timestamps (matching the macro's contract).
#[test]
fn tax_filing_new_is_stamped_and_empty() {
    let t = TaxFiling::new().unwrap();
    assert_eq!(t.id.len(), 32, "128-bit hex id");
    assert!(t.id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(t.created_at > 0 && t.created_at == t.updated_at);
    assert!(t.year.is_empty() && t.notes.is_empty() && t.documents.is_empty());
    assert!(t.history.is_empty());
    let other = TaxFiling::new().unwrap();
    assert_ne!(t.id, other.id, "ids are distinct");
}

/// `TaxFiling::label()`: placeholder when fully blank, legacy `Taxes <year>` when
/// only a year is set, and `<owner> - <year>` (or just the owner) once an owner is
/// present — including odd, non-sanitized year strings (the label is verbatim).
#[test]
fn tax_filing_label_variants() {
    let mut t = TaxFiling::default();
    assert_eq!(t.label(), "(no year)");
    t.year = "2024".into();
    assert_eq!(t.label(), "Taxes 2024");
    // The label does NOT sanitize; it echoes the raw year.
    t.year = "FY-2024 (amended)".into();
    assert_eq!(t.label(), "Taxes FY-2024 (amended)");
    // Owner present: "<owner> - <year>". Owner-only drops the year part.
    t.owner = "Jane".into();
    assert_eq!(t.label(), "Jane - FY-2024 (amended)");
    t.year = String::new();
    assert_eq!(t.label(), "Jane");
}

/// Every TaxFiling field that the diff tracks, exercised individually.
#[test]
fn tax_filing_diff_covers_each_field() {
    let base = TaxFiling::default();
    let now = unix_now();

    // owner
    let mut n = base.clone();
    n.owner = "Jane".into();
    let c = base.diff(&n, now);
    assert!(c.iter().any(|x| x.detail.contains("owner") && x.detail.contains("Jane")));

    // year
    let mut n = base.clone();
    n.year = "2025".into();
    let c = base.diff(&n, now);
    assert!(c.iter().any(|x| x.detail.contains("year") && x.detail.contains("2025")));
    assert!(c.iter().all(|x| x.action == "updated"));

    // notes
    let mut n = base.clone();
    n.notes = "extension filed".into();
    let c = base.diff(&n, now);
    assert!(c.iter().any(|x| x.detail.contains("notes") && x.detail.contains("extension filed")));

    // documents: count goes up
    let mut n = base.clone();
    n.documents = vec!["a".into(), "b".into()];
    let c = base.diff(&n, now);
    assert!(c.iter().any(|x| x.detail.contains("documents") && x.detail.contains("0 -> 2")));

    // documents: count goes down (removal)
    let mut start = base.clone();
    start.documents = vec!["a".into(), "b".into(), "c".into()];
    let mut fewer = start.clone();
    fewer.documents = vec!["a".into()];
    let c = start.diff(&fewer, now);
    assert!(c.iter().any(|x| x.detail.contains("documents") && x.detail.contains("3 -> 1")));
}

/// A document set that changes contents but keeps the same length is still a
/// diff (the diff compares the Vec, not just its length) — yet the human
/// detail reports the (unchanged) count, which is the documented behaviour.
#[test]
fn tax_filing_diff_detects_swapped_doc_same_count() {
    let mut old = TaxFiling::default();
    old.documents = vec!["blob-old".into()];
    let mut new = old.clone();
    new.documents = vec!["blob-new".into()];
    let c = old.diff(&new, unix_now());
    assert_eq!(c.len(), 1, "a swapped (but equal-count) document is a change");
    assert!(c[0].detail.contains("documents") && c[0].detail.contains("1 -> 1"));
}

/// The diff must not leak document volume-file ids into the history detail.
#[test]
fn tax_filing_diff_does_not_expose_doc_ids() {
    let old = TaxFiling::default();
    let mut new = old.clone();
    new.documents = vec!["super-secret-blob-id".into()];
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("documents")));
    assert!(!c.iter().any(|x| x.detail.contains("super-secret-blob-id")), "doc id must not appear in history");
}

/// An identical TaxFiling produces no diff at all (every field equal).
#[test]
fn tax_filing_unchanged_yields_no_diff() {
    let mut t = TaxFiling::default();
    t.year = "2024".into();
    t.notes = "n".into();
    t.documents = vec!["d1".into(), "d2".into()];
    assert!(t.diff(&t.clone(), unix_now()).is_empty());
}

/// All three TaxFiling text fields changing at once yields three changes.
#[test]
fn tax_filing_diff_all_fields_at_once() {
    let old = TaxFiling::default();
    let mut new = old.clone();
    new.year = "2026".into();
    new.notes = "all changed".into();
    new.documents = vec!["d".into()];
    let c = old.diff(&new, unix_now());
    assert_eq!(c.len(), 3, "year + notes + documents");
}

// --- Expanded RealEstate diff: one test per NEW field --------------------

#[test]
fn real_estate_diff_financing_balance() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.financing_balance = "199999.99".into();
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("financing_balance") && x.detail.contains("199999.99")));
    assert_eq!(c.len(), 1, "only one field changed");
}

#[test]
fn real_estate_diff_property_mgmt_portal() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.property_mgmt_url = "https://pm.example".into();
    new.property_mgmt_username = "pmuser".into();
    new.property_mgmt_password = "pmpass".into();
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("property_mgmt_url")));
    assert!(c.iter().any(|x| x.detail.contains("property_mgmt_username") && x.detail.contains("pmuser")));
    // Full before/after of the portal password is recorded (matches Account).
    assert!(c.iter().any(|x| x.detail.contains("property_mgmt_password") && x.detail.contains("pmpass")));
    assert_eq!(c.len(), 3);
}

#[test]
fn real_estate_diff_insurance_portal() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.insurance_url = "https://ins.example".into();
    new.insurance_username = "insuser".into();
    new.insurance_password = "inspass".into();
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("insurance_url")));
    assert!(c.iter().any(|x| x.detail.contains("insurance_username") && x.detail.contains("insuser")));
    assert!(c.iter().any(|x| x.detail.contains("insurance_password") && x.detail.contains("inspass")));
    assert_eq!(c.len(), 3);
}

#[test]
fn real_estate_diff_hoa_portal() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.hoa_url = "https://hoa.example".into();
    new.hoa_username = "hoauser".into();
    new.hoa_password = "hoapass".into();
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("hoa_url")));
    assert!(c.iter().any(|x| x.detail.contains("hoa_username") && x.detail.contains("hoauser")));
    assert!(c.iter().any(|x| x.detail.contains("hoa_password") && x.detail.contains("hoapass")));
    assert_eq!(c.len(), 3);
}

/// The plain `hoa` (dues) field and the `hoa_url` portal field are distinct;
/// changing one must not be reported as the other.
#[test]
fn real_estate_diff_distinguishes_hoa_dues_from_hoa_portal() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.hoa = "$300/mo".into();
    let c = old.diff(&new, unix_now());
    assert_eq!(c.len(), 1);
    // The detail starts with the field name "hoa:"; the portal fields are
    // "hoa_url"/"hoa_username"/"hoa_password" and must not be matched here.
    assert!(c[0].detail.starts_with("hoa:"), "got {:?}", c[0].detail);
}

#[test]
fn real_estate_diff_comments() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.comments = "roof replaced 2025".into();
    let c = old.diff(&new, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("comments") && x.detail.contains("roof replaced 2025")));
    assert_eq!(c.len(), 1);
}

/// documents count change is reported without exposing ids; both grow and
/// shrink are covered, plus a same-count swap.
#[test]
fn real_estate_diff_documents_count() {
    let old = RealEstate::default();
    let mut grow = old.clone();
    grow.documents = vec!["deed".into(), "policy".into()];
    let c = old.diff(&grow, unix_now());
    assert!(c.iter().any(|x| x.detail.contains("documents") && x.detail.contains("0 -> 2")));
    assert!(!c.iter().any(|x| x.detail.contains("deed") || x.detail.contains("policy")), "doc ids not leaked");

    let mut shrink = grow.clone();
    shrink.documents = vec!["deed".into()];
    let c2 = grow.diff(&shrink, unix_now());
    assert!(c2.iter().any(|x| x.detail.contains("2 -> 1")));

    let mut swap = grow.clone();
    swap.documents = vec!["deed2".into(), "policy2".into()];
    let c3 = grow.diff(&swap, unix_now());
    assert_eq!(c3.len(), 1);
    assert!(c3[0].detail.contains("2 -> 2"), "swap with same count still diffs");
}

/// Every original RealEstate text field is still tracked after the expansion.
#[test]
fn real_estate_diff_original_fields_still_tracked() {
    let old = RealEstate::default();
    let mut new = old.clone();
    new.address = "1 A St".into();
    new.owner = "JT".into();
    new.taxes = "5000".into();
    new.income_account = "inc".into();
    new.financing_account = "fin".into();
    new.payment_account = "pay".into();
    let c = old.diff(&new, unix_now());
    for field in ["address", "owner", "taxes", "income_account", "financing_account", "payment_account"] {
        assert!(c.iter().any(|x| x.detail.contains(field)), "missing diff for {field}");
    }
}

/// Changing EVERY new+old RealEstate field at once yields exactly one change
/// per field (no double-counting, no missing field). This pins the diff's
/// field count so adding/removing a tracked field is caught.
#[test]
fn real_estate_diff_all_fields_counts_exactly() {
    let old = RealEstate::default();
    let mut n = old.clone();
    n.address = "a".into();
    n.owner = "b".into();
    n.taxes = "c".into();
    n.hoa = "d".into();
    n.income_account = "e".into();
    n.financing_account = "f".into();
    n.financing_balance = "g".into();
    n.payment_account = "h".into();
    n.property_mgmt_url = "i".into();
    n.property_mgmt_username = "j".into();
    n.property_mgmt_password = "k".into();
    n.insurance_url = "l".into();
    n.insurance_username = "m".into();
    n.insurance_password = "n".into();
    n.hoa_url = "o".into();
    n.hoa_username = "p".into();
    n.hoa_password = "q".into();
    n.comments = "r".into();
    n.documents = vec!["doc".into()];
    let c = old.diff(&n, unix_now());
    // 18 scalar text fields + 1 documents change = 19.
    assert_eq!(c.len(), 19, "expected one change per tracked field; got {:?}", c.iter().map(|x| x.detail.clone()).collect::<Vec<_>>());
}

/// An identical RealEstate (with every new field populated) yields no diff.
#[test]
fn real_estate_unchanged_yields_no_diff() {
    let mut re = RealEstate::default();
    re.address = "x".into();
    re.financing_balance = "100".into();
    re.property_mgmt_password = "p".into();
    re.insurance_username = "u".into();
    re.hoa_url = "h".into();
    re.comments = "c".into();
    re.documents = vec!["d1".into(), "d2".into()];
    assert!(re.diff(&re.clone(), unix_now()).is_empty(), "no change -> empty diff");
}

/// RealEstate label: blank address -> placeholder; otherwise the address.
#[test]
fn real_estate_label_variants() {
    let mut re = RealEstate::default();
    assert_eq!(re.label(), "(no address)");
    re.address = "742 Evergreen Terrace".into();
    assert_eq!(re.label(), "742 Evergreen Terrace");
}

// --- Folder helpers: adversarial inputs (path-traversal hardening) -------

/// Internal invariant for any tax folder: exactly `taxes/<one-segment>`,
/// no `..`, no extra '/', and the segment is non-empty and ASCII-alnum.
fn assert_tax_folder_safe(input: &str) {
    let f = tax_doc_location(input);
    assert!(f.starts_with("taxes/"), "{input:?} -> {f:?} lost prefix");
    let seg = &f["taxes/".len()..];
    assert!(!seg.is_empty(), "{input:?} -> empty segment");
    assert!(!seg.contains('/'), "{input:?} -> {f:?} has nested slash");
    assert!(!f.contains(".."), "{input:?} -> {f:?} contains ..");
    assert!(!seg.contains('.'), "{input:?} -> {f:?} contains a dot");
    // Either the safe fallback, or pure ASCII-alphanumeric.
    assert!(seg == "unspecified" || seg.chars().all(|c| c.is_ascii_alphanumeric()), "{input:?} -> {f:?} not alnum");
}

/// Internal invariant for any real-estate folder: exactly
/// `real-estate/<one-segment>`, lowercased, <=40 chars, no traversal.
fn assert_re_folder_safe(input: &str) {
    let f = real_estate_doc_location(input);
    assert!(f.starts_with("real-estate/"), "{input:?} -> {f:?} lost prefix");
    let seg = &f["real-estate/".len()..];
    assert!(!seg.is_empty(), "{input:?} -> empty segment");
    assert!(!seg.contains('/'), "{input:?} -> {f:?} has nested slash");
    assert!(!f.contains(".."), "{input:?} -> {f:?} contains ..");
    assert!(!seg.contains('.'), "{input:?} -> {f:?} contains a dot");
    assert!(seg.len() <= 40, "{input:?} -> {f:?} segment >40 chars");
    assert_eq!(seg, seg.to_lowercase(), "{input:?} -> {f:?} not lowercased");
    assert!(seg == "property" || seg.chars().all(|c| c.is_ascii_alphanumeric()), "{input:?} -> {f:?} not alnum");
}

#[test]
fn tax_doc_location_is_always_safe() {
    let adversarial = [
        "",
        "   ",
        "\t\n  \t",
        "..",
        "../",
        "../../etc/passwd",
        "....//....//",
        "taxes/../secret",
        "/etc/shadow",
        "2024/../2025",
        "  ../2024/..  ",
        "C:\\Windows\\System32",
        "year\0null",
        "2024年",          // unicode suffix
        "二千二十四",        // all-unicode -> fallback
        "café",            // accented -> "caf"
        "\u{ff12}\u{ff10}\u{ff12}\u{ff14}", // full-width digits -> dropped -> fallback
        "FY-2024 #final!",
        "   2023/   ",
        &"9".repeat(100),  // very long
        &("a".repeat(60) + "/../../x"),
    ];
    for input in adversarial {
        assert_tax_folder_safe(input);
    }
    // Spot-check exact, documented outputs.
    assert_eq!(tax_doc_location("2024"), "taxes/2024");
    assert_eq!(tax_doc_location(" 2023/ "), "taxes/2023");
    assert_eq!(tax_doc_location("../../etc/passwd"), "taxes/etcpasswd");
    assert_eq!(tax_doc_location(""), "taxes/unspecified");
    assert_eq!(tax_doc_location("..."), "taxes/unspecified");
    // tax_doc_location preserves case (unlike real-estate).
    assert_eq!(tax_doc_location("FY2024"), "taxes/FY2024");
}

#[test]
fn real_estate_doc_location_is_always_safe() {
    let adversarial = [
        "",
        "   ",
        "\t\n",
        "..",
        "../",
        "../../etc/passwd",
        "....//....//",
        "real-estate/../secret",
        "/etc/shadow",
        "123 Main St/../../root",
        "  ../1 main/..  ",
        "C:\\Users\\victim",
        "addr\0null",
        "Champs-Élysées",  // accented chars dropped
        "東京タワー",        // all-unicode -> fallback
        "\u{ff11}\u{ff12}", // full-width digits -> dropped -> fallback
        "Unit #4B, Apt. 12!",
        &"A".repeat(100),                 // long -> truncated to 40
        &("X".repeat(50) + "/../../x"),  // long + traversal
    ];
    for input in adversarial {
        assert_re_folder_safe(input);
    }
    // Spot-check exact, documented outputs.
    assert_eq!(real_estate_doc_location("123 Main St"), "real-estate/123mainst");
    assert_eq!(real_estate_doc_location(""), "real-estate/property");
    assert_eq!(real_estate_doc_location("..."), "real-estate/property");
    assert_eq!(real_estate_doc_location("../../etc/passwd"), "real-estate/etcpasswd");
    // Truncation is to 40 alnum chars, then lowercased.
    let long = real_estate_doc_location(&"A".repeat(100));
    assert_eq!(long, format!("real-estate/{}", "a".repeat(40)));
}

/// Long inputs are truncated to 40 chars *of the sanitized form* — and
/// separators/junk between alnum runs don't count toward the 40.
#[test]
fn real_estate_doc_location_truncates_sanitized_length_not_raw() {
    // 30 'a', then lots of slashes/spaces, then 30 'b': only 40 alnum survive.
    let raw = format!("{}{}{}", "a".repeat(30), " / / / ".repeat(10), "b".repeat(30));
    let f = real_estate_doc_location(&raw);
    let seg = &f["real-estate/".len()..];
    assert_eq!(seg.len(), 40);
    assert_eq!(seg, format!("{}{}", "a".repeat(30), "b".repeat(10)));
}

// --- uniform document layout helpers (General Documents + new path scheme) ---

#[test]
fn doc_slug_is_safe_and_bounded() {
    assert_eq!(doc_slug("Federal 2024", "fb"), "federal-2024");
    assert_eq!(doc_slug("  My Docs!! ", "fb"), "my-docs");
    assert_eq!(doc_slug("a//b\\c", "fb"), "a-b-c");
    assert_eq!(doc_slug("../../etc/passwd", "fb"), "etc-passwd"); // no traversal survives
    assert_eq!(doc_slug("", "fb"), "fb"); // empty -> fallback
    assert_eq!(doc_slug("！！！", "fb"), "fb"); // all non-ascii -> fallback
    assert_eq!(doc_slug("---", "fb"), "fb"); // separators-only -> fallback
    // Length is capped at 40 with no trailing dash.
    let long = doc_slug(&"a ".repeat(60), "fb");
    assert!(long.len() <= 40 && !long.ends_with('-'));
}

#[test]
fn compact_utc_is_fixed_width_sortable() {
    // 2024-01-02 03:04:05 UTC = 1704164645.
    assert_eq!(compact_utc(1_704_164_645), "20240102-030405");
    assert_eq!(compact_utc(0), "19700101-000000");
    // Always 15 chars (YYYYMMDD-HHMMSS); lexical order == chronological order.
    assert_eq!(compact_utc(1_704_164_645).len(), 15);
    assert!(compact_utc(1_000) < compact_utc(2_000_000_000));
}

#[test]
fn doc_upload_dir_builds_the_uniform_layout() {
    // <prefix>[/<subfolder>] — the timestamp now lives in the filename, not a dir level.
    let prefix = tax_doc_location("2024"); // "taxes/2024"
    assert_eq!(doc_upload_dir(&prefix, "federal"), "taxes/2024/federal");
    // Blank subfolder is omitted entirely.
    assert_eq!(doc_upload_dir(&prefix, "   "), "taxes/2024");
    // Subfolder is slugged (no separators/traversal leak into the path).
    assert_eq!(doc_upload_dir("general-documents/passport", "../ids"), "general-documents/passport/ids");
}

#[test]
fn owner_initials_takes_first_letter_of_each_word() {
    assert_eq!(owner_initials("Jane Doe"), "JD");
    assert_eq!(owner_initials("michael kaissi"), "MK");
    assert_eq!(owner_initials("Michael and Sarah"), "MAS"); // no connector special-casing
    assert_eq!(owner_initials("Michael & Sarah"), "MS"); // '&' has no alphanumeric -> skipped
    assert_eq!(owner_initials("Joint"), "J");
    assert_eq!(owner_initials("  spaced   out  "), "SO");
    assert_eq!(owner_initials("(John) [Q] Public"), "JQP"); // first ALNUM of each word
    assert_eq!(owner_initials(""), ""); // blank -> empty (caller omits the level)
    assert_eq!(owner_initials("   "), "");
    assert_eq!(owner_initials("a b c d e f g h i j"), "ABCDEFGH"); // capped at 8
}

#[test]
fn owner_prefix_is_owner_first_and_omits_blank() {
    assert_eq!(owner_prefix(Some("Jane Doe"), "assets"), "JD/assets");
    assert_eq!(owner_prefix(Some("Jane Doe"), "taxes/2024"), "JD/taxes/2024");
    assert_eq!(owner_prefix(Some("  "), "assets"), "assets"); // blank owner -> no level
    assert_eq!(owner_prefix(None, "trust-will/living-trust"), "trust-will/living-trust");
}

#[test]
fn is_compact_utc_matches_only_the_exact_stamp() {
    assert!(is_compact_utc("20240102-030405"));
    assert!(is_compact_utc("19700101-000000"));
    assert!(!is_compact_utc("2024010-030405")); // too short
    assert!(!is_compact_utc("20240102_030405")); // wrong separator
    assert!(!is_compact_utc("2024010a-030405")); // non-digit before '-'
    assert!(!is_compact_utc("20240102-03040")); // one char short
    assert!(!is_compact_utc("20240102-030405x")); // trailing char (16 long)
}

#[test]
fn timestamped_filename_prefixes_with_underscore() {
    assert_eq!(timestamped_filename("20240102-030405", "return.pdf"), "20240102-030405_return.pdf");
    // Round-trips with is_compact_utc on the 15-char prefix (the migration's idempotency key).
    let f = timestamped_filename(&compact_utc(0), "x.pdf");
    assert!(is_compact_utc(&f[..15]) && f.as_bytes()[15] == b'_');
}

#[test]
fn doc_filename_is_user_controlled_but_safe() {
    assert_eq!(doc_filename("return.pdf"), "return.pdf"); // extension preserved
    assert_eq!(doc_filename("a/b/c.pdf"), "a_b_c.pdf"); // forward slashes neutralized
    assert_eq!(doc_filename("a\\b.pdf"), "a_b.pdf"); // BACKslashes too (no extra path level)
    assert_eq!(doc_filename("a\u{7}b.pdf"), "a_b.pdf"); // control chars (bell) neutralized
    assert_eq!(doc_filename("my report.pdf"), "my-report.pdf"); // spaces -> '-'
    assert_eq!(doc_filename("  spaced  name .pdf"), "spaced--name-.pdf"); // no spaces remain
    assert_eq!(doc_filename("tab\tname.pdf"), "tab-name.pdf"); // tabs are whitespace too
    assert!(!doc_filename("a b\tc\nd.pdf").contains(' '), "no whitespace survives");
    assert_eq!(doc_filename("  ..  "), "file"); // dot/space-only -> fallback
    assert_eq!(doc_filename(""), "file");
    assert!(doc_filename(&"x".repeat(500)).len() <= 120); // capped
    // A multibyte filename whose 120th byte lands mid-character must NOT panic
    // (a raw String::truncate(120) would), must stay within the cap, AND must
    // keep a real prefix (not collapse to the "file" fallback — which a broken
    // truncation loop that ran cut to 0 would produce). 5-byte ASCII prefix + 50
    // CJK chars (3 bytes each) = 155 bytes; the cap falls inside a character.
    let multibyte = doc_filename(&format!("file_{}", "\u{6570}".repeat(50)));
    assert!(multibyte.len() <= 120, "capped");
    assert!(multibyte.starts_with("file_"), "prefix preserved, not collapsed to fallback: {multibyte}");
    // Emoji (4-byte) near the boundary likewise truncates safely on a boundary.
    let emoji = doc_filename(&"\u{1F600}".repeat(40)); // 160 bytes
    assert!(emoji.len() <= 120 && !emoji.is_empty());
}

#[test]
fn doc_filename_neutralizes_windows_reserved_names() {
    // Reserved device-name stems are prefixed so the stored/exported file is a real file
    // on Windows, not the device. Case-insensitive; the extension does not save it.
    assert_eq!(doc_filename("con"), "_con");
    assert_eq!(doc_filename("CON.pdf"), "_CON.pdf");
    assert_eq!(doc_filename("nul"), "_nul");
    assert_eq!(doc_filename("com1.txt"), "_com1.txt");
    assert_eq!(doc_filename("LPT9"), "_LPT9");
    // Not reserved: a longer name, a non-1-9 digit, or the name merely containing them.
    assert_eq!(doc_filename("console.pdf"), "console.pdf");
    assert_eq!(doc_filename("com0.txt"), "com0.txt");
    assert_eq!(doc_filename("com10.txt"), "com10.txt");
    assert_eq!(doc_filename("report-con.pdf"), "report-con.pdf");
    assert!(is_windows_reserved_name("aux") && is_windows_reserved_name("PRN.doc"));
    assert!(!is_windows_reserved_name("auxiliary") && !is_windows_reserved_name("lpt"));
    // Regression (doc_paths fuzz): the reserved-name '_' prefix must not push a name that
    // was already at the 120-byte cap to 121. A long reserved-stem name stays bounded and
    // keeps the no-edge-dot + non-empty invariants.
    let long = doc_filename(&format!("con.{}", "a".repeat(200)));
    assert!(long.len() <= 120, "reserved+long stays capped: {} bytes", long.len());
    assert!(long.starts_with("_con") && !long.ends_with('.') && !long.is_empty());
}

#[test]
fn display_safe_neutralizes_control_and_bidi_chars() {
    // Replaces the RLO override, zero-width/BOM, and control chars with '_' while keeping
    // ordinary text (including non-ASCII letters) intact. Used by export_document_into for
    // a real on-disk name and by the merge preview for an untrusted source label.
    assert_eq!(display_safe("invoice\u{202e}fdp.exe"), "invoice_fdp.exe"); // RIGHT-TO-LEFT OVERRIDE
    assert_eq!(display_safe("a\u{200b}b\u{feff}c"), "a_b_c"); // zero-width space + BOM
    assert_eq!(display_safe("tab\tnl\n"), "tab_nl_"); // ASCII control
    // U+2028 LINE SEPARATOR / U+2029 PARAGRAPH SEPARATOR are real line breaks that
    // char::is_control() does NOT catch — they must still be neutralized so a CSV cell
    // (or a terminal preview / filename) cannot be split by an unquoted line break.
    assert_eq!(display_safe("line\u{2028}sep\u{2029}end"), "line_sep_end");
    assert_eq!(display_safe("José café 北京"), "José café 北京"); // ordinary unicode preserved
    assert_eq!(display_safe("plain.txt"), "plain.txt");
}

#[test]
fn display_safe_neutralizes_the_alm_bidi_control_and_every_invisible_form() {
    // Audit 2026-07-25 round 2. The set used to be an enumerated handful of ranges, which
    // left 154 Unicode `Cf` characters — including a BIDI CONTROL — flowing verbatim into
    // the merge preview the user authorizes, CSV cells, and real on-disk export filenames.

    // U+061C ARABIC LETTER MARK is the clearest hole: it is the ALM counterpart of LRM/RLM
    // (U+200E/U+200F, both already covered) and reorders the neutral characters around it —
    // digits and punctuation — with no override character needed. So a stored filename could
    // still be made to DISPLAY with its digits transposed.
    assert_eq!(display_safe("invoice\u{061C}2024.pdf"), "invoice_2024.pdf");
    assert_eq!(display_safe("\u{200E}a\u{200F}b\u{061C}c"), "_a_b_c", "all three marks alike");

    // Characters that draw NOTHING: two labels differing only by these are identical on
    // screen, so a crafted merge source could present a decoy that reads as a known record.
    assert_eq!(display_safe("Chase\u{00AD}Checking"), "Chase_Checking"); // SOFT HYPHEN
    assert_eq!(display_safe("a\u{2062}b\u{2064}c"), "a_b_c"); // INVISIBLE TIMES / PLUS
    assert_eq!(display_safe("a\u{180E}b"), "a_b"); // MONGOLIAN VOWEL SEPARATOR
    assert_eq!(display_safe("a\u{206A}b\u{206F}c"), "a_b_c"); // deprecated shaping controls
    assert_eq!(display_safe("a\u{FFF9}b\u{FFFB}c"), "a_b_c"); // interlinear annotation
    assert_eq!(display_safe("a\u{3164}b\u{FFA0}c\u{115F}d"), "a_b_c_d"); // blank fillers

    // The U+E0000 TAGS block is an entire invisible ASCII alphabet — the modern
    // "invisible text" smuggling vector. It draws no glyph at all.
    assert_eq!(display_safe("bank\u{E0041}\u{E0042}.pdf"), "bank__.pdf");
    assert_eq!(display_safe("x\u{E0001}y\u{E007F}z"), "x_y_z");

    // Deliberately NOT neutralized: format characters with a real visible rendering in a
    // living script, and the variation selectors a colour emoji needs. Mangling these would
    // corrupt honest labels for no security gain — they neither hide nor reorder.
    assert_eq!(display_safe("\u{0600}\u{06DD}"), "\u{0600}\u{06DD}", "Arabic number signs kept");
    assert_eq!(display_safe("note \u{1F600}\u{FE0F}"), "note \u{1F600}\u{FE0F}", "emoji VS kept");
    assert_eq!(display_safe("\u{1D173}"), "\u{1D173}", "musical beam mark kept");

    // The same set backs the real on-disk filename, so the repair reaches doc_filename too.
    assert_eq!(doc_filename("deed\u{061C}2024.pdf"), "deed_2024.pdf");
    assert_eq!(doc_filename("a\u{E0041}b"), "a_b");
}

#[test]
fn windows_reserved_names_cover_the_console_handles_and_superscript_com_forms() {
    // Audit 2026-07-25 round 2. `CONIN$`/`CONOUT$` are console device handles, and Windows'
    // path canonicalization folds the SUPERSCRIPT digit spellings onto COM1/2/3 — so all of
    // these resolve to a device, not a file, and must be repaired before they become a real
    // path component. Left unrepaired, an heir extracting on Windows gets an I/O error where
    // the document should be.
    assert!(is_windows_reserved_name("CONIN$") && is_windows_reserved_name("conout$.txt"));
    assert!(is_windows_reserved_name("COM\u{00B9}"), "COM¹ folds to COM1");
    assert!(is_windows_reserved_name("lpt\u{00B3}.pdf"), "lpt³ folds to LPT3");
    assert_eq!(doc_filename("CONIN$.txt"), "_CONIN$.txt");
    assert_eq!(doc_filename("com\u{00B2}.log"), "_com\u{00B2}.log", "renamed, digit kept as typed");
    // Still not reserved: a longer name that merely starts with the prefix, or `$` elsewhere.
    assert!(!is_windows_reserved_name("continue") && !is_windows_reserved_name("con$"));
    assert!(!is_windows_reserved_name("com\u{00B9}0"), "COM10 is a real name, not a device");
}

#[test]
fn parse_approx_value_handles_currency_commas_and_suffixes() {
    assert_eq!(parse_approx_value("1500"), Some(1500.0));
    assert_eq!(parse_approx_value(" $12,000.50 "), Some(12_000.50));
    assert_eq!(parse_approx_value("250k"), Some(250_000.0));
    assert_eq!(parse_approx_value("1.2M"), Some(1_200_000.0));
    assert_eq!(parse_approx_value("-2500"), Some(-2500.0));
    assert_eq!(parse_approx_value("€1 234"), Some(1234.0));
    // Not numeric → None.
    assert_eq!(parse_approx_value(""), None);
    assert_eq!(parse_approx_value("about 5"), None);
    assert_eq!(parse_approx_value("$"), None);
    assert_eq!(parse_approx_value("tbd"), None);
    // A finite mantissa that OVERFLOWS once scaled by the suffix must be rejected (not
    // Some(inf)) — else it passes save-time validation and poisons the Summary totals.
    assert_eq!(parse_approx_value("1e300t"), None);
    assert_eq!(parse_approx_value("1e308k"), None);
    assert_eq!(parse_approx_value("1e400"), None); // already inf before the suffix path
    assert!(parse_approx_value("9e11t").unwrap().is_finite(), "a large-but-finite scaled value still parses");
}

#[test]
fn value_bucket_classifies_real_estate_retirement_cash_and_other() {
    use ValueBucket::*;
    assert_eq!(value_bucket("Real Estate", "", false), RealEstate);
    assert_eq!(value_bucket("Rental Property", "", false), RealEstate);
    assert_eq!(value_bucket("401k", "Fidelity", false), BeforeTax);
    assert_eq!(value_bucket("Roth IRA", "Vanguard", false), BeforeTax);
    assert_eq!(value_bucket("HSA", "Health Equity", false), BeforeTax);
    assert_eq!(value_bucket("Brokerage", "Health Equity", false), BeforeTax); // institution alone
    // Cash = cash / savings / checking, segregated out of After Tax.
    assert_eq!(value_bucket("Savings", "Ally Bank", false), Cash);
    assert_eq!(value_bucket("Checking", "Chase", false), Cash);
    assert_eq!(value_bucket("Cash", "", false), Cash);
    assert_eq!(value_bucket("Money Market", "Schwab", false), Cash);
    // Precedence: a retirement keyword beats cash (a "Roth savings" is retirement, not cash).
    assert_eq!(value_bucket("Roth Savings", "Vanguard", false), BeforeTax);
    assert_eq!(value_bucket("Brokerage", "Schwab", false), AfterTax); // everything else
    // Liabilities never use the Real-Estate bucket (the summary doesn't tax-split them).
    assert_eq!(value_bucket("Real Estate", "", true), AfterTax);
    assert_eq!(value_bucket("Loan", "401k", true), BeforeTax);
}

#[test]
fn owner_value_summary_aggregates_by_owner_kind_and_bucket() {
    let mk = |owner: &str, kind: &str, ty: &str, inst: &str, val: &str| {
        let mut a = AssetLiability::new().unwrap();
        a.owner = owner.into();
        a.kind = kind.into();
        a.asset_type = ty.into();
        a.institution = inst.into();
        a.approx_value = val.into();
        a
    };
    let items = [
        mk("Alice", "Asset", "Real Estate", "", "500000"),
        mk("Alice", "Asset", "401k", "Fidelity", "200000"),
        mk("Alice", "Asset", "HSA", "Health Equity", "10000"),
        mk("Alice", "Asset", "Brokerage", "Schwab", "50000"),
        mk("Alice", "Asset", "Savings", "Ally Bank", "25000"), // cash/savings/checking -> Cash column
        mk("Alice", "Liability", "Mortgage", "", "300000"),
        // A liability whose keywords WOULD have matched the old "before tax" bucket — it must
        // now land in the single liability column, not a separate before-tax liability column.
        mk("Alice", "Liability", "401k Loan", "Fidelity", "5000"),
        mk("Bob", "Asset", "Brokerage", "", "not-a-number"), // unparseable → 0
        mk("", "Asset", "Cash", "", "1000"),                  // blank owner → "(no owner)"
    ];
    let rows = owner_value_summary(items.iter());
    let alice = rows.iter().find(|r| r.owner == "Alice").unwrap();
    assert_eq!(alice.asset_real_estate, 500_000.0);
    assert_eq!(alice.asset_cash, 25_000.0, "savings is segregated into the Cash column");
    assert_eq!(alice.asset_before_tax, 210_000.0, "401k + HSA");
    assert_eq!(alice.asset_after_tax, 50_000.0, "cash is NOT in after-tax");
    assert_eq!(alice.liability, 305_000.0, "all liabilities (mortgage + 401k loan) in one column");
    assert_eq!(alice.asset_total(), 785_000.0);
    assert_eq!(alice.liability_total(), 305_000.0);
    assert_eq!(alice.net(), 480_000.0);
    let bob = rows.iter().find(|r| r.owner == "Bob").unwrap();
    assert_eq!(bob.asset_after_tax, 0.0, "unparseable value contributes 0");
    assert!(rows.iter().any(|r| r.owner == "(no owner)"));
}

#[test]
fn asset_validation_requires_owner_and_numeric_value() {
    let mut a = AssetLiability::new().unwrap();
    a.owner = String::new();
    a.approx_value = "1000".into();
    assert!(asset_validation_error(&a).unwrap().contains("Owner"));
    a.owner = "Alice".into();
    a.approx_value = "lots".into();
    assert!(asset_validation_error(&a).unwrap().contains("number"));
    a.approx_value = "1000".into();
    assert!(asset_validation_error(&a).is_none());
}

#[test]
fn effective_doc_filename_falls_back_to_source_basename() {
    // A given filename wins (trimmed).
    assert_eq!(effective_doc_filename("report.pdf", "/home/u/anything.bin"), "report.pdf");
    assert_eq!(effective_doc_filename("  report.pdf  ", "/x/y.bin"), "report.pdf");
    // Empty/whitespace filename -> the source file's basename ("use the same filename").
    assert_eq!(effective_doc_filename("", "/home/u/Downloads/deed.pdf"), "deed.pdf");
    assert_eq!(effective_doc_filename("   ", "relative/w2.png"), "w2.png");
    assert_eq!(effective_doc_filename("", "bare.txt"), "bare.txt");
    // Degenerate source (no final component) -> empty, which callers reject.
    assert_eq!(effective_doc_filename("", "/"), "");
    assert_eq!(effective_doc_filename("", ""), "");
}

#[test]
fn general_document_diff_and_label() {
    let mut a = GeneralDocument::new().unwrap();
    a.title = "Passport".into();
    a.description = "scan".into();
    let mut b = a.clone();
    b.description = "scan v2".into();
    b.file = Some("deadbeef".into());
    let c = a.diff(&b, 100);
    assert!(c.iter().any(|x| x.detail.contains("description")));
    // The file id itself must never appear in the history detail.
    assert!(c.iter().any(|x| x.detail.contains("attached file changed")));
    assert!(!c.iter().any(|x| x.detail.contains("deadbeef")), "doc id must not leak into history");
    assert_eq!(a.label(), "Passport");
    assert_eq!(GeneralDocument::default().label(), "(untitled)");
    // The per-tab <root>/<auto-group> prefix helpers slug their identifying field,
    // with a stable fallback for blank input.
    assert_eq!(general_doc_location("My Passport"), "general-documents/my-passport");
    assert_eq!(general_doc_location(""), "general-documents/untitled");
    assert_eq!(trust_will_doc_location("Living Trust"), "trust-will/living-trust");
    assert_eq!(trust_will_doc_location(""), "trust-will/document");
    // Assets/Liabilities are kind-based with NO slugged auto-group: the root IS the kind.
    assert_eq!(asset_doc_location("Asset"), "assets");
    assert_eq!(asset_doc_location("Liability"), "liabilities");
    assert_eq!(asset_doc_location("liability"), "liabilities"); // case-insensitive
    assert_eq!(asset_doc_location(""), "assets"); // blank/unknown kind defaults to assets
}

#[test]
fn compact_history_includes_general_documents() {
    let mut vault = Vault::default();
    let mut g = GeneralDocument::default();
    g.history = vec![Change::new("created", String::new()), Change::new("updated", "title".into())];
    vault.general_documents.push(g);
    assert_eq!(history_stats(&vault, None, true), 2);
    assert_eq!(compact_history(&mut vault, None, true), 2);
    assert!(vault.general_documents[0].history.is_empty());
}

// --- compact_history / history_stats include tax_filings & real_estate ---

/// `compact_history` and `history_stats` both account for tax_filings under a
/// cutoff (not just drop_all), and agree with each other.
#[test]
fn compact_history_counts_tax_filings_under_cutoff() {
    let mut vault = Vault::default();
    let mut t = TaxFiling::default();
    t.history = vec![
        Change { at: 100, action: "u".into(), detail: String::new() },
        Change { at: 200, action: "u".into(), detail: String::new() },
        Change { at: 300, action: "u".into(), detail: String::new() },
    ];
    vault.tax_filings.push(t);
    // cutoff 250: at=100,200 are older (removed); at=300 kept.
    assert_eq!(history_stats(&vault, Some(250), false), 2);
    assert_eq!(compact_history(&mut vault, Some(250), false), 2);
    assert_eq!(vault.tax_filings[0].history.iter().map(|c| c.at).collect::<Vec<_>>(), vec![300]);
}

/// `compact_history`/`history_stats` count real-estate AND tax histories in
/// the same pass as the other record types, and the two functions agree.
#[test]
fn compact_history_spans_all_six_record_types() {
    let mut vault = Vault::default();
    let mk = |at| Change { at, action: "u".into(), detail: String::new() };
    let mut ins = Instruction::default();
    ins.history = vec![mk(1)];
    let mut tw = TrustWill::default();
    tw.history = vec![mk(1)];
    let mut al = AssetLiability::default();
    al.history = vec![mk(1)];
    let mut ac = Account::default();
    ac.history = vec![mk(1)];
    let mut re = RealEstate::default();
    re.history = vec![mk(1), mk(2)];
    let mut tx = TaxFiling::default();
    tx.history = vec![mk(1), mk(2), mk(3)];
    vault.instructions.push(ins);
    vault.trust_wills.push(tw);
    vault.assets.push(al);
    vault.accounts.push(ac);
    vault.real_estate.push(re);
    vault.tax_filings.push(tx);
    // 1+1+1+1+2+3 = 9
    assert_eq!(history_stats(&vault, None, true), 9);
    assert_eq!(compact_history(&mut vault, None, true), 9, "all six types trimmed");
    assert!(vault.real_estate[0].history.is_empty());
    assert!(vault.tax_filings[0].history.is_empty());
    // Idempotent: nothing left to remove.
    assert_eq!(compact_history(&mut vault, None, true), 0);
}

// --- upsert wiring for the two new record types --------------------------

/// `upsert` works end-to-end for TaxFiling: insert logs "created", and a
/// subsequent edit appends the field diff while keeping id + creation time.
#[test]
fn upsert_taxfiling_insert_then_edit() {
    let mut list: Vec<TaxFiling> = Vec::new();
    let mut t = TaxFiling::new().unwrap();
    t.year = "2024".into();
    let id = t.id.clone();
    let created = t.created_at;
    upsert(&mut list, t);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].history.len(), 1);
    assert_eq!(list[0].history[0].action, "created");
    assert!(list[0].history[0].detail.contains("Taxes 2024"));

    let mut edit = list[0].clone();
    edit.notes = "amended".into();
    edit.documents.push("blob".into());
    upsert(&mut list, edit);
    assert_eq!(list.len(), 1, "same id replaces");
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].created_at, created, "creation time preserved");
    assert!(list[0].history.iter().any(|c| c.detail.contains("notes")));
    assert!(list[0].history.iter().any(|c| c.detail.contains("documents") && c.detail.contains("0 -> 1")));
}

/// `upsert` for RealEstate preserves creation time and appends a portal diff.
#[test]
fn upsert_real_estate_insert_then_edit() {
    let mut list: Vec<RealEstate> = Vec::new();
    let mut re = RealEstate::new().unwrap();
    re.address = "9 Pine".into();
    let id = re.id.clone();
    let created = re.created_at;
    upsert(&mut list, re);
    assert_eq!(list[0].history.len(), 1);
    assert_eq!(list[0].history[0].action, "created");

    let mut edit = list[0].clone();
    edit.hoa_password = "rotated".into();
    upsert(&mut list, edit);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].created_at, created);
    assert!(list[0].history.iter().any(|c| c.detail.contains("hoa_password") && c.detail.contains("rotated")));
}

/// `remove` logs a deletion using the RealEstate/TaxFiling labels.
#[test]
fn remove_logs_real_estate_and_tax_labels() {
    let mut re_list: Vec<RealEstate> = Vec::new();
    let mut re = RealEstate::new().unwrap();
    re.address = "Lot 7".into();
    let re_id = re.id.clone();
    upsert(&mut re_list, re);
    let mut audit = Vec::new();
    assert!(remove(&mut re_list, &re_id, &mut audit, "RealEstate"));
    assert!(audit.iter().any(|c| c.action == "deleted" && c.detail.contains("Lot 7")));

    let mut tx_list: Vec<TaxFiling> = Vec::new();
    let mut tx = TaxFiling::new().unwrap();
    tx.year = "2030".into();
    let tx_id = tx.id.clone();
    upsert(&mut tx_list, tx);
    assert!(remove(&mut tx_list, &tx_id, &mut audit, "TaxFiling"));
    assert!(audit.iter().any(|c| c.action == "deleted" && c.detail.contains("Taxes 2030")));
}

// --- ZeroizeOnDrop coverage of the new secret-bearing fields -------------

/// The expanded RealEstate's new portal passwords / comments / documents are
/// covered by the derived `Zeroize` (no `#[zeroize(skip)]`), so they are
/// wiped on drop. We call `zeroize()` directly (drop calls the same impl).
#[test]
fn real_estate_zeroize_wipes_new_secret_fields() {
    let mut re = RealEstate::default();
    re.property_mgmt_password = "pm-secret".into();
    re.insurance_password = "ins-secret".into();
    re.hoa_password = "hoa-secret".into();
    re.property_mgmt_username = "user".into();
    re.comments = "private note".into();
    re.documents = vec!["blobA".into(), "blobB".into()];
    Zeroize::zeroize(&mut re);
    assert!(re.property_mgmt_password.is_empty());
    assert!(re.insurance_password.is_empty());
    assert!(re.hoa_password.is_empty());
    assert!(re.property_mgmt_username.is_empty());
    assert!(re.comments.is_empty());
    assert!(re.documents.is_empty(), "document id list must be wiped");
}

/// TaxFiling notes + document id list are wiped by the derived `Zeroize`.
#[test]
fn tax_filing_zeroize_wipes_fields() {
    let mut t = TaxFiling::default();
    t.year = "2024".into();
    t.notes = "sensitive".into();
    t.documents = vec!["doc1".into(), "doc2".into()];
    Zeroize::zeroize(&mut t);
    assert!(t.year.is_empty());
    assert!(t.notes.is_empty());
    assert!(t.documents.is_empty());
}

/// Comprehensive guard: EVERY record type derives both `Default` and `Zeroize`,
/// and `zeroize()` resets each field to its default — so a fully-populated record
/// must serialize identically to `T::default()` afterwards. Comparing the whole
/// serialized value (rather than hand-listing fields) makes this AUTO-COVER any
/// field added later: a new secret field that is not wiped — or that a future
/// contributor marks `#[zeroize(skip)]` to silence a trait-bound error (e.g. on
/// `Account.password`) — leaves a non-default value and fails here, instead of
/// silently stranding a plaintext secret in freed heap.
#[test]
fn every_record_type_is_fully_wiped_by_zeroize() {
    fn assert_wiped<T: serde::Serialize + Zeroize + Default>(mut full: T, name: &str) {
        full.zeroize();
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            serde_json::to_value(T::default()).unwrap(),
            "{name}: a field survived zeroize() — a secret may be stranded in freed memory"
        );
    }
    let s = || "SENTINEL".to_string();
    let hist = || vec![Change::new("updated", "password: \"old\" -> \"new\"".into())];

    let mut ins = Instruction::default();
    ins.id = s(); ins.title = s(); ins.description = s();
    ins.created_at = 7; ins.updated_at = 9; ins.history = hist();
    assert_wiped(ins, "Instruction");

    let mut tw = TrustWill::default();
    tw.id = s(); tw.document = s(); tw.usage = s(); tw.file = Some(s());
    tw.created_at = 7; tw.updated_at = 9; tw.history = hist();
    assert_wiped(tw, "TrustWill");

    let mut al = AssetLiability::default();
    al.id = s(); al.kind = s(); al.description = s(); al.owner = s(); al.title = s();
    al.approx_value = s(); al.as_of_date = s(); al.institution = s(); al.asset_type = s();
    al.url = s(); al.beneficiary = s(); al.review = true; al.statement = Some(s());
    al.created_at = 7; al.updated_at = 9; al.history = hist();
    assert_wiped(al, "AssetLiability");

    let mut acc = Account::default();
    acc.id = s(); acc.title = s(); acc.account_type = s(); acc.account_subtype = s();
    acc.owner = s(); acc.username = s(); acc.password = s(); acc.description = s();
    acc.url = s(); acc.closed_as_of = s(); acc.review = true;
    acc.created_at = 7; acc.updated_at = 9; acc.history = hist();
    assert_wiped(acc, "Account");

    let mut re = RealEstate::default();
    re.id = s(); re.address = s(); re.owner = s(); re.taxes = s(); re.hoa = s();
    re.income_account = s(); re.financing_account = s(); re.payment_account = s();
    re.financing_balance = s();
    re.property_mgmt_url = s(); re.property_mgmt_username = s();
    re.property_mgmt_password = s(); re.property_mgmt_comment = s();
    re.insurance_url = s(); re.insurance_username = s();
    re.insurance_password = s(); re.insurance_comment = s();
    re.hoa_url = s(); re.hoa_username = s(); re.hoa_password = s(); re.hoa_comment = s();
    re.tax_portal_url = s(); re.tax_portal_username = s();
    re.tax_portal_password = s(); re.tax_portal_comment = s();
    re.comments = s(); re.documents = vec![s(), s()];
    re.created_at = 7; re.updated_at = 9; re.history = hist();
    assert_wiped(re, "RealEstate");

    let mut tax = TaxFiling::default();
    tax.id = s(); tax.owner = s(); tax.year = s(); tax.notes = s(); tax.documents = vec![s()];
    tax.created_at = 7; tax.updated_at = 9; tax.history = hist();
    assert_wiped(tax, "TaxFiling");

    let mut gd = GeneralDocument::default();
    gd.id = s(); gd.title = s(); gd.description = s(); gd.file = Some(s());
    gd.created_at = 7; gd.updated_at = 9; gd.history = hist();
    assert_wiped(gd, "GeneralDocument");
}

use proptest::prelude::*;
proptest! {
    /// `upsert` is append-only on a record's history: editing the SAME id over and
    /// over keeps the "created" entry first, never shrinks the history, keeps
    /// `created_at` constant, and never moves `updated_at` backwards. The metamorphic
    /// suite never re-edits one id, so a regression clobbering prior history (or the
    /// creation time) would slip past it — this pins the audit-trail integrity.
    #[test]
    fn prop_upsert_history_is_append_only(
        edits in proptest::collection::vec(("[a-z ]{0,8}", "[a-z0-9]{0,8}"), 0..12)
    ) {
        let mut list: Vec<Account> = Vec::new();
        let mut a = Account::new().unwrap();
        a.id = "fixed-id".into();
        upsert(&mut list, a);
        let created_at = list[0].created_at;
        prop_assert_eq!(list.len(), 1);
        prop_assert_eq!(list[0].history[0].action.as_str(), "created");
        let mut prev_hist = list[0].history.len();
        let mut prev_updated = list[0].updated_at;
        for (user, pass) in edits {
            let mut e = list[0].clone();
            e.username = user;
            e.password = pass;
            upsert(&mut list, e);
            prop_assert_eq!(list.len(), 1, "upsert of the same id never adds a row");
            prop_assert_eq!(list[0].created_at, created_at, "created_at is immutable");
            prop_assert_eq!(list[0].history[0].action.as_str(), "created", "created entry stays first");
            prop_assert!(list[0].history.len() >= prev_hist, "history never shrinks");
            prop_assert!(list[0].updated_at >= prev_updated, "updated_at is monotonic");
            prev_hist = list[0].history.len();
            prev_updated = list[0].updated_at;
        }
    }

    /// `civil_from_unix` and `unix_from_civil` are exact inverses across the whole
    /// post-epoch range the app uses — a single off-by-one in the calendar math
    /// would break this.
    #[test]
    fn prop_civil_unix_roundtrip(ts in 0i64..=253_402_300_799i64) {
        let (y, mo, d, h, mi, s) = civil_from_unix(ts);
        prop_assert_eq!(unix_from_civil(y, mo, d, h, mi, s), ts);
    }

    /// `parse_ymd_utc` never panics on arbitrary input (returns None or Some).
    #[test]
    fn prop_parse_ymd_never_panics(s in ".*") {
        let _ = parse_ymd_utc(&s);
    }

    /// For valid `YYYY-MM-DD` dates, `parse_ymd_utc` is strictly monotonic in the
    /// calendar date, and a valid date round-trips through `civil_from_unix`.
    /// (`d in 1..=28` keeps every (y,m,d) a real date, so both parses are `Some`.)
    #[test]
    fn prop_parse_ymd_monotonic_and_roundtrips(
        y1 in 1970..=9999i64, m1 in 1..=12i64, d1 in 1..=28i64,
        y2 in 1970..=9999i64, m2 in 1..=12i64, d2 in 1..=28i64,
    ) {
        let a = format!("{y1:04}-{m1:02}-{d1:02}");
        let b = format!("{y2:04}-{m2:02}-{d2:02}");
        let ta = parse_ymd_utc(&a).expect("valid date a");
        let tb = parse_ymd_utc(&b).expect("valid date b");
        prop_assert_eq!(ta.cmp(&tb), (y1, m1, d1).cmp(&(y2, m2, d2)));
        let (cy, cmo, cd, ..) = civil_from_unix(ta);
        prop_assert_eq!((cy, cmo, cd), (y1, m1, d1));
    }

    /// doc_slug yields a safe single path component for ANY input: ASCII
    /// [a-z0-9-] only, no edge dash, <=40, never empty.
    #[test]
    fn prop_doc_slug_is_safe(s in ".*") {
        let slug = doc_slug(&s, "fb");
        prop_assert!(!slug.is_empty());
        prop_assert!(slug.len() <= 40);
        prop_assert!(!slug.starts_with('-') && !slug.ends_with('-'));
        prop_assert!(slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    /// doc_filename never yields a path separator, control char, whitespace, an
    /// edge dot, an empty name, or an over-long one — for ANY input.
    #[test]
    fn prop_doc_filename_is_safe(s in ".*") {
        let f = doc_filename(&s);
        prop_assert!(!f.is_empty());
        prop_assert!(f.len() <= 120);
        prop_assert!(!f.chars().any(|c| c == '/' || c == '\\' || c.is_control() || c.is_whitespace()));
        prop_assert!(!f.starts_with('.') && !f.ends_with('.'));
    }

    /// doc_upload_dir keeps the trusted prefix and never introduces a space, traversal
    /// segment, or empty component — for ANY user subfolder.
    #[test]
    fn prop_doc_upload_dir_is_safe(sub in ".*") {
        let dir = doc_upload_dir("taxes/2024", &sub);
        prop_assert!(dir.starts_with("taxes/2024"));
        prop_assert!(!dir.contains(' '));
        prop_assert!(!dir.contains("/../") && !dir.contains("/./") && !dir.ends_with("/.."));
        prop_assert!(dir.split('/').all(|c| !c.is_empty()));
    }
}


// --- mutation-testing kill-tests (round 7: cargo-mutants survivor closure) ---
#[test]
fn mut_acct_match_subtype_and_title_filters_are_exact() {
    let mk = |t: &str, st: &str, o: &str, ti: &str| {
        let mut a = Account::new().unwrap();
        a.account_type = t.into();
        a.account_subtype = st.into();
        a.owner = o.into();
        a.title = ti.into();
        a
    };
    let accts = vec![mk("Financial", "IRA", "Alice", "Retire"), mk("Financial", "Bank", "Bob", "Checking")];

    // subtype=IRA constrains the owners facet to ONLY the IRA account's owner.
    // Kills line-76 `||`->`&&` (which would yield []) and `==`->`!=` (which would yield ["Bob"]).
    let by_st = account_facets(&accts, "", "IRA", "", "", "", false);
    assert_eq!(by_st.owners, vec!["Alice"], "subtype=IRA keeps only the IRA account's owner");

    // title=Retire constrains owners to that one account. Kills line-78 `==`->`!=`
    // (which would yield ["Bob"]).
    let by_ti = account_facets(&accts, "", "", "", "Retire", "", false);
    assert_eq!(by_ti.owners, vec!["Alice"], "title=Retire keeps only that account's owner");

    // A subtype value present on no account leaves the cross-filtered facet empty,
    // pinning the exact-match semantics (an inverted `==` would surface everything).
    let none = account_facets(&accts, "", "Brokerage", "", "", "", false);
    assert!(none.owners.is_empty(), "an unmatched subtype leaves no owners");
}

#[test]
fn mut_history_stats_cutoff_is_strictly_older() {
    // Entry exactly AT the cutoff must be KEPT (not counted as removable): the
    // predicate is `at < cutoff`. If `<` became `<=`, the at=1000 entry would also
    // be counted and the total would be 2.
    let mut vault = Vault::default();
    let mut a = Account::default();
    a.history = vec![
        Change { at: 999, action: "u".into(), detail: String::new() },
        Change { at: 1000, action: "u".into(), detail: String::new() },
        Change { at: 1001, action: "u".into(), detail: String::new() },
    ];
    vault.accounts.push(a);
    assert_eq!(
        history_stats(&vault, Some(1000), false),
        1,
        "only at < cutoff is removable; at == cutoff is kept (would be 2 if `<` became `<=`)"
    );
}

#[test]
fn mut_parse_ymd_utc_year_out_of_range_rejected_at_guard() {
    // A year beyond the 1970..=9999 guard, with a valid month/day that DOES
    // round-trip through the civil math, so ONLY the range guard rejects it. If the
    // first `||` (between the year and month checks) became `&&`, this would parse.
    assert_eq!(parse_ymd_utc("10000-01-01"), None, "year > 9999 rejected by the range guard");
    // The in-range upper bound is still accepted (pins the guard's other side).
    assert!(parse_ymd_utc("9999-12-31").is_some(), "the in-range upper bound is accepted");
}

#[test]
fn mut_doc_filename_boundary_at_120_bytes() {
    // Boundary documentation around the 120-byte cap (see notes: the `>`->`>=`
    // mutants here are equivalent — output is byte-identical either way).
    let exact = doc_filename(&"x".repeat(120));
    assert_eq!(exact.len(), 120, "exactly 120 bytes is kept whole");
    let over = doc_filename(&"x".repeat(121));
    assert_eq!(over.len(), 120, "121 bytes is capped to 120");
    // A multibyte name whose 120th byte lands mid-character truncates on a char
    // boundary (never panics) and keeps a real prefix rather than collapsing.
    let multibyte = doc_filename(&format!("file_{}", "\u{6570}".repeat(50)));
    assert!(multibyte.len() <= 120 && multibyte.is_char_boundary(multibyte.len()));
    assert!(multibyte.starts_with("file_"));
}
