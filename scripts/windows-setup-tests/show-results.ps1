<#
Run this on the HOST (not inside a sandbox) to collate what the scenarios have produced
so far. Answers the only question the kit exists to answer -- did every scenario end up
with vaultis actually installed, two working shortcuts on the desktop -- and names the
ones that did not.

Safe to run mid-run: a scenario still in progress shows as RUNNING, with the last step
it reported, so a slow download is distinguishable from a hang.
#>
$ErrorActionPreference = 'Stop'

$ResultsDir = Join-Path $PSScriptRoot 'results'
if (-not (Test-Path -LiteralPath $ResultsDir)) {
    Write-Host "No results yet: $ResultsDir does not exist. Run make-sandbox-configs.ps1 first."
    exit 0
}

# Same order and names make-sandbox-configs.ps1 writes, so a scenario that has never
# been run is reported as missing rather than silently omitted.
$Scenarios = @(
    'fresh'
    'reinstall'
)

$Rows = foreach ($Scenario in $Scenarios) {
    $ResultFile = Join-Path $ResultsDir "$Scenario.result"
    $StatusFile = Join-Path $ResultsDir "$Scenario.status"

    if (Test-Path -LiteralPath $ResultFile) {
        # PASS/FAIL <tab> scenario <tab> exit=N [<tab> failures]
        # Show the failures when there are any, and the exit code when there are not.
        $Fields = (Get-Content -LiteralPath $ResultFile -TotalCount 1) -split "`t"
        [pscustomobject]@{
            Scenario = $Scenario
            Result = $Fields[0]
            Detail = if ($Fields.Count -gt 3) { $Fields[3] } else { $Fields[2] }
        }
    }
    elseif (Test-Path -LiteralPath $StatusFile) {
        $LastStep = Get-Content -LiteralPath $StatusFile | Where-Object { $_ } | Select-Object -Last 1
        [pscustomobject]@{ Scenario = $Scenario; Result = 'RUNNING'; Detail = $LastStep }
    }
    else {
        [pscustomobject]@{ Scenario = $Scenario; Result = 'not run'; Detail = '' }
    }
}

$Rows | Format-Table -AutoSize -Wrap

$Passed = @($Rows | Where-Object { $_.Result -eq 'PASS' }).Count
Write-Host ""
Write-Host "$Passed of $($Scenarios.Count) scenarios ended with vaultis installed and both desktop shortcuts working."
$Bad = @($Rows | Where-Object { $_.Result -eq 'FAIL' })
if ($Bad) {
    Write-Host ""
    Write-Host "Full transcripts for the failures:"
    foreach ($Row in $Bad) {
        $LogFile = Join-Path $ResultsDir ($Row.Scenario + '.log')
        Write-Host "  $LogFile"
    }
}
