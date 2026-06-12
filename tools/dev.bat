@echo off
rem ============================================================
rem  Rerics build wrapper (cmd side)
rem  Sets up the MSVC build environment (vcvars64) then runs cargo.
rem  Usage: dev.bat build / dev.bat run / dev.bat test
rem  From git-bash, call via dev.sh.
rem ============================================================
setlocal
rem MSYS launch drops env vars whose name contains "(" -- restore it
set "ProgramFiles(x86)=C:\Program Files (x86)"
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"

rem Locate VS dynamically via vswhere (edition/version independent)
set "VSINSTALL="
for /f "usebackq tokens=*" %%i in (`vswhere -latest -products * -property installationPath 2^>nul`) do set "VSINSTALL=%%i"
if not defined VSINSTALL (
  echo [dev.bat] Visual Studio / Build Tools not found 1>&2
  exit /b 1
)

call "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 (
  echo [dev.bat] vcvars64 init failed 1>&2
  exit /b 1
)

cargo %*
