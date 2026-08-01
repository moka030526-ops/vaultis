//! Unit tests for the parent module ([`super`], `csv.rs`), split into their own
//! file via `#[cfg(test)] #[path = "csv_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use super::*;

// A resolver that just echoes the id as a "name", to exercise document columns.
fn echo(id: &str) -> String {
    id.to_string()
}

#[test]
fn basename_takes_last_path_component() {
    assert_eq!(basename("taxes/2024/20240101-000000/w2.pdf"), "w2.pdf");
    assert_eq!(basename("flat.txt"), "flat.txt");
    assert_eq!(basename(""), "");
}

#[test]
fn esc_quotes_only_when_needed() {
    assert_eq!(esc("plain"), "plain");
    assert_eq!(esc("a,b"), "\"a,b\"");
    assert_eq!(esc("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(esc(" leading"), "\" leading\"");
    assert_eq!(esc("trailing "), "\"trailing \"");
    // Control chars (here a newline) are neutralized to '_', not quoted.
    assert_eq!(esc("line1\nline2"), "line1_line2");
}

#[test]
fn esc_neutralizes_leading_formula_triggers() {
    // A cell starting with a spreadsheet formula trigger is prefixed with a quote.
    assert_eq!(esc("=1+1"), "'=1+1");
    assert_eq!(esc("+CALL"), "'+CALL");
    assert_eq!(esc("-2"), "'-2");
    assert_eq!(esc("@SUM"), "'@SUM");
    // Quoting still applies on top when the (guarded) cell needs it (here a comma).
    assert_eq!(esc("=A1,B1"), "\"'=A1,B1\"");
    // A trigger char NOT at the start is untouched — realistic passwords round-trip.
    assert_eq!(esc("p@ss=w0rd"), "p@ss=w0rd");
}

#[test]
fn accounts_csv_has_header_and_one_row_per_record_with_password() {
    let mut a = Account::new().unwrap();
    a.id = "id1".into();
    a.title = "My, Bank".into(); // comma forces quoting
    a.owner = "Jane".into();
    a.username = "jane".into();
    a.password = "p@ss=w0rd".into(); // included verbatim (no anti-formula mangling)
    a.review = true;
    let out = accounts_csv(&[a]);
    let lines: Vec<&str> = out.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "header + one record");
    assert!(lines[0].starts_with("id,title,account_type"));
    assert!(lines[0].contains(",password,"));
    assert!(lines[1].contains("\"My, Bank\""), "comma field is quoted");
    assert!(lines[1].contains("p@ss=w0rd"), "password exported in plaintext");
    assert!(lines[1].contains(",yes,"), "review bool rendered yes/no");
}

#[test]
fn tax_filings_csv_owner_year_and_doc_names() {
    let mut t = TaxFiling::new().unwrap();
    t.id = "t1".into();
    t.owner = "Joint".into();
    t.year = "2024".into();
    t.documents = vec!["docA".into(), "docB".into()];
    let out = tax_filings_csv(&[t], echo);
    let line = out.split("\r\n").nth(1).unwrap();
    assert!(line.contains("Joint"));
    assert!(line.contains("2024"));
    assert!(line.contains("docA; docB"), "multiple doc names joined with '; '");
}

#[test]
fn empty_list_still_emits_header_only() {
    let out = instructions_csv(&[]);
    assert_eq!(out, "id,title,description,created,updated\r\n");
}

#[test]
fn realestate_csv_resolves_document_names_and_keeps_portal_passwords() {
    let mut r = RealEstate::new().unwrap();
    r.id = "re1".into();
    r.address = "1 Main St".into();
    r.tax_portal_password = "secret".into();
    r.documents = vec!["deed".into()];
    let out = real_estate_csv(&[r], echo);
    let line = out.split("\r\n").nth(1).unwrap();
    assert!(line.contains("1 Main St"));
    assert!(line.contains("secret"), "portal password kept in plaintext");
    assert!(line.contains("deed"), "document name resolved");
}

#[test]
fn assets_csv_header_columns_review_and_statement() {
    let mut a = AssetLiability::new().unwrap();
    a.id = "a1".into();
    a.kind = "Asset".into();
    a.description = "Brokerage".into();
    a.owner = "Jane".into();
    a.review = true;
    a.statement = Some("stmt".into());
    let out = assets_csv(&[a], echo, echo);
    let lines: Vec<&str> = out.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("id,kind,description,owner,title,approx_value"), "header field order: {}", lines[0]);
    assert!(lines[0].ends_with(",statement,linked_accounts,created,updated"));
    assert!(lines[1].contains(",yes,"), "review=true -> yes");
    assert!(lines[1].contains("stmt"), "statement doc id resolved to a name");
}

#[test]
fn assets_csv_exports_linked_account_labels_with_raw_id_fallback() {
    let mut a = AssetLiability::new().unwrap();
    a.id = "a1".into();
    a.linked_accounts = vec!["acc1".into(), "gone".into()];
    // Mirror build_tab_csv's resolver contract: label when the account exists,
    // RAW id when it doesn't (a dangling link stays visible in the export).
    let label_of = |id: &str| if id == "acc1" { "Bank - jane".to_string() } else { id.to_string() };
    let out = assets_csv(&[a], echo, label_of);
    let line = out.split("\r\n").nth(1).unwrap();
    assert!(line.contains("Bank - jane; gone"), "labels joined with '; ', dangling id raw: {line}");
}

#[test]
fn build_tab_csv_assets_resolves_linked_accounts_from_the_vault() {
    let mut v = Vault::default();
    let mut acc = Account::new().unwrap();
    acc.id = "acc1".into();
    acc.title = "Bank".into();
    acc.username = "jane".into();
    v.accounts.push(acc);
    let mut a = AssetLiability::new().unwrap();
    a.linked_accounts = vec!["acc1".into(), "gone".into()];
    v.assets.push(a);
    let (_base, text, n) = build_tab_csv(&v, CsvTab::Assets, echo);
    assert_eq!(n, 1);
    let line = text.split("\r\n").nth(1).unwrap();
    assert!(line.contains("Bank - jane"), "live link resolved to the account label: {line}");
    assert!(line.contains("gone"), "dangling link exported as the raw id: {line}");
}

#[test]
fn trust_wills_and_general_documents_resolve_optional_file_or_empty() {
    let mut tw = TrustWill::new().unwrap();
    tw.id = "tw1".into();
    tw.document = "Living Trust".into();
    tw.file = Some("trust.pdf".into());
    let twl = trust_wills_csv(&[tw], echo);
    assert!(twl.split("\r\n").next().unwrap().starts_with("id,document,usage,file,"));
    assert!(twl.split("\r\n").nth(1).unwrap().contains("trust.pdf"));

    let mut g = GeneralDocument::new().unwrap();
    g.id = "g1".into();
    g.title = "Passport".into();
    g.file = None; // no attachment -> empty file cell, no panic
    let gl = general_documents_csv(&[g], echo);
    let row = gl.split("\r\n").nth(1).unwrap();
    // The penultimate column group is `file,created,updated`; file must be empty.
    assert!(row.starts_with("g1,Passport,"), "row: {row}");
}

#[test]
fn join_names_drops_unresolved_ids_without_trailing_separator() {
    // A resolver that fails to resolve "gone" (tombstoned/missing doc) -> empty name.
    let resolve = |id: &str| if id == "gone" { String::new() } else { id.to_string() };
    let mut t = TaxFiling::new().unwrap();
    t.id = "t".into();
    t.documents = vec!["deed".into(), "gone".into()];
    let line = tax_filings_csv(&[t], resolve).split("\r\n").nth(1).unwrap().to_string();
    assert!(line.contains("deed"), "live doc name present");
    assert!(!line.contains("deed;"), "no dangling '; ' for the dropped unresolved id: {line}");
    assert!(!line.contains("; "), "the only doc cell holds a single resolved name");
}

#[test]
fn iso_utc_is_fixed_width_for_extreme_timestamps() {
    // civil_from_unix clamps the domain, so the CSV created/updated columns stay fixed-width
    // even for a crafted i64::MAX/MIN timestamp (no widened year breaking the column).
    assert_eq!(iso_utc(0), "1970-01-01 00:00:00Z");
    assert!(iso_utc(i64::MAX).starts_with("9999-12-31"));
    assert_eq!(iso_utc(i64::MAX).len(), iso_utc(0).len(), "fixed width regardless of value");
    assert_eq!(iso_utc(i64::MIN), "1970-01-01 00:00:00Z");
}

#[test]
fn u2028_line_separator_is_neutralized_in_a_cell() {
    // U+2028 must not survive into a CSV cell as a real line break (regression guard
    // for the display_safe gap). The whole record stays one physical line.
    let mut a = Account::new().unwrap();
    a.id = "x".into();
    a.title = "a\u{2028}b".into();
    let out = accounts_csv(&[a]);
    assert_eq!(out.matches("\r\n").count(), 2, "header + exactly one record line");
    assert!(out.contains("a_b"), "U+2028 replaced with '_'");
}

#[test]
fn build_tab_csv_maps_each_tab_to_its_collection() {
    let mut v = Vault::default();
    let mut acc = Account::new().unwrap();
    acc.title = "Bank".into();
    v.accounts.push(acc);
    let mut tax = TaxFiling::new().unwrap();
    tax.owner = "Joint".into();
    v.tax_filings.push(tax);

    let (base, text, n) = build_tab_csv(&v, CsvTab::Accounts, echo);
    assert_eq!(base, "accounts");
    assert_eq!(n, 1);
    assert!(text.contains("Bank"));

    let (base, text, n) = build_tab_csv(&v, CsvTab::Taxes, echo);
    assert_eq!(base, "taxes");
    assert_eq!(n, 1);
    assert!(text.contains("Joint"));

    // A tab with no records yields a header-only CSV and count 0.
    let (base, _t, n) = build_tab_csv(&v, CsvTab::Instructions, echo);
    assert_eq!((base, n), ("instructions", 0));
}
