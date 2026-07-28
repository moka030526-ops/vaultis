# Testing get_vaultis.bat in Windows Sandbox

Runs the real `get_vaultis.bat` in a disposable, genuinely clean Windows Sandbox VM.
Real network calls, the real published release, a real install -- nothing is mocked.

**One sandbox.** Inside it the installer runs twice: once on a machine that has never
seen vaultis, then again over the top of that install -- the path every future update
takes. Both are checked. The whole thing takes about two minutes.

There is no git/rustup/winget matrix any more. `get_vaultis.bat` downloads a prebuilt
release and uses none of those tools, so the old eight combinations exercised one code
path eight times, at 10-20 minutes of compiling each.

A run PASSES only if the sandbox ends up with vaultis actually installed **after each
run**: both binaries under `%LOCALAPPDATA%\Programs\vaultis`, and both desktop
shortcuts -- "vaultis (View)" and "vaultis (Edit)" -- pointing at a `vaultis-gui.exe`
that exists. `get_vaultis.bat`'s exit code is not enough on its own, because it treats a
failure to create the shortcuts as a warning and still exits 0.

Requires Windows 10/11 Pro, Enterprise, or Education (not Home), with virtualization
enabled in firmware.

## One-time setup

1. Enable the feature (needs a restart), as Administrator:
   ```powershell
   Enable-WindowsOptionalFeature -Online -FeatureName "Containers-DisposableClientVM" -All
   ```
2. Generate the sandbox config (run on the host, from this folder):
   ```powershell
   .\make-sandbox-configs.ps1
   ```
   If execution policy blocks it:
   `powershell -ExecutionPolicy Bypass -File .\make-sandbox-configs.ps1`

   This also deletes configs and results left by older versions of this kit, which
   generated one `.wsb` per combination and never cleaned up after itself.

## Running it

Double-click `sandbox-configs\vaultis.wsb`. The sandbox boots, runs `get_vaultis.bat`
twice, and writes its results to the mapped `results\` folder on the host.

No UAC prompt should appear: the install is per-user, under `%LOCALAPPDATA%`, and needs
no administrator rights. The run records whether it was elevated, so that claim has
evidence rather than being assumed.

When it finishes, the window says PASS or FAIL and waits. Minimize it and you are
looking at the sandbox desktop with the two vaultis icons on it -- **double-click one**,
since only launching the app proves the downloaded binary actually runs on a clean
machine, which no amount of file checking can tell you. A SmartScreen warning there is
expected: the release is not code-signed yet.

Close the sandbox window when you are done; everything inside it is discarded, and what
is in `results\` persists.

## Reading results

`results\run.status` is the whole story, and it ends with `RESULT: PASS` or `RESULT:
FAIL` followed by each failure. Watch it live from the host while the sandbox runs:

```powershell
Get-Content results\run.status -Wait
```

| file | what it is |
| --- | --- |
| `run.status` | timestamped steps, **written straight through**, ending in the verdict -- usually enough on its own |
| `get_vaultis-1.log` | **everything `get_vaultis.bat` printed** on the clean install, stdout and stderr -- the file that says *why* a run failed |
| `get_vaultis-2.log` | the same, for the second run over the top of the first |
| `run.log` | the PowerShell transcript of the steps around them |
| `run-desktop.png` | a screenshot of the sandbox desktop, taken just before it was discarded |

Watch `run.status` rather than `run.log` while a run is in flight. The transcript is
buffered, so mid-run it stops dead partway through and looks exactly like a hang;
`run.status` is always current.

## What is actually under test

`get_vaultis.bat` downloads **the latest published GitHub release**, not your working
copy. So:

* Local edits to `get_vaultis.bat` itself **are** picked up -- the kit copies the mapped
  local copy into the sandbox before running it.
* Local edits to anything that ends up *inside the release* -- the binaries,
  `packaging\windows\make-shortcuts.ps1`, the icons -- are **not**. Those come from the
  last release, so cut one (`scripts/release.sh`, which pushes the tag that triggers
  `.github/workflows/release.yml`) before testing them here.
* If there is no published release at all, the run fails at the lookup step.
