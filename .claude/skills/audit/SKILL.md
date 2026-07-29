---
name: audit
description:
  "Adversarial security and correctness audit of vaultis: static verification
  (clippy, fmt, cargo-audit, cargo-deny), dynamic verification (the full test
  matrix, fuzzing, fault injection, mutation testing), and a manual review pass
  aimed at the failure classes that actually matter for an offline encrypted
  estate vault. Activates on 'audit', 'security review', 'check for
  vulnerabilities', 'verify this is safe', 'static and dynamic verification', or
  before cutting a release. vaultis holds people's estate data behind two
  passwords with no server and no recovery path, so a defect here is not a bug
  report — it is somebody's inheritance. Reports findings by severity with a
  reproduction for each, and refuses to call a run clean when checks were
  skipped."
---

# Audit

vaultis is an offline, two-password encrypted estate vault. There is no server, no
telemetry, no password reset, and no second chance: a vault that will not open is data
that is gone, and a vault that opens for the wrong person is an estate handed to a
stranger. Audit it accordingly.

Nine prior audit rounds live in `docs/AUDIT_*.md`, the standing security posture in
`docs/HARDENING.md`, and the threat model in `docs/THREAT_MODEL.md`. **Read the threat
model and the two most recent audit docs before starting.** They record what has already
been chased and *disproved*, which is the difference between a new round and a re-run of
the last one.

## Ground rules

1. **Never report a finding you have not reproduced.** Every finding needs concrete
   inputs and an observed wrong result — a failing test, a panic, a diff between what
   the code does and what it must do. "This looks risky" is a lead, not a finding.
2. **Say what you did not run.** A missing nightly toolchain or a skipped fuzz budget
   makes the run partial. Report it as partial. Never imply coverage you did not get.
3. **Chased-and-refuted leads are output too.** Record them so the next round does not
   re-chase them. That is the house convention (see any `docs/AUDIT_*.md`).
4. **Do not fix while hunting.** Collect findings first; fixing mid-sweep loses the
   thread and makes the diff unreviewable. Fix after the sweep, one finding per commit,
   each with the regression test that would have caught it.
5. **A clean result is a real result.** Round 2 of 2026-07-25 found two Low findings and
   said so plainly. Do not manufacture severity to justify the run.

## Phase 1 — Static verification

Run all of these. They are cheap and they gate everything else.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo audit                 # RustSec advisories; config in .cargo/audit.toml
cargo deny check            # bans, licenses, sources; config in deny.toml
```

`cargo-audit` and `cargo-deny` may need `cargo install --locked` first. If either tool
cannot be installed, say so — do not silently drop supply-chain coverage.

Then read for the classes that matter here, with `docs/THREAT_MODEL.md` open:

- **Crypto misuse** — nonce reuse, a salt reused across KDF invocations, AAD that does
  not bind everything it must (header, vault id, partition), a tag compared
  non-constant-time, KDF params trusted from the file without validation on *both* read
  and write.
- **Key and secret lifetime** — a derived key or password that outlives its
  `Zeroizing`, a `clone()` that copies a secret past its wipe, secrets reaching a log,
  an error message, the clipboard without the armed auto-clear, or a panic payload.
- **Untrusted input** — everything from `vault.pmv`, the manifest, the volume, an
  imported mirror, and a merge source is hostile. Look for panics (indexing, `unwrap`,
  arithmetic overflow, allocation sized from a file field), and for parsers that trust a
  length before checking it against what is actually there.
- **Path handling** — traversal, absolute paths, symlinks, Windows device names,
  spoofing characters (bidi controls, invisible `Cf` forms) in anything that becomes a
  real on-disk filename or a label a user authorizes against. `is_safe_doc_path`,
  `doc_tree_relpath`, `doc_filename`, and `display_safe` are the choke points.
- **Guard asymmetry** — the recurring shape in this codebase: an invariant enforced at
  some call sites but not all. When you find a guard, find *every* path into what it
  guards.
- **Crash safety and TOCTOU** — a power loss must leave the old vault or the new one,
  never a mix. Check the commit ordering and the fsync points against
  `docs/DESIGN.md`, and check the single-writer lock (`single_instance.rs`) for races
  between the check and the open.
- **Read-only really being read-only** — `--write` gates edits. Any write reachable
  without it is a finding.

## Phase 2 — Dynamic verification

The test matrix mirrors `.github/workflows/ci.yml`; run all four, because they compile
different code:

```bash
cargo test --workspace --all-targets                                   # default features
cargo test --workspace --release                                       # overflow-checks on, stripped
cargo test -p vaultis-core -p vaultis --features vaultis/fault-injection
cargo test -p vaultis-core --no-default-features                       # the mobile/musl feature set
```

The feature-gated lock tests are a pair and must both be exercised:

```bash
cargo test -p vaultis-core single_writer                     # feature on (default)
cargo test -p vaultis-core --no-default-features no_op_lock  # feature off
```

**Fuzzing** (needs nightly + `cargo-fuzz`). Six targets live in `fuzz/fuzz_targets/`.
The parser targets are cheap; `merge_from` does real disk I/O and crypto and is the
highest-value one:

```bash
cargo +nightly fuzz build
for t in parse_header parse_frame parse_manifest scan_volume doc_paths; do
  cargo +nightly fuzz run "$t" -- -max_total_time=120 -print_final_stats=1
done
cargo +nightly fuzz run merge_from -- -max_total_time=120 -print_final_stats=1
```

CI runs 30s slices as a smoke test. An audit should give them real budget — minutes at
minimum, longer if the round is targeting the parsers. Report the cumulative execs and
whether any target produced a crash artifact.

**Mutation testing** (`cargo-mutants`) is what catches tests that pass without asserting
anything. It is slow, so scope it to the diff unless the round is a full sweep:

```bash
cargo mutants --in-diff <(git diff origin/main...HEAD)   # a change-scoped round
cargo mutants -p vaultis-core                            # a full-crate round (hours)
```

A surviving mutant is a lead: either a missing assertion, or a genuinely equivalent
mutant. Say which, and write the kill-test for the former.

Optional but valuable when the round touches the crypto or storage core:

```bash
cargo llvm-cov --workspace --all-features --summary-only   # where the tests are not
```

## Phase 3 — Report

Severity: **Critical** (vault opens for the wrong person, or plaintext escapes),
**High** (data loss, or a vault that will not reopen), **Medium** (a guard bypassable
with attacker-controlled input), **Low** (spoofing, information leak with no direct
compromise, robustness).

Write to `docs/AUDIT_<YYYY-MM-DD>.md`, or `_roundN` if the date already exists. Follow
the existing docs' structure:

- A header stating scope, what this round deliberately went after that the last one did
  not, and the threat model it assumes.
- A one-paragraph honest result, including "found less than last round because there is
  less left to find" when that is true.
- **Findings**, each with: the file, what the code does, what it must do, the concrete
  input that separates them, the severity and why, and the fix plus its regression test.
- **Leads chased and refuted**, with the reasoning that killed each one.
- **A reproduction appendix** — the exact commands, so the round can be re-run.

Then report to the user: what ran, what did not and why, the findings by severity, and
the honest bottom line. If nothing was found, say that; do not pad.

## When the audit is pre-release

Add the packaging surface, which the test suite does not cover:

- The release zip's contents and layout, its SHA-256, and that both exes are the
  expected subsystem (console vs GUI) with no unexpected DLL imports.
- `get_vaultis.bat`'s download path: it must refuse an asset with no `.sha256`, and must
  not install one whose hash does not match.
- The Windows Sandbox kit in `scripts/windows-setup-tests/` for the real install.
