//! Does a dropped secret actually leave the process's memory?
//!
//! Everything else in this suite verifies zeroization *by construction* — the types
//! derive `ZeroizeOnDrop`, the key is `Zeroizing`, mutation testing confirms the
//! destructors are reachable. None of that observes the bytes. `HARDENING.md` says so
//! explicitly, recording `Key::drop` as an accepted mutation survivor because
//! "zeroize-on-drop can't be observed by a safe test".
//!
//! It can. Not from *inside* the process that owns the secret — the needle you search
//! with is itself a copy — but from the parent of a child that owned it. This test
//! re-executes its own test binary as a child, has the child build a vault whose master
//! password AND one account password are a run-unique sentinel, and then reads the
//! child's `/proc/<pid>/mem` from the parent and counts occurrences of that sentinel.
//! The parent never holds the child's secret in a form the child could have leaked; it
//! *derives* the same sentinel from a seed it passed in, and the seed is a strict
//! substring of the sentinel, so the seed sitting in the child's `environ` can never
//! match the needle.
//!
//! Three child modes, and the two controls are what make the third meaningful:
//!
//! * `none` — derive the sentinel, wipe it, touch no vault. Sentinel must be **gone**.
//!   If it is not, this harness leaks its own needle and every other result is noise.
//! * `hold` — build the vault and keep it **open**. Sentinel must be **found**. If it is
//!   not, the scanner cannot see a live secret and a clean `drop` result proves nothing.
//! * `drop` — build the vault, save it, drop it, wipe our own copies. This is the
//!   measurement. Sentinel must be **gone**.
//!
//! Linux-only: it reads `/proc`. `ptrace_scope=1` (the common default) permits a parent
//! to read a direct child's memory, which is exactly the relationship used here.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use vaultis_core::crypto::KdfParams;
use vaultis_core::records::{self, Account};
use vaultis_core::vault::OpenVault;
use zeroize::Zeroize;

/// Set on the child; absent in the parent. Its value is the mode.
const MODE_ENV: &str = "VAULTIS_RESIDUE_MODE";
/// The seed the sentinel is derived from. Deliberately NOT the sentinel itself, so the
/// child's `environ` (which the scan reads like any other memory) cannot match.
const SEED_ENV: &str = "VAULTIS_RESIDUE_SEED";
/// Long enough that a chance hit in 100s of MiB is not a thing, and distinctive enough
/// to grep for by hand when something does survive.
const PREFIX: &str = "vaultisResidueSentinel";
/// A SECOND, independent sentinel used only as a record field, so a survivor can be
/// attributed: the master-password path and the record/serialize path are different code
/// with different lifetimes, and "3 copies survived" is not actionable until you know
/// which of the two produced them.
const PREFIX_REC: &str = "vaultisResidueRecordfld";

/// The sentinel for a seed. Built into an exactly-sized buffer rather than with
/// `format!`, so the *harness* does not scatter reallocated copies of it across the heap
/// and fail its own `none` control.
fn sentinel(seed: &str) -> String {
    with_prefix(PREFIX, seed)
}

/// The record-field sentinel for a seed.
fn sentinel_rec(seed: &str) -> String {
    with_prefix(PREFIX_REC, seed)
}

fn with_prefix(prefix: &str, seed: &str) -> String {
    let mut s = String::with_capacity(prefix.len() + seed.len());
    s.push_str(prefix);
    s.push_str(seed);
    s
}

#[test]
fn secrets_do_not_survive_in_process_memory_after_the_vault_is_dropped() {
    // Re-exec'd child? Then be the child. (Both roles live in one test binary because a
    // dev-dependency helper binary would not be built by `cargo test`.)
    if let Ok(mode) = std::env::var(MODE_ENV) {
        let seed = std::env::var(SEED_ENV).expect("child needs a seed");
        child(&mode, &seed);
        return;
    }

    let (none_pw, none_rec) = measure("none");
    assert_eq!(
        (none_pw, none_rec),
        (0, 0),
        "CONTROL FAILED: this harness leaked its own sentinels without ever touching a vault. \
         Every other number in this test is noise until that is fixed."
    );

    let (hold_pw, hold_rec) = measure("hold");
    assert!(
        hold_pw > 0 && hold_rec > 0,
        "CONTROL FAILED: the scanner could not find sentinels that are still LIVE in an open \
         vault (master-password {hold_pw}, record-field {hold_rec}). A clean `drop` result would \
         prove nothing, so this is a broken test, not a pass."
    );

    // Staged, so a survivor is attributable to a stage rather than to "somewhere in
    // create -> save -> drop". Each stage drops everything before being photographed.
    let (kdf_pw, _) = measure("kdf"); // the two-password Argon2id derivation, alone
    let (create_pw, create_rec) = measure("create"); // master password only, no record, no save
    let (record_pw, record_rec) = measure("record"); // + the record, still no save
    let (save_pw, save_rec) = measure("drop"); // + save(): serialize -> encrypt -> write

    assert_eq!(
        (kdf_pw, create_pw, create_rec, record_pw, record_rec, save_pw, save_rec),
        (0, 0, 0, 0, 0, 0, 0),
        "un-zeroized plaintext survived the drop (live control: master-password {hold_pw}, \
         record-field {hold_rec}).\n  kdf:    pw={kdf_pw}\n  create: pw={create_pw} rec={create_rec}\n  record: \
         pw={record_pw} rec={record_rec}\n  save:   pw={save_pw} rec={save_rec}\nThe first \
         stage with a non-zero count is the one that keeps the copy."
    );
}

/// Run one child in `mode` and return how many times its sentinel appears in its memory.
fn measure(mode: &str) -> (usize, usize) {
    let seed = format!("{:016x}{:08x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64, std::process::id());
    let needle = sentinel(&seed);
    let needle_rec = sentinel_rec(&seed);

    let mut ch = spawn(mode, &seed);
    // The child prints READY once it is in the state we want to photograph, then blocks
    // on stdin so the state cannot move under us while we read it. libtest writes its own
    // banner ("running 1 test", blank lines) to the same stdout, so scan for the marker
    // rather than assuming it lands first.
    let out = BufReader::new(ch.stdout.take().expect("child stdout"));
    let mut ready = false;
    for line in out.lines().map_while(Result::ok) {
        if line.starts_with("READY") {
            ready = true;
            break;
        }
    }
    assert!(ready, "child never reached the READY state in mode {mode}");

    let (hits, hits_rec, scanned) =
        count_in_process_memory(ch.id(), needle.as_bytes(), needle_rec.as_bytes());
    // Printed (visible under --nocapture) because the NUMBERS are the evidence: a
    // reviewer needs to see that the live control found copies and that the scan covered
    // a plausible amount of memory, not just that the asserts held.
    println!(
        "residue[{mode}]: master-password {hits} hit(s), record-field {hits_rec} hit(s), \
         across {scanned} bytes of the child's anonymous memory"
    );

    // Release the child (closing its stdin ends its wait), then reap it.
    drop(ch.stdin.take());
    let _ = ch.wait();
    (hits, hits_rec)
}

fn spawn(mode: &str, seed: &str) -> Child {
    Command::new(std::env::current_exe().expect("test binary path"))
        // Run exactly this one test in the child, and nothing else in the binary.
        .args(["--exact", "secrets_do_not_survive_in_process_memory_after_the_vault_is_dropped", "--nocapture"])
        .env(MODE_ENV, mode)
        .env(SEED_ENV, seed)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("re-exec the test binary as a child")
}

/// The child half: get into the requested state, say READY, and hold still.
fn child(mode: &str, seed: &str) {
    let mut secret = sentinel(seed);

    if mode == "none" {
        // Control: prove the harness itself does not strew the sentinel around.
        secret.zeroize();
        drop(secret);
        ready_and_wait();
        return;
    }

    let mut secret_rec = sentinel_rec(seed);

    if mode == "kdf" {
        // Narrowest possible stage: ONLY the two-password Argon2id derivation, no vault,
        // no file, no record. If the sentinel survives here it is the KDF path and
        // nothing else.
        let salt = [7u8; 16];
        let k = vaultis_core::crypto::derive_key_chained(
            secret.as_bytes(),
            b"second-password",
            &salt,
            &KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 },
        )
        .expect("derive");
        drop(k);
        secret.zeroize();
        secret_rec.zeroize();
        ready_and_wait();
        return;
    }

    let dir = std::env::temp_dir().join(format!("vaultis-residue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp vault dir");
    let path: PathBuf = dir.join("vault.pmv");
    // Deliberately weak KDF: this test is about plaintext lifetime, not key strength, and
    // a real Argon2id cost would make the child take seconds for no added signal.
    let fast = KdfParams { m_cost: 256, t_cost: 1, p_cost: 1 };

    {
        // The two sentinels enter by DIFFERENT doors: one as a master password (the crypto
        // path), one as a record field (the serialize-then-encrypt path). Attribution
        // depends on never mixing them.
        let mut v = OpenVault::create(path, secret.as_bytes(), b"second-password", fast)
            .expect("create vault");

        if mode != "create" {
            let mut a = Account::new().expect("new account");
            a.title = "residue probe".into();
            a.username = "probe".into();
            a.password = secret_rec.clone();
            records::upsert(&mut v.vault.accounts, a);
        }
        if mode != "create" && mode != "record" {
            v.save().expect("save vault");
        }

        if mode == "hold" {
            // Control: the vault is OPEN and both secrets are legitimately live. Anything
            // that cannot find them here cannot be trusted to find a leak later.
            ready_and_wait();
            return;
        }
    } // `v` (and the record inside it) drop here — this is the property under test.

    secret.zeroize();
    secret_rec.zeroize();
    drop(secret);
    drop(secret_rec);
    let _ = std::fs::remove_dir_all(&dir);
    ready_and_wait();
}

/// Announce the state is ready to photograph, then block until the parent closes stdin.
fn ready_and_wait() {
    println!("READY");
    let _ = std::io::stdout().flush();
    let mut sink = String::new();
    let _ = std::io::stdin().read_line(&mut sink);
}

/// Count non-overlapping occurrences of `needle` in another process's readable
/// anonymous memory. File-backed regions are skipped: the needle is generated at run
/// time so it cannot live in a mapped file, and reading them would mean paging in
/// hundreds of MiB of shared libraries for nothing.
fn count_in_process_memory(pid: u32, needle: &[u8], needle_rec: &[u8]) -> (usize, usize, usize) {
    let maps = std::fs::File::open(format!("/proc/{pid}/maps")).expect("child maps");
    let mut mem = std::fs::File::open(format!("/proc/{pid}/mem"))
        .expect("child mem — needs ptrace_scope <= 1, which permits a parent to read its child");
    let mut hits = 0usize;
    let mut hits_rec = 0usize;
    let mut scanned = 0usize;

    for line in BufReader::new(maps).lines().map_while(Result::ok) {
        // 7f3c...-7f3c... rw-p 00000000 00:00 0 [heap]
        let mut parts = line.split_whitespace();
        let (Some(range), Some(perms)) = (parts.next(), parts.next()) else { continue };
        if !perms.starts_with('r') {
            continue;
        }
        let path = parts.nth(3).unwrap_or("");
        let anonymous = path.is_empty() || path == "[heap]" || path == "[stack]" || path == "[anon]";
        if !anonymous {
            continue;
        }
        let Some((lo, hi)) = range.split_once('-') else { continue };
        let (Ok(lo), Ok(hi)) = (u64::from_str_radix(lo, 16), u64::from_str_radix(hi, 16)) else {
            continue;
        };
        let len = (hi - lo) as usize;
        // Guard against a pathological mapping; the real ones here are MiB, not GiB.
        if len == 0 || len > 512 * 1024 * 1024 {
            continue;
        }
        // Read in CHUNKS, tolerating a failure per chunk rather than per region. A single
        // `read_exact` over a whole region gives up the moment it meets one unreadable
        // page — which silently skipped almost the entire heap in the first version of
        // this test, leaving a "no residue found" result that had barely looked anywhere.
        const CHUNK: usize = 1 << 20;
        let mut buf = vec![0u8; CHUNK + needle.len().max(needle_rec.len())];
        let mut off = 0usize;
        // Bytes carried between chunks so a needle straddling a boundary is still seen.
        let mut carry = 0usize;
        while off < len {
            let want = CHUNK.min(len - off);
            if mem.seek(SeekFrom::Start(lo + off as u64)).is_err() {
                break;
            }
            match mem.read_exact(&mut buf[carry..carry + want]) {
                Ok(()) => {
                    let filled = carry + want;
                    scanned += want;
                    if filled >= needle.len() {
                        hits += buf[..filled].windows(needle.len()).filter(|w| *w == needle).count();
                    }
                    if filled >= needle_rec.len() {
                        hits_rec +=
                            buf[..filled].windows(needle_rec.len()).filter(|w| *w == needle_rec).count();
                    }
                    // Carry the tail so the next chunk can complete a straddling match.
                    carry = needle.len().max(needle_rec.len()).saturating_sub(1).min(filled);
                    buf.copy_within(filled - carry..filled, 0);
                }
                // Unreadable page: skip this chunk and keep going with the rest.
                Err(_) => carry = 0,
            }
            off += want;
        }
        buf.zeroize(); // don't let OUR copy of the child's memory become the next leak
    }

    assert!(
        scanned > 64 * 1024,
        "only {scanned} bytes of child memory were readable — the scan did not run, so its \
         result means nothing"
    );
    (hits, hits_rec, scanned)
}
