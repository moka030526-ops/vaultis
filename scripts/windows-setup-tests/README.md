# Testing get_vaultis.bat in Windows Sandbox

Runs the real `get_vaultis.bat` in a disposable, genuinely clean Windows Sandbox VM.
Real network calls, the real published release, a real install -- nothing is mocked.

A run PASSES only if the sandbox ends up with vaultis actually installed: both
binaries present under `%LOCALAPPDATA%\Programs\vaultis`, and both desktop shortcuts --
"vaultis (View)" and "vaultis (Edit)" -- pointing at a `vaultis-gui.exe` that exists.
`get_vaultis.bat`'s exit code is not enough on its own, because it treats a failure to
create the shortcuts as a warning and still exits 0.

Requires Windows 10/11 Pro, Enterprise, or Education (not Home), with virtualization
enabled in firmware.

## Scenarios

| scenario | what it covers |
| --- | --- |
| `fresh` | a clean machine that has never seen vaultis |
| `reinstall` | runs it twice, so the second run replaces the first install -- the same path every future update takes |

There is no git/rustup/winget matrix. `get_vaultis.bat` downloads a prebuilt release
and uses none of those tools, so the old eight combinations exercised one code path
eight times, at 10-20 minutes of compiling each. A run now takes about a minute.

## One-time setup

1. Enable the feature (needs a restart), as Administrator:
   ```powershell
   Enable-WindowsOptionalFeature -Online -FeatureName "Containers-DisposableClientVM" -All
   ```
2. Generate the sandbox configs (run on the host, from this folder):
   ```powershell
   .\make-sandbox-configs.ps1
   ```

## Running one

Double-click a `.wsb` file, e.g. `sandbox-configs\fresh.wsb`. The sandbox boots, runs
`get_vaultis.bat`, and writes its results to the mapped `results\` folder on the host.

No UAC prompt should appear: the install is per-user, under `%LOCALAPPDATA%`, and needs
no administrator rights. The run records whether it was elevated, so that claim has
evidence rather than being assumed.

When it finishes, the window says PASS or FAIL and waits. Minimize it and you are
looking at the sandbox desktop with the two vaultis icons on it -- **double-click one**,
since only launching the app proves the downloaded binary actually runs on a clean
machine, which no amount of file checking can tell you. Close the sandbox window when
you are done; everything inside it is discarded, and what is in `results\` persists.

## Reading results

```powershell
.\show-results.ps1
```

One line per scenario -- PASS, FAIL, RUNNING, or not run -- and the log paths for any
failures. Safe to run while a sandbox is still going.

Per scenario, `results\` holds:

| file | what it is |
| --- | --- |
| `<scenario>-get_vaultis-N.log` | **everything `get_vaultis.bat` printed** on run N, stdout and stderr -- the file that says *why* a run failed |
| `<scenario>.status` | timestamped steps, **written straight through**, plus the tail of any failure -- usually enough on its own |
| `<scenario>.log` | the PowerShell transcript of the steps around it |
| `<scenario>-desktop.png` | a screenshot of that sandbox's desktop, taken just before it was discarded |

Watch `.status` rather than `.log` while a run is in flight. The transcript is buffered,
so mid-run it stops dead partway through and looks exactly like a hang; `.status` is
always current.

## What is actually under test

`get_vaultis.bat` downloads **the latest published GitHub release**, not your working
copy. So:

* Local edits to `get_vaultis.bat` itself **are** picked up -- the kit copies the mapped
  local copy into the sandbox before running it.
* Local edits to anything that ends up *inside the release* -- the binaries,
  `packaging\windows\make-shortcuts.ps1`, the icons -- are **not**. Those come from the
  last release, so cut one (`git tag vX.Y.Z && git push origin vX.Y.Z`, which triggers
  `.github/workflows/release.yml`) before testing them here.
* If there is no published release at all, every scenario fails at the lookup step.
