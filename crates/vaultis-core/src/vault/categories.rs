//! The editable category lists stored in the vault (asset types, account types and
//! subtypes): usage counts, adding, removing, and resyncing them from the records.

use super::*;

impl OpenVault {
    // Returns a *borrow* (`&TypeLists`) into the vault rather than a copy: the
    // caller may read the category lists but the data stays owned by the vault.
    pub fn categories(&self) -> &TypeLists {
        &self.vault.categories
    }

    /// How many live asset/liability records use `name` as their `asset_type`
    /// (trimmed, case-insensitive). `0` means the configured type is unused — safe to remove,
    /// and flagged as such in the Config screen. This is the single source of truth for the
    /// "in use?" question (`remove_asset_type` uses it too). Matching is trimmed on BOTH sides
    /// so it keys off the same normalized value `add_*`/`sync_types_from_records` store — a
    /// whitespace-padded record value (legacy/imported data) still counts as in use.
    pub fn asset_type_usage(&self, name: &str) -> usize {
        let name = name.trim();
        self.vault.assets.iter().filter(|a| a.asset_type.trim().eq_ignore_ascii_case(name)).count()
    }

    /// How many live accounts use `name` as their `account_type` (trimmed, case-insensitive).
    /// `0` means the configured type is unused. Shared with `remove_account_type`.
    pub fn account_type_usage(&self, name: &str) -> usize {
        let name = name.trim();
        self.vault.accounts.iter().filter(|a| a.account_type.trim().eq_ignore_ascii_case(name)).count()
    }

    /// How many live accounts use the (`type_name`, `subtype`) pair (trimmed, case-insensitive).
    /// `0` means the configured subtype is unused. Shared with `remove_account_subtype`.
    pub fn account_subtype_usage(&self, type_name: &str, subtype: &str) -> usize {
        let (type_name, subtype) = (type_name.trim(), subtype.trim());
        self.vault
            .accounts
            .iter()
            .filter(|a| {
                a.account_type.trim().eq_ignore_ascii_case(type_name)
                    && a.account_subtype.trim().eq_ignore_ascii_case(subtype)
            })
            .count()
    }

    pub fn add_asset_type(&mut self, name: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_asset_type(name))
    }

    pub fn add_account_type(&mut self, name: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_account_type(name))
    }

    pub fn add_account_subtype(&mut self, type_name: &str, subtype: &str) -> Result<bool, VaultError> {
        self.mutate_categories(|c| c.add_account_subtype(type_name, subtype))
    }

    /// Scan every record and add any asset/account **type** + account **subtype** it uses that
    /// is missing from the editable category lists (§4.2), so types brought in by a merge,
    /// `import-tree`, or older data show up in Config and the dropdowns. Returns the number of
    /// category entries added; a no-op returns `Ok(0)` without writing. Requires `--write`.
    ///
    /// **Purely additive**: this only inserts missing entries — it NEVER deletes a configured
    /// type or subtype, including ones no record currently uses. (Removal is a deliberate,
    /// per-entry action via `remove_*`.) This is what makes it safe to run automatically at
    /// vault open.
    pub fn sync_types_from_records(&mut self) -> Result<usize, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Snapshot the category list + audit length so a save failure rolls the in-memory
        // state back to match disk. Sync now runs automatically at open, so it must be
        // all-or-nothing: never leave memory holding types the persisted vault doesn't.
        let cats_before = self.vault.categories.clone();
        let audit_len = self.vault.audit.len();
        let mut added = 0usize;
        // Snapshot the type strings first so the immutable record borrow is released before the
        // category lists are mutated (the `add_*` are case-insensitive dedup).
        let asset_types: Vec<String> = self.vault.assets.iter().map(|a| a.asset_type.clone()).collect();
        for t in &asset_types {
            // Sanitize with display_safe BEFORE the type enters the category list. A record's
            // type field can be UNTRUSTED (it arrived via merge or import_tree), and this sync
            // runs automatically on every writable open — so without this, a bidi/zero-width-
            // spoofed record type would be re-injected RAW here, silently undoing the exact
            // sanitization apply_merge_from does (idempotent + a no-op for normal names).
            let t = records::display_safe(t.trim());
            if !t.is_empty() && self.vault.categories.add_asset_type(&t) {
                added += 1;
            }
        }
        let accts: Vec<(String, String)> =
            self.vault.accounts.iter().map(|a| (a.account_type.clone(), a.account_subtype.clone())).collect();
        for (t, st) in &accts {
            let t = records::display_safe(t.trim()); // sanitize untrusted record type, as above
            if t.is_empty() {
                continue;
            }
            if self.vault.categories.add_account_type(&t) {
                added += 1;
            }
            let st = records::display_safe(st.trim());
            if !st.is_empty() && self.vault.categories.add_account_subtype(&t, &st) {
                added += 1;
            }
        }
        if added > 0 {
            self.vault.audit.push(records::Change::new("types_synced", format!("{added} category type(s) added from records")));
            if let Err(e) = self.save() {
                // Roll the in-memory additions (and the audit entry) back so memory matches
                // the unchanged on-disk vault.
                self.vault.categories = cats_before;
                self.vault.audit.truncate(audit_len);
                return Err(e);
            }
        }
        Ok(added)
    }

    /// Delete an Asset/Liability type — only if **no live asset/liability record**
    /// still has that `asset_type`. (History never blocks: a `Change.detail` string
    /// is not the `asset_type` field, so it is not scanned here.) See [`CategoryRemoval`].
    pub fn remove_asset_type(&mut self, name: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let used = self.asset_type_usage(name);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_asset_type(name))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    /// Delete an account type — refused if it still has **subtypes defined**
    /// (delete those first) or if any **live account** still has that `account_type`.
    pub fn remove_account_type(&mut self, name: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        // Block while subtypes exist (chosen policy): the user removes each subtype
        // first, then the now-empty type.
        if !self.vault.categories.subtypes_for(name).is_empty() {
            return Ok(CategoryRemoval::HasSubtypes);
        }
        let used = self.account_type_usage(name);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_account_type(name))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    /// Delete a subtype under an account type — only if **no live account** has that
    /// (`account_type`, `account_subtype`) pair.
    pub fn remove_account_subtype(&mut self, type_name: &str, subtype: &str) -> Result<CategoryRemoval, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        let used = self.account_subtype_usage(type_name, subtype);
        if used > 0 {
            return Ok(CategoryRemoval::InUse(used));
        }
        let removed = self.mutate_categories(|c| c.remove_account_subtype(type_name, subtype))?;
        Ok(if removed { CategoryRemoval::Removed } else { CategoryRemoval::NotFound })
    }

    // Shared helper for the three `add_*` methods above. `edit: impl FnOnce(...)`
    // accepts any closure (here `|c| c.add_*(...)`) that takes an exclusive borrow
    // of the category lists and returns whether it actually changed something.
    // `FnOnce` means the closure is callable at least once. This is the generics +
    // higher-order-function pattern: behavior is passed in as a parameter.
    pub(super) fn mutate_categories(&mut self, edit: impl FnOnce(&mut TypeLists) -> bool) -> Result<bool, VaultError> {
        if self.read_only {
            return Err(VaultError::ReadOnly);
        }
        if edit(&mut self.vault.categories) { // run the closure; only persist if it changed state
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
