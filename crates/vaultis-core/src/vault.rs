//! The encrypted vault file and the orchestration over the partitioned document
//! store ([`crate::storage`]).
//!
//! The user supplies a **directory** `mypath`; inside it:
//! ```text
//!   mypath/vault.pmv          encrypted JSON vault (header + AEAD ciphertext)
//!   mypath/manifest/manifest.<N>   encrypted per-partition document index
//!   mypath/volume/vol.<N>          append-only, per-blob-encrypted documents
//! ```
//! `OpenVault` is given the vault *file* path (`mypath/vault.pmv`) and derives the
//! directory as its parent; the [`VolumeStore`] lives under that directory.
//!
//! Vault file layout (all integers little-endian):
//! ```text
//!   0   8   magic  b"PMVAULT\0"
//!   8   1   format version (currently 4)
//!   9   4   Argon2 m_cost (KiB)
//!   13  4   Argon2 t_cost
//!   17  4   Argon2 p_cost
//!   21  16  salt1
//!   37  24  nonce (XChaCha20-Poly1305)
//!   61  ..  ciphertext of the JSON vault
//! ```
//! The **entire 61-byte header** (incl. the nonce) is the AEAD associated data, so
//! tampering with the version/params/salt/nonce fails the Poly1305 tag on decrypt.
//!
//! Crash-safety: the document store commits per-operation (see [`crate::storage`]);
//! the vault file is the **final** commit point. A password change re-encrypts the
//! whole tree under a fresh key via a staged-and-rolled-forward protocol so a crash
//! mid-rotation always leaves either the old or the new tree fully working.

// `use` brings names into scope (like `import` elsewhere). `std::fs::{self, ..}`
// imports the `fs` module itself AND the listed items from it.
use std::fs::{self, OpenOptions};
use std::io::Write; // a *trait* (interface); brought in so `.write_all()` is callable
use std::path::{Path, PathBuf}; // `Path` = borrowed path (like `&str`); `PathBuf` = owned (like `String`)

use thiserror::Error; // a derive macro that auto-generates the std `Error` impl for our enum
use zeroize::Zeroizing; // wrapper that overwrites (zeroes) its contents on drop — for secrets

// `crate::` = this crate's own modules. `{self, ..}` again pulls in the module
// name plus the listed types/constants from it.
use crate::crypto::{self, CryptoError, KdfParams, Key, NONCE_LEN, SALT_LEN};
use crate::records::{self, Change, Vault};
use crate::storage::{self, MAX_DOC_SIZE, ManifestEntry, StorageError, VolumeStore};
use crate::types::TypeLists;

// THROWAWAY: one-shot owner-first / ts-in-filename document-path migration + history
// deletion + compaction. Delete this line and `src/vault/migrate.rs` to remove it.
pub mod migrate;

/// Take the single-writer lock for a read-only operation on `dir`, tolerating a source
/// we are not allowed to write to.
///
/// `backup` locks the SOURCE so a multi-file copy cannot straddle a concurrent rekey
/// (which would pair an old-key vault.pmv with a new-key store). But acquiring the lock
/// CREATES `vaultis.lock` in that directory, which is impossible on read-only media, a
/// restored snapshot, or a `chmod 500` directory — and those are exactly the cases where
/// a backup matters most. Refusing there meant you could not back up a vault you could
/// only read, and the error was a bare "Permission denied" naming no cause.
///
/// So: if the lock cannot be created *because we lack write access*, proceed without it.
/// That is sound rather than merely convenient — a directory we cannot create a file in
/// is one no other process can be writing the vault in either, so there is no concurrent
/// rekey for the lock to protect against. Every other failure (including `Locked`, i.e. a
/// real concurrent holder) is still propagated.
///
/// That argument holds only while the lock file DOES NOT EXIST, which is why this checks.
/// `WriteLock::acquire` opens `vaultis.lock` read+write+create, and `PermissionDenied`
/// from that open has two causes that are indistinguishable by error kind alone: the
/// directory is unwritable (above — nothing can be holding a lock that cannot exist), or
/// the lock file is already there and WE cannot open it, e.g. it belongs to another user
/// on a shared vault directory or was left by a `sudo` session. In that second case a
/// concurrent writer may well be holding it and rekeying, and proceeding unlocked is
/// precisely the torn snapshot the lock exists to prevent — an old-key `vault.pmv` paired
/// with a new-key `volume/`+`manifest/`, i.e. a backup that will not open. Every case the
/// tolerance was added for has no lock file: read-only media and a `chmod 500` directory
/// cannot have one created, and a restored snapshot never carries one (`backup_snapshot`
/// copies only `vault.pmv`, `manifest/` and `volume/`).
fn lock_for_read_only_copy(dir: &Path) -> Result<Option<WriteLock>, VaultError> {
    match WriteLock::acquire(dir) {
        Ok(l) => Ok(Some(l)),
        // Gated with `LOCK_FILE` itself: without the `single-writer-lock` feature there is no
        // lock file (and `acquire` is an infallible no-op), so this arm is both unreachable and
        // un-compilable. Leaving it ungated broke every build that turns the feature off — i.e.
        // the mobile-only `cargo build -p vaultis-ffi`, which is not covered by the workspace
        // build because feature unification switches the feature back on there.
        #[cfg(feature = "single-writer-lock")]
        Err(VaultError::Io(e))
            if matches!(e.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem)
                // `symlink_metadata` so a link planted at the lock path counts as PRESENT
                // rather than being resolved (a symlink there is refused as `Locked` by
                // `acquire` anyway, so it does not reach here).
                && fs::symlink_metadata(dir.join(LOCK_FILE)).is_err() =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// A decrypted document returned to the CLI: its manifest metadata plus its
/// plaintext bytes (which wipe on drop).
// `type` is an alias (a nickname for a longer type). A tuple `(A, B)` pairs two
// values. `Vec<u8>` is a growable byte array; wrapping it in `Zeroizing` means the
// plaintext bytes are scrubbed from memory when this value goes out of scope.
pub type DecryptedDoc = (ManifestEntry, Zeroizing<Vec<u8>>);

// `const` = compile-time constant. `&[u8; 8]` is a shared reference (`&`, a
// read-only borrow) to a fixed-size array of 8 bytes. `b"..."` is a byte-string
// literal; `\0` is a NUL byte. `u8` = unsigned 8-bit int; `usize` = pointer-sized
// unsigned int (used for lengths/indices).
const MAGIC: &[u8; 8] = b"PMVAULT\0";
const FORMAT_VERSION: u8 = 4;
const HEADER_LEN: usize = 61;
/// Hard ceiling on the vault file read into memory before any auth/decrypt — a
/// DoS guard against a crafted, oversized `vault.pmv` (the record JSON is small;
/// 256 MiB is far above any legitimate vault).
const MAX_VAULT_SIZE: u64 = 256 * 1024 * 1024;
/// Fixed vault-file name inside the user's directory.
const VAULT_FILE: &str = "vault.pmv";
/// Sidecar file in an export-tree mirror's `manifest/` dir recording the authoritative
/// partition count, so `import_tree` can fail closed against a TAIL-truncated mirror
/// (audit R4-3). Distinct from the `manifest.<N>.json` names, so it never trips the
/// contiguity scan. A mirror without it (legacy/hand-built) keeps the middle-gap-only guard.
const MIRROR_PARTITIONS_FILE: &str = "partitions";
/// Staging directory used during a password-change re-encryption.
const REKEY_DIR: &str = ".rekey";
const REKEY_READY: &str = "READY";
/// Single-writer advisory lock file inside the vault directory.
#[cfg(feature = "single-writer-lock")]
const LOCK_FILE: &str = "vaultis.lock";
/// Upper bound on the opt-in in-place redundancy depth (§12.8): the number of prior
/// `vault.pmv` generations retained. Each generation is a small encrypted copy, so a
/// few is plenty; this caps disk use and lingering old-secret copies.
const MAX_REDUNDANCY: u32 = 10;
/// Sane bounds for `volume_max_size` adopted from an UNTRUSTED import mirror
/// (`import_tree`): a floor so a tiny value can't fragment the store into a huge
/// number of partitions, and a generous ceiling that still rejects absurd values.
const MIN_VOLUME_MAX_SIZE: u64 = 64 * 1024; // 64 KiB
const MAX_VOLUME_MAX_SIZE: u64 = 64 * 1024 * 1024 * 1024; // 64 GiB

// Sanity bounds for KDF parameters now live on `KdfParams` (crypto.rs) as
// `KdfParams::validate()`, so the read path (Header::parse, a pre-derivation DoS
// guard) and the write paths (create/import_tree) share one definition and can
// never disagree (which would let a vault be written that can never be reopened).

// An `enum` is a tagged union: a value is exactly ONE of the listed variants,
// some of which carry data (e.g. `NotFound(PathBuf)`). This is the single error
// type every fallible function here returns.
// `#[derive(...)]` auto-generates trait impls: `Error` (from thiserror, using the
// `#[error("...")]` strings as the human-readable message) and `Debug` (a
// developer-facing dump). `{0}` in those strings interpolates the variant's data.
#[derive(Error, Debug)]
pub enum VaultError {
    #[error("vault not found at {0}")]
    NotFound(PathBuf),
    #[error("a vault already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("not a vaultis vault (bad magic bytes)")]
    BadMagic,
    #[error("unsupported vault format version {0} (this build expects v{FORMAT_VERSION}; recreate the vault)")]
    BadVersion(u8),
    #[error("vault file is truncated or corrupt")]
    Truncated,
    #[error("vault KDF parameters are out of the allowed range")]
    BadParams,
    #[error("document or archive exceeds the maximum allowed size")]
    TooLarge,
    #[error("a document referenced by the vault is missing from the document store (possible tampering or rollback)")]
    ArchiveMismatch,
    #[error("cannot remove a document that a record still references (unlink it from the record first)")]
    StillReferenced,
    #[error("an interrupted password change is pending; reopen with --write to finish recovery")]
    RekeyPending,
    #[error("vault is open read-only (relaunch with --write to make changes)")]
    ReadOnly,
    #[error("another writable session already has this vault open (close it, or open read-only)")]
    Locked,
    #[error("no such partition: {0}")]
    NoSuchPartition(u32),
    // `#[from]` generates a conversion so a `StorageError` (etc.) automatically
    // becomes a `VaultError` — this is what lets the `?` operator (used below)
    // bubble up errors of other types without manual wrapping. `transparent`
    // means this variant just forwards the inner error's message unchanged.
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("vault contents are not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Self-describing header parsed from / written to the vault file.
// A `struct` groups named fields (like a record/object). `#[derive(Clone)]` lets
// callers make an independent copy with `.clone()`; `Debug` enables `{:?}` dumps.
// `[u8; SALT_LEN]` is a fixed-length byte array whose length is the constant.
#[derive(Debug, Clone)]
struct Header {
    params: KdfParams,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
}

// `impl Header { ... }` attaches methods to the `Header` type (like defining the
// methods of a class). Methods taking `&self` borrow the value read-only.
impl Header {
    // Serialize this header to its fixed 61-byte on-disk form. `&self` = read-only
    // borrow of the header; the return type is an owned 61-byte array.
    fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN]; // `mut` = mutable; an array of 61 zero bytes
        // `b[0..8]` is a slice (a view) of bytes 0..7; `copy_from_slice` fills it.
        // `&self.params...` takes a borrow of each field. `to_le_bytes()` encodes an
        // integer as little-endian bytes (matching the on-disk format).
        b[0..8].copy_from_slice(MAGIC);
        b[8] = FORMAT_VERSION;
        b[9..13].copy_from_slice(&self.params.m_cost.to_le_bytes());
        b[13..17].copy_from_slice(&self.params.t_cost.to_le_bytes());
        b[17..21].copy_from_slice(&self.params.p_cost.to_le_bytes());
        b[21..37].copy_from_slice(&self.salt);
        b[37..61].copy_from_slice(&self.nonce);
        b // last expression with no `;` is the return value (no `return` needed)
    }

    // Parse a header out of untrusted file bytes. `buf: &[u8]` is a read-only byte
    // slice. The return type `Result<Header, VaultError>` is "either an `Ok(Header)`
    // on success, or an `Err(VaultError)` on failure" — Rust's checked-error type.
    fn parse(buf: &[u8]) -> Result<Header, VaultError> {
        if buf.len() < HEADER_LEN {
            return Err(VaultError::Truncated); // early-return an error variant
        }
        if &buf[0..8] != MAGIC {
            return Err(VaultError::BadMagic);
        }
        if buf[8] != FORMAT_VERSION {
            return Err(VaultError::BadVersion(buf[8]));
        }
        // `from_le_bytes` rebuilds a u32 from 4 little-endian bytes. `try_into()`
        // converts the variable-length slice into the fixed `[u8; 4]` it needs and
        // returns a `Result`; `.unwrap()` takes the `Ok` value or panics. It is
        // safe here because the length was already checked to be >= HEADER_LEN, so
        // these fixed sub-ranges always exist.
        let params = KdfParams {
            m_cost: u32::from_le_bytes(buf[9..13].try_into().unwrap()),
            t_cost: u32::from_le_bytes(buf[13..17].try_into().unwrap()),
            p_cost: u32::from_le_bytes(buf[17..21].try_into().unwrap()),
        };
        // Reject out-of-range params BEFORE the (expensive, memory-hard) derivation —
        // a tampered/forged header cannot force an unbounded Argon2 allocation.
        params.validate().map_err(|_| VaultError::BadParams)?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&buf[21..37]);
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&buf[37..61]);
        // Build and return the header. `Header { params, salt, nonce }` is field
        // shorthand: each field is set from the like-named local variable.
        Ok(Header { params, salt, nonce })
    }
}

/// An unlocked vault: the decrypted data, the derived key + KDF salt/params, and
/// the partitioned document store. The key zeroizes on drop; `vault` zeroizes via
/// its own `ZeroizeOnDrop`.
// Fields are private by default (encapsulated); only `vault` is marked `pub`, so
// callers can read/edit records directly but everything security-sensitive (the
// key, the lock) is reachable only through this module's methods.
pub struct OpenVault {
    pub vault: Vault,
    key: Key, // the symmetric encryption key derived from the passwords
    params: KdfParams,
    salt: [u8; SALT_LEN],
    /// The vault *file* (`<dir>/vault.pmv`).
    path: PathBuf,
    previous_access: i64,
    previous_generation: u64,
    read_only: bool,
    storage: VolumeStore,
    /// Set by the open path when the live `vault.pmv` was unreadable and the vault
    /// was recovered from an in-place redundant copy (§12.8) — a human-readable
    /// notice the front-ends surface so the user knows a roll-forward/rollback
    /// happened. `None` on a normal open.
    recovery_notice: Option<String>,
    /// Held for a writable session: the OS advisory lock on `vaultis.lock`.
    /// `None` for read-only opens. Released automatically when this `OpenVault`
    /// drops (including on process crash), so the lock never goes stale.
    // `Option<T>` is "either `Some(value)` or `None`" — Rust's null-free optional.
    // The leading `_` says "stored only to keep it alive, not read"; when this
    // struct is dropped the `WriteLock` is dropped too, which releases the lock.
    _write_lock: Option<WriteLock>,
}

/// Outcome of deleting a category (asset type / account type / account subtype) via
/// `OpenVault::remove_*`. Distinct from a hard `VaultError` so the UI can react with a
/// helpful message instead of a generic failure: the refusals (`InUse`/`HasSubtypes`)
/// are normal "can't do that yet" states, not errors. (Read-only opens still return
/// `Err(VaultError::ReadOnly)`; an actual save failure still returns `Err`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryRemoval {
    /// Deleted from the list and the change was persisted.
    Removed,
    /// The type/subtype was not in the list (nothing to do).
    NotFound,
    /// Refused: this many LIVE records still reference it (history does not count).
    InUse(usize),
    /// Refused: an account type that still has subtypes defined (delete those first).
    HasSubtypes,
}

/// An OS advisory lock on `<dir>/vaultis.lock`, held for the lifetime of a
/// writable [`OpenVault`]. The lock is taken on the open file handle, so the
/// kernel releases it when the handle closes — no stale lock file to clean up.
struct WriteLock {
    #[cfg(feature = "single-writer-lock")]
    _file: fs::File,
}

impl WriteLock {
    /// Acquire the single-writer lock for `dir`. Errors with
    /// [`VaultError::Locked`] if another writable session already holds it.
    // `Self` is shorthand for the type being impl'd (here `WriteLock`).
    #[cfg(feature = "single-writer-lock")]
    fn acquire(dir: &Path) -> Result<Self, VaultError> {
        let path = dir.join(LOCK_FILE); // `.join()` appends a path component
        // The lock file carries no contents; never truncate it (avoids racing a
        // concurrent holder's handle), just ensure it exists and is lockable.
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        // On Unix, open with O_NOFOLLOW so a symlink planted at the lock path is REFUSED
        // (ELOOP) rather than followed — matching single_instance.rs and append_frame, and
        // closing the one attacker-reachable open that previously followed symlinks. A
        // symlinked lock path is surfaced as `Locked`; we do NOT remove it (it lives in the
        // shared vault dir, and removing it could disrupt a legitimate concurrent holder).
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match opts.open(&path) {
            Ok(f) => f,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(VaultError::Locked),
            Err(e) => return Err(VaultError::Io(e)),
        };
        // NOTE: deliberately do NOT chmod this path — the lock file holds no secrets and its
        // parent directory is already 0700. (With O_NOFOLLOW above, a symlinked lock path is
        // now refused outright, so the old chmod-through-symlink concern cannot arise.)
        // `match` examines every possible variant of the Result and picks one arm.
        // `try_lock` returns `Ok(())` if we got the lock, or specific errors otherwise.
        match file.try_lock() {
            Ok(()) => Ok(WriteLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(VaultError::Locked), // someone else holds it
            Err(fs::TryLockError::Error(e)) => Err(VaultError::Io(e)),   // `e` binds the inner error
        }
    }

    /// No-op stand-in when the `single-writer-lock` feature is disabled (the mobile
    /// build). A single app process serializes all vault access behind one mutex, so
    /// there is no second writable process to exclude — this never returns `Locked`.
    /// The crash-safe atomic-commit + rekey roll-forward design already tolerates a
    /// crash without the lock, so dropping it only removes cross-process exclusion.
    #[cfg(not(feature = "single-writer-lock"))]
    fn acquire(_dir: &Path) -> Result<Self, VaultError> {
        Ok(WriteLock {})
    }
}

// With the lock feature ON, `WriteLock` owns an `fs::File` whose own `Drop` releases the
// OS lock, so the struct already has drop glue and explicit `drop(lock)` is meaningful. With
// the feature OFF the struct is empty and has none, which makes `drop(lock)` a `clippy::
// drop_non_drop` lint (and reads as a no-op). Give the disabled build a trivial `Drop` so
// every explicit `drop(lock)` / lock-release site compiles and reads the same on both configs.
#[cfg(not(feature = "single-writer-lock"))]
impl Drop for WriteLock {
    fn drop(&mut self) {}
}

// The main API surface of the vault: all the public operations live as methods here.
impl OpenVault {
    /// Create a brand-new vault in the directory containing `path`
    /// (`<dir>/vault.pmv`), protected by two passwords.
    // `path: PathBuf` is taken *by value* (this function now owns it / can keep it).
    // `pw1: &[u8]` / `pw2: &[u8]` are read-only borrows of the password bytes — the
    // caller keeps ownership, and we never copy or store them.
    pub fn create(path: PathBuf, pw1: &[u8], pw2: &[u8], params: KdfParams) -> Result<Self, VaultError> {
        if path.exists() {
            return Err(VaultError::AlreadyExists(path));
        }
        // Validate params on the WRITE path with the same bounds the READ path
        // (Header::parse) enforces, so we can never write a vault the reader would
        // later refuse to open (BadParams) — including its mirror/ring copies.
        params.validate().map_err(|_| VaultError::BadParams)?;
        let dir = parent_dir(&path); // `&path` lends the path without giving it away
        fs::create_dir_all(&dir)?;
        harden_dir(&dir);
        // fsync the new vault directory's own entry into its parent, so a power loss
        // right after the first save can't lose the directory that holds vault.pmv.
        sync_parent_dir(&dir);
        // Take the single-writer lock before writing anything into the directory.
        let write_lock = Some(WriteLock::acquire(&dir)?);
        // Re-check existence UNDER the lock. The pre-lock `exists()` above is a TOCTOU: a
        // competing creator could have written `vault.pmv` between that check and now. Once
        // we hold the single-writer lock this check is authoritative — without it the later
        // `save()` would `rename` a fresh, EMPTY vault over the winner's file and destroy it
        // (data loss), not merely report a confusing error.
        if path.exists() {
            return Err(VaultError::AlreadyExists(path));
        }
        // Discard any stale `.rekey` staging left in this directory. A fresh create
        // gets a brand-new vault id/key, so an unrelated leftover staging must never
        // be rolled forward over it by the next open's `recover_pending_rekey`
        // (matches `staged_rewrite`'s stale-staging clear). Best-effort.
        let _ = fs::remove_dir_all(dir.join(REKEY_DIR));
        // `::<SALT_LEN>` is a turbofish: it pins the generic length parameter so the
        // call returns a `[u8; SALT_LEN]` of random bytes.
        let salt = crypto::random_bytes::<SALT_LEN>()?;
        let key = crypto::derive_key_chained(pw1, pw2, &salt, &params)?;

        let mut vault = Vault::default(); // `default()` builds an empty/zeroed value
        vault.version = FORMAT_VERSION;
        vault.last_opened_at = records::unix_now();
        vault.id = records::random_id()?; // binds the volumes/manifests to this vault
        vault.categories = TypeLists::with_defaults();
        vault.audit.push(Change::new("vault_created", String::new()));

        let mut storage = VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size)?;
        // The store keeps a spare copy of each manifest when redundancy is on; it cannot
        // see `settings`, so the depth is handed to it here and on every later change.
        storage.set_redundancy(vault.settings.redundancy);

        // Construct the struct, moving each local into the matching field. After
        // this, those locals are owned by `open` and can't be used again.
        let mut open = OpenVault {
            vault,
            key,
            params,
            salt,
            path,
            previous_access: 0,
            previous_generation: 0,
            read_only: false,
            storage,
            recovery_notice: None,
            _write_lock: write_lock,
        };
        open.save()?; // first on-disk commit of the new vault file
        Ok(open)
    }

    // The three `open*` methods are thin wrappers that forward to `open_inner`
    // with the read-only flag set appropriately (a small convenience API).
    /// Unlock an existing vault read-write.
    pub fn open(path: PathBuf, pw1: &[u8], pw2: &[u8]) -> Result<Self, VaultError> {
        Self::open_inner(path, pw1, pw2, false)
    }

    /// Unlock an existing vault **read-only**: every mutating operation is refused
    /// and nothing is written to disk on open.
    pub fn open_read_only(path: PathBuf, pw1: &[u8], pw2: &[u8]) -> Result<Self, VaultError> {
        Self::open_inner(path, pw1, pw2, true)
    }

    /// Unlock, choosing read-only explicitly.
    pub fn open_with(path: PathBuf, pw1: &[u8], pw2: &[u8], read_only: bool) -> Result<Self, VaultError> {
        Self::open_inner(path, pw1, pw2, read_only)
    }

    fn open_inner(path: PathBuf, pw1: &[u8], pw2: &[u8], read_only: bool) -> Result<Self, VaultError> {
        let dir = parent_dir(&path);
        // Single-writer: a writable open takes the advisory lock first, so a
        // second writable instance fails fast and recovery/writes below are
        // exclusive. Read-only opens never take it.
        let write_lock = if read_only { None } else { Some(WriteLock::acquire(&dir)?) };
        // Finish/abort an interrupted password change before touching the vault.
        recover_pending_rekey(&dir, read_only)?;
        // Sweep stale atomic-write temps left by a crash mid-save (best-effort,
        // writable only). They are encrypted (no plaintext leak) but sweeping keeps
        // the dir tidy and avoids old-key temps lingering after a password change.
        if !read_only {
            sweep_stale_temps(&dir);
        }

        // Destructuring assignment: the returned tuple is unpacked into bindings at
        // once. `mut vault` is mutable so we can update its timestamp. The 4th element
        // is `Some(notice)` when the live `vault.pmv` was unreadable and we recovered
        // from an in-place redundant copy (§12.8); `None` on a normal open.
        let (mut vault, header, key, notice) = decrypt_with_redundancy(&path, pw1, pw2)?;
        let previous_access = vault.last_opened_at;
        let previous_generation = vault.generation;
        vault.last_opened_at = records::unix_now();

        // A concurrent writer's rekey can swap volume/manifest to the NEW key after a
        // read-only open already read the OLD vault.pmv (a reader-vs-writer race,
        // §9.16). In that window the store won't decrypt / a referenced doc looks
        // missing — surface a clear, retryable `RekeyPending` rather than an alarming
        // Crypto/`ArchiveMismatch`. Best-effort: re-checking `.rekey` catches the
        // in-flight case (a rekey that fully completed mid-read is the rare tail).
        let mut storage = match VolumeStore::open(&dir, &key, &vault.id, vault.settings.volume_max_size) {
            Ok(s) => s,
            Err(e) => {
                if dir.join(REKEY_DIR).exists() {
                    return Err(VaultError::RekeyPending);
                }
                return Err(e.into());
            }
        };
        // Hand the store the redundancy depth (it cannot read `settings` itself), so
        // every manifest it commits from here keeps a spare copy beside it — or, at
        // depth 0, so it clears any spare a previously-enabled session left behind.
        storage.set_redundancy(vault.settings.redundancy);
        // Consistency: every document a record references must be present.
        // `for id in ...` iterates the returned Vec, binding each element to `id`.
        for id in referenced_doc_ids(&vault) {
            if !storage.contains(&id) { // `!` is boolean NOT
                if dir.join(REKEY_DIR).exists() {
                    return Err(VaultError::RekeyPending);
                }
                return Err(VaultError::ArchiveMismatch);
            }
        }

        let mut open = OpenVault {
            vault,
            key,
            params: header.params,
            salt: header.salt,
            path,
            previous_access,
            previous_generation,
            read_only,
            storage,
            recovery_notice: notice,
            _write_lock: write_lock,
        };
        // Best-effort refresh of last-opened; skipped entirely in read-only mode.
        // `let _ =` discards the Result: if this write fails we still hand back the
        // opened vault (the refresh is non-essential). When we recovered from a
        // redundant copy, this same save also HEALS the live tree — it rewrites a
        // fresh `vault.pmv` (+ mirror) from the recovered state.
        //
        // This open-time save passes `rotate_ring=false` and so NEVER rotates the
        // redundancy ring (§12.8). It only refreshes `last_opened_at` (and, on a
        // recovery, heals the primary) — it never reflects a user content edit, so it
        // must not consume a "prior generation" slot. Rotating here (the prior behavior)
        // ringed the outgoing primary on *every* writable open; because `last_opened_at`
        // is refreshed just above, the bytes always differ, so a couple of routine no-edit
        // opens silently overwrote the whole ring with copies of the current state —
        // eroding the advertised undo/rollback depth the user opted into (audit M1, the
        // owner's non-destructive guarantee). The heal case already required false (its
        // outgoing primary is the corrupt file we recovered around); a normal open wants
        // false for the same net reason — no real generation is being superseded. A
        // genuine `save()` (an actual edit) still rotates via `rotate_ring=true`.
        if !read_only {
            // Reap any cross-epoch (old-password) redundancy leftover before the refresh
            // save (audit F3): a rekey whose best-effort cleanup partially failed can strand
            // an old-key bak/mirror that the rotate_ring=false refresh below would NOT remove
            // (it never rotates and only prunes ABOVE the configured depth). The recovered
            // open's salt is the current epoch's, so this only deletes genuinely foreign copies.
            sweep_foreign_epoch_copies(&open.path, &open.salt);
            let _ = open.save_internal(false);
        }
        Ok(open)
    }

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

    /// Re-encrypt the vault and write it atomically, bumping the write-generation.
    // `&mut self` is an *exclusive* borrow: this method may mutate the vault, and
    // while it runs no one else can read or write the same `OpenVault`.
    // `Result<(), VaultError>` returns `()` (the empty/unit value) on success —
    // i.e. "succeeded, no data to hand back".
    pub fn save(&mut self) -> Result<(), VaultError> {
        self.save_internal(true)
    }

    /// The save path. `rotate_ring` is `true` for a normal save — the outgoing
    /// generation is ringed into `bak1`. It is `false` for a recovery HEAL save
    /// (§12.8): there the outgoing `vault.pmv` is the corrupt file we just recovered
    /// *around*, so it must NOT be preserved as a "generation" (that would silently
    /// void a ring slot with garbage).
    fn save_internal(&mut self, rotate_ring: bool) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // `saturating_add` increments but clamps at the max value instead of
        // overflowing/panicking — a monotonically rising version counter.
        self.vault.generation = self.vault.generation.saturating_add(1);

        // Opt-in in-place redundancy (§12.8). `0` = off (the default): a single
        // `vault.pmv`, exactly as before. `N >= 1` = keep `N` prior generations and a
        // same-generation mirror so a bit-rotted vault file can be recovered in place.
        let depth = self.vault.settings.redundancy;

        // Capture the OUTGOING generation's bytes BEFORE the primary is overwritten,
        // but ring them in only AFTER the new primary commits (below) — so a FAILED
        // save never shifts/degrades the ring. Skipped on a heal (the outgoing
        // primary is known-bad) and on the first save (nothing to retain yet).
        //
        // Distinguish "no current primary yet" (NotFound → legitimately None, first save)
        // from a real read error. A blanket `.ok()` would collapse a transient EIO/EACCES —
        // or a TooLarge corruption signal — on the outgoing primary into None, silently
        // skipping the ring rotation AND letting us overwrite a primary we could not even
        // read. Instead, fail the save on any non-NotFound error so the caller retries; the
        // new primary is not written yet, so nothing is lost.
        let prev = if rotate_ring && depth > 0 {
            match read_capped_vault(&self.path) {
                Ok(bytes) => Some(bytes),
                Err(VaultError::NotFound(_)) => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };

        // The single authoritative commit point — identical to the non-redundant
        // path. If this fails (e.g. ENOSPC) the whole save fails, the live file is
        // untouched (atomic temp+rename), AND the ring is untouched (not yet rotated).
        write_vault_file(&self.path, &self.vault, &self.key, &self.salt, self.params)?;

        if depth > 0 {
            // Ring the outgoing generation in ONLY if it still DECODES under the current key.
            // `prev` was read with `read_capped_vault` (bytes only, no AEAD check), so a
            // primary that bit-rotted on disk during this session — or a corrupt primary left
            // behind by a heal whose best-effort re-save failed — would otherwise be ingested
            // as bak1 WHILE `rotate_generations` deletes the oldest GOOD generation, replacing
            // a recoverable snapshot with an unrecoverable one and eroding the ring. If the
            // outgoing bytes don't decode, drop them and prune only. (Audit 2026-07-03 A-3.)
            let prev_decodes = prev.as_deref().is_some_and(|b| decode_vault_with_key(b, &self.key).is_ok());
            match &prev {
                // Normal save: ring the (validated) outgoing generation into bak1 (atomic +
                // symlink-safe), shifting the rest and pruning beyond `depth`.
                Some(bytes) if prev_decodes => rotate_generations(&self.path, depth, bytes),
                // First save, a heal, OR a non-decoding outgoing primary: no good generation
                // to ring in — just prune any slots beyond the configured depth.
                _ => prune_generations_above(&self.path, depth),
            }
            // Best-effort same-generation mirror: a fresh, independent encryption of
            // the same vault (its own random nonce). Failing it does not fail the
            // save — the primary already committed.
            // Fault point (crash-test only): a crash/ENOSPC here is AFTER the
            // authoritative primary commit, so it must leave the vault openable from
            // the primary. On an injected ENOSPC the best-effort mirror is skipped.
            if crate::fault::point("redundancy.mirror").is_ok() {
                let _ = write_vault_file(&mirror_path(&self.path), &self.vault, &self.key, &self.salt, self.params);
            }
        } else {
            // Redundancy off: remove any copies left over from a previously-enabled
            // state, so disabling the feature also stops leaving old secrets on disk.
            cleanup_redundancy(&self.path);
        }
        // The change is now durably committed (write_vault_file above succeeded, else we
        // returned early). Refresh the `last_update_<UTC>` marker — strictly AFTER the commit,
        // never before, so a failed save can't bump it. Best-effort; see touch_last_update.
        touch_last_update(&parent_dir(&self.path));
        Ok(())
    }

    /// Best-effort regeneration of the in-place redundancy copies (mirror + `bak1`)
    /// under the CURRENT key, without bumping the generation. Used right after a
    /// rekey/compaction commit so the configured protection is restored immediately
    /// instead of being absent until the next ordinary save (§12.8).
    fn refresh_redundancy_copies(&self) {
        let depth = self.vault.settings.redundancy;
        if depth == 0 {
            return;
        }
        // Fault point (crash-test only): a crash here leaves the just-committed vault
        // with no redundant copies until the next save — recovery from the primary is
        // unaffected (it is the authoritative, already-durable tree).
        let _ = crate::fault::point("redundancy.refresh");
        // A fresh mirror of the just-committed vault, and a bak1 copy of the live
        // primary (the post-rekey generations legitimately reset to the new epoch).
        let _ = write_vault_file(&mirror_path(&self.path), &self.vault, &self.key, &self.salt, self.params);
        // Ring bak1 in ONLY if the primary still decodes under the current key — the same
        // A-3 invariant `save_internal` enforces via `prev_decodes`. This is the other
        // site that feeds the recovery ring, and it was reading the primary back with no
        // AEAD check: a primary that bit-rotted between the rename above and this read
        // would be copied into bak1 as an unrecoverable "generation". In practice the
        // bytes were just written and fsync'd by this process under the single-writer
        // lock, so this is defence in depth — but an invariant enforced at one of two
        // sites is not an invariant.
        if let Ok(bytes) = read_capped_vault(&self.path)
            && decode_vault_with_key(&bytes, &self.key).is_ok()
        {
            let _ = write_bytes_atomic(&bak_path(&self.path, 1), &bytes);
        }
        prune_generations_above(&self.path, depth);
        // The document index's spares were swapped out with the old manifest directory
        // (rekey/compact replace it wholesale), so write them again under the new key.
        self.storage.refresh_manifest_mirrors(&self.key);
    }

    /// Set the in-place redundancy depth (§12.8): `0` = off, `N >= 1` = keep a
    /// same-generation mirror plus `N` prior generations of `vault.pmv`. Clamped to
    /// [`MAX_REDUNDANCY`]. Persists immediately (the new copies appear on this save).
    pub fn set_redundancy(&mut self, depth: u32) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let depth = depth.min(MAX_REDUNDANCY);
        self.vault.settings.redundancy = depth;
        self.vault.audit.push(Change::new("redundancy_changed", depth.to_string()));
        // Apply the change to the document index's spare copies too: turning it off
        // deletes them, turning it on writes them now rather than at the next upload
        // (matching `refresh_redundancy_copies` for the vault file's own copies).
        self.storage.set_redundancy(depth);
        self.storage.refresh_manifest_mirrors(&self.key);
        self.save()
    }

    /// The current in-place redundancy depth (`0` = off).
    pub fn redundancy(&self) -> u32 {
        self.vault.settings.redundancy
    }

    /// A notice if this vault was recovered from a redundant copy on open (§12.8),
    /// for the front-ends to surface; `None` on a normal open.
    pub fn recovery_notice(&self) -> Option<&str> {
        self.recovery_notice.as_deref()
    }

    /// Snapshot this OPEN vault's on-disk tree into `dest_dir` (the last-saved state;
    /// encrypted files copied as-is). Use this from an open session instead of the
    /// free [`backup`] function: a writable session already holds the single-writer
    /// lock, and re-acquiring it (as the free function does) would self-deadlock —
    /// flock binds to the open file description, so a second in-process acquire returns
    /// `Locked`. A read-only session holds no lock, so this acquires one for the
    /// duration of the snapshot (to exclude a concurrent writer in another process).
    pub fn backup(&self, dest_dir: &Path) -> Result<PathBuf, VaultError> {
        if !self.path.exists() {
            return Err(VaultError::NotFound(self.path.clone()));
        }
        let src_dir = parent_dir(&self.path);
        if self.read_only {
            // No write lock held by this session — take one for the snapshot, tolerating
            // a source we cannot write to (see `lock_for_read_only_copy`).
            let _lock = lock_for_read_only_copy(&src_dir)?;
            backup_snapshot(&self.path, &src_dir, dest_dir)
        } else {
            // Writable session already holds the lock; reuse it (do NOT re-acquire).
            backup_snapshot(&self.path, &src_dir, dest_dir)
        }
    }

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
    fn staged_rewrite(
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

    /// Reclaim space without changing the passwords. `opts.volume` rewrites the
    /// document store keeping only live blobs (dropping the dead frames left by
    /// updates/deletes), reusing the crash-safe staged rewrite above. `opts.json`
    /// trims each record's per-edit `history` (older than the cutoff, or all),
    /// leaving the vault-level `audit` intact and appending a `compacted` event.
    /// Either or both may run; refused on a read-only handle. Returns a report of
    /// what was reclaimed.
    pub fn compact(&mut self, opts: &CompactOptions) -> Result<CompactReport, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Measure reclaimable garbage and removable history BEFORE mutating, so the
        // report reflects the change (the staged rewrite reproduces live frames at
        // their original size, so committed-after ≈ live-before).
        let (committed, live) = self.storage.space_stats();
        let bytes_reclaimed = if opts.volume { committed.saturating_sub(live) } else { 0 };
        let history_removed = if opts.json {
            records::history_stats(&self.vault, opts.history_cutoff, opts.drop_all_history)
        } else {
            0
        };
        let partitions_before = self.storage.partition_count();
        let detail = compaction_detail(opts, bytes_reclaimed, history_removed);

        if opts.volume {
            // Re-pack the volume AND (optionally) trim history in one atomic commit.
            // The closure captures only Copy values + the owned `detail` string, so
            // it does not borrow `self` (no conflict with `&mut self`).
            let (cutoff, drop_all, do_json) = (opts.history_cutoff, opts.drop_all_history, opts.json);
            self.staged_rewrite(None, move |v| {
                if do_json {
                    records::compact_history(v, cutoff, drop_all);
                }
                v.audit.push(Change::new("compacted", detail));
            })?;
        } else {
            // JSON-only: trim history in place, then the normal atomic vault save
            // (which bumps the generation). The volume is untouched.
            records::compact_history(&mut self.vault, opts.history_cutoff, opts.drop_all_history);
            self.vault.audit.push(Change::new("compacted", detail));
            self.save()?;
        }

        Ok(CompactReport {
            bytes_reclaimed,
            history_removed,
            partitions_before,
            partitions_after: self.storage.partition_count(),
        })
    }

    /// Compute what `compact` *would* reclaim without writing anything (used by
    /// `--dry-run`; safe on a read-only handle). `partitions_after` mirrors the
    /// current count — the post-compaction count is only known after a real run.
    pub fn compact_dry_run(&self, opts: &CompactOptions) -> CompactReport {
        let (committed, live) = self.storage.space_stats();
        CompactReport {
            bytes_reclaimed: if opts.volume { committed.saturating_sub(live) } else { 0 },
            history_removed: if opts.json {
                records::history_stats(&self.vault, opts.history_cutoff, opts.drop_all_history)
            } else {
                0
            },
            partitions_before: self.storage.partition_count(),
            partitions_after: self.storage.partition_count(),
        }
    }

    // Simple read-only getters: `&self` borrows the vault, and each returns a copy
    // of a small `Copy` field (integers copy implicitly, so no `.clone()` needed).
    pub fn previous_access(&self) -> i64 {
        self.previous_access
    }

    pub fn opened_generation(&self) -> u64 {
        self.previous_generation
    }

    /// The per-partition volume-size cap, in bytes.
    pub fn volume_max_size(&self) -> u64 {
        self.vault.settings.volume_max_size
    }

    /// Set the per-partition volume-size cap (bytes, clamped to the same
    /// [MIN_VOLUME_MAX_SIZE, MAX_VOLUME_MAX_SIZE] window as import_tree). Updates the
    /// saved settings and the live store so the change governs **future** placement this
    /// session, then persists. Existing partitions are untouched.
    pub fn set_volume_max_size(&mut self, bytes: u64) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Clamp to the same bounds import_tree uses (single source of truth): a sub-64-KiB cap
        // would put nearly every new document in its own partition (vol.N/manifest.N + fsync +
        // dir-sync per doc) — self-inflicted disk/inode/IO amplification — and an absurd ceiling
        // is likewise rejected. A floor of 1 (the old value) did NOT prevent this fragmentation.
        let bytes = bytes.clamp(MIN_VOLUME_MAX_SIZE, MAX_VOLUME_MAX_SIZE);
        self.vault.settings.volume_max_size = bytes;
        self.storage.set_max_size(bytes);
        self.vault.audit.push(Change::new("volume_size_changed", bytes.to_string()));
        self.save()
    }

    // --- Documents (delegated to the partitioned store) ----------------------

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
    fn is_tombstoned(&self, file_id: &str) -> bool {
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

    // --- Cross-vault merge: "update this vault from another vault" ------------
    //
    // A one-way, ADDITIVE pull: records that are newer (by `updated_at`) or entirely
    // new in `source` are copied into `self`, along with the document blobs they
    // reference. Nothing in `self` is ever deleted. See `crate::merge` for the
    // semantics and `docs/DESIGN.md` for the security/crash-safety rationale.

    /// Compute the patch that [`apply_merge_from`](Self::apply_merge_from) would apply,
    /// for previewing. Read-only: touches no files and mutates nothing. `source` must be
    /// a *separate* already-open vault (opened with its own two passwords).
    ///
    /// A record is selected when its id is absent from `self` (New) or its `updated_at`
    /// is strictly greater than the same-id record in `self` (Updated). A selected record
    /// whose referenced document cannot be safely resolved — tombstoned in `self`, missing
    /// from `source`, or carrying an unsafe id/path — is reported in `skipped` and NOT
    /// applied (so the merge can never brick the vault or resurrect a deleted-then-garbage
    /// frame). Every displayed path is validated control/bidi-safe.
    pub fn plan_merge_from(&self, source: &OpenVault) -> Result<crate::merge::MergePlan, VaultError> {
        use std::collections::BTreeMap;
        // The source is UNTRUSTED: its `vault.id` is AEAD-authenticated but attacker-chosen,
        // and it is rendered in the preview AND recorded in this vault's audit log. Apply the
        // same allowlist `import_tree` uses (short ASCII-alphanumeric) so a crafted source
        // can't inject control/bidi bytes into the UI or persist them into our audit.
        let sid = &source.vault.id;
        if sid.is_empty() || sid.len() > 64 || !sid.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe vault id in merge source: {sid:?}"))));
        }
        let mut plan = crate::merge::MergePlan { source_vault_id: source.vault.id.clone(), ..Default::default() };
        // Dedup the blob plan by id (a doc can be referenced by several records).
        let mut blobs: BTreeMap<String, crate::merge::PlannedBlob> = BTreeMap::new();

        // Resolve every doc id a selected record references. Returns `Err(reason)` if the
        // record must be blocked, else `Ok(())` having recorded each doc in `blobs`.
        // `&mut blobs` is threaded in because a closure can't capture it while `self` is
        // also borrowed by the outer iteration.
        let resolve = |this: &OpenVault,
                       docs: &[String],
                       blobs: &mut BTreeMap<String, crate::merge::PlannedBlob>|
         -> Result<(), String> {
            for id in docs {
                if !is_safe_blob_id(id) {
                    return Err("references a document with an unsafe id".into());
                }
                if this.vault.deleted_docs.iter().any(|t| t == id) {
                    // Tombstoned here: a lingering deleted frame may still exist, so
                    // re-adding the same id in place would risk a duplicate frame (R-8).
                    return Err("references a document deleted in this vault (compact to unblock)".into());
                }
                if this.storage.contains(id) {
                    blobs.entry(id.clone()).or_insert(crate::merge::PlannedBlob {
                        id: id.clone(),
                        path: this.storage.entry(id).map(|e| e.path.clone()).unwrap_or_default(),
                        size: this.storage.entry(id).map(|e| e.size).unwrap_or(0),
                        already_present: true,
                    });
                    continue;
                }
                // Must be copied from the source — it has to be there (source opened
                // consistent) and carry a safe, displayable path.
                match source_entry_validated(source, id)? {
                    Some((path, size)) => {
                        blobs.entry(id.clone()).or_insert(crate::merge::PlannedBlob {
                            id: id.clone(),
                            path,
                            size,
                            already_present: false,
                        });
                    }
                    None => return Err("references a document missing from the source vault".into()),
                }
            }
            Ok(())
        };

        // One closure per collection, run via the generic `plan_collection` helper so the
        // recency diff + blocked-record handling is written once. `docs_of` extracts the
        // blob ids a record references (empty for Instruction/Account).
        self.plan_collection(crate::merge::RecordKind::Urgent, &self.vault.urgent, &source.vault.urgent, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Instruction, &self.vault.instructions, &source.vault.instructions, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::TrustWill, &self.vault.trust_wills, &source.vault.trust_wills, |r| r.file.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Asset, &self.vault.assets, &source.vault.assets, |r| r.statement.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Account, &self.vault.accounts, &source.vault.accounts, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::RealEstate, &self.vault.real_estate, &source.vault.real_estate, |r| r.documents.clone(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::TaxFiling, &self.vault.tax_filings, &source.vault.tax_filings, |r| r.documents.clone(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::GeneralDocument, &self.vault.general_documents, &source.vault.general_documents, |r| r.file.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;

        plan.blobs = blobs.into_values().collect();

        // Reconcile category TYPES: collect the asset/account types + subtypes the to-apply
        // records use that this vault's editable lists lack. Without this, a merged record's
        // type wouldn't appear in Config or the dropdowns. Read-only here; `apply` adds them.
        let cats = &self.vault.categories;
        let mut seen_cat: std::collections::HashSet<String> = std::collections::HashSet::new();
        let accepted_ids = |kind: crate::merge::RecordKind| -> std::collections::HashSet<&str> {
            plan.records.iter().filter(|r| r.kind == kind).map(|r| r.id.as_str()).collect()
        };
        let asset_ids = accepted_ids(crate::merge::RecordKind::Asset);
        // Only the FIRST source occurrence of an accepted id is actually applied
        // (merge_records is first-occurrence-wins), so a later DUPLICATE id carrying a
        // different type must not seed a phantom category that no applied record uses.
        let mut done_assets: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.assets {
            if !asset_ids.contains(a.id.as_str()) || !done_assets.insert(a.id.as_str()) {
                continue;
            }
            // Sanitize the UNTRUSTED source type with display_safe UP FRONT, then make the
            // existence + dedup decisions on that SAME value apply_merge_from stores — otherwise
            // two raw types that sanitize equal, or a raw type that sanitizes to an existing
            // category, make the previewed new-category count drift from what apply actually adds
            // (and a crafted source vault must never inject bidi/escape chars into the screen the
            // user authorizes). `to_ascii_lowercase` matches the apply-time `eq_ignore_ascii_case`.
            let t = records::display_safe(a.asset_type.trim());
            if !t.is_empty()
                && !cats.asset.iter().any(|x| x.eq_ignore_ascii_case(&t))
                && seen_cat.insert(format!("a\u{1f}{}", t.to_ascii_lowercase()))
            {
                plan.new_categories.push(format!("asset type \u{201c}{t}\u{201d}"));
            }
        }
        let acct_ids = accepted_ids(crate::merge::RecordKind::Account);
        let mut done_accounts: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.accounts {
            if !acct_ids.contains(a.id.as_str()) || !done_accounts.insert(a.id.as_str()) {
                continue; // first-occurrence-wins, like the asset loop above
            }
            let t = records::display_safe(a.account_type.trim()); // sanitized up front, as above
            if !t.is_empty() {
                if !cats.account.iter().any(|x| x.name.eq_ignore_ascii_case(&t))
                    && seen_cat.insert(format!("c\u{1f}{}", t.to_ascii_lowercase()))
                {
                    plan.new_categories.push(format!("account type \u{201c}{t}\u{201d}"));
                }
                let st = records::display_safe(a.account_subtype.trim());
                if !st.is_empty()
                    && !cats.subtypes_for(&t).iter().any(|x| x.eq_ignore_ascii_case(&st))
                    && seen_cat.insert(format!("s\u{1f}{}\u{1f}{}", t.to_ascii_lowercase(), st.to_ascii_lowercase()))
                {
                    plan.new_categories.push(format!("subtype \u{201c}{st}\u{201d} under \u{201c}{t}\u{201d}"));
                }
            }
        }

        Ok(plan)
    }

    /// Generic per-collection planner: run the recency diff, then for each selected source
    /// record resolve its referenced docs; on success record a [`PlannedRecord`], on a
    /// block record a [`SkippedRecord`]. Shared by all seven collections.
    #[allow(clippy::too_many_arguments)]
    fn plan_collection<R: crate::records::Record>(
        &self,
        kind: crate::merge::RecordKind,
        current: &[R],
        src: &[R],
        docs_of: impl Fn(&R) -> Vec<String>,
        resolve: &impl Fn(&OpenVault, &[String], &mut std::collections::BTreeMap<String, crate::merge::PlannedBlob>) -> Result<(), String>,
        blobs: &mut std::collections::BTreeMap<String, crate::merge::PlannedBlob>,
        plan: &mut crate::merge::MergePlan,
    ) -> Result<(), VaultError> {
        for sel in crate::merge::collection_changes(current, src) {
            let s = &src[sel.source_index];
            let docs = docs_of(s);
            // Resolve into a fresh PER-RECORD scratch map: a blocked record (resolve -> Err)
            // must not leak its docs into the committed plan's blob list. Using a small empty
            // map and `extend`-ing on success keeps the planner linear in total doc references
            // (the old `blobs.clone()` per record was O(records x accumulated-blobs); `resolve`
            // only inserts via entry().or_insert(), so the deduped union is identical).
            let mut scratch = std::collections::BTreeMap::new();
            match resolve(self, &docs, &mut scratch) {
                Ok(()) => {
                    blobs.extend(scratch);
                    plan.records.push(crate::merge::PlannedRecord {
                        kind,
                        change: sel.change,
                        id: s.id().to_string(),
                        // Sanitize the UNTRUSTED source label for display: this string is
                        // rendered into the CLI/TUI merge preview the user authorizes, so a
                        // crafted source vault must not inject terminal escapes or bidi/zero-
                        // width characters that spoof which records are being merged in.
                        label: records::display_safe(&s.label()),
                        current_updated_at: sel.current_updated_at,
                        source_updated_at: s.updated_at(),
                    });
                }
                Err(reason) => plan.skipped.push(crate::merge::SkippedRecord {
                    kind,
                    id: s.id().to_string(),
                    label: records::display_safe(&s.label()), // untrusted source label — see above
                    reason,
                }),
            }
        }
        Ok(())
    }

    /// Apply the merge from `source` into `self`: copy the needed document blobs, replace/
    /// insert the newer/new records, append a vault-level audit entry, and atomically save.
    /// Recomputes the plan internally against the live `source` (so the applied set always
    /// matches a freshly-built [`plan_merge_from`]), then commits **add-only**:
    ///
    /// 1. copy each needed blob into this vault's volume (each `storage.put` is individually
    ///    durable; an interrupted copy only leaves harmless orphan frames),
    /// 2. replace/insert the records (in memory),
    /// 3. one atomic `save()` of `vault.pmv` — the single commit point.
    ///
    /// Because nothing is ever removed or rewritten, this needs no staged multi-file commit:
    /// every referenced blob is durable *before* the `vault.pmv` that references it, so the
    /// open-time `referenced ⊆ stored` invariant always holds (a crash leaves the old vault
    /// plus harmless garbage). Requires `--write`.
    pub fn apply_merge_from(&mut self, source: &OpenVault) -> Result<crate::merge::MergeReport, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Recompute the plan against the live source — never trust a caller-held plan, and
        // skip the work entirely when there is nothing to do.
        let plan = self.plan_merge_from(source)?;
        let mut report = crate::merge::MergeReport::default();
        if plan.is_empty() {
            report.records_skipped = plan.skipped.len();
            return Ok(report);
        }

        // (1) Copy every not-already-present blob into THIS vault's volume, re-encrypted
        // under our key + vault id (a fresh nonce; never a frame byte-copy). Re-validate
        // each id/path defensively at the moment of use.
        for b in &plan.blobs {
            if b.already_present {
                continue;
            }
            if !is_safe_blob_id(&b.id) || !is_safe_doc_path(&b.path) {
                return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe document in merge source: {:?}", b.id))));
            }
            // Skip if it somehow already arrived (idempotent re-put guard: never append a
            // second frame for one id — the R-8 hazard).
            if self.storage.contains(&b.id) {
                continue;
            }
            let entry = source
                .storage
                .entry(&b.id)
                .ok_or_else(|| VaultError::Storage(StorageError::Corrupt(format!("merge source lost document {:?}", b.id))))?;
            let bytes = source.storage.read(&b.id, &source.key)?; // bounded + id/path-verified
            self.storage.put(&b.id, &entry.path, &bytes, entry.uploaded_at, &self.key)?;
            report.blobs_copied += 1;
            report.bytes_copied = report.bytes_copied.saturating_add(entry.size);
        }

        // (1b) FAIL-CLOSED, *before* mutating any record: every document the accepted records
        // reference must now be in the store (just-copied or already present). `plan.blobs`
        // is exactly that referenced set, so checking it here — rather than over the merged
        // vault after the mutation — means a storage anomaly aborts with BOTH the on-disk and
        // the in-memory vault still intact (no half-merged, never-committed state to leak).
        for b in &plan.blobs {
            if !self.storage.contains(&b.id) {
                return Err(VaultError::ArchiveMismatch);
            }
        }

        // (2) Group the accepted ids by collection, then replace/insert verbatim.
        let accepted = |kind: crate::merge::RecordKind| -> std::collections::HashSet<&str> {
            plan.records.iter().filter(|r| r.kind == kind).map(|r| r.id.as_str()).collect()
        };
        let (a0, u0) = crate::merge::merge_records(&mut self.vault.urgent, &source.vault.urgent, &accepted(crate::merge::RecordKind::Urgent));
        let (a1, u1) = crate::merge::merge_records(&mut self.vault.instructions, &source.vault.instructions, &accepted(crate::merge::RecordKind::Instruction));
        let (a2, u2) = crate::merge::merge_records(&mut self.vault.trust_wills, &source.vault.trust_wills, &accepted(crate::merge::RecordKind::TrustWill));
        let (a3, u3) = crate::merge::merge_records(&mut self.vault.assets, &source.vault.assets, &accepted(crate::merge::RecordKind::Asset));
        let (a4, u4) = crate::merge::merge_records(&mut self.vault.accounts, &source.vault.accounts, &accepted(crate::merge::RecordKind::Account));
        let (a5, u5) = crate::merge::merge_records(&mut self.vault.real_estate, &source.vault.real_estate, &accepted(crate::merge::RecordKind::RealEstate));
        let (a6, u6) = crate::merge::merge_records(&mut self.vault.tax_filings, &source.vault.tax_filings, &accepted(crate::merge::RecordKind::TaxFiling));
        let (a7, u7) = crate::merge::merge_records(&mut self.vault.general_documents, &source.vault.general_documents, &accepted(crate::merge::RecordKind::GeneralDocument));
        report.records_added = a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7;
        report.records_updated = u0 + u1 + u2 + u3 + u4 + u5 + u6 + u7;
        report.records_skipped = plan.skipped.len();

        // (2b) Reconcile category TYPES so the merged records' asset/account types + subtypes
        // appear in Config and the dropdowns (the lists' add_* are case-insensitive dedup, and
        // the subtype add finds the type just added above). Persisted by the single save below.
        let asset_ids = accepted(crate::merge::RecordKind::Asset);
        // Dedup by id (first-occurrence-wins) so a duplicate source id with a different
        // type can't add an orphan category type whose only "user" was the un-applied dup.
        let mut done_assets: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.assets {
            if !asset_ids.contains(a.id.as_str()) || !done_assets.insert(a.id.as_str()) {
                continue;
            }
            // Sanitize the UNTRUSTED source type with display_safe BEFORE storing it, exactly
            // as plan_merge_from did for the approval preview — otherwise the category the user
            // approved (cleaned) and the one persisted (raw, possibly bidi/zero-width-spoofed)
            // would diverge, letting a crafted source vault slip a spoofed type into the lists.
            let t = records::display_safe(a.asset_type.trim());
            if !t.is_empty() && self.vault.categories.add_asset_type(&t) {
                report.categories_added += 1;
            }
        }
        let acct_ids = accepted(crate::merge::RecordKind::Account);
        let mut done_accounts: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.accounts {
            if !acct_ids.contains(a.id.as_str()) || !done_accounts.insert(a.id.as_str()) {
                continue; // first-occurrence-wins, like the asset loop above
            }
            let t = records::display_safe(a.account_type.trim()); // sanitized to match the preview
            if !t.is_empty() {
                if self.vault.categories.add_account_type(&t) {
                    report.categories_added += 1;
                }
                let st = records::display_safe(a.account_subtype.trim());
                if !st.is_empty() && self.vault.categories.add_account_subtype(&t, &st) {
                    report.categories_added += 1;
                }
            }
        }

        // Vault-level audit entry — counts only, no record contents or document ids.
        let short = plan.source_vault_id.get(..8).unwrap_or(plan.source_vault_id.as_str());
        self.vault.audit.push(records::Change::new(
            "merged",
            format!(
                "from vault {short}: {} new, {} updated, {} document(s) copied, {} type(s) added",
                report.records_added, report.records_updated, report.blobs_copied, report.categories_added
            ),
        ));

        // (3) The single atomic commit. The referenced⊆stored invariant was already verified
        // in (1b) before any mutation, so we only have to guard the save itself: if it fails
        // (e.g. ENOSPC), the in-memory vault now holds the merged records + audit entry but
        // the on-disk vault is still the old one — POISON the handle so a later unrelated
        // save() can never silently flush this never-committed merge (mirrors `compact`'s
        // partial-commit poisoning). The caller must reopen.
        if let Err(e) = self.save() {
            self.read_only = true;
            return Err(e);
        }
        Ok(report)
    }

    // --- Category lists (stored in the vault) --------------------------------

    // Returns a *borrow* (`&TypeLists`) into the vault rather than a copy: the
    // caller may read the category lists but the data stays owned by the vault.
    pub fn categories(&self) -> &TypeLists {
        &self.vault.categories
    }

    /// How many live asset/liability records use `name` as their `asset_type`
    /// (trimmed, case-insensitive). `0` means the configured type is unused — safe to remove,
    /// and flagged as such in the Config screen. This is the single source of truth for the
    /// "in use?" question (`remove_asset_type` uses it too). Matching is trimmed on BOTH sides
    /// so it keys off the same normalized value `add_*`/`sync_types_from_records` store — a
    /// whitespace-padded record value (legacy/imported data) still counts as in use.
    pub fn asset_type_usage(&self, name: &str) -> usize {
        let name = name.trim();
        self.vault.assets.iter().filter(|a| a.asset_type.trim().eq_ignore_ascii_case(name)).count()
    }

    /// How many live accounts use `name` as their `account_type` (trimmed, case-insensitive).
    /// `0` means the configured type is unused. Shared with `remove_account_type`.
    pub fn account_type_usage(&self, name: &str) -> usize {
        let name = name.trim();
        self.vault.accounts.iter().filter(|a| a.account_type.trim().eq_ignore_ascii_case(name)).count()
    }

    /// How many live accounts use the (`type_name`, `subtype`) pair (trimmed, case-insensitive).
    /// `0` means the configured subtype is unused. Shared with `remove_account_subtype`.
    pub fn account_subtype_usage(&self, type_name: &str, subtype: &str) -> usize {
        let (type_name, subtype) = (type_name.trim(), subtype.trim());
        self.vault
            .accounts
            .iter()
            .filter(|a| {
                a.account_type.trim().eq_ignore_ascii_case(type_name)
                    && a.account_subtype.trim().eq_ignore_ascii_case(subtype)
            })
            .count()
    }

    pub fn add_asset_type(&mut self, name: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_asset_type(name))
    }

    pub fn add_account_type(&mut self, name: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_account_type(name))
    }

    pub fn add_account_subtype(&mut self, type_name: &str, subtype: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_account_subtype(type_name, subtype))
    }

    /// Scan every record and add any asset/account **type** + account **subtype** it uses that
    /// is missing from the editable category lists (§4.2), so types brought in by a merge,
    /// `import-tree`, or older data show up in Config and the dropdowns. Returns the number of
    /// category entries added; a no-op returns `Ok(0)` without writing. Requires `--write`.
    ///
    /// **Purely additive**: this only inserts missing entries — it NEVER deletes a configured
    /// type or subtype, including ones no record currently uses. (Removal is a deliberate,
    /// per-entry action via `remove_*`.) This is what makes it safe to run automatically at
    /// vault open.
    pub fn sync_types_from_records(&mut self) -> Result<usize, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Snapshot the category list + audit length so a save failure rolls the in-memory
        // state back to match disk. Sync now runs automatically at open, so it must be
        // all-or-nothing: never leave memory holding types the persisted vault doesn't.
        let cats_before = self.vault.categories.clone();
        let audit_len = self.vault.audit.len();
        let mut added = 0usize;
        // Snapshot the type strings first so the immutable record borrow is released before the
        // category lists are mutated (the `add_*` are case-insensitive dedup).
        let asset_types: Vec<String> = self.vault.assets.iter().map(|a| a.asset_type.clone()).collect();
        for t in &asset_types {
            // Sanitize with display_safe BEFORE the type enters the category list. A record's
            // type field can be UNTRUSTED (it arrived via merge or import_tree), and this sync
            // runs automatically on every writable open — so without this, a bidi/zero-width-
            // spoofed record type would be re-injected RAW here, silently undoing the exact
            // sanitization apply_merge_from does (idempotent + a no-op for normal names).
            let t = records::display_safe(t.trim());
            if !t.is_empty() && self.vault.categories.add_asset_type(&t) {
                added += 1;
            }
        }
        let accts: Vec<(String, String)> =
            self.vault.accounts.iter().map(|a| (a.account_type.clone(), a.account_subtype.clone())).collect();
        for (t, st) in &accts {
            let t = records::display_safe(t.trim()); // sanitize untrusted record type, as above
            if t.is_empty() {
                continue;
            }
            if self.vault.categories.add_account_type(&t) {
                added += 1;
            }
            let st = records::display_safe(st.trim());
            if !st.is_empty() && self.vault.categories.add_account_subtype(&t, &st) {
                added += 1;
            }
        }
        if added > 0 {
            self.vault.audit.push(records::Change::new("types_synced", format!("{added} category type(s) added from records")));
            if let Err(e) = self.save() {
                // Roll the in-memory additions (and the audit entry) back so memory matches
                // the unchanged on-disk vault.
                self.vault.categories = cats_before;
                self.vault.audit.truncate(audit_len);
                return Err(e);
            }
        }
        Ok(added)
    }

    /// Delete an Asset/Liability type — only if **no live asset/liability record**
    /// still has that `asset_type`. (History never blocks: a `Change.detail` string
    /// is not the `asset_type` field, so it is not scanned here.) See [`CategoryRemoval`].
    pub fn remove_asset_type(&mut self, name: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let used = self.asset_type_usage(name);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_asset_type(name))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    /// Delete an account type — refused if it still has **subtypes defined**
    /// (delete those first) or if any **live account** still has that `account_type`.
    pub fn remove_account_type(&mut self, name: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Block while subtypes exist (chosen policy): the user removes each subtype
        // first, then the now-empty type.
        if !self.vault.categories.subtypes_for(name).is_empty() {
            return Ok(CategoryRemoval::HasSubtypes);
        }
        let used = self.account_type_usage(name);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_account_type(name))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    /// Delete a subtype under an account type — only if **no live account** has that
    /// (`account_type`, `account_subtype`) pair.
    pub fn remove_account_subtype(&mut self, type_name: &str, subtype: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let used = self.account_subtype_usage(type_name, subtype);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_account_subtype(type_name, subtype))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    // Shared helper for the three `add_*` methods above. `edit: impl FnOnce(...)`
    // accepts any closure (here `|c| c.add_*(...)`) that takes an exclusive borrow
    // of the category lists and returns whether it actually changed something.
    // `FnOnce` means the closure is callable at least once. This is the generics +
    // higher-order-function pattern: behavior is passed in as a parameter.
    fn mutate_categories(&mut self, edit: impl FnOnce(&mut TypeLists) -> bool) -> Result<bool, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        if edit(&mut self.vault.categories) { // run the closure; only persist if it changed state
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// The directory containing the vault file (its parent, or "." if none).
fn parent_dir(vault_file: &Path) -> PathBuf {
    // `.parent()` yields an `Option<&Path>`. The `match` has a guarded arm:
    // `Some(p) if <cond>` matches only when there's a parent AND it's non-empty;
    // `_` is the catch-all (covers `None` and the empty-parent case).
    match vault_file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(), // own a copy of the borrowed path
        _ => PathBuf::from("."), // fall back to the current directory
    }
}

/// Normalize `location` and join `filename` into a virtual path "/a/b/file".
/// Exposed to the UIs so they can validate path length against
/// [`storage::MAX_PATH_LEN`] with the exact string the core will store.
// `pub(crate)` = visible to the rest of this crate but not external callers.
pub fn virtual_path(location: &str, filename: &str) -> String {
    let loc = normalize_dir(location);
    // `if ... { } else { }` is an *expression* here: the chosen branch's value is
    // returned. `format!` builds a `String` (like sprintf); `{filename}` inlines it.
    if loc.is_empty() { format!("/{filename}") } else { format!("{loc}/{filename}") }
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

/// True if `id` is a safe single path component to use as a blob filename when
/// reading an (untrusted) import mirror: non-empty, no path separators, no NUL,
/// and not a `.`/`..` traversal. Real ids are random hex, so this never rejects a
/// genuine export — it only stops a crafted mirror from escaping its directory.
fn is_safe_blob_id(id: &str) -> bool {
    // Blob ids we generate are always 32 lowercase hex chars (`records::random_id`),
    // so a hex-digit allowlist is both correct and the tightest safe check for an
    // UNTRUSTED import mirror's ids. Crucially it rejects every filesystem-escape
    // vector that the old `!contains(['/','\\','\0'])` denylist missed on Windows:
    // `:` (NTFS alternate-data-stream `foo:bar` / drive-relative `C:evil`), reserved
    // device names (NUL/CON/COM1 — they contain non-hex letters), control bytes,
    // trailing dot/space, and `.`/`..`. The id is later used as a real filename on
    // both import-read (`vol.<p>/<id>`) and export-write, so this must hold.
    // LOWERCASE hex only: `records::random_id` emits lowercase, and accepting
    // uppercase too would let an import-planted `AA..` and a real `aa..` coexist on
    // Linux but COLLIDE on a case-insensitive filesystem (APFS/NTFS), breaking a
    // later `export_tree`/backup-via-mirror with an EEXIST mid-walk.
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// True if an untrusted mirror's virtual document path is safe to store. The path
/// is display-oriented (e.g. `trust-wills/auto/ts/deed.pdf`); reject control bytes
/// (NUL, newlines, terminal-escape injection) AND Unicode bidi/format/zero-width
/// chars (display-spoofing — see `records::is_spoofy_format_char`). Length is
/// enforced separately by `VolumeStore::put`.
fn is_safe_doc_path(path: &str) -> bool {
    !path.contains(|c: char| c.is_control() || records::is_spoofy_format_char(c))
}

/// Highest `N` for which a file named exactly `manifest.<N>.json` exists in `dir` (strict
/// `<decimal>` between the fixed prefix/suffix), or `None`. Used by `import_tree` to detect a
/// non-contiguous mirror (a missing middle partition), mirroring `VolumeStore::open`'s guard.
fn highest_mirror_manifest(dir: &Path) -> Option<u32> {
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

/// Look up a blob in the merge SOURCE's store: returns its `(validated path, size)` if
/// present, `Ok(None)` if the source lacks it, or `Err(reason)` if its stored path is
/// unsafe to display/store. The id is assumed already `is_safe_blob_id`-checked by the
/// caller. Used by `plan_merge_from` to build (and gate) the blob-copy plan.
fn source_entry_validated(source: &OpenVault, id: &str) -> Result<Option<(String, u64)>, String> {
    match source.storage.entry(id) {
        None => Ok(None),
        Some(e) => {
            if !is_safe_doc_path(&e.path) {
                return Err("references a document with an unsafe path".into());
            }
            // Enforce the SAME length bound `VolumeStore::put` enforces at apply time, so an
            // over-long source path surfaces as a skipped record in the PREVIEW instead of
            // aborting the merge with a hard error after the user already approved the plan.
            if e.path.len() > crate::storage::MAX_PATH_LEN {
                return Err("references a document whose path is too long".into());
            }
            // Likewise enforce the MAX_DOC_SIZE bound `VolumeStore::put` checks at apply time.
            // `storage::read` accepts a frame slightly larger than MAX_DOC_SIZE, so a hand-crafted
            // source volume could otherwise pass this preview and then abort apply_merge_from with
            // TooLarge AFTER the user approved the plan. Surface it as a skipped preview record.
            if e.size > crate::storage::MAX_DOC_SIZE {
                return Err("references a document that is too large".into());
            }
            Ok(Some((e.path.clone(), e.size)))
        }
    }
}

/// Reject a path that is a symlink, used to guard the INTERMEDIATE directories of an
/// untrusted import mirror (`manifest/`, `volume/`, `vol.<p>/`). `read_capped`/
/// `read_bounded` apply O_NOFOLLOW to the final component only, so without this a
/// symlinked parent directory could still redirect a blob/manifest read outside the
/// mirror. A non-existent path is fine here (the subsequent read fails on its own).
pub fn reject_symlink_dir(path: &Path) -> Result<(), VaultError> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(VaultError::Storage(StorageError::Corrupt(format!(
            "refusing to traverse a symlinked directory: {}",
            path.display()
        ))));
    }
    Ok(())
}

/// The sanitized RELATIVE on-disk path a document gets when recreating its virtual folder tree
/// under an export root. Each `/`-component of the virtual path is re-cleaned defense-in-depth
/// (drop empty / `.` / `..` / `\\` / `:` / NUL components; neutralize control+bidi spoof chars and
/// Windows reserved device names; strip trailing dots/spaces) so the result can NEVER escape the
/// root. A degenerate path (no usable component) falls back to `<id>.bin`. Shared by
/// `export_document_into` and `export_tree`'s `documents/` view. (The desktop `extract` CLI has
/// its own equivalent `safe_relative_path`.)
/// `pub` so the desktop `extract` CLI's own component sanitizer can be asserted to AGREE with
/// this one (audit 2026-07-25 round 2 found the two had silently drifted). Not part of the
/// storage contract — callers outside the exporters should not need it.
pub fn doc_tree_relpath(virtual_path: &str, id: &str) -> PathBuf {
    let mut rel = PathBuf::new();
    for part in virtual_path.split('/') {
        let p = part.trim();
        if p.is_empty() || p == "." || p == ".." || p.contains(['\\', ':', '\0']) {
            continue;
        }
        let p = records::display_safe(p.trim_end_matches(['.', ' ']));
        if p.is_empty() {
            continue;
        } else if records::is_windows_reserved_name(&p) {
            rel.push(format!("_{p}"));
        } else {
            rel.push(&p);
        }
    }
    if rel.as_os_str().is_empty() {
        rel.push(format!("{id}.bin"));
    }
    rel
}

/// Reject a pre-planted symlink anywhere on the chain of INTERMEDIATE directories from
/// `root` (exclusive — the trusted, user-chosen destination) down to `leaf` (inclusive).
/// Used before `create_dir_all` on a document EXPORT: `create_dir_all` follows a symlinked
/// component and the final O_EXCL write only guards the last name, so without this a symlink
/// seeded in a shared/reused export dir (e.g. `taxes -> ~/.ssh`) could redirect freshly
/// decrypted plaintext outside `root`. Same discipline the import side already uses.
pub fn reject_symlinked_descendants(root: &Path, leaf: &Path) -> Result<(), VaultError> {
    let Ok(rel) = leaf.strip_prefix(root) else {
        return Ok(()); // leaf not under root (shouldn't happen) — the O_EXCL write still guards the file
    };
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        reject_symlink_dir(&cur)?;
    }
    Ok(())
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
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, VaultError> {
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

/// Read a file with a hard size ceiling (unlike `fs::read`, which allocates without
/// bound). Reads at most `max + 1` bytes — one past the limit — so an over-size
/// source is detected and rejected without ever allocating more than `max + 1`.
/// Follows symlinks (the caller has already vetted the target with `fs::metadata`).
fn read_file_capped(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, VaultError> {
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

/// Options for [`OpenVault::compact`]. `volume` re-packs the document store
/// (drops dead frames); `json` trims each record's per-edit history. When
/// `drop_all_history` is false, `history_cutoff` (Unix seconds) keeps entries
/// with `at >= cutoff` and drops older ones; when true, all history is removed.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactOptions {
    pub volume: bool,
    pub json: bool,
    pub history_cutoff: Option<i64>,
    pub drop_all_history: bool,
}

/// What a compaction reclaimed. Also returned by `compact_dry_run` as a
/// pre-flight estimate (its `partitions_after` mirrors `partitions_before`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactReport {
    pub bytes_reclaimed: u64,
    pub history_removed: usize,
    pub partitions_before: usize,
    pub partitions_after: usize,
}

/// One-line summary of a compaction run, recorded in the vault `audit` log.
fn compaction_detail(opts: &CompactOptions, bytes_reclaimed: u64, history_removed: usize) -> String {
    let mode = match (opts.volume, opts.json) {
        (true, true) => "volume+history",
        (true, false) => "volume",
        (false, true) => "history",
        (false, false) => "noop",
    };
    format!("{mode}: reclaimed {bytes_reclaimed} bytes, removed {history_removed} history entries")
}

/// Doc ids referenced by any record (Trust&Will `file`, Asset `statement`, every
/// Taxes filing's and Real Estate property's `documents`, and each General
/// Document's `file`).
fn referenced_doc_ids(vault: &Vault) -> Vec<String> {
    let mut ids = Vec::new();
    // `for t in &vault.trust_wills` iterates by shared reference (doesn't consume
    // the vault's vector). `if let Some(f) = &t.file` runs the body only when the
    // optional field holds a value, binding the inner id to `f`. `.clone()` because
    // `f` is borrowed but we need an owned `String` in the result list.
    for t in &vault.trust_wills {
        if let Some(f) = &t.file {
            ids.push(f.clone());
        }
    }
    for a in &vault.assets {
        if let Some(f) = &a.statement {
            ids.push(f.clone());
        }
    }
    // Taxes tab: every document attached to a filing year is referenced, so
    // compaction (`--volume`) never reclaims a tax document.
    for t in &vault.tax_filings {
        for f in &t.documents {
            ids.push(f.clone());
        }
    }
    // Real Estate documents (deeds, policies, statements) are referenced too, so
    // compaction (`--volume`) never reclaims them.
    for re in &vault.real_estate {
        for f in &re.documents {
            ids.push(f.clone());
        }
    }
    // General Documents each reference a single attached file.
    for g in &vault.general_documents {
        if let Some(f) = &g.file {
            ids.push(f.clone());
        }
    }
    ids
}

/// Read, parse, and decrypt the vault file at `path`. Performs no writes.
fn decrypt_file(path: &Path, pw1: &[u8], pw2: &[u8]) -> Result<(Vault, Header, Key), VaultError> {
    let raw = read_capped_vault(path)?;
    decode_vault_bytes(&raw, pw1, pw2)
}

/// Read a `vault.pmv`-shaped file with the DoS size cap applied *before* the read
/// (a crafted, oversized file is rejected before allocation, not after). A missing
/// file maps to [`VaultError::NotFound`].
fn read_capped_vault(path: &Path) -> Result<Vec<u8>, VaultError> {
    use std::io::Read;
    // O_NOFOLLOW, like every other read in the vault directory. That directory is treated
    // as attacker-reachable (see `open_read_nofollow`'s callers: the recovery candidates,
    // the header probe, the lock file, and the whole storage layer), and the primary
    // `vault.pmv` was the one file still opened with a symlink-following `File::open` —
    // an inconsistency, not a considered exception. Nothing legitimate is broken by
    // closing it: every write goes through temp+rename (`write_bytes_atomic`,
    // `write_vault_file`), which REPLACES a symlink with a regular file, so a symlinked
    // vault.pmv is already destroyed by the first save.
    //
    // Open first so the cap can be enforced on the READ (a bounded `take`), not on a
    // separate stat that a concurrent grow could outrun. A missing file maps to
    // NotFound (the create flow + redundancy recovery rely on this).
    let f = match open_read_nofollow(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(VaultError::NotFound(path.to_path_buf())),
        Err(e) => return Err(e.into()),
    };
    let mut buf = Vec::new();
    f.take(MAX_VAULT_SIZE.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_VAULT_SIZE {
        return Err(VaultError::TooLarge);
    }
    Ok(buf)
}

/// Parse the header, derive the key from the two passwords, AEAD-verify+decrypt, and
/// deserialize the JSON vault. The full header (incl. nonce) is the AEAD associated
/// data, so any header tamper or bit-rot fails the tag (fail closed).
fn decode_vault_bytes(raw: &[u8], pw1: &[u8], pw2: &[u8]) -> Result<(Vault, Header, Key), VaultError> {
    let header = Header::parse(raw)?;
    let key = crypto::derive_key_chained(pw1, pw2, &header.salt, &header.params)?;
    let (vault, _) = decode_vault_with_key(raw, &key)?;
    Ok((vault, header, key))
}

/// Like [`decode_vault_bytes`] but with the key **already derived** — used by the
/// redundancy recovery path so the (expensive, memory-hard) key derivation runs
/// once even when several copies must be tried (also stops a wrong password from
/// triggering N Argon2 runs).
fn decode_vault_with_key(raw: &[u8], key: &Key) -> Result<(Vault, Header), VaultError> {
    let header = Header::parse(raw)?;
    let ciphertext = &raw[HEADER_LEN..]; // everything after the fixed-size header
    let aad = header.to_bytes();
    // Decrypt into a `Zeroizing` buffer so the plaintext JSON is wiped on drop.
    let plaintext = Zeroizing::new(crypto::decrypt(key, &header.nonce, ciphertext, &aad)?);
    let vault: Vault = serde_json::from_slice(&plaintext)?;
    // Defense-in-depth / forward-compat: the header byte is the authoritative version gate
    // (Header::parse), but the AEAD-authenticated JSON body carries its own `version` too.
    // Assert they agree so the two signals can't diverge silently — a future version-
    // conditional decode must never be fed a body whose version disagrees with the header.
    // Mirrors the equivalent check on the import path.
    if vault.version != FORMAT_VERSION {
        return Err(VaultError::BadVersion(vault.version));
    }
    Ok((vault, header))
}

/// Open `vault.pmv`, transparently falling back to the opt-in in-place redundant
/// copies (§12.8) when the live file is unreadable. Returns `Some(notice)` as the
/// 4th element when recovery happened. Order: the live file, then the
/// same-generation mirror (no data loss), then prior generations newest-first.
fn decrypt_with_redundancy(
    path: &Path,
    pw1: &[u8],
    pw2: &[u8],
) -> Result<(Vault, Header, Key, Option<String>), VaultError> {
    // Normal path — the live file reads cleanly.
    let primary_err = match decrypt_file(path, pw1, pw2) {
        Ok((v, h, k)) => return Ok((v, h, k, None)),
        Err(e) => e, // live file missing / too big / bit-rotted / wrong password
    };

    // The live file is unreadable. If no redundant copy exists, surface the original
    // error unchanged (so a wrong password still reads as "wrong password").
    let candidates = redundancy_candidates(path);
    if candidates.is_empty() {
        return Err(primary_err);
    }
    let mirror = mirror_path(path);

    // The primary's own header salt (when its fixed-size header still parses), used below to
    // block a cross-epoch rollback. See `restrict_salt` after PASS 1 for the full rationale
    // and the corroboration condition that keeps salt-bit-rot recovery working.
    let primary_salt = read_header_of(path).ok().map(|h| h.salt);

    // PASS 1 — collect up to MAX_RECOVERY_SALTS distinct candidate salts by reading
    // ONLY each candidate's fixed-size header (cheap), then derive one key per distinct
    // salt. CRITICAL: the live header is NOT a trusted key-derivation source — a
    // corruption confined to its salt/params would defeat recovery even with a perfect
    // mirror — so we derive from each *candidate* salt. All same-epoch copies share one
    // salt, so this is ~1 Argon2 in practice (an older generation adds at most one
    // more). The cap bounds an attacker who plants many distinct-salt + maxed-param
    // candidates from forcing one expensive chained derivation per salt on every open.
    const MAX_RECOVERY_SALTS: usize = 3;
    let mut keys: Vec<Key> = Vec::new();
    let mut key_salts: Vec<[u8; SALT_LEN]> = Vec::new();
    // Remember, per candidate, the index into `keys` of the key derived from THAT
    // candidate's own header salt (None if its header is unreadable or its salt was
    // dropped at the cap). PASS 2 uses this to try the right key first and avoid the
    // full candidates × keys cross-product.
    let mut cand_key: Vec<Option<usize>> = Vec::with_capacity(candidates.len());
    for c in &candidates {
        let Ok(header) = read_header_of(c) else {
            cand_key.push(None);
            continue;
        };
        if let Some(pos) = key_salts.iter().position(|s| s == &header.salt) {
            cand_key.push(Some(pos)); // key for this salt already derived
            continue;
        }
        if keys.len() >= MAX_RECOVERY_SALTS {
            cand_key.push(None); // refuse to derive past the bound (planted distinct-salt DoS guard)
            continue;
        }
        match crypto::derive_key_chained(pw1, pw2, &header.salt, &header.params) {
            Ok(key) => {
                keys.push(key);
                key_salts.push(header.salt);
                cand_key.push(Some(keys.len() - 1));
            }
            Err(_) => cand_key.push(None),
        }
    }
    if keys.is_empty() {
        return Err(primary_err); // no candidate header parsed / wrong password
    }

    // Cross-epoch rollback guard. Confine recovery to copies sharing the primary's salt —
    // BUT ONLY when that salt is CORROBORATED by at least one redundant copy. The two cases
    // this must separate:
    //   * Wrong password on an INTACT primary (F3 rollback vector): the primary's salt is the
    //     genuine current-epoch salt, and the same-epoch mirror/bak1 (rewritten at the new
    //     epoch by `refresh_redundancy_copies` after a rekey) share it — so it IS corroborated.
    //     A stranded OLD-epoch `bak` (cleanup failed on transient EIO/EACCES) has a DIFFERENT
    //     salt; without this guard, entering the OLD password would decode it and silently roll
    //     the password change back, then the heal/sweep would destroy the new-epoch copies.
    //     Restricting to the corroborated current salt makes the old password fail closed.
    //   * Bit-rot INSIDE the primary's salt bytes: the header still parses but the salt is
    //     garbage that matches NO copy, so it is NOT corroborated and we do NOT restrict — the
    //     intact mirror legitimately carries a different (correct) salt and must stay usable
    //     (regression-tested by `recovers_from_mirror_when_primary_salt_corrupt`).
    let restrict_salt = primary_salt.filter(|ps| key_salts.iter().any(|s| s == ps));

    // PASS 2 — try EACH candidate against ONLY the key derived from its OWN header salt,
    // holding at most one candidate buffer in memory at a time (an earlier version slurped
    // every candidate up front, risking OOM from planted max-size copies). Trying a
    // different-salt ("sibling") key is pointless and was removed: the salt is part of the
    // AEAD associated data (`Header::to_bytes` covers bytes 21..37), so for any candidate a
    // wrong-salt key fails the tag AND a corrupted-salt header makes its body undecryptable
    // under any key — there is no cross-salt recovery to be had. This bounds recovery to
    // EXACTLY O(candidates) full AEAD decrypts, closing a CPU-amplification DoS where a
    // vault-dir-write attacker plants many max-size distinct-salt copies.
    for (idx, c) in candidates.iter().enumerate() {
        let Ok(raw) = read_capped_vault(c) else { continue };
        let Some(k) = cand_key[idx] else { continue }; // header unreadable / salt past the cap
        // Cross-epoch guard (see `restrict_salt` above): when the primary's salt is known and
        // corroborated, never recover from a copy under a DIFFERENT salt — that would roll
        // back across a password change instead of healing a same-epoch bit-rot.
        if let Some(ps) = restrict_salt
            && key_salts[k] != ps
        {
            continue;
        }
        if let Ok((vault, hdr)) = decode_vault_with_key(&raw, &keys[k]) {
            let key = keys.swap_remove(k); // take ownership of the matching key
            // Wording is keyed on the SOURCE only as a coarse hint — NOT a generation claim.
            // After a rekey/compact, `refresh_redundancy_copies` rewrites the mirror AND bak1
            // at the CURRENT generation, so a bak is frequently the same generation as the
            // lost primary; asserting it is an "earlier generation — data lost" cried wolf
            // (audit R-12). Both notices say only that the latest change *may* be missing.
            let notice = if *c == mirror {
                "The main vault file was unreadable and was recovered from its mirror copy \
                 (normally the latest state — but if a save was interrupted before the \
                 mirror was written, the most recent change may be missing). Re-save, and \
                 refresh your off-device backups.".to_string()
            } else {
                "The main vault file and its mirror were unreadable; recovered from a \
                 redundant copy. If a recent save was interrupted, the most recent \
                 change(s) may be missing. Re-save, and refresh your off-device backups.".to_string()
            };
            return Ok((vault, hdr, key, Some(notice)));
        }
        // `raw` is dropped here before the next candidate is read (bounded memory).
    }
    // No copy decrypted under any candidate-derived key — wrong password, or every
    // copy is also corrupt. Return the live file's original error.
    Err(primary_err)
}

/// Read and parse ONLY the fixed-size header of a vault file. Used by redundancy
/// recovery to learn a candidate's salt/params without pulling the whole (possibly
/// attacker-inflated) file into memory.
fn read_header_of(path: &Path) -> Result<Header, VaultError> {
    use std::io::Read;
    // O_NOFOLLOW: recovery candidates (mirror/bakN) live in the same vault directory the
    // storage layer already treats as attacker-reachable; every other read of that dir uses
    // O_NOFOLLOW (read_bounded, append_frame, the lock). This was the lone recovery read that
    // followed a final-component symlink — close it so a planted `vault.pmv.mirror -> /etc/…`
    // can't redirect the read. (On non-unix, a plain open.)
    let mut f = open_read_nofollow(path)?;
    let mut buf = [0u8; HEADER_LEN];
    f.read_exact(&mut buf)?;
    Header::parse(&buf)
}

/// Open a file for reading WITHOUT following a final-component symlink (O_NOFOLLOW on unix;
/// plain open elsewhere). Used by the redundancy-recovery candidate reads, whose paths sit
/// in the attacker-reachable vault directory — matching the discipline in `read_bounded`
/// and `storage::append_frame`.
fn open_read_nofollow(path: &Path) -> std::io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
    }
    #[cfg(not(unix))]
    {
        fs::File::open(path)
    }
}


// --- In-place redundancy file management (§12.8) -----------------------------

/// `vault.pmv` -> `vault.pmv<suffix>` (append, not replace-extension).
fn with_suffix(primary: &Path, suffix: &str) -> PathBuf {
    let mut name = primary.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| std::ffi::OsString::from(VAULT_FILE));
    name.push(suffix);
    match primary.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// The same-generation mirror path (`vault.pmv.mirror`).
fn mirror_path(primary: &Path) -> PathBuf {
    with_suffix(primary, ".mirror")
}

/// The k-th retained prior generation (`vault.pmv.bak1` = newest prior).
fn bak_path(primary: &Path, k: u32) -> PathBuf {
    with_suffix(primary, &format!(".bak{k}"))
}

/// Write `bytes` to `dst` atomically and **symlink-safely**: a fresh O_EXCL temp
/// (0600, never follows a symlink) is written and fsync'd, then renamed over `dst`
/// — and a rename REPLACES any symlink planted at `dst` rather than following it.
/// This matches vault.pmv's own write discipline; using `fs::copy` here would follow
/// a planted symlink and redirect the (encrypted) write + chmod to an arbitrary file.
fn write_bytes_atomic(dst: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    // Fault point (crash-test only): abort/ENOSPC while writing a bak generation.
    crate::fault::point("redundancy.bak").map_err(VaultError::from)?;
    let tmp = sibling_tmp(dst)?;
    if let Err(e) = write_new_file(&tmp, bytes, &[]) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, dst).map_err(VaultError::from) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent_dir(dst);
    Ok(())
}

/// Remove stale `*.tmp` siblings left by a crash mid atomic-write — `.vault.pmv*.tmp`
/// (primary/mirror/bak temps) in the vault dir and `.manifest*.tmp` in `manifest/` —
/// AND any orphaned `.<name>.old` directory trees from a rekey. Best-effort, writable
/// opens only. The temps are encrypted (no plaintext leak), but sweeping keeps the
/// directory tidy and avoids OLD-KEY material lingering after a rekey.
fn sweep_stale_temps(dir: &Path) {
    let sweep = |d: &Path, prefix: &str| {
        if let Ok(rd) = fs::read_dir(d) {
            for entry in rd.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(prefix) && name.ends_with(".tmp") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    };
    sweep(dir, &format!(".{VAULT_FILE}")); // .vault.pmv* / .vault.pmv.mirror* / .vault.pmv.bakN*
    sweep(&dir.join("manifest"), ".manifest"); // .manifest.N* manifest-commit temps
    // `.last_update_<ts>.<rand>.tmp` temps leaked by `touch_last_update` if a crash lands between
    // its fsync and rename. The live marker (`last_update_<ts>`, no leading dot, no `.tmp`) is never
    // matched, so only orphaned temps are reaped — otherwise they would accumulate across crashes.
    sweep(dir, ".last_update_");
    // Reap orphaned `.volume.old` / `.manifest.old` trees. `replace_dir`'s OWN cleanup of
    // its `.old` sibling runs at the START of the next commit, but only on a RE-ENTRANT
    // rekey (staging still present). If the trailing best-effort `remove_dir_all` failed
    // AFTER the rekey fully committed (staging gone), recover_pending_rekey returns early on
    // later opens and replace_dir is never re-entered — so an `.old` dir full of OLD-KEY
    // ciphertext would linger forever, defeating change_password's forward secrecy. Reaping
    // it on every writable open closes that gap. The live dirs (`volume`/`manifest`) have no
    // `.old` suffix, so this can never touch them.
    for sub in ["volume", "manifest"] {
        let _ = fs::remove_dir_all(sibling_old(&dir.join(sub)));
    }
}

/// Remove every retained generation numbered above `depth` (e.g. after the depth is
/// lowered), so the on-disk generation count never exceeds the configured retention.
fn prune_generations_above(primary: &Path, depth: u32) {
    for k in (depth.min(MAX_REDUNDANCY) + 1)..=MAX_REDUNDANCY {
        let _ = fs::remove_file(bak_path(primary, k));
    }
}

/// Ring the outgoing generation (`prev_bytes` — the just-replaced `vault.pmv`) into
/// the ring: drop the oldest, shift the rest down, write `prev_bytes` as `bak1`
/// (atomic + symlink-safe), then prune any slot beyond `depth`. Called AFTER the new
/// primary has committed, so a failed save never disturbs the ring. Best-effort (a
/// partial/odd copy is skipped on recovery, since each is AEAD-validated when used).
fn rotate_generations(primary: &Path, depth: u32, prev_bytes: &[u8]) {
    let depth = depth.min(MAX_REDUNDANCY);
    if depth == 0 {
        return;
    }
    // Fault point (crash-test only): abort mid ring-rotation — AFTER the authoritative
    // primary commit — to prove the primary still opens (the ring is best-effort).
    let _ = crate::fault::point("redundancy.rotate");
    let _ = fs::remove_file(bak_path(primary, depth)); // the oldest falls off the ring
    for k in (1..depth).rev() {
        let from = bak_path(primary, k);
        if from.exists() {
            let _ = fs::rename(&from, bak_path(primary, k + 1)); // bak{k} -> bak{k+1}
        }
    }
    let _ = write_bytes_atomic(&bak_path(primary, 1), prev_bytes); // outgoing -> bak1
    prune_generations_above(primary, depth);
    // Make the whole ring shift (renames + drop + prune) durable as a unit. (The bak1
    // write already fsync'd the dir, but the prune removals after it had not been; one
    // fsync here covers them so a power loss can't resurrect a pruned generation.)
    sync_parent_dir(&bak_path(primary, 1));
}

/// Remove every redundant copy (mirror + all generations). Safe to call on every
/// non-redundant save. The fast-path no-op (the common default, when no copies
/// exist) keys on `redundancy_candidates` so it can never skip an orphaned
/// higher-numbered generation — it returns only when there is genuinely nothing to
/// remove (each `remove_file` on a non-existent path is itself a cheap ENOENT).
fn cleanup_redundancy(primary: &Path) {
    if redundancy_candidates(primary).is_empty() {
        return;
    }
    let _ = fs::remove_file(mirror_path(primary));
    for k in 1..=MAX_REDUNDANCY {
        let _ = fs::remove_file(bak_path(primary, k));
    }
}

/// Existing redundant copies in recovery-preference order: mirror (same generation,
/// no data loss) first, then prior generations newest-first.
fn redundancy_candidates(primary: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let m = mirror_path(primary);
    if m.exists() {
        out.push(m);
    }
    for k in 1..=MAX_REDUNDANCY {
        let b = bak_path(primary, k);
        if b.exists() {
            out.push(b);
        }
    }
    out
}

/// Defensively remove any redundancy copy (mirror / `bakN`) whose header salt does NOT
/// match the live primary's salt — a cross-epoch leftover from a password change whose
/// best-effort [`cleanup_redundancy`] partially failed (audit F3). The salt changes only
/// on rekey and is authenticated AAD, so an old-epoch copy was written under a PREVIOUS
/// password's salt: it cannot decode under the current password, is therefore useless for
/// recovery, and only lingers as old-key plaintext-equivalent ciphertext at rest — a
/// forward-secrecy leftover after a password change. Removing it can never drop a
/// recovery-useful copy (recovery only ever uses a copy that decodes under the current
/// password, i.e. the current salt). Same defensive posture as the stale-temp / `.old`-dir
/// sweeps; only runs on a writable open. A copy whose header is unreadable is LEFT ALONE —
/// we never delete what we cannot positively classify as foreign (a salt-damaged copy is
/// already unrecoverable and harmless).
fn sweep_foreign_epoch_copies(primary: &Path, current_salt: &[u8; SALT_LEN]) {
    for c in redundancy_candidates(primary) {
        if let Ok(h) = read_header_of(&c)
            && &h.salt != current_salt
        {
            let _ = fs::remove_file(&c);
        }
    }
}

/// Encrypt `vault` under `key` and write it atomically to `path` (new nonce, full
/// header as AAD, temp → fsync → rename → dir fsync).
/// Serialize a SECRET-bearing value (the decrypted `Vault`) to JSON in a single,
/// exactly-sized [`Zeroizing`] buffer so the plaintext (every password) is never stranded
/// in freed heap. `serde_json::to_vec`/`to_string_pretty` start from an empty `Vec` and
/// grow it by reallocation, freeing each smaller buffer WITHOUT zeroizing — leaving partial
/// cleartext JSON fragments behind on every save/export/decrypt. To avoid that we measure
/// the exact serialized length first (a counting pass that holds NO plaintext buffer), then
/// serialize once into a buffer pre-sized to exactly that length, so it never reallocates.
/// `pub` so the desktop CLI's `decrypt` can reuse the same hardened path.
pub fn serialize_secret_json<T: serde::Serialize>(value: &T, pretty: bool) -> Result<Zeroizing<Vec<u8>>, serde_json::Error> {
    // A `Write` sink that only counts bytes — no allocation, so the measuring pass can't
    // strand plaintext.
    struct CountingWriter(usize);
    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = CountingWriter(0);
    if pretty {
        serde_json::to_writer_pretty(&mut counter, value)?;
    } else {
        serde_json::to_writer(&mut counter, value)?;
    }
    // capacity == exact len => the second (real) pass never grows the Vec, so no smaller
    // buffer is ever freed unwiped. The whole buffer is zeroized on drop.
    let mut buf = Zeroizing::new(Vec::<u8>::with_capacity(counter.0));
    if pretty {
        serde_json::to_writer_pretty(&mut *buf, value)?;
    } else {
        serde_json::to_writer(&mut *buf, value)?;
    }
    Ok(buf)
}

fn write_vault_file(
    path: &Path,
    vault: &Vault,
    key: &Key,
    salt: &[u8; SALT_LEN],
    params: KdfParams,
) -> Result<(), VaultError> {
    // Serialize the vault to JSON bytes (wiped on drop, no realloc strand), pick a fresh
    // random nonce, and build the header. `*salt` dereferences the `&[u8; N]` borrow to
    // copy the array by value into the new `Header`.
    let plaintext = serialize_secret_json(vault, false)?;
    let nonce = crypto::random_bytes::<NONCE_LEN>()?;
    let header = Header { params, salt: *salt, nonce };
    let header_bytes = header.to_bytes();
    let ciphertext = crypto::encrypt_with_nonce(key, &nonce, &plaintext, &header_bytes)?;

    // Fail CLOSED if the encrypted file would exceed the read-side cap. `read_capped_vault`
    // (and every reopen through it) rejects a `vault.pmv` larger than MAX_VAULT_SIZE, but
    // the write path never checked — so a vault grown past the cap (a huge merge, or a
    // record set with very large history) would commit successfully yet be UNOPENABLE on
    // the next launch, a silent brick. Refusing the save here keeps the on-disk vault
    // (the previous, still-openable generation) intact and surfaces the error to the caller
    // instead. The file layout is exactly header ‖ ciphertext (see write_new_file), so its
    // length is known before we touch the disk.
    let file_len = (header_bytes.len() as u64).saturating_add(ciphertext.len() as u64);
    if file_len > MAX_VAULT_SIZE {
        return Err(VaultError::TooLarge);
    }

    // A *let-chain*: the block runs only if `path.parent()` is `Some(parent)` AND
    // that parent is non-empty. `parent` is in scope for the whole condition + body.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
        harden_dir(parent);
    }
    // Atomic write: stage to a temp sibling file, then rename over the target.
    // A rename is atomic on POSIX, so a reader never sees a half-written vault.
    let tmp = sibling_tmp(path)?;
    // `if let Err(e) = ...` = handle just the failure case. On error (incl. an
    // injected ENOSPC), best-effort delete the temp (`let _ =` ignores that
    // cleanup's own result) then return — the live vault.pmv is never touched.
    if let Err(e) = crate::fault::point("vault.write").map_err(VaultError::from).and_then(|()| {
        write_new_file(&tmp, &header_bytes, &ciphertext)
    }) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) =
        crate::fault::point("vault.rename").map_err(VaultError::from).and_then(|()| Ok(fs::rename(&tmp, path)?))
    {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent_dir(path); // fsync the directory so the rename is durable on disk
    Ok(())
}

// --- Password-change (rekey) staging recovery --------------------------------

/// Recover an interrupted password change found at `<dir>/.rekey`:
/// a `READY` marker means the new tree is complete → **roll forward** (commit);
/// no marker means staging was incomplete → **discard** it (the old tree stands).
/// In read-only mode we cannot write, so a pending rekey is reported.
fn recover_pending_rekey(dir: &Path, read_only: bool) -> Result<(), VaultError> {
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
fn commit_rekey(dir: &Path, staging: &Path) -> Result<(), VaultError> {
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
fn replace_dir(live: &Path, staged: &Path) -> Result<(), VaultError> {
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
fn replace_path(live: &Path, staged: &Path) -> Result<(), VaultError> {
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

fn sibling_old(path: &Path) -> PathBuf {
    // `.file_name()` -> `Option<&OsStr>`; `.and_then(|n| n.to_str())` chains another
    // optional step (the name may not be valid UTF-8, giving `None`); `.unwrap_or("x")`
    // supplies a fallback name if either step yielded `None`.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(format!(".{name}.old")),
        _ => PathBuf::from(format!(".{name}.old")),
    }
}

/// Normalize a virtual directory path to `/a/b/c` form (empty string == root).
fn normalize_dir(path: &str) -> String {
    // Iterator pipeline: split on '/', `.filter(|p| !p.is_empty())` drops empty
    // segments (so "a//b" and trailing slashes collapse), then `.collect()` gathers
    // the kept `&str` pieces into a `Vec`. The closure `|p| !p.is_empty()` is the test.
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { String::new() } else { format!("/{}", parts.join("/")) }
}

fn rand_suffix() -> Result<String, CryptoError> {
    // 8 random bytes -> `.iter()` over them -> `.map(|b| format!("{b:02x}"))` formats
    // each as a 2-digit lowercase hex string -> `.collect()` concatenates into one
    // `String` (a 16-char hex suffix). `?` propagates a failure of the RNG call.
    Ok(crypto::random_bytes::<8>()?.iter().map(|b| format!("{b:02x}")).collect())
}

fn sibling_tmp(path: &Path) -> Result<PathBuf, VaultError> {
    let suffix = rand_suffix()?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let file = format!(".{name}.{suffix}.tmp");
    Ok(match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(file),
        _ => PathBuf::from(file),
    })
}

/// Copy the whole vault directory (`vault.pmv` + `manifest/` + `volume/`) into a
/// fresh timestamped subdirectory of `dest_dir`, as a consistent set. Copies the
/// encrypted files as-is — nothing is decrypted. Returns the backup vault path.
pub fn backup(vault_path: &Path, dest_dir: &Path) -> Result<PathBuf, VaultError> {
    if !vault_path.exists() {
        return Err(VaultError::NotFound(vault_path.to_path_buf()));
    }
    let src_dir = parent_dir(vault_path);
    // CLI/standalone path: no open session holds the lock, so acquire it for the WHOLE
    // snapshot. An ALREADY-OPEN session must instead use `OpenVault::backup` — calling
    // this free function from a session that already holds the lock self-deadlocks,
    // because flock binds to the open file description and a second in-process
    // acquire returns `WouldBlock` → `Locked`. Holding the lock makes the multi-file
    // copy atomic vs. a concurrent rekey (which would otherwise pair an old-key
    // vault.pmv with a new-key store). On the mobile build (no single-writer-lock
    // feature) this is a no-op — that build serializes all access behind one mutex.
    let _lock = lock_for_read_only_copy(&src_dir)?;
    backup_snapshot(vault_path, &src_dir, dest_dir)
}

/// The lock-free body of a backup snapshot: copy `vault.pmv` + `manifest/` +
/// `volume/` into a fresh timestamped dir under `dest_dir` as a consistent set
/// (encrypted files as-is, nothing decrypted). The CALLER must already hold the
/// single-writer lock for `src_dir` — the free `backup` acquires it; an open
/// session's `OpenVault::backup` reuses (or, when read-only, acquires) its own.
fn backup_snapshot(vault_path: &Path, src_dir: &Path, dest_dir: &Path) -> Result<PathBuf, VaultError> {
    // Don't snapshot a tree mid-rekey: the volume/manifest may be the new key while
    // vault.pmv is still the old one, yielding an unopenable backup. With the lock
    // held a present `.rekey` means a *crashed* rekey; finish/discard it via --write.
    if src_dir.join(REKEY_DIR).exists() {
        return Err(VaultError::RekeyPending);
    }
    // Refuse a symlink at the SOURCE vault.pmv: `fs::copy` below FOLLOWS it, which
    // would copy an arbitrary file's bytes (whatever the link targets) into the
    // backup set — the same exfiltration F-14 closed for `copy_dir`, here for the
    // top-level file. `symlink_metadata` inspects the link itself, not its target.
    if fs::symlink_metadata(vault_path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(VaultError::Storage(StorageError::Corrupt("vault file is a symlink".to_string())));
    }
    // Refuse a symlinked destination directory: an attacker who can write the vault
    // dir could otherwise point the backup into the very tree we are reading, or at
    // arbitrary files the user can write. (A non-existent dest is fine — created below.)
    if let Ok(meta) = fs::symlink_metadata(dest_dir)
        && meta.file_type().is_symlink()
    {
        return Err(VaultError::Storage(StorageError::Corrupt("backup destination is a symlink".to_string())));
    }
    fs::create_dir_all(dest_dir)?;
    harden_dir(dest_dir);

    let stamp = compact_timestamp(records::unix_now());
    let mut target = dest_dir.join(format!("backup-{stamp}"));
    let mut n = 1;
    // Find a non-colliding name: keep appending `_n` while the path already exists.
    while target.exists() {
        target = dest_dir.join(format!("backup-{stamp}_{n}")); // reassign `target` (it's `mut`)
        n += 1;
    }
    fs::create_dir_all(&target)?;
    harden_dir(&target);

    fs::copy(vault_path, target.join(VAULT_FILE))?;
    harden_file(&target.join(VAULT_FILE))?;
    // Iterate a literal array of the two subdirectory names; `sub` binds each in turn.
    for sub in ["manifest", "volume"] {
        let s = src_dir.join(sub);
        if s.exists() {
            copy_dir(&s, &target.join(sub))?;
        }
    }
    // Belt-and-suspenders for the lock-less (mobile) build: re-check `.rekey`. With the
    // write lock held (desktop) no writer can have started a rekey during the copy, so
    // this can only fire on the lock-less build; harmless to keep on both.
    if src_dir.join(REKEY_DIR).exists() {
        let _ = fs::remove_dir_all(&target);
        return Err(VaultError::RekeyPending);
    }
    Ok(target.join(VAULT_FILE))
}

/// Recursively copy a directory tree (files hardened to 0600 on Unix).
fn copy_dir(src: &Path, dst: &Path) -> Result<(), VaultError> {
    fs::create_dir_all(dst)?;
    harden_dir(dst);
    // `read_dir` yields each entry as a `Result`; `let entry = entry?;` unwraps it
    // (propagating any I/O error), shadowing the loop variable with the unwrapped value.
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // `entry.file_type()` reflects the directory entry itself and does NOT follow
        // symlinks — unlike `Path::is_dir` and `fs::copy`, which both dereference. A
        // same-UID attacker who plants a symlink in the vault tree (e.g.
        // `volume/vol.7 -> /etc/passwd`, or a dir symlink for runaway recursion) would
        // otherwise have its target copied into the backup. Refuse symlink entries.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            return Err(VaultError::Storage(StorageError::Corrupt(format!(
                "refusing to back up a symlink in the vault tree: {}",
                from.display()
            ))));
        } else if ft.is_dir() {
            copy_dir(&from, &to)?; // recurse into real subdirectories
        } else {
            fs::copy(&from, &to)?;
            harden_file(&to)?;
        }
    }
    Ok(())
}

/// Format unix seconds as a filename-safe UTC stamp `YYYYMMDD-HHMMSS`.
fn compact_timestamp(ts: i64) -> String {
    let (year, mo, d, h, m, s) = records::civil_from_unix(ts);
    format!("{year:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

// --- Cross-platform file hardening (compile on Windows + Linux) --------------
// `pub` so the CLI binary (a separate crate over this library) can reuse them.

// `#[cfg(unix)]` is *conditional compilation*: this version of the function is
// compiled ONLY on Unix-like systems. The `#[cfg(not(unix))]` twin below is
// compiled everywhere else. Exactly one definition of `harden_file` exists per
// build, so the rest of the code can call it unconditionally.
#[cfg(unix)]
pub fn harden_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt; // trait that adds `.set_mode()` to permissions
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600); // owner read/write only — no access for group/others
    fs::set_permissions(path, perms)
}
#[cfg(not(unix))]
pub fn harden_file(_path: &Path) -> std::io::Result<()> {
    Ok(()) // no-op on non-Unix; the `_path` name marks the arg as intentionally unused
}

// Same Unix / non-Unix split as `harden_file`, but for directories (0700 =
// owner-only access). Returns nothing and ignores errors (best-effort hardening).
#[cfg(unix)]
pub fn harden_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // `if let Ok(meta) = ...` runs the body only when the metadata read succeeded.
    if let Ok(meta) = fs::metadata(dir) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700); // owner: read/write/execute; group & others: nothing
        let _ = fs::set_permissions(dir, perms); // best-effort; ignore the result
    }
}
#[cfg(not(unix))]
pub fn harden_dir(_dir: &Path) {} // no-op on non-Unix (empty body)

/// Open a brand-new file with `create_new` (O_EXCL; no symlink-follow) + 0600.
fn create_new_0600(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = OpenOptions::new();
    // `.create_new(true)` = fail if the path already exists (atomic O_EXCL). This
    // refuses to clobber an existing file and won't follow a planted symlink.
    opts.write(true).create_new(true);
    // A `#[cfg(unix)]` on a *block*: this whole `{ ... }` is compiled only on Unix.
    // There it sets the file's creation mode to 0600 (owner read/write only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt; // brings `.mode()` into scope
        opts.mode(0o600);
    }
    opts.open(path)
}

fn write_new_file(path: &Path, part1: &[u8], part2: &[u8]) -> Result<(), VaultError> {
    let mut f = create_new_0600(path)?; // `f` is mutable: writing to it changes its state
    harden_file(path)?;
    f.write_all(part1)?; // write the header bytes...
    f.write_all(part2)?; // ...then the ciphertext bytes
    f.sync_all()?; // flush to disk (fsync) before returning, for durability
    Ok(())
}

/// Create a brand-new file and write a single buffer (O_EXCL + 0600); removes the
/// partial file on a write error. Shared by `export_document` and the CLI.
/// Return `p` if it does not exist, else a sibling with a `_N` suffix, so an export
/// never silently overwrites an existing file (mirrors the CLI extract's behaviour).
fn unique_export_path(p: PathBuf, fallback_token: Option<&str>) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let parent = p.parent().map(PathBuf::from).unwrap_or_default();
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 1..10_000 {
        let cand = parent.join(format!("{stem}_{n}{ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    // Range exhausted: >10000 files already share this name. If the caller supplied a
    // guaranteed-unique token (a document id), disambiguate with it so an O_EXCL create
    // can't EEXIST and abort the whole export (audit L2) — the human documents/ tree is
    // cosmetic, but its failure used to propagate via `?` and strand the already-written
    // plaintext. Without a token, fall back to the colliding path and let the write
    // surface the collision as an error (the prior behavior for the other callers).
    match fallback_token {
        Some(t) => {
            let cand = parent.join(format!("{stem}_{t}{ext}"));
            if cand.exists() { p } else { cand }
        }
        None => p,
    }
}

/// Write `data` to `<dir>/<filename>`, creating `dir` (and parents) if missing, NEVER
/// overwriting an existing file (a `_N` suffix is appended, like document export), with
/// 0600 perms, an fsync of the file contents, AND an fsync of the parent directory so the
/// new file's directory entry is crash-durable. Returns the path actually written. Backs
/// the front-ends' "Export to CSV" action, which drops a timestamped CSV into the export dir.
pub fn write_export_bytes(dir: &Path, filename: &str, data: &[u8]) -> Result<PathBuf, VaultError> {
    // Refuse a symlinked export dir: the CSV carries every password in cleartext, and
    // create_dir_all + harden_dir both follow a symlink, so without this a pre-planted
    // symlink at `dir` would redirect the plaintext CSV outside the chosen directory and
    // chmod the target 0700 (audit R4-1, same root guard as export_tree / backup).
    reject_symlink_dir(dir)?;
    fs::create_dir_all(dir)?;
    harden_dir(dir); // best-effort 0700 on the export dir (no-op off unix)
    let path = unique_export_path(dir.join(filename), None);
    write_new_bytes(&path, data)?;
    // Make the freshly-created file's directory entry durable too — the contents are
    // fsync'd in write_new_bytes, but without this the link can be lost on power loss
    // right after the call returns. Best-effort, no-op off unix (matches the other writers).
    sync_parent_dir(&path);
    Ok(path)
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

pub fn write_new_bytes(path: &Path, data: &[u8]) -> Result<(), VaultError> {
    let mut f = create_new_0600(path)?;
    // Harden perms, then write + fsync, as one fail-cleanup unit: if hardening OR the
    // write OR the fsync fails, close the handle and unlink the just-created file so a
    // failure never leaves a partial (or empty) file behind — the no-clobber / "partial
    // file removed on error" contract the CSV and document exporters rely on.
    let res = harden_file(path).and_then(|()| f.write_all(data)).and_then(|()| f.sync_all());
    if let Err(e) = res {
        drop(f); // close the handle before unlinking (matters on some platforms)
        let _ = fs::remove_file(path);
        return Err(e.into());
    }
    Ok(())
}

// fsync the *directory* so a rename/create is durable (a crash can't lose it).
// Only meaningful on Unix; the non-Unix twin is a no-op.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    // `.filter(...)` keeps the parent only if non-empty; `.unwrap_or_else(closure)`
    // computes the fallback `"."` lazily (the closure runs only when needed).
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all(); // best-effort directory fsync
    }
}
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

/// Refresh the vault directory's single `last_update_<UTC>` marker after a committed change.
///
/// A glanceable CONVENIENCE hint — its NAME *and* contents are the commit time
/// (`YYYYMMDD-HHMMSS` UTC) — so an external backup/sync can notice "the vault changed" without
/// decrypting it. Called ONLY after the vault content is durably committed ([`OpenVault::
/// save_internal`], [`commit_rekey`]), NEVER before: a failed/aborted save leaves it untouched.
/// It is NOT written for the desktop `prefs.json` (a separate, non-vault file).
///
/// AUTHORITATIVE source of truth is `vault.pmv`'s own mtime, which the filesystem updates
/// atomically with the temp+rename commit (zero gap). This marker can lag the real commit by one
/// in the tiny crash window *between* the vault commit and this write, so a sync tool needing a
/// HARD guarantee should key off `vault.pmv`'s mtime and treat this only as a fast hint.
///
/// Written atomically (unique temp → fsync → rename → dir fsync) so a concurrent reader never
/// sees a half-written/empty marker and a crash can't leave it partial. Entirely BEST-EFFORT:
/// the vault is already durably committed by the time this runs, so any failure here just leaves
/// a slightly stale/missing hint and must never fail the operation. Ignored by every dir scan
/// (the partition/manifest scanners match strict `vol.<N>`/`manifest.<N>` inside the `volume/`
/// and `manifest/` SUBDIRS, not the vault root where this lives).
fn touch_last_update(dir: &Path) {
    let ts = records::compact_utc(records::unix_now());
    let name = format!("last_update_{ts}");
    let marker = dir.join(&name);
    // Write the NEW marker first (atomic temp → fsync → rename → dir fsync). Writing before
    // removing the old one means there is never a window with NO marker; putting the timestamp
    // in the NAME means a same-second re-save reuses the same path (the rename just refreshes it)
    // rather than self-deleting in the cleanup below.
    let content = format!("{ts}\n");
    let Ok(tmp) = sibling_tmp(&marker) else { return };
    if write_new_file(&tmp, content.as_bytes(), &[]).is_err() {
        let _ = fs::remove_file(&tmp);
        return; // leave the previous marker in place rather than risk a gap
    }
    if fs::rename(&tmp, &marker).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    sync_parent_dir(&marker);
    // Remove any OTHER (older-named) `last_update_*` so exactly one remains. Skipping the file we
    // just wrote keeps a same-second re-save (identical name) from deleting itself.
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name();
            let ns = n.to_string_lossy();
            if ns.starts_with("last_update_") && ns != name {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// Fuzzing entry point (hidden). The vault-file header parser; see `fuzz/`.
// `mod fuzz { ... }` declares an inner module (a namespace). `#[doc(hidden)]`
// keeps it out of generated docs. It just exposes the header parser so a fuzzer
// can feed it arbitrary bytes; `super::` means "the parent module" (this file).
#[doc(hidden)]
pub mod fuzz {
    pub fn header(buf: &[u8]) {
        let _ = super::Header::parse(buf); // discard result; we only care that it doesn't crash
    }
}

// The test suite for this module lives in `vault_tests.rs` (~4.8k lines), pulled in
// here by the `#[path]` attribute. `#[cfg(test)]` compiles it ONLY when running
// `cargo test`, so it is never part of the shipped binary.
//
// It stays an INNER module rather than moving to `tests/` because it exercises private
// items — `Header`, `decode_vault_with_key`, `write_vault_file` — that an integration
// test crate could not name without making them `pub` purely to be testable.
#[cfg(test)]
#[path = "vault_tests.rs"]
mod tests;
