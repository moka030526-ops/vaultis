//! Low-level file helpers: owner-only permissions (cross-platform), create-new writes,
//! collision-free export names, temp-file naming, and parent-directory fsync.

use super::*;

pub(super) fn rand_suffix() -> Result<String, CryptoError> {
    // 8 random bytes -> `.iter()` over them -> `.map(|b| format!("{b:02x}"))` formats
    // each as a 2-digit lowercase hex string -> `.collect()` concatenates into one
    // `String` (a 16-char hex suffix). `?` propagates a failure of the RNG call.
    Ok(crypto::random_bytes::<8>()?.iter().map(|b| format!("{b:02x}")).collect())
}

pub(super) fn sibling_tmp(path: &Path) -> Result<PathBuf, VaultError> {
    let suffix = rand_suffix()?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let file = format!(".{name}.{suffix}.tmp");
    Ok(match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(file),
        _ => PathBuf::from(file),
    })
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
pub(super) fn create_new_0600(path: &Path) -> std::io::Result<std::fs::File> {
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

pub(super) fn write_new_file(path: &Path, part1: &[u8], part2: &[u8]) -> Result<(), VaultError> {
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
pub(super) fn unique_export_path(p: PathBuf, fallback_token: Option<&str>) -> PathBuf {
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
pub(super) fn sync_parent_dir(path: &Path) {
    // `.filter(...)` keeps the parent only if non-empty; `.unwrap_or_else(closure)`
    // computes the fallback `"."` lazily (the closure runs only when needed).
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all(); // best-effort directory fsync
    }
}

#[cfg(not(unix))]
pub(super) fn sync_parent_dir(_path: &Path) {}
