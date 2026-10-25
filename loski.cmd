@echo off
rem Runs scripts\loski.ps1 without changing PowerShell's script policy.
rem Double-click it to start Loski, or run it with a command, like: loski status
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\loski.ps1" %*
if "%~1"=="" pause
