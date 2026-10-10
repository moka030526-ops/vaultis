//! Validation of the user-chosen export directory, and the "is this destination inside
//! the vault?" check that keeps plaintext exports out of the vault folder.

use std::path::{Path, PathBuf};

use super::*;

// --- Cleartext-export destination guard (shared by the CLI, GUI and TUI) ------
//
// Writing a decrypted document — or a per-tab CSV, which carries every account and
// portal password in the clear — INTO the encrypted vault directory strands plaintext
// next to `vault.pmv`, where the user's next backup or folder sync sweeps it up. The CLI
// has refused that since the `extract`/`export-tree`/`compact --backup-dest` guards; the
// windowed and terminal front-ends export to the Config directory and so need the same
// check. It lives here (not in the `vaultis` binary) so all three front-ends share ONE
// definition and cannot drift.

/// Validate the Config-screen export directory for a session whose vault file is
/// `vault_path`, returning the normalized directory to write into or the message the
/// front-end should show.
///
/// Two refusals, in the order the user hits them:
/// 1. unset — the front-ends have no per-export path prompt, so there is nothing to write to;
/// 2. inside the vault directory — a per-tab CSV holds every account and portal password in
///    the clear, and a document export is the decrypted file, so landing either next to
///    `vault.pmv` means the user's next backup or folder sync of the vault carries the
///    plaintext with it. `dest_inside` resolves both sides through the filesystem, so a
///    symlinked export directory pointing back into the vault is caught too.
///
/// Shared by the GUI and TUI so the rule and its wording cannot drift between them; the
/// CLI's `extract`/`export-tree`/`compact --backup-dest` enforce the same thing directly.
pub fn checked_export_dir(vault_path: &Path, configured: &str) -> Result<PathBuf, String> {
    let dir = records::unquote_path(configured);
    if dir.is_empty() {
        return Err("Set an export directory in Config first (Config > Export directory).".to_string());
    }
    let dir = PathBuf::from(dir);
    if let Some(vault_dir) = vault_path.parent().filter(|p| !p.as_os_str().is_empty())
        && dest_inside(vault_dir, &dir)
    {
        return Err(format!(
            "Export directory must be OUTSIDE the vault folder ({}) — exports are UNENCRYPTED \
             and would be swept into your next backup of the vault. Pick another folder in Config.",
            vault_dir.display()
        ));
    }
    Ok(dir)
}

/// Whether `dest` is the vault directory itself or a path inside it (a backup
/// there would be copied into the very tree being rewritten). Best-effort: uses
/// canonical paths when both exist, else a lexical prefix check.
pub fn dest_inside(vault_dir: &Path, dest: &Path) -> bool {
    // Both sides are resolved AS FAR AS THE FILESYSTEM ALLOWS (see `resolve_existing`),
    // never by text alone.
    //
    // This used to canonicalize both and fall back to a purely LEXICAL comparison when
    // either failed. A destination almost never exists yet — it is a fresh export or
    // backup directory — so the lexical path was the normal one, and it folds `..` and
    // absolutizes without ever touching the filesystem. A destination reached through a
    // SYMLINKED PARENT therefore compared as "outside" while physically resolving inside
    // the vault directory, and `extract`/`export-tree` would write a full cleartext
    // mirror (vault.json holds every password) right next to vault.pmv — exactly what
    // this guard exists to prevent, since the user's next backup of the vault folder
    // sweeps the plaintext up with it.
    let v = resolve_existing(vault_dir);
    let d = resolve_existing(dest);
    d == v || d.starts_with(&v)
}

/// Resolve `path` as far as it exists: canonicalize the deepest ancestor that is really
/// there (following symlinks, folding `..` truthfully) and re-append the components below
/// it, folding any `.`/`..` left in that non-existent tail lexically.
///
/// Canonicalizing the whole path is not an option — the interesting paths here are ones
/// that have not been created yet — and comparing them purely as text is what let a
/// symlinked parent slip past. This gives the filesystem the final say over every
/// component that exists, which is every component that can carry a symlink.
pub fn resolve_existing(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
    };
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.clone();
    loop {
        if let Ok(real) = std::fs::canonicalize(&cur) {
            let mut out = real;
            // `suffix` was collected from the leaf upward, so replay it in reverse.
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return lexical_normalize(&out);
        }
        // Not there (yet): peel one component and try the parent.
        match cur.file_name() {
            Some(name) => suffix.push(name.to_os_string()),
            // No file name (root, or a trailing `..`): nothing left to peel.
            None => return lexical_normalize(&abs),
        }
        if !cur.pop() {
            return lexical_normalize(&abs);
        }
    }
}

/// Absolutize `path` against the current directory and fold away `.` and `..`
/// components purely lexically (no filesystem access, so it works for paths that do
/// not exist yet). Used by [`dest_inside`] when the destination cannot be canonicalized.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    for comp in path.components() {
        match comp {
            Component::CurDir => {}                       // drop "."
            Component::ParentDir => { out.pop(); }        // resolve ".." lexically
            Component::RootDir | Component::Prefix(_) => out.push(comp.as_os_str()),
            Component::Normal(c) => out.push(c),
        }
    }
    out
}
