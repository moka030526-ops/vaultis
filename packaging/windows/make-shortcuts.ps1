<#
Create two vaultis shortcuts on the Windows Desktop:

    "vaultis (View)"  -> vaultis-gui.exe            (locked vault icon)
    "vaultis (Edit)"  -> vaultis-gui.exe --write    (unlocked vault icon)

Both point at the SAME windowed binary (no console window); only the --write flag
and the icon differ.

NOTE: vaultis-gui.exe is a BUILD ARTIFACT and is NOT shipped in the repo. Build it
first (`cargo build --release` -> target\release\vaultis-gui.exe), or copy it
somewhere and point this script at it.

Usage (PowerShell):

  # Simplest: after building in the repo. Auto-finds target\release\vaultis-gui.exe
  # (then target\debug, then the windows-gnu cross target, then your PATH) and the
  # committed icons in packaging\icons:
  powershell -ExecutionPolicy Bypass -File make-shortcuts.ps1

  # Point at the exe explicitly:
  powershell -ExecutionPolicy Bypass -File make-shortcuts.ps1 -Exe "C:\apps\vaultis\vaultis-gui.exe"

  # Deployed install (exe + the icons copied into ONE folder):
  powershell -ExecutionPolicy Bypass -File make-shortcuts.ps1 -InstallDir "C:\Program Files\vaultis"
#>

param(
    [string]$Exe = "",          # path to vaultis-gui.exe (auto-detected if empty)
    [string]$IconDir = "",      # folder holding the two .ico files (defaults to repo icons)
    [string]$InstallDir = ""    # a folder containing the exe (+ icons) for a deployed install
)

$ErrorActionPreference = "Stop"

# repo root = two levels up from packaging\windows\
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

# --- locate the binary -------------------------------------------------------
if (-not $Exe) {
    if ($InstallDir) {
        $Exe = Join-Path $InstallDir "vaultis-gui.exe"
    } else {
        $candidates = @(
            (Join-Path $repo "target\release\vaultis-gui.exe"),
            (Join-Path $repo "target\debug\vaultis-gui.exe"),
            (Join-Path $repo "target\x86_64-pc-windows-gnu\release\vaultis-gui.exe")
        )
        foreach ($c in $candidates) { if (Test-Path $c) { $Exe = $c; break } }
        if (-not $Exe) {
            $onPath = Get-Command vaultis-gui.exe -ErrorAction SilentlyContinue
            if ($onPath) { $Exe = $onPath.Source }
        }
    }
}

if (-not $Exe -or -not (Test-Path $Exe)) {
    Write-Host ""
    Write-Host "Could not find vaultis-gui.exe." -ForegroundColor Yellow
    Write-Host "It is a build artifact, not part of the repo. Do one of:"
    Write-Host "  * build it:   cargo build --release      (-> target\release\vaultis-gui.exe)"
    Write-Host "  * pass it:    make-shortcuts.ps1 -Exe `"C:\path\to\vaultis-gui.exe`""
    Write-Host "  * deploy it:  copy the exe + the packaging\icons folder into one directory,"
    Write-Host "                then: make-shortcuts.ps1 -InstallDir `"C:\that\directory`""
    exit 1
}
$Exe = (Resolve-Path $Exe).Path

# --- locate the icons --------------------------------------------------------
if (-not $IconDir) {
    if ($InstallDir -and (Test-Path (Join-Path $InstallDir "vaultis-locked.ico"))) {
        $IconDir = $InstallDir
    } elseif ($InstallDir -and (Test-Path (Join-Path $InstallDir "icons\vaultis-locked.ico"))) {
        $IconDir = Join-Path $InstallDir "icons"
    } else {
        $IconDir = Join-Path $repo "packaging\icons"
    }
}
$lockedIco = Join-Path $IconDir "vaultis-locked.ico"
$unlockIco = Join-Path $IconDir "vaultis-unlocked.ico"
foreach ($p in @($lockedIco, $unlockIco)) {
    if (-not (Test-Path $p)) {
        throw "missing icon: $p  (generate with packaging\icons\make_icons.py, or pass -IconDir)"
    }
}
$lockedIco = (Resolve-Path $lockedIco).Path
$unlockIco = (Resolve-Path $unlockIco).Path

# --- create the shortcuts ----------------------------------------------------
$workDir = Split-Path -Parent $Exe
$desktop = [Environment]::GetFolderPath("Desktop")
$shell   = New-Object -ComObject WScript.Shell

function New-PMShortcut($name, $arguments, $icon) {
    $lnk = $shell.CreateShortcut((Join-Path $desktop "$name.lnk"))
    $lnk.TargetPath       = $Exe
    $lnk.Arguments        = $arguments
    $lnk.WorkingDirectory = $workDir
    $lnk.IconLocation     = "$icon,0"
    $lnk.Description       = "vaultis encrypted estate vault"
    $lnk.Save()
    Write-Host "created: $name.lnk"
}

New-PMShortcut "vaultis (View)" ""        $lockedIco
New-PMShortcut "vaultis (Edit)" "--write" $unlockIco

# --- refresh the shell icon cache -------------------------------------------
# Windows caches shortcut icons keyed by icon PATH, not by file contents. If an icon
# file at a path it has already cached is replaced (e.g. regenerating the .ico files),
# Explorer keeps drawing the OLD cached image — including a stale generic icon from a
# file it previously failed to decode. Nudging the cache makes the new icon appear
# without a sign-out. Best effort: ie4uinit is absent on some editions/older builds.
$ie4 = Join-Path $env:SystemRoot "System32\ie4uinit.exe"
if (Test-Path $ie4) {
    # -show on Win10/11; -ClearIconCache on older builds. Either may be a no-op.
    & $ie4 -show 2>$null
    & $ie4 -ClearIconCache 2>$null
}

Write-Host ""
Write-Host "Done. Two shortcuts are on your Desktop:"
Write-Host "  vaultis (View)  - read-only  (locked vault icon)"
Write-Host "  vaultis (Edit)  - edit mode  (unlocked vault icon)"
Write-Host "exe:   $Exe"
Write-Host "icons: $IconDir"
