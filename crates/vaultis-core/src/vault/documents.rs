//! Attached documents, delegated to the partitioned store ([`crate::storage`]).

use super::*;

impl OpenVault {
    /// Add the file at `source` under virtual directory `location` with name
    /// `filename`. Commits the blob + its manifest; the caller links the new id
    /// onto a record and saves the vault (the final commit). Returns the id.
    pub fn add_document(&mut self, location: &str, filename: &str, source: &Path) -> Result<String, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // `source` is a user-chosen file. `fs::metadata` follows symlinks, so a
        // symlink to a real document is fine, but a non-regular file (character
        // device like /dev/zero, a FIFO, …) reports len()==0 yet reads unboundedly —
        // reject it up front so it can't drive an OOM.
        let meta = fs::metadata(source)?;
        if !meta.file_type().is_file() {
            return Err(VaultError::Storage(StorageError::Corrupt(format!(
                "document source is not a regular file: {}",
                source.display()
            ))));
        }
        if meta.len() > MAX_DOC_SIZE {
            return Err(VaultError::TooLarge);
        }
        let vpath = virtual_path(location, filename);
        if vpath.len() > storage::MAX_PATH_LEN {
            return Err(VaultError::Storage(StorageError::PathTooLong));
        }
        // Read into memory wrapped in `Zeroizing` (plaintext wiped on drop), with a
        // HARD ceiling rather than the unbounded `fs::read`: a file that grows between
        // the stat and the read — or a special file that slips past the is_file()
        // check on an exotic filesystem — still cannot exhaust memory.
        let data = read_file_capped(source, MAX_DOC_SIZE)?;
        let id = records::random_id()?;
        self.storage.put(&id, &vpath, &data, records::unix_now(), &self.key)?;
        Ok(id)
    }

    /// Permanently remove a stored document by id (drops its manifest entry; the
    /// blob lingers as garbage until reclaimed by a `compact` volume rewrite).
    ///
    /// Refuses to remove a blob that a record still references: dropping it would save
    /// a dangling reference and brick the vault on the next open (`referenced ⊄ stored`
    /// → `ArchiveMismatch`). Callers must unlink the document from its record first
    /// (the UIs already do); a stray call now fails closed with `StillReferenced`
    /// instead of corrupting the vault.
    pub fn remove_document(&mut self, file_id: &str) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        if referenced_doc_ids(&self.vault).iter().any(|r| r == file_id) {
            return Err(VaultError::StillReferenced);
        }
        // Tombstone the id so that, if a later manifest-loss rebuild re-admits the
        // still-physically-present frame, the readers below suppress it and the next
        // volume rewrite drops it for good — a lazy delete can't be resurrected
        // (audit R-2). Deduplicated; cleared by `staged_rewrite` after the rewrite.
        //
        // Persist the tombstone BEFORE physically dropping the manifest entry, not after:
        // the two are separate durable commits, and a crash in the gap must fail SAFE. The
        // tombstone-then-remove order leaves "tombstone without removal" on a crash (the
        // doc reads as deleted and is idempotently re-removable / dropped by the next
        // compaction) instead of "removal without tombstone" (the deleted frame silently
        // resurrects on a later manifest-loss rebuild). Callers persist the record→doc
        // unlink before calling this; the extra save here makes the tombstone durable too.
        let id = file_id.to_string();
        if !self.vault.deleted_docs.contains(&id) {
            self.vault.deleted_docs.push(id);
            self.save()?;
        }
        self.storage.remove(file_id, &self.key)?;
        Ok(())
    }

    /// True if `file_id` has been tombstoned by `remove_document` — used to suppress
    /// a frame that a manifest-loss rebuild may have resurrected.
    pub(super) fn is_tombstoned(&self, file_id: &str) -> bool {
        self.vault.deleted_docs.iter().any(|d| d == file_id)
    }

    /// Decrypt and return one stored document.
    pub fn read_document(&self, file_id: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        // A tombstoned id is treated as absent even if a rebuild resurrected its frame.
        if self.is_tombstoned(file_id) {
            return Err(VaultError::Storage(StorageError::NotFound(file_id.to_string())));
        }
        Ok(self.storage.read(file_id, &self.key)?)
    }

    /// Write a stored document out to `dest` as an **unencrypted** copy (O_EXCL +
    /// 0600; fails if `dest` exists).
    pub fn export_document(&self, file_id: &str, dest: &Path) -> Result<(), VaultError> {
        let data = self.read_document(file_id)?;
        write_new_bytes(dest, &data)?;
        sync_parent_dir(dest); // dir entry durable too (best-effort; no-op off unix), like CSV export
        Ok(())
    }

    /// Export a stored document into `root`, **recreating its virtual folder structure**
    /// under it (`<root>/<location>/<filename>`) and creating the intermediate dirs
    /// (0700). The plaintext file is written 0600; if the target already exists a `_N`
    /// suffix is used so an export never overwrites. Returns the path written.
    ///
    /// The virtual path's components are already sanitized when a document is stored, but
    /// each is re-cleaned here (drop empty / `.` / `..` / separator-bearing components) as
    /// defense in depth, so the result can never escape `root`. Used by the UIs so the
    /// user sets ONE export directory and every export lands in the same tree layout
    /// instead of being prompted for a path each time.
    pub fn export_document_into(&self, file_id: &str, root: &Path) -> Result<PathBuf, VaultError> {
        let vpath =
            self.doc_path(file_id).ok_or_else(|| StorageError::NotFound(file_id.to_string()))?;
        // Recreate the virtual folder tree under `root` via the shared, hardened sanitizer
        // (drops `..`/separators/`:`/NUL, neutralizes control+bidi + Windows reserved names,
        // strips edge dots/spaces; degenerate path -> `<id>.bin`) so it can never escape `root`.
        let dest = root.join(doc_tree_relpath(&vpath, file_id));
        let data = self.read_document(file_id)?;
        if let Some(parent) = dest.parent() {
            // The components above are sanitized, but `root` is a user-chosen, reused dir a
            // local process could have seeded with a symlinked component; `create_dir_all`
            // would follow it and write plaintext outside `root`. Reject symlinked ancestors
            // first (same guard the import side uses), then create + harden.
            reject_symlinked_descendants(root, parent)?;
            fs::create_dir_all(parent)?;
            harden_dir(parent);
        }
        let dest = unique_export_path(dest, None); // never overwrite an existing export
        write_new_bytes(&dest, &data)?;
        sync_parent_dir(&dest); // dir entry durable too (best-effort; no-op off unix), like CSV export
        Ok(dest)
    }

    /// The virtual path ("/loc/filename") of a stored document, for UI display.
    // `&str` is a borrowed string slice (read-only view); `String` is owned. The
    // `Option<String>` return is `Some(path)` if the id exists, else `None`.
    pub fn doc_path(&self, file_id: &str) -> Option<String> {
        if self.is_tombstoned(file_id) {
            return None;
        }
        // `.map(|e| e.path.clone())` transforms a `Some(entry)` into `Some(owned_path)`,
        // leaving `None` as `None`. We `.clone()` because `e` is only a borrow.
        self.storage.entry(file_id).map(|e| e.path.clone())
    }

    /// Whether a document id is present in the store (and not tombstoned).
    pub fn has_document(&self, file_id: &str) -> bool {
        self.storage.contains(file_id) && !self.is_tombstoned(file_id)
    }
}

/// Read a file with a hard size ceiling (unlike `fs::read`, which allocates without
/// bound). Reads at most `max + 1` bytes — one past the limit — so an over-size
/// source is detected and rejected without ever allocating more than `max + 1`.
/// Follows symlinks (the caller has already vetted the target with `fs::metadata`).
pub(super) fn read_file_capped(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    use std::io::Read;
    let f = fs::File::open(path)?;
    // Pre-size to the file's actual length (clamped to the ceiling, +1 to still detect an
    // over-size file) so `read_to_end` never REALLOCATES. A growing Vec frees each smaller
    // backing buffer WITHOUT zeroizing, stranding cleartext fragments in freed heap; with
    // exact capacity the only live buffer is this `Zeroizing` one, wiped on drop. The
    // `take(max + 1)` bound still lets an over-size source be rejected without allocating
    // past the ceiling.
    let hint = f.metadata().map(|m| m.len()).unwrap_or(0).min(max).saturating_add(1);
    let mut buf = Zeroizing::new(Vec::with_capacity(hint as usize));
    f.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(VaultError::TooLarge);
    }
    Ok(buf)
}
