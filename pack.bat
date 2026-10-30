@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage.
rem Build the Windows package (release exe + bundled ffmpeg) via Git Bash.
cd /d "%~dp0"
set "BASH=D:\work\tools\Git\bin\bash.exe"
if not exist "%BASH%" set "BASH=bash"
"%BASH%" "scripts/package.sh" %*
if errorlevel 1 exit /b 1
