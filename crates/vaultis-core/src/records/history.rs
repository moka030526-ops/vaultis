//! Per-record change history: recording field changes (with secrets masked), and
//! compacting, measuring and trimming the history.

use super::*;

/// Trim every record's per-edit `history` log in `vault`. With `drop_all`, all
/// history entries are removed; otherwise entries strictly older than `cutoff`
/// (Unix seconds) are dropped and `at >= cutoff` are kept (inclusive keep). The
/// vault-level `audit` log is deliberately **left untouched**. Returns the count
/// of history entries removed. Removed `Change`s are `ZeroizeOnDrop`, so their
/// (possibly secret-bearing) before/after detail strings are wiped from RAM.
pub fn compact_history(vault: &mut Vault, cutoff: Option<i64>, drop_all: bool) -> usize {
    // Each record collection shares the generic `Record` interface, so one helper
    // trims them all.
    trim_histories(&mut vault.urgent, cutoff, drop_all)
        + trim_histories(&mut vault.instructions, cutoff, drop_all)
        + trim_histories(&mut vault.trust_wills, cutoff, drop_all)
        + trim_histories(&mut vault.assets, cutoff, drop_all)
        + trim_histories(&mut vault.accounts, cutoff, drop_all)
        + trim_histories(&mut vault.real_estate, cutoff, drop_all)
        + trim_histories(&mut vault.tax_filings, cutoff, drop_all)
        + trim_histories(&mut vault.general_documents, cutoff, drop_all)
        + trim_histories(&mut vault.zakat, cutoff, drop_all)
}

/// How many history entries `compact_history` would remove for the same
/// arguments — a non-mutating count for `--dry-run` and result reporting.
pub fn history_stats(vault: &Vault, cutoff: Option<i64>, drop_all: bool) -> usize {
    // Closure counting removable entries in one record's history.
    let count = |list: &[Change]| -> usize {
        if drop_all {
            list.len()
        } else if let Some(c) = cutoff {
            list.iter().filter(|ch| ch.at < c).count()
        } else {
            0
        }
    };
    let mut n = 0;
    for r in &vault.urgent {
        n += count(&r.history);
    }
    for r in &vault.instructions {
        n += count(&r.history);
    }
    for r in &vault.trust_wills {
        n += count(&r.history);
    }
    for r in &vault.assets {
        n += count(&r.history);
    }
    for r in &vault.accounts {
        n += count(&r.history);
    }
    for r in &vault.real_estate {
        n += count(&r.history);
    }
    for r in &vault.tax_filings {
        n += count(&r.history);
    }
    for r in &vault.general_documents {
        n += count(&r.history);
    }
    for r in &vault.zakat {
        n += count(&r.history);
    }
    n
}

/// Apply the history trim to one record collection; returns entries removed.
// Generic over any `Record` (uses its `history_mut` accessor). `&mut [R]` borrows
// the caller's Vec as a mutable slice. `retain` keeps only matching elements,
// dropping (and zeroizing) the rest in place.
pub(super) fn trim_histories<R: Record>(list: &mut [R], cutoff: Option<i64>, drop_all: bool) -> usize {
    let mut removed = 0;
    for rec in list.iter_mut() {
        let h = rec.history_mut();
        let before = h.len();
        if drop_all {
            h.clear();
        } else if let Some(c) = cutoff {
            h.retain(|ch| ch.at >= c);
        }
        removed += before - h.len();
    }
    removed
}

/// A single timestamped audit record. Pushed onto a record's history on every
/// edit, or onto the vault-level audit / volume upload log.
// `#[derive(...)]` auto-implements these traits for the struct below:
//   Serialize/Deserialize -> can be encoded to/from disk bytes,
//   Clone -> can be deep-copied, Debug -> printable for debugging,
//   Default -> has a zero/empty default value,
//   Zeroize/ZeroizeOnDrop -> wipes its memory (and does so automatically on drop).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Change {
    pub at: i64,        // unix-seconds timestamp of the change
    pub action: String, // e.g. "created", "updated", "deleted"
    pub detail: String, // human-readable description
}

// An `impl` block attaches methods/associated functions to a type (like adding
// methods to a class).
impl Change {
    // `&str` is a borrowed string slice (caller keeps ownership of its text);
    // `detail: String` is taken by value (an owned string moved in). `-> Self`
    // means it returns a `Change`.
    pub fn new(action: &str, detail: String) -> Self {
        // `action.to_string()` copies the borrowed text into a new owned String.
        Change { at: unix_now(), action: action.to_string(), detail }
    }
}

/// Append a field change to `out` if `old != new` (full before/after values).
// `out: &mut Vec<Change>` is an *exclusive borrow* of the caller's vector, so this
// function can push into the caller's list without copying or owning it. Plain
// `fn` (no `pub`) means this helper is private to the module.
pub(super) fn track(out: &mut Vec<Change>, at: i64, name: &str, old: &str, new: &str) {
    if old != new {
        out.push(Change {
            at,
            // `.into()` converts the "updated" `&str` literal into an owned
            // `String` (the field's type) via the trait-driven `Into` conversion.
            action: "updated".into(),
            // `{old:?}`/`{new:?}` use the Debug format (quotes the strings).
            detail: format!("{name}: {old:?} -> {new:?}"),
        });
    }
}

/// Append a boolean field change to `out` if it changed.
pub(super) fn track_bool(out: &mut Vec<Change>, at: i64, name: &str, old: bool, new: bool) {
    if old != new {
        out.push(Change { at, action: "updated".into(), detail: format!("{name}: {old} -> {new}") });
    }
}

/// True if a history `Change.detail` describes a secret (password) field change.
/// `detail` is formatted `"{field}: {old:?} -> {new:?}"`; the secret fields are
/// exactly those whose name ends in `password` (the account password and the
/// four RealEstate portal passwords). The UIs use this to mask secret values in
/// the history pane.
pub fn detail_is_secret(detail: &str) -> bool {
    // Only a real "field: old -> new" diff (which always contains a colon) can be a secret.
    // Require the colon so a colon-less history label that merely ENDS in "password" (e.g. an
    // Instruction titled "Reset my password") is not over-masked into "password: <hidden> -> …".
    match detail.split_once(':') {
        Some((name, _)) => name.trim_end().ends_with("password"),
        None => false,
    }
}

/// A history `Change.detail` formatted for display, with the before/after values
/// of a secret (password) field **masked**. The live edit field has its own reveal
/// toggle, but the history pane must never show a cleartext password (it can't be
/// copied from there and is a shoulder-surf/screen-share leak) — so the audit
/// trail keeps the field name ("the password changed") but hides the values.
/// Non-secret details pass through unchanged.
pub fn display_detail(detail: &str) -> String {
    if detail_is_secret(detail) {
        let name = detail.split_once(':').map(|(n, _)| n).unwrap_or("password");
        format!("{name}: <hidden> -> <hidden>")
    } else {
        detail.to_string()
    }
}
