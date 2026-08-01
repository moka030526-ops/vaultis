//! Unit tests for the parent module ([`super`], `single_instance.rs`), split into their own
//! file via `#[cfg(test)] #[path = "single_instance_tests.rs"] mod tests;` so the tests do not sit
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A unique throwaway directory for the lock/socket files.
fn tmp() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("pmsi-{nanos}-{n}"));
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn first_launch_is_primary_second_is_secondary_then_primary_again() {
    let dir = tmp();
    let vault = dir.join("vault.pmv");

    let first = acquire_in(&dir, &vault).unwrap();
    assert!(matches!(first, Instance::Primary { .. }), "first launch should own the instance");

    // While the first guard is alive, a second acquire for the SAME vault must
    // detect it and refuse to become primary.
    let second = acquire_in(&dir, &vault).unwrap();
    assert!(matches!(second, Instance::AlreadyRunning), "second launch should see the first");

    // Releasing the primary frees the lock for a fresh launch.
    drop(first);
    let third = acquire_in(&dir, &vault).unwrap();
    assert!(matches!(third, Instance::Primary { .. }), "after release, a new launch is primary");
}

#[test]
fn different_vaults_get_separate_instances() {
    let dir = tmp();
    let a = acquire_in(&dir, &dir.join("a.pmv")).unwrap();
    let b = acquire_in(&dir, &dir.join("b.pmv")).unwrap();
    assert!(matches!(a, Instance::Primary { .. }));
    assert!(matches!(b, Instance::Primary { .. }), "a different vault is independently primary");
}

#[test]
fn instance_key_is_stable_and_path_specific() {
    let a1 = instance_key(Path::new("/tmp/some/where/vault.pmv"));
    let a2 = instance_key(Path::new("/tmp/some/where/vault.pmv"));
    let b = instance_key(Path::new("/tmp/some/other/vault.pmv"));
    assert_eq!(a1, a2, "same path must hash to the same token across calls");
    assert_ne!(a1, b, "different paths must not collide");
}

#[test]
fn env_flag_reads_truthy_values() {
    assert!(!env_flag("PMVAULT_DEFINITELY_UNSET_VAR_XYZ"));
}

/// ThreadSanitizer reproducer for the focus accept-loop thread. The normal
/// tests never call [`FocusServer::serve`] (it needs a live `egui::Context`),
/// so the one place we hand a shared object across threads — the detached
/// accept loop touching the `Context` the GUI also renders from — goes
/// unexercised by `cargo test`. This drives the *real* `serve` thread with a
/// real `Context` under concurrent pings while the main thread pokes the same
/// `Context`, reproducing the exact cross-thread sharing for TSan to inspect.
///
/// `#[ignore]`d: it spawns a detached thread and does hundreds of socket
/// connections, which is pointless noise in the normal suite. Run it as the
/// one-off race check (needs nightly + `rust-src`):
///
/// ```text
/// RUSTFLAGS=-Zsanitizer=thread cargo +nightly test -Zbuild-std \
///   --target x86_64-unknown-linux-gnu --lib \
///   single_instance::tests::focus_accept_thread_is_race_free \
///   -- --ignored --nocapture
/// ```
#[cfg(unix)]
#[test]
#[ignore = "TSan one-off; needs nightly + -Zsanitizer=thread (see doc comment)"]
fn focus_accept_thread_is_race_free() {
    use eframe::egui;
    use std::thread;

    let dir = tmp();
    let key = instance_key(&dir.join("vault.pmv"));

    // Bind the real focus socket and start the real accept thread on a live
    // Context — exactly what the GUI does in the eframe creation closure.
    let server = serve_socket(&dir, &key);
    let ctx = egui::Context::default();
    server.serve(ctx.clone());

    // Several threads hammer the accept loop with connections; every accepted
    // connection makes the accept thread call `ctx.send_viewport_cmd` +
    // `ctx.request_repaint`. Meanwhile the main thread touches the SAME Context,
    // so any unsynchronized sharing in our usage would surface under TSan.
    let pingers: Vec<_> = (0..4)
        .map(|_| {
            let dir = dir.clone();
            let key = key.clone();
            thread::spawn(move || {
                for _ in 0..50 {
                    request_focus(&dir, &key);
                }
            })
        })
        .collect();

    for _ in 0..400 {
        ctx.request_repaint();
    }

    for p in pingers {
        p.join().unwrap();
    }
    // The accept thread is detached by design (torn down at process exit); TSan
    // evaluates the interleavings observed during the contention above.
}

#[test]
fn flag_truthiness_rule() {
    // "on": any non-empty value other than "0".
    assert!(flag_is_truthy("1"));
    assert!(flag_is_truthy("true"));
    assert!(flag_is_truthy("yes"));
    assert!(flag_is_truthy(" ")); // a space is non-empty and not "0"
    // "off": empty or exactly "0".
    assert!(!flag_is_truthy(""));
    assert!(!flag_is_truthy("0"));
}
