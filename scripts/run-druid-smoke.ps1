# One-click concept smoke: headless search + frontier + Tier-5 attempt.
# Launch via right-click -> Run with PowerShell. Artifacts land in
# <repo>\data\ and the transcript in scripts\druid-run.log so the Cowork
# session can read everything from its side.
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
Start-Transcript -Path (Join-Path $repo 'scripts\druid-run.log') -Force

function Invoke-Native {
    # Merge stderr+stdout so cargo/dotnet/exe output lands in the transcript
    # (external processes write to the console handle directly otherwise and
    # the transcript only captures the terminating error).
    & $args[0] $args[1..($args.Count-1)] 2>&1 | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "exit code $LASTEXITCODE" }
}
# Never let an exception kill the window before the transcript flushes.
trap { Write-Host "DRUID SMOKE ERROR: $_" -ForegroundColor Red; try { Stop-Transcript } catch {}; exit 1 }

# Keep this run's archive inside the repo (readable by the Cowork session)
# instead of the production archive in %APPDATA%.
$env:MOSSRAVEN_ARCHIVE_PATH = Join-Path $repo 'data\archive.json'

$svc = "dist\mossraven-service.exe"
Write-Host ("service build time: " + (Get-Item $svc).LastWriteTime)

Invoke-Native $svc --headless --concept "Shieldy lightning wolf druid with boss and clear weapon swaps" --generations 8
Invoke-Native $svc --tool get_frontier
# Mode A (MOSSRAVEN_ANTHROPIC_API_KEY set): writes guides + saves finalists.
# Mode B (no key): prints the frontier + curation instructions; the Cowork
# session curates and saves via --tool save_finalists.
Invoke-Native $svc --tool synthesize_finalists

Write-Host "DRUID SMOKE COMPLETE" -ForegroundColor Green
Stop-Transcript
