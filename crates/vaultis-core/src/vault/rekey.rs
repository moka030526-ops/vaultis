//! Password change: re-encrypting the whole tree under a fresh key through a staged,
//! rolled-forward protocol, and recovering a rekey interrupted by a crash.

use super::*;

// --- Password-change (rekey) staging recovery --------------------------------

impl OpenVault {
    /// Re-key under two new passwords via a **full re-encryption** of the vault and
    /// the entire document store, staged then rolled forward so a crash leaves
    /// either the old or the new tree fully working (never a mix).
    pub fn change_password(&mut self, pw1: &[u8], pw2: &[u8]) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Derive a brand-new key under a fresh salt, then drive the shared staged
        // full-rewrite: re-encrypt every live document and the vault under the new
        // key, stage it, and atomically swap it in. `Some(...)` tells
        // `staged_rewrite` to ADOPT the new key/salt once the commit succeeds; the
        // transform records the rotation in the audit log.
        let new_salt = crypto::random_bytes::<SALT_LEN>()?;
        let new_key = crypto::derive_key_chained(pw1, pw2, &new_salt, &self.params)?;
        self.staged_rewrite(Some((new_key, new_salt)), |v| {
            v.audit.push(Change::new("password_changed", String::new()));
        })
    }

    /// The shared **staged full-rewrite** behind both `change_password` and
    /// `compact`. It re-encrypts every *live* document (and the vault) into the
    /// `.rekey` staging directory, writes a `READY` marker, then atomically swaps
    /// the new tree into place via `commit_rekey`. A crash before `READY` is
    /// discarded on reopen (the old tree stands); a crash after it rolls forward
    /// (`recover_pending_rekey`). On a partial commit the live handle is poisoned
    /// (`read_only`) so the caller must reopen and finish the idempotent commit.
    ///
    /// `new_key` is `Some((key, salt))` to re-key (the staged tree is encrypted
    /// under the new key, adopted on success) or `None` to reuse the current
    /// key/salt (compaction — reads and writes both use `self.key`, with fresh
    /// per-frame nonces). `transform` mutates the staged vault clone before it is
    /// written (e.g. trim history, append an audit event). The write-generation is
    /// always bumped so the committed tree is detectably newer than any snapshot.
    pub(super) fn staged_rewrite(
        &mut self,
        new_key: Option<(Key, [u8; SALT_LEN])>,
        transform: impl FnOnce(&mut Vault),
    ) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let dir = parent_dir(&self.path);
        let staging = dir.join(REKEY_DIR);
        let _ = fs::remove_dir_all(&staging); // clear any stale staging
        fs::create_dir_all(&staging)?;
        harden_dir(&staging);
        // fsync the vault dir so the `.rekey` directory ENTRY itself is durable before
        // any staged content (and the READY marker) is written into it — otherwise a
        // power loss could lose the whole staging directory, defeating the roll-forward.
        sync_parent_dir(&staging);

        // The key/salt the STAGED tree is encrypted under: the new key when
        // re-keying, else the current key (compaction). Reads always decrypt under
        // the CURRENT key (`self.key`). `match &new_key` borrows, so `new_key`
        // stays available to move out of after the staged tree is written.
        let (write_key, write_salt) = match &new_key {
            Some((k, s)) => (k, s),
            None => (&self.key, &self.salt),
        };

        // Re-encrypt every LIVE document into the fresh staged store. Iterating
        // `self.storage.ids()` yields the manifest-referenced blobs (dead frames from
        // updates/deletes are dropped here — this is what makes the rewrite double as a
        // volume compaction), EXCEPT any id carrying a deletion tombstone. A tombstoned
        // id can only be present because a manifest-loss rebuild re-admitted a deleted
        // frame; excluding it here means a delete stays deleted instead of being baked
        // in permanently (audit R-2). Unreferenced-but-not-deleted orphans (e.g. a doc
        // added but not yet linked) are deliberately KEPT, preserving the "compaction
        // never silently drops a not-yet-reclaimed blob" guarantee.
        let mut new_store =
            VolumeStore::open(&staging, write_key, &self.vault.id, self.vault.settings.volume_max_size)?;
        // Keep a blob if it is NOT tombstoned, OR if a record still references it. Dropping
        // a referenced-but-tombstoned blob would leave a dangling reference in the rewritten
        // vault (`deleted_docs.clear()` below wipes the tombstone) and brick it on next open
        // with ArchiveMismatch. That contradictory "referenced AND tombstoned" state should
        // not arise via the API (remove_document refuses a referenced id), but a crash that
        // lost the unlink-save while persisting the tombstone, then a manifest-loss rebuild,
        // can produce it — so reference wins here and the document is healed back to live.
        let referenced = referenced_doc_ids(&self.vault);
        let ids: Vec<String> = self
            .storage
            .ids()
            .filter(|id| !self.vault.deleted_docs.iter().any(|d| d == id) || referenced.iter().any(|r| r == id))
            .map(|s| s.to_string())
            .collect();
        for id in &ids {
            let bytes = self.storage.read(id, &self.key)?; // decrypt under the CURRENT key
            // `id` came from `self.storage.ids()` (the in-memory index), so its manifest
            // `entry` must exist. Fail CLOSED if it doesn't rather than silently writing
            // an empty path / `uploaded_at = 0`: a missing entry means the index and
            // manifest have desynced, and silently defaulting would bake corrupt metadata
            // into the rewritten store with no error (audit — `unwrap_or_default` removed).
            let entry = self
                .storage
                .entry(id)
                .ok_or_else(|| StorageError::Corrupt(format!("index/manifest desync: no manifest entry for {id}")))?;
            new_store.put(id, &entry.path, &bytes, entry.uploaded_at, write_key)?; // encrypt under the staged key
        }
        drop(new_store); // flush/close the staged store before commit

        // If the live tree has a volume directory (possibly full of garbage) but
        // the staged store wrote no partitions — e.g. every document was deleted,
        // the maximum-garbage case — materialize empty staged `volume/`+`manifest/`
        // dirs so `commit_rekey` swaps the garbage dirs OUT. Otherwise `replace_dir`
        // no-ops on the absent staged dirs and the live garbage would survive.
        if self.storage.partition_count() > 0 {
            for sub in ["volume", "manifest"] {
                let d = staging.join(sub);
                fs::create_dir_all(&d)?;
                harden_dir(&d);
            }
        }

        // Stage the rewritten vault: clone, bump the write-generation, apply the
        // caller's transform, write it, then mark the staging complete with READY.
        let mut staged_vault = self.vault.clone();
        staged_vault.generation = staged_vault.generation.saturating_add(1);
        // The staged volume was just re-encrypted from the (tombstone-filtered) live
        // ids, so no tombstoned frame exists on disk anymore — drop the tombstones so
        // the set can't grow without bound across rekeys/compactions.
        staged_vault.deleted_docs.clear();
        transform(&mut staged_vault);
        write_vault_file(&staging.join(VAULT_FILE), &staged_vault, write_key, write_salt, self.params)?;
        write_new_bytes(&staging.join(REKEY_READY), b"ready")?;
        sync_parent_dir(&staging.join(REKEY_READY));

        // commit_rekey moves volume/ then manifest/ then vault.pmv (the final commit
        // point). A partial failure leaves a half-new tree while this handle is
        // stale: poison it so the caller must reopen (which finishes the idempotent
        // roll-forward). A crash here recovers the same way on the next open.
        if let Err(e) = commit_rekey(&dir, &staging) {
            self.read_only = true; // poison this handle so the caller must reopen
            return Err(e);
        }

        // The on-disk tree is now the committed new tree. Adopt the new key/salt
        // when re-keying (moving `new_key` in drops & zeroizes the old `Key`); for
        // compaction the key/salt are unchanged. Then reopen the store so the
        // in-memory index reflects the re-keyed/compacted volume.
        if let Some((k, s)) = new_key {
            self.key = k;
            self.salt = s;
        }
        self.vault = staged_vault;
        self.previous_generation = self.vault.generation;
        match VolumeStore::open(&dir, &self.key, &self.vault.id, self.vault.settings.volume_max_size) {
            Ok(mut store) => {
                // A freshly opened store defaults to redundancy off; tell it the setting
                // again or the manifests it commits after this would drop their spares.
                store.set_redundancy(self.vault.settings.redundancy);
                self.storage = store;
                // commit_rekey cleared the old-key redundancy copies; regenerate them
                // under the NEW key NOW so the configured protection isn't absent in
                // the window until the next ordinary save (§12.8). Best-effort.
                self.refresh_redundancy_copies();
                Ok(())
            }
            Err(e) => {
                self.read_only = true; // mismatched handle; force a fresh open
                Err(e.into())
            }
        }
    }
}

/// Recover an interrupted password change found at `<dir>/.rekey`:
/// a `READY` marker means the new tree is complete → **roll forward** (commit);
/// no marker means staging was incomplete → **discard** it (the old tree stands).
/// In read-only mode we cannot write, so a pending rekey is reported.
pub(super) fn recover_pending_rekey(dir: &Path, read_only: bool) -> Result<(), VaultError> {
    let staging = dir.join(REKEY_DIR);
    if !staging.exists() {
        return Ok(()); // nothing pending — the common case
    }
    if read_only {
        return Err(VaultError::RekeyPending); // can't write, so can't recover; report it
    }
    if staging.join(REKEY_READY).exists() {
        commit_rekey(dir, &staging)?; // marker present -> the new tree is complete -> finish it
    } else {
        let _ = fs::remove_dir_all(&staging); // no marker -> incomplete -> throw it away (best-effort)
    }
    Ok(())
}

/// Commit a staged rekey by moving the new tree into place: volumes and manifests
/// first, then the vault file **last** (the commit point). Idempotent: re-running
/// after a partial move finishes the remaining items.
pub(super) fn commit_rekey(dir: &Path, staging: &Path) -> Result<(), VaultError> {
    replace_dir(&dir.join("volume"), &staging.join("volume"))?;
    // Fault point: a crash here (new volume in place, old manifest+vault still
    // live, .rekey still present with READY) must roll forward on the next open.
    crate::fault::point("rekey.after_volume")?;
    replace_dir(&dir.join("manifest"), &staging.join("manifest"))?;
    crate::fault::point("rekey.after_manifest")?;
    replace_path(&dir.join(VAULT_FILE), &staging.join(VAULT_FILE))?;
    crate::fault::point("rekey.after_vault")?;
    // The in-place redundancy copies (mirror + prior generations) are now under the
    // OLD key/garbage layout — drop them. The next normal save regenerates them under
    // the new key (if redundancy is still enabled). Idempotent across a re-run.
    cleanup_redundancy(&dir.join(VAULT_FILE));
    sync_parent_dir(&dir.join(VAULT_FILE));
    let _ = fs::remove_dir_all(staging);
    // A rekey/compact is also a committed vault change — refresh the marker AFTER the swap.
    touch_last_update(dir);
    Ok(())
}

/// Replace `live` with `staged` (a directory) if `staged` still exists.
pub(super) fn replace_dir(live: &Path, staged: &Path) -> Result<(), VaultError> {
    let old = sibling_old(live); // a temporary ".<name>.old" path next to `live`
    // Sweep any leftover ".<name>.old" FIRST — before the early return below. A
    // crash AFTER `rename(staged, live)` but BEFORE the trailing cleanup leaves the
    // OLD-key-encrypted dir behind; recovery re-enters here with `staged` already
    // gone, so cleaning up only after the `staged.exists()` guard would leak that
    // old-key ciphertext on disk forever (defeating change_password's forward
    // secrecy). Doing it here makes the cleanup unconditional and idempotent.
    let _ = fs::remove_dir_all(&old);
    if !staged.exists() {
        return Ok(());
    }
    if live.exists() {
        fs::rename(live, &old)?; // move the current dir aside...
    }
    fs::rename(staged, live)?; // ...then move the staged dir into its place
    // Make THIS swap durable before the caller proceeds to the next one. Without this
    // barrier the directory renames in `commit_rekey` (volume → manifest → vault.pmv)
    // can reach disk out of program order on a power loss, leaving a NEW-key vault.pmv
    // durable while volume/manifest are still OLD-key — an unopenable vault that the
    // roll-forward cannot repair. The fsync forces new-volume-durable-before-new-vault.
    sync_parent_dir(live);
    let _ = fs::remove_dir_all(&old); // drop the old copy (best-effort; harmless if it lingers)
    Ok(())
}

/// Replace `live` with `staged` (a file) if `staged` still exists.
pub(super) fn replace_path(live: &Path, staged: &Path) -> Result<(), VaultError> {
    if !staged.exists() {
        return Ok(());
    }
    fs::rename(staged, live)?;
    // Durability barrier — same reasoning as `replace_dir`: the vault.pmv rename is the
    // rekey commit point and must be durable before staging (with its READY marker) is
    // removed, so a crash never loses the commit while erasing its source of truth.
    sync_parent_dir(live);
    Ok(())
}

pub(super) fn sibling_old(path: &Path) -> PathBuf {
    // `.file_name()` -> `Option<&OsStr>`; `.and_then(|n| n.to_str())` chains another
    // optional step (the name may not be valid UTF-8, giving `None`); `.unwrap_or("x")`
    // supplies a fallback name if either step yielded `None`.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(format!(".{name}.old")),
        _ => PathBuf::from(format!(".{name}.old")),
    }
}
