//! Plain (unencrypted) exports out of the vault: a tab as CSV, and a document into the
//! configured export directory.

use super::*;

impl GuiApp {
    /// Build the CSV text for the current tab's records (ALL of them, ignoring any display
    /// filter), plus a base filename and the record count. The tab -> collection mapping
    /// lives in the shared `csv::build_tab_csv` core helper; this only maps the GUI's local
    /// `Tab` to `csv::CsvTab`. The `Summary => None` arm keeps the match exhaustive — Summary
    /// has no records and shows no CSV button, so it is unreachable from the GUI. Document/
    /// file columns hold file NAMES. The result is wrapped in `Zeroizing` because it can
    /// contain plaintext passwords (Accounts / Real Estate portals).
    pub(super) fn build_tab_csv(&self) -> Option<(&'static str, Zeroizing<String>, usize)> {
        let ov = self.vault.as_ref()?;
        let tab = match self.tab {
            Tab::Urgent => csv::CsvTab::Urgent,
            Tab::Instructions => csv::CsvTab::Instructions,
            Tab::TrustWill => csv::CsvTab::TrustWill,
            Tab::Assets => csv::CsvTab::Assets,
            Tab::Accounts => csv::CsvTab::Accounts,
            Tab::RealEstate => csv::CsvTab::RealEstate,
            Tab::Taxes => csv::CsvTab::Taxes,
            Tab::GeneralDocuments => csv::CsvTab::GeneralDocuments,
            Tab::Zakat => csv::CsvTab::Zakat,
            Tab::Summary => return None,
        };
        let name_of = |id: &str| ov.doc_path(id).map(|p| csv::basename(&p)).unwrap_or_default();
        let (base, text, n) = csv::build_tab_csv(&ov.vault, tab, name_of);
        Some((base, Zeroizing::new(text), n))
    }

    /// Export every record on the current tab to a timestamped CSV in the configured
    /// export directory (e.g. `accounts-20240628-143000.csv`).
    ///
    /// Available in READ-ONLY sessions, like document export — the vault owner asked for
    /// this explicitly, overriding the earlier write-mode gate. A CSV can hold every
    /// record's plaintext password, so the warning the gate used to enforce now travels
    /// with the feature instead: it is on the button's tooltip and on every success line.
    pub(super) fn export_current_tab_csv(&mut self) {
        // Available in READ-ONLY sessions too, at the vault owner's explicit request. The
        // file it writes is plain, unencrypted text and — on Accounts and Real Estate —
        // contains every password in the clear, so the status line below says so rather
        // than reporting a bare success.
        // Refuses an unset directory AND one inside the vault folder: this CSV is
        // unencrypted and carries every password, so it must never land where the user's
        // next backup of the vault picks it up (the same rule the CLI's extract/export-tree
        // enforce).
        let dir = match crate::checked_export_dir(&self.path, &self.export_dir) {
            Ok(d) => d,
            Err(msg) => {
                self.fail(msg);
                return;
            }
        };
        let Some((base, text, n)) = self.build_tab_csv() else {
            self.status = "Nothing to export on this tab.".into();
            return;
        };
        let filename = format!("{base}-{}.csv", records::compact_utc(records::unix_now()));
        match vault::write_export_bytes(&dir, &filename, text.as_bytes()) {
            Ok(p) => {
                // Caveat FIRST — the path can be arbitrarily long and the status strip
                // truncates. See `EXPORT_CAVEAT_PREFIX`.
                self.status = format!(
                    "{EXPORT_CAVEAT_PREFIX} — this CSV holds every password in the clear. \
                     Exported {n} record(s) to {}",
                    p.display()
                );
            }
            Err(e) => self.fail(format!("CSV export failed: {e}")),
        }
    }

    /// Export document `id` into the configured export directory, recreating its volume
    /// folder structure under it. Used by every tab's Export button — there is no
    /// per-export path prompt; the destination is the directory set in Config (which is
    /// editable even in read-only mode, so this works for a read-only heir).
    pub(super) fn export_doc_to_config_dir(&mut self, id: &str) {
        // Same guard as the CSV path: unset, or inside the vault folder, is refused —
        // the file written here is the DECRYPTED document.
        let dir = match crate::checked_export_dir(&self.path, &self.export_dir) {
            Ok(d) => d,
            Err(msg) => {
                self.fail(msg);
                return;
            }
        };
        if let Some(ov) = self.vault.as_ref() {
            match ov.export_document_into(id, &dir) {
                Ok(p) => {
                    // Caveat FIRST — see `EXPORT_CAVEAT_PREFIX`.
                    self.status = format!(
                        "{EXPORT_CAVEAT_PREFIX} — this copy is a plain, readable file. \
                         Exported to {}",
                        p.display()
                    )
                }
                Err(e) => self.fail(format!("Export failed: {e}")),
            }
        }
    }
}
