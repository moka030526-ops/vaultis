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

/// Case-insensitive substring match used by the UIs' free-text search (e.g.
/// searching accounts by username). An empty/whitespace-only `query` matches
/// everything (no filter). Both sides are lower-cased and the query is trimmed.
// `haystack`/`query` are borrowed `&str`; the function only reads them.
pub fn matches_search(haystack: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || haystack.to_lowercase().contains(&q)
}

/// Consonant class of an ASCII letter (lower-cased): `Some(digit)` for a coded consonant,
/// `None` for a vowel-like letter (a/e/i/o/u/y — and h/w, which [`soundex`] treats specially).
/// Shared by [`soundex`] and [`soundex_key`] so both agree on what "sounds the same".
fn soundex_class(c: char) -> Option<u8> {
    match c {
        'b' | 'f' | 'p' | 'v' => Some(b'1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some(b'2'),
        'd' | 't' => Some(b'3'),
        'l' => Some(b'4'),
        'm' | 'n' => Some(b'5'),
        'r' => Some(b'6'),
        _ => None,
    }
}

/// The American **Soundex** code of one word (`"Robert" -> "R163"`), or `None` when the word
/// holds no ASCII letter to seed a code (digits, punctuation, or non-ASCII script).
///
/// Soundex maps a word to its first letter plus three consonant-class digits, so spellings that
/// SOUND alike collapse to the same code (`Smith`/`Smyth`, `Nguyen`/`Nguyan`). It is deliberately
/// ASCII-only and deliberately crude: it is used to WIDEN a search, never to narrow one, so a
/// word it cannot code simply falls back to the substring rule in [`matches_search_soundlike`].
///
/// Non-letters are skipped; h/w are transparent (they don't break a repeat) and vowels reset the
/// "previous class", so `Tymczak`-style repeats across a vowel still yield two digits. The result
/// is always exactly 4 chars (zero-padded). This is the TEXTBOOK code, which keeps the first
/// letter verbatim; the search matcher compares [`soundex_key`]s so `Katherine`/`Catherine` meet.
pub fn soundex(word: &str) -> Option<String> {
    let mut letters = word.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase());
    let first = letters.next()?;
    let mut code = String::with_capacity(4);
    code.push(first.to_ascii_uppercase());
    // `prev` is the class of the last letter that was NOT h/w — that is what a repeat is
    // measured against, which is why h/w are skipped without touching it.
    let mut prev = soundex_class(first);
    for c in letters {
        if c == 'h' || c == 'w' {
            continue; // transparent: "Ashcraft" codes A261, not A226
        }
        let cur = soundex_class(c);
        if let Some(d) = cur
            && cur != prev
        {
            code.push(d as char);
        }
        prev = cur; // a vowel sets `prev` to None, so a repeat after it IS coded
        if code.len() == 4 {
            break;
        }
    }
    while code.len() < 4 {
        code.push('0');
    }
    Some(code)
}

/// The comparison key used by [`matches_search_soundlike`]: the word's [`soundex`] code with its
/// INITIAL letter also folded to its consonant class (`Katherine -> "2365"`, `Catherine -> "2365"`).
///
/// Textbook Soundex keeps the first letter as-is, which is exactly where sound-alike spellings
/// differ most often for names people actually search — Katherine/Catherine, Chris/Kris,
/// Fisher/Visher. Folding it makes those meet. A vowel/h/w/y initial has no class, so it is kept
/// verbatim (Allen and Ellen genuinely start differently).
pub fn soundex_key(word: &str) -> Option<String> {
    let code = soundex(word)?;
    let mut chars = code.chars();
    let first = chars.next()?; // `soundex` always yields 4 chars, so this is infallible
    let folded = match soundex_class(first.to_ascii_lowercase()) {
        Some(d) => d as char,
        None => first,
    };
    Some(std::iter::once(folded).chain(chars).collect())
}

/// The fewest ASCII letters a word needs before its Soundex code is trusted as a "sounds like"
/// signal. Below this, a code is nearly content-free — a 1-letter word codes to `<letter>000`,
/// so `u2` and `u1` (two DIFFERENT logins) would collide, and a search for one would silently
/// pull in the other. Short words still match as substrings, which is what a 1–2 character
/// query means anyway ("show me things containing this").
const SOUNDEX_MIN_LETTERS: usize = 3;

/// [`soundex_key`], but only for a word long enough for the code to mean something (see
/// [`SOUNDEX_MIN_LETTERS`]); `None` otherwise, so the caller falls back to substring matching.
fn phonetic_key(word: &str) -> Option<String> {
    if word.chars().filter(|c| c.is_ascii_alphabetic()).count() < SOUNDEX_MIN_LETTERS {
        return None;
    }
    soundex_key(word)
}

/// The free-text search behind the UIs' **search box**: [`matches_search`]'s case-insensitive
/// substring rule, WIDENED with a sound-alike (Soundex) match so a name typed the way it sounds
/// still finds the record (`"jonson"` finds *Johnson*, `"catherine"` finds *Katherine*).
///
/// An empty/whitespace-only query matches everything. Otherwise EVERY whitespace-separated word
/// of the query must be satisfied by the haystack — each either as a plain substring (exactly as
/// before) or by an equal [`soundex_key`] against some word of the haystack. Requiring every word
/// keeps a multi-word query narrowing rather than widening, and the substring arm makes this a
/// strict SUPERSET of the old behaviour: a search that used to hit still hits.
///
/// A query word with fewer than [`SOUNDEX_MIN_LETTERS`] letters (`"u2"`, `"2024"`, `"@"`) has no
/// trusted code, so it is matched by substring alone — digits never sound like anything, and a
/// 1–2 letter code would collide with half the vault. Soundex is a coarse ASCII heuristic, so
/// even coded words collide (`"bob"`/`"bab"`); that is the intended trade for finding a name the
/// user cannot spell, and the exact filter dropdowns remain available to narrow the list again.
pub fn matches_search_soundlike(haystack: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let hay_lower = haystack.to_lowercase();
    // Code every haystack word once, not once per query word. The haystack splits on any
    // non-alphanumeric character, not just whitespace, so an email/handle like
    // `alice.smith@example.com` contributes the words a human hears — alice, smith, example,
    // com — instead of one unpronounceable run.
    let hay_codes: Vec<String> = haystack.split(|c: char| !c.is_alphanumeric()).filter_map(phonetic_key).collect();
    q.split_whitespace().all(|word| {
        hay_lower.contains(&word.to_lowercase())
            || phonetic_key(word).is_some_and(|qc| hay_codes.contains(&qc))
    })
}

/// The cross-filtered (faceted) options for the Accounts filters. For each field,
/// the distinct values present among accounts matching **every other** active
/// selection — so each dropdown only offers values that would actually yield
/// results given the rest of the filters.
pub struct AccountFacets {
    pub types: Vec<String>,
    pub subtypes: Vec<String>,
    pub owners: Vec<String>,
    pub titles: Vec<String>,
}

/// Does `a` match the given selections? An empty string for a field means "no
/// filter on that field"; `query` is the free-text username search — substring OR
/// sound-alike ([`matches_search_soundlike`]), the same rule the UIs' search box applies, so
/// the facet dropdowns never offer fewer values than the list actually shows;
/// `review_only` keeps only review-flagged accounts when true.
fn acct_match(a: &Account, t: &str, st: &str, o: &str, ti: &str, query: &str, review_only: bool) -> bool {
    (t.is_empty() || a.account_type == t)
        && (st.is_empty() || a.account_subtype == st)
        && (o.is_empty() || a.owner == o)
        && (ti.is_empty() || a.title == ti)
        && (!review_only || a.review)
        && matches_search_soundlike(&a.username, query)
}

/// Distinct, sorted, non-empty values of `field` over the accounts that pass
/// `keep` — the building block for one facet.
fn facet<F: Fn(&Account) -> &str, K: Fn(&Account) -> bool>(accounts: &[Account], field: F, keep: K) -> Vec<String> {
    let mut v: Vec<String> = accounts
        .iter()
        .filter(|a| keep(a))
        .map(|a| field(a).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Compute the faceted Accounts filter options: each field's distinct values among
/// accounts matching all the OTHER current selections (its own selection is ignored
/// when building its own list, so the user can still switch to another compatible
/// value). Empty selection strings mean "unset". The username `query` participates
/// as a constraint on every facet.
pub fn account_facets(
    accounts: &[Account],
    t: &str,
    st: &str,
    o: &str,
    ti: &str,
    query: &str,
    review_only: bool,
) -> AccountFacets {
    AccountFacets {
        types: facet(accounts, |a| &a.account_type, |a| acct_match(a, "", st, o, ti, query, review_only)),
        subtypes: facet(accounts, |a| &a.account_subtype, |a| acct_match(a, t, "", o, ti, query, review_only)),
        owners: facet(accounts, |a| &a.owner, |a| acct_match(a, t, st, "", ti, query, review_only)),
        titles: facet(accounts, |a| &a.title, |a| acct_match(a, t, st, o, "", query, review_only)),
    }
}

// --- Grouped (tree) view of accounts ----------------------------------------

/// A leaf of the account tree: one account, shown by its title only (the owner /
/// type / subtype are implied by its position in the tree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLeaf {
    pub id: String,
    pub title: String,
}

/// A node of the grouped account tree: one grouping value (`label`, never empty)
/// with its child groups and the accounts that end at this node. The grouping order
/// is owner → type → subtype; an EMPTY grouping value is SKIPPED (its accounts are
/// promoted to the parent level), so there are no "(none)" placeholder nodes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AcctNode {
    pub label: String,
    pub children: Vec<AcctNode>,
    pub leaves: Vec<AccountLeaf>,
}

impl AcctNode {
    /// Find or create the child group named `label`, preserving insertion order
    /// (the final sort reorders). Linear scan — group counts are modest.
    fn child_mut(&mut self, label: &str) -> &mut AcctNode {
        match self.children.iter().position(|c| c.label == label) {
            Some(i) => &mut self.children[i],
            None => {
                self.children.push(AcctNode { label: label.to_string(), ..Default::default() });
                self.children.last_mut().unwrap()
            }
        }
    }
    /// Sort children by label and leaves by title, case-insensitively, recursively.
    fn sort_recursive(&mut self) {
        self.children.sort_by_key(|c| c.label.to_lowercase());
        self.leaves.sort_by_key(|l| l.title.to_lowercase());
        for c in &mut self.children {
            c.sort_recursive();
        }
    }
}

/// Build the **grouped tree** of `accounts` for the GUI/TUI "grouped" view: each
/// account is placed along the path of its NON-EMPTY grouping values in the order
/// **owner → type → subtype**, then added as a leaf (title only) at the end of that
/// path. An empty owner/type/subtype is **skipped** — there are no "(none)" nodes —
/// so an account with no owner appears at the top level, an account with no type
/// appears directly under its owner, and so on. The returned ROOT node's `label` is
/// unused: render its `children` (top-level groups) and `leaves` (accounts that have
/// no grouping at all). Every level is sorted case-insensitively. Takes any iterator
/// of account references so a caller can pass the FILTERED accounts (no clone).
pub fn account_tree<'a>(accounts: impl IntoIterator<Item = &'a Account>) -> AcctNode {
    let mut root = AcctNode::default();
    for a in accounts {
        // Descend (creating as needed) along the non-empty grouping values.
        let mut node = &mut root;
        for level in [&a.owner, &a.account_type, &a.account_subtype] {
            // Group by the TRIMMED value and skip whitespace-only levels: a stray " "
            // (e.g. legacy/imported data not yet re-saved) must not create a blank node,
            // nor split " " and "  " into separate groups (`child_mut` matches exactly).
            let level = level.trim();
            if !level.is_empty() {
                node = node.child_mut(level);
            }
        }
        node.leaves.push(AccountLeaf { id: a.id.clone(), title: a.title.clone() });
    }
    root.sort_recursive();
    root
}

/// Build the **grouped tree** of `assets` for the "grouped" Assets view: each asset is
/// placed along the path of its NON-EMPTY grouping values in the order **owner → kind
/// (Asset/Liability) → type**, then added as a leaf at the end of that path. Empty grouping
/// values are skipped (no "(none)" nodes), every level is sorted case-insensitively, and the
/// leaf shows the entry's title (or description, then a placeholder) — the `[kind]` prefix is
/// omitted since the kind is already a grouping level. Reuses [`AcctNode`]/[`AccountLeaf`]
/// (the leaf `title` field carries the display label). Takes any iterator of asset references
/// so a caller can pass the FILTERED assets without cloning.
pub fn asset_tree<'a>(assets: impl IntoIterator<Item = &'a AssetLiability>) -> AcctNode {
    let mut root = AcctNode::default();
    for a in assets {
        let mut node = &mut root;
        for level in [&a.owner, &a.kind, &a.asset_type] {
            let level = level.trim();
            if !level.is_empty() {
                node = node.child_mut(level);
            }
        }
        // Display label without the `[kind]` prefix (kind is a grouping level here).
        let label = if !a.title.trim().is_empty() {
            a.title.clone()
        } else if !a.description.trim().is_empty() {
            a.description.clone()
        } else {
            "(no description)".to_string()
        };
        node.leaves.push(AccountLeaf { id: a.id.clone(), title: label });
    }
    root.sort_recursive();
    root
}

// --- Asset ↔ account links ----------------------------------------------------
//
// `AssetLiability::linked_accounts` holds Account record ids. These two helpers are
// the shared resolve/reverse-lookup used by both front-ends (GUI + TUI) and the CSV
// export, so the display convention lives in ONE place.

/// Resolve an account id to its display label ([`Record::label`]: "Title - Type -
/// Username"). `None` when no account with that id exists — a dangling link (the
/// account was deleted, or the link arrived via merge from a vault whose account
/// isn't here). Callers then show the raw id, mirroring the tolerant `doc_path`
/// fallback, so the link is visible-but-flagged rather than silently dropped.
pub fn account_label(accounts: &[Account], id: &str) -> Option<String> {
    accounts.iter().find(|a| a.id == id).map(|a| a.label())
}

/// Reverse lookup: `(id, label)` of every asset/liability whose `linked_accounts`
/// contains `account_id`, in list order. Computed on demand by scanning the assets
/// (same approach as the category-usage counts — no stored back-pointers to drift).
/// Feeds the read-only "Linked from" view on an account and the delete-time
/// warning: deleting a still-linked account is allowed but surfaced first.
pub fn assets_linking_account(assets: &[AssetLiability], account_id: &str) -> Vec<(String, String)> {
    assets
        .iter()
        .filter(|a| a.linked_accounts.iter().any(|id| id == account_id))
        .map(|a| (a.id.clone(), a.label()))
        .collect()
}

// --- Value summary (owner × asset/liability buckets) -------------------------
//
// The "Summary" tab aggregates every Asset/Liability's `approx_value` into a small matrix:
// one ROW per owner, columns split by kind (Asset vs Liability). ASSETS get four buckets —
// Real Estate, Cash (cash/savings/checking), Before Tax (retirement + HSA), After Tax
// (everything else); the asset bucket is inferred by keyword from the entry's Type + Institution.
// LIABILITIES are NOT tax-split (there is no meaningful "before tax" liability) — every liability
// aggregates into one Liability column.

/// Which summary column an Asset/Liability falls into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueBucket {
    RealEstate,
    Cash,
    BeforeTax,
    AfterTax,
}

/// Keyword-classify an entry by its `asset_type` + `institution`. `is_liability` suppresses
/// the Real-Estate bucket (the summary doesn't tax-split liabilities anyway).
///
/// REAL ESTATE = property / real estate. BEFORE TAX = retirement accounts
/// (401k/403b/457/IRA/Roth/pension/annuity/TSP) AND pre-tax HEALTH accounts (HSA / "Health
/// Equity"). CASH = cash / savings / checking. Everything else is AFTER TAX. Precedence:
/// real estate, then before-tax (so a "Roth savings" counts as retirement, not cash), then
/// cash, then after-tax. The keyword lists are intentionally simple — extend them here if a
/// holding isn't bucketed the way you expect.
pub fn value_bucket(asset_type: &str, institution: &str, is_liability: bool) -> ValueBucket {
    let hay = format!("{asset_type} {institution}").to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| hay.contains(n));
    const BEFORE_TAX: &[&str] = &[
        "retire", "401", "403", "457", "ira", "roth", "pension", "annuity", "tsp", "hsa",
        "health equity", "healthequity", "health savings",
    ];
    const REAL_ESTATE: &[&str] = &["real estate", "real-estate", "realestate", "property", "rental"];
    const CASH: &[&str] = &["cash", "saving", "checking", "chequing", "money market"];
    if !is_liability && has(REAL_ESTATE) {
        ValueBucket::RealEstate
    } else if has(BEFORE_TAX) {
        ValueBucket::BeforeTax
    } else if has(CASH) {
        ValueBucket::Cash
    } else {
        ValueBucket::AfterTax
    }
}

/// Parse a free-text `approx_value` into a number for aggregation and validation. Accepts an
/// optional leading currency symbol, thousands separators (commas/spaces/underscores), a
/// decimal point, a leading sign, surrounding whitespace, and an optional magnitude suffix
/// k/m/b/t (case-insensitive: `1.2m` = 1_200_000). Returns `None` if the remainder is not a
/// finite number.
pub fn parse_approx_value(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    // A trailing ASCII k/m/b/t is a magnitude suffix; the slice boundary is safe because the
    // matched byte is ASCII (1 byte), so `len - 1` lands on a char boundary.
    let (digits, mult): (&str, f64) = match lower.as_bytes().last() {
        Some(b'k') => (&lower[..lower.len() - 1], 1e3),
        Some(b'm') => (&lower[..lower.len() - 1], 1e6),
        Some(b'b') => (&lower[..lower.len() - 1], 1e9),
        Some(b't') => (&lower[..lower.len() - 1], 1e12),
        _ => (lower.as_str(), 1.0),
    };
    let cleaned: String =
        digits.chars().filter(|c| !matches!(c, '$' | '€' | '£' | '¥' | ',' | ' ' | '_')).collect();
    let v: f64 = cleaned.parse().ok()?;
    // Check finiteness of the SCALED value, not the bare mantissa: a finite mantissa can overflow
    // to ±inf once multiplied by the k/m/b/t suffix (e.g. "1e300t"). Returning Some(inf) here would
    // pass save-time validation and then poison the Summary aggregate (inf totals), so reject it.
    let scaled = v * mult;
    scaled.is_finite().then_some(scaled)
}

/// One owner's row in the value summary. Each field sums the parseable `approx_value`s for
/// that owner in the given kind + bucket.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnerValueRow {
    pub owner: String,
    pub asset_real_estate: f64,
    /// Cash-like assets (cash / savings / checking), segregated from After Tax.
    pub asset_cash: f64,
    pub asset_before_tax: f64,
    pub asset_after_tax: f64,
    /// All of this owner's liabilities. Liabilities are NOT split by tax bucket — there is no
    /// meaningful "before tax" liability — so they aggregate into this single column.
    pub liability: f64,
}

impl OwnerValueRow {
    pub fn asset_total(&self) -> f64 {
        self.asset_real_estate + self.asset_cash + self.asset_before_tax + self.asset_after_tax
    }
    pub fn liability_total(&self) -> f64 {
        self.liability
    }
    pub fn net(&self) -> f64 {
        self.asset_total() - self.liability_total()
    }
}

/// Build the owner × (Asset buckets / Liability buckets) summary from an asset/liability
/// iterator. Rows are sorted by owner (case-insensitive); a blank owner groups under
/// "(no owner)". An unparseable `approx_value` contributes 0 (the save-time validation keeps
/// real entries numeric). Used by the GUI + TUI Summary tabs.
pub fn owner_value_summary<'a>(items: impl IntoIterator<Item = &'a AssetLiability>) -> Vec<OwnerValueRow> {
    let mut map: std::collections::BTreeMap<String, OwnerValueRow> = std::collections::BTreeMap::new();
    for a in items {
        let owner = a.owner.trim();
        let disp = if owner.is_empty() { "(no owner)" } else { owner };
        let val = parse_approx_value(&a.approx_value).unwrap_or(0.0);
        let is_liab = a.kind.trim().eq_ignore_ascii_case("Liability");
        let row = map
            .entry(disp.to_lowercase())
            .or_insert_with(|| OwnerValueRow { owner: disp.to_string(), ..Default::default() });
        // Assets fall into Real-Estate / Before-Tax / After-Tax buckets; ALL liabilities go into
        // the single liability column (liabilities are not tax-split — see the module comment).
        if is_liab {
            row.liability += val;
        } else {
            match value_bucket(&a.asset_type, &a.institution, false) {
                ValueBucket::RealEstate => row.asset_real_estate += val,
                ValueBucket::Cash => row.asset_cash += val,
                ValueBucket::BeforeTax => row.asset_before_tax += val,
                ValueBucket::AfterTax => row.asset_after_tax += val,
            }
        }
    }
    map.into_values().collect()
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

/// The virtual folder a tax year's documents live in: `taxes/<sanitized-year>`.
/// Non-alphanumeric characters in the year are dropped so the folder name is
/// always safe; an empty/blank year falls back to `taxes/unspecified`. Shared by
/// the GUI and TUI so both store a given year's documents in the same place.
pub fn tax_doc_location(year: &str) -> String {
    let y: String = year.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if y.is_empty() { "taxes/unspecified".to_string() } else { format!("taxes/{y}") }
}

/// The virtual folder a property's documents live in: `real-estate/<sanitized>`,
/// derived from the address (alphanumeric only, lowercased, truncated), with a
/// `real-estate/property` fallback for a blank address. Shared by both UIs.
pub fn real_estate_doc_location(address: &str) -> String {
    let a: String =
        address.chars().filter(|c| c.is_ascii_alphanumeric()).take(40).collect::<String>().to_lowercase();
    if a.is_empty() { "real-estate/property".to_string() } else { format!("real-estate/{a}") }
}

/// Slugify one virtual-path component: lowercase, keep ASCII alphanumerics, turn
/// every other run into a single '-', trim leading/trailing '-', and cap the
/// length at 40. An empty result falls back to `fallback`. Used for the auto-group
/// level (document/description/title) and the optional user subfolder so the
/// volume path is always filesystem-safe and free of separators or traversal.
pub fn doc_slug(s: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.truncate(40);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { fallback.to_string() } else { out }
}

/// Uppercased initials of an owner for the owner-first directory level: the first
/// ASCII-alphanumeric character of each whitespace-separated word, uppercased and
/// concatenated, capped at 8 chars. No connector special-casing ("Michael and Sarah"
/// -> "MAS", "Michael & Sarah" -> "MS", "Joint" -> "J"). Returns "" when the owner is
/// blank or has no alphanumeric — callers then OMIT the initials level entirely.
pub fn owner_initials(owner: &str) -> String {
    let mut out = String::new();
    for word in owner.split_whitespace() {
        if let Some(c) = word.chars().find(char::is_ascii_alphanumeric) {
            out.push(c.to_ascii_uppercase());
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

/// Prepend the owner-initials top-level folder to a `<root>[/<group>]` base, giving the
/// owner-first layout `[<INITIALS>/]<base>`. When `owner` is `None` (tabs without an
/// owner) or the initials are empty (blank owner), `base` is returned unchanged.
pub fn owner_prefix(owner: Option<&str>, base: &str) -> String {
    match owner.map(owner_initials).filter(|i| !i.is_empty()) {
        Some(init) => format!("{init}/{base}"),
        None => base.to_string(),
    }
}

/// The per-tab `<root>[/<group>]` base for the Trust & Will and General Documents tabs
/// (the multi-doc Taxes/Real-Estate tabs have their own helpers above; Assets uses
/// [`asset_doc_location`] below = the kind root). The group is slugged from the record's
/// identifying field. The owner-initials top level (for owner-bearing tabs) is layered on
/// by [`owner_prefix`].
pub fn trust_will_doc_location(document: &str) -> String {
    format!("trust-will/{}", doc_slug(document, "document"))
}
/// The kind root for an Asset/Liability document: `liabilities` when the record's `kind`
/// is "Liability" (case-insensitive), else `assets`. Used as the `base` for
/// [`owner_prefix`], giving the owner-first `<INITIALS>/assets|liabilities`. No slugged
/// auto-group level (assets are grouped by owner, not description).
pub fn asset_doc_location(kind: &str) -> String {
    if kind.trim().eq_ignore_ascii_case("Liability") { "liabilities".to_string() } else { "assets".to_string() }
}
pub fn general_doc_location(title: &str) -> String {
    format!("general-documents/{}", doc_slug(title, "untitled"))
}

/// A compact UTC timestamp `YYYYMMDD-HHMMSS` from Unix seconds, prefixed onto each
/// uploaded filename ([`timestamped_filename`]). Sortable, fixed-width, filesystem-safe.
pub fn compact_utc(unix_secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix_secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// True if `s` is exactly a compact-UTC stamp `YYYYMMDD-HHMMSS` (8 digits, '-', 6 digits
/// = 15 chars). Used by the throwaway migration to locate/recognize the upload timestamp
/// in a stored path or filename prefix.
pub fn is_compact_utc(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 15 || b[8] != b'-' || !b[..8].iter().all(u8::is_ascii_digit) || !b[9..].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    // Validate the date/time components are PLAUSIBLE (not merely digit-shaped) so an arbitrary
    // all-digit directory or year (e.g. "12345678-901234") can't be misread as a timestamp by the
    // migration. All slices are ASCII digits, so parse always succeeds.
    let n = |lo: usize, hi: usize| s[lo..hi].parse::<u32>().unwrap_or(u32::MAX);
    let (mo, d, h, mi, sec) = (n(4, 6), n(6, 8), n(9, 11), n(11, 13), n(13, 15));
    (1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60 && sec < 60
}

/// Prefix an already-sanitized filename with the upload timestamp: `<ts>_<name>`.
/// `name` must be [`doc_filename`] output (<=120 B sanitized); `ts` is [`compact_utc`]
/// (15 B). The result is <=136 B for the last path component — well under MAX_PATH_LEN.
pub fn timestamped_filename(ts: &str, name: &str) -> String {
    format!("{ts}_{name}")
}

/// Build the virtual *directory* a freshly-uploaded document is filed under:
///   `<prefix>[/<subfolder>]`
/// `prefix` is the owner-first `[<INITIALS>/]<root>[/<group>]` (see [`owner_prefix`] plus
/// the per-tab `*_doc_location` helpers); `subfolder` is the optional user level (slugged,
/// omitted when blank). The per-upload timestamp is NOT a directory level any more — it is
/// folded into the filename via [`timestamped_filename`]. The caller appends the filename
/// with `vault::virtual_path`.
pub fn doc_upload_dir(prefix: &str, subfolder: &str) -> String {
    let mut dir = prefix.to_string();
    let sub = subfolder.trim();
    if !sub.is_empty() {
        dir.push('/');
        dir.push_str(&doc_slug(sub, "subfolder"));
    }
    dir
}

/// Sanitize a user-supplied filename for the volume path: replace any whitespace
/// with `-` (so no path component contains a space), neutralize path separators and
/// control characters with `_` (so the user controls the name without injecting
/// extra path levels or `..` traversal), strip surrounding dots, and cap the length.
/// Falls back to `"file"` when nothing usable remains. Dots inside the name are kept
/// so extensions like `return.pdf` survive.
/// A Unicode formatting/bidi/zero-width char that `char::is_control` (Cc-only) does
/// NOT catch but which can still spoof how a name/path DISPLAYS — most dangerously
/// the right-to-left override U+202E, which renders `report\u{202e}txt.exe` as
/// `report exe.txt`. Rejected/neutralized in document names and untrusted paths.
///
/// The set covers two families, and deliberately stops there (audit 2026-07-25 round 2):
///
/// * **Bidi controls** — every character that can reorder the run around it. The obvious
///   overrides/embeddings/isolates, *plus* U+061C ARABIC LETTER MARK: it is the ALM
///   counterpart of LRM/RLM (U+200E/U+200F) and reorders adjacent neutrals (digits,
///   punctuation) with no override needed, so omitting it left a hole in the exact family
///   this function exists to close.
/// * **Characters that render as nothing** — zero-width, invisible operators, blank
///   fillers, and the U+E0000 TAGS block (an entire shadow ASCII alphabet that draws no
///   glyph, the modern "invisible text" smuggling vector). Two labels or filenames that
///   differ only by these are indistinguishable on screen.
///
/// It is NOT "every Unicode `Cf`". Format characters that have a legitimate *visible*
/// rendering in a living script — the Arabic/Syriac/Kaithi number and honorific signs
/// (U+0600–U+0605, U+06DD, U+070F, U+0890, U+08E2, U+110BD, U+110CD), the Egyptian
/// hieroglyph joiners, the musical beam/tie/slur marks and the shorthand overlaps — are
/// left alone, as are the variation selectors (U+FE00–U+FE0F), which a legitimate emoji
/// needs to render in colour. Neutralizing those would mangle honest labels to no
/// security end: they are not invisible and they do not reorder.
pub(crate) fn is_spoofy_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                  // SOFT HYPHEN — invisible in virtually every renderer
        | '\u{061C}'                // ARABIC LETTER MARK — bidi control (the ALM twin of LRM/RLM)
        | '\u{115F}' | '\u{1160}'   // HANGUL CHOSEONG/JUNGSEONG FILLER — draw nothing
        | '\u{180E}'               // MONGOLIAN VOWEL SEPARATOR — zero-width (Cf since Unicode 6.3)
        | '\u{200B}'..='\u{200F}'   // zero-width space/joiners + LRM/RLM
        | '\u{2028}'              // LINE SEPARATOR — a real line break that char::is_control misses
        | '\u{2029}'              // PARAGRAPH SEPARATOR — likewise (keeps CSV cells one physical line)
        | '\u{202A}'..='\u{202E}' // bidi embeddings + LRO/RLO override
        | '\u{2060}'..='\u{2064}'   // word joiner + FUNCTION APPLICATION / INVISIBLE TIMES/SEPARATOR/PLUS
        | '\u{2066}'..='\u{2069}' // bidi isolates
        | '\u{206A}'..='\u{206F}'   // deprecated bidi/shaping controls (symmetric swapping, digit shapes)
        | '\u{3164}'                // HANGUL FILLER — draws nothing
        | '\u{FEFF}'              // zero-width no-break space / BOM
        | '\u{FFA0}'                // HALFWIDTH HANGUL FILLER — draws nothing
        | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotation — hides the annotated run
        | '\u{E0001}'               // LANGUAGE TAG (deprecated)
        | '\u{E0020}'..='\u{E007F}' // TAGS block — an invisible shadow ASCII alphabet
    )
}

/// Replace control characters and Unicode bidi/format/zero-width characters (everything
/// [`is_spoofy_format_char`] flags, plus [`char::is_control`]) with `_`, for rendering an
/// UNTRUSTED string into a context where those characters would spoof — a terminal line, a
/// merge preview the user authorizes, or a real on-disk filename. Unlike [`doc_filename`] it
/// does NOT touch separators/whitespace or cap length; it only neutralizes the invisible/bidi
/// spoof set, so it is safe to apply to an arbitrary display label without otherwise mangling it.
/// `pub` (not `pub(crate)`) so the desktop CLI `extract` (main.rs) can apply the same
/// neutralization to a filename derived from an untrusted manifest path.
pub fn display_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || is_spoofy_format_char(c) { '_' } else { c }).collect()
}

/// True if `name`'s stem (the part before the first '.') is a Windows reserved DEVICE name
/// (case-insensitive): CON, PRN, AUX, NUL, CONIN$, CONOUT$, COM1–9, LPT1–9. On Windows such
/// a name maps to a device, not a file, regardless of extension (`con.pdf` opens the
/// console), so it must be neutralized before becoming a real filesystem path component on
/// export — otherwise an heir extracting on Windows gets an I/O error instead of the document.
///
/// `pub` so the desktop `extract` CLI shares this one definition rather than keeping its own
/// copy (which had drifted: it recognized a shorter list and *dropped* the offending
/// component instead of renaming it, so the same vault extracted to a different tree
/// depending on which front-end wrote it — audit 2026-07-25 round 2).
///
/// Two forms beyond the classic list are covered because Windows folds them onto a device:
/// the console handles `CONIN$`/`CONOUT$`, and the SUPERSCRIPT digit spellings `COM¹`/`COM²`/
/// `COM³` (U+00B9/U+00B2/U+00B3), which its path canonicalization maps to `COM1`/`COM2`/`COM3`.
pub fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    // Uppercase, and fold the superscript digits onto their ASCII twins, so `com¹` is
    // recognized as `COM1`. Both source chars are 2 bytes and both replacements are 1, so
    // the byte-length test below still sees a 4-byte `COM1`-shaped stem.
    let s: String = stem
        .chars()
        .map(|c| match c {
            '\u{00B9}' => '1',
            '\u{00B2}' => '2',
            '\u{00B3}' => '3',
            _ => c.to_ascii_uppercase(),
        })
        .collect();
    matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        // `len() == 4` is in BYTES, but `starts_with("COM")` pins the first three to ASCII,
        // so a 4-byte match always has a single-byte 4th character to index.
        || (s.len() == 4
            && (s.starts_with("COM") || s.starts_with("LPT"))
            && matches!(s.as_bytes()[3], b'1'..=b'9'))
}

pub fn doc_filename(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                '-' // no spaces (or tabs/newlines) anywhere in a volume path
            } else if c == '/' || c == '\\' || c.is_control() || is_spoofy_format_char(c) {
                '_' // neutralize separators, control, AND bidi/zero-width spoof chars
            } else {
                c
            }
        })
        .collect();
    // Cap at 120 bytes, truncating on a UTF-8 char boundary. A raw `truncate(120)`
    // PANICS when byte 120 lands mid-character (multibyte name: accented Latin, CJK,
    // emoji, …), so step the cut back to the nearest boundary first. Inner helper so the
    // cap can be re-applied AFTER the reserved-name prefix below (which can add a byte).
    fn cap_120(s: &mut String) {
        if s.len() > 120 {
            let mut cut = 120;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
        }
    }
    cap_120(&mut out);
    // Strip leading/trailing dots and dashes (whitespace is already mapped to `-`),
    // so a dot/space-only name collapses to the fallback rather than "--..".
    let trimmed = out.trim_matches(|c: char| c == '.' || c == '-');
    if trimmed.is_empty() {
        return "file".to_string();
    }
    // Neutralize a Windows reserved device name so the stored (and later exported) file is a
    // real, distinct file on Windows rather than the CON/NUL/COM1/… device. Harmless on Unix.
    let mut out = if is_windows_reserved_name(trimmed) { format!("_{trimmed}") } else { trimmed.to_string() };
    // The reserved-name '_' prefix can push a name that was already at the 120-byte cap to
    // 121, so re-cap (on a char boundary) and re-trim any trailing dot/dash the cut exposed,
    // keeping BOTH the length and no-edge-dot invariants. (Caught by the doc_paths fuzz target.)
    if out.len() > 120 {
        cap_120(&mut out);
        let keep = out.trim_end_matches(['.', '-']).len();
        out.truncate(keep);
    }
    if out.is_empty() { "file".to_string() } else { out }
}

/// Resolve the upload filename: the user-typed `name` if non-empty (trimmed), else the
/// **basename of the `source` path** ("if a filename isn't specified, use the same
/// filename as the file being uploaded"). The result is NOT yet sanitized — callers run
/// it through [`doc_filename`]. Returns `""` only if both are empty / the source has no
/// final component, which callers reject.
pub fn effective_doc_filename(name: &str, source: &str) -> String {
    let n = name.trim();
    if !n.is_empty() {
        return n.to_string();
    }
    std::path::Path::new(unquote_path(source))
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Normalize a user-typed "upload from" source path: trim surrounding whitespace, then
/// strip a single MATCHED pair of surrounding ASCII double quotes. File managers'
/// "Copy as path" (Windows Explorer) and shells wrap a path — especially one containing
/// spaces — in double quotes, so accept that form and let the user paste it directly.
/// Only a matched leading+trailing pair is removed; the content INSIDE the quotes is left
/// exactly as-is (quotes preserve inner spaces, matching shell semantics), and a lone
/// quote at just one end is left alone (it is a legitimate, if unusual, path character).
pub fn unquote_path(s: &str) -> &str {
    let t = s.trim();
    // `strip_prefix`/`strip_suffix` return `None` when the affix is absent. Require BOTH
    // (and length >= 2 so a single `"` isn't treated as an empty quoted string).
    if t.len() >= 2
        && let Some(inner) = t.strip_prefix('"').and_then(|r| r.strip_suffix('"'))
    {
        inner
    } else {
        t
    }
}

/// Break a unix-seconds timestamp into civil UTC `(year, month, day, hour, min,
/// sec)` using Howard Hinnant's `civil_from_days` algorithm. Negative/zero clamps
/// to the epoch. Shared by the human and filename timestamp formatters so the
/// (fiddly) calendar math lives in exactly one place.
// `pub(crate)` = visible anywhere in this crate but not to outside users.
// The return type `(i64, i64, ...)` is a *tuple*: several values bundled together.
pub fn civil_from_unix(ts: i64) -> (i64, i64, i64, i64, i64, i64) {
    // `let ts = ...` here *shadows* the parameter `ts`: a new binding reusing the name.
    // `.clamp(lo, hi)` keeps the value inside the representable 4-digit-year calendar: negatives
    // become the epoch, and anything past 9999-12-31T23:59:59Z is capped there. This guarantees
    // year ∈ [1970, 9999], so the `{:04}`/`{:02}` formatters (iso_utc, compact_utc, format_time,
    // fmt_unix) always emit fixed-width fields — a crafted/odd created_at/updated_at (up to
    // i64::MAX) can never widen the year and desync a CSV column or a timestamped filename.
    let ts = ts.clamp(0, 253_402_300_799); // 253_402_300_799 = unix_from_civil(9999, 12, 31, 23, 59, 59)
    let days = ts.div_euclid(86_400);
    let sod = ts.rem_euclid(86_400);
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, h, m, s)
}

/// Days since the Unix epoch for a civil UTC date — Howard Hinnant's
/// `days_from_civil`, the exact inverse of the `civil_from_unix` calendar math
/// above (proleptic Gregorian). `div_euclid(400)` is floored division, which is
/// what the algorithm needs for the era.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // March-based year: shift Jan/Feb into the previous year so the leap day is
    // the last day of the year, simplifying the day-of-year formula.
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400; // year of era, [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // month, March=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // day of year, [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era, [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Unix seconds for a civil UTC date-time — inverse of `civil_from_unix`.
pub(crate) fn unix_from_civil(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// Parse a `YYYY-MM-DD` date as **UTC midnight**, returning Unix seconds.
/// Returns `None` for malformed input or an impossible calendar date (e.g.
/// `2026-02-31`), which the round-trip canonicalization check rejects. Used by
/// the `compact --history-before` cutoff.
pub fn parse_ymd_utc(s: &str) -> Option<i64> {
    // `split('-')` then `collect` into a Vec so we can require exactly 3 fields.
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    // `parse::<i64>()` returns Err on non-numeric text; `.ok()?` maps that to None.
    let y: i64 = parts[0].parse().ok()?;
    let mo: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[2].parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let ts = unix_from_civil(y, mo, d, 0, 0, 0);
    // Canonicalization: re-deriving the date must reproduce the input, which
    // rejects impossible dates (Feb 31, Apr 31, ...) that days_from_civil would
    // otherwise silently normalize.
    let (cy, cmo, cd, ..) = civil_from_unix(ts);
    if (cy, cmo, cd) != (y, mo, d) {
        return None;
    }
    Some(ts)
}

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
    n
}

/// Apply the history trim to one record collection; returns entries removed.
// Generic over any `Record` (uses its `history_mut` accessor). `&mut [R]` borrows
// the caller's Vec as a mutable slice. `retain` keeps only matching elements,
// dropping (and zeroizing) the rest in place.
fn trim_histories<R: Record>(list: &mut [R], cutoff: Option<i64>, drop_all: bool) -> usize {
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
fn track(out: &mut Vec<Change>, at: i64, name: &str, old: &str, new: &str) {
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
fn track_bool(out: &mut Vec<Change>, at: i64, name: &str, old: bool, new: bool) {
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
