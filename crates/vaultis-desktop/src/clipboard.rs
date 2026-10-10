//! OS clipboard handling shared by the GUI and TUI: copying a secret with the
//! clipboard-manager "don't record" hint, plain copies, and the auto-clear timing rule.

/// Copy a SECRET (a password) into the OS clipboard, flagging it so clipboard
/// managers don't retain it. On Linux, arboard's `exclude_from_history` sets the
/// `x-kde-passwordManagerHint` (honoured over X11 — including XWayland — by
/// klipper/GPaste/clipman), so the password isn't logged into a manager's
/// persistent history; the GUI/TUI 15 s + on-exit clears only overwrite the live
/// selection, not such a log. On macOS the same call sets `org.nspasteboard.ConcealedType`,
/// the marker Mac clipboard managers (Maccy, Raycast, Alfred, Paste…) honour by not
/// recording the entry. On Windows this is a plain set. Shared by the
/// GUI and TUI so both copy paths get the hint. (A clipboard manager that ignores
/// the hint, or a native-Wayland-only setup, may still retain history.)
///
/// Behind the `clipboard` feature: on Linux arboard dynamically loads X11/Wayland, so a
/// fully-static (musl) terminal build omits it (the TUI's copy then becomes a no-op).
#[cfg(feature = "clipboard")]
pub(crate) fn copy_secret_to_clipboard(text: &str) -> Result<(), arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    #[cfg(target_os = "linux")]
    {
        use arboard::SetExtLinux;
        cb.set().exclude_from_history().text(text.to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        use arboard::SetExtApple;
        cb.set().exclude_from_history().text(text.to_owned())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        cb.set_text(text.to_owned())
    }
}

/// Copy a NON-secret (a URL or username) into the OS clipboard. Unlike
/// [`copy_secret_to_clipboard`] this is a plain `set_text` on every platform: no
/// `exclude_from_history` hint (a non-secret belongs in normal clipboard history so
/// a clipboard manager can keep it) and the caller schedules NO auto-clear timer.
/// Kept separate from the secret path so the two security contracts never blur: a
/// password is always history-excluded + auto-cleared, a URL/username never is.
///
/// Gated on `gui` (which implies `clipboard`), not `clipboard`, because only the egui
/// GUI has the URL/username copy buttons — the TUI copies via Ctrl+Y, which targets
/// the password (secret) field. Gating on `clipboard` alone would make this dead code
/// in a `clipboard`-without-`gui` build (e.g. the minimal TUI with OS-copy added back).
#[cfg(feature = "gui")]
pub(crate) fn copy_plain_to_clipboard(text: &str) -> Result<(), arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_owned())
}

/// Pure decision for the clipboard auto-clear "tick", shared by the TUI (`ui.rs`) and
/// GUI (`gui.rs`) so both obey the SAME security-relevant contract. Given the pending
/// wipe `deadline` (if any), the current time `now`, and the current `status` line:
///   * `None` — nothing scheduled, or the deadline has not been reached: do nothing.
///   * `Some(None)` — wipe the clipboard now, but LEAVE the status untouched (it shows a
///     message the user may not have seen yet, e.g. `"Save failed: …"`).
///   * `Some(Some(s))` — wipe the clipboard now and set the status to `s`.
///
/// Kept side-effect-free (no clipboard or egui access) so the two rules a password
/// manager must not get wrong — fire only at/after the deadline, and never clobber an
/// unseen status, only a blank or a prior `"Copied …"` notice — are unit-testable.
pub(crate) fn clipboard_tick_decision(
    deadline: Option<std::time::Instant>,
    now: std::time::Instant,
    status: &str,
) -> Option<Option<String>> {
    match deadline {
        Some(t) if now >= t => {
            if status.is_empty() || status.starts_with("Copied") {
                Some(Some("Clipboard cleared.".to_string()))
            } else {
                Some(None) // keep the existing (possibly unseen) status
            }
        }
        _ => None,
    }
}
