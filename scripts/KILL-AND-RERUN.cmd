@echo off
REM Releases the dist\ binary lock (a stale mossraven-service.exe holds it),
REM then rebuilds/publishes and runs the druid smoke. Double-click to run.
cd /d "%~dp0.."
echo ======== killing any running MossRaven processes ========
taskkill /F /IM mossraven-service.exe 2>nul
taskkill /F /IM mossraven-node.exe 2>nul
REM give the OS a moment to release file handles
ping -n 3 127.0.0.1 >nul
echo ======== STAGE 1: windows-validate.ps1 ========
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-validate.ps1"
echo ======== STAGE 2: run-druid-smoke.ps1 ========
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-druid-smoke.ps1"
echo ======== DONE - see scripts\validate-last-run.log and scripts\druid-run.log ========
