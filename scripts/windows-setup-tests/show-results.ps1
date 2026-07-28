<#
Run this on the HOST (not inside a sandbox) to collate what the combos have produced
so far. Answers the only question the kit exists to answer -- did every combination of
git/rustup/winget end up with vaultis actually installed, two working shortcuts on the
desktop -- and names the ones that did not.

Safe to run mid-run: a combo still in progress shows as RUNNING, with the last step it
reported, so a long cargo build is distinguishable from a hang.
#>
$ErrorActionPreference = 'Stop'

$ResultsDir = Join-Path $PSScriptRoot 'results'
if (-not (Test-Path -LiteralPath $ResultsDir)) {
    Write-Host "No results yet: $ResultsDir does not exist. Run make-sandbox-configs.ps1 first."
    exit 0
}

# Same order and names make-sandbox-configs.ps1 writes, so a combo that has never been
# run is reported as missing rather than silently omitted.
$Combos = @(
    'git-rustup-winget'
    'git-rustup-nowinget'
    'git-norustup-winget'
    'git-norustup-nowinget'
    'nogit-rustup-winget'
    'nogit-rustup-nowinget'
    'nogit-norustup-winget'
    'nogit-norustup-nowinget'
)

$Rows = foreach ($Combo in $Combos) {
    $ResultFile = Join-Path $ResultsDir "$Combo.result"
    $StatusFile = Join-Path $ResultsDir "$Combo.status"

    if (Test-Path -LiteralPath $ResultFile) {
        # PASS/FAIL <tab> combo <tab> wanted-state <tab> exit=N [<tab> failures]
        $Fields = (Get-Content -LiteralPath $ResultFile -TotalCount 1) -split "`t"
        [pscustomobject]@{
            Combo  = $Combo
            Result = $Fields[0]
            Detail = if ($Fields.Count -gt 4) { $Fields[4] } else { $Fields[3] }
        }
    }
    elseif (Test-Path -LiteralPath $StatusFile) {
        $LastStep = Get-Content -LiteralPath $StatusFile | Where-Object { $_ } | Select-Object -Last 1
        [pscustomobject]@{ Combo = $Combo; Result = 'RUNNING'; Detail = $LastStep }
    }
    else {
        [pscustomobject]@{ Combo = $Combo; Result = 'not run'; Detail = '' }
    }
}

$Rows | Format-Table -AutoSize -Wrap

$Passed = @($Rows | Where-Object { $_.Result -eq 'PASS' }).Count
Write-Host ""
Write-Host "$Passed of $($Combos.Count) combos ended with vaultis installed and both desktop shortcuts working."
$Bad = @($Rows | Where-Object { $_.Result -eq 'FAIL' })
if ($Bad) {
    Write-Host ""
    Write-Host "Full transcripts for the failures:"
    foreach ($Row in $Bad) {
        $LogFile = Join-Path $ResultsDir ($Row.Combo + '.log')
        Write-Host "  $LogFile"
    }
}
