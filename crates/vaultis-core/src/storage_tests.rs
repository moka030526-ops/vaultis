//! Unit tests for the parent module ([`super`], `storage.rs`), split into their own
//! file via `#[cfg(test)] #[path = "storage_tests.rs"] mod tests;` so the tests do not sit
//! inside the implementation.
//!
//! This stays an **inner module** rather than moving to `tests/`: `use super::*` reaches
//! the parent's PRIVATE items, which a separate test crate under `tests/` could not name
//! without marking them `pub` purely to be testable. Tests needing only the public API
//! (or a real process) already live in `tests/`.
//!
//! `#[cfg(test)]` on the declaration means this file is compiled ONLY under `cargo test`
//! — never part of a shipped binary.

use super::*; // pull in everything from the parent module under test
use crate::crypto::{derive_key, KdfParams};
use std::time::{SystemTime, UNIX_EPOCH};

fn fast_key() -> Key {
    derive_key(b"pw", b"sixteen-byte-slt", &KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }).unwrap()
}
fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}
fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pmstore-{tag}-{}", nanos()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn missing_middle_partition_is_detected_not_silently_dropped() {
    // A lost middle partition must FAIL CLOSED on open, never silently drop the
    // documents in the surviving higher partition(s). Without the contiguity guard
    // in `open`, the partition-scan loop stops at the first gap and "successfully"
    // returns a store missing partition 2's document — silent data loss.
    let dir = tmp_dir("gap");
    let key = fast_key();
    // Small cap so each (large) doc rolls into its own partition: 0, 1, 2.
    let mut s = VolumeStore::open(&dir, &key, "v", 1024).unwrap();
    let big = vec![b'x'; 1000];
    s.put("a", "/a", &big, 1, &key).unwrap();
    s.put("b", "/b", &big, 2, &key).unwrap();
    s.put("c", "/c", &big, 3, &key).unwrap();
    assert_eq!(s.partition_count(), 3, "each large doc should roll to its own partition");
    drop(s);
    // Lose ONLY the middle partition (1); 0 and 2 survive.
    std::fs::remove_file(dir.join("volume").join("vol.1")).unwrap();
    std::fs::remove_file(dir.join("manifest").join("manifest.1")).unwrap();
    match VolumeStore::open(&dir, &key, "v", 1024) {
        Err(StorageError::Corrupt(_)) => {} // fail-closed: correct
        Ok(s2) => panic!(
            "a missing middle partition was silently accepted: contains(c)={}, partitions={}",
            s2.contains("c"),
            s2.partition_count()
        ),
        Err(e) => panic!("unexpected error variant: {e:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_rebuilt_manifest_obeys_the_same_entry_cap_as_a_stored_one() {
    // The read side must never admit a manifest the write side would refuse. `load_manifest`
    // fails closed on more than MAX_MANIFEST_ENTRIES entries, but `scan_volume` (the
    // manifest-loss rebuild) is infallible by design and used to hand back any number.
    // An over-cap rebuild would be committed by the next put/remove in that partition,
    // and every later open would then fail `TooLarge` — which `open` does NOT rebuild
    // from — bricking an otherwise-intact vault. Pin the boundary on both sides.
    let entry = |i: usize| ManifestEntry {
        id: format!("{i:x}"),
        path: "/d".into(),
        size: 1,
        offset: 0,
        length: 1,
        uploaded_at: 0,
    };
    let manifest = |n: usize| Manifest { seq: 1, end_offset: 0, entries: (0..n).map(entry).collect() };

    // Exactly at the cap is ACCEPTED (the guard is a strict `>`, matching load_manifest),
    // and the manifest passes through unchanged.
    let at_cap = reject_over_cap(manifest(8), 8).expect("exactly max is accepted");
    assert_eq!(at_cap.entries.len(), 8, "an in-range manifest is returned untouched");
    // One over is rejected — and with the SAME variant load_manifest uses, so `open`
    // treats it identically (not as rebuildable corruption).
    assert!(
        matches!(reject_over_cap(manifest(9), 8), Err(StorageError::TooLarge)),
        "max + 1 is rejected as TooLarge"
    );
    // And an empty/typical rebuild is unaffected.
    assert!(reject_over_cap(manifest(0), MAX_MANIFEST_ENTRIES).is_ok());
    assert!(reject_over_cap(manifest(3), MAX_MANIFEST_ENTRIES).is_ok());
}

#[test]
fn rebuild_from_a_volume_still_succeeds_under_the_cap() {
    // Wiring check for the guard above: the real rebuild path (corrupt manifest, intact
    // volume) must still recover normally — the cap must not have turned recovery into a
    // refusal for an ordinary vault.
    let dir = tmp_dir("rebuildcap");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for i in 0..5 {
        s.put(&format!("id{i}"), "/d", b"body", i, &key).unwrap();
    }
    drop(s);
    std::fs::write(dir.join("manifest/manifest.0"), b"garbage").unwrap();
    let rebuilt = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for i in 0..5 {
        assert_eq!(&*rebuilt.read(&format!("id{i}"), &key).unwrap(), b"body");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn put_rolls_to_a_new_partition_at_the_manifest_entry_cap() {
    // A partition manifest must never grow past MAX_MANIFEST_ENTRIES: the reader
    // (load_manifest) fails CLOSED (`TooLarge`) on a larger one, and `open` does NOT
    // treat that as rebuildable — so a write past the cap would BRICK an otherwise-intact
    // vault on the next open. `target_partition` must roll to a fresh partition on the
    // entry-count cap, exactly as it already does on the byte cap. (Audit 2026-07-03 A-1.)
    let dir = tmp_dir("entrycap");
    let key = fast_key();
    // Huge byte cap so BYTE-based rollover never fires — isolate the entry-count path.
    let mut s = VolumeStore::open(&dir, &key, "v", u64::MAX).unwrap();
    s.put("real", "/real", b"x", 1, &key).unwrap();
    assert_eq!(s.partition_count(), 1);
    // Fill partition 0's manifest to the cap WITHOUT writing 100k real documents.
    let start = s.manifests[0].entries.len();
    for i in start..MAX_MANIFEST_ENTRIES {
        s.manifests[0].entries.push(ManifestEntry {
            id: format!("f{i}"),
            path: "/f".into(),
            size: 1,
            offset: 0,
            length: 1,
            uploaded_at: 0,
        });
    }
    assert_eq!(s.manifests[0].entries.len(), MAX_MANIFEST_ENTRIES);
    // A brand-new id must NOT be placed into the full partition (that would push it to
    // MAX+1, which the reader rejects) — it rolls to a fresh partition instead.
    assert_eq!(s.target_partition("brand-new", 1), 1, "new id rolls to a fresh partition at the cap");
    // An UPDATE to an existing id stays in its own partition: retain+push keeps the count
    // constant, so it never grows past the cap.
    assert_eq!(s.target_partition("real", 1), 0, "an update stays put — it doesn't grow the count");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg(unix)]
fn load_manifest_refuses_a_symlinked_manifest() {
    // A `manifest.N` swapped for a SYMLINK must be REFUSED, never followed: a metadata()
    // cap + fs::read would follow a symlink to e.g. /dev/zero (stat size 0, so the size
    // cap passes) and read unboundedly → OOM. We assert the manifest reader directly: the
    // decoy target is a real readable file, so following it WOULD "succeed" — proving the
    // refusal is about the symlink itself. (`open()` itself recovers by rebuilding from the
    // volume, which never reads the manifest, so the symlink can't OOM or brick the vault.)
    let dir = tmp_dir("mansym");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", 1 << 20).unwrap();
    s.put("a", "/a", b"hello", 1, &key).unwrap();
    let manifest = dir.join("manifest").join("manifest.0");
    let decoy = dir.join("decoy");
    std::fs::write(&decoy, b"not a manifest").unwrap();
    std::fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink(&decoy, &manifest).unwrap();
    match s.load_manifest(0, &key) {
        Err(StorageError::Corrupt(_)) => {} // refused the symlink: correct
        Err(e) => panic!("a symlinked manifest must be refused as Corrupt, got {e:?}"),
        Ok(_) => panic!("a symlinked manifest must be refused, not followed"),
    }
    // And open() still recovers (rebuilds from the volume) rather than bricking.
    drop(s);
    let s2 = VolumeStore::open(&dir, &key, "v", 1 << 20).unwrap();
    assert!(s2.contains("a"), "open recovers the document by rebuilding from the volume");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_file_bounded_nofollow_enforces_the_exact_cap() {
    // Pins the `buf.len() > max` boundary (the TOCTOU / lying-size guard): a file of
    // EXACTLY `max` bytes is accepted, one byte over is TooLarge. (Mutation kill-test:
    // `>` -> `==`/`>=` would wrongly reject the exactly-max case.)
    let dir = tmp_dir("rfbn");
    let path = dir.join("blob");
    std::fs::write(&path, vec![0u8; 100]).unwrap();
    assert_eq!(read_file_bounded_nofollow(&path, 100).unwrap().len(), 100, "exactly max is accepted");
    assert!(
        matches!(read_file_bounded_nofollow(&path, 99), Err(StorageError::TooLarge)),
        "max + 1 is rejected"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn single_bit_corruption_in_a_committed_frame_fails_closed_on_read() {
    // Bit rot inside a committed frame's bytes (manifest intact) is the most common
    // real-world corruption on the normal read path. AEAD must turn it into a clean
    // error — never silently return altered plaintext under the right document id.
    let dir = tmp_dir("bitrot");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("doc", "/d", b"the quick brown fox", 1, &key).unwrap();
    let e = s.entry("doc").unwrap();
    let target = (e.offset + e.length - 1) as usize; // last byte = inside the Poly1305 tag
    drop(s);
    let vpath = dir.join("volume").join("vol.0");
    let mut bytes = std::fs::read(&vpath).unwrap();
    bytes[target] ^= 0x01;
    std::fs::write(&vpath, &bytes).unwrap();
    // open() is lazy (reads no frames), so the intact manifest still opens fine...
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    // ...but read() must fail closed.
    match s2.read("doc", &key) {
        Err(StorageError::Crypto(_) | StorageError::Corrupt(_)) => {}
        Ok(b) => panic!("bit rot served wrong plaintext instead of failing: {:?}", *b),
        Err(e) => panic!("unexpected error variant: {e:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn put_read_round_trip() {
    let dir = tmp_dir("rt");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "vault-1", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("id1", "/wills/will.pdf", b"last will and testament", 100, &key).unwrap();
    assert_eq!(&*s.read("id1", &key).unwrap(), b"last will and testament");
    // Reopen: lazily load manifests, read again.
    let s2 = VolumeStore::open(&dir, &key, "vault-1", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("id1"));
    assert_eq!(&*s2.read("id1", &key).unwrap(), b"last will and testament");
    assert_eq!(s2.entry("id1").unwrap().path, "/wills/will.pdf");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_stays_in_same_partition_and_new_value_wins() {
    let dir = tmp_dir("upd");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/p", b"v1", 1, &key).unwrap();
    s.put("a", "/p", b"version two", 2, &key).unwrap();
    assert_eq!(s.partition_count(), 1, "update reuses the same partition");
    assert_eq!(&*s.read("a", &key).unwrap(), b"version two");
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(&*s2.read("a", &key).unwrap(), b"version two");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn partition_rolls_over_at_cap() {
    let dir = tmp_dir("roll");
    let key = fast_key();
    // Tiny cap so each ~1 KiB doc lands in its own partition.
    let mut s = VolumeStore::open(&dir, &key, "v", 1024).unwrap();
    let big = vec![7u8; 600];
    s.put("a", "/a", &big, 1, &key).unwrap();
    s.put("b", "/b", &big, 2, &key).unwrap();
    s.put("c", "/c", &big, 3, &key).unwrap();
    assert!(s.partition_count() >= 2, "documents rolled into new partitions");
    let s2 = VolumeStore::open(&dir, &key, "v", 1024).unwrap();
    for id in ["a", "b", "c"] {
        assert!(s2.contains(id));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_drops_entry() {
    let dir = tmp_dir("rm");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"data", 1, &key).unwrap();
    s.remove("a", &key).unwrap();
    assert!(!s.contains("a"));
    assert!(matches!(s.read("a", &key), Err(StorageError::NotFound(_))));
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(!s2.contains("a"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn path_too_long_rejected() {
    let dir = tmp_dir("path");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    let long = "x".repeat(MAX_PATH_LEN + 1);
    assert!(matches!(s.put("a", &long, b"d", 1, &key), Err(StorageError::PathTooLong)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn crash_after_append_before_manifest_commit_is_ignored() {
    // Simulate a crash between the volume fsync and the manifest commit by
    // appending a raw frame to vol.0 WITHOUT updating manifest.0. On reopen,
    // the manifest's end_offset is authoritative, so the orphan is invisible.
    let dir = tmp_dir("crash1");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"committed", 1, &key).unwrap();
    let committed_end = s.manifests[0].end_offset;

    // Append an extra (uncommitted) frame directly to the volume.
    let orphan = encode_frame(&key, "v", 0, "ghost", "/g", b"never committed").unwrap();
    append_frame(&dir.join("volume/vol.0"), committed_end, &orphan).unwrap();

    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("a"), "committed doc survives");
    assert!(!s2.contains("ghost"), "uncommitted orphan is ignored");
    assert_eq!(s2.manifests[0].end_offset, committed_end);

    // A subsequent put overwrites the orphan region; data stays consistent.
    let mut s3 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s3.put("b", "/b", b"next", 2, &key).unwrap();
    assert_eq!(&*s3.read("a", &key).unwrap(), b"committed");
    assert_eq!(&*s3.read("b", &key).unwrap(), b"next");
    assert!(!s3.contains("ghost"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_manifest_is_rebuilt_from_volume() {
    let dir = tmp_dir("rebuild");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    s.put("b", "/b", b"bravo", 2, &key).unwrap();

    // Corrupt the manifest (truncate it to garbage); the volume is intact.
    std::fs::write(dir.join("manifest/manifest.0"), b"garbage").unwrap();

    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    assert_eq!(&*s2.read("b", &key).unwrap(), b"bravo");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn torn_tail_is_ignored_and_overwritten() {
    let dir = tmp_dir("torn");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    let end = s.manifests[0].end_offset;
    // Append random trailing garbage (a torn frame) beyond the committed end.
    {
        let mut f = OpenOptions::new().write(true).open(dir.join("volume/vol.0")).unwrap();
        f.seek(SeekFrom::Start(end)).unwrap();
        f.write_all(&[0xAB; 37]).unwrap();
        f.sync_all().unwrap();
    }
    let mut s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    s2.put("b", "/b", b"bravo", 2, &key).unwrap();
    assert_eq!(&*s2.read("b", &key).unwrap(), b"bravo");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn foreign_vault_id_cannot_read_documents() {
    // The manifest + frames are AEAD-bound to the vault id. Opening under a
    // different id can't decrypt them, so the document is not exposed. (In the
    // real flow the id always comes from the already-decrypted vault, so this
    // is a defense-in-depth property; tampering that drops a referenced doc is
    // caught by the vault-level manifest⊆referenced check in Phase 3.)
    let dir = tmp_dir("foreign");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "vault-A", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"secret", 1, &key).unwrap();
    let other = VolumeStore::open(&dir, &key, "vault-B", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(!other.contains("a"), "documents are not readable under a foreign vault id");
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Phase 7: parsers, AAD binding, bounds, nonce uniqueness, crash matrix --

/// Build a decrypted-frame plaintext `[u32 id_len][id][u32 path_len][path][body]`.
fn frame_plaintext(id: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(id.len() as u32).to_le_bytes());
    p.extend_from_slice(id.as_bytes());
    p.extend_from_slice(&(path.len() as u32).to_le_bytes());
    p.extend_from_slice(path.as_bytes());
    p.extend_from_slice(body);
    p
}

#[test]
fn frame_plaintext_round_trips_and_rejects_malformed() {
    let p = frame_plaintext("the-id", "/loc/file.pdf", b"body bytes");
    let (id, path, body) = parse_plaintext(&p).unwrap();
    assert_eq!(id, "the-id");
    assert_eq!(path, "/loc/file.pdf");
    assert_eq!(&body[..], b"body bytes");
    // Too short for even the id-length prefix.
    assert!(parse_plaintext(b"\x01\x00").is_err());
    // id_len claims more bytes than are present.
    let mut short = (99u32).to_le_bytes().to_vec();
    short.extend_from_slice(b"abc");
    assert!(matches!(parse_plaintext(&short), Err(StorageError::Corrupt(_))));
    // id_len = u32::MAX must not wrap or over-allocate.
    let mut huge = u32::MAX.to_le_bytes().to_vec();
    huge.extend_from_slice(b"x");
    assert!(parse_plaintext(&huge).is_err());
}

#[test]
fn read_frame_at_bounds_and_per_blob_aad() {
    let dir = tmp_dir("frame");
    let key = fast_key();
    let aad0 = volume_aad("v", 0);
    let frame = encode_frame(&key, "v", 0, "id", "/p", b"payload").unwrap();
    let vol = dir.join("vol");
    append_frame(&vol, 0, &frame).unwrap();
    let mut f = File::open(&vol).unwrap();
    let len = f.metadata().unwrap().len();
    // Correct read, including the expected-length sanity check.
    let (id, path, body) = read_frame_at(&mut f, len, 0, len, &key, &aad0).unwrap();
    assert_eq!((id.as_str(), path.as_str(), &body[..]), ("id", "/p", &b"payload"[..]));
    // Per-blob AAD binding: a frame for partition 0 won't authenticate as 1.
    assert!(read_frame_at(&mut f, len, 0, 0, &key, &volume_aad("v", 1)).is_err());
    // Foreign vault-id AAD also fails.
    assert!(read_frame_at(&mut f, len, 0, 0, &key, &volume_aad("other", 0)).is_err());
    // An expected-length that disagrees with the manifest is rejected.
    assert!(read_frame_at(&mut f, len, 0, len + 1, &key, &aad0).is_err());
    // Offset past EOF.
    assert!(read_frame_at(&mut f, len, len + 1, 0, &key, &aad0).is_err());
    // A corrupt length prefix (u32::MAX) is rejected, not over-read.
    {
        let mut w = OpenOptions::new().write(true).open(&vol).unwrap();
        w.write_all(&u32::MAX.to_le_bytes()).unwrap();
        w.sync_all().unwrap();
    }
    let mut f2 = File::open(&vol).unwrap();
    let len2 = f2.metadata().unwrap().len();
    assert!(read_frame_at(&mut f2, len2, 0, 0, &key, &aad0).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn manifest_aad_binds_vault_id_and_partition() {
    let dir = tmp_dir("maad");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "vault-A", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"data", 1, &key).unwrap();
    let raw = std::fs::read(dir.join("manifest/manifest.0")).unwrap();
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    // The right AAD decrypts; the wrong vault id or partition does not.
    assert!(crypto::decrypt(&key, nonce, ct, &manifest_aad("vault-A", 0)).is_ok());
    assert!(crypto::decrypt(&key, nonce, ct, &manifest_aad("vault-B", 0)).is_err());
    assert!(crypto::decrypt(&key, nonce, ct, &manifest_aad("vault-A", 1)).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn frame_nonces_are_unique_across_writes() {
    let dir = tmp_dir("nonce");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for i in 0..50 {
        s.put(&format!("id{i}"), "/p", b"identical body", i as i64, &key).unwrap();
    }
    let bytes = std::fs::read(dir.join("volume/vol.0")).unwrap();
    let mut nonces = std::collections::BTreeSet::new();
    let mut off = 0usize;
    while off + 4 <= bytes.len() {
        let flen = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let nstart = off + 4;
        if nstart + NONCE_LEN > bytes.len() {
            break;
        }
        nonces.insert(bytes[nstart..nstart + NONCE_LEN].to_vec());
        off = nstart + flen;
    }
    assert_eq!(nonces.len(), 50, "every frame uses a distinct nonce");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wrong_key_cannot_read_blob() {
    let dir = tmp_dir("wrongkey");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"secret", 1, &key).unwrap();
    let other =
        derive_key(b"different", b"sixteen-byte-slt", &KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 }).unwrap();
    assert!(s.read("a", &other).is_err(), "a foreign key cannot decrypt the blob");
    std::fs::remove_dir_all(&dir).ok();
}

/// A single flipped bit mid-volume, with the manifest gone, must cost exactly the frame
/// it landed in.
///
/// This test used to assert the opposite — that the rebuild STOPPED there, discarding
/// every later frame as well. That was safe but needlessly lossy: those frames are
/// intact and authentic, and dropping them from the index is what turns one bad byte
/// into a vault that will not open (a record still pointing at a dropped document fails
/// the `referenced ⊆ stored` check). The scan now steps over the damage and carries on,
/// admitting only frames that pass their AEAD tag.
#[test]
fn mid_file_corrupt_frame_is_skipped_and_later_frames_still_recover() {
    let dir = tmp_dir("midcorrupt");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    let b_off = s.manifests[0].end_offset; // where b's frame will start
    s.put("b", "/b", b"bravo", 2, &key).unwrap();
    s.put("c", "/c", b"charlie", 3, &key).unwrap();
    // Flip a byte inside b's ciphertext (just past its 4-byte len + nonce).
    {
        let mut w = OpenOptions::new().read(true).write(true).open(dir.join("volume/vol.0")).unwrap();
        let pos = b_off + 4 + NONCE_LEN as u64;
        w.seek(SeekFrom::Start(pos)).unwrap();
        let mut byte = [0u8; 1];
        w.read_exact(&mut byte).unwrap();
        w.seek(SeekFrom::Start(pos)).unwrap();
        w.write_all(&[byte[0] ^ 0xFF]).unwrap();
        w.sync_all().unwrap();
    }
    // Clobber the manifest to force a rebuild by scanning the volume.
    std::fs::write(dir.join("manifest/manifest.0"), b"x").unwrap();
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("a"), "frames before the corruption recover");
    assert!(!s2.contains("b"), "the corrupt frame itself is never admitted");
    assert!(s2.contains("c"), "the scan resynchronises past the damage instead of giving up");
    assert_eq!(&*s2.read("c", &key).unwrap(), b"charlie", "and the recovered frame really reads");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stray_temp_file_does_not_disturb_open() {
    // A crash mid manifest temp-write leaves a hidden ".*.tmp" sibling; it must
    // be ignored (only manifest.<N> is authoritative).
    let dir = tmp_dir("tmp");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"data", 1, &key).unwrap();
    std::fs::write(dir.join("manifest/.manifest.0.deadbeef.tmp"), b"garbage temp").unwrap();
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(&*s2.read("a", &key).unwrap(), b"data");
    assert_eq!(s2.partition_count(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn frame_substitution_within_partition_is_rejected() {
    // Two equal-length, individually-authentic frames in the same partition.
    let dir = tmp_dir("swap");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/x", b"AAAAAAAAAA", 1, &key).unwrap();
    s.put("b", "/y", b"BBBBBBBBBB", 2, &key).unwrap();
    let ea = s.entry("a").unwrap().clone();
    let eb = s.entry("b").unwrap().clone();
    assert_eq!(ea.length, eb.length, "frames must be equal length for the swap");
    // Swap the two frames' bytes in the volume (the manifest is untouched).
    let volp = dir.join("volume/vol.0");
    let mut bytes = std::fs::read(&volp).unwrap();
    let (oa, ob, len) = (ea.offset as usize, eb.offset as usize, ea.length as usize);
    let fa = bytes[oa..oa + len].to_vec();
    let fb = bytes[ob..ob + len].to_vec();
    bytes[oa..oa + len].copy_from_slice(&fb);
    bytes[ob..ob + len].copy_from_slice(&fa);
    std::fs::write(&volp, &bytes).unwrap();
    // The substituted frame's authenticated id no longer matches the manifest.
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(matches!(s2.read("a", &key), Err(StorageError::Corrupt(_))), "swap into a detected");
    assert!(matches!(s2.read("b", &key), Err(StorageError::Corrupt(_))), "swap into b detected");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_rejects_a_manifest_size_that_lies_about_the_body() {
    // The manifest `size` is an independently-serialized field; read() must verify the
    // DECRYPTED body length against it. A crafted source vault could otherwise declare a
    // SMALL size for an authentic OVERSIZE frame to slip past the merge oversize-preview
    // guard, then abort apply post-approval. read() rejects any size mismatch as Corrupt.
    let dir = tmp_dir("sizecheck");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"0123456789", 1, &key).unwrap(); // 10-byte body
    assert_eq!(&*s.read("a", &key).unwrap(), b"0123456789", "reads with an honest size");
    // Forge the manifest's declared size (the on-disk frame is untouched).
    let e = s.manifests[0].entries.iter_mut().find(|e| e.id == "a").unwrap();
    e.size = 0;
    assert!(
        matches!(s.read("a", &key), Err(StorageError::Corrupt(_))),
        "a manifest size that disagrees with the real body is rejected, never served"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn append_frame_refuses_to_write_through_a_symlink() {
    let dir = tmp_dir("symlink");
    let key = fast_key();
    std::fs::create_dir_all(dir.join("volume")).unwrap();
    let target = dir.join("secret_target");
    std::fs::write(&target, b"do not touch").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("volume/vol.0")).unwrap();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(matches!(s.put("a", "/a", b"data", 1, &key), Err(StorageError::Io(_))));
    assert_eq!(std::fs::read(&target).unwrap(), b"do not touch", "symlink target untouched");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn manifest_seq_advances_on_each_committed_change() {
    // The per-partition manifest sequence counter advances on every committed put
    // and remove (locks the format invariant; kills `seq += 1` -> `seq *= 1`).
    let dir = tmp_dir("seq");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"one", 1, &key).unwrap();
    let after_put = s.manifests[0].seq;
    s.put("a", "/a", b"two", 2, &key).unwrap(); // update the same id
    let after_update = s.manifests[0].seq;
    s.remove("a", &key).unwrap();
    let after_remove = s.manifests[0].seq;
    assert!(after_update > after_put, "seq advances on update ({after_put} -> {after_update})");
    assert!(after_remove > after_update, "seq advances on remove ({after_update} -> {after_remove})");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_manifest_on_higher_partition_is_rebuilt() {
    // Kills the `&&` -> `||` mutant in the partition scan: a partition with a
    // present volume but missing manifest must still be loaded (rebuilt).
    let dir = tmp_dir("higherrebuild");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", 1024).unwrap();
    let big = vec![5u8; 600];
    s.put("a", "/a", &big, 1, &key).unwrap();
    s.put("b", "/b", &big, 2, &key).unwrap();
    assert!(s.partition_count() >= 2, "docs span >= 2 partitions");
    std::fs::remove_file(dir.join("manifest/manifest.1")).unwrap();
    let s2 = VolumeStore::open(&dir, &key, "v", 1024).unwrap();
    assert!(s2.contains("a"), "partition 0 intact");
    assert!(s2.contains("b"), "partition 1 rebuilt from its volume");
    assert_eq!(&*s2.read("b", &key).unwrap(), &big[..]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_frame_at_rejects_subnonce_frame_length() {
    // Kills the `||` -> `&&` mutant in the plausibility check: a frame_len below
    // NONCE_LEN must be rejected, not cause an OOB split_at on a short buffer.
    let dir = tmp_dir("subnonce");
    let key = fast_key();
    let aad = volume_aad("v", 0);
    let vol = dir.join("vol");
    let mut raw = 10u32.to_le_bytes().to_vec(); // frame_len = 10 (< NONCE_LEN = 24)
    raw.extend_from_slice(&[0u8; 10]);
    std::fs::write(&vol, &raw).unwrap();
    let mut f = File::open(&vol).unwrap();
    let len = f.metadata().unwrap().len();
    assert!(read_frame_at(&mut f, len, 0, 0, &key, &aad).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn volume_and_manifest_files_are_hardened() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("perms");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"data", 1, &key).unwrap();
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(dir.join("volume/vol.0")), 0o600, "volume file is 0600");
    assert_eq!(mode(dir.join("manifest/manifest.0")), 0o600, "manifest file is 0600");
    assert_eq!(mode(dir.join("volume")), 0o700, "volume dir is 0700");
    assert_eq!(mode(dir.join("manifest")), 0o700, "manifest dir is 0700");
    std::fs::remove_dir_all(&dir).ok();
}

// `proptest!` is property-based testing: instead of fixed inputs, it generates many
// random inputs matching each parameter's strategy (e.g. the regex `"[ -~]{0,40}"`
// or `vec(any::<u8>(), 0..200)`) and asserts the property holds for all of them.
// `prop_assert_eq!` is the property-test form of `assert_eq!`.
// ---- Full-disk (ENOSPC) fault injection (cargo test --features fault-injection) ----

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_on_volume_append_leaves_prior_state_intact() {
    let dir = tmp_dir("enospc-vol");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    // Disk fills exactly as the next document's frame is about to be written.
    crate::fault::fail_at("volume.write", 1);
    let err = s.put("b", "/b", b"bravo", 2, &key).unwrap_err();
    crate::fault::clear();
    assert!(matches!(err, StorageError::Io(_)), "put fails cleanly, got {err:?}");
    // Reopen: the failed put left no trace; the prior doc is intact and a later
    // put now succeeds.
    let mut s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("a") && !s2.contains("b"));
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    s2.put("b", "/b", b"bravo", 2, &key).unwrap();
    assert_eq!(&*s2.read("b", &key).unwrap(), b"bravo");
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_on_manifest_commit_ignores_the_uncommitted_frame() {
    let dir = tmp_dir("enospc-man");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    // The volume append succeeds but the manifest commit hits a full disk: the
    // new frame is now a torn tail past the committed end_offset.
    crate::fault::fail_at("atomic.write", 1);
    assert!(s.put("b", "/b", b"bravo", 2, &key).is_err());
    crate::fault::clear();
    let mut s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("a") && !s2.contains("b"), "uncommitted frame is invisible");
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    // The torn tail is overwritten by the next successful put — no corruption.
    s2.put("c", "/c", b"charlie", 3, &key).unwrap();
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    assert_eq!(&*s2.read("c", &key).unwrap(), b"charlie");
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(feature = "fault-injection")]
#[test]
fn enospc_on_manifest_rename_keeps_old_manifest() {
    let dir = tmp_dir("enospc-ren");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    crate::fault::fail_at("atomic.rename", 1);
    assert!(s.put("b", "/b", b"bravo", 2, &key).is_err());
    crate::fault::clear();
    // No stray manifest temp is loaded; the old manifest stands.
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert!(s2.contains("a") && !s2.contains("b"));
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    std::fs::remove_dir_all(&dir).ok();
}

use proptest::prelude::*;
proptest! {
    /// Length-prefixed frames round-trip for arbitrary id/path/body, even with
    /// separators or non-ASCII bytes embedded in the (authenticated) plaintext.
    #[test]
    fn prop_frame_plaintext_round_trips(
        id in "[ -~]{0,40}",
        path in "[ -~]{0,80}",
        body in proptest::collection::vec(any::<u8>(), 0..200),
    ) {
        let p = frame_plaintext(&id, &path, &body);
        let (rid, rpath, rbody) = parse_plaintext(&p).unwrap();
        prop_assert_eq!(rid, id);
        prop_assert_eq!(rpath, path);
        prop_assert_eq!(&rbody[..], &body[..]);
    }

    /// The hand-rolled parser only ever returns Ok/Err on arbitrary bytes.
    #[test]
    fn prop_parse_plaintext_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse_plaintext(&bytes);
    }

    /// Manifests serialize/parse round-trip for arbitrary contents.
    #[test]
    fn prop_manifest_json_round_trips(seq in any::<u64>(), end in any::<u64>(), n in 0usize..6) {
        let entries: Vec<ManifestEntry> = (0..n)
            .map(|i| ManifestEntry {
                id: format!("id{i}"),
                path: format!("/p/{i}"),
                size: i as u64,
                offset: i as u64 * 10,
                length: 7,
                uploaded_at: i as i64,
            })
            .collect();
        let m = Manifest { seq, end_offset: end, entries };
        let back: Manifest = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        prop_assert_eq!(m, back);
    }

    /// Scanning arbitrary bytes as a volume never panics/over-allocates.
    #[test]
    fn prop_scan_volume_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let key = fast_key();
        let aad = volume_aad("v", 0);
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = scan_volume(&mut cur, bytes.len() as u64, &key, &aad);
    }
}


// --- mutation-testing kill-tests (round 7: cargo-mutants survivor closure) ---
#[test]
fn mut_const_size_caps_have_exact_values() {
    // Kills the `*` -> `+` mutants on lines 58/62/64: pin the EXACT byte values
    // of the module size caps. With `+`, e.g. MAX_DOC_SIZE would be 64+1024+1024
    // = 3072 instead of 67108864, so each equality below fails under the mutant.
    assert_eq!(MAX_DOC_SIZE, 64 * 1024 * 1024);
    assert_eq!(MAX_DOC_SIZE, 67_108_864u64);
    assert_eq!(DEFAULT_VOLUME_MAX_SIZE, 256 * 1024 * 1024);
    assert_eq!(DEFAULT_VOLUME_MAX_SIZE, 268_435_456u64);
    assert_eq!(MAX_MANIFEST_SIZE, 256 * 1024 * 1024);
    assert_eq!(MAX_MANIFEST_SIZE, 268_435_456u64);
}

#[test]
fn mut_put_rejects_at_doc_size_boundary() {
    // Pins the MAX_DOC_SIZE cap on line 354 (`bytes.len() as u64 > MAX_DOC_SIZE`):
    //   - a doc of EXACTLY MAX_DOC_SIZE must be ACCEPTED  (kills `>` -> `>=`)
    //   - a doc of MAX_DOC_SIZE + 1   must be REJECTED  (kills `>` -> `==`)
    let dir = tmp_dir("dcap");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    // One allocation of cap+1 bytes, reused as both slices.
    let buf = vec![0u8; MAX_DOC_SIZE as usize + 1];
    // Over-cap by one byte: real code rejects with TooLarge before any write.
    assert!(
        matches!(s.put("over", "/o", &buf[..], 1, &key), Err(StorageError::TooLarge)),
        "a document of MAX_DOC_SIZE + 1 must be rejected (kills `>` -> `==`)"
    );
    // Exactly at the cap: real code accepts it (kills `>` -> `>=`).
    s.put("atcap", "/c", &buf[..MAX_DOC_SIZE as usize], 2, &key)
        .expect("a document of exactly MAX_DOC_SIZE must be accepted (kills `>` -> `>=`)");
    assert!(s.contains("atcap"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_load_manifest_truncation_boundary_is_nonce_len() {
    // Pins line 478 (`raw.len() < NONCE_LEN`):
    //   - NONCE_LEN - 1 bytes  -> Corrupt("truncated")   (reject side)
    //   - EXACTLY NONCE_LEN    -> NOT the truncated branch; it proceeds to decrypt
    //     an empty ciphertext, which fails AEAD verification -> Crypto.
    // Under `<` -> `<=`, the NONCE_LEN-exact file would be rejected as Corrupt,
    // so the `Crypto` assertion below would fail. That distinguishes the mutant.
    let dir = tmp_dir("manlen");
    let key = fast_key();
    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    std::fs::create_dir_all(dir.join("manifest")).unwrap();
    let mpath = dir.join("manifest/manifest.0");

    // One byte short of a nonce: truncated on either operator.
    std::fs::write(&mpath, vec![0u8; NONCE_LEN - 1]).unwrap();
    assert!(
        matches!(s.load_manifest(0, &key), Err(StorageError::Corrupt(_))),
        "a sub-nonce manifest is truncated/corrupt"
    );

    // Exactly NONCE_LEN bytes: real code does NOT take the truncated branch; it
    // decrypts an empty ciphertext, which fails -> Crypto (NOT Corrupt).
    std::fs::write(&mpath, vec![0u8; NONCE_LEN]).unwrap();
    assert!(
        matches!(s.load_manifest(0, &key), Err(StorageError::Crypto(_))),
        "a manifest of exactly NONCE_LEN bytes must reach decrypt (kills `<` -> `<=`)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_load_manifest_rejects_oversize_before_read() {
    // Pins line 474 (`meta.len() > MAX_MANIFEST_SIZE`) on the over-cap side:
    // a (sparse) manifest of MAX_MANIFEST_SIZE + 1 bytes is rejected with
    // TooLarge BEFORE the file is read into memory. Under `>` -> `==`, cap+1 is
    // not equal to the cap, so the guard would be false and the file would be
    // read (no TooLarge), failing this assertion.
    let dir = tmp_dir("manbig");
    let key = fast_key();
    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    std::fs::create_dir_all(dir.join("manifest")).unwrap();
    let mpath = dir.join("manifest/manifest.0");
    // Sparse allocation: logical length cap+1, ~no physical bytes / no big read.
    let f = OpenOptions::new().write(true).create(true).truncate(true).open(&mpath).unwrap();
    f.set_len(MAX_MANIFEST_SIZE + 1).unwrap();
    f.sync_all().unwrap();
    drop(f);
    assert!(
        matches!(s.load_manifest(0, &key), Err(StorageError::TooLarge)),
        "an over-cap manifest must be rejected with TooLarge (kills `>` -> `==`)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_open_corrupt_manifest_with_volume_is_rebuilt() {
    // Pins line 196 guard `vpath.exists()` toward `true`: a CORRUPT (Corrupt/
    // Crypto/Json) manifest WITH its volume present must be rebuilt, so open
    // succeeds and the document is recoverable. Under guard -> `false`, the
    // rebuild arm is skipped, the corrupt manifest falls through to the
    // `Err(e) => return Err(e)` arm, and open() fails.
    let dir = tmp_dir("open-corrupt-rebuild");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    // Corrupt the manifest in place; the volume (vol.0) stays intact.
    std::fs::write(dir.join("manifest/manifest.0"), b"garbage").unwrap();
    let s2 = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE)
        .expect("corrupt manifest + present volume must rebuild (kills line 196 guard -> false)");
    assert!(s2.contains("a"), "document recovered by volume rebuild");
    assert_eq!(&*s2.read("a", &key).unwrap(), b"alpha");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_open_corrupt_manifest_without_volume_propagates() {
    // Pins line 196 guard `vpath.exists()` toward `true`: when a manifest is
    // present-but-corrupt and its volume is ABSENT, open must PROPAGATE the
    // corruption error (Corrupt), NOT attempt a rebuild. Under guard -> `true`,
    // the rebuild arm runs, File::open on the missing volume fails, and open
    // returns an Io error instead of Corrupt.
    let dir = tmp_dir("open-corrupt-novol");
    let key = fast_key();
    std::fs::create_dir_all(dir.join("manifest")).unwrap();
    // Sub-nonce garbage -> load_manifest returns Corrupt("truncated").
    std::fs::write(dir.join("manifest/manifest.0"), b"garbage").unwrap();
    // No volume/ dir at all: vpath does not exist.
    // `let-else` instead of `.unwrap_err()` (VolumeStore isn't Debug).
    let Err(err) = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE) else {
        panic!("a corrupt manifest with no volume must error, not rebuild (kills line 196 guard -> true)");
    };
    assert!(
        matches!(err, StorageError::Corrupt(_)),
        "a corrupt manifest with no volume must propagate Corrupt, not rebuild; got {err:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn mut_open_non_corruption_error_with_present_manifest_is_not_rebuilt() {
    // Pins line 206 (`vpath.exists() && !mpath.exists()`): a manifest that is
    // PRESENT but fails load for a NON-corruption reason (an I/O error, here a
    // path that is a directory -> EISDIR on read) must NOT be silently rebuilt
    // over via a lossy volume scan; the error is propagated and open() fails.
    // Under `&&` -> `||` (or the guard -> true), `vpath.exists() || !mpath.exists()`
    // is true (the volume is present), the manifest is rebuilt, and open()
    // succeeds instead — which this test detects as a failure.
    let dir = tmp_dir("open-iofail");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"alpha", 1, &key).unwrap(); // valid vol.0 + manifest.0
    // Replace manifest.0 (a file) with a directory: it still "exists" (mpath
    // present) but fs::read on it errors with Io, not a corruption variant.
    let mpath = dir.join("manifest/manifest.0");
    std::fs::remove_file(&mpath).unwrap();
    std::fs::create_dir(&mpath).unwrap();
    assert!(mpath.exists(), "manifest path still present (as a directory)");
    assert!(dir.join("volume/vol.0").exists(), "volume present");
    assert!(
        VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).is_err(),
        "a present manifest with a non-corruption (I/O) error must propagate, not rebuild \
         (kills line 206 `&&` -> `||` and the guard -> true)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_read_frame_at_eof_and_overrun_bounds() {
    // Pins the EOF bounds in read_frame_at against three mutation clusters:
    //   * line 632 `end > file_len`  -> `==` / `>=`  (the 4-byte prefix bound)
    //   * line 647 `end > file_len`  -> `==` / `>=`  (the whole-frame bound)
    //   * line 646 `+ frame_len`     -> `-` / `*` / `/` (the overrun offset math)
    // Strategy: build one genuine, fully-valid frame, then probe file_len at the
    // exact byte boundaries so a one-step operator change flips parse<->reject.
    let dir = tmp_dir("mut-eof");
    let key = fast_key();
    let aad = volume_aad("v", 0);
    let frame = encode_frame(&key, "v", 0, "id", "/p", b"payload").unwrap();
    let vol = dir.join("vol");
    append_frame(&vol, 0, &frame).unwrap();
    let mut f = File::open(&vol).unwrap();
    let total = f.metadata().unwrap().len(); // == 4 + frame_len, the full frame size

    // (1) file_len EXACTLY equal to the frame size must PARSE. Kills line 647
    //     `>` -> `>=`/`==` (they would reject at `end == file_len`) and line 646
    //     `+` -> `-` (underflow -> None -> reject) and `+` -> `*` (4*frame_len
    //     overshoots `total` -> reject). All of those would turn this Ok into Err.
    let (id, path, body) = read_frame_at(&mut f, total, 0, 0, &key, &aad).unwrap();
    assert_eq!((id.as_str(), path.as_str(), &body[..]), ("id", "/p", &b"payload"[..]));

    // (2) file_len one byte SHORT must be rejected as an overrun. The real file
    //     still holds every byte, so only the bound (not read_exact) can catch it.
    //     Kills line 646 `+` -> `/`: `4 / frame_len == 0 <= total-1` would wrongly
    //     pass the check, then read_exact would succeed and the frame would decrypt.
    match read_frame_at(&mut f, total - 1, 0, 0, &key, &aad) {
        Err(StorageError::Corrupt(m)) => assert_eq!(m, "frame overruns EOF", "one byte short overruns"),
        other => panic!("expected Corrupt overrun at total-1, got {other:?}"),
    }

    // (3) file_len == 4 (only the prefix is in bounds). The real code does NOT
    //     reject at line 632 (`4 > 4` is false): it reads the prefix, then the
    //     whole-frame check at 644 rejects with "frame overruns EOF". A `>=`/`==`
    //     mutation at line 632 would instead reject early with the DIFFERENT
    //     message "frame offset past EOF", so the message pins the operator.
    match read_frame_at(&mut f, 4, 0, 0, &key, &aad) {
        Err(StorageError::Corrupt(m)) => assert_eq!(m, "frame overruns EOF", "prefix bound is `>`, not `>=`/`==`"),
        other => panic!("expected Corrupt overrun at file_len=4, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_read_frame_at_lower_plausibility_bound() {
    // Pins line 641 col 18 `frame_len < NONCE_LEN` -> `<=`. At the exact boundary
    // frame_len == NONCE_LEN the real code must NOT reject as "implausible"; it
    // proceeds (empty ciphertext) and fails later in AEAD decryption. A `<=`
    // mutant would short-circuit with "implausible frame length" at the boundary.
    let dir = tmp_dir("mut-lower");
    let key = fast_key();
    let aad = volume_aad("v", 0);
    let vol = dir.join("vol");
    // frame_len == NONCE_LEN (24): a body of exactly the nonce, zero ciphertext.
    let mut raw = (NONCE_LEN as u32).to_le_bytes().to_vec();
    raw.extend_from_slice(&[0u8; NONCE_LEN]);
    std::fs::write(&vol, &raw).unwrap();
    let mut f = File::open(&vol).unwrap();
    let len = f.metadata().unwrap().len(); // == 4 + NONCE_LEN, so no overrun
    match read_frame_at(&mut f, len, 0, 0, &key, &aad) {
        // Real behaviour: reaches decrypt (empty ct) and fails there, NOT at the
        // plausibility check.
        Err(StorageError::Corrupt(m)) => {
            assert_ne!(m, "implausible frame length", "frame_len == NONCE_LEN is the inclusive lower edge")
        }
        Err(_) => {} // a non-Corrupt error (e.g. Crypto) is exactly the real path
        Ok(_) => panic!("frame_len == NONCE_LEN has empty ciphertext and must not parse"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mut_read_frame_at_upper_plausibility_bound() {
    // Pins line 641 col 50 `frame_len > MAX_DOC_SIZE + 4096` -> `==`/`>=` and
    // col 65 the `MAX_DOC_SIZE + 4096` addend -> `-`/`*`. Each probe writes only a
    // 4-byte length prefix and calls with file_len = 4, so whichever branch does
    // NOT reject at 641 instead rejects at the overrun check (644) -- no huge
    // allocation is ever reached. The resulting Corrupt message names the branch.
    let dir = tmp_dir("mut-upper");
    let key = fast_key();
    let aad = volume_aad("v", 0);
    let probe = |frame_len: u64| -> StorageError {
        let vol = dir.join(format!("vol-{frame_len}"));
        std::fs::write(&vol, (frame_len as u32).to_le_bytes()).unwrap();
        let mut f = File::open(&vol).unwrap();
        read_frame_at(&mut f, 4, 0, 0, &key, &aad).expect_err("4-byte file cannot hold a frame body")
    };
    let msg = |e: StorageError| match e {
        StorageError::Corrupt(m) => m,
        other => panic!("expected Corrupt, got {other:?}"),
    };

    // (a) frame_len == real bound: NOT "implausible" (real uses strict `>`), so it
    //     falls through to the overrun check. `>=`/`==` mutants would reject here
    //     with "implausible frame length"; the `+`->`-` mutant lowers the bound so
    //     a value == real bound would also become "implausible".
    assert_eq!(msg(probe(MAX_DOC_SIZE + 4096)), "frame overruns EOF", "real upper edge is exclusive");

    // (b) frame_len == MAX_DOC_SIZE: below the real bound (not implausible), but a
    //     `+`->`-` mutant (bound = MAX_DOC_SIZE - 4096) would flag it implausible.
    assert_eq!(msg(probe(MAX_DOC_SIZE)), "frame overruns EOF", "addend is `+`, not `-`");

    // (c) frame_len far above the real bound but far below MAX_DOC_SIZE*4096: the
    //     real code rejects it as implausible. A `+`->`*` mutant inflates the bound
    //     so high that this value passes 641 and only trips the overrun check.
    assert_eq!(msg(probe(100_000_000)), "implausible frame length", "addend is `+`, not `*`");
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn mut_write_atomic_puts_temp_in_target_dir() {
    // Pins line 751 `path.parent().filter(|p| !p.as_os_str().is_empty())`: deleting
    // the `!` makes `dir = None` for a normal path, so the temp is created in CWD
    // instead of beside the target. We make the target's parent dir read-only and
    // arm the `atomic.write` fault point (which fires only AFTER the temp is opened):
    //   * real (`!`): temp is `parent/.name.tmp` in a read-only dir -> create_new
    //     open fails with EACCES BEFORE the fault point -> the injected ENOSPC is
    //     never produced.
    //   * mutant (no `!`): temp is `.name.tmp` in (writable) CWD -> open succeeds ->
    //     the fault point fires -> error message is the injected ENOSPC.
    use std::os::unix::fs::PermissionsExt;
    let base = tmp_dir("mut-atomic");
    let target = base.join("manifest.x");
    crate::fault::fail_at("atomic.write", 1);
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o500)).unwrap();
    let err = write_atomic(&target, b"payload").expect_err("write_atomic must fail here");
    // Restore perms so cleanup (and any later test on this thread) is unaffected.
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::fault::clear();
    // Real code never reached the fault point (open failed in the read-only dir).
    // The mutant did reach it, so it would carry the injected-ENOSPC text.
    assert!(matches!(err, StorageError::Io(_)), "expected an I/O error, got {err:?}");
    assert!(
        !format!("{err}").contains("injected"),
        "temp must be created in the (read-only) target dir, not CWD: got {err}"
    );
    std::fs::remove_dir_all(&base).ok();
}

// Kills: storage.rs harden_file_fd body -> ().
// The volume append path opens vol.<N> with create-time mode 0600, so a freshly
// created file would be 0600 even with the mutant. We therefore call harden_file_fd
// directly on a file we deliberately created at 0644: only its set_permissions call
// can pull it to 0600. With the body replaced by `()`, the file stays 0644.
#[cfg(unix)]
#[test]
fn mut_harden_file_chmods_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("hardenfile");
    let path = dir.join("blob.bin");
    fs::write(&path, b"x").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o644);
    fs::set_permissions(&path, p).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);

    let f = OpenOptions::new().write(true).open(&path).unwrap();
    harden_file_fd(&f);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "harden_file_fd must chmod the file to 0600"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Byte offsets of every frame in partition 0, in order, plus the file length — so a
/// test can aim damage at a frame's middle or exactly across a frame boundary.
fn frame_offsets(dir: &Path, key: &Key, vault_id: &str) -> (Vec<(u64, u64)>, u64) {
    let path = dir.join("volume").join("vol.0");
    let len = std::fs::metadata(&path).unwrap().len();
    let mut f = File::open(&path).unwrap();
    let m = scan_volume(&mut f, len, key, &volume_aad(vault_id, 0));
    (m.entries.iter().map(|e| (e.offset, e.length)).collect(), len)
}

/// Overwrite `len` bytes at `at` in partition 0's volume with garbage — simulating rot,
/// a torn write, or a bad sector.
fn corrupt_volume(dir: &Path, at: u64, len: usize) {
    let path = dir.join("volume").join("vol.0");
    let mut f = OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(&vec![0xEE; len]).unwrap();
    f.sync_all().unwrap();
}

/// Damage INSIDE one document's frame, with the manifest intact.
///
/// The manifest gives every document an exact offset and length, so the store never has
/// to guess where anything lives: the damaged document fails its authentication check
/// and reports corrupt, and every other document reads normally. Nothing else is lost —
/// the manifest itself is untouched, so all four documents stay indexed and the vault
/// still opens.
#[test]
fn corruption_inside_one_frame_costs_only_that_document() {
    let dir = tmp_dir("rot-in-frame");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for (id, body) in [("a", b'A'), ("b", b'B'), ("c", b'C'), ("d", b'D')] {
        s.put(id, &format!("/{id}"), &vec![body; 500], 1, &key).unwrap();
    }
    drop(s);

    // Aim at the middle of the SECOND frame's ciphertext.
    let (frames, _) = frame_offsets(&dir, &key, "v");
    let (off, len) = frames[1];
    corrupt_volume(&dir, off + len / 2, 32);

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(s.ids().count(), 4, "the manifest is intact, so every document is still indexed");
    assert!(s.read("b", &key).is_err(), "the damaged document fails its authentication check");
    for id in ["a", "c", "d"] {
        assert_eq!(s.read(id, &key).unwrap().len(), 500, "{id} is unaffected by damage to another frame");
    }
}

/// Damage SPANNING the boundary between two frames, with the manifest intact.
///
/// Both frames the damage touches are lost — one loses ciphertext, the next loses the
/// length prefix that says how big it is — but the manifest's stored offsets mean the
/// frames after them are still addressed exactly, so they read normally.
#[test]
fn corruption_across_a_frame_boundary_costs_only_the_two_frames_it_touches() {
    let dir = tmp_dir("rot-boundary");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for (id, body) in [("a", b'A'), ("b", b'B'), ("c", b'C'), ("d", b'D')] {
        s.put(id, &format!("/{id}"), &vec![body; 500], 1, &key).unwrap();
    }
    drop(s);

    // Straddle the b/c boundary: the last bytes of b's frame and the first of c's
    // (c's 4-byte length prefix included).
    let (frames, _) = frame_offsets(&dir, &key, "v");
    let boundary = frames[1].0 + frames[1].1;
    assert_eq!(boundary, frames[2].0, "frames are contiguous, so this really is the boundary");
    corrupt_volume(&dir, boundary - 16, 32);

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(s.ids().count(), 4, "the manifest is intact, so every document is still indexed");
    assert!(s.read("b", &key).is_err(), "the frame whose tail was hit is unreadable");
    assert!(s.read("c", &key).is_err(), "the frame whose head was hit is unreadable");
    assert_eq!(s.read("a", &key).unwrap().len(), 500, "the frame before the damage is fine");
    assert_eq!(s.read("d", &key).unwrap().len(), 500, "the frame after the damage is fine");
}

/// The same in-frame damage with the manifest ALSO lost — the case that has to fall back
/// to scanning the volume. The scan must step over the damaged frame and keep going:
/// before it did, one rotted frame discarded every LATER frame too, and any record still
/// pointing at one of them made the whole vault refuse to open.
#[test]
fn rebuild_steps_over_a_damaged_frame_instead_of_stopping() {
    let dir = tmp_dir("rebuild-in-frame");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for (id, body) in [("a", b'A'), ("b", b'B'), ("c", b'C'), ("d", b'D')] {
        s.put(id, &format!("/{id}"), &vec![body; 500], 1, &key).unwrap();
    }
    drop(s);

    let (frames, _) = frame_offsets(&dir, &key, "v");
    let (off, len) = frames[1];
    corrupt_volume(&dir, off + len / 2, 32);
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    let ids: Vec<&str> = s.ids().collect();
    assert!(!ids.contains(&"b"), "the damaged frame cannot be recovered: {ids:?}");
    for id in ["a", "c", "d"] {
        assert!(ids.contains(&id), "{id} must survive damage to an unrelated frame: {ids:?}");
        assert_eq!(s.read(id, &key).unwrap().len(), 500);
    }
}

/// Rebuilding when the damage spans a frame BOUNDARY — the harder resync. The next
/// frame's own length prefix is part of the damage, so stepping over it by length is
/// impossible; the scan has to find the following frame by looking for one that
/// authenticates.
#[test]
fn rebuild_resyncs_after_damage_spanning_two_frames() {
    let dir = tmp_dir("rebuild-boundary");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    for (id, body) in [("a", b'A'), ("b", b'B'), ("c", b'C'), ("d", b'D')] {
        s.put(id, &format!("/{id}"), &vec![body; 500], 1, &key).unwrap();
    }
    drop(s);

    let (frames, _) = frame_offsets(&dir, &key, "v");
    let boundary = frames[1].0 + frames[1].1;
    corrupt_volume(&dir, boundary - 16, 32);
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    let ids: Vec<&str> = s.ids().collect();
    assert!(!ids.contains(&"b") && !ids.contains(&"c"), "both damaged frames are gone: {ids:?}");
    assert!(ids.contains(&"a"), "the frame before the damage survives: {ids:?}");
    assert!(ids.contains(&"d"), "the scan resynchronised past the damage: {ids:?}");
    assert_eq!(s.read("d", &key).unwrap().len(), 500, "and the recovered entry really reads");
}

/// With redundancy on, a manifest that is lost or damaged is recovered from its spare
/// copy — which holds the SAME entries, so nothing is lost at all. That is the whole
/// point of keeping it: the fallback (scanning the volume) can only index frames it can
/// still read, so it would have dropped the document sitting in the damaged frame.
#[test]
fn a_lost_manifest_is_recovered_from_its_spare_not_a_lossy_scan() {
    let dir = tmp_dir("manifest-spare");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.set_redundancy(1);
    for (id, body) in [("a", b'A'), ("b", b'B'), ("c", b'C')] {
        s.put(id, &format!("/{id}"), &vec![body; 500], 1, &key).unwrap();
    }
    drop(s);
    assert!(dir.join("manifest").join("manifest.0.mirror").exists(), "the spare is written beside the manifest");

    // Lose the manifest AND damage a frame, so a scan-based rebuild would drop "b".
    let (frames, _) = frame_offsets(&dir, &key, "v");
    corrupt_volume(&dir, frames[1].0 + frames[1].1 / 2, 32);
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    let ids: Vec<&str> = s.ids().collect();
    assert_eq!(ids.len(), 3, "the spare restored every entry, including the damaged one: {ids:?}");
    assert!(s.read("b", &key).is_err(), "its bytes are still damaged — the index is what was recovered");
    assert_eq!(s.read("a", &key).unwrap().len(), 500);
    assert_eq!(s.read("c", &key).unwrap().len(), 500);
}

/// Turning redundancy off removes the spares, so the setting genuinely stops leaving
/// extra encrypted copies of the index on disk.
#[test]
fn turning_redundancy_off_removes_the_manifest_spares() {
    let dir = tmp_dir("manifest-spare-off");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.set_redundancy(2);
    s.put("a", "/a", b"body", 1, &key).unwrap();
    let mirror = dir.join("manifest").join("manifest.0.mirror");
    assert!(mirror.exists());
    // Recording the depth is side-effect-free (audit 2026-08-03 A-1) — the next manifest
    // commit is what clears the spare, and later writes never bring it back.
    s.set_redundancy(0);
    s.put("b", "/b", b"body", 1, &key).unwrap();
    assert!(!mirror.exists(), "the next commit clears the spare once redundancy is off");
    s.put("c", "/c", b"body", 1, &key).unwrap();
    assert!(!mirror.exists(), "and later writes do not bring it back");
}

/// A spare manifest that LAGS the volume must not cost the newer documents.
///
/// The live manifest is committed first and the spare written after it, so a failure in
/// that gap leaves a spare describing one commit ago while the volume already holds the
/// newer frames. Recovering from it verbatim would drop those documents from the index —
/// and worse, its short `end_offset` is the next append point, so the following upload
/// would overwrite frames that are still perfectly good. The region the spare does not
/// cover is scanned and merged instead.
#[test]
fn a_stale_spare_manifest_is_topped_up_from_the_volume() {
    let dir = tmp_dir("manifest-spare-stale");
    let key = fast_key();
    let mirror = dir.join("manifest").join("manifest.0.mirror");

    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.set_redundancy(1);
    s.put("a", "/a", b"alpha", 1, &key).unwrap();
    // Keep the spare as it stands now — this is the "spare write failed" state.
    let stale = std::fs::read(&mirror).unwrap();
    s.put("b", "/b", b"bravo", 2, &key).unwrap();
    s.put("c", "/c", b"charlie", 3, &key).unwrap();
    let true_end = s.manifests[0].end_offset;
    drop(s);

    // Live manifest lost, spare frozen at the state after "a" only.
    std::fs::write(&mirror, &stale).unwrap();
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    let ids: Vec<&str> = s.ids().collect();
    assert_eq!(ids.len(), 3, "the frames the stale spare did not know about were merged in: {ids:?}");
    assert_eq!(&*s.read("b", &key).unwrap(), b"bravo");
    assert_eq!(&*s.read("c", &key).unwrap(), b"charlie");
    assert_eq!(
        s.manifests[0].end_offset, true_end,
        "the append point must land past the newest frame, never inside it"
    );
}

/// The same lag, but the newer frame is an UPDATE of a document the stale spare already
/// lists: the merged entry must be the newer frame, not the one the spare remembers.
#[test]
fn a_stale_spare_manifest_takes_the_newer_frame_for_a_repeated_id() {
    let dir = tmp_dir("manifest-spare-stale-update");
    let key = fast_key();
    let mirror = dir.join("manifest").join("manifest.0.mirror");

    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.set_redundancy(1);
    s.put("a", "/a", b"first-version", 1, &key).unwrap();
    let stale = std::fs::read(&mirror).unwrap();
    s.put("a", "/a", b"second-version", 2, &key).unwrap(); // appends a newer frame
    drop(s);

    std::fs::write(&mirror, &stale).unwrap();
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    assert_eq!(s.ids().count(), 1, "one id, not two entries for it");
    assert_eq!(&*s.read("a", &key).unwrap(), b"second-version", "the newer frame wins");
}

/// AUDIT 2026-08-03 A-2: a resync must not read unboundedly.
///
/// The attempt cap bounds how MANY decrypt attempts a damaged region costs, but not how
/// big each one is, and a frame may legitimately be up to `MAX_DOC_SIZE`. A volume whose
/// bytes all decode as plausible large lengths — crafted, or a big file damaged across a
/// wide region — cost 128 attempts of up to 64 MiB each, ~8 GiB of reads per damaged
/// region (measured: 3.3 s for a 64 MiB volume, growing with the file).
/// `RESYNC_READ_BUDGET` bounds the bytes instead.
///
/// Asserted by COUNTING the bytes the scan reads rather than by timing it: the bound is
/// a property of the code, and a wall-clock assertion would really be measuring the disk
/// and whatever else the machine is running.
#[test]
fn a_crafted_volume_cannot_make_a_rebuild_read_unboundedly() {
    /// A `Read + Seek` that tallies every byte handed out.
    struct Counting {
        inner: std::io::Cursor<Vec<u8>>,
        bytes: u64,
    }
    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes += n as u64;
            Ok(n)
        }
    }
    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    // 64 MiB where every 4-byte window decodes to a large, in-range, in-file frame
    // length (0x00300000 = 3 MiB), so every candidate offset looks plausible and each
    // decrypt attempt must read 3 MiB before the AEAD rejects it.
    let mut buf = Vec::with_capacity(64 * 1024 * 1024);
    while buf.len() < 64 * 1024 * 1024 {
        buf.extend_from_slice(&[0x00, 0x00, 0x30, 0x00]);
    }
    let len = buf.len() as u64;
    let mut f = Counting { inner: std::io::Cursor::new(buf), bytes: 0 };
    let key = fast_key();
    let m = scan_volume(&mut f, len, &key, &volume_aad("v", 0));

    assert!(m.entries.is_empty(), "nothing in this volume authenticates");
    // Budget, plus one frame of overshoot (the in-flight attempt is always allowed to
    // finish), plus the 64 KiB window read itself and some slack for prefix reads.
    let ceiling = RESYNC_READ_BUDGET + MAX_DOC_SIZE + 1024 * 1024;
    assert!(
        f.bytes < ceiling,
        "a crafted volume made the rebuild read {} bytes (ceiling {ceiling})",
        f.bytes
    );
}

/// The budget must not cost a legitimate large document its recovery: damage that takes
/// out a frame's length prefix, followed by a genuinely big frame, must still resync.
#[test]
fn the_resync_budget_still_recovers_a_large_document() {
    let dir = tmp_dir("resync-budget-large");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", u64::MAX).unwrap();
    s.put("small", "/small", b"x", 1, &key).unwrap();
    let big = vec![0xC3u8; 12 * 1024 * 1024]; // comfortably past RESYNC_READ_BUDGET
    s.put("big", "/big", &big, 2, &key).unwrap();
    let frames: Vec<(u64, u64)> = s.manifests[0].entries.iter().map(|e| (e.offset, e.length)).collect();
    drop(s);

    // Damage the SMALL frame's length prefix, so the fast path (step over the damaged
    // frame by its own length) has nothing usable and the byte-wise walk has to find
    // the large frame that follows. A frame whose own prefix is destroyed is gone for
    // good — the scan cannot know its size — so the damage goes on the small one.
    corrupt_volume(&dir, frames[0].0, 8);
    std::fs::remove_file(dir.join("manifest").join("manifest.0")).unwrap();

    let s = VolumeStore::open(&dir, &key, "v", u64::MAX).unwrap();
    let ids: Vec<&str> = s.ids().collect();
    assert!(ids.contains(&"big"), "a large frame after the damage must still be found: {ids:?}");
    assert_eq!(s.read("big", &key).unwrap().len(), 12 * 1024 * 1024);
    std::fs::remove_dir_all(&dir).ok();
}

/// AUDIT 2026-08-03 A-1: the spare copies are removed only by an explicit call, so
/// `set_redundancy` cannot delete anything — which is what makes it safe for
/// `open_inner` to call on a read-only open.
#[test]
fn setting_the_redundancy_depth_never_deletes_anything() {
    let dir = tmp_dir("redundancy-no-side-effect");
    let key = fast_key();
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.set_redundancy(1);
    s.put("a", "/a", b"body", 1, &key).unwrap();
    let mirror = dir.join("manifest").join("manifest.0.mirror");
    assert!(mirror.exists());
    s.set_redundancy(0);
    assert!(mirror.exists(), "recording the depth must not touch the disk");
    s.drop_manifest_mirrors();
    assert!(!mirror.exists(), "the explicit call is what removes them");
    std::fs::remove_dir_all(&dir).ok();
}

/// AUDIT 2026-08-03 A-5 (kill-test): `refresh_manifest_mirrors` had no coverage at all —
/// `cargo mutants` replaced its whole body with `()`, and flipped its `redundancy == 0`
/// guard to `!=`, and the suite stayed green both times. It is the path that restores the
/// spares after a rekey or a compaction, so "it silently does nothing" is exactly the
/// failure that would leave a vault without the protection its settings claim.
#[test]
fn refreshing_the_spares_covers_partitions_that_already_exist() {
    let dir = tmp_dir("refresh-mirrors");
    let key = fast_key();
    let mirror = dir.join("manifest").join("manifest.0.mirror");

    // Redundancy off: a write leaves no spare.
    let mut s = VolumeStore::open(&dir, &key, "v", DEFAULT_VOLUME_MAX_SIZE).unwrap();
    s.put("a", "/a", b"body", 1, &key).unwrap();
    assert!(!mirror.exists(), "no spare while redundancy is off");

    // Turning it on and refreshing writes spares for partitions that ALREADY exist,
    // without waiting for the next document write.
    s.set_redundancy(1);
    s.refresh_manifest_mirrors(&key);
    assert!(mirror.exists(), "refresh must write the spare for an existing partition");

    // And at depth 0 it must write nothing — the guard, in the other direction.
    std::fs::remove_file(&mirror).unwrap();
    s.set_redundancy(0);
    s.refresh_manifest_mirrors(&key);
    assert!(!mirror.exists(), "refresh at depth 0 must write nothing");
    std::fs::remove_dir_all(&dir).ok();
}
