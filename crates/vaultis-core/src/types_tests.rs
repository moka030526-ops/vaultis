//! Unit tests for the parent module ([`super`], `types.rs`), split into their own
//! file via `#[cfg(test)] #[path = "types_tests.rs"] mod tests;` so the tests do not sit
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

#[test]
fn defaults_are_populated() {
    let t = TypeLists::with_defaults();
    assert!(!t.asset.is_empty());
    assert!(t.account_type_names().contains(&"Financial".to_string()));
    assert!(t.subtypes_for("Financial").contains(&"Bank".to_string()));
    assert!(t.subtypes_for("nope").is_empty());
}

#[test]
fn add_asset_type_dedups_case_insensitively_and_sorts() {
    let mut t = TypeLists::default();
    assert!(t.add_asset_type("Crypto"));
    assert!(!t.add_asset_type("  crypto ")); // case-insensitive dup
    assert!(!t.add_asset_type("   ")); // blank
    assert!(t.add_asset_type("Annuity"));
    assert_eq!(t.asset, vec!["Annuity".to_string(), "Crypto".to_string()]); // sorted
}

#[test]
fn add_account_type_and_subtype() {
    let mut t = TypeLists::default();
    assert!(t.add_account_type("Crypto"));
    assert!(!t.add_account_type("crypto")); // dup
    assert!(t.add_account_subtype("Crypto", "Exchange"));
    assert!(!t.add_account_subtype("Crypto", "exchange")); // dup
    assert!(!t.add_account_subtype("Unknown", "X")); // unknown type
    assert!(!t.add_account_subtype("Crypto", "  ")); // blank
    assert_eq!(t.subtypes_for("Crypto"), vec!["Exchange".to_string()]);
}

#[test]
fn remove_types_and_subtypes_case_insensitively() {
    let mut t = TypeLists::default();
    t.add_asset_type("Crypto");
    t.add_account_type("Bank");
    t.add_account_subtype("Bank", "Checking");
    t.add_account_subtype("Bank", "Savings");

    // Asset type: case-insensitive removal; no-op when absent.
    assert!(t.remove_asset_type("crypto")); // case-insensitive match of stored "Crypto"
    assert!(!t.remove_asset_type("Crypto"), "already gone");
    assert!(t.asset.is_empty());

    // Subtype removal is case-insensitive and scoped to the type.
    assert!(t.remove_account_subtype("bank", "CHECKING"));
    assert_eq!(t.subtypes_for("Bank"), vec!["Savings".to_string()]);
    assert!(!t.remove_account_subtype("Bank", "Checking"), "already gone");
    assert!(!t.remove_account_subtype("Unknown", "x"), "unknown type");

    // Account type removal drops the whole entry (incl. remaining subtypes).
    assert!(t.remove_account_type("BANK"));
    assert!(!t.account_type_names().contains(&"Bank".to_string()));
    assert!(t.subtypes_for("Bank").is_empty());
    assert!(!t.remove_account_type("Bank"), "already gone");
}

#[test]
fn default_is_empty_so_serde_round_trips() {
    let t = TypeLists::default();
    assert!(t.asset.is_empty() && t.account.is_empty());
    // These return `Result<T, E>` (Ok value or Err). `.unwrap()` extracts
    // the Ok value and *panics* on Err — fine in a test, where a serde
    // failure should loudly fail the test rather than be handled.
    let json = serde_json::to_string(&t).unwrap();
    let back: TypeLists = serde_json::from_str(&json).unwrap();
    assert_eq!(t, back);
}
