# Deep Audit — 2026-10-10

The second run of the [`deep-audit`](../.claude/skills/deep-audit/SKILL.md) skill, on the same
uncommitted tree as [`AUDIT_2026-10-10.md`](AUDIT_2026-10-10.md): a 10,000-line module split
plus the removal of the terminal UI, on top of `28dfaea`. That round asks *"what did the change
break?"*. This one asks whether the **standing claims still hold** now that every parser,
every secret helper and the vault's write path live in different files.

**Threat model:** unchanged from [`THREAT_MODEL.md`](THREAT_MODEL.md). As in July, an attacker
already running as the user with the vault unlocked is out of scope, so everything below about
process memory is about *secret lifetime*, not a new way in.

**Result: no new finding in the product.** Every standing claim that was verified in July was
re-verified at the same strength or better, and the build is still byte-reproducible at a
different path. The round's one defect was in the **instrument**: the memory-residue harness
could no longer observe anything under the sanitizers (filed and fixed as **A-2** in the
companion report). It is recorded here because a residue claim measured by a broken harness is
worth nothing, and because its negative control is what exposed it.

---

## Claims verified

| Claim | How it was verified this round | Strength |
|---|---|---|
| Record secrets are wiped on drop | `memory_residue.rs`, record-field sentinel, under a normal allocator, ASan **and** TSan | **observed**: 0 copies at every dropped stage under all three, with the live control finding 2 under all three |
| No stage after the KDF retains the master password | same test | **observed**: dropped-stage counts constant at the KDF's own value (0 normal, 3 ASan, 0 TSan) |
| `Header::parse` never panics on hostile bytes | Kani/CBMC, `header_parse_never_panics_on_short_hostile_input` | **proved** for every input ≤ the harness bound (`unwind(4)`): `VERIFICATION:- SUCCESSFUL`, 0 of 485 checks failed, 11.7 s. Re-proved against the moved `vault/vault_file.rs` layout |
| The parsers never panic on hostile input | 6 fuzz targets, 150 s each | **sampled**: 134,924,579 executions, 0 crash artifacts |
| The build is reproducible from source | release build; clean rebuild at the same path; build of a copy at a **different path** | **observed**: SHA-256 identical across all three, for both binaries |
| No data races | ThreadSanitizer, core suite, `-Zbuild-std` | **observed**: 409 unit + 13 integration tests, 0 race reports |
| No memory errors / leaks | ASan + LSan, core suite, `-Zbuild-std` | **observed**: the same 422 tests, 0 reports |
| The crate contains no `unsafe` | `cargo geiger` | **observed**: `vaultis-core 0/0` in every category, unchanged by the split |
| No unused dependencies | `cargo machete` | **observed**: none (this also confirms the new direct `crossterm` edge is used) |
| The public API of the audited core did not change | `cargo public-api` vs `28dfaea` | **observed**: the same 742 items in `vaultis-core`; `vaultis-ffi` identical |

**Binary hardening** (both release binaries, unchanged from July): **PIE**, **full RELRO**
(`GNU_RELRO` + `BIND_NOW`), **non-executable stack** (`GNU_STACK RW`), stripped. Stack
canaries are absent, which is Rust's default and inconclusive rather than a finding.

**Reproducible-build hashes:**

| binary | SHA-256 (all three builds) |
|---|---|
| `vaultis` | `e051b00dbc4fdae3e34a5269c7958b0384a7df927608fd125b6be61c39c35c5a` |
| `vaultis-gui` | `7b67b4fc450d7207d284ed00162673f069d6575435f0ac6d0ec2dd13f2dc0220` |

---

## The memory-residue measurements

Measured **after** the A-2 fix. Before it, both sanitizer runs failed their `hold` control
with 0 / 0, because the child had been killed mid-scan; see A-2.

| stage | normal | ASan | TSan |
|---|---:|---:|---:|
| `none` (control: must be 0) | 0 / 0 | 0 / 0 | 0 / 0 |
| `hold` (control: must be > 0) | 1 / 2 | **4 / 2** | 1 / 2 |
| `kdf` | 0 / 0 | 3 / 0 | 0 / 0 |
| `create` | 0 / 0 | 3 / 0 | 0 / 0 |
| `record` | 0 / 0 | 3 / 0 | 0 / 0 |
| `drop` | 0 / 0 | 3 / 0 | 0 / 0 |

(master-password / record-field copies. Bytes scanned per mode: ~1.7 MB normal, ~337 MB
ASan, ~205 MB TSan. ASan run 890 s, TSan run 726 s. Both are far past the 60 s point that
used to kill the child. Default sanitizer options throughout.)

**D-1 is unchanged under ASan.** It still shows exactly 3 master-password copies, attributed
wholly to the `kdf` stage, matching July. It stays an accepted residual.

**One difference from July, stated rather than smoothed over.** In July, TSan *also* showed
D-1's 3 copies. This round TSan shows 0 at the `kdf` stage. Nothing on the KDF path changed:
`crypto.rs` is untouched and `argon2` is the same 0.5.3. The likely cause is that this
nightly's TSan allocator recycles freed memory sooner, so D-1 is observable under one
instrument this round instead of two. That **weakens the evidence for D-1's presence by one
instrument; it is not evidence of a fix**, and `HARDENING.md`'s D-1 entry stays as written.

---

## Not run, or inconclusive

* **Three of the four Kani harnesses did not terminate** within a 20-minute cap each:
  `doc_slug_invariants…` (1,386 s), `doc_filename_invariants…` (1,201 s) and
  `doc_upload_dir_can_never_traverse…` (1,201 s), all at `unwind(16)` over 3 arbitrary chars,
  with `Not unwinding loop` reports in `any_char_string`, `memcmp` and `str` pattern code. This
  is the same state as July. **Not a pass and not a counterexample.** These properties remain
  **sampled** (the `doc_paths` fuzz target, 2.98 M executions this round), not proved.
* **The TSan doctest build failed** with 48 `mixing -Zsanitizer will cause an ABI mismatch`
  errors: rustdoc compiles doctests without the `-Zsanitizer` flag. No coverage was lost,
  because the core has no runnable doctests (ASan's doctest run executed 0). Recorded so the
  next round does not mistake the error for a test failure. Setting `RUSTDOCFLAGS` to match
  would silence it.
* **R-7 (egui `TextEdit` undo snapshots) is still unobserved.** It is an accepted residual
  that the residue harness could in principle measure, but only with a GUI-driving child
  mode. The harness lives in `vaultis-core`, which has no egui. It is a candidate for a
  desktop-side residue test. Not attempted this round.
* **Miri, loom, `cargo vet`, the Windows PE hardening flags** — not run, for the reasons given
  in July (no `unsafe` to reach; `flock` is the wrong shape for loom; no curated trust store;
  no Windows artifact).

## Reproduction appendix

```bash
# Memory residue (normal, ASan, TSan) — separate target dirs per sanitizer
cargo test -p vaultis-core --test memory_residue -- --nocapture
CARGO_TARGET_DIR=target/audit/asan RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu --test memory_residue -- --nocapture
CARGO_TARGET_DIR=target/audit/tsan RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu --test memory_residue -- --nocapture

# Whole core suite under each sanitizer
CARGO_TARGET_DIR=target/audit/asan RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu --no-fail-fast
CARGO_TARGET_DIR=target/audit/tsan RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu --no-fail-fast

# Kani (from crates/vaultis-core; never pipe through head)
env -u CARGO -u RUSTUP_TOOLCHAIN cargo kani --harness header_parse_never_panics_on_short_hostile_input > kani.log 2>&1

# Reproducible build: same path twice, then a copy at a different path
cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
cargo clean --release && cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
mkdir -p /elsewhere && git ls-files -co --exclude-standard -z | xargs -0 cp --parents -t /elsewhere
(cd /elsewhere && CARGO_TARGET_DIR=/elsewhere-target cargo build --release -p vaultis --bins)

# Hardening and dependency trust
readelf -hlWd target/release/vaultis
cargo machete
cd crates/vaultis-core && cargo geiger
```
