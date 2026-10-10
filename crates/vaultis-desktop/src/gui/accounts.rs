//! The Accounts tab: filters, the grouped tree, the account form, and the links between
//! assets and the accounts that hold them.

use super::*;

impl GuiApp {
    /// The Accounts that pass the current filters (type/subtype/owner/review) and
    /// the username search, as `(id, label)` pairs. Extracted from the render so it
    /// can be unit-tested; the search uses [`records::matches_search`].
    pub(super) fn filtered_account_labels(&self) -> Vec<(String, String)> {
        self.vault_ref()
            .vault
            .accounts
            .iter()
            .filter(|a| self.account_passes_filters(a))
            .map(|a| (a.id.clone(), a.label()))
            .collect()
    }

    /// Whether an account passes the current Accounts filters (type/subtype/owner/
    /// title/review + the free-text search, which matches the username OR the title).
    /// Shared by the flat list and the grouped tree so both honour the same filters.
    pub(super) fn account_passes_filters(&self, a: &Account) -> bool {
        (self.acct_filter_type.is_empty() || a.account_type == self.acct_filter_type)
            && (self.acct_filter_subtype.is_empty() || a.account_subtype == self.acct_filter_subtype)
            && (self.acct_filter_owner.is_empty() || a.owner == self.acct_filter_owner)
            && (self.acct_filter_title.is_empty() || a.title == self.acct_filter_title)
            && (!self.acct_filter_review || a.review)
            // Free-text search matches the username OR the title (empty query = all).
            && (records::matches_search_soundlike(&a.username, &self.acct_search_user)
                || records::matches_search_soundlike(&a.title, &self.acct_search_user))
    }

    /// Build a fresh Account for the "New" button, pre-populated from the active
    /// Accounts filters / username search so the entry starts in the bucket the user
    /// is viewing. The filter fields are "" when unset, leaving those fields blank.
    /// Nothing is persisted — this only seeds the edit buffer.
    pub(super) fn new_account_from_filters(&self) -> Option<Account> {
        let mut a = Account::new().ok()?;
        a.title = self.acct_filter_title.clone();
        a.account_type = self.acct_filter_type.clone();
        a.account_subtype = self.acct_filter_subtype.clone();
        a.owner = self.acct_filter_owner.clone();
        a.username = self.acct_search_user.clone();
        Some(a)
    }

    /// Seed the edit buffer for a NEW account. When a record is currently open, the
    /// new entry inherits its *grouping* fields — account type, subtype, and owner —
    /// so several related logins can be entered in a row without re-picking the
    /// category each time; the identifying/secret fields (title, username, password,
    /// URL, notes) deliberately start blank. With nothing open it falls back to the
    /// active list filters (see [`Self::new_account_from_filters`]).
    pub(super) fn new_account_seeded(&self) -> Option<Account> {
        match self.edit_account.as_ref() {
            Some(cur) => {
                let mut a = Account::new().ok()?;
                a.account_type = cur.account_type.clone();
                a.account_subtype = cur.account_subtype.clone();
                a.owner = cur.owner.clone();
                Some(a)
            }
            None => self.new_account_from_filters(),
        }
    }

    /// After saving an account, move any ACTIVE field filter to the saved record's
    /// value so the entry stays visible in the filtered list (changing a filtered
    /// field follows the entry rather than hiding it). Unset filters stay unset.
    pub(super) fn sync_account_filters_to(&mut self, a: &Account) {
        if !self.acct_filter_type.is_empty() {
            self.acct_filter_type = a.account_type.clone();
        }
        if !self.acct_filter_subtype.is_empty() {
            self.acct_filter_subtype = a.account_subtype.clone();
        }
        if !self.acct_filter_owner.is_empty() {
            self.acct_filter_owner = a.owner.clone();
        }
        if !self.acct_filter_title.is_empty() {
            self.acct_filter_title = a.title.clone();
        }
        // Also relax the NON-facet constraints, or the just-saved record can still
        // vanish: clear the review-only filter if the saved record isn't flagged, and
        // clear the username search if it no longer matches the saved username.
        if self.acct_filter_review && !a.review {
            self.acct_filter_review = false;
        }
        if !self.acct_search_user.is_empty()
            && !records::matches_search_soundlike(&a.username, &self.acct_search_user)
        {
            self.acct_search_user.clear();
        }
    }

    pub(super) fn tab_accounts(&mut self, ui: &mut egui::Ui) {
        // Configured account types for the EDIT form's type dropdown (offers every
        // configured type, not just the ones currently in use).
        let type_names = self.vault_ref().categories().account_type_names();
        // Cross-filtered (faceted) options: each dropdown offers only values present
        // on accounts matching ALL the OTHER active filters. Recompute to a fixpoint,
        // auto-clearing any selection that is no longer one of its narrowed options
        // (so a stale pick never leaves the list silently empty).
        let facets = loop {
            let f = records::account_facets(
                &self.vault_ref().vault.accounts,
                &self.acct_filter_type,
                &self.acct_filter_subtype,
                &self.acct_filter_owner,
                &self.acct_filter_title,
                &self.acct_search_user,
                self.acct_filter_review,
            );
            let mut changed = false;
            if !self.acct_filter_type.is_empty() && !f.types.contains(&self.acct_filter_type) {
                self.acct_filter_type.clear();
                changed = true;
            }
            if !self.acct_filter_subtype.is_empty() && !f.subtypes.contains(&self.acct_filter_subtype) {
                self.acct_filter_subtype.clear();
                changed = true;
            }
            if !self.acct_filter_owner.is_empty() && !f.owners.contains(&self.acct_filter_owner) {
                self.acct_filter_owner.clear();
                changed = true;
            }
            if !self.acct_filter_title.is_empty() && !f.titles.contains(&self.acct_filter_title) {
                self.acct_filter_title.clear();
                changed = true;
            }
            if !changed {
                break f;
            }
        };

        // Set inside the filter row's closure when the one-off trim button is clicked;
        // handled just after so the bulk vault mutation isn't tangled in the UI borrow.
        let mut trim_all = false;
        // The filter row is a card with two labelled lines — the narrowing controls on
        // one, the view toggles on the other. Previously all eleven controls ran
        // together on a single wrapped line, where "reveal all" (which exposes every
        // password on screen) sat between two dropdowns and read like one of them.
        let accent_c = accent(self.theme);
        let filters_active = !self.acct_filter_type.is_empty()
            || !self.acct_filter_subtype.is_empty()
            || !self.acct_filter_owner.is_empty()
            || !self.acct_filter_title.is_empty()
            || self.acct_filter_review
            || !self.acct_search_user.is_empty();
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Filter").strong().small().color(accent_c));
                ui.label(egui::RichText::new("type").weak().small());
                filter_combo(ui, "acct_ftype", &mut self.acct_filter_type, &facets.types);
                ui.label(egui::RichText::new("subtype").weak().small());
                filter_combo(ui, "acct_fsub", &mut self.acct_filter_subtype, &facets.subtypes);
                ui.label(egui::RichText::new("owner").weak().small());
                filter_combo(ui, "acct_fowner", &mut self.acct_filter_owner, &facets.owners);
                ui.label(egui::RichText::new("title").weak().small());
                filter_combo(ui, "acct_ftitle", &mut self.acct_filter_title, &facets.titles);
                search_box(
                    ui,
                    &mut self.acct_search_user,
                    "username or title…",
                    // No arrow glyphs in DRAWN text: the font-coverage test exempts them as
                    // comment-only, so one here could ship as a tofu box (see
                    // `every_glyph_in_the_gui_source_exists_in_the_bundled_fonts`).
                    "Free-text search over the username and the title. The letters may appear \
                     ANYWHERE in the value, and spelling is forgiving: a name that SOUNDS like \
                     the record still matches, so \"jonson\" finds Johnson and \"catherine\" \
                     finds Katherine.",
                    accent_c,
                    180.0,
                );
                ui.checkbox(&mut self.acct_filter_review, "review only");
                // Only offer Clear when there is something to clear, and mark it when
                // filters are hiding rows — an unexplained short list is the single
                // most common "where did my records go" confusion.
                if ui.button("× Clear").on_hover_text("Reset every filter and the search box").clicked() {
                    self.acct_filter_type.clear();
                    self.acct_filter_subtype.clear();
                    self.acct_filter_owner.clear();
                    self.acct_filter_title.clear();
                    self.acct_filter_review = false;
                    self.acct_search_user.clear();
                }
                // A badge when filters are actually hiding rows — an unexplained short
                // list is the most common "where did my records go" confusion.
                if filters_active {
                    badge(ui, "filtered", egui::Color32::from_rgb(190, 105, 10));
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                // Flat filtered list ⇄ grouped tree (type → subtype → owner → title).
                option_toggle(
                    ui,
                    &mut self.acct_grouped,
                    "grouped tree",
                    "Group the list by owner > type > subtype > title",
                );
                // Global reveal: the ONLY reveal control on this screen.
                option_toggle(
                    ui,
                    &mut self.reveal_all,
                    "👁 reveal all passwords",
                    "Unmask every password on this screen. Resets when you switch tabs.",
                );
                // One-off maintenance: left/right-trim every field on every record (all tabs).
                if self.writable
                    && ui
                        .button("Trim all fields")
                        .on_hover_text("One-off: left/right-trim every field on every record in the whole vault (recorded in history)")
                        .clicked()
                {
                    trim_all = true;
                }
            });
        });
        ui.add_space(6.0);

        // Perform the one-off bulk trim (after the filter row, before the list is
        // built, so the cleaned values show this frame).
        if trim_all {
            self.trim_all_records();
        }

        // Filtered list (after the filter row, so a change applies this frame).
        let labels = self.filtered_account_labels();
        // In grouped mode, the same filtered accounts as a type→subtype→owner→title
        // tree (built here so the render closure doesn't re-borrow `self`).
        let tree = if self.acct_grouped {
            Some(records::account_tree(self.vault_ref().vault.accounts.iter().filter(|a| self.account_passes_filters(a))))
        } else {
            None
        };
        let cur = self.edit_account.as_ref().map(|r| r.id.clone());
        // Flat-list arrow navigation: when not grouped, ↑/↓ move to the prev/next item.
        let nav_target = list_nav_target(ui, !self.acct_grouped, &labels, cur.as_deref());
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut generate = false;
        // Deferred password-copy: `None` unless the user clicks copy, in which
        // case it holds the secret in a self-wiping `Zeroizing<String>`.
        let mut copy_pw: Option<Zeroizing<String>> = None;
        // Deferred plain-copy for the non-secret URL / username buttons (acted on after
        // rendering, like `copy_pw`, so the clipboard call sits outside the `self` borrow
        // held by the form). A plain `String` — these are not secrets, so no zeroizing.
        let mut copy_plain: Option<String> = None;
        // Subtypes for the record under edit, looked up from the vault's category lists
        // before the mutable borrow of `edit_account` below. The record's current subtype is
        // kept selectable even when off-list — `combo` prepends the current value, so no
        // manual prepend is needed here. `.unwrap_or_default()` yields an empty `Vec` when no
        // record is being edited.
        let subtypes: Vec<String> = self
            .edit_account
            .as_ref()
            .map(|r| self.vault_ref().categories().subtypes_for(&r.account_type))
            .unwrap_or_default();
        // "Linked from": every Asset/Liability whose `linked_accounts` references the
        // record under edit, resolved before the mutable `edit_account` borrow below
        // (same borrow discipline as `subtypes`). Informational only — links are edited
        // on the ASSET side; here each row just offers Open (navigation is a read).
        let linked_from: Vec<(String, String)> = self
            .edit_account
            .as_ref()
            .map(|r| records::assets_linking_account(&self.vault_ref().vault.assets, &r.id))
            .unwrap_or_default();
        // Deferred jump to a linking asset (its id), applied after the columns closure.
        let mut open_asset: Option<String> = None;
        // Deferred resolution of an armed linked-account delete warning (see the
        // `pending_account_delete` field): confirm proceeds with the delete, cancel disarms.
        let mut confirm_delete = false;
        let mut cancel_delete = false;

        two_col(ui, |c| {
            match &tree {
                // Grouped tree: owner → type → subtype → title (leaf), with empty
                // levels skipped. egui's CollapsingHeader gives the +/- expand control.
                Some(root) => {
                    let lp = &mut c[0];
                    // Same header as the flat `list_panel`, so switching to the tree
                    // does not change what the top of the pane looks like.
                    lp.horizontal_wrapped(|ui| {
                        let accent = ui_accent(ui);
                        section_heading(ui, "Accounts", accent);
                        badge(ui, &format!("{}", labels.len()), accent);
                        ui.add_space(4.0);
                        if self.writable && ui.button("➕ New").clicked() {
                            new = true;
                        }
                        if ui
                            .button("⬇ CSV")
                            .on_hover_text(
                                "Export every row on this tab to a timestamped CSV in the export directory.\n\
                                 The file is UNENCRYPTED and includes passwords in plain text.",
                            )
                            .clicked()
                        {
                            export = true;
                        }
                    });
                    lp.add_space(4.0);
                    lp.separator();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("acct_tree").show(lp, |ui| {
                        let mut path: Vec<String> = Vec::new();
                        if let Some(s) = render_acct_node(ui, root, &mut path, cur.as_deref(), &labels, "acct") {
                            select = Some(s);
                        }
                    });
                }
                None => {
                    (new, select, export) =
                        list_panel(&mut c[0], "Accounts", "➕ New", &labels, cur.as_deref(), self.writable, nav_target);
                }
            }
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_accounts").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_account.as_mut() {
                    let w = self.writable;
                    egui::Grid::new("acct_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Title", &mut r.title, w);
                        ui.label("Account type");
                        let prev_type = r.account_type.clone();
                        combo(ui, "acct_type", &mut r.account_type, &type_names, w);
                        if r.account_type != prev_type {
                            // Subtypes are type-specific; drop a now-mismatched subtype.
                            r.account_subtype.clear();
                        }
                        ui.end_row();
                        ui.label("Subtype");
                        combo(ui, "acct_subtype", &mut r.account_subtype, &subtypes, w);
                        ui.end_row();
                        ui.label("Owner");
                        field_singleline(ui, &mut r.owner, w, 420.0);
                        ui.end_row();
                        ui.label("Username");
                        ui.horizontal(|ui| {
                            field_singleline_with_buttons(ui, &mut r.username, w, 380.0, 1);
                            // Copy is a read, so it stays available even in read-only mode;
                            // disabled only when the field is empty (nothing to copy).
                            if ui.add_enabled(!r.username.is_empty(), egui::Button::new("📋")).on_hover_text("Copy").clicked() {
                                copy_plain = Some(r.username.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("Password");
                        ui.horizontal(|ui| {
                            // Masked unless the single global "reveal all" toggle is on (there
                            // is no per-record reveal). `secret_text_edit` (audit R-7) scrubs
                            // egui's undo buffer and re-routes the built-in copy through the
                            // history-excluded clipboard path. Read-only: the field is shown,
                            // selectable, and copyable, but not editable.
                            secret_text_edit(ui, "acct_pw", &mut r.password, self.reveal_all, w, fit_with_buttons(ui, 280.0, 2), &mut copy_pw);
                            // Generate is only useful when you can save; copy is a read.
                            if w && ui.button("🎲").on_hover_text("Generate").clicked() {
                                generate = true;
                            }
                            if ui.button("📋").on_hover_text("Copy").clicked() {
                                // Stash a self-wiping copy to act on after rendering.
                                copy_pw = Some(Zeroizing::new(r.password.clone()));
                            }
                        });
                        ui.end_row();
                        ui.label("URL");
                        ui.horizontal(|ui| {
                            field_singleline(ui, &mut r.url, w, 380.0);
                            if ui.add_enabled(!r.url.is_empty(), egui::Button::new("📋")).on_hover_text("Copy").clicked() {
                                copy_plain = Some(r.url.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("Closed as of");
                        field_singleline_hint(ui, &mut r.closed_as_of, w, 420.0, "YYYY-MM-DD");
                        ui.end_row();
                        ui.label("Review");
                        ui.add_enabled(w, egui::Checkbox::new(&mut r.review, "flag for review"));
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 4);
                    // Read-only reverse view of the asset-side links (hidden when nothing
                    // links here). Open stays available read-only — navigation is a read.
                    if !linked_from.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Linked from").strong());
                        for (id, label) in &linked_from {
                            ui.horizontal(|ui| {
                                ui.label(format!("• {label}"));
                                if ui.button("Open").clicked() {
                                    open_asset = Some(id.clone());
                                }
                            });
                        }
                    }
                    action = form_buttons(ui, self.writable);
                    // Armed by the Delete handling below when assets link this account: the
                    // warning (count + consequence) with an explicit second-click pair. The
                    // id guard keeps a warning armed for one record from ever rendering —
                    // or confirming — against another.
                    if self.pending_account_delete.as_deref() == Some(r.id.as_str())
                        && let Some(msg) = account_delete_link_warning(linked_from.len())
                    {
                        ui.colored_label(egui::Color32::from_rgb(0xC0, 0x30, 0x30), msg);
                        ui.horizontal(|ui| {
                            if ui.button("Delete anyway").clicked() {
                                confirm_delete = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel_delete = true;
                            }
                        });
                    }
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an account");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_account = self.new_account_seeded();
            // Loading a different record disarms any pending delete warning: the armed
            // id no longer matches, so leaving it set could only go stale.
            self.pending_account_delete = None;
        }
        // A pointer click on a list row (`select`) wins over keyboard nav. But a keyboard
        // arrow (`nav_target`) CAN land in the same egui frame as a button click (events
        // are batched per repaint), and the deferred actions below (Delete, confirm/cancel,
        // Generate) were captured against the CURRENTLY shown record. Applying a same-frame
        // nav swap first would retarget them at the NEIGHBOR — e.g. deleting or regenerating
        // the wrong account. So suppress the keyboard nav whenever a record-targeted action
        // is pending this frame; a click (`select`) cannot co-occur with another button
        // click (one pointer), so it is always safe to honor.
        let record_action_pending =
            new || !matches!(action, FormAction::None) || confirm_delete || cancel_delete || generate;
        if !record_action_pending {
            select = select.or(nav_target);
        }
        if let Some(i) = select {
            // `labels` is the FILTERED list, so resolve the clicked row to its id
            // and look the account up by id (a positional index into the
            // unfiltered vector would select the wrong record when filtering).
            if let Some((id, _)) = labels.get(i) {
                self.edit_account =
                    self.vault_ref().vault.accounts.iter().find(|a| &a.id == id).cloned();
                self.pending_account_delete = None; // selection change disarms (see `new` above)
            }
        }
        // Pre-size the password buffer so typing in the egui field doesn't reallocate
        // and strand un-zeroized fragments of the account secret in freed heap. The
        // Account record is ZeroizeOnDrop, but that only wipes the final buffer, not
        // the copies abandoned by per-keystroke growth. `presize_secret` is a no-op once
        // the capacity is sufficient, so this is cheap to call each frame.
        if let Some(r) = self.edit_account.as_mut() {
            presize_secret(&mut r.password);
        }
        if generate
            && let Some(r) = self.edit_account.as_mut()
        {
            // Wipe the previous candidate's bytes before dropping it: a plain
            // `String` reassignment frees the old buffer WITHOUT zeroizing, leaving a
            // prior password in freed heap. `.unwrap_or_default()` yields the new
            // password on success or an empty string on the (unexpected) error case.
            r.password.zeroize();
            r.password = password::generate(&GenOptions::default()).unwrap_or_default();
            // Reveal is global-only now: turn on "reveal all" so the just-generated
            // password is visible (the per-record reveal that used to do this is gone).
            self.reveal_all = true;
        }
        if let Some(pw) = copy_pw {
            // `pw` is moved into the call and wiped when it drops there.
            self.copy_to_clipboard(pw);
        }
        if let Some(text) = copy_plain {
            self.copy_plain(&text);
        }
        match action {
            FormAction::Save => {
                // Left/right-trim every field before persisting. Trim the live edit
                // form too, so the displayed values match what was saved.
                if let Some(r) = self.edit_account.as_mut() {
                    r.trim_fields();
                }
                // Title and owner are mandatory: refuse to save an account missing
                // either (after trimming), keeping the edit form open to fill it.
                if let Some(msg) = self.edit_account.as_ref().and_then(account_required_field_error) {
                    self.refuse(msg);
                } else {
                    if let Some(r) = self.edit_account.clone()
                        && let Some(ov) = self.vault.as_mut()
                    {
                        records::upsert(&mut ov.vault.accounts, r.clone());
                        // Keep the just-saved entry visible: move any ACTIVE filter to the
                        // saved record's value (so changing a filtered field doesn't make
                        // the entry vanish from the filtered list).
                        self.sync_account_filters_to(&r);
                    }
                    if self.persist() {
                        self.status = "Saved.".into();
                        self.sync_edit_buffer(Tab::Accounts);
                    }
                    // On failure persist() has already set the "Save failed: …" status.
                }
            }
            FormAction::Delete => {
                // Deleting a linked-from account is allowed but never silent: instead of
                // deleting, ARM the confirmation (rendered next frame — the warning text
                // + "Delete anyway"/"Cancel" above). The links are NOT cascaded, so the
                // existing delete rollback stays correct — nothing else is touched. An
                // unlinked account deletes immediately, exactly as before.
                if linked_from.is_empty() {
                    self.delete_current(Tab::Accounts);
                } else {
                    self.pending_account_delete = self.edit_account.as_ref().map(|r| r.id.clone());
                }
            }
            _ => {}
        }
        if confirm_delete {
            self.confirm_pending_account_delete();
        }
        if cancel_delete {
            self.pending_account_delete = None;
            self.status = "Delete cancelled.".into();
        }
        if let Some(id) = open_asset {
            self.open_linking_asset(&id);
        }
    }

    /// Apply a deferred linked-accounts request from the Assets form (see [`LinkReq`]).
    /// Add/Unlink edit the WORKING BUFFER only — the link list persists through the
    /// ordinary Save path with the rest of the form (never a direct vault write, so
    /// abandoning the edit discards it like any other unsaved change). Open navigates.
    /// Execute an armed "Delete anyway" confirmation for a linked-from account.
    /// Confirm-time id re-check: the render-time guard alone is NOT enough — a bare
    /// arrow-key nav event can land in the SAME egui frame as the "Delete anyway"
    /// click, and the select/nav handler runs before this, swapping `edit_account`
    /// to the neighboring record (and disarming `pending_account_delete`) after the
    /// click was captured. Without this check the raced confirm would delete the
    /// NEIGHBOR. Requiring the armed id to still match the loaded record drops such
    /// a stale confirm (the swap set pending to None, so the equality fails).
    pub(super) fn confirm_pending_account_delete(&mut self) {
        let armed_matches_current = self.pending_account_delete.is_some()
            && self.pending_account_delete.as_deref()
                == self.edit_account.as_ref().map(|r| r.id.as_str());
        self.pending_account_delete = None;
        if armed_matches_current {
            self.delete_current(Tab::Accounts);
        }
    }

    pub(super) fn handle_link_req(&mut self, req: LinkReq) {
        match req {
            LinkReq::None => {}
            LinkReq::Add(id) => {
                // The dropdown only offers not-yet-linked accounts, but the request is
                // re-checked here (deferred handling) so a duplicate can never slip in.
                if let Some(r) = self.edit_asset.as_mut()
                    && !r.linked_accounts.iter().any(|l| l == &id)
                {
                    r.linked_accounts.push(id);
                }
            }
            LinkReq::Remove(i) => {
                // Bounds-checked: the index was captured a frame ago against the same
                // buffer, but a stale/raced index must drop the request, not panic.
                if let Some(r) = self.edit_asset.as_mut()
                    && i < r.linked_accounts.len()
                {
                    r.linked_accounts.remove(i);
                }
            }
            LinkReq::Open(id) => self.open_linked_account(&id),
        }
    }

    /// Jump from an asset's link row to the linked Account: load it in the Accounts
    /// editor and switch tabs. A dangling link (the account was deleted — links are
    /// never cascaded) surfaces a status message and does NOT navigate.
    pub(super) fn open_linked_account(&mut self, id: &str) {
        let Some(a) = self.vault_ref().vault.accounts.iter().find(|a| a.id == id).cloned() else {
            self.refuse("Linked account not found — it may have been deleted.");
            return;
        };
        // A programmatic tab change bypasses ui_top_bar's prev_tab compare, so perform
        // the same switch resets here: re-mask to the saved reveal default and clear
        // the shared document-input buffers (see the reset block in `ui_top_bar`).
        self.tab = Tab::Accounts;
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.clear_doc_inputs();
        // Retarget any ACTIVE Accounts filters/search to the jump target so the list
        // can't hide the record we just navigated to (same rule as the after-save follow).
        self.sync_account_filters_to(&a);
        self.edit_account = Some(a);
    }

    /// Jump from an account's "Linked from" row to the Asset/Liability linking it —
    /// the reverse of [`Self::open_linked_account`], with the same programmatic
    /// tab-switch resets. The row list is rebuilt from the vault each frame, but the
    /// id is still re-resolved here (deferred handling) rather than trusted.
    pub(super) fn open_linking_asset(&mut self, id: &str) {
        let Some(r) = self.vault_ref().vault.assets.iter().find(|r| r.id == id).cloned() else {
            self.refuse("Linked record not found — it may have been deleted.");
            return;
        };
        self.tab = Tab::Assets;
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.clear_doc_inputs();
        // The Assets list's only hiding filter is the review-only toggle: clear it when
        // it would hide the jump target (mirrors the Accounts-side filter follow).
        if self.asset_filter_review && !r.review {
            self.asset_filter_review = false;
        }
        self.edit_asset = Some(r);
    }
}

/// A two-column "label + single-line edit" row inside a Grid.
// `value: &mut String` lets the text widget write the user's edits straight back
// into the caller's field.
/// Validate a to-be-saved account, returning the user-facing error for the first
/// missing mandatory field (title, then owner) or `None` when it may be saved. The
/// GUI save path and its tests share this so the rule lives in exactly one place.
pub(super) fn account_required_field_error(a: &Account) -> Option<&'static str> {
    if a.title.trim().is_empty() {
        Some("Title is required — every account must have a title.")
    } else if a.owner.trim().is_empty() {
        Some("Owner is required — every account must have an owner.")
    } else {
        None
    }
}

/// The warning shown before deleting an account that assets/liabilities still link to:
/// states the linked-from count and the consequence — the links are KEPT (no cascade,
/// per the additive/no-silent-loss policy) and will render as unresolved raw ids.
/// `None` when nothing links to the account, in which case delete proceeds unwarned
/// exactly as before. Shared by the form's warning banner and its tests.
pub(super) fn account_delete_link_warning(linked_from: usize) -> Option<String> {
    if linked_from == 0 {
        return None;
    }
    Some(format!(
        "This account is linked from {linked_from} asset/liability record(s). Deleting it will NOT \
         remove those links — they will show as unresolved ids."
    ))
}

/// Deferred linked-accounts action gathered while rendering the Assets form (see
/// [`linked_accounts_section`]). `Add`/`Open` carry an Account id; `Remove` carries the
/// index into the asset's `linked_accounts` list. Applied after the columns closure
/// like every other deferred request, so mutation/navigation stays outside the form
/// borrow. Not `Copy` (unlike [`DocSectionReq`]) — two variants own a `String`.
#[derive(PartialEq, Eq, Clone)]
pub(super) enum LinkReq {
    None,
    Add(String),
    Remove(usize),
    Open(String),
}

/// Resolve an asset's linked-account ids to display rows `(id, resolved label)`. A
/// dangling id (the account was deleted — links are never cascaded) resolves to the
/// RAW id: tolerant and nothing hidden, per the additive/no-silent-loss policy.
pub(super) fn linked_account_rows(accounts: &[Account], linked: &[String]) -> Vec<(String, String)> {
    linked
        .iter()
        .map(|id| (id.clone(), records::account_label(accounts, id).unwrap_or_else(|| id.clone())))
        .collect()
}

/// The accounts offered by the Assets form's "add link" dropdown: every account NOT
/// already linked (a second link to the same account would be meaningless).
pub(super) fn link_candidates(accounts: &[Account], linked: &[String]) -> Vec<(String, String)> {
    accounts
        .iter()
        .filter(|a| !linked.iter().any(|id| id == &a.id))
        .map(|a| (a.id.clone(), a.label()))
        .collect()
}

/// The "Linked accounts" section of the Asset/Liability form (modeled on
/// [`doc_section`]): one row per link — Open always (navigation is a read, kept in
/// read-only mode), Unlink writable-only (it edits the record) — plus, when writable,
/// an "add link" dropdown over `candidates`. `linked` comes from
/// [`linked_account_rows`], `candidates` from [`link_candidates`]. The caller applies
/// the returned request after rendering, keeping `self` borrows disjoint.
/// The link dropdown's visible entries for `query`: the `(id, label)` candidates whose LABEL
/// matches, by the same rule as the Accounts search box ([`records::matches_search_soundlike`])
/// — the letters may appear anywhere in the label (no prefix/suffix anchoring), and a
/// sound-alike spelling still matches. An empty query keeps every candidate, in the order the
/// caller supplied. Split out of the popup so the filtering is unit-testable without driving
/// egui's combo popup.
pub(super) fn filter_link_candidates<'a>(candidates: &'a [(String, String)], query: &str) -> Vec<&'a (String, String)> {
    candidates.iter().filter(|(_, label)| records::matches_search_soundlike(label, query)).collect()
}

pub(super) fn linked_accounts_section(
    ui: &mut egui::Ui,
    linked: &[(String, String)],
    candidates: &[(String, String)],
    query: &mut String,
    writable: bool,
) -> LinkReq {
    let mut req = LinkReq::None;
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    card(ui, |ui| {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("🔗 Linked accounts").strong().color(accent));
        ui.label(egui::RichText::new("the logins that hold or service this").weak().small());
    });
    ui.add_space(6.0);
    if linked.is_empty() {
        ui.label(egui::RichText::new("No linked accounts.").weak().italics());
    }
    for (i, (id, label)) in linked.iter().enumerate() {
        egui::containers::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.label(egui::RichText::new("🔑").color(accent));
                ui.add(egui::Label::new(label).truncate()).on_hover_text(label);
            },
            |ui| {
                if writable && ui.button("Unlink").on_hover_text("Remove this link (the account itself is untouched)").clicked() {
                    req = LinkReq::Remove(i);
                }
                if ui.button("Open").on_hover_text("Jump to this account on the Accounts tab").clicked() {
                    req = LinkReq::Open(id.clone());
                }
            },
        );
    }
    if writable {
        ui.add_space(4.0);
        // Hand-rolled (id, label) dropdown: the shared `combo`/`filter_combo` helpers
        // bind a &mut String VALUE from a String list, but a link stores the account's
        // ID while showing its LABEL — so there is no bound buffer; a click on an entry
        // just emits the Add request (nothing is "currently selected").
        //
        // The popup opens with a SEARCH box: a vault with dozens of accounts made this a long
        // scroll where the user had to recognise the right login by eye. Typing narrows the
        // list to the matching accounts (which brings the wanted one to the top, right under
        // the cursor) using the same forgiving rule as the Accounts search — the letters may
        // appear ANYWHERE in the label, not just at its start, and a sound-alike spelling
        // still matches. The list scrolls inside a bounded area so a big vault's popup can
        // never grow taller than the window.
        let out = egui::ComboBox::from_id_salt("asset_link_add")
            .selected_text("➕ Link an account…")
            .show_ui(ui, |ui| {
                let sb = search_box(
                    ui,
                    query,
                    "type to find an account…",
                    "Filters the accounts below. The letters may appear anywhere in the \
                     account's label, and a name that SOUNDS like it still matches.",
                    accent,
                    220.0,
                );
                // Focus the box as the popup opens so the user can just start typing. Gated on
                // an empty query so it is not re-requested on every later frame, which would
                // fight the user for focus if they clicked into the list.
                if query.is_empty() && !sb.has_focus() {
                    sb.request_focus();
                }
                ui.separator();
                let hits = filter_link_candidates(candidates, query);
                if candidates.is_empty() {
                    ui.label(egui::RichText::new("(no more accounts to link)").weak());
                } else if hits.is_empty() {
                    ui.label(egui::RichText::new("(no account matches that search)").weak().italics());
                }
                egui::ScrollArea::vertical().max_height(240.0).id_salt("asset_link_add_scroll").show(ui, |ui| {
                    for (i, (id, label)) in hits.iter().enumerate() {
                        let resp = ui.selectable_label(false, label.as_str());
                        // On each keystroke, bring the best (first) remaining match into view,
                        // so a long list follows what is being typed instead of staying
                        // wherever it was last scrolled to.
                        if i == 0 && sb.changed() {
                            resp.scroll_to_me(Some(egui::Align::TOP));
                        }
                        if resp.clicked() {
                            req = LinkReq::Add((*id).clone());
                        }
                    }
                });
            });
        // The closure runs only while the popup is open (`inner` is `None` otherwise), so this
        // is the moment the popup closed: forget the query, and the next open starts from the
        // full list rather than a stale filter the user has to notice and clear.
        if out.inner.is_none() {
            query.clear();
        }
    }
    });
    ui.add_space(4.0);
    req
}
