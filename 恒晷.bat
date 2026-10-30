@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage.
rem Launch the Rust app. Prefer the packaged green build, fall back to whatever
rem cargo left in target/. Args pass straight through, so you can hand it a
rem folder or a file: 恒晷.bat D:\...\lr4
rem
rem The dist\lumen entry is deliberate: anyone who already unzipped the 0.4
rem green package still has dist\lumen\lumen.exe on disk, and the launcher must
rem keep working for them. Do not remove it.
cd /d "%~dp0"
set "EXE="
if exist "dist\aevum\aevum.exe" set "EXE=dist\aevum\aevum.exe"
if not defined EXE if exist "dist\lumen\lumen.exe" set "EXE=dist\lumen\lumen.exe"
if not defined EXE if exist "target\release\aevum.exe" set "EXE=target\release\aevum.exe"
if not defined EXE if exist "target\debug\aevum.exe" set "EXE=target\debug\aevum.exe"
if not defined EXE (
    echo No aevum.exe found. Run scripts\package.sh or build with cargo first.
    exit /b 1
)
start "" "%EXE%" %*
