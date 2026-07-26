# Installing vaultis on an Android phone

Step-by-step, from a clean Linux machine to the app running on your phone. Everything
here happens over USB — nothing is uploaded anywhere, and the app has no network
permission at all.

Before you start, please read **[Which build should I install?](#which-build-should-i-install)**
below. It is the one decision in this document with a real security consequence, and
right now it is an awkward trade-off rather than a clean choice.

---

## 0. What you need

- The phone, its USB cable, and the ability to enable Developer options on it.
- This repo, on a computer.
- About 4 GB of disk for the toolchain and ~15 minutes for the first build.

An **arm64** phone (every mainstream Android phone made in the last decade) — the APK
ships `arm64-v8a` and `x86_64` (emulator) only. It requires **Android 7.0 or newer**
(minSdk 24).

---

## 1. Install the build toolchain (once)

The repo ships a no-sudo installer that puts a JDK, the Android SDK + NDK, the Rust
Android targets, and `cargo-ndk` under `~/toolchains`:

```bash
bash mobile/scripts/install-android-toolchain.sh
source ~/toolchains/env.sh
```

`env.sh` sets `JAVA_HOME`, `ANDROID_HOME`, `ANDROID_NDK_HOME` and `PATH`. **You must
`source` it in every new terminal** you build or run `adb` from. If you already use
Android Studio, its SDK + NDK and a JDK 17 work just as well.

---

## 2. Build the app

```bash
source ~/toolchains/env.sh
mobile/gradlew -p mobile :composeApp:assembleDebug
```

Output:

```
mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
```

This one command cross-compiles the audited Rust core (`libvaultis_ffi.so`) for each
ABI, generates the Kotlin bindings from it, and packs the lot into the APK. The first
run takes several minutes; later ones are seconds.

---

## 3. Put the phone in developer mode

On the phone:

1. **Settings → About phone** → tap **Build number** seven times. It will say you are
   now a developer.
2. **Settings → System → Developer options** → turn on **USB debugging**.
3. Plug the phone into the computer. A dialog asks you to **Allow USB debugging** —
   accept it, and tick "always allow from this computer" if you want.

Check the computer can see it:

```bash
source ~/toolchains/env.sh
adb devices -l
```

You should see your phone listed as `device`. If it says `unauthorized`, the dialog on
the phone has not been accepted yet. If nothing is listed at all, try another cable —
charge-only cables are extremely common and carry no data.

**Turn USB debugging back off when you are done.** Leaving it on means anyone who gets
the phone and a cable has a much easier time with it.

---

## 4. Install the APK

```bash
adb install -r mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
```

`vaultis` now appears in the app drawer.

<details>
<summary>No cable? Installing the APK by hand instead</summary>

Copy the `.apk` to the phone by any means you trust (USB file transfer, an SD card),
then open it with the phone's file manager. Android will ask you to allow that file
manager to install unknown apps — you have to grant it, and you should turn it back off
afterwards.

Do not email the APK to yourself or put it through a cloud drive if you can avoid it.
It is not secret, but a copy in a mailbox is a copy an attacker could later swap for a
modified one.
</details>

---

## 5. Get your vault onto the phone

The app has **no import screen yet** (it is on the roadmap), and nothing syncs — you
move the encrypted files yourself, over USB.

On the computer, make a backup of your vault (this copies the encrypted `vault.pmv`
plus the `manifest/` and `volume/` folders):

```bash
vaultis backup <path-to-your-vault> /tmp/vault-for-phone
```

Then push it into the app's private storage. Android does not let `adb push` write
there directly, so it goes via a scratch directory:

```bash
adb push /tmp/vault-for-phone/. /data/local/tmp/vaultis-import/
adb shell run-as com.vaultis mkdir -p files/vault
adb shell "run-as com.vaultis sh -c 'cp -r /data/local/tmp/vaultis-import/. files/vault/'"
adb shell rm -rf /data/local/tmp/vaultis-import      # don't leave a copy in world-readable scratch
rm -rf /tmp/vault-for-phone                          # and none on the computer either
```

Open the app and unlock with the same two passwords, in the same order. The encrypted
format is identical on desktop and mobile, so there is no conversion step.

> `run-as` only works on a **debuggable** build — which is why the debug APK is
> currently the only one you can actually load a vault into. See the next section.

---

## Which build should I install?

There are two, and neither is currently ideal.

| | Debug APK | Release APK |
|---|---|---|
| Signed with | the **public** Android debug key | **your** private keystore |
| `run-as` (shell access to app data) | **allowed** | blocked |
| Can you load your vault into it today? | **yes**, via step 5 | **no** — there is no import UI yet |
| Debugger can attach to the running app | **yes** | no |

**What the debug build actually costs you.** `run-as` and debugger attachment both
require someone to have the *unlocked* phone plus a cable plus USB debugging enabled.
They do not open a remote hole, and they do not weaken the encryption — a copy of the
vault files is still ciphertext that needs both passwords. But if someone has your
unlocked phone, a debug build hands them the vault files far more easily than a release
build would, and it lets them attach to the process while the vault is open.

The debug key being public matters for a different reason: **anyone can build an APK
that Android considers a legitimate update to yours.** A release keystore is what makes
"is this really my app?" a question with an answer.

**Recommendation.** Use the debug build only as long as you need to, treat the phone as
a convenience copy rather than the authoritative vault, and keep USB debugging off
except while you are actually transferring. The in-app encrypted import is the fix that
makes a release-signed install genuinely usable; until it lands, this is the honest
state of things.

### Building a release-signed APK

Create a keystore once (keep it somewhere safe and backed up — losing it means you can
never update the app in place):

```bash
source ~/toolchains/env.sh
keytool -genkeypair -v -keystore ~/vaultis-release.jks \
  -alias vaultis -keyalg RSA -keysize 4096 -validity 10000
```

Then build, passing the keystore in from outside the repo (never commit it):

```bash
PM_KEYSTORE=~/vaultis-release.jks \
PM_KEYSTORE_PASSWORD=... \
PM_KEY_ALIAS=vaultis \
PM_KEY_PASSWORD=... \
mobile/gradlew -p mobile :composeApp:assembleRelease
```

The build **warns loudly and falls back to the debug key** if no keystore is
configured, so a "release" APK you did not sign yourself is never silently mistaken for
a real one.

---

## Everyday use

- **Unlock:** both passwords, in the order you set them.
- **Auto-lock:** the vault locks when you leave the app, and after two minutes without
  touching the screen. Locking discards the key from memory; you unlock again from scratch.
- **Reveal / copy a password:** the copy is marked sensitive, is wiped from the
  clipboard after 15 seconds, and is wiped immediately if you lock before then.
- **Screenshots and screen recording are blocked**, and the app shows blank in the
  app-switcher.
- **Read-only:** v1 views but cannot change anything. Create and edit on the desktop,
  then re-copy the vault across with step 5.

## Keeping it up to date

The phone copy does not update itself. After you change things on the desktop, repeat
step 5 to push a fresh copy. Watch the **"Last opened: … (generation N)"** line the app
shows after unlocking — the generation only ever goes up, so a number lower than you
remember means you are looking at an older copy than you think.

## Removing it

Uninstalling the app deletes its private storage, and with it the phone's copy of the
vault:

```bash
adb uninstall com.vaultis
```

Your desktop vault and your backups are untouched.

---

## Before you trust the phone copy

Putting the vault on a phone genuinely widens the attack surface, and some of what it
widens is not something this app can close. **[`docs/THREAT_MODEL.md`](../docs/THREAT_MODEL.md)**
sets out plainly what spyware on a phone (or on a desktop) can and cannot do to this
data, and what actually helps. Please read it before deciding what to put on the phone.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `adb: no devices/emulators found` | USB debugging off, dialog not accepted, or a charge-only cable. |
| `adb devices` shows `unauthorized` | Accept the "Allow USB debugging" prompt on the phone. |
| `INSTALL_FAILED_UPDATE_INCOMPATIBLE` | A differently-signed vaultis is already installed. `adb uninstall com.vaultis` first — this deletes the phone's vault copy. |
| App says "No vault found" | Step 5 did not land the files. Check with `adb shell run-as com.vaultis ls files/vault`. |
| "Wrong passwords, or the vault is damaged" | Both passwords, in the right order. The message is deliberately the same for a wrong password and a damaged vault, so it cannot be used to test guesses. |
| `run-as: unknown package` | You installed the release APK; `run-as` only works on debug builds. |
| Gradle cannot find a JDK or the NDK | You did not `source ~/toolchains/env.sh` in this terminal. |
