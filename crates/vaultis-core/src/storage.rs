//! Partitioned, lazily-loaded, crash-safe document store (format v4).
//!
//! Replaces the single `<vault>.vol` archive. Documents live under a user-chosen
//! directory in **append-only, per-blob-encrypted volumes** (`volume/vol.<N>`),
//! each indexed by an **encrypted manifest** (`manifest/manifest.<N>`). See
//! `docs/PLAN.md` and `DESIGN.md` §11 for the full design.
//!
//! Crash-safety backbone (per add/update/delete): (1) append the encrypted frame
//! to `vol.N` and **fsync**; then (2) atomically swap `manifest.N` (temp → fsync →
//! rename → fsync dir) — the storage-layer **commit point**.
//! The manifest's `end_offset` is authoritative for where valid data ends, so a
//! torn trailing frame from a crash is ignored and overwritten. A lost/corrupt
//! manifest is **rebuilt by scanning** its self-describing volume. The caller
//! (the vault) commits last, so anything here not referenced by the vault is
//! harmless garbage. Net: any crash recovers to the last fully-committed state.
//!
//! On-disk volume frame: `[u32 frame_len][nonce(24)][ciphertext]`, where
//! `ciphertext = AEAD(key, nonce, plaintext, aad = PREFIX|vault_id|partition)` and
//! `plaintext = [u32 id_len][id][u32 path_len][path][doc_bytes]`. The id/path live
//! inside the (authenticated) plaintext — not the AAD — so a rebuild can decrypt a
//! frame without first knowing them.
//!
//! --- Rust orientation for non-Rust readers (this file) ---
//! - `&T` is a *shared/read-only borrow* (a pointer the callee may read but not
//!   own); `&mut T` is an *exclusive borrow* (may mutate). Passing `&x` lends `x`
//!   without giving it away. `.clone()` makes an independent owned copy.
//! - `Result<T, E>` is either `Ok(T)` (success) or `Err(E)` (failure); `Option<T>`
//!   is either `Some(T)` or `None`. The `?` operator means "if this is `Err`/`None`,
//!   stop and return it from the current function" — concise error propagation.
//! - `unwrap()`/`expect(..)` extract the inner value but *panic* (abort) if it's
//!   `Err`/`None`; used only where the value is provably present.
//! - `match`/`if let`/`let ... else` are pattern-matching control flow.
//! - `Vec<T>` is a growable array; `String` is an owned text buffer, `&str` a
//!   borrowed view of one; `&[u8]` is a borrowed byte slice; `Path`/`PathBuf` are
//!   the borrowed/owned filesystem-path types.
//! - `#[derive(..)]` auto-generates trait implementations; `impl T { .. }` defines
//!   methods on a type; traits are like interfaces. `Zeroizing<_>` wipes its bytes
//!   from memory when dropped (secret hygiene).

use std::collections::BTreeMap; // ordered map (sorted by key), unlike a hash map
use std::fs::{self, File, OpenOptions}; // filesystem APIs (`self` re-exports the `fs` module itself)
use std::io::{Read, Seek, SeekFrom, Write}; // traits for byte readers/writers and seeking
use std::path::{Path, PathBuf}; // borrowed / owned filesystem paths

use serde::{Deserialize, Serialize}; // (de)serialization derives (used for the manifest <-> JSON)
use thiserror::Error; // derive macro that builds the std `Error` trait + messages for our enum
use zeroize::Zeroizing; // wrapper that zeroes the wrapped bytes on drop (don't leave secrets in RAM)

// `crypto::self` re-exports the module so we can call `crypto::decrypt(..)` etc.;
// `Key`, `CryptoError`, `NONCE_LEN` are pulled in by name.
use crate::crypto::{self, CryptoError, Key, NONCE_LEN};

/// AAD prefixes — separate domains for manifests and volume frames.
const MANIFEST_AAD_PREFIX: &[u8] = b"PMVAULT-MANIFEST-v1\0";
const VOLUME_AAD_PREFIX: &[u8] = b"PMVAULT-VOLUME-v1\0";

/// Max bytes for one stored document (bounds allocation on read/rebuild).
pub const MAX_DOC_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB
/// Max length (bytes) of a document's virtual path (`location` + `/` + filename).
pub const MAX_PATH_LEN: usize = 256;
/// Default partition size cap; new documents roll to a fresh partition past this.
pub const DEFAULT_VOLUME_MAX_SIZE: u64 = 256 * 1024 * 1024; // 256 MiB
/// Hard ceiling on a single manifest file (DoS guard).
pub const MAX_MANIFEST_SIZE: u64 = 256 * 1024 * 1024;
/// Hard ceiling on the NUMBER of entries in one partition manifest (round-1 L3 / audit
/// R5-1). The byte cap above still admits millions of minimal (~70-byte) entries, and the
/// per-document `put`/import loop re-serializes + re-encrypts the whole partition manifest
/// each time — O(M) per entry, O(M²) overall — so a crafted mirror or vault packing millions
/// of tiny documents into one partition (by adopting a huge `volume_max_size`) could hang
/// open/import/merge/compact for hours. This caps a partition to ~100k entries — orders of
/// magnitude above any real vault — and fails closed (`TooLarge`) on deserialize, before the
/// quadratic loop runs.
pub const MAX_MANIFEST_ENTRIES: usize = 100_000;

const FRAME_PREFIX_LEN: u64 = 4; // the `[u32 frame_len]`
/// Worst-case per-frame on-disk overhead (prefix + nonce + tag + the two length
/// prefixes + a full-length virtual path), reserved when deciding partition
/// rollover so a partition does not overshoot its size cap.
const FRAME_OVERHEAD_EST: u64 = 512;

// The error type for this module. `enum` = a tagged union: a value is exactly one
// of these variants. `#[derive(Error, Debug)]` auto-generates the std `Error` trait
// (so `?` works) plus a debug printer. Each `#[error("..")]` is the human-readable
// message; `{0}` interpolates the variant's first field.
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("document not found: {0}")]
    NotFound(String),
    #[error("virtual path exceeds {MAX_PATH_LEN} bytes")]
    PathTooLong,
    #[error("document or manifest exceeds the maximum allowed size")]
    TooLarge,
    #[error("document store is corrupt: {0}")]
    Corrupt(String),
    // `#[from]` auto-implements `From<CryptoError>`, so a `CryptoError` hit by `?`
    // is automatically converted into `StorageError::Crypto`. `transparent` reuses
    // the inner error's message verbatim.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    // Same auto-conversion for std I/O errors and serde_json errors: any `?` on a
    // call returning those error types lands in the matching variant here.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// One document's entry in a partition manifest.
// `struct` = a record of named fields. The derives auto-generate: JSON
// (de)serialization (Serialize/Deserialize), `.clone()` (Clone), debug printing
// (Debug), and `==` equality (PartialEq/Eq). `pub` fields are visible outside this
// module.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    pub id: String,
    /// Virtual path: normalized `location` + `/` + filename (<= MAX_PATH_LEN).
    pub path: String,
    /// Plaintext document size in bytes.
    pub size: u64,
    /// Byte offset of the frame (its `[u32 frame_len]`) within the volume.
    pub offset: u64,
    /// Total on-disk frame length (`4 + frame_len`).
    pub length: u64,
    pub uploaded_at: i64,
}

/// A partition manifest (encrypted on disk as `nonce ‖ ciphertext`).
// `Default` here adds `Manifest::default()` (all fields zero/empty), used when a
// partition has no manifest yet.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Monotonic per-partition write counter.
    pub seq: u64,
    /// Committed valid length of the volume — the append point. Authoritative;
    /// bytes beyond it are a torn/garbage tail to ignore.
    pub end_offset: u64,
    pub entries: Vec<ManifestEntry>,
}

/// Where a document currently lives (in-memory lookup).
// `Copy` means this small struct is duplicated bit-for-bit on assignment (no move),
// so passing it around never invalidates the original. No `pub`, so it's private to
// this module.
#[derive(Clone, Copy, Debug)]
struct Located {
    partition: u32,
    offset: u64,
    length: u64,
    /// Position of this id's entry within its partition's `entries` Vec, so `entry()` is
    /// O(1) instead of a linear scan (kept in sync by `reindex` after every mutation; a
    /// stale value is caught by the id check in `entry`, which falls back to a scan).
    entry_index: u32,
}

/// The partitioned document store for one vault directory.
// Owns its directory paths and the decrypted manifests + lookup index in memory.
pub struct VolumeStore {
    manifest_dir: PathBuf,
    volume_dir: PathBuf,
    vault_id: String,
    max_size: u64,
    /// `manifests[p]` is the manifest for partition `p` (partitions are 0..N).
    manifests: Vec<Manifest>,
    /// id -> location, rebuilt from `manifests` after each change.
    index: BTreeMap<String, Located>,
}

// `impl VolumeStore { .. }` attaches methods to the struct. Methods taking `&self`
// only read the store; `&mut self` may mutate it; functions without a `self`
// parameter (like `open`) are "associated functions" called as `VolumeStore::open`.
impl VolumeStore {
    /// Open (or lazily initialise) the store under `dir`, decrypting every
    /// manifest. A manifest that fails to decrypt/parse is **rebuilt** by scanning
    /// its volume. No volume bytes are read for documents (lazy). Creates nothing
    /// on disk — directories are made on the first write.
    // Borrows its inputs (`&Path`, `&Key`, `&str`) — it only reads them. Returns
    // `Self` (a new `VolumeStore`) on success. `?` inside propagates any error out.
    pub fn open(dir: &Path, key: &Key, vault_id: &str, max_size: u64) -> Result<Self, StorageError> {
        let manifest_dir = dir.join("manifest"); // join = append a path segment, returns a new PathBuf
        let volume_dir = dir.join("volume");
        // `mut` makes `store` reassignable/mutable; we populate its manifests below.
        let mut store = VolumeStore {
            manifest_dir,
            volume_dir,
            vault_id: vault_id.to_string(), // copy the borrowed &str into an owned String
            max_size: max_size.max(1),      // clamp to >= 1 to avoid a zero cap
            manifests: Vec::new(),          // empty growable vec
            index: BTreeMap::new(),         // empty ordered map
        };

        // Load contiguous partitions 0,1,2,... stopping at the first absent one.
        let mut part: u32 = 0;
        loop {
            let mpath = store.manifest_path(part);
            let vpath = store.volume_path(part);
            if !mpath.exists() && !vpath.exists() {
                break; // no more partitions on disk
            }
            // `match` selects a branch by pattern. `load_manifest` returns a Result:
            //   - Ok(m): use the decrypted manifest `m`.
            //   - Err(_) if vpath.exists(): a *guard* — only taken when the volume is
            //     present; rebuild from it (the `?` re-raises a rebuild error).
            //   - Err(e): otherwise propagate the original error.
            let manifest = match store.load_manifest(part, key) {
                // A present, valid manifest whose volume file is MISSING (partial restore /
                // selective backup / FS damage) must fail closed: opening it would let the next
                // put() create a fresh volume at the manifest's non-zero end_offset, zero-filling
                // [0,end) and silently destroying every prior frame. Mirror the contiguity guard.
                Ok(_) if !vpath.exists() => {
                    return Err(StorageError::Corrupt(format!(
                        "partition {part}: manifest present but its volume file is missing ({})",
                        vpath.display()
                    )));
                }
                Ok(m) => m,
                // Genuine corruption (won't decrypt, won't parse, or truncated) with a
                // present volume → rebuild by scanning the self-describing volume.
                Err(StorageError::Corrupt(_) | StorageError::Crypto(_) | StorageError::Json(_))
                    if vpath.exists() =>
                {
                    store.rebuild_manifest(part, key)?
                }
                // A *missing* manifest (file absent) but present volume → also rebuild.
                // But a manifest that IS present and failed for a transient/operational
                // reason (an I/O glitch, or a momentary size-cap trip) is NOT corruption:
                // propagate it rather than discard a valid manifest — and its
                // authoritative `end_offset` — via a lossy volume scan that silently
                // drops every frame past the first unreadable one.
                Err(_) if vpath.exists() && !mpath.exists() => store.rebuild_manifest(part, key)?,
                Err(e) => return Err(e),
            };
            store.manifests.push(manifest); // append to the vec
            part += 1;
        }

        // Fail closed on a NON-CONTIGUOUS partition set. The loop above stops at the
        // first absent partition, so a lost MIDDLE partition (vol.1/manifest.1 gone via a
        // partial restore, selective backup, or filesystem corruption) while a higher
        // partition survives would otherwise be SILENTLY dropped — the vault would open
        // "successfully" minus every document in the orphaned higher partitions. Silent
        // data loss is the worst possible outcome for a vault, so detect a surviving
        // higher partition and refuse (corruption fails closed, like everywhere else here).
        let highest = highest_partition_index(&store.manifest_dir, "manifest.")
            .max(highest_partition_index(&store.volume_dir, "vol."));
        if let Some(hi) = highest
            && hi as usize >= store.manifests.len()
        {
            return Err(StorageError::Corrupt(format!(
                "non-contiguous partitions: loaded {} but partition {hi} still exists on disk \
                 (a middle partition is missing)",
                store.manifests.len()
            )));
        }

        store.reindex();
        Ok(store) // wrap the finished store as the success value
    }

    // `format!` builds a String; `{part}` interpolates the variable inline.
    fn manifest_path(&self, part: u32) -> PathBuf {
        self.manifest_dir.join(format!("manifest.{part}"))
    }
    fn volume_path(&self, part: u32) -> PathBuf {
        self.volume_dir.join(format!("vol.{part}"))
    }

    /// Rebuild the in-memory id → location index from the loaded manifests.
    // `&mut self`: this method mutates the store (rewrites `index`).
    fn reindex(&mut self) {
        self.index.clear();
        // `.iter().enumerate()` yields `(p, m)` pairs: `p` is the index (the
        // partition number), `m` is a shared reference to each Manifest.
        for (p, m) in self.manifests.iter().enumerate() {
            for (i, e) in m.entries.iter().enumerate() { // `i` = position within this partition
                self.index.insert(
                    e.id.clone(), // map keys are owned; clone the id String to store it
                    Located { partition: p as u32, offset: e.offset, length: e.length, entry_index: i as u32 },
                );
            }
        }
    }

    /// The document ids currently stored (live entries).
    // Return type `impl Iterator<Item = &str>` = "some iterator yielding string
    // slices"; the borrows live as long as `&self`. `.map(closure)` transforms each
    // item; `|s| s.as_str()` is a closure (anonymous fn) turning `&String` -> `&str`.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(|s| s.as_str())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// Metadata for a stored document (path/size), if present.
    // Returns `Option<&ManifestEntry>`: `Some(ref)` if found, else `None`. The `?`
    // on `index.get(id)` early-returns `None` when the id is absent. `.get(..)` on a
    // Vec is bounds-checked and also returns an Option. `.and_then(closure)` runs the
    // closure only on `Some`, flattening the nested Option. `.find(predicate)` returns
    // the first entry matching the closure `|e| e.id == id`.
    pub fn entry(&self, id: &str) -> Option<&ManifestEntry> {
        let loc = self.index.get(id)?;
        let m = self.manifests.get(loc.partition as usize)?;
        // O(1): the index records the entry's slot, refreshed by `reindex` after every
        // mutation. Verify the id still matches there; on a (theoretical) desync, fall back
        // to a linear scan so a stale index can never serve the wrong entry. This removes the
        // per-read linear scan that made bulk paths (export/compact/migrate/merge) O(N^2).
        match m.entries.get(loc.entry_index as usize) {
            Some(e) if e.id == id => Some(e),
            _ => m.entries.iter().find(|e| e.id == id),
        }
    }

    /// Iterate every stored document's metadata.
    // `.flat_map` maps each manifest to its entries-iterator, then concatenates them
    // into one flat stream of `&ManifestEntry`.
    pub fn entries(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.manifests.iter().flat_map(|m| m.entries.iter())
    }

    /// Iterate the metadata of documents in a single partition (empty if that
    /// partition does not exist).
    // `.get(..)` -> Option; `.into_iter()` turns it into an iterator of 0-or-1 items,
    // so a missing partition yields nothing (no panic).
    pub fn partition_entries(&self, part: u32) -> impl Iterator<Item = &ManifestEntry> {
        self.manifests.get(part as usize).into_iter().flat_map(|m| m.entries.iter())
    }

    pub fn partition_count(&self) -> usize {
        self.manifests.len()
    }

    /// `(committed, live)` on-disk volume bytes: `committed` is the sum of each
    /// partition's `end_offset` (the authoritative valid length), and `live` is
    /// the sum of the on-disk frame lengths still referenced by a manifest entry.
    /// `committed - live` is the **reclaimable garbage** — dead frames left by
    /// updates and deletes — that a `compact` rewrite would remove.
    pub fn space_stats(&self) -> (u64, u64) {
        let committed: u64 = self.manifests.iter().fold(0u64, |a, m| a.saturating_add(m.end_offset));
        // Sum live bytes from the UNIQUE index (one entry per id), not by summing all
        // manifest entries — a duplicate id across partitions (only reachable via a
        // crafted/corrupt authenticated manifest) would otherwise be double-counted,
        // making `committed - live` underflow-saturate to 0 and under-report garbage.
        let live: u64 = self.index.values().fold(0u64, |a, loc| a.saturating_add(loc.length));
        (committed, live)
    }

    /// Update the per-partition size cap for **future** placement decisions
    /// (existing partitions are untouched). Clamped to at least 1 byte.
    pub fn set_max_size(&mut self, max_size: u64) {
        self.max_size = max_size.max(1);
    }

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
    fn target_partition(&self, id: &str, doc_size: u64) -> u32 {
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

    fn aad(&self, part: u32) -> Vec<u8> {
        volume_aad(&self.vault_id, part)
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
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

    // --- Manifest I/O --------------------------------------------------------

    fn load_manifest(&self, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        let path = self.manifest_path(part);
        // Reject a symlinked manifest and CAP THE READ ITSELF — not just a pre-stat. A plain
        // `fs::metadata` cap + `fs::read` both FOLLOW a symlink and leave a stat-then-read
        // gap: a `manifest.N` swapped for a symlink to `/dev/zero` (whose stat size is 0, so
        // the cap passes) would otherwise be read UNBOUNDEDLY → OOM on every vault open.
        // `symlink_metadata` does not follow the link (cheap early reject + over-size reject
        // without a big read); the bounded O_NOFOLLOW read closes the residual open-time
        // TOCTOU, matching `vault::read_bounded` and `append_frame`.
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(StorageError::Corrupt(format!("manifest.{part} is a symlink")));
        }
        if meta.len() > MAX_MANIFEST_SIZE {
            return Err(StorageError::TooLarge);
        }
        let raw = read_file_bounded_nofollow(&path, MAX_MANIFEST_SIZE)?;
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

    /// Write `manifest.<part>` atomically: temp → fsync → rename → fsync dir.
    fn commit_manifest(&self, part: u32, manifest: &Manifest, key: &Key) -> Result<(), StorageError> {
        self.ensure_dirs()?;
        let plain = Zeroizing::new(serde_json::to_vec(manifest)?); // serialize to JSON bytes, wiped after
        // `random_bytes::<NONCE_LEN>()` is a generic call: `::<N>` picks the array
        // length at compile time, returning `[u8; NONCE_LEN]`.
        let nonce = crypto::random_bytes::<NONCE_LEN>()?;
        let ct = crypto::encrypt_with_nonce(key, &nonce, &plain, &manifest_aad(&self.vault_id, part))?;
        // `with_capacity` pre-allocates the exact size to avoid reallocations.
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce); // on-disk layout: nonce ‖ ciphertext
        blob.extend_from_slice(&ct);
        write_atomic(&self.manifest_path(part), &blob) // last expression = return value (no `;`)
    }

    /// Reconstruct a partition manifest by scanning its self-describing volume up
    /// to the last decryptable frame (recovery for a lost/corrupt manifest).
    fn rebuild_manifest(&self, part: u32, key: &Key) -> Result<Manifest, StorageError> {
        let mut f = File::open(self.volume_path(part))?;
        let file_len = f.metadata()?.len();
        let aad = volume_aad(&self.vault_id, part);
        reject_over_cap(scan_volume(&mut f, file_len, key, &aad), MAX_MANIFEST_ENTRIES)
    }
}

/// Apply the manifest entry cap to a REBUILT manifest, the same way `load_manifest`
/// applies it to a stored one.
///
/// [`scan_volume`] is deliberately infallible — it returns whatever intact prefix it found
/// — so the rebuild path was the one way an over-cap manifest could enter memory. From
/// there the next `put`/`remove` in that partition would COMMIT it, and every subsequent
/// open would then hit `load_manifest`'s `TooLarge`, which [`VolumeStore::open`] does NOT
/// treat as rebuildable: an otherwise-intact vault would be permanently unopenable.
/// Rejecting at the rebuild fails closed with the same error and destroys nothing.
///
/// (`target_partition` keeps a legitimately-written partition under the cap, so this is
/// unreachable for a vault this code produced — it exists so the read side cannot admit
/// something the write side would refuse.)
fn reject_over_cap(manifest: Manifest, max_entries: usize) -> Result<Manifest, StorageError> {
    if manifest.entries.len() > max_entries {
        return Err(StorageError::TooLarge);
    }
    Ok(manifest)
}

/// Scan a self-describing volume (any `Read + Seek`) from the start, decrypting
/// frame after frame, and rebuild its manifest up to the last good frame. Any
/// torn / garbage / foreign / undersized frame ends the scan, so this never
/// fails — it returns whatever prefix is intact. Used by both the on-disk
/// rebuild path and the fuzzer.
// Generic over `R` with the trait bound `R: Read + Seek` — i.e. `f` can be any type
// that can be read and seeked (a real `File`, or an in-memory `Cursor` in tests/fuzz).
// Returns a `Manifest` directly (never errors): it stops at the first bad frame.
fn scan_volume<R: Read + Seek>(f: &mut R, file_len: u64, key: &Key, aad: &[u8]) -> Manifest {
    let mut offset = 0u64;
    // Last write wins for a repeated id (updates append a newer frame).
    let mut latest: BTreeMap<String, ManifestEntry> = BTreeMap::new(); // id -> newest entry seen
    let mut order: Vec<String> = Vec::new(); // preserve first-seen order of ids
    while offset.checked_add(FRAME_PREFIX_LEN).is_some_and(|end| end <= file_len) {
        match read_frame_at(f, file_len, offset, 0, key, aad) {
            Ok((id, path, bytes)) => {
                // read_frame_at(length=0) parsed the prefix to learn the size;
                // recover the on-disk frame length to advance.
                // `let Ok(..) = .. else { break }`: on failure, stop scanning.
                let Ok(frame_len) = frame_total_len(f, offset) else { break };
                let entry = ManifestEntry {
                    id: id.clone(),
                    path,
                    size: bytes.len() as u64,
                    offset,
                    length: frame_len,
                    uploaded_at: 0,
                };
                // `insert` returns the previous value for this key (or None). If it
                // was None this is the first sighting, so record its order.
                if latest.insert(id.clone(), entry).is_none() {
                    order.push(id);
                }
                offset += frame_len; // advance to the next frame
            }
            // Torn/garbage/foreign frame → end of valid data.
            Err(_) => break, // `_` matches any error without naming it
        }
    }
    // Reassemble entries in first-seen order: `into_iter()` consumes `order` (moving
    // each id out), `filter_map` keeps only ids still in the map (and removes them),
    // `collect()` gathers the results into a Vec.
    let entries: Vec<ManifestEntry> = order.into_iter().filter_map(|id| latest.remove(&id)).collect();
    Manifest { seq: 1, end_offset: offset, entries }
}

// --- Frame & AAD helpers -----------------------------------------------------

// Builds the AEAD "associated data": prefix ‖ vault_id ‖ partition. AAD is
// authenticated-but-not-encrypted context, so a ciphertext only verifies under the
// exact same vault_id+partition it was written with (binding it in place).
fn manifest_aad(vault_id: &str, part: u32) -> Vec<u8> {
    let mut a = MANIFEST_AAD_PREFIX.to_vec(); // copy the static prefix into an owned Vec
    a.extend_from_slice(vault_id.as_bytes()); // `.as_bytes()` views the &str as &[u8]
    a.extend_from_slice(&part.to_le_bytes()); // u32 -> 4 little-endian bytes
    a // return the built vec
}

fn volume_aad(vault_id: &str, part: u32) -> Vec<u8> {
    let mut a = VOLUME_AAD_PREFIX.to_vec();
    a.extend_from_slice(vault_id.as_bytes());
    a.extend_from_slice(&part.to_le_bytes());
    a
}

/// Build a complete on-disk frame: `[u32 frame_len][nonce][ciphertext]`.
fn encode_frame(key: &Key, vault_id: &str, part: u32, id: &str, path: &str, bytes: &[u8]) -> Result<Vec<u8>, StorageError> {
    // Assemble the length-prefixed plaintext: id_len ‖ id ‖ path_len ‖ path ‖ body.
    let mut plain = Vec::with_capacity(8 + id.len() + path.len() + bytes.len());
    plain.extend_from_slice(&(id.len() as u32).to_le_bytes());
    plain.extend_from_slice(id.as_bytes());
    plain.extend_from_slice(&(path.len() as u32).to_le_bytes());
    plain.extend_from_slice(path.as_bytes());
    plain.extend_from_slice(bytes);
    // Shadowing: re-bind `plain` to a Zeroizing wrapper around the same bytes so the
    // plaintext is wiped on drop. The old binding is moved in and inaccessible after.
    let plain = Zeroizing::new(plain);

    let nonce = crypto::random_bytes::<NONCE_LEN>()?; // fresh per-frame nonce
    let ct = crypto::encrypt_with_nonce(key, &nonce, &plain, &volume_aad(vault_id, part))?;
    let frame_len = (NONCE_LEN + ct.len()) as u32;
    let mut frame = Vec::with_capacity(FRAME_PREFIX_LEN as usize + frame_len as usize);
    frame.extend_from_slice(&frame_len.to_le_bytes());
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&ct);
    Ok(frame)
}

/// Read the `[u32 frame_len]` at `offset` and return the total frame length
/// (`4 + frame_len`), bounds-checked against the file.
fn frame_total_len<R: Read + Seek>(f: &mut R, offset: u64) -> Result<u64, StorageError> {
    f.seek(SeekFrom::Start(offset))?; // move the read cursor to `offset`
    let mut lb = [0u8; 4]; // a fixed 4-byte stack buffer for the length prefix
    f.read_exact(&mut lb)?; // fill it exactly (errors if fewer than 4 bytes remain)
    Ok(FRAME_PREFIX_LEN + u32::from_le_bytes(lb) as u64) // 4-byte prefix + the frame body
}

/// Read and decrypt the frame at `offset` within a reader of length `file_len`.
/// If `expected_len` is non-zero it is a sanity check against the manifest.
/// Returns `(id, path, doc_bytes)`. Every read is bounds-checked so a corrupt
/// length can't over-read or over-allocate.
fn read_frame_at<R: Read + Seek>(
    f: &mut R,
    file_len: u64,
    offset: u64,
    expected_len: u64,
    key: &Key,
    aad: &[u8],
) -> Result<(String, String, Zeroizing<Vec<u8>>), StorageError> {
    // `.into()` converts the string literal into the `String` the Corrupt variant
    // holds. Each check below guards against a corrupt length over-reading/-allocating.
    // `checked_add` so a corrupt/forged near-u64::MAX offset yields a clean Corrupt
    // error instead of wrapping (release) or panicking (debug-overflow). Authentic
    // offsets come from an AEAD-authenticated manifest or a bounded volume scan, so
    // this is defense-in-depth, not a reachable path with a valid vault.
    if offset.checked_add(FRAME_PREFIX_LEN).is_none_or(|end| end > file_len) {
        return Err(StorageError::Corrupt("frame offset past EOF".into()));
    }
    f.seek(SeekFrom::Start(offset))?;
    let mut lb = [0u8; 4];
    f.read_exact(&mut lb)?;
    let frame_len = u32::from_le_bytes(lb) as u64;
    // `||` is logical OR: reject a length that's too small to even hold a nonce, or
    // implausibly large. This bound runs before any allocation.
    if frame_len < NONCE_LEN as u64 || frame_len > MAX_DOC_SIZE + 4096 {
        return Err(StorageError::Corrupt("implausible frame length".into()));
    }
    if offset
        .checked_add(FRAME_PREFIX_LEN)
        .and_then(|x| x.checked_add(frame_len))
        .is_none_or(|end| end > file_len)
    {
        return Err(StorageError::Corrupt("frame overruns EOF".into()));
    }
    // `&&` is logical AND: only check the manifest agreement when a non-zero
    // `expected_len` was supplied (scan_volume passes 0 to skip this).
    if expected_len != 0 && expected_len != FRAME_PREFIX_LEN + frame_len {
        return Err(StorageError::Corrupt("frame length disagrees with manifest".into()));
    }
    let mut buf = vec![0u8; frame_len as usize]; // `vec![v; n]` = a Vec of `n` copies of `v`
    f.read_exact(&mut buf)?;
    let (nonce, ct) = buf.split_at(NONCE_LEN); // split the frame body into nonce ‖ ciphertext
    let plain = Zeroizing::new(crypto::decrypt(key, nonce, ct, aad)?); // AEAD-verify + decrypt
    parse_plaintext(&plain) // returns (id, path, body); its result becomes ours
}

/// Parse `[u32 id_len][id][u32 path_len][path][bytes]` with bounds checks.
fn parse_plaintext(plain: &[u8]) -> Result<(String, String, Zeroizing<Vec<u8>>), StorageError> {
    let mut cur = 0usize; // running offset into `plain`
    // A closure (anonymous helper) that reads the next `n` bytes and advances `cur`.
    // It borrows `cur` mutably (`&mut usize`) so it can update the caller's offset.
    let take = |cur: &mut usize, n: usize| -> Result<&[u8], StorageError> {
        // `checked_add` returns None on integer overflow (instead of wrapping), so a
        // hostile huge length can't wrap past the buffer; `?` turns None-handling into
        // an early error here.
        let end = cur.checked_add(n).ok_or_else(|| StorageError::Corrupt("length overflow".into()))?;
        // `plain.get(range)` is bounds-checked, returning None if the range exceeds the
        // slice — so a lying length yields an error, never an out-of-bounds read.
        let s = plain.get(*cur..end).ok_or_else(|| StorageError::Corrupt("short frame".into()))?;
        *cur = end; // `*cur` writes through the mutable borrow
        Ok(s)
    };
    // `.try_into().unwrap()` converts the 4-byte slice to a `[u8; 4]` array; it cannot
    // fail here because `take(.., 4)` returned exactly 4 bytes, so the unwrap is safe.
    let id_len = u32::from_le_bytes(take(&mut cur, 4)?.try_into().unwrap()) as usize;
    // `String::from_utf8` validates UTF-8; `.map_err(..)` rewrites its error into our
    // Corrupt variant; `?` propagates it. `.to_vec()` copies the borrowed bytes into
    // an owned Vec the String can take ownership of.
    let id = String::from_utf8(take(&mut cur, id_len)?.to_vec()).map_err(|_| StorageError::Corrupt("bad id utf8".into()))?;
    let path_len = u32::from_le_bytes(take(&mut cur, 4)?.try_into().unwrap()) as usize;
    let path = String::from_utf8(take(&mut cur, path_len)?.to_vec()).map_err(|_| StorageError::Corrupt("bad path utf8".into()))?;
    let bytes = Zeroizing::new(plain[cur..].to_vec()); // everything after the headers is the body
    Ok((id, path, bytes))
}

// --- Crash-safe filesystem helpers ------------------------------------------

/// Append `frame` to the volume at `start`, truncating any torn tail beyond it,
/// then fsync. Opens read/write (create if absent).
fn append_frame(path: &Path, start: u64, frame: &[u8]) -> Result<(), StorageError> {
    // Refuse to write through a symlink planted at the volume path: an attacker
    // with write access to the vault dir could otherwise redirect our writes (and
    // the 0600 chmod) to an arbitrary file the user can write. The atomic
    // manifest/vault writes use O_EXCL + rename; this append path opens the file
    // directly, so it needs its own guard.
    // This stat is a fast, friendly EARLY rejection — but it is NOT the security
    // boundary, because the file could be swapped for a symlink between this check
    // and the open below (a TOCTOU race). The atomic guarantee comes from opening
    // with O_NOFOLLOW (see below), which makes the kernel refuse a final-component
    // symlink at open time. `symlink_metadata` does not follow the link.
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to write through a symlink at the volume path",
        )));
    }
    let mut opts = OpenOptions::new(); // builder for how to open the file
    opts.read(true).write(true).create(true); // chained builder calls
    // `#[cfg(unix)]` compiles this block ONLY on unix targets (conditional
    // compilation). It sets 0600 perms and, crucially, adds `O_NOFOLLOW` so the
    // open itself fails atomically (ELOOP) if the path's final component is a
    // symlink — closing the TOCTOU window the stat above cannot. `custom_flags` is
    // a safe API (no `unsafe`); the flag only affects the final component, matching
    // the stat's scope. Legitimate `vol.<N>` files are regular files, so this never
    // rejects a valid append.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600); // owner read/write only
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    // Belt-and-suspenders chmod to 0600 (no-op on non-unix) via the OPEN descriptor
    // (fchmod), NOT by path. A by-path chmod here would re-resolve `path` and follow a
    // symlink, reintroducing the chmod-through-symlink TOCTOU that the O_NOFOLLOW open
    // above just closed — on a non-first append `vol.<N>` already exists, so a same-UID
    // attacker could swap in a symlink between the open and a by-path chmod. fchmod acts
    // on the file we actually opened, with no path resolution.
    harden_file_fd(&f);
    // Defense-in-depth: never write PAST the current end of the volume. A legitimate append has
    // `start` == the file's current length; `start > len` would seek past EOF and leave a sparse
    // zero hole over [len, start), corrupting every prior frame — e.g. if a fresh `vol.<N>` were
    // created here at a non-zero `start` because the file had gone missing. Fail closed.
    let cur_len = f.metadata()?.len();
    if start > cur_len {
        return Err(StorageError::Corrupt(format!(
            "refusing to append at offset {start} past the end of {} (len {cur_len})",
            path.display()
        )));
    }
    f.seek(SeekFrom::Start(start))?;
    crate::fault::point("volume.write")?; // inject ENOSPC before the volume append
    f.write_all(frame)?; // `frame: &[u8]` is borrowed, not consumed
    // Drop any pre-existing garbage tail beyond the new committed end. Use checked_add so the
    // end offset can never silently wrap (and never panic under overflow-checks); restores the
    // saturating/checked discipline used for `end_offset` on the put path.
    let new_len = start
        .checked_add(frame.len() as u64)
        .ok_or_else(|| StorageError::Corrupt(format!("frame end offset overflow at {start} in {}", path.display())))?;
    f.set_len(new_len)?;
    f.sync_all()?; // fsync: force the bytes (and metadata) to durable storage
    // Make the (possibly newly created) vol.<N> directory entry durable BEFORE its
    // referencing manifest is committed, so a crash can never leave a committed
    // manifest pointing at a volume the filesystem never durably linked.
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Read at most `max + 1` bytes from `path` WITHOUT following a final-component symlink,
/// erroring `TooLarge` if the file holds more than `max`. Unlike `fs::read` (which both
/// follows symlinks and allocates without bound), this caps the allocation on the READ and
/// refuses a symlinked file at the open, so a file swapped for a symlink to `/dev/zero`
/// (stat size 0) can neither be followed nor drive an OOM. Mirrors `vault::read_bounded`
/// and `append_frame`'s O_NOFOLLOW discipline. (On non-unix the caller's `symlink_metadata`
/// pre-check remains the guard.)
fn read_file_bounded_nofollow(path: &Path, max: u64) -> Result<Vec<u8>, StorageError> {
    use std::io::Read;
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
        return Err(StorageError::TooLarge);
    }
    Ok(buf)
}

/// Highest partition index `N` for which a file named exactly `<prefix>N` (e.g.
/// `manifest.3`, `vol.3`) exists in `dir`; `None` if the dir is absent or holds no
/// such file. The strict `<prefix><decimal>` match ignores the hidden in-flight temp
/// files (`.manifest.N.<suffix>.tmp`) the atomic writer leaves, so a mid-write open is
/// not mistaken for a real partition. Used by `open` to detect a missing middle partition.
fn highest_partition_index(dir: &Path, prefix: &str) -> Option<u32> {
    let mut hi: Option<u32> = None;
    let Ok(rd) = fs::read_dir(dir) else { return None };
    for entry in rd.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
            // Canonical decimal only — reject leading zeros (vol.007) so a foreign/mis-named
            // sibling can't be counted as a partition and spuriously trip the contiguity guard.
            && (rest == "0" || !rest.starts_with('0'))
            && rest.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = rest.parse::<u32>()
        {
            hi = Some(hi.map_or(n, |h| h.max(n)));
        }
    }
    hi
}

/// Atomic write: unique hidden temp in the same dir → fsync → rename → fsync dir.
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    // `.filter(closure)` keeps the parent only if the closure is true (here: non-empty),
    // otherwise yields None.
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    // `.and_then` chains another Option-returning step (bytes -> valid UTF-8 name);
    // `.unwrap_or("f")` supplies a fallback name if either step yields None.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("f");
    // Build a random hex suffix: map each random byte to a 2-char hex string, then
    // `.collect()` concatenates them into one String (target type from the annotation).
    let suffix: String = crypto::random_bytes::<8>()?.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = match dir {
        Some(d) => d.join(format!(".{name}.{suffix}.tmp")), // hidden temp beside the target
        None => PathBuf::from(format!(".{name}.{suffix}.tmp")),
    };
    {
        // Inner scope so the file `f` is dropped (closed) at the closing brace, before
        // the rename below.
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true); // create_new fails if the temp already exists (O_EXCL)
        #[cfg(unix)] // unix-only permission setting
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        // Write then fsync, chained: `and_then(|()| ..)` runs the sync only if the write
        // succeeded. On any error (incl. an injected ENOSPC), close the file and remove
        // the temp before returning — the live target is never touched.
        if let Err(e) = crate::fault::point("atomic.write")
            .and_then(|()| f.write_all(data))
            .and_then(|()| f.sync_all())
        {
            drop(f); // close explicitly before deleting
            let _ = fs::remove_file(&tmp); // `let _ =` deliberately ignores the result
            return Err(e.into()); // `.into()` converts io::Error -> StorageError
        }
    }
    // Atomic step: rename temp over the target. On a crash, either the old or new file
    // is fully present — never a half-written one. The fault point models a failure
    // (e.g. ENOSPC) at the rename; on any error the temp is removed and the live
    // target is left untouched.
    if let Err(e) =
        crate::fault::point("atomic.rename").map_err(StorageError::from).and_then(|()| Ok(fs::rename(&tmp, path)?))
    {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(d) = dir {
        sync_dir(d); // fsync the directory so the rename itself is durable
    }
    Ok(())
}

// These functions are defined twice: the `#[cfg(unix)]` version is compiled on unix;
// the `#[cfg(not(unix))]` version (a no-op) is compiled everywhere else. Only one of
// each pair exists in any given build, so callers don't need to branch.

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    // Open the directory and fsync it (commits the directory entry itself). Errors are
    // ignored (`if let Ok` / `let _`) — best-effort durability.
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {} // no-op; `_dir` underscore-prefix marks it intentionally unused

/// Tighten an ALREADY-OPEN file to 0600 via its descriptor (fchmod), with no path
/// re-resolution — so it cannot be tricked into chmod-ing a symlink target. Best-effort.
#[cfg(unix)]
fn harden_file_fd(f: &File) {
    use std::os::unix::fs::PermissionsExt; // brings `from_mode` into scope
    let _ = f.set_permissions(fs::Permissions::from_mode(0o600)); // owner read/write only
}
#[cfg(not(unix))]
fn harden_file_fd(_f: &File) {} // no-op off unix (permission bits don't apply the same way)

#[cfg(unix)]
fn harden_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(dir) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700); // owner-only directory access
        let _ = fs::set_permissions(dir, perms);
    }
}
#[cfg(not(unix))]
fn harden_dir(_dir: &Path) {} // no-op off unix

/// Fuzz entry points: feed arbitrary bytes into the untrusted-input parsers.
/// The invariant is strict — these must only ever return (`Ok`/`Err` internally),
/// never panic, hang, or over-allocate, no matter the input.
// `pub mod fuzz` is a nested public sub-module.
pub mod fuzz {
    use super::*; // re-import everything from the parent module (this file)
    use std::io::Cursor; // an in-memory `Read + Seek` over a byte buffer
    use std::sync::OnceLock; // a thread-safe cell initialised at most once

    /// A cheap, process-wide key so fuzzing the scanner doesn't pay an Argon2
    /// derivation per input (decryption fails on arbitrary bytes regardless; the
    /// key value is irrelevant to the parse/bounds logic under test).
    // Returns `&'static Key`: a reference valid for the whole program lifetime
    // (`'static`), since the key lives in a process-wide cell.
    fn fuzz_key() -> &'static Key {
        // `static` is a single global; `OnceLock` lets us lazily build the key on first
        // use and reuse it thereafter.
        static KEY: OnceLock<Key> = OnceLock::new();
        // `get_or_init(closure)` runs the closure once to populate the cell, then always
        // returns the stored reference. `.expect(..)` panics with this message if the
        // derivation fails (acceptable in a fuzz harness).
        KEY.get_or_init(|| {
            crypto::derive_key(b"fuzz", b"sixteen-byte-slt", &crypto::KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 })
                .expect("fuzz key derivation")
        })
    }

    /// The decrypted-manifest JSON parser (post-decrypt path).
    pub fn manifest(buf: &[u8]) {
        let _ = serde_json::from_slice::<Manifest>(buf);
    }

    /// The hand-rolled decrypted-frame plaintext parser
    /// (`[u32 id_len][id][u32 path_len][path][bytes]`) — the highest-risk
    /// length-prefixed surface for an out-of-bounds read or over-allocation.
    pub fn frame(buf: &[u8]) {
        let _ = parse_plaintext(buf);
    }

    /// The volume scan/rebuild path over arbitrary bytes: exercises the frame
    /// length prefix, the bounds checks, and the seek/advance loop.
    pub fn scan_volume(buf: &[u8]) {
        let aad = volume_aad("fuzz", 0);
        let mut cur = Cursor::new(buf); // wrap the bytes so they look like a seekable file
        // `super::scan_volume` disambiguates the parent's function from this module's
        // same-named wrapper. `let _ =` discards the returned Manifest (we only care
        // that it doesn't panic/over-allocate).
        let _ = super::scan_volume(&mut cur, buf.len() as u64, fuzz_key(), &aad);
    }
}

// `#[cfg(test)]` compiles this whole module ONLY under `cargo test`, so the tests add
// no code to the shipped binary. Each `#[test]` fn is a test case; `assert!` /
// `assert_eq!` panic (fail the test) when their condition is false. `.unwrap()` here
// is fine because a panic in a test is just a test failure.
#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
