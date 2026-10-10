//! The Zakat tab: an editable year-by-year ledger table rather than a list + form.

use super::*;

impl GuiApp {
    /// The "Zakat" tab: a four-column ledger of Ramadan years — **Ramadan Year**, **Amount
    /// Due**, **Amount Paid**, **Remaining**.
    ///
    /// Unlike every other record tab this is a TABLE, not a list beside a single-record
    /// form: each row holds three short values, so a form pane per year would make the user
    /// click through the years one at a time to read the one number the tab exists to show —
    /// what is still owed, across all of them. The whole ledger is therefore edited in place
    /// (`self.edit_zakat`) and saved in one go.
    ///
    /// **Remaining is computed, never stored** ([`records::ZakatEntry::remaining`]): due −
    /// paid, recomputed every frame. A row whose amounts are non-blank but unparseable shows
    /// "—" rather than a number, so an un-numeric note in a cell can never read as a settled
    /// obligation.
    pub(super) fn tab_zakat(&mut self, ui: &mut egui::Ui) {
        let accent_c = accent(self.theme);
        let writable = self.writable;
        // Deferred actions, applied after the render closures release their `self` borrows.
        let mut new_row = false;
        let mut export = false;
        let mut save = false;
        let mut delete_row: Option<usize> = None;
        // Totals across the BUFFER (what is on screen), not the saved vault, so the header
        // figures track the cells the user is editing rather than lagging a save behind.
        let (total_due, total_paid, total_remaining) = records::zakat_totals(self.edit_zakat.iter());
        let dirty = self.has_unsaved_edits();

        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                section_heading(ui, "Zakat", accent_c);
                badge(ui, &format!("{}", self.edit_zakat.len()), accent_c);
                ui.add_space(6.0);
                if writable && ui.button("➕ New Year").on_hover_text("Add a row for another Ramadan year").clicked() {
                    new_row = true;
                }
                // Save is the whole table at once — a table has no "current record".
                // Disabled when nothing has changed, so the button doubles as a dirty light.
                if writable
                    && ui
                        .add_enabled(dirty, egui::Button::new("💾 Save"))
                        .on_hover_text("Save every row on this tab")
                        .clicked()
                {
                    save = true;
                }
                if ui
                    .button("⬇ CSV")
                    .on_hover_text(
                        "Export every row on this tab to a timestamped CSV in the export directory.\n\
                         The file is UNENCRYPTED.",
                    )
                    .clicked()
                {
                    export = true;
                }
                if dirty {
                    badge(ui, "unsaved", egui::Color32::from_rgb(190, 105, 10));
                }
            });
        });
        ui.add_space(6.0);

        if self.edit_zakat.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("🌙").size(28.0).color(accent_c.gamma_multiply(0.7)));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("No zakat years recorded yet").strong());
                ui.label(
                    egui::RichText::new(if writable {
                        "Click New Year to add one — Remaining is worked out for you."
                    } else {
                        "This vault has no zakat rows."
                    })
                    .weak()
                    .small(),
                );
            });
        } else {
            // The three numbers someone opens this tab to find, before the table itself.
            // Remaining takes the polarity color: anything still owed is the "owed" red.
            ui.horizontal_wrapped(|ui| {
                stat_tile(ui, "Total due", &crate::fmt_money(total_due), accent_c);
                stat_tile(ui, "Total paid", &crate::fmt_money(total_paid), STAT_GOOD);
                stat_tile(
                    ui,
                    "Total remaining",
                    &crate::fmt_money(total_remaining),
                    if total_remaining > 0.0 { STAT_BAD } else { STAT_GOOD },
                );
            });
            ui.add_space(12.0);

            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("zakat_scroll").show(ui, |ui| {
                // Four data columns, plus a fifth for the per-row delete when writable.
                let cols = if writable { 5 } else { 4 };
                egui::Grid::new("zakat_grid").striped(true).num_columns(cols).spacing([18.0, 6.0]).show(ui, |ui| {
                    for h in ["Ramadan Year", "Amount Due", "Amount Paid", "Remaining"] {
                        ui.label(egui::RichText::new(h).strong());
                    }
                    if writable {
                        ui.label("");
                    }
                    ui.end_row();

                    for (i, r) in self.edit_zakat.iter_mut().enumerate() {
                        // `id_salt` per cell: egui identifies a widget by its id, and three
                        // same-width text fields per row would otherwise collide across rows
                        // and share focus/undo state. The record id is stable for the row's
                        // whole life, so the salt survives adding and deleting other rows.
                        zakat_cell(ui, ("zakat_year", &r.id), &mut r.ramadan_year, writable, 120.0, "1446");
                        zakat_cell(ui, ("zakat_due", &r.id), &mut r.amount_due, writable, 140.0, "0");
                        zakat_cell(ui, ("zakat_paid", &r.id), &mut r.amount_paid, writable, 140.0, "0");
                        // The derived column. `None` = at least one amount is non-blank and
                        // does not parse, so there is no honest number to show.
                        match r.remaining() {
                            Some(v) => ui.label(
                                egui::RichText::new(crate::fmt_money(v))
                                    .monospace()
                                    .strong()
                                    .color(if v > 0.0 { STAT_BAD } else { STAT_GOOD }),
                            ),
                            None => ui
                                .label(egui::RichText::new("—").monospace().weak())
                                .on_hover_text("Due or paid is not a number, so Remaining cannot be worked out."),
                        };
                        if writable
                            && ui
                                .button("🗑")
                                .on_hover_text("Delete this year (saved immediately)")
                                .clicked()
                        {
                            delete_row = Some(i);
                        }
                        ui.end_row();
                    }

                    // Total row: bold, and consistent with the tiles above.
                    ui.label(egui::RichText::new("Total").strong());
                    ui.label(egui::RichText::new(crate::fmt_money(total_due)).strong().monospace());
                    ui.label(egui::RichText::new(crate::fmt_money(total_paid)).strong().monospace());
                    ui.label(
                        egui::RichText::new(crate::fmt_money(total_remaining))
                            .strong()
                            .monospace()
                            .color(if total_remaining > 0.0 { STAT_BAD } else { STAT_GOOD }),
                    );
                    if writable {
                        ui.label("");
                    }
                    ui.end_row();
                });
            });
        }

        if export {
            self.export_current_tab_csv();
        }
        if new_row && let Some(r) = records::ZakatEntry::new().ok() {
            self.edit_zakat.push(r);
        }
        if save {
            self.save_zakat();
        }
        if let Some(i) = delete_row {
            self.delete_zakat_row(i);
        }
    }

    /// Write every row of the Zakat table back to the vault and persist once.
    ///
    /// [`records::upsert`] replaces a row with the same id and appends a new one otherwise,
    /// so a table that has both edited and brand-new rows lands correctly in a single pass.
    /// Rows the user deleted are already gone from the vault ([`Self::delete_zakat_row`]
    /// removes and persists on the spot), so nothing here has to reconcile deletions.
    pub(super) fn save_zakat(&mut self) {
        // Trim before writing, like every other tab's Save.
        for r in self.edit_zakat.iter_mut() {
            r.trim_fields();
        }
        // ONLY the rows whose content actually changed (audit 2026-09-16 F-1).
        //
        // `records::upsert` opens with `set_updated_at(now)` unconditionally, and
        // `updated_at` is not decoration: it is the cross-vault merge's RECENCY KEY —
        // `merge.rs` takes a source record only when it is STRICTLY newer than the
        // destination's copy. Every other tab upserts the single record the user had open,
        // so stamping it is honest. This tab saves the WHOLE table, so upserting
        // indiscriminately re-stamped rows the user never touched, making this vault falsely
        // claim the newest copy of them — which silently suppressed a genuine edit merged in
        // from another machine, with an EMPTY merge preview to show for it. `records::trim_all`,
        // the program's other bulk writer, already follows this rule for the same reason.
        let changed: Vec<records::ZakatEntry> = match self.vault.as_ref() {
            Some(ov) => self
                .edit_zakat
                .iter()
                .filter(|r| match ov.vault.zakat.iter().find(|s| s.id == r.id) {
                    // Compare only the three fields the user can type into — the same ones
                    // `ZakatEntry`'s `diff` tracks. `updated_at`/`history` are bookkeeping
                    // `upsert` maintains itself, so comparing them would make every row
                    // differ and defeat the filter entirely.
                    Some(saved) => {
                        saved.ramadan_year != r.ramadan_year
                            || saved.amount_due != r.amount_due
                            || saved.amount_paid != r.amount_paid
                    }
                    // No saved copy: a brand-new row from ➕ New Year.
                    None => true,
                })
                .cloned()
                .collect(),
            None => return,
        };
        if let Some(ov) = self.vault.as_mut() {
            for r in changed {
                records::upsert(&mut ov.vault.zakat, r);
            }
        }
        if self.persist() {
            self.status = "Saved.".into();
            self.sync_edit_buffer(Tab::Zakat);
        }
        // On failure persist() has already set the "Save failed: …" status.
    }

    /// Delete row `i` of the Zakat table, from both the buffer and the vault, and persist.
    ///
    /// Deleting is immediate rather than staged until Save, because the row it removes is
    /// the only place the deletion could otherwise be noticed — there is no form to leave
    /// dirty. On a failed persist the removal is rolled back in memory, for the reason
    /// spelled out in [`Self::delete_current`]: a change the user was told had FAILED must
    /// not sit in memory waiting for the next successful save to commit it silently.
    pub(super) fn delete_zakat_row(&mut self, i: usize) {
        // `get(i)` rather than indexing: the deferred index was captured during rendering,
        // so treat it as possibly stale instead of risking a panic.
        let Some(id) = self.edit_zakat.get(i).map(|r| r.id.clone()) else { return };
        let removed = self.edit_zakat.remove(i);
        let mut rolled_back = false;
        if let Some(ov) = self.vault.as_mut() {
            let v = &mut ov.vault;
            let audit_len = v.audit.len();
            // The SAVED row, not the (possibly edited) buffer row — restoring the buffer's
            // version on a failed delete would commit unsaved edits the user never saved.
            let stored = v.zakat.iter().find(|x| x.id == id).cloned();
            records::remove(&mut v.zakat, &id, &mut v.audit, "Zakat");
            if !self.persist() {
                // persist() already set the "Save failed: …" status.
                if let Some(ov) = self.vault.as_mut() {
                    ov.vault.audit.truncate(audit_len);
                    if let Some(stored) = stored {
                        ov.vault.zakat.push(stored);
                    }
                }
                self.edit_zakat.insert(i, removed);
                rolled_back = true;
            }
        }
        if !rolled_back {
            self.status = "Deleted.".into();
        }
    }
}

/// One editable cell of the Zakat table.
///
/// Read-only mode deliberately does NOT use `TextEdit::interactive(false)` (audit
/// 2026-09-16 F-2): that removes the widget from the accessibility tree entirely, so the
/// value cannot be selected, copied, or announced by a screen reader. Read-only is the
/// desktop's DEFAULT mode and the mobile viewer's ONLY mode — it is the session in which an
/// heir actually reads this ledger — so losing those affordances there is worse than losing
/// them anywhere else. It falls back instead to the same immutable-but-selectable field the
/// rest of the program uses; see [`field_singleline`] for why binding an immutable buffer
/// beats disabling the widget.
///
/// The per-cell `id_salt` only matters for the editable case: three same-width text fields
/// per row would otherwise collide across rows and share focus and undo state. The record id
/// is stable for the row's whole life, so the salt survives adding and deleting other rows.
fn zakat_cell(
    ui: &mut egui::Ui,
    salt: (&str, &str),
    value: &mut String,
    writable: bool,
    width: f32,
    hint: &str,
) {
    if writable {
        ui.add(
            egui::TextEdit::singleline(value).id_salt(salt).hint_text(hint).desired_width(width),
        );
    } else {
        read_only_value(ui, value);
    }
}
