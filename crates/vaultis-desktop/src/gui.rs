//! Graphical interface (egui/eframe): a tabbed estate vault.
//!
//! Five tabs map to the five record types in [`crate::records`]; each tab is a
//! list of records on the left and an edit form on the right. The Trust & Will
//! and Asset/Liability tabs can attach documents, which are uploaded into the
//! encrypted volume via [`OpenVault::add_document`].
//!
//! egui is immediate-mode, so all vault-mutating side effects (save, delete,
//! attach, …) are recorded as flags while rendering and applied *after* the
//! panel closures return, which keeps borrows of `self` disjoint and simple.
//!
//! Rust orientation for non-Rust readers of this file:
//! - `&T` is a *shared* (read-only) borrow of a value; `&mut T` is an
//!   *exclusive* (read/write) borrow. Rust allows many `&T` xor one `&mut T` at
//!   a time, which is why this file defers writes (see above).
//! - `String` is an owned, growable, heap-allocated UTF-8 string; `&str` is a
//!   borrowed string slice (a view into a `String` or a literal).
//! - `Option<T>` is "maybe a T": `Some(x)` or `None`. `Result<T, E>` is
//!   "success `Ok(x)` or failure `Err(e)`". The `?` operator early-returns the
//!   error/`None` from the enclosing function. `.unwrap()`/`.expect("msg")`
//!   extract the inner value but *panic* (abort) if it is absent.
//! - "Closures" are inline anonymous functions written `|args| body`; egui's
//!   `.show(ui, |ui| { ... })` calls our closure to draw a panel's contents.

use std::path::Path;
use std::time::{Duration, Instant};

// `use` brings names into scope (like an import). `eframe`/`egui` are the
// GUI framework; `zeroize` provides helpers that wipe secrets from memory.
use eframe::egui;
// `Zeroize` is a trait giving values a `.zeroize()` method (overwrite with
// zeros); `Zeroizing<T>` is a wrapper that auto-zeroes its contents on drop.
use zeroize::{Zeroize, Zeroizing};

use crate::csv;
use crate::password::{self, GenOptions};
use crate::records::{
    self, Account, AssetLiability, GeneralDocument, Instruction, RealEstate, Record, TaxFiling, TrustWill, Urgent,
};
use crate::ui::format_time;
use crate::vault::{self, CategoryRemoval, OpenVault, VaultError};

/// Launch the graphical app and block until the window is closed. `writable`
/// enables mutations; when false the vault is opened read-only and write
/// controls are hidden.
///
/// `pub` makes this callable from outside this module. `PathBuf` is an owned,
/// heap-allocated filesystem path (the borrowed view is `&Path`). The return
/// type `anyhow::Result<()>` means "succeeds with the empty value `()` or fails
/// with a boxed error".
pub fn run(path: std::path::PathBuf, writable: bool) -> anyhow::Result<()> {
    // Single-instance guard: if a window for this vault is already open, ask it to
    // come to the front and exit instead of stacking another window the user would
    // have to close one by one (see `crate::single_instance`). `_guard` holds an OS
    // lock for the lifetime of this function — i.e. the whole GUI session — and
    // releases it on return; `focus` is moved into the creation closure so later
    // launches can raise this window.
    let (_guard, focus) = match crate::single_instance::acquire(&path) {
        crate::single_instance::Instance::AlreadyRunning => {
            eprintln!("vaultis is already open for this vault; raising the existing window.");
            return Ok(());
        }
        crate::single_instance::Instance::Primary { guard, focus } => (guard, focus),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 680.0])
            // The floor is set so the non-scrolling lock screen (its tallest variant,
            // Create, with the two confirm rows) always fits whole: ~560 wide for the
            // centered card plus margins, ~600 tall for logo + vault picker + four
            // password rows + button. In-vault tabs scroll their own panes, so they are
            // comfortable at this size too.
            .with_min_inner_size(MIN_INNER_SIZE)
            .with_title("vaultis")
            // `with_icon` takes IconData directly; a decode failure yields None and the
            // platform default, so a bad asset can never stop the window opening.
            .with_icon(window_icon().unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native(
        "vaultis",
        options,
        // `Box::new(...)` heap-allocates; `Box<T>` is an owning pointer to a
        // heap value. `move |cc| ...` is a closure that *takes ownership* of the
        // captured `path`/`writable`/`focus` (the `move` keyword) so they outlive `run`.
        Box::new(move |cc| {
            // Now that the egui context exists, let later launches raise this window.
            focus.serve(cc.egui_ctx.clone());
            // Resolve the start-page root the same way `GuiApp::new` will, so the saved
            // look can be read from `<vault_root>/prefs.json` BEFORE the first frame (a
            // flash of the default theme otherwise). When no root is known yet the start
            // page opens empty and these simply fall back to the built-in defaults; the
            // app re-applies all three live once a root is chosen.
            let boot_root =
                crate::launch::initial_root_and_name(&path, crate::launch::load_last_root().as_deref()).0;
            apply_theme(&cc.egui_ctx, load_theme(&boot_root));
            // Same reason as the theme: apply the saved zoom before the first frame so
            // the window does not visibly resize itself a frame after opening.
            apply_ui_scale(&cc.egui_ctx, load_ui_scale(&boot_root));
            apply_fonts(&cc.egui_ctx, load_font_choice(&boot_root));
            Ok(Box::new(GuiApp::new(path, writable)))
        }),
    )
    // `.map_err(|e| ...)` transforms only the error case of a `Result`; here it
    // wraps eframe's error into an `anyhow` error with context.
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}

/// A light egui theme — brighter than the default light visuals (panels and
/// widget faces lifted toward white for a lighter overall feel).
fn light_visuals() -> egui::Visuals {
    // `let mut v` declares a mutable local; without `mut`, bindings are
    // read-only in Rust. We tweak fields of the default light theme below.
    let mut v = egui::Visuals::light();
    v.panel_fill = egui::Color32::from_rgb(252, 253, 255);
    v.window_fill = egui::Color32::from_rgb(255, 255, 255);
    v.extreme_bg_color = egui::Color32::from_rgb(255, 255, 255);
    v.faint_bg_color = egui::Color32::from_rgb(248, 250, 253);
    // Lift the widget backgrounds (inactive/hovered/active) so controls read lighter.
    v.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(250, 251, 253);
    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(244, 247, 251);
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(248, 250, 253);
    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(232, 240, 252);
    v.selection.bg_fill = egui::Color32::from_rgb(198, 222, 255);
    v.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(40, 90, 170));
    v
}

/// The selectable GUI color themes (curated palettes). The chosen theme is
/// remembered in a small **non-secret** preferences file (`load_theme`/`save_theme`)
/// — it holds no vault data, so it can apply on the lock screen too.
#[derive(PartialEq, Eq, Clone, Copy, Default, Debug)]
enum Theme {
    #[default]
    Light,
    Dark,
    HighContrast,
    Solarized,
    Sepia,
    Nord,
    Dracula,
    GruvboxDark,
    GruvboxLight,
    RosePine,
    CatppuccinMocha,
    CatppuccinLatte,
    TokyoNight,
    OneDark,
    Everforest,
    Zenburn,
}

impl Theme {
    /// Every theme, in menu order.
    const ALL: [Theme; 16] = [
        Theme::Light,
        Theme::Dark,
        Theme::HighContrast,
        Theme::Solarized,
        Theme::Sepia,
        Theme::Nord,
        Theme::Dracula,
        Theme::GruvboxDark,
        Theme::GruvboxLight,
        Theme::RosePine,
        Theme::CatppuccinMocha,
        Theme::CatppuccinLatte,
        Theme::TokyoNight,
        Theme::OneDark,
        Theme::Everforest,
        Theme::Zenburn,
    ];

    /// Stable on-disk id (kept separate from the display label so relabelling
    /// never invalidates a saved preference).
    fn id(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::HighContrast => "high-contrast",
            Theme::Solarized => "solarized",
            Theme::Sepia => "sepia",
            Theme::Nord => "nord",
            Theme::Dracula => "dracula",
            Theme::GruvboxDark => "gruvbox-dark",
            Theme::GruvboxLight => "gruvbox-light",
            Theme::RosePine => "rose-pine",
            Theme::CatppuccinMocha => "catppuccin-mocha",
            Theme::CatppuccinLatte => "catppuccin-latte",
            Theme::TokyoNight => "tokyo-night",
            Theme::OneDark => "one-dark",
            Theme::Everforest => "everforest",
            Theme::Zenburn => "zenburn",
        }
    }

    /// Human-readable name for the dropdown.
    fn label(self) -> &'static str {
        match self {
            Theme::Light => "Light",
            Theme::Dark => "Dark",
            Theme::HighContrast => "High contrast",
            Theme::Solarized => "Solarized",
            Theme::Sepia => "Sepia",
            Theme::Nord => "Nord",
            Theme::Dracula => "Dracula",
            Theme::GruvboxDark => "Gruvbox Dark",
            Theme::GruvboxLight => "Gruvbox Light",
            Theme::RosePine => "Rosé Pine",
            Theme::CatppuccinMocha => "Catppuccin Mocha",
            Theme::CatppuccinLatte => "Catppuccin Latte",
            Theme::TokyoNight => "Tokyo Night",
            Theme::OneDark => "One Dark",
            Theme::Everforest => "Everforest",
            Theme::Zenburn => "Zenburn",
        }
    }

    /// Parse a saved id back into a theme (`None` for an unknown id).
    fn from_id(id: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// Build the egui visuals for a theme. Each curated palette starts from egui's
/// light or dark base and overrides the panel/widget fills, the text color, and
/// the selection color for a coherent look.
fn visuals_for(theme: Theme) -> egui::Visuals {
    use egui::Color32;
    let rgb = Color32::from_rgb;
    match theme {
        Theme::Light => light_visuals(),
        Theme::Dark => {
            let mut v = egui::Visuals::dark();
            v.selection.bg_fill = rgb(40, 80, 140);
            v.selection.stroke = egui::Stroke::new(1.0_f32, rgb(120, 170, 240));
            v.hyperlink_color = rgb(110, 170, 240);
            v
        }
        Theme::HighContrast => {
            let mut v = egui::Visuals::dark();
            v.panel_fill = Color32::BLACK;
            v.window_fill = Color32::BLACK;
            v.extreme_bg_color = Color32::BLACK;
            v.faint_bg_color = rgb(18, 18, 18);
            v.override_text_color = Some(Color32::WHITE);
            v.widgets.noninteractive.bg_fill = rgb(14, 14, 14);
            v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.4_f32, Color32::WHITE);
            v.widgets.inactive.bg_fill = rgb(32, 32, 32);
            v.widgets.inactive.weak_bg_fill = rgb(24, 24, 24);
            v.widgets.hovered.bg_fill = rgb(64, 64, 64);
            v.widgets.active.bg_fill = rgb(0, 120, 215);
            v.selection.bg_fill = rgb(0, 90, 180);
            v.selection.stroke = egui::Stroke::new(1.2_f32, rgb(140, 200, 255));
            v.hyperlink_color = rgb(120, 200, 255);
            v
        }
        Theme::Solarized => {
            // Ethan Schoonover's Solarized Dark palette.
            let base03 = rgb(0, 43, 54);
            let base02 = rgb(7, 54, 66);
            let base01 = rgb(88, 110, 117);
            let base1 = rgb(147, 161, 161);
            let blue = rgb(38, 139, 210);
            let mut v = egui::Visuals::dark();
            v.panel_fill = base03;
            v.window_fill = base03;
            v.extreme_bg_color = rgb(0, 33, 43);
            v.faint_bg_color = base02;
            v.override_text_color = Some(base1);
            v.widgets.noninteractive.bg_fill = base02;
            v.widgets.inactive.bg_fill = base02;
            v.widgets.inactive.weak_bg_fill = base02;
            v.widgets.hovered.bg_fill = base01;
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = base01;
            v.selection.stroke = egui::Stroke::new(1.0_f32, blue);
            v.hyperlink_color = blue;
            v
        }
        Theme::Sepia => {
            // Warm, paper-like light theme.
            let ink = rgb(60, 46, 33);
            let mut v = egui::Visuals::light();
            v.panel_fill = rgb(244, 236, 216);
            v.window_fill = rgb(250, 244, 228);
            v.extreme_bg_color = rgb(252, 248, 236);
            v.faint_bg_color = rgb(240, 231, 210);
            v.override_text_color = Some(ink);
            v.widgets.noninteractive.bg_fill = rgb(243, 234, 213);
            v.widgets.inactive.bg_fill = rgb(236, 226, 203);
            v.widgets.inactive.weak_bg_fill = rgb(243, 234, 213);
            v.widgets.hovered.bg_fill = rgb(226, 212, 182);
            v.selection.bg_fill = rgb(214, 196, 158);
            v.selection.stroke = egui::Stroke::new(1.0_f32, rgb(120, 90, 50));
            v
        }
        Theme::Nord => {
            // Nord — cool, muted polar palette.
            let (bg, bg2, bg3) = (rgb(46, 52, 64), rgb(59, 66, 82), rgb(67, 76, 94));
            let (txt, frost, blue) = (rgb(216, 222, 233), rgb(136, 192, 208), rgb(129, 161, 193));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = rgb(38, 43, 54);
            v.faint_bg_color = bg2;
            v.override_text_color = Some(txt);
            v.widgets.noninteractive.bg_fill = bg2;
            v.widgets.inactive.bg_fill = bg2;
            v.widgets.inactive.weak_bg_fill = bg2;
            v.widgets.hovered.bg_fill = bg3;
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = bg3;
            v.selection.stroke = egui::Stroke::new(1.0_f32, frost);
            v.hyperlink_color = frost;
            v
        }
        Theme::Dracula => {
            // Dracula — dark with vivid purple/cyan accents.
            let (bg, panel, sel) = (rgb(40, 42, 54), rgb(48, 50, 64), rgb(68, 71, 90));
            let (fg, purple, cyan) = (rgb(248, 248, 242), rgb(189, 147, 249), rgb(139, 233, 253));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = rgb(33, 34, 44);
            v.faint_bg_color = panel;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = panel;
            v.widgets.inactive.bg_fill = panel;
            v.widgets.inactive.weak_bg_fill = panel;
            v.widgets.hovered.bg_fill = sel;
            v.widgets.active.bg_fill = purple;
            v.selection.bg_fill = sel;
            v.selection.stroke = egui::Stroke::new(1.0_f32, purple);
            v.hyperlink_color = cyan;
            v
        }
        Theme::GruvboxDark => {
            // Gruvbox — warm retro dark.
            let (bg, bg1, bg2) = (rgb(40, 40, 40), rgb(60, 56, 54), rgb(80, 73, 69));
            let (fg, orange, aqua) = (rgb(235, 219, 178), rgb(254, 128, 25), rgb(142, 192, 124));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = rgb(29, 32, 33);
            v.faint_bg_color = bg1;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = bg1;
            v.widgets.inactive.bg_fill = bg1;
            v.widgets.inactive.weak_bg_fill = bg1;
            v.widgets.hovered.bg_fill = bg2;
            v.widgets.active.bg_fill = orange;
            v.selection.bg_fill = bg2;
            v.selection.stroke = egui::Stroke::new(1.0_f32, aqua);
            v.hyperlink_color = aqua;
            v
        }
        Theme::GruvboxLight => {
            // Gruvbox — warm retro light.
            let (bg, bg1, bg2) = (rgb(251, 241, 199), rgb(235, 219, 178), rgb(213, 196, 161));
            let (fg, orange) = (rgb(60, 56, 54), rgb(214, 93, 14));
            let mut v = egui::Visuals::light();
            v.panel_fill = bg;
            v.window_fill = rgb(249, 245, 215);
            v.extreme_bg_color = rgb(252, 248, 227);
            v.faint_bg_color = bg1;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = bg1;
            v.widgets.inactive.bg_fill = bg1;
            v.widgets.inactive.weak_bg_fill = bg1;
            v.widgets.hovered.bg_fill = bg2;
            v.widgets.active.bg_fill = orange;
            v.selection.bg_fill = bg2;
            v.selection.stroke = egui::Stroke::new(1.0_f32, rgb(175, 58, 3));
            v
        }
        Theme::CatppuccinMocha => {
            // Catppuccin Mocha — the widely-used warm pastel dark palette.
            let (base, mantle, surface) = (rgb(30, 30, 46), rgb(24, 24, 37), rgb(49, 50, 68));
            let (text, blue, teal) = (rgb(205, 214, 244), rgb(137, 180, 250), rgb(148, 226, 213));
            let mut v = egui::Visuals::dark();
            v.panel_fill = base;
            v.window_fill = base;
            v.extreme_bg_color = mantle;
            v.faint_bg_color = surface;
            v.override_text_color = Some(text);
            v.widgets.noninteractive.bg_fill = surface;
            v.widgets.inactive.bg_fill = surface;
            v.widgets.inactive.weak_bg_fill = mantle;
            v.widgets.hovered.bg_fill = rgb(69, 71, 90);
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = rgb(69, 71, 90);
            v.selection.stroke = egui::Stroke::new(1.0_f32, teal);
            v.hyperlink_color = teal;
            v
        }
        Theme::CatppuccinLatte => {
            // Catppuccin Latte — the light member of the same family.
            let (base, crust, surface) = (rgb(239, 241, 245), rgb(220, 224, 232), rgb(204, 208, 218));
            let (text, blue) = (rgb(76, 79, 105), rgb(30, 102, 245));
            let mut v = egui::Visuals::light();
            v.panel_fill = base;
            v.window_fill = rgb(245, 247, 250);
            v.extreme_bg_color = Color32::WHITE;
            v.faint_bg_color = rgb(230, 233, 239);
            v.override_text_color = Some(text);
            v.widgets.noninteractive.bg_fill = rgb(235, 238, 243);
            v.widgets.inactive.bg_fill = rgb(228, 232, 239);
            v.widgets.inactive.weak_bg_fill = rgb(236, 239, 244);
            v.widgets.hovered.bg_fill = surface;
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = rgb(188, 208, 245);
            v.selection.stroke = egui::Stroke::new(1.0_f32, blue);
            v.hyperlink_color = blue;
            v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, crust);
            v
        }
        Theme::TokyoNight => {
            // Tokyo Night — deep blue-black with cool accents.
            let (bg, bg_dark, bg_hl) = (rgb(26, 27, 38), rgb(22, 22, 30), rgb(41, 46, 66));
            let (fg, blue, cyan) = (rgb(192, 202, 245), rgb(122, 162, 247), rgb(125, 207, 255));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = bg_dark;
            v.faint_bg_color = bg_hl;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = bg_hl;
            v.widgets.inactive.bg_fill = bg_hl;
            v.widgets.inactive.weak_bg_fill = bg_dark;
            v.widgets.hovered.bg_fill = rgb(54, 60, 84);
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = rgb(54, 60, 84);
            v.selection.stroke = egui::Stroke::new(1.0_f32, cyan);
            v.hyperlink_color = cyan;
            v
        }
        Theme::OneDark => {
            // Atom One Dark — the familiar editor palette.
            let (bg, bg_dark, gutter) = (rgb(40, 44, 52), rgb(33, 37, 43), rgb(49, 54, 63));
            let (fg, blue, green) = (rgb(171, 178, 191), rgb(97, 175, 239), rgb(152, 195, 121));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = bg_dark;
            v.faint_bg_color = gutter;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = gutter;
            v.widgets.inactive.bg_fill = gutter;
            v.widgets.inactive.weak_bg_fill = bg_dark;
            v.widgets.hovered.bg_fill = rgb(62, 68, 81);
            v.widgets.active.bg_fill = blue;
            v.selection.bg_fill = rgb(62, 68, 81);
            v.selection.stroke = egui::Stroke::new(1.0_f32, green);
            v.hyperlink_color = blue;
            v
        }
        Theme::Everforest => {
            // Everforest Dark — low-saturation green, easy on the eyes for long reading.
            let (bg, bg_dim, bg1) = (rgb(45, 53, 59), rgb(35, 42, 46), rgb(52, 63, 68));
            let (fg, green, aqua) = (rgb(211, 198, 170), rgb(167, 192, 128), rgb(131, 192, 146));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = bg_dim;
            v.faint_bg_color = bg1;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = bg1;
            v.widgets.inactive.bg_fill = bg1;
            v.widgets.inactive.weak_bg_fill = bg_dim;
            v.widgets.hovered.bg_fill = rgb(61, 72, 77);
            v.widgets.active.bg_fill = green;
            v.selection.bg_fill = rgb(61, 72, 77);
            v.selection.stroke = egui::Stroke::new(1.0_f32, aqua);
            v.hyperlink_color = aqua;
            v
        }
        Theme::Zenburn => {
            // Zenburn — the classic low-contrast warm grey palette.
            let (bg, bg_dark, bg_mid) = (rgb(63, 63, 63), rgb(51, 51, 51), rgb(79, 79, 79));
            let (fg, cyan, yellow) = (rgb(220, 220, 204), rgb(140, 208, 211), rgb(240, 223, 175));
            let mut v = egui::Visuals::dark();
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = bg_dark;
            v.faint_bg_color = bg_mid;
            v.override_text_color = Some(fg);
            v.widgets.noninteractive.bg_fill = bg_mid;
            v.widgets.inactive.bg_fill = bg_mid;
            v.widgets.inactive.weak_bg_fill = bg_dark;
            v.widgets.hovered.bg_fill = rgb(95, 95, 95);
            v.widgets.active.bg_fill = cyan;
            v.selection.bg_fill = rgb(95, 95, 95);
            v.selection.stroke = egui::Stroke::new(1.0_f32, yellow);
            v.hyperlink_color = cyan;
            v
        }
        Theme::RosePine => {
            // Rosé Pine — soft, moody low-contrast dark.
            let (base, surface, overlay) = (rgb(25, 23, 36), rgb(31, 29, 46), rgb(38, 35, 58));
            let (text, iris, foam) = (rgb(224, 222, 244), rgb(196, 167, 231), rgb(156, 207, 216));
            let mut v = egui::Visuals::dark();
            v.panel_fill = base;
            v.window_fill = base;
            v.extreme_bg_color = rgb(20, 18, 30);
            v.faint_bg_color = surface;
            v.override_text_color = Some(text);
            v.widgets.noninteractive.bg_fill = surface;
            v.widgets.inactive.bg_fill = surface;
            v.widgets.inactive.weak_bg_fill = surface;
            v.widgets.hovered.bg_fill = overlay;
            v.widgets.active.bg_fill = iris;
            v.selection.bg_fill = overlay;
            v.selection.stroke = egui::Stroke::new(1.0_f32, foam);
            v.hyperlink_color = foam;
            v
        }
    }
}

// --- The visual design system ------------------------------------------------
//
// Everything below shapes how the app LOOKS, and nothing below changes what any
// control does. It is kept in one block so the whole app restyles from a single
// place: `apply_theme` sets both the palette (`visuals_for`, above) and the
// typography/spacing/shape rules (`apply_style`), and `accent` gives each palette
// one signature color used for headings, section labels, and the active tab.

/// The signature color of a theme. Used for headings, the active tab's underline,
/// section labels, and list badges — the small amount of color that tells the eye
/// where the structure of a screen is.
fn accent(theme: Theme) -> egui::Color32 {
    use egui::Color32;
    let rgb = Color32::from_rgb;
    match theme {
        Theme::Light => rgb(21, 92, 170),
        Theme::Dark => rgb(110, 170, 240),
        Theme::HighContrast => rgb(120, 200, 255),
        Theme::Solarized => rgb(38, 139, 210),
        Theme::Sepia => rgb(140, 88, 38),
        Theme::Nord => rgb(136, 192, 208),
        Theme::Dracula => rgb(189, 147, 249),
        Theme::GruvboxDark => rgb(254, 128, 25),
        Theme::GruvboxLight => rgb(175, 58, 3),
        Theme::RosePine => rgb(196, 167, 231),
        Theme::CatppuccinMocha => rgb(137, 180, 250),
        Theme::CatppuccinLatte => rgb(30, 102, 245),
        Theme::TokyoNight => rgb(122, 162, 247),
        Theme::OneDark => rgb(97, 175, 239),
        Theme::Everforest => rgb(167, 192, 128),
        Theme::Zenburn => rgb(140, 208, 211),
    }
}

/// How large the whole interface is drawn — a second, independent axis of styling
/// from [`Theme`], which only changes colour.
///
/// This matters more than usual for this program. An estate vault is read by whoever
/// has to settle an estate, which skews older than the person who set it up, often on
/// an unfamiliar machine, sometimes in a hurry. "I cannot read it" is a real failure
/// mode for a document nobody can afford to misread, and the fix should not be
/// "change your display resolution".
///
/// Implemented with egui's zoom factor rather than by rewriting the type scale: zoom
/// scales text, padding, icons, scrollbars and hit targets together, so the layout
/// stays in proportion instead of large text overflowing controls sized for small text.
#[derive(PartialEq, Eq, Clone, Copy, Default, Debug)]
enum UiScale {
    Compact,
    #[default]
    Normal,
    Large,
    Larger,
    Largest,
}

impl UiScale {
    const ALL: [UiScale; 5] =
        [UiScale::Compact, UiScale::Normal, UiScale::Large, UiScale::Larger, UiScale::Largest];

    /// Stable id for prefs.json (never the label — labels are free to be reworded).
    fn id(self) -> &'static str {
        match self {
            UiScale::Compact => "compact",
            UiScale::Normal => "normal",
            UiScale::Large => "large",
            UiScale::Larger => "larger",
            UiScale::Largest => "largest",
        }
    }

    fn from_id(id: &str) -> Option<UiScale> {
        UiScale::ALL.into_iter().find(|s| s.id() == id)
    }

    fn label(self) -> &'static str {
        match self {
            UiScale::Compact => "Compact (90%)",
            UiScale::Normal => "Normal (100%)",
            UiScale::Large => "Large (115%)",
            UiScale::Larger => "Larger (130%)",
            UiScale::Largest => "Largest (150%)",
        }
    }

    /// The egui zoom factor. Capped at 1.5: past that the lock screen stops fitting a
    /// small laptop display even with the scaled minimum window size below.
    fn factor(self) -> f32 {
        match self {
            UiScale::Compact => 0.9,
            UiScale::Normal => 1.0,
            UiScale::Large => 1.15,
            UiScale::Larger => 1.3,
            UiScale::Largest => 1.5,
        }
    }
}

/// The typeface the interface is drawn in — a third styling axis, independent of
/// [`Theme`] (colour) and [`UiScale`] (size).
///
/// **Both faces are compiled into the binary.** Nothing here reads a font from the
/// operating system, so the program looks identical on a machine with no fonts
/// installed, renders the same on every platform, and cannot be changed by altering a
/// file on disk. That last point is not only about portability: a font file is parsed
/// by a rasterizer, so loading one from a path outside the binary would turn a cosmetic
/// preference into a way to feed attacker-chosen bytes to a parser.
///
/// `Monospace` is not just a matter of taste. In a fixed-width face `0`/`O` and
/// `1`/`l`/`I` are drawn differently, which is exactly the distinction you need when
/// reading a revealed password off the screen to type it somewhere else.
#[derive(PartialEq, Eq, Clone, Copy, Default, Debug)]
enum FontChoice {
    /// Ubuntu-Light — the proportional face bundled with egui.
    #[default]
    Default,
    /// Hack — the fixed-width face bundled with egui; unambiguous digits and letters.
    Monospace,
}

impl FontChoice {
    const ALL: [FontChoice; 2] = [FontChoice::Default, FontChoice::Monospace];

    fn id(self) -> &'static str {
        match self {
            FontChoice::Default => "default",
            FontChoice::Monospace => "monospace",
        }
    }

    fn from_id(id: &str) -> Option<FontChoice> {
        FontChoice::ALL.into_iter().find(|f| f.id() == id)
    }

    fn label(self) -> &'static str {
        match self {
            FontChoice::Default => "Default (proportional)",
            FontChoice::Monospace => "Monospace (clearer 0/O and 1/l)",
        }
    }
}

/// Install the chosen typeface as the highest-priority proportional font.
///
/// Both faces are already loaded by `FontDefinitions::default()` (they ship inside
/// epaint), so this only reorders the priority list — no file is read and no allocation
/// of font bytes happens here. egui's own families stay behind the choice as the
/// fallback chain, so a glyph the face lacks (emoji, accents, CJK) still renders from
/// the bundled fonts rather than showing tofu.
fn apply_fonts(ctx: &egui::Context, choice: FontChoice) {
    ctx.set_fonts(font_definitions(choice));
}

/// The font set for a choice, as a PURE function of it.
///
/// Split out from [`apply_fonts`] so the self-containment property can be asserted
/// directly: reading it back off a live `egui::Context` needs a frame in progress,
/// which a unit test has no reason to fake.
fn font_definitions(choice: FontChoice) -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    if choice == FontChoice::Monospace
        && let Some(list) = defs.families.get_mut(&egui::FontFamily::Proportional)
    {
        list.insert(0, "Hack".to_owned());
    }
    defs
}

/// Load the saved typeface (see [`load_theme`] — same prefs file, same best-effort rules).
fn load_font_choice(vault_root: &str) -> FontChoice {
    crate::prefs_path(vault_root).map(|p| load_font_choice_from(&p)).unwrap_or_default()
}

fn load_font_choice_from(path: &std::path::Path) -> FontChoice {
    crate::effective_prefs_obj_from(path)
        .get("font")
        .and_then(|v| v.as_str())
        .and_then(FontChoice::from_id)
        .unwrap_or_default()
}

fn save_font_choice(vault_root: &str, font: FontChoice) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_font_choice_to(&path, font);
    }
}

/// Persist the typeface, preserving every other prefs key (theme, ui_scale, export_dir…).
fn save_font_choice_to(path: &std::path::Path, font: FontChoice) {
    let mut obj = crate::read_prefs_obj(path);
    obj.insert("font".into(), serde_json::Value::String(font.id().to_string()));
    crate::write_prefs_obj(path, &obj);
}

/// The window's minimum inner size, in points — the size the layout is designed against,
/// independent of the interface scale (which the framework multiplies in; see
/// [`min_inner_size`], the accessor everything should go through, which also clamps this
/// to the display so a floor can never demand a window larger than the screen).
///
/// **Height** is sized so the lock screen's tallest variant — Create, with the two
/// confirm rows — fits whole, plus ~70 px for the Help footer beneath the card.
///
/// **Width** is sized so the two-pane record tabs actually fit. This is the "devise a
/// minimum and stop shrinking" line: the list pane and the form pane each have an
/// intrinsic minimum (a label column, a field, and a row of buttons that cannot
/// usefully get narrower), and below roughly this width the form pane's content — and
/// on the Accounts and Real Estate tabs even the pane's own scrollbar — was pushed
/// outside the window and clipped by `two_col`. 620 was chosen for the lock screen
/// alone, before the two-pane tabs were measured against it.
///
/// Neither floor is a guarantee, because it yields to the display: on a screen too small
/// for it the lock screen tightens ([`auth_space_scale`]) and the form panes scroll.
const MIN_INNER_SIZE: [f32; 2] = [900.0, 670.0];

/// The window/taskbar icon, decoded from the committed 512×512 PNG that the desktop
/// shortcuts already use, so the window, the launcher and the Desktop shortcut all
/// show the same vault mark instead of a generic placeholder.
///
/// Embedded with `include_bytes!` rather than read from disk at runtime: the icon must
/// not depend on the repository still being present next to the binary, and a missing
/// file must not be able to change what the program does. Decode failure is not fatal —
/// the window simply opens with the platform default, exactly as before.
#[cfg(feature = "gui")]
fn window_icon() -> Option<egui::IconData> {
    // The locked-vault mark (the read-only default), matching packaging/linux's
    // "vaultis (View)" launcher.
    const PNG: &[u8] = include_bytes!("../../../packaging/icons/vaultis-locked.png");

    // `Cursor` because png 0.18's reader wants `Read + Seek`, and a bare `&[u8]` is
    // only `Read`.
    let decoder = png::Decoder::new(std::io::Cursor::new(PNG));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    // The committed icon is RGBA8; anything else means the asset was regenerated in a
    // different format, in which case fall back rather than show garbled pixels.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(egui::IconData { rgba: buf, width: info.width, height: info.height })
}

/// Apply a theme to the egui context: its palette AND the shared typography and
/// spacing rules. Called once before the first frame and again whenever the user
/// picks a different theme.
fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_visuals(visuals_for(theme));
    apply_style(ctx, theme);
}

/// How much of the monitor the window's minimum size may claim. The remainder absorbs
/// the things a monitor's raw size does not account for: the title bar and borders the
/// window manager adds OUTSIDE the inner size this floor describes, plus a taskbar,
/// dock or panel. A floor that exactly equalled the monitor would leave a window that
/// cannot be placed fully on screen.
const MONITOR_FIT: f32 = 0.9;

/// The window's minimum inner size, in the units [`egui::ViewportCommand::MinInnerSize`]
/// takes, clamped so it always FITS the display.
///
/// Two things this gets right that a bare `MIN_INNER_SIZE` does not:
///
/// * **The zoom is applied by the framework, not here.** egui-winit turns this value into
///   physical pixels by multiplying by `zoom_factor * native_pixels_per_point`, so passing
///   an already-scaled floor applies the interface scale TWICE. At 150% that squared the
///   floor to 2025×1507 points — larger than a 1080p display, so the window manager capped
///   the window below its own stated minimum and the lock screen, laid out for the floor it
///   was promised, overflowed into a scrollbar. `MIN_INNER_SIZE` is therefore passed as-is.
/// * **A floor is never allowed to exceed the screen.** `monitor` (from
///   [`egui::ViewportInfo::monitor_size`]) is in these same units — the winit backend derives
///   it by dividing the physical monitor by that same `pixels_per_point` — so the two compare
///   directly, and the comparison stays correct at every zoom level: raising the zoom shrinks
///   the monitor's point size exactly as fast as it grows the floor's physical size. `None`
///   (no monitor reported yet, e.g. before the first frame) keeps the unclamped floor.
///
/// Clamping DOWN is always safe: this is a floor, so lowering it only ever permits a smaller
/// window than the layout would prefer. On a display too small for the content, that is the
/// difference between a lock screen the user can scroll and a window they cannot fit on
/// screen at all.
fn min_inner_size(monitor: Option<egui::Vec2>) -> egui::Vec2 {
    let want = egui::vec2(MIN_INNER_SIZE[0], MIN_INNER_SIZE[1]);
    match monitor {
        // `> 1.0` rejects the degenerate/unknown sizes a backend can report before the
        // window is mapped, which would otherwise clamp the floor to nothing.
        Some(m) if m.x > 1.0 && m.y > 1.0 => want.min(m * MONITOR_FIT),
        _ => want,
    }
}

/// The monitor size egui currently reports for this window, if any.
fn monitor_size(ctx: &egui::Context) -> Option<egui::Vec2> {
    ctx.input(|i| i.viewport().monitor_size)
}

/// Apply a UI scale, and re-assert the window's minimum size for it.
///
/// The floor exists so the lock screen — which is meant not to scroll — always fits. It is
/// re-sent here rather than only at startup because [`min_inner_size`] clamps to the
/// display, and a scale change moves the content's physical size against a fixed screen.
fn apply_ui_scale(ctx: &egui::Context, scale: UiScale) {
    ctx.set_zoom_factor(scale.factor());
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min_inner_size(monitor_size(ctx))));
}

/// Load the saved UI scale (see [`load_theme`] — same prefs file, same best-effort rules).
fn load_ui_scale(vault_root: &str) -> UiScale {
    crate::prefs_path(vault_root).map(|p| load_ui_scale_from(&p)).unwrap_or_default()
}

fn load_ui_scale_from(path: &std::path::Path) -> UiScale {
    crate::effective_prefs_obj_from(path)
        .get("ui_scale")
        .and_then(|v| v.as_str())
        .and_then(UiScale::from_id)
        .unwrap_or_default()
}

fn save_ui_scale(vault_root: &str, scale: UiScale) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_ui_scale_to(&path, scale);
    }
}

/// Persist the scale, preserving every other prefs key (theme, export_dir, …).
fn save_ui_scale_to(path: &std::path::Path, scale: UiScale) {
    let mut obj = crate::read_prefs_obj(path);
    obj.insert("ui_scale".into(), serde_json::Value::String(scale.id().to_string()));
    crate::write_prefs_obj(path, &obj);
}

/// The typography, spacing, and shape rules shared by every screen.
///
/// egui's defaults are tuned for debug tooling: 14 px text, tight 8/3 spacing, and
/// small corner radii. This is a document-shaped application that people read, so
/// the scale is opened up — larger body text, a real heading step, roomier control
/// padding, and softer corners — which is most of what makes the window feel less
/// like a debug panel and more like an application.
fn apply_style(ctx: &egui::Context, theme: Theme) {
    use egui::{FontFamily, FontId, TextStyle};

    let mut style = (*ctx.global_style()).clone();

    // A deliberate type scale rather than one size for everything: headings lead,
    // body text is comfortable to read for a while, and small text is genuinely
    // secondary instead of merely greyer.
    style.text_styles = [
        (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace)),
    ]
    .into();

    // Spacing: more air between rows, and buttons with enough padding to look
    // pressable. `indent` widens the step of collapsing trees so the grouped
    // Accounts/Assets views read as a hierarchy at a glance.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 7.0);
    s.button_padding = egui::vec2(10.0, 5.0);
    s.indent = 20.0;
    s.window_margin = egui::Margin::same(10);
    s.menu_margin = egui::Margin::same(8);
    s.interact_size.y = 24.0;
    s.scroll.bar_width = 10.0;
    s.scroll.floating = false;

    // Text wrapping is left at egui's defaults: WRAP inside vertical layouts (so long
    // help text, paths, and error banners reflow) and EXTEND inside horizontal/grid
    // rows (so short field labels and button captions stay on one line). Forcing Wrap
    // globally was tried and reverted — it mangled form labels by wrapping them one
    // word per line. Two-pane content is kept inside its column by `two_col`'s clip,
    // not by wrapping, so no global override is needed.

    // Shape: consistently rounded controls, and a visible focus ring in the
    // accent color so keyboard focus is never guesswork.
    let v = &mut style.visuals;
    let radius = egui::CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.window_corner_radius = egui::CornerRadius::same(8);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.selection.stroke = egui::Stroke::new(1.0_f32, accent(theme));
    v.widgets.hovered.expansion = 1.0;
    v.widgets.active.expansion = 1.0;

    ctx.set_global_style(style);
}

/// A framed content card: a subtly filled, rounded, hairlined box used to group a
/// form or a panel so the eye can tell one region from the next.
///
/// Purely presentational — it wraps whatever the caller draws and returns what the
/// closure returned, so wrapping an existing block in a card never changes it.
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
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
fn two_col<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut [egui::Ui]) -> R) -> R {
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
fn auth_space_scale(available_height: f32) -> f32 {
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
const AUTH_SPACE_MIN: f32 = 0.12;

/// The Vaultis brand lockup shown at the top of the lock screen: a vault-door glyph
/// drawn from egui shapes (no image asset — the icon scales crisply and the static/
/// terminal build needs nothing extra) beside the letter-spaced "VAULTIS" wordmark,
/// with the app descriptor beneath. Everything is tinted in the active theme's accent.
///
/// `scale` is [`auth_space_scale`]'s output: below 1.0 the glyph and wordmark shrink
/// proportionally, and under half the tagline is dropped entirely. The mark is the most
/// compressible thing on the screen — it identifies the app, it is not something the user
/// has to read or click — so it yields height before any control does.
fn vaultis_logo(ui: &mut egui::Ui, accent: egui::Color32, scale: f32) {
    // Never below 0.6: past that the wordmark stops reading as a logotype.
    let shrink = 0.6 + 0.4 * scale.clamp(0.0, 1.0);
    ui.vertical_centered(|ui| {
        // The icon + wordmark sit on one row; `vertical_centered` centers the row.
        ui.horizontal(|ui| {
            let sz = 34.0_f32 * shrink;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), egui::Sense::hover());
            let stroke = egui::Stroke::new(2.2_f32, accent);
            let c = rect.center();
            {
                let p = ui.painter();
                // The vault door: a rounded square set just inside the allotted box.
                p.rect_stroke(rect.shrink(2.0), egui::CornerRadius::same(7), stroke, egui::StrokeKind::Inside);
                // The combination dial: an outer ring, a filled hub, and three spokes.
                p.circle_stroke(c, sz * 0.26, stroke);
                p.circle_filled(c, sz * 0.07, accent);
                for k in 0..3 {
                    let a = std::f32::consts::TAU * (k as f32) / 3.0 - std::f32::consts::FRAC_PI_2;
                    let dir = egui::vec2(a.cos(), a.sin());
                    p.line_segment([c + dir * (sz * 0.10), c + dir * (sz * 0.26)], stroke);
                }
            }
            ui.add_space(12.0 * shrink);
            // Letter-spaced wordmark: thin spaces (U+2009) between the glyphs give the
            // tracked, "set" look of a logotype without needing a custom font.
            let word: String = "VAULTIS".chars().map(|ch| ch.to_string()).collect::<Vec<_>>().join("\u{2009}");
            ui.label(egui::RichText::new(word).strong().size(26.0 * shrink).color(accent));
        });
        // The tagline is the first thing to go: it is the only line here that is pure
        // description, repeated verbatim in the Help manual's opening section.
        if scale > 0.5 {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Offline, two-password estate vault").weak().small());
        }
    });
}

/// A small filled pill — used for counts and mode badges, where a number needs to
/// read as a label rather than as body text.
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
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
fn section_heading(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
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
const EXPORT_CAVEAT_PREFIX: &str = "⚠ UNENCRYPTED";

/// The footer's live "the form holds changes the vault does not" indicator, shown whenever
/// [`GuiApp::has_unsaved_edits`] is true (see the status panel in `ui_top_level`). A named
/// constant so the tests assert on the SAME string the footer draws.
const UNSAVED_WARNING: &str = "⚠ unsaved changes — click 💾 Save first";

/// Whether a status message is the plaintext-on-disk caveat raised by an export
/// (`export_doc_to_config_dir` and the tab-CSV path). Drawn in red wherever the status
/// is shown, so it does not read as just another quiet "Saved."-style confirmation.
///
/// Matched on [`EXPORT_CAVEAT_PREFIX`] rather than on "Exported" as it first was: Config's
/// own "Export directory set to …" is a single character away from that prefix, so the
/// check was one reworded message away from painting an unrelated confirmation red.
fn is_export_caveat(status: &str) -> bool {
    status.starts_with(EXPORT_CAVEAT_PREFIX)
}

/// The color an export caveat is drawn in: a statement of fact about a file that now sits
/// unencrypted on disk, not the app's usual amber "might go wrong" caution.
///
/// Picked per theme rather than hardcoded. A single mid-red reads at roughly 3.2:1 on the
/// dark palettes — under the 4.5:1 WCAG AA floor for text this small, and the *default*
/// theme (Catppuccin Mocha) is one of them, which would have made the one message that
/// most needs reading the hardest to read. Each variant is chosen to clear 4.5:1 against
/// its own family's backgrounds — enforced by
/// `export_caveat_color_clears_wcag_aa_on_every_theme`, which walks all 16.
///
/// The dark variant is as pale as it is because Zenburn is the binding case: its light-grey
/// panels leave the least room, and one value has to clear every dark palette. Weight
/// (`.strong()`) and the leading ⚠ carry the urgency that saturation would otherwise.
fn export_caveat_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(255, 175, 175)
    } else {
        egui::Color32::from_rgb(178, 24, 24)
    }
}

// The color theme is stored in the shared, non-secret `prefs.json` alongside the
// export directory (see `crate::prefs_path` / `crate::read_prefs_obj` in lib.rs). The
// theme accessors live here because they reference the GUI-only `Theme` type; the
// generic prefs primitives and the export-dir accessors are shared in `crate`.

/// Load the saved theme from the standard preferences path.
fn load_theme(vault_root: &str) -> Theme {
    crate::prefs_path(vault_root).map(|p| load_theme_from(&p)).unwrap_or_default()
}

/// Load the theme from a specific path. Best-effort/bounded: missing/symlinked/over-cap/
/// unparseable all fall back to the default — a UI preference must never block startup.
fn load_theme_from(path: &std::path::Path) -> Theme {
    crate::effective_prefs_obj_from(path).get("theme").and_then(|t| t.as_str()).and_then(Theme::from_id).unwrap_or_default()
}

/// Persist the chosen theme to the standard preferences path.
fn save_theme(vault_root: &str, theme: Theme) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_theme_to(&path, theme);
    }
}

/// Persist the theme to a specific path, preserving any other prefs keys (export_dir).
fn save_theme_to(path: &std::path::Path, theme: Theme) {
    let mut obj = crate::read_prefs_obj(path);
    obj.insert("theme".into(), serde_json::Value::String(theme.id().to_string()));
    crate::write_prefs_obj(path, &obj);
}

// `enum` is a closed set of named alternatives (a tagged union). `#[derive(...)]`
// auto-generates trait implementations: `PartialEq`/`Eq` enable `==`/`!=`
// comparisons; `Clone` enables explicit `.clone()`; `Copy` makes the value
// trivially duplicated on assignment (so passing it around does not "move" it).
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Screen {
    Auth,
    Main,
    Config,
    Help,
    /// "Update from another vault": collect the source dir + its two passwords, preview the
    /// patch, then apply. Reached from Config (writable only).
    Merge,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum AuthMode {
    Create,
    Unlock,
    ChangePassword,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Tab {
    Urgent,
    Instructions,
    TrustWill,
    Assets,
    Accounts,
    RealEstate,
    Taxes,
    GeneralDocuments,
    Summary,
}

/// Deferred form action gathered during rendering, applied afterwards.
#[derive(PartialEq, Eq, Clone, Copy)]
enum FormAction {
    None,
    Save,
    Delete,
}

/// Deferred document action gathered during rendering.
#[derive(PartialEq, Eq, Clone, Copy)]
enum DocReq {
    None,
    Attach,
    Export,
    Remove,
}

/// Deferred Taxes-tab document action gathered during rendering. `Export`/`Remove`
/// carry the index of the document within the filing's `documents` list.
#[derive(PartialEq, Eq, Clone, Copy)]
enum TaxDocReq {
    None,
    Upload,
    Export(usize),
    Remove(usize),
}

/// Deferred Real-Estate document action. `Export`/`Remove` carry the index into
/// the property's `documents` list.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ReDocReq {
    None,
    Upload,
    Export(usize),
    Remove(usize),
}

// `struct` is a record of named fields — the whole application state lives here.
// Field types tell you the shape of each piece: `String` (owned text),
// `bool` (flag), `Option<T>` (maybe present). egui calls our `ui()` method each
// frame with `&mut GuiApp`, so every field is freely readable/writable there.
/// Undo for an in-memory change that a failed save must not leave behind (see
/// `delete_current`): a one-shot closure that puts the app back the way it was.
type Rollback = Box<dyn FnOnce(&mut GuiApp)>;

struct GuiApp {
    path: std::path::PathBuf,
    /// When false the vault is opened read-only and write controls are hidden.
    writable: bool,
    screen: Screen,
    // Auth.
    auth_mode: AuthMode,
    /// The directory whose `vault.pmv` we open/create. On the collapsed start page this is
    /// DERIVED as `<vault_root>/<vault_name>` (see `recompute_vault_path`), never edited
    /// directly. Kept in sync with `path` (`path == <vault_dir>/vault.pmv`).
    vault_dir: String,
    /// Editable ROOT directory scanned (one level deep) for vaults to populate the
    /// start-page dropdown. Seeded from the saved `vault_root` preference (else the launch
    /// dir's parent); editing it re-scans and is persisted back to prefs.
    vault_root: String,
    /// The selected/typed vault folder NAME (leaf under `vault_root`) — the editable "Vault"
    /// box. The dropdown fills it; typing a name not on disk arms Create. Empty = the root
    /// itself. Together with `vault_root` it derives `vault_dir`/`path`.
    vault_name: String,
    /// Names of the subdirectories of `vault_root` that contain a `vault.pmv`, refreshed
    /// whenever `vault_root` changes — the dropdown's items. Sorted case-insensitively.
    discovered_vaults: Vec<String>,
    /// A warning from the most recent scan (root unreadable, or some entries skipped),
    /// shown beneath the picker. `None` when the scan was clean.
    vault_scan_warning: Option<String>,
    /// Where `scripts/build.sh`'s demo vault actually lives, if one was ever built.
    /// Resolved once at startup (see `launch::sample_vault_dir`); `None` hides the
    /// lock screen's "Sample vault" button entirely rather than offering it and
    /// failing on click.
    sample_vault: Option<std::path::PathBuf>,
    pw1: String,
    confirm1: String,
    pw2: String,
    confirm2: String,
    auth_error: Option<String>,
    // Unlocked vault. `Option<OpenVault>` is `None` until the user authenticates,
    // then `Some(vault)`; this is how Rust models "may or may not be present"
    // without null pointers.
    vault: Option<OpenVault>,
    // "Update from another vault" (Screen::Merge) state. The source directory + its two
    // passwords are collected, then `merge_source` holds the opened (read-only) source and
    // `merge_plan` the computed patch between the preview and the apply. Passwords are
    // wiped (and pre-reserved) like the auth buffers.
    merge_src_dir: String,
    merge_pw1: String,
    merge_pw2: String,
    merge_source: Option<OpenVault>,
    merge_plan: Option<crate::merge::MergePlan>,
    merge_error: Option<String>,
    // Tabs + per-tab working edit buffer. Each `edit_*` is the record currently
    // being edited on that tab, or `None` when nothing is selected.
    tab: Tab,
    edit_urgent: Option<Urgent>,
    edit_instruction: Option<Instruction>,
    edit_trustwill: Option<TrustWill>,
    edit_asset: Option<AssetLiability>,
    edit_account: Option<Account>,
    edit_realestate: Option<RealEstate>,
    edit_taxfiling: Option<TaxFiling>,
    edit_general: Option<GeneralDocument>,
    // The ONLY reveal control on the Accounts screen: a single global toggle that
    // unmasks every account password at once (there is no per-record reveal).
    reveal_all: bool,
    // The same single global toggle for the Real Estate screen's four portal passwords.
    // Kept separate from `reveal_all` so the two screens don't reveal each other.
    re_reveal_all: bool,
    // Saved "view defaults" preferences (the three Config checkboxes, persisted in
    // prefs.json). They are kept SEPARATE from the live view state above so the Config
    // checkboxes always reflect the saved default, never a transient per-tab toggle.
    // `reveal_default` seeds `reveal_all`/`re_reveal_all` at open AND is re-applied by the
    // tab-switch reset (instead of forcing reveal back to masked); the two grouping
    // defaults seed `acct_grouped`/`asset_grouped` at open.
    reveal_default: bool,
    group_assets_default: bool,
    group_accounts_default: bool,
    // Accounts-tab display filters ("" = no filter).
    acct_filter_type: String,
    acct_filter_subtype: String,
    acct_filter_owner: String,
    acct_filter_title: String,
    acct_filter_review: bool,
    // Free-text, case-insensitive substring search over account usernames.
    acct_search_user: String,
    // Accounts view: false = flat filtered list, true = grouped tree
    // (type → subtype → owner → title).
    acct_grouped: bool,
    // Assets view: false = flat filtered list, true = grouped tree (owner → Asset/Liability → type).
    asset_grouped: bool,
    // Assets-tab "review only" filter.
    asset_filter_review: bool,
    /// Account id whose Delete click is awaiting confirmation because assets still
    /// link to it: deleting such an account is allowed but never silent (the links are
    /// NOT cascaded — they render as raw ids afterwards), so the first click arms this
    /// and the form shows the linked-from count + a "Delete anyway"/"Cancel" pair.
    /// Guarded by the record id so a warning armed for one account can never confirm
    /// a delete of another; disarmed on selection change / New / cancel / confirm.
    pending_account_delete: Option<String>,
    // Config screen inputs.
    new_asset_type: String,
    new_account_type: String,
    new_subtype_for: String,
    new_subtype_name: String,
    backup_dest: String,
    // Volume-size config input (whole MiB).
    cfg_volume_size: String,
    /// The redundancy-depth picker's selection (persistent across frames — egui's
    /// ComboBox closure only runs while the popup is open, so a frame-local would
    /// reset before Apply and the control would be dead). Re-seeded from the vault
    /// each time the Config screen is opened.
    cfg_redundancy: u32,
    // Shared document-attach input buffers. The storage location is auto-derived
    // ([<owner-initials>/]<root>[/<group>][/subfolder], timestamp folded into the
    // filename as <ts>_<file>); the user controls only the optional subfolder and the
    // filename.
    doc_subfolder: String,
    doc_filename: String,
    doc_source: String,
    /// The query typed into the "Link an account…" dropdown's search box (Assets tab). Kept
    /// here rather than frame-local because a ComboBox's closure only runs while its popup is
    /// open, so a local would reset on every frame and the box could never be typed into. It
    /// is cleared whenever the popup is closed, so re-opening always starts from the full list.
    link_search: String,
    // Prefs-backed export destination directory (replaces the old per-export "Export to"
    // path prompt). Settable even in read-only mode — it is a local-machine preference,
    // not vault content — so read-only document export (the heir use case) keeps working.
    export_dir: String,
    status: String,
    /// When `Some`, a hard operation failure (a failed save/export/backup/upload, …) to
    /// surface in a CONSPICUOUS top banner — not just the easily-missed weak status line.
    /// Cleared on dismissal or when any later status message replaces the failure text
    /// (see [`error_banner_is_stale`]).
    error: Option<String>,
    clipboard_dirty: bool,
    // When set, the clipboard should be wiped at/after this instant.
    // `Option<Instant>`: `None` = no pending wipe, `Some(t)` = wipe at time `t`.
    clipboard_clear_at: Option<Instant>,
    /// The selected color theme, and the one currently applied to egui — so we
    /// only call `set_visuals` (and persist) when the selection actually changes.
    theme: Theme,
    applied_theme: Theme,
    /// The selected interface scale, and the one currently applied — same
    /// selected/applied pair as the theme, so zoom is only pushed to egui (and
    /// persisted) when the selection actually changes, not every frame.
    ui_scale: UiScale,
    applied_ui_scale: UiScale,
    /// The window minimum last pushed to the viewport, so the command is only re-sent when
    /// the value actually changes. It cannot be settled once at startup: the monitor size
    /// [`min_inner_size`] clamps against is not reported until a frame has been drawn, and
    /// it changes again if the window is dragged to a different display. `ZERO` is the
    /// "nothing sent yet" marker — never a legitimate floor, so the first frame always
    /// pushes one.
    applied_min_inner: egui::Vec2,
    /// The selected typeface, and the one currently applied — same selected/applied
    /// pair as the theme and scale. `set_fonts` rebuilds the font atlas, so it must
    /// only run when the choice actually changes, never per frame.
    font: FontChoice,
    applied_font: FontChoice,
    /// The in-app manual's browser state (search box + selected topic), kept here
    /// so the user's place in it survives leaving and re-entering Help.
    help: crate::gui_help::HelpState,
    /// Which screen Help must return to when its Back is pressed.
    ///
    /// Help is now reachable from the LOCK screen as well as the top bar, and those
    /// need different exits: returning to `Main` from the lock screen would draw the
    /// in-vault UI with no vault open. Recorded on the way in rather than inferred on
    /// the way out.
    help_return: Screen,
}

/// How long a copied password stays on the clipboard before it is auto-cleared.
const CLIPBOARD_CLEAR_AFTER: Duration = Duration::from_secs(15);

// `impl Trait for Type` provides a trait's methods for a type (like implementing
// an interface). `Drop` runs `drop()` automatically when a `GuiApp` goes out of
// scope (e.g. on quit) — used here to wipe the in-memory password buffers and
// clear the OS clipboard so secrets do not linger after exit.
impl Drop for GuiApp {
    // `&mut self` is an exclusive borrow of the value being dropped, so we can
    // overwrite its fields. `.zeroize()` overwrites the heap bytes with zeros.
    fn drop(&mut self) {
        self.pw1.zeroize();
        self.confirm1.zeroize();
        self.pw2.zeroize();
        self.confirm2.zeroize();
        self.merge_pw1.zeroize();
        self.merge_pw2.zeroize();
        if self.clipboard_dirty {
            clear_clipboard();
        }
    }
}

// Inherent methods of `GuiApp` (its own functions, not from a trait). `Self`
// inside this block is shorthand for the type `GuiApp`.
impl GuiApp {
    // A constructor by convention; `-> Self` returns a new `GuiApp`. There is no
    // `new` keyword in Rust — this is just a regular function.
    fn new(path: std::path::PathBuf, writable: bool) -> Self {
        // Collapsed start page: the open target is `<root>/<name>`. The root comes from the
        // launched path, else the last root a vault was successfully opened from (see
        // `launch::save_last_root`), else nothing — the start page opens EMPTY and the user
        // types or pastes a root. The working directory is deliberately NOT consulted; see
        // `launch::initial_root_and_name` for why, and the prefs comment in `lib.rs`.
        let last_root = crate::launch::load_last_root();
        let (vault_root, vault_name) = crate::launch::initial_root_and_name(&path, last_root.as_deref());
        // Default the backup destination to the root (see the `backup_dest` field).
        let backup_dest = vault_root.clone();
        let vault_dir = crate::launch::join_root_name(&vault_root, &vault_name);
        let path = crate::launch::vault_file(&vault_dir);
        // `if ... { } else { }` is an expression here: its value initializes
        // `auth_mode` (unlock an existing vault file, else offer to create one).
        let auth_mode = if path.exists() { AuthMode::Unlock } else { AuthMode::Create };
        let scan = crate::launch::discover_vaults(&vault_root);
        // Load the saved theme; `applied_theme` starts equal to it so the first
        // frame doesn't needlessly re-apply/re-save (the same value `run` already set).
        let theme = load_theme(&vault_root);
        let ui_scale = load_ui_scale(&vault_root);
        let font = load_font_choice(&vault_root);
        // Saved "view defaults" (Config checkboxes, `<vault_root>/prefs.json`): seed the
        // grouped/flat view state so a freshly opened vault honours the user's preferences.
        // `reveal_default` is always false — reveal is a per-session toggle that is never
        // persisted, so a tampered prefs.json can't unmask passwords (see `lib.rs`).
        let reveal_default = crate::load_reveal_all_default(&vault_root);
        let group_assets_default = crate::load_group_assets_default(&vault_root);
        let group_accounts_default = crate::load_group_accounts_default(&vault_root);
        // Hoisted above the struct literal because `vault_root` is moved into the struct
        // below; the vault-root fallback needs to read it before that move.
        let export_dir = crate::load_export_dir(&vault_root);
        GuiApp {
            path,
            writable,
            screen: Screen::Auth,
            auth_mode,
            vault_dir,
            vault_root,
            vault_name,
            discovered_vaults: scan.vaults,
            vault_scan_warning: scan.warning,
            sample_vault: crate::launch::sample_vault_dir(),
            // Pre-reserve generous capacity so typing a password never grows (and so
            // reallocates) these buffers, which would strand un-zeroized fragments of
            // the master password in freed heap. `wipe_passwords`/`Drop` wipe the live
            // buffer; pre-sizing removes the reallocation leak in between.
            pw1: String::with_capacity(256),
            confirm1: String::with_capacity(256),
            pw2: String::with_capacity(256),
            confirm2: String::with_capacity(256),
            auth_error: None,
            vault: None,
            merge_src_dir: String::new(),
            // Pre-reserve so typing the source passwords never reallocates (which would
            // strand un-zeroized fragments) — same discipline as the auth buffers.
            merge_pw1: String::with_capacity(256),
            merge_pw2: String::with_capacity(256),
            merge_source: None,
            merge_plan: None,
            merge_error: None,
            tab: Tab::Urgent,
            edit_urgent: None,
            edit_instruction: None,
            edit_trustwill: None,
            edit_asset: None,
            edit_account: None,
            edit_realestate: None,
            edit_taxfiling: None,
            edit_general: None,
            reveal_all: reveal_default,
            re_reveal_all: reveal_default,
            reveal_default,
            group_assets_default,
            group_accounts_default,
            acct_filter_type: String::new(),
            acct_filter_subtype: String::new(),
            acct_filter_owner: String::new(),
            acct_filter_title: String::new(),
            acct_filter_review: false,
            acct_search_user: String::new(),
            acct_grouped: group_accounts_default,
            asset_grouped: group_assets_default,
            asset_filter_review: false,
            pending_account_delete: None,
            new_asset_type: String::new(),
            new_account_type: String::new(),
            new_subtype_for: String::new(),
            new_subtype_name: String::new(),
            // Default the backup destination to the vault ROOT (editable in Config). It
            // tracks the root while still on the start page; once unlocked it's the user's.
            backup_dest,
            cfg_volume_size: String::new(),
            cfg_redundancy: 0,
            doc_subfolder: String::new(),
            doc_filename: String::new(),
            doc_source: String::new(),
            link_search: String::new(),
            export_dir,
            status: String::new(),
            error: None,
            clipboard_dirty: false,
            clipboard_clear_at: None,
            theme,
            applied_theme: theme,
            ui_scale,
            applied_ui_scale: ui_scale,
            applied_min_inner: egui::Vec2::ZERO,
            font,
            applied_font: font,
            help: crate::gui_help::HelpState::default(),
            help_return: Screen::Main,
        }
    }

    /// Wipe the clipboard once the auto-clear deadline has passed; otherwise
    /// schedule a repaint so the deadline fires even with no user interaction.
    fn tick_clipboard(&mut self, ctx: &egui::Context) {
        // `if let Some(x) = opt { ... }` runs the block only when `opt` is
        // `Some`, binding its inner value to `x`. Here: only act if a wipe
        // deadline has been scheduled. `&egui::Context` is a shared borrow.
        if let Some(deadline) = self.clipboard_clear_at {
            let now = Instant::now();
            // The deadline/status-preservation rules live in a pure, unit-tested helper
            // shared with the TUI; `Some` means "wipe now", `None` means "not yet".
            match crate::clipboard_tick_decision(Some(deadline), now, &self.status) {
                Some(status_change) => {
                    clear_clipboard();
                    self.clipboard_dirty = false;
                    self.clipboard_clear_at = None;
                    if let Some(s) = status_change {
                        self.status = s;
                    }
                }
                None => {
                    ctx.request_repaint_after(deadline - now);
                }
            }
        }
    }

    // Returns a shared borrow (`&OpenVault`) of the open vault. `.as_ref()` turns
    // `&Option<T>` into `Option<&T>` (borrow without taking ownership);
    // `.expect("…")` then unwraps it, panicking with this message if `None` —
    // safe here because this is only called on the Main screen where the vault
    // is guaranteed open.
    fn vault_ref(&self) -> &OpenVault {
        self.vault.as_ref().expect("vault is open on the main screen")
    }

    /// Persist the in-memory vault, reporting any error to the status bar.
    /// Save the open vault. Returns `true` only if the vault was actually written
    /// to disk. Callers that reclaim a document blob AFTER persisting MUST gate the
    /// reclaim on this: if the save failed (e.g. a full disk), `vault.pmv` still
    /// references the doc, so dropping its blob would leave a dangling reference
    /// (`ArchiveMismatch` — an unopenable vault) on the next open.
    fn persist(&mut self) -> bool {
        // Borrow the vault mutably if present, attempt the save, and return early on the
        // success/absent paths. We can't call `self.fail()` (a `&mut self` method) while
        // `self.vault` is borrowed for the save, so we capture the message and report it
        // AFTER the borrow ends — surfacing a failed save in the conspicuous banner.
        let err = match self.vault.as_mut() {
            Some(ov) => match ov.save() {
                Ok(()) => return true,
                Err(e) => format!("Save failed: {e}"),
            },
            None => return false,
        };
        self.fail(err);
        false
    }

    /// Record a hard operation FAILURE: show `msg` in the CONSPICUOUS top error banner
    /// (rendered by [`GuiApp::ui`]) as well as the status line. A failed save (e.g. a full
    /// disk) must be impossible to miss — hidden in the weak status text alone, the user
    /// would believe the edit was saved when it was not. The banner clears when the user
    /// dismisses it or any later status message replaces this text (see
    /// [`error_banner_is_stale`]).
    fn fail(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.error = Some(msg.clone());
        self.status = msg;
    }

    fn clear_doc_inputs(&mut self) {
        self.doc_subfolder.clear();
        self.doc_filename.clear();
        self.doc_source.clear();
    }

    /// Build the CSV text for the current tab's records (ALL of them, ignoring any display
    /// filter), plus a base filename and the record count. The tab -> collection mapping
    /// lives in the shared `csv::build_tab_csv` core helper; this only maps the GUI's local
    /// `Tab` to `csv::CsvTab`. The `Summary => None` arm keeps the match exhaustive — Summary
    /// has no records and shows no CSV button, so it is unreachable from the GUI. Document/
    /// file columns hold file NAMES. The result is wrapped in `Zeroizing` because it can
    /// contain plaintext passwords (Accounts / Real Estate portals).
    fn build_tab_csv(&self) -> Option<(&'static str, Zeroizing<String>, usize)> {
        let ov = self.vault.as_ref()?;
        let tab = match self.tab {
            Tab::Urgent => csv::CsvTab::Urgent,
            Tab::Instructions => csv::CsvTab::Instructions,
            Tab::TrustWill => csv::CsvTab::TrustWill,
            Tab::Assets => csv::CsvTab::Assets,
            Tab::Accounts => csv::CsvTab::Accounts,
            Tab::RealEstate => csv::CsvTab::RealEstate,
            Tab::Taxes => csv::CsvTab::Taxes,
            Tab::GeneralDocuments => csv::CsvTab::GeneralDocuments,
            Tab::Summary => return None,
        };
        let name_of = |id: &str| ov.doc_path(id).map(|p| csv::basename(&p)).unwrap_or_default();
        let (base, text, n) = csv::build_tab_csv(&ov.vault, tab, name_of);
        Some((base, Zeroizing::new(text), n))
    }

    /// Export every record on the current tab to a timestamped CSV in the configured
    /// export directory (e.g. `accounts-20240628-143000.csv`).
    ///
    /// Available in READ-ONLY sessions, like document export — the vault owner asked for
    /// this explicitly, overriding the earlier write-mode gate. A CSV can hold every
    /// record's plaintext password, so the warning the gate used to enforce now travels
    /// with the feature instead: it is on the button's tooltip and on every success line.
    fn export_current_tab_csv(&mut self) {
        // Available in READ-ONLY sessions too, at the vault owner's explicit request. The
        // file it writes is plain, unencrypted text and — on Accounts and Real Estate —
        // contains every password in the clear, so the status line below says so rather
        // than reporting a bare success.
        // Refuses an unset directory AND one inside the vault folder: this CSV is
        // unencrypted and carries every password, so it must never land where the user's
        // next backup of the vault picks it up (the same rule the CLI's extract/export-tree
        // enforce).
        let dir = match crate::checked_export_dir(&self.path, &self.export_dir) {
            Ok(d) => d,
            Err(msg) => {
                self.fail(msg);
                return;
            }
        };
        let Some((base, text, n)) = self.build_tab_csv() else {
            self.status = "Nothing to export on this tab.".into();
            return;
        };
        let filename = format!("{base}-{}.csv", records::compact_utc(records::unix_now()));
        match vault::write_export_bytes(&dir, &filename, text.as_bytes()) {
            Ok(p) => {
                // Caveat FIRST — the path can be arbitrarily long and the status strip
                // truncates. See `EXPORT_CAVEAT_PREFIX`.
                self.status = format!(
                    "{EXPORT_CAVEAT_PREFIX} — this CSV holds every password in the clear. \
                     Exported {n} record(s) to {}",
                    p.display()
                );
            }
            Err(e) => self.fail(format!("CSV export failed: {e}")),
        }
    }

    /// Export document `id` into the configured export directory, recreating its volume
    /// folder structure under it. Used by every tab's Export button — there is no
    /// per-export path prompt; the destination is the directory set in Config (which is
    /// editable even in read-only mode, so this works for a read-only heir).
    fn export_doc_to_config_dir(&mut self, id: &str) {
        // Same guard as the CSV path: unset, or inside the vault folder, is refused —
        // the file written here is the DECRYPTED document.
        let dir = match crate::checked_export_dir(&self.path, &self.export_dir) {
            Ok(d) => d,
            Err(msg) => {
                self.fail(msg);
                return;
            }
        };
        if let Some(ov) = self.vault.as_ref() {
            match ov.export_document_into(id, &dir) {
                Ok(p) => {
                    // Caveat FIRST — see `EXPORT_CAVEAT_PREFIX`.
                    self.status = format!(
                        "{EXPORT_CAVEAT_PREFIX} — this copy is a plain, readable file. \
                         Exported to {}",
                        p.display()
                    )
                }
                Err(e) => self.fail(format!("Export failed: {e}")),
            }
        }
    }

    /// Whether the record currently open in the form on `self.tab` has changes that
    /// would be lost by navigating away without saving — either it differs from the
    /// saved copy with the same id, or (a brand-new record started with ➕ New) it
    /// has no saved copy at all yet. Read by the footer to turn the old "save before
    /// you click away" help-text warning into a live indicator instead: see
    /// `ui_top_level` around the status panel.
    fn has_unsaved_edits(&self) -> bool {
        let Some(ov) = self.vault.as_ref() else { return false };
        let v = &ov.vault;
        match self.tab {
            Tab::Urgent => {
                self.edit_urgent.as_ref().is_some_and(|r| v.urgent.iter().find(|s| s.id == r.id) != Some(r))
            }
            Tab::Instructions => self
                .edit_instruction
                .as_ref()
                .is_some_and(|r| v.instructions.iter().find(|s| s.id == r.id) != Some(r)),
            Tab::TrustWill => self
                .edit_trustwill
                .as_ref()
                .is_some_and(|r| v.trust_wills.iter().find(|s| s.id == r.id) != Some(r)),
            Tab::Assets => {
                self.edit_asset.as_ref().is_some_and(|r| v.assets.iter().find(|s| s.id == r.id) != Some(r))
            }
            Tab::Accounts => {
                self.edit_account.as_ref().is_some_and(|r| v.accounts.iter().find(|s| s.id == r.id) != Some(r))
            }
            Tab::RealEstate => self
                .edit_realestate
                .as_ref()
                .is_some_and(|r| v.real_estate.iter().find(|s| s.id == r.id) != Some(r)),
            Tab::Taxes => self
                .edit_taxfiling
                .as_ref()
                .is_some_and(|r| v.tax_filings.iter().find(|s| s.id == r.id) != Some(r)),
            Tab::GeneralDocuments => self
                .edit_general
                .as_ref()
                .is_some_and(|r| v.general_documents.iter().find(|s| s.id == r.id) != Some(r)),
            // Summary is a read-only computed view; it has no edit buffer to lose.
            Tab::Summary => false,
        }
    }

    /// Re-read the record open in `tab`'s form back out of the vault, so the edit buffer
    /// holds exactly what was just written.
    ///
    /// Call after a SUCCESSFUL [`Self::persist`] that upserted that buffer — every tab's
    /// Save, and the document attach/remove paths, which persist the record→document link
    /// on the spot. [`records::upsert`] stamps `updated_at` and appends the field diffs to
    /// the record's history, so the STORED record is never identical to the buffer that
    /// produced it: without this write-back [`Self::has_unsaved_edits`] compared the two,
    /// found them different, and left the footer's [`UNSAVED_WARNING`] lit for the rest of
    /// the session — telling the user their saved work was still unsaved. (It also leaves
    /// the History panel under the form showing the pre-save trail.)
    ///
    /// Only on success: after a FAILED save the vault holds the change but the disk does
    /// not, and the warning — "click 💾 Save first" — is still the right advice.
    fn sync_edit_buffer(&mut self, tab: Tab) {
        // `self.vault` and the `edit_*` buffers are disjoint fields, so the shared borrow
        // of one and the exclusive borrow of the other coexist.
        let Some(ov) = self.vault.as_ref() else { return };
        let v = &ov.vault;
        match tab {
            Tab::Urgent => sync_from_saved(&mut self.edit_urgent, &v.urgent),
            Tab::Instructions => sync_from_saved(&mut self.edit_instruction, &v.instructions),
            Tab::TrustWill => sync_from_saved(&mut self.edit_trustwill, &v.trust_wills),
            Tab::Assets => sync_from_saved(&mut self.edit_asset, &v.assets),
            Tab::Accounts => sync_from_saved(&mut self.edit_account, &v.accounts),
            Tab::RealEstate => sync_from_saved(&mut self.edit_realestate, &v.real_estate),
            Tab::Taxes => sync_from_saved(&mut self.edit_taxfiling, &v.tax_filings),
            Tab::GeneralDocuments => sync_from_saved(&mut self.edit_general, &v.general_documents),
            // Summary is a read-only computed view — no edit buffer, nothing to sync.
            Tab::Summary => {}
        }
    }

    // --- Auth ----------------------------------------------------------------

    // Returns either `Ok((pw1, pw2))` (a 2-tuple of zeroizing strings) or
    // `Err(message)`. `&self` is a read-only borrow — this validates without
    // mutating. `.into()` converts the string literal `&str` into an owned
    // `String` to match the `Err` type.
    fn confirmed_passwords(&self) -> Result<(Zeroizing<String>, Zeroizing<String>), String> {
        if self.pw1.is_empty() || self.pw2.is_empty() {
            return Err("Both passwords are required.".into());
        }
        if self.pw1 != self.confirm1 || self.pw2 != self.confirm2 {
            return Err("Password confirmations do not match.".into());
        }
        // `.clone()` makes owned copies of the password strings; wrapping them in
        // `Zeroizing` means those copies are wiped from the heap when dropped.
        Ok((Zeroizing::new(self.pw1.clone()), Zeroizing::new(self.pw2.clone())))
    }

    fn submit_auth(&mut self) {
        // `match` dispatches on the value, like a switch but exhaustive: every
        // variant must be handled. Each `Variant => { ... }` is an arm.
        match self.auth_mode {
            AuthMode::ChangePassword => {
                // Destructure the success tuple into `pw1`/`pw2`; on `Err`, record
                // the message and `return` early from the whole method.
                let (pw1, pw2) = match self.confirmed_passwords() {
                    Ok(p) => p,
                    Err(m) => {
                        self.auth_error = Some(m);
                        return;
                    }
                };
                if let Some(ov) = self.vault.as_mut() {
                    // `.as_bytes()` views the string as a read-only byte slice
                    // (`&[u8]`), which the crypto layer expects.
                    match ov.change_password(pw1.as_bytes(), pw2.as_bytes()) {
                        Ok(()) => {
                            self.status = "Master passwords changed.".into();
                            self.auth_error = None;
                            self.wipe_passwords();
                            self.screen = Screen::Main;
                        }
                        Err(e) => {
                            // The rekey may have left the handle poisoned (read-only)
                            // with a pending `.rekey` on disk. Drop the handle to
                            // release the single-writer lock, then return to the
                            // unlock screen: reopening runs recover_pending_rekey,
                            // which finishes or discards the interrupted rekey
                            // idempotently. Without this the dead handle keeps the
                            // lock and the session can't recover in place.
                            self.vault = None;
                            self.auth_mode = AuthMode::Unlock;
                            self.screen = Screen::Auth;
                            self.wipe_passwords();
                            self.auth_error =
                                Some(format!("Password change interrupted: {e}. Unlock again to recover."));
                        }
                    }
                }
            }
            // `A | B =>` matches either variant with one arm.
            AuthMode::Create | AuthMode::Unlock => self.submit_open_or_create(true),
        }
    }

    /// `remember_root` gates [`launch::save_last_root`] on success: `true` for a real,
    /// user-driven open (the normal path here), `false` for [`Self::open_sample_vault`] —
    /// the demo directory under `target/` is not somewhere the start page should default to
    /// on the next launch.
    fn submit_open_or_create(&mut self, remember_root: bool) {
        let creating = self.auth_mode == AuthMode::Create;
        if creating && !self.writable {
            self.auth_error =
                Some("No vault here, and this is read-only. Relaunch with --write to create one.".into());
            return;
        }
        // `result` is assigned from an `if/else` expression: create a new vault
        // or open an existing one. `self.path.clone()` hands an owned copy of the
        // path to the call (the original stays in `self`).
        let result = if creating {
            let (pw1, pw2) = match self.confirmed_passwords() {
                Ok(p) => p,
                Err(m) => {
                    self.auth_error = Some(m);
                    return;
                }
            };
            OpenVault::create(self.path.clone(), pw1.as_bytes(), pw2.as_bytes(), crate::kdf_params_for_new_vault())
        } else {
            OpenVault::open_with(
                self.path.clone(),
                self.pw1.as_bytes(),
                self.pw2.as_bytes(),
                !self.writable,
            )
        };

        match result {
            Ok(v) => {
                // Remember the ROOT (not which vault within it) so the next bare launch
                // starts here — see `launch::save_last_root` and the `lib.rs` prefs comment
                // for why this one pointer lives in the OS data dir while everything else
                // stays inside `<vault_root>/prefs.json`. Skipped for the one-click sample
                // vault: that directory lives under `target/` and is not a real vault
                // location worth defaulting future launches to.
                if remember_root {
                    crate::launch::save_last_root(records::unquote_path(&self.vault_root));
                }

                // If the live vault.pmv was unreadable and we recovered from an
                // in-place redundant copy (§12.8), that notice takes priority — the
                // user needs to know a roll-forward/rollback happened.
                let recovered = v.recovery_notice().map(|s| s.to_string());
                self.status = if let Some(notice) = recovered {
                    notice
                } else if creating {
                    "New vault created.".to_string()
                } else if v.previous_access() == 0 {
                    "Vault unlocked.".to_string()
                } else {
                    // Show the write-generation so a rollback to an older snapshot
                    // is noticeable (§9.12).
                    format!(
                        "Unlocked. Last opened: {} (generation {})",
                        format_time(v.previous_access()),
                        v.opened_generation()
                    )
                };
                // Start the new vault's UI from a clean slate — never inherit the previous
                // session's edit buffers/filters/reveal (see reset_per_vault_ui_state). Done
                // BEFORE installing the vault so nothing from vault A is ever rendered for B.
                self.reset_per_vault_ui_state();
                self.vault = Some(v);
                // Bring the Config type lists into line with what records actually use, so a
                // freshly opened (writable) vault's Config matches its data — types brought in
                // by an older import/merge appear without a manual "sync". This is ADD-ONLY:
                // `sync_types_from_records` only inserts missing types/subtypes, it never
                // deletes a configured entry. Read-only sessions skip it; with no drift it adds
                // nothing and writes nothing. Appended to the open message so a recovery/unlock
                // notice is never clobbered.
                if self.writable {
                    match self.vault.as_mut().map(|ov| ov.sync_types_from_records()) {
                        Some(Ok(n)) if n > 0 => {
                            self.status = format!("{} · Synced {n} type(s) from records.", self.status)
                        }
                        Some(Err(e)) => self.status = format!("{} · Type sync failed: {e}", self.status),
                        _ => {}
                    }
                }
                self.auth_error = None;
                self.wipe_passwords();
                self.screen = Screen::Main;
            }
            // Collapse every CORRECT-password-reachable failure into ONE message so the
            // unlock screen can't be used as a "this password is correct" oracle: a
            // wrong password yields `Crypto`, while a missing/rolled-back document
            // (`ArchiveMismatch`), corrupt plaintext (`Json`), or storage error are
            // reachable ONLY after a successful decrypt, so a distinct message for them
            // would reveal the password was right (audit O-1; mirrors the FFI collapse).
            // Structural, password-INDEPENDENT errors (bad magic/version/truncated/
            // params/too-large, not-found, locked, rekey-pending) keep their specific,
            // useful messages below — they leak nothing about password correctness.
            Err(VaultError::Crypto(_) | VaultError::ArchiveMismatch | VaultError::Json(_) | VaultError::Storage(_)) => {
                self.auth_error = Some("Wrong password(s) or corrupted/unreadable vault.".into());
                // Wipe the entered passwords on failure too (not just on success), so
                // they don't linger in memory after a failed attempt — the moment a
                // user is most likely to step away. Mirrors the TUI, which rebuilds
                // (and thus zeroizes) its AuthState on a failed unlock.
                self.wipe_passwords();
            }
            // `Err(e)` catches every other (password-independent) error variant.
            Err(e) => {
                self.auth_error = Some(format!("{e}"));
                self.wipe_passwords();
            }
        }
    }

    /// Re-derive the open target from `<vault_root>/<vault_name>`: rebuild `vault_dir` and
    /// `path`, then flip the mode — Unlock if a `vault.pmv` already exists there, else Create
    /// (which, in --write mode, creates the directory + vault on submit). Called whenever the
    /// root, the vault name, or the dropdown selection changes.
    fn recompute_vault_path(&mut self) {
        self.vault_dir = crate::launch::join_root_name(&self.vault_root, &self.vault_name);
        self.path = crate::launch::vault_file(&self.vault_dir);
        self.auth_mode = if self.path.exists() { AuthMode::Unlock } else { AuthMode::Create };
        self.auth_error = None;
    }

    /// Re-scan `vault_root` for vaults (one level deep) and refresh the dropdown items
    /// plus any access warning. Called when the root field changes.
    fn refresh_discovered_vaults(&mut self) {
        let scan = crate::launch::discover_vaults(&self.vault_root);
        self.discovered_vaults = scan.vaults;
        self.vault_scan_warning = scan.warning;
    }

    /// Adopt `<vault_root>/prefs.json` after the root changed, and apply it immediately.
    ///
    /// Preferences live in the root, but the root is chosen ON the lock screen — so at boot
    /// there is often no root yet (a bare launch starts empty) and the built-in defaults are
    /// what got applied. Without this, pointing at a root that carries its own look would do
    /// nothing until the next launch, which defeats the whole point of a portable root.
    ///
    /// The three look settings are applied to the context DIRECTLY here, and `applied_*` is
    /// set to match, rather than letting `render`'s "changed since applied" path do it. That
    /// path also SAVES, which would write a `prefs.json` into any folder the user merely
    /// browsed to — the file must appear only when a setting is deliberately changed.
    fn adopt_root_prefs(&mut self, ctx: &egui::Context) {
        let root = self.vault_root.clone();
        self.theme = load_theme(&root);
        self.applied_theme = self.theme;
        apply_theme(ctx, self.theme);

        self.ui_scale = load_ui_scale(&root);
        self.applied_ui_scale = self.ui_scale;
        apply_ui_scale(ctx, self.ui_scale);

        self.font = load_font_choice(&root);
        self.applied_font = self.font;
        apply_fonts(ctx, self.font);

        // View defaults seed the live view state the same way `GuiApp::new` does.
        self.group_assets_default = crate::load_group_assets_default(&root);
        self.group_accounts_default = crate::load_group_accounts_default(&root);
        self.asset_grouped = self.group_assets_default;
        self.acct_grouped = self.group_accounts_default;
    }

    /// Pick a vault `name` from the dropdown: set the vault name and re-derive the
    /// path/mode so the user lands ready to unlock it.
    fn select_vault(&mut self, name: &str) {
        self.vault_name = name.to_string();
        self.recompute_vault_path();
    }

    fn wipe_passwords(&mut self) {
        self.pw1.zeroize();
        self.confirm1.zeroize();
        self.pw2.zeroize();
        self.confirm2.zeroize();
        self.merge_pw1.zeroize();
        self.merge_pw2.zeroize();
    }

    /// Point the start page at the build script's sample vault, fill in its two demo
    /// passwords, and open it — the one-click version of the walk-through in "Trying it out
    /// on a sample vault".
    ///
    /// This can only ever OPEN, never create. `self.sample_vault` is resolved once, in the
    /// constructor, so by the time the button is clicked the directory may be gone (a
    /// `cargo clean` in another terminal removes it — it lives under `target/`). Left to
    /// `submit_open_or_create`, a missing vault means `AuthMode::Create`, and in a `--write`
    /// session that would silently BUILD a real vault locked with the two publicly-known
    /// demo passwords, at a path the user believed already held a throwaway sample. The
    /// same arm catches a directory whose name is not valid UTF-8, where the lossy
    /// round-trip through `vault_name` would resolve to a different, non-existent path.
    fn open_sample_vault(&mut self, dir: std::path::PathBuf) {
        self.wipe_passwords();
        self.vault_root = dir.parent().map(|p| p.display().to_string()).unwrap_or_default();
        self.vault_name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.refresh_discovered_vaults();
        self.recompute_vault_path();
        if self.auth_mode != AuthMode::Unlock {
            self.sample_vault = None; // it is gone; stop offering it
            self.auth_error =
                Some(format!("No sample vault at {} any more — rebuild it with scripts/build.sh.", dir.display()));
            return;
        }

        self.pw1.push_str(crate::launch::SAMPLE_PW1);
        self.pw2.push_str(crate::launch::SAMPLE_PW2);
        self.submit_open_or_create(false);
    }

    /// Clear every piece of PER-VAULT UI state to its fresh-launch default. Called on each
    /// successful open so a newly-unlocked vault never inherits the previous session's edit
    /// buffers (which can hold cleartext passwords), armed delete, active filters/search, or
    /// reveal toggles. Without this, an error path that drops the vault back to the unlock
    /// screen WITHOUT going through the constructor (e.g. a change-password or merge-apply
    /// failure) leaves vault A's state visible after vault B is opened — cross-vault secret
    /// leakage and confusing filter carryover. The edit buffers are `Zeroize`-on-drop, so
    /// replacing them here also wipes any secret they held.
    fn reset_per_vault_ui_state(&mut self) {
        self.tab = Tab::Urgent;
        self.edit_urgent = None;
        self.edit_instruction = None;
        self.edit_trustwill = None;
        self.edit_asset = None;
        self.edit_account = None;
        self.edit_realestate = None;
        self.edit_taxfiling = None;
        self.edit_general = None;
        self.pending_account_delete = None;
        // Reveal + grouping return to the saved view DEFAULTS (not hard false), matching the
        // constructor and the tab-switch reset.
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.acct_grouped = self.group_accounts_default;
        self.asset_grouped = self.group_assets_default;
        // Filters + searches back to "no filter".
        self.acct_filter_type.clear();
        self.acct_filter_subtype.clear();
        self.acct_filter_owner.clear();
        self.acct_filter_title.clear();
        self.acct_filter_review = false;
        self.acct_search_user.clear();
        self.asset_filter_review = false;
        self.link_search.clear();
        // Any half-typed document-upload inputs from the prior vault.
        self.clear_doc_inputs();
    }

    /// Leave the merge flow: drop the opened source vault + computed plan and wipe the
    /// source passwords. Called on cancel, on apply, and whenever Config/Merge is entered.
    fn reset_merge(&mut self) {
        self.merge_source = None;
        self.merge_plan = None;
        self.merge_error = None;
        self.wipe_merge_pw();
    }

    /// Zeroize + clear the two source-vault password buffers.
    fn wipe_merge_pw(&mut self) {
        self.merge_pw1.zeroize();
        self.merge_pw2.zeroize();
        self.merge_pw1.clear();
        self.merge_pw2.clear();
    }

    // `&mut egui::Ui` is the drawing surface, borrowed mutably so widgets can be
    // added to it. egui is immediate-mode: this method re-runs every frame.
    fn ui_auth(&mut self, ui: &mut egui::Ui) {
        // The lock screen is the app's front door and the only screen an heir may ever
        // see, so it is presented as one centered, width-limited card rather than a
        // full-width form: a narrow measure is easier to read, and the card gives the
        // password fields a visible boundary. Purely presentational — `ui_auth_inner`
        // holds the entire flow unchanged.
        let accent = accent(self.theme);
        // Everything decorative on this screen is spent through `k`, so that a window too
        // short for the comfortable layout gives up padding instead of putting the password
        // fields behind a scrollbar. Sampled once, before anything is drawn, so every gap
        // below is measured against the same height.
        let k = auth_space_scale(ui.available_height());
        ui.add_space(24.0 * k);
        vaultis_logo(ui, accent, k);
        ui.add_space(14.0 * k);
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);
            card(ui, |ui| {
                self.ui_auth_inner(ui, k);
            });
            ui.add_space(10.0 * k);
            // The mode the session will open in, stated before the password is typed
            // rather than discovered afterwards by a control that is missing.
            if self.writable {
                ui.label(egui::RichText::new("This session can make changes (--write).").weak().small());
            } else {
                ui.label(
                    egui::RichText::new("🔒 Read-only session — relaunch with --write to make changes.")
                        .weak()
                        .small(),
                );
            }

            // Only shown on the actual lock screen (never mid-"Change master passwords" —
            // this same widget renders that screen too, and clicking Sample would abandon
            // an in-progress password change and open an unrelated vault instead), and only
            // when `scripts/build.sh` actually built a demo vault at the resolved location
            // (see `launch::sample_vault_dir`) — never a button that would fail on click.
            // One click fills in its folder and its two demo passwords (sample1/sample2)
            // and opens it directly, honouring whatever read-only/write mode this session
            // was launched in, same as any other vault.
            if self.auth_mode != AuthMode::ChangePassword
                && let Some(dir) = self.sample_vault.clone()
            {
                ui.add_space(10.0 * k);
                if ui
                    .button("Sample vault")
                    .on_hover_text(
                        "Open a throwaway practice vault full of invented data — see \
                         “Trying it out on a sample vault” in Help.",
                    )
                    .clicked()
                {
                    self.open_sample_vault(dir);
                }
            }

            // --- Footer: the way in to the manual, from the front door ------------
            //
            // The lock screen may be the ONLY screen an heir ever sees, and until now
            // the manual was reachable only from the top bar — i.e. only *after*
            // successfully unlocking. Someone handed this program and two passwords in
            // an envelope had nowhere to turn before typing them.
            //
            // Composed as a quiet footer rather than a button beside "Unlock": the
            // primary action must stay unambiguous, so this sits below the card,
            // separated by a hairline, in the same weak/small register as the mode
            // line above it. The question is asked before the link is offered, because
            // someone who needs it is looking for a sentence that describes their
            // situation, not a control they already know how to use.
            ui.add_space(18.0 * k);
            // A hairline the width of the card, so the footer reads as part of the same
            // composition instead of floating text below it.
            ui.scope(|ui| {
                ui.set_max_width(360.0);
                ui.separator();
            });
            ui.add_space(10.0 * k);
            ui.label(
                egui::RichText::new("New to this, or settling an estate?").weak().small(),
            );
            ui.add_space(2.0 * k);
            if ui
                .link(egui::RichText::new("❓  Read the guide").color(accent))
                .on_hover_text(
                    "The built-in manual: what this program is, how to open a vault, \
                     and what to do if you are an executor. No password needed.",
                )
                .clicked()
            {
                // Wipe any partly-typed master passwords before leaving for the manual.
                // Reading a guide is an open-ended pause — plausibly the longest the
                // program is left unattended — and there is no reason for two plaintext
                // passwords to sit in memory (and in the fields, behind their mask) for
                // the duration. The desktop already wipes on a failed unlock and on the
                // change-password transition for the same reason; this is the same rule
                // applied to the same kind of moment. Retyping after reading the manual
                // is the expected flow anyway.
                self.wipe_passwords();
                self.auth_error = None;
                // Back must come here, not to the vault UI — there is no vault open yet.
                self.help_return = Screen::Auth;
                self.screen = Screen::Help;
            }
            ui.add_space(6.0 * k);
        });
    }

    /// The unlock/create/change-password form itself (see [`Self::ui_auth`], which
    /// frames it).
    ///
    /// `k` is the caller's [`auth_space_scale`], applied to this form's gaps for the same
    /// reason it is applied outside the card: on a short window the screen tightens rather
    /// than scrolls. Only the gaps — the fields, the button and the messages between them
    /// keep their full size at every window height.
    fn ui_auth_inner(&mut self, ui: &mut egui::Ui, k: f32) {
        // `match` used as an expression: it yields a `(heading, help)` pair which
        // we immediately destructure into two named bindings.
        ui.add_space(4.0 * k);
        // On the start page (not the in-vault Change-password flow) the user picks the vault
        // by ROOT + a collapsed "Vault" box: an editable ROOT path scanned (one level deep)
        // for vaults, and a Vault box that the dropdown fills — pick an existing vault, or
        // TYPE a new folder name to create one. Both editable in read-only AND --write mode.
        // The open target is always `<root>/<name>`. Rendered FIRST so the heading/confirm
        // fields below reflect the just-updated mode.
        if self.auth_mode != AuthMode::ChangePassword {
            // Deferred edits/picks gathered during the (borrow-locked) closure, applied after
            // it returns so the handlers can take `&mut self` freely.
            let mut root_changed = false;
            let mut name_changed = false;
            let mut picked: Option<String> = None;
            // The dropdown's button text: the current name, or a placeholder.
            let current = self.vault_name.trim().to_string();
            let selected_text = if !current.is_empty() {
                current.clone()
            } else if self.discovered_vaults.is_empty() {
                "(no vaults found)".to_string()
            } else {
                "— choose —".to_string()
            };
            ui.vertical_centered(|ui| {
                // Editable ROOT path: the folder scanned (one level deep) for vaults.
                ui.label("Vault root");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.vault_root)
                        .hint_text("/path/that/holds/vault-folders")
                        .desired_width(fit(ui, 360.0)),
                );
                root_changed = resp.changed();
                ui.add_space(4.0 * k);
                // The "Vault" control: an editable leaf-name box plus a dropdown of the
                // vaults discovered under the root. Pick one to fill the box (→ Unlock), or
                // type a new name (→ Create, in --write mode). Empty = the root itself.
                ui.label("Vault");
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.vault_name)
                            .hint_text("vault name")
                            .desired_width(fit(ui, 244.0)),
                    );
                    name_changed = resp.changed();
                    egui::ComboBox::from_id_salt("vault_picker")
                        .selected_text(selected_text)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for name in &self.discovered_vaults {
                                if ui.selectable_label(current == *name, name).clicked() {
                                    picked = Some(name.clone());
                                }
                            }
                        });
                });
                // Surface a scan problem (root unreadable, or entries skipped) plainly.
                if let Some(warn) = &self.vault_scan_warning {
                    ui.colored_label(egui::Color32::from_rgb(190, 120, 50), warn);
                }
            });
            if root_changed {
                self.refresh_discovered_vaults();
                self.recompute_vault_path();
                // Keep the default backup destination tracking the root until the vault is
                // unlocked (the Config backup field is freely editable afterwards).
                self.backup_dest = records::unquote_path(&self.vault_root).to_string();
                // Preferences live in the root, so a new root means a new (or absent)
                // prefs.json — adopt it now rather than at the next launch.
                self.adopt_root_prefs(ui.ctx());
            }
            if name_changed {
                self.recompute_vault_path();
            }
            if let Some(name) = picked {
                self.select_vault(&name);
            }
            ui.add_space(8.0 * k);
        }

        // `auth_mode` answers "does a vault exist at the current target?", which is a fact
        // about the DISK and stays true regardless of how this session was launched. Whether
        // the create AFFORDANCES are shown is a separate question, and the answer in a
        // read-only session is no: creating is refused at submit (see `submit_open_or_create`),
        // so offering a "Create vault" heading, a "choose two passwords" instruction, two
        // confirmation fields and a "Create vault" button describes an action this session
        // cannot perform. An heir who was handed the View shortcut and lands on a folder with
        // no vault should not be walked through creating one.
        //
        // The root/name fields stay live either way — retyping them to find the real vault is
        // exactly what that heir needs to do — and the warning below says why creating is not
        // on offer.
        let offer_create = self.auth_mode == AuthMode::Create && self.writable;
        let (heading, help) = match self.auth_mode {
            AuthMode::Create if offer_create => {
                ("Create vault", "Choose two passwords. Both are required to open this vault.")
            }
            // Read-only with nothing at the target: the screen is still the way IN to a vault,
            // so it reads as one rather than as a create form that will be refused.
            AuthMode::Create => ("Unlock vault", "Enter both passwords to unlock."),
            AuthMode::Unlock => ("Unlock vault", "Enter both passwords to unlock."),
            AuthMode::ChangePassword => ("Change master passwords", "Set two new passwords."),
        };
        // Confirmations exist to catch a typo in a password being SET. Nothing is being set
        // here unless a vault is actually being created or its passwords changed.
        let confirm = offer_create || self.auth_mode == AuthMode::ChangePassword;

        // `|ui| { ... }` is a closure (anonymous function). egui passes a child
        // `ui` into it so everything inside is laid out vertically and centered.
        ui.vertical_centered(|ui| {
            ui.heading(heading);
            ui.label(egui::RichText::new(format!("Vault: {}", self.path.display())).weak());
            ui.label(help);
            // In read-only mode an empty directory can't be created — say so plainly, but
            // only once that is actually true. `auth_mode == Create` merely means nothing
            // exists at the CURRENT `<root>/<name>` — which is also the state of a totally
            // blank start page (nothing specified yet: not a real warning) and of a root
            // that DOES hold vaults but has none picked yet (the dropdown is sitting right
            // there — "no vault in this folder" would be actively wrong). So this is
            // gated to the cases where it is true: a root was actually given, and either a
            // specific (nonexistent) name was typed, or the root holds no vaults at all.
            let create_blocked_by_read_only = self.auth_mode == AuthMode::Create
                && !self.writable
                && !self.vault_root.trim().is_empty()
                && (!self.vault_name.trim().is_empty() || self.discovered_vaults.is_empty());
            if create_blocked_by_read_only {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 120, 50),
                    "No vault in this folder. Read-only — relaunch with --write to create one.",
                );
            }
        });
        ui.add_space(16.0 * k);

        // Track whether the user requested submission; `|=` ORs in `true` if any
        // password field had Enter pressed (see `password_field`'s return value).
        let mut submit = false;
        // A built-in Ctrl+C/cut of a master-password field surfaces here so we can arm
        // the clipboard auto-clear/exit-wipe (the field can't reach `self` itself).
        let mut copied: Option<Zeroizing<String>> = None;
        egui::Grid::new("auth_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Password 1");
            // `&mut self.pw1` lends the field to the widget so typing updates it.
            submit |= password_field(ui, "auth_pw1", &mut self.pw1, &mut copied);
            ui.end_row();
            if confirm {
                ui.label("Confirm password 1");
                submit |= password_field(ui, "auth_confirm1", &mut self.confirm1, &mut copied);
                ui.end_row();
            }
            ui.label("Password 2");
            submit |= password_field(ui, "auth_pw2", &mut self.pw2, &mut copied);
            ui.end_row();
            if confirm {
                ui.label("Confirm password 2");
                submit |= password_field(ui, "auth_confirm2", &mut self.confirm2, &mut copied);
                ui.end_row();
            }
        });
        // Route a copied master password through the hardened + armed clipboard path.
        if let Some(pw) = copied {
            self.copy_to_clipboard(pw);
        }

        ui.add_space(8.0 * k);
        // `&self.auth_error` borrows the Option so we can read the message
        // without moving it out; show it only when an error is present.
        if let Some(err) = &self.auth_error {
            ui.colored_label(egui::Color32::from_rgb(190, 50, 50), err);
            ui.add_space(4.0 * k);
        }

        ui.horizontal(|ui| {
            // Same reasoning as the heading above: a read-only session cannot create, so the
            // button never offers to. `offer_create` already folds in `writable`.
            let label = match self.auth_mode {
                AuthMode::Create if offer_create => "Create vault",
                AuthMode::Create | AuthMode::Unlock => "🔓 Unlock",
                AuthMode::ChangePassword => "Change passwords",
            };
            // The one action of this screen, drawn as the primary (filled) button.
            let accent = accent(self.theme);
            if ui
                .add_sized(
                    [150.0, 28.0],
                    egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE)).fill(accent),
                )
                .clicked()
            {
                submit = true;
            }
            if self.auth_mode == AuthMode::ChangePassword && ui.button("Cancel").clicked() {
                self.auth_error = None;
                self.wipe_passwords();
                self.screen = Screen::Main;
            }
        });

        if submit {
            self.submit_auth();
        }
    }

    // --- Main: top bar + active tab -----------------------------------------

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        // Remember the active tab so a tab switch can reset the global reveal toggles
        // below: reveal is meant to be a momentary, in-context action, so it must not
        // persist into a later visit and expose every password to a bystander.
        let prev_tab = self.tab;
        let accent = accent(self.theme);

        // Row 1 — identity on the left, global actions on the right. `Sides` is the
        // primitive built for exactly this: it sizes the gap between the two groups
        // from the actual available width in a single pass. `shrink_left` lays the
        // ACTIONS out first and lets the vault name give up space, so the buttons can
        // never be pushed out of the window by a long name — and, unlike a
        // right-to-left layout nested in a wrapping row, there is no width estimate to
        // disagree with itself between frames.
        // Precomputed so the two `Sides` closures capture only plain values, not
        // `self` — the actions closure needs to MUTATE self, and the file's standard
        // deferred-action pattern (record the click, act after rendering) keeps the
        // borrows disjoint.
        let vault_path = self.path.display().to_string();
        let vault_name = self
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "vault".to_string());
        let writable = self.writable;
        let (mut go_help, mut go_config, mut go_passwords, mut do_quit) = (false, false, false, false);

        egui::containers::Sides::new().shrink_left().show(
            ui,
            |ui| {
                // Which vault is open — the folder name, with the full path on hover.
                // Two windows onto two vaults look identical without this.
                ui.label(egui::RichText::new("🗄").color(accent).size(16.0)).on_hover_text(&vault_path);
                ui.add(egui::Label::new(egui::RichText::new(&vault_name).strong()).truncate())
                    .on_hover_text(&vault_path);
                // The mode badge: quiet when writable, loud when not. A read-only session
                // hides its write controls, so the badge is what explains their absence.
                if writable {
                    badge(ui, "WRITE", accent);
                } else {
                    badge(ui, "🔒 READ-ONLY", egui::Color32::from_rgb(190, 105, 10));
                }
            },
            |ui| {
                // The right group is laid out right-to-left, hence the reversed order.
                do_quit = ui
                    .button("Quit")
                    .on_hover_text("Close the window (secrets are wiped and the clipboard cleared)")
                    .clicked();
                go_help = ui.button("❓ Help").on_hover_text("The built-in manual").clicked();
                go_config = ui
                    .button("⚙ Config")
                    .on_hover_text("Appearance, view defaults, type lists, export, backup, storage")
                    .clicked();
                // Change-password is a write; only offer it when writable.
                // `&&` short-circuits: the button is only drawn/evaluated when
                // `writable` is true, so read-only mode hides it entirely.
                go_passwords =
                    writable && ui.button("🔑 Passwords").on_hover_text("Change the vault's two passwords").clicked();
            },
        );

        if do_quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if go_help {
            self.help_return = Screen::Main;
            self.screen = Screen::Help;
        }
        if go_config {
            // Seed the redundancy picker from the live setting each time Config opens, so
            // the combo reflects the current value (and its selection survives across
            // frames until Apply).
            self.cfg_redundancy = self.vault_ref().redundancy();
            self.screen = Screen::Config;
        }
        if go_passwords {
            self.auth_mode = AuthMode::ChangePassword;
            self.auth_error = None;
            self.wipe_passwords();
            self.screen = Screen::Auth;
        }

        ui.add_space(6.0);

        // Row 2 — the tab strip. Each tab carries a glyph so it is recognisable by
        // shape before the label is read, and the active one gets an accent underline.
        //
        // The strip WRAPS onto further lines when the window is too narrow to hold it on one
        // (`horizontal_wrapped`), rather than sitting in the horizontal ScrollArea it used to.
        // A scrolling strip hid tabs off the right edge behind a scrollbar the user had to
        // notice and drag; wrapping keeps every tab visible and clickable at any width, which
        // is what a navigation bar has to guarantee. The top panel sizes itself to its
        // content, so an extra line pushes the body down instead of overlapping it.
        ui.horizontal_wrapped(|ui| {
            // Each tab keeps its label on ONE line, so the wrapping happens BETWEEN tabs (a whole
            // button moves down) rather than inside a multi-word label like "Assets and
            // Liabilities". Set on the strip's own Ui so every tab is a direct child of the
            // wrapped layout — which is what lets that layout see each button's full width and
            // decide to start a new row.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            tab_button(ui, &mut self.tab, Tab::Urgent, "❗ URGENT", accent);
            tab_button(ui, &mut self.tab, Tab::Instructions, "📝 Instructions", accent);
            tab_button(ui, &mut self.tab, Tab::TrustWill, "⚖ Trust and Will", accent);
            tab_button(ui, &mut self.tab, Tab::Assets, "💰 Assets and Liabilities", accent);
            tab_button(ui, &mut self.tab, Tab::Accounts, "🔑 Accounts", accent);
            tab_button(ui, &mut self.tab, Tab::RealEstate, "🏠 Real Estate", accent);
            tab_button(ui, &mut self.tab, Tab::Taxes, "📃 Taxes", accent);
            tab_button(ui, &mut self.tab, Tab::GeneralDocuments, "📁 General Documents", accent);
            tab_button(ui, &mut self.tab, Tab::Summary, "📊 Summary", accent);
        });
        // Reset the global reveal toggles when the user switches tabs (see prev_tab above):
        // reveal is momentary, so a stale "reveal all" must not persist into a later tab
        // visit. The reset target is the saved "reveal all by default" preference, not a
        // hardcoded `false`: when that pref is OFF this re-masks exactly as before, and when
        // it is ON every tab re-opens revealed (the user's chosen default). Also clear the
        // shared document-input buffers so a half-typed "Upload from" path / name / subfolder
        // from one tab does not linger in the next tab's attach form.
        if self.tab != prev_tab {
            self.reveal_all = self.reveal_default;
            self.re_reveal_all = self.reveal_default;
            self.clear_doc_inputs();
        }
    }

    // --- Help screen ---------------------------------------------------------

    /// The in-app manual: a searchable, topic-navigated browser over the content in
    /// [`crate::gui_help`]. Reachable from the top-bar "Help" button.
    ///
    /// All of the text (and the search) lives in `gui_help`; this only supplies the
    /// live facts the manual quotes back — where this vault and the preferences file
    /// are — and routes the Back button.
    fn ui_help(&mut self, ui: &mut egui::Ui) {
        let ctx = crate::gui_help::HelpContext {
            vault: self.path.display().to_string(),
            prefs: crate::prefs_path(&self.vault_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none yet — created when you change a setting)".into()),
            writable: self.writable,
        };
        if crate::gui_help::ui(ui, &mut self.help, &ctx, accent(self.theme)) {
            // Back to wherever Help was opened from — the vault UI, or the lock screen.
            self.screen = self.help_return;
        }
    }

    // --- Config screen -------------------------------------------------------

    fn ui_config(&mut self, ui: &mut egui::Ui) {
        let accent = accent(self.theme);
        // Back sits FIRST, at the left edge where a back control is looked for, and
        // the heading follows it — the old order put the way out after the title.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                self.screen = Screen::Main;
            }
            ui.add_space(4.0);
            section_heading(ui, "Configuration", accent);
        });
        ui.add_space(4.0);
        ui.separator();
        // Show where this vault lives on disk (the vault.pmv path; its parent dir holds
        // the manifest/ and volume/ too).
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Vault location").weak().small());
            ui.label(egui::RichText::new(self.path.display().to_string()).monospace().small());
        });
        if !self.writable {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "🔒  Read-only: no vault field can be edited. The color theme and the view \
                     defaults below can still be changed (they are local preferences); \
                     backup and document export are still available.",
                )
                .color(egui::Color32::from_rgb(170, 90, 0)),
            );
        }

        // These `bool` flags are the deferred-action pattern: rendering only
        // *sets* them; the actual vault mutations happen after the closures below
        // return, so we never hold a render-time borrow of `self` and a write
        // borrow at the same time.
        let mut add_asset = false;
        let mut add_account = false;
        let mut add_subtype = false;
        let mut do_backup = false;
        let mut set_export = false;
        let mut set_volume = false;
        let mut set_redundancy = false;
        let mut start_merge = false;
        let mut sync_types = false;
        // Deferred DELETE actions: which category the user clicked × on (handled after
        // the render closures, same borrow-discipline as the add_* flags).
        let mut remove_asset: Option<String> = None;
        let mut remove_account: Option<String> = None;
        let mut remove_subtype: Option<(String, String)> = None;
        // Snapshot the category lists + volume cap (from the open vault) before the
        // render closure borrows `self` mutably for the text inputs.
        let cur_volume_mib = self.vault_ref().volume_max_size() / (1024 * 1024);
        // The current on-disk depth, to skip a no-op Apply. The picker's selection
        // lives in the PERSISTENT `self.cfg_redundancy` (seeded when Config opened),
        // not a frame-local, so it survives until the user clicks Apply.
        let cur_redundancy = self.vault_ref().redundancy();
        let cats = self.vault_ref().categories();
        let type_names = cats.account_type_names();
        // Owned snapshots so the render closures don't hold a borrow of `self`/`cats`.
        let asset_names: Vec<String> = cats.asset.clone();
        // Each account type with its subtypes kept as a list (so each gets its own ×).
        let account_list: Vec<(String, Vec<String>)> =
            cats.account.iter().map(|t| (t.name.clone(), t.subtypes.clone())).collect();

        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("config_scroll").show(ui, |ui| {
            // Appearance: a color-theme picker. Changing it applies live and is
            // saved to a small preferences file (it carries no vault data), so it
            // works in read-only mode too and persists to the next launch.
            config_heading(ui, "Appearance");
            egui::ComboBox::from_label("Color theme").selected_text(self.theme.label()).show_ui(ui, |ui| {
                for t in Theme::ALL {
                    ui.selectable_value(&mut self.theme, t, t.label());
                }
            });
            // Interface scale: the second styling axis. Applied and saved by `render`
            // the moment the selection changes, so the effect is immediate and survives
            // the next launch — like the theme, it is a preference in <vault_root>/prefs.json
            // and holds no vault data, so it works in read-only mode too.
            egui::ComboBox::from_label("Interface size")
                .selected_text(self.ui_scale.label())
                .show_ui(ui, |ui| {
                    for sc in UiScale::ALL {
                        ui.selectable_value(&mut self.ui_scale, sc, sc.label());
                    }
                });
            ui.label(
                egui::RichText::new(
                    "Scales the whole window — text, buttons and spacing together. \
                     Useful if the default is hard to read.",
                )
                .small()
                .weak(),
            );
            egui::ComboBox::from_label("Typeface")
                .selected_text(self.font.label())
                .show_ui(ui, |ui| {
                    for f in FontChoice::ALL {
                        ui.selectable_value(&mut self.font, f, f.label());
                    }
                });
            ui.label(
                egui::RichText::new(
                    "Both typefaces are built into the program — nothing is loaded from \
                     this computer, so it looks the same everywhere.",
                )
                .small()
                .weak(),
            );
            ui.add_space(14.0);

            // View defaults: cosmetic UI preferences (`<vault_root>/prefs.json`), not vault
            // content — so they work in read-only mode too and persist to the next launch.
            // Each checkbox binds to the saved-default field, saves on change, and applies to
            // the live view state so the effect is immediate; the saved value re-seeds these
            // on the next vault open (see `GuiApp::new` and the tab-switch reset).
            //
            // "Reveal all passwords by default" deliberately is NOT here. prefs.json sits
            // unencrypted beside the vault folders, so anyone who can write to the media
            // without knowing the passwords authors it — and a persisted reveal-all would let
            // that tampering unmask every password on open. Reveal stays a per-session toggle
            // that always starts off (see the prefs comment in `lib.rs`).
            config_heading(ui, "View defaults");
            if ui
                .checkbox(&mut self.group_assets_default, "Group assets by default")
                .changed()
            {
                crate::save_group_assets_default(&self.vault_root, self.group_assets_default);
                self.asset_grouped = self.group_assets_default;
            }
            if ui
                .checkbox(&mut self.group_accounts_default, "Group accounts by default")
                .changed()
            {
                crate::save_group_accounts_default(&self.vault_root, self.group_accounts_default);
                self.acct_grouped = self.group_accounts_default;
            }
            ui.add_space(14.0);

            config_heading(ui, "Asset / Liability types");
            // One chip per type with a delete (×) button. The × only deletes when the
            // type is unused by a live record (else a status message explains why).
            ui.horizontal_wrapped(|ui| {
                for name in &asset_names {
                    ui.label(egui::RichText::new(name).weak());
                    // The category list is stored independently of records; tag entries no
                    // live record uses so the user can see what's safe to delete.
                    if self.vault_ref().asset_type_usage(name) == 0 {
                        ui.label(egui::RichText::new("· unused").weak().italics());
                    }
                    if self.writable
                        && ui.small_button("×").on_hover_text(format!("Delete “{name}” (only if unused)")).clicked()
                    {
                        remove_asset = Some(name.clone());
                    }
                    ui.add_space(8.0);
                }
            });
            ui.horizontal(|ui| {
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_asset_type).hint_text("New type").desired_width(fit(ui, 240.0)),
                );
                if self.writable && ui.button("Add type").clicked() {
                    add_asset = true;
                }
            });

            ui.add_space(14.0);
            config_heading(ui, "Account types & subtypes");
            // Each type on its own row: a × to delete the type (blocked while it has
            // subtypes or is in use), then each subtype with its own × (blocked if used).
            for (name, subs) in &account_list {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(name).strong());
                    if self.vault_ref().account_type_usage(name) == 0 {
                        ui.label(egui::RichText::new("· unused").weak().italics());
                    }
                    if self.writable
                        && ui
                            .small_button("×")
                            .on_hover_text("Delete type (only if it has no subtypes and is unused)")
                            .clicked()
                    {
                        remove_account = Some(name.clone());
                    }
                    ui.label(":");
                    if subs.is_empty() {
                        ui.label(egui::RichText::new("—").weak());
                    }
                    for sub in subs {
                        ui.label(egui::RichText::new(sub).weak());
                        if self.vault_ref().account_subtype_usage(name, sub) == 0 {
                            ui.label(egui::RichText::new("· unused").weak().italics());
                        }
                        if self.writable
                            && ui.small_button("×").on_hover_text(format!("Delete subtype “{sub}” (only if unused)")).clicked()
                        {
                            remove_subtype = Some((name.clone(), sub.clone()));
                        }
                        ui.add_space(6.0);
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_account_type)
                        .hint_text("New account type")
                        .desired_width(fit(ui, 220.0)),
                );
                if self.writable && ui.button("Add type").clicked() {
                    add_account = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Add subtype to:");
                // Pick the type the subtype belongs to.
                let cur = if self.new_subtype_for.is_empty() { "(choose type)".to_string() } else { self.new_subtype_for.clone() };
                ui.add_enabled_ui(self.writable, |ui| {
                    egui::ComboBox::from_id_salt("subtype_for").selected_text(cur).show_ui(ui, |ui| {
                        for name in &type_names {
                            ui.selectable_value(&mut self.new_subtype_for, name.clone(), name);
                        }
                    });
                });
                ui.add_enabled(
                    self.writable,
                    egui::TextEdit::singleline(&mut self.new_subtype_name).hint_text("New subtype").desired_width(fit(ui, 180.0)),
                );
                if self.writable && ui.button("Add subtype").clicked() {
                    add_subtype = true;
                }
            });

            ui.add_space(16.0);
            ui.separator();
            config_heading(ui, "Export directory");
            ui.label(
                egui::RichText::new(
                    "Where the per-document Export buttons write the decrypted file. Each export \
                     is saved under this directory, recreating the document's folder structure from \
                     inside the vault — you are never asked for a path at export time. Stored as a \
                     local preference (not in the vault), so it can be set even in read-only mode.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("Export directory:");
                // Deliberately NOT gated on `writable`: the export dir is a local preference,
                // so a read-only session (e.g. an heir) can set where to extract documents.
                ui.add(egui::TextEdit::singleline(&mut self.export_dir).hint_text("/path/to/exports").desired_width(fit(ui, 340.0)));
                if ui.button("Set").clicked() {
                    set_export = true;
                }
            });

            ui.add_space(16.0);
            ui.separator();
            config_heading(ui, "Backup");
            ui.label(
                egui::RichText::new(
                    "Copies the encrypted vault and its document archive into a directory, \
                     timestamped to the second. Nothing is decrypted.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("Destination directory:");
                ui.add(egui::TextEdit::singleline(&mut self.backup_dest).hint_text("/path/to/backups").desired_width(fit(ui, 340.0)));
                if ui.button("Backup now").clicked() {
                    do_backup = true;
                }
            });

            if self.writable {
                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Storage — volume size");
                ui.label(
                    egui::RichText::new(format!(
                        "New documents roll into a fresh volume once a partition passes this size. \
                         Current: {cur_volume_mib} MiB. Changing it affects only future placement."
                    ))
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("New size (MiB):");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg_volume_size).hint_text("e.g. 256").desired_width(fit(ui, 140.0)));
                    if ui.button("Set volume size").clicked() {
                        set_volume = true;
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Vault file redundancy (advanced)");
                ui.label(
                    egui::RichText::new(
                        "Keeps extra encrypted copies of the small vault file so a damaged \
                         vault.pmv can be recovered in place: a same-generation mirror plus N \
                         prior generations (also an 'undo last save'). 0 = off. This does NOT \
                         replace off-device backups, and it leaves more old encrypted data on disk.",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("Copies to keep:");
                    egui::ComboBox::from_id_salt("redundancy")
                        .selected_text(if self.cfg_redundancy == 0 { "Off".to_string() } else { self.cfg_redundancy.to_string() })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cfg_redundancy, 0, "Off");
                            for n in 1..=5u32 {
                                ui.selectable_value(&mut self.cfg_redundancy, n, n.to_string());
                            }
                        });
                    if ui.button("Apply").clicked() {
                        set_redundancy = true;
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Update from another vault");
                ui.label(
                    egui::RichText::new(
                        "Pull records that are newer (or new) in ANOTHER vault — together with the \
                         documents they reference — into this one. One-way and additive: it never \
                         deletes anything here. You'll choose the other vault's folder and enter its \
                         two passwords, then preview the exact changes before applying.",
                    )
                    .weak(),
                );
                if ui.button("Update from another vault…").clicked() {
                    start_merge = true;
                }

                ui.add_space(16.0);
                ui.separator();
                config_heading(ui, "Sync types from records");
                ui.label(
                    egui::RichText::new(
                        "Scan every record and add any asset/account type or subtype it uses that \
                         is missing from the lists above — useful after pulling in records (from a \
                         merge or import) whose types aren't yet listed here.",
                    )
                    .weak(),
                );
                if ui.button("Sync types from records").clicked() {
                    sync_types = true;
                }
            }
        });

        // Deferred actions (kept out of the closures to keep borrows simple).
        if add_asset {
            // `.trim()` returns a trimmed `&str`; `.to_string()` makes it owned.
            let name = self.new_asset_type.trim().to_string();
            // `.expect(...)` unwraps the open vault (safe on the config screen).
            // The call returns `Result<bool, _>`: `Ok(true)` = added,
            // `Ok(false)` = no-op (duplicate/empty), `Err` = save failure.
            match self.vault.as_mut().expect("vault open on config").add_asset_type(&name) {
                Ok(true) => {
                    self.status = format!("Added asset/liability type “{name}”.");
                    self.new_asset_type.clear();
                }
                Ok(false) => self.status = "Type is empty or already exists.".into(),
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if add_account {
            let name = self.new_account_type.trim().to_string();
            match self.vault.as_mut().expect("vault open on config").add_account_type(&name) {
                Ok(true) => {
                    self.status = format!("Added account type “{name}”.");
                    self.new_account_type.clear();
                }
                Ok(false) => self.status = "Type is empty or already exists.".into(),
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if add_subtype {
            let ty = self.new_subtype_for.clone();
            let sub = self.new_subtype_name.trim().to_string();
            if ty.is_empty() {
                self.status = "Choose an account type for the subtype.".into();
            } else {
                match self
                    .vault
                    .as_mut()
                    .expect("vault open on config")
                    .add_account_subtype(&ty, &sub)
                {
                    Ok(true) => {
                        self.status = format!("Added subtype “{sub}” under “{ty}”.");
                        self.new_subtype_name.clear();
                    }
                    Ok(false) => self.status = "Subtype is empty or already exists.".into(),
                    Err(e) => self.fail(format!("Save failed: {e}")),
                }
            }
        }
        // Deferred DELETE handlers. A refusal (in use / has subtypes) is a normal
        // status message, not a failure; only a real save error reads as "failed".
        if let Some(name) = remove_asset {
            // A save failure must surface in the conspicuous banner (via `fail`), not just the
            // weak status line — a refusal (in use / not found) is an ordinary status message.
            match self.vault.as_mut().expect("vault open on config").remove_asset_type(&name) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted asset/liability type “{name}”."),
                Ok(CategoryRemoval::InUse(n)) => self.status = format!("Can’t delete “{name}”: still used by {n} record(s)."),
                Ok(CategoryRemoval::NotFound) => self.status = format!("“{name}” was not found."),
                Ok(CategoryRemoval::HasSubtypes) => unreachable!("asset types have no subtypes"),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if let Some(name) = remove_account {
            match self.vault.as_mut().expect("vault open on config").remove_account_type(&name) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted account type “{name}”."),
                Ok(CategoryRemoval::HasSubtypes) => self.status = format!("Can’t delete “{name}”: delete its subtypes first."),
                Ok(CategoryRemoval::InUse(n)) => self.status = format!("Can’t delete “{name}”: still used by {n} account(s)."),
                Ok(CategoryRemoval::NotFound) => self.status = format!("“{name}” was not found."),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if let Some((ty, sub)) = remove_subtype {
            match self.vault.as_mut().expect("vault open on config").remove_account_subtype(&ty, &sub) {
                Ok(CategoryRemoval::Removed) => self.status = format!("Deleted subtype “{sub}” under “{ty}”."),
                Ok(CategoryRemoval::InUse(n)) => self.status = format!("Can’t delete “{sub}”: still used by {n} account(s)."),
                Ok(CategoryRemoval::NotFound) => self.status = format!("“{sub}” was not found under “{ty}”."),
                Ok(CategoryRemoval::HasSubtypes) => unreachable!("a subtype has no subtypes"),
                Err(e) => self.fail(format!("Delete failed: {e}")),
            }
        }
        if set_export {
            // Held for THIS SESSION only, never written to disk. It names where cleartext
            // exports land — the CSV carries every password in the clear — and the only file
            // this app writes is `<vault_root>/prefs.json`, which anyone with write access to
            // the vault media (but not the passwords) can edit. Persisting it there would let
            // tampering redirect those secrets; see the prefs comment in `lib.rs`. Normalize
            // the value: trimmed, with a pasted "Copy as path" quote pair stripped.
            let dir = records::unquote_path(&self.export_dir).to_string();
            self.export_dir = dir.clone();
            // Tell the user NOW if the folder they just picked is one every Export button
            // will refuse, instead of letting them discover it at the first export.
            self.status = match crate::checked_export_dir(&self.path, &dir) {
                _ if dir.is_empty() => "Export directory cleared.".into(),
                Ok(_) => format!("Export directory set to {dir} (this session only)."),
                Err(msg) => msg,
            };
        }
        if do_backup {
            let dest = records::unquote_path(&self.backup_dest).to_string();
            if dest.is_empty() {
                self.status = "Enter a backup destination directory.".into();
            } else if let Some(ov) = self.vault.as_ref() {
                // Use the OPEN handle's backup (reuses this session's write lock).
                // Calling the free `vault::backup` here would self-deadlock: it tries
                // to re-acquire the per-fd flock this session already holds → Locked.
                match ov.backup(Path::new(&dest)) {
                    Ok(p) => self.status = format!("Backed up to {}", p.display()),
                    Err(e) => self.fail(format!("Backup failed: {e}")),
                }
            }
        }
        if set_volume {
            // `.parse::<u64>()` parses text into an unsigned 64-bit integer,
            // returning a `Result` (`Err` if the text is not a number).
            match self.cfg_volume_size.trim().parse::<u64>() {
                // A "match guard": this arm matches `Ok(mib)` only if `mib >= 1`.
                Ok(mib) if mib >= 1 => {
                    // `.saturating_mul` multiplies but clamps at the max instead
                    // of overflowing/panicking.
                    let bytes = mib.saturating_mul(1024 * 1024);
                    match self.vault.as_mut().expect("vault open on config").set_volume_max_size(bytes) {
                        Ok(()) => {
                            self.status = format!("Volume size set to {mib} MiB (applies to future documents).");
                            self.cfg_volume_size.clear();
                        }
                        Err(e) => self.fail(format!("Save failed: {e}")),
                    }
                }
                // `_` is the catch-all arm: any other case (parse error, or 0).
                _ => self.status = "Enter a whole number of MiB (at least 1).".into(),
            }
        }
        if set_redundancy && self.cfg_redundancy != cur_redundancy {
            let choice = self.cfg_redundancy;
            match self.vault.as_mut().expect("vault open on config").set_redundancy(choice) {
                Ok(()) => {
                    self.status = if choice == 0 {
                        "Vault file redundancy turned off (extra copies removed).".into()
                    } else {
                        format!("Vault file redundancy set to {choice} (mirror + {choice} prior generation(s)).")
                    };
                }
                Err(e) => self.fail(format!("Save failed: {e}")),
            }
        }
        if start_merge {
            // Enter the merge flow with fresh state. Pre-fill the source folder with the
            // vault root (the folder that holds vaults) as a convenient starting point.
            self.reset_merge();
            self.merge_src_dir = records::unquote_path(&self.vault_root).to_string();
            self.screen = Screen::Merge;
        }
        if sync_types {
            match self.vault.as_mut().expect("vault open on config").sync_types_from_records() {
                Ok(0) => self.status = "Types already in sync — nothing to add.".into(),
                Ok(n) => self.status = format!("Added {n} type(s) from records to the lists."),
                Err(e) => self.fail(format!("Sync failed: {e}")),
            }
        }

        if !self.status.is_empty() {
            ui.separator();
            let text = egui::RichText::new(&self.status);
            let text = if is_export_caveat(&self.status) {
                text.color(export_caveat_color(ui.visuals())).strong()
            } else {
                text.weak()
            };
            ui.label(text);
        }
    }

    /// The "Update from another vault" screen: collect the source directory + its two
    /// passwords, preview the patch (`plan_merge_from`), then apply (`apply_merge_from`).
    /// Only reachable in `--write` mode (the entry button is gated). The opened source
    /// handle + computed plan live in `self.merge_*` between the preview and the apply.
    fn ui_merge(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("⬅ Back to Config").clicked() {
                self.reset_merge();
                self.screen = Screen::Config;
            }
            ui.add_space(4.0);
            section_heading(ui, "Update from another vault", accent(self.theme));
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("One-way and additive — nothing in this vault is ever deleted by an update.")
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        ui.separator();

        // Deferred actions (set in the render below, run after to avoid borrow clashes).
        let mut do_preview = false;
        let mut do_apply = false;
        let mut do_reset = false;
        let mut copied: Option<Zeroizing<String>> = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("merge_scroll").show(ui, |ui| {
            if self.merge_plan.is_none() {
                // --- Phase 1: collect the source folder + its two passwords. ---
                ui.label(
                    egui::RichText::new(
                        "Choose the OTHER vault's folder and enter ITS two passwords. The other vault \
                         is opened read-only; this vault is only changed when you click Apply on the \
                         next screen. Nothing here is deleted — only newer/new records are pulled in.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                egui::Grid::new("merge_form").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Other vault folder");
                    ui.add(egui::TextEdit::singleline(&mut self.merge_src_dir).hint_text("/path/to/other-vault-folder").desired_width(fit(ui, 360.0)));
                    ui.end_row();
                    ui.label("Other password 1");
                    password_field(ui, "merge_pw1", &mut self.merge_pw1, &mut copied);
                    ui.end_row();
                    ui.label("Other password 2");
                    password_field(ui, "merge_pw2", &mut self.merge_pw2, &mut copied);
                    ui.end_row();
                });
                ui.add_space(10.0);
                if ui.button("Preview update").clicked() {
                    do_preview = true;
                }
                if let Some(err) = &self.merge_error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), err);
                }
            } else if let Some(plan) = self.merge_plan.as_ref() {
                // --- Phase 2: show the computed plan; Apply or Cancel. ---
                let short = plan.source_vault_id.get(..8).unwrap_or(plan.source_vault_id.as_str());
                ui.label(egui::RichText::new(format!("From vault {short}")).weak());
                if plan.is_empty() && plan.skipped.is_empty() {
                    ui.add_space(6.0);
                    ui.label("Already up to date — no records in the other vault are newer or new.");
                } else {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!(
                        "{} record(s) to change ({} new, {} updated) · {} document(s) to copy ({} bytes)",
                        plan.records.len(),
                        plan.new_count(),
                        plan.updated_count(),
                        plan.blobs_to_copy(),
                        plan.bytes_to_copy(),
                    )).strong());
                    ui.add_space(6.0);
                    egui::Grid::new("merge_records").striped(true).num_columns(3).show(ui, |ui| {
                        ui.label(egui::RichText::new("Change").strong());
                        ui.label(egui::RichText::new("Type").strong());
                        ui.label(egui::RichText::new("Record / recency").strong());
                        ui.end_row();
                        for r in &plan.records {
                            ui.label(r.change.as_str());
                            ui.label(r.kind.as_str());
                            let recency = match r.current_updated_at {
                                Some(cur) => format!("{} ({} -> {})", r.label, format_time(cur), format_time(r.source_updated_at)),
                                None => format!("{} (new @ {})", r.label, format_time(r.source_updated_at)),
                            };
                            ui.label(recency);
                            ui.end_row();
                        }
                    });
                    if !plan.blobs.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Documents").strong());
                        for b in &plan.blobs {
                            let tag = if b.already_present { "already here" } else { "copy" };
                            ui.label(format!("  [{tag}] {} ({} bytes)", b.path, b.size));
                        }
                    }
                    if !plan.new_categories.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Category types to add (so the merged types show in Config)").strong());
                        for c in &plan.new_categories {
                            ui.label(format!("  + {c}"));
                        }
                    }
                    if !plan.skipped.is_empty() {
                        ui.add_space(8.0);
                        ui.colored_label(egui::Color32::from_rgb(190, 120, 50), "Skipped (not applied):");
                        for s in &plan.skipped {
                            ui.label(format!("  {} — {} — {}", s.kind.as_str(), s.label, s.reason));
                        }
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let can_apply = !plan.is_empty();
                    if ui.add_enabled(can_apply, egui::Button::new("Apply update")).clicked() {
                        do_apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_reset = true;
                    }
                });
            }
        });

        // A copied source-vault password (built-in Ctrl+C) routes through the hardened,
        // auto-clearing clipboard path, exactly like the unlock screen.
        if let Some(text) = copied {
            self.copy_to_clipboard(text);
        }

        if do_preview {
            self.merge_preview();
        }
        if do_apply {
            self.merge_apply();
        }
        if do_reset {
            // Cancel the preview but stay on the screen to re-enter credentials.
            self.reset_merge();
        }
    }

    /// Open the source vault read-only and compute the patch into `self.merge_plan`.
    /// Collapses the source's open errors into ONE generic message so this screen can't be
    /// used as a password-correctness oracle for the other vault (mirrors the unlock screen).
    fn merge_preview(&mut self) {
        self.merge_error = None;
        // The just-typed source-vault passwords are secrets: wipe them on EVERY exit path
        // (each validation early-return below, the open failure, the plan error, and success),
        // never leaving them resident in the heap buffers after this call.
        let dir = records::unquote_path(&self.merge_src_dir);
        if dir.is_empty() {
            self.merge_error = Some("Enter the other vault's folder.".into());
            self.wipe_merge_pw();
            return;
        }
        let src_path = crate::launch::vault_file(dir);
        if !src_path.exists() {
            self.merge_error = Some("No vault found in that folder.".into());
            self.wipe_merge_pw();
            return;
        }
        // Guard against merging this vault into itself.
        if same_vault_path(&src_path, &self.path) {
            self.merge_error = Some("That is this same vault — choose a different one.".into());
            self.wipe_merge_pw();
            return;
        }
        let source = match OpenVault::open_read_only(src_path, self.merge_pw1.as_bytes(), self.merge_pw2.as_bytes()) {
            Ok(v) => v,
            Err(_) => {
                // Single generic message for EVERY failure (wrong password, corrupt, etc.)
                // so the screen never confirms whether the entered passwords were right.
                self.merge_error = Some("Could not open that vault — wrong password(s) or unreadable.".into());
                self.wipe_merge_pw();
                return;
            }
        };
        let plan = match self.vault_ref().plan_merge_from(&source) {
            Ok(p) => p,
            Err(e) => {
                self.merge_error = Some(format!("Could not build the update: {e}"));
                self.wipe_merge_pw();
                return;
            }
        };
        // Keep the opened source + plan for the apply step; wipe the entered passwords now.
        self.merge_source = Some(source);
        self.merge_plan = Some(plan);
        self.wipe_merge_pw();
    }

    /// Apply the previewed patch (copy blobs, replace/insert records, save), then return to
    /// Config with a status summary. Recomputes against the held source handle internally.
    fn merge_apply(&mut self) {
        // Disjoint field borrows: `self.vault` (mut) and `self.merge_source` (shared).
        let result = match (self.vault.as_mut(), self.merge_source.as_ref()) {
            (Some(cur), Some(src)) => cur.apply_merge_from(src),
            _ => {
                self.merge_error = Some("Nothing to apply.".into());
                return;
            }
        };
        match result {
            Ok(report) => {
                self.status = format!(
                    "Updated from another vault: {} new, {} updated record(s); {} document(s) copied; {} type(s) added.{}",
                    report.records_added,
                    report.records_updated,
                    report.blobs_copied,
                    report.categories_added,
                    if report.records_skipped > 0 { format!(" {} skipped.", report.records_skipped) } else { String::new() },
                );
                self.reset_merge();
                self.screen = Screen::Config;
            }
            Err(e) => {
                // A failed apply may have poisoned the handle (the in-memory merge can no
                // longer be saved — see apply_merge_from's save-failure poisoning). Drop it
                // and return to the unlock screen so reopening loads the clean on-disk vault,
                // mirroring the change-password recovery path. Nothing committed is lost: the
                // merge did not persist, and any prior edits were already saved.
                self.vault = None;
                self.reset_merge();
                self.auth_mode = AuthMode::Unlock;
                self.screen = Screen::Auth;
                self.wipe_passwords();
                self.auth_error = Some(format!("Update interrupted: {e}. Unlock again to recover."));
            }
        }
    }

    // --- Tab: Instructions ---------------------------------------------------

    fn tab_urgent(&mut self, ui: &mut egui::Ui) {
        // Same shape as tab_instructions — a title + free-text-body note list — but for the
        // separate, first-in-order URGENT collection.
        let labels = label_list(&self.vault_ref().vault.urgent);
        let cur = self.edit_urgent.as_ref().map(|r| r.id.clone());
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "URGENT", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_urgent").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_urgent.as_mut() {
                    egui::Grid::new("urgent_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Details");
                    field_multiline(ui, &mut r.description, self.writable, 12);
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an urgent note");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_urgent = Urgent::new().ok();
        }
        if let Some(i) = select {
            self.edit_urgent = self.vault_ref().vault.urgent.get(i).cloned();
        }
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_urgent.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_urgent.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.urgent, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    // Show what was written (see `sync_edit_buffer`), or the footer keeps
                    // warning that the record the user just saved is unsaved.
                    self.sync_edit_buffer(Tab::Urgent);
                }
            }
            FormAction::Delete => self.delete_current(Tab::Urgent),
            _ => {}
        }
    }

    fn tab_instructions(&mut self, ui: &mut egui::Ui) {
        // Build the left-hand list (id+label pairs) from the vault's records.
        let labels = label_list(&self.vault_ref().vault.instructions);
        // `cur` = id of the record being edited, if any. `.as_ref()` borrows the
        // Option's contents; `.map(|r| r.id.clone())` runs the closure only when
        // `Some`, producing `Option<String>` (an owned copy of the id).
        let cur = self.edit_instruction.as_ref().map(|r| r.id.clone());
        // Deferred-action flags (filled during rendering, acted on afterwards).
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;

        // `ui.columns(2, |c| ...)`: `c` is a slice of two child UIs (left/right).
        two_col(ui, |c| {
            // Destructuring assignment into the outer `new`/`select` vars.
            // `cur.as_deref()` turns `Option<String>` into `Option<&str>` (a
            // borrowed view) without consuming `cur`.
            (new, select, export) = list_panel(&mut c[0], "Instructions", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // Shadow `ui` with a mutable borrow of the right column. "Shadowing"
            // reuses the name `ui` for a new binding within this block.
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_instructions").show(&mut c[1], |ui| {
                // `.as_mut()` borrows the edited record mutably so the form widgets
                // below can write directly into its fields.
                if let Some(r) = self.edit_instruction.as_mut() {
                    egui::Grid::new("instr_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 12);
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an instruction");
                }
            });
        });

        // Now apply the deferred actions outside the render closure.
        if export {
            self.export_current_tab_csv();
        }
        if new {
            // `Instruction::new()` returns a `Result`; `.ok()` discards any error
            // and yields `Option<Instruction>` (Some on success, None on error).
            self.edit_instruction = Instruction::new().ok();
        }
        if let Some(i) = select {
            // `.get(i)` returns `Option<&Instruction>` (None if out of range);
            // `.cloned()` turns that into an owned `Option<Instruction>`.
            self.edit_instruction = self.vault_ref().vault.instructions.get(i).cloned();
        }
        match action {
            FormAction::Save => {
                // Left/right-trim every field before persisting (whole-vault policy);
                // trim the live form too so the displayed values match what was saved.
                if let Some(r) = self.edit_instruction.as_mut() {
                    r.trim_fields();
                }
                // Let-chain: take an owned clone of the edited record AND a mutable
                // borrow of the vault, then upsert (insert-or-update) into it.
                if let Some(r) = self.edit_instruction.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.instructions, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    // Re-read the stored record into the form: `upsert` stamped its
                    // timestamp and history, so without this the buffer stays "different
                    // from the vault" forever and the footer warning never clears.
                    self.sync_edit_buffer(Tab::Instructions);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::Instructions),
            // `_ => {}` handles the remaining `FormAction::None` with a no-op.
            _ => {}
        }
    }

    // --- Tab: Trust and Will -------------------------------------------------

    fn tab_trustwill(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.trust_wills);
        let cur = self.edit_trustwill.as_ref().map(|r| r.id.clone());
        // `.and_then(|r| r.file.clone())` chains two Options: only if a record is
        // being edited AND it has an attached `file` do we get `Some(id)`. (Using
        // `.map` here would give a nested `Option<Option<…>>`; `and_then`
        // flattens it.)
        let attached: Vec<String> =
            self.attached_label(self.edit_trustwill.as_ref().and_then(|r| r.file.clone())).into_iter().collect();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Trust and Will", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_trustwill").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_trustwill.as_mut() {
                    egui::Grid::new("tw_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Document");
                        field_singleline(ui, &mut r.document, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Usage");
                    field_multiline(ui, &mut r.usage, self.writable, 8);
                    ui.separator();
                    docreq = doc_section(
                        ui,
                        &attached,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        self.writable,
                    )
                    .to_single();
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a document");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_trustwill = TrustWill::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_trustwill = self.vault_ref().vault.trust_wills.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::TrustWill);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_trustwill.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_trustwill.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.trust_wills, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::TrustWill);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::TrustWill),
            _ => {}
        }
    }

    // --- Tab: General Documents ----------------------------------------------

    fn tab_general(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.general_documents);
        let cur = self.edit_general.as_ref().map(|r| r.id.clone());
        let attached: Vec<String> =
            self.attached_label(self.edit_general.as_ref().and_then(|r| r.file.clone())).into_iter().collect();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;

        two_col(ui, |c| {
            (new, select, export) =
                list_panel(&mut c[0], "General Documents", "➕ New", &labels, cur.as_deref(), self.writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_general").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_general.as_mut() {
                    egui::Grid::new("gen_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, self.writable, 420.0);
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 8);
                    ui.separator();
                    docreq = doc_section(
                        ui,
                        &attached,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        self.writable,
                    )
                    .to_single();
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a document");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_general = GeneralDocument::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_general = self.vault_ref().vault.general_documents.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::General);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_general.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_general.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.general_documents, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::GeneralDocuments);
                }
            }
            FormAction::Delete => self.delete_current(Tab::GeneralDocuments),
            _ => {}
        }
    }

    // --- Tab: Assets and Liabilities ----------------------------------------

    fn tab_assets(&mut self, ui: &mut egui::Ui) {
        // Same card treatment as the Accounts filter row, so the two list tabs have
        // the same control strip in the same place.
        let accent_c = accent(self.theme);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                // Grouped tree: owner → Asset/Liability → type (empty levels skipped).
                ui.checkbox(&mut self.asset_grouped, "grouped tree")
                    .on_hover_text("Group the list by owner > asset/liability > type");
                ui.checkbox(&mut self.asset_filter_review, "review only")
                    .on_hover_text("Show only items flagged for review");
                if self.asset_filter_review {
                    badge(ui, "filtered", egui::Color32::from_rgb(190, 105, 10));
                }
            });
        });
        ui.add_space(6.0);
        let fr = self.asset_filter_review;
        // In grouped mode, the same review-filtered assets as an owner→kind→type tree
        // (built here so the render closure doesn't re-borrow `self`).
        let tree = if self.asset_grouped {
            Some(records::asset_tree(self.vault_ref().vault.assets.iter().filter(|a| !fr || a.review)))
        } else {
            None
        };
        // Iterator pipeline: walk assets by reference, keep only those passing the
        // filter closure (`!fr` = filter off, or the item is flagged), turn each
        // into an `(id, label)` tuple, and collect into a `Vec`.
        let labels: Vec<(String, String)> = self
            .vault_ref()
            .vault
            .assets
            .iter()
            .filter(|a| !fr || a.review)
            .map(|a| (a.id.clone(), a.label()))
            .collect();
        let cur = self.edit_asset.as_ref().map(|r| r.id.clone());
        // Flat-list arrow navigation: when not grouped, ↑/↓ move to the prev/next item.
        let nav_target = list_nav_target(ui, !self.asset_grouped, &labels, cur.as_deref());
        let attached: Vec<String> =
            self.attached_label(self.edit_asset.as_ref().and_then(|r| r.statement.clone())).into_iter().collect();
        let asset_types = self.vault_ref().categories().asset.clone();
        // Linked-accounts data for the record under edit, resolved BEFORE the mutable
        // `edit_asset` borrow inside the columns closure (same borrow discipline as the
        // Accounts tab's `subtypes` precompute): the linked ids with display labels — a
        // dangling id renders as the RAW id, see `linked_account_rows` — plus the
        // not-yet-linked accounts offered by the "add link" dropdown.
        let linked_rows: Vec<(String, String)> = self
            .edit_asset
            .as_ref()
            .map(|r| linked_account_rows(&self.vault_ref().vault.accounts, &r.linked_accounts))
            .unwrap_or_default();
        let link_candidates: Vec<(String, String)> = self
            .edit_asset
            .as_ref()
            .map(|r| link_candidates(&self.vault_ref().vault.accounts, &r.linked_accounts))
            .unwrap_or_default();
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = DocReq::None;
        let mut linkreq = LinkReq::None;

        two_col(ui, |c| {
            match &tree {
                // Grouped tree: owner → Asset/Liability → type → entry (leaf), empty levels
                // skipped. egui's CollapsingHeader gives the +/- expand control.
                Some(root) => {
                    let lp = &mut c[0];
                    // Same header as the flat `list_panel`, so switching to the tree
                    // does not change what the top of the pane looks like.
                    lp.horizontal_wrapped(|ui| {
                        let accent = ui_accent(ui);
                        section_heading(ui, "Assets and Liabilities", accent);
                        badge(ui, &format!("{}", labels.len()), accent);
                        ui.add_space(4.0);
                        if self.writable && ui.button("➕ New").clicked() {
                            new = true;
                        }
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
                    lp.add_space(4.0);
                    lp.separator();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("asset_tree").show(lp, |ui| {
                        let mut path: Vec<String> = Vec::new();
                        if let Some(s) = render_acct_node(ui, root, &mut path, cur.as_deref(), &labels, "asset") {
                            select = Some(s);
                        }
                    });
                }
                None => {
                    (new, select, export) =
                        list_panel(&mut c[0], "Assets and Liabilities", "➕ New", &labels, cur.as_deref(), self.writable, nav_target);
                }
            }
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_assets").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_asset.as_mut() {
                    let w = self.writable;
                    egui::Grid::new("asset_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Asset / Liability");
                        combo(ui, "asset_kind", &mut r.kind, &["Asset".to_string(), "Liability".to_string()], w);
                        ui.end_row();
                        ui.label("Owner");
                        field_singleline(ui, &mut r.owner, w, 420.0);
                        ui.end_row();
                        ui.label("Title");
                        field_singleline(ui, &mut r.title, w, 420.0);
                        ui.end_row();
                        ui.label("Beneficiary");
                        field_singleline(ui, &mut r.beneficiary, w, 420.0);
                        ui.end_row();
                        ui.label("Approximate value");
                        field_singleline(ui, &mut r.approx_value, w, 420.0);
                        ui.end_row();
                        ui.label("As-of date");
                        field_singleline_hint(ui, &mut r.as_of_date, w, 420.0, "YYYY-MM-DD");
                        ui.end_row();
                        ui.label("Institution");
                        field_singleline(ui, &mut r.institution, w, 420.0);
                        ui.end_row();
                        ui.label("Type");
                        combo(ui, "asset_type", &mut r.asset_type, &asset_types, w);
                        ui.end_row();
                        ui.label("URL");
                        field_singleline(ui, &mut r.url, w, 420.0);
                        ui.end_row();
                        ui.label("Review");
                        ui.add_enabled(w, egui::Checkbox::new(&mut r.review, "flag for review"));
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 4);
                    ui.separator();
                    // Cross-record links to Accounts (edited on the asset side ONLY; the
                    // Accounts form shows the read-only reverse view). Deferred like docreq.
                    linkreq = linked_accounts_section(
                        ui,
                        &linked_rows,
                        &link_candidates,
                        &mut self.link_search,
                        self.writable,
                    );
                    ui.separator();
                    docreq = doc_section(
                        ui,
                        &attached,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        self.writable,
                    )
                    .to_single();
                    action = form_buttons(ui, self.writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an asset or liability");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_asset = self.new_asset_seeded();
            self.clear_doc_inputs();
        }
        // A click wins over keyboard nav. A keyboard arrow CAN land in the same egui frame
        // as a button click (events are batched per repaint), and the deferred requests
        // below — Save/Delete, the document Export/Remove, and the link Add/Unlink — were
        // all captured against the CURRENTLY shown asset. Applying a same-frame nav swap
        // first would retarget them at the NEIGHBOUR.
        //
        // Today `list_nav_target`'s `focused()` guard already blocks this (clicking a
        // button focuses it), which is why the pinned test passes without this line. But
        // that protection is emergent, and Accounts was given this explicit guard by audit
        // A-8 while Assets — which has strictly MORE record-targeted deferred actions —
        // was left relying on the implicit one. Belt and braces, matching Accounts.
        let record_action_pending = new
            || !matches!(action, FormAction::None)
            || !matches!(docreq, DocReq::None)
            || !matches!(linkreq, LinkReq::None);
        if !record_action_pending {
            select = select.or(nav_target);
        }
        if let Some(i) = select
            && let Some((id, _)) = labels.get(i)
        {
            // Resolve by id (the list may be filtered by the review flag). The
            // `(id, _)` pattern keeps the id and ignores the label. `.find(|a|
            // ...)` returns the first matching element (`&a.id == id` compares the
            // borrowed ids); `.cloned()` makes an owned copy for the edit buffer.
            self.edit_asset = self.vault_ref().vault.assets.iter().find(|a| &a.id == id).cloned();
            self.clear_doc_inputs();
        }
        self.handle_doc(docreq, DocTarget::Asset);
        self.handle_link_req(linkreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_asset.as_mut() {
                    r.trim_fields();
                }
                // Validate before saving: every Asset/Liability must have an owner and a
                // NUMERIC approximate value, so the Summary tab can aggregate it. On failure,
                // surface the reason in the conspicuous banner and do NOT save the bad record.
                let invalid = self.edit_asset.as_ref().and_then(records::asset_validation_error);
                if let Some(msg) = invalid {
                    self.fail(msg);
                } else {
                    if let Some(r) = self.edit_asset.clone()
                        && let Some(ov) = self.vault.as_mut()
                    {
                        records::upsert(&mut ov.vault.assets, r);
                    }
                    if self.persist() {
                        self.status = "Saved.".into();
                        self.sync_edit_buffer(Tab::Assets);
                    }
                    // On failure persist() has already set the "Save failed: …" status.
                }
            }
            FormAction::Delete => self.delete_current(Tab::Assets),
            _ => {}
        }
    }

    // --- Tab: Summary --------------------------------------------------------

    /// The "Summary" tab: a flat table aggregating every Asset/Liability's approximate value
    /// by owner, split into asset buckets (Real Estate / Before Tax / After Tax) and liability
    /// buckets (Before Tax / After Tax), with per-owner totals + net worth and a grand-total
    /// row. Before Tax = retirement + HSA; After Tax = everything else (records::value_bucket).
    fn tab_summary(&mut self, ui: &mut egui::Ui) {
        let accent_c = accent(self.theme);
        ui.add_space(6.0);
        section_heading(ui, "Summary of Assets & Liabilities", accent_c);
        ui.label(
            egui::RichText::new(
                "Aggregated approximate values by owner. Cash = cash/savings/checking; Before Tax = retirement + HSA; After Tax = everything else.",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);
        let rows = records::owner_value_summary(self.vault_ref().vault.assets.iter());
        if rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("📊").size(28.0).color(accent_c.gamma_multiply(0.7)));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Nothing to summarise yet").strong());
                ui.label(
                    egui::RichText::new("Add records on the Assets and Liabilities tab and their values total up here.")
                        .weak()
                        .small(),
                );
            });
            return;
        }
        // Grand total across all owners.
        let mut total = records::OwnerValueRow { owner: "All owners".to_string(), ..Default::default() };
        for r in &rows {
            total.asset_real_estate += r.asset_real_estate;
            total.asset_cash += r.asset_cash; // BUG FIX: cash was omitted, understating Assets Σ / Net
            total.asset_before_tax += r.asset_before_tax;
            total.asset_after_tax += r.asset_after_tax;
            total.liability += r.liability;
        }
        // A headline row before the table: the three numbers someone opens this tab
        // to find, at a size they can read across a desk, instead of having to pick
        // them out of the bottom-right corner of an eight-column grid.
        ui.horizontal_wrapped(|ui| {
            stat_tile(ui, "Total assets", &crate::fmt_money(total.asset_total()), STAT_GOOD);
            stat_tile(ui, "Total liabilities", &crate::fmt_money(total.liability), STAT_BAD);
            // Net worth is the polarity number, so it takes the sign's color — and the
            // sign is in the text too, never color alone.
            stat_tile(
                ui,
                "Net worth",
                &crate::fmt_money(total.net()),
                if total.net() < 0.0 { STAT_BAD } else { STAT_GOOD },
            );
            stat_tile(ui, "Owners", &rows.len().to_string(), accent_c);
        });
        ui.add_space(12.0);
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("summary_scroll").show(ui, |ui| {
            egui::Grid::new("summary_grid").striped(true).num_columns(8).spacing([18.0, 6.0]).show(ui, |ui| {
                // Group header: ASSETS over its 5 value columns, LIABILITIES over its 1.
                ui.label("");
                ui.label(egui::RichText::new("ASSETS").strong().small().color(STAT_GOOD));
                ui.label("");
                ui.label("");
                ui.label("");
                ui.label("");
                ui.label(egui::RichText::new("LIABILITIES").strong().small().color(STAT_BAD));
                ui.label("");
                ui.end_row();
                // Column headers (Cash = cash/savings/checking; liabilities are not tax-split).
                for h in ["Owner", "Real Estate", "Cash", "Before Tax", "After Tax", "Assets Σ", "Liability", "Net"] {
                    ui.label(egui::RichText::new(h).strong());
                }
                ui.end_row();
                // One row per owner (monospace amounts so the digits line up).
                for r in &rows {
                    ui.label(egui::RichText::new(r.owner.as_str()).strong());
                    ui.monospace(crate::fmt_money(r.asset_real_estate));
                    ui.monospace(crate::fmt_money(r.asset_cash));
                    ui.monospace(crate::fmt_money(r.asset_before_tax));
                    ui.monospace(crate::fmt_money(r.asset_after_tax));
                    ui.monospace(crate::fmt_money(r.asset_total()));
                    // Liability and Net carry the reserved status colors; the sign is in
                    // the text as well, so the meaning never rests on color alone.
                    ui.label(egui::RichText::new(crate::fmt_money(r.liability)).monospace().color(
                        if r.liability > 0.0 { STAT_BAD } else { ui.visuals().text_color() },
                    ));
                    ui.label(
                        egui::RichText::new(crate::fmt_money(r.net()))
                            .monospace()
                            .color(if r.net() < 0.0 { STAT_BAD } else { STAT_GOOD }),
                    );
                    ui.end_row();
                }
                // Grand-total row (bold).
                ui.label(egui::RichText::new(total.owner.as_str()).strong());
                for v in [
                    total.asset_real_estate,
                    total.asset_cash,
                    total.asset_before_tax,
                    total.asset_after_tax,
                    total.asset_total(),
                    total.liability,
                    total.net(),
                ] {
                    ui.label(egui::RichText::new(crate::fmt_money(v)).strong().monospace());
                }
                ui.end_row();
            });
        });
    }

    // --- Tab: Accounts -------------------------------------------------------

    /// The Accounts that pass the current filters (type/subtype/owner/review) and
    /// the username search, as `(id, label)` pairs. Extracted from the render so it
    /// can be unit-tested; the search uses [`records::matches_search`].
    fn filtered_account_labels(&self) -> Vec<(String, String)> {
        self.vault_ref()
            .vault
            .accounts
            .iter()
            .filter(|a| self.account_passes_filters(a))
            .map(|a| (a.id.clone(), a.label()))
            .collect()
    }

    /// Whether an account passes the current Accounts filters (type/subtype/owner/
    /// title/review + the free-text search, which matches the username OR the title).
    /// Shared by the flat list and the grouped tree so both honour the same filters.
    fn account_passes_filters(&self, a: &Account) -> bool {
        (self.acct_filter_type.is_empty() || a.account_type == self.acct_filter_type)
            && (self.acct_filter_subtype.is_empty() || a.account_subtype == self.acct_filter_subtype)
            && (self.acct_filter_owner.is_empty() || a.owner == self.acct_filter_owner)
            && (self.acct_filter_title.is_empty() || a.title == self.acct_filter_title)
            && (!self.acct_filter_review || a.review)
            // Free-text search matches the username OR the title (empty query = all).
            && (records::matches_search_soundlike(&a.username, &self.acct_search_user)
                || records::matches_search_soundlike(&a.title, &self.acct_search_user))
    }

    /// Build a fresh Account for the "New" button, pre-populated from the active
    /// Accounts filters / username search so the entry starts in the bucket the user
    /// is viewing. The filter fields are "" when unset, leaving those fields blank.
    /// Nothing is persisted — this only seeds the edit buffer.
    fn new_account_from_filters(&self) -> Option<Account> {
        let mut a = Account::new().ok()?;
        a.title = self.acct_filter_title.clone();
        a.account_type = self.acct_filter_type.clone();
        a.account_subtype = self.acct_filter_subtype.clone();
        a.owner = self.acct_filter_owner.clone();
        a.username = self.acct_search_user.clone();
        Some(a)
    }

    /// Seed the edit buffer for a NEW account. When a record is currently open, the
    /// new entry inherits its *grouping* fields — account type, subtype, and owner —
    /// so several related logins can be entered in a row without re-picking the
    /// category each time; the identifying/secret fields (title, username, password,
    /// URL, notes) deliberately start blank. With nothing open it falls back to the
    /// active list filters (see [`Self::new_account_from_filters`]).
    fn new_account_seeded(&self) -> Option<Account> {
        match self.edit_account.as_ref() {
            Some(cur) => {
                let mut a = Account::new().ok()?;
                a.account_type = cur.account_type.clone();
                a.account_subtype = cur.account_subtype.clone();
                a.owner = cur.owner.clone();
                Some(a)
            }
            None => self.new_account_from_filters(),
        }
    }

    /// Seed the edit buffer for a NEW asset/liability. When a record is open the new
    /// entry inherits its grouping fields — kind (Asset/Liability), asset type, and
    /// owner — so a run of related holdings shares a category; the identifying fields
    /// (title, value, institution, dates, links) start blank. Blank when nothing is open.
    fn new_asset_seeded(&self) -> Option<AssetLiability> {
        let mut a = AssetLiability::new().ok()?;
        if let Some(cur) = self.edit_asset.as_ref() {
            a.kind = cur.kind.clone();
            a.asset_type = cur.asset_type.clone();
            a.owner = cur.owner.clone();
        }
        Some(a)
    }

    /// After saving an account, move any ACTIVE field filter to the saved record's
    /// value so the entry stays visible in the filtered list (changing a filtered
    /// field follows the entry rather than hiding it). Unset filters stay unset.
    fn sync_account_filters_to(&mut self, a: &Account) {
        if !self.acct_filter_type.is_empty() {
            self.acct_filter_type = a.account_type.clone();
        }
        if !self.acct_filter_subtype.is_empty() {
            self.acct_filter_subtype = a.account_subtype.clone();
        }
        if !self.acct_filter_owner.is_empty() {
            self.acct_filter_owner = a.owner.clone();
        }
        if !self.acct_filter_title.is_empty() {
            self.acct_filter_title = a.title.clone();
        }
        // Also relax the NON-facet constraints, or the just-saved record can still
        // vanish: clear the review-only filter if the saved record isn't flagged, and
        // clear the username search if it no longer matches the saved username.
        if self.acct_filter_review && !a.review {
            self.acct_filter_review = false;
        }
        if !self.acct_search_user.is_empty()
            && !records::matches_search_soundlike(&a.username, &self.acct_search_user)
        {
            self.acct_search_user.clear();
        }
    }

    /// One-off maintenance: left/right-trim every field on every record across ALL
    /// tabs, persist, and report the count. Each change is recorded in that record's
    /// history. Returns the number of records changed.
    fn trim_all_records(&mut self) -> usize {
        let n = match self.vault.as_mut() {
            Some(ov) => records::trim_all_records(&mut ov.vault),
            None => return 0,
        };
        if n == 0 {
            self.status = "Nothing to trim — every field is already clean.".into();
        } else if self.persist() {
            self.status = format!("Trimmed {n} record(s).");
        }
        n
    }

    fn tab_accounts(&mut self, ui: &mut egui::Ui) {
        // Configured account types for the EDIT form's type dropdown (offers every
        // configured type, not just the ones currently in use).
        let type_names = self.vault_ref().categories().account_type_names();
        // Cross-filtered (faceted) options: each dropdown offers only values present
        // on accounts matching ALL the OTHER active filters. Recompute to a fixpoint,
        // auto-clearing any selection that is no longer one of its narrowed options
        // (so a stale pick never leaves the list silently empty).
        let facets = loop {
            let f = records::account_facets(
                &self.vault_ref().vault.accounts,
                &self.acct_filter_type,
                &self.acct_filter_subtype,
                &self.acct_filter_owner,
                &self.acct_filter_title,
                &self.acct_search_user,
                self.acct_filter_review,
            );
            let mut changed = false;
            if !self.acct_filter_type.is_empty() && !f.types.contains(&self.acct_filter_type) {
                self.acct_filter_type.clear();
                changed = true;
            }
            if !self.acct_filter_subtype.is_empty() && !f.subtypes.contains(&self.acct_filter_subtype) {
                self.acct_filter_subtype.clear();
                changed = true;
            }
            if !self.acct_filter_owner.is_empty() && !f.owners.contains(&self.acct_filter_owner) {
                self.acct_filter_owner.clear();
                changed = true;
            }
            if !self.acct_filter_title.is_empty() && !f.titles.contains(&self.acct_filter_title) {
                self.acct_filter_title.clear();
                changed = true;
            }
            if !changed {
                break f;
            }
        };

        // Set inside the filter row's closure when the one-off trim button is clicked;
        // handled just after so the bulk vault mutation isn't tangled in the UI borrow.
        let mut trim_all = false;
        // The filter row is a card with two labelled lines — the narrowing controls on
        // one, the view toggles on the other. Previously all eleven controls ran
        // together on a single wrapped line, where "reveal all" (which exposes every
        // password on screen) sat between two dropdowns and read like one of them.
        let accent_c = accent(self.theme);
        let filters_active = !self.acct_filter_type.is_empty()
            || !self.acct_filter_subtype.is_empty()
            || !self.acct_filter_owner.is_empty()
            || !self.acct_filter_title.is_empty()
            || self.acct_filter_review
            || !self.acct_search_user.is_empty();
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Filter").strong().small().color(accent_c));
                ui.label(egui::RichText::new("type").weak().small());
                filter_combo(ui, "acct_ftype", &mut self.acct_filter_type, &facets.types);
                ui.label(egui::RichText::new("subtype").weak().small());
                filter_combo(ui, "acct_fsub", &mut self.acct_filter_subtype, &facets.subtypes);
                ui.label(egui::RichText::new("owner").weak().small());
                filter_combo(ui, "acct_fowner", &mut self.acct_filter_owner, &facets.owners);
                ui.label(egui::RichText::new("title").weak().small());
                filter_combo(ui, "acct_ftitle", &mut self.acct_filter_title, &facets.titles);
                search_box(
                    ui,
                    &mut self.acct_search_user,
                    "username or title…",
                    // No arrow glyphs in DRAWN text: the font-coverage test exempts them as
                    // comment-only, so one here could ship as a tofu box (see
                    // `every_glyph_in_the_gui_source_exists_in_the_bundled_fonts`).
                    "Free-text search over the username and the title. The letters may appear \
                     ANYWHERE in the value, and spelling is forgiving: a name that SOUNDS like \
                     the record still matches, so \"jonson\" finds Johnson and \"catherine\" \
                     finds Katherine.",
                    accent_c,
                    180.0,
                );
                ui.checkbox(&mut self.acct_filter_review, "review only");
                // Only offer Clear when there is something to clear, and mark it when
                // filters are hiding rows — an unexplained short list is the single
                // most common "where did my records go" confusion.
                if ui.button("× Clear").on_hover_text("Reset every filter and the search box").clicked() {
                    self.acct_filter_type.clear();
                    self.acct_filter_subtype.clear();
                    self.acct_filter_owner.clear();
                    self.acct_filter_title.clear();
                    self.acct_filter_review = false;
                    self.acct_search_user.clear();
                }
                // A badge when filters are actually hiding rows — an unexplained short
                // list is the most common "where did my records go" confusion.
                if filters_active {
                    badge(ui, "filtered", egui::Color32::from_rgb(190, 105, 10));
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                // Flat filtered list ⇄ grouped tree (type → subtype → owner → title).
                ui.checkbox(&mut self.acct_grouped, "grouped tree")
                    .on_hover_text("Group the list by owner > type > subtype > title");
                // Global reveal: the ONLY reveal control on this screen.
                ui.checkbox(&mut self.reveal_all, "👁 reveal all passwords")
                    .on_hover_text("Unmask every password on this screen. Resets when you switch tabs.");
                // One-off maintenance: left/right-trim every field on every record (all tabs).
                if self.writable
                    && ui
                        .button("Trim all fields")
                        .on_hover_text("One-off: left/right-trim every field on every record in the whole vault (recorded in history)")
                        .clicked()
                {
                    trim_all = true;
                }
            });
        });
        ui.add_space(6.0);

        // Perform the one-off bulk trim (after the filter row, before the list is
        // built, so the cleaned values show this frame).
        if trim_all {
            self.trim_all_records();
        }

        // Filtered list (after the filter row, so a change applies this frame).
        let labels = self.filtered_account_labels();
        // In grouped mode, the same filtered accounts as a type→subtype→owner→title
        // tree (built here so the render closure doesn't re-borrow `self`).
        let tree = if self.acct_grouped {
            Some(records::account_tree(self.vault_ref().vault.accounts.iter().filter(|a| self.account_passes_filters(a))))
        } else {
            None
        };
        let cur = self.edit_account.as_ref().map(|r| r.id.clone());
        // Flat-list arrow navigation: when not grouped, ↑/↓ move to the prev/next item.
        let nav_target = list_nav_target(ui, !self.acct_grouped, &labels, cur.as_deref());
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut generate = false;
        // Deferred password-copy: `None` unless the user clicks copy, in which
        // case it holds the secret in a self-wiping `Zeroizing<String>`.
        let mut copy_pw: Option<Zeroizing<String>> = None;
        // Deferred plain-copy for the non-secret URL / username buttons (acted on after
        // rendering, like `copy_pw`, so the clipboard call sits outside the `self` borrow
        // held by the form). A plain `String` — these are not secrets, so no zeroizing.
        let mut copy_plain: Option<String> = None;
        // Subtypes for the record under edit, looked up from the vault's category lists
        // before the mutable borrow of `edit_account` below. The record's current subtype is
        // kept selectable even when off-list — `combo` prepends the current value, so no
        // manual prepend is needed here. `.unwrap_or_default()` yields an empty `Vec` when no
        // record is being edited.
        let subtypes: Vec<String> = self
            .edit_account
            .as_ref()
            .map(|r| self.vault_ref().categories().subtypes_for(&r.account_type))
            .unwrap_or_default();
        // "Linked from": every Asset/Liability whose `linked_accounts` references the
        // record under edit, resolved before the mutable `edit_account` borrow below
        // (same borrow discipline as `subtypes`). Informational only — links are edited
        // on the ASSET side; here each row just offers Open (navigation is a read).
        let linked_from: Vec<(String, String)> = self
            .edit_account
            .as_ref()
            .map(|r| records::assets_linking_account(&self.vault_ref().vault.assets, &r.id))
            .unwrap_or_default();
        // Deferred jump to a linking asset (its id), applied after the columns closure.
        let mut open_asset: Option<String> = None;
        // Deferred resolution of an armed linked-account delete warning (see the
        // `pending_account_delete` field): confirm proceeds with the delete, cancel disarms.
        let mut confirm_delete = false;
        let mut cancel_delete = false;

        two_col(ui, |c| {
            match &tree {
                // Grouped tree: owner → type → subtype → title (leaf), with empty
                // levels skipped. egui's CollapsingHeader gives the +/- expand control.
                Some(root) => {
                    let lp = &mut c[0];
                    // Same header as the flat `list_panel`, so switching to the tree
                    // does not change what the top of the pane looks like.
                    lp.horizontal_wrapped(|ui| {
                        let accent = ui_accent(ui);
                        section_heading(ui, "Accounts", accent);
                        badge(ui, &format!("{}", labels.len()), accent);
                        ui.add_space(4.0);
                        if self.writable && ui.button("➕ New").clicked() {
                            new = true;
                        }
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
                    lp.add_space(4.0);
                    lp.separator();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("acct_tree").show(lp, |ui| {
                        let mut path: Vec<String> = Vec::new();
                        if let Some(s) = render_acct_node(ui, root, &mut path, cur.as_deref(), &labels, "acct") {
                            select = Some(s);
                        }
                    });
                }
                None => {
                    (new, select, export) =
                        list_panel(&mut c[0], "Accounts", "➕ New", &labels, cur.as_deref(), self.writable, nav_target);
                }
            }
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_accounts").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_account.as_mut() {
                    let w = self.writable;
                    egui::Grid::new("acct_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Title", &mut r.title, w);
                        ui.label("Account type");
                        let prev_type = r.account_type.clone();
                        combo(ui, "acct_type", &mut r.account_type, &type_names, w);
                        if r.account_type != prev_type {
                            // Subtypes are type-specific; drop a now-mismatched subtype.
                            r.account_subtype.clear();
                        }
                        ui.end_row();
                        ui.label("Subtype");
                        combo(ui, "acct_subtype", &mut r.account_subtype, &subtypes, w);
                        ui.end_row();
                        ui.label("Owner");
                        field_singleline(ui, &mut r.owner, w, 420.0);
                        ui.end_row();
                        ui.label("Username");
                        ui.horizontal(|ui| {
                            field_singleline_with_buttons(ui, &mut r.username, w, 380.0, 1);
                            // Copy is a read, so it stays available even in read-only mode;
                            // disabled only when the field is empty (nothing to copy).
                            if ui.add_enabled(!r.username.is_empty(), egui::Button::new("📋")).on_hover_text("Copy").clicked() {
                                copy_plain = Some(r.username.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("Password");
                        ui.horizontal(|ui| {
                            // Masked unless the single global "reveal all" toggle is on (there
                            // is no per-record reveal). `secret_text_edit` (audit R-7) scrubs
                            // egui's undo buffer and re-routes the built-in copy through the
                            // history-excluded clipboard path. Read-only: the field is shown,
                            // selectable, and copyable, but not editable.
                            secret_text_edit(ui, "acct_pw", &mut r.password, self.reveal_all, w, fit_with_buttons(ui, 280.0, 2), &mut copy_pw);
                            // Generate is only useful when you can save; copy is a read.
                            if w && ui.button("🎲").on_hover_text("Generate").clicked() {
                                generate = true;
                            }
                            if ui.button("📋").on_hover_text("Copy").clicked() {
                                // Stash a self-wiping copy to act on after rendering.
                                copy_pw = Some(Zeroizing::new(r.password.clone()));
                            }
                        });
                        ui.end_row();
                        ui.label("URL");
                        ui.horizontal(|ui| {
                            field_singleline(ui, &mut r.url, w, 380.0);
                            if ui.add_enabled(!r.url.is_empty(), egui::Button::new("📋")).on_hover_text("Copy").clicked() {
                                copy_plain = Some(r.url.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("Closed as of");
                        field_singleline_hint(ui, &mut r.closed_as_of, w, 420.0, "YYYY-MM-DD");
                        ui.end_row();
                        ui.label("Review");
                        ui.add_enabled(w, egui::Checkbox::new(&mut r.review, "flag for review"));
                        ui.end_row();
                    });
                    ui.label("Description");
                    field_multiline(ui, &mut r.description, self.writable, 4);
                    // Read-only reverse view of the asset-side links (hidden when nothing
                    // links here). Open stays available read-only — navigation is a read.
                    if !linked_from.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Linked from").strong());
                        for (id, label) in &linked_from {
                            ui.horizontal(|ui| {
                                ui.label(format!("• {label}"));
                                if ui.button("Open").clicked() {
                                    open_asset = Some(id.clone());
                                }
                            });
                        }
                    }
                    action = form_buttons(ui, self.writable);
                    // Armed by the Delete handling below when assets link this account: the
                    // warning (count + consequence) with an explicit second-click pair. The
                    // id guard keeps a warning armed for one record from ever rendering —
                    // or confirming — against another.
                    if self.pending_account_delete.as_deref() == Some(r.id.as_str())
                        && let Some(msg) = account_delete_link_warning(linked_from.len())
                    {
                        ui.colored_label(egui::Color32::from_rgb(0xC0, 0x30, 0x30), msg);
                        ui.horizontal(|ui| {
                            if ui.button("Delete anyway").clicked() {
                                confirm_delete = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel_delete = true;
                            }
                        });
                    }
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "an account");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_account = self.new_account_seeded();
            // Loading a different record disarms any pending delete warning: the armed
            // id no longer matches, so leaving it set could only go stale.
            self.pending_account_delete = None;
        }
        // A pointer click on a list row (`select`) wins over keyboard nav. But a keyboard
        // arrow (`nav_target`) CAN land in the same egui frame as a button click (events
        // are batched per repaint), and the deferred actions below (Delete, confirm/cancel,
        // Generate) were captured against the CURRENTLY shown record. Applying a same-frame
        // nav swap first would retarget them at the NEIGHBOR — e.g. deleting or regenerating
        // the wrong account. So suppress the keyboard nav whenever a record-targeted action
        // is pending this frame; a click (`select`) cannot co-occur with another button
        // click (one pointer), so it is always safe to honor.
        let record_action_pending =
            new || !matches!(action, FormAction::None) || confirm_delete || cancel_delete || generate;
        if !record_action_pending {
            select = select.or(nav_target);
        }
        if let Some(i) = select {
            // `labels` is the FILTERED list, so resolve the clicked row to its id
            // and look the account up by id (a positional index into the
            // unfiltered vector would select the wrong record when filtering).
            if let Some((id, _)) = labels.get(i) {
                self.edit_account =
                    self.vault_ref().vault.accounts.iter().find(|a| &a.id == id).cloned();
                self.pending_account_delete = None; // selection change disarms (see `new` above)
            }
        }
        // Pre-size the password buffer so typing in the egui field doesn't reallocate
        // and strand un-zeroized fragments of the account secret in freed heap. The
        // Account record is ZeroizeOnDrop, but that only wipes the final buffer, not
        // the copies abandoned by per-keystroke growth. `presize_secret` is a no-op once
        // the capacity is sufficient, so this is cheap to call each frame.
        if let Some(r) = self.edit_account.as_mut() {
            presize_secret(&mut r.password);
        }
        if generate
            && let Some(r) = self.edit_account.as_mut()
        {
            // Wipe the previous candidate's bytes before dropping it: a plain
            // `String` reassignment frees the old buffer WITHOUT zeroizing, leaving a
            // prior password in freed heap. `.unwrap_or_default()` yields the new
            // password on success or an empty string on the (unexpected) error case.
            r.password.zeroize();
            r.password = password::generate(&GenOptions::default()).unwrap_or_default();
            // Reveal is global-only now: turn on "reveal all" so the just-generated
            // password is visible (the per-record reveal that used to do this is gone).
            self.reveal_all = true;
        }
        if let Some(pw) = copy_pw {
            // `pw` is moved into the call and wiped when it drops there.
            self.copy_to_clipboard(pw);
        }
        if let Some(text) = copy_plain {
            self.copy_plain(&text);
        }
        match action {
            FormAction::Save => {
                // Left/right-trim every field before persisting. Trim the live edit
                // form too, so the displayed values match what was saved.
                if let Some(r) = self.edit_account.as_mut() {
                    r.trim_fields();
                }
                // Title and owner are mandatory: refuse to save an account missing
                // either (after trimming), keeping the edit form open to fill it.
                if let Some(msg) = self.edit_account.as_ref().and_then(account_required_field_error) {
                    self.status = msg.into();
                } else {
                    if let Some(r) = self.edit_account.clone()
                        && let Some(ov) = self.vault.as_mut()
                    {
                        records::upsert(&mut ov.vault.accounts, r.clone());
                        // Keep the just-saved entry visible: move any ACTIVE filter to the
                        // saved record's value (so changing a filtered field doesn't make
                        // the entry vanish from the filtered list).
                        self.sync_account_filters_to(&r);
                    }
                    if self.persist() {
                        self.status = "Saved.".into();
                        self.sync_edit_buffer(Tab::Accounts);
                    }
                    // On failure persist() has already set the "Save failed: …" status.
                }
            }
            FormAction::Delete => {
                // Deleting a linked-from account is allowed but never silent: instead of
                // deleting, ARM the confirmation (rendered next frame — the warning text
                // + "Delete anyway"/"Cancel" above). The links are NOT cascaded, so the
                // existing delete rollback stays correct — nothing else is touched. An
                // unlinked account deletes immediately, exactly as before.
                if linked_from.is_empty() {
                    self.delete_current(Tab::Accounts);
                } else {
                    self.pending_account_delete = self.edit_account.as_ref().map(|r| r.id.clone());
                }
            }
            _ => {}
        }
        if confirm_delete {
            self.confirm_pending_account_delete();
        }
        if cancel_delete {
            self.pending_account_delete = None;
            self.status = "Delete cancelled.".into();
        }
        if let Some(id) = open_asset {
            self.open_linking_asset(&id);
        }
    }

    // --- Tab: Real Estate ----------------------------------------------------

    fn tab_realestate(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.real_estate);
        let cur = self.edit_realestate.as_ref().map(|r| r.id.clone());
        // Pre-compute attached document labels (needs an immutable vault borrow).
        let doc_labels: Vec<String> = match self.edit_realestate.as_ref() {
            Some(r) => r
                .documents
                .iter()
                .map(|id| self.vault_ref().doc_path(id).unwrap_or_else(|| id.clone()))
                .collect(),
            None => Vec::new(),
        };
        // The single global "reveal all" toggle for this screen (mirrors Accounts): when
        // on, all four portal passwords are shown. There is no per-record reveal.
        let accent_c = accent(self.theme);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("View").strong().small().color(accent_c));
                ui.checkbox(&mut self.re_reveal_all, "👁 reveal all portal passwords").on_hover_text(
                    "Unmask the four portal passwords on this screen. Resets when you switch tabs.",
                );
            });
        });
        ui.add_space(6.0);
        let reveal = self.re_reveal_all;
        let writable = self.writable;
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut copy_pw: Option<Zeroizing<String>> = None;
        let mut docreq = ReDocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Real Estate", "➕ New", &labels, cur.as_deref(), writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_realestate").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_realestate.as_mut() {
                    // No inner ScrollArea here: the whole tab is already wrapped in the
                    // CentralPanel's both-axis scroll. A nested vertical scroll over this
                    // form would capture the wheel and (having no overflow of its own)
                    // scroll nothing, while the outer area never saw the event.
                    egui::Grid::new("re_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Address", &mut r.address, writable);
                        text_row(ui, "Owner", &mut r.owner, writable);
                        text_row(ui, "Taxes", &mut r.taxes, writable);
                        text_row(ui, "HOA dues / info", &mut r.hoa, writable);
                        text_row(ui, "Income account", &mut r.income_account, writable);
                        text_row(ui, "Financing account", &mut r.financing_account, writable);
                        text_row(ui, "Financing balance", &mut r.financing_balance, writable);
                        text_row(ui, "Payment account", &mut r.payment_account, writable);
                    });

                    portal_section(ui, "Property Management portal", &mut r.property_mgmt_url, &mut r.property_mgmt_username, &mut r.property_mgmt_password, &mut r.property_mgmt_comment, reveal, writable, &mut copy_pw);
                    portal_section(ui, "Insurance portal", &mut r.insurance_url, &mut r.insurance_username, &mut r.insurance_password, &mut r.insurance_comment, reveal, writable, &mut copy_pw);
                    portal_section(ui, "HOA portal", &mut r.hoa_url, &mut r.hoa_username, &mut r.hoa_password, &mut r.hoa_comment, reveal, writable, &mut copy_pw);
                    portal_section(ui, "Tax portal", &mut r.tax_portal_url, &mut r.tax_portal_username, &mut r.tax_portal_password, &mut r.tax_portal_comment, reveal, writable, &mut copy_pw);

                    ui.separator();
                    ui.label("Comments");
                    field_multiline(ui, &mut r.comments, writable, 3);

                    ui.separator();
                    ui.label(format!(
                        "Documents ({}) — under <owner>/{}[/subfolder]/<ts>_<file>",
                        r.documents.len(),
                        records::real_estate_doc_location(&r.address)
                    ));
                    // Same uniform widget as Trust & Will (multi-document: the list
                    // holds every attached doc); map its request to ReDocReq.
                    docreq = match doc_section(
                        ui,
                        &doc_labels,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        writable,
                    ) {
                        DocSectionReq::Upload => ReDocReq::Upload,
                        DocSectionReq::Export(i) => ReDocReq::Export(i),
                        DocSectionReq::Remove(i) => ReDocReq::Remove(i),
                        DocSectionReq::None => ReDocReq::None,
                    };

                    action = form_buttons(ui, writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a property");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_realestate = RealEstate::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_realestate = self.vault_ref().vault.real_estate.get(i).cloned();
            self.clear_doc_inputs();
        }
        // Pre-size the portal password buffers so per-keystroke typing never grows
        // (and so reallocates) them — a reallocation frees the old buffer WITHOUT
        // zeroizing, stranding cleartext fragments of a portal password in freed
        // heap. RealEstate is ZeroizeOnDrop, but that only wipes the final buffer,
        // not abandoned reallocations. Same mitigation as the Accounts password field.
        if let Some(r) = self.edit_realestate.as_mut() {
            presize_secret(&mut r.property_mgmt_password);
            presize_secret(&mut r.insurance_password);
            presize_secret(&mut r.hoa_password);
            presize_secret(&mut r.tax_portal_password);
        }
        if let Some(pw) = copy_pw {
            self.copy_to_clipboard(pw);
        }
        self.handle_re_doc(docreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_realestate.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::RealEstate);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::RealEstate),
            _ => {}
        }
    }

    // --- Tab: Taxes ----------------------------------------------------------

    fn tab_taxes(&mut self, ui: &mut egui::Ui) {
        let labels = label_list(&self.vault_ref().vault.tax_filings);
        let cur = self.edit_taxfiling.as_ref().map(|r| r.id.clone());
        // Pre-compute each attached document's "location/filename" label (needs an
        // immutable borrow of the vault, so it can't happen inside the edit form).
        let doc_labels: Vec<String> = match self.edit_taxfiling.as_ref() {
            Some(r) => r
                .documents
                .iter()
                .map(|id| self.vault_ref().doc_path(id).unwrap_or_else(|| id.clone()))
                .collect(),
            None => Vec::new(),
        };
        let writable = self.writable;
        let mut new = false;
        let mut select = None;
        let mut export = false;
        let mut action = FormAction::None;
        let mut docreq = TaxDocReq::None;

        two_col(ui, |c| {
            (new, select, export) = list_panel(&mut c[0], "Taxes", "➕ New", &labels, cur.as_deref(), writable, None);
            // The form pane scrolls on its OWN axis. Previously the whole tab sat inside
            // one both-axis ScrollArea, so this vertical scroller was nested inside
            // another one and was handed unbounded height — the layout could not settle
            // on a scrollbar, which is what flickered on a small window.
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("form_pane_taxes").show(&mut c[1], |ui| {
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    egui::Grid::new("tax_form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        text_row(ui, "Owner", &mut r.owner, writable);
                        text_row(ui, "Filing year", &mut r.year, writable);
                    });
                    ui.label("Notes");
                    field_multiline(ui, &mut r.notes, writable, 4);
                    ui.separator();

                    // Attached documents — all live under <owner>/taxes/<year>/…/<ts>_<file>
                    ui.label(format!(
                        "Documents ({}) — under <owner>/{}[/subfolder]/<ts>_<file>",
                        r.documents.len(),
                        records::tax_doc_location(&r.year)
                    ));
                    // Same uniform widget as Trust & Will; map its request to TaxDocReq.
                    docreq = match doc_section(
                        ui,
                        &doc_labels,
                        &mut self.doc_subfolder,
                        &mut self.doc_filename,
                        &mut self.doc_source,
                        writable,
                    ) {
                        DocSectionReq::Upload => TaxDocReq::Upload,
                        DocSectionReq::Export(i) => TaxDocReq::Export(i),
                        DocSectionReq::Remove(i) => TaxDocReq::Remove(i),
                        DocSectionReq::None => TaxDocReq::None,
                    };

                    action = form_buttons(ui, writable);
                    history_view(ui, &r.history);
                } else {
                    empty_form_hint(ui, "a tax year");
                }
            });
        });

        if export {
            self.export_current_tab_csv();
        }
        if new {
            self.edit_taxfiling = TaxFiling::new().ok();
            self.clear_doc_inputs();
        }
        if let Some(i) = select {
            self.edit_taxfiling = self.vault_ref().vault.tax_filings.get(i).cloned();
            self.clear_doc_inputs();
        }
        self.handle_tax_doc(docreq);
        match action {
            FormAction::Save => {
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    r.trim_fields();
                }
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                if self.persist() {
                    self.status = "Saved.".into();
                    self.sync_edit_buffer(Tab::Taxes);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            FormAction::Delete => self.delete_current(Tab::Taxes),
            _ => {}
        }
    }

    // Performs a Real-Estate document action (upload to real-estate/<address>/,
    // export, or remove), mirroring handle_doc's persist-then-reclaim ordering.
    fn handle_re_doc(&mut self, req: ReDocReq) {
        match req {
            ReDocReq::None => {}
            ReDocReq::Upload => {
                // Accept a path pasted with surrounding double quotes ("Copy as path").
                let src = records::unquote_path(&self.doc_source).to_string();
                if src.is_empty() {
                    self.status = "'Upload from' path is required.".into();
                    return;
                }
                // If no filename is given, default to the source file's own name.
                let name = records::effective_doc_filename(&self.doc_filename, &src);
                if name.trim().is_empty() {
                    self.status = "Filename is required (the source path has no file name).".into();
                    return;
                }
                let address = self.edit_realestate.as_ref().map(|r| r.address.clone()).unwrap_or_default();
                let prefix = records::owner_prefix(
                    self.edit_realestate.as_ref().map(|r| r.owner.as_str()),
                    &records::real_estate_doc_location(&address),
                );
                let ts = records::compact_utc(records::unix_now());
                let name = records::timestamped_filename(&ts, &records::doc_filename(&name));
                let loc = records::doc_upload_dir(&prefix, &self.doc_subfolder);
                let vpath = vault::virtual_path(&loc, &name);
                if vpath.len() > crate::storage::MAX_PATH_LEN {
                    self.status = format!(
                        "Path too long: {} bytes (max {}). Shorten the filename.",
                        vpath.len(),
                        crate::storage::MAX_PATH_LEN
                    );
                    return;
                }
                let id = match self.vault.as_mut() {
                    Some(ov) => match ov.add_document(&loc, &name, Path::new(&src)) {
                        Ok(id) => id,
                        Err(e) => {
                            self.fail(format!("Upload failed: {e}"));
                            return;
                        }
                    },
                    None => return,
                };
                if let Some(r) = self.edit_realestate.as_mut() {
                    r.documents.push(id);
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                self.clear_doc_inputs();
                if self.persist() {
                    self.status = "Document uploaded to the encrypted volume.".into();
                    self.sync_edit_buffer(Tab::RealEstate);
                }
            }
            ReDocReq::Export(i) => {
                if let Some(id) = self.edit_realestate.as_ref().and_then(|r| r.documents.get(i).cloned()) {
                    self.export_doc_to_config_dir(&id);
                }
            }
            ReDocReq::Remove(i) => {
                let id = self.edit_realestate.as_ref().and_then(|r| r.documents.get(i).cloned());
                if let Some(r) = self.edit_realestate.as_mut()
                    && i < r.documents.len()
                {
                    r.documents.remove(i);
                }
                if let Some(r) = self.edit_realestate.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.real_estate, r);
                }
                if !self.persist() {
                    return;
                }
                self.sync_edit_buffer(Tab::RealEstate);
                if let Some(id) = id
                    && let Some(ov) = self.vault.as_mut()
                    && let Err(e) = ov.remove_document(&id)
                {
                    self.fail(format!("Unlinked, but blob cleanup failed: {e}"));
                    return;
                }
                self.status = "Removed document from the vault.".into();
            }
        }
    }

    // --- Shared deferred operations -----------------------------------------

    /// Human-readable "location/filename" of an attached volume file id.
    fn attached_label(&self, file_id: Option<String>) -> Option<String> {
        // `file_id?` is the `?` operator on an Option: if `None`, return `None`
        // from this function immediately; otherwise unwrap to `id` and continue.
        let id = file_id?;
        self.vault_ref().doc_path(&id)
    }

    /// Upsert the current edit buffer for a document-bearing tab into the vault,
    /// so a document link is persisted together with its manifest entry.
    fn upsert_doc_target(&mut self, target: DocTarget) {
        match target {
            DocTarget::TrustWill => {
                if let Some(r) = self.edit_trustwill.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.trust_wills, r);
                }
            }
            DocTarget::Asset => {
                if let Some(r) = self.edit_asset.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.assets, r);
                }
            }
            DocTarget::General => {
                if let Some(r) = self.edit_general.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.general_documents, r);
                }
            }
        }
    }

    // Performs the document attach/export/detach requested during rendering.
    // Split out so the vault is borrowed mutably *here*, not while drawing.
    fn handle_doc(&mut self, req: DocReq, target: DocTarget) {
        match req {
            DocReq::None => {}
            DocReq::Attach => {
                // Accept a path pasted with surrounding double quotes ("Copy as path").
                let src = records::unquote_path(&self.doc_source).to_string();
                if src.is_empty() {
                    self.status = "'Upload from' path is required.".into();
                    return;
                }
                // If no filename is given, default to the source file's own name.
                let name = records::effective_doc_filename(&self.doc_filename, &src);
                if name.trim().is_empty() {
                    self.status = "Filename is required (the source path has no file name).".into();
                    return;
                }
                // Don't upload+persist an INVALID asset that the Save path rejects (empty owner
                // or non-numeric value → the Summary silently treats it as 0). Validate first,
                // mirroring the Save path's records::asset_validation_error gate.
                if let DocTarget::Asset = target
                    && let Some(r) = self.edit_asset.as_ref()
                    && let Some(msg) = records::asset_validation_error(r)
                {
                    self.fail(msg);
                    return;
                }
                // Owner-first prefix: Assets nest under the owner initials + kind root
                // (/<INITIALS>/assets|liabilities); Trust&Will/General have no owner and keep
                // their slugged group. The timestamp is folded into the filename, so the
                // directory is <prefix>[/<subfolder>].
                let prefix = match target {
                    DocTarget::TrustWill => records::trust_will_doc_location(
                        self.edit_trustwill.as_ref().map(|r| r.document.as_str()).unwrap_or(""),
                    ),
                    DocTarget::Asset => records::owner_prefix(
                        self.edit_asset.as_ref().map(|r| r.owner.as_str()),
                        &records::asset_doc_location(self.edit_asset.as_ref().map(|r| r.kind.as_str()).unwrap_or("")),
                    ),
                    DocTarget::General => records::general_doc_location(
                        self.edit_general.as_ref().map(|r| r.title.as_str()).unwrap_or(""),
                    ),
                };
                let ts = records::compact_utc(records::unix_now());
                let fname = records::timestamped_filename(&ts, &records::doc_filename(&name));
                let loc = records::doc_upload_dir(&prefix, &self.doc_subfolder);
                let vpath = vault::virtual_path(&loc, &fname);
                if vpath.len() > crate::storage::MAX_PATH_LEN {
                    self.status = format!(
                        "Path too long: {} bytes (max {}). Shorten the filename or subfolder.",
                        vpath.len(),
                        crate::storage::MAX_PATH_LEN
                    );
                    return;
                }
                // Nested match: get the vault (mut), then attempt the upload. Each
                // branch either yields the new document `id` or returns early.
                let id = match self.vault.as_mut() {
                    Some(ov) => match ov.add_document(&loc, &fname, Path::new(&src)) {
                        Ok(id) => id,
                        Err(e) => {
                            self.fail(format!("Upload failed: {e}"));
                            return;
                        }
                    },
                    None => return,
                };
                // Capture any document this record already had, so re-attaching
                // reclaims the replaced blob instead of orphaning it (matches TUI).
                let previous = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                match target {
                    DocTarget::TrustWill => {
                        if let Some(r) = self.edit_trustwill.as_mut() {
                            r.file = Some(id);
                        }
                    }
                    DocTarget::Asset => {
                        if let Some(r) = self.edit_asset.as_mut() {
                            r.statement = Some(id);
                        }
                    }
                    DocTarget::General => {
                        if let Some(r) = self.edit_general.as_mut() {
                            r.file = Some(id);
                        }
                    }
                }
                // Persist the record→document link immediately so the manifest
                // entry is referenced (no orphan if the user navigates away).
                self.upsert_doc_target(target);
                self.clear_doc_inputs();
                if self.persist() {
                    // Only reclaim the replaced blob once the new link actually reached
                    // disk. If the save failed, vault.pmv still references `old`, so
                    // dropping it would create a dangling reference (ArchiveMismatch).
                    if let Some(old) = previous
                        && let Some(ov) = self.vault.as_mut()
                    {
                        // `let _ = ...` deliberately discards the `Result`: a failure
                        // here only orphans a blob (harmless), so it is not reported.
                        let _ = ov.remove_document(&old);
                    }
                    self.status = "Document uploaded to the encrypted volume.".into();
                    // The upsert above wrote the whole record, not just the link, so the
                    // form must be re-read from the vault like any other save.
                    self.sync_edit_buffer(target.tab());
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            DocReq::Export => {
                let file_id = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                if let Some(id) = file_id {
                    self.export_doc_to_config_dir(&id);
                }
            }
            DocReq::Remove => {
                // Unlink from the record AND reclaim the encrypted blob, so a
                // "removed" document does not linger in the archive.
                let id = match target {
                    DocTarget::TrustWill => self.edit_trustwill.as_ref().and_then(|r| r.file.clone()),
                    DocTarget::Asset => self.edit_asset.as_ref().and_then(|r| r.statement.clone()),
                    DocTarget::General => self.edit_general.as_ref().and_then(|r| r.file.clone()),
                };
                match target {
                    DocTarget::TrustWill => {
                        if let Some(r) = self.edit_trustwill.as_mut() {
                            r.file = None;
                        }
                    }
                    DocTarget::Asset => {
                        if let Some(r) = self.edit_asset.as_mut() {
                            r.statement = None;
                        }
                    }
                    DocTarget::General => {
                        if let Some(r) = self.edit_general.as_mut() {
                            r.file = None;
                        }
                    }
                }
                self.upsert_doc_target(target);
                // Persist the unlink BEFORE reclaiming the blob, AND only reclaim if
                // the save succeeded. A crash or a failed save between the two would
                // otherwise leave vault.pmv referencing a doc whose manifest entry is
                // gone (ArchiveMismatch -> unopenable). An orphaned blob is harmless.
                if !self.persist() {
                    return; // persist() already set the "Save failed" status
                }
                // Saved: the form shows the record as stored (see `sync_edit_buffer`).
                self.sync_edit_buffer(target.tab());
                // Three-part let-chain: there is an id, the vault is open, and the
                // blob removal failed — only then report the cleanup error.
                if let Some(id) = id
                    && let Some(ov) = self.vault.as_mut()
                    && let Err(e) = ov.remove_document(&id)
                {
                    self.fail(format!("Unlinked, but blob cleanup failed: {e}"));
                    return;
                }
                self.status = "Removed document from the vault.".into();
            }
        }
    }

    // Performs a Taxes-tab document action (upload to taxes/<year>/, export, or
    // remove). Like `handle_doc`, the vault is borrowed mutably here, not while
    // drawing, and the persist-then-reclaim ordering keeps a crash from leaving a
    // dangling reference.
    fn handle_tax_doc(&mut self, req: TaxDocReq) {
        match req {
            TaxDocReq::None => {}
            TaxDocReq::Upload => {
                // Accept a path pasted with surrounding double quotes ("Copy as path").
                let src = records::unquote_path(&self.doc_source).to_string();
                if src.is_empty() {
                    self.status = "'Upload from' path is required.".into();
                    return;
                }
                // If no filename is given, default to the source file's own name.
                let name = records::effective_doc_filename(&self.doc_filename, &src);
                if name.trim().is_empty() {
                    self.status = "Filename is required (the source path has no file name).".into();
                    return;
                }
                // The folder is derived from the filing year, NOT user-entered.
                let year = self.edit_taxfiling.as_ref().map(|r| r.year.clone()).unwrap_or_default();
                let prefix = records::owner_prefix(
                    self.edit_taxfiling.as_ref().map(|r| r.owner.as_str()),
                    &records::tax_doc_location(&year),
                );
                let ts = records::compact_utc(records::unix_now());
                let name = records::timestamped_filename(&ts, &records::doc_filename(&name));
                let loc = records::doc_upload_dir(&prefix, &self.doc_subfolder);
                let vpath = vault::virtual_path(&loc, &name);
                if vpath.len() > crate::storage::MAX_PATH_LEN {
                    self.status = format!(
                        "Path too long: {} bytes (max {}). Shorten the filename.",
                        vpath.len(),
                        crate::storage::MAX_PATH_LEN
                    );
                    return;
                }
                let id = match self.vault.as_mut() {
                    Some(ov) => match ov.add_document(&loc, &name, Path::new(&src)) {
                        Ok(id) => id,
                        Err(e) => {
                            self.fail(format!("Upload failed: {e}"));
                            return;
                        }
                    },
                    None => return,
                };
                if let Some(r) = self.edit_taxfiling.as_mut() {
                    r.documents.push(id);
                }
                // Persist the record→document link immediately so the manifest entry
                // is referenced (no orphan if the user navigates away).
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                self.clear_doc_inputs();
                if self.persist() {
                    self.status = "Document uploaded to the encrypted volume.".into();
                    self.sync_edit_buffer(Tab::Taxes);
                }
                // On failure persist() has already set the "Save failed: …" status.
            }
            TaxDocReq::Export(i) => {
                if let Some(id) = self.edit_taxfiling.as_ref().and_then(|r| r.documents.get(i).cloned()) {
                    self.export_doc_to_config_dir(&id);
                }
            }
            TaxDocReq::Remove(i) => {
                // Unlink from the record, persist, THEN reclaim the blob — same
                // crash-safe ordering as handle_doc's Remove.
                let id = self.edit_taxfiling.as_ref().and_then(|r| r.documents.get(i).cloned());
                if let Some(r) = self.edit_taxfiling.as_mut()
                    && i < r.documents.len()
                {
                    r.documents.remove(i);
                }
                if let Some(r) = self.edit_taxfiling.clone()
                    && let Some(ov) = self.vault.as_mut()
                {
                    records::upsert(&mut ov.vault.tax_filings, r);
                }
                if !self.persist() {
                    return; // persist() already set the "Save failed" status
                }
                self.sync_edit_buffer(Tab::Taxes);
                if let Some(id) = id
                    && let Some(ov) = self.vault.as_mut()
                    && let Err(e) = ov.remove_document(&id)
                {
                    self.fail(format!("Unlinked, but blob cleanup failed: {e}"));
                    return;
                }
                self.status = "Removed document from the vault.".into();
            }
        }
    }

    /// Apply a deferred linked-accounts request from the Assets form (see [`LinkReq`]).
    /// Add/Unlink edit the WORKING BUFFER only — the link list persists through the
    /// ordinary Save path with the rest of the form (never a direct vault write, so
    /// abandoning the edit discards it like any other unsaved change). Open navigates.
    /// Execute an armed "Delete anyway" confirmation for a linked-from account.
    /// Confirm-time id re-check: the render-time guard alone is NOT enough — a bare
    /// arrow-key nav event can land in the SAME egui frame as the "Delete anyway"
    /// click, and the select/nav handler runs before this, swapping `edit_account`
    /// to the neighboring record (and disarming `pending_account_delete`) after the
    /// click was captured. Without this check the raced confirm would delete the
    /// NEIGHBOR. Requiring the armed id to still match the loaded record drops such
    /// a stale confirm (the swap set pending to None, so the equality fails).
    fn confirm_pending_account_delete(&mut self) {
        let armed_matches_current = self.pending_account_delete.is_some()
            && self.pending_account_delete.as_deref()
                == self.edit_account.as_ref().map(|r| r.id.as_str());
        self.pending_account_delete = None;
        if armed_matches_current {
            self.delete_current(Tab::Accounts);
        }
    }

    fn handle_link_req(&mut self, req: LinkReq) {
        match req {
            LinkReq::None => {}
            LinkReq::Add(id) => {
                // The dropdown only offers not-yet-linked accounts, but the request is
                // re-checked here (deferred handling) so a duplicate can never slip in.
                if let Some(r) = self.edit_asset.as_mut()
                    && !r.linked_accounts.iter().any(|l| l == &id)
                {
                    r.linked_accounts.push(id);
                }
            }
            LinkReq::Remove(i) => {
                // Bounds-checked: the index was captured a frame ago against the same
                // buffer, but a stale/raced index must drop the request, not panic.
                if let Some(r) = self.edit_asset.as_mut()
                    && i < r.linked_accounts.len()
                {
                    r.linked_accounts.remove(i);
                }
            }
            LinkReq::Open(id) => self.open_linked_account(&id),
        }
    }

    /// Jump from an asset's link row to the linked Account: load it in the Accounts
    /// editor and switch tabs. A dangling link (the account was deleted — links are
    /// never cascaded) surfaces a status message and does NOT navigate.
    fn open_linked_account(&mut self, id: &str) {
        let Some(a) = self.vault_ref().vault.accounts.iter().find(|a| a.id == id).cloned() else {
            self.status = "Linked account not found — it may have been deleted.".into();
            return;
        };
        // A programmatic tab change bypasses ui_top_bar's prev_tab compare, so perform
        // the same switch resets here: re-mask to the saved reveal default and clear
        // the shared document-input buffers (see the reset block in `ui_top_bar`).
        self.tab = Tab::Accounts;
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.clear_doc_inputs();
        // Retarget any ACTIVE Accounts filters/search to the jump target so the list
        // can't hide the record we just navigated to (same rule as the after-save follow).
        self.sync_account_filters_to(&a);
        self.edit_account = Some(a);
    }

    /// Jump from an account's "Linked from" row to the Asset/Liability linking it —
    /// the reverse of [`Self::open_linked_account`], with the same programmatic
    /// tab-switch resets. The row list is rebuilt from the vault each frame, but the
    /// id is still re-resolved here (deferred handling) rather than trusted.
    fn open_linking_asset(&mut self, id: &str) {
        let Some(r) = self.vault_ref().vault.assets.iter().find(|r| r.id == id).cloned() else {
            self.status = "Linked record not found — it may have been deleted.".into();
            return;
        };
        self.tab = Tab::Assets;
        self.reveal_all = self.reveal_default;
        self.re_reveal_all = self.reveal_default;
        self.clear_doc_inputs();
        // The Assets list's only hiding filter is the review-only toggle: clear it when
        // it would hide the jump target (mirrors the Accounts-side filter follow).
        if self.asset_filter_review && !r.review {
            self.asset_filter_review = false;
        }
        self.edit_asset = Some(r);
    }

    fn delete_current(&mut self, tab: Tab) {
        // Collect any attached document ids to reclaim after removing the record.
        let mut doc_ids: Vec<String> = Vec::new();
        // Roll back the IN-MEMORY removal if the save fails. Without this, a failed persist
        // would leave the record gone from memory (the user was told it failed) and a LATER
        // successful save would silently serialize the whole vault and commit the deletion —
        // unrecoverable data loss. The closure re-inserts the removed record, truncates the
        // remove() audit entry, and restores the edit buffer. (Mirrors the merge path's care.)
        let mut rollback: Option<Rollback> = None;
        if let Some(ov) = self.vault.as_mut() {
            // `&mut ov.vault` is an exclusive borrow of the in-memory vault data,
            // reused below as `v` to keep the match arms terse.
            let v = &mut ov.vault;
            let audit_len = v.audit.len(); // snapshot to undo the remove() audit entry on rollback
            match tab {
                Tab::Urgent => {
                    if let Some(r) = self.edit_urgent.take() {
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.urgent.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.urgent, &r.id, &mut v.audit, "Urgent") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.urgent.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_urgent = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::Instructions => {
                    // `.take()` moves the edited record out of the Option, leaving
                    // `None` behind (so the form clears after deletion) and giving
                    // us owned `r` to read its id.
                    if let Some(r) = self.edit_instruction.take() {
                        // Snapshot the STORED record (its last-SAVED state) for the vault
                        // rollback — NOT the edit buffer `r`, which may hold unsaved edits a
                        // failed delete must not silently commit on a later save.
                        let stored = v.instructions.iter().find(|x| x.id == r.id).cloned();
                        // Only arm the rollback when a record was ACTUALLY removed. A New-but-
                        // never-saved record isn't in the list (remove is a no-op), so the rollback
                        // must NOT resurrect it on a persist failure — the user is discarding it.
                        if records::remove(&mut v.instructions, &r.id, &mut v.audit, "Instruction") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.instructions.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_instruction = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::TrustWill => {
                    if let Some(r) = self.edit_trustwill.take() {
                        if let Some(f) = &r.file {
                            doc_ids.push(f.clone());
                        }
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.trust_wills.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.trust_wills, &r.id, &mut v.audit, "Trust/Will") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.trust_wills.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_trustwill = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::Assets => {
                    if let Some(r) = self.edit_asset.take() {
                        if let Some(f) = &r.statement {
                            doc_ids.push(f.clone());
                        }
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.assets.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.assets, &r.id, &mut v.audit, "Asset/Liability") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.assets.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_asset = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::Accounts => {
                    if let Some(r) = self.edit_account.take() {
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm. Especially load-bearing here: the account edit
                        // buffer can hold an unsaved password change that a failed delete must
                        // never resurrect-and-commit.
                        let stored = v.accounts.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.accounts, &r.id, &mut v.audit, "Account") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.accounts.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_account = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::RealEstate => {
                    if let Some(r) = self.edit_realestate.take() {
                        // Reclaim every document attached to this property.
                        for f in &r.documents {
                            doc_ids.push(f.clone());
                        }
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.real_estate.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.real_estate, &r.id, &mut v.audit, "Real Estate") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.real_estate.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_realestate = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::Taxes => {
                    if let Some(r) = self.edit_taxfiling.take() {
                        // Reclaim every document attached to this filing year.
                        for f in &r.documents {
                            doc_ids.push(f.clone());
                        }
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.tax_filings.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.tax_filings, &r.id, &mut v.audit, "Tax filing") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.tax_filings.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_taxfiling = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                Tab::GeneralDocuments => {
                    if let Some(r) = self.edit_general.take() {
                        if let Some(f) = &r.file {
                            doc_ids.push(f.clone());
                        }
                        // Restore the SAVED record, not the (possibly dirty) edit buffer — see
                        // the Instructions arm.
                        let stored = v.general_documents.iter().find(|x| x.id == r.id).cloned();
                        if records::remove(&mut v.general_documents, &r.id, &mut v.audit, "General document") {
                            rollback = Some(Box::new(move |s: &mut Self| {
                                if let Some(ov) = s.vault.as_mut() {
                                    ov.vault.audit.truncate(audit_len);
                                    if let Some(stored) = stored {
                                        ov.vault.general_documents.push(stored); // restore the SAVED state verbatim
                                    }
                                }
                                s.edit_general = Some(r); // restore the user's editing session (UI state)
                            }));
                        }
                    }
                }
                // The Summary tab is read-only (no records of its own), so it never deletes.
                Tab::Summary => {}
            }
        }
        // Persist the record removal BEFORE reclaiming its blobs, AND only reclaim
        // if the save succeeded — otherwise the on-disk vault still references the
        // record and dropping its blobs would make it unopenable (ArchiveMismatch).
        if self.persist() {
            for id in doc_ids {
                if let Some(ov) = self.vault.as_mut() {
                    let _ = ov.remove_document(&id);
                }
            }
            self.status = "Deleted.".into();
        } else if let Some(rb) = rollback {
            // persist() already set the "Save failed: …" status; undo the in-memory removal so
            // a later successful save cannot silently commit the deletion the user was told failed.
            rb(self);
        }
    }

    fn copy_to_clipboard(&mut self, text: Zeroizing<String>) {
        // `text` is wiped on drop; the shared helper copies it into the OS clipboard
        // with the Linux history-exclusion hint so clipboard managers don't retain
        // the password (cleared on the 15s timer and on exit either way).
        match crate::copy_secret_to_clipboard(text.as_str()) {
            Ok(()) => {
                self.clipboard_dirty = true;
                self.clipboard_clear_at = Some(Instant::now() + CLIPBOARD_CLEAR_AFTER);
                self.status = "Copied (clipboard auto-clears in 15s, and on exit).".into();
            }
            Err(e) => self.fail(format!("Clipboard unavailable: {e}")),
        }
    }

    /// Copy a NON-secret (a URL or username) to the OS clipboard. Unlike
    /// [`Self::copy_to_clipboard`] this schedules NO 15 s auto-clear and uses the plain
    /// (history-kept) clipboard path. The fresh non-secret has just overwritten whatever
    /// was on the clipboard, so any pending secret auto-clear is cancelled and the dirty
    /// flag cleared: there is no longer a copied password to wipe, and leaving the timer
    /// armed would blank the user's freshly copied URL/username 15 s later.
    fn copy_plain(&mut self, text: &str) {
        match crate::copy_plain_to_clipboard(text) {
            Ok(()) => {
                self.clipboard_dirty = false;
                self.clipboard_clear_at = None;
                self.status = "Copied.".into();
            }
            Err(e) => self.fail(format!("Clipboard unavailable: {e}")),
        }
    }
}

// Identifies which document-bearing tab a deferred doc action applies to.
#[derive(Clone, Copy)]
enum DocTarget {
    TrustWill,
    Asset,
    General,
}

impl DocTarget {
    /// The tab whose form owns this target — the edit buffer [`GuiApp::upsert_doc_target`]
    /// writes into the vault, and so the one to re-read afterwards
    /// ([`GuiApp::sync_edit_buffer`]).
    fn tab(self) -> Tab {
        match self {
            DocTarget::TrustWill => Tab::TrustWill,
            DocTarget::Asset => Tab::Assets,
            DocTarget::General => Tab::GeneralDocuments,
        }
    }
}

// Implement eframe's `App` trait so `GuiApp` can be driven by the framework.
// eframe calls `ui()` on every frame to (re)draw the whole window.
impl eframe::App for GuiApp {
    // The leading `_` in `_frame` marks the parameter as intentionally unused.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl GuiApp {
    /// Draw the whole window. Split out of [`eframe::App::ui`] (which only forwards
    /// to it) because it needs nothing from `eframe::Frame` — so a headless
    /// `egui_kittest` harness can lay out the REAL window, panels and all, rather
    /// than a hand-assembled approximation of it.
    fn render(&mut self, ui: &mut egui::Ui) {
        self.tick_clipboard(ui.ctx());
        // Apply (and persist) the color theme only when the selection changed.
        if self.theme != self.applied_theme {
            // The palette AND the accent-colored parts of the style change together.
            apply_theme(ui.ctx(), self.theme);
            save_theme(&self.vault_root, self.theme);
            self.applied_theme = self.theme;
        }
        // Same pattern for the interface scale (an independent axis from colour).
        if self.ui_scale != self.applied_ui_scale {
            apply_ui_scale(ui.ctx(), self.ui_scale);
            save_ui_scale(&self.vault_root, self.ui_scale);
            self.applied_ui_scale = self.ui_scale;
        }
        // …and the typeface (rebuilds the font atlas, hence only on a real change).
        if self.font != self.applied_font {
            apply_fonts(ui.ctx(), self.font);
            save_font_choice(&self.vault_root, self.font);
            self.applied_font = self.font;
        }
        // The window's minimum size, re-asserted whenever the value it depends on moves.
        // Unlike the three settings above this is not driven by a user choice: it is clamped
        // to the DISPLAY, which is unknown until the first frame has been drawn and changes
        // again when the window is dragged to another monitor. Comparing before sending keeps
        // this to a real change rather than a viewport command every frame.
        let want_min = min_inner_size(monitor_size(ui.ctx()));
        if want_min != self.applied_min_inner {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::MinInnerSize(want_min));
            self.applied_min_inner = want_min;
        }
        // Clear the error banner once any later status message has replaced the failure
        // text it was showing (a success/info line means the problem is no longer current).
        if error_banner_is_stale(self.error.as_deref(), &self.status) {
            self.error = None;
        }
        // A hard failure (a failed save/export/backup/upload, …) gets a bright, dismissable
        // banner across the TOP of EVERY screen — far more visible than the weak status
        // line, so a failure can never be missed (e.g. a save that failed on a full disk,
        // where the status line alone would leave the user believing the edit was saved).
        // Rendered before the per-screen panels so it sits above all of them.
        show_error_banner(&mut self.error, ui);
        if self.screen == Screen::Auth {
            // The lock screen is meant to read as one simple page that does NOT scroll.
            // Two things hold that up: `min_inner_size`, the floor the window cannot go
            // below, and `auth_space_scale`, which spends this screen's decorative padding
            // according to the height actually available — so a window shorter than the
            // comfortable layout gets a tighter front door rather than a scrollbar over
            // the password fields.
            //
            // The ScrollArea below is a SAFETY NET for the case neither can cover: a
            // display so short that even the collapsed layout does not fit (the floor is
            // clamped to the monitor, so on such a screen the window is legitimately
            // smaller than the content). Being wrong there without it does not look
            // untidy — it puts the password fields or the Unlock button permanently out of
            // reach with no way to get to them. It draws no bar whenever the content fits,
            // which after the above is the case on any ordinary display, and `auto_shrink`
            // keeps the layout identical then.
            egui::CentralPanel::default().show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.ui_auth(ui));
            });
            return;
        }
        if self.screen == Screen::Config {
            egui::CentralPanel::default().show_inside(ui, |ui| self.ui_config(ui));
            return;
        }
        if self.screen == Screen::Merge {
            egui::CentralPanel::default().show_inside(ui, |ui| self.ui_merge(ui));
            return;
        }
        if self.screen == Screen::Help {
            egui::CentralPanel::default().show_inside(ui, |ui| self.ui_help(ui));
            return;
        }

        egui::Panel::top("topbar").show_inside(ui, |ui| {
            ui.add_space(4.0);
            self.ui_top_bar(ui);
            ui.add_space(4.0);
        });
        // The status bar is ALWAYS drawn, even when idle. Showing it conditionally
        // made the whole tab jump by a row whenever a message arrived or aged out;
        // a fixed strip keeps the layout still and gives the message a known home.
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.add_space(3.0);
            let accent = accent(self.theme);
            egui::containers::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    if self.status.is_empty() {
                        ui.label(egui::RichText::new("•").color(accent.gamma_multiply(0.5)).small());
                        ui.label(egui::RichText::new("Ready").weak().small());
                    } else {
                        let caveat = is_export_caveat(&self.status);
                        let caveat_color = export_caveat_color(ui.visuals());
                        ui.label(
                            egui::RichText::new("•")
                                .color(if caveat { caveat_color } else { accent })
                                .small(),
                        );
                        // A long message truncates here rather than widening the window;
                        // hover carries the full text. The export caveat is worded to put
                        // its warning FIRST so truncation can only ever eat the path.
                        let text = egui::RichText::new(&self.status).small();
                        let text = if caveat { text.color(caveat_color).strong() } else { text };
                        ui.add(egui::Label::new(text).truncate()).on_hover_text(&self.status);
                    }
                },
                |ui| {
                    // A live, hard-to-miss stand-in for what used to be only a line in the
                    // Help manual ("selecting another record discards unsaved edits"): the
                    // footer is where the eye already looks for state, so an unsaved edit is
                    // shown right where the user is about to click away from it, not just
                    // documented somewhere they may never open.
                    if self.has_unsaved_edits() {
                        ui.label(
                            egui::RichText::new(UNSAVED_WARNING)
                                .small()
                                .strong()
                                .color(egui::Color32::from_rgb(200, 90, 20)),
                        );
                        ui.add_space(10.0);
                    }
                    // The clipboard's auto-clear state belongs where the eye already looks
                    // for state — otherwise a copied password's lifetime is invisible.
                    if self.clipboard_dirty {
                        ui.label(
                            egui::RichText::new("📋 clipboard clears automatically")
                                .small()
                                .color(egui::Color32::from_rgb(190, 105, 10)),
                        );
                    }
                },
            );
            ui.add_space(3.0);
        });
        // The tab body fills the panel and does NOT scroll as a whole. Scrolling belongs
        // to the frames that actually hold overflowing content — each tab's list pane and
        // its form pane scroll independently (and Summary's wide table scrolls both ways
        // on its own).
        //
        // It used to be one both-axis ScrollArea wrapped around everything, with the list
        // and form scrollers nested inside it. A scroll area gives its contents unbounded
        // space on its scrolling axes, so those inner vertical scrollers were laid out
        // against infinite height and never decided they needed a scrollbar; meanwhile the
        // outer horizontal bar appeared, took width away, forced a re-layout, and
        // disappeared again. On a window too small for the content that oscillation ran
        // every frame — the flicker.
        // `Frame::new()` starts fully transparent (no fill), so a bare custom frame here
        // left the in-vault tabs showing the raw window background — black, regardless of
        // theme — while every other screen (which uses `CentralPanel::default()`'s own
        // frame) tracked the theme correctly. `Frame::central_panel` supplies the same
        // `panel_fill` those screens get; only the margin is customized on top of it.
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show_inside(ui, |ui| {
                match self.tab {
                    Tab::Urgent => self.tab_urgent(ui),
                    Tab::Instructions => self.tab_instructions(ui),
                    Tab::TrustWill => self.tab_trustwill(ui),
                    Tab::Assets => self.tab_assets(ui),
                    Tab::Summary => self.tab_summary(ui),
                    Tab::Accounts => self.tab_accounts(ui),
                    Tab::RealEstate => self.tab_realestate(ui),
                    Tab::Taxes => self.tab_taxes(ui),
                    Tab::GeneralDocuments => self.tab_general(ui),
                }
            });
    }
}

// --- Free helper widgets -----------------------------------------------------

/// Pure lifetime rule for the conspicuous error banner, unit-testable without egui (mirrors
/// the `clipboard_tick_decision` pattern). The banner shows the last hard failure and must
/// disappear as soon as any later status line replaces that text — a success/info message
/// means the failure is no longer current — while staying put as long as the status still
/// equals it. Returns `true` when the stored `error` is stale and should be cleared.
fn error_banner_is_stale(error: Option<&str>, status: &str) -> bool {
    error.is_some_and(|e| e != status)
}

/// Render the CONSPICUOUS error banner for a hard failure: a bright red full-width strip at
/// the top of the window with a ⚠ and the failure message, plus a Dismiss button that clears
/// it (`*error = None`). Does nothing when `error` is `None`. Kept a free function (taking
/// just `&mut Option<String>` and `ui`) so a headless `egui_kittest` harness can drive it
/// without constructing an `eframe::Frame`. Far more visible than the weak status line, so a
/// failed save/upload can't be silently overlooked.
fn show_error_banner(error: &mut Option<String>, ui: &mut egui::Ui) {
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
fn tab_button(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str, accent: egui::Color32) {
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
// the two trees would share expand/collapse state (the TUI keeps separate expand-sets for the
// same reason).
fn render_acct_node(
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
fn list_nav_target(
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
fn stepped_list_index(current: Option<usize>, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    match current {
        Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        None if delta > 0 => 0,
        None => len - 1,
    }
}

fn list_panel(
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
fn empty_form_hint(ui: &mut egui::Ui, noun: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("👈").size(28.0).color(ui_accent(ui).gamma_multiply(0.7)));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("Select {noun} from the list")).strong());
        ui.label(egui::RichText::new("…or click New to add one.").weak().small());
    });
}

// The two reserved status colors for money. They are NOT part of the accent
// family and are never reused as decoration: green means "this is what is owned",
// red "this is what is owed". Both are readable on the light and the dark
// surfaces of all ten themes.
const STAT_GOOD: egui::Color32 = egui::Color32::from_rgb(45, 130, 80);
const STAT_BAD: egui::Color32 = egui::Color32::from_rgb(185, 70, 70);

/// A headline figure with its label: the Summary tab's KPI row.
///
/// Label above in secondary ink (never in the value's color — the number carries
/// the meaning), value below at display size. Read-only presentation of numbers
/// the table below already contains.
fn stat_tile(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.35)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(140.0);
                ui.label(egui::RichText::new(label).weak().small());
                ui.add_space(2.0);
                ui.label(egui::RichText::new(value).size(22.0).strong().color(color));
            });
        });
    ui.add_space(6.0);
}

/// A Config-screen section heading: accent-colored, with the vertical rhythm that
/// separates one settings group from the next. Config used to run every group
/// together in one undifferentiated column.
fn config_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(text).strong().size(16.0).color(ui_accent(ui)));
    ui.add_space(4.0);
}

/// The theme's accent color, recovered from the style. `apply_style` parks it in
/// the selection stroke so free-standing widgets (which have no `GuiApp` to ask)
/// can stay in the palette without threading a color through every call.
fn ui_accent(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().selection.stroke.color
}

/// Save / Delete buttons; returns the chosen action. Renders nothing (and stays
/// `None`) in read-only mode.
fn form_buttons(ui: &mut egui::Ui, writable: bool) -> FormAction {
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

/// A two-column "label + single-line edit" row inside a Grid.
// `value: &mut String` lets the text widget write the user's edits straight back
// into the caller's field.
/// Validate a to-be-saved account, returning the user-facing error for the first
/// missing mandatory field (title, then owner) or `None` when it may be saved. The
/// GUI save path and its tests share this so the rule lives in exactly one place
/// (the TUI enforces the same rule on its `fields[0]`/`fields[3]`).
fn account_required_field_error(a: &Account) -> Option<&'static str> {
    if a.title.trim().is_empty() {
        Some("Title is required — every account must have a title.")
    } else if a.owner.trim().is_empty() {
        Some("Owner is required — every account must have an owner.")
    } else {
        None
    }
}

/// The warning shown before deleting an account that assets/liabilities still link to:
/// states the linked-from count and the consequence — the links are KEPT (no cascade,
/// per the additive/no-silent-loss policy) and will render as unresolved raw ids.
/// `None` when nothing links to the account, in which case delete proceeds unwarned
/// exactly as before. Shared by the form's warning banner and its tests.
fn account_delete_link_warning(linked_from: usize) -> Option<String> {
    if linked_from == 0 {
        return None;
    }
    Some(format!(
        "This account is linked from {linked_from} asset/liability record(s). Deleting it will NOT \
         remove those links — they will show as unresolved ids."
    ))
}

/// Give a freshly-cloned secret field 128 bytes of spare capacity so later per-keystroke
/// edits don't reallocate (which frees the old buffer WITHOUT zeroizing, stranding cleartext
/// in freed heap). Calling `String::reserve` directly on the clone would ITSELF reallocate —
/// the clone has capacity == len — committing the very leak it means to prevent. So we move
/// the value into a roomier buffer and zeroize the original. A no-op once headroom exists
/// (e.g. an empty new-record field), so it is cheap to call every frame.
fn presize_secret(s: &mut String) {
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
fn read_only_value(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Wrap).selectable(true))
}

/// Treat a designed field width as a MAXIMUM, shrinking it to whatever the pane
/// actually offers.
///
/// The form pane scrolls vertically only, so a field wider than the pane is clipped
/// rather than scrolled to. Wide windows are unaffected (the designed width already
/// fits); narrow ones shrink the field instead of losing its right-hand end. The
/// floor keeps a field from collapsing to nothing.
fn fit(ui: &egui::Ui, desired: f32) -> f32 {
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
fn fit_with_buttons(ui: &egui::Ui, desired: f32, buttons: usize) -> f32 {
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
fn field_singleline(ui: &mut egui::Ui, value: &mut String, writable: bool, width: f32) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::singleline(value).desired_width(fit(ui, width)))
    } else {
        read_only_value(ui, value)
    }
}

/// Like [`field_singleline`], but for a field followed by `buttons` controls on the same
/// row — see [`fit_with_buttons`] for why the plain version pushes them off the pane.
fn field_singleline_with_buttons(
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
fn field_singleline_hint(ui: &mut egui::Ui, value: &mut String, writable: bool, width: f32, hint: &str) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(fit(ui, width)))
    } else {
        read_only_value(ui, value)
    }
}

/// A multi-line field: editable when `writable`, else immutable-but-selectable (see
/// [`field_singleline`]).
fn field_multiline(ui: &mut egui::Ui, value: &mut String, writable: bool, rows: usize) -> egui::Response {
    if writable {
        ui.add(egui::TextEdit::multiline(value).desired_rows(rows).desired_width(f32::INFINITY))
    } else {
        read_only_value(ui, value)
    }
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, writable: bool) {
    ui.label(label);
    field_singleline(ui, value, writable, 420.0);
    ui.end_row();
}

/// Render one portal-login section (URL / username / masked password + copy, plus a
/// free-form comment) into the Real Estate form. The password is masked unless
/// `reveal`; `copy_pw` is set when the copy button is clicked, to be acted on after
/// rendering.
#[allow(clippy::too_many_arguments)]
fn portal_section(
    ui: &mut egui::Ui,
    title: &str,
    url: &mut String,
    username: &mut String,
    password: &mut String,
    comment: &mut String,
    reveal: bool,
    writable: bool,
    copy_pw: &mut Option<Zeroizing<String>>,
) {
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    // Each of the four portals is its own card, so they read as four separate
    // logins rather than one long run of near-identical fields.
    card(ui, |ui| {
        ui.label(egui::RichText::new(format!("🔐 {title}")).strong().color(accent));
        ui.add_space(4.0);
        egui::Grid::new(title).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            text_row(ui, "URL", url, writable);
            text_row(ui, "Username", username, writable);
            ui.label("Password");
            ui.horizontal(|ui| {
                // `title` is unique per portal (Property Mgmt / Insurance / HOA / Tax), so
                // it is a valid per-field id salt for the secret-field hardening. Copy stays
                // available read-only (it is a read, not an edit).
                secret_text_edit(ui, title, password, reveal, writable, fit_with_buttons(ui, 260.0, 1), copy_pw);
                if ui
                    .button("📋")
                    .on_hover_text("Copy to the clipboard (cleared automatically after 15 seconds)")
                    .clicked()
                {
                    *copy_pw = Some(Zeroizing::new(password.clone()));
                }
            });
            ui.end_row();
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new("Comment").weak().small());
        // Editable when writable, else immutable-but-selectable (see `field_singleline`).
        // The per-portal `id_salt` keeps the four comment boxes' ids distinct.
        let salt = (title, "comment");
        if writable {
            ui.add(
                egui::TextEdit::multiline(comment)
                    .id_salt(salt)
                    .hint_text("security questions, account numbers, who to ask for…")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        } else {
            let _ = salt; // the id salt only matters for the editable widget
            read_only_value(ui, comment);
        }
    });
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
fn search_box(
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

fn filter_combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[String]) {
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
fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[String], writable: bool) {
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

/// The document attach / export / detach section. Returns the requested action;
/// the caller performs the actual volume operation (to keep `self` borrows
/// disjoint). `attached_present` reflects whether the record currently has a file.
// `#[allow(...)]` silences a specific lint (here: the linter's "too many
// arguments" warning) — it does not change behavior. The `&mut String` inputs
// are the caller's text buffers, edited in place by the widgets below.
/// Outcome of the shared [`doc_section`] widget. Indices refer to the `attached`
/// slice passed in (single-document tabs pass at most one document).
#[derive(PartialEq, Eq, Clone, Copy)]
enum DocSectionReq {
    None,
    Upload,
    Export(usize),
    Remove(usize),
}

impl DocSectionReq {
    /// Map to the single-document [`DocReq`] (Trust & Will / Assets / General),
    /// where there is exactly one slot so the index is irrelevant.
    fn to_single(self) -> DocReq {
        match self {
            DocSectionReq::Upload => DocReq::Attach,
            DocSectionReq::Export(_) => DocReq::Export,
            DocSectionReq::Remove(_) => DocReq::Remove,
            DocSectionReq::None => DocReq::None,
        }
    }
}

/// The uniform document widget used by EVERY document tab (modeled on Trust &
/// Will): it lists the currently-attached documents — each with Export / Remove —
/// and, when writable, shows the **Subfolder / Filename / Upload-from** inputs and
/// an Attach button. Single-document tabs pass a 0-or-1-element `attached` slice;
/// the multi-document tabs pass the full list. The caller maps the returned request
/// to its own handler (so `self` borrows stay disjoint from the widget).
fn doc_section(
    ui: &mut egui::Ui,
    attached: &[String],
    subfolder: &mut String,
    filename: &mut String,
    source: &mut String,
    writable: bool,
) -> DocSectionReq {
    let mut req = DocSectionReq::None;
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    // The whole document area is one card, so a form reads as "fields, then the
    // files that belong to them" rather than as an undifferentiated column.
    card(ui, |ui| {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("📎 Documents").strong().color(accent));
        ui.label(egui::RichText::new("stored encrypted inside the vault").weak().small());
    });
    ui.add_space(6.0);
    if attached.is_empty() {
        ui.label(egui::RichText::new("No documents attached.").weak().italics());
    } else {
        for (i, label) in attached.iter().enumerate() {
            // `shrink_left`: the buttons are placed first and the FILENAME gives up
            // space, so a long name truncates instead of shoving Export/Remove out of
            // the pane.
            egui::containers::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    ui.label(egui::RichText::new("📄").color(accent));
                    ui.add(egui::Label::new(label).truncate()).on_hover_text(label);
                },
                |ui| {
                    if writable
                        && ui
                            .button("🗑 Remove")
                            .on_hover_text("Detach this document from the record and reclaim its space")
                            .clicked()
                    {
                        req = DocSectionReq::Remove(i);
                    }
                    // Export is a read (always allowed); Remove mutates the vault. Export
                    // writes into the directory configured in Config, recreating the document's
                    // volume folder structure — there is no per-export path prompt.
                    if ui
                        .button("⬇ Export")
                        .on_hover_text("Write a DECRYPTED copy into the export directory set in Config")
                        .clicked()
                    {
                        req = DocSectionReq::Export(i);
                    }
                },
            );
        }
    }
    if writable {
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Attach a file").strong().small());
        egui::Grid::new("doc_attach").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Subfolder (optional)");
            ui.add(egui::TextEdit::singleline(subfolder).hint_text("statements").desired_width(fit(ui, 300.0)));
            ui.end_row();
            ui.label("Filename");
            ui.add(egui::TextEdit::singleline(filename).hint_text("statement.pdf").desired_width(fit(ui, 300.0)));
            ui.end_row();
            ui.label("Upload from");
            ui.add(
                egui::TextEdit::singleline(source)
                    .hint_text("/path/on/disk/file.pdf")
                    .desired_width(fit(ui, 300.0)),
            )
            .on_hover_text("The full path to the file. A double-quoted path is accepted as-is.");
            ui.end_row();
        });
        ui.label(
            egui::RichText::new("Leave Filename blank to keep the source file's own name. The original file is not moved.")
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        // Approximate the virtual path length: the stored path also includes the
        // owner-initials/group levels and the <ts>_ filename prefix (~80 bytes, not
        // visible here), so reserve for them. `handle_doc`/`handle_*_doc` do the
        // authoritative check on write.
        let vpath_len = vault::virtual_path(subfolder, filename).len() + 80;
        let over_limit = vpath_len > crate::storage::MAX_PATH_LEN;
        if over_limit {
            ui.colored_label(
                egui::Color32::from_rgb(0xC0, 0x30, 0x30),
                format!("Path may be too long (~{vpath_len} / {} bytes) — shorten the filename or subfolder.", crate::storage::MAX_PATH_LEN),
            );
        }
        if ui
            .add_enabled(
                !over_limit,
                egui::Button::new(egui::RichText::new("⬆ Attach").strong().color(egui::Color32::WHITE)).fill(accent),
            )
            .on_hover_text("Encrypt a copy of the file into the vault's document archive")
            .clicked()
        {
            req = DocSectionReq::Upload;
        }
    }
    });
    ui.add_space(4.0);
    req
}

/// Deferred linked-accounts action gathered while rendering the Assets form (see
/// [`linked_accounts_section`]). `Add`/`Open` carry an Account id; `Remove` carries the
/// index into the asset's `linked_accounts` list. Applied after the columns closure
/// like every other deferred request, so mutation/navigation stays outside the form
/// borrow. Not `Copy` (unlike [`DocSectionReq`]) — two variants own a `String`.
#[derive(PartialEq, Eq, Clone)]
enum LinkReq {
    None,
    Add(String),
    Remove(usize),
    Open(String),
}

/// Resolve an asset's linked-account ids to display rows `(id, resolved label)`. A
/// dangling id (the account was deleted — links are never cascaded) resolves to the
/// RAW id: tolerant and nothing hidden, per the additive/no-silent-loss policy.
fn linked_account_rows(accounts: &[Account], linked: &[String]) -> Vec<(String, String)> {
    linked
        .iter()
        .map(|id| (id.clone(), records::account_label(accounts, id).unwrap_or_else(|| id.clone())))
        .collect()
}

/// The accounts offered by the Assets form's "add link" dropdown: every account NOT
/// already linked (a second link to the same account would be meaningless).
fn link_candidates(accounts: &[Account], linked: &[String]) -> Vec<(String, String)> {
    accounts
        .iter()
        .filter(|a| !linked.iter().any(|id| id == &a.id))
        .map(|a| (a.id.clone(), a.label()))
        .collect()
}

/// The "Linked accounts" section of the Asset/Liability form (modeled on
/// [`doc_section`]): one row per link — Open always (navigation is a read, kept in
/// read-only mode), Unlink writable-only (it edits the record) — plus, when writable,
/// an "add link" dropdown over `candidates`. `linked` comes from
/// [`linked_account_rows`], `candidates` from [`link_candidates`]. The caller applies
/// the returned request after rendering, keeping `self` borrows disjoint.
/// The link dropdown's visible entries for `query`: the `(id, label)` candidates whose LABEL
/// matches, by the same rule as the Accounts search box ([`records::matches_search_soundlike`])
/// — the letters may appear anywhere in the label (no prefix/suffix anchoring), and a
/// sound-alike spelling still matches. An empty query keeps every candidate, in the order the
/// caller supplied. Split out of the popup so the filtering is unit-testable without driving
/// egui's combo popup.
fn filter_link_candidates<'a>(candidates: &'a [(String, String)], query: &str) -> Vec<&'a (String, String)> {
    candidates.iter().filter(|(_, label)| records::matches_search_soundlike(label, query)).collect()
}

fn linked_accounts_section(
    ui: &mut egui::Ui,
    linked: &[(String, String)],
    candidates: &[(String, String)],
    query: &mut String,
    writable: bool,
) -> LinkReq {
    let mut req = LinkReq::None;
    let accent = ui_accent(ui);
    ui.add_space(4.0);
    card(ui, |ui| {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("🔗 Linked accounts").strong().color(accent));
        ui.label(egui::RichText::new("the logins that hold or service this").weak().small());
    });
    ui.add_space(6.0);
    if linked.is_empty() {
        ui.label(egui::RichText::new("No linked accounts.").weak().italics());
    }
    for (i, (id, label)) in linked.iter().enumerate() {
        egui::containers::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.label(egui::RichText::new("🔑").color(accent));
                ui.add(egui::Label::new(label).truncate()).on_hover_text(label);
            },
            |ui| {
                if writable && ui.button("Unlink").on_hover_text("Remove this link (the account itself is untouched)").clicked() {
                    req = LinkReq::Remove(i);
                }
                if ui.button("Open").on_hover_text("Jump to this account on the Accounts tab").clicked() {
                    req = LinkReq::Open(id.clone());
                }
            },
        );
    }
    if writable {
        ui.add_space(4.0);
        // Hand-rolled (id, label) dropdown: the shared `combo`/`filter_combo` helpers
        // bind a &mut String VALUE from a String list, but a link stores the account's
        // ID while showing its LABEL — so there is no bound buffer; a click on an entry
        // just emits the Add request (nothing is "currently selected").
        //
        // The popup opens with a SEARCH box: a vault with dozens of accounts made this a long
        // scroll where the user had to recognise the right login by eye. Typing narrows the
        // list to the matching accounts (which brings the wanted one to the top, right under
        // the cursor) using the same forgiving rule as the Accounts search — the letters may
        // appear ANYWHERE in the label, not just at its start, and a sound-alike spelling
        // still matches. The list scrolls inside a bounded area so a big vault's popup can
        // never grow taller than the window.
        let out = egui::ComboBox::from_id_salt("asset_link_add")
            .selected_text("➕ Link an account…")
            .show_ui(ui, |ui| {
                let sb = search_box(
                    ui,
                    query,
                    "type to find an account…",
                    "Filters the accounts below. The letters may appear anywhere in the \
                     account's label, and a name that SOUNDS like it still matches.",
                    accent,
                    220.0,
                );
                // Focus the box as the popup opens so the user can just start typing. Gated on
                // an empty query so it is not re-requested on every later frame, which would
                // fight the user for focus if they clicked into the list.
                if query.is_empty() && !sb.has_focus() {
                    sb.request_focus();
                }
                ui.separator();
                let hits = filter_link_candidates(candidates, query);
                if candidates.is_empty() {
                    ui.label(egui::RichText::new("(no more accounts to link)").weak());
                } else if hits.is_empty() {
                    ui.label(egui::RichText::new("(no account matches that search)").weak().italics());
                }
                egui::ScrollArea::vertical().max_height(240.0).id_salt("asset_link_add_scroll").show(ui, |ui| {
                    for (i, (id, label)) in hits.iter().enumerate() {
                        let resp = ui.selectable_label(false, label.as_str());
                        // On each keystroke, bring the best (first) remaining match into view,
                        // so a long list follows what is being typed instead of staying
                        // wherever it was last scrolled to.
                        if i == 0 && sb.changed() {
                            resp.scroll_to_me(Some(egui::Align::TOP));
                        }
                        if resp.clicked() {
                            req = LinkReq::Add((*id).clone());
                        }
                    }
                });
            });
        // The closure runs only while the popup is open (`inner` is `None` otherwise), so this
        // is the moment the popup closed: forget the query, and the next open starts from the
        // full list rather than a stale filter the user has to notice and clear.
        if out.inner.is_none() {
            query.clear();
        }
    }
    });
    ui.add_space(4.0);
    req
}

/// A collapsing, timestamped history view for a record.
// `&[records::Change]` is a read-only slice of change entries.
fn history_view(ui: &mut egui::Ui, history: &[records::Change]) {
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
fn secret_text_edit(
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
/// True if two `vault.pmv` paths refer to the same vault on disk (canonicalized when both
/// exist, else compared raw). Used to refuse "update from another vault" pointed at itself.
fn same_vault_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn password_field(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut String,
    copied_out: &mut Option<Zeroizing<String>>,
) -> bool {
    // Always masked (revealed = false); the secret hardening (undo scrub + copy
    // re-route) still applies — a master password is the most sensitive of all.
    // Always editable (`writable = true`): this is the unlock/create field, which
    // exists before any vault is open, so the read-only mode does not apply here.
    // `copied_out` surfaces a built-in Ctrl+C of the master password so the caller
    // arms the auto-clear (otherwise it would linger on the clipboard).
    let resp = secret_text_edit(ui, id_salt, value, false, true, fit(ui, 280.0), copied_out);
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Build `(id, label)` pairs for a record list.
// `<R: Record>` is a generic: this works for any type `R` that implements the
// `Record` trait (i.e. exposes `.id()` and `.label()`). `&[R]` is a slice of
// such records. `.to_string()` makes an owned `String` from the borrowed id.
fn label_list<R: Record>(list: &[R]) -> Vec<(String, String)> {
    list.iter().map(|r| (r.id().to_string(), r.label())).collect()
}

/// Copy the record `buf` is editing back out of `saved` (the vault's own list), replacing
/// the buffer with the stored copy. The per-tab dispatcher is [`GuiApp::sync_edit_buffer`],
/// which documents when this may be called and why it must be.
///
/// A no-op if the buffer is empty or its id is not in `saved` — a record deleted from
/// under the form leaves the buffer as the user's only remaining copy of that editing
/// session, so it is never dropped on the floor here.
fn sync_from_saved<R: Record>(buf: &mut Option<R>, saved: &[R]) {
    let Some(cur) = buf.as_ref() else { return };
    let Some(stored) = saved.iter().find(|s| s.id() == cur.id()) else { return };
    // Assigning drops the old buffer, which zeroizes the secrets it held.
    *buf = Some(stored.clone());
}

/// Best-effort clearing of the system clipboard on exit.
fn clear_clipboard() {
    // `let _ = ...` ignores the `Result`: if the clipboard is unavailable there
    // is nothing useful to do. Setting it to an empty `String` overwrites any
    // copied secret.
    let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(String::new()));
}

// `#[cfg(test)]` is conditional compilation: this module is compiled ONLY when
// running tests, so it adds nothing to the shipped binary. `use super::*` pulls
// in everything from the parent module (this file) for the tests to exercise.
#[cfg(test)]
#[path = "gui_tests.rs"]
mod tests;

// Headless egui-driven verification (egui_kittest): runs the REAL `render_acct_node`
// through a real egui Context + accesskit, simulates a real click, and observes widget
// visibility — i.e. drives the actual GUI surface without a window.
#[cfg(test)]
#[path = "gui_kittest_tests.rs"]
mod kittest_tests;

#[cfg(test)]
#[path = "gui_glyph_tests.rs"]
mod glyph_tests;
