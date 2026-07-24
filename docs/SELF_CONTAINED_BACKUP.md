# vaultis — Self-Contained Backup Executable (Design Proposal)

_Status: **PROPOSED — not implemented.** Design notes for discussion. Last updated: 2026-06-25._

## 1. Goal

From **Config → "Generate a backup"**, in addition to (or instead of) the current
encrypted-directory backup, produce a single file:

```
vaultis-backup-<timestamp>.exe
```

that is:

- **Self-contained** — it embeds the entire encrypted vault (the `vault.pmv`
  header/body plus the `volume/` document blobs and `manifest/` index).
- **Directory-free** — it needs no surrounding vault directory; the data travels
  inside the executable itself.
- **Read-only** — it can be opened only in read mode (view, copy, export
  documents), never to edit, save, merge, compact, or re-key.

Opening it still requires **both master passwords** — embedding the vault in an
executable changes packaging, not the cryptographic protection.

## 2. Feasibility — short answer

**Yes, doable.** This is the classic *self-extracting / self-contained executable*
pattern (makeself, PyInstaller one-file, Go `embed`, NSIS): a program *stub* with
opaque data glued onto the end, where the program reads its own file at startup to
recover the data. The vault is already a blob of AEAD-encrypted bytes, so embedding
it is straightforward; the interesting work is the container format, the startup
bootstrap, and the read-only mounting. The constraints below (platform binding,
"self-contained GUI" vs CLI, and the unsigned-binary trust model) are the parts to
decide deliberately, not technical blockers.

## 3. Container format

Lay the output file out as:

```
[ stub binary ][ archive blob ][ footer (fixed 32 bytes) ]
```

### 3.1 Footer (trailer read from EOF)

A fixed 32-byte trailer, read by seeking to `file_len - 32`:

| field         | size | meaning                                                        |
| ------------- | ---- | -------------------------------------------------------------- |
| `magic`       | 8    | `"PMSELFX1"` (vaultis self-extract, format v1)                |
| `archive_off` | u64  | byte offset where the archive blob begins                      |
| `archive_len` | u64  | length of the archive blob                                     |
| `archive_hash`| 8    | truncated BLAKE3 of the archive (integrity only, not security) |

The magic at the tail is the embedded-mode signal. `archive_off` (rather than just
length) is what lets the generator cleanly **strip an existing overlay** when a
backup is generated *from* a backup (`stub = bytes[0..archive_off]`).

Appending bytes to the **end** of a PE / ELF / Mach-O does not affect execution: the
loader reads headers and sections at known offsets near the start; trailing bytes are
ignored. This is exactly why self-extractors work.

### 3.2 Archive blob (vault tree → one blob)

A trivial TAR-lite: a small index of `(relpath, len)` entries followed by the
concatenated file bytes. It holds exactly the encrypted vault tree:

- `vault.pmv`
- `manifest/manifest.0`, `manifest/manifest.1`, …
- `volume/vol.0`, `volume/vol.1`, …

**Validation at load time:** restrict relpaths to that exact allowed set (the fixed
`vault.pmv` / `manifest/<n>` / `volume/<n>` shape), rejecting anything else — no
traversal, no surprise paths. The *contents* stay AEAD-encrypted, so structural
tampering of the overlay only makes the subsequent open **fail closed**. Crucially,
this reuses the **already-audited** read path (non-contiguous-partition detection,
bounded/symlink-safe reads, AEAD body checks), so "untrusted overlay bytes" is
already covered by the existing threat model.

## 4. Backing the read — extract-to-temp vs in-memory

The vault is normally a *directory* of files, and the storage layer reads `vol.N`
frames with positioned file reads. So at open time the embedded build must present
those bytes somehow. Two options:

### 4.1 Extract-to-temp (recommended MVP)

Write the archive's encrypted files into a `0700` temp directory, `open_read_only`
it, and delete on exit.

- **Pros:** *near-zero core change.* It reuses the existing read-only open and is
  essentially the inverse of today's `backup` (which copies the same encrypted
  files). Lives almost entirely in the desktop crate.
- **Cons:** transient encrypted files on disk; a crash can leave (encrypted,
  harmless) leftovers; not *literally* "no directory touched."
- **Note:** it leaks **no additional secret** — the temp bytes are exactly the
  encrypted bytes already inside the executable.

### 4.2 In-memory (clean v2)

A read-only `MemoryBackend` behind the storage layer, so nothing touches disk.

- **Pros:** the true "self-contained, nothing on disk" version.
- **Cons:** requires abstracting the (currently fs-coupled) read path behind a
  trait, e.g. `trait FileBackend { read_manifest(part), read_frame_at(part, off,
  len), read_vault_header(), … }`, with a `FileBackend` and a `MemoryBackend`
  implementation. Read-only drops the hard parts (no write lock, no fsync, no
  `O_NOFOLLOW`, no rebuild), so this is *moderate*, not large.

**Recommendation:** ship **extract-to-temp first**, and only build the in-memory
backend if the transient-temp property proves undesirable. Document export and all
read paths behave identically either way.

## 5. Startup bootstrap

At `main` entry, **before** normal argument parsing:

1. Read the footer via `std::env::current_exe()`. (A running executable is
   write-locked on Windows but always *readable*, so self-read works on
   Windows / macOS / Linux.) If the magic is absent → behave as normal `vaultis`.
2. If embedded → enter **embedded read-only mode**:
   - force `writable = false`; ignore `--write`;
   - skip the start-page directory picker (the vault is implied);
   - mount the vault (extract-to-temp or in-memory);
   - drop straight to the unlock prompt.
3. The heir enters both master passwords → views records, reveals/copies secrets,
   and exports documents. No mutation is possible.

## 6. Read-only enforcement

Read-only mode already exists, so the bootstrap mainly hard-wires it. In embedded
mode, disable every write action:

- save / edit / delete records and category types;
- change master passwords; set volume size / redundancy;
- merge (update-from), compact, import-tree;
- generating further backups.

**Document export stays enabled** — the heir legitimately needs to pull the actual
PDF / will / statement out of the vault, and that is already an allowed read-only
operation in the current design.

## 7. Edge cases

- **Backup-of-a-backup:** if the running binary is itself an embedded build, strip
  the prior overlay using `archive_off` before appending the fresh archive + new
  footer (no double-embedding).
- **Self-read on Windows:** the running `.exe` is write-locked but readable;
  writing the *new* output file is unaffected.
- **Tamper / corruption of the overlay:** a swapped or truncated archive makes the
  open **fail closed** (AEAD body + the audited partition/bounds checks). No
  forgery, no silent data loss.
- **Temp cleanup (extract-to-temp):** remove the temp dir on clean exit; a crash
  leaves encrypted, non-sensitive leftovers. In-memory avoids this entirely.
- **Size / RAM:** the archive includes document blobs. Extract-to-temp streams to
  disk (low RAM); in-memory holds everything in RAM (fine for typical vaults,
  notable for hundreds of MB of attachments).

## 8. Constraints to decide deliberately

These are not blockers, but they shape what "self-contained" actually means.

### 8.1 Platform binding

The output is a copy of the **currently running** binary + the overlay, so it runs
only on the **same OS / architecture** as the machine that generated it. You cannot
mint a Windows `.exe` from Linux without shipping a Windows stub. For estate planning
where the heir may use a different OS, this matters; supporting it means bundling
per-platform stubs.

### 8.2 "Self-contained GUI" is the hard part

The **CLI / TUI** already builds as a **fully-static single file** (musl), which
absorbs cleanly into a real self-contained executable. The **GUI** dynamically links
the desktop graphics stack (X11 / Wayland / Mesa on Linux; Direct3D / the VC++
runtime on Windows), so a copied GUI binary still needs those libraries present on
the target — *not* truly self-contained. Realistically the self-contained backup
embeds the **static CLI / TUI viewer** (heir runs it in a terminal). A windowed,
double-click GUI would require bundling the graphics libraries, accepting
"not fully self-contained," or a heavier installer.

### 8.3 Delivery friction (unsigned executable)

An unsigned, self-extractor-shaped executable trips:

- **SmartScreen** ("unknown publisher") on Windows;
- **Gatekeeper** on macOS (unsigned binaries are blocked without an override);
- **antivirus heuristics** (appended-overlay binaries resemble malware patterns);
- **mail filters** — Gmail and most providers block `.exe` attachments.

Smoothing this needs **code-signing** (a paid certificate + identity), and
sign-then-append is fiddly: a generic append breaks an Authenticode signature
(Windows has a specific certificate-overlay area, but generic trailing data does
not use it).

## 9. Security analysis

- **The data is as safe as the vault directory.** It is the same AEAD-encrypted
  bytes; opening still requires both master passwords; a swapped/tampered embedded
  vault **fails closed**. Embedding exposes nothing new about the contents.
- **The new risk is running an unsigned executable.** With a plain vault directory,
  the heir uses a viewer *they* obtained and can audit. With a self-contained `.exe`,
  the heir must trust that whoever produced it embedded a legitimate, untampered
  `vaultis` — a malicious build could capture the password they type. This is a
  **weaker trust model** than "encrypted data + independently-obtained, auditable,
  open-source viewer." Reproducible builds + a published hash help in theory, but
  heirs will not realistically verify them.
- **Conclusion:** the self-contained executable is a **convenience**, not a security
  improvement. It should be offered *alongside* the plain encrypted backup (which
  remains the secure, portable, auditable default), with explicit warnings about
  platform binding and the unsigned-binary trust caveat.

## 10. Touch points & rough effort

- **New `embed.rs` (desktop crate):** footer read/write, archive pack/unpack, own-exe
  handling, overlay-strip. *Small.*
- **`main.rs`:** early embedded-mode detection + bootstrap branch + write-action
  lockdown. *Small.*
- **Config UI (`gui.rs` / `ui.rs`):** a write-mode-only "Generate self-contained
  backup" action. *Small.*
- **Core (`vaultis-core`):** **none** for the extract-to-temp MVP; add a
  `FileBackend` trait + `MemoryBackend` only if/when in-memory mounting is wanted.

The MVP is therefore **mostly desktop-crate work that reuses already-audited read
paths** — the appealing part of this approach.

## 11. Recommended MVP scope

- Stub = the **static CLI / TUI** viewer (the only genuinely self-contained option).
- **Same-platform** output, **extract-to-temp** mounting, **read-only**, document
  export allowed.
- Presented as an **opt-in convenience** beside the plain encrypted backup, with
  clear unsigned-binary and platform-binding warnings.

## 12. Open questions (decide before building)

1. **Who runs it, and on what?** Heir on Windows? You on the same machine?
   Cross-platform needed? — decides whether single-stub platform binding is
   acceptable or per-platform stubs are required.
2. **Windowed double-click, or is a static CLI / TUI viewer acceptable?** — decides
   whether "self-contained" is actually achievable or whether we must bundle the
   graphics stack / accept partial self-containment / code-sign.
3. **Extract-to-temp now, in-memory later?** — or invest in the `FileBackend`
   abstraction up front for the "nothing touches disk" property.
4. **Replace or augment the existing backup?** Recommendation: **augment** — keep the
   plain encrypted backup as the secure default.
