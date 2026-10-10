//! Reading, decrypting and writing the encrypted vault file (`vault.pmv`) itself:
//! bounded no-follow reads, header-authenticated decryption (falling back through the
//! redundancy copies), secret-safe JSON serialization, and the atomic write.

use super::*;

/// Read, parse, and decrypt the vault file at `path`. Performs no writes.
pub(super) fn decrypt_file(path: &Path, pw1: &[u8], pw2: &[u8]) -> Result<(Vault, Header, Key), VaultError> {
    let raw = read_capped_vault(path)?;
    decode_vault_bytes(&raw, pw1, pw2)
}

/// Read a `vault.pmv`-shaped file with the DoS size cap applied *before* the read
/// (a crafted, oversized file is rejected before allocation, not after). A missing
/// file maps to [`VaultError::NotFound`].
pub(super) fn read_capped_vault(path: &Path) -> Result<Vec<u8>, VaultError> {
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
pub(super) fn decode_vault_bytes(raw: &[u8], pw1: &[u8], pw2: &[u8]) -> Result<(Vault, Header, Key), VaultError> {
    let header = Header::parse(raw)?;
    let key = crypto::derive_key_chained(pw1, pw2, &header.salt, &header.params)?;
    let (vault, _) = decode_vault_with_key(raw, &key)?;
    Ok((vault, header, key))
}

/// Like [`decode_vault_bytes`] but with the key **already derived** — used by the
/// redundancy recovery path so the (expensive, memory-hard) key derivation runs
/// once even when several copies must be tried (also stops a wrong password from
/// triggering N Argon2 runs).
pub(super) fn decode_vault_with_key(raw: &[u8], key: &Key) -> Result<(Vault, Header), VaultError> {
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
pub(super) fn decrypt_with_redundancy(
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
pub(super) fn read_header_of(path: &Path) -> Result<Header, VaultError> {
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
pub(super) fn open_read_nofollow(path: &Path) -> std::io::Result<fs::File> {
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

/// Encrypt `vault` under `key` and write it atomically to `path` (new nonce, full
/// header as AAD, temp → fsync → rename → dir fsync).
pub(super) fn write_vault_file(
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
pub(super) fn touch_last_update(dir: &Path) {
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
