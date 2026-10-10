//! The "Update from another vault" screen: open a second vault read-only, preview the
//! patch between the two, then apply it.

use super::*;

impl GuiApp {
    /// Leave the merge flow: drop the opened source vault + computed plan and wipe the
    /// source passwords. Called on cancel, on apply, and whenever Config/Merge is entered.
    pub(super) fn reset_merge(&mut self) {
        self.merge_source = None;
        self.merge_plan = None;
        self.merge_error = None;
        self.wipe_merge_pw();
    }

    /// Zeroize + clear the two source-vault password buffers.
    pub(super) fn wipe_merge_pw(&mut self) {
        self.merge_pw1.zeroize();
        self.merge_pw2.zeroize();
        self.merge_pw1.clear();
        self.merge_pw2.clear();
    }

    /// The "Update from another vault" screen: collect the source directory + its two
    /// passwords, preview the patch (`plan_merge_from`), then apply (`apply_merge_from`).
    /// Only reachable in `--write` mode (the entry button is gated). The opened source
    /// handle + computed plan live in `self.merge_*` between the preview and the apply.
    pub(super) fn ui_merge(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("⬅ Back to Config").clicked() {
                self.reset_merge();
                self.screen = Screen::Config;
            }
            ui.add_space(4.0);
            section_heading(ui, "Update from another vault", accent(self.theme));
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("One-way and additive — nothing in this vault is ever deleted by an update.")
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        ui.separator();

        // Deferred actions (set in the render below, run after to avoid borrow clashes).
        let mut do_preview = false;
        let mut do_apply = false;
        let mut do_reset = false;
        let mut copied: Option<Zeroizing<String>> = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("merge_scroll").show(ui, |ui| {
            if self.merge_plan.is_none() {
                // --- Phase 1: collect the source folder + its two passwords. ---
                ui.label(
                    egui::RichText::new(
                        "Choose the OTHER vault's folder and enter ITS two passwords. The other vault \
                         is opened read-only; this vault is only changed when you click Apply on the \
                         next screen. Nothing here is deleted — only newer/new records are pulled in.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                egui::Grid::new("merge_form").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Other vault folder");
                    ui.add(egui::TextEdit::singleline(&mut self.merge_src_dir).hint_text("/path/to/other-vault-folder").desired_width(fit(ui, 360.0)));
                    ui.end_row();
                    ui.label("Other password 1");
                    password_field(ui, "merge_pw1", &mut self.merge_pw1, &mut copied, None);
                    ui.end_row();
                    ui.label("Other password 2");
                    password_field(ui, "merge_pw2", &mut self.merge_pw2, &mut copied, None);
                    ui.end_row();
                });
                ui.add_space(10.0);
                if ui.button("Preview update").clicked() {
                    do_preview = true;
                }
                if let Some(err) = &self.merge_error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), err);
                }
            } else if let Some(plan) = self.merge_plan.as_ref() {
                // --- Phase 2: show the computed plan; Apply or Cancel. ---
                let short = plan.source_vault_id.get(..8).unwrap_or(plan.source_vault_id.as_str());
                ui.label(egui::RichText::new(format!("From vault {short}")).weak());
                if plan.is_empty() && plan.skipped.is_empty() {
                    ui.add_space(6.0);
                    ui.label("Already up to date — no records in the other vault are newer or new.");
                } else {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!(
                        "{} record(s) to change ({} new, {} updated) · {} document(s) to copy ({} bytes)",
                        plan.records.len(),
                        plan.new_count(),
                        plan.updated_count(),
                        plan.blobs_to_copy(),
                        plan.bytes_to_copy(),
                    )).strong());
                    ui.add_space(6.0);
                    egui::Grid::new("merge_records").striped(true).num_columns(3).show(ui, |ui| {
                        ui.label(egui::RichText::new("Change").strong());
                        ui.label(egui::RichText::new("Type").strong());
                        ui.label(egui::RichText::new("Record / recency").strong());
                        ui.end_row();
                        for r in &plan.records {
                            ui.label(r.change.as_str());
                            ui.label(r.kind.as_str());
                            let recency = match r.current_updated_at {
                                Some(cur) => format!("{} ({} -> {})", r.label, format_time(cur), format_time(r.source_updated_at)),
                                None => format!("{} (new @ {})", r.label, format_time(r.source_updated_at)),
                            };
                            ui.label(recency);
                            ui.end_row();
                        }
                    });
                    if !plan.blobs.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Documents").strong());
                        for b in &plan.blobs {
                            let tag = if b.already_present { "already here" } else { "copy" };
                            ui.label(format!("  [{tag}] {} ({} bytes)", b.path, b.size));
                        }
                    }
                    if !plan.new_categories.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Category types to add (so the merged types show in Config)").strong());
                        for c in &plan.new_categories {
                            ui.label(format!("  + {c}"));
                        }
                    }
                    if !plan.skipped.is_empty() {
                        ui.add_space(8.0);
                        ui.colored_label(egui::Color32::from_rgb(190, 120, 50), "Skipped (not applied):");
                        for s in &plan.skipped {
                            ui.label(format!("  {} — {} — {}", s.kind.as_str(), s.label, s.reason));
                        }
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let can_apply = !plan.is_empty();
                    if ui.add_enabled(can_apply, egui::Button::new("Apply update")).clicked() {
                        do_apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_reset = true;
                    }
                });
            }
        });

        // A copied source-vault password (built-in Ctrl+C) routes through the hardened,
        // auto-clearing clipboard path, exactly like the unlock screen.
        if let Some(text) = copied {
            self.copy_to_clipboard(text);
        }

        if do_preview {
            self.merge_preview();
        }
        if do_apply {
            self.merge_apply();
        }
        if do_reset {
            // Cancel the preview but stay on the screen to re-enter credentials.
            self.reset_merge();
        }
    }

    /// Open the source vault read-only and compute the patch into `self.merge_plan`.
    /// Collapses the source's open errors into ONE generic message so this screen can't be
    /// used as a password-correctness oracle for the other vault (mirrors the unlock screen).
    pub(super) fn merge_preview(&mut self) {
        self.merge_error = None;
        // The just-typed source-vault passwords are secrets: wipe them on EVERY exit path
        // (each validation early-return below, the open failure, the plan error, and success),
        // never leaving them resident in the heap buffers after this call.
        let dir = records::unquote_path(&self.merge_src_dir);
        if dir.is_empty() {
            self.merge_error = Some("Enter the other vault's folder.".into());
            self.wipe_merge_pw();
            return;
        }
        let src_path = crate::launch::vault_file(dir);
        if !src_path.exists() {
            self.merge_error = Some("No vault found in that folder.".into());
            self.wipe_merge_pw();
            return;
        }
        // Guard against merging this vault into itself.
        if same_vault_path(&src_path, &self.path) {
            self.merge_error = Some("That is this same vault — choose a different one.".into());
            self.wipe_merge_pw();
            return;
        }
        let source = match OpenVault::open_read_only(src_path, self.merge_pw1.as_bytes(), self.merge_pw2.as_bytes()) {
            Ok(v) => v,
            Err(_) => {
                // Single generic message for EVERY failure (wrong password, corrupt, etc.)
                // so the screen never confirms whether the entered passwords were right.
                self.merge_error = Some("Could not open that vault — wrong password(s) or unreadable.".into());
                self.wipe_merge_pw();
                return;
            }
        };
        let plan = match self.vault_ref().plan_merge_from(&source) {
            Ok(p) => p,
            Err(e) => {
                self.merge_error = Some(format!("Could not build the update: {e}"));
                self.wipe_merge_pw();
                return;
            }
        };
        // Keep the opened source + plan for the apply step; wipe the entered passwords now.
        self.merge_source = Some(source);
        self.merge_plan = Some(plan);
        self.wipe_merge_pw();
    }

    /// Apply the previewed patch (copy blobs, replace/insert records, save), then return to
    /// Config with a status summary. Recomputes against the held source handle internally.
    pub(super) fn merge_apply(&mut self) {
        // Disjoint field borrows: `self.vault` (mut) and `self.merge_source` (shared).
        let result = match (self.vault.as_mut(), self.merge_source.as_ref()) {
            (Some(cur), Some(src)) => cur.apply_merge_from(src),
            _ => {
                self.merge_error = Some("Nothing to apply.".into());
                return;
            }
        };
        match result {
            Ok(report) => {
                self.status = format!(
                    "Updated from another vault: {} new, {} updated record(s); {} document(s) copied; {} type(s) added.{}",
                    report.records_added,
                    report.records_updated,
                    report.blobs_copied,
                    report.categories_added,
                    if report.records_skipped > 0 { format!(" {} skipped.", report.records_skipped) } else { String::new() },
                );
                // A merge rewrites collections underneath the UI. Every other tab re-reads
                // the vault as it renders, but Zakat renders from its own table buffer — so
                // without this re-seed the tab would keep showing the pre-merge ledger, and
                // its next Save would write those stale rows back over the merged ones.
                self.sync_edit_buffer(Tab::Zakat);
                self.reset_merge();
                self.screen = Screen::Config;
            }
            Err(e) => {
                // A failed apply may have poisoned the handle (the in-memory merge can no
                // longer be saved — see apply_merge_from's save-failure poisoning). Drop it
                // and return to the unlock screen so reopening loads the clean on-disk vault,
                // mirroring the change-password recovery path. Nothing committed is lost: the
                // merge did not persist, and any prior edits were already saved.
                self.vault = None;
                self.reset_merge();
                self.auth_mode = AuthMode::Unlock;
                self.screen = Screen::Auth;
                self.wipe_passwords();
                self.auth_error = Some(format!("Update interrupted: {e}. Unlock again to recover."));
            }
        }
    }
}
