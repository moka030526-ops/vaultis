//! The Config screen: category lists, view defaults, appearance, volume size,
//! redundancy, backups and the export directory.

use super::*;

impl GuiApp {
    pub(super) fn ui_config(&mut self, ui: &mut egui::Ui) {
        let accent = accent(self.theme);
        // Back sits FIRST, at the left edge where a back control is looked for, and
        // the heading follows it — the old order put the way out after the title.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                self.screen = Screen::Main;
            }
            ui.add_space(4.0);
            section_heading(ui, "Configuration", accent);
        });
        ui.add_space(4.0);
        ui.separator();
        // Show where this vault lives on disk (the vault.pmv path; its parent dir holds
        // the manifest/ and volume/ too).
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Vault location").weak().small());
            ui.label(egui::RichText::new(self.path.display().to_string()).monospace().small());
        });
        if !self.writable {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "🔒  Read-only: no vault field can be edited. The color theme and the view \
                     defaults below can still be changed (they are local preferences); \
                     backup and document export are still available.",
                )
                .color(egui::Color32::from_rgb(170, 90, 0)),
            );
        }

        // These `bool` flags are the deferred-action pattern: rendering only
        // *sets* them; the actual vault mutations happen after the closures below
        // return, so we never hold a render-time borrow of `self` and a write
        // borrow at the same time.
        let mut add_asset = false;
        let mut add_account = false;
        let mut add_subtype = false;
        let mut do_backup = false;
        let mut set_export = false;
        let mut set_volume = false;
        let mut set_redundancy = false;
        let mut start_merge = false;
        let mut sync_types = false;
        // Deferred DELETE actions: which category the user clicked × on (handled after
        // the render closures, same borrow-discipline as the add_* flags).
        let mut remove_asset: Option<String> = None;
        let mut remove_account: Option<String> = None;
        let mut remove_subtype: Option<(String, String)> = None;
        // Snapshot the category lists + volume cap (from the open vault) before the
        // render closure borrows `self` mutably for the text inputs.
        let cur_volume_mib = self.vault_ref().volume_max_size() / (1024 * 1024);
        // The current on-disk depth, to skip a no-op Apply. The picker's selection
        // lives in the PERSISTENT `self.cfg_redundancy` (seeded when Config opened),
        // not a frame-local, so it survives until the user clicks Apply.
        let cur_redundancy = self.vault_ref().redundancy();
        let cats = self.vault_ref().categories();
        let type_names = cats.account_type_names();
        // Owned snapshots so the render closures don't hold a borrow of `self`/`cats`.
        let asset_names: Vec<String> = cats.asset.clone();
        // Each account type with its subtypes kept as a list (so each gets its own ×).
        let account_list: Vec<(String, Vec<String>)> =
            cats.account.iter().map(|t| (t.name.clone(), t.subtypes.clone())).collect();

        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("config_scroll").show(ui, |ui| {
            // Appearance: a color-theme picker. Changing it applies live and is
            // saved to a small preferences file (it carries no vault data), so it
            // works in read-only mode too and persists to the next launch.
            config_heading(ui, "Appearance");
            egui::ComboBox::from_label("Color theme").selected_text(self.theme.label()).show_ui(ui, |ui| {
                for t in Theme::ALL {
                    ui.selectable_value(&mut self.theme, t, t.label());
                }
            });
            // Interface scale: the second styling axis. Applied and saved by `render`
            // the moment the selection changes, so the effect is immediate and survives
            // the next launch — like the theme, it is a preference in <vault_root>/prefs.json
            // and holds no vault data, so it works in read-only mode too.
            egui::ComboBox::from_label("Interface size")
                .selected_text(self.ui_scale.label())
                .show_ui(ui, |ui| {
                    for sc in UiScale::ALL {
                        ui.selectable_value(&mut self.ui_scale, sc, sc.label());
                    }
                });
            ui.label(
                egui::RichText::new(
                    "Scales the whole window — text, buttons and spacing together. \
                     Useful if the default is hard to read.",
                )
                .small()
                .weak(),
            );
            egui::ComboBox::from_label("Typeface")
                .selected_text(self.font.label())
                .show_ui(ui, |ui| {
                    for f in FontChoice::ALL {
                        ui.selectable_value(&mut self.font, f, f.label());
                    }
                });
            ui.label(
                egui::RichText::new(
                    "Both typefaces are built into the program — nothing is loaded from \
                     this computer, so it looks the same everywhere.",
                )
                .small()
                .weak(),
            );
            ui.add_space(14.0);

            // View defaults: cosmetic UI preferences (`<vault_root>/prefs.json`), not vault
            // content — so they work in read-only mode too and persist to the next launch.
            // Each checkbox binds to the saved-default field, saves on change, and applies to
            // the live view state so the effect is immediate; the saved value re-seeds these
            // on the next vault open (see `GuiApp::new` and the tab-switch reset).
            //
            // "Reveal all passwords by default" deliberately is NOT here. prefs.json sits
            // unencrypted beside the vault folders, so anyone who can write to the media
            // without knowing the passwords authors it — and a persisted reveal-all would let
            // that tampering unmask every password on open. Reveal stays a per-session toggle
            // that always starts off (see the prefs comment in `prefs.rs`).
            config_heading(ui, "View defaults");
            if ui
                .checkbox(&mut self.group_assets_default, "Group assets by default")
                .changed()
            {
                crate::save_group_assets_default(&self.vault_root, self.group_assets_default);
                self.asset_grouped = self.group_assets_default;
            }
            if ui
                .checkbox(&mut self.group_accounts_default, "Group accounts by default")
                .changed()
            {
                crate::save_group_accounts_default(&self.vault_root, self.group_accounts_default);
                self.acct_grouped = self.group_accounts_default;
            }
            ui.add_space(14.0);

            config_heading(ui, "Asset / Liability types");
            // One chip per type with a delete (×) button. The × only deletes when the
            // type is unused by a live record (else a status message explains why).
            ui.horizontal_wrapped(|ui| {
                for name in &asset_names {
                    ui.label(egui::RichText::new(name).weak());
                    // The category list is stored independently of records; tag entries no
                    // live record uses so the user can see what's safe to delete.
                    if self.vault_ref().asset_type_usage(name) == 0 {
                        ui.label(egui::RichText::new("· unused").weak().italics());
                    }
                    if self.writable
                        && ui.small_button("×").on_hover_text(format!("Delete “{name}” (only if unused)")).clicked()
                    {
                        remove_asset = Some(name.clone());
                    }
                    ui.add_space(8.0);
                }
            });
            ui.horizontal(|ui| {
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_asset_type).hint_text("New type").desired_width(fit(ui, 240.0)),
                );
                if self.writable && ui.button("Add type").clicked() {
                    add_asset = true;
                }
            });

            ui.add_space(14.0);
            config_heading(ui, "Account types & subtypes");
            // Each type on its own row: a × to delete the type (blocked while it has
            // subtypes or is in use), then each subtype with its own × (blocked if used).
            for (name, subs) in &account_list {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(name).strong());
                    if self.vault_ref().account_type_usage(name) == 0 {
                        ui.label(egui::RichText::new("· unused").weak().italics());
                    }
                    if self.writable
                        && ui
                            .small_button("×")
                            .on_hover_text("Delete type (only if it has no subtypes and is unused)")
                            .clicked()
                    {
                        remove_account = Some(name.clone());
                    }
                    ui.label(":");
                    if subs.is_empty() {
                        ui.label(egui::RichText::new("—").weak());
                    }
                    for sub in subs {
                        ui.label(egui::RichText::new(sub).weak());
                        if self.vault_ref().account_subtype_usage(name, sub) == 0 {
                            ui.label(egui::RichText::new("· unused").weak().italics());
                        }
                        if self.writable
                            && ui.small_button("×").on_hover_text(format!("Delete subtype “{sub}” (only if unused)")).clicked()
                        {
                            remove_subtype = Some((name.clone(), sub.clone()));
                        }
                        ui.add_space(6.0);
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_account_type)
                        .hint_text("New account type")
                        .desired_width(fit(ui, 220.0)),
                );
                if self.writable && ui.button("Add type").clicked() {
                    add_account = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Add subtype to:");
                // Pick the type the subtype belongs to.
                let cur = if self.new_subtype_for.is_empty() { "(choose type)".to_string() } else { self.new_subtype_for.clone() };
                ui.add_enabled_ui(self.writable, |ui| {
                    egui::ComboBox::from_id_salt("subtype_for").selected_text(cur).show_ui(ui, |ui| {
                        for name in &type_names {
                            ui.selectable_value(&mut self.new_subtype_for, name.clone(), name);
                        }
                    });
                });
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_subtype_name).hint_text("New subtype").desired_width(fit(ui, 180.0)),
                );
                if self.writable && ui.button("Add subtype").clicked() {
                    add_subtype = true;
                }
            });

            ui.add_space(16.0);
            ui.separator();
            config_heading(ui, "Export directory");
            ui.label(
                egui::RichText::new(
                    "Where the per-document Export buttons write the decrypted file. Each export \
                     is saved under this directory, recreating the document's folder structure from \
                     inside the vault — you are never asked for a path at export time. Stored as a \
                     local preference (not in the vault), so it can be set even in read-only mode.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("Export directory:");
                // Deliberately NOT gated on `writable`: the export dir is a local preference,
                // so a read-only session (e.g. an heir) can set where to extract documents.
                ui.add(egui::TextEdit::singleline(&mut self.export_dir).hint_text("/path/to/exports").desired_width(fit(ui, 340.0)));
                if ui.button("Set").clicked() {
                    set_export = true;
                }
            });

            ui.add_space(16.0);
            ui.separator();
            config_heading(ui, "Backup");
            ui.label(
                egui::RichText::new(
                    "Copies the encrypted vault and its document archive into a directory, \
                     timestamped to the second. Nothing is decrypted.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("Destination directory:");
                ui.add(egui::TextEdit::singleline(&mut self.backup_dest).hint_text("/path/to/backups").desired_width(fit(ui, 340.0)));
                if ui.button("Backup now").clicked() {
                    do_backup = true;
                }
            });

            if self.writable {
                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Storage — volume size");
                ui.label(
                    egui::RichText::new(format!(
                        "New documents roll into a fresh volume once a partition passes this size. \
                         Current: {cur_volume_mib} MiB. Changing it affects only future placement."
                    ))
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("New size (MiB):");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg_volume_size).hint_text("e.g. 256").desired_width(fit(ui, 140.0)));
                    if ui.button("Set volume size").clicked() {
                        set_volume = true;
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Vault file redundancy (advanced)");
                ui.label(
                    egui::RichText::new(
                        "Keeps extra encrypted copies of the small vault file so a damaged \
                         vault.pmv can be recovered in place: a same-generation mirror plus N \
                         prior generations (also an 'undo last save'). 0 = off. This does NOT \
                         replace off-device backups, and it leaves more old encrypted data on disk.",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("Copies to keep:");
                    egui::ComboBox::from_id_salt("redundancy")
                        .selected_text(if self.cfg_redundancy == 0 { "Off".to_string() } else { self.cfg_redundancy.to_string() })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cfg_redundancy, 0, "Off");
                            for n in 1..=5u32 {
                                ui.selectable_value(&mut self.cfg_redundancy, n, n.to_string());
                            }
                        });
                    if ui.button("Apply").clicked() {
                        set_redundancy = true;
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Update from another vault");
                ui.label(
                    egui::RichText::new(
                        "Pull records that are newer (or new) in ANOTHER vault — together with the \
                         documents they reference — into this one. One-way and additive: it never \
                         deletes anything here. You'll choose the other vault's folder and enter its \
                         two passwords, then preview the exact changes before applying.",
                    )
                    .weak(),
                );
                if ui.button("Update from another vault…").clicked() {
                    start_merge = true;
                }

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Sync types from records");
                ui.label(
                    egui::RichText::new(
                        "Scan every record and add any asset/account type or subtype it uses that \
                         is missing from the lists above — useful after pulling in records (from a \
                         merge or import) whose types aren't yet listed here.",
                    )
                    .weak(),
                );
                if ui.button("Sync types from records").clicked() {
                    sync_types = true;
                }
            }
        });

        // Deferred actions (kept out of the closures to keep borrows simple).
        if add_asset {
            // `.trim()` returns a trimmed `&str`; `.to_string()` makes it owned.
            let name = self.new_asset_type.trim().to_string();
            // `.expect(...)` unwraps the open vault (safe on the config screen).
            // The call returns `Result<bool, _>`: `Ok(true)` = added,
            // `Ok(false)` = no-op (duplicate/empty), `Err` = save failure.
            match self.vault.as_mut().expect("vault open on config").add_asset_type(&name) {
                Ok(true) => {
                    self.status = format!("Added asset/liability type “{name}”.");
                    self.new_asset_type.clear();
                }
                Ok(false) => self.refuse("Type is empty or already exists."),
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if add_account {
            let name = self.new_account_type.trim().to_string();
            match self.vault.as_mut().expect("vault open on config").add_account_type(&name) {
                Ok(true) => {
                    self.status = format!("Added account type “{name}”.");
                    self.new_account_type.clear();
                }
                Ok(false) => self.refuse("Type is empty or already exists."),
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if add_subtype {
            let ty = self.new_subtype_for.clone();
            let sub = self.new_subtype_name.trim().to_string();
            if ty.is_empty() {
                self.refuse("Choose an account type for the subtype.");
            } else {
                match self
                    .vault
                    .as_mut()
                    .expect("vault open on config")
                    .add_account_subtype(&ty, &sub)
                {
                    Ok(true) => {
                        self.status = format!("Added subtype “{sub}” under “{ty}”.");
                        self.new_subtype_name.clear();
                    }
                    Ok(false) => self.refuse("Subtype is empty or already exists."),
                    Err(e) => self.fail(format!("Save failed: {e}")),
                }
            }
        }
        // Deferred DELETE handlers. A refusal (in use / has subtypes) is a normal
        // status message, not a failure; only a real save error reads as "failed".
        if let Some(name) = remove_asset {
            // A save failure must surface in the conspicuous banner (via `fail`), not just the
            // weak status line — a refusal (in use / not found) is an ordinary status message.
            match self.vault.as_mut().expect("vault open on config").remove_asset_type(&name) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted asset/liability type “{name}”."),
                Ok(CategoryRemoval::InUse(n)) => self.refuse(format!("Can’t delete “{name}”: still used by {n} record(s).")),
                Ok(CategoryRemoval::NotFound) => self.refuse(format!("“{name}” was not found.")),
                Ok(CategoryRemoval::HasSubtypes) => unreachable!("asset types have no subtypes"),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if let Some(name) = remove_account {
            match self.vault.as_mut().expect("vault open on config").remove_account_type(&name) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted account type “{name}”."),
                Ok(CategoryRemoval::HasSubtypes) => self.refuse(format!("Can’t delete “{name}”: delete its subtypes first.")),
                Ok(CategoryRemoval::InUse(n)) => self.refuse(format!("Can’t delete “{name}”: still used by {n} account(s).")),
                Ok(CategoryRemoval::NotFound) => self.refuse(format!("“{name}” was not found.")),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if let Some((ty, sub)) = remove_subtype {
            match self.vault.as_mut().expect("vault open on config").remove_account_subtype(&ty, &sub) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted subtype “{sub}” under “{ty}”."),
                Ok(CategoryRemoval::InUse(n)) => self.refuse(format!("Can’t delete “{sub}”: still used by {n} account(s).")),
                Ok(CategoryRemoval::NotFound) => self.refuse(format!("“{sub}” was not found under “{ty}”.")),
                Ok(CategoryRemoval::HasSubtypes) => unreachable!("a subtype has no subtypes"),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if set_export {
            // Held for THIS SESSION only, never written to disk. It names where cleartext
            // exports land — the CSV carries every password in the clear — and the only file
            // this app writes is `<vault_root>/prefs.json`, which anyone with write access to
            // the vault media (but not the passwords) can edit. Persisting it there would let
            // tampering redirect those secrets; see the prefs comment in `prefs.rs`. Normalize
            // the value: trimmed, with a pasted "Copy as path" quote pair stripped.
            let dir = records::unquote_path(&self.export_dir).to_string();
            self.export_dir = dir.clone();
            // Tell the user NOW if the folder they just picked is one every Export button
            // will refuse, instead of letting them discover it at the first export.
            match crate::checked_export_dir(&self.path, &dir) {
                _ if dir.is_empty() => self.status = "Export directory cleared.".into(),
                Ok(_) => self.status = format!("Export directory set to {dir} (this session only)."),
                Err(msg) => self.refuse(msg),
            }
        }
        if do_backup {
            let dest = records::unquote_path(&self.backup_dest).to_string();
            if dest.is_empty() {
                self.refuse("Enter a backup destination directory.");
            } else if let Some(ov) = self.vault.as_ref() {
                // Use the OPEN handle's backup (reuses this session's write lock).
                // Calling the free `vault::backup` here would self-deadlock: it tries
                // to re-acquire the per-fd flock this session already holds → Locked.
                match ov.backup(Path::new(&dest)) {
                    Ok(p) => self.status = format!("Backed up to {}", p.display()),
                    Err(e) => self.fail(format!("Backup failed: {e}")),
                }
            }
        }
        if set_volume {
            // `.parse::<u64>()` parses text into an unsigned 64-bit integer,
            // returning a `Result` (`Err` if the text is not a number).
            match self.cfg_volume_size.trim().parse::<u64>() {
                // A "match guard": this arm matches `Ok(mib)` only if `mib >= 1`.
                Ok(mib) if mib >= 1 => {
                    // `.saturating_mul` multiplies but clamps at the max instead
                    // of overflowing/panicking.
                    let bytes = mib.saturating_mul(1024 * 1024);
                    match self.vault.as_mut().expect("vault open on config").set_volume_max_size(bytes) {
                        Ok(()) => {
                            self.status = format!("Volume size set to {mib} MiB (applies to future documents).");
                            self.cfg_volume_size.clear();
                        }
                        Err(e) => self.fail(format!("Save failed: {e}")),
                    }
                }
                // `_` is the catch-all arm: any other case (parse error, or 0).
                _ => self.refuse("Enter a whole number of MiB (at least 1)."),
            }
        }
        if set_redundancy && self.cfg_redundancy != cur_redundancy {
            let choice = self.cfg_redundancy;
            match self.vault.as_mut().expect("vault open on config").set_redundancy(choice) {
                Ok(()) => {
                    self.status = if choice == 0 {
                        "Vault file redundancy turned off (extra copies removed).".into()
                    } else {
                        format!("Vault file redundancy set to {choice} (mirror + {choice} prior generation(s)).")
                    };
                }
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if start_merge {
            // Enter the merge flow with fresh state. Pre-fill the source folder with the
            // vault root (the folder that holds vaults) as a convenient starting point.
            self.reset_merge();
            self.merge_src_dir = records::unquote_path(&self.vault_root).to_string();
            self.screen = Screen::Merge;
        }
        if sync_types {
            match self.vault.as_mut().expect("vault open on config").sync_types_from_records() {
                Ok(0) => self.status = "Types already in sync — nothing to add.".into(),
                Ok(n) => self.status = format!("Added {n} type(s) from records to the lists."),
                Err(e) => self.fail(format!("Sync failed: {e}")),
            }
        }

        if !self.status.is_empty() {
            ui.separator();
            let text = egui::RichText::new(&self.status);
            let text = if self.status_is_alert() {
                text.color(alert_color(ui.visuals())).strong()
            } else {
                text.weak()
            };
            ui.label(text);
        }
    }
}

/// A Config-screen section heading: accent-colored, with the vertical rhythm that
/// separates one settings group from the next. Config used to run every group
/// together in one undifferentiated column.
fn config_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(text).strong().size(16.0).color(ui_accent(ui)));
    ui.add_space(4.0);
}
