# Threat model — what spy software can and cannot do to your vault

This document answers one question as plainly as possible:

> **If something malicious is running on my phone or my computer, can it read my vault?**

The short answer is **it depends entirely on whether the vault is locked or unlocked at
the moment the malicious code is running**, and that distinction is the whole of this
document.

This is written to be honest rather than reassuring. Where the answer is "yes, and we
cannot stop it," it says so.

---

## The one-paragraph version

Your vault on disk is a strongly encrypted file. Spyware that copies it gets ciphertext
and is left guessing two passwords — which, with the key derivation this uses, is not a
practical attack. **But once you type both passwords and unlock, the contents are
plaintext in the app's memory and on your screen, and software already running on that
device with enough privilege can read them.** No password manager can prevent this,
including this one. Encryption protects data *at rest* and data that *leaves* the
device. It cannot protect data that is being *used* on a device that is already
compromised.

So the practical rule is: **the vault is exactly as trustworthy as the device you unlock
it on.**

---

## What the encryption actually covers

These properties hold even against an attacker who has your vault files, your backups,
and unlimited time on their own hardware:

- **Two passwords, chained through Argon2id**, then **XChaCha20-Poly1305** for the
  contents. Both passwords are required; there is no recovery path and no back door.
- **The whole file header is authenticated** — the parameters, salt, and nonce cannot be
  altered without detection, so an attacker cannot weaken the key derivation of a vault
  file and hand it back to you.
- **Tampering fails closed.** Every read is authenticated. Damage or modification
  surfaces as an explicit error, never as silently wrong data. (A test flips every
  single bit of a valid vault file and asserts all of it is rejected without a crash.)
- **A wrong password and a corrupt file give the identical error**, on desktop and
  mobile, so the app cannot be used as an oracle to test password guesses.
- **Nothing is uploaded, ever.** There is no network code in the program. The Android
  app does not even request the `INTERNET` permission, so the operating system itself
  would refuse an attempt.

If someone steals the phone, steals the laptop, steals a backup drive, or takes a copy
of the encrypted files off any of them — this is the part that protects you, and it
holds.

---

## Quantum computers, and "harvest now, decrypt later"

The concern is exact and reasonable: an estate vault must stay secret for *decades*, and
someone who copies the encrypted file today can simply keep it until better machines
exist. So: is this vault's encryption going to survive that?

**The short answer is yes, and mostly for a structural reason rather than a clever one.**

### Why Shor's algorithm — the one that matters — does not apply

Nearly all real "harvest now, decrypt later" risk in the world is about **public-key**
cryptography: RSA, Diffie-Hellman and elliptic curves, which Shor's algorithm breaks
outright. That is what threatens recorded TLS sessions, PGP mail and encrypted messengers,
because their session keys are wrapped in public-key operations.

**vaultis contains no public-key cryptography at all.** There is no key exchange, no
certificate, no signature, no recipient key — because there is nothing to exchange keys
*with*. The vault is a local file encrypted directly from your two passwords. Shor's
algorithm has nothing to factor and no discrete log to solve. This is not a parameter that
was tuned well; it is a consequence of the program being offline by design, and it removes
the entire category.

### Grover's algorithm, and why 256 bits is the right answer

The quantum attack that *does* apply to symmetric ciphers is Grover's algorithm, which
gives a **quadratic** speed-up on brute-force search — effectively halving key strength.

vaultis uses **XChaCha20-Poly1305 with a full 256-bit key**, so Grover leaves roughly
**128-bit** security. That is the standard post-quantum-safe target: it is exactly why NIST
points at 256-bit symmetric keys, and it is not a number anyone expects to reach —
Grover's search is also inherently *sequential*, so it parallelises poorly and cannot be
brute-forced by simply building more machines.

**There is nothing to "upgrade" the cipher to.** Post-quantum cryptography — ML-KEM
(Kyber), ML-DSA (Dilithium) and friends — replaces key exchange and signatures. It does not
replace symmetric encryption, because quantum computers do not break symmetric encryption;
they only weaken it by the square root, which a 256-bit key already absorbs. Bolting a PQC
algorithm onto this design would add moving parts and protect nothing.

### The part that actually decides it: your two passwords

An attacker with your encrypted vault will not attack ChaCha20. They will **guess your
passwords**, because that is the cheaper path by an enormous margin — today and in any
quantum future. The key is only as strong as the two secrets it is derived from.

Two things stand in their way:

- **Password entropy.** This dominates everything else on this page. Two long, unique,
  unguessable passwords are what make the vault survive; a memorable phrase reused from
  somewhere else means the cipher never gets attacked at all.
- **Argon2id**, at 64 MiB and 3 passes, run **twice** (once per password, chained). Being
  *memory-hard* matters here specifically: Grover would have to evaluate the whole
  memory-hard function in superposition, which needs quantum RAM proportional to the memory
  cost. Coherent quantum memory on that scale is far beyond anything on the horizon, so
  memory-hard password hashing is one of the *worst* possible targets for a quantum
  speed-up. The KDF is doing more for post-quantum safety than the cipher choice is.

### If you want to turn it up

The Argon2id cost is baked into a vault when it is **created** and cannot be changed
afterwards without re-creating it. It can be raised at creation time:

```bash
VAULTIS_KDF_MCOST_MIB=256 VAULTIS_KDF_TCOST=4 vaultis import-tree <mirror> <new-vault-dir>
```

Accepted range: **1–512 MiB** memory, **1–16** passes. Anything outside it, or unparseable,
falls back to the default with a warning rather than writing a vault the reader would
refuse.

**Think hard before raising it.** That cost is paid on *every* open, forever, on *every*
device. A 512 MiB vault may be openable on your desktop and simply refuse to open on a
phone. For an estate vault, **"my executor could not open it" is a far more likely
catastrophe than "a quantum computer read it in 2050"** — and unlike the quantum scenario,
it is one you can cause yourself today. Add entropy to your passwords first: it costs
nothing at open time and buys strictly more than the KDF knob does.

### The honest bottom line on quantum

The cryptography here is in good shape against a future quantum adversary, and the reason
is unglamorous: no public-key crypto to break, a 256-bit symmetric key, and a memory-hard
KDF. The realistic failure mode for a vault harvested today is not a quantum computer — it
is that the two passwords were guessable, or that the plaintext leaked from a device while
the vault was unlocked (everything else on this page). The strongest thing you can do about
"harvest now, decrypt later" is still to **not let the file be harvested**: keep it off
cloud storage, keep backups on media you physically control, and treat every copy as
permanent.

## What it does not cover

| The attacker can… | Can they read your vault? |
|---|---|
| Steal the encrypted files, a backup, or the whole device (locked) | **No.** Ciphertext; they need both passwords. |
| Steal the device while the vault is **unlocked on screen** | **Yes** — it is on the screen. |
| Run code as your user while the vault is **locked** | **No**, but they can wait — see the next rows. |
| Log your keystrokes | **Yes.** They capture both passwords as you type them, and then everything. |
| Read the app's memory while unlocked (root / admin / debugger) | **Yes.** The decrypted contents are in there by necessity. |
| Read what is on your screen (accessibility scraping, screen share, a camera) | **Yes**, for whatever you have open. |
| Replace the app itself with a modified build | **Yes**, and then everything above. |
| Have physical access to a powered-off, encrypted device | **No.** |

The pattern: **anything that gets between you and the app at the moment you use it wins.
Anything that only touches the stored data loses.**

---

## On the phone

### What Android gives you, and what this app adds

- **The app sandbox.** The vault lives in app-private storage
  (`/data/data/com.vaultis/files/vault/`). Other installed apps cannot read it — this is
  enforced by the OS, not by us. **This is the main thing protecting the file on a
  healthy phone, and root defeats it entirely.**
- **Storage encrypted to your screen lock.** Android ties app-private storage to your
  lock credential; it is not readable until the first unlock after boot. **A phone with
  no PIN/pattern/password loses this protection.** Set a screen lock.
- **No network permission.** The app cannot exfiltrate anything even if it wanted to,
  and any future version that tried would need a permission change you would see.
- **Excluded from cloud backup and from device-to-device transfer**, so the encrypted
  vault cannot be swept up by Google backup or copied onto a new phone during migration.
- **Screenshots, screen recording, and the app-switcher thumbnail are blocked**
  (`FLAG_SECURE`).
- **Auto-lock** when you leave the app, and after two minutes without touching the
  screen. Locking discards the key.
- **Clipboard hardening.** A copied password is marked sensitive (so Android 13+ does not
  render it in the paste preview, and history-aware keyboards are asked not to keep it),
  is wiped after 15 seconds, and is wiped immediately on lock.
- **Overlay tap-jacking is blocked.** Touches are discarded while another app's window is
  drawn on top, so a malicious overlay cannot trick you into tapping "Reveal" or "Copy"
  underneath its own interface.

### What defeats all of that

Listed roughly in order of how likely they are in practice, not how sophisticated:

1. **A malicious accessibility service.** This is *the* Android spyware technique —
   commodity stalkerware and banking trojans both use it. An app you granted the
   "Accessibility" permission to can read the text of everything on screen.
   **`FLAG_SECURE` does not stop it**; that flag blocks screen *capture*, not the
   accessibility APIs. If such an app is active while you have an entry open, it can read
   that entry. **Go to Settings → Accessibility and turn off anything you do not
   specifically recognise and need.**
2. **A malicious or careless keyboard.** Whatever keyboard is set as your input method
   sees every character you type, including both master passwords. The password fields
   ask the keyboard not to learn or suggest their contents, but that is a request a
   hostile keyboard simply ignores. **Type your master passwords on the phone's stock
   keyboard.**
3. **Root, an unlocked bootloader, or a custom recovery.** Any of these ends the
   discussion: app-private files are readable, process memory is readable, and
   instrumentation frameworks can hook the app directly. **Do not root a phone you keep
   this vault on.**
4. **A device administrator / MDM profile.** A work profile or "security" app with device
   admin can have broad powers over the device. If your employer controls the phone,
   treat the phone as your employer's.
5. **A modified build of vaultis.** An APK that looks like this one but is not. This is
   what signing keys are for — see
   [Which build should I install?](../mobile/INSTALL_ANDROID.md#which-build-should-i-install).
6. **Someone reading over your shoulder.** The password mask is a fixed width that does
   not reveal the length, and the app blocks screenshots — but none of that helps against
   a person, or a camera, looking at the screen.

### Two things about the phone build specifically

- **The key is not locked out of swap on mobile.** The desktop pins the derived key out
  of swap (`mlock`); phones grant apps no budget for that, so the mobile build compiles
  that off. In practice Android does not use swap by default and iOS encrypts its swap,
  and the key is still wiped when the vault locks — but it is a real, disclosed
  difference.
- **Decrypted values pass into managed memory.** Viewing an entry hands its fields to
  Kotlin as strings, in a garbage-collected heap the Rust side cannot wipe. The app
  minimises how often that happens and clears what it can, but after you have viewed a
  record, treat that app's memory as containing it until the vault locks.

---

## On the desktop

The desktop is *not* categorically safer — it is differently exposed. Same rule: the
stored file is safe, the unlocked session is not.

**What the desktop does better:** the derived key is locked out of swap; there is no
managed-heap copy of your secrets (everything is wiped on drop); password buffers are
overwritten in place rather than left for a garbage collector; and a single-writer lock
prevents two sessions corrupting each other.

**What is worse or simply absent:**

- **No screen-capture protection.** There is no desktop equivalent of `FLAG_SECURE`. Any
  screen-recording or remote-desktop software sees the window.
- **No idle auto-lock.** The desktop app does not lock itself after inactivity the way
  the phone app does. **Close it when you walk away.**
- **Same-user malware can read the process's memory.** On a typical desktop OS, anything
  running as *you* can attach a debugger to another of *your* processes. It does not need
  root.
- **Clipboard managers.** The app asks the system not to retain a copied password in
  clipboard history and wipes it after 15 seconds, but a clipboard manager that keeps its
  own history may ignore the hint.
- **Core dumps.** If the program crashes, a core dump could in principle contain
  decrypted contents. This is a known, accepted residual — see `docs/HARDENING.md`.
- **Backups you make are as sensitive as the vault.** They are encrypted with the same
  two passwords, which is exactly why they are safe to keep on a separate drive — and
  exactly why losing both passwords loses the backups too.

---

## What actually helps

For **security** (keeping others out), in rough order of value:

1. **Choose the device you unlock on deliberately.** A phone you install games and random
   utilities on is a worse place to unlock this than a computer you keep boring. The most
   effective control available to you is deciding where the vault gets opened at all.
2. **Screen lock on the phone, disk encryption on the computer.** Both are what make a
   stolen device merely a lost device.
3. **Audit Accessibility permissions on the phone** and keep the stock keyboard for typing
   master passwords. These two cover the realistic Android spyware vectors.
4. **Do not root or jailbreak. Keep the OS updated.** Most real-world compromise is an
   unpatched OS, not a clever attack on the app.
5. **Two long, unique passwords, never typed anywhere else**, in an order you will
   remember. The key derivation is deliberately slow, which makes guessing impractical —
   but only if the passwords are not guessable and are not reused somewhere breachable.
6. **Lock or close the app when you step away.** The phone does it for you after two
   minutes; the desktop does not.
7. **Turn USB debugging back off** after transferring anything to the phone.
8. **Prefer viewing to copying.** A revealed password is on screen; a copied one is on the
   clipboard, which is a shared surface.

For **safety** (not losing the data), which is the risk that actually materialises far
more often than an attacker does:

1. **Back up regularly, and keep a copy somewhere else physically.** The vault is a file.
   Files die with the disks under them. Use the built-in Backup button.
2. **Write both passwords down and store them safely.** There is no recovery. A sealed
   envelope in a safe, or with a lawyer or trusted person, is not a weakness — for an
   estate vault it is the entire point. The far more common failure is a family that
   cannot open the vault at all, not a burglar who can.
3. **Test your backup.** Actually open it, once. An untested backup is a hope.
4. **Tell your executor the vault exists and how to get in.** Perfect encryption plus
   nobody knowing it is there equals the information being gone.
5. **Watch the "Last opened" line** the app shows after you unlock — but know exactly what
   it does and does not catch. It is refreshed only by a **writable** open (desktop
   `--write`/Edit mode). The desktop *defaults* to read-only and the phone app is
   read-only always, so **someone who opens your vault merely to READ it leaves this
   timestamp untouched.** It detects unauthorised *edits*, not unauthorised *reads* —
   which is the more likely intrusion. Treat an unfamiliar time as a definite alarm, but
   never treat an unchanged one as proof that nobody has been in. The same caveat applies
   to the generation number: it advances on save, so a lower-than-remembered value means
   you are looking at an older copy of the file, while an unchanged one proves nothing.

---

## The honest bottom line

This program is built so that the encrypted vault — on your disk, in your backups, on a
stolen laptop, on a lost phone — is genuinely hard to break, and the audit work behind
that claim is documented in [`HARDENING.md`](HARDENING.md).

It is **not** built to survive a device that is already compromised at the moment you use
it, because nothing is. If you have real reason to believe a device has spyware on it, do
not unlock the vault on that device, and change what was in it from a device you trust.
