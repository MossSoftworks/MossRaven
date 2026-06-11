# Unattended corpus growth (SPEC §3.7): loop run_search so every Tier-4 eval
# lands in %APPDATA%\Moss\MossRaven\data\corpus\evals-<pob2-version>.jsonl.
#
#   .\scripts\corpus-churn.ps1                  # default: 25 gens/cycle, forever
#   .\scripts\corpus-churn.ps1 -Generations 50 -MaxCycles 10
#
# Stop: create the sentinel file  scratch\STOP-CHURN  (checked between cycles)
# or Ctrl+C. Each cycle logs to %TEMP%\mr-churn-<n>.log (last 10 kept).
# PS 5.1 NOTE: never pipe the service through Select-Object -First — it kills
# the process mid-run. This script redirects to files instead.

param(
    [int]$Generations = 25,
    [int]$MaxCycles = 0   # 0 = run until sentinel/Ctrl+C
)

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "dist\mossraven-service.exe"
if (-not (Test-Path $exe)) { Write-Error "missing $exe — build/publish first"; exit 1 }
$sentinel = Join-Path $root "scratch\STOP-CHURN"
$argsFile = Join-Path $env:TEMP "mr-churn-args.json"
[System.IO.File]::WriteAllText($argsFile, "{`"generations`": $Generations}",
    (New-Object System.Text.UTF8Encoding($false)))

# Provider keys from user-level env (set via setx / the WPF settings).
foreach ($k in "CEREBRAS_API_KEY", "GROQ_API_KEY", "GEMINI_API_KEY") {
    $v = [Environment]::GetEnvironmentVariable($k, "User")
    if ($v) { Set-Item "env:$k" $v }
}

$cycle = 0
while ($true) {
    if (Test-Path $sentinel) { Write-Host "STOP-CHURN sentinel found — exiting."; break }
    if ($MaxCycles -gt 0 -and $cycle -ge $MaxCycles) { break }
    $cycle++
    $log = Join-Path $env:TEMP ("mr-churn-{0}.log" -f ($cycle % 10))
    Write-Host ("[{0}] cycle {1}: {2} generations -> {3}" -f (Get-Date -Format T), $cycle, $Generations, $log)
    & $exe --tool run_search --tool-args-file $argsFile *> $log
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "cycle $cycle exited $LASTEXITCODE — backing off 60s (see $log)"
        Start-Sleep -Seconds 60
    }
    # Free-tier RPM bucket courtesy gap.
    Start-Sleep -Seconds 5
}
$corpus = Join-Path $env:APPDATA "Moss\MossRaven\data\corpus"
if (Test-Path $corpus) {
    Get-ChildItem $corpus | ForEach-Object {
        $lines = 0; try { $lines = (Get-Content $_.FullName | Measure-Object -Line).Lines } catch {}
        Write-Host ("corpus: {0}  rows={1}  {2:N1} MB" -f $_.Name, $lines, ($_.Length / 1MB))
    }
}
