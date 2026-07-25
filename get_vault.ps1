$ErrorActionPreference = "Stop"

# ==========================================
# Configuration
# ==========================================

$RepoUrl = "https://github.com/moka030526-ops/vaultis.git"
$RepoFolder = "vault"

$ScriptPath = ".\scripts\build.bat"
$ScriptArgs = @("--release", "--install-rust")

# ==========================================
# Ensure Git is installed
# ==========================================

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Host "Git not found. Installing Git..."

    $Installer = Join-Path $env:TEMP "GitInstaller.exe"

    Invoke-WebRequest `
        -Uri "https://github.com/git-for-windows/git/releases/latest/download/Git-64-bit.exe" `
        -OutFile $Installer

    Start-Process `
        -FilePath $Installer `
        -ArgumentList "/VERYSILENT", "/NORESTART" `
        -Wait

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