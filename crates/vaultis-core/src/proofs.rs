//! Bounded proofs (Kani / CBMC) for the untrusted-input helpers.
//!
//! Compiled only under `cargo kani` (`#[cfg(kani)]` from `lib.rs`), so this module costs
//! a normal build nothing.
//!
//! **Why this exists next to the fuzzers.** `fuzz/fuzz_targets/doc_paths.rs` and
//! `parse_header.rs` assert exactly the same properties, and have run them against
//! hundreds of millions of *sampled* inputs. Sampling is evidence, not proof: it says no
//! counterexample was found, never that none exists. Kani converts each of these into a
//! statement with no sampling in it — *for every* input up to the stated bound, the
//! property holds — by handing the whole path condition to a solver.
//!
//! **The bounds are the honest part.** A proof over inputs of at most N bytes is exactly
//! that, and says nothing about N+1. The bounds here are small because CBMC's cost grows
//! sharply with them; they are chosen to cover the structural cases (empty, all-separator,
//! all-alphanumeric, mixed, over-long relative to the internal caps) rather than to
//! impress. The fuzzers remain the long-tail check, and the two are complementary:
//! unbounded-but-sampled, bounded-but-exhaustive.
//!
//! Run with `cargo kani -p vaultis-core`.

use crate::records::{doc_filename, doc_slug, doc_upload_dir};

/// An arbitrary `String` of at most `N` **chars**.
///
/// Built from unconstrained `char`s rather than by lossy-decoding unconstrained bytes.
/// Two reasons, one fidelity and one cost:
///
/// * Fidelity: every real call site takes a `&str` that came from a text field, so the
///   input is always valid UTF-8. `String::from_utf8_lossy` is a *fuzzer* convenience for
///   turning a byte buffer into a `&str`; modelling it here would prove something about
///   the lossy decoder, not about these functions.
/// * Cost: `from_utf8_lossy` drags `Utf8Chunks`'s own loops into the solver. With them,
///   `doc_filename` and `doc_upload_dir` did not terminate in 25 minutes with them.
///
/// The unwind bounds below count BYTES, not chars: an arbitrary `char` is up to 4 bytes,
/// so `N` chars is up to `4N` bytes, and the byte-level loops inside these functions need
/// that many unwindings. An `unwind` set from the char count exhausts and reports as a
/// verification FAILURE that is not a counterexample — see the skill's note on that.
fn any_char_string<const N: usize>() -> String {
    let mut s = String::new();
    let len: usize = kani::any();
    kani::assume(len <= N);
    for _ in 0..len {
        s.push(kani::any::<char>());
    }
    s
}

/// `doc_slug` output charset/shape, for EVERY input of up to 3 arbitrary chars.
///
/// Mirrors the assertions in the `doc_paths` fuzz target one-for-one.
#[kani::proof]
#[kani::unwind(16)]
fn doc_slug_invariants_hold_for_every_short_input() {
    let s = any_char_string::<3>();
    let slug = doc_slug(&s, "fb");

    assert!(!slug.is_empty(), "slug is never empty (falls back)");
    assert!(slug.len() <= 40, "slug is bounded");
    assert!(!slug.starts_with('-') && !slug.ends_with('-'), "no leading/trailing dash");
    assert!(
        slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "slug charset is [a-z0-9-]"
    );
}

/// `doc_filename` must never emit a path separator, a control character, or whitespace —
/// the properties that stop a stored document escaping its directory or spoofing a name.
#[kani::proof]
#[kani::unwind(16)]
fn doc_filename_invariants_hold_for_every_short_input() {
    let s = any_char_string::<3>();
    let f = doc_filename(&s);

    assert!(!f.is_empty(), "filename is never empty");
    assert!(f.len() <= 120, "filename is bounded");
    assert!(
        !f.chars().any(|c| c == '/' || c == '\\' || c.is_control() || c.is_whitespace()),
        "filename has no separator, control character or whitespace"
    );
    assert!(!f.starts_with('.') && !f.ends_with('.'), "no leading/trailing dot");
}

/// The traversal property: an arbitrary attacker-controlled subfolder appended to a
/// TRUSTED prefix can never produce `..`, `.`, an empty component, or lose the prefix.
/// This is the one whose failure would let an uploaded document land outside its vault
/// directory, so it is the one worth a proof rather than a sample.
#[kani::proof]
#[kani::unwind(16)]
fn doc_upload_dir_can_never_traverse_out_of_its_prefix() {
    let sub = any_char_string::<3>();
    let dir = doc_upload_dir("taxes/2024", &sub);

    assert!(dir.starts_with("taxes/2024"), "the trusted prefix survives");
    assert!(!dir.contains(' '), "no space");
    assert!(!dir.contains("/../") && !dir.contains("/./") && !dir.ends_with("/.."), "no traversal");
    let mut rest = dir.as_str();
    while let Some((comp, tail)) = rest.split_once('/') {
        assert!(!comp.is_empty(), "no empty path component");
        rest = tail;
    }
    assert!(!rest.is_empty(), "no empty trailing component");
}

/// The file-format entry point: arbitrary bytes must produce `Ok` or `Err`, never a
/// panic — no slice index out of range, no arithmetic overflow, no unwrap on `None`.
/// This is the first thing a hostile `vault.pmv` reaches.
///
/// Bound: a full header is larger than this, so what is proved here is that every
/// *truncated and malformed* header up to 32 bytes is rejected without panicking —
/// which is the class the parser is most likely to get wrong.
#[kani::proof]
#[kani::unwind(4)]
fn header_parse_never_panics_on_short_hostile_input() {
    let bytes: [u8; 32] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= 32);
    crate::vault::fuzz::header(&bytes[..len]);
}
