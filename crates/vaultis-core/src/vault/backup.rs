//! Backups: a consistent snapshot of the vault directory, taken under the write lock.

use super::*;

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
pub(super) fn lock_for_read_only_copy(dir: &Path) -> Result<Option<WriteLock>, VaultError> {
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

impl OpenVault {
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
pub(super) fn backup_snapshot(vault_path: &Path, src_dir: &Path, dest_dir: &Path) -> Result<PathBuf, VaultError> {
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
    copy_vault_tree(vault_path, src_dir, &target)?;
    Ok(target.join(VAULT_FILE))
}

/// Copy one vault's files (`vault.pmv` + `manifest/` + `volume/`) into the fresh
/// directory `target`, encrypted as-is, refusing symlinks and a password change that is
/// in flight. Shared by [`backup_snapshot`] and the pre-upgrade safety copy
/// ([`super::upgrade`]); the caller holds (or deliberately skips) the single-writer lock.
pub(super) fn copy_vault_tree(vault_path: &Path, src_dir: &Path, target: &Path) -> Result<(), VaultError> {
    fs::create_dir_all(target)?;
    harden_dir(target);

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
        let _ = fs::remove_dir_all(target);
        return Err(VaultError::RekeyPending);
    }
    Ok(())
}

/// Recursively copy a directory tree (files hardened to 0600 on Unix).
pub(super) fn copy_dir(src: &Path, dst: &Path) -> Result<(), VaultError> {
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
pub(super) fn compact_timestamp(ts: i64) -> String {
    let (year, mo, d, h, m, s) = records::civil_from_unix(ts);
    format!("{year:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}
