//! The per-root `prefs.json` file: bounded, symlink-refusing reads, atomic writes, and the
//! typed load/save helpers for each preference the GUI keeps.

// --- Local, non-secret preferences (used by the GUI) -------------------------
//
// UI preferences live in ONE optional file, `prefs.json`, in the **vault root** — the
// folder that holds your vault folders, not the encrypted vault itself. Nothing is ever
// written to an OS config directory: the app leaves no trace outside the folder you point
// it at, so a vault root on a USB stick carries its own look with it and a machine that
// has only ever *viewed* a vault is left exactly as it was found.
//
// The file is OPTIONAL and is created only when a setting is actually changed. Absent (or
// corrupt, or over-size — see `read_prefs_obj`) simply means the built-in defaults:
// Catppuccin Mocha, 100% interface size, the default proportional typeface, ungrouped
// lists.
//
// It holds exactly five keys — `theme`, `ui_scale`, `font`, `group_assets_default`,
// `group_accounts_default` — all purely cosmetic. That limit is the security boundary,
// not an oversight, because `prefs.json` sits OUTSIDE the encryption as an ordinary file
// next to the vault folders: anyone who can write to the vault media WITHOUT knowing the
// two passwords can edit it. Two settings are therefore deliberately not persisted
// anywhere, so that write access can never become plaintext theft:
//
//   * `export_dir` — where the GUI writes CLEARTEXT exports: the per-tab CSV (every
//     account and portal password in the clear) and every decrypted document. Persisting
//     it here would let a tampered vault root silently redirect those secrets to a
//     cloud-synced folder, a Windows UNC share, or back into the vault folder where the
//     next backup sweeps them up. It is set per session, in Config.
//   * `reveal_all_default` — opens every password tab UNMASKED. A tampered file flipping
//     it on would turn off the shoulder-surf/screen-share protection unasked. Reveal is
//     now a per-session toggle that always starts OFF.
//
// The start page's vault root is the one exception to "nothing outside the vault root": it
// can't be recorded inside the folder it names, so it is remembered in a single plain-text
// file in the per-user OS data directory instead (`launch::save_last_root`/`load_last_root`) —
// nothing else lives there, and it holds nothing but that one path. Precedence at startup is
// the command line (`vaultis-gui DIR`) > that remembered root > empty. The working directory
// is deliberately NOT consulted — it used to be, and the sample vault shipping beside the
// executables turned it into a trap (see `launch::initial_root_and_name`). The vault NAME
// within the root is never pre-selected — the user always picks it.
//
// Every write is a read-modify-write so one key never clobbers another.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::*;

/// Hard cap on the prefs file size. It holds one short JSON object, so a larger file
/// is corrupt or hostile; bounding the read before allocating means a huge or
/// symlinked `prefs.json` can never stall or OOM the UI at startup.
pub(crate) const MAX_PREFS_SIZE: u64 = 64 * 1024;

/// The complete set of keys `prefs.json` may carry (deny by default).
///
/// Enforced on READ, so a hand-edited or tampered file cannot smuggle in a key the app
/// would otherwise honour — notably `export_dir` and `reveal_all_default`, which were
/// removed from the persisted set precisely because this file is writable by anyone with
/// access to the vault media but not the passwords. See the module comment above.
pub(crate) const PREFS_KEYS: &[&str] =
    &["theme", "ui_scale", "font", "group_assets_default", "group_accounts_default"];

/// Bounded, symlink-safe read of the prefs JSON object (empty map on any failure), so
/// a setter can read-modify-write without clobbering other keys.
///
/// The `symlink_metadata` pre-check is a cheap early reject (it inspects the link itself,
/// so a symlinked prefs file fails `is_file()`), but it is NOT the security boundary: it is
/// a separate syscall from the read, and `std::fs::read` both FOLLOWS a symlink and
/// allocates without bound. The read below therefore opens with `O_NOFOLLOW` and takes at
/// most `MAX_PREFS_SIZE + 1` bytes, so a file swapped for a symlink to `/dev/zero` after the
/// stat — or one that simply grows between the two calls — can neither be followed nor drive
/// an unbounded allocation at UI startup. This mirrors `vaultis_core::vault::read_bounded`
/// and `storage::read_file_bounded_nofollow`; it matters here because the vault-root
/// fallback reads this file from the (untrusted) vault media.
pub(crate) fn read_prefs_obj(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_PREFS_SIZE => {}
        _ => return serde_json::Map::new(),
    }
    let Ok(bytes) = read_bounded_nofollow(path, MAX_PREFS_SIZE) else { return serde_json::Map::new() };
    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes).unwrap_or_default()
}

/// Read at most `max + 1` bytes from `path` without following a final-component symlink,
/// erroring once the file is known to exceed `max`. The `+ 1` detects an over-size file
/// without ever allocating past the ceiling. (On non-unix the caller's `symlink_metadata`
/// pre-check remains the only symlink guard, exactly as in the core crate.)
///
/// `pub(crate)` because it is the single hardened reader for **every** small local file this
/// app reads outside the vault itself — `prefs.json` here and `last_root.txt` in
/// [`crate::launch`]. Keeping one definition is the point: `last_root.txt` originally shipped
/// with a raw `std::fs::read_to_string` while its writer used the hardened `write_atomic`,
/// which is the guard asymmetry the 2026-07-29 audit (L-1) found.
pub(crate) fn read_bounded_nofollow(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    #[cfg(unix)]
    let f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?
    };
    #[cfg(not(unix))]
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file exceeds the size cap"));
    }
    Ok(buf)
}

/// Best-effort atomic write of `bytes` to `path`, through a fresh `O_EXCL` 0600 temp
/// sibling, then renamed over the target. A failure at either step (temp creation or
/// rename) is silently ignored, leaving whatever was previously at `path` untouched, and
/// the temp is cleaned up.
///
/// `std::fs::write` opens the path directly, so it FOLLOWS a symlink planted at `path` and
/// truncates-then-writes in place — leaving a window where a concurrent reader sees a
/// half-written file, and a crash leaves it truncated. A rename REPLACES a symlink rather
/// than writing through it, and matches the temp→rename discipline every other writer in
/// this project uses. Shared by every small, non-critical, best-effort local file this app
/// writes outside the vault itself (`prefs.json`, `launch::save_last_root`'s pointer file).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A unique hidden temp beside the target, so two windows (or two callers) saving at
    // once never collide on the same temp name.
    let Ok(suffix) = crypto::random_bytes::<8>() else { return };
    let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(d) => d.join(format!(".{name}.{suffix}.tmp")),
        None => PathBuf::from(format!(".{name}.{suffix}.tmp")),
    };
    if vault::write_new_bytes(&tmp, bytes).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Best-effort write of the prefs object (a write failure is ignored — prefs are
/// non-critical and trivially re-picked). See [`write_atomic`] for the write discipline.
pub(crate) fn write_prefs_obj(path: &Path, obj: &serde_json::Map<String, serde_json::Value>) {
    let Ok(bytes) = serde_json::to_vec_pretty(obj) else { return };
    write_atomic(path, &bytes);
}

/// Path to `<vault_root>/prefs.json`, or `None` when no vault root is known yet (the start
/// page before a root is typed or picked). This is the ONLY preferences file the app reads
/// or writes — see the module comment above.
pub(crate) fn prefs_path(vault_root: &str) -> Option<PathBuf> {
    // Normalized like every other directory the UI takes: trimmed, with a pasted
    // "Copy as path" quote pair stripped, so a quoted root still finds its prefs.json.
    let root = records::unquote_path(vault_root);
    (!root.is_empty()).then(|| Path::new(root).join("prefs.json"))
}

/// The effective prefs object for a vault root: `<vault_root>/prefs.json`, filtered to
/// [`PREFS_KEYS`].
///
/// The filter is applied on READ and is the security boundary: `prefs.json` is an ordinary
/// unencrypted file beside the vault folders, so anyone with write access to the media —
/// but not the two passwords — authors it. Restricting it to cosmetic keys means that
/// access can change how the app LOOKS and nothing else. A missing, corrupt or over-size
/// file contributes nothing (see [`read_prefs_obj`]), leaving the built-in defaults.
pub(crate) fn effective_prefs_obj(vault_root: &str) -> serde_json::Map<String, serde_json::Value> {
    prefs_path(vault_root).map(|p| effective_prefs_obj_from(&p)).unwrap_or_default()
}

/// Path-parametrized core of [`effective_prefs_obj`], so the key filter is testable
/// against an arbitrary file.
pub(crate) fn effective_prefs_obj_from(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = read_prefs_obj(path);
    obj.retain(|k, _| PREFS_KEYS.contains(&k.as_str()));
    obj
}

/// The export-destination directory for THIS SESSION ("" until the user sets one).
///
/// Deliberately **not persisted anywhere**. This is where the GUI writes CLEARTEXT
/// exports — the per-tab CSV (every account and portal password in the clear) and every
/// decrypted document. The only file the app writes is `<vault_root>/prefs.json`, which
/// sits unencrypted beside the vault folders and is therefore authored by anyone who can
/// write to the media without knowing the two passwords; storing this key there would let
/// tampering silently redirect those secrets to a synced folder or a UNC share. Asking
/// once per session is the price of that guarantee.
///
/// Kept as a function (rather than inlining `String::new()` at the call sites) so the
/// "starts unset every session" rule has one documented home.
pub(crate) fn load_export_dir(_vault_root: &str) -> String {
    String::new()
}

// --- View defaults: cosmetic, persisted in `<vault_root>/prefs.json` ---------
//
// The GUI reads these at startup to seed per-tab view state. They only choose
// grouped-vs-flat list rendering, which carries no security meaning, so they are safe to
// carry in a file that travels with the vault media.
//
// The LOADERS run for real under test: prefs resolve against the vault root, which the suite
// points at a temp directory, so reading is both hermetic and exercised.
//
// The SAVERS still short-circuit under `cfg(test)`. The test helpers put each vault directly
// in `std::env::temp_dir()`, which makes the shared temp dir every test's vault ROOT — so one
// test persisting a view default would write a `prefs.json` that every other test then reads,
// silently flipping their lists to grouped and changing row counts. The write path stays
// fully covered by the path-parametrized `_to`/`_from` round-trip tests.

/// "Reveal all passwords by default" is **not** a persisted setting.
///
/// Reveal is a per-session toggle that always starts OFF. Persisting it would mean storing
/// it in `<vault_root>/prefs.json`, where anyone able to write to the vault media — without
/// the passwords — could flip it on and defeat the app's shoulder-surf/screen-share
/// protection unasked. Returning a constant keeps every caller unchanged.
pub(crate) fn load_reveal_all_default(_vault_root: &str) -> bool {
    false
}

/// "Group assets by default" — when set, the Assets & Liabilities view opens grouped.
pub(crate) fn load_group_assets_default(vault_root: &str) -> bool {
    effective_prefs_obj(vault_root).get("group_assets_default").and_then(|v| v.as_bool()).unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn load_group_assets_default_from(path: &Path) -> bool {
    effective_prefs_obj_from(path).get("group_assets_default").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Persist the "group assets by default" flag, preserving any other prefs keys.
pub(crate) fn save_group_assets_default(vault_root: &str, on: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path(vault_root) {
        save_group_assets_default_to(&path, on);
    }
}

pub(crate) fn save_group_assets_default_to(path: &Path, on: bool) {
    let mut obj = read_prefs_obj(path);
    obj.insert("group_assets_default".into(), serde_json::Value::Bool(on));
    write_prefs_obj(path, &obj);
}

/// "Group accounts by default" — when set, the Accounts view opens grouped.
pub(crate) fn load_group_accounts_default(vault_root: &str) -> bool {
    effective_prefs_obj(vault_root).get("group_accounts_default").and_then(|v| v.as_bool()).unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn load_group_accounts_default_from(path: &Path) -> bool {
    effective_prefs_obj_from(path).get("group_accounts_default").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Persist the "group accounts by default" flag, preserving any other prefs keys.
pub(crate) fn save_group_accounts_default(vault_root: &str, on: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path(vault_root) {
        save_group_accounts_default_to(&path, on);
    }
}

pub(crate) fn save_group_accounts_default_to(path: &Path, on: bool) {
    let mut obj = read_prefs_obj(path);
    obj.insert("group_accounts_default".into(), serde_json::Value::Bool(on));
    write_prefs_obj(path, &obj);
}
