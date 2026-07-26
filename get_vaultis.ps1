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
