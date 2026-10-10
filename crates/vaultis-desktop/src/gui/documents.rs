//! Attaching, exporting and removing documents on the Trust & Will, Assets and General
//! Documents tabs.

use super::*;

impl GuiApp {
    /// Human-readable "location/filename" of an attached volume file id.
    pub(super) fn attached_label(&self, file_id: Option<String>) -> Option<String> {
        // `file_id?` is the `?` operator on an Option: if `None`, return `None`
        // from this function immediately; otherwise unwrap to `id` and continue.
        let id = file_id?;
        self.vault_ref().doc_path(&id)
    }

    /// Upsert the current edit buffer for a document-bearing tab into the vault,
    /// so a document link is persisted together with its manifest entry.
    pub(super) fn upsert_doc_target(&mut self, target: DocTarget) {
        match target {
            DocTarget::TrustWill => {
                if let Some(r) = self.edit_trustwill.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.trust_wills, r);
                }
            }
            DocTarget::Asset => {
                if let Some(r) = self.edit_asset.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.assets, r);
                }
            }
            DocTarget::General => {
                if let Some(r) = self.edit_general.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.general_documents, r);
                }
            }
        }
    }

    // Performs the document attach/export/detach requested during rendering.
    // Split out so the vault is borrowed mutably *here*, not while drawing.
    pub(super) fn handle_doc(&mut self, req: DocReq, target: DocTarget) {
        match req {
            DocReq::None => {}
            DocReq::Attach => {
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
                // Don't upload+persist an INVALID asset that the Save path rejects (empty owner
                // or non-numeric value → the Summary silently treats it as 0). Validate first,
                // mirroring the Save path's records::asset_validation_error gate.
                if let DocTarget::Asset = target
                    && let Some(r) = self.edit_asset.as_ref()
                    && let Some(msg) = records::asset_validation_error(r)
                {
                    self.fail(msg);
                    return;
                }
                // Owner-first prefix: Assets nest under the owner initials + kind root
                // (/<INITIALS>/assets|liabilities); Trust&Will/General have no owner and keep
                // their slugged group. The timestamp is folded into the filename, so the
                // directory is <prefix>[/<subfolder>].
                let prefix = match target {
                    DocTarget::TrustWill => records::trust_will_doc_location(
                        self.edit_trustwill.as_ref().map(|r| r.document.as_str()).unwrap_or(""),
                    ),
                    DocTarget::Asset => records::owner_prefix(
                        self.edit_asset.as_ref().map(|r| r.owner.as_str()),
                        &records::asset_doc_location(self.edit_asset.as_ref().map(|r| r.kind.as_str()).unwrap_or("")),
                    ),
                    DocTarget::General => records::general_doc_location(
                        self.edit_general.as_ref().map(|r| r.title.as_str()).unwrap_or(""),
                    ),
                };
                let ts = records::compact_utc(records::unix_now());
                let fname = records::timestamped_filename(&ts, &records::doc_filename(&name));
                let loc = records::doc_upload_dir(&prefix, &self.doc_subfolder);
                let vpath = vault::virtual_path(&loc, &fname);
                if vpath.len() > crate::storage::MAX_PATH_LEN {
                    self.refuse(format!(
                        "Path too long: {} bytes (max {}). Shorten the filename or subfolder.",
                        vpath.len(),
                        crate::storage::MAX_PATH_LEN
                    ));
                    return;
                }
                // Nested match: get the vault (mut), then attempt the upload. Each
                // branch either yields the new document `id` or returns early.
                let id = match self.vault.as_mut() {
                    Some(ov) => match ov.add_document(&loc, &fname, Path::new(&src)) {
                        Ok(id) => id,
                        Err(e) => {
                            self.fail(format!("Upload failed: {e}"));
                            return;
                        }
                    },
                    None => return,
                };
                // Capture any document this record already had, so re-attaching
                // reclaims the replaced blob instead of orphaning it.
                let previous = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                match target {
                    DocTarget::TrustWill => {
                        if let Some(r) = self.edit_trustwill.as_mut() {
                            r.file = Some(id);
                        }
                    }
                    DocTarget::Asset => {
                        if let Some(r) = self.edit_asset.as_mut() {
                            r.statement = Some(id);
                        }
                    }
                    DocTarget::General => {
                        if let Some(r) = self.edit_general.as_mut() {
                            r.file = Some(id);
                        }
                    }
                }
                // Persist the record→document link immediately so the manifest
                // entry is referenced (no orphan if the user navigates away).
                self.upsert_doc_target(target);
                self.clear_doc_inputs();
                if self.persist() {
                    // Only reclaim the replaced blob once the new link actually reached
                    // disk. If the save failed, vault.pmv still references `old`, so
                    // dropping it would create a dangling reference (ArchiveMismatch).
                    if let Some(old) = previous
                        && let Some(ov) = self.vault.as_mut()
                    {
                        // `let _ = ...` deliberately discards the `Result`: a failure
                        // here only orphans a blob (harmless), so it is not reported.
                        let _ = ov.remove_document(&old);
                    }
                    self.status = "Document uploaded to the encrypted volume.".into();
                    // The upsert above wrote the whole record, not just the link, so the
                    // form must be re-read from the vault like any other save.
                    self.sync_edit_buffer(target.tab());
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            DocReq::Export => {
                let file_id = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                if let Some(id) = file_id {
                    self.export_doc_to_config_dir(&id);
                }
            }
            DocReq::Remove => {
                // Unlink from the record AND reclaim the encrypted blob, so a
                // "removed" document does not linger in the archive.
                let id = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                match target {
                    DocTarget::TrustWill => {
                        if let Some(r) = self.edit_trustwill.as_mut() {
                            r.file = None;
                        }
                    }
                    DocTarget::Asset => {
                        if let Some(r) = self.edit_asset.as_mut() {
                            r.statement = None;
                        }
                    }
                    DocTarget::General => {
                        if let Some(r) = self.edit_general.as_mut() {
                            r.file = None;
                        }
                    }
                }
                self.upsert_doc_target(target);
                // Persist the unlink BEFORE reclaiming the blob, AND only reclaim if
                // the save succeeded. A crash or a failed save between the two would
                // otherwise leave vault.pmv referencing a doc whose manifest entry is
                // gone (ArchiveMismatch -> unopenable). An orphaned blob is harmless.
                if !self.persist() {
                    return; // persist() already set the "Save failed" status
                }
                // Saved: the form shows the record as stored (see `sync_edit_buffer`).
                self.sync_edit_buffer(target.tab());
                // Three-part let-chain: there is an id, the vault is open, and the
                // blob removal failed — only then report the cleanup error.
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

// Identifies which document-bearing tab a deferred doc action applies to.
#[derive(Clone, Copy)]
pub(super) enum DocTarget {
    TrustWill,
    Asset,
    General,
}

impl DocTarget {
    /// The tab whose form owns this target — the edit buffer [`GuiApp::upsert_doc_target`]
    /// writes into the vault, and so the one to re-read afterwards
    /// ([`GuiApp::sync_edit_buffer`]).
    pub(super) fn tab(self) -> Tab {
        match self {
            DocTarget::TrustWill => Tab::TrustWill,
            DocTarget::Asset => Tab::Assets,
            DocTarget::General => Tab::GeneralDocuments,
        }
    }
}

/// The document attach / export / detach section. Returns the requested action;
/// the caller performs the actual volume operation (to keep `self` borrows
/// disjoint). `attached_present` reflects whether the record currently has a file.
// `#[allow(...)]` silences a specific lint (here: the linter's "too many
// arguments" warning) — it does not change behavior. The `&mut String` inputs
// are the caller's text buffers, edited in place by the widgets below.
/// Outcome of the shared [`doc_section`] widget. Indices refer to the `attached`
/// slice passed in (single-document tabs pass at most one document).
#[derive(PartialEq, Eq, Clone, Copy)]
pub(super) enum DocSectionReq {
    None,
    Upload,
    Export(usize),
    Remove(usize),
}

impl DocSectionReq {
    /// Map to the single-document [`DocReq`] (Trust & Will / Assets / General),
    /// where there is exactly one slot so the index is irrelevant.
    pub(super) fn to_single(self) -> DocReq {
        match self {
            DocSectionReq::Upload => DocReq::Attach,
            DocSectionReq::Export(_) => DocReq::Export,
            DocSectionReq::Remove(_) => DocReq::Remove,
            DocSectionReq::None => DocReq::None,
        }
    }
}

/// The uniform document widget used by EVERY document tab (modeled on Trust &
/// Will): it lists the currently-attached documents — each with Export / Remove —
/// and, when writable, shows the **Subfolder / Filename / Upload-from** inputs and
/// an Attach button. Single-document tabs pass a 0-or-1-element `attached` slice;
/// the multi-document tabs pass the full list. The caller maps the returned request
/// to its own handler (so `self` borrows stay disjoint from the widget).
pub(super) fn doc_section(
    ui: &mut egui::Ui,
    attached: &[String],
    subfolder: &mut String,
    filename: &mut String,
    source: &mut String,
    writable: bool,
) -> DocSectionReq {
    let mut req = DocSectionReq::None;
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    // The whole document area is one card, so a form reads as "fields, then the
    // files that belong to them" rather than as an undifferentiated column.
    card(ui, |ui| {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("📎 Documents").strong().color(accent));
        ui.label(egui::RichText::new("stored encrypted inside the vault").weak().small());
    });
    ui.add_space(6.0);
    if attached.is_empty() {
        ui.label(egui::RichText::new("No documents attached.").weak().italics());
    } else {
        for (i, label) in attached.iter().enumerate() {
            // `shrink_left`: the buttons are placed first and the FILENAME gives up
            // space, so a long name truncates instead of shoving Export/Remove out of
            // the pane.
            egui::containers::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    ui.label(egui::RichText::new("📄").color(accent));
                    ui.add(egui::Label::new(label).truncate()).on_hover_text(label);
                },
                |ui| {
                    if writable
                        && ui
                            .button("🗑 Remove")
                            .on_hover_text("Detach this document from the record and reclaim its space")
                            .clicked()
                    {
                        req = DocSectionReq::Remove(i);
                    }
                    // Export is a read (always allowed); Remove mutates the vault. Export
                    // writes into the directory configured in Config, recreating the document's
                    // volume folder structure — there is no per-export path prompt.
                    if ui
                        .button("⬇ Export")
                        .on_hover_text("Write a DECRYPTED copy into the export directory set in Config")
                        .clicked()
                    {
                        req = DocSectionReq::Export(i);
                    }
                },
            );
        }
    }
    if writable {
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Attach a file").strong().small());
        egui::Grid::new("doc_attach").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Subfolder (optional)");
            ui.add(egui::TextEdit::singleline(subfolder).hint_text("statements").desired_width(fit(ui, 300.0)));
            ui.end_row();
            ui.label("Filename");
            ui.add(egui::TextEdit::singleline(filename).hint_text("statement.pdf").desired_width(fit(ui, 300.0)));
            ui.end_row();
            ui.label("Upload from");
            ui.add(
                egui::TextEdit::singleline(source)
                    .hint_text("/path/on/disk/file.pdf")
                    .desired_width(fit(ui, 300.0)),
            )
            .on_hover_text("The full path to the file. A double-quoted path is accepted as-is.");
            ui.end_row();
        });
        ui.label(
            egui::RichText::new("Leave Filename blank to keep the source file's own name. The original file is not moved.")
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        // Approximate the virtual path length: the stored path also includes the
        // owner-initials/group levels and the <ts>_ filename prefix (~80 bytes, not
        // visible here), so reserve for them. `handle_doc`/`handle_*_doc` do the
        // authoritative check on write.
        let vpath_len = vault::virtual_path(subfolder, filename).len() + 80;
        let over_limit = vpath_len > crate::storage::MAX_PATH_LEN;
        if over_limit {
            ui.colored_label(
                egui::Color32::from_rgb(0xC0, 0x30, 0x30),
                format!("Path may be too long (~{vpath_len} / {} bytes) — shorten the filename or subfolder.", crate::storage::MAX_PATH_LEN),
            );
        }
        if ui
            .add_enabled(
                !over_limit,
                egui::Button::new(egui::RichText::new("⬆ Attach").strong().color(egui::Color32::WHITE)).fill(accent),
            )
            .on_hover_text("Encrypt a copy of the file into the vault's document archive")
            .clicked()
        {
            req = DocSectionReq::Upload;
        }
    }
    });
    ui.add_space(4.0);
    req
}
