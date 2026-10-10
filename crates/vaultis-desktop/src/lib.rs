//! vaultis (desktop) — the command-line and graphical (egui) front-ends for the
//! offline, two-password encrypted **estate vault**.
//!
//! All of the vault logic — data model, file format, crypto, and the
//! [`vault::OpenVault`] API — lives in the headless [`vaultis_core`] crate.
//! This crate is the desktop *shell* on top of it: the two binaries
//! (`vaultis`, the console build, and `vaultis-gui`, the Windows
//! GUI-subsystem build) plus the graphical front-end, `gui` (behind the `gui` feature).
//!
//! The core modules are re-exported here so the binaries' `vaultis::<mod>`
//! import paths and the front-ends' in-crate `crate::<mod>` paths keep
//! resolving unchanged after the workspace split.
//!
//! (`//!` is an inner doc comment for the whole crate; `///` documents the item
//! that follows; `//` is an ordinary comment.)
#![forbid(unsafe_code)]

// Re-export the headless core so existing `vaultis::crypto`, `vaultis::vault`,
// `crate::records`, … paths in the binaries and front-ends resolve unchanged.
pub use vaultis_core::{crypto, csv, fault, merge, password, records, storage, types, vault};

#[cfg(feature = "gui")]
pub mod gui; // graphical front-end; behind `gui`
#[cfg(feature = "gui")]
pub mod gui_help; // the GUI's built-in manual (content + the help browser); behind `gui`
pub mod launch; // vault-path/flag resolution shared by the console + windowed binaries
#[cfg(feature = "gui")]
pub mod single_instance; // GUI single-instance guard (raises the egui window); behind `gui`

// Shared plumbing, split by concern. The modules are private and their items are
// re-exported at the crate root at their original visibility, so the
// `crate::read_prefs_obj` / `vaultis::dest_inside` style paths used across the
// front-ends, binaries and tests resolve unchanged.
//
// `clipboard`, `prefs` and `fmt_money` serve only the GUI (they were shared with the
// terminal UI until it was removed). They stay compiled in a `--no-default-features`
// (CLI-only) build so their tests keep running there too, with the dead-code lint
// relaxed for that build alone; the linker drops the unused code from the binary.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod clipboard; // secret/plain clipboard copies + the auto-clear rule
mod export_dir; // export-directory validation + "inside the vault?" check
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod prefs; // the per-root prefs.json file and its typed preferences

#[cfg_attr(not(feature = "gui"), allow(unused_imports))]
pub(crate) use clipboard::*;
pub use export_dir::*;
pub(crate) use prefs::*;

/// Format a Summary-tab amount as a grouped, whole-unit currency string for the GUI:
/// `1_234_567.8 -> "$1,234,568"`, `-2500.0 -> "-$2,500"`,
/// `0.0 -> "$0"`. The summary is an approximation, so cents are rounded away for legibility.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub(crate) fn fmt_money(v: f64) -> String {
    // A non-finite total can reach here even though `parse_approx_value` rejects a non-finite
    // FIELD: the Summary sums many finite values, and two near-`f64::MAX` entries add to +inf.
    // `f64 as u64` saturates, so inf would render as the literal `$18,446,744,073,709,551,615`
    // and NaN as `$0` — a made-up figure presented as a real one in a financial view. Say
    // "out of range" instead.
    if !v.is_finite() {
        return "$—".to_string();
    }
    let rounded = v.abs().round() as u64;
    // Take the sign from the ROUNDED magnitude, not the raw float, so a value that rounds to 0
    // never shows "-$0". A tiny negative residue is realistic on the Summary's Net column —
    // assets minus an equal liability leaves e.g. -1e-17 from f64 subtraction.
    let neg = v < 0.0 && rounded != 0;
    let digits = rounded.to_string();
    let bytes = digits.as_bytes();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*b as char);
    }
    if neg {
        format!("-${grouped}")
    } else {
        format!("${grouped}")
    }
}

/// The Argon2id cost to CREATE a new vault with — the built-in default (64 MiB, 3
/// passes, 1 lane, applied twice by the two-password chain) unless overridden by
/// `VAULTIS_KDF_MCOST_MIB` / `VAULTIS_KDF_TCOST`.
///
/// # Why this knob exists
///
/// Against a "harvest now, decrypt later" adversary — someone who copies the encrypted
/// vault today and waits — the cipher is not the weak point. There is no public-key
/// cryptography anywhere in vaultis, so Shor's algorithm has nothing to attack, and the
/// key is a full 256 bits, which leaves ~128-bit security under Grover. What an attacker
/// actually does is **guess the two passwords**, and the only two things standing in the
/// way are their entropy and the per-guess cost set here. Password entropy dominates; this
/// is the second lever, and it was previously not reachable at all — every create site
/// hardcoded the default even though the on-disk format has always supported up to
/// `MAX_M_COST` (512 MiB) and `MAX_T_COST` (16).
///
/// # Read this before raising it
///
/// The cost is baked into the vault at creation and paid on **every open, forever, on
/// every device**. Raise it to 512 MiB and an unlock needs half a gigabyte of memory —
/// which a phone may simply refuse, leaving the vault openable only on the desktop. For an
/// estate vault, "my executor cannot open it" is a far more likely catastrophe than "a
/// quantum computer read it in 2050". Prefer longer passwords first: entropy is free at
/// open time, and this is not.
///
/// Invalid or out-of-range values fall back to the default with a warning rather than
/// failing the create — the bound is `KdfParams::validate`, the same gate the reader uses,
/// so this can never write a vault the reader would refuse.
pub fn kdf_params_for_new_vault() -> vaultis_core::crypto::KdfParams {
    use vaultis_core::crypto::KdfParams;
    let default = KdfParams::default();

    // `MiB` in the variable name because m_cost is in KiB — an easy factor-of-1024 trap.
    let mib = std::env::var("VAULTIS_KDF_MCOST_MIB").ok();
    let t = std::env::var("VAULTIS_KDF_TCOST").ok();
    if mib.is_none() && t.is_none() {
        return default;
    }

    let parse = |v: &Option<String>, name: &str, fallback: u32| -> u32 {
        match v {
            None => fallback,
            Some(s) => match s.trim().parse::<u32>() {
                Ok(n) => n,
                Err(_) => {
                    eprintln!("warning: {name}={s:?} is not a number; using {fallback}");
                    fallback
                }
            },
        }
    };

    let m_cost_mib = parse(&mib, "VAULTIS_KDF_MCOST_MIB", default.m_cost / 1024);
    let candidate = KdfParams {
        // Saturating: a silly MiB value must not wrap into a *weak* KiB cost. Anything
        // out of range is caught by validate() below anyway.
        m_cost: m_cost_mib.saturating_mul(1024),
        t_cost: parse(&t, "VAULTIS_KDF_TCOST", default.t_cost),
        p_cost: default.p_cost,
    };

    // An unparseable value falls back per-field, which can land exactly on the default;
    // announcing that as "non-default" would be simply untrue. Say nothing in that case.
    if candidate.m_cost == default.m_cost && candidate.t_cost == default.t_cost {
        return default;
    }

    match candidate.validate() {
        Ok(()) => {
            eprintln!(
                "note: creating this vault with a non-default KDF cost ({} MiB, {} passes). \
                 Every future open — on every device, including phones — must pay it.",
                candidate.m_cost / 1024,
                candidate.t_cost
            );
            candidate
        }
        Err(_) => {
            eprintln!(
                "warning: requested KDF cost ({} MiB, {} passes) is outside the accepted range \
                 ({}–{} MiB, 1–{} passes); using the default ({} MiB, {} passes).",
                candidate.m_cost / 1024,
                candidate.t_cost,
                KdfParams::MIN_M_COST.div_ceil(1024),
                KdfParams::MAX_M_COST / 1024,
                KdfParams::MAX_T_COST,
                default.m_cost / 1024,
                default.t_cost,
            );
            default
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
