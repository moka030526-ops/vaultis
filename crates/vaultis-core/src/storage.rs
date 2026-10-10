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

// The store is split by concern. Each submodule may add its own `impl VolumeStore` block
// (a type's methods may be spread over several modules of the crate), and the free
// helpers are glob-imported here so the whole store — and its tests and fuzz entry
// points — sees one flat namespace, exactly as when this was a single file.
mod blobs; // document reads + mutations
mod frame; // on-disk frame format + AAD
mod fsutil; // crash-safe filesystem helpers
mod manifest; // manifest load / commit / recovery
mod scan; // frame-by-frame volume scan + resync

use frame::*;
use fsutil::*;
use scan::*;

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
    /// The vault's in-place redundancy depth (§12.8), mirrored here so a manifest
    /// commit knows whether to keep a spare copy beside it. `0` = off, the default.
    /// Set by the vault after opening (see [`VolumeStore::set_redundancy`]) — the
    /// value itself lives in `vault.settings.redundancy`, which this layer cannot see.
    ///
    /// Only the WRITE side consults it: recovery always tries an existing spare,
    /// whatever the setting says now, because a spare on disk can only help.
    redundancy: u32,
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
            redundancy: 0,                  // off until the vault tells us otherwise
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
                    store.recover_manifest(part, key)?
                }
                // A *missing* manifest (file absent) but present volume → also rebuild.
                // But a manifest that IS present and failed for a transient/operational
                // reason (an I/O glitch, or a momentary size-cap trip) is NOT corruption:
                // propagate it rather than discard a valid manifest — and its
                // authoritative `end_offset` — via a lossy volume scan that silently
                // drops every frame past the first unreadable one.
                Err(_) if vpath.exists() && !mpath.exists() => store.recover_manifest(part, key)?,
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

    /// The spare copy of partition `part`'s manifest, kept beside it when redundancy
    /// is on (§12.8). Same contents as the live file, encrypted independently (its own
    /// random nonce), so one damaged sector cannot land on the same bytes in both.
    fn manifest_mirror_path(&self, part: u32) -> PathBuf {
        self.manifest_dir.join(format!("manifest.{part}.mirror"))
    }

    /// Set the in-place redundancy depth this store writes at (`0` = off).
    ///
    /// Records the depth and NOTHING else. It used to delete the spares when handed a
    /// depth of 0, which made it a write — and `open_inner` calls it on every open,
    /// including a READ-ONLY one, so opening a vault read-only could delete files in the
    /// vault folder (audit 2026-08-03 A-1). Removing spares is now an explicit, separate
    /// call ([`Self::drop_manifest_mirrors`]) that only write paths make, so the
    /// read-only guarantee does not depend on remembering to check a flag here.
    pub fn set_redundancy(&mut self, depth: u32) {
        self.redundancy = depth;
    }

    /// Remove every partition's spare manifest (redundancy turned off, or the copies
    /// are about to be rewritten under a new key). Best-effort by design: a spare that
    /// cannot be deleted is stale, not dangerous — recovery AEAD-verifies what it uses.
    pub fn drop_manifest_mirrors(&self) {
        for part in 0..self.manifests.len() as u32 {
            let _ = fs::remove_file(self.manifest_mirror_path(part));
        }
    }

    /// Rewrite every partition's spare manifest from the loaded manifests. Used after a
    /// rekey/compaction commit so the configured protection is back immediately rather
    /// than at the next document write, mirroring `OpenVault::refresh_redundancy_copies`.
    pub fn refresh_manifest_mirrors(&self, key: &Key) {
        if self.redundancy == 0 {
            return;
        }
        for (part, manifest) in self.manifests.iter().enumerate() {
            self.write_manifest_mirror(part as u32, manifest, key);
        }
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
}

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
