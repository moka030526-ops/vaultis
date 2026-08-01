//! Unit tests for the parent module ([`super`], `crypto.rs`), split into their own
//! file via `#[cfg(test)] #[path = "crypto_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use super::*;

// Cheap params so the test suite stays fast; production uses the defaults.
fn fast() -> KdfParams {
    KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }
}

/// Two secrets in the SAME page must be locked once and unlocked once — when the LAST of
/// them goes away, never when the first does.
///
/// This is the shape of the Windows crash this module was written for: with one
/// `region::LockGuard` per key, dropping key A unlocked the shared page, and dropping key B
/// then called `VirtualUnlock` on an unlocked page, which fails with ERROR_NOT_LOCKED and
/// trips `LockGuard::drop`'s `debug_assert!` — a debug-build panic at exit. On Linux a
/// second `munlock` simply succeeds, so only the refcount itself is observable everywhere;
/// that is what this asserts.
#[cfg(feature = "mlock")]
#[test]
fn page_locks_are_refcounted_so_a_shared_page_is_unlocked_exactly_once() {
    // One buffer, so both claims are guaranteed to cover the same page.
    let buf = Box::new([0u8; KEY_LEN]);
    let p = buf.as_ref().as_ptr();
    assert_eq!(page_lock::holders_of(p), 0, "nothing locked yet");

    let first = page_lock::PageLock::acquire(p, KEY_LEN).expect("locking one page must work");
    assert_eq!(page_lock::holders_of(p), 1, "first claim locks the page");
    let second = page_lock::PageLock::acquire(p, KEY_LEN).expect("a second claim is counted, not re-locked");
    assert_eq!(page_lock::holders_of(p), 2, "both secrets are counted");

    drop(first);
    assert_eq!(
        page_lock::holders_of(p),
        1,
        "the page must STAY locked while another secret lives in it (this is the bug)"
    );
    drop(second);
    assert_eq!(page_lock::holders_of(p), 0, "the last claim unlocks the page");

    // Re-locking the same page after full release works — i.e. the unlock really happened
    // and left no stale count behind.
    let again = page_lock::PageLock::acquire(p, KEY_LEN).expect("relocking after release works");
    assert_eq!(page_lock::holders_of(p), 1);
    drop(again);
    assert_eq!(page_lock::holders_of(p), 0);
}

/// The end-to-end version of the above, through the type that actually holds keys: the
/// chained two-password derivation builds an intermediate key, drops it, and keeps the
/// second — the exact sequence that panicked at process exit on Windows. Dropping keys in
/// that order must leave no page counted, and (on Windows, in a debug build) must not
/// panic.
#[cfg(feature = "mlock")]
#[test]
fn dropping_chained_keys_leaves_no_page_locked() {
    let salt = [7u8; SALT_LEN];
    let k = derive_key_chained(b"pw1", b"pw2", &salt, &fast()).expect("derivation works");
    let page_of_key = k.as_bytes().as_ptr();
    assert!(page_lock::holders_of(page_of_key) >= 1, "the surviving key's page stays locked");
    drop(k);
    assert_eq!(
        page_lock::holders_of(page_of_key),
        0,
        "once every key is dropped, no claim (and so no OS lock) is left on its page"
    );
}

#[test]
fn default_params_pass_their_own_validator() {
    // Invariant: the SHIPPED default must validate — the GUI/TUI/CLI always create
    // and import with `KdfParams::default()`, so if the default fell outside the
    // bounds, every new vault would be rejected (BadParams). This also pins the
    // ceilings ABOVE the default (e.g. catches MAX_M_COST being mis-set below the
    // 64 MiB default), and the explicit value guards the constant arithmetic.
    let d = KdfParams::default();
    assert!(d.validate().is_ok(), "default params must be within bounds");
    assert!(d.m_cost >= KdfParams::MIN_M_COST && d.m_cost <= KdfParams::MAX_M_COST);
    assert!(d.t_cost >= 1 && d.t_cost <= KdfParams::MAX_T_COST);
    assert!(d.p_cost >= 1 && d.p_cost <= KdfParams::MAX_P_COST);
    assert_eq!(KdfParams::MAX_M_COST, 524_288, "memory ceiling is 512 MiB (in KiB)");
    // Out-of-range params are rejected (both ends + t_cost).
    assert!(KdfParams { m_cost: KdfParams::MAX_M_COST + 1, t_cost: 3, p_cost: 1 }.validate().is_err());
    assert!(KdfParams { m_cost: KdfParams::MIN_M_COST - 1, t_cost: 3, p_cost: 1 }.validate().is_err());
    assert!(KdfParams { m_cost: 65_536, t_cost: KdfParams::MAX_T_COST + 1, p_cost: 1 }.validate().is_err());
}

#[test]
fn round_trip() {
    let key = derive_key(b"correct horse", b"sixteen-byte-slt", &fast()).unwrap();
    let aad = b"header-bytes";
    let (nonce, ct) = encrypt(&key, b"top secret", aad).unwrap();
    let pt = decrypt(&key, &nonce, &ct, aad).unwrap();
    assert_eq!(pt, b"top secret");
}

#[test]
fn wrong_password_fails() {
    let salt = b"sixteen-byte-slt";
    let good = derive_key(b"right", salt, &fast()).unwrap();
    let bad = derive_key(b"wrong", salt, &fast()).unwrap();
    let (nonce, ct) = encrypt(&good, b"secret", b"aad").unwrap();
    assert!(matches!(
        decrypt(&bad, &nonce, &ct, b"aad"),
        Err(CryptoError::Decrypt)
    ));
}

#[test]
fn tampered_ciphertext_fails() {
    let key = derive_key(b"pw", b"sixteen-byte-slt", &fast()).unwrap();
    let (nonce, mut ct) = encrypt(&key, b"secret", b"aad").unwrap();
    ct[0] ^= 0xff;
    assert!(decrypt(&key, &nonce, &ct, b"aad").is_err());
}

#[test]
fn encrypt_with_nonce_round_trips_and_binds_nonce() {
    let key = derive_key(b"pw", b"sixteen-byte-slt", &fast()).unwrap();
    let nonce = [7u8; NONCE_LEN];
    let aad = b"full-header-bytes";
    let ct = encrypt_with_nonce(&key, &nonce, b"secret", aad).unwrap();
    assert_eq!(decrypt(&key, &nonce, &ct, aad).unwrap(), b"secret");
    // A different nonce (or different aad) must not verify.
    assert!(decrypt(&key, &[9u8; NONCE_LEN], &ct, aad).is_err());
    assert!(decrypt(&key, &nonce, &ct, b"other-header").is_err());
}

#[test]
fn tampered_aad_fails() {
    // Changing the header (aad) must invalidate the tag.
    let key = derive_key(b"pw", b"sixteen-byte-slt", &fast()).unwrap();
    let (nonce, ct) = encrypt(&key, b"secret", b"header-v1").unwrap();
    assert!(decrypt(&key, &nonce, &ct, b"header-v2").is_err());
}

#[test]
fn derivation_is_deterministic() {
    let a = derive_key(b"pw", b"sixteen-byte-slt", &fast()).unwrap();
    let b = derive_key(b"pw", b"sixteen-byte-slt", &fast()).unwrap();
    assert_eq!(a.as_bytes(), b.as_bytes());
}

#[test]
fn random_bytes_differ() {
    let a = random_bytes::<NONCE_LEN>().unwrap();
    let b = random_bytes::<NONCE_LEN>().unwrap();
    assert_ne!(a, b, "two random nonces should not collide");
}

#[test]
fn chained_round_trip() {
    let salt = b"sixteen-byte-slt";
    let key = derive_key_chained(b"first", b"second", salt, &fast()).unwrap();
    let (nonce, ct) = encrypt(&key, b"two-pw secret", b"aad").unwrap();
    let pt = decrypt(&key, &nonce, &ct, b"aad").unwrap();
    assert_eq!(pt, b"two-pw secret");
}

#[test]
fn chained_requires_both_passwords() {
    let salt = b"sixteen-byte-slt";
    let good = derive_key_chained(b"first", b"second", salt, &fast()).unwrap();
    // Either password wrong -> different key.
    let bad_pw1 = derive_key_chained(b"FIRST", b"second", salt, &fast()).unwrap();
    let bad_pw2 = derive_key_chained(b"first", b"SECOND", salt, &fast()).unwrap();
    assert_ne!(good.as_bytes(), bad_pw1.as_bytes());
    assert_ne!(good.as_bytes(), bad_pw2.as_bytes());
}

#[test]
fn chained_is_order_sensitive() {
    // Swapping the two passwords must yield a different key.
    let salt = b"sixteen-byte-slt";
    let ab = derive_key_chained(b"alpha", b"beta", salt, &fast()).unwrap();
    let ba = derive_key_chained(b"beta", b"alpha", salt, &fast()).unwrap();
    assert_ne!(ab.as_bytes(), ba.as_bytes());
}

#[test]
fn chained_is_deterministic() {
    let salt = b"sixteen-byte-slt";
    let a = derive_key_chained(b"x", b"y", salt, &fast()).unwrap();
    let b = derive_key_chained(b"x", b"y", salt, &fast()).unwrap();
    assert_eq!(a.as_bytes(), b.as_bytes());
}


// --- mutation-testing kill-tests (round 7: cargo-mutants survivor closure) ---
#[test]
fn mut_kdf_validate_upper_bounds_at_exact_max() {
    // Pin each KDF ceiling at its exact value AND pin the `>` comparison in
    // validate(): a params set sitting EXACTLY on each ceiling must validate
    // (real: `max > max` is false), while one-over must be rejected. This kills
    // the const `*`->`+`/`/` mutations (the resolved literal differs) together
    // with the `>`->`>=`/`==` comparison mutants (which would reject the at-max
    // case). Each parameter is exercised independently.
    assert_eq!(KdfParams::MAX_M_COST, 524_288u32, "m_cost ceiling is 512 MiB in KiB");
    assert_eq!(KdfParams::MAX_T_COST, 16u32);
    assert_eq!(KdfParams::MAX_P_COST, 16u32);
    assert_eq!(KdfParams::MIN_M_COST, 8u32);

    // Exactly on the ceiling: must pass.
    assert!(
        KdfParams { m_cost: KdfParams::MAX_M_COST, t_cost: 1, p_cost: 1 }.validate().is_ok(),
        "m_cost == MAX must validate"
    );
    assert!(
        KdfParams { m_cost: KdfParams::MIN_M_COST, t_cost: KdfParams::MAX_T_COST, p_cost: 1 }.validate().is_ok(),
        "t_cost == MAX must validate"
    );
    assert!(
        KdfParams { m_cost: 8 * KdfParams::MAX_P_COST, t_cost: 1, p_cost: KdfParams::MAX_P_COST }.validate().is_ok(),
        "p_cost == MAX must validate (with m_cost >= 8*p_cost, the argon2 floor)"
    );
    // The argon2 cross-invariant m_cost >= 8*p_cost is now enforced by validate(): a
    // shape-valid-but-derivation-failing combo (m_cost=8, p_cost=2) is rejected up front.
    assert!(
        KdfParams { m_cost: 8, t_cost: 1, p_cost: 2 }.validate().is_err(),
        "m_cost < 8*p_cost must be rejected (argon2 MemoryTooLittle)"
    );

    // One over each ceiling: must be rejected.
    assert!(
        KdfParams { m_cost: KdfParams::MAX_M_COST + 1, t_cost: 1, p_cost: 1 }.validate().is_err(),
        "m_cost == MAX + 1 must be rejected"
    );
    assert!(
        KdfParams { m_cost: KdfParams::MIN_M_COST, t_cost: KdfParams::MAX_T_COST + 1, p_cost: 1 }.validate().is_err(),
        "t_cost == MAX + 1 must be rejected"
    );
    assert!(
        KdfParams { m_cost: KdfParams::MIN_M_COST, t_cost: 1, p_cost: KdfParams::MAX_P_COST + 1 }.validate().is_err(),
        "p_cost == MAX + 1 must be rejected"
    );

    // Exactly on the m_cost floor passes; one under is rejected (guards MIN_M_COST).
    assert!(
        KdfParams { m_cost: KdfParams::MIN_M_COST, t_cost: 1, p_cost: 1 }.validate().is_ok(),
        "m_cost == MIN must validate"
    );
    assert!(
        KdfParams { m_cost: KdfParams::MIN_M_COST - 1, t_cost: 1, p_cost: 1 }.validate().is_err(),
        "m_cost == MIN - 1 must be rejected"
    );
}

// --- known-answer tests (KAT) -----------------------------------------
// Every OTHER crypto test here compares the code against ITSELF (round-trip,
// determinism), so a silent dependency/configuration change that still
// self-round-trips would pass them all while making EVERY previously-written
// vault permanently undecryptable. These KATs pin the primitives to fixed,
// externally-anchored outputs so that failure class fails CI instead.

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn from_hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex string must have even length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

#[test]
fn kat_argon2id_pins_algorithm_version_and_output() {
    // Pins that derive_key is Argon2id / Version V0x13 / 32-byte output AND the
    // exact derived bytes. An argon2 default flip (Argon2id->Argon2i, V0x13->V0x10),
    // an output-length change, or a dropped parameter would still self-round-trip
    // and pass `derivation_is_deterministic`, but would diverge here.
    let params = KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 };
    let pw = b"pass-mgr-kat-password";
    let salt = b"pass-mgr-kat!slt"; // 16 bytes, like a real per-vault salt

    // (1) Independent, explicitly-configured reference derivation.
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN)).unwrap(),
    );
    let mut reference = [0u8; KEY_LEN];
    argon.hash_password_into(pw, salt, &mut reference).unwrap();
    let derived = derive_key(pw, salt, &params).unwrap();
    assert_eq!(
        derived.as_bytes(),
        &reference[..],
        "derive_key must be Argon2id / V0x13 / {KEY_LEN}-byte output"
    );

    // (2) Frozen vector. This literal is a CONTRACT: if it ever changes, the
    // on-disk KDF changed and existing vaults will not open — it must be paired
    // with a FORMAT_VERSION bump + migration, never silently edited to pass.
    assert_eq!(
        to_hex(derived.as_bytes()),
        "8b02325a933c6bb4a13ba99a8b455c4ccab09c7073d08f5b8643101df03cf30d",
    );
}

#[test]
fn kat_xchacha20poly1305_matches_the_published_vector() {
    // (1) Published external vector: draft-irtf-cfrg-xchacha-03 §A.3.1
    // (AEAD_XCHACHA20_POLY1305). encrypt_with_nonce must produce the exact
    // ciphertext+tag the spec specifies, and decrypt must invert it. This anchors
    // the AEAD to the standard independently of the chacha20poly1305 crate's own
    // tests, so a crate behaviour change (HChaCha subkey derivation, tag layout)
    // that still self-round-trips is caught here.
    let key = {
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&from_hex(
            "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
        ));
        Key::new(k)
    };
    let nonce: [u8; NONCE_LEN] = from_hex("404142434445464748494a4b4c4d4e4f5051525354555657")
        .try_into()
        .unwrap();
    let aad = from_hex("50515253c0c1c2c3c4c5c6c7");
    let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let expected = "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b4522f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff921f9664c97637da9768812f615c68b13b52ec0875924c1c7987947deafd8780acf49";

    let ct = encrypt_with_nonce(&key, &nonce, plaintext, &aad).unwrap();
    assert_eq!(
        to_hex(&ct),
        expected,
        "ciphertext+tag must match the published XChaCha20-Poly1305 vector"
    );
    assert_eq!(
        decrypt(&key, &nonce, &ct, &aad).unwrap(),
        plaintext.to_vec(),
        "decrypt inverts the published vector"
    );

    // (2) encrypt_with_nonce must equal a hand-built XChaCha20Poly1305 call: pins
    // that the wrapper uses this exact AEAD and binds nonce + aad the obvious way.
    let direct = XChaCha20Poly1305::new(ChaChaKey::from_slice(key.as_bytes()))
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plaintext[..], aad: &aad })
        .unwrap();
    assert_eq!(ct, direct);

    // (3) Frozen vector under our own fixed inputs (drift detector / contract).
    let fk = Key::new([0x42u8; KEY_LEN]);
    let fct = encrypt_with_nonce(&fk, &[0x07u8; NONCE_LEN], b"pass-mgr KAT plaintext", b"pass-mgr KAT aad")
        .unwrap();
    assert_eq!(
        to_hex(&fct),
        "876df61b4e2cf9d169efbc1f65bd6c4df1b50b431219787e2f3f9b2b6bab0281e821c4a8e4a3",
    );
}

#[test]
fn kdf_params_feed_the_derivation() {
    // The header binds m/t/p_cost as AAD (tampering fails the tag), but that alone
    // does not prove the params actually CHANGE the key. If the derivation ever
    // ignored a parameter, an attacker could rewrite the header to the minimum cost
    // (m_cost=8) and still derive the same key, silently defeating the memory-hard
    // work factor. Varying only one parameter at a time must change the key.
    let pw = b"pw";
    let salt = b"sixteen-byte-slt";
    let base = KdfParams { m_cost: 256, t_cost: 2, p_cost: 1 };
    let k = |p: &KdfParams| derive_key(pw, salt, p).unwrap().as_bytes().to_vec();
    let kb = k(&base);
    assert_ne!(kb, k(&KdfParams { m_cost: 512, ..base }), "m_cost must affect the key");
    assert_ne!(kb, k(&KdfParams { t_cost: 3, ..base }), "t_cost must affect the key");
    assert_ne!(kb, k(&KdfParams { p_cost: 2, ..base }), "p_cost must affect the key");
}

#[test]
fn chained_derivation_uses_k1_as_the_second_salt() {
    // Pin the documented construction key = Argon2id(pw2, salt = Argon2id(pw1, salt1)).
    // The existing chained tests prove order-sensitivity and both-required, but a
    // mutant that ignored the chaining (salted pass 2 with salt1, say) could still
    // pass those; this pins the exact algebraic relationship.
    let salt1 = b"sixteen-byte-slt";
    let params = fast();
    let k1 = derive_key(b"pw1", salt1, &params).unwrap();
    let expected = derive_key(b"pw2", k1.as_bytes(), &params).unwrap();
    let chained = derive_key_chained(b"pw1", b"pw2", salt1, &params).unwrap();
    assert_eq!(chained.as_bytes(), expected.as_bytes());
    // Must NOT equal the wrong-but-plausible "salt pass 2 with salt1" construction.
    let wrong = derive_key(b"pw2", salt1, &params).unwrap();
    assert_ne!(chained.as_bytes(), wrong.as_bytes(), "pass 2 must be salted by k1, not salt1");
}
