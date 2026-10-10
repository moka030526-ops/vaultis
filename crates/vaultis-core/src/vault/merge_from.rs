//! Cross-vault merge: "update this vault from another vault" — planning the patch
//! between two open vaults and applying it (see [`crate::merge`]).

use super::*;

impl OpenVault {
    // --- Cross-vault merge: "update this vault from another vault" ------------
    //
    // A one-way, ADDITIVE pull: records that are newer (by `updated_at`) or entirely
    // new in `source` are copied into `self`, along with the document blobs they
    // reference. Nothing in `self` is ever deleted. See `crate::merge` for the
    // semantics and `docs/DESIGN.md` for the security/crash-safety rationale.

    /// Compute the patch that [`apply_merge_from`](Self::apply_merge_from) would apply,
    /// for previewing. Read-only: touches no files and mutates nothing. `source` must be
    /// a *separate* already-open vault (opened with its own two passwords).
    ///
    /// A record is selected when its id is absent from `self` (New) or its `updated_at`
    /// is strictly greater than the same-id record in `self` (Updated). A selected record
    /// whose referenced document cannot be safely resolved — tombstoned in `self`, missing
    /// from `source`, or carrying an unsafe id/path — is reported in `skipped` and NOT
    /// applied (so the merge can never brick the vault or resurrect a deleted-then-garbage
    /// frame). Every displayed path is validated control/bidi-safe.
    pub fn plan_merge_from(&self, source: &OpenVault) -> Result<crate::merge::MergePlan, VaultError> {
        use std::collections::BTreeMap;
        // The source is UNTRUSTED: its `vault.id` is AEAD-authenticated but attacker-chosen,
        // and it is rendered in the preview AND recorded in this vault's audit log. Apply the
        // same allowlist `import_tree` uses (short ASCII-alphanumeric) so a crafted source
        // can't inject control/bidi bytes into the UI or persist them into our audit.
        let sid = &source.vault.id;
        if sid.is_empty() || sid.len() > 64 || !sid.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe vault id in merge source: {sid:?}"))));
        }
        let mut plan = crate::merge::MergePlan { source_vault_id: source.vault.id.clone(), ..Default::default() };
        // Dedup the blob plan by id (a doc can be referenced by several records).
        let mut blobs: BTreeMap<String, crate::merge::PlannedBlob> = BTreeMap::new();

        // Resolve every doc id a selected record references. Returns `Err(reason)` if the
        // record must be blocked, else `Ok(())` having recorded each doc in `blobs`.
        // `&mut blobs` is threaded in because a closure can't capture it while `self` is
        // also borrowed by the outer iteration.
        let resolve = |this: &OpenVault,
                       docs: &[String],
                       blobs: &mut BTreeMap<String, crate::merge::PlannedBlob>|
         -> Result<(), String> {
            for id in docs {
                if !is_safe_blob_id(id) {
                    return Err("references a document with an unsafe id".into());
                }
                if this.vault.deleted_docs.iter().any(|t| t == id) {
                    // Tombstoned here: a lingering deleted frame may still exist, so
                    // re-adding the same id in place would risk a duplicate frame (R-8).
                    return Err("references a document deleted in this vault (compact to unblock)".into());
                }
                if this.storage.contains(id) {
                    blobs.entry(id.clone()).or_insert(crate::merge::PlannedBlob {
                        id: id.clone(),
                        path: this.storage.entry(id).map(|e| e.path.clone()).unwrap_or_default(),
                        size: this.storage.entry(id).map(|e| e.size).unwrap_or(0),
                        already_present: true,
                    });
                    continue;
                }
                // Must be copied from the source — it has to be there (source opened
                // consistent) and carry a safe, displayable path.
                match source_entry_validated(source, id)? {
                    Some((path, size)) => {
                        blobs.entry(id.clone()).or_insert(crate::merge::PlannedBlob {
                            id: id.clone(),
                            path,
                            size,
                            already_present: false,
                        });
                    }
                    None => return Err("references a document missing from the source vault".into()),
                }
            }
            Ok(())
        };

        // One closure per collection, run via the generic `plan_collection` helper so the
        // recency diff + blocked-record handling is written once. `docs_of` extracts the
        // blob ids a record references (empty for Instruction/Account).
        self.plan_collection(crate::merge::RecordKind::Urgent, &self.vault.urgent, &source.vault.urgent, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Instruction, &self.vault.instructions, &source.vault.instructions, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::TrustWill, &self.vault.trust_wills, &source.vault.trust_wills, |r| r.file.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Asset, &self.vault.assets, &source.vault.assets, |r| r.statement.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::Account, &self.vault.accounts, &source.vault.accounts, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::RealEstate, &self.vault.real_estate, &source.vault.real_estate, |r| r.documents.clone(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::TaxFiling, &self.vault.tax_filings, &source.vault.tax_filings, |r| r.documents.clone(), &resolve, &mut blobs, &mut plan)?;
        self.plan_collection(crate::merge::RecordKind::GeneralDocument, &self.vault.general_documents, &source.vault.general_documents, |r| r.file.iter().cloned().collect(), &resolve, &mut blobs, &mut plan)?;
        // Zakat entries carry no documents, so `docs_of` is empty (like Instruction/Account).
        self.plan_collection(crate::merge::RecordKind::Zakat, &self.vault.zakat, &source.vault.zakat, |_r| Vec::new(), &resolve, &mut blobs, &mut plan)?;

        plan.blobs = blobs.into_values().collect();

        // Reconcile category TYPES: collect the asset/account types + subtypes the to-apply
        // records use that this vault's editable lists lack. Without this, a merged record's
        // type wouldn't appear in Config or the dropdowns. Read-only here; `apply` adds them.
        let cats = &self.vault.categories;
        let mut seen_cat: std::collections::HashSet<String> = std::collections::HashSet::new();
        let accepted_ids = |kind: crate::merge::RecordKind| -> std::collections::HashSet<&str> {
            plan.records.iter().filter(|r| r.kind == kind).map(|r| r.id.as_str()).collect()
        };
        let asset_ids = accepted_ids(crate::merge::RecordKind::Asset);
        // Only the FIRST source occurrence of an accepted id is actually applied
        // (merge_records is first-occurrence-wins), so a later DUPLICATE id carrying a
        // different type must not seed a phantom category that no applied record uses.
        let mut done_assets: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.assets {
            if !asset_ids.contains(a.id.as_str()) || !done_assets.insert(a.id.as_str()) {
                continue;
            }
            // Sanitize the UNTRUSTED source type with display_safe UP FRONT, then make the
            // existence + dedup decisions on that SAME value apply_merge_from stores — otherwise
            // two raw types that sanitize equal, or a raw type that sanitizes to an existing
            // category, make the previewed new-category count drift from what apply actually adds
            // (and a crafted source vault must never inject bidi/escape chars into the screen the
            // user authorizes). `to_ascii_lowercase` matches the apply-time `eq_ignore_ascii_case`.
            let t = records::display_safe(a.asset_type.trim());
            if !t.is_empty()
                && !cats.asset.iter().any(|x| x.eq_ignore_ascii_case(&t))
                && seen_cat.insert(format!("a\u{1f}{}", t.to_ascii_lowercase()))
            {
                plan.new_categories.push(format!("asset type \u{201c}{t}\u{201d}"));
            }
        }
        let acct_ids = accepted_ids(crate::merge::RecordKind::Account);
        let mut done_accounts: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.accounts {
            if !acct_ids.contains(a.id.as_str()) || !done_accounts.insert(a.id.as_str()) {
                continue; // first-occurrence-wins, like the asset loop above
            }
            let t = records::display_safe(a.account_type.trim()); // sanitized up front, as above
            if !t.is_empty() {
                if !cats.account.iter().any(|x| x.name.eq_ignore_ascii_case(&t))
                    && seen_cat.insert(format!("c\u{1f}{}", t.to_ascii_lowercase()))
                {
                    plan.new_categories.push(format!("account type \u{201c}{t}\u{201d}"));
                }
                let st = records::display_safe(a.account_subtype.trim());
                if !st.is_empty()
                    && !cats.subtypes_for(&t).iter().any(|x| x.eq_ignore_ascii_case(&st))
                    && seen_cat.insert(format!("s\u{1f}{}\u{1f}{}", t.to_ascii_lowercase(), st.to_ascii_lowercase()))
                {
                    plan.new_categories.push(format!("subtype \u{201c}{st}\u{201d} under \u{201c}{t}\u{201d}"));
                }
            }
        }

        Ok(plan)
    }

    /// Generic per-collection planner: run the recency diff, then for each selected source
    /// record resolve its referenced docs; on success record a [`PlannedRecord`], on a
    /// block record a [`SkippedRecord`]. Shared by all seven collections.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_collection<R: crate::records::Record>(
        &self,
        kind: crate::merge::RecordKind,
        current: &[R],
        src: &[R],
        docs_of: impl Fn(&R) -> Vec<String>,
        resolve: &impl Fn(&OpenVault, &[String], &mut std::collections::BTreeMap<String, crate::merge::PlannedBlob>) -> Result<(), String>,
        blobs: &mut std::collections::BTreeMap<String, crate::merge::PlannedBlob>,
        plan: &mut crate::merge::MergePlan,
    ) -> Result<(), VaultError> {
        for sel in crate::merge::collection_changes(current, src) {
            let s = &src[sel.source_index];
            let docs = docs_of(s);
            // Resolve into a fresh PER-RECORD scratch map: a blocked record (resolve -> Err)
            // must not leak its docs into the committed plan's blob list. Using a small empty
            // map and `extend`-ing on success keeps the planner linear in total doc references
            // (the old `blobs.clone()` per record was O(records x accumulated-blobs); `resolve`
            // only inserts via entry().or_insert(), so the deduped union is identical).
            let mut scratch = std::collections::BTreeMap::new();
            match resolve(self, &docs, &mut scratch) {
                Ok(()) => {
                    blobs.extend(scratch);
                    plan.records.push(crate::merge::PlannedRecord {
                        kind,
                        change: sel.change,
                        id: s.id().to_string(),
                        // Sanitize the UNTRUSTED source label for display: this string is
                        // rendered into the CLI/TUI merge preview the user authorizes, so a
                        // crafted source vault must not inject terminal escapes or bidi/zero-
                        // width characters that spoof which records are being merged in.
                        label: records::display_safe(&s.label()),
                        current_updated_at: sel.current_updated_at,
                        source_updated_at: s.updated_at(),
                    });
                }
                Err(reason) => plan.skipped.push(crate::merge::SkippedRecord {
                    kind,
                    id: s.id().to_string(),
                    label: records::display_safe(&s.label()), // untrusted source label — see above
                    reason,
                }),
            }
        }
        Ok(())
    }

    /// Apply the merge from `source` into `self`: copy the needed document blobs, replace/
    /// insert the newer/new records, append a vault-level audit entry, and atomically save.
    /// Recomputes the plan internally against the live `source` (so the applied set always
    /// matches a freshly-built [`plan_merge_from`]), then commits **add-only**:
    ///
    /// 1. copy each needed blob into this vault's volume (each `storage.put` is individually
    ///    durable; an interrupted copy only leaves harmless orphan frames),
    /// 2. replace/insert the records (in memory),
    /// 3. one atomic `save()` of `vault.pmv` — the single commit point.
    ///
    /// Because nothing is ever removed or rewritten, this needs no staged multi-file commit:
    /// every referenced blob is durable *before* the `vault.pmv` that references it, so the
    /// open-time `referenced ⊆ stored` invariant always holds (a crash leaves the old vault
    /// plus harmless garbage). Requires `--write`.
    pub fn apply_merge_from(&mut self, source: &OpenVault) -> Result<crate::merge::MergeReport, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Recompute the plan against the live source — never trust a caller-held plan, and
        // skip the work entirely when there is nothing to do.
        let plan = self.plan_merge_from(source)?;
        let mut report = crate::merge::MergeReport::default();
        if plan.is_empty() {
            report.records_skipped = plan.skipped.len();
            return Ok(report);
        }

        // (1) Copy every not-already-present blob into THIS vault's volume, re-encrypted
        // under our key + vault id (a fresh nonce; never a frame byte-copy). Re-validate
        // each id/path defensively at the moment of use.
        for b in &plan.blobs {
            if b.already_present {
                continue;
            }
            if !is_safe_blob_id(&b.id) || !is_safe_doc_path(&b.path) {
                return Err(VaultError::Storage(StorageError::Corrupt(format!("unsafe document in merge source: {:?}", b.id))));
            }
            // Skip if it somehow already arrived (idempotent re-put guard: never append a
            // second frame for one id — the R-8 hazard).
            if self.storage.contains(&b.id) {
                continue;
            }
            let entry = source
                .storage
                .entry(&b.id)
                .ok_or_else(|| VaultError::Storage(StorageError::Corrupt(format!("merge source lost document {:?}", b.id))))?;
            let bytes = source.storage.read(&b.id, &source.key)?; // bounded + id/path-verified
            self.storage.put(&b.id, &entry.path, &bytes, entry.uploaded_at, &self.key)?;
            report.blobs_copied += 1;
            report.bytes_copied = report.bytes_copied.saturating_add(entry.size);
        }

        // (1b) FAIL-CLOSED, *before* mutating any record: every document the accepted records
        // reference must now be in the store (just-copied or already present). `plan.blobs`
        // is exactly that referenced set, so checking it here — rather than over the merged
        // vault after the mutation — means a storage anomaly aborts with BOTH the on-disk and
        // the in-memory vault still intact (no half-merged, never-committed state to leak).
        for b in &plan.blobs {
            if !self.storage.contains(&b.id) {
                return Err(VaultError::ArchiveMismatch);
            }
        }

        // (2) Group the accepted ids by collection, then replace/insert verbatim.
        let accepted = |kind: crate::merge::RecordKind| -> std::collections::HashSet<&str> {
            plan.records.iter().filter(|r| r.kind == kind).map(|r| r.id.as_str()).collect()
        };
        let (a0, u0) = crate::merge::merge_records(&mut self.vault.urgent, &source.vault.urgent, &accepted(crate::merge::RecordKind::Urgent));
        let (a1, u1) = crate::merge::merge_records(&mut self.vault.instructions, &source.vault.instructions, &accepted(crate::merge::RecordKind::Instruction));
        let (a2, u2) = crate::merge::merge_records(&mut self.vault.trust_wills, &source.vault.trust_wills, &accepted(crate::merge::RecordKind::TrustWill));
        let (a3, u3) = crate::merge::merge_records(&mut self.vault.assets, &source.vault.assets, &accepted(crate::merge::RecordKind::Asset));
        let (a4, u4) = crate::merge::merge_records(&mut self.vault.accounts, &source.vault.accounts, &accepted(crate::merge::RecordKind::Account));
        let (a5, u5) = crate::merge::merge_records(&mut self.vault.real_estate, &source.vault.real_estate, &accepted(crate::merge::RecordKind::RealEstate));
        let (a6, u6) = crate::merge::merge_records(&mut self.vault.tax_filings, &source.vault.tax_filings, &accepted(crate::merge::RecordKind::TaxFiling));
        let (a7, u7) = crate::merge::merge_records(&mut self.vault.general_documents, &source.vault.general_documents, &accepted(crate::merge::RecordKind::GeneralDocument));
        let (a8, u8) = crate::merge::merge_records(&mut self.vault.zakat, &source.vault.zakat, &accepted(crate::merge::RecordKind::Zakat));
        report.records_added = a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8;
        report.records_updated = u0 + u1 + u2 + u3 + u4 + u5 + u6 + u7 + u8;
        report.records_skipped = plan.skipped.len();

        // (2b) Reconcile category TYPES so the merged records' asset/account types + subtypes
        // appear in Config and the dropdowns (the lists' add_* are case-insensitive dedup, and
        // the subtype add finds the type just added above). Persisted by the single save below.
        let asset_ids = accepted(crate::merge::RecordKind::Asset);
        // Dedup by id (first-occurrence-wins) so a duplicate source id with a different
        // type can't add an orphan category type whose only "user" was the un-applied dup.
        let mut done_assets: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.assets {
            if !asset_ids.contains(a.id.as_str()) || !done_assets.insert(a.id.as_str()) {
                continue;
            }
            // Sanitize the UNTRUSTED source type with display_safe BEFORE storing it, exactly
            // as plan_merge_from did for the approval preview — otherwise the category the user
            // approved (cleaned) and the one persisted (raw, possibly bidi/zero-width-spoofed)
            // would diverge, letting a crafted source vault slip a spoofed type into the lists.
            let t = records::display_safe(a.asset_type.trim());
            if !t.is_empty() && self.vault.categories.add_asset_type(&t) {
                report.categories_added += 1;
            }
        }
        let acct_ids = accepted(crate::merge::RecordKind::Account);
        let mut done_accounts: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &source.vault.accounts {
            if !acct_ids.contains(a.id.as_str()) || !done_accounts.insert(a.id.as_str()) {
                continue; // first-occurrence-wins, like the asset loop above
            }
            let t = records::display_safe(a.account_type.trim()); // sanitized to match the preview
            if !t.is_empty() {
                if self.vault.categories.add_account_type(&t) {
                    report.categories_added += 1;
                }
                let st = records::display_safe(a.account_subtype.trim());
                if !st.is_empty() && self.vault.categories.add_account_subtype(&t, &st) {
                    report.categories_added += 1;
                }
            }
        }

        // Vault-level audit entry — counts only, no record contents or document ids.
        let short = plan.source_vault_id.get(..8).unwrap_or(plan.source_vault_id.as_str());
        self.vault.audit.push(records::Change::new(
            "merged",
            format!(
                "from vault {short}: {} new, {} updated, {} document(s) copied, {} type(s) added",
                report.records_added, report.records_updated, report.blobs_copied, report.categories_added
            ),
        ));

        // (3) The single atomic commit. The referenced⊆stored invariant was already verified
        // in (1b) before any mutation, so we only have to guard the save itself: if it fails
        // (e.g. ENOSPC), the in-memory vault now holds the merged records + audit entry but
        // the on-disk vault is still the old one — POISON the handle so a later unrelated
        // save() can never silently flush this never-committed merge (mirrors `compact`'s
        // partial-commit poisoning). The caller must reopen.
        if let Err(e) = self.save() {
            self.read_only = true;
            return Err(e);
        }
        Ok(report)
    }
}

/// Look up a blob in the merge SOURCE's store: returns its `(validated path, size)` if
/// present, `Ok(None)` if the source lacks it, or `Err(reason)` if its stored path is
/// unsafe to display/store. The id is assumed already `is_safe_blob_id`-checked by the
/// caller. Used by `plan_merge_from` to build (and gate) the blob-copy plan.
fn source_entry_validated(source: &OpenVault, id: &str) -> Result<Option<(String, u64)>, String> {
    match source.storage.entry(id) {
        None => Ok(None),
        Some(e) => {
            if !is_safe_doc_path(&e.path) {
                return Err("references a document with an unsafe path".into());
            }
            // Enforce the SAME length bound `VolumeStore::put` enforces at apply time, so an
            // over-long source path surfaces as a skipped record in the PREVIEW instead of
            // aborting the merge with a hard error after the user already approved the plan.
            if e.path.len() > crate::storage::MAX_PATH_LEN {
                return Err("references a document whose path is too long".into());
            }
            // Likewise enforce the MAX_DOC_SIZE bound `VolumeStore::put` checks at apply time.
            // `storage::read` accepts a frame slightly larger than MAX_DOC_SIZE, so a hand-crafted
            // source volume could otherwise pass this preview and then abort apply_merge_from with
            // TooLarge AFTER the user approved the plan. Surface it as a skipped preview record.
            if e.size > crate::storage::MAX_DOC_SIZE {
                return Err("references a document that is too large".into());
            }
            Ok(Some((e.path.clone(), e.size)))
        }
    }
}
