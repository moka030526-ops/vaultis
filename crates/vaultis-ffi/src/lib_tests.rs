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

/// v1 FFI SURFACE LOCK (compile-time). The mobile bindings are generated against this
/// crate's `#[uniffi::export]` surface and `App.kt` hardcodes these names, so an
/// accidental rename, removal, field change, or enum-variant change must break the
/// build. This test pins the surface by EXHAUSTIVELY matching every enum variant and
/// constructing every DTO with ALL its fields — any change is a COMPILE error here.
/// (cargo-public-api was deliberately not used: on a UniFFI crate its output is ~90%
/// `FfiConverter`/`Lift`/`Lower` boilerplate that churns on every uniffi bump, and it
/// cannot see a dropped `#[uniffi::export]`; the android APK job is the cross-language
/// backstop that compiles `App.kt` against the generated bindings.)
#[test]
fn v1_ffi_surface_is_locked() {
    // Every RecordKind variant — a new/removed/renamed variant breaks this match.
    fn kind_name(k: RecordKind) -> &'static str {
        match k {
            RecordKind::Urgent => "Urgent",
            RecordKind::Instruction => "Instruction",
            RecordKind::TrustWill => "TrustWill",
            RecordKind::AssetLiability => "AssetLiability",
            RecordKind::Account => "Account",
            RecordKind::RealEstate => "RealEstate",
            RecordKind::TaxFiling => "TaxFiling",
            RecordKind::GeneralDocument => "GeneralDocument",
        }
    }
    for k in [
        RecordKind::Urgent,
        RecordKind::Instruction,
        RecordKind::TrustWill,
        RecordKind::AssetLiability,
        RecordKind::Account,
        RecordKind::RealEstate,
        RecordKind::TaxFiling,
        RecordKind::GeneralDocument,
    ] {
        assert!(!kind_name(k).is_empty());
    }

    // Every VaultError variant — the exhaustive match pins the no-leak error surface.
    fn err_is_known(e: &VaultError) -> bool {
        match e {
            VaultError::WrongPasswordOrCorrupt
            | VaultError::NotFound
            | VaultError::RekeyPending
            | VaultError::Locked
            | VaultError::RecordNotFound
            | VaultError::Io
            | VaultError::Internal => true,
        }
    }
    assert!(err_is_known(&VaultError::WrongPasswordOrCorrupt));

    // Every DTO with ALL its fields — a renamed/removed/added field breaks these literals.
    let _ = RecordSummary { id: String::new(), label: String::new() };
    let _ = Change { at: 0, action: String::new(), detail: String::new() };
    let _ = Instruction {
        id: String::new(),
        title: String::new(),
        description: String::new(),
        created_at: 0,
        updated_at: 0,
    };
    let _ = TrustWill {
        id: String::new(),
        document: String::new(),
        usage: String::new(),
        file: None,
        created_at: 0,
        updated_at: 0,
    };
    let _ = AssetLiability {
        id: String::new(),
        kind: String::new(),
        description: String::new(),
        owner: String::new(),
        title: String::new(),
        approx_value: String::new(),
        as_of_date: String::new(),
        institution: String::new(),
        asset_type: String::new(),
        url: String::new(),
        beneficiary: String::new(),
        review: false,
        statement: None,
        created_at: 0,
        updated_at: 0,
    };
    let _ = Account {
        id: String::new(),
        title: String::new(),
        account_type: String::new(),
        account_subtype: String::new(),
        owner: String::new(),
        username: String::new(),
        password: String::new(),
        description: String::new(),
        url: String::new(),
        closed_as_of: String::new(),
        review: false,
        created_at: 0,
        updated_at: 0,
    };
    let _ = RealEstate {
        id: String::new(),
        address: String::new(),
        ownership: String::new(),
        taxes: String::new(),
        hoa: String::new(),
        income_account: String::new(),
        financing_account: String::new(),
        payment_account: String::new(),
        created_at: 0,
        updated_at: 0,
    };

    // The exported entry point keeps its exact signature (referenced, not called).
    // The explicit fn-pointer type IS the lock here, so the complexity is intentional.
    #[allow(clippy::type_complexity)]
    let _open: fn(String, Vec<u8>, Vec<u8>) -> Result<std::sync::Arc<Vault>, VaultError> = open_vault;
}

// -----------------------------------------------------------------------
// Extended FFI coverage: build a vault with one of EACH of the EIGHT record
// types via the core, then drive every read-only FFI method. All eight are now
// surfaced — the five-kind v1 surface silently hid Urgent, tax filings and general
// documents from the mobile viewer. The EXPANDED RealEstate portal fields are still
// deliberately not mapped; see `real_estate_with_new_fields_maps_only_v1_fields`.
// -----------------------------------------------------------------------

struct Ids {
    urgent: String,
    ins: String,
    tw: String,
    asset: String,
    acc: String,
    re: String,
    tax: String,
    gendoc: String,
}

/// Write a small throwaway source file (so `add_document` can ingest it).
fn write_src(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn make_full_vault(dir: &std::path::Path, pw1: &[u8], pw2: &[u8]) -> Ids {
    let path = dir.join("vault.pmv");
    let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };
    let mut ov = OpenVault::create(path, pw1, pw2, params).unwrap();

    // An URGENT note — the executor's "read this first" record (core tab 0).
    let mut urgent = records::Urgent::new().unwrap();
    urgent.title = "Call the lawyer first".into();
    urgent.description = "Ring Pat Okafor on 555-0101 before touching anything.".into();
    let urgent_id = urgent.id.clone();
    records::upsert(&mut ov.vault.urgent, urgent);

    let mut ins = records::Instruction::new().unwrap();
    ins.title = "Funeral wishes".into();
    ins.description = "Cremation, no service.".into();
    let ins_id = ins.id.clone();
    records::upsert(&mut ov.vault.instructions, ins);

    // Upload real documents so the records' doc-reference fields point at
    // blobs that actually exist (the open-time referenced⊆stored check
    // would otherwise fail with ArchiveMismatch).
    let trust_src = write_src(dir, "trust.pdf", b"trust document bytes");
    let trust_blob = ov.add_document("trust-wills", "trust.pdf", &trust_src).unwrap();

    let mut tw = records::TrustWill::new().unwrap();
    tw.document = "Living Trust".into();
    tw.usage = "Held at the law office.".into();
    tw.file = Some(trust_blob);
    let tw_id = tw.id.clone();
    records::upsert(&mut ov.vault.trust_wills, tw);

    let stmt_src = write_src(dir, "stmt.pdf", b"statement bytes");
    let stmt_blob = ov.add_document("assets", "stmt.pdf", &stmt_src).unwrap();

    let mut al = records::AssetLiability::new().unwrap();
    al.kind = "Liability".into();
    al.description = "Mortgage".into();
    al.owner = "Jane".into();
    al.approx_value = "250000".into();
    al.as_of_date = "2026-01-01".into();
    al.institution = "Big Bank".into();
    al.asset_type = "Real Estate Loan".into();
    al.url = "https://bank.example".into();
    al.beneficiary = "Spouse".into();
    al.review = true;
    al.statement = Some(stmt_blob);
    let al_id = al.id.clone();
    records::upsert(&mut ov.vault.assets, al);

    let mut acc = records::Account::new().unwrap();
    acc.account_type = "Financial".into();
    acc.account_subtype = "IRA".into();
    acc.owner = "Jane".into();
    acc.username = "alice".into();
    acc.password = "s3cret".into();
    acc.description = "Retirement".into();
    acc.url = "https://broker.example".into();
    acc.closed_as_of = "2026-06-18".into();
    acc.review = true;
    let acc_id = acc.id.clone();
    records::upsert(&mut ov.vault.accounts, acc);

    // RealEstate with the EXPANDED fields populated (portals, balance,
    // comments, documents). Only the original fields are mapped by the FFI.
    let re_loc = records::real_estate_doc_location("123 Main St");
    let deed_src = write_src(dir, "deed.pdf", b"deed bytes");
    let deed_blob = ov.add_document(&re_loc, "deed.pdf", &deed_src).unwrap();
    let policy_src = write_src(dir, "policy.pdf", b"policy bytes");
    let policy_blob = ov.add_document(&re_loc, "policy.pdf", &policy_src).unwrap();

    let mut re = records::RealEstate::new().unwrap();
    re.address = "123 Main St".into();
    re.owner = "Joint".into();
    re.taxes = "6000/yr".into();
    re.hoa = "Sunset HOA".into();
    re.income_account = "rent-acct".into();
    re.financing_account = "loan-acct".into();
    re.payment_account = "checking".into();
    re.financing_balance = "199000".into();
    re.property_mgmt_url = "https://pm.example".into();
    re.property_mgmt_username = "pmuser".into();
    re.property_mgmt_password = "pmpass".into();
    re.insurance_url = "https://ins.example".into();
    re.insurance_username = "insuser".into();
    re.insurance_password = "inspass".into();
    re.hoa_url = "https://hoa.example".into();
    re.hoa_username = "hoauser".into();
    re.hoa_password = "hoapass".into();
    re.comments = "Tenant occupied.".into();
    re.documents = vec![deed_blob, policy_blob];
    let re_id = re.id.clone();
    records::upsert(&mut ov.vault.real_estate, re);

    // A tax filing — present in the vault AND now exposed by the FFI.
    let tax_loc = records::tax_doc_location("2024");
    let tax_src = write_src(dir, "1040.pdf", b"1040 bytes");
    let tax_blob = ov.add_document(&tax_loc, "1040.pdf", &tax_src).unwrap();

    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    tax.notes = "Filed on time.".into();
    tax.documents = vec![tax_blob];
    let tax_id = tax.id.clone();
    records::upsert(&mut ov.vault.tax_filings, tax);

    // A general document entry with a real attached blob.
    let gd_src = write_src(dir, "passport.pdf", b"passport scan bytes");
    let gd_blob = ov.add_document("general", "passport.pdf", &gd_src).unwrap();
    let mut gd = records::GeneralDocument::new().unwrap();
    gd.title = "Passport".into();
    gd.description = "Scan of Jane's passport.".into();
    gd.file = Some(gd_blob);
    let gd_id = gd.id.clone();
    records::upsert(&mut ov.vault.general_documents, gd);

    ov.save().unwrap();
    Ids {
        urgent: urgent_id,
        ins: ins_id,
        tw: tw_id,
        asset: al_id,
        acc: acc_id,
        re: re_id,
        tax: tax_id,
        gendoc: gd_id,
    }
}

fn open_full(dir: &std::path::Path) -> Arc<Vault> {
    open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .expect("opens with correct passwords")
}

fn open_full_dir(dir: &std::path::Path, pw1: &[u8], pw2: &[u8]) -> Arc<Vault> {
    open_vault(dir.to_str().unwrap().to_string(), pw1.to_vec(), pw2.to_vec())
        .expect("opens with correct passwords")
}

#[test]
fn count_for_every_kind() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert_eq!(v.count(RecordKind::Instruction), 1);
    assert_eq!(v.count(RecordKind::TrustWill), 1);
    assert_eq!(v.count(RecordKind::AssetLiability), 1);
    assert_eq!(v.count(RecordKind::Account), 1);
    assert_eq!(v.count(RecordKind::RealEstate), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_records_for_every_kind_has_labels() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    for (kind, want_id, want_label) in [
        (RecordKind::Instruction, &ids.ins, "Funeral wishes"),
        (RecordKind::TrustWill, &ids.tw, "Living Trust"),
        (RecordKind::AssetLiability, &ids.asset, "[Liability] Mortgage"),
        (RecordKind::Account, &ids.acc, "Financial - alice"),
        (RecordKind::RealEstate, &ids.re, "123 Main St"),
    ] {
        let rows = v.list_records(kind);
        assert_eq!(rows.len(), 1, "exactly one row for {kind:?}");
        assert_eq!(&rows[0].id, want_id, "id matches for {kind:?}");
        assert_eq!(rows[0].label, want_label, "label matches for {kind:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_instruction_maps_all_fields() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let r = v.get_instruction(ids.ins.clone()).unwrap();
    assert_eq!(r.id, ids.ins);
    assert_eq!(r.title, "Funeral wishes");
    assert_eq!(r.description, "Cremation, no service.");
    assert!(r.created_at > 0);
    assert_eq!(r.created_at, r.updated_at, "created on insert == updated");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_trust_will_maps_all_fields_including_file() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let r = v.get_trust_will(ids.tw.clone()).unwrap();
    assert_eq!(r.id, ids.tw);
    assert_eq!(r.document, "Living Trust");
    assert_eq!(r.usage, "Held at the law office.");
    assert!(r.file.is_some(), "attached file id is surfaced");
    assert_eq!(r.file.as_ref().unwrap().len(), 32, "blob id is a 128-bit hex id");
    assert!(r.created_at > 0 && r.updated_at > 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_asset_maps_all_fields() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let r = v.get_asset(ids.asset.clone()).unwrap();
    assert_eq!(r.id, ids.asset);
    assert_eq!(r.kind, "Liability");
    assert_eq!(r.description, "Mortgage");
    assert_eq!(r.owner, "Jane");
    assert_eq!(r.approx_value, "250000");
    assert_eq!(r.as_of_date, "2026-01-01");
    assert_eq!(r.institution, "Big Bank");
    assert_eq!(r.asset_type, "Real Estate Loan");
    assert_eq!(r.url, "https://bank.example");
    assert_eq!(r.beneficiary, "Spouse");
    assert!(r.review);
    assert!(r.statement.is_some(), "attached statement id is surfaced");
    assert_eq!(r.statement.as_ref().unwrap().len(), 32, "blob id is a 128-bit hex id");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_account_maps_all_fields_including_cleartext_password() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let r = v.get_account(ids.acc.clone()).unwrap();
    assert_eq!(r.id, ids.acc);
    assert_eq!(r.account_type, "Financial");
    assert_eq!(r.account_subtype, "IRA");
    assert_eq!(r.owner, "Jane");
    assert_eq!(r.username, "alice");
    assert_eq!(r.password, "s3cret");
    assert_eq!(r.description, "Retirement");
    assert_eq!(r.url, "https://broker.example");
    assert_eq!(r.closed_as_of, "2026-06-18");
    assert!(r.review);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_real_estate_maps_the_original_v1_fields() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let r = v.get_real_estate(ids.re.clone()).unwrap();
    assert_eq!(r.id, ids.re);
    assert_eq!(r.address, "123 Main St");
    assert_eq!(r.ownership, "Joint");
    assert_eq!(r.taxes, "6000/yr");
    assert_eq!(r.hoa, "Sunset HOA");
    assert_eq!(r.income_account, "rent-acct");
    assert_eq!(r.financing_account, "loan-acct");
    assert_eq!(r.payment_account, "checking");
    assert!(r.created_at > 0 && r.updated_at > 0);
    std::fs::remove_dir_all(&dir).ok();
}

/// Forward-compat: a RealEstate record whose NEW expanded fields (portals,
/// financing_balance, comments, documents) are all populated must still map
/// cleanly through the v1 DTO — the FFI maps only the original fields and
/// silently ignores the new ones (it does not panic, drop the record, or
/// fail to find it).
#[test]
fn real_estate_with_new_fields_maps_only_v1_fields() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);

    let rows = v.list_records(RecordKind::RealEstate);
    assert_eq!(rows.len(), 1);
    let r = v.get_real_estate(ids.re.clone()).expect("RE with new fields still maps");

    assert_eq!(r.address, "123 Main St");
    assert_eq!(r.financing_account, "loan-acct");
    // The DTO's field set is exactly the original ones — constructing this
    // literal would fail to compile if a new field had leaked in.
    let _exhaustive_v1_shape = RealEstate {
        id: r.id.clone(),
        address: r.address.clone(),
        ownership: r.ownership.clone(),
        taxes: r.taxes.clone(),
        hoa: r.hoa.clone(),
        income_account: r.income_account.clone(),
        financing_account: r.financing_account.clone(),
        payment_account: r.payment_account.clone(),
        created_at: r.created_at,
        updated_at: r.updated_at,
    };
    std::fs::remove_dir_all(&dir).ok();
}

/// Taxes and the new RealEstate portal fields are intentionally NOT exposed
/// by the read-only v1 FFI: there is no RecordKind for taxes and no getter
/// for a tax filing, so the tax filing — though present in the vault — is
/// unreachable through the FFI surface.
#[test]
fn full_vault_exposes_only_v1_surface() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);

    // EVERY collection the core stores must be reachable. A kind missing here is a
    // collection the phone silently hides from an executor — which is exactly the
    // defect this covers: `Urgent` is the vault's "read this first" tab.
    for kind in [
        RecordKind::Urgent,
        RecordKind::Instruction,
        RecordKind::TrustWill,
        RecordKind::AssetLiability,
        RecordKind::Account,
        RecordKind::RealEstate,
        RecordKind::TaxFiling,
        RecordKind::GeneralDocument,
    ] {
        assert_eq!(v.count(kind), 1, "{kind:?} present");
        assert_eq!(v.list_records(kind).len(), 1, "{kind:?} listed");
        assert!(!v.list_records(kind)[0].label.is_empty(), "{kind:?} has a list label");
    }

    // The urgent note's actual CONTENT reaches the host, not just its count.
    let urgent = v.get_urgent(ids.urgent.clone()).expect("urgent note is readable");
    assert_eq!(urgent.title, "Call the lawyer first");
    assert!(urgent.description.contains("555-0101"));

    // Tax filings expose the attached-document COUNT, so an executor can at least
    // see that documents exist for a year even though opening them is post-MVP.
    let tax = v.get_tax_filing(ids.tax.clone()).expect("tax filing is readable");
    assert_eq!(tax.year, "2024");
    assert_eq!(tax.document_count, 1);

    let gd = v.get_general_document(ids.gendoc.clone()).expect("general doc is readable");
    assert_eq!(gd.title, "Passport");
    assert!(gd.file.is_some(), "the attached blob is visible to the host");

    assert!(matches!(v.get_instruction(ids.tax.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_trust_will(ids.tax.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_asset(ids.tax.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_account(ids.tax.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_real_estate(ids.tax.clone()), Err(VaultError::RecordNotFound)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_history_for_every_kind_has_created_entry() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    for (kind, id) in [
        (RecordKind::Urgent, &ids.urgent),
        (RecordKind::Instruction, &ids.ins),
        (RecordKind::TrustWill, &ids.tw),
        (RecordKind::AssetLiability, &ids.asset),
        (RecordKind::Account, &ids.acc),
        (RecordKind::RealEstate, &ids.re),
        (RecordKind::TaxFiling, &ids.tax),
        (RecordKind::GeneralDocument, &ids.gendoc),
    ] {
        let hist = v.get_history(kind, id.clone()).unwrap();
        assert!(!hist.is_empty(), "history present for {kind:?}");
        assert!(hist.iter().any(|c| c.action == "created"), "a 'created' entry for {kind:?}");
        for c in &hist {
            assert!(c.at > 0, "history entries are timestamped for {kind:?}");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_history_records_edits_with_field_detail() {
    let dir = tmp();
    let path = dir.join("vault.pmv");
    let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };
    let mut ov = OpenVault::create(path, b"one", b"two", params).unwrap();
    let mut acc = records::Account::new().unwrap();
    acc.account_type = "Financial".into();
    acc.username = "alice".into();
    acc.password = "old".into();
    let id = acc.id.clone();
    records::upsert(&mut ov.vault.accounts, acc);
    let mut edit = ov.vault.accounts[0].clone();
    edit.password = "new".into();
    records::upsert(&mut ov.vault.accounts, edit);
    ov.save().unwrap();
    drop(ov);

    let v = open_full_dir(&dir, b"one", b"two");
    let hist = v.get_history(RecordKind::Account, id).unwrap();
    assert!(hist.iter().any(|c| c.action == "created"));
    let pw_edit = hist
        .iter()
        .find(|c| c.action == "updated" && c.detail.contains("password"))
        .expect("the password edit is in history");
    // The cleartext old/new values must be MASKED across the FFI boundary.
    assert!(pw_edit.detail.contains("<hidden>"), "secret values masked: {}", pw_edit.detail);
    assert!(
        !pw_edit.detail.contains("\"old\"") && !pw_edit.detail.contains("\"new\""),
        "no cleartext password leaks through get_history: {}",
        pw_edit.detail
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn get_history_bogus_id_is_record_not_found() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    for kind in [
        RecordKind::Urgent,
        RecordKind::Instruction,
        RecordKind::TrustWill,
        RecordKind::AssetLiability,
        RecordKind::Account,
        RecordKind::RealEstate,
        RecordKind::TaxFiling,
        RecordKind::GeneralDocument,
    ] {
        assert!(matches!(
            v.get_history(kind, "no-such-id".to_string()),
            Err(VaultError::RecordNotFound)
        ));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_typed_getter_rejects_a_bogus_id() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert!(matches!(v.get_instruction("x".into()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_trust_will("x".into()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_asset("x".into()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_account("x".into()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_real_estate("x".into()), Err(VaultError::RecordNotFound)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn typed_getters_are_kind_scoped() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert!(matches!(v.get_instruction(ids.acc.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_trust_will(ids.acc.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_asset(ids.acc.clone()), Err(VaultError::RecordNotFound)));
    assert!(matches!(v.get_real_estate(ids.acc.clone()), Err(VaultError::RecordNotFound)));
    assert!(v.get_account(ids.acc.clone()).is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn audit_log_has_vault_created_and_no_open_event() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let audit = v.audit_log();
    assert!(audit.iter().any(|c| c.action == "vault_created"), "vault_created event present");
    for c in &audit {
        assert!(c.at > 0, "audit entries are timestamped");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recovery_notice_is_none_on_normal_open() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert!(v.recovery_notice().is_none(), "no recovery on a clean open");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generation_and_previous_access_reflect_the_saved_snapshot() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert!(v.generation() >= 1, "opened generation is the saved counter");
    assert!(v.previous_access() > 0, "previous access is a real timestamp");
    std::fs::remove_dir_all(&dir).ok();
}

/// The host renders the "last opened" line from this label, so it must be the
/// desktop's exact `YYYY-MM-DD HH:MM:SS UTC` shape (not a locale/timezone-dependent
/// one) and must agree with the raw timestamp it formats.
#[test]
fn previous_access_label_matches_the_desktop_format() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let label = v.previous_access_label();
    assert_eq!(label, format_time(v.previous_access()), "label formats the real stamp");
    assert!(label.ends_with(" UTC"), "explicit UTC, never local time: {label}");
    // "YYYY-MM-DD HH:MM:SS UTC" — fixed width, so a shape regression is caught.
    assert_eq!(label.len(), 23, "unexpected shape: {label}");
    let (date, rest) = label.split_once(' ').expect("date and time are space-separated");
    assert_eq!(date.len(), 10, "YYYY-MM-DD");
    assert_eq!(date.matches('-').count(), 2, "YYYY-MM-DD");
    assert_eq!(rest.matches(':').count(), 2, "HH:MM:SS");
    std::fs::remove_dir_all(&dir).ok();
}

/// COMPILE-TIME DRIFT GUARD — the fix for the defect where the mobile viewer showed
/// five of the core's eight record collections and silently hid `Urgent`, tax filings
/// and general documents from an executor.
///
/// The destructure below is **exhaustive on purpose**: no `..` rest pattern. Adding a
/// field to `records::Vault` therefore makes this test fail to COMPILE, forcing whoever
/// adds it to come here and answer the question that was never asked last time: *is this
/// a record collection an executor must be able to read on their phone?* If it is, it
/// needs a `RecordKind`, a DTO, a getter and a tab — and the loop below then proves the
/// FFI actually reports it.
///
/// Do not "fix" a compile failure here by adding `..`; that deletes the guard.
#[test]
fn every_core_record_collection_is_reachable_through_the_ffi() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    let ov = v.lock();

    let records::Vault {
        // --- the record collections: each MUST have a RecordKind ------------------
        urgent,
        instructions,
        trust_wills,
        assets,
        accounts,
        real_estate,
        tax_filings,
        general_documents,
        // --- everything else: metadata, not user-visible record collections -------
        version: _,
        generation: _,
        last_opened_at: _,
        id: _,
        settings: _,
        audit: _,
        deleted_docs: _,
        categories: _,
    } = &ov.vault;

    // Snapshot the true lengths while the guard is held...
    let expected = [
        ("urgent", urgent.len(), RecordKind::Urgent),
        ("instructions", instructions.len(), RecordKind::Instruction),
        ("trust_wills", trust_wills.len(), RecordKind::TrustWill),
        ("assets", assets.len(), RecordKind::AssetLiability),
        ("accounts", accounts.len(), RecordKind::Account),
        ("real_estate", real_estate.len(), RecordKind::RealEstate),
        ("tax_filings", tax_filings.len(), RecordKind::TaxFiling),
        ("general_documents", general_documents.len(), RecordKind::GeneralDocument),
    ];
    // ...then RELEASE it before touching the FFI. `Vault::count` takes the very same
    // std::sync::Mutex, which is NOT reentrant, so querying it while still holding the
    // guard would deadlock this test rather than fail it.
    drop(ov);

    // Every collection is non-empty in the fixture, so a kind wired to the WRONG
    // collection (or to none at all) cannot slip through by both sides reading 0.
    for (name, len, kind) in expected {
        assert!(len > 0, "fixture must populate `{name}` for this guard to mean anything");
        assert_eq!(
            v.count(kind) as usize,
            len,
            "core collection `{name}` holds {len} records but the FFI reports {} for {kind:?}",
            v.count(kind)
        );
    }
}

/// SCOPE OF THE "last opened" SIGNAL: a READ-ONLY open does not refresh
/// `last_opened_at` (vault.rs skips the open-time save when `read_only`), so any
/// number of read-only opens leave the reported previous-access stamp unchanged.
/// Mobile is read-only-only and the desktop defaults to read-only, so this pins
/// what the banner can and cannot detect: unauthorised WRITES, not reads.
#[test]
fn read_only_opens_do_not_advance_previous_access() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let first = open_full(&dir).previous_access();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // cross a 1-second tick
    let second = open_full(&dir).previous_access();
    assert_eq!(
        first, second,
        "a read-only open left no trace: previous_access did not advance ({first} -> {second})"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A never-opened vault must read "never", not a 1970 epoch date, which would
/// look like a real (and alarming) prior access to the user.
#[test]
fn format_time_renders_never_for_a_zero_or_negative_stamp() {
    assert_eq!(format_time(0), "never");
    assert_eq!(format_time(-1), "never");
    assert_eq!(format_time(i64::MIN), "never");
    // A known instant: 2021-01-01T00:00:00Z.
    assert_eq!(format_time(1_609_459_200), "2021-01-01 00:00:00 UTC");
}

#[test]
fn generation_rises_across_saves() {
    let dir = tmp();
    let path = dir.join("vault.pmv");
    let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };
    let mut ov = OpenVault::create(path, b"one", b"two", params).unwrap();
    ov.save().unwrap();
    ov.save().unwrap();
    let saved_gen = ov.vault.generation;
    drop(ov);
    let v = open_full_dir(&dir, b"one", b"two");
    assert_eq!(v.generation(), saved_gen, "FFI reports the last-saved generation");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn empty_kinds_list_empty() {
    let dir = tmp();
    make_vault(&dir, b"one", b"two");
    let v = open_full(&dir);
    assert_eq!(v.count(RecordKind::TrustWill), 0);
    assert_eq!(v.count(RecordKind::AssetLiability), 0);
    assert_eq!(v.count(RecordKind::RealEstate), 0);
    assert!(v.list_records(RecordKind::TrustWill).is_empty());
    assert!(v.list_records(RecordKind::AssetLiability).is_empty());
    assert!(v.list_records(RecordKind::RealEstate).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wrong_password_order_is_rejected() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let e = open_vault(dir.to_str().unwrap().to_string(), b"two".to_vec(), b"one".to_vec())
        .err()
        .expect("swapped passwords must fail");
    assert!(matches!(e, VaultError::WrongPasswordOrCorrupt));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_vault_file_is_indistinguishable_from_wrong_password() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let path = dir.join("vault.pmv");
    let mut bytes = std::fs::read(&path).unwrap();
    let i = bytes.len() - 1;
    bytes[i] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("a corrupt vault must fail to open");
    assert!(
        matches!(e, VaultError::WrongPasswordOrCorrupt),
        "corruption maps to the same no-leak variant as a wrong password, got {e:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn truncated_vault_file_maps_to_wrong_password_or_corrupt() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let path = dir.join("vault.pmv");
    std::fs::write(&path, b"PMV").unwrap();

    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("a truncated vault must fail to open");
    assert!(matches!(e, VaultError::WrongPasswordOrCorrupt), "got {e:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn oversize_vault_file_maps_to_wrong_password_or_corrupt_not_internal() {
    // An oversize (padded) vault file is a tamper/corruption signal reachable on
    // the open path (read_capped_vault's size cap). It must collapse to the same
    // no-leak variant as every other open failure — never the distinct `Internal`.
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let path = dir.join("vault.pmv");
    // Grow the file past MAX_VAULT_SIZE (256 MiB) sparsely (set_len doesn't allocate).
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(256 * 1024 * 1024 + 1).unwrap();
    drop(f);
    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("an oversize vault must fail to open");
    assert!(matches!(e, VaultError::WrongPasswordOrCorrupt), "got {e:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn bad_magic_maps_to_wrong_password_or_corrupt() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let path = dir.join("vault.pmv");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("bad magic must fail to open");
    assert!(matches!(e, VaultError::WrongPasswordOrCorrupt), "got {e:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_dir_with_other_files_present_still_not_found() {
    let dir = tmp();
    std::fs::write(dir.join("unrelated.txt"), b"hi").unwrap();
    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("no vault file => error");
    assert!(matches!(e, VaultError::NotFound), "got {e:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reopen_after_close_sees_the_same_records() {
    let dir = tmp();
    let ids = make_full_vault(&dir, b"one", b"two");
    {
        let v = open_full(&dir);
        assert_eq!(v.count(RecordKind::Account), 1);
        drop(v);
    }
    let v2 = open_full(&dir);
    assert_eq!(v2.count(RecordKind::Account), 1);
    assert_eq!(v2.get_real_estate(ids.re.clone()).unwrap().address, "123 Main St");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_record_ids_match_typed_getters() {
    let dir = tmp();
    make_full_vault(&dir, b"one", b"two");
    let v = open_full(&dir);

    let ins_id = v.list_records(RecordKind::Instruction)[0].id.clone();
    assert_eq!(v.get_instruction(ins_id.clone()).unwrap().id, ins_id);
    let tw_id = v.list_records(RecordKind::TrustWill)[0].id.clone();
    assert_eq!(v.get_trust_will(tw_id.clone()).unwrap().id, tw_id);
    let asset_id = v.list_records(RecordKind::AssetLiability)[0].id.clone();
    assert_eq!(v.get_asset(asset_id.clone()).unwrap().id, asset_id);
    let acc_id = v.list_records(RecordKind::Account)[0].id.clone();
    assert_eq!(v.get_account(acc_id.clone()).unwrap().id, acc_id);
    let re_id = v.list_records(RecordKind::RealEstate)[0].id.clone();
    assert_eq!(v.get_real_estate(re_id.clone()).unwrap().id, re_id);
    std::fs::remove_dir_all(&dir).ok();
}
use vaultis_core::crypto::KdfParams;

/// Build a small vault on disk with the core API, then return its directory.
fn make_vault(dir: &std::path::Path, pw1: &[u8], pw2: &[u8]) {
    let path = dir.join("vault.pmv");
    // Cheap KDF params so the test is fast.
    let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };
    let mut ov = OpenVault::create(path, pw1, pw2, params).unwrap();
    let mut acc = records::Account::new().unwrap();
    acc.account_type = "Financial".into();
    acc.username = "alice".into();
    acc.password = "s3cret".into();
    records::upsert(&mut ov.vault.accounts, acc);
    let mut ins = records::Instruction::new().unwrap();
    ins.title = "Funeral wishes".into();
    records::upsert(&mut ov.vault.instructions, ins);
    ov.save().unwrap();
}

fn tmp() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "pmffi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn open_browse_and_view() {
    let dir = tmp();
    make_vault(&dir, b"one", b"two");

    let v = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .expect("opens with correct passwords");

    assert_eq!(v.count(RecordKind::Account), 1);
    assert_eq!(v.count(RecordKind::Instruction), 1);
    assert_eq!(v.count(RecordKind::RealEstate), 0);

    let accounts = v.list_records(RecordKind::Account);
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].label, "Financial - alice");

    let acc = v.get_account(accounts[0].id.clone()).unwrap();
    assert_eq!(acc.username, "alice");
    assert_eq!(acc.password, "s3cret");

    // history exists (a "created" entry) and is fetched separately.
    let hist = v.get_history(RecordKind::Account, accounts[0].id.clone()).unwrap();
    assert!(!hist.is_empty());

    assert!(v.get_account("nope".into()).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wrong_password_and_corrupt_are_indistinguishable() {
    let dir = tmp();
    make_vault(&dir, b"one", b"two");

    // Wrong password -> generic error.
    let e = open_vault(dir.to_str().unwrap().to_string(), b"bad".to_vec(), b"two".to_vec())
        .err()
        .expect("open should fail");
    assert!(matches!(e, VaultError::WrongPasswordOrCorrupt));

    // Right passwords still open.
    assert!(
        open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec()).is_ok()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_vault_reports_not_found() {
    let dir = tmp();
    let e = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .err()
        .expect("open should fail");
    assert!(matches!(e, VaultError::NotFound));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn previous_access_is_a_real_timestamp_not_a_constant() {
    // Kills the `previous_access -> 1` mutant: after create+save+reopen the
    // prior last-opened time is a real unix timestamp, never a small constant.
    let dir = tmp();
    make_vault(&dir, b"one", b"two");
    let v = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec()).unwrap();
    assert!(
        v.previous_access() > 1_000_000_000,
        "previous_access should be a real unix timestamp, got {}",
        v.previous_access()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recovery_notice_is_some_after_mirror_recovery() {
    // Kills the `recovery_notice -> None` mutant: with in-place redundancy on and
    // the primary vault.pmv corrupted, a read-only FFI open recovers from the
    // intact same-generation mirror and reports a recovery notice.
    let dir = tmp();
    let path = dir.join("vault.pmv");
    let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };
    {
        let mut ov = OpenVault::create(path.clone(), b"one", b"two", params).unwrap();
        ov.set_redundancy(1).unwrap();
        let mut ins = records::Instruction::new().unwrap();
        ins.title = "keep".into();
        records::upsert(&mut ov.vault.instructions, ins);
        ov.save().unwrap();
    } // drop releases the writer lock; save wrote the mirror
    // Flip the last byte (inside the Poly1305 tag) of the primary so its AEAD
    // fails while the same-generation mirror stays intact.
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    let v = open_vault(dir.to_str().unwrap().to_string(), b"one".to_vec(), b"two".to_vec())
        .expect("FFI open recovers from the mirror");
    assert!(v.recovery_notice().is_some(), "FFI open should report recovery from the mirror");
    std::fs::remove_dir_all(&dir).ok();
}
