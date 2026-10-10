//! The Assets & Liabilities tab.

use super::*;

impl GuiApp {
    pub(super) fn tab_assets(&mut self, ui: &mut egui::Ui) {
        // Same card treatment as the Accounts filter row, so the two list tabs have
        // the same control strip in the same place.
        let accent_c = accent(self.theme);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                // Grouped tree: owner → Asset/Liability → type (empty levels skipped).
                option_toggle(
                    ui,
                    &mut self.asset_grouped,
                    "grouped tree",
                    "Group the list by owner > asset/liability > type",
                );
                option_toggle(
                    ui,
                    &mut self.asset_filter_review,
                    "review only",
                    "Show only items flagged for review",
                );
                if self.asset_filter_review {
                    badge(ui, "filtered", egui::Color32::from_rgb(190, 105, 10));
                }
            });
        });
        ui.add_space(6.0);
        let fr = self.asset_filter_review;
        // In grouped mode, the same review-filtered assets as an owner→kind→type tree
        // (built here so the render closure doesn't re-borrow `self`).
        let tree = if self.asset_grouped {
            Some(records::asset_tree(self.vault_ref().vault.assets.iter().filter(|a| !fr || a.review)))
        } else {
            None
        };
        // Iterator pipeline: walk assets by reference, keep only those passing the
        // filter closure (`!fr` = filter off, or the item is flagged), turn each
        // into an `(id, label)` tuple, and collect into a `Vec`.
        let labels: Vec<(String, String)> = self
            .vault_ref()
            .vault
            .assets
            .iter()
            .filter(|a| !fr || a.review)
            .map(|a| (a.id.clone(), a.label()))
            .collect();
        let cur = self.edit_asset.as_ref().map(|r| r.id.clone());
        // Flat-list arrow navigation: when not grouped, ↑/↓ move to the prev/next item.
        let nav_target = list_nav_target(ui, !self.asset_grouped, &labels, cur.as_deref());
        let attached: Vec<String> =
            self.attached_label(self.edit_asset.as_ref().and_then(|r| r.statement.clone())).into_iter().collect();
        let asset_types = self.vault_ref().categories().asset.clone();
        // Linked-accounts data for the record under edit, resolved BEFORE the mutable
        // `edit_asset` borrow inside the columns closure (same borrow discipline as the
        // Accounts tab's `subtypes` precompute): the linked ids with display labels — a
        // dangling id renders as the RAW id, see `linked_account_rows` — plus the
        // not-yet-linked accounts offered by the "add link" dropdown.
        let linked_rows: Vec<(String, String)> = self
            .edit_asset
            .as_ref()
            .map(|r| linked_account_rows(&self.vault_ref().vault.accounts, &r.linked_accounts))
            .unwrap_or_default();
        let link_candidates: Vec<(String, String)> = self
            .edit_asset
            .as_ref()
            .map(|r| link_candidates(&self.vault_ref().vault.accounts, &r.linked_accounts))
            .unwrap_or_default();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;
        let mut linkreq = LinkReq::None;

        two_col(ui, |c| {
            match &tree {
                // Grouped tree: owner → Asset/Liability → type → entry (leaf), empty levels
                // skipped. egui's CollapsingHeader gives the +/- expand control.
                Some(root) => {
                    let lp = &mut c[0];
                    // Same header as the flat `list_panel`, so switching to the tree
                    // does not change what the top of the pane looks like.
                    lp.horizontal_wrapped(|ui| {
                        let accent = ui_accent(ui);
                        section_heading(ui, "Assets and Liabilities", accent);
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
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("asset_tree").show(lp, |ui| {
                        let mut path: Vec<String> = Vec::new();
                        if let Some(s) = render_acct_node(ui, root, &mut path, cur.as_deref(), &labels, "asset") {
                            select = Some(s);
                        }
                    });
                }
                None => {
                    (new, select, export) =
                        list_panel(&mut c[0], "Assets and Liabilities", "➕ New", &labels, cur.as_deref(), self.writable, nav_target);
                }
            }
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_assets").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_asset.as_mut() {
                    let w = self.writable;
                    egui::Grid::new("asset_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Asset / Liability");
                        combo(ui, "asset_kind", &mut r.kind, &["Asset".to_string(), "Liability".to_string()], w);
                        ui.end_row();
                        ui.label("Owner");
                        field_singleline(ui, &mut r.owner, w, 420.0);
                        ui.end_row();
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, w, 420.0);
                        ui.end_row();
                        ui.label("Beneficiary");
                        field_singleline(ui, &mut r.beneficiary, w, 420.0);
                        ui.end_row();
                        ui.label("Approximate value");
                        field_singleline(ui, &mut r.approx_value, w, 420.0);
                        ui.end_row();
                        ui.label("As-of date");
                        field_singleline_hint(ui, &mut r.as_of_date, w, 420.0, "YYYY-MM-DD");
                        ui.end_row();
                        ui.label("Institution");
                        field_singleline(ui, &mut r.institution, w, 420.0);
                        ui.end_row();
                        ui.label("Type");
                        combo(ui, "asset_type", &mut r.asset_type, &asset_types, w);
                        ui.end_row();
                        ui.label("URL");
                        field_singleline(ui, &mut r.url, w, 420.0);
                        ui.end_row();
                        ui.label("Review");
                        ui.add_enabled(w, egui::Checkbox::new(&mut r.review, "flag for review"));
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 4);
                    ui.separator();
                    // Cross-record links to Accounts (edited on the asset side ONLY; the
                    // Accounts form shows the read-only reverse view). Deferred like docreq.
                    linkreq = linked_accounts_section(
                        ui,
                        &linked_rows,
                        &link_candidates,
                        &mut self.link_search,
                        self.writable,
                    );
                    ui.separator();
                    docreq = doc_section(
                        ui,
                        &attached,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        self.writable,
                    )
                    .to_single();
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an asset or liability");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_asset = self.new_asset_seeded();
            self.clear_doc_inputs();
        }
        // A click wins over keyboard nav. A keyboard arrow CAN land in the same egui frame
        // as a button click (events are batched per repaint), and the deferred requests
        // below — Save/Delete, the document Export/Remove, and the link Add/Unlink — were
        // all captured against the CURRENTLY shown asset. Applying a same-frame nav swap
        // first would retarget them at the NEIGHBOUR.
        //
        // Today `list_nav_target`'s `focused()` guard already blocks this (clicking a
        // button focuses it), which is why the pinned test passes without this line. But
        // that protection is emergent, and Accounts was given this explicit guard by audit
        // A-8 while Assets — which has strictly MORE record-targeted deferred actions —
        // was left relying on the implicit one. Belt and braces, matching Accounts.
        let record_action_pending = new
            || !matches!(action, FormAction::None)
            || !matches!(docreq, DocReq::None)
            || !matches!(linkreq, LinkReq::None);
        if !record_action_pending {
            select = select.or(nav_target);
        }
        if let Some(i) = select
            && let Some((id, _)) = labels.get(i)
        {
            // Resolve by id (the list may be filtered by the review flag). The
            // `(id, _)` pattern keeps the id and ignores the label. `.find(|a|
            // ...)` returns the first matching element (`&a.id == id` compares the
            // borrowed ids); `.cloned()` makes an owned copy for the edit buffer.
            self.edit_asset = self.vault_ref().vault.assets.iter().find(|a| &a.id == id).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::Asset);
        self.handle_link_req(linkreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_asset.as_mut() {
                    r.trim_fields();
                }
                // Validate before saving: every Asset/Liability must have an owner and a
                // NUMERIC approximate value, so the Summary tab can aggregate it. On failure,
                // refuse (the user's input, not a broken save) and do NOT save the bad record.
                let invalid = self.edit_asset.as_ref().and_then(records::asset_validation_error);
                if let Some(msg) = invalid {
                    self.refuse(msg);
                } else {
                    if let Some(r) = self.edit_asset.clone()
                        && let Some(ov) = self.vault.as_mut()
                    {
                        records::upsert(&mut ov.vault.assets, r);
                    }
                    if self.persist() {
                        self.status = "Saved.".into();
                        self.sync_edit_buffer(Tab::Assets);
                    }
                    // On failure persist() has already set the "Save failed: …" status.
                }
            }
            FormAction::Delete => self.delete_current(Tab::Assets),
            _ => {}
        }
    }

    /// Seed the edit buffer for a NEW asset/liability. When a record is open the new
    /// entry inherits its grouping fields — kind (Asset/Liability), asset type, and
    /// owner — so a run of related holdings shares a category; the identifying fields
    /// (title, value, institution, dates, links) start blank. Blank when nothing is open.
    pub(super) fn new_asset_seeded(&self) -> Option<AssetLiability> {
        let mut a = AssetLiability::new().ok()?;
        if let Some(cur) = self.edit_asset.as_ref() {
            a.kind = cur.kind.clone();
            a.asset_type = cur.asset_type.clone();
            a.owner = cur.owner.clone();
        }
        Some(a)
    }
}
