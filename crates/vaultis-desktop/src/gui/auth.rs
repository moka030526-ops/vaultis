//! The lock screen: picking or creating a vault, the two-password unlock / create /
//! change-password flows, and resetting per-vault UI state when a vault opens.

use super::*;

/// The Vaultis brand lockup shown at the top of the lock screen: a vault-door glyph
/// drawn from egui shapes (no image asset — the icon scales crisply and the static/
/// terminal build needs nothing extra) beside the letter-spaced "VAULTIS" wordmark,
/// with the app descriptor beneath. Everything is tinted in the active theme's accent.
///
/// `scale` is [`auth_space_scale`]'s output: below 1.0 the glyph and wordmark shrink
/// proportionally, and under half the tagline is dropped entirely. The mark is the most
/// compressible thing on the screen — it identifies the app, it is not something the user
/// has to read or click — so it yields height before any control does.
fn vaultis_logo(ui: &mut egui::Ui, accent: egui::Color32, scale: f32) {
    // Never below 0.6: past that the wordmark stops reading as a logotype.
    let shrink = 0.6 + 0.4 * scale.clamp(0.0, 1.0);
    ui.vertical_centered(|ui| {
        // The icon + wordmark sit on one row; `vertical_centered` centers the row.
        ui.horizontal(|ui| {
            let sz = 34.0_f32 * shrink;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), egui::Sense::hover());
            let stroke = egui::Stroke::new(2.2_f32, accent);
            let c = rect.center();
            {
                let p = ui.painter();
                // The vault door: a rounded square set just inside the allotted box.
                p.rect_stroke(rect.shrink(2.0), egui::CornerRadius::same(7), stroke, egui::StrokeKind::Inside);
                // The combination dial: an outer ring, a filled hub, and three spokes.
                p.circle_stroke(c, sz * 0.26, stroke);
                p.circle_filled(c, sz * 0.07, accent);
                for k in 0..3 {
                    let a = std::f32::consts::TAU * (k as f32) / 3.0 - std::f32::consts::FRAC_PI_2;
                    let dir = egui::vec2(a.cos(), a.sin());
                    p.line_segment([c + dir * (sz * 0.10), c + dir * (sz * 0.26)], stroke);
                }
            }
            ui.add_space(12.0 * shrink);
            // Letter-spaced wordmark: thin spaces (U+2009) between the glyphs give the
            // tracked, "set" look of a logotype without needing a custom font.
            let word: String = "VAULTIS".chars().map(|ch| ch.to_string()).collect::<Vec<_>>().join("\u{2009}");
            ui.label(egui::RichText::new(word).strong().size(26.0 * shrink).color(accent));
        });
        // The tagline is the first thing to go: it is the only line here that is pure
        // description, repeated verbatim in the Help manual's opening section.
        if scale > 0.5 {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Offline, two-password estate vault").weak().small());
        }
    });
}

impl GuiApp {
    // Returns either `Ok((pw1, pw2))` (a 2-tuple of zeroizing strings) or
    // `Err(message)`. `&self` is a read-only borrow — this validates without
    // mutating. `.into()` converts the string literal `&str` into an owned
    // `String` to match the `Err` type.
    pub(super) fn confirmed_passwords(&self) -> Result<(Zeroizing<String>, Zeroizing<String>), String> {
        if self.pw1.is_empty() || self.pw2.is_empty() {
            return Err("Both passwords are required.".into());
        }
        if self.pw1 != self.confirm1 || self.pw2 != self.confirm2 {
            return Err("Password confirmations do not match.".into());
        }
        // `.clone()` makes owned copies of the password strings; wrapping them in
        // `Zeroizing` means those copies are wiped from the heap when dropped.
        Ok((Zeroizing::new(self.pw1.clone()), Zeroizing::new(self.pw2.clone())))
    }

    pub(super) fn submit_auth(&mut self) {
        // `match` dispatches on the value, like a switch but exhaustive: every
        // variant must be handled. Each `Variant => { ... }` is an arm.
        match self.auth_mode {
            AuthMode::ChangePassword => {
                // Destructure the success tuple into `pw1`/`pw2`; on `Err`, record
                // the message and `return` early from the whole method.
                let (pw1, pw2) = match self.confirmed_passwords() {
                    Ok(p) => p,
                    Err(m) => {
                        self.auth_error = Some(m);
                        return;
                    }
                };
                if let Some(ov) = self.vault.as_mut() {
                    // `.as_bytes()` views the string as a read-only byte slice
                    // (`&[u8]`), which the crypto layer expects.
                    match ov.change_password(pw1.as_bytes(), pw2.as_bytes()) {
                        Ok(()) => {
                            self.status = "Master passwords changed.".into();
                            self.auth_error = None;
                            self.wipe_passwords();
                            self.screen = Screen::Main;
                        }
                        Err(e) => {
                            // The rekey may have left the handle poisoned (read-only)
                            // with a pending `.rekey` on disk. Drop the handle to
                            // release the single-writer lock, then return to the
                            // unlock screen: reopening runs recover_pending_rekey,
                            // which finishes or discards the interrupted rekey
                            // idempotently. Without this the dead handle keeps the
                            // lock and the session can't recover in place.
                            self.vault = None;
                            self.auth_mode = AuthMode::Unlock;
                            self.screen = Screen::Auth;
                            self.wipe_passwords();
                            self.auth_error =
                                Some(format!("Password change interrupted: {e}. Unlock again to recover."));
                        }
                    }
                }
            }
            // `A | B =>` matches either variant with one arm.
            AuthMode::Create | AuthMode::Unlock => self.submit_open_or_create(true),
        }
    }

    /// How a read-only session becomes a writable one, for the messages that refuse a write:
    /// the lock-screen switch where there is one, otherwise a relaunch with `--write`.
    pub(super) fn how_to_write(&self) -> &'static str {
        if self.mode_switchable { "tick “Open for editing” below" } else { "relaunch with --write" }
    }

    /// `remember_root` gates [`launch::save_last_root`] on success: `true` for a real,
    /// user-driven open (the normal path here), `false` for [`Self::open_sample_vault`] —
    /// the demo directory under `target/` is not somewhere the start page should default to
    /// on the next launch.
    pub(super) fn submit_open_or_create(&mut self, remember_root: bool) {
        let creating = self.auth_mode == AuthMode::Create;
        if creating && !self.writable {
            self.auth_error =
                Some(format!("No vault here, and this is read-only. {} to create one.", self.how_to_write()));
            return;
        }
        // `result` is assigned from an `if/else` expression: create a new vault
        // or open an existing one. `self.path.clone()` hands an owned copy of the
        // path to the call (the original stays in `self`).
        let result = if creating {
            let (pw1, pw2) = match self.confirmed_passwords() {
                Ok(p) => p,
                Err(m) => {
                    self.auth_error = Some(m);
                    return;
                }
            };
            OpenVault::create(self.path.clone(), pw1.as_bytes(), pw2.as_bytes(), crate::kdf_params_for_new_vault())
        } else {
            OpenVault::open_with(
                self.path.clone(),
                self.pw1.as_bytes(),
                self.pw2.as_bytes(),
                !self.writable,
            )
        };

        match result {
            Ok(v) => {
                // Remember the ROOT (not which vault within it) so the next bare launch
                // starts here — see `launch::save_last_root` and the `prefs.rs` header comment
                // for why this one pointer lives in the OS data dir while everything else
                // stays inside `<vault_root>/prefs.json`. Skipped for the one-click sample
                // vault: that directory lives under `target/` and is not a real vault
                // location worth defaulting future launches to.
                if remember_root {
                    crate::launch::save_last_root(records::unquote_path(&self.vault_root));
                }

                // If the live vault.pmv was unreadable and we recovered from an
                // in-place redundant copy (§12.8), that notice takes priority — the
                // user needs to know a roll-forward/rollback happened.
                let recovered = v.recovery_notice().map(|s| s.to_string());
                self.status = if let Some(notice) = recovered {
                    notice
                } else if creating {
                    "New vault created.".to_string()
                } else if v.previous_access() == 0 {
                    "Vault unlocked.".to_string()
                } else {
                    // Show the write-generation so a rollback to an older snapshot
                    // is noticeable (§9.12).
                    format!(
                        "Unlocked. Last opened: {} (generation {})",
                        format_time(v.previous_access()),
                        v.opened_generation()
                    )
                };
                // Start the new vault's UI from a clean slate — never inherit the previous
                // session's edit buffers/filters/reveal (see reset_per_vault_ui_state). Done
                // BEFORE installing the vault so nothing from vault A is ever rendered for B.
                self.reset_per_vault_ui_state();
                self.vault = Some(v);
                // Bring the Config type lists into line with what records actually use, so a
                // freshly opened (writable) vault's Config matches its data — types brought in
                // by an older import/merge appear without a manual "sync". This is ADD-ONLY:
                // `sync_types_from_records` only inserts missing types/subtypes, it never
                // deletes a configured entry. Read-only sessions skip it; with no drift it adds
                // nothing and writes nothing. Appended to the open message so a recovery/unlock
                // notice is never clobbered.
                if self.writable {
                    match self.vault.as_mut().map(|ov| ov.sync_types_from_records()) {
                        Some(Ok(n)) if n > 0 => {
                            self.status = format!("{} · Synced {n} type(s) from records.", self.status)
                        }
                        Some(Err(e)) => self.refuse(format!("{} · Type sync failed: {e}", self.status)),
                        _ => {}
                    }
                }
                // The Zakat tab renders from its own whole-table buffer rather than reading
                // the vault every frame, so seed it once the vault is installed. Every other
                // tab's buffer legitimately starts empty (nothing is selected yet); a table
                // has no selection, so an unseeded one would show an empty ledger.
                self.sync_edit_buffer(Tab::Zakat);
                self.auth_error = None;
                self.wipe_passwords();
                self.screen = Screen::Main;
                // A type-sync refusal above was raised on the lock screen, so re-anchor it
                // to the main screen it belongs to, or it would count as stale and vanish.
                if self.refused_in.is_some() {
                    self.refused_in = Some(self.form_context());
                }
            }
            // Collapse every CORRECT-password-reachable failure into ONE message so the
            // unlock screen can't be used as a "this password is correct" oracle: a
            // wrong password yields `Crypto`, while a missing/rolled-back document
            // (`ArchiveMismatch`), corrupt plaintext (`Json`), or storage error are
            // reachable ONLY after a successful decrypt, so a distinct message for them
            // would reveal the password was right (audit O-1; mirrors the FFI collapse).
            // Structural, password-INDEPENDENT errors (bad magic/version/truncated/
            // params/too-large, not-found, locked, rekey-pending) keep their specific,
            // useful messages below — they leak nothing about password correctness.
            Err(VaultError::Crypto(_) | VaultError::ArchiveMismatch | VaultError::Json(_) | VaultError::Storage(_)) => {
                self.auth_error = Some("Wrong password(s) or corrupted/unreadable vault.".into());
                // Wipe the entered passwords on failure too (not just on success), so
                // they don't linger in memory after a failed attempt — the moment a
                // user is most likely to step away. Mirrors the TUI, which rebuilds
                // (and thus zeroizes) its AuthState on a failed unlock.
                self.wipe_passwords();
            }
            // `Err(e)` catches every other (password-independent) error variant.
            Err(e) => {
                self.auth_error = Some(format!("{e}"));
                self.wipe_passwords();
            }
        }
    }

    /// Re-derive the open target from `<vault_root>/<vault_name>`: rebuild `vault_dir` and
    /// `path`, then flip the mode — Unlock if a `vault.pmv` already exists there, else Create
    /// (which, in --write mode, creates the directory + vault on submit). Called whenever the
    /// root, the vault name, or the dropdown selection changes.
    pub(super) fn recompute_vault_path(&mut self) {
        self.vault_dir = crate::launch::join_root_name(&self.vault_root, &self.vault_name);
        self.path = crate::launch::vault_file(&self.vault_dir);
        self.auth_mode = if self.path.exists() { AuthMode::Unlock } else { AuthMode::Create };
        self.auth_error = None;
    }

    /// Re-scan `vault_root` for vaults (one level deep) and refresh the dropdown items
    /// plus any access warning. Called when the root field changes.
    pub(super) fn refresh_discovered_vaults(&mut self) {
        let scan = crate::launch::discover_vaults(&self.vault_root);
        self.discovered_vaults = scan.vaults;
        self.vault_scan_warning = scan.warning;
    }

    /// Adopt `<vault_root>/prefs.json` after the root changed, and apply it immediately.
    ///
    /// Preferences live in the root, but the root is chosen ON the lock screen — so at boot
    /// there is often no root yet (a bare launch starts empty) and the built-in defaults are
    /// what got applied. Without this, pointing at a root that carries its own look would do
    /// nothing until the next launch, which defeats the whole point of a portable root.
    ///
    /// The three look settings are applied to the context DIRECTLY here, and `applied_*` is
    /// set to match, rather than letting `render`'s "changed since applied" path do it. That
    /// path also SAVES, which would write a `prefs.json` into any folder the user merely
    /// browsed to — the file must appear only when a setting is deliberately changed.
    pub(super) fn adopt_root_prefs(&mut self, ctx: &egui::Context) {
        let root = self.vault_root.clone();
        self.theme = load_theme(&root);
        self.applied_theme = self.theme;
        apply_theme(ctx, self.theme);

        self.ui_scale = load_ui_scale(&root);
        self.applied_ui_scale = self.ui_scale;
        apply_ui_scale(ctx, self.ui_scale);

        self.font = load_font_choice(&root);
        self.applied_font = self.font;
        apply_fonts(ctx, self.font);

        // View defaults seed the live view state the same way `GuiApp::new` does.
        self.group_assets_default = crate::load_group_assets_default(&root);
        self.group_accounts_default = crate::load_group_accounts_default(&root);
        self.asset_grouped = self.group_assets_default;
        self.acct_grouped = self.group_accounts_default;
    }

    /// Pick a vault `name` from the dropdown: set the vault name and re-derive the
    /// path/mode so the user lands ready to unlock it.
    pub(super) fn select_vault(&mut self, name: &str) {
        self.vault_name = name.to_string();
        self.recompute_vault_path();
    }

    pub(super) fn wipe_passwords(&mut self) {
        self.pw1.zeroize();
        self.confirm1.zeroize();
        self.pw2.zeroize();
        self.confirm2.zeroize();
        self.merge_pw1.zeroize();
        self.merge_pw2.zeroize();
    }

    /// Point the start page at the build script's sample vault, fill in its two demo
    /// passwords, and open it — the one-click version of the walk-through in "Trying it out
    /// on a sample vault".
    ///
    /// This can only ever OPEN, never create. `self.sample_vault` is resolved once, in the
    /// constructor, so by the time the button is clicked the directory may be gone (a
    /// `cargo clean` in another terminal removes it — it lives under `target/`). Left to
    /// `submit_open_or_create`, a missing vault means `AuthMode::Create`, and in a `--write`
    /// session that would silently BUILD a real vault locked with the two publicly-known
    /// demo passwords, at a path the user believed already held a throwaway sample. The
    /// same arm catches a directory whose name is not valid UTF-8, where the lossy
    /// round-trip through `vault_name` would resolve to a different, non-existent path.
    pub(super) fn open_sample_vault(&mut self, dir: std::path::PathBuf) {
        self.wipe_passwords();
        self.vault_root = dir.parent().map(|p| p.display().to_string()).unwrap_or_default();
        self.vault_name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.refresh_discovered_vaults();
        self.recompute_vault_path();
        if self.auth_mode != AuthMode::Unlock {
            self.sample_vault = None; // it is gone; stop offering it
            self.auth_error =
                Some(format!("No sample vault at {} any more — rebuild it with scripts/build.sh.", dir.display()));
            return;
        }

        self.pw1.push_str(crate::launch::SAMPLE_PW1);
        self.pw2.push_str(crate::launch::SAMPLE_PW2);
        self.submit_open_or_create(false);
    }

    /// Clear every piece of PER-VAULT UI state to its fresh-launch default. Called on each
    /// successful open so a newly-unlocked vault never inherits the previous session's edit
    /// buffers (which can hold cleartext passwords), armed delete, active filters/search, or
    /// reveal toggles. Without this, an error path that drops the vault back to the unlock
    /// screen WITHOUT going through the constructor (e.g. a change-password or merge-apply
    /// failure) leaves vault A's state visible after vault B is opened — cross-vault secret
    /// leakage and confusing filter carryover. The edit buffers are `Zeroize`-on-drop, so
    /// replacing them here also wipes any secret they held.
    pub(super) fn reset_per_vault_ui_state(&mut self) {
        self.tab = Tab::Urgent;
        self.edit_urgent = None;
        self.edit_instruction = None;
        self.edit_trustwill = None;
        self.edit_asset = None;
        self.edit_account = None;
        self.edit_realestate = None;
        self.edit_taxfiling = None;
        self.edit_general = None;
        // `Vec::new()` replaces (and so drops, and so zeroizes) the previous vault's ledger.
        self.edit_zakat = Vec::new();
        self.pending_account_delete = None;
        // Reveal + grouping return to the saved view DEFAULTS (not hard false), matching the
        // constructor and the tab-switch reset.
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.acct_grouped = self.group_accounts_default;
        self.asset_grouped = self.group_assets_default;
        // Filters + searches back to "no filter".
        self.acct_filter_type.clear();
        self.acct_filter_subtype.clear();
        self.acct_filter_owner.clear();
        self.acct_filter_title.clear();
        self.acct_filter_review = false;
        self.acct_search_user.clear();
        self.asset_filter_review = false;
        self.link_search.clear();
        // Any half-typed document-upload inputs from the prior vault.
        self.clear_doc_inputs();
    }

    // `&mut egui::Ui` is the drawing surface, borrowed mutably so widgets can be
    // added to it. egui is immediate-mode: this method re-runs every frame.
    pub(super) fn ui_auth(&mut self, ui: &mut egui::Ui) {
        // The lock screen is the app's front door and the only screen an heir may ever
        // see, so it is presented as one centered, width-limited card rather than a
        // full-width form: a narrow measure is easier to read, and the card gives the
        // password fields a visible boundary. Purely presentational — `ui_auth_inner`
        // holds the entire flow unchanged.
        let accent = accent(self.theme);
        // Everything decorative on this screen is spent through `k`, so that a window too
        // short for the comfortable layout gives up padding instead of putting the password
        // fields behind a scrollbar. Sampled once, before anything is drawn, so every gap
        // below is measured against the same height.
        let k = auth_space_scale(ui.available_height());
        ui.add_space(24.0 * k);
        vaultis_logo(ui, accent, k);
        ui.add_space(14.0 * k);
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);
            card(ui, |ui| {
                self.ui_auth_inner(ui, k);
            });
            ui.add_space(10.0 * k);
            // The mode the session will open in, stated before the password is typed
            // rather than discovered afterwards by a control that is missing. Where the
            // mode is chosen here rather than at launch (macOS), the statement is the
            // switch itself — but never mid-"Change master passwords", which renders this
            // same screen over an OPEN vault whose mode is already fixed.
            if self.mode_switchable && self.auth_mode != AuthMode::ChangePassword {
                ui.checkbox(&mut self.writable, "Open for editing").on_hover_text(
                    "Off, the vault opens read-only and nothing in it can be changed by \
                     accident — the right way to look something up. Turn it on to add, edit \
                     or delete entries, or to create a new vault.",
                );
            } else if self.writable {
                ui.label(egui::RichText::new("This session can make changes (--write).").weak().small());
            } else {
                ui.label(
                    egui::RichText::new("🔒 Read-only session — relaunch with --write to make changes.")
                        .weak()
                        .small(),
                );
            }

            // Only shown on the actual lock screen (never mid-"Change master passwords" —
            // this same widget renders that screen too, and clicking Sample would abandon
            // an in-progress password change and open an unrelated vault instead), and only
            // when `scripts/build.sh` actually built a demo vault at the resolved location
            // (see `launch::sample_vault_dir`) — never a button that would fail on click.
            // One click fills in its folder and its two demo passwords (sample1/sample2)
            // and opens it directly, honouring whatever read-only/write mode this session
            // was launched in, same as any other vault.
            if self.auth_mode != AuthMode::ChangePassword
                && let Some(dir) = self.sample_vault.clone()
            {
                ui.add_space(10.0 * k);
                if ui
                    .button("Sample vault")
                    .on_hover_text(
                        "Open a throwaway practice vault full of invented data — see \
                         “Trying it out on a sample vault” in Help.",
                    )
                    .clicked()
                {
                    self.open_sample_vault(dir);
                }
            }

            // --- Footer: the way in to the manual, from the front door ------------
            //
            // The lock screen may be the ONLY screen an heir ever sees, and until now
            // the manual was reachable only from the top bar — i.e. only *after*
            // successfully unlocking. Someone handed this program and two passwords in
            // an envelope had nowhere to turn before typing them.
            //
            // Composed as a quiet footer rather than a button beside "Unlock": the
            // primary action must stay unambiguous, so this sits below the card,
            // separated by a hairline, in the same weak/small register as the mode
            // line above it. The question is asked before the link is offered, because
            // someone who needs it is looking for a sentence that describes their
            // situation, not a control they already know how to use.
            ui.add_space(18.0 * k);
            // A hairline the width of the card, so the footer reads as part of the same
            // composition instead of floating text below it.
            ui.scope(|ui| {
                ui.set_max_width(360.0);
                ui.separator();
            });
            ui.add_space(10.0 * k);
            ui.label(
                egui::RichText::new("New to this, or settling an estate?").weak().small(),
            );
            ui.add_space(2.0 * k);
            if ui
                .link(egui::RichText::new("❓  Read the guide").color(accent))
                .on_hover_text(
                    "The built-in manual: what this program is, how to open a vault, \
                     and what to do if you are an executor. No password needed.",
                )
                .clicked()
            {
                // Wipe any partly-typed master passwords before leaving for the manual.
                // Reading a guide is an open-ended pause — plausibly the longest the
                // program is left unattended — and there is no reason for two plaintext
                // passwords to sit in memory (and in the fields, behind their mask) for
                // the duration. The desktop already wipes on a failed unlock and on the
                // change-password transition for the same reason; this is the same rule
                // applied to the same kind of moment. Retyping after reading the manual
                // is the expected flow anyway.
                self.wipe_passwords();
                self.auth_error = None;
                // Back must come here, not to the vault UI — there is no vault open yet.
                self.help_return = Screen::Auth;
                self.screen = Screen::Help;
            }
            ui.add_space(6.0 * k);
        });
    }

    /// The unlock/create/change-password form itself (see [`Self::ui_auth`], which
    /// frames it).
    ///
    /// `k` is the caller's [`auth_space_scale`], applied to this form's gaps for the same
    /// reason it is applied outside the card: on a short window the screen tightens rather
    /// than scrolls. Only the gaps — the fields, the button and the messages between them
    /// keep their full size at every window height.
    pub(super) fn ui_auth_inner(&mut self, ui: &mut egui::Ui, k: f32) {
        // `match` used as an expression: it yields a `(heading, help)` pair which
        // we immediately destructure into two named bindings.
        ui.add_space(4.0 * k);
        // On the start page (not the in-vault Change-password flow) the user picks the vault
        // by ROOT + a collapsed "Vault" box: an editable ROOT path scanned (one level deep)
        // for vaults, and a Vault box that the dropdown fills — pick an existing vault, or
        // TYPE a new folder name to create one. Both editable in read-only AND --write mode.
        // The open target is always `<root>/<name>`. Rendered FIRST so the heading/confirm
        // fields below reflect the just-updated mode.
        if self.auth_mode != AuthMode::ChangePassword {
            // Deferred edits/picks gathered during the (borrow-locked) closure, applied after
            // it returns so the handlers can take `&mut self` freely.
            let mut root_changed = false;
            let mut name_changed = false;
            let mut picked: Option<String> = None;
            // The dropdown's button text: the current name, or a placeholder.
            let current = self.vault_name.trim().to_string();
            let selected_text = if !current.is_empty() {
                current.clone()
            } else if self.discovered_vaults.is_empty() {
                "(no vaults found)".to_string()
            } else {
                "— choose —".to_string()
            };
            ui.vertical_centered(|ui| {
                // Editable ROOT path: the folder scanned (one level deep) for vaults.
                const ROOT_TIP: &str =
                    "The folder that contains your vault's folder. E.g. for D:\\Vaults\\recent, the root is D:\\Vaults.";
                ui.label("Vault root").on_hover_text(ROOT_TIP);
                let resp = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.vault_root)
                            .hint_text("/path/that/holds/vault-folders")
                            .desired_width(fit(ui, 360.0)),
                    )
                    .on_hover_text(ROOT_TIP);
                root_changed = resp.changed();
                ui.add_space(4.0 * k);
                // The "Vault" control: an editable leaf-name box plus a dropdown of the
                // vaults discovered under the root. Pick one to fill the box (→ Unlock), or
                // type a new name (→ Create, in --write mode). Empty = the root itself.
                const NAME_TIP: &str =
                    "The vault's folder name, e.g. recent. Pick one from the list, or type a new name to create one.";
                ui.label("Vault").on_hover_text(NAME_TIP);
                ui.horizontal(|ui| {
                    let resp = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.vault_name)
                                .hint_text("vault name")
                                .desired_width(fit(ui, 244.0)),
                        )
                        .on_hover_text(NAME_TIP);
                    name_changed = resp.changed();
                    egui::ComboBox::from_id_salt("vault_picker")
                        .selected_text(selected_text)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for name in &self.discovered_vaults {
                                if ui.selectable_label(current == *name, name).clicked() {
                                    picked = Some(name.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text("Vaults found in the root folder.");
                });
                // Surface a scan problem (root unreadable, or entries skipped) plainly.
                if let Some(warn) = &self.vault_scan_warning {
                    ui.colored_label(egui::Color32::from_rgb(190, 120, 50), warn);
                }
            });
            if root_changed {
                self.refresh_discovered_vaults();
                self.recompute_vault_path();
                // Keep the default backup destination tracking the root until the vault is
                // unlocked (the Config backup field is freely editable afterwards).
                self.backup_dest = records::unquote_path(&self.vault_root).to_string();
                // Preferences live in the root, so a new root means a new (or absent)
                // prefs.json — adopt it now rather than at the next launch.
                self.adopt_root_prefs(ui.ctx());
            }
            if name_changed {
                self.recompute_vault_path();
            }
            if let Some(name) = picked {
                self.select_vault(&name);
            }
            ui.add_space(8.0 * k);
        }

        // `auth_mode` answers "does a vault exist at the current target?", which is a fact
        // about the DISK and stays true regardless of how this session was launched. Whether
        // the create AFFORDANCES are shown is a separate question, and the answer in a
        // read-only session is no: creating is refused at submit (see `submit_open_or_create`),
        // so offering a "Create vault" heading, a "choose two passwords" instruction, two
        // confirmation fields and a "Create vault" button describes an action this session
        // cannot perform. An heir who was handed the View shortcut and lands on a folder with
        // no vault should not be walked through creating one.
        //
        // The root/name fields stay live either way — retyping them to find the real vault is
        // exactly what that heir needs to do — and the warning below says why creating is not
        // on offer.
        let offer_create = self.auth_mode == AuthMode::Create && self.writable;
        let (heading, help) = match self.auth_mode {
            AuthMode::Create if offer_create => {
                ("Create vault", "Choose two passwords. Both are required to open this vault.")
            }
            // Read-only with nothing at the target: the screen is still the way IN to a vault,
            // so it reads as one rather than as a create form that will be refused.
            AuthMode::Create => ("Unlock vault", "Enter both passwords to unlock."),
            AuthMode::Unlock => ("Unlock vault", "Enter both passwords to unlock."),
            AuthMode::ChangePassword => ("Change master passwords", "Set two new passwords."),
        };
        // Confirmations exist to catch a typo in a password being SET. Nothing is being set
        // here unless a vault is actually being created or its passwords changed.
        let confirm = offer_create || self.auth_mode == AuthMode::ChangePassword;

        // `|ui| { ... }` is a closure (anonymous function). egui passes a child
        // `ui` into it so everything inside is laid out vertically and centered.
        ui.vertical_centered(|ui| {
            ui.heading(heading);
            ui.label(egui::RichText::new(format!("Vault: {}", self.path.display())).weak());
            ui.label(help);
            // In read-only mode an empty directory can't be created — say so plainly, but
            // only once that is actually true. `auth_mode == Create` merely means nothing
            // exists at the CURRENT `<root>/<name>` — which is also the state of a totally
            // blank start page (nothing specified yet: not a real warning) and of a root
            // that DOES hold vaults but has none picked yet (the dropdown is sitting right
            // there — "no vault in this folder" would be actively wrong). So this is
            // gated to the cases where it is true: a root was actually given, and either a
            // specific (nonexistent) name was typed, or the root holds no vaults at all.
            let create_blocked_by_read_only = self.auth_mode == AuthMode::Create
                && !self.writable
                && !self.vault_root.trim().is_empty()
                && (!self.vault_name.trim().is_empty() || self.discovered_vaults.is_empty());
            if create_blocked_by_read_only {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 120, 50),
                    format!("No vault in this folder. Read-only — {} to create one.", self.how_to_write()),
                );
            }
        });
        ui.add_space(16.0 * k);

        // Track whether the user requested submission; `|=` ORs in `true` if any
        // password field had Enter pressed (see `password_field`'s return value).
        let mut submit = false;
        // A built-in Ctrl+C/cut of a master-password field surfaces here so we can arm
        // the clipboard auto-clear/exit-wipe (the field can't reach `self` itself).
        let mut copied: Option<Zeroizing<String>> = None;
        // Hover help for each field, shown on the label and on the field alike. When passwords
        // are being SET, the tip is the one thing worth saying then: there is no recovery.
        let (pw1_tip, pw2_tip) = if confirm {
            (
                "First password. Write it down — it cannot be recovered.",
                "Second password. Write it down — it cannot be recovered.",
            )
        } else {
            ("First password. Order matters.", "Second password. Both are required.")
        };
        const CONFIRM_TIP: &str = "Retype it to catch a typo.";
        egui::Grid::new("auth_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Password 1").on_hover_text(pw1_tip);
            // `&mut self.pw1` lends the field to the widget so typing updates it.
            submit |= password_field(ui, "auth_pw1", &mut self.pw1, &mut copied, Some(pw1_tip));
            ui.end_row();
            if confirm {
                ui.label("Confirm password 1").on_hover_text(CONFIRM_TIP);
                submit |= password_field(ui, "auth_confirm1", &mut self.confirm1, &mut copied, Some(CONFIRM_TIP));
                ui.end_row();
            }
            ui.label("Password 2").on_hover_text(pw2_tip);
            submit |= password_field(ui, "auth_pw2", &mut self.pw2, &mut copied, Some(pw2_tip));
            ui.end_row();
            if confirm {
                ui.label("Confirm password 2").on_hover_text(CONFIRM_TIP);
                submit |= password_field(ui, "auth_confirm2", &mut self.confirm2, &mut copied, Some(CONFIRM_TIP));
                ui.end_row();
            }
        });
        // Route a copied master password through the hardened + armed clipboard path.
        if let Some(pw) = copied {
            self.copy_to_clipboard(pw);
        }

        ui.add_space(8.0 * k);
        // `&self.auth_error` borrows the Option so we can read the message
        // without moving it out; show it only when an error is present.
        if let Some(err) = &self.auth_error {
            ui.colored_label(egui::Color32::from_rgb(190, 50, 50), err);
            ui.add_space(4.0 * k);
        }

        ui.horizontal(|ui| {
            // Same reasoning as the heading above: a read-only session cannot create, so the
            // button never offers to. `offer_create` already folds in `writable`.
            let (label, tip) = match self.auth_mode {
                AuthMode::Create if offer_create => ("Create vault", "Create a new encrypted vault here."),
                AuthMode::Create | AuthMode::Unlock => ("🔓 Unlock", "Open the vault."),
                AuthMode::ChangePassword => ("Change passwords", "Replace both passwords."),
            };
            // The one action of this screen, drawn as the primary (filled) button.
            let accent = accent(self.theme);
            if ui
                .add_sized(
                    [150.0, 28.0],
                    egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE)).fill(accent),
                )
                .on_hover_text(tip)
                .clicked()
            {
                submit = true;
            }
            if self.auth_mode == AuthMode::ChangePassword && ui.button("Cancel").clicked() {
                self.auth_error = None;
                self.wipe_passwords();
                self.screen = Screen::Main;
            }
        });

        if submit {
            self.submit_auth();
        }
    }
}
