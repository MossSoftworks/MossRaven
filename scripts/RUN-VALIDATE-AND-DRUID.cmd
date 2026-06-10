@echo off
REM One-click: build+test+publish, then the druid smoke run.
REM Double-click this in File Explorer. Output goes to the console AND to
REM scripts\validate-last-run.log + scripts\druid-run.log (read by Cowork).
cd /d "%~dp0.."
echo ======== STAGE 1: windows-validate.ps1 ========
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-validate.ps1"
echo ======== STAGE 2: run-druid-smoke.ps1 ========
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-druid-smoke.ps1"
echo ======== DONE ========
echo Wrote scripts\validate-last-run.log and scripts\druid-run.log
