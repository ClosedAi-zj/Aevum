@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage.
rem Build the Windows package (release exe + bundled ffmpeg) via Git Bash.
rem
rem bash.exe is located by probing, never by a hard-coded path: a path that only
rem exists on the machine of whoever wrote this file is worse than no path at all,
rem because it fails silently on every other machine and nobody notices until the
rem build produces a stale or missing package.
cd /d "%~dp0"
set "BASH="
for %%i in (bash.exe) do if not defined BASH set "BASH=%%~$PATH:i"
if not defined BASH if exist "%ProgramFiles%\Git\bin\bash.exe" set "BASH=%ProgramFiles%\Git\bin\bash.exe"
if not defined BASH if exist "%ProgramFiles(x86)%\Git\bin\bash.exe" set "BASH=%ProgramFiles(x86)%\Git\bin\bash.exe"
if not defined BASH if exist "%LOCALAPPDATA%\Programs\Git\bin\bash.exe" set "BASH=%LOCALAPPDATA%\Programs\Git\bin\bash.exe"
if not defined BASH (
    echo Git Bash not found. Install Git for Windows, or run: bash scripts/package.sh
    exit /b 1
)
echo Using %BASH%
"%BASH%" "scripts/package.sh" %*
if errorlevel 1 exit /b 1
