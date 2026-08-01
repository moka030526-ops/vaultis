//! Unit tests for the parent module ([`super`], `gui.rs`), split into their own
//! file via `#[cfg(test)] #[path = "gui_kittest_tests.rs"] mod kittest_tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use super::render_acct_node;
use crate::records::{AccountLeaf, AcctNode};
use eframe::egui;
use egui_kittest::{kittest::Queryable, Harness};

/// Every article in the manual must actually render. The help browser nests
/// panels (top + left + central) inside the screen's own CentralPanel and builds
/// per-table grid ids from content pointers, so a layout or duplicate-id fault
/// would only ever show up when a real egui lays it out — not in a data test.
#[test]
fn every_help_topic_renders_in_real_egui() {
    use crate::gui_help::{HelpContext, HelpState, TOPICS};

    for (i, topic) in TOPICS.iter().enumerate() {
        let state = std::cell::RefCell::new(HelpState { query: String::new(), topic: i });
        let ctx = HelpContext {
            vault: "/tmp/vault/vault.pmv".into(),
            prefs: "/tmp/prefs.json".into(),
            writable: true,
        };
        let mut h = Harness::new_ui(|ui| {
            let mut s = state.borrow_mut();
            crate::gui_help::ui(ui, &mut s, &ctx, egui::Color32::from_rgb(21, 92, 170));
        });
        h.run();
        // The title renders twice — once in the index, once as the article's
        // heading — so this counts matches rather than expecting exactly one.
        assert!(
            h.query_all_by_label(topic.title).count() >= 2,
            "help topic {:?} did not render in both the index and the body",
            topic.id
        );
    }
}

/// Searching must narrow the visible index and leave the browser on a topic the
/// index still lists — the case where the previously-selected article is filtered
/// away is exactly where a stale index would render an unreachable page.
#[test]
fn help_search_narrows_the_index_and_follows_the_selection_in_real_egui() {
    use crate::gui_help::{HelpContext, HelpState};

    // Start on the LAST topic, then search for something only an early topic
    // matches: the selection must move to a topic that is still listed.
    let last = crate::gui_help::TOPICS.len() - 1;
    let state = std::cell::RefCell::new(HelpState { query: "argon2id".into(), topic: last });
    let ctx =
        HelpContext { vault: "/v".into(), prefs: "/p".into(), writable: false };
    let mut h = Harness::new_ui(|ui| {
        let mut s = state.borrow_mut();
        crate::gui_help::ui(ui, &mut s, &ctx, egui::Color32::from_rgb(21, 92, 170));
    });
    h.run();
    let landed = state.borrow().topic;
    let hits = crate::gui_help::search("argon2id");
    assert!(hits.contains(&landed), "the browser must land on a topic the filtered index lists");
    assert!(
        h.query_all_by_label(crate::gui_help::TOPICS[landed].title).count() >= 2,
        "the followed-to article renders, in the index and as the article heading"
    );
}

fn one_group_tree(group: &str, leaf_id: &str, leaf_title: &str) -> AcctNode {
    AcctNode {
        label: String::new(),
        children: vec![AcctNode {
            label: group.into(),
            children: vec![],
            leaves: vec![AccountLeaf { id: leaf_id.into(), title: leaf_title.into() }],
        }],
        leaves: vec![],
    }
}

// The PRE-FIX render: id_salt WITHOUT the per-tree `kind` discriminant (used as a negative
// control to prove the test actually detects the shared-state bug).
fn render_buggy(ui: &mut egui::Ui, node: &AcctNode, path: &mut Vec<String>) {
    for child in &node.children {
        path.push(child.label.clone());
        egui::CollapsingHeader::new(&child.label)
            .id_salt(("group_node", path.as_slice()))
            .show(ui, |ui| render_buggy(ui, child, path));
        path.pop();
    }
    for leaf in &node.leaves {
        let _ = ui.selectable_label(false, &leaf.title);
    }
}

#[test]
fn error_banner_renders_and_dismiss_clears_it_in_real_egui() {
    use super::show_error_banner;
    use std::cell::RefCell;

    // A live failure message: the conspicuous banner must render with a Dismiss control.
    let error = RefCell::new(Some("Save failed: disk full".to_string()));
    let mut h = Harness::new_ui(|ui| {
        let mut e = error.borrow_mut();
        show_error_banner(&mut e, ui);
    });
    h.run();
    // The banner is on-screen (its Dismiss button is the deterministic, queryable marker).
    assert!(
        h.query_by_label("Dismiss ×").is_some(),
        "the conspicuous error banner renders while an error is set"
    );

    // Clicking Dismiss clears the error and removes the banner entirely.
    h.get_by_label("Dismiss ×").click();
    h.run();
    assert!(error.borrow().is_none(), "Dismiss clears the stored error");
    assert!(
        h.query_by_label("Dismiss ×").is_none(),
        "the banner is gone after dismissal (nothing rendered when error is None)"
    );
}

/// A long failure must reflow inside the banner, not run off the window.
///
/// `Sides` shrinks the message side to leave room for Dismiss, but a shrinking
/// side still defaults to `TextWrapMode::Extend` — so the text simply overflowed
/// the window and the only way to read the end of it was to widen the window,
/// which defeats the whole point of a conspicuous banner. Wrapping makes the
/// banner grow DOWNWARD instead, which is what this asserts: the same banner is
/// taller for a long message than for a short one.
#[test]
fn a_long_error_message_wraps_and_grows_the_banner_in_real_egui() {
    use super::show_error_banner;
    use std::cell::{Cell, RefCell};

    let height_of = |msg: &str| -> f32 {
        let error = RefCell::new(Some(msg.to_string()));
        let drawn = Cell::new(0.0f32);
        let mut h = Harness::builder().with_size(egui::vec2(420.0, 300.0)).build_ui(|ui| {
            let before = ui.min_rect().height();
            show_error_banner(&mut error.borrow_mut(), ui);
            drawn.set(ui.min_rect().height() - before);
        });
        h.run();
        drawn.get()
    };

    let short = height_of("Save failed.");
    let long = height_of(
        "Save failed: no space left on device while writing the vault to \
         /a/very/long/path/that/keeps/going/somewhere/deep/on/disk/vault.pmv — \
         the vault on disk is unchanged and still openable.",
    );
    assert!(
        long > short,
        "a long failure must wrap onto more lines ({long}pt) than a short one ({short}pt) \
         instead of running off the window"
    );
}

#[test]
fn grouped_account_and_asset_trees_expand_independently_in_real_egui() {
    // Both trees share the group label "Bob" but have uniquely-labelled leaves, so a leaf
    // being visible tells us exactly which tree's "Bob" is expanded.
    use std::cell::Cell;
    let acct = one_group_tree("Bob", "a1", "acct-leaf");
    let asset = one_group_tree("Bob", "s1", "asset-leaf");
    let labels: Vec<(String, String)> = vec![];

    // Faithfully model the real bug, which is CROSS-TAB: only one tab renders per frame, but
    // both share the same egui Context (hence the persistent collapse state). `tab` selects
    // which tree the harness renders this frame (0 = Accounts, 1 = Assets); `fixed` picks the
    // real render_acct_node (per-tree id) vs the pre-fix shared-id render. Returns whether the
    // Assets "Bob" leaked OPEN after we expanded the Accounts "Bob" and switched tabs.
    let asset_leaks_after_expanding_accounts = |fixed: bool| -> bool {
        let tab = Cell::new(0u8);
        let mut h = Harness::new_ui(|ui| {
            let mut p = Vec::new();
            let (tree, kind) = if tab.get() == 0 { (&acct, "acct") } else { (&asset, "asset") };
            if fixed {
                render_acct_node(ui, tree, &mut p, None, &labels, kind);
            } else {
                render_buggy(ui, tree, &mut p);
            }
        });
        // Accounts tab: expand "Bob".
        tab.set(0);
        h.run();
        assert!(h.query_by_label("acct-leaf").is_none(), "accounts/Bob collapsed before the click");
        h.get_by_label("Bob").click();
        h.run();
        assert!(h.query_by_label("acct-leaf").is_some(), "accounts/Bob expanded after the click");
        // Switch to the Assets tab (same Context → shared persistent state) and observe.
        tab.set(1);
        h.run();
        h.query_by_label("asset-leaf").is_some()
    };

    // FIX: expanding Accounts/Bob then switching to Assets leaves Assets/Bob COLLAPSED.
    assert!(
        !asset_leaks_after_expanding_accounts(true),
        "FIX: Assets/Bob must stay collapsed after expanding Accounts/Bob (independent state)"
    );
    // NEGATIVE CONTROL: the pre-fix shared id DOES leak the expand across tabs — proving the
    // test detects the real bug, and that the discriminant is what prevents it.
    assert!(
        asset_leaks_after_expanding_accounts(false),
        "control: the pre-fix shared id leaks the expand to the Assets tab (reproduces the bug)"
    );
}
