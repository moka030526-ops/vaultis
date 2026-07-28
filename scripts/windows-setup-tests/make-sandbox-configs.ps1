<#
Run this ONCE on the host (not inside a sandbox) to (re)generate sandbox-configs\
vaultis.wsb. Double-click that file to run the test in a disposable Windows Sandbox.

There is ONE config, deliberately. get_vaultis.bat downloads a prebuilt release and
uses no git, rustup, winget or compiler, so the old eight-way matrix of those tools ran
eight copies of a single code path. What is worth exercising -- a clean machine, and
the replace-an-existing-install path every update takes -- is two runs of the installer,
which is two minutes inside one sandbox rather than two more VMs to boot.

Regenerate any time this script or prep-and-run.ps1 moves, since the .wsb bakes in
absolute host paths.
#>
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ResultsDir = Join-Path $PSScriptRoot 'results'
$ConfigDir = Join-Path $PSScriptRoot 'sandbox-configs'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null

$ConfigName = 'vaultis.wsb'

# Earlier versions of this kit wrote one .wsb per combination -- eight of them, then two
# -- and this script only ever ADDED files. So a folder that has seen an older version
# is still full of configs that boot a scenario this kit no longer has, and there is no
# way to tell them apart by looking. Clear them out rather than leave the pile.
$Stale = @(Get-ChildItem -LiteralPath $ConfigDir -Filter '*.wsb' -File |
    Where-Object { $_.Name -ne $ConfigName })
foreach ($File in $Stale) {
    Remove-Item -LiteralPath $File.FullName -Force
    Write-Host "removed stale config: $($File.Name)"
}

# Same for their results: an old FAIL sitting in results\ is indistinguishable from a
# current one, and this run cannot overwrite a file it will never write.
$Keep = 'run.status', 'run.result', 'run.log', 'run-desktop.png',
        'get_vaultis-1.log', 'get_vaultis-2.log'
$StaleResults = @(Get-ChildItem -LiteralPath $ResultsDir -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -notin $Keep })
foreach ($File in $StaleResults) {
    Remove-Item -LiteralPath $File.FullName -Force
    Write-Host "removed stale result: $($File.Name)"
}

# Nothing compiles in the sandbox, so the default allocation is ample: this reserves
# host RAM for a download and an unzip, not for a build.
$SandboxMemoryMB = 4096

$PrepScriptInSandbox = 'C:\vaultis-src\scripts\windows-setup-tests\prep-and-run.ps1'
$Command = "powershell -NoProfile -ExecutionPolicy Bypass -File $PrepScriptInSandbox"

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

$OutFile = Join-Path $ConfigDir $ConfigName
Set-Content -LiteralPath $OutFile -Value $Xml -Encoding ASCII

Write-Host ""
Write-Host "wrote $OutFile"
Write-Host "Double-click it to run the test. Results land in:"
Write-Host "  $ResultsDir"
Write-Host "Watch it while it runs with:"
Write-Host "  Get-Content '$(Join-Path $ResultsDir 'run.status')' -Wait"
