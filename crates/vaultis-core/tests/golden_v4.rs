//! Backward-compatibility GOLDEN fixture.
//!
//! Every other on-disk-format test constructs the vault at runtime with the CURRENT
//! code, so the reader and writer drift together: a silent change to the binary layout
//! (field order, endianness, header offsets, an AAD prefix string) still self-round-trips
//! and passes them all — while making every PREVIOUSLY-saved vault permanently unreadable.
//!
//! This test instead opens a real v4 vault committed as FROZEN BYTES under
//! `tests/fixtures/golden_v4/` (built once by an earlier build), proving today's reader
//! still opens vaults written by older code, and pins the deterministic header prefix.
//! If this test ever fails, the on-disk format changed in a vault-bricking way: it must
//! be paired with a `FORMAT_VERSION` bump + migration and a regenerated fixture — never a
//! silent edit to "make the test pass".

use std::path::{Path, PathBuf};

use vaultis_core::vault::OpenVault;

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn tmp(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "pmgold-{tag}-{n}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn committed_v4_vault_still_opens_and_header_prefix_is_pinned() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden_v4");

    // (1) Pin the DETERMINISTIC header prefix: MAGIC | version=4 | m/t/p_cost (LE u32).
    // Bytes 21..61 (salt + nonce) are random per write and intentionally excluded.
    let raw = std::fs::read(fixture.join("vault.pmv")).expect("golden fixture present");
    let prefix: String = raw[0..21].iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        prefix, "504d5641554c540004000100000100000001000000",
        "on-disk header layout (magic/version/Argon2 params) changed — existing vaults may not open"
    );

    // (2) Open the FROZEN bytes (read-only writes nothing) from a temp copy, so the
    // committed fixture is never mutated, and verify the decrypted contents + a blob.
    let work = tmp("open");
    copy_dir(&fixture, &work);
    let ov = OpenVault::open_read_only(work.join("vault.pmv"), b"golden-pw-one", b"golden-pw-two")
        .expect("a v4 vault written by an earlier build must still open with today's reader");

    let acc = ov
        .vault
        .accounts
        .iter()
        .find(|a| a.id == "golden-account-id")
        .expect("the golden account survives the format");
    assert_eq!(acc.username, "golden-user");
    assert_eq!(acc.password, "golden-secret", "the encrypted account password still decrypts verbatim");

    let tw = ov
        .vault
        .trust_wills
        .iter()
        .find(|t| t.id == "golden-tw-id")
        .expect("the golden trust-will survives the format");
    let blob = tw.file.clone().expect("the trust-will's attached blob id");
    assert_eq!(
        &**ov.read_document(&blob).unwrap(),
        &b"golden-document-bytes"[..],
        "the committed volume frame still decrypts to the original document bytes"
    );

    drop(ov);
    std::fs::remove_dir_all(&work).ok();
}

/// The upgrade path a real user takes: the committed fixture was written by an earlier
/// release, so it carries no `written_by` stamp. The FIRST writable open by this release
/// must copy it aside — byte for byte, as the old release left it — before writing, then
/// stamp it; the next writable open copies nothing. Proves the safety copy works on a vault
/// this release did not create, which no runtime-built test can.
#[test]
fn an_old_release_vault_gets_one_safety_copy_on_its_first_writable_open() {
    use vaultis_core::vault::{APP_VERSION, SAFETY_DIR};
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden_v4");
    let root = tmp("upgrade");
    let work = root.join("golden");
    copy_dir(&fixture, &work);
    let original = std::fs::read(work.join("vault.pmv")).unwrap();

    let ov = OpenVault::open(work.join("vault.pmv"), b"golden-pw-one", b"golden-pw-two")
        .expect("an old vault must open for writing with today's code");
    let copy = ov.safety_copy().expect("an unstamped (older) vault is copied first").to_path_buf();
    assert_eq!(ov.vault.written_by.as_deref(), Some(APP_VERSION));
    drop(ov);

    assert!(copy.starts_with(root.join(SAFETY_DIR)), "{}", copy.display());
    let folder = copy.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert!(folder.starts_with(&format!("golden@before-v{APP_VERSION}@")), "{folder}");
    assert_eq!(std::fs::read(&copy).unwrap(), original, "the copy is the old release's bytes");
    let old = OpenVault::open_read_only(copy, b"golden-pw-one", b"golden-pw-two").expect("the copy opens");
    assert!(old.vault.written_by.is_none(), "the copy keeps the old, unstamped state");
    drop(old);

    let ov = OpenVault::open(work.join("vault.pmv"), b"golden-pw-one", b"golden-pw-two").unwrap();
    assert!(ov.safety_copy().is_none(), "already stamped by this release: no second copy");
    drop(ov);
    assert_eq!(std::fs::read_dir(root.join(SAFETY_DIR)).unwrap().count(), 1);
    std::fs::remove_dir_all(&root).ok();
}
