//! Manifest I/O: loading, encoding and atomically committing each partition's encrypted
//! document index, its mirrors, and recovering or rebuilding it from the volume.

use super::*;

impl VolumeStore {
    // --- Manifest I/O --------------------------------------------------------

    pub(super) fn load_manifest(&self, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        self.load_manifest_at(&self.manifest_path(part), part, key)
    }

    /// Load a manifest for partition `part` from an explicit `path` — the live
    /// `manifest.<part>` or its spare copy. The AEAD binds vault id + partition, not the
    /// filename, so the spare verifies under exactly the same AAD as the file it copies.
    pub(super) fn load_manifest_at(&self, path: &Path, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        // Reject a symlinked manifest and CAP THE READ ITSELF — not just a pre-stat. A plain
        // `fs::metadata` cap + `fs::read` both FOLLOW a symlink and leave a stat-then-read
        // gap: a `manifest.N` swapped for a symlink to `/dev/zero` (whose stat size is 0, so
        // the cap passes) would otherwise be read UNBOUNDEDLY → OOM on every vault open.
        // `symlink_metadata` does not follow the link (cheap early reject + over-size reject
        // without a big read); the bounded O_NOFOLLOW read closes the residual open-time
        // TOCTOU, matching `vault::read_bounded` and `append_frame`.
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(StorageError::Corrupt(format!("manifest.{part} is a symlink")));
        }
        if meta.len() > MAX_MANIFEST_SIZE {
            return Err(StorageError::TooLarge);
        }
        let raw = read_file_bounded_nofollow(path, MAX_MANIFEST_SIZE)?;
        if raw.len() < NONCE_LEN {
            return Err(StorageError::Corrupt(format!("manifest.{part} truncated")));
        }
        // `split_at(n)` returns two slices borrowing `raw`: the nonce prefix and the
        // ciphertext remainder. The decrypted plaintext is wrapped in `Zeroizing` so
        // it is wiped after use.
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let plain = Zeroizing::new(crypto::decrypt(key, nonce, ct, &manifest_aad(&self.vault_id, part))?);
        // `from_slice` parses JSON into a `Manifest` (type annotation tells serde which
        // type to build). `?` converts a parse error to `StorageError::Json`.
        let manifest: Manifest = serde_json::from_slice(&plain)?;
        // Fail closed on an over-count manifest BEFORE any O(M) consumer touches it (round-1
        // L3 / audit R5-1): the byte cap admits millions of tiny entries, which would make the
        // per-document put/import/compact loop O(M²).
        if manifest.entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(StorageError::TooLarge);
        }
        Ok(manifest)
    }

    /// Encrypt `manifest` for partition `part` into its on-disk form (`nonce ‖ ciphertext`).
    /// Called once per copy so the live file and its spare each get a FRESH random nonce —
    /// two independent ciphertexts of the same contents, never a byte-for-byte duplicate.
    pub(super) fn encode_manifest(&self, part: u32, manifest: &Manifest, key: &Key) -> Result<Vec<u8>, StorageError> {
        let plain = Zeroizing::new(serde_json::to_vec(manifest)?); // serialize to JSON bytes, wiped after
        // `random_bytes::<NONCE_LEN>()` is a generic call: `::<N>` picks the array
        // length at compile time, returning `[u8; NONCE_LEN]`.
        let nonce = crypto::random_bytes::<NONCE_LEN>()?;
        let ct = crypto::encrypt_with_nonce(key, &nonce, &plain, &manifest_aad(&self.vault_id, part))?;
        // `with_capacity` pre-allocates the exact size to avoid reallocations.
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce); // on-disk layout: nonce ‖ ciphertext
        blob.extend_from_slice(&ct);
        Ok(blob)
    }

    /// Write `manifest.<part>` atomically: temp → fsync → rename → fsync dir.
    ///
    /// That rename is the storage layer's commit point. The spare copy is written only
    /// AFTER it succeeds, so a failed commit can never leave a spare that is newer than
    /// the manifest it stands in for — the same ordering `save_internal` uses for the
    /// vault file's own copies. Keeping the spare is best-effort: it must not fail a
    /// document write, because the authoritative manifest is already durable by then.
    pub(super) fn commit_manifest(&self, part: u32, manifest: &Manifest, key: &Key) -> Result<(), StorageError> {
        self.ensure_dirs()?;
        let blob = self.encode_manifest(part, manifest, key)?;
        write_atomic(&self.manifest_path(part), &blob)?;
        if self.redundancy > 0 {
            self.write_manifest_mirror(part, manifest, key);
        } else {
            // Redundancy off: drop any spare left over from when it was on, so the
            // setting genuinely stops leaving extra encrypted copies on disk.
            let _ = fs::remove_file(self.manifest_mirror_path(part));
        }
        Ok(())
    }

    /// Best-effort spare copy of a just-committed manifest. Errors are dropped on
    /// purpose — see [`Self::commit_manifest`].
    pub(super) fn write_manifest_mirror(&self, part: u32, manifest: &Manifest, key: &Key) {
        if let Ok(blob) = self.encode_manifest(part, manifest, key) {
            let _ = write_atomic(&self.manifest_mirror_path(part), &blob);
        }
    }

    /// Get partition `part`'s manifest back when the live file is unusable (missing,
    /// corrupt, or written under a key it does not verify against).
    ///
    /// Prefers the spare copy: it holds the SAME contents as the file it replaces, so
    /// recovering from it loses nothing at all. Rebuilding from the volume is the
    /// fallback, and a lossier one — the scan can only index frames it can still read,
    /// so damage in the volume costs entries the spare would have kept.
    ///
    /// A spare is accepted only if it agrees with the volume it indexes: `end_offset`
    /// past the end of the volume file would send the next append into a hole beyond
    /// EOF, so such a copy is rejected in favour of the scan.
    ///
    /// A spare may also be BEHIND the volume. The live manifest is committed first and
    /// the spare written after it, so a failure (or a crash) in that gap leaves a spare
    /// describing one commit ago while the volume already holds the newer frames. Taking
    /// it at face value would be the worst kind of wrong: its short `end_offset` is the
    /// next append point, so the following upload would overwrite those newer frames, and
    /// any record pointing at one of them would fail the open-time `referenced ⊆ stored`
    /// check first. So the region the spare does not cover is scanned and merged in.
    pub(super) fn recover_manifest(&self, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        let volume_len = fs::metadata(self.volume_path(part)).map(|m| m.len()).unwrap_or(0);
        if let Ok(mut m) = self.load_manifest_at(&self.manifest_mirror_path(part), part, key)
            && m.end_offset <= volume_len
        {
            if m.end_offset < volume_len {
                self.merge_volume_tail(&mut m, part, key)?;
            }
            return Ok(m);
        }
        self.rebuild_manifest(part, key)
    }

    /// Scan the volume past `m.end_offset` and fold whatever authentic frames are there
    /// into `m`, so a spare manifest that lags the volume still yields the complete
    /// index. Later frames win for a repeated id (an update appends a newer frame), which
    /// is the same last-write-wins rule [`scan_volume`] applies within a scan.
    pub(super) fn merge_volume_tail(&self, m: &mut Manifest, part: u32, key: &Key) -> Result<(), StorageError> {
        let mut f = File::open(self.volume_path(part))?;
        let file_len = f.metadata()?.len();
        let aad = volume_aad(&self.vault_id, part);
        let tail = scan_volume_from(&mut f, file_len, m.end_offset, key, &aad);
        for e in tail.entries {
            m.entries.retain(|x| x.id != e.id); // the newer frame supersedes the older entry
            m.entries.push(e);
        }
        // Never move the append point backwards: `max` keeps it at the end of the last
        // frame either source could see.
        m.end_offset = m.end_offset.max(tail.end_offset);
        // The same cap `load_manifest` and the rebuild enforce — a merge must not be the
        // one path that admits an over-count manifest into memory.
        if m.entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(StorageError::TooLarge);
        }
        Ok(())
    }

    /// Reconstruct a partition manifest by scanning its self-describing volume up
    /// to the last decryptable frame (recovery for a lost/corrupt manifest).
    pub(super) fn rebuild_manifest(&self, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        let mut f = File::open(self.volume_path(part))?;
        let file_len = f.metadata()?.len();
        let aad = volume_aad(&self.vault_id, part);
        reject_over_cap(scan_volume(&mut f, file_len, key, &aad), MAX_MANIFEST_ENTRIES)
    }
}
