@echo off
setlocal EnableExtensions

REM ==========================================================================
REM Self-contained Windows setup script for vaultis: double-click it.
REM
REM Downloads the latest released build and puts two shortcuts on your Desktop.
REM Pass a tag to install that exact release instead -- get_vaultis.bat v0.2.1 --
REM which is how you go back to a known-good version, or install the precise
REM build a bug report is about.
REM It does NOT compile anything: no Git, no Rust, no compiler, no Visual
REM Studio. Building on the machine being set up was tried and abandoned --
REM every Rust toolchain rustup can install unaided fails to LINK on a clean
REM Windows box, and the alternative was a multi-gigabyte Visual Studio
REM download in the middle of a double-click install. The compiling happens in
REM CI now, once, on a runner that already has the toolchain; see
REM .github/workflows/release.yml. To build from source yourself, clone the
REM repository and run scripts\build.bat.
REM
REM This file is both a batch script and a PowerShell script. cmd.exe runs the
REM header below, which launches PowerShell on this same file with
REM -ExecutionPolicy Bypass -- needed because the default Restricted /
REM RemoteSigned policy blocks a downloaded script, and because Explorer will
REM not run a .ps1 on double-click at all. The bypass applies to that one
REM PowerShell process; it does not change the machine's execution policy.
REM
REM The PowerShell source is everything after the ":PSBEGIN:" marker near the
REM bottom. cmd.exe never reads that far -- the header ends at "exit /b" -- so
REM the two languages never collide. Edit the PowerShell part as an ordinary
REM script; just keep the marker line intact.
REM ==========================================================================

REM Prefer the fixed System32 path: PATH is not always intact in a shell spawned
REM by an installer or a locked-down profile. Fall back to a PATH lookup only if
REM that is somehow missing.
set "PSEXE=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PSEXE%" set "PSEXE=powershell.exe"

REM Handing the path over in an environment variable rather than inlining %~f0
REM into the -Command string keeps quoting out of it, so a folder containing an
REM apostrophe or an ampersand cannot break -- or reshape -- the command line.
set "VAULTIS_SELF=%~f0"

REM An optional first argument pins the release to install, e.g.
REM     get_vaultis.bat v0.2.1
REM With no argument the latest release is installed, which is what a
REM double-click does and what almost everyone wants. The pin exists for the two
REM cases where "latest" is the wrong answer: going BACK to a known-good version,
REM and installing the exact build a bug report is about. Handed over in an
REM environment variable for the same reason as the path above -- so quoting in
REM the value cannot reshape the command line. The PowerShell section validates
REM it before it reaches a URL.
set "VAULTIS_TAG=%~1"

REM An optional SECOND argument is the install folder, e.g.
REM     get_vaultis.bat v0.2.1 D:\tools
REM Given it, the script does not prompt -- which is what makes an unattended run able to
REM choose a location rather than silently accepting the default. Same env-var handover, for
REM the same quoting reason.
set "VAULTIS_DIR=%~2"

REM Pin the working directory to this file's folder. Nothing is written here any
REM more, but a predictable working directory keeps relative paths in error
REM messages meaningful, and "Run as administrator" would otherwise start in
REM C:\Windows\System32.
pushd "%~dp0"
if errorlevel 1 (
    echo ERROR: Could not enter "%~dp0".
    set "RC=1"
    goto :done
)

REM The marker is searched for as '#:PS' + 'BEGIN:' so that this line -- which has
REM to mention it -- is not itself a match. Otherwise a file whose marker had been
REM edited away would "find" it here and run the batch header as PowerShell. The
REM match starts at the '#' so the marker line itself parses as a comment.
"%PSEXE%" -NoProfile -ExecutionPolicy Bypass -Command "$src = [IO.File]::ReadAllText($env:VAULTIS_SELF); $i = $src.IndexOf('#:PS' + 'BEGIN:'); if ($i -lt 0) { Write-Error 'This file is damaged: the embedded PowerShell section is missing.'; exit 9 }; & ([scriptblock]::Create($src.Substring($i)))"
set "RC=%ERRORLEVEL%"

popd

:done
if not "%RC%"=="0" (
    echo.
    echo Setup failed with exit code %RC%.
)

REM Pause only when the console will vanish on exit -- i.e. when this was launched
REM by double-clicking in Explorer, where an error would otherwise flash past too
REM fast to read. cmd puts this file's name in cmdcmdline exactly in that case; when
REM run from an existing prompt it does not.
echo %cmdcmdline% | find /i "%~nx0" >nul && pause

exit /b %RC%

#:PSBEGIN:
$ErrorActionPreference = "Stop"

# Invoke-WebRequest redraws a progress bar for every chunk it receives, and on Windows
# PowerShell 5.1 that redraw costs far more than the transfer itself -- turning a short
# download into minutes on a console that looks frozen the whole time. This is the
# fresh-Windows path, exactly where nobody can tell a slow download from a hung script.
$ProgressPreference = "SilentlyContinue"

# ==========================================
# Configuration
# ==========================================

$Repo = "moka030526-ops/vaultis"

# Matched by PATTERN from the releases API rather than guessed as a fixed name: the
# asset carries its version (vaultis-v0.1.0-windows-x86_64.zip), so the convenient
# /releases/latest/download/<fixed-name> URL would be a 404 -- and with
# $ErrorActionPreference = "Stop" that kills this script on exactly the machines it
# exists to set up.
$AssetPattern = '^vaultis-.*-windows-x86_64\.zip$'

# Per-user, so nothing here needs elevation: %LOCALAPPDATA%\Programs is where per-user
# applications belong on Windows. An install under Program Files would need admin
# rights and buy nothing -- one person's vault app is not a machine-wide service.
$DefaultInstallDir = Join-Path $env:LOCALAPPDATA "Programs\vaultis"

# The files this package installs, used both to recognise an existing install and to know
# what an uninstall may remove. Deliberately an explicit list rather than "whatever is in
# the folder": it is what makes it safe to install into, and later clean up, a directory
# that may hold things this script did not put there.
$OwnedNames = @(
    'vaultis.exe', 'vaultis-gui.exe', 'vaultis-locked.ico', 'vaultis-unlocked.ico',
    'make-shortcuts.ps1', 'get_vaultis.bat', 'sample-vault'
)

# The on-disk fingerprints of a vault, taken from the code that writes them:
#   vault.pmv     the encrypted record file      (launch.rs VAULT_FILE)
#   manifest      the encrypted document index   (storage.rs)
#   volume        the encrypted document store   (storage.rs)
#   vaultis.lock  the single-writer lock         (vault.rs LOCK_FILE)
#
# ANY directory containing one of these is somebody's vault, and this installer does not
# delete, overwrite or install into one. This program has no recovery path, so the rule is
# the blunt one -- see a marker, leave the directory alone.
$VaultMarkers = @('vault.pmv', 'manifest', 'volume', 'vaultis.lock')

# The ONE exception, by explicit project decision: the shipped practice vault.
#
# `sample-vault` matches the markers above -- it is a real vault -- but it is *this
# package's* vault, not the user's: it arrives in every release, it is regenerated by every
# install, its two passwords are published (`sample1` / `sample2`), and the Help manual and
# README both say never to put anything real in it. So removing an old installation removes
# it too, rather than leaving a folder behind forever because a throwaway demo is sitting
# in it.
#
# The accepted risk, stated rather than glossed: someone who ignored all of that and kept
# real data in the sample vault loses it when they relocate their install and answer "yes"
# to the removal prompt. That prompt defaults to no and names what it is about to do. Every
# vault that is NOT called sample-vault stays protected by the guard above, under any name.
$SampleVaultName = 'sample-vault'

function Test-LooksLikeVault {
    param([string]$Path)
    if (-not $Path) { return $false }
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) { return $false }
    foreach ($Marker in $VaultMarkers) {
        if (Test-Path -LiteralPath (Join-Path $Path $Marker)) { return $true }
    }
    return $false
}

# A directory "is a vaultis install" iff it holds the executables. Checking the exes rather
# than any owned name means a folder that merely has a copy of get_vaultis.bat sitting in it
# -- your Downloads folder, typically -- is NOT mistaken for an install and overwritten.
function Test-IsVaultisInstall {
    param([string]$Path)
    if (-not $Path) { return $false }
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    return (Test-Path -LiteralPath (Join-Path $Path 'vaultis.exe')) -or
           (Test-Path -LiteralPath (Join-Path $Path 'vaultis-gui.exe'))
}

# ==========================================
# Where to install
# ==========================================

# Precedence: an explicit second argument, else what the prompt is answered with, else the
# per-user default. The prompt is skipped entirely when the argument is given, so an
# unattended run (scripts\windows-setup-tests drives this with empty stdin) can choose a
# location instead of silently taking whatever the default happens to be.
$Chosen = $env:VAULTIS_DIR
if ($Chosen) { $Chosen = $Chosen.Trim().Trim('"') }

if (-not $Chosen) {
    Write-Host ""
    Write-Host "Where should vaultis be installed?"
    Write-Host "  Press Enter for the usual place:  $DefaultInstallDir"
    Write-Host "  Or type a folder (a 'vaultis' subfolder is created inside it)."
    # Read-Host throws at end-of-input rather than returning empty, and this script is
    # deliberately run with `< NUL` by scripts\windows-setup-tests (so the trailing pause
    # cannot hang a sandbox). Treat EOF as "pressed Enter" instead of dying at the prompt.
    $Chosen = ''
    try { $Chosen = Read-Host "Folder" } catch { $Chosen = '' }
    if ($Chosen) { $Chosen = $Chosen.Trim().Trim('"') }
}

if (-not $Chosen) {
    # Enter: the per-user default. %LOCALAPPDATA%\Programs is where per-user applications
    # belong on Windows, it needs no elevation, and it does not sit in a folder people
    # periodically empty -- which a Downloads-relative install would.
    $InstallDir = $DefaultInstallDir
}
else {
    # Resolve relative paths ('.', '..\tools') against the folder this was launched from,
    # which is where the batch header pushd'd to.
    try {
        $Chosen = [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).ProviderPath, $Chosen))
    }
    catch {
        throw "'$Chosen' is not a usable folder path."
    }
    # Refuse to scatter executables inside somebody's vault. This is not about deletion --
    # nothing here would delete it -- but an install directory is a place this script writes
    # files into and later offers to clean up, and a vault is not that.
    if (Test-LooksLikeVault $Chosen) {
        throw ("'$Chosen' looks like a vault (it contains " +
            (($VaultMarkers | Where-Object { Test-Path -LiteralPath (Join-Path $Chosen $_) }) -join ', ') +
            "). Refusing to install into a vault directory -- choose somewhere else.")
    }
    if (Test-IsVaultisInstall $Chosen) {
        # Already an install: upgrade it in place, rather than burying a second copy in a
        # 'vaultis' subfolder of it.
        $InstallDir = $Chosen
        Write-Host "That folder already holds vaultis -- upgrading it in place."
    }
    else {
        # Anything else gets a subfolder, so this never scatters executables through a
        # directory the user keeps other things in, and the uninstall later has an
        # unambiguous folder to remove.
        $InstallDir = Join-Path $Chosen 'vaultis'
    }
}

Write-Host "Installing to: $InstallDir"

# ==========================================
# Find the release to install
# ==========================================

# No argument -> whatever /releases/latest points at (the double-click path).
# An argument -> that exact tag, for rolling BACK to a known-good version or for
# installing the precise build a bug report names.
$Tag = $env:VAULTIS_TAG
if ($Tag) { $Tag = $Tag.Trim() }

if ($Tag) {
    # This value is about to become part of a URL, so it is validated against the
    # shape release.sh actually produces rather than escaped and hoped for. Anything
    # else -- a path traversal, a query string, a second URL -- is refused outright
    # instead of being sent to api.github.com to see what happens.
    if ($Tag -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$') {
        throw ("'$Tag' is not a vaultis release tag. Expected something like " +
            "v0.2.1 (optionally with a -rc1 suffix), or no argument at all to " +
            "install the latest release.")
    }
    # Tags are pushed as v-prefixed; accept "0.2.1" as well and normalise.
    if ($Tag -notmatch '^v') { $Tag = "v$Tag" }

    Write-Host "Looking up vaultis release $Tag..."
    try {
        $Release = Invoke-RestMethod `
            -Uri "https://api.github.com/repos/$Repo/releases/tags/$Tag" `
            -Headers @{ "User-Agent" = "vaultis-setup" }
    } catch {
        throw ("No published release is tagged $Tag. " +
            "See https://github.com/$Repo/releases for the list.")
    }
} else {
    Write-Host "Looking up the latest vaultis release..."

    $Release = Invoke-RestMethod `
        -Uri "https://api.github.com/repos/$Repo/releases/latest" `
        -Headers @{ "User-Agent" = "vaultis-setup" }
}

$Asset = $Release.assets |
    Where-Object { $_.name -match $AssetPattern } |
    Select-Object -First 1

if (-not $Asset) {
    throw ("Release '$($Release.tag_name)' has no Windows package. " +
        "Download it yourself: $($Release.html_url)")
}

# Refuse to install something whose integrity cannot be established at all. Failing
# closed here costs one release-publishing mistake; installing an unverifiable binary
# costs whatever that binary turns out to be.
$Checksum = $Release.assets |
    Where-Object { $_.name -eq ($Asset.name + ".sha256") } |
    Select-Object -First 1

if (-not $Checksum) {
    throw ("Release '$($Release.tag_name)' ships $($Asset.name) with no .sha256 " +
        "beside it. Refusing to install a download that cannot be verified.")
}

Write-Host "Found $($Release.tag_name): $($Asset.name)"

# ==========================================
# Download and verify
# ==========================================

# A FRESH randomly-named directory rather than a fixed %TEMP%\vaultis.zip: a predictable
# path in a world-writable-ish location is something another process on this machine can
# pre-create, and verify-then-extract is only meaningful if nothing can swap the file
# underneath it in between.
$DownloadDir = Join-Path $env:TEMP ("vaultis-dl-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $DownloadDir -Force | Out-Null

try {
    $Zip = Join-Path $DownloadDir $Asset.name
    $ShaFile = Join-Path $DownloadDir $Checksum.name

    Write-Host "Downloading $($Asset.name) (~$([int]($Asset.size / 1MB)) MB)..."
    Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $Zip
    Invoke-WebRequest -Uri $Checksum.browser_download_url -OutFile $ShaFile

    Write-Host "Verifying the download..."

    # The published file is "<hex>  <filename>", the sha256sum format the release job
    # writes; take the first whitespace-delimited field.
    $Expected = ((Get-Content -LiteralPath $ShaFile -Raw) -split '\s+')[0].Trim().ToLowerInvariant()
    $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Zip).Hash.ToLowerInvariant()

    if ($Expected -ne $Actual) {
        throw ("The download does not match its published checksum. Refusing to " +
            "install it.`n  expected: $Expected`n  actual:   $Actual")
    }

    # Worth being straight about what that check is and is not. The .sha256 comes from
    # the same release, over the same TLS connection, from the same host as the zip --
    # so it proves the bytes arrived intact, catching a truncated or corrupted download
    # or a broken proxy. It is NOT proof of authorship: anyone able to publish a release
    # here could publish a matching checksum with it. Authenticode signing is what would
    # bind these bytes to a publisher, and until the release is signed this line is the
    # honest description of the guarantee.
    Write-Host "Checksum OK."

    # ==========================================
    # Install
    # ==========================================

    # Expand into a staging folder first, then place the files. The previous version
    # deleted $InstallDir outright and expanded into it, which is only safe while the
    # install directory is one this script created and owns exclusively. Now that the
    # directory can be chosen -- and can be a folder the user already keeps things in --
    # "delete the destination, then extract" would erase whatever else lives there. So the
    # destructive step is confined to the staging folder, and only the files this package
    # actually contains are written at the destination.
    $StageDir = Join-Path $DownloadDir "stage"
    New-Item -ItemType Directory -Path $StageDir -Force | Out-Null
    Expand-Archive -LiteralPath $Zip -DestinationPath $StageDir -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Write-Host "Installing into $InstallDir..."

    # The file currently being executed. cmd.exe holds this open for the rest of the
    # batch header, and Windows generally refuses to overwrite it -- which, now that
    # get_vaultis.bat ships inside the package, would fail an in-place install partway
    # through. Skip exactly that one file: the copy being skipped is the same version
    # being installed, so nothing is lost. Every other case (any destination other than
    # the folder this was launched from) writes it normally.
    $SelfPath = $null
    if ($env:VAULTIS_SELF -and (Test-Path -LiteralPath $env:VAULTIS_SELF)) {
        $SelfPath = (Get-Item -LiteralPath $env:VAULTIS_SELF).FullName
    }

    foreach ($Item in Get-ChildItem -LiteralPath $StageDir -Force) {
        $Target = Join-Path $InstallDir $Item.Name
        if ($SelfPath -and -not $Item.PSIsContainer -and (Test-Path -LiteralPath $Target)) {
            $Existing = Get-Item -LiteralPath $Target -ErrorAction SilentlyContinue
            if ($Existing -and $Existing.FullName -eq $SelfPath) {
                Write-Host "  keeping the running $($Item.Name) (same version as the package)"
                continue
            }
        }
        if ($Item.PSIsContainer -and (Test-Path -LiteralPath $Target)) {
            if ($Item.Name -eq $SampleVaultName) {
                # Refresh the shipped practice vault: DELETE then copy, never merge.
                #
                # `Copy-Item -Recurse -Force` onto an existing directory merges into it --
                # same-named files are overwritten, but files present only at the
                # destination survive. For a vault that is the worst of both: the package's
                # sample-vault carries vault.pmv + manifest/manifest.0 + volume/vol.0 all
                # keyed to a freshly generated salt, so any extra partitions an older
                # sample had grown (vol.1, manifest.1, from documents added while
                # practising) would be left behind encrypted under the PREVIOUS key --
                # undecryptable files in a directory the storage engine scans. Removing the
                # old directory first is what makes "the new sample vault" actually mean
                # that.
                Remove-Item -LiteralPath $Target -Recurse -Force -ErrorAction SilentlyContinue
                if (Test-Path -LiteralPath $Target) {
                    throw ("Could not replace the old $($Item.Name) in $InstallDir. If " +
                        "vaultis has that sample vault open, close it and run this again " +
                        "-- continuing would merge the new practice vault into the old " +
                        "one and leave it unopenable.")
                }
                Write-Host "  replacing $($Item.Name) with the one from this release"
            }
            elseif (Test-LooksLikeVault $Target) {
                # Any OTHER vault in the install folder is left strictly alone -- not
                # merged into, not deleted. It is not ours.
                Write-Host "  keeping the existing $($Item.Name) (it is a vault -- not touching it)"
                continue
            }
        }
        # -Force overwrites files. Directories merge, which is what an upgrade over an
        # existing install wants for anything that is not a vault.
        Copy-Item -LiteralPath $Item.FullName -Destination $InstallDir -Recurse -Force
    }
}
finally {
    # `finally` so a rejected or half-finished download is never left behind in %TEMP%
    # for something else to find. Deliberately NOT subject to Test-LooksLikeVault, and the
    # reason matters: the staging folder holds the package's own sample-vault, so the guard
    # would match and leave the whole download sitting in %TEMP% forever. This directory was
    # created by this run, under a fresh GUID name, and contains nothing but what was just
    # unpacked from the zip -- no vault of the user's can be in here.
    Remove-Item -LiteralPath $DownloadDir -Recurse -Force -ErrorAction SilentlyContinue
}

# ==========================================
# Desktop shortcuts
# ==========================================

# The same script the from-source build uses, shipped inside the zip. -InstallDir makes
# it take both the exe and the icons from that one folder, which is exactly the flat
# layout the release job packages.
$ShortcutScript = Join-Path $InstallDir "make-shortcuts.ps1"

if (Test-Path -LiteralPath $ShortcutScript) {
    & $ShortcutScript -InstallDir $InstallDir
}
else {
    Write-Warning ("The package has no make-shortcuts.ps1, so no Desktop shortcuts " +
        "were created. vaultis is installed: $InstallDir\vaultis-gui.exe")
}

# ==========================================
# Offer to remove an install left behind at the default location
# ==========================================

# Only the default location can be checked -- there is no register of past installs, so a
# previous custom location is unknowable. Defaults to NO, like every other destructive step
# in this project: an accidental Enter must not delete anything.
#
# What it removes is the old install FOLDER and the two Desktop shortcuts, which now point
# at a folder that is about to stop existing. It does not touch vaults: those live wherever
# you chose, never under the install directory, and nothing here goes looking for them.
if ($InstallDir -ne $DefaultInstallDir -and (Test-IsVaultisInstall $DefaultInstallDir)) {
    Write-Host ""
    Write-Host "There is still an older vaultis installed at:"
    Write-Host "  $DefaultInstallDir"
    Write-Host "Its Desktop shortcuts point there, not at the copy just installed."
    Write-Host "Your vaults are stored elsewhere and are NOT affected either way."
    # Same EOF handling as the folder prompt, and note what it means here: an unattended
    # run declines. A destructive step must never happen because nobody was there to say no.
    $Answer = ''
    try { $Answer = Read-Host "Delete that older installation? [y/N]" } catch { $Answer = '' }

    if ($Answer -match '^[Yy]') {
        # Remove only the files this package installs, then the folder itself if that
        # leaves it empty -- NOT a recursive delete of the whole directory. Someone may
        # have put a vault inside the install folder, and in a program with no recovery
        # path a stray `Remove-Item -Recurse` is how you destroy an estate. Anything not
        # on the owned list is left exactly where it is, and the folder stays with it.
        $Failed = @()
        $KeptVaults = @()
        foreach ($Name in $OwnedNames) {
            $Victim = Join-Path $DefaultInstallDir $Name
            if (-not (Test-Path -LiteralPath $Victim)) { continue }
            # The guard, applied to every directory before it is deleted. The shipped
            # sample vault is the one deliberate exception (see $SampleVaultName); any
            # OTHER vault found in an install folder -- under any name -- survives.
            if ($Name -ne $SampleVaultName -and (Test-LooksLikeVault $Victim)) {
                $KeptVaults += $Name
                continue
            }
            Remove-Item -LiteralPath $Victim -Recurse -Force -ErrorAction SilentlyContinue
            if (Test-Path -LiteralPath $Victim) { $Failed += $Name }
        }
        if ($KeptVaults.Count -gt 0) {
            Write-Host ("Kept (these are vaults, so they were not touched): " +
                ($KeptVaults -join ', '))
        }
        if ($Failed.Count -gt 0) {
            Write-Warning ("Could not remove: " + ($Failed -join ', ') +
                " -- if vaultis is still open from there, close it and delete them by hand.")
        }
        $Leftover = @(Get-ChildItem -LiteralPath $DefaultInstallDir -Force -ErrorAction SilentlyContinue)
        if ($Leftover.Count -eq 0) {
            Remove-Item -LiteralPath $DefaultInstallDir -Force -ErrorAction SilentlyContinue
            Write-Host "Removed $DefaultInstallDir"
        }
        else {
            Write-Host ("Removed the vaultis files from $DefaultInstallDir, but kept the " +
                "folder: it still contains " + $Leftover.Count + " item(s) this installer " +
                "did not put there.")
        }
        # The shortcuts are re-created below/above by make-shortcuts.ps1 for the NEW
        # location, so removing the stale ones is only needed if they were named
        # differently. Remove by the names this project creates, and only those.
        foreach ($Link in 'vaultis (View).lnk', 'vaultis (Edit).lnk') {
            $Lnk = Join-Path ([Environment]::GetFolderPath('Desktop')) $Link
            if (Test-Path -LiteralPath $Lnk) {
                Remove-Item -LiteralPath $Lnk -Force -ErrorAction SilentlyContinue
            }
        }
        Write-Host "Re-creating shortcuts for the new location..."
        if (Test-Path -LiteralPath $ShortcutScript) {
            & $ShortcutScript -InstallDir $InstallDir
        }
    }
    else {
        Write-Host "Left it alone. Both installations are now present."
    }
}

Write-Host ""
Write-Host "------------------------------------------------------------------------"
Write-Host " vaultis $($Release.tag_name) is installed"
Write-Host "------------------------------------------------------------------------"
Write-Host "  Location: $InstallDir"
Write-Host ""
Write-Host "  Two shortcuts are on your Desktop:"
Write-Host "    vaultis (View)  - read-only"
Write-Host "    vaultis (Edit)  - edit mode"
Write-Host ""
Write-Host "  Re-run this file any time to update to the latest release,"
Write-Host "  or pass a tag to install a specific one:  get_vaultis.bat v0.2.1"
Write-Host "------------------------------------------------------------------------"
