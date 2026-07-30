---
name: deep-audit
description:
  "The heavyweight verification pass for vaultis, beyond what `audit` runs: bounded
  proofs (Kani/CBMC) of the untrusted-input parsers, an out-of-process memory-residue
  test that observes whether dropped secrets actually leave RAM, whole-suite
  AddressSanitizer/LeakSanitizer and ThreadSanitizer, reproducible-build and binary
  hardening verification, and dependency-trust checks (geiger, machete) that go past
  the advisory database. Activates on 'deep audit', 'deep-audit', 'prove', 'formal
  verification', 'memory residue', 'zeroization check', 'sanitizers', 'reproducible
  build', 'supply chain', or before a release that matters. Slower and heavier than
  `audit` — hours, not minutes — and aimed at the claims the ordinary suite asserts by
  construction but never observes. Reports what it proved, what it merely sampled, and
  what it could not run, and refuses to call any of them the same thing."
---

# Deep audit

`audit` is the per-round sweep: lints, the test matrix, fuzzing, mutation testing, a
manual review. It is the right tool for "what did these commits break".

This is the other question — **are the standing claims actually true?** vaultis asserts
that dropped secrets are wiped, that the parsers cannot panic on hostile input, that the
build is reproducible from source. Those are asserted *by construction*: the types derive
`ZeroizeOnDrop`, the fuzzers found no crash in 100M+ samples, the docs say the build is
reproducible. None of that observes the property. This skill does.

**Read `docs/HARDENING.md` and `docs/THREAT_MODEL.md` first**, plus the most recent
`docs/AUDIT_*.md`. `HARDENING.md` in particular records *accepted residuals* — things
previously judged unobservable or not worth fixing. Several are observable with the tools
below, and re-testing an accepted residual is one of the highest-value things this skill
does. When one turns out to be checkable, say so and update the doc: an accepted residual
that is silently still accepted after it became testable is a stale claim.

## Ground rules

The `audit` skill's rules all apply — never report an unreproduced finding, say what you
did not run, record refuted leads, do not fix while hunting, a clean result is a real
result. Three more matter especially here:

1. **A proof, a sample, and an assertion are three different things.** Say which you
   have. "Kani proves no panic for inputs ≤ 32 bytes" and "the fuzzer found no panic in
   64M samples" are not interchangeable sentences, and neither is "the type derives
   `ZeroizeOnDrop`".
2. **State every bound.** A bounded proof over 4-byte inputs says nothing about 5 bytes.
   The bound is not a footnote; it is half the claim.
3. **A test without a working negative control is not evidence.** This applies hardest
   to the memory-residue test: if the scanner cannot find a secret that is *deliberately
   still live*, then "no secret found after drop" means the scanner is broken, not that
   the code is clean. Every such test here ships with its control, and the control's
   result gets reported alongside the measurement.
4. **A *fix* needs its own control, and this is where you will be fooled.** When a
   mitigation makes the number go to zero, run the identical configuration with the
   mitigation *removed* before believing it. The D-1 stack scrub produced a clean 0 and
   was minutes from being committed as a verified fix; the control showed the 0 came
   entirely from an `ASAN_OPTIONS` change made in the same step, and the scrub did
   nothing. Changing the instrument and the code at once, then attributing the result to
   the code, is the single easiest way to ship security theatre from a real measurement.
5. **The instrument's configuration is part of the finding.** ASan's
   `detect_stack_use_after_return` deliberately keeps dead stack frames alive. Residue that
   only appears with it on is residue a shipped binary does not have. Record the exact
   options every number was taken under, and check whether a finding survives the
   instrument being configured like production.

## 1. Memory residue — do dropped secrets actually leave RAM?

`crates/vaultis-core/tests/memory_residue.rs`. Run it with `--nocapture`; the numbers are
the evidence:

```bash
cargo test -p vaultis-core --test memory_residue -- --nocapture
```

The shape, and why it is built this way: a process cannot search its own memory for its
own secret, because the needle you search with is itself a copy. So the test re-executes
its own test binary as a **child**, has the child build a vault whose master password and
one account password are a run-unique sentinel, and reads the child's `/proc/<pid>/mem`
from the parent. The parent derives the sentinel from a seed it passed in, and the seed is
a strict substring of the sentinel, so the seed sitting in the child's `environ` cannot
produce a false hit.

Three child modes; the first two are controls and both must pass or the third is noise:

| mode | state at photograph time | required result | what a wrong answer means |
|---|---|---|---|
| `none` | sentinels derived, wiped, no vault touched | **0 hits** | the harness leaks its own needle; every number is noise |
| `hold` | vault open, secrets legitimately live | **> 0 hits** | the scanner cannot see memory; a clean `drop` proves nothing |
| `kdf` → `create` → `record` → `drop` | staged, each dropped before the photograph | see below | attribution |

Two sentinels enter by different doors — one as the **master password**, one as a **record
field** — because "3 copies survived" is not actionable until you know which path produced
them. The stages then localise it: in the run that found D-1, the `kdf` stage alone
accounted for every copy and no later stage added one, which pointed straight at the
Argon2 call rather than at serialization.

**What it asserts, and the one thing it deliberately does not.** The record field must be
zero at every stage (that is `ZeroizeOnDrop`, verified on bytes), and no stage after the
KDF may add a master-password copy. It does *not* assert the KDF residue is zero: that is
the accepted residual D-1, and asserting it would leave the test red under every sanitizer
— which is how a test stops being read, and the surest way to hide the next real leak. If
you accept a residual, encode the acceptance in the assertion and say so in the comment;
do not delete the test and do not leave it failing.

Two traps this test already fell into, both worth remembering because both produce a
*passing* test that has checked nothing:

- **`read_exact` over a whole mapping** gives up at the first unreadable page, silently
  skipping most of the heap. Read in chunks and tolerate failure per chunk. Watch the
  reported byte count: if it collapses, the scan stopped looking.
- **Building the sentinel with `format!`** scatters reallocated copies across the heap and
  fails the `none` control. Build into an exactly-sized buffer.

Linux-only (`/proc`), and it needs `ptrace_scope <= 1`, which permits a parent to read a
direct child. Check `/proc/sys/kernel/yama/ptrace_scope` before concluding anything from a
failure to open `/proc/<pid>/mem`.

To extend it: the same harness shape works for any secret with a claimed lifetime — the
derived `Key`, clipboard contents, a decrypted document buffer. Add a mode, not a new test.

## 2. Bounded proofs (Kani / CBMC)

`crates/vaultis-core/src/proofs.rs`, compiled only under `#[cfg(kani)]`.

```bash
cargo install --locked kani-verifier && cargo kani setup     # first time; downloads CBMC
cd crates/vaultis-core
env -u CARGO -u RUSTUP_TOOLCHAIN cargo kani --harness <name>
```

**`env -u CARGO -u RUSTUP_TOOLCHAIN` is required.** Invoked as a cargo subcommand, Kani
inherits `CARGO`/`RUSTUP_TOOLCHAIN` pointing at the outer toolchain and dies with
`Failed to get cargo metadata … No such file or directory`. Unset both.

**Never pipe Kani through `head`.** It emits thousands of `aborting path on assume(false)`
lines while solving; `| head -N` closes the pipe, SIGPIPEs the solver, and the harness dies
early looking like it merely finished. Redirect the whole run to a file and grep the file.

The harnesses mirror the fuzz targets' properties exactly, on purpose — same assertions,
different quantifier. Where a fuzzer says "no counterexample in N samples", these say "no
counterexample exists, for every input up to the bound":

- `doc_slug_invariants_hold_for_every_short_input` — charset, bounds, no edge dash
- `doc_filename_invariants_hold_for_every_short_input` — no separator, control, whitespace
- `doc_upload_dir_can_never_traverse_out_of_its_prefix` — the traversal property
- `header_parse_never_panics_on_short_hostile_input` — the file-format entry point

Bounds are small because CBMC's cost grows sharply with input length. Raising a bound is
the main way to strengthen these; do it one harness at a time and record the new bound and
the wall time. If a harness stops terminating, **say it did not terminate** — a timeout is
not a pass, and reporting it as one is the single worst thing this skill could do.

Three failure modes that all look like "the code is broken" and are not:

- **`VERIFICATION:- FAILED` with `Not unwinding loop … iteration N`** is an exhausted
  unwind bound, *not* a counterexample. Read the failed-checks list before believing a
  failure. A real counterexample names your assertion.
- **Unwind bounds count bytes, not chars.** An arbitrary `char` is up to 4 bytes, so a
  3-char string needs ~12+ unwindings in every byte-level loop. Setting `unwind` from the
  char count exhausts and reports as FAILED.
- **Input encoding decides tractability.** Feeding arbitrary *bytes* through
  `String::from_utf8_lossy` (as the fuzz targets do) pulls `Utf8Chunks`'s loops into the
  solver, and `doc_filename`/`doc_upload_dir` then do not terminate in 30 minutes.
  Arbitrary `char`s are both cheaper and closer to the real call sites, which always hold
  valid UTF-8.

Good candidates to add: `KdfParams::validate` (the read/write bound symmetry), frame
header parsing, and any new helper that turns attacker bytes into a path.

## 3. Sanitizers over the real test suite

The fuzzers already run under ASan, but only on fuzz targets. The test suite is a
different body of code — and LeakSanitizer matters here specifically, because a leaked
buffer is an un-zeroized secret that outlives its owner.

```bash
RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu
RUSTFLAGS="-Zsanitizer=thread"  cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu
```

**Use a separate `CARGO_TARGET_DIR` for the second sanitizer.** Reusing the first one's
artifacts produces `error: mixing -Zsanitizer will cause an ABI mismatch`, *after* printing
a screen of passing test results from the stale build — a result that looks like a clean
run and is worthless. Check for that error before believing the numbers.

`-Zbuild-std` instruments `std` too, which is what makes the result meaningful and also
what makes it slow and disk-hungry. TSan is aimed at `single_instance.rs` and the GUI
threads; `IMPLEMENTATION.md` documents an existing `#[ignore]`d TSan reproducer, so that
area has a history of races and deserves the whole-suite run rather than one test.

**Sanitizers are also the observation instrument for §1.** Both ASan and TSan replace the
allocator with one that does not promptly recycle freed memory. That is what makes
un-zeroized plaintext *visible*: in a normal build the next allocation overwrites it and
the residue test reports a clean 0. A residue result therefore has to say which allocator
it was measured under, and a finding is much stronger when both sanitizers reproduce it
(as D-1 does) — one tool could be an artifact; two agreeing is the allocator behaviour
they share, which is precisely the property being exploited to look.

## 4. Reproducible build

`DESIGN.md` claims the build is reproducible from source. Verify both halves — they fail
for different reasons:

```bash
cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
cargo clean --release && cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
# then build a `git archive` of HEAD at a DIFFERENT path and compare again
```

Same-path rebuild catches timestamps and nondeterministic codegen; the different-path
build catches absolute paths embedded in the binary, which is the one that usually fails
and the one that matters for anyone trying to confirm a release was built from the
published source.

## 5. Binary hardening

No Windows host here, so the ELF side is what can be checked locally:

```bash
readelf -hlWd target/release/vaultis   # PIE (Type: DYN), GNU_RELRO, BIND_NOW, GNU_STACK not RWE
```

Want: PIE yes, full RELRO (`GNU_RELRO` **and** `BIND_NOW`), non-executable stack. Stack
canaries are absent by default in Rust and their symbol is stripped from release builds —
inconclusive rather than a finding, and low-stakes in a crate that forbids `unsafe`. The
PE side (`/DYNAMICBASE`, `/NXCOMPAT`, `/GUARD:CF`) needs a Windows artifact; if there
isn't one, put it under *Not run* rather than inferring it from the workflow.

## 6. Dependency trust, past the advisory database

`cargo audit` and `cargo deny` answer "is anything known-vulnerable / wrongly licensed".
They say nothing about how much unsafe code the tree carries or whether it is all even
used:

```bash
cargo machete                              # dependencies declared but never used
cd crates/vaultis-core && cargo geiger     # unsafe usage per dependency
```

`cargo geiger` refuses to run against the workspace root (*"virtual manifest … requires an
actual package"*) — run it from inside a crate.

For an offline security tool, an unused dependency is pure attack surface with no
benefit — delete it. `geiger`'s counts are for judgement, not a pass/fail gate: the crypto
crates legitimately use `unsafe` for SIMD, and the point is knowing where it is
concentrated.

## 7. What to do with the results

Write to `docs/AUDIT_<YYYY-MM-DD>_deep.md`, following the house structure (scope, honest
one-paragraph result, findings with reproductions, refuted leads, **Not run**,
reproduction appendix). Two additions specific to this skill:

- A **claims table**: each standing claim, how it was verified this round, and the exact
  strength of that verification (proved to bound N / sampled M times / asserted only).
- Any **accepted residual in `HARDENING.md` that this round made testable**, with the
  result and a note to update that doc.

If a check could not run — no Windows host, Kani did not terminate, ptrace blocked — it
goes under *Not run* with the reason. A deep audit that quietly drops its hardest checks
is worse than no deep audit, because it produces confidence nobody earned.
