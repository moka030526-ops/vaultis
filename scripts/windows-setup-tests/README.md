# Testing get_vaultis.bat in Windows Sandbox

Exercises all 8 combinations of git / rustup / winget present-or-absent against the
real `get_vaultis.bat`, each in a disposable, genuinely clean Windows Sandbox VM. Real
network calls, real installers, real `cargo build` -- nothing here is mocked.

A combo PASSES only if that sandbox ends up with vaultis actually installed: both
desktop shortcuts -- "vaultis (View)" and "vaultis (Edit)" -- present, and each
pointing at a `vaultis-gui.exe` that exists. `get_vaultis.bat`'s own exit code is not
enough on its own, because it treats a failure to install the shortcuts as a warning
and still exits 0.

Requires Windows 10/11 Pro, Enterprise, or Education (not Home), with virtualization
enabled in firmware.

## One-time setup

1. Enable the feature (needs a restart), as Administrator:
   ```powershell
   Enable-WindowsOptionalFeature -Online -FeatureName "Containers-DisposableClientVM" -All
   ```
2. Generate the sandbox configs (run on the host, from this folder):
   ```powershell
   .\make-sandbox-configs.ps1
   ```
   This writes 8 `.wsb` files into `sandbox-configs\`, sized to give each sandbox half
   the host's RAM (4-8 GB) -- a release build of the workspace needs it.

## Running a combo

Double-click a `.wsb` file, e.g. `sandbox-configs\nogit-norustup-nowinget.wsb`. The
sandbox boots, installs whatever the combo calls for (skip this for "absent" --
sandboxes start with none of these tools), then runs the real `get_vaultis.bat`.

Budget **10-20 minutes per combo**: it clones the repo, installs a Rust toolchain, and
does a full release build of the workspace, from scratch, every time.

Watch the sandbox window at the very start. The script takes an elevated token up
front -- Git's installer is `requireAdministrator`, so `/VERYSILENT` still stalls on a
UAC consent dialog without one -- so click "Yes" if a prompt appears in the first few
seconds. After that the run is unattended.

Run combos one at a time. When a combo finishes, the window says PASS or FAIL and
waits: minimize it and you are looking at the sandbox desktop with the two vaultis
icons on it. Close the sandbox window when you're done; everything inside it is
discarded, and what's in `results\` is what persists.

## Reading results

```powershell
.\show-results.ps1
```

One line per combo -- PASS, FAIL, RUNNING, or not run -- and the log paths for any
failures. Safe to run while a sandbox is still going.

Per combo, `results\` holds:

| file | what it is |
| --- | --- |
| `<combo>.result` | the one-line verdict `show-results.ps1` reads |
| `<combo>.status` | timestamped steps, **written straight through** -- this is the one to watch mid-run |
| `<combo>.log` | the full PowerShell transcript: prep, then all of `get_vaultis.bat`'s output |
| `<combo>-desktop.png` | a screenshot of that sandbox's desktop, taken just before it was discarded |

Watch `.status` rather than `.log` while a run is in flight. The transcript is
buffered, so mid-run it stops dead partway through and looks exactly like a hang; the
`.status` file is always current, and its last line tells you whether the sandbox is
downloading, building, or genuinely stuck.

## Note on what's actually under test

get_vaultis.bat clones the real `vaultis` repo from GitHub and builds *that* clone --
not your local working copy. If you're testing local edits to `get_vaultis.bat`
itself, this kit picks those up (it copies the mapped, local copy into the sandbox
before running it). If you're testing local edits to `scripts\build.bat`,
`packaging\windows\make-shortcuts.ps1`, or anything else in the repo, push first --
get_vaultis.bat has no way to build from anything but the GitHub clone. A shortcut fix
that only exists locally will show up here as a FAIL.
