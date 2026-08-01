//! Unit tests for the parent module ([`super`], `migrate.rs`), split into their own
//! file via `#[cfg(test)] #[path = "migrate_tests.rs"] mod tests;` so the tests do not sit
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
fn asset_doc_is_refiled_owner_first_by_kind() {
    // Old asset scheme (pre-redesign): assets/<desc>/<ts>/<file>. Re-filed under owner.
    let t = DocTarget::Asset { kind: "Asset".into(), owner: "Jane Doe".into() };
    assert_eq!(new_doc_path("/assets/brokerage/20240102-030405/stmt.pdf", &t, 0), "/JD/assets/20240102-030405_stmt.pdf");
}

#[test]
fn liability_root_and_initials() {
    let t = DocTarget::Asset { kind: "Liability".into(), owner: "Bob".into() };
    assert_eq!(new_doc_path("/assets/20240102-030405/loan.pdf", &t, 0), "/B/liabilities/20240102-030405_loan.pdf");
}

#[test]
fn tax_keeps_year_under_owner() {
    let t = DocTarget::Tax { owner: "Michael Kaissi".into(), year: "2024".into() };
    assert_eq!(
        new_doc_path("/taxes/2024/20240102-030405/federal/w2.pdf", &t, 0),
        "/MK/taxes/2024/20240102-030405_w2.pdf"
    );
}

#[test]
fn real_estate_keeps_address_under_owner() {
    let t = DocTarget::RealEstate { owner: "Jane Doe".into(), address: "123 Main St".into() };
    assert_eq!(
        new_doc_path("/real-estate/123mainst/20240102-030405/deed.pdf", &t, 0),
        "/JD/real-estate/123mainst/20240102-030405_deed.pdf"
    );
}

#[test]
fn plain_keeps_dirs_and_subfolder_drops_ts_dir() {
    let t = DocTarget::Plain;
    assert_eq!(
        new_doc_path("/trust-will/living-trust/20240102-030405/scans/will.pdf", &t, 0),
        "/trust-will/living-trust/scans/20240102-030405_will.pdf"
    );
}

#[test]
fn orphan_is_plain_just_moves_ts() {
    // No owning record -> Plain: dirs preserved (minus ts), ts folded into filename.
    let t = DocTarget::Plain;
    assert_eq!(
        new_doc_path("/liabilities/20240102-030405/loan.pdf", &t, 0),
        "/liabilities/20240102-030405_loan.pdf"
    );
}

#[test]
fn no_timestamp_falls_back_to_uploaded_at() {
    let t = DocTarget::Plain;
    let ts = records::compact_utc(1_704_164_645); // "20240102-030405"
    assert_eq!(
        new_doc_path("/general-documents/deed/deed.pdf", &t, 1_704_164_645),
        format!("/general-documents/deed/{ts}_deed.pdf")
    );
}

#[test]
fn idempotent_on_already_migrated_paths() {
    // Feeding a migrated path back in (same target) returns it unchanged, even with
    // uploaded_at=0 — proving the embedded ts prefix is reused, never re-synthesized.
    let t = DocTarget::Asset { kind: "Asset".into(), owner: "Jane Doe".into() };
    let once = new_doc_path("/assets/brokerage/20240102-030405/stmt.pdf", &t, 0);
    assert_eq!(new_doc_path(&once, &t, 0), once);

    let p = DocTarget::Plain;
    let once_p = new_doc_path("/trust-will/living-trust/20240102-030405/will.pdf", &p, 0);
    assert_eq!(new_doc_path(&once_p, &p, 0), once_p);
}

#[test]
fn blank_owner_omits_initials_level() {
    let t = DocTarget::Asset { kind: "Asset".into(), owner: "   ".into() };
    assert_eq!(new_doc_path("/assets/20240102-030405/stmt.pdf", &t, 0), "/assets/20240102-030405_stmt.pdf");
}
