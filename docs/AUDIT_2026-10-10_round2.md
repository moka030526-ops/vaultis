# Audit — 2026-10-10, round 2

A change-scoped static + dynamic audit of the **uncommitted pre-upgrade safety-copy work** on
top of `2e0872f` (v0.5.0):

* `vault/upgrade.rs` (new) — each vault records the release that last wrote it
  (`Vault::written_by`, inside the encrypted body). The first WRITABLE open by a newer release
  copies the vault aside before anything is written; a vault from a newer release is refused for
  writing; a "safety-copy all vaults" action makes the same copy on demand.
* The copy is a transaction: staged in `.incomplete-…`, verified byte for byte, fsync'd,
  marked `SAFETY-COPY-COMPLETE`, then atomically renamed into place. A failure or crash at any
  step refuses the writable open and leaves the vault untouched and unstamped.
* `get_vaultis.bat` keeps the version it replaces (`previous\` on Windows; the app's data
  folder on macOS).

**Threat model:** unchanged from [`THREAT_MODEL.md`](THREAT_MODEL.md). In scope as usual:
theft of or tampering with the on-disk files, a crafted vault, malicious paths and names, and —
specific to this change — **someone who can write to the vault root without the passwords**
(the same attacker the `prefs.json` design is built around), who can plant folders in
`vaultis-backups/`.

**What this round went after.** This is the first code in the project that **deletes files
in a folder it does not exclusively own**, on the user's behalf, during an ordinary open. So
the round asked four questions:

1. Can the new version ever write to a vault without a finished, durable copy of it first —
   on a full disk, on a crash, on any error path?
2. Can the deletion (retention pruning, crash-leftover cleanup) be steered onto anything it
   should not touch: a symlink's target, another vault's copy, the copy just made?
3. Does the stamp break compatibility in either direction?
4. Does the read-only gate still hold?

**Result: three defects found and fixed during the work, all in the new code, none
shipped.** Two came from the round's own tests and review, one from the user asking the
right question. Nothing was found in pre-existing code.

---

## Findings (all in the uncommitted change; fixed before commit)

### B-1 — the first version of the copy was not durable and not atomic (Medium, data loss on crash)

**What the code did.** It copied, verified by reading back, and returned. It did not fsync, and
a crash mid-copy left a folder with a final-looking name.

**Why it matters.** After a verified-but-unflushed copy the open-time save rewrites
`vault.pmv`. A power cut then can leave the *live* vault rewritten by the new release while the
copy never reached the disk — exactly the scenario the copy exists for. And a half-written
folder with a real-looking name could pass for a copy, to the user and to retention.

**Fix.** The staged/verified/fsync'd/marked/renamed transaction described above, with fault
points `safety.copied` and `safety.committing`.

**Regression tests.** `a_crash_mid_safety_copy_never_lets_the_new_version_write_and_is_retried`
(subprocess, `PMVAULT_CRASH_AT` at both points: the child must die, the vault must be
byte-identical and unstamped, nothing may count as a copy, and the retry must produce exactly one
finished copy); `a_full_disk_during_the_safety_copy_refuses_the_open_and_leaves_no_half_copy`
(ENOSPC at both points); `leftovers_of_a_crashed_copy_are_cleared_and_never_count_as_a_copy`;
`a_finished_copy_carries_its_completion_marker_and_no_staging_is_left`.

### B-2 — retention could delete the newest copy (Medium, the guarantee defeated)

**What the code did.** Copies made within one second got `_1`, `_2`… by taking the first FREE
slot, and retention sorted names as text. After pruning freed the unsuffixed name, the next
copy reused it and sorted as the OLDEST — so the copy just made was the one pruned. As text,
`_10` also sorted before `_9`.

**How it was found.** The retention test failed in every configuration once a "never prune the
copy just made" guard was added (the guard turned a silent deletion into a visible count of 4).

**Fix.** Suffixes are one past the highest in use (never a freed slot); ordering is by
`(timestamp, suffix as a number)`; and the copy just made is never pruned, whatever else the
folder holds.

**Regression tests.** `same_second_copies_keep_strict_order_past_ten_and_never_reuse_a_pruned_name`,
`only_the_newest_copies_are_kept_and_manual_copies_never_evict_upgrade_ones`,
`far_future_named_folders_can_never_push_out_the_copy_just_made`.

### B-3 — crash-leftover cleanup matched other vaults by name prefix (Low, robustness)

**What the code did.** Clearing vault `a`'s `.incomplete-a@…` matched `.incomplete-a@b@…`, the
in-progress copy of a different vault named `a@b`. That copy's rename would then fail and refuse
its open — a safe failure, but a wrong one.

**Fix.** Leftovers are parsed exactly as retention parses copies (`<name>@<label>@<time>`,
`@`-free label). **Regression test:** `clearing_leftovers_never_touches_a_vault_whose_name_merely_starts_the_same`.

---

## Leads chased and refuted

* **Can the new release write before the copy exists?** No. The stamp check and the copy run
  inside `open_inner` after decryption and before the document store opens or the open-time save
  runs; `written_by` is set in memory only after the copy returns. Every error path out of the
  copy is an `Err` from the open (`SafetyCopyFailed`), so no handle exists to write with. The
  crash and ENOSPC tests above observe it.
* **Does the copy decrypt anything?** No. `copy_vault_tree` copies files; the only decryption is
  the open's own, which the user's passwords already authorize. The copy needs no key (the
  "safety-copy all vaults" action works with no passwords at all).
* **Can planted folders redirect the deletion?** `prune` and `clear_incomplete` only touch real
  directories (`symlink_metadata`, never followed) whose names parse as this module's own; prune
  only ever deletes folders holding the completion marker, and never the copy just made.
  `std::fs::remove_dir_all` does not follow a top-level symlink and is race-hardened inside
  (CVE-2022-21658). The backups folder itself must be a real directory or the copy refuses. An
  attacker who can write to the vault root can delete the copies outright, so pruning grants them
  nothing new.
* **Can the stamp put a hostile path on disk?** It is authenticated (inside the encrypted body),
  but filtered to `[A-Za-z0-9._-]`, ≤ 32 chars, before reaching a folder name
  (`../../etc` → `v....etc`).
* **Compatibility.** New code reads unstamped vaults (`#[serde(default)]`; the committed golden
  fixture now also goes through the upgrade path). Older releases ignore the field (no
  `deny_unknown_fields` anywhere); when they save they drop it, which costs one extra copy on the
  next new-release open — the safe direction. The on-disk format stays v4.
* **Read-only gate.** A read-only open never copies and never stamps (tested).
* **Concurrency.** Every copy of a vault runs under that vault's single-writer lock (the writable
  open holds it; the manual free function takes it), so two copies of one vault cannot race, and
  leftover cleanup cannot remove an in-progress copy of the same vault.

## Verification

| Check | Result |
|---|---|
| `cargo +1.99.0 clippy` — workspace all-features; desktop and core `--no-default-features` | clean |
| Test matrix: workspace, release, fault-injection, core no-default, desktop no-default, lock pair | **2,524 passed, 0 failed** |
| Temp-dir pollution (`/tmp/vaultis-backups`) after the matrix | none |
| Installer, bash half: `bash -n` on the extracted section | OK |
| Installer, PowerShell half: parse + PSScriptAnalyzer | parses; no findings beyond the existing `Write-Host` style |
| Installer, PowerShell `previous\` block, run for real under `pwsh` | keeps the programs, replaces on re-run, never copies or touches a vault or user file, refuses a vault planted in `previous\` |
| Fuzzing, 120 s per target (shared CPU with the matrix) | 63,153,057 executions, **0 crash artifacts** |
| `cargo mutants --in-diff` (82 mutants: `upgrade.rs` + the open-path hunk) | **partial**: 18 of 82 tested — 14 caught, **0 missed**, 4 unviable, 0 timeouts |

## Not run, or partial

* **No Windows host and no Mac**: the installer changes were checked by parsing, linting, and —
  for the PowerShell block — executing it under `pwsh` on Linux; the macOS block by `bash -n`
  only. The real installs were not run.
* **Directory fsync is best-effort**, as everywhere in this codebase (some filesystems refuse
  it); file fsync is strict. A filesystem that silently ignores fsync defeats both, as it would
  defeat the vault's own commit protocol.
* **Mutation testing is partial: 18 of the 82 mutants** in the change were tested (14 caught,
  0 missed, 4 unviable) before the run was stopped to cut the release; the full run needs about
  two more hours at `-j 3`. This round therefore does **not** claim "no surviving mutant" for
  the safety-copy code. The behaviours it exists for are each pinned by a test that was seen to
  fail on the defective version (B-1, B-2, B-3 above), which is stronger evidence for those
  specific behaviours than a sample of mutants, but says nothing about the rest. Finish with:
  `TMPDIR=$PWD/target/mutants-tmp cargo mutants --in-diff round.diff -p vaultis-core --copy-target false -j 3`.
* **The marker records sizes, not checksums**: the copy is verified byte for byte when made,
  but later bit-rot inside a copy would not be detected by the marker. Adding a hash would need
  a new crypto dependency in the core; not done in this round.
