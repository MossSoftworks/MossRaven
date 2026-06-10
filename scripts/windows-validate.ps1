# MossRaven Windows validation gate — vendor currency, build, tests, smoke drive, dist refresh.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\windows-validate.ps1
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
$failed = @()

function Invoke-Native {
    & $args[0] $args[1..($args.Count-1)]
    if ($LASTEXITCODE -ne 0) { throw "exit code $LASTEXITCODE" }
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
    Invoke-Native cargo test -p mossraven-pob --release --test init_smoke -- --nocapture
}

Stage "pob parity fixtures (slow; self-skips without fixtures)" {
    Invoke-Native cargo test -p mossraven-pob --release --test parity -- --ignored --nocapture
}

Stage "smoke drive: seed -> run -> frontier -> synthesize (temp archive)" {
    $env:MOSSRAVEN_ARCHIVE_PATH = Join-Path $env:TEMP "mossraven-validate\archive.json"
    $dir = Split-Path $env:MOSSRAVEN_ARCHIVE_PATH
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $svc = "target\release\mossraven-service.exe"
    Invoke-Native $svc --tool seed_hypothesis --tool-args '{"concept":"validation smoke: off-meta cold DoT"}'
    Invoke-Native $svc --tool run_search --tool-args '{"generations":2}'
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
} else {
    Write-Host ("FAILED STAGES: " + ($failed -join ', ')) -ForegroundColor Red
    exit 1
}
