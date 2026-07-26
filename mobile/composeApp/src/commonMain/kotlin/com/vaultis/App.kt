@file:OptIn(ExperimentalMaterial3Api::class)

package com.vaultis

import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScrollableTabRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.vaultis.ffi.Vault
import com.vaultis.ffi.VaultException
import com.vaultis.ffi.RecordKind
import com.vaultis.ffi.openVault
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.time.Duration
import kotlin.time.Duration.Companion.minutes
import kotlin.time.TimeMark
import kotlin.time.TimeSource

/**
 * Lock the vault after this long with no touch anywhere in the app. Two minutes is the
 * usual password-manager figure: long enough to read an entry through without being
 * interrupted, short enough that a phone put down and walked away from does not stay
 * unlocked. Locking is cheap and lossless here (v1 is read-only — there is nothing
 * unsaved to lose), so the tradeoff is entirely in the user's favour.
 */
private val IDLE_LOCK_AFTER = 2.minutes

/**
 * Last-touch instant for the idle auto-lock.
 *
 * Deliberately a plain `var` in a holder rather than Compose state: a touch must NOT
 * invalidate composition. Pointer events arrive continuously during a scroll, so making
 * this observable would recompose (or restart the timer coroutine) tens of times a second
 * for something no UI element displays. The timer reads it from inside a coroutine, which
 * is not a snapshot observer, so nothing needs to be notified.
 *
 * A MONOTONIC mark, not a wall clock: the countdown must not be skippable by changing the
 * device's time zone or clock while the vault is open.
 */
private class IdleClock {
    var lastTouch: TimeMark = TimeSource.Monotonic.markNow()
}

/**
 * The vault tabs, each mapping to a core [RecordKind]. **All eight collections the core
 * stores appear here**, in the desktop's tab order.
 *
 * `Urgent` is first, and is the tab the app opens on, because that is what it is for: the
 * core describes it as "the most time-critical things an executor must know (whom to call,
 * where the safe key is, an in-flight crisis) … the first thing seen on unlock". Earlier
 * versions of this app showed only five tabs and opened on Accounts, so an executor reaching
 * for a phone in exactly that crisis saw a complete-looking app with the urgent note, the tax
 * filings and the general documents silently missing from it.
 */
private enum class Section(val title: String, val kind: RecordKind) {
    Urgent("Urgent", RecordKind.URGENT),
    Instructions("Instructions", RecordKind.INSTRUCTION),
    TrustWill("Trust & Will", RecordKind.TRUST_WILL),
    Assets("Assets & Liabilities", RecordKind.ASSET_LIABILITY),
    Accounts("Accounts", RecordKind.ACCOUNT),
    RealEstate("Real Estate", RecordKind.REAL_ESTATE),
    Taxes("Taxes", RecordKind.TAX_FILING),
    Documents("Documents", RecordKind.GENERAL_DOCUMENT),
}

/**
 * App-wide "left the foreground" signal. Each platform entry point calls [onEnterBackground]
 * when the app is backgrounded — Android `Activity.onStop`, iOS scene phase != `.active` — and
 * [App] observes [backgroundEpoch] to LOCK the vault. Without this a backgrounded password
 * manager resumes still-unlocked, leaving secrets in memory and on screen for whoever next
 * picks up the device. (The app-switcher SNAPSHOT is separately covered by Android FLAG_SECURE
 * and the iOS overlay; this adds the missing auto-lock.)
 */
object AppLifecycle {
    // Only `onEnterBackground` bumps this; `App` reads it as a snapshot state to trigger a lock.
    var backgroundEpoch by mutableStateOf(0)

    fun onEnterBackground() {
        backgroundEpoch++
    }
}

/**
 * Root composable. Shared verbatim by Android and iOS. Holds the locked/unlocked
 * state; the opaque [Vault] handle is destroyed when locking so the Rust side
 * zeroizes the key.
 *
 * [copySecret] is an optional platform copy-to-clipboard hook for SECRETS. Android
 * passes one that marks the clip `EXTRA_IS_SENSITIVE` (so the Android 13+ paste
 * preview and history keyboards don't expose the password); when `null` (iOS) the
 * shared Compose clipboard is used. The 15 s + on-lock wipe is platform-agnostic.
 */
@Composable
fun App(vaultDir: String, copySecret: ((String) -> Unit)? = null) {
    // Follow the system light/dark setting. Without an explicit scheme MaterialTheme
    // always uses the LIGHT one, so a phone in dark mode got a full-brightness white
    // screen — bad at night and, for a vault you may open in public, needlessly
    // conspicuous. (No dynamic/wallpaper colour on purpose: the palette should not
    // vary by device for an app whose screenshots are its own documentation.)
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        // `Surface` paints the themed background across the WHOLE window — including
        // behind the status and navigation bars, which we now draw under (below).
        // `safeDrawingPadding` then insets the actual content out of those bars, the
        // display cutout, AND the on-screen keyboard. Android 15 (targetSdk 35) makes
        // edge-to-edge mandatory, so without this the top app bar would sit under the
        // status-bar clock and the "Unlock" button under the gesture-nav pill.
        var vault by remember { mutableStateOf<Vault?>(null) }
        val clipboard = LocalClipboardManager.current
        // App-scoped clipboard auto-clear. Copying a password bumps this token,
        // which (re)arms a single 15s wipe at the app ROOT — so it SURVIVES
        // navigating back to the list or locking the vault (the timer is no longer
        // tied to the detail screen's lifecycle, which would cancel it on dispose).
        //
        // Plain `remember`, NOT `rememberSaveable`: the realistic Activity-recreation
        // triggers (rotation, dark/light, locale, font/display size, split-screen) are
        // already prevented by the android:configChanges manifest list, so the wipe
        // coroutine survives them in-process. We deliberately do NOT persist the token
        // across PROCESS DEATH: a restored token would, 15 s after a later relaunch,
        // wipe whatever UNRELATED content the user copied meanwhile (the original copy
        // time is long gone, and the vault restarts locked). True process death inside
        // the 15 s window falls under the documented best-effort clipboard caveat.
        var clipboardToken by remember { mutableStateOf(0) }
        LaunchedEffect(clipboardToken) {
            if (clipboardToken == 0) return@LaunchedEffect // nothing pending / already wiped
            delay(15_000)
            clipboard.setText(AnnotatedString(""))
            clipboardToken = 0 // mark wiped
        }
        val copyToClipboard: (String) -> Unit = { secret ->
            // Prefer the platform secret-copy hook (Android marks the clip sensitive);
            // fall back to the shared Compose clipboard (iOS).
            if (copySecret != null) copySecret(secret) else clipboard.setText(AnnotatedString(secret))
            clipboardToken++
        }

        // Auto-lock on background: the platform entry points bump AppLifecycle when the app
        // leaves the foreground; we drop the Vault handle (Rust zeroizes the key) so the app
        // never resumes still-unlocked. backgroundEpoch starts at 0, so this is a no-op until
        // the first real background event.
        LaunchedEffect(AppLifecycle.backgroundEpoch) {
            if (AppLifecycle.backgroundEpoch > 0 && vault != null) {
                vault?.destroy()
                vault = null
                if (clipboardToken != 0) {
                    clipboard.setText(AnnotatedString(""))
                    clipboardToken = 0
                }
            }
        }

        // Idle auto-lock. Backgrounding the app already locks it, but a phone left face-up
        // and UNTOUCHED on a desk or table stays foregrounded and unlocked indefinitely —
        // the single most likely way this vault gets read by someone who is not the owner.
        // (The desktop has no equivalent because it is not a device you put down in public;
        // this is one of the few places the mobile app is deliberately STRICTER.)
        //
        // Any touch anywhere in the app stamps `idle.lastTouch` (see the pointer observer
        // below); this loop sleeps exactly until the deadline that stamp implies, re-checks,
        // and only locks once the screen has genuinely been idle for the whole window — so a
        // touch mid-countdown extends it without restarting the coroutine. Reading a long
        // entry without touching the screen is the one false positive, hence a tolerant
        // timeout; the cost of being wrong is re-entering two passwords.
        val idle = remember { IdleClock() }
        LaunchedEffect(vault != null) {
            if (vault == null) return@LaunchedEffect // already locked; nothing to time out
            idle.lastTouch = TimeSource.Monotonic.markNow() // unlocking counts as activity
            while (true) {
                val remaining = IDLE_LOCK_AFTER - idle.lastTouch.elapsedNow()
                if (remaining <= Duration.ZERO) break
                delay(remaining)
            }
            vault?.destroy()
            vault = null
            if (clipboardToken != 0) {
                clipboard.setText(AnnotatedString("")) // same wipe as an explicit lock
                clipboardToken = 0
            }
        }

        val current = vault
        // `Surface` paints the themed background across the WHOLE window, including
        // behind the status and navigation bars (which we draw under — see
        // MainActivity's enableEdgeToEdge). `safeDrawingPadding` then insets the
        // CONTENT out of those bars, the display cutout, and the on-screen keyboard.
        // Android 15 (targetSdk 35) makes edge-to-edge mandatory, so without this the
        // top app bar would sit under the status-bar clock and the "Unlock" button
        // under the gesture-nav pill.
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            Box(
                Modifier
                    .fillMaxSize()
                    .safeDrawingPadding()
                    // Observe every touch to feed the idle timer above. `Initial` pass +
                    // never consuming the event means this only WATCHES: taps, scrolls and
                    // text entry still reach the UI underneath exactly as before.
                    .pointerInput(Unit) {
                        awaitPointerEventScope {
                            while (true) {
                                awaitPointerEvent(PointerEventPass.Initial)
                                idle.lastTouch = TimeSource.Monotonic.markNow()
                            }
                        }
                    }
            ) {
                if (current == null) {
                    UnlockScreen(vaultDir) { vault = it }
                } else {
                    VaultScreen(current, copyToClipboard) {
                        current.destroy()
                        vault = null
                        // Only wipe the clipboard if WE put a secret there (token != 0); otherwise
                        // an unrelated clip the user copied meanwhile must be left untouched on lock.
                        if (clipboardToken != 0) {
                            clipboard.setText(AnnotatedString("")) // wipe the copied secret on lock
                            clipboardToken = 0 // we just wiped — cancel any pending auto-clear
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun UnlockScreen(vaultDir: String, onUnlocked: (Vault) -> Unit) {
    var pw1 by remember { mutableStateOf("") }
    var pw2 by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    Column(
        // Scrollable: with the soft keyboard up (and `safeDrawingPadding` insetting the
        // content out of it), a short phone in landscape has too little height left for
        // the two fields plus the button — without a scroll the "Unlock" button is simply
        // unreachable. `Center` still centres it whenever it does fit.
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text("vaultis", style = MaterialTheme.typography.headlineMedium)
        Spacer(Modifier.height(4.dp))
        Text("Enter your two passwords, in order.", style = MaterialTheme.typography.bodyMedium)
        Spacer(Modifier.height(20.dp))
        OutlinedTextField(
            value = pw1,
            onValueChange = { pw1 = it },
            label = { Text("First password") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            // KeyboardType.Password tells the IME this is a secret field (inputType
            // textPassword + IME_FLAG_NO_PERSONALIZED_LEARNING) so the soft keyboard does
            // NOT add the master password to its dictionary / next-word model or suggest it.
            // The visual mask alone does not change IME behavior.
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = pw2,
            onValueChange = { pw2 = it },
            label = { Text("Second password") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(20.dp))
        Button(
            enabled = !busy && pw1.isNotEmpty() && pw2.isNotEmpty(),
            onClick = {
                busy = true
                error = null
                scope.launch {
                    // Key derivation (Argon2id) is heavy — keep it off the UI thread.
                    val result = runCatching {
                        withContext(Dispatchers.Default) {
                            // `open_vault` wipes the RUST-owned copies of these arrays, but
                            // the host's own copies are the host's to clear (it says so in
                            // its docs). A ByteArray is mutable, so — unlike the immutable
                            // String fields below — this wipe really does overwrite the
                            // master-password bytes. `finally` so it runs on the throwing
                            // path too, which is where a wrong-password attempt lands.
                            val b1 = pw1.encodeToByteArray()
                            val b2 = pw2.encodeToByteArray()
                            try {
                                openVault(vaultDir, b1, b2)
                            } finally {
                                b1.fill(0)
                                b2.fill(0)
                            }
                        }
                    }
                    // Drop the plaintext passwords from the UI state as soon as the attempt
                    // is over — on success AND on failure, mirroring the desktop's
                    // `wipe_passwords()` on both paths (a failed attempt is the moment a
                    // user is most likely to step away). Kotlin Strings are immutable, so
                    // this cannot overwrite the bytes the way the desktop's `zeroize()`
                    // does; what it does do is release the last reference so the GC can
                    // reclaim them, instead of pinning both master passwords in the heap
                    // for as long as this screen lives.
                    pw1 = ""
                    pw2 = ""
                    busy = false
                    result
                        .onSuccess { onUnlocked(it) }
                        .onFailure { error = friendlyError(it) }
                }
            },
            modifier = Modifier.fillMaxWidth(),
        ) { Text(if (busy) "Unlocking…" else "Unlock") }

        if (busy) {
            Spacer(Modifier.height(16.dp))
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
        }
        error?.let {
            Spacer(Modifier.height(16.dp))
            Text(it, color = MaterialTheme.colorScheme.error)
        }
    }
}

/** Map the FFI error to a calm, non-leaking message (wrong-pw == corrupt). */
private fun friendlyError(e: Throwable): String = when (e) {
    is VaultException.NotFound ->
        "No vault found. Copy your encrypted vault folder into the app's storage first."
    is VaultException.WrongPasswordOrCorrupt ->
        "Wrong passwords, or the vault is damaged. Re-check both passwords and their order."
    is VaultException.RekeyPending ->
        "An interrupted password change is pending. Finish it on the desktop app, then try again."
    // The UniFFI-generated VaultException subclasses return an EMPTY message (not null),
    // so a plain `?:` would show a blank error for Io/Internal/Locked. Treat blank as missing.
    is VaultException -> e.message?.takeIf { it.isNotBlank() } ?: "Could not open the vault."
    else -> e.message?.takeIf { it.isNotBlank() } ?: "Unexpected error."
}

@Composable
private fun VaultScreen(vault: Vault, onCopy: (String) -> Unit, onLock: () -> Unit) {
    // Open on Urgent, like the desktop: the executor's "read this first" tab.
    var section by remember { mutableStateOf(Section.Urgent) }
    var selectedId by remember { mutableStateOf<String?>(null) }

    // System Back on a record returns to the list, like every other Android app. Only
    // enabled while a record is open; on the list itself Back is left to leave the app,
    // which locks the vault (see PlatformBackHandler).
    PlatformBackHandler(enabled = selectedId != null) { selectedId = null }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (selectedId == null) "vaultis" else section.title) },
                navigationIcon = {
                    if (selectedId != null) {
                        TextButton(onClick = { selectedId = null }) { Text("Back") }
                    }
                },
                actions = { TextButton(onClick = onLock) { Text("Lock") } },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            val id = selectedId
            if (id == null) {
                Column(Modifier.fillMaxSize()) {
                    // The unlock banner, with the SAME content and priority order both
                    // desktop front-ends use (gui.rs / ui.rs after a successful open):
                    //
                    //   1. the core's rollback/recovery notice, if any — the vault was
                    //      recovered from its in-place mirror, or its generation went
                    //      backwards. Takes priority: a tampered/rolled-back vault must
                    //      not open silently on mobile either.
                    //   2. otherwise "Last opened: <time> (generation N)". Both halves are
                    //      tamper signals the user is the only one who can check: an access
                    //      time they do not recognise means somebody else opened the vault
                    //      with their two passwords, and a generation that went DOWN since
                    //      last time means the whole file was swapped for an older snapshot.
                    //      Without this the mobile app was strictly weaker than the desktop
                    //      at surfacing an unauthorised open.
                    //
                    // The timestamp is formatted by the FFI (`previousAccessLabel`), not
                    // here, so the calendar math stays in the one audited implementation and
                    // reads identically on desktop, Android, and iOS. Computed once per
                    // unlock — these are properties of the snapshot that was opened.
                    val recovery = remember { vault.recoveryNotice() }
                    val opened = remember {
                        // previousAccess() == 0 means "never opened before" (a vault created
                        // on desktop and copied over, opened here for the first time) — the
                        // desktop shows a bare "Vault unlocked." there rather than a
                        // meaningless date, so there is no banner to show.
                        if (vault.previousAccess() == 0L) null
                        else "Last opened: ${vault.previousAccessLabel()} (generation ${vault.generation()})"
                    }
                    if (recovery != null) {
                        Surface(
                            color = MaterialTheme.colorScheme.errorContainer,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(
                                "⚠ $recovery",
                                color = MaterialTheme.colorScheme.onErrorContainer,
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.padding(12.dp),
                            )
                        }
                    } else if (opened != null) {
                        Surface(
                            color = MaterialTheme.colorScheme.surfaceVariant,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(
                                opened,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                style = MaterialTheme.typography.bodySmall,
                                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                            )
                        }
                    }
                    ScrollableTabRow(selectedTabIndex = section.ordinal, edgePadding = 8.dp) {
                        Section.entries.forEach { s ->
                            Tab(
                                selected = s == section,
                                onClick = { section = s },
                                text = { Text(s.title) },
                            )
                        }
                    }
                    val rows = remember(section) { vault.listRecords(section.kind) }
                    if (rows.isEmpty()) {
                        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                            Text("No entries", style = MaterialTheme.typography.bodyLarge)
                        }
                    } else {
                        LazyColumn(Modifier.fillMaxSize()) {
                            items(rows.size) { i ->
                                ListItem(
                                    headlineContent = { Text(rows[i].label) },
                                    modifier = Modifier.clickable { selectedId = rows[i].id },
                                )
                                HorizontalDivider()
                            }
                        }
                    }
                }
            } else {
                DetailScreen(vault, section.kind, id, onCopy)
            }
        }
    }
}

@Composable
private fun DetailScreen(vault: Vault, kind: RecordKind, id: String, onCopy: (String) -> Unit) {
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
    ) {
        // NEVER add an `else ->` branch here. This `when` is exhaustive over RecordKind,
        // which makes the Kotlin compiler REFUSE to build if the core/FFI gains a record
        // kind this screen cannot render ("'when' expression must be exhaustive" —
        // verified by deleting a branch). That compile error is the guard that stops a new
        // collection being silently invisible on the phone, which is exactly how Urgent,
        // Taxes and Documents went missing before. An `else` would turn that error back
        // into a blank detail screen.
        when (kind) {
            RecordKind.URGENT -> {
                val r = remember(id) { runCatching { vault.getUrgent(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Title", r.title)
                    Field("Description", r.description)
                }
            }
            RecordKind.INSTRUCTION -> {
                val r = remember(id) { runCatching { vault.getInstruction(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Title", r.title)
                    Field("Description", r.description)
                }
            }
            RecordKind.TAX_FILING -> {
                val r = remember(id) { runCatching { vault.getTaxFiling(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Owner", r.owner)
                    Field("Year", r.year)
                    Field("Notes", r.notes)
                    // Show the count even though opening the files is post-MVP: an executor
                    // must be able to tell that documents exist for a year.
                    Field(
                        "Attached documents",
                        if (r.documentCount == 0u) "none"
                        else "${r.documentCount} (open on desktop)",
                    )
                }
            }
            RecordKind.GENERAL_DOCUMENT -> {
                val r = remember(id) { runCatching { vault.getGeneralDocument(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Title", r.title)
                    Field("Description", r.description)
                    Field("Attached document", if (r.file != null) "yes (open on desktop)" else "none")
                }
            }
            RecordKind.TRUST_WILL -> {
                val r = remember(id) { runCatching { vault.getTrustWill(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Document", r.document)
                    Field("Usage", r.usage)
                    Field("Attached document", if (r.file != null) "yes (open on desktop)" else "none")
                }
            }
            RecordKind.ASSET_LIABILITY -> {
                val r = remember(id) { runCatching { vault.getAsset(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Kind", r.kind)
                    Field("Description", r.description)
                    Field("Owner", r.owner)
                    Field("Approx. value", r.approxValue)
                    Field("As of", r.asOfDate)
                    Field("Institution", r.institution)
                    Field("Type", r.assetType)
                    Field("Beneficiary", r.beneficiary)
                    Field("URL", r.url)
                    if (r.statement != null) Field("Attached statement", "yes (open on desktop)")
                }
            }
            RecordKind.ACCOUNT -> {
                val r = remember(id) { runCatching { vault.getAccount(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Type", r.accountType)
                    Field("Subtype", r.accountSubtype)
                    Field("Owner", r.owner)
                    Field("Username", r.username)
                    PasswordField(r.password, onCopy)
                    Field("URL", r.url)
                    Field("Closed as of", r.closedAsOf)
                    Field("Description", r.description)
                }
            }
            RecordKind.REAL_ESTATE -> {
                val r = remember(id) { runCatching { vault.getRealEstate(id) }.getOrNull() }
                if (r == null) NotFound() else {
                    Field("Address", r.address)
                    Field("Ownership", r.ownership)
                    Field("Taxes", r.taxes)
                    Field("HOA", r.hoa)
                    Field("Income account", r.incomeAccount)
                    Field("Financing account", r.financingAccount)
                    Field("Payment account", r.paymentAccount)
                }
            }
        }
    }
}

@Composable
private fun NotFound() {
    Text("This entry is no longer available.", color = MaterialTheme.colorScheme.error)
}

@Composable
private fun Field(label: String, value: String) {
    if (value.isBlank()) return
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
        Text(value, style = MaterialTheme.typography.bodyLarge)
    }
    HorizontalDivider()
}

/**
 * Password row: hidden by default, with a reveal toggle and a copy button. The
 * actual clipboard write + the 15s auto-clear are owned by [App] (via `onCopy`),
 * so the wipe survives navigating away or locking — see the App-scoped timer.
 */
@Composable
private fun PasswordField(password: String, onCopy: (String) -> Unit) {
    var revealed by remember { mutableStateOf(false) }
    var copied by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        Text("Password", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                // Fixed-width mask: do NOT key the dot count off password.length, which would
                // leak the exact length to a shoulder-surfer (FLAG_SECURE only blocks
                // screenshots/recents, not a person looking at the screen). Show a constant
                // mask when there is a password, nothing when empty.
                text = if (revealed) password else if (password.isEmpty()) "" else "••••••••",
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { revealed = !revealed }) { Text(if (revealed) "Hide" else "Reveal") }
            TextButton(onClick = {
                onCopy(password) // copies + (re)arms the app-scoped 15s auto-clear
                copied = true
            }) { Text("Copy") }
        }
        if (copied) {
            Text("Copied — clipboard auto-clears in 15s (and on lock)", style = MaterialTheme.typography.bodySmall)
        }
    }
    HorizontalDivider()
}
