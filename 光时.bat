@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage.
rem Launch the Rust app. Prefer the packaged green build, fall back to whatever
rem cargo left in target/. Args pass straight through, so you can hand it a
rem folder or a file: 光时.bat D:\...\lr4
cd /d "%~dp0"
set "EXE="
if exist "dist\lumen\lumen.exe" set "EXE=dist\lumen\lumen.exe"
if not defined EXE if exist "target\release\lumen.exe" set "EXE=target\release\lumen.exe"
if not defined EXE if exist "target\debug\lumen.exe" set "EXE=target\debug\lumen.exe"
if not defined EXE (
    echo No lumen.exe found. Run scripts\package.sh or build with cargo first.
    exit /b 1
)
start "" "%EXE%" %*
