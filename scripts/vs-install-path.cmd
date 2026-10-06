@echo off
setlocal enabledelayedexpansion
REM Print the Visual Studio / Build Tools install root as a Git-Bash-style
REM POSIX path (`/c/Program Files (x86)/...`), for `just` recipes that shell
REM out to MSBuild.exe directly (unlike `cargo-win.cmd`, which stays in native
REM Windows-path form for its own cmd.exe-internal use). Discovered via
REM vswhere.exe (build-system.md § Windows toolchain location) —
REM see cargo-win.cmd's matching header comment for why this needs
REM `setlocal enabledelayedexpansion` + `!VAR!`, not `%VAR%`, inside every
REM block below (the `(x86)` in the discovered path breaks cmd's block parser
REM under immediate expansion). Always succeeds: falls back to this box's
REM last-known-good install path if vswhere.exe is missing or finds nothing.
set "FALLBACK_VS_PATH=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools"
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VS_PATH="
if exist "!VSWHERE!" (
    for /f "usebackq delims=" %%P in (`"!VSWHERE!" -latest -products * -property installationPath`) do (
        set "VS_PATH=%%P"
        goto :found
    )
)
:found
if "!VS_PATH!"=="" set "VS_PATH=!FALLBACK_VS_PATH!"

set "POSIX_PATH=!VS_PATH:\=/!"
set "DRIVE=!POSIX_PATH:~0,1!"
if /i "!DRIVE!"=="C" set "DRIVE=c"
echo /!DRIVE!!POSIX_PATH:~2!
