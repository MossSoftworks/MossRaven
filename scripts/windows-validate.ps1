# MossRaven Windows validation gate — vendor currency, build, tests, smoke drive, dist refresh.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\windows-validate.ps1
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
# Transcript so a headless/Explorer-launched run leaves a readable record
# even after the console window closes.
Start-Transcript -Path (Join-Path $repo 'scripts\validate-last-run.log') -Force
$failed = @()

# A lingering service/WPF/test process locks target\release\*.exe (cargo
# relink → os error 32) and dist\*.exe (copy → resource busy). Registered
# MCP clients respawn the daemon on their next call, so this is best-effort
# hygiene, not a guarantee — stages that still hit a lock should be re-run.
Get-Process | Where-Object { $_.ProcessName -match '^(mossraven|MossRaven)' } |
    Stop-Process -Force -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

function Invoke-Native {
    # Native tools (cargo, dotnet, our service) write progress to STDERR as a
    # matter of course. Under $ErrorActionPreference='Stop' a merged stderr
    # line becomes a terminating error, so drop to 'Continue' for the call and
    # gate success on the real exit code instead.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $args[0] $args[1..($args.Count-1)] 2>&1 | ForEach-Object { Write-Host $_ }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { throw "exit code $code" }
}

function Stage($name, [scriptblock]$body) {
    Write-Host "`n== $name ==" -ForegroundColor Cyan
    try { & $body; Write-Host "PASS: $name" -ForegroundColor Green }
    catch { Write-Host "FAIL: $name - $_" -ForegroundColor Red; $script:failed += $name }
}

Stage "league currency: vendor PoB2 pull (ff-only)" {
    Invoke-Native git -C vendor/PathOfBuilding-PoE2 pull --ff-only
    Get-Content vendor/PathOfBuilding-PoE2/manifest.xml | Select-String 'Version number' | Select-Object -First 1
}

Stage "cargo build --workspace --release" {
    Invoke-Native cargo build --workspace --release
}

Stage "unit tests (core / surrogate / archive / node-protocol)" {
    Invoke-Native cargo test -p mossraven-core -p mossraven-surrogate -p mossraven-archive -p mossraven-node-protocol --release
}

Stage "pob init smoke (loads Lua VM)" {
    # The test is #[ignore]d (loads the full Lua VM) — without --ignored this
    # stage passed VACUOUSLY ("0 passed; 1 ignored"). Run it for real.
    Invoke-Native cmd /c "cargo test -p mossraven-pob --release --test init_smoke -- --ignored --nocapture 2>&1"
}

Stage "pob parity fixtures (slow; self-skips without fixtures)" {
    Invoke-Native cmd /c "cargo test -p mossraven-pob --release --test parity -- --ignored --nocapture 2>&1"
}

Stage "smoke drive: seed -> run -> frontier -> synthesize (temp archive)" {
    $env:MOSSRAVEN_ARCHIVE_PATH = Join-Path $env:TEMP "mossraven-validate\archive.json"
    $dir = Split-Path $env:MOSSRAVEN_ARCHIVE_PATH
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $svc = "target\release\mossraven-service.exe"
    # No spaces inside the JSON: PS 5.1 re-tokenizes the escaped-quote string
    # when splatting through Invoke-Native and a space splits it into extra
    # argv entries ("unknown arg: smoke:"). Spaceless JSON can't be split.
    Invoke-Native $svc --tool seed_hypothesis --tool-args '{\"concept\":\"validation-smoke-off-meta-cold-DoT\"}'
    Invoke-Native $svc --tool run_search --tool-args '{\"generations\":2}'
    Invoke-Native $svc --tool get_frontier
    # Mode A (key set): synthesizes + persists. Mode B (no key): returns the
    # frontier + curation instructions. Both are healthy outcomes here.
    Invoke-Native $svc --tool synthesize_finalists
    Remove-Item Env:MOSSRAVEN_ARCHIVE_PATH
}

Stage "dist refresh (rust binaries)" {
    Copy-Item target\release\mossraven-service.exe dist\ -Force
    Copy-Item target\release\mossraven-node.exe dist\ -Force
}

Stage "dist refresh (WPF single-file publish)" {
    Invoke-Native dotnet publish ui\MossRaven\MossRaven.csproj -c Release -r win-x64 --self-contained -p:PublishSingleFile=true -o dist\
}

Write-Host ""
if ($failed.Count -eq 0) {
    Write-Host "ALL STAGES GREEN" -ForegroundColor Green
    Stop-Transcript
} else {
    Write-Host ("FAILED STAGES: " + ($failed -join ', ')) -ForegroundColor Red
    Stop-Transcript
    exit 1
}
