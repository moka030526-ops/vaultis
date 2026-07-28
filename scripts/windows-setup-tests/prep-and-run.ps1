<#
Runs INSIDE a Windows Sandbox instance (via the .wsb LogonCommand, never by hand).
Runs the real get_vaultis.bat on a genuinely clean Windows machine and logs the whole
thing to a folder mapped back to the host, so the log survives after the disposable
sandbox is closed and discarded.

get_vaultis.bat is run TWICE, and checked after each:

  run 1  a clean machine that has never seen vaultis
  run 2  the same installer over the top of run 1's install -- get_vaultis.bat deletes
         and re-extracts the install directory, which is the path every user takes on
         every later release, and the one where a half-deleted install would show up

Both in one sandbox: the second run needs the first run's install to be there, so they
have to share a machine anyway, and booting a second VM to test an upgrade would have
to install first regardless.

A run PASSES only if vaultis really ended up installed BOTH times: both binaries under
%LOCALAPPDATA%\Programs\vaultis, and both desktop shortcuts pointing at a
vaultis-gui.exe that exists. That is checked here rather than left to the eye, because
the sandbox -- and its desktop -- is destroyed the moment the window closes. A
screenshot of that desktop is saved next to the log so the icons remain to look at.

get_vaultis.bat's own exit code is not sufficient: it treats a failure to create the
shortcuts as a warning and still exits 0. Nothing here needs elevation either, which
the run records rather than assumes.
#>
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$ResultsDir = 'C:\vaultis-results'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null

# Start-Transcript buffers, so a host watching results\ mid-run sees a log that stops
# dead after a line or two and cannot tell a slow download from a hang. These step lines
# are written straight through to disk, so run.status is always current.
$StatusFile = Join-Path $ResultsDir 'run.status'
[IO.File]::WriteAllText($StatusFile, '')

function Write-Step([string]$Message) {
    $Line = '[{0}] {1}' -f (Get-Date -Format 'HH:mm:ss'), $Message
    Write-Host $Line
    [IO.File]::AppendAllText($StatusFile, $Line + [Environment]::NewLine)
}

Start-Transcript -Path (Join-Path $ResultsDir 'run.log') -Force

$ExitCode = $null
$Failures = New-Object Collections.Generic.List[string]
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\vaultis'
$Desktop = [Environment]::GetFolderPath('Desktop')

# Checked after EVERY run, not once at the end: an upgrade that leaves a working install
# is the whole point of run 2, and checking only afterwards cannot say which run broke.
function Test-Installed([int]$Run) {
    foreach ($Exe in 'vaultis-gui.exe', 'vaultis.exe') {
        $Path = Join-Path $InstallDir $Exe
        if (Test-Path -LiteralPath $Path) { Write-Step "  run ${Run}: installed $Exe" }
        else { $Failures.Add("run ${Run}: not installed: $Path") }
    }

    # The point of the whole exercise: two working shortcuts on the desktop.
    $Shell = New-Object -ComObject WScript.Shell
    foreach ($Name in 'vaultis (View)', 'vaultis (Edit)') {
        $Lnk = Join-Path $Desktop "$Name.lnk"
        if (-not (Test-Path -LiteralPath $Lnk)) {
            $Failures.Add("run ${Run}: no desktop shortcut: $Name.lnk")
            continue
        }
        $Target = $Shell.CreateShortcut($Lnk).TargetPath
        if (-not $Target -or -not (Test-Path -LiteralPath $Target)) {
            $Failures.Add("run ${Run}: $Name.lnk points at a missing exe: $Target")
            continue
        }
        Write-Step "  run ${Run}: shortcut OK: $Name.lnk"
    }
}

try {
    $Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $IsElevated = (New-Object Security.Principal.WindowsPrincipal($Identity)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    Write-Step '=== testing get_vaultis.bat on a clean machine ==='
    # Recorded because "needs no admin rights" is a claim about the installer worth
    # having evidence for, not an assumption.
    Write-Step "elevated: $IsElevated"

    $WorkDir = 'C:\vaultis-work'
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
    Copy-Item 'C:\vaultis-src\get_vaultis.bat' (Join-Path $WorkDir 'get_vaultis.bat') -Force
    Push-Location $WorkDir

    foreach ($Run in 1, 2) {
        $What = if ($Run -eq 1) { 'clean install' } else { 'over the top of run 1' }
        $RunLog = Join-Path $ResultsDir "get_vaultis-$Run.log"
        Write-Step "=== run $Run of 2: $What ==="
        Write-Step "    its output -> $RunLog"
        # Both redirections inside the cmd string are load-bearing:
        #   < NUL  get_vaultis.bat's trailing `pause` fires whenever %cmdcmdline%
        #          mentions its own filename, which `cmd /c get_vaultis.bat` does. Empty
        #          stdin makes that pause a no-op instead of hanging the sandbox forever.
        #   2>&1   so a failure written to stderr is interleaved with the stdout it
        #          belongs between, before it reaches the pipe.
        #
        # And piped rather than left to Start-Transcript, which records only what the
        # PowerShell host itself writes: a native command's output goes straight to the
        # console down an inherited handle, so the transcript never sees a byte of it.
        # That is not theoretical -- it produced a failing run whose log held the exit
        # code and nothing else.
        cmd /c "get_vaultis.bat < NUL 2>&1" | Tee-Object -FilePath $RunLog
        $ExitCode = $LASTEXITCODE
        Write-Step "=== run $Run exited with code $ExitCode ==="

        if ($ExitCode -ne 0) {
            $Failures.Add("get_vaultis.bat run $Run exited $ExitCode")
            if (Test-Path -LiteralPath $RunLog) {
                Write-Step '--- last 20 lines of its output ---'
                Get-Content -LiteralPath $RunLog |
                    Where-Object { $_.Trim() } |
                    Select-Object -Last 20 |
                    ForEach-Object { Write-Step "  | $_" }
            }
            break
        }

        Test-Installed -Run $Run
    }

    Pop-Location

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
        $Shot = Join-Path $ResultsDir 'run-desktop.png'
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

$Summary = "$Result`texit=$ExitCode"
if ($Failures.Count) { $Summary += "`t" + ($Failures -join '; ') }
[IO.File]::WriteAllText((Join-Path $ResultsDir 'run.result'), $Summary + [Environment]::NewLine)

Stop-Transcript

# The log is complete on the host by now, so holding here costs nothing and buys the
# one thing a transcript cannot show: the actual desktop, with the two icons on it.
Write-Host ''
Write-Host "$Result -- minimize this window to see the two vaultis icons on the desktop."
Write-Host 'Try double-clicking one: only launching it proves the download really runs here.'
Write-Host 'Press Enter to close (or just close the sandbox window).'
Read-Host | Out-Null
