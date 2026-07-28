<#
Run this ONE on the host (not inside a sandbox) to (re)generate the 8 .wsb files under
sandbox-configs\, one per git/rustup/winget present-or-absent combination. Double-click
any of the generated files to launch that combo in a disposable Windows Sandbox.

Regenerate any time this script or prep-and-run.ps1 moves, since the .wsb files bake in
absolute host paths.
#>
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ResultsDir = Join-Path $PSScriptRoot 'results'
$ConfigDir = Join-Path $PSScriptRoot 'sandbox-configs'
New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null

$Combos = @(
    @{ Name = 'git-rustup-winget';       Git = 'true';  Rustup = 'true';  Winget = 'true'  }
    @{ Name = 'git-rustup-nowinget';     Git = 'true';  Rustup = 'true';  Winget = 'false' }
    @{ Name = 'git-norustup-winget';     Git = 'true';  Rustup = 'false'; Winget = 'true'  }
    @{ Name = 'git-norustup-nowinget';   Git = 'true';  Rustup = 'false'; Winget = 'false' }
    @{ Name = 'nogit-rustup-winget';     Git = 'false'; Rustup = 'true';  Winget = 'true'  }
    @{ Name = 'nogit-rustup-nowinget';   Git = 'false'; Rustup = 'true';  Winget = 'false' }
    @{ Name = 'nogit-norustup-winget';   Git = 'false'; Rustup = 'false'; Winget = 'true'  }
    @{ Name = 'nogit-norustup-nowinget'; Git = 'false'; Rustup = 'false'; Winget = 'false' }
)

foreach ($Combo in $Combos) {
    $PrepScriptInSandbox = 'C:\vaultis-src\scripts\windows-setup-tests\prep-and-run.ps1'
    $Command = "powershell -NoProfile -ExecutionPolicy Bypass -File $PrepScriptInSandbox " +
        "-Git $($Combo.Git) -Rustup $($Combo.Rustup) -Winget $($Combo.Winget) -ComboName $($Combo.Name)"

    $Xml = @"
<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Enable</Networking>
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

    $OutFile = Join-Path $ConfigDir "$($Combo.Name).wsb"
    Set-Content -LiteralPath $OutFile -Value $Xml -Encoding ASCII
    Write-Host "wrote $OutFile"
}

Write-Host ""
Write-Host "Done. Run the combos one at a time (only one sandbox needs to be open):"
Write-Host "  each .wsb in $ConfigDir"
Write-Host "Logs land in:"
Write-Host "  $ResultsDir"
