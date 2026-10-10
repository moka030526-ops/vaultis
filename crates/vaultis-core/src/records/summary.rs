//! Approximate value totals: bucketing assets and liabilities, parsing the free-text
//! value field, the owner × bucket summary, and the Zakat totals.

use super::*;

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

/// One zakat amount as a number: blank = 0, otherwise the lenient money parse.
/// Shared by [`ZakatEntry::remaining`] and [`zakat_totals`] so the table's rows and its
/// total row can never disagree about what a blank cell means.
pub fn zakat_amount(s: &str) -> Option<f64> {
    if s.trim().is_empty() { Some(0.0) } else { parse_approx_value(s) }
}

/// Column totals for the Zakat table: `(due, paid, remaining)`.
///
/// Unparseable cells contribute 0 to their column (the same "an unparseable value
/// aggregates as 0" rule `owner_value_summary` uses), and `remaining` is the difference
/// of the two totals — so the total row is always internally consistent even when an
/// individual row shows "—".
// `impl Iterator<Item = &'a ZakatEntry>` accepts any iterator over borrowed entries, so
// callers can pass `.iter()` or a filtered view without allocating a Vec first.
pub fn zakat_totals<'a>(rows: impl Iterator<Item = &'a ZakatEntry>) -> (f64, f64, f64) {
    let mut due = 0.0;
    let mut paid = 0.0;
    for r in rows {
        due += zakat_amount(&r.amount_due).unwrap_or(0.0);
        paid += zakat_amount(&r.amount_paid).unwrap_or(0.0);
    }
    (due, paid, due - paid)
}
