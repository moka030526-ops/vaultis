//! The Taxes tab and its per-filing documents.

use super::*;

impl GuiApp {
    pub(super) fn tab_taxes(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.tax_filings);
        let cur = self.edit_taxfiling.as_ref().map(|r| r.id.clone());
        // Pre-compute each attached document's "location/filename" label (needs an
        // immutable borrow of the vault, so it can't happen inside the edit form).
        let doc_labels: Vec<String> = match self.edit_taxfiling.as_ref() {
            Some(r) => r
                .documents
                .iter()
                .map(|id| self.vault_ref().doc_path(id).unwrap_or_else(|| id.clone()))
                .collect(),
            None => Vec::new(),
        };
        let writable = self.writable;
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = TaxDocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Taxes", "➕ New", &labels, cur.as_deref(), writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_taxes").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    egui::Grid::new("tax_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Owner", &mut r.owner, writable);
                        text_row(ui, "Filing year", &mut r.year, writable);
                    });
                    ui.label("Notes");
                    field_multiline(ui, &mut r.notes, writable, 4);
                    ui.separator();

                    // Attached documents — all live under <owner>/taxes/<year>/…/<ts>_<file>
                    ui.label(format!(
                        "Documents ({}) — under <owner>/{}[/subfolder]/<ts>_<file>",
                        r.documents.len(),
                        records::tax_doc_location(&r.year)
                    ));
                    // Same uniform widget as Trust & Will; map its request to TaxDocReq.
                    docreq = match doc_section(
                        ui,
                        &doc_labels,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        writable,
                    ) {
                        DocSectionReq::Upload => TaxDocReq::Upload,
                        DocSectionReq::Export(i) => TaxDocReq::Export(i),
                        DocSectionReq::Remove(i) => TaxDocReq::Remove(i),
                        DocSectionReq::None => TaxDocReq::None,
                    };

                    action = form_buttons(ui, writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a tax year");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_taxfiling = TaxFiling::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_taxfiling = self.vault_ref().vault.tax_filings.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_tax_doc(docreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::Taxes);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::Taxes),
            _ => {}
        }
    }

    // Performs a Taxes-tab document action (upload to taxes/<year>/, export, or
    // remove). Like `handle_doc`, the vault is borrowed mutably here, not while
    // drawing, and the persist-then-reclaim ordering keeps a crash from leaving a
    // dangling reference.
    pub(super) fn handle_tax_doc(&mut self, req: TaxDocReq) {
        match req {
            TaxDocReq::None => {}
            TaxDocReq::Upload => {
                // Accept a path pasted with surrounding double quotes ("Copy as path").
                let src = records::unquote_path(&self.doc_source).to_string();
                if src.is_empty() {
                    self.refuse("'Upload from' path is required.");
                    return;
                }
                // If no filename is given, default to the source file's own name.
                let name = records::effective_doc_filename(&self.doc_filename, &src);
                if name.trim().is_empty() {
                    self.refuse("Filename is required (the source path has no file name).");
                    return;
                }
                // The folder is derived from the filing year, NOT user-entered.
                let year = self.edit_taxfiling.as_ref().map(|r| r.year.clone()).unwrap_or_default();
                let prefix = records::owner_prefix(
                    self.edit_taxfiling.as_ref().map(|r| r.owner.as_str()),
                    &records::tax_doc_location(&year),
                );
                let ts = records::compact_utc(records::unix_now());
                let name = records::timestamped_filename(&ts, &records::doc_filename(&name));
                let loc = records::doc_upload_dir(&prefix, &self.doc_subfolder);
                let vpath = vault::virtual_path(&loc, &name);
                if vpath.len() > crate::storage::MAX_PATH_LEN {
                    self.refuse(format!(
                        "Path too long: {} bytes (max {}). Shorten the filename.",
                        vpath.len(),
                        crate::storage::MAX_PATH_LEN
                    ));
                    return;
                }
                let id = match self.vault.as_mut() {
                    Some(ov) => match ov.add_document(&loc, &name, Path::new(&src)) {
                        Ok(id) => id,
                        Err(e) => {
                            self.fail(format!("Upload failed: {e}"));
                            return;
                        }
                    },
                    None => return,
                };
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    r.documents.push(id);
                }
                // Persist the record→document link immediately so the manifest entry
                // is referenced (no orphan if the user navigates away).
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                self.clear_doc_inputs();
                if self.persist() {
                    self.status = "Document uploaded to the encrypted volume.".into();
                    self.sync_edit_buffer(Tab::Taxes);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            TaxDocReq::Export(i) => {
                if let Some(id) = self.edit_taxfiling.as_ref().and_then(|r| r.documents.get(i).cloned()) {
                    self.export_doc_to_config_dir(&id);
                }
            }
            TaxDocReq::Remove(i) => {
                // Unlink from the record, persist, THEN reclaim the blob — same
                // crash-safe ordering as handle_doc's Remove.
                let id = self.edit_taxfiling.as_ref().and_then(|r| r.documents.get(i).cloned());
                if let Some(r) = self.edit_taxfiling.as_mut()
                    && i < r.documents.len()
                {
                    r.documents.remove(i);
                }
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                if !self.persist() {
                    return; // persist() already set the "Save failed" status
                }
                self.sync_edit_buffer(Tab::Taxes);
                if let Some(id) = id
                    && let Some(ov) = self.vault.as_mut()
                    && let Err(e) = ov.remove_document(&id)
                {
                    self.fail(format!("Unlinked, but blob cleanup failed: {e}"));
                    return;
                }
                self.status = "Removed document from the vault.".into();
            }
        }
    }
}
