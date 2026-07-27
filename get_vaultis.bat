@echo off
setlocal EnableExtensions

REM ==========================================================================
REM Self-contained Windows setup script for vaultis: double-click it.
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

REM The script clones into a RELATIVE folder ("vaultis"), so the working directory
REM decides where everything lands. Pin it to this file's folder: double-clicking
REM already starts there, but "Run as administrator" starts in C:\Windows\System32.
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

# ==========================================
# Configuration
# ==========================================

$RepoUrl = "https://github.com/moka030526-ops/vaultis.git"
$RepoFolder = "vaultis"

$ScriptPath = ".\scripts\build.bat"
$ScriptArgs = @("--release", "--install-rust")

# ==========================================
# Ensure Git is installed
# ==========================================

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Host "Git not found. Installing Git..."

    # Prefer winget where it exists: it resolves the current version itself and
    # verifies the package hash, so nothing unverified is executed here.
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        Write-Host "Installing Git with winget..."
        winget install --id Git.Git --exact --source winget `
            --accept-package-agreements --accept-source-agreements
        if ($LASTEXITCODE -ne 0) {
            throw "winget failed to install Git (exit $LASTEXITCODE). Install Git yourself: https://git-scm.com/download/win"
        }
    }
    else {
        # No winget: download the official installer. The asset name must be resolved
        # from the releases API, NOT guessed. There is no stable "Git-64-bit.exe"
        # asset -- the real ones are versioned (Git-2.55.0.3-64-bit.exe) -- so the
        # convenient-looking /releases/latest/download/Git-64-bit.exe URL is a 404,
        # and with $ErrorActionPreference = "Stop" that killed this script on exactly
        # the machines it exists to set up: a fresh Windows box with no Git.
        Write-Host "winget not available; downloading the Git installer..."

        $Installer = Join-Path $env:TEMP "GitInstaller.exe"

        $Release = Invoke-RestMethod `
            -Uri "https://api.github.com/repos/git-for-windows/git/releases/latest" `
            -Headers @{ "User-Agent" = "vaultis-setup" }

        # Anchored so PortableGit-<ver>-64-bit.7z.exe (a self-extracting archive, not
        # an installer) cannot be picked up instead.
        $Asset = $Release.assets |
            Where-Object { $_.name -match '^Git-[0-9.]+-64-bit\.exe$' } |
            Select-Object -First 1

        if (-not $Asset) {
            throw "Could not find a 64-bit Git installer in the latest release. Install Git yourself: https://git-scm.com/download/win"
        }

        Write-Host "Downloading $($Asset.name)..."
        Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $Installer

        Start-Process `
            -FilePath $Installer `
            -ArgumentList "/VERYSILENT", "/NORESTART" `
            -Wait

        Remove-Item -LiteralPath $Installer -Force -ErrorAction SilentlyContinue
    }

    # Add Git to PATH for the current PowerShell session.
    $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"

    if (${env:ProgramFiles(x86)}) {
        $env:Path = "${env:ProgramFiles(x86)}\Git\cmd;$env:Path"
    }

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "Git was installed, but it is not available in this PowerShell session. Restart PowerShell and run the script again."
    }
}

# ==========================================
# Clone or update repository
# ==========================================

if (-not (Test-Path -LiteralPath $RepoFolder)) {
    Write-Host "Cloning repository into '$RepoFolder'..."

    git clone $RepoUrl $RepoFolder

    if ($LASTEXITCODE -ne 0) {
        throw "Failed to clone repository."
    }
}
else {
    Write-Host "Directory '$RepoFolder' already exists."

    $GitDirectory = Join-Path $RepoFolder ".git"

    if (-not (Test-Path -LiteralPath $GitDirectory)) {
        throw "Directory '$RepoFolder' exists, but it is not a Git repository."
    }

    Push-Location $RepoFolder

    try {
        # Verify this really is the vaultis repository BEFORE pulling from it and then
        # executing scripts\build.bat out of it. Without this check, any pre-existing
        # git repo that happens to sit at .\vault -- planted, or just an unrelated
        # folder of the same name -- would be fetched from ITS own remote and its build
        # script run, which is arbitrary code execution from an unverified source.
        $Origin = (git remote get-url origin 2>$null)
        if ($LASTEXITCODE -ne 0 -or -not $Origin) {
            throw "Directory '$RepoFolder' is a Git repository with no 'origin' remote; refusing to build from it."
        }
        # Compare ignoring a trailing .git and case, which are cosmetic on GitHub URLs.
        # TrimEnd('/') first, so a remote written as ".../vaultis.git/" also normalises.
        $Normalize = { param($u) (($u.Trim().TrimEnd('/')) -replace '\.git$', '').ToLowerInvariant() }
        if ((& $Normalize $Origin) -ne (& $Normalize $RepoUrl)) {
            throw "Directory '$RepoFolder' points at a different repository ('$Origin', expected '$RepoUrl'). Refusing to pull and build from it. Move or delete that folder and re-run."
        }

        Write-Host "Pulling the latest changes..."

        git pull --ff-only

        if ($LASTEXITCODE -ne 0) {
            throw "Failed to pull the latest repository changes."
        }
    }
    finally {
        Pop-Location
    }
}

# ==========================================
# Enter repository
# ==========================================

Set-Location $RepoFolder

# ==========================================
# Run build script
# ==========================================

if (-not (Test-Path -LiteralPath $ScriptPath)) {
    throw "Build script not found: $ScriptPath"
}

Write-Host "Running build script..."

& cmd.exe /c $ScriptPath @ScriptArgs

if ($LASTEXITCODE -ne 0) {
    throw "Build script failed with exit code $LASTEXITCODE."
}

Write-Host "Build completed successfully."
