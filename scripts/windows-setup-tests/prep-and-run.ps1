<#
Runs INSIDE a Windows Sandbox instance (via a .wsb LogonCommand, never by hand).
Makes the sandbox's git/rustup/winget state match the requested combo, then runs the
real get_vaultis.bat and logs the whole thing to a folder mapped back to the host, so
the log survives after the disposable sandbox is closed and discarded.

Each sandbox starts genuinely clean (no git, no rustup/cargo), so "absent" is the
default and needs no work. "Present" is made real by actually installing the tool --
nothing here is mocked, since the one thing a sandbox is for is testing the real
network+installer code paths safely.

The run only PASSES if the sandbox ends up with vaultis actually installed: both
desktop shortcuts present, each pointing at a vaultis-gui.exe that exists. That is
checked here rather than left to the eye, because the sandbox -- and its desktop --
is destroyed the moment the window closes. A screenshot of that desktop is saved
next to the log so the two icons are still there to look at afterwards.
#>
param(
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Git,
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Rustup,
    [Parameter(Mandatory)] [ValidateSet('true', 'false')] [string]$Winget,
    [Parameter(Mandatory)] [string]$ComboName,
    # Set only by the self-elevation re-launch below, to stop it recursing forever if
    # the elevated token somehow still is not there.
    [switch]$Relaunched
)

$ErrorActionPreference = 'Stop'

# Invoke-WebRequest draws a progress bar for every chunk it receives, and on PowerShell
# 5.1 that redraw dominates the transfer -- the ~70 MB Git installer goes from seconds
# to many minutes, with a console that looks frozen the whole time. Off, everywhere.
$ProgressPreference = 'SilentlyContinue'

$WantGit = $Git -eq 'true'
$WantRustup = $Rustup -eq 'true'
$WantWinget = $Winget -eq 'true'

$ResultsDir = 'C:\vaultis-results'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null

# --- elevation --------------------------------------------------------------
# Git's installer is manifested requireAdministrator, so /VERYSILENT does NOT mean
# unattended without an elevated token: it raises a UAC consent dialog and then waits
# forever. The sandbox account is an administrator, but a LogonCommand is not
# guaranteed to start with the elevated token. Take it here, once, up front -- where a
# single consent click is expected and visible -- rather than halfway through a run.
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$IsElevated = (New-Object Security.Principal.WindowsPrincipal($Identity)).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $IsElevated -and -not $Relaunched) {
    Write-Host "Not running elevated; re-launching elevated (accept the UAC prompt if one appears)..."
    Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
        '-Git', $Git, '-Rustup', $Rustup, '-Winget', $Winget,
        '-ComboName', $ComboName, '-Relaunched'
    )
    exit 0
}

# --- progress that reaches the host WHILE the run is going -------------------
# Start-Transcript buffers, so a host watching results\ mid-run sees a log that stops
# dead after the first line or two and cannot tell a slow download or a 10-minute
# cargo build from a hang. These step lines are written straight through to disk, so
# the .status file next to the log is always current.
$StatusFile = Join-Path $ResultsDir "$ComboName.status"
[IO.File]::WriteAllText($StatusFile, '')

function Write-Step([string]$Message) {
    $Line = '[{0}] {1}' -f (Get-Date -Format 'HH:mm:ss'), $Message
    Write-Host $Line
    [IO.File]::AppendAllText($StatusFile, $Line + [Environment]::NewLine)
}

Start-Transcript -Path (Join-Path $ResultsDir "$ComboName.log") -Force

$ExitCode = $null
$Failures = New-Object Collections.Generic.List[string]

try {
    Write-Step "=== combo '$ComboName': want git=$WantGit rustup=$WantRustup winget=$WantWinget ==="
    Write-Step "elevated: $IsElevated"

    # --- winget -------------------------------------------------------------
    # Most current sandbox base images ship App Installer (winget) already provisioned.
    # There is no offline way to ADD it if the image lacks it, so "present" just means
    # "leave it alone"; "absent" removes the per-user package so Get-Command can't find it.
    if (-not $WantWinget) {
        Write-Step 'Removing winget (App Installer) for this combo...'
        Get-AppxPackage -Name Microsoft.DesktopAppInstaller -ErrorAction SilentlyContinue |
            Remove-AppxPackage -ErrorAction SilentlyContinue
    }
    $HasWinget = [bool](Get-Command winget -ErrorAction SilentlyContinue)
    Write-Step "winget on PATH: $HasWinget"
    if ($WantWinget -and -not $HasWinget) {
        Write-Warning "This sandbox image has no winget to begin with -- this run is really testing the 'winget absent' path no matter what the combo name says."
    }

    # --- git ----------------------------------------------------------------
    if ($WantGit) {
        Write-Step 'Pre-installing Git for this combo (via winget if available, else a direct download)...'
        if ($HasWinget) {
            winget install --id Git.Git --exact --source winget --accept-package-agreements --accept-source-agreements
        }
        else {
            $GitInstaller = Join-Path $env:TEMP 'Git-prep.exe'
            $Release = Invoke-RestMethod -Uri 'https://api.github.com/repos/git-for-windows/git/releases/latest' -Headers @{ 'User-Agent' = 'vaultis-sandbox-test' }
            $Asset = $Release.assets | Where-Object { $_.name -match '^Git-[0-9.]+-64-bit\.exe$' } | Select-Object -First 1
            if (-not $Asset) {
                throw 'No 64-bit Git installer in the latest git-for-windows release; cannot set up this combo.'
            }
            Write-Step "Downloading $($Asset.name) (~$([int]($Asset.size / 1MB)) MB)..."
            Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $GitInstaller
            Write-Step 'Running the Git installer...'
            # Same installer the real script would use. Needs the elevated token taken above.
            Start-Process -FilePath $GitInstaller -ArgumentList '/VERYSILENT', '/NORESTART' -Wait
        }
        $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
    }
    Write-Step "git on PATH: $([bool](Get-Command git -ErrorAction SilentlyContinue))"

    # --- rustup -------------------------------------------------------------
    # "rustup present" is deliberately installed WITHOUT a default toolchain, so cargo
    # stays absent. That mirrors the one special-cased branch in scripts\build.bat --
    # "rustup is installed but no toolchain is active" -- rather than just pre-satisfying
    # the whole Rust check and skipping it entirely.
    if ($WantRustup) {
        Write-Step 'Pre-installing rustup only, with no default toolchain...'
        $RustupInit = Join-Path $env:TEMP 'rustup-init-prep.exe'
        Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $RustupInit
        & $RustupInit -y --default-toolchain none --profile minimal
        $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    }
    Write-Step "rustup on PATH: $([bool](Get-Command rustup -ErrorAction SilentlyContinue))"
    Write-Step "cargo on PATH: $([bool](Get-Command cargo -ErrorAction SilentlyContinue))"

    # --- run the real script under test -------------------------------------
    $WorkDir = 'C:\vaultis-work'
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
    Copy-Item 'C:\vaultis-src\get_vaultis.bat' (Join-Path $WorkDir 'get_vaultis.bat') -Force
    Push-Location $WorkDir

    $RunLog = Join-Path $ResultsDir "$ComboName-get_vaultis.log"
    Write-Step '=== running get_vaultis.bat (clone + release build; expect 10-20 minutes) ==='
    Write-Step "    its output -> $RunLog"
    # Both redirections inside the cmd string are load-bearing:
    #   < NUL  get_vaultis.bat's trailing `pause` fires whenever %cmdcmdline% mentions
    #          its own filename, which `cmd /c get_vaultis.bat` does. Empty stdin makes
    #          that pause a no-op instead of hanging the sandbox forever.
    #   2>&1   build.bat reports EVERY error with `1>&2`. Merged here, by cmd, so stderr
    #          is interleaved with the stdout it belongs between before it reaches the
    #          pipe -- redirecting only stdout captures a log with no error in it.
    #
    # And piped rather than left to Start-Transcript, which records only what the
    # PowerShell host itself writes: a native command's output goes straight to the
    # console down an inherited handle, so the transcript never sees a byte of it. That
    # is not theoretical -- it produced a failing run whose log held the exit code and
    # nothing else. The pipe forces every descendant's output through something we own.
    cmd /c "get_vaultis.bat < NUL 2>&1" | Tee-Object -FilePath $RunLog
    $ExitCode = $LASTEXITCODE
    Write-Step "=== get_vaultis.bat exited with code $ExitCode ==="

    Pop-Location

    # --- did vaultis actually end up installed? -----------------------------
    # The point of the whole exercise: two working shortcuts on the desktop. Checked,
    # not assumed -- get_vaultis.bat treats a shortcut failure as a warning and still
    # exits 0, so the exit code alone does not answer the question.
    if ($ExitCode -ne 0) {
        $Failures.Add("get_vaultis.bat exited $ExitCode")
        # Echo the tail into the step log as well, so the small .status file says WHY it
        # failed and not merely where -- the whole point of a one-screen status file.
        if (Test-Path -LiteralPath $RunLog) {
            Write-Step '--- last 20 lines of get_vaultis.bat output ---'
            Get-Content -LiteralPath $RunLog |
                Where-Object { $_.Trim() } |
                Select-Object -Last 20 |
                ForEach-Object { Write-Step "  | $_" }
        }
    }

    $Desktop = [Environment]::GetFolderPath('Desktop')
    $Shell = New-Object -ComObject WScript.Shell
    foreach ($Name in @('vaultis (View)', 'vaultis (Edit)')) {
        $Lnk = Join-Path $Desktop "$Name.lnk"
        if (-not (Test-Path -LiteralPath $Lnk)) {
            $Failures.Add("no desktop shortcut: $Name.lnk")
            continue
        }
        $Target = $Shell.CreateShortcut($Lnk).TargetPath
        if (-not $Target -or -not (Test-Path -LiteralPath $Target)) {
            $Failures.Add("$Name.lnk points at a missing exe: $Target")
            continue
        }
        Write-Step "shortcut OK: $Name.lnk -> $Target"
    }

    # --- a picture of that desktop, since the sandbox is about to be thrown away ---
    # Best effort: this is evidence, not a check.
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $Explorer = New-Object -ComObject Shell.Application
        $Explorer.MinimizeAll()
        Start-Sleep -Seconds 2
        $Bounds = [Windows.Forms.SystemInformation]::VirtualScreen
        $Bitmap = New-Object Drawing.Bitmap $Bounds.Width, $Bounds.Height
        $Graphics = [Drawing.Graphics]::FromImage($Bitmap)
        $Graphics.CopyFromScreen($Bounds.Location, [Drawing.Point]::Empty, $Bounds.Size)
        $Shot = Join-Path $ResultsDir "$ComboName-desktop.png"
        $Bitmap.Save($Shot, [Drawing.Imaging.ImageFormat]::Png)
        $Graphics.Dispose()
        $Bitmap.Dispose()
        $Explorer.UndoMinimizeALL()
        Write-Step "desktop screenshot: $Shot"
    }
    catch {
        Write-Step "could not screenshot the desktop: $($_.Exception.Message)"
    }
}
catch {
    # Without this the window vanishes on a terminating error and the transcript ends
    # mid-sentence, which is indistinguishable from a hang.
    $Failures.Add("prep script error: $($_.Exception.Message)")
    Write-Step "ERROR: $($_.Exception.Message)"
    Write-Host $_.ScriptStackTrace
}

$Result = if ($Failures.Count -eq 0) { 'PASS' } else { 'FAIL' }
Write-Step "RESULT: $Result"
foreach ($Failure in $Failures) { Write-Step "  - $Failure" }

# One line per combo, for show-results.ps1 on the host to collate.
$Summary = "$Result`t$ComboName`tgit=$WantGit rustup=$WantRustup winget=$WantWinget`texit=$ExitCode"
if ($Failures.Count) { $Summary += "`t" + ($Failures -join '; ') }
[IO.File]::WriteAllText((Join-Path $ResultsDir "$ComboName.result"), $Summary + [Environment]::NewLine)

Stop-Transcript

# The log is complete on the host by now, so holding here costs nothing and buys the
# one thing a transcript cannot show: the actual desktop, with the two icons on it.
Write-Host ''
Write-Host "$Result -- $ComboName. Minimize this window to see the two vaultis icons on the desktop."
Write-Host 'Press Enter to close (or just close the sandbox window).'
Read-Host | Out-Null
