//! Path rules for documents: virtual paths inside the vault, the safety checks applied to
//! blob ids and document paths from untrusted input, and symlink refusal on the way out.

use super::*;

/// The directory containing the vault file (its parent, or "." if none).
pub(super) fn parent_dir(vault_file: &Path) -> PathBuf {
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

/// True if `id` is a safe single path component to use as a blob filename when
/// reading an (untrusted) import mirror: non-empty, no path separators, no NUL,
/// and not a `.`/`..` traversal. Real ids are random hex, so this never rejects a
/// genuine export — it only stops a crafted mirror from escaping its directory.
pub(super) fn is_safe_blob_id(id: &str) -> bool {
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
pub(super) fn is_safe_doc_path(path: &str) -> bool {
    !path.contains(|c: char| c.is_control() || records::is_spoofy_format_char(c))
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

/// Normalize a virtual directory path to `/a/b/c` form (empty string == root).
pub(super) fn normalize_dir(path: &str) -> String {
    // Iterator pipeline: split on '/', `.filter(|p| !p.is_empty())` drops empty
    // segments (so "a//b" and trailing slashes collapse), then `.collect()` gathers
    // the kept `&str` pieces into a `Vec`. The closure `|p| !p.is_empty()` is the test.
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { String::new() } else { format!("/{}", parts.join("/")) }
}
