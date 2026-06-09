<#
.SYNOPSIS
    Install mossraven-node.exe as a Scheduled Task that starts at logon and
    restarts on failure.

.PARAMETER Bearer
    Shared-secret bearer token. Must match what the orchestrator sends.

.PARAMETER PobPath
    Path to a clone of PathOfBuildingCommunity/PathOfBuilding-PoE2.

.PARAMETER Bind
    Bind address. Default 0.0.0.0:5380.

.PARAMETER ExePath
    Path to mossraven-node.exe. Default: same dir as this script.

.PARAMETER TaskName
    Scheduled-task name. Default: MossRavenNode.

.EXAMPLE
    .\install-mossraven-node.ps1 -Bearer "abc123..." `
                               -PobPath "C:\PathOfBuilding-PoE2"
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Bearer,
    [Parameter(Mandatory = $true)] [string] $PobPath,
    [string] $Bind     = "0.0.0.0:5380",
    [string] $ExePath  = (Join-Path $PSScriptRoot "mossraven-node.exe"),
    [string] $TaskName = "MossRavenNode"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $ExePath))  { throw "mossraven-node.exe not found at $ExePath" }
if (-not (Test-Path $PobPath))  { throw "PathOfBuilding-PoE2 not found at $PobPath" }

# Resolve absolute paths so the scheduled task works regardless of working dir.
$ExePath = (Resolve-Path $ExePath).Path
$PobPath = (Resolve-Path $PobPath).Path

# Build the action: launch mossraven-node.exe with no args.
$action = New-ScheduledTaskAction -Execute $ExePath -WorkingDirectory (Split-Path $ExePath -Parent)

# Trigger at user logon. For headless / 24-7 farm boxes, swap to:
#   $trigger = New-ScheduledTaskTrigger -AtStartup
# and run as SYSTEM (see -Principal below) or a stored-password service account.
$trigger = New-ScheduledTaskTrigger -AtLogOn

# Run elevated as the current user; do not require a password to be stored.
$principal = New-ScheduledTaskPrincipal `
    -UserId ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name) `
    -LogonType Interactive `
    -RunLevel Highest

# Restart on failure, no time limit, allow start on battery.
$settings = New-ScheduledTaskSettingsSet `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -RestartCount 999 `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -MultipleInstances IgnoreNew

# Environment for the task — Scheduled Tasks don't have a direct EnvironmentFile
# concept like systemd, so we set machine-scope env vars. The task inherits them.
[Environment]::SetEnvironmentVariable("MOSSRAVEN_NODE_BEARER", $Bearer,  "Machine")
[Environment]::SetEnvironmentVariable("MOSSRAVEN_NODE_BIND",   $Bind,    "Machine")
[Environment]::SetEnvironmentVariable("MOSSRAVEN_POB_PATH",    $PobPath, "Machine")
[Environment]::SetEnvironmentVariable("RUST_LOG",            "info",   "Machine")

# Remove an existing task with this name (idempotent install/upgrade).
$existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) { Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false }

Register-ScheduledTask `
    -TaskName $TaskName `
    -Description "MossRaven POE2 build-discovery farm node" `
    -Action $action `
    -Trigger $trigger `
    -Principal $principal `
    -Settings $settings | Out-Null

# Start it now so the user doesn't have to log out / in.
Start-ScheduledTask -TaskName $TaskName

Write-Output "Registered task '$TaskName' and started it."
Write-Output "Logs: Event Viewer > Applications and Services Logs > Microsoft > Windows > TaskScheduler"
Write-Output "Smoke test: curl http://localhost:$($Bind -replace '.*:')/health"
