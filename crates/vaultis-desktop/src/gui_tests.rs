//! Unit tests for the parent module ([`super`], `gui.rs`), split into their own
//! file via `#[cfg(test)] #[path = "gui_tests.rs"] mod tests;` so the tests do not sit
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
use crate::crypto::KdfParams;
use crate::launch;
use crate::records::AssetLiability;
use std::time::{SystemTime, UNIX_EPOCH};

fn fast() -> KdfParams {
    KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }
}

/// `open_sample_vault` points the start page at the given directory, fills in the
/// build script's two demo passwords, and opens it in one call — the button just
/// needs to find `self.sample_vault` and forward it here.
#[test]
fn open_sample_vault_fills_fields_and_opens_with_demo_passwords() {
    let path = tmp("sample-target");
    OpenVault::create(path.clone(), launch::SAMPLE_PW1.as_bytes(), launch::SAMPLE_PW2.as_bytes(), fast())
        .unwrap();
    let dir = path.parent().unwrap().to_path_buf();

    let mut app = GuiApp::new(tmp("sample-launch"), true);
    app.open_sample_vault(dir.clone());

    assert!(app.vault.is_some(), "the sample vault opens");
    assert_eq!(app.vault_dir, dir.display().to_string());
    assert_eq!(app.screen, Screen::Main, "lands past the lock screen");
    // Same discipline as any other successful unlock: the typed passwords are
    // wiped from memory once they are no longer needed, sample or not.
    assert!(app.pw1.is_empty(), "password 1 is wiped after a successful open");
    assert!(app.pw2.is_empty(), "password 2 is wiped after a successful open");
    cleanup(&path);
}

/// A read-only session cannot create a vault — `submit_open_or_create` refuses — so the
/// lock screen must not present creating one. Pointing View mode at a folder with no
/// vault used to render the full create form: a "Create vault" heading and button, the
/// "choose two passwords" instruction, and BOTH confirmation fields, none of which could
/// lead anywhere. The heir handed the View shortcut is exactly the person least able to
/// tell that apart from a real setup step.
#[test]
fn read_only_lock_screen_never_offers_to_create_a_vault() {
    use egui_kittest::{kittest::Queryable, Harness};

    // A path with no vault at it: `auth_mode` is Create in both sessions below.
    let path = tmp("ro-nocreate").parent().unwrap().join("nothing-here").join("vault.pmv");

    for (writable, label) in [(false, "read-only"), (true, "writable")] {
        let app = std::cell::RefCell::new(GuiApp::new(path.clone(), writable));
        assert_eq!(app.borrow().auth_mode, AuthMode::Create, "{label}: nothing exists at the target");
        let mut h = Harness::builder()
            .with_size(egui::vec2(760.0, 700.0))
            .with_max_steps(64)
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.try_run().expect("lock screen settles");

        // `writable` decides every create affordance, together: heading, primary button,
        // and the two confirmation fields (which exist to catch a typo in a password
        // being SET, and nothing is being set here).
        let creates = h.query_all_by_label("Create vault").count();
        let unlocks = h.query_all_by_label("🔓 Unlock").count();
        let confirms = h.query_all_by_label("Confirm password 1").count()
            + h.query_all_by_label("Confirm password 2").count();

        if writable {
            assert!(creates > 0, "{label}: creating IS on offer, so it is shown");
            assert_eq!(confirms, 2, "{label}: both confirmation fields are shown");
        } else {
            assert_eq!(creates, 0, "{label}: nothing may offer to create a vault");
            assert_eq!(unlocks, 1, "{label}: the screen reads as the way in to a vault");
            assert_eq!(confirms, 0, "{label}: no confirmation fields for a password never set");
        }

        // Unchanged either way: both password fields stay live, so an heir who landed on
        // the wrong folder can still type their way into the real vault.
        assert_eq!(h.query_all_by_label("Password 1").count(), 1, "{label}: password 1 stays");
        assert_eq!(h.query_all_by_label("Password 2").count(), 1, "{label}: password 2 stays");
    }
}

/// The lock screen only offers the "Sample vault" button when a sample vault was
/// actually found (`self.sample_vault`) — never a button that fails on click.
#[test]
fn sample_vault_button_shown_only_when_one_was_found() {
    use egui_kittest::{kittest::Queryable, Harness};

    let path = tmp("sample-button");
    let app = std::cell::RefCell::new(GuiApp::new(path.clone(), false));
    let mut h = Harness::builder()
        .with_size(egui::vec2(760.0, 700.0))
        .with_max_steps(64)
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.try_run().expect("lock screen settles");
    assert_eq!(
        h.query_all_by_label("Sample vault").count(),
        0,
        "no button when no sample vault was found"
    );

    app.borrow_mut().sample_vault = Some(path.parent().unwrap().to_path_buf());
    h.try_run().expect("lock screen settles again");
    assert_eq!(
        h.query_all_by_label("Sample vault").count(),
        1,
        "the button appears once a sample vault is found"
    );
    cleanup(&path);
}

/// `self.sample_vault` is resolved ONCE in the constructor, so the directory can be gone
/// by the time the button is clicked (it lives under `target/`; a `cargo clean` in another
/// terminal removes it). Opening the sample vault must then FAIL, not fall through to
/// `AuthMode::Create` and silently build a real vault locked with the two publicly-known
/// demo passwords at a path the user believed already held a throwaway sample.
#[test]
fn open_sample_vault_refuses_to_create_a_vault_that_is_no_longer_there() {
    let path = tmp("sample-vanished");
    let dir = path.parent().unwrap().to_path_buf();
    // The directory `sample_vault` pointed at exists but holds no vault (the same state
    // `cargo clean` leaves, and the state a lossy non-UTF-8 name would resolve to).
    assert!(!path.exists(), "no vault.pmv at the sample location");

    let mut app = GuiApp::new(tmp("sample-vanished-launch"), true); // --write: could create
    app.open_sample_vault(dir.clone());

    assert!(app.vault.is_none(), "nothing is opened");
    assert!(!path.exists(), "and crucially NOTHING was created with the demo passwords");
    assert!(app.auth_error.is_some(), "the user is told why");
    assert!(app.sample_vault.is_none(), "the dead button stops being offered");
    cleanup(&path);
}

/// Both export paths must produce a status the styling recognises, and the caveat must
/// LEAD it: the status strip truncates, so a caveat placed after an arbitrarily long
/// export path would be the first thing cut off — leaving a bare "Exported to …" reading
/// as an ordinary success notice. Pins the wording and the marker together.
#[test]
fn export_status_messages_are_flagged_as_caveats() {
    let (mut app, path) = app_unlocked("export-caveat");
    let outdir = export_out(&path, "csv");
    {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut ov.vault.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.export_dir = outdir.to_string_lossy().into();
    app.export_current_tab_csv();
    assert!(is_export_caveat(&app.status), "CSV export is styled as a caveat: {}", app.status);
    assert!(app.status.starts_with(EXPORT_CAVEAT_PREFIX), "the caveat leads: {}", app.status);

    // Config's own confirmation is one character away from the old "Exported" prefix and
    // must NOT be painted red.
    assert!(
        !is_export_caveat("Export directory set to /tmp/x (used by every Export button)."),
        "an ordinary Config confirmation is not an export caveat"
    );
    assert!(!is_export_caveat("Saved."));
    cleanup(&path);
}

/// The caveat has to be legible in BOTH palette families. A single mid-red sat at ~3.2:1
/// on the dark themes — under the 4.5:1 WCAG AA floor for text this small, and the
/// default theme (Catppuccin Mocha) is one of them.
#[test]
fn export_caveat_color_clears_wcag_aa_on_every_theme() {
    // WCAG relative luminance, then the standard contrast ratio.
    fn lum(c: egui::Color32) -> f64 {
        let f = |v: u8| {
            let s = v as f64 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }
    fn contrast(a: egui::Color32, b: egui::Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    for t in Theme::ALL {
        let v = visuals_for(t);
        let fg = export_caveat_color(&v);
        // Both surfaces the status line is ever drawn on.
        for (what, bg) in [("panel", v.panel_fill), ("faint", v.faint_bg_color)] {
            let ratio = contrast(fg, bg);
            assert!(
                ratio >= 4.5,
                "{} on the {what} background contrasts at only {ratio:.2}:1 — under WCAG AA \
                 for small text, and this is the one message that most needs reading",
                t.label()
            );
        }
    }
}

/// `ui_auth`/`ui_auth_inner` render BOTH the lock screen and, reached via 🔑
/// Passwords, the in-vault "Change master passwords" screen (`AuthMode::ChangePassword`,
/// see the `auth_mode != ChangePassword` gate a few lines above this one on the
/// vault-root/name picker). The Sample vault button must not leak into that second
/// screen: clicking it there would abandon whatever new passwords the user had already
/// typed and instead open a completely unrelated vault.
#[test]
fn sample_vault_button_is_absent_during_change_password() {
    use egui_kittest::{kittest::Queryable, Harness};

    let (mut app, path) = app_unlocked("sample-vs-changepw");
    app.sample_vault = Some(path.parent().unwrap().to_path_buf());
    app.auth_mode = AuthMode::ChangePassword;
    app.screen = Screen::Auth;
    let app = std::cell::RefCell::new(app);
    let mut h = Harness::builder()
        .with_size(egui::vec2(760.0, 700.0))
        .with_max_steps(64)
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.try_run().expect("change-password screen settles");

    assert_eq!(
        h.query_all_by_label("Sample vault").count(),
        0,
        "Sample vault must not appear while changing the master passwords"
    );
    cleanup(&path);
}

/// Preferences live in `<vault_root>/prefs.json`, but the root is chosen ON the lock
/// screen — so at boot there is usually no root and the built-in defaults apply. Pointing
/// at a root that carries its own look must adopt it immediately, not at the next launch.
///
/// The second half matters just as much: merely BROWSING to a folder must not write a
/// `prefs.json` into it. The file appears only when a setting is deliberately changed.
#[test]
fn adopting_a_root_applies_its_look_without_creating_a_prefs_file() {
    let path = tmp("adopt-prefs");
    let root = path.parent().unwrap().to_path_buf();
    std::fs::write(
        root.join("prefs.json"),
        br#"{"theme":"nord","ui_scale":"large","font":"monospace","group_assets_default":true}"#,
    )
    .unwrap();

    let mut app = GuiApp::new(tmp("adopt-prefs-launch"), false);
    // A launch elsewhere: the built-in defaults are in force.
    assert_eq!(app.theme, Theme::Light, "starts on the default theme");

    app.vault_root = root.display().to_string();
    // A bare egui context is all `adopt_root_prefs` needs (it only sets visuals/zoom/fonts).
    let ctx = egui::Context::default();
    app.adopt_root_prefs(&ctx);

    assert_eq!(app.theme, Theme::Nord, "the root's theme is adopted");
    assert_eq!(app.ui_scale, UiScale::Large, "...and its interface size");
    assert_eq!(app.font, FontChoice::Monospace, "...and its typeface");
    assert!(app.group_assets_default && app.asset_grouped, "...and its view defaults, applied live");
    // `applied_*` must match so `render`'s changed-since-applied path does NOT fire and
    // write the file back out.
    assert_eq!(app.applied_theme, app.theme, "adoption must not look like a user change");
    assert_eq!(app.applied_ui_scale, app.ui_scale);
    assert_eq!(app.applied_font, app.font);

    // Browsing to a root with NO prefs.json falls back to the defaults and creates nothing.
    let bare = root.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    app.vault_root = bare.display().to_string();
    app.adopt_root_prefs(&ctx);
    assert_eq!(app.theme, Theme::Light, "no prefs.json -> built-in default");
    assert!(!bare.join("prefs.json").exists(), "browsing a folder must not create prefs.json in it");

    cleanup(&path);
}

/// The lock-screen Help link exists, and — the part that matters — Back returns to
/// the LOCK SCREEN, not to the in-vault UI.
///
/// `ui_help` used to hardcode `Screen::Main` on Back, which was right when the top
/// bar was the only way in. Reached from the lock screen that would have dropped the
/// user into the vault UI **with no vault open**, so this pins the `help_return`
/// routing in both directions.
#[test]
fn lock_screen_help_opens_and_returns_to_the_lock_screen() {
    use egui_kittest::{kittest::Queryable, Harness};

    let path = tmp("authhelp");
    let app = std::cell::RefCell::new(GuiApp::new(path.clone(), false));
    let mut h = Harness::builder()
        .with_size(egui::vec2(760.0, 700.0))
        .with_max_steps(64)
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.try_run().expect("lock screen settles");

    assert_eq!(app.borrow().screen, Screen::Auth, "starts locked");
    assert_eq!(
        h.query_all_by_label("❓  Read the guide").count(),
        1,
        "the lock screen offers the manual"
    );

    h.get_by_label("❓  Read the guide").click();
    h.try_run().expect("help settles");
    assert_eq!(app.borrow().screen, Screen::Help, "the link opens the manual");
    assert_eq!(app.borrow().help_return, Screen::Auth, "…remembering where it came from");
    assert!(app.borrow().vault.is_none(), "no vault was opened along the way");

    h.get_by_label("⬅ Back").click();
    h.try_run().expect("returns and settles");
    assert_eq!(
        app.borrow().screen,
        Screen::Auth,
        "Back returns to the LOCK screen, not to the vault UI"
    );
    cleanup(&path);
}

/// Opening the manual from the lock screen WIPES any partly-typed master passwords.
/// Reading a guide is an open-ended pause; two plaintext passwords must not sit in
/// the buffers for its duration.
#[test]
fn opening_help_from_the_lock_screen_wipes_typed_passwords() {
    use egui_kittest::{kittest::Queryable, Harness};

    let path = tmp("authwipe");
    let mut app = GuiApp::new(path.clone(), false);
    app.pw1 = "correct horse battery".into();
    app.pw2 = "staple cathedral".into();
    let app = std::cell::RefCell::new(app);

    let mut h = Harness::builder()
        .with_size(egui::vec2(760.0, 700.0))
        .with_max_steps(64)
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.try_run().expect("settles");

    h.get_by_label("❓  Read the guide").click();
    h.try_run().expect("settles");

    assert!(app.borrow().pw1.is_empty(), "password 1 was wiped before leaving for Help");
    assert!(app.borrow().pw2.is_empty(), "password 2 was wiped before leaving for Help");
    cleanup(&path);
}

/// The lock screen must remain fully usable at the window's own minimum size and at
/// every interface scale — the footer added height to a screen whose floor predates
/// it, and the scale setting can stretch it by 1.5×.
///
/// "Usable" means the two things you cannot do without: the Unlock button and the
/// Help link must both still be laid out and reachable.
#[test]
fn lock_screen_stays_reachable_at_min_size_and_every_scale() {
    use egui_kittest::{kittest::Queryable, Harness};

    for scale in UiScale::ALL {
        let f = scale.factor();
        let path = tmp("authscale");
        // No vault.pmv exists at this fresh path, so the screen is in Create mode. The
        // session must also be WRITABLE for that to be the TALLEST variant: the two
        // extra confirm rows are shown only when creating is actually on offer, so a
        // read-only Create screen is the shortest one, not the worst case. The floor was
        // chosen for the tallest, so that is the one to squeeze.
        let app = std::cell::RefCell::new(GuiApp::new(path.clone(), true));
        assert_eq!(app.borrow().auth_mode, AuthMode::Create, "tallest lock-screen variant");

        // The real window cannot go below this, so it is the worst case that can
        // actually occur — sized exactly as `apply_ui_scale` scales the floor, and
        // ZOOMED to match. Scaling the window without zooming the content (or vice
        // versa) would test a configuration that cannot happen.
        let mut h = Harness::builder()
            .with_size(egui::vec2(MIN_INNER_SIZE[0] * f, MIN_INNER_SIZE[1] * f))
            .with_max_steps(64)
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.ctx.set_zoom_factor(f);
        h.try_run().unwrap_or_else(|e| panic!("lock screen never settled at {scale:?}: {e}"));

        assert_eq!(
            h.query_all_by_label("❓  Read the guide").count(),
            1,
            "the Help link stays reachable at {scale:?}"
        );
        // `>= 1`, not `== 1`: "Create vault" is both the heading and the button, so
        // an exact count pins the wording of the screen rather than its reachability,
        // which is what this test is about.
        assert!(
            h.query_all_by_label("Create vault").count() >= 1,
            "the primary action stays reachable at {scale:?}"
        );
        cleanup(&path);
    }
}

/// REGRESSION: the floor must not scale itself with the interface size.
///
/// `ViewportCommand::MinInnerSize` is turned into physical pixels by egui-winit
/// multiplying by `zoom_factor * native_pixels_per_point`, so a floor that pre-multiplied
/// by the zoom applied it TWICE — squaring to 2025×1507 at 150%, larger than a 1080p
/// display. The window manager then capped the window below the minimum it had just been
/// given, and the lock screen (laid out for the floor it was promised) overflowed into
/// exactly the scrollbar the floor exists to prevent.
#[test]
fn min_inner_size_is_not_pre_scaled_by_the_interface_size() {
    // A display with room to spare, so nothing here is the clamp's doing.
    let roomy = Some(egui::vec2(3840.0, 2160.0));
    assert_eq!(min_inner_size(roomy), egui::vec2(MIN_INNER_SIZE[0], MIN_INNER_SIZE[1]));
    // Unknown monitor (before the first frame) keeps the same designed floor.
    assert_eq!(min_inner_size(None), egui::vec2(MIN_INNER_SIZE[0], MIN_INNER_SIZE[1]));
}

/// A minimum size larger than the screen is not a minimum — it is a window that cannot be
/// placed. The floor therefore yields to the display, with room left over for the title
/// bar and a taskbar.
#[test]
fn min_inner_size_never_exceeds_the_display() {
    // A short display: the height is clamped, the width is not.
    let short = min_inner_size(Some(egui::vec2(1920.0, 720.0)));
    assert_eq!(short.x, MIN_INNER_SIZE[0], "a wide display does not touch the width floor");
    assert!(short.y < MIN_INNER_SIZE[1], "the height floor yields to a short display");
    assert!(short.y <= 720.0 * MONITOR_FIT + 0.01, "and leaves room for the chrome around it");

    // A small display clamps both axes.
    let small = min_inner_size(Some(egui::vec2(1024.0, 600.0)));
    assert!(small.x <= 1024.0 * MONITOR_FIT + 0.01 && small.y <= 600.0 * MONITOR_FIT + 0.01);

    // A degenerate monitor size (reported by some backends before the window is mapped)
    // must not clamp the floor to nothing.
    assert_eq!(min_inner_size(Some(egui::vec2(0.0, 0.0))), egui::vec2(MIN_INNER_SIZE[0], MIN_INNER_SIZE[1]));
}

/// The lock screen's padding is what gives way on a short window, and it does so
/// continuously and within bounds — never negative, never larger than designed.
#[test]
fn auth_space_scale_tapers_between_comfortable_and_cramped() {
    assert_eq!(auth_space_scale(1000.0), 1.0, "a tall window gets the designed layout");
    assert_eq!(auth_space_scale(620.0), 1.0, "…right down to the comfortable height");
    assert_eq!(auth_space_scale(300.0), AUTH_SPACE_MIN, "a tiny window collapses to the floor");
    assert_eq!(auth_space_scale(0.0), AUTH_SPACE_MIN, "and cannot go below it");

    // Monotonic in between, so resizing reads as continuous rather than snapping.
    let mid = auth_space_scale(520.0);
    assert!(AUTH_SPACE_MIN < mid && mid < 1.0, "tapers rather than switching, got {mid}");
    assert!(auth_space_scale(450.0) < mid && mid < auth_space_scale(600.0), "monotonic");
}

/// THE guarantee the user actually sees: the lock screen does not scroll.
///
/// The Help link is the LAST thing on the screen, so if its bottom edge is inside the
/// window then nothing is below the fold and no scrollbar is drawn. Checked at the real
/// floor and at a window deliberately shorter than the comfortable layout — the case a
/// display-clamped floor makes reachable — where the padding must give way instead.
#[test]
fn lock_screen_fits_without_scrolling_even_when_short() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    // The last of these is the shortest window a display-clamped floor realistically
    // produces (a 1366×768 laptop at 150% interface size).
    for h_px in [MIN_INNER_SIZE[1], 560.0, 470.0, 460.0] {
        let path = tmp("authfit");
        // Create mode again: the tallest variant, with both confirm rows.
        let app = std::cell::RefCell::new(GuiApp::new(path.clone(), false));
        assert_eq!(app.borrow().auth_mode, AuthMode::Create, "tallest lock-screen variant");

        let mut h = Harness::builder()
            .with_size(egui::vec2(MIN_INNER_SIZE[0], h_px))
            .with_max_steps(64)
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.try_run().unwrap_or_else(|e| panic!("lock screen never settled at height {h_px}: {e}"));

        let bottom = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().label().as_deref() == Some("❓  Read the guide"))
            .filter_map(|n| n.accesskit_node().bounding_box())
            .map(|bb| bb.y1 as f32)
            .next()
            .unwrap_or_else(|| panic!("the Help link did not lay out at height {h_px}"));

        assert!(
            bottom <= h_px + 1.0,
            "the lock screen runs past the bottom at height {h_px} (last element ends at \
             {bottom:.0}) — it must tighten its padding, not scroll"
        );
        cleanup(&path);
    }
}

/// SELF-CONTAINMENT: every typeface must come from inside the binary.
///
/// `apply_fonts` only reorders the families that `FontDefinitions::default()` already
/// populates from fonts compiled into epaint, so the set of font DATA it produces must
/// be byte-identical to the default for every choice. If someone later adds a face by
/// reading a file from the machine, this fails — which is the point: the program must
/// render identically on a computer with no fonts installed, and must never hand
/// bytes from outside the binary to a font rasterizer.
#[test]
fn every_typeface_is_compiled_in_and_reads_nothing_from_disk() {
    let defaults = egui::FontDefinitions::default();

    for choice in FontChoice::ALL {
        let defs = font_definitions(choice);

        // THE self-containment assertion: the font DATA must be exactly what epaint
        // ships. Any face loaded from this computer would appear here as an extra
        // key, so adding one later fails this test rather than silently making the
        // program depend on the machine's font set.
        assert_eq!(
            defs.font_data.keys().collect::<Vec<_>>(),
            defaults.font_data.keys().collect::<Vec<_>>(),
            "{choice:?} introduced font data that is not compiled into the binary"
        );

        // The chosen face is one of those bundled names.
        let name = match choice {
            FontChoice::Default => "Ubuntu-Light",
            FontChoice::Monospace => "Hack",
        };
        assert!(
            defaults.font_data.contains_key(name),
            "{choice:?} uses `{name}`, which must be a BUNDLED font, not a system one"
        );
        assert_eq!(choice, FontChoice::from_id(choice.id()).unwrap(), "id round-trips");
        assert!(!choice.label().is_empty());
    }
    assert_eq!(FontChoice::from_id("nonsense"), None);
}

/// Monospace must actually take effect — i.e. be promoted ahead of the default
/// proportional face — otherwise the menu entry would be a no-op.
#[test]
fn monospace_choice_is_promoted_for_body_text() {
    let mut defs = egui::FontDefinitions::default();
    let before = defs.families.get(&egui::FontFamily::Proportional).unwrap().clone();
    assert_ne!(before.first().map(String::as_str), Some("Hack"), "not the default already");

    // Mirror what apply_fonts does, so the assertion is about the CHOICE, not the ctx.
    defs.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "Hack".to_owned());
    let after = defs.families.get(&egui::FontFamily::Proportional).unwrap();
    assert_eq!(after.first().map(String::as_str), Some("Hack"), "Hack leads for body text");
    // The bundled fallbacks stay BEHIND it, so a glyph Hack lacks still renders.
    assert!(after.len() > 1, "fallback chain preserved: {after:?}");
    for f in before {
        assert!(after.contains(&f), "default fallback `{f}` was not dropped");
    }
}

/// Every theme is distinct and complete: unique ids, unique labels, and an accent
/// that is actually readable against that theme's own panel background.
///
/// The contrast check is the useful half — a new palette added by copying a
/// neighbouring arm and forgetting the accent would otherwise ship an invisible
/// focus ring and hyperlink colour.
#[test]
fn every_theme_is_distinct_and_its_accent_is_visible() {
    use std::collections::HashSet;
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();

    // Relative luminance (WCAG), used only to reject an accent that vanishes.
    fn luma(c: egui::Color32) -> f32 {
        let f = |v: u8| {
            let s = v as f32 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    for t in Theme::ALL {
        assert!(ids.insert(t.id()), "duplicate theme id: {}", t.id());
        assert!(labels.insert(t.label()), "duplicate theme label: {}", t.label());
        assert_eq!(Theme::from_id(t.id()), Some(t), "{t:?} round-trips through its id");

        let v = visuals_for(t);
        let a = accent(t);
        let bg = v.panel_fill;
        let (l1, l2) = (luma(a), luma(bg));
        let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        let ratio = (hi + 0.05) / (lo + 0.05);
        assert!(
            ratio >= 2.0,
            "{t:?}: accent {a:?} is nearly invisible on its panel {bg:?} (contrast {ratio:.2})"
        );
    }
    assert_eq!(ids.len(), Theme::ALL.len());
}

/// The window icon must actually DECODE. `with_icon(window_icon().unwrap_or_default())`
/// fails silently by design — a broken asset just yields the platform default — so
/// without this test, regenerating the PNG in a non-RGBA8 format (or moving it) would
/// quietly drop the icon and nobody would notice until they looked at the taskbar.
#[test]
fn window_icon_decodes_to_rgba8() {
    let icon = window_icon().expect("the committed icon PNG must decode");
    assert_eq!(icon.width, 512, "expected the 512px source");
    assert_eq!(icon.height, 512);
    assert_eq!(
        icon.rgba.len(),
        (icon.width * icon.height * 4) as usize,
        "RGBA8 = 4 bytes per pixel"
    );
    // Real artwork, not a fully transparent square.
    assert!(icon.rgba.chunks_exact(4).any(|px| px[3] > 0), "icon has visible pixels");
}

/// Every UiScale round-trips through its prefs id, and the zoom factors are sane and
/// strictly increasing — a duplicate or inverted factor would make two menu entries
/// behave identically, or backwards.
#[test]
fn ui_scale_ids_round_trip_and_factors_increase() {
    let mut prev = 0.0_f32;
    for s in UiScale::ALL {
        assert_eq!(UiScale::from_id(s.id()), Some(s), "{s:?} round-trips");
        assert!(!s.label().is_empty());
        let f = s.factor();
        assert!(f > prev, "factors strictly increase: {s:?} = {f} after {prev}");
        assert!((0.5..=2.0).contains(&f), "{s:?} factor {f} is in a usable range");
        prev = f;
    }
    assert_eq!(UiScale::default().factor(), 1.0, "the default must not rescale");
    assert_eq!(UiScale::from_id("nonsense"), None);
}

/// The scale preference persists next to the theme WITHOUT clobbering it — both live
/// in the same prefs.json, so a naive write of one would drop the other.
#[test]
fn saving_ui_scale_preserves_the_theme_and_vice_versa() {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("vaultis-prefs-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prefs.json");

    save_theme_to(&path, Theme::Nord);
    save_ui_scale_to(&path, UiScale::Larger);
    assert_eq!(load_theme_from(&path), Theme::Nord, "theme survived the scale write");
    assert_eq!(load_ui_scale_from(&path), UiScale::Larger);

    save_theme_to(&path, Theme::Dracula);
    assert_eq!(load_ui_scale_from(&path), UiScale::Larger, "scale survived the theme write");
    assert_eq!(load_theme_from(&path), Theme::Dracula);

    // A missing file falls back to the default rather than failing startup.
    assert_eq!(load_ui_scale_from(&dir.join("nope.json")), UiScale::default());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stepped_list_index_clamps_and_seeds_from_empty_selection() {
    // From a known position, ±1 with clamped ends (no wrap).
    assert_eq!(stepped_list_index(Some(2), 1, 5), 3);
    assert_eq!(stepped_list_index(Some(2), -1, 5), 1);
    assert_eq!(stepped_list_index(Some(4), 1, 5), 4, "down at the bottom stays put");
    assert_eq!(stepped_list_index(Some(0), -1, 5), 0, "up at the top stays put");
    // With nothing selected, down seeds the top and up seeds the bottom.
    assert_eq!(stepped_list_index(None, 1, 5), 0);
    assert_eq!(stepped_list_index(None, -1, 5), 4);
    // Single-item list: both directions stay on the only row.
    assert_eq!(stepped_list_index(Some(0), 1, 1), 0);
    assert_eq!(stepped_list_index(None, -1, 1), 0);
    // Empty list: must not panic. `clamp(0, -1)` panics (min > max) and `len - 1`
    // underflows, so an empty list would have crashed the app two different ways.
    // The caller guards today; this makes the guard non-load-bearing.
    assert_eq!(stepped_list_index(None, 1, 0), 0);
    assert_eq!(stepped_list_index(None, -1, 0), 0);
    assert_eq!(stepped_list_index(Some(0), 1, 0), 0);
    assert_eq!(stepped_list_index(Some(3), -1, 0), 0);
}

#[test]
fn error_banner_clears_when_a_later_status_replaces_the_failure() {
    // Nothing showing → never stale (the banner is hidden anyway).
    assert!(!error_banner_is_stale(None, ""));
    assert!(!error_banner_is_stale(None, "Saved."));
    // The failure is still current (the status line still holds it) → keep the banner.
    assert!(!error_banner_is_stale(Some("Save failed: disk full"), "Save failed: disk full"));
    // A later success/info line replaced the failure text → the banner is stale and the
    // core rule fires: a fixed problem must not leave a scary banner stuck on screen.
    assert!(error_banner_is_stale(Some("Save failed: disk full"), "Saved."));
    assert!(error_banner_is_stale(Some("Upload failed: bad path"), ""));
}

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    // Unique per-test directory; the vault file name is fixed (vault.pmv),
    // matching production where the user controls only the directory.
    let dir = std::env::temp_dir().join(format!("vaultis-gui-{tag}-{}", nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("vault.pmv")
}

fn cleanup(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
        // Also drop any sibling export directory the test used (see `export_out`).
        // The vault dir name is nanosecond-tagged and unique, so this only ever
        // matches directories this test created.
        if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
            && let Ok(rd) = std::fs::read_dir(parent)
        {
            let prefix = format!("{name}-");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
}

/// An export destination OUTSIDE the vault directory — a sibling named
/// `<vault-dir>-<tag>`. The front-ends refuse an export directory inside the vault
/// folder ([`crate::checked_export_dir`]): a CSV holds every password in the clear and a
/// document export is the decrypted file, so either one landing next to `vault.pmv`
/// would be swept up by the user's next backup of the vault. Export tests therefore
/// write where a real user has to. `cleanup` removes these siblings too.
fn export_out(vault_path: &Path, tag: &str) -> std::path::PathBuf {
    let dir = vault_path.parent().expect("vault path has a parent");
    std::path::PathBuf::from(format!("{}-{tag}", dir.display()))
}

/// A GuiApp with a freshly-created, unlocked vault on the Main screen.
fn app_unlocked(tag: &str) -> (GuiApp, std::path::PathBuf) {
    let path = tmp(tag);
    let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
    let mut app = GuiApp::new(path.clone(), true);
    app.vault = Some(ov);
    app.screen = Screen::Main;
    (app, path)
}

/// Lay out every tab in a real egui — with and without a record selected — so a
/// layout fault in the shared chrome (the cards, the right-aligned action rows,
/// the stat tiles, the empty-state pane) fails here rather than in front of the
/// user. Nested layouts and duplicate widget ids only misbehave once something
/// actually measures them.
/// Lay the real window out across a fine sweep of widths, including sizes well
/// below the 720 px minimum, and require that it SETTLES and that the per-tab
/// controls survive the squeeze.
///
/// Two things this pins down:
/// - **Settling.** `try_run` drives frames until egui stops asking to repaint. A
///   layout that cannot decide whether it needs a scrollbar never stops asking.
///   (This is a necessary condition, not a sufficient one — a headless harness does
///   not reproduce every real-window oscillation — so it is a guard, not a proof.)
/// - **Reachability.** "⬇ CSV" must still be present at every width on every tab
///   that has a CSV form. It once sat in a right-aligned group where a narrow pane
///   pushed it against the divider; Summary is the sole tab with no CSV, because it
///   is a calculated view with no `csv::CsvTab` of its own.
#[test]
fn window_settles_and_keeps_its_controls_at_every_width() {
    use egui_kittest::{kittest::Queryable, Harness};

    let mut w = 480.0f32;
    while w <= 1040.0 {
        for tab in [Tab::Accounts, Tab::Assets, Tab::RealEstate, Tab::Taxes, Tab::Summary] {
            let (mut app, path) = app_unlocked("settle");
            // Enough rows, and long enough labels, that a small window genuinely
            // overflows — the condition under which scroll geometry gets decided.
            {
                let ov = app.vault.as_mut().unwrap();
                for i in 0..40 {
                    let mut a = Account::new().unwrap();
                    a.title = format!("A rather long account title number {i}");
                    a.owner = "Jane Q. Longname".into();
                    ov.vault.accounts.push(a);
                }
            }
            app.tab = tab;
            app.edit_account = Account::new().ok();
            app.edit_asset = AssetLiability::new().ok();
            app.edit_realestate = RealEstate::new().ok();
            app.edit_taxfiling = TaxFiling::new().ok();
            app.status = "Exported 3 record(s) to /a/deliberately/long/export/path/file.csv".into();
            let app = std::cell::RefCell::new(app);

            let mut h = Harness::builder()
                .with_size(egui::vec2(w, 420.0))
                .with_max_steps(64)
                .build_ui(|ui| app.borrow_mut().render(ui));
            h.try_run().unwrap_or_else(|e| panic!("window never settled at {w}x420 on {tab:?}: {e}"));

            if tab != Tab::Summary {
                assert_eq!(
                    h.query_all_by_label("⬇ CSV").count(),
                    1,
                    "the CSV export button must stay reachable at {w}x420 on {tab:?}"
                );
            }
            cleanup(&path);
        }
        w += 16.0;
    }
}


/// The tab strip WRAPS onto more lines on a narrow window instead of scrolling, so every
/// tab must be present (laid out, not clipped off the end behind a scrollbar) at every
/// width — that is the whole point of the change, and it is what a horizontal ScrollArea
/// could not promise.
/// Each strip label as rendered (glyph + text). Shared by the wrap tests, which also pins
/// that no tab is silently dropped from the strip.
#[cfg(test)]
const TAB_STRIP_LABELS: [&str; 9] = [
    "❗ URGENT",
    "📝 Instructions",
    "⚖ Trust and Will",
    "💰 Assets and Liabilities",
    "🔑 Accounts",
    "🏠 Real Estate",
    "📃 Taxes",
    "📁 General Documents",
    "📊 Summary",
];

/// The on-screen rectangles of the nine tab-strip labels, in strip order, with the app
/// rendered at `w` x 460. `None` for a label that laid out nowhere.
#[cfg(test)]
fn tab_strip_boxes(w: f32) -> Vec<Option<egui::Rect>> {
    use egui_kittest::{kittest::NodeT as _, Harness};

    let (mut app, path) = app_unlocked("tabwrap");
    app.tab = Tab::Urgent;
    let app = std::cell::RefCell::new(app);
    let mut h = Harness::builder()
        .with_size(egui::vec2(w, 460.0))
        .with_max_steps(64)
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.try_run().unwrap_or_else(|e| panic!("window never settled at {w}x460: {e}"));
    let boxes = TAB_STRIP_LABELS
        .iter()
        .map(|label| {
            h.root()
                .children_recursive()
                .filter(|n| n.accesskit_node().label().as_deref() == Some(*label))
                .filter_map(|n| n.accesskit_node().bounding_box())
                .map(|b| {
                    egui::Rect::from_min_max(
                        egui::pos2(b.x0 as f32, b.y0 as f32),
                        egui::pos2(b.x1 as f32, b.y1 as f32),
                    )
                })
                .next()
        })
        .collect();
    cleanup(&path);
    boxes
}

/// The tab strip WRAPS onto more lines on a narrow window instead of scrolling or running
/// off the right edge, so every tab must be laid out INSIDE the window at every width —
/// that is the whole point of the change, and neither a horizontal ScrollArea nor a
/// clipped single row can promise it.
///
/// Geometry, not mere presence: a clipped tab still appears in the accessibility tree, so
/// only its rectangle can tell a wrapped strip from a strip that runs off the window.
#[test]
fn every_tab_is_laid_out_inside_the_window_at_narrow_widths_in_real_egui() {
    for w in [420.0f32, 560.0, 800.0, 1200.0] {
        for (label, r) in TAB_STRIP_LABELS.iter().zip(tab_strip_boxes(w)) {
            let r = r.unwrap_or_else(|| panic!("tab {label:?} did not lay out at all at {w}x460"));
            assert!(
                r.right() <= w + 1.0,
                "tab {label:?} runs off the right edge at width {w} (its right edge is {})",
                r.right()
            );
        }
    }
}

/// The strip must keep wrapping — never scrolling, never clipping — at EVERY interface
/// scale, not just the default one.
///
/// The wrap guarantee predates the interface-size setting, which can make every tab
/// 1.5× wider. That is precisely the case where a strip would be tempted to scroll or
/// run off the edge, and the requirement is absolute: extra rows, never a scrollbar.
#[test]
fn the_tab_strip_wraps_at_every_interface_scale() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    for scale in UiScale::ALL {
        let f = scale.factor();
        // The narrowest real window at this scale — the worst case for a strip.
        let w = MIN_INNER_SIZE[0] * f;
        let (mut app, path) = app_unlocked("tabscale");
        app.tab = Tab::Urgent;
        let app = std::cell::RefCell::new(app);
        let mut h = Harness::builder()
            .with_size(egui::vec2(w, 700.0 * f))
            .with_max_steps(64)
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.ctx.set_zoom_factor(f);
        h.try_run().unwrap_or_else(|e| panic!("never settled at {scale:?}: {e}"));

        for label in TAB_STRIP_LABELS {
            let bb = h
                .root()
                .children_recursive()
                .filter(|n| n.accesskit_node().label().as_deref() == Some(label))
                .filter_map(|n| n.accesskit_node().bounding_box())
                .next()
                .unwrap_or_else(|| panic!("tab {label:?} did not lay out at {scale:?}"));
            assert!(
                bb.x1 as f32 <= w + 1.0,
                "tab {label:?} runs off the right edge at {scale:?} (right={:.0}, window={w:.0}) \
                 — the strip must add a row, never scroll or clip",
                bb.x1
            );
        }
        cleanup(&path);
    }
}

/// The top bar's OTHER row — the global actions — must stay inside the window too. They are
/// laid out right-to-left against the vault name, which gives up space first, so a narrow
/// window must never push a button off either edge.
#[test]
fn the_top_bar_actions_stay_inside_the_window_at_narrow_widths() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    for w in [420.0f32, 560.0, 900.0] {
        let (mut app, path) = app_unlocked("topbarfit");
        app.tab = Tab::Urgent;
        let app = std::cell::RefCell::new(app);
        let mut h = Harness::builder()
            .with_size(egui::vec2(w, 460.0))
            .with_max_steps(64)
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.try_run().unwrap_or_else(|e| panic!("window never settled at {w}x460: {e}"));
        for label in ["🔑 Passwords", "⚙ Config", "❓ Help", "Quit"] {
            let b = h
                .root()
                .children_recursive()
                .filter(|n| n.accesskit_node().label().as_deref() == Some(label))
                .filter_map(|n| n.accesskit_node().bounding_box())
                .next()
                .unwrap_or_else(|| panic!("action {label:?} did not lay out at {w}x460"));
            assert!(b.x0 >= -1.0, "action {label:?} runs off the LEFT edge at width {w} (x0 {})", b.x0);
            assert!(
                b.x1 <= w as f64 + 1.0,
                "action {label:?} runs off the RIGHT edge at width {w} (x1 {})",
                b.x1
            );
        }
        cleanup(&path);
    }
}

/// The complement of the width check: at a width that cannot fit all nine tabs on one
/// line, the strip must actually USE a second line (the last tab sits BELOW the first),
/// while a wide window keeps them all on one.
#[test]
fn the_tab_strip_uses_a_second_line_only_when_it_must() {
    let boxes = tab_strip_boxes(420.0);
    let first = boxes[0].expect("URGENT lays out");
    let last = boxes[8].expect("Summary lays out");
    assert!(
        last.top() > first.bottom() - 1.0,
        "at 420pt wide the strip must wrap: Summary (top {}) should sit below URGENT (bottom {})",
        last.top(),
        first.bottom()
    );

    let boxes = tab_strip_boxes(1400.0);
    let first = boxes[0].expect("URGENT lays out");
    let last = boxes[8].expect("Summary lays out");
    assert!(
        (last.top() - first.top()).abs() <= 1.0,
        "at 1400pt wide every tab fits on ONE line: Summary top {} vs URGENT top {}",
        last.top(),
        first.top()
    );
}

/// A read-only value must occupy the width of its TEXT, not the width of the pane.
/// It used to render as a disabled text box, so a one-word owner name took as much
/// room as a full address and the form read as a column of empty boxes.
#[test]
fn read_only_values_are_not_stretched_to_the_pane_in_real_egui() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    let width_of = |writable: bool| -> f32 {
        let path = tmp("rowidth");
        let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let mut app = GuiApp::new(path.clone(), writable);
        app.vault = Some(ov);
        app.screen = Screen::Main;
        app.tab = Tab::RealEstate;
        let mut r = RealEstate::new().unwrap();
        r.owner = "Jane".into();
        app.edit_realestate = Some(r);
        let app = std::cell::RefCell::new(app);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 680.0))
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.run();
        let w = h
            .root()
            .children_recursive()
            .filter(|n| n.value().as_deref() == Some("Jane"))
            .filter_map(|n| n.accesskit_node().bounding_box().map(|b| b.width() as f32))
            .fold(0.0_f32, f32::max);
        cleanup(&path);
        w
    };

    let ro = width_of(false);
    assert!(ro > 0.0, "the read-only value must render at all");
    assert!(
        ro < 120.0,
        "a four-letter read-only value should take the width of its text, not the pane (got {ro})"
    );
    // Write mode still uses a real edit box, which is meant to be a uniform target.
    assert!(width_of(true) > 200.0, "editable fields keep their designed width");
}

/// A long UNBROKEN value in a right-hand form pane must stay inside the pane.
///
/// `TextWrapMode::Wrap` breaks at word boundaries, so a value with no spaces in it —
/// a URL, an account or policy number, a long token — has nowhere to break and runs
/// past the pane. The form pane scrolls only VERTICALLY and `two_col` clips each
/// column at the divider, so the overflow is not merely ugly: it is invisible and
/// unreachable, with no scrollbar to say anything is missing.
/// NOTHING IS EVER TRUNCATED WITHOUT A SCROLLBAR.
///
/// The form pane is clipped at the column divider by `two_col`, so content wider than
/// the pane does not merely spill — it vanishes, silently. Reported on Assets: a long
/// value simply stopped, with no scrollbar to say there was more.
///
/// The invariant asserted here is the user-visible one rather than a pixel budget:
/// on every tab, in both modes, at the window's own minimum size, either the content
/// fits **or** a horizontal scrollbar exists to reach it.
///
/// Long UNBROKEN values are used deliberately: `TextWrapMode::Wrap` breaks on word
/// boundaries, so a URL or an account number is the case with nowhere to break.
#[test]
fn no_tab_truncates_content_without_offering_a_scrollbar() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    const LONG: &str =
        "https://portal.example-financial-services.com/accounts/statements/2026/Q4/download?token=AbCdEf0123456789XYZ";

    for writable in [false, true] {
        for win_w in [MIN_INNER_SIZE[0], 1200.0] {
            for tab in [
                Tab::Urgent, Tab::Instructions, Tab::TrustWill, Tab::Assets,
                Tab::Accounts, Tab::RealEstate, Tab::Taxes, Tab::GeneralDocuments,
            ] {
                let path = tmp("notrunc");
                let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
                let mut app = GuiApp::new(path.clone(), writable);
                app.vault = Some(ov);
                app.screen = Screen::Main;
                app.tab = tab;
                let mut a = AssetLiability::new().unwrap();
                a.title = LONG.into(); a.url = LONG.into(); a.description = LONG.into();
                a.institution = LONG.into(); a.owner = LONG.into();
                app.edit_asset = Some(a);
                let mut ac = Account::new().unwrap();
                ac.title = LONG.into(); ac.url = LONG.into(); ac.description = LONG.into();
                ac.username = LONG.into(); ac.password = LONG.into();
                app.edit_account = Some(ac);
                let mut re = RealEstate::new().unwrap();
                re.address = LONG.into(); re.comments = LONG.into();
                app.edit_realestate = Some(re);
                let mut tf = TaxFiling::new().unwrap();
                tf.notes = LONG.into();
                app.edit_taxfiling = Some(tf);
                let app = std::cell::RefCell::new(app);

                let mut h = Harness::builder()
                    .with_size(egui::vec2(win_w, 700.0))
                    .with_max_steps(64)
                    .build_ui(|ui| app.borrow_mut().render(ui));
                h.run();

                let (mut maxx, mut has_hbar) = (0.0_f32, false);
                for n in h.root().children_recursive() {
                    let role = format!("{:?}", n.accesskit_node().role());
                    let Some(bb) = n.accesskit_node().bounding_box() else { continue };
                    // A TextRun inside a TextEdit is the field's own clipped text,
                    // which the field scrolls internally — normal, not a layout fault.
                    if role != "TextRun" {
                        maxx = maxx.max(bb.x1 as f32);
                    }
                    if role == "ScrollBar" && (bb.x1 - bb.x0) > (bb.y1 - bb.y0) {
                        has_hbar = true;
                    }
                }
                cleanup(&path);

                // A few px of slack: the pane's own vertical scrollbar sits at the edge.
                assert!(
                    maxx <= win_w + 4.0 || has_hbar,
                    "{tab:?} (writable={writable}) at {win_w}px: content reaches x={maxx:.0} \
                     with NO horizontal scrollbar — it is clipped and unreachable"
                );
            }
        }
    }
}

/// A keyboard arrow landing in the SAME egui frame as a Delete click on the Assets
/// tab must not retarget the delete at the neighbouring record.
///
/// The 2026-07-03 audit found and fixed exactly this on the ACCOUNTS tab, where the
/// fix is an explicit `record_action_pending` guard. The Assets call site has no such
/// guard — it still carries the pre-fix comment ("they can't both happen in one
/// frame") — so it looks like the sibling bug was missed. It was not: Assets is
/// protected by a subtler mechanism, the `focused()` check inside `list_nav_target`
/// (clicking a button focuses it, which suppresses list nav for that frame).
///
/// That protection is emergent rather than stated, so this test exists to pin it: if
/// the focus guard is ever relaxed, Assets silently regains the data-loss bug that
/// Accounts was explicitly fixed for. The controls below keep the test honest.
#[test]
fn same_frame_arrow_does_not_retarget_a_delete_on_the_assets_tab() {
    use egui_kittest::{kittest::Queryable, Harness};

    let (mut app, path) = app_unlocked("navrace");
    {
        let ov = app.vault.as_mut().unwrap();
        for name in ["AAA-first", "BBB-second"] {
            let mut a = AssetLiability::new().unwrap();
            a.title = name.into();
            a.owner = "Jane".into();
            a.approx_value = "1".into();
            ov.vault.assets.push(a);
        }
    }
    app.tab = Tab::Assets;
    app.asset_grouped = false;
    // Editing the FIRST asset: this is the record the form shows and the one the
    // user's Delete click is aimed at.
    let first_id = app.vault_ref().vault.assets[0].id.clone();
    let second_id = app.vault_ref().vault.assets[1].id.clone();
    app.edit_asset = app.vault_ref().vault.assets.first().cloned();

    let app = std::cell::RefCell::new(app);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 1600.0))
        .build_ui(|ui| app.borrow_mut().render(ui));
    h.run();

    // Queue BOTH in the same frame: the arrow key and the Delete click. egui batches
    // input per repaint, so this is exactly what a real keypress landing alongside a
    // click looks like.
    // CONTROL: the arrow key alone must really move the selection. Without this the
    // assertion below would pass vacuously — an arrow key that does nothing in the
    // harness would "prove" the absence of a race it never exercised.
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        app.borrow().edit_asset.as_ref().map(|r| r.title.clone()).as_deref(),
        Some("BBB-second"),
        "control: a lone ArrowDown must move the selection"
    );
    h.key_press(egui::Key::ArrowUp);
    h.run();
    assert_eq!(
        app.borrow().edit_asset.as_ref().map(|r| r.title.clone()).as_deref(),
        Some("AAA-first"),
        "control: ArrowUp must move it back, so the delete below targets the first record"
    );

    h.get_by_label("🗑 Delete").click();
    h.key_press(egui::Key::ArrowDown);
    h.run();

    let a = app.borrow();
    let ids: Vec<String> = a.vault_ref().vault.assets.iter().map(|r| r.id.clone()).collect();
    assert!(
        !ids.contains(&first_id),
        "the asset the user was looking at should have been deleted; remaining={:?}",
        a.vault_ref().vault.assets.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
    );
    assert!(
        ids.contains(&second_id),
        "the NEIGHBOUR must survive — a same-frame arrow must not retarget the delete"
    );
    drop(a);
    cleanup(&path);
}

/// No password may ever reach the PLAINTEXT render path.
///
/// Read-only values are drawn as ordinary selectable text (`read_only_value`), which
/// is right for an address and catastrophic for a password. Secrets are supposed to
/// go through `secret_text_edit`, which masks unless the screen's reveal toggle is
/// on. This walks the real accessibility tree of the real window and asserts the
/// secret is not in it while masked — the check a label-name assertion cannot make,
/// since the widget is present either way and only its CONTENT differs.
#[test]
fn a_masked_password_never_appears_in_the_rendered_tree() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    const SECRET: &str = "correct-horse-battery-staple";

    // Both modes: read-only is the one whose render path just changed, write mode is
    // where the field is a real editable box.
    for writable in [false, true] {
        for reveal in [false, true] {
            let path = tmp("nopwleak");
            let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
            let mut app = GuiApp::new(path.clone(), writable);
            app.vault = Some(ov);
            app.screen = Screen::Main;
            app.tab = Tab::Accounts;
            app.reveal_all = reveal;
            let mut a = Account::new().unwrap();
            a.title = "Bank".into();
            a.owner = "Jane".into();
            a.password = SECRET.into();
            app.edit_account = Some(a);
            let app = std::cell::RefCell::new(app);
            let mut h = Harness::builder()
                .with_size(egui::vec2(1200.0, 1600.0))
                .build_ui(|ui| app.borrow_mut().render(ui));
            h.run();

            let exposed = h
                .root()
                .children_recursive()
                .filter_map(|n| n.value())
                .any(|v| v.contains(SECRET));
            assert_eq!(
                exposed, reveal,
                "password in the rendered tree should match the reveal toggle \
                 (writable={writable}, reveal={reveal})"
            );
            cleanup(&path);
        }
    }
}


/// A read-only field must render the STORED text, byte for byte.
///
/// egui copies a label's galley text, so the string handed to the label is exactly
/// what Ctrl+C returns — and a read-only session is the mode an heir is told to use,
/// with the manual promising these fields can be selected and copied. A previous
/// version pre-wrapped the text and inserted hyphens at the breaks, and normalised
/// whitespace with `split_whitespace`, so it silently altered values that never even
/// wrapped: a double space in an address vanished from the copy.
///
/// This reads the value back out of the real accessibility tree, which is what a
/// screen reader and the copy path both see.
#[test]
fn read_only_fields_render_the_stored_text_byte_for_byte() {
    use egui_kittest::{kittest::NodeT as _, Harness};

    // Values chosen for the ways the old implementation corrupted them: a run of
    // spaces, a tab, leading indentation, and a long unbreakable token that forces
    // a wrap in a narrow pane.
    let cases = [
        "1234  N Elm Street, Apt 5",
        "a\tb",
        "  indented",
        "https://portal.example.com/customer/login?ref=abcdefghijklmnopqrstuvwxyz0123456789",
    ];

    for value in cases {
        let path = tmp("rofidelity");
        let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let mut app = GuiApp::new(path.clone(), false); // read-only: the heir's mode
        app.vault = Some(ov);
        app.screen = Screen::Main;
        app.tab = Tab::RealEstate;
        let mut r = RealEstate::new().unwrap();
        r.address = value.to_string();
        app.edit_realestate = Some(r);
        let app = std::cell::RefCell::new(app);
        // Deliberately narrow, so the long value must wrap and any wrap-time
        // transformation would show up.
        let mut h = Harness::builder()
            .with_size(egui::vec2(620.0, 900.0))
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.run();

        let found = h
            .root()
            .children_recursive()
            .filter_map(|n| n.value())
            .any(|v| v == value);
        assert!(
            found,
            "the read-only field must expose the stored text unaltered; \
             {value:?} was not found verbatim in the rendered tree"
        );
        cleanup(&path);
    }
}

#[test]
fn every_tab_renders_in_real_egui() {
    use egui_kittest::Harness;

    let (app, path) = app_unlocked("tabrender");
    let app = std::cell::RefCell::new(app);
    let tabs = [
        Tab::Urgent,
        Tab::Instructions,
        Tab::TrustWill,
        Tab::Assets,
        Tab::Accounts,
        Tab::RealEstate,
        Tab::Taxes,
        Tab::GeneralDocuments,
        Tab::Summary,
    ];
    // `selected == true` starts a blank record on the tab first, which is what
    // brings the form, its document/link/portal cards, and the history view into
    // the frame; `false` exercises the empty-state pane.
    for selected in [false, true] {
        for tab in tabs {
            {
                let mut a = app.borrow_mut();
                a.tab = tab;
                if selected {
                    // Seed the tab's edit buffer directly (the same thing its "New"
                    // button does) so the form half of the split renders.
                    match tab {
                        Tab::Urgent => a.edit_urgent = Urgent::new().ok(),
                        Tab::Instructions => a.edit_instruction = Instruction::new().ok(),
                        Tab::TrustWill => a.edit_trustwill = TrustWill::new().ok(),
                        Tab::Assets => a.edit_asset = AssetLiability::new().ok(),
                        Tab::Accounts => a.edit_account = Account::new().ok(),
                        Tab::RealEstate => a.edit_realestate = RealEstate::new().ok(),
                        Tab::Taxes => a.edit_taxfiling = TaxFiling::new().ok(),
                        Tab::GeneralDocuments => a.edit_general = GeneralDocument::new().ok(),
                        Tab::Summary => {} // a calculated view — no edit buffer
                    }
                }
            }
            let mut h = Harness::new_ui(|ui| {
                let mut a = app.borrow_mut();
                a.ui_top_bar(ui);
                let tab = a.tab;
                match tab {
                    Tab::Urgent => a.tab_urgent(ui),
                    Tab::Instructions => a.tab_instructions(ui),
                    Tab::TrustWill => a.tab_trustwill(ui),
                    Tab::Assets => a.tab_assets(ui),
                    Tab::Accounts => a.tab_accounts(ui),
                    Tab::RealEstate => a.tab_realestate(ui),
                    Tab::Taxes => a.tab_taxes(ui),
                    Tab::GeneralDocuments => a.tab_general(ui),
                    Tab::Summary => a.tab_summary(ui),
                }
            });
            h.run();
        }
    }
    // The Config and Help screens go through the same treatment.
    let mut h = Harness::new_ui(|ui| app.borrow_mut().ui_config(ui));
    h.run();
    let mut h = Harness::new_ui(|ui| app.borrow_mut().ui_help(ui));
    h.run();
    cleanup(&path);
}

#[test]
fn start_page_prefills_vault_dir_and_switches_mode_with_directory() {
    // The start page pre-fills the vault directory (the default/launch dir) and flips
    // Unlock<->Create as the directory changes; in --write mode an empty dir creates.
    let base = std::env::temp_dir().join(format!("vaultis-gui-startdir-{}", nanos()));
    std::fs::create_dir_all(&base).unwrap();

    // Launched at a non-existent path -> Create, and vault_dir is pre-filled with the dir.
    let start = base.join("fresh").join("vault.pmv");
    let mut app = GuiApp::new(start.clone(), true);
    assert_eq!(app.auth_mode, AuthMode::Create, "no vault yet -> Create");
    assert_eq!(app.vault_dir, base.join("fresh").display().to_string(), "dir pre-filled");

    // Collapsed model: root pre-filled with the launch dir's parent, name = its folder.
    assert_eq!(app.vault_root, base.display().to_string(), "root = parent of launch dir");
    assert_eq!(app.vault_name, "fresh", "name = launch dir's folder");

    // Type a brand-new vault name to create it under the root.
    app.vault_name = "brandnew".into();
    app.recompute_vault_path();
    let fresh = base.join("brandnew");
    assert_eq!(app.vault_dir, fresh.display().to_string(), "dir = root/name");
    assert_eq!(app.auth_mode, AuthMode::Create, "no vault there -> Create");
    app.pw1 = "a".into();
    app.confirm1 = "a".into();
    app.pw2 = "b".into();
    app.confirm2 = "b".into();
    app.submit_auth();
    assert!(app.vault.is_some(), "vault created in the new dir; status: {}", app.status);
    assert!(fresh.join("vault.pmv").exists(), "vault.pmv created on disk");

    // A new app pointed at that now-existing dir resolves to Unlock.
    let app2 = GuiApp::new(fresh.join("vault.pmv"), true);
    assert_eq!(app2.auth_mode, AuthMode::Unlock, "existing vault -> Unlock");
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn start_page_read_only_cannot_create_in_empty_dir() {
    // Pointing read-only mode at a directory with no vault: Create mode is shown, but
    // submit refuses (you can't create a vault read-only) — the field stays usable so
    // an heir can still point at a real vault to READ.
    let base = std::env::temp_dir().join(format!("vaultis-gui-rodir-{}", nanos()));
    std::fs::create_dir_all(&base).unwrap();
    let mut app = GuiApp::new(base.join("empty").join("vault.pmv"), false); // read-only
    assert_eq!(app.auth_mode, AuthMode::Create);
    app.pw1 = "a".into();
    app.confirm1 = "a".into();
    app.pw2 = "b".into();
    app.confirm2 = "b".into();
    app.submit_auth();
    assert!(app.vault.is_none(), "read-only must not create a vault");
    assert!(
        app.auth_error.as_deref().unwrap_or("").contains("--write"),
        "error explains --write is needed; got {:?}",
        app.auth_error
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn theme_id_round_trips_and_defaults_to_light() {
    for t in Theme::ALL {
        assert_eq!(Theme::from_id(t.id()), Some(t), "{} id must round-trip", t.label());
    }
    assert_eq!(Theme::from_id("nonsense"), None);
    assert_eq!(Theme::default(), Theme::Light);
    // Every theme builds a usable Visuals (no panic / field mismatch).
    for t in Theme::ALL {
        let _ = visuals_for(t);
    }
}

#[test]
fn sync_account_filters_to_follows_only_active_filters() {
    let (mut app, path) = app_unlocked("guifsync");
    app.acct_filter_type = "Email".into(); // active
    app.acct_filter_title = "Personal".into(); // active
    app.acct_filter_owner = String::new(); // inactive
    let mut a = Account::new().unwrap();
    a.account_type = "Bank".into();
    a.title = "Savings".into();
    a.owner = "Bob".into();
    app.sync_account_filters_to(&a);
    assert_eq!(app.acct_filter_type, "Bank", "active type filter follows the saved value");
    assert_eq!(app.acct_filter_title, "Savings", "active title filter follows the saved value");
    assert_eq!(app.acct_filter_owner, "", "an inactive filter stays unset");
    cleanup(&path);
}

#[test]
fn sync_account_filters_relaxes_review_and_search_in_gui() {
    let (mut app, path) = app_unlocked("guirelax");
    app.acct_filter_review = true;
    app.acct_search_user = "alice".into();
    let mut a = Account::new().unwrap();
    a.review = false; // saved record is NOT flagged
    a.username = "bob".into(); // and does not match the search
    app.sync_account_filters_to(&a);
    assert!(!app.acct_filter_review, "review-only filter relaxed so a non-flagged save stays visible");
    assert_eq!(app.acct_search_user, "", "username search relaxed when it no longer matches the save");

    // A still-matching save leaves the search in place.
    app.acct_search_user = "bo".into();
    let mut keep = Account::new().unwrap();
    keep.username = "bob".into();
    app.sync_account_filters_to(&keep);
    assert_eq!(app.acct_search_user, "bo", "a still-matching search is left as-is");
    cleanup(&path);
}

#[test]
fn account_search_matches_username_or_title_in_gui() {
    let (mut app, path) = app_unlocked("guisearchtitle");
    let mut by_title = Account::new().unwrap();
    by_title.username = "u1".into();
    by_title.title = "Brokerage account".into();
    let mut other = Account::new().unwrap();
    other.username = "u2".into();
    other.title = "Email".into();

    app.acct_search_user = "broker".into();
    assert!(app.account_passes_filters(&by_title), "title substring matches");
    assert!(!app.account_passes_filters(&other), "non-match excluded");
    // Still matches by username.
    app.acct_search_user = "u2".into();
    assert!(app.account_passes_filters(&other));
    assert!(!app.account_passes_filters(&by_title));
    cleanup(&path);
}

#[test]
fn link_dropdown_search_matches_anywhere_in_the_label() {
    // The "Link an account…" popup's search box narrows the candidate list. The typed
    // letters must be found ANYWHERE in the label — not anchored to its start or end — and
    // a sound-alike spelling must still hit, matching the Accounts search box.
    let candidates: Vec<(String, String)> = vec![
        ("a1".into(), "Fidelity Brokerage — katherine".into()),
        ("a2".into(), "Chase Checking — bob".into()),
        ("a3".into(), "Gmail — alice".into()),
    ];
    let ids = |q: &str| -> Vec<String> {
        filter_link_candidates(&candidates, q).iter().map(|(id, _)| id.clone()).collect()
    };
    // Empty query keeps everything, in the given order.
    assert_eq!(ids(""), vec!["a1", "a2", "a3"]);
    assert_eq!(ids("   "), vec!["a1", "a2", "a3"], "whitespace-only query is no filter");
    // Mid-word letters match (the old dropdown offered no search at all).
    assert_eq!(ids("elit"), vec!["a1"], "matches inside 'Fidelity'");
    assert_eq!(ids("heck"), vec!["a2"], "matches inside 'Checking'");
    assert_eq!(ids("ali"), vec!["a3"]);
    // Case-insensitive, and a sound-alike spelling of a name still finds the account.
    assert_eq!(ids("CHASE"), vec!["a2"]);
    assert_eq!(ids("catherine"), vec!["a1"], "sounds like 'katherine'");
    // A genuine non-match yields nothing (the popup then says so).
    assert!(ids("zebra").is_empty());
}

#[test]
fn account_requires_title_then_owner_in_gui() {
    // The shared save-validation rule the GUI form enforces: title first, then
    // owner; only a record with both (non-blank after trim) may be saved.
    let mut a = Account::new().unwrap();
    assert_eq!(
        account_required_field_error(&a),
        Some("Title is required — every account must have a title.")
    );
    a.title = "  Brokerage  ".into(); // whitespace-only would still fail; real text passes
    assert_eq!(
        account_required_field_error(&a),
        Some("Owner is required — every account must have an owner."),
        "title satisfied, owner still missing"
    );
    a.owner = "   ".into(); // whitespace-only owner is still missing
    assert_eq!(account_required_field_error(&a), Some("Owner is required — every account must have an owner."));
    a.owner = "Alice".into();
    assert_eq!(account_required_field_error(&a), None, "title + owner present -> savable");
}

#[test]
fn export_current_tab_csv_writes_accounts_file_in_gui() {
    let (mut app, path) = app_unlocked("guicsv");
    let outdir = export_out(&path, "csv");
    {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut ov.vault.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.export_dir = outdir.to_string_lossy().into();
    app.export_current_tab_csv();
    assert!(is_export_caveat(&app.status), "status leads with the caveat: {}", app.status);
    assert!(app.status.contains("Exported 1 record"), "status: {}", app.status);
    let entry = std::fs::read_dir(&outdir).unwrap().next().unwrap().unwrap();
    let name = entry.file_name().to_string_lossy().into_owned();
    assert!(name.starts_with("accounts-") && name.ends_with(".csv"), "timestamped name: {name}");
    let body = std::fs::read_to_string(entry.path()).unwrap();
    assert!(body.contains("hunter2"), "password exported in plaintext (user opted in)");
    assert!(body.contains("Bank"));
    cleanup(&path);
}

#[test]
fn exports_into_the_vault_folder_are_refused_in_gui() {
    // A CSV export carries every account password in the CLEAR and a document export is
    // the decrypted file, so neither may land inside the vault folder — the user's next
    // backup or folder sync of that vault would carry the plaintext with it. The CLI has
    // refused this since the extract/export-tree guards; the GUI must too.
    let (mut app, path) = app_unlocked("guioutguard");
    let vault_dir = path.parent().unwrap().to_path_buf();
    {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut ov.vault.accounts, a);
    }
    app.tab = Tab::Accounts;

    // Both the vault folder itself and a subdirectory of it are refused, and NOTHING is
    // written — in particular no partial CSV holding the password.
    for inside in [vault_dir.clone(), vault_dir.join("exports")] {
        app.export_dir = inside.to_string_lossy().into();
        app.status.clear();
        app.error = None;
        app.export_current_tab_csv();
        let shown = format!("{} {}", app.status, app.error.clone().unwrap_or_default());
        assert!(shown.contains("OUTSIDE the vault folder"), "refused with an actionable message: {shown}");
        assert!(!inside.join("exports").exists(), "no export directory was created under the vault");
        let stray: Vec<_> = std::fs::read_dir(&vault_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".csv"))
            .collect();
        assert!(stray.is_empty(), "no cleartext CSV written into the vault folder: {stray:?}");
    }

    // The same session exports fine once the directory is outside the vault.
    let outside = export_out(&path, "csv");
    app.export_dir = outside.to_string_lossy().into();
    app.export_current_tab_csv();
    assert!(is_export_caveat(&app.status), "an outside directory still works: {}", app.status);
    cleanup(&path);
}

#[test]
fn export_current_tab_csv_works_in_read_only_mode_in_gui() {
    // CSV export is deliberately available to a READ-ONLY session (the vault owner
    // asked for it). The file is unencrypted and may hold plaintext passwords, so the
    // status line has to say so rather than report a bare success.
    let (mut app, path) = app_unlocked("guicsvro");
    let outdir = export_out(&path, "csv");
    {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.owner = "Jane".into();
        a.password = "hunter2".into();
        records::upsert(&mut ov.vault.accounts, a);
    }
    app.tab = Tab::Accounts;
    app.export_dir = outdir.to_string_lossy().into();
    app.writable = false; // a read-only session
    app.export_current_tab_csv();
    assert!(is_export_caveat(&app.status), "export ran read-only: {}", app.status);
    assert!(
        app.status.contains("UNENCRYPTED"),
        "the plaintext warning must ride along with the success: {}",
        app.status
    );
    assert!(app.error.is_none(), "a successful export raises no failure banner");
    let written: Vec<_> = std::fs::read_dir(&outdir).unwrap().filter_map(Result::ok).collect();
    assert_eq!(written.len(), 1, "one CSV written");
    let body = std::fs::read_to_string(written[0].path()).unwrap();
    assert!(body.contains("hunter2"), "the CSV carries the plaintext password it warns about");
    cleanup(&path);
}

/// The button itself must be present in a read-only session, on both the flat list
/// and the grouped tree — the export is useless if it cannot be reached.
#[test]
fn csv_button_is_offered_in_read_only_mode_in_real_egui() {
    use egui_kittest::{kittest::Queryable, Harness};

    for grouped in [false, true] {
        let path = tmp("rocsvbtn");
        let ov = OpenVault::create(path.clone(), b"a", b"b", fast()).unwrap();
        let mut app = GuiApp::new(path.clone(), false); // read-only
        app.vault = Some(ov);
        app.screen = Screen::Main;
        app.tab = Tab::Accounts;
        app.acct_grouped = grouped;
        let app = std::cell::RefCell::new(app);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 680.0))
            .build_ui(|ui| app.borrow_mut().render(ui));
        h.run();
        assert_eq!(
            h.query_all_by_label("⬇ CSV").count(),
            1,
            "CSV must be reachable read-only (grouped={grouped})"
        );
        // "New" stays hidden: creating records is still a write.
        assert_eq!(h.query_all_by_label("➕ New").count(), 0, "New stays write-only");
        cleanup(&path);
    }
}

#[test]
fn trim_all_records_bulk_trims_every_tab_and_reports_in_gui() {
    let (mut app, path) = app_unlocked("guitrimall");
    {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.owner = "  Alice  ".into();
        a.title = " Brokerage ".into();
        a.password = "  s3cret  ".into();
        records::upsert(&mut ov.vault.accounts, a);
        let b = Account::new().unwrap(); // already clean (all empty)
        records::upsert(&mut ov.vault.accounts, b);
        // A dirty record on ANOTHER tab must also be trimmed (whole-vault sweep).
        let mut re = RealEstate::new().unwrap();
        re.address = "  1 Main St  ".into();
        re.property_mgmt_password = "  portalpw  ".into();
        records::upsert(&mut ov.vault.real_estate, re);
        let mut tax = TaxFiling::new().unwrap();
        tax.year = " 2024 ".into();
        records::upsert(&mut ov.vault.tax_filings, tax);
    }
    let n = app.trim_all_records();
    assert_eq!(n, 3, "the dirty account + real-estate + tax records are all counted");
    let a = &app.vault.as_ref().unwrap().vault.accounts[0];
    assert_eq!(a.owner, "Alice");
    assert_eq!(a.title, "Brokerage");
    assert_eq!(a.password, "s3cret", "the password is trimmed too (configured policy)");
    let re = &app.vault.as_ref().unwrap().vault.real_estate[0];
    assert_eq!(re.address, "1 Main St");
    assert_eq!(re.property_mgmt_password, "portalpw", "portal passwords are trimmed too");
    assert_eq!(app.vault.as_ref().unwrap().vault.tax_filings[0].year, "2024");
    assert!(app.status.contains("Trimmed 3"), "status: {}", app.status);
    // Idempotent.
    assert_eq!(app.trim_all_records(), 0);
    assert!(app.status.contains("Nothing to trim"), "status: {}", app.status);
    cleanup(&path);
}

#[test]
fn new_account_from_filters_prepopulates() {
    let (mut app, path) = app_unlocked("guifilterprefill");
    app.acct_filter_title = "Bank login".into();
    app.acct_filter_type = "Financial".into();
    app.acct_filter_subtype = "IRA".into();
    app.acct_filter_owner = "Alice".into();
    app.acct_search_user = "alice99".into();
    let a = app.new_account_from_filters().unwrap();
    assert_eq!(a.title, "Bank login");
    assert_eq!(a.account_type, "Financial");
    assert_eq!(a.account_subtype, "IRA");
    assert_eq!(a.owner, "Alice");
    assert_eq!(a.username, "alice99");
    assert!(a.password.is_empty(), "no secret invented");
    // Empty filters -> blank new account.
    app.acct_filter_title.clear();
    app.acct_filter_type.clear();
    app.acct_filter_subtype.clear();
    app.acct_filter_owner.clear();
    app.acct_search_user.clear();
    let b = app.new_account_from_filters().unwrap();
    assert_eq!(b.title, "");
    assert_eq!(b.account_type, "");
    assert_eq!(b.owner, "");
    assert_eq!(b.username, "");
    cleanup(&path);
}

#[test]
fn new_account_seeded_inherits_grouping_from_open_record() {
    let (mut app, path) = app_unlocked("guiacctseed");
    // A record is open in the editor: +New inherits its grouping fields only.
    let mut cur = Account::new().unwrap();
    cur.title = "Existing bank".into();
    cur.account_type = "Financial".into();
    cur.account_subtype = "Checking".into();
    cur.owner = "Bob".into();
    cur.username = "bob99".into();
    cur.password = "s3cret".into();
    app.edit_account = Some(cur);
    let a = app.new_account_seeded().unwrap();
    assert_eq!(a.account_type, "Financial", "type inherited");
    assert_eq!(a.account_subtype, "Checking", "subtype inherited");
    assert_eq!(a.owner, "Bob", "owner inherited");
    assert_eq!(a.title, "", "identifying title NOT carried over");
    assert_eq!(a.username, "", "username NOT carried over");
    assert!(a.password.is_empty(), "secret NEVER carried over");
    // With no record open it falls back to the active filters.
    app.edit_account = None;
    app.acct_filter_type = "Email".into();
    app.acct_filter_owner = "Carol".into();
    let b = app.new_account_seeded().unwrap();
    assert_eq!(b.account_type, "Email");
    assert_eq!(b.owner, "Carol");
    cleanup(&path);
}

#[test]
fn new_asset_seeded_inherits_grouping_from_open_record() {
    let (mut app, path) = app_unlocked("guiassetseed");
    let mut cur = AssetLiability::new().unwrap();
    cur.kind = "Liability".into();
    cur.asset_type = "Mortgage".into();
    cur.owner = "Dana".into();
    cur.title = "Existing loan".into();
    cur.approx_value = "250000".into();
    cur.institution = "Big Bank".into();
    app.edit_asset = Some(cur);
    let a = app.new_asset_seeded().unwrap();
    assert_eq!(a.kind, "Liability", "kind inherited");
    assert_eq!(a.asset_type, "Mortgage", "asset type inherited");
    assert_eq!(a.owner, "Dana", "owner inherited");
    assert_eq!(a.title, "", "identifying title NOT carried over");
    assert_eq!(a.approx_value, "", "value NOT carried over");
    assert_eq!(a.institution, "", "institution NOT carried over");
    // With nothing open the new asset is blank (default kind, no owner/type).
    app.edit_asset = None;
    let b = app.new_asset_seeded().unwrap();
    assert_eq!(b.asset_type, "");
    assert_eq!(b.owner, "");
    cleanup(&path);
}

#[test]
fn gui_general_document_upload_export_remove() {
    let (mut app, path) = app_unlocked("guigendoc");
    let dir = path.parent().unwrap().to_path_buf();
    let mut g = GeneralDocument::new().unwrap();
    g.title = "Passport".into();
    app.edit_general = Some(g);

    let src = dir.join("p.pdf");
    std::fs::write(&src, b"passport").unwrap();
    app.doc_filename = "p.pdf".into();
    app.doc_source = src.to_string_lossy().into();
    app.doc_subfolder = "ids".into();
    app.handle_doc(DocReq::Attach, DocTarget::General);
    let id = app.edit_general.as_ref().unwrap().file.clone();
    assert!(id.is_some(), "uploaded; status: {}", app.status);
    let id = id.unwrap();
    assert_eq!(
        app.vault.as_ref().unwrap().vault.general_documents[0].file.as_deref(),
        Some(id.as_str()),
        "persisted"
    );
    // Uniform layout: /general-documents/<title>/<subfolder>/<ts>_<filename>.
    let vpath = app.vault.as_ref().unwrap().doc_path(&id).unwrap();
    assert!(vpath.trim_start_matches('/').starts_with("general-documents/passport/ids/"), "got {vpath}");
    assert!(vpath.ends_with("_p.pdf"), "ts-prefixed filename, got {vpath}");

    // Export goes to the configured export dir, recreating the volume folder structure.
    let export_root = export_out(&path, "exports");
    app.export_dir = export_root.to_string_lossy().into();
    app.handle_doc(DocReq::Export, DocTarget::General);
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(
        std::fs::read(&exported).unwrap(),
        b"passport",
        "export recreates the volume structure under the export dir (status: {})",
        app.status
    );

    app.handle_doc(DocReq::Remove, DocTarget::General);
    assert!(app.edit_general.as_ref().unwrap().file.is_none(), "removed");
    assert!(!app.vault.as_ref().unwrap().has_document(&id), "blob reclaimed");
    cleanup(&path);
}

#[test]
fn gui_real_estate_document_upload_export_remove() {
    let (mut app, path) = app_unlocked("guiredoc");
    let dir = path.parent().unwrap().to_path_buf();
    let mut re = RealEstate::new().unwrap();
    re.address = "1 Main".into();
    app.edit_realestate = Some(re);

    // --- upload ---
    let src = dir.join("deed.txt");
    std::fs::write(&src, b"the deed").unwrap();
    app.doc_filename = "deed.txt".into();
    app.doc_source = src.to_string_lossy().into();
    app.handle_re_doc(ReDocReq::Upload);
    assert_eq!(app.edit_realestate.as_ref().unwrap().documents.len(), 1, "uploaded one doc");
    assert_eq!(app.vault.as_ref().unwrap().vault.real_estate[0].documents.len(), 1, "persisted");

    // --- export (into the configured export dir, structure preserved) ---
    let export_root = export_out(&path, "exports");
    app.export_dir = export_root.to_string_lossy().into();
    let re_id = app.edit_realestate.as_ref().unwrap().documents[0].clone();
    let vpath = app.vault.as_ref().unwrap().doc_path(&re_id).unwrap();
    app.handle_re_doc(ReDocReq::Export(0));
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(std::fs::read(&exported).unwrap(), b"the deed", "export recreates structure (status: {})", app.status);

    // --- remove ---
    app.handle_re_doc(ReDocReq::Remove(0));
    assert!(app.edit_realestate.as_ref().unwrap().documents.is_empty(), "removed the doc");
    assert!(app.vault.as_ref().unwrap().vault.real_estate[0].documents.is_empty(), "unlinked");
    cleanup(&path);
}

#[test]
fn gui_tax_document_upload_export_remove() {
    let (mut app, path) = app_unlocked("guitaxdoc");
    let dir = path.parent().unwrap().to_path_buf();
    let mut tf = TaxFiling::new().unwrap();
    tf.year = "2024".into();
    app.edit_taxfiling = Some(tf);

    let src = dir.join("w2.txt");
    std::fs::write(&src, b"taxable income").unwrap();
    app.doc_filename = "w2.txt".into();
    app.doc_source = src.to_string_lossy().into();
    app.handle_tax_doc(TaxDocReq::Upload);
    assert_eq!(app.edit_taxfiling.as_ref().unwrap().documents.len(), 1, "uploaded one doc");
    assert_eq!(app.vault.as_ref().unwrap().vault.tax_filings[0].documents.len(), 1, "persisted");

    let export_root = export_out(&path, "exports");
    app.export_dir = export_root.to_string_lossy().into();
    let tax_id = app.edit_taxfiling.as_ref().unwrap().documents[0].clone();
    let vpath = app.vault.as_ref().unwrap().doc_path(&tax_id).unwrap();
    app.handle_tax_doc(TaxDocReq::Export(0));
    let exported = export_root.join(vpath.trim_start_matches('/'));
    assert_eq!(std::fs::read(&exported).unwrap(), b"taxable income", "export recreates structure (status: {})", app.status);

    app.handle_tax_doc(TaxDocReq::Remove(0));
    assert!(app.edit_taxfiling.as_ref().unwrap().documents.is_empty(), "removed the doc");
    assert!(app.vault.as_ref().unwrap().vault.tax_filings[0].documents.is_empty(), "unlinked");
    cleanup(&path);
}

#[test]
fn upload_with_empty_filename_uses_source_basename_in_gui() {
    // "If a filename isn't specified, use the same filename as the uploaded file."
    let (mut app, path) = app_unlocked("guinofname");
    let dir = path.parent().unwrap().to_path_buf();
    app.edit_general = Some(GeneralDocument::new().unwrap());
    let src = dir.join("MyDeed.PDF");
    std::fs::write(&src, b"x").unwrap();
    app.doc_filename = String::new(); // no filename given
    app.doc_source = src.to_string_lossy().into();
    app.handle_doc(DocReq::Attach, DocTarget::General);
    let id = app.edit_general.as_ref().unwrap().file.clone().expect("uploaded (status: ");
    let vpath = app.vault.as_ref().unwrap().doc_path(&id).unwrap();
    assert!(vpath.ends_with("_MyDeed.PDF"), "empty filename falls back to the source basename: {vpath}");
    cleanup(&path);
}

#[test]
fn load_theme_from_round_trips_and_is_bounded_and_symlink_safe() {
    let dir = std::env::temp_dir().join(format!("pmprefs-{}", nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("prefs.json");
    // A valid small prefs file round-trips through save/load.
    save_theme_to(&p, Theme::Solarized);
    assert_eq!(load_theme_from(&p), Theme::Solarized);
    // Unknown id falls back to the default.
    std::fs::write(&p, br#"{"theme":"nope"}"#).unwrap();
    assert_eq!(load_theme_from(&p), Theme::Light);
    // Over-cap file is rejected before the body is parsed (DoS guard).
    std::fs::write(&p, vec![b'{'; (crate::MAX_PREFS_SIZE as usize) + 1]).unwrap();
    assert_eq!(load_theme_from(&p), Theme::Light);
    // Missing file -> default.
    assert_eq!(load_theme_from(&dir.join("absent.json")), Theme::Light);
    // A symlinked prefs file is refused even if its target is a valid prefs file.
    #[cfg(unix)]
    {
        let real = dir.join("real.json");
        save_theme_to(&real, Theme::Dark);
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(load_theme_from(&link), Theme::Light, "symlinked prefs refused");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_flow_builds_vault() {
    let path = tmp("create");
    let mut app = GuiApp::new(path.clone(), true);
    app.auth_mode = AuthMode::Create;
    app.pw1 = "a".into();
    app.confirm1 = "a".into();
    app.pw2 = "b".into();
    app.confirm2 = "b".into();
    app.submit_auth();
    assert!(app.vault.is_some());
    assert!(app.screen == Screen::Main);
    assert!(app.pw1.is_empty(), "passwords wiped after submit");
    cleanup(&path);
}

#[test]
fn account_username_search_filters() {
    let (mut app, path) = app_unlocked("usersearch");
    {
        let v = &mut app.vault.as_mut().unwrap().vault;
        for u in ["alice", "alice2", "bob"] {
            let mut a = Account::new().unwrap();
            a.username = u.into();
            records::upsert(&mut v.accounts, a);
        }
    }
    assert_eq!(app.filtered_account_labels().len(), 3, "no search → all");
    app.acct_search_user = "ALI".into(); // case-insensitive substring
    assert_eq!(app.filtered_account_labels().len(), 2, "alice + alice2");
    app.acct_search_user = "bob".into();
    assert_eq!(app.filtered_account_labels().len(), 1);
    app.acct_search_user = "zzz".into();
    assert_eq!(app.filtered_account_labels().len(), 0, "no match");
    cleanup(&path);
}

#[test]
fn urgent_tab_is_the_default_and_delete_removes_the_note() {
    let (mut app, path) = app_unlocked("urgent");
    // URGENT is the first/default tab.
    assert!(matches!(app.tab, Tab::Urgent));
    // Seed an urgent note, load it into the editor, and delete it via the tab arm.
    let id = {
        let ov = app.vault.as_mut().unwrap();
        let mut u = Urgent::new().unwrap();
        u.title = "Hospital".into();
        u.description = "Contact Dr. Smith".into();
        let id = u.id.clone();
        records::upsert(&mut ov.vault.urgent, u.clone());
        app.edit_urgent = Some(u);
        id
    };
    assert_eq!(app.vault.as_ref().unwrap().vault.urgent.len(), 1);
    app.delete_current(Tab::Urgent);
    assert!(app.vault.as_ref().unwrap().vault.urgent.is_empty(), "note deleted");
    assert!(app.edit_urgent.is_none(), "editor cleared");
    assert!(!id.is_empty());
    cleanup(&path);
}

#[test]
fn mismatched_confirmation_is_rejected() {
    let path = tmp("mismatch");
    let mut app = GuiApp::new(path.clone(), true);
    app.auth_mode = AuthMode::Create;
    app.pw1 = "a".into();
    app.confirm1 = "a".into();
    app.pw2 = "b".into();
    app.confirm2 = "WRONG".into();
    app.submit_auth();
    assert!(app.vault.is_none());
    assert!(app.auth_error.is_some());
    cleanup(&path);
}

#[test]
fn attach_then_detach_document_round_trip() {
    let (mut app, path) = app_unlocked("doc");
    let src = std::env::temp_dir().join(format!("vaultis-guisrc-{}.txt", nanos()));
    std::fs::write(&src, b"will body").unwrap();

    let mut asset = AssetLiability::new().unwrap();
    asset.owner = "Jane Doe".into(); // owner + numeric value are now required before attach
    asset.approx_value = "1000".into();
    app.edit_asset = Some(asset);
    app.doc_subfolder = "wills".into();
    app.doc_filename = "will.txt".into();
    app.doc_source = src.display().to_string();
    app.handle_doc(DocReq::Attach, DocTarget::Asset);

    let id = app.edit_asset.as_ref().unwrap().statement.clone();
    assert!(id.is_some(), "statement linked to the uploaded doc");
    let id = id.unwrap();
    let ov = app.vault.as_ref().unwrap();
    assert!(ov.has_document(&id));
    assert_eq!(&ov.read_document(&id).unwrap()[..], b"will body");

    // Detach reclaims the blob and unlinks the record.
    app.handle_doc(DocReq::Remove, DocTarget::Asset);
    assert!(app.edit_asset.as_ref().unwrap().statement.is_none());
    assert!(!app.vault.as_ref().unwrap().has_document(&id));

    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn attach_accepts_a_double_quoted_upload_from_path() {
    // A path pasted with surrounding double quotes ("Copy as path") uploads the same
    // file as the unquoted path — the quotes are stripped, not treated as part of the name.
    let (mut app, path) = app_unlocked("docq");
    let src = std::env::temp_dir().join(format!("vaultis-guiq-{}.txt", nanos()));
    std::fs::write(&src, b"quoted body").unwrap();

    let mut asset = AssetLiability::new().unwrap();
    asset.owner = "Jane Doe".into();
    asset.approx_value = "1000".into();
    app.edit_asset = Some(asset);
    app.doc_subfolder = "wills".into();
    // No explicit filename → it must default to the (quoted) source's basename.
    app.doc_source = format!("\"{}\"", src.display()); // wrap the path in double quotes
    app.handle_doc(DocReq::Attach, DocTarget::Asset);

    let id = app.edit_asset.as_ref().unwrap().statement.clone();
    assert!(id.is_some(), "quoted upload path was accepted and the doc attached");
    let ov = app.vault.as_ref().unwrap();
    let id = id.unwrap();
    assert_eq!(&ov.read_document(&id).unwrap()[..], b"quoted body");
    // The stored virtual path ends with the real filename, not a quote-mangled one.
    let vpath = ov.doc_path(&id).unwrap_or_default();
    let stem = src.file_stem().unwrap().to_string_lossy();
    assert!(vpath.contains(&*stem) && !vpath.contains('"'), "clean filename in {vpath}");

    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn over_long_filename_is_capped_not_rejected_in_gui() {
    // With the uniform layout every path component is length-capped (filename to
    // 120 bytes, group/subfolder to 40, timestamp fixed), so a huge filename can
    // no longer push the virtual path over MAX_PATH_LEN — it is sanitized and
    // truncated, and the upload succeeds rather than being rejected.
    let (mut app, path) = app_unlocked("guipath");
    let src = std::env::temp_dir().join(format!("vaultis-guipath-{}.txt", nanos()));
    std::fs::write(&src, b"x").unwrap();
    let mut asset = AssetLiability::new().unwrap();
    asset.owner = "Jane Doe".into(); // owner + numeric value are now required before attach
    asset.approx_value = "1000".into();
    app.edit_asset = Some(asset);
    app.doc_subfolder = "d".into();
    app.doc_filename = "f".repeat(crate::storage::MAX_PATH_LEN);
    app.doc_source = src.display().to_string();
    app.handle_doc(DocReq::Attach, DocTarget::Asset);
    let id = app.edit_asset.as_ref().unwrap().statement.clone();
    assert!(id.is_some(), "upload should succeed with a capped name; status: {}", app.status);
    // The stored virtual path stays within the limit.
    let vpath = app.vault.as_ref().unwrap().doc_path(&id.unwrap()).unwrap_or_default();
    assert!(vpath.len() <= crate::storage::MAX_PATH_LEN, "path within limit: {} bytes", vpath.len());
    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn detach_skips_blob_reclaim_when_save_fails_keeping_vault_openable() {
    // The cross-confirmed HIGH fix: if the vault save fails (full disk), the
    // blob reclaim must be SKIPPED, or the on-disk vault would reference a
    // dropped doc (ArchiveMismatch -> unopenable). Here the doc must survive.
    let (mut app, path) = app_unlocked("faildetach");
    let src = std::env::temp_dir().join(format!("vaultis-faild-{}.txt", nanos()));
    std::fs::write(&src, b"statement body").unwrap();
    let id = {
        let ov = app.vault.as_mut().unwrap();
        let id = ov.add_document("/a", "stmt.txt", std::path::Path::new(&src)).unwrap();
        let mut a = AssetLiability::new().unwrap();
        a.statement = Some(id.clone());
        records::upsert(&mut ov.vault.assets, a.clone());
        ov.save().unwrap();
        app.edit_asset = Some(a);
        id
    };
    // Detach with the disk full at the vault save.
    crate::fault::fail_at("vault.write", 1);
    app.handle_doc(DocReq::Remove, DocTarget::Asset);
    crate::fault::clear();
    assert!(app.status.contains("Save failed"), "status was: {}", app.status);
    drop(app); // release the lock
    // The save failed, so the on-disk vault still references the doc; because the
    // reclaim was skipped, the doc is still present -> the vault opens cleanly.
    let re = OpenVault::open(path.clone(), b"a", b"b").unwrap();
    assert!(re.has_document(&id), "blob retained; vault openable");
    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn delete_current_removes_record_and_reclaims_blob() {
    let (mut app, path) = app_unlocked("del");
    let src = std::env::temp_dir().join(format!("vaultis-guidel-{}.txt", nanos()));
    std::fs::write(&src, b"stmt").unwrap();
    // Build an asset with an attached statement, saved into the vault.
    let id = app.vault.as_mut().unwrap().add_document("/s", "s.txt", std::path::Path::new(&src)).unwrap();
    let mut a = AssetLiability::new().unwrap();
    a.statement = Some(id.clone());
    records::upsert(&mut app.vault.as_mut().unwrap().vault.assets, a.clone());
    app.edit_asset = Some(a);

    app.delete_current(Tab::Assets);
    assert!(app.vault.as_ref().unwrap().vault.assets.is_empty());
    assert!(app.vault.as_ref().unwrap().read_document(&id).is_err(), "blob reclaimed");

    let _ = std::fs::remove_file(&src);
    cleanup(&path);
}

#[test]
fn handle_link_req_adds_and_removes_links_in_edit_buffer_only() {
    let (mut app, path) = app_unlocked("guilinkreq");
    let acct_id = {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Brokerage".into();
        a.owner = "Jane".into();
        let id = a.id.clone();
        records::upsert(&mut ov.vault.accounts, a);
        id
    };
    let mut asset = AssetLiability::new().unwrap();
    asset.owner = "Jane".into();
    asset.approx_value = "10".into();
    app.edit_asset = Some(asset);

    // Add appends to the WORKING BUFFER; a duplicate request is dropped (the
    // dropdown never offers one, but the deferred handler re-checks anyway).
    app.handle_link_req(LinkReq::Add(acct_id.clone()));
    app.handle_link_req(LinkReq::Add(acct_id.clone()));
    assert_eq!(app.edit_asset.as_ref().unwrap().linked_accounts, vec![acct_id.clone()]);
    assert!(
        app.vault.as_ref().unwrap().vault.assets.is_empty(),
        "no direct vault write — the link persists through the ordinary Save path"
    );
    // An out-of-range Remove (stale index) is dropped, not a panic; in-range unlinks.
    app.handle_link_req(LinkReq::Remove(5));
    assert_eq!(app.edit_asset.as_ref().unwrap().linked_accounts.len(), 1);
    app.handle_link_req(LinkReq::Remove(0));
    assert!(app.edit_asset.as_ref().unwrap().linked_accounts.is_empty(), "unlinked");
    cleanup(&path);
}

#[test]
fn confirm_pending_account_delete_only_fires_when_the_armed_id_still_matches() {
    let (mut app, path) = app_unlocked("guiconfirmrace");
    let (x_id, y_id) = {
        let ov = app.vault.as_mut().unwrap();
        let mut x = Account::new().unwrap();
        x.title = "Linked".into();
        let mut y = Account::new().unwrap();
        y.title = "Neighbor".into();
        let ids = (x.id.clone(), y.id.clone());
        records::upsert(&mut ov.vault.accounts, x);
        records::upsert(&mut ov.vault.accounts, y);
        ids
    };

    // The raced-confirm shape: the same-frame select handler swapped the
    // editor to the NEIGHBOR and disarmed pending before the captured click
    // is applied. The stale confirm must delete nothing.
    app.edit_account =
        app.vault.as_ref().unwrap().vault.accounts.iter().find(|a| a.id == y_id).cloned();
    app.pending_account_delete = None;
    app.confirm_pending_account_delete();
    assert_eq!(app.vault.as_ref().unwrap().vault.accounts.len(), 2, "raced confirm dropped");

    // Belt-and-braces: an armed id that no longer matches the loaded record
    // (any other way the two could diverge) is also dropped — and disarmed.
    app.pending_account_delete = Some(x_id.clone());
    app.confirm_pending_account_delete();
    assert_eq!(app.vault.as_ref().unwrap().vault.accounts.len(), 2, "mismatched confirm dropped");
    assert!(app.pending_account_delete.is_none(), "always disarmed");

    // The legitimate path: armed id matches the loaded record -> deletes it.
    app.edit_account =
        app.vault.as_ref().unwrap().vault.accounts.iter().find(|a| a.id == x_id).cloned();
    app.pending_account_delete = Some(x_id.clone());
    app.confirm_pending_account_delete();
    let accounts = &app.vault.as_ref().unwrap().vault.accounts;
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, y_id, "exactly the armed record was deleted");
    cleanup(&path);
}

#[test]
fn opening_a_vault_clears_prior_per_vault_ui_state() {
    // Audit 2026-07-03 A-8: a fresh open must not inherit the previous session's edit
    // buffers (which can hold cleartext secrets), armed delete, active filters/search, or
    // reveal toggles — otherwise an error path that drops to the unlock screen without going
    // through the constructor leaks vault A's state into vault B.
    let (mut app, path) = app_unlocked("resetui");
    // Dirty a spread of per-vault state as a prior session would leave it.
    let mut a = Account::new().unwrap();
    a.password = "leaky-secret".into();
    app.edit_account = Some(a);
    app.pending_account_delete = Some("armed-id".into());
    app.acct_filter_owner = "Alice".into();
    app.acct_search_user = "bob".into();
    app.reveal_all = true;
    app.tab = Tab::Accounts;

    app.reset_per_vault_ui_state();

    assert!(app.edit_account.is_none(), "edit buffer cleared (its secret is wiped on drop)");
    assert!(app.pending_account_delete.is_none(), "armed delete disarmed");
    assert!(app.acct_filter_owner.is_empty(), "filters cleared");
    assert!(app.acct_search_user.is_empty(), "search cleared");
    assert_eq!(app.reveal_all, app.reveal_default, "reveal back to the saved default");
    assert!(matches!(app.tab, Tab::Urgent), "tab back to the first (URGENT)");
    cleanup(&path);
}

#[cfg(feature = "fault-injection")]
#[test]
fn delete_rollback_restores_the_saved_record_not_the_dirty_edit_buffer() {
    // Audit 2026-07-03 A-6: on a failed-persist rollback the vault must be restored to the
    // record's LAST-SAVED state, never the (possibly edited) edit buffer — otherwise a delete
    // the user was told FAILED would silently commit the buffer's unsaved edits on a later save.
    let (mut app, path) = app_unlocked("delrollback");
    let id = {
        let ov = app.vault.as_mut().unwrap();
        let mut a = Account::new().unwrap();
        a.title = "Bank".into();
        a.password = "saved-pw".into();
        let id = a.id.clone();
        records::upsert(&mut ov.vault.accounts, a);
        ov.save().unwrap();
        id
    };
    // Load into the editor, then make an UNSAVED edit to the buffer.
    app.edit_account = app.vault.as_ref().unwrap().vault.accounts.iter().find(|a| a.id == id).cloned();
    app.edit_account.as_mut().unwrap().password = "unsaved-pw".into();
    // Delete with the vault save forced to fail (full disk).
    crate::fault::fail_at("vault.write", 1);
    app.delete_current(Tab::Accounts);
    crate::fault::clear();
    assert!(app.status.contains("Save failed"), "status: {}", app.status);
    // The record is back (rollback fired) with the SAVED password — the unsaved edit is gone.
    let accounts = &app.vault.as_ref().unwrap().vault.accounts;
    assert_eq!(accounts.len(), 1, "record restored by rollback");
    assert_eq!(accounts[0].password, "saved-pw", "restored the SAVED state, not the dirty buffer");
    cleanup(&path);
}

#[test]
fn open_linked_account_jumps_with_tab_switch_resets_and_filter_follow() {
    let (mut app, path) = app_unlocked("guilinkjump");
    let mut a = Account::new().unwrap();
    a.title = "Brokerage".into();
    a.owner = "Jane".into();
    a.account_type = "Financial".into();
    let acct_id = a.id.clone();
    records::upsert(&mut app.vault.as_mut().unwrap().vault.accounts, a);

    app.tab = Tab::Assets;
    app.reveal_default = false;
    app.reveal_all = true; // momentary reveal left on — must not leak into Accounts
    app.doc_filename = "half-typed.pdf".into(); // shared doc buffer — must not linger
    app.acct_filter_type = "Email".into(); // active filter that would hide the target
    app.acct_filter_review = true; // review-only would hide the (unflagged) target
    app.open_linked_account(&acct_id);
    assert!(app.tab == Tab::Accounts, "navigated to the Accounts tab");
    assert_eq!(app.edit_account.as_ref().unwrap().id, acct_id, "target loaded in the editor");
    assert!(!app.reveal_all, "programmatic switch performs ui_top_bar's reveal reset");
    assert!(app.doc_filename.is_empty(), "doc inputs cleared like a real tab switch");
    assert_eq!(app.acct_filter_type, "Financial", "active filter retargeted to the record");
    assert!(!app.acct_filter_review, "review-only relaxed so the target is visible");

    // A dangling link: a status message, and NO navigation.
    app.tab = Tab::Assets;
    app.open_linked_account("gone");
    assert!(app.tab == Tab::Assets, "no navigation for a dangling link");
    assert!(app.status.contains("not found"), "status: {}", app.status);
    cleanup(&path);
}

#[test]
fn open_linking_asset_jumps_back_and_clears_hiding_review_filter() {
    let (mut app, path) = app_unlocked("guilinkback");
    let mut asset = AssetLiability::new().unwrap();
    asset.owner = "Jane".into();
    asset.approx_value = "10".into();
    let asset_id = asset.id.clone();
    records::upsert(&mut app.vault.as_mut().unwrap().vault.assets, asset);

    app.tab = Tab::Accounts;
    app.reveal_default = false;
    app.reveal_all = true;
    app.asset_filter_review = true; // would hide the (unflagged) jump target
    app.open_linking_asset(&asset_id);
    assert!(app.tab == Tab::Assets, "navigated back to the Assets tab");
    assert_eq!(app.edit_asset.as_ref().unwrap().id, asset_id, "target loaded in the editor");
    assert!(!app.reveal_all, "programmatic switch re-masks to the saved default");
    assert!(!app.asset_filter_review, "review-only cleared so the target is visible");
    cleanup(&path);
}

#[test]
fn account_delete_link_warning_states_count_and_consequence() {
    assert_eq!(account_delete_link_warning(0), None, "unlinked accounts delete unwarned, as before");
    let msg = account_delete_link_warning(3).unwrap();
    assert!(msg.contains("linked from 3 asset/liability record(s)"), "count stated: {msg}");
    assert!(msg.contains("unresolved ids"), "consequence stated (links kept, no cascade): {msg}");
}

#[test]
fn linked_account_rows_resolve_labels_and_render_dangling_raw_ids() {
    let mut a = Account::new().unwrap();
    a.title = "Brokerage".into();
    a.account_type = "Financial".into();
    a.username = "jane".into();
    let accounts = vec![a.clone()];
    let rows = linked_account_rows(&accounts, &[a.id.clone(), "gone".into()]);
    assert_eq!(rows[0], (a.id.clone(), a.label()), "live link resolves to the display label");
    assert_eq!(
        rows[1],
        ("gone".to_string(), "gone".to_string()),
        "dangling link renders the RAW id — tolerant, nothing hidden"
    );
    // The add-dropdown offers only the not-yet-linked accounts.
    assert!(link_candidates(&accounts, &[a.id.clone()]).is_empty(), "already linked → not offered");
    assert_eq!(link_candidates(&accounts, &[]).len(), 1, "unlinked account is offered");
}

#[test]
fn change_password_via_auth_rekeys() {
    let (mut app, path) = app_unlocked("rekey");
    app.auth_mode = AuthMode::ChangePassword;
    app.pw1 = "c".into();
    app.confirm1 = "c".into();
    app.pw2 = "d".into();
    app.confirm2 = "d".into();
    app.submit_auth();
    assert!(app.screen == Screen::Main);
    drop(app); // release the single-writer lock before reopening
    // Reopens only with the new passwords.
    assert!(OpenVault::open(path.clone(), b"a", b"b").is_err());
    assert!(OpenVault::open(path.clone(), b"c", b"d").is_ok());
    cleanup(&path);
}

#[test]
fn merge_preview_then_apply_updates_vault_and_copies_blob() {
    use crate::records;
    // SOURCE vault in its own dir, with a newer shared account + a doc-bearing record.
    let s_path = tmp("merge-gui-src");
    let s_dir = s_path.parent().unwrap().to_path_buf();
    let blob_id;
    {
        let mut s = OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
        let mut a = records::Account::new().unwrap();
        a.id = "shared".into();
        a.title = "Shared".into();
        a.owner = "o".into();
        a.account_type = "Checking".into();
        a.username = "alice".into();
        a.password = "NEWPW".into();
        a.updated_at = 2_000;
        s.vault.accounts.push(a);
        let f = std::env::temp_dir().join(format!("pmgui-doc-{}.txt", nanos()));
        std::fs::write(&f, b"deed-bytes").unwrap();
        blob_id = s.add_document("general-documents/deed", "deed.pdf", &f).unwrap();
        let mut gd = records::GeneralDocument::new().unwrap();
        gd.id = "gd-1".into();
        gd.title = "Deed".into();
        gd.file = Some(blob_id.clone());
        gd.updated_at = 3_000;
        s.vault.general_documents.push(gd);
        s.save().unwrap();
    }

    // CURRENT vault (writable, open) with an OLDER version of the shared account.
    let (mut app, c_path) = app_unlocked("merge-gui-cur");
    {
        let cur = app.vault.as_mut().unwrap();
        let mut a = records::Account::new().unwrap();
        a.id = "shared".into();
        a.title = "Shared".into();
        a.owner = "o".into();
        a.account_type = "Checking".into();
        a.username = "alice".into();
        a.password = "OLDPW".into();
        a.updated_at = 1_000;
        cur.vault.accounts.push(a);
        cur.save().unwrap();
    }

    // PREVIEW: enter the source folder + its passwords.
    app.merge_src_dir = s_dir.display().to_string();
    app.merge_pw1 = "s1".into();
    app.merge_pw2 = "s2".into();
    app.merge_preview();
    assert!(app.merge_error.is_none(), "preview error: {:?}", app.merge_error);
    let plan = app.merge_plan.as_ref().expect("a plan was built");
    assert_eq!(plan.updated_count(), 1, "shared account is newer in source");
    assert_eq!(plan.new_count(), 1, "the general document is new");
    assert_eq!(plan.blobs_to_copy(), 1);
    // Passwords were wiped after a successful preview.
    assert!(app.merge_pw1.is_empty() && app.merge_pw2.is_empty());

    // APPLY.
    app.merge_apply();
    assert!(app.merge_plan.is_none(), "merge state cleared after apply");
    assert_eq!(app.screen, Screen::Config);
    assert!(app.status.contains("Updated from another vault"), "status: {}", app.status);
    let cur = app.vault.as_ref().unwrap();
    assert_eq!(cur.vault.accounts.iter().find(|a| a.id == "shared").unwrap().password, "NEWPW");
    assert_eq!(&**cur.read_document(&blob_id).unwrap(), b"deed-bytes");

    cleanup(&s_path);
    cleanup(&c_path);
}

#[test]
fn merge_preview_wrong_password_gives_generic_error() {
    let s_path = tmp("merge-gui-badpw-src");
    let s_dir = s_path.parent().unwrap().to_path_buf();
    {
        OpenVault::create(s_path.clone(), b"s1", b"s2", fast()).unwrap();
    }
    let (mut app, c_path) = app_unlocked("merge-gui-badpw-cur");
    app.merge_src_dir = s_dir.display().to_string();
    app.merge_pw1 = "wrong".into();
    app.merge_pw2 = "wrong".into();
    app.merge_preview();
    assert!(app.merge_plan.is_none());
    // One generic message — never confirms whether the password was right (oracle-safe).
    let err = app.merge_error.as_deref().unwrap_or("");
    assert!(err.contains("wrong password(s) or unreadable"), "got: {err:?}");
    cleanup(&s_path);
    cleanup(&c_path);
}
