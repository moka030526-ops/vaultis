//! The estate-vault data model: the five record types behind the UI tabs, the
//! encrypted-volume manifest, and the [`Vault`] that owns them all.
//!
//! Every record carries an `id`, `created_at`/`updated_at` timestamps, and an
//! append-only `history` of timestamped [`Change`]s (req: trace history). The
//! shared insert/edit/diff logic lives in the [`Record`] trait + the generic
//! [`upsert`]/[`remove`] helpers, so each type only describes its own fields and
//! field-level diff. All types wipe their contents on drop (they hold secrets
//! such as passwords).
//!
//! Rust orientation for non-Rust readers (concepts used throughout this file):
//! - `//!` starts a *module*-level doc comment (this whole block describes the
//!   file); `///` documents the item right below it; `//` is an ordinary comment.
//! - `&T` is a *shared (read-only) borrow* of a value, `&mut T` an *exclusive
//!   (read/write) borrow*. Passing `&x` lends access without giving up ownership;
//!   `clone()` makes an independent copy when a value would otherwise be moved.
//! - `Result<T, E>` is "either an `Ok(T)` or an `Err(E)`"; `Option<T>` is "either
//!   `Some(T)` or `None`". The `?` operator means "if this is an error/None,
//!   return it from the current function early; otherwise unwrap the value".
//! - `Vec<T>` is a growable array; `String` is an owned text buffer; `&str` is a
//!   borrowed view of text. `derive(...)` auto-generates trait implementations.

// `use` brings names into scope (like imports).
// serde = serialization framework; Deserialize/Serialize let these structs be
// converted to/from bytes (used for encrypting the vault to disk).
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
// zeroize = securely wipe memory. `Zeroize` exposes a wipe method; `ZeroizeOnDrop`
// makes a value wipe itself automatically when it goes out of scope (req: secrets
// must not linger in RAM).
use zeroize::{Zeroize, ZeroizeOnDrop};

// `crate::crypto` is the sibling `crypto` module of this same crate (binary).
// `self` here also imports the `crypto` module name itself, so both `crypto::...`
// and `CryptoError` are usable below.
use crate::crypto::{self, CryptoError};

// Helpers over the records, split by topic. Each submodule's items are glob re-exported
// here, so public items stay reachable at `records::<name>` exactly as before, while
// crate-internal helpers keep their narrower visibility.
mod dates; // calendar arithmetic on unix timestamps
mod doc_paths; // document storage locations + filename rules
mod grouping; // account facets, account/asset trees, asset <-> account links
mod history; // per-record change history
mod search; // substring + "sounds like" search
mod summary; // value buckets, owner summary, Zakat totals

pub use dates::*;
pub use doc_paths::*;
pub use grouping::*;
pub use history::*;
pub use search::*;
pub use summary::*;

/// Unix-seconds "now" (0 if the clock is before the epoch).
// `pub fn` = public function; `-> i64` = returns a 64-bit signed integer.
pub fn unix_now() -> i64 {
    SystemTime::now()
        // `duration_since` returns a `Result`: Ok(duration) if now >= epoch, else Err.
        .duration_since(UNIX_EPOCH)
        // `.map(|d| ...)` transforms the Ok value with a *closure* (an inline
        // anonymous function `|d| body`). `as i64` is a numeric cast.
        .map(|d| d.as_secs() as i64)
        // `.unwrap_or(0)` yields the inner value, or 0 if it was an Err.
        .unwrap_or(0)
}

/// Validate an Asset/Liability for the summary: it must have an owner and a NUMERIC
/// approximate value, so every entry lands in a row and contributes a real number. Returns
/// `Some(message)` describing the first problem, or `None` if valid.
pub fn asset_validation_error(a: &AssetLiability) -> Option<String> {
    if a.owner.trim().is_empty() {
        return Some("Owner is required.".to_string());
    }
    if parse_approx_value(&a.approx_value).is_none() {
        return Some("Approximate value must be a number (e.g. 1500, 12,000.50, or 250k).".to_string());
    }
    None
}

/// A random 128-bit hex id, used for records and volume blobs.
// Returns `Ok(String)` on success or an `Err(CryptoError)` if the RNG fails.
pub fn random_id() -> Result<String, CryptoError> {
    // `::<16>` is a const generic argument: ask for exactly 16 random bytes.
    // The trailing `?` propagates an error: if `random_bytes` returns Err, this
    // function returns that Err immediately; otherwise `bytes` is the 16 bytes.
    let bytes = crypto::random_bytes::<16>()?;
    // Iterate the bytes, format each as 2 lowercase hex digits, and `.collect()`
    // the resulting chars into one `String`. `Ok(...)` wraps it as the success case.
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Shared behaviour for the five record types so insert/edit/history is generic.
// A `trait` is like an interface: it lists methods a type must provide. `: Clone`
// is a *supertrait bound* — anything implementing `Record` must also be cloneable.
// These are method *signatures* only; each record type fills in the bodies later.
pub trait Record: Clone {
    // `&self` borrows the value read-only (a getter). `-> &str` returns a borrowed
    // view of the id, tied to the lifetime of `self` (no copy).
    fn id(&self) -> &str;
    fn created_at(&self) -> i64;
    /// Last-modified unix-seconds timestamp — the recency key the cross-vault merge
    /// compares (a source record is "more recent" iff its `updated_at` is greater).
    fn updated_at(&self) -> i64;
    // `&mut self` borrows exclusively so the method may mutate the value (a setter).
    fn set_created_at(&mut self, at: i64);
    fn set_updated_at(&mut self, at: i64);
    // Returns an exclusive borrow of the history vector so callers can push to it.
    fn history_mut(&mut self) -> &mut Vec<Change>;
    /// Field-level diff describing the change from `self` to `new`.
    // `Self` (capital S) means "the implementing type itself", so `new: &Self`
    // borrows another value of the same record type.
    fn diff(&self, new: &Self, at: i64) -> Vec<Change>;
    /// Short label for list display.
    fn label(&self) -> String;
    /// Left/right-trim every free-text field in place (including secrets such as
    /// passwords, per the project policy). Returns `true` if any field changed.
    /// Bookkeeping fields (id/timestamps/history), booleans, volume file ids, and
    /// record-link id lists (`linked_accounts`) are left untouched. Applied on
    /// every save and by the bulk [`trim_all_records`].
    fn trim_fields(&mut self) -> bool;
}

/// Left/right-trim each string in place, zeroizing the old buffer before replacing
/// it so a trimmed secret is never stranded in freed heap (a plain `*f = ...` would
/// deallocate the old `String` without wiping it). Returns whether anything changed.
/// Shared by every record's [`Record::trim_fields`].
fn trim_strings_in_place(fields: &mut [&mut String]) -> bool {
    let mut changed = false;
    for f in fields {
        // `trim()` only strips leading/trailing whitespace, so the value changed iff the
        // trimmed length differs — checked WITHOUT allocating. The previous code always
        // built `f.trim().to_string()` and, on the common already-trimmed path, dropped
        // that plain (non-zeroized) `String` copy of the secret, stranding a plaintext
        // password in freed heap (contradicting this fn's own contract). Allocate only on
        // a real change, and MOVE the new buffer into the field so no transient copy is
        // left unwiped (the live value is wiped later by the record's ZeroizeOnDrop).
        if f.trim().len() != f.len() {
            let new = f.trim().to_string();
            f.zeroize();
            **f = new;
            changed = true;
        }
    }
    changed
}

/// Insert `rec` or, if a record with the same id exists, replace it — appending
/// the field-level diff to history and preserving the original creation time.
// `<R: Record>` is a *generic* parameter: this one function works for any type `R`
// that implements the `Record` trait. `list: &mut Vec<R>` borrows the caller's
// vector exclusively; `mut rec: R` takes ownership of the record (moved in) and
// `mut` lets us modify it locally.
pub fn upsert<R: Record>(list: &mut Vec<R>, mut rec: R) {
    let now = unix_now();
    rec.set_updated_at(now);
    // `match` is pattern-matching (like a powerful switch). `.position(..)` finds
    // the index of the first element matching the closure `|e| ...`, returning
    // `Some(index)` or `None`.
    match list.iter().position(|e| e.id() == rec.id()) {
        // Existing record at index `i`: this is an edit.
        Some(i) => {
            // `&rec` lends the new record to `diff` (which only needs to read it).
            let changes = list[i].diff(&rec, now);
            rec.set_created_at(list[i].created_at()); // keep original creation time
            // MOVE the old history out (`std::mem::take` leaves an empty Vec in its
            // place) rather than cloning it: the clone duplicated every prior `Change`,
            // including cleartext old/new password values, and was O(n²) over a growing
            // history. Append the new diffs and install on the replacement record.
            let mut history = std::mem::take(list[i].history_mut());
            history.extend(changes); // old history + the new diffs
            *rec.history_mut() = history;
            list[i] = rec; // replace the slot (the old record is dropped & wiped)
        }
        // No match: this is a fresh insert.
        None => {
            let label = rec.label();
            rec.history_mut().push(Change::new("created", label));
            list.push(rec);
        }
    }
}

/// Remove a record by id, logging a timestamped deletion in `audit`.
// Generic over any `Record` type. Returns `bool`: true if something was removed.
pub fn remove<R: Record>(list: &mut Vec<R>, id: &str, audit: &mut Vec<Change>, kind: &str) -> bool {
    match list.iter().position(|e| e.id() == id) {
        Some(i) => {
            let label = list[i].label();
            list.remove(i);
            audit.push(Change::new("deleted", format!("{kind}: {label}")));
            true
        }
        None => false,
    }
}

// --- The record types --------------------------------------------------------
// Each struct below is one record kind. They share the same derives as `Change`
// (see that note): Serialize/Deserialize for disk, Clone/Debug/Default, and
// Zeroize/ZeroizeOnDrop so every field (including secrets) is wiped on drop.

/// Tab 0 — an URGENT free-text note. The first tab so the most time-critical
/// things an executor must know (whom to call, where the safe key is, an in-flight
/// crisis) are the first thing seen on unlock. Same shape as [`Instruction`]
/// (title + free-text body) — a separate, prominent collection, not a subtype.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Urgent {
    pub id: String,
    pub title: String,
    pub description: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>, // append-only audit trail for this record
}

/// Tab 1 — free-form instruction note.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Instruction {
    pub id: String,
    pub title: String,
    pub description: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>, // append-only audit trail for this record
}

/// Tab 2 — a trust/will document with a usage note and an attached file.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct TrustWill {
    pub id: String,
    pub document: String,
    pub usage: String,
    /// Volume file id of the attached document, if any.
    // `Option<String>` = either `Some(id)` (a file is attached) or `None` (no file).
    pub file: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 3 — an asset or liability.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct AssetLiability {
    pub id: String,
    /// "Asset" or "Liability".
    pub kind: String,
    pub description: String,
    pub owner: String,
    /// Short title/name for the entry (shown under Owner in the editor and used as the
    /// list label when set). Added after `owner`; `#[serde(default)]` keeps older vaults
    /// (which lack it) loadable — the field defaults to "".
    #[serde(default)]
    pub title: String,
    pub approx_value: String,
    pub as_of_date: String,
    pub institution: String,
    /// Category taken from the external asset-types list.
    pub asset_type: String,
    // `#[serde(default)]` on a field: if an older saved vault lacks this field,
    // deserialization fills it with the type's default ("" for String, false for
    // bool) instead of failing. This keeps newly-added fields backward-compatible.
    #[serde(default)]
    pub url: String,
    /// Beneficiary (chiefly for liabilities, but stored for any entry).
    #[serde(default)]
    pub beneficiary: String,
    /// Flagged for review.
    #[serde(default)]
    pub review: bool,
    /// Volume file id of the attached statement, if any.
    pub statement: Option<String>,
    /// Ids of the [`Account`] records this entry is linked to (e.g. the brokerage
    /// login plus the checking account that funds it). This is the vault's first
    /// record→record reference: account ids are stable for a record's whole life
    /// and the cross-vault merge copies records verbatim (id included), so a link
    /// survives save/reopen/merge. Deleting a linked account does NOT touch this
    /// list (additive/no-silent-loss policy) — the UIs warn at delete time and
    /// afterwards render the unresolvable id raw, like a missing `doc_path`.
    #[serde(default)]
    pub linked_accounts: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 4 — a login/account (the original password-manager record).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Account {
    pub id: String,
    /// Short human title/name for this account entry. Shown in the list (when set)
    /// and filterable, like type/subtype/owner.
    #[serde(default)]
    pub title: String,
    /// Category taken from the external account-types list.
    pub account_type: String,
    /// Subtype connected to the account type (e.g. type "Financial" -> "IRA").
    #[serde(default)]
    pub account_subtype: String,
    pub owner: String,
    pub username: String,
    pub password: String,
    pub description: String,
    pub url: String,
    /// Date the account was closed, as `YYYY-MM-DD`. Blank while the account is
    /// open; the UIs hint the format but store it as free text (like the other
    /// date fields), so legacy/partial values are never rejected.
    #[serde(default)]
    pub closed_as_of: String,
    /// Flagged for review.
    #[serde(default)]
    pub review: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 5 — a real-estate holding.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct RealEstate {
    pub id: String,
    pub address: String,
    /// Who owns the property. Renamed from `ownership`; `#[serde(alias = "ownership")]`
    /// keeps older vaults (whose JSON key is `ownership`) loadable with their value
    /// intact — they re-save under `owner`. Also feeds the owner-first document folder.
    #[serde(alias = "ownership")]
    pub owner: String,
    pub taxes: String,
    pub hoa: String,
    pub income_account: String,
    pub financing_account: String,
    pub payment_account: String,
    /// Outstanding financing/mortgage balance (free text).
    #[serde(default)]
    pub financing_balance: String,
    /// Property-management portal login.
    #[serde(default)]
    pub property_mgmt_url: String,
    #[serde(default)]
    pub property_mgmt_username: String,
    #[serde(default)]
    pub property_mgmt_password: String,
    /// Free-form notes for the property-management portal.
    #[serde(default)]
    pub property_mgmt_comment: String,
    /// Insurance portal login.
    #[serde(default)]
    pub insurance_url: String,
    #[serde(default)]
    pub insurance_username: String,
    #[serde(default)]
    pub insurance_password: String,
    /// Free-form notes for the insurance portal.
    #[serde(default)]
    pub insurance_comment: String,
    /// HOA portal login.
    #[serde(default)]
    pub hoa_url: String,
    #[serde(default)]
    pub hoa_username: String,
    #[serde(default)]
    pub hoa_password: String,
    /// Free-form notes for the HOA portal.
    #[serde(default)]
    pub hoa_comment: String,
    /// Tax portal login (property-tax authority / payment site).
    #[serde(default)]
    pub tax_portal_url: String,
    #[serde(default)]
    pub tax_portal_username: String,
    #[serde(default)]
    pub tax_portal_password: String,
    /// Free-form notes for the tax portal.
    #[serde(default)]
    pub tax_portal_comment: String,
    /// Free-form comments.
    #[serde(default)]
    pub comments: String,
    /// Volume file ids of documents attached to this property (deed, policy,
    /// statements), all stored under `real-estate/<address>/`.
    #[serde(default)]
    pub documents: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 6 — a tax filing for a given year, holding its uploaded documents.
/// Every document attached to a filing is stored together under the
/// `taxes/<year>/` virtual folder in the encrypted volume.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct TaxFiling {
    pub id: String,
    /// Who the filing is for (e.g. "Jane", "Joint"). `#[serde(default)]` keeps
    /// older vaults (which predate this field) loadable — it defaults to "".
    /// Shown together with the year in the list label ("<owner> - <year>").
    #[serde(default)]
    pub owner: String,
    /// The filing/tax year, e.g. "2024". Also names the document folder.
    pub year: String,
    pub notes: String,
    /// Volume file ids of the documents attached to this filing year (all stored
    /// under `taxes/<year>/`). An entry can hold several documents.
    pub documents: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 7 — a general document: a title, a free-form description, and a single
/// uploaded file. Its file is stored under `general-documents/<title>/<timestamp>/
/// [subfolder]/<filename>` in the encrypted volume.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct GeneralDocument {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Volume file id of the attached document, if any (single file per entry).
    pub file: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

/// Tab 8 — one Ramadan year's zakat obligation: the year, what was due, and what has
/// been paid against it. The fourth column the UI shows, **Remaining, is never stored**:
/// it is recomputed from the other two by [`ZakatEntry::remaining`], so the vault cannot
/// hold a remainder that silently disagrees with the numbers it came from.
///
/// The two amounts are free text (not `f64`) for the same reason `AssetLiability::
/// approx_value` is: the user's own notation — "12,500", "$12,500", "12.5k" — is
/// preserved verbatim, and [`parse_approx_value`] does the lenient parse when a number
/// is actually needed.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct ZakatEntry {
    pub id: String,
    /// The Ramadan (Hijri) year the obligation belongs to, e.g. "1446". Free text, so a
    /// user who tracks it as "1446 / 2025" can write exactly that.
    pub ramadan_year: String,
    /// Amount due for that year, as typed.
    pub amount_due: String,
    /// Amount paid so far against that year, as typed.
    pub amount_paid: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub history: Vec<Change>,
}

impl ZakatEntry {
    /// Remaining = due − paid.
    ///
    /// A **blank** amount counts as zero (a year with a due amount and nothing paid yet
    /// still has a meaningful remainder). A **non-blank but unparseable** amount yields
    /// `None` — the UI renders that as "—" rather than inventing a number, because
    /// quietly treating "ask Dad" as 0 would understate what is still owed.
    pub fn remaining(&self) -> Option<f64> {
        // `?` on each side propagates the `None` from an unparseable (non-blank) field.
        let v = zakat_amount(&self.amount_due)? - zakat_amount(&self.amount_paid)?;
        // ...but two individually-FINITE amounts can still overflow on subtraction:
        // `1.7e308 - -1.7e308` is +inf, and `parse_approx_value` accepts both sides. The
        // GUI happens to survive that because `fmt_money` guards non-finite values, but
        // the CSV export and the FFI format this number raw and so emitted the literal
        // text "inf" — a value the FFI's own DTO contract says is impossible (audit
        // 2026-09-16 F-3). Guarding HERE rather than at each boundary is what keeps the
        // table, the totals, the CSV and the phone from disagreeing: a remainder that is
        // not a real number is reported as "cannot be worked out", exactly like an
        // unreadable amount.
        v.is_finite().then_some(v)
    }
}

/// Stamp a freshly-built record with an id and creation/update timestamps.
// `macro_rules!` defines a compile-time code template (a macro), expanded inline
// wherever it's invoked — used here to avoid repeating identical constructor code
// for all five types. `$ty:ident` is a parameter that captures a type name.
// Note: the macro body uses `?`, so it only compiles inside a function that
// returns a `Result` (the `new()` methods below). The double `{{ }}` makes the
// expansion a block expression whose last value `r` is the result.
macro_rules! new_record {
    ($ty:ident) => {{
        let now = unix_now();
        // `mut r` so we can assign fields; `$ty::default()` builds an all-defaults
        // value of the named type (from the derived `Default`).
        let mut r = $ty::default();
        r.id = random_id()?; // `?` bubbles up an RNG error to the caller
        r.created_at = now;
        r.updated_at = now;
        r // last expression of the block = the value the macro produces
    }};
}

// One `impl` block per type providing a `new()` constructor. Each returns
// `Result<Self, CryptoError>` because id generation can fail; `Ok(...)` wraps the
// success value.
impl Urgent {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(Urgent))
    }
}
impl Instruction {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(Instruction))
    }
}
impl TrustWill {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(TrustWill))
    }
}
impl AssetLiability {
    pub fn new() -> Result<Self, CryptoError> {
        // This type defaults to an "Asset" (vs "Liability"), so it overrides the
        // field after the macro builds the base record.
        let mut r = new_record!(AssetLiability);
        r.kind = "Asset".to_string();
        Ok(r)
    }
}
impl Account {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(Account))
    }
}

/// One-off bulk maintenance: left/right-trim every field on every record in `list`,
/// routing each changed record through [`upsert`] so the trim is recorded in that
/// record's history (old -> new) and bumps `updated_at`. Returns how many changed.
/// Generic over any [`Record`] type via its [`Record::trim_fields`].
pub fn trim_all<R: Record>(list: &mut Vec<R>) -> usize {
    // Trim clones first (so we don't mutate while iterating), collect the ones that
    // actually changed, then upsert them back by id.
    let mut changed: Vec<R> = Vec::new();
    for r in list.iter() {
        let mut t = r.clone();
        if t.trim_fields() {
            changed.push(t);
        }
    }
    let n = changed.len();
    for t in changed {
        upsert(list, t);
    }
    n
}

/// Trim every account (the original bulk action, kept as a named convenience).
pub fn trim_all_accounts(accounts: &mut Vec<Account>) -> usize {
    trim_all(accounts)
}

/// Trim EVERY field on EVERY record across ALL tabs of the vault. Returns the total
/// number of records changed. Backs the "Trim all fields" maintenance action so the
/// whole vault — not just accounts — has its leading/trailing whitespace removed.
pub fn trim_all_records(vault: &mut Vault) -> usize {
    trim_all(&mut vault.urgent)
        + trim_all(&mut vault.instructions)
        + trim_all(&mut vault.trust_wills)
        + trim_all(&mut vault.assets)
        + trim_all(&mut vault.accounts)
        + trim_all(&mut vault.real_estate)
        + trim_all(&mut vault.tax_filings)
        + trim_all(&mut vault.general_documents)
        + trim_all(&mut vault.zakat)
}
impl RealEstate {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(RealEstate))
    }
}
impl TaxFiling {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(TaxFiling))
    }
}
impl GeneralDocument {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(GeneralDocument))
    }
}
impl ZakatEntry {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(new_record!(ZakatEntry))
    }
}

// --- Record trait impls (per-type fields + diff) -----------------------------

/// Generate the boilerplate `Record` impl. The id/timestamp/history accessors
/// are identical across types; the per-type `diff` and `label` are passed as
/// non-capturing closures (which coerce to `fn` pointers).
// `$ty:ty` captures a type, `$diff:expr`/`$label:expr` capture expressions (the
// two closures supplied at each call site below). The macro stamps out a full
// `impl Record for <type>` so we don't hand-write the same accessors five times.
macro_rules! impl_record {
    ($ty:ty, $diff:expr, $label:expr, $trim:expr) => {
        // `impl Record for $ty` = "this type provides the Record interface".
        impl Record for $ty {
            fn id(&self) -> &str {
                &self.id
            }
            fn created_at(&self) -> i64 {
                self.created_at
            }
            fn updated_at(&self) -> i64 {
                self.updated_at
            }
            fn set_created_at(&mut self, at: i64) {
                self.created_at = at;
            }
            fn set_updated_at(&mut self, at: i64) {
                self.updated_at = at;
            }
            fn history_mut(&mut self) -> &mut Vec<Change> {
                &mut self.history
            }
            fn diff(&self, new: &Self, at: i64) -> Vec<Change> {
                let mut out = Vec::new(); // empty, growable list to fill with diffs
                // Bind the supplied closure to a function-pointer-typed variable
                // (`fn(...)` is a plain function pointer). A closure that captures
                // nothing coerces to this. Then call it, passing `&mut out` so it
                // can append changes into our local vector.
                let f: fn(&$ty, &$ty, i64, &mut Vec<Change>) = $diff;
                f(self, new, at, &mut out);
                out // return the collected changes
            }
            fn label(&self) -> String {
                let f: fn(&$ty) -> String = $label;
                f(self)
            }
            fn trim_fields(&mut self) -> bool {
                let f: fn(&mut $ty) -> bool = $trim;
                f(self)
            }
        }
    };
}

// Each call below passes: the type, a diff closure, and a label closure.
// Diff closure args: `s` = self (old), `n` = new, `at` = timestamp, `out` = the
// vector to append changes to. `&s.title` lends the field to `track` (read-only).
impl_record!(
    Urgent,
    |s: &Urgent, n: &Urgent, at: i64, out: &mut Vec<Change>| {
        track(out, at, "title", &s.title, &n.title);
        track(out, at, "description", &s.description, &n.description);
    },
    |l: &Urgent| if l.title.is_empty() { "(urgent note)".to_string() } else { l.title.clone() },
    |r: &mut Urgent| trim_strings_in_place(&mut [&mut r.title, &mut r.description])
);

impl_record!(
    Instruction,
    |s: &Instruction, n: &Instruction, at: i64, out: &mut Vec<Change>| {
        track(out, at, "title", &s.title, &n.title);
        track(out, at, "description", &s.description, &n.description);
    },
    // Label closure: `l` is the record. `if/else` is an expression here (it yields
    // a value). Uses a literal placeholder when empty, else `.clone()`s the title
    // into a new owned String (the trait requires returning an owned `String`).
    |l: &Instruction| if l.title.is_empty() { "(untitled)".to_string() } else { l.title.clone() },
    |r: &mut Instruction| trim_strings_in_place(&mut [&mut r.title, &mut r.description])
);

impl_record!(
    TrustWill,
    |s: &TrustWill, n: &TrustWill, at: i64, out: &mut Vec<Change>| {
        track(out, at, "document", &s.document, &n.document);
        track(out, at, "usage", &s.usage, &n.usage);
        // `file` is an `Option`, not a string, so it's compared directly (rather
        // than via `track`) and logged without exposing the file id.
        if s.file != n.file {
            out.push(Change { at, action: "updated".into(), detail: "attached file changed".into() });
        }
    },
    |l: &TrustWill| if l.document.is_empty() { "(untitled)".to_string() } else { l.document.clone() },
    |r: &mut TrustWill| trim_strings_in_place(&mut [&mut r.document, &mut r.usage])
);

impl_record!(
    AssetLiability,
    |s: &AssetLiability, n: &AssetLiability, at: i64, out: &mut Vec<Change>| {
        track(out, at, "kind", &s.kind, &n.kind);
        track(out, at, "description", &s.description, &n.description);
        track(out, at, "owner", &s.owner, &n.owner);
        track(out, at, "title", &s.title, &n.title);
        track(out, at, "approx_value", &s.approx_value, &n.approx_value);
        track(out, at, "as_of_date", &s.as_of_date, &n.as_of_date);
        track(out, at, "institution", &s.institution, &n.institution);
        track(out, at, "type", &s.asset_type, &n.asset_type);
        track(out, at, "url", &s.url, &n.url);
        track(out, at, "beneficiary", &s.beneficiary, &n.beneficiary);
        track_bool(out, at, "review", s.review, n.review);
        if s.statement != n.statement {
            out.push(Change { at, action: "updated".into(), detail: "statement document changed".into() });
        }
        // Like `statement`, the link list is compared directly and logged without
        // exposing the raw account ids (they are meaningless in a history line).
        if s.linked_accounts != n.linked_accounts {
            out.push(Change { at, action: "updated".into(), detail: "linked accounts changed".into() });
        }
    },
    |l: &AssetLiability| {
        // Prefer the (new) title for the list label; fall back to the description, then a
        // placeholder. `.as_str()` borrows the String as a `&str` so every arm has the same
        // type (the literal is already a `&str`); no allocation happens here.
        // Gate on the TRIMMED value (matching `asset_tree`'s leaf label), so a
        // whitespace-only title doesn't show as a blank label in the flat list while the
        // grouped tree falls back to the description.
        let d = if !l.title.trim().is_empty() {
            l.title.as_str()
        } else if !l.description.trim().is_empty() {
            l.description.as_str()
        } else {
            "(no description)"
        };
        format!("[{}] {d}", l.kind)
    },
    |r: &mut AssetLiability| {
        trim_strings_in_place(&mut [
            &mut r.kind,
            &mut r.description,
            &mut r.owner,
            &mut r.title,
            &mut r.approx_value,
            &mut r.as_of_date,
            &mut r.institution,
            &mut r.asset_type,
            &mut r.url,
            &mut r.beneficiary,
        ])
    }
);

impl_record!(
    Account,
    |s: &Account, n: &Account, at: i64, out: &mut Vec<Change>| {
        track(out, at, "title", &s.title, &n.title);
        track(out, at, "type", &s.account_type, &n.account_type);
        track(out, at, "subtype", &s.account_subtype, &n.account_subtype);
        track(out, at, "owner", &s.owner, &n.owner);
        track(out, at, "username", &s.username, &n.username);
        // Full before/after of the password is recorded (accepted decision).
        track(out, at, "password", &s.password, &n.password);
        track(out, at, "description", &s.description, &n.description);
        track(out, at, "url", &s.url, &n.url);
        track(out, at, "closed_as_of", &s.closed_as_of, &n.closed_as_of);
        track_bool(out, at, "review", s.review, n.review);
    },
    |l: &Account| {
        // List display: "Title - Account Type - Username", joined by " - ", with the
        // title omitted when blank. The third part is the username, falling back to
        // the owner when there is no username. Empty parts are dropped (no dangling
        // separators); a wholly-empty record shows "(account)".
        let who = if l.username.trim().is_empty() { l.owner.trim() } else { l.username.trim() };
        let mut parts: Vec<&str> = Vec::new();
        if !l.title.trim().is_empty() {
            parts.push(l.title.trim());
        }
        if !l.account_type.trim().is_empty() {
            parts.push(l.account_type.trim());
        }
        if !who.is_empty() {
            parts.push(who);
        }
        if parts.is_empty() { "(account)".to_string() } else { parts.join(" - ") }
    },
    |r: &mut Account| {
        // Every text field, including the password (accepted policy). `review`,
        // id, timestamps, and history are deliberately excluded.
        trim_strings_in_place(&mut [
            &mut r.title,
            &mut r.account_type,
            &mut r.account_subtype,
            &mut r.owner,
            &mut r.username,
            &mut r.password,
            &mut r.url,
            &mut r.closed_as_of,
            &mut r.description,
        ])
    }
);

impl_record!(
    RealEstate,
    |s: &RealEstate, n: &RealEstate, at: i64, out: &mut Vec<Change>| {
        track(out, at, "address", &s.address, &n.address);
        track(out, at, "owner", &s.owner, &n.owner);
        track(out, at, "taxes", &s.taxes, &n.taxes);
        track(out, at, "hoa", &s.hoa, &n.hoa);
        track(out, at, "income_account", &s.income_account, &n.income_account);
        track(out, at, "financing_account", &s.financing_account, &n.financing_account);
        track(out, at, "financing_balance", &s.financing_balance, &n.financing_balance);
        track(out, at, "payment_account", &s.payment_account, &n.payment_account);
        track(out, at, "property_mgmt_url", &s.property_mgmt_url, &n.property_mgmt_url);
        track(out, at, "property_mgmt_username", &s.property_mgmt_username, &n.property_mgmt_username);
        track(out, at, "property_mgmt_password", &s.property_mgmt_password, &n.property_mgmt_password);
        track(out, at, "property_mgmt_comment", &s.property_mgmt_comment, &n.property_mgmt_comment);
        track(out, at, "insurance_url", &s.insurance_url, &n.insurance_url);
        track(out, at, "insurance_username", &s.insurance_username, &n.insurance_username);
        track(out, at, "insurance_password", &s.insurance_password, &n.insurance_password);
        track(out, at, "insurance_comment", &s.insurance_comment, &n.insurance_comment);
        track(out, at, "hoa_url", &s.hoa_url, &n.hoa_url);
        track(out, at, "hoa_username", &s.hoa_username, &n.hoa_username);
        track(out, at, "hoa_password", &s.hoa_password, &n.hoa_password);
        track(out, at, "hoa_comment", &s.hoa_comment, &n.hoa_comment);
        track(out, at, "tax_portal_url", &s.tax_portal_url, &n.tax_portal_url);
        track(out, at, "tax_portal_username", &s.tax_portal_username, &n.tax_portal_username);
        track(out, at, "tax_portal_password", &s.tax_portal_password, &n.tax_portal_password);
        track(out, at, "tax_portal_comment", &s.tax_portal_comment, &n.tax_portal_comment);
        track(out, at, "comments", &s.comments, &n.comments);
        if s.documents != n.documents {
            out.push(Change {
                at,
                action: "updated".into(),
                detail: format!("documents: {} -> {}", s.documents.len(), n.documents.len()),
            });
        }
    },
    |l: &RealEstate| if l.address.is_empty() { "(no address)".to_string() } else { l.address.clone() },
    |r: &mut RealEstate| {
        // Every text field, including the four portal passwords. `documents` (volume
        // ids), id, timestamps, and history are excluded.
        trim_strings_in_place(&mut [
            &mut r.address,
            &mut r.owner,
            &mut r.taxes,
            &mut r.hoa,
            &mut r.income_account,
            &mut r.financing_account,
            &mut r.payment_account,
            &mut r.financing_balance,
            &mut r.property_mgmt_url,
            &mut r.property_mgmt_username,
            &mut r.property_mgmt_password,
            &mut r.property_mgmt_comment,
            &mut r.insurance_url,
            &mut r.insurance_username,
            &mut r.insurance_password,
            &mut r.insurance_comment,
            &mut r.hoa_url,
            &mut r.hoa_username,
            &mut r.hoa_password,
            &mut r.hoa_comment,
            &mut r.tax_portal_url,
            &mut r.tax_portal_username,
            &mut r.tax_portal_password,
            &mut r.tax_portal_comment,
            &mut r.comments,
        ])
    }
);

impl_record!(
    TaxFiling,
    |s: &TaxFiling, n: &TaxFiling, at: i64, out: &mut Vec<Change>| {
        track(out, at, "owner", &s.owner, &n.owner);
        track(out, at, "year", &s.year, &n.year);
        track(out, at, "notes", &s.notes, &n.notes);
        // Log document-count changes without exposing the volume file ids.
        if s.documents != n.documents {
            out.push(Change {
                at,
                action: "updated".into(),
                detail: format!("documents: {} -> {}", s.documents.len(), n.documents.len()),
            });
        }
    },
    // List label: "<owner> - <year>" when both are set. Falls back to just the
    // owner, the legacy "Taxes <year>" (owner-less vaults), or "(no year)".
    |l: &TaxFiling| {
        let owner = l.owner.trim();
        let year = l.year.trim();
        match (owner.is_empty(), year.is_empty()) {
            (false, false) => format!("{owner} - {year}"),
            (false, true) => owner.to_string(),
            (true, false) => format!("Taxes {year}"),
            (true, true) => "(no year)".to_string(),
        }
    },
    |r: &mut TaxFiling| trim_strings_in_place(&mut [&mut r.owner, &mut r.year, &mut r.notes])
);

impl_record!(
    GeneralDocument,
    |s: &GeneralDocument, n: &GeneralDocument, at: i64, out: &mut Vec<Change>| {
        track(out, at, "title", &s.title, &n.title);
        track(out, at, "description", &s.description, &n.description);
        // `file` is an Option holding a volume id; log changes without exposing it.
        if s.file != n.file {
            out.push(Change { at, action: "updated".into(), detail: "attached file changed".into() });
        }
    },
    |l: &GeneralDocument| if l.title.is_empty() { "(untitled)".to_string() } else { l.title.clone() },
    |r: &mut GeneralDocument| trim_strings_in_place(&mut [&mut r.title, &mut r.description])
);

impl_record!(
    ZakatEntry,
    |s: &ZakatEntry, n: &ZakatEntry, at: i64, out: &mut Vec<Change>| {
        track(out, at, "ramadan_year", &s.ramadan_year, &n.ramadan_year);
        track(out, at, "amount_due", &s.amount_due, &n.amount_due);
        track(out, at, "amount_paid", &s.amount_paid, &n.amount_paid);
        // `remaining` is derived, not stored, so there is nothing of its own to track —
        // any change to it is already implied by the two amounts logged above.
    },
    |l: &ZakatEntry| {
        let year = l.ramadan_year.trim();
        if year.is_empty() { "(no year)".to_string() } else { format!("Ramadan {year}") }
    },
    |r: &mut ZakatEntry| {
        trim_strings_in_place(&mut [&mut r.ramadan_year, &mut r.amount_due, &mut r.amount_paid])
    }
);

// --- Vault settings ----------------------------------------------------------

/// User-configurable vault settings, stored (encrypted) inside the vault.
// Note: no `Default` in the derive list — a custom one is written by hand below
// because the default cap isn't the numeric zero.
#[derive(Serialize, Deserialize, Clone, Debug, Zeroize, ZeroizeOnDrop)]
pub struct VaultSettings {
    /// Per-partition document-volume size cap (bytes). A new document that would
    /// push the active partition past this rolls into a fresh partition.
    pub volume_max_size: u64, // u64 = unsigned 64-bit integer
    /// Opt-in in-place redundancy for `vault.pmv` (see `docs/DESIGN.md` §12.8).
    /// `0` (the default) = off: just the single `vault.pmv`. `N >= 1` = also write a
    /// same-generation mirror (`vault.pmv.mirror`) AND retain the last `N` prior
    /// generations (`vault.pmv.bak1`..`bakN`), so a bit-rotted vault file can be
    /// recovered in place. This is a complement to off-device backups, NOT a
    /// replacement, and it leaves more encrypted copies of old secrets on disk.
    /// `#[serde(default)]` keeps vaults written before this field existed loadable
    /// (they decode as `0`).
    #[serde(default)]
    pub redundancy: u32,
}

// Hand-written `Default` implementation (the `Default` trait's one method).
// Returning `Self` here means a `VaultSettings` whose cap is the project-wide
// constant rather than 0.
impl Default for VaultSettings {
    fn default() -> Self {
        VaultSettings { volume_max_size: crate::storage::DEFAULT_VOLUME_MAX_SIZE, redundancy: 0 }
    }
}

/// The decrypted contents of a vault: all six record collections plus the
/// volume manifest, access time, and vault-level audit log. Wipes on drop.
// This is the top-level in-memory object; `ZeroizeOnDrop` means the entire vault
// (and every record inside it) is securely erased when it leaves scope.
// `#[serde(default)]` on each field keeps older saved vaults loadable when new
// fields are added (missing fields take their type default).
#[derive(Serialize, Deserialize, Clone, Debug, Default, Zeroize, ZeroizeOnDrop)]
pub struct Vault {
    #[serde(default)]
    pub version: u8, // u8 = unsigned 8-bit integer (0..=255)
    /// Monotonically increasing write counter, bumped on every successful save.
    /// Surfaced on unlock so a user can notice a whole-file rollback to an older
    /// snapshot (see `docs/DESIGN.md` §9.12).
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub last_opened_at: i64,
    /// URGENT notes (Tab 0). `#[serde(default)]` keeps vaults written before this tab
    /// existed loadable — a missing key decodes to an empty list.
    #[serde(default)]
    pub urgent: Vec<Urgent>,
    #[serde(default)]
    pub instructions: Vec<Instruction>,
    #[serde(default)]
    pub trust_wills: Vec<TrustWill>,
    #[serde(default)]
    pub assets: Vec<AssetLiability>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub real_estate: Vec<RealEstate>,
    /// Tax filings (the Taxes tab); each year's documents live under `taxes/<year>/`.
    #[serde(default)]
    pub tax_filings: Vec<TaxFiling>,
    /// General documents (the General Documents tab); each entry's single file lives
    /// under `general-documents/<title>/<timestamp>/[subfolder]/`.
    #[serde(default)]
    pub general_documents: Vec<GeneralDocument>,
    /// Zakat obligations by Ramadan year (the Zakat tab). No documents — the tab is a
    /// four-column ledger. `#[serde(default)]` keeps vaults written before this tab
    /// existed loadable: a missing key decodes to an empty list.
    #[serde(default)]
    pub zakat: Vec<ZakatEntry>,
    /// Stable random id binding the document volumes/manifests to this vault (so a
    /// foreign or swapped volume/manifest fails authentication). Set on create.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub settings: VaultSettings,
    #[serde(default)]
    pub audit: Vec<Change>,
    /// Tombstones: blob ids explicitly removed via `remove_document`. A lazy delete
    /// only drops the manifest entry, leaving the encrypted frame as garbage until the
    /// next volume rewrite — so a manifest-loss rebuild (which re-scans the volume)
    /// would otherwise RESURRECT a deleted document, and a later compact would bake it
    /// in permanently (audit R-2). Recording the id here (authenticated inside
    /// vault.pmv) lets the doc readers suppress a resurrected frame and lets a volume
    /// rewrite drop it for good. Cleared by `staged_rewrite` once the volume has been
    /// fully re-encrypted (the tombstoned frames then no longer exist on disk). These
    /// are non-secret random hex ids.
    #[serde(default)]
    pub deleted_docs: Vec<String>,
    /// The editable category lists for the dropdowns, stored in the vault itself
    /// (not in external files). A vault that predates this field falls back to
    /// the built-in defaults. Category names are not secrets, so they are skipped
    /// by the zeroize-on-drop wipe.
    // `#[serde(default = "path::to::fn")]` names a function to call for the default
    // when the field is missing (here, the built-in category lists) — used instead
    // of the plain `#[serde(default)]` because the desired default isn't "empty".
    #[serde(default = "crate::types::TypeLists::with_defaults")]
    // `#[zeroize(skip)]` excludes this one field from the secret-wiping on drop
    // (category names aren't sensitive, and `TypeLists` may not be zeroize-able).
    #[zeroize(skip)]
    pub categories: crate::types::TypeLists,
}

// `#[cfg(test)]` is *conditional compilation*: this whole module is compiled only
// when running tests, never in the shipped binary. `use super::*` pulls in
// everything from the parent module (this file). Each `#[test]` fn is run by the
// test harness; `assert!`/`assert_eq!` panic (fail the test) if their condition
// is false. `.unwrap()` extracts the value from a Result/Option and panics if it's
// Err/None — acceptable in tests, where a panic simply marks the test failed.
#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
