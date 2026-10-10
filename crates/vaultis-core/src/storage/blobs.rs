//! Reading and writing documents: lazy single-frame reads, and the mutations (append a
//! frame, then commit the manifest atomically).

use super::*;

impl VolumeStore {
    // --- Reads (lazy: open the one volume, read one frame) -------------------

    /// Decrypt and return one stored document.
    // Returns the plaintext wrapped in `Zeroizing` so the secret bytes are wiped from
    // memory when the caller drops them.
    pub fn read(&self, id: &str, key: &Key) -> Result<Zeroizing<Vec<u8>>, StorageError> {
        // `.get(id)` -> Option<&Located>; `.ok_or_else(..)` converts `None` into an
        // `Err(NotFound)`, then `?` early-returns it. The leading `*` dereferences the
        // borrowed `Located` to a copy (cheap — it is `Copy`).
        let loc = *self.index.get(id).ok_or_else(|| StorageError::NotFound(id.to_string()))?;
        let mut f = File::open(self.volume_path(loc.partition))?; // `?` propagates I/O errors
        let file_len = f.metadata()?.len();
        // Destructure the returned 3-tuple into three named bindings. `&mut f` lends
        // the file exclusively (the callee seeks/reads it); `&self.aad(..)` borrows a
        // freshly built AAD byte vector.
        let (frame_id, frame_path, bytes) =
            read_frame_at(&mut f, file_len, loc.offset, loc.length, key, &self.aad(loc.partition))?;
        // The frame's id/path are authenticated (inside the AEAD plaintext) but the
        // AAD binds only vault_id+partition, so any equal-length authentic frame in
        // the same partition would otherwise decrypt here. Verify the frame's id
        // (and path) match the manifest entry, so a relocated/substituted frame
        // cannot be served under the wrong document identity.
        if frame_id != id {
            return Err(StorageError::Corrupt(format!("frame id mismatch in partition {}", loc.partition)));
        }
        // A "let-chain": this `if` body runs only when BOTH (a) `self.entry(id)` is
        // `Some(expected)` (binding `expected`) AND (b) the stored path differs. If the
        // entry is `None`, the whole condition is false and the body is skipped.
        if let Some(expected) = self.entry(id) {
            if frame_path != expected.path {
                return Err(StorageError::Corrupt(format!("frame path mismatch for {id}")));
            }
            // Verify the DECRYPTED body length matches the manifest-declared `size`. `size` is an
            // independently-serialized field that nothing else cross-checks (read_frame_at only
            // validates the frame `length`), so without this a crafted source vault could declare a
            // small `size` for an authentic OVERSIZE frame to slip past the merge preview's
            // MAX_DOC_SIZE guard, then abort apply post-approval. Checking it here hardens EVERY
            // reader and guarantees an oversize body can never be served/copied under a small size.
            if bytes.len() as u64 != expected.size {
                return Err(StorageError::Corrupt(format!("frame size mismatch for {id}")));
            }
        }
        Ok(bytes)
    }

    // --- Mutations (append + atomic manifest commit) -------------------------

    /// Add or replace a document. A new id goes to the active partition (rolling
    /// to a fresh one if it would exceed `max_size`); an existing id is appended
    /// to **its own** partition (old frame becomes garbage). The append is fsync'd
    /// before the manifest is atomically committed.
    // `&mut self` (mutates the store); takes its inputs by shared borrow — `bytes:
    // &[u8]` is a borrowed byte slice (the doc body, not copied). Returns `Result<(),
    // _>`: `Ok(())` is success with no payload (`()` is the empty/"unit" value).
    pub fn put(&mut self, id: &str, path: &str, bytes: &[u8], uploaded_at: i64, key: &Key) -> Result<(), StorageError> {
        if path.len() > MAX_PATH_LEN {
            return Err(StorageError::PathTooLong);
        }
        if bytes.len() as u64 > MAX_DOC_SIZE { // `as u64` is an explicit numeric cast
            return Err(StorageError::TooLarge);
        }

        let part = self.target_partition(id, bytes.len() as u64);
        let frame = encode_frame(key, &self.vault_id, part, id, path, bytes)?;
        self.ensure_dirs()?;

        // (1) Append the frame at the committed end_offset; fsync the volume.
        // `.map(|m| m.end_offset)` reads the field out of the Option if present;
        // `.unwrap_or(0)` supplies 0 for a not-yet-existing partition.
        let start = self.manifests.get(part as usize).map(|m| m.end_offset).unwrap_or(0);
        append_frame(&self.volume_path(part), start, &frame)?;
        // Fault point: a crash here (volume durable, manifest NOT yet committed)
        // must leave the frame as an ignored tail past end_offset on reopen.
        crate::fault::point("put.after_append")?;

        // (2) Build and atomically commit the new manifest for that partition.
        // `.cloned()` turns `Option<&Manifest>` into an owned `Option<Manifest>`;
        // `.unwrap_or_default()` yields a fresh empty Manifest for a new partition.
        let mut manifest = self.manifests.get(part as usize).cloned().unwrap_or_default();
        // `.retain(closure)` keeps only entries where the closure is true, i.e. drops
        // any prior entry with this id (an update supersedes the old one).
        manifest.entries.retain(|e| e.id != id); // replace any previous entry
        manifest.entries.push(ManifestEntry {
            id: id.to_string(),     // owned copies of the borrowed &str inputs
            path: path.to_string(),
            size: bytes.len() as u64,
            offset: start,
            length: frame.len() as u64,
            uploaded_at, // field shorthand: same as `uploaded_at: uploaded_at`
        });
        manifest.end_offset = start.saturating_add(frame.len() as u64);
        // saturating_add: `seq` is deserialized verbatim from the (authenticated) manifest
        // JSON, so a crafted seq == u64::MAX would otherwise panic here under the release
        // build's overflow-checks. Matches the saturating discipline used for end_offset.
        manifest.seq = manifest.seq.saturating_add(1);
        self.commit_manifest(part, &manifest, key)?; // disk commit point; `?` aborts on failure
        // Fault point: a crash here (both volume + manifest committed) is a fully
        // committed put; reopen must show the document.
        crate::fault::point("put.after_commit")?;

        // Reflect in memory only after the on-disk commit succeeds.
        // If `part` is one past the current end, this is a brand-new partition (push);
        // otherwise overwrite the existing slot (a `move` of `manifest` into the vec).
        if part as usize == self.manifests.len() {
            self.manifests.push(manifest);
        } else {
            self.manifests[part as usize] = manifest;
        }
        self.reindex();
        Ok(())
    }

    /// Remove a document: drop its entry from the partition manifest and commit.
    /// The blob stays in the volume as garbage until reclaimed by a `compact`
    /// volume rewrite (see `OpenVault::compact`).
    pub fn remove(&mut self, id: &str, key: &Key) -> Result<(), StorageError> {
        // `let Some(loc) = .. else { .. }`: if `index.get(id).copied()` is `Some`, bind
        // `loc`; if it's `None` (id not present), run the else block (here: nothing to
        // remove, return success). `.copied()` turns `Option<&Located>` into an owned
        // `Option<Located>` (Located is Copy).
        let Some(loc) = self.index.get(id).copied() else {
            return Ok(());
        };
        let mut manifest = self.manifests[loc.partition as usize].clone(); // work on an owned copy
        manifest.entries.retain(|e| e.id != id); // drop this id's entry (blob stays as garbage)
        manifest.seq = manifest.seq.saturating_add(1); // see put(): avoid overflow panic on a crafted seq
        self.commit_manifest(loc.partition, &manifest, key)?;
        self.manifests[loc.partition as usize] = manifest;
        self.reindex();
        Ok(())
    }

    /// Choose the partition for a `put`: the document's own partition if it exists
    /// (update locality), else the active (last) partition, rolling to a new one
    /// if the frame would push it past `max_size`.
    pub(super) fn target_partition(&self, id: &str, doc_size: u64) -> u32 {
        // `if let Some(loc) = ..` runs the body only when the lookup succeeds, binding
        // the inner value to `loc`. An existing doc stays in its own partition.
        if let Some(loc) = self.index.get(id) {
            return loc.partition;
        }
        // `match` on the last manifest (`Option`): the `Some(m) if ..` arm has a guard
        // (the size check); the next `Some(_)` arm catches a full partition with `_`
        // ignoring the value; `None` means no partitions exist yet.
        match self.manifests.last() {
            // Reserve the worst-case per-frame overhead (prefix + nonce + tag +
            // id/path length prefixes + a full-length path ~= 340 B; round up) so a
            // partition does not overshoot `max_size`. Roll to a fresh partition when
            // EITHER the byte cap OR the entry-count cap would be exceeded: `put` grows
            // the active partition's manifest one entry per new id, and `load_manifest`
            // fails CLOSED (`TooLarge`) on any manifest with more than MAX_MANIFEST_ENTRIES
            // — a variant NOT in `open`'s rebuild set — so without this count roll a user
            // (or a hostile merge/import packing many tiny docs into one partition under a
            // large `max_size`) could write a manifest the very next open rejects,
            // permanently BRICKING an otherwise-intact vault. Rolling on count keeps every
            // partition manifest <= the read-side cap, so the write and read paths agree.
            Some(m)
                if m.end_offset.saturating_add(doc_size).saturating_add(FRAME_OVERHEAD_EST) <= self.max_size
                    && m.entries.len() < MAX_MANIFEST_ENTRIES =>
            {
                (self.manifests.len() - 1) as u32
            }
            Some(_) => self.manifests.len() as u32, // full (bytes or entry count) → new partition
            None => 0,
        }
    }

    pub(super) fn aad(&self, part: u32) -> Vec<u8> {
        volume_aad(&self.vault_id, part)
    }

    pub(super) fn ensure_dirs(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.manifest_dir)?; // make the dir (and parents); no-op if it exists
        fs::create_dir_all(&self.volume_dir)?;
        harden_dir(&self.manifest_dir); // chmod 0700 on unix (see cfg(unix) defs below)
        harden_dir(&self.volume_dir);
        // Make the volume/ and manifest/ directory entries themselves durable, so a
        // crash right after the first write can't lose the subdirectory that holds
        // a just-committed file.
        // `.parent()` returns `Option<&Path>` (None at filesystem root); `if let`
        // syncs the parent dir only when there is one.
        if let Some(vault_dir) = self.manifest_dir.parent() {
            sync_dir(vault_dir);
        }
        Ok(())
    }
}
