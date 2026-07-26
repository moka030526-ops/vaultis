package com.vaultis

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable

/** Android: the real thing — the system Back button/gesture and the predictive-back API. */
@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) = BackHandler(enabled, onBack)
