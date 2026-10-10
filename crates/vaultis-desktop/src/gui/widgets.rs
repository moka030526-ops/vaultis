//! Free helper widgets shared by the screens and tabs: cards, banners, list panels,
//! form fields, combos, secret fields, and the status/error-banner rules.

use super::*;

/// A framed content card: a subtly filled, rounded, hairlined box used to group a
/// form or a panel so the eye can tell one region from the next.
///
/// Purely presentational — it wraps whatever the caller draws and returns what the
/// closure returned, so wrapping an existing block in a card never changes it.
pub(super) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(12))
        .show(ui, add)
        .inner
}

/// Two equal-width columns whose content CANNOT paint across the divider.
///
/// `ui.columns` places its child panes at fixed x-offsets and does **not** clip them,
/// so on a narrow window a wide field or a long, non-wrapping label in the left pane
/// spills straight over the right pane — two panes visually overlapping. Clipping each
/// child to its own rect confines every pane's drawing to its column; combined with the
/// global `TextWrapMode::Wrap`, content reflows and shrinks with the window instead of
/// colliding. Drop-in replacement for `ui.columns(2, …)`.
pub(super) fn two_col<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut [egui::Ui]) -> R) -> R {
    ui.columns(2, |cols| {
        for col in cols.iter_mut() {
            let bounds = col.max_rect().intersect(col.clip_rect());
            col.set_clip_rect(bounds);
        }
        add(cols)
    })
}

/// How much of the lock screen's discretionary vertical spacing to actually spend, given
/// the height available to it. `1.0` is the designed, comfortable layout; the value tapers
/// toward [`AUTH_SPACE_MIN`] as the window gets shorter.
///
/// This is what lets the lock screen keep its promise of not scrolling. The floor
/// ([`min_inner_size`]) is clamped to the display, so on a short screen — or at 150%
/// interface size, which costs the same content half as much screen again — the window can
/// legitimately be shorter than the comfortable layout wants. Padding is the right thing to
/// give up there: a tighter front door still shows every control, whereas the alternative is
/// a scrollbar over the password fields.
///
/// Only the decorative gaps scale. Widget sizes, text and the card's own margins are left
/// alone, so the screen gets tighter but never smaller or harder to hit.
pub(super) fn auth_space_scale(available_height: f32) -> f32 {
    /// Above this the full, designed spacing is affordable.
    const COMFORTABLE: f32 = 620.0;
    /// At or below this, spacing has given up everything it can. Chosen from the shortest
    /// window a display-clamped floor can actually produce — a 1366×768 laptop at 150%
    /// interface size leaves about 460 points — so the collapse is complete before the
    /// realistic worst case, not exactly at it.
    const CRAMPED: f32 = 500.0;
    if available_height >= COMFORTABLE {
        return 1.0;
    }
    if available_height <= CRAMPED {
        return AUTH_SPACE_MIN;
    }
    // Linear between the two, so resizing the window reads as continuous rather than
    // snapping between a roomy and a cramped layout.
    let t = (available_height - CRAMPED) / (COMFORTABLE - CRAMPED);
    AUTH_SPACE_MIN + t * (1.0 - AUTH_SPACE_MIN)
}

/// The least discretionary spacing the lock screen will collapse to — not zero, because
/// the card, the mode line and the footer still have to read as separate things, but low
/// enough that the tallest variant (Create, both confirm rows) clears a ~460-point window.
pub(super) const AUTH_SPACE_MIN: f32 = 0.12;

/// A small filled pill — used for counts and mode badges, where a number needs to
/// read as a label rather than as body text.
pub(super) fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).small().strong());
        });
}

/// A screen or panel heading in the accent color, with the vertical rhythm the
/// rest of the design system expects.
pub(super) fn section_heading(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(egui::RichText::new(text).heading().color(color));
}

/// The opening words of every "this file is now plaintext on disk" status message.
///
/// It leads the message — rather than trailing the path, as it first did — because the
/// status strip TRUNCATES (`Label::truncate`, see `ui_top_level`): with the caveat last,
/// exporting to any deep path pushed the entire warning off the visible end and left a
/// bare "Exported to /home/…/very/long/pa…" reading as an ordinary success notice. The
/// one part of the sentence that must survive truncation is therefore the first part.
///
/// Doubling as the marker [`is_export_caveat`] matches on keeps the styling and the
/// wording from drifting apart: there is one string, asserted by
/// `export_status_messages_are_flagged_as_caveats`.
pub(super) const EXPORT_CAVEAT_PREFIX: &str = "⚠ UNENCRYPTED";

/// The footer's live "the form holds changes the vault does not" indicator, shown whenever
/// [`GuiApp::has_unsaved_edits`] is true (see the status panel in `ui_top_level`). A named
/// constant so the tests assert on the SAME string the footer draws.
pub(super) const UNSAVED_WARNING: &str = "⚠ unsaved changes — click 💾 Save first";

/// Whether a status message is the plaintext-on-disk caveat raised by an export
/// (`export_doc_to_config_dir` and the tab-CSV path). Drawn in red wherever the status
/// is shown, so it does not read as just another quiet "Saved."-style confirmation.
///
/// Matched on [`EXPORT_CAVEAT_PREFIX`] rather than on "Exported" as it first was: Config's
/// own "Export directory set to …" is a single character away from that prefix, so the
/// check was one reworded message away from painting an unrelated confirmation red.
pub(super) fn is_export_caveat(status: &str) -> bool {
    status.starts_with(EXPORT_CAVEAT_PREFIX)
}

/// The color an alerting status is drawn in: an export caveat (a statement of fact about a
/// file that now sits unencrypted on disk) or a failed/refused action (see
/// [`GuiApp::status_is_alert`]), not the app's usual amber "might go wrong" caution.
///
/// Picked per theme rather than hardcoded. A single mid-red reads at roughly 3.2:1 on the
/// dark palettes — under the 4.5:1 WCAG AA floor for text this small, and the *default*
/// theme (Catppuccin Mocha) is one of them, which would have made the one message that
/// most needs reading the hardest to read. Each variant is chosen to clear 4.5:1 against
/// its own family's backgrounds — enforced by
/// `alert_color_clears_wcag_aa_on_every_theme`, which walks all 16.
///
/// The dark variant is as pale as it is because Zenburn is the binding case: its light-grey
/// panels leave the least room, and one value has to clear every dark palette. Weight
/// (`.strong()`) and the leading ⚠ carry the urgency that saturation would otherwise.
pub(super) fn alert_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(255, 175, 175)
    } else {
        egui::Color32::from_rgb(178, 24, 24)
    }
}

/// Whether a refusal raised in `raised_in` no longer applies because the user has moved to
/// a different screen, tab or record (`now`). `false` when there is no refusal.
pub(super) fn refusal_is_stale(raised_in: Option<&FormContext>, now: &FormContext) -> bool {
    raised_in.is_some_and(|r| r != now)
}

/// Pure lifetime rule for the conspicuous error banner, unit-testable without egui (mirrors
/// the `clipboard_tick_decision` pattern). The banner shows the last hard failure and must
/// disappear as soon as any later status line replaces that text — a success/info message
/// means the failure is no longer current — while staying put as long as the status still
/// equals it. Returns `true` when the stored `error` is stale and should be cleared.
pub(super) fn error_banner_is_stale(error: Option<&str>, status: &str) -> bool {
    error.is_some_and(|e| e != status)
}

/// Render the CONSPICUOUS error banner for a hard failure: a bright red full-width strip at
/// the top of the window with a ⚠ and the failure message, plus a Dismiss button that clears
/// it (`*error = None`). Does nothing when `error` is `None`. Kept a free function (taking
/// just `&mut Option<String>` and `ui`) so a headless `egui_kittest` harness can drive it
/// without constructing an `eframe::Frame`. Far more visible than the weak status line, so a
/// failed save/upload can't be silently overlooked.
pub(super) fn show_error_banner(error: &mut Option<String>, ui: &mut egui::Ui) {
    let Some(msg) = error.clone() else { return };
    egui::Panel::top("error_banner")
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(176, 0, 32))
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show_inside(ui, |ui| {
            // Dismiss is placed first and the MESSAGE yields space, so the button is
            // reachable no matter how long the failure text is. `.wrap()` makes that
            // yielded space actually hold the text: a shrinking side defaults to
            // Extend, so a long failure ran off the window and could only be read by
            // widening it — now the banner grows downward and stays readable.
            egui::containers::Sides::new().shrink_left().wrap().show(
                ui,
                |ui| {
                    ui.label(egui::RichText::new("⚠").color(egui::Color32::WHITE).strong().size(18.0));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&msg).color(egui::Color32::WHITE).strong().size(15.0));
                },
                |ui| {
                    if ui.button("Dismiss ×").clicked() {
                        *error = None;
                    }
                },
            );
        });
}

// `current` is borrowed mutably so the click can change it. `*current` is a
// *dereference*: it reads/writes the value behind the `&mut` reference (compare
// `*current == tab`, assign `*current = tab`).
pub(super) fn tab_button(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str, accent: egui::Color32) {
    let selected = *current == tab;
    // The active tab is bolded and tinted, then underlined with an accent bar drawn
    // just under its rect — the underline is what makes "which tab am I on" readable
    // at a glance across ten differently-colored themes.
    let text = if selected {
        egui::RichText::new(label).strong().color(accent)
    } else {
        egui::RichText::new(label)
    };
    // Added DIRECTLY to `ui` (no `ui.scope`): the caller lays the strip out with
    // `horizontal_wrapped`, and that layout only breaks a row inside `allocate_space`, which
    // needs the item's full width up front. A nested region does not declare a width before
    // its content runs, so wrapping a scope-wrapped button silently never happened — the
    // strip stayed one row and the last tabs ran off the right edge of the window. The
    // one-line-per-tab wrap mode is set once on the strip's Ui by the caller instead.
    let resp = ui.selectable_label(selected, text);
    if selected {
        let r = resp.rect;
        ui.painter().hline(
            r.min.x + 2.0..=r.max.x - 2.0,
            r.max.y + 1.0,
            egui::Stroke::new(2.0_f32, accent),
        );
    }
    if resp.clicked() {
        *current = tab;
    }
}

/// Render the left list panel; return `(new_clicked, selected_index)`.
// `labels: &[(String, String)]` is a borrowed *slice* — a read-only view of a
// contiguous run of `(id, label)` tuples (no ownership taken). `Option<&str>`
// is a maybe-present borrowed string. Returning a tuple lets one call report two
// outcomes at once.
/// Recursive render of one grouped-tree node ([`records::AcctNode`]): child groups (each an
/// expandable `CollapsingHeader`) followed by this node's leaves (shown by label only).
/// Returns the index into `labels` of a clicked leaf, if any. `path` is the stack of ancestor
/// labels; it is hashed AS A SLICE for each header's `id_salt`, which is collision-free
/// (unlike a `/`-joined string, where owner "a/b" would collide with owner "a" + type "b" and
/// share expand state). Shared by the grouped Accounts and Assets views.
// `kind` ("acct" / "asset") prefixes the header id_salt so the Accounts and Assets trees get
// DISTINCT persistent collapse state for a same-named group (e.g. owner "Bob" in both). egui's
// ScrollArea id_salt namespaces only the scroll offset, not child widget ids, so without this
// the two trees would share expand/collapse state.
pub(super) fn render_acct_node(
    ui: &mut egui::Ui,
    node: &records::AcctNode,
    path: &mut Vec<String>,
    cur: Option<&str>,
    labels: &[(String, String)],
    kind: &str,
) -> Option<usize> {
    let mut select = None;
    for child in &node.children {
        path.push(child.label.clone());
        let resp = egui::CollapsingHeader::new(&child.label)
            .id_salt((kind, "group_node", path.as_slice()))
            .show(ui, |ui| render_acct_node(ui, child, path, cur, labels, kind));
        if let Some(s) = resp.body_returned.flatten() {
            select = Some(s);
        }
        path.pop();
    }
    for leaf in &node.leaves {
        let sel = cur == Some(leaf.id.as_str());
        let title = if leaf.title.is_empty() { "(no title)".to_string() } else { leaf.title.clone() };
        if ui.selectable_label(sel, title).clicked() {
            // An index into `labels` (the same filtered set as the tree), matching the
            // flat-list model used by the form.
            select = labels.iter().position(|(id, _)| *id == leaf.id);
        }
    }
    select
}

/// Keyboard-navigation target for a FLAT (non-grouped) record list. Returns `Some(index)`
/// when the user pressed ↑/↓ this frame and `enabled` is set and neither a widget holds
/// keyboard focus NOR a popup is open. Those guards mean typing in an edit-pane field moves
/// the text cursor, and an open Type/Subtype dropdown navigates its own options, rather than
/// moving the list selection (nav runs at the top of the tab, before the dropdowns render,
/// so without the popup guard it would drain the arrow key the open combo needs). `enabled`
/// is false in grouped mode (the tree has its own layout).
///
/// The arrow key is consumed so a focused widget that also reads arrows (e.g. a slider)
/// won't act on the same press too. Note this does NOT suppress egui's cardinal focus
/// navigation (`focus_direction` is captured from RawInput before any UI runs); egui only
/// moves focus directionally when a widget already holds it, so the `focused()` guard is
/// what keeps arrows driving the list here.
pub(super) fn list_nav_target(
    ui: &egui::Ui,
    enabled: bool,
    labels: &[(String, String)],
    current_id: Option<&str>,
) -> Option<usize> {
    if !enabled
        || labels.is_empty()
        || ui.memory(|m| m.focused().is_some())
        || egui::Popup::is_any_open(ui.ctx())
    {
        return None;
    }
    let delta = ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            1isize
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            -1
        } else {
            0
        }
    });
    if delta == 0 {
        return None;
    }
    let here = current_id.and_then(|id| labels.iter().position(|(lid, _)| lid == id));
    Some(stepped_list_index(here, delta, labels.len()))
}

/// Step a flat-list cursor by `delta` (±1), clamped to `[0, len-1]` (the ends don't wrap).
/// With nothing currently selected, ↓ (`delta > 0`) starts at the top and ↑ at the bottom.
///
/// `len == 0` returns 0 rather than panicking. The sole caller ([`list_nav_target`]) does
/// guard on a non-empty list, so that case is unreachable today — but the arithmetic here
/// panics two different ways on an empty list (`clamp(0, -1)` panics because min > max, and
/// `len - 1` underflows), which is a sharp edge to leave lying in a mission-critical app
/// for the next caller to find. Saturating is the honest behaviour: "no rows, so row 0".
pub(super) fn stepped_list_index(current: Option<usize>, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    match current {
        Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        None if delta > 0 => 0,
        None => len - 1,
    }
}

pub(super) fn list_panel(
    ui: &mut egui::Ui,
    title: &str,
    new_label: &str,
    labels: &[(String, String)],
    current_id: Option<&str>,
    writable: bool,
    // When `Some(i)`, scroll so row `i` is visible (set only on the frame the user navigates
    // with the arrow keys, so it never fights manual scrolling).
    scroll_to: Option<usize>,
) -> (bool, Option<usize>, bool) {
    let mut new = false;
    let mut select = None;
    let mut export = false;
    // `apply_style` parks the theme's accent in the selection stroke, so free widgets
    // can pick it up without every call site having to pass it down.
    let accent = ui_accent(ui);
    // Heading, count, then the two actions — all left-to-right and wrapping, so the
    // buttons stay next to the title they belong to. Right-aligning them pushed them
    // against the divider between the panes, where "⬇ CSV" read as part of the form
    // and was easy to miss entirely on a narrow window.
    ui.horizontal_wrapped(|ui| {
        section_heading(ui, title, accent);
        badge(ui, &format!("{}", labels.len()), accent);
        ui.add_space(4.0);
        // "New" is a write; only offered when writable.
        if writable && ui.button(new_label).clicked() {
            new = true;
        }
        // Offered in read-only sessions too. The tooltip carries the warning the old
        // write-mode gate used to enforce: the file is unencrypted and, on Accounts and
        // Real Estate, holds every password in the clear.
        if ui
            .button("⬇ CSV")
            .on_hover_text(
                "Export every row on this tab to a timestamped CSV in the export directory.\n\
                 The file is UNENCRYPTED and includes passwords in plain text.",
            )
            .clicked()
        {
            export = true;
        }
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
    if labels.is_empty() {
        // An empty list previously read as a blank panel, which is indistinguishable
        // from a broken one. Say which it is.
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(if writable {
                "Nothing here yet — click New to add the first record."
            } else {
                "Nothing here (or every record is hidden by a filter)."
            })
            .weak()
            .italics(),
        );
    }
    // `id_salt(title)` because `ui.columns` builds both panes with the same child id, so
    // an unsalted scroller here resolves to ONE id shared by every tab's flat list —
    // scrolling the Instructions list moved the Trust & Will list to the same offset.
    // Every other scroller in this file is salted; this was the one that was not.
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(title).show(ui, |ui| {
        // `.enumerate()` pairs each item with its index `i`; the `(i, (id, label))`
        // pattern destructures the index and the inner tuple together.
        for (i, (id, label)) in labels.iter().enumerate() {
            // `id.as_str()` borrows the `String` as `&str` to compare with the
            // currently-selected id.
            let selected = current_id == Some(id.as_str());
            let resp = ui.selectable_label(selected, label);
            if resp.clicked() {
                select = Some(i);
            }
            if scroll_to == Some(i) {
                resp.scroll_to_me(Some(egui::Align::Center));
            }
        }
    });
    (new, select, export)
}

/// The right-hand pane before anything is selected. A bare one-line label read as
/// a stray sentence; this centers a glyph and the instruction so the empty pane
/// looks deliberate rather than unfinished.
pub(super) fn empty_form_hint(ui: &mut egui::Ui, noun: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("👈").size(28.0).color(ui_accent(ui).gamma_multiply(0.7)));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("Select {noun} from the list")).strong());
        ui.label(egui::RichText::new("…or click New to add one.").weak().small());
    });
}

/// The theme's accent color, recovered from the style. `apply_style` parks it in
/// the selection stroke so free-standing widgets (which have no `GuiApp` to ask)
/// can stay in the palette without threading a color through every call.
pub(super) fn ui_accent(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().selection.stroke.color
}

/// A "View" row toggle drawn inside its own visible border.
///
/// A bare `ui.checkbox` in these rows is a tick box and a word floating in the card with
/// nothing to mark where one option ends and the next begins — on a wide window the two
/// options in the Accounts View row read as a single run of text. The outline gives each
/// option an edge; when the option is ON it also takes the accent color and a tinted fill,
/// so an active view option (a grouped list, revealed passwords) is visible at a glance
/// without reading the labels. The tick box is still there, so the state is never carried
/// by color alone.
pub(super) fn option_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str, hover: &str) -> egui::Response {
    // Read the state BEFORE the checkbox runs: the frame is painted around the widget, so
    // it must be styled from the value the user is currently looking at.
    let on = *value;
    let (stroke, fill) =
        option_toggle_colors(on, ui_accent(ui), ui.visuals().widgets.inactive.fg_stroke.color);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, stroke))
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| ui.checkbox(value, label).on_hover_text(hover))
        .inner
}

/// [`option_toggle`]'s border and fill colors for a given state: `(stroke, fill)`.
///
/// Split out as a pure function so the one decision that matters — that the border is
/// ALWAYS drawn, in both states — is testable without a render harness. An OFF option
/// gets a dimmed neutral outline rather than no outline: the whole point of the border is
/// to bound the control, and a border that appears only when the option is on would leave
/// the unchecked options as bare text again, which is the problem it was added to fix.
pub(super) fn option_toggle_colors(
    on: bool,
    accent: egui::Color32,
    idle: egui::Color32,
) -> (egui::Color32, egui::Color32) {
    if on {
        (accent.gamma_multiply(0.8), accent.gamma_multiply(0.13))
    } else {
        (idle.gamma_multiply(0.5), egui::Color32::TRANSPARENT)
    }
}

/// Save / Delete buttons; returns the chosen action. Renders nothing (and stays
/// `None`) in read-only mode.
pub(super) fn form_buttons(ui: &mut egui::Ui, writable: bool) -> FormAction {
    if !writable {
        return FormAction::None;
    }
    let mut action = FormAction::None;
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        // Save is the primary action, so it is the filled one; delete is destructive,
        // so it is tinted red and sits apart from the button you actually want.
        let accent = ui_accent(ui);
        if ui
            .add(egui::Button::new(egui::RichText::new("💾 Save").strong().color(egui::Color32::WHITE)).fill(accent))
            .on_hover_text("Write this record to the vault")
            .clicked()
        {
            action = FormAction::Save;
        }
        ui.add_space(10.0);
        if ui
            .add(egui::Button::new(egui::RichText::new("🗑 Delete").color(egui::Color32::from_rgb(200, 60, 60))))
            .on_hover_text("Remove this record from the vault")
            .clicked()
        {
            action = FormAction::Delete;
        }
    });
    action
}

/// Give a freshly-cloned secret field 128 bytes of spare capacity so later per-keystroke
/// edits don't reallocate (which frees the old buffer WITHOUT zeroizing, stranding cleartext
/// in freed heap). Calling `String::reserve` directly on the clone would ITSELF reallocate —
/// the clone has capacity == len — committing the very leak it means to prevent. So we move
/// the value into a roomier buffer and zeroize the original. A no-op once headroom exists
/// (e.g. an empty new-record field), so it is cheap to call every frame.
pub(super) fn presize_secret(s: &mut String) {
    if s.capacity() >= s.len() + 128 {
        return;
    }
    let mut roomy = String::with_capacity(s.len() + 128);
    roomy.push_str(s);
    s.zeroize(); // wipe the cloned buffer before it is freed by the move below
    *s = roomy;
}

/// Render a stored value as READ-ONLY text: left-justified, wrapped to the pane, and
/// still selectable so it can be copied.
///
/// The alternative — a disabled text box — gave every value the same full-pane width
/// whatever its length, so a one-word owner name occupied as much screen as an address
/// and a form read as a column of near-empty boxes.
///
/// **The text handed to the label is the stored text, byte for byte.** An earlier version
/// pre-wrapped it and inserted a hyphen at each break, which was wrong in a way that is
/// easy to miss: egui copies a label's GALLEY text, so whatever string is passed here is
/// what Ctrl+C returns. That version also normalised whitespace via `split_whitespace`,
/// so it corrupted values that never wrapped at all — "1234  N Elm Street" (two spaces)
/// was displayed and COPIED as "1234 N Elm Street". A read-only session is the mode an
/// heir is told to use, and the manual promises these fields can be selected and copied;
/// handing them a silently altered account number or address is worse than any layout
/// problem it solved. egui's own wrapping breaks long words without a hyphen but leaves
/// the source string untouched, so a copy is exact.
pub(super) fn read_only_value(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Wrap).selectable(true))
}

/// Treat a designed field width as a MAXIMUM, shrinking it to whatever the pane
/// actually offers.
///
/// The form pane scrolls vertically only, so a field wider than the pane is clipped
/// rather than scrolled to. Wide windows are unaffected (the designed width already
/// fits); narrow ones shrink the field instead of losing its right-hand end. The
/// floor keeps a field from collapsing to nothing.
pub(super) fn fit(ui: &egui::Ui, desired: f32) -> f32 {
    desired.min(ui.available_width() - 8.0).max(90.0)
}

/// Like [`fit`], but for a field that is followed by `buttons` trailing controls on the
/// SAME row (a 📋 copy, a 🎲 generate, …).
///
/// `fit` alone reserves 8 px, which is right for a field that owns its whole row. In a
/// `ui.horizontal` row the field is measured FIRST, so on a narrow pane it takes all the
/// remaining width and the buttons after it are laid out past the pane's right edge —
/// where `two_col`'s clip hides them. That is how the Accounts tab could push its copy,
/// generate and reveal buttons out of the window at any window size: the overflow scaled
/// with the pane instead of disappearing as the window grew.
///
/// The reserve is derived from the theme's own metrics rather than a magic number, so it
/// tracks the interface-size setting: an emoji button is about one `interact_size.y`
/// square plus its horizontal padding, and each needs an `item_spacing.x` gap.
pub(super) fn fit_with_buttons(ui: &egui::Ui, desired: f32, buttons: usize) -> f32 {
    let s = ui.spacing();
    let per_button = s.interact_size.y + s.button_padding.x * 2.0 + s.item_spacing.x;
    let reserve = per_button * buttons as f32;
    (desired).min(ui.available_width() - 8.0 - reserve).max(90.0)
}

/// A single-line text field that is editable when `writable`, and otherwise shown as
/// an **immutable but still selectable/copyable** field. egui edits require a *mutable*
/// `TextBuffer` while selection only needs an interactive widget — so binding a `&str`
/// (an immutable `TextBuffer`) gives a read-only field whose text the user can still
/// highlight and Ctrl+C, exactly what read-only mode wants (vs. `add_enabled(false)`,
/// which greys it out and blocks selection entirely).
pub(super) fn field_singleline(ui: &mut egui::Ui, value: &mut String, writable: bool, width: f32) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::singleline(value).desired_width(fit(ui, width)))
    } else {
        read_only_value(ui, value)
    }
}

/// Like [`field_singleline`], but for a field followed by `buttons` controls on the same
/// row — see [`fit_with_buttons`] for why the plain version pushes them off the pane.
pub(super) fn field_singleline_with_buttons(
    ui: &mut egui::Ui,
    value: &mut String,
    writable: bool,
    width: f32,
    buttons: usize,
) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::singleline(value).desired_width(fit_with_buttons(ui, width, buttons)))
    } else {
        read_only_value(ui, value)
    }
}

/// Like [`field_singleline`] but with a placeholder hint (shown only when editable).
pub(super) fn field_singleline_hint(ui: &mut egui::Ui, value: &mut String, writable: bool, width: f32, hint: &str) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(fit(ui, width)))
    } else {
        read_only_value(ui, value)
    }
}

/// A multi-line field: editable when `writable`, else immutable-but-selectable (see
/// [`field_singleline`]).
pub(super) fn field_multiline(ui: &mut egui::Ui, value: &mut String, writable: bool, rows: usize) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::multiline(value).desired_rows(rows).desired_width(f32::INFINITY))
    } else {
        read_only_value(ui, value)
    }
}

pub(super) fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, writable: bool) {
    ui.label(label);
    field_singleline(ui, value, writable, 420.0);
    ui.end_row();
}

/// A [`text_row`] followed by a 📋 copy button — for the NON-secret fields a user reaches
/// for as often as the password beside them (a portal URL, a username).
///
/// Same rules as the Accounts tab's copy buttons: copying is a *read*, so the button stays
/// live in a read-only session, and it is disabled only when the field is empty (nothing to
/// copy). The value is stashed in `copy_plain` and put on the clipboard after rendering, so
/// the clipboard call sits outside the `self` borrow the form holds.
pub(super) fn text_row_with_copy(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    writable: bool,
    copy_plain: &mut Option<String>,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        field_singleline_with_buttons(ui, value, writable, 380.0, 1);
        if ui.add_enabled(!value.is_empty(), egui::Button::new("📋")).on_hover_text("Copy").clicked() {
            *copy_plain = Some(value.clone());
        }
    });
    ui.end_row();
}

/// Sorted, de-duplicated, non-empty values — used to populate filter dropdowns.
// `impl Iterator<Item = String>` is a generic parameter: accept *any* iterator
// yielding `String`s (the caller decides the concrete type). `.dedup()` removes
// *consecutive* duplicates, which is why it follows `.sort()`.
/// A filter dropdown: "All" (empty value) plus each option.
/// The free-text SEARCH field, drawn as a highlighted pill so it stands out from the filter
/// dropdowns beside it: a magnifier glyph, an accent-outlined rounded frame, and — while a query
/// is active — a tinted fill, a thicker outline and an inline "×" to clear it. It sits in a row
/// of combos that all look alike; the search is the control users reach for first, so it is the
/// one given the visual weight, and an active search is visible without reading the text (an
/// unexplained short list is the most common "where did my records go" confusion).
///
/// Returns the `TextEdit`'s response (so callers can react to `.changed()`) and takes the hover
/// text describing the match rule — the search is sound-alike ([`records::matches_search_soundlike`]),
/// which is worth saying where the user types.
pub(super) fn search_box(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    hover: &str,
    accent: egui::Color32,
    width: f32,
) -> egui::Response {
    let active = !value.trim().is_empty();
    // `gamma_multiply` scales the color's alpha, so both states read correctly on light AND
    // dark themes (a fixed grey would vanish on one of them).
    // Annotated rather than inferred: `stroke_w` reaches `Stroke::new`, whose width is an
    // `impl Into<f32>`, so a bare literal here has no concrete type to latch onto and rustc
    // falls back to `f32` with a future-incompatibility warning (`float_literal_f32_fallback`).
    // Naming the tuple's types fixes both arms at once and says what these actually are.
    let (fill, stroke_w, stroke_a): (egui::Color32, f32, f32) = if active {
        (accent.gamma_multiply(0.14), 2.0, 0.9)
    } else {
        (ui.visuals().extreme_bg_color, 1.0, 0.5)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(stroke_w, accent.gamma_multiply(stroke_a)))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🔍").color(accent).strong());
                // `Frame::NONE` drops the TextEdit's own box: the pill IS the frame, so the
                // two outlines never double up.
                let resp = ui.add(
                    egui::TextEdit::singleline(value)
                        .hint_text(hint)
                        .frame(egui::Frame::NONE)
                        .desired_width(fit(ui, width)),
                );
                if active && ui.small_button("×").on_hover_text("Clear the search").clicked() {
                    value.clear();
                }
                resp
            })
            .inner
        })
        .inner
        .on_hover_text(hover)
}

pub(super) fn filter_combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[String]) {
    let text = if value.is_empty() { "All".to_string() } else { value.clone() };
    egui::ComboBox::from_id_salt(id).selected_text(text).show_ui(ui, |ui| {
        ui.selectable_value(value, String::new(), "All");
        for opt in options {
            ui.selectable_value(value, opt.clone(), opt);
        }
    });
}

/// A dropdown over `options`. Non-interactive (display-only) in read-only mode. The
/// record's CURRENT value is always offered as a choice — even when it is off-list (legacy
/// data, or a type later removed from Config) — so opening the dropdown can never drop it.
pub(super) fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[String], writable: bool) {
    let current = if value.is_empty() { "(choose)".to_string() } else { value.clone() };
    ui.add_enabled_ui(writable, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(current).show_ui(ui, |ui| {
            // Keep an off-list current value selectable, listed first. Compare trimmed +
            // case-insensitively (matching the core's category dedup) so a value differing
            // from a configured entry only by case/whitespace isn't shown as a near-duplicate.
            if !value.is_empty() && !options.iter().any(|o| o.trim().eq_ignore_ascii_case(value.trim())) {
                let cur = value.clone();
                ui.selectable_value(value, cur.clone(), cur);
            }
            for opt in options {
                ui.selectable_value(value, opt.clone(), opt);
            }
        });
    });
}

/// A collapsing, timestamped history view for a record.
// `&[records::Change]` is a read-only slice of change entries.
pub(super) fn history_view(ui: &mut egui::Ui, history: &[records::Change]) {
    ui.add_space(10.0);
    // The entry count sits in the header so it is visible without expanding —
    // "has this record ever been touched" is answerable at a glance.
    let title = if history.is_empty() {
        "🕘 History".to_string()
    } else {
        format!("🕘 History  ({} entr{})", history.len(), if history.len() == 1 { "y" } else { "ies" })
    };
    egui::CollapsingHeader::new(egui::RichText::new(title).strong()).default_open(false).show(ui, |ui| {
        if history.is_empty() {
            ui.label(egui::RichText::new("No changes recorded yet.").weak().italics());
        }
        egui::ScrollArea::vertical().max_height(180.0).id_salt("hist").show(ui, |ui| {
            // `.iter().rev()` walks the entries newest-first (reverse order).
            for c in history.iter().rev() {
                // `display_detail` masks password before/after values so the history
                // pane never leaks a cleartext password (it can't be copied from here
                // and the live field's reveal toggle deliberately does not extend here).
                let detail =
                    if c.detail.is_empty() { c.action.clone() } else { records::display_detail(&c.detail) };
                ui.horizontal_top(|ui| {
                    // A fixed-width monospace timestamp column makes the log scannable
                    // instead of a ragged run of prose.
                    ui.label(egui::RichText::new(format_time(c.at)).monospace().weak().small());
                    ui.label(egui::RichText::new(detail).small());
                });
            }
        });
    });
}

/// Format a unix-seconds timestamp as `YYYY-MM-DD HH:MM:SS UTC` (no date crate).
/// Returns "never" for a zero/negative timestamp. The calendar math lives once in
/// [`crate::records::civil_from_unix`].
pub(super) fn format_time(ts: i64) -> String {
    if ts <= 0 {
        return "never".to_string();
    }
    // Destructure the six returned date/time components into named bindings.
    let (year, mo, d, h, m, s) = records::civil_from_unix(ts);
    // `{year:04}` etc. are format specs: zero-pad to the given width (4 or 2).
    format!("{year:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// A single-line text field for a SECRET (a password), hardening egui's stock
/// `TextEdit` against the two residual leaks the audit flagged (R-7):
///
/// 1. **Undo residue.** egui keeps un-zeroized snapshots of the edited string in its
///    per-widget undo buffer, which would otherwise retain past values of the secret
///    for the whole process lifetime. We clear the undoer every frame (undo on a
///    password is not worth the residue), bounding it to at most the current frame.
/// 2. **Copy hint bypass.** The built-in Ctrl+C / Ctrl+X / context-menu copy queues an
///    `OutputCommand::CopyText` that eframe writes via a plain clipboard `set_text`
///    (no history-exclusion hint), unlike the dedicated 📋 button. While this field is
///    focused we intercept that command and re-route the secret through the hardened
///    [`crate::copy_secret_to_clipboard`] (Linux `exclude_from_history`).
///
/// `id_salt` MUST be unique per field (it pins a stable widget id for the state-scrub).
pub(super) fn secret_text_edit(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut String,
    revealed: bool,
    writable: bool,
    // The FINAL width, already fitted by the caller — `fit` for a field that owns its
    // row, `fit_with_buttons` when controls follow it on the same row. Passing it in
    // rather than fitting here keeps the caller's row layout in one place (and this
    // function within clippy's argument budget).
    width: f32,
    copied_out: &mut Option<Zeroizing<String>>,
) -> egui::Response {
    let id = ui.make_persistent_id(id_salt);
    // Read-only: bind a `&str` (immutable TextBuffer) so the field stays selectable and
    // copyable (incl. the hardened Ctrl+C reroute below) but cannot be edited; writable
    // binds the real `&mut String`.
    let resp = if writable {
        ui.add(egui::TextEdit::singleline(value).id(id).password(!revealed).desired_width(width))
    } else {
        let mut ro = value.as_str();
        ui.add(egui::TextEdit::singleline(&mut ro).id(id).password(!revealed).desired_width(width))
    };
    // (1) Never accumulate undo snapshots of a secret.
    if let Some(mut state) = egui::widgets::text_edit::TextEditState::load(ui.ctx(), id) {
        state.clear_undoer();
        state.store(ui.ctx(), id);
    }
    // (2) Re-route any built-in copy/cut of THIS focused field through the hardened
    // clipboard path. Gating on focus means we only touch a CopyText that this field
    // produced (you cannot have two focused widgets), so other widgets' copies are
    // untouched.
    if resp.has_focus() {
        let mut copied: Vec<String> = ui.ctx().output_mut(|o| {
            // MOVE the secret out of each CopyText command (leaving an empty String) rather
            // than cloning it: a `retain` that cloned then returned false would DROP the
            // command's original String — the cleartext password egui staged for the
            // clipboard — without zeroizing it, stranding it in freed heap. mem::take leaves
            // an empty String behind, which the retain below then drops harmlessly.
            let mut taken = Vec::new();
            for c in o.commands.iter_mut() {
                if let egui::OutputCommand::CopyText(t) = c {
                    taken.push(std::mem::take(t));
                }
            }
            // Remove the (now-emptied) CopyText commands so eframe's plain set_text never runs.
            o.commands.retain(|c| !matches!(c, egui::OutputCommand::CopyText(_)));
            taken
        });
        // Surface the intercepted secret to the caller so it routes through the app's
        // `copy_to_clipboard`, which applies the hardened (history-excluded) copy AND
        // arms the 15s auto-clear + on-exit wipe. Doing the hardened copy directly here
        // (as before) skipped that arming, leaving a Ctrl+C/cut'd password on the
        // clipboard indefinitely (audit B-1). There is at most one focused field, so
        // at most one CopyText; take it and zeroize any stray extras.
        if let Some(t) = copied.pop() {
            *copied_out = Some(Zeroizing::new(t));
        }
        for mut leftover in copied {
            leftover.zeroize();
        }
    }
    resp
}

/// A masked single-line password field; returns true if Enter was pressed. `id_salt`
/// is unique per field (unlock/create/change-password use four distinct fields).
pub(super) fn password_field(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut String,
    copied_out: &mut Option<Zeroizing<String>>,
    hover: Option<&str>,
) -> bool {
    // Always masked (revealed = false); the secret hardening (undo scrub + copy
    // re-route) still applies — a master password is the most sensitive of all.
    // Always editable (`writable = true`): this is the unlock/create field, which
    // exists before any vault is open, so the read-only mode does not apply here.
    // `copied_out` surfaces a built-in Ctrl+C of the master password so the caller
    // arms the auto-clear (otherwise it would linger on the clipboard).
    let mut resp = secret_text_edit(ui, id_salt, value, false, true, fit(ui, 280.0), copied_out);
    if let Some(hover) = hover {
        resp = resp.on_hover_text(hover);
    }
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Build `(id, label)` pairs for a record list.
// `<R: Record>` is a generic: this works for any type `R` that implements the
// `Record` trait (i.e. exposes `.id()` and `.label()`). `&[R]` is a slice of
// such records. `.to_string()` makes an owned `String` from the borrowed id.
pub(super) fn label_list<R: Record>(list: &[R]) -> Vec<(String, String)> {
    list.iter().map(|r| (r.id().to_string(), r.label())).collect()
}

/// Copy the record `buf` is editing back out of `saved` (the vault's own list), replacing
/// the buffer with the stored copy. The per-tab dispatcher is [`GuiApp::sync_edit_buffer`],
/// which documents when this may be called and why it must be.
///
/// A no-op if the buffer is empty or its id is not in `saved` — a record deleted from
/// under the form leaves the buffer as the user's only remaining copy of that editing
/// session, so it is never dropped on the floor here.
pub(super) fn sync_from_saved<R: Record>(buf: &mut Option<R>, saved: &[R]) {
    let Some(cur) = buf.as_ref() else { return };
    let Some(stored) = saved.iter().find(|s| s.id() == cur.id()) else { return };
    // Assigning drops the old buffer, which zeroizes the secrets it held.
    *buf = Some(stored.clone());
}

/// Best-effort clearing of the system clipboard on exit.
pub(super) fn clear_clipboard() {
    // `let _ = ...` ignores the `Result`: if the clipboard is unavailable there
    // is nothing useful to do. Setting it to an empty `String` overwrites any
    // copied secret.
    let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(String::new()));
}
