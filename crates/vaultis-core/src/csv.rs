//! Plain-text CSV export of the vault's record tabs (RFC 4180 quoting).
//!
//! Each `*_csv` function turns one tab's records into a CSV string: a header row
//! followed by one row per record, in the same field order as the struct. The text
//! is meant to be written to disk by the front-ends (see `vault::write_export_bytes`)
//! into the user's configured export directory.
//!
//! Design notes:
//! * Document/file columns hold the file **names**, not the internal volume blob ids.
//!   The caller passes a `name_of` resolver (typically `OpenVault::doc_path` reduced to
//!   its basename via [`basename`]); a record with several documents gets them joined
//!   with `"; "`, and a missing/tombstoned id resolves to an empty cell.
//! * Every cell is first run through [`records::display_safe`], which replaces control
//!   characters and Unicode bidi/zero-width spoof characters with `_`. That keeps the
//!   export strictly one physical line per record (embedded newlines become `_`) and
//!   prevents a crafted field from spoofing a spreadsheet/terminal. It does NOT alter
//!   ordinary printable text, so realistic data — including generated passwords — round
//!   trips unchanged. As an anti-formula-injection guard, a cell that BEGINS with `=`,
//!   `+`, `-`, or `@` is additionally prefixed with a single quote so spreadsheets render
//!   it as text (a crafted field cannot become a live `=HYPERLINK`/DDE formula that
//!   exfiltrates the neighboring cleartext-password cells); the quote is a display marker.
//! * Line endings are CRLF, per RFC 4180.
//!
//! Secret handling: the Accounts / Real-Estate CSVs contain plaintext passwords by the
//! user's explicit opt-in, and the front-ends move the FINAL assembled string into a
//! `Zeroizing` buffer (and write the file 0600). The transient intermediates here — the
//! growing `out` buffer's pre-reallocation copies, plus the small per-cell strings from
//! `display_safe`/`esc` — are plain `String`s and are NOT zeroized, so password fragments
//! can briefly linger in freed heap (cf. `vault::serialize_secret_json`, which pre-sizes to
//! avoid this for the on-disk vault). That residue is deliberately tolerated here: the whole
//! point of this export is to put those same secrets on disk in cleartext, so the marginal
//! in-memory exposure is not worth a counting-pass rewrite.

use crate::records::{
    self, Account, AssetLiability, GeneralDocument, Instruction, RealEstate, TaxFiling, TrustWill, Urgent, Vault,
};

/// The basename of a volume virtual path (`taxes/2024/<ts>/w2.pdf` -> `w2.pdf`).
/// Used by the front-ends to turn a resolved `doc_path` into a bare file name.
pub fn basename(virtual_path: &str) -> String {
    virtual_path.rsplit('/').next().unwrap_or(virtual_path).to_string()
}

/// RFC 4180-escape one cell: neutralize control/bidi/zero-width chars, then wrap in
/// double quotes (doubling any internal quote) when the value contains a comma or a
/// quote, or has leading/trailing whitespace. After `display_safe` no CR/LF remains,
/// so quoting is only ever needed for commas, quotes, and edge spaces.
fn esc(s: &str) -> String {
    let safe = records::display_safe(s);
    // Anti-CSV-formula-injection: a cell beginning with '=', '+', '-', or '@' is parsed as
    // a formula by spreadsheets. A crafted field (e.g. a value merged in from a hostile
    // vault) could then run =HYPERLINK/IMPORTXML/WEBSERVICE to exfiltrate the neighboring
    // plaintext-password cells, or legacy DDE to execute. Prefix it with a single quote so
    // the spreadsheet treats it as text. display_safe already stripped any leading TAB/CR.
    let guarded = match safe.chars().next() {
        Some('=' | '+' | '-' | '@') => format!("'{safe}"),
        _ => safe,
    };
    let needs_quote =
        guarded.contains(',') || guarded.contains('"') || guarded.starts_with(' ') || guarded.ends_with(' ');
    if needs_quote {
        let mut out = String::with_capacity(guarded.len() + 2);
        out.push('"');
        out.push_str(&guarded.replace('"', "\"\""));
        out.push('"');
        out
    } else {
        guarded
    }
}

/// Append one CSV record line (escaped cells, comma-separated, CRLF-terminated).
fn row(out: &mut String, cells: &[&str]) {
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&esc(c));
    }
    out.push_str("\r\n");
}

/// A human-readable UTC timestamp `YYYY-MM-DD HH:MM:SSZ` for the created/updated cells.
fn iso_utc(unix: i64) -> String {
    let (y, mo, d, h, mi, s) = records::civil_from_unix(unix);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Resolve a list of document ids to a `"; "`-joined list of file names (empties dropped).
fn join_names(ids: &[String], name_of: &impl Fn(&str) -> String) -> String {
    ids.iter().map(|id| name_of(id)).filter(|n| !n.is_empty()).collect::<Vec<_>>().join("; ")
}

/// Resolve an optional single document id to its file name (empty when `None`/unresolved).
fn opt_name(opt: &Option<String>, name_of: &impl Fn(&str) -> String) -> String {
    opt.as_deref().map(name_of).unwrap_or_default()
}

// --- Per-tab exporters -------------------------------------------------------

pub fn urgent_csv(rows: &[Urgent]) -> String {
    let mut out = String::new();
    row(&mut out, &["id", "title", "description", "created", "updated"]);
    for r in rows {
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(&mut out, &[&r.id, &r.title, &r.description, &created, &updated]);
    }
    out
}

pub fn instructions_csv(rows: &[Instruction]) -> String {
    let mut out = String::new();
    row(&mut out, &["id", "title", "description", "created", "updated"]);
    for r in rows {
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(&mut out, &[&r.id, &r.title, &r.description, &created, &updated]);
    }
    out
}

pub fn trust_wills_csv(rows: &[TrustWill], name_of: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    row(&mut out, &["id", "document", "usage", "file", "created", "updated"]);
    for r in rows {
        let file = opt_name(&r.file, &name_of);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(&mut out, &[&r.id, &r.document, &r.usage, &file, &created, &updated]);
    }
    out
}

/// `account_label_of` resolves a linked Account id to its display label; unlike the
/// document resolver it should fall back to the RAW id for a dangling link (see
/// `build_tab_csv`) so an unresolvable link is exported visibly, not dropped.
pub fn assets_csv(
    rows: &[AssetLiability],
    name_of: impl Fn(&str) -> String,
    account_label_of: impl Fn(&str) -> String,
) -> String {
    let mut out = String::new();
    row(
        &mut out,
        &[
            "id", "kind", "description", "owner", "title", "approx_value", "as_of_date",
            "institution", "asset_type", "url", "beneficiary", "review", "statement",
            "linked_accounts", "created", "updated",
        ],
    );
    for r in rows {
        let review = yn(r.review);
        let statement = opt_name(&r.statement, &name_of);
        let linked = join_names(&r.linked_accounts, &account_label_of);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(
            &mut out,
            &[
                &r.id, &r.kind, &r.description, &r.owner, &r.title, &r.approx_value, &r.as_of_date,
                &r.institution, &r.asset_type, &r.url, &r.beneficiary, review, &statement, &linked,
                &created, &updated,
            ],
        );
    }
    out
}

pub fn accounts_csv(rows: &[Account]) -> String {
    let mut out = String::new();
    row(
        &mut out,
        &[
            "id", "title", "account_type", "account_subtype", "owner", "username", "password",
            "description", "url", "closed_as_of", "review", "created", "updated",
        ],
    );
    for r in rows {
        let review = yn(r.review);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(
            &mut out,
            &[
                &r.id, &r.title, &r.account_type, &r.account_subtype, &r.owner, &r.username,
                &r.password, &r.description, &r.url, &r.closed_as_of, review, &created, &updated,
            ],
        );
    }
    out
}

pub fn real_estate_csv(rows: &[RealEstate], name_of: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    row(
        &mut out,
        &[
            "id", "address", "owner", "taxes", "hoa", "income_account", "financing_account",
            "payment_account", "financing_balance", "property_mgmt_url", "property_mgmt_username",
            "property_mgmt_password", "property_mgmt_comment", "insurance_url", "insurance_username",
            "insurance_password", "insurance_comment", "hoa_url", "hoa_username", "hoa_password",
            "hoa_comment", "tax_portal_url", "tax_portal_username", "tax_portal_password",
            "tax_portal_comment", "comments", "documents", "created", "updated",
        ],
    );
    for r in rows {
        let docs = join_names(&r.documents, &name_of);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(
            &mut out,
            &[
                &r.id, &r.address, &r.owner, &r.taxes, &r.hoa, &r.income_account,
                &r.financing_account, &r.payment_account, &r.financing_balance, &r.property_mgmt_url,
                &r.property_mgmt_username, &r.property_mgmt_password, &r.property_mgmt_comment,
                &r.insurance_url, &r.insurance_username, &r.insurance_password, &r.insurance_comment,
                &r.hoa_url, &r.hoa_username, &r.hoa_password, &r.hoa_comment, &r.tax_portal_url,
                &r.tax_portal_username, &r.tax_portal_password, &r.tax_portal_comment, &r.comments,
                &docs, &created, &updated,
            ],
        );
    }
    out
}

pub fn tax_filings_csv(rows: &[TaxFiling], name_of: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    row(&mut out, &["id", "owner", "year", "notes", "documents", "created", "updated"]);
    for r in rows {
        let docs = join_names(&r.documents, &name_of);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(&mut out, &[&r.id, &r.owner, &r.year, &r.notes, &docs, &created, &updated]);
    }
    out
}

pub fn general_documents_csv(rows: &[GeneralDocument], name_of: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    row(&mut out, &["id", "title", "description", "file", "created", "updated"]);
    for r in rows {
        let file = opt_name(&r.file, &name_of);
        let created = iso_utc(r.created_at);
        let updated = iso_utc(r.updated_at);
        row(&mut out, &[&r.id, &r.title, &r.description, &file, &created, &updated]);
    }
    out
}

// --- Tab dispatch (shared by both front-ends) --------------------------------

/// Which record tab to export. Lets the GUI and TUI share ONE tab -> collection
/// mapping (each front-end has its own `Tab` enum but maps it to this), so adding a
/// record type or changing a base filename is a single-site change here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsvTab {
    Urgent,
    Instructions,
    TrustWill,
    Assets,
    Accounts,
    RealEstate,
    Taxes,
    GeneralDocuments,
}

/// Build the CSV for one tab's records: returns the base filename (the front-ends append
/// `-<timestamp>.csv`), the CSV text, and the record count. `name_of` resolves a document
/// blob id to its file name. The text may contain plaintext passwords (Accounts / Real
/// Estate) by the user's explicit opt-in; the front-ends hold it in `Zeroizing`.
pub fn build_tab_csv(v: &Vault, tab: CsvTab, name_of: impl Fn(&str) -> String) -> (&'static str, String, usize) {
    match tab {
        CsvTab::Urgent => ("urgent", urgent_csv(&v.urgent), v.urgent.len()),
        CsvTab::Instructions => ("instructions", instructions_csv(&v.instructions), v.instructions.len()),
        CsvTab::TrustWill => ("trust-will", trust_wills_csv(&v.trust_wills, name_of), v.trust_wills.len()),
        CsvTab::Assets => {
            // Linked-account cells hold the accounts' display labels. A dangling id
            // (deleted account) falls back to the raw id — exported visibly rather
            // than silently dropped (unlike unresolved doc ids, which have no
            // meaning outside the volume).
            let label_of =
                |id: &str| records::account_label(&v.accounts, id).unwrap_or_else(|| id.to_string());
            ("assets-liabilities", assets_csv(&v.assets, name_of, label_of), v.assets.len())
        }
        CsvTab::Accounts => ("accounts", accounts_csv(&v.accounts), v.accounts.len()),
        CsvTab::RealEstate => ("real-estate", real_estate_csv(&v.real_estate, name_of), v.real_estate.len()),
        CsvTab::Taxes => ("taxes", tax_filings_csv(&v.tax_filings, name_of), v.tax_filings.len()),
        CsvTab::GeneralDocuments => {
            ("general-documents", general_documents_csv(&v.general_documents, name_of), v.general_documents.len())
        }
    }
}

#[cfg(test)]
#[path = "csv_tests.rs"]
mod tests;
