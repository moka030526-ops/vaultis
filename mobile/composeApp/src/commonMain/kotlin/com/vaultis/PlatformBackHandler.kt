package com.vaultis

import androidx.compose.runtime.Composable

/**
 * Intercept the platform's own "go back" gesture while [enabled], routing it to [onBack].
 *
 * Android needs this: the system Back button/gesture is the primary way people navigate,
 * and without an interception it FINISHES THE ACTIVITY. On the record-detail screen that
 * means the natural "go back to the list" gesture instead throws the user out of the app
 * (and, because backgrounding locks the vault, forces a full two-password unlock to get
 * back to a list they were already looking at). The in-app "Back" button in the top bar
 * is not a substitute — nobody reaches for it first on Android.
 *
 * Deliberately NOT enabled on the list screen: there, "back" SHOULD leave the app, which
 * triggers `onStop` → auto-lock. That is the safe direction, so it is left alone.
 *
 * Compose Multiplatform 1.7 has no common back handler (`androidx.compose.ui.backhandler`
 * arrived in 1.8), hence the expect/actual. iOS has no equivalent system-wide gesture for
 * this screen, so its actual is a no-op — the top-bar button is the affordance there.
 */
@Composable
expect fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit)
