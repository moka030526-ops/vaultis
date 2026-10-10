//! In-place redundancy copies of the vault file (§12.8): the mirror, the rotating `.bak`
//! generations, and sweeping stale or foreign-epoch copies.

use super::*;

// --- In-place redundancy file management (§12.8) -----------------------------

impl OpenVault {
    /// Best-effort regeneration of the in-place redundancy copies (mirror + `bak1`)
    /// under the CURRENT key, without bumping the generation. Used right after a
    /// rekey/compaction commit so the configured protection is restored immediately
    /// instead of being absent until the next ordinary save (§12.8).
    pub(super) fn refresh_redundancy_copies(&self) {
        let depth = self.vault.settings.redundancy;
        if depth == 0 {
            return;
        }
        // Fault point (crash-test only): a crash here leaves the just-committed vault
        // with no redundant copies until the next save — recovery from the primary is
        // unaffected (it is the authoritative, already-durable tree).
        let _ = crate::fault::point("redundancy.refresh");
        // A fresh mirror of the just-committed vault, and a bak1 copy of the live
        // primary (the post-rekey generations legitimately reset to the new epoch).
        let _ = write_vault_file(&mirror_path(&self.path), &self.vault, &self.key, &self.salt, self.params);
        // Ring bak1 in ONLY if the primary still decodes under the current key — the same
        // A-3 invariant `save_internal` enforces via `prev_decodes`. This is the other
        // site that feeds the recovery ring, and it was reading the primary back with no
        // AEAD check: a primary that bit-rotted between the rename above and this read
        // would be copied into bak1 as an unrecoverable "generation". In practice the
        // bytes were just written and fsync'd by this process under the single-writer
        // lock, so this is defence in depth — but an invariant enforced at one of two
        // sites is not an invariant.
        if let Ok(bytes) = read_capped_vault(&self.path)
            && decode_vault_with_key(&bytes, &self.key).is_ok()
        {
            let _ = write_bytes_atomic(&bak_path(&self.path, 1), &bytes);
        }
        prune_generations_above(&self.path, depth);
        // The document index's spares were swapped out with the old manifest directory
        // (rekey/compact replace it wholesale), so write them again under the new key.
        self.storage.refresh_manifest_mirrors(&self.key);
    }

    /// Set the in-place redundancy depth (§12.8): `0` = off, `N >= 1` = keep a
    /// same-generation mirror plus `N` prior generations of `vault.pmv`. Clamped to
    /// [`MAX_REDUNDANCY`]. Persists immediately (the new copies appear on this save).
    pub fn set_redundancy(&mut self, depth: u32) -> Result<(), VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let depth = depth.min(MAX_REDUNDANCY);
        self.vault.settings.redundancy = depth;
        self.vault.audit.push(Change::new("redundancy_changed", depth.to_string()));
        // Apply the change to the document index's spare copies too: turning it off
        // deletes them, turning it on writes them now rather than at the next upload
        // (matching `refresh_redundancy_copies` for the vault file's own copies). Both
        // side effects are explicit here — this path is write-gated by the check above,
        // which is what makes them safe to perform (audit 2026-08-03 A-1).
        self.storage.set_redundancy(depth);
        if depth == 0 {
            self.storage.drop_manifest_mirrors();
        } else {
            self.storage.refresh_manifest_mirrors(&self.key);
        }
        self.save()
    }

    /// The current in-place redundancy depth (`0` = off).
    pub fn redundancy(&self) -> u32 {
        self.vault.settings.redundancy
    }

    /// A notice if this vault was recovered from a redundant copy on open (§12.8),
    /// for the front-ends to surface; `None` on a normal open.
    pub fn recovery_notice(&self) -> Option<&str> {
        self.recovery_notice.as_deref()
    }
}

/// `vault.pmv` -> `vault.pmv<suffix>` (append, not replace-extension).
pub(super) fn with_suffix(primary: &Path, suffix: &str) -> PathBuf {
    let mut name = primary.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| std::ffi::OsString::from(VAULT_FILE));
    name.push(suffix);
    match primary.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// The same-generation mirror path (`vault.pmv.mirror`).
pub(super) fn mirror_path(primary: &Path) -> PathBuf {
    with_suffix(primary, ".mirror")
}

/// The k-th retained prior generation (`vault.pmv.bak1` = newest prior).
pub(super) fn bak_path(primary: &Path, k: u32) -> PathBuf {
    with_suffix(primary, &format!(".bak{k}"))
}

/// Write `bytes` to `dst` atomically and **symlink-safely**: a fresh O_EXCL temp
/// (0600, never follows a symlink) is written and fsync'd, then renamed over `dst`
/// — and a rename REPLACES any symlink planted at `dst` rather than following it.
/// This matches vault.pmv's own write discipline; using `fs::copy` here would follow
/// a planted symlink and redirect the (encrypted) write + chmod to an arbitrary file.
pub(super) fn write_bytes_atomic(dst: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    // Fault point (crash-test only): abort/ENOSPC while writing a bak generation.
    crate::fault::point("redundancy.bak").map_err(VaultError::from)?;
    let tmp = sibling_tmp(dst)?;
    if let Err(e) = write_new_file(&tmp, bytes, &[]) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, dst).map_err(VaultError::from) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent_dir(dst);
    Ok(())
}

/// Remove stale `*.tmp` siblings left by a crash mid atomic-write — `.vault.pmv*.tmp`
/// (primary/mirror/bak temps) in the vault dir and `.manifest*.tmp` in `manifest/` —
/// AND any orphaned `.<name>.old` directory trees from a rekey. Best-effort, writable
/// opens only. The temps are encrypted (no plaintext leak), but sweeping keeps the
/// directory tidy and avoids OLD-KEY material lingering after a rekey.
pub(super) fn sweep_stale_temps(dir: &Path) {
    let sweep = |d: &Path, prefix: &str| {
        if let Ok(rd) = fs::read_dir(d) {
            for entry in rd.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(prefix) && name.ends_with(".tmp") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    };
    sweep(dir, &format!(".{VAULT_FILE}")); // .vault.pmv* / .vault.pmv.mirror* / .vault.pmv.bakN*
    sweep(&dir.join("manifest"), ".manifest"); // .manifest.N* manifest-commit temps
    // `.last_update_<ts>.<rand>.tmp` temps leaked by `touch_last_update` if a crash lands between
    // its fsync and rename. The live marker (`last_update_<ts>`, no leading dot, no `.tmp`) is never
    // matched, so only orphaned temps are reaped — otherwise they would accumulate across crashes.
    sweep(dir, ".last_update_");
    // Reap orphaned `.volume.old` / `.manifest.old` trees. `replace_dir`'s OWN cleanup of
    // its `.old` sibling runs at the START of the next commit, but only on a RE-ENTRANT
    // rekey (staging still present). If the trailing best-effort `remove_dir_all` failed
    // AFTER the rekey fully committed (staging gone), recover_pending_rekey returns early on
    // later opens and replace_dir is never re-entered — so an `.old` dir full of OLD-KEY
    // ciphertext would linger forever, defeating change_password's forward secrecy. Reaping
    // it on every writable open closes that gap. The live dirs (`volume`/`manifest`) have no
    // `.old` suffix, so this can never touch them.
    for sub in ["volume", "manifest"] {
        let _ = fs::remove_dir_all(sibling_old(&dir.join(sub)));
    }
}

/// Remove every retained generation numbered above `depth` (e.g. after the depth is
/// lowered), so the on-disk generation count never exceeds the configured retention.
pub(super) fn prune_generations_above(primary: &Path, depth: u32) {
    for k in (depth.min(MAX_REDUNDANCY) + 1)..=MAX_REDUNDANCY {
        let _ = fs::remove_file(bak_path(primary, k));
    }
}

/// Ring the outgoing generation (`prev_bytes` — the just-replaced `vault.pmv`) into
/// the ring: drop the oldest, shift the rest down, write `prev_bytes` as `bak1`
/// (atomic + symlink-safe), then prune any slot beyond `depth`. Called AFTER the new
/// primary has committed, so a failed save never disturbs the ring. Best-effort (a
/// partial/odd copy is skipped on recovery, since each is AEAD-validated when used).
pub(super) fn rotate_generations(primary: &Path, depth: u32, prev_bytes: &[u8]) {
    let depth = depth.min(MAX_REDUNDANCY);
    if depth == 0 {
        return;
    }
    // Fault point (crash-test only): abort mid ring-rotation — AFTER the authoritative
    // primary commit — to prove the primary still opens (the ring is best-effort).
    let _ = crate::fault::point("redundancy.rotate");
    let _ = fs::remove_file(bak_path(primary, depth)); // the oldest falls off the ring
    for k in (1..depth).rev() {
        let from = bak_path(primary, k);
        if from.exists() {
            let _ = fs::rename(&from, bak_path(primary, k + 1)); // bak{k} -> bak{k+1}
        }
    }
    let _ = write_bytes_atomic(&bak_path(primary, 1), prev_bytes); // outgoing -> bak1
    prune_generations_above(primary, depth);
    // Make the whole ring shift (renames + drop + prune) durable as a unit. (The bak1
    // write already fsync'd the dir, but the prune removals after it had not been; one
    // fsync here covers them so a power loss can't resurrect a pruned generation.)
    sync_parent_dir(&bak_path(primary, 1));
}

/// Remove every redundant copy (mirror + all generations). Safe to call on every
/// non-redundant save. The fast-path no-op (the common default, when no copies
/// exist) keys on `redundancy_candidates` so it can never skip an orphaned
/// higher-numbered generation — it returns only when there is genuinely nothing to
/// remove (each `remove_file` on a non-existent path is itself a cheap ENOENT).
pub(super) fn cleanup_redundancy(primary: &Path) {
    if redundancy_candidates(primary).is_empty() {
        return;
    }
    let _ = fs::remove_file(mirror_path(primary));
    for k in 1..=MAX_REDUNDANCY {
        let _ = fs::remove_file(bak_path(primary, k));
    }
}

/// Existing redundant copies in recovery-preference order: mirror (same generation,
/// no data loss) first, then prior generations newest-first.
pub(super) fn redundancy_candidates(primary: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let m = mirror_path(primary);
    if m.exists() {
        out.push(m);
    }
    for k in 1..=MAX_REDUNDANCY {
        let b = bak_path(primary, k);
        if b.exists() {
            out.push(b);
        }
    }
    out
}

/// Defensively remove any redundancy copy (mirror / `bakN`) whose header salt does NOT
/// match the live primary's salt — a cross-epoch leftover from a password change whose
/// best-effort [`cleanup_redundancy`] partially failed (audit F3). The salt changes only
/// on rekey and is authenticated AAD, so an old-epoch copy was written under a PREVIOUS
/// password's salt: it cannot decode under the current password, is therefore useless for
/// recovery, and only lingers as old-key plaintext-equivalent ciphertext at rest — a
/// forward-secrecy leftover after a password change. Removing it can never drop a
/// recovery-useful copy (recovery only ever uses a copy that decodes under the current
/// password, i.e. the current salt). Same defensive posture as the stale-temp / `.old`-dir
/// sweeps; only runs on a writable open. A copy whose header is unreadable is LEFT ALONE —
/// we never delete what we cannot positively classify as foreign (a salt-damaged copy is
/// already unrecoverable and harmless).
pub(super) fn sweep_foreign_epoch_copies(primary: &Path, current_salt: &[u8; SALT_LEN]) {
    for c in redundancy_candidates(primary) {
        if let Ok(h) = read_header_of(&c)
            && &h.salt != current_salt
        {
            let _ = fs::remove_file(&c);
        }
    }
}
