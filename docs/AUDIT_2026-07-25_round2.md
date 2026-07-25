# Deep Audit — 2026-07-25, round 2

A second, deeper pass over the same tree as [`AUDIT_2026-07-25.md`](AUDIT_2026-07-25.md), run
immediately after that round's six fixes landed. Round 1 worked outward from the prefs/export
layer; this round deliberately went after the surfaces round 1 did **not** name — the
single-instance guard, the launch/path resolution, the merge apply path, the central
sanitizers, and the numeric-cast and file-I/O primitives across all four crates — plus a
re-audit of round 1's own new security code.

**Threat model:** unchanged from [`AUDIT_2026-07-03.md`](AUDIT_2026-07-03.md).

**Result:** no Critical, no High, no Medium. The cryptographic core, the on-disk format, the
crash-safety machinery and round 1's fixes all came through clean. **Two Low findings** were
confirmed and fixed, both in the same place — the shared character/name sanitizers that every
untrusted-string path funnels through — and both of the "a guard enforced at some sites but not
all" shape that round 1 kept turning up.

The honest summary of this round is that it found less than round 1 because there is less left
to find. Most of the time went into confirming that guards hold, and several promising leads
were chased and **disproved** (recorded below, so the next round need not re-chase them).

---

## Findings fixed

### R2-1 — The spoof-character set missed a bidi control and every invisible form except zero-width (Low, spoofing)

`crates/vaultis-core/src/records.rs`

`is_spoofy_format_char` is the single definition of "a character that can lie about what a
string says". Everything untrusted passes through it: `display_safe` (merge-preview labels the
user authorizes, CSV cells, the CLI extract listing), `doc_filename` (a **real on-disk
filename**), and `is_safe_doc_path` / `doc_tree_relpath` (untrusted manifest and mirror paths).

It was an enumerated list of seven ranges. Enumerating a deny-set by hand is exactly the kind
of thing that ages badly, and it had: **154 Unicode `Cf` format characters passed through
unaltered**, because `char::is_control` — the other half of the test — is `Cc`-only and catches
none of them. Two of those groups matter:

* **U+061C ARABIC LETTER MARK is a bidi control.** It is the ALM counterpart of LRM/RLM
  (U+200E/U+200F), *both of which were already covered*, and it reorders the neutral characters
  around it — digits and punctuation — with no override character required. So the function
  that exists to stop `report<RLO>txt.exe` still let a stored filename be made to display with
  its digits transposed: `invoice<ALM>2024.pdf`.
* **Characters that draw no glyph at all** — U+00AD SOFT HYPHEN, U+2061–U+2064 (INVISIBLE
  TIMES / SEPARATOR / PLUS), U+180E, U+206A–U+206F, U+FFF9–U+FFFB (interlinear annotation, which
  hides the run it brackets), the blank Hangul fillers, and the **entire U+E0000 TAGS block** —
  an invisible shadow ASCII alphabet, the modern "invisible text" smuggling vector. Two labels
  or two exported filenames differing only by these are *indistinguishable on screen*, so a
  crafted merge source could put a decoy in the preview that reads as a record the user trusts,
  and a vault could hold two documents whose exported filenames look identical.

**Fix.** The set now covers both families: every bidi control, and everything that renders as
nothing. It is deliberately **not** "all of `Cf`" — the Arabic/Syriac/Kaithi number and
honorific signs, the Egyptian hieroglyph joiners, the musical beam/tie marks, and the variation
selectors a colour emoji needs are left alone. Those have a real visible rendering; neutralizing
them would mangle honest labels for no security gain, since they neither hide nor reorder. The
reasoning is written into the doc comment so the line is maintainable rather than folklore.
Regression test: `display_safe_neutralizes_the_alm_bidi_control_and_every_invisible_form`,
which also pins the not-neutralized set so a later "just reject all Cf" cannot quietly regress it.

### R2-2 — Two copies of the reserved-device-name rule had drifted, and one of them dropped a path level (Low, availability/correctness)

`crates/vaultis-core/src/{records,vault}.rs`, `crates/vaultis-desktop/src/main.rs`

A Windows reserved device name (`CON`, `NUL`, `COM1`, …) must not become a real path component:
on Windows it resolves to a *device*, so `con.pdf` opens the console and the write fails. For an
estate vault whose entire purpose is an heir retrieving documents, that is the difference between
a document and an I/O error.

The rule existed **twice**, and the copies disagreed:

| | core `doc_tree_relpath` (GUI/TUI export, `export_tree`) | CLI `safe_relative_path` (`vaultis extract`) |
|---|---|---|
| recognized | `CON PRN AUX NUL COM1-9 LPT1-9` | the same list, re-implemented inline |
| repair | **renames** → `_CON` | **drops the component entirely** |

Two consequences. First, one vault extracted to **two different trees** depending on which
front-end wrote it — for a project that goes out of its way to guarantee `vaultis DIR` and
`vaultis-gui DIR` behave identically, that is a real inconsistency. Second, *dropping* a
directory level collapses documents that differ only by that level into one folder; the CLI's
`unique_path` then renames the collision to `_1`, so nothing is lost, but the tree no longer
reflects the vault.

Both copies also missed two forms Windows genuinely folds onto a device: the console handles
**`CONIN$` / `CONOUT$`**, and the **superscript spellings `COM¹`/`COM²`/`COM³`** (U+00B9/B2/B3),
which Windows' path canonicalization maps to `COM1`/`COM2`/`COM3`.

**Fix.** One definition: `records::is_windows_reserved_name` is now `pub`, widened to those
forms, and the CLI calls it and applies the core's `_`-prefix repair, so both front-ends lay the
same vault out identically and the reserved level survives instead of collapsing.
`vault::doc_tree_relpath` was made `pub` purely so the agreement can be **asserted** rather than
assumed — drift between duplicated rules was the bug, so the test pins the two together.
Regression tests: `windows_reserved_names_cover_the_console_handles_and_superscript_com_forms`,
`safe_path_repairs_windows_reserved_names_exactly_like_the_core_exporter`, and
`safe_path_agrees_with_the_core_exporter_on_reserved_and_spoofy_paths`.

---

## Leads chased and disproved (so the next round need not re-chase them)

* **Round 1's own H-1 fix — verified airtight.** The `VAULT_FALLBACK_KEYS` `retain` sits at the
  single chokepoint (`effective_prefs_obj_from`), and all seven prefs keys were traced
  individually: the two cosmetic ones are the only ones the vault-media file can reach;
  `export_dir` and `reveal_all_default` read through the filtered path and are excluded from
  the allowlist; `vault_root` / `last_vault` / `theme` never consult the fallback at all.
* **`is_windows_reserved_name` not applied to directory components.** Suspected, then
  disproved: `doc_tree_relpath` applies it per component, and `doc_slug`'s
  ASCII-alphanumeric allowlist means a group named `CON` becomes `_con` on export, not a device.
  (What *was* wrong there is R2-2 — the CLI twin — not the core.)
* **Truncating numeric casts.** Every `as u32` / `as usize` in the core was walked against its
  guard. `frame_len = (NONCE_LEN + ct.len()) as u32` is bounded by the `MAX_DOC_SIZE` (64 MiB)
  check upstream; `read_file_capped`'s `Vec::with_capacity(hint)` is clamped to `max` before the
  cast. None can truncate or over-allocate.
* **Raw file I/O outside the hardened helpers.** The round-1 L-1/M-2 class is fully closed:
  there is no `fs::read`, `fs::write`, or bare `File::create` left in non-test code in any of the
  four crates. Every reader is `O_NOFOLLOW` + size-capped; every writer is `O_EXCL` 0600 →
  rename, fsync'd, with the parent directory synced.
* **Cleartext export permissions.** `write_export_bytes` → `write_new_bytes` is
  `create_new_0600` + `harden_file` + fsync, with the partial file unlinked on any error,
  symlinked export dirs rejected before `create_dir_all`, symlinked *intermediate* dirs rejected
  under a reused export root, and `unique_export_path` so an export never clobbers.
* **The single-instance guard** (`single_instance.rs`). The lock file is opened `O_NOFOLLOW` in a
  private 0700 per-user runtime dir, with a documented refusal to fall back to a world-writable
  temp dir (which would let another user pin every launch to "already running"). The focus socket
  transports no trusted bytes — the connection *is* the signal.
* **The merge apply path** (`vault.rs` `plan_merge_from` / `apply_merge_from`). Source vault id
  allowlisted before it reaches the UI or the audit log; duplicate source ids selected and
  applied first-occurrence-once; blob ids/paths re-validated at the moment of use; blobs
  re-encrypted under the destination key, never frame-copied; the `referenced ⊆ stored` check
  runs *before* any record mutation so a storage anomaly aborts with both copies intact; a failed
  save poisons the handle. Category types are `display_safe`d identically on the preview and the
  apply side, so what the user authorizes is what is stored.
* **The `mlock` page refcount** (`crypto.rs`, fixed in `6eb84f0`). The rollback in `acquire`
  drains only the pages already counted (the `push` happens after the lock succeeds), the
  address-wrap case is rejected, and a poisoned mutex leaks a lock rather than double-unlocking.
* **CSV row injection via the sanitizer gap.** `csv.rs` relies on "after `display_safe` no CR/LF
  remains" to skip quoting. Re-checked against the 154 uncovered characters: CR, LF, NEL (U+0085,
  which is `Cc`) and U+2028/U+2029 were all already caught, so the gap could not break a row —
  it was a spoofing issue only, which is why R2-1 is Low and not Medium.

---

## Verification

* `cargo test --workspace` — **640 tests, 0 failures** (637 before; 3 new regression tests, at
  least one per finding, one of which cross-checks the two sanitizers against each other).
* `cargo clippy --workspace --all-targets -- -D warnings` — clean.
* `cargo check -p vaultis --bin vaultis --no-default-features` (static/terminal-only) and
  `cargo check -p vaultis-ffi` (mobile feature set) both compile.
* libfuzzer, re-run against the changed sanitizers: `doc_paths` **6.12 M** executions and the
  end-to-end `merge_from` **1.2 k** (real crypto + disk I/O per iteration) — zero crashes.
