//! The look of the window: color themes, the accent palette, interface scale, typeface,
//! the window icon and minimum size, and the per-root `prefs.json` persistence of all of
//! these.

use super::*;

/// A light egui theme — brighter than the default light visuals (panels and
/// widget faces lifted toward white for a lighter overall feel).
pub(super) fn light_visuals() -> egui::Visuals {
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
pub(super) enum Theme {
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
    pub(super) const ALL: [Theme; 16] = [
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
    pub(super) fn id(self) -> &'static str {
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
    pub(super) fn label(self) -> &'static str {
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
    pub(super) fn from_id(id: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// Build the egui visuals for a theme. Each curated palette starts from egui's
/// light or dark base and overrides the panel/widget fills, the text color, and
/// the selection color for a coherent look.
pub(super) fn visuals_for(theme: Theme) -> egui::Visuals {
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
pub(super) fn accent(theme: Theme) -> egui::Color32 {
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
pub(super) enum UiScale {
    Compact,
    #[default]
    Normal,
    Large,
    Larger,
    Largest,
}

impl UiScale {
    pub(super) const ALL: [UiScale; 5] =
        [UiScale::Compact, UiScale::Normal, UiScale::Large, UiScale::Larger, UiScale::Largest];

    /// Stable id for prefs.json (never the label — labels are free to be reworded).
    pub(super) fn id(self) -> &'static str {
        match self {
            UiScale::Compact => "compact",
            UiScale::Normal => "normal",
            UiScale::Large => "large",
            UiScale::Larger => "larger",
            UiScale::Largest => "largest",
        }
    }

    pub(super) fn from_id(id: &str) -> Option<UiScale> {
        UiScale::ALL.into_iter().find(|s| s.id() == id)
    }

    pub(super) fn label(self) -> &'static str {
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
    pub(super) fn factor(self) -> f32 {
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
pub(super) enum FontChoice {
    /// Ubuntu-Light — the proportional face bundled with egui.
    #[default]
    Default,
    /// Hack — the fixed-width face bundled with egui; unambiguous digits and letters.
    Monospace,
}

impl FontChoice {
    pub(super) const ALL: [FontChoice; 2] = [FontChoice::Default, FontChoice::Monospace];

    pub(super) fn id(self) -> &'static str {
        match self {
            FontChoice::Default => "default",
            FontChoice::Monospace => "monospace",
        }
    }

    pub(super) fn from_id(id: &str) -> Option<FontChoice> {
        FontChoice::ALL.into_iter().find(|f| f.id() == id)
    }

    pub(super) fn label(self) -> &'static str {
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
pub(super) fn apply_fonts(ctx: &egui::Context, choice: FontChoice) {
    ctx.set_fonts(font_definitions(choice));
}

/// The font set for a choice, as a PURE function of it.
///
/// Split out from [`apply_fonts`] so the self-containment property can be asserted
/// directly: reading it back off a live `egui::Context` needs a frame in progress,
/// which a unit test has no reason to fake.
pub(super) fn font_definitions(choice: FontChoice) -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    if choice == FontChoice::Monospace
        && let Some(list) = defs.families.get_mut(&egui::FontFamily::Proportional)
    {
        list.insert(0, "Hack".to_owned());
    }
    defs
}

/// Load the saved typeface (see [`load_theme`] — same prefs file, same best-effort rules).
pub(super) fn load_font_choice(vault_root: &str) -> FontChoice {
    crate::prefs_path(vault_root).map(|p| load_font_choice_from(&p)).unwrap_or_default()
}

pub(super) fn load_font_choice_from(path: &std::path::Path) -> FontChoice {
    crate::effective_prefs_obj_from(path)
        .get("font")
        .and_then(|v| v.as_str())
        .and_then(FontChoice::from_id)
        .unwrap_or_default()
}

pub(super) fn save_font_choice(vault_root: &str, font: FontChoice) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_font_choice_to(&path, font);
    }
}

/// Persist the typeface, preserving every other prefs key (theme, ui_scale, export_dir…).
pub(super) fn save_font_choice_to(path: &std::path::Path, font: FontChoice) {
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
pub(super) const MIN_INNER_SIZE: [f32; 2] = [900.0, 670.0];

/// The window/taskbar icon, decoded from the committed 512×512 PNG that the desktop
/// shortcuts already use, so the window, the launcher and the Desktop shortcut all
/// show the same vault mark instead of a generic placeholder.
///
/// Embedded with `include_bytes!` rather than read from disk at runtime: the icon must
/// not depend on the repository still being present next to the binary, and a missing
/// file must not be able to change what the program does. Decode failure is not fatal —
/// the window simply opens with the platform default, exactly as before.
#[cfg(feature = "gui")]
pub(super) fn window_icon() -> Option<egui::IconData> {
    // The locked-vault mark (the read-only default), matching packaging/linux's
    // "vaultis (View)" launcher.
    const PNG: &[u8] = include_bytes!("../../../../packaging/icons/vaultis-locked.png");

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
pub(super) fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_visuals(visuals_for(theme));
    apply_style(ctx, theme);
}

/// How much of the monitor the window's minimum size may claim. The remainder absorbs
/// the things a monitor's raw size does not account for: the title bar and borders the
/// window manager adds OUTSIDE the inner size this floor describes, plus a taskbar,
/// dock or panel. A floor that exactly equalled the monitor would leave a window that
/// cannot be placed fully on screen.
pub(super) const MONITOR_FIT: f32 = 0.9;

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
pub(super) fn min_inner_size(monitor: Option<egui::Vec2>) -> egui::Vec2 {
    let want = egui::vec2(MIN_INNER_SIZE[0], MIN_INNER_SIZE[1]);
    match monitor {
        // `> 1.0` rejects the degenerate/unknown sizes a backend can report before the
        // window is mapped, which would otherwise clamp the floor to nothing.
        Some(m) if m.x > 1.0 && m.y > 1.0 => want.min(m * MONITOR_FIT),
        _ => want,
    }
}

/// The monitor size egui currently reports for this window, if any.
pub(super) fn monitor_size(ctx: &egui::Context) -> Option<egui::Vec2> {
    ctx.input(|i| i.viewport().monitor_size)
}

/// Apply a UI scale, and re-assert the window's minimum size for it.
///
/// The floor exists so the lock screen — which is meant not to scroll — always fits. It is
/// re-sent here rather than only at startup because [`min_inner_size`] clamps to the
/// display, and a scale change moves the content's physical size against a fixed screen.
pub(super) fn apply_ui_scale(ctx: &egui::Context, scale: UiScale) {
    ctx.set_zoom_factor(scale.factor());
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min_inner_size(monitor_size(ctx))));
}

/// Load the saved UI scale (see [`load_theme`] — same prefs file, same best-effort rules).
pub(super) fn load_ui_scale(vault_root: &str) -> UiScale {
    crate::prefs_path(vault_root).map(|p| load_ui_scale_from(&p)).unwrap_or_default()
}

pub(super) fn load_ui_scale_from(path: &std::path::Path) -> UiScale {
    crate::effective_prefs_obj_from(path)
        .get("ui_scale")
        .and_then(|v| v.as_str())
        .and_then(UiScale::from_id)
        .unwrap_or_default()
}

pub(super) fn save_ui_scale(vault_root: &str, scale: UiScale) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_ui_scale_to(&path, scale);
    }
}

/// Persist the scale, preserving every other prefs key (theme, export_dir, …).
pub(super) fn save_ui_scale_to(path: &std::path::Path, scale: UiScale) {
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
pub(super) fn apply_style(ctx: &egui::Context, theme: Theme) {
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

// The color theme is stored in the shared, non-secret `prefs.json` alongside the
// export directory (see `crate::prefs_path` / `crate::read_prefs_obj` in `prefs.rs`). The
// theme accessors live here because they reference the GUI-only `Theme` type; the
// generic prefs primitives and the export-dir accessors are shared in `crate`.

/// Load the saved theme from the standard preferences path.
pub(super) fn load_theme(vault_root: &str) -> Theme {
    crate::prefs_path(vault_root).map(|p| load_theme_from(&p)).unwrap_or_default()
}

/// Load the theme from a specific path. Best-effort/bounded: missing/symlinked/over-cap/
/// unparseable all fall back to the default — a UI preference must never block startup.
pub(super) fn load_theme_from(path: &std::path::Path) -> Theme {
    crate::effective_prefs_obj_from(path).get("theme").and_then(|t| t.as_str()).and_then(Theme::from_id).unwrap_or_default()
}

/// Persist the chosen theme to the standard preferences path.
pub(super) fn save_theme(vault_root: &str, theme: Theme) {
    if let Some(path) = crate::prefs_path(vault_root) {
        save_theme_to(&path, theme);
    }
}

/// Persist the theme to a specific path, preserving any other prefs keys (export_dir).
pub(super) fn save_theme_to(path: &std::path::Path, theme: Theme) {
    let mut obj = crate::read_prefs_obj(path);
    obj.insert("theme".into(), serde_json::Value::String(theme.id().to_string()));
    crate::write_prefs_obj(path, &obj);
}
