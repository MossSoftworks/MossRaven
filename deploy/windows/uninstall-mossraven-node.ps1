<#
.SYNOPSIS
    Uninstall the MossRavenNode scheduled task and clear its machine-scope env vars.
#>

[CmdletBinding()]
param(
    [string] $TaskName = "MossRavenNode"
)

$ErrorActionPreference = "Stop"

$existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) {
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Output "Unregistered task '$TaskName'."
} else {
    Write-Output "No task '$TaskName' found."
}

foreach ($name in "MOSSRAVEN_NODE_BEARER", "MOSSRAVEN_NODE_BIND", "MOSSRAVEN_POB_PATH") {
    [Environment]::SetEnvironmentVariable($name, $null, "Machine")
}
Write-Output "Cleared machine-scope env vars (MOSSRAVEN_NODE_BEARER, MOSSRAVEN_NODE_BIND, MOSSRAVEN_POB_PATH)."
