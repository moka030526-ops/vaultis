# Changelog

All notable changes to **vaultis** (the offline, two-password encrypted estate
vault) are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project aims to follow [Semantic Versioning](https://semver.org/).

The full, per-finding security write-up for the hardening work below lives in
[`docs/HARDENING.md`](docs/HARDENING.md); the design rationale is in
[`docs/DESIGN.md`](docs/DESIGN.md).

## [Unreleased]

These changes are committed but **not yet tagged** — the workspace crates are still
at `0.1.0`. When cutting the release, rename this section to the chosen version +
date and bump the crate versions to match.

### Added

- **Idle auto-lock on mobile (2 minutes).** Backgrounding already locked the vault, but a
  phone left face-up and untouched stayed foregrounded and unlocked indefinitely — the most
  likely way this vault gets read by someone who is not its owner. Any touch restarts the
  countdown; the timer runs on a *monotonic* clock, so changing the device's time zone or
  clock cannot extend it. The desktop has no equivalent, making this one of the few places
  the mobile app is deliberately stricter.
- **Anti-tap-jacking on Android.** `filterTouchesWhenObscured` on the content root discards
  touches delivered while another app's window is drawn over ours, so a malicious overlay
  cannot render its own interface and quietly route the taps into "Reveal" or "Copy"
  underneath — turning a password the attacker cannot see into one on the clipboard.
- **[`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md)** — a plain answer to "can spy software
  on my phone or computer read my vault?", covering what the encryption does and does not
  cover, the realistic Android vectors (accessibility-service scraping, hostile keyboards,
  root) and the desktop ones (same-user memory access, no screen-capture protection, no
  idle lock), and what actually helps for both security and not-losing-the-data.
- **[`mobile/INSTALL_ANDROID.md`](mobile/INSTALL_ANDROID.md)** — step-by-step install on a
  real phone: toolchain, build, USB install, moving the encrypted vault into app-private
  storage, and an honest account of the debug-vs-release-signing trade-off (the debug build
  is currently the only one a vault can be loaded into, because the in-app import is still
  a roadmap item).

### Fixed

- **Android UI fixes for modern phones.** The app was built against targetSdk 35, where
  Android 15 makes edge-to-edge mandatory, but handled no window insets — the top app bar
  would have rendered under the status-bar clock and the "Unlock" button under the
  gesture-navigation pill. It also had no dark theme (a dark-mode phone got a
  full-brightness white screen, both from Compose and as a white launch flash from the
  window theme), ignored the system Back button so the standard "go back to the list"
  gesture instead quit the app and forced a full re-unlock, and could not scroll the unlock
  screen — so with the keyboard up on a short screen or in landscape, the Unlock button was
  unreachable. All four fixed: `safeDrawingPadding` + `enableEdgeToEdge`, a day/night colour
  scheme and window theme, an expect/actual back handler (enabled only on the detail screen —
  on the list, Back still leaves the app, which locks the vault), and a scrollable unlock
  screen.
- **Desktop → mobile hardening-parity sweep 2026-07-26** — every hardening measure the
  desktop front-ends apply, checked one by one against the Android/iOS app (write-up in
  [`docs/HARDENING.md`](docs/HARDENING.md) §3.1h). Most were already at parity; the
  mobile-only downgrades (`mlock` off, no single-writer lock, cleartext in the managed
  heap) are unchanged and still disclosed. Two gaps closed:
  - **The mobile app never showed "Last opened … (generation N)" (Med).** Both desktop
    front-ends print it after unlocking, and both halves are tamper signals only the
    owner can judge: an access time you do not recognise means someone else opened the
    vault with your two passwords, and a generation that went *down* means the file was
    rolled back to an older copy. The FFI already exposed both values; the mobile UI
    discarded them and showed only the recovery notice — so an unauthorised open was
    **silent on the phone but visible on the desktop**. The app now renders the desktop's
    exact banner and priority order, with the timestamp formatted by a new
    `Vault::previous_access_label()` so the calendar math stays in the one audited
    implementation instead of being rewritten in Kotlin.
  - **The host never wiped its own copies of the master passwords (Low).** `open_vault`
    wipes the Rust-side copies and documents that the host must clear its own; it did
    not. The `ByteArray`s are now overwritten in a `finally` (mutable, so this is a real
    wipe), and both unlock fields are cleared after **every** attempt — success and
    failure alike, matching the desktop's `wipe_passwords()`; a failed attempt is exactly
    when someone is likely to set the phone down.
- **Deep audit 2026-07-25, round 2** — a second, deeper pass targeting the surfaces round 1 did
  not name: the single-instance guard, launch/path resolution, the merge apply path, the central
  sanitizers, and the numeric-cast and file-I/O primitives (full write-up in
  [`docs/AUDIT_2026-07-25_round2.md`](docs/AUDIT_2026-07-25_round2.md)). No Critical, High or
  Medium findings. Confirmed and fixed:
  - **A bidi control and every invisible character slipped the spoof filter (Low).** The
    hand-enumerated set behind `display_safe` — which guards the merge preview the user
    authorizes, CSV cells and real on-disk export filenames — let **154 Unicode format
    characters** through, because the other half of the test (`char::is_control`) is `Cc`-only.
    Among them: U+061C ARABIC LETTER MARK, a bidi control that reorders adjacent digits and
    punctuation and is the direct counterpart of the LRM/RLM marks *already* covered; and every
    glyphless form — soft hyphen, the invisible math operators, interlinear annotation, the blank
    Hangul fillers and the whole U+E0000 TAGS block — so two labels or filenames could be made
    indistinguishable on screen. Both families are now neutralized. Format characters with a
    genuine visible rendering (Arabic/Kaithi number signs, hieroglyph joiners, musical marks) and
    the variation selectors a colour emoji needs are deliberately left intact.
  - **Two copies of the Windows reserved-name rule had drifted (Low).** A reserved device name
    must not become a real path component (`con.pdf` opens the console, so the write fails — an
    heir extracting on Windows gets an I/O error instead of the document). The rule existed twice
    and disagreed: the core *renames* the component `_CON`, while the `extract` CLI's own copy
    **dropped** it — so one vault extracted to two different trees depending on which front-end
    wrote it, and dropping a directory level collapsed documents that differ only by it into one
    folder. Both now share one definition, widened to the `CONIN$`/`CONOUT$` console handles and
    the superscript `COM¹`/`COM²`/`COM³` spellings Windows folds onto `COM1`–`COM3`. A test now
    asserts the two sanitizers agree, since the drift itself was the bug.
- **Deep audit 2026-07-25** — a workspace-wide bug hunt and security review across the core,
  the desktop front-ends, the FFI and the dependency tree (full write-up in
  [`docs/AUDIT_2026-07-25.md`](docs/AUDIT_2026-07-25.md)). Confirmed and fixed:
  - **A vault could choose where this machine writes cleartext (High, security).** The
    vault-root `prefs.json` fallback — the feature that lets a portable vault carry its own UI
    defaults — could supply `export_dir`, the destination for the per-tab CSV (**every account
    and portal password in the clear**) and every decrypted document, plus
    `reveal_all_default`, which opens every password tab unmasked. Since that file ships with
    the vault media, a vault received from someone else could silently redirect exported
    secrets or disable masking. The fallback is now restricted to a deny-by-default allowlist
    of purely cosmetic view keys; both security-relevant keys are read from the local config
    directory alone.
  - **GUI/TUI exports could land inside the vault folder (Medium, security).** The CLI has
    long refused to write cleartext into the live vault directory (the user's next backup or
    folder sync sweeps it up); the windowed and terminal front-ends had no such check. The
    `dest_inside` guard moved into the library and both front-ends now apply it — at every
    export site and when the directory is set in Config — including through a symlinked export
    directory that resolves back into the vault.
  - **Unbounded, symlink-following prefs read (Medium).** `read_prefs_obj` pre-checked with
    `symlink_metadata` and then read with `fs::read`, which follows symlinks and allocates
    without bound — the stat-then-read gap the core closes everywhere else, and reachable
    because the fallback reads that file from untrusted vault media. It is now an
    `O_NOFOLLOW`, size-capped read.
  - **Prefs written through a planted symlink (Low).** `write_prefs_obj` used `fs::write`
    (follows a symlink, truncates in place); it is now an atomic `O_EXCL` 0600 temp → rename.
  - **Rebuilt manifests bypassed the entry cap (Low, availability).** `MAX_MANIFEST_ENTRIES`
    was enforced on the stored-manifest, write and import paths but not on the manifest-loss
    rebuild, which could produce an over-cap manifest that the next write commits and every
    later open then refuses — bricking an intact vault. The rebuild now fails closed with the
    same error.
  - **Non-finite Summary totals (Low).** A total that overflowed to infinity rendered as the
    literal `$18,446,744,073,709,551,615` (saturating float→int cast) and `NaN` as `$0`; both
    now render `$—`.
- **Deep audit 2026-07-03** — a workspace-wide, adversarially-verified bug hunt and security
  review (full write-up in [`docs/AUDIT_2026-07-03.md`](docs/AUDIT_2026-07-03.md)). Confirmed
  and fixed:
  - **Brick on ~100 k documents / hostile merge (High).** `VolumeStore::put` could grow a
    partition manifest past the read-side `MAX_MANIFEST_ENTRIES` cap that the next `open`
    rejects as unrecoverable — reachable by normal heavy use or by a hostile merge source
    packing tiny blobs. `target_partition` now rolls partitions on the entry-count cap too.
  - **Vault-file brick (Medium).** A large additive merge could push `vault.pmv` past the
    read-side `MAX_VAULT_SIZE`; `write_vault_file` now fails closed before committing.
  - **Silent password-change rollback (Medium, security).** Entering the OLD password after a
    committed rekey that left a stranded old-epoch redundancy copy could "recover" the
    pre-rekey vault and destroy the new-epoch copies. Recovery is now confined to the
    current, corroborated salt (bit-rot recovery of a genuinely damaged salt still works).
  - **Recovery-ring erosion (Low).** The outgoing generation is now validated (decodes under
    the current key) before being ringed in, so a bit-rotted primary can't replace a good
    generation.
  - **GUI same-frame nav race (High, data-integrity).** A keyboard arrow landing in the same
    frame as a Delete/confirm click could act on the neighboring record; the nav swap is now
    suppressed when a record-targeted action is pending.
  - **Cross-vault UI-state leak (Medium, security).** Edit buffers (holding cleartext
    secrets), armed deletes, and filters no longer survive a lock→re-unlock into a different
    vault.
  - **Delete-rollback committed unsaved edits (Medium).** A failed-persist delete rollback now
    restores the last-SAVED record, not the dirty edit buffer.
  - **Partial-plaintext shred warning (Medium, security).** A failed `export-tree`/`extract`
    into a non-empty directory now warns that pre-existing subtrees may also hold cleartext.
  - **Supply chain:** `anyhow`/`memmap2` patch-bumped to clear unsoundness advisories;
    `quick-xml`/`ttf-parser` advisories documented as build-time/bundled-only and ignored with
    rationale. `cargo audit` and `cargo deny` are green.
  - All six fuzz targets re-run (~128 M executions, zero crashes); full test suite green.

### Added

- **A comprehensive in-app manual (GUI).** The Help screen is now a searchable, two-pane
  browser — a topic index on the left, the article on the right — over **22 articles in 5
  sections**: getting started, one article per tab, working with records (editing, secrets,
  documents, links, history), settings & maintenance (every Config setting, merges, backups,
  compaction), and reference (security model, keyboard, the full CLI, troubleshooting, FAQ).
  Search matches every word of every article, not just titles (AND semantics across words).
  The content lives as plain data in the new `gui_help` module, so the search is a pure
  function and the manual's structure is unit-tested; a headless `egui_kittest` test lays out
  every article. Replaces the previous eight collapsible blurbs. **No behavior change.**

- **URGENT tab.** A new free-text note collection placed **first**, before Instructions, so
  the most time-critical things an executor needs (whom to call, where the safe key is, an
  in-flight crisis) are the first thing shown on unlock. Same shape as Instructions (a title
  + free-text body per note), available in both the **GUI** and **TUI**, and the default
  landing tab. Merges (`update-from`), CSV export (`urgent.csv`), history, and the bulk
  trim/compact all cover it; old vaults load unchanged (`#[serde(default)]`, format stays v4).
  The read-only mobile viewer does not expose it yet.

- **Asset ↔ account links.** An asset/liability can now be linked to any number of
  Accounts records by **stable record id** (the vault's first record→record reference;
  design + trade-offs in [`docs/ASSET_ACCOUNT_LINKS.md`](docs/ASSET_ACCOUNT_LINKS.md)):
  - **GUI:** a "Linked accounts" section on the Assets editor — add via dropdown,
    **Open** jumps to the account (retargeting any filters that would hide it),
    **Unlink** removes; the Accounts editor shows a read-only **"Linked from"** list
    with jump-back buttons.
  - **TUI:** a numbered "Linked accounts" sub-list on the Assets edit screen with an
    add-link chooser; **Ctrl+L** link, **Ctrl+O** open link `#N`, **Ctrl+X** unlink.
  - **Semantics:** links survive save/reopen/merge (ids are copied verbatim by
    `update-from`); deleting a linked account never cascades and is surfaced — the GUI
    requires a second **Delete anyway** click behind a red linked-from-N warning, the
    TUI reports the linked-from count in the status line — and a dangling link renders
    as the raw id and stays removable (additive/no-silent-loss policy). History logs a content-free
    "linked accounts changed" line; the assets CSV gains a `linked_accounts` column
    holding account labels (raw id for dangling links). Old vaults load unchanged
    (`#[serde(default)]`, format stays v4); the mobile FFI surface is intentionally
    untouched.

- **Update from another vault.** A new way to pull changes from a SECOND vault into the
  current one: records that are **newer** (by `updated_at`) or **new** in the other vault,
  together with the **documents** they reference, are previewed and then applied. It is
  **one-way and additive** — it never deletes anything from the current vault. Surfaces:
  - **CLI:** `vaultis update-from OTHER [DIR]` (prompts four passwords: the current vault's
    two, then the other vault's two). `--dry-run` previews the patch without writing.
  - **GUI:** Config → "Update from another vault…" (writable only) → enter the other vault's
    folder + its two passwords → preview the exact records/documents → Apply.
  - **TUI:** Config → **Ctrl+U** → same collect → preview → apply flow.
  - Engine in `vaultis-core::merge` + `OpenVault::plan_merge_from`/`apply_merge_from`: blobs
    are re-encrypted under the destination key (never byte-copied), the apply is crash-safe
    add-only (every referenced blob is durable before the `vault.pmv` that references it), the
    source vault is opened read-only with its errors collapsed (no password-correctness
    oracle), and records that depend on a locally-deleted (tombstoned) document are skipped.
  - **Category reconciliation:** a merged record's `asset_type`/`account_type`/`subtype` that
    the destination's lists lack is added to them (previewed + counted), so the merged types
    show up in **Config** and the dropdowns instead of being invisible.
  - **Hardening:** the apply checks `referenced ⊆ stored` *before* mutating and poisons the
    handle on a save failure (so a never-committed merge can't be re-flushed); the GUI/TUI drop
    a poisoned handle back to the unlock screen. Verified by fault-injection crash-recovery
    tests (force-kill at each commit step, incl. redundancy), in-process ENOSPC tests, a
    `merge_from` fuzz target, and `cargo-mutants` (0 missed on `merge.rs`).
- **Mobile apps.** Native Android and iOS apps (Compose Multiplatform UI over the
  audited core via a UniFFI/Gobley FFI). Read-only viewer surface: open with the two
  passwords, browse the tabs, view a record, read its history. The Android APK builds
  in CI; iOS builds on a Mac (see `mobile/iosApp/IOS_SECURITY_VERIFY.md`).
- **Taxes tab** — tax filings keyed by year, each with a per-year document folder.
- **Real Estate tab** — property records with management/insurance/HOA/**tax** portal
  logins (url + username + password), a **per-portal comment** block, financing balance,
  free-text comments, and multiple attached documents.
- **General Documents tab** — standalone titled documents, on a uniform document
  path layout (`<root>/<auto-group>/<compact-utc>/[subfolder]/<filename>`) shared by
  every document-bearing tab, with a single, consistent attach/export widget.
- **Accounts enhancements:**
  - **Title** field (shown in the list as `Title - Account Type - Username`, with its
    own filter and new-entry prefill).
  - **Mandatory title and owner** — an account cannot be saved without both (enforced
    in the GUI and TUI; see `account_required_field_error`).
  - **Grouped tree view** — toggle the list into an `owner → type → subtype → title`
    tree (empty grouping levels are skipped, no "(none)" buckets).
  - **Closed as of** date field.
  - **Faceted (cross-filtering) filters** — type/subtype/owner/title each narrow to
    the values still valid under the other active filters, auto-clearing stale picks.
  - **Reveal** is a single global toggle on the Accounts and Real Estate screens
    (there is no per-record reveal); it clears on tab switch so it can't linger.
  - **Keep-visible-on-save** — editing a filtered field moves the active filter to the
    saved value (incl. the review-only and username-search filters) so the entry never
    silently vanishes.
  - **New-from-filter** — clicking *New* under active filters pre-populates the form.
  - **Trim all fields** — every field of **every record type** (all tabs, secrets
    included) is left/right-trimmed on save, plus a one-off bulk-trim action that
    sweeps the whole vault (history-recorded).
- **Assets/Liabilities: grouped tree view** — toggle the Assets list into a grouped tree
  **owner → Asset/Liability → type** (empty levels skipped), mirroring the Accounts grouping:
  a "grouped" checkbox in the GUI, `g` in the TUI (`records::asset_tree`). Honors the
  review-only filter.
- **Assets/Liabilities: Title field** — a short title, shown under **Owner** in the editor
  (GUI + TUI) and used as the list label when set (falling back to the description). Additive
  and `#[serde(default)]`, so older vaults load unchanged; also surfaced over the mobile FFI.
- **Start-page vault picker** — the unlock/create screen now selects a vault by **root +
  a collapsed "Vault" control** instead of a free-form directory path. An editable **Vault
  root** is scanned one level deep (`launch::discover_vaults` lists immediate sub-directories
  holding a `vault.pmv`; never recursive), and the **Vault** control is an editable leaf name
  with a **dropdown**: pick an existing vault (→ Unlock) or type a new folder name (→ Create,
  with `--write`). The open target is always `<root>/<name>`. Discovery reports access problems
  instead of hiding them: an unreadable root or any skipped (inaccessible) entry surfaces a
  warning. GUI uses an `egui::ComboBox`; the TUI cycles the Vault row with `←/→`.
  - The chosen **root is remembered** across sessions as a local, non-secret preference
    (`vault_root` in `prefs.json`, never inside a vault), and an explicit `vaultis DIR` launch
    still takes precedence over it.
  - The Config **backup destination** now defaults to that root (still editable).
- **Config: delete an unused category** — asset types, account types, and account
  subtypes can be deleted from Config, **only when no live record uses them** (history
  mentions never block); an account type with subtypes must have those removed first.
- **Color themes** — ten curated palettes (Light, Dark, High contrast, Solarized,
  Sepia, Nord, Dracula, Gruvbox Dark, Gruvbox Light, Rosé Pine); the choice persists
  in a small non-secret prefs file and applies on the lock screen too.
- **Packaging & platform:** Windows GUI-subsystem binary (no console window on
  launch); desktop shortcuts with locked/unlocked vault icons; packaging docs.
- **CI / tooling:** GitHub Actions verification suite (clippy `-D warnings`, tests,
  fault-injection crash/full-disk recovery, Windows cross-compile, parser fuzz smoke,
  Android APK); `cargo-deny` supply-chain policy and the `doc_paths` fuzzer as standing
  checks; a release-mode test job.

### Changed

- **Tofu boxes removed from the GUI.** The app ships as a single self-contained binary with
  no asset files, so it can only draw glyphs present in the fonts egui *bundles*. Several
  characters were outside that set and rendered as `□` on screen — including three that
  predate the visual overhaul: `⤓` (the CSV/Export buttons), `✕` (Dismiss), `⟵` (Back to
  Config), plus `→` in several status messages and tooltips. All replaced with bundled
  equivalents (`⬇`, `×`, `⬅`, `>`). A new test derives the character set from the GUI source
  with `include_str!` — so a newly introduced glyph is checked automatically rather than
  quietly shipping — and verifies each against egui's bundled fonts, with a control pair so a
  broken probe cannot pass silently.

- **CSV export is now available in read-only sessions** (GUI and TUI), at the vault owner's
  explicit request. It was previously write-mode-only on the reasoning that an Accounts or Real
  Estate CSV holds every password in plain text and a read-only heir should not be able to
  bulk-dump secrets. That gate is removed; the warning it enforced is now carried where the user
  can act on it — the button's tooltip states the file is unencrypted and includes plaintext
  passwords, and every successful export reports "… — UNENCRYPTED, incl. any passwords." rather
  than a bare success. `➕ New` and the other record-mutating controls remain write-only. The
  manual's read-only/write-mode lists were updated to match, and the two tests that pinned the
  refusal now pin the new behaviour (including that the written CSV really does contain the
  plaintext password it warns about).

- **Read-only values are text again, not boxes.** A read-only session rendered every stored
  value in a disabled text box sized to the pane, so a four-letter owner name occupied the same
  356 px as a full address and a form read as a column of near-empty boxes. Values now render
  as left-justified, wrapped, still-selectable text that takes the width of its content. When a
  single word (a path, a URL, a record id) is too long for the line it breaks with a **trailing
  hyphen** rather than stopping mid-character, via a pure `wrap_hyphenated` line-breaker that is
  unit-tested against a fake measurer — including multi-byte text and degenerate widths.
  Display-only: the stored value never changes, so the dashes exist purely on screen. Editable
  fields in write mode keep their designed box width.

- **GUI scrolling put on the right frames.** The tab body no longer sits inside one
  both-axis `ScrollArea`. Scrolling now belongs to the frames that actually overflow: each
  tab's **list pane** and its **form pane** scroll vertically and independently, and only
  Summary's wide table scrolls both ways. A scroll area hands its contents unbounded space on
  its scrolling axes, so the old nesting laid the inner vertical scrollers out against
  infinite height — they never decided they needed a scrollbar, while the outer horizontal bar
  appeared, took width, forced a re-layout, and vanished again. Alongside it:
  - Every right-aligned row (top bar, list headers, document and link rows, status bar, error
    banner) now uses `egui::Sides`, which sizes the gap from the real available width in one
    pass, instead of a right-to-left layout nested in a wrapping row whose width estimate
    could disagree with itself between frames. Long vault names, filenames, and status
    messages truncate (full text on hover) rather than pushing controls out of the window.
  - Designed field widths are now **maxima** that shrink to the pane (`fit`), so a narrow form
    pane shrinks its fields instead of clipping their right-hand end.
  - The window's minimum size drops from 720×480 to **560×400**, so a small screen can shrink
    the window to fit.
  - **"⤓ CSV" and "➕ New" moved back beside their list heading.** Right-aligning them pushed
    them against the divider between the panes, where CSV read as part of the form and was
    easy to miss. (Summary has no CSV button by design — it is a calculated view with no
    `csv::CsvTab`.)
  - Guarded by a test that lays the real window out across a width sweep from 480 px up,
    requiring it to settle and keeping the CSV control reachable at every width.

- **Graphical interface visual overhaul.** A shared design system replaces egui's
  debug-tool defaults, applied through `apply_theme` (palette + typography/spacing/shape) with
  one accent color per theme. **Purely presentational — no control changed what it does:**
  - **Type & spacing:** a real heading step, 14.5 px body, roomier control padding, rounded
    corners, and an accent focus ring.
  - **Top bar:** shows *which* vault is open (folder name, full path on hover) with a
    WRITE / 🔒 READ-ONLY badge; global actions right-aligned; the tab strip gained per-tab
    glyphs and an accent underline on the active tab.
  - **Cards:** documents, linked accounts, the four Real Estate portals, and the
    Accounts/Assets/Real-Estate control strips are each framed, so a form reads as grouped
    regions instead of one long column. Config's ten settings groups gained accent headings.
  - **Summary tab:** a headline stat row (total assets, total liabilities, net worth, owners)
    above the table, with the two reserved status colors also applied to the Liability and Net
    columns — always alongside a word label or the number's own sign, never color alone.
  - **Status bar** is now always present (it used to appear and disappear, shifting the whole
    tab by a row) and reports when the clipboard is holding a copied secret.
  - **Empty states:** the unselected form pane and an empty list now say what to do instead of
    rendering blank; the Accounts filter row shows a "filtered" badge when filters are hiding
    rows.
  - **Lock screen** is a centered, width-limited card that states the session's mode before a
    password is typed.
  - Verified by a headless `egui_kittest` test that lays out every tab (with and without a
    record selected), Config, and Help.

- **Cargo workspace split** into `vaultis-core` (audited, `#![forbid(unsafe_code)]`),
  `vaultis-desktop` (GUI/TUI/CLI), and `vaultis-ffi` (the only `unsafe`-permitting
  crate, for the UniFFI scaffolding).
- **Feature-gated** `mlock` and the single-writer file lock — on for desktop, off for
  the mobile build (which serializes access in-process).
- **Release profile** now sets `overflow-checks = true` (fail-closed on integer
  overflow) in addition to `strip`.
- New fields are additive (`#[serde(default)]`); the on-disk **format stays v4** and
  older vaults open unchanged.
- **Read-only mode is a true view, not an editor.** A read-only session previously let
  you type into a record's form fields (edits were silently discarded on close). The
  fields can no longer be edited — but in the GUI they remain **selectable and copyable**
  (bound to an immutable `&str` buffer) so you can highlight and copy a value without
  changing it. Only the color theme can be changed; backup and document export (both
  read-only-safe) remain available.

### Security

Six rounds of adversarial multi-agent audit (including a 152-agent and a 159-agent
hunt, an overnight three-phase autonomous sweep, and a dynamic-verification round)
fixed **36 confirmed defects**; none broke the cryptographic envelope (no finding lets
an attacker read a vault they could not already open). Highlights:

- **Dynamic verification (round 6)** — moved past static review: mutation testing
  (`cargo-mutants`) on the changed security core (closed the one test gap it found,
  in `trim_all_records`), a fresh fuzzing run (~67 M executions, 0 crashes), and a new
  exhaustive **every-byte tamper matrix** asserting any single-byte change to a vault
  file fails closed without panicking. Two fixes landed: **momentary reveal** (the
  "reveal all" toggles now clear on tab switch instead of persisting) and a
  **fail-closed `staged_rewrite`** (a future index/manifest desync errors instead of
  silently storing an empty document path).

- **Rekey crash-durability (round 5)** — the password-change commit renamed the new
  `volume/`/`manifest/`/`vault.pmv` into place but fsync'd the directory only once at the
  end, so a power loss could leave a new-key `vault.pmv` durable while the new
  volume/manifest weren't — an unopenable vault. Each commit step now fsyncs the
  directory, enforcing the staged order on disk.
- **Clipboard auto-clear on Ctrl+C (round 5)** — a password copied with the built-in
  Ctrl+C / cut / context-menu (including the master-password fields) was hardened but
  never armed the 15 s auto-clear or on-exit wipe; it now routes through the same armed
  path as the 📋 button.
- **Desktop no-oracle parity (round 5)** — the desktop unlock no longer shows a distinct
  message for correct-password-only failures (`ArchiveMismatch`/`Json`/`Storage`),
  closing the same "this password is correct" oracle the FFI already folds.

- **Untrusted-import path safety** — `import_tree` symlink **TOCTOU** that could
  launder an arbitrary file (e.g. `/etc/shadow`) into the vault is closed with
  `O_NOFOLLOW`; blob ids restricted to a lowercase-hex allowlist (rejects Windows
  ADS/drive-relative/device-name escapes and case-insensitive-FS collisions);
  document paths reject control bytes **and** Unicode bidi/zero-width spoofing;
  duplicate ids in a mirror are rejected (closes a version-rollback vector).
- **Deletion durability** — a deleted document could be resurrected by a manifest-loss
  rebuild and made permanent by compaction; an authenticated deletion **tombstone**
  now keeps deletes deleted.
- **No-oracle contract (FFI)** — every open failure (wrong password, any corruption,
  the post-decrypt `ArchiveMismatch`, the pre-decrypt size-cap `TooLarge`) collapses to
  one `WrongPasswordOrCorrupt` variant, so the read-only mobile surface is never a
  correct-password oracle.
- **Open-time DoS resistance** — bounded distinct-salt key derivations and lazy
  one-buffer-at-a-time redundancy recovery; KDF cost ceiling lowered (1 GiB → 512 MiB
  memory) and validated on **both** the read and write paths so a vault can't be
  written that won't reopen, and a tampered header can't force a multi-GiB allocation.
- **Secret hygiene** — password **history** values are masked in the UI; clipboard
  copies are flagged sensitive on every platform (Linux `exclude_from_history`,
  Android `EXTRA_IS_SENSITIVE` + `FLAG_SECURE`, iOS `UIPasteboard` local-only + expiry,
  scene-phase snapshot overlay, real file Data Protection); the egui password fields no
  longer retain secret snapshots in the undo buffer or bypass the clipboard hint on
  copy; FFI password buffers are wiped even on a panic-unwind.
- **Backup integrity** — backups run under the single-writer lock (no corrupt snapshot
  under a concurrent rekey) and refuse symlinked source/destination.
- **CLI safety** — a value-flag could swallow the vault-dir positional and retarget a
  destructive `compact` onto the default vault; the resolved target is now validated
  and echoed.
- **Tooling assurance** — extended fuzzing (~183 M executions across five parser
  targets, 0 crashes), mutation testing (≈99–100 % kill rate on the changed security
  code), AddressSanitizer clean on the FFI, and `cargo-audit` + `cargo-deny` clean.

### Fixed

- **Regression:** the in-app *Backup* button self-deadlocked on the session's own
  write lock; it now reuses the held lock (`OpenVault::backup`).
- The redundancy-recovery notice no longer cries "data may be lost" when the recovered
  copy is actually the current generation.
- `"Saved." / "Deleted."` status messages are gated on the write actually reaching
  disk (no false success on a full disk / read-only handle).

## [0.1.0] — initial baseline

The foundational offline estate vault:

- **Crypto:** two required passwords → chained **Argon2id** key derivation →
  **XChaCha20-Poly1305** AEAD, with the entire file header (magic, version, KDF
  params, salt, nonce) authenticated as associated data. Wrong password and a
  corrupt/tampered vault fail closed and indistinguishably (no oracle); secret
  material is zeroized and (on desktop) memory-locked.
- **Front-ends:** desktop **GUI** (egui) and **TUI** (ratatui) over one shared vault
  API, plus a **CLI** (`compact`, `backup`, `extract`, `export-tree`, `import-tree`,
  `verify`).
- **Storage:** records and a **partitioned encrypted document store** inside a single
  vault directory; **crash-safe** atomic writes (temp → fsync → rename → dir-fsync)
  with manifest-rebuild recovery, and optional in-place redundancy (mirror + prior
  generations) with a generation counter for rollback detection.
- Read-only by default (mutations require `--write`); editable category type lists
  stored inside the encrypted vault.
