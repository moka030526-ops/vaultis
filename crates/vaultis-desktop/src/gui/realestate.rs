//! The Real Estate tab and its per-property documents.

use super::*;

impl GuiApp {
    pub(super) fn tab_realestate(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.real_estate);
        let cur = self.edit_realestate.as_ref().map(|r| r.id.clone());
        // Pre-compute attached document labels (needs an immutable vault borrow).
        let doc_labels: Vec<String> = match self.edit_realestate.as_ref() {
            Some(r) => r
                .documents
                .iter()
                .map(|id| self.vault_ref().doc_path(id).unwrap_or_else(|| id.clone()))
                .collect(),
            None => Vec::new(),
        };
        // The single global "reveal all" toggle for this screen (mirrors Accounts): when
        // on, all four portal passwords are shown. There is no per-record reveal.
        let accent_c = accent(self.theme);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                option_toggle(
                    ui,
                    &mut self.re_reveal_all,
                    "👁 reveal all portal passwords",
                    "Unmask the four portal passwords on this screen. Resets when you switch tabs.",
                );
            });
        });
        ui.add_space(6.0);
        let reveal = self.re_reveal_all;
        let writable = self.writable;
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut copy_pw: Option<Zeroizing<String>> = None;
        // Deferred plain-copy for the portals' non-secret URL / username buttons (acted on
        // after rendering, like `copy_pw`). A plain `String` — not secrets, so no zeroizing.
        let mut copy_plain: Option<String> = None;
        let mut docreq = ReDocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Real Estate", "➕ New", &labels, cur.as_deref(), writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_realestate").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_realestate.as_mut() {
                    // No inner ScrollArea here: the whole tab is already wrapped in the
                    // CentralPanel's both-axis scroll. A nested vertical scroll over this
                    // form would capture the wheel and (having no overflow of its own)
                    // scroll nothing, while the outer area never saw the event.
                    egui::Grid::new("re_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Address", &mut r.address, writable);
                        text_row(ui, "Owner", &mut r.owner, writable);
                        text_row(ui, "Taxes", &mut r.taxes, writable);
                        text_row(ui, "HOA dues / info", &mut r.hoa, writable);
                        text_row(ui, "Income account", &mut r.income_account, writable);
                        text_row(ui, "Financing account", &mut r.financing_account, writable);
                        text_row(ui, "Financing balance", &mut r.financing_balance, writable);
                        text_row(ui, "Payment account", &mut r.payment_account, writable);
                    });

                    portal_section(ui, "Property Management portal", &mut r.property_mgmt_url, &mut r.property_mgmt_username, &mut r.property_mgmt_password, &mut r.property_mgmt_comment, reveal, writable, &mut copy_pw, &mut copy_plain);
                    portal_section(ui, "Insurance portal", &mut r.insurance_url, &mut r.insurance_username, &mut r.insurance_password, &mut r.insurance_comment, reveal, writable, &mut copy_pw, &mut copy_plain);
                    portal_section(ui, "HOA portal", &mut r.hoa_url, &mut r.hoa_username, &mut r.hoa_password, &mut r.hoa_comment, reveal, writable, &mut copy_pw, &mut copy_plain);
                    portal_section(ui, "Tax portal", &mut r.tax_portal_url, &mut r.tax_portal_username, &mut r.tax_portal_password, &mut r.tax_portal_comment, reveal, writable, &mut copy_pw, &mut copy_plain);

                    ui.separator();
                    ui.label("Comments");
                    field_multiline(ui, &mut r.comments, writable, 3);

                    ui.separator();
                    ui.label(format!(
                        "Documents ({}) — under <owner>/{}[/subfolder]/<ts>_<file>",
                        r.documents.len(),
                        records::real_estate_doc_location(&r.address)
                    ));
                    // Same uniform widget as Trust & Will (multi-document: the list
                    // holds every attached doc); map its request to ReDocReq.
                    docreq = match doc_section(
                        ui,
                        &doc_labels,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        writable,
                    ) {
                        DocSectionReq::Upload => ReDocReq::Upload,
                        DocSectionReq::Export(i) => ReDocReq::Export(i),
                        DocSectionReq::Remove(i) => ReDocReq::Remove(i),
                        DocSectionReq::None => ReDocReq::None,
                    };

                    action = form_buttons(ui, writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a property");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_realestate = RealEstate::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_realestate = self.vault_ref().vault.real_estate.get(i).cloned();
            self.clear_doc_inputs();
        }
        // Pre-size the portal password buffers so per-keystroke typing never grows
        // (and so reallocates) them — a reallocation frees the old buffer WITHOUT
        // zeroizing, stranding cleartext fragments of a portal password in freed
        // heap. RealEstate is ZeroizeOnDrop, but that only wipes the final buffer,
        // not abandoned reallocations. Same mitigation as the Accounts password field.
        if let Some(r) = self.edit_realestate.as_mut() {
            presize_secret(&mut r.property_mgmt_password);
            presize_secret(&mut r.insurance_password);
            presize_secret(&mut r.hoa_password);
            presize_secret(&mut r.tax_portal_password);
        }
        if let Some(pw) = copy_pw {
            self.copy_to_clipboard(pw);
        }
        // After `copy_pw`, so a same-frame secret copy is never overwritten by a plain one
        // (only one button can be clicked per frame, but the ordering makes that explicit).
        if let Some(text) = copy_plain {
            self.copy_plain(&text);
        }
        self.handle_re_doc(docreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_realestate.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::RealEstate);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::RealEstate),
            _ => {}
        }
    }

    // Performs a Real-Estate document action (upload to real-estate/<address>/,
    // export, or remove), mirroring handle_doc's persist-then-reclaim ordering.
    pub(super) fn handle_re_doc(&mut self, req: ReDocReq) {
        match req {
            ReDocReq::None => {}
            ReDocReq::Upload => {
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
                let address = self.edit_realestate.as_ref().map(|r| r.address.clone()).unwrap_or_default();
                let prefix = records::owner_prefix(
                    self.edit_realestate.as_ref().map(|r| r.owner.as_str()),
                    &records::real_estate_doc_location(&address),
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
                if let Some(r) = self.edit_realestate.as_mut() {
                    r.documents.push(id);
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                self.clear_doc_inputs();
                if self.persist() {
                    self.status = "Document uploaded to the encrypted volume.".into();
                    self.sync_edit_buffer(Tab::RealEstate);
                }
            }
            ReDocReq::Export(i) => {
                if let Some(id) = self.edit_realestate.as_ref().and_then(|r| r.documents.get(i).cloned()) {
                    self.export_doc_to_config_dir(&id);
                }
            }
            ReDocReq::Remove(i) => {
                let id = self.edit_realestate.as_ref().and_then(|r| r.documents.get(i).cloned());
                if let Some(r) = self.edit_realestate.as_mut()
                    && i < r.documents.len()
                {
                    r.documents.remove(i);
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                if !self.persist() {
                    return;
                }
                self.sync_edit_buffer(Tab::RealEstate);
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

/// Render one portal-login section (URL / username / masked password, each with a copy
/// button, plus a free-form comment) into the Real Estate form. The password is masked
/// unless `reveal`; `copy_pw` (secret: auto-cleared after 15 s) and `copy_plain` (the URL
/// and username: no auto-clear) are set when a copy button is clicked, to be acted on
/// after rendering.
#[allow(clippy::too_many_arguments)]
fn portal_section(
    ui: &mut egui::Ui,
    title: &str,
    url: &mut String,
    username: &mut String,
    password: &mut String,
    comment: &mut String,
    reveal: bool,
    writable: bool,
    copy_pw: &mut Option<Zeroizing<String>>,
    copy_plain: &mut Option<String>,
) {
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    // Each of the four portals is its own card, so they read as four separate
    // logins rather than one long run of near-identical fields.
    card(ui, |ui| {
        ui.label(egui::RichText::new(format!("🔐 {title}")).strong().color(accent));
        ui.add_space(4.0);
        egui::Grid::new(title).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            text_row_with_copy(ui, "URL", url, writable, copy_plain);
            text_row_with_copy(ui, "Username", username, writable, copy_plain);
            ui.label("Password");
            ui.horizontal(|ui| {
                // The password field reserves room for TWO buttons on the Accounts tab
                // (generate + copy) but only one here, matching the URL/username rows above.
                // `title` is unique per portal (Property Mgmt / Insurance / HOA / Tax), so
                // it is a valid per-field id salt for the secret-field hardening. Copy stays
                // available read-only (it is a read, not an edit).
                secret_text_edit(ui, title, password, reveal, writable, fit_with_buttons(ui, 260.0, 1), copy_pw);
                if ui
                    .button("📋")
                    .on_hover_text("Copy to the clipboard (cleared automatically after 15 seconds)")
                    .clicked()
                {
                    *copy_pw = Some(Zeroizing::new(password.clone()));
                }
            });
            ui.end_row();
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new("Comment").weak().small());
        // Editable when writable, else immutable-but-selectable (see `field_singleline`).
        // The per-portal `id_salt` keeps the four comment boxes' ids distinct.
        let salt = (title, "comment");
        if writable {
            ui.add(
                egui::TextEdit::multiline(comment)
                    .id_salt(salt)
                    .hint_text("security questions, account numbers, who to ask for…")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        } else {
            let _ = salt; // the id salt only matters for the editable widget
            read_only_value(ui, comment);
        }
    });
}
