# Desktop shortcuts & icons

Two shortcuts launch the **same** windowed binary (`vaultis-gui`) in two modes,
each with its own vault icon:

| Shortcut | Launches | Icon |
|----------|----------|------|
| **vaultis (View)** | `vaultis-gui` (read-only) | `vaultis-locked.*` — a **locked** vault |
| **vaultis (Edit)** | `vaultis-gui --write` | `vaultis-unlocked.*` — an **unlocked** vault |

The locked vault is the safe, view-only default; the unlocked vault means "you can
change things". They are the same vault drawing — only the padlock state and colour
(blue/closed vs amber/open) differ, so a glance at the desktop tells you which mode
you're about to open.

## The icon files (`icons/`)

`icons/make_icons.py` draws them with Pillow (`pip install pillow`), producing both
formats from one source:

```bash
python3 packaging/icons/make_icons.py
```

- `vaultis-locked.png` / `vaultis-unlocked.png` — 512 px, for Linux.
- `vaultis-locked.ico` / `vaultis-unlocked.ico` — multi-size (16–256 px), for
  Windows shortcuts.

The generated files are committed, so you only need to re-run the script if you
want to tweak the artwork.

> **If you change how the `.ico` files are written, keep the frame formats.** Windows
> only decodes a **PNG-compressed** icon frame at **256 px**; at 16/32/48/64/128 it
> requires **BMP/DIB**, and silently falls back to a **generic icon** when it cannot
> decode one. Explorer draws shortcut icons at 32–48 px, so an all-PNG `.ico` looks
> completely iconless on the Desktop while still previewing fine in image viewers and
> on Linux — which makes it an easy bug to ship. `make_icons.py` writes the mixed
> layout by hand (`save_ico`/`dib_frame`) precisely because Pillow's `sizes=` shortcut
> produces an all-PNG file and its `bitmap_format="bmp"` produces an all-BMP one.
>
> To check a rebuilt icon without a Windows machine:
>
> ```bash
> file packaging/icons/vaultis-locked.ico
> # want: "256x256 with PNG image data ... 128x128, 32 bits/pixel"
> # NOT:  "16x16 with PNG image data"   <- the broken, all-PNG shape
> ```

> **You may not need to run any of this by hand.** `scripts/build.sh` (Linux) and
> `scripts\build.bat` (Windows) install the shortcuts themselves as their last step,
> pointed at the binary they just built. Pass `--no-shortcuts` to skip it; `--no-sample`
> skips it too, since that flag means "just build". The rest of this file is for
> installing them separately — most usefully against a *permanent* copy of the binary,
> as described below.

## Linux

One command installs both shortcuts for the current user (onto the Desktop and into
the application menu):

```bash
# uses target/release/vaultis-gui if present, else target/debug, else $PATH;
# or pass the binary path explicitly:
packaging/linux/install-shortcuts.sh /usr/local/bin/vaultis-gui
```

It copies the PNG icons to `~/.local/share/icons/vaultis/`, fills the `Exec=` /
`Icon=` paths into the two `.desktop` files, installs them to
`~/.local/share/applications/`, and copies them to your Desktop (marking them
trusted on GNOME so they run on double-click).

To do it by hand instead: edit `linux/vaultis.desktop` and `linux/vaultis-edit.desktop`,
replacing `__BIN__` with the path to `vaultis-gui` and `__ICONDIR__` with the folder
holding the PNGs, then copy both files to `~/Desktop` and `~/.local/share/applications/`
and `chmod +x` them.

**Two things to know on Linux:**

- **First double-click.** On GNOME a brand-new desktop launcher may need a one-time
  right-click → **Allow Launching** before it will run, even though the installer
  marks it trusted (`gio set … metadata::trusted true`).
- **Point at a stable binary.** The shortcuts store the *absolute path* to
  `vaultis-gui`. If you pass a path inside a build tree (`target/release/…`), a later
  `cargo clean` or moving the repo breaks them. For a permanent setup, copy the binary
  somewhere lasting and install against that:

  ```bash
  install -Dm755 target/release/vaultis-gui ~/.local/bin/vaultis-gui
  packaging/linux/install-shortcuts.sh ~/.local/bin/vaultis-gui
  ```

## Windows

> **First build the app.** `vaultis-gui.exe` is a build artifact — it is **not**
> committed to the repo, so it will not be in `packaging\windows\`. Produce it with
> `cargo build --release` (→ `target\release\vaultis-gui.exe`), or copy a prebuilt
> exe somewhere and point the script at it.

**Simplest — right after building in the repo.** The script auto-finds the exe
(`target\release`, then `target\debug`, then the windows-gnu cross target, then your
`PATH`) and the committed icons in `packaging\icons`:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\make-shortcuts.ps1
```

**Point at the exe explicitly** (e.g. a prebuilt one you copied):

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\make-shortcuts.ps1 `
    -Exe "C:\apps\vaultis\vaultis-gui.exe"
```

**Deployed install** — for a permanent setup, copy `vaultis-gui.exe` **and** the
`packaging\icons` folder into one stable directory (so the shortcuts don't point into
a build tree), then:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\make-shortcuts.ps1 `
    -InstallDir "C:\Program Files\vaultis"
```

Any of these creates **vaultis (View)** and **vaultis (Edit)** on your Desktop, with
the locked and unlocked icons respectively.

### Or by hand

1. Right-click `vaultis-gui.exe` → **Create shortcut**; put it on the Desktop.
2. For the **Edit** shortcut: Properties → **Target**, append a space and `--write`
   (so it ends `...\vaultis-gui.exe --write`).
3. Properties → **Change Icon…** → Browse to `icons\vaultis-unlocked.ico` (use
   `vaultis-locked.ico` for the read-only one) → OK.
4. Rename the shortcuts to **vaultis (View)** and **vaultis (Edit)**.

> Both shortcuts point at `vaultis-gui.exe` (the GUI build), so neither opens a
> console window. The console `vaultis.exe` is only for the command-line tools.

### Shortcut shows a blank or generic icon

- **Stale icon cache.** Windows caches shortcut icons by the icon's *path*, so
  replacing an `.ico` in place leaves Explorer drawing the old image. The script
  refreshes the cache itself; by hand, run `ie4uinit.exe -show`, or delete and recreate
  the shortcut, or sign out and back in.
- **The icon file moved.** The shortcut stores an absolute path to the `.ico`. If you
  built in the repo and the shortcut points at `packaging\icons\…`, then moved or
  deleted the repo, the icon is simply gone. Re-run the script with `-InstallDir`
  against a stable folder holding both the exe and the icons.
- **A hand-built `.ico`.** See the frame-format note above — an all-PNG `.ico` shows as
  a generic icon at Desktop sizes.
