# Deep Audit — 2026-07-29

The first run of the new [`deep-audit`](../.claude/skills/deep-audit/SKILL.md) skill. Where
the ordinary rounds ask *"what did these commits break?"*, this one asks **"are the
standing claims actually true?"** — the properties the suite asserts by construction
(`ZeroizeOnDrop` derives, "the parsers never panic", "the build is reproducible from
source") but has never *observed*.

**Threat model:** unchanged from [`THREAT_MODEL.md`](THREAT_MODEL.md). Note one boundary
that matters throughout this report: an attacker already running code as the user, with
the vault unlocked in RAM, is **out of scope**. Everything below about process memory is
therefore about *secret lifetime* — how long plaintext outlives its owner — not about a
new way in.

**Result:** **one Low finding**, and five standing claims verified for the first time. The
finding (**D-1**) is that the master password survives the Argon2id derivation in freed
heap, inside the `argon2` crate's initial-hash buffer. It is a new instance of a class
`HARDENING.md` already documents as F-1, it is only observable with an allocator that does
not recycle, and the obvious fix does not work — all three stated plainly below.

The more consequential outcome may be a **retired assumption**. `HARDENING.md:258` records
`Key::drop` as an accepted mutation-testing survivor on the grounds that *"zeroize-on-drop
can't be observed by a safe test"*. That is no longer true: it cannot be observed from
*inside* the process holding the secret, but it can be observed from the parent of a child
that held it, in safe Rust, in about 200 lines. `tests/memory_residue.rs` does exactly
that, and the technique generalises to any secret with a claimed lifetime.

---

## Findings

### D-1 — the master password survives the KDF in freed heap (Low, secret lifetime)

`crates/vaultis-core/src/crypto.rs:362` (`derive_key`) → `argon2 0.5.3`

**What was measured.** `tests/memory_residue.rs` runs a child process through staged
scenarios and counts occurrences of two run-unique sentinels — one entering as the *master
password*, one as an *account password* — in the child's anonymous memory, read from the
parent via `/proc/<pid>/mem`. Under AddressSanitizer:

| stage | master-password copies | record-field copies |
|---|---:|---:|
| `none` — sentinels derived and wiped, no vault (control) | 0 | 0 |
| `hold` — vault open, secrets legitimately live (control) | 4 | 2 |
| **`kdf` — `derive_key_chained` alone, then dropped** | **3** | 0 |
| `create` — `OpenVault::create`, dropped | 3 | 0 |
| `record` — + an `Account` holding the second sentinel, dropped | 3 | 0 |
| `drop` — + `save()`, everything dropped | 3 | 0 |

Both controls hold, which is what makes the rest meaningful: the harness does not leak its
own needle, and the scanner demonstrably finds a live secret.

**The attribution is unambiguous.** The `kdf` stage — nothing but
`derive_key_chained(pw, …)` and a drop — already shows all 3 copies, and no later stage adds
any. The **record path is clean at every stage** (0), so `Account`'s `ZeroizeOnDrop` does
what it claims. This is the KDF and nothing else.

**Which buffer.** Dumping the bytes around a hit identifies the owner exactly:

```
01000000  lanes = 1          20000000  outlen = 32
00010000  m_cost = 256       01000000  t_cost = 1
13000000  version = 0x13     02000000  type = Argon2id
2e000000  pwd_len = 46       <the password>
10000000  salt_len = 16      07 × 16   <the salt>
```

That layout is Argon2's **H0 initial-hash pre-image** — the buffer the crate assembles to
feed Blake2b. It belongs to `argon2`, not to vaultis: vaultis passes the password as a
borrowed `&[u8]` and wipes its own copies correctly.

**Severity: Low, and why.** Reading it requires local process-memory access, which the
threat model already places out of scope, and `HARDENING.md:70` already accepts core-dump
exposure of the *live* key as residual. What this adds is that the *master password* — not
just the derived key — outlives its use, in a buffer nothing wipes. It is the same class as
F-1 ("plaintext fragments across freed heap that may persist until overwritten"), now on
the KDF path.

**Visible only with a non-recycling allocator, and that is not a caveat that dismisses it.**
In a normal build the same test reports **0** at every dropped stage — later allocations
overwrite the buffer. Under ASan (quarantine) the copies persist and are counted; the
**ThreadSanitizer** run reproduces the failure independently, so this is not one tool's
artifact. The honest reading: the copies are genuinely written and genuinely freed
un-wiped; how long they survive is left to allocator luck, which is exactly what the
project's own F-1 write-up calls "a gratuitous secret-lifetime extension".

**Attempted fix that does NOT work.** `argon2`'s `zeroize` feature is not on by default,
which looked like the answer. Enabling it left the count at **3** — it covers the `Block`
memory, not the H0 pre-image. The Cargo.toml change was reverted rather than shipped with
a comment claiming a result the measurement had disproved. A real fix has to come from
upstream (wipe the pre-image), or from vaultis pre-hashing the password itself so that what
reaches `argon2` is already a non-secret digest. **Not fixed in this round**; recorded with
its reproduction so the decision can be made deliberately.

**Regression test.** `crates/vaultis-core/tests/memory_residue.rs` — already committed, and
already failing under ASan/TSan, which is the point. It passes in a normal build.

---

## Claims verified (the point of the round)

| Claim | How it was verified | Strength |
|---|---|---|
| Record secrets are wiped on drop | memory-residue test, record-field sentinel | **observed**: 0 copies at every dropped stage, with a live control finding 2 |
| `Header::parse` never panics on hostile bytes | Kani/CBMC | **proved** for *every* input ≤ 32 bytes (27 s) |
| The build is reproducible from source (`DESIGN.md:1192`) | clean rebuild + rebuild at a different path | **observed**: SHA-256 identical for both binaries, both ways |
| No data races | ThreadSanitizer over the core suite, `-Zbuild-std` | **observed**: 388 tests, 0 races |
| No leaks / memory errors | ASan + LSan over the core suite, `-Zbuild-std` | **observed**: 388 tests clean (except D-1's own test) |
| The crate contains no `unsafe` | `cargo geiger` | **observed**: `vaultis-core` scores `0/0` across every category |

Binary hardening (both release binaries): **PIE**, **full RELRO** (`GNU_RELRO` +
`BIND_NOW`), **non-executable stack**, stripped. Stack canaries are absent — Rust does not
emit them by default and the symbol is stripped — which is inconclusive rather than a
finding in a crate that forbids `unsafe`.

Dependency hygiene: `cargo machete` finds **no unused dependencies**. `cargo geiger`
concentrates the tree's `unsafe` in `memchr` (1981 expressions, via `serde_json`), `libc`,
`region` and `zmij`; `chacha20poly1305` scores **0/0** and `argon2` 2/2. Noted in passing:
`getrandom` appears twice in the tree (0.2.17 and 0.4.2).

---

## Not run, or inconclusive

* **Three of the four Kani harnesses did not verify**, and this is *not* a statement about
  the code. `doc_slug` exhausts its unwind bound (`Not unwinding loop … iteration N`) at
  4-byte inputs/`unwind(6)`, at 3 arbitrary chars/`unwind(8)`, and still at `unwind(16)`;
  `doc_filename` and `doc_upload_dir` did not terminate in 30 minutes each. The cause is
  understood: unwind bounds count **bytes**, an arbitrary `char` is up to 4 bytes, and
  feeding bytes through `String::from_utf8_lossy` (as the fuzz targets do) drags
  `Utf8Chunks`'s own loops into the solver. A verification FAILURE from an exhausted bound
  is neither a pass nor a counterexample, and is reported here as neither. The properties
  remain covered by the fuzzers at 117 M samples — sampled, not proved.
* **Miri** — not run. All three crates are `#![forbid(unsafe_code)]`, so it could only
  reach dependency UB, and the crypto crates' SIMD intrinsics largely will not execute
  under it. Low expected yield; stated rather than quietly skipped.
* **loom** — not attempted. The single-writer lock is `flock`-based, i.e. kernel state
  rather than the atomics/`Mutex` interleavings loom models, so it is the wrong instrument
  for the thing most worth checking. TSan over the real suite was run instead.
* **The Windows packaging surface** — no Windows host, as in every prior round.
* **`cargo vet`** — not run; it needs a curated trust store, which is a project decision
  rather than an audit step.

---

## Reproduction appendix

```bash
# Memory residue (the finding). Passes in a normal build; FAILS under ASan, which is the point.
cargo test -p vaultis-core --test memory_residue -- --nocapture
RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu --test memory_residue -- --nocapture

# Sanitizers over the whole core suite. NOTE the separate target dir for the second one:
# reusing the first's artifacts fails with "mixing -Zsanitizer will cause an ABI mismatch".
RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu
CARGO_TARGET_DIR=target-tsan RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -p vaultis-core \
  -Zbuild-std --target x86_64-unknown-linux-gnu

# Bounded proofs. `env -u CARGO -u RUSTUP_TOOLCHAIN` is required; never pipe through `head`.
cd crates/vaultis-core
env -u CARGO -u RUSTUP_TOOLCHAIN cargo kani --harness header_parse_never_panics_on_short_hostile_input

# Reproducible build
cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
cargo clean --release && cargo build --release -p vaultis --bins && sha256sum target/release/vaultis{,-gui}
git archive HEAD | tar -x -C /tmp/alt && (cd /tmp/alt && cargo build --release -p vaultis --bins \
  && sha256sum target/release/vaultis{,-gui})

# Binary hardening
readelf -hlWd target/release/vaultis | grep -E "Type:|GNU_RELRO|GNU_STACK|BIND_NOW"

# Dependency trust (geiger needs a real package, not the virtual workspace root)
cargo machete
cd crates/vaultis-core && cargo geiger
```
