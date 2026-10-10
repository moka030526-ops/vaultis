//! The four list + form tabs without special behavior beyond documents: Urgent,
//! Instructions, Trust & Will, and General Documents.

use super::*;

impl GuiApp {
    pub(super) fn tab_urgent(&mut self, ui: &mut egui::Ui) {
        // Same shape as tab_instructions — a title + free-text-body note list — but for the
        // separate, first-in-order URGENT collection.
        let labels = label_list(&self.vault_ref().vault.urgent);
        let cur = self.edit_urgent.as_ref().map(|r| r.id.clone());
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "URGENT", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_urgent").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_urgent.as_mut() {
                    egui::Grid::new("urgent_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Details");
                    field_multiline(ui, &mut r.description, self.writable, 12);
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an urgent note");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_urgent = Urgent::new().ok();
        }
        if let Some(i) = select {
            self.edit_urgent = self.vault_ref().vault.urgent.get(i).cloned();
        }
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_urgent.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_urgent.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.urgent, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    // Show what was written (see `sync_edit_buffer`), or the footer keeps
                    // warning that the record the user just saved is unsaved.
                    self.sync_edit_buffer(Tab::Urgent);
                }
            }
            FormAction::Delete => self.delete_current(Tab::Urgent),
            _ => {}
        }
    }

    pub(super) fn tab_instructions(&mut self, ui: &mut egui::Ui) {
        // Build the left-hand list (id+label pairs) from the vault's records.
        let labels = label_list(&self.vault_ref().vault.instructions);
        // `cur` = id of the record being edited, if any. `.as_ref()` borrows the
        // Option's contents; `.map(|r| r.id.clone())` runs the closure only when
        // `Some`, producing `Option<String>` (an owned copy of the id).
        let cur = self.edit_instruction.as_ref().map(|r| r.id.clone());
        // Deferred-action flags (filled during rendering, acted on afterwards).
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;

        // `ui.columns(2, |c| ...)`: `c` is a slice of two child UIs (left/right).
        two_col(ui, |c| {
            // Destructuring assignment into the outer `new`/`select` vars.
            // `cur.as_deref()` turns `Option<String>` into `Option<&str>` (a
            // borrowed view) without consuming `cur`.
            (new, select, export) = list_panel(&mut c[0], "Instructions", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // Shadow `ui` with a mutable borrow of the right column. "Shadowing"
            // reuses the name `ui` for a new binding within this block.
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_instructions").show(&mut c[1], |ui| {
                // `.as_mut()` borrows the edited record mutably so the form widgets
                // below can write directly into its fields.
                if let Some(r) = self.edit_instruction.as_mut() {
                    egui::Grid::new("instr_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 12);
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an instruction");
                }
            });
        });

        // Now apply the deferred actions outside the render closure.
        if export {
            self.export_current_tab_csv();
        }
        if new {
            // `Instruction::new()` returns a `Result`; `.ok()` discards any error
            // and yields `Option<Instruction>` (Some on success, None on error).
            self.edit_instruction = Instruction::new().ok();
        }
        if let Some(i) = select {
            // `.get(i)` returns `Option<&Instruction>` (None if out of range);
            // `.cloned()` turns that into an owned `Option<Instruction>`.
            self.edit_instruction = self.vault_ref().vault.instructions.get(i).cloned();
        }
        match action {
            FormAction::Save => {
                // Left/right-trim every field before persisting (whole-vault policy);
                // trim the live form too so the displayed values match what was saved.
                if let Some(r) = self.edit_instruction.as_mut() {
                    r.trim_fields();
                }
                // Let-chain: take an owned clone of the edited record AND a mutable
                // borrow of the vault, then upsert (insert-or-update) into it.
                if let Some(r) = self.edit_instruction.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.instructions, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    // Re-read the stored record into the form: `upsert` stamped its
                    // timestamp and history, so without this the buffer stays "different
                    // from the vault" forever and the footer warning never clears.
                    self.sync_edit_buffer(Tab::Instructions);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::Instructions),
            // `_ => {}` handles the remaining `FormAction::None` with a no-op.
            _ => {}
        }
    }

    // --- Tab: Trust and Will -------------------------------------------------

    pub(super) fn tab_trustwill(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.trust_wills);
        let cur = self.edit_trustwill.as_ref().map(|r| r.id.clone());
        // `.and_then(|r| r.file.clone())` chains two Options: only if a record is
        // being edited AND it has an attached `file` do we get `Some(id)`. (Using
        // `.map` here would give a nested `Option<Option<…>>`; `and_then`
        // flattens it.)
        let attached: Vec<String> =
            self.attached_label(self.edit_trustwill.as_ref().and_then(|r| r.file.clone())).into_iter().collect();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Trust and Will", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_trustwill").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_trustwill.as_mut() {
                    egui::Grid::new("tw_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Document");
                        field_singleline(ui, &mut r.document, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Usage");
                    field_multiline(ui, &mut r.usage, self.writable, 8);
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
                    empty_form_hint(ui, "a document");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_trustwill = TrustWill::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_trustwill = self.vault_ref().vault.trust_wills.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::TrustWill);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_trustwill.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_trustwill.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.trust_wills, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::TrustWill);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::TrustWill),
            _ => {}
        }
    }

    // --- Tab: General Documents ----------------------------------------------

    pub(super) fn tab_general(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.general_documents);
        let cur = self.edit_general.as_ref().map(|r| r.id.clone());
        let attached: Vec<String> =
            self.attached_label(self.edit_general.as_ref().and_then(|r| r.file.clone())).into_iter().collect();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;

        two_col(ui, |c| {
            (new, select, export) =
                list_panel(&mut c[0], "General Documents", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_general").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_general.as_mut() {
                    egui::Grid::new("gen_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 8);
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
                    empty_form_hint(ui, "a document");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_general = GeneralDocument::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_general = self.vault_ref().vault.general_documents.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::General);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_general.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_general.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.general_documents, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::GeneralDocuments);
                }
            }
            FormAction::Delete => self.delete_current(Tab::GeneralDocuments),
            _ => {}
        }
    }
}
