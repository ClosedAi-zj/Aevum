@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage.
rem Launch the Rust app. Prefer the packaged green build, fall back to whatever
rem cargo left in target/. Args pass straight through, so you can hand it a
rem folder or a file:  (drag a folder onto this .bat, or pass a path)
rem
rem The dist\lumen fallback that used to live here was removed in 1.0.0: it pointed
rem at the 0.4 green package, which no longer exists in any supported install path.
rem A fallback that can never resolve is not harmless -- it makes the launcher look
rem like it supports an upgrade route it does not.
cd /d "%~dp0"
set "EXE="
if exist "dist\aevum\aevum.exe" set "EXE=dist\aevum\aevum.exe"
if not defined EXE if exist "target\release\aevum.exe" set "EXE=target\release\aevum.exe"
if not defined EXE if exist "target\debug\aevum.exe" set "EXE=target\debug\aevum.exe"
if not defined EXE (
    echo No aevum.exe found. Run scripts\package.sh or build with cargo first.
    exit /b 1
)
start "" "%EXE%" %*
