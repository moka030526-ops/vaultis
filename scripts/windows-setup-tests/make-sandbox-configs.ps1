<#
Run this ONCE on the host (not inside a sandbox) to (re)generate the .wsb files under
sandbox-configs\, one per scenario. Double-click any of the generated files to launch
that scenario in a disposable Windows Sandbox.

Regenerate any time this script or prep-and-run.ps1 moves, since the .wsb files bake in
absolute host paths.
#>
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ResultsDir = Join-Path $PSScriptRoot 'results'
$ConfigDir = Join-Path $PSScriptRoot 'sandbox-configs'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null

# get_vaultis.bat downloads a prebuilt release: no git, no rustup, no winget, no
# compiler. The old eight-way git/rustup/winget matrix therefore ran eight copies of one
# code path, and each one spent 10-20 minutes compiling. What is actually worth
# exercising is a clean machine and the replace-an-existing-install path every update
# takes.
$Scenarios = @(
    @{ Name = 'fresh';     Note = 'a clean machine that has never seen vaultis' }
    @{ Name = 'reinstall'; Note = 'run twice: the second run replaces the first install' }
)

# Nothing compiles in the sandbox any more, so the default memory allocation is ample
# and there is no reason to reserve gigabytes of the host's RAM for a download.
$SandboxMemoryMB = 4096

foreach ($Scenario in $Scenarios) {
    $PrepScriptInSandbox = 'C:\vaultis-src\scripts\windows-setup-tests\prep-and-run.ps1'
    $Command = "powershell -NoProfile -ExecutionPolicy Bypass -File $PrepScriptInSandbox " +
        "-Scenario $($Scenario.Name)"

    $Xml = @"
<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Enable</Networking>
  <MemoryInMB>$SandboxMemoryMB</MemoryInMB>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$RepoRoot</HostFolder>
      <SandboxFolder>C:\vaultis-src</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$ResultsDir</HostFolder>
      <SandboxFolder>C:\vaultis-results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>$Command</Command>
  </LogonCommand>
</Configuration>
"@

    $OutFile = Join-Path $ConfigDir "$($Scenario.Name).wsb"
    Set-Content -LiteralPath $OutFile -Value $Xml -Encoding ASCII
    Write-Host "wrote $OutFile  -- $($Scenario.Note)"
}

Write-Host ""
Write-Host "Done. Run them one at a time (only one sandbox needs to be open):"
Write-Host "  each .wsb in $ConfigDir"
Write-Host "Logs land in:"
Write-Host "  $ResultsDir"
Write-Host "Then collate them with:"
Write-Host "  .\show-results.ps1"
