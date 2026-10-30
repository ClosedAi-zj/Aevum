@echo off
rem Keep this file ASCII-only: cmd parses .bat in the OEM codepage, so any
rem non-ASCII text here turns into garbage commands. Flags live in the Python.
cd /d "%~dp0"
set PYTHONIOENCODING=utf-8
".venv\Scripts\python.exe" "apps\build_exe.py"
