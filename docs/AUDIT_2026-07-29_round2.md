# Deep Audit — 2026-07-29 (round 2)

A change-scoped static + dynamic audit of the two commits that landed after **Release 0.2.1**
(`56a09ba`), run on top of the full regression matrix:

* `c261c83` — the binaries learned their own version: `env!("CARGO_PKG_VERSION")` prepended to the
  console `HELP`, a new `--version` / `-V` flag, and the number shown in the GUI manual's header.
* `270106e` — the README's "Command-line options (advanced)" section, which still documented the
  pre-format-v4 CLI, reconciled against `vaultis --help`.

**Threat model:** unchanged from [`THREAT_MODEL.md`](THREAT_MODEL.md) and
[`AUDIT_2026-07-29.md`](AUDIT_2026-07-29.md) — a standalone, OFFLINE two-password encrypted estate
vault. In scope: theft of / tampering with the on-disk vault files, a crafted vault, a hostile merge
source, malicious paths/filenames/record content, a local multi-user filesystem, and secrets
lingering in memory / on disk / in the terminal. Out of scope: network attacks, and an attacker
already running as the user with the vault unlocked in RAM.

**What this round deliberately went after that round 1 did not.** Round 1 swept the ~40 commits
between 0.2.0 and 0.2.1 and closed M-1/L-1/L-2. The only new surface since is two commits — but one
of them adds a **new branch to argument parsing**, and argument parsing is where this project's
recurring *guard-asymmetry* shape lives (round 1's L-1 was a read/write asymmetry; `launch.rs`
carries an explicit written promise that the console and windowed binaries resolve arguments
**identically**). The round was therefore aimed at three questions:

1. Does the new `--version` branch change dispatch or positional handling for anything else?
2. Is the two-binary argument-resolution guarantee still true after adding flags to only one of them?
3. Did rewriting the 61-line `HELP` string literal as a `concat!` silently change the text?

**Result:** no Critical, no High, no Medium. **Three Low findings**, none in the cryptographic core,
the on-disk format, the parsers, the crash-safety machinery or the read-only gate — all of which came
through the full matrix clean alongside **117,494,150 fuzz executions with zero crashes** and **no
surviving mutant** on the diff.

The answer to question 3 is *no* (proved by diffing the old literal against the new binary's output),
and the answer to question 1 is *no*. The answer to question 2 is **L-1**, and it is the familiar
shape: the console binary intercepts `--help`/`-h`/`--version`/`-V` before anything else, the
windowed binary treats those same four tokens as a **vault directory name**, and `launch.rs:319-324`
states in a comment that the two resolve identically. Two of those four tokens diverged before this
round; `c261c83` widened the gap to four rather than creating it. **L-2** and **L-3** are
documentation and process drift found along the way (an internal CLI table that no longer matches
`main.rs`, and a fuzz-workspace lockfile that goes stale on every release bump and dirties the tree
the moment an audit runs the fuzzers — which then blocks `scripts/release.sh`).

Found in the core: nothing — as in the two rounds before this one, and consistent with the shape of
the change, since a printed string cannot reach the crypto. This report is short because the diff is short;
it is not short because the checks were skipped, and *Not run* says exactly which ones were.

---

## Findings

### L-1 — `vaultis-gui` treats `--version` and `--help` as a vault directory name (Low, robustness)

`crates/vaultis-desktop/src/launch.rs:317-336` (`resolve_interactive`) vs
`crates/vaultis-desktop/src/main.rs:276-304`

**What the code does.** The console binary intercepts four tokens anywhere in `argv`, prints, and
exits 0:

```rust
if args.iter().any(|a| a == "--help" || a == "-h")      { println!("{HELP}");         return SUCCESS; }
if args.iter().any(|a| a == "--version" || a == "-V")   { println!("{VERSION_LINE}"); return SUCCESS; }
```

The windowed binary's resolver filters **exactly two**:

```rust
let positionals: Vec<&String> = args.iter().filter(|a| !matches!(a.as_str(), "--write" | "--tui")).collect();
```

Everything else becomes the positional vault DIR, and `vault_file(dir)` unconditionally joins
`vault.pmv` onto it.

**What it must do.** `launch.rs:319-324` states the requirement in its own words — the flag set is
matched exactly *"so `vaultis DIR` and `vaultis-gui DIR` could [not] open DIFFERENT vaults. Matching
the exact set keeps both binaries' resolution identical (the module's stated guarantee)."* Four
tokens now break that guarantee.

**The concrete input that separates them.**

```
$ ./target/debug/vaultis --version
vaultis 0.2.1

$ ./target/debug/vaultis-gui --version /tmp/somevault
vaultis error: too many arguments: expected at most one vault DIR, got 2:
  ["--version", "/tmp/somevault"]. Usage: vaultis-gui [DIR] [--write]

$ ./target/debug/vaultis-gui --version        # resolves --version/vault.pmv and launches
vaultis error: GUI error: winit EventLoopError: … neither WAYLAND_DISPLAY nor … DISPLAY is set.
```

The third case is the one that matters: argument resolution **succeeded** and handed
`--version/vault.pmv` to `gui::run` (only the headless test host stopped the window from opening).

**Severity: Low, and why.** No write happens without `--write`, no plaintext escapes, and no vault
opens for the wrong person — the resolved path simply does not exist, so the start page opens rooted
at a folder named `--version`. What makes it worth fixing rather than noting: on Windows,
`vaultis-gui` is a **GUI-subsystem** binary with no console, so the `eprintln!` that reports the
arity error goes nowhere. A Windows user asked "what version are you on?", running the binary their
Desktop shortcut points at, gets **a window on a nonexistent vault and no message at all**.

**Fix (applied, `edc7b40`).** `VERSION_LINE` moved into `launch.rs`, so **one** constant answers
`--version` in both binaries. `resolve_interactive` now returns
`Interactive::{Open, Version, Help}` instead of a bare `(path, writable)` tuple, and answers the
four tokens before any positional handling — the type makes "treat a print-and-exit request as a
directory" unrepresentable rather than merely fixed. `vaultis-gui` prints `VERSION_LINE` or a short
`GUI_USAGE` that points at the console binary for the subcommands. On Windows that print is still
swallowed by the GUI subsystem — attaching to the parent console needs `unsafe`, which this crate
forbids — but exiting 0 in silence beats opening a window on a bogus vault.

**Regression tests.** `launch::tests::resolve_interactive_answers_help_and_version_instead_of_opening_them`
(all four tokens, from any position, `--help` outranking `--version` as in `main.rs`, and the
near-miss `--versions` still resolving to a *directory*), plus
`cli::the_windowed_binary_answers_version_and_help_without_opening_a_vault`, which runs the real
`vaultis-gui` binary and pins its stdout to the same line the console binary prints. Both were
confirmed to **fail** with the two early returns removed and the rest of the change in place — see
the table under *Verification*.

### L-2 — `IMPLEMENTATION.md` §7's CLI table no longer matches `main.rs` (Low, documentation)

`docs/IMPLEMENTATION.md:573-586`

**What the doc says.** Ten rows: `[DIR]`, `--write`, `--tui`, `decrypt`, `manifest`, `extract`,
`backup`, `export-tree`, `import-tree`, `compact`.

**What `main.rs` dispatches.** Those ten plus `update-from` (`main.rs:439`) and the throwaway
`migrate-doc-paths` — and, since `c261c83`, `--help` and `--version` as first-class flags.

**Why it counts.** This is the internal reference a future round reads to answer "what is the CLI
surface?" — the same question this round had to answer from source because the table could not be
trusted. `update-from` has been missing since it landed; `--version` is this round's addition.
Nothing user-facing is wrong (the README and `--help` agree with each other and with the code, as
re-verified below), so: Low.

**Fix (applied, `e4104b7`).** The four missing rows added, plus a sentence recording that both
binaries now answer `--help`/`--version` from the shared constant. **Regression test:** none that is
worth its weight — a test asserting a prose table matches a dispatch `match` would pin formatting,
not behavior. Recorded here instead, which is the honest option.

### L-3 — `fuzz/Cargo.lock` goes stale at every release, and dirties the tree the moment anyone fuzzes (Low, process)

`fuzz/Cargo.lock:541`

**What happens.** `fuzz/` is a **detached** workspace with its own lockfile that records the path
dependency's version number. `Release 0.2.0` (`ef088cc`) and `Release 0.2.1` (`56a09ba`) both bumped
the three `Cargo.toml`s and the root `Cargo.lock` — and neither touched `fuzz/Cargo.lock`, which
still said `vaultis-core 0.2.0` at the start of this round. The first `cargo +nightly fuzz build`
rewrites it:

```
-name = "vaultis-core"
-version = "0.2.0"
+version = "0.2.1"
```

**Why it counts.** `scripts/release.sh:146` refuses to cut a release while
`git status --porcelain` is non-empty. So the sequence this project actually follows — *audit
(which fuzzes) → fix → release* — reliably arrives at the release script with a tree dirtied by a
file nobody edited, and the operator has to decide mid-release whether that diff is theirs. The
resolution is not itself risky (path dependencies resolve from disk, so the stale number never
selected different code, and nothing was mis-fuzzed) which is exactly why it is Low.

**Fix (applied, `a00337d`).** The refreshed lockfile is committed, and `scripts/release.sh` now
warns when `fuzz/Cargo.lock` disagrees with the crate version — beside the existing lockstep warning
for the sibling crates, and equally non-fatal, since nothing in the release reads it. **Regression
test:** none; the check itself is the guard, and the next release either carries the file or prints
the warning.

---

## Leads chased and refuted (so the next round need not re-chase them)

* **The `concat!` rewrite silently changing `HELP`.** The literal's delimiters were rewritten
  (`"\ …"` → `concat!("vaultis ", env!(…), " …")`), which is exactly the edit that quietly drops a
  line. Refuted mechanically: the pre-change literal was extracted from `56a09ba` and diffed against
  the new binary's actual `--help` output. The **only** differences are the two intended ones — the
  version on line 1 and the new `--version` row. 61 lines, byte-identical otherwise.
* **`--version` shadowing a legitimate flag value.** The scan runs before `extract_compact_flags`,
  so `vaultis compact DIR --backup --version` prints the version instead of using `--version` as a
  backup destination. Contrived (the destination would have to be a directory literally named
  `--version`), it mirrors the `--help` precedent that has always behaved this way, and no security
  property depends on it. Not a defect — recorded because it *looks* like one.
* **`-V` / `-h` shadowing a vault directory whose name starts with `-`.** Real, but it is the other
  half of L-1 rather than a separate finding: `launch.rs`'s "exact flag set" rule exists precisely so
  a `-`-prefixed *directory* stays openable, and the console binary's interception is the thing that
  breaks the symmetry. Fixing L-1 makes both binaries refuse those four tokens as a DIR, together.
* **Version drift between `vaultis-core` and `vaultis-desktop`.** Both are 0.2.1. If they ever
  diverge, the help prints the **desktop** crate's number — which is the correct one, because
  `scripts/release.sh` checks the release tag against exactly that file (`VERSION_CRATE`), so the
  printed number is the one that names the published build.
* **The new GUI label changing the help browser's layout.** Measured, not eyeballed: the real
  `gui_help::ui` was rendered head-less through `egui_kittest::Harness` at 320 / 480 / 900 pt pane
  widths, with and without the version label. Drawn width was **identical to the hundredth of a
  point in all three** (`397.75 / 480 / 900` either way) — the label costs zero layout. (The 320 pt
  overflow is the left nav panel's 238 pt default and predates this change.)
* **The version label leaking anything.** It is the public release number, rendered in the help
  header with no vault state in scope; `HelpContext` carries only paths already shown on that page.
* **The README's new claims, verified against the code rather than assumed.** All six subcommand
  arities match `main.rs`'s own usage strings verbatim (`manifest`/`extract`/`backup`/`export-tree`
  `[DIR]`-first, `import-tree`/`update-from` SRC-first); `--vol` appears nowhere in the tree; and
  the removed `decrypt ./vault.pmv` example genuinely fails today —
  `vaultis error: no vault found at ./vault.pmv/vault.pmv` — which is what made it worth correcting.
* **Other argv entry points.** `env::args` appears in exactly three places:
  `main.rs`, `bin/vaultis-gui.rs` (L-1), and `examples/seed_sample_vault.rs`, which takes three
  fixed positionals, has no flags, and is a dev-only example never shipped in the release zip.

---

## Not run

* **The Windows packaging surface** — the release zip's contents and SHA-256, both executables'
  subsystem and DLL imports, and the Windows Sandbox kit. No Windows host and no local release
  artifact, exactly as in round 1. This round changed nothing in `packaging/`,
  `.github/workflows/release.yml`, `scripts/release.sh` or `get_vaultis.bat`, so round 1's reading of
  those files still stands — but reading is not verifying, and this is not a pre-release round.
* **The GUI rendered on a real display.** This host is headless (`DISPLAY` and `WAYLAND_DISPLAY`
  both unset), so the version label was never seen by a human eye. It *was* measured through
  `egui_kittest` (above), which is stronger evidence about layout than a screenshot and no evidence
  at all about aesthetics.
* **`cargo fmt --all -- --check`** — not run as a gate. CI states the policy in its own header:
  *"The project is deliberately hand-formatted, so there is NO `cargo fmt` check."* Recorded again so
  a future round does not mistake the 28-file diff for a regression.
* **A full-crate mutation run.** Scoped to the diff, per the skill's own guidance for a
  change-scoped round. `cargo mutants -p vaultis-core` (hours) was not run; round 1's scoped run on
  `launch.rs` is the most recent full-file evidence there.

---

## Verification

Every command in the reproduction appendix was run on this tree.

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo audit` | exit 0, **no advisory reported** — 599 crate dependencies scanned against 1172 advisories |
| `cargo deny check` | advisories / bans / licenses / sources **all ok** |
| `cargo test --workspace --all-targets` (pre-fix baseline) | **674 passed, 0 failed**, 1 ignored |
| `cargo test --workspace --all-targets` (**after all three fixes**) | **676 passed, 0 failed**, 1 ignored |
| `cargo test --workspace --release` (overflow-checks on, stripped) | pass — 676 |
| `cargo test -p vaultis-core -p vaultis --features vaultis/fault-injection` | pass — 679 (incl. the 24 crash / full-disk recovery tests) |
| `cargo test -p vaultis-core --no-default-features` (mobile / static-musl set) | pass — 392 |
| `cargo test -p vaultis-core single_writer` (feature ON) | pass |
| `cargo test -p vaultis-core --no-default-features no_op_lock` (feature OFF) | pass |
| `cargo check -p vaultis --no-default-features` (terminal-only / musl build) | clean |

The counts reconcile exactly: round 1 ended at **673**; `c261c83` added
`version_flag_prints_the_crate_version_and_exits_zero` (**674**); the L-1 fix added its two
regression tests (**676**).

Each L-1 regression test was confirmed to **fail on the pre-fix behaviour**, not merely to pass
after it — a test that passes both ways proves nothing. Because the tests are written against the
new `Interactive` type, the check was run by removing the two early returns and keeping the rest of
the change:

| Test | Fails without the fix with |
|---|---|
| `resolve_interactive_answers_help_and_version_instead_of_opening_them` | *"--help must be answered, not treated as a vault DIR"* (`launch.rs:448`) |
| `the_windowed_binary_answers_version_and_help_without_opening_a_vault` | *"vaultis-gui --version must exit 0"* (`tests/cli.rs:71`) — the pre-fix binary opened the vault instead, and exited non-zero only because this host is headless |

That last caveat is worth stating: on a machine *with* a display, the pre-fix `vaultis-gui
--version` would have opened a window and the CLI test would hang rather than fail. The unit test is
the display-independent half of the pair, which is why both exist.

**Fuzzing** (nightly + `cargo-fuzz`, libFuzzer, 120 s per target — a regression budget, not round 1's
deep 126 M-execution run, because no parser changed):

| Target | Executions | Crashes |
|---|---:|---|
| `parse_header` | 64,446,577 | 0 |
| `parse_frame` | 31,679,646 | 0 |
| `parse_manifest` | 11,003,965 | 0 |
| `scan_volume` | 8,211,230 | 0 |
| `doc_paths` | 2,143,011 | 0 |
| `merge_from` (real crypto + disk I/O per iteration) | 9,721 | 0 |
| **total** | **117,494,150** | **0** |

`fuzz/artifacts/` was empty before the run and empty after it — no target produced a crash artifact.

Round 1 gave the parsers 180 s each and `merge_from` 300 s; this round gave every target 120 s,
because no parser changed and the purpose here is regression rather than discovery. The execution
counts are nonetheless in the same order as round 1's (117.5 M vs 126.3 M in roughly half the wall
time — this host was quieter), and `merge_from`'s rate is effectively identical (80/s here, 74/s
then), which is the cheap check that the targets are still reaching real parser code rather than
failing early.

**Mutation testing** — `cargo mutants --in-diff`, scoped to the round's diff:

```
Found 6 mutants to test
ok       Unmutated baseline in 125s build + 39s test
6 mutants tested in 6m: 4 caught, 2 unviable
```

| Mutant | Outcome |
|---|---|
| `main.rs:287` replace `main -> ExitCode` with `Default::default()` | **caught** |
| `main.rs:300` replace `\|\|` with `&&` | **caught** |
| `main.rs:300` replace `==` with `!=` (`--version` arm) | **caught** |
| `main.rs:300` replace `==` with `!=` (`-V` arm) | **caught** |
| `gui_help.rs:1820` replace `ui -> bool` with `true` / `false` | unviable (does not compile) |

**No mutant survived.** The two `==`→`!=` kills and the `||`→`&&` kill are the ones worth naming:
they are only possible because `version_flag_prints_the_crate_version_and_exits_zero` exercises
**both** spellings and pins the exact output, so no single-token change to that condition can pass.
The two `gui_help::ui` mutants are *unviable* rather than missed — replacing the whole function body
with a bare `true`/`false` leaves its parameters unused and fails to compile, so they say nothing
about test coverage in either direction.

---

## Reproduction appendix

```bash
# Scope
git log --oneline 56a09ba..HEAD

# Phase 1 — static
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo audit
cargo deny check
cargo check -p vaultis --no-default-features

# Phase 2 — dynamic
cargo test --workspace --all-targets
cargo test --workspace --release
cargo test -p vaultis-core -p vaultis --features vaultis/fault-injection
cargo test -p vaultis-core --no-default-features
cargo test -p vaultis-core single_writer
cargo test -p vaultis-core --no-default-features no_op_lock

cargo +nightly fuzz build
for t in parse_header parse_frame parse_manifest scan_volume doc_paths merge_from; do
  cargo +nightly fuzz run "$t" -- -max_total_time=120 -print_final_stats=1
done

git diff 56a09ba..HEAD > /tmp/round.diff
cargo mutants --in-diff /tmp/round.diff

# L-1 — the divergence, both halves
./target/debug/vaultis --version                        # -> vaultis 0.2.1
./target/debug/vaultis-gui --version /tmp/somevault     # -> "too many arguments … [\"--version\", …]"
./target/debug/vaultis-gui --version                    # -> resolves --version/vault.pmv, launches

# The HELP text was not altered by the concat! rewrite
git show 56a09ba:crates/vaultis-desktop/src/main.rs \
  | sed -n '/^const HELP/,/no external configuration files\.";$/p' \
  | sed '1d; $s/";$//' > /tmp/old_help.txt
./target/debug/vaultis --help > /tmp/new_help.txt
diff /tmp/old_help.txt /tmp/new_help.txt   # only line 1 (version) and the new --version row

# The README example that used to be wrong
./target/debug/vaultis decrypt ./vault.pmv   # -> no vault found at ./vault.pmv/vault.pmv

# L-3
git status --porcelain     # fuzz/Cargo.lock is modified after `cargo fuzz build`
```
