//! Crash-safe filesystem helpers: bounded no-follow reads, partition discovery, atomic
//! writes, directory fsync, and owner-only permissions.

use super::*;

// --- Crash-safe filesystem helpers ------------------------------------------

/// Read at most `max + 1` bytes from `path` WITHOUT following a final-component symlink,
/// erroring `TooLarge` if the file holds more than `max`. Unlike `fs::read` (which both
/// follows symlinks and allocates without bound), this caps the allocation on the READ and
/// refuses a symlinked file at the open, so a file swapped for a symlink to `/dev/zero`
/// (stat size 0) can neither be followed nor drive an OOM. Mirrors `vault::read_bounded`
/// and `append_frame`'s O_NOFOLLOW discipline. (On non-unix the caller's `symlink_metadata`
/// pre-check remains the guard.)
pub(super) fn read_file_bounded_nofollow(path: &Path, max: u64) -> Result<Vec<u8>, StorageError> {
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
pub(super) fn highest_partition_index(dir: &Path, prefix: &str) -> Option<u32> {
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
pub(super) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), StorageError> {
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
pub(super) fn sync_dir(dir: &Path) {
    // Open the directory and fsync it (commits the directory entry itself). Errors are
    // ignored (`if let Ok` / `let _`) — best-effort durability.
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) {} // no-op; `_dir` underscore-prefix marks it intentionally unused

/// Tighten an ALREADY-OPEN file to 0600 via its descriptor (fchmod), with no path
/// re-resolution — so it cannot be tricked into chmod-ing a symlink target. Best-effort.
#[cfg(unix)]
pub(super) fn harden_file_fd(f: &File) {
    use std::os::unix::fs::PermissionsExt; // brings `from_mode` into scope
    let _ = f.set_permissions(fs::Permissions::from_mode(0o600)); // owner read/write only
}

#[cfg(not(unix))]
pub(super) fn harden_file_fd(_f: &File) {} // no-op off unix (permission bits don't apply the same way)

#[cfg(unix)]
pub(super) fn harden_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(dir) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700); // owner-only directory access
        let _ = fs::set_permissions(dir, perms);
    }
}

#[cfg(not(unix))]
pub(super) fn harden_dir(_dir: &Path) {} // no-op off unix
