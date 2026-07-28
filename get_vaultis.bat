@echo off
setlocal EnableExtensions

REM ==========================================================================
REM Self-contained Windows setup script for vaultis: double-click it.
REM
REM Downloads the latest released build and puts two shortcuts on your Desktop.
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
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\vaultis"

# ==========================================
# Find the latest release
# ==========================================

Write-Host "Looking up the latest vaultis release..."

$Release = Invoke-RestMethod `
    -Uri "https://api.github.com/repos/$Repo/releases/latest" `
    -Headers @{ "User-Agent" = "vaultis-setup" }

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

    if (Test-Path -LiteralPath $InstallDir) {
        Write-Host "Replacing the existing install in $InstallDir..."
        try {
            Remove-Item -LiteralPath $InstallDir -Recurse -Force
        }
        catch {
            throw ("Could not replace '$InstallDir'. If vaultis is open, close it and " +
                "run this again. ($($_.Exception.Message))")
        }
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Write-Host "Installing into $InstallDir..."
    Expand-Archive -LiteralPath $Zip -DestinationPath $InstallDir -Force
}
finally {
    # `finally` so a rejected or half-finished download is never left behind in %TEMP%
    # for something else to find.
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
Write-Host "  Re-run this file any time to update to the latest release."
Write-Host "------------------------------------------------------------------------"
