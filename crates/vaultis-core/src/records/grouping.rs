//! Grouped views over the records: the Accounts filter facets, the account and asset
//! trees, and the links between assets and the accounts that hold them.

use super::*;

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
pub(super) fn acct_match(a: &Account, t: &str, st: &str, o: &str, ti: &str, query: &str, review_only: bool) -> bool {
    (t.is_empty() || a.account_type == t)
        && (st.is_empty() || a.account_subtype == st)
        && (o.is_empty() || a.owner == o)
        && (ti.is_empty() || a.title == ti)
        && (!review_only || a.review)
        && matches_search_soundlike(&a.username, query)
}

/// Distinct, sorted, non-empty values of `field` over the accounts that pass
/// `keep` — the building block for one facet.
pub(super) fn facet<F: Fn(&Account) -> &str, K: Fn(&Account) -> bool>(accounts: &[Account], field: F, keep: K) -> Vec<String> {
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
    pub(super) fn child_mut(&mut self, label: &str) -> &mut AcctNode {
        match self.children.iter().position(|c| c.label == label) {
            Some(i) => &mut self.children[i],
            None => {
                self.children.push(AcctNode { label: label.to_string(), ..Default::default() });
                self.children.last_mut().unwrap()
            }
        }
    }
    /// Sort children by label and leaves by title, case-insensitively, recursively.
    pub(super) fn sort_recursive(&mut self) {
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
