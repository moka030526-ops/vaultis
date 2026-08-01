//! Launch helpers shared by the two binaries.
//!
//! The project ships two executables that resolve the vault location identically:
//! the console `vaultis` (CLI subcommands + the `--tui` terminal UI) and the
//! windowed `vaultis-gui` (the graphical UI built as a Windows **GUI-subsystem**
//! app, so it opens *without* a command window). Keeping the path/flag logic here —
//! instead of duplicated in each binary — guarantees `vaultis DIR` and
//! `vaultis-gui DIR` open the same vault.
//!
//! That guarantee covers the *flags* as well as the path, which is why [`VERSION_LINE`]
//! and the `--help`/`--version` handling in [`resolve_interactive`] live here too: when
//! only the console binary knew those tokens, `vaultis-gui --version` opened a vault
//! directory named `--version` instead of answering (audit 2026-07-29 round 2, L-1).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Default vault location: the per-user data directory for this app
/// (`~/.local/share/vaultis/` on Linux, `%APPDATA%\vaultis\` on Windows).
pub fn default_vault_path() -> PathBuf {
    match ProjectDirs::from("dev", "vaultis", "vaultis") {
        Some(dirs) => dirs.data_dir().join("vault.pmv"),
        None => PathBuf::from("vault.pmv"),
    }
}

/// The name of the encrypted vault file inside a vault directory. A directory is
/// treated as a vault iff it directly contains a file with this name.
const VAULT_FILE: &str = "vault.pmv";

/// The ONE file this app writes outside a vault root: a single line, in the per-user OS
/// data directory, naming the last vault root a vault was successfully opened from.
/// Everything else it remembers — theme, ui scale, font, list grouping — lives in
/// `<vault_root>/prefs.json` and travels with the vault media instead (see the `lib.rs`
/// prefs comment); this exception exists purely so the start page can find its way back
/// to where you last worked without a shortcut or a `cd` trick.
fn last_root_file() -> Option<PathBuf> {
    ProjectDirs::from("dev", "vaultis", "vaultis").map(|d| d.data_dir().join("last_root.txt"))
}

/// Hard cap on `last_root.txt`. It holds ONE filesystem path, so 4 KiB is already far more
/// than any real vault root needs (Linux `PATH_MAX` is 4096); anything larger is corrupt or
/// hostile. Bounding the read before allocating means an over-size or `/dev/zero`-symlinked
/// file can never stall or OOM the UI at startup. Same role as [`crate::MAX_PREFS_SIZE`].
const MAX_LAST_ROOT_SIZE: u64 = 4 * 1024;

/// The remembered last-opened root, or `None` on first run, or if the file is
/// missing, unreadable, or empty.
///
/// Guarded under `cfg(test)` (returning `None`) because — unlike `prefs.json`, which is
/// resolved per-vault-root and so is naturally hermetic under test — this file lives at one
/// fixed OS path shared by every test AND the real app. Reading it for real would make the
/// test suite's "does the start page open empty" assertions depend on whatever this
/// developer's machine happens to have last opened. See [`load_last_root_from`] for the
/// exercised, path-parametrized logic.
pub fn load_last_root() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    load_last_root_from(&last_root_file()?)
}
pub(crate) fn load_last_root_from(path: &Path) -> Option<String> {
    // Read through the SAME hardened helper `prefs.json` uses, not `std::fs::read_to_string`.
    // That call follows a final-component symlink and allocates without any ceiling, so a
    // symlink planted here was read THROUGH (returning an arbitrary readable file's contents,
    // which then land in the start page's vault-root box) and a huge file was read whole —
    // at UI startup, before anything is drawn. The writer for this very file already used the
    // hardened `write_atomic`; only the reader was raw, which is exactly the "guard on some
    // paths but not all" shape this codebase keeps producing (audit 2026-07-29, L-1).
    //
    // The `symlink_metadata` pre-check is a cheap early reject and the only symlink guard on
    // non-unix; `read_bounded_nofollow`'s `O_NOFOLLOW` is the real boundary on unix, since the
    // stat is a separate syscall from the open. Mirrors `crate::read_prefs_obj` exactly.
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_LAST_ROOT_SIZE => {}
        _ => return None,
    }
    let bytes = crate::read_bounded_nofollow(path, MAX_LAST_ROOT_SIZE).ok()?;
    // Fails closed to None on invalid UTF-8, same as the old `read_to_string` did, and the
    // same as a missing file — "no remembered root" is already a supported state (first run).
    let s = String::from_utf8(bytes).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Remember `root` as the last-opened root, creating the file — and its parent directory,
/// if the OS data directory doesn't exist yet — the first time this is called. Best-effort:
/// a write failure is silently ignored, since this is a convenience for the next launch, not
/// data the app depends on. Written via [`crate::write_atomic`] — the same temp-then-rename
/// discipline `prefs.json` uses — rather than a direct `std::fs::write`, so a crash mid-write
/// or a concurrent read can never see a half-written file, and a symlink planted at the path
/// is replaced by the rename rather than opened and written through.
///
/// Guarded under `cfg(test)` for the same reason as [`load_last_root`]: this is the one fixed
/// OS path shared by every test, and a test run must never leave a trace in — or take a cue
/// from — this developer's real `last_root.txt`.
pub fn save_last_root(root: &str) {
    if cfg!(test) {
        return;
    }
    if let Some(path) = last_root_file() {
        save_last_root_to(&path, root);
    }
}
pub(crate) fn save_last_root_to(path: &Path, root: &str) {
    crate::write_atomic(path, root.as_bytes());
}

/// The vault file inside a user-supplied vault directory.
pub fn vault_file(dir: &str) -> PathBuf {
    PathBuf::from(dir).join(VAULT_FILE)
}

/// The two passwords the demo vault is built with — kept in sync with
/// `SAMPLE_PW1`/`SAMPLE_PW2` in `scripts/build.sh` and with the release workflow, which
/// seeds the same vault into the shipped package.
pub const SAMPLE_PW1: &str = "sample1";
pub const SAMPLE_PW2: &str = "sample2";

/// The demo vault, if one actually exists — `None` (rather than a guessed path) when it
/// does not, so callers can hide the affordance instead of offering a button that fails.
///
/// `VAULTIS_SAMPLE_DIR` wins when set. Otherwise there are two layouts to find, because a
/// copy of vaultis arrives one of two ways and they put the sample in different places:
///
/// * **Installed** — `<exe dir>/sample-vault`. `.github/workflows/release.yml` seeds the
///   vault into the release zip, and `get_vaultis.bat` extracts that zip whole, so the
///   folder lands directly beside the two executables.
/// * **Built from source** — `target/sample-vault`, where `scripts/build.sh` puts it. The
///   executable is at `target/<debug|release>/<exe>`, so walking up two directories
///   reaches `target/` without needing the repo root or the profile that was built.
///
/// Checked in that order, and both are checked every time: a developer's `target/release`
/// build and an installed copy can exist on one machine, and the exe being run decides
/// which sample is its own. Neither layout can produce the other's path by accident —
/// `<exe dir>/sample-vault` for a cargo build would be `target/release/sample-vault`,
/// which nothing writes.
pub fn sample_vault_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VAULTIS_SAMPLE_DIR") {
        let dir = PathBuf::from(dir);
        return dir.join(VAULT_FILE).is_file().then_some(dir);
    }
    sample_vault_beside(std::env::current_exe().ok()?.parent()?)
}

/// The layout search of [`sample_vault_dir`], split out from the two things that cannot be
/// arranged in a test — the process's own executable path and the environment.
fn sample_vault_beside(exe_dir: &Path) -> Option<PathBuf> {
    let installed = exe_dir.join("sample-vault");
    if installed.join(VAULT_FILE).is_file() {
        return Some(installed);
    }
    let from_source = exe_dir.parent()?.join("sample-vault");
    from_source.join(VAULT_FILE).is_file().then_some(from_source)
}

/// The outcome of scanning a root directory for vaults: the discovered vault names
/// plus an optional human-readable `warning` to surface in the UI. The warning is
/// `Some` when the root itself can't be read (the list is then empty) or when some
/// entries beneath it had to be skipped because they were inaccessible.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VaultScan {
    pub vaults: Vec<String>,
    pub warning: Option<String>,
}

/// The vault directory for a `root` plus a selected/typed leaf `name`. An empty name
/// resolves to the root itself (so a vault sitting directly at the root still opens);
/// otherwise it is `<root>/<name>`. This is the collapsed start page's single source of
/// truth: the open target is always `root` + `name`.
///
/// The ROOT is normalized with [`records::unquote_path`](crate::records::unquote_path):
/// trimmed, and a matched pair of surrounding double quotes removed, so a folder pasted
/// from a file manager's "Copy as path" (`"C:\Users\me\My Vaults"`) opens the directory it
/// names rather than a literally-quoted one that cannot exist.
pub fn join_root_name(root: &str, name: &str) -> String {
    let root = crate::records::unquote_path(root);
    // Trim only to DECIDE empty-vs-present; when present, join the name VERBATIM so this is
    // the exact inverse of `discover_vaults`, which returns raw directory names. Trimming the
    // joined name would make a vault folder whose name has leading/trailing whitespace
    // un-openable from the dropdown — the derived path wouldn't match the real folder, so the
    // start page would silently flip to "Create" instead of opening the selected vault.
    if name.trim().is_empty() {
        root.to_string()
    } else {
        Path::new(root).join(name).display().to_string()
    }
}

/// Compute the start page's initial `(root, vault_name)` for a launched vault `path`, given
/// the OS-remembered `last_root` ([`load_last_root`], `None` on first run).
///
/// Precedence is **argument > last root > empty**:
///
/// 1. An **explicitly launched** vault (a `path` differing from the per-user default) always
///    wins: its parent becomes the root and its folder the selected name, so `vaultis DIR`
///    opens exactly `DIR`.
/// 2. Otherwise, the last root a vault was successfully opened from (if any): browsed, with
///    nothing pre-selected — the user still picks the vault.
/// 3. Otherwise **empty**: the start page opens with both boxes blank and the user types or
///    pastes a root.
///
/// # Why the working directory is NOT consulted
///
/// There used to be a rule between (1) and (2): a current directory that was itself a folder
/// of vaults became the root, so `cd /my/vaults && vaultis-gui` browsed it. That rule was
/// removed because shipping the sample vault turned it into a trap.
///
/// `make-shortcuts.ps1` sets both Desktop shortcuts' working directory to the **install
/// folder**, and since the sample vault ships *beside the executables*, that folder contains
/// `sample-vault/vault.pmv` — so it qualified as a vault root. Every shortcut launch therefore
/// resolved to the install directory showing `sample-vault`, and because the cwd outranked
/// `last_root`, it **permanently shadowed the user's real remembered root**: `last_root.txt`
/// was written correctly and could never win. For a program whose whole job is to put an
/// executor in front of the real estate vault, silently presenting a vault of invented
/// practice data on every launch is the wrong default, and no amount of ranking fixes the
/// general problem — any future file shipped next to the exe could re-trigger it.
///
/// Launching a specific folder is still fully supported, explicitly: `vaultis-gui DIR`.
pub fn initial_root_and_name(path: &Path, last_root: Option<&str>) -> (String, String) {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir_parent = dir.and_then(|d| d.parent()).filter(|p| !p.as_os_str().is_empty());
    let parent_str = dir_parent.map(|p| p.display().to_string());
    let leaf = dir.and_then(|d| d.file_name()).and_then(|n| n.to_str()).map(str::to_owned);

    if path != default_vault_path() {
        // Honor the launched vault: root = its parent, name = its folder.
        (parent_str.unwrap_or_else(|| ".".into()), leaf.unwrap_or_default())
    } else if let Some(last) = last_root {
        // A root was remembered from a previous session.
        (last.to_string(), String::new())
    } else {
        (String::new(), String::new())
    }
}

/// Discover the vaults directly beneath `root`: every IMMEDIATE subdirectory that
/// contains a `vault.pmv`. Returns the subdirectory NAMES (not full paths), sorted
/// case-insensitively. The scan is one level deep only (never recursive) and never
/// includes `root` itself. This powers the start-page vault dropdown in both front-ends.
///
/// Errors are reported, not hidden. An unreadable root (missing, not a directory, or
/// permission-denied) yields an empty list with an explanatory `warning`. Individual
/// entries that can't be inspected — an unreadable directory entry, a subdirectory
/// whose metadata or vault-marker can't be read — are skipped and tallied into a
/// "N skipped (inaccessible)" warning rather than aborting the whole scan.
///
/// The root is normalized exactly as in [`join_root_name`] (trimmed, with a matched pair of
/// surrounding double quotes stripped), so a pasted quoted folder scans the same directory the
/// start page would open — the dropdown and the open target can never disagree.
pub fn discover_vaults(root: &str) -> VaultScan {
    let root = crate::records::unquote_path(root);
    // An empty root isn't a read ERROR — nothing has been typed yet — so say so plainly
    // rather than surfacing the OS's "No such file or directory" for path "".
    if root.is_empty() {
        return VaultScan { vaults: Vec::new(), warning: Some("Specify a vault root.".to_string()) };
    }
    let root = std::path::Path::new(root);
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            return VaultScan {
                vaults: Vec::new(),
                warning: Some(format!("Cannot read vault root '{}': {e}", root.display())),
            };
        }
    };
    let mut vaults: Vec<String> = Vec::new();
    let mut skipped: usize = 0;
    for entry in entries {
        // An entry that errors mid-iteration (e.g. a racing unlink, an unreadable
        // name) is skipped, not fatal.
        let Ok(entry) = entry else {
            skipped += 1;
            continue;
        };
        let path = entry.path();
        // `metadata` follows symlinks, so a subdir symlinked to a vault still counts;
        // a permission error reading the metadata means we can't classify it → skip.
        let is_dir = match std::fs::metadata(&path) {
            Ok(m) => m.is_dir(),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if !is_dir {
            continue; // a plain file under the root is simply not a vault.
        }
        // `try_exists` distinguishes "no marker" (Ok(false)) from "couldn't check"
        // (Err, e.g. a permission-denied subtree) — the latter is surfaced as skipped
        // rather than silently treated as "not a vault".
        match path.join(VAULT_FILE).try_exists() {
            Ok(true) => {
                if let Some(name) = entry.file_name().to_str() {
                    vaults.push(name.to_owned());
                } else {
                    skipped += 1; // a non-UTF-8 directory name we can't display/select.
                }
            }
            Ok(false) => {}
            Err(_) => skipped += 1,
        }
    }
    // Case-insensitive so the list reads naturally regardless of the OS's raw
    // directory-entry order.
    vaults.sort_by_key(|s| s.to_lowercase());
    let warning = (skipped > 0)
        .then(|| format!("{skipped} item(s) under the root were skipped (inaccessible)."));
    VaultScan { vaults, warning }
}

/// This build's identity: the program name plus the `vaultis` crate version.
///
/// Lives here rather than in `main.rs` so the console and windowed binaries print the
/// *same* string — `scripts/release.sh` requires the release tag to match this crate's
/// version, so this line names exactly one published build.
pub const VERSION_LINE: &str = concat!("vaultis ", env!("CARGO_PKG_VERSION"));

/// What the windowed binary should print for `--help`. Deliberately short: the console
/// binary owns the subcommands (and the full `--help`), because it is the one with a
/// console to print them to.
pub const GUI_USAGE: &str = concat!(
    "vaultis ",
    env!("CARGO_PKG_VERSION"),
    " — standalone, offline, two-password encrypted estate vault

USAGE:
    vaultis-gui [DIR]        Open the graphical UI (read-only by default)
    vaultis-gui --write [DIR]  Open it able to create/edit/delete
    vaultis-gui --version    Print the version of this build and exit
    vaultis-gui --help       Show this message

DIR is the vault DIRECTORY (it holds vault.pmv, manifest/, and volume/); the per-user
default is used if it is omitted. This windowed binary understands only an interactive
launch. The CLI subcommands (decrypt, manifest, extract, backup, export-tree,
import-tree, update-from, compact) and the terminal UI live in the console binary —
run `vaultis --help` for those."
);

/// What a windowed-binary command line asked for.
///
/// `--help`/`--version` are requests to *print and exit*, not vault paths — modelled as
/// their own variants so the caller cannot accidentally treat them as a directory, which
/// is exactly the bug this type replaced (audit 2026-07-29 round 2, L-1).
pub enum Interactive {
    /// Open this vault file; `writable` is `--write`.
    Open { path: PathBuf, writable: bool },
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h`.
    Help,
}

/// Resolve an interactive launch from the process arguments (everything *after*
/// the program name).
///
/// The first non-flag argument is the vault DIRECTORY (its `vault.pmv` is used);
/// if there is none, the per-user default is used. `--write` enables mutations.
/// This is the windowed launcher's whole command line — it has no console, so it
/// deliberately understands only what an interactive launch needs and leaves the
/// CLI subcommands to the console binary.
pub fn resolve_interactive(args: &[String]) -> Result<Interactive, String> {
    // The four print-and-exit tokens, answered FIRST and in the same order as the console
    // binary (`main.rs`), so both binaries agree on what a bare `--version` means. Before
    // this, only the console binary knew them: `vaultis-gui --version` fell through to the
    // positional filter below and opened a vault directory literally named `--version`,
    // silently on Windows, where the GUI subsystem swallows the error message.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Interactive::Help);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Ok(Interactive::Version);
    }
    let writable = args.iter().any(|a| a == "--write");
    // Treat ONLY the exact known flags as flags — NOT any '-'-prefixed token. A blanket
    // `starts_with('-')` filter silently ignored a vault directory whose name begins with
    // '-' (falling back to the default vault) while the console binary, which strips only the
    // exact `--write`/`--tui` tokens, treated it as the directory — so `vaultis DIR` and
    // `vaultis-gui DIR` could open DIFFERENT vaults. Matching the exact set keeps both
    // binaries' resolution identical (the module's stated guarantee). The help/version
    // tokens never reach here — they returned above, in both binaries.
    let positionals: Vec<&String> = args.iter().filter(|a| !matches!(a.as_str(), "--write" | "--tui")).collect();
    // At most ONE positional (the optional vault DIR). Reject extras instead of silently
    // opening the first and ignoring the rest — matching the console binary's arity checks.
    if positionals.len() > 1 {
        return Err(format!(
            "too many arguments: expected at most one vault DIR, got {}: {:?}. Usage: vaultis-gui [DIR] [--write]",
            positionals.len(),
            positionals
        ));
    }
    let path = positionals.first().map(|d| vault_file(d)).unwrap_or_else(default_vault_path);
    Ok(Interactive::Open { path, writable })
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
