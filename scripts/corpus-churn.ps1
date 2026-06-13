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
    # Big cycles by default: each cycle is a fresh service that loads one PoB
    # Lua VM PER WORKER (~11 on a 12-core box) sequentially — tens of seconds of
    # mostly-single-threaded startup. Tiny 25-gen cycles meant that startup
    # DOMINATED the wall clock (the "only 20% CPU" symptom). 400 gens per cycle
    # amortizes the load over far more scoring so the pool stays saturated.
    # Corpus rows are append-logged per eval, so a long cycle never risks data.
    [int]$Generations = 400,
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

    # MECHANICAL proposals (no cloud LLM): the corpus is a DATA factory, not a
    # search — it wants volume, not intelligent proposals. The free cloud tiers
    # (Cerebras 429) made churn idle in 5-minute backoffs (RAM, no CPU). Pure
    # mechanical mutation keeps every core scoring. To instead drive churn off a
    # LOCAL GPU model, clear this and set OLLAMA_MODEL=qwen2.5:14b.
    if (-not $env:OLLAMA_MODEL) { $env:MOSSRAVEN_MECHANICAL = "1" }
    Write-Output ("multi-threaded churn: {0} scoring workers ({1} cores), mechanical={2}, ollama={3}" -f $pool, $cores, $env:MOSSRAVEN_MECHANICAL, $env:OLLAMA_MODEL)

    # Single-instance lock: a force-closed app used to leave orphan churns that
    # piled up (4 at once), all hammering the same rate-limited key. If a live
    # churn already holds the lock, exit instead of stacking.
    $lock = Join-Path $root "scratch\CHURN-RUNNING"
    if (Test-Path $lock) {
        $heldBy = (Get-Content $lock -ErrorAction SilentlyContinue | Select-Object -First 1)
        if ($heldBy -and (Get-Process -Id $heldBy -ErrorAction SilentlyContinue)) {
            Write-Output "another churn (PID $heldBy) is already running - exiting to avoid pile-up."
            exit 0
        }
    }
    Set-Content $lock -Value $PID -Encoding ascii

    $cycle = 0
    while ($true) {
        if (Test-Path $sentinel) { Write-Output "STOP-CHURN sentinel found - exiting."; break }
        if ($MaxCycles -gt 0 -and $cycle -ge $MaxCycles) { break }
        $cycle++
        # PID-namespaced: an orphaned churn (force-closed app) must never
        # lock the log names a fresh churn session wants.
        $log = Join-Path $env:TEMP ("mr-churn-{0}-{1}.log" -f $PID, ($cycle % 10))
        Write-Output ("cycle {0}: {1} generations (log {2})" -f $cycle, $Generations, $log)
        # Launch run_search as a child we can KILL instantly when STOP-CHURN
        # appears — don't wait for the whole 400-gen cycle to finish.
        $proc = Start-Process -FilePath $exe -ArgumentList @("--tool","run_search","--tool-args-file","`"$argsFile`"") -NoNewWindow -PassThru -RedirectStandardOutput $log -RedirectStandardError "$log.err"
        while (-not $proc.HasExited) {
            if (Test-Path $sentinel) {
                Write-Output "STOP-CHURN seen mid-cycle - killing run_search now."
                try { $proc.Kill($true) } catch { try { Stop-Process -Id $proc.Id -Force } catch {} }
                break
            }
            Start-Sleep -Milliseconds 500
        }
        if (Test-Path $sentinel) { Write-Output "STOP-CHURN sentinel found - exiting."; break }
        if ($proc.ExitCode -ne 0) {
            Write-Output "cycle $cycle exited $($proc.ExitCode) - backing off 30s (see $log)"
            for ($w = 0; $w -lt 30; $w++) { if (Test-Path $sentinel) { break }; Start-Sleep -Seconds 1 }
        }
        Start-Sleep -Seconds 1
    }
    $corpus = Join-Path $env:APPDATA "Moss\MossRaven\data\corpus"
    if (Test-Path $corpus) {
        Get-ChildItem $corpus | ForEach-Object {
            Write-Output ("corpus: {0}  {1} KB" -f $_.Name, [Math]::Round($_.Length / 1KB))
        }
    }
    Remove-Item $lock -ErrorAction SilentlyContinue
}
catch {
    Write-Output "CHURN CRASH: $($_.Exception.Message)"
    if ($lock) { Remove-Item $lock -ErrorAction SilentlyContinue }
    exit 1
}
