//! Unit tests for the parent module ([`super`], `gui.rs`), split into their own
//! file via `#[cfg(test)] #[path = "gui_glyph_tests.rs"] mod glyph_tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use eframe::egui;
use egui_kittest::Harness;

/// Every non-ASCII character in the GUI's source must exist in the fonts egui
/// BUNDLES — the app ships as a single self-contained binary with no asset files,
/// so it cannot rely on what fonts the target machine happens to have. A character
/// outside the bundled set renders as a tofu box (□) on the user's screen, which no
/// test that merely queries labels would ever notice.
///
/// The character set is taken from the source with `include_str!` rather than
/// hand-listed, so introducing a new glyph automatically brings it under this check
/// instead of quietly shipping. It over-approximates (comments are scanned too),
/// which is the safe direction: a comment-only character that the fonts lack costs
/// one line in the allow-list below, whereas a missed rendered one costs a tofu box.
#[test]
fn every_glyph_in_the_gui_source_exists_in_the_bundled_fonts() {
    const SOURCES: [&str; 2] = [include_str!("gui.rs"), include_str!("gui_help.rs")];

    // Characters that appear only in prose/comments and are never drawn as UI
    // chrome. Listed explicitly so the exemption is a decision, not an accident;
    // each was checked against the source when it was added here.
    const COMMENT_ONLY: &str = "→↑↓⇄□";

    let mut chars: Vec<char> = SOURCES
        .iter()
        .flat_map(|s| s.chars())
        .filter(|c| !c.is_ascii() && !COMMENT_ONLY.contains(*c))
        .collect();
    chars.sort_unstable();
    chars.dedup();

    let mut h = Harness::new_ui(|_ui| {});
    h.run();
    let (missing, control_present, control_absent): (Vec<char>, bool, bool) = h.ctx.fonts_mut(|f| {
        let id = egui::FontId::proportional(14.0);
        (
            chars.iter().copied().filter(|c| !f.has_glyph(&id, *c)).collect(),
            // Controls, so a broken probe cannot pass silently: a plain letter must
            // be present and a Gothic codepoint must not.
            f.has_glyph(&id, 'A'),
            !f.has_glyph(&id, '\u{10348}'),
        )
    });
    assert!(control_present && control_absent, "the glyph probe itself is broken");
    assert!(
        missing.is_empty(),
        "these characters are NOT in egui's bundled fonts and render as tofu boxes: {missing:?}"
    );
}
