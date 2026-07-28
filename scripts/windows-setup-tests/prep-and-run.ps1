<#
Runs INSIDE a Windows Sandbox instance (via a .wsb LogonCommand, never by hand).
Makes the sandbox's git/rustup/winget state match the requested combo, then runs the
real get_vaultis.bat and logs the whole thing to a folder mapped back to the host, so
the log survives after the disposable sandbox is closed and discarded.

Each sandbox starts genuinely clean (no git, no rustup/cargo), so "absent" is the
default and needs no work. "Present" is made real by actually installing the tool --
nothing here is mocked, since the one thing a sandbox is for is testing the real
network+installer code paths safely.
#>
param(
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Git,
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Rustup,
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Winget,
    [Parameter(Mandatory)] [string]$ComboName
)

$ErrorActionPreference = 'Stop'
$WantGit = $Git -eq 'true'
$WantRustup = $Rustup -eq 'true'
$WantWinget = $Winget -eq 'true'

$ResultsDir = 'C:\vaultis-results'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null
Start-Transcript -Path (Join-Path $ResultsDir "$ComboName.log") -Force

Write-Host "=== combo '$ComboName': want git=$WantGit rustup=$WantRustup winget=$WantWinget ==="

# --- winget -----------------------------------------------------------------
# Most current sandbox base images ship App Installer (winget) already provisioned.
# There is no offline way to ADD it if the image lacks it, so "present" just means
# "leave it alone"; "absent" removes the per-user package so Get-Command can't find it.
if (-not $WantWinget) {
    Write-Host "Removing winget (App Installer) for this combo..."
    Get-AppxPackage -Name Microsoft.DesktopAppInstaller -ErrorAction SilentlyContinue |
        Remove-AppxPackage -ErrorAction SilentlyContinue
}
$HasWinget = [bool](Get-Command winget -ErrorAction SilentlyContinue)
Write-Host "winget on PATH: $HasWinget"
if ($WantWinget -and -not $HasWinget) {
    Write-Warning "This sandbox image has no winget to begin with -- this run is really testing the 'winget absent' path no matter what the combo name says."
}

# --- git ----------------------------------------------------------------
if ($WantGit) {
    Write-Host "Pre-installing Git for this combo (via winget if available, else a direct download)..."
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        winget install --id Git.Git --exact --source winget --accept-package-agreements --accept-source-agreements
    }
    else {
        $GitInstaller = Join-Path $env:TEMP 'Git-prep.exe'
        $Release = Invoke-RestMethod -Uri 'https://api.github.com/repos/git-for-windows/git/releases/latest' -Headers @{ 'User-Agent' = 'vaultis-sandbox-test' }
        $Asset = $Release.assets | Where-Object { $_.name -match '^Git-[0-9.]+-64-bit\.exe$' } | Select-Object -First 1
        Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $GitInstaller
        # Same installer the real script would use -- may raise a UAC consent prompt;
        # click "Yes" if it appears, this isn't running elevated by default.
        Start-Process -FilePath $GitInstaller -ArgumentList '/VERYSILENT', '/NORESTART' -Wait
    }
    $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
}
Write-Host "git on PATH: $([bool](Get-Command git -ErrorAction SilentlyContinue))"

# --- rustup ---------------------------------------------------------------
# "rustup present" is deliberately installed WITHOUT a default toolchain, so cargo
# stays absent. That mirrors the one special-cased branch in scripts\build.bat --
# "rustup is installed but no toolchain is active" -- rather than just pre-satisfying
# the whole Rust check and skipping it entirely.
if ($WantRustup) {
    Write-Host "Pre-installing rustup only, with no default toolchain..."
    $RustupInit = Join-Path $env:TEMP 'rustup-init-prep.exe'
    Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $RustupInit
    & $RustupInit -y --default-toolchain none --profile minimal
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
Write-Host "rustup on PATH: $([bool](Get-Command rustup -ErrorAction SilentlyContinue))"
Write-Host "cargo on PATH: $([bool](Get-Command cargo -ErrorAction SilentlyContinue))"

# --- run the real script under test --------------------------------------
$WorkDir = 'C:\vaultis-work'
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
Copy-Item 'C:\vaultis-src\get_vaultis.bat' (Join-Path $WorkDir 'get_vaultis.bat') -Force
Push-Location $WorkDir

Write-Host "=== running get_vaultis.bat ==="
# Redirected from NUL: get_vaultis.bat's own trailing `pause` fires whenever
# %cmdcmdline% mentions its own filename, which is true of `cmd /c get_vaultis.bat`
# too. Empty stdin makes that pause a no-op instead of hanging the sandbox forever.
cmd /c "get_vaultis.bat < NUL"
Write-Host "=== get_vaultis.bat exited with code $LASTEXITCODE ==="

Pop-Location
Stop-Transcript
