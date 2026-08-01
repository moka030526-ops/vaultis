//! Unit tests for the parent module ([`super`], `merge.rs`), split into their own
//! file via `#[cfg(test)] #[path = "merge_tests.rs"] mod tests;` so the tests do not sit
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
use crate::records::Instruction;

fn instr(id: &str, updated_at: i64) -> Instruction {
    let mut r = Instruction::new().unwrap();
    r.id = id.to_string();
    r.title = format!("title-{id}");
    r.updated_at = updated_at;
    r.created_at = 1;
    r
}

#[test]
fn collection_changes_selects_new_and_strictly_newer() {
    let current = vec![instr("a", 100), instr("b", 200)];
    // a: newer in source -> Updated; b: equal -> skip; c: absent -> New; d: older -> skip.
    let source = vec![instr("a", 150), instr("b", 200), instr("c", 50), instr("d", 999)];
    let mut current2 = current.clone();
    current2.push(instr("d", 1000)); // d newer in dest -> not selected
    let sel = collection_changes(&current2, &source);
    let ids: Vec<(usize, ChangeKind)> = sel.iter().map(|s| (s.source_index, s.change)).collect();
    assert_eq!(ids, vec![(0, ChangeKind::Updated), (2, ChangeKind::New)]);
    // The Updated selection carries the destination's current timestamp.
    assert_eq!(sel[0].current_updated_at, Some(100));
    assert_eq!(sel[1].current_updated_at, None);
}

#[test]
fn merge_records_replaces_verbatim_and_inserts() {
    let mut current = vec![instr("a", 100), instr("b", 200)];
    let source = vec![instr("a", 150), instr("c", 50)];
    let apply: HashSet<&str> = ["a", "c"].into_iter().collect();
    let (added, updated) = merge_records(&mut current, &source, &apply);
    assert_eq!((added, updated), (1, 1));
    // 'a' replaced verbatim: the source's updated_at survived (idempotency).
    let a = current.iter().find(|r| r.id == "a").unwrap();
    assert_eq!(a.updated_at, 150, "source updated_at preserved, not stamped to now");
    // 'b' untouched, 'c' inserted.
    assert!(current.iter().any(|r| r.id == "b" && r.updated_at == 200));
    assert!(current.iter().any(|r| r.id == "c"));
    assert_eq!(current.len(), 3);
}

#[test]
fn duplicate_source_ids_are_selected_and_applied_once() {
    // A crafted source with two records sharing an id in one collection.
    let current = vec![instr("a", 100)];
    let source = vec![instr("a", 200), instr("a", 300)];
    // collection_changes selects the id ONCE (first occurrence).
    let sel = collection_changes(&current, &source);
    assert_eq!(sel.len(), 1, "shared id selected once, not twice");
    assert_eq!(sel[0].source_index, 0);
    // merge_records applies it once → one update, not insert+replace.
    let mut cur2 = current.clone();
    let apply: HashSet<&str> = ["a"].into_iter().collect();
    let (added, updated) = merge_records(&mut cur2, &source, &apply);
    assert_eq!((added, updated), (0, 1), "applied once");
    assert_eq!(cur2.len(), 1);
}

#[test]
fn positional_first_occurrence_wins_among_duplicate_source_ids() {
    // When a crafted source lists the same id twice with DIFFERENT updated_at, the
    // merge keeps the FIRST positional occurrence — NOT the newest. This behaviour is
    // load-bearing (the `seen`/`done` HashSet guards in collection_changes/merge_records
    // walk source in index order) yet was previously unasserted: a refactor to
    // max-updated-at or last-occurrence would silently change which value the user gets.
    let apply: HashSet<&str> = ["a"].into_iter().collect();
    let mut dest1: Vec<Instruction> = vec![];
    merge_records(&mut dest1, &[instr("a", 100), instr("a", 300)], &apply);
    assert_eq!(dest1.len(), 1);
    assert_eq!(dest1[0].updated_at, 100, "first occurrence wins, not the newest");
    // Reversing the slice changes the winner — proving it is positional, not by recency.
    let mut dest2: Vec<Instruction> = vec![];
    merge_records(&mut dest2, &[instr("a", 300), instr("a", 100)], &apply);
    assert_eq!(dest2.len(), 1);
    assert_eq!(dest2[0].updated_at, 300, "reversing the slice changes the winner");
}

#[test]
fn merge_records_ignores_ids_not_in_apply_set() {
    let mut current = vec![instr("a", 100)];
    let source = vec![instr("a", 999), instr("z", 999)];
    let apply: HashSet<&str> = HashSet::new(); // accept nothing
    let (added, updated) = merge_records(&mut current, &source, &apply);
    assert_eq!((added, updated), (0, 0));
    assert_eq!(current[0].updated_at, 100, "untouched when not accepted");
}

#[test]
fn plan_counters() {
    let rec = |change| PlannedRecord {
        kind: RecordKind::Account,
        change,
        id: "x".into(),
        label: "x".into(),
        current_updated_at: None,
        source_updated_at: 9,
    };
    // Deliberately use counts that are NOT 1, so a "always return 1" mutation of any
    // counter is caught (and not just "always 0").
    let plan = MergePlan {
        records: vec![rec(ChangeKind::New), rec(ChangeKind::New), rec(ChangeKind::Updated), rec(ChangeKind::Updated)],
        blobs: vec![
            PlannedBlob { id: "b1".into(), path: "/a".into(), size: 10, already_present: false },
            PlannedBlob { id: "b2".into(), path: "/b".into(), size: 4, already_present: false },
            PlannedBlob { id: "b3".into(), path: "/c".into(), size: 7, already_present: true },
        ],
        skipped: vec![],
        new_categories: vec![],
        source_vault_id: "vid".into(),
    };
    assert!(!plan.is_empty());
    assert_eq!(plan.new_count(), 2);
    assert_eq!(plan.updated_count(), 2);
    assert_eq!(plan.blobs_to_copy(), 2, "only the two not-already-present blobs");
    assert_eq!(plan.bytes_to_copy(), 14, "10 + 4, excluding the already-present blob");
    // An empty plan reports empty + zero counts (kills "always 1"/"always non-empty").
    let empty = MergePlan::default();
    assert!(empty.is_empty());
    assert_eq!(empty.new_count(), 0);
    assert_eq!(empty.updated_count(), 0);
    assert_eq!(empty.blobs_to_copy(), 0);
    assert_eq!(empty.bytes_to_copy(), 0);
}

#[test]
fn kind_and_change_display_strings() {
    // Pin the human-readable labels so a mutated `as_str` (empty / garbage) is caught.
    assert_eq!(RecordKind::Instruction.as_str(), "Instruction");
    assert_eq!(RecordKind::TrustWill.as_str(), "Trust & Will");
    assert_eq!(RecordKind::Asset.as_str(), "Asset/Liability");
    assert_eq!(RecordKind::Account.as_str(), "Account");
    assert_eq!(RecordKind::RealEstate.as_str(), "Real Estate");
    assert_eq!(RecordKind::TaxFiling.as_str(), "Tax filing");
    assert_eq!(RecordKind::GeneralDocument.as_str(), "General document");
    assert_eq!(ChangeKind::New.as_str(), "new");
    assert_eq!(ChangeKind::Updated.as_str(), "updated");
}
