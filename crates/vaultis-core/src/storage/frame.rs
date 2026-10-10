//! The on-disk frame format and its AAD: encoding, length, reading one frame at an
//! offset, parsing a decrypted frame, and appending a frame to a volume.

use super::*;

// --- Frame & AAD helpers -----------------------------------------------------

// Builds the AEAD "associated data": prefix ‖ vault_id ‖ partition. AAD is
// authenticated-but-not-encrypted context, so a ciphertext only verifies under the
// exact same vault_id+partition it was written with (binding it in place).
pub(super) fn manifest_aad(vault_id: &str, part: u32) -> Vec<u8> {
    let mut a = MANIFEST_AAD_PREFIX.to_vec(); // copy the static prefix into an owned Vec
    a.extend_from_slice(vault_id.as_bytes()); // `.as_bytes()` views the &str as &[u8]
    a.extend_from_slice(&part.to_le_bytes()); // u32 -> 4 little-endian bytes
    a // return the built vec
}

pub(super) fn volume_aad(vault_id: &str, part: u32) -> Vec<u8> {
    let mut a = VOLUME_AAD_PREFIX.to_vec();
    a.extend_from_slice(vault_id.as_bytes());
    a.extend_from_slice(&part.to_le_bytes());
    a
}

/// Build a complete on-disk frame: `[u32 frame_len][nonce][ciphertext]`.
pub(super) fn encode_frame(key: &Key, vault_id: &str, part: u32, id: &str, path: &str, bytes: &[u8]) -> Result<Vec<u8>, StorageError> {
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
pub(super) fn frame_total_len<R: Read + Seek>(f: &mut R, offset: u64) -> Result<u64, StorageError> {
    f.seek(SeekFrom::Start(offset))?; // move the read cursor to `offset`
    let mut lb = [0u8; 4]; // a fixed 4-byte stack buffer for the length prefix
    f.read_exact(&mut lb)?; // fill it exactly (errors if fewer than 4 bytes remain)
    Ok(FRAME_PREFIX_LEN + u32::from_le_bytes(lb) as u64) // 4-byte prefix + the frame body
}

/// Read and decrypt the frame at `offset` within a reader of length `file_len`.
/// If `expected_len` is non-zero it is a sanity check against the manifest.
/// Returns `(id, path, doc_bytes)`. Every read is bounds-checked so a corrupt
/// length can't over-read or over-allocate.
pub(super) fn read_frame_at<R: Read + Seek>(
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
pub(super) fn parse_plaintext(plain: &[u8]) -> Result<(String, String, Zeroizing<Vec<u8>>), StorageError> {
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

/// Append `frame` to the volume at `start`, truncating any torn tail beyond it,
/// then fsync. Opens read/write (create if absent).
pub(super) fn append_frame(path: &Path, start: u64, frame: &[u8]) -> Result<(), StorageError> {
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
