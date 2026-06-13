# Unattended corpus growth (SPEC 3.7): loop run_search so every judge eval
# lands in %APPDATA%\Moss\MossRaven\data\corpus\evals-<pob2-version>.jsonl.
#
#   .\scripts\corpus-churn.ps1                  # default: 25 gens/cycle, forever
#   .\scripts\corpus-churn.ps1 -Generations 50 -MaxCycles 10
#
# Stop: create the sentinel file scratch\STOP-CHURN (checked between cycles).
# ASCII ONLY in this file: PowerShell 5.1 reads BOM-less scripts as ANSI and
# unicode punctuation corrupts string parsing (caused instant exit-1 crashes).

param(
    [int]$Generations = 25,
    [int]$MaxCycles = 0
)

$ErrorActionPreference = "Continue"
try {
    $root = Split-Path $PSScriptRoot -Parent
    $exe = Join-Path $root "dist\mossraven-service.exe"
    if (-not (Test-Path $exe)) { Write-Output "FATAL missing $exe - build/publish first"; exit 1 }
    $sentinel = Join-Path $root "scratch\STOP-CHURN"
    $argsFile = Join-Path $env:TEMP "mr-churn-args.json"
    $jsonBody = '{"generations": ' + $Generations + '}'
    [System.IO.File]::WriteAllText($argsFile, $jsonBody, (New-Object System.Text.UTF8Encoding($false)))

    foreach ($k in "CEREBRAS_API_KEY", "GROQ_API_KEY", "GEMINI_API_KEY") {
        $v = [Environment]::GetEnvironmentVariable($k, "User")
        if ($v) { Set-Item "env:$k" $v }
    }

    # MULTI-THREADED scoring: one PoB Lua VM per worker, judged concurrently.
    # Use all but one core (the service clamps to cores-1, max 16). Each worker
    # is ~150 MB RAM. This is the single biggest corpus-throughput lever.
    $cores = [Environment]::ProcessorCount
    $pool = [Math]::Max(2, $cores - 1)
    $env:MOSSRAVEN_POOL_SIZE = "$pool"
    Write-Output ("multi-threaded churn: {0} scoring workers ({1} cores detected)" -f $pool, $cores)

    $cycle = 0
    while ($true) {
        if (Test-Path $sentinel) { Write-Output "STOP-CHURN sentinel found - exiting."; break }
        if ($MaxCycles -gt 0 -and $cycle -ge $MaxCycles) { break }
        $cycle++
        # PID-namespaced: an orphaned churn (force-closed app) must never
        # lock the log names a fresh churn session wants.
        $log = Join-Path $env:TEMP ("mr-churn-{0}-{1}.log" -f $PID, ($cycle % 10))
        Write-Output ("cycle {0}: {1} generations (log {2})" -f $cycle, $Generations, $log)
        & $exe --tool run_search --tool-args-file $argsFile *> $log
        if ($LASTEXITCODE -ne 0) {
            Write-Output "cycle $cycle exited $LASTEXITCODE - backing off 60s (see $log)"
            Start-Sleep -Seconds 60
        }
        Start-Sleep -Seconds 5
    }
    $corpus = Join-Path $env:APPDATA "Moss\MossRaven\data\corpus"
    if (Test-Path $corpus) {
        Get-ChildItem $corpus | ForEach-Object {
            Write-Output ("corpus: {0}  {1} KB" -f $_.Name, [Math]::Round($_.Length / 1KB))
        }
    }
}
catch {
    Write-Output "CHURN CRASH: $($_.Exception.Message)"
    exit 1
}
