//! vaultis (desktop) — the command-line, terminal (ratatui) and graphical
//! (egui) front-ends for the offline, two-password encrypted **estate vault**.
//!
//! All of the vault logic — data model, file format, crypto, and the
//! [`vault::OpenVault`] API — lives in the headless [`vaultis_core`] crate.
//! This crate is the desktop *shell* on top of it: the two binaries
//! (`vaultis`, the console build, and `vaultis-gui`, the Windows
//! GUI-subsystem build) plus the interchangeable [`gui`] and [`ui`] front-ends.
//!
//! The core modules are re-exported here so the binaries' `vaultis::<mod>`
//! import paths and the front-ends' in-crate `crate::<mod>` paths keep
//! resolving unchanged after the workspace split.
//!
//! (`//!` is an inner doc comment for the whole crate; `///` documents the item
//! that follows; `//` is an ordinary comment.)
#![forbid(unsafe_code)]

// Re-export the headless core so existing `vaultis::crypto`, `vaultis::vault`,
// `crate::records`, … paths in the binaries and front-ends resolve unchanged.
pub use vaultis_core::{crypto, csv, fault, merge, password, records, storage, types, vault};

#[cfg(feature = "gui")]
pub mod gui; // graphical front-end (drives the same vault API as `ui`); behind `gui`
#[cfg(feature = "gui")]
pub mod gui_help; // the GUI's built-in manual (content + the help browser); behind `gui`
pub mod launch; // vault-path/flag resolution shared by the console + windowed binaries
#[cfg(feature = "gui")]
pub mod single_instance; // GUI single-instance guard (raises the egui window); behind `gui`
pub mod ui; // text/terminal front-end (interchangeable with `gui`)

/// Copy a SECRET (a password) into the OS clipboard, flagging it so clipboard
/// managers don't retain it. On Linux, arboard's `exclude_from_history` sets the
/// `x-kde-passwordManagerHint` (honoured over X11 — including XWayland — by
/// klipper/GPaste/clipman), so the password isn't logged into a manager's
/// persistent history; the GUI/TUI 15 s + on-exit clears only overwrite the live
/// selection, not such a log. On other platforms this is a plain set. Shared by the
/// GUI and TUI so both copy paths get the hint. (A clipboard manager that ignores
/// the hint, or a native-Wayland-only setup, may still retain history.)
///
/// Behind the `clipboard` feature: on Linux arboard dynamically loads X11/Wayland, so a
/// fully-static (musl) terminal build omits it (the TUI's copy then becomes a no-op).
#[cfg(feature = "clipboard")]
pub(crate) fn copy_secret_to_clipboard(text: &str) -> Result<(), arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    #[cfg(target_os = "linux")]
    {
        use arboard::SetExtLinux;
        cb.set().exclude_from_history().text(text.to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        cb.set_text(text.to_owned())
    }
}

/// Copy a NON-secret (a URL or username) into the OS clipboard. Unlike
/// [`copy_secret_to_clipboard`] this is a plain `set_text` on every platform: no
/// `exclude_from_history` hint (a non-secret belongs in normal clipboard history so
/// a clipboard manager can keep it) and the caller schedules NO auto-clear timer.
/// Kept separate from the secret path so the two security contracts never blur: a
/// password is always history-excluded + auto-cleared, a URL/username never is.
///
/// Gated on `gui` (which implies `clipboard`), not `clipboard`, because only the egui
/// GUI has the URL/username copy buttons — the TUI copies via Ctrl+Y, which targets
/// the password (secret) field. Gating on `clipboard` alone would make this dead code
/// in a `clipboard`-without-`gui` build (e.g. the minimal TUI with OS-copy added back).
#[cfg(feature = "gui")]
pub(crate) fn copy_plain_to_clipboard(text: &str) -> Result<(), arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_owned())
}

/// Pure decision for the clipboard auto-clear "tick", shared by the TUI (`ui.rs`) and
/// GUI (`gui.rs`) so both obey the SAME security-relevant contract. Given the pending
/// wipe `deadline` (if any), the current time `now`, and the current `status` line:
///   * `None` — nothing scheduled, or the deadline has not been reached: do nothing.
///   * `Some(None)` — wipe the clipboard now, but LEAVE the status untouched (it shows a
///     message the user may not have seen yet, e.g. `"Save failed: …"`).
///   * `Some(Some(s))` — wipe the clipboard now and set the status to `s`.
///
/// Kept side-effect-free (no clipboard or egui access) so the two rules a password
/// manager must not get wrong — fire only at/after the deadline, and never clobber an
/// unseen status, only a blank or a prior `"Copied …"` notice — are unit-testable.
pub(crate) fn clipboard_tick_decision(
    deadline: Option<std::time::Instant>,
    now: std::time::Instant,
    status: &str,
) -> Option<Option<String>> {
    match deadline {
        Some(t) if now >= t => {
            if status.is_empty() || status.starts_with("Copied") {
                Some(Some("Clipboard cleared.".to_string()))
            } else {
                Some(None) // keep the existing (possibly unseen) status
            }
        }
        _ => None,
    }
}

/// Format a Summary-tab amount as a grouped, whole-unit currency string, shared by the GUI
/// and TUI so both render identically: `1_234_567.8 -> "$1,234,568"`, `-2500.0 -> "-$2,500"`,
/// `0.0 -> "$0"`. The summary is an approximation, so cents are rounded away for legibility.
pub(crate) fn fmt_money(v: f64) -> String {
    // A non-finite total can reach here even though `parse_approx_value` rejects a non-finite
    // FIELD: the Summary sums many finite values, and two near-`f64::MAX` entries add to +inf.
    // `f64 as u64` saturates, so inf would render as the literal `$18,446,744,073,709,551,615`
    // and NaN as `$0` — a made-up figure presented as a real one in a financial view. Say
    // "out of range" instead.
    if !v.is_finite() {
        return "$—".to_string();
    }
    let rounded = v.abs().round() as u64;
    // Take the sign from the ROUNDED magnitude, not the raw float, so a value that rounds to 0
    // never shows "-$0". A tiny negative residue is realistic on the Summary's Net column —
    // assets minus an equal liability leaves e.g. -1e-17 from f64 subtraction.
    let neg = v < 0.0 && rounded != 0;
    let digits = rounded.to_string();
    let bytes = digits.as_bytes();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*b as char);
    }
    if neg {
        format!("-${grouped}")
    } else {
        format!("${grouped}")
    }
}


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


// --- Local, non-secret preferences (shared by the GUI and TUI) ---------------
//
// A tiny `prefs.json` in the OS config dir holds UI preferences that are NOT vault
// content — the GUI color theme and the document **export directory**. Keeping the
// export directory here (rather than in the encrypted vault) is deliberate: it is a
// local-machine preference, so it can be changed even in a READ-ONLY session, which
// is what lets a read-only user (an heir) set where to extract documents. Both
// front-ends read/write the same file through these helpers, and every write is a
// read-modify-write so the theme and export-dir keys never clobber each other.
//
// Vault-root fallback: when a key is absent from the config-dir file, the loaders fall
// back to a `prefs.json` sitting in the open vault's root folder. This lets a
// self-contained / portable vault (see `docs/SELF_CONTAINED_BACKUP.md`) carry its own UI
// defaults so they travel with the vault to a new machine. The fallback is READ-only:
// the config-dir file always wins when it sets a key, and every `save_*` keeps writing
// to the config dir, so a local choice is never silently overridden by the vault.
//
// The fallback file is UNTRUSTED INPUT — it arrives with the vault media (a USB stick, a
// restored archive, a vault handed to an heir), so whoever produced the vault chooses its
// contents. It is therefore restricted to an ALLOWLIST of purely cosmetic keys
// ([`VAULT_FALLBACK_KEYS`]); every security-relevant key is read from the config dir
// alone. See that constant for which keys are excluded and why.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Hard cap on the prefs file size. It holds one short JSON object, so a larger file
/// is corrupt or hostile; bounding the read before allocating means a huge or
/// symlinked `prefs.json` can never stall or OOM the UI at startup.
pub(crate) const MAX_PREFS_SIZE: u64 = 64 * 1024;

/// The ONLY keys a vault-root `prefs.json` may contribute (deny by default).
///
/// That file travels with the vault media, so its author is whoever produced the vault —
/// not necessarily the person opening it. Two of the keys it used to be able to set are
/// security decisions, and are deliberately absent here:
///
/// * `export_dir` — the destination the front-ends write **cleartext** exports to: the
///   per-tab CSV (which carries every account and portal password in the clear) and every
///   decrypted document. A foreign vault that set it could silently redirect those secrets
///   to a world-readable folder, a cloud-synced folder, a Windows UNC share, or back into
///   the vault directory itself (where the user's next backup sweeps them up). The user
///   picks this in Config; a vault must not pick it for them.
/// * `reveal_all_default` — opens every password tab UNMASKED. A foreign vault flipping it
///   on turns off the app's shoulder-surf/screen-share protection without the user asking.
///
/// The remaining view defaults only choose grouped-vs-flat list rendering, which carries no
/// security meaning, so a portable vault may still carry them. (`theme` is not listed
/// because it is read from the config dir directly, never through this fallback.)
pub(crate) const VAULT_FALLBACK_KEYS: &[&str] = &["group_assets_default", "group_accounts_default"];

/// Standard preferences path (`<config dir>/vaultis/prefs.json`), or `None` if the
/// platform has no config dir.
pub(crate) fn prefs_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "vaultis", "vaultis").map(|d| d.config_dir().join("prefs.json"))
}

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
fn read_bounded_nofollow(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
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
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "prefs file exceeds the size cap"));
    }
    Ok(buf)
}

/// Best-effort write of the prefs object (a write failure is ignored — prefs are
/// non-critical and trivially re-picked).
///
/// Written atomically through a fresh `O_EXCL` 0600 temp sibling, then renamed over the
/// target. `std::fs::write` (the previous implementation) opens the path directly, so it
/// FOLLOWS a symlink planted at `prefs.json` and truncates-then-writes in place — leaving a
/// window where a concurrent front-end reads a half-written file, and a crash leaves it
/// truncated. A rename REPLACES a symlink rather than writing through it, and matches the
/// temp→rename discipline every other writer in this project uses.
pub(crate) fn write_prefs_obj(path: &Path, obj: &serde_json::Map<String, serde_json::Value>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(bytes) = serde_json::to_vec_pretty(obj) else { return };
    // A unique hidden temp beside the target, so two front-ends saving at once never
    // collide on the same temp name.
    let Ok(suffix) = crypto::random_bytes::<8>() else { return };
    let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("prefs.json");
    let tmp = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(d) => d.join(format!(".{name}.{suffix}.tmp")),
        None => PathBuf::from(format!(".{name}.{suffix}.tmp")),
    };
    if vault::write_new_bytes(&tmp, &bytes).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Path to the vault-local `prefs.json` (`<vault_root>/prefs.json`), or `None` when no
/// vault root is known yet (e.g. on the start page before a root is chosen). This file is
/// a read-only fallback that lets a portable vault carry its own UI defaults.
pub(crate) fn vault_prefs_path(vault_root: &str) -> Option<PathBuf> {
    // Normalized like every other directory the UI takes: trimmed, with a pasted
    // "Copy as path" quote pair stripped, so a quoted root still finds its prefs.json.
    let root = records::unquote_path(vault_root);
    (!root.is_empty()).then(|| Path::new(root).join("prefs.json"))
}

/// Overlay `config` onto `fallback`: every key present in `config` wins, while keys found
/// only in `fallback` survive. Split out from `effective_prefs_obj` so the precedence rule
/// (config-dir beats vault-root) is unit-tested without touching the real config dir.
pub(crate) fn merge_prefs(
    config: serde_json::Map<String, serde_json::Value>,
    fallback: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = fallback;
    for (k, v) in config {
        obj.insert(k, v);
    }
    obj
}

/// The effective prefs object for the given vault root: the config-dir `prefs.json`
/// overlaid (key-by-key) onto the vault-root `prefs.json` fallback. A missing/corrupt file
/// on either side contributes nothing (see `read_prefs_obj`), so the worst case is an empty
/// map and today's built-in defaults.
pub(crate) fn effective_prefs_obj(vault_root: &str) -> serde_json::Map<String, serde_json::Value> {
    effective_prefs_obj_from(prefs_path().as_deref(), vault_root)
}
/// Path-parametrized core of `effective_prefs_obj` (testable without the real config dir).
///
/// The vault-root side is filtered down to [`VAULT_FALLBACK_KEYS`] BEFORE the merge, so a
/// `prefs.json` carried by untrusted vault media can only influence cosmetic rendering — it
/// can never choose where cleartext exports are written, nor unmask passwords by default.
pub(crate) fn effective_prefs_obj_from(
    config_path: Option<&Path>,
    vault_root: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let config = config_path.map(read_prefs_obj).unwrap_or_default();
    let mut fallback = vault_prefs_path(vault_root).map(|p| read_prefs_obj(&p)).unwrap_or_default();
    fallback.retain(|k, _| VAULT_FALLBACK_KEYS.contains(&k.as_str()));
    merge_prefs(config, fallback)
}

/// The saved export-destination directory ("" if unset).
///
/// Read from the config dir ONLY — `export_dir` is excluded from [`VAULT_FALLBACK_KEYS`],
/// so a `prefs.json` shipped with a foreign vault cannot choose where this machine writes
/// cleartext CSVs (every password) and decrypted documents. Unset simply means the user is
/// asked to set it in Config before the first export.
pub(crate) fn load_export_dir(vault_root: &str) -> String {
    effective_prefs_obj(vault_root).get("export_dir").and_then(|v| v.as_str()).unwrap_or("").to_string()
}
/// Single-path read (no fallback) — retained for the prefs round-trip tests.
#[cfg(test)]
pub(crate) fn load_export_dir_from(path: &Path) -> String {
    read_prefs_obj(path).get("export_dir").and_then(|v| v.as_str()).unwrap_or("").to_string()
}
/// Persist the export-destination directory, preserving any other prefs keys (theme).
pub(crate) fn save_export_dir(dir: &str) {
    if let Some(path) = prefs_path() {
        save_export_dir_to(&path, dir);
    }
}
pub(crate) fn save_export_dir_to(path: &Path, dir: &str) {
    let mut obj = read_prefs_obj(path);
    obj.insert("export_dir".into(), serde_json::Value::String(dir.to_string()));
    write_prefs_obj(path, &obj);
}

/// The saved **vault root** — the folder the start page scans for vaults ("" if unset).
/// Like the export dir, this is a local-machine UI preference (NOT vault content), so it
/// persists across sessions and is settable even in a read-only session.
pub(crate) fn load_vault_root() -> String {
    prefs_path().map(|p| load_vault_root_from(&p)).unwrap_or_default()
}
pub(crate) fn load_vault_root_from(path: &Path) -> String {
    read_prefs_obj(path).get("vault_root").and_then(|v| v.as_str()).unwrap_or("").to_string()
}
/// Persist the vault root, preserving any other prefs keys (theme, export dir).
pub(crate) fn save_vault_root(dir: &str) {
    // This is reached from the unlock/create flow, which the front-end unit tests drive
    // directly; skip the real-prefs write under `cfg(test)` so the suite never clobbers the
    // developer's `~/.config/vaultis/prefs.json`. The write logic itself stays covered via
    // the path-parametrized `save_vault_root_to` (see the `vault_root_round_trips` test).
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path() {
        save_vault_root_to(&path, dir);
    }
}
pub(crate) fn save_vault_root_to(path: &Path, dir: &str) {
    let mut obj = read_prefs_obj(path);
    obj.insert("vault_root".into(), serde_json::Value::String(dir.to_string()));
    write_prefs_obj(path, &obj);
}

/// The **last opened vault** — the name (the folder leaf under the vault root) of the most
/// recently unlocked/created vault ("" if none yet). Saved on a successful open/create and
/// read at startup so the start page pre-selects it in the vault dropdown. Like `vault_root`
/// this is a bootstrap key (it decides which vault among the root to highlight, read before
/// any vault is open), so it lives in the config dir alone — no vault-root fallback.
pub(crate) fn load_last_vault() -> String {
    prefs_path().map(|p| load_last_vault_from(&p)).unwrap_or_default()
}
pub(crate) fn load_last_vault_from(path: &Path) -> String {
    read_prefs_obj(path).get("last_vault").and_then(|v| v.as_str()).unwrap_or("").to_string()
}
/// Persist the last opened vault name, preserving any other prefs keys. Skipped under
/// `cfg(test)` (reached from the unlock/create flow the unit tests drive) so the suite never
/// clobbers the developer's real prefs; the write logic stays covered via `save_last_vault_to`.
pub(crate) fn save_last_vault(name: &str) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path() {
        save_last_vault_to(&path, name);
    }
}
pub(crate) fn save_last_vault_to(path: &Path, name: &str) {
    let mut obj = read_prefs_obj(path);
    obj.insert("last_vault".into(), serde_json::Value::String(name.to_string()));
    write_prefs_obj(path, &obj);
}

// --- View defaults: three local UI preferences (NOT vault content) -----------
//
// Each is a bool persisted in the shared `prefs.json` and read at startup by BOTH
// front-ends to seed per-tab view state (so a freshly opened vault honours the
// user's saved defaults). Like every other UI preference here they are settable
// even in a read-only session and follow the export-dir read-modify-write template
// (`as_bool()` / `Value::Bool`, defaulting to `false` = today's behaviour when
// unset). The public `load_*`/`save_*` wrappers short-circuit under `cfg(test)`:
// the loaders are invoked from `App::new`/`GuiApp::new` (which the unit tests
// construct), so a stable `false` keeps those tests hermetic and independent of the
// developer's real `~/.config/vaultis/prefs.json`; the path-parametrized
// `_from`/`_to` helpers stay fully exercised by the round-trip tests.

/// "Reveal all passwords by default" — when set, every tab that has passwords opens
/// with its reveal-all toggle ON instead of masked.
///
/// Read from the config dir ONLY (excluded from [`VAULT_FALLBACK_KEYS`]): unmasking every
/// password on screen is the user's decision, not one a vault handed to them may make.
pub(crate) fn load_reveal_all_default(vault_root: &str) -> bool {
    if cfg!(test) {
        return false;
    }
    effective_prefs_obj(vault_root).get("reveal_all_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
#[cfg(test)]
pub(crate) fn load_reveal_all_default_from(path: &Path) -> bool {
    read_prefs_obj(path).get("reveal_all_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
/// Persist the "reveal all by default" flag, preserving any other prefs keys.
pub(crate) fn save_reveal_all_default(on: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path() {
        save_reveal_all_default_to(&path, on);
    }
}
pub(crate) fn save_reveal_all_default_to(path: &Path, on: bool) {
    let mut obj = read_prefs_obj(path);
    obj.insert("reveal_all_default".into(), serde_json::Value::Bool(on));
    write_prefs_obj(path, &obj);
}

/// "Group assets by default" — when set, the Assets & Liabilities view opens grouped.
pub(crate) fn load_group_assets_default(vault_root: &str) -> bool {
    if cfg!(test) {
        return false;
    }
    effective_prefs_obj(vault_root).get("group_assets_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
#[cfg(test)]
pub(crate) fn load_group_assets_default_from(path: &Path) -> bool {
    read_prefs_obj(path).get("group_assets_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
/// Persist the "group assets by default" flag, preserving any other prefs keys.
pub(crate) fn save_group_assets_default(on: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path() {
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
    if cfg!(test) {
        return false;
    }
    effective_prefs_obj(vault_root).get("group_accounts_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
#[cfg(test)]
pub(crate) fn load_group_accounts_default_from(path: &Path) -> bool {
    read_prefs_obj(path).get("group_accounts_default").and_then(|v| v.as_bool()).unwrap_or(false)
}
/// Persist the "group accounts by default" flag, preserving any other prefs keys.
pub(crate) fn save_group_accounts_default(on: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = prefs_path() {
        save_group_accounts_default_to(&path, on);
    }
}
pub(crate) fn save_group_accounts_default_to(path: &Path, on: bool) {
    let mut obj = read_prefs_obj(path);
    obj.insert("group_accounts_default".into(), serde_json::Value::Bool(on));
    write_prefs_obj(path, &obj);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hermetic prefs round-trip via the path-parametrized helpers (never touches the
    // real `~/.config` prefs). Uses a nanosecond-tagged temp dir for isolation.
    fn tmp_prefs_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("pmprefs-export-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fmt_money_formats_and_never_shows_negative_zero() {
        assert_eq!(fmt_money(0.0), "$0");
        assert_eq!(fmt_money(1_234_567.8), "$1,234,568");
        assert_eq!(fmt_money(-2500.0), "-$2,500");
        // Anything that rounds to zero — including a tiny NEGATIVE f64 residue (realistic on the
        // Summary Net column when assets exactly equal liabilities) — renders "$0", never "-$0".
        assert_eq!(fmt_money(-1e-16), "$0");
        assert_eq!(fmt_money(-0.4), "$0");
        assert_eq!(fmt_money(0.4), "$0");
        assert_eq!(fmt_money(-0.6), "-$1"); // rounds to a nonzero magnitude -> keeps its sign
    }

    #[test]
    fn export_dir_round_trips_and_defaults_empty() {
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        // Absent file -> empty string (unset).
        assert_eq!(load_export_dir_from(&p), "");
        // Save then load round-trips.
        save_export_dir_to(&p, "/srv/exports");
        assert_eq!(load_export_dir_from(&p), "/srv/exports");
        // Re-saving overwrites the value.
        save_export_dir_to(&p, "/other");
        assert_eq!(load_export_dir_from(&p), "/other");
        // Clearing it (empty string) is preserved as empty.
        save_export_dir_to(&p, "");
        assert_eq!(load_export_dir_from(&p), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_dir_save_preserves_other_prefs_keys() {
        // The read-modify-write must not clobber a co-resident key (e.g. the GUI theme).
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        std::fs::write(&p, br#"{"theme":"solarized"}"#).unwrap();
        save_export_dir_to(&p, "/exports");
        let obj = read_prefs_obj(&p);
        assert_eq!(obj.get("theme").and_then(|v| v.as_str()), Some("solarized"), "theme key preserved");
        assert_eq!(obj.get("export_dir").and_then(|v| v.as_str()), Some("/exports"), "export_dir written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_default_bool_prefs_round_trip_and_default_false() {
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        // Absent file -> false (today's behaviour) for each flag.
        assert!(!load_reveal_all_default_from(&p));
        assert!(!load_group_assets_default_from(&p));
        assert!(!load_group_accounts_default_from(&p));
        // Save true, then load round-trips for each.
        save_reveal_all_default_to(&p, true);
        save_group_assets_default_to(&p, true);
        save_group_accounts_default_to(&p, true);
        assert!(load_reveal_all_default_from(&p));
        assert!(load_group_assets_default_from(&p));
        assert!(load_group_accounts_default_from(&p));
        // Toggling back to false is preserved as false.
        save_reveal_all_default_to(&p, false);
        save_group_assets_default_to(&p, false);
        save_group_accounts_default_to(&p, false);
        assert!(!load_reveal_all_default_from(&p));
        assert!(!load_group_assets_default_from(&p));
        assert!(!load_group_accounts_default_from(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_default_bool_prefs_coexist_with_other_keys() {
        // Each flag's read-modify-write must not clobber co-resident keys (theme,
        // export dir, vault root, or the sibling bool flags).
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        std::fs::write(&p, br#"{"theme":"solarized"}"#).unwrap();
        save_export_dir_to(&p, "/exports");
        save_vault_root_to(&p, "/vaults");
        save_reveal_all_default_to(&p, true);
        save_group_assets_default_to(&p, true);
        save_group_accounts_default_to(&p, true);
        let obj = read_prefs_obj(&p);
        assert_eq!(obj.get("theme").and_then(|v| v.as_str()), Some("solarized"), "theme preserved");
        assert_eq!(obj.get("export_dir").and_then(|v| v.as_str()), Some("/exports"), "export_dir preserved");
        assert_eq!(obj.get("vault_root").and_then(|v| v.as_str()), Some("/vaults"), "vault_root preserved");
        assert_eq!(obj.get("reveal_all_default").and_then(|v| v.as_bool()), Some(true), "reveal_all_default written");
        assert_eq!(obj.get("group_assets_default").and_then(|v| v.as_bool()), Some(true), "group_assets_default written");
        assert_eq!(obj.get("group_accounts_default").and_then(|v| v.as_bool()), Some(true), "group_accounts_default written");
        // A non-bool value for a flag key falls back to false rather than panicking.
        std::fs::write(&p, br#"{"reveal_all_default":"yes"}"#).unwrap();
        assert!(!load_reveal_all_default_from(&p), "non-bool value falls back to false");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_root_round_trips_and_coexists_with_other_keys() {
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        // Absent -> empty (unset), so the front-end falls back to its launch default.
        assert_eq!(load_vault_root_from(&p), "");
        // Round-trips, and the read-modify-write preserves a co-resident export dir.
        save_export_dir_to(&p, "/exports");
        save_vault_root_to(&p, "/vaults");
        assert_eq!(load_vault_root_from(&p), "/vaults");
        assert_eq!(load_export_dir_from(&p), "/exports", "export_dir preserved");
        save_vault_root_to(&p, "/other-vaults");
        assert_eq!(load_vault_root_from(&p), "/other-vaults");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_vault_round_trips_and_coexists_with_other_keys() {
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        // Absent -> empty (no vault remembered yet), so the start page falls back to its
        // launch/derived default.
        assert_eq!(load_last_vault_from(&p), "");
        // Round-trips, and the read-modify-write preserves a co-resident root + export dir.
        save_vault_root_to(&p, "/vaults");
        save_export_dir_to(&p, "/exports");
        save_last_vault_to(&p, "personal");
        assert_eq!(load_last_vault_from(&p), "personal");
        assert_eq!(load_vault_root_from(&p), "/vaults", "vault_root preserved");
        assert_eq!(load_export_dir_from(&p), "/exports", "export_dir preserved");
        // Re-saving overwrites with the newly opened vault.
        save_last_vault_to(&p, "work");
        assert_eq!(load_last_vault_from(&p), "work");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_prefs_obj_is_bounded_and_symlink_safe() {
        let dir = tmp_prefs_dir();
        let p = dir.join("prefs.json");
        // Over-cap file is rejected before the body is parsed (DoS guard) -> empty map.
        std::fs::write(&p, vec![b'{'; (MAX_PREFS_SIZE as usize) + 1]).unwrap();
        assert!(read_prefs_obj(&p).is_empty(), "over-cap prefs rejected");
        // A symlinked prefs file is refused even if its target is valid.
        #[cfg(unix)]
        {
            let real = dir.join("real.json");
            std::fs::write(&real, br#"{"export_dir":"/x"}"#).unwrap();
            let link = dir.join("link.json");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(read_prefs_obj(&link).is_empty(), "symlinked prefs refused");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_prefs_config_wins_key_by_key() {
        // Config-dir keys override the vault-root fallback; fallback-only keys survive.
        let mut config = serde_json::Map::new();
        config.insert("export_dir".into(), serde_json::Value::String("/config".into()));
        let mut fallback = serde_json::Map::new();
        fallback.insert("export_dir".into(), serde_json::Value::String("/vault".into()));
        fallback.insert("reveal_all_default".into(), serde_json::Value::Bool(true));
        let merged = merge_prefs(config, fallback);
        assert_eq!(merged.get("export_dir").and_then(|v| v.as_str()), Some("/config"), "config wins on shared key");
        assert_eq!(merged.get("reveal_all_default").and_then(|v| v.as_bool()), Some(true), "fallback-only key survives");
    }

    #[test]
    fn vault_root_prefs_json_is_a_read_fallback_for_cosmetic_keys_only() {
        // A `prefs.json` in the vault root seeds COSMETIC keys the config-dir file leaves
        // unset, and never overrides a key the config-dir file does set.
        let dir = tmp_prefs_dir();
        let config = dir.join("config-prefs.json");
        let vault_root = dir.join("vault");
        std::fs::create_dir_all(&vault_root).unwrap();
        let vault_prefs = vault_root.join("prefs.json");
        let vroot = vault_root.to_str().unwrap();

        // Vault carries its own defaults; config dir is absent entirely.
        std::fs::write(&vault_prefs, br#"{"group_assets_default":true}"#).unwrap();
        let eff = effective_prefs_obj_from(Some(&config), vroot);
        assert_eq!(
            eff.get("group_assets_default").and_then(|v| v.as_bool()),
            Some(true),
            "vault seeds a cosmetic view flag"
        );

        // Now the config dir sets the same key: it wins.
        std::fs::write(&config, br#"{"group_assets_default":false}"#).unwrap();
        let eff = effective_prefs_obj_from(Some(&config), vroot);
        assert_eq!(
            eff.get("group_assets_default").and_then(|v| v.as_bool()),
            Some(false),
            "config-dir value wins over the vault's"
        );

        // An empty vault root disables the fallback (start page, before a root is chosen).
        assert!(vault_prefs_path("").is_none(), "no vault-root path without a root");
        assert!(vault_prefs_path("   ").is_none(), "whitespace-only root is treated as unset");
        let eff = effective_prefs_obj_from(None, "");
        assert!(eff.is_empty(), "no fallback contribution without a vault root");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_vaults_prefs_cannot_choose_where_cleartext_exports_go() {
        // THE security property behind `VAULT_FALLBACK_KEYS`. The vault-root `prefs.json`
        // ships with the vault media (a USB stick, a restored archive, a vault handed to an
        // heir), so its author is whoever produced the vault. It must not be able to set:
        //   * `export_dir`        — where the front-ends write the per-tab CSV (every
        //                           account and portal password in the CLEAR) and every
        //                           decrypted document. A foreign value could redirect
        //                           those secrets to a world-readable folder, a synced
        //                           folder, a Windows UNC share, or the vault dir itself.
        //   * `reveal_all_default` — opens every password tab UNMASKED.
        // Both must stay unset here even though the config dir says nothing about them.
        let dir = tmp_prefs_dir();
        let config = dir.join("config-prefs.json"); // deliberately absent: nothing to override
        let vault_root = dir.join("vault");
        std::fs::create_dir_all(&vault_root).unwrap();
        std::fs::write(
            vault_root.join("prefs.json"),
            br#"{"export_dir":"/tmp/attacker-drop","reveal_all_default":true,
                 "vault_root":"/tmp/elsewhere","last_vault":"decoy","theme":"solarized",
                 "group_accounts_default":true}"#,
        )
        .unwrap();
        let vroot = vault_root.to_str().unwrap();
        let eff = effective_prefs_obj_from(Some(&config), vroot);

        assert_eq!(eff.get("export_dir"), None, "a vault must NOT choose the cleartext export destination");
        assert_eq!(eff.get("reveal_all_default"), None, "a vault must NOT unmask passwords by default");
        // The bootstrap keys were never read through the fallback; confirm they still aren't.
        assert_eq!(eff.get("vault_root"), None, "a vault cannot relocate the vault root");
        assert_eq!(eff.get("last_vault"), None, "a vault cannot choose the remembered vault");
        assert_eq!(eff.get("theme"), None, "theme is read from the config dir directly, not the fallback");
        // The purely cosmetic view key still travels with a portable vault.
        assert_eq!(
            eff.get("group_accounts_default").and_then(|v| v.as_bool()),
            Some(true),
            "cosmetic grouping default still travels with the vault"
        );
        // And the loader that actually feeds the export path reports "unset", so the UI asks
        // the user to pick a folder instead of silently using the vault's.
        assert_eq!(load_export_dir(vroot), "", "load_export_dir ignores the vault-supplied value");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checked_export_dir_refuses_unset_and_anything_inside_the_vault_folder() {
        // A per-tab CSV holds every account and portal password in the CLEAR, and a document
        // export is the decrypted file. Either one written INSIDE the vault folder is swept
        // into the user's next backup or folder sync of that vault. The CLI has refused this
        // since the extract/export-tree guards; this is the shared check the GUI and TUI use.
        let dir = tmp_prefs_dir();
        let vault_dir = dir.join("myvault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let vault_path = vault_dir.join("vault.pmv");

        // Unset -> the "pick a folder" prompt, not a write.
        assert!(checked_export_dir(&vault_path, "").is_err(), "unset is refused");
        assert!(checked_export_dir(&vault_path, "   ").is_err(), "whitespace-only is refused");

        // The vault folder itself, and anything beneath it, are refused.
        for inside in [vault_dir.clone(), vault_dir.join("exports"), vault_dir.join("volume").join("deep")] {
            let err = checked_export_dir(&vault_path, &inside.to_string_lossy())
                .expect_err("a destination inside the vault folder must be refused");
            assert!(err.contains("OUTSIDE the vault folder"), "actionable message: {err}");
        }

        // A sibling directory outside the vault is accepted, and comes back normalized
        // (trimmed, with a pasted "Copy as path" quote pair stripped).
        let outside = dir.join("exports");
        assert_eq!(checked_export_dir(&vault_path, &outside.to_string_lossy()).unwrap(), outside);
        let quoted = format!("  \"{}\"  ", outside.display());
        assert_eq!(checked_export_dir(&vault_path, &quoted).unwrap(), outside, "quotes/space normalized");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn checked_export_dir_sees_through_a_symlink_pointing_back_into_the_vault() {
        // The realistic evasion: the export directory is named through a symlink that
        // resolves into the vault folder. A purely textual comparison would call it
        // "outside"; `dest_inside` resolves both sides through the filesystem.
        let dir = tmp_prefs_dir();
        let vault_dir = dir.join("myvault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let vault_path = vault_dir.join("vault.pmv");

        let link = dir.join("looks-external");
        std::os::unix::fs::symlink(&vault_dir, &link).unwrap();
        // The destination itself does not exist yet — the normal case for a fresh export dir.
        let dest = link.join("csvs");
        assert!(!dest.exists());
        assert!(
            checked_export_dir(&vault_path, &dest.to_string_lossy()).is_err(),
            "a symlinked export dir resolving into the vault folder must be refused"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_bounded_nofollow_enforces_the_exact_cap() {
        // Pins the `> max` boundary of the bounded read that replaced `fs::read` in
        // `read_prefs_obj`: exactly `max` bytes is accepted, one over is refused — without
        // ever allocating past the ceiling, however large the file is.
        let dir = tmp_prefs_dir();
        let p = dir.join("blob");
        std::fs::write(&p, vec![b'x'; 100]).unwrap();
        assert_eq!(read_bounded_nofollow(&p, 100).unwrap().len(), 100, "exactly max is accepted");
        assert!(read_bounded_nofollow(&p, 99).is_err(), "max + 1 is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn prefs_read_does_not_follow_a_symlink_even_past_the_stat() {
        // `read_prefs_obj`'s `symlink_metadata` pre-check is an early reject, not the
        // boundary — it is a separate syscall from the read, and the old `fs::read` both
        // FOLLOWED a symlink and allocated without bound. The read itself is now
        // O_NOFOLLOW-capped, which is what makes the check race-proof; assert the read
        // primitive refuses a symlink on its own, with no stat in front of it.
        let dir = tmp_prefs_dir();
        let real = dir.join("real.json");
        std::fs::write(&real, br#"{"export_dir":"/x"}"#).unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(
            read_bounded_nofollow(&link, MAX_PREFS_SIZE).is_err(),
            "the read itself refuses a symlink, so winning the stat/read race gains nothing"
        );
        // Reading the real file still works (the guard is about the link, not the content).
        assert!(read_bounded_nofollow(&real, MAX_PREFS_SIZE).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_prefs_obj_replaces_a_symlink_instead_of_writing_through_it() {
        // Atomic temp -> rename: a rename REPLACES a planted symlink at the prefs path
        // rather than writing to whatever it points at, and never leaves a truncated file
        // for a concurrent front-end to read.
        let dir = tmp_prefs_dir();
        let target = dir.join("victim.txt");
        std::fs::write(&target, b"do not touch").unwrap();
        let p = dir.join("prefs.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &p).unwrap();

        save_export_dir_to(&p, "/exports");
        assert_eq!(load_export_dir_from(&p), "/exports", "the prefs write landed");
        #[cfg(unix)]
        {
            assert_eq!(std::fs::read(&target).unwrap(), b"do not touch", "the symlink target is untouched");
            assert!(!std::fs::symlink_metadata(&p).unwrap().file_type().is_symlink(), "symlink replaced by a real file");
        }
        // No temp files are left behind on the success path.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp leftovers: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fmt_money_reports_a_non_finite_total_as_out_of_range() {
        // `parse_approx_value` rejects a non-finite FIELD, but the Summary SUMS many finite
        // values, so two near-f64::MAX entries add to +inf. `f64 as u64` saturates, so inf
        // used to render as the literal "$18,446,744,073,709,551,615" and NaN as "$0" — a
        // fabricated figure shown as a real one in a financial view.
        assert_eq!(fmt_money(f64::INFINITY), "$—");
        assert_eq!(fmt_money(f64::NEG_INFINITY), "$—");
        assert_eq!(fmt_money(f64::NAN), "$—");
        assert_eq!(fmt_money(f64::MAX + f64::MAX), "$—", "an overflowing sum is not a number we invent");
        // Ordinary values are untouched by the guard.
        assert_eq!(fmt_money(1500.0), "$1,500");
        assert_eq!(fmt_money(f64::MAX), "$18,446,744,073,709,551,615", "finite-but-huge still saturates, as before");
    }

    #[test]
    fn fmt_money_groups_thousands_and_signs() {
        assert_eq!(fmt_money(0.0), "$0");
        assert_eq!(fmt_money(1500.0), "$1,500");
        assert_eq!(fmt_money(1_234_567.8), "$1,234,568"); // rounds, groups
        assert_eq!(fmt_money(-2500.0), "-$2,500");
        assert_eq!(fmt_money(999.0), "$999");
    }

    #[test]
    fn clipboard_tick_decision_obeys_deadline_and_preserves_unseen_status() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let future = now + Duration::from_secs(60);
        // Nothing scheduled, or deadline not reached → no action.
        assert_eq!(clipboard_tick_decision(None, now, ""), None);
        assert_eq!(clipboard_tick_decision(Some(future), now, "anything"), None);
        // Deadline reached + a blank or "Copied …" status → wipe and show the cleared notice.
        assert_eq!(
            clipboard_tick_decision(Some(now), now, ""),
            Some(Some("Clipboard cleared.".to_string()))
        );
        assert_eq!(
            clipboard_tick_decision(Some(now), now, "Copied password to clipboard."),
            Some(Some("Clipboard cleared.".to_string()))
        );
        // Deadline reached but an important status is showing → wipe, but DON'T clobber it
        // (the core rule: a "Save failed: …" the user hasn't seen must survive the auto-clear).
        assert_eq!(clipboard_tick_decision(Some(now), now, "Save failed: disk full"), Some(None));
    }
}
