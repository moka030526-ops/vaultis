package com.vaultis

import androidx.compose.runtime.Composable

/**
 * iOS: no-op. There is no system Back button, and this screen is not inside a
 * `UINavigationController`, so there is no interactive-pop gesture to intercept —
 * the top bar's own "Back" button is the affordance. Kept as an actual so the
 * shared UI can call it unconditionally.
 *
 * NOTE: iOS-only source set — compiled only on a Mac; build-verify there.
 */
@Composable
@Suppress("UNUSED_PARAMETER")
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) {
}
