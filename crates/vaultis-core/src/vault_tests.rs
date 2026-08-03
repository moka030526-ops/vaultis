//! Unit tests for the parent module ([`super`], `vault.rs`), kept in their own file
//! via `#[cfg(test)] #[path = "vault_tests.rs"] mod tests;` so ~4.8k lines of tests do
//! not sit inside the ~3.3k-line implementation.
//!
//! This is an **inner module**, not an integration test under `tests/`. That matters:
//! `use super::*` reaches the PRIVATE items these tests exercise — `Header`,
//! `Header::parse`, `decode_vault_with_key`, `write_vault_file` — which a test crate in
//! `tests/` could not even name. Moving these cases there would mean marking those items
//! `pub` purely to test them, widening the crate's public surface for no other reason.
//! Tests that only need the public API (or a real process) already live in `tests/`:
//! `golden_v4.rs`, `format_compat.rs`, `metamorphic.rs`, `memory_residue.rs`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — it is never part of the shipped binary, so the fixed test salts/nonces/passwords in
//! here cannot reach a user's vault. Each `#[test]` function is an independent check the
//! runner executes; `assert!` / `assert_eq!` fail (panic) the test if a condition doesn't
//! hold. `.unwrap()` is used liberally because a panic in a test is just a test failure.

use super::*; // pull every item from the parent module (this file) into the tests

/// Backing up a vault you can only READ must work.
///
/// `backup` takes the single-writer lock on the SOURCE so the multi-file copy cannot
/// straddle a concurrent rekey. Acquiring it CREATES `vaultis.lock` in the source
/// directory — impossible on read-only media, a restored snapshot, or a `chmod 500`
/// directory. A restored backup is exactly this case (a snapshot carries no lock
/// file), so the first thing you want to do with a recovered vault failed with a bare
/// "Permission denied" that named no cause.
///
/// A source we cannot even create a file in cannot be being written by anyone else
/// either, which is what makes a lock-less snapshot safe precisely there.
// Gated on `single-writer-lock` as well as unix: `LOCK_FILE` only exists with that
// feature, and without it `WriteLock::acquire` is an unconditional `Ok`, so there is
// no lock-creation failure for this test to be about. Omitting the feature gate broke
// `cargo test -p vaultis-core --no-default-features` — the configuration the mobile
// and static-musl builds actually use, and the one CI never exercises because every
// CI invocation is `--workspace`, which unifies features and switches the lock back on.
#[cfg(all(unix, feature = "single-writer-lock"))]
#[test]
fn backup_works_when_the_source_directory_is_not_writable() {
    use std::os::unix::fs::PermissionsExt;

    let path = tmp_path("robackup_src");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v = OpenVault::create(path.clone(), b"p1", b"p2", fast()).unwrap();
    drop(v); // release the lock
    let src_dir = path.parent().unwrap().to_path_buf();
    let dest = tmp_path("robackup_dest").parent().unwrap().to_path_buf();

    // A restored snapshot carries no lock file: remove it, then make the directory
    // unwritable so one cannot be created either.
    let _ = std::fs::remove_file(src_dir.join(LOCK_FILE));
    let orig = std::fs::metadata(&src_dir).unwrap().permissions();
    std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let result = backup(&path, &dest);

    // Restore permissions before asserting, so a failing assert cannot leave an
    // undeletable directory behind.
    std::fs::set_permissions(&src_dir, orig).unwrap();

    // `backup` returns the copied vault FILE, not its directory.
    let made = result.expect("a read-only source must still be backup-able");
    assert!(made.exists(), "the vault file was copied");
    assert_eq!(made.file_name().unwrap(), "vault.pmv");
    // A lock-less snapshot is still a real one.
    assert!(
        OpenVault::open_read_only(made.clone(), b"p1", b"p2").is_ok(),
        "the backed-up vault opens"
    );

    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dest);
}

/// The lock tolerance above must not swallow a lock file we simply cannot OPEN.
///
/// `PermissionDenied` from `WriteLock::acquire` means either "the directory is
/// unwritable, so no lock can exist" (safe to proceed — the test above) or "the lock
/// file is right there and belongs to someone else" (NOT safe: a concurrent writer may
/// be mid-rekey, and a lock-less snapshot could then pair an old-key vault.pmv with a
/// new-key volume/manifest). Only the first may proceed unlocked.
// Same gate, and for the same reason, as the sibling test above: `LOCK_FILE` only
// exists with `single-writer-lock`, so `#[cfg(unix)]` alone fails to compile under
// `cargo test -p vaultis-core --no-default-features` — the feature set the mobile and
// static-musl builds actually ship.
#[cfg(all(unix, feature = "single-writer-lock"))]
#[test]
fn backup_does_not_skip_the_lock_when_the_lock_file_merely_cannot_be_opened() {
    use std::os::unix::fs::PermissionsExt;

    let path = tmp_path("lockperm_src");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v = OpenVault::create(path.clone(), b"p1", b"p2", fast()).unwrap();
    drop(v); // release the lock, leaving the lock FILE behind
    let src_dir = path.parent().unwrap().to_path_buf();
    let dest = tmp_path("lockperm_dest").parent().unwrap().to_path_buf();
    let lock = src_dir.join(LOCK_FILE);

    // The lock file exists (a concurrent holder's, as far as we can tell) but is not
    // ours to open. The DIRECTORY stays writable, so this is unambiguously the second
    // case: nothing about it says "no one else can be writing here".
    assert!(lock.exists(), "the vault left a lock file behind");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();

    // Root ignores the permission bits this test is built on, so there would be no
    // PermissionDenied to react to. Detect that by the same means (`#![forbid(unsafe_code)]`
    // rules out `geteuid`) and skip rather than assert something untrue.
    if fs::File::open(&lock).is_ok() {
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(&src_dir);
        return;
    }

    let result = backup(&path, &dest);

    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        result.is_err(),
        "an unopenable EXISTING lock file must not be read as 'nobody can be writing here'"
    );

    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dest);
}

// ---- Documents: taxes + real estate (hardening) ------------------------

#[test]
fn referenced_doc_ids_spans_all_four_record_kinds() {
    // referenced_doc_ids must surface ids from Trust&Will.file, Asset.statement,
    // every Taxes filing's documents, every Real Estate property's documents, and
    // each General Document's file.
    let path = tmp_path("refids");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("refids", b"x");

    let tw_id = v.add_document("/wills", "will.pdf", &src).unwrap();
    let asset_id = v.add_document("/assets", "stmt.pdf", &src).unwrap();
    let tax_id1 = v.add_document("taxes/2024", "w2.pdf", &src).unwrap();
    let tax_id2 = v.add_document("taxes/2024", "1099.pdf", &src).unwrap();
    let re_id1 = v.add_document("real-estate/main", "deed.pdf", &src).unwrap();
    let re_id2 = v.add_document("real-estate/main", "policy.pdf", &src).unwrap();
    let gen_id = v.add_document("general-documents/passport", "passport.pdf", &src).unwrap();

    let mut tw = records::TrustWill::new().unwrap();
    tw.file = Some(tw_id.clone());
    records::upsert(&mut v.vault.trust_wills, tw);

    let mut asset = records::AssetLiability::new().unwrap();
    asset.statement = Some(asset_id.clone());
    records::upsert(&mut v.vault.assets, asset);

    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    tax.documents.push(tax_id1.clone());
    tax.documents.push(tax_id2.clone());
    records::upsert(&mut v.vault.tax_filings, tax);

    let mut re = records::RealEstate::new().unwrap();
    re.address = "Main".into();
    re.documents.push(re_id1.clone());
    re.documents.push(re_id2.clone());
    records::upsert(&mut v.vault.real_estate, re);

    let mut g = records::GeneralDocument::new().unwrap();
    g.title = "Passport".into();
    g.file = Some(gen_id.clone());
    records::upsert(&mut v.vault.general_documents, g);

    let ids = referenced_doc_ids(&v.vault);
    for want in [&tw_id, &asset_id, &tax_id1, &tax_id2, &re_id1, &re_id2, &gen_id] {
        assert!(ids.contains(want), "referenced_doc_ids missing {want}; got {ids:?}");
    }
    assert_eq!(ids.len(), 7, "exactly the seven referenced ids, got {ids:?}");

    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn referenced_doc_ids_empty_when_no_attachments() {
    // Records with no attached files contribute nothing.
    let path = tmp_path("refidsempty");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.trust_wills, records::TrustWill::new().unwrap());
    records::upsert(&mut v.vault.assets, records::AssetLiability::new().unwrap());
    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    records::upsert(&mut v.vault.tax_filings, tax);
    let mut re = records::RealEstate::new().unwrap();
    re.address = "x".into();
    records::upsert(&mut v.vault.real_estate, re);
    assert!(referenced_doc_ids(&v.vault).is_empty(), "no attachments -> no referenced ids");
    cleanup(&path);
}

#[test]
fn add_read_document_under_taxes_and_real_estate_locations() {
    // add_document under the shared tax/RE virtual folders, then read_document
    // returns the exact bytes; doc_path reflects the normalized virtual path.
    let path = tmp_path("addread");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let tax_body = vec![1u8; 500];
    let re_body = vec![2u8; 600];
    let tax_src = write_src("addread-tax", &tax_body);
    let re_src = write_src("addread-re", &re_body);

    let tax_loc = records::tax_doc_location("2024"); // "taxes/2024"
    let re_loc = records::real_estate_doc_location("123 Main St"); // "real-estate/123mainst"
    let tax_id = v.add_document(&tax_loc, "w2.pdf", &tax_src).unwrap();
    let re_id = v.add_document(&re_loc, "deed.pdf", &re_src).unwrap();

    assert_eq!(&*v.read_document(&tax_id).unwrap(), &tax_body[..]);
    assert_eq!(&*v.read_document(&re_id).unwrap(), &re_body[..]);
    assert_eq!(v.doc_path(&tax_id).unwrap(), "/taxes/2024/w2.pdf");
    assert_eq!(v.doc_path(&re_id).unwrap(), "/real-estate/123mainst/deed.pdf");
    assert!(v.has_document(&tax_id) && v.has_document(&re_id));

    cleanup(&path);
    fs::remove_file(&tax_src).ok();
    fs::remove_file(&re_src).ok();
}

#[test]
fn export_document_writes_plaintext_for_tax_and_re_docs() {
    // export_document writes the decrypted bytes out (O_EXCL, 0600); a second
    // export to the same path must fail (no clobber).
    let path = tmp_path("expdoc");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let body = vec![7u8; 333];
    let src = write_src("expdoc", &body);
    let tax_id = v.add_document("taxes/2023", "1099.pdf", &src).unwrap();
    let re_id = v.add_document("real-estate/addr", "policy.pdf", &src).unwrap();

    let out_dir = parent_dir(&path).join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let tax_out = out_dir.join("tax.bin");
    let re_out = out_dir.join("re.bin");
    v.export_document(&tax_id, &tax_out).unwrap();
    v.export_document(&re_id, &re_out).unwrap();
    assert_eq!(fs::read(&tax_out).unwrap(), body);
    assert_eq!(fs::read(&re_out).unwrap(), body);
    // O_EXCL: re-exporting onto an existing path is refused.
    assert!(v.export_document(&tax_id, &tax_out).is_err(), "export must not clobber");

    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
#[cfg(unix)]
fn export_document_into_rejects_symlinked_intermediate() {
    // M1 regression: a symlink pre-planted as an intermediate component of the export
    // root must NOT be followed by create_dir_all (else the decrypted plaintext escapes
    // the root into the symlink target).
    use std::os::unix::fs::symlink;
    let path = tmp_path("expsym");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let body = vec![9u8; 64];
    let src = write_src("expsym", &body);
    let id = v.add_document("taxes/2024", "leak.pdf", &src).unwrap(); // -> /taxes/2024/leak.pdf

    let base = parent_dir(&path);
    let root = base.join("exproot");
    let outside = base.join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    // Pre-plant `exproot/taxes -> outside`; the export must refuse to traverse it.
    symlink(&outside, root.join("taxes")).unwrap();

    let err = v.export_document_into(&id, &root).unwrap_err();
    assert!(
        matches!(&err, VaultError::Storage(StorageError::Corrupt(m)) if m.contains("symlink")),
        "must reject the symlinked intermediate, got {err:?}"
    );
    // The decrypted plaintext must NOT have escaped through the symlink.
    assert!(!outside.join("2024").exists(), "plaintext escaped via the symlinked intermediate");

    cleanup(&path);
    fs::remove_file(&src).ok();
    fs::remove_dir_all(&outside).ok();
    fs::remove_dir_all(&root).ok();
}

#[test]
fn compact_volume_keeps_tax_and_re_docs_simultaneously() {
    // Both a tax doc and an RE doc are referenced at once; a single
    // `compact --volume` reclaims garbage while keeping BOTH present.
    let path = tmp_path("cvolboth");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let tax_body = vec![3u8; 400];
    let re_body = vec![4u8; 450];
    let tax_src = write_src("cvolboth-tax", &tax_body);
    let re_src = write_src("cvolboth-re", &re_body);

    let tax_id = v.add_document("taxes/2024", "w2.pdf", &tax_src).unwrap();
    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    tax.documents.push(tax_id.clone());
    records::upsert(&mut v.vault.tax_filings, tax);

    let re_id = v.add_document("real-estate/main", "deed.pdf", &re_src).unwrap();
    let mut re = records::RealEstate::new().unwrap();
    re.address = "Main".into();
    re.documents.push(re_id.clone());
    records::upsert(&mut v.vault.real_estate, re);

    // Dead frames so compaction has reclaimable garbage around the two live docs.
    for i in 0..4 {
        let id = v.add_document("/g", &format!("g{i}.bin"), &tax_src).unwrap();
        v.remove_document(&id).unwrap();
    }
    v.save().unwrap();

    let opts = volume_opts();
    assert!(v.compact_dry_run(&opts).bytes_reclaimed > 0, "garbage should be reclaimable");
    v.compact(&opts).unwrap();
    // Garbage gone, BOTH docs still readable and intact.
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "garbage fully reclaimed");
    assert_eq!(&*v.read_document(&tax_id).unwrap(), &tax_body[..], "tax doc kept");
    assert_eq!(&*v.read_document(&re_id).unwrap(), &re_body[..], "RE doc kept");
    drop(v);

    // Reopens cleanly (referenced subset of stored holds) with both docs intact.
    let re_open = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re_open.read_document(&tax_id).unwrap(), &tax_body[..]);
    assert_eq!(&*re_open.read_document(&re_id).unwrap(), &re_body[..]);
    assert!(!parent_dir(&path).join(REKEY_DIR).exists(), "no staging debris");

    cleanup(&path);
    fs::remove_file(&tax_src).ok();
    fs::remove_file(&re_src).ok();
}

#[test]
fn deleting_tax_filing_then_reclaiming_docs_and_compacting_frees_all() {
    // Mirrors the GUI/TUI delete flow: remove the record, save, then
    // remove_document each attached blob, then `compact --volume` reclaims them.
    let path = tmp_path("deltax");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("deltax", &vec![6u8; 500]);
    let mut docs = Vec::new();
    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    for i in 0..3 {
        let id = v.add_document("taxes/2024", &format!("d{i}.pdf"), &src).unwrap();
        tax.documents.push(id.clone());
        docs.push(id);
    }
    let tax_id = tax.id.clone();
    records::upsert(&mut v.vault.tax_filings, tax);
    // Keep one unrelated live doc so the store does not go fully empty.
    let mut keep_tax = records::TaxFiling::new().unwrap();
    keep_tax.year = "2025".into();
    let keep = v.add_document("taxes/2025", "keep.pdf", &src).unwrap();
    keep_tax.documents.push(keep.clone());
    records::upsert(&mut v.vault.tax_filings, keep_tax);
    v.save().unwrap();

    // Delete the 2024 filing record, persist, then reclaim each of its blobs.
    assert!(records::remove(&mut v.vault.tax_filings, &tax_id, &mut v.vault.audit, "Tax filing"));
    v.save().unwrap();
    for id in &docs {
        v.remove_document(id).unwrap();
    }
    // The deleted filing's blobs are now dead frames; compaction reclaims them.
    let opts = volume_opts();
    assert!(v.compact_dry_run(&opts).bytes_reclaimed > 0, "deleted-filing blobs are reclaimable");
    v.compact(&opts).unwrap();
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "all 2024 docs reclaimed");
    // The deleted docs are gone; the kept one survives.
    for id in &docs {
        assert!(!v.has_document(id), "deleted tax doc {id} should be gone");
    }
    assert_eq!(&*v.read_document(&keep).unwrap(), &vec![6u8; 500][..], "kept doc survives");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![6u8; 500][..]);
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn deleting_real_estate_property_then_reclaiming_docs_and_compacting_frees_all() {
    // Same flow for a Real Estate property holding several documents.
    let path = tmp_path("delre");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("delre", &vec![8u8; 480]);
    let mut docs = Vec::new();
    let mut re = records::RealEstate::new().unwrap();
    re.address = "999 Removed Ave".into();
    for i in 0..4 {
        let id = v.add_document("real-estate/999removedave", &format!("d{i}.pdf"), &src).unwrap();
        re.documents.push(id.clone());
        docs.push(id);
    }
    let re_id = re.id.clone();
    records::upsert(&mut v.vault.real_estate, re);
    // An unrelated kept property doc.
    let mut keep_re = records::RealEstate::new().unwrap();
    keep_re.address = "1 Keep St".into();
    let keep = v.add_document("real-estate/1keepst", "keep.pdf", &src).unwrap();
    keep_re.documents.push(keep.clone());
    records::upsert(&mut v.vault.real_estate, keep_re);
    v.save().unwrap();

    assert!(records::remove(&mut v.vault.real_estate, &re_id, &mut v.vault.audit, "Real Estate"));
    v.save().unwrap();
    for id in &docs {
        v.remove_document(id).unwrap();
    }
    let opts = volume_opts();
    assert!(v.compact_dry_run(&opts).bytes_reclaimed > 0);
    v.compact(&opts).unwrap();
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "all property docs reclaimed");
    for id in &docs {
        assert!(!v.has_document(id), "deleted RE doc {id} should be gone");
    }
    assert_eq!(&*v.read_document(&keep).unwrap(), &vec![8u8; 480][..]);
    drop(v);
    let reopen = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*reopen.read_document(&keep).unwrap(), &vec![8u8; 480][..]);
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn records_remove_alone_leaves_blobs_as_orphans_compaction_keeps_them() {
    // By design: `records::remove` only drops the record JSON; it does NOT touch
    // the store. Until the caller also calls remove_document, the blobs are still
    // LIVE manifest entries, so `compact --volume` conservatively KEEPS them
    // (regression guard against compaction silently dropping not-yet-reclaimed docs).
    let path = tmp_path("orphanre");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("orphanre", &vec![5u8; 300]);
    let id = v.add_document("real-estate/x", "deed.pdf", &src).unwrap();
    let mut re = records::RealEstate::new().unwrap();
    re.address = "X".into();
    re.documents.push(id.clone());
    let re_id = re.id.clone();
    records::upsert(&mut v.vault.real_estate, re);
    v.save().unwrap();

    // Remove ONLY the record (forget to reclaim the blob).
    assert!(records::remove(&mut v.vault.real_estate, &re_id, &mut v.vault.audit, "Real Estate"));
    v.save().unwrap();
    // The blob is now unreferenced but still a live store entry, so there is no
    // reclaimable garbage and compaction keeps it readable.
    let opts = volume_opts();
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "orphan is still a live frame");
    v.compact(&opts).unwrap();
    assert_eq!(&*v.read_document(&id).unwrap(), &vec![5u8; 300][..], "orphan blob preserved");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn remove_document_persists_tombstone_across_reopen() {
    // Regression (deep-hunt): `remove_document` must make its anti-resurrection
    // tombstone DURABLE. The UI flow saves the record→doc unlink BEFORE calling
    // remove_document, so if remove_document only pushed the tombstone in memory it
    // would be lost on close — and a later manifest-loss rebuild could resurrect the
    // deleted frame. Here we mimic that flow (no further save after remove_document)
    // and assert the tombstone survives a reopen.
    let path = tmp_path("tombstone");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("tombstone", &vec![7u8; 256]);
    let id = v.add_document("general-documents/x", "doc.pdf", &src).unwrap();
    let mut g = records::GeneralDocument::new().unwrap();
    g.title = "X".into();
    g.file = Some(id.clone());
    records::upsert(&mut v.vault.general_documents, g);
    v.save().unwrap(); // UI persists the link first...

    // ...then unlinks + reclaims. Persist the unlink, then remove the blob WITHOUT a
    // further save (exactly the UI ordering).
    v.vault.general_documents[0].file = None;
    v.save().unwrap();
    v.remove_document(&id).unwrap();
    drop(v);

    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.vault.deleted_docs.iter().any(|d| d == &id), "tombstone persisted across reopen");
    assert!(re.is_tombstoned(&id), "reopened handle treats the id as deleted");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn serialize_secret_json_matches_serde_and_never_reallocates() {
    // The zero-reallocation serializer must produce byte-identical JSON to serde_json
    // (so nothing else changes) AND end up with capacity == len (the measuring pass sized
    // it exactly, so the real pass never grew/freed an intermediate cleartext buffer).
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    let mut acc = records::Account::new().unwrap();
    acc.title = "Bank".into();
    acc.owner = "Alice".into();
    acc.password = "s3cret-with-some-length-to-force-growth".into();
    records::upsert(&mut vault.accounts, acc);

    let compact = serialize_secret_json(&vault, false).unwrap();
    assert_eq!(&compact[..], serde_json::to_vec(&vault).unwrap().as_slice(), "compact matches serde");
    assert_eq!(compact.capacity(), compact.len(), "exact capacity => no realloc strand");

    let pretty = serialize_secret_json(&vault, true).unwrap();
    assert_eq!(&pretty[..], serde_json::to_vec_pretty(&vault).unwrap().as_slice(), "pretty matches serde");
    assert_eq!(pretty.capacity(), pretty.len(), "exact capacity => no realloc strand");
}

#[test]
fn remove_document_refuses_a_still_referenced_id() {
    // Regression (deep-hunt): removing a blob a record still references would save a
    // dangling reference and brick the vault on next open. remove_document must refuse.
    let path = tmp_path("stillref");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("stillref", &[9u8; 200]);
    let id = v.add_document("general-documents/x", "doc.pdf", &src).unwrap();
    let mut g = records::GeneralDocument::new().unwrap();
    g.title = "X".into();
    g.file = Some(id.clone());
    records::upsert(&mut v.vault.general_documents, g);
    v.save().unwrap();

    // Still referenced -> refused, vault untouched.
    assert!(matches!(v.remove_document(&id), Err(VaultError::StillReferenced)));
    assert!(v.has_document(&id), "blob retained after refused removal");
    // Unlink first, then it succeeds and the vault still opens cleanly.
    v.vault.general_documents[0].file = None;
    v.save().unwrap();
    v.remove_document(&id).unwrap();
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(!re.has_document(&id), "removed after unlink");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn compact_heals_a_referenced_but_tombstoned_blob_instead_of_bricking() {
    // Regression (deep-hunt): a crash-derived "referenced AND tombstoned" state must not
    // make compact/rekey drop the blob (which would brick the next open). Reference wins:
    // the document is kept and the vault stays openable.
    let path = tmp_path("refxtomb");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("refxtomb", &[1u8; 220]);
    let id = v.add_document("general-documents/x", "doc.pdf", &src).unwrap();
    let mut g = records::GeneralDocument::new().unwrap();
    g.title = "X".into();
    g.file = Some(id.clone());
    records::upsert(&mut v.vault.general_documents, g);
    // Force the contradictory state directly (the API now refuses to create it): the doc
    // is referenced by the record AND carries a tombstone.
    v.vault.deleted_docs.push(id.clone());
    v.save().unwrap();

    v.compact(&volume_opts()).unwrap();
    assert!(v.has_document(&id), "referenced blob survived compaction (reference wins)");
    assert!(v.vault.deleted_docs.is_empty(), "tombstones cleared after rewrite");
    assert_eq!(&*v.read_document(&id).unwrap(), &[1u8; 220][..], "content intact");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.has_document(&id), "vault reopens cleanly, document healed back to live");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn many_documents_on_one_record_all_survive_save_reopen_read() {
    // A single record holding MANY (8) documents: every distinct byte pattern
    // survives save -> reopen -> read.
    let path = tmp_path("manydocs");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut tax = records::TaxFiling::new().unwrap();
    tax.year = "2024".into();
    let mut ids = Vec::new();
    for i in 0u8..8 {
        // Distinct content per doc so a cross-wired id would be caught.
        let body = vec![i; 200 + i as usize * 13];
        let src = write_src(&format!("manydocs-{i}"), &body);
        let id = v.add_document("taxes/2024", &format!("doc{i}.pdf"), &src).unwrap();
        fs::remove_file(&src).ok();
        tax.documents.push(id.clone());
        ids.push((id, body));
    }
    records::upsert(&mut v.vault.tax_filings, tax);
    v.save().unwrap();
    drop(v);

    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(reopened.vault.tax_filings[0].documents.len(), 8, "all 8 ids persisted");
    for (id, body) in &ids {
        assert_eq!(&*reopened.read_document(id).unwrap(), &body[..], "doc {id} survives reopen");
    }
    cleanup(&path);
}

#[test]
fn removing_one_of_several_docs_leaves_the_others_readable() {
    // Remove the middle of several documents on a record; the rest stay readable
    // and the removed one is gone.
    let path = tmp_path("rmone");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut ids = Vec::new();
    for i in 0u8..5 {
        let body = vec![i + 1; 150];
        let src = write_src(&format!("rmone-{i}"), &body);
        let id = v.add_document("taxes/2024", &format!("f{i}.pdf"), &src).unwrap();
        fs::remove_file(&src).ok();
        ids.push((id, body));
    }
    // Remove index 2.
    let (gone_id, _) = ids.remove(2);
    v.remove_document(&gone_id).unwrap();
    assert!(!v.has_document(&gone_id), "removed doc is gone");
    assert!(matches!(v.read_document(&gone_id), Err(VaultError::Storage(StorageError::NotFound(_)))));
    // The rest are intact.
    for (id, body) in &ids {
        assert_eq!(&*v.read_document(id).unwrap(), &body[..], "remaining doc {id} readable");
    }
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    for (id, body) in &ids {
        assert_eq!(&*re.read_document(id).unwrap(), &body[..]);
    }
    assert!(!re.has_document(&gone_id));
    cleanup(&path);
}

#[test]
fn add_document_path_length_boundary() {
    // virtual_path("", name) == "/" + name. MAX_PATH_LEN (256) bytes is accepted;
    // one byte over is rejected with PathTooLong.
    let path = tmp_path("pathlen");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("pathlen", b"body");

    // Exactly MAX_PATH_LEN: "/" + 255 chars = 256 bytes.
    let at_limit = "x".repeat(storage::MAX_PATH_LEN - 1);
    assert_eq!(virtual_path("", &at_limit).len(), storage::MAX_PATH_LEN);
    let id = v.add_document("", &at_limit, &src).unwrap();
    assert_eq!(&*v.read_document(&id).unwrap(), b"body");

    // One byte over: "/" + 256 chars = 257 bytes -> PathTooLong.
    let over = "y".repeat(storage::MAX_PATH_LEN);
    assert_eq!(virtual_path("", &over).len(), storage::MAX_PATH_LEN + 1);
    let err = v.add_document("", &over, &src).unwrap_err();
    assert!(matches!(err, VaultError::Storage(StorageError::PathTooLong)), "got {err:?}");

    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn add_document_path_length_boundary_with_location() {
    // The boundary holds when the length comes from location + filename together.
    // virtual_path("/taxes/2024", name) == "/taxes/2024/" + name (12-byte prefix).
    let path = tmp_path("pathlen2");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("pathlen2", b"body");
    let prefix = virtual_path("/taxes/2024", ""); // "/taxes/2024/"
    let fill = storage::MAX_PATH_LEN - prefix.len();
    let name = "z".repeat(fill);
    assert_eq!(virtual_path("/taxes/2024", &name).len(), storage::MAX_PATH_LEN);
    let id = v.add_document("/taxes/2024", &name, &src).unwrap();
    assert_eq!(&*v.read_document(&id).unwrap(), b"body");

    let over = "z".repeat(fill + 1);
    let err = v.add_document("/taxes/2024", &over, &src).unwrap_err();
    assert!(matches!(err, VaultError::Storage(StorageError::PathTooLong)), "got {err:?}");
    cleanup(&path);
    fs::remove_file(&src).ok();
}
use crate::records::{self, Account};
use std::time::{SystemTime, UNIX_EPOCH};

fn fast() -> KdfParams {
    KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }
}
fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}
/// A fresh, unique vault directory; returns its `vault.pmv` path.
fn tmp_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pmvault-{tag}-{}", nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(VAULT_FILE)
}
fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(parent_dir(path));
}
fn write_src(tag: &str, body: &[u8]) -> PathBuf {
    let p = std::env::temp_dir().join(format!("pmsrc-{tag}-{}.txt", nanos()));
    fs::write(&p, body).unwrap();
    p
}
fn sample_account(user: &str, pw: &str) -> Account {
    let mut a = Account::new().unwrap();
    a.account_type = "Checking".into();
    a.username = user.into();
    a.password = pw.into();
    a
}

#[test]
fn create_open_round_trip() {
    let path = tmp_path("roundtrip");
    let mut v = OpenVault::create(path.clone(), b"first", b"second", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("octocat", "hunter2"));
    v.save().unwrap();
    // `drop(v)` ends `v`'s lifetime right here (instead of at the end of scope),
    // which runs its destructor and releases the single-writer lock so the
    // reopen below can take it. This `drop`-to-release pattern recurs throughout.
    drop(v); // release the single-writer lock before reopening

    let reopened = OpenVault::open(path.clone(), b"first", b"second").unwrap();
    assert_eq!(reopened.vault.accounts.len(), 1);
    assert_eq!(reopened.vault.accounts[0].password, "hunter2");
    assert_eq!(reopened.vault.version, FORMAT_VERSION);
    cleanup(&path);
}

#[test]
fn both_passwords_required_and_order_matters() {
    let path = tmp_path("twopw");
    OpenVault::create(path.clone(), b"right1", b"right2", fast()).unwrap();
    assert!(OpenVault::open(path.clone(), b"wrong1", b"right2").is_err());
    assert!(OpenVault::open(path.clone(), b"right1", b"wrong2").is_err());
    assert!(OpenVault::open(path.clone(), b"right2", b"right1").is_err()); // order
    assert!(OpenVault::open(path.clone(), b"right1", b"right2").is_ok());
    cleanup(&path);
}

#[test]
fn create_refuses_existing() {
    let path = tmp_path("exists");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let err = OpenVault::create(path.clone(), b"a", b"b", fast()).err().unwrap();
    // `matches!(value, Pattern)` is true if `value` fits the pattern. Here `_`
    // inside the variant ignores the contained path — we only check the *kind*
    // of error. Used throughout these tests to assert a specific failure.
    assert!(matches!(err, VaultError::AlreadyExists(_)));
    cleanup(&path);
}

#[test]
fn document_round_trip_and_consistency_check() {
    let path = tmp_path("vol");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("doc", b"statement contents");
    let id1 = v.add_document("/statements/2026", "q1.txt", &src).unwrap();
    let id2 = v.add_document("/wills", "will.txt", &src).unwrap();
    assert_eq!(&*v.read_document(&id1).unwrap(), b"statement contents");
    assert_eq!(v.doc_path(&id1).unwrap(), "/statements/2026/q1.txt");

    // Link one doc to a record so the consistency check has something to verify.
    let mut tw = crate::records::TrustWill::new().unwrap();
    tw.file = Some(id1.clone());
    records::upsert(&mut v.vault.trust_wills, tw);
    v.save().unwrap();
    drop(v); // release the single-writer lock before reopening

    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*v2.read_document(&id1).unwrap(), b"statement contents");
    assert_eq!(&*v2.read_document(&id2).unwrap(), b"statement contents");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn add_document_rejects_non_regular_source() {
    let path = tmp_path("nonreg");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // A directory is not a regular file; add_document must refuse it rather than
    // attempt an unbounded read (the /dev/zero / FIFO class of zero-length-but-
    // endless inputs that would otherwise drive an OOM).
    let dir_src = std::env::temp_dir().join(format!("pmsrc-dir-{}", nanos()));
    fs::create_dir_all(&dir_src).unwrap();
    let err = v.add_document("/d", "f.txt", &dir_src).unwrap_err();
    assert!(matches!(err, VaultError::Storage(StorageError::Corrupt(_))));
    let _ = fs::remove_dir_all(&dir_src);
    cleanup(&path);
}

#[test]
fn redundancy_off_by_default_writes_no_extra_files() {
    let path = tmp_path("redoff");
    let v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    assert_eq!(v.redundancy(), 0, "redundancy is off by default");
    drop(v);
    assert!(!mirror_path(&path).exists(), "no mirror when off");
    assert!(!bak_path(&path, 1).exists(), "no generations when off");
    cleanup(&path);
}

#[test]
fn redundancy_writes_mirror_and_generations() {
    let path = tmp_path("redon");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap(); // depth 2 + mirror
    records::upsert(&mut v.vault.accounts, sample_account("u1", "p1"));
    v.save().unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u2", "p2"));
    v.save().unwrap();
    drop(v);
    assert!(mirror_path(&path).exists(), "mirror is written");
    assert!(bak_path(&path, 1).exists(), "newest prior generation kept");
    assert!(bak_path(&path, 2).exists(), "second prior generation kept");
    assert!(!bak_path(&path, 3).exists(), "ring is bounded to the configured depth");
    cleanup(&path);
}

#[test]
fn recovers_from_mirror_when_primary_ciphertext_corrupt() {
    let path = tmp_path("redmir");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p"));
    v.save().unwrap();
    drop(v);
    // Flip a ciphertext byte (header still parses) so the live file fails the AEAD
    // tag but the same-generation mirror is intact — recovery loses no data.
    let mut bytes = fs::read(&path).unwrap();
    bytes[HEADER_LEN] ^= 0xff;
    fs::write(&path, &bytes).unwrap();
    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(v2.recovery_notice().is_some(), "recovery is reported");
    let users: Vec<&str> = v2.vault.accounts.iter().map(|a| a.username.as_str()).collect();
    assert!(users.contains(&"keep-me"), "mirror restores the exact latest state");
    cleanup(&path);
}

#[test]
fn salt_is_authenticated_so_a_salt_damaged_copy_is_unrecoverable() {
    // The salt lives in the AEAD associated data (Header::to_bytes covers bytes 21..37),
    // so corrupting a COPY's header salt makes its body undecryptable under ANY key (wrong
    // key from the bad salt, and a wrong AAD even with a sibling key). This pins WHY
    // recovery only uses each candidate's own-salt key (no cross-salt "sibling" fallback):
    // here the live primary AND the mirror's salt are damaged, leaving no intact-header
    // copy, so the open must FAIL closed rather than appear to recover.
    let path = tmp_path("redsaltauth");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p"));
    v.save().unwrap();
    drop(v);
    let flip = |p: &Path, off: usize| {
        let mut b = fs::read(p).unwrap();
        b[off] ^= 0xff;
        fs::write(p, &b).unwrap();
    };
    flip(&path, 70); // primary body -> recovery runs
    flip(&mirror_path(&path), 21); // mirror header SALT -> body authenticated under the old salt, unrecoverable
    flip(&bak_path(&path, 1), 70); // the only other copy: body corrupt -> not recoverable either
    assert!(
        OpenVault::open(path.clone(), b"a", b"b").is_err(),
        "a salt-damaged copy cannot be recovered (salt is authenticated); with no intact copy, open fails closed"
    );
    cleanup(&path);
}

#[test]
fn recovers_from_generation_when_primary_and_mirror_corrupt() {
    let path = tmp_path("redbak");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p")); // state A
    v.save().unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("newer", "p")); // state B
    v.save().unwrap();
    drop(v);
    // Destroy BOTH the live file and its mirror; only the prior generation (= A) survives.
    fs::write(&path, b"not a vault at all").unwrap();
    fs::write(mirror_path(&path), b"corrupt mirror").unwrap();
    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(v2.recovery_notice().is_some(), "recovery is reported");
    let users: Vec<&str> = v2.vault.accounts.iter().map(|a| a.username.as_str()).collect();
    assert!(users.contains(&"keep-me"), "the prior generation's data survives");
    assert!(!users.contains(&"newer"), "the most recent change was rolled back (expected for a generation)");
    cleanup(&path);
}

#[test]
fn recovery_caps_distinct_salt_derivations_to_bound_open_dos() {
    // Red-team finding: an attacker who can write the vault dir plants candidate
    // copies each with a DISTINCT salt + valid header to force one expensive
    // Argon2 derivation per salt on every open. decrypt_with_redundancy caps the
    // number of distinct salts it derives (MAX_RECOVERY_SALTS = 3). Here a VALID
    // copy is placed only AFTER three distinct junk salts, so the cap is reached
    // before it and recovery fails closed — proving the bound fires. (Honest 1-2
    // salt recovery is covered by the recovers_from_* tests above, which still
    // pass, so the cap does not regress real recovery.)
    let path = tmp_path("redcap");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p"));
    v.save().unwrap();
    drop(v);

    let valid = fs::read(&path).unwrap(); // decryptable under the real (salt_T) key
    // A candidate that PARSES (valid header) but never decodes: a distinct salt
    // (bytes 21..37) makes the derived key — and the AAD — wrong, and we also
    // break a ciphertext byte. The recovery loop derives once per such salt.
    let junk = |salt_byte: u8| {
        let mut b = valid.clone();
        for x in b.iter_mut().take(37).skip(21) {
            *x = salt_byte; // distinct 16-byte salt
        }
        b[HEADER_LEN] ^= 0xff; // corrupt the ciphertext too
        b
    };
    // Candidate order is mirror, bak1, bak2, bak3: put three distinct junk salts
    // first, then the valid copy at bak3 (the 4th candidate).
    fs::write(mirror_path(&path), junk(0x11)).unwrap();
    fs::write(bak_path(&path, 1), junk(0x22)).unwrap();
    fs::write(bak_path(&path, 2), junk(0x33)).unwrap();
    fs::write(bak_path(&path, 3), &valid).unwrap();
    fs::write(&path, b"not a vault").unwrap(); // corrupt the live file -> recovery runs

    assert!(
        OpenVault::open(path.clone(), b"a", b"b").is_err(),
        "a valid copy reachable only past the distinct-salt cap is NOT recovered (DoS bound holds)"
    );
    cleanup(&path);
}

#[test]
fn wrong_password_still_fails_with_redundancy_enabled() {
    let path = tmp_path("redpw");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    v.save().unwrap();
    drop(v);
    // A wrong password must fail (every copy fails the same way) — never a false
    // "recovery". (Also a regression guard that this stays ~one Argon2, not N.)
    let res = OpenVault::open(path.clone(), b"a", b"WRONG");
    assert!(matches!(res, Err(VaultError::Crypto(_))), "wrong password must be a crypto error");
    cleanup(&path);
}

#[test]
fn disabling_redundancy_removes_existing_copies() {
    let path = tmp_path("reddis");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    v.save().unwrap();
    assert!(mirror_path(&path).exists());
    v.set_redundancy(0).unwrap(); // turning it off cleans up the extra copies
    drop(v);
    assert!(!mirror_path(&path).exists(), "mirror removed when disabled");
    assert!(!bak_path(&path, 1).exists(), "generations removed when disabled");
    cleanup(&path);
}

#[test]
fn rekey_regenerates_redundancy_under_new_key() {
    let path = tmp_path("redrekey");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    v.save().unwrap();
    assert!(mirror_path(&path).exists());
    v.change_password(b"c", b"d").unwrap();
    drop(v);
    // The stale OLD-key copies are cleared and FRESH copies are regenerated under
    // the NEW key immediately (no redundancy gap until the next save, §12.8).
    assert!(mirror_path(&path).exists(), "mirror regenerated after rekey");
    assert!(bak_path(&path, 1).exists(), "a generation regenerated after rekey");
    // The regenerated mirror decodes under the NEW passwords (not the old ones).
    let raw = read_capped_vault(&mirror_path(&path)).unwrap();
    assert!(decode_vault_bytes(&raw, b"c", b"d").is_ok(), "mirror is under the new key");
    assert!(decode_vault_bytes(&raw, b"a", b"b").is_err(), "mirror is NOT under the old key");
    // The vault still opens cleanly under the NEW passwords (no recovery needed).
    let v2 = OpenVault::open(path.clone(), b"c", b"d").unwrap();
    assert!(v2.recovery_notice().is_none());
    cleanup(&path);
}

#[test]
fn recovers_from_mirror_when_primary_salt_corrupt() {
    // Regression for the HIGH finding: recovery must NOT derive the key from the
    // corrupt live header. Flipping a byte inside the salt region leaves the
    // header parseable but makes the key derived from it useless; the mirror's
    // (intact) salt must be used instead.
    let path = tmp_path("redsalt");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p"));
    v.save().unwrap();
    drop(v);
    let mut bytes = fs::read(&path).unwrap();
    bytes[21] ^= 0xff; // the salt starts at header offset 21
    fs::write(&path, &bytes).unwrap();
    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(v2.recovery_notice().is_some(), "recovery is reported");
    let users: Vec<&str> = v2.vault.accounts.iter().map(|a| a.username.as_str()).collect();
    assert!(users.contains(&"keep-me"), "recovered the exact latest state from the mirror");
    cleanup(&path);
}

#[test]
fn reducing_redundancy_prunes_excess_generations() {
    let path = tmp_path("redprune");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(5).unwrap();
    for i in 0..6 {
        records::upsert(&mut v.vault.accounts, sample_account(&format!("u{i}"), "p"));
        v.save().unwrap();
    }
    assert!(bak_path(&path, 5).exists(), "depth 5 fills the ring up to bak5");
    v.set_redundancy(2).unwrap(); // lower the depth -> excess generations must be pruned
    drop(v);
    assert!(bak_path(&path, 1).exists() && bak_path(&path, 2).exists(), "kept within the new depth");
    assert!(
        !bak_path(&path, 3).exists() && !bak_path(&path, 4).exists() && !bak_path(&path, 5).exists(),
        "generations beyond the new depth are pruned (no stale old secrets left)"
    );
    cleanup(&path);
}

#[test]
fn redundant_copies_decode_to_expected_generations() {
    let path = tmp_path("redgens");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    for i in 0..3 {
        records::upsert(&mut v.vault.accounts, sample_account(&format!("u{i}"), "p"));
        v.save().unwrap();
    }
    drop(v);
    let gen_of = |p: &Path| {
        let raw = read_capped_vault(p).unwrap();
        decode_vault_bytes(&raw, b"a", b"b").unwrap().0.generation
    };
    let prim = gen_of(&path);
    assert_eq!(gen_of(&mirror_path(&path)), prim, "mirror == current generation (lossless)");
    assert_eq!(gen_of(&bak_path(&path, 1)), prim - 1, "bak1 == previous generation");
    assert_eq!(gen_of(&bak_path(&path, 2)), prim - 2, "bak2 == two generations back");
    cleanup(&path);
}

#[test]
fn no_edit_reopen_preserves_prior_generations() {
    // Audit M1 regression: a writable open does a metadata-only `last_opened_at`
    // refresh (open_inner), which must NOT rotate the redundancy ring. Otherwise
    // routine no-edit opens overwrite the "prior generations" (the undo/rollback
    // depth, §12.8) with copies of the current state, silently eroding the depth the
    // user opted into.
    let path = tmp_path("rednoedit");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    for i in 0..3 {
        records::upsert(&mut v.vault.accounts, sample_account(&format!("u{i}"), "p"));
        v.save().unwrap();
    }
    drop(v);
    // Decode each retained generation's account set.
    let accts = |p: &Path| {
        let raw = read_capped_vault(p).unwrap();
        let mut u: Vec<String> = decode_vault_bytes(&raw, b"a", b"b")
            .unwrap()
            .0
            .accounts
            .iter()
            .map(|a| a.username.clone())
            .collect();
        u.sort();
        u
    };
    let bak1_before = accts(&bak_path(&path, 1));
    let bak2_before = accts(&bak_path(&path, 2));
    // bak2 is the OLDEST retained generation — a genuinely older (smaller) state here.
    assert!(
        bak2_before.len() < bak1_before.len(),
        "precondition: bak2 ({:?}) is an older generation than bak1 ({:?})",
        bak2_before,
        bak1_before
    );
    // Re-open the HEALTHY vault several times with NO edits.
    for _ in 0..3 {
        let v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
        assert!(v.recovery_notice().is_none(), "a healthy open is not a recovery");
        drop(v);
    }
    // The prior generations are unchanged: no-edit opens did not rotate the ring.
    assert_eq!(accts(&bak_path(&path, 1)), bak1_before, "bak1 unchanged by no-edit reopens");
    assert_eq!(
        accts(&bak_path(&path, 2)),
        bak2_before,
        "bak2 (oldest) NOT overwritten with the current state by no-edit reopens"
    );
    cleanup(&path);
}

#[test]
fn open_sweeps_old_key_redundancy_leftover_after_a_failed_rekey_cleanup() {
    // Audit F3: a password change whose best-effort cleanup_redundancy fails to remove an
    // old-key bak leaves a full pre-rekey vault copy decryptable under the OLD password.
    // A subsequent writable open must reap that foreign-epoch leftover (forward secrecy).
    let path = tmp_path("f3-leftover");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("keep", "p"));
    v.save().unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("two", "p"));
    v.save().unwrap();
    // Capture the OLD-key bak1 (decryptable under a/b).
    let old_bak1 = read_capped_vault(&bak_path(&path, 1)).unwrap();
    assert!(decode_vault_bytes(&old_bak1, b"a", b"b").is_ok(), "precondition: bak1 decodes under the old password");
    // Change the password: cleanup_redundancy + refresh rewrite the ring under c/d.
    v.change_password(b"c", b"d").unwrap();
    drop(v);
    // Simulate cleanup_redundancy PARTIALLY failing: re-plant the old-key bak1 bytes (as if
    // its remove_file had failed while the sibling staging-dir unlink succeeded).
    fs::write(bak_path(&path, 1), &old_bak1).unwrap();
    assert!(
        decode_vault_bytes(&read_capped_vault(&bak_path(&path, 1)).unwrap(), b"a", b"b").is_ok(),
        "planted old-key leftover decodes under the OLD password before the sweep"
    );
    // A writable open must reap the foreign-epoch leftover.
    let v2 = OpenVault::open(path.clone(), b"c", b"d").unwrap();
    assert!(v2.recovery_notice().is_none(), "healthy open (primary intact)");
    drop(v2);
    // No surviving redundancy copy decodes under the OLD password.
    for c in redundancy_candidates(&path) {
        let bytes = read_capped_vault(&c).unwrap();
        assert!(
            decode_vault_bytes(&bytes, b"a", b"b").is_err(),
            "no old-key-decodable copy survives the open: {}",
            c.display()
        );
    }
    cleanup(&path);
}

#[test]
fn create_discards_stale_rekey_staging() {
    // A leftover `.rekey/READY` from an aborted rekey of a since-removed vault must
    // NOT be rolled forward over a freshly created vault on the next open.
    let path = tmp_path("crrekey");
    let dir = parent_dir(&path);
    let staging = dir.join(REKEY_DIR);
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join(VAULT_FILE), b"bogus stale staged vault").unwrap();
    fs::write(staging.join(REKEY_READY), b"ready").unwrap();
    {
        let _v = OpenVault::create(path.clone(), b"c", b"d", fast()).unwrap();
    }
    assert!(!staging.exists(), "create() cleared the stale staging");
    // Without the fix, the next open would roll the bogus stage over vault.pmv and fail.
    let v = OpenVault::open(path.clone(), b"c", b"d").unwrap();
    assert!(v.recovery_notice().is_none());
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn redundancy_bak_write_is_symlink_safe() {
    // A symlink planted at a bak path must be REPLACED by the atomic write, not
    // followed (which would clobber the symlink's target + chmod it).
    use std::os::unix::fs::symlink;
    let path = tmp_path("redsym");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    v.save().unwrap();
    drop(v);
    let victim = std::env::temp_dir().join(format!("redsym-victim-{}", nanos()));
    fs::write(&victim, b"do not touch").unwrap();
    let b1 = bak_path(&path, 1);
    let _ = fs::remove_file(&b1);
    symlink(&victim, &b1).unwrap(); // bak1 -> victim
    // Reopening (writable) triggers a heal/refresh save that rotates the ring.
    let mut v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    records::upsert(&mut v2.vault.accounts, sample_account("x", "y"));
    v2.save().unwrap();
    drop(v2);
    assert_eq!(fs::read(&victim).unwrap(), b"do not touch", "the symlink target must be untouched");
    assert!(
        !fs::symlink_metadata(&b1).unwrap().file_type().is_symlink(),
        "bak1 is now a real file, not the planted symlink"
    );
    let _ = fs::remove_file(&victim);
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn failed_save_does_not_degrade_generation_ring() {
    // Regression: the ring must be rotated only AFTER the primary commits, so a
    // failed save leaves the retained generations untouched.
    let path = tmp_path("redfault");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    for i in 0..3 {
        records::upsert(&mut v.vault.accounts, sample_account(&format!("u{i}"), "p"));
        v.save().unwrap();
    }
    let b1_before = fs::read(bak_path(&path, 1)).unwrap();
    let b2_before = fs::read(bak_path(&path, 2)).unwrap();
    crate::fault::fail_at("vault.write", 1);
    records::upsert(&mut v.vault.accounts, sample_account("late", "p"));
    let res = v.save();
    crate::fault::clear();
    assert!(res.is_err(), "save fails when the primary write fails");
    assert_eq!(fs::read(bak_path(&path, 1)).unwrap(), b1_before, "bak1 untouched after a failed save");
    assert_eq!(fs::read(bak_path(&path, 2)).unwrap(), b2_before, "bak2 untouched after a failed save");
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn generation_recovery_with_unreadable_mirror_reports_loss_not_mirror() {
    // Regression: when the mirror's READ fails (so it drops out of the candidate
    // blobs), recovery from a prior generation must NOT be mislabeled as a
    // lossless mirror recovery — the notice must warn that the latest change may be
    // missing. (The notice intentionally hedges with "may be missing" rather than
    // asserting a definite "earlier generation": at recovery time we cannot read
    // the lost primary's generation, and after a rekey a bak is often the CURRENT
    // generation — asserting loss there cried wolf, audit R-12. The non-mirror
    // wording still prompts the user to re-save and refresh backups.)
    let path = tmp_path("redmislabel");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("old", "p")); // state A -> becomes bak1
    v.save().unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("new", "p")); // state B -> primary + mirror
    v.save().unwrap();
    drop(v);
    // Live file corrupt; mirror replaced by a DIRECTORY so its read fails (EISDIR)
    // and it drops out of the candidate blobs; only bak1 (=A) survives → recovery
    // is from an earlier generation, which must be reported as such.
    fs::write(&path, b"garbage not a vault").unwrap();
    fs::remove_file(mirror_path(&path)).unwrap();
    fs::create_dir(mirror_path(&path)).unwrap();
    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    let notice = v2.recovery_notice().unwrap_or("");
    assert!(
        notice.contains("may be missing") && !notice.contains("its mirror copy"),
        "must warn of possible loss and NOT claim a lossless mirror recovery, got: {notice:?}"
    );
    assert!(!notice.contains("no data lost"), "must NOT claim no data lost, got: {notice:?}");
    let users: Vec<&str> = v2.vault.accounts.iter().map(|a| a.username.as_str()).collect();
    assert!(users.contains(&"old") && !users.contains(&"new"), "recovered the prior generation A");
    cleanup(&path);
}

/// A fresh directory for an import-mirror source.
fn tmp_src(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pmsrc-mirror-{tag}-{}", nanos()));
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn add_and_read_a_moderately_large_document() {
    // A ~256 KiB document must round-trip — well under MAX_DOC_SIZE (64 MiB) but
    // far above the trivial test docs, so it also catches a mutation that shrinks
    // the size cap to a tiny value (which would then wrongly reject it).
    let path = tmp_path("biggish");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let body = vec![0x5Au8; 256 * 1024];
    let src = write_src("big", &body);
    let id = v.add_document("/d", "big.bin", &src).unwrap();
    assert_eq!(&*v.read_document(&id).unwrap(), &body[..], "large doc round-trips intact");
    let _ = fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn import_tree_rejects_unsafe_vault_id() {
    let src = tmp_src("badid");
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = "../../etc/passwd".into(); // not a safe ASCII-alnum token
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let dest = tmp_path("impbadid");
    let res = OpenVault::import_tree(&src, &dest, b"a", b"b", fast());
    assert!(res.is_err(), "an unsafe vault id in the untrusted mirror is rejected");
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(parent_dir(&dest));
}

#[test]
fn is_safe_blob_id_allows_only_hex_and_blocks_escapes() {
    // Real ids: 32 lowercase hex chars.
    assert!(is_safe_blob_id(&records::random_id().unwrap()));
    assert!(is_safe_blob_id("00ff"));
    // Filesystem-escape / device vectors that the old denylist missed:
    for bad in [
        "", "..", ".", "a/b", "a\\b", "a\0b", // separators / dot / nul
        "C:evil", "secret:hidden",            // Windows drive-relative / NTFS ADS
        "NUL", "CON", "COM1", "LPT1",          // Windows reserved device names
        "foo.", "foo ", " foo",                // trailing dot / spaces
        "deadbeefg",                            // non-hex letter
        "DEADBEEF", "AAbb",                     // UPPERCASE hex (R-11: case-insensitive-FS collision)
        &"a".repeat(65),                        // over-length
    ] {
        assert!(!is_safe_blob_id(bad), "must reject {bad:?}");
    }
}

#[test]
fn is_safe_doc_path_rejects_control_and_bidi() {
    assert!(is_safe_doc_path("trust-wills/auto/ts/deed.pdf"));
    assert!(!is_safe_doc_path("a\nb"));
    assert!(!is_safe_doc_path("a\x1b[2Jb")); // terminal escape
    assert!(!is_safe_doc_path("a\0b"));
    // R-5: Unicode bidi/format/zero-width chars (NOT caught by is_control).
    assert!(!is_safe_doc_path("invoice\u{202e}fdp.scr"), "RLO override rejected");
    assert!(!is_safe_doc_path("a\u{200b}b"), "zero-width space rejected");
    assert!(!is_safe_doc_path("a\u{2066}b"), "bidi isolate rejected");
    assert!(!is_safe_doc_path("\u{feff}name"), "BOM rejected");
}

#[test]
fn create_rejects_kdf_params_the_reader_would_refuse() {
    // Write path must enforce the same bounds as Header::parse, so a vault can
    // never be written that is then permanently unopenable (BadParams).
    let path = tmp_path("createbadparams");
    let bad = KdfParams { m_cost: KdfParams::MAX_M_COST + 1, t_cost: 3, p_cost: 1 };
    let res = OpenVault::create(path.clone(), b"a", b"b", bad);
    assert!(matches!(res, Err(VaultError::BadParams)), "create must reject out-of-range params");
    assert!(!path.exists(), "no vault file is written when params are rejected");
    let _ = fs::remove_dir_all(parent_dir(&path));
}

#[test]
fn import_tree_clamps_absurd_volume_size() {
    let src = tmp_src("bigvol");
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = "abc123def456".into(); // valid token
    vault.settings.volume_max_size = u64::MAX; // absurd, untrusted
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let dest = tmp_path("impbigvol");
    let v = OpenVault::import_tree(&src, &dest, b"c", b"d", fast()).unwrap();
    assert!(v.volume_max_size() <= MAX_VOLUME_MAX_SIZE, "absurd volume_max_size clamped on import");
    assert!(v.volume_max_size() >= MIN_VOLUME_MAX_SIZE);
    drop(v);
    let _ = fs::remove_dir_all(&src);
    cleanup(&dest);
}

#[test]
fn read_only_open_does_not_write_redundancy_or_touch_primary() {
    let path = tmp_path("ro");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        v.set_redundancy(2).unwrap();
        v.save().unwrap();
    }
    // Remove the redundancy copies; a READ-ONLY open must not regenerate them or
    // rewrite the primary (no auto-save, no heal, no rotation on a read-only open).
    let _ = fs::remove_file(mirror_path(&path));
    for k in 1..=MAX_REDUNDANCY {
        let _ = fs::remove_file(bak_path(&path, k));
    }
    let before = fs::metadata(&path).unwrap().len();
    {
        let v = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
        assert!(v.recovery_notice().is_none());
        drop(v);
    }
    assert!(!mirror_path(&path).exists(), "read-only open wrote a mirror");
    assert!(!bak_path(&path, 1).exists(), "read-only open wrote a generation");
    assert_eq!(fs::metadata(&path).unwrap().len(), before, "primary unchanged on read-only open");
    cleanup(&path);
}

#[test]
fn stale_temp_files_swept_on_writable_open() {
    let path = tmp_path("tmpsweep");
    {
        let _ = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    }
    let dir = parent_dir(&path);
    // Simulate atomic-write temps leaked by a crash mid-save.
    let stale_primary = dir.join(".vault.pmv.deadbeef.tmp");
    let stale_mirror = dir.join(".vault.pmv.mirror.cafef00d.tmp");
    // A leaked last_update marker temp (crash between write_new_file and rename) must also be
    // reaped — otherwise distinct-named orphans accumulate across crashes.
    let stale_marker_tmp = dir.join(".last_update_19990101-000000.deadbeef.tmp");
    fs::write(&stale_primary, b"leaked encrypted temp").unwrap();
    fs::write(&stale_mirror, b"leaked encrypted temp").unwrap();
    fs::write(&stale_marker_tmp, b"19990101-000000\n").unwrap();
    assert_eq!(markers(&dir).len(), 1, "create() left one live marker");
    {
        let _ = OpenVault::open(path.clone(), b"a", b"b").unwrap(); // writable open sweeps
    }
    assert!(!stale_primary.exists(), "stale primary .tmp swept on writable open");
    assert!(!stale_mirror.exists(), "stale mirror .tmp swept on writable open");
    assert!(!stale_marker_tmp.exists(), "leaked last_update marker temp swept too");
    // The LIVE marker (no leading dot, no .tmp) must survive the sweep.
    assert_eq!(markers(&dir).len(), 1, "the live last_update marker survives the sweep");
    cleanup(&path);
}

#[test]
fn missing_referenced_document_is_rejected_on_open() {
    let path = tmp_path("mismatch");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("d", b"doc");
    let id = v.add_document("/d", "f.txt", &src).unwrap();
    let mut tw = crate::records::TrustWill::new().unwrap();
    tw.file = Some(id);
    records::upsert(&mut v.vault.trust_wills, tw);
    v.save().unwrap();
    drop(v);

    // Wipe the volume directory: the referenced doc is now missing.
    fs::remove_dir_all(parent_dir(&path).join("volume")).unwrap();
    fs::remove_dir_all(parent_dir(&path).join("manifest")).unwrap();
    let err = OpenVault::open(path.clone(), b"a", b"b").err().unwrap();
    assert!(matches!(err, VaultError::ArchiveMismatch));
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn change_password_full_reencrypt_keeps_docs() {
    let path = tmp_path("rekey");
    let mut v = OpenVault::create(path.clone(), b"old1", b"old2", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    let src = write_src("rk", b"will body");
    let id = v.add_document("/wills", "will.txt", &src).unwrap();
    v.change_password(b"new1", b"new2").unwrap();
    drop(v); // release the single-writer lock before reopening

    // Old passwords no longer work; new ones open and the doc still reads.
    assert!(OpenVault::open(path.clone(), b"old1", b"old2").is_err());
    let reopened = OpenVault::open(path.clone(), b"new1", b"new2").unwrap();
    assert_eq!(reopened.vault.accounts.len(), 1);
    assert_eq!(&*reopened.read_document(&id).unwrap(), b"will body");
    // Staging was cleaned up.
    assert!(!parent_dir(&path).join(REKEY_DIR).exists());
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn old_password_cannot_roll_back_a_committed_rekey_via_a_stranded_bak() {
    // Audit 2026-07-03 A-2 (forward-secrecy / silent rollback). After a committed password
    // change, a stranded OLD-epoch `bak` (a best-effort cleanup that failed) must NOT let a
    // user who types the OLD password "recover" the pre-rekey vault: that would undo the
    // password change, re-validate the old credentials, and let the writable-open sweep/heal
    // destroy the new-epoch copies. Recovery must stay confined to the current (corroborated)
    // salt, so the old password fails closed.
    let path = tmp_path("rollback");
    let mut v = OpenVault::create(path.clone(), b"old1", b"old2", fast()).unwrap();
    v.set_redundancy(2).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("secret-user", "p"));
    v.save().unwrap();
    // Snapshot the OLD-epoch primary ciphertext, then rekey to new passwords.
    let old_epoch_bytes = fs::read(&path).unwrap();
    v.change_password(b"new1", b"new2").unwrap();
    drop(v); // release the writer lock

    // Simulate a cleanup that failed to remove an old-epoch bak: plant it as bak2.
    fs::write(bak_path(&path, 2), &old_epoch_bytes).unwrap();

    // The OLD password must NOT open the vault (no rollback to the pre-rekey epoch),
    // even though a decodable old-epoch copy is sitting right there.
    assert!(
        OpenVault::open(path.clone(), b"old1", b"old2").is_err(),
        "old password must fail closed — never roll back across the committed rekey"
    );
    // The NEW password still opens it, and the data is intact.
    let reopened = OpenVault::open(path.clone(), b"new1", b"new2").unwrap();
    assert_eq!(reopened.vault.accounts.len(), 1);
    assert_eq!(reopened.vault.accounts[0].username, "secret-user");
    cleanup(&path);
}

#[test]
fn rekey_roll_forward_on_interrupted_commit() {
    // Simulate a crash AFTER staging was marked READY but BEFORE commit: the
    // .rekey dir (with READY) is present. Reopening must roll forward.
    let path = tmp_path("rollfwd");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
        v.save().unwrap();
    }
    let dir = parent_dir(&path);
    // Manually stage a new-key tree (mirror change_password's staging).
    let staging = dir.join(REKEY_DIR);
    fs::create_dir_all(&staging).unwrap();
    let new_salt = crypto::random_bytes::<SALT_LEN>().unwrap();
    let new_key = crypto::derive_key_chained(b"c", b"d", &new_salt, &fast()).unwrap();
    let (vault, _h, _k) = decrypt_file(&path, b"a", b"b").unwrap();
    // Empty store under the staging dir (no docs in this vault).
    let _ = VolumeStore::open(&staging, &new_key, &vault.id, vault.settings.volume_max_size).unwrap();
    write_vault_file(&staging.join(VAULT_FILE), &vault, &new_key, &new_salt, fast()).unwrap();
    write_new_bytes(&staging.join(REKEY_READY), b"ready").unwrap();

    // Reopen: roll-forward completes, so the NEW passwords open it.
    let reopened = OpenVault::open(path.clone(), b"c", b"d").unwrap();
    assert_eq!(reopened.vault.accounts.len(), 1);
    assert!(!dir.join(REKEY_DIR).exists());
    cleanup(&path);
}

#[test]
fn rekey_discard_on_incomplete_staging() {
    // .rekey present WITHOUT READY → staging is discarded, old passwords work.
    let path = tmp_path("discard");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let staging = parent_dir(&path).join(REKEY_DIR);
    fs::create_dir_all(staging.join("volume")).unwrap();
    fs::write(staging.join("vault.pmv"), b"partial").unwrap();

    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(reopened.vault.version, FORMAT_VERSION);
    assert!(!staging.exists(), "incomplete staging discarded");
    cleanup(&path);
}

#[test]
fn read_only_with_pending_rekey_is_reported() {
    let path = tmp_path("ropending");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    fs::create_dir_all(parent_dir(&path).join(REKEY_DIR)).unwrap();
    let err = OpenVault::open_read_only(path.clone(), b"a", b"b").err().unwrap();
    assert!(matches!(err, VaultError::RekeyPending));
    cleanup(&path);
}

#[test]
fn truncated_file_detected() {
    let path = tmp_path("trunc");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    fs::write(&path, b"PMVAULT\0").unwrap();
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_err());
    cleanup(&path);
}

#[test]
fn rejects_absurd_kdf_params() {
    let path = tmp_path("badparams");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut raw = fs::read(&path).unwrap();
    raw[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path, &raw).unwrap();
    let err = OpenVault::open(path.clone(), b"a", b"b").err().unwrap();
    assert!(matches!(err, VaultError::BadParams));
    cleanup(&path);
}

#[test]
fn header_parse_param_and_length_boundaries() {
    // Build a 61-byte header with the given KDF params (no ciphertext needed:
    // Header::parse validates magic/version/params/length only).
    fn header_bytes(m: u32, t: u32, p: u32) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..8].copy_from_slice(MAGIC);
        b[8] = FORMAT_VERSION;
        b[9..13].copy_from_slice(&m.to_le_bytes());
        b[13..17].copy_from_slice(&t.to_le_bytes());
        b[17..21].copy_from_slice(&p.to_le_bytes());
        b
    }
    // Exactly at each bound: accepted (kills `< ` -> `<=`, `>` -> `>=`).
    assert!(Header::parse(&header_bytes(8, 1, 1)).is_ok());
    assert!(
        Header::parse(&header_bytes(KdfParams::MAX_M_COST, KdfParams::MAX_T_COST, KdfParams::MAX_P_COST)).is_ok()
    );
    // One step outside each bound: rejected (kills the `||` and comparison mutants).
    for h in [
        header_bytes(7, 1, 1),
        header_bytes(KdfParams::MAX_M_COST + 1, 1, 1),
        header_bytes(8, 0, 1),
        header_bytes(8, KdfParams::MAX_T_COST + 1, 1),
        header_bytes(8, 1, 0),
        header_bytes(8, 1, KdfParams::MAX_P_COST + 1),
    ] {
        assert!(matches!(Header::parse(&h), Err(VaultError::BadParams)), "params should be rejected");
    }
    // Exactly HEADER_LEN bytes is NOT truncated; one byte short is (kills `<`->`<=`).
    assert!(Header::parse(&header_bytes(8, 1, 1)[..]).is_ok());
    assert!(matches!(Header::parse(&header_bytes(8, 1, 1)[..HEADER_LEN - 1]), Err(VaultError::Truncated)));
    // Bad magic / unsupported version.
    let mut bad_magic = header_bytes(8, 1, 1);
    bad_magic[0] ^= 0xFF;
    assert!(matches!(Header::parse(&bad_magic), Err(VaultError::BadMagic)));
    let mut bad_version = header_bytes(8, 1, 1);
    bad_version[8] = FORMAT_VERSION + 1;
    assert!(matches!(Header::parse(&bad_version), Err(VaultError::BadVersion(_))));
}

#[test]
fn header_tampering_is_detected() {
    let path = tmp_path("hdrtamper");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let good = fs::read(&path).unwrap();
    let flipped_fails = |offset: usize| -> bool {
        let mut bad = good.clone();
        bad[offset] ^= 0x01;
        fs::write(&path, &bad).unwrap();
        OpenVault::open(path.clone(), b"a", b"b").is_err()
    };
    assert!(flipped_fails(9), "param tampering detected");
    assert!(flipped_fails(21), "salt tampering detected");
    assert!(flipped_fails(37), "nonce tampering detected");
    fs::write(&path, &good).unwrap();
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_ok());
    cleanup(&path);
}

#[test]
fn body_version_disagreeing_with_header_is_rejected() {
    // decode_vault_with_key re-checks the AEAD-authenticated body's `version` against
    // the header as defense-in-depth. A single-byte flip can't reach this branch (it
    // fails the Poly1305 tag first), so the exhaustive byte-flip test never covers it
    // and a mutant deleting the check survives. Craft a body whose version disagrees
    // UNDER A VALID TAG and assert it is rejected with BadVersion.
    let params = KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 };
    let salt = [0x11u8; SALT_LEN];
    let nonce = [0x22u8; NONCE_LEN];
    let key = crypto::derive_key(b"k", &salt, &params).unwrap();
    let header = Header { params, salt, nonce };
    let aad = header.to_bytes();
    let mut body = Vault::default();
    body.version = FORMAT_VERSION - 1; // authenticated, but the WRONG version
    let json = serde_json::to_vec(&body).unwrap();
    let ct = crypto::encrypt_with_nonce(&key, &nonce, &json, &aad).unwrap();
    let mut raw = aad.to_vec();
    raw.extend_from_slice(&ct);
    // The tag verifies (we are past AEAD), but body.version != FORMAT_VERSION.
    let err = decode_vault_with_key(&raw, &key).unwrap_err();
    assert!(
        matches!(err, VaultError::BadVersion(v) if v == FORMAT_VERSION - 1),
        "expected BadVersion, got {err:?}"
    );
}

#[test]
fn every_single_byte_flip_of_a_valid_vault_is_rejected_without_panic() {
    // Exhaustive tamper matrix over the WHOLE open path (parse → KDF → AEAD → JSON →
    // referenced⊆stored), complementing the byte-level parser fuzzers: flip one bit
    // of EVERY byte of a valid `vault.pmv` and assert the open fails closed — never
    // a panic, never a silent accept. This pins the guarantee that the entire file
    // (magic/version/params/salt/nonce as AAD + ciphertext + Poly1305 tag) is
    // integrity-protected, not just the three header offsets checked above.
    let path = tmp_path("byteflip");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let good = fs::read(&path).unwrap();
    // Default: flip EVERY byte (exhaustive). Under a long mutation-testing run this
    // ~3000-open test dominates each mutant's wall-time, so `VAULTIS_TAMPER_SAMPLE`
    // thins it to a representative stride (header bytes are always hit because the
    // header is <61 bytes; the stride then samples the ciphertext + tag).
    let step = if std::env::var_os("VAULTIS_TAMPER_SAMPLE").is_some() { 7 } else { 1 };
    for off in (0..good.len()).step_by(step) {
        for bit in [0x01u8, 0x80u8] {
            let mut bad = good.clone();
            bad[off] ^= bit;
            if bad == good {
                continue;
            }
            fs::write(&path, &bad).unwrap();
            // `is_err()` requires the open to RETURN an error — a panic here would
            // fail the test (libtest treats a panic as a failure), which is the
            // point: no single-byte corruption may crash the opener.
            assert!(
                OpenVault::open(path.clone(), b"a", b"b").is_err(),
                "flipping bit {bit:#x} of byte {off} was accepted (integrity gap)"
            );
        }
    }
    // The untouched file still opens — proves the matrix wasn't vacuously passing.
    fs::write(&path, &good).unwrap();
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_ok());
    cleanup(&path);
}

#[test]
fn export_documents_spans_all_partitions() {
    let path = tmp_path("exportall");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Tiny volume cap so the two docs land in different partitions.
    v.vault.settings.volume_max_size = 1024;
    v.save().unwrap();
    drop(v); // release the single-writer lock before reopening
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    let src = write_src("big", &vec![3u8; 600]);
    v.add_document("/a", "a.bin", &src).unwrap();
    v.add_document("/b", "b.bin", &src).unwrap();
    drop(v);

    let docs = OpenVault::export_documents(&path, b"a", b"b", None).unwrap();
    assert_eq!(docs.len(), 2, "extract spans every partition");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn export_document_into_recreates_structure_and_never_overwrites() {
    let path = tmp_path("exintodir");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("exinto", b"the deed");
    let id = v.add_document("/real-estate/123main", "deed.pdf", &src).unwrap();
    let root = parent_dir(&path).join("exports");
    // First export recreates the document's virtual path under `root`.
    let p1 = v.export_document_into(&id, &root).unwrap();
    let vpath = v.doc_path(&id).unwrap();
    assert_eq!(p1, root.join(vpath.trim_start_matches('/')), "structure recreated under root");
    assert_eq!(std::fs::read(&p1).unwrap(), b"the deed");
    // A second export of the same document must NOT overwrite — it gets a `_N` sibling.
    let p2 = v.export_document_into(&id, &root).unwrap();
    assert_ne!(p1, p2, "second export never overwrites the first");
    assert!(p2.file_name().unwrap().to_string_lossy().contains("_1"), "got {p2:?}");
    assert_eq!(std::fs::read(&p2).unwrap(), b"the deed");
    std::fs::remove_dir_all(&root).ok();
    fs::remove_file(&src).ok();
    cleanup(&path);
}

#[test]
fn export_document_into_never_escapes_root_for_adversarial_names() {
    // Security invariant: no matter what the (user-controlled) document filename/location
    // is, the export path stays strictly UNDER `root` and writes a real file. Covers
    // traversal, separators, reserved device names, and unicode.
    let path = tmp_path("exadv");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let root = parent_dir(&path).join("exports");
    let canon_root = {
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    };
    let names = [
        "../../../../etc/passwd",
        "..\\..\\windows\\system32",
        "a/b/c.pdf",
        "con.pdf",
        "nul",
        "  ...  ",
        "deed\u{202e}fdp.exe",
        &"x".repeat(400),
    ];
    for (i, name) in names.iter().enumerate() {
        let src = write_src(&format!("exadv-{i}"), b"payload");
        // The UI sanitizes the filename (records::doc_filename) before add_document, so do
        // that here too; and inject traversal via the LOCATION (a defense-in-depth check
        // that export drops `..`/separators even when the stored virtual path carries them).
        let fname = records::doc_filename(name);
        let loc = format!("/general-documents/../../sneaky/{}", records::doc_slug(name, "fb"));
        let Ok(id) = v.add_document(&loc, &fname, &src) else {
            fs::remove_file(&src).ok();
            continue; // a path rejected at store time is itself a safe outcome
        };
        let dest = v.export_document_into(&id, &root).unwrap();
        let canon_dest = dest.canonicalize().unwrap();
        assert!(canon_dest.starts_with(&canon_root), "escaped root: name={name:?} -> {canon_dest:?}");
        assert_eq!(std::fs::read(&canon_dest).unwrap(), b"payload", "exported content intact for {name:?}");
        fs::remove_file(&src).ok();
    }
    std::fs::remove_dir_all(&root).ok();
    cleanup(&path);
}

#[test]
fn backup_copies_consistent_tree() {
    let path = tmp_path("bkp");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("bk", b"doc body");
    let id = v.add_document("/d", "f.txt", &src).unwrap();
    drop(v);

    let dest = std::env::temp_dir().join(format!("pmbkp-{}", nanos()));
    let backup_vault = backup(&path, &dest).unwrap();
    assert!(backup_vault.exists());

    let reopened = OpenVault::open(backup_vault.clone(), b"a", b"b").unwrap();
    assert_eq!(&*reopened.read_document(&id).unwrap(), b"doc body");
    fs::remove_dir_all(&dest).ok();
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn export_document_is_hardened_and_no_clobber() {
    let path = tmp_path("expdoc");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("exp", b"secret doc");
    let id = v.add_document("/d", "f.txt", &src).unwrap();

    let dest = std::env::temp_dir().join(format!("pmexp-{}.txt", nanos()));
    v.export_document(&id, &dest).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"secret doc");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert!(v.export_document(&id, &dest).is_err(), "no clobber");
    fs::remove_file(&dest).ok();
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn write_export_bytes_creates_dir_hardens_and_never_clobbers() {
    let dir = std::env::temp_dir().join(format!("pmcsv-{}", nanos()));
    assert!(!dir.exists(), "starts from a non-existent dir");
    // Creates the (missing) export dir and writes the file 0600.
    let p1 = write_export_bytes(&dir, "accounts-20240101-000000.csv", b"col\r\na\r\n").unwrap();
    assert!(p1.exists());
    assert_eq!(fs::read(&p1).unwrap(), b"col\r\na\r\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&p1).unwrap().permissions().mode() & 0o777, 0o600, "export file is 0600");
    }
    // Same dir+filename again must NOT clobber — it gets a `_N` suffix and the first
    // file is left byte-for-byte intact.
    let p2 = write_export_bytes(&dir, "accounts-20240101-000000.csv", b"other\r\n").unwrap();
    assert_ne!(p1, p2, "second export does not overwrite the first");
    assert!(p2.file_name().unwrap().to_string_lossy().contains("_1"), "got: {p2:?}");
    assert_eq!(fs::read(&p1).unwrap(), b"col\r\na\r\n", "first export untouched by the second");
    fs::remove_dir_all(&dir).ok();
}

// Names of the `last_update_*` marker files currently in `dir`.
fn markers(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("last_update_").then_some(n)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn save_writes_a_single_last_update_marker_after_commit() {
    let path = tmp_path("lastupd");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let dir = path.parent().unwrap().to_path_buf();
    // create() does its first on-disk commit (open.save()), so a marker already exists —
    // the marker tracks "last time vault.pmv was committed", which includes creation.
    let ms = markers(&dir);
    assert_eq!(ms.len(), 1, "exactly one marker after the create commit: {ms:?}");
    let ts = ms[0].strip_prefix("last_update_").unwrap();
    assert_eq!(ts.len(), 15, "name carries a compact_utc YYYYMMDD-HHMMSS: {}", ms[0]);
    // Contents = the same timestamp (+ newline); name and content agree.
    assert_eq!(fs::read_to_string(dir.join(&ms[0])).unwrap(), format!("{ts}\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(dir.join(&ms[0])).unwrap().permissions().mode() & 0o777, 0o600, "marker is 0600");
    }
    // A subsequent committed save keeps it at exactly one (the old one is replaced).
    v.save().unwrap();
    assert_eq!(markers(&dir).len(), 1, "still exactly one marker after another save: {:?}", markers(&dir));
    cleanup(&path);
}

#[test]
fn save_replaces_any_existing_last_update_markers() {
    let path = tmp_path("lastupd2");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let dir = path.parent().unwrap().to_path_buf();
    v.save().unwrap();
    // Plant a STALE marker (as if from an earlier, different-second save).
    fs::write(dir.join("last_update_19990101-000000"), b"19990101-000000\n").unwrap();
    assert_eq!(markers(&dir).len(), 2, "two markers planted");
    v.save().unwrap();
    let ms = markers(&dir);
    assert_eq!(ms.len(), 1, "exactly one marker remains after the next save: {ms:?}");
    assert!(!ms[0].contains("19990101"), "the stale marker was replaced: {ms:?}");
    cleanup(&path);
}

#[test]
fn read_only_session_does_not_touch_the_last_update_marker() {
    let path = tmp_path("lastupd-ro");
    drop(OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap());
    let dir = path.parent().unwrap().to_path_buf();
    let before = markers(&dir);
    assert_eq!(before.len(), 1, "create left one marker");
    // A read-only (heir) session never saves, so it must not add or replace the marker.
    drop(OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap());
    assert_eq!(markers(&dir), before, "read-only open left the marker unchanged");
    cleanup(&path);
}

#[test]
fn last_update_marker_does_not_break_reopen() {
    let path = tmp_path("lastupd-reopen");
    let id;
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let src = write_src("lu", b"doc bytes");
        id = v.add_document("/d", "f.txt", &src).unwrap(); // add_document persists -> marks
        fs::remove_file(&src).ok();
    }
    let dir = path.parent().unwrap().to_path_buf();
    assert_eq!(markers(&dir).len(), 1, "one marker after add_document: {:?}", markers(&dir));
    // Reopen with the marker sitting in the vault root — must open cleanly, data intact
    // (the marker is ignored by the partition/manifest scanners, which read the subdirs).
    let v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&v.read_document(&id).unwrap()[..], b"doc bytes", "doc survives a reopen-with-marker");
    cleanup(&path);
}

#[test]
fn generation_increments_and_is_surfaced() {
    let path = tmp_path("gen");
    let created = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let g = created.vault.generation;
    assert!(g >= 1);
    drop(created);
    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(reopened.opened_generation(), g);
    assert!(reopened.vault.generation > g);
    cleanup(&path);
}

#[test]
fn read_only_refuses_all_mutations() {
    let path = tmp_path("ro");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
        v.save().unwrap();
    }
    let mut ro = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    assert_eq!(ro.vault.accounts.len(), 1);
    assert!(matches!(ro.save(), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.change_password(b"c", b"d"), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.set_volume_max_size(1024), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.remove_document("x"), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.add_asset_type("X"), Err(VaultError::ReadOnly)));
    let src = write_src("ro", b"x");
    assert!(matches!(ro.add_document("/d", "f", &src), Err(VaultError::ReadOnly)));

    let g_before = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap().vault.generation;
    let _ = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    let g_after = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap().vault.generation;
    assert_eq!(g_before, g_after, "read-only open writes nothing");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn categories_persist_and_read_only_refuses_edits() {
    let path = tmp_path("cats");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    assert!(v.categories().account_type_names().contains(&"Financial".to_string()));
    assert!(v.add_asset_type("Annuity").unwrap());
    assert!(v.add_account_subtype("Financial", "HSA").unwrap());
    drop(v);

    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(reopened.categories().asset.contains(&"Annuity".to_string()));
    assert!(reopened.categories().subtypes_for("Financial").contains(&"HSA".to_string()));
    cleanup(&path);
}

#[test]
fn compact_timestamp_is_filename_safe() {
    assert_eq!(compact_timestamp(1_609_459_200), "20210101-000000");
    assert!(!compact_timestamp(records::unix_now()).contains([':', ' ', '/']));
}

// ---- Phase 5: single-writer lock + partition-filtered export -----------

#[cfg(feature = "single-writer-lock")]
#[test]
fn single_writer_lock_blocks_second_writable_open() {
    let path = tmp_path("lock");
    let v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // A second writable open fails fast while the first session is held.
    assert!(matches!(OpenVault::open(path.clone(), b"a", b"b"), Err(VaultError::Locked)));
    // Read-only opens never take the lock, so they are always allowed.
    assert!(OpenVault::open_read_only(path.clone(), b"a", b"b").is_ok());
    drop(v); // releasing the writer frees the lock (no stale lock file)
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_ok());
    cleanup(&path);
}

/// Complement of the test above for the `single-writer-lock`-off build (the
/// mobile/FFI configuration): `WriteLock::acquire` is a no-op, so a second
/// writable open must succeed rather than fail with `Locked`. This pins the no-op
/// `acquire` under that config (run with `--no-default-features`), and documents
/// that the cargo-mutants survivor on that `#[cfg(not(...))]` line is a phantom —
/// it is dead code in the default (feature-on) build cargo-mutants compiles.
#[cfg(not(feature = "single-writer-lock"))]
#[test]
fn no_op_lock_allows_a_second_writable_open() {
    let path = tmp_path("noop_lock");
    let v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // No cross-process lock is taken, so a second writable open is allowed.
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_ok());
    drop(v);
    cleanup(&path);
}

#[test]
fn export_filters_by_partition() {
    // seed_multi_partition lands one document in each of three partitions.
    let (path, docs) = seed_multi_partition("partfilter", b"a", b"b");
    assert_eq!(docs.len(), 3);
    // All manifests vs a single partition's manifest.
    let all = OpenVault::export_manifests(&path, b"a", b"b", None).unwrap();
    assert_eq!(all.len(), 3, "every partition's entries");
    for p in 0..3u32 {
        let one = OpenVault::export_manifests(&path, b"a", b"b", Some(p)).unwrap();
        assert_eq!(one.len(), 1, "partition {p} holds exactly one doc");
    }
    // Documents filtered by partition decrypt only that one volume.
    let d1 = OpenVault::export_documents(&path, b"a", b"b", Some(1)).unwrap();
    assert_eq!(d1.len(), 1);
    // Out-of-range partitions are rejected for both facilities.
    assert!(matches!(
        OpenVault::export_manifests(&path, b"a", b"b", Some(9)),
        Err(VaultError::NoSuchPartition(9))
    ));
    assert!(matches!(
        OpenVault::export_documents(&path, b"a", b"b", Some(9)),
        Err(VaultError::NoSuchPartition(9))
    ));
    cleanup(&path);
}

#[test]
fn read_facilities_do_not_mutate_the_vault() {
    // decrypt/manifest/extract are read-only: they must not bump the
    // generation or otherwise change the on-disk vault.
    let (path, _docs) = seed_multi_partition("nomutate", b"a", b"b");
    let before = fs::read(&path).unwrap();
    let gen_before = OpenVault::export(&path, b"a", b"b").unwrap().generation;
    let _ = OpenVault::export(&path, b"a", b"b").unwrap();
    let _ = OpenVault::export_manifests(&path, b"a", b"b", None).unwrap();
    let _ = OpenVault::export_documents(&path, b"a", b"b", None).unwrap();
    let after = fs::read(&path).unwrap();
    assert_eq!(before, after, "the vault file is byte-identical after read facilities");
    let gen_after = OpenVault::export(&path, b"a", b"b").unwrap().generation;
    assert_eq!(gen_before, gen_after, "generation unchanged");
    cleanup(&path);
}

#[test]
fn export_then_import_tree_round_trips() {
    // Seed a vault with an account, three docs across partitions, and a record
    // that references one doc (so the consistency check is exercised on import).
    let (path, docs) = seed_multi_partition("exptree", b"o1", b"o2");
    {
        let mut v = OpenVault::open(path.clone(), b"o1", b"o2").unwrap();
        let mut tw = crate::records::TrustWill::new().unwrap();
        tw.file = Some(docs[0].0.clone());
        records::upsert(&mut v.vault.trust_wills, tw);
        v.save().unwrap();
    }
    // Decrypt to a plaintext mirror.
    let mirror = std::env::temp_dir().join(format!("pmmirror-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    assert!(mirror.join("vault.json").exists());
    assert!(mirror.join("manifest/manifest.0.json").exists());
    assert!(mirror.join("volume/vol.0").join(&docs[0].0).exists());

    // Rebuild a fresh encrypted vault from the mirror under NEW passwords.
    let dest_dir = std::env::temp_dir().join(format!("pmimport-{}", nanos()));
    let dest = dest_dir.join(VAULT_FILE);
    drop(OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()).unwrap());

    // Only the new passwords open it; every record and document round-tripped.
    assert!(OpenVault::open(dest.clone(), b"o1", b"o2").is_err(), "old passwords must not work");
    let v = OpenVault::open(dest.clone(), b"n1", b"n2").unwrap();
    assert_eq!(v.vault.accounts.len(), 1);
    assert_eq!(v.vault.trust_wills.len(), 1);
    for (id, body) in &docs {
        assert_eq!(&v.read_document(id).unwrap()[..], &body[..], "doc {id} survives the round-trip");
    }
    // import-tree refuses to overwrite an existing vault.
    assert!(matches!(
        OpenVault::import_tree(&mirror, &dest, b"x", b"y", fast()),
        Err(VaultError::AlreadyExists(_))
    ));

    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(&dest_dir).ok();
    cleanup(&path);
}

#[test]
fn export_tree_writes_documents_tree_and_per_tab_csv() {
    // export_tree also writes a human-browsable documents/<virtual-path> tree (alongside the
    // id-keyed volume/ that import reads) and one CSV per record tab.
    let path = tmp_path("exptree-extras");
    let body = vec![7u8; 200];
    let src = write_src("exptree-extras", &body);
    {
        let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
        let id = v.add_document("general-documents/passport", "scan.pdf", &src).unwrap();
        let mut gd = records::GeneralDocument::new().unwrap();
        gd.title = "Passport".into();
        gd.file = Some(id);
        records::upsert(&mut v.vault.general_documents, gd);
        let mut a = records::Account::new().unwrap();
        a.owner = "Jane".into();
        a.username = "jane".into();
        a.password = "pw".into();
        records::upsert(&mut v.vault.accounts, a);
        v.save().unwrap();
    }
    let mirror = std::env::temp_dir().join(format!("pmmirror-extras-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();

    // Human tree: the document recreated at its virtual path, with the decrypted bytes.
    let tree_file = mirror.join("documents/general-documents/passport/scan.pdf");
    assert!(tree_file.exists(), "documents/ tree file missing: {}", tree_file.display());
    assert_eq!(std::fs::read(&tree_file).unwrap(), body, "tree copy has the decrypted bytes");
    // A CSV per tab; the accounts CSV carries the row.
    for f in ["urgent.csv", "accounts.csv", "general-documents.csv", "assets-liabilities.csv", "real-estate.csv", "taxes.csv", "trust-will.csv", "instructions.csv"] {
        assert!(mirror.join("csv").join(f).exists(), "missing csv/{f}");
    }
    assert!(std::fs::read_to_string(mirror.join("csv/accounts.csv")).unwrap().contains("jane"));
    // The id-keyed volume + manifest (import's canonical round-trip source) are still written.
    assert!(mirror.join("manifest/manifest.0.json").exists());
    assert!(mirror.join("volume/vol.0").exists());

    std::fs::remove_dir_all(&mirror).ok();
    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn export_tree_completes_when_a_cosmetic_human_copy_name_is_too_long() {
    // Audit F2: two documents that legitimately share a ~255-byte single-component
    // filename collide in the human documents/ tree; the second's disambiguated name
    // (`stem_1` or `stem_<id>`) exceeds NAME_MAX, so its create fails ENAMETOOLONG.
    // That COSMETIC copy must be skipped, NOT abort the whole export — the authoritative
    // id-keyed volume/ + manifest + CSV mirror must still complete and round-trip.
    let path = tmp_path("exptree-longname");
    let body1 = vec![1u8; 64];
    let body2 = vec![2u8; 64];
    let src1 = write_src("exptree-longname-1", &body1);
    let src2 = write_src("exptree-longname-2", &body2);
    // A 255-byte single filename component: virtual_path("", name) == "/" + 255 == MAX_PATH_LEN.
    let name = format!("{}.pdf", "a".repeat(storage::MAX_PATH_LEN - 1 - 4));
    assert_eq!(virtual_path("", &name).len(), storage::MAX_PATH_LEN);
    let (id1, id2) = {
        let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
        let id1 = v.add_document("", &name, &src1).unwrap();
        let id2 = v.add_document("", &name, &src2).unwrap(); // legitimately shares the virtual path
        v.save().unwrap();
        (id1, id2)
    };
    let mirror = std::env::temp_dir().join(format!("pmmirror-longname-{}", nanos()));
    // COMPLETES despite the second cosmetic copy's name being too long (pre-F2 this aborted).
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    // Authoritative mirror is complete: both id-keyed blobs + manifest + CSVs present.
    assert!(mirror.join("volume/vol.0").join(&id1).exists(), "id1 blob written");
    assert!(mirror.join("volume/vol.0").join(&id2).exists(), "id2 blob written");
    assert!(mirror.join("manifest/manifest.0.json").exists(), "manifest written");
    assert!(mirror.join("csv/accounts.csv").exists(), "CSVs written (the loop ran to completion)");
    // And it round-trips: both documents restore from the id-keyed mirror under new passwords.
    let dest_dir = std::env::temp_dir().join(format!("pmimport-longname-{}", nanos()));
    let dest = dest_dir.join(VAULT_FILE);
    drop(OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()).unwrap());
    let v = OpenVault::open(dest.clone(), b"n1", b"n2").unwrap();
    assert_eq!(&v.read_document(&id1).unwrap()[..], &body1[..], "doc1 round-trips");
    assert_eq!(&v.read_document(&id2).unwrap()[..], &body2[..], "doc2 round-trips");
    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(&dest_dir).ok();
    let _ = std::fs::remove_file(&src1);
    let _ = std::fs::remove_file(&src2);
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn export_tree_refuses_a_symlinked_out_root() {
    // Audit R4-1: a symlink pre-planted at the export OUT root must not redirect the
    // whole-vault cleartext (vault.json etc.) into the attacker's directory.
    use std::os::unix::fs::symlink;
    let path = tmp_path("exptree-symroot");
    let src = write_src("exptree-symroot", b"doc");
    {
        let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
        v.add_document("general-documents/x", "x.pdf", &src).unwrap();
        let mut a = records::Account::new().unwrap();
        a.owner = "o".into();
        a.username = "u".into();
        a.password = "secret".into();
        records::upsert(&mut v.vault.accounts, a);
        v.save().unwrap();
    }
    let attacker = std::env::temp_dir().join(format!("pmattacker-root-{}", nanos()));
    fs::create_dir_all(&attacker).unwrap();
    let out = std::env::temp_dir().join(format!("pmout-symroot-{}", nanos()));
    symlink(&attacker, &out).unwrap();
    let res = OpenVault::export_tree(&path, b"o1", b"o2", &out);
    assert!(res.is_err(), "export_tree must refuse a symlinked OUT root");
    assert!(!attacker.join("vault.json").exists(), "no cleartext escaped into the symlink target");
    fs::remove_file(&out).ok();
    fs::remove_dir_all(&attacker).ok();
    let _ = fs::remove_file(&src);
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn export_tree_refuses_a_symlinked_subdir() {
    // Audit R4-2: a symlink pre-planted at an AUTHORITATIVE subdir (out/volume) must not
    // redirect decrypted document blobs outside the export root.
    use std::os::unix::fs::symlink;
    let path = tmp_path("exptree-symsub");
    let src = write_src("exptree-symsub", b"docbytes");
    let id = {
        let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
        let id = v.add_document("general-documents/x", "x.pdf", &src).unwrap();
        v.save().unwrap();
        id
    };
    let out = std::env::temp_dir().join(format!("pmout-symsub-{}", nanos()));
    fs::create_dir_all(&out).unwrap();
    let attacker = std::env::temp_dir().join(format!("pmattacker-sub-{}", nanos()));
    fs::create_dir_all(&attacker).unwrap();
    symlink(&attacker, out.join("volume")).unwrap(); // pre-plant out/volume -> attacker
    let res = OpenVault::export_tree(&path, b"o1", b"o2", &out);
    assert!(res.is_err(), "export_tree must refuse a symlinked volume/ subdir");
    assert!(!attacker.join("vol.0").join(&id).exists(), "no blob written through the symlinked subdir");
    fs::remove_dir_all(&out).ok();
    fs::remove_dir_all(&attacker).ok();
    let _ = fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn import_tree_rejects_a_tail_truncated_mirror() {
    // Audit R4-3: a mirror missing its HIGHER (tail) partitions is a clean contiguous prefix
    // the middle-gap guard cannot catch; the recorded partition count makes import fail closed.
    let (path, _docs) = seed_multi_partition("tailtrunc", b"o1", b"o2");
    let mirror = std::env::temp_dir().join(format!("pmtail-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    let hi = highest_mirror_manifest(&mirror.join("manifest")).expect("multi-partition mirror");
    assert!(hi >= 1, "seed must produce >1 partition (highest {hi})");
    // Truncate the tail: remove the highest partition's manifest + volume dir, as an export
    // aborted at a partition boundary would leave. The lower partitions stay a clean prefix.
    fs::remove_file(mirror.join("manifest").join(format!("manifest.{hi}.json"))).unwrap();
    fs::remove_dir_all(mirror.join("volume").join(format!("vol.{hi}"))).unwrap();
    let dest = std::env::temp_dir().join(format!("pmtail-imp-{}", nanos())).join(VAULT_FILE);
    let res = OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast());
    assert!(res.is_err(), "import must refuse a tail-truncated mirror (partition-count mismatch)");
    fs::remove_dir_all(&mirror).ok();
    cleanup(&path);
}

#[test]
fn import_tree_rejects_a_manifest_with_too_many_entries() {
    // Audit R5-1 / round-1 L3: a crafted mirror packing > MAX_MANIFEST_ENTRIES tiny entries
    // into one partition must be rejected (TooLarge) BEFORE the O(M^2) per-entry import loop,
    // not hang for hours.
    let path = tmp_path("import-overcount");
    let src = write_src("import-overcount", b"x");
    {
        let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
        v.add_document("general-documents/x", "x.pdf", &src).unwrap();
        v.save().unwrap();
    }
    let mirror = std::env::temp_dir().join(format!("pmovercount-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    // Overwrite manifest.0.json with one more than the cap of minimal entries (the count
    // check fires before any per-entry id/path validation, so identical dummies are fine).
    let n = storage::MAX_MANIFEST_ENTRIES + 1;
    let entries: Vec<ManifestEntry> = (0..n)
        .map(|_| ManifestEntry { id: "a".into(), path: "/a".into(), size: 1, offset: 0, length: 1, uploaded_at: 0 })
        .collect();
    let man = serde_json::to_vec(&entries).unwrap();
    fs::write(mirror.join("manifest").join("manifest.0.json"), &man).unwrap();
    let dest = std::env::temp_dir().join(format!("pmovercount-imp-{}", nanos())).join(VAULT_FILE);
    // `OpenVault` has no `Debug` (it holds secrets), so match rather than format the Ok arm.
    match OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()) {
        Err(VaultError::Storage(StorageError::TooLarge)) => {}
        Err(e) => panic!("expected TooLarge, got Err({e:?})"),
        Ok(_) => panic!("expected TooLarge, but import SUCCEEDED on an over-count manifest"),
    }
    fs::remove_dir_all(&mirror).ok();
    let _ = fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn import_tree_sanitizes_spoofed_category_names() {
    // import_tree adopts vault.categories WHOLESALE from an UNTRUSTED mirror. add_* never
    // sanitizes, so the source stores RAW spoofed names (simulating a crafted mirror); import
    // must display_safe-neutralize them — the fourth untrusted-category path, now consistent
    // with plan/apply/sync.
    let path = tmp_path("imp-cat-src");
    let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
    v.vault.categories.add_asset_type("Bank\u{202e}x"); // RLO override (stored raw)
    v.vault.categories.add_account_type("Cre\u{200b}dit"); // zero-width space
    v.vault.categories.add_account_subtype("Cre\u{200b}dit", "Vi\u{200c}sa");
    v.save().unwrap();

    let mirror = std::env::temp_dir().join(format!("pmcatmirror-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    let dest_dir = std::env::temp_dir().join(format!("pmcatimp-{}", nanos()));
    let dest = dest_dir.join(VAULT_FILE);
    drop(OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()).unwrap());

    let imported = OpenVault::open(dest.clone(), b"n1", b"n2").unwrap();
    let san_asset = records::display_safe("Bank\u{202e}x");
    let san_type = records::display_safe("Cre\u{200b}dit");
    let san_sub = records::display_safe("Vi\u{200c}sa");
    assert!(imported.vault.categories.asset.iter().any(|x| x.as_str() == san_asset));
    assert!(
        !imported.vault.categories.asset.iter().any(|x| x.as_str().contains('\u{202e}')),
        "raw bidi-spoofed asset type must NOT survive import"
    );
    assert!(
        imported
            .vault
            .categories
            .account
            .iter()
            .any(|a| a.name == san_type && a.subtypes.iter().any(|s| s.as_str() == san_sub)),
        "account type + subtype sanitized on import: {:?}",
        imported.vault.categories.account
    );
    assert!(
        !imported.vault.categories.account.iter().any(|a| a.name.contains('\u{200b}')),
        "raw zero-width-spoofed account type must NOT survive import"
    );

    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(&dest_dir).ok();
    cleanup(&path);
}

#[test]
fn import_tree_rejects_a_non_contiguous_mirror() {
    // A mirror with a missing MIDDLE manifest must FAIL CLOSED, never silently drop the
    // documents in the surviving higher partitions — symmetric with VolumeStore::open's
    // non-contiguous-partition guard. (seed_multi_partition spans partitions 0,1,2.)
    let (path, _docs) = seed_multi_partition("impgap", b"o1", b"o2");
    let mirror = std::env::temp_dir().join(format!("pmgap-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    assert!(mirror.join("manifest/manifest.2.json").exists(), "seed should span >= 3 partitions");
    // Lose the MIDDLE manifest (1); 0 and 2 survive → a gap the loop would silently truncate.
    std::fs::remove_file(mirror.join("manifest/manifest.1.json")).unwrap();
    let dest = std::env::temp_dir().join(format!("pmgapd-{}", nanos())).join(VAULT_FILE);
    assert!(
        matches!(
            OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()),
            Err(VaultError::Storage(StorageError::Corrupt(_)))
        ),
        "a non-contiguous mirror (missing middle manifest) must be rejected"
    );
    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(parent_dir(&dest)).ok();
    cleanup(&path);
}

#[test]
fn import_tree_rejects_oversized_manifest() {
    // A crafted mirror with an oversized manifest must be rejected before the
    // wholesale read (no OOM), like every other manifest-ingest path.
    let (path, _docs) = seed_multi_partition("impbig", b"o1", b"o2");
    let mirror = std::env::temp_dir().join(format!("pmbig-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    {
        // Sparse-extend manifest.0.json past the cap (no real bytes written).
        let f = OpenOptions::new().write(true).open(mirror.join("manifest/manifest.0.json")).unwrap();
        f.set_len(storage::MAX_MANIFEST_SIZE + 1).unwrap();
    }
    let dest = std::env::temp_dir().join(format!("pmbigd-{}", nanos())).join(VAULT_FILE);
    assert!(matches!(
        OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()),
        Err(VaultError::TooLarge)
    ));
    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(parent_dir(&dest)).ok();
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn import_tree_rejects_symlink_blob() {
    // A blob replaced by a symlink (e.g. -> /dev/zero or an arbitrary file) is
    // rejected rather than followed.
    let (path, docs) = seed_multi_partition("impsym", b"o1", b"o2");
    let mirror = std::env::temp_dir().join(format!("pmsym-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    let blob = mirror.join("volume/vol.0").join(&docs[0].0);
    std::fs::remove_file(&blob).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", &blob).unwrap();
    let dest = std::env::temp_dir().join(format!("pmsymd-{}", nanos())).join(VAULT_FILE);
    assert!(matches!(
        OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()),
        Err(VaultError::Storage(_))
    ));
    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(parent_dir(&dest)).ok();
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn import_tree_rejects_symlinked_partition_dir() {
    // A symlinked INTERMEDIATE directory (here vol.0 -> elsewhere) must be rejected:
    // `read_capped`'s O_NOFOLLOW only guards the final component, so without the
    // `reject_symlink_dir` guard a symlinked `vol.<p>/` would redirect blob reads
    // outside the mirror (audit U-2).
    let (path, _docs) = seed_multi_partition("impsymdir", b"o1", b"o2");
    let mirror = std::env::temp_dir().join(format!("pmsymdir-{}", nanos()));
    OpenVault::export_tree(&path, b"o1", b"o2", &mirror).unwrap();
    let vol0 = mirror.join("volume/vol.0");
    let elsewhere = mirror.join("volume/elsewhere");
    std::fs::rename(&vol0, &elsewhere).unwrap(); // move the real partition aside...
    std::os::unix::fs::symlink(&elsewhere, &vol0).unwrap(); // ...and symlink to it
    let dest = std::env::temp_dir().join(format!("pmsymdird-{}", nanos())).join(VAULT_FILE);
    assert!(matches!(
        OpenVault::import_tree(&mirror, &dest, b"n1", b"n2", fast()),
        Err(VaultError::Storage(_))
    ));
    std::fs::remove_dir_all(&mirror).ok();
    std::fs::remove_dir_all(parent_dir(&dest)).ok();
    cleanup(&path);
}

#[test]
fn set_volume_max_size_governs_future_placement() {
    let path = tmp_path("volcfg");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Default cap (256 MiB) is large: two ~30 KiB docs share partition 0.
    let src = write_src("vc", &vec![7u8; 30_000]);
    v.add_document("/a", "a.bin", &src).unwrap();
    v.add_document("/b", "b.bin", &src).unwrap();
    // Shrink the cap live; it persists and updates the running store. A sub-floor request
    // (1024) is clamped UP to MIN_VOLUME_MAX_SIZE (64 KiB) so a tiny cap can't fragment the
    // store into one partition per document. At the 64 KiB floor the two existing ~30 KiB
    // docs still fit partition 0, but a third no longer does.
    v.set_volume_max_size(1024).unwrap();
    assert_eq!(v.volume_max_size(), MIN_VOLUME_MAX_SIZE, "sub-floor cap clamped up to the 64 KiB floor");
    // A further doc now rolls into a fresh partition.
    v.add_document("/c", "c.bin", &src).unwrap();
    drop(v);
    // All three manifest entries survive; the third is in its own partition.
    let p1 = OpenVault::export_manifests(&path, b"a", b"b", Some(1)).unwrap();
    assert_eq!(p1.len(), 1, "the post-resize doc landed in partition 1");
    let all = OpenVault::export_manifests(&path, b"a", b"b", None).unwrap();
    assert_eq!(all.len(), 3);
    // The persisted (clamped) setting is read back on reopen.
    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(reopened.volume_max_size(), MIN_VOLUME_MAX_SIZE);
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn set_volume_max_size_clamps_to_min_and_max() {
    let path = tmp_path("volclamp");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_volume_max_size(1).unwrap(); // absurdly small -> floored
    assert_eq!(v.volume_max_size(), MIN_VOLUME_MAX_SIZE);
    v.set_volume_max_size(u64::MAX).unwrap(); // absurdly large -> capped
    assert_eq!(v.volume_max_size(), MAX_VOLUME_MAX_SIZE);
    cleanup(&path);
}

// ---- Phase 4: exhaustive rekey crash-injection -------------------------
//
// Protocol: stage a complete new-key tree under `.rekey/`, mark it `READY`,
// then commit by roll-forward — move `volume/`, then `manifest/`, then
// `vault.pmv` **last** — and finally delete `.rekey/`. A crash at *any*
// point must leave either the old tree (no `READY`) or the new tree
// (`READY`) fully working, never a mix. Each test below reproduces the
// on-disk state after a crash at one step and asserts recovery on reopen.

/// Seed a vault whose documents span several partitions (one tiny doc each).
/// Returns the vault path plus every stored `(id, body)` for later readback.
fn seed_multi_partition(tag: &str, pw1: &[u8], pw2: &[u8]) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    let path = tmp_path(tag);
    let mut v = OpenVault::create(path.clone(), pw1, pw2, fast()).unwrap();
    v.vault.settings.volume_max_size = 1024; // tiny cap → one doc per partition
    records::upsert(&mut v.vault.accounts, sample_account("user", "secret"));
    v.save().unwrap();
    drop(v); // release the single-writer lock before reopening
    // Reopen so the store picks up the small cap before we add documents.
    let mut v = OpenVault::open(path.clone(), pw1, pw2).unwrap();
    let mut docs = Vec::new();
    for i in 0..3u8 {
        let body = vec![i + 1; 600];
        let src = write_src(&format!("{tag}-{i}"), &body);
        let id = v.add_document(&format!("/dir{i}"), &format!("f{i}.bin"), &src).unwrap();
        fs::remove_file(&src).ok();
        docs.push((id, body));
    }
    drop(v);
    (path, docs)
}

/// Build a complete, `READY`-marked staging tree under `<dir>/.rekey`,
/// re-encrypting the live vault + every blob under the new passwords —
/// exactly like `change_password`, but stopping **before** the commit.
fn stage_ready_rekey(path: &Path, old1: &[u8], old2: &[u8], new1: &[u8], new2: &[u8]) -> PathBuf {
    let open = OpenVault::open(path.to_path_buf(), old1, old2).unwrap();
    let dir = parent_dir(path);
    let staging = dir.join(REKEY_DIR);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).unwrap();
    let new_salt = crypto::random_bytes::<SALT_LEN>().unwrap();
    let new_key = crypto::derive_key_chained(new1, new2, &new_salt, &open.params).unwrap();
    let mut new_store =
        VolumeStore::open(&staging, &new_key, &open.vault.id, open.vault.settings.volume_max_size).unwrap();
    let ids: Vec<String> = open.storage.ids().map(|s| s.to_string()).collect();
    for id in &ids {
        let bytes = open.storage.read(id, &open.key).unwrap();
        let (vpath, uploaded_at) =
            open.storage.entry(id).map(|e| (e.path.clone(), e.uploaded_at)).unwrap_or_default();
        new_store.put(id, &vpath, &bytes, uploaded_at, &new_key).unwrap();
    }
    drop(new_store);
    let mut staged_vault = open.vault.clone();
    staged_vault.audit.push(Change::new("password_changed", String::new()));
    write_vault_file(&staging.join(VAULT_FILE), &staged_vault, &new_key, &new_salt, open.params).unwrap();
    write_new_bytes(&staging.join(REKEY_READY), b"ready").unwrap();
    staging
}

/// After a roll-forward: only the NEW passwords open the vault, every doc
/// reads back, the audit records the change, and no staging/`.old` debris
/// is left behind.
fn assert_rolled_forward(path: &Path, old: (&[u8], &[u8]), new: (&[u8], &[u8]), docs: &[(String, Vec<u8>)]) {
    assert!(OpenVault::open(path.to_path_buf(), old.0, old.1).is_err(), "old passwords must fail");
    let v = OpenVault::open(path.to_path_buf(), new.0, new.1).unwrap();
    for (id, body) in docs {
        assert_eq!(&v.read_document(id).unwrap()[..], &body[..], "doc {id} survives rekey");
    }
    assert!(v.vault.audit.iter().any(|c| c.action == "password_changed"), "audit records rekey");
    let dir = parent_dir(path);
    assert!(!dir.join(REKEY_DIR).exists(), "staging removed");
    assert!(!sibling_old(&dir.join("volume")).exists(), "no .volume.old debris");
    assert!(!sibling_old(&dir.join("manifest")).exists(), "no .manifest.old debris");
}

/// After a discard: only the OLD passwords open the vault, every doc reads
/// back unchanged, and staging is gone.
fn assert_discarded(path: &Path, old: (&[u8], &[u8]), new: (&[u8], &[u8]), docs: &[(String, Vec<u8>)]) {
    // The first open (any password) runs recovery and discards the staging.
    assert!(OpenVault::open(path.to_path_buf(), new.0, new.1).is_err(), "new passwords must fail");
    let v = OpenVault::open(path.to_path_buf(), old.0, old.1).unwrap();
    for (id, body) in docs {
        assert_eq!(&v.read_document(id).unwrap()[..], &body[..], "doc {id} intact after discard");
    }
    assert!(!parent_dir(path).join(REKEY_DIR).exists(), "staging discarded");
}

#[test]
fn rekey_across_partitions_roundtrip() {
    let (path, docs) = seed_multi_partition("rkmulti", b"o1", b"o2");
    {
        let mut v = OpenVault::open(path.clone(), b"o1", b"o2").unwrap();
        v.change_password(b"n1", b"n2").unwrap();
        // The in-memory handle is already on the new key.
        for (id, body) in &docs {
            assert_eq!(&v.read_document(id).unwrap()[..], &body[..]);
        }
    }
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_chained_twice_only_last_password_opens() {
    let (path, docs) = seed_multi_partition("rkchain", b"a1", b"a2");
    {
        let mut v = OpenVault::open(path.clone(), b"a1", b"a2").unwrap();
        v.change_password(b"b1", b"b2").unwrap();
        v.change_password(b"c1", b"c2").unwrap();
    }
    assert!(OpenVault::open(path.clone(), b"a1", b"a2").is_err());
    assert!(OpenVault::open(path.clone(), b"b1", b"b2").is_err());
    let v = OpenVault::open(path.clone(), b"c1", b"c2").unwrap();
    for (id, body) in &docs {
        assert_eq!(&v.read_document(id).unwrap()[..], &body[..]);
    }
    cleanup(&path);
}

#[test]
fn rekey_crash_before_any_commit_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkp0", b"o1", b"o2");
    // Crash right after READY, before a single item is moved.
    stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_crash_after_volume_commit_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkp1", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    // Volume moved into place; manifest + vault.pmv still staged.
    replace_dir(&dir.join("volume"), &staging.join("volume")).unwrap();
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_crash_after_manifest_commit_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkp2", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    // Volume + manifest moved; vault.pmv still staged → old key still on disk.
    replace_dir(&dir.join("volume"), &staging.join("volume")).unwrap();
    replace_dir(&dir.join("manifest"), &staging.join("manifest")).unwrap();
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_crash_after_vault_commit_before_cleanup_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkp3", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    // Everything moved (new key is live), but `.rekey/` not yet removed.
    replace_dir(&dir.join("volume"), &staging.join("volume")).unwrap();
    replace_dir(&dir.join("manifest"), &staging.join("manifest")).unwrap();
    replace_path(&dir.join(VAULT_FILE), &staging.join(VAULT_FILE)).unwrap();
    assert!(staging.exists(), "staging still present at this crash point");
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn replace_dir_sweeps_leftover_old_when_staged_already_gone() {
    // Regression: a crash AFTER `rename(staged, live)` but BEFORE the `.old`
    // cleanup leaves the OLD-key-encrypted dir behind. Recovery re-enters
    // replace_dir with `staged` already gone; it must still sweep `.<name>.old`
    // (not skip cleanup via the early return) or that old-key ciphertext leaks
    // on disk forever, defeating change_password's forward secrecy.
    let base = std::env::temp_dir().join(format!("pmreplace-{}", nanos()));
    let live = base.join("volume");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("current"), b"new-key data").unwrap();
    // Simulate the leaked old-key dir from the crash window.
    let old = sibling_old(&live);
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("blob"), b"OLD-KEY CIPHERTEXT").unwrap();
    // `staged` does not exist (it was already renamed into place before the crash).
    let staged = base.join(".rekey").join("volume");

    replace_dir(&live, &staged).unwrap();

    assert!(!old.exists(), "leftover .volume.old (old-key data) must be swept on recovery");
    assert!(live.exists() && live.join("current").exists(), "live dir is left intact");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn rekey_crash_mid_volume_swap_rolls_forward() {
    // The dangerous window inside replace_dir: the live dir has been moved
    // aside to `.volume.old` but the staged dir is not yet renamed in.
    let (path, docs) = seed_multi_partition("rkmidv", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    let old = sibling_old(&dir.join("volume"));
    fs::rename(dir.join("volume"), &old).unwrap(); // crash here: live gone, staged intact
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    let _ = &staging;
    cleanup(&path);
}

#[test]
fn rekey_crash_mid_manifest_swap_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkmidm", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    // Volume already committed; crash mid-manifest swap.
    replace_dir(&dir.join("volume"), &staging.join("volume")).unwrap();
    let old = sibling_old(&dir.join("manifest"));
    fs::rename(dir.join("manifest"), &old).unwrap();
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_commit_is_idempotent() {
    // Running the roll-forward twice (e.g. a crash during the second
    // recovery) must not panic or corrupt state.
    let (path, docs) = seed_multi_partition("rkidem", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    let dir = parent_dir(&path);
    commit_rekey(&dir, &staging).unwrap();
    commit_rekey(&dir, &staging).unwrap(); // no-op the second time
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_complete_tree_without_ready_is_discarded() {
    // A fully-staged tree missing only the READY marker is NOT trusted: it
    // is discarded and the old (intact) tree stands.
    let (path, docs) = seed_multi_partition("rknoready", b"o1", b"o2");
    let staging = stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    fs::remove_file(staging.join(REKEY_READY)).unwrap(); // the only thing missing
    assert_discarded(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn rekey_partial_staging_with_docs_discarded() {
    let (path, docs) = seed_multi_partition("rkpartial", b"o1", b"o2");
    let staging = parent_dir(&path).join(REKEY_DIR);
    fs::create_dir_all(staging.join("volume")).unwrap();
    fs::write(staging.join("vault.pmv"), b"half-written").unwrap();
    assert_discarded(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn read_only_with_ready_rekey_is_reported_then_rw_rolls_forward() {
    let (path, docs) = seed_multi_partition("rkro", b"o1", b"o2");
    stage_ready_rekey(&path, b"o1", b"o2", b"n1", b"n2");
    // Read-only cannot finish the commit, so it must refuse, untouched.
    let err = OpenVault::open_read_only(path.clone(), b"n1", b"n2").err().unwrap();
    assert!(matches!(err, VaultError::RekeyPending));
    assert!(parent_dir(&path).join(REKEY_DIR).exists(), "read-only left staging in place");
    // A read-write open then completes the roll-forward.
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn stale_staging_cleared_then_rekey_succeeds() {
    // A leftover incomplete `.rekey/` from a prior aborted attempt must not
    // block a fresh password change.
    let (path, docs) = seed_multi_partition("rkstale", b"o1", b"o2");
    let staging = parent_dir(&path).join(REKEY_DIR);
    fs::create_dir_all(staging.join("volume")).unwrap(); // stale, no READY
    {
        // Open discards the stale staging, then change_password stages anew.
        let mut v = OpenVault::open(path.clone(), b"o1", b"o2").unwrap();
        v.change_password(b"n1", b"n2").unwrap();
    }
    assert_rolled_forward(&path, (b"o1", b"o2"), (b"n1", b"n2"), &docs);
    cleanup(&path);
}

#[test]
fn oversized_vault_file_is_rejected() {
    let path = tmp_path("toobig");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Sparse-extend vault.pmv beyond the cap (no real bytes written); the
    // metadata-size guard must reject it before the wholesale read.
    {
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(MAX_VAULT_SIZE + 1).unwrap();
    }
    assert!(matches!(OpenVault::open(path.clone(), b"a", b"b"), Err(VaultError::TooLarge)));
    cleanup(&path);
}

#[test]
fn orphaned_blob_after_unlink_save_opens_cleanly() {
    // The fixed delete/detach order saves the unlinked vault FIRST, then drops
    // the blob. A crash in that window leaves an orphaned blob (harmless) but no
    // dangling reference; this reproduces that state and asserts a clean reopen.
    let path = tmp_path("orphan");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("orphan", b"body");
    let id = v.add_document("/d", "f.txt", &src).unwrap();
    let mut tw = crate::records::TrustWill::new().unwrap();
    tw.file = Some(id.clone());
    records::upsert(&mut v.vault.trust_wills, tw.clone());
    v.save().unwrap();
    // Unlink the record and save (the blob is still present == orphan).
    tw.file = None;
    records::upsert(&mut v.vault.trust_wills, tw);
    v.save().unwrap();
    drop(v); // simulate a crash before the blob reclaim runs
    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(reopened.has_document(&id), "orphan blob lingers but is harmless");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn pending_rekey_blocks_read_facilities_and_backup() {
    let (path, _docs) = seed_multi_partition("rkblock", b"a", b"b");
    stage_ready_rekey(&path, b"a", b"b", b"n1", b"n2"); // a complete READY staging
    assert!(matches!(
        OpenVault::export_documents(&path, b"a", b"b", None),
        Err(VaultError::RekeyPending)
    ));
    assert!(matches!(
        OpenVault::export_manifests(&path, b"a", b"b", None),
        Err(VaultError::RekeyPending)
    ));
    let dest = std::env::temp_dir().join(format!("pmbk-{}", nanos()));
    assert!(matches!(backup(&path, &dest), Err(VaultError::RekeyPending)));
    let _ = fs::remove_dir_all(&dest);
    cleanup(&path);
}

// Property-based testing: instead of fixed inputs, `proptest!` generates many
// random inputs matching the given specs (the `in "regex"` strings) and checks
// the `prop_assert!` invariants hold for all of them. `prelude::*` imports its
// common names with a single glob.
// ---- Full-disk (ENOSPC) fault injection (cargo test --features fault-injection) ----

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_on_save_keeps_old_vault() {
    let path = tmp_path("enospc-save");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        records::upsert(&mut v.vault.accounts, sample_account("u", "p1"));
        v.save().unwrap();
        // The disk fills on the next save; the prior vault.pmv must survive.
        v.vault.accounts[0].password = "p2".into();
        crate::fault::fail_at("vault.write", 1);
        assert!(matches!(v.save(), Err(VaultError::Io(_))));
        crate::fault::clear();
    }
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(re.vault.accounts[0].password, "p1", "old vault.pmv intact after a failed save");
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_during_rekey_discards_staging_and_old_passwords_work() {
    let path = tmp_path("enospc-rekey");
    let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
    let src = write_src("rk", b"will body");
    let id = v.add_document("/w", "w.txt", &src).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    v.save().unwrap();
    // The disk fills while re-encrypting documents into the .rekey staging tree,
    // BEFORE the READY marker is written.
    crate::fault::fail_at("volume.write", 1);
    let err = v.change_password(b"n1", b"n2").unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, VaultError::Storage(_)), "rekey staging fails cleanly, got {err:?}");
    drop(v); // release the lock before reopening
    // No READY was written, so the staging is discarded on reopen: the OLD
    // passwords still open the intact vault; the new ones do not.
    assert!(OpenVault::open(path.clone(), b"n1", b"n2").is_err());
    let re = OpenVault::open(path.clone(), b"o1", b"o2").unwrap();
    assert_eq!(re.vault.accounts.len(), 1);
    assert_eq!(&*re.read_document(&id).unwrap(), b"will body");
    assert!(!parent_dir(&path).join(REKEY_DIR).exists(), "staging discarded");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_at_rekey_manifest_commit_discards_staging() {
    // Same as above but the disk fills at the staged MANIFEST commit (after the
    // staged volume append) rather than the volume write — still no READY, so
    // the staging is discarded and the old vault stands intact.
    let path = tmp_path("enospc-rekey2");
    let mut v = OpenVault::create(path.clone(), b"o1", b"o2", fast()).unwrap();
    let src = write_src("rk2", b"trust body");
    let id = v.add_document("/t", "t.txt", &src).unwrap();
    v.save().unwrap();
    crate::fault::fail_at("atomic.write", 1);
    let err = v.change_password(b"n1", b"n2").unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, VaultError::Storage(_)), "got {err:?}");
    drop(v);
    assert!(OpenVault::open(path.clone(), b"n1", b"n2").is_err());
    let re = OpenVault::open(path.clone(), b"o1", b"o2").unwrap();
    assert_eq!(&*re.read_document(&id).unwrap(), b"trust body");
    assert!(!parent_dir(&path).join(REKEY_DIR).exists(), "staging discarded");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

// ---- Compaction --------------------------------------------------------

/// A vault with one live, record-referenced document plus `garbage` extra
/// documents that are added then immediately removed, leaving dead frames in
/// the volume. Returns the vault path and the id of the live ("keep") doc.
fn seed_with_garbage(tag: &str, garbage: usize) -> (PathBuf, String) {
    let path = tmp_path(tag);
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src(tag, &vec![9u8; 400]);
    let keep = v.add_document("/keep", "keep.bin", &src).unwrap();
    let mut tw = records::TrustWill::new().unwrap();
    tw.file = Some(keep.clone());
    records::upsert(&mut v.vault.trust_wills, tw);
    // Dead frames: add then remove (drops the manifest entry; frame lingers).
    for i in 0..garbage {
        let id = v.add_document("/g", &format!("g{i}.bin"), &src).unwrap();
        v.remove_document(&id).unwrap();
    }
    v.save().unwrap();
    fs::remove_file(&src).ok();
    (path, keep)
}

fn volume_opts() -> CompactOptions {
    CompactOptions { volume: true, json: false, history_cutoff: None, drop_all_history: false }
}

#[test]
fn compact_volume_reclaims_garbage_and_keeps_live_docs() {
    let (path, keep) = seed_with_garbage("cvol", 3);
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    let opts = volume_opts();
    let before = v.compact_dry_run(&opts).bytes_reclaimed;
    assert!(before > 0, "garbage should be reclaimable, got {before}");
    let report = v.compact(&opts).unwrap();
    assert_eq!(report.bytes_reclaimed, before);
    // No garbage remains, and the live doc is still readable.
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "garbage fully reclaimed");
    assert_eq!(&*v.read_document(&keep).unwrap(), &vec![9u8; 400][..]);
    assert!(v.vault.audit.iter().any(|c| c.action == "compacted"));
    drop(v);
    // Reopens cleanly (consistency check passes), doc intact, no staging debris.
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![9u8; 400][..]);
    assert!(!parent_dir(&path).join(REKEY_DIR).exists());
    cleanup(&path);
}

#[test]
fn compact_volume_keeps_tax_documents() {
    // Regression guard: a Taxes filing's documents must be treated as live by
    // referenced_doc_ids, so `compact --volume` never reclaims them.
    let path = tmp_path("cvoltax");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("cvoltax", &vec![7u8; 300]);
    let keep = v.add_document("taxes/2024", "w2.pdf", &src).unwrap();
    let mut t = records::TaxFiling::new().unwrap();
    t.year = "2024".into();
    t.documents.push(keep.clone());
    records::upsert(&mut v.vault.tax_filings, t);
    // Dead frames so compaction actually has work to do around the live doc.
    for i in 0..3 {
        let id = v.add_document("/g", &format!("g{i}.bin"), &src).unwrap();
        v.remove_document(&id).unwrap();
    }
    v.save().unwrap();
    fs::remove_file(&src).ok();
    drop(v); // release the single-writer lock before reopening

    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    v.compact(&volume_opts()).unwrap();
    assert_eq!(&*v.read_document(&keep).unwrap(), &vec![7u8; 300][..], "tax doc survives compaction");
    drop(v);
    // And the vault still opens (the referenced ⊆ stored consistency holds).
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![7u8; 300][..]);
    cleanup(&path);
}

#[test]
fn compact_volume_keeps_real_estate_documents() {
    // Regression guard: a property's documents must be treated as live by
    // referenced_doc_ids, so `compact --volume` never reclaims them.
    let path = tmp_path("cvolre");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("cvolre", &vec![5u8; 320]);
    let keep = v.add_document("real-estate/123mainst", "deed.pdf", &src).unwrap();
    let mut re = records::RealEstate::new().unwrap();
    re.address = "123 Main St".into();
    re.documents.push(keep.clone());
    records::upsert(&mut v.vault.real_estate, re);
    for i in 0..3 {
        let id = v.add_document("/g", &format!("g{i}.bin"), &src).unwrap();
        v.remove_document(&id).unwrap();
    }
    v.save().unwrap();
    fs::remove_file(&src).ok();
    drop(v);

    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    v.compact(&volume_opts()).unwrap();
    assert_eq!(&*v.read_document(&keep).unwrap(), &vec![5u8; 320][..], "RE doc survives compaction");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![5u8; 320][..]);
    cleanup(&path);
}

#[test]
fn compact_volume_when_all_docs_deleted_shrinks_to_nothing() {
    // Maximum-garbage case: every document removed. The staged store has zero
    // partitions, so compaction must still swap the garbage volume/manifest out
    // (regression guard for the all-deleted reclaim fix in `staged_rewrite`).
    let (path, docs) = seed_multi_partition("calldel", b"a", b"b");
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    for (id, _) in &docs {
        v.remove_document(id).unwrap();
    }
    let opts = volume_opts();
    assert!(v.compact_dry_run(&opts).bytes_reclaimed > 0);
    v.compact(&opts).unwrap();
    assert_eq!(v.compact_dry_run(&opts).bytes_reclaimed, 0, "all garbage gone");
    assert_eq!(v.storage.partition_count(), 0, "empty store after all-deleted compaction");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(re.storage.partition_count(), 0);
    cleanup(&path);
}

#[test]
fn compact_json_trims_history_by_cutoff_and_keeps_audit() {
    let path = tmp_path("cjson");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    // Controlled history: one old, one recent entry (plus the upsert "created").
    v.vault.accounts[0].history.push(records::Change { at: 1_000, action: "updated".into(), detail: "old".into() });
    v.vault.accounts[0].history.push(records::Change { at: 9_000, action: "updated".into(), detail: "newer".into() });
    v.save().unwrap();
    let audit_before = v.vault.audit.len();
    let opts = CompactOptions { volume: false, json: true, history_cutoff: Some(3_000), drop_all_history: false };
    let removed = v.compact(&opts).unwrap().history_removed;
    assert_eq!(removed, 1, "only the at=1000 entry is older than the cutoff");
    // The old entry is gone; the recent one (and the created one) remain.
    assert!(v.vault.accounts[0].history.iter().all(|c| c.at >= 3_000));
    assert!(v.vault.accounts[0].history.iter().any(|c| c.at == 9_000));
    // Audit preserved and gained exactly the compaction event.
    assert_eq!(v.vault.audit.len(), audit_before + 1);
    assert!(v.vault.audit.iter().any(|c| c.action == "compacted"));
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.vault.accounts[0].history.iter().all(|c| c.at >= 3_000));
    cleanup(&path);
}

#[test]
fn compact_json_drop_all_clears_history_only() {
    let path = tmp_path("cjsonall");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    v.vault.accounts[0].history.push(records::Change { at: 1, action: "updated".into(), detail: "x".into() });
    v.save().unwrap();
    let opts = CompactOptions { volume: false, json: true, history_cutoff: None, drop_all_history: true };
    v.compact(&opts).unwrap();
    assert!(v.vault.accounts[0].history.is_empty(), "all record history dropped");
    assert!(v.vault.audit.iter().any(|c| c.action == "compacted"), "audit retained");
    cleanup(&path);
}

#[test]
fn compact_both_reclaims_and_trims_in_one_commit() {
    let (path, keep) = seed_with_garbage("cboth", 2);
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    v.vault.accounts[0].history.push(records::Change { at: 1, action: "updated".into(), detail: "old".into() });
    v.save().unwrap();
    let opts = CompactOptions { volume: true, json: true, history_cutoff: None, drop_all_history: true };
    let report = v.compact(&opts).unwrap();
    assert!(report.bytes_reclaimed > 0 && report.history_removed >= 1);
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![9u8; 400][..]);
    assert!(re.vault.accounts.iter().all(|a| a.history.is_empty()));
    cleanup(&path);
}

#[test]
fn compact_on_clean_vault_is_a_safe_noop_rewrite() {
    let path = tmp_path("cclean");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("cc", b"body");
    let id = v.add_document("/d", "d.bin", &src).unwrap();
    let mut tw = records::TrustWill::new().unwrap();
    tw.file = Some(id.clone());
    records::upsert(&mut v.vault.trust_wills, tw);
    v.save().unwrap();
    fs::remove_file(&src).ok();
    let report = v.compact(&volume_opts()).unwrap();
    assert_eq!(report.bytes_reclaimed, 0, "nothing to reclaim on a clean vault");
    assert_eq!(&*v.read_document(&id).unwrap(), b"body", "doc intact after no-op rewrite");
    cleanup(&path);
}

#[test]
fn compact_refused_on_read_only_handle() {
    let path = tmp_path("cro");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut ro = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    assert!(matches!(ro.compact(&volume_opts()), Err(VaultError::ReadOnly)));
    cleanup(&path);
}

#[test]
fn compact_bumps_write_generation() {
    let (path, _keep) = seed_with_garbage("cgen", 1);
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    let before = v.vault.generation;
    v.compact(&volume_opts()).unwrap();
    assert!(v.vault.generation > before, "compaction advances the generation");
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_during_compact_staging_leaves_original_tree_intact() {
    // The disk fills while re-encrypting into the .compact staging tree, BEFORE
    // READY. The compaction fails cleanly, the handle is poisoned, and the
    // ORIGINAL (uncompacted) vault still opens with its live doc intact.
    let (path, keep) = seed_with_garbage("cenospc", 2);
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    crate::fault::fail_at("volume.write", 1);
    let err = v.compact(&volume_opts()).unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, VaultError::Storage(_)), "got {err:?}");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert_eq!(&*re.read_document(&keep).unwrap(), &vec![9u8; 400][..]);
    assert!(!parent_dir(&path).join(REKEY_DIR).exists(), "staging discarded");
    cleanup(&path);
}

#[test]
fn compact_json_only_leaves_volume_garbage_untouched() {
    // JSON-only compaction must not rewrite the volume: the dead bytes stay.
    let (path, _keep) = seed_with_garbage("cjvol", 2);
    let mut v = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    v.vault.accounts[0].history.push(records::Change { at: 1, action: "u".into(), detail: String::new() });
    v.save().unwrap();
    let before = v.compact_dry_run(&volume_opts()).bytes_reclaimed;
    assert!(before > 0, "there should be reclaimable volume garbage");
    let opts = CompactOptions { volume: false, json: true, history_cutoff: None, drop_all_history: true };
    let report = v.compact(&opts).unwrap();
    assert_eq!(report.bytes_reclaimed, 0, "json-only reclaims no volume bytes");
    assert!(report.history_removed >= 1);
    // The volume garbage is exactly as before — untouched by a json-only run.
    assert_eq!(v.compact_dry_run(&volume_opts()).bytes_reclaimed, before);
    cleanup(&path);
}

#[test]
fn compact_preserves_unreferenced_orphan_blobs() {
    // Compaction copies every live manifest entry (storage.ids()), so an
    // unreferenced orphan blob is conservatively kept (never silently dropped),
    // while genuinely dead frames (removed) are reclaimed.
    let path = tmp_path("corphan");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("co", &vec![3u8; 300]);
    let referenced = v.add_document("/r", "r.bin", &src).unwrap();
    let mut tw = records::TrustWill::new().unwrap();
    tw.file = Some(referenced.clone());
    records::upsert(&mut v.vault.trust_wills, tw);
    let orphan = v.add_document("/o", "o.bin", &src).unwrap(); // never linked → orphan
    let garbage = v.add_document("/g", "g.bin", &src).unwrap();
    v.remove_document(&garbage).unwrap(); // dead frame
    v.save().unwrap();
    fs::remove_file(&src).ok();

    v.compact(&volume_opts()).unwrap();
    assert!(v.has_document(&referenced));
    assert!(v.has_document(&orphan), "unreferenced orphan is preserved by compaction");
    assert!(!v.has_document(&garbage), "removed doc's frame is reclaimed");
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.has_document(&referenced) && re.has_document(&orphan));
    cleanup(&path);
}

#[test]
fn deleted_document_stays_deleted_after_manifest_loss_and_compact() {
    // R-2: a `remove_document`'d blob must not be resurrected by a manifest-loss
    // rebuild (which re-scans the volume) and must not be baked back in by compact.
    let path = tmp_path("r2tomb");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let src = write_src("r2", &vec![7u8; 300]);
    let id = v.add_document("/secret", "will.pdf", &src).unwrap();
    let mut tw = records::TrustWill::new().unwrap();
    tw.file = Some(id.clone());
    let tw_id = tw.id.clone();
    records::upsert(&mut v.vault.trust_wills, tw);
    v.save().unwrap();
    // Delete it the way the UI does: detach from the record AND remove the blob.
    if let Some(t) = v.vault.trust_wills.iter_mut().find(|t| t.id == tw_id) {
        t.file = None;
    }
    v.remove_document(&id).unwrap();
    v.save().unwrap();
    assert!(!v.has_document(&id), "deleted");
    // Attacker deletes the partition manifest (encrypted `manifest.0`, no
    // extension), forcing a volume-scan rebuild on the next open.
    fs::remove_file(parent_dir(&path).join("manifest").join("manifest.0")).unwrap();
    drop(v);
    let mut v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(!v2.has_document(&id), "tombstone suppresses the resurrected frame");
    assert!(v2.read_document(&id).is_err(), "resurrected deleted doc is not readable");
    v2.compact(&volume_opts()).unwrap();
    assert!(!v2.has_document(&id), "compact dropped the tombstoned frame for good");
    drop(v2);
    // After the rewrite the tombstone set is cleared (the frame is physically gone).
    let v3 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(v3.vault.deleted_docs.is_empty(), "tombstones cleared after volume rewrite");
    cleanup(&path);
    fs::remove_file(&src).ok();
}

#[test]
fn import_tree_rejects_duplicate_blob_id_in_mirror() {
    // R-8: a mirror listing the same id twice would leave two frames for one id,
    // enabling a later truncation to roll the document back to a stale version.
    let src = tmp_src("dupid");
    fs::create_dir_all(src.join("manifest")).unwrap();
    fs::create_dir_all(src.join("volume").join("vol.0")).unwrap();
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = "abcd1234".into();
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let id = "aa".repeat(16); // 32 lowercase hex
    let entries = serde_json::json!([
        {"id": id, "path": "x/a", "size": 1, "offset": 0, "length": 1, "uploaded_at": 0},
        {"id": id, "path": "x/b", "size": 1, "offset": 1, "length": 1, "uploaded_at": 0},
    ]);
    fs::write(src.join("manifest").join("manifest.0.json"), serde_json::to_vec(&entries).unwrap()).unwrap();
    fs::write(src.join("volume").join("vol.0").join(&id), b"x").unwrap();
    let dest = tmp_path("impdup");
    let res = OpenVault::import_tree(&src, &dest, b"a", b"b", fast());
    assert!(res.is_err(), "duplicate blob id in the mirror manifest must be rejected");
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(parent_dir(&dest));
}

#[test]
fn open_writable_session_can_back_up_without_self_deadlock() {
    // R-9 regression: an OPEN writable session backs up via OpenVault::backup
    // (reusing its held lock). The free `backup` would self-deadlock here because
    // flock binds to the open file description (a second in-process acquire blocks).
    let path = tmp_path("r9backup");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("u", "p"));
    v.save().unwrap();
    let dest = std::env::temp_dir().join(format!("pmbk-r9-{}", nanos()));
    // The method works while the session is open...
    let bp = v.backup(&dest).expect("OpenVault::backup succeeds while the session holds the lock");
    assert!(bp.exists());
    // ...whereas the free function self-deadlocks (the regression we fixed).
    //
    // Only meaningful WITH the lock feature. Without it `WriteLock::acquire` is an
    // unconditional `Ok`, so the free function simply succeeds and there is no
    // self-deadlock to assert. This assertion was ungated, so the whole test failed
    // under `--no-default-features` — the configuration the mobile and static-musl
    // builds use, and one CI never runs (every CI invocation is `--workspace`, which
    // unifies features and turns the lock back on).
    #[cfg(feature = "single-writer-lock")]
    assert!(matches!(backup(&path, &dest), Err(VaultError::Locked)), "free backup is Locked while a session is open");
    // The produced backup actually opens.
    OpenVault::open(bp.clone(), b"a", b"b").expect("backup is a valid, openable vault");
    drop(v);
    cleanup(&path);
    let _ = fs::remove_dir_all(&dest);
}

// --- Category deletion (asset/account types + subtypes) ------------------

#[test]
fn remove_asset_type_blocks_when_in_use_then_allows_when_free() {
    let path = tmp_path("rmasset");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    assert!(v.add_asset_type("Crypto").unwrap());
    // Used by a live asset -> refused with the count.
    let mut al = records::AssetLiability::new().unwrap();
    al.asset_type = "Crypto".into();
    let al_id = al.id.clone();
    records::upsert(&mut v.vault.assets, al);
    v.save().unwrap();
    assert_eq!(v.remove_asset_type("Crypto").unwrap(), CategoryRemoval::InUse(1));
    assert!(v.categories().asset.contains(&"Crypto".to_string()), "kept while in use");
    // Remove the using record -> now deletable; persists across reopen.
    records::remove(&mut v.vault.assets, &al_id, &mut v.vault.audit, "Asset");
    v.save().unwrap();
    assert_eq!(v.remove_asset_type("crypto").unwrap(), CategoryRemoval::Removed); // case-insensitive
    assert_eq!(v.remove_asset_type("Crypto").unwrap(), CategoryRemoval::NotFound);
    drop(v);
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(!re.categories().asset.contains(&"Crypto".to_string()), "deletion persisted");
    cleanup(&path);
}

#[test]
fn remove_account_type_blocks_on_subtypes_then_on_use_then_allows() {
    let path = tmp_path("rmacct");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.add_account_type("Bank").unwrap();
    v.add_account_subtype("Bank", "Checking").unwrap();
    // (1) Has a subtype -> blocked (delete subtypes first).
    assert_eq!(v.remove_account_type("Bank").unwrap(), CategoryRemoval::HasSubtypes);
    // (2) A live account uses the subtype -> the subtype can't go yet.
    let mut acc = sample_account("u", "p");
    acc.account_type = "Bank".into();
    acc.account_subtype = "Checking".into();
    let acc_id = acc.id.clone();
    records::upsert(&mut v.vault.accounts, acc);
    v.save().unwrap();
    assert_eq!(v.remove_account_subtype("Bank", "Checking").unwrap(), CategoryRemoval::InUse(1));
    // Move the account off the subtype (history will record the change, but that
    // must NOT block deletion) -> subtype now free.
    let mut edited = v.vault.accounts.iter().find(|a| a.id == acc_id).unwrap().clone();
    edited.account_subtype = String::new();
    records::upsert(&mut v.vault.accounts, edited);
    v.save().unwrap();
    assert_eq!(v.remove_account_subtype("Bank", "Checking").unwrap(), CategoryRemoval::Removed);
    // (3) Type now has no subtypes but the account still uses the TYPE -> InUse.
    assert_eq!(v.remove_account_type("Bank").unwrap(), CategoryRemoval::InUse(1));
    // Move the account off the type entirely -> type now deletable.
    let mut edited = v.vault.accounts.iter().find(|a| a.id == acc_id).unwrap().clone();
    edited.account_type = "Email".into();
    records::upsert(&mut v.vault.accounts, edited);
    v.save().unwrap();
    assert_eq!(v.remove_account_type("Bank").unwrap(), CategoryRemoval::Removed);
    assert!(!v.categories().account_type_names().contains(&"Bank".to_string()));
    cleanup(&path);
}

#[test]
fn category_deletion_ignores_history_only_usage() {
    // The crux of the requested behaviour: a type that appears only in a record's
    // HISTORY (never on a live record) is deletable.
    let path = tmp_path("rmhist");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.add_account_type("Legacy").unwrap();
    let mut acc = sample_account("u", "p");
    acc.account_type = "Legacy".into();
    let id = acc.id.clone();
    records::upsert(&mut v.vault.accounts, acc);
    v.save().unwrap();
    // Edit the account OFF "Legacy"; upsert records `type: "Legacy" -> "Email"`.
    let mut edited = v.vault.accounts.iter().find(|a| a.id == id).unwrap().clone();
    edited.account_type = "Email".into();
    records::upsert(&mut v.vault.accounts, edited);
    v.save().unwrap();
    // Sanity: the history really does mention "Legacy"...
    assert!(
        v.vault.accounts[0].history.iter().any(|c| c.detail.contains("Legacy")),
        "history retains the old type"
    );
    // ...but no LIVE account uses it, so it deletes.
    assert_eq!(v.remove_account_type("Legacy").unwrap(), CategoryRemoval::Removed);
    cleanup(&path);
}

#[test]
fn category_removal_is_blocked_read_only() {
    let path = tmp_path("rmro");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        v.add_asset_type("Crypto").unwrap();
        v.save().unwrap();
    }
    let mut ro = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    assert!(matches!(ro.remove_asset_type("Crypto"), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.remove_account_type("Email"), Err(VaultError::ReadOnly)));
    assert!(matches!(ro.remove_account_subtype("Financial", "Bank"), Err(VaultError::ReadOnly)));
    cleanup(&path);
}

#[cfg(unix)]
#[test]
fn backup_refuses_symlinked_destination() {
    // Defense in depth: an attacker who can write the vault dir must not be able
    // to redirect a backup through a symlinked destination directory.
    let path = tmp_path("bksym");
    OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let realdest = std::env::temp_dir().join(format!("pmbkreal-{}", nanos()));
    fs::create_dir_all(&realdest).unwrap();
    let linkdest = std::env::temp_dir().join(format!("pmbklink-{}", nanos()));
    std::os::unix::fs::symlink(&realdest, &linkdest).unwrap();
    let err = backup(&path, &linkdest).unwrap_err();
    assert!(matches!(err, VaultError::Storage(_)), "symlinked dest refused, got {err:?}");
    // A normal (non-symlink) destination still backs up fine.
    let bp = backup(&path, &realdest).unwrap();
    assert!(bp.exists());
    cleanup(&path);
    let _ = fs::remove_dir_all(&realdest);
    let _ = fs::remove_file(&linkdest);
}

/// EXPORT CONTAINMENT (adversarial corpus). `doc_tree_relpath` is the only thing
/// standing between a document path carried INSIDE a vault — which can come from a
/// hostile vault via `import_tree` or a cross-vault merge — and a real filesystem
/// write in `export_document_into` / `export_tree`. It had no direct test; its
/// containment was only ever exercised incidentally through the export paths.
///
/// Every component of the result must be `Component::Normal`. That single assertion
/// rules out the whole traversal family at once: `ParentDir` (`..`), `RootDir` (a
/// leading `/`, which would make the join absolute) and `Prefix` (`C:`, `\\?\`,
/// `\\server\share` — on Windows `PathBuf::push` of a rooted or prefixed component
/// REPLACES everything pushed before it, which is the classic way a "relative" path
/// silently becomes absolute).
#[test]
fn doc_tree_relpath_never_escapes_the_export_root() {
    let id = "0123456789abcdef0123456789abcdef";
    let root = Path::new("/tmp/vaultis-export-root");
    let hostile = [
        "../../../../etc/passwd",
        "..",
        "../",
        "/..",
        "a/../../b",
        "....//....//etc",
        "...",
        ".....",
        ". .",
        " .. ",
        "/etc/passwd",
        "//etc//passwd",
        "\\..\\..\\Windows\\System32",
        "C:\\Windows\\System32\\drivers\\etc\\hosts",
        "C:/Windows",
        "\\\\server\\share\\file",
        "\\\\?\\C:\\Windows",
        "con",
        "CON.pdf",
        "lpt1",
        "com\u{00B9}",           // superscript 1 -> COM1 on Windows
        "a\u{202e}fdp.exe",      // RIGHT-TO-LEFT OVERRIDE
        "a\u{200b}b",            // zero-width space
        "\u{feff}bom",
        "nul\0byte",
        "trailing...",
        "trailing   ",
        "",
        "/",
        "///",
        "\u{2028}line\u{2029}sep",
        "tab\there",
        "ok/../ok2",
        "ünïcodé/文件",
    ];
    for vp in hostile {
        let rel = doc_tree_relpath(vp, id);
        assert!(!rel.as_os_str().is_empty(), "never empty (falls back to <id>.bin): {vp:?}");
        assert!(rel.is_relative(), "must stay relative: {vp:?} -> {rel:?}");
        for c in rel.components() {
            assert!(
                matches!(c, std::path::Component::Normal(_)),
                "non-Normal component {c:?} from {vp:?} -> {rel:?}"
            );
        }
        let joined = root.join(&rel);
        assert!(joined.starts_with(root), "escaped the root: {vp:?} -> {joined:?}");
        // A literal ".." must not survive as a whole component anywhere.
        assert!(
            !rel.components().any(|c| c.as_os_str() == ".."),
            "a `..` component survived: {vp:?} -> {rel:?}"
        );
        assert_no_windows_rooting(&rel, vp);
    }
}

/// Cross-platform check for the WINDOWS rooting escape, asserted on characters
/// rather than on `Component`.
///
/// `Path::components()` parses with the HOST platform's rules. On Linux
/// `C:\Windows`, `\\server\share` and `\\?\C:\` are perfectly ordinary file names, so
/// they come back as `Component::Normal` and every containment assertion above passes
/// — while on Windows the same strings are a `Prefix`/`RootDir`, and `PathBuf::push`
/// of a prefixed or rooted component **replaces everything pushed before it**, turning
/// the "relative" export path absolute. A Linux-only test suite is therefore
/// structurally blind to the very escape `doc_tree_relpath`'s `['\\', ':', '\0']`
/// rejection exists to stop. Asserting on the characters closes that gap, so this
/// machine (and CI) really does verify the Windows behaviour.
fn assert_no_windows_rooting(rel: &Path, src: &str) {
    for c in rel.components() {
        let s = c.as_os_str().to_string_lossy();
        assert!(!s.contains('\\'), "backslash survived (rooted on Windows): {src:?} -> {rel:?}");
        assert!(!s.contains(':'), "colon survived (drive prefix on Windows): {src:?} -> {rel:?}");
        assert!(!s.contains('\0'), "NUL survived: {src:?} -> {rel:?}");
    }
}

use proptest::prelude::*;
proptest! {
    /// EXPORT CONTAINMENT (arbitrary input). The corpus above pins the attacks we
    /// thought of; this pins the ones we did not. For ANY string whatsoever, the
    /// sanitized relative path must stay inside the export root.
    #[test]
    fn prop_doc_tree_relpath_never_escapes_the_export_root(
        vp in any::<String>(),
        id in "[0-9a-f]{32}",
    ) {
        let rel = doc_tree_relpath(&vp, &id);
        prop_assert!(!rel.as_os_str().is_empty());
        prop_assert!(rel.is_relative(), "must stay relative: {:?} -> {:?}", vp, rel);
        for c in rel.components() {
            prop_assert!(
                matches!(c, std::path::Component::Normal(_)),
                "non-Normal component {:?} from {:?}", c, vp
            );
        }
        let root = Path::new("/tmp/vaultis-export-root");
        prop_assert!(root.join(&rel).starts_with(root), "escaped: {:?} -> {:?}", vp, rel);
        // See `assert_no_windows_rooting`: `Component` is host-parsed, so this
        // character check is what makes a Linux run verify the Windows property.
        for c in rel.components() {
            let s = c.as_os_str().to_string_lossy();
            prop_assert!(!s.contains('\\'), "backslash survived: {:?} -> {:?}", vp, rel);
            prop_assert!(!s.contains(':'), "colon survived: {:?} -> {:?}", vp, rel);
            prop_assert!(!s.contains('\0'), "NUL survived: {:?} -> {:?}", vp, rel);
        }
    }

    /// Virtual paths are always rooted, and `normalize_dir` is idempotent and
    /// never yields empty ("//") segments — so the limit check and storage see
    /// a single canonical form.
    #[test]
    fn prop_virtual_path_rooted_and_normalize_idempotent(
        loc in "[ -~]{0,80}",
        name in "[ -~]{1,40}",
    ) {
        let vp = virtual_path(&loc, &name);
        prop_assert!(vp.starts_with('/'), "virtual path is rooted: {vp:?}");
        let n1 = normalize_dir(&loc);
        prop_assert_eq!(normalize_dir(&n1), n1.clone());
        prop_assert!(!n1.contains("//"), "no empty segments: {n1:?}");
        prop_assert!(n1.is_empty() || n1.starts_with('/'));
    }

    /// referenced_doc_ids surfaces EVERY attached blob id across all record kinds
    /// (TrustWill.file, Asset.statement, Taxes/RealEstate documents, and
    /// GeneralDocument.file) and nothing extra — the invariant compaction relies
    /// on so a live document is never reclaimed and a delete reclaims exactly a
    /// record's own blobs. (Pure: builds a Vault in memory, no crypto/IO.)
    #[test]
    fn prop_referenced_doc_ids_covers_every_attachment(
        tw in proptest::collection::vec("[a-f0-9]{4}", 0..4),
        asset in proptest::collection::vec("[a-f0-9]{4}", 0..4),
        tax in proptest::collection::vec("[a-f0-9]{4}", 0..6),
        re in proptest::collection::vec("[a-f0-9]{4}", 0..6),
        gen_docs in proptest::collection::vec("[a-f0-9]{4}", 0..4),
    ) {
        let mut v = Vault::default();
        let mut want: Vec<String> = Vec::new();
        for f in &tw {
            let mut r = records::TrustWill::default();
            r.file = Some(f.clone());
            v.trust_wills.push(r);
            want.push(f.clone());
        }
        for f in &asset {
            let mut r = records::AssetLiability::default();
            r.statement = Some(f.clone());
            // Account LINKS are record ids, not volume blobs: they must never
            // surface as doc ids (the open-time `referenced ⊆ stored` check
            // would brick every vault with links). Guarded by the exact-count
            // assertion below.
            r.linked_accounts = vec![format!("link-{f}")];
            v.assets.push(r);
            want.push(f.clone());
        }
        let mut tf = records::TaxFiling::default();
        tf.documents = tax.clone();
        v.tax_filings.push(tf);
        want.extend(tax.iter().cloned());
        let mut rp = records::RealEstate::default();
        rp.documents = re.clone();
        v.real_estate.push(rp);
        want.extend(re.iter().cloned());
        for f in &gen_docs {
            let mut r = records::GeneralDocument::default();
            r.file = Some(f.clone());
            v.general_documents.push(r);
            want.push(f.clone());
        }
        let got = referenced_doc_ids(&v);
        for id in &want {
            prop_assert!(got.contains(id), "referenced_doc_ids missing {id}");
        }
        prop_assert_eq!(got.len(), want.len(), "no extra or dropped ids");
    }
}

proptest! {
    // Each case creates a real vault and does several Argon2-backed saves, so keep
    // the case count modest.
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    /// For ANY depth and ANY sequence of saves, the in-place redundancy ring stays
    /// well-formed: the vault opens cleanly from the primary, the mirror is the
    /// current generation, each retained generation decodes with a STRICTLY
    /// DESCENDING generation number (a contiguous ring), and the ring never exceeds
    /// the configured depth. Then corrupting the live file recovers from the mirror.
    #[test]
    fn prop_redundancy_ring_well_formed(depth in 1u32..=4, saves in 1usize..=6) {
        let path = tmp_path("propring");
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        v.set_redundancy(depth).unwrap();
        for i in 0..saves {
            records::upsert(&mut v.vault.accounts, sample_account(&format!("u{i}"), "p"));
            v.save().unwrap();
        }
        let cur_gen = v.vault.generation;
        drop(v);

        let gen_of = |p: &Path| {
            read_capped_vault(p).ok().and_then(|raw| decode_vault_bytes(&raw, b"a", b"b").ok().map(|t| t.0.generation))
        };
        // Mirror is the current generation (lossless copy of the latest save).
        prop_assert_eq!(gen_of(&mirror_path(&path)), Some(cur_gen), "mirror == current generation");
        // Generations strictly descending, contiguous, never above current, count <= depth.
        let mut prev = cur_gen;
        let mut count = 0u32;
        for k in 1..=MAX_REDUNDANCY {
            match gen_of(&bak_path(&path, k)) {
                Some(g) => {
                    count += 1;
                    prop_assert!(g < prev, "bak{} gen {} not strictly below {}", k, g, prev);
                    prev = g;
                }
                None => break, // contiguous ring: no holes
            }
        }
        prop_assert!(count <= depth, "ring depth {} exceeds configured {}", count, depth);

        // The vault opens cleanly from the primary; all saved records present.
        let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
        prop_assert!(v2.recovery_notice().is_none(), "primary intact — no recovery");
        prop_assert_eq!(v2.vault.accounts.len(), saves);
        drop(v2);

        // Corrupt the live file: recovery from the (intact) mirror must succeed.
        std::fs::write(&path, b"garbage not a vault").unwrap();
        let v3 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
        prop_assert!(v3.recovery_notice().is_some(), "recovered from a redundant copy");
        prop_assert_eq!(v3.vault.accounts.len(), saves, "no records lost on mirror recovery");
        drop(v3);
        cleanup(&path);
    }
}


// --- mutation-testing kill-tests (round 7: cargo-mutants survivor closure) ---
#[test]
fn mut_module_size_consts_have_exact_values() {
    // Pin the DoS-guard / clamp constants to their exact byte counts. A mutation
    // that swaps one of the `*` in `256 * 1024 * 1024` etc. for `+` or `/` changes
    // the value, so asserting against the resolved integer literal (NOT the same
    // arithmetic, which would mutate identically) kills it.
    assert_eq!(MAX_VAULT_SIZE, 268_435_456u64, "MAX_VAULT_SIZE must be exactly 256 MiB");
    assert_eq!(MIN_VOLUME_MAX_SIZE, 65_536u64, "MIN_VOLUME_MAX_SIZE must be exactly 64 KiB");
    assert_eq!(MAX_VOLUME_MAX_SIZE, 68_719_476_736u64, "MAX_VOLUME_MAX_SIZE must be exactly 64 GiB");
}

#[test]
fn mut_import_tree_vault_id_length_boundary() {
    // import_tree line: `id.is_empty() || id.len() > 64 || !alphanumeric`.
    // An id of EXACTLY 64 ascii-alnum chars must be accepted (real: `64 > 64`
    // is false). The `>`->`==`/`>=` mutants would reject 64 -> import fails, so a
    // SUCCESSFUL import at exactly 64 distinguishes the real operator from them.
    let id64: String = "a".repeat(64);
    assert_eq!(id64.len(), 64);
    let src = tmp_src("idlen64");
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = id64;
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let dest = tmp_path("impidlen64");
    let v = OpenVault::import_tree(&src, &dest, b"a", b"b", fast())
        .expect("a 64-char alphanumeric mirror id must import successfully");
    drop(v);
    let _ = fs::remove_dir_all(&src);
    cleanup(&dest);
}

#[test]
fn mut_import_tree_vault_id_over_length_rejected() {
    // An id of 65 ascii-alnum chars violates ONLY the `id.len() > 64` clause
    // (not is_empty, not the alnum check). Real code rejects it. The `||`->`&&`
    // mutant (col 32) would make the chain require is_empty too -> false -> the id
    // passes and the 65-char (still a valid filename) import SUCCEEDS, so a
    // REJECTION here kills `||`->`&&`. The `>`->`==`/`>=` mutants still reject 65,
    // so this test isolates the OR-vs-AND change.
    let id65: String = "a".repeat(65);
    assert_eq!(id65.len(), 65);
    let src = tmp_src("idlen65");
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = id65;
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let dest = tmp_path("impidlen65");
    let res = OpenVault::import_tree(&src, &dest, b"a", b"b", fast());
    assert!(res.is_err(), "a 65-char mirror id exceeds the 64-char cap and must be rejected");
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(parent_dir(&dest));
}

#[test]
fn mut_import_tree_empty_vault_id_rejected() {
    // The empty-id case violates ONLY the `id.is_empty()` clause. Real code rejects
    // it. The `||`->`&&` mutant would require all three clauses, so an empty id
    // (len 0 > 64 is false) would NOT be rejected here -> complements the 65-char
    // test from the other side of the OR.
    let src = tmp_src("idempty");
    let mut vault = Vault::default();
    vault.version = FORMAT_VERSION;
    vault.id = String::new();
    fs::write(src.join("vault.json"), serde_json::to_vec(&vault).unwrap()).unwrap();
    let dest = tmp_path("impidempty");
    let res = OpenVault::import_tree(&src, &dest, b"a", b"b", fast());
    assert!(res.is_err(), "an empty mirror id must be rejected");
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(parent_dir(&dest));
}

#[test]
fn mut_add_document_size_cap_boundary() {
    // add_document line: `if meta.len() > MAX_DOC_SIZE { TooLarge }`.
    // A file of EXACTLY MAX_DOC_SIZE must be accepted (real: `cap > cap` is false;
    // read_file_capped also lets exactly `cap` bytes through). The `>`->`==` and
    // `>`->`>=` mutants would reject the at-cap file, so a SUCCESSFUL add at exactly
    // the cap kills both. A file ONE byte over must be rejected with TooLarge,
    // confirming the upper side of the boundary.
    let path = tmp_path("docsizecap");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();

    let at_cap = vec![0u8; MAX_DOC_SIZE as usize];
    let at_cap_src = write_src("atcap", &at_cap);
    let id = v
        .add_document("/d", "atcap.bin", &at_cap_src)
        .expect("a document of exactly MAX_DOC_SIZE must be accepted");
    assert_eq!(v.read_document(&id).unwrap().len(), MAX_DOC_SIZE as usize, "at-cap doc round-trips");

    let over_cap = vec![0u8; MAX_DOC_SIZE as usize + 1];
    let over_cap_src = write_src("overcap", &over_cap);
    let res = v.add_document("/d", "overcap.bin", &over_cap_src);
    assert!(matches!(res, Err(VaultError::TooLarge)), "one byte over MAX_DOC_SIZE must be TooLarge");

    let _ = fs::remove_file(&at_cap_src);
    let _ = fs::remove_file(&over_cap_src);
    cleanup(&path);
}

#[test]
fn mut_redundancy_returns_the_set_depth_not_zero() {
    // Kills vault.rs:773 (redundancy() body replaced with 0). The default depth
    // is 0, so we must assert a NON-zero configured value is returned: set it to
    // 3 (below MAX_REDUNDANCY=10, so no clamping) and require the getter echoes 3.
    let path = tmp_path("mut-redun");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    assert_eq!(v.redundancy(), 0, "precondition: off by default");
    v.set_redundancy(3).unwrap();
    // The mutant returns 0; the real getter returns the stored 3.
    assert_eq!(v.redundancy(), 3, "redundancy() must return the configured depth, not 0");
    cleanup(&path);
}

#[test]
fn mut_opened_generation_returns_real_prior_generation_not_one() {
    // Kills vault.rs:1028 (opened_generation() replaced with 1). A freshly
    // created vault already has generation 1, so a test that only reaches gen 1
    // could not distinguish the mutant. We save twice more so the persisted
    // generation is well above 1, then reopen and require the getter matches it.
    let path = tmp_path("mut-opengen");
    let mut created = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    created.save().unwrap();
    created.save().unwrap();
    let g = created.vault.generation;
    assert!(g > 1, "precondition: persisted generation must exceed 1 to expose the mutant (got {g})");
    drop(created); // release the single-writer lock before reopening

    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    // opened_generation() reports the generation read off disk at open time
    // (before this open's own save bumps it). The mutant would return 1.
    assert_eq!(reopened.opened_generation(), g, "opened_generation() must surface the real prior generation, not 1");
    cleanup(&path);
}

#[test]
fn mut_previous_access_returns_real_prior_timestamp_not_0_1_neg1() {
    // Kills vault.rs:1024 (previous_access() replaced with 0 / 1 / -1). On create
    // last_opened_at is stamped with unix_now() and persisted; on reopen that real
    // timestamp becomes previous_access. A genuine timestamp is a large positive
    // value (>= ~1.7e9), so it is distinct from every constant the mutant returns.
    let path = tmp_path("mut-prevaccess");
    let before = records::unix_now();
    let created = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    drop(created); // persist + release the lock

    let after = records::unix_now();
    let reopened = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    let prev = reopened.previous_access();
    // Must be the genuine create-time timestamp, not 0, 1, or -1.
    assert!(prev > 1, "previous_access() must be a real timestamp, not 0/1/-1 (got {prev})");
    assert!(
        prev >= before && prev <= after,
        "previous_access() ({prev}) must fall in [{before}, {after}] — the create-time stamp"
    );
    cleanup(&path);
}

#[test]
fn mut_export_returns_real_vault_records_not_default() {
    // Kills vault.rs:458 (export() replaced with Ok(Default::default())). A
    // Default Vault has an empty account list, an empty id, and generation 0, so
    // we seed a known account and require export() round-trips the REAL contents.
    let path = tmp_path("mut-export");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("octocat", "hunter2"));
    v.save().unwrap();
    drop(v); // release the single-writer lock before the associated-fn export

    let exported = OpenVault::export(&path, b"a", b"b").unwrap();
    // A Default::default() Vault would have none of these.
    assert_eq!(exported.accounts.len(), 1, "export must return the real records, not an empty Default");
    assert_eq!(exported.accounts[0].username, "octocat");
    assert_eq!(exported.accounts[0].password, "hunter2");
    assert!(!exported.id.is_empty(), "real vault has a non-empty id; a Default does not");
    assert!(exported.generation >= 1, "real vault has a bumped generation; a Default is 0");
    cleanup(&path);
}

#[test]
fn mut_read_bounded_cap_boundary() {
    // `read_bounded(path, max)` rejects only when len > max. Pin the exact edge:
    // a file of EXACTLY `max` bytes reads OK (kills `> -> >=`, which would reject
    // len == max), and one byte over is TooLarge (kills dropping the rejection).
    let max: u64 = 16;
    let exact = write_src("rb_exact", &vec![0xABu8; max as usize]);
    let got = read_bounded(&exact, max).expect("a file of exactly `max` bytes must read OK");
    assert_eq!(got.len() as u64, max);
    assert_eq!(got, vec![0xABu8; max as usize]);
    fs::remove_file(&exact).ok();

    let over = write_src("rb_over", &vec![0xCDu8; (max + 1) as usize]);
    assert!(
        matches!(read_bounded(&over, max), Err(VaultError::TooLarge)),
        "one byte over `max` must be TooLarge"
    );
    fs::remove_file(&over).ok();
}

#[test]
fn mut_read_file_capped_cap_boundary() {
    // `read_file_capped(path, max)` rejects only when len > max. The exact-cap-OK
    // case kills both `> -> >=` (would reject len == max) and `> -> ==` (would
    // reject len == max as well); the cap+1 case confirms over-size is rejected.
    let max: u64 = 24;
    let exact = write_src("rfc_exact", &vec![0x11u8; max as usize]);
    let got = read_file_capped(&exact, max).expect("a file of exactly `max` bytes must read OK");
    assert_eq!(got.len() as u64, max);
    assert_eq!(&got[..], vec![0x11u8; max as usize].as_slice());
    fs::remove_file(&exact).ok();

    let over = write_src("rfc_over", &vec![0x22u8; (max + 1) as usize]);
    assert!(
        matches!(read_file_capped(&over, max), Err(VaultError::TooLarge)),
        "one byte over `max` must be TooLarge"
    );
    fs::remove_file(&over).ok();
}

/// The primary `vault.pmv` must be read with O_NOFOLLOW, like every other file in
/// the vault directory (which the design treats as attacker-reachable). A local
/// process that can write to that directory could otherwise swap the vault for a
/// symlink and have the app read an arbitrary file into memory on the next open.
///
/// Nothing legitimate is broken by this: every write is temp+rename, which replaces
/// a symlink with a regular file, so a symlinked vault.pmv never survives a save
/// anyway. Unix-only — O_NOFOLLOW has no portable Windows equivalent here.
#[cfg(unix)]
#[test]
fn a_symlinked_vault_file_is_refused_rather_than_followed() {
    let target = tmp_path("symlink_target");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"not a vault, but readable").unwrap();

    let link = tmp_path("symlink_vault");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = read_capped_vault(&link).expect_err("a symlinked vault.pmv must not be followed");
    // ELOOP surfaces as a plain Io error, NOT as NotFound (which would wrongly read
    // as "no vault here" and could send the caller down the create path).
    assert!(
        matches!(err, VaultError::Io(_)),
        "expected an Io error from O_NOFOLLOW, got {err:?}"
    );
    // The control: the very same bytes ARE readable through a real file, so the test
    // is detecting the symlink and not merely an unreadable path.
    assert!(read_capped_vault(&target).is_ok(), "control: the target itself reads fine");

    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_file(&target);
}

#[test]
fn mut_read_capped_vault_notfound_guard() {
    // The NotFound guard (the `Err(e) if e.kind() == NotFound => NotFound` arm):
    // a missing vault path maps specifically to VaultError::NotFound, while a
    // present, readable file does NOT take that arm (it returns its bytes). This
    // pins the guard: deleting it would surface a raw Io error for the missing
    // case, and a guard that always fired would wrongly reject the present file.
    let missing = tmp_path("rcv_missing"); // dir exists, vault.pmv does not
    assert!(!missing.exists());
    assert!(
        matches!(read_capped_vault(&missing), Err(VaultError::NotFound(p)) if p == missing),
        "a missing vault file must map to NotFound with the queried path"
    );

    // A present vault file is read back verbatim (NOT NotFound, NOT an error).
    let present = tmp_path("rcv_present");
    OpenVault::create(present.clone(), b"a", b"b", fast()).unwrap();
    let on_disk = fs::read(&present).unwrap();
    let via = read_capped_vault(&present).expect("a present vault must read OK");
    assert_eq!(via, on_disk, "present file must be returned verbatim");
    assert!(!via.is_empty());
    cleanup(&present);
    cleanup(&missing);
}

#[test]
fn mut_save_internal_heal_does_not_ring_corrupt_primary() {
    // save_internal line 703: `if rotate_ring && depth > 0`. On a recovery HEAL
    // open the save runs with rotate_ring=FALSE, so the (corrupt) outgoing
    // primary must NOT be ringed into bak1 — prev stays None and the good prior
    // generation in bak1 is preserved untouched. Mutating `&&` to `||` makes the
    // condition true on a heal (depth>0), so prev=Some(corrupt-primary) and
    // rotate_generations overwrites bak1 with the un-decryptable primary bytes.
    let path = tmp_path("muthealring");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    v.set_redundancy(1).unwrap(); // depth 1: one prior generation + a mirror
    records::upsert(&mut v.vault.accounts, sample_account("keep-me", "p")); // state A
    v.save().unwrap();
    records::upsert(&mut v.vault.accounts, sample_account("newer", "p")); // state B
    v.save().unwrap();
    drop(v);

    // After the two saves bak1 holds state A (the prior generation). Snapshot it.
    let bak1_before = fs::read(bak_path(&path, 1)).unwrap();

    // Corrupt BOTH the live file and the same-generation mirror so the open must
    // recover from bak1 (state A) and then HEAL the live tree (rotate_ring=false).
    let sentinel: &[u8] = b"not a vault at all";
    fs::write(&path, sentinel).unwrap();
    fs::write(mirror_path(&path), b"corrupt mirror").unwrap();

    let v2 = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(v2.recovery_notice().is_some(), "the open must have recovered + healed");
    drop(v2);

    // The heal must not have ringed the corrupt outgoing primary into bak1.
    let bak1_after = fs::read(bak_path(&path, 1)).unwrap();
    assert_ne!(
        bak1_after, sentinel,
        "heal ringed the corrupt primary into bak1 (the `&&`->`||` mutant): bak1 is now garbage"
    );
    assert_eq!(bak1_before, bak1_after, "heal left the good prior generation in bak1 untouched");
    // bak1 is still a real, decryptable vault holding state A (not state B).
    let (recovered, _h, _k) = decode_vault_bytes(&bak1_after, b"a", b"b").unwrap();
    let users: Vec<&str> = recovered.accounts.iter().map(|a| a.username.as_str()).collect();
    assert!(users.contains(&"keep-me"), "bak1 still decrypts to the prior generation");

    cleanup(&path);
}

#[test]
fn mut_staged_rewrite_empty_store_creates_no_volume_dirs() {
    // staged_rewrite line 897: `if self.storage.partition_count() > 0`. A vault
    // with NO documents has zero partitions and (crucially) NO live volume/ or
    // manifest/ dirs. With `> 0` (false) the rewrite must NOT materialize empty
    // staged volume/manifest dirs, so after a change_password (which always runs
    // staged_rewrite) the tree still has no volume/manifest dirs. Mutating `>` to
    // `>=` makes the condition true at count==0, creating empty staged dirs that
    // commit_rekey then swaps INTO the live tree.
    let path = tmp_path("mutstageempty");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let dir = parent_dir(&path);
    assert_eq!(v.storage.partition_count(), 0, "an empty vault has zero partitions");
    assert!(!dir.join("volume").exists(), "empty vault: no live volume dir to begin with");
    assert!(!dir.join("manifest").exists(), "empty vault: no live manifest dir to begin with");

    v.change_password(b"c", b"d").unwrap(); // runs staged_rewrite with partition_count()==0
    drop(v);

    assert!(
        !dir.join("volume").exists(),
        "staged_rewrite created an empty volume dir at count==0 (the `>`->`>=` mutant)"
    );
    assert!(
        !dir.join("manifest").exists(),
        "staged_rewrite created an empty manifest dir at count==0 (the `>`->`>=` mutant)"
    );
    // And the rekey still committed: the new passwords open, the old ones don't.
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_err(), "old password rejected after rekey");
    assert!(OpenVault::open(path.clone(), b"c", b"d").is_ok(), "new password opens the rekeyed vault");
    cleanup(&path);
}

#[test]
fn mut_backup_snapshot_collision_counter_increments() {
    // backup_snapshot line 1967: `n += 1`. Three backups into the SAME dest within
    // one timestamp-second must yield `backup-<stamp>`, `backup-<stamp>_1`, then
    // `backup-<stamp>_2` — proving the collision counter is incremented by 1 each
    // probe. Mutating `+= 1` to `-= 1` makes the third name `_0` (not `_2`);
    // `*= 1` pins n at 1 and spins forever on the already-present `_1` (timeout-kill).
    let path = tmp_path("mutbkpcollide");
    let v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    drop(v);
    let src_dir = parent_dir(&path);
    let dest = std::env::temp_dir().join(format!("pmmutbkp-{}", nanos()));

    // Retry the whole 3-call sequence into a fresh dest if a wall-clock second
    // boundary splits the stamps (rare); this keeps the assertion deterministic
    // without depending on absolute timing.
    let mut third_name = String::new();
    let mut stamp = String::new();
    for attempt in 0..20 {
        let d = dest.join(format!("try{attempt}"));
        let b1 = backup_snapshot(&path, &src_dir, &d).unwrap();
        let b2 = backup_snapshot(&path, &src_dir, &d).unwrap();
        let b3 = backup_snapshot(&path, &src_dir, &d).unwrap();
        let name = |p: &Path| p.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        let (n1, n2, n3) = (name(&b1), name(&b2), name(&b3));
        // All three share the stamp only if no second boundary was crossed.
        let s = n1.clone(); // "backup-<stamp>"
        if n2 == format!("{s}_1") && n3.starts_with(&format!("{s}_")) {
            stamp = s;
            third_name = n3;
            break;
        }
    }
    assert!(!third_name.is_empty(), "could not get three same-stamp backups in 20 tries");
    assert_eq!(
        third_name,
        format!("{stamp}_2"),
        "third colliding backup must be `_2` — proves `n += 1` increments the collision counter"
    );

    fs::remove_dir_all(&dest).ok();
    cleanup(&path);
}

// Kills: vault.rs harden_file (line ~2037) body -> Ok(()).
// harden_file must actually chmod the file to 0600. We deliberately loosen the
// perms first (to 0644) so create_new_0600's creation-time 0600 cannot mask the
// mutant; only harden_file's set_mode call can pull it back to 0600. If the body
// is replaced with `Ok(())`, the file stays 0644 and the assert fails.
#[cfg(unix)]
#[test]
fn mut_harden_file_chmods_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    let path = tmp_path("hardenfile");
    let _v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Loosen to 0644 so only harden_file can restore 0600.
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o644);
    fs::set_permissions(&path, p).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);

    harden_file(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "harden_file must chmod the file to 0600"
    );
    cleanup(&path);
}

// Kills: vault.rs harden_dir (line ~2051) body -> ().
// Direct-call form: loosen the vault dir to 0755, then harden_dir must restore
// 0700. A no-op body leaves it at 0755 and fails.
#[cfg(unix)]
#[test]
fn mut_harden_dir_chmods_to_0700() {
    use std::os::unix::fs::PermissionsExt;
    let path = tmp_path("hardendir");
    let _v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let dir = parent_dir(&path);
    let mut p = fs::metadata(&dir).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&dir, p).unwrap();
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o755);

    harden_dir(&dir);
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700,
        "harden_dir must chmod the directory to 0700"
    );
    cleanup(&path);
}

// Integration backstop for harden_dir (line ~2051): a freshly created vault's
// parent directory is created with create_dir_all at the umask (typically 0755)
// and is tightened to 0700 ONLY by harden_dir. With the body mutated to `()`,
// the directory keeps its umask perms and this assert catches it. (Also pins the
// created vault.pmv at 0600 for good measure.)
#[cfg(unix)]
#[test]
fn mut_create_vault_dir_is_0700() {
    use std::os::unix::fs::PermissionsExt;
    let path = tmp_path("createperm");
    let _v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let dir = parent_dir(&path);
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700,
        "create() must harden the vault directory to 0700"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "create() must leave vault.pmv at 0600"
    );
    cleanup(&path);
}

#[test]
fn mut_sweep_stale_temps_needs_prefix_and_tmp_suffix() {
    // sweep_stale_temps removes a file only if it BOTH starts with the vault prefix
    // AND ends in ".tmp" (the `&&` at line ~1665). An `||` would also wipe unrelated
    // "*.tmp" files and (worse) the live ".vault.pmv.*" data — so a file matching only
    // ONE clause must survive. (Kills the `&&` -> `||` mutant — verified by applying it.)
    let base = tmp_path("sweeptmp"); // tmp_path already created the dir
    let dir = parent_dir(&base);
    let swept = dir.join(format!(".{VAULT_FILE}.abc123.tmp")); // prefix AND .tmp -> removed
    let keep_tmp_only = dir.join("unrelated.tmp"); // .tmp only -> kept (|| would delete)
    let keep_prefix_only = dir.join(format!(".{VAULT_FILE}.mirror")); // prefix only -> kept
    for f in [&swept, &keep_tmp_only, &keep_prefix_only] {
        fs::write(f, b"x").unwrap();
    }
    sweep_stale_temps(&dir);
    assert!(!swept.exists(), "a genuine .vault.pmv.*.tmp temp is swept");
    assert!(keep_tmp_only.exists(), "an unrelated *.tmp must survive (kills && -> ||)");
    assert!(keep_prefix_only.exists(), "a non-.tmp vault sibling must survive (kills && -> ||)");
    cleanup(&base);
}

// ---- Cross-vault merge ("update from another vault") --------------------

use crate::merge::ChangeKind;

/// An account with a chosen id + updated_at, so two vaults can share a record id.
fn acct_with(id: &str, user: &str, pw: &str, updated_at: i64) -> Account {
    let mut a = sample_account(user, pw);
    a.id = id.to_string();
    a.title = format!("acct-{id}");
    a.owner = "owner".into();
    a.updated_at = updated_at;
    a.created_at = 1;
    a
}

#[test]
fn merge_pulls_newer_and_new_records_and_copies_blobs() {
    // SOURCE: a newer version of a shared account, a brand-new account, and a general
    // document with an attached blob.
    let s_path = tmp_path("merge-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    s.vault.accounts.push(acct_with("shared", "alice", "NEWpw", 200));
    s.vault.accounts.push(acct_with("only-in-source", "bob", "bobpw", 50));
    let doc = write_src("merge-doc", b"top-secret-statement");
    let blob_id = s.add_document("general-documents/passport", "passport.pdf", &doc).unwrap();
    let mut gd = records::GeneralDocument::new().unwrap();
    gd.id = "gd-1".into();
    gd.title = "Passport".into();
    gd.file = Some(blob_id.clone());
    gd.updated_at = 300;
    s.vault.general_documents.push(gd);
    s.save().unwrap();

    // CURRENT: an OLDER version of the shared account, plus a current-only record that
    // the additive merge must leave untouched.
    let c_path = tmp_path("merge-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    c.vault.accounts.push(acct_with("shared", "alice", "OLDpw", 100));
    c.vault.accounts.push(acct_with("only-in-current", "carol", "carolpw", 999));
    c.save().unwrap();

    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    // PLAN: 1 updated (shared), 2 new (only-in-source account + the general doc); 1 blob.
    let plan = c.plan_merge_from(&source).unwrap();
    assert_eq!(plan.updated_count(), 1, "shared account is newer in source");
    assert_eq!(plan.new_count(), 2, "source-only account + general document");
    assert_eq!(plan.blobs_to_copy(), 1, "the passport blob must be copied");
    assert_eq!(plan.bytes_to_copy(), b"top-secret-statement".len() as u64);
    assert!(plan.skipped.is_empty());
    // The shared account's preview shows the old->new recency.
    let shared = plan.records.iter().find(|r| r.id == "shared").unwrap();
    assert_eq!(shared.change, ChangeKind::Updated);
    assert_eq!(shared.current_updated_at, Some(100));
    assert_eq!(shared.source_updated_at, 200);

    // APPLY.
    let report = c.apply_merge_from(&source).unwrap();
    assert_eq!((report.records_added, report.records_updated), (2, 1));
    assert_eq!(report.blobs_copied, 1);

    // Shared account updated verbatim (source password + source updated_at preserved).
    let shared = c.vault.accounts.iter().find(|a| a.id == "shared").unwrap();
    assert_eq!(shared.password, "NEWpw");
    assert_eq!(shared.updated_at, 200, "source updated_at preserved (idempotency)");
    // New records present; current-only record untouched.
    assert!(c.vault.accounts.iter().any(|a| a.id == "only-in-source"));
    assert!(c.vault.accounts.iter().any(|a| a.id == "only-in-current" && a.password == "carolpw"));
    assert!(c.vault.general_documents.iter().any(|g| g.id == "gd-1" && g.file.as_deref() == Some(blob_id.as_str())));
    // The copied blob is readable in the destination under the SAME id.
    assert_eq!(&**c.read_document(&blob_id).unwrap(), b"top-secret-statement");
    // A vault-level audit entry records the merge (counts only, no secrets).
    assert!(c.vault.audit.iter().any(|ch| ch.action == "merged"));

    // IDEMPOTENT: a second plan against the same source is empty.
    let plan2 = c.plan_merge_from(&source).unwrap();
    assert!(plan2.is_empty(), "re-merge of identical data is a no-op");

    // The destination reopens cleanly (referenced ⊆ stored holds after the merge).
    drop(c);
    let c2 = OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    assert_eq!(&**c2.read_document(&blob_id).unwrap(), b"top-secret-statement");
    drop(c2);
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_duplicate_source_id_does_not_add_a_phantom_category_type() {
    // Two SOURCE accounts share id "dup" but carry DIFFERENT account types. merge_records
    // is first-occurrence-wins, so only the first ("Checking") is actually applied — the
    // second ("Phantom") must NOT seed an orphan category type that no applied record uses.
    let s_path = tmp_path("merge-dup-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a1 = acct_with("dup", "alice", "pw", 100);
    a1.account_type = "Checking".into();
    let mut a2 = acct_with("dup", "alice", "pw", 100);
    a2.account_type = "Phantom".into();
    s.vault.accounts.push(a1);
    s.vault.accounts.push(a2);
    s.save().unwrap();
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    let c_path = tmp_path("merge-dup-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();

    // PLAN: only the first occurrence's type appears in the preview's new categories.
    let plan = c.plan_merge_from(&source).unwrap();
    assert!(plan.new_categories.iter().any(|s| s.contains("Checking")), "first type previewed");
    assert!(
        !plan.new_categories.iter().any(|s| s.contains("Phantom")),
        "the duplicate id's second type must not be a phantom category: {:?}",
        plan.new_categories
    );

    // APPLY: the category list gains "Checking" but never the orphan "Phantom".
    c.apply_merge_from(&source).unwrap();
    assert!(c.vault.categories.account.iter().any(|x| x.name == "Checking"));
    assert!(
        !c.vault.categories.account.iter().any(|x| x.name == "Phantom"),
        "no orphan category type from the un-applied duplicate"
    );
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_duplicate_source_asset_id_does_not_add_a_phantom_type() {
    // Same first-occurrence-wins dedup as the account test, but exercises the ASSET loop
    // in plan/apply_merge_from (mutation kill-test: `||`->`&&` in the asset dedup guard
    // would process the un-applied duplicate and seed an orphan "Phantom" asset type).
    let s_path = tmp_path("merge-dupa-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a1 = records::AssetLiability::new().unwrap();
    a1.id = "dup".into();
    a1.asset_type = "Bank".into();
    a1.updated_at = 100;
    let mut a2 = records::AssetLiability::new().unwrap();
    a2.id = "dup".into();
    a2.asset_type = "Phantom".into();
    a2.updated_at = 100;
    s.vault.assets.push(a1);
    s.vault.assets.push(a2);
    s.save().unwrap();
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    let c_path = tmp_path("merge-dupa-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    let plan = c.plan_merge_from(&source).unwrap();
    assert!(plan.new_categories.iter().any(|s| s.contains("Bank")), "first asset type previewed");
    assert!(
        !plan.new_categories.iter().any(|s| s.contains("Phantom")),
        "duplicate asset id's second type must not be a phantom: {:?}",
        plan.new_categories
    );
    c.apply_merge_from(&source).unwrap();
    assert!(c.vault.categories.asset.iter().any(|x| x.as_str() == "Bank"));
    assert!(
        !c.vault.categories.asset.iter().any(|x| x.as_str() == "Phantom"),
        "no orphan asset type from the un-applied duplicate"
    );
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_apply_stores_sanitized_category_type_matching_the_preview() {
    // A crafted SOURCE asset's type carries a bidi override (U+202E). The approval preview
    // shows the SANITIZED name (display_safe -> '_'); apply must persist that SAME sanitized
    // type into the category list, not the raw spoofed string — otherwise what the user
    // approved and what gets stored diverge (preview/apply spoof divergence).
    let s_path = tmp_path("merge-san-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a = records::AssetLiability::new().unwrap();
    a.id = "spoof".into();
    a.asset_type = "Bank\u{202e}x".into();
    a.updated_at = 100;
    s.vault.assets.push(a);
    s.save().unwrap();
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    let c_path = tmp_path("merge-san-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    let sanitized = records::display_safe("Bank\u{202e}x"); // "Bank_x"
    // Preview already shows the sanitized name (round-2 fix); apply must agree.
    let plan = c.plan_merge_from(&source).unwrap();
    assert!(plan.new_categories.iter().any(|s| s.contains(&sanitized)));
    c.apply_merge_from(&source).unwrap();
    assert!(
        c.vault.categories.asset.iter().any(|x| x.as_str() == sanitized),
        "stored category is the sanitized name the user previewed: {:?}",
        c.vault.categories.asset
    );
    assert!(
        !c.vault.categories.asset.iter().any(|x| x.as_str().contains('\u{202e}')),
        "the raw bidi-spoofed type must NOT be persisted"
    );
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_preview_count_matches_apply_for_sanitize_colliding_types() {
    // Preview and apply must dedup category types on the SAME (display_safe-sanitized) value,
    // or plan.new_categories drifts from report.categories_added. Two accepted source assets
    // whose types differ only by a zero-width char both sanitize to "Acme_": the preview must
    // show ONE new asset category and apply must add ONE (not 2 previewed vs 1 applied).
    let s_path = tmp_path("merge-div-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a1 = records::AssetLiability::new().unwrap();
    a1.id = "a1".into();
    a1.asset_type = "Acme\u{200b}".into(); // ZERO WIDTH SPACE
    a1.updated_at = 100;
    let mut a2 = records::AssetLiability::new().unwrap();
    a2.id = "a2".into();
    a2.asset_type = "Acme\u{200c}".into(); // ZERO WIDTH NON-JOINER -> also "Acme_"
    a2.updated_at = 100;
    s.vault.assets.push(a1);
    s.vault.assets.push(a2);
    s.save().unwrap();
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    let c_path = tmp_path("merge-div-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    let plan = c.plan_merge_from(&source).unwrap();
    let previewed_asset_cats = plan.new_categories.iter().filter(|l| l.contains("asset type")).count();
    assert_eq!(previewed_asset_cats, 1, "two collapse to one previewed category");
    let report = c.apply_merge_from(&source).unwrap();
    assert_eq!(
        previewed_asset_cats, report.categories_added,
        "previewed new-category count must equal what apply added (no preview/apply drift)"
    );
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_preview_sanitizes_untrusted_source_record_labels() {
    // A crafted SOURCE vault must not inject bidi/zero-width characters into the merge
    // preview label the user authorizes (terminal/TUI/GUI spoofing). The label is cleaned
    // at the source in plan_collection, so neither the CLI nor the TUI renderer is spoofable.
    let s_path = tmp_path("merge-spoof-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a = acct_with("spoof", "alice", "pw", 100);
    a.title = "invoice\u{202e}fdp.exe".into(); // U+202E RIGHT-TO-LEFT OVERRIDE
    a.account_type = "Bank\u{202e}x".into(); // untrusted category type, also spoofed
    s.vault.accounts.push(a);
    s.save().unwrap();
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();

    let c_path = tmp_path("merge-spoof-cur");
    let c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    let plan = c.plan_merge_from(&source).unwrap();
    let r = plan.records.iter().find(|r| r.id == "spoof").expect("the source account is previewed");
    assert!(
        !r.label.contains('\u{202e}'),
        "the bidi-override char must be stripped from the preview label, got {:?}",
        r.label
    );
    assert!(r.label.contains('_'), "the spoof char is replaced with '_', got {:?}", r.label);
    // The new-category preview strings (sibling text on the same approval screen, derived
    // from the same untrusted source) must be sanitized too.
    assert!(
        !plan.new_categories.iter().any(|c| c.contains('\u{202e}')),
        "preview category strings must be sanitized: {:?}",
        plan.new_categories
    );
    assert!(
        plan.new_categories.iter().any(|c| c.contains("Bank_x")),
        "the spoofed account type appears sanitized in the preview: {:?}",
        plan.new_categories
    );
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_ignores_older_or_equal_and_current_only_records() {
    let s_path = tmp_path("merge-old-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    s.vault.accounts.push(acct_with("a", "x", "src-older", 100)); // older than current
    s.vault.accounts.push(acct_with("b", "x", "src-equal", 500)); // equal to current
    s.save().unwrap();

    let c_path = tmp_path("merge-old-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    c.vault.accounts.push(acct_with("a", "x", "cur-newer", 300));
    c.vault.accounts.push(acct_with("b", "x", "cur-equal", 500));
    c.save().unwrap();

    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    let plan = c.plan_merge_from(&source).unwrap();
    assert!(plan.is_empty(), "neither older nor equal source records are pulled");
    let report = c.apply_merge_from(&source).unwrap();
    assert_eq!((report.records_added, report.records_updated), (0, 0));
    // Current values untouched.
    assert_eq!(c.vault.accounts.iter().find(|a| a.id == "a").unwrap().password, "cur-newer");
    assert_eq!(c.vault.accounts.iter().find(|a| a.id == "b").unwrap().password, "cur-equal");
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_apply_refused_when_read_only() {
    let s_path = tmp_path("merge-ro-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    s.vault.accounts.push(acct_with("z", "x", "p", 100));
    s.save().unwrap();
    let c_path = tmp_path("merge-ro-cur");
    OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();

    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    let mut c_ro = OpenVault::open_read_only(c_path.clone(), b"c1", b"c2").unwrap();
    // Planning is allowed read-only (it mutates nothing)...
    assert_eq!(c_ro.plan_merge_from(&source).unwrap().new_count(), 1);
    // ...but applying is refused.
    assert!(matches!(c_ro.apply_merge_from(&source), Err(VaultError::ReadOnly)));
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_skips_record_referencing_locally_tombstoned_doc() {
    // SOURCE has a general document with an attached blob.
    let s_path = tmp_path("merge-tomb-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let doc = write_src("tomb-doc", b"deed-bytes");
    let blob_id = s.add_document("general-documents/deed", "deed.pdf", &doc).unwrap();
    let mut gd = records::GeneralDocument::new().unwrap();
    gd.id = "gd-tomb".into();
    gd.title = "Deed".into();
    gd.file = Some(blob_id.clone());
    gd.updated_at = 100;
    s.vault.general_documents.push(gd);
    s.save().unwrap();

    // First merge into CURRENT: the doc + blob arrive.
    let c_path = tmp_path("merge-tomb-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    {
        let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
        c.apply_merge_from(&source).unwrap();
    }
    assert!(c.has_document(&blob_id));

    // In CURRENT: unlink the record and delete (tombstone) the blob.
    c.vault.general_documents.clear();
    c.save().unwrap();
    c.remove_document(&blob_id).unwrap();
    assert!(c.vault.deleted_docs.contains(&blob_id), "blob is now tombstoned locally");

    // SOURCE bumps the record so recency would re-select it.
    s.vault.general_documents[0].updated_at = 500;
    s.save().unwrap();

    // The re-selected record references a locally-tombstoned doc → blocked, not applied.
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    let plan = c.plan_merge_from(&source).unwrap();
    assert!(plan.records.is_empty(), "record blocked by the tombstoned dependency");
    assert_eq!(plan.skipped.len(), 1);
    assert!(plan.skipped[0].reason.contains("deleted"), "reason explains the block: {:?}", plan.skipped[0].reason);

    let report = c.apply_merge_from(&source).unwrap();
    assert_eq!(report.records_added, 0);
    assert_eq!(report.records_skipped, 1);
    // The tombstone is intact (nothing resurrected), and the vault still reopens.
    assert!(c.vault.deleted_docs.contains(&blob_id));
    drop(c);
    OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_rejects_source_with_unsafe_vault_id() {
    // A crafted source whose (attacker-controlled) vault.id carries a bidi/control char
    // must be refused before it can reach the preview or this vault's audit log.
    let s_path = tmp_path("merge-badid-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    s.vault.accounts.push(acct_with("x", "u", "p", 5));
    s.save().unwrap();

    let c_path = tmp_path("merge-badid-cur");
    let c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();

    let mut source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    // Tamper with the in-memory source id (a real crafted vault could carry this).
    source.vault.id = "ab\u{202e}cd".into();
    let err = c.plan_merge_from(&source).unwrap_err();
    assert!(matches!(err, VaultError::Storage(StorageError::Corrupt(_))), "got {err:?}");
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_reconciles_category_types_into_config_lists() {
    // SOURCE: an account with a NOVEL type+subtype and an asset with a NOVEL type — none
    // present in the destination's editable category lists.
    let s_path = tmp_path("merge-cats-src");
    let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    let mut a = sample_account("alice", "pw");
    a.id = "acct-x".into();
    a.title = "X".into();
    a.owner = "o".into();
    a.account_type = "Brokerage".into();
    a.account_subtype = "Margin".into();
    a.updated_at = 9;
    s.vault.accounts.push(a);
    let mut asset = records::AssetLiability::new().unwrap();
    asset.id = "asset-x".into();
    asset.asset_type = "Crypto".into();
    asset.updated_at = 9;
    s.vault.assets.push(asset);
    s.save().unwrap();

    let c_path = tmp_path("merge-cats-cur");
    let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
    // Sanity: the destination's lists do NOT have these types yet.
    assert!(!c.categories().account_type_names().iter().any(|t| t == "Brokerage"));
    assert!(!c.categories().asset.iter().any(|t| t == "Crypto"));

    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    // The plan previews the new types that will be added.
    let plan = c.plan_merge_from(&source).unwrap();
    assert_eq!(plan.new_categories.len(), 3, "account type + subtype + asset type: {:?}", plan.new_categories);

    let report = c.apply_merge_from(&source).unwrap();
    assert_eq!(report.categories_added, 3);
    // The types now appear in the editable Config lists + the subtype is under its type.
    assert!(c.categories().account_type_names().iter().any(|t| t == "Brokerage"));
    assert!(c.categories().subtypes_for("Brokerage").iter().any(|s| s == "Margin"));
    assert!(c.categories().asset.iter().any(|t| t == "Crypto"));
    // Idempotent: a re-merge adds nothing more.
    assert!(c.plan_merge_from(&source).unwrap().new_categories.is_empty());
    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn sync_types_from_records_backfills_missing_category_types() {
    let path = tmp_path("sync-types");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Records carrying types NOT in the editable lists (as if from an older import/merge).
    let mut acct = sample_account("u", "p");
    acct.account_type = "Brokerage".into();
    acct.account_subtype = "Margin".into();
    v.vault.accounts.push(acct);
    let mut asset = records::AssetLiability::new().unwrap();
    asset.asset_type = "Crypto".into();
    v.vault.assets.push(asset);
    // A blank type contributes nothing.
    let mut blank = records::AssetLiability::new().unwrap();
    blank.asset_type = "   ".into();
    v.vault.assets.push(blank);
    v.save().unwrap();

    let added = v.sync_types_from_records().unwrap();
    assert_eq!(added, 3, "account type + subtype + asset type");
    assert!(v.categories().account_type_names().iter().any(|t| t == "Brokerage"));
    assert!(v.categories().subtypes_for("Brokerage").iter().any(|s| s == "Margin"));
    assert!(v.categories().asset.iter().any(|t| t == "Crypto"));
    // Idempotent: a second sync adds nothing and writes nothing new.
    assert_eq!(v.sync_types_from_records().unwrap(), 0);

    // Read-only handles refuse.
    drop(v);
    let mut ro = OpenVault::open_read_only(path.clone(), b"a", b"b").unwrap();
    assert!(matches!(ro.sync_types_from_records(), Err(VaultError::ReadOnly)));
    cleanup(&path);
}

#[test]
fn sync_types_from_records_sanitizes_spoofed_category_types() {
    // A record's type field can be UNTRUSTED (delivered via merge/import). sync runs
    // automatically on every writable open, so it must NOT re-inject a raw bidi/zero-width
    // spoofed type into the category list — it must sanitize identically to apply_merge_from.
    let path = tmp_path("sync-spoof");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut a = records::AssetLiability::new().unwrap();
    a.asset_type = "Bank\u{202e}x".into(); // RIGHT-TO-LEFT OVERRIDE mid-string
    v.vault.assets.push(a);
    let mut acct = sample_account("u", "p");
    acct.account_type = "Cre\u{200b}dit".into(); // ZERO WIDTH SPACE
    v.vault.accounts.push(acct);
    v.sync_types_from_records().unwrap();
    let san_asset = records::display_safe("Bank\u{202e}x");
    let san_acct = records::display_safe("Cre\u{200b}dit");
    assert!(v.vault.categories.asset.iter().any(|x| x.as_str() == san_asset));
    assert!(v.vault.categories.account.iter().any(|x| x.name == san_acct));
    assert!(
        !v.vault.categories.asset.iter().any(|x| x.as_str().contains('\u{202e}')),
        "raw bidi-spoofed asset type must NOT be synced"
    );
    assert!(
        !v.vault.categories.account.iter().any(|x| x.name.contains('\u{200b}')),
        "raw zero-width-spoofed account type must NOT be synced"
    );
    cleanup(&path);
}

#[test]
fn type_usage_helpers_count_live_records_and_sync_never_deletes() {
    let path = tmp_path("type-usage");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    // Configured entries that NO record uses (as if added then their records deleted).
    v.add_account_type("Unused Bank").unwrap();
    v.add_account_subtype("Unused Bank", "GhostSub").unwrap();
    v.add_asset_type("Unused Asset").unwrap();
    // Records using OTHER types not yet in the lists.
    for u in ["u1", "u2"] {
        let mut a = sample_account(u, "p");
        a.account_type = "Used Bank".into();
        a.account_subtype = "Checking".into();
        v.vault.accounts.push(a);
    }
    let mut asset = records::AssetLiability::new().unwrap();
    asset.asset_type = "Used Asset".into();
    v.vault.assets.push(asset);
    v.save().unwrap();

    // Usage counts reflect live records (case-insensitive); unused entries report 0.
    assert_eq!(v.account_type_usage("Used Bank"), 2);
    assert_eq!(v.account_type_usage("used bank"), 2, "case-insensitive");
    assert_eq!(v.account_subtype_usage("Used Bank", "Checking"), 2);
    assert_eq!(v.asset_type_usage("Used Asset"), 1);
    assert_eq!(v.account_type_usage("Unused Bank"), 0);
    assert_eq!(v.account_subtype_usage("Unused Bank", "GhostSub"), 0);
    assert_eq!(v.asset_type_usage("Unused Asset"), 0);

    // Auto-sync is ADDITIVE: it adds the record-derived types and keeps the unused ones.
    let before_accounts = v.categories().account_type_names();
    let before_assets = v.categories().asset.clone();
    let added = v.sync_types_from_records().unwrap();
    assert_eq!(added, 3, "Used Bank + Checking + Used Asset");
    let after_accounts = v.categories().account_type_names();
    let after_assets = v.categories().asset.clone();
    for t in &before_accounts {
        assert!(after_accounts.contains(t), "sync kept pre-existing account type {t}");
    }
    for t in &before_assets {
        assert!(after_assets.contains(t), "sync kept pre-existing asset type {t}");
    }
    assert!(after_accounts.iter().any(|t| t == "Unused Bank"), "unused type survives sync");
    assert!(after_assets.iter().any(|t| t == "Unused Asset"), "unused asset type survives sync");
    assert!(
        v.categories().subtypes_for("Unused Bank").iter().any(|s| s == "GhostSub"),
        "unused subtype survives sync"
    );
    cleanup(&path);
}

#[test]
fn type_usage_matches_whitespace_padded_record_values() {
    // A record carrying a whitespace-padded type (legacy/imported data) must still count
    // as "in use" of the TRIMMED configured type — otherwise a just-synced type would be
    // mislabeled "unused" and wrongly deletable. usage/sync/remove must agree on the key.
    let path = tmp_path("type-usage-trim");
    let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut acct = sample_account("u", "p");
    acct.account_type = " Brokerage ".into();
    acct.account_subtype = " Margin ".into();
    v.vault.accounts.push(acct);
    let mut asset = records::AssetLiability::new().unwrap();
    asset.asset_type = " Crypto ".into();
    v.vault.assets.push(asset);
    v.save().unwrap();

    // Sync inserts the TRIMMED entries.
    assert_eq!(v.sync_types_from_records().unwrap(), 3);
    // The trimmed entries are reported IN USE (not "unused"), despite the padded records.
    assert_eq!(v.asset_type_usage("Crypto"), 1);
    assert_eq!(v.account_type_usage("Brokerage"), 1);
    assert_eq!(v.account_subtype_usage("Brokerage", "Margin"), 1);
    // And deletion is correctly refused as in-use rather than silently orphaning the record.
    assert!(matches!(v.remove_asset_type("Crypto"), Ok(CategoryRemoval::InUse(1))));
    assert!(matches!(v.remove_account_subtype("Brokerage", "Margin"), Ok(CategoryRemoval::InUse(1))));
    cleanup(&path);
}

/// Build a (source, current) pair where the source has a NEWER general-document record
/// (same id "gd") referencing a fresh blob, and the current has an OLDER `gd` with no
/// document. Returns the two vault.pmv paths. Used by the ENOSPC merge tests.
#[cfg(feature = "fault-injection")]
fn merge_pair(tag: &str) -> (PathBuf, PathBuf) {
    let s_path = tmp_path(&format!("{tag}-src"));
    {
        let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
        let f = write_src(tag, b"document-bytes");
        let blob = s.add_document("general-documents/x", "x.pdf", &f).unwrap();
        let mut gd = records::GeneralDocument::new().unwrap();
        gd.id = "gd".into();
        gd.updated_at = 2000;
        gd.file = Some(blob);
        s.vault.general_documents.push(gd);
        s.save().unwrap();
        fs::remove_file(&f).ok();
    }
    let c_path = tmp_path(&format!("{tag}-cur"));
    {
        let mut c = OpenVault::create(c_path.clone(), b"c1", b"c2", fast()).unwrap();
        let mut gd = records::GeneralDocument::new().unwrap();
        gd.id = "gd".into();
        gd.updated_at = 1000;
        gd.file = None;
        c.vault.general_documents.push(gd);
        c.save().unwrap();
    }
    (s_path, c_path)
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_during_merge_blob_copy_leaves_current_unchanged_and_unpoisoned() {
    let (s_path, c_path) = merge_pair("enospc-merge-blob");
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    let mut c = OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    // The merge must copy the source's blob — make that copy hit a full disk.
    crate::fault::fail_at("volume.write", 1);
    let err = c.apply_merge_from(&source).unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, VaultError::Storage(_)), "blob copy fails cleanly: {err:?}");
    // Phase 1 failed before any record mutation: in-memory record is still the old one,
    // and the handle is NOT poisoned (nothing was committed) — a normal save still works.
    assert_eq!(c.vault.general_documents.iter().find(|g| g.id == "gd").unwrap().updated_at, 1000);
    c.save().expect("handle still writable (not poisoned)");
    drop(c);
    // On disk the older record stands and the vault reopens.
    let re = OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    assert_eq!(re.vault.general_documents.iter().find(|g| g.id == "gd").unwrap().updated_at, 1000);
    cleanup(&s_path);
    cleanup(&c_path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_during_merge_save_poisons_handle_and_does_not_persist() {
    let (s_path, c_path) = merge_pair("enospc-merge-save");
    let source = OpenVault::open_read_only(s_path.clone(), b"s1", b"s2").unwrap();
    let mut c = OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    // The blob copy succeeds; the FINAL vault.pmv save hits a full disk.
    crate::fault::fail_at("vault.write", 1);
    let err = c.apply_merge_from(&source).unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, VaultError::Io(_)), "the final save fails: {err:?}");
    // The handle is POISONED so the diverged in-memory (merged) state can never be
    // re-flushed by a later unrelated save (the #0/#3 fix).
    assert!(matches!(c.save(), Err(VaultError::ReadOnly)), "handle poisoned after a failed merge save");
    drop(c);
    // The merge did NOT take on disk (the save failed): the older record stands, and the
    // vault reopens cleanly (the copied blob is a harmless unreferenced orphan).
    let re = OpenVault::open(c_path.clone(), b"c1", b"c2").unwrap();
    assert_eq!(re.vault.general_documents.iter().find(|g| g.id == "gd").unwrap().updated_at, 1000, "merge not persisted");
    cleanup(&s_path);
    cleanup(&c_path);
}

/// AUDIT 2026-08-03 A-1 (reproduction): a READ-ONLY open must not delete files in the
/// vault directory. `open_inner` hands the store the redundancy depth unconditionally,
/// and `set_redundancy(0)` deletes every manifest spare — so opening read-only a vault
/// whose settings say 0 while spares exist on disk WRITES to the vault folder.
///
/// The reachable state comes from `set_redundancy` itself: it writes the spares before
/// `save()`, so a save that fails (full disk) leaves spares on disk with the on-disk
/// setting still 0.
#[cfg(feature = "fault-injection")]
#[test]
fn audit_a1_read_only_open_must_not_delete_manifest_spares() {
    let path = tmp_path("audit-a1");
    let mirror = path.parent().unwrap().join("manifest").join("manifest.0.mirror");
    {
        let mut v = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let src = path.parent().unwrap().join("doc.txt");
        std::fs::write(&src, b"body").unwrap();
        v.add_document("loc", "doc.txt", &src).unwrap();
        // Turn redundancy on, with the vault save forced to fail: the spares reach disk
        // (they are written first), the setting does not.
        crate::fault::fail_at("vault.write", 1);
        let _ = v.set_redundancy(2);
        crate::fault::clear();
    }
    assert!(mirror.exists(), "precondition: a spare is on disk");
    let v = OpenVault::open_with(path.clone(), b"a", b"b", true).unwrap();
    assert_eq!(v.redundancy(), 0, "precondition: the saved setting is still 0");
    drop(v);
    assert!(mirror.exists(), "a read-only open must not delete anything in the vault folder");
    cleanup(&path);
}
