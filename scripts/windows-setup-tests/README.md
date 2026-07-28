# Testing get_vaultis.bat in Windows Sandbox

Exercises all 8 combinations of git / rustup / winget present-or-absent against the
real `get_vaultis.bat`, each in a disposable, genuinely clean Windows Sandbox VM. Real
network calls, real installers, real `cargo build` -- nothing here is mocked.

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
   This writes 8 `.wsb` files into `sandbox-configs\`.

## Running a combo

Double-click a `.wsb` file, e.g. `sandbox-configs\nogit-norustup-nowinget.wsb`. The
sandbox boots, installs whatever the combo calls for (skip this for "absent" --
sandboxes start with none of these tools), then runs the real `get_vaultis.bat` and
writes a full transcript to `results\<combo-name>.log` on the host.

Run combos one at a time. Watch the sandbox window: a real installer (Git's, or
rustup-init) can raise a UAC consent prompt since the sandbox account is an
administrator but not pre-elevated -- click "Yes" if you see one. Close the sandbox
window when it's done (or once you've seen enough); everything inside it is discarded,
the log in `results\` is what persists.

## Reading results

`results\<combo>.log` is a full PowerShell transcript: which of git/rustup/winget the
sandbox actually had going in, then get_vaultis.bat's own output, ending with its exit
code. Exit code 0 means the whole flow -- detect, install if needed, clone/pull, run
scripts\build.bat, build, seed the sample vault -- succeeded.

## Note on what's actually under test

get_vaultis.bat clones the real `vaultis` repo from GitHub and builds *that* clone --
not your local working copy. If you're testing local edits to `get_vaultis.bat`
itself, this kit picks those up (it copies the mapped, local copy into the sandbox
before running it). If you're testing local edits to `scripts\build.bat` or anything
else in the repo, push first -- get_vaultis.bat has no way to build from anything but
the GitHub clone.
