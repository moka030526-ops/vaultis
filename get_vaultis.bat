:; exec /bin/bash -c "$(sed -n '/^#:SHBEGIN:/,/^#:PS[B]EGIN:/p' "$0" | tr -d '\r')" "$0" "$@" #
@echo off
setlocal EnableExtensions

REM ==========================================================================
REM Self-contained setup script for vaultis, for Windows AND macOS.
REM   Windows: double-click it.
REM   macOS:   in Terminal, run   bash get_vaultis.bat
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
REM
REM It is a bash script too, for the Mac. The FIRST LINE is the whole trick: cmd.exe
REM reads a line starting with ':' as a label and skips it, while bash reads ':' as
REM a no-op and runs the rest -- which cuts the section from the ":SHBEGIN:" marker
REM up to the PowerShell marker out of this file, strips the CRLF line endings this
REM file must have for cmd (.gitattributes), and execs bash on it. So bash never
REM reads past line 1 either. Rules for keeping all three working:
REM   * line 1 must stay line 1, and must end in '#' so its CR is inside a comment;
REM   * it must not contain the PowerShell marker literally (hence the [B]): the
REM     PowerShell launcher below finds its section with an unanchored IndexOf,
REM     and would start from line 1;
REM   * the bash section sits after the batch header's final exit /b and before
REM     the PowerShell marker, so neither of the other two ever reads it.
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

REM A failed install leaves this file as it was: a half-finished run should not swap
REM in an installer for a version that never got installed.
if not "%RC%"=="0" if exist "%~f0.new" del /f /q "%~f0.new" >nul 2>&1

REM Pause only when the console will vanish on exit -- i.e. when this was launched
REM by double-clicking in Explorer, where an error would otherwise flash past too
REM fast to read. cmd puts this file's name in cmdcmdline exactly in that case; when
REM run from an existing prompt it does not. Decided here, but the pause itself is
REM in the block below, so a warning from the swap is still on screen for it.
set "PAUSE_AT_END="
echo %cmdcmdline% | find /i "%~nx0" >nul && set "PAUSE_AT_END=1"

REM When this ran from inside the install folder, the PowerShell section left the
REM package's copy of this script beside it as "<name>.new" rather than overwrite a
REM file cmd is still executing. Swap it in now. This must stay ONE parenthesized
REM block ending in exit /b: cmd parses a whole block before running any of it, so
REM once the move replaces this file nothing more is read from it -- a further line
REM after the block would be read from the NEW file at the OLD offset. Keep any
REM parentheses out of the echo text: they would end the block early.
(
    if exist "%~f0.new" (
        move /y "%~f0.new" "%~f0" >nul || echo WARNING: Could not update "%~f0" - the new copy is "%~f0.new".
    )
    if defined PAUSE_AT_END pause
    exit /b %RC%
)

#:SHBEGIN:
# ==========================================================================
# The macOS installer (bash). Reached only through line 1 of this file; see the
# header. Same job as the PowerShell section below, the Mac way: download the
# release's vaultis.app, verify it, and put it in Applications.
#
#   bash get_vaultis.bat                     latest release, asks where
#   bash get_vaultis.bat v0.4.0              that exact release
#   bash get_vaultis.bat v0.4.0 ~/Apps       that release into ~/Apps, no prompt
#   bash get_vaultis.bat "" ~/Apps           latest release into ~/Apps
# ==========================================================================
set -euo pipefail

REPO="moka030526-ops/vaultis"
# Matched by pattern, not guessed: the asset carries its version. See the Windows side.
ASSET_RE='^vaultis-.*-macos-universal\.zip$'
# A vault's on-disk fingerprints -- the same list, and the same rule, as the Windows
# side: never install into, replace or delete anything that holds one.
VAULT_MARKERS=(vault.pmv manifest volume vaultis.lock)
BUNDLE_ID="dev.vaultis.vaultis"

die() { printf '\nERROR: %s\n' "$*" >&2; exit 1; }

if [ "$(uname -s)" != "Darwin" ]; then
    die "this installer is for Windows and macOS. On Linux, build from source: scripts/build.sh"
fi

looks_like_vault() {
    local m
    for m in "${VAULT_MARKERS[@]}"; do
        if [ -e "$1/$m" ]; then return 0; fi
    done
    return 1
}

is_vaultis_app() {
    [ -f "$1/Contents/Info.plist" ] &&
        [ "$(plutil -extract CFBundleIdentifier raw -o - "$1/Contents/Info.plist" 2>/dev/null)" = "$BUNDLE_ID" ]
}

TAG=${1:-}
CHOSEN=${2:-}

# ------------------------------------------------------------------ where to install

# /Applications is where Mac apps go and is writable by any admin account; a standard
# account falls back to ~/Applications, which macOS also lists as an Applications folder.
if [ -w /Applications ]; then DEFAULT_DIR=/Applications; else DEFAULT_DIR="$HOME/Applications"; fi

if [ -z "$CHOSEN" ] && [ -t 0 ]; then
    echo
    echo "Where should vaultis be installed?"
    echo "  Press Enter for the usual place:  $DEFAULT_DIR"
    echo "  Or type a folder (vaultis.app is put inside it)."
    read -r -p "Folder: " CHOSEN || CHOSEN=""
fi
CHOSEN=${CHOSEN#\"}; CHOSEN=${CHOSEN%\"}
CHOSEN=${CHOSEN/#\~/$HOME}
if [ -z "$CHOSEN" ]; then CHOSEN=$DEFAULT_DIR; fi

CHOSEN_TRIMMED=${CHOSEN%/}
if [[ "$CHOSEN_TRIMMED" == *.app ]] || is_vaultis_app "$CHOSEN"; then
    # An .app path was given: only an existing vaultis.app may be named directly, and it
    # is upgraded in place.
    is_vaultis_app "$CHOSEN" || die "'$CHOSEN' is not an installed vaultis.app."
    APP="$(cd "$CHOSEN" && pwd)"
    PARENT=$(dirname "$APP")
else
    if [ -e "$CHOSEN" ] && looks_like_vault "$CHOSEN"; then
        die "'$CHOSEN' looks like a vault. Refusing to install into a vault folder -- choose somewhere else."
    fi
    mkdir -p "$CHOSEN" || die "could not create '$CHOSEN'."
    PARENT="$(cd "$CHOSEN" && pwd)"
    APP="$PARENT/vaultis.app"
fi
[ -w "$PARENT" ] || die "'$PARENT' is not writable by you. Choose a folder you own, such as ~/Applications."

if [ -e "$APP" ]; then
    is_vaultis_app "$APP" || die "'$APP' exists and is not vaultis. Refusing to replace it."
    # Nothing of the user's belongs inside the bundle -- the app keeps the practice vault
    # and its own settings in ~/Library/Application Support -- but replacing a bundle
    # deletes everything in it, so a vault somebody put there anyway stops the install.
    STRAY=$(find "$APP" \( -name vault.pmv -o -name vaultis.lock \) \
        -not -path "$APP/Contents/Resources/sample-vault/*" -print -quit 2>/dev/null || true)
    if [ -n "$STRAY" ]; then
        die "'$APP' contains a vault ($STRAY). Move it out of the app before updating."
    fi
fi
echo "Installing to: $APP"

# ------------------------------------------------------------------ find the release

if [ -n "$TAG" ]; then
    # Validated against the shape release tags have before it goes into a URL.
    if ! [[ "$TAG" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
        die "'$TAG' is not a vaultis release tag. Expected something like v0.4.0, or no argument for the latest."
    fi
    [[ "$TAG" == v* ]] || TAG="v$TAG"
    echo "Looking up vaultis release $TAG..."
    API="https://api.github.com/repos/$REPO/releases/tags/$TAG"
    JSON=$(curl -fsSL -H "User-Agent: vaultis-setup" "$API") ||
        die "no published release is tagged $TAG. See https://github.com/$REPO/releases"
else
    echo "Looking up the latest vaultis release..."
    JSON=$(curl -fsSL -H "User-Agent: vaultis-setup" "https://api.github.com/repos/$REPO/releases/latest") ||
        die "could not reach GitHub to find the latest release."
fi

# No jq on a stock Mac. grep -o pulls out every "browser_download_url" wherever it sits:
# the API may return its JSON pretty-printed or all on one line (it now does the latter,
# and a per-line sed then saw only the last asset). An asset's name is the last segment
# of its URL, so that is all this needs.
RELEASE_TAG=$(printf '%s\n' "$JSON" | grep -oE '"tag_name": *"[^"]*"' | head -n 1 | sed 's/.*"\([^"]*\)"$/\1/')
URLS=$(printf '%s\n' "$JSON" | grep -oE '"browser_download_url": *"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/')
ZIP_URL=""
while IFS= read -r u; do
    if [[ "${u##*/}" =~ $ASSET_RE ]]; then ZIP_URL=$u; break; fi
done <<< "$URLS"
[ -n "$ZIP_URL" ] || die "release $RELEASE_TAG has no macOS package. See https://github.com/$REPO/releases"
grep -qxF "$ZIP_URL.sha256" <<< "$URLS" ||
    die "release $RELEASE_TAG ships ${ZIP_URL##*/} with no .sha256 beside it. Refusing to install a download that cannot be verified."
echo "Found $RELEASE_TAG: ${ZIP_URL##*/}"

# ------------------------------------------------------------------ download and verify

WORK=$(mktemp -d "${TMPDIR:-/tmp}/vaultis-dl.XXXXXXXX")
trap 'rm -rf "$WORK"' EXIT
ZIP="$WORK/${ZIP_URL##*/}"

echo "Downloading ${ZIP_URL##*/}..."
curl -fL --progress-bar -o "$ZIP" "$ZIP_URL"
curl -fsSL -o "$ZIP.sha256" "$ZIP_URL.sha256"

echo "Verifying the download..."
EXPECTED=$(awk '{ print tolower($1); exit }' "$ZIP.sha256")
ACTUAL=$(shasum -a 256 "$ZIP" | awk '{ print $1 }')
if [ "$EXPECTED" != "$ACTUAL" ]; then
    die "the download does not match its published checksum. Refusing to install it.
  expected: $EXPECTED
  actual:   $ACTUAL"
fi
# The same honest limit as on Windows: this proves the bytes arrived intact, not who
# made them. On a Mac the code signature below is what can say that -- when the
# release is signed with a Developer ID rather than ad hoc.
echo "Checksum OK."

ditto -x -k "$ZIP" "$WORK/stage"
NEW="$WORK/stage/vaultis.app"
is_vaultis_app "$NEW" || die "the package does not contain vaultis.app."
codesign --verify --strict "$NEW" 2>/dev/null ||
    die "the app's code signature does not verify. Refusing to install it."

# ------------------------------------------------------------------ install

if pgrep -qf "$APP/Contents/MacOS/vaultis-gui"; then
    echo "  (vaultis is open -- it keeps running the old version until you quit it)"
fi

# Copied in under a temporary name first, then swapped, so an interrupted install never
# leaves a half-copied app under the real name.
INCOMING="$PARENT/.vaultis.app.incoming-$$"
OUTGOING="$PARENT/.vaultis.app.outgoing-$$"
rm -rf "$INCOMING" "$OUTGOING"
ditto "$NEW" "$INCOMING"
if [ -e "$APP" ]; then mv "$APP" "$OUTGOING"; fi
if ! mv "$INCOMING" "$APP"; then
    [ -e "$OUTGOING" ] && mv "$OUTGOING" "$APP"
    die "could not put the new vaultis.app in place; the previous one was restored."
fi
rm -rf "$OUTGOING"

echo
echo "------------------------------------------------------------------------"
echo " vaultis $RELEASE_TAG is installed"
echo "------------------------------------------------------------------------"
echo "  Location: $APP"
echo
echo "  Open it from Launchpad, Spotlight, or the Applications folder."
echo "  It opens read-only; tick \"Open for editing\" on the lock screen to"
echo "  make changes."
echo
echo "  The terminal version (vaultis --tui, and the command line):"
echo "    \"$APP/Contents/MacOS/vaultis\""
echo
echo "  Run this file again any time to update, or pass a tag to install a"
echo "  specific release:  bash ${0##*/} v0.4.0"
echo "------------------------------------------------------------------------"
exit 0

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

# The ONE exception to the guard above, by explicit project decision: the shipped practice
# vault. `sample-vault` matches the markers -- it is a real vault -- but it is *this
# package's* vault rather than the user's. It arrives in every release, its two passwords
# are published (`sample1` / `sample2`), and both the README and the Help manual say never
# to put anything real in it, so an install replaces it with the current one instead of
# preserving whatever was there. Stated plainly because it is a real trade-off: anything you
# did in the sample vault is discarded when you update. Every vault that is NOT called
# sample-vault is left strictly alone, under any name.
$SampleVaultName = 'sample-vault'

# The user's own data that can sit directly in the install folder when the vault root is
# the install folder itself: the vault files, plus prefs.json (theme, ui scale, font --
# written to <vault_root>/prefs.json, see launch.rs). None of these is in the package
# today, but an upgrade must never overwrite them even if a future package ships a file of
# the same name -- if one exists at the destination, it is kept and the package's copy is
# skipped.
$UserDataNames = $VaultMarkers + @('prefs.json')

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
    if (Test-IsVaultisInstall $Chosen) {
        # Already an install: upgrade it in place, rather than burying a second copy in a
        # 'vaultis' subfolder of it. Checked BEFORE the vault guard below, because some
        # people keep their vault (and its prefs.json) right beside the executables -- that
        # layout is theirs to choose, and refusing to upgrade it would strand them on an old
        # version. The copy loop below never overwrites $UserDataNames, so the vault and the
        # preferences survive the upgrade untouched.
        $InstallDir = $Chosen
        Write-Host "That folder already holds vaultis -- upgrading it in place."
    }
    elseif (Test-LooksLikeVault $Chosen) {
        # Refuse to scatter executables inside somebody's vault that is NOT already an
        # install. This is not about deletion -- nothing here would delete it -- but a vault
        # directory is not a place to start a new install.
        throw ("'$Chosen' looks like a vault (it contains " +
            (($VaultMarkers | Where-Object { Test-Path -LiteralPath (Join-Path $Chosen $_) }) -join ', ') +
            "). Refusing to install into a vault directory -- choose somewhere else.")
    }
    else {
        # Anything else gets a subfolder, so this never scatters executables through a
        # directory the user keeps other things in, and the uninstall later has an
        # unambiguous folder to remove.
        $InstallDir = Join-Path $Chosen 'vaultis'
    }
}

$InstallDir = [IO.Path]::GetFullPath($InstallDir)
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

    # The file currently being executed. cmd.exe is still running it -- it reads a batch
    # file a line at a time, by offset, as it goes -- so overwriting it now would at best
    # be refused and at worst have cmd resume reading the NEW file at the OLD offset. Yet
    # skipping it is not harmless either: the running copy is whatever version you
    # double-clicked, not necessarily the one being installed, so an in-place upgrade
    # would leave an old installer next to new binaries, and a pinned rollback a newer
    # one. So the package's copy is written beside it as "<name>.new", and the batch
    # header swaps it in after PowerShell has exited, as the very last thing it does.
    # Every other case (any destination other than the folder this was launched from)
    # writes it normally.
    $SelfPath = $null
    if ($env:VAULTIS_SELF -and (Test-Path -LiteralPath $env:VAULTIS_SELF)) {
        $SelfPath = (Get-Item -LiteralPath $env:VAULTIS_SELF).FullName
    }

    foreach ($Item in Get-ChildItem -LiteralPath $StageDir -Force) {
        $Target = Join-Path $InstallDir $Item.Name
        if (($UserDataNames -contains $Item.Name) -and (Test-Path -LiteralPath $Target)) {
            Write-Host "  keeping your existing $($Item.Name) (vault data -- not touching it)"
            continue
        }
        if ($SelfPath -and -not $Item.PSIsContainer -and (Test-Path -LiteralPath $Target)) {
            $Existing = Get-Item -LiteralPath $Target -ErrorAction SilentlyContinue
            if ($Existing -and $Existing.FullName -eq $SelfPath) {
                # Exactly the name the batch header looks for: "%~f0.new".
                Copy-Item -LiteralPath $Item.FullName -Destination ($env:VAULTIS_SELF + '.new') -Force
                Write-Host "  updating the running $($Item.Name) once this script finishes"
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
