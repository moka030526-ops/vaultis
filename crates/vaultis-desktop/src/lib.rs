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
/// The Argon2id cost to CREATE a new vault with — the built-in default (64 MiB, 3
/// passes, 1 lane, applied twice by the two-password chain) unless overridden by
/// `VAULTIS_KDF_MCOST_MIB` / `VAULTIS_KDF_TCOST`.
///
/// # Why this knob exists
///
/// Against a "harvest now, decrypt later" adversary — someone who copies the encrypted
/// vault today and waits — the cipher is not the weak point. There is no public-key
/// cryptography anywhere in vaultis, so Shor's algorithm has nothing to attack, and the
/// key is a full 256 bits, which leaves ~128-bit security under Grover. What an attacker
/// actually does is **guess the two passwords**, and the only two things standing in the
/// way are their entropy and the per-guess cost set here. Password entropy dominates; this
/// is the second lever, and it was previously not reachable at all — every create site
/// hardcoded the default even though the on-disk format has always supported up to
/// `MAX_M_COST` (512 MiB) and `MAX_T_COST` (16).
///
/// # Read this before raising it
///
/// The cost is baked into the vault at creation and paid on **every open, forever, on
/// every device**. Raise it to 512 MiB and an unlock needs half a gigabyte of memory —
/// which a phone may simply refuse, leaving the vault openable only on the desktop. For an
/// estate vault, "my executor cannot open it" is a far more likely catastrophe than "a
/// quantum computer read it in 2050". Prefer longer passwords first: entropy is free at
/// open time, and this is not.
///
/// Invalid or out-of-range values fall back to the default with a warning rather than
/// failing the create — the bound is `KdfParams::validate`, the same gate the reader uses,
/// so this can never write a vault the reader would refuse.
pub fn kdf_params_for_new_vault() -> vaultis_core::crypto::KdfParams {
    use vaultis_core::crypto::KdfParams;
    let default = KdfParams::default();

    // `MiB` in the variable name because m_cost is in KiB — an easy factor-of-1024 trap.
    let mib = std::env::var("VAULTIS_KDF_MCOST_MIB").ok();
    let t = std::env::var("VAULTIS_KDF_TCOST").ok();
    if mib.is_none() && t.is_none() {
        return default;
    }

    let parse = |v: &Option<String>, name: &str, fallback: u32| -> u32 {
        match v {
            None => fallback,
            Some(s) => match s.trim().parse::<u32>() {
                Ok(n) => n,
                Err(_) => {
                    eprintln!("warning: {name}={s:?} is not a number; using {fallback}");
                    fallback
                }
            },
        }
    };

    let m_cost_mib = parse(&mib, "VAULTIS_KDF_MCOST_MIB", default.m_cost / 1024);
    let candidate = KdfParams {
        // Saturating: a silly MiB value must not wrap into a *weak* KiB cost. Anything
        // out of range is caught by validate() below anyway.
        m_cost: m_cost_mib.saturating_mul(1024),
        t_cost: parse(&t, "VAULTIS_KDF_TCOST", default.t_cost),
        p_cost: default.p_cost,
    };

    // An unparseable value falls back per-field, which can land exactly on the default;
    // announcing that as "non-default" would be simply untrue. Say nothing in that case.
    if candidate.m_cost == default.m_cost && candidate.t_cost == default.t_cost {
        return default;
    }

    match candidate.validate() {
        Ok(()) => {
            eprintln!(
                "note: creating this vault with a non-default KDF cost ({} MiB, {} passes). \
                 Every future open — on every device, including phones — must pay it.",
                candidate.m_cost / 1024,
                candidate.t_cost
            );
            candidate
        }
        Err(_) => {
            eprintln!(
                "warning: requested KDF cost ({} MiB, {} passes) is outside the accepted range \
                 ({}–{} MiB, 1–{} passes); using the default ({} MiB, {} passes).",
                candidate.m_cost / 1024,
                candidate.t_cost,
                KdfParams::MIN_M_COST.div_ceil(1024),
                KdfParams::MAX_M_COST / 1024,
                KdfParams::MAX_T_COST,
                default.m_cost / 1024,
                default.t_cost,
            );
            default
        }
    }
}

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
//   * `export_dir` — where the front-ends write CLEARTEXT exports: the per-tab CSV (every
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
// Both front-ends share these helpers, and every write is a read-modify-write so one key
// never clobbers another.

use std::io::Read;
use std::path::{Path, PathBuf};

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
    // A unique hidden temp beside the target, so two front-ends (or two callers) saving at
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
/// Deliberately **not persisted anywhere**. This is where the front-ends write CLEARTEXT
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
// Both front-ends read these at startup to seed per-tab view state. They only choose
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
