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

// The vault is split by responsibility. Each submodule may add its own `impl OpenVault`
// block (a type's methods may be spread over several modules of the crate). Their free
// items are glob re-exported here: public items stay reachable at `vault::<name>` exactly
// as before, while crate-internal helpers keep their narrower visibility.
mod backup; // consistent snapshot backups
mod categories; // editable category lists stored in the vault
mod compact; // compaction + document reachability
mod documents; // attached documents (delegated to the partitioned store)
mod fsutil; // permissions, create-new writes, export names, fsync
mod merge_from; // "update this vault from another vault"
mod paths; // virtual paths + untrusted-path safety checks
mod redundancy; // mirror + .bak generations of the vault file
mod rekey; // password change: staged re-encryption + crash recovery
mod tree; // whole-vault export / import
mod upgrade; // safety copy before a new release first writes a vault
mod vault_file; // reading, decrypting and writing vault.pmv

pub use backup::*;
pub use compact::*;
pub use fsutil::*;
pub use paths::*;
pub use upgrade::*;
pub use vault_file::*;
use redundancy::*;
use rekey::*;

// THROWAWAY: one-shot owner-first / ts-in-filename document-path migration + history
// deletion + compaction. Delete this line and `src/vault/migrate.rs` to remove it.
pub mod migrate;

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
    /// A writable open of a vault last written by a NEWER vaultis than this one. Writing
    /// it with older code could drop or mangle what that release stored, so only a
    /// read-only open is allowed (see `vault/upgrade.rs`).
    #[error(
        "this vault was last saved by vaultis {written_by}, which is newer than this app \
         ({}); open it read-only, or install vaultis {written_by} or later to edit it",
        upgrade::APP_VERSION
    )]
    NewerVault { written_by: String },
    /// The pre-upgrade safety copy could not be made or did not verify, so the vault was
    /// NOT opened for writing (see `vault/upgrade.rs`).
    #[error(
        "could not save a safety copy of this vault before this version's first change to it \
         ({0}); it was not opened for editing. Free some disk space or check the folder's \
         permissions, or open it read-only"
    )]
    SafetyCopyFailed(String),
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
    /// The pre-upgrade safety copy this open took (its `vault.pmv`), when the vault had
    /// last been written by an older release; `None` otherwise (see `vault/upgrade.rs`).
    safety_copy: Option<PathBuf>,
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
        vault.written_by = Some(APP_VERSION.to_string());
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
            safety_copy: None,
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
        // Upgrade safety (see `vault/upgrade.rs`): the release that last WROTE the vault is
        // only known once it decrypts, and the open-time save below writes straight
        // after — so this is the one point where a vault from an older release can still be
        // copied aside untouched, and one from a newer release refused for writing. Under
        // the lock, before the document store opens. A read-only open writes nothing and
        // needs neither. Then stamp this release, which every save from here writes out.
        let safety_copy = if read_only {
            None
        } else {
            let copy = upgrade::before_first_write(&path, &dir, vault.written_by.as_deref())?;
            vault.written_by = Some(APP_VERSION.to_string());
            copy
        };
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
            safety_copy,
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
