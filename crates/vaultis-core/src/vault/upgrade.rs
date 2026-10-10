//! Safety copies across upgrades: before a vaultis release writes to a vault for the first
//! time, the vault as the previous release left it is copied aside, so a defect in the new
//! release cannot be the only copy of anyone's estate data.
//!
//! The rule hangs on [`Vault::written_by`], the release that last wrote the vault:
//!
//! * **same release** — nothing to do;
//! * **older release, or unknown** (a vault from 0.5.0 or earlier carries no stamp) — copy
//!   the vault into `<parent>/vaultis-backups/<vault>@v<old>@<UTC time>/`, verify the copy
//!   byte for byte, keep the newest [`KEEP_SAFETY_COPIES`], then let the open continue;
//! * **newer release** — refuse to open it for writing ([`VaultError::NewerVault`]).
//!
//! It runs inside a WRITABLE open, under the single-writer lock, after the vault decrypts
//! and BEFORE anything is written — a writable open saves at once (it refreshes
//! `last_opened_at`), so later would be too late. A read-only open writes nothing and so
//! needs no copy. If the copy cannot be made or does not verify, the writable open fails
//! ([`VaultError::SafetyCopyFailed`]): a missing safety copy must never quietly turn into
//! "carry on without one".
//!
//! The copies sit in a sibling folder, not beside the vault as `<vault>.v0.5.0`, because
//! the start page lists every folder under the vault root that holds a `vault.pmv`; a
//! sibling copy would appear there as an ordinary vault and could be edited by mistake.
//! They are on the same disk as the vault, so they guard against a bad release, not a dead
//! drive — the Backup command is still the real backup.

use super::*;

/// This build's release, the value stamped into every vault it writes. All of the
/// workspace's crates share one version, so the core's own is the app's.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Folder, beside the vault's own folder, that holds the safety copies.
pub const SAFETY_DIR: &str = "vaultis-backups";

/// How many safety copies of each kind (pre-upgrade / manual) to keep per vault. Older
/// ones are deleted only after a newer copy has been written and verified.
pub const KEEP_SAFETY_COPIES: usize = 3;

/// Label for a copy taken on request ("Back up all vaults now") rather than by an upgrade.
const MANUAL_LABEL: &str = "manual";

/// What a writable open must do about the release that last wrote the vault.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Upgrade {
    /// Written by this release: nothing to do.
    Same,
    /// Written by an older or unknown release: copy it aside first. Carries the label
    /// for the copy's folder name.
    Older(String),
    /// Written by a newer release: refuse to write.
    Newer(String),
}

/// `major.minor.patch` of a version string, ignoring any `-pre`/`+build` suffix; `None`
/// when it is not of that shape.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let parsed = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(parsed)
}

/// Keep a label safe as part of ONE folder name: only `[A-Za-z0-9._-]`, at most 32
/// characters. The stamp is authenticated (it is inside the encrypted body), but it was
/// still written by whoever held the passwords, so it never reaches a path unfiltered.
fn folder_safe(label: &str) -> String {
    let s: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(32).collect();
    if s.is_empty() || s.chars().all(|c| c == '.') { "unknown".to_string() } else { s }
}

/// Decide what a writable open of a vault stamped `written_by` must do, for this build
/// `app`. An unparseable stamp cannot be shown to be newer, so it is treated as older —
/// the direction that takes a copy rather than the one that skips it.
pub(super) fn classify(written_by: Option<&str>, app: &str) -> Upgrade {
    let Some(stamp) = written_by else {
        return Upgrade::Older(format!("before-v{}", folder_safe(app)));
    };
    match (parse_version(stamp), parse_version(app)) {
        (Some(v), Some(a)) if v == a => Upgrade::Same,
        (Some(v), Some(a)) if v > a => Upgrade::Newer(stamp.to_string()),
        _ => Upgrade::Older(format!("v{}", folder_safe(stamp))),
    }
}

/// Where the safety copies of the vault in directory `dir` go, and the vault's folder
/// name used to label them. Made absolute (so a relative path still gets one stable
/// place) without `canonicalize`, which on Windows yields `\\?\C:\…` paths that would
/// then appear verbatim in the messages telling the user where their copy is.
fn safety_dir_and_name(dir: &Path) -> Result<(PathBuf, String), VaultError> {
    let real: PathBuf = std::path::absolute(dir)?.components().collect();
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "vault".to_string());
    let parent = real.parent().map(Path::to_path_buf).unwrap_or_else(|| real.clone());
    Ok((parent.join(SAFETY_DIR), name))
}

/// Run the upgrade rule for a writable open of `path` (in directory `dir`) whose
/// decrypted stamp is `written_by`. Returns the safety copy's `vault.pmv` when one was
/// taken. The caller holds the single-writer lock and has written nothing yet.
pub(super) fn before_first_write(path: &Path, dir: &Path, written_by: Option<&str>) -> Result<Option<PathBuf>, VaultError> {
    match classify(written_by, APP_VERSION) {
        Upgrade::Same => Ok(None),
        Upgrade::Newer(written_by) => Err(VaultError::NewerVault { written_by }),
        Upgrade::Older(label) => safety_copy(path, dir, &label).map(Some),
    }
}

/// Copy the vault aside under `label`, all or nothing, then prune old copies of the same
/// kind. Returns the finished copy's `vault.pmv`.
///
/// The copy is a transaction whose commit point is one atomic rename:
///
/// 1. copy the encrypted files, as-is, into a hidden staging folder
///    `.incomplete-<name>@<label>@<time>` (nothing is decrypted);
/// 2. compare the copy with the vault byte for byte;
/// 3. fsync every copied file (strictly — an error fails the copy) and every folder, so
///    the copy is on the disk and not merely in the OS cache;
/// 4. write [`COMPLETE_MARKER`] listing every file and its size, and fsync it;
/// 5. rename the staging folder to `<name>@<label>@<time>` and fsync the parent.
///
/// Only a folder that reached step 5 is a safety copy: it has the final name and the
/// marker. A crash or full disk before then leaves at most an `.incomplete-…` folder,
/// which is never counted, is cleared by the next attempt — and, because the vault was
/// not stamped (that happens only after this returns), the next writable open has to
/// make the copy again before it can proceed. Any failure is a
/// [`VaultError::SafetyCopyFailed`], after the staging folder is removed.
fn safety_copy(path: &Path, dir: &Path, label: &str) -> Result<PathBuf, VaultError> {
    let fail = |what: &str, e: &dyn std::fmt::Display| VaultError::SafetyCopyFailed(format!("{what}: {e}"));
    // The same refusals as `backup`: a half-finished password change is not a vault
    // anyone could restore, and `fs::copy` would follow a symlinked `vault.pmv` to
    // whatever it points at.
    if dir.join(REKEY_DIR).exists() {
        return Err(VaultError::RekeyPending);
    }
    if fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(VaultError::SafetyCopyFailed("the vault file is a symlink".to_string()));
    }
    let (safety_dir, name) = safety_dir_and_name(dir).map_err(|e| fail("cannot resolve the vault folder", &e))?;
    // The copies' folder must be a real directory: never write through a planted symlink.
    if let Ok(meta) = fs::symlink_metadata(&safety_dir)
        && !meta.file_type().is_dir()
    {
        return Err(VaultError::SafetyCopyFailed(format!("{} exists and is not a folder", safety_dir.display())));
    }
    fs::create_dir_all(&safety_dir).map_err(|e| fail("cannot create the backups folder", &e))?;
    harden_dir(&safety_dir);
    clear_incomplete(&safety_dir, &name);

    let stamp = compact_timestamp(records::unix_now());
    let base = unique_name(&safety_dir, &format!("{name}@{label}@{stamp}"));
    let staging = safety_dir.join(format!("{INCOMPLETE_PREFIX}{base}"));
    let target = safety_dir.join(&base);

    let result = (|| -> Result<(), VaultError> {
        copy_vault_tree(path, dir, &staging)?;
        if !trees_identical(path, dir, &staging)? {
            return Err(VaultError::SafetyCopyFailed("the copy does not match the vault byte for byte".to_string()));
        }
        crate::fault::point("safety.copied")?;
        let files = sync_tree(&staging, &staging)?;
        write_marker(&staging, label, &files)?;
        crate::fault::point("safety.committing")?;
        fs::rename(&staging, &target)?;
        sync_parent_dir(&target);
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(match e {
            VaultError::SafetyCopyFailed(_) | VaultError::RekeyPending => e,
            other => fail("copying the vault", &other),
        });
    }
    prune(&safety_dir, &name, label == MANUAL_LABEL, &target);
    Ok(target.join(VAULT_FILE))
}

/// A folder name for a new copy: `stem` itself, or — when copies made within the same
/// second already exist — `stem_<n>` with `n` one past the HIGHEST suffix in use. Never a
/// freed lower slot: pruning deletes the oldest copies, and reusing one of their names
/// would make the newest copy sort as the oldest (see [`time_key`]).
fn unique_name(safety_dir: &Path, stem: &str) -> String {
    let taken: Vec<u64> = fs::read_dir(safety_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let n = n.strip_prefix(INCOMPLETE_PREFIX).map(str::to_string).unwrap_or(n);
            if n == stem {
                Some(0)
            } else {
                n.strip_prefix(stem)?.strip_prefix('_')?.parse().ok()
            }
        })
        .collect();
    match taken.iter().max() {
        None => stem.to_string(),
        Some(max) => format!("{stem}_{}", max + 1),
    }
}

/// Chronological sort key of a copy's `<YYYYMMDD-HHMMSS>[_<n>]` time part: the timestamp,
/// then the same-second suffix compared as a NUMBER (as text, `_10` would sort before `_9`).
fn time_key(time: &str) -> (String, u64) {
    match time.split_once('_') {
        Some((stamp, n)) => (stamp.to_string(), n.parse().unwrap_or(0)),
        None => (time.to_string(), 0),
    }
}

/// Name prefix of a copy still being made (or left behind by a crash). Never a safety copy.
const INCOMPLETE_PREFIX: &str = ".incomplete-";

/// Written last, inside a finished copy, before the rename that publishes it: the file's
/// presence is what says the copy is whole. Lists the release, the time and every file.
pub const COMPLETE_MARKER: &str = "SAFETY-COPY-COMPLETE";

/// Remove staging folders a crashed attempt left for the vault `name`. Real folders whose
/// names follow this module's own pattern only; never a symlink, never another vault's.
fn clear_incomplete(safety_dir: &Path, name: &str) {
    let Ok(entries) = fs::read_dir(safety_dir) else { return };
    let prefix = format!("{INCOMPLETE_PREFIX}{name}@");
    for e in entries.filter_map(Result::ok) {
        // Exactly `<name>@<label>@<time>` with an `@`-free label, as `prune` parses it — so
        // vault `a` never matches the leftovers of a vault named `a@b`.
        let ours = e.file_name().to_str().and_then(|n| n.strip_prefix(&prefix)?.rsplit_once('@')).is_some_and(|(label, _)| !label.contains('@'));
        let is_dir = fs::symlink_metadata(e.path()).map(|m| m.file_type().is_dir()).unwrap_or(false);
        if ours && is_dir {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// fsync every file under `dir` (strictly: an error is returned) and every folder (best
/// effort, as everywhere else in this codebase — some filesystems refuse a directory
/// fsync). Returns each file's path relative to `root` and its size, for the marker.
/// Files are reopened for writing because Windows will not flush a read-only handle.
fn sync_tree(root: &Path, dir: &Path) -> Result<Vec<(String, u64)>, VaultError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if entry.file_type()?.is_dir() {
            files.extend(sync_tree(root, &p)?);
        } else {
            let f = OpenOptions::new().read(true).write(true).open(&p)?;
            f.sync_all()?;
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            files.push((rel, f.metadata()?.len()));
        }
    }
    sync_parent_dir(&dir.join("."));
    files.sort();
    Ok(files)
}

/// Write [`COMPLETE_MARKER`] into the staging folder and fsync it.
fn write_marker(staging: &Path, label: &str, files: &[(String, u64)]) -> Result<(), VaultError> {
    let mut body = format!(
        "vaultis safety copy — complete and verified.\nmade by: vaultis {APP_VERSION}\nlabel: {label}\n\
         made at (unix): {}\nfiles ({}):\n",
        records::unix_now(),
        files.len()
    );
    for (rel, len) in files {
        body.push_str(&format!("  {len:>12}  {rel}\n"));
    }
    let marker = staging.join(COMPLETE_MARKER);
    let mut f = OpenOptions::new().write(true).create_new(true).open(&marker)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    harden_file(&marker)?;
    sync_parent_dir(&marker);
    Ok(())
}

/// Whether the copy at `target` holds exactly the vault's files: `vault.pmv` and every
/// file under `manifest/` and `volume/`, compared byte for byte (streamed, so a large
/// volume never sits in memory). The source cannot change meanwhile — the caller holds
/// the single-writer lock.
fn trees_identical(vault_path: &Path, src_dir: &Path, target: &Path) -> Result<bool, VaultError> {
    if !files_identical(vault_path, &target.join(VAULT_FILE))? {
        return Ok(false);
    }
    for sub in ["manifest", "volume"] {
        let s = src_dir.join(sub);
        if s.exists() && !dirs_identical(&s, &target.join(sub))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn dirs_identical(src: &Path, dst: &Path) -> Result<bool, VaultError> {
    let mut count = 0usize;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        let same = if entry.file_type()?.is_dir() {
            dirs_identical(&entry.path(), &to)?
        } else {
            files_identical(&entry.path(), &to)?
        };
        if !same {
            return Ok(false);
        }
        count += 1;
    }
    // Nothing extra in the copy either.
    Ok(fs::read_dir(dst)?.count() == count)
}

fn files_identical(a: &Path, b: &Path) -> Result<bool, VaultError> {
    use std::io::Read;
    let (Ok(fa), Ok(fb)) = (fs::File::open(a), fs::File::open(b)) else { return Ok(false) };
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ra, mut rb) = (std::io::BufReader::new(fa), std::io::BufReader::new(fb));
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = ra.read(&mut ba)?;
        if n == 0 {
            // Equal lengths were checked above, so `b` is exhausted too.
            return Ok(true);
        }
        rb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// Keep the newest [`KEEP_SAFETY_COPIES`] copies of the vault `name` of one kind (manual,
/// or pre-upgrade), deleting the rest. Only ever called after a newer copy verified. It
/// touches nothing but real directories (never symlinks) whose names follow this module's
/// own `<name>@<label>@<time>` pattern and hold the completion marker; a failure to delete
/// is left for the next run.
fn prune(safety_dir: &Path, name: &str, manual: bool, just_made: &Path) {
    let Ok(entries) = fs::read_dir(safety_dir) else { return };
    let prefix = format!("{name}@");
    let mut ours: Vec<((String, u64), PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file_name = e.file_name().into_string().ok()?;
            let (label, time) = file_name.strip_prefix(&prefix)?.rsplit_once('@')?;
            let is_dir = fs::symlink_metadata(e.path()).map(|m| m.file_type().is_dir()).unwrap_or(false);
            // Only finished copies count — and only they are ever deleted. A folder without
            // the marker is not a copy this module completed, so it is left alone.
            let finished = e.path().join(COMPLETE_MARKER).is_file();
            (is_dir && finished && (label == MANUAL_LABEL) == manual && !label.contains('@'))
                .then(|| (time_key(time), e.path()))
        })
        .collect();
    // Newest first, by (timestamp, same-second suffix).
    ours.sort_by(|a, b| b.0.cmp(&a.0));
    // The copy just made is never deleted, whatever else the folder holds: retention
    // orders by the time in the NAME, and a folder planted with a far-future name must not
    // be able to push out the copy this open is relying on.
    for (_, old) in ours.into_iter().skip(KEEP_SAFETY_COPIES) {
        if old != just_made {
            let _ = fs::remove_dir_all(old);
        }
    }
}

impl OpenVault {
    /// Where this session's pre-upgrade safety copy went (its `vault.pmv`), if opening it
    /// took one: the vault had last been written by an older release.
    pub fn safety_copy(&self) -> Option<&Path> {
        self.safety_copy.as_deref()
    }

    /// Take a safety copy of this open vault now, labelled `manual` (the GUI's "Back up
    /// all vaults now"), into the same `vaultis-backups/` folder and under the same
    /// retention as the pre-upgrade copies, which it never evicts. A writable session
    /// already holds the single-writer lock; a read-only one takes it for the copy.
    pub fn manual_safety_copy(&self) -> Result<PathBuf, VaultError> {
        let dir = parent_dir(&self.path);
        let _lock = if self.read_only { lock_for_read_only_copy(&dir)? } else { None };
        safety_copy(&self.path, &dir, MANUAL_LABEL)
    }
}

/// Take a `manual` safety copy of the vault at `vault_path` that is NOT open in this
/// process, under the single-writer lock (so it fails with [`VaultError::Locked`] while
/// another session is writing it). Needs no passwords: only encrypted files are copied.
pub fn manual_safety_copy(vault_path: &Path) -> Result<PathBuf, VaultError> {
    if !vault_path.exists() {
        return Err(VaultError::NotFound(vault_path.to_path_buf()));
    }
    let dir = parent_dir(vault_path);
    let _lock = lock_for_read_only_copy(&dir)?;
    safety_copy(vault_path, &dir, MANUAL_LABEL)
}
