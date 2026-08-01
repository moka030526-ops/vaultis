//! Unit tests for the parent module ([`super`], `password.rs`), split into their own
//! file via `#[cfg(test)] #[path = "password_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

// `use super::*;` imports everything from the parent module (this file), so
// the tests can call `generate`, `uniform`, `GenOptions`, etc. directly.
use super::*;

// `#[test]` marks a function as a unit test the test runner will execute.
#[test]
fn respects_length() {
    // `..Default::default()` is "struct update syntax": set `length` to 32 and
    // fill every other field from GenOptions::default().
    let opts = GenOptions { length: 32, ..Default::default() };
    // `.unwrap()` extracts the Ok value of the Result and panics (failing the
    // test) if it's an Err — acceptable in tests where we expect success.
    // `assert_eq!` fails the test unless the two arguments are equal.
    assert_eq!(generate(&opts).unwrap().len(), 32);
}

#[test]
fn only_selected_classes() {
    let opts = GenOptions {
        length: 64,
        lowercase: false,
        uppercase: false,
        digits: true,
        symbols: false,
    };
    let pw = generate(&opts).unwrap();
    // `pw.bytes()` iterates over the password's bytes; `.all(|b| ...)` returns
    // true only if the closure holds for every byte. `assert!` fails the test
    // if its condition is false.
    assert!(pw.bytes().all(|b| b.is_ascii_digit()));
}

#[test]
fn guarantees_each_class_when_room() {
    let opts = GenOptions { length: 8, ..Default::default() };
    let pw = generate(&opts).unwrap();
    // `.any(|b| ...)` is the mirror of `.all`: true if at least one byte
    // satisfies the closure. `SYMBOLS.contains(&b)` checks membership; `&b`
    // passes a borrow of the byte because `contains` expects a reference.
    assert!(pw.bytes().any(|b| b.is_ascii_lowercase()));
    assert!(pw.bytes().any(|b| b.is_ascii_uppercase()));
    assert!(pw.bytes().any(|b| b.is_ascii_digit()));
    assert!(pw.bytes().any(|b| SYMBOLS.contains(&b)));
}

#[test]
fn rejects_no_classes() {
    let opts = GenOptions {
        length: 10,
        lowercase: false,
        uppercase: false,
        digits: false,
        symbols: false,
    };
    // `matches!(value, Pattern)` is true if `value` fits the given pattern
    // here, that the Result is the Err variant carrying GenError::NoClasses.
    assert!(matches!(generate(&opts), Err(GenError::NoClasses)));
}

#[test]
fn rejects_zero_length() {
    let opts = GenOptions { length: 0, ..Default::default() };
    assert!(matches!(generate(&opts), Err(GenError::ZeroLength)));
}

#[test]
fn rejects_overlong_length() {
    // A programmatic caller asking for an absurd length is refused with a clean
    // error, not served a multi-gigabyte allocation.
    let opts = GenOptions { length: usize::MAX, ..Default::default() };
    assert!(matches!(generate(&opts), Err(GenError::TooLong)));
}

// Regression: with a 32-bit draw the rejection zone collapsed to 0 for any
// n > 2^32, so `uniform` spun forever. A 64-bit draw must return promptly. Gated
// to 64-bit targets, where `usize` can actually exceed 2^32.
#[cfg(target_pointer_width = "64")]
#[test]
fn uniform_terminates_for_n_above_2_pow_32() {
    let n: usize = (1usize << 33) + 1; // > 2^32
    assert!(uniform(n).unwrap() < n);
}

#[test]
fn uniform_in_range() {
    // `for _ in 0..1000` repeats 1000 times; `_` is a throwaway loop variable
    // (we don't use the counter).
    for _ in 0..1000 {
        assert!(uniform(7).unwrap() < 7);
    }
}

#[test]
fn uniform_covers_the_whole_range() {
    // Over many draws every index in 0..n must appear. This rejects a sampler
    // that collapses to a constant (e.g. always 0 or 1) or grossly biases the
    // output away from part of the range.
    let n = 6;
    // `[false; 6]` is a fixed-size array of 6 booleans, all initialized false.
    let mut seen = [false; 6];
    for _ in 0..3000 {
        // Mark each drawn index as seen.
        seen[uniform(n).unwrap()] = true;
    }
    // `|&s| s` is a closure that destructures the borrowed bool `&s` into the
    // value `s`; the trailing string is the message shown if the assert fails.
    assert!(seen.iter().all(|&s| s), "uniform must reach every index in 0..n");
}

#[test]
fn generate_has_high_character_diversity() {
    // A correct generator drawing from all four classes produces many distinct
    // characters; a constant-index sampler would collapse to only a handful.
    let opts = GenOptions { length: 64, ..Default::default() };
    let pw = generate(&opts).unwrap();
    // `.collect()` into a `BTreeSet<u8>` (an ordered set) automatically
    // de-duplicates the bytes, so `distinct.len()` is the number of unique
    // characters. The target type on the left tells `collect` what to build.
    let distinct: std::collections::BTreeSet<u8> = pw.bytes().collect();
    assert!(distinct.len() >= 16, "expected diverse output, got {} distinct", distinct.len());
}

use proptest::prelude::*;
proptest! {
    /// `uniform(n)` is always in `[0, n)` and never panics, for any `n >= 1`.
    #[test]
    fn prop_uniform_in_range(n in 1usize..=2_000_000) {
        let r = uniform(n).unwrap();
        prop_assert!(r < n, "uniform({n}) = {r} out of range");
    }

    /// `generate` produces exactly the requested length using ONLY characters
    /// from the enabled classes (and never panics).
    #[test]
    fn prop_generate_length_and_charset(
        length in 1usize..=128,
        // Derive the four class flags from the bits of a NON-ZERO 4-bit mask, so at
        // least one class is always enabled WITHOUT a `prop_assume!` reject. The old
        // form (four independent bools + assume) discarded the all-false combo (1/16 of
        // cases); past ~1024 such rejects proptest aborts with "Too many global rejects",
        // so the property was silently un-runnable at a raised PROPTEST_CASES (it only
        // passed at the default 256). This strategy enumerates all 15 non-empty class
        // combinations with zero rejects, so the generator can be stress-tested at any
        // case count.
        mask in 1u8..=15,
    ) {
        let lowercase = mask & 1 != 0;
        let uppercase = mask & 2 != 0;
        let digits = mask & 4 != 0;
        let symbols = mask & 8 != 0;
        let opts = GenOptions { length, lowercase, uppercase, digits, symbols };
        let pw = generate(&opts).unwrap();
        prop_assert_eq!(pw.len(), length, "exact requested length (ASCII, 1 byte/char)");
        for c in pw.bytes() {
            let ok = (lowercase && LOWER.contains(&c))
                || (uppercase && UPPER.contains(&c))
                || (digits && DIGITS.contains(&c))
                || (symbols && SYMBOLS.contains(&c));
            prop_assert!(ok, "byte {c} is not from any enabled class");
        }
    }
}


// --- mutation-testing kill-tests (round 7: cargo-mutants survivor closure) ---
#[test]
fn mut_generate_accepts_exactly_max_length() {
    // Boundary: length == MAX_LENGTH must be ACCEPTED (the guard is strict `>`).
    // Mutating `>` to `>=` (line 171) would reject this exact value with TooLong,
    // so the unwrap below would panic and the length assertion would never pass.
    let opts = GenOptions { length: MAX_LENGTH, ..Default::default() };
    let pw = generate(&opts).unwrap();
    assert_eq!(pw.len(), MAX_LENGTH);
    // And one past the cap is still refused, pinning the cap location itself.
    let over = GenOptions { length: MAX_LENGTH + 1, ..Default::default() };
    assert!(matches!(generate(&over), Err(GenError::TooLong)));
}

#[test]
fn mut_generate_shuffle_uses_inclusive_swap_partner() {
    // Pins the Fisher-Yates index `uniform(i + 1)` (line 211) against the
    // `i * 1` mutation. With length 2 and only lower+upper enabled, the
    // pre-shuffle buffer is exactly [lower, upper] (classes() pushes LOWER then
    // UPPER). The only shuffle step is i == 1: real code draws uniform(2) -> j in
    // {0,1}, so when j == 1 there is NO swap and index 0 stays lowercase. The
    // `* 1` mutation draws uniform(1) -> j == 0 always, forcing swap(1,0) every
    // time, so index 0 would ALWAYS be uppercase. Observing a lowercase at index
    // 0 even once distinguishes the two (false-negative prob ~ 2^-256).
    let opts = GenOptions {
        length: 2,
        lowercase: true,
        uppercase: true,
        digits: false,
        symbols: false,
    };
    let mut saw_lower_at_0 = false;
    let mut saw_upper_at_0 = false;
    for _ in 0..256 {
        let pw = generate(&opts).unwrap();
        let b0 = pw.as_bytes()[0];
        if b0.is_ascii_lowercase() {
            saw_lower_at_0 = true;
        }
        if b0.is_ascii_uppercase() {
            saw_upper_at_0 = true;
        }
    }
    // Real code yields both outcomes (~50/50); the `* 1` mutant never leaves a
    // lowercase at index 0, so this fails under the mutation.
    assert!(saw_lower_at_0, "index 0 must sometimes stay lowercase (uniform(i+1), not uniform(i))");
    assert!(saw_upper_at_0, "index 0 must sometimes be uppercase too (sanity)");
}
