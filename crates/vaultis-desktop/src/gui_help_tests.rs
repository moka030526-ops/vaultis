//! Unit tests for the parent module ([`super`], `gui_help.rs`), split into their own
//! file via `#[cfg(test)] #[path = "gui_help_tests.rs"] mod tests;` so the tests do not sit
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

/// Every block kind, rendered in a deliberately NARROW pane, must stay inside it.
///
/// The bug this pins: egui wraps text by default only in a vertical layout, so
/// the manual's bullets, numbered steps, callouts and two-column tables — all of
/// which draw their text in a horizontal layout or a grid — silently defaulted to
/// `TextWrapMode::Extend` and ran off the right edge of the window. The reader had
/// to widen the window to see the end of a sentence. Asserting on the drawn width
/// (rather than eyeballing a screenshot) catches a regression in any one block kind.
#[test]
fn every_block_kind_wraps_inside_a_narrow_pane() {
    use egui_kittest::Harness;
    use std::cell::Cell;

    const LONG: &str = "This sentence is deliberately far longer than any narrow \
                        window could ever show on a single line, so if it does not \
                        wrap it will run off the right-hand edge of the pane.";
    const ROWS: &[(&str, &str)] = &[("A key", LONG), ("Another key", LONG)];
    const PANE: f32 = 320.0;

    for block in [
        Block::P(LONG),
        Block::Sub(LONG),
        Block::Bullets(&[LONG, LONG]),
        Block::Steps(&[LONG, LONG]),
        Block::Rows(ROWS),
        Block::Note(LONG),
        Block::Warn(LONG),
    ] {
        let drawn = Cell::new(0.0f32);
        let mut h = Harness::new_ui(|ui| {
            ui.set_max_width(PANE);
            render_block(ui, &block, egui::Color32::from_rgb(0, 120, 200));
            drawn.set(ui.min_rect().width());
        });
        // Two frames: a grid only knows its column widths from the previous frame.
        h.run();
        h.run();
        assert!(
            drawn.get() <= PANE + 1.0,
            "{block:?} drew {}pt wide in a {PANE}pt pane — it is not wrapping",
            drawn.get()
        );
    }
}

#[test]
fn every_topic_is_well_formed_and_uniquely_identified() {
    let mut ids: Vec<&str> = Vec::new();
    for t in TOPICS {
        assert!(!t.title.is_empty(), "topic {} has no title", t.id);
        assert!(!t.blurb.is_empty(), "topic {} has no blurb", t.id);
        assert!(!t.body.is_empty(), "topic {} has an empty body", t.id);
        assert!(
            SECTIONS.contains(&t.section),
            "topic {} is filed under unknown section {:?}",
            t.id,
            t.section
        );
        assert!(!ids.contains(&t.id), "duplicate topic id {}", t.id);
        ids.push(t.id);
    }
    // Every declared section must actually have topics, or the index would
    // render a heading with nothing under it.
    for s in SECTIONS {
        assert!(TOPICS.iter().any(|t| t.section == *s), "section {s:?} has no topics");
    }
}

#[test]
fn search_matches_title_body_and_requires_every_word() {
    // Empty query = the whole manual.
    assert_eq!(search("").len(), TOPICS.len());
    assert_eq!(search("   ").len(), TOPICS.len());

    // Matches on the title, case-insensitively.
    let hits = search("URGENT");
    assert!(hits.iter().any(|i| TOPICS[*i].id == "tab-urgent"));

    // Matches on body text, not just titles: "Argon2id" appears only in prose.
    let hits = search("argon2id");
    assert!(!hits.is_empty(), "a word that appears only in body text must be findable");
    assert!(hits.iter().any(|i| TOPICS[*i].id == "passwords" || TOPICS[*i].id == "security"));

    // AND semantics: adding a word narrows the result rather than widening it.
    let one = search("export").len();
    let two = search("export clipboard").len();
    assert!(two <= one, "a second word must not widen the result set ({two} > {one})");

    // A word in no article matches nothing.
    assert!(search("zzzznotaword").is_empty());
}

/// Each feature the manual is supposed to explain must be REACHABLE by the words a user
/// would type into the help search — a topic nobody can find is not documentation. The
/// query/topic pairs below are the behaviours that are not self-evident from the screen.
#[test]
fn every_documented_feature_is_findable_by_search() {
    for (query, expect_id) in [
        // Quoted / pasted paths, in the dedicated topic and where they are used.
        ("quoted path", "paths"),
        ("copy as path", "paths"),
        // The search box: how it looks and how it matches.
        ("sound-alike", "searching"),
        ("search box", "searching"),
        // The searchable link dropdown.
        ("link an account dropdown", "links"),
        // The sample vault the build script produces.
        ("sample vault", "demo-vault"),
        ("sample1", "demo-vault"),
        // What a crash or a failed save does to the vault, and the optional spare copies.
        ("power cut", "interrupted"),
        ("rotating spare", "interrupted"),
        // Long-standing behaviour that is easy to miss.
        ("trim all fields", "editing"),
        ("before tax bucket", "tab-summary"),
    ] {
        let hits = search(query);
        assert!(
            hits.iter().any(|i| TOPICS[*i].id == expect_id),
            "searching the manual for {query:?} must reach the {expect_id:?} topic; \
             it matched {:?}",
            hits.iter().map(|i| TOPICS[*i].id).collect::<Vec<_>>()
        );
    }
}

/// Every cross-reference — the manual's `See “Article title”` / `see “Article title”` form —
/// must name a topic that actually exists, so following one never sends the reader hunting for
/// an article that was renamed or never written. Only quoted phrases introduced by "see" are
/// checked; other quoted text is button labels and values, which are not references.
#[test]
fn cross_references_name_real_topics() {
    // `haystack` lower-cases (it feeds the search), so compare lower-cased titles.
    let titles: Vec<String> = TOPICS.iter().map(|t| t.title.to_lowercase()).collect();
    let mut checked = 0usize;
    for t in TOPICS {
        let text = haystack(t);
        // Scan for the marker itself rather than pairing every quote in the article: the
        // prose quotes UI labels too, and pairing sequentially misaligns on those.
        for marker in ["see \u{201c}"] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(marker) {
                rest = &rest[at + marker.len()..];
                let Some(close) = rest.find('\u{201d}') else {
                    panic!("topic {} has an unterminated cross-reference", t.id)
                };
                let referenced = &rest[..close];
                rest = &rest[close..];
                checked += 1;
                assert!(
                    titles.iter().any(|title| title == referenced),
                    "topic {} points at {referenced:?}, which is not an article title",
                    t.id
                );
            }
        }
    }
    // Guard against the check silently becoming vacuous if the convention changes.
    assert!(checked >= 8, "only {checked} cross-references found — has the “See …” convention changed?");
}

#[test]
fn troubleshooting_covers_the_failure_users_actually_hit() {
    // The read-only surprise is the single most common confusion, so the manual
    // must always answer it somewhere findable.
    let hits = search("read-only");
    assert!(hits.len() >= 2, "read-only must be documented in more than one place");
    assert!(!search("password rejected").is_empty() || !search("passwords are rejected").is_empty());
}

/// Backing up is the one thing the owner of a brand-new vault has to set up BEFORE it holds
/// anything they cannot afford to lose — an offline vault's only copy is the one on this
/// disk. It is therefore filed under Getting started, not left in Settings & maintenance
/// where a first-time reader would not reach it until the vault already mattered.
#[test]
fn backing_up_is_filed_under_getting_started() {
    let t = TOPICS.iter().find(|t| t.id == "backups").expect("the manual has a backup article");
    assert_eq!(t.section, "Getting started", "the backup article belongs in Getting started");
}
