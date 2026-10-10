//! Whole-vault export and import: the raw encrypted files (`export`,
//! `export_documents`, `export_manifests`), the human-readable decrypted tree
//! (`export_tree`), and rebuilding a vault from such a tree (`import_tree`).

use super::*;

impl OpenVault {
    /// Decrypt the vault and return its contents **without** modifying any file.
    // Note these `export*` functions take `&Path` (a borrow) and are "associated
    // functions" you call as `OpenVault::export(...)` — they don't need a live
    // `OpenVault`; they open, read, and drop everything internally.
    pub fn export(path: &Path, pw1: &[u8], pw2: &[u8]) -> Result<Vault, VaultError> {
        // The `_header` / `_key` names start with `_` to say "intentionally unused".
        let (vault, _header, _key) = decrypt_file(path, pw1, pw2)?;
        Ok(vault)
    }

    /// Decrypt documents without modifying any file. With `part = Some(n)` only
    /// partition `n`'s volume is decrypted; with `None`, every partition.
    /// Returns each document's manifest entry + plaintext (wiped on drop).
    pub fn export_documents(
        path: &Path,
        pw1: &[u8],
        pw2: &[u8],
        part: Option<u32>,
    ) -> Result<Vec<DecryptedDoc>, VaultError> {
        let (vault, _header, key) = decrypt_file(path, pw1, pw2)?;
        let dir = parent_dir(path);
        // Refuse to read a half-committed rekey tree (old vault.pmv vs new-key
        // volume/manifest); this read-only path cannot finish the roll-forward.
        if dir.join(REKEY_DIR).exists() {
            return Err(VaultError::RekeyPending);
        }
        let store = VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size)?;
        // Collect entries first so the immutable borrow for reads is clean.
        let mut entries: Vec<ManifestEntry> = selected_entries(&store, part)?;
        // Honor deletion tombstones: remove_document is two durable commits (tombstone-save,
        // then storage.remove); a crash/ENOSPC in that gap leaves a tombstoned-but-present
        // frame. The standalone export paths read the manifest directly, so filter the
        // tombstoned ids here (mirroring is_tombstoned) — otherwise a "deleted" secret would be
        // resurrected into an UNENCRYPTED export and re-admitted by import_tree.
        entries.retain(|e| !vault.deleted_docs.iter().any(|d| d == &e.id));
        let mut out = Vec::new(); // a growable, initially-empty result vector
        for e in entries { // `e` is moved out of the vector on each iteration
            let bytes = store.read(&e.id, &key)?; // decrypt this doc's plaintext
            out.push((e, bytes)); // append the (entry, plaintext) pair
        }
        Ok(out)
    }

    /// Decrypt and return manifest entries (the document index). With
    /// `part = Some(n)` only partition `n`'s manifest; with `None`, all of them.
    pub fn export_manifests(
        path: &Path,
        pw1: &[u8],
        pw2: &[u8],
        part: Option<u32>,
    ) -> Result<Vec<ManifestEntry>, VaultError> {
        let (vault, _header, key) = decrypt_file(path, pw1, pw2)?;
        let dir = parent_dir(path);
        if dir.join(REKEY_DIR).exists() {
            return Err(VaultError::RekeyPending);
        }
        let store = VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size)?;
        let mut entries = selected_entries(&store, part)?;
        // Drop tombstoned ids so a "deleted" doc never surfaces in an exported manifest.
        entries.retain(|e| !vault.deleted_docs.iter().any(|d| d == &e.id));
        Ok(entries)
    }

    /// Decrypt the **entire** vault directory into a plaintext mirror at `out`
    /// (DESIGN.md §6.3). It writes:
    /// - `out/vault.json` — all records/settings;
    /// - `out/manifest/manifest.<N>.json` + `out/volume/vol.<N>/<id>` — the id-keyed document
    ///   store, which is the canonical, unambiguous source [`OpenVault::import_tree`] reads back
    ///   (two documents may legitimately share a virtual path, so this round-trips them exactly);
    /// - `out/documents/<virtual/path>` — the SAME documents recreated in their human-browsable
    ///   folder tree (like `extract`; duplicate paths get a `_N` suffix). For viewing only.
    /// - `out/csv/<tab>.csv` — a CSV export of every record tab.
    ///
    /// Reuses the standard decrypt + store-read paths (no new crypto) and refuses a
    /// half-committed rekey.
    ///
    /// WARNING: the output is UNENCRYPTED (every password + document in the clear);
    /// see DESIGN.md §9.17. Files are written 0600 with `create_new` (no clobber).
    pub fn export_tree(path: &Path, pw1: &[u8], pw2: &[u8], out: &Path) -> Result<(), VaultError> {
        let (vault, _header, key) = decrypt_file(path, pw1, pw2)?;
        let dir = parent_dir(path);
        if dir.join(REKEY_DIR).exists() {
            return Err(VaultError::RekeyPending);
        }
        let store = VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size)?;

        // Refuse a symlinked export ROOT before writing any cleartext into it: create_dir_all
        // and harden_dir both follow a symlink, and create_new's O_EXCL guards only the final
        // filename — so without this a symlink pre-planted at `out` (by a local process winning a
        // predictable/reused export path) would redirect the ENTIRE decrypted mirror (vault.json +
        // manifests + blobs + CSVs) outside the chosen directory and chmod the target 0700 (audit
        // R4-1). Same root guard backup() applies; the subdirs below get reject_symlinked_descendants.
        reject_symlink_dir(out)?;
        fs::create_dir_all(out)?;
        harden_dir(out);
        // vault.json — pretty for human inspection; the buffer wipes on drop and is
        // serialized without a mid-write realloc that would strand cleartext (see
        // serialize_secret_json).
        let vault_json = serialize_secret_json(&vault, true)?;
        write_new_bytes(&out.join("vault.json"), &vault_json)?;

        let man_dir = out.join("manifest");
        let vol_root = out.join("volume");
        let docs_dir = out.join("documents"); // human-browsable tree (round-trip source stays volume/)
        // Create + guard the manifest dir up front (the AUTHORITATIVE secret-bearing subdirs are
        // guarded the same way the cosmetic documents/ tree is: create_dir_all follows a symlinked
        // parent and the O_EXCL write guards only the leaf, so a pre-planted out/manifest or
        // out/volume symlink would otherwise redirect the decrypted manifest + blobs outside the
        // export root — audit R4-2). Then record the authoritative partition count BEFORE writing
        // any partition, so import_tree can fail closed against a TAIL-truncated mirror: a mid-export
        // abort leaves the full count on disk but fewer partitions, which import detects (audit R4-3).
        reject_symlinked_descendants(out, &man_dir)?;
        fs::create_dir_all(&man_dir)?;
        harden_dir(&man_dir);
        write_new_bytes(&man_dir.join(MIRROR_PARTITIONS_FILE), store.partition_count().to_string().as_bytes())?;
        // Walk every partition: write its manifest as JSON and each blob by id.
        for p in 0..store.partition_count() as u32 {
            // Skip tombstoned ids so a "deleted" secret is never written into the plaintext mirror.
            let entries: Vec<ManifestEntry> =
                store.partition_entries(p).filter(|e| !vault.deleted_docs.iter().any(|d| d == &e.id)).cloned().collect();
            let man_json = serde_json::to_vec_pretty(&entries)?;
            write_new_bytes(&man_dir.join(format!("manifest.{p}.json")), &man_json)?;
            let vol_dir = vol_root.join(format!("vol.{p}"));
            reject_symlinked_descendants(out, &vol_dir)?;
            fs::create_dir_all(&vol_dir)?;
            harden_dir(&vol_dir);
            for e in &entries {
                // Symmetry with `import_tree`: the id becomes a filename here
                // (`vol_dir.join(&e.id)`), so enforce the same lowercase-hex allowlist
                // on the WRITE side too. With a genuine vault the id is always safe
                // (authenticated, 32 hex chars); this just guarantees export can never
                // traverse out of `vol_dir` even if a future path admitted a stray id.
                if !is_safe_blob_id(&e.id) {
                    return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe document id in vault: {:?}", e.id))));
                }
                let bytes = store.read(&e.id, &key)?; // decrypts + verifies id/path
                write_new_bytes(&vol_dir.join(&e.id), &bytes)?;
                // Human-browsable copy at the document's virtual-path tree (like `extract`). The
                // canonical round-trip source remains the id-keyed volume/ above, because two docs
                // may legitimately share a virtual path (so a pure path-tree can't round-trip them).
                //
                // This copy is COSMETIC and best-effort: `import_tree` reads only volume/ +
                // manifest, never documents/. So a failure to place it — ENAMETOOLONG on a
                // ~255-byte filename component (a legal single-component name plus a `_N`/id suffix
                // can exceed NAME_MAX), a symlink-guard rejection, ENOSPC — must NEVER propagate
                // and truncate the AUTHORITATIVE mirror (later partitions' volume/manifest and the
                // per-tab CSVs are still unwritten at this point). On any error we skip just this
                // one viewing copy; the document is fully present and recoverable from volume/
                // (audit F2). `let _ =` discards the best-effort Result.
                let tree_dest = unique_export_path(docs_dir.join(doc_tree_relpath(&e.path, &e.id)), Some(&e.id));
                let _ = write_human_tree_copy(out, &tree_dest, &bytes);
            }
        }
        // Per-tab CSV exports (records in cleartext, consistent with vault.json) under `csv/`.
        let csv_dir = out.join("csv");
        reject_symlinked_descendants(out, &csv_dir)?; // CSVs carry every password — same guard (audit R4-2)
        fs::create_dir_all(&csv_dir)?;
        harden_dir(&csv_dir);
        // doc id -> file basename for the CSV "documents" columns; tombstoned ids -> empty
        // (matching the front-end CSV export's tombstone-aware resolver).
        let name_of = |id: &str| {
            if vault.deleted_docs.iter().any(|d| d == id) {
                String::new()
            } else {
                store.entry(id).map(|e| crate::csv::basename(&e.path)).unwrap_or_default()
            }
        };
        for tab in [
            crate::csv::CsvTab::Urgent,
            crate::csv::CsvTab::Instructions,
            crate::csv::CsvTab::TrustWill,
            crate::csv::CsvTab::Assets,
            crate::csv::CsvTab::Accounts,
            crate::csv::CsvTab::RealEstate,
            crate::csv::CsvTab::Taxes,
            crate::csv::CsvTab::GeneralDocuments,
        ] {
            let (base, text, _) = crate::csv::build_tab_csv(&vault, tab, name_of);
            write_new_bytes(&csv_dir.join(format!("{base}.csv")), text.as_bytes())?;
        }
        Ok(())
    }

    /// Create a **new** encrypted vault (at the `vault.pmv` path `dest`) from a
    /// plaintext mirror at `src` (as produced by [`export_tree`]), under two new
    /// passwords. Preserves the records, categories, settings, and vault `id` from
    /// `src/vault.json` and re-encrypts every document from the mirror — reusing
    /// the same `VolumeStore::put` + atomic vault writer a password change uses (no
    /// duplicated crypto), then returns a fully-validated handle via the normal
    /// open path. Refuses to overwrite an existing vault.
    pub fn import_tree(
        src: &Path,
        dest: &Path,
        pw1: &[u8],
        pw2: &[u8],
        params: KdfParams,
    ) -> Result<OpenVault, VaultError> {
        if dest.exists() {
            return Err(VaultError::AlreadyExists(dest.to_path_buf()));
        }
        // Same write-path param validation as `create` (see there): never build a
        // vault whose params the reader would later reject.
        params.validate().map_err(|_| VaultError::BadParams)?;
        // Read + validate the mirror's vault JSON (size-capped, symlink-rejected;
        // wipe the buffer after parsing). The mirror is untrusted input.
        let vault_json = Zeroizing::new(read_capped(&src.join("vault.json"), MAX_VAULT_SIZE)?);
        let mut vault: Vault = serde_json::from_slice(&vault_json)?;
        if vault.version != FORMAT_VERSION {
            return Err(VaultError::BadVersion(vault.version));
        }
        // The mirror is UNTRUSTED. `vault.id` becomes the AEAD AAD domain for every
        // volume/manifest, and `volume_max_size` drives partition placement — sanitize
        // both rather than adopting crafted values. The id is normally 32 random hex
        // chars (`records::random_id`); reject anything that isn't a short ASCII
        // alphanumeric token. Clamp the volume size into a sane range; cap the
        // redundancy depth. (Per-blob ids are separately checked by `is_safe_blob_id`.)
        if vault.id.is_empty() || vault.id.len() > 64 || !vault.id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe vault id in mirror: {:?}", vault.id))));
        }
        vault.settings.volume_max_size = vault.settings.volume_max_size.clamp(MIN_VOLUME_MAX_SIZE, MAX_VOLUME_MAX_SIZE);
        vault.settings.redundancy = vault.settings.redundancy.min(MAX_REDUNDANCY);
        // Drop any tombstones carried in the mirror. The store is rebuilt below by re-putting
        // only the LIVE manifest entries, so no tombstoned frame can exist in the new tree
        // (the same reasoning staged_rewrite uses when it clears deleted_docs). Keeping them
        // would let the set grow unbounded across export/import cycles and, worse, a carried
        // tombstone whose id collides with a re-imported live blob would silently suppress it.
        vault.deleted_docs.clear();
        // The mirror's `categories` were adopted WHOLESALE from the untrusted vault.json. import_tree
        // is the FOURTH untrusted-category path (alongside plan_merge_from / apply_merge_from /
        // sync_types_from_records) and the ONLY one that does not route through the add_* mutators, so
        // a crafted mirror could otherwise inject bidi/zero-width/control-spoofed type names straight
        // into TypeLists (then rendered raw in the Config screen + the type/subtype dropdowns). Rebuild
        // the lists through display_safe + the case-insensitive add_* dedup, exactly like
        // sync_types_from_records, so import stays consistent with the rest of the triad.
        let raw_cats = std::mem::take(&mut vault.categories);
        let mut clean = crate::types::TypeLists::default();
        for t in &raw_cats.asset {
            let t = records::display_safe(t.trim());
            if !t.is_empty() {
                clean.add_asset_type(&t);
            }
        }
        for at in &raw_cats.account {
            let t = records::display_safe(at.name.trim());
            if t.is_empty() {
                continue;
            }
            clean.add_account_type(&t);
            for st in &at.subtypes {
                let st = records::display_safe(st.trim());
                if !st.is_empty() {
                    clean.add_account_subtype(&t, &st);
                }
            }
        }
        vault.categories = clean;
        let dir = parent_dir(dest);
        fs::create_dir_all(&dir)?;
        harden_dir(&dir);
        // Hold the single-writer lock for the WHOLE build. The `dest.exists()` check
        // above is a TOCTOU on its own — two concurrent imports into the same fresh
        // directory could both pass it and then interleave their volume/manifest
        // writes into a corrupt, mixed tree. The lock makes the build exclusive, in
        // keeping with the create/open paths (which lock before writing anything). It
        // is released before the final `OpenVault::open` re-acquires it below.
        let build_lock = WriteLock::acquire(&dir)?;
        let salt = crypto::random_bytes::<SALT_LEN>()?;
        let key = crypto::derive_key_chained(pw1, pw2, &salt, &params)?;

        // Re-encrypt every document from the mirror into a fresh store under the
        // new key (fresh per-blob nonces). Partitions are re-placed by the imported
        // volume_max_size, so the layout reflects the imported settings.
        let mut store = VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size)?;
        let man_dir = src.join("manifest");
        let vol_root = src.join("volume");
        // `read_capped`/`read_bounded` apply O_NOFOLLOW to the FINAL path component only,
        // so a symlinked `manifest/`, `volume/`, or `vol.<p>/` in an untrusted mirror
        // could still redirect reads outside the mirror. Reject symlinked intermediate
        // directories up front (the per-partition `vol.<p>` dirs are checked in the loop).
        reject_symlink_dir(&man_dir)?;
        reject_symlink_dir(&vol_root)?;
        // Reject a mirror that lists the same blob id more than once (across ALL
        // partitions). A duplicate id makes `store.put` append a SECOND frame for one
        // id while only one manifest entry survives — and a later manifest-loss rebuild
        // + volume truncation could then resurrect the OLDER frame, silently rolling
        // the document back to a superseded version (audit R-8). Genuine exports never
        // reuse an id (each is a fresh random hex), so this only rejects crafted mirrors.
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut p = 0u32;
        loop {
            let man_path = man_dir.join(format!("manifest.{p}.json"));
            if !man_path.exists() {
                break; // partitions are contiguous from 0
            }
            let entries: Vec<ManifestEntry> = serde_json::from_slice(&read_capped(&man_path, storage::MAX_MANIFEST_SIZE)?)?;
            // Fail closed on a crafted mirror packing millions of tiny entries into one
            // partition, which would drive the per-entry store.put below O(M²) (round-1 L3 /
            // audit R5-1). Same cap the live store enforces in load_manifest.
            if entries.len() > storage::MAX_MANIFEST_ENTRIES {
                return Err(VaultError::Storage(StorageError::TooLarge));
            }
            let vol_dir = vol_root.join(format!("vol.{p}"));
            reject_symlink_dir(&vol_dir)?; // don't read blobs through a symlinked partition dir
            for e in &entries {
                // The mirror is untrusted input: the blob is read from
                // `vol.<p>/<id>`, so a crafted id containing a path separator or
                // `..` would traverse out of the mirror. Require a plain filename.
                if !is_safe_blob_id(&e.id) {
                    return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe document id in mirror: {:?}", e.id))));
                }
                if !seen_ids.insert(e.id.clone()) {
                    return Err(VaultError::Storage(StorageError::Corrupt(format!("duplicate document id in mirror: {:?}", e.id))));
                }
                // The mirror also supplies the virtual path verbatim; reject control
                // bytes so a crafted mirror can't store a path that injects terminal
                // escapes or NULs into the UI / future consumers. (Length is bounded
                // by `store.put`.)
                if !is_safe_doc_path(&e.path) {
                    return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe document path in mirror: {:?}", e.path))));
                }
                // Size-capped + symlink-rejected read (no OOM, no /dev/zero or
                // arbitrary-file read through a planted symlink).
                let bytes = Zeroizing::new(read_capped(&vol_dir.join(&e.id), MAX_DOC_SIZE)?);
                store.put(&e.id, &e.path, &bytes, e.uploaded_at, &key)?;
            }
            p += 1;
        }
        // FAIL CLOSED on a TAIL-truncated mirror (audit R4-3): the loop stops at the first absent
        // `manifest.<N>.json`, so a mirror missing only its HIGHER (tail) partitions is a valid-looking
        // contiguous prefix 0..k that the middle-gap check below cannot catch (no surviving higher
        // manifest). export_tree records the authoritative partition count up front; require the number
        // read to equal it, so a partial export (e.g. aborted at a partition boundary, then re-imported
        // despite the "partial mirror — shred it" warning) cannot silently drop the orphan documents in
        // the unwritten tail partitions. A mirror without the count (legacy / hand-built) keeps the
        // middle-gap-only guard below.
        if let Ok(raw) = read_capped(&man_dir.join(MIRROR_PARTITIONS_FILE), 32) {
            let expected: u32 = std::str::from_utf8(&raw)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .ok_or_else(|| VaultError::Storage(StorageError::Corrupt("unreadable partition count in mirror".into())))?;
            if p != expected {
                return Err(VaultError::Storage(StorageError::Corrupt(format!(
                    "truncated mirror: imported {p} partition(s) but the export recorded {expected} \
                     (an aborted/partial export-tree must not be imported)"
                ))));
            }
        }
        // FAIL CLOSED on a NON-CONTIGUOUS mirror, exactly like `VolumeStore::open`: the loop
        // above stops at the first absent `manifest.<N>.json`, so a lost MIDDLE partition (a
        // partial copy or selective restore of the mirror) while a HIGHER one survives would
        // otherwise be silently dropped — importing a vault missing every document in the
        // orphaned higher partitions. Detect a surviving higher manifest and refuse.
        if let Some(hi) = highest_mirror_manifest(&man_dir)
            && hi >= p
        {
            return Err(VaultError::Storage(StorageError::Corrupt(format!(
                "non-contiguous partitions in mirror: imported {p} but manifest.{hi}.json still exists \
                 (a middle partition is missing)"
            ))));
        }
        drop(store);

        // Write the encrypted vault (the final commit point), then open it through
        // the normal path so validation + the referenced⊆stored consistency check
        // + the single-writer lock all apply to the freshly-built vault.
        write_vault_file(dest, &vault, &key, &salt, params)?;
        // Release the build lock before reopening: `OpenVault::open` takes its own
        // single-writer lock, which (being a second handle in this process) would
        // otherwise collide with the one still held here.
        drop(build_lock);
        OpenVault::open(dest.to_path_buf(), pw1, pw2)
    }
}

/// Manifest entries selected by an optional partition filter. `Some(n)` returns
/// only partition `n`'s entries (erroring if `n` is out of range); `None`
/// returns every partition's entries.
fn selected_entries(store: &VolumeStore, part: Option<u32>) -> Result<Vec<ManifestEntry>, VaultError> {
    // Branch on whether a specific partition was requested (`Some(p)`) or not (`None`).
    match part {
        Some(p) => {
            // `p as usize` is an explicit numeric cast (u32 -> usize) so it can be
            // compared against the count, which is a `usize`.
            if p as usize >= store.partition_count() {
                return Err(VaultError::NoSuchPartition(p));
            }
            // Iterator: yield this partition's entries (each a `&ManifestEntry`),
            // `.cloned()` turns each borrow into an owned value, `.collect()` into a Vec.
            Ok(store.partition_entries(p).cloned().collect())
        }
        None => Ok(store.entries().cloned().collect()), // all partitions
    }
}

/// Highest `N` for which a file named exactly `manifest.<N>.json` exists in `dir` (strict
/// `<decimal>` between the fixed prefix/suffix), or `None`. Used by `import_tree` to detect a
/// non-contiguous mirror (a missing middle partition), mirroring `VolumeStore::open`'s guard.
pub(super) fn highest_mirror_manifest(dir: &Path) -> Option<u32> {
    let mut hi: Option<u32> = None;
    let rd = fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Some(rest) = name.strip_prefix("manifest.")
            && let Some(num) = rest.strip_suffix(".json")
            && !num.is_empty()
            // Canonical decimal only (no leading zeros), matching storage::highest_partition_index
            // so a foreign mis-named mirror file can't spuriously trip the contiguity guard.
            && (num == "0" || !num.starts_with('0'))
            && num.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = num.parse::<u32>()
        {
            hi = Some(hi.map_or(n, |h| h.max(n)));
        }
    }
    hi
}

/// Read a file from an UNTRUSTED import mirror with a size ceiling, rejecting a
/// symlink at the path. Mirrors the stat-before-read discipline used everywhere
/// else (load_manifest, decrypt_file, add_document) so a crafted mirror cannot
/// OOM the import (a multi-GB manifest/blob) or redirect a read through a symlink
/// (e.g. to `/dev/zero` or an arbitrary file).
fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, VaultError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(VaultError::Storage(StorageError::Corrupt(format!("mirror entry is a symlink: {}", path.display()))));
    }
    // Bound the READ itself (not just a pre-stat), so a file that grows between the
    // stat and the read can't bypass the ceiling or OOM the import (matches
    // `read_file_capped`).
    read_bounded(path, max)
}

/// Read at most `max + 1` bytes from `path`, erroring `TooLarge` if the file holds
/// more than `max`. The `+ 1` lets us detect an over-size file without ever
/// allocating beyond the ceiling, regardless of a concurrent grow-after-stat.
pub(super) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, VaultError> {
    use std::io::Read;
    // Open WITHOUT following a final-component symlink. `read_capped` pre-checks with
    // `symlink_metadata`, but that is a SEPARATE syscall from this open — a TOCTOU an
    // attacker who controls the (untrusted) mirror directory can win by swapping a
    // regular file for a symlink in between, redirecting the read to an arbitrary file
    // (e.g. /etc/shadow) and laundering its bytes into the importer's vault. O_NOFOLLOW
    // closes the race at the open itself, matching `storage::append_frame` and the
    // single-instance lock open. (On non-unix the pre-check remains the guard.)
    #[cfg(unix)]
    let f = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?
    };
    #[cfg(not(unix))]
    let f = fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(VaultError::TooLarge);
    }
    Ok(buf)
}

/// Write one COSMETIC human-tree document copy under `out` at `dest`, rejecting a
/// symlinked intermediate dir first (so a planted symlink can't redirect plaintext
/// outside `out`). Returns an error instead of propagating it via `?` at the call site,
/// so a failed viewing copy (e.g. an over-long filename component) never aborts the
/// authoritative `export_tree` mirror (audit F2). Even on the error path, plaintext only
/// ever lands under `out` (the symlink guard runs before any write).
fn write_human_tree_copy(out: &Path, dest: &Path, data: &[u8]) -> Result<(), VaultError> {
    if let Some(parent) = dest.parent() {
        reject_symlinked_descendants(out, parent)?; // no symlink redirect out of `out`
        fs::create_dir_all(parent)?;
        harden_dir(parent);
    }
    write_new_bytes(dest, data)
}
