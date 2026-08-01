//! Cross-vault merge — "update the current vault from another vault".
//!
//! This module computes a **one-way, additive** patch that pulls records that are
//! *more recent* (or entirely new) from a SOURCE vault into the current (destination)
//! vault, together with the document blobs those records reference. It owns the
//! patch/plan data types and the small, pure, `Record`-generic diff/apply helpers;
//! the I/O-bound orchestration (reading the source's blobs, copying them into the
//! destination volume, and the atomic save) lives on `OpenVault` in `vault.rs`,
//! which has access to the private key/storage handles.
//!
//! ## Semantics (deliberately simple and safe)
//! * **Recency key:** each record's `updated_at`. A source record is selected iff its
//!   `id` is absent from the destination (**New**), or present with a strictly greater
//!   `updated_at` (**Updated**). Records only in the destination, or newer in the
//!   destination, are left untouched.
//! * **One-way / additive:** the merge never deletes a destination record or document.
//!   Deletions are not represented by `updated_at` (and records have no tombstones), so
//!   they are not propagated — a known, documented limitation.
//! * **Verbatim replace:** an applied record is copied from the source *as-is* (its
//!   `updated_at`, `created_at`, and full `history` preserved), NOT via `records::upsert`
//!   (which would stamp `now` and discard the source's history). This keeps a re-merge a
//!   no-op (idempotent) and faithfully reflects the source's newer version.
//! * **Blocked records:** a selected record whose referenced document is **tombstoned**
//!   in the destination (`deleted_docs`) is *skipped*, not applied — resurrecting such a
//!   blob in place would risk a duplicate volume frame (the R-8 hazard). It is surfaced in
//!   the plan's `skipped` list; compacting the destination first clears the tombstone and
//!   unblocks it.
//!
//! ## Security
//! `updated_at` is attacker-controlled in a crafted source vault, so recency is treated as
//! **advisory only**: the real authorization is the user previewing the [`MergePlan`] and
//! explicitly accepting it. Every blob id/path copied from the source is validated against
//! the same allowlists `import_tree` uses, and blobs are re-encrypted under the destination
//! key (never byte-copied). See `vault.rs` for the apply path.

use std::collections::HashSet;

use crate::records::Record;

/// Which of the record collections a planned change belongs to (used only for
/// display/grouping in the preview — the apply path matches on the concrete collections).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Urgent,
    Instruction,
    TrustWill,
    Asset,
    Account,
    RealEstate,
    TaxFiling,
    GeneralDocument,
}

impl RecordKind {
    /// A short, human-readable name for the preview.
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Urgent => "Urgent",
            RecordKind::Instruction => "Instruction",
            RecordKind::TrustWill => "Trust & Will",
            RecordKind::Asset => "Asset/Liability",
            RecordKind::Account => "Account",
            RecordKind::RealEstate => "Real Estate",
            RecordKind::TaxFiling => "Tax filing",
            RecordKind::GeneralDocument => "General document",
        }
    }
}

/// Whether an incoming record is brand new to the destination or updates an existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    New,
    Updated,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::New => "new",
            ChangeKind::Updated => "updated",
        }
    }
}

/// One planned record change, for display in the preview. Carries only the record's
/// non-secret `label` (never field contents), the change kind, and the recency
/// timestamps so the user can see *what* would be pulled and *how much newer* it is.
#[derive(Clone, Debug)]
pub struct PlannedRecord {
    pub kind: RecordKind,
    pub change: ChangeKind,
    pub id: String,
    pub label: String,
    /// The destination record's current `updated_at` (`None` when the record is New).
    pub current_updated_at: Option<i64>,
    pub source_updated_at: i64,
}

/// One document blob the merge would copy into the destination volume, for display.
#[derive(Clone, Debug)]
pub struct PlannedBlob {
    pub id: String,
    /// The blob's virtual path (already validated control/bidi-safe for display).
    pub path: String,
    pub size: u64,
    /// True when the destination already stores this blob — the copy is skipped, but it
    /// is listed so the preview can show every document an applied record depends on.
    pub already_present: bool,
}

/// A record selected by recency but **not** applied, with the reason (for transparency).
#[derive(Clone, Debug)]
pub struct SkippedRecord {
    pub kind: RecordKind,
    pub id: String,
    pub label: String,
    pub reason: String,
}

/// The full preview/patch the user accepts or rejects. Empty (`is_empty`) means the
/// current vault is already at least as new as the source for every record.
#[derive(Clone, Debug, Default)]
pub struct MergePlan {
    /// Records that will be applied (new or updated, all document dependencies resolvable).
    pub records: Vec<PlannedRecord>,
    /// Blobs referenced by the applied records, with `already_present` flagged.
    pub blobs: Vec<PlannedBlob>,
    /// Records selected by recency but blocked (e.g. depend on a locally-deleted document).
    pub skipped: Vec<SkippedRecord>,
    /// Editable **category types** (asset types, account types, account subtypes) that the
    /// applied records use but that this vault's lists lack — they will be added so the
    /// merged records' types show up in Config and the dropdowns. Human-readable descriptions.
    pub new_categories: Vec<String>,
    /// The source vault's id (non-secret) — shown in the preview and recorded in the audit log.
    pub source_vault_id: String,
}

impl MergePlan {
    /// True when nothing would be applied (no records to add or update).
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn new_count(&self) -> usize {
        self.records.iter().filter(|r| r.change == ChangeKind::New).count()
    }

    pub fn updated_count(&self) -> usize {
        self.records.iter().filter(|r| r.change == ChangeKind::Updated).count()
    }

    /// Number of blobs that will actually be copied (not already present).
    pub fn blobs_to_copy(&self) -> usize {
        self.blobs.iter().filter(|b| !b.already_present).count()
    }

    /// Total plaintext bytes that will be copied (sum of not-already-present blobs).
    pub fn bytes_to_copy(&self) -> u64 {
        self.blobs.iter().filter(|b| !b.already_present).map(|b| b.size).sum()
    }
}

/// The outcome of applying a [`MergePlan`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub records_added: usize,
    pub records_updated: usize,
    pub records_skipped: usize,
    pub blobs_copied: usize,
    pub bytes_copied: u64,
    /// Editable category types (asset/account types + subtypes) added so the merged
    /// records' types appear in Config and the dropdowns.
    pub categories_added: usize,
}

/// A source record selected by the recency diff: its index in the source collection,
/// the change kind, and the destination's current `updated_at` (for the preview).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Selected {
    pub source_index: usize,
    pub change: ChangeKind,
    pub current_updated_at: Option<i64>,
}

/// Pure recency diff over one record collection: return, for each SOURCE record that is
/// new to the destination or strictly newer than its destination namesake, its index +
/// change kind + the destination's current `updated_at`. Keyed entirely by `id`
/// (128-bit random ids; a shared id reliably denotes the same logical record).
pub(crate) fn collection_changes<R: Record>(current: &[R], source: &[R]) -> Vec<Selected> {
    let mut out = Vec::new();
    // A crafted source vault can carry two records sharing an id in one collection (genuine
    // vaults never do — ids are random 128-bit). Select each id at most ONCE (keep the first
    // occurrence) so the preview can't double-list it and `merge_records` can't apply it
    // twice (inflating the counts / inserting-then-replacing).
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, s) in source.iter().enumerate() {
        if !seen.insert(s.id()) {
            continue;
        }
        match current.iter().find(|c| c.id() == s.id()) {
            None => out.push(Selected { source_index: i, change: ChangeKind::New, current_updated_at: None }),
            Some(c) => {
                // Strictly greater: equal timestamps are treated as "no change" so a
                // re-merge of identical data is a no-op.
                if s.updated_at() > c.updated_at() {
                    out.push(Selected {
                        source_index: i,
                        change: ChangeKind::Updated,
                        current_updated_at: Some(c.updated_at()),
                    });
                }
            }
        }
    }
    out
}

/// Pure apply over one record collection: for every source record whose id is in
/// `apply_ids`, replace the destination record with the same id (preserving the source's
/// timestamps + history **verbatim**) or insert it if absent. Returns `(added, updated)`.
///
/// This is intentionally NOT `records::upsert`: upsert stamps `updated_at = now` and keeps
/// only a computed field diff, which would defeat the recency comparison on a future merge
/// and drop the source's own history. A verbatim replace keeps the merge idempotent.
pub(crate) fn merge_records<R: Record>(
    current: &mut Vec<R>,
    source: &[R],
    apply_ids: &HashSet<&str>,
) -> (usize, usize) {
    let mut added = 0;
    let mut updated = 0;
    // Apply each accepted id at most once, even if a crafted source lists it twice in this
    // collection — otherwise the second occurrence would replace the first and double-count.
    let mut done: HashSet<&str> = HashSet::new();
    for s in source {
        if !apply_ids.contains(s.id()) || !done.insert(s.id()) {
            continue;
        }
        match current.iter().position(|c| c.id() == s.id()) {
            // Replace the slot wholesale; the old record is dropped (and zeroized).
            Some(i) => {
                current[i] = s.clone();
                updated += 1;
            }
            None => {
                current.push(s.clone());
                added += 1;
            }
        }
    }
    (added, updated)
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
