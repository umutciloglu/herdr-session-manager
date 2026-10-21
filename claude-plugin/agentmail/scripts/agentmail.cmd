@echo off
rem agentmail launcher for the Claude Code plugin, Windows half.
rem
rem Claude Code starts the plugin's MCP server by the extensionless path
rem scripts\agentmail; on Windows that path resolves through PATHEXT to this file.
rem agentmail.ps1 fetches and verifies the binary and prints its path; cmd.exe then runs
rem it, because cmd hands the MCP stdio pipes to its child untouched.
rem
rem stdin is taken from nul so PowerShell can never read the MCP request stream.
rem The script path stays inside double quotes, where a `)` in it cannot end the
rem for-loop set early.
setlocal
set "exe="
for /f "usebackq delims=" %%e in (`powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0agentmail.ps1" ^<nul`) do set "exe=%%e"
if not defined exe exit /b 1
"%exe%" %*
exit /b %errorlevel%
