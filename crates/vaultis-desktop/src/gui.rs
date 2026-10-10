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

// The window is split by screen and tab. Each submodule adds its own `impl GuiApp` block
// (a type's methods may be spread over several modules of the crate), and the free
// helpers they define are glob-imported back here so every part of the GUI — and its
// tests — sees one flat namespace, exactly as when this was a single file.
mod accounts; // Accounts tab + asset <-> account links
mod appearance; // theme, scale, font, window size + their prefs
mod assets; // Assets & Liabilities tab
mod auth; // lock screen: open / create / change passwords
mod config; // Config screen
mod documents; // document attach/export/remove for the doc-bearing tabs
mod export; // CSV + document export out of the vault
mod merge; // "Update from another vault" screen
mod realestate; // Real Estate tab
mod summary; // Summary tab
mod taxes; // Taxes tab
mod text_tabs; // Urgent, Instructions, Trust & Will, General Documents tabs
mod widgets; // shared helper widgets
mod zakat; // Zakat ledger tab

use accounts::*;
use appearance::*;
use documents::*;
use summary::*;
use widgets::*;

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

/// A screen, tab and open record id: the form a refusal belongs to.
type FormContext = (Screen, Tab, Option<String>);

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
    Zakat,
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
    /// Whether the lock screen offers an "Open for editing" switch for `writable`, instead
    /// of fixing it at launch with `--write`. True on macOS only: a Mac app opens one way —
    /// there is no second shortcut to carry `--write`, as Windows and Linux have — so the
    /// choice moves onto the lock screen. It is still made BEFORE the vault opens and
    /// still defaults to read-only, so it is the same decision in a different place.
    mode_switchable: bool,
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
    // The Zakat tab is a TABLE, not a list + single-record form, so its working buffer is
    // the whole ledger rather than one selected record: the user edits several years' cells
    // and saves them together. Loaded from the vault by `sync_edit_buffer(Tab::Zakat)` and
    // written back by the tab's Save.
    edit_zakat: Vec<records::ZakatEntry>,
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
    /// The status text most recently raised as a failure or refusal (by [`GuiApp::fail`] or
    /// [`GuiApp::refuse`]). While `status` still equals it, the status line draws in the bold
    /// alert color instead of the quiet default; any later message replaces the text and so
    /// ends the alert on its own, with no reset needed at the many plain `status =` sites.
    alert: Option<String>,
    /// Where the current refusal was raised: the screen, tab and record id on show when
    /// [`GuiApp::refuse`] ran. A refusal is about THAT form's input, so once the user moves
    /// to another record (➕ New, a list pick, another tab or screen) it no longer applies
    /// and is cleared — otherwise "Title is required" would sit in red over a fresh, blank
    /// form the user has not tried to save yet (see [`refusal_is_stale`]).
    refused_in: Option<FormContext>,
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
        // `launch::initial_root_and_name` for why, and the prefs comment in `prefs.rs`.
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
        // persisted, so a tampered prefs.json can't unmask passwords (see `prefs.rs`).
        let reveal_default = crate::load_reveal_all_default(&vault_root);
        let group_assets_default = crate::load_group_assets_default(&vault_root);
        let group_accounts_default = crate::load_group_accounts_default(&vault_root);
        // Hoisted above the struct literal because `vault_root` is moved into the struct
        // below; the vault-root fallback needs to read it before that move.
        let export_dir = crate::load_export_dir(&vault_root);
        GuiApp {
            path,
            writable,
            mode_switchable: cfg!(target_os = "macos"),
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
            edit_zakat: Vec::new(),
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
            alert: None,
            refused_in: None,
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
        self.alert = Some(msg.clone());
        self.status = msg;
    }

    /// Record a REFUSAL: an action the app declined because of the user's input (a missing
    /// required field, a name already taken, …). Nothing broke, so no top banner, but the
    /// status line draws it in the bold alert color so "not saved" never looks like a
    /// routine "Saved."-style notice.
    fn refuse(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.alert = Some(msg.clone());
        self.refused_in = Some(self.form_context());
        self.status = msg;
    }

    /// The screen, tab and open record id right now — what a refusal is tied to.
    fn form_context(&self) -> FormContext {
        (self.screen, self.tab, self.edit_id())
    }

    /// The id of the record open in the current tab's form, or `None` when nothing is
    /// open. Zakat edits its whole table at once, so it has no per-record id.
    fn edit_id(&self) -> Option<String> {
        match self.tab {
            Tab::Urgent => self.edit_urgent.as_ref().map(|r| r.id.clone()),
            Tab::Instructions => self.edit_instruction.as_ref().map(|r| r.id.clone()),
            Tab::TrustWill => self.edit_trustwill.as_ref().map(|r| r.id.clone()),
            Tab::Assets => self.edit_asset.as_ref().map(|r| r.id.clone()),
            Tab::Accounts => self.edit_account.as_ref().map(|r| r.id.clone()),
            Tab::RealEstate => self.edit_realestate.as_ref().map(|r| r.id.clone()),
            Tab::Taxes => self.edit_taxfiling.as_ref().map(|r| r.id.clone()),
            Tab::GeneralDocuments => self.edit_general.as_ref().map(|r| r.id.clone()),
            Tab::Zakat | Tab::Summary => None,
        }
    }

    /// Drop a refusal once the form it was about is no longer on show. Only a status that
    /// is still the refusal text is cleared; any later message has already replaced it.
    fn clear_stale_refusal(&mut self) {
        if refusal_is_stale(self.refused_in.as_ref(), &self.form_context()) {
            if self.alert.as_deref() == Some(self.status.as_str()) {
                self.status.clear();
            }
            self.alert = None;
            self.refused_in = None;
        }
    }

    /// Whether the status line should be drawn as an alert: the current message is a
    /// failure/refusal raised via [`GuiApp::fail`]/[`GuiApp::refuse`], or an export caveat.
    fn status_is_alert(&self) -> bool {
        is_export_caveat(&self.status) || self.alert.as_deref() == Some(self.status.as_str())
    }

    fn clear_doc_inputs(&mut self) {
        self.doc_subfolder.clear();
        self.doc_filename.clear();
        self.doc_source.clear();
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
            // Zakat's buffer is the whole table, so "dirty" is a whole-table comparison:
            // a cell edited, a row added with ➕ New Year, or a row deleted all show up as
            // the buffer no longer matching the saved ledger. `!=` on the two slices is an
            // element-wise `PartialEq` (order included — both come from the same source and
            // only ever grow at the end, so order can't drift on its own).
            Tab::Zakat => self.edit_zakat != v.zakat,
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
            // Zakat's buffer is the whole table: re-read the saved ledger verbatim, which is
            // what makes the post-save state match (upsert stamps `updated_at` and appends
            // history, so the saved rows are never identical to the buffer that produced them).
            Tab::Zakat => self.edit_zakat = v.zakat.clone(),
            // Summary is a read-only computed view — no edit buffer, nothing to sync.
            Tab::Summary => {}
        }
    }

    // --- Main: top bar + active tab ------------------------------------------

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
            tab_button(ui, &mut self.tab, Tab::Zakat, "🌙 Zakat", accent);
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

    // --- Shared deferred operations ------------------------------------------

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
            // The bulk trim edits EVERY tab's records, including Zakat's — and Zakat renders
            // from its own table buffer, so re-read it or the tab keeps showing the untrimmed
            // rows and would write them back on its next Save.
            self.sync_edit_buffer(Tab::Zakat);
        }
        n
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
                // Zakat deletes a specific ROW, not "the current record" — there is no single
                // selected record on a table tab — so it has its own `delete_zakat_row`.
                Tab::Zakat => {}
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
        self.clear_stale_refusal();
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
                        let alert = self.status_is_alert();
                        let alert_color = alert_color(ui.visuals());
                        ui.label(
                            egui::RichText::new("•")
                                .color(if alert { alert_color } else { accent })
                                .small(),
                        );
                        // A long message truncates here rather than widening the window;
                        // hover carries the full text. The export caveat is worded to put
                        // its warning FIRST so truncation can only ever eat the path.
                        let text = egui::RichText::new(&self.status).small();
                        let text = if alert { text.color(alert_color).strong() } else { text };
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
                    Tab::Zakat => self.tab_zakat(ui),
                }
            });
    }
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
