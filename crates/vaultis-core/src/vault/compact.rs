//! Compaction: reclaiming dead volume space and trimming history, plus the reachability
//! scan (`referenced_doc_ids`) that decides which documents are still live.

use super::*;

impl OpenVault {
    /// Reclaim space without changing the passwords. `opts.volume` rewrites the
    /// document store keeping only live blobs (dropping the dead frames left by
    /// updates/deletes), reusing the crash-safe staged rewrite above. `opts.json`
    /// trims each record's per-edit `history` (older than the cutoff, or all),
    /// leaving the vault-level `audit` intact and appending a `compacted` event.
    /// Either or both may run; refused on a read-only handle. Returns a report of
    /// what was reclaimed.
    pub fn compact(&mut self, opts: &CompactOptions) -> Result<CompactReport, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Measure reclaimable garbage and removable history BEFORE mutating, so the
        // report reflects the change (the staged rewrite reproduces live frames at
        // their original size, so committed-after ≈ live-before).
        let (committed, live) = self.storage.space_stats();
        let bytes_reclaimed = if opts.volume { committed.saturating_sub(live) } else { 0 };
        let history_removed = if opts.json {
            records::history_stats(&self.vault, opts.history_cutoff, opts.drop_all_history)
        } else {
            0
        };
        let partitions_before = self.storage.partition_count();
        let detail = compaction_detail(opts, bytes_reclaimed, history_removed);

        if opts.volume {
            // Re-pack the volume AND (optionally) trim history in one atomic commit.
            // The closure captures only Copy values + the owned `detail` string, so
            // it does not borrow `self` (no conflict with `&mut self`).
            let (cutoff, drop_all, do_json) = (opts.history_cutoff, opts.drop_all_history, opts.json);
            self.staged_rewrite(None, move |v| {
                if do_json {
                    records::compact_history(v, cutoff, drop_all);
                }
                v.audit.push(Change::new("compacted", detail));
            })?;
        } else {
            // JSON-only: trim history in place, then the normal atomic vault save
            // (which bumps the generation). The volume is untouched.
            records::compact_history(&mut self.vault, opts.history_cutoff, opts.drop_all_history);
            self.vault.audit.push(Change::new("compacted", detail));
            self.save()?;
        }

        Ok(CompactReport {
            bytes_reclaimed,
            history_removed,
            partitions_before,
            partitions_after: self.storage.partition_count(),
        })
    }

    /// Compute what `compact` *would* reclaim without writing anything (used by
    /// `--dry-run`; safe on a read-only handle). `partitions_after` mirrors the
    /// current count — the post-compaction count is only known after a real run.
    pub fn compact_dry_run(&self, opts: &CompactOptions) -> CompactReport {
        let (committed, live) = self.storage.space_stats();
        CompactReport {
            bytes_reclaimed: if opts.volume { committed.saturating_sub(live) } else { 0 },
            history_removed: if opts.json {
                records::history_stats(&self.vault, opts.history_cutoff, opts.drop_all_history)
            } else {
                0
            },
            partitions_before: self.storage.partition_count(),
            partitions_after: self.storage.partition_count(),
        }
    }
}

/// Options for [`OpenVault::compact`]. `volume` re-packs the document store
/// (drops dead frames); `json` trims each record's per-edit history. When
/// `drop_all_history` is false, `history_cutoff` (Unix seconds) keeps entries
/// with `at >= cutoff` and drops older ones; when true, all history is removed.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactOptions {
    pub volume: bool,
    pub json: bool,
    pub history_cutoff: Option<i64>,
    pub drop_all_history: bool,
}

/// What a compaction reclaimed. Also returned by `compact_dry_run` as a
/// pre-flight estimate (its `partitions_after` mirrors `partitions_before`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactReport {
    pub bytes_reclaimed: u64,
    pub history_removed: usize,
    pub partitions_before: usize,
    pub partitions_after: usize,
}

/// One-line summary of a compaction run, recorded in the vault `audit` log.
pub(super) fn compaction_detail(opts: &CompactOptions, bytes_reclaimed: u64, history_removed: usize) -> String {
    let mode = match (opts.volume, opts.json) {
        (true, true) => "volume+history",
        (true, false) => "volume",
        (false, true) => "history",
        (false, false) => "noop",
    };
    format!("{mode}: reclaimed {bytes_reclaimed} bytes, removed {history_removed} history entries")
}

/// Doc ids referenced by any record (Trust&Will `file`, Asset `statement`, every
/// Taxes filing's and Real Estate property's `documents`, and each General
/// Document's `file`).
pub(super) fn referenced_doc_ids(vault: &Vault) -> Vec<String> {
    let mut ids = Vec::new();
    // `for t in &vault.trust_wills` iterates by shared reference (doesn't consume
    // the vault's vector). `if let Some(f) = &t.file` runs the body only when the
    // optional field holds a value, binding the inner id to `f`. `.clone()` because
    // `f` is borrowed but we need an owned `String` in the result list.
    for t in &vault.trust_wills {
        if let Some(f) = &t.file {
            ids.push(f.clone());
        }
    }
    for a in &vault.assets {
        if let Some(f) = &a.statement {
            ids.push(f.clone());
        }
    }
    // Taxes tab: every document attached to a filing year is referenced, so
    // compaction (`--volume`) never reclaims a tax document.
    for t in &vault.tax_filings {
        for f in &t.documents {
            ids.push(f.clone());
        }
    }
    // Real Estate documents (deeds, policies, statements) are referenced too, so
    // compaction (`--volume`) never reclaims them.
    for re in &vault.real_estate {
        for f in &re.documents {
            ids.push(f.clone());
        }
    }
    // General Documents each reference a single attached file.
    for g in &vault.general_documents {
        if let Some(f) = &g.file {
            ids.push(f.clone());
        }
    }
    ids
}
