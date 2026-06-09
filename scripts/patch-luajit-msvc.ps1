<#
.SYNOPSIS
    Patch luajit-src's msvcbuild.bat in the cargo registry so LuaJIT builds
    on Windows hosts that block bare-name CWD-resolution of executables.

.DESCRIPTION
    LuaJIT's MSVC bootstrap builds minilua.exe and buildvm.exe into the
    current directory, then invokes them via bare name: `minilua dynasm.lua ...`.
    Windows installations that enforce CWD-non-search (via Group Policy, WDAC,
    or the NoDefaultCurrentDirectoryInExePath kernel policy — independent of
    the env var of the same name) refuse to resolve bare names from CWD, and
    cmd.exe reports `'minilua' is not recognized`.

    This script patches both copies of msvcbuild.bat inside the cargo registry
    (the upstream luajit2/src/ original and the luajit-src crate's extras/
    override) to prefix `minilua` and `buildvm` with `.\` so cmd.exe accepts
    them as relative-path invocations.

    Run this script once after a fresh checkout, and again whenever cargo
    updates the luajit-src crate (e.g. after `cargo update`). It's idempotent —
    re-running on an already-patched checkout is a no-op.

.NOTES
    A proper durable fix would fork luajit-src and pin via [patch.crates-io]
    in Cargo.toml. Until then, this script is the practical workaround.
#>

[CmdletBinding()]
param(
    [string] $CargoRegistry = "$env:USERPROFILE\.cargo\registry\src"
)

$ErrorActionPreference = "Stop"

# Find the luajit-src crate (version-suffixed dir name)
$crateRoot = Get-ChildItem -Path "$CargoRegistry\index.crates.io-*\luajit-src-*" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $crateRoot) {
    Write-Error "luajit-src crate not found under $CargoRegistry. Run 'cargo fetch' first to download it."
    exit 1
}

Write-Output "Found luajit-src at: $($crateRoot.FullName)"

$bats = @(
    Join-Path $crateRoot.FullName "extras\msvcbuild.bat"
    Join-Path $crateRoot.FullName "luajit2\src\msvcbuild.bat"
) | Where-Object { Test-Path $_ }

if ($bats.Count -eq 0) {
    Write-Error "No msvcbuild.bat files found under $($crateRoot.FullName)"
    exit 1
}

$patchedCount = 0
$alreadyPatchedCount = 0

foreach ($bat in $bats) {
    $content = [System.IO.File]::ReadAllText($bat)

    # Already patched? (any `.\minilua ` or `.\buildvm ` at line start)
    if ($content -match '(?m)^\.\\(minilua|buildvm) ') {
        Write-Output "  [skip] $bat — already patched"
        $alreadyPatchedCount++
        continue
    }

    $patched = $content -replace '(?m)^(minilua|buildvm) ', '.\$1 '
    [System.IO.File]::WriteAllText($bat, $patched)

    # Verify patch took
    $hits = (Select-String -Path $bat -Pattern '^\.\\(minilua|buildvm) ' | Measure-Object).Count
    if ($hits -lt 9) {
        Write-Warning "  [partial] $bat — expected 9 patched lines, got $hits"
    } else {
        Write-Output "  [patched] $bat ($hits lines)"
        $patchedCount++
    }
}

Write-Output ""
Write-Output "Done. $patchedCount patched, $alreadyPatchedCount already-patched."
Write-Output "If cargo had a partial build cached, run: cargo clean -p mlua-sys"
Write-Output "Then: cargo build --workspace --release"
